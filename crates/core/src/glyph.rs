//! Glyph 形状の orb 描画モジュール。
//!
//! [`crate::orb::OrbShape::Glyph`] を選んだ orb は、フォントアウトラインを
//! いったん SDF に焼いてから `blur` / `softness` / rotation 付きで
//! サンプリングした形状になる。文字色は orb の色、不透明度は softness 軸 +
//! per-orb 揺らぎで決まる。
//!
//! # 設計メモ
//!
//! - フォントは [`include_bytes!`] でクレートに埋め込み、`'static` バイト列を
//!   そのまま [`ttf_parser::Face::parse`] に渡す。バイト列が静的なので
//!   `Face<'static>` は `Send + Sync`、`OnceLock` 経由でプロセス全体で 1 回だけ初期化する
//! - グリフごとの `bounding_box` / `outline` 計算は SDF bake 時にだけ行い、
//!   呼び出し側 ([`render_glyph_orb`]) はキャッシュ済み texture を bilinear
//!   sampling で使い回す
//! - グリフが見つからない場合 ([`Face::glyph_index`] が `None` を返す or `outline_glyph`
//!   が空アウトラインを返す) は **何も描画しない**。tofu は出さない。Phase A の方針として、
//!   絵文字など Symbols 2 に無い文字は静かに無視する
//! - フォントのアウトラインは Y 軸が上向き（font em スケール）。tiny-skia は Y 軸下向きなので、
//!   `OutlineBuilder` 内で y を反転して積み込む
//! - センタリングは `glyph_bounding_box` の中央を orb 中心に合わせ、半径 × 2 の正方領域に
//!   収まるよう em-square 基準でスケールする

use crate::style::{falloff_curve, FalloffProfile};
use std::collections::HashMap;
use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::{Arc, Mutex, OnceLock};
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, Shader, Transform};
use ttf_parser::{Face, OutlineBuilder, Rect};

/// WebGL / preview path で使う既定 Glyph SDF texture size。
pub const DEFAULT_GLYPH_SDF_SIZE: u32 = 256;
const MAX_GLYPH_SDF_SIZE: u32 = 1024;
const GLYPH_SDF_RADIUS_FACTOR: f32 = 0.45;
const GLYPH_SDF_CONTENT_SPAN: f32 = FRAC_1_SQRT_2;
const GLYPH_SDF_MAX_DIST_FACTOR: f32 = 0.06;

/// orber-core が同梱するフォント識別子。
///
/// 将来的に複数フォントを同梱する余地を残すため `enum` にしている。Phase A では
/// `NotoSymbols2` の 1 種類のみ。`Copy + Eq` を保つことで [`crate::orb::OrbShape`]
/// が引き続き `Copy` でいられる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GlyphFontId {
    /// Noto Sans Symbols 2 (記号類専用 subset)。`☆ ♪ ♥ ✿` 等が含まれる。
    #[default]
    NotoSymbols2,
}

/// 同梱フォントの生バイト列を返す。`'static` ライフタイムなので、
/// [`ttf_parser::Face::parse`] にそのまま渡せる。
pub fn font_bytes(id: GlyphFontId) -> &'static [u8] {
    match id {
        GlyphFontId::NotoSymbols2 => {
            include_bytes!("../assets/fonts/NotoSansSymbols2-Regular.ttf")
        }
    }
}

/// パース済み `Face` をプロセスでただ 1 つキャッシュする。
///
/// `Face<'static>` を `OnceLock` に保持できる根拠:
/// - フォントバイト列が `include_bytes!` 由来の `'static`
/// - `ttf_parser::Face<'static>` は `Send + Sync`（内部は不変な参照のみ）
///
/// 同梱 TTF が破損していた場合は `None` を返し、Glyph 描画は静かにスキップされる。
/// 同梱フォントは `include_bytes!` 由来の固定バイト列なので通常はパース失敗しない。
/// 万一 Phase B で外部フォント切替対応する場合は fail-fast 化を検討する。
fn face_for(id: GlyphFontId) -> Option<&'static Face<'static>> {
    match id {
        GlyphFontId::NotoSymbols2 => {
            static CELL: OnceLock<Option<Face<'static>>> = OnceLock::new();
            CELL.get_or_init(|| Face::parse(font_bytes(id), 0).ok())
                .as_ref()
        }
    }
}

