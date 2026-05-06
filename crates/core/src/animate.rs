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
//! - 移動中は orb ごとに 3 軸独立の位相で半径・blur・opacity が呼吸的に微揺らぎ
//!   （phi_radius / phi_blur / phi_opacity は seed 由来で per-orb / per-axis 独立。
//!   独立モードではなく、常に薄く乗る自動効果）
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
//!   全部整数なので t=1 で fract が 0 になり、ループ性は保たれる。VerySlow /
//!   Slow (cycle_count = 1 / 2) と組み合わせると実効周回数は {1, 2, 3, 4, 6} の
//!   5 段階に分散し、1 動画内のリズムが豊かになる（#53）。
//! - phase の散らばり (0..1 の一様分布) も併用する。phase が違えば、画面上の各
//!   時点の orb 位置が散らばるので「同期して動いていない」感が出る。
//! - 呼吸揺らぎは **3 軸独立**で sin。半径 ±10% / blur ±15% / opacity ±5%、
//!   それぞれ別位相 (phi_radius / phi_blur / phi_opacity)。動画全体で各 1 周。
//! - RNG は [`rand_chacha::ChaCha8Rng`] を `seed` で固定。同じ seed・clusters・
//!   t で 100% 同一フレームが返る。
//! - 描画は [`crate::orb::render_one_orb`] / [`crate::glyph::render_glyph_orb`] を
//!   per-orb で呼ぶ。背景塗りと un-premultiply は animate 側で同等の処理を行う。

use crate::cluster::{Centroid, Cluster};
use crate::color_track::interpolate_color_track;
use crate::keyframe_track::{interpolate_keyframe_track, KeyframeClusterPoint};
use crate::orb::{adjust_saturation_pub, render_one_orb, OrbShape, OrbStyle};
use crate::style::{FalloffProfile, SoftnessPreset};
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

/// 流れの速さ。動画全体（duration）で何回画面を横断するかを整数で表す。
///
/// 整数横断回数にすることで `t=0` と `t=1` のフレームが完全一致する（ループ性）。
/// `cycle_count` は単調増加: VerySlow < Slow < Mid < Fast。各 variant の意味は:
///
/// - VerySlow: 全クリップで 1 回横断（最も穏やか）
/// - Slow: 全クリップで 2 回横断（旧デフォルト、既定）
/// - Mid: 全クリップで 3 回横断（#55 で追加、新デフォルト想定）
/// - Fast: 全クリップで 4 回横断（#55 で追加、リッチ鑑賞用）
///
/// #55 で Mid / Fast を追加。既存の VerySlow / Slow は値変更なし（regression なし）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionSpeed {
    /// 動画全体で画面 1 回横断（最も穏やか）。
    VerySlow,
    /// 動画全体で画面 2 回横断（旧既定）。
    Slow,
    /// 動画全体で画面 3 回横断（#55 で追加）。
    Mid,
    /// 動画全体で画面 4 回横断（#55 で追加）。
    Fast,
}

impl MotionSpeed {
    /// 動画全体での横断回数。整数なので `t=0` と `t=1` で進行量が完全一致する。
    ///
    /// `MotionSpeed` 自体が `pub` なので、外部利用者が enum 各 variant の意味
    /// （何回画面を横断するか）を introspect できるよう `pub` にしている。
    pub fn cycle_count(self) -> u32 {
        match self {
            MotionSpeed::VerySlow => 1,
            MotionSpeed::Slow => 2,
            MotionSpeed::Mid => 3,
            MotionSpeed::Fast => 4,
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
    /// ぼかし (Softness) preset（#55, #131 で改名）。Mid で既存挙動と完全同値。
    pub softness: SoftnessPreset,
    /// Glyph 形状時に per-orb 回転をアニメーションさせるかどうか（#136）。
    /// `true` で従来挙動（base_angle + cycle * rot_speed_signed * t * TAU）。
    /// `false` で全 t において base_angle を保ち、glyph は静止向きで描かれる。
    /// Circle / Aquarelle 経路では使われない。既定 `true` で互換維持。
    pub glyph_rotate: bool,
    /// 動画入力（#7）の per-cluster 色トラック。
    ///
    /// `Some(tracks)` のとき、各 orb の `cluster.color` は
    /// `interpolate_color_track(tracks[cluster_idx], t)` で動的に上書きされる。
    /// `tracks.len()` が clusters の数より少ない場合（理論上ないが防衛）や、
    /// 個別 track が空の場合は `cluster.color` にフォールバックする。
    /// `None` は静止画入力の従来挙動（色固定）。
    pub color_tracks: Option<Vec<Vec<[u8; 3]>>>,
    /// 動画入力（#33）の per-cluster キーフレーム補間トラック。
    ///
    /// `Some(tracks)` のとき、各 orb の `cluster.color` / `cluster.centroid` /
    /// `cluster.weight` は [`crate::keyframe_track::interpolate_keyframe_track`]
    /// で時刻 `t` の補間値に動的に上書きされる。`color_tracks` (#7) と排他で、
    /// 両方 Some の場合は `keyframe_tracks` を優先する（#33 が #7 の上位互換）。
    /// `None` のときは `color_tracks` に従う。
    /// WebGL 経路 (`pack_render_data_for_webgl`) は keyframe_tracks を見ない。
    pub keyframe_tracks: Option<Vec<Vec<crate::keyframe_track::KeyframeClusterPoint>>>,
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
            softness: SoftnessPreset::Mid,
            glyph_rotate: true,
            color_tracks: None,
            keyframe_tracks: None,
        }
    }
}

/// 半径呼吸の上限係数。`render_frame` の `radius_factor` の最大値（1.0 + 0.10）と
/// 一致させる必要がある。`r_pixels_max` の見積りで使うため定数として括り出している。
const BREATH_RADIUS_MAX_FACTOR: f32 = 1.10;

