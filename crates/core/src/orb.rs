//! orb（円形ぼかし）描画モジュール。
//!
//! [`crate::cluster::Cluster`] のリストを入力に、各クラスタを 1 個の orb（中心が
//! 不透明、外側に向かって透明に減衰する円）として 2D ラスター上に重ねていく。
//! 出力は [`image::RgbaImage`]。
//!
//! # 設計メモ
//!
//! - 描画バックエンドは [`tiny_skia`]。pure Rust で外部ライブラリ不要、
//!   `RadialGradient` をネイティブで持っているため orb の表現に向く
//! - tiny-skia の Pixmap は **premultiplied alpha**。出力時に un-premultiply して
//!   `RgbaImage` の straight alpha に揃える
//! - キャンバスは黒で初期化。複数 orb は Source-Over で重ねる（後に書いた orb が前面）
//! - blur は中心の不透明領域の広さで近似する。blur=0 → 中心が広く、急峻に縁が落ちる。
//!   blur=1 → 中心の不透明領域が点に近く、緩やかに減衰
//! - 彩度調整は palette の HSL 経由

use crate::aquarelle::{render_aquarelle_orb, AquarelleParams};
use crate::cluster::Cluster;
use crate::glyph::{render_glyph_orb, GlyphFontId};
use image::RgbaImage;
use palette::{FromColor, Hsl, IntoColor, Srgb};
use tiny_skia::{
    Color, FillRule, GradientStop, Paint, PathBuilder, Pixmap, Point, RadialGradient, SpreadMode,
    Transform,
};

/// 個別 orb の描画スタイル。1 フレーム内で混在させる前提。
///
/// `Rim` は中心明 → 中間で少し落として外周フェードのリング感、`Soft` は中心明 →
/// 外周フェードの単純グラデーション。`render_one_orb` 経由で per-orb に切替できる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrbStyle {
    /// リム強調（中間 stop で alpha を一段落として輪郭感を出す）。
    #[default]
    Rim,
    /// 単純ソフト（中間 stop なし、中心 → 透明への単調減衰）。
    Soft,
}

/// orb 描画形式。`Circle` は単一の radial gradient、`Aquarelle` はセル画夜景の
/// 質感セット（[`crate::aquarelle`]）、`Glyph` は同梱フォント 1 文字のアウトライン
/// 塗りを有効にする。
///
/// `Glyph` のフォントは [`GlyphFontId`] enum で識別する設計のため、`OrbShape` は
/// 引き続き `Copy + Send + Sync`。実体の `Face` パースはモジュール側の
/// `OnceLock` キャッシュに任せ、`OrbShape` 自体に重い state を持たせない。
#[derive(Debug, Clone, Copy, Default)]
pub enum OrbShape {
    #[default]
    Circle,
    Aquarelle(AquarelleParams),
    /// 1 文字のグリフを orb として描く。`ch` は描画する文字、`font` は同梱フォント識別子。
    Glyph { ch: char, font: GlyphFontId },
}

impl PartialEq for OrbShape {
    // Aquarelle 内部のパラメータ (AquarelleParams) は比較対象から外す。
    // ここでの "等価" は「形が同じか」だけを判定する用途を想定している。
    // Glyph は文字とフォント識別子まで含めて比較する（軽い値なので）。
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (OrbShape::Circle, OrbShape::Circle) => true,
            (OrbShape::Aquarelle(_), OrbShape::Aquarelle(_)) => true,
            (
                OrbShape::Glyph { ch: a, font: fa },
                OrbShape::Glyph { ch: b, font: fb },
            ) => a == b && fa == fb,
            _ => false,
        }
    }
}

