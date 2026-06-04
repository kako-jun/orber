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

/// Upper bound of the radius breath factor (`1.0 + 0.10`), mirroring
/// `animate::BREATH_RADIUS_MAX_FACTOR` / the WGSL constant. Used only to size the
/// glyph SDF for the frame from the largest possible orb radius.
const BREATH_RADIUS_MAX_FACTOR: f32 = 1.10;
/// wgpu requires `bytes_per_row` of a texture→buffer copy to be a multiple of
/// this (`COPY_BYTES_PER_ROW_ALIGNMENT`). This applies to the read-back
/// (texture→buffer) only — `write_texture` (buffer/CPU→texture) is exempt, so the
/// orb data-texture upload uses its tight 48-byte rows directly.
const ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// Width, in texels, of the per-orb data-texture: one texel each for the color,
/// phase, misc, and rotation `vec4`s (see `orb_circle.wgsl::load_orb` /
/// `orb_glyph.wgsl::load_orb`). Widened 3→4 in Phase 1b (#212) so the Glyph
/// shader can read the per-orb rotation (`base_angle`, `rot_speed_signed`); the
/// Circle shader ignores texel x=3 and stays bit-exact.
const ORB_TEX_WIDTH: u32 = 4;
/// Bytes per texel of the `Rgba32Float` orb data-texture (4 × f32).
const ORB_TEX_BYTES_PER_TEXEL: u32 = 16;
/// Bytes per row of the orb data-texture (`4 × 16 = 64`). `write_texture` has no
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

/// The Glyph orb WGSL (#212 Phase 1b). Same data-texture orb layout as the Circle
/// shader, plus an `R8Unorm` glyph SDF texture + bilinear sampler (bindings 2/3),
/// and reads the per-orb rotation texel (x=3). It reproduces the CPU
/// `glyph::render_glyph_orb` fill (pre-bleed), not the WebGL mask×profile arm.
fn orb_glyph_wgsl() -> &'static str {
    include_str!("orb_glyph.wgsl")
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
    // row 3: alpha_mul, glyph_rotate, edge_softness, padding
    alpha_mul: f32,
    /// Glyph rotation toggle (#136): `1.0` = animate per-orb rotation, `0.0` =
    /// hold `base_angle`. Circle ignores this. Lives in the former padding so the
    /// uniform buffer size is unchanged (Circle's WGSL `Params` struct stays
    /// valid; it simply names the slots `_pad0`/`_pad1`).
    glyph_rotate: f32,
    /// Glyph edge softness (#205): unused by the current Glyph fill (it uses
    /// `falloff_curve` like the CPU `render_glyph_orb`), reserved for the future
    /// SDF-mask path; kept so the header layout mirrors the WebGL one.
    edge_softness: f32,
    /// Glyph SDF square side in texels. The Glyph shader uses it to reproduce the
    /// CPU `sample_sdf_bilinear` convention (`coord = u*(size-1)`) when remapping
    /// UVs to the wgpu sampler's texel space. Circle ignores this slot (its WGSL
    /// `Params` names it `_pad2`). `0.0` for the Circle path.
    sdf_size: f32,
}

/// One orb as the shaders see it: four `vec4`s mirroring `struct Orb` in
/// `orb_circle.wgsl` / `orb_glyph.wgsl` (color+weight, phase quartet, misc,
/// rotation). Filled from the `pack_render_data_for_webgl` per-orb words. One
/// `GpuOrb` packs to one row of the `Rgba32Float` orb data-texture (4 texels =
/// 64 bytes); the shader reads it back with `textureLoad`s.
///
/// The Circle shader reads only `color` / `phase` / `misc` (texels x=0..2) and
/// ignores `rot` (x=3), so widening the row to 4 texels leaves Circle output
/// bit-exact. The Glyph shader additionally reads `rot = (base_angle,
/// rot_speed_signed, _, _)` for #136 rotation.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuOrb {
    color: [f32; 4], // r, g, b, weight
    phase: [f32; 4], // phase, phi_radius, phi_blur, phi_opacity
    misc: [f32; 4],  // cross_axis, style_bit, speed_mult, _
    rot: [f32; 4],   // base_angle, rot_speed_signed, _, _
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

/// A single glyph's SDF uploaded as an `R8Unorm` texture (square `size × size`),
/// cached per `(char, size)` so a video reuses the upload across frames. `R8Unorm`
/// is chosen because it is **linear-filterable on the wgpu WebGL2 backend**
/// (unlike `Rgba32Float`), so the Glyph shader can read it with a real bilinear
/// `sampler` and stay portable to Phase 2 (#212).
struct GlyphSdfTexture {
    view: wgpu::TextureView,
}