/// `tiny_skia::PathBuilder` にアウトラインを積む `OutlineBuilder` 実装。
///
/// フォントは Y 軸上向き、tiny-skia は Y 軸下向きなので、ここで y を反転する。
/// 同時に em スケールから「orb 半径×2 の正方領域」スケールへの線形変換を適用する。
struct GlyphPathBuilder {
    pb: PathBuilder,
    /// X 方向のスケール係数（em 単位 → ピクセル）。
    scale: f32,
    /// オフセット（em 中心 → orb 中心 - bbox 半幅）。
    offset_x: f32,
    offset_y: f32,
    /// orb 中心 (px)。
    cx: f32,
    cy: f32,
}

impl GlyphPathBuilder {
    /// em 座標 (x_em, y_em) を tiny-skia ピクセル座標に変換する。
    /// y は反転（フォント上向き → スクリーン下向き）。
    #[inline]
    fn map(&self, x_em: f32, y_em: f32) -> (f32, f32) {
        let px = self.cx + (x_em + self.offset_x) * self.scale;
        let py = self.cy - (y_em + self.offset_y) * self.scale;
        (px, py)
    }
}

impl OutlineBuilder for GlyphPathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.map(x, y);
        self.pb.move_to(px, py);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.map(x, y);
        self.pb.line_to(px, py);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (p1x, p1y) = self.map(x1, y1);
        let (px, py) = self.map(x, y);
        self.pb.quad_to(p1x, p1y, px, py);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (p1x, p1y) = self.map(x1, y1);
        let (p2x, p2y) = self.map(x2, y2);
        let (px, py) = self.map(x, y);
        self.pb.cubic_to(p1x, p1y, p2x, p2y, px, py);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

/// 同梱フォント `font` に文字 `ch` のグリフが収録されているかを返す。
///
/// CLI が起動直後に「`--glyph-char` の文字が同梱 subset に無い」ことを警告するための
/// public ヘルパ。`Face::glyph_index(ch)` だけで判定し、bbox / outline までは取りに
/// 行かない（warning 用途には十分）。フォント読み込みに失敗した場合は `false`。
pub fn has_glyph(font: GlyphFontId, ch: char) -> bool {
    face_for(font).and_then(|f| f.glyph_index(ch)).is_some()
}

/// 1 文字分のグリフを `tiny_skia::Path` に焼き、orb 中心に center 揃えで返す。
///
/// 戻り値は描画可能な `Path`（中身が空のグリフや未収録文字では `None`）。
/// `radius_px` は orb の見た目半径相当のピクセル長。グリフはこの 2× の正方領域に
/// 収まるよう等比スケールされ、bbox 中心が `center` に来るよう平行移動される。
pub fn build_glyph_path(
    font: GlyphFontId,
    ch: char,
    center: (f32, f32),
    radius_px: f32,
) -> Option<Path> {
    if radius_px <= 0.0 {
        return None;
    }
    let face = face_for(font)?;
    let glyph_id = face.glyph_index(ch)?;
    // bbox 取得は必須。バイトごとのアウトラインが空でも bbox は取れることが多いが、
    // 取れないケース（スペース等）はそもそも描画する意味がないのでスキップ。
    let bbox: Rect = face.glyph_bounding_box(glyph_id)?;

    // bbox は em 単位。正方領域 (radius * 2) に収まる等比スケール。
    let bbox_w = (bbox.x_max - bbox.x_min) as f32;
    let bbox_h = (bbox.y_max - bbox.y_min) as f32;
    if bbox_w <= 0.0 || bbox_h <= 0.0 {
        return None;
    }
    let max_extent = bbox_w.max(bbox_h);
    // radius_px は半径相当なので、収めたい正方領域の辺長は 2 * radius_px。
    let scale = (2.0 * radius_px) / max_extent;

    // bbox 中心を origin に合わせる平行移動（em 単位）。
    let center_x_em = (bbox.x_min as f32 + bbox.x_max as f32) * 0.5;
    let center_y_em = (bbox.y_min as f32 + bbox.y_max as f32) * 0.5;

    let mut builder = GlyphPathBuilder {
        pb: PathBuilder::new(),
        scale,
        offset_x: -center_x_em,
        offset_y: -center_y_em,
        cx: center.0,
        cy: center.1,
    };

    // outline_glyph は描画コマンドが 1 つでもあれば bbox を返す。空なら None。
    face.outline_glyph(glyph_id, &mut builder)?;
    builder.pb.finish()
}

