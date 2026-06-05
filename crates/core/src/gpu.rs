//! wgpu (Rust + WGSL) production render path — orber #207 Phase 0–1c, #225, #229,
//! #235.
//!
//! [`GpuRenderer`] is the headless renderer and — since #225 — the **only**
//! renderer (the CPU pixel path and the CPU↔GPU parity oracle were purged). Since
//! #235 the orb mechanism is the **only** mechanism for orb / glyph / image: one
//! unified WGSL template ([`orb.wgsl`](../src/orb.wgsl)) is compiled into two
//! variants — the plain orb (analytic circle distance, byte-exact with the old
//! `orb_circle.wgsl`) and the SDF orb (glyph / image: the same orb math fed a
//! different silhouette via an SDF sample; no bleed/halo). Aquarelle keeps its own
//! WGSL ([`orb_aquarelle.wgsl`](../src/orb_aquarelle.wgsl)). All four shapes (Orb,
//! Glyph, Image, Aquarelle) render on the GPU; the CLI renders every PNG / video /
//! variation frame through it.
//!
//! ## Output paths (#229)
//!
//! - **Read-back (native only)**: `render_frame*` / `render_packed` draw into a
//!   cached offscreen `Rgba8Unorm` target and read it back into an `RgbaImage`
//!   (blocking `device.poll`). This is the CLI path; it does not exist on wasm32
//!   (no blocking executor / buffer mapping is async there).
//! - **to_view**: the `*_to_view` variants draw the same frame into an
//!   **externally supplied** `wgpu::TextureView` + `wgpu::TextureFormat` and
//!   submit, without read-back. This is the browser WebGPU present seam: the
//!   caller (orber-wasm, #230) owns the canvas surface — creation, configure,
//!   per-frame acquire and present — so core never touches web-sys. Pipelines are
//!   cached per `(shader, target format)`; both the orb and SDF variants draw a
//!   single pass straight into the caller's view (surfaces are typically
//!   `Bgra8Unorm`). Two caller contracts: `format` must be **non-sRGB**
//!   (`Bgra8Unorm`, not `Bgra8UnormSrgb`) — the shaders emit already-sRGB-encoded
//!   values and write them raw, so an sRGB format would apply the encoding twice
//!   (guarded by a `debug_assert`) — and the `view` must match the requested
//!   width × height (a smaller / larger view silently smears or clips). Construction
//!   on wasm uses the async [`GpuRenderer::new_async`] (the sync
//!   [`GpuRenderer::new`] wraps it in `pollster::block_on`, native only).
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

#[cfg(not(target_arch = "wasm32"))]
use image::RgbaImage;
use wgpu::util::DeviceExt;

use crate::animate::{pack_render_data_for_webgl, AnimateOptions, MotionDirection, MAX_ORB_COUNT};
use crate::cluster::Cluster;
use crate::orb::adjust_saturation_pub;

use palette::{FromColor, Hsl, IntoColor, Srgb};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Bytes per pixel for `Rgba8Unorm` (read-back path; native only).
#[cfg(not(target_arch = "wasm32"))]
const BYTES_PER_PIXEL: u32 = 4;

/// Upper bound of the radius breath factor (`1.0 + 0.10`), mirroring
/// `animate::BREATH_RADIUS_MAX_FACTOR` / the WGSL constant. Used only to size the
/// glyph SDF for the frame from the largest possible orb radius.
const BREATH_RADIUS_MAX_FACTOR: f32 = 1.10;
/// wgpu requires `bytes_per_row` of a texture→buffer copy to be a multiple of
/// this (`COPY_BYTES_PER_ROW_ALIGNMENT`). This applies to the read-back
/// (texture→buffer) only — `write_texture` (buffer/CPU→texture) is exempt, so the
/// orb data-texture upload uses its tight 48-byte rows directly.
#[cfg(not(target_arch = "wasm32"))]
const ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// Width, in texels, of the per-orb data-texture: one texel each for the color,
/// phase, misc, and rotation `vec4`s (see `orb.wgsl`'s `load_orb`). Widened 3→4 in
/// Phase 1b (#212) so the SDF orb variant can read the per-orb rotation
/// (`base_angle`, `rot_speed_signed`); the plain orb variant ignores texel x=3
/// (its `load_orb` does not even read it) and stays bit-exact.
const ORB_TEX_WIDTH: u32 = 4;
/// Bytes per texel of the `Rgba32Float` orb data-texture (4 × f32).
const ORB_TEX_BYTES_PER_TEXEL: u32 = 16;
/// Bytes per row of the orb data-texture (`4 × 16 = 64`). `write_texture` has no
/// row-alignment requirement, so this tight value is used as-is.
const ORB_TEX_BYTES_PER_ROW: u32 = ORB_TEX_WIDTH * ORB_TEX_BYTES_PER_TEXEL;

/// Width, in texels, of the per-orb **aquarelle** data-texture (#216). Nine texels
/// per orb hold the four `render_aquarelle_orb` layers' geometry + u8 colors; see
/// `orb_aquarelle.wgsl`'s header for the slot map. Independent of the orb / SDF
/// `ORB_TEX_WIDTH` (4) — aquarelle binds its own texture so the two never alias.
const AQUARELLE_TEX_WIDTH: u32 = 9;
/// Bytes per row of the aquarelle data-texture (`9 × 16 = 144`). `write_texture` is
/// exempt from row-alignment, so this tight value is used directly.
const AQUARELLE_TEX_BYTES_PER_ROW: u32 = AQUARELLE_TEX_WIDTH * ORB_TEX_BYTES_PER_TEXEL;

/// Header words / per-orb words in the `pack_render_data_for_webgl` layout.
/// Kept in sync with that function (header 16 words, per-orb 16 words).
const HEADER_WORDS: usize = 16;
const PER_ORB_WORDS: usize = 16;

/// The unified orb WGSL template (`orb.wgsl`, #235). The orb mechanism is now the
/// **only** mechanism for orb / glyph / image: each pixel's normalized "distance
/// from the shape" feeds the same 3-axis breath, `falloff_curve`, and Skia-lowp
/// premultiply compositing. Only the **DISTANCE SOURCE** block differs per shape,
/// so "feeding the orb a different silhouette" is the literal implementation.
///
/// The template carries `//!ORB_*` markers that [`orb_wgsl`] / [`orb_sdf_wgsl`]
/// substitute to generate the two variants (Rust-side string composition, as the
/// plan recommends): the orb variant injects the analytic circle distance and
/// **no** SDF bindings, so its compiled shader is byte-for-byte the old
/// `orb_circle.wgsl` body (the orb output stays bit-exact); the SDF variant adds
/// the `R8Unorm` SDF texture + bilinear sampler (bindings 2/3), reads the per-orb
/// rotation texel (x=3), and computes `r` from the SDF sample.
const ORB_WGSL_TEMPLATE: &str = include_str!("orb.wgsl");

/// The orb (analytic circle) variant of [`ORB_WGSL_TEMPLATE`]. No SDF bindings,
/// no rotation; the DISTANCE SOURCE block inlines the same two lines the old
/// `orb_circle.wgsl` had (`dist = distance(...); r = dist / radius`), so the
/// compiled shader is identical and the orb output is unchanged (byte-exact).
/// Built once (`OnceLock`, MSRV 1.78 — `LazyLock` is 1.80); the resulting
/// `&'static str` doubles as the stable pipeline-cache key.
fn orb_wgsl() -> &'static str {
    use std::sync::OnceLock;
    static ORB: OnceLock<String> = OnceLock::new();
    ORB.get_or_init(|| {
        ORB_WGSL_TEMPLATE
            .replace("//!ORB_EXTRA_BINDINGS", ORB_EXTRA_BINDINGS_NONE)
            .replace("//!ORB_LOAD", ORB_LOAD_ORB)
            .replace("//!ORB_HELPERS", ORB_HELPERS_NONE)
            .replace("//!ORB_DISTANCE_SOURCE", ORB_DISTANCE_SOURCE_CIRCLE)
    })
}

/// The SDF (glyph / image, #235) variant of [`ORB_WGSL_TEMPLATE`]. Adds the
/// `R8Unorm` SDF texture + bilinear sampler (bindings 2/3) and the rotation
/// helper, reads the rotation texel, and computes `r` from the SDF sample
/// (rotation applied **before** sampling; `CONTENT_SPAN` clip + `sdf_size` texel
/// remap preserved). The DISTANCE SOURCE is the only difference from the orb
/// variant; the falloff / breath / compositing are the **same** orb math, so
/// glyph / image now blur exactly like orb (no bleed/halo). Built once (`OnceLock`).
fn orb_sdf_wgsl() -> &'static str {
    use std::sync::OnceLock;
    static SDF: OnceLock<String> = OnceLock::new();
    SDF.get_or_init(|| {
        ORB_WGSL_TEMPLATE
            .replace("//!ORB_EXTRA_BINDINGS", ORB_EXTRA_BINDINGS_SDF)
            .replace("//!ORB_LOAD", ORB_LOAD_ORB_WITH_ROT)
            .replace("//!ORB_HELPERS", ORB_HELPERS_SDF)
            .replace("//!ORB_DISTANCE_SOURCE", ORB_DISTANCE_SOURCE_SDF)
    })
}

/// No extra bindings (orb variant): the analytic circle needs only the params
/// uniform (0) and the orb data-texture (1) the template already declares.
const ORB_EXTRA_BINDINGS_NONE: &str = "";

/// SDF variant bindings: the glyph / image SDF (`R8Unorm`, bilinear-filterable)
/// at binding 2 and a filtering sampler at binding 3.
const ORB_EXTRA_BINDINGS_SDF: &str = "\
// glyph/image SDF（R8Unorm, 単一文字 / 単一シルエット）と bilinear sampler。\n\
@group(0) @binding(2) var sdf_tex: texture_2d<f32>;\n\
@group(0) @binding(3) var sdf_samp: sampler;";

/// Orb variant `load_orb`: reads only color / phase / misc (3 texels), exactly as
/// the old `orb_circle.wgsl` did (no `rot` read), keeping the orb shader identical.
const ORB_LOAD_ORB: &str = "\
struct Orb {\n\
    color: vec4<f32>, // (r, g, b, weight)\n\
    phase: vec4<f32>, // (phase, phi_radius, phi_blur, phi_opacity)\n\
    misc: vec4<f32>,  // (cross_axis, style_bit, speed_mult, _)\n\
};\n\
\n\
fn load_orb(i: u32) -> Orb {\n\
    let row = i32(i);\n\
    var o: Orb;\n\
    o.color = textureLoad(orb_tex, vec2<i32>(0, row), 0);\n\
    o.phase = textureLoad(orb_tex, vec2<i32>(1, row), 0);\n\
    o.misc = textureLoad(orb_tex, vec2<i32>(2, row), 0);\n\
    return o;\n\
}";

/// SDF variant `load_orb`: the orb fields plus the rotation texel (x=3) the SDF
/// DISTANCE SOURCE rotates by (`base_angle`, `rot_speed_signed`).
const ORB_LOAD_ORB_WITH_ROT: &str = "\
struct Orb {\n\
    color: vec4<f32>, // (r, g, b, weight)\n\
    phase: vec4<f32>, // (phase, phi_radius, phi_blur, phi_opacity)\n\
    misc: vec4<f32>,  // (cross_axis, style_bit, speed_mult, _)\n\
    rot: vec4<f32>,   // (base_angle, rot_speed_signed, _, _)\n\
};\n\
\n\
fn load_orb(i: u32) -> Orb {\n\
    let row = i32(i);\n\
    var o: Orb;\n\
    o.color = textureLoad(orb_tex, vec2<i32>(0, row), 0);\n\
    o.phase = textureLoad(orb_tex, vec2<i32>(1, row), 0);\n\
    o.misc = textureLoad(orb_tex, vec2<i32>(2, row), 0);\n\
    o.rot = textureLoad(orb_tex, vec2<i32>(3, row), 0);\n\
    return o;\n\
}";

/// No extra helpers (orb variant).
const ORB_HELPERS_NONE: &str = "";

/// SDF variant helper: `glyph_rotation_angle`, identical to
/// `crate::animate::glyph_rotation_angle` (rotation applied before SDF sampling).
const ORB_HELPERS_SDF: &str = "\
// `crate::animate::glyph_rotation_angle`(cycle, t, base_angle, rot_speed_signed, glyph_rotate)\n\
// と同式。glyph_rotate=false → base_angle 静止。それ以外 → base_angle +\n\
// rem_euclid(cycle * rot_speed_signed * t, 1.0) * TAU。cycle * rot_speed_signed が整数\n\
// なので t=1 で turns=0 に閉じる（loop closure）。\n\
fn glyph_rotation_angle(base_angle: f32, rot_speed_signed: f32) -> f32 {\n\
    if (params.glyph_rotate < 0.5) {\n\
        return base_angle;\n\
    }\n\
    let x = params.cycle * rot_speed_signed * params.t;\n\
    let turns = x - floor(x);\n\
    return base_angle + turns * TAU;\n\
}";

/// Orb DISTANCE SOURCE: the analytic circle distance, inlined into the loop body
/// **verbatim** from the old `orb_circle.wgsl` (so the orb shader is byte-exact).
/// Defines `r` for the shared `falloff_curve` downstream; no rotation, no discard.
const ORB_DISTANCE_SOURCE_CIRCLE: &str = "\
        let dist = distance(sample_px, vec2<f32>(cx, cy));\n\
        let r = dist / radius;";

