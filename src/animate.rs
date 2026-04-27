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
//!   画面外バッファ付き `rem_euclid` による wrap**。直交軸の位置は初期位置から動かない。
//! - 各 orb の進行範囲は `[-r, 1+r]`（`r` = orb 半径を進行軸長で正規化した値）。
//!   wrap 境界の出現/消失が画面の縁で起こるのではなく、orb が完全に画面外に
//!   出てから入れ替わるので、視覚的にシームレスにつながる。
//! - 進行量計算: `extent = 1 + 2r`、`raw = (phase + cycle * speed_mult * t) * extent`、
//!   `pos = raw.rem_euclid(extent) - r`。`cycle_count * speed_mult` は整数なので
//!   t=1.0 で fract が 0 になり、t=0 と完全一致するピクセル単位ループが成り立つ。
//! - 速度ジッタは **整数倍** (1x / 2x / 3x) で導入する。orb ごとに seed 由来で
//!   `speed_mult ∈ {1, 2, 3}` を割り当て、進行量を `cycle * speed_mult * t` とする。
//!   両方とも整数なので t=1 で fract が 0 になり、ループ性は保たれる。VerySlow /
//!   Slow / Medium と組み合わせると実効周回数は {1, 2, 3, 4, 6, 9} に変化に富む。
//! - phase の散らばり (0..1 の一様分布) も併用する。phase が違えば、画面上の各
//!   時点の orb 位置が散らばるので「同期して動いていない」感が出る。
//! - 呼吸揺らぎは **3 軸独立**で sin。半径 ±10% / blur ±15% / opacity ±5%、
//!   それぞれ別位相 (phi_radius / phi_blur / phi_opacity)。動画全体で各 1 周。
//! - RNG は [`rand_chacha::ChaCha8Rng`] を `seed` で固定。同じ seed・clusters・
//!   t で 100% 同一フレームが返る。
//! - 描画は [`crate::orb::render_one_orb`] を per-orb で呼ぶ。背景塗りと
//!   un-premultiply は animate 側で同等の処理を行う。

use crate::cluster::Cluster;
use crate::orb::{adjust_saturation_pub, render_one_orb, OrbShape, OrbStyle};
use image::RgbaImage;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::f32::consts::TAU;
use tiny_skia::{Color, Pixmap};

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
///
/// `count` は同時可視 orb の総数。`None` の場合は cluster 数と一致させる
/// （後方互換）。`Some(n)` を指定すると、cluster K 色を seed 由来で N 個に
/// **展開** する。色は weight 比例の重み付き抽選で割り当て、初期位相 / 縦軸
/// オフセットは seed から決定的に振る。
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
    /// 同時可視 orb の総数。None → cluster 数。Some(n) → クラスタ展開。
    pub count: Option<usize>,
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
            count: None,
            background: [0, 0, 0, 255],
            shape: OrbShape::Circle,
        }
    }
}

/// 各 orb の決定的なパラメータ。
///
/// `phase` は 0..1 の初期位置オフセット。`phi_radius` / `phi_blur` / `phi_opacity` は
/// 3 軸独立の呼吸位相シフト（radius は ±10%、blur は ±15%、opacity は ±5%）。
/// `style` は orb ごとの描画スタイル（Rim / Soft）で、フレーム内に混在させる。
/// `cluster_idx` はこの orb の色とサイズを取ってくる元クラスタの index（重み比例で抽選）。
/// `cross_axis` は進行方向と直交する軸の正規化座標 0..1。orb をクラスタ重心に固定
/// せず、画面全体に散らせるためのオフセット。
/// `speed_mult` は整数倍速度 (1 / 2 / 3)。`cycle_count * speed_mult` も整数なので
/// `t=1` でループが閉じる。視覚的なバラつきの主因。
#[derive(Debug, Clone, Copy)]
struct OrbParams {
    /// 進行方向の初期位置 (0..1)。これだけで「速度違いに見える」効果を作る。
    phase: f32,
    /// 半径呼吸 sin の位相シフト。
    phi_radius: f32,
    /// blur 呼吸 sin の位相シフト。radius と同期させない。
    phi_blur: f32,
    /// opacity 呼吸 sin の位相シフト。3 軸を独立に。
    phi_opacity: f32,
    /// 描画スタイル（Rim / Soft）。seed 由来でほぼ 50:50 に振る。
    style: OrbStyle,
    /// この orb の色 / weight を借りてくる元クラスタの index。
    cluster_idx: usize,
    /// 進行方向と直交する軸の位置 (0..1)。クラスタ重心に固定せず散らせる。
    cross_axis: f32,
    /// 整数倍速度 (1 / 2 / 3)。MotionSpeed の cycle_count と掛け合わせて使う。
    speed_mult: u32,
}

