//! orb のゆったり移動アニメーションモジュール。
//!
//! 時間 `t ∈ [0, 1]` を受け取り、その時刻における 1 フレーム
//! ([`image::RgbaImage`]) を返す関数 [`render_frame`] を提供する。
//! `t = 0` と `t = 1` は同一フレームに収束する完全ループ。
//!
//! # 設計メモ
//!
//! - 軌道は **リサジュー曲線** (sin(2π·a·t + φx), sin(2π·b·t + φy)) を採用する。
//!   周波数比 (a, b) は整数比から RNG で選ぶ。整数比なので位相は t=1 で完全に
//!   閉じ、ループ性が浮動小数点誤差なしに保証される。位相を計算する際は
//!   `(a * t * freq_scale).fract()` で先に整数部を捨てておくことで、
//!   t=1.0 の整数倍を `sin(2π·0)` と完全同一の演算に収束させる。
//! - RNG は [`rand_chacha::ChaCha8Rng`] を `seed` で固定。同じ seed・clusters・
//!   t で 100% 同一フレームが返る。
//! - 描画は [`crate::orb::render_static`] を素直に呼ぶ。位置と色を変調した
//!   `Cluster` 列を作って渡す。
//! - 色揺らぎは HSL の S と L に微小な追加倍率をかける。`AnimateOptions.saturation`
//!   とは独立 — saturation はそのまま [`crate::orb::RenderOptions`] に渡し、
//!   揺らぎは local な追加変動として上に乗る（二重に saturation が掛からないよう
//!   注意）。
//! - `MotionPreset::Still` は amplitude も freq_scale も 0 で、`render_static`
//!   と完全同一の結果を返す。

use crate::cluster::{Centroid, Cluster};
use crate::orb::{render_static, RenderOptions};
use image::RgbaImage;
use palette::{FromColor, Hsl, IntoColor, Srgb};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::f32::consts::TAU;

/// アニメーションの動きの強さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionPreset {
    /// 動かない（render_static と同じ結果）
    Still,
    /// ゆったり（既定）
    Slow,
    /// 活発
    Lively,
}

impl MotionPreset {
    /// motion preset から (位置振幅, 色振幅, 周波数倍率) を取り出す。
    ///
    /// - 位置振幅は短辺 `min(width, height)` に対する比（0.06 → 6%）。
    /// - 色振幅は HSL の S/L にかかる加算的な揺らぎ係数。
    /// - 周波数倍率は「t=1 で何周回るか」を制御する**整数限定**。リサジュー比
    ///   (a, b) と掛け合わせても整数のままなのでループ性は保たれる。ここを
    ///   非整数にすると `(a * t * scale).fract()` が t=1 で 0 にならず、
    ///   t=0/t=1 のフレーム完全一致が崩れる。
    // Still は amplitude=0 なので freq_scale の値は何でも結果が同じだが、
    // 「動いていない」を表現するために 0 で揃える。
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn coefficients(self) -> (f32, f32, u32) {
        match self {
            MotionPreset::Still => (0.0, 0.0, 0),
            MotionPreset::Slow => (0.06, 0.05, 1),
            MotionPreset::Lively => (0.12, 0.10, 2),
        }
    }
}

/// アニメーション 1 フレーム描画のオプション。
///
/// CLI 側の `Motion` enum との橋渡しは #5 (動画出力結線) で行う。
/// それまで MotionPreset は内部 enum として独立。
#[derive(Debug, Clone)]
pub struct AnimateOptions {
    pub width: u32,
    pub height: u32,
    pub orb_size: f32,
    pub blur: f32,
    pub saturation: f32,
    pub motion: MotionPreset,
    pub seed: u64,
}

impl Default for AnimateOptions {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            motion: MotionPreset::Slow,
            seed: 0,
        }
    }
}

/// リサジュー周波数比の候補。すべて整数比なのでループ性が保証される。
pub(crate) const FREQ_RATIOS: &[(u32, u32)] = &[(1, 2), (2, 3), (3, 4), (1, 3), (2, 5)];

