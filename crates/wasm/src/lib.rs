//! WASM bindings for orber-core. Exposes the rendering pipeline to browsers.
//!
//! 画像デコードは JS 側に任せる: 呼び出し側は `<canvas>` / `ImageData` で
//! 生 RGB バイトを取り出して `WasmParams.source_rgb` に詰めて渡す。core クレート
//! は wasm バンドルサイズ削減のため PNG デコード以外を積まない。
//!
//! ## API の責務分離
//!
//! - `generate_single`: 呼び出し側のパラメータ（seed/direction/speed/count/
//!   orb_size/blur/shape）をそのまま使って 1 フレームを描く。フル制御版。
//! - `generate_batch`: `random_batch_specs(params.seed, n, ceil(n/2))` で `n`
//!   件のランダム spec を生成し、各 spec ごとに 1 フレームを描く。前半 `ceil(n/2)`
//!   は `Png`、残りは `Mp4` の枠を維持する（当面 GUI 側は両方とも先頭フレームを
//!   PNG として表示する。`Mp4` の動画化は #40 / #50 で別途扱う）。`params` の
//!   うち direction/speed/count/orb_size/blur は **無視** され、ランダム値が
//!   使われる（shape / 入力画像 / 出力サイズ / k / seed のみ反映）。
//! - `generate_svg`: SVG は静的なので動き系パラメータは無視。orb_size/blur のみ反映。

const MAX_DIM: u32 = 8192;

use orber_core::animate::{render_frame, AnimateOptions, MotionDirection, MotionSpeed};
use orber_core::batch::{generate_batch as core_generate_batch, BatchInput};
use orber_core::cluster::{derive_background_rgba, drop_dominant, extract_clusters, Cluster};
use orber_core::glyph::{has_glyph, render_glyph_alpha_mask, GlyphFontId};
use orber_core::orb::OrbShape;
use orber_core::style::{render_svg as core_render_svg, ContrastPreset, StyleOptions};
use orber_core::variations::{
    random_batch_specs, VariationSpec, GUI_VIDEO_COUNT_DEFAULT, GUI_VIDEO_DIRECTIONS,
    GUI_VIDEO_SPEEDS,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::TAU;
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
    /// Glyph 文字（`shape == "glyph"` のときのみ意味を持つ）。grapheme 単位
    /// 1 文字。空文字や複数 grapheme は呼び出し側で reject すること
    /// （wasm 側では先頭 char をそのまま採用する）。Phase B (#55) で追加。
    /// 既存呼び出しの後方互換のため `default = ""`、`default` で `'☆'` を採用しない
    /// （Glyph 経路に入る前提で呼び出し側が必ず指定するため）。
    #[serde(default)]
    pub glyph_char: String,
    /// `count` の preset 上書き。`""` で無視（spec.count を使う）。
    /// Phase B (#55) で追加。`"low" | "mid" | "high"` のいずれかなら
    /// 10/20/35 を spec.count に上書きしてからレンダリングする。
    #[serde(default)]
    pub count_preset: String,
    /// `speed` の preset 上書き。`""` で無視（spec.speed と GUI_VIDEO_SPEEDS を使う）。
    /// Phase B (#55) で追加。`"very-slow" | "slow" | "mid" | "fast"`。
    #[serde(default)]
    pub speed_preset: String,
    /// `contrast` の preset。`""` で `Mid` (既存挙動と同値)。Phase B (#55) で追加。
    #[serde(default)]
    pub contrast_preset: String,
}

// Pure parsers/validators return String errors so they can be unit-tested on
// the host (non-wasm) target where JsError can't be constructed.

fn parse_direction(s: &str) -> Result<MotionDirection, String> {
    match s {
        "lr" => Ok(MotionDirection::LeftToRight),
        "rl" => Ok(MotionDirection::RightToLeft),
        "tb" => Ok(MotionDirection::TopToBottom),
        "bt" => Ok(MotionDirection::BottomToTop),
        other => Err(format!(
            "invalid direction: {other} (expected one of lr / rl / tb / bt)"
        )),
    }
}

fn parse_speed(s: &str) -> Result<MotionSpeed, String> {
    match s {
        "very-slow" => Ok(MotionSpeed::VerySlow),
        "slow" => Ok(MotionSpeed::Slow),
        "mid" => Ok(MotionSpeed::Mid),
        "fast" => Ok(MotionSpeed::Fast),
        other => Err(format!(
            "invalid speed: {other} (expected one of very-slow / slow / mid / fast)"
        )),
    }
}