fn render_glyph_binary_mask(font: GlyphFontId, ch: char, size: u32) -> Vec<u8> {
    let s = size.max(1);
    let mut pix = match Pixmap::new(s, s) {
        Some(p) => p,
        None => return vec![0u8; (s as usize) * (s as usize)],
    };
    let center = (s as f32 * 0.5, s as f32 * 0.5);
    let radius = (s as f32) * GLYPH_SDF_RADIUS_FACTOR * GLYPH_SDF_CONTENT_SPAN;
    let path = match build_glyph_path(font, ch, center, radius) {
        Some(p) => p,
        None => return vec![0u8; (s as usize) * (s as usize)],
    };
    let paint = Paint {
        shader: Shader::SolidColor(Color::from_rgba8(255, 255, 255, 255)),
        anti_alias: true,
        ..Default::default()
    };
    pix.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    let raw = pix.data();
    let mut out = Vec::with_capacity((s as usize) * (s as usize));
    for px in raw.chunks_exact(4) {
        out.push(px[3]);
    }
    out
}

fn edt_1d(f: &[f32]) -> Vec<f32> {
    let n = f.len();
    let mut d = vec![0.0; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        let qf = q as f32;
        let mut s =
            ((f[q] + qf * qf) - (f[v[k]] + (v[k] as f32).powi(2))) / (2.0 * (qf - v[k] as f32));
        while k > 0 && s <= z[k] {
            k -= 1;
            s = ((f[q] + qf * qf) - (f[v[k]] + (v[k] as f32).powi(2))) / (2.0 * (qf - v[k] as f32));
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let dx = q as f32 - v[k] as f32;
        *out = dx * dx + f[v[k]];
    }
    d
}

fn edt_2d(features: &[bool], size: usize) -> Vec<f32> {
    const INF: f32 = 1.0e12;
    let mut tmp = vec![0.0f32; size * size];
    for x in 0..size {
        let mut col = vec![INF; size];
        for y in 0..size {
            if features[y * size + x] {
                col[y] = 0.0;
            }
        }
        let dist = edt_1d(&col);
        for y in 0..size {
            tmp[y * size + x] = dist[y];
        }
    }
    let mut out = vec![0.0f32; size * size];
    for y in 0..size {
        let row = &tmp[y * size..(y + 1) * size];
        let dist = edt_1d(row);
        out[y * size..(y + 1) * size].copy_from_slice(&dist);
    }
    out
}

/// 二値 mask (`size × size`、各バイト `>= 128` を inside とみなす) を
/// signed-distance field の 8-bit R texture に変換する。
///
/// Glyph (フォント由来) と Image (#217、画像シルエット由来) で共有する mask→SDF 段。
/// inside 判定 → 内側 / 外側それぞれの 2D Euclidean Distance Transform →
/// `signed_px` → `size * GLYPH_SDF_MAX_DIST_FACTOR` で正規化 → `[0, 255]` に量子化、
/// という [`render_glyph_sdf`] が従来 inline で持っていた算術をそのまま括り出したもの。
/// バイト列のフォーマットは [`render_glyph_sdf`] と完全同一なので、Image 経路は
/// この関数で得た SDF を glyph と同じレンダラ（CPU の [`render_glyph_orb`] / GPU の
/// glyph SDF パイプライン）にそのまま渡せる。
///
/// `mask` が全 0（描画なし）のときは全 0 の SDF を返す（[`render_glyph_sdf`] の
/// tofu 契約と同じ）。`mask.len()` は `(size * size)` 以上を想定する（不足分は
/// パニックするので、呼び出し側で長さを保証すること）。
pub fn mask_to_sdf(mask: &[u8], size: u32) -> Vec<u8> {
    let s = size.max(1) as usize;
    if mask.iter().take(s * s).all(|&b| b == 0) {
        return vec![0u8; s * s];
    }
    let inside: Vec<bool> = mask.iter().take(s * s).map(|&b| b >= 128).collect();
    let outside: Vec<bool> = inside.iter().map(|&on| !on).collect();
    let dist_to_inside = edt_2d(&inside, s);
    let dist_to_outside = edt_2d(&outside, s);
    let norm = (size as f32 * GLYPH_SDF_MAX_DIST_FACTOR).max(1.0);
    let mut out = Vec::with_capacity(s * s);
    for i in 0..(s * s) {
        let signed_px = if inside[i] {
            dist_to_outside[i].sqrt() - 0.5
        } else {
            0.5 - dist_to_inside[i].sqrt()
        };
        let signed_unit = (signed_px / norm).clamp(-1.0, 1.0);
        let byte = ((signed_unit * 0.5 + 0.5) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
        out.push(byte);
    }
    out
}

/// Glyph 1 文字の signed-distance field を `size × size` の 8-bit R texture として返す。
///
/// 値域は `[-1, +1]` を `[0, 255]` に写したもの。0.5 (= 128 前後) が輪郭、
/// 1.0 側ほど内側、0.0 側ほど外側を表す。距離は glyph 全半径ではなく
/// `size * GLYPH_SDF_MAX_DIST_FACTOR` の「エッジ近傍 falloff 幅」で正規化する。
/// これにより `r = 1 - signed_unit` が「edge からどれだけ内側か」の共通尺度になる。
///
/// フォント由来の binary mask を作る段だけが glyph 固有で、mask→SDF 段は
/// [`mask_to_sdf`] に切り出して Image (#217) と共有する。
pub fn render_glyph_sdf(font: GlyphFontId, ch: char, size: u32) -> Vec<u8> {
    let mask = render_glyph_binary_mask(font, ch, size);
    mask_to_sdf(&mask, size)
}

type GlyphSdfKey = (GlyphFontId, u32, u32);

fn glyph_sdf_cache() -> &'static Mutex<HashMap<GlyphSdfKey, Arc<[u8]>>> {
    static CELL: OnceLock<Mutex<HashMap<GlyphSdfKey, Arc<[u8]>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_glyph_sdf(font: GlyphFontId, ch: char, size: u32) -> Arc<[u8]> {
    let key = (font, ch as u32, size);
    if let Some(v) = glyph_sdf_cache()
        .lock()
        .expect("glyph sdf cache poisoned")
        .get(&key)
    {
        return Arc::clone(v);
    }
    let sdf: Arc<[u8]> = Arc::from(render_glyph_sdf(font, ch, size));
    glyph_sdf_cache()
        .lock()
        .expect("glyph sdf cache poisoned")
        .insert(key, Arc::clone(&sdf));
    sdf
}

/// GPU (#212) entry point: return the cached SDF bytes **and** the chosen square
/// size for an orb of pixel `radius`, picking the same size the CPU
/// [`render_glyph_orb`] would (`glyph_sdf_size_for_radius`). The GPU path uploads
/// these bytes as an `R8Unorm` texture and samples them with a bilinear sampler,
/// exactly mirroring the CPU `sample_sdf_bilinear` so the two fills agree
/// (pre-bleed) within a loose tolerance.
///
/// Returns `None` for a non-positive radius or an unknown/empty glyph (all-zero
/// SDF), so the caller can skip GPU glyph upload and leave the frame
/// background-only — matching the CPU "draw nothing for tofu" contract.
pub fn cached_glyph_sdf_for_radius(
    font: GlyphFontId,
    ch: char,
    radius: f32,
) -> Option<(Arc<[u8]>, u32)> {
    if radius <= 0.0 {
        return None;
    }
    let size = glyph_sdf_size_for_radius(radius);
    let sdf = cached_glyph_sdf(font, ch, size);
    if sdf.iter().all(|&b| b == 0) {
        return None;
    }
    Some((sdf, size))
}

/// The glyph SDF content-span constant (`1/√2`), exposed for the GPU shader so the
/// UV mapping in `orb_glyph.wgsl` matches the CPU `render_glyph_orb` exactly.
pub const GLYPH_SDF_CONTENT_SPAN_PUB: f32 = GLYPH_SDF_CONTENT_SPAN;

#[inline]
fn glyph_sdf_size_for_radius(radius: f32) -> u32 {
    if radius <= 0.0 {
        return DEFAULT_GLYPH_SDF_SIZE;
    }
    let desired = (radius * 2.25).ceil().max(DEFAULT_GLYPH_SDF_SIZE as f32) as u32;
    desired.next_power_of_two().min(MAX_GLYPH_SDF_SIZE)
}

fn sample_sdf_bilinear(bytes: &[u8], size: usize, u: f32, v: f32) -> f32 {
    let x = u.clamp(0.0, 1.0) * (size.saturating_sub(1) as f32);
    let y = v.clamp(0.0, 1.0) * (size.saturating_sub(1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(size - 1);
    let y1 = (y0 + 1).min(size - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let idx = |xx: usize, yy: usize| yy * size + xx;
    let p00 = bytes[idx(x0, y0)] as f32 / 255.0;
    let p10 = bytes[idx(x1, y0)] as f32 / 255.0;
    let p01 = bytes[idx(x0, y1)] as f32 / 255.0;
    let p11 = bytes[idx(x1, y1)] as f32 / 255.0;
    let top = p00 + (p10 - p00) * tx;
    let bottom = p01 + (p11 - p01) * tx;
    top + (bottom - top) * ty
}

fn blend_source_over(pixmap: &mut Pixmap, x: u32, y: u32, rgb: [u8; 3], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let width = pixmap.width() as usize;
    let idx = ((y as usize) * width + x as usize) * 4;
    let dst = &mut pixmap.data_mut()[idx..idx + 4];
    let dst_a = dst[3] as f32 / 255.0;
    let one_minus_a = 1.0 - alpha;
    let src_r = rgb[0] as f32 / 255.0 * alpha;
    let src_g = rgb[1] as f32 / 255.0 * alpha;
    let src_b = rgb[2] as f32 / 255.0 * alpha;
    let dst_r = dst[0] as f32 / 255.0;
    let dst_g = dst[1] as f32 / 255.0;
    let dst_b = dst[2] as f32 / 255.0;
    dst[0] = ((src_r + dst_r * one_minus_a) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[1] = ((src_g + dst_g * one_minus_a) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[2] = ((src_b + dst_b * one_minus_a) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[3] = ((alpha + dst_a * one_minus_a) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
}

/// 単一の Glyph orb を pixmap に SourceOver で重ねる。
///
/// Circle と同じ `falloff_curve` を使うため、入力は `blur` / `style` / `opacity`
/// をそのまま受ける。`rotation` は glyph テクスチャ空間に対する回転角 (rad)。
#[allow(clippy::too_many_arguments)]
pub fn render_glyph_orb(
    pixmap: &mut Pixmap,
    center: (f32, f32),
    radius: f32,
    rgb: [u8; 3],
    blur: f32,
    opacity: f32,
    profile: FalloffProfile,
    font: GlyphFontId,
    ch: char,
    rotation: f32,
) {
    if radius <= 0.0 {
        return;
    }
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    let sdf_size = glyph_sdf_size_for_radius(radius);
    let sdf = cached_glyph_sdf(font, ch, sdf_size);
    if sdf.iter().all(|&b| b == 0) {
        return;
    }
    let size = sdf_size as usize;
    let (cx, cy) = center;
    let cos_a = rotation.cos();
    let sin_a = rotation.sin();
    let width = pixmap.width();
    let height = pixmap.height();
    let min_x = (cx - radius).floor().max(0.0) as u32;
    let min_y = (cy - radius).floor().max(0.0) as u32;
    let max_x = (cx + radius).ceil().min(width as f32) as u32;
    let max_y = (cy + radius).ceil().min(height as f32) as u32;
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let dx = px - cx;
            let dy = py - cy;
            let rx = cos_a * dx - sin_a * dy;
            let ry = sin_a * dx + cos_a * dy;
            let u = rx / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;
            let v = ry / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let sdf01 = sample_sdf_bilinear(&sdf, size, u, v);
            let signed_unit = sdf01 * 2.0 - 1.0;
            let r = 1.0 - signed_unit;
            let alpha = falloff_curve(profile, r, blur, opacity);
            blend_source_over(pixmap, x, y, rgb, alpha);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_bytes_is_nonempty_and_parses() {
        let bytes = font_bytes(GlyphFontId::NotoSymbols2);
        assert!(bytes.len() > 1024, "font bytes too small: {}", bytes.len());
        let face = Face::parse(bytes, 0).expect("Noto Symbols 2 must parse");
        // フォントには最低限ある程度のグリフが含まれているはず。
        assert!(face.number_of_glyphs() > 10);
    }

    #[test]
    fn known_symbol_resolves_to_glyph() {
        // ☆ (U+2606) は Noto Sans Symbols 2 に収録されている標準的な記号。
        let face = face_for(GlyphFontId::NotoSymbols2).expect("font must load");
        let id = face.glyph_index('☆').expect("☆ should resolve");
        assert!(face.glyph_bounding_box(id).is_some());
    }

    #[test]
    fn unknown_glyph_returns_none_path() {
        // 絵文字（ピザ U+1F355）は Symbols 2 subset には含まれない見込み。
        // 含まれていても None を返すこと自体はバグではないので、
        // 「panic しない」だけを確認する。
        let _ = build_glyph_path(GlyphFontId::NotoSymbols2, '\u{1F355}', (32.0, 32.0), 16.0);
    }

    #[test]
    fn build_glyph_path_returns_some_for_known_char() {
        let path = build_glyph_path(GlyphFontId::NotoSymbols2, '☆', (32.0, 32.0), 16.0);
        assert!(path.is_some(), "☆ outline should produce a non-empty path");
    }

    #[test]
    fn render_glyph_orb_paints_pixels() {
        // 64x64 の Pixmap に ☆ を描いて、alpha が立っているピクセルが
        // 一定数以上あることを確認する。
        let mut pix = Pixmap::new(64, 64).unwrap();
        render_glyph_orb(
            &mut pix,
            (32.0, 32.0),
            20.0,
            [255, 255, 255],
            0.5,
            1.0,
            FalloffProfile::Rim,
            GlyphFontId::NotoSymbols2,
            '☆',
            0.0,
        );
        let lit = pix.data().chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(
            lit > 32,
            "rendering ☆ should produce at least 32 lit pixels, got {lit}"
        );
    }

    #[test]
    fn render_glyph_orb_zero_radius_no_panic() {
        let mut pix = Pixmap::new(16, 16).unwrap();
        render_glyph_orb(
            &mut pix,
            (8.0, 8.0),
            0.0,
            [255, 255, 255],
            0.5,
            1.0,
            FalloffProfile::Rim,
            GlyphFontId::NotoSymbols2,
            '☆',
            0.0,
        );
        // 何も描かれていないことを確認。
        let lit = pix.data().chunks_exact(4).filter(|p| p[3] > 0).count();
        assert_eq!(lit, 0);
    }

    #[test]
    fn render_glyph_orb_zero_opacity_no_paint() {
        let mut pix = Pixmap::new(64, 64).unwrap();
        render_glyph_orb(
            &mut pix,
            (32.0, 32.0),
            20.0,
            [255, 255, 255],
            0.5,
            0.0,
            FalloffProfile::Rim,
            GlyphFontId::NotoSymbols2,
            '☆',
            0.0,
        );
        let lit = pix.data().chunks_exact(4).filter(|p| p[3] > 0).count();
        assert_eq!(lit, 0, "opacity=0 must not paint anything");
    }

    // Glyph SDF の単体テスト群。WebGL / CPU の両 glyph 経路で使う canonical texture。

    /// size を変えれば長さが size² になる。基本契約。
    #[test]
    fn glyph_sdf_size_matches_input() {
        for size in [16u32, 32, 64, 128, 256] {
            let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '☆', size);
            assert_eq!(
                bytes.len(),
                (size as usize) * (size as usize),
                "size={size} must produce {} bytes",
                (size as usize) * (size as usize)
            );
        }
    }

    /// 既知文字 ☆ で inside 側のサンプルが一定数以上あること。
    #[test]
    fn glyph_sdf_known_char_has_inside_pixels() {
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '☆', 64);
        let lit = bytes.iter().filter(|&&b| b > 127).count();
        assert!(
            lit >= 32,
            "rendering ☆ at 64x64 should produce >=32 inside pixels, got {lit}"
        );
        assert!(
            lit > 64 * 64 / 20,
            "rendering ☆ at 64x64 should produce >=5% inside pixels, got {lit}"
        );
    }

    /// 未収録文字（絵文字 U+1F355 ピザ等）で全ピクセル 0。
    /// tofu 出力ではなく「何も描かない」が Phase A の方針。WebGL 経路でも
    /// 同じ契約を保つことで、shape='glyph' + 未収録文字 = 完全透明 orb になる。
    #[test]
    fn glyph_sdf_unknown_char_returns_empty_or_zero() {
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '\u{1F355}', 32);
        assert_eq!(bytes.len(), 32 * 32);
        assert!(
            bytes.iter().all(|&b| b == 0),
            "unknown char must produce all-zero sdf"
        );
    }

    #[test]
    fn glyph_sdf_has_both_inside_and_outside_regions() {
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '☆', 64);
        assert!(
            bytes.iter().any(|&b| b < 120),
            "must contain outside samples"
        );
        assert!(
            bytes.iter().any(|&b| b > 136),
            "must contain inside samples"
        );
    }

    #[test]
    fn glyph_sdf_size_scales_up_for_large_cpu_orbs() {
        assert_eq!(glyph_sdf_size_for_radius(8.0), DEFAULT_GLYPH_SDF_SIZE);
        assert_eq!(glyph_sdf_size_for_radius(160.0), 512);
        assert_eq!(glyph_sdf_size_for_radius(400.0), 1024);
    }

    /// #217: `mask_to_sdf` 抽出後も `render_glyph_sdf` の出力が
    /// 「binary mask → mask_to_sdf」と byte 完全一致すること。これがリグレッション
    /// 検出の正本。glyph と image が同じ mask→SDF 段を共有する根拠でもある。
    #[test]
    fn render_glyph_sdf_equals_mask_to_sdf_of_binary_mask() {
        for ch in ['☆', '♪', '♥'] {
            for size in [16u32, 64, 256] {
                let mask = render_glyph_binary_mask(GlyphFontId::NotoSymbols2, ch, size);
                let via_mask = mask_to_sdf(&mask, size);
                let via_glyph = render_glyph_sdf(GlyphFontId::NotoSymbols2, ch, size);
                assert_eq!(
                    via_glyph, via_mask,
                    "render_glyph_sdf must equal mask_to_sdf(binary_mask) for ch={ch:?} size={size}"
                );
            }
        }
    }

    /// `mask_to_sdf` 単体のサニティ: 中央に inside ブロックがある mask は、内側で
    /// 128 超・外側で 128 未満になり、長さは size² になる。
    #[test]
    fn mask_to_sdf_basic_inside_outside() {
        let size = 32u32;
        let s = size as usize;
        let mut mask = vec![0u8; s * s];
        // 中央 8x8 を inside にする。
        for y in 12..20 {
            for x in 12..20 {
                mask[y * s + x] = 255;
            }
        }
        let sdf = mask_to_sdf(&mask, size);
        assert_eq!(sdf.len(), s * s);
        assert!(sdf[16 * s + 16] > 128, "center of inside block must be inside (>128)");
        assert!(sdf[0] < 128, "far corner must be outside (<128)");
    }

    /// 全 0 mask は全 0 SDF（tofu 契約）。
    #[test]
    fn mask_to_sdf_all_zero_is_all_zero() {
        let sdf = mask_to_sdf(&vec![0u8; 16 * 16], 16);
        assert!(sdf.iter().all(|&b| b == 0));
    }

    #[test]
    fn rotated_glyph_does_not_clip_severely() {
        let mut plain = Pixmap::new(256, 256).unwrap();
        let mut rotated = Pixmap::new(256, 256).unwrap();
        render_glyph_orb(
            &mut plain,
            (128.0, 128.0),
            80.0,
            [255, 255, 255],
            0.5,
            1.0,
            FalloffProfile::Rim,
            GlyphFontId::NotoSymbols2,
            '☆',
            0.0,
        );
        render_glyph_orb(
            &mut rotated,
            (128.0, 128.0),
            80.0,
            [255, 255, 255],
            0.5,
            1.0,
            FalloffProfile::Rim,
            GlyphFontId::NotoSymbols2,
            '☆',
            std::f32::consts::FRAC_PI_4,
        );
        let lit_plain = plain.data().chunks_exact(4).filter(|p| p[3] > 0).count() as f32;
        let lit_rotated = rotated.data().chunks_exact(4).filter(|p| p[3] > 0).count() as f32;
        let ratio = lit_rotated / lit_plain.max(1.0);
        assert!(
            ratio > 0.9,
            "rotated glyph should keep most lit pixels; ratio={ratio}"
        );
    }
}