/// 半径呼吸の振幅。`BREATH_RADIUS_MAX_FACTOR = 1.0 + BREATH_RADIUS_AMPLITUDE`。
const BREATH_RADIUS_AMPLITUDE: f32 = 0.10;

/// 各 orb の決定的なパラメータ。
///
/// `phase` は 0..1 の初期位置オフセット。`phi_radius` / `phi_blur` / `phi_opacity` は
/// 3 軸独立の呼吸位相シフト（radius は ±10%、blur は ±15%、opacity は ±5%）。
/// `style` は orb ごとの描画スタイル（Rim / Soft）で、フレーム内に混在させる。
/// `cluster_idx` はこの orb の色とサイズを取ってくる元クラスタの index（重み比例で抽選）。
/// `cross_axis` は進行方向と直交する軸の正規化座標 0..1。orb をクラスタ重心に固定
/// せず、画面全体に散らせるためのオフセット。
/// `speed_mult` は整数倍速度 (1 / 2 / 3)。`cycle_count * speed_mult` も整数
/// なので `t=1` でループが閉じる。視覚的なバラつきの主因（#53 で 2 → 3 段階）。
/// `base_angle` / `rot_speed_signed` は glyph 用の回転で、`cycle_count` と同じ
/// 整数周期に乗せることで `t=0 ≡ t=1` を保つ。
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
    /// glyph がある場合の初期回転角 [0, 2π)。
    base_angle: f32,
    /// glyph の signed 回転速度。±{1, 2, 3} で、絶対値は speed_mult と一致する。
    rot_speed_signed: f32,
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
    debug_assert!(
        !weights.is_empty(),
        "pick_weighted assumes non-empty weights after early return"
    );
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
        .map(|i| {
            let phase = rng.gen_range(0.0..1.0);
            let phi_radius = rng.gen_range(0.0..TAU);
            let phi_blur = rng.gen_range(0.0..TAU);
            let phi_opacity = rng.gen_range(0.0..TAU);
            // cross_axis は完全独立の一様分布で散らす。cluster centroid をそのまま
            // 使うと同色 orb が同じ縦軸 / 横軸に並んで縞模様になるため、画面全体に
            // 散布する目的でクラスタ重心とは無関係なオフセットを使う。
            let cross_axis = rng.gen_range(0.0..1.0);
            let style = if rng.gen::<u32>() & 1 == 0 {
                OrbStyle::Rim
            } else {
                OrbStyle::Soft
            };
            let cluster_idx = pick_weighted(&mut rng, cluster_weights, total_w);
            // 整数倍速度。1/2 を均等に割り当て。整数 × 整数の cycle_count なので
            // t=1 で fract が 0 になりループ性は保たれる。
            // #53: 1..=2 → 1..=3 に拡張。1 画像内に「ゆっくり / 普通 / 速い」が
            // 混在する。cycle_count {1, 2} × speed_mult {1, 2, 3} で実効周回は
            // {1, 2, 3, 4, 6} の 5 段階。全部整数なのでループ性は保たれる。
            let speed_mult = rng.gen_range(1..=3);
            let rot_hash = splitmix64(seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let base_angle = unit_from_hash(rot_hash) * TAU;
            let rot_dir = if splitmix64(rot_hash ^ 0xD1B5_4A32_D192_ED03) & 1 == 0 {
                1.0
            } else {
                -1.0
            };
            OrbParams {
                phase,
                phi_radius,
                phi_blur,
                phi_opacity,
                style,
                cluster_idx,
                cross_axis,
                speed_mult,
                base_angle,
                rot_speed_signed: speed_mult as f32 * rot_dir,
            }
        })
        .collect()
}

#[inline]
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[inline]
fn unit_from_hash(x: u64) -> f32 {
    let bits = (x >> 40) as u32;
    bits as f32 / ((1u32 << 24) as f32)
}

/// WebGL / wasm 向けに per-orb render data を詰めた Float32 words を返す。
///
/// `orber-wasm` が shape / softness / rotation を CPU 経路と同じ決定論性で
/// WebGL へ渡すための purpose-built helper。内部 RNG 列や `OrbParams` の
/// レイアウトは公開しない。
///
/// `glyph_rotate` (#136): `false` を渡すと shader 側で per-orb 回転を抑止し、
/// 全 t で `base_angle` のまま描く。Circle 経路には影響しない。
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn pack_render_data_for_webgl(
    clusters: &[Cluster],
    bg: [u8; 4],
    base_radius_unit: f32,
    base_blur: f32,
    direction_id: f32,
    cycle: f32,
    seed: u64,
    n_orbs: usize,
    alpha_mul: f32,
    shape_id: f32,
    glyph_rotate: bool,
) -> Vec<f32> {
    let header_words = 16usize;
    let per_orb_words = 16usize;
    let mut buf = vec![0.0f32; header_words + per_orb_words * n_orbs];

    buf[0] = bg[0] as f32 / 255.0;
    buf[1] = bg[1] as f32 / 255.0;
    buf[2] = bg[2] as f32 / 255.0;
    buf[3] = bg[3] as f32 / 255.0;
    buf[4] = base_radius_unit;
    buf[5] = base_blur;
    buf[6] = direction_id;
    buf[7] = cycle;
    buf[8] = n_orbs as f32;
    buf[9] = alpha_mul;
    buf[10] = shape_id;
    // #136: header[11] = glyph_rotate (1.0 = ON / 既定, 0.0 = OFF)。既存ヘッダ予約域に追加。
    buf[11] = if glyph_rotate { 1.0 } else { 0.0 };

    if n_orbs == 0 || clusters.is_empty() {
        return buf;
    }

    let cluster_weights: Vec<f32> = clusters.iter().map(|c| c.weight.max(0.0)).collect();
    let params = generate_orb_params(seed, n_orbs, &cluster_weights);
    for (i, p) in params.iter().enumerate() {
        let c = &clusters[p.cluster_idx.min(clusters.len() - 1)];
        let off = header_words + per_orb_words * i;
        buf[off] = c.color[0] as f32 / 255.0;
        buf[off + 1] = c.color[1] as f32 / 255.0;
        buf[off + 2] = c.color[2] as f32 / 255.0;
        buf[off + 3] = c.weight.max(0.0);
        buf[off + 4] = p.phase;
        buf[off + 5] = p.phi_radius;
        buf[off + 6] = p.phi_blur;
        buf[off + 7] = p.phi_opacity;
        buf[off + 8] = p.cross_axis;
        buf[off + 9] = if p.style == OrbStyle::Rim { 0.0 } else { 1.0 };
        buf[off + 10] = p.speed_mult as f32;
        buf[off + 11] = p.base_angle;
        buf[off + 12] = p.rot_speed_signed;
    }
    buf
}

