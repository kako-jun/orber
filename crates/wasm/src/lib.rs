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

use orber_core::animate::{
    render_frame, AnimateOptions, AnimationCursor, MotionDirection, MotionSpeed,
};
use orber_core::batch::{generate_batch as core_generate_batch, BatchInput};
use orber_core::cluster::{derive_background_rgba, drop_dominant, extract_clusters};
use orber_core::orb::OrbShape;
use orber_core::style::{render_svg as core_render_svg, StyleOptions};
use orber_core::variations::{
    random_batch_specs, VariationSpec, GUI_VIDEO_COUNT_DEFAULT, GUI_VIDEO_DIRECTIONS,
};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

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
        other => Err(format!(
            "invalid speed: {other} (expected one of very-slow / slow)"
        )),
    }
}

fn parse_shape(s: &str) -> Result<OrbShape, String> {
    // OrbShape::Aquarelle はパラメータが多いので wasm 入口では `circle` のみ受ける。
    // Aquarelle は将来必要になったら別 API を生やす。
    match s {
        "circle" => Ok(OrbShape::Circle),
        other => Err(format!(
            "invalid shape: {other} (only 'circle' is supported for now)"
        )),
    }
}

fn build_source_image(p: &mut WasmParams) -> Result<image::RgbImage, String> {
    let rgb = std::mem::take(&mut p.source_rgb);
    image::RgbImage::from_raw(p.source_width, p.source_height, rgb).ok_or_else(|| {
        "source_rgb length does not match source_width * source_height * 3".to_string()
    })
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
    let speed = parse_speed(&p.speed).map_err(err_to_js)?;
    let shape = parse_shape(&p.shape).map_err(err_to_js)?;

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
    let shape = parse_shape(&p.shape).map_err(err_to_js)?;

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

/// バッチ N 枚のうち `spec_idx` 番目だけを 1 枚 PNG として生成する。
///
/// `generate_batch` と同じ `random_batch_specs(seed, total, still_count)` で
/// spec 列を再構築し、`spec_idx` 番目だけ描画する。`width`/`height` を上げて
/// 呼べば「同じバリエーションの高解像版」が得られるので、GUI のダウンロード時
/// に表示用プレビュー（540×960）と別の解像度で焼き直す用途を想定（#73）。
///
/// 動画タイル領域（`spec_idx >= still_count`）では
/// `start_animation_for_batch_spec` と同じく `GUI_VIDEO_DIRECTIONS` で
/// direction を 4 方向に上書きし、video タイルの t=0 フレームと完全一致させる。
#[wasm_bindgen]
pub fn generate_one_at_index(
    params_js: JsValue,
    n: u32,
    spec_idx: u32,
) -> Result<js_sys::Uint8Array, JsError> {
    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
    let shape = parse_shape(&p.shape).map_err(err_to_js)?;

    let total = (n as usize).clamp(1, 50);
    let spec_idx = spec_idx as usize;
    if spec_idx >= total {
        return Err(JsError::new(&format!(
            "spec_idx {spec_idx} is out of range [0, {total})"
        )));
    }
    let still_count = total.saturating_sub(GUI_VIDEO_COUNT_DEFAULT);

    let source = build_source_image(&mut p).map_err(err_to_js)?;
    let clusters_full = extract_clusters(&source, p.k)
        .map_err(|e| JsError::new(&format!("cluster extraction failed: {e}")))?;
    let bg = derive_background_rgba(&clusters_full);
    let clusters = drop_dominant(&clusters_full);

    let specs = random_batch_specs(p.seed as u64, total, still_count);
    let spec = specs[spec_idx];

    // direction の決定は純粋関数 direction_for_spec_idx に集約。
    // start_animation_for_batch_spec と同じロジックを共有することで、
    // プレビュー静止 PNG と動画タイルの direction を構造的に揃える。
    let direction = direction_for_spec_idx(spec_idx, still_count, &spec);

    let opts = AnimateOptions {
        width: p.width,
        height: p.height,
        seed: spec.seed,
        direction,
        speed: spec.speed,
        count: Some(spec.count),
        orb_size: spec.orb_size,
        blur: spec.blur,
        saturation: 1.0,
        background: bg,
        shape,
    };
    let frame = render_frame(&clusters, &opts, 0.0);
    let png = encode_png_rgba(&frame)?;
    Ok(js_sys::Uint8Array::from(&png[..]))
}

/// 動画 1 タイル分の RGBA フレームを 1 枚ずつ取り出すハンドル。
///
/// `start_animation_for_batch_spec` で構築する。各 `next_frame()` は
/// `width * height * 4` バイトの RGBA8 ピクセル列 (`Uint8ClampedArray`) を
/// 返す。完了後は `null`。`<video loop>` 用途を想定しており、
/// `t = i / total_frames` (i = 0..total_frames) を出すので、最後のフレームの
/// 次が t=0 とピクセル一致する（README の loop closure 不変条件を維持）。
#[wasm_bindgen]
pub struct AnimationHandle {
    cursor: AnimationCursor,
}

#[wasm_bindgen]
impl AnimationHandle {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.cursor.width()
    }
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.cursor.height()
    }
    #[wasm_bindgen(getter)]
    pub fn total_frames(&self) -> u32 {
        self.cursor.total_frames()
    }
    #[wasm_bindgen(getter)]
    pub fn next_index(&self) -> u32 {
        self.cursor.next_index()
    }

    /// 次のフレームの RGBA バイト列を返す。完了後は `null`。
    pub fn next_frame(&mut self) -> Option<js_sys::Uint8ClampedArray> {
        let img = self.cursor.next_frame()?;
        Some(js_sys::Uint8ClampedArray::from(img.as_raw().as_slice()))
    }
}

