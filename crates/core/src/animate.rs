//! orb の一方通行コンベアベルト型アニメーションの **per-orb パラメータ計算**モジュール。
//!
//! 時間 `t ∈ [0, 1]` における各 orb の位置・呼吸・回転・色割当を決定論的に算出する。
//! `t = 0` と `t = 1` は同一状態に収束する完全ループ。#225 で CPU のピクセル
//! 描画は撲滅され、実描画は GPU(WGSL, [`crate::gpu`]) がネイティブ CLI と Web wasm の
//! 両方で担う。本モジュールはそれらが共有する **算術と pack** だけを提供する
//! （[`pack_render_data`] と per-orb パラメータ計算）。
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
//!   100% 同一の per-orb 列が返る（GPU(WGSL) 描画の決定論性の根拠）。

use crate::cluster::Cluster;
use crate::color_track::interpolate_color_track;
use crate::keyframe_track::interpolate_keyframe_track;
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

/// #241「薄い影」の production 既定値。orb 機構（orb / glyph / image）の最外周
/// フェードセグメントに掛ける rgb 暗化の強度係数（0..1）。
///
/// - `0.0` = 影なし（#242 直後の旧 WebGL 式と bit 同一）
/// - `1.0` = #242 で撤去した旧 lowp の rgb→0 フェードと同等の暗さ
///
/// kako-jun が gpu-lab のスライダー（`WasmParams::shadow_strength`）で実機選定した
/// 値（#241、freeza session595: 「０．２」）。製品（CLI / Studio）はこの定数のみを
/// 使い、調整ノブは外に出さない。値の置き場はこの 1 箇所に集約する。
pub const SHADOW_STRENGTH_DEFAULT: f32 = 0.2;

/// #255: 位置追従（[`keyframe_cross_drift`]）のドリフト量を抑える係数（0..1）。
///
/// デトレンドした重心の揺らぎ（端点を結ぶ直線からの偏差）にこの係数を掛けて、
/// **ごく微妙な揺らぎ**に抑える。`1.0` ならデトレンド生値そのまま、`0.0` ならドリフト
/// 無効（pack の `off+13` が常に 0）。
///
/// 値は kako-jun のライブ blink で選定（#255 サインオフ）。gain 0.25 や 0.60 では
/// 「動きすぎ」で、止まってはいない"気配だけ"の揺らぎとして **0.10** に確定した。製品（CLI / core）は
/// この定数のみを使い、調整ノブは外に出さない（値の置き場はこの 1 箇所に集約する。
/// `SHADOW_STRENGTH_DEFAULT` と同じ方針）。
pub const KEYFRAME_DRIFT_GAIN: f32 = 0.10;

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
    /// #239: 水彩のにじみ（bleed/bloom/offset/halo）を統一機構の上の加算レイヤーとして
    /// 任意の shape（orb/glyph/image）に乗せる設定。`None`（既定）のとき加算層の
    /// パラメータは全 0 = plain orb と byte 一致（既存挙動を一切変えない）。`Some(cfg)`
    /// のとき非 0 パラメータを GPU 経路へ流す（幾何は唯一の continuous space-blur）。
    pub aqua: Option<AquaBleedConfig>,
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
    /// Circle 経路では使われない。既定 `true` で互換維持。
    pub glyph_rotate: bool,
    /// #241「薄い影」の強度係数（0..1）。orb 機構（orb / glyph / image）の最外周
    /// フェードセグメントで orb 色 rgb を `mix(1.0, 1.0-u, s)` 倍に暗化する。
    /// `0.0` = #242 直後（影なし）と bit 同一、`1.0` = 旧 lowp の rgb→0 フェードと
    /// 同等。製品は [`SHADOW_STRENGTH_DEFAULT`] 固定（CLI フラグ・Studio UI なし）。
    /// gpu-lab（dev）だけが `WasmParams::shadow_strength` 経由で上書きする。
    pub shadow_strength: f32,
    /// 動画入力（#7）の per-cluster 色トラック。
    ///
    /// `Some(tracks)` のとき、各 orb の `cluster.color` は
    /// `interpolate_color_track(tracks[cluster_idx], t)` で動的に上書きされる
    /// （#251 で [`apply_color_tracks_at_t`] が pack 直前に評価する）。
    /// `tracks.len()` が clusters の数より少ない場合（理論上ないが防衛）や、
    /// 個別 track が空の場合は `cluster.color` にフォールバックする。
    /// `None` は静止画入力の従来挙動（色固定）。
    pub color_tracks: Option<Vec<Vec<[u8; 3]>>>,
    /// 動画入力（#33）の per-cluster キーフレーム補間トラック。
    ///
    /// `Some(tracks)` のとき、各 orb の `cluster.color` が
    /// [`crate::keyframe_track::interpolate_keyframe_track`] の補間色で時刻 `t` に
    /// 動的に上書きされる（#251 で [`apply_color_tracks_at_t`] が pack 直前に評価。
    /// **色だけ**反映し、補間結果の `centroid` / `weight` は捨てる）。`color_tracks`
    /// (#7) と排他で、両方 Some の場合は `keyframe_tracks` を優先する（#33 が #7 の
    /// 上位互換）。`None` のときは `color_tracks` に従う。
    /// 位置追従（centroid の cross 軸ドリフト）は #255 で実装済み。ただし
    /// [`apply_color_tracks_at_t`] は色だけを扱い、ドリフトは別関数
    /// [`keyframe_cross_drift`] が per-cluster の delta（**デトレンドした重心揺らぎ ×
    /// [`KEYFRAME_DRIFT_GAIN`]**）を算出して pack の `off+13` に載せる（`orb.wgsl` が
    /// `misc.w` として cross 軸に加算）。正味スイープは除去されループが閉じる
    /// （`t=0 ≡ t=1` で 0）、直線トラックは 0。`weight` 変調は色割当の安定のため
    /// 意図的に適用しない（#255 で確定）。位置 wrap / breathing は `orb.wgsl` 自身がやるので、
    /// 補間 centroid をそのまま戻すと二重適用になる（だから散布保持 + delta 加算の B 案）。
    pub keyframe_tracks: Option<Vec<Vec<crate::keyframe_track::KeyframeClusterPoint>>>,
}

