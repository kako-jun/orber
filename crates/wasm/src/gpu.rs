//! #230: ブラウザ WebGPU canvas present 経路（#207 Phase 2 スライス 2/5）。
//!
//! orber-core の `GpuRenderer`（WGSL）で `<canvas>` に直接描く最小経路。
//! main thread 配置（Worker 配線の要否は Phase 3 で判断）。公開 API は 4 つ:
//!
//! - [`gpu_init`]\(canvas\) — async。instance → canvas surface → adapter
//!   （`compatible_surface` 付き）→ device の順に bring-up し、surface を
//!   configure して renderer を構築する。WebGPU 不在（adapter 無し）は明確な
//!   エラーで reject する。**fallback は無い**（#207 方針。wgpu の `webgl`
//!   feature は採らない）。
//! - [`gpu_set_render_data`]\(params_js, n, spec_idx\) — `get_render_data` と
//!   同じ spec 解決経路（`build_gpu_render_inputs` → 共有の `resolve_frame`）で
//!   shape 別の描画入力（clusters + opts + orb 用 pack）を構築して保持する。入力は
//!   spec ごとに静的で、モーションは `t` がシェーダ内で駆動する（WebGL 版と同じ構造）。
//! - [`gpu_render`]\(t\) — surface frame を acquire し、保持入力 + `t` で
//!   `opts.shape` 別の core 経路（`render_packed_to_view` / `render_frame_*_to_view`）
//!   → present。
//! - [`gpu_resize`]\(w, h\) — surface を新サイズで再 configure する。
//!
//! ## 形状について（#231 で全 shape 配線）
//!
//! `gpu_set_render_data` は orb / glyph / image / aquarelle の 4 shape を受ける。
//! `build_gpu_render_inputs` が `build_render_pack`（WebGL）と同じ `resolve_frame`
//! で spec / preset / kmeans を解決し、形状を `OrbShape` まで解決して
//! [`AnimateOptions`] + clusters（+ orb 用 pack）を保持する。`gpu_render(t)` は
//! `opts.shape` で core の公開 API へ分岐する（CLI の `FrameRenderer::render` と
//! 同じ分岐構造 = parity）:
//! - Orb: pack 経由 `render_packed_to_view`（#230 の見た目を一切変えないため温存）
//! - Glyph: `render_frame_glyph_to_view`（SDF orb 単パス、#235。glyph_rotate 含む #136）
//! - Image: `render_frame_image_to_view`（`image_rgba_to_sdf` で作った SDF を食わせる）
//! - Aquarelle: `render_frame_aquarelle_to_view`（4 層モデル、ChaCha8 per-orb pack）
//!
//! ## surface format / alpha mode の選択
//!
//! format は caps から **non-sRGB**（`Bgra8Unorm` / `Rgba8Unorm`）を明示的に
//! 選ぶ。orber のシェーダは sRGB エンコード済みの値をそのまま書く（core の
//! compositing contract）ため、sRGB format だと二重エンコードになる
//! （core 側 `debug_assert` の契約）。alpha mode は `Opaque` 優先 — orber の
//! 背景は不透明で、WebGL 版 (`orberGl.ts`) も `alpha: false` 相当の不透明
//! canvas なので、#232 の A/B 照合でも合成条件が揃う。

use std::sync::OnceLock;

use orber_core::animate::AnimateOptions;
use orber_core::cluster::Cluster;
use orber_core::gpu::GpuRenderer;
use orber_core::orb::OrbShape;
use wasm_bindgen::prelude::*;

use crate::{build_gpu_render_inputs, deserialize_params, err_to_js, WasmSingleThreadCell};

/// `gpu_init` で組み上がる一式。surface の configure（resize / Outdated 回復）
/// には device が要るが、`GpuRenderer` は device を private に持つため、
/// ここで clone を別途保持する（`wgpu::Device` は安価な handle clone）。
struct GpuState {
    renderer: GpuRenderer,
    device: wgpu::Device,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// `gpu_set_render_data` が解決した 1 タイル分の描画入力（#231）。spec ごとに
    /// 静的で、`gpu_render(t)` のたびに `t` だけ変えて再利用する。orb は `pack`、
    /// glyph / image / aquarelle は `clusters` + `opts` で core 経路へ分岐する。
    frame: Option<FrameInputs>,
}

/// `gpu_set_render_data` が保持する 1 タイル分の解決済み入力（#231）。`crate::
/// GpuRenderInputs` をモジュール内の保持用に展開したもの。
struct FrameInputs {
    clusters: Vec<Cluster>,
    opts: AnimateOptions,
    pack: Vec<f32>,
}

