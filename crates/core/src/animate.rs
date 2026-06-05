//! orb の一方通行コンベアベルト型アニメーションの **per-orb パラメータ計算**モジュール。
//!
//! 時間 `t ∈ [0, 1]` における各 orb の位置・呼吸・回転・色割当を決定論的に算出する。
//! `t = 0` と `t = 1` は同一状態に収束する完全ループ。#225 で CPU のピクセル
//! 描画は撲滅され、実描画は GPU(WGSL, [`crate::gpu`]) と web(WebGL) が担う。本モジュールは
//! 両者が共有する **算術と pack** だけを提供する（[`pack_render_data_for_webgl`] /
//! [`precompute_orb_params`] / [`aquarelle_modulated_clusters`]）。
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
//!   （phi_radius / phi_blur / phi_opacity は seed 由来で per-orb / per-axis 独立）
//! - 静止画は流れの一瞬。t=0 の状態で、phase 由来で orb が散らばる
//!
//! # 設計メモ
//!
//! - 軌道は**進行方向への線形運動 + 画面外バッファ付き `rem_euclid` による wrap**。
//!   直交軸の位置は初期位置から動かない。
//! - 進行量計算: `extent = 1 + 2r`、`raw = (phase + cycle * speed_mult * t) * extent`、
//!   `pos = raw.rem_euclid(extent) - r`。`cycle_count * speed_mult` は整数なので
//!   t=1.0 で fract が 0 になり、t=0 と完全一致するループが成り立つ。
//! - 速度ジッタは **整数倍** (1x / 2x / 3x)。VerySlow / Slow (cycle_count = 1 / 2) と
//!   組み合わせると実効周回数は {1, 2, 3, 4, 6} の 5 段階に分散する（#53）。
//! - 呼吸揺らぎは **3 軸独立**で sin。半径 ±10% / blur ±15% / opacity ±5%、
//!   それぞれ別位相。動画全体で各 1 周。
//! - RNG は [`rand_chacha::ChaCha8Rng`] を `seed` で固定。同じ seed・clusters・count で
//!   100% 同一の per-orb 列が返る（GPU / WebGL の決定論性の根拠）。

use crate::cluster::{Centroid, Cluster};
use crate::color_track::interpolate_color_track;
use crate::keyframe_track::{interpolate_keyframe_track, KeyframeClusterPoint};
use crate::orb::{OrbShape, OrbStyle};
use crate::style::SoftnessPreset;
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

/// 半径呼吸の振幅（±10%）。Aquarelle の per-orb 変調が weight_scale に乗せる。
/// GPU 経路は `radius_factor = 1.0 + BREATH_RADIUS_AMPLITUDE * sin(...)` の最大値
/// （= 1.10）を WGSL / gpu.rs 側の `BREATH_RADIUS_MAX_FACTOR` と揃える。
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
///
/// `edge_softness` (#205): Glyph / image アームの smoothstep 幅を softness preset と
/// 連動させるため、`SoftnessPreset::edge_softness()` の値 (0.3 / 0.6 / 1.0) を
/// shader に流す。Circle アームは Euclidean distance + falloff_curve なので影響を
/// 受けない。
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
    edge_softness: f32,
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
    // #205: header[12] = edge_softness (Glyph/image アーム smoothstep 幅、0.3..=1.0)。
    // Circle アームは参照しない。残り header[13..16] は今後の拡張用に予約。
    buf[12] = edge_softness;

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

/// glyph の per-orb 回転角を出す CPU 参照実装。実描画は GPU(`orb_glyph.wgsl`) が
/// 行うため、本関数は WGSL の `glyph_rotation_angle` と式が一致することを担保する
/// テスト（`wgsl_glyph_rotation_angle_matches_rust_rem_euclid`）専用の reference。
/// #225 で CPU 描画が消え、lib 本体からの呼び出しは無くなったので `cfg(test)`。
#[cfg(test)]
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

/// `count` の上限。万一おかしな値が来てもメモリ枯渇しないように防衛。
///
/// GPU 経路 (#207) も同じ上限で n_orbs を clamp するため `pub`。WebGL 経路の
/// `GL_RENDERER_MAX_ORBS`(=64) とは別の、アニメーション側の絶対上限。
pub const MAX_ORB_COUNT: usize = 1024;

