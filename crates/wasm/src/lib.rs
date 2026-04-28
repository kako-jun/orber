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
use orber_core::cluster::{derive_background_rgba, drop_dominant, extract_clusters};
use orber_core::orb::OrbShape;
use orber_core::style::{render_svg as core_render_svg, StyleOptions};
use orber_core::variations::random_batch_specs;
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
/// 後半 `MP4_COUNT` (= 5) 件を `VariationKind::Mp4`、残りを `Png` にする。
/// GUI では n が 9（横長 3×3）／10（縦長 5×2）で運用されるため、どちらの
/// 場合でも「後半 5 枚は動画枠」になる。`n < MP4_COUNT` のときは全件 Mp4。
/// 当面はどちらも先頭フレーム PNG として返す（Mp4 の動画化は別 Issue）。
///
/// `n` は 1..=50 にクランプする。
#[wasm_bindgen]
pub fn generate_batch(params_js: JsValue, n: u32) -> Result<js_sys::Array, JsError> {
    /// 後半 N 枚を Mp4 枠とする（n=9 でも 10 でも「後半 5」を維持）。
    const MP4_COUNT: usize = 5;

    let mut p = deserialize_params(params_js).map_err(err_to_js)?;
    let shape = parse_shape(&p.shape).map_err(err_to_js)?;

    let source = build_source_image(&mut p).map_err(err_to_js)?;

    let total = (n as usize).clamp(1, 50);
    let still_count = total.saturating_sub(MP4_COUNT);
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
}