/// wasm シングルスレッド前提のグローバル状態。lib.rs の `source_cache` と同じ
/// `OnceLock<WasmSingleThreadCell<...>>` パターン（unsafe 境界はラッパ 1 か所）。
fn gpu_state() -> &'static WasmSingleThreadCell<Option<GpuState>> {
    static CELL: OnceLock<WasmSingleThreadCell<Option<GpuState>>> = OnceLock::new();
    CELL.get_or_init(|| WasmSingleThreadCell::new(None))
}

/// WebGPU を bring-up して canvas に紐付ける。成功時は adapter 名を返す
/// （診断用。dev ページが init 時間と一緒に console へ出す）。
///
/// canvas の現在の width/height で surface を configure する。呼び出し側は
/// `gpu_set_render_data` に渡す `params.width/height` と canvas サイズを
/// 一致させること（pack の base_radius がそのサイズ基準で焼かれるため。
/// 不一致でもエラーにはならないが orb のスケールがずれる）。
///
/// WebGPU 不在（`navigator.gpu` 無し / adapter 拒否）は明確なエラーで reject
/// する。fallback は無い（#207 方針）。
///
/// 同時二重呼び出し（先行 init の await 完了前に再度呼ぶ）は last-writer-wins
/// で、先勝ち調停は意図的に持たない。呼び出し側でガードすること（dev ページ
/// /gpu-lab は start ボタンの disable でガード済み）。
#[wasm_bindgen]
pub async fn gpu_init(canvas: web_sys::HtmlCanvasElement) -> Result<String, JsError> {
    // 再 init 時は、新しい surface を作る前に旧 GpuState（旧 surface）を先に
    // drop する。同一 canvas の GPUCanvasContext を包む新旧 surface の共存を
    // 避けるため（旧 surface が configure 済みのまま新 surface を作らない）。
    *gpu_state().borrow_mut() = None;

    // BROWSER_WEBGPU のみ = WebGPU 必須を instance レベルでも明示する。
    // （wgpu の `webgl` feature は積んでいないので、どのみち他バックエンドは無い）
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    let width = canvas.width().max(1);
    let height = canvas.height().max(1);
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| JsError::new(&format!("WebGPU: failed to create canvas surface: {e}")))?;

    // adapter は compatible_surface 付きで取るのが正道（headless の
    // `GpuRenderer::new_async` とはここが違う）。core へは出来上がった
    // device/queue だけを `from_device_queue` で渡す。
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        })
        .await
        .map_err(|e| {
            JsError::new(&format!(
                "WebGPU: no adapter available (browser without WebGPU support, or access denied): {e}"
            ))
        })?;
    let adapter_name = adapter.get_info().name;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("orber-wasm-gpu-device"),
            ..Default::default()
        })
        .await
        .map_err(|e| JsError::new(&format!("WebGPU: request_device failed: {e}")))?;

    // format: non-sRGB（Bgra8Unorm / Rgba8Unorm）を明示的に選ぶ。シェーダ出力は
    // sRGB エンコード済みの raw 書きなので、*Srgb format だと二重エンコードに
    // なる（core の to_view 契約）。caps.formats は preferred 順なので、先頭
    // から最初に合う non-sRGB を採る。
    let caps = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .ok_or_else(|| {
            JsError::new(&format!(
                "WebGPU: no non-sRGB surface format (Bgra8Unorm/Rgba8Unorm) in caps: {:?}",
                caps.formats
            ))
        })?;
    // alpha mode: Opaque 優先（orber は背景不透明。WebGL 版 canvas も不透明なので
    // #232 の A/B で合成条件が揃う）。caps.alpha_modes は Opaque か Inherit を
    // 必ず 1 つ以上含む契約なので、Opaque が無ければ先頭（= Inherit）を採る。
    let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        wgpu::CompositeAlphaMode::Opaque
    } else {
        caps.alpha_modes[0]
    };

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode,
        view_formats: vec![],
    };
    surface.configure(&device, &config);

    let renderer = GpuRenderer::from_device_queue(device.clone(), queue, adapter_name.clone());
    *gpu_state().borrow_mut() = Some(GpuState {
        renderer,
        device,
        surface,
        config,
        frame: None,
    });
    Ok(adapter_name)
}