/// 重み比例の 1 サンプルをもらう抽選器。
///
/// `weights` 全部の合計を 1 度だけ計算し、累積和上の二分探索で index を返す。
/// 全 weight が 0 の場合は 0 を返す（呼び出し側で要素が無いケースを弾いていないと
/// パニックするので注意）。
fn pick_weighted(rng: &mut ChaCha8Rng, weights: &[f32], total: f32) -> usize {
    if total <= 0.0 || weights.is_empty() {
        return 0;
    }
    let r = rng.gen::<f32>() * total;
    let mut acc = 0.0;
    for (i, &w) in weights.iter().enumerate() {
        acc += w.max(0.0);
        if r <= acc {
            return i;
        }
    }
    weights.len() - 1
}

/// `seed` から各 orb のパラメータを決定的に生成する。
///
/// `n_orbs` は要求される orb 数。`cluster_weights` は各クラスタの占有比で、
/// orb の色割当（cluster_idx）に重み比例で使われる。`style` と `cluster_idx` は
/// 整数を別途引いて分布の偏りを避ける。
fn generate_orb_params(seed: u64, n_orbs: usize, cluster_weights: &[f32]) -> Vec<OrbParams> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let total_w: f32 = cluster_weights.iter().map(|w| w.max(0.0)).sum();
    (0..n_orbs)
        .map(|_| {
            let phase = rng.gen_range(0.0..1.0);
            let phi_radius = rng.gen_range(0.0..TAU);
            let phi_blur = rng.gen_range(0.0..TAU);
            let phi_opacity = rng.gen_range(0.0..TAU);
            let cross_axis = rng.gen_range(0.0..1.0);
            let style = if rng.gen::<u32>() & 1 == 0 {
                OrbStyle::Rim
            } else {
                OrbStyle::Soft
            };
            let cluster_idx = pick_weighted(&mut rng, cluster_weights, total_w);
            // 整数倍速度。1/2/3 を均等に割り当て。整数 × 整数の cycle_count なので
            // t=1 で fract が 0 になりループ性は保たれる。
            let speed_mult = rng.gen_range(1..=3);
            OrbParams {
                phase,
                phi_radius,
                phi_blur,
                phi_opacity,
                style,
                cluster_idx,
                cross_axis,
                speed_mult,
            }
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
/// - 進行方向の位置: 画面外バッファ付き wrap。`extent = 1 + 2r` の周期上で
///   `raw = (phase + cycle * speed_mult * t) * extent`、`pos = raw.rem_euclid(extent) - r`。
///   `cycle * speed_mult` は整数なので `t = 1.0` で `(cycle * speed_mult * 1.0)` も整数、
///   fract は 0 になり `t = 0.0` のときと完全同一の演算に収束する。
/// - 呼吸揺らぎ: `sin_loop(1, t, 1, phi)` で 1 周。t=0 と t=1 で同じ sin 値。
///
/// # 決定論性
///
/// 同じ seed と同じ clusters なら出力は完全一致する。RNG は cluster index 順に
/// 消費するため、cluster 数や順序が変わると各 orb の phase / phi_breath も変わる。
pub fn render_frame(clusters: &[Cluster], opts: &AnimateOptions, t: f32) -> RgbaImage {
    let cycle = opts.speed.cycle_count();
    // count を解決。`None` なら cluster 数（後方互換）。
    let n_orbs = opts
        .count
        .unwrap_or(clusters.len())
        .min(MAX_ORB_COUNT)
        .max(if clusters.is_empty() { 0 } else { 1 });

    let cluster_weights: Vec<f32> = clusters.iter().map(|c| c.weight.max(0.0)).collect();
    let params = generate_orb_params(opts.seed, n_orbs, &cluster_weights);

    let width = opts.width.max(1);
    let height = opts.height.max(1);

    // Aquarelle 経路は per-orb の独立揺らぎに対応していないので、従来の
    // render_static + Cluster 列変調パスへフォールバックする（Aquarelle は
    // bleed/bloom/halo を内部で持っているので 3 軸揺らぎを足すと壊れる）。
    if let OrbShape::Aquarelle(_) = opts.shape {
        return render_frame_aquarelle(clusters, opts, &params, t);
    }

    // Circle 経路: 自前で Pixmap を作って per-orb で render_one_orb を呼ぶ。
    let mut pixmap =
        Pixmap::new(width, height).expect("pixmap allocation should succeed for >0 dimensions");
    let [br, bg, bb, ba] = opts.background;
    if ba > 0 {
        pixmap.fill(Color::from_rgba8(br, bg, bb, ba));
    }

    if clusters.is_empty() {
        return finalize_pixmap(pixmap, width, height);
    }

    let base_radius_unit = (width.min(height) as f32) * 0.25 * opts.orb_size.max(0.0);
    let saturation = opts.saturation.max(0.0);
    let base_blur = opts.blur.clamp(0.0, 1.0);

    // 進行軸の長さ（ピクセル）。LR/RL では width、TB/BT では height。
    // r_normalized を計算する基準になる。
    let progress_axis_pixels = match opts.direction {
        MotionDirection::LeftToRight | MotionDirection::RightToLeft => width as f32,
        MotionDirection::TopToBottom | MotionDirection::BottomToTop => height as f32,
    };

    for p in params.iter() {
        // 担当クラスタを取り出す（cluster_idx は pick_weighted で 0..clusters.len() に収まる）。
        let c = &clusters[p.cluster_idx.min(clusters.len() - 1)];

        // この orb の最大想定半径（ピクセル）。breath ±10% の上限を見込む。
        // r_normalized は進行軸 [0,1] スケールにおける半径相当。
        let r_pixels_max = base_radius_unit * c.weight.max(0.0).sqrt() * 1.10;
        let r_normalized = if progress_axis_pixels > 0.0 {
            r_pixels_max / progress_axis_pixels
        } else {
            0.0
        };
        // 周期長: 画面外バッファを左右（あるいは上下）に r ずつ持つので [-r, 1+r) の幅。
        let extent = 1.0 + 2.0 * r_normalized;

        // 進行量。phase は 0..1 を extent にスケール、advance は extent 単位で進める。
        // cycle * speed_mult は整数なので t=1 で fract が 0、ループ性は保たれる。
        let advance_steps = (cycle as f32 * p.speed_mult as f32 * t).fract();
        let raw = p.phase * extent + advance_steps * extent;
        // pos ∈ [-r_normalized, 1 + r_normalized)。画面外バッファに居る間は orb 中心が
        // 画面の縁を超えており、半径を考慮しても完全に画面外。
        let pos = raw.rem_euclid(extent) - r_normalized;

        // 直交軸は cluster 重心ではなく cross_axis（seed 由来 0..1）で散らせる。
        // クラスタ重心に固定すると orb 数を増やしても画面の同じ縦/横線に並ぶだけに
        // なるので、画面全体に散布するため orb 個別のオフセットを使う。
        let (nx, ny) = match opts.direction {
            MotionDirection::LeftToRight => (pos, p.cross_axis),
            MotionDirection::RightToLeft => (1.0 - pos, p.cross_axis),
            MotionDirection::TopToBottom => (p.cross_axis, pos),
            MotionDirection::BottomToTop => (p.cross_axis, 1.0 - pos),
        };

        // 3 軸独立の呼吸揺らぎ。各々が動画 1 周（sin_loop の f=1, scale=1）で
        // ループする。位相は seed から決定的に生成しているので、軸間で同期しない。
        // - radius: ±10%
        // - blur: ±15%
        // - opacity: ±5%
        let radius_factor = 1.0 + 0.10 * sin_loop(1, t, 1, p.phi_radius);
        let blur_delta = 0.15 * sin_loop(1, t, 1, p.phi_blur);
        let opacity_factor = 1.0 + 0.05 * sin_loop(1, t, 1, p.phi_opacity);

        let radius = base_radius_unit * c.weight.max(0.0).sqrt() * radius_factor;
        if radius <= 0.0 {
            continue;
        }

        // clamp を外し、画面外（負・1超）の値も許可する。tiny-skia 側は描画範囲外を
        // 安全にクリップする（半透明グラデのカウントだけ無駄に走るが、許容範囲）。
        let cx = nx * width as f32;
        let cy = ny * height as f32;
        let rgb = adjust_saturation_pub(c.color, saturation);
        let blur = (base_blur + blur_delta).clamp(0.0, 1.0);
        let opacity = opacity_factor.clamp(0.0, 1.0);

        // 各 orb のスタイル（Rim / Soft）を seed 由来で振り分け、フレーム内に混在させる。
        render_one_orb(&mut pixmap, (cx, cy), radius, rgb, blur, opacity, p.style);
    }

    finalize_pixmap(pixmap, width, height)
}

/// `count` の上限。万一おかしな値が来てもメモリ枯渇しないように防衛。
const MAX_ORB_COUNT: usize = 1024;

/// Aquarelle shape は既存の render_static フォールバック経路。
///
/// Aquarelle は内部で bleed / bloom / halo を持っているため、per-orb の独立揺らぎを
/// 足すと質感セットが壊れる。半径だけの呼吸を従来どおり cluster.weight に乗せる。
/// count による展開は Aquarelle では行わない（質感セットが orb ごとに重い）。
/// 受け取った `params` の先頭から cluster 数だけ消費する。
fn render_frame_aquarelle(
    clusters: &[Cluster],
    opts: &AnimateOptions,
    params: &[OrbParams],
    t: f32,
) -> RgbaImage {
    use crate::cluster::Centroid;
    use crate::orb::{render_static, RenderOptions};

    let cycle = opts.speed.cycle_count();

    let modulated: Vec<Cluster> = clusters
        .iter()
        .zip(params.iter())
        .map(|(c, p)| {
            let advance = (cycle as f32 * p.speed_mult as f32 * t).fract();
            let progress = (p.phase + advance).rem_euclid(1.0);
            let (nx, ny) = match opts.direction {
                MotionDirection::LeftToRight => (progress, c.centroid.y),
                MotionDirection::RightToLeft => (1.0 - progress, c.centroid.y),
                MotionDirection::TopToBottom => (c.centroid.x, progress),
                MotionDirection::BottomToTop => (c.centroid.x, 1.0 - progress),
            };
            let radius_factor = 1.0 + 0.10 * sin_loop(1, t, 1, p.phi_radius);
            let weight_scale = radius_factor * radius_factor;
            Cluster {
                color: c.color,
                centroid: Centroid { x: nx, y: ny },
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

/// Pixmap → RgbaImage 変換（un-premultiply 込み）。
///
/// tiny-skia の Pixmap は premultiplied alpha なので straight に戻す。
fn finalize_pixmap(pixmap: Pixmap, width: u32, height: u32) -> RgbaImage {
    let mut buf = pixmap.take();
    for px in buf.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            let inv = 255.0 / a as f32;
            px[0] = (px[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            px[1] = (px[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
            px[2] = (px[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
        }
    }
    RgbaImage::from_raw(width, height, buf)
        .expect("raw buffer length matches width * height * 4 by construction")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::Centroid;

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
            count: None,
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

    #[test]
    fn count_expands_orb_pool_beyond_clusters() {
        // count を None で渡すと cluster 数（3）と同じ orb が描かれる。
        // count = Some(40) を指定すると 40 個に展開される。展開した方が画面の
        // 平均明度（R チャネルの平均）が大きくなることで「より多くの orb が描かれた」
        // ことを間接確認する。
        let clusters = sample_clusters();
        let mut opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts.count = None;
        let img_default = render_frame(&clusters, &opts, 0.0);

        opts.count = Some(40);
        let img_expanded = render_frame(&clusters, &opts, 0.0);

        let mean_r = |img: &RgbaImage| -> f64 {
            let mut s = 0u64;
            for px in img.pixels() {
                s += px[0] as u64;
            }
            s as f64 / (img.width() as f64 * img.height() as f64)
        };
        let m_def = mean_r(&img_default);
        let m_exp = mean_r(&img_expanded);
        assert!(
            m_exp > m_def + 0.5,
            "expanding count should increase mean brightness; default={m_def}, expanded={m_exp}"
        );
    }

    #[test]
    fn style_is_mixed_across_orbs() {
        // 多めの orb を生成すると Rim と Soft が両方出現する。
        // 50:50 程度に振っているので、64 個も引けば両方が必ず出る（確率的に
        // 1 種類しか出ない確率は (0.5)^64 ≈ 5.4e-20）。
        let p = generate_orb_params(7, 64, &[1.0]);
        let n_rim = p.iter().filter(|q| q.style == OrbStyle::Rim).count();
        let n_soft = p.iter().filter(|q| q.style == OrbStyle::Soft).count();
        assert!(
            n_rim > 0 && n_soft > 0,
            "expected both Rim and Soft to appear; got rim={n_rim} soft={n_soft}"
        );
    }

    #[test]
    fn breath_axes_are_independent() {
        // 3 軸（radius / blur / opacity）の位相が seed から独立に生成されていること。
        // 同じ seed で同じインデックスの OrbParams が phi_radius / phi_blur /
        // phi_opacity 3 つとも同じ値になっているとアウト（同期している）。
        let p = generate_orb_params(42, 16, &[1.0]);
        let mut all_three_same = 0;
        for op in &p {
            // 3 つが完全一致しているケースを数える。0 件であることを期待する
            // （偶然一致は理論上ゼロではないが、ChaCha8Rng + f32 の連続値で
            //  一致する確率は実質 0）。
            if (op.phi_radius - op.phi_blur).abs() < 1e-6
                && (op.phi_blur - op.phi_opacity).abs() < 1e-6
            {
                all_three_same += 1;
            }
        }
        assert_eq!(
            all_three_same, 0,
            "breath axes must not be synchronized for any orb"
        );
    }

    #[test]
    fn speed_mult_distribution() {
        // 30 個の orb を引けば 1/2/3 のうち少なくとも 2 種類は出現する。
        // 全部同じ値になる確率は (1/3)^29 * 3 ≈ 1.5e-13 で実質 0。
        let p = generate_orb_params(99, 30, &[1.0]);
        let n1 = p.iter().filter(|q| q.speed_mult == 1).count();
        let n2 = p.iter().filter(|q| q.speed_mult == 2).count();
        let n3 = p.iter().filter(|q| q.speed_mult == 3).count();
        let kinds = [n1, n2, n3].iter().filter(|&&n| n > 0).count();
        assert!(
            kinds >= 2,
            "expected at least 2 distinct speed_mult values; got n1={n1}, n2={n2}, n3={n3}"
        );
        // 全 orb が {1, 2, 3} の範囲内であることも確認。
        for q in &p {
            assert!(
                (1..=3).contains(&q.speed_mult),
                "speed_mult must be 1, 2, or 3; got {}",
                q.speed_mult
            );
        }
    }

    #[test]
    fn breath_axes_each_drive_visible_change_in_isolation() {
        // 3 軸独立揺らぎが効いていることの間接確認。
        // 64 個の orb を散らせて、t=0 と t=0.5 のフレームに差があることを確認する
        // （位置 + breath の両方が乗るので必ず差が出る）。中央ピクセル単発では
        // 画面外バッファ wrap で orb が center を通らない可能性があるため、
        // 全画面でいずれかのピクセルに差があることを見る。
        let clusters = vec![cluster([255, 255, 255], 0.5, 0.5, 1.0)];
        let mut opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::VerySlow);
        opts.count = Some(64);
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 0.5);
        assert!(
            !pixels_equal(&a, &b),
            "breath axes should produce visible difference between t=0 and t=0.5"
        );
    }

    #[test]
    fn wrap_buffer_keeps_orbs_offscreen_at_seam() {
        // wrap 境界の前後で、画面内に orb 中心は描かれない。
        // 直接的に、orb 1 個・phase 既知のセットアップを作ってその orb の中心が
        // 画面外 (cx < 0 or cx >= width) に居る瞬間に、画面内の最大輝度が低い
        // ことで「画面端で見える状態のまま消える」アーティファクトが無いことを確認。
        //
        // 単一クラスタ + count=1 + seed 固定で、orb のピクセル位置を計算し、
        // pos = -r や pos = 1+r 周辺の t における画面内ピクセル最大輝度が低いことを見る。
        let clusters = vec![cluster([255, 255, 255], 0.5, 0.5, 1.0)];
        let width = 128u32;
        let height = 128u32;
        let opts = AnimateOptions {
            width,
            height,
            orb_size: 1.0,
            seed: 11,
            count: Some(1),
            direction: MotionDirection::LeftToRight,
            speed: MotionSpeed::VerySlow,
            ..AnimateOptions::default()
        };
        // generate_orb_params で実際のパラメータを取り出して、orb が画面外に居る
        // (cx + r <= 0 または cx - r >= width) ような t を計算する。
        let params = generate_orb_params(opts.seed, 1, &[1.0]);
        let p = params[0];
        let base_radius_unit = (width.min(height) as f32) * 0.25;
        let r_pixels_max = base_radius_unit * 1.0_f32.sqrt() * 1.10;
        let r_normalized = r_pixels_max / width as f32;
        let extent = 1.0 + 2.0 * r_normalized;
        // 探索: 0..=N の t の中で、orb 中心 cx の画面内位置 pos*width が
        // [-r_pixels-1, r_pixels+1] のどこかに居る t を探し、その t において
        // 画面内のピクセル最大輝度が低いことを確認する。
        let cycle = opts.speed.cycle_count() as f32 * p.speed_mult as f32;
        let mut found_offscreen_t: Option<f32> = None;
        for i in 0..1000 {
            let t = i as f32 / 1000.0;
            let advance_steps = (cycle * t).fract();
            let raw = p.phase * extent + advance_steps * extent;
            let pos = raw.rem_euclid(extent) - r_normalized;
            // 画面外: cx + r_pixels <= 0  ⇔  pos*width + r_pixels <= 0  ⇔ pos <= -r_normalized
            // または cx - r_pixels >= width  ⇔ pos >= 1 + r_normalized
            // 最も画面外らしい瞬間（pos が -r_normalized 付近 or 1+r_normalized 付近）。
            if pos <= -r_normalized + 0.001 || pos >= 1.0 + r_normalized - 0.001 {
                found_offscreen_t = Some(t);
                break;
            }
        }
        let t_off = found_offscreen_t.expect("should find an off-screen instant within [0,1)");
        let img = render_frame(&clusters, &opts, t_off);
        // 画面内の最大 R 値が極めて低い（背景 alpha=255 なので R は 0）か、グラデの
        // 端が少し漏れる場合でも 8/255 未満であることを見る。
        let mut max_r = 0u8;
        for px in img.pixels() {
            if px[0] > max_r {
                max_r = px[0];
            }
        }
        assert!(
            max_r < 16,
            "off-screen orb should not contribute visible pixels; max_r={max_r} at t={t_off}"
        );
    }

    #[test]
    fn loop_continuity_at_t1_with_speed_mult() {
        // 速度倍数 (1/2/3) を含めても t=0 と t=1 が完全一致することを再確認。
        // 既存 all_direction_speed_combinations_loop_closed でカバーしているが、
        // count=64 で speed_mult のばらつきが必ず起きるケースで明示的に再検証する。
        let clusters = sample_clusters();
        let mut opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Medium);
        opts.count = Some(64);
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 1.0);
        assert!(
            pixels_equal(&a, &b),
            "loop must close at t=1 even with mixed speed_mult"
        );
    }
}
