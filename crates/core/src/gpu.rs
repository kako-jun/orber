//! wgpu (Rust + WGSL) production render path — orber #207 Phase 0.
//!
//! [`GpuRenderer`] is the headless, native side of the renderer. It runs the
//! **Circle** orb WGSL ([`orb_circle.wgsl`](../src/orb_circle.wgsl)) that is a
//! faithful translation of the browser's `web/src/lib/orberGl.ts` Circle arm, so
//! the native CLI and the web produce matching Circle frames, and so the GPU path
//! matches the CPU (tiny-skia) oracle ([`crate::animate::render_frame`]) within
//! ±2/channel.
//!
//! ## Parity scope (narrow — do not over-claim)
//!
//! Parity with the CPU oracle holds **only** on this exact path:
//!
//! - **shape = Circle** (Glyph / image / aquarelle are Phase 1; the CLI routes
//!   those to the CPU renderer);
//! - **saturation reflected**: [`GpuRenderer::render_frame`] re-applies
//!   [`adjust_saturation_pub`](crate::orb::adjust_saturation_pub) with
//!   `opts.saturation` to each packed orb color after
//!   [`pack_render_data_for_webgl`] (which itself never applies saturation,
//!   because it is shared with the WebGL path). Without that step the GPU and CPU
//!   would diverge for `--saturation != 1.0`;
//! - **count up to [`MAX_ORB_COUNT`] (1024)**: per-orb data is uploaded as a
//!   **data-texture** (`Rgba32Float`, read with `textureLoad`) — not a fixed-size
//!   uniform array — so the WGSL has **no 64-orb cap** and the GPU renders the same
//!   count the CPU does (#210 Phase 1a). The old `count > 64 → CPU` fallback in the
//!   CLI is gone. (The 64 limit only ever applied to the WebGL GLSL path —
//!   `web/src/lib/orberGl.ts::MAX_ORBS` / `crates/wasm/src/lib.rs::GL_RENDERER_MAX_ORBS`
//!   — which is untouched until Phase 3; do not re-sync a 64 cap onto this path.)
//!
//! Outside that path the GPU is **not** a drop-in for the CPU oracle: video is
//! rendered on the CPU, and the per-orb `color_tracks` / `keyframe_tracks` (#7 /
//! #33) are not yet folded into the GPU pack, so animated color/position tracks
//! are CPU-only for now.
//!
//! ## Parity contract (must match [`crate::animate::render_frame`] for Circle)
//!
//! The CPU oracle composites orbs in **straight sRGB byte space** (tiny-skia
//! premultiplied internally, then un-premultiplied back to straight on output).
//! To match it the GPU path:
//!
//! - renders into an [`wgpu::TextureFormat::Rgba8Unorm`] target (NOT `*Srgb`), so
//!   no sRGB↔linear conversion happens — the shader's float blend maps to bytes
//!   by `round(value * 255)`, the same arithmetic as the CPU loop;
//! - feeds the shader the **exact same per-orb data** the WebGL path uses
//!   ([`crate::animate::pack_render_data_for_webgl`]): the parameter arithmetic is
//!   reused, never reimplemented, so the orb positions / radii / alphas are
//!   identical to the proven web/CPU result. The per-orb data goes up as a
//!   `Rgba32Float` data-texture (3 texels wide × N orbs tall) so float precision is
//!   preserved exactly — no quantization happens on the orb data itself;
//! - reads the result back accounting for wgpu's 256-byte row-alignment
//!   requirement on `copy_texture_to_buffer` (that alignment applies only to the
//!   texture→buffer read-back; the orb data upload via `write_texture` is exempt).
//!
//! ## Scope
//!
//! Phase 0 covers **Circle orbs only**. Glyph / image shapes / aquarelle are
//! Phase 1; the CLI falls back to the CPU renderer for non-Circle shapes.

use std::collections::HashMap;

use image::RgbaImage;
use wgpu::util::DeviceExt;

use crate::animate::{pack_render_data_for_webgl, AnimateOptions, MotionDirection, MAX_ORB_COUNT};
use crate::cluster::Cluster;
use crate::orb::adjust_saturation_pub;

/// Bytes per pixel for `Rgba8Unorm`.
const BYTES_PER_PIXEL: u32 = 4;
/// wgpu requires `bytes_per_row` of a texture→buffer copy to be a multiple of
/// this (`COPY_BYTES_PER_ROW_ALIGNMENT`). This applies to the read-back
/// (texture→buffer) only — `write_texture` (buffer/CPU→texture) is exempt, so the
/// orb data-texture upload uses its tight 48-byte rows directly.
const ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// Width, in texels, of the per-orb data-texture: one texel each for the color,
/// phase, and misc `vec4`s (see `orb_circle.wgsl::load_orb`).
const ORB_TEX_WIDTH: u32 = 3;
/// Bytes per texel of the `Rgba32Float` orb data-texture (4 × f32).
const ORB_TEX_BYTES_PER_TEXEL: u32 = 16;
/// Bytes per row of the orb data-texture (`3 × 16 = 48`). `write_texture` has no
/// row-alignment requirement, so this tight value is used as-is.
const ORB_TEX_BYTES_PER_ROW: u32 = ORB_TEX_WIDTH * ORB_TEX_BYTES_PER_TEXEL;

