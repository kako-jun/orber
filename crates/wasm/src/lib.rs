//! WASM bindings for orber-core. Exposes the rendering pipeline to browsers.
//!
//! 画像デコードは JS 側に任せる: 呼び出し側は `<canvas>` / `ImageData` で
//! 生 RGB バイトを取り出して `WasmParams.source_rgb` に詰めて渡す。core クレート
//! は wasm バンドルサイズ削減のため PNG デコード以外を積まない。
//!
//! ## API の責務分離（#225 以降）
//!
//! CPU 描画は撲滅され、wasm は **データ供給だけ**を担う。実描画は
//! ブラウザ側の WebGL2/WebGPU が行う:
//!
//! - `get_render_data`: バッチ `spec_idx` 番目の per-orb 決定論データ（色 / phase /
//!   呼吸位相 / cross_axis / style / speed_mult / 回転 + ヘッダ）を `Float32Array`
//!   1 本にパックして返す。WebGL fragment shader が各 t のフレームを描く。
//! - `get_glyph_sdf`: グリフ 1 文字の SDF テクスチャ（`Uint8Array`）を返す。
//! - `glyph_supported`: 同梱フォントに文字が収録されているかの判定。

const MAX_DIM: u32 = 8192;

use orber_core::animate::{pack_render_data_for_webgl, MotionDirection, MotionSpeed};
use orber_core::cluster::{derive_background_rgba, drop_dominant, extract_clusters, Cluster};
use orber_core::glyph::{has_glyph, render_glyph_sdf, GlyphFontId};
use orber_core::orb::OrbShape;
use orber_core::style::SoftnessPreset;
use orber_core::variations::{
    random_batch_specs, VariationSpec, GUI_VIDEO_COUNT_DEFAULT, GUI_VIDEO_DIRECTIONS,
    GUI_VIDEO_SPEEDS,
};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use wasm_bindgen::prelude::*;

/// orb 数の上限。core::animate::MAX_ORB_COUNT と一致させる必要がある。
const MAX_ORB_COUNT: usize = 1024;

/// WebGL2 fragment shader 側の uniform array サイズ上限
/// (`web/src/lib/orberGl.ts::MAX_ORBS`)。ここで超過を早期エラーにし、
/// shader アップロード時に黙って切り詰められるのを防ぐ。GUI の
/// `random_ranges::COUNT_MAX = 50` を網羅する余裕として 64 を採る。
/// 将来 GUI の COUNT_MAX を増やす場合は両方同時に上げること。
// SYNC WITH web/src/lib/orberGl.ts::MAX_ORBS
const GL_RENDERER_MAX_ORBS: usize = 64;

/// パニック時にブラウザコンソールへスタックトレースを出すためのフック。
#[wasm_bindgen(start)]
pub fn init_panic_hook() {
    console_error_panic_hook::set_once();
}

/// JS 側から渡されるレンダリングパラメータ。
///
/// `source_rgb` は **正確に** `source_width * source_height * 3` バイトの
/// 行優先 (R,G,B,R,G,B,...) でなければならない。
/// `seed` は f64 で受ける（JS の Number 互換、BigInt を強制したくない）。
/// 内部で `as u64` キャスト。実用上 `Number.MAX_SAFE_INTEGER` までは無損失。
#[derive(Deserialize)]
pub struct WasmParams {
    pub source_rgb: Vec<u8>,
    pub source_width: u32,
    pub source_height: u32,
    pub k: usize,
    pub width: u32,
    pub height: u32,
    pub seed: f64,
    pub direction: String,
    pub speed: String,
    pub count: usize,
    pub orb_size: f32,
    pub blur: f32,
    pub shape: String,
    /// Glyph 文字（`shape == "glyph"` のときのみ意味を持つ）。Unicode scalar
    /// 1 文字。空文字や複数 scalar は呼び出し側で reject すること
    /// （wasm 側では先頭 char をそのまま採用する）。Phase B (#55) で追加。
    /// 既存呼び出しの後方互換のため `default = ""`、`default` で `'☆'` を採用しない
    /// （Glyph 経路に入る前提で呼び出し側が必ず指定するため）。
    #[serde(default)]
    pub glyph_char: String,
    /// `count` の preset 上書き。`""` で無視（spec.count を使う）。
    /// Phase B (#55) で追加。`"low" | "mid" | "high"` のいずれかなら
    /// 10/20/30 を spec.count に上書きしてからレンダリングする。
    #[serde(default)]
    pub count_preset: String,
    /// `speed` の preset 上書き。`""` で無視（spec.speed と GUI_VIDEO_SPEEDS を使う）。
    /// Phase B (#55) で追加。`"slow" | "mid" | "fast"` のみ受け付ける。
    #[serde(default)]
    pub speed_preset: String,
    /// `softness` の preset。`""` で `Mid` (既存挙動と同値)。Phase B (#55) で追加。
    #[serde(default)]
    pub softness_preset: String,
    /// Glyph 形状時に per-orb 回転をアニメーションさせるか（#136）。
    /// `true` で従来挙動、`false` で全 t において base_angle を保つ静止描画。
    /// Circle 形状では使われない。`#[serde(default = "default_glyph_rotate")]`
    /// で省略時は `true`（従来挙動互換）。既存の wasm caller が `glyph_rotate`
    /// フィールドを送っていなくても `true` でデシリアライズされるため影響を受けない。
    #[serde(default = "default_glyph_rotate")]
    pub glyph_rotate: bool,
}

/// `glyph_rotate` の serde default。既存呼び出しが省略しても従来挙動を保つために `true`。
fn default_glyph_rotate() -> bool {
    true
}

// Pure parsers/validators return String errors so they can be unit-tested on
// the host (non-wasm) target where JsError can't be constructed.