/// The glyph SDF binding passed into `render_packed_inner` for the Glyph path:
/// the (cached) SDF texture view and its square side in texels. `None` selects
/// the Circle path instead.
struct GlyphBindings<'a> {
    sdf_view: &'a wgpu::TextureView,
    size: u32,
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
    /// Glyph SDF textures keyed by `(char as u32, size)`. Grow-only (never
    /// evicts): a clip renders one glyph at one size, so this holds a single
    /// entry; supporting several glyphs in one clip just adds entries.
    glyph_sdf_cache: std::sync::Mutex<HashMap<(u32, u32), GlyphSdfTexture>>,
    /// The bilinear (linear/linear, clamp-to-edge) sampler the Glyph shader uses
    /// to read the `R8Unorm` SDF. Built once; reused for every glyph frame.
    glyph_sampler: wgpu::Sampler,
    /// Serializes the whole GPU side of [`Self::render_packed`] (orb/params
    /// upload → pass record → `queue.submit` → map/readback) so concurrent
    /// `render_frame` calls on one shared renderer cannot alias the shared cached
    /// resources (the grow-only orb texture, the per-size output texture /
    /// read-back buffer). Without it a second thread's upload could overwrite the
    /// one shared orb texture before the first thread's pass samples it, so frames
    /// render with another thread's orb colors (#210). The single-threaded CLI
    /// never contends this lock, so it costs nothing there. Taken as the outermost
    /// lock, exactly once per `render_packed`, before any cache Mutex, so it cannot
    /// deadlock against the inner caches.
    render_guard: std::sync::Mutex<()>,
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
        // Bilinear, clamp-to-edge sampler for the glyph SDF. Clamp-to-edge mirrors
        // the CPU `sample_sdf_bilinear` neighbor clamp (`x1 = (x0+1).min(size-1)`),
        // and linear min/mag gives the 2×2 lerp the CPU does by hand.
        let glyph_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("orber-glyph-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Some(Self {
            device,
            queue,
            adapter_name,
            pipeline_cache: std::sync::Mutex::new(HashMap::new()),
            sized_cache: std::sync::Mutex::new(HashMap::new()),
            orb_texture: std::sync::Mutex::new(None),
            glyph_sdf_cache: std::sync::Mutex::new(HashMap::new()),
            glyph_sampler,
            render_guard: std::sync::Mutex::new(()),
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
            self.pipeline_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            self.sized_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
        )
    }

    /// Current capacity (height in orbs) of the grow-only orb data-texture, or
    /// `0` if no frame has been rendered yet. Exposed for the grow-only test
    /// (`orb_texture_grows_only_on_increase`): a higher count must grow it, a lower
    /// or equal count must leave it unchanged. Mirrors the `cache_sizes` test hook.
    #[cfg(test)]
    fn orb_capacity(&self) -> u32 {
        self.orb_texture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or(0, |tex| tex.capacity)
    }

    /// Get-or-build the Circle pipeline for `shader_wgsl`, compiling the shader and
    /// pipeline only on first use. The closure runs at most once per distinct
    /// shader source for the life of the renderer.
    fn pipeline<R>(
        &self,
        shader_wgsl: &str,
        glyph: bool,
        f: impl FnOnce(&CachedPipeline) -> R,
    ) -> R {
        let mut cache = self
            .pipeline_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = cache
            .entry(shader_wgsl.to_owned())
            .or_insert_with(|| self.build_pipeline(shader_wgsl, glyph));
        f(entry)
    }

    /// Compile a pipeline for `shader_wgsl`. The Circle pipeline has binding 0 =
    /// `Params` uniform, binding 1 = orb data-texture (`Rgba32Float`, read via
    /// `textureLoad`, `filterable: false`). The Glyph pipeline (`glyph = true`)
    /// additionally has binding 2 = glyph SDF (`R8Unorm`, `filterable: true`) and
    /// binding 3 = a filtering sampler, so the shader can bilinear-sample the SDF.
    /// The orb texture stays `textureLoad`-only either way, keeping the path
    /// portable to wgpu's WebGL2 backend (#210/#212).
    fn build_pipeline(&self, shader_wgsl: &str, glyph: bool) -> CachedPipeline {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut entries = vec![uniform_entry(0), orb_texture_entry(1)];
        if glyph {
            entries.push(glyph_sdf_texture_entry(2));
            entries.push(glyph_sampler_entry(3));
        }
        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("orber-orb-bgl"),
                    entries: &entries,
                });
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("orber-orb-shader"),
                source: wgpu::ShaderSource::Wgsl(shader_wgsl.into()),
            });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("orber-orb-pl"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("orber-orb-pipeline"),
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
        let mut map = self
            .sized_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    /// return a view to bind. The texture is 4 texels wide (color / phase / misc /
    /// rot) × `orbs.len()` tall; it is reallocated only when the orb count exceeds
    /// the cached capacity, then `write_texture` fills the live rows each frame.
    ///
    /// `write_texture` has no 256-byte row-alignment requirement (that is only for
    /// buffer→texture copies), so the tight `ORB_TEX_BYTES_PER_ROW` (64) is used.
    fn upload_orb_texture(&self, orbs: &[GpuOrb]) -> wgpu::TextureView {
        let rows = orbs.len().max(1) as u32;
        let mut guard = self
            .orb_texture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let needs_realloc = match guard.as_ref() {
            Some(tex) => tex.capacity < rows,
            None => true,
        };
        if needs_realloc {
            *guard = Some(self.build_orb_texture(rows));
        }
        let tex = guard.as_ref().expect("orb texture just ensured present");

        // `write_texture` reads exactly `rows × 4 × 16` bytes from `orbs`, which is
        // `bytemuck`-castable to a flat `&[u8]` (GpuOrb is 4 × vec4<f32> = 64 bytes,
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

    /// Allocate the per-orb data-texture sized for `capacity` orbs (4 texels wide).
    /// `usage = TEXTURE_BINDING | COPY_DST` (sampled in the shader, written via
    /// `write_texture`). No `RENDER_ATTACHMENT` / `COPY_SRC` — it is input only.
    fn build_orb_texture(&self, capacity: u32) -> OrbTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-orb-tex"),
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

    /// Get-or-upload the glyph `sdf` (`size × size`, one byte per texel, 128≈edge)
    /// as an `R8Unorm` texture and return a view to bind, cached per `(ch, size)`.
    /// The cache is grow-only (mirrors the other caches): one glyph at one size
    /// keeps a single entry across a whole clip.
    ///
    /// `R8Unorm` rows are 1 byte/texel; `write_texture` is exempt from the 256-byte
    /// row-alignment requirement (that is buffer→texture only), so the tight `size`
    /// bytes-per-row is used as-is.
    fn upload_glyph_sdf(&self, ch: char, size: u32, sdf: &[u8]) -> wgpu::TextureView {
        let key = (ch as u32, size);
        let mut cache = self
            .glyph_sdf_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tex) = cache.get(&key) {
            return tex.view.clone();
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-glyph-sdf-tex"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            sdf,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        cache.insert(key, GlyphSdfTexture { view: view.clone() });
        view
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

    /// Render one **Glyph** frame at time `t` from `clusters` + `opts`, matching
    /// the CPU [`crate::glyph::render_glyph_orb`] fill (#212 Phase 1b).
    ///
    /// `opts.shape` must be [`OrbShape::Glyph`]; the glyph `ch` / `font` select the
    /// SDF. The per-orb arithmetic reuses [`pack_render_data_for_webgl`] (so
    /// positions / radii / rotation match the CPU path), saturation is re-applied
    /// per orb like the CPU oracle, the glyph SDF is uploaded as an `R8Unorm`
    /// texture, and the Glyph shader bilinear-samples it and fills with
    /// `falloff_curve(1 - signed_unit)`.
    ///
    /// # Parity scope (loose — bleed is excluded)
    ///
    /// The CPU `render_frame` applies a per-frame aquarelle **bleed pass** after
    /// the glyph fill (#195); this GPU path renders only the **pre-bleed** fill.
    /// So lit coverage / edge position / softness response / rotation match the CPU
    /// within a loose tolerance, but bleed-derived halo differences are expected and
    /// allowed (a separate slice will add the bleed pass).
    ///
    /// Returns a background-only frame (no glyph fill) when the glyph is unknown /
    /// empty in the bundled font, mirroring the CPU "draw nothing for tofu"
    /// contract.
    pub fn render_frame_glyph(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
    ) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);

        let (ch, font) = match opts.shape {
            crate::orb::OrbShape::Glyph { ch, font } => (ch, font),
            // Not a glyph shape: fall back to the Circle path so the call is total.
            _ => return self.render_frame(clusters, opts, t),
        };

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
        let n_orbs = opts
            .count
            .unwrap_or(clusters.len())
            .min(MAX_ORB_COUNT)
            .max(if clusters.is_empty() { 0 } else { 1 });

        // SDF size: the CPU picks one per orb from its breath radius; the GPU binds
        // one SDF for the frame, so size it from the *largest* orb radius (max
        // weight × the breath max factor) so most orbs sample at or above the size
        // the CPU used — bilinear sampling then matches closely.
        let max_weight = clusters
            .iter()
            .map(|c| c.weight.max(0.0))
            .fold(0.0_f32, f32::max);
        let frame_radius = base_radius_unit * max_weight.sqrt() * BREATH_RADIUS_MAX_FACTOR;

        // No glyph (radius 0 / unknown char / empty SDF) ⇒ background-only frame,
        // matching the CPU "draw nothing" contract. Build a glyph-shaped pack with
        // zero orbs so only the background paints.
        let Some((sdf, sdf_size)) =
            crate::glyph::cached_glyph_sdf_for_radius(font, ch, frame_radius)
        else {
            let pack = pack_render_data_for_webgl(
                clusters,
                opts.background,
                base_radius_unit,
                base_blur,
                direction_id,
                cycle,
                opts.seed,
                0, // no orbs → background only
                alpha_mul,
                1.0, // shape_id = Glyph
                opts.glyph_rotate,
                opts.softness.edge_softness(),
            );
            return self.render_packed(&pack, width, height, t);
        };

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
            1.0, // shape_id = Glyph
            opts.glyph_rotate,
            opts.softness.edge_softness(),
        );
        // CPU glyph path applies per-orb saturation too (`render_frame_with_params`
        // calls `adjust_saturation_pub(color_at_t, saturation)` before drawing).
        apply_saturation_to_pack(&mut pack, opts.saturation.max(0.0), n_orbs);

        // Upload (or reuse) the glyph SDF, then render with the Glyph pipeline.
        // The SDF texture is keyed per `(ch, size)` and immutable once created, so
        // (unlike the shared, overwritten orb texture) it needs no extra
        // serialization here — `render_packed_inner` takes `render_guard` for the
        // pass/upload/readback that actually shares mutable resources.
        let sdf_view = self.upload_glyph_sdf(ch, sdf_size, &sdf);

        self.render_packed_inner(
            &pack,
            width,
            height,
            t,
            Some(GlyphBindings {
                sdf_view: &sdf_view,
                size: sdf_size,
            }),
        )
    }

    /// Render one **Circle** frame from a raw `pack_render_data_for_webgl` buffer.
    ///
    /// `pack` must be the header(16) + per-orb(16 × n_orbs) layout produced by
    /// [`pack_render_data_for_webgl`]. `t` is the normalized time written into the
    /// shader's `u_t`; it is clamped to `0.0..=1.0`. This is the seam the WebGL
    /// path will also share (Phase 2). Glyph rendering uses the private
    /// `render_packed_inner` with a glyph SDF binding instead.
    pub fn render_packed(&self, pack: &[f32], width: u32, height: u32, t: f32) -> RgbaImage {
        self.render_packed_inner(pack, width, height, t, None)
    }

    /// Shared core of the Circle / Glyph paths. `glyph = Some(_)` selects the Glyph
    /// pipeline (`orb_glyph.wgsl`) and binds the SDF texture + sampler; `None` is
    /// the Circle pipeline. The orb data-texture, per-size resources, the
    /// `render_guard` serialization, and the read-back are all shared.
    fn render_packed_inner(
        &self,
        pack: &[f32],
        width: u32,
        height: u32,
        t: f32,
        glyph: Option<GlyphBindings<'_>>,
    ) -> RgbaImage {
        // Serialize the whole GPU body below (orb/params upload → pass record →
        // submit → readback). Concurrent `render_frame` on one shared renderer
        // otherwise alias the shared cached resources (grow-only orb texture,
        // per-size output texture / read-back buffer): a second thread's upload
        // could overwrite the one shared orb texture before the first thread's pass
        // samples it, rendering a frame with another thread's orb colors (#210).
        // This is the outermost lock, taken exactly once before any cache Mutex, so
        // it cannot deadlock against the inner pipeline / sized / orb caches. The
        // single-threaded CLI never contends it. It does not change the drawing
        // result — bit-exact parity is preserved.
        let _render_guard = self
            .render_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

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
            // header[11] = glyph_rotate (#136), header[12] = edge_softness (#205).
            // Both are Glyph-only; the Circle shader never reads them. `sdf_size`
            // comes from the Glyph binding (the shader uses it to match the CPU
            // bilinear convention); Circle leaves it 0.
            glyph_rotate: pack[11],
            edge_softness: pack[12],
            sdf_size: glyph.as_ref().map_or(0.0, |g| g.size as f32),
        };
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("orber-circle-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        // Per-orb words → one `GpuOrb` (4 vec4s) per orb: color+weight, phase
        // quartet, cross_axis/style/speed, and rotation (base_angle,
        // rot_speed_signed). Circle ignores the rot texel; Glyph reads it for #136.
        // The shader iterates `params.n_orbs` rows, so the row count must equal
        // `n_orbs` even if an (externally hand-built) `pack` runs short — short rows
        // stay zeroed.
        let mut orbs = vec![
            GpuOrb {
                color: [0.0; 4],
                phase: [0.0; 4],
                misc: [0.0; 4],
                rot: [0.0; 4],
            };
            n_orbs.max(1)
        ];
        for (i, slot) in orbs.iter_mut().enumerate().take(n_orbs) {
            let off = HEADER_WORDS + PER_ORB_WORDS * i;
            // Max word read below is `pack[off + 12]` (rot_speed_signed), so the
            // guard must allow `off + 12 == len - 1`, i.e. `off + 13 == len`. Using
            // `>=` here would wrongly break one orb early when an externally
            // hand-built buffer is sized to exactly `off + 13`; the correct
            // cut-off is `off + 13 > len`. (Circle only reads up to `off + 10`, but
            // requiring the full 13 words is safe: the production packer always
            // emits 16 per-orb words. The Circle parity tests feed full packs.)
            if off + 13 > pack.len() {
                break;
            }
            *slot = GpuOrb {
                color: [pack[off], pack[off + 1], pack[off + 2], pack[off + 3]],
                phase: [pack[off + 4], pack[off + 5], pack[off + 6], pack[off + 7]],
                misc: [pack[off + 8], pack[off + 9], pack[off + 10], 0.0],
                // off + 11 = base_angle, off + 12 = rot_speed_signed (#136).
                // Glyph reads these; Circle ignores the rot texel.
                rot: [pack[off + 11], pack[off + 12], 0.0, 0.0],
            };
        }

        // Upload the per-orb data into the grow-only data-texture and grab a
        // (clonable) view to bind. Done before entering the pipeline/sized
        // closures so we don't nest the orb-texture lock under them.
        let orb_view = self.upload_orb_texture(&orbs);

        // Pipeline (shader compile) cached per shader source; target / read-back
        // cached per size; orb texture grows as needed. Only the small params
        // uniform / bind group are rebuilt per frame. Glyph selects a different
        // shader + adds the SDF texture / sampler bindings (2/3).
        let (shader, is_glyph) = match &glyph {
            Some(_) => (orb_glyph_wgsl(), true),
            None => (orb_circle_wgsl(), false),
        };
        self.pipeline(shader, is_glyph, |cached| {
            self.sized_resources(width, height, |res| {
                let mut entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&orb_view),
                    },
                ];
                if let Some(g) = &glyph {
                    entries.push(wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(g.sdf_view),
                    });
                    entries.push(wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.glyph_sampler),
                    });
                }
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("orber-orb-bg"),
                    layout: &cached.bind_group_layout,
                    entries: &entries,
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

