//! orb の一方通行コンベアベルト型アニメーションモジュール。
//!
//! 時間 `t ∈ [0, 1]` を受け取り、その時刻における 1 フレーム
//! ([`image::RgbaImage`]) を返す関数 [`render_frame`] を提供する。
//! `t = 0` と `t = 1` は同一フレームに収束する完全ループ。
//!
//! # コンセプト
//!
//! - 1 動画につき方向は 1 つだけ（左→右 / 右→左 / 上→下 / 下→上）
//! - 全 orb が同じ方向に**ゆったり一方通行**で進む
//! - orb は元の位置に戻らず、リサジュー反射もしない
//! - 画面端から消えた orb は、反対側から新しい orb として入ってくる
//!   （wrap = `rem_euclid` による永続ループ）
//! - orb ごとに初期位相 (phase) を 0..1 でばらけさせ、配置と「同期しない」感を作る
//! - 移動中は全 orb 共通で半径・blur・opacity が呼吸的に微揺らぎ
//!   （独立モードではなく、常に薄く乗る自動効果）
//! - 静止画は流れの一瞬。t=0 のフレームを切り取った絵で、phase 由来で
//!   orb が散らばっており、画面端で半分欠けるのが普通の状態
//!
//! # 設計メモ
//!
//! - 軌道はもはやリサジュー曲線ではなく、**進行方向への線形運動 +
//!   `rem_euclid(1.0)` による wrap**。直交軸の位置は初期位置から動かない。
//! - 各 orb の進行量は `(phase + (cycle_count * t).fract()).rem_euclid(1.0)`。
//!   `cycle_count` は整数 (1/2/3) なので t=1.0 で fract が 0 になり、t=0 と
//!   完全一致するピクセル単位ループが成り立つ。
//! - 速度ジッタ（orb ごとに ±20% 速度変える）は **入れない**。整数 cycle で
//!   ループ閉じる前提を壊さないため。代わりに **phase の散らばり** (0..1 の一様分布)
//!   で「個体差」を出す。phase が違えば、画面上の各時点の orb 位置が散らばる
//!   ので「同期して動いていない」感が出る。
//! - 呼吸揺らぎは `sin_loop(1, t, 1, phi_breath)` で全動画 1 周。半径 ±10% の
//!   `weight` 倍率に転換することで `render_static` 側に手を入れずに揺らせる。
//!   blur / opacity の独立揺らぎは将来 Issue で検討（現状は半径だけで十分）。
//! - RNG は [`rand_chacha::ChaCha8Rng`] を `seed` で固定。同じ seed・clusters・
//!   t で 100% 同一フレームが返る。
//! - 描画は [`crate::orb::render_static`] を素直に呼ぶ。位置と weight を変調した
//!   `Cluster` 列を作って渡す。

use crate::cluster::{Centroid, Cluster};
use crate::orb::{render_static, OrbShape, RenderOptions};
use image::RgbaImage;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::f32::consts::TAU;

/// 流れる方向。1 動画で 1 方向のみ。
///
/// 各 orb は同じ方向に同じ向きで進む。逆向きの orb は混ぜない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionDirection {
    /// 左から右へ流れる。
    LeftToRight,
    /// 右から左へ流れる。
    RightToLeft,
    /// 上から下へ流れる。
    TopToBottom,
    /// 下から上へ流れる。
    BottomToTop,
}

/// 流れの速さ。動画全体（duration）で何回画面を横断するかを整数 3 段階で表す。
///
/// 整数横断回数にすることで `t=0` と `t=1` のフレームが完全一致する（ループ性）。
/// 8 秒クリップで VerySlow なら 8 秒で 1 回横断、Slow なら 4 秒で 1 回横断、
/// Medium なら 2.7 秒で 1 回横断。実時間で「ずっと遅い」感を出すには、長め
/// の `duration_ms`（6000〜10000 ms 程度）と組み合わせること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionSpeed {
    /// 動画全体で画面 1 回横断（最も穏やか、既定相当）。
    VerySlow,
    /// 動画全体で画面 2 回横断（既定）。
    Slow,
    /// 動画全体で画面 3 回横断（少し速め）。
    Medium,
}

