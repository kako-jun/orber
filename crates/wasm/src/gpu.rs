//! #230: ブラウザ WebGPU canvas present 経路（#207 Phase 2 スライス 2/5）。
//!
//! orber-core の `GpuRenderer`（WGSL）で `<canvas>` に直接描く最小経路。
//! main thread（gpu-lab / AbPanel、[`gpu_init`]）と Worker（本番生成経路、
//! [`gpu_init_offscreen`]、#245）の両方から使う。公開 API:
//!
//! - [`gpu_init`]\(canvas\) — async。instance → canvas surface → adapter
//!   （`compatible_surface` 付き）→ device の順に bring-up し、surface を
//!   configure して renderer を構築する。WebGPU 不在（adapter 無し）は明確な
//!   エラーで reject する。**fallback は無い**（#207 方針。wgpu の `webgl`
//!   feature は採らない）。
//! - [`gpu_init_offscreen`]\(canvas\) — #245。`OffscreenCanvas` 版の init。
//!   Worker（`orberWorker.ts`）の本番生成経路用で、bring-up は [`gpu_init`] と
//!   完全共有（[`init_for_target`]。surface target だけ
//!   `SurfaceTarget::OffscreenCanvas`）。Worker 内では wgpu が
//!   `DedicatedWorkerGlobalScope` の `navigator.gpu` を自動で引く。
//! - [`gpu_set_render_data`]\(params_js, n, spec_idx\) — `get_render_data` と
//!   同じ spec 解決経路（`build_gpu_render_inputs` → 共有の `resolve_frame`）で
//!   shape 別の描画入力（clusters + opts + orb 用 pack）を構築して保持する。入力は
//!   spec ごとに静的で、モーションは `t` がシェーダ内で駆動する（WebGL 版と同じ構造）。
//! - [`gpu_render`]\(t\) — surface frame を acquire し、保持入力 + `t` で
//!   `opts.shape` 別の core 経路（`render_packed_to_view` / `render_frame_*_to_view`）
//!   → present。
//! - [`gpu_render_rgba`]\(t\) — #245。canvas を経由せず内部テクスチャに 1 フレーム
//!   描いて **straight-alpha RGBA バイト列**を読み戻す async 経路。透過 export
//!   （bg.a=0）用: WebGPU canvas の alphaMode は `opaque` / `premultiplied` しか
//!   なく、straight alpha をそのまま canvas から取り出せないため、native CLI の
//!   readback と同じ「texture → padded buffer → map」をブラウザ向けに async で行う。
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
/// queue は #245 の readback 経路（`gpu_render_rgba` の texture→buffer copy
/// submit）用に同じ理由で clone を持つ。
struct GpuState {
    renderer: GpuRenderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
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
    let width = canvas.width().max(1);
    let height = canvas.height().max(1);
    init_for_target(wgpu::SurfaceTarget::Canvas(canvas), width, height).await
}

/// [`gpu_init`] の `OffscreenCanvas` 版（#245）。Worker（`orberWorker.ts`）の
/// 本番生成経路が使う。bring-up・surface format / alpha mode の選択・エラー
/// 文言は [`gpu_init`] と完全共有（[`init_for_target`]）で、surface target が
/// `SurfaceTarget::OffscreenCanvas` になるだけ。`navigator.gpu` は wgpu が
/// 実行コンテキスト（Window / DedicatedWorkerGlobalScope）から自動で引く。
#[wasm_bindgen]
pub async fn gpu_init_offscreen(canvas: web_sys::OffscreenCanvas) -> Result<String, JsError> {
    let width = canvas.width().max(1);
    let height = canvas.height().max(1);
    init_for_target(wgpu::SurfaceTarget::OffscreenCanvas(canvas), width, height).await
}

