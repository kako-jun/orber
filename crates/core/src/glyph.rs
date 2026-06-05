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
//!   呼び出し側（GPU の glyph レンダラ [`crate::gpu`]）はキャッシュ済み texture を
//!   bilinear sampling で使い回す
//! - グリフが見つからない場合 ([`Face::glyph_index`] が `None` を返す or `outline_glyph`
//!   が空アウトラインを返す) は **何も描画しない**。tofu は出さない。Phase A の方針として、
//!   絵文字など Symbols 2 に無い文字は静かに無視する
//! - フォントのアウトラインは Y 軸が上向き（font em スケール）。ラスタライザ (zeno) は
//!   Y 軸下向きなので、`OutlineBuilder` 内で y を反転して積み込む。SDF の fill は
//!   Skia lowp から zeno (pure Rust, wasm 可) に置換済み (#223)
//! - センタリングは `glyph_bounding_box` の中央を orb 中心に合わせ、半径 × 2 の正方領域に
//!   収まるよう em-square 基準でスケールする

use std::collections::HashMap;
use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::{Arc, Mutex, OnceLock};
use ttf_parser::{Face, OutlineBuilder, Rect};
use zeno::{Command, Mask, Point};

/// WebGL / preview path で使う既定 Glyph SDF texture size。
pub const DEFAULT_GLYPH_SDF_SIZE: u32 = 256;
const MAX_GLYPH_SDF_SIZE: u32 = 1024;
const GLYPH_SDF_RADIUS_FACTOR: f32 = 0.45;
const GLYPH_SDF_CONTENT_SPAN: f32 = FRAC_1_SQRT_2;
const GLYPH_SDF_MAX_DIST_FACTOR: f32 = 0.06;

/// orber-core が同梱するフォント識別子。
///
/// 将来的に複数フォントを同梱する余地を残すため `enum` にしている。Phase A では
/// `NotoSymbols2` の 1 種類のみ。`Copy + Eq` の軽量 enum なので、[`crate::orb::OrbShape`]
/// の `Glyph` アームを安価に複製できる（#217 で `OrbShape` 自体は `Image` の
/// `Arc<[u8]>` のため `Copy` ではなく `Clone` になった）。
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

/// zeno のパスコマンド列 (`Vec<zeno::Command>`) にアウトラインを積む
/// `OutlineBuilder` 実装。
///
/// フォントは Y 軸上向き、zeno のラスタライザは Y 軸下向き (`Origin::TopLeft`) なので、
/// ここで y を反転する。同時に em スケールから「orb 半径×2 の正方領域」スケールへの
/// 線形変換を適用する。`map()` の幾何は Skia lowp 時代から不変 (#223)。
struct GlyphPathBuilder {
    cmds: Vec<Command>,
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
    /// em 座標 (x_em, y_em) を zeno ピクセル座標 (`Point`) に変換する。
    /// y は反転（フォント上向き → スクリーン下向き）。
    #[inline]
    fn map(&self, x_em: f32, y_em: f32) -> Point {
        let px = self.cx + (x_em + self.offset_x) * self.scale;
        let py = self.cy - (y_em + self.offset_y) * self.scale;
        Point::new(px, py)
    }
}