/// Phase B (#55): preset 文字列を `Option<MotionSpeed>` に変換。
///
/// UI 経路は `slow` / `mid` / `fast` の **3 値のみ** を受理する。空文字だけが
/// identity（= 上書きしない、`Ok(None)`）で、`spec.speed` と `GUI_VIDEO_SPEEDS`
/// の固定割当を温存する。明示選択時は:
/// - `slow` => `VerySlow`
/// - `mid` => `Slow`
/// - `fast` => `Mid`
/// - `Fast` は CLI 専用に格下げされ、GUI では露出しない。
fn parse_speed_preset(s: &str) -> Result<Option<MotionSpeed>, String> {
    match s {
        // identity: spec.speed / GUI_VIDEO_SPEEDS を温存
        "" => Ok(None),
        "slow" => Ok(Some(MotionSpeed::VerySlow)),
        "mid" => Ok(Some(MotionSpeed::Slow)),
        "fast" => Ok(Some(MotionSpeed::Mid)),
        other => Err(format!(
            "invalid speed_preset: {other} (expected one of '' / slow / mid / fast)"
        )),
    }
}

/// Phase B (#55): count preset 文字列を絶対値に変換。`""` は `Ok(None)` で
/// 「上書きしない（spec.count を使う）」を意味する。値は GUI 仕様に合わせて
/// low=10 / mid=20 / high=30 で固定。
fn parse_count_preset(s: &str) -> Result<Option<usize>, String> {
    match s {
        "" => Ok(None),
        "low" => Ok(Some(10)),
        "mid" => Ok(Some(20)),
        "high" => Ok(Some(30)),
        other => Err(format!(
            "invalid count_preset: {other} (expected one of '' / low / mid / high)"
        )),
    }
}

/// Phase B (#55): softness preset 文字列を `SoftnessPreset` に変換。空文字 /
/// "mid" は既存挙動と完全同値の `Mid`。
fn parse_softness_preset(s: &str) -> Result<SoftnessPreset, String> {
    match s {
        "" | "mid" => Ok(SoftnessPreset::Mid),
        "low" => Ok(SoftnessPreset::Low),
        "high" => Ok(SoftnessPreset::High),
        other => Err(format!(
            "invalid softness_preset: {other} (expected one of '' / low / mid / high)"
        )),
    }
}

/// Phase B (#55): "glyph" 形状時の文字列から先頭 char を取り出す。空文字なら
/// エラー。複数 char でも先頭の Unicode scalar のみ採用する
/// （UI 側で 1 文字制限済みの想定）。
fn first_char_of(s: &str) -> Result<char, String> {
    s.chars()
        .next()
        .ok_or_else(|| "glyph_char is empty (expected exactly 1 character)".to_string())
}

fn parse_shape(s: &str, glyph_char: &str) -> Result<OrbShape, String> {
    // OrbShape::Aquarelle はパラメータが多いので wasm 入口では `circle` / `glyph` のみ受ける。
    // Aquarelle は将来必要になったら別 API を生やす。
    match s {
        "circle" => Ok(OrbShape::Circle),
        "glyph" => {
            let ch = first_char_of(glyph_char)?;
            Ok(OrbShape::Glyph {
                ch,
                font: GlyphFontId::NotoSymbols2,
            })
        }
        other => Err(format!(
            "invalid shape: {other} (expected 'circle' or 'glyph')"
        )),
    }
}

fn build_source_image(p: &mut WasmParams) -> Result<image::RgbImage, String> {
    let rgb = std::mem::take(&mut p.source_rgb);
    image::RgbImage::from_raw(p.source_width, p.source_height, rgb).ok_or_else(|| {
        "source_rgb length does not match source_width * source_height * 3".to_string()
    })
}

/// kmeans 結果のキャッシュ。同じソース画像 + 同じ K なら kmeans を skip する。
///
/// Android 計測 (kako-jun, 2026-05-01) で `extract_clusters` が 1 spec あたり
/// ~3 秒かかり、12 stills + 4 mp4 = 16 呼び出しで合計 ~50 秒のロスになって
/// いた（PC では合計 ~1 秒）。kmeans 結果はソース画像が変わらない限り同じ
/// なので、(source_rgb の長さ + 4 隅 8 byte サンプル + width + height + k)
/// を fingerprint にして再利用する。
///
/// レビュー S1: 旧実装は `static mut Option<CachedClusters>` で
/// `#[allow(static_mut_refs)]` を必要としていた。Rust 2024 以降の lint 強化で
/// 将来の事故源になるため `OnceLock<WasmSingleThreadCell<...>>` に移行する。
/// **wasm は single-threaded** なので `RefCell` の borrow 衝突は構造的に
/// 起きないが、`RefCell` 自体が `Sync` ではないため、wasm 専用の薄い
/// ラッパで `Sync`/`Send` を手動 impl する（unsafe 境界は **このラッパ
/// 1 か所だけ**に閉じ込める）。worker を複数起動しても各 worker は独立した
/// wasm モジュールインスタンスを持つので static 共有は発生しない。
struct CachedClusters {
    fingerprint: u64,
    clusters_full: Vec<Cluster>,
    bg: [u8; 4],
    clusters: Vec<Cluster>,
}

/// wasm シングルスレッド前提の `RefCell` ラッパ。
///
/// `OnceLock<T>` の `T` は `Sync` を要求するが、`RefCell<T>` は `Sync` を
/// 提供しないため、そのままでは `OnceLock<RefCell<...>>` を `static` に
/// 置けない。wasm32 ターゲットでは static の共有が実質スレッド境界を
/// 越えないので、ここで `Sync`/`Send` を手動 impl する。これで以後は
/// `static mut` も `#[allow(static_mut_refs)]` も不要になる。
struct WasmSingleThreadCell<T>(RefCell<T>);
// SAFETY: wasm32 はシングルスレッド (Web Worker は別 wasm インスタンスを持つ)。
// 同一 wasm インスタンス内で `RefCell` を多スレッドから同時アクセスすることは
// 構造的に起きない。borrow 衝突は通常の RefCell ルールで実行時に検出される。
unsafe impl<T> Sync for WasmSingleThreadCell<T> {}
unsafe impl<T> Send for WasmSingleThreadCell<T> {}