/// Phase B (#55): preset 文字列を `Option<MotionSpeed>` に変換。
///
/// UI 経路は `slow` / `mid` / `fast` の **3 値のみ** を受理する。空文字 / `mid`
/// は identity（= 上書きしない、`Ok(None)`）を意味し、`spec.speed` と
/// `GUI_VIDEO_SPEEDS` の固定割当を温存する（M1/M2: identity 不変条件）。
/// `very-slow` は CLI 専用なのでここでは reject する（DESIGN.md §13 / CHANGELOG）。
fn parse_speed_preset(s: &str) -> Result<Option<MotionSpeed>, String> {
    match s {
        // identity: spec.speed / GUI_VIDEO_SPEEDS を温存
        "" | "mid" => Ok(None),
        "slow" => Ok(Some(MotionSpeed::Slow)),
        "fast" => Ok(Some(MotionSpeed::Fast)),
        // very-slow は UI 経路では受け付けない（CLI 専用）。`parse_speed` は
        // 4 値を受け付けるので CLI / generate_single 側で個別に使うこと。
        other => Err(format!(
            "invalid speed_preset: {other} (expected one of '' / slow / mid / fast)"
        )),
    }
}

/// Phase B (#55): count preset 文字列を絶対値に変換。`""` は `Ok(None)` で
/// 「上書きしない（spec.count を使う）」を意味する。値は GUI 仕様に合わせて
/// low=10 / mid=20 / high=35 で固定。
fn parse_count_preset(s: &str) -> Result<Option<usize>, String> {
    match s {
        "" => Ok(None),
        "low" => Ok(Some(10)),
        "mid" => Ok(Some(20)),
        "high" => Ok(Some(35)),
        other => Err(format!(
            "invalid count_preset: {other} (expected one of '' / low / mid / high)"
        )),
    }
}

/// Phase B (#55): contrast preset 文字列を `ContrastPreset` に変換。空文字 /
/// "mid" は既存挙動と完全同値の `Mid`。
fn parse_contrast_preset(s: &str) -> Result<ContrastPreset, String> {
    match s {
        "" | "mid" => Ok(ContrastPreset::Mid),
        "low" => Ok(ContrastPreset::Low),
        "high" => Ok(ContrastPreset::High),
        other => Err(format!(
            "invalid contrast_preset: {other} (expected one of '' / low / mid / high)"
        )),
    }
}

/// Phase B (#55): "glyph" 形状時の文字列から先頭 char を取り出す。空文字なら
/// エラー。複数 char（grapheme cluster ではなく Unicode scalar）でも先頭のみ
/// 採用する（UI 側で 1 文字制限済みの想定）。
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
/// では動画 4 枚に LR/RL/TB/BT が 1 枚ずつ重複なく割り当てられる
/// （core の `generate_batch` は spec.direction をそのまま使うので、この
/// 上書きは wasm 入口を通った GUI 経路でのみ発生することに注意）。
///
/// `generate_one_at_index` と `start_animation_for_batch_spec` の両方から
/// 呼ばれることで、両 API が同じ index に対して同じ direction を返すこと
/// を構造的に保証する（プレビュー静止 PNG と動画の direction が一致）。
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
/// `direction_for_spec_idx` と同じ責務分担で、core の `generate_batch`
/// は spec.speed をそのまま使うので、speed 固定割当は wasm 入口経由の
/// GUI 経路でのみ適用される。
fn speed_for_spec_idx(spec_idx: usize, still_count: usize, spec: &VariationSpec) -> MotionSpeed {
    if spec_idx >= still_count {
        let video_idx = spec_idx - still_count;
        debug_assert!(video_idx < GUI_VIDEO_COUNT_DEFAULT);
        GUI_VIDEO_SPEEDS[video_idx]
    } else {
        spec.speed
    }
}

fn encode_png_rgba(img: &image::RgbaImage) -> Result<Vec<u8>, JsError> {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    let mut buf = Vec::new();
    PngEncoder::new(&mut buf)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| JsError::new(&format!("PNG encode failed: {e}")))?;
    Ok(buf)
}