/// Header words / per-orb words in the `pack_render_data_for_webgl` layout.
/// Kept in sync with that function (header 16 words, per-orb 16 words).
const HEADER_WORDS: usize = 16;
const PER_ORB_WORDS: usize = 16;

/// The Circle orb WGSL (translation of `orberGl.ts` Circle arm). Per-orb data is
/// read from a data-texture with `textureLoad`, so the shader has no fixed orb
/// cap and needs no template substitution — it is used verbatim (#210 Phase 1a).
/// The `&'static str` doubles as a stable cache key for the pipeline cache.
fn orb_circle_wgsl() -> &'static str {
    include_str!("orb_circle.wgsl")
}

/// Header uniform block handed to the Circle shader. Mirrors `struct Params` in
/// `orb_circle.wgsl`. `#[repr(C)]` + explicit padding to satisfy WGSL std140-ish
/// uniform layout (vec2 then scalars packed into 16-byte rows).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    // row 0: resolution.xy, t, base_radius
    resolution: [f32; 2],
    t: f32,
    base_radius: f32,
    // row 1: bg.rgba
    bg: [f32; 4],
    // row 2: base_blur, direction, cycle, n_orbs
    base_blur: f32,
    direction: f32,
    cycle: f32,
    n_orbs: f32,
    // row 3: alpha_mul + padding
    alpha_mul: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// One orb as the Circle shader sees it: three `vec4`s mirroring `struct Orb` in
/// `orb_circle.wgsl` (color+weight, phase quartet, misc). Filled from the
/// `pack_render_data_for_webgl` per-orb words. One `GpuOrb` packs to one row of
/// the `Rgba32Float` orb data-texture (3 texels = 48 bytes); the shader reads it
/// back with three `textureLoad`s.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuOrb {
    color: [f32; 4], // r, g, b, weight
    phase: [f32; 4], // phase, phi_radius, phi_blur, phi_opacity
    misc: [f32; 4],  // cross_axis, style_bit, speed_mult, _
}

/// A render pipeline plus its bind-group layout, compiled once per distinct
/// shader source. Caching keeps shader compilation / pipeline creation off the
/// per-frame path: a long video renders the same shader for every frame.
struct CachedPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

/// Per-dimension GPU resources reused across same-sized frames: the render
/// target and the padded read-back buffer. Reallocating these every frame is the
/// other half of the per-frame cost the cache removes.
struct SizedResources {
    width: u32,
    height: u32,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    output_buffer: wgpu::Buffer,
    padded_bytes_per_row: u32,
}

/// The per-orb data-texture (`Rgba32Float`, 3 texels wide × `capacity` orbs
/// tall) and its view, reused across frames. `capacity` is the texture's current
/// height in orbs; it only ever grows (a frame needing more rows reallocates to
/// the larger size), mirroring the grow-only spirit of the other caches so a long
/// clip allocates the orb texture at most a handful of times.
struct OrbTexture {
    capacity: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Headless wgpu renderer for the Circle orb path. Holds a device/queue plus a
/// per-shader pipeline cache and a per-size resource cache, so a multi-frame
/// render (a long `--duration-ms` video) compiles the shader and allocates the
/// target/read-back buffer only once instead of every frame.
///
/// The caches are unbounded and never evict; for the single-resolution clip use
/// case this is intentional. A caller streaming arbitrarily many sizes through
/// one long-lived renderer should drop and rebuild it to release memory.
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_name: String,
    // Caches use `Mutex` (not `RefCell`) so `GpuRenderer` is `Sync`: the parity
    // tests share one `&'static GpuRenderer` across threads (built once via
    // `OnceLock`) to stop several `wgpu::Instance`/adapter/device bring-ups from
    // racing under the default multi-threaded `cargo test`. wgpu's `Device`/`Queue`
    // are already `Send + Sync`; only these interior-mutable caches needed locking.
    // The CLI uses one renderer single-threaded, so the lock is uncontended there.
    /// Circle pipelines, keyed by shader source.
    pipeline_cache: std::sync::Mutex<HashMap<String, CachedPipeline>>,
    /// Per-size resources, keyed by `(width, height)`.
    sized_cache: std::sync::Mutex<HashMap<(u32, u32), SizedResources>>,
    /// The grow-only per-orb data-texture (reallocated only when a frame needs
    /// more rows than the cached capacity). `None` until the first frame.
    orb_texture: std::sync::Mutex<Option<OrbTexture>>,
}

