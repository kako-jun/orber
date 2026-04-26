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
use image::RgbaImage;
use palette::{FromColor, Hsl, IntoColor, Srgb};
use tiny_skia::{
    Color, FillRule, GradientStop, Paint, PathBuilder, Pixmap, Point, RadialGradient, SpreadMode,
    Transform,
};

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
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
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
    // 初期化を「黒 不透明」にしたいので明示的に塗る。
    let mut pixmap =
        Pixmap::new(width, height).expect("pixmap allocation should succeed for >0 dimensions");
    pixmap.fill(Color::from_rgba8(0, 0, 0, 255));

    let base_radius_unit = (width.min(height) as f32) * 0.25 * orb_size;

    for cluster in clusters {
        // 半径 0 の orb は何も描画しないのでスキップ（0 半径の RadialGradient は tiny-skia で None になる）。
        let radius = base_radius_unit * cluster.weight.max(0.0).sqrt();
        if radius <= 0.0 {
            continue;
        }

        let cx = cluster.centroid.x.clamp(0.0, 1.0) * width as f32;
        let cy = cluster.centroid.y.clamp(0.0, 1.0) * height as f32;

        let [r, g, b] = adjust_saturation(cluster.color, saturation);
        let center_color = Color::from_rgba8(r, g, b, 255);
        // 中間 stop の半透明色は中心と同じ RGB で alpha だけ落とす。
        let mid_color = Color::from_rgba8(r, g, b, 128);
        let edge_color = Color::TRANSPARENT;

        // blur=0 で中間 stop が外寄り（中心の不透明領域が広い）、
        // blur=1 で中間 stop が中心寄り（中心の不透明領域が点に近い）。
        let mid_stop = (1.0 - blur * 0.8).clamp(0.05, 0.95);

        let stops = vec![
            GradientStop::new(0.0, center_color),
            GradientStop::new(mid_stop, mid_color),
            GradientStop::new(1.0, edge_color),
        ];

        let Some(shader) = RadialGradient::new(
            Point::from_xy(cx, cy),
            Point::from_xy(cx, cy),
            radius,
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            // 想定外（半径 0 等）。スキップ。
            continue;
        };

        let paint = Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };

        // 半径の 1.5 倍程度の円パスで塗る範囲を限定（fill_rect で全画面塗るより軽い）。
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

/// sRGB 0-255 を HSL に変換し、彩度を `factor` 倍してから sRGB に戻す。
fn adjust_saturation(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    if (factor - 1.0).abs() < f32::EPSILON {
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
}