/// 入力画像 1 枚から 1 フレーム PNG を生成する。
///
/// パラメータの seed / direction / speed / count / orb_size / blur をそのまま
/// AnimateOptions に流して `t = 0` のフレームを描く。背景色は
/// `derive_background_rgba` で入力画像から自動決定する。
#[wasm_bindgen]
pub fn generate_single(params_js: JsValue) -> Result<js_sys::Uint8Array, JsError> {
    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
    let direction = parse_direction(&p.direction).map_err(err_to_js)?;
    // generate_single は呼び出し側が指定した speed をそのまま使う（preset 上書きは
    // get_render_data 側のみ）。Mid / Fast も Phase B から受け付ける。
    let speed = parse_speed(&p.speed).map_err(err_to_js)?;
    let shape = parse_shape(&p.shape, &p.glyph_char).map_err(err_to_js)?;
    let contrast = parse_contrast_preset(&p.contrast_preset).map_err(err_to_js)?;

    let source = build_source_image(&mut p).map_err(err_to_js)?;
    let clusters_full = extract_clusters(&source, p.k)
        .map_err(|e| JsError::new(&format!("cluster extraction failed: {e}")))?;
    let bg = derive_background_rgba(&clusters_full);
    let clusters = drop_dominant(&clusters_full);

    let opts = AnimateOptions {
        width: p.width,
        height: p.height,
        seed: p.seed as u64,
        direction,
        speed,
        count: Some(p.count),
        orb_size: p.orb_size,
        blur: p.blur,
        saturation: 1.0,
        background: bg,
        shape,
        contrast,
    };
    let frame = render_frame(&clusters, &opts, 0.0);
    let png = encode_png_rgba(&frame)?;
    Ok(js_sys::Uint8Array::from(&png[..]))
}

/// 入力画像 1 枚から `n` 個の variation PNG をランダム生成する。
///
/// 後半 [`GUI_VIDEO_COUNT_DEFAULT`] (= 4) 件を `VariationKind::Mp4`、残りを
/// `Png` にする。GUI では n = 12 (#61 で縦横共通に統一、前半 8 枚静止 +
/// 後半 4 枚動画) で運用されるため、「後半 4 枚は動画枠」になる。動画 4 枚
/// には `start_animation_for_batch_spec` で LR / RL / TB / BT が 1 枚ずつ
/// 重複なく割り当てられる（#59）。
/// `n < GUI_VIDEO_COUNT_DEFAULT` のときは全件 Mp4。Mp4 タイルも当面は先頭
/// フレーム PNG として返す（動画化は `start_animation_for_batch_spec` 経由）。
///
/// `n` は 1..=50 にクランプする。
#[wasm_bindgen]
pub fn generate_batch(params_js: JsValue, n: u32) -> Result<js_sys::Array, JsError> {
    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
    let shape = parse_shape(&p.shape, &p.glyph_char).map_err(err_to_js)?;

    let source = build_source_image(&mut p).map_err(err_to_js)?;

    let total = (n as usize).clamp(1, 50);
    let still_count = total.saturating_sub(GUI_VIDEO_COUNT_DEFAULT);
    let specs = random_batch_specs(p.seed as u64, total, still_count);

    let input = BatchInput {
        source,
        k: p.k,
        width: p.width,
        height: p.height,
        shape,
        specs,
    };
    let pngs = core_generate_batch(input)
        .map_err(|e| JsError::new(&format!("batch generation failed: {e}")))?;

    let arr = js_sys::Array::new();
    for png in pngs {
        arr.push(&js_sys::Uint8Array::from(&png[..]));
    }
    Ok(arr)
}

