//! WASM bindings for orber-core. Exposes the rendering pipeline to browsers.
//!
//! 画像デコードは JS 側に任せる: 呼び出し側は `<canvas>` / `ImageData` で
//! 生 RGB バイトを取り出して `WasmParams.source_rgb` に詰めて渡す。core クレート
//! は wasm バンドルサイズ削減のため PNG デコード以外を積まない。

use orber_core::animate::{render_frame, AnimateOptions, MotionDirection, MotionSpeed};
use orber_core::batch::{generate_batch as core_generate_batch, BatchInput};
use orber_core::cluster::{derive_background_rgba, drop_dominant, extract_clusters};
use orber_core::orb::OrbShape;
use orber_core::style::{render_svg as core_render_svg, StyleOptions};
use orber_core::variations::DEFAULT_VARIATIONS;
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

fn parse_direction(s: &str) -> Result<MotionDirection, JsError> {
    match s {
        "lr" => Ok(MotionDirection::LeftToRight),
        "rl" => Ok(MotionDirection::RightToLeft),
        "tb" => Ok(MotionDirection::TopToBottom),
        "bt" => Ok(MotionDirection::BottomToTop),
        other => Err(JsError::new(&format!("invalid direction: {other}"))),
    }
}

fn parse_speed(s: &str) -> Result<MotionSpeed, JsError> {
    match s {
        "very-slow" => Ok(MotionSpeed::VerySlow),
        "slow" => Ok(MotionSpeed::Slow),
        other => Err(JsError::new(&format!("invalid speed: {other}"))),
    }
}

fn parse_shape(s: &str) -> Result<OrbShape, JsError> {
    // OrbShape::Aquarelle はパラメータが多いので wasm 入口では `circle` のみ受ける。
    // Aquarelle は将来必要になったら別 API を生やす。
    match s {
        "circle" => Ok(OrbShape::Circle),
        other => Err(JsError::new(&format!(
            "invalid shape: {other} (only 'circle' is supported for now)"
        ))),
    }
}

fn build_source_image(p: &WasmParams) -> Result<image::RgbImage, JsError> {
    image::RgbImage::from_raw(p.source_width, p.source_height, p.source_rgb.clone()).ok_or_else(
        || JsError::new("source_rgb length does not match source_width * source_height * 3"),
    )
}

fn deserialize_params(params_js: JsValue) -> Result<WasmParams, JsError> {
    serde_wasm_bindgen::from_value(params_js)
        .map_err(|e| JsError::new(&format!("failed to parse params: {e}")))
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
    let p = deserialize_params(params_js)?;
    let direction = parse_direction(&p.direction)?;
    let speed = parse_speed(&p.speed)?;
    let shape = parse_shape(&p.shape)?;

    let source = build_source_image(&p)?;
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

/// 入力画像 1 枚から `n` 個の variation PNG を一括生成する。
///
/// `DEFAULT_VARIATIONS` の先頭 `n` 件を使う。`n` が 10 を超える場合は実際の
/// preset 件数まで丸める。返値は `Array<Uint8Array>`。
#[wasm_bindgen]
pub fn generate_batch(params_js: JsValue, n: u32) -> Result<js_sys::Array, JsError> {
    let p = deserialize_params(params_js)?;
    let shape = parse_shape(&p.shape)?;

    let source = build_source_image(&p)?;

    let take_n = (n as usize).min(DEFAULT_VARIATIONS.len());
    let specs = DEFAULT_VARIATIONS.iter().take(take_n).copied().collect();

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
    let p = deserialize_params(params_js)?;
    let source = build_source_image(&p)?;

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