/// バッチ `spec_idx` 番目の描画入力を構築して保持する（#231 で全 shape 配線）。
/// `get_render_data` と同じ spec 解決経路（[`build_gpu_render_inputs`] → 共有の
/// `resolve_frame`: spec 再構築・preset 上書き・kmeans キャッシュ込み）なので、
/// 同じ params なら WebGL 版と同一の spec / per-orb 解決になる。形状は
/// `resolve_orb_shape` で全 shape（orb / glyph / image / aquarelle）に解決する。
/// モーションは入力に焼かれず、`gpu_render(t)` の `t` がシェーダ内で駆動する。
#[wasm_bindgen]
pub fn gpu_set_render_data(params_js: JsValue, n: u32, spec_idx: u32) -> Result<(), JsError> {
    let p = deserialize_params(params_js).map_err(err_to_js)?;
    let inputs = build_gpu_render_inputs(p, n, spec_idx).map_err(err_to_js)?;
    let mut guard = gpu_state().borrow_mut();
    let state = guard
        .as_mut()
        .ok_or_else(|| JsError::new("gpu_set_render_data called before gpu_init"))?;
    state.frame = Some(FrameInputs {
        clusters: inputs.clusters,
        opts: inputs.opts,
        pack: inputs.pack,
    });
    Ok(())
}

/// 保持入力の時刻 `t`（0..1、シェーダ側で clamp）のフレームを canvas に描いて
/// present する。requestAnimationFrame ごとに呼ぶ想定。`opts.shape` で core の
/// 公開 API へ分岐する（#231、CLI の `FrameRenderer::render` と同じ分岐 = parity）。
///
/// surface frame の取得に失敗したフレームは黙って skip する（Timeout /
/// Occluded。次の rAF で回復する一時状態）。Outdated は保持 config で
/// 再 configure して skip（次フレームから描ける）。Lost / Validation は
/// 回復不能としてエラーを投げる。
#[wasm_bindgen]
pub fn gpu_render(t: f32) -> Result<(), JsError> {
    let mut guard = gpu_state().borrow_mut();
    let state = guard
        .as_mut()
        .ok_or_else(|| JsError::new("gpu_render called before gpu_init"))?;
    if state.frame.is_none() {
        return Err(JsError::new("gpu_render called before gpu_set_render_data"));
    }

    let frame = match state.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
        // 一時状態: このフレームは捨てて次の rAF に任せる。
        wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
            return Ok(())
        }
        // canvas サイズが裏で変わった等。保持 config で configure し直して skip。
        wgpu::CurrentSurfaceTexture::Outdated => {
            state.surface.configure(&state.device, &state.config);
            return Ok(());
        }
        wgpu::CurrentSurfaceTexture::Lost => {
            return Err(JsError::new(
                "WebGPU: surface lost — re-run gpu_init to rebuild the surface",
            ))
        }
        wgpu::CurrentSurfaceTexture::Validation => {
            return Err(JsError::new(
                "WebGPU: validation error while acquiring the surface texture",
            ))
        }
    };

    // view は surface frame そのもの = config の width × height に正確に一致する
    // （core の to_view 契約）。format も config のものを渡す（non-sRGB は
    // gpu_init で選択済み）。
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let width = state.config.width;
    let height = state.config.height;
    let format = state.config.format;
    // `frame.is_none()` を上で弾いているので unwrap は安全（同一 borrow 内で値は変わらない）。
    let f = state.frame.as_ref().expect("frame checked above");
    let renderer = &state.renderer;
    // shape 別ディスパッチ（CLI の FrameRenderer::render と同じ構造）。Orb は #230 の
    // pack 経路を温存して見た目を一切変えない。glyph / image / aquarelle は clusters +
    // opts を core の専用 to_view 経路へ渡す（SDF / aquarelle pack の面倒は core が見る）。
    match &f.opts.shape {
        OrbShape::Glyph { .. } => {
            renderer.render_frame_glyph_to_view(&f.clusters, &f.opts, t, &view, format)
        }
        OrbShape::Image { .. } => {
            renderer.render_frame_image_to_view(&f.clusters, &f.opts, t, &view, format)
        }
        OrbShape::Aquarelle(_) => {
            renderer.render_frame_aquarelle_to_view(&f.clusters, &f.opts, t, &view, format)
        }
        OrbShape::Orb => renderer.render_packed_to_view(&f.pack, width, height, t, &view, format),
    }
    frame.present();
    Ok(())
}

/// surface を新サイズで再 configure する。呼び出し側は canvas の width/height
/// 属性を同じ値に変更してから呼ぶこと。pack の base_radius は
/// `gpu_set_render_data` 時点の params.width/height 基準なので、スケールを
/// 合わせたい場合は resize 後に `gpu_set_render_data` も呼び直す。
#[wasm_bindgen]
pub fn gpu_resize(width: u32, height: u32) -> Result<(), JsError> {
    let mut guard = gpu_state().borrow_mut();
    let state = guard
        .as_mut()
        .ok_or_else(|| JsError::new("gpu_resize called before gpu_init"))?;
    state.config.width = width.max(1);
    state.config.height = height.max(1);
    state.surface.configure(&state.device, &state.config);
    Ok(())
}