/// #239: the additive "bleed" watercolor config carried on [`AnimateOptions`].
/// The four sliders feed the additive layer over the unified orb mechanism. All four
/// at `0.0` makes the layer structurally inert (byte-identical to plain orb) — the
/// non-regression gate. Carried as `Option` on `AnimateOptions` so the default
/// (`None`) path is exactly the existing behaviour. The geometry is the single
/// continuous space-blur (the Blob A/B variant was dropped in #239 Phase 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AquaBleedConfig {
    pub bleed: f32,
    pub bloom: f32,
    pub offset: f32,
    pub halo: f32,
}

impl Default for AnimateOptions {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            aqua: None,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction: MotionDirection::LeftToRight,
            speed: MotionSpeed::Slow,
            seed: 0,
            count: None,
            background: [0, 0, 0, 255],
            shape: OrbShape::Orb,
            softness: SoftnessPreset::Mid,
            glyph_rotate: true,
            shadow_strength: SHADOW_STRENGTH_DEFAULT,
            color_tracks: None,
            keyframe_tracks: None,
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

/// per-orb render data を詰めた Float32 words を返す（ネイティブ GPU / CLI と
/// Web wasm が共有する正規 pack ヘルパ）。
///
/// shape / softness / rotation を CPU 経路と同じ決定論性で GPU(WGSL) 描画へ
/// 渡すための purpose-built helper。内部 RNG 列や `OrbParams` のレイアウトは
/// 公開しない。#247 で旧称 `pack_render_data_for_webgl` から改名（WebGL レンダラ
/// 撤去後、これは core GPU / CLI の正規 pack ヘルパであり WebGL 専用ではない）。
///
/// `glyph_rotate` (#136): `false` を渡すと shader 側で per-orb 回転を抑止し、
/// 全 t で `base_angle` のまま描く。Circle 経路には影響しない。
///
/// `edge_softness` (#205): Glyph / image アームの smoothstep 幅を softness preset と
/// 連動させるため、`SoftnessPreset::edge_softness()` の値 (0.3 / 0.6 / 1.0) を
/// shader に流す。Circle アームは Euclidean distance + falloff_curve なので影響を
/// 受けない。
///
/// `shadow_strength` (#241): orb 機構（orb / glyph / image）の最外周フェードの
/// rgb 暗化強度（0..1、`header[13]`）。0..1 にクランプして詰める。WGSL（gpu.rs）が
/// Params uniform に読む（#241 で追加された word）。
///
/// `cross_drift` (#255): per-cluster の cross 軸 重心ドリフト delta（B 案、動画 #33
/// キーフレームの位置追従）。**デトレンドした重心揺らぎ × [`KEYFRAME_DRIFT_GAIN`]**。
/// index は cluster index、各 orb は `cluster_idx` で引く。per-orb word `off+13`
/// （従来 0.0 埋めの空き）に書く。`None`（tracks 無し）のとき 0.0 ＝ **従来と byte
/// 完全一致**（非回帰ゲート）。正味スイープは除去されループが閉じる（`t=0 ≡ t=1` で 0）、
/// 直線トラックは 0（[`keyframe_cross_drift`] が算出。centroid 絶対値は使わない＝縞を出さない）。
#[allow(clippy::too_many_arguments)]
pub fn pack_render_data(
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
    shadow_strength: f32,
    cross_drift: Option<&[f32]>,
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
    // Circle アームは参照しない。
    buf[12] = edge_softness;
    // #241: header[13] = shadow_strength（最外周フェードの rgb 暗化強度、0..1 に
    // クランプ）。WGSL の Params uniform へ。残り header[14..16] は今後の拡張用に予約。
    buf[13] = shadow_strength.clamp(0.0, 1.0);

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
        // #255: off+13 = cross 軸 重心ドリフト delta（B 案）。`None`（tracks 無し）の
        // とき 0.0 ＝ 従来と byte 完全一致。`p.cluster_idx` で per-cluster delta を引く。
        // `cluster_idx` は `pick_weighted` が常に `[0, clusters.len())` を返し、production の
        // `cross_drift` は長さ `clusters.len()`（[`keyframe_cross_drift`] が `n_clusters` で生成）
        // なので添字は常に範囲内。色の `.min(len-1)` clamp と同じ不変条件に乗っており、
        // `.get(..).unwrap_or(0.0)` は防御（範囲外なら「ドリフト無し」が正しい既定値）。
        buf[off + 13] = cross_drift.map_or(0.0, |d| d.get(p.cluster_idx).copied().unwrap_or(0.0));
    }
    buf
}

