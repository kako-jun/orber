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
//!   同一経路（`build_render_pack`）で pack を構築して保持する。pack は spec
//!   ごとに静的で、モーションは `t` がシェーダ内で駆動する（WebGL 版と同じ構造）。
//! - [`gpu_render`]\(t\) — surface frame を acquire し、保持 pack + `t` で
//!   `render_packed_to_view` → present。
//! - [`gpu_resize`]\(w, h\) — surface を新サイズで再 configure する。
//!
//! ## 形状について
//!
//! 現状 **Orb のみ**の最小経路。`gpu_set_render_data` は #231 で配線されるまで
//! circle 以外の shape（glyph / image）を明確なエラーで reject する
//! （`ensure_gpu_supported_shape`）。`gpu_render` は orb パイプライン
//! （`render_packed_to_view`、SDF 無し）しか持たないため、黙って受理すると
//! orb として誤描画される。Glyph / Image / Aquarelle（#231）は shape ごとの
//! SDF / aquarelle pack の保持と `render_frame_*_to_view` への分岐をここに
//! 足し、この reject を外す。
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

use orber_core::gpu::GpuRenderer;
use wasm_bindgen::prelude::*;

use crate::{
    build_render_pack, deserialize_params, ensure_gpu_supported_shape, err_to_js,
    WasmSingleThreadCell,
};

/// `gpu_init` で組み上がる一式。surface の configure（resize / Outdated 回復）
/// には device が要るが、`GpuRenderer` は device を private に持つため、
/// ここで clone を別途保持する（`wgpu::Device` は安価な handle clone）。
struct GpuState {
    renderer: GpuRenderer,
    device: wgpu::Device,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// `gpu_set_render_data` が構築した pack（`pack_render_data_for_webgl`
    /// レイアウト）。spec ごとに静的で、`gpu_render(t)` のたびに再利用する。
    pack: Option<Vec<f32>>,
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
        pack: None,
    });
    Ok(adapter_name)
}

/// バッチ `spec_idx` 番目の pack を構築して保持する。`get_render_data` と
/// 完全に同じ経路（[`build_render_pack`]: spec 再構築・preset 上書き・kmeans
/// キャッシュ込み）なので、同じ params なら WebGL 版と同一の pack になる。
/// モーションは pack に焼かれず、`gpu_render(t)` の `t` がシェーダ内で駆動する。
///
/// shape は #231 で配線されるまで `"circle"` のみ。glyph / image は orb として
/// 誤描画されるため明確なエラーで reject する（モジュール doc「形状について」）。
#[wasm_bindgen]
pub fn gpu_set_render_data(params_js: JsValue, n: u32, spec_idx: u32) -> Result<(), JsError> {
    let p = deserialize_params(params_js).map_err(err_to_js)?;
    // S1: gpu_render は orb パイプラインしか持たない。circle 以外は silent
    // wrong-render になるため、pack を構築する前に reject する。
    ensure_gpu_supported_shape(&p.shape).map_err(err_to_js)?;
    let pack = build_render_pack(p, n, spec_idx).map_err(err_to_js)?;
    let mut guard = gpu_state().borrow_mut();
    let state = guard
        .as_mut()
        .ok_or_else(|| JsError::new("gpu_set_render_data called before gpu_init"))?;
    state.pack = Some(pack);
    Ok(())
}

/// 保持 pack の時刻 `t`（0..1、シェーダ側で clamp）のフレームを canvas に描いて
/// present する。requestAnimationFrame ごとに呼ぶ想定。
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
    let pack = state
        .pack
        .as_ref()
        .ok_or_else(|| JsError::new("gpu_render called before gpu_set_render_data"))?;

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
    state.renderer.render_packed_to_view(
        pack,
        state.config.width,
        state.config.height,
        t,
        &view,
        state.config.format,
    );
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
