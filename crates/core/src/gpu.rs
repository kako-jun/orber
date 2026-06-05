//! wgpu (Rust + WGSL) production render path — orber #207 Phase 0–1c, #225.
//!
//! [`GpuRenderer`] is the headless, native side of the renderer and — since #225 —
//! the **only** renderer (the CPU / tiny-skia path and the CPU↔GPU parity oracle
//! were purged). It runs the Circle orb WGSL
//! ([`orb_circle.wgsl`](../src/orb_circle.wgsl)), the glyph / image SDF WGSL
//! ([`orb_glyph.wgsl`](../src/orb_glyph.wgsl)) and the aquarelle WGSL
//! ([`orb_aquarelle.wgsl`](../src/orb_aquarelle.wgsl)). All four shapes (Circle,
//! Glyph, Image, Aquarelle) render on the GPU; the CLI renders every PNG / video /
//! variation frame through it.
//!
//! ## Per-orb data path
//!
//! - **saturation reflected**: [`GpuRenderer::render_frame`] re-applies
//!   [`adjust_saturation_pub`](crate::orb::adjust_saturation_pub) with
//!   `opts.saturation` to each packed orb color after
//!   [`pack_render_data_for_webgl`] (which itself never applies saturation,
//!   because it is shared with the WebGL path);
//! - **count up to [`MAX_ORB_COUNT`] (1024)**: per-orb data is uploaded as a
//!   **data-texture** (`Rgba32Float`, read with `textureLoad`) — not a fixed-size
//!   uniform array — so the WGSL has **no 64-orb cap** (#210 Phase 1a). (The 64
//!   limit only ever applied to the WebGL GLSL path —
//!   `web/src/lib/orberGl.ts::MAX_ORBS` / `crates/wasm/src/lib.rs::GL_RENDERER_MAX_ORBS`
//!   — which is untouched until Phase 3; do not re-sync a 64 cap onto this path.)
//!
//! The per-orb `color_tracks` / `keyframe_tracks` (#7 / #33) are not yet folded into
//! the GPU pack; animated color/position tracks come in via the cluster列 instead.
//!
//! ## Compositing contract
//!
//! Orbs are composited in **straight sRGB byte space**. The GPU path:
//!
//! - renders into an [`wgpu::TextureFormat::Rgba8Unorm`] target (NOT `*Srgb`), so
//!   no sRGB↔linear conversion happens — the shader's float blend maps to bytes
//!   by `round(value * 255)`;
//! - feeds the shader the per-orb data the WebGL path also uses
//!   ([`crate::animate::pack_render_data_for_webgl`]): the parameter arithmetic is
//!   reused, never reimplemented, so the orb positions / radii / alphas match the
//!   web result. The per-orb data goes up as a `Rgba32Float` data-texture
//!   (4 texels wide × N orbs tall) so float precision is preserved exactly;
//! - reads the result back accounting for wgpu's 256-byte row-alignment
//!   requirement on `copy_texture_to_buffer` (that alignment applies only to the
//!   texture→buffer read-back; the orb data upload via `write_texture` is exempt).
//!
//! ## Validation (post-#225)
//!
//! With the CPU oracle gone, the tests in this module are **GPU-only structural
//! checks** (lit-pixel presence, determinism for the same seed/t, cache reuse vs
//! growth, rotation loop closure, alpha/empty-cluster background-only). They no
//! longer compare against a CPU reference; correctness on new GPUs is confirmed by
//! these structural invariants plus real-machine visual inspection.

use std::collections::HashMap;

use image::RgbaImage;
use wgpu::util::DeviceExt;

use crate::animate::{pack_render_data_for_webgl, AnimateOptions, MotionDirection, MAX_ORB_COUNT};
use crate::cluster::Cluster;
use crate::orb::adjust_saturation_pub;

use palette::{FromColor, Hsl, IntoColor, Srgb};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

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

/// Width, in texels, of the per-orb **aquarelle** data-texture (#216). Nine texels
/// per orb hold the four `render_aquarelle_orb` layers' geometry + u8 colors; see
/// `orb_aquarelle.wgsl`'s header for the slot map. Independent of the Circle/Glyph
/// `ORB_TEX_WIDTH` (4) — aquarelle binds its own texture so the two never alias.
const AQUARELLE_TEX_WIDTH: u32 = 9;
/// Bytes per row of the aquarelle data-texture (`9 × 16 = 144`). `write_texture` is
/// exempt from row-alignment, so this tight value is used directly.
const AQUARELLE_TEX_BYTES_PER_ROW: u32 = AQUARELLE_TEX_WIDTH * ORB_TEX_BYTES_PER_TEXEL;

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

/// The Glyph **bleed/halo** WGSL (#214 Phase 1b.5). The 2nd-pass group that writes
/// the aquarelle paper-bleed over the glyph fill: premultiply + separable box-blur
/// (H/V), halo saturation boost, and the intensity compose + finalize. Translated
/// from `aquarelle::render_aquarelle_bleed_pass` (default params, seed=0); see the
/// shader header and [`BLEED_BOX_RADIUS`] / [`BLEED_HALO_FACTOR`] /
/// [`BLEED_INTENSITY`] for the constant provenance. The paper-grain noise (step 4)
/// is **omitted** on the GPU (loose-parity decision documented there).
fn orb_glyph_bleed_wgsl() -> &'static str {
    include_str!("orb_glyph_bleed.wgsl")
}

/// The Aquarelle orb WGSL (#216 Phase 1c). A dedicated pipeline + data-texture
/// (separate from the Circle/Glyph orb texture) that evaluates the four
/// `aquarelle::render_aquarelle_orb` layers analytically: offset main 3-stop
/// radial, 0..3 bleed satellites, and the bloom core, composited SourceOver in
/// the same u8-quantize → premultiply → source_over lowp流儀 as `orb_circle.wgsl`.
/// The ChaCha8 RNG / HSL color math is **not** ported to WGSL; `pack_aquarelle_orbs`
/// runs it on the CPU (bit-identical to the crate) and uploads the resulting
/// centers / radii / u8 colors.
fn orb_aquarelle_wgsl() -> &'static str {
    include_str!("orb_aquarelle.wgsl")
}

/// Aquarelle bleed constants, fixed to `AquarelleBleedParams::default()` (the
/// values the CPU `render_frame` passes: `radius = 3.0, intensity = 0.5,
/// halo = 0.3`) so the GPU 2nd pass matches the CPU paper-bleed.
///
/// `BLEED_BOX_RADIUS` is `round(radius * 1.15).max(1)` = `round(3.45)` = `3`, the
/// box-blur half-window the crate derives from `radius` (see
/// `aquarelle::render_aquarelle_bleed_pass`). `BLEED_HALO_FACTOR` is
/// `1.0 + 0.6 * halo` = `1.18` (the saturation multiplier
/// `boost_saturation_buffer` applies). `BLEED_INTENSITY` is the compose mix
/// (`dst = original * (1 - t) + blurred * t`). `BLEED_BLUR_ITERATIONS` is the
/// crate's 3-pass (H→V) box-blur loop.
const BLEED_BOX_RADIUS: f32 = 3.0;
const BLEED_HALO_FACTOR: f32 = 1.18;
const BLEED_INTENSITY: f32 = 0.5;
const BLEED_BLUR_ITERATIONS: usize = 3;

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

/// Header uniform block for the Aquarelle shader. Mirrors `struct Params` in
/// `orb_aquarelle.wgsl` (resolution, orb count, background). `#[repr(C)]` + padding
/// to 16-byte rows. Separate from the Circle [`Params`] because the aquarelle pack
/// needs no motion scalars (positions are baked per orb on the CPU).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AquarelleHeader {
    // row 0: resolution.xy, n_orbs, pad
    resolution: [f32; 2],
    n_orbs: f32,
    _pad0: f32,
    // row 1: bg.rgba (straight)
    bg: [f32; 4],
}

/// One aquarelle orb as the shader sees it: nine `vec4`s mirroring `struct AquaOrb`
/// in `orb_aquarelle.wgsl`. Filled by [`GpuRenderer::pack_aquarelle_orbs`] from the
/// per-orb ChaCha8 + HSL math (run on the CPU, bit-identical to the crate). One
/// `GpuAquaOrb` packs to one row of the `Rgba32Float` aquarelle data-texture
/// (9 texels = 144 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuAquaOrb {
    main: [f32; 4],       // main_cx, main_cy, main_radius, sat_count
    inner: [f32; 4],      // inner_rgb (color@255), bloom_flag
    halo: [f32; 4],       // halo_rgb (mid@128 / edge@0), _
    bloom_geom: [f32; 4], // bloom_cx, bloom_cy, bloom_core_radius, _
    bloom_col: [f32; 4],  // bloom_rgb (mix_with_white), _
    bleed_col: [f32; 4],  // bleed_rgb (satellite color), _
    sat0: [f32; 4],       // sat0_cx, sat0_cy, sat0_radius, _
    sat1: [f32; 4],       // sat1_cx, sat1_cy, sat1_radius, _
    sat2: [f32; 4],       // sat2_cx, sat2_cy, sat2_radius, _
}

/// Uniform block for the Glyph bleed pass shader (`orb_glyph_bleed.wgsl`).
/// Mirrors `struct BleedParams` there. `#[repr(C)]` + padding to a 32-byte (two
/// 16-byte rows) uniform layout. One of these is built per bleed sub-pass with the
/// per-pass `radius` / `premultiply` set; `halo_factor` / `intensity` are constant
/// across passes but live here so every pass shares one uniform/bind-group layout.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BleedParams {
    // row 0: resolution.xy, radius, premultiply
    resolution: [f32; 2],
    radius: f32,
    premultiply: f32,
    // row 1: halo_factor, intensity, pad, pad
    halo_factor: f32,
    intensity: f32,
    _pad0: f32,
    _pad1: f32,
}

/// A render pipeline plus its bind-group layout, compiled once per distinct
/// shader source. Caching keeps shader compilation / pipeline creation off the
/// per-frame path: a long video renders the same shader for every frame.
struct CachedPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

/// The four bleed-pass pipelines (one per `orb_glyph_bleed.wgsl` fragment entry
/// point), plus the shared bind-group layout. Built once per renderer; the
/// vertex-only-varying pipelines differ solely in their fragment entry point, so
/// they all reuse the same layout (uniform 0 + `src` 1 + `blurred` 2). Cached so a
/// long glyph clip compiles the bleed shader and its pipelines only once.
struct BleedPipelines {
    bind_group_layout: wgpu::BindGroupLayout,
    /// Horizontal box-blur (`fs_blur_h`); the `premultiply` uniform flag turns the
    /// straight-RGBA glyph fill into premultiplied on the first iteration only.
    blur_h: wgpu::RenderPipeline,
    /// Vertical box-blur (`fs_blur_v`).
    blur_v: wgpu::RenderPipeline,
    /// Halo saturation boost (`fs_halo`).
    halo: wgpu::RenderPipeline,
    /// Intensity compose + finalize to straight RGBA (`fs_compose`).
    compose: wgpu::RenderPipeline,
}

/// Per-size intermediate textures for the Glyph bleed pass, reused across
/// same-sized frames (mirrors [`SizedResources`]). The glyph fill renders into
/// `fill` (straight RGBA, as the glyph shader emits); the box-blur ping-pongs
/// between `ping` / `pong` (premultiplied); the compose pass reads `fill` +
/// the final blurred texture and writes straight RGBA to the `SizedResources`
/// `target`. All three are `Rgba8Unorm` so each box-blur pass quantizes to u8 the
/// way the CPU crate does between its H/V passes.
struct BleedTextures {
    fill_view: wgpu::TextureView,
    ping_view: wgpu::TextureView,
    pong_view: wgpu::TextureView,
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

/// The per-orb data-texture (`Rgba32Float`, 4 texels wide × `capacity` orbs
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
    /// The four Glyph bleed-pass pipelines (#214), compiled lazily once. `None`
    /// until the first glyph frame (Circle never touches the bleed path, so a
    /// Circle-only run never compiles the bleed shader).
    bleed_pipelines: std::sync::Mutex<Option<BleedPipelines>>,
    /// Per-size bleed intermediate textures (`fill` + blur ping-pong), keyed by
    /// `(width, height)`. Grow-only / never-evicts like `sized_cache`: a glyph clip
    /// at one size keeps a single entry.
    bleed_textures: std::sync::Mutex<HashMap<(u32, u32), BleedTextures>>,
    /// The grow-only per-orb **aquarelle** data-texture (#216), reallocated only
    /// when a frame needs more rows than the cached capacity. `None` until the first
    /// aquarelle frame (Circle/Glyph-only runs never allocate it). Separate from
    /// `orb_texture` so the 9-texel aquarelle layout never aliases the 4-texel
    /// Circle/Glyph one, keeping Circle bit-exact.
    aquarelle_texture: std::sync::Mutex<Option<OrbTexture>>,
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
            bleed_pipelines: std::sync::Mutex::new(None),
            bleed_textures: std::sync::Mutex::new(HashMap::new()),
            aquarelle_texture: std::sync::Mutex::new(None),
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