/// `seed` / `count` / `clusters.weight` から決定的に算出した per-orb パラメータ列。
///
/// per-orb パラメータ計算の結果を保持するシーム。GPU Aquarelle 経路
/// （[`aquarelle_modulated_clusters`]）が位置 wrap / 呼吸 / 色補間に使う。
/// `seed` / `count` / `cluster_weights` のいずれかを変える場合は再計算する必要がある
/// （`Default` / `Copy` を実装しないのは、その不変条件をうっかり壊すのを防ぐため）。
#[derive(Debug, Clone)]
pub struct CachedOrbParams {
    params: Vec<OrbParams>,
}

/// `opts.seed` / `opts.count` / `clusters.weight` から決定的な orb パラメータ列を生成する。
///
/// per-orb パラメータ計算（位置・呼吸・回転・wrap・cluster 割当）の入口。GPU 経路
/// （`aquarelle_modulated_clusters` 等）が同じ決定論列を共有するために使う。
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

/// Aquarelle フレームの per-orb 変調（位置 wrap・半径呼吸・#33/#7 色補間）を 1 箇所に
/// 集約したヘルパ。GPU 経路（[`aquarelle_modulated_clusters`]）が使う。返す
/// `Vec<Cluster>` の index は `gpu::GpuRenderer::render_frame_aquarelle` の per-orb 4 層
/// 描画順（`aquarelle::render_aquarelle_orb` の `seed = i`）に対応する。
fn modulate_aquarelle_clusters(
    clusters: &[Cluster],
    opts: &AnimateOptions,
    params: &[OrbParams],
    t: f32,
) -> Vec<Cluster> {
    use crate::cluster::Centroid;

    let cycle = opts.speed.cycle_count();

    clusters
        .iter()
        .zip(params.iter())
        .enumerate()
        .map(|(idx, (c, p))| {
            // 動画入力（#33）: keyframe_tracks があれば色 + 重心 + 重みを時刻 t の
            // 補間値で読み替える。#7 (color_tracks) より優先される。
            let interpolated = pick_cluster_at_t(opts.keyframe_tracks.as_deref(), idx, c, t);
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
        .collect()
}

/// GPU Aquarelle 経路（#216）が per-orb の変調済み cluster 列を得るための
/// `#[doc(hidden)]` シーム。`pack_render_data_for_webgl` と同様、内部の `OrbParams`
/// レイアウトや RNG 列は公開しない。
///
/// 返り値の index は `gpu::GpuRenderer::render_frame_aquarelle` の per-orb 描画順
/// （`aquarelle::render_aquarelle_orb` の `seed = i`）に一致する。各 cluster の
/// 変調済み中心 / 重み / 色を読み、per-orb の 4 層を WGSL で描く。
#[doc(hidden)]
pub fn aquarelle_modulated_clusters(
    clusters: &[Cluster],
    opts: &AnimateOptions,
    t: f32,
) -> Vec<Cluster> {
    let cache = precompute_orb_params(opts, clusters);
    modulate_aquarelle_clusters(clusters, opts, &cache.params, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_count_matches_speed() {
        // 速度の cycle_count が仕様通りであることを保証する回帰テスト。
        assert_eq!(MotionSpeed::VerySlow.cycle_count(), 1);
        assert_eq!(MotionSpeed::Slow.cycle_count(), 2);
        assert_eq!(MotionSpeed::Mid.cycle_count(), 3);
        assert_eq!(MotionSpeed::Fast.cycle_count(), 4);
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
        assert!(
            (20..=44).contains(&cw),
            "clockwise count out of band: cw={cw} ccw={ccw}"
        );
        assert!(
            (20..=44).contains(&ccw),
            "counter-clockwise count out of band: cw={cw} ccw={ccw}"
        );
    }

    #[test]
    fn glyph_rotation_loop_closure_at_t_one() {
        let p = generate_orb_params(42, 16, &[1.0]);
        for cycle in 1..=4 {
            for q in &p {
                let a0 = glyph_rotation_angle(cycle, 0.0, q.base_angle, q.rot_speed_signed, true);
                let a1 = glyph_rotation_angle(cycle, 1.0, q.base_angle, q.rot_speed_signed, true);
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
        let p = generate_orb_params(42, 16, &[1.0]);
        for cycle in 1..=4 {
            for q in &p {
                for &t in &[0.0_f32, 0.13, 0.25, 0.5, 0.77, 1.0] {
                    let a = glyph_rotation_angle(cycle, t, q.base_angle, q.rot_speed_signed, false);
                    assert!(
                        (a - q.base_angle).abs() < 1e-6,
                        "glyph_rotate=false must hold base_angle: cycle={cycle} t={t} base={} got={a}",
                        q.base_angle,
                    );
                }
            }
        }
    }

    /// #212: WGSL の turns 式 `x - floor(x)` は Rust の `rem_euclid(x, 1.0)` と
    /// 一致しなければならない。回転角が WGSL と CPU(param 計算) で乖離しないことを固定する。
    #[test]
    fn wgsl_glyph_rotation_angle_matches_rust_rem_euclid() {
        fn wgsl_turns(cycle: u32, rot_speed_signed: f32, t: f32) -> f32 {
            let x = cycle as f32 * rot_speed_signed * t;
            x - x.floor()
        }
        fn wgsl_angle(cycle: u32, t: f32, base_angle: f32, rot_speed_signed: f32) -> f32 {
            base_angle + wgsl_turns(cycle, rot_speed_signed, t) * TAU
        }

        let base_angles = [0.0_f32, 0.3, 1.7, std::f32::consts::PI];
        let speeds = [-2.0_f32, -1.0, -0.5, 0.5, 1.0, 2.0, 3.0];
        let times = [0.0_f32, 0.1, 0.25, 0.4999, 0.5, 0.7777, 0.9, 1.0];
        for cycle in 1u32..=4 {
            for &base_angle in &base_angles {
                for &speed in &speeds {
                    for &t in &times {
                        let rust = glyph_rotation_angle(cycle, t, base_angle, speed, true);
                        let wgsl = wgsl_angle(cycle, t, base_angle, speed);
                        assert!(
                            (rust - wgsl).abs() < 1e-5,
                            "WGSL turns (x - floor(x)) must match Rust rem_euclid(_, 1.0): \
                             cycle={cycle} base={base_angle} speed={speed} t={t} \
                             rust={rust} wgsl={wgsl}"
                        );
                    }
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
        // - 同じ orb 内では radius / blur / opacity の 3 軸が異なる位相
        // - orb 間でも同じ軸の位相が散らばっている
        let p = generate_orb_params(42, 16, &[1.0]);
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
        assert!(
            max_r - min_r > 1.0,
            "phi_radius spread too narrow ({min_r} .. {max_r})"
        );
        assert!(
            max_b - min_b > 1.0,
            "phi_blur spread too narrow ({min_b} .. {max_b})"
        );
        assert!(
            max_o - min_o > 1.0,
            "phi_opacity spread too narrow ({min_o} .. {max_o})"
        );
    }

    /// #225: per-orb パラメータ計算（pack/GPU 共有）が決定論的であること。
    /// `aquarelle_modulated_clusters` の入口 `precompute_orb_params` が同じ seed /
    /// count / clusters で同じ列を返すことを cross_axis / phase 列の一致で固定する。
    #[test]
    fn precompute_orb_params_is_deterministic() {
        use crate::cluster::Centroid;
        let clusters = vec![
            Cluster {
                color: [220, 60, 60],
                centroid: Centroid { x: 0.3, y: 0.4 },
                weight: 0.5,
            },
            Cluster {
                color: [60, 120, 220],
                centroid: Centroid { x: 0.7, y: 0.6 },
                weight: 0.3,
            },
        ];
        let opts = AnimateOptions {
            seed: 777,
            count: Some(16),
            ..AnimateOptions::default()
        };
        let a = precompute_orb_params(&opts, &clusters);
        let b = precompute_orb_params(&opts, &clusters);
        assert_eq!(a.params.len(), 16);
        for (pa, pb) in a.params.iter().zip(b.params.iter()) {
            assert_eq!(pa.phase, pb.phase);
            assert_eq!(pa.cross_axis, pb.cross_axis);
            assert_eq!(pa.cluster_idx, pb.cluster_idx);
            assert_eq!(pa.speed_mult, pb.speed_mult);
        }
    }
}