impl<T> WasmSingleThreadCell<T> {
    fn new(v: T) -> Self {
        Self(RefCell::new(v))
    }
    fn borrow_mut(&self) -> std::cell::RefMut<'_, T> {
        self.0.borrow_mut()
    }
}

fn source_cache() -> &'static WasmSingleThreadCell<Option<CachedClusters>> {
    static CELL: OnceLock<WasmSingleThreadCell<Option<CachedClusters>>> = OnceLock::new();
    CELL.get_or_init(|| WasmSingleThreadCell::new(None))
}

fn fingerprint(rgb: &[u8], w: u32, h: u32, k: usize) -> u64 {
    // 完全一致は不要。長さ + dims + k + 4 隅サンプルで衝突は実用上ゼロ。
    let mut acc: u64 = 0xcbf29ce484222325; // FNV offset basis
    let mix = |acc: u64, b: u64| acc.wrapping_mul(0x100000001b3).wrapping_add(b);
    acc = mix(acc, rgb.len() as u64);
    acc = mix(acc, w as u64);
    acc = mix(acc, h as u64);
    acc = mix(acc, k as u64);
    if rgb.len() >= 12 {
        for i in 0..3 {
            acc = mix(acc, rgb[i] as u64);
            acc = mix(acc, rgb[rgb.len() / 2 + i] as u64);
            acc = mix(acc, rgb[rgb.len() - 1 - i] as u64);
            acc = mix(acc, rgb[rgb.len() / 4 + i] as u64);
        }
    }
    acc
}

/// kmeans 結果（clusters_full / bg / clusters）を取得する。同じソース画像なら
/// キャッシュヒットして O(1)、違う画像なら kmeans 実行 + キャッシュ更新。
///
/// レビュー S1: `static mut SOURCE_CACHE` を `OnceLock<WasmSingleThreadCell<...>>`
/// 経由に切り替え。`unsafe` ブロックも `#[allow(static_mut_refs)]` も不要になる。
type ClustersBundle = (Vec<Cluster>, [u8; 4], Vec<Cluster>);

fn get_or_build_clusters(p: &mut WasmParams) -> Result<ClustersBundle, String> {
    let fp = fingerprint(&p.source_rgb, p.source_width, p.source_height, p.k);
    {
        let cache = source_cache().borrow_mut();
        if let Some(c) = cache.as_ref() {
            if c.fingerprint == fp {
                return Ok((c.clusters_full.clone(), c.bg, c.clusters.clone()));
            }
        }
    }
    let source = build_source_image(p)?;
    let clusters_full =
        extract_clusters(&source, p.k).map_err(|e| format!("cluster extraction failed: {e}"))?;
    let bg = derive_background_rgba(&clusters_full);
    let clusters = drop_dominant(&clusters_full);
    let cached = CachedClusters {
        fingerprint: fp,
        clusters_full: clusters_full.clone(),
        bg,
        clusters: clusters.clone(),
    };
    *source_cache().borrow_mut() = Some(cached);
    Ok((clusters_full, bg, clusters))
}

fn deserialize_params(params_js: JsValue) -> Result<WasmParams, String> {
    let p: WasmParams = serde_wasm_bindgen::from_value(params_js)
        .map_err(|e| format!("failed to parse params: {e}"))?;
    validate_params(&p)?;
    Ok(p)
}

fn validate_params(p: &WasmParams) -> Result<(), String> {
    if !p.seed.is_finite() || p.seed < 0.0 {
        return Err(format!(
            "seed must be a non-negative finite number, got {}",
            p.seed
        ));
    }
    if p.source_width == 0 || p.source_height == 0 {
        return Err("source_width / source_height must be > 0".to_string());
    }
    if p.width == 0 || p.height == 0 {
        return Err("width / height must be > 0".to_string());
    }
    if p.width > MAX_DIM || p.height > MAX_DIM {
        return Err(format!(
            "width / height must be <= {MAX_DIM}, got {}x{}",
            p.width, p.height
        ));
    }
    if p.source_width > MAX_DIM || p.source_height > MAX_DIM {
        return Err(format!(
            "source_width / source_height must be <= {MAX_DIM}, got {}x{}",
            p.source_width, p.source_height
        ));
    }
    Ok(())
}

fn err_to_js(s: String) -> JsError {
    JsError::new(&s)
}

/// バッチ index に対応する direction を返す。
///
/// 静止画タイル領域 (`spec_idx < still_count`) では spec の direction を
/// そのまま使い、動画タイル領域 (`spec_idx >= still_count`) では
/// `GUI_VIDEO_DIRECTIONS` の対応 index で上書きする。これにより GUI 経路
/// では動画 4 枚に LR/RL/TB/BT が 1 枚ずつ重複なく割り当てられる。
/// `get_render_data` から呼ばれ、各 spec_idx の direction を決める。
fn direction_for_spec_idx(
    spec_idx: usize,
    still_count: usize,
    spec: &VariationSpec,
) -> MotionDirection {
    if spec_idx >= still_count {
        let video_idx = spec_idx - still_count;
        debug_assert!(video_idx < GUI_VIDEO_COUNT_DEFAULT);
        GUI_VIDEO_DIRECTIONS[video_idx]
    } else {
        spec.direction
    }
}