/// 動画入力（#7 / #33）の色トラックを時刻 `t` で評価し、各 cluster の `color` だけを
/// 上書きした新しい `Cluster` 列を返す（#251 で統一 WGSL レンダラへ再配線する一本）。
///
/// **色だけ**を扱う。`centroid` と `weight` は元のまま素通しする。位置追従（#33 の
/// centroid ドリフト）は #255 で実装済みだが、本関数ではなく [`keyframe_cross_drift`] が
/// per-cluster の cross 軸 delta を別途算出して担う（pack の `off+13` 経由）。`weight` 変調は
/// 色割当の安定のため意図的に適用しない（#255 で確定）。旧 `modulate_aquarelle_clusters`
/// は色に加えて位置 wrap・breathing も焼き込んでいたが、それらは今 `orb.wgsl` 自身が
/// `advance_steps` / `radius_factor` でやるので、ここで戻すと二重適用になる。
///
/// 優先順位は `keyframe_tracks` (#33) > `color_tracks` (#7)（旧挙動と同じ。#33 が
/// #7 の上位互換）。track の index は cluster の index と一致する
/// （`pack_render_data` が引く `cluster_idx` と同じ）。
///
/// 両 tracks が `None` のときは入力 `clusters` をそのまま複製して返す（変調なし）。
/// 呼び出し側は両 `None` のとき本関数を呼ばずに元 `clusters` を直接使い、無駄な
/// 複製と byte 差混入の両方を避けるべき（#251 の非回帰ゲート）。
///
/// `pub`（兄弟の `interpolate_color_track` / `interpolate_keyframe_track` と同様）。
/// 実消費は gpu feature の pack 経路だが、feature を切った素の core lib でも
/// 「未使用」扱いにならないよう公開 API として置く。
pub fn apply_color_tracks_at_t(
    clusters: &[Cluster],
    opts: &AnimateOptions,
    t: f32,
) -> Vec<Cluster> {
    clusters
        .iter()
        .enumerate()
        .map(|(cluster_idx, cluster)| {
            // #33 (keyframe) を #7 (color) より優先。色だけ取り出し centroid/weight は捨てる。
            if let Some(track) = opts
                .keyframe_tracks
                .as_ref()
                .and_then(|tracks| tracks.get(cluster_idx))
                .filter(|track| !track.is_empty())
            {
                let (color, _centroid, _weight) = interpolate_keyframe_track(track, t);
                return Cluster { color, ..*cluster };
            }
            if let Some(track) = opts
                .color_tracks
                .as_ref()
                .and_then(|tracks| tracks.get(cluster_idx))
                .filter(|track| !track.is_empty())
            {
                return Cluster {
                    color: interpolate_color_track(track, t),
                    ..*cluster
                };
            }
            // どちらも該当なし → color 据え置き（centroid / weight は常に元のまま）。
            *cluster
        })
        .collect()
}

/// 動画入力（#33 キーフレーム）の **cross 軸 重心ドリフト delta** を per-cluster で返す。
///
/// B 案（#255）: 一様散布（`OrbParams::cross_axis`）は保持したまま、その cluster の
/// 重心の **デトレンドした揺らぎ × [`KEYFRAME_DRIFT_GAIN`]** だけを上に**加算**する
/// ためのスカラー列。各 orb は `cluster_idx` でこの列を引き、同色の群れが塊ごとに
/// ドリフトする（縞＝同色 orb が 1 帯に吸着、は出さない）。
///
/// orber 出力は**ループ動画**（`t=0 ≡ t=1`）。素の `cross(t) − cross(0)` は片道
/// スイープ成分を含み、継ぎ目（`t=1→0`）で跳ねてループを壊す。そこで端点を結ぶ直線を
/// 差し引く（デトレンド）:
///
/// ```text
/// detrended = cross(t) − (cross(0)·(1−t) + cross(1)·t)
/// slot      = KEYFRAME_DRIFT_GAIN · detrended
/// ```
///
/// これにより `drift(0) = drift(1) = 0`（ループ閉じ）。直線的な片道スイープ
/// （2 キーの linear track）は全 `t` で `detrended = 0` ＝ 正味移動を乗せない（正しい）。
/// 揺らぎ（3 キー以上で中点が直線から外れる動き）にだけ反応する。
///
/// - `keyframe_tracks` が `None` または空なら `None` を返す。呼び出し側はこれを
///   `pack_render_data` に `None` として渡し、`misc.w = 0` ＝ **従来と byte 一致**にする。
/// - `Some` のとき、長さ `n_clusters` の `Vec<f32>` を返す。各 index について
///   [`interpolate_keyframe_track`] の centroid を `t` / `0.0` / `1.0` で取り、
///   上式の **cross 軸座標のデトレンド偏差 × gain** を入れる。centroid の
///   **絶対値は使わない**（A 案＝縞を避ける）。
/// - cross 軸は direction 依存: LR/RL（`direction_id < 1.5`）→ centroid.y、
///   TB/BT（`>= 1.5`）→ centroid.x。
/// - track が無い / 空の cluster index は delta = 0.0。
///
/// `weight` 変調は戻さない（#251 の挙動を維持。位置だけ）。色も別ヘルパ
/// [`apply_color_tracks_at_t`] の管轄でここでは触らない。
pub fn keyframe_cross_drift(
    opts: &AnimateOptions,
    t: f32,
    direction_id: f32,
    n_clusters: usize,
) -> Option<Vec<f32>> {
    let tracks = opts.keyframe_tracks.as_ref()?;
    if tracks.is_empty() {
        return None;
    }
    // cross 軸の選択: LR/RL → y、TB/BT → x。
    let cross_is_y = direction_id < 1.5;
    let cross = |c: &crate::cluster::Centroid| if cross_is_y { c.y } else { c.x };
    // デトレンドの直線は `t`/`1−t` を掛けるので、NaN t を素通しすると drift が NaN に
    // なる（interpolate 側の NaN→先頭クランプだけでは防げない）。NaN は先頭時刻 0.0
    // 相当（drift 0、跳ねなし）に正規化する。
    let t = if t.is_nan() { 0.0 } else { t };
    let mut drift = vec![0.0f32; n_clusters];
    for (idx, slot) in drift.iter_mut().enumerate() {
        let Some(track) = tracks.get(idx).filter(|track| !track.is_empty()) else {
            continue; // track 無し / 空 → delta = 0.0。
        };
        let (_color, cen_t, _weight) = interpolate_keyframe_track(track, t);
        let (_color0, cen_0, _weight0) = interpolate_keyframe_track(track, 0.0);
        let (_color1, cen_1, _weight1) = interpolate_keyframe_track(track, 1.0);
        let (c_t, c_0, c_1) = (cross(&cen_t), cross(&cen_0), cross(&cen_1));
        // 端点を結ぶ直線からの偏差（デトレンド）。t=0/t=1 で必ず 0 ＝ ループ閉じ。
        // 直線スイープ（2 キー linear）は全 t で 0、揺らぎだけが残る。NaN t は
        // interpolate が先頭値クランプするので panic しない。
        let detrended = c_t - (c_0 * (1.0 - t) + c_1 * t);
        *slot = KEYFRAME_DRIFT_GAIN * detrended;
    }
    Some(drift)
}