/// 各 cluster の決定的な軌道パラメータ。
#[derive(Debug, Clone, Copy)]
struct OrbitParams {
    a: u32,
    b: u32,
    phi_x: f32,
    phi_y: f32,
    phi_color: f32,
}

/// `seed` から各 cluster の軌道パラメータを決定的に生成する。
fn generate_orbit_params(seed: u64, n_clusters: usize) -> Vec<OrbitParams> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n_clusters)
        .map(|_| {
            let (a, b) = FREQ_RATIOS[rng.gen_range(0..FREQ_RATIOS.len())];
            OrbitParams {
                a,
                b,
                phi_x: rng.gen_range(0.0..TAU),
                phi_y: rng.gen_range(0.0..TAU),
                phi_color: rng.gen_range(0.0..TAU),
            }
        })
        .collect()
}

/// `(f * t * scale)` を [0, 1) に巻き戻してから 2π を掛け、phi を加えて sin を取る。
///
/// `f` と `scale` がともに整数のとき、`t = 1.0` ちょうどで `(f * t * scale)` は
/// 整数になり `fract()` は 0.0、`t = 0.0` のときの `sin(phi)` と完全に同一の
/// 演算に収束する。これが t=0 / t=1 フレーム完全一致（=ループ性）の根拠。
/// 引数の型 `u32` がこの「整数限定」を型レベルで保証する。
#[inline]
fn sin_loop(f: u32, t: f32, scale: u32, phi: f32) -> f32 {
    let raw = (f as f32 * t * scale as f32).fract();
    (raw * TAU + phi).sin()
}