/// バッチ N 枚のうち `spec_idx` 番目の描画に必要な per-orb データをパックして返す。
///
/// `generate_one_at_index` / `start_animation_for_batch_spec` の置き換え。CPU 側
/// （core）で per-orb の決定論パラメータと clusters / 背景色を計算し、
/// Float32Array 1 本にエンコードして JS に渡す。GPU 側（WebGL2 fragment shader）
/// で各 t におけるフレームを per-pixel ループ + Source-Over 合成で描く。
///
/// 既存 `generate_batch` と同じ `random_batch_specs(seed, total, still_count)`
/// で spec 列を再構築するので、`spec_idx` 番目の spec / direction / speed /
/// count / orb_size / blur / seed は他 API と完全一致する（互換性維持）。
///
/// per-orb の rng シーケンスも `crates/core::animate::generate_orb_params` と
/// 同じ ChaCha8Rng + 同じドロー順で再現するので、同じ seed なら同じ
/// (phase, phi_radius, phi_blur, phi_opacity, cross_axis, style, cluster_idx,
/// speed_mult) が得られる。
///
/// # Float32Array レイアウト
///
/// `[0..16]` ヘッダ:
/// - `[0..4]`: 背景 RGBA (0..1 正規化)
/// - `[4]`: base_radius_unit (px) = `min(w, h) * 0.25 * orb_size`
/// - `[5]`: base_blur (0..1) — `(spec.blur + contrast.blur_offset()).clamp(0,1)` で
///   contrast 軸を反映済み
/// - `[6]`: direction_id (0=LR, 1=RL, 2=TB, 3=BT)
/// - `[7]`: cycle_count (1 = VerySlow, 2 = Slow, 3 = Mid, 4 = Fast)
/// - `[8]`: n_orbs (整数を f32 として)
/// - `[9]`: contrast_alpha_mul (0..1) — Phase B (#55)。Mid なら 1.0
/// - `[10]`: shape_id (0=Circle, 1=Glyph) — Phase B (#55)
/// - `[11..16]`: 予約（0 詰め）
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
/// - `[+11..+16]`: 予約（0 詰め）
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
    let contrast = parse_contrast_preset(&p.contrast_preset).map_err(err_to_js)?;

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
    // count_preset (high=35) は未満。将来 high を 64 超に上げるならここを更新。
    if n_orbs > GL_RENDERER_MAX_ORBS {
        return Err(JsError::new(&format!(
            "n_orbs {n_orbs} exceeds WebGL renderer limit {GL_RENDERER_MAX_ORBS} (orberGl.ts MAX_ORBS と同期して上げること)"
        )));
    }

    let base_radius_unit = (p.width.min(p.height) as f32) * 0.25 * spec.orb_size.max(0.0);
    // Phase B (#55): contrast.blur_offset() を base_blur に積算（core/animate と同式）。
    // Mid なら +0.0 で既存挙動と完全同値。
    let base_blur = (spec.blur + contrast.blur_offset()).clamp(0.0, 1.0);
    let alpha_mul = contrast.alpha_mul().clamp(0.0, 1.0);
    let shape_id: f32 = match shape {
        OrbShape::Circle => 0.0,
        OrbShape::Glyph { .. } => 1.0,
        // Aquarelle は WebGL 経路非対応。parse_shape で弾かれているはずだが念のため
        // Circle 扱いにフォールバック（パニックさせない）。
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
    );

    Ok(js_sys::Float32Array::from(buf.as_slice()))
}

/// `generate_orb_params` (core) と同じ rng ドロー順で per-orb データを生成し、
/// ヘッダ + per-orb フィールドを Float32 ベクタに詰める。
///
/// core 側 `generate_orb_params` を呼び出さずに同じシーケンスを **再現** する
/// （core の `OrbParams` は private struct で wasm から読めないため）。順序を
/// 1 つでも変えると同じ seed でも別の orb 列になり、視覚パリティが壊れる。
// TODO(orber#future): pack_render_data の引数が 10 個に達した。Phase C で
// orb 形状軸が更に増えるなら struct で受けるリファクタを検討する。
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
) -> Vec<f32> {
    let header_words = 16usize;
    let per_orb_words = 16usize;
    let mut buf = vec![0.0f32; header_words + per_orb_words * n_orbs];

    // header
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
    // [11..16] reserved (0)

    if n_orbs == 0 || clusters.is_empty() {
        return buf;
    }

    let cluster_weights: Vec<f32> = clusters.iter().map(|c| c.weight.max(0.0)).collect();
    let total_w: f32 = cluster_weights.iter().sum();

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    for i in 0..n_orbs {
        // 順序は crates/core::animate::generate_orb_params と完全一致させる。
        let phase: f32 = rng.gen_range(0.0..1.0);
        let phi_radius: f32 = rng.gen_range(0.0..TAU);
        let phi_blur: f32 = rng.gen_range(0.0..TAU);
        let phi_opacity: f32 = rng.gen_range(0.0..TAU);
        let cross_axis: f32 = rng.gen_range(0.0..1.0);
        let style_bit: f32 = if rng.gen::<u32>() & 1 == 0 { 0.0 } else { 1.0 };
        let cluster_idx = pick_weighted(&mut rng, &cluster_weights, total_w);
        let speed_mult: u32 = rng.gen_range(1..=3);

        let c = &clusters[cluster_idx.min(clusters.len() - 1)];

        let off = header_words + per_orb_words * i;
        buf[off] = c.color[0] as f32 / 255.0;
        buf[off + 1] = c.color[1] as f32 / 255.0;
        buf[off + 2] = c.color[2] as f32 / 255.0;
        buf[off + 3] = c.weight.max(0.0);
        buf[off + 4] = phase;
        buf[off + 5] = phi_radius;
        buf[off + 6] = phi_blur;
        buf[off + 7] = phi_opacity;
        buf[off + 8] = cross_axis;
        buf[off + 9] = style_bit;
        buf[off + 10] = speed_mult as f32;
        // [+11..+16] reserved
    }
    buf
}

