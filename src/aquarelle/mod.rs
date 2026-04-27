//! セル画夜景の質感セット（aquarelle）。
//!
//! 円形ぼかしの単純な orb を、80-90 年代アニメのセル画夜景風に踏み込ませるための
//! 4 要素（bleed / bloom / offset / halo）を集約する。将来 `aquarelle` を独立 crate
//! に切り出すための土台で、orber 本体への依存を持たない型と関数だけを公開する。
//!
//! # 4 要素
//!
//! 1. **bleed** — 円形ぼかしに歪みを足したフィルム感。同色の小さなオフセット
//!    gradient を周辺に重ねて滲み感を作る
//! 2. **bloom** — 中心が完全飽和（白に近い）になり光源っぽく見える。半径の内側
//!    `bloom * R_inner` までを白寄りでクリップ
//! 3. **offset** — gradient 中心が円の幾何中心から少しズレる。完全な同心円より
//!    片寄った光のほうが光源として自然。seed で決定的にオフセット方向を選ぶ
//! 4. **halo** — 周辺へ滲むときに彩度を **上げて** 滲ませる（フィルムのハレーション風）
//!
//! # 設計メモ
//!
//! - tiny-skia の `Pixmap` を借りて描く。Pixmap は orber 本体でも使われているが、
//!   aquarelle はそれを「ただのピクセルバッファの抽象」として扱い、`Cluster` 等
//!   orber 固有の型は持ち込まない（独立 crate 化に備える）
//! - 入力は `center: (f32, f32)`, `radius: f32`, `color: [u8; 3]`, `seed: u64` の
//!   primitive のみ
//! - HSL 経路で彩度ブーストする際は palette を使う（orber 本体と同じ依存。crate
//!   分割時にも palette は持っていける）

use palette::{FromColor, Hsl, IntoColor, Srgb};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::f32::consts::TAU;
use tiny_skia::{
    Color, FillRule, GradientStop, Paint, PathBuilder, Pixmap, Point, RadialGradient, SpreadMode,
    Transform,
};

/// aquarelle 4 要素の強度。各 0.0..=1.0。
#[derive(Debug, Clone, Copy)]
pub struct AquarelleParams {
    /// 周辺の小さなオフセット gradient による滲み。0=なし、1=最大。
    pub bleed: f32,
    /// 中心の白飛びコア。0=単色、1=半径内側 30% を白寄りに置換。
    pub bloom: f32,
    /// gradient 中心の同心ズレ。0=同心、1=半径の 25% までズラす。
    pub offset: f32,
    /// 周辺彩度ブースト。0=なし、1=元色の彩度を 1.6 倍まで上げる。
    pub halo: f32,
}

impl Default for AquarelleParams {
    /// 中庸の見栄え（全要素 0.5 で穏やかなセル画感）。
    fn default() -> Self {
        Self {
            bleed: 0.5,
            bloom: 0.5,
            offset: 0.5,
            halo: 0.5,
        }
    }
}

impl AquarelleParams {
    /// 各値を [0, 1] に丸めた、安全に処理できる版を返す。
    fn clamped(self) -> Self {
        Self {
            bleed: self.bleed.clamp(0.0, 1.0),
            bloom: self.bloom.clamp(0.0, 1.0),
            offset: self.offset.clamp(0.0, 1.0),
            halo: self.halo.clamp(0.0, 1.0),
        }
    }
}

/// 1 つの aquarelle orb を `pixmap` に描き込む。
///
/// `seed` は決定的なオフセット方向 / bleed 配置に使われる。同じ `seed` ・
/// `center` ・ `radius` ・ `color` ・ `params` で同じ結果が返る。
///
/// 描画は SourceOver で重ねる。背景の塗り潰しは呼び出し側の責任。
pub fn render_aquarelle_orb(
    pixmap: &mut Pixmap,
    center: (f32, f32),
    radius: f32,
    color: [u8; 3],
    seed: u64,
    params: AquarelleParams,
) {
    if radius <= 0.0 {
        return;
    }
    let p = params.clamped();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);

    // 1. 中心オフセット: 半径の最大 25% まで、決定論的にズラす。
    let offset_dist = radius * 0.25 * p.offset;
    let theta: f32 = rng.gen_range(0.0..TAU);
    let cx = center.0 + offset_dist * theta.cos();
    let cy = center.1 + offset_dist * theta.sin();

    // 2. メインの radial gradient。halo パラメータで周辺彩度を上げる。
    let halo_color = boost_saturation(color, 1.0 + 0.6 * p.halo);
    draw_radial(
        pixmap,
        cx,
        cy,
        radius,
        color_with_alpha(color, 255),
        color_with_alpha(halo_color, 128),
        color_with_alpha(halo_color, 0),
        0.55,
    );

    // 3. bleed: 周辺に小さな同色 gradient を 0..3 個ばら撒く。
    let bleed_count = (3.0 * p.bleed).round() as u32;
    for _ in 0..bleed_count {
        let bleed_theta: f32 = rng.gen_range(0.0..TAU);
        let bleed_dist = radius * rng.gen_range(0.4..0.9);
        let bx = center.0 + bleed_dist * bleed_theta.cos();
        let by = center.1 + bleed_dist * bleed_theta.sin();
        let bleed_radius = radius * rng.gen_range(0.2..0.4) * (0.5 + 0.5 * p.bleed);
        let bleed_color = boost_saturation(color, 1.0 + 0.4 * p.halo);
        draw_radial(
            pixmap,
            bx,
            by,
            bleed_radius,
            color_with_alpha(bleed_color, 100),
            color_with_alpha(bleed_color, 50),
            color_with_alpha(bleed_color, 0),
            0.5,
        );
    }

    // 4. bloom: 中心の白飛びコア。半径内側 0..30% を白に近づける。
    if p.bloom > 0.0 {
        let core_radius = radius * 0.3 * p.bloom;
        if core_radius > 0.0 {
            let mix_amount = 0.7;
            let bloom_color = mix_with_white(color, mix_amount);
            draw_radial(
                pixmap,
                cx,
                cy,
                core_radius,
                color_with_alpha(bloom_color, 255),
                color_with_alpha(bloom_color, 128),
                color_with_alpha(bloom_color, 0),
                0.55,
            );
        }
    }
}