    /// Number of live entries in the grow-only glyph SDF cache (one per distinct
    /// `(char, size)` uploaded). Exposed for the cache-reuse tests
    /// (`gpu_glyph_sdf_cache_reuse_same_char_size` /
    /// `gpu_glyph_sdf_cache_grows_on_new_char_or_size`): re-rendering the same
    /// `(char, size)` must keep this at 1, while a new char / size must add an
    /// entry. Mirrors the `cache_sizes` / `orb_capacity` test hooks (poison
    /// recovery via `into_inner`, `#[cfg(test)]` so production API stays clean).
    #[cfg(test)]
    fn glyph_sdf_cache_len(&self) -> usize {
        self.glyph_sdf_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Number of live entries in the grow-only per-size bleed-texture cache (one
    /// `BleedTextures` per distinct `(width, height)`). Exposed for the #214
    /// bleed-cache tests (`gpu_glyph_bleed_textures_reuse_same_size` /
    /// `gpu_glyph_bleed_textures_grow_on_new_size`): re-rendering a glyph at the
    /// same size must keep this at 1, while a new size must add an entry. Mirrors
    /// the `cache_sizes` / `glyph_sdf_cache_len` hooks (poison recovery via
    /// `into_inner`, `#[cfg(test)]` so the production API stays clean).
    #[cfg(test)]
    fn bleed_textures_len(&self) -> usize {
        self.bleed_textures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether the lazy bleed pipelines have been compiled yet (`Some`). Exposed
    /// for `gpu_bleed_pipelines_lazy_not_built_for_circle_only`: a Circle-only run
    /// must leave this `false` (the bleed shader never compiles), and the first
    /// glyph frame must flip it to `true`. Mirrors the `cache_sizes` /
    /// `glyph_sdf_cache_len` hooks (poison recovery via `into_inner`).
    #[cfg(test)]
    fn bleed_pipelines_built(&self) -> bool {
        self.bleed_pipelines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
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
    fn upload_glyph_sdf(&self, ch: char, size: u32, sdf: &[u8]) -> wgpu::TextureView {
        // `ch as u32` is a Unicode scalar (<= 0x10FFFF). `upload_image_sdf` derives
        // keys with the high bit set (> 0x10FFFF) so glyph / image never collide in
        // the shared `glyph_sdf_cache`.
        self.upload_sdf_texture(ch as u32, size, sdf)
    }

    /// Get-or-upload an **image silhouette** SDF (#217) as an `R8Unorm` texture and
    /// return a view to bind, reusing the same `glyph_sdf_cache`. The cache key is
    /// **content-derived** (FNV-1a hash of the SDF bytes, folded to 31 bits) with bit
    /// 31 forced on so it lands at `> 0x10FFFF` and can never collide with a glyph's
    /// `(ch as u32, size)` key. A single image is one SDF, so this keeps exactly one
    /// entry for the whole clip; identical re-uploads (same content) reuse it.
    fn upload_image_sdf(&self, size: u32, sdf: &[u8]) -> wgpu::TextureView {
        // FNV-1a over the SDF bytes → stable per-content id, disjoint from any char.
        let mut hash: u32 = 0x811c_9dc5;
        for &b in sdf {
            hash ^= b as u32;
            hash = hash.wrapping_mul(0x0100_0193);
        }
        let key_id = (hash & 0x7fff_ffff) | 0x8000_0000; // bit31 set ⇒ > 0x10FFFF
        self.upload_sdf_texture(key_id, size, sdf)
    }

    /// Shared `R8Unorm` SDF upload (glyph + image), cached per `(key_id, size)` in
    /// `glyph_sdf_cache`. `R8Unorm` rows are 1 byte/texel; `write_texture` is exempt
    /// from the 256-byte row-alignment requirement (that is buffer→texture only), so
    /// the tight `size` bytes-per-row is used as-is.
    fn upload_sdf_texture(&self, key_id: u32, size: u32, sdf: &[u8]) -> wgpu::TextureView {
        let key = (key_id, size);
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
    /// here; the caller routes `Glyph` to `render_frame_glyph` and `Aquarelle`
    /// to `render_frame_aquarelle` (both GPU), and only falls back to the CPU
    /// renderer when no GPU adapter is available. See the module docs.
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
    /// # Parity scope (loose — structural + tolerant)
    ///
    /// The CPU `render_frame` applies a per-frame aquarelle **bleed pass** after
    /// the glyph fill (#195). This GPU path now reproduces it as a 2nd pass group
    /// (#214): the glyph fill renders into an intermediate texture, then a WGSL
    /// premultiply → separable box-blur ×3 → halo saturation → intensity compose +
    /// finalize writes the backbuffer (see [`Self::run_glyph_fill_bleed_readback`]
    /// / `orb_glyph_bleed.wgsl`). Parity is **loose, not bit-exact**: the box-blur
    /// structure / halo / intensity match the crate, but the aquarelle paper-grain
    /// noise (a faint ±0.05 seed-derived jitter) is **omitted** on the GPU because
    /// its ChaCha8 per-pixel-order consumption cannot be bit-reproduced in parallel,
    /// and the GPU's HSL path differs from `palette`'s by sub-ULP rounding. Those
    /// small per-pixel differences vs. the CPU are **expected and allowed** (a
    /// future WGSL hash could approximate the noise).
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
        // zero orbs so only the background paints. This routes through `render_packed`
        // (the Circle path), so the bleed 2nd pass is skipped — intentional: the CPU
        // runs `render_aquarelle_bleed_pass` unconditionally for Glyph, but blurring
        // orber's uniform opaque background is a no-op *up to the omitted paper-grain
        // noise* (the CPU would jitter that flat background by ±0.05; the GPU leaves it
        // flat). Both yield a background-only frame within the same noise-omitted
        // loose-parity contract this pass already accepts.
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

    /// Render one **Image** frame at time `t` from `clusters` + `opts` (#217),
    /// matching the CPU [`crate::glyph::render_sdf_orb`] fill.
    ///
    /// `opts.shape` must be [`OrbShape::Image`]; its `sdf` / `size` are uploaded as
    /// an `R8Unorm` texture and bound to the **same** Glyph pipeline + bleed 2nd pass
    /// (`orb_glyph.wgsl` / `orb_glyph_bleed.wgsl`). The only difference from
    /// [`Self::render_frame_glyph`] is the SDF source: an image silhouette (supplied
    /// from outside, one fixed texture for the whole frame) instead of a per-radius
    /// cached font glyph. Per-orb positions / radii / rotation reuse
    /// [`pack_render_data_for_webgl`] and saturation is re-applied per orb, exactly
    /// like the glyph path, so the same loose-parity contract (structural + tolerant,
    /// paper-grain noise omitted on the GPU) applies. Non-Image shapes fall back to
    /// the Circle path so the call is total.
    pub fn render_frame_image(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
    ) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);

        let (sdf, sdf_size) = match &opts.shape {
            crate::orb::OrbShape::Image { sdf, size } => (sdf.clone(), *size),
            // Not an image shape: fall back to the Circle path so the call is total.
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

        // Empty SDF (all-zero / no contrast slipped through) or wrong length ⇒
        // background-only frame, matching the CPU "draw nothing" contract.
        if sdf_size == 0
            || sdf.len() < (sdf_size as usize) * (sdf_size as usize)
            || sdf.iter().all(|&b| b == 0)
        {
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
                1.0, // shape_id = SDF (glyph/image share id 1)
                opts.glyph_rotate,
                opts.softness.edge_softness(),
            );
            return self.render_packed(&pack, width, height, t);
        }

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
            1.0, // shape_id = SDF (image uses the same glyph shader path as Web shape_id==1)
            opts.glyph_rotate,
            opts.softness.edge_softness(),
        );
        // CPU image path applies per-orb saturation too (render_sdf_orb receives the
        // saturation-adjusted rgb in `render_frame_with_params`).
        apply_saturation_to_pack(&mut pack, opts.saturation.max(0.0), n_orbs);

        // Upload (or reuse) the image SDF with a content-derived key disjoint from
        // glyph keys, then render with the Glyph pipeline + bleed pass.
        let sdf_view = self.upload_image_sdf(sdf_size, &sdf);

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

    /// Render one **Aquarelle** frame at time `t` from `clusters` + `opts`,
    /// matching the CPU [`crate::animate::render_frame`] → `render_frame_aquarelle`
    /// → `render_static` → `aquarelle::render_aquarelle_orb` path (#216 Phase 1c).
    ///
    /// `opts.shape` must be [`OrbShape::Aquarelle`]; its [`AquarelleParams`] drive
    /// the four layers. Per-orb positions / radii / colors come from
    /// [`crate::animate::aquarelle_modulated_clusters`] (the same modulation the CPU
    /// path uses), then [`Self::pack_aquarelle_orbs`] runs the crate's ChaCha8 RNG
    /// (`seed = orb index`) + `palette` HSL color math on the CPU to produce the
    /// offset center, satellite placements, and boosted/mixed u8 colors. The
    /// `orb_aquarelle.wgsl` shader evaluates the radials and composites SourceOver.
    ///
    /// # Parity scope (loose — structural + tolerant)
    ///
    /// Bit-exact in the RNG / color math (it reuses the crate's exact arithmetic),
    /// but the radial fill is analytic where tiny-skia anti-aliases `fill_path`, so
    /// the residual is the same AA-only difference Circle accepts (±2/channel on
    /// most hardware, 0 on the real GPU for interior pixels). Non-Aquarelle shapes
    /// fall back to the Circle path so the call is total.
    pub fn render_frame_aquarelle(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
    ) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);

        let params = match opts.shape {
            crate::orb::OrbShape::Aquarelle(p) => p,
            // Not an aquarelle shape: fall back to the Circle path so the call is total.
            _ => return self.render_frame(clusters, opts, t),
        };

        // Same per-orb modulation the CPU aquarelle path uses (position wrap, radius
        // breath, #33/#7 color interpolation). Index order == `render_static` draw
        // order == `render_aquarelle_orb` seed `i`.
        let modulated = crate::animate::aquarelle_modulated_clusters(clusters, opts, t);

        // `base_radius_unit` mirrors `render_static`: min(w,h) * 0.25 * orb_size.
        let base_radius_unit = (width.min(height) as f32) * 0.25 * opts.orb_size.max(0.0);
        let saturation = opts.saturation.max(0.0);

        let orbs = Self::pack_aquarelle_orbs(
            &modulated,
            width as f32,
            height as f32,
            base_radius_unit,
            saturation,
            params,
        );

        self.render_aquarelle_packed(&orbs, width, height, opts.background)
    }

    /// Run the crate's four-layer `render_aquarelle_orb` math on the CPU and pack
    /// each orb into a [`GpuAquaOrb`] row. `seed = orb index` (matching `orb.rs:176`
    /// `i as u64`); the ChaCha8 `gen_range` consumption order is **identical** to
    /// `aquarelle::render_aquarelle_orb` (offset θ → per-satellite θ/dist/radius →
    /// bloom with no RNG) so the satellite count / placement and offset direction
    /// are bit-identical to the crate. Colors run through the same `boost_saturation`
    /// (HSL via `palette`) / `mix_with_white` as the crate, quantized to u8.
    ///
    /// Orbs with radius `<= 0.0` (zero weight) pack a zero-radius row, which the
    /// shader skips — matching the crate's early `return` for non-positive radius.
    ///
    /// Pure data transform (no `self`): an associated function so pack-only tests
    /// can call it without a live GPU adapter (RNG/layout reproduction is verified
    /// on every host, not just GPU CI).
    fn pack_aquarelle_orbs(
        clusters: &[Cluster],
        width: f32,
        height: f32,
        base_radius_unit: f32,
        saturation: f32,
        params: aquarelle::AquarelleParams,
    ) -> Vec<GpuAquaOrb> {
        use std::f32::consts::TAU;

        // `clamped()` mirrors the crate: out-of-range slider values are capped.
        let p = params.clamped();
        let n = clusters.len().min(MAX_ORB_COUNT);

        let mut orbs = Vec::with_capacity(n.max(1));
        for (i, cluster) in clusters.iter().take(n).enumerate() {
            // `render_static`: radius = base_radius_unit * sqrt(weight),
            // color = adjust_saturation(cluster.color, saturation),
            // center = clamp(centroid, 0..1) * (width, height).
            let radius = base_radius_unit * cluster.weight.max(0.0).sqrt();
            let color = adjust_saturation_pub(cluster.color, saturation);
            let center_x = cluster.centroid.x.clamp(0.0, 1.0) * width;
            let center_y = cluster.centroid.y.clamp(0.0, 1.0) * height;

            // Zero / negative radius ⇒ the crate returns early (draws nothing). Pack
            // a zero-radius row; the shader's `main_radius <= 0.0` guard skips it.
            if radius <= 0.0 {
                orbs.push(GpuAquaOrb {
                    main: [center_x, center_y, 0.0, 0.0],
                    inner: [0.0; 4],
                    halo: [0.0; 4],
                    bloom_geom: [0.0; 4],
                    bloom_col: [0.0; 4],
                    bleed_col: [0.0; 4],
                    sat0: [0.0; 4],
                    sat1: [0.0; 4],
                    sat2: [0.0; 4],
                });
                continue;
            }

            // RNG seeded per orb with `seed = i`, consumed in the *exact* order of
            // `render_aquarelle_orb`. Any deviation here desyncs the satellite stream.
            let mut rng = ChaCha8Rng::seed_from_u64(i as u64);

            // 1. offset: shift the gradient center by up to 25 % of the radius.
            let offset_dist = radius * 0.25 * p.offset;
            let theta: f32 = rng.gen_range(0.0..TAU);
            let cx = center_x + offset_dist * theta.cos();
            let cy = center_y + offset_dist * theta.sin();

            // 2. main color: halo color = boost_saturation(color, 1 + 0.6 * halo).
            let halo_color = boost_saturation(color, 1.0 + 0.6 * p.halo);

            // 3. bleed satellites: 0..3 small same-color gradients. The RNG draws
            //    per satellite in order (θ, dist, radius-factor), matching the crate.
            let bleed_count = (3.0 * p.bleed).round() as u32;
            let bleed_color = boost_saturation(color, 1.0 + 0.4 * p.halo);
            let mut sats = [[0.0f32; 4]; 3];
            for sat in sats.iter_mut().take(bleed_count.min(3) as usize) {
                let bleed_theta: f32 = rng.gen_range(0.0..TAU);
                let bleed_dist = radius * rng.gen_range(0.4..0.9);
                let bx = center_x + bleed_dist * bleed_theta.cos();
                let by = center_y + bleed_dist * bleed_theta.sin();
                let bleed_radius = radius * rng.gen_range(0.2..0.4) * (0.5 + 0.5 * p.bleed);
                *sat = [bx, by, bleed_radius, 0.0];
            }

            // 4. bloom: near-white core inside the inner ~30 % when bloom > 0.
            let (bloom_flag, bloom_core_radius, bloom_color) = if p.bloom > 0.0 {
                let core_radius = radius * 0.3 * p.bloom;
                if core_radius > 0.0 {
                    let bloom_color = mix_with_white(color, 0.7);
                    (1.0, core_radius, bloom_color)
                } else {
                    (0.0, 0.0, [0u8; 3])
                }
            } else {
                (0.0, 0.0, [0u8; 3])
            };

            let to_unit = |c: [u8; 3]| {
                [
                    c[0] as f32 / 255.0,
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                ]
            };
            let inner_u = to_unit(color);
            let halo_u = to_unit(halo_color);
            let bleed_u = to_unit(bleed_color);
            let bloom_u = to_unit(bloom_color);

            orbs.push(GpuAquaOrb {
                main: [cx, cy, radius, bleed_count.min(3) as f32],
                inner: [inner_u[0], inner_u[1], inner_u[2], bloom_flag],
                halo: [halo_u[0], halo_u[1], halo_u[2], 0.0],
                // bloom center == main offset center (crate draws bloom at cx,cy).
                bloom_geom: [cx, cy, bloom_core_radius, 0.0],
                bloom_col: [bloom_u[0], bloom_u[1], bloom_u[2], 0.0],
                bleed_col: [bleed_u[0], bleed_u[1], bleed_u[2], 0.0],
                sat0: sats[0],
                sat1: sats[1],
                sat2: sats[2],
            });
        }
        orbs
    }

    /// Upload the packed aquarelle orbs into the grow-only aquarelle data-texture,
    /// build the header uniform, run the aquarelle pass, and read back. Serialized
    /// under `render_guard` (like the Circle/Glyph path) so concurrent renders on a
    /// shared renderer cannot alias the one shared aquarelle texture / per-size
    /// target / read-back buffer (the #210 concurrency contract).
    fn render_aquarelle_packed(
        &self,
        orbs: &[GpuAquaOrb],
        width: u32,
        height: u32,
        background: [u8; 4],
    ) -> RgbaImage {
        let _render_guard = self
            .render_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let width = width.max(1);
        let height = height.max(1);

        let header = AquarelleHeader {
            resolution: [width as f32, height as f32],
            n_orbs: orbs.len() as f32,
            _pad0: 0.0,
            bg: [
                background[0] as f32 / 255.0,
                background[1] as f32 / 255.0,
                background[2] as f32 / 255.0,
                background[3] as f32 / 255.0,
            ],
        };
        let header_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("orber-aquarelle-params"),
                contents: bytemuck::bytes_of(&header),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let orb_view = self.upload_aquarelle_texture(orbs);

        self.pipeline(orb_aquarelle_wgsl(), false, |cached| {
            self.sized_resources(width, height, |res| {
                let entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: header_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&orb_view),
                    },
                ];
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("orber-aquarelle-bg"),
                    layout: &cached.bind_group_layout,
                    entries: &entries,
                });
                self.run_pass_and_readback(&cached.pipeline, &bind_group, res)
            })
        })
    }

    /// Upload the packed aquarelle orbs into the grow-only `Rgba32Float` aquarelle
    /// data-texture (9 texels wide × `orbs.len()` tall) and return a view to bind.
    /// Mirrors [`Self::upload_orb_texture`] but for the separate aquarelle texture so
    /// the Circle/Glyph orb texture is never resized to the wider aquarelle layout.
    fn upload_aquarelle_texture(&self, orbs: &[GpuAquaOrb]) -> wgpu::TextureView {
        let rows = orbs.len().max(1) as u32;
        let mut guard = self
            .aquarelle_texture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let needs_realloc = match guard.as_ref() {
            Some(tex) => tex.capacity < rows,
            None => true,
        };
        if needs_realloc {
            *guard = Some(self.build_aquarelle_texture(rows));
        }
        let tex = guard
            .as_ref()
            .expect("aquarelle texture just ensured present");

        // One `GpuAquaOrb` is 9 × vec4<f32> = 144 bytes = one texel row.
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
                bytes_per_row: Some(AQUARELLE_TEX_BYTES_PER_ROW),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width: AQUARELLE_TEX_WIDTH,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
        tex.view.clone()
    }

    /// Allocate the aquarelle data-texture sized for `capacity` orbs (9 texels wide).
    /// `usage = TEXTURE_BINDING | COPY_DST` (sampled via `textureLoad`, written via
    /// `write_texture`) — input only, like [`Self::build_orb_texture`].
    fn build_aquarelle_texture(&self, capacity: u32) -> OrbTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-aquarelle-tex"),
            size: wgpu::Extent3d {
                width: AQUARELLE_TEX_WIDTH,
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
                if glyph.is_some() {
                    // Glyph (#214): render the fill into the bleed `fill` texture,
                    // then run the aquarelle bleed pass group (premult → box-blur×3
                    // → halo → compose) into `res.target`, then read `res.target`
                    // back. The bleed pipelines / intermediate textures are taken
                    // here, nested under the (already-held) render_guard → pipeline
                    // → sized locks; this is the only path that touches them, so the
                    // ordering stays consistent and cannot deadlock.
                    self.run_glyph_fill_bleed_readback(&cached.pipeline, &bind_group, res)
                } else {
                    self.run_pass_and_readback(&cached.pipeline, &bind_group, res)
                }
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
        self.copy_target_and_readback(encoder, extent, res)
    }

    /// Copy `res.target` into the padded read-back buffer, submit, map, and strip
    /// wgpu's row padding into a tight `RgbaImage`. Shared by the Circle path
    /// ([`run_pass_and_readback`](Self::run_pass_and_readback)) and the Glyph bleed
    /// path ([`run_glyph_fill_bleed_readback`](Self::run_glyph_fill_bleed_readback)),
    /// which both end by reading `res.target` back the same way; `encoder` already
    /// holds the render pass(es) that wrote `res.target`.
    fn copy_target_and_readback(
        &self,
        mut encoder: wgpu::CommandEncoder,
        extent: wgpu::Extent3d,
        res: &SizedResources,
    ) -> RgbaImage {
        let (width, height) = (res.width, res.height);
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

    /// Get-or-build the four Glyph bleed-pass pipelines (#214), compiling the bleed
    /// shader and its pipelines at most once for the renderer's life. Returns the
    /// caller's `f` applied to the cached `BleedPipelines`. Lazy: a Circle-only run
    /// never compiles the bleed shader.
    fn bleed_pipelines<R>(&self, f: impl FnOnce(&BleedPipelines) -> R) -> R {
        let mut guard = self
            .bleed_pipelines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pipelines = guard.get_or_insert_with(|| self.build_bleed_pipelines());
        f(pipelines)
    }

    /// Compile the bleed shader once and build its four fragment-entry pipelines,
    /// all sharing one bind-group layout (uniform 0 + `src` texture 1 + `blurred`
    /// texture 2; both textures `filterable: false` because the shader reads them
    /// with `textureLoad`, keeping the box-blur on exact texel centers like the CPU
    /// crate). All targets are `Rgba8Unorm`, `blend: None`.
    fn build_bleed_pipelines(&self) -> BleedPipelines {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("orber-bleed-bgl"),
                    entries: &[
                        uniform_entry(0),
                        // `src` / `blurred` read via textureLoad (no sampler).
                        orb_texture_entry(1),
                        orb_texture_entry(2),
                    ],
                });
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("orber-bleed-shader"),
                source: wgpu::ShaderSource::Wgsl(orb_glyph_bleed_wgsl().into()),
            });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("orber-bleed-pl"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
        let make = |fs_entry: &str| {
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("orber-bleed-pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(fs_entry),
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
                })
        };
        BleedPipelines {
            blur_h: make("fs_blur_h"),
            blur_v: make("fs_blur_v"),
            halo: make("fs_halo"),
            compose: make("fs_compose"),
            bind_group_layout,
        }
    }

    /// Get-or-build the per-size bleed intermediate textures (`fill` + blur
    /// ping-pong), allocating only on first use of a `(width, height)`. Grow-only
    /// like `sized_cache`.
    fn bleed_textures<R>(&self, width: u32, height: u32, f: impl FnOnce(&BleedTextures) -> R) -> R {
        let mut map = self
            .bleed_textures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map
            .entry((width, height))
            .or_insert_with(|| Self::build_bleed_textures(&self.device, width, height));
        f(entry)
    }

    /// Allocate the three `Rgba8Unorm` bleed intermediates for a size: `fill` (the
    /// glyph fill target, sampled by the bleed passes) and the `ping` / `pong`
    /// blur ping-pong. Each is `RENDER_ATTACHMENT | TEXTURE_BINDING` (written by one
    /// pass, read by the next); none needs `COPY_SRC` (only `res.target` is read
    /// back).
    fn build_bleed_textures(device: &wgpu::Device, width: u32, height: u32) -> BleedTextures {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let make = |label: &str| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            });
            tex.create_view(&wgpu::TextureViewDescriptor::default())
        };
        BleedTextures {
            fill_view: make("orber-bleed-fill"),
            ping_view: make("orber-bleed-ping"),
            pong_view: make("orber-bleed-pong"),
        }
    }

    /// The Glyph (#214) render: draw the glyph fill into the `fill` intermediate,
    /// run the aquarelle bleed pass group over it (premultiply → separable
    /// box-blur ×[`BLEED_BLUR_ITERATIONS`] → halo saturation → intensity compose +
    /// finalize) into `res.target`, then read `res.target` back. `fill_pipeline` /
    /// `fill_bind_group` are the already-built Glyph fill pipeline + bind group.
    ///
    /// One command encoder records every pass in order; wgpu serializes passes that
    /// read a texture a prior pass wrote, so the box-blur ping-pong is correct
    /// without manual barriers. All work runs under the outer `render_guard`, so
    /// concurrent `render_frame` calls cannot alias these shared intermediates.
    fn run_glyph_fill_bleed_readback(
        &self,
        fill_pipeline: &wgpu::RenderPipeline,
        fill_bind_group: &wgpu::BindGroup,
        res: &SizedResources,
    ) -> RgbaImage {
        let (width, height) = (res.width, res.height);
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        self.bleed_pipelines(|bp| {
            self.bleed_textures(width, height, |bt| {
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("orber-glyph-bleed-encoder"),
                        });

                // 1) Glyph fill → `fill` (straight RGBA, as the glyph shader emits).
                self.record_fullscreen_pass(
                    &mut encoder,
                    "orber-glyph-fill-pass",
                    &bt.fill_view,
                    fill_pipeline,
                    fill_bind_group,
                );

                // 2) Bleed pass group. Two blur textures ping-pong: H always writes
                // `ping` (reading the previous V output, or `fill` on iter 0), V
                // always reads `ping` and writes `pong`. After every iteration the
                // blurred layer is in `pong`; the next H reads `pong`. No pass ever
                // reads and writes the same texture. The first H pass sets
                // `premultiply = 1` to turn the straight fill into premultiplied;
                // every later pass reads already-premultiplied data.
                let base = |radius: f32, premultiply: f32| BleedParams {
                    resolution: [width as f32, height as f32],
                    radius,
                    premultiply,
                    halo_factor: BLEED_HALO_FACTOR,
                    intensity: BLEED_INTENSITY,
                    _pad0: 0.0,
                    _pad1: 0.0,
                };
                for i in 0..BLEED_BLUR_ITERATIONS {
                    // H: (fill | pong) → ping. premultiply only on the first pass.
                    let h_src = if i == 0 { &bt.fill_view } else { &bt.pong_view };
                    self.record_bleed_pass(
                        &mut encoder,
                        bp,
                        &bp.blur_h,
                        &bt.ping_view,
                        h_src,
                        None,
                        base(BLEED_BOX_RADIUS, if i == 0 { 1.0 } else { 0.0 }),
                    );
                    // V: ping → pong.
                    self.record_bleed_pass(
                        &mut encoder,
                        bp,
                        &bp.blur_v,
                        &bt.pong_view,
                        &bt.ping_view,
                        None,
                        base(BLEED_BOX_RADIUS, 0.0),
                    );
                }
                // The 3×(H,V) blurred premult layer is now in `pong`.

                // 3) Halo saturation boost: pong → ping (ping is free again).
                self.record_bleed_pass(
                    &mut encoder,
                    bp,
                    &bp.halo,
                    &bt.ping_view,
                    &bt.pong_view,
                    None,
                    base(BLEED_BOX_RADIUS, 0.0),
                );

                // 4) Compose + finalize: original = `fill` (premultiplied inline),
                // blurred = halo output (`ping`) → `res.target` (straight RGBA, read
                // back).
                self.record_bleed_pass(
                    &mut encoder,
                    bp,
                    &bp.compose,
                    &res.target_view,
                    &bt.fill_view,
                    Some(&bt.ping_view),
                    base(BLEED_BOX_RADIUS, 0.0),
                );

                self.copy_target_and_readback(encoder, extent, res)
            })
        })
    }

    /// Record a single full-screen triangle pass into `target` with `pipeline` +
    /// `bind_group` (clear-to-transparent load). Shared by the glyph fill pass and
    /// indirectly by the bleed passes.
    fn record_fullscreen_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
        target: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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

    /// Record one bleed sub-pass: build the per-pass uniform + bind group (uniform
    /// 0, `src` texture 1, `blurred` texture 2 — bound to `src` itself when the pass
    /// does not use a second input), then draw the full-screen triangle into
    /// `target`. `pipeline` selects which `orb_glyph_bleed.wgsl` fragment entry runs.
    #[allow(clippy::too_many_arguments)]
    fn record_bleed_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bp: &BleedPipelines,
        pipeline: &wgpu::RenderPipeline,
        target: &wgpu::TextureView,
        src: &wgpu::TextureView,
        blurred: Option<&wgpu::TextureView>,
        params: BleedParams,
    ) {
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("orber-bleed-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        // Binding 2 (`blurred`) is only read by the compose pass; for the other
        // passes bind `src` to it so the one bind-group layout is always satisfied.
        let blurred_view = blurred.unwrap_or(src);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("orber-bleed-bg"),
            layout: &bp.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(blurred_view),
                },
            ],
        });
        self.record_fullscreen_pass(encoder, "orber-bleed-pass", target, pipeline, &bind_group);
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