/// A fragment-visible sampled-texture entry for the glyph SDF (#212).
/// `sample_type = Float { filterable: true }` because the Glyph shader reads it
/// with a real bilinear `sampler`. `R8Unorm` is linear-filterable even on the
/// wgpu WebGL2 backend, so this stays portable (unlike the `Rgba32Float` orb
/// texture, which is `textureLoad`-only).
fn glyph_sdf_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// A fragment-visible filtering-sampler entry for the glyph SDF (#212).
fn glyph_sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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

    /// `n` clusters each with its **own** distinct color / position / weight, so a
    /// `count = n` render exercises `n` independent texture rows (not the
    /// weight-scattered expansion of the 4-color `sample_clusters` palette). Used
    /// by the distinct-rows parity test to prove every orb-texture row is loaded
    /// independently and correctly.
    fn distinct_clusters(n: usize) -> Vec<Cluster> {
        (0..n)
            .map(|i| {
                // Spread hue across all three channels and scatter the centroid on a
                // lattice so no two clusters collapse to the same color/position.
                let r = (37 + (i * 53)) % 256;
                let g = (91 + (i * 17)) % 256;
                let b = (151 + (i * 31)) % 256;
                let cx = ((i * 7) % 19) as f32 / 19.0;
                let cy = ((i * 11) % 23) as f32 / 23.0;
                let weight = 0.1 + ((i % 5) as f32) * 0.15;
                cluster([r as u8, g as u8, b as u8], cx, cy, weight)
            })
            .collect()
    }

    /// C3 (#210): parity with **65+ individually-colored** clusters. The existing
    /// `gpu_matches_cpu_high_count` only has the 4-color `sample_clusters`, so a
    /// high `count` there just re-scatters those 4 colors — it can't catch a bug
    /// where texture row `k` loads the wrong orb's color. Here each cluster is its
    /// own color, so the `clusters.len()` (= count) distinct rows must each load
    /// independently and correctly for CPU↔GPU to agree within ±2/channel.
    #[test]
    fn gpu_matches_cpu_high_count_distinct_clusters() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_matches_cpu_high_count_distinct_clusters")
        else {
            return;
        };
        for &n in &[65usize, 200] {
            let clusters = distinct_clusters(n);
            // count = None so every distinct cluster becomes its own orb row.
            let mut opts = circle_opts(50, 34, MotionDirection::LeftToRight, MotionSpeed::Mid);
            opts.count = Some(n);
            for &t in &[0.0_f32, 0.5] {
                let cpu = render_frame(&clusters, &opts, t);
                let gpu = renderer.render_frame(&clusters, &opts, t);
                let max_diff =
                    assert_within_tolerance(&cpu, &gpu, &format!("distinct n={n} t={t}"));
                eprintln!("distinct n={n} t={t}: max per-channel diff = {max_diff}");
            }
        }
    }

    /// C4 (#210): the orb data-texture grows **only on increase**. A higher count
    /// must grow `capacity`; a lower or equal count must leave it unchanged (no
    /// shrink, no reallocation when it already fits). Uses a *private* renderer so
    /// the capacity isn't perturbed by the shared parity tests, and the
    /// `orb_capacity()` test hook to observe the grow-only invariant directly.
    #[test]
    fn orb_texture_grows_only_on_increase() {
        let Some(renderer) = require_or_skip_fresh_renderer("orb_texture_grows_only_on_increase")
        else {
            return;
        };
        let clusters = sample_clusters();
        let mut opts = circle_opts(40, 28, MotionDirection::LeftToRight, MotionSpeed::Slow);

        assert_eq!(renderer.orb_capacity(), 0, "no frame yet → capacity 0");

        // First frame at count=50 allocates capacity >= 50.
        opts.count = Some(50);
        let _ = renderer.render_frame(&clusters, &opts, 0.0);
        let cap_50 = renderer.orb_capacity();
        assert!(
            cap_50 >= 50,
            "count=50 must allocate capacity >= 50, got {cap_50}"
        );

        // Increase to 200 → must grow.
        opts.count = Some(200);
        let _ = renderer.render_frame(&clusters, &opts, 0.0);
        let cap_200 = renderer.orb_capacity();
        assert!(
            cap_200 >= 200 && cap_200 > cap_50,
            "count=200 must grow capacity (was {cap_50}, now {cap_200})"
        );

        // Decrease to 100 → must NOT shrink (grow-only) and must not reallocate.
        opts.count = Some(100);
        let _ = renderer.render_frame(&clusters, &opts, 0.0);
        assert_eq!(
            renderer.orb_capacity(),
            cap_200,
            "count down to 100 must leave capacity unchanged (grow-only)"
        );

        // Re-render the same count=100 → no reallocation (capacity stable).
        let _ = renderer.render_frame(&clusters, &opts, 0.5);
        assert_eq!(
            renderer.orb_capacity(),
            cap_200,
            "same count re-render must not reallocate the orb texture"
        );
    }

    /// C5 (#210): after the orb texture grows, a later **smaller** frame must not
    /// leak the previous (larger) frame's rows. Render count=256 then count=100 on
    /// the same renderer (the texture stays at capacity 256, but only 100 rows are
    /// live); the count=100 output must equal a *fresh* renderer's single count=100
    /// render. Mirrors `cached_resources_do_not_leak_previous_frame` but for the
    /// grow-only orb texture's stale-row risk.
    #[test]
    fn high_count_cache_does_not_leak_previous_frame() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("high_count_cache_does_not_leak_previous_frame")
        else {
            return;
        };
        let clusters = distinct_clusters(256);
        let mut opts = circle_opts(48, 32, MotionDirection::LeftToRight, MotionSpeed::Mid);

        // Grow the texture to 256 rows first, then render the same renderer at 100.
        opts.count = Some(256);
        let _ = renderer.render_frame(&clusters, &opts, 0.4);
        opts.count = Some(100);
        let grown_then_100 = renderer.render_frame(&clusters, &opts, 0.4);

        // Oracle: a fresh renderer that only ever saw count=100 (texture sized 100).
        let Some(fresh) = require_or_skip_fresh_renderer(
            "high_count_cache_does_not_leak_previous_frame (oracle leg)",
        ) else {
            return;
        };
        let fresh_100 = fresh.render_frame(&clusters, &opts, 0.4);

        let max_diff = assert_within_tolerance(
            &fresh_100,
            &grown_then_100,
            "grown-to-256-then-100 vs fresh-100",
        );
        eprintln!("grow-then-shrink leak check: max per-channel diff = {max_diff}");
    }

    /// C6 (#210): `render_packed` zero-fills short rows. Hand-build a pack whose
    /// header `n_orbs` is larger than the per-orb rows actually present in the
    /// buffer; the missing rows must stay zeroed (the early `off + 11 > len` break)
    /// and the render must not panic. The orb-texture row count equals the header
    /// `n_orbs` (clamped), so a short buffer is safe — verified by rendering and
    /// comparing to a pack truncated to its real orb count (the trailing zeroed
    /// rows are alpha-0 orbs that contribute nothing, so both must match).
    #[test]
    fn render_packed_zero_fills_short_rows() {
        let Some(renderer) = require_or_skip_renderer("render_packed_zero_fills_short_rows") else {
            return;
        };
        let clusters = sample_clusters();
        let (w, h) = (40u32, 28u32);
        // A valid 8-orb pack via the production packer (header + 8 per-orb rows).
        let real_orbs = 8usize;
        let mut pack = pack_render_data_for_webgl(
            &clusters,
            [12, 18, 28, 255],
            1.0,
            0.5,
            0.0, // direction_id = LeftToRight
            MotionSpeed::Mid.cycle_count() as f32,
            12345,
            real_orbs,
            1.0,
            0.0,  // shape_id (Circle)
            true, // glyph_rotate (ignored by Circle)
            0.5,  // edge_softness (ignored by Circle)
        );
        // Lie in the header: claim more orbs than the buffer actually carries.
        // The buffer still only has `real_orbs` per-orb rows, so rows
        // [real_orbs..claimed) must zero-fill rather than read OOB or panic.
        let claimed = 40usize;
        pack[8] = claimed as f32;

        // Must not panic; the extra claimed rows are zeroed (alpha-0, no contribution).
        let short = renderer.render_packed(&pack, w, h, 0.3);

        // Oracle: an honest pack that declares exactly its real orb count.
        let honest = pack_render_data_for_webgl(
            &clusters,
            [12, 18, 28, 255],
            1.0,
            0.5,
            0.0, // direction_id = LeftToRight
            MotionSpeed::Mid.cycle_count() as f32,
            12345,
            real_orbs,
            1.0,
            0.0,
            true,
            0.5,
        );
        let honest_img = renderer.render_packed(&honest, w, h, 0.3);

        assert_eq!(
            short.dimensions(),
            (w, h),
            "short-row pack must still produce a {w}x{h} image (row count = output size, not n_orbs)"
        );
        let max_diff =
            assert_within_tolerance(&honest_img, &short, "short-row zero-fill vs honest pack");
        eprintln!("short-row zero-fill: max per-channel diff = {max_diff}");
    }

    /// C7 (#210): a width whose `width*4` is **not** 256-aligned, at the max
    /// count=1024, in a single frame. Stresses the read-back row-padding strip and
    /// the tall (1024-row) orb texture at once; CPU↔GPU must still agree.
    #[test]
    fn gpu_matches_cpu_high_count_unaligned_width_max() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_matches_cpu_high_count_unaligned_width_max")
        else {
            return;
        };
        let clusters = sample_clusters();
        // 50*4 = 200, not a multiple of 256 → read-back rows are padded.
        let mut opts = circle_opts(50, 34, MotionDirection::LeftToRight, MotionSpeed::Mid);
        opts.count = Some(1024);
        let cpu = render_frame(&clusters, &opts, 0.5);
        let gpu = renderer.render_frame(&clusters, &opts, 0.5);
        let max_diff = assert_within_tolerance(&cpu, &gpu, "unaligned width × count=1024");
        eprintln!("unaligned-width count=1024: max per-channel diff = {max_diff}");
    }

    /// C8 (#210): count=1 — the minimum (one orb row, `rows.max(1)` / `n_orbs.max(1)`
    /// lower bounds). The single-row orb texture must render and match the CPU.
    #[test]
    fn gpu_matches_cpu_count_one() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_count_one") else {
            return;
        };
        let clusters = sample_clusters();
        let mut opts = circle_opts(40, 28, MotionDirection::LeftToRight, MotionSpeed::Mid);
        opts.count = Some(1);
        for &t in &[0.0_f32, 0.5, 1.0] {
            let cpu = render_frame(&clusters, &opts, t);
            let gpu = renderer.render_frame(&clusters, &opts, t);
            let max_diff = assert_within_tolerance(&cpu, &gpu, &format!("count=1 t={t}"));
            eprintln!("count=1 t={t}: max per-channel diff = {max_diff}");
        }
    }

    /// C9 (#210): the internal `render_packed` contract clamps a header `n_orbs`
    /// above [`MAX_ORB_COUNT`] (1024) down to 1024 — no panic, no out-of-bounds
    /// texture read. This bypasses the CLI (which can't request > 1024 via
    /// `--count`), so it pins the *internal* clamp directly. Build an honest pack
    /// then overwrite the header count to 2000; the render must succeed at the
    /// frame size. (The shader reads `params.n_orbs` rows from a texture sized to
    /// the same clamped count, so an unclamped header would read past the texture.)
    #[test]
    fn gpu_clamps_count_above_max_orb_count() {
        let Some(renderer) = require_or_skip_renderer("gpu_clamps_count_above_max_orb_count")
        else {
            return;
        };
        let clusters = sample_clusters();
        let (w, h) = (40u32, 28u32);
        // An honest 1024-orb pack, then a lying header claiming 2000 orbs.
        let mut pack = pack_render_data_for_webgl(
            &clusters,
            [12, 18, 28, 255],
            1.0,
            0.5,
            0.0, // direction_id = LeftToRight
            MotionSpeed::Mid.cycle_count() as f32,
            12345,
            MAX_ORB_COUNT,
            1.0,
            0.0,
            true,
            0.5,
        );
        pack[8] = 2000.0;

        // Must clamp to 1024 internally: no panic, no OOB texture read.
        let clamped = renderer.render_packed(&pack, w, h, 0.5);
        assert_eq!(
            clamped.dimensions(),
            (w, h),
            "clamped over-max render must still produce a {w}x{h} image"
        );

        // Equivalence: header=2000 (clamped to 1024) must equal header=1024 exactly,
        // since both render the same 1024 rows.
        let mut honest = pack.clone();
        honest[8] = MAX_ORB_COUNT as f32;
        let honest_img = renderer.render_packed(&honest, w, h, 0.5);
        let max_diff = assert_within_tolerance(
            &honest_img,
            &clamped,
            "header=2000 (clamped) vs header=1024",
        );
        eprintln!("over-max clamp: max per-channel diff = {max_diff}");
    }

    /// C10 (#210): `--saturation != 1.0` parity holds at high count. The
    /// `apply_saturation_to_pack` loop must run over **all** 256 orb rows (not just
    /// the first 64), so a desaturated (0.5) and a boosted (2.0) high-count frame
    /// must each still match the CPU oracle.
    #[test]
    fn gpu_matches_cpu_high_count_with_saturation() {
        let Some(renderer) = require_or_skip_renderer("gpu_matches_cpu_high_count_with_saturation")
        else {
            return;
        };
        let clusters = distinct_clusters(256);
        for &saturation in &[0.5_f32, 2.0] {
            let mut opts = circle_opts(40, 28, MotionDirection::LeftToRight, MotionSpeed::Mid);
            opts.count = Some(256);
            opts.saturation = saturation;
            let cpu = render_frame(&clusters, &opts, 0.5);
            let gpu = renderer.render_frame(&clusters, &opts, 0.5);
            let max_diff =
                assert_within_tolerance(&cpu, &gpu, &format!("sat={saturation} count=256"));
            eprintln!("sat={saturation} count=256: max per-channel diff = {max_diff}");
        }
    }

    /// C11 (#210): after the lock fix, concurrent `render_frame` on a shared
    /// renderer must each match its solo render. Several threads render **different**
    /// high counts concurrently on the *same* renderer; each thread's output must
    /// equal that same input rendered alone (a fresh renderer). This is the only
    /// test exercising the shared `orb_texture` / sized caches under contention.
    ///
    /// `GpuRenderer::render_packed` now serializes its whole GPU body under
    /// `render_guard` (orb/params upload → pass record → submit → readback), so the
    /// shared orb texture and per-size resources can no longer be aliased mid-frame
    /// by another thread. Before that fix `upload_orb_texture` released the
    /// `orb_texture` Mutex right after the `write_texture` enqueue, so a second
    /// thread could overwrite the one shared texture before the first thread's pass
    /// sampled it, and frames rendered with another thread's orb colors (observed
    /// diffs ~120–140/channel). With the serialization in place each concurrent
    /// frame matches its solo oracle within the ±2/channel contract.
    #[test]
    fn shared_gpu_concurrent_high_count_render() {
        let Some(renderer) = require_or_skip_renderer("shared_gpu_concurrent_high_count_render")
        else {
            return;
        };
        let counts = [80usize, 150, 256, 400];
        // Per-count oracle: render each count alone on a fresh renderer first, so
        // the concurrent outputs have something independent to match against.
        let mut oracles = Vec::new();
        for &c in &counts {
            let clusters = distinct_clusters(c);
            let mut opts = circle_opts(44, 30, MotionDirection::LeftToRight, MotionSpeed::Mid);
            opts.count = Some(c);
            let Some(fresh) = require_or_skip_fresh_renderer(
                "shared_gpu_concurrent_high_count_render (oracle leg)",
            ) else {
                return;
            };
            oracles.push(fresh.render_frame(&clusters, &opts, 0.5));
        }

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (idx, &c) in counts.iter().enumerate() {
                let oracle = &oracles[idx];
                handles.push(scope.spawn(move || {
                    let clusters = distinct_clusters(c);
                    let mut opts =
                        circle_opts(44, 30, MotionDirection::LeftToRight, MotionSpeed::Mid);
                    opts.count = Some(c);
                    // Each thread renders a few times to maximize grow/clone overlap.
                    for _ in 0..3 {
                        let img = renderer.render_frame(&clusters, &opts, 0.5);
                        assert_within_tolerance(
                            oracle,
                            &img,
                            &format!("concurrent count={c} vs solo render"),
                        );
                    }
                }));
            }
            for h in handles {
                h.join().expect("concurrent render thread panicked");
            }
        });
        eprintln!("concurrent high-count render: all threads matched their solo oracle");
    }

    // ---- Glyph (#212 Phase 1b) ----------------------------------------------

    /// The `GLYPH_SDF_CONTENT_SPAN` constant hardcoded in `orb_glyph.wgsl` must
    /// match the Rust `crate::glyph::GLYPH_SDF_CONTENT_SPAN_PUB` (= 1/√2). If the
    /// CPU constant ever changes, this guards against the WGSL drifting out of sync
    /// (which would shift the glyph UV mapping and break parity).
    #[test]
    fn glyph_wgsl_content_span_matches_rust() {
        let wgsl = orb_glyph_wgsl();
        // Find the literal after `GLYPH_SDF_CONTENT_SPAN: f32 = ` in the shader.
        let needle = "GLYPH_SDF_CONTENT_SPAN: f32 = ";
        let start = wgsl
            .find(needle)
            .expect("shader must declare the span const")
            + needle.len();
        let rest = &wgsl[start..];
        let end = rest.find(';').expect("const decl must end with ;");
        let lit: f32 = rest[..end].trim().parse().expect("span literal must parse");
        assert!(
            (lit - crate::glyph::GLYPH_SDF_CONTENT_SPAN_PUB).abs() < 1e-6,
            "orb_glyph.wgsl GLYPH_SDF_CONTENT_SPAN ({lit}) must match Rust ({})",
            crate::glyph::GLYPH_SDF_CONTENT_SPAN_PUB
        );
    }

    /// A Glyph `AnimateOptions` for ☆ (U+2606), large orbs on a dark opaque bg so
    /// the glyph fill is well-separated from the background.
    fn glyph_opts(
        w: u32,
        h: u32,
        direction: MotionDirection,
        speed: MotionSpeed,
        glyph_rotate: bool,
    ) -> AnimateOptions {
        AnimateOptions {
            width: w,
            height: h,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction,
            speed,
            seed: 7,
            count: Some(6),
            background: [10, 12, 20, 255],
            shape: OrbShape::Glyph {
                ch: '☆',
                font: crate::glyph::GlyphFontId::NotoSymbols2,
            },
            softness: SoftnessPreset::Mid,
            glyph_rotate,
            color_tracks: None,
            keyframe_tracks: None,
        }
    }

    /// Count pixels that differ from the (opaque) background color by more than
    /// `thresh` on any channel — a proxy for "glyph fill is present here".
    fn lit_vs_bg(img: &RgbaImage, bg: [u8; 4], thresh: u8) -> usize {
        img.pixels()
            .filter(|p| {
                (0..3).any(|c| p.0[c].abs_diff(bg[c]) > thresh) || p.0[3].abs_diff(bg[3]) > thresh
            })
            .count()
    }

    /// The Glyph WGSL must compile and the fill must produce lit pixels: a known
    /// glyph (☆) on an opaque dark background paints a non-trivial number of
    /// foreground pixels that differ from the background.
    #[test]
    fn gpu_glyph_renders_lit_pixels() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_renders_lit_pixels") else {
            return;
        };
        eprintln!(
            "GPU Glyph render test running on adapter: {}",
            renderer.adapter_name()
        );
        let clusters = sample_clusters();
        let opts = glyph_opts(
            120,
            90,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        let img = renderer.render_frame_glyph(&clusters, &opts, 0.0);
        assert_eq!(img.dimensions(), (120, 90));
        let lit = lit_vs_bg(&img, opts.background, 8);
        assert!(
            lit > 200,
            "glyph fill must paint a non-trivial number of lit pixels, got {lit}"
        );
        eprintln!("glyph lit pixels = {lit}");
    }

    /// Lit-pixel bounding box of `img` against the opaque background, with a
    /// per-channel `thresh`. Returns `None` when nothing is lit.
    fn lit_bbox(img: &RgbaImage, bg: [u8; 4], thresh: u8) -> Option<(u32, u32, u32, u32)> {
        let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut any = false;
        for (x, y, p) in img.enumerate_pixels() {
            if (0..3).any(|c| p.0[c].abs_diff(bg[c]) > thresh) {
                any = true;
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
        }
        any.then_some((minx, miny, maxx, maxy))
    }

    /// Structural parity with the CPU oracle (loose; bleed excluded). The CPU
    /// `render_frame` adds an aquarelle bleed pass *after* the glyph fill, which
    /// blurs/spreads the fill and shifts many pixels above/below the lit threshold,
    /// so exact coverage is *not* expected to match. What must match is the
    /// **structure** of the fill (same glyph, same orb positions / scale):
    ///   - the lit-pixel **bounding boxes** align within a few pixels (position +
    ///     extent of the glyph fill agree, proving UV mapping / rotation / scale);
    ///   - both paths light a non-trivial, comparable number of pixels (within a
    ///     2× band either way — bleed can raise or lower the count vs the sharp
    ///     pre-bleed fill);
    ///   - the two lit sets overlap substantially (the fill is in the same place).
    ///
    /// Bleed-derived per-pixel differences are expected and allowed.
    #[test]
    fn gpu_glyph_structural_parity_with_cpu() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_structural_parity_with_cpu")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts = glyph_opts(
            120,
            90,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        for &t in &[0.0_f32, 0.5] {
            let cpu = render_frame(&clusters, &opts, t);
            let gpu = renderer.render_frame_glyph(&clusters, &opts, t);
            assert_eq!(cpu.dimensions(), gpu.dimensions());

            let bg = opts.background;
            let cpu_lit = lit_vs_bg(&cpu, bg, 8);
            let gpu_lit = lit_vs_bg(&gpu, bg, 8);
            assert!(cpu_lit > 0 && gpu_lit > 0, "both paths must light pixels");

            // Comparable coverage (loose 2× band either direction).
            let ratio = gpu_lit as f32 / cpu_lit as f32;
            assert!(
                (0.5..=2.0).contains(&ratio),
                "t={t}: gpu_lit={gpu_lit} / cpu_lit={cpu_lit} = {ratio:.2}, expected within [0.5, 2.0]"
            );

            // Lit bounding boxes must align within a small tolerance (the fill is
            // in the same screen region at the same scale). Bleed grows the CPU
            // bbox slightly; allow 6 px slack on each edge.
            let cb = lit_bbox(&cpu, bg, 8).expect("cpu has lit pixels");
            let gb = lit_bbox(&gpu, bg, 8).expect("gpu has lit pixels");
            let tol = 6i64;
            for (label, a, b) in [
                ("minx", cb.0 as i64, gb.0 as i64),
                ("miny", cb.1 as i64, gb.1 as i64),
                ("maxx", cb.2 as i64, gb.2 as i64),
                ("maxy", cb.3 as i64, gb.3 as i64),
            ] {
                assert!(
                    (a - b).abs() <= tol,
                    "t={t}: lit bbox {label} differs by {} (cpu={a} gpu={b}), tol={tol}",
                    (a - b).abs()
                );
            }

            // Overlap: a good fraction of the smaller lit set coincides with the
            // larger one (the fills occupy the same pixels, not merely the same box).
            let mut overlap = 0usize;
            for (cp, gp) in cpu.pixels().zip(gpu.pixels()) {
                let c_lit = (0..3).any(|c| cp.0[c].abs_diff(bg[c]) > 8);
                let g_lit = (0..3).any(|c| gp.0[c].abs_diff(bg[c]) > 8);
                if c_lit && g_lit {
                    overlap += 1;
                }
            }
            let overlap_frac = overlap as f32 / cpu_lit.min(gpu_lit) as f32;
            assert!(
                overlap_frac > 0.6,
                "t={t}: lit overlap {:.1}% of the smaller set; expected >60%",
                overlap_frac * 100.0
            );
            eprintln!(
                "glyph parity t={t}: cpu_lit={cpu_lit} gpu_lit={gpu_lit} ratio={ratio:.2} overlap={:.1}%",
                overlap_frac * 100.0
            );
        }
    }

    /// Rotation (#136): ON animates the glyph (frames at different t differ), OFF
    /// holds the orientation (the per-orb `base_angle` is fixed, so the frame is
    /// identical for every t — only rotation depends on t in a single-glyph,
    /// stationary-flow check). Loop closure: ON frame at t=0 equals t=1.
    #[test]
    fn gpu_glyph_rotation_on_off_and_loop_closure() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_rotation_on_off_and_loop_closure")
        else {
            return;
        };
        let clusters = sample_clusters();

        // OFF: base_angle is fixed for all t. Conveyor advance also depends on t,
        // so to isolate rotation use VerySlow with a single, centered orb is hard;
        // instead assert the *rotation-only* invariant by comparing OFF frames at
        // two times against each other is NOT valid (flow still moves). So we test
        // OFF differently: a glyph rendered OFF must equal the same glyph rendered
        // OFF again (determinism) and the ON/OFF frames must differ at a non-zero t
        // (rotation visibly changes the picture).
        let off = glyph_opts(
            100,
            100,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            false,
        );
        let on = AnimateOptions {
            glyph_rotate: true,
            ..off.clone()
        };

        // Determinism for OFF.
        let off_a = renderer.render_frame_glyph(&clusters, &off, 0.3);
        let off_b = renderer.render_frame_glyph(&clusters, &off, 0.3);
        assert_eq!(off_a, off_b, "OFF glyph render must be deterministic");

        // ON vs OFF differ at t=0.3 (rotation changes the glyph orientation).
        let on_t = renderer.render_frame_glyph(&clusters, &on, 0.3);
        let diff_on_off = off_a
            .pixels()
            .zip(on_t.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            diff_on_off > 100,
            "rotation ON must differ from OFF at t=0.3, differing pixels={diff_on_off}"
        );

        // Loop closure for ON: t=0 and t=1 render identical frames.
        let on_t0 = renderer.render_frame_glyph(&clusters, &on, 0.0);
        let on_t1 = renderer.render_frame_glyph(&clusters, &on, 1.0);
        let max_diff = assert_within_tolerance(&on_t0, &on_t1, "glyph ON loop closure t=0 vs t=1");
        eprintln!(
            "glyph rotation: on/off differ by {diff_on_off} px, loop closure max diff = {max_diff}"
        );
    }

    /// softness preset must change the Glyph fill (alpha_mul / blur feed the same
    /// `falloff_curve` the CPU uses): Low / Mid / High produce visibly different
    /// frames on the GPU glyph path.
    #[test]
    fn gpu_glyph_softness_changes_output() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_softness_changes_output") else {
            return;
        };
        let clusters = sample_clusters();
        let base = glyph_opts(
            100,
            80,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        let low = renderer.render_frame_glyph(
            &clusters,
            &AnimateOptions {
                softness: SoftnessPreset::Low,
                ..base.clone()
            },
            0.0,
        );
        let high = renderer.render_frame_glyph(
            &clusters,
            &AnimateOptions {
                softness: SoftnessPreset::High,
                ..base.clone()
            },
            0.0,
        );
        let diff = low
            .pixels()
            .zip(high.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            diff > 100,
            "softness Low vs High must change the glyph fill, differing pixels={diff}"
        );
        eprintln!("glyph softness Low vs High differing pixels = {diff}");
    }

    /// An unknown / unrenderable glyph (pizza emoji, absent from the bundled
    /// Symbols 2 subset) must yield a background-only frame — no orb fill, matching
    /// the CPU "draw nothing for tofu" contract.
    #[test]
    fn gpu_glyph_unknown_char_background_only() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_unknown_char_background_only")
        else {
            return;
        };
        let clusters = sample_clusters();
        let mut opts = glyph_opts(
            48,
            40,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        opts.shape = OrbShape::Glyph {
            ch: '\u{1F355}', // pizza — not in Noto Sans Symbols 2
            font: crate::glyph::GlyphFontId::NotoSymbols2,
        };
        let img = renderer.render_frame_glyph(&clusters, &opts, 0.3);
        let bg = opts.background;
        let lit = lit_vs_bg(&img, bg, 1);
        assert_eq!(
            lit, 0,
            "unknown glyph must paint background only (no fill), got {lit} non-bg pixels"
        );
    }

    /// `render_frame_glyph` on a non-Glyph shape falls back to the Circle path
    /// (the call is total). A Circle-shaped opts through the glyph entry must match
    /// the dedicated Circle `render_frame` within the ±2/channel contract.
    #[test]
    fn gpu_glyph_entry_circle_shape_falls_back() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_entry_circle_shape_falls_back")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts = circle_opts(40, 28, MotionDirection::LeftToRight, MotionSpeed::Mid);
        let via_circle = renderer.render_frame(&clusters, &opts, 0.5);
        let via_glyph_entry = renderer.render_frame_glyph(&clusters, &opts, 0.5);
        let max_diff = assert_within_tolerance(
            &via_circle,
            &via_glyph_entry,
            "circle-shape through glyph entry vs render_frame",
        );
        eprintln!("glyph-entry circle fallback: max per-channel diff = {max_diff}");
    }
}