/// 静的 orb 描画のオプション。
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// 出力幅（ピクセル）
    pub width: u32,
    /// 出力高さ（ピクセル）
    pub height: u32,
    /// orb サイズ倍率（1.0 = デフォルト）
    pub orb_size: f32,
    /// ぼかし強度 0.0..=1.0（0=シャープ、1=完全ぼかし）
    pub blur: f32,
    /// 彩度倍率（1.0 = unchanged）
    pub saturation: f32,
    /// 背景 RGBA。alpha=0 で透過。デフォルトは黒不透明。
    pub background: [u8; 4],
    /// orb の描画形式。Circle なら現状互換、Aquarelle ならセル画夜景の質感セット。
    pub shape: OrbShape,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            background: [0, 0, 0, 255],
            shape: OrbShape::Circle,
        }
    }
}

/// クラスタを orb として描画した RGBA 画像を返す。
///
/// 結果は `opts.width` × `opts.height` の `RgbaImage`。背景は黒（不透明）。
/// 各 cluster は中心 = `centroid * (width, height)`、半径 = `min(width, height) * 0.25
/// * orb_size * sqrt(weight)` の orb として描かれる。
pub fn render_static(clusters: &[Cluster], opts: &RenderOptions) -> RgbaImage {
    // 不正・極端な値を握りつぶさず、最低限の防衛だけ行う。
    let width = opts.width.max(1);
    let height = opts.height.max(1);
    let blur = opts.blur.clamp(0.0, 1.0);
    let saturation = opts.saturation.max(0.0);
    let orb_size = opts.orb_size.max(0.0);

    // Pixmap::new は uninit を 0 埋めしてくれる（つまり全画面が透明）。
    // 透過 (alpha=0) 指定なら fill をスキップしてその透明初期値を活かす。
    // 不透明な背景指定なら明示的に塗る。tiny-skia は premultiplied alpha だが、
    // Color::from_rgba8 は straight 色を内部で premul に直して塗る。
    let mut pixmap =
        Pixmap::new(width, height).expect("pixmap allocation should succeed for >0 dimensions");
    let [br, bg, bb, ba] = opts.background;
    if ba > 0 {
        pixmap.fill(Color::from_rgba8(br, bg, bb, ba));
    }

    let base_radius_unit = (width.min(height) as f32) * 0.25 * orb_size;

    for (i, cluster) in clusters.iter().enumerate() {
        // 半径 0 の orb は何も描画しないのでスキップ（0 半径の RadialGradient は tiny-skia で None になる）。
        let radius = base_radius_unit * cluster.weight.max(0.0).sqrt();
        if radius <= 0.0 {
            continue;
        }

        let cx = cluster.centroid.x.clamp(0.0, 1.0) * width as f32;
        let cy = cluster.centroid.y.clamp(0.0, 1.0) * height as f32;

        let [r, g, b] = adjust_saturation(cluster.color, saturation);

        // Aquarelle は別モジュールへ委譲。同じ Pixmap に SourceOver で書き込む。
        if let OrbShape::Aquarelle(params) = opts.shape {
            // i (cluster index) を seed の差分にして orb 同士で異なるオフセットを得る。
            render_aquarelle_orb(&mut pixmap, (cx, cy), radius, [r, g, b], i as u64, params);
            continue;
        }

        // Glyph: 1 文字のアウトラインを fill。半径は Circle と同じ意味で渡す。
        // render_static の opacity=1.0 を踏襲。
        if let OrbShape::Glyph { ch, font } = opts.shape {
            render_glyph_orb(&mut pixmap, (cx, cy), radius, [r, g, b], 1.0, font, ch);
            continue;
        }

        // Circle は per-orb 描画ヘルパへ委譲。render_static は全 orb を Rim・opacity=1.0
        // で固定（既存挙動の互換）。動的揺らぎが必要な経路は render_one_orb を直接呼ぶ。
        render_one_orb(
            &mut pixmap,
            (cx, cy),
            radius,
            [r, g, b],
            blur,
            1.0,
            OrbStyle::Rim,
        );
    }

    // tiny-skia の Pixmap は premultiplied alpha なので un-premultiply して straight に戻す。
    // 各 orb は alpha=255 の中心を持ち、背景は alpha=255 の黒。orb 同士の重なりも
    // SourceOver で最終的に alpha=255 になるはずだが、念のため一般化された un-premultiply を実装する。
    let mut buf = pixmap.take(); // Vec<u8>, RGBA premultiplied
    for px in buf.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            // 完全透明は RGB を意味しないが、出力では 0 にしておく。
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            // straight = round(premul * 255 / a)
            let inv = 255.0 / a as f32;
            px[0] = (px[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            px[1] = (px[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            px[2] = (px[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
        }
    }

    RgbaImage::from_raw(width, height, buf)
        .expect("raw buffer length matches width * height * 4 by construction")
}

/// 単一 orb（Circle 系）を既存の Pixmap に SourceOver で重ねる。
///
/// `blur` / `opacity` / `style` は per-orb で受ける。`render_static` から「全 orb 共通の
/// 静的設定」で呼ぶこともできるし、[`crate::animate::render_frame`] から「フレーム毎の
/// per-orb 揺らぎ」で呼ぶこともできる共通エントリ。
///
/// - `center` は描画位置（ピクセル座標、左上原点）。
/// - `radius` は不透明領域から外周フェード端までの全長。`<= 0.0` なら何もしない。
/// - `rgb` は sRGB 0-255。彩度補正は呼び出し側で済ませておくこと。
/// - `blur` ∈ [0, 1]。0 でシャープ、1 で完全ソフト。`Rim` では中間 stop の位置、
///   `Soft` では外周フェード曲線に効く。
/// - `opacity` ∈ [0, 1]。中心の alpha 倍率（外周は常に 0 にフェード）。
/// - `style` は `Rim` / `Soft` の 2 種類。
///
/// 描画失敗（半径 0 や tiny-skia 内部失敗）はサイレントに無視する。
pub fn render_one_orb(
    pixmap: &mut Pixmap,
    center: (f32, f32),
    radius: f32,
    rgb: [u8; 3],
    blur: f32,
    opacity: f32,
    style: OrbStyle,
) {
    if radius <= 0.0 {
        return;
    }
    let blur = blur.clamp(0.0, 1.0);
    let opacity = opacity.clamp(0.0, 1.0);
    let (cx, cy) = center;
    let [r, g, b] = rgb;

    // 中心 alpha は opacity に比例。255 を超えないよう u8 で握る。
    let center_a = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
    if center_a == 0 {
        return;
    }
    let center_color = Color::from_rgba8(r, g, b, center_a);
    let edge_color = Color::TRANSPARENT;

    let stops = match style {
        OrbStyle::Rim => {
            // 中間 stop の alpha を下げ、縁の境界を柔らかくする (#78)。
            // 旧値 128/255 ≒ 0.50 だと「中央 → 中間」と「中間 → 透明」の差が
            // 等しく、中間 stop の位置に視覚的なエッジが立つ。80/255 ≒ 0.31
            // に下げると中央→中間で alpha が 0.69 落ち、中間→透明で 0.31
            // 落ちる非対称になり、縁が滑らかにフェードして文字オーバーレイ
            // 時の可読性が上がる。
            // blur=0 で中間 stop が外寄り（不透明領域広い）、blur=1 で中心寄り（点に近い）。
            let mid_a = ((opacity * 80.0).round().clamp(0.0, 255.0)) as u8;
            let mid_color = Color::from_rgba8(r, g, b, mid_a);
            let mid_stop = (1.0 - blur * 0.8).clamp(0.05, 0.95);
            vec![
                GradientStop::new(0.0, center_color),
                GradientStop::new(mid_stop, mid_color),
                GradientStop::new(1.0, edge_color),
            ]
        }
        OrbStyle::Soft => {
            // 単純な中心 → 透明グラデーション。中間 stop なし。
            // blur=0 では中心 alpha を保つ範囲が広くなるよう、フェード開始位置を
            // 外側に寄せた中間 stop（同じ alpha）を 1 つ挟む。
            let hold_stop = (1.0 - blur).clamp(0.05, 0.95);
            vec![
                GradientStop::new(0.0, center_color),
                GradientStop::new(hold_stop, center_color),
                GradientStop::new(1.0, edge_color),
            ]
        }
    };

    let Some(shader) = RadialGradient::new(
        Point::from_xy(cx, cy),
        Point::from_xy(cx, cy),
        radius,
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };

    let paint = Paint {
        shader,
        anti_alias: true,
        ..Default::default()
    };

    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, radius * 1.5);
    if let Some(path) = pb.finish() {
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// sRGB 0-255 を HSL に変換し、彩度を `factor` 倍してから sRGB に戻す。
///
/// 彩度調整は HSL 経路で行う。cluster 抽出は LAB（知覚距離）を使うが、
/// 彩度のフラグは「CSS 的な見た目の彩度」に合わせるほうが UI 直感に近いため、
/// 意図的に色空間を分けている。
pub fn adjust_saturation_pub(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    adjust_saturation(rgb, factor)
}

pub(crate) fn adjust_saturation(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    // 1.0001 等の浮動小数点誤差レベルの入力でも fast path に入るよう、緩めの 1e-4
    // 閾値を使う（f32::EPSILON ≈ 1.19e-7 だと CLI 入力では実用上ほぼ通らない）。
    if (factor - 1.0).abs() < 1e-4 {
        return rgb;
    }
    let srgb = Srgb::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    let mut hsl: Hsl = Hsl::from_color(srgb);
    hsl.saturation = (hsl.saturation * factor).clamp(0.0, 1.0);
    let out: Srgb = hsl.into_color();
    [
        (out.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{Centroid, Cluster};

    fn cluster(color: [u8; 3], cx: f32, cy: f32, weight: f32) -> Cluster {
        Cluster {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight,
        }
    }

    #[test]
    fn single_cluster_renders_to_correct_dimensions() {
        let opts = RenderOptions {
            width: 100,
            height: 100,
            ..Default::default()
        };
        let img = render_static(&[cluster([200, 100, 50], 0.5, 0.5, 1.0)], &opts);
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 100);
    }

    #[test]
    fn single_cluster_centered_color() {
        let opts = RenderOptions {
            width: 100,
            height: 100,
            blur: 0.5,
            saturation: 1.0,
            orb_size: 1.0,
            ..Default::default()
        };
        let img = render_static(&[cluster([255, 0, 0], 0.5, 0.5, 1.0)], &opts);
        let center = img.get_pixel(50, 50);
        assert!(
            center[0] > 200,
            "center red channel should be high, got {}",
            center[0]
        );
        assert!(
            center[1] < 50,
            "center green channel should be low, got {}",
            center[1]
        );
        assert!(
            center[2] < 50,
            "center blue channel should be low, got {}",
            center[2]
        );
    }

    #[test]
    fn empty_clusters_returns_black_image() {
        // 空 clusters の場合、背景の黒 (0, 0, 0, 255) で全画面が埋まる仕様。
        let opts = RenderOptions {
            width: 32,
            height: 32,
            ..Default::default()
        };
        let img = render_static(&[], &opts);
        assert_eq!(img.width(), 32);
        assert_eq!(img.height(), 32);
        for px in img.pixels() {
            assert_eq!(px[0], 0, "R should be 0");
            assert_eq!(px[1], 0, "G should be 0");
            assert_eq!(px[2], 0, "B should be 0");
            assert_eq!(px[3], 255, "A should be 255 (opaque black)");
        }
    }

    #[test]
    fn respects_dimensions() {
        let opts = RenderOptions {
            width: 200,
            height: 300,
            ..Default::default()
        };
        let img = render_static(&[cluster([10, 20, 30], 0.5, 0.5, 1.0)], &opts);
        assert_eq!(img.width(), 200);
        assert_eq!(img.height(), 300);
    }

    #[test]
    fn zero_weight_cluster_skipped_yields_black() {
        // weight=0 の cluster は半径 0 になりスキップされ、結果は背景黒のままになる。
        let opts = RenderOptions {
            width: 16,
            height: 16,
            ..Default::default()
        };
        let img = render_static(&[cluster([255, 255, 255], 0.5, 0.5, 0.0)], &opts);
        for px in img.pixels() {
            assert_eq!(px[0], 0);
            assert_eq!(px[1], 0);
            assert_eq!(px[2], 0);
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn saturation_zero_produces_grayscale_center() {
        // saturation=0.0 で中心ピクセルは R==G==B（グレースケール）になるはず。
        let opts = RenderOptions {
            width: 100,
            height: 100,
            blur: 0.5,
            saturation: 0.0,
            orb_size: 1.0,
            ..Default::default()
        };
        let img = render_static(&[cluster([220, 30, 40], 0.5, 0.5, 1.0)], &opts);
        let center = img.get_pixel(50, 50);
        let r = center[0] as i32;
        let g = center[1] as i32;
        let b = center[2] as i32;
        assert!(
            (r - g).abs() <= 2 && (g - b).abs() <= 2 && (r - b).abs() <= 2,
            "saturation=0 should produce grayscale, got R={} G={} B={}",
            r,
            g,
            b
        );
    }

    #[test]
    fn blur_one_softens_edge_more_than_blur_zero() {
        // 同じ cluster で blur=0.0 と blur=1.0 を render。
        // 中心からある程度離れた位置（半径の約 50%）で、blur=1.0 の方が
        // より中間的（中心色から遠い）な値になっていることを確認する。
        // blur=0 → 中心の不透明領域が広い → 中心と同じ赤に近い
        // blur=1 → 中心の不透明領域が点 → サンプル位置はもっと暗い／黒寄り
        let base = RenderOptions {
            width: 100,
            height: 100,
            saturation: 1.0,
            orb_size: 1.0,
            blur: 0.0,
            ..Default::default()
        };
        let opts_sharp = RenderOptions {
            blur: 0.0,
            ..base.clone()
        };
        let opts_blurred = RenderOptions {
            blur: 1.0,
            ..base.clone()
        };
        let c = cluster([255, 0, 0], 0.5, 0.5, 1.0);
        let img_sharp = render_static(std::slice::from_ref(&c), &opts_sharp);
        let img_blurred = render_static(&[c], &opts_blurred);

        // orb 半径 = min(w,h)*0.25*orb_size*sqrt(weight) = 100*0.25 = 25
        // 半径の 50% = 12.5px。中心 (50,50) から x 方向に 13px ずらした位置をサンプル。
        let sx = 63u32;
        let sy = 50u32;
        let p_sharp = img_sharp.get_pixel(sx, sy);
        let p_blurred = img_blurred.get_pixel(sx, sy);

        // blur=0 ではこの位置はまだ不透明領域内に近く、赤が強く残る。
        // blur=1 では中心の不透明領域がほぼ点なので、この位置の赤は急峻に減衰している。
        assert!(
            p_sharp[0] > p_blurred[0],
            "blur=0 should keep red stronger at edge sample than blur=1, sharp R={} blurred R={}",
            p_sharp[0],
            p_blurred[0]
        );
    }

    #[test]
    fn renders_at_default_resolution() {
        // RenderOptions::default() のサイズ（1080x1920）で実走できることを確認する。
        let opts = RenderOptions::default();
        let img = render_static(&[cluster([100, 150, 200], 0.5, 0.5, 1.0)], &opts);
        assert_eq!(img.width(), 1080);
        assert_eq!(img.height(), 1920);
    }
}