/// バッチ index に対応する speed を返す。
///
/// 静止画タイル領域では spec.speed をそのまま使い、動画タイル領域では
/// `GUI_VIDEO_SPEEDS` の対応 index で上書きする。これにより GUI 経路の
/// 動画 4 枚は VerySlow / Slow / VerySlow / Slow と必ずばらけて、
/// 「4 つ全部速い / 全部遅い」のガチャ感低下を防ぐ (#77)。
///
/// `direction_for_spec_idx` と同じ責務分担で、`get_render_data` から呼ばれる。
fn speed_for_spec_idx(spec_idx: usize, still_count: usize, spec: &VariationSpec) -> MotionSpeed {
    if spec_idx >= still_count {
        let video_idx = spec_idx - still_count;
        debug_assert!(video_idx < GUI_VIDEO_COUNT_DEFAULT);
        GUI_VIDEO_SPEEDS[video_idx]
    } else {
        spec.speed
    }
}

/// バッチ N 枚のうち `spec_idx` 番目の描画に必要な per-orb データをパックして返す。
///
/// core で per-orb の決定論パラメータと clusters / 背景色を計算し、Float32Array
/// 1 本にエンコードして JS に渡す。GPU 側（WebGL2 fragment shader）で各 t における
/// フレームを per-pixel ループ + Source-Over 合成で描く（#225 で CPU 描画は撲滅され、
/// wasm はこのデータ供給と `get_glyph_sdf` だけを担う）。
///
/// `random_batch_specs(seed, total, still_count)` で spec 列を再構築するので、
/// `spec_idx` 番目の spec / direction / speed / count / orb_size / blur / seed は
/// バッチ全体で決定論的に一致する。
///
/// per-orb の rng シーケンスは `orber_core::animate::generate_orb_params`
/// をそのまま使うので、core 側アニメーションと同じ seed なら同じ
/// (phase, phi_radius, phi_blur, phi_opacity, cross_axis, style, cluster_idx,
/// speed_mult, base_angle, rot_speed_signed) が得られる。
///
/// # Float32Array レイアウト
///
/// `[0..16]` ヘッダ:
/// - `[0..4]`: 背景 RGBA (0..1 正規化)
/// - `[4]`: base_radius_unit (px) = `min(w, h) * 0.25 * orb_size`
/// - `[5]`: base_blur (0..1) — `(spec.blur + softness.blur_offset()).clamp(0,1)` で
///   softness 軸を反映済み
/// - `[6]`: direction_id (0=LR, 1=RL, 2=TB, 3=BT)
/// - `[7]`: cycle_count (1 = VerySlow, 2 = Slow, 3 = Mid, 4 = Fast)
/// - `[8]`: n_orbs (整数を f32 として)
/// - `[9]`: softness_alpha_mul (0..1) — Phase B (#55)。Mid なら 0.55 (#205 後)
/// - `[10]`: shape_id (0=Circle, 1=Glyph) — Phase B (#55)
/// - `[11]`: glyph_rotate (1.0 = ON / 0.0 = OFF) — #136
/// - `[12]`: edge_softness (Glyph/image アーム smoothstep 幅、0.3..=1.0) — #205
/// - `[13..16]`: 予約（0 詰め）
///
/// `[16 + 16*i ..]` per orb i:
/// - `[+0..+3]`: color_rgb (0..1)
/// - `[+3]`: cluster_weight (0..1)
/// - `[+4]`: phase (0..1)
/// - `[+5]`: phi_radius (0..2π)
/// - `[+6]`: phi_blur (0..2π)
/// - `[+7]`: phi_opacity (0..2π)
/// - `[+8]`: cross_axis (0..1)
/// - `[+9]`: style_bit (0=rim, 1=soft)
/// - `[+10]`: speed_mult (1..3)
/// - `[+11]`: base_angle (0..2π)
/// - `[+12]`: rot_speed_signed (±1..±3)
/// - `[+13..+16]`: 予約（0 詰め）
#[wasm_bindgen]
pub fn get_render_data(
    params_js: JsValue,
    n: u32,
    spec_idx: u32,
) -> Result<js_sys::Float32Array, JsError> {
    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
    // Phase B (#55): shape = "circle" | "glyph"。glyph_char は Glyph のときに必須。
    let shape = parse_shape(&p.shape, &p.glyph_char).map_err(err_to_js)?;
    let count_override = parse_count_preset(&p.count_preset).map_err(err_to_js)?;
    let speed_override = parse_speed_preset(&p.speed_preset).map_err(err_to_js)?;
    let softness = parse_softness_preset(&p.softness_preset).map_err(err_to_js)?;

    let total = (n as usize).clamp(1, 50);
    let spec_idx = spec_idx as usize;
    if spec_idx >= total {
        return Err(JsError::new(&format!(
            "spec_idx {spec_idx} is out of range [0, {total})"
        )));
    }
    let still_count = total.saturating_sub(GUI_VIDEO_COUNT_DEFAULT);

    // kmeans は同じソース画像なら同じ結果になるのでキャッシュする。
    // Android では kmeans が ~3 秒かかり、これがタイル毎に走ることで
    // 12 stills + 4 mp4 = 16 呼び出しで合計 ~50 秒の律速になっていた。
    let (clusters_full, bg, clusters) = get_or_build_clusters(&mut p).map_err(err_to_js)?;
    let _ = clusters_full; // 現在は未使用だが将来 spec に diversity 等で使う可能性

    let specs = random_batch_specs(p.seed as u64, total, still_count);
    let spec = specs[spec_idx];
    let direction = direction_for_spec_idx(spec_idx, still_count, &spec);
    // Phase B (#55): UI から speed_preset が来ていれば、video 領域の
    // GUI_VIDEO_SPEEDS 固定割当も無視してユーザ指定値で全タイル統一する。
    // none なら従来どおり (still=spec.speed, video=GUI_VIDEO_SPEEDS)。
    let speed = match speed_override {
        Some(s) => s,
        None => speed_for_spec_idx(spec_idx, still_count, &spec),
    };

    let direction_id: f32 = match direction {
        MotionDirection::LeftToRight => 0.0,
        MotionDirection::RightToLeft => 1.0,
        MotionDirection::TopToBottom => 2.0,
        MotionDirection::BottomToTop => 3.0,
    };
    let cycle = speed.cycle_count() as f32;

    // Phase B (#55): count_preset があれば spec.count を上書きする。
    // 未指定なら従来どおり spec.count（random_ranges から 10..=50 一様抽選）。
    let effective_count = count_override.unwrap_or(spec.count);
    let n_orbs = effective_count
        .min(MAX_ORB_COUNT)
        .max(if clusters.is_empty() { 0 } else { 1 });

    // review S2: WebGL fragment shader の uniform 配列上限を超えると黙って
    // 切り詰められて視覚パリティが壊れる。発見が遅れないよう wasm 側で
    // 早期 throw する。Phase B でも GL_RENDERER_MAX_ORBS=64 を超えうる
    // count_preset (high=30) は未満。将来 high を 64 超に上げるならここを更新。
    if n_orbs > GL_RENDERER_MAX_ORBS {
        return Err(JsError::new(&format!(
            "n_orbs {n_orbs} exceeds WebGL renderer limit {GL_RENDERER_MAX_ORBS} (orberGl.ts MAX_ORBS と同期して上げること)"
        )));
    }

    let base_radius_unit = (p.width.min(p.height) as f32) * 0.25 * spec.orb_size.max(0.0);
    // Phase B (#55): softness.blur_offset() を base_blur に積算（core/animate と同式）。
    // #205 以降 Mid は +0.25 で blurry 寄りの新 default。
    let base_blur = (spec.blur + softness.blur_offset()).clamp(0.0, 1.0);
    let alpha_mul = softness.alpha_mul().clamp(0.0, 1.0);
    // #205: Glyph/image アーム smoothstep 幅を softness 連動。Circle は参照しない。
    let edge_softness = softness.edge_softness();
    let shape_id: f32 = match shape {
        OrbShape::Circle => 0.0,
        OrbShape::Glyph { .. } => 1.0,
        // Aquarelle は WebGL 経路非対応で parse_shape が弾く。Image (#217) は web が
        // 既存の SDF 直渡し経路で描くため、この wasm 入口（parse_shape は circle/glyph
        // のみ返す）には到達しない。いずれも念のため Circle 扱いにフォールバック
        // （パニックさせない）。Image を wasm 入口で受けるのは将来 Phase 3。
        _ => 0.0,
    };

    let buf = pack_render_data(
        &clusters,
        bg,
        base_radius_unit,
        base_blur,
        direction_id,
        cycle,
        spec.seed,
        n_orbs,
        alpha_mul,
        shape_id,
        p.glyph_rotate,
        edge_softness,
    );

    Ok(js_sys::Float32Array::from(buf.as_slice()))
}