/// Phase B (#55): Glyph 1 文字の alpha mask を JS 側に返す wasm wrapper。
///
/// 実体は [`orber_core::glyph::render_glyph_alpha_mask`] を参照。本関数は
/// その上に `(font, ch, size)` キャッシュ + size validation + JS 型変換だけを
/// 加える。size は `[16, 1024]` の範囲のみ受理（GUI は 256 固定の想定）。
/// 戻り値は長さ `size * size` の `Uint8Array`（行優先 alpha 0..255）。
/// 同梱フォントに無い文字は全 0 を返し panic しない。
#[wasm_bindgen]
pub fn get_glyph_alpha_mask(ch: &str, size: u32) -> Result<js_sys::Uint8Array, JsError> {
    // 入力 validation。size は 16..=1024 の範囲を許可（GUI は 256 を使う想定）。
    if !(16..=1024).contains(&size) {
        return Err(JsError::new(&format!(
            "size must be in [16, 1024], got {size}"
        )));
    }
    let ch = first_char_of(ch).map_err(err_to_js)?;
    let bytes = glyph_alpha_mask_bytes(GlyphFontId::NotoSymbols2, ch, size);
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
/// この wasm 側 `glyph_mask_cache` は wasm モジュール再ロード（HMR / 再起動）まで
/// 残る。同一 size + 同一 ch なら毎回同じ bytes が返るので決定論的に問題は
/// 無いが、開発時 HMR で再ロードしない場合に古いキャッシュエントリが残ったまま
/// になる点だけ注意。実運用では size=GLYPH_MASK_SIZE=256 に固定なので、
/// glyph 文字数 ×サイズ 1 通りで メモリ上限は数十エントリ程度。
type GlyphMaskKey = (GlyphFontId, u32, u32);

/// レビュー S1: `static mut CACHE` を `OnceLock<WasmSingleThreadCell<HashMap<...>>>`
/// に置換。`#[allow(static_mut_refs)]` を削除し、`unsafe` も無くなる。
fn glyph_mask_cache() -> &'static WasmSingleThreadCell<HashMap<GlyphMaskKey, Vec<u8>>> {
    static CELL: OnceLock<WasmSingleThreadCell<HashMap<GlyphMaskKey, Vec<u8>>>> =
        OnceLock::new();
    CELL.get_or_init(|| WasmSingleThreadCell::new(HashMap::new()))
}

fn glyph_alpha_mask_bytes(font: GlyphFontId, ch: char, size: u32) -> Vec<u8> {
    let key = (font, ch as u32, size);
    {
        let cache = glyph_mask_cache().borrow_mut();
        if let Some(v) = cache.get(&key) {
            return v.clone();
        }
    }
    let v = render_glyph_alpha_mask(font, ch, size);
    glyph_mask_cache().borrow_mut().insert(key, v.clone());
    v
}


/// 重み比例の 1 サンプル抽選器。`crates/core::animate::pick_weighted` と同等。
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