/// SDF DISTANCE SOURCE (glyph / image, #235): rotate the offset by the per-orb
/// angle **before** sampling, map to the SDF UV (CONTENT_SPAN clip + the
/// `sdf_size` texel remap that cancels the sampler's half-texel offset), bilinear
/// sample the signed distance, and convert to `r = 1 - signed_unit`. Out-of-range
/// UVs `continue` (the orb does not cover this pixel). `r` then feeds the **same**
/// `falloff_curve` / compositing the orb uses — the form is the only difference.
const ORB_DISTANCE_SOURCE_SDF: &str = "\
        // 座標変換: orb 中心からの差分を +angle で回し、(2*radius) で割って\n\
        // CONTENT_SPAN を掛けて 0.5 中心の UV にする（回転は SDF サンプル前）。\n\
        let angle = glyph_rotation_angle(o.rot.x, o.rot.y);\n\
        let cos_a = cos(angle);\n\
        let sin_a = sin(angle);\n\
        let dx = sample_px.x - cx;\n\
        let dy = sample_px.y - cy;\n\
        let rx = cos_a * dx - sin_a * dy;\n\
        let ry = sin_a * dx + cos_a * dy;\n\
        let u = rx / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;\n\
        let v = ry / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;\n\
        if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) {\n\
            continue;\n\
        }\n\
        // SDF bilinear サンプルの規約に合わせる: coord = clamp(u,0,1)*(size-1) の格子点を\n\
        // 線形補間。GPU sampler は uv*size-0.5 の texel 空間で補間するので、\n\
        //   uv_gpu = (u*(size-1) + 0.5) / size\n\
        // と remap すると同じ格子点・同じ重みで補間し、半 texel ずれを消せる。\n\
        let s = params.sdf_size;\n\
        let uu = (clampf(u, 0.0, 1.0) * (s - 1.0) + 0.5) / s;\n\
        let vv = (clampf(v, 0.0, 1.0) * (s - 1.0) + 0.5) / s;\n\
        // bilinear sample（sampler が線形補間）。R8Unorm なので .r に SDF が入る。\n\
        let sdf01 = textureSampleLevel(sdf_tex, sdf_samp, vec2<f32>(uu, vv), 0.0).r;\n\
        let signed_unit = sdf01 * 2.0 - 1.0;\n\
        let r = 1.0 - signed_unit;";

/// The Aquarelle orb WGSL (#216 Phase 1c). A dedicated pipeline + data-texture
/// (separate from the orb data-texture) that evaluates the four
/// `aquarelle::render_aquarelle_orb` layers analytically: offset main 3-stop
/// radial, 0..3 bleed satellites, and the bloom core, composited SourceOver in
/// the same u8-quantize → premultiply → source_over lowp流儀 as `orb.wgsl`.
/// The ChaCha8 RNG / HSL color math is **not** ported to WGSL; `pack_aquarelle_orbs`
/// runs it on the CPU (bit-identical to the crate) and uploads the resulting
/// centers / radii / u8 colors.
fn orb_aquarelle_wgsl() -> &'static str {
    include_str!("orb_aquarelle.wgsl")
}

/// Header uniform block handed to the orb / SDF shader. Mirrors `struct Params` in
/// `orb.wgsl`. `#[repr(C)]` + explicit padding to satisfy WGSL std140-ish
/// uniform layout (vec2 then scalars packed into 16-byte rows). The same struct
/// drives both variants (the orb variant simply ignores the SDF-only slots).
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
    // row 3: alpha_mul, glyph_rotate, edge_softness, sdf_size
    alpha_mul: f32,
    /// Per-orb rotation toggle (#136): `1.0` = animate per-orb rotation, `0.0` =
    /// hold `base_angle`. Read only by the SDF variant (glyph / image); the plain
    /// orb variant never references it. `0.0` is harmless for the orb path.
    glyph_rotate: f32,
    /// Edge softness (#205): reserved (currently unread by either variant); kept so
    /// the header layout mirrors the WebGL one.
    edge_softness: f32,
    /// SDF square side in texels. The SDF variant uses it for the SDF bilinear
    /// sampling convention (`coord = u*(size-1)`) when remapping UVs to the wgpu
    /// sampler's texel space. The orb variant ignores this slot; `0.0` for the orb
    /// path.
    sdf_size: f32,
}

/// One orb as the shaders see it: four `vec4`s mirroring `struct Orb` in
/// `orb.wgsl` (color+weight, phase quartet, misc, rotation). Filled from the
/// `pack_render_data_for_webgl` per-orb words. One `GpuOrb` packs to one row of
/// the `Rgba32Float` orb data-texture (4 texels = 64 bytes); the shader reads it
/// back with `textureLoad`s.
///
/// The plain orb variant's `load_orb` reads only `color` / `phase` / `misc`
/// (texels x=0..2) and never touches `rot` (x=3), so widening the row to 4 texels
/// leaves the orb output bit-exact. The SDF variant additionally reads
/// `rot = (base_angle, rot_speed_signed, _, _)` for #136 rotation.
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
/// to 16-byte rows. Separate from the orb [`Params`] because the aquarelle pack
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

/// A render pipeline plus its bind-group layout, compiled once per distinct
/// `(shader source, target format)` key (#229: the to_view path made the target
/// format vary, so it joined the cache key). Caching keeps shader compilation /
/// pipeline creation off the per-frame path: a long video renders the same
/// shader for every frame.
struct CachedPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

/// Per-dimension GPU resources reused across same-sized frames: the render
/// target and the padded read-back buffer. Reallocating these every frame is the
/// other half of the per-frame cost the cache removes. Read-back path only, so
/// native only (#229) — the wasm32/to_view path draws into an externally supplied
/// view and never allocates an offscreen target or read-back buffer.
#[cfg(not(target_arch = "wasm32"))]
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
/// (unlike `Rgba32Float`), so the SDF orb shader can read it with a real bilinear
/// `sampler` and stay portable to Phase 2 (#212).
struct GlyphSdfTexture {
    view: wgpu::TextureView,
}

/// The SDF binding passed into `render_packed_inner` for the glyph / image path:
/// the (cached) SDF texture view and its square side in texels. `None` selects
/// the orb (analytic circle) path instead.
struct GlyphBindings<'a> {
    sdf_view: &'a wgpu::TextureView,
    size: u32,
}

/// One prepared SDF (glyph / image, #229) frame, shared by the read-back and
/// to_view paths: the pack buffer plus the uploaded SDF binding, or one of the
/// two degenerate outcomes the SDF entry points must keep handling.
enum SdfFramePack {
    /// `opts.shape` was not the expected SDF shape: the caller falls back to the
    /// orb path so the call stays total.
    NotSdfShape,
    /// No drawable SDF (radius 0 / unknown char / empty SDF): a zero-orb pack
    /// that renders the background only through the orb pipeline (the same single
    /// pass) — the "draw nothing for tofu" contract.
    BackgroundOnly(Vec<f32>),
    /// A drawable frame: the pack plus the (cached) uploaded SDF texture view
    /// and its square side in texels, to bind on the SDF orb pipeline.
    Sdf {
        pack: Vec<f32>,
        sdf_view: wgpu::TextureView,
        size: u32,
    },
}