#[inline]
fn color_with_alpha(rgb: [u8; 3], a: u8) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], a]
}

fn draw_radial(
    pixmap: &mut Pixmap,
    cx: f32,
    cy: f32,
    radius: f32,
    inner_rgba: [u8; 4],
    mid_rgba: [u8; 4],
    edge_rgba: [u8; 4],
    mid_stop: f32,
) {
    let center_color =
        Color::from_rgba8(inner_rgba[0], inner_rgba[1], inner_rgba[2], inner_rgba[3]);
    let mid_color = Color::from_rgba8(mid_rgba[0], mid_rgba[1], mid_rgba[2], mid_rgba[3]);
    let edge_color = Color::from_rgba8(edge_rgba[0], edge_rgba[1], edge_rgba[2], edge_rgba[3]);
    let stops = vec![
        GradientStop::new(0.0, center_color),
        GradientStop::new(mid_stop.clamp(0.05, 0.95), mid_color),
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

fn boost_saturation(rgb: [u8; 3], factor: f32) -> [u8; 3] {
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

fn mix_with_white(rgb: [u8; 3], amount: f32) -> [u8; 3] {
    let a = amount.clamp(0.0, 1.0);
    [
        (rgb[0] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
        (rgb[1] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
        (rgb[2] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    fn fresh_pixmap(w: u32, h: u32) -> Pixmap {
        let mut p = Pixmap::new(w, h).expect("pixmap");
        p.fill(Color::from_rgba8(0, 0, 0, 255));
        p
    }

    fn count_non_black(pix: &Pixmap) -> u64 {
        pix.data()
            .chunks_exact(4)
            .filter(|px| px[0] > 0 || px[1] > 0 || px[2] > 0)
            .count() as u64
    }

    #[test]
    fn aquarelle_renders_visible_orb() {
        let mut pix = fresh_pixmap(64, 64);
        render_aquarelle_orb(
            &mut pix,
            (32.0, 32.0),
            16.0,
            [200, 100, 50],
            42,
            AquarelleParams::default(),
        );
        assert!(
            count_non_black(&pix) > 0,
            "aquarelle orb should produce visible pixels"
        );
    }

    #[test]
    fn aquarelle_zero_radius_is_noop() {
        let mut pix = fresh_pixmap(32, 32);
        render_aquarelle_orb(
            &mut pix,
            (16.0, 16.0),
            0.0,
            [200, 100, 50],
            1,
            AquarelleParams::default(),
        );
        assert_eq!(count_non_black(&pix), 0);
    }

    #[test]
    fn bloom_brightens_center() {
        // bloom=1.0 にすると中心ピクセルが元色 (200, 100, 50) より白寄り(全成分大)になる。
        let mut a = fresh_pixmap(64, 64);
        let mut b = fresh_pixmap(64, 64);
        let zero_bloom = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        };
        let full_bloom = AquarelleParams {
            bleed: 0.0,
            bloom: 1.0,
            offset: 0.0,
            halo: 0.0,
        };
        render_aquarelle_orb(&mut a, (32.0, 32.0), 24.0, [200, 100, 50], 1, zero_bloom);
        render_aquarelle_orb(&mut b, (32.0, 32.0), 24.0, [200, 100, 50], 1, full_bloom);
        let pa = a.pixel(32, 32).expect("center pixel exists");
        let pb = b.pixel(32, 32).expect("center pixel exists");
        // bloom=1 では中心の青成分が bloom=0 より高い（白に近づく）。
        assert!(
            pb.blue() > pa.blue(),
            "bloom should raise blue at center: zero={} full={}",
            pa.blue(),
            pb.blue()
        );
    }

    #[test]
    fn params_individually_change_output() {
        // 各要素を個別に上げると、bloom=0 のベースから差分が出ることを確認する。
        let base = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        };
        let mut p_base = fresh_pixmap(64, 64);
        render_aquarelle_orb(&mut p_base, (32.0, 32.0), 20.0, [200, 100, 50], 7, base);
        let base_data: Vec<u8> = p_base.data().to_vec();

        for (name, modified) in [
            ("bleed", AquarelleParams { bleed: 1.0, ..base }),
            ("bloom", AquarelleParams { bloom: 1.0, ..base }),
            (
                "offset",
                AquarelleParams {
                    offset: 1.0,
                    ..base
                },
            ),
            ("halo", AquarelleParams { halo: 1.0, ..base }),
        ] {
            let mut p = fresh_pixmap(64, 64);
            render_aquarelle_orb(&mut p, (32.0, 32.0), 20.0, [200, 100, 50], 7, modified);
            assert_ne!(
                p.data(),
                &base_data[..],
                "{name}=1.0 should change rendered orb"
            );
        }
    }

    #[test]
    fn deterministic_with_seed() {
        let mut a = fresh_pixmap(64, 64);
        let mut b = fresh_pixmap(64, 64);
        let params = AquarelleParams::default();
        render_aquarelle_orb(&mut a, (32.0, 32.0), 20.0, [200, 100, 50], 12345, params);
        render_aquarelle_orb(&mut b, (32.0, 32.0), 20.0, [200, 100, 50], 12345, params);
        assert_eq!(
            a.data(),
            b.data(),
            "same seed + inputs must produce identical output"
        );
    }
}