/// 入力画像 1 枚から SVG 文字列を生成する。
///
/// SVG の viewBox は core 側で固定（1080×1920）。`width` / `height` /
/// `direction` / `speed` / `count` / `seed` / `shape` は使われない（SVG は
/// 静的のみ）。`orb_size` / `blur` のみ反映する。
#[wasm_bindgen]
pub fn generate_svg(params_js: JsValue) -> Result<String, JsError> {
    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
    let contrast = parse_contrast_preset(&p.contrast_preset).map_err(err_to_js)?;
    let source = build_source_image(&mut p).map_err(err_to_js)?;

    let clusters_full = extract_clusters(&source, p.k)
        .map_err(|e| JsError::new(&format!("cluster extraction failed: {e}")))?;
    let bg = derive_background_rgba(&clusters_full);
    let clusters = drop_dominant(&clusters_full);

    let opts = StyleOptions {
        orb_size: p.orb_size,
        blur: p.blur,
        saturation: 1.0,
        background: bg,
        contrast,
    };
    Ok(core_render_svg(&clusters, &opts))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn parse_direction_roundtrip() {
        assert!(matches!(
            parse_direction("lr"),
            Ok(MotionDirection::LeftToRight)
        ));
        assert!(matches!(
            parse_direction("rl"),
            Ok(MotionDirection::RightToLeft)
        ));
        assert!(matches!(
            parse_direction("tb"),
            Ok(MotionDirection::TopToBottom)
        ));
        assert!(matches!(
            parse_direction("bt"),
            Ok(MotionDirection::BottomToTop)
        ));
        assert!(parse_direction("xx").is_err());
    }

    #[test]
    fn parse_speed_roundtrip() {
        // Phase B (#55) で Mid / Fast を追加済み。
        assert!(matches!(
            parse_speed("very-slow"),
            Ok(MotionSpeed::VerySlow)
        ));
        assert!(matches!(parse_speed("slow"), Ok(MotionSpeed::Slow)));
        assert!(matches!(parse_speed("mid"), Ok(MotionSpeed::Mid)));
        assert!(matches!(parse_speed("fast"), Ok(MotionSpeed::Fast)));
        assert!(parse_speed("xxx").is_err());
    }

    #[test]
    fn parse_speed_preset_handles_empty_and_values() {
        // M1/M2: 空文字 / "mid" は identity（None）を返す。spec.speed と
        // GUI_VIDEO_SPEEDS が温存されるための不変条件。
        assert!(matches!(parse_speed_preset(""), Ok(None)));
        assert!(matches!(parse_speed_preset("mid"), Ok(None)));
        assert!(matches!(
            parse_speed_preset("slow"),
            Ok(Some(MotionSpeed::Slow))
        ));
        assert!(matches!(
            parse_speed_preset("fast"),
            Ok(Some(MotionSpeed::Fast))
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

    /// M1: speed_preset='' / 'mid' のとき speed_for_spec_idx の戻り値（
    /// still=spec.speed / video=GUI_VIDEO_SPEEDS）を温存する。
    #[test]
    fn speed_preset_empty_or_unspecified_uses_spec_idx() {
        for empty_input in ["", "mid"] {
            let speed_override = parse_speed_preset(empty_input).unwrap();
            assert!(
                speed_override.is_none(),
                "speed_preset={empty_input:?} must be identity (None)"
            );
        }
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

    /// M1: contrast_preset='' のとき ContrastPreset::Mid と一致する（identity）。
    #[test]
    fn contrast_preset_empty_is_mid_identity() {
        assert_eq!(parse_contrast_preset("").unwrap(), ContrastPreset::Mid);
        assert_eq!(parse_contrast_preset("mid").unwrap(), ContrastPreset::Mid);
        // Mid は alpha_mul=1.0 / blur_offset=0.0 で既存挙動と完全同値であることが
        // crates/core/src/style.rs の regression test で固定されている。
    }

    #[test]
    fn parse_count_preset_table() {
        assert_eq!(parse_count_preset("").unwrap(), None);
        assert_eq!(parse_count_preset("low").unwrap(), Some(10));
        assert_eq!(parse_count_preset("mid").unwrap(), Some(20));
        assert_eq!(parse_count_preset("high").unwrap(), Some(35));
        assert!(parse_count_preset("xxx").is_err());
    }

    #[test]
    fn parse_contrast_preset_table() {
        assert_eq!(parse_contrast_preset("").unwrap(), ContrastPreset::Mid);
        assert_eq!(parse_contrast_preset("mid").unwrap(), ContrastPreset::Mid);
        assert_eq!(parse_contrast_preset("low").unwrap(), ContrastPreset::Low);
        assert_eq!(parse_contrast_preset("high").unwrap(), ContrastPreset::High);
        assert!(parse_contrast_preset("xxx").is_err());
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
    fn glyph_alpha_mask_paints_known_char() {
        // ☆ は同梱 NotoSansSymbols2 にある。alpha が立っているピクセルが
        // 一定数以上あること（少なくとも全ピクセルの 5% は塗られる想定）。
        let bytes = render_glyph_alpha_mask(GlyphFontId::NotoSymbols2, '☆', 64);
        assert_eq!(bytes.len(), 64 * 64);
        let lit = bytes.iter().filter(|&&b| b > 0).count();
        assert!(
            lit > 64 * 64 / 20,
            "rendering ☆ at 64x64 should produce >=5% lit pixels, got {lit}"
        );
    }

    #[test]
    fn glyph_alpha_mask_unknown_char_returns_empty() {
        // 絵文字 (Symbols 2 subset 外) は全 0 を返す。
        let bytes = render_glyph_alpha_mask(GlyphFontId::NotoSymbols2, '\u{1F355}', 32);
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
            contrast_preset: String::new(),
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
}