/// Headless wgpu renderer for the unified orb path (orb / glyph / image / #235).
/// Holds a device/queue plus a per-shader pipeline cache and a per-size resource
/// cache, so a multi-frame render (a long `--duration-ms` video) compiles the
/// shader and allocates the target/read-back buffer only once instead of every
/// frame.
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
    /// Orb pipelines, keyed by `(shader source, target format)`. The format is
    /// part of the key since #229: the native read-back path always targets
    /// `Rgba8Unorm`, while the to_view (surface present) path targets whatever
    /// format the caller's view has (browser surfaces are typically `Bgra8Unorm`).
    pipeline_cache: std::sync::Mutex<HashMap<(String, wgpu::TextureFormat), CachedPipeline>>,
    /// Per-size resources, keyed by `(width, height)`. Read-back path only
    /// (native; the wasm32/to_view path never allocates these).
    #[cfg(not(target_arch = "wasm32"))]
    sized_cache: std::sync::Mutex<HashMap<(u32, u32), SizedResources>>,
    /// The grow-only per-orb data-texture (reallocated only when a frame needs
    /// more rows than the cached capacity). `None` until the first frame.
    orb_texture: std::sync::Mutex<Option<OrbTexture>>,
    /// Glyph SDF textures keyed by `(char as u32, size)`. Grow-only (never
    /// evicts): a clip renders one glyph at one size, so this holds a single
    /// entry; supporting several glyphs in one clip just adds entries.
    glyph_sdf_cache: std::sync::Mutex<HashMap<(u32, u32), GlyphSdfTexture>>,
    /// The bilinear (linear/linear, clamp-to-edge) sampler the SDF orb shader uses
    /// to read the `R8Unorm` SDF. Built once; reused for every glyph / image frame.
    glyph_sampler: wgpu::Sampler,
    /// The grow-only per-orb **aquarelle** data-texture (#216), reallocated only
    /// when a frame needs more rows than the cached capacity. `None` until the first
    /// aquarelle frame (orb/glyph/image-only runs never allocate it). Separate from
    /// `orb_texture` so the 9-texel aquarelle layout never aliases the 4-texel
    /// orb/glyph/image one, keeping the orb output bit-exact.
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
    /// Bring up a headless GPU context (no surface), blocking on
    /// [`Self::new_async`]. Returns `None` when no adapter is available (e.g. CI
    /// without a GPU / software rasterizer). GPU is the only renderer (#225), so
    /// the CLI treats `None` as a fatal error and exits; tests treat it as skip.
    ///
    /// Native only: `pollster::block_on` cannot run on wasm32, where the caller
    /// must `await` [`Self::new_async`] directly (#229).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new() -> Option<Self> {
        pollster::block_on(Self::new_async())
    }

    /// Bring up a headless GPU context (no surface), async. This is the wasm32
    /// entry point (#229): the browser's `requestAdapter` / `requestDevice` are
    /// inherently async and there is no blocking executor on wasm. Native callers
    /// normally use the [`Self::new`] sync wrapper instead.
    ///
    /// Headless on purpose: no surface is created here even on wasm. The caller
    /// (orber-wasm, #230) owns the canvas surface — creation, configuration and
    /// present — and hands frames to this renderer via the `*_to_view` methods,
    /// keeping core free of any web-sys / canvas knowledge. A caller that needs a
    /// **surface-compatible** adapter (requested with `compatible_surface`) brings
    /// up the instance / adapter / device itself and uses
    /// [`Self::from_device_queue`] instead (#230).
    pub async fn new_async() -> Option<Self> {
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
        Some(Self::from_device_queue(device, queue, adapter_name))
    }

    /// Build a renderer around an **externally created** device / queue (#230).
    ///
    /// This is the surface-compatible construction seam for the browser WebGPU
    /// path: a canvas surface must be created from the same `wgpu::Instance` that
    /// requests the adapter (with `compatible_surface` set), so the caller
    /// (orber-wasm) owns the whole instance → surface → adapter → device bring-up
    /// and only hands the resulting device / queue here. Core stays free of any
    /// web-sys / canvas knowledge; rendering then goes through the `*_to_view`
    /// methods against the surface's frame views.
    ///
    /// `wgpu::Device` / `wgpu::Queue` are cheaply cloneable handles, so the caller
    /// can keep clones (e.g. for `Surface::configure`) while the renderer owns its
    /// own. [`Self::new_async`] routes through here, so both constructions share
    /// the same cache / sampler setup.
    pub fn from_device_queue(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_name: String,
    ) -> Self {
        // Bilinear, clamp-to-edge sampler for the glyph SDF. Clamp-to-edge gives the
        // neighbor clamp the SDF sampling convention expects (`x1 = (x0+1).min(size-1)`),
        // and linear min/mag gives the 2×2 lerp.
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
        Self {
            device,
            queue,
            adapter_name,
            pipeline_cache: std::sync::Mutex::new(HashMap::new()),
            #[cfg(not(target_arch = "wasm32"))]
            sized_cache: std::sync::Mutex::new(HashMap::new()),
            orb_texture: std::sync::Mutex::new(None),
            glyph_sdf_cache: std::sync::Mutex::new(HashMap::new()),
            glyph_sampler,
            aquarelle_texture: std::sync::Mutex::new(None),
            render_guard: std::sync::Mutex::new(()),
        }
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

    /// Get-or-build the orb pipeline for `(shader_wgsl, format)`, compiling the
    /// shader and pipeline only on first use. The build runs at most once per
    /// distinct `(shader source, target format)` key for the life of the renderer
    /// (#229: the read-back path always passes `Rgba8Unorm`; the to_view path
    /// passes the caller's view format).
    fn pipeline<R>(
        &self,
        shader_wgsl: &str,
        glyph: bool,
        format: wgpu::TextureFormat,
        f: impl FnOnce(&CachedPipeline) -> R,
    ) -> R {
        let mut cache = self
            .pipeline_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = cache
            .entry((shader_wgsl.to_owned(), format))
            .or_insert_with(|| self.build_pipeline(shader_wgsl, glyph, format));
        f(entry)
    }

    /// Compile a pipeline for `shader_wgsl` targeting `format`. The orb
    /// pipeline has binding 0 = `Params` uniform, binding 1 = orb data-texture
    /// (`Rgba32Float`, read via `textureLoad`, `filterable: false`). The SDF
    /// pipeline (`glyph = true`, glyph / image) additionally has binding 2 = the
    /// SDF (`R8Unorm`, `filterable: true`) and binding 3 = a filtering sampler, so
    /// the shader can bilinear-sample the SDF. The orb texture stays
    /// `textureLoad`-only either way, keeping the path portable to wgpu's WebGL2
    /// backend (#210/#212). `format` is the color-target format: `Rgba8Unorm` for
    /// the offscreen read-back path, the caller's view format for the to_view
    /// (surface present) path (#229).
    fn build_pipeline(
        &self,
        shader_wgsl: &str,
        glyph: bool,
        format: wgpu::TextureFormat,
    ) -> CachedPipeline {
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
    /// read-back buffer only on first use of a `(width, height)`. Read-back path
    /// only (native).
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    fn build_sized_resources(device: &wgpu::Device, width: u32, height: u32) -> SizedResources {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-orb-target"),
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
            label: Some("orber-orb-readback"),
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

    /// Render one plain orb frame at time `t` from `clusters` + `opts`.
    ///
    /// The per-orb data is computed by [`pack_render_data_for_webgl`] — the same
    /// arithmetic the WebGL path (`orberGl.ts`) uses — so the orb positions /
    /// radii / alphas match the web result. `opts.width` / `opts.height` give the
    /// output size; `t` is clamped to `0.0..=1.0`.
    ///
    /// # Orb count
    ///
    /// The resolved orb count is clamped only to [`MAX_ORB_COUNT`] (1024). Per-orb
    /// data is uploaded as a data-texture that grows to fit, so there is no 64-orb
    /// cap and the GPU renders any count up to 1024 directly (#210 Phase 1a).
    ///
    /// # Scope
    ///
    /// This is the **plain orb** path only. The shape in `opts.shape` is ignored
    /// here; the caller routes `Glyph` to `render_frame_glyph`, `Image` to
    /// `render_frame_image`, and `Aquarelle` to `render_frame_aquarelle` (all GPU).
    /// GPU is the only renderer (#225) — no CPU fallback exists; when no adapter is
    /// available, [`GpuRenderer::new`] returns `None` and the CLI exits with an
    /// error. See the module docs.
    ///
    /// Native only (#229): this returns an [`RgbaImage`] via the GPU→CPU
    /// read-back, which needs a blocking `device.poll` that wasm32 does not have.
    /// The wasm/browser path uses [`Self::render_frame_to_view`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_frame(&self, clusters: &[Cluster], opts: &AnimateOptions, t: f32) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        let pack = Self::pack_orb_frame(clusters, opts, width, height);
        self.render_packed(&pack, width, height, t)
    }

    /// [`Self::render_frame`], but drawing into an externally supplied
    /// `view` of `format` instead of the offscreen read-back target (#229).
    /// This is the surface-present seam for the browser WebGPU path: the caller
    /// (orber-wasm) acquires the surface frame, hands its view + format here, and
    /// presents after this returns. Same packing / saturation arithmetic as
    /// [`Self::render_frame`]; only the final color target differs.
    ///
    /// # `view` / `format` contract (#229)
    ///
    /// - `format` must be **non-sRGB** (`Bgra8Unorm`, not `Bgra8UnormSrgb`): the
    ///   shaders emit already-sRGB-encoded values and write them raw into a Unorm
    ///   target (see the module's compositing contract), so an sRGB format would
    ///   apply the sRGB encoding a second time. Guarded by a `debug_assert`.
    /// - `view` must be exactly `opts.width × opts.height` texels. A mismatch is
    ///   **not** a wgpu validation error — the shader samples its inputs with
    ///   clamped fetches, so an oversized view silently renders a frame with
    ///   smeared / clipped edges instead of failing.
    ///
    /// Every `*_to_view` entry point shares this contract.
    pub fn render_frame_to_view(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        let pack = Self::pack_orb_frame(clusters, opts, width, height);
        self.render_packed_to_view(&pack, width, height, t, view, format);
    }

    /// Build the plain orb pack buffer for one frame: derive the WebGL pack-buffer
    /// scalars exactly as `get_render_data` (the wasm/WebGL entry) does, reuse
    /// [`pack_render_data_for_webgl`] (so the per-orb arithmetic is never
    /// reimplemented), then re-apply per-orb saturation. Shared by the read-back
    /// ([`Self::render_frame`]) and to_view ([`Self::render_frame_to_view`]) paths.
    fn pack_orb_frame(
        clusters: &[Cluster],
        opts: &AnimateOptions,
        width: u32,
        height: u32,
    ) -> Vec<f32> {
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
        let n_orbs = Self::resolved_orb_count(clusters, opts);
        // shape_id / glyph_rotate / edge_softness are SDF (glyph / image) inputs; the
        // plain orb shader ignores them. Pass orb defaults.
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
            0.0,  // shape_id = Orb
            true, // glyph_rotate (unused by Orb)
            opts.softness.edge_softness(),
        );

        // `pack_render_data_for_webgl` is shared with the WebGL path and must NOT
        // bake in saturation (the web side has its own knob). The native CLI has no
        // separate saturation knob, so we apply
        // `adjust_saturation_pub(color_at_t, saturation)` per orb here, in native
        // GPU land only, over the packed color words.
        //
        // Each color word triple is `c.color[i] as f32 / 255.0`, so `round(w*255)`
        // recovers the exact u8; we run the HSL saturation transform and write the
        // result back as `u8 / 255.0`.
        apply_saturation_to_pack(&mut pack, opts.saturation.max(0.0), n_orbs);
        pack
    }

    /// Render one **Glyph** frame at time `t` from `clusters` + `opts` (#212 Phase 1b,
    /// #235).
    ///
    /// `opts.shape` must be [`OrbShape::Glyph`]; the glyph `ch` / `font` select the
    /// SDF. The per-orb arithmetic reuses [`pack_render_data_for_webgl`] (so
    /// positions / radii / rotation match the WebGL path), saturation is re-applied
    /// per orb (the native CLI has no separate saturation knob), the glyph SDF is
    /// uploaded as an `R8Unorm` texture, and the SDF orb shader bilinear-samples it.
    ///
    /// # Unified orb mechanism (#235)
    ///
    /// Since #235 the glyph is just a different silhouette fed to the orb mechanism:
    /// the SDF sample becomes the normalized distance `r = 1 - signed_unit`, which
    /// feeds the **same** `falloff_curve` / 3-axis breath / Skia-lowp premultiply
    /// compositing the plain orb uses (`orb.wgsl`, SDF variant). It is **one** pass —
    /// the old aquarelle-derived bleed/halo 2nd pass group is removed, so glyph now
    /// blurs exactly like an orb (a `●` glyph looks like an orb; a `▲` blurs while
    /// keeping its triangular form). "Bleed" is now the領分 of the Aquarelle shape only.
    ///
    /// Returns a background-only frame (no glyph fill) when the glyph is unknown /
    /// empty in the bundled font — the "draw nothing for tofu" contract (never
    /// draw `.notdef` boxes).
    ///
    /// Native only (#229): read-back path; the wasm/browser path uses
    /// [`Self::render_frame_glyph_to_view`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_frame_glyph(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
    ) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        match self.prepare_glyph_frame(clusters, opts, width, height) {
            // Not a glyph shape: fall back to the plain orb path so the call is total.
            SdfFramePack::NotSdfShape => self.render_frame(clusters, opts, t),
            // Background-only routes through `render_packed` (the plain orb path),
            // which paints only the background — see `prepare_glyph_frame`.
            SdfFramePack::BackgroundOnly(pack) => self.render_packed(&pack, width, height, t),
            SdfFramePack::Sdf {
                pack,
                sdf_view,
                size,
            } => self.render_packed_inner(
                &pack,
                width,
                height,
                t,
                Some(GlyphBindings {
                    sdf_view: &sdf_view,
                    size,
                }),
            ),
        }
    }

    /// [`Self::render_frame_glyph`], but drawing into an externally supplied
    /// `view` of `format` instead of the offscreen read-back target (#229 / #235).
    /// One pass straight into `view` (the SDF orb variant; no bleed 2nd pass since
    /// #235). The orb / background fallbacks mirror the read-back variant.
    ///
    /// Same `view` / `format` contract as [`Self::render_frame_to_view`]:
    /// `format` non-sRGB (the shader output is already sRGB-encoded; an sRGB
    /// format would encode twice), `view` exactly `opts.width × opts.height`
    /// texels (a mismatch is no validation error — the shader clamps its
    /// fetches, so the edges silently smear / clip).
    pub fn render_frame_glyph_to_view(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        match self.prepare_glyph_frame(clusters, opts, width, height) {
            // Not a glyph shape: fall back to the plain orb path so the call is total.
            SdfFramePack::NotSdfShape => self.render_frame_to_view(clusters, opts, t, view, format),
            SdfFramePack::BackgroundOnly(pack) => {
                self.render_packed_to_view(&pack, width, height, t, view, format)
            }
            SdfFramePack::Sdf {
                pack,
                sdf_view,
                size,
            } => self.render_packed_inner_to_view(
                &pack,
                width,
                height,
                t,
                Some(GlyphBindings {
                    sdf_view: &sdf_view,
                    size,
                }),
                view,
                format,
            ),
        }
    }

    /// Build one Glyph frame's pack + SDF binding (shared by
    /// [`Self::render_frame_glyph`] / [`Self::render_frame_glyph_to_view`]).
    ///
    /// SDF size: the GPU binds one SDF for the whole frame (it cannot pick a
    /// per-orb size like a per-orb fill would), so it is sized from the *largest*
    /// orb radius (max weight × the breath max factor) so most orbs sample at or
    /// above their own size — bilinear up/down-sampling then keeps the edge sharp.
    ///
    /// No glyph (radius 0 / unknown char / empty SDF) ⇒ `BackgroundOnly`, the
    /// "draw nothing" contract: a glyph-shaped pack with zero orbs so only the
    /// background paints. The caller routes it through the plain orb pipeline (a
    /// zero-orb single pass), which paints just the background.
    ///
    /// The SDF upload happens here (outside `render_guard`): the SDF texture is
    /// keyed per `(ch, size)` and immutable once created, so (unlike the shared,
    /// overwritten orb texture) it needs no extra serialization —
    /// `render_packed_inner*` takes `render_guard` for the pass/upload/readback
    /// that actually shares mutable resources.
    fn prepare_glyph_frame(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        width: u32,
        height: u32,
    ) -> SdfFramePack {
        let (ch, font) = match opts.shape {
            crate::orb::OrbShape::Glyph { ch, font } => (ch, font),
            _ => return SdfFramePack::NotSdfShape,
        };

        let base_radius_unit = (width.min(height) as f32) * 0.25 * opts.orb_size.max(0.0);
        let max_weight = clusters
            .iter()
            .map(|c| c.weight.max(0.0))
            .fold(0.0_f32, f32::max);
        let frame_radius = base_radius_unit * max_weight.sqrt() * BREATH_RADIUS_MAX_FACTOR;

        let Some((sdf, sdf_size)) =
            crate::glyph::cached_glyph_sdf_for_radius(font, ch, frame_radius)
        else {
            return SdfFramePack::BackgroundOnly(Self::pack_sdf_frame(
                clusters, opts, width, height, 0,
            ));
        };

        let n_orbs = Self::resolved_orb_count(clusters, opts);
        let pack = Self::pack_sdf_frame(clusters, opts, width, height, n_orbs);
        let sdf_view = self.upload_glyph_sdf(ch, sdf_size, &sdf);
        SdfFramePack::Sdf {
            pack,
            sdf_view,
            size: sdf_size,
        }
    }

    /// Render one **Image** frame at time `t` from `clusters` + `opts` (#217,
    /// #235), using the same SDF orb path as [`Self::render_frame_glyph`].
    ///
    /// `opts.shape` must be [`OrbShape::Image`]; its `sdf` / `size` are uploaded as
    /// an `R8Unorm` texture and bound to the **same** SDF orb pipeline
    /// (`orb.wgsl`, SDF variant). The only difference from
    /// [`Self::render_frame_glyph`] is the SDF source: an image silhouette (supplied
    /// from outside, one fixed texture for the whole frame) instead of a per-radius
    /// cached font glyph. Per-orb positions / radii / rotation reuse
    /// [`pack_render_data_for_webgl`] and saturation is re-applied per orb, exactly
    /// like the glyph path. Since #235 it is a single pass that feeds the SDF to
    /// the orb mechanism — the image silhouette blurs like an orb (no bleed/halo).
    /// Non-Image shapes fall back to the plain orb path so the call is total.
    ///
    /// Native only (#229): read-back path; the wasm/browser path uses
    /// [`Self::render_frame_image_to_view`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_frame_image(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
    ) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        match self.prepare_image_frame(clusters, opts, width, height) {
            // Not an image shape: fall back to the plain orb path so the call is total.
            SdfFramePack::NotSdfShape => self.render_frame(clusters, opts, t),
            SdfFramePack::BackgroundOnly(pack) => self.render_packed(&pack, width, height, t),
            SdfFramePack::Sdf {
                pack,
                sdf_view,
                size,
            } => self.render_packed_inner(
                &pack,
                width,
                height,
                t,
                Some(GlyphBindings {
                    sdf_view: &sdf_view,
                    size,
                }),
            ),
        }
    }

    /// [`Self::render_frame_image`], but drawing into an externally supplied
    /// `view` of `format` instead of the offscreen read-back target (#229 / #235).
    /// Mirrors [`Self::render_frame_glyph_to_view`] (the two shapes share the SDF
    /// orb pipeline, single pass), including the `view` / `format` contract of
    /// [`Self::render_frame_to_view`]: `format` non-sRGB (the output is already
    /// sRGB-encoded), `view` exactly `opts.width × opts.height` texels (a
    /// mismatch silently smears the edges instead of failing validation).
    pub fn render_frame_image_to_view(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        match self.prepare_image_frame(clusters, opts, width, height) {
            // Not an image shape: fall back to the plain orb path so the call is total.
            SdfFramePack::NotSdfShape => self.render_frame_to_view(clusters, opts, t, view, format),
            SdfFramePack::BackgroundOnly(pack) => {
                self.render_packed_to_view(&pack, width, height, t, view, format)
            }
            SdfFramePack::Sdf {
                pack,
                sdf_view,
                size,
            } => self.render_packed_inner_to_view(
                &pack,
                width,
                height,
                t,
                Some(GlyphBindings {
                    sdf_view: &sdf_view,
                    size,
                }),
                view,
                format,
            ),
        }
    }

    /// Build one Image frame's pack + SDF binding (shared by
    /// [`Self::render_frame_image`] / [`Self::render_frame_image_to_view`]).
    /// Empty SDF (all-zero / no contrast slipped through) or wrong length ⇒
    /// `BackgroundOnly` ("draw nothing" contract). The image SDF is uploaded with
    /// a content-derived key disjoint from glyph keys.
    fn prepare_image_frame(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        width: u32,
        height: u32,
    ) -> SdfFramePack {
        let (sdf, sdf_size) = match &opts.shape {
            crate::orb::OrbShape::Image { sdf, size } => (sdf.clone(), *size),
            _ => return SdfFramePack::NotSdfShape,
        };

        if sdf_size == 0
            || sdf.len() < (sdf_size as usize) * (sdf_size as usize)
            || sdf.iter().all(|&b| b == 0)
        {
            return SdfFramePack::BackgroundOnly(Self::pack_sdf_frame(
                clusters, opts, width, height, 0,
            ));
        }

        let n_orbs = Self::resolved_orb_count(clusters, opts);
        let pack = Self::pack_sdf_frame(clusters, opts, width, height, n_orbs);
        let sdf_view = self.upload_image_sdf(sdf_size, &sdf);
        SdfFramePack::Sdf {
            pack,
            sdf_view,
            size: sdf_size,
        }
    }

    /// Resolved orb count: `count.unwrap_or(clusters.len())` clamped to
    /// [`MAX_ORB_COUNT`], at least 1 if there are clusters (mirrors
    /// `precompute_orb_params`). Shared by every pack builder.
    fn resolved_orb_count(clusters: &[Cluster], opts: &AnimateOptions) -> usize {
        opts.count
            .unwrap_or(clusters.len())
            .min(MAX_ORB_COUNT)
            .max(if clusters.is_empty() { 0 } else { 1 })
    }

    /// Build the SDF (Glyph / Image, `shape_id = 1`) pack buffer for one frame
    /// with `n_orbs` orbs (`0` = background only), then re-apply per-orb
    /// saturation, same as the plain orb path: the shared WebGL pack never bakes it
    /// in, so the native side runs `adjust_saturation_pub` here.
    fn pack_sdf_frame(
        clusters: &[Cluster],
        opts: &AnimateOptions,
        width: u32,
        height: u32,
        n_orbs: usize,
    ) -> Vec<f32> {
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
            1.0, // shape_id = SDF (glyph/image share id 1)
            opts.glyph_rotate,
            opts.softness.edge_softness(),
        );
        apply_saturation_to_pack(&mut pack, opts.saturation.max(0.0), n_orbs);
        pack
    }

    /// Render one **Aquarelle** frame at time `t` from `clusters` + `opts`, built on
    /// the [`aquarelle::render_aquarelle_orb`] four-layer model (#216 Phase 1c).
    ///
    /// `opts.shape` must be [`OrbShape::Aquarelle`]; its [`AquarelleParams`] drive
    /// the four layers. Per-orb positions / radii / colors come from
    /// [`crate::animate::aquarelle_modulated_clusters`] (the shared per-orb
    /// modulation), then [`Self::pack_aquarelle_orbs`] runs the crate's ChaCha8 RNG
    /// (`seed = orb index`) + `palette` HSL color math host-side to produce the
    /// offset center, satellite placements, and boosted/mixed u8 colors. The
    /// `orb_aquarelle.wgsl` shader evaluates the radials and composites SourceOver.
    ///
    /// # Fidelity
    ///
    /// The RNG / color math reuse the crate's exact arithmetic (host-side), so those
    /// are byte-identical to the crate; only the radial fill is analytic where Skia
    /// lowp anti-aliases `fill_path`, so the residual is an AA-only difference at orb
    /// edges (the same kind the orb has). Non-Aquarelle shapes fall back to the plain
    /// orb path so the call is total.
    ///
    /// Native only (#229): read-back path; the wasm/browser path uses
    /// [`Self::render_frame_aquarelle_to_view`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_frame_aquarelle(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
    ) -> RgbaImage {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        match Self::prepare_aquarelle_frame(clusters, opts, t, width, height) {
            // Not an aquarelle shape: fall back to the plain orb path so the call is total.
            None => self.render_frame(clusters, opts, t),
            Some(orbs) => self.render_aquarelle_packed(&orbs, width, height, opts.background),
        }
    }

    /// [`Self::render_frame_aquarelle`], but drawing into an externally supplied
    /// `view` of `format` instead of the offscreen read-back target (#229). Same
    /// CPU-side ChaCha8 / HSL pack; only the final color target differs.
    ///
    /// Same `view` / `format` contract as [`Self::render_frame_to_view`]:
    /// `format` non-sRGB (the shader output is already sRGB-encoded; an sRGB
    /// format would encode twice), `view` exactly `opts.width × opts.height`
    /// texels.
    pub fn render_frame_aquarelle_to_view(
        &self,
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let width = opts.width.max(1);
        let height = opts.height.max(1);
        let t = t.clamp(0.0, 1.0);
        match Self::prepare_aquarelle_frame(clusters, opts, t, width, height) {
            // Not an aquarelle shape: fall back to the plain orb path so the call is total.
            None => self.render_frame_to_view(clusters, opts, t, view, format),
            Some(orbs) => self.render_aquarelle_packed_to_view(
                &orbs,
                width,
                height,
                opts.background,
                view,
                format,
            ),
        }
    }

    /// Build one Aquarelle frame's packed orbs, or `None` when `opts.shape` is not
    /// Aquarelle (the caller falls back to the plain orb path). Shared by the
    /// read-back and to_view entry points.
    ///
    /// Runs the shared per-orb modulation (position wrap, radius breath, #33/#7
    /// color interpolation) — the cluster index order is the orb draw order and is
    /// also the `render_aquarelle_orb` seed `i`, so RNG consumption stays in step —
    /// then [`Self::pack_aquarelle_orbs`] (the crate's ChaCha8 + HSL math, CPU-side).
    fn prepare_aquarelle_frame(
        clusters: &[Cluster],
        opts: &AnimateOptions,
        t: f32,
        width: u32,
        height: u32,
    ) -> Option<Vec<GpuAquaOrb>> {
        let params = match opts.shape {
            crate::orb::OrbShape::Aquarelle(p) => p,
            _ => return None,
        };

        let modulated = crate::animate::aquarelle_modulated_clusters(clusters, opts, t);

        // `base_radius_unit` is the standard orb radius unit: min(w,h) * 0.25 * orb_size.
        let base_radius_unit = (width.min(height) as f32) * 0.25 * opts.orb_size.max(0.0);
        let saturation = opts.saturation.max(0.0);

        Some(Self::pack_aquarelle_orbs(
            &modulated,
            width as f32,
            height as f32,
            base_radius_unit,
            saturation,
            params,
        ))
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
            // Standard orb geometry: radius = base_radius_unit * sqrt(weight),
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
    /// under `render_guard` (like the orb / glyph / image path) so concurrent renders on a
    /// shared renderer cannot alias the one shared aquarelle texture / per-size
    /// target / read-back buffer (the #210 concurrency contract).
    #[cfg(not(target_arch = "wasm32"))]
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

        let (header_buffer, orb_view) =
            self.upload_aquarelle_frame(orbs, width, height, background);

        self.pipeline(
            orb_aquarelle_wgsl(),
            false,
            wgpu::TextureFormat::Rgba8Unorm,
            |cached| {
                self.sized_resources(width, height, |res| {
                    let bind_group = self.aquarelle_bind_group(
                        &cached.bind_group_layout,
                        &header_buffer,
                        &orb_view,
                    );
                    self.run_pass_and_readback(&cached.pipeline, &bind_group, res)
                })
            },
        )
    }

    /// [`Self::render_aquarelle_packed`], but drawing into an externally supplied
    /// `view` of `format` instead of the offscreen read-back target (#229). Same
    /// upload + single pass; serialized under `render_guard` because the to_view
    /// path shares the same grow-only aquarelle data-texture (the #210 contract).
    fn render_aquarelle_packed_to_view(
        &self,
        orbs: &[GpuAquaOrb],
        width: u32,
        height: u32,
        background: [u8; 4],
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        debug_assert_view_format_not_srgb(format);

        let _render_guard = self
            .render_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let width = width.max(1);
        let height = height.max(1);

        let (header_buffer, orb_view) =
            self.upload_aquarelle_frame(orbs, width, height, background);

        self.pipeline(orb_aquarelle_wgsl(), false, format, |cached| {
            let bind_group =
                self.aquarelle_bind_group(&cached.bind_group_layout, &header_buffer, &orb_view);
            self.run_pass_to_view(&cached.pipeline, &bind_group, view);
        });
    }

    /// Build the aquarelle header uniform buffer and upload the packed orbs into
    /// the grow-only aquarelle data-texture. Shared by the read-back and to_view
    /// paths; the caller must already hold `render_guard` (the orb texture is the
    /// shared mutable resource).
    fn upload_aquarelle_frame(
        &self,
        orbs: &[GpuAquaOrb],
        width: u32,
        height: u32,
        background: [u8; 4],
    ) -> (wgpu::Buffer, wgpu::TextureView) {
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
        (header_buffer, orb_view)
    }

    /// Build the aquarelle bind group (header uniform 0 + orb data-texture 1).
    fn aquarelle_bind_group(
        &self,
        layout: &wgpu::BindGroupLayout,
        header_buffer: &wgpu::Buffer,
        orb_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("orber-aquarelle-bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: header_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(orb_view),
                },
            ],
        })
    }

    /// Upload the packed aquarelle orbs into the grow-only `Rgba32Float` aquarelle
    /// data-texture (9 texels wide × `orbs.len()` tall) and return a view to bind.
    /// Mirrors [`Self::upload_orb_texture`] but for the separate aquarelle texture so
    /// the orb / glyph / image data-texture is never resized to the wider aquarelle layout.
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

    /// Render one **plain orb** frame from a raw `pack_render_data_for_webgl` buffer.
    ///
    /// `pack` must be the header(16) + per-orb(16 × n_orbs) layout produced by
    /// [`pack_render_data_for_webgl`]. `t` is the normalized time written into the
    /// shader's `u_t`; it is clamped to `0.0..=1.0`. Glyph / image rendering uses the
    /// private `render_packed_inner` with an SDF binding instead.
    ///
    /// Native only (#229): read-back path; the wasm/browser path uses
    /// [`Self::render_packed_to_view`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_packed(&self, pack: &[f32], width: u32, height: u32, t: f32) -> RgbaImage {
        self.render_packed_inner(pack, width, height, t, None)
    }

    /// Render one **plain orb** frame from a raw `pack_render_data_for_webgl` buffer
    /// into an externally supplied `view` of `format` (#229). This is the
    /// pack-level surface-present seam the browser WebGPU path shares: the caller
    /// owns the surface (creation / configure / present); core only draws into the
    /// frame's view.
    ///
    /// Same `view` / `format` contract as [`Self::render_frame_to_view`]:
    /// `format` non-sRGB (the shader output is already sRGB-encoded; an sRGB
    /// format would encode twice), `view` exactly `width × height` texels.
    pub fn render_packed_to_view(
        &self,
        pack: &[f32],
        width: u32,
        height: u32,
        t: f32,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        self.render_packed_inner_to_view(pack, width, height, t, None, view, format);
    }

    /// Shared core of the orb / glyph / image read-back paths (#235). `glyph =
    /// Some(_)` selects the SDF orb pipeline (`orb_sdf_wgsl`) and binds the SDF
    /// texture + sampler; `None` is the plain orb pipeline (`orb_wgsl`). Either way
    /// it is **one** full-screen pass — glyph / image now go through the same orb
    /// mechanism (the SDF is just a different silhouette fed to it), with no
    /// bleed/halo 2nd pass (#235). The orb data-texture, per-size resources, the
    /// `render_guard` serialization, and the read-back are all shared.
    #[cfg(not(target_arch = "wasm32"))]
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
        // result — the same params render byte-identically with or without the lock.
        let _render_guard = self
            .render_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let width = width.max(1);
        let height = height.max(1);
        let t = t.clamp(0.0, 1.0);

        let (params_buffer, orb_view) = self.upload_packed_frame(pack, width, height, t, &glyph);

        // Pipeline (shader compile) cached per (shader source, target format);
        // target / read-back cached per size; orb texture grows as needed. Only the
        // small params uniform / bind group are rebuilt per frame. The SDF variant
        // selects a different shader source + adds the SDF texture / sampler
        // bindings (2/3) but draws the same single pass. The read-back path always
        // targets `Rgba8Unorm`.
        let (shader, is_glyph) = match &glyph {
            Some(_) => (orb_sdf_wgsl(), true),
            None => (orb_wgsl(), false),
        };
        self.pipeline(
            shader,
            is_glyph,
            wgpu::TextureFormat::Rgba8Unorm,
            |cached| {
                self.sized_resources(width, height, |res| {
                    let bind_group = self.orb_bind_group(
                        &cached.bind_group_layout,
                        &params_buffer,
                        &orb_view,
                        &glyph,
                    );
                    self.run_pass_and_readback(&cached.pipeline, &bind_group, res)
                })
            },
        )
    }

    /// Shared core of the orb / glyph / image **to_view** paths (#229 / #235): same
    /// upload / bind-group / single-pass structure as [`Self::render_packed_inner`],
    /// but the pass targets the externally supplied `view` of `format` and nothing
    /// is read back. The SDF variant draws the same one pass as the orb variant
    /// (no bleed 2nd pass since #235); both target `format` directly. Serialized
    /// under `render_guard` because this path shares the same grow-only orb texture
    /// (the #210 contract).
    #[allow(clippy::too_many_arguments)]
    fn render_packed_inner_to_view(
        &self,
        pack: &[f32],
        width: u32,
        height: u32,
        t: f32,
        glyph: Option<GlyphBindings<'_>>,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        debug_assert_view_format_not_srgb(format);

        let _render_guard = self
            .render_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let width = width.max(1);
        let height = height.max(1);
        let t = t.clamp(0.0, 1.0);

        let (params_buffer, orb_view) = self.upload_packed_frame(pack, width, height, t, &glyph);

        // Both orb and SDF draw straight into the caller's view, so the pipeline
        // targets `format` either way (the SDF variant just binds the SDF
        // texture / sampler and runs a different shader source).
        let (shader, is_glyph) = match &glyph {
            Some(_) => (orb_sdf_wgsl(), true),
            None => (orb_wgsl(), false),
        };
        self.pipeline(shader, is_glyph, format, |cached| {
            let bind_group =
                self.orb_bind_group(&cached.bind_group_layout, &params_buffer, &orb_view, &glyph);
            self.run_pass_to_view(&cached.pipeline, &bind_group, view);
        });
    }

    /// Build the `Params` uniform buffer and upload the per-orb data-texture for
    /// one packed frame. Shared by the read-back and to_view paths; the caller
    /// must already hold `render_guard` (the grow-only orb texture is the shared
    /// mutable resource). Done before entering the pipeline/sized closures so the
    /// orb-texture lock is never nested under them.
    fn upload_packed_frame(
        &self,
        pack: &[f32],
        width: u32,
        height: u32,
        t: f32,
        glyph: &Option<GlyphBindings<'_>>,
    ) -> (wgpu::Buffer, wgpu::TextureView) {
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
            // Both are SDF-only; the plain orb variant never reads them. `sdf_size`
            // comes from the SDF binding (the shader uses it for the SDF bilinear
            // sampling convention); the orb path leaves it 0.
            glyph_rotate: pack[11],
            edge_softness: pack[12],
            sdf_size: glyph.as_ref().map_or(0.0, |g| g.size as f32),
        };
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("orber-orb-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        // Per-orb words → one `GpuOrb` (4 vec4s) per orb: color+weight, phase
        // quartet, cross_axis/style/speed, and rotation (base_angle,
        // rot_speed_signed). The orb variant ignores the rot texel; the SDF variant
        // reads it for #136.
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
            // cut-off is `off + 13 > len`. (The orb variant only reads up to
            // `off + 10`, but requiring the full 13 words is safe: the production
            // packer always emits 16 per-orb words. The orb parity tests feed full packs.)
            if off + 13 > pack.len() {
                break;
            }
            *slot = GpuOrb {
                color: [pack[off], pack[off + 1], pack[off + 2], pack[off + 3]],
                phase: [pack[off + 4], pack[off + 5], pack[off + 6], pack[off + 7]],
                misc: [pack[off + 8], pack[off + 9], pack[off + 10], 0.0],
                // off + 11 = base_angle, off + 12 = rot_speed_signed (#136).
                // The SDF variant reads these; the orb variant ignores the rot texel.
                rot: [pack[off + 11], pack[off + 12], 0.0, 0.0],
            };
        }

        // Upload the per-orb data into the grow-only data-texture and grab a
        // (clonable) view to bind.
        let orb_view = self.upload_orb_texture(&orbs);
        (params_buffer, orb_view)
    }

    /// Build the orb bind group: params uniform 0 + orb data-texture 1, plus the
    /// glyph SDF texture 2 / sampler 3 when `glyph` is set.
    fn orb_bind_group(
        &self,
        layout: &wgpu::BindGroupLayout,
        params_buffer: &wgpu::Buffer,
        orb_view: &wgpu::TextureView,
        glyph: &Option<GlyphBindings<'_>>,
    ) -> wgpu::BindGroup {
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(orb_view),
            },
        ];
        if let Some(g) = glyph {
            entries.push(wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(g.sdf_view),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&self.glyph_sampler),
            });
        }
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("orber-orb-bg"),
            layout,
            entries: &entries,
        })
    }

    /// Render one full-screen pass into the externally supplied `view` and submit
    /// (#229). No read-back: this is the to_view/present path — the caller (e.g.
    /// orber-wasm holding a surface frame) presents after this returns.
    fn run_pass_to_view(
        &self,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
        view: &wgpu::TextureView,
    ) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("orber-view-encoder"),
            });
        self.record_fullscreen_pass(&mut encoder, "orber-view-pass", view, pipeline, bind_group);
        self.queue.submit(Some(encoder.finish()));
    }

    /// Render one full-screen pass into `res.target`, copy it into the read-back
    /// buffer, map it, and strip wgpu's row padding into a tight `RgbaImage`.
    #[cfg(not(target_arch = "wasm32"))]
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
                label: Some("orber-orb-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("orber-orb-pass"),
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
    /// wgpu's row padding into a tight `RgbaImage`. Used by the single-pass read-back
    /// ([`run_pass_and_readback`](Self::run_pass_and_readback)) for every shape;
    /// `encoder` already holds the render pass that wrote `res.target`.
    ///
    /// Native only (#229): buffer mapping needs the blocking
    /// `device.poll(wait_indefinitely)`, which does not exist on wasm32 (the
    /// browser maps buffers asynchronously). The wasm path never reads back — it
    /// draws into the surface view and presents.
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Record a single full-screen triangle pass into `target` with `pipeline` +
    /// `bind_group` (clear-to-transparent load). Used by the to_view present path.
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
}