/// 後半の動画タイルのアニメーションを起動する。
///
/// `random_batch_specs(params.seed, n, n - GUI_VIDEO_COUNT_DEFAULT)` を再生成
/// して `spec_idx` 番目の spec を取り、入力画像のクラスタ抽出を 1 回だけ
/// 走らせて `AnimationCursor` を返す。JS 側は `next_frame()` を `total_frames`
/// 回呼んで WebCodecs `VideoEncoder` に流し込む想定。
///
/// # 決定論性
///
/// `random_batch_specs` は同じ `(seed, total, still_count)` で同じ spec 列を
/// 返す（`crates/core::variations::random_batch_specs_is_deterministic_per_seed`
/// テストで担保）。よって `generate_batch(params, n)` で得た spec 列の
/// `spec_idx` 番目と、ここで再構築した spec 列の `spec_idx` 番目は完全一致する。
/// その結果、静止画タイル（`generate_batch` で描かれた `t=0` フレーム）と
/// 動画タイル（このアニメーション）は同じパラメータで描画され、見た目の
/// 整合性が保たれる。
///
/// `total_frames` は呼び出し側で計算する（GUI 既定: `fps × seconds = 24 × 4 = 96`）。
/// `spec_idx` が `[still_count, total)` の範囲外なら `Mp4` 枠ではないので
/// `JsError`。
#[wasm_bindgen]
pub fn start_animation_for_batch_spec(
    params_js: JsValue,
    n: u32,
    spec_idx: u32,
    total_frames: u32,
) -> Result<AnimationHandle, JsError> {
    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
    let shape = parse_shape(&p.shape).map_err(err_to_js)?;

    let total = (n as usize).clamp(1, 50);
    let still_count = total.saturating_sub(GUI_VIDEO_COUNT_DEFAULT);
    let spec_idx = spec_idx as usize;
    if spec_idx < still_count || spec_idx >= total {
        return Err(JsError::new(&format!(
            "spec_idx {spec_idx} is not within the Mp4 range [{still_count}, {total})"
        )));
    }
    if total_frames == 0 {
        return Err(JsError::new("total_frames must be > 0"));
    }

    let source = build_source_image(&mut p).map_err(err_to_js)?;
    let clusters_full = extract_clusters(&source, p.k)
        .map_err(|e| JsError::new(&format!("cluster extraction failed: {e}")))?;
    let bg = derive_background_rgba(&clusters_full);
    let clusters = drop_dominant(&clusters_full);

    let specs = random_batch_specs(p.seed as u64, total, still_count);
    let spec = specs[spec_idx];

    // #59: 動画タイル 4 枚に LR / RL / TB / BT を 1 枚ずつ重複なく割り当てる。
    // direction_for_spec_idx を経由することで、`generate_one_at_index` で
    // 描かれる t=0 PNG と完全に同じ direction を選ぶ（プレビュー静止画と
    // 動画の整合性を構造的に保証）。
    let direction = direction_for_spec_idx(spec_idx, still_count, &spec);

    let opts = AnimateOptions {
        width: p.width,
        height: p.height,
        seed: spec.seed,
        direction,
        speed: spec.speed,
        count: Some(spec.count),
        orb_size: spec.orb_size,
        blur: spec.blur,
        saturation: 1.0,
        background: bg,
        shape,
    };

    let cursor = AnimationCursor::new(clusters, opts, total_frames);
    Ok(AnimationHandle { cursor })
}

/// 入力画像 1 枚から SVG 文字列を生成する。
///
/// SVG の viewBox は core 側で固定（1080×1920）。`width` / `height` /
/// `direction` / `speed` / `count` / `seed` / `shape` は使われない（SVG は
/// 静的のみ）。`orb_size` / `blur` のみ反映する。
#[wasm_bindgen]
pub fn generate_svg(params_js: JsValue) -> Result<String, JsError> {
    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
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
        assert!(matches!(
            parse_speed("very-slow"),
            Ok(MotionSpeed::VerySlow)
        ));
        assert!(matches!(parse_speed("slow"), Ok(MotionSpeed::Slow)));
        assert!(parse_speed("fast").is_err());
    }

    #[test]
    fn parse_shape_only_circle() {
        assert!(matches!(parse_shape("circle"), Ok(OrbShape::Circle)));
        assert!(parse_shape("aquarelle").is_err());
        assert!(parse_shape("").is_err());
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
}