/// [`gpu_init`] / [`gpu_init_offscreen`] の共通本体。instance → surface →
/// adapter（`compatible_surface` 付き）→ device の bring-up と surface
/// configure を行い、`GpuState` を据える。成功時は adapter 名を返す。
async fn init_for_target(
    target: wgpu::SurfaceTarget<'static>,
    width: u32,
    height: u32,
) -> Result<String, JsError> {
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

    let surface = instance
        .create_surface(target)
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

    let renderer =
        GpuRenderer::from_device_queue(device.clone(), queue.clone(), adapter_name.clone());
    *gpu_state().borrow_mut() = Some(GpuState {
        renderer,
        device,
        queue,
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
    dispatch_render_to_view(&state.renderer, f, t, &view, format, width, height);
    frame.present();
    Ok(())
}

/// 保持入力 1 フレーム分を任意の view へ描く shape 別ディスパッチ（CLI の
/// `FrameRenderer::render` と同じ構造）。Orb は #230 の pack 経路を温存して
/// 見た目を一切変えない。glyph / image / aquarelle は clusters + opts を core の
/// 専用 to_view 経路へ渡す（SDF / aquarelle pack の面倒は core が見る）。
/// [`gpu_render`]（surface present）と [`gpu_render_rgba`]（#245 readback）で共有。
fn dispatch_render_to_view(
    renderer: &GpuRenderer,
    f: &FrameInputs,
    t: f32,
    view: &wgpu::TextureView,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) {
    match &f.opts.shape {
        OrbShape::Glyph { .. } => {
            renderer.render_frame_glyph_to_view(&f.clusters, &f.opts, t, view, format)
        }
        OrbShape::Image { .. } => {
            renderer.render_frame_image_to_view(&f.clusters, &f.opts, t, view, format)
        }
        OrbShape::Aquarelle(_) => {
            renderer.render_frame_aquarelle_to_view(&f.clusters, &f.opts, t, view, format)
        }
        OrbShape::Orb => renderer.render_packed_to_view(&f.pack, width, height, t, view, format),
    }
}

/// 保持入力の時刻 `t` の 1 フレームを **canvas を経由せず**内部テクスチャに
/// 描き、straight-alpha RGBA バイト列（行優先、`width * height * 4`）として
/// 読み戻す（#245）。
///
/// 透過 export（`WasmParams.transparent_background` で bg.a=0）用の経路。
/// WebGPU canvas の alphaMode は `opaque`（alpha 破棄）/ `premultiplied`
/// （straight で書く orber のシェーダ出力と不整合）しかなく、旧 WebGL
/// （`alpha: true, premultipliedAlpha: false`）のように straight alpha を
/// canvas からそのまま取り出せない。そこで native CLI の readback
/// （`render_packed` 系）と同じ texture → padded buffer → map をブラウザ向けに
/// async（`map_async` + Promise）で行う。シェーダ出力をそのまま返すので
/// alpha は bit-exact に保たれる。
///
/// テクスチャ / バッファは呼び出しごとに作って捨てる: 192 frame の透過動画
/// でも確保コストは readback 自体に比べ軽微で、`await` を跨いで共有資源を
/// 持たないことで並行呼び出し（worker の message interleave）にも安全になる。
/// サイズは surface config と同じ `width × height`（呼び出し側は canvas /
/// `gpu_resize` と `params.width/height` を一致させる既存契約のまま）。
#[wasm_bindgen]
pub async fn gpu_render_rgba(t: f32) -> Result<js_sys::Uint8Array, JsError> {
    const BYTES_PER_PIXEL: u32 = 4;
    // フェーズ 1（同期・単一 borrow 内）: 描画 → copy → submit まで終わらせ、
    // await を跨ぐのはローカルの buffer だけにする（RefCell borrow を await
    // 越しに保持しない）。
    let (buffer, width, height, padded_bytes_per_row) = {
        let guard = gpu_state().borrow_mut();
        let state = guard
            .as_ref()
            .ok_or_else(|| JsError::new("gpu_render_rgba called before gpu_init"))?;
        let f = state
            .frame
            .as_ref()
            .ok_or_else(|| JsError::new("gpu_render_rgba called before gpu_set_render_data"))?;

        let width = state.config.width;
        let height = state.config.height;
        // readback は常に Rgba8Unorm（native の `build_sized_resources` と同じ）。
        // surface format（Bgra8Unorm の可能性あり）とは独立で、PNG / ImageData の
        // RGBA 並びにそのまま渡せる。
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-wasm-readback-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        dispatch_render_to_view(&state.renderer, f, t, &view, format, width, height);

        // texture→buffer copy は bytes_per_row の 256 byte alignment が必須
        // （native readback と同じ padding。読み出し時に行ごとに unpad する）。
        let unpadded_bytes_per_row = width * BYTES_PER_PIXEL;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("orber-wasm-readback-buffer"),
            size: (padded_bytes_per_row as u64) * (height as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("orber-wasm-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        state.queue.submit(Some(encoder.finish()));
        (buffer, width, height, padded_bytes_per_row)
    };

    // フェーズ 2: map_async を Promise 化して await。WebGPU バックエンドの
    // map_async は内部の `GPUBuffer.mapAsync` Promise 解決で callback が
    // 発火するため、native のような blocking poll は不要。
    let slice = buffer.slice(..);
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        slice.map_async(wgpu::MapMode::Read, move |res| match res {
            Ok(()) => {
                let _ = resolve.call0(&JsValue::UNDEFINED);
            }
            Err(e) => {
                let _ = reject.call1(&JsValue::UNDEFINED, &JsValue::from_str(&e.to_string()));
            }
        });
    });
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| {
            JsError::new(&format!(
                "WebGPU: readback buffer map failed: {}",
                e.as_string().unwrap_or_else(|| format!("{e:?}"))
            ))
        })?;

    // フェーズ 3: 行ごとに padding を落として詰め直す（native readback と同形）。
    let unpadded_bytes_per_row = (width * BYTES_PER_PIXEL) as usize;
    let mut out = Vec::with_capacity(unpadded_bytes_per_row * height as usize);
    {
        let data = slice.get_mapped_range();
        for row in 0..height as usize {
            let start = row * padded_bytes_per_row as usize;
            out.extend_from_slice(&data[start..start + unpadded_bytes_per_row]);
        }
    }
    buffer.unmap();
    Ok(js_sys::Uint8Array::from(&out[..]))
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