impl OutlineBuilder for GlyphPathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.cmds.push(Command::MoveTo(self.map(x, y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cmds.push(Command::LineTo(self.map(x, y)));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.cmds
            .push(Command::QuadTo(self.map(x1, y1), self.map(x, y)));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.cmds.push(Command::CurveTo(
            self.map(x1, y1),
            self.map(x2, y2),
            self.map(x, y),
        ));
    }
    fn close(&mut self) {
        self.cmds.push(Command::Close);
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

/// 1 文字分のグリフを zeno のパスコマンド列 (`Vec<zeno::Command>`) に焼き、
/// orb 中心に center 揃えで返す。
///
/// 戻り値は描画可能なコマンド列（中身が空のグリフや未収録文字では `None`）。
/// `radius_px` は orb の見た目半径相当のピクセル長。グリフはこの 2× の正方領域に
/// 収まるよう等比スケールされ、bbox 中心が `center` に来るよう平行移動される。
pub fn build_glyph_path(
    font: GlyphFontId,
    ch: char,
    center: (f32, f32),
    radius_px: f32,
) -> Option<Vec<Command>> {
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
        cmds: Vec::new(),
        scale,
        offset_x: -center_x_em,
        offset_y: -center_y_em,
        cx: center.0,
        cy: center.1,
    };

    // outline_glyph は描画コマンドが 1 つでもあれば bbox を返す。空なら None。
    face.outline_glyph(glyph_id, &mut builder)?;
    if builder.cmds.is_empty() {
        return None;
    }
    Some(builder.cmds)
}

fn render_glyph_binary_mask(font: GlyphFontId, ch: char, size: u32) -> Vec<u8> {
    let s = size.max(1);
    let n = (s as usize) * (s as usize);
    let center = (s as f32 * 0.5, s as f32 * 0.5);
    let radius = (s as f32) * GLYPH_SDF_RADIUS_FACTOR * GLYPH_SDF_CONTENT_SPAN;
    let cmds = match build_glyph_path(font, ch, center, radius) {
        Some(c) => c,
        None => return vec![0u8; n],
    };
    // zeno: Alpha (1 byte/px) coverage を size×size に直接ラスタライズする。
    // 既定の Fill::NonZero は旧 Skia lowp の FillRule::Winding と等価で、Origin は
    // TopLeft (Y 下向き)。GlyphPathBuilder 側で font Y-up → screen Y-down を反転済み。
    // 明示 size を与えると buffer は size×size、placement も同サイズになる。
    let (coverage, placement) = Mask::new(&cmds).size(s, s).render();
    // 念のため placement / 長さを確認。想定外（zeno の size 契約違反）なら全 0 で
    // 返して描画スキップ（tofu を出さない契約を保つ）。
    if placement.width != s || placement.height != s || coverage.len() != n {
        return vec![0u8; n];
    }
    coverage
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
/// この関数で得た SDF を glyph と同じ GPU の glyph SDF パイプラインにそのまま渡せる。
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

/// 画像シルエットを `size × size` の SDF（[`mask_to_sdf`] と同フォーマット）に変換する
/// （#217、`web/src/lib/jsGlyphSdf.ts::generateImageSdf` の Rust 移植）。
///
/// 入力はデコード済みの [`image::RgbaImage`]（デコードは wasm 配慮で CLI 側に置く。
/// core の image dep は png のみ）。Web の `generateImageSdf` と **1:1 同一**の
/// ヒューリスティックで inside mask を作り、[`mask_to_sdf`] で SDF にする。
///
/// 処理:
/// 1. **contain リサンプル**でアスペクト維持のまま `size × size` の透明キャンバスに
///    レターボックスして描く（Canvas の `drawImage(... dx,dy,dw,dh)` 相当）。dx/dy/dw/dh
///    は Web と同式。リサンプルは bilinear（`FilterType::Triangle`、cluster.rs と同じ）。
/// 2. **評価範囲は描画矩形 `dx..dx+dw, dy..dy+dh` に限定**（#174 のレタボ修正）。
/// 3. 描画矩形内で `alpha < 255` のピクセルが **1% 以上**（`alphaPixelCount*100 >=
///    drawnPixelCount`）なら **alpha 経路**（`alpha >= 128` を inside）。そうでなければ
///    **輝度経路** `Y = 0.299R + 0.587G + 0.114B`、平均輝度境界、
///    **auto-polarity（少数派 = 被写体）**。`invert` は #181 で削除済み＝移植しない。
/// 4. `insideCount == 0` または `== drawnPixelCount`（コントラスト無し）は **`None`**
///    を返す（#169 相当。CLI 側で明示エラーにする）。
///
/// 戻り値の `Vec<u8>` は長さ `size * size`、glyph SDF と同フォーマット（R8、128≈edge）。
pub fn image_rgba_to_sdf(rgba: &image::RgbaImage, size: u32) -> Option<Vec<u8>> {
    let s = size.max(1) as usize;
    let bw = rgba.width().max(1);
    let bh = rgba.height().max(1);

    // contain リサンプル。Web: scale = min(s/bw, s/bh)、dw/dh = round(bw/bh * scale)、
    // dx/dy = round((s - dw/dh) / 2)。
    let scale = (s as f32 / bw as f32).min(s as f32 / bh as f32);
    let dw = ((bw as f32 * scale).round() as u32).max(1);
    let dh = ((bh as f32 * scale).round() as u32).max(1);
    // dx/dy は Web の `Math.round((s - dw) / 2)` と同式。dw/dh <= s が contain で
    // 保証されるので非負だが、丸め後に念のため 0 で下限を取る。
    let dx = (((s as f32 - dw as f32) / 2.0).round() as i64).max(0) as usize;
    let dy = (((s as f32 - dh as f32) / 2.0).round() as i64).max(0) as usize;

    // 透明キャンバス (alpha=0) に bilinear リサンプル結果をレターボックス配置する。
    // Canvas の clearRect + drawImage と同じ「描画矩形の外は alpha=0 のまま」を再現。
    let resized = image::imageops::resize(rgba, dw, dh, image::imageops::FilterType::Triangle);
    let mut canvas = image::RgbaImage::from_pixel(s as u32, s as u32, image::Rgba([0, 0, 0, 0]));
    image::imageops::replace(&mut canvas, &resized, dx as i64, dy as i64);

    // 評価範囲は描画矩形に限定。dx..dx+dw / dy..dy+dh は canvas 内に収まる。
    let dw = dw as usize;
    let dh = dh as usize;
    let drawn_pixel_count = dw * dh;
    let px = |x: usize, y: usize| -> &image::Rgba<u8> { canvas.get_pixel(x as u32, y as u32) };

    // #171 + #174: 描画矩形内で alpha<255 が 1% 以上なら alpha 経路。
    let mut alpha_pixel_count = 0usize;
    for y in dy..dy + dh {
        for x in dx..dx + dw {
            if px(x, y)[3] < 255 {
                alpha_pixel_count += 1;
            }
        }
    }
    let has_meaningful_alpha = alpha_pixel_count * 100 >= drawn_pixel_count;

    let mut inside = vec![0u8; s * s];
    let mut inside_count = 0usize;
    if has_meaningful_alpha {
        // alpha しきい値経路（描画矩形内のみ）。
        for y in dy..dy + dh {
            for x in dx..dx + dw {
                if px(x, y)[3] >= 128 {
                    inside[y * s + x] = 255;
                    inside_count += 1;
                }
            }
        }
    } else {
        // 輝度しきい値経路（auto-polarity: 少数派 = 被写体）。
        let mut sum_y = 0.0f32;
        let mut y_buf = vec![0.0f32; s * s];
        for y in dy..dy + dh {
            for x in dx..dx + dw {
                let p = px(x, y);
                let yv = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
                y_buf[y * s + x] = yv;
                sum_y += yv;
            }
        }
        let avg_y = sum_y / drawn_pixel_count as f32;
        let mut dark_count = 0usize;
        for y in dy..dy + dh {
            for x in dx..dx + dw {
                if y_buf[y * s + x] < avg_y {
                    dark_count += 1;
                }
            }
        }
        // Web parity: `darkCount < drawnPixelCount / 2` uses float division in
        // jsGlyphSdf.ts. Use `2 * dark_count < drawn_pixel_count` (not integer
        // `dark_count < drawn_pixel_count / 2`, which floors and flips the
        // polarity at the odd-count tie `dark_count == floor(N/2)`).
        let inside_is_dark = 2 * dark_count < drawn_pixel_count;
        for y in dy..dy + dh {
            for x in dx..dx + dw {
                let yv = y_buf[y * s + x];
                let is_inside = if inside_is_dark {
                    yv < avg_y
                } else {
                    yv >= avg_y
                };
                if is_inside {
                    inside[y * s + x] = 255;
                    inside_count += 1;
                }
            }
        }
    }

    // #169: 全 inside でも全 outside でもコントラスト 0 として None。
    if inside_count == 0 || inside_count == drawn_pixel_count {
        return None;
    }

    Some(mask_to_sdf(&inside, size))
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
/// size for an orb of pixel `radius` (`glyph_sdf_size_for_radius`). The GPU path
/// uploads these bytes as an `R8Unorm` texture and samples them with a bilinear
/// sampler.
///
/// Returns `None` for a non-positive radius or an unknown/empty glyph (all-zero
/// SDF), so the caller can skip GPU glyph upload and leave the frame
/// background-only — "draw nothing for tofu".
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
/// UV mapping in `orb_glyph.wgsl` matches the SDF bake (`render_glyph_binary_mask`).
pub const GLYPH_SDF_CONTENT_SPAN_PUB: f32 = GLYPH_SDF_CONTENT_SPAN;

#[inline]
fn glyph_sdf_size_for_radius(radius: f32) -> u32 {
    if radius <= 0.0 {
        return DEFAULT_GLYPH_SDF_SIZE;
    }
    let desired = (radius * 2.25).ceil().max(DEFAULT_GLYPH_SDF_SIZE as f32) as u32;
    desired.next_power_of_two().min(MAX_GLYPH_SDF_SIZE)
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

    /// #223: zeno ラスタライザが**非退化なシルエット**を出すことの構造アサート。
    /// ☆ を 256×256 で SDF 化したとき、(1) inside (SDF byte > 128) 数が「0 でなく
    /// 全面でもない」妥当範囲にあること、(2) inside ピクセルの**重心が画像中央付近**に
    /// あること（グリフが中央揃えで適正サイズに焼けている証拠）を確認する。
    /// トートロジーを避けるため固定 byte 値は見ず、空 SDF・全面塗り・偏った配置を弾く。
    /// 中央 1 点ではなく重心を見るのは、☆ の幾何中心は星形の hollow で空くため。
    #[test]
    fn glyph_sdf_zeno_silhouette_is_non_degenerate() {
        let size = 256usize;
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '☆', size as u32);
        assert_eq!(bytes.len(), size * size);
        let total = size * size;

        let mut inside = 0usize;
        let mut sum_x = 0usize;
        let mut sum_y = 0usize;
        for y in 0..size {
            for x in 0..size {
                if bytes[y * size + x] > 128 {
                    inside += 1;
                    sum_x += x;
                    sum_y += y;
                }
            }
        }

        // (1a) 0 でないこと（描画されている）。経験的下限 2%（適正サイズで焼けている）。
        assert!(
            inside > total / 50,
            "silhouette too sparse ({inside}/{total}); glyph likely shrank away or empty"
        );
        // (1b) 全面でないこと（背景が残りシルエットになっている）。星形は大半が外側。
        assert!(
            inside < total / 2,
            "silhouette must not fill the whole field (got {inside}/{total}); \
             a full fill would mean the glyph degenerated to a solid block"
        );

        // (2) inside の重心が画像中央 (128,128) の ±10% (≈±25px) 以内。中央揃え +
        // 等比スケールが効いている証拠。偏って焼けると重心がずれて落ちる。
        let cx = sum_x / inside;
        let cy = sum_y / inside;
        let tol = size / 10;
        let center = size / 2;
        assert!(
            cx.abs_diff(center) <= tol && cy.abs_diff(center) <= tol,
            "glyph silhouette must be centered: centroid=({cx},{cy}), \
             expected near ({center},{center}) within ±{tol}px"
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
        assert!(
            sdf[16 * s + 16] > 128,
            "center of inside block must be inside (>128)"
        );
        assert!(sdf[0] < 128, "far corner must be outside (<128)");
    }

    /// 全 0 mask は全 0 SDF（tofu 契約）。
    #[test]
    fn mask_to_sdf_all_zero_is_all_zero() {
        let sdf = mask_to_sdf(&vec![0u8; 16 * 16], 16);
        assert!(sdf.iter().all(|&b| b == 0));
    }

    // ===== #217: image_rgba_to_sdf（generateImageSdf 移植）の経路別サニティ =====

    /// 透過画像（alpha 1% 以上）: alpha 経路で「不透明部分 = inside」になり SDF が出る。
    /// 透明背景に中央の不透明ブロックを置く → inside ピクセルが取れて Some。
    #[test]
    fn image_rgba_to_sdf_transparent_alpha_path() {
        let w = 64u32;
        let mut img = image::RgbaImage::from_pixel(w, w, image::Rgba([0, 0, 0, 0]));
        for y in 20..44 {
            for x in 20..44 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let sdf = image_rgba_to_sdf(&img, 256).expect("opaque block on transparent bg → Some");
        assert_eq!(sdf.len(), 256 * 256);
        assert!(
            sdf.iter().any(|&b| b > 128),
            "alpha path must yield inside (>128) samples"
        );
        assert!(
            sdf.iter().any(|&b| b < 128),
            "alpha path must yield outside (<128) samples"
        );
    }

    /// 不透明画像（輝度経路 + auto-polarity）: 白背景に小さい黒い被写体。
    /// 被写体は少数派なので auto-polarity で「暗い側 = inside」が選ばれる。
    #[test]
    fn image_rgba_to_sdf_opaque_luma_auto_polarity_dark_subject() {
        let w = 64u32;
        // 全面白 (不透明)。
        let mut img = image::RgbaImage::from_pixel(w, w, image::Rgba([255, 255, 255, 255]));
        // 中央に小さい黒い四角（少数派 = 被写体）。
        for y in 26..38 {
            for x in 26..38 {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        let sdf =
            image_rgba_to_sdf(&img, 256).expect("dark subject on white bg → Some (luma path)");
        // 中央（被写体）が inside 側になっているはず。
        assert!(
            sdf.iter().any(|&b| b > 128),
            "luma auto-polarity must produce inside region for the minority (dark) subject"
        );
        assert!(
            sdf.iter().any(|&b| b < 128),
            "must produce outside region too"
        );
    }

    /// 不透明・少数派が明るい被写体でも auto-polarity が拾う（黒背景に白い小四角）。
    #[test]
    fn image_rgba_to_sdf_opaque_luma_auto_polarity_light_subject() {
        let w = 64u32;
        let mut img = image::RgbaImage::from_pixel(w, w, image::Rgba([0, 0, 0, 255]));
        for y in 26..38 {
            for x in 26..38 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let sdf =
            image_rgba_to_sdf(&img, 256).expect("light subject on black bg → Some (luma path)");
        assert!(sdf.iter().any(|&b| b > 128));
        assert!(sdf.iter().any(|&b| b < 128));
    }

    /// コントラスト無し（単色不透明）→ None（#169）。
    #[test]
    fn image_rgba_to_sdf_no_contrast_returns_none() {
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([128, 128, 128, 255]));
        assert!(
            image_rgba_to_sdf(&img, 256).is_none(),
            "flat solid color has no contrast → None"
        );
    }

    /// 非正方形入力でもパニックせず contain で SDF が出る（#174 レタボ範囲限定）。
    #[test]
    fn image_rgba_to_sdf_non_square_input_ok() {
        // 横長 (alpha 経路)。透明背景に中央不透明ブロック。
        let mut img = image::RgbaImage::from_pixel(120, 40, image::Rgba([0, 0, 0, 0]));
        for y in 10..30 {
            for x in 40..80 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let sdf = image_rgba_to_sdf(&img, 128).expect("non-square with shape → Some");
        assert_eq!(sdf.len(), 128 * 128);
    }

    /// #217 / generateImageSdf 移植: 経路選択しきい値の助手。`size × size` の不透明
    /// 単色キャンバスに、同色のまま `transparent` (alpha=0) を `k` ピクセルだけ混ぜる。
    ///
    /// 色を 1 色に揃えてあるので **輝度は完全フラット**。よって輝度経路に入れば
    /// auto-polarity が全 inside/全 outside になり `None`（#169）。alpha 経路に入れば
    /// 不透明部分が inside で `Some`。`alphaPixelCount*100 >= drawnPixelCount`（1% 以上）
    /// で alpha 経路という Web の `hasMeaningfulAlpha` 境界を、結果が Some/None で割れる
    /// ように作ってある（drawn = `size*size` を入力 = `size` の正方形にすることで
    /// 描画矩形 = キャンバス全域、`drawn_pixel_count = size*size`）。
    fn alpha_threshold_probe(size: u32, transparent_px: usize) -> Option<Vec<u8>> {
        let s = size;
        let mut img = image::RgbaImage::from_pixel(s, s, image::Rgba([200, 200, 200, 255]));
        let mut set = 0usize;
        'outer: for y in 0..s {
            for x in 0..s {
                if set >= transparent_px {
                    break 'outer;
                }
                // 同色のまま alpha だけ 0 に（輝度フラットを維持して経路だけ切り替える）。
                img.put_pixel(x, y, image::Rgba([200, 200, 200, 0]));
                set += 1;
            }
        }
        image_rgba_to_sdf(&img, s)
    }

    /// #217 (#1): `alphaPixelCount*100 < drawnPixelCount`（1% に 1 ピクセル足りない）と
    /// 輝度経路が選ばれる。色をフラットに揃えてあるので輝度経路 = auto-polarity が
    /// コントラスト 0 を検知して `None` を返す。alpha 経路に倒れていれば不透明部分が
    /// inside で `Some` になるはずなので、`None` であることが「輝度経路が選ばれた」証拠。
    #[test]
    fn image_rgba_to_sdf_alpha_threshold_boundary_minus_one_uses_luma() {
        // 100x100 -> drawn=10000、1% = 100。99 個（=99*100 < 10000）で輝度経路。
        assert!(
            alpha_threshold_probe(100, 99).is_none(),
            "alphaCount*100 < drawnCount must take the luma path (flat luma → None), \
             not the alpha path (which would yield Some)"
        );
    }

    /// #217 (#2): `alphaPixelCount*100 == drawnPixelCount`（ちょうど 1%）で alpha 経路。
    /// Web の `hasMeaningfulAlpha = alphaPixelCount*100 >= drawnPixelCount` は `>=` なので
    /// 同点は alpha 側。色フラットなので alpha 経路でだけ inside（不透明部分）が立ち
    /// `Some`、輝度経路なら `None` になる。`Some` であることが「alpha 経路が選ばれた」証拠。
    #[test]
    fn image_rgba_to_sdf_alpha_threshold_boundary_exact_uses_alpha() {
        // 100x100 -> drawn=10000、ちょうど 100 個（100*100 == 10000）で alpha 経路。
        assert!(
            alpha_threshold_probe(100, 100).is_some(),
            "alphaCount*100 == drawnCount (exactly 1%) must take the alpha path (>= boundary) \
             and yield Some; the luma path would return None for this flat-luma image"
        );
    }

    /// #217 (#3): 描画矩形が**奇数ピクセル**で `dark_count == floor(drawn/2)` の同点を
    /// 作り、D1 修正後の Web 一致挙動を固定する。Web は `darkCount < drawnPixelCount/2`
    /// を**浮動小数除算**で評価する（`12 < 12.5` = true）ので inside=dark。整数 floor の
    /// `dark_count < drawn/2`（`12 < 12` = false）だと極性が反転し inside=light に割れる。
    /// よって少数派（dark）が被写体になる Web 挙動を assert する。
    #[test]
    fn image_rgba_to_sdf_inside_is_dark_odd_drawncount_web_parity() {
        // 5x5 -> drawn=25（奇数、identity resize）。12 黒 + 13 白で dark_count=12=floor(25/2)。
        let s = 5u32;
        let mut img = image::RgbaImage::from_pixel(s, s, image::Rgba([255, 255, 255, 255]));
        let mut set = 0usize;
        'outer: for y in 0..s {
            for x in 0..s {
                if set >= 12 {
                    break 'outer;
                }
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
                set += 1;
            }
        }
        let sdf = image_rgba_to_sdf(&img, s).expect("12 dark + 13 light → contrast → Some");
        // (0,0) は黒（少数派 dark）。Web 一致なら inside（>128）。
        let black = sdf[0];
        // (4,4) は白（多数派 light）。outside（<128）。
        let white = sdf[(4 * s + 4) as usize];
        assert!(
            black > 128,
            "Web parity (2*dark_count < drawn): the minority dark pixel must be inside (>128), \
             got {black}. Integer floor `dark_count < drawn/2` would flip this at the odd-count tie."
        );
        assert!(
            white < 128,
            "the majority light pixel must be outside (<128), got {white}"
        );
    }

    /// #217 (#4): inside-dark 極性のとき、`Y == avgY` ちょうどのピクセルは **outside**
    /// （Web の `yBuf[i] < avgY` は厳密不等号、`>= avgY` 側が outside）。`<` を `<=` に
    /// 取り違えると同点ピクセルが inside に倒れる回帰を捕まえる。
    #[test]
    fn image_rgba_to_sdf_avg_y_tie_pixel_is_outside() {
        // 4x4 grayscale（drawn=16、Y = グレー値）。3 黒(0) + 12 白(255) + 1 px=204。
        // avgY = (3*0 + 12*255 + 204)/16 = 204 ちょうど → (3,3) が Y==avgY の同点。
        // dark_count=3（黒のみ、204 も 255 も < 204 ではない）→ 2*3 < 16 で inside=dark。
        let s = 4u32;
        let mut img = image::RgbaImage::from_pixel(s, s, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(2, 0, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(3, 3, image::Rgba([204, 204, 204, 255]));
        let sdf = image_rgba_to_sdf(&img, s).expect("dark subject → contrast → Some");
        let tie = sdf[(3 * s + 3) as usize];
        let dark = sdf[0];
        assert!(
            tie < 128,
            "a pixel with Y == avgY must be OUTSIDE under inside-dark polarity (strict `< avgY`), \
             got {tie}; `<=` would wrongly count it as inside"
        );
        assert!(
            dark > 128,
            "the dark subject pixel must still be inside (>128), got {dark}"
        );
    }

    /// #217 (#5): 「ほぼ全 inside」の境界（`inside_count == drawn` ⇒ None /
    /// `== drawn-1` ⇒ Some、#169）。alpha 経路で作る: 描画矩形内に `alpha<255` を
    /// 1% 以上混ぜて alpha 経路に入れつつ、その不透明度を `>=128` に保てば全 inside。
    /// 1 ピクセルだけ `alpha<128`（背景）にすると inside が 1 つ減って Some。
    #[test]
    fn image_rgba_to_sdf_all_inside_one_px_diff_none_vs_some() {
        let s = 50u32; // drawn = 2500、1% = 25。
        let build = |one_bg: bool| -> Option<Vec<u8>> {
            let mut img = image::RgbaImage::from_pixel(s, s, image::Rgba([180, 180, 180, 255]));
            // 60 px (>1%) を alpha=200（[128,254]）にして alpha 経路へ。全部 inside のまま。
            let mut set = 0usize;
            'outer: for y in 0..s {
                for x in 0..s {
                    if set >= 60 {
                        break 'outer;
                    }
                    img.put_pixel(x, y, image::Rgba([180, 180, 180, 200]));
                    set += 1;
                }
            }
            if one_bg {
                // 1 px だけ alpha<128 → outside。inside_count = drawn-1。
                img.put_pixel(s - 1, s - 1, image::Rgba([180, 180, 180, 0]));
            }
            image_rgba_to_sdf(&img, s)
        };
        assert!(
            build(false).is_none(),
            "inside_count == drawn_pixel_count (全 inside) must be None (#169 no-contrast)"
        );
        assert!(
            build(true).is_some(),
            "one background pixel (inside_count == drawn-1) must be Some"
        );
    }

    /// #217 (#6): 極端アスペクト比でも、contain のレタボックス領域は必ず背景
    /// （`< 128`）で、シルエットは矩形ではなく被写体形状になる（#174 本質回帰）。
    #[test]
    fn image_rgba_to_sdf_letterbox_region_excluded_from_silhouette() {
        // 256x16 を 256 に contain → scale=1、dw=256/dh=16、dx=0/dy=120。
        // 描画矩形は行 120..136。黒背景 + 小さな白被写体。
        let size = 256u32;
        let mut img = image::RgbaImage::from_pixel(256, 16, image::Rgba([0, 0, 0, 255]));
        for y in 4..12 {
            for x in 100..140 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let sdf = image_rgba_to_sdf(&img, size).expect("white subject on black → Some");

        // レタボックス領域（描画矩形 120..136 の外）に inside は 1 つも無いこと。
        // 旧 #174 バグでは全域評価で alpha 経路に倒れ、描画矩形 = 純矩形 inside になり
        // レタボ部まで含めた矩形シルエットになっていた。
        let mut inside_in_letterbox = 0usize;
        let mut inside_in_rect = 0usize;
        for y in 0..size {
            for x in 0..size {
                if sdf[(y * size + x) as usize] > 128 {
                    if (120..136).contains(&y) {
                        inside_in_rect += 1;
                    } else {
                        inside_in_letterbox += 1;
                    }
                }
            }
        }
        let drawn_pixel_count = 256 * 16; // dw*dh
        assert_eq!(
            inside_in_letterbox, 0,
            "letterbox region must contain no inside pixels (#174); got {inside_in_letterbox}"
        );
        assert!(
            inside_in_rect > 0,
            "subject silhouette must light some pixels inside the drawn rect, got 0"
        );
        assert!(
            inside_in_rect < drawn_pixel_count / 2,
            "silhouette must follow the small subject, not fill the drawn rect \
             (a full-rectangle silhouette is the #174 bug): inside_in_rect={inside_in_rect} \
             of drawn={drawn_pixel_count}"
        );
    }
}