impl GpuRenderer {
    /// Bring up a headless GPU context (no surface). Returns `None` when no
    /// adapter is available (e.g. CI without a GPU / software rasterizer), so
    /// callers can fall back to the CPU path instead of hard-failing.
    pub fn new() -> Option<Self> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> Option<Self> {
        // Headless: no window/display handle is needed (backends still come from env).
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok()?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("orber-gpu-device"),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Self {
            device,
            queue,
            adapter_name,
            pipeline_cache: std::sync::Mutex::new(HashMap::new()),
            sized_cache: std::sync::Mutex::new(HashMap::new()),
            orb_texture: std::sync::Mutex::new(None),
        })
    }

    /// Name of the underlying adapter (for diagnostics / proving the GPU path ran).
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Live entry counts of the two caches, `(pipelines, sizes)`. Exposed for the
    /// cache-effectiveness test: rendering a clip of many frames at one size with
    /// one shader must leave exactly one pipeline and one sized entry.
    #[cfg(test)]
    fn cache_sizes(&self) -> (usize, usize) {
        (
            self.pipeline_cache.lock().unwrap().len(),
            self.sized_cache.lock().unwrap().len(),
        )
    }

    /// Get-or-build the Circle pipeline for `shader_wgsl`, compiling the shader and
    /// pipeline only on first use. The closure runs at most once per distinct
    /// shader source for the life of the renderer.
    fn pipeline<R>(&self, shader_wgsl: &str, f: impl FnOnce(&CachedPipeline) -> R) -> R {
        let mut cache = self.pipeline_cache.lock().unwrap();
        let entry = cache
            .entry(shader_wgsl.to_owned())
            .or_insert_with(|| self.build_pipeline(shader_wgsl));
        f(entry)
    }

    /// Compile the Circle pipeline (binding 0 = `Params` uniform, binding 1 = orb
    /// data-texture). The orb texture is `Rgba32Float`, sampled with `filterable:
    /// false` and read via `textureLoad` (no sampler) so the path never depends on
    /// linear filtering and stays portable to wgpu's WebGL2 backend (#210).
    fn build_pipeline(&self, shader_wgsl: &str) -> CachedPipeline {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("orber-circle-bgl"),
                    entries: &[uniform_entry(0), orb_texture_entry(1)],
                });
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("orber-circle-shader"),
                source: wgpu::ShaderSource::Wgsl(shader_wgsl.into()),
            });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("orber-circle-pl"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("orber-circle-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        CachedPipeline {
            pipeline,
            bind_group_layout,
        }
    }

    /// Get-or-build the per-size resources, allocating the target texture and
    /// read-back buffer only on first use of a `(width, height)`.
    fn sized_resources<R>(
        &self,
        width: u32,
        height: u32,
        f: impl FnOnce(&SizedResources) -> R,
    ) -> R {
        let mut map = self.sized_cache.lock().unwrap();
        let entry = map
            .entry((width, height))
            .or_insert_with(|| Self::build_sized_resources(&self.device, width, height));
        f(entry)
    }

    /// Allocate the target texture and the padded read-back buffer for a size.
    fn build_sized_resources(device: &wgpu::Device, width: u32, height: u32) -> SizedResources {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-circle-target"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let unpadded_bytes_per_row = width * BYTES_PER_PIXEL;
        let padded_bytes_per_row = align_up(unpadded_bytes_per_row, ROW_ALIGNMENT);
        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("orber-circle-readback"),
            size: (padded_bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        SizedResources {
            width,
            height,
            target,
            target_view,
            output_buffer,
            padded_bytes_per_row,
        }
    }

    /// Upload the per-orb data into the grow-only `Rgba32Float` data-texture and
    /// return a view to bind. The texture is 3 texels wide (color / phase / misc)
    /// × `orbs.len()` tall; it is reallocated only when the orb count exceeds the
    /// cached capacity, then `write_texture` fills the live rows each frame.
    ///
    /// `write_texture` has no 256-byte row-alignment requirement (that is only for
    /// buffer→texture copies), so the tight `ORB_TEX_BYTES_PER_ROW` (48) is used.
    fn upload_orb_texture(&self, orbs: &[GpuOrb]) -> wgpu::TextureView {
        let rows = orbs.len().max(1) as u32;
        let mut guard = self.orb_texture.lock().unwrap();
        let needs_realloc = match guard.as_ref() {
            Some(tex) => tex.capacity < rows,
            None => true,
        };
        if needs_realloc {
            *guard = Some(self.build_orb_texture(rows));
        }
        let tex = guard.as_ref().expect("orb texture just ensured present");

        // `write_texture` reads exactly `rows × 3 × 16` bytes from `orbs`, which is
        // `bytemuck`-castable to a flat `&[u8]` (GpuOrb is 3 × vec4<f32> = 48 bytes,
        // matching one texel row).
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(orbs),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ORB_TEX_BYTES_PER_ROW),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width: ORB_TEX_WIDTH,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
        tex.view.clone()
    }

    /// Allocate the per-orb data-texture sized for `capacity` orbs (3 texels wide).
    /// `usage = TEXTURE_BINDING | COPY_DST` (sampled in the shader, written via
    /// `write_texture`). No `RENDER_ATTACHMENT` / `COPY_SRC` — it is input only.
    fn build_orb_texture(&self, capacity: u32) -> OrbTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-circle-orb-tex"),
            size: wgpu::Extent3d {
                width: ORB_TEX_WIDTH,
                height: capacity,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        OrbTexture {
            capacity,
            texture,
            view,
        }
    }

    /// Render one Circle frame at time `t` from `clusters` + `opts`, matching
    /// [`crate::animate::render_frame`].
    ///
    /// The per-orb data is computed by [`pack_render_data_for_webgl`] — the same
    /// arithmetic the WebGL path uses — so the GPU output matches the CPU oracle
    /// within ±2/channel. `opts.width` / `opts.height` give the output size; `t`
    /// is clamped to `0.0..=1.0`.
    ///
    /// # Orb count
    ///
    /// The resolved orb count is clamped only to [`MAX_ORB_COUNT`] (1024), the same
    /// ceiling the CPU oracle uses. Per-orb data is uploaded as a data-texture that
    /// grows to fit, so there is no 64-orb cap and the GPU renders the same image
    /// the CPU does for any count up to 1024 (#210 Phase 1a). No CPU fallback for
    /// `count > 64` is needed anymore.
    ///
    /// # Panics / scope
    ///
    /// This is the **Circle** path only. The shape in `opts.shape` is ignored
    /// here (the caller is responsible for routing non-Circle shapes to the CPU
    /// renderer); see the module docs.
    pub fn render_frame(&self, clusters: &[Cluster], opts: &AnimateOptions, t: f32) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);

        // Derive the WebGL pack-buffer scalars exactly as the CPU oracle /
        // `get_render_data` do, then reuse `pack_render_data_for_webgl` so the
        // per-orb arithmetic is never reimplemented.
        let base_radius_unit = (width.min(height) as f32) * 0.25 * opts.orb_size.max(0.0);
        let base_blur = (opts.blur + opts.softness.blur_offset()).clamp(0.0, 1.0);
        let alpha_mul = opts.softness.alpha_mul().clamp(0.0, 1.0);
        let direction_id: f32 = match opts.direction {
            MotionDirection::LeftToRight => 0.0,
            MotionDirection::RightToLeft => 1.0,
            MotionDirection::TopToBottom => 2.0,
            MotionDirection::BottomToTop => 3.0,
        };
        let cycle = opts.speed.cycle_count() as f32;
        // n_orbs mirrors `precompute_orb_params`: count.unwrap_or(clusters.len())
        // clamped to MAX_ORB_COUNT, at least 1 if there are clusters.
        let n_orbs = opts
            .count
            .unwrap_or(clusters.len())
            .min(MAX_ORB_COUNT)
            .max(if clusters.is_empty() { 0 } else { 1 });
        // shape_id / glyph_rotate / edge_softness are Phase-1 (Glyph) inputs; the
        // Circle shader ignores them. Pass Circle defaults.
        let mut pack = pack_render_data_for_webgl(
            clusters,
            opts.background,
            base_radius_unit,
            base_blur,
            direction_id,
            cycle,
            opts.seed,
            n_orbs,
            alpha_mul,
            0.0,  // shape_id = Circle
            true, // glyph_rotate (unused by Circle)
            opts.softness.edge_softness(),
        );

        // `pack_render_data_for_webgl` is shared with the WebGL path and must NOT
        // bake in saturation (the web side has its own knob). The CPU oracle,
        // however, applies `adjust_saturation_pub(color_at_t, saturation)` per orb
        // (`animate::render_frame`). To stay bit-exact we re-apply the *same*
        // function here, in native GPU land only, over the packed color words.
        //
        // Each color word triple is `c.color[i] as f32 / 255.0`, so `round(w*255)`
        // recovers the exact u8 the CPU fed to `adjust_saturation_pub`; we run the
        // identical HSL transform and write the result back as `u8 / 255.0`.
        apply_saturation_to_pack(&mut pack, opts.saturation.max(0.0), n_orbs);

        self.render_packed(&pack, width, height, t)
    }

    /// Render one Circle frame from a raw `pack_render_data_for_webgl` buffer.
    ///
    /// `pack` must be the header(16) + per-orb(16 × n_orbs) layout produced by
    /// [`pack_render_data_for_webgl`]. `t` is the normalized time written into the
    /// shader's `u_t`; it is clamped to `0.0..=1.0`. This is the seam the WebGL
    /// path will also share (Phase 2).
    pub fn render_packed(&self, pack: &[f32], width: u32, height: u32, t: f32) -> RgbaImage {
        let width = width.max(1);
        let height = height.max(1);
        let t = t.clamp(0.0, 1.0);

        assert!(
            pack.len() >= HEADER_WORDS,
            "pack buffer too short: {} < {HEADER_WORDS}",
            pack.len()
        );

        // Header → Params. Layout per `pack_render_data_for_webgl` doc-comment.
        // count is clamped to MAX_ORB_COUNT (1024); the data-texture grows to hold
        // it, so there is no 64-orb cap here anymore (#210).
        let n_orbs_packed = pack[8].max(0.0) as usize;
        let n_orbs = n_orbs_packed.min(MAX_ORB_COUNT);
        let params = Params {
            resolution: [width as f32, height as f32],
            t,
            base_radius: pack[4],
            bg: [pack[0], pack[1], pack[2], pack[3]],
            base_blur: pack[5],
            direction: pack[6],
            cycle: pack[7],
            n_orbs: n_orbs as f32,
            alpha_mul: pack[9],
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("orber-circle-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        // Per-orb words → one `GpuOrb` (3 vec4s) per orb. Only the words the Circle
        // shader reads are unpacked (color+weight, phase quartet,
        // cross_axis/style/speed); rotation words are skipped. The shader iterates
        // `params.n_orbs` rows, so the row count must equal `n_orbs` even if an
        // (externally hand-built) `pack` runs short — short rows stay zeroed.
        let mut orbs = vec![
            GpuOrb {
                color: [0.0; 4],
                phase: [0.0; 4],
                misc: [0.0; 4],
            };
            n_orbs.max(1)
        ];
        for (i, slot) in orbs.iter_mut().enumerate().take(n_orbs) {
            let off = HEADER_WORDS + PER_ORB_WORDS * i;
            // Max word read below is `pack[off + 10]`, so the guard must allow
            // `off + 10 == len - 1`, i.e. `off + 11 == len`. Using `>=` here would
            // wrongly break one orb early when an externally hand-built buffer is
            // sized to exactly `off + 11`; the correct cut-off is `off + 11 > len`.
            if off + 11 > pack.len() {
                break;
            }
            *slot = GpuOrb {
                color: [pack[off], pack[off + 1], pack[off + 2], pack[off + 3]],
                phase: [pack[off + 4], pack[off + 5], pack[off + 6], pack[off + 7]],
                misc: [pack[off + 8], pack[off + 9], pack[off + 10], 0.0],
            };
        }

        // Upload the per-orb data into the grow-only data-texture and grab a
        // (clonable) view to bind. Done before entering the pipeline/sized
        // closures so we don't nest the orb-texture lock under them.
        let orb_view = self.upload_orb_texture(&orbs);

        // Pipeline (shader compile) cached per shader source; target / read-back
        // cached per size; orb texture grows as needed. Only the small params
        // uniform / bind group are rebuilt per frame.
        self.pipeline(orb_circle_wgsl(), |cached| {
            self.sized_resources(width, height, |res| {
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("orber-circle-bg"),
                    layout: &cached.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: params_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&orb_view),
                        },
                    ],
                });
                self.run_pass_and_readback(&cached.pipeline, &bind_group, res)
            })
        })
    }

    /// Render one full-screen pass into `res.target`, copy it into the read-back
    /// buffer, map it, and strip wgpu's row padding into a tight `RgbaImage`.
    fn run_pass_and_readback(
        &self,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
        res: &SizedResources,
    ) -> RgbaImage {
        let (width, height) = (res.width, res.height);
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("orber-circle-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("orber-circle-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &res.target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &res.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &res.output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(res.padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = res.output_buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed");

        let unpadded_bytes_per_row = width * BYTES_PER_PIXEL;
        let mapped = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in 0..height {
            let start = (row * res.padded_bytes_per_row) as usize;
            let end = start + unpadded_bytes_per_row as usize;
            pixels.extend_from_slice(&mapped[start..end]);
        }
        drop(mapped);
        res.output_buffer.unmap();

        RgbaImage::from_raw(width, height, pixels)
            .expect("read-back buffer matches image dimensions")
    }
}

/// Round `value` up to the next multiple of `align` (a power of two).
fn align_up(value: u32, align: u32) -> u32 {
    value.div_ceil(align) * align
}

/// Apply `adjust_saturation_pub` to the per-orb color words of a
/// `pack_render_data_for_webgl` buffer, in place (native GPU path only).
///
/// `pack_render_data_for_webgl` is shared with the WebGL path and intentionally
/// leaves saturation out, but the CPU oracle applies it per orb, so the native
/// GPU path re-applies the identical transform here to keep bit-exact parity.
/// Color words live at `[off .. off+3]` per orb as `u8 / 255.0`; we round back to
/// the original u8, run the same HSL adjust, and write the result back the same
/// way. A factor of `1.0` is the `adjust_saturation_pub` fast-path (no change).
fn apply_saturation_to_pack(pack: &mut [f32], saturation: f32, n_orbs: usize) {
    for i in 0..n_orbs {
        let off = HEADER_WORDS + PER_ORB_WORDS * i;
        // Bounds: we touch [off, off+2]. `off + 3 > len` means the color triple
        // does not fit, so stop.
        if off + 3 > pack.len() {
            break;
        }
        let rgb = [
            (pack[off] * 255.0).round() as u8,
            (pack[off + 1] * 255.0).round() as u8,
            (pack[off + 2] * 255.0).round() as u8,
        ];
        let out = adjust_saturation_pub(rgb, saturation);
        pack[off] = out[0] as f32 / 255.0;
        pack[off + 1] = out[1] as f32 / 255.0;
        pack[off + 2] = out[2] as f32 / 255.0;
    }
}

/// A fragment-visible uniform-buffer bind-group-layout entry.
fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// A fragment-visible sampled-texture bind-group-layout entry for the orb
/// data-texture. `sample_type = Float { filterable: false }` because the shader
/// reads it with `textureLoad` (no sampler / no filtering), which is what keeps
/// the path portable to the wgpu WebGL2 backend where `Rgba32Float` is not
/// filterable (#210).
fn orb_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animate::{render_frame, AnimateOptions, MotionDirection, MotionSpeed};
    use crate::cluster::{Centroid, Cluster};
    use crate::orb::OrbShape;
    use crate::style::SoftnessPreset;

    fn cluster(color: [u8; 3], cx: f32, cy: f32, weight: f32) -> Cluster {
        Cluster {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight,
        }
    }

    /// Turn a missing adapter into the right outcome for `what`:
    ///
    /// - **`ORBER_REQUIRE_GPU=1`** (CI / any environment that must actually verify
    ///   parity): a missing adapter is a hard **`panic!`** — the test fails loudly
    ///   instead of silently passing. This is what stops the "no GPU ⇒ green
    ///   without checking anything" false-positive.
    /// - **unset** (developer machine without a GPU): the caller skips, so a
    ///   missing adapter just prints a SKIP line. This keeps the suite convenient
    ///   locally while making "skipped" and "really verified" distinguishable.
    ///
    /// Under `ORBER_REQUIRE_GPU=1` this `panic!`s and never returns; otherwise it
    /// prints the SKIP line and returns, leaving the caller to skip. `what` names
    /// the test for the message.
    fn require_gpu_or_panic(what: &str) {
        if std::env::var("ORBER_REQUIRE_GPU").as_deref() == Ok("1") {
            panic!(
                "{what}: ORBER_REQUIRE_GPU=1 but no GPU adapter is available; \
                 parity could not be verified (install a Vulkan ICD, e.g. \
                 mesa-vulkan-drivers + VK_ICD_FILENAMES=lvp_icd, or unset \
                 ORBER_REQUIRE_GPU to allow skipping)"
            );
        }
        eprintln!(
            "SKIP {what}: no GPU adapter available (set ORBER_REQUIRE_GPU=1 to fail instead)"
        );
    }

    /// A single `GpuRenderer` shared by the whole parity-test group.
    ///
    /// `cargo test` is multi-threaded by default, and each parity test used to
    /// build its own `wgpu::Instance` + adapter + device. On a real GPU, several
    /// `request_adapter` / `request_device` calls racing at once would transiently
    /// return `None` (~1 run in 8), which `ORBER_REQUIRE_GPU=1` then turned into a
    /// hard, *spurious* failure. Bringing the context up exactly once removes that
    /// contention: wgpu's `Device` / `Queue` are `Send + Sync`, so a
    /// `&'static GpuRenderer` is safe to borrow concurrently from every test.
    static SHARED_TEST_GPU: std::sync::OnceLock<Option<GpuRenderer>> = std::sync::OnceLock::new();

    /// Get the shared parity renderer, or decide what to do when no adapter is
    /// available (panic under `ORBER_REQUIRE_GPU=1`, skip otherwise). The context
    /// is built at most once for the whole test binary, regardless of how many
    /// threads call this. `what` names the test for the panic / skip message.
    fn require_or_skip_renderer(what: &str) -> Option<&'static GpuRenderer> {
        let shared = SHARED_TEST_GPU.get_or_init(GpuRenderer::new);
        match shared {
            Some(r) => Some(r),
            None => {
                require_gpu_or_panic(what);
                None
            }
        }
    }

    /// Build a *fresh, independent* renderer for tests that need a second context
    /// (e.g. the cache-leak oracle leg). Because this is the rare single-instance
    /// path it can still race transiently with the shared context's bring-up, so
    /// retry the bring-up a few times before falling back to require/skip.
    fn require_or_skip_fresh_renderer(what: &str) -> Option<GpuRenderer> {
        for _ in 0..3 {
            if let Some(r) = GpuRenderer::new() {
                return Some(r);
            }
        }
        require_gpu_or_panic(what);
        None
    }

    /// A small varied palette so per-pixel parity isn't trivially satisfied by a
    /// flat color: several colors / weights scattered around the frame.
    fn sample_clusters() -> Vec<Cluster> {
        vec![
            cluster([220, 60, 60], 0.3, 0.4, 0.5),
            cluster([60, 120, 220], 0.7, 0.6, 0.3),
            cluster([200, 200, 80], 0.5, 0.2, 0.2),
            cluster([90, 220, 140], 0.2, 0.8, 0.25),
        ]
    }

    fn circle_opts(
        w: u32,
        h: u32,
        direction: MotionDirection,
        speed: MotionSpeed,
    ) -> AnimateOptions {
        AnimateOptions {
            width: w,
            height: h,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction,
            speed,
            seed: 12345,
            count: Some(12),
            background: [12, 18, 28, 255],
            shape: OrbShape::Circle,
            softness: SoftnessPreset::Mid,
            glyph_rotate: true,
            color_tracks: None,
            keyframe_tracks: None,
        }
    }

    /// Assert every channel of `a`/`b` agrees within `±2`, returning the max diff.
    fn assert_within_tolerance(a: &RgbaImage, b: &RgbaImage, ctx: &str) -> u8 {
        assert_eq!(a.dimensions(), b.dimensions(), "{ctx}: dimension mismatch");
        let mut max_diff = 0u8;
        let mut worst = (0u32, 0u32, 0usize, [0u8; 4], [0u8; 4]);
        for (x, y, ap) in a.enumerate_pixels() {
            let bp = b.get_pixel(x, y);
            for ch in 0..4 {
                let d = ap.0[ch].abs_diff(bp.0[ch]);
                if d > max_diff {
                    max_diff = d;
                    worst = (x, y, ch, ap.0, bp.0);
                }
            }
        }
        assert!(
            max_diff <= 2,
            "{ctx}: max per-channel diff {max_diff} at pixel ({},{}) channel {} (cpu={:?} gpu={:?})",
            worst.0,
            worst.1,
            worst.2,
            worst.3,
            worst.4,
        );
        max_diff
    }

    #[test]
    fn gpu_matches_cpu_within_tolerance() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_within_tolerance") else {
            return;
        };
        eprintln!(
            "GPU Circle parity test running on adapter: {}",
            renderer.adapter_name()
        );

        let clusters = sample_clusters();
        // Cover the read-back strip boundary both ways with orb-bearing frames:
        //   - 37x23: width*4 = 148, not a multiple of 256, so rows ARE padded;
        //   - 64x16: width*4 = 256 is already row-aligned, so NO padding.
        // (1x1 is exercised separately by `gpu_matches_cpu_1x1_readback`: at a
        // sub-pixel orb radius tiny-skia's analytic path-coverage AA diverges from
        // a point-sampled shader — a degenerate case orber never renders — so the
        // 1x1 row-padding readback is checked background-only instead.)
        let mut overall_max = 0u8;
        for &(w, h) in &[(37u32, 23u32), (64, 16)] {
            let dir = match (w, h) {
                (37, 23) => MotionDirection::LeftToRight,
                _ => MotionDirection::TopToBottom,
            };
            let opts = circle_opts(w, h, dir, MotionSpeed::Slow);
            for &t in &[0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                let cpu = render_frame(&clusters, &opts, t);
                let gpu = renderer.render_frame(&clusters, &opts, t);
                let max_diff =
                    assert_within_tolerance(&cpu, &gpu, &format!("{w}x{h} {dir:?} t={t}"));
                overall_max = overall_max.max(max_diff);
                eprintln!("{w}x{h} {dir:?} t={t}: max per-channel diff = {max_diff}");
            }
        }
        eprintln!("overall max per-channel diff across all cases = {overall_max}");
    }

    /// count > 64 must hold parity now that orb data is a data-texture (no 64
    /// cap). Covers the old CPU-fallback boundary (65) plus 100 / 256 / 1024.
    /// A size with > 64 distinct clusters so each orb is independent (not just the
    /// weight-scattered expansion of a handful of colors); the texture must grow to
    /// hold every row and the shader must iterate the full dynamic count. Expect
    /// bit-exact on a real GPU; the assertion allows the ±2/channel contract.
    #[test]
    fn gpu_matches_cpu_high_count() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_high_count") else {
            return;
        };
        eprintln!(
            "GPU Circle high-count parity test running on adapter: {}",
            renderer.adapter_name()
        );
        let clusters = sample_clusters();
        let mut overall_max = 0u8;
        for &count in &[65usize, 100, 256, 1024] {
            // A size whose width*4 is not 256-aligned, to also keep the read-back
            // padding strip in play while the orb count grows.
            let mut opts = circle_opts(50, 34, MotionDirection::LeftToRight, MotionSpeed::Mid);
            opts.count = Some(count);
            for &t in &[0.0_f32, 0.5] {
                let cpu = render_frame(&clusters, &opts, t);
                let gpu = renderer.render_frame(&clusters, &opts, t);
                let max_diff = assert_within_tolerance(&cpu, &gpu, &format!("count={count} t={t}"));
                overall_max = overall_max.max(max_diff);
                eprintln!("count={count} t={t}: max per-channel diff = {max_diff}");
            }
        }
        eprintln!("high-count overall max per-channel diff = {overall_max}");
    }

    /// `--saturation != 1.0` must hold parity: the native GPU path re-applies
    /// `adjust_saturation_pub` (the CPU oracle's own per-orb saturation) after the
    /// shared `pack_render_data_for_webgl`. Without that step desaturated (0.5) and
    /// boosted (2.0) frames would diverge from the CPU. Covers a couple of sizes /
    /// times so the orb colors actually change between frames.
    #[test]
    fn gpu_matches_cpu_with_saturation() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_with_saturation") else {
            return;
        };
        let clusters = sample_clusters();
        for &saturation in &[0.5_f32, 2.0] {
            for &(w, h) in &[(37u32, 23u32), (40, 28)] {
                let mut opts = circle_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Mid);
                opts.saturation = saturation;
                for &t in &[0.0_f32, 0.5, 1.0] {
                    let cpu = render_frame(&clusters, &opts, t);
                    let gpu = renderer.render_frame(&clusters, &opts, t);
                    let max_diff = assert_within_tolerance(
                        &cpu,
                        &gpu,
                        &format!("sat={saturation} {w}x{h} t={t}"),
                    );
                    eprintln!("sat={saturation} {w}x{h} t={t}: max per-channel diff = {max_diff}");
                }
            }
        }
    }

    /// 1x1 read-back boundary: width*4 = 4 bytes/row is padded to 256, so this
    /// exercises the most extreme row-padding strip. Use a background-only frame
    /// (no clusters) so the assertion is about read-back geometry, not the
    /// degenerate sub-pixel-orb AA where a point-sampled shader cannot match
    /// tiny-skia's analytic path coverage. Also checks 1x3 / 3x1 strips.
    #[test]
    fn gpu_matches_cpu_1x1_readback() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_1x1_readback") else {
            return;
        };
        for &(w, h) in &[(1u32, 1u32), (1, 3), (3, 1), (5, 1)] {
            let opts = circle_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow);
            for &t in &[0.0_f32, 0.5, 1.0] {
                // Background-only frame on both paths.
                let cpu = render_frame(&[], &opts, t);
                let gpu = renderer.render_frame(&[], &opts, t);
                let max_diff =
                    assert_within_tolerance(&cpu, &gpu, &format!("{w}x{h} bg-only t={t}"));
                eprintln!("{w}x{h} bg-only t={t}: max per-channel diff = {max_diff}");
            }
        }
    }

    /// All four flow directions must hold parity at a single non-trivial size/t.
    #[test]
    fn gpu_matches_cpu_all_directions() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_all_directions") else {
            return;
        };
        let clusters = sample_clusters();
        for dir in [
            MotionDirection::LeftToRight,
            MotionDirection::RightToLeft,
            MotionDirection::TopToBottom,
            MotionDirection::BottomToTop,
        ] {
            let opts = circle_opts(40, 28, dir, MotionSpeed::Mid);
            let cpu = render_frame(&clusters, &opts, 0.37);
            let gpu = renderer.render_frame(&clusters, &opts, 0.37);
            let max_diff = assert_within_tolerance(&cpu, &gpu, &format!("dir {dir:?}"));
            eprintln!("dir {dir:?}: max per-channel diff = {max_diff}");
        }
    }

    /// Empty clusters → background-only frame must still match (bg fill path).
    #[test]
    fn gpu_matches_cpu_empty_clusters() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_empty_clusters") else {
            return;
        };
        let opts = circle_opts(32, 24, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let cpu = render_frame(&[], &opts, 0.5);
        let gpu = renderer.render_frame(&[], &opts, 0.5);
        let max_diff = assert_within_tolerance(&cpu, &gpu, "empty clusters");
        eprintln!("empty clusters: max per-channel diff = {max_diff}");
    }

    /// Pipeline + sized caches must each hold exactly one entry after a clip of
    /// many same-size frames, and a second size must grow only the sized cache.
    ///
    /// Uses a *private* renderer (not the shared one): it asserts exact cache
    /// entry counts, which the shared renderer would violate because the other
    /// parity tests render it at many different sizes.
    #[test]
    fn caches_resources_across_a_clip() {
        let Some(renderer) = require_or_skip_fresh_renderer("caches_resources_across_a_clip")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts = circle_opts(48, 32, MotionDirection::LeftToRight, MotionSpeed::Slow);
        for k in 0..16 {
            let t = k as f32 / 15.0;
            let _ = renderer.render_frame(&clusters, &opts, t);
        }
        let (pipes, sizes) = renderer.cache_sizes();
        assert_eq!(pipes, 1, "shader must compile exactly once");
        assert_eq!(sizes, 1, "size must allocate exactly once");

        let opts2 = circle_opts(24, 24, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let _ = renderer.render_frame(&clusters, &opts2, 0.5);
        let (pipes, sizes) = renderer.cache_sizes();
        assert_eq!(pipes, 1, "second size must not recompile the shader");
        assert_eq!(sizes, 2, "second size must add one sized entry");
    }

    /// Reused per-size resources must not leak the previous frame's bytes: a
    /// second frame with different inputs at the same size (hitting the cache)
    /// must equal a fresh renderer's single render of those inputs.
    ///
    /// Uses a *private* renderer (not the shared one) so the `sizes == 1`
    /// assertion holds — the shared renderer accumulates sizes from other tests.
    #[test]
    fn cached_resources_do_not_leak_previous_frame() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("cached_resources_do_not_leak_previous_frame")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts_a = circle_opts(40, 24, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let mut opts_b = circle_opts(40, 24, MotionDirection::TopToBottom, MotionSpeed::Mid);
        opts_b.seed = 999;
        opts_b.background = [40, 5, 50, 255];

        let _a = renderer.render_frame(&clusters, &opts_a, 0.3);
        let b_cached = renderer.render_frame(&clusters, &opts_b, 0.7);

        let (_, sizes) = renderer.cache_sizes();
        assert_eq!(sizes, 1, "both frames must share one cached size");

        // This leg needs a *second, independent* renderer (a fresh per-size cache)
        // to prove the shared one didn't leak frame A's bytes. The shared context
        // already came up, so a second must too under ORBER_REQUIRE_GPU=1; the
        // helper retries the bring-up to absorb any transient adapter race before
        // treating a failure as real (not a skip).
        let Some(fresh) = require_or_skip_fresh_renderer(
            "cached_resources_do_not_leak_previous_frame (oracle leg)",
        ) else {
            return;
        };
        let b_fresh = fresh.render_frame(&clusters, &opts_b, 0.7);
        let max_diff =
            assert_within_tolerance(&b_fresh, &b_cached, "reused frame B vs fresh render");
        eprintln!("reuse-vs-fresh frame B: max per-channel diff = {max_diff}");
    }
}
