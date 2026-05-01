//! Glyph 形状の orb 描画モジュール。
//!
//! [`crate::orb::OrbShape::Glyph`] を選んだ orb は、円グラデではなく
//! 1 文字のフォントアウトラインを fill した形状になる。文字色は orb の色、
//! 不透明度は contrast 軸 + per-orb 揺らぎで決まる。
//!
//! # 設計メモ
//!
//! - フォントは [`include_bytes!`] でクレートに埋め込み、`'static` バイト列を
//!   そのまま [`ttf_parser::Face::parse`] に渡す。バイト列が静的なので
//!   `Face<'static>` は `Send + Sync`、`OnceLock` 経由でプロセス全体で 1 回だけ初期化する
//! - グリフごとの `bounding_box` / `outline` 計算をフレーム単位でやり直さないよう、
//!   呼び出し側 ([`render_glyph_orb`]) は 1 度のアウトライン抽出で `tiny_skia::Path`
//!   を作り、その後の塗りに使い回す前提
//! - グリフが見つからない場合 ([`Face::glyph_index`] が `None` を返す or `outline_glyph`
//!   が空アウトラインを返す) は **何も描画しない**。tofu は出さない。Phase A の方針として、
//!   絵文字など Symbols 2 に無い文字は静かに無視する
//! - フォントのアウトラインは Y 軸が上向き（font em スケール）。tiny-skia は Y 軸下向きなので、
//!   `OutlineBuilder` 内で y を反転して積み込む
//! - センタリングは `glyph_bounding_box` の中央を orb 中心に合わせ、半径 × 2 の正方領域に
//!   収まるよう em-square 基準でスケールする

use std::sync::OnceLock;
use tiny_skia::{
    Color, FillRule, Paint, Path, PathBuilder, Pixmap, Shader, Transform,
};
use ttf_parser::{Face, OutlineBuilder, Rect};

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
    face_for(font)
        .and_then(|f| f.glyph_index(ch))
        .is_some()
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

/// Phase B (#55): Glyph 1 文字のアウトラインを `size × size` の正方領域に
/// 中心揃えで fill し、alpha チャネルだけを `Vec<u8>` で返す。
///
/// 用途: WebGL2 fragment shader が `shape == "glyph"` のときに texture
/// sampling で orb の alpha を決めるためのプリベイク済みマスク。`R8` ないし
/// `RGBA(alpha のみ意味)` として GPU にアップロードする想定。
///
/// `size` は `1..=4096` の範囲を想定（呼び出し側で validate する）。
/// 同梱フォントに収録されていない文字を渡すと全 0 を返す（panic しない）。
/// 生成は決定的（同じ入力なら毎回同じバイト列）。キャッシュは呼び出し側で行う。
pub fn render_glyph_alpha_mask(font: GlyphFontId, ch: char, size: u32) -> Vec<u8> {
    let s = size.max(1);
    let mut pix = match Pixmap::new(s, s) {
        Some(p) => p,
        None => return vec![0u8; (s as usize) * (s as usize)],
    };
    let center = (s as f32 * 0.5, s as f32 * 0.5);
    // 余白を残しつつ正方領域いっぱいに描く: 半径は size の 0.45 倍。
    // build_glyph_path は半径 × 2 の正方領域に等比スケールするので、
    // radius = size * 0.45 にすれば文字の bbox 最大辺が 0.9 * size に揃う。
    let radius = (s as f32) * 0.45;
    let path = match build_glyph_path(font, ch, center, radius) {
        Some(p) => p,
        None => return vec![0u8; (s as usize) * (s as usize)],
    };
    let paint = Paint {
        shader: Shader::SolidColor(Color::from_rgba8(255, 255, 255, 255)),
        anti_alias: true,
        ..Default::default()
    };
    pix.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    // tiny-skia は premultiplied alpha だが、white(255) を fill しているので
    // alpha と RGB が一致する。alpha チャネルだけ抽出する。
    let raw = pix.data();
    let mut out = Vec::with_capacity((s as usize) * (s as usize));
    for px in raw.chunks_exact(4) {
        out.push(px[3]);
    }
    out
}

/// 単一の Glyph orb を pixmap に SourceOver で重ねる。
///
/// `radius` は orb の見た目半径相当（円 orb と揃える）。`opacity` ∈ [0, 1] は
/// 中心の不透明度（contrast / animate 軸で揺らされた最終値）。`blur` パラメータは
/// グリフでは使わない（グリフはアウトライン fill で表現するため）。
pub fn render_glyph_orb(
    pixmap: &mut Pixmap,
    center: (f32, f32),
    radius: f32,
    rgb: [u8; 3],
    opacity: f32,
    font: GlyphFontId,
    ch: char,
) {
    if radius <= 0.0 {
        return;
    }
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    let Some(path) = build_glyph_path(font, ch, center, radius) else {
        return;
    };

    let alpha_u8 = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
    let [r, g, b] = rgb;
    let paint = Paint {
        shader: Shader::SolidColor(Color::from_rgba8(r, g, b, alpha_u8)),
        anti_alias: true,
        ..Default::default()
    };

    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
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
            1.0,
            GlyphFontId::NotoSymbols2,
            '☆',
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
            1.0,
            GlyphFontId::NotoSymbols2,
            '☆',
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
            0.0,
            GlyphFontId::NotoSymbols2,
            '☆',
        );
        let lit = pix.data().chunks_exact(4).filter(|p| p[3] > 0).count();
        assert_eq!(lit, 0, "opacity=0 must not paint anything");
    }
}