/// glyph の per-orb 回転角を出す CPU 参照実装。実描画は GPU の SDF variant
/// （`orb.wgsl` を `gpu.rs` の `orb_sdf_wgsl()` が合成）が行うため、本関数は WGSL の
/// `glyph_rotation_angle` と式が一致することを担保する
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
/// GPU 経路 (#207) も同じ上限で n_orbs を clamp するため `pub`。wasm 供給系の
/// `GL_RENDERER_MAX_ORBS`(=64、旧固定 uniform-array レンダラ由来) とは別の、
/// アニメーション側の絶対上限。
pub const MAX_ORB_COUNT: usize = 1024;

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

    /// #241: header[13] に shadow_strength がそのまま（ただし 0..=1 クランプで）
    /// 入ること。WGSL の Params uniform はこの word を読むので、ここがずれると
    /// 影強度が黙って変わる。クランプは hand-built な範囲外値への防衛（ただし
    /// f32::clamp は NaN を素通しする — NaN まで守る趣旨ではない）。production
    /// 経路は wasm validate（0..=1・NaN reject）か定数なのでクランプ恒等。
    #[test]
    fn pack_header13_carries_clamped_shadow_strength() {
        let clusters = vec![Cluster {
            color: [200, 100, 50],
            centroid: crate::cluster::Centroid { x: 0.5, y: 0.5 },
            weight: 1.0,
        }];
        let pack_with = |s: f32| {
            pack_render_data(
                &clusters,
                [0, 0, 0, 255],
                32.0,
                0.5,
                0.0,
                2.0,
                7,
                1,
                1.0,
                0.0,
                true,
                0.5,
                s,
                None,
            )
        };
        assert_eq!(pack_with(0.7)[13], 0.7, "in-range value must pass through");
        assert_eq!(pack_with(0.0)[13], 0.0, "0.0 is inclusive");
        assert_eq!(pack_with(1.0)[13], 1.0, "1.0 is inclusive");
        assert_eq!(pack_with(-0.5)[13], 0.0, "below range clamps to 0");
        assert_eq!(pack_with(1.5)[13], 1.0, "above range clamps to 1");
    }

    // ---- #251: apply_color_tracks_at_t (color-only fold) ----

    use crate::cluster::Centroid;
    use crate::keyframe_track::KeyframeClusterPoint;

    fn track_cluster(color: [u8; 3], cx: f32, cy: f32, weight: f32) -> Cluster {
        Cluster {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight,
        }
    }

    /// #251: with neither track set, the fold returns the clusters unchanged
    /// (color / centroid / weight all preserved) — the inert / non-regression path.
    #[test]
    fn apply_color_tracks_none_returns_input_unchanged() {
        let clusters = vec![
            track_cluster([10, 20, 30], 0.1, 0.2, 0.3),
            track_cluster([200, 100, 50], 0.7, 0.8, 0.6),
        ];
        let opts = AnimateOptions::default(); // color_tracks / keyframe_tracks both None
        let out = apply_color_tracks_at_t(&clusters, &opts, 0.5);
        assert_eq!(
            out, clusters,
            "no tracks ⇒ clusters must pass through unchanged"
        );
    }

    /// #251: a color track (#7) overwrites only `color` at time `t`; centroid and
    /// weight stay exactly on the original cluster.
    #[test]
    fn apply_color_tracks_color_track_overwrites_color_only() {
        let clusters = vec![track_cluster([10, 20, 30], 0.1, 0.2, 0.3)];
        let opts = AnimateOptions {
            color_tracks: Some(vec![vec![[0, 0, 0], [100, 200, 40]]]),
            ..AnimateOptions::default()
        };
        // t=1.0 ⇒ end key color.
        let out = apply_color_tracks_at_t(&clusters, &opts, 1.0);
        assert_eq!(
            out[0].color,
            [100, 200, 40],
            "color must come from the track"
        );
        assert_eq!(
            out[0].centroid, clusters[0].centroid,
            "centroid must be untouched"
        );
        assert_eq!(
            out[0].weight, clusters[0].weight,
            "weight must be untouched"
        );
    }

    /// #251: a keyframe track (#33) reflects **color only** in `apply_color_tracks_at_t`
    /// — the interpolated centroid / weight from the keyframe are discarded; the original
    /// cluster's centroid / weight are kept. The position follow-through is handled
    /// separately by `keyframe_cross_drift` (#255), not by this function.
    #[test]
    fn apply_color_tracks_keyframe_track_reflects_color_only() {
        let clusters = vec![track_cluster([0, 0, 0], 0.5, 0.5, 0.5)];
        let kf = |color: [u8; 3], cx: f32, cy: f32, weight: f32, time: f32| KeyframeClusterPoint {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight,
            time,
        };
        let opts = AnimateOptions {
            keyframe_tracks: Some(vec![vec![
                kf([0, 0, 0], 0.0, 0.0, 0.0, 0.0),
                kf([200, 100, 50], 1.0, 1.0, 1.0, 1.0),
            ]]),
            ..AnimateOptions::default()
        };
        // t=1.0 ⇒ end key: color = [200,100,50], but centroid/weight stay original.
        let out = apply_color_tracks_at_t(&clusters, &opts, 1.0);
        assert_eq!(
            out[0].color,
            [200, 100, 50],
            "color must come from the keyframe"
        );
        assert_eq!(
            out[0].centroid,
            Centroid { x: 0.5, y: 0.5 },
            "keyframe centroid must be discarded here (position drift is handled by keyframe_cross_drift, #255)"
        );
        assert_eq!(out[0].weight, 0.5, "keyframe weight must be discarded");
    }

    /// #251: keyframe (#33) takes priority over color track (#7) when both are set
    /// for the same cluster index (the #33-is-superset-of-#7 ordering).
    #[test]
    fn apply_color_tracks_keyframe_wins_over_color_track() {
        let clusters = vec![track_cluster([0, 0, 0], 0.5, 0.5, 0.5)];
        let opts = AnimateOptions {
            color_tracks: Some(vec![vec![[9, 9, 9], [9, 9, 9]]]),
            keyframe_tracks: Some(vec![vec![
                KeyframeClusterPoint {
                    color: [1, 2, 3],
                    centroid: Centroid { x: 0.0, y: 0.0 },
                    weight: 0.0,
                    time: 0.0,
                },
                KeyframeClusterPoint {
                    color: [77, 88, 99],
                    centroid: Centroid { x: 1.0, y: 1.0 },
                    weight: 1.0,
                    time: 1.0,
                },
            ]]),
            ..AnimateOptions::default()
        };
        let out = apply_color_tracks_at_t(&clusters, &opts, 1.0);
        assert_eq!(
            out[0].color,
            [77, 88, 99],
            "keyframe track must win over color track"
        );
    }

    /// #251: an empty per-cluster track falls back to the cluster's own color
    /// (defensive: a Some(tracks) with a zero-length entry must not blacken the orb).
    #[test]
    fn apply_color_tracks_empty_track_falls_back_to_cluster_color() {
        let clusters = vec![
            track_cluster([10, 20, 30], 0.1, 0.2, 0.3),
            track_cluster([200, 100, 50], 0.7, 0.8, 0.6),
        ];
        // cluster 0 has an empty track; cluster 1 has a real one.
        let opts = AnimateOptions {
            color_tracks: Some(vec![vec![], vec![[5, 5, 5], [250, 250, 250]]]),
            ..AnimateOptions::default()
        };
        let out = apply_color_tracks_at_t(&clusters, &opts, 1.0);
        assert_eq!(
            out[0].color,
            [10, 20, 30],
            "empty track ⇒ keep cluster color"
        );
        assert_eq!(
            out[1].color,
            [250, 250, 250],
            "non-empty track ⇒ track color"
        );
    }

    // ---- #255: keyframe_cross_drift (cross-axis centroid drift delta, B案) ----

    /// Build a [`KeyframeClusterPoint`] with the given centroid / time
    /// (color / weight are inert for drift — only centroid matters here).
    fn kfp(cx: f32, cy: f32, time: f32) -> KeyframeClusterPoint {
        KeyframeClusterPoint {
            color: [0, 0, 0],
            centroid: Centroid { x: cx, y: cy },
            weight: 0.0,
            time,
        }
    }

    fn drift_opts(tracks: Option<Vec<Vec<KeyframeClusterPoint>>>) -> AnimateOptions {
        AnimateOptions {
            keyframe_tracks: tracks,
            ..AnimateOptions::default()
        }
    }

    fn approx(a: f32, b: f32, label: &str) {
        assert!(
            (a - b).abs() < 1e-6,
            "{label}: expected ~{b}, got {a} (eps=1e-6)"
        );
    }

    /// #255 A1: no keyframe tracks ⇒ `None` (caller passes `None` to the packer,
    /// so `misc.w` stays 0.0 = byte-identical to pre-#255).
    #[test]
    fn keyframe_cross_drift_none_tracks_returns_none() {
        let opts = drift_opts(None);
        assert!(
            keyframe_cross_drift(&opts, 0.5, 0.0, 3).is_none(),
            "no tracks ⇒ None"
        );
    }

    /// #255 A2: an empty tracks Vec is treated like `None` (no per-cluster track to
    /// follow), so the drift is `None` — not a `Some(vec![0.0; n])` that would still
    /// be byte-identical but allocate needlessly.
    #[test]
    fn keyframe_cross_drift_empty_tracks_returns_none() {
        let opts = drift_opts(Some(vec![]));
        assert!(
            keyframe_cross_drift(&opts, 0.5, 0.0, 3).is_none(),
            "empty tracks Vec ⇒ None"
        );
    }

    /// #255 A3 (loop): drift is **detrended** from the line joining the endpoints, so
    /// at `t=0` every cluster's drift is ~0 (`drift(0)=0` is one half of the loop-close
    /// guarantee; the orb sits exactly where the still image would). track[0].time=0.0.
    #[test]
    fn keyframe_cross_drift_t0_is_all_zero_delta() {
        let opts = drift_opts(Some(vec![
            vec![kfp(0.2, 0.2, 0.0), kfp(0.9, 0.8, 1.0)],
            vec![kfp(0.5, 0.1, 0.0), kfp(0.5, 0.7, 1.0)],
        ]));
        let drift = keyframe_cross_drift(&opts, 0.0, 0.0, 2).expect("Some at t=0");
        for (i, d) in drift.iter().enumerate() {
            approx(*d, 0.0, &format!("t=0 drift for cluster {i}"));
        }
    }

    /// #255 A4a: a straight one-way sweep (2-key linear track, y:0.2→0.8) is fully
    /// removed by the detrend at **every** `t` ⇒ drift ≈ 0 everywhere. This pins that
    /// the net sweep carries no position offset (loops cannot pick up a one-way drift).
    #[test]
    fn keyframe_cross_drift_linear_track_is_zero_after_detrend() {
        let opts = drift_opts(Some(vec![vec![kfp(0.5, 0.2, 0.0), kfp(0.5, 0.8, 1.0)]]));
        for &t in &[0.0f32, 0.5, 1.0] {
            let d = keyframe_cross_drift(&opts, t, 0.0, 1).expect("Some")[0];
            approx(
                d,
                0.0,
                &format!("linear LR sweep must detrend to 0 at t={t}"),
            );
        }
    }

    /// #255 A4b: LR (direction_id=0.0) ⇒ cross axis = **y**. A 3-key wobble
    /// (y: 0.2→0.8→0.2) leaves the line at the midpoint: detrended(0.5)=0.8−0.2=0.6,
    /// ×gain ⇒ `KEYFRAME_DRIFT_GAIN·0.6`; endpoints 0. Moving x must NOT change it
    /// (LR ignores x). Expectation is symbolic in the gain so re-tuning the const
    /// (a product value) does not break this axis/detrend test.
    #[test]
    fn keyframe_cross_drift_lr_wobble_uses_centroid_y() {
        // x fixed: only y wobbles.
        let fixed_x = drift_opts(Some(vec![vec![
            kfp(0.5, 0.2, 0.0),
            kfp(0.5, 0.8, 0.5),
            kfp(0.5, 0.2, 1.0),
        ]]));
        approx(
            keyframe_cross_drift(&fixed_x, 0.5, 0.0, 1).expect("Some")[0],
            KEYFRAME_DRIFT_GAIN * 0.6,
            "LR t=0.5 y-wobble (0.6 detrended × gain)",
        );
        approx(
            keyframe_cross_drift(&fixed_x, 0.0, 0.0, 1).expect("Some")[0],
            0.0,
            "LR t=0 wobble drift",
        );
        approx(
            keyframe_cross_drift(&fixed_x, 1.0, 0.0, 1).expect("Some")[0],
            0.0,
            "LR t=1 wobble drift",
        );

        // Same y wobble but x now also moves: LR must give the identical y-drift.
        let moving_x = drift_opts(Some(vec![vec![
            kfp(0.1, 0.2, 0.0),
            kfp(0.95, 0.8, 0.5),
            kfp(0.1, 0.2, 1.0),
        ]]));
        approx(
            keyframe_cross_drift(&moving_x, 0.5, 0.0, 1).expect("Some")[0],
            KEYFRAME_DRIFT_GAIN * 0.6,
            "LR wobble drift must be x-invariant",
        );
    }

    /// #255 A5: TB (direction_id=2.0) ⇒ cross axis = **x**. A 3-key wobble
    /// (x: 0.2→0.8→0.2) gives `KEYFRAME_DRIFT_GAIN·0.6` at t=0.5, 0 at the endpoints;
    /// moving y must not change it (TB ignores y).
    #[test]
    fn keyframe_cross_drift_tb_uses_centroid_x() {
        let fixed_y = drift_opts(Some(vec![vec![
            kfp(0.2, 0.5, 0.0),
            kfp(0.8, 0.5, 0.5),
            kfp(0.2, 0.5, 1.0),
        ]]));
        approx(
            keyframe_cross_drift(&fixed_y, 0.5, 2.0, 1).expect("Some")[0],
            KEYFRAME_DRIFT_GAIN * 0.6,
            "TB t=0.5 x-wobble",
        );
        approx(
            keyframe_cross_drift(&fixed_y, 0.0, 2.0, 1).expect("Some")[0],
            0.0,
            "TB t=0 drift",
        );
        approx(
            keyframe_cross_drift(&fixed_y, 1.0, 2.0, 1).expect("Some")[0],
            0.0,
            "TB t=1 drift",
        );
        let moving_y = drift_opts(Some(vec![vec![
            kfp(0.2, 0.1, 0.0),
            kfp(0.8, 0.95, 0.5),
            kfp(0.2, 0.1, 1.0),
        ]]));
        approx(
            keyframe_cross_drift(&moving_y, 0.5, 2.0, 1).expect("Some")[0],
            KEYFRAME_DRIFT_GAIN * 0.6,
            "TB wobble drift must be y-invariant",
        );
    }

    /// #255 A6: the axis switch happens at direction_id 1.5. One wobble track whose x
    /// and y both deviate from their endpoint line at the midpoint
    /// (x:0.2→0.7→0.2 ⇒ Δx=0.5, y:0.1→0.9→0.1 ⇒ Δy=0.8): RL(1.0, below 1.5) must pick
    /// the y-wobble (`gain·0.8`), TB(2.0, above 1.5) the x-wobble (`gain·0.5`).
    /// Contrasted at t=0.5 in one test so a flipped boundary is caught (0.8 ≠ 0.5).
    #[test]
    fn keyframe_cross_drift_axis_switch_boundary_rl_vs_tb() {
        let opts = drift_opts(Some(vec![vec![
            kfp(0.2, 0.1, 0.0),
            kfp(0.7, 0.9, 0.5),
            kfp(0.2, 0.1, 1.0),
        ]]));
        let rl = keyframe_cross_drift(&opts, 0.5, 1.0, 1).expect("Some")[0]; // RL → y
        let tb = keyframe_cross_drift(&opts, 0.5, 2.0, 1).expect("Some")[0]; // TB → x
        approx(
            rl,
            KEYFRAME_DRIFT_GAIN * 0.8,
            "RL (<1.5) ⇒ y-wobble × gain (Δy=0.8)",
        );
        approx(
            tb,
            KEYFRAME_DRIFT_GAIN * 0.5,
            "TB (>=1.5) ⇒ x-wobble × gain (Δx=0.5)",
        );
    }

    /// #255: loop-close guarantee — for **any** wobble track, `drift(0)` and `drift(1)`
    /// are both ≈0 so the loop seam (`t=1→0`) does not jump. Several clusters with
    /// distinct wobble shapes so this is not an accident of one track.
    #[test]
    fn keyframe_cross_drift_loop_closes_t0_and_t1_are_zero() {
        let opts = drift_opts(Some(vec![
            // asymmetric wobble (peak off-center), endpoints differ.
            vec![kfp(0.5, 0.1, 0.0), kfp(0.5, 0.9, 0.3), kfp(0.5, 0.4, 1.0)],
            // x-and-y wobble.
            vec![kfp(0.2, 0.2, 0.0), kfp(0.7, 0.8, 0.5), kfp(0.3, 0.1, 1.0)],
        ]));
        for &dir in &[0.0f32, 2.0] {
            let d0 = keyframe_cross_drift(&opts, 0.0, dir, 2).expect("Some");
            let d1 = keyframe_cross_drift(&opts, 1.0, dir, 2).expect("Some");
            for c in 0..2 {
                approx(d0[c], 0.0, &format!("loop-close dir={dir} cluster {c} t=0"));
                approx(d1[c], 0.0, &format!("loop-close dir={dir} cluster {c} t=1"));
            }
        }
    }

    /// #255: the gain constant is applied exactly once. A wobble whose raw detrended
    /// value at t=0.5 is 0.6 must come back as 0.6 × [`KEYFRAME_DRIFT_GAIN`], pinning
    /// that the const is not mis-multiplied (e.g. squared, or omitted).
    #[test]
    fn keyframe_cross_drift_applies_subtle_gain() {
        let opts = drift_opts(Some(vec![vec![
            kfp(0.5, 0.2, 0.0),
            kfp(0.5, 0.8, 0.5),
            kfp(0.5, 0.2, 1.0),
        ]]));
        let got = keyframe_cross_drift(&opts, 0.5, 0.0, 1).expect("Some")[0];
        let raw_detrended = 0.6f32; // 0.8 − (0.2·0.5 + 0.2·0.5)
        approx(
            got,
            KEYFRAME_DRIFT_GAIN * raw_detrended,
            "gain applied once",
        );
    }

    /// #255 A7: an empty per-cluster track yields drift 0 for that index, while a
    /// real wobble track at another index drifts. Index 0 empty, index 1 wobble.
    #[test]
    fn keyframe_cross_drift_empty_track_index_is_zero() {
        let opts = drift_opts(Some(vec![
            vec![], // cluster 0: no track → drift 0
            vec![kfp(0.5, 0.2, 0.0), kfp(0.5, 0.8, 0.5), kfp(0.5, 0.2, 1.0)], // y wobble
        ]));
        let drift = keyframe_cross_drift(&opts, 0.5, 0.0, 2).expect("Some");
        approx(drift[0], 0.0, "empty track ⇒ drift 0");
        approx(
            drift[1],
            KEYFRAME_DRIFT_GAIN * 0.6,
            "real wobble ⇒ y-drift gain·0.6 at t=0.5",
        );
    }

    /// #255 A8: the output length is always `n_clusters`, independent of the number
    /// of tracks supplied. Fewer tracks than clusters ⇒ trailing clusters get 0;
    /// more tracks than clusters ⇒ the surplus tracks are ignored.
    #[test]
    fn keyframe_cross_drift_output_len_is_n_clusters_not_tracks_len() {
        // 1 wobble track, 3 clusters → len 3, only index 0 drifts (at t=0.5).
        let few = drift_opts(Some(vec![vec![
            kfp(0.5, 0.2, 0.0),
            kfp(0.5, 0.8, 0.5),
            kfp(0.5, 0.2, 1.0),
        ]]));
        let d = keyframe_cross_drift(&few, 0.5, 0.0, 3).expect("Some");
        assert_eq!(d.len(), 3, "len must equal n_clusters");
        approx(d[0], KEYFRAME_DRIFT_GAIN * 0.6, "cluster 0 wobbles");
        approx(d[1], 0.0, "no track ⇒ 0");
        approx(d[2], 0.0, "no track ⇒ 0");

        // 3 tracks, 2 clusters → len 2 (surplus track ignored, no panic). Length is
        // axis/value-independent so plain 2-key tracks suffice here.
        let many = drift_opts(Some(vec![
            vec![kfp(0.5, 0.2, 0.0), kfp(0.5, 0.8, 1.0)],
            vec![kfp(0.5, 0.1, 0.0), kfp(0.5, 0.5, 1.0)],
            vec![kfp(0.5, 0.0, 0.0), kfp(0.5, 1.0, 1.0)],
        ]));
        let d = keyframe_cross_drift(&many, 0.5, 0.0, 2).expect("Some");
        assert_eq!(d.len(), 2, "len must be n_clusters, surplus tracks ignored");
    }

    /// #255 A9: a NaN `t` must not panic. `interpolate_keyframe_track` clamps NaN to
    /// the head value, so `c_t` equals `c_0` and the detrend yields ~0 (no jump).
    #[test]
    fn keyframe_cross_drift_nan_t_does_not_panic_and_is_zero() {
        let opts = drift_opts(Some(vec![vec![
            kfp(0.5, 0.2, 0.0),
            kfp(0.5, 0.8, 0.5),
            kfp(0.5, 0.2, 1.0),
        ]]));
        let d = keyframe_cross_drift(&opts, f32::NAN, 0.0, 1).expect("Some");
        approx(d[0], 0.0, "NaN t ⇒ head value clamped ⇒ drift 0");
    }

    // ---- #255: pack_render_data off+13 carries the per-cluster drift ----

    /// Call the packer with a fixed orb-1 layout. `off = 16 + 16*i`; off+13 is drift.
    #[allow(clippy::too_many_arguments)]
    fn pack_with_drift(clusters: &[Cluster], n_orbs: usize, drift: Option<&[f32]>) -> Vec<f32> {
        pack_render_data(
            clusters,
            [0, 0, 0, 255],
            32.0,
            0.5,
            0.0, // direction_id (LR)
            2.0,
            7, // seed
            n_orbs,
            1.0,
            0.0,
            true,
            0.5,
            0.5,
            drift,
        )
    }

    /// #255 B10: with `cross_drift = None` every orb's off+13 word is exactly 0.0 —
    /// the minimal byte-level proof that the no-track path is unchanged.
    #[test]
    fn pack_off13_zero_when_cross_drift_none() {
        let clusters = vec![
            track_cluster([200, 100, 50], 0.3, 0.4, 0.5),
            track_cluster([50, 100, 200], 0.7, 0.6, 0.5),
        ];
        let n_orbs = 6;
        let pack = pack_with_drift(&clusters, n_orbs, None);
        for i in 0..n_orbs {
            let off = 16 + 16 * i;
            assert_eq!(pack[off + 13], 0.0, "orb {i} off+13 must be 0.0 with None");
        }
    }

    /// #255 B11: with a `Some` per-cluster drift, each orb's off+13 word carries the
    /// drift value of **its** cluster (looked up by `cluster_idx`). Verified by
    /// reading off+0..2 (the orb's color) back to which cluster it borrowed.
    #[test]
    fn pack_off13_carries_per_cluster_drift_by_cluster_idx() {
        // Two clearly distinct colors so we can recover cluster_idx from the packed color.
        let clusters = vec![
            track_cluster([200, 0, 0], 0.3, 0.4, 0.5),
            track_cluster([0, 0, 200], 0.7, 0.6, 0.5),
        ];
        let drift = [0.11f32, 0.22f32];
        let n_orbs = 8;
        let pack = pack_with_drift(&clusters, n_orbs, Some(&drift));
        for i in 0..n_orbs {
            let off = 16 + 16 * i;
            // Recover which cluster this orb borrowed from its packed red channel.
            let red = pack[off];
            let cluster_idx = if (red - 200.0 / 255.0).abs() < 1e-3 {
                0
            } else {
                1
            };
            assert!(
                (pack[off + 13] - drift[cluster_idx]).abs() < 1e-6,
                "orb {i} (cluster {cluster_idx}) off+13 must be drift[{cluster_idx}]={}",
                drift[cluster_idx]
            );
        }
    }

    /// #255 B12: B案 = per-cluster delta, **not** a per-orb stripe. With one cluster
    /// and many orbs, every orb shares the single cluster's delta in off+13, while
    /// off+8 (cross_axis, the uniform scatter) still spreads across orbs. This pins
    /// "same cluster ⇒ same delta, scatter preserved ⇒ no stripe" purely in unit land.
    #[test]
    fn pack_drift_is_per_cluster_not_striped() {
        let clusters = vec![track_cluster([120, 120, 120], 0.5, 0.5, 1.0)];
        let drift = [0.5f32];
        let n_orbs = 8;
        let pack = pack_with_drift(&clusters, n_orbs, Some(&drift));
        let mut cross_axes = Vec::new();
        for i in 0..n_orbs {
            let off = 16 + 16 * i;
            assert!(
                (pack[off + 13] - 0.5).abs() < 1e-6,
                "orb {i} shares the single cluster's delta (0.5)"
            );
            cross_axes.push(pack[off + 8]);
        }
        // The cross_axis scatter must NOT collapse to one value (that would be a stripe).
        let first = cross_axes[0];
        assert!(
            cross_axes.iter().any(|&c| (c - first).abs() > 1e-3),
            "cross_axis must still scatter across orbs (delta shared, scatter kept): {cross_axes:?}"
        );
    }
}