/// HSL の S と L に乗算的な微変動をかける（揺らぎ）。
///
/// `s_factor` / `l_factor` は `1 + amplitude * sin(...)` の形で渡されることを
/// 想定する。saturation の cluster 全体倍率は呼び出し側（`render_static`）で
/// かかるので、ここでは触らない。
///
/// 注意: render_static 側でも HSL 経由の saturation 調整があるので、
/// このフレームでは色が HSL→RGB→HSL→RGB と往復する。
/// 各段階で f32→u8 量子化が入るが、Slow preset の amplitude_color=0.05 程度なら
/// 視覚的に差は出ない（経験的に色差 ΔE で 1 未満）。
/// より精密な制御が必要になったら fused パイプラインに置換する。
fn modulate_color(rgb: [u8; 3], s_factor: f32, l_factor: f32) -> [u8; 3] {
    let srgb = Srgb::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    let mut hsl: Hsl = Hsl::from_color(srgb);
    hsl.saturation = (hsl.saturation * s_factor).clamp(0.0, 1.0);
    hsl.lightness = (hsl.lightness * l_factor).clamp(0.0, 1.0);
    let out: Srgb = hsl.into_color();
    [
        (out.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// 時間 `t` における 1 フレームを描画する。
///
/// `t = 0.0` と `t = 1.0` は同一フレームを返す（完全ループ）。
/// `MotionPreset::Still` のときは `t` に依存せず常に同じフレームを返す。
///
/// 決定論性: 同じ seed と同じ clusters なら出力は完全一致する。
/// ただし RNG は cluster index 順に消費するため、cluster 数や順序が変わると
/// 各 orb の軌道パラメータも変わる（後段の cluster ほど影響を受ける）。
// TODO: weight ベースの振幅補正（小さい orb は小さく動く）は将来 Issue で検討。
// 現状は全 orb 同振幅で軌道を描く。
pub fn render_frame(clusters: &[Cluster], opts: &AnimateOptions, t: f32) -> RgbaImage {
    let (amp_pos, amp_color, freq_scale) = opts.motion.coefficients();
    let params = generate_orbit_params(opts.seed, clusters.len());

    // amp_pos は短辺 (min(w, h)) 基準の振幅比。normalized 座標 [0, 1] に直接
    // 加算すると、render_static 側で x は width 倍・y は height 倍されて
    // 縦横比に歪んだ楕円になってしまう。axis-scale 補正で normalized 座標系
    // における円軌道を保つ（実際のピクセル変位は dx_pixel = amp_pos*min_side*sin、
    // dy_pixel も同じ）。
    let min_side = opts.width.min(opts.height) as f32;
    let scale_x = min_side / opts.width as f32; // <= 1.0
    let scale_y = min_side / opts.height as f32; // <= 1.0

    let modulated: Vec<Cluster> = clusters
        .iter()
        .zip(params.iter())
        .map(|(c, p)| {
            // 位置: リサジュー軌道（短辺基準で円軌道を維持）
            let dx = amp_pos * scale_x * sin_loop(p.a, t, freq_scale, p.phi_x);
            let dy = amp_pos * scale_y * sin_loop(p.b, t, freq_scale, p.phi_y);
            // 範囲外に出た場合は単純に clamp する。reflect / wrap のほうが見た目は滑らかだが、
            // prototype 段階では clamp で十分（軌道半径は数% 程度なので長時間停滞は起きにくい）。
            let new_x = (c.centroid.x + dx).clamp(0.0, 1.0);
            let new_y = (c.centroid.y + dy).clamp(0.0, 1.0);

            // 色揺らぎ: HSL の S と L を 1 ± amp_color の範囲で振る。
            // L は派手すぎないよう半振幅に。
            let color_phase = sin_loop(1, t, freq_scale, p.phi_color);
            let s_factor = 1.0 + amp_color * color_phase;
            let l_factor = 1.0 + (amp_color * 0.5) * color_phase;
            let new_color = modulate_color(c.color, s_factor, l_factor);

            Cluster {
                color: new_color,
                centroid: Centroid { x: new_x, y: new_y },
                weight: c.weight,
            }
        })
        .collect();

    let render_opts = RenderOptions {
        width: opts.width,
        height: opts.height,
        orb_size: opts.orb_size,
        blur: opts.blur,
        saturation: opts.saturation,
    };
    render_static(&modulated, &render_opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster(color: [u8; 3], cx: f32, cy: f32, weight: f32) -> Cluster {
        Cluster {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight,
        }
    }

    fn sample_clusters() -> Vec<Cluster> {
        vec![
            cluster([220, 60, 60], 0.3, 0.4, 0.5),
            cluster([60, 120, 220], 0.7, 0.6, 0.3),
            cluster([200, 200, 80], 0.5, 0.2, 0.2),
        ]
    }

    fn small_opts(motion: MotionPreset) -> AnimateOptions {
        AnimateOptions {
            width: 64,
            height: 64,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            motion,
            seed: 12345,
        }
    }

    fn pixels_equal(a: &RgbaImage, b: &RgbaImage) -> bool {
        a.dimensions() == b.dimensions() && a.as_raw() == b.as_raw()
    }

    #[test]
    fn t_zero_and_t_one_match() {
        // 整数周波数比 + 整数 freq_scale + fract 補正により、t=0 と t=1 は
        // 完全同一フレームになることを検証（ループ性）。
        let opts = small_opts(MotionPreset::Slow);
        let clusters = sample_clusters();
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 1.0);
        assert!(
            pixels_equal(&a, &b),
            "t=0.0 and t=1.0 must produce identical frames (loop closure)"
        );
    }

    #[test]
    fn same_seed_same_t_deterministic() {
        // 同じ seed・clusters・opts・t で 2 回 render すると完全一致する。
        let opts = small_opts(MotionPreset::Slow);
        let clusters = sample_clusters();
        let a = render_frame(&clusters, &opts, 0.37);
        let b = render_frame(&clusters, &opts, 0.37);
        assert!(
            pixels_equal(&a, &b),
            "same seed + same t must produce identical frames"
        );
    }

    #[test]
    fn different_t_produces_different_frame() {
        // 同じ seed で t を変えると別フレームになる（Slow preset は動く）。
        let opts = small_opts(MotionPreset::Slow);
        let clusters = sample_clusters();
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.25);
        assert!(
            !pixels_equal(&a, &b),
            "different t must produce different frames under Slow motion"
        );
    }

    #[test]
    fn still_motion_independent_of_t() {
        // Still preset は amplitude=0, freq_scale=0 なので t に依存せず常に同じ。
        let opts = small_opts(MotionPreset::Still);
        let clusters = sample_clusters();
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.3);
        let c = render_frame(&clusters, &opts, 0.7);
        assert!(pixels_equal(&a, &b), "Still: t=0.0 vs t=0.3 must match");
        assert!(pixels_equal(&b, &c), "Still: t=0.3 vs t=0.7 must match");
    }

    #[test]
    fn lively_amplitude_larger_than_slow() {
        // Slow と Lively で完全一致しないこと（preset の違いが描画に表れる）と、
        // t=0 からの差分総和が Lively > Slow になることの両方を担保する。
        // 位置振幅 (Slow=0.06, Lively=0.12) と freq_scale (1 vs 2) の両方が
        // 効くので、Lively の方が動きが大きく出るはず。
        let clusters = sample_clusters();
        let slow_opts = small_opts(MotionPreset::Slow);
        let lively_opts = small_opts(MotionPreset::Lively);
        let slow = render_frame(&clusters, &slow_opts, 0.25);
        let lively = render_frame(&clusters, &lively_opts, 0.25);

        assert!(
            !pixels_equal(&slow, &lively),
            "Slow and Lively must render differently at t=0.25"
        );

        let slow_t0 = render_frame(&clusters, &slow_opts, 0.0);
        let lively_t0 = render_frame(&clusters, &lively_opts, 0.0);
        let slow_diff: u64 = slow
            .as_raw()
            .iter()
            .zip(slow_t0.as_raw().iter())
            .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
            .sum();
        let lively_diff: u64 = lively
            .as_raw()
            .iter()
            .zip(lively_t0.as_raw().iter())
            .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
            .sum();
        assert!(
            lively_diff > slow_diff,
            "Lively diff ({}) should exceed Slow diff ({}) from t=0 reference",
            lively_diff,
            slow_diff
        );
    }

    #[test]
    fn different_seed_changes_orbit() {
        // 同じ clusters・opts でも seed が違うと（Still 以外で）軌道が変わるはず。
        let clusters = sample_clusters();
        let mut opts_a = small_opts(MotionPreset::Slow);
        let mut opts_b = small_opts(MotionPreset::Slow);
        opts_a.seed = 1;
        opts_b.seed = 2;
        let a = render_frame(&clusters, &opts_a, 0.25);
        let b = render_frame(&clusters, &opts_b, 0.25);
        assert!(
            !pixels_equal(&a, &b),
            "different seed should change the orbit (and hence the frame)"
        );
    }

    #[test]
    fn dimensions_match_options() {
        let opts = AnimateOptions {
            width: 80,
            height: 120,
            ..AnimateOptions::default()
        };
        let clusters = sample_clusters();
        let img = render_frame(&clusters, &opts, 0.1);
        assert_eq!(img.width(), 80);
        assert_eq!(img.height(), 120);
    }

    #[test]
    fn freq_scale_combinations_have_integer_period() {
        // すべての (a, b) × freq_scale で t=1 のとき位相が完全に閉じることを
        // 数値レベルで検証。整数 a/b と整数 scale の積は常に整数なので
        // fract() は 0.0 になるはず。
        for &(a, b) in FREQ_RATIOS {
            for preset in [
                MotionPreset::Still,
                MotionPreset::Slow,
                MotionPreset::Lively,
            ] {
                let (_, _, scale) = preset.coefficients();
                let prod_a = (a as f32 * 1.0 * scale as f32).fract();
                let prod_b = (b as f32 * 1.0 * scale as f32).fract();
                assert_eq!(prod_a, 0.0, "a={a} scale={scale}");
                assert_eq!(prod_b, 0.0, "b={b} scale={scale}");
            }
        }
    }
}