/// `boost_saturation` from `aquarelle::render_aquarelle_orb`, reproduced verbatim
/// so the CPU pack produces the **same u8 color** the crate feeds tiny-skia. The
/// HSL transform runs through `palette` at the exact version aquarelle pins
/// (`palette 0.7`, the workspace dep), so the result is bit-identical and never
/// reimplemented in WGSL. A factor within an ULP of 1.0 short-circuits like the
/// crate.
fn boost_saturation(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    if (factor - 1.0).abs() < f32::EPSILON {
        return rgb;
    }
    let srgb = Srgb::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    let mut hsl: Hsl = Hsl::from_color(srgb);
    hsl.saturation = (hsl.saturation * factor).clamp(0.0, 1.0);
    let out: Srgb = hsl.into_color();
    [
        (out.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// `mix_with_white` from `aquarelle::render_aquarelle_orb`, reproduced verbatim so
/// the bloom core color matches the crate's u8 output exactly.
fn mix_with_white(rgb: [u8; 3], amount: f32) -> [u8; 3] {
    let a = amount.clamp(0.0, 1.0);
    [
        (rgb[0] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
        (rgb[1] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
        (rgb[2] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
    ]
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
    use crate::animate::{AnimateOptions, MotionDirection, MotionSpeed};
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

    /// #217: build an `OrbShape::Image` from a synthetic centered silhouette so GPU
    /// image tests do not depend on font assets.
    fn test_image_shape() -> OrbShape {
        let w = 64u32;
        let mut img = image::RgbaImage::from_pixel(w, w, image::Rgba([0, 0, 0, 0]));
        for y in 16..48 {
            for x in 16..48 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let sdf = crate::glyph::image_rgba_to_sdf(&img, 256).expect("test silhouette → Some");
        OrbShape::Image {
            sdf: std::sync::Arc::from(sdf),
            size: 256,
        }
    }

    fn image_opts(w: u32, h: u32) -> AnimateOptions {
        AnimateOptions {
            shape: test_image_shape(),
            ..glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true)
        }
    }

    /// #217: the Image WGSL path (shared with Glyph) must compile and the supplied
    /// silhouette SDF must paint a non-trivial number of foreground pixels.
    #[test]
    fn gpu_image_renders_lit_pixels() {
        let Some(renderer) = require_or_skip_renderer("gpu_image_renders_lit_pixels") else {
            return;
        };
        let clusters = sample_clusters();
        let opts = image_opts(120, 90);
        let img = renderer.render_frame_image(&clusters, &opts, 0.0);
        assert_eq!(img.dimensions(), (120, 90));
        let lit = lit_vs_bg(&img, opts.background, 8);
        assert!(
            lit > 200,
            "image silhouette fill must paint a non-trivial number of lit pixels, got {lit}"
        );
    }

    /// #217: `render_frame_image` is deterministic for the same seed / t.
    #[test]
    fn gpu_image_is_deterministic() {
        let Some(renderer) = require_or_skip_renderer("gpu_image_is_deterministic") else {
            return;
        };
        let clusters = sample_clusters();
        let opts = image_opts(96, 96);
        let a = renderer.render_frame_image(&clusters, &opts, 0.37);
        let b = renderer.render_frame_image(&clusters, &opts, 0.37);
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "render_frame_image must be byte-equal for same seed/t"
        );
    }

    /// #217: an empty (all-zero) image SDF yields a background-only frame, mirroring
    /// the CPU "draw nothing" contract (no panic).
    #[test]
    fn gpu_image_empty_sdf_is_background_only() {
        let Some(renderer) = require_or_skip_renderer("gpu_image_empty_sdf_is_background_only")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts = AnimateOptions {
            shape: OrbShape::Image {
                sdf: std::sync::Arc::from(vec![0u8; 256 * 256]),
                size: 256,
            },
            ..glyph_opts(
                64,
                64,
                MotionDirection::LeftToRight,
                MotionSpeed::Slow,
                true,
            )
        };
        let img = renderer.render_frame_image(&clusters, &opts, 0.0);
        let lit = lit_vs_bg(&img, opts.background, 8);
        assert_eq!(lit, 0, "empty image SDF must paint no foreground pixels");
    }

    /// #217: `render_frame_image` on a non-Image shape falls back to the Circle path
    /// (the call is total). It must still produce a valid frame.
    #[test]
    fn gpu_image_entry_non_image_falls_back_to_circle() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_image_entry_non_image_falls_back_to_circle")
        else {
            return;
        };
        let clusters = sample_clusters();
        let mut opts = image_opts(80, 80);
        opts.shape = OrbShape::Circle;
        let via_image_entry = renderer.render_frame_image(&clusters, &opts, 0.5);
        let via_circle = renderer.render_frame(&clusters, &opts, 0.5);
        assert_eq!(
            via_image_entry.as_raw(),
            via_circle.as_raw(),
            "render_frame_image on Circle must equal render_frame (Circle path)"
        );
    }


    /// #217 (#14): rotation loop closure for the GPU image path. With
    /// `glyph_rotate=true`, `render_frame_image(t=0)` and `(t=1)` must render the
    /// same frame within tolerance (the per-orb rotation + one-way conveyor both
    /// close at integer cycle×speed_mult). Image analogue of
    /// `gpu_glyph_rotation_loop_closure_fast_high_speed`.
    #[test]
    fn gpu_image_rotation_loop_closure_t0_eq_t1() {
        let Some(renderer) = require_or_skip_renderer("gpu_image_rotation_loop_closure_t0_eq_t1")
        else {
            return;
        };
        let clusters = sample_clusters();
        let mut opts = AnimateOptions {
            shape: test_image_shape(),
            speed: MotionSpeed::Fast,
            glyph_rotate: true,
            ..glyph_opts(
                100,
                100,
                MotionDirection::LeftToRight,
                MotionSpeed::Fast,
                true,
            )
        };
        // More orbs → wider speed_mult spread → largest cycle×speed_mult product the
        // wrap + rotation has to close.
        opts.count = Some(24);
        let t0 = renderer.render_frame_image(&clusters, &opts, 0.0);
        let t1 = renderer.render_frame_image(&clusters, &opts, 1.0);
        let max_diff = assert_within_tolerance(
            &t0,
            &t1,
            "image rotation loop closure (Fast, high cycle×speed) t=0 vs t=1",
        );
        eprintln!("image fast loop closure: max per-channel diff = {max_diff}");
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

    // ---- #212 Phase 1b: 4-texel widening / glyph dispatch regression guards ----


    /// #212 (#2): the `render_packed_inner` short-row guard changed from a per-orb
    /// `off + 11` cut-off to `off + 13` (so the new rotation words at `off+11` /
    /// `off+12` are read). A hand-built single-orb Circle pack sized to **exactly**
    /// `off + 13` words must render that orb — it must NOT be cut one orb early.
    /// This is the regression guard for the boundary change: with the old `off+11`
    /// (or a `>=` form) the last orb would have been dropped at this exact length.
    ///
    /// Build a real 1-orb Circle pack via the production packer (so the per-orb
    /// arithmetic is correct), truncate it to `HEADER_WORDS + 13` (dropping only the
    /// 3 trailing unused padding words of the 16-word slot), render it, and assert
    /// (a) it equals the full untruncated pack bit-exact, and (b) the orb is
    /// actually drawn (output is not background-only).
    #[test]
    fn render_packed_short_pack_circle_unaffected_by_13word_guard() {
        let Some(renderer) =
            require_or_skip_renderer("render_packed_short_pack_circle_unaffected_by_13word_guard")
        else {
            return;
        };
        let clusters = sample_clusters();
        let (w, h) = (40u32, 28u32);
        let bg = [12u8, 18, 28, 255];
        // One orb, real packed words (header 16 + per-orb 16 = 32 words total).
        let full = pack_render_data_for_webgl(
            &clusters,
            bg,
            1.0,
            0.5,
            0.0, // direction_id = LeftToRight
            MotionSpeed::Mid.cycle_count() as f32,
            12345,
            1,    // n_orbs = 1
            1.0,  // alpha_mul
            0.0,  // shape_id = Circle
            true, // glyph_rotate (ignored by Circle)
            0.5,  // edge_softness (ignored by Circle)
        );
        // The single orb lives at off = HEADER_WORDS. The shader reads up to
        // pack[off + 12] (rot_speed_signed), so off + 13 words is the minimal length
        // that still carries the whole orb. Truncate to exactly that.
        let off = HEADER_WORDS; // i = 0
        let truncated_len = off + 13;
        assert!(
            full.len() > truncated_len,
            "production pack must be longer than the off+13 minimal slot"
        );
        let mut short = full.clone();
        short.truncate(truncated_len);
        assert_eq!(
            short.len(),
            HEADER_WORDS + 13,
            "short pack must be exactly off+13 words (the boundary case)"
        );

        // The orb must NOT be cut early: the truncated pack must equal the full one.
        let short_img = renderer.render_packed(&short, w, h, 0.3);
        let full_img = renderer.render_packed(&full, w, h, 0.3);
        assert_eq!(
            short_img.dimensions(),
            (w, h),
            "off+13 short pack must still produce a {w}x{h} image"
        );
        let max_diff = assert_within_tolerance(
            &full_img,
            &short_img,
            "off+13 truncated 1-orb pack vs full pack",
        );
        assert_eq!(
            max_diff, 0,
            "off+13 truncated pack must be bit-exact to the full pack \
             (the trailing 3 words are unused padding)"
        );

        // And the orb must actually be drawn — a one-orb-early cut would leave a
        // background-only frame, so confirm some pixels differ from the bg.
        let lit = lit_vs_bg(&short_img, bg, 2);
        assert!(
            lit > 0,
            "the single orb must be drawn (not cut early by the guard); \
             got a background-only frame ({lit} non-bg pixels)"
        );
        eprintln!("off+13 short-pack guard: lit pixels = {lit}, max diff vs full = {max_diff}");
    }

    /// #212 (#3): the Glyph path must handle orb counts straddling the orb-texture
    /// grow boundary (64). For {1, 64, 65, 1024} the glyph fill must light pixels,
    /// the output dims must be correct, and there must be no panic / OOB. (Circle
    /// covers count parity numerically; here the concern is the Glyph dispatch +
    /// data-texture growth, not bit-exactness.)
    #[test]
    fn gpu_glyph_count_boundaries() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_count_boundaries") else {
            return;
        };
        let clusters = sample_clusters();
        for &count in &[1usize, 64, 65, 1024] {
            let mut opts = glyph_opts(
                96,
                72,
                MotionDirection::LeftToRight,
                MotionSpeed::Slow,
                true,
            );
            opts.count = Some(count);
            let img = renderer.render_frame_glyph(&clusters, &opts, 0.0);
            assert_eq!(
                img.dimensions(),
                (96, 72),
                "count={count} glyph frame must be 96x72"
            );
            let lit = lit_vs_bg(&img, opts.background, 8);
            assert!(
                lit > 0,
                "count={count} glyph fill must light some pixels (no panic/OOB), got {lit}"
            );
            eprintln!("glyph count={count}: lit pixels = {lit}");
        }
    }

    /// #212 (#4): empty clusters on the Glyph path → background-only frame, correct
    /// dims, no panic. The glyph fill has nothing to draw, so only the background
    /// paints (mirrors the Circle empty-clusters test for the glyph dispatch).
    #[test]
    fn gpu_glyph_empty_clusters_background_only() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_empty_clusters_background_only")
        else {
            return;
        };
        let opts = glyph_opts(
            48,
            40,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        let img = renderer.render_frame_glyph(&[], &opts, 0.3);
        assert_eq!(img.dimensions(), (48, 40), "empty-cluster glyph frame dims");
        let lit = lit_vs_bg(&img, opts.background, 1);
        assert_eq!(
            lit, 0,
            "empty clusters must paint background only on the glyph path, got {lit} non-bg pixels"
        );
    }

    /// #212 (#5): when `opacity * alpha_mul == 0` the glyph fill contributes nothing,
    /// so the frame must be background-only. The fill alpha is `opacity * alpha_mul`;
    /// driving `alpha_mul` (header[9]) to 0 collapses every orb's contribution to 0
    /// regardless of the SDF / opacity envelope. Build a real glyph pack (so the SDF
    /// binding and orb rows are valid), then force `alpha_mul = 0` and assert the
    /// frame paints only the background. Pins the `alpha_mul == 0` → no-fill invariant.
    #[test]
    fn gpu_glyph_alpha_mul_zero_no_fill() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_alpha_mul_zero_no_fill") else {
            return;
        };
        let clusters = sample_clusters();
        let bg = [10u8, 12, 20, 255];
        // Pick a glyph + radius so cached_glyph_sdf_for_radius yields a real SDF.
        let (w, h) = (80u32, 64u32);
        let base_radius_unit = (w.min(h) as f32) * 0.25;
        let frame_radius = base_radius_unit * 1.0 * BREATH_RADIUS_MAX_FACTOR;
        let Some((sdf, sdf_size)) = crate::glyph::cached_glyph_sdf_for_radius(
            crate::glyph::GlyphFontId::NotoSymbols2,
            '☆',
            frame_radius,
        ) else {
            panic!("expected a real SDF for ☆ at this radius");
        };
        // Build a real 4-orb glyph pack, then force alpha_mul (header[9]) to 0 so
        // every orb's fill alpha = opacity * alpha_mul = 0 → no fill anywhere.
        let mut pack = pack_render_data_for_webgl(
            &clusters,
            bg,
            base_radius_unit,
            0.5,
            0.0,
            MotionSpeed::Slow.cycle_count() as f32,
            7,
            4,
            0.0, // alpha_mul = 0 → fill alpha collapses to 0
            1.0, // shape_id = Glyph
            true,
            0.5,
        );
        // (header[9] is already 0.0 from the arg above; re-pin defensively in case
        // the packer ever changes which header slot carries alpha_mul.)
        pack[9] = 0.0;
        let sdf_view = renderer.upload_glyph_sdf('☆', sdf_size, &sdf);
        let img = renderer.render_packed_inner(
            &pack,
            w,
            h,
            0.4,
            Some(GlyphBindings {
                sdf_view: &sdf_view,
                size: sdf_size,
            }),
        );
        assert_eq!(img.dimensions(), (w, h));
        let lit = lit_vs_bg(&img, bg, 1);
        assert_eq!(
            lit, 0,
            "opacity*alpha_mul == 0 must paint background only, got {lit} non-bg pixels"
        );
    }

    /// #212 (#6): rotation loop closure under stress — ON, MotionSpeed::Fast (high
    /// cycle count) plus high per-orb speed multipliers — the t=0 and t=1 frames
    /// must still match within tolerance. This stresses the WGSL `fract` / `floor`
    /// wrap (`turns = cycle * speed * t - floor(...)`) against the CPU `rem_euclid`
    /// at the largest `cycle * speed` products, where a float-precision mismatch in
    /// the wrap would be most visible. (Loop closure relies on `cycle * speed_mult`
    /// being integral so turns = 0 at t = 1.)
    #[test]
    fn gpu_glyph_rotation_loop_closure_fast_high_speed() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_glyph_rotation_loop_closure_fast_high_speed")
        else {
            return;
        };
        let clusters = sample_clusters();
        let mut opts = glyph_opts(
            100,
            100,
            MotionDirection::LeftToRight,
            MotionSpeed::Fast,
            true,
        );
        // More orbs → a wider spread of per-orb speed_mult values, maximizing the
        // largest cycle * speed_mult product the wrap has to close.
        opts.count = Some(24);
        let t0 = renderer.render_frame_glyph(&clusters, &opts, 0.0);
        let t1 = renderer.render_frame_glyph(&clusters, &opts, 1.0);
        let max_diff = assert_within_tolerance(
            &t0,
            &t1,
            "glyph rotation loop closure (Fast, high cycle×speed) t=0 vs t=1",
        );
        eprintln!("glyph fast loop closure: max per-channel diff = {max_diff}");
    }

    /// #212 (#8): the glyph SDF cache reuses the same `(char, size)` upload across
    /// frames. Two frames with the same glyph at the same frame radius/size must
    /// leave the cache at exactly one entry (the second frame reuses the first
    /// upload, not re-uploads). Uses a *private* renderer so the count isn't
    /// perturbed by the other glyph tests sharing the renderer.
    #[test]
    fn gpu_glyph_sdf_cache_reuse_same_char_size() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("gpu_glyph_sdf_cache_reuse_same_char_size")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts = glyph_opts(
            80,
            64,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        assert_eq!(renderer.glyph_sdf_cache_len(), 0, "no glyph uploaded yet");
        let _ = renderer.render_frame_glyph(&clusters, &opts, 0.0);
        assert_eq!(
            renderer.glyph_sdf_cache_len(),
            1,
            "first glyph frame must upload exactly one SDF"
        );
        // Same char + same size (same opts → same frame radius → same SDF size).
        let _ = renderer.render_frame_glyph(&clusters, &opts, 0.5);
        assert_eq!(
            renderer.glyph_sdf_cache_len(),
            1,
            "same (char, size) re-render must reuse the cached SDF (still 1 entry)"
        );
    }

    /// #212 (#9): a different char (♪) or a different size must add a new glyph SDF
    /// cache entry — both frames must still render correctly (lit pixels). Uses a
    /// *private* renderer so the entry count is observable in isolation.
    #[test]
    fn gpu_glyph_sdf_cache_grows_on_new_char_or_size() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("gpu_glyph_sdf_cache_grows_on_new_char_or_size")
        else {
            return;
        };
        let clusters = sample_clusters();
        // Frame 1: ☆ at the default size.
        let star = glyph_opts(
            80,
            64,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        let star_img = renderer.render_frame_glyph(&clusters, &star, 0.0);
        assert_eq!(renderer.glyph_sdf_cache_len(), 1, "first glyph → 1 entry");
        assert!(
            lit_vs_bg(&star_img, star.background, 8) > 0,
            "☆ frame must light pixels"
        );

        // Frame 2: a *different char* (★, a filled star — distinct from ☆ and also
        // present in Noto Sans Symbols 2) → new cache entry.
        let filled = AnimateOptions {
            shape: OrbShape::Glyph {
                ch: '★',
                font: crate::glyph::GlyphFontId::NotoSymbols2,
            },
            ..star.clone()
        };
        let filled_img = renderer.render_frame_glyph(&clusters, &filled, 0.0);
        assert_eq!(
            renderer.glyph_sdf_cache_len(),
            2,
            "a different char must add a glyph SDF cache entry"
        );
        assert!(
            lit_vs_bg(&filled_img, filled.background, 8) > 0,
            "★ frame must light pixels"
        );

        // Frame 3: same ☆ but a *different SDF size*. The SDF size is
        // `next_power_of_two(max(radius * 2.25, 256))`, so the 80x64 frames above
        // both clamp to the 256 floor; to bump the chosen size the frame radius must
        // exceed ~114 px. A large canvas + large orb_size pushes the frame radius
        // there, selecting a larger power-of-two SDF and thus a new `(char, size)`
        // cache entry for the *same* char.
        let big_star = glyph_opts(
            400,
            320,
            MotionDirection::LeftToRight,
            MotionSpeed::Slow,
            true,
        );
        let big_star = AnimateOptions {
            orb_size: 4.0,
            ..big_star
        };
        let big_img = renderer.render_frame_glyph(&clusters, &big_star, 0.0);
        assert_eq!(
            renderer.glyph_sdf_cache_len(),
            3,
            "a different SDF size for the same char must add a glyph SDF cache entry"
        );
        assert!(
            lit_vs_bg(&big_img, big_star.background, 8) > 0,
            "larger ☆ frame must light pixels"
        );
    }

    /// #212 (#10): concurrent glyph renders on the shared renderer must each match
    /// their solo oracle — no panic / poison, no cross-thread aliasing of the shared
    /// orb texture / per-size resources. Several threads render glyph frames with
    /// mixed char / t on the *same* renderer; each thread's output must equal that
    /// same input rendered alone (a fresh renderer). Mirrors the Circle concurrent
    /// test (`shared_gpu_concurrent_high_count_render`) for the glyph path.
    #[test]
    fn shared_gpu_concurrent_glyph_render() {
        let Some(renderer) = require_or_skip_renderer("shared_gpu_concurrent_glyph_render") else {
            return;
        };
        // (char, t) cases — mixed glyph + time so threads exercise different SDF
        // uploads and rotation angles at once. Both ☆ and ★ are in the bundled
        // Noto Sans Symbols 2 subset, so each produces a real (distinct) fill.
        let cases: [(char, f32); 4] = [('☆', 0.0), ('★', 0.25), ('☆', 0.5), ('★', 0.75)];

        // Per-case oracle: render each alone on a fresh renderer first.
        let mut oracles = Vec::new();
        for &(ch, t) in &cases {
            let mut opts = glyph_opts(
                72,
                56,
                MotionDirection::LeftToRight,
                MotionSpeed::Slow,
                true,
            );
            opts.shape = OrbShape::Glyph {
                ch,
                font: crate::glyph::GlyphFontId::NotoSymbols2,
            };
            let clusters = sample_clusters();
            let Some(fresh) =
                require_or_skip_fresh_renderer("shared_gpu_concurrent_glyph_render (oracle leg)")
            else {
                return;
            };
            oracles.push(fresh.render_frame_glyph(&clusters, &opts, t));
        }

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (idx, &(ch, t)) in cases.iter().enumerate() {
                let oracle = &oracles[idx];
                handles.push(scope.spawn(move || {
                    let clusters = sample_clusters();
                    let mut opts = glyph_opts(
                        72,
                        56,
                        MotionDirection::LeftToRight,
                        MotionSpeed::Slow,
                        true,
                    );
                    opts.shape = OrbShape::Glyph {
                        ch,
                        font: crate::glyph::GlyphFontId::NotoSymbols2,
                    };
                    // A few iterations per thread to maximize overlap on the shared
                    // orb texture / per-size resources.
                    for _ in 0..3 {
                        let img = renderer.render_frame_glyph(&clusters, &opts, t);
                        assert_within_tolerance(
                            oracle,
                            &img,
                            &format!("concurrent glyph ch={ch} t={t} vs solo render"),
                        );
                    }
                }));
            }
            for h in handles {
                h.join().expect("concurrent glyph render thread panicked");
            }
        });
        eprintln!("concurrent glyph render: all threads matched their solo oracle");
    }

    /// #212 (#12): the glyph path is deterministic — same opts / seed / t rendered
    /// twice must be byte-identical, with rotation ON (so the rotation arithmetic is
    /// part of what must be reproducible). Guards against any nondeterminism creeping
    /// into the glyph dispatch (e.g. uninitialized padding, cache state leaking).
    #[test]
    fn gpu_glyph_determinism_same_seed_same_output() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_glyph_determinism_same_seed_same_output")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts = glyph_opts(88, 66, MotionDirection::LeftToRight, MotionSpeed::Mid, true);
        let a = renderer.render_frame_glyph(&clusters, &opts, 0.37);
        let b = renderer.render_frame_glyph(&clusters, &opts, 0.37);
        assert_eq!(
            a, b,
            "same opts/seed/t must render byte-identical glyph frames (rotation ON)"
        );
        eprintln!("glyph determinism: two renders byte-identical");
    }

    // ---- #214 Phase 1b.5: Glyph bleed/halo 2nd pass (WGSL) ----

    /// A Glyph `AnimateOptions` matching the CPU bleed oracle's setup: a single
    /// white centered cluster, `orb_size = 1.0`, `softness = Low` (the sharp
    /// pre-#205 baseline the CPU bleed tests pin so the halo R survives the blur),
    /// no flow advance / rotation so the glyph sits at the canvas center. Mirrors
    /// the `orb.rs` `glyph_bleed_produces_halo_around_lit_pixel_cluster` opts.
    fn glyph_bleed_opts(w: u32, h: u32) -> AnimateOptions {
        AnimateOptions {
            width: w,
            height: h,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction: MotionDirection::LeftToRight,
            speed: MotionSpeed::Slow,
            seed: 0,
            count: Some(1),
            background: [0, 0, 0, 255],
            shape: OrbShape::Glyph {
                ch: '☆',
                font: crate::glyph::GlyphFontId::NotoSymbols2,
            },
            softness: SoftnessPreset::Low,
            glyph_rotate: false,
            color_tracks: None,
            keyframe_tracks: None,
        }
    }

    /// #214 (A): the GPU bleed pass must leak a **halo ring** outside the glyph
    /// body — the direct evidence the box-blur/compose 2nd pass ran. Mirrors the
    /// CPU oracle `glyph_bleed_produces_halo_around_lit_pixel_cluster`: a single
    /// white ☆ at 64×64, `orb_size = 1.0` (radius ≈ 16 px, center (32,32)). The
    /// star body is essentially complete by r ≈ 16; the ring 18..21 px from the
    /// center is outside the body, so any lit (R>0) pixel there must come from the
    /// bleed pass spreading the fill outward. Without the 2nd pass that ring would
    /// be pure background black. Loose count (`>= 10`) like the CPU oracle (noise
    /// is omitted on the GPU, so the absolute count differs from the CPU's).
    #[test]
    fn gpu_glyph_bleed_produces_halo_ring() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_bleed_produces_halo_ring") else {
            return;
        };
        eprintln!(
            "GPU Glyph bleed halo test running on adapter: {}",
            renderer.adapter_name()
        );
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = glyph_bleed_opts(64, 64);
        let img = renderer.render_frame_glyph(&[c], &opts, 0.0);
        assert_eq!(img.dimensions(), (64, 64));
        let (cx, cy) = (32.0f32, 32.0f32);
        let mut halo_count = 0usize;
        let mut halo_max_r = 0u8;
        for y in 0..64u32 {
            for x in 0..64u32 {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                if (18.0..21.0).contains(&d) {
                    let px = img.get_pixel(x, y);
                    if px[0] > 0 {
                        halo_count += 1;
                        halo_max_r = halo_max_r.max(px[0]);
                    }
                }
            }
        }
        assert!(
            halo_count >= 10,
            "GPU bleed pass must leak a halo (R>0) into the ring 18..21px from the \
             glyph center; found {halo_count} halo pixels (max R = {halo_max_r}). \
             A missing 2nd pass would leave this ring pure background black."
        );
        eprintln!("gpu bleed halo ring: {halo_count} lit pixels (max R = {halo_max_r})");
    }

    /// #214 (A): the bleed pass must not wash the glyph fill out — the lit body
    /// pixels must survive the box-blur/compose. Mirrors the CPU oracle
    /// `glyph_lit_pixels_remain_visible_after_bleed` (softness Low ☆, `lit > 32`
    /// counted as R > 32). If the compose `intensity` over-weighted the (dimmer)
    /// blurred layer the bright body would collapse below this threshold.
    #[test]
    fn gpu_glyph_lit_pixels_remain_visible_after_bleed() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_glyph_lit_pixels_remain_visible_after_bleed")
        else {
            return;
        };
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = glyph_bleed_opts(100, 100);
        let img = renderer.render_frame_glyph(&[c], &opts, 0.0);
        let lit = img.pixels().filter(|p| p[0] > 32).count();
        assert!(
            lit > 32,
            "glyph lit pixels must survive the GPU bleed pass (R>32), got lit={lit}"
        );
        eprintln!("gpu lit-after-bleed: lit={lit}");
    }

    /// #214 (A): empty clusters routed explicitly through `render_frame_glyph`
    /// must stay background-only after the bleed contract — a blur of nothing is
    /// still nothing. (The empty path early-outs before the fill, so this also
    /// pins that the bleed orchestration is never asked to halo a blank canvas.)
    /// Named for the bleed intent even though `gpu_glyph_empty_clusters_background_only`
    /// covers the plain glyph dispatch.
    #[test]
    fn gpu_glyph_bleed_empty_clusters_stays_background() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_glyph_bleed_empty_clusters_stays_background")
        else {
            return;
        };
        let opts = glyph_bleed_opts(48, 40);
        let img = renderer.render_frame_glyph(&[], &opts, 0.3);
        assert_eq!(img.dimensions(), (48, 40));
        let lit = lit_vs_bg(&img, opts.background, 1);
        assert_eq!(
            lit, 0,
            "empty clusters + glyph bleed path must stay background-only, got {lit} non-bg pixels"
        );
    }

    /// #214 (A): a weight-0 cluster yields zero orbs (radius 0 → skipped), so the
    /// glyph fill is empty and the bleed pass has nothing to spread. The frame
    /// must stay background-only. Mirrors the CPU oracle
    /// `glyph_zero_weight_cluster_stays_black_after_bleed`.
    #[test]
    fn gpu_glyph_bleed_weight_zero_stays_background() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_glyph_bleed_weight_zero_stays_background")
        else {
            return;
        };
        let opts = glyph_bleed_opts(48, 40);
        let img =
            renderer.render_frame_glyph(&[cluster([255, 255, 255], 0.5, 0.5, 0.0)], &opts, 0.3);
        let lit = lit_vs_bg(&img, opts.background, 1);
        assert_eq!(
            lit, 0,
            "weight=0 cluster + glyph bleed path must stay background-only, got {lit} non-bg pixels"
        );
    }

    /// #214 (A): an unknown glyph (pizza emoji, absent from the bundled Symbols 2
    /// subset) produces no SDF, so the fill is empty and the bleed pass spreads
    /// nothing — the frame must stay background-only. The "draw nothing for tofu"
    /// contract must hold through the bleed path too.
    #[test]
    fn gpu_glyph_bleed_unknown_char_stays_background() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_glyph_bleed_unknown_char_stays_background")
        else {
            return;
        };
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let mut opts = glyph_bleed_opts(48, 40);
        opts.shape = OrbShape::Glyph {
            ch: '\u{1F355}', // pizza — not in Noto Sans Symbols 2
            font: crate::glyph::GlyphFontId::NotoSymbols2,
        };
        let img = renderer.render_frame_glyph(&[c], &opts, 0.3);
        let lit = lit_vs_bg(&img, opts.background, 1);
        assert_eq!(
            lit, 0,
            "unknown glyph + bleed path must stay background-only, got {lit} non-bg pixels"
        );
    }

    /// #214 (A): the whole glyph + bleed pipeline is fully deterministic. The
    /// paper-grain noise (the one nondeterministic-looking step) is omitted on the
    /// GPU, so the same opts/seed/t rendered twice must be **byte-identical** — no
    /// jitter from the box-blur ping-pong, the halo HSL transform, or the compose.
    /// Stronger than `gpu_glyph_determinism_same_seed_same_output` because it pins
    /// determinism specifically over the bleed pass (single centered glyph, the
    /// halo well clear of the body).
    #[test]
    fn gpu_glyph_bleed_determinism_byte_identical() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_bleed_determinism_byte_identical")
        else {
            return;
        };
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = glyph_bleed_opts(64, 64);
        let a = renderer.render_frame_glyph(&[c], &opts, 0.42);
        let b = renderer.render_frame_glyph(&[c], &opts, 0.42);
        assert_eq!(
            a, b,
            "glyph bleed pass must be byte-identical on repeat (noise omitted → full determinism)"
        );
        eprintln!("gpu glyph bleed determinism: two renders byte-identical");
    }


    /// #214 (B): the box-blur radius clamp (`r = min(radius, w-1)` / `min(radius,
    /// h-1)` in `orb_glyph_bleed.wgsl`) must keep tiny canvases from sampling out
    /// of bounds. Several sub-`2r+1` sizes (1×1, 2×2, 5×4, where the box radius 3
    /// exceeds the dimension) must render without panic / device loss and produce a
    /// correctly-sized image. A missing clamp would read negative / past-edge
    /// texels.
    #[test]
    fn gpu_glyph_bleed_tiny_canvas_no_panic() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_bleed_tiny_canvas_no_panic")
        else {
            return;
        };
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        for &(w, h) in &[(1u32, 1u32), (2, 2), (5, 4)] {
            let opts = glyph_bleed_opts(w, h);
            let img = renderer.render_frame_glyph(&[c], &opts, 0.0);
            assert_eq!(
                img.dimensions(),
                (w, h),
                "tiny {w}x{h} glyph bleed frame must have correct dims (radius clamp held)"
            );
        }
        eprintln!("gpu glyph bleed tiny canvas: 1x1 / 2x2 / 5x4 rendered without panic");
    }

    /// #214 (B): re-rendering a glyph at the **same size** must reuse the cached
    /// `BleedTextures` (fill + ping/pong), leaving the per-size bleed cache at
    /// exactly one entry — mirrors `caches_resources_across_a_clip` for the bleed
    /// intermediates. Uses a *fresh* renderer so the count is observable in
    /// isolation (the shared renderer accumulates sizes from other tests).
    #[test]
    fn gpu_glyph_bleed_textures_reuse_same_size() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("gpu_glyph_bleed_textures_reuse_same_size")
        else {
            return;
        };
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let opts = glyph_bleed_opts(64, 48);
        assert_eq!(
            renderer.bleed_textures_len(),
            0,
            "no glyph frame yet → 0 bleed entries"
        );
        let _ = renderer.render_frame_glyph(&[c], &opts, 0.0);
        assert_eq!(
            renderer.bleed_textures_len(),
            1,
            "first glyph frame must allocate exactly one bleed-texture entry"
        );
        let _ = renderer.render_frame_glyph(&[c], &opts, 0.5);
        assert_eq!(
            renderer.bleed_textures_len(),
            1,
            "same-size second glyph frame must reuse the cached bleed textures (still 1)"
        );
    }

    /// #214 (B): a glyph frame at a **new size** must add one bleed-texture entry
    /// (grow-only, like `sized_cache`). Uses a *fresh* renderer so the exact entry
    /// count is observable. Two distinct sizes → exactly two entries.
    #[test]
    fn gpu_glyph_bleed_textures_grow_on_new_size() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("gpu_glyph_bleed_textures_grow_on_new_size")
        else {
            return;
        };
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let _ = renderer.render_frame_glyph(&[c], &glyph_bleed_opts(64, 48), 0.0);
        assert_eq!(
            renderer.bleed_textures_len(),
            1,
            "first size → 1 bleed entry"
        );
        let _ = renderer.render_frame_glyph(&[c], &glyph_bleed_opts(40, 32), 0.0);
        assert_eq!(
            renderer.bleed_textures_len(),
            2,
            "a second distinct size must add one bleed-texture entry (grow-only)"
        );
    }

    /// #214 (B): the bleed pipelines are **lazy** — a renderer that only ever drew
    /// Circle frames must never compile the bleed shader (`bleed_pipelines_built()`
    /// stays `false`), and the first glyph (lit) frame must compile them (flips to
    /// `true`). A fresh renderer isolates the lazy state. Uses a real ☆ so the
    /// glyph path actually enters `run_glyph_fill_bleed_readback` (an empty/unknown
    /// glyph early-outs through the Circle pipeline and would *not* build them).
    #[test]
    fn gpu_bleed_pipelines_lazy_not_built_for_circle_only() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("gpu_bleed_pipelines_lazy_not_built_for_circle_only")
        else {
            return;
        };
        let clusters = sample_clusters();
        assert!(
            !renderer.bleed_pipelines_built(),
            "fresh renderer must not have compiled the bleed pipelines yet"
        );
        // Several Circle frames: still no bleed shader.
        let circle = circle_opts(48, 32, MotionDirection::LeftToRight, MotionSpeed::Slow);
        for k in 0..4 {
            let _ = renderer.render_frame(&clusters, &circle, k as f32 / 4.0);
        }
        assert!(
            !renderer.bleed_pipelines_built(),
            "Circle-only rendering must never compile the bleed pipelines"
        );
        // First glyph frame with a real fill compiles them.
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let _ = renderer.render_frame_glyph(&[c], &glyph_bleed_opts(48, 32), 0.0);
        assert!(
            renderer.bleed_pipelines_built(),
            "the first glyph (lit) frame must compile the bleed pipelines"
        );
    }

    /// #214 (C): concurrent glyph+bleed renders on the shared renderer at the
    /// **same size** must each match their solo oracle within ±2/channel — i.e. the
    /// bleed intermediates (`fill` / `ping` / `pong`) are never aliased across
    /// threads. The per-size bleed textures are shared mutable state behind the
    /// `render_guard`; if a second thread's box-blur overwrote `ping` mid-pass for
    /// the first thread the halo would corrupt. Several threads hammer the *same*
    /// size + char + t (collision maximized), each output compared to a fresh solo
    /// render. Strengthens `shared_gpu_concurrent_glyph_render` by explicitly
    /// stressing the bleed mid-pass textures (Phase 1a's #210 aliasing class).
    #[test]
    fn shared_gpu_concurrent_glyph_bleed_no_aliasing() {
        let Some(renderer) =
            require_or_skip_renderer("shared_gpu_concurrent_glyph_bleed_no_aliasing")
        else {
            return;
        };
        // Same size for every thread so they all contend the one cached
        // `(w,h)` BleedTextures entry — that is the aliasing surface under test.
        let (w, h) = (72u32, 56u32);
        let make_opts = || glyph_bleed_opts(w, h);

        // Solo oracle on a fresh renderer (its own bleed textures, uncontended).
        let Some(oracle_renderer) = require_or_skip_fresh_renderer(
            "shared_gpu_concurrent_glyph_bleed_no_aliasing (oracle)",
        ) else {
            return;
        };
        let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
        let oracle = oracle_renderer.render_frame_glyph(&[c], &make_opts(), 0.3);

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..4 {
                let oracle = &oracle;
                handles.push(scope.spawn(move || {
                    let c = cluster([255, 255, 255], 0.5, 0.5, 1.0);
                    let opts = make_opts();
                    // Reuse the same char/size/t so all threads collide on the one
                    // shared BleedTextures entry as hard as possible.
                    for _ in 0..4 {
                        let img = renderer.render_frame_glyph(&[c], &opts, 0.3);
                        assert_within_tolerance(
                            oracle,
                            &img,
                            "concurrent glyph bleed vs solo render (no intermediate aliasing)",
                        );
                    }
                }));
            }
            for handle in handles {
                handle
                    .join()
                    .expect("concurrent glyph bleed thread panicked");
            }
        });
        eprintln!("concurrent glyph bleed: all threads matched solo oracle (no mid-pass aliasing)");
    }


    // ===== #216: Aquarelle WGSL path — RNG/color parity + structural GPU↔CPU =====

    use aquarelle::AquarelleParams;

    fn aquarelle_opts(w: u32, h: u32, params: AquarelleParams) -> AnimateOptions {
        AnimateOptions {
            width: w,
            height: h,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction: MotionDirection::LeftToRight,
            speed: MotionSpeed::Mid,
            seed: 12345,
            // Aquarelle ignores --count (one orb per cluster), so count is irrelevant.
            count: None,
            background: [12, 18, 28, 255],
            shape: OrbShape::Aquarelle(params),
            softness: SoftnessPreset::Mid,
            glyph_rotate: true,
            color_tracks: None,
            keyframe_tracks: None,
        }
    }

    /// `boost_saturation` / `mix_with_white` are reproduced verbatim from the crate.
    /// A factor of exactly 1.0 (halo=0 ⇒ 1+0.6*0) must be the identity fast-path,
    /// and a boost must raise HSL saturation (here: pull a desaturated gray toward
    /// its hue). `mix_with_white(c, 0.7)` must land 70 % of the way to white.
    #[test]
    fn aquarelle_color_helpers_match_crate_semantics() {
        // Identity fast-path: factor 1.0 returns the input untouched.
        assert_eq!(boost_saturation([200, 100, 50], 1.0), [200, 100, 50]);

        // A boost on an already-colored pixel must not *lower* its max-min spread
        // (saturation goes up, clamped at 1.0).
        let base = [200u8, 120, 60];
        let boosted = boost_saturation(base, 1.6);
        let spread = |c: [u8; 3]| c.iter().max().unwrap() - c.iter().min().unwrap();
        assert!(
            spread(boosted) >= spread(base),
            "boost_saturation must not reduce chroma: base={base:?} boosted={boosted:?}"
        );

        // mix_with_white(c, 0.7): each channel = round(c*0.3 + 255*0.7).
        let mixed = mix_with_white([100, 0, 200], 0.7);
        let expect = |c: u8| (c as f32 * 0.3 + 255.0 * 0.7).round() as u8;
        assert_eq!(mixed, [expect(100), expect(0), expect(200)]);

        // Full white mix => white; zero mix => unchanged.
        assert_eq!(mix_with_white([10, 20, 30], 1.0), [255, 255, 255]);
        assert_eq!(mix_with_white([10, 20, 30], 0.0), [10, 20, 30]);
    }

    /// `bleed_count = round(3 * bleed)` and the satellite stream is consumed in the
    /// crate's order. Pack one orb and assert the satellite count for representative
    /// `bleed` values, plus that bloom presence tracks `bloom > 0`.
    #[test]
    fn aquarelle_pack_satellite_count_and_bloom_flag() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this RNG/layout test runs on every host, not just GPU CI.
        let single = vec![cluster([200, 120, 60], 0.5, 0.5, 1.0)];
        // bleed → expected round(3*bleed): 0→0, 0.5→2 (1.5 rounds to 2), 1.0→3.
        for (bleed, expected) in [(0.0_f32, 0u32), (0.5, 2), (1.0, 3)] {
            let params = AquarelleParams {
                bleed,
                bloom: 0.5,
                offset: 0.5,
                halo: 0.5,
            };
            let orbs = GpuRenderer::pack_aquarelle_orbs(
                &single,
                200.0,
                200.0,
                40.0,
                1.0,
                params.clamped(),
            );
            assert_eq!(orbs.len(), 1);
            assert_eq!(
                orbs[0].main[3] as u32, expected,
                "bleed={bleed} must pack {expected} satellites"
            );
            // bloom=0.5 > 0 ⇒ bloom_flag set, core radius positive.
            assert!(orbs[0].inner[3] > 0.5, "bloom>0 must set bloom_flag");
            assert!(
                orbs[0].bloom_geom[2] > 0.0,
                "bloom core radius must be positive when bloom>0"
            );
        }
        // bloom=0 ⇒ no bloom layer.
        let no_bloom = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        };
        let orbs = GpuRenderer::pack_aquarelle_orbs(&single, 200.0, 200.0, 40.0, 1.0, no_bloom);
        assert!(orbs[0].inner[3] < 0.5, "bloom=0 must clear bloom_flag");
    }


    /// Aquarelle GPU determinism: the same `(clusters, opts, t)` must render
    /// byte-identical twice (the pack RNG is seeded per orb index, no thread_rng).
    #[test]
    fn aquarelle_gpu_determinism_byte_identical() {
        let Some(renderer) = require_or_skip_renderer("aquarelle_gpu_determinism_byte_identical")
        else {
            return;
        };
        let clusters = vec![
            cluster([200, 100, 50], 0.4, 0.4, 0.7),
            cluster([50, 180, 120], 0.6, 0.6, 0.5),
        ];
        let opts = aquarelle_opts(80, 80, AquarelleParams::default());
        let a = renderer.render_frame_aquarelle(&clusters, &opts, 0.0);
        let b = renderer.render_frame_aquarelle(&clusters, &opts, 0.0);
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "aquarelle GPU render must be byte-identical on repeated calls"
        );
    }

    /// #216: concurrent aquarelle renders on the shared renderer must each match
    /// their solo oracle — no panic / poison, no cross-thread aliasing of the shared
    /// aquarelle data-texture / per-size target / read-back buffer (the #210
    /// serialization contract). Several threads render different aquarelle frames
    /// (mixed params + t) on the *same* renderer; each thread's output must equal
    /// that same input rendered alone (a fresh renderer). Mirrors the Circle
    /// (`shared_gpu_concurrent_high_count_render`) and Glyph
    /// (`shared_gpu_concurrent_glyph_render`) concurrent tests for the aquarelle
    /// path. Aquarelle is deterministic (per-orb-index ChaCha8 seed, no thread_rng),
    /// so the match is byte-identical, not just within tolerance.
    #[test]
    fn aquarelle_shared_gpu_concurrent_render() {
        let Some(renderer) = require_or_skip_renderer("aquarelle_shared_gpu_concurrent_render")
        else {
            return;
        };
        // (params, t) cases — mixed layer params + time so threads exercise different
        // packs (satellite counts, bloom, offset) and per-orb modulation at once.
        let cases: [(AquarelleParams, f32); 4] = [
            (
                AquarelleParams {
                    bleed: 1.0,
                    bloom: 0.8,
                    offset: 0.6,
                    halo: 0.4,
                },
                0.0,
            ),
            (
                AquarelleParams {
                    bleed: 0.5,
                    bloom: 0.0,
                    offset: 0.3,
                    halo: 0.7,
                },
                0.25,
            ),
            (
                AquarelleParams {
                    bleed: 0.0,
                    bloom: 0.5,
                    offset: 0.9,
                    halo: 0.2,
                },
                0.5,
            ),
            (AquarelleParams::default(), 0.75),
        ];

        let clusters = vec![
            cluster([220, 60, 60], 0.3, 0.35, 0.8),
            cluster([60, 120, 220], 0.7, 0.55, 0.5),
            cluster([200, 200, 80], 0.5, 0.8, 0.4),
        ];

        // Per-case oracle: render each alone on a fresh renderer first.
        let mut oracles = Vec::new();
        for &(params, t) in &cases {
            let opts = aquarelle_opts(96, 72, params);
            let Some(fresh) = require_or_skip_fresh_renderer(
                "aquarelle_shared_gpu_concurrent_render (oracle leg)",
            ) else {
                return;
            };
            oracles.push(fresh.render_frame_aquarelle(&clusters, &opts, t));
        }

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (idx, &(params, t)) in cases.iter().enumerate() {
                let oracle = &oracles[idx];
                let clusters = &clusters;
                handles.push(scope.spawn(move || {
                    let opts = aquarelle_opts(96, 72, params);
                    // A few iterations per thread to maximize overlap on the shared
                    // aquarelle texture / per-size resources.
                    for _ in 0..3 {
                        let img = renderer.render_frame_aquarelle(clusters, &opts, t);
                        assert_eq!(
                            oracle.as_raw(),
                            img.as_raw(),
                            "concurrent aquarelle (case {idx}, t={t}) must be byte-identical to its solo render"
                        );
                    }
                }));
            }
            for h in handles {
                h.join()
                    .expect("concurrent aquarelle render thread panicked");
            }
        });
        eprintln!("concurrent aquarelle render: all threads matched their solo oracle");
    }

    /// A non-Aquarelle shape passed to `render_frame_aquarelle` must fall back to the
    /// Circle path (the call is total), matching the Glyph-entry fallback contract.
    #[test]
    fn aquarelle_entry_circle_shape_falls_back() {
        let Some(renderer) = require_or_skip_renderer("aquarelle_entry_circle_shape_falls_back")
        else {
            return;
        };
        let clusters = vec![cluster([200, 100, 50], 0.5, 0.5, 1.0)];
        let mut opts = aquarelle_opts(64, 64, AquarelleParams::default());
        opts.shape = OrbShape::Circle;
        // Should not panic and should produce the same image as the Circle path.
        let via_aqua = renderer.render_frame_aquarelle(&clusters, &opts, 0.0);
        let via_circle = renderer.render_frame(&clusters, &opts, 0.0);
        assert_eq!(
            via_aqua.as_raw(),
            via_circle.as_raw(),
            "non-aquarelle shape must fall back to the Circle path byte-for-byte"
        );
    }

    // --- #216 new coverage: RNG reproduction / offset / round boundaries / clamp /
    //     desaturation / count-ignore / GPU corner & branch parity ---

    /// Oracle satellite placement for one orb, mirroring `render_aquarelle_orb`'s
    /// **exact** ChaCha8 consumption order (offset θ → per satellite θ/dist/radius)
    /// so the test owns an independent reference the pack must match bit-for-bit.
    /// `seed` == orb index (the pack seeds `ChaCha8Rng::seed_from_u64(i)`).
    fn oracle_aquarelle_sats(
        seed: u64,
        center: (f32, f32),
        radius: f32,
        bleed: f32,
    ) -> (f32, Vec<(f32, f32, f32)>) {
        use std::f32::consts::TAU;
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        // 1) offset angle is the first draw (consumed even when offset==0).
        let theta: f32 = rng.gen_range(0.0..TAU);
        // 2) satellites, in θ → dist → radius-factor order, `round(3*bleed)` of them.
        let bleed_count = (3.0 * bleed).round() as u32;
        let mut sats = Vec::new();
        for _ in 0..bleed_count.min(3) {
            let bleed_theta: f32 = rng.gen_range(0.0..TAU);
            let bleed_dist = radius * rng.gen_range(0.4..0.9);
            let bx = center.0 + bleed_dist * bleed_theta.cos();
            let by = center.1 + bleed_dist * bleed_theta.sin();
            let bleed_radius = radius * rng.gen_range(0.2..0.4) * (0.5 + 0.5 * bleed);
            sats.push((bx, by, bleed_radius));
        }
        (theta, sats)
    }

    /// (#1, most important) The packed satellite centers/radii must be **bit-identical**
    /// to an independent ChaCha8 oracle consuming the RNG in `render_aquarelle_orb`'s
    /// exact order. Guards the RNG-reproduction contract the whole WGSL path rests on.
    #[test]
    fn aquarelle_pack_satellite_positions_match_crate() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this RNG-reproduction test runs on every host, not just GPU CI.
        let (w, h, base_radius_unit) = (200.0_f32, 200.0_f32, 40.0_f32);
        // weight 1.0 ⇒ radius == base_radius_unit; center at (0.5,0.5) ⇒ (100,100).
        let single = vec![cluster([200, 120, 60], 0.5, 0.5, 1.0)];
        let radius = base_radius_unit * 1.0_f32.sqrt();
        let center = (0.5_f32.clamp(0.0, 1.0) * w, 0.5_f32.clamp(0.0, 1.0) * h);

        // bleed=1.0 ⇒ 3 satellites; saturation 1.0 keeps colors untouched.
        let params = AquarelleParams {
            bleed: 1.0,
            bloom: 0.5,
            offset: 0.5,
            halo: 0.5,
        };
        let orbs = GpuRenderer::pack_aquarelle_orbs(
            &single,
            w,
            h,
            base_radius_unit,
            1.0,
            params.clamped(),
        );
        assert_eq!(orbs.len(), 1);
        assert_eq!(
            orbs[0].main[3] as u32, 3,
            "bleed=1.0 must pack 3 satellites"
        );

        let (_, sats) = oracle_aquarelle_sats(0, center, radius, 1.0);
        assert_eq!(sats.len(), 3);
        let packed = [orbs[0].sat0, orbs[0].sat1, orbs[0].sat2];
        for (i, ((ox, oy, or), p)) in sats.iter().zip(packed.iter()).enumerate() {
            assert_eq!(
                p[0], *ox,
                "sat{i} cx must bit-match oracle (packed={}, oracle={ox})",
                p[0]
            );
            assert_eq!(
                p[1], *oy,
                "sat{i} cy must bit-match oracle (packed={}, oracle={oy})",
                p[1]
            );
            assert_eq!(
                p[2], *or,
                "sat{i} radius must bit-match oracle (packed={}, oracle={or})",
                p[2]
            );
        }
    }

    /// (#2) offset direction. With `offset=1.0` the packed main center must equal
    /// `center + radius*0.25*(cosθ,sinθ)` for the seed's first `gen_range(0..TAU)`;
    /// with `offset=0.0` the offset distance is 0 so the main center is the geometric
    /// center *exactly* (the θ draw is still consumed but multiplied by 0).
    #[test]
    fn aquarelle_pack_offset_direction_matches_seed_angle() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this RNG-reproduction test runs on every host, not just GPU CI.
        let (w, h, base_radius_unit) = (200.0_f32, 200.0_f32, 40.0_f32);
        let single = vec![cluster([200, 120, 60], 0.5, 0.5, 1.0)];
        let radius = base_radius_unit;
        let center = (100.0_f32, 100.0_f32);
        let (theta, _) = oracle_aquarelle_sats(0, center, radius, 0.0);

        // offset == 1.0 ⇒ center shifted radius*0.25 along θ.
        let p1 = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 1.0,
            halo: 0.0,
        };
        let orbs1 =
            GpuRenderer::pack_aquarelle_orbs(&single, w, h, base_radius_unit, 1.0, p1.clamped());
        let exp_cx = center.0 + radius * 0.25 * theta.cos();
        let exp_cy = center.1 + radius * 0.25 * theta.sin();
        assert_eq!(
            orbs1[0].main[0], exp_cx,
            "offset=1.0 main cx must follow seed θ"
        );
        assert_eq!(
            orbs1[0].main[1], exp_cy,
            "offset=1.0 main cy must follow seed θ"
        );

        // offset == 0.0 ⇒ exact geometric center (no shift).
        let p0 = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        };
        let orbs0 =
            GpuRenderer::pack_aquarelle_orbs(&single, w, h, base_radius_unit, 1.0, p0.clamped());
        assert_eq!(
            orbs0[0].main[0], center.0,
            "offset=0.0 main cx must be exact center"
        );
        assert_eq!(
            orbs0[0].main[1], center.1,
            "offset=0.0 main cy must be exact center"
        );
    }

    /// (#3) `round(3*bleed)` boundaries (round-half-away-from-zero). The existing test
    /// only covers 0.0/0.5/1.0; this nails the three rounding edges so a `.round()` →
    /// `.floor()`/`.trunc()` regression in the satellite count is caught.
    #[test]
    fn aquarelle_pack_bleed_count_round_boundaries() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this rounding-edge test runs on every host, not just GPU CI.
        let single = vec![cluster([200, 120, 60], 0.5, 0.5, 1.0)];
        // (bleed, expected sat_count): just below/above each .5 boundary.
        for (bleed, expected) in [
            (0.16_f32, 0u32), // 0.48 → 0
            (0.17, 1),        // 0.51 → 1
            (0.49, 1),        // 1.47 → 1
            (0.50, 2),        // 1.50 → 2 (half away from zero)
            (0.83, 2),        // 2.49 → 2
            (0.84, 3),        // 2.52 → 3
        ] {
            let params = AquarelleParams {
                bleed,
                bloom: 0.0,
                offset: 0.0,
                halo: 0.0,
            };
            let orbs = GpuRenderer::pack_aquarelle_orbs(
                &single,
                200.0,
                200.0,
                40.0,
                1.0,
                params.clamped(),
            );
            assert_eq!(
                orbs[0].main[3] as u32, expected,
                "bleed={bleed} ⇒ round(3*bleed) must be {expected}"
            );
        }
    }

    /// (#4) bloom guard. A tiny positive bloom (`1e-4`) must still set the bloom flag
    /// and produce a positive core radius (the inner `core_radius > 0.0` guard must not
    /// reject it); bloom `0.0` must clear the flag and pack a zero core radius.
    #[test]
    fn aquarelle_pack_bloom_tiny_positive_keeps_core_positive() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this bloom-guard test runs on every host, not just GPU CI.
        let single = vec![cluster([200, 120, 60], 0.5, 0.5, 1.0)];

        let tiny = AquarelleParams {
            bleed: 0.0,
            bloom: 1e-4,
            offset: 0.0,
            halo: 0.0,
        };
        let orbs =
            GpuRenderer::pack_aquarelle_orbs(&single, 200.0, 200.0, 40.0, 1.0, tiny.clamped());
        assert!(
            orbs[0].inner[3] > 0.5,
            "bloom=1e-4 (>0) must set bloom_flag"
        );
        assert!(
            orbs[0].bloom_geom[2] > 0.0,
            "bloom=1e-4 must keep core radius positive (got {})",
            orbs[0].bloom_geom[2]
        );

        let zero = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        };
        let orbs0 =
            GpuRenderer::pack_aquarelle_orbs(&single, 200.0, 200.0, 40.0, 1.0, zero.clamped());
        assert!(orbs0[0].inner[3] < 0.5, "bloom=0 must clear bloom_flag");
        assert_eq!(
            orbs0[0].bloom_geom[2], 0.0,
            "bloom=0 must pack zero core radius"
        );
    }

    /// (#5) A zero-weight orb packs a fully zeroed row (radius 0, every layer slot 0),
    /// and a positive-weight orb mixed in the same call is unaffected — only the dead
    /// row collapses. Guards the early `radius <= 0.0` skip path and row independence.
    #[test]
    fn aquarelle_pack_zero_weight_orb_packs_skip_row() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this skip-row test runs on every host, not just GPU CI.
        // Orb 0: zero weight (dead). Orb 1: positive weight (real).
        let clusters = vec![
            cluster([200, 120, 60], 0.3, 0.3, 0.0),
            cluster([60, 120, 220], 0.7, 0.7, 1.0),
        ];
        let params = AquarelleParams::default();
        let orbs =
            GpuRenderer::pack_aquarelle_orbs(&clusters, 200.0, 200.0, 40.0, 1.0, params.clamped());
        assert_eq!(orbs.len(), 2);

        // Dead row: radius 0, sat_count 0, all layer slots zero.
        assert_eq!(orbs[0].main[2], 0.0, "zero-weight orb must pack radius 0");
        assert_eq!(
            orbs[0].main[3], 0.0,
            "zero-weight orb must pack 0 satellites"
        );
        for slot in [
            orbs[0].inner,
            orbs[0].halo,
            orbs[0].bloom_geom,
            orbs[0].bloom_col,
            orbs[0].bleed_col,
            orbs[0].sat0,
            orbs[0].sat1,
            orbs[0].sat2,
        ] {
            assert_eq!(slot, [0.0; 4], "zero-weight orb layer slots must be zeroed");
        }

        // Live row: positive radius (the dead neighbor did not poison it).
        assert!(
            orbs[1].main[2] > 0.0,
            "positive-weight orb must have radius > 0"
        );
    }

    /// (#6) Out-of-range slider values are clamped (the pack calls `params.clamped()`):
    /// `bleed=5.0` must still cap at 3 satellites, and a negative `bloom=-1.0` must
    /// clear the bloom flag (clamped to 0) rather than wrapping to a huge core.
    #[test]
    fn aquarelle_pack_clamps_out_of_range_params() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this clamp test runs on every host, not just GPU CI.
        let single = vec![cluster([200, 120, 60], 0.5, 0.5, 1.0)];
        let wild = AquarelleParams {
            bleed: 5.0,
            bloom: -1.0,
            offset: 9.0,
            halo: -3.0,
        };
        // The renderer clamps internally; pass the raw params (matching production:
        // `pack_aquarelle_orbs` calls `params.clamped()`).
        let orbs = GpuRenderer::pack_aquarelle_orbs(&single, 200.0, 200.0, 40.0, 1.0, wild);
        assert!(
            (orbs[0].main[3] as u32) <= 3,
            "bleed=5.0 must clamp to ≤3 satellites (got {})",
            orbs[0].main[3]
        );
        assert!(
            orbs[0].inner[3] < 0.5,
            "negative bloom must clamp to 0 and clear the bloom flag"
        );
        assert_eq!(
            orbs[0].bloom_geom[2], 0.0,
            "negative bloom must pack a zero core radius (clamped)"
        );
    }

    /// (#7) `saturation=0.0` desaturates every packed layer color: each must equal the
    /// grayscale `adjust_saturation_pub(base, 0.0)` (boosting a gray stays gray, mixing
    /// a gray toward white stays gray), so the GPU never sees a saturated color the CPU
    /// oracle desaturated.
    #[test]
    fn aquarelle_pack_saturation_zero_desaturates_layer_colors() {
        // Pack-only: `pack_aquarelle_orbs` is an associated fn (no GPU adapter
        // needed), so this saturation test runs on every host, not just GPU CI.
        let base = [200u8, 120, 60];
        let single = vec![cluster(base, 0.5, 0.5, 1.0)];
        // bleed=1.0 ⇒ satellites exist; bloom=0.5 ⇒ bloom color packed; halo=0.5.
        let params = AquarelleParams {
            bleed: 1.0,
            bloom: 0.5,
            offset: 0.5,
            halo: 0.5,
        };
        let orbs =
            GpuRenderer::pack_aquarelle_orbs(&single, 200.0, 200.0, 40.0, 0.0, params.clamped());

        // The pack desaturates the source first (saturation 0.0 ⇒ grayscale), then
        // boost_saturation/mix_with_white operate on that gray. A boost of an
        // already-gray color stays gray; a white-mix of gray stays gray. So every
        // packed layer's three channels must be equal (chroma == 0).
        let gray = adjust_saturation_pub(base, 0.0);
        assert_eq!(gray[0], gray[1], "desaturated base must be gray");
        assert_eq!(gray[1], gray[2], "desaturated base must be gray");

        let is_gray = |rgb: [f32; 4]| -> bool {
            let to8 = |c: f32| (c * 255.0).round() as i32;
            (to8(rgb[0]) - to8(rgb[1])).abs() <= 1 && (to8(rgb[1]) - to8(rgb[2])).abs() <= 1
        };
        assert!(
            is_gray(orbs[0].inner),
            "inner color must be gray at saturation 0"
        );
        assert!(
            is_gray(orbs[0].halo),
            "halo color must be gray at saturation 0"
        );
        assert!(
            is_gray(orbs[0].bleed_col),
            "bleed color must be gray at saturation 0"
        );
        assert!(
            is_gray(orbs[0].bloom_col),
            "bloom color must be gray at saturation 0"
        );
        // And the inner color is exactly the desaturated base (round-trip via u8).
        let inner8 = [
            (orbs[0].inner[0] * 255.0).round() as u8,
            (orbs[0].inner[1] * 255.0).round() as u8,
            (orbs[0].inner[2] * 255.0).round() as u8,
        ];
        assert_eq!(
            inner8, gray,
            "inner layer must be the desaturated base color"
        );
    }





}
