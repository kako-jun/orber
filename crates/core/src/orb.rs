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

use crate::cluster::Cluster;
use crate::glyph::{render_glyph_orb, GlyphFontId};
use crate::style::{rim_mid_stop, soft_hold_stop, FalloffProfile, SoftnessPreset};
use aquarelle::{
    render_aquarelle_bleed_pass, render_aquarelle_orb, AquarelleBleedParams, AquarelleParams,
};
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
/// 質感セット（[`aquarelle`] crate）、`Glyph` は同梱フォント 1 文字のアウトライン
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
    Glyph {
        ch: char,
        font: GlyphFontId,
    },
}

impl PartialEq for OrbShape {
    // Aquarelle 内部のパラメータ (AquarelleParams) は比較対象から外す。
    // ここでの "等価" は「形が同じか」だけを判定する用途を想定している。
    // Glyph は文字とフォント識別子まで含めて比較する（軽い値なので）。
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (OrbShape::Circle, OrbShape::Circle) => true,
            (OrbShape::Aquarelle(_), OrbShape::Aquarelle(_)) => true,
            (OrbShape::Glyph { ch: a, font: fa }, OrbShape::Glyph { ch: b, font: fb }) => {
                a == b && fa == fb
            }
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
    /// ぼかし (Softness) preset（#55, #131 で改名）。Mid で既存挙動と完全同値。
    pub softness: SoftnessPreset,
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
            softness: SoftnessPreset::Mid,
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
    // softness offset を blur に積算してから clamp。Mid なら既存と完全同値。
    let blur = (opts.blur + opts.softness.blur_offset()).clamp(0.0, 1.0);
    let saturation = opts.saturation.max(0.0);
    let orb_size = opts.orb_size.max(0.0);
    // softness による中心 alpha 倍率。Circle / Glyph 経路で共通に使う。
    let alpha_mul = opts.softness.alpha_mul().clamp(0.0, 1.0);

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

