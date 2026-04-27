//! 色クラスタに対する HSL ベースのモジュレーション。
//!
//! variations preset は色相シフト・明度バイアス・彩度倍率・支配色ローテーションの
//! 4 軸でひとつの入力画像から多様な見え方を作り出す。本モジュールは [`Cluster`] の
//! 列に対してこれら 4 軸を適用するピュア関数を提供する。
//!
//! # 設計メモ
//!
//! - 色変換は [`palette::Hsl`] 経由。`Srgb<u8> → Srgb<f32> → Hsl` で操作してから戻す。
//! - 既存 `animate.rs::modulate_color` と同じく f32→u8 量子化はラウンドトリップで一度
//!   発生するが、視覚的な差異は無視できる範囲。
//! - `dominant_rotation` は weight 降順済みの cluster 列を右回転 N で並び替えるだけ。
//!   重心や色そのものは変えず、リサジュー軌道で割り当てられる位相だけが変わる。
//! - 何も変更しない場合（[`ColorMod::identity`]）は、入力をそのまま返す（コピーは発生する）。
//!
//! 使い方:
//!
//! ```
//! use orber::cluster::{Cluster, Centroid};
//! use orber::color_mod::{apply_color_mod, ColorMod};
//!
//! let clusters = vec![Cluster {
//!     color: [200, 80, 80],
//!     centroid: Centroid { x: 0.5, y: 0.5 },
//!     weight: 1.0,
//! }];
//! let m = ColorMod {
//!     hue_shift_deg: 30.0,
//!     lightness_bias: 0.1,
//!     saturation: 1.2,
//!     dominant_rotation: 0,
//! };
//! let out = apply_color_mod(clusters, &m);
//! assert_eq!(out.len(), 1);
//! ```

use crate::cluster::Cluster;
use palette::{FromColor, Hsl, IntoColor, Srgb};

/// HSL ベースのカラーモジュレーション設定。
///
/// - `hue_shift_deg`: -180..180 を想定（範囲外でも HSL 側が wrap するので落ちない）
/// - `lightness_bias`: -0.5..0.5 を想定。HSL の L に加算したあと 0..1 にクランプ
/// - `saturation`: 0.0..2.0 を想定。HSL の S に乗算（既存 `saturation` を吸収する想定）
/// - `dominant_rotation`: weight 降順ソート済み cluster 列に対する右回転オフセット
#[derive(Debug, Clone, Copy)]
pub struct ColorMod {
    pub hue_shift_deg: f32,
    pub lightness_bias: f32,
    pub saturation: f32,
    pub dominant_rotation: usize,
}

impl ColorMod {
    /// 何も変更しない identity（hue=0、light=0、sat=1、rot=0）。
    pub fn identity() -> Self {
        Self {
            hue_shift_deg: 0.0,
            lightness_bias: 0.0,
            saturation: 1.0,
            dominant_rotation: 0,
        }
    }
}

impl Default for ColorMod {
    fn default() -> Self {
        Self::identity()
    }
}

/// `clusters` に対して `m` で指定された HSL モジュレーションを適用する。
///
/// 戻り値は新しい `Vec`。元の順序（weight 降順）は `dominant_rotation` で右回転される。
pub fn apply_color_mod(clusters: Vec<Cluster>, m: &ColorMod) -> Vec<Cluster> {
    let n = clusters.len();
    if n == 0 {
        return clusters;
    }

    // 1. 各 cluster の色を HSL で変調。
    let mut modulated: Vec<Cluster> = clusters
        .into_iter()
        .map(|c| Cluster {
            color: shift_color(c.color, m),
            centroid: c.centroid,
            weight: c.weight,
        })
        .collect();

    // 2. dominant_rotation で右回転。N=0 や N % len == 0 は no-op。
    let rot = m.dominant_rotation % n;
    if rot != 0 {
        modulated.rotate_right(rot);
    }

    modulated
}