/// 動画入力（#7）: `color_tracks` が指定されているときは `tracks[cluster_idx]` を
/// `t` で線形補間した色を返す。指定が無い・index 範囲外・track が空のときは
/// `fallback`（cluster.color）にフォールバックする。
///
/// 補間そのものは [`crate::color_track::interpolate_color_track`] が担い、
/// この関数は「track が無い場合の素通し」を担当するだけの薄いラッパー。
#[inline]
fn pick_color_at_t(
    tracks: Option<&[Vec<[u8; 3]>]>,
    cluster_idx: usize,
    fallback: [u8; 3],
    t: f32,
) -> [u8; 3] {
    let Some(tracks) = tracks else {
        return fallback;
    };
    let Some(track) = tracks.get(cluster_idx) else {
        return fallback;
    };
    if track.is_empty() {
        return fallback;
    }
    interpolate_color_track(track, t)
}

/// 動画入力（#33）: `keyframe_tracks` が指定されているときは色 + 位置 + 重みを
/// 全部時刻 `t` で補間して返す。指定が無い・index 範囲外・track が空のときは
/// 静止画の `fallback` cluster を素通しする。
///
/// `keyframe_tracks` と `color_tracks` (#7) を両方持つ `AnimateOptions` でも、
/// この関数は keyframe_tracks のみを参照する（呼び出し側で優先順位を決める）。
#[inline]
fn pick_cluster_at_t(
    keyframe_tracks: Option<&[Vec<KeyframeClusterPoint>]>,
    cluster_idx: usize,
    fallback: &Cluster,
    t: f32,
) -> Option<([u8; 3], Centroid, f32)> {
    let tracks = keyframe_tracks?;
    let track = tracks.get(cluster_idx)?;
    if track.is_empty() {
        return None;
    }
    let _ = fallback; // 現状はフォールバック値を使わず None を返して呼び出し側で素通しさせる。
    Some(interpolate_keyframe_track(track, t))
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

#[inline]
fn glyph_rotation_angle(
    cycle: u32,
    t: f32,
    base_angle: f32,
    rot_speed_signed: f32,
    glyph_rotate: bool,
) -> f32 {
    // #136: glyph_rotate=false なら全 t で base_angle のまま静止する。
    // OFF でも t=0 と t=1 が一致する（loop closure は自明）。
    if !glyph_rotate {
        return base_angle;
    }
    let turns = (cycle as f32 * rot_speed_signed * t).rem_euclid(1.0);
    base_angle + turns * TAU
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
/// 同じ seed と同じ clusters なら出力は完全一致する。RNG は orb index 順に固定
/// シーケンスで消費されるため、count や seed が変わると各 orb の phase /
/// phi_radius / phi_blur / phi_opacity / cross_axis / style / cluster_idx /
/// speed_mult 割当が同時に変わる。
pub fn render_frame(clusters: &[Cluster], opts: &AnimateOptions, t: f32) -> RgbaImage {
    let params = precompute_orb_params(opts, clusters);
    render_frame_with_params(clusters, opts, &params, t)
}

/// `render_frame` で使う per-orb パラメータをまとめてプリコンピュートしたもの。
///
/// 連続するフレームで同じ `seed` / `count` / `clusters` を使う場合（典型的には
/// 動画書き出し）、これを 1 回計算してフレームループで使い回すことで
/// `Vec<OrbParams>` 割当と RNG 走行のコストを排除できる。
///
/// `seed` / `count` / `cluster_weights` のいずれかを変える場合は再計算する必要がある
/// （`Default` / `Copy` を実装しないのは、その不変条件をうっかり壊すのを防ぐため）。
#[derive(Debug, Clone)]
pub struct CachedOrbParams {
    params: Vec<OrbParams>,
}

/// `opts.seed` / `opts.count` / `clusters.weight` から決定的な orb パラメータ列を生成する。
///
/// 動画書き出しのフレームループで使い回す前提のキャッシュ。`render_frame` の中で
/// 暗黙に毎フレーム呼ばれていたものを、呼び出し側で 1 回呼んで保持できるよう
/// 公開した。
pub fn precompute_orb_params(opts: &AnimateOptions, clusters: &[Cluster]) -> CachedOrbParams {
    let n_orbs = opts
        .count
        .unwrap_or(clusters.len())
        .min(MAX_ORB_COUNT)
        .max(if clusters.is_empty() { 0 } else { 1 });
    let cluster_weights: Vec<f32> = clusters.iter().map(|c| c.weight.max(0.0)).collect();
    CachedOrbParams {
        params: generate_orb_params(opts.seed, n_orbs, &cluster_weights),
    }
}

/// プリコンピュート済みの `CachedOrbParams` を使って 1 フレーム描画する。
///
/// `precompute_orb_params(opts, clusters)` で得た cache を渡すこと。
/// `clusters` / `opts` （seed / count / 解像度等）が cache 計算時と一致していないと
/// レンダリング結果は不定（パニックはしないが意味のある画像にならない）。
pub fn render_frame_with_params(
    clusters: &[Cluster],
    opts: &AnimateOptions,
    cache: &CachedOrbParams,
    t: f32,
) -> RgbaImage {
    let cycle = opts.speed.cycle_count();
    let params = &cache.params;

    let width = opts.width.max(1);
    let height = opts.height.max(1);

    // Aquarelle 経路は per-orb の独立揺らぎに対応していないので、従来の
    // render_static + Cluster 列変調パスへフォールバックする（Aquarelle は
    // bleed/bloom/halo を内部で持っているので 3 軸揺らぎを足すと壊れる）。
    if let OrbShape::Aquarelle(_) = opts.shape {
        return render_frame_aquarelle(clusters, opts, params, t);
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
    // softness 軸: blur は事前に offset を加算、alpha は per-orb opacity_factor に乗じる。
    // Mid なら blur_offset=0, alpha_mul=1.0 で既存挙動と完全同値。
    let base_blur = (opts.blur + opts.softness.blur_offset()).clamp(0.0, 1.0);
    let softness_alpha_mul = opts.softness.alpha_mul().clamp(0.0, 1.0);

    // 進行軸の長さ（ピクセル）。LR/RL では width、TB/BT では height。
    // r_normalized を計算する基準になる。
    let progress_axis_pixels = match opts.direction {
        MotionDirection::LeftToRight | MotionDirection::RightToLeft => width as f32,
        MotionDirection::TopToBottom | MotionDirection::BottomToTop => height as f32,
    };

    for p in params.iter() {
        // 担当クラスタを取り出す（cluster_idx は pick_weighted で 0..clusters.len() に収まる）。
        let cluster_idx = p.cluster_idx.min(clusters.len() - 1);
        let static_c = &clusters[cluster_idx];

        // 動画入力（#33）: keyframe_tracks が指定されているときは色 + 位置 + 重みを
        // 全部時刻 t の補間値に置き換える。これがない場合は静止画クラスタを使う。
        // 動画入力（#7）: keyframe_tracks が無く、color_tracks があるときは色だけ
        // 上書き。位置・サイズ・揺らぎは変えない。
        let interpolated =
            pick_cluster_at_t(opts.keyframe_tracks.as_deref(), cluster_idx, static_c, t);
        let (color_static, centroid_used, weight_used) = match interpolated {
            Some((col, cen, w)) => (col, cen, w),
            None => (static_c.color, static_c.centroid, static_c.weight),
        };
        // #33 review M1: keyframe_tracks ありのときだけ centroid を反映。
        // cross_axis (seed 由来 RNG) と centroid 補間値を 50:50 でブレンドして
        // 入力動画のコンポジショナルな動きを視覚化する。none のとき (#7 / 静止画) は
        // 完全に既存挙動を保つ（縞模様回避維持）。
        let cross_axis_used = if opts.keyframe_tracks.is_some() {
            let centroid_axis = match opts.direction {
                MotionDirection::LeftToRight | MotionDirection::RightToLeft => centroid_used.y,
                MotionDirection::TopToBottom | MotionDirection::BottomToTop => centroid_used.x,
            };
            p.cross_axis * 0.5 + centroid_axis * 0.5
        } else {
            p.cross_axis
        };
        let color_at_t = if opts.keyframe_tracks.is_some() {
            color_static
        } else {
            pick_color_at_t(opts.color_tracks.as_deref(), cluster_idx, static_c.color, t)
        };

        // この orb の最大想定半径（ピクセル）。breath ±10% の上限を見込む。
        // r_normalized は進行軸 [0,1] スケールにおける半径相当。
        let r_pixels_max = base_radius_unit * weight_used.max(0.0).sqrt() * BREATH_RADIUS_MAX_FACTOR;
        let r_normalized = if progress_axis_pixels > 0.0 {
            r_pixels_max / progress_axis_pixels
        } else {
            0.0
        };
        // 周期長: 画面外バッファを左右（あるいは上下）に r ずつ持つので [-r, 1+r) の幅。
        // すなわち extent = 1 + 2r、pos ∈ [-r, 1+r) という対応関係。
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
            MotionDirection::LeftToRight => (pos, cross_axis_used),
            MotionDirection::RightToLeft => (1.0 - pos, cross_axis_used),
            MotionDirection::TopToBottom => (cross_axis_used, pos),
            MotionDirection::BottomToTop => (cross_axis_used, 1.0 - pos),
        };

        // 3 軸独立の呼吸揺らぎ。各々が動画 1 周（sin_loop の f=1, scale=1）で
        // ループする。位相は seed から決定的に生成しているので、軸間で同期しない。
        // - radius: ±10%
        // - blur: ±15%
        // - opacity: ±5%
        let radius_factor = 1.0 + 0.10 * sin_loop(1, t, 1, p.phi_radius);
        let blur_delta = 0.15 * sin_loop(1, t, 1, p.phi_blur);
        let opacity_factor = 1.0 + 0.05 * sin_loop(1, t, 1, p.phi_opacity);

        let radius = base_radius_unit * weight_used.max(0.0).sqrt() * radius_factor;
        if radius <= 0.0 {
            continue;
        }

        // clamp を外し、画面外（負・1超）の値も許可する。tiny-skia 側は描画範囲外を
        // 安全にクリップする（半透明グラデのカウントだけ無駄に走るが、許容範囲）。
        let cx = nx * width as f32;
        let cy = ny * height as f32;
        let rgb = adjust_saturation_pub(color_at_t, saturation);
        let blur = (base_blur + blur_delta).clamp(0.0, 1.0);
        // softness の alpha 倍率を per-orb の opacity_factor に積算（Mid なら ×1.0 で同値）。
        let opacity = (opacity_factor * softness_alpha_mul).clamp(0.0, 1.0);

        // shape による分岐:
        // - Glyph: 1 文字の SDF を回転サンプリングし、blur + Rim/Soft falloff を共有
        // - それ以外（Circle）: per-orb の Rim / Soft スタイルで render_one_orb
        match opts.shape {
            OrbShape::Glyph { ch, font } => {
                // Glyph でも style / blur は Circle と同じ falloff カーブに流し込む。
                // RNG の `style` 引きは Circle/Glyph 切替時の seed 列互換にも使われる。
                crate::glyph::render_glyph_orb(
                    &mut pixmap,
                    (cx, cy),
                    radius,
                    rgb,
                    blur,
                    opacity,
                    match p.style {
                        OrbStyle::Rim => FalloffProfile::Rim,
                        OrbStyle::Soft => FalloffProfile::Soft,
                    },
                    font,
                    ch,
                    glyph_rotation_angle(
                        cycle,
                        t,
                        p.base_angle,
                        p.rot_speed_signed,
                        opts.glyph_rotate,
                    ),
                );
            }
            _ => {
                render_one_orb(&mut pixmap, (cx, cy), radius, rgb, blur, opacity, p.style);
            }
        }
    }

    finalize_pixmap(pixmap, width, height)
}

/// `count` の上限。万一おかしな値が来てもメモリ枯渇しないように防衛。
const MAX_ORB_COUNT: usize = 1024;

/// Aquarelle shape は既存の render_static フォールバック経路。
///
/// 動画 1 タイル分のフレームを 1 枚ずつ生成する反復子。
///
/// `precompute_orb_params` を 1 回だけ走らせ、`next_frame()` 呼び出しごとに
/// `t = i / total_frames` (i = 0..total_frames) の RGBA フレームを返す。
/// 完了後は `None`。`total_frames` の倍数では `t = 1` を出さない設計なので、
/// `t = 0` と「最後のフレームの次」がピクセル一致する `<video loop>` 用途に
/// そのまま使える（README の "loop closure at t=0 ≡ t=1" を維持）。
///
/// `clusters` と `opts` を所有する: 呼び出し側のライフタイムに縛られず JS /
/// wasm-bindgen 経由で Cursor をハンドルとして持ち回せる設計。
pub struct AnimationCursor {
    clusters: Vec<Cluster>,
    opts: AnimateOptions,
    cache: CachedOrbParams,
    total_frames: u32,
    next_idx: u32,
}

impl AnimationCursor {
    /// `clusters` / `opts` / `total_frames` からカーソルを構築する。
    ///
    /// # Panics
    ///
    /// `total_frames == 0` で panic する（ループ閉鎖の不変条件 `t = i / N`
    /// で `N > 0` を要求するため）。WASM ラッパーは早期に Result でエラーを
    /// 返すので、ここに 0 が来るのは内部バグ扱い。
    pub fn new(clusters: Vec<Cluster>, opts: AnimateOptions, total_frames: u32) -> Self {
        assert!(
            total_frames > 0,
            "AnimationCursor requires total_frames > 0"
        );
        let cache = precompute_orb_params(&opts, &clusters);
        Self {
            clusters,
            opts,
            cache,
            total_frames,
            next_idx: 0,
        }
    }

    /// 次のフレームを 1 枚返す。すべてのフレームを返し終えたら `None`。
    pub fn next_frame(&mut self) -> Option<RgbaImage> {
        if self.next_idx >= self.total_frames {
            return None;
        }
        let t = self.next_idx as f32 / self.total_frames as f32;
        let frame = render_frame_with_params(&self.clusters, &self.opts, &self.cache, t);
        self.next_idx += 1;
        Some(frame)
    }

    pub fn total_frames(&self) -> u32 {
        self.total_frames
    }
    pub fn next_index(&self) -> u32 {
        self.next_idx
    }
    pub fn width(&self) -> u32 {
        self.opts.width
    }
    pub fn height(&self) -> u32 {
        self.opts.height
    }
}

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
        .enumerate()
        .map(|(idx, (c, p))| {
            // 動画入力（#33）: keyframe_tracks があれば色 + 重心 + 重みを時刻 t の
            // 補間値で読み替える。#7 (color_tracks) より優先される。
            let interpolated =
                pick_cluster_at_t(opts.keyframe_tracks.as_deref(), idx, c, t);
            let (color_t33, centroid_t33, weight_t33) = match interpolated {
                Some((col, cen, w)) => (col, cen, w),
                None => (c.color, c.centroid, c.weight),
            };
            let advance = (cycle as f32 * p.speed_mult as f32 * t).fract();
            let progress = (p.phase + advance).rem_euclid(1.0);
            let (nx, ny) = match opts.direction {
                MotionDirection::LeftToRight => (progress, centroid_t33.y),
                MotionDirection::RightToLeft => (1.0 - progress, centroid_t33.y),
                MotionDirection::TopToBottom => (centroid_t33.x, progress),
                MotionDirection::BottomToTop => (centroid_t33.x, 1.0 - progress),
            };
            let radius_factor = 1.0 + BREATH_RADIUS_AMPLITUDE * sin_loop(1, t, 1, p.phi_radius);
            let weight_scale = radius_factor * radius_factor;
            // #33 が無いときだけ #7 の color_tracks を見る（#33 が指定済みなら色は補間値）。
            let color_at_t = if opts.keyframe_tracks.is_some() {
                color_t33
            } else {
                pick_color_at_t(opts.color_tracks.as_deref(), idx, c.color, t)
            };
            Cluster {
                color: color_at_t,
                centroid: Centroid { x: nx, y: ny },
                weight: (weight_t33 * weight_scale).max(0.0),
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
        softness: opts.softness,
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
            softness: SoftnessPreset::Mid,
            glyph_rotate: true,
            color_tracks: None,
            keyframe_tracks: None,
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
                MotionSpeed::Mid,
                MotionSpeed::Fast,
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
            MotionSpeed::Mid,
            MotionSpeed::Fast,
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
    fn cycle_count_matches_speed() {
        // 速度の cycle_count が仕様通りであることを保証する回帰テスト。
        assert_eq!(MotionSpeed::VerySlow.cycle_count(), 1);
        assert_eq!(MotionSpeed::Slow.cycle_count(), 2);
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
        // 50:50 程度に振っているので、64 サンプルなら両方とも 20..=44 の範囲に
        // 収まることをチェック（理論期待値 32、片側に大きく寄ると分布が壊れている
        // サインなので緩めに 20..=44 で監視）。
        let p = generate_orb_params(7, 64, &[1.0]);
        let n_rim = p.iter().filter(|q| q.style == OrbStyle::Rim).count();
        let n_soft = p.iter().filter(|q| q.style == OrbStyle::Soft).count();
        assert!(
            (20..=44).contains(&n_rim),
            "Rim count out of expected band 20..=44; got rim={n_rim} soft={n_soft}"
        );
        assert!(
            (20..=44).contains(&n_soft),
            "Soft count out of expected band 20..=44; got rim={n_rim} soft={n_soft}"
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
        // #53: 1..=3 の 3 段階。120 サンプルで各値が均等近くに散らばること。
        // 理論期待値 40 (1/3 ずつ)。片側極端な偏りは分布バグのサイン。
        // ±15 のバンドで 25..=55 を許容。
        let p = generate_orb_params(99, 120, &[1.0]);
        let n1 = p.iter().filter(|q| q.speed_mult == 1).count();
        let n2 = p.iter().filter(|q| q.speed_mult == 2).count();
        let n3 = p.iter().filter(|q| q.speed_mult == 3).count();
        for (label, n) in [("1x", n1), ("2x", n2), ("3x", n3)] {
            assert!(
                (25..=55).contains(&n),
                "speed_mult={label} count out of expected band 25..=55; got n1={n1} n2={n2} n3={n3}"
            );
        }
        // 全 orb が {1, 2, 3} の範囲内であること。
        for q in &p {
            assert!(
                (1..=3).contains(&q.speed_mult),
                "speed_mult must be 1, 2, or 3; got {}",
                q.speed_mult
            );
        }
    }

    #[test]
    fn glyph_rotation_speed_correlates_with_translation_speed() {
        let p = generate_orb_params(99, 120, &[1.0]);
        for q in &p {
            assert!(
                (q.rot_speed_signed.abs() - q.speed_mult as f32).abs() < 1e-6,
                "glyph rotation speed must match speed_mult: rot={} speed_mult={}",
                q.rot_speed_signed,
                q.speed_mult
            );
        }
    }

    #[test]
    fn glyph_rotation_direction_is_mixed() {
        let p = generate_orb_params(7, 64, &[1.0]);
        let cw = p.iter().filter(|q| q.rot_speed_signed > 0.0).count();
        let ccw = p.iter().filter(|q| q.rot_speed_signed < 0.0).count();
        assert!((20..=44).contains(&cw), "clockwise count out of band: cw={cw} ccw={ccw}");
        assert!((20..=44).contains(&ccw), "counter-clockwise count out of band: cw={cw} ccw={ccw}");
    }

    #[test]
    fn glyph_rotation_loop_closure_at_t_one() {
        let p = generate_orb_params(42, 16, &[1.0]);
        for cycle in 1..=4 {
            for q in &p {
                let a0 =
                    glyph_rotation_angle(cycle, 0.0, q.base_angle, q.rot_speed_signed, true);
                let a1 =
                    glyph_rotation_angle(cycle, 1.0, q.base_angle, q.rot_speed_signed, true);
                let delta = (a1 - a0).rem_euclid(TAU);
                assert!(
                    delta.abs() < 1e-5 || (TAU - delta).abs() < 1e-5,
                    "rotation must close at t=1: cycle={cycle} base={} rot={} delta={delta}",
                    q.base_angle,
                    q.rot_speed_signed
                );
            }
        }
    }

    #[test]
    fn glyph_rotation_off_keeps_base_angle() {
        // #136: glyph_rotate=false なら全 t で角度は base_angle のまま不変。
        // OFF パスでも t=0 と t=1 が一致する（loop closure は自明）。
        let p = generate_orb_params(42, 16, &[1.0]);
        for cycle in 1..=4 {
            for q in &p {
                for &t in &[0.0_f32, 0.13, 0.25, 0.5, 0.77, 1.0] {
                    let a = glyph_rotation_angle(
                        cycle,
                        t,
                        q.base_angle,
                        q.rot_speed_signed,
                        false,
                    );
                    assert!(
                        (a - q.base_angle).abs() < 1e-6,
                        "glyph_rotate=false must hold base_angle: cycle={cycle} t={t} base={} got={a}",
                        q.base_angle,
                    );
                }
            }
        }
    }

    #[test]
    fn glyph_rotation_does_not_perturb_legacy_rng_sequence() {
        let seed = 42;
        let weights = [0.7, 0.3];
        let params = generate_orb_params(seed, 8, &weights);
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let total_w: f32 = weights.iter().sum();

        for p in &params {
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
            let cluster_idx = pick_weighted(&mut rng, &weights, total_w);
            let speed_mult = rng.gen_range(1..=3);

            assert!((p.phase - phase).abs() < 1e-6);
            assert!((p.phi_radius - phi_radius).abs() < 1e-6);
            assert!((p.phi_blur - phi_blur).abs() < 1e-6);
            assert!((p.phi_opacity - phi_opacity).abs() < 1e-6);
            assert!((p.cross_axis - cross_axis).abs() < 1e-6);
            assert_eq!(p.style, style);
            assert_eq!(p.cluster_idx, cluster_idx);
            assert_eq!(p.speed_mult, speed_mult);
        }
    }

    #[test]
    fn breath_phases_are_seeded_per_orb_and_per_axis() {
        // breath 機能の検証は OrbParams の位相分布だけでやる。
        // - 同じ orb 内では radius / blur / opacity の 3 軸が異なる位相
        // - orb 間でも同じ軸の位相が散らばっている（どれか 1 軸でも全 orb 一致は許さない）
        //
        // 旧 breath テストはどちらも LeftToRight + t!=0 で「pixels_equal にならない」
        // ことを主張していたが、orb は LR 移動するので breath を OFF にしても
        // 通ってしまう（=何も検証していない）。位相分布チェックに置き換える。
        let p = generate_orb_params(42, 16, &[1.0]);
        // 3 軸が同一位相の orb が 1 つでもあったらアウト。
        for op in &p {
            assert!(
                (op.phi_radius - op.phi_blur).abs() > 1e-6
                    || (op.phi_blur - op.phi_opacity).abs() > 1e-6,
                "breath axes must not all share the same phase: phi_radius={} phi_blur={} phi_opacity={}",
                op.phi_radius,
                op.phi_blur,
                op.phi_opacity
            );
        }
        // orb 間でも各軸の位相が散らばっていること（最小値と最大値が十分離れている）。
        // f32 の位相が完全一致する確率は実質 0 だが、念のため幅を見る。
        let mut min_r = f32::INFINITY;
        let mut max_r = f32::NEG_INFINITY;
        let mut min_b = f32::INFINITY;
        let mut max_b = f32::NEG_INFINITY;
        let mut min_o = f32::INFINITY;
        let mut max_o = f32::NEG_INFINITY;
        for op in &p {
            min_r = min_r.min(op.phi_radius);
            max_r = max_r.max(op.phi_radius);
            min_b = min_b.min(op.phi_blur);
            max_b = max_b.max(op.phi_blur);
            min_o = min_o.min(op.phi_opacity);
            max_o = max_o.max(op.phi_opacity);
        }
        // TAU ≈ 6.28 のうち、16 サンプルあれば spread が 1.0 以上は固い。
        assert!(
            max_r - min_r > 1.0,
            "phi_radius spread too narrow ({} .. {})",
            min_r,
            max_r
        );
        assert!(
            max_b - min_b > 1.0,
            "phi_blur spread too narrow ({} .. {})",
            min_b,
            max_b
        );
        assert!(
            max_o - min_o > 1.0,
            "phi_opacity spread too narrow ({} .. {})",
            min_o,
            max_o
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
        let r_pixels_max = base_radius_unit * 1.0_f32.sqrt() * BREATH_RADIUS_MAX_FACTOR;
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
    fn animation_cursor_yields_n_frames_then_none() {
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        let mut cursor = AnimationCursor::new(sample_clusters(), opts, 4);
        assert_eq!(cursor.total_frames(), 4);
        for expected_idx in 0..4 {
            assert_eq!(cursor.next_index(), expected_idx);
            assert!(
                cursor.next_frame().is_some(),
                "frame {expected_idx} missing"
            );
        }
        assert!(
            cursor.next_frame().is_none(),
            "exhausted cursor must return None"
        );
    }

    #[test]
    fn animation_cursor_first_frame_matches_render_frame_at_zero() {
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        let clusters = sample_clusters();
        let mut cursor = AnimationCursor::new(clusters.clone(), opts.clone(), 24);
        let cursor_frame = cursor.next_frame().expect("first frame");
        let direct = render_frame(&clusters, &opts, 0.0);
        assert!(
            pixels_equal(&cursor_frame, &direct),
            "AnimationCursor first frame must equal render_frame(t=0)"
        );
    }

    #[test]
    fn animation_cursor_does_not_emit_t_one() {
        // ループ閉鎖の不変条件: cursor は t = i/N (i = 0..N) のみ出すので
        // 最後のフレームは i = N-1。i = N のフレーム（= t = 1）は出さず、
        // <video loop> の次のループ頭 (= 改めて render_frame(.., 0.0)) と
        // ピクセル一致する。逆に、最後の next_frame() が render_frame(.., 1.0)
        // と一致しないことで「t=1 を出していない」ことが確認できる
        // （t=0 と t=1 が一致するという別不変条件を逆手に使うと、
        // 最後フレーム == render_frame(t=0) と一致してはいけない）。
        let n = 8;
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        let clusters = sample_clusters();
        let mut cursor = AnimationCursor::new(clusters.clone(), opts.clone(), n);
        let mut last = None;
        for _ in 0..n {
            last = Some(cursor.next_frame().unwrap());
        }
        let t_zero = render_frame(&clusters, &opts, 0.0);
        assert!(
            !pixels_equal(&last.unwrap(), &t_zero),
            "last frame (t=(N-1)/N) must NOT equal render_frame(t=0); otherwise the cursor is emitting t=1"
        );
    }

    #[test]
    #[should_panic(expected = "total_frames > 0")]
    fn animation_cursor_panics_on_zero_frames() {
        let opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        let _ = AnimationCursor::new(sample_clusters(), opts, 0);
    }

    #[test]
    fn loop_continuity_at_t1_with_speed_mult() {
        // 速度倍数 (1/2) を含めても t=0 と t=1 が完全一致することを再確認。
        // 既存 all_direction_speed_combinations_loop_closed でカバーしているが、
        // count=64 で speed_mult のばらつきが必ず起きるケースで明示的に再検証する。
        let clusters = sample_clusters();
        let mut opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts.count = Some(64);
        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 1.0);
        assert!(
            pixels_equal(&a, &b),
            "loop must close at t=1 even with mixed speed_mult"
        );
    }

    // ---- #33 review S1: keyframe_tracks E2E 統合テスト ----
    //
    // ここから 4 件は keyframe_tracks 経路の入口〜出口（render_frame_with_params）まで
    // を通して回し、(a) Aquarelle / Circle で centroid drift が視覚反映されること、
    // (b) 決定論性、(c) keyframe_tracks が color_tracks より優先されること、を検証する。

    /// 1 cluster ぶんの keyframe トラックを (k0=左上 / k1=右下、色も大きく異なる) で
    /// 構築するヘルパ。M1/S1 の centroid drift 検証はこの「t=0 と t=1 で位置・色が
    /// 大幅に異なる」性質に依存する。
    fn drift_keyframe_tracks() -> Vec<Vec<KeyframeClusterPoint>> {
        vec![vec![
            KeyframeClusterPoint {
                color: [240, 40, 40],
                centroid: Centroid { x: 0.15, y: 0.15 },
                weight: 1.0,
                time: 0.0,
            },
            KeyframeClusterPoint {
                color: [40, 80, 240],
                centroid: Centroid { x: 0.85, y: 0.85 },
                weight: 1.0,
                time: 1.0,
            },
        ]]
    }

    /// drift_keyframe_tracks() に対応する static cluster（先頭キーと同じ位置・色）。
    fn drift_static_clusters() -> Vec<Cluster> {
        vec![cluster([240, 40, 40], 0.15, 0.15, 1.0)]
    }

    /// 中央列 (x = width/2) の RGB 行ベクトルを取り出す。Aquarelle の centroid drift
    /// 検出に使う（左上→右下のドリフトでは中央列上の輝度分布が t によって変わる）。
    fn middle_column_bytes(img: &RgbaImage) -> Vec<u8> {
        let mid_x = img.width() / 2;
        let mut out = Vec::with_capacity(img.height() as usize * 3);
        for y in 0..img.height() {
            let p = img.get_pixel(mid_x, y);
            out.push(p[0]);
            out.push(p[1]);
            out.push(p[2]);
        }
        out
    }

    fn pixel_diff_count(a: &RgbaImage, b: &RgbaImage) -> usize {
        assert_eq!(a.dimensions(), b.dimensions());
        a.as_raw()
            .chunks_exact(4)
            .zip(b.as_raw().chunks_exact(4))
            .filter(|(pa, pb)| pa != pb)
            .count()
    }

    #[test]
    fn keyframe_tracks_render_centroid_drift_aquarelle() {
        // Aquarelle 経路は cluster.centroid を直接位置に使う。keyframe_tracks で
        // centroid が大きく移動するキー列を渡すと t=0 と t=1 で中央列 RGB が
        // 異なるはず（左上→右下の対角ドリフト）。
        let clusters = drift_static_clusters();
        let mut opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts.shape = OrbShape::Aquarelle(crate::aquarelle::AquarelleParams::default());
        opts.keyframe_tracks = Some(drift_keyframe_tracks());

        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 1.0);
        let col_a = middle_column_bytes(&a);
        let col_b = middle_column_bytes(&b);
        assert_ne!(
            col_a, col_b,
            "Aquarelle centroid drift must change middle-column RGB between t=0 and t=1"
        );
    }

    #[test]
    fn keyframe_tracks_render_centroid_drift_circle_with_keyframes() {
        // M1 修正後: Circle 経路でも keyframe_tracks ありのとき centroid を 50% 反映。
        // 同じ keyframe トラックで t=0 と t=1 を比べると少なくとも 1 ピクセル以上
        // 差分が出るはず（cross_axis 単独だった以前は色変化のみで微差はあったが、
        // ここでは「位置」の効果も加わるため、より顕著に異なるフレームが出る）。
        let clusters = drift_static_clusters();
        let mut opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts.shape = OrbShape::Circle;
        opts.keyframe_tracks = Some(drift_keyframe_tracks());

        let a = render_frame(&clusters, &opts, 0.0);
        let b = render_frame(&clusters, &opts, 1.0);
        let diff = pixel_diff_count(&a, &b);
        assert!(
            diff >= 1,
            "Circle + keyframe_tracks must show centroid drift (>=1 pixel diff between t=0 and t=1, got {diff})"
        );
    }

    #[test]
    fn keyframe_tracks_determinism_e2e() {
        // 同じ keyframe_tracks + 同じ seed + 同じ t=0.5 で render_frame_with_params を
        // 2 回呼んで byte-exact 同値であることを保証する。決定論性の最終ガード。
        let clusters = drift_static_clusters();
        let mut opts = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts.keyframe_tracks = Some(drift_keyframe_tracks());

        let cache = precompute_orb_params(&opts, &clusters);
        let a = render_frame_with_params(&clusters, &opts, &cache, 0.5);
        let b = render_frame_with_params(&clusters, &opts, &cache, 0.5);
        assert!(
            pixels_equal(&a, &b),
            "keyframe_tracks render must be byte-exact deterministic at t=0.5"
        );
    }

    #[test]
    fn keyframe_tracks_takes_precedence_over_color_tracks() {
        // 色が大きく違う color_tracks (#7) と keyframe_tracks (#33) を両方指定して、
        // 出力色は keyframe_tracks に従うことを確認する（#33 が #7 の上位互換のため）。
        // color_tracks 単独 vs keyframe_tracks+color_tracks の出力を比較し、
        // 後者が「keyframe_tracks 単独」と byte-exact 一致することを assert する。
        let clusters = drift_static_clusters();

        // keyframe_tracks のみ
        let mut opts_kf_only = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts_kf_only.keyframe_tracks = Some(drift_keyframe_tracks());

        // keyframe_tracks + color_tracks（color_tracks は明らかに違う色 — 緑系）
        let mut opts_both = opts_with(MotionDirection::LeftToRight, MotionSpeed::Slow);
        opts_both.keyframe_tracks = Some(drift_keyframe_tracks());
        opts_both.color_tracks = Some(vec![vec![[20, 240, 20], [20, 240, 20]]]);

        let kf_only = render_frame(&clusters, &opts_kf_only, 0.5);
        let both = render_frame(&clusters, &opts_both, 0.5);
        assert!(
            pixels_equal(&kf_only, &both),
            "keyframe_tracks must take precedence over color_tracks (output should match keyframe-only render)"
        );
    }
}