/// core 側と共有の `generate_orb_params` 出力を使って、ヘッダ + per-orb
/// フィールドを Float32 ベクタに詰める。
///
/// WebGL path が core のアニメーションと別 RNG 列を持たないよう、乱数列は
/// ここで再実装せず `orber_core::animate::generate_orb_params` に委譲する。
// TODO(orber#future): pack_render_data の引数が 12 個に達した (#205 で edge_softness 追加)。
// Phase C で orb 形状軸が更に増えるなら struct で受けるリファクタを検討する。
#[allow(clippy::too_many_arguments)]
fn pack_render_data(
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
    pack_render_data_for_webgl(
        clusters,
        bg,
        base_radius_unit,
        base_blur,
        direction_id,
        cycle,
        seed,
        n_orbs,
        alpha_mul,
        shape_id,
        glyph_rotate,
        edge_softness,
    )
}

/// Glyph 1 文字の SDF texture を JS 側に返す wasm wrapper。
///
/// 実体は [`orber_core::glyph::render_glyph_sdf`] を参照。本関数は
/// その上に `(font, ch, size)` キャッシュ + size validation + JS 型変換だけを
/// 加える。size は `[16, 1024]` の範囲のみ受理（GUI は 256 固定の想定）。
/// 戻り値は長さ `size * size` の `Uint8Array`（行優先 SDF 0..255）。
/// 同梱フォントに無い文字は全 0 を返し panic しない。
#[wasm_bindgen]
pub fn get_glyph_sdf(ch: &str, size: u32) -> Result<js_sys::Uint8Array, JsError> {
    // 入力 validation。size は 16..=1024 の範囲を許可（GUI は 256 を使う想定）。
    if !(16..=1024).contains(&size) {
        return Err(JsError::new(&format!(
            "size must be in [16, 1024], got {size}"
        )));
    }
    let ch = first_char_of(ch).map_err(err_to_js)?;
    let bytes = glyph_sdf_bytes(GlyphFontId::NotoSymbols2, ch, size);
    Ok(js_sys::Uint8Array::from(&bytes[..]))
}

/// `has_glyph(NotoSymbols2, ch)` の wasm 公開ラッパ。UI の警告表示で使う。
/// 空文字や複数 char の場合は先頭 char のみ判定する（UI 側で 1 char 制限想定）。
#[wasm_bindgen]
pub fn glyph_supported(ch: &str) -> bool {
    match ch.chars().next() {
        Some(c) => has_glyph(GlyphFontId::NotoSymbols2, c),
        None => false,
    }
}

/// `(font, ch, size) -> Vec<u8>` の同一プロセス内キャッシュ。
/// HashMap キーは `(font, ch as u32, size)`。
///
/// レビュー S2: worker の `getRenderer` は (w, h) 切替時に renderer を作り直すが
/// この wasm 側 `glyph_sdf_cache` は wasm モジュール再ロード（HMR / 再起動）まで
/// 残る。同一 size + 同一 ch なら毎回同じ bytes が返るので決定論的に問題は
/// 無いが、開発時 HMR で再ロードしない場合に古いキャッシュエントリが残ったまま
/// になる点だけ注意。実運用では size=GLYPH_SDF_SIZE=256 に固定なので、
/// glyph 文字数 ×サイズ 1 通りで メモリ上限は数十エントリ程度。
type GlyphSdfKey = (GlyphFontId, u32, u32);