impl MotionSpeed {
    /// 動画全体での横断回数。整数なので `t=0` と `t=1` で進行量が完全一致する。
    pub(crate) fn cycle_count(self) -> u32 {
        match self {
            MotionSpeed::VerySlow => 1,
            MotionSpeed::Slow => 2,
            MotionSpeed::Medium => 3,
        }
    }
}

/// アニメーション 1 フレーム描画のオプション。
#[derive(Debug, Clone)]
pub struct AnimateOptions {
    pub width: u32,
    pub height: u32,
    pub orb_size: f32,
    pub blur: f32,
    pub saturation: f32,
    pub direction: MotionDirection,
    pub speed: MotionSpeed,
    pub seed: u64,
    /// 背景 RGBA。alpha=0 で透過。
    pub background: [u8; 4],
    /// orb の描画形式。
    pub shape: OrbShape,
}

impl Default for AnimateOptions {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction: MotionDirection::LeftToRight,
            speed: MotionSpeed::Slow,
            seed: 0,
            background: [0, 0, 0, 255],
            shape: OrbShape::Circle,
        }
    }
}

/// 各 orb の決定的なパラメータ。
///
/// `phase` は 0..1 の初期位置オフセット。`phi_breath` は呼吸の位相シフト。
/// 速度ジッタは入れない（ループ性が崩れるため。代わりに phase で散らばらせる）。
#[derive(Debug, Clone, Copy)]
struct OrbParams {
    /// 進行方向の初期位置 (0..1)。これだけで「速度違いに見える」効果を作る。
    phase: f32,
    /// 呼吸 sin の位相シフト。
    phi_breath: f32,
}

/// `seed` から各 orb のパラメータを決定的に生成する。
fn generate_orb_params(seed: u64, n_orbs: usize) -> Vec<OrbParams> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n_orbs)
        .map(|_| OrbParams {
            phase: rng.gen_range(0.0..1.0),
            phi_breath: rng.gen_range(0.0..TAU),
        })
        .collect()
}

/// `(f * t * scale)` を [0, 1) に巻き戻してから 2π を掛け、phi を加えて sin を取る。
///
/// `f` と `scale` がともに整数のとき、`t = 1.0` ちょうどで `(f * t * scale)` は
/// 整数になり `fract()` は 0.0、`t = 0.0` のときの `sin(phi)` と完全に同一の
/// 演算に収束する。これが t=0 / t=1 フレーム完全一致（=ループ性）の根拠。
#[inline]
fn sin_loop(f: u32, t: f32, scale: u32, phi: f32) -> f32 {
    let raw = (f as f32 * t * scale as f32).fract();
    (raw * TAU + phi).sin()
}

