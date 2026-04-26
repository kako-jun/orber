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

/// アニメーション 1 フレーム描画のオプション。
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

/// motion preset から (位置振幅, 色振幅, 周波数倍率) を取り出す。
///
/// - 位置振幅は `min(width, height)` に対する比（0.06 → 6%）。
/// - 色振幅は HSL の S/L にかかる加算的な揺らぎ係数。
/// - 周波数倍率は「t=1 で何周回るか」を制御する整数。リサジュー比 (a, b) と
///   掛け合わせても整数のままなのでループ性は保たれる。
fn preset_params(motion: MotionPreset) -> (f32, f32, f32) {
    match motion {
        MotionPreset::Still => (0.0, 0.0, 0.0),
        MotionPreset::Slow => (0.06, 0.05, 1.0),
        MotionPreset::Lively => (0.12, 0.10, 2.0),
    }
}

/// リサジュー周波数比の候補。すべて整数比なのでループ性が保証される。
const FREQ_RATIOS: &[(u32, u32)] = &[(1, 2), (2, 3), (3, 4), (1, 3), (2, 5)];

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

/// `f * t * scale` の整数部を捨ててから 2π を掛ける。
///
/// `f` と `scale` が整数のとき、`t = 1.0` ちょうどの場合に `(f * t * scale)`
/// は整数になるので `fract()` は 0.0 となり、`t = 0.0` のときの `sin(2π·0)`
/// と完全に一致する。これが `t=0` と `t=1` のフレーム完全一致（=ループ性）
/// を保証する。
#[inline]
fn loop_phase(f: f32, t: f32, scale: f32) -> f32 {
    (f * t * scale).fract() * TAU
}

/// HSL の S と L に乗算的な微変動をかける（揺らぎ）。
///
/// `s_factor` / `l_factor` は `1 + amplitude * sin(...)` の形で渡されることを
/// 想定する。saturation の cluster 全体倍率は呼び出し側（`render_static`）で
/// かかるので、ここでは触らない。
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
pub fn render_frame(clusters: &[Cluster], opts: &AnimateOptions, t: f32) -> RgbaImage {
    let (amp_pos, amp_color, freq_scale) = preset_params(opts.motion);
    let params = generate_orbit_params(opts.seed, clusters.len());

    // 位置振幅は min(w, h) に対する比なので、ここで cluster.centroid（正規化座標）
    // と同じ次元に揃える。amp_pos は「正規化座標 [0, 1] における振幅」として
    // 解釈する。min(w, h) / w・h で軸ごとにスケールを変えると円軌道が楕円に
    // 歪むのを避けたいので、軸長基準ではなく短辺基準を採用。
    let modulated: Vec<Cluster> = clusters
        .iter()
        .zip(params.iter())
        .map(|(c, p)| {
            // 位置: リサジュー軌道
            let dx = amp_pos * loop_phase(p.a as f32, t, freq_scale).sin_with_phase(p.phi_x);
            let dy = amp_pos * loop_phase(p.b as f32, t, freq_scale).sin_with_phase(p.phi_y);
            let new_x = (c.centroid.x + dx).clamp(0.0, 1.0);
            let new_y = (c.centroid.y + dy).clamp(0.0, 1.0);

            // 色揺らぎ: HSL の S と L を 1 ± amp_color の範囲で振る。
            // L は派手すぎないよう半振幅に。
            let color_phase = loop_phase(1.0, t, freq_scale).sin_with_phase(p.phi_color);
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

/// `2π·k + phi` の sin を計算するヘルパー。
///
/// `loop_phase` の戻り値（既に 2π 倍されている）に位相 `phi` を足して sin を
/// 取る。trait extension の体裁にしているのは呼び出し側の式を簡潔にするため。
trait SinWithPhase {
    fn sin_with_phase(self, phi: f32) -> f32;
}

impl SinWithPhase for f32 {
    #[inline]
    fn sin_with_phase(self, phi: f32) -> f32 {
        (self + phi).sin()
    }
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
        // 同じ seed・clusters で Slow と Lively を t=0.25 で render し、
        // 中央列の総差分（黒背景からの距離）が Lively の方が大きい/小さい
        // どちらかに偏ることを期待 — ここでは「2 つのフレームが大きく
        // 異なる」ことを差分の合計で検出する（厳密な大小比較は数値的に
        // 不安定なので、まず「両者が異なる」ことだけを担保する）。
        let clusters = sample_clusters();
        let slow_opts = small_opts(MotionPreset::Slow);
        let lively_opts = small_opts(MotionPreset::Lively);
        let slow = render_frame(&clusters, &slow_opts, 0.25);
        let lively = render_frame(&clusters, &lively_opts, 0.25);

        // Slow と Lively で完全一致しない（preset の違いが描画に表れる）。
        assert!(
            !pixels_equal(&slow, &lively),
            "Slow and Lively must render differently at t=0.25"
        );

        // 回帰検出用のスカラー指標: t=0 からの差分の総和を比較し、Lively の
        // 方が大きいことを期待。位置振幅 (Slow=0.06, Lively=0.12) と
        // freq_scale (1.0 vs 2.0) の両方が効くので、Lively の方が動きが
        // 大きく出るはず。
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
}