/// Round `value` up to the next multiple of `align` (a power of two).
/// Read-back row padding only, so native only.
#[cfg(not(target_arch = "wasm32"))]
fn align_up(value: u32, align: u32) -> u32 {
    value.div_ceil(align) * align
}

/// Apply `adjust_saturation_pub` to the per-orb color words of a
/// `pack_render_data_for_webgl` buffer, in place (native GPU path only).
///
/// `pack_render_data_for_webgl` is shared with the WebGL path and intentionally
/// leaves saturation out (the web side has its own knob), so the native GPU path
/// re-applies the `adjust_saturation_pub` transform per orb here instead.
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
/// so the CPU pack produces the **same u8 color** the crate feeds Skia lowp. The
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

/// Debug-guard for the to_view paths (#229): the shaders emit already-sRGB-encoded
/// values and write them raw into a Unorm target (see the module's compositing
/// contract), so an sRGB view format would apply the sRGB encoding a second time.
/// `debug_assert` so the release/browser hot path pays nothing; called from the
/// two internal to_view funnels (`render_packed_inner_to_view` /
/// `render_aquarelle_packed_to_view`) that every `*_to_view` entry point routes
/// through.
fn debug_assert_view_format_not_srgb(format: wgpu::TextureFormat) {
    debug_assert!(
        !format.is_srgb(),
        "to_view format must be non-sRGB (e.g. Bgra8Unorm, not Bgra8UnormSrgb): the shader \
         output is already sRGB-encoded, so an sRGB format would encode twice (got {format:?})"
    );
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

    fn orb_opts(w: u32, h: u32, direction: MotionDirection, speed: MotionSpeed) -> AnimateOptions {
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
            shape: OrbShape::Orb,
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
            "{ctx}: max per-channel diff {max_diff} at pixel ({},{}) channel {} (a={:?} b={:?})",
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
        let opts = orb_opts(48, 32, MotionDirection::LeftToRight, MotionSpeed::Slow);
        for k in 0..16 {
            let t = k as f32 / 15.0;
            let _ = renderer.render_frame(&clusters, &opts, t);
        }
        let (pipes, sizes) = renderer.cache_sizes();
        assert_eq!(pipes, 1, "shader must compile exactly once");
        assert_eq!(sizes, 1, "size must allocate exactly once");

        let opts2 = orb_opts(24, 24, MotionDirection::LeftToRight, MotionSpeed::Slow);
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
        let opts_a = orb_opts(40, 24, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let mut opts_b = orb_opts(40, 24, MotionDirection::TopToBottom, MotionSpeed::Mid);
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
        let mut opts = orb_opts(40, 28, MotionDirection::LeftToRight, MotionSpeed::Slow);

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
        let mut opts = orb_opts(48, 32, MotionDirection::LeftToRight, MotionSpeed::Mid);

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
            0.0,  // shape_id (Orb)
            true, // glyph_rotate (ignored by Orb)
            0.5,  // edge_softness (ignored by Orb)
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
            let mut opts = orb_opts(44, 30, MotionDirection::LeftToRight, MotionSpeed::Mid);
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
                    let mut opts = orb_opts(44, 30, MotionDirection::LeftToRight, MotionSpeed::Mid);
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

    /// The `GLYPH_SDF_CONTENT_SPAN` constant hardcoded in `orb.wgsl` (the SDF
    /// variant) must match the Rust `crate::glyph::GLYPH_SDF_CONTENT_SPAN_PUB`
    /// (= 1/√2). If the CPU constant ever changes, this guards against the WGSL
    /// drifting out of sync (which would shift the glyph UV mapping and break parity).
    #[test]
    fn glyph_wgsl_content_span_matches_rust() {
        let wgsl = orb_sdf_wgsl();
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
            "orb.wgsl GLYPH_SDF_CONTENT_SPAN ({lit}) must match Rust ({})",
            crate::glyph::GLYPH_SDF_CONTENT_SPAN_PUB
        );
    }

    // ---- #235: orb / SDF WGSL variant composition (no GPU needed) -------------

    /// #235 byte-exact土台: the orb variant's loop body must inline the **analytic
    /// circle distance** — the same two lines the old `orb_circle.wgsl` had —
    /// `let dist = distance(...);` then `let r = dist / radius;`, and must NOT carry
    /// any SDF binding, rotation read, or rotation helper. If this drifts, the orb
    /// variant's compiled shader is no longer the old circle body and the orb output
    /// is no longer bit-exact. Locks the DISTANCE SOURCE inlining + the absence of
    /// SDF-only machinery in one place.
    #[test]
    fn orb_variant_wgsl_is_byte_exact_with_old_circle_body() {
        let wgsl = orb_wgsl();
        assert!(
            wgsl.contains("let dist = distance(sample_px, vec2<f32>(cx, cy));"),
            "orb variant must inline the analytic circle distance line"
        );
        assert!(
            wgsl.contains("let r = dist / radius;"),
            "orb variant must inline `r = dist / radius` (the old circle body)"
        );
        // The orb variant must not carry any SDF machinery.
        assert!(
            !wgsl.contains("sdf_tex") && !wgsl.contains("sdf_samp"),
            "orb variant must not declare the SDF texture / sampler bindings"
        );
        assert!(
            !wgsl.contains("o.rot") && !wgsl.contains("glyph_rotation_angle"),
            "orb variant must not read the rotation texel or the rotation helper"
        );
        assert!(
            !wgsl.contains("//!ORB_DISTANCE_SOURCE")
                && !wgsl.contains("//!ORB_LOAD")
                && !wgsl.contains("//!ORB_EXTRA_BINDINGS")
                && !wgsl.contains("//!ORB_HELPERS"),
            "every template marker must be substituted away in the orb variant"
        );
    }

    /// #235: the orb variant must contain no SDF binding, no per-orb rotation read,
    /// and no texture sample call — proving it is the SDF-free path (a stray
    /// `textureSampleLevel` would mean an SDF leaked into the orb shader and the
    /// byte-exact guarantee is gone).
    #[test]
    fn orb_variant_has_no_sdf_bindings_or_rot() {
        let wgsl = orb_wgsl();
        assert!(
            !wgsl.contains("@binding(2)") && !wgsl.contains("@binding(3)"),
            "orb variant must declare only bindings 0 (params) and 1 (orb_tex)"
        );
        assert!(
            !wgsl.contains("o.rot"),
            "orb variant must not read the per-orb rotation texel"
        );
        assert!(
            !wgsl.contains("textureSampleLevel"),
            "orb variant must not sample any SDF texture"
        );
    }

    /// #235: the SDF variant (glyph / image) must carry the SDF texture + sampler
    /// bindings, read the per-orb rotation texel, apply the rotation helper before
    /// sampling, and convert the signed SDF sample to `r = 1.0 - signed_unit`. This
    /// is the positive counterpart of `orb_variant_has_no_sdf_bindings_or_rot`:
    /// together they pin that the two variants differ exactly by the SDF source.
    #[test]
    fn sdf_variant_has_sdf_bindings_and_rot() {
        let wgsl = orb_sdf_wgsl();
        assert!(
            wgsl.contains("sdf_tex") && wgsl.contains("sdf_samp"),
            "SDF variant must declare the SDF texture + sampler bindings"
        );
        assert!(
            wgsl.contains("@binding(2)") && wgsl.contains("@binding(3)"),
            "SDF variant must bind the SDF texture (2) and sampler (3)"
        );
        assert!(
            wgsl.contains("o.rot") && wgsl.contains("glyph_rotation_angle"),
            "SDF variant must read the rotation texel and apply glyph_rotation_angle"
        );
        assert!(
            wgsl.contains("textureSampleLevel"),
            "SDF variant must bilinear-sample the SDF texture"
        );
        assert!(
            wgsl.contains("let r = 1.0 - signed_unit;"),
            "SDF variant must convert the signed SDF sample to r = 1 - signed_unit"
        );
    }

    /// #235 境界取り違え狙い撃ち: the SDF distance source's CONTENT_SPAN UV clip must
    /// use **strict** inequalities (`u < 0.0 || u > 1.0`), not `<=` / `>=`. A `<=`
    /// at 0.0 / `>=` at 1.0 would `continue` on the exact edge texel and drop the
    /// silhouette's outermost row/column. Asserts the strict form is present and the
    /// inclusive forms are absent.
    #[test]
    fn wgsl_clip_uses_strict_inequality() {
        let wgsl = orb_sdf_wgsl();
        assert!(
            wgsl.contains("if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) {"),
            "SDF UV clip must use strict < 0.0 / > 1.0 inequalities"
        );
        assert!(
            !wgsl.contains("u <= 0.0")
                && !wgsl.contains("u >= 1.0")
                && !wgsl.contains("v <= 0.0")
                && !wgsl.contains("v >= 1.0"),
            "SDF UV clip must NOT use inclusive <= / >= (would drop the edge texel)"
        );
    }

    /// #235 エッジ消失の死守: the shared `falloff_curve` early-out must fire on
    /// `r_in >= 1.0` (inclusive), not `> 1.0`. At exactly r = 1.0 (the silhouette
    /// edge) the fill must already be transparent; a `>` would keep the edge faintly
    /// lit and re-introduce a hairline ring. Checked on the template (shared by both
    /// variants).
    #[test]
    fn falloff_early_out_uses_ge_one() {
        let wgsl = ORB_WGSL_TEMPLATE;
        assert!(
            wgsl.contains("if (opacity <= 0.0 || r_in >= 1.0) {"),
            "falloff_curve early-out must use r_in >= 1.0 (inclusive edge transparency)"
        );
        assert!(
            !wgsl.contains("r_in > 1.0"),
            "falloff_curve must NOT use a strict r_in > 1.0 (would keep the edge lit)"
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
        // #235: glyph now goes through the unified orb mechanism (one pass, no
        // bleed/halo spread), so the lit count is lower than the old bleed-pass
        // render (~150 here vs >200 before) but the star is still clearly painted.
        assert!(
            lit > 100,
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

    /// #217: an empty (all-zero) image SDF yields a background-only frame
    /// ("draw nothing" contract, no panic).
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

    /// #217: `render_frame_image` on a non-Image shape falls back to the plain orb path
    /// (the call is total). It must still produce a valid frame.
    #[test]
    fn gpu_image_entry_non_image_falls_back_to_orb() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_image_entry_non_image_falls_back_to_orb")
        else {
            return;
        };
        let clusters = sample_clusters();
        let mut opts = image_opts(80, 80);
        opts.shape = OrbShape::Orb;
        let via_image_entry = renderer.render_frame_image(&clusters, &opts, 0.5);
        let via_orb = renderer.render_frame(&clusters, &opts, 0.5);
        assert_eq!(
            via_image_entry.as_raw(),
            via_orb.as_raw(),
            "render_frame_image on Orb must equal render_frame (plain orb path)"
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
    /// Symbols 2 subset) must yield a background-only frame — no orb fill, the
    /// "draw nothing for tofu" contract.
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

    /// `render_frame_glyph` on a non-Glyph shape falls back to the plain orb path
    /// (the call is total). An Orb-shaped opts through the glyph entry must match
    /// the dedicated orb `render_frame` within the ±2/channel contract.
    #[test]
    fn gpu_glyph_entry_orb_shape_falls_back() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_entry_orb_shape_falls_back")
        else {
            return;
        };
        let clusters = sample_clusters();
        let opts = orb_opts(40, 28, MotionDirection::LeftToRight, MotionSpeed::Mid);
        let via_orb = renderer.render_frame(&clusters, &opts, 0.5);
        let via_glyph_entry = renderer.render_frame_glyph(&clusters, &opts, 0.5);
        let max_diff = assert_within_tolerance(
            &via_orb,
            &via_glyph_entry,
            "orb-shape through glyph entry vs render_frame",
        );
        eprintln!("glyph-entry orb fallback: max per-channel diff = {max_diff}");
    }

    // ---- #212 Phase 1b: 4-texel widening / glyph dispatch regression guards ----

    /// #212 (#2): the `render_packed_inner` short-row guard changed from a per-orb
    /// `off + 11` cut-off to `off + 13` (so the new rotation words at `off+11` /
    /// `off+12` are read). A hand-built single-orb pack sized to **exactly**
    /// `off + 13` words must render that orb — it must NOT be cut one orb early.
    /// This is the regression guard for the boundary change: with the old `off+11`
    /// (or a `>=` form) the last orb would have been dropped at this exact length.
    ///
    /// Build a real 1-orb pack via the production packer (so the per-orb
    /// arithmetic is correct), truncate it to `HEADER_WORDS + 13` (dropping only the
    /// 3 trailing unused padding words of the 16-word slot), render it, and assert
    /// (a) it equals the full untruncated pack bit-exact, and (b) the orb is
    /// actually drawn (output is not background-only).
    #[test]
    fn render_packed_short_pack_orb_unaffected_by_13word_guard() {
        let Some(renderer) =
            require_or_skip_renderer("render_packed_short_pack_orb_unaffected_by_13word_guard")
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
            0.0,  // shape_id = Orb
            true, // glyph_rotate (ignored by Orb)
            0.5,  // edge_softness (ignored by Orb)
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
    /// the output dims must be correct, and there must be no panic / OOB. (orb
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
    /// paints (mirrors the orb empty-clusters test for the glyph dispatch).
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
    /// wrap (`turns = cycle * speed * t - floor(...)`) against the Rust
    /// `crate::animate::glyph_rotation_angle` `rem_euclid`
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
    /// same input rendered alone (a fresh renderer). Mirrors the orb concurrent
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
    /// that same input rendered alone (a fresh renderer). Mirrors the orb
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
    /// plain orb path (the call is total), matching the Glyph-entry fallback contract.
    #[test]
    fn aquarelle_entry_orb_shape_falls_back() {
        let Some(renderer) = require_or_skip_renderer("aquarelle_entry_orb_shape_falls_back")
        else {
            return;
        };
        let clusters = vec![cluster([200, 100, 50], 0.5, 0.5, 1.0)];
        let mut opts = aquarelle_opts(64, 64, AquarelleParams::default());
        opts.shape = OrbShape::Orb;
        // Should not panic and should produce the same image as the plain orb path.
        let via_aqua = renderer.render_frame_aquarelle(&clusters, &opts, 0.0);
        let via_orb = renderer.render_frame(&clusters, &opts, 0.0);
        assert_eq!(
            via_aqua.as_raw(),
            via_orb.as_raw(),
            "non-aquarelle shape must fall back to the plain orb path byte-for-byte"
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
    /// a gray toward white stays gray), so the desaturation is applied in the pack and
    /// the shader only ever sees already-grayscale colors.
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

    /// #229 helper: create a fresh offscreen texture of `format`, run `draw` with
    /// its view (the to_view seam), then read the texture back into an
    /// `RgbaImage` (raw bytes, no channel reordering). Mirrors the production
    /// read-back's row-padding handling.
    fn readback_via_view(
        renderer: &GpuRenderer,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        draw: impl FnOnce(&wgpu::TextureView),
    ) -> RgbaImage {
        let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("orber-test-view-target"),
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
        draw(&view);

        let unpadded = width * BYTES_PER_PIXEL;
        let padded = align_up(unpadded, ROW_ALIGNMENT);
        let buffer = renderer.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("orber-test-view-readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = renderer
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("orber-test-view-encoder"),
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
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        renderer.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        renderer
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed");
        let mapped = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((unpadded * height) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&mapped[start..start + unpadded as usize]);
        }
        drop(mapped);
        buffer.unmap();
        RgbaImage::from_raw(width, height, pixels)
            .expect("read-back buffer matches image dimensions")
    }

    /// #229: the to_view path must draw the same bytes the read-back path draws.
    /// Orb (`render_packed_to_view`, the pack-level browser seam) and Glyph
    /// including the bleed 2nd pass (`render_frame_glyph_to_view`) are rendered
    /// into a fresh offscreen `Rgba8Unorm` texture via to_view, read back
    /// manually, and required byte-identical to `render_packed` /
    /// `render_frame_glyph`. Lit pixels are asserted first so the identity is not
    /// trivially satisfied by an empty frame.
    #[test]
    fn to_view_matches_readback_orb_and_glyph() {
        let Some(renderer) = require_or_skip_renderer("to_view_matches_readback_orb_and_glyph")
        else {
            return;
        };
        let (w, h) = (96u32, 64u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();

        // Orb, via the pack-level seam shared with the browser path.
        let opts = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let pack = GpuRenderer::pack_orb_frame(&clusters, &opts, w, h);
        let reference = renderer.render_packed(&pack, w, h, 0.3);
        let via_view = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_packed_to_view(&pack, w, h, 0.3, view, format);
        });
        assert!(
            lit_vs_bg(&reference, opts.background, 8) > 0,
            "circle reference must have lit pixels"
        );
        assert_eq!(
            reference.as_raw(),
            via_view.as_raw(),
            "circle to_view bytes must match the read-back path"
        );

        // Glyph (bleed 2nd pass included), via the frame-level seam.
        let glyph = glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true);
        let reference = renderer.render_frame_glyph(&clusters, &glyph, 0.3);
        let via_view = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_frame_glyph_to_view(&clusters, &glyph, 0.3, view, format);
        });
        assert!(
            lit_vs_bg(&reference, glyph.background, 8) > 0,
            "glyph reference must have lit pixels"
        );
        assert_eq!(
            reference.as_raw(),
            via_view.as_raw(),
            "glyph to_view bytes must match the read-back path"
        );
    }

    /// #229: the to_view path must draw the same bytes the read-back path draws
    /// for the two remaining SDF/aquarelle shapes — Image (the Glyph-shared SDF
    /// pipeline + bleed via `render_frame_image_to_view`) and Aquarelle
    /// (`render_frame_aquarelle_to_view`). Completes the per-shape to_view ↔
    /// read-back identity started by `to_view_matches_readback_orb_and_glyph`.
    /// Lit pixels are asserted first so the identity is not trivially satisfied
    /// by an empty frame.
    #[test]
    fn to_view_matches_readback_image_and_aquarelle() {
        let Some(renderer) =
            require_or_skip_renderer("to_view_matches_readback_image_and_aquarelle")
        else {
            return;
        };
        let (w, h) = (96u32, 64u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();

        // Image, via the frame-level seam (shares the Glyph SDF pipeline + bleed).
        let image = image_opts(w, h);
        let reference = renderer.render_frame_image(&clusters, &image, 0.3);
        let via_view = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_frame_image_to_view(&clusters, &image, 0.3, view, format);
        });
        assert!(
            lit_vs_bg(&reference, image.background, 8) > 0,
            "image reference must have lit pixels"
        );
        assert_eq!(
            reference.as_raw(),
            via_view.as_raw(),
            "image to_view bytes must match the read-back path"
        );

        // Aquarelle, via the frame-level seam (single-pass aquarelle WGSL).
        let aqua = aquarelle_opts(w, h, AquarelleParams::default());
        let reference = renderer.render_frame_aquarelle(&clusters, &aqua, 0.3);
        let via_view = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_frame_aquarelle_to_view(&clusters, &aqua, 0.3, view, format);
        });
        assert!(
            lit_vs_bg(&reference, aqua.background, 8) > 0,
            "aquarelle reference must have lit pixels"
        );
        assert_eq!(
            reference.as_raw(),
            via_view.as_raw(),
            "aquarelle to_view bytes must match the read-back path"
        );
    }

    /// Swap the R/B channels of every pixel: reinterprets a raw read-back of a
    /// `Bgra8Unorm` texture (which `readback_via_view` returns without channel
    /// reordering) as a true RGBA image.
    fn swap_rb(img: &RgbaImage) -> RgbaImage {
        let mut out = img.clone();
        for p in out.pixels_mut() {
            p.0.swap(0, 2);
        }
        out
    }

    /// #229: the `(shader, format)` pipeline-cache key must produce a *working*
    /// non-`Rgba8Unorm` pipeline for every shape's to_view entry point. Each shape
    /// is rendered to_view into a `Bgra8Unorm` texture (the typical browser
    /// surface format) and, after the R/B swap, must match its own read-back
    /// (`Rgba8Unorm`) reference within the ±2/channel contract — a format-key
    /// collision (reusing the Rgba8 pipeline for the Bgra8 target) would either
    /// fail validation or come back channel-swapped, far outside tolerance. For
    /// Glyph / Image this also pins the #229 format split: the fill + blur
    /// intermediates stay `Rgba8Unorm` while only the final compose pass targets
    /// `Bgra8Unorm`. Lit pixels are asserted per shape so the parity is never
    /// satisfied by an empty frame.
    #[test]
    fn to_view_bgra8_matches_readback_after_swap_for_all_shapes() {
        let Some(renderer) =
            require_or_skip_renderer("to_view_bgra8_matches_readback_after_swap_for_all_shapes")
        else {
            return;
        };
        let (w, h) = (64u32, 48u32);
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let clusters = sample_clusters();

        // (name, reference read-back render, to_view render) per shape.
        let circle = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let glyph = glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true);
        let image = image_opts(w, h);
        let aqua = aquarelle_opts(w, h, AquarelleParams::default());
        type ToView<'a> = Box<dyn Fn(&wgpu::TextureView) + 'a>;
        let cases: Vec<(&str, [u8; 4], RgbaImage, ToView<'_>)> = vec![
            (
                "circle",
                circle.background,
                renderer.render_frame(&clusters, &circle, 0.3),
                Box::new(|view: &wgpu::TextureView| {
                    renderer.render_frame_to_view(&clusters, &circle, 0.3, view, format);
                }),
            ),
            (
                "glyph",
                glyph.background,
                renderer.render_frame_glyph(&clusters, &glyph, 0.3),
                Box::new(|view: &wgpu::TextureView| {
                    renderer.render_frame_glyph_to_view(&clusters, &glyph, 0.3, view, format);
                }),
            ),
            (
                "image",
                image.background,
                renderer.render_frame_image(&clusters, &image, 0.3),
                Box::new(|view: &wgpu::TextureView| {
                    renderer.render_frame_image_to_view(&clusters, &image, 0.3, view, format);
                }),
            ),
            (
                "aquarelle",
                aqua.background,
                renderer.render_frame_aquarelle(&clusters, &aqua, 0.3),
                Box::new(|view: &wgpu::TextureView| {
                    renderer.render_frame_aquarelle_to_view(&clusters, &aqua, 0.3, view, format);
                }),
            ),
        ];

        for (name, bg, reference, draw) in &cases {
            assert!(
                lit_vs_bg(reference, *bg, 8) > 0,
                "{name}: reference must have lit pixels"
            );
            let raw_bgra = readback_via_view(renderer, w, h, format, draw);
            let as_rgba = swap_rb(&raw_bgra);
            let max_diff = assert_within_tolerance(
                reference,
                &as_rgba,
                &format!("{name} Bgra8 to_view (after R/B swap) vs Rgba8 read-back"),
            );
            eprintln!("{name} Bgra8 to_view vs read-back: max per-channel diff = {max_diff}");
        }
    }

    /// #229 (F1): the orb pipeline cache is keyed by `(shader, target format)`.
    /// On a *private* renderer (exact entry counts), the same orb shader
    /// rendered to_view at `Rgba8Unorm` then `Bgra8Unorm` must hold exactly two
    /// pipeline entries, and re-rendering both formats must not add more (cache
    /// hit). The sized (read-back) cache must stay empty throughout — the to_view
    /// path never allocates an offscreen target / read-back buffer (on wasm32
    /// that cache does not even exist).
    #[test]
    fn to_view_pipeline_cache_keyed_by_format() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("to_view_pipeline_cache_keyed_by_format")
        else {
            return;
        };
        let (w, h) = (32u32, 24u32);
        let clusters = sample_clusters();
        let opts = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let pack = GpuRenderer::pack_orb_frame(&clusters, &opts, w, h);
        let draw = |format: wgpu::TextureFormat| {
            let _ = readback_via_view(&renderer, w, h, format, |view| {
                renderer.render_packed_to_view(&pack, w, h, 0.3, view, format);
            });
        };

        assert_eq!(
            renderer.cache_sizes(),
            (0, 0),
            "fresh renderer: empty caches"
        );
        draw(wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(
            renderer.cache_sizes(),
            (1, 0),
            "first format compiles one pipeline; to_view must not allocate sized resources"
        );
        draw(wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(
            renderer.cache_sizes(),
            (2, 0),
            "same shader at a second format must add exactly one pipeline entry"
        );
        draw(wgpu::TextureFormat::Rgba8Unorm);
        draw(wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(
            renderer.cache_sizes(),
            (2, 0),
            "repeated formats must hit the pipeline cache (2 entries stable)"
        );
    }

    /// #229 (B1): `render_frame_glyph_to_view` on a non-Glyph shape must fall back
    /// to the orb to_view path (the call is total), matching the read-back
    /// variant's fallback contract — byte-identical to `render_frame` at
    /// `Rgba8Unorm` (the to_view ↔ read-back identity is pinned separately).
    #[test]
    fn glyph_to_view_non_glyph_shape_falls_back_to_orb() {
        let Some(renderer) =
            require_or_skip_renderer("glyph_to_view_non_glyph_shape_falls_back_to_orb")
        else {
            return;
        };
        let (w, h) = (40u32, 28u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();
        let opts = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Mid);
        let via_orb = renderer.render_frame(&clusters, &opts, 0.5);
        let via_glyph_entry = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_frame_glyph_to_view(&clusters, &opts, 0.5, view, format);
        });
        assert!(
            lit_vs_bg(&via_orb, opts.background, 8) > 0,
            "circle fallback reference must have lit pixels"
        );
        assert_eq!(
            via_glyph_entry.as_raw(),
            via_orb.as_raw(),
            "glyph to_view on an Orb shape must fall back to the plain orb path byte-for-byte"
        );
    }

    /// #229 (B2) / #235: `render_frame_glyph_to_view` with an unknown char (no SDF)
    /// must draw the background only — the "draw nothing for tofu" contract. The
    /// background-only pack routes through the plain orb pipeline (a zero-orb single
    /// pass), exactly like the read-back variant.
    #[test]
    fn glyph_to_view_unknown_char_background_only() {
        let Some(renderer) =
            require_or_skip_fresh_renderer("glyph_to_view_unknown_char_background_only")
        else {
            return;
        };
        let (w, h) = (48u32, 40u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();
        let mut opts = glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true);
        opts.shape = OrbShape::Glyph {
            ch: '\u{1F355}', // pizza — not in Noto Sans Symbols 2
            font: crate::glyph::GlyphFontId::NotoSymbols2,
        };
        let img = readback_via_view(&renderer, w, h, format, |view| {
            renderer.render_frame_glyph_to_view(&clusters, &opts, 0.3, view, format);
        });
        let lit = lit_vs_bg(&img, opts.background, 1);
        assert_eq!(
            lit, 0,
            "unknown glyph via to_view must paint background only, got {lit} non-bg pixels"
        );
    }

    /// #229 (B3): `render_frame_image_to_view` on a non-Image shape must fall back
    /// to the orb to_view path (the call is total) — byte-identical to
    /// `render_frame` at `Rgba8Unorm`, mirroring the read-back variant's contract.
    #[test]
    fn image_to_view_non_image_shape_falls_back_to_orb() {
        let Some(renderer) =
            require_or_skip_renderer("image_to_view_non_image_shape_falls_back_to_orb")
        else {
            return;
        };
        let (w, h) = (40u32, 28u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();
        let mut opts = image_opts(w, h);
        opts.shape = OrbShape::Orb;
        let via_orb = renderer.render_frame(&clusters, &opts, 0.5);
        let via_image_entry = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_frame_image_to_view(&clusters, &opts, 0.5, view, format);
        });
        assert_eq!(
            via_image_entry.as_raw(),
            via_orb.as_raw(),
            "image to_view on an Orb shape must fall back to the plain orb path byte-for-byte"
        );
    }

    /// #229 (B4): an empty (all-zero) image SDF through
    /// `render_frame_image_to_view` must yield a background-only frame
    /// ("draw nothing" contract, no panic), like the read-back variant.
    #[test]
    fn image_to_view_empty_sdf_background_only() {
        let Some(renderer) = require_or_skip_renderer("image_to_view_empty_sdf_background_only")
        else {
            return;
        };
        let (w, h) = (64u32, 64u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();
        let opts = AnimateOptions {
            shape: OrbShape::Image {
                sdf: std::sync::Arc::from(vec![0u8; 256 * 256]),
                size: 256,
            },
            ..glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true)
        };
        let img = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_frame_image_to_view(&clusters, &opts, 0.0, view, format);
        });
        let lit = lit_vs_bg(&img, opts.background, 1);
        assert_eq!(
            lit, 0,
            "empty image SDF via to_view must paint no foreground pixels"
        );
    }

    /// #229 (B5): `render_frame_aquarelle_to_view` on a non-Aquarelle shape must
    /// fall back to the orb to_view path (the call is total) — byte-identical
    /// to `render_frame` at `Rgba8Unorm`, mirroring the read-back variant.
    #[test]
    fn aquarelle_to_view_non_aquarelle_shape_falls_back_to_orb() {
        let Some(renderer) =
            require_or_skip_renderer("aquarelle_to_view_non_aquarelle_shape_falls_back_to_orb")
        else {
            return;
        };
        let (w, h) = (40u32, 28u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();
        let mut opts = aquarelle_opts(w, h, AquarelleParams::default());
        opts.shape = OrbShape::Orb;
        let via_orb = renderer.render_frame(&clusters, &opts, 0.5);
        let via_aqua_entry = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_frame_aquarelle_to_view(&clusters, &opts, 0.5, view, format);
        });
        assert_eq!(
            via_aqua_entry.as_raw(),
            via_orb.as_raw(),
            "aquarelle to_view on an Orb shape must fall back to the plain orb path byte-for-byte"
        );
    }

    /// #229 (D1): read-back and to_view renders racing on the *shared* renderer
    /// must each match their solo render byte-for-byte — the `render_guard`
    /// serialization covers both paths (they share the grow-only orb texture and
    /// the bleed intermediates), so no panic / validation error / cross-thread
    /// aliasing may occur. Two threads use the read-back entry points (orb /
    /// Glyph) and two use the to_view entry points at `Rgba8Unorm`; oracles are
    /// uncontended solo renders on the same renderer (rendering is deterministic
    /// regardless of cache state, pinned by the determinism / leak tests, so any
    /// mismatch here is a concurrency artifact).
    #[test]
    fn shared_gpu_concurrent_readback_and_to_view_no_aliasing() {
        let Some(renderer) =
            require_or_skip_renderer("shared_gpu_concurrent_readback_and_to_view_no_aliasing")
        else {
            return;
        };
        let (w, h) = (72u32, 56u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();
        let circle = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let glyph = glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true);

        // Solo (uncontended) oracles on the shared renderer. to_view ↔ read-back
        // byte identity at Rgba8Unorm is pinned separately, so the read-back
        // frames also serve as the to_view threads' oracles.
        let oracle_circle = renderer.render_frame(&clusters, &circle, 0.3);
        let oracle_glyph = renderer.render_frame_glyph(&clusters, &glyph, 0.3);

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            // Read-back legs.
            handles.push(scope.spawn(|| {
                for _ in 0..3 {
                    let img = renderer.render_frame(&clusters, &circle, 0.3);
                    assert_eq!(
                        oracle_circle.as_raw(),
                        img.as_raw(),
                        "concurrent circle read-back must match its solo render"
                    );
                }
            }));
            handles.push(scope.spawn(|| {
                for _ in 0..3 {
                    let img = renderer.render_frame_glyph(&clusters, &glyph, 0.3);
                    assert_eq!(
                        oracle_glyph.as_raw(),
                        img.as_raw(),
                        "concurrent glyph read-back must match its solo render"
                    );
                }
            }));
            // to_view legs (each iteration draws into its own private texture).
            handles.push(scope.spawn(|| {
                for _ in 0..3 {
                    let img = readback_via_view(renderer, w, h, format, |view| {
                        renderer.render_frame_to_view(&clusters, &circle, 0.3, view, format);
                    });
                    assert_eq!(
                        oracle_circle.as_raw(),
                        img.as_raw(),
                        "concurrent circle to_view must match the solo read-back render"
                    );
                }
            }));
            handles.push(scope.spawn(|| {
                for _ in 0..3 {
                    let img = readback_via_view(renderer, w, h, format, |view| {
                        renderer.render_frame_glyph_to_view(&clusters, &glyph, 0.3, view, format);
                    });
                    assert_eq!(
                        oracle_glyph.as_raw(),
                        img.as_raw(),
                        "concurrent glyph to_view must match the solo read-back render"
                    );
                }
            }));
            for handle in handles {
                handle
                    .join()
                    .expect("concurrent read-back/to_view thread panicked");
            }
        });
        eprintln!("concurrent read-back + to_view: all threads matched their solo oracle");
    }

    /// #229 (D2): concurrent glyph to_view renders with *mixed target formats*
    /// (`Rgba8Unorm` and `Bgra8Unorm` racing on the shared renderer) must each
    /// match their same-format solo render byte-for-byte — the per-format
    /// `(shader, format)` pipeline-cache and compose-cache grows race here, and a
    /// wrong-format pipeline pick would fail validation or come back
    /// channel-swapped. Oracles are uncontended solo to_view renders on the same
    /// renderer, one per format.
    #[test]
    fn shared_gpu_concurrent_glyph_to_view_mixed_formats() {
        let Some(renderer) =
            require_or_skip_renderer("shared_gpu_concurrent_glyph_to_view_mixed_formats")
        else {
            return;
        };
        let (w, h) = (72u32, 56u32);
        let clusters = sample_clusters();
        let glyph = glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true);

        let render_to = |format: wgpu::TextureFormat| {
            readback_via_view(renderer, w, h, format, |view| {
                renderer.render_frame_glyph_to_view(&clusters, &glyph, 0.3, view, format);
            })
        };
        // Solo (uncontended) per-format oracles; raw bytes (no channel reorder),
        // so each thread compares within its own format's byte order.
        let oracle_rgba = render_to(wgpu::TextureFormat::Rgba8Unorm);
        let oracle_bgra = render_to(wgpu::TextureFormat::Bgra8Unorm);
        assert!(
            lit_vs_bg(&oracle_rgba, glyph.background, 8) > 0,
            "glyph oracle must have lit pixels"
        );

        let formats = [
            (wgpu::TextureFormat::Rgba8Unorm, &oracle_rgba),
            (wgpu::TextureFormat::Bgra8Unorm, &oracle_bgra),
            (wgpu::TextureFormat::Rgba8Unorm, &oracle_rgba),
            (wgpu::TextureFormat::Bgra8Unorm, &oracle_bgra),
        ];
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for &(format, oracle) in &formats {
                let clusters = &clusters;
                let glyph = &glyph;
                handles.push(scope.spawn(move || {
                    for _ in 0..3 {
                        let img = readback_via_view(renderer, w, h, format, |view| {
                            renderer.render_frame_glyph_to_view(clusters, glyph, 0.3, view, format);
                        });
                        assert_eq!(
                            oracle.as_raw(),
                            img.as_raw(),
                            "concurrent glyph to_view ({format:?}) must match its same-format solo render"
                        );
                    }
                }));
            }
            for handle in handles {
                handle
                    .join()
                    .expect("concurrent mixed-format to_view thread panicked");
            }
        });
        eprintln!("concurrent mixed-format glyph to_view: all threads matched their solo oracle");
    }

    // ---- #230: from_device_queue construction seam ----

    /// Bring up a wgpu device / queue *outside* `GpuRenderer`, the way the
    /// browser path (orber-wasm `gpu_init`, #230) does for its surface-compatible
    /// adapter. Mirrors `new_async`'s descriptor choices so `from_device_queue`
    /// is the only variable under test. Retries like
    /// `require_or_skip_fresh_renderer` because this single-instance bring-up can
    /// transiently race the shared context's; a missing adapter then panics under
    /// `ORBER_REQUIRE_GPU=1` / skips otherwise.
    fn require_or_skip_external_device_queue(
        what: &str,
    ) -> Option<(wgpu::Device, wgpu::Queue, String)> {
        async fn bring_up() -> Option<(wgpu::Device, wgpu::Queue, String)> {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .ok()?;
            let adapter_name = adapter.get_info().name;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("orber-gpu-device-test-external"),
                    ..Default::default()
                })
                .await
                .ok()?;
            Some((device, queue, adapter_name))
        }
        for _ in 0..3 {
            if let Some(t) = pollster::block_on(bring_up()) {
                return Some(t);
            }
        }
        require_gpu_or_panic(what);
        None
    }

    /// #230 (B1): a renderer built around an *externally created* device / queue
    /// via `from_device_queue` must render identically to one built through the
    /// pre-existing `new()` bring-up. The refactor extracted the shared setup
    /// (glyph sampler, caches) out of `new_async` into `from_device_queue`, so
    /// both constructions must be the same renderer behaviorally — compared
    /// within the suite-wide ±2/channel contract (same default adapter on one
    /// machine, so byte equality is expected, but the parity tolerance is the
    /// established convention).
    #[test]
    fn from_device_queue_renderer_matches_new_bring_up() {
        let Some(via_new) =
            require_or_skip_renderer("from_device_queue_renderer_matches_new_bring_up")
        else {
            return;
        };
        let Some((device, queue, adapter_name)) = require_or_skip_external_device_queue(
            "from_device_queue_renderer_matches_new_bring_up (external leg)",
        ) else {
            return;
        };
        let via_external = GpuRenderer::from_device_queue(device, queue, adapter_name);
        let clusters = sample_clusters();
        let opts = orb_opts(40, 28, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let frame_new = via_new.render_frame(&clusters, &opts, 0.4);
        let frame_external = via_external.render_frame(&clusters, &opts, 0.4);
        let max_diff = assert_within_tolerance(
            &frame_new,
            &frame_external,
            "from_device_queue construction vs new() bring-up",
        );
        // The frame must actually contain orbs, so the parity isn't trivially
        // satisfied by two background-only frames.
        let lit = lit_vs_bg(&frame_external, opts.background, 2);
        assert!(
            lit > 0,
            "from_device_queue renderer must draw orbs (not a background-only frame)"
        );
        eprintln!("from_device_queue vs new(): max per-channel diff = {max_diff}, lit = {lit}");
    }

    /// #230 (B2): `from_device_queue` must hand the caller-supplied adapter name
    /// through to `adapter_name()` verbatim — the browser path surfaces it as the
    /// `gpu_init` diagnostic (proof the WebGPU path ran), so construction must
    /// not lose or rewrite it.
    #[test]
    fn from_device_queue_passes_adapter_name_through() {
        let Some((device, queue, adapter_name)) =
            require_or_skip_external_device_queue("from_device_queue_passes_adapter_name_through")
        else {
            return;
        };
        assert!(
            !adapter_name.is_empty(),
            "real adapter must report a non-empty name (otherwise the check is vacuous)"
        );
        let expected = adapter_name.clone();
        let renderer = GpuRenderer::from_device_queue(device, queue, adapter_name);
        assert_eq!(
            renderer.adapter_name(),
            expected,
            "adapter_name() must return the exact string passed to from_device_queue"
        );
    }

    /// Build a deterministic single-orb pack centered at (0.5, 0.5) for a
    /// `dim × dim` LR frame, with neutral breath (phi_* = 0 → factors = 1 at t=0),
    /// no rotation (base_angle / rot_speed = 0, header glyph_rotate = 0), Soft
    /// style, white. `shape_id` selects orb (0.0) vs SDF (1.0). The position math
    /// is exact for direction LR at t=0, so the silhouette lands centered
    /// regardless of seed — making the SDF-shape geometry tests position-stable.
    fn centered_single_orb_pack(dim: u32, bg: [u8; 4], shape_id: f32) -> Vec<f32> {
        let base_radius_unit = (dim as f32) * 0.25; // orb_size = 1.0
        let weight = 1.0f32;
        let r_pixels_max = base_radius_unit * weight.sqrt() * BREATH_RADIUS_MAX_FACTOR;
        let r_norm = r_pixels_max / dim as f32; // progress_axis = width for LR
        let extent = 1.0 + 2.0 * r_norm;
        // nx = phase*extent - r_norm = 0.5  ⇒ phase = (0.5 + r_norm) / extent.
        let phase = (0.5 + r_norm) / extent;
        let mut pack = pack_render_data_for_webgl(
            &[cluster([255, 255, 255], 0.5, 0.5, weight)],
            bg,
            base_radius_unit,
            0.5, // base_blur
            0.0, // direction = LR
            MotionSpeed::Slow.cycle_count() as f32,
            7,
            1,   // n_orbs
            1.0, // alpha_mul
            shape_id,
            false, // glyph_rotate OFF → angle = base_angle = 0
            0.5,
        );
        pack[11] = 0.0; // header glyph_rotate OFF (defensive)
        let off = HEADER_WORDS;
        pack[off + 4] = phase; // phase → centers nx at 0.5
        pack[off + 5] = 0.0; // phi_radius → radius_factor = 1
        pack[off + 6] = 0.0; // phi_blur
        pack[off + 7] = 0.0; // phi_opacity
        pack[off + 8] = 0.5; // cross_axis → ny = 0.5
        pack[off + 9] = 1.0; // style_bit = Soft
        pack[off + 11] = 0.0; // base_angle = 0 (upright)
        pack[off + 12] = 0.0; // rot_speed_signed = 0
        pack
    }

    /// Render a centered single-orb SDF frame for `ch` (glyph) on a `dim × dim`
    /// frame, sharing [`centered_single_orb_pack`] so the silhouette lands centered
    /// regardless of seed. Used by the #235 silhouette-geometry tests.
    fn render_centered_glyph(renderer: &GpuRenderer, ch: char, dim: u32, bg: [u8; 4]) -> RgbaImage {
        let frame_radius = (dim as f32) * 0.25 * BREATH_RADIUS_MAX_FACTOR;
        let (sdf, sdf_size) = crate::glyph::cached_glyph_sdf_for_radius(
            crate::glyph::GlyphFontId::NotoSymbols2,
            ch,
            frame_radius,
        )
        .expect("bundled NotoSansSymbols2 must have a real SDF for this char");
        let sdf_view = renderer.upload_glyph_sdf(ch, sdf_size, &sdf);
        renderer.render_packed_inner(
            &centered_single_orb_pack(dim, bg, 1.0),
            dim,
            dim,
            0.0,
            Some(GlyphBindings {
                sdf_view: &sdf_view,
                size: sdf_size,
            }),
        )
    }

    /// True when pixel `(x,y)` differs from the (opaque) background by > 8 on any
    /// RGB channel — "this pixel carries silhouette fill".
    fn px_lit(img: &RgbaImage, bg: [u8; 4], x: u32, y: u32) -> bool {
        (0..3).any(|c| img.get_pixel(x, y).0[c].abs_diff(bg[c]) > 8)
    }

    // ---- #235: unified orb mechanism — silhouette geometry (no bleed/halo) ----

    /// #235: a glyph silhouette must **not** leave a halo ring outside the orb body.
    /// This is the direct inverse of the deleted `gpu_glyph_bleed_produces_halo_ring`
    /// test: the old bleed/halo 2nd pass spread fill into a ring several px outside
    /// the body; the unified orb mechanism has no such pass, so a `●` glyph (the
    /// densest silhouette) rendered at the **same** centered position as a plain orb
    /// must never light a pixel more than a few px outside the orb's footprint.
    ///
    /// The position is pinned by [`centered_single_orb_pack`] (not RNG), so orb and
    /// glyph share an exact center; any glyph-lit pixel far from every orb-lit pixel
    /// would be halo leakage. Measured on Apple A18 Pro: 0 such pixels even at the
    /// strict 2 px radius, so the loose 5 px threshold here has wide headroom while
    /// staying robust to per-GPU edge antialiasing.
    #[test]
    fn gpu_glyph_no_halo_ring_around_silhouette() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_no_halo_ring_around_silhouette")
        else {
            return;
        };
        let dim = 96u32;
        let bg = [10u8, 12, 20, 255];
        let bullet = render_centered_glyph(renderer, '\u{25CF}', dim, bg);
        let orb = renderer.render_packed_inner(
            &centered_single_orb_pack(dim, bg, 0.0),
            dim,
            dim,
            0.0,
            None,
        );
        let orb_lit: Vec<(i32, i32)> = (0..dim)
            .flat_map(|y| (0..dim).map(move |x| (x, y)))
            .filter(|&(x, y)| px_lit(&orb, bg, x, y))
            .map(|(x, y)| (x as i32, y as i32))
            .collect();
        assert!(
            !orb_lit.is_empty(),
            "the centered orb must actually paint (otherwise the halo check is vacuous)"
        );
        // Chebyshev distance > 5 px from EVERY orb-lit pixel ⇒ detached/expanded
        // halo. The unified mechanism must produce zero such glyph pixels.
        let k = 5i32;
        let halo = (0..dim)
            .flat_map(|y| (0..dim).map(move |x| (x, y)))
            .filter(|&(x, y)| px_lit(&bullet, bg, x, y))
            .filter(|&(x, y)| {
                !orb_lit
                    .iter()
                    .any(|&(ox, oy)| (ox - x as i32).abs() <= k && (oy - y as i32).abs() <= k)
            })
            .count();
        assert_eq!(
            halo, 0,
            "● glyph must not light pixels more than {k}px from the orb body \
             (no bleed/halo since #235); found {halo} halo pixels"
        );
        eprintln!("gpu glyph no-halo: 0 halo pixels beyond {k}px of the orb body");
    }

    /// #235 acceptance: a `●` glyph "looks like an orb". Rendered at the **same**
    /// centered position as a plain orb (via [`centered_single_orb_pack`]), the two
    /// lit footprints must overlap heavily.
    ///
    /// A per-channel pixel diff is **not** a faithful metric here: the `●` glyph is
    /// a font silhouette scaled to fit the em-square, so it is a few pixels larger /
    /// softer at the edge than the analytic circle, giving a large max per-channel
    /// diff (≈146 on A18 Pro) concentrated in the soft edge band — that would force
    /// a meaningless threshold near 255. Instead we assert the footprints'
    /// intersection-over-union, which captures "same blob, same place" directly.
    /// Measured IoU on A18 Pro = 0.957; the `>= 0.85` floor leaves ample headroom
    /// for per-GPU edge antialiasing without going slack.
    #[test]
    fn gpu_glyph_bullet_approx_equals_orb() {
        let Some(renderer) = require_or_skip_renderer("gpu_glyph_bullet_approx_equals_orb") else {
            return;
        };
        let dim = 96u32;
        let bg = [10u8, 12, 20, 255];
        let bullet = render_centered_glyph(renderer, '\u{25CF}', dim, bg);
        let orb = renderer.render_packed_inner(
            &centered_single_orb_pack(dim, bg, 0.0),
            dim,
            dim,
            0.0,
            None,
        );
        let (mut inter, mut uni) = (0usize, 0usize);
        for (x, y) in (0..dim).flat_map(|y| (0..dim).map(move |x| (x, y))) {
            let (la, lb) = (px_lit(&orb, bg, x, y), px_lit(&bullet, bg, x, y));
            if la || lb {
                uni += 1;
            }
            if la && lb {
                inter += 1;
            }
        }
        assert!(uni > 0, "neither orb nor glyph painted — vacuous IoU");
        let iou = inter as f32 / uni as f32;
        assert!(
            iou >= 0.85,
            "● glyph and orb footprints must overlap heavily (IoU >= 0.85); got {iou:.3} \
             (inter={inter}, union={uni})"
        );
        eprintln!("gpu bullet≈orb: IoU = {iou:.3} (inter={inter}, union={uni})");
    }

    /// #235 acceptance: a `▲` glyph keeps its **triangular** silhouette through the
    /// orb blur — it does not round into a disk. Rendered centered (apex up) via
    /// [`centered_single_orb_pack`], the lit silhouette must be narrow at the top
    /// (apex) and widest near the bottom (base): the widest scan row lies below the
    /// vertical center, and the apex row is far narrower than the base.
    ///
    /// A disk would be widest at its vertical center and symmetric top↔bottom, so
    /// the "widest row below center" + "apex ≪ base" pair is what a round-off
    /// regression (e.g. accidentally feeding the analytic circle distance) would
    /// break. Measured on A18 Pro: ybbox 31..66 (mid 48), widest row y=66, apex
    /// width 2, base width 40.
    #[test]
    fn gpu_triangle_silhouette_preserved_not_round() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_triangle_silhouette_preserved_not_round")
        else {
            return;
        };
        let dim = 96u32;
        let bg = [10u8, 12, 20, 255];
        let img = render_centered_glyph(renderer, '\u{25B2}', dim, bg);
        let row_w = |y: u32| -> usize { (0..dim).filter(|&x| px_lit(&img, bg, x, y)).count() };
        let (mut miny, mut maxy) = (dim, 0u32);
        for y in 0..dim {
            if row_w(y) > 0 {
                miny = miny.min(y);
                maxy = maxy.max(y);
            }
        }
        assert!(
            maxy > miny,
            "▲ glyph must paint a non-degenerate silhouette"
        );
        // Widest scan row.
        let (mut widest_y, mut widest_w) = (miny, 0usize);
        for y in miny..=maxy {
            let w = row_w(y);
            if w > widest_w {
                widest_w = w;
                widest_y = y;
            }
        }
        let mid = (miny + maxy) / 2;
        assert!(
            widest_y > mid,
            "▲ (apex up) must be widest below its vertical center, not at it (a disk \
             would be widest at center): widest row y={widest_y}, bbox {miny}..{maxy} (mid {mid})"
        );
        // Apex (top) must be far narrower than the base (bottom).
        let apex = row_w(miny + 1).max(row_w(miny));
        let base = row_w(maxy - 1).max(row_w(maxy));
        assert!(
            base >= apex * 3 && apex < widest_w / 2,
            "▲ apex must be far narrower than its base (apex={apex}, base={base}, widest={widest_w}) \
             — a rounded silhouette would have apex ≈ base"
        );
        eprintln!(
            "gpu triangle preserved: bbox {miny}..{maxy}, widest y={widest_y} (w={widest_w}), apex={apex}, base={base}"
        );
    }

    /// #235: the SDF distance source maps an exact silhouette edge to `r = 1.0`,
    /// which `falloff_curve` makes transparent — so a known image silhouette must be
    /// lit **inside**, transparent **well outside**, and have a clear lit→unlit
    /// boundary (the edge), not a frame-wide smear. Uses the synthetic centered
    /// square [`test_image_shape`] (a 32 px white square in a 64 px field) so the
    /// test does not depend on font assets, rendered centered via
    /// [`centered_single_orb_pack`].
    ///
    /// Measured on A18 Pro: the center pixel and a band around it are lit; the frame
    /// corners are transparent; a center horizontal scan has a single bounded lit
    /// run (≈31..64 on a 96 px frame), proving the edge is honored rather than the
    /// fill bleeding to the borders.
    #[test]
    fn gpu_sdf_edge_r_eq_one_is_transparent() {
        let Some(renderer) = require_or_skip_renderer("gpu_sdf_edge_r_eq_one_is_transparent")
        else {
            return;
        };
        let dim = 96u32;
        let bg = [10u8, 12, 20, 255];
        let shape = test_image_shape();
        let (sdf, sdf_size) = match &shape {
            OrbShape::Image { sdf, size } => (sdf.clone(), *size),
            _ => unreachable!("test_image_shape is always an Image"),
        };
        let sdf_view = renderer.upload_image_sdf(sdf_size, &sdf);
        let img = renderer.render_packed_inner(
            &centered_single_orb_pack(dim, bg, 1.0),
            dim,
            dim,
            0.0,
            Some(GlyphBindings {
                sdf_view: &sdf_view,
                size: sdf_size,
            }),
        );
        let cx = dim / 2;
        let cy = dim / 2;
        // Inside the silhouette (center) must be lit.
        assert!(
            px_lit(&img, bg, cx, cy),
            "the silhouette interior (center) must be lit"
        );
        // Well outside (every corner) must be transparent — the edge (r >= 1) is not
        // smeared to the frame borders.
        for (x, y) in [(2u32, 2u32), (dim - 3, 2), (2, dim - 3), (dim - 3, dim - 3)] {
            assert!(
                !px_lit(&img, bg, x, y),
                "the frame corner ({x},{y}) must be transparent (background only) — \
                 the SDF edge must clip the fill, not bleed to the borders"
            );
        }
        // The center horizontal scan must have a single bounded lit run with a clear
        // lit→unlit edge on both sides (not a frame-wide fill).
        let lit_xs: Vec<u32> = (0..dim).filter(|&x| px_lit(&img, bg, x, cy)).collect();
        let (first, last) = (
            *lit_xs.first().expect("center row must have lit pixels"),
            *lit_xs.last().unwrap(),
        );
        assert!(
            first > 0 && last < dim - 1,
            "the center row must have a transparent margin on both sides (edge honored): \
             lit run {first}..{last} on a {dim}px frame"
        );
        assert_eq!(
            lit_xs.len(),
            (last - first + 1) as usize,
            "the center row's lit pixels must form one contiguous run (a solid \
             silhouette interior, not scattered specks): {first}..{last} with {} lit",
            lit_xs.len()
        );
        eprintln!("gpu sdf edge: center lit, corners transparent, center run {first}..{last}");
    }
}