/// 時間 `t` における 1 フレームを描画する。
///
/// `t = 0.0` と `t = 1.0` は同一フレームを返す（完全ループ）。
///
/// # ループ性の根拠
///
/// - 進行方向の位置: `(phase + (cycle * t).fract()).rem_euclid(1.0)`。
///   `cycle_count` を整数 1 / 2 / 3 に固定し、`(cycle * t).fract()` で先に整数部を
///   捨てる。`t = 1.0` ちょうどで `(cycle * 1.0)` は整数になるので fract は 0、
///   `t = 0.0` のときと完全同一の演算に収束する。
/// - 呼吸揺らぎ: `sin_loop(1, t, 1, phi)` で 1 周。t=0 と t=1 で同じ sin 値。
///
/// # 決定論性
///
/// 同じ seed と同じ clusters なら出力は完全一致する。RNG は cluster index 順に
/// 消費するため、cluster 数や順序が変わると各 orb の phase / phi_breath も変わる。
pub fn render_frame(clusters: &[Cluster], opts: &AnimateOptions, t: f32) -> RgbaImage {
    let cycle = opts.speed.cycle_count();
    let params = generate_orb_params(opts.seed, clusters.len());

    let modulated: Vec<Cluster> = clusters
        .iter()
        .zip(params.iter())
        .map(|(c, p)| {
            // 進行量（0..1）。phase で初期位置を散らばらせ、cycle * t で進める。
            // cycle が整数なので t=0 と t=1 では `phase` と `phase + cycle` が
            // mod 1 で完全一致し、フレームがピクセル一致でループする。
            // `(cycle * t).fract()` で先に整数部を捨てておくのが鍵。
            let advance = (cycle as f32 * t).fract();
            let progress = (p.phase + advance).rem_euclid(1.0);

            // 方向に応じて x または y のどちらかだけが進む。直交軸は不動。
            // 「初期位置 = centroid」ではなく、進行方向の軸は `progress` で完全に
            // 上書きする。これにより phase が散らばっていれば orb が画面全体に
            // ばらまかれた状態になる。
            let (new_x, new_y) = match opts.direction {
                MotionDirection::LeftToRight => (progress, c.centroid.y),
                MotionDirection::RightToLeft => (1.0 - progress, c.centroid.y),
                MotionDirection::TopToBottom => (c.centroid.x, progress),
                MotionDirection::BottomToTop => (c.centroid.x, 1.0 - progress),
            };

            // 呼吸揺らぎ（全 orb 共通の自動効果）。半径のみ変調する。
            // blur / opacity を本気で変えるには render_static 側に手を入れる必要があるが、
            // 半径の ±10% で「ふわっとした明滅感」は十分に出るので、現状は半径だけに
            // 留める（API 拡張なしで実装できる範囲）。
            let breath_phase = sin_loop(1, t, 1, p.phi_breath);
            let radius_factor = 1.0 + 0.10 * breath_phase;

            // radius = base * sqrt(weight) なので、半径を radius_factor 倍するには
            // weight を radius_factor^2 倍すれば良い。
            let weight_scale = radius_factor * radius_factor;

            Cluster {
                color: c.color,
                centroid: Centroid { x: new_x, y: new_y },
                weight: (c.weight * weight_scale).max(0.0),
            }
        })
        .collect();

    let render_opts = RenderOptions {
        width: opts.width,
        height: opts.height,
        orb_size: opts.orb_size,
        blur: opts.blur,
        saturation: opts.saturation,
        background: opts.background,
        shape: opts.shape,
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

    fn opts_with(direction: MotionDirection, speed: MotionSpeed) -> AnimateOptions {
        AnimateOptions {
            width: 64,
            height: 64,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction,
            speed,
            seed: 12345,
            background: [0, 0, 0, 255],
            shape: OrbShape::Circle,
        }
    }

    fn pixels_equal(a: &RgbaImage, b: &RgbaImage) -> bool {
        a.dimensions() == b.dimensions() && a.as_raw() == b.as_raw()
    }

    #[test]
    fn t_zero_and_t_one_match() {
        // wrap で進行量 = (phase + cycle*0) と (phase + cycle*1) は mod 1 で同一に
        // なるので t=0 と t=1 のフレームは完全一致する（ループ性）。
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
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
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
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
        // t を変えると進行量が変わって別フレームになる。
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        let clusters = sample_clusters();
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.5);
        assert!(
            !pixels_equal(&a, &b),
            "different t must produce different frames under Slow motion"
        );
    }

    #[test]
    fn different_seed_changes_layout() {
        // 同じ clusters・opts でも seed が違うと phase が変わって配置が変わる。
        let clusters = sample_clusters();
        let mut opts_a = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        let mut opts_b = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts_a.seed = 1;
        opts_b.seed = 2;
        let a = render_frame(&clusters, &opts_a, 0.25);
        let b = render_frame(&clusters, &opts_b, 0.25);
        assert!(
            !pixels_equal(&a, &b),
            "different seed should change orb phase (and hence the frame)"
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
    fn all_direction_speed_combinations_loop_closed() {
        // 全 direction × 全 speed で t=0 と t=1 が完全一致することを検証する。
        let clusters = sample_clusters();
        for dir in [
            MotionDirection::LeftToRight,
            MotionDirection::RightToLeft,
            MotionDirection::TopToBottom,
            MotionDirection::BottomToTop,
        ] {
            for speed in [
                MotionSpeed::VerySlow,
                MotionSpeed::Slow,
                MotionSpeed::Medium,
            ] {
                let opts = opts_with(dir, speed);
                let a = render_frame(&clusters, &opts, 0.0);
                let b = render_frame(&clusters, &opts, 1.0);
                assert!(
                    pixels_equal(&a, &b),
                    "loop closure broken for direction={dir:?} speed={speed:?}"
                );
            }
        }
    }

    #[test]
    fn left_to_right_does_not_move_vertically() {
        // LeftToRight では y が初期位置（centroid.y）のまま動かない。
        // 1 cluster だけ centroid を画面下半分に置き、最も明るいピクセルの y 座標が
        // t=0 と t=0.5 で一致することを確認する。
        let clusters = vec![cluster([255, 0, 0], 0.5, 0.7, 0.5)];
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.5);
        let bright_y = |img: &RgbaImage| -> u32 {
            let mut best = 0u32;
            let mut best_v = 0u32;
            for y in 0..img.height() {
                let mut row_sum = 0u32;
                for x in 0..img.width() {
                    row_sum += img.get_pixel(x, y)[0] as u32;
                }
                if row_sum > best_v {
                    best_v = row_sum;
                    best = y;
                }
            }
            best
        };
        assert_eq!(
            bright_y(&a),
            bright_y(&b),
            "LeftToRight must not shift vertically"
        );
    }

    #[test]
    fn top_to_bottom_does_not_move_horizontally() {
        // TopToBottom では x が初期位置のまま動かない。
        let clusters = vec![cluster([255, 0, 0], 0.3, 0.5, 0.5)];
        let opts = opts_with(MotionDirection::TopToBottom, MotionSpeed::Slow);
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.5);
        let bright_x = |img: &RgbaImage| -> u32 {
            let mut best = 0u32;
            let mut best_v = 0u32;
            for x in 0..img.width() {
                let mut col_sum = 0u32;
                for y in 0..img.height() {
                    col_sum += img.get_pixel(x, y)[0] as u32;
                }
                if col_sum > best_v {
                    best_v = col_sum;
                    best = x;
                }
            }
            best
        };
        assert_eq!(
            bright_x(&a),
            bright_x(&b),
            "TopToBottom must not shift horizontally"
        );
    }

    #[test]
    fn left_to_right_advances_x_over_time() {
        // 単一 orb で phase=0 になるよう調整は難しいので、直接位置を計算して比較する。
        // generate_orb_params の RNG は seed=0 で再現できるので、その orb が t=0.0 と
        // t=0.25 で異なる x を持つことを確認する。
        let clusters = vec![cluster([255, 255, 255], 0.5, 0.5, 1.0)];
        let opts = AnimateOptions {
            width: 128,
            height: 128,
            seed: 7,
            ..AnimateOptions::default()
        };
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.25);
        assert!(
            !pixels_equal(&a, &b),
            "LeftToRight must shift horizontally between t=0 and t=0.25"
        );
    }

    #[test]
    fn wrap_brings_orb_back_at_t_one() {
        // 1 周期分進んだ orb は元の位置に戻ってくる（rem_euclid による wrap）。
        // 既に t_zero_and_t_one_match で確認済みだが、Slow 以外も含めて再確認する。
        let clusters = sample_clusters();
        for speed in [
            MotionSpeed::VerySlow,
            MotionSpeed::Slow,
            MotionSpeed::Medium,
        ] {
            let opts = opts_with(MotionDirection::LeftToRight, speed);
            let a = render_frame(&clusters, &opts, 0.0);
            let b = render_frame(&clusters, &opts, 1.0);
            assert!(
                pixels_equal(&a, &b),
                "wrap loop must bring frame back at t=1 (speed={speed:?})"
            );
        }
    }

    #[test]
    fn breathe_changes_frame_over_time() {
        // 全 orb 共通の呼吸揺らぎ（半径 ±10%）が効いていることを、
        // 進行方向に動かない y 座標を持つ orb で間接的に確認する。
        // t=0 と t=0.25 では breath_phase が異なるので weight も異なり、フレームも異なる。
        // （Top/Bottom 方向ではないので x も動くが、半径も変わる）
        let clusters = sample_clusters();
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::VerySlow);
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.25);
        assert!(
            !pixels_equal(&a, &b),
            "breathing modulation must produce different frames over time"
        );
    }

    #[test]
    fn cycle_count_matches_speed() {
        // 速度の cycle_count が仕様通りであることを保証する回帰テスト。
        assert_eq!(MotionSpeed::VerySlow.cycle_count(), 1);
        assert_eq!(MotionSpeed::Slow.cycle_count(), 2);
        assert_eq!(MotionSpeed::Medium.cycle_count(), 3);
    }
}