        // 3 shape を対等な match で分岐させる。Circle が暗黙の default fall-through に
        // ならないよう、各アームで明示的にヘルパを呼ぶ（#195）。
        match opts.shape {
            OrbShape::Circle => {
                // Circle は per-orb 描画ヘルパへ委譲。render_static は全 orb を Rim・
                // softness 経由の opacity（Mid なら 1.0 で既存と完全同値）で固定。
                // 動的揺らぎが必要な経路は render_one_orb を直接呼ぶ。
                render_one_orb(
                    &mut pixmap,
                    (cx, cy),
                    radius,
                    [r, g, b],
                    blur,
                    alpha_mul,
                    OrbStyle::Rim,
                );
            }
            OrbShape::Aquarelle(params) => {
                // Aquarelle は別モジュールへ委譲。同じ Pixmap に SourceOver で書き込む。
                // i (cluster index) を seed の差分にして orb 同士で異なるオフセットを得る。
                render_aquarelle_orb(&mut pixmap, (cx, cy), radius, [r, g, b], i as u64, params);
            }
            OrbShape::Glyph { ch, font } => {
                // Glyph: 1 文字の SDF を Circle と同じ半径・blur・softness の意味で描く。
                // 静止画経路では回転だけ 0 固定にして、見た目の falloff は Circle と揃える。
                render_glyph_orb(
                    &mut pixmap,
                    (cx, cy),
                    radius,
                    [r, g, b],
                    blur,
                    alpha_mul,
                    FalloffProfile::Rim,
                    font,
                    ch,
                    0.0,
                );
            }
        }
    }

    // Glyph shape のときだけ、全 orb 描画後に aquarelle v0.2 の bleed pass を 1 回かける。
    // per-orb ではなく全体 1 回にすることで、グリフ群が水彩のにじみで馴染むようにする (#195)。
    // seed は決定論性のため固定 0。AquarelleBleedParams::default() = radius=3, intensity=0.5, halo=0.3。
    if let OrbShape::Glyph { .. } = opts.shape {
        render_aquarelle_bleed_pass(&mut pixmap, AquarelleBleedParams::default(), 0);
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
            let mid_stop = rim_mid_stop(blur);
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
            let hold_stop = soft_hold_stop(blur);
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

    /// 平均 alpha（厳密には平均 R）を計算するヘルパ。softness preset の比較で使う。
    fn mean_red(img: &RgbaImage) -> f64 {
        let mut s = 0u64;
        for px in img.pixels() {
            s += px[0] as u64;
        }
        s as f64 / (img.width() as f64 * img.height() as f64)
    }

    #[test]
    fn softness_high_is_softer_than_low_for_circle() {
        // Circle 経路で High は Low よりソフト（alpha 低め + blur 強め）になる。
        // 入力色は赤、背景は黒（R=0）。High ほど orb 中心の R が抑えられ、
        // 平均 R も小さくなる。
        let c = cluster([255, 0, 0], 0.5, 0.5, 1.0);
        let make = |softness: SoftnessPreset| {
            render_static(
                &[c],
                &RenderOptions {
                    width: 100,
                    height: 100,
                    blur: 0.5,
                    saturation: 1.0,
                    orb_size: 1.0,
                    softness,
                    ..Default::default()
                },
            )
        };
        let low = mean_red(&make(SoftnessPreset::Low));
        let mid = mean_red(&make(SoftnessPreset::Mid));
        let high = mean_red(&make(SoftnessPreset::High));
        assert!(
            low >= mid,
            "softness=Low mean R ({low}) must be >= Mid ({mid}) because Low is sharper"
        );
        assert!(
            high < mid,
            "softness=High mean R ({high}) must be < Mid ({mid}) because High is softer"
        );
        assert!(
            high < low,
            "softness=High ({high}) must be visibly softer / dimmer than Low ({low})"
        );
    }

    #[test]
    fn softness_mid_matches_default_render() {
        // softness=Mid を明示しても RenderOptions::default() と同じピクセルが出る（regression なし）。
        let c = cluster([200, 50, 50], 0.5, 0.5, 1.0);
        let opts_default = RenderOptions {
            width: 64,
            height: 64,
            ..Default::default()
        };
        let opts_mid = RenderOptions {
            width: 64,
            height: 64,
            softness: SoftnessPreset::Mid,
            ..Default::default()
        };
        let a = render_static(&[c], &opts_default);
        let b = render_static(&[c], &opts_mid);
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "softness=Mid must be byte-exact identical to default"
        );
    }

    #[test]
    fn glyph_render_differs_from_circle_for_same_cluster() {
        // 同じ cluster / opts を Circle と Glyph で別々に描画したとき、出力 RGBA が
        // pixel-level で異なる必要がある。これが一致してしまうと、Glyph 経路を通って
        // いても実際には Circle と同じ絵が出ているという退化に気付けない。
        use crate::glyph::GlyphFontId;
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let base = RenderOptions {
            width: 96,
            height: 96,
            ..Default::default()
        };
        let circle_opts = RenderOptions {
            shape: OrbShape::Circle,
            ..base.clone()
        };
        let glyph_opts = RenderOptions {
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..base
        };
        let img_circle = render_static(std::slice::from_ref(&c), &circle_opts);
        let img_glyph = render_static(std::slice::from_ref(&c), &glyph_opts);
        assert_ne!(
            img_circle.as_raw(),
            img_glyph.as_raw(),
            "Glyph rendering must produce a different pixmap than Circle for the same cluster"
        );
    }

    #[test]
    fn glyph_shape_renders_via_render_static() {
        // OrbShape::Glyph が render_static 経由でも一定数のピクセルを描く。
        use crate::glyph::GlyphFontId;
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = RenderOptions {
            width: 100,
            height: 100,
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..Default::default()
        };
        let img = render_static(&[c], &opts);
        // 背景は黒。グリフの白塗りピクセルが一定数立っているはず。
        let lit = img.pixels().filter(|p| p[0] > 32).count();
        assert!(
            lit > 32,
            "OrbShape::Glyph via render_static should paint visible pixels, lit={lit}"
        );
    }

    // ===== #195: 3 shape の決定論性と Glyph bleed pass の効果検証テスト群 =====

    #[test]
    fn circle_render_static_is_deterministic_after_refactor() {
        // Circle の render_static は同じ入力で 2 回呼ぶと完全に byte-equal を保つ。
        // #195 で 3 shape を対等な match 分岐に整理したが、Circle 経路は退化していない。
        let clusters = [
            cluster([200, 100, 50], 0.3, 0.4, 1.0),
            cluster([50, 200, 100], 0.7, 0.6, 0.8),
            cluster([100, 50, 200], 0.5, 0.5, 0.5),
        ];
        let opts = RenderOptions {
            width: 64,
            height: 64,
            shape: OrbShape::Circle,
            ..Default::default()
        };
        let a = render_static(&clusters, &opts);
        let b = render_static(&clusters, &opts);
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "Circle render_static must be byte-equal on repeated calls (determinism)"
        );
    }

    #[test]
    fn circle_multi_cluster_premul_invariant() {
        // un-premultiply 後の不変条件: alpha==0 のとき rgb は (0,0,0) に正規化される。
        // また、各チャネルは u8 として 0..=255 の範囲に収まる（型として保証されるが念のため）。
        let clusters = [
            cluster([200, 100, 50], 0.3, 0.4, 1.0),
            cluster([50, 200, 100], 0.7, 0.6, 0.8),
            cluster([100, 50, 200], 0.5, 0.5, 0.5),
        ];
        let opts = RenderOptions {
            width: 64,
            height: 64,
            shape: OrbShape::Circle,
            ..Default::default()
        };
        let img = render_static(&clusters, &opts);
        for px in img.pixels() {
            if px[3] == 0 {
                assert_eq!(
                    [px[0], px[1], px[2]],
                    [0, 0, 0],
                    "alpha=0 pixel must have RGB normalized to 0"
                );
            }
            // u8 の range は型で保証されるが、明示的にチェックして不変条件を残す。
            assert!(px[0] <= 255 && px[1] <= 255 && px[2] <= 255);
        }
    }

    #[test]
    fn aquarelle_render_static_is_deterministic() {
        // Aquarelle 経路の render_static は同じ入力で byte-equal を保つ。
        // 内部 seed は cluster index 由来で決定論的なので、2 回描画して一致するはず。
        let c = cluster([200, 100, 50], 0.5, 0.5, 1.0);
        let opts = RenderOptions {
            width: 64,
            height: 64,
            shape: OrbShape::Aquarelle(AquarelleParams::default()),
            ..Default::default()
        };
        let a = render_static(std::slice::from_ref(&c), &opts);
        let b = render_static(std::slice::from_ref(&c), &opts);
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "Aquarelle render_static must be byte-equal on repeated calls (determinism)"
        );
    }

    #[test]
    fn aquarelle_differs_from_circle_for_same_cluster() {
        // 同じ cluster / opts を Circle と Aquarelle で描画したとき、出力は別物になる。
        // 一致してしまうと Aquarelle 経路が事実上 Circle と同じ絵を出す退化に気付けない。
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let base = RenderOptions {
            width: 96,
            height: 96,
            ..Default::default()
        };
        let circle_opts = RenderOptions {
            shape: OrbShape::Circle,
            ..base.clone()
        };
        let aquarelle_opts = RenderOptions {
            shape: OrbShape::Aquarelle(AquarelleParams::default()),
            ..base
        };
        let img_circle = render_static(std::slice::from_ref(&c), &circle_opts);
        let img_aquarelle = render_static(std::slice::from_ref(&c), &aquarelle_opts);
        assert_ne!(
            img_circle.as_raw(),
            img_aquarelle.as_raw(),
            "Aquarelle rendering must produce a different pixmap than Circle for the same cluster"
        );
    }

    #[test]
    fn glyph_bleed_pass_is_deterministic() {
        // Glyph + bleed pass (seed=0 固定) は同じ入力で 2 回描画して byte-equal を保つ。
        // bleed pass が非決定論的な乱数を引き始めたらこのテストが破綻して警告となる。
        use crate::glyph::GlyphFontId;
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = RenderOptions {
            width: 64,
            height: 64,
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..Default::default()
        };
        let a = render_static(std::slice::from_ref(&c), &opts);
        let b = render_static(std::slice::from_ref(&c), &opts);
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "Glyph render_static (with bleed pass seed=0) must be byte-equal on repeated calls"
        );
    }

    #[test]
    fn glyph_lit_pixels_remain_visible_after_bleed() {
        // bleed pass を通してもグリフの白塗りピクセルが極端に薄まって消えていないこと。
        // 既存 glyph_shape_renders_via_render_static と同じ閾値 (lit > 32) で担保する。
        use crate::glyph::GlyphFontId;
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = RenderOptions {
            width: 100,
            height: 100,
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..Default::default()
        };
        let img = render_static(&[c], &opts);
        let lit = img.pixels().filter(|p| p[0] > 32).count();
        assert!(
            lit > 32,
            "Glyph lit pixels must survive the bleed pass (lit > 32), got lit={lit}"
        );
    }

    #[test]
    fn glyph_bleed_produces_halo_around_lit_pixel_cluster() {
        // bleed pass の直接の証拠: グリフ本体の境界から外側に halo が漏れる。
        // 3-pass box blur (radius=3) の到達距離は概ね 9px。
        //
        // アプローチ: 同じグリフを 2 通りで描画し halo 領域の brightness 合計を比較する。
        // - reference: 通常の Glyph render_static (= bleed pass あり)
        // - control:  Circle render_static (= bleed pass なし) を「同じ位置に絶対に lit pixel が
        //              ない領域」の参照背景として使い、その領域の R 合計 (= 0) と比較
        //
        // ……ではなく、より単純に「Glyph 出力の中で『中心から十分離れた / 黒背景の領域』に
        // R > 0 の pixel が一定数以上ある」を確認する。bleed が無ければ glyph SDF 範囲外の
        // pixel は完全な黒 (R=0) であるはず。
        //
        // 判断ポイント: 64x64 に default orb_size=1.0 で描画 → 半径 = 16px。グリフ中心は
        // (32, 32)。「中心から距離 >= 22px」の pixel (= 半径より 6px 外側) を halo 候補領域
        // とし、その中に R > 0 pixel が少なくとも 8 個あれば bleed pass が halo を出している。
        // bleed radius は box-blur*3 で実効 ~9px のため、中心から 22..=25px の範囲には
        // halo がしっかり届く。
        use crate::glyph::GlyphFontId;
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = RenderOptions {
            width: 64,
            height: 64,
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..Default::default()
        };
        let img = render_static(&[c], &opts);
        let cx = 32.0f32;
        let cy = 32.0f32;
        // 半径 18..=20 のリング: ☆ グリフ本体は r<=16 でほぼ完結し (max R ~30+)、
        // r=17 以降は max R が <= 2 まで急落する = 本体ピクセルは無くなる領域。
        // ここで lit pixel (R>0) が複数残っていれば、それは bleed pass が外側に
        // にじみを広げた halo の直接の証拠。
        // 経験的に r=18 で ~29px, r=19 で ~15px, r=20 で ~3px の halo が観測される。
        let mut halo_count = 0;
        let mut halo_max_r = 0u8;
        for y in 0..64u32 {
            for x in 0..64u32 {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                if (18.0..21.0).contains(&d) {
                    let px = img.get_pixel(x, y);
                    if px[0] > 0 {
                        halo_count += 1;
                        halo_max_r = halo_max_r.max(px[0]);
                    }
                }
            }
        }
        // halo の R 値は薄いがゼロではない (typical max_R ~= 1)。
        // pixel 数のみで判定する。
        assert!(
            halo_count >= 10,
            "bleed pass must leak halo (R>0) into the ring 18..=20px from glyph center; \
             found {halo_count} halo pixels (max R = {halo_max_r}). \
             expected ~30+ halo pixels."
        );
    }

    #[test]
    fn glyph_with_empty_clusters_stays_black_after_bleed() {
        // clusters=&[] で Glyph 経路に入っても bleed pass で偽 pixel が生まれず、
        // 全画面が背景の黒 (0,0,0,255) のままになる。
        use crate::glyph::GlyphFontId;
        let opts = RenderOptions {
            width: 32,
            height: 32,
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..Default::default()
        };
        let img = render_static(&[], &opts);
        for px in img.pixels() {
            assert_eq!(
                [px[0], px[1], px[2], px[3]],
                [0, 0, 0, 255],
                "empty clusters + Glyph (with bleed) must stay solid black"
            );
        }
    }

    #[test]
    fn glyph_zero_weight_cluster_stays_black_after_bleed() {
        // weight=0 の cluster は半径 0 でスキップされ、Glyph 描画自体が走らない。
        // 残った真っ黒 Pixmap に bleed pass をかけても黒のまま。
        use crate::glyph::GlyphFontId;
        let opts = RenderOptions {
            width: 32,
            height: 32,
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..Default::default()
        };
        let img = render_static(&[cluster([255, 255, 255], 0.5, 0.5, 0.0)], &opts);
        for px in img.pixels() {
            assert_eq!(
                [px[0], px[1], px[2], px[3]],
                [0, 0, 0, 255],
                "weight=0 + Glyph (with bleed) must stay solid black"
            );
        }
    }

    #[test]
    fn all_shapes_produce_valid_premultiplied_inverted_rgba() {
        // 3 shape どの経路でも、un-premultiply 後の出力は不変条件
        // 「alpha==0 なら rgb==(0,0,0)」を満たす。
        use crate::glyph::GlyphFontId;
        let c = cluster([200, 100, 50], 0.5, 0.5, 1.0);
        let shapes = [
            OrbShape::Circle,
            OrbShape::Aquarelle(AquarelleParams::default()),
            OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
        ];
        for shape in shapes {
            let opts = RenderOptions {
                width: 48,
                height: 48,
                shape,
                ..Default::default()
            };
            let img = render_static(std::slice::from_ref(&c), &opts);
            for px in img.pixels() {
                if px[3] == 0 {
                    assert_eq!(
                        [px[0], px[1], px[2]],
                        [0, 0, 0],
                        "alpha=0 pixel must have RGB=0 for shape={:?}",
                        shape
                    );
                }
            }
        }
    }

    #[test]
    fn all_shapes_produce_buffer_of_correct_size() {
        // 3 shape どの経路でも、出力バッファ長は width*height*4 と一致する。
        // RgbaImage::from_raw の expect で守られているが、明示的に契約として残す。
        use crate::glyph::GlyphFontId;
        let c = cluster([200, 100, 50], 0.5, 0.5, 1.0);
        let shapes = [
            OrbShape::Circle,
            OrbShape::Aquarelle(AquarelleParams::default()),
            OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
        ];
        for shape in shapes {
            let opts = RenderOptions {
                width: 40,
                height: 56,
                shape,
                ..Default::default()
            };
            let img = render_static(std::slice::from_ref(&c), &opts);
            assert_eq!(
                img.as_raw().len(),
                (opts.width * opts.height * 4) as usize,
                "buffer length must be width*height*4 for shape={:?}",
                shape
            );
        }
    }

    #[test]
    fn glyph_bleed_with_softness_high_still_deterministic() {
        // softness=High でも Glyph + bleed pass の決定論性が保たれる。
        // softness が blur/alpha に積算される経路でも乱数化されていないことを担保。
        use crate::glyph::GlyphFontId;
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = RenderOptions {
            width: 64,
            height: 64,
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            softness: SoftnessPreset::High,
            ..Default::default()
        };
        let a = render_static(std::slice::from_ref(&c), &opts);
        let b = render_static(std::slice::from_ref(&c), &opts);
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "Glyph + softness=High must remain byte-equal on repeated calls"
        );
    }
}