/// 1 色の sRGB(u8) に HSL モジュレーションを適用する。
fn shift_color(rgb: [u8; 3], m: &ColorMod) -> [u8; 3] {
    let srgb = Srgb::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    let mut hsl: Hsl = Hsl::from_color(srgb);

    // hue は度数加算（palette の Hsl::hue は RgbHue<f32>、`+= deg` で wrap）
    hsl.hue += m.hue_shift_deg;
    hsl.saturation = (hsl.saturation * m.saturation).clamp(0.0, 1.0);
    hsl.lightness = (hsl.lightness + m.lightness_bias).clamp(0.0, 1.0);

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
    use crate::cluster::Centroid;

    fn cluster(color: [u8; 3], cx: f32, cy: f32, w: f32) -> Cluster {
        Cluster {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight: w,
        }
    }

    #[test]
    fn identity_preserves_color_and_order() {
        let input = vec![
            cluster([200, 80, 80], 0.3, 0.4, 0.5),
            cluster([80, 200, 80], 0.7, 0.5, 0.3),
            cluster([80, 80, 200], 0.5, 0.7, 0.2),
        ];
        let out = apply_color_mod(input.clone(), &ColorMod::identity());
        assert_eq!(out.len(), 3);
        for (a, b) in input.iter().zip(out.iter()) {
            // identity でも HSL ラウンドトリップで ±1 程度の量子化誤差が出るので近似比較。
            for ch in 0..3 {
                let diff = (a.color[ch] as i32 - b.color[ch] as i32).abs();
                assert!(
                    diff <= 1,
                    "channel {ch}: {} vs {}",
                    a.color[ch],
                    b.color[ch]
                );
            }
            assert_eq!(a.centroid, b.centroid);
            assert_eq!(a.weight, b.weight);
        }
    }

    #[test]
    fn hue_shift_180_yields_complementary() {
        // 真っ赤 (255, 0, 0) を 180° 回すとシアン系（cyan）になる。
        let input = vec![cluster([255, 0, 0], 0.5, 0.5, 1.0)];
        let m = ColorMod {
            hue_shift_deg: 180.0,
            ..ColorMod::identity()
        };
        let out = apply_color_mod(input, &m);
        let c = out[0].color;
        // 赤チャンネルが大きく落ちて、緑+青が立ち上がる。
        assert!(c[0] < 30, "red should drop, got {}", c[0]);
        assert!(c[1] > 200, "green should rise, got {}", c[1]);
        assert!(c[2] > 200, "blue should rise, got {}", c[2]);
    }

    #[test]
    fn saturation_zero_yields_grayscale() {
        // sat=0 で R=G=B（グレースケール）になる。
        let input = vec![
            cluster([200, 80, 40], 0.5, 0.5, 1.0),
            cluster([60, 180, 200], 0.3, 0.3, 0.5),
        ];
        let m = ColorMod {
            saturation: 0.0,
            ..ColorMod::identity()
        };
        let out = apply_color_mod(input, &m);
        for c in out {
            // sat=0 後の HSL→RGB は R=G=B で出てくる（量子化で ±1 ずれることがある）。
            let r = c.color[0] as i32;
            let g = c.color[1] as i32;
            let b = c.color[2] as i32;
            assert!((r - g).abs() <= 1, "R-G: {r} vs {g}");
            assert!((g - b).abs() <= 1, "G-B: {g} vs {b}");
        }
    }

    #[test]
    fn lightness_bias_brightens_and_darkens() {
        let input = vec![cluster([128, 128, 128], 0.5, 0.5, 1.0)];

        let bright = apply_color_mod(
            input.clone(),
            &ColorMod {
                lightness_bias: 0.3,
                ..ColorMod::identity()
            },
        );
        assert!(
            bright[0].color[0] > 150,
            "bias +0.3 should brighten gray, got {}",
            bright[0].color[0]
        );

        let dark = apply_color_mod(
            input,
            &ColorMod {
                lightness_bias: -0.3,
                ..ColorMod::identity()
            },
        );
        assert!(
            dark[0].color[0] < 100,
            "bias -0.3 should darken gray, got {}",
            dark[0].color[0]
        );
    }

    #[test]
    fn dominant_rotation_rotates_order() {
        let input = vec![
            cluster([255, 0, 0], 0.0, 0.0, 0.5),
            cluster([0, 255, 0], 0.0, 0.0, 0.3),
            cluster([0, 0, 255], 0.0, 0.0, 0.2),
        ];
        let m = ColorMod {
            dominant_rotation: 1,
            ..ColorMod::identity()
        };
        let out = apply_color_mod(input, &m);
        // 右回転 1: [r, g, b] -> [b, r, g]
        assert!(out[0].color[2] > 200, "first should be blue-ish");
        assert!(out[1].color[0] > 200, "second should be red-ish");
        assert!(out[2].color[1] > 200, "third should be green-ish");
    }

    #[test]
    fn dominant_rotation_overflows_modulo() {
        // rotation=N (=len) は no-op。
        let input = vec![
            cluster([255, 0, 0], 0.0, 0.0, 0.5),
            cluster([0, 255, 0], 0.0, 0.0, 0.3),
            cluster([0, 0, 255], 0.0, 0.0, 0.2),
        ];
        let m = ColorMod {
            dominant_rotation: 3,
            ..ColorMod::identity()
        };
        let out = apply_color_mod(input.clone(), &m);
        for (a, b) in input.iter().zip(out.iter()) {
            assert_eq!(a.color, b.color);
        }
    }

    #[test]
    fn empty_input_returns_empty() {
        let out = apply_color_mod(Vec::<Cluster>::new(), &ColorMod::identity());
        assert!(out.is_empty());
    }
}