/// レビュー S1: `static mut CACHE` を `OnceLock<WasmSingleThreadCell<HashMap<...>>>`
/// に置換。`#[allow(static_mut_refs)]` を削除し、`unsafe` も無くなる。
fn glyph_sdf_cache() -> &'static WasmSingleThreadCell<HashMap<GlyphSdfKey, Vec<u8>>> {
    static CELL: OnceLock<WasmSingleThreadCell<HashMap<GlyphSdfKey, Vec<u8>>>> = OnceLock::new();
    CELL.get_or_init(|| WasmSingleThreadCell::new(HashMap::new()))
}

fn glyph_sdf_bytes(font: GlyphFontId, ch: char, size: u32) -> Vec<u8> {
    let key = (font, ch as u32, size);
    {
        let cache = glyph_sdf_cache().borrow_mut();
        if let Some(v) = cache.get(&key) {
            return v.clone();
        }
    }
    let v = render_glyph_sdf(font, ch, size);
    glyph_sdf_cache().borrow_mut().insert(key, v.clone());
    v
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// `source_cache()` のグローバル `RefCell` を触るテストを直列化するためのガード。
    /// native の `cargo test` は既定でテストを並列実行するため、複数テストが同時に
    /// `get_or_build_clusters()` → `borrow_mut()` すると `BorrowMutError` になる
    /// (#220)。production(wasm) は single-thread なのでこの直列化はテスト専用。
    static CACHE_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_speed_preset_handles_empty_and_values() {
        // 空文字だけが identity（None）。明示選択時は GUI の 3 段を
        // VerySlow / Slow / Mid にマップする。
        assert!(matches!(parse_speed_preset(""), Ok(None)));
        assert!(matches!(
            parse_speed_preset("slow"),
            Ok(Some(MotionSpeed::VerySlow))
        ));
        assert!(matches!(
            parse_speed_preset("mid"),
            Ok(Some(MotionSpeed::Slow))
        ));
        assert!(matches!(
            parse_speed_preset("fast"),
            Ok(Some(MotionSpeed::Mid))
        ));
        // M2: very-slow は UI 経路では受け付けない（CLI 専用、parse_speed が担当）。
        assert!(parse_speed_preset("very-slow").is_err());
        assert!(parse_speed_preset("xxx").is_err());
    }

    /// M1: count_preset='' のとき effective_count == spec.count を保つ。
    /// `parse_count_preset` が `None` を返し、`get_render_data` 内で
    /// `count_override.unwrap_or(spec.count)` がそのまま spec.count を採用する。
    #[test]
    fn count_preset_empty_or_unspecified_uses_spec_count() {
        let count_override = parse_count_preset("").unwrap();
        assert!(count_override.is_none());
        // identity 不変条件: count_override.unwrap_or(spec_count) == spec_count
        let spec_count: usize = 27;
        assert_eq!(count_override.unwrap_or(spec_count), spec_count);
    }

    /// M1: speed_preset='' のとき speed_for_spec_idx の戻り値（
    /// still=spec.speed / video=GUI_VIDEO_SPEEDS）を温存する。
    #[test]
    fn speed_preset_empty_uses_spec_idx() {
        let speed_override = parse_speed_preset("").unwrap();
        assert!(
            speed_override.is_none(),
            "speed_preset='' must be identity (None)"
        );
        // identity 経路: get_render_data の match arm が
        // `speed_for_spec_idx(spec_idx, still_count, &spec)` を採用する。
        let still_count = 8;
        let total = 12;
        let mut spec = synth_spec(MotionDirection::TopToBottom);
        spec.speed = MotionSpeed::Slow;
        // still 領域は spec.speed を保つ。
        for spec_idx in 0..still_count {
            assert_eq!(
                speed_for_spec_idx(spec_idx, still_count, &spec),
                MotionSpeed::Slow
            );
        }
        // video 領域は GUI_VIDEO_SPEEDS の固定割当を保つ。
        for spec_idx in still_count..total {
            assert_eq!(
                speed_for_spec_idx(spec_idx, still_count, &spec),
                GUI_VIDEO_SPEEDS[spec_idx - still_count]
            );
        }
    }

    /// M1: softness_preset='' のとき SoftnessPreset::Mid と一致する（identity）。
    #[test]
    fn softness_preset_empty_is_mid_identity() {
        assert_eq!(parse_softness_preset("").unwrap(), SoftnessPreset::Mid);
        assert_eq!(parse_softness_preset("mid").unwrap(), SoftnessPreset::Mid);
        // Mid は alpha_mul=1.0 / blur_offset=0.0 で既存挙動と完全同値であることが
        // crates/core/src/style.rs の regression test で固定されている。
    }

    #[test]
    fn parse_count_preset_table() {
        assert_eq!(parse_count_preset("").unwrap(), None);
        assert_eq!(parse_count_preset("low").unwrap(), Some(10));
        assert_eq!(parse_count_preset("mid").unwrap(), Some(20));
        assert_eq!(parse_count_preset("high").unwrap(), Some(30));
        assert!(parse_count_preset("xxx").is_err());
    }

    #[test]
    fn parse_softness_preset_table() {
        assert_eq!(parse_softness_preset("").unwrap(), SoftnessPreset::Mid);
        assert_eq!(parse_softness_preset("mid").unwrap(), SoftnessPreset::Mid);
        assert_eq!(parse_softness_preset("low").unwrap(), SoftnessPreset::Low);
        assert_eq!(parse_softness_preset("high").unwrap(), SoftnessPreset::High);
        assert!(parse_softness_preset("xxx").is_err());
    }

    #[test]
    fn parse_shape_circle_and_glyph() {
        assert!(matches!(parse_shape("circle", ""), Ok(OrbShape::Circle)));
        // glyph では glyph_char が必須。空はエラー。
        assert!(parse_shape("glyph", "").is_err());
        let g = parse_shape("glyph", "☆").unwrap();
        assert!(matches!(g, OrbShape::Glyph { ch, .. } if ch == '☆'));
        // Aquarelle は wasm 入口で受けない。
        assert!(parse_shape("aquarelle", "").is_err());
        assert!(parse_shape("", "").is_err());
    }

    #[test]
    fn glyph_sdf_paints_known_char() {
        // ☆ は同梱 NotoSansSymbols2 にある。inside 側サンプルが一定数以上あること
        // （少なくとも全ピクセルの 5% は 0.5 超になる想定）。
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '☆', 64);
        assert_eq!(bytes.len(), 64 * 64);
        let lit = bytes.iter().filter(|&&b| b > 127).count();
        assert!(
            lit > 64 * 64 / 20,
            "rendering ☆ at 64x64 should produce >=5% inside pixels, got {lit}"
        );
    }

    #[test]
    fn glyph_sdf_unknown_char_returns_empty() {
        // 絵文字 (Symbols 2 subset 外) は全 0 を返す。
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '\u{1F355}', 32);
        assert_eq!(bytes.len(), 32 * 32);
        assert!(bytes.iter().all(|&b| b == 0));
    }

    fn base_params() -> WasmParams {
        WasmParams {
            source_rgb: vec![0; 4 * 4 * 3],
            source_width: 4,
            source_height: 4,
            k: 4,
            width: 64,
            height: 64,
            seed: 42.0,
            direction: "lr".into(),
            speed: "slow".into(),
            count: 10,
            orb_size: 3.0,
            blur: 0.5,
            shape: "circle".into(),
            // Phase B (#55): 既存挙動互換のため空文字。
            glyph_char: String::new(),
            count_preset: String::new(),
            speed_preset: String::new(),
            softness_preset: String::new(),
            glyph_rotate: true,
        }
    }

    #[test]
    fn validate_rejects_negative_seed() {
        let mut p = base_params();
        p.seed = -1.0;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_rejects_nan_seed() {
        let mut p = base_params();
        p.seed = f64::NAN;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_rejects_zero_dimensions() {
        let mut p = base_params();
        p.width = 0;
        assert!(validate_params(&p).is_err());

        let mut p = base_params();
        p.source_height = 0;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_rejects_oversize_dimensions() {
        let mut p = base_params();
        p.width = MAX_DIM + 1;
        assert!(validate_params(&p).is_err());

        let mut p = base_params();
        p.source_width = 100_000;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_accepts_reasonable_params() {
        assert!(validate_params(&base_params()).is_ok());
    }

    fn synth_spec(direction: MotionDirection) -> VariationSpec {
        VariationSpec {
            direction,
            speed: MotionSpeed::Slow,
            count: 10,
            orb_size: 3.0,
            blur: 0.5,
            seed: 1,
            duration_ms: 0,
            kind: orber_core::variations::VariationKind::Png,
            label: "test",
        }
    }

    /// 静止画タイル領域では spec.direction をそのまま使う。
    #[test]
    fn direction_for_spec_idx_returns_spec_direction_for_still_range() {
        let still_count = 8;
        let spec = synth_spec(MotionDirection::TopToBottom);
        for spec_idx in 0..still_count {
            assert_eq!(
                direction_for_spec_idx(spec_idx, still_count, &spec),
                MotionDirection::TopToBottom,
                "still tile {spec_idx} must inherit spec.direction"
            );
        }
    }

    /// 動画タイル領域 (8..12) では GUI_VIDEO_DIRECTIONS で上書きされる。
    /// LR / RL / TB / BT が重複なく 1 枚ずつ割り当てられる。
    #[test]
    fn direction_for_spec_idx_overrides_video_range_with_gui_directions() {
        let still_count = 8;
        let total = 12;
        // spec.direction は何を入れても video 領域では無視される。
        let spec = synth_spec(MotionDirection::LeftToRight);
        let mut seen: Vec<MotionDirection> = Vec::new();
        for spec_idx in still_count..total {
            let dir = direction_for_spec_idx(spec_idx, still_count, &spec);
            assert_eq!(dir, GUI_VIDEO_DIRECTIONS[spec_idx - still_count]);
            seen.push(dir);
        }
        assert_eq!(seen.len(), GUI_VIDEO_COUNT_DEFAULT);
        // 重複がない（4 方向揃い踏み）。
        let mut sorted = seen.clone();
        sorted.sort_by_key(|d| format!("{d:?}"));
        sorted.dedup_by_key(|d| format!("{d:?}"));
        assert_eq!(sorted.len(), GUI_VIDEO_COUNT_DEFAULT);
    }

    /// 境界: spec_idx == still_count - 1 は静止 (spec.direction)、
    /// spec_idx == still_count は video (GUI_VIDEO_DIRECTIONS[0])。
    #[test]
    fn direction_for_spec_idx_boundary() {
        let still_count = 8;
        let spec = synth_spec(MotionDirection::BottomToTop);
        assert_eq!(
            direction_for_spec_idx(still_count - 1, still_count, &spec),
            MotionDirection::BottomToTop,
            "spec_idx == still_count - 1 is still range"
        );
        assert_eq!(
            direction_for_spec_idx(still_count, still_count, &spec),
            GUI_VIDEO_DIRECTIONS[0],
            "spec_idx == still_count is video range index 0"
        );
    }

    /// #77: 動画タイル領域の speed は GUI_VIDEO_SPEEDS で固定割当される。
    /// VerySlow / Slow が必ず 2 枚ずつ（4 タイルがガチャ感を保つ最小条件）。
    #[test]
    fn speed_for_spec_idx_overrides_video_range_with_assigned_speeds() {
        let still_count = 8;
        let total = 12;
        // spec.speed は何を入れても video 領域では無視される。
        let mut spec = synth_spec(MotionDirection::LeftToRight);
        spec.speed = MotionSpeed::Slow;
        let mut very_slow = 0;
        let mut slow = 0;
        for spec_idx in still_count..total {
            let s = speed_for_spec_idx(spec_idx, still_count, &spec);
            assert_eq!(s, GUI_VIDEO_SPEEDS[spec_idx - still_count]);
            match s {
                MotionSpeed::VerySlow => very_slow += 1,
                MotionSpeed::Slow => slow += 1,
                // GUI_VIDEO_SPEEDS は現在 VerySlow / Slow しか含まないので
                // この arm に来たら GUI_VIDEO_SPEEDS の定義変更ミス。
                // (Phase B でも GUI_VIDEO_SPEEDS は変更していない)
                MotionSpeed::Mid | MotionSpeed::Fast => {
                    panic!("GUI_VIDEO_SPEEDS unexpectedly contains Mid/Fast: {s:?}")
                }
            }
        }
        // 4 タイルで 2 + 2 のばらけが固定で保証される。
        assert_eq!(very_slow, 2, "must have exactly 2 VerySlow tiles");
        assert_eq!(slow, 2, "must have exactly 2 Slow tiles");
    }

    /// 静止画タイル領域では spec.speed をそのまま使う。
    #[test]
    fn speed_for_spec_idx_returns_spec_speed_for_still_range() {
        let still_count = 8;
        let mut spec = synth_spec(MotionDirection::TopToBottom);
        spec.speed = MotionSpeed::VerySlow;
        for spec_idx in 0..still_count {
            assert_eq!(
                speed_for_spec_idx(spec_idx, still_count, &spec),
                MotionSpeed::VerySlow,
                "still tile {spec_idx} must inherit spec.speed"
            );
        }
    }

    #[test]
    fn pack_render_data_matches_core_pack_helper() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = base_params();
        p.k = 2;
        p.source_width = 2;
        p.source_height = 2;
        p.source_rgb = vec![
            255, 0, 0, 255, 0, 0, //
            0, 0, 255, 0, 0, 255,
        ];
        let total = 12usize;
        let spec_idx = 3usize;
        let still_count = total - GUI_VIDEO_COUNT_DEFAULT;
        let (_, bg, clusters) = get_or_build_clusters(&mut p).expect("clusters should build");
        let specs = random_batch_specs(42, total, still_count);
        let spec = specs[spec_idx];
        let speed = speed_for_spec_idx(spec_idx, still_count, &spec);
        let direction = direction_for_spec_idx(spec_idx, still_count, &spec);
        let direction_id = match direction {
            MotionDirection::LeftToRight => 0.0,
            MotionDirection::RightToLeft => 1.0,
            MotionDirection::TopToBottom => 2.0,
            MotionDirection::BottomToTop => 3.0,
        };
        let softness = parse_softness_preset("").unwrap();
        let n_orbs = spec.count.clamp(1, MAX_ORB_COUNT);
        let buf = pack_render_data(
            &clusters,
            bg,
            (64f32.min(64.0)) * 0.25 * spec.orb_size.max(0.0),
            (spec.blur + softness.blur_offset()).clamp(0.0, 1.0),
            direction_id,
            speed.cycle_count() as f32,
            spec.seed,
            n_orbs,
            softness.alpha_mul().clamp(0.0, 1.0),
            1.0,
            true,
            softness.edge_softness(),
        );
        let expected = pack_render_data_for_webgl(
            &clusters,
            bg,
            (64f32.min(64.0)) * 0.25 * spec.orb_size.max(0.0),
            (spec.blur + softness.blur_offset()).clamp(0.0, 1.0),
            direction_id,
            speed.cycle_count() as f32,
            spec.seed,
            n_orbs,
            softness.alpha_mul().clamp(0.0, 1.0),
            1.0,
            true,
            softness.edge_softness(),
        );
        assert_eq!(buf, expected);
    }

    /// #205: get_render_data の header[12] に softness.edge_softness() がそのまま
    /// 入っていることを担保する。
    #[test]
    fn pack_render_data_header_includes_edge_softness() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = base_params();
        p.k = 2;
        p.source_width = 2;
        p.source_height = 2;
        p.source_rgb = vec![
            255, 0, 0, 255, 0, 0, //
            0, 0, 255, 0, 0, 255,
        ];
        let total = 12usize;
        let spec_idx = 0usize;
        let still_count = total - GUI_VIDEO_COUNT_DEFAULT;
        let (_, bg, clusters) = get_or_build_clusters(&mut p).expect("clusters should build");
        let specs = random_batch_specs(42, total, still_count);
        let spec = specs[spec_idx];
        let speed = speed_for_spec_idx(spec_idx, still_count, &spec);
        // Low / Mid / High それぞれで header[12] が edge_softness() と一致すること。
        for preset in [
            SoftnessPreset::Low,
            SoftnessPreset::Mid,
            SoftnessPreset::High,
        ] {
            let n_orbs = spec.count.clamp(1, MAX_ORB_COUNT);
            let buf = pack_render_data(
                &clusters,
                bg,
                (64f32) * 0.25 * spec.orb_size.max(0.0),
                (spec.blur + preset.blur_offset()).clamp(0.0, 1.0),
                0.0,
                speed.cycle_count() as f32,
                spec.seed,
                n_orbs,
                preset.alpha_mul().clamp(0.0, 1.0),
                1.0,
                true,
                preset.edge_softness(),
            );
            assert!((buf[12] - preset.edge_softness()).abs() < 1e-6);
        }
    }
}
