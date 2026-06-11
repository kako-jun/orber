//! wgpu (Rust + WGSL) production render path — orber #207 Phase 0–1c, #225, #229,
//! #235.
//!
//! [`GpuRenderer`] is the headless renderer and — since #225 — the **only**
//! renderer (the CPU pixel path and the CPU↔GPU parity oracle were purged). Since
//! #235 the orb mechanism is the **only** mechanism for orb / glyph / image: one
//! unified WGSL template ([`orb.wgsl`](../src/orb.wgsl)) is compiled into two
//! variants — the plain orb (analytic circle distance) and the SDF orb (glyph /
//! image: the same orb math fed a different silhouette via an SDF sample; no
//! bleed/halo). Since #242 the shared compositing is the straight-alpha float
//! Source-Over ported 1:1 from the legacy WebGL shader. The optional additive
//! watercolor bleed layer ([`crate::animate::AquaBleedConfig`]) rides on top of any
//! shape via the same template. All three shapes (Orb, Glyph, Image) render on the
//! GPU; the CLI renders every PNG / video / variation frame through it.
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
//!   [`pack_render_data`] (which itself never applies saturation,
//!   because it is shared with the Web wasm path, which has no saturation knob);
//! - **count up to [`MAX_ORB_COUNT`] (1024)**: per-orb data is uploaded as a
//!   **data-texture** (`Rgba32Float`, read with `textureLoad`) — not a fixed-size
//!   uniform array — so the WGSL has **no 64-orb cap** (#210 Phase 1a). (The 64
//!   limit only ever applied to the legacy fixed-uniform-array renderer —
//!   `crates/wasm/src/lib.rs::GL_RENDERER_MAX_ORBS` — do not re-sync a 64 cap onto
//!   this path.)
//!
//! Input is still images only: the per-orb colors come straight from the
//! extracted `clusters` and stay fixed for the whole loop. Animation over `t`
//! (position wrap, 3-axis breath, procedural wobble #260 / hue pulse #261) is
//! produced entirely by the unified `orb.wgsl`. The pack `off+13` slot is always
//! zeroed (`misc.w` unused).
//!
//! ## Compositing contract
//!
//! Orbs are composited in **straight sRGB float space** with a plain float
//! Source-Over — the exact arithmetic of the legacy WebGL fragment shader,
//! adopted as the reference by the #242 ruling
//! (the previous Skia-lowp u8-quantize/premultiply emulation darkened the
//! output vs. the WebGL look). The GPU path:
//!
//! - renders into an [`wgpu::TextureFormat::Rgba8Unorm`] target (NOT `*Srgb`), so
//!   no sRGB↔linear conversion happens — the shader's float blend maps to bytes
//!   by `round(value * 255)`, the same quantization the WebGL canvas applied;
//! - feeds the shader the per-orb data the Web wasm path also uses
//!   ([`crate::animate::pack_render_data`]): the parameter arithmetic is
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

use crate::animate::{pack_render_data, AnimateOptions, MotionDirection, MAX_ORB_COUNT};
use crate::cluster::Cluster;
use crate::orb::adjust_saturation_pub;

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

/// Header words / per-orb words in the `pack_render_data` layout.
/// Kept in sync with that function (header 16 words, per-orb 16 words).
const HEADER_WORDS: usize = 16;
const PER_ORB_WORDS: usize = 16;

/// The unified orb WGSL template (`orb.wgsl`, #235 / #242). The orb mechanism is
/// now the **only** mechanism for orb / glyph / image: each pixel's normalized
/// "distance from the shape" feeds the same 3-axis breath, `falloff_curve`, and —
/// since #242 — the **straight-alpha float Source-Over** compositing ported 1:1
/// from the legacy WebGL fragment shader. Only the
/// **DISTANCE SOURCE** block differs per shape, so "feeding the orb a different
/// silhouette" is the literal implementation.
///
/// The template carries `//!ORB_*` markers that [`orb_wgsl`] / [`orb_sdf_wgsl`]
/// substitute to generate the two variants (Rust-side string composition, as the
/// plan recommends): the orb variant injects the analytic circle distance and
/// **no** SDF bindings; the SDF variant adds the `R8Unorm` SDF texture + bilinear
/// sampler (bindings 2/3), reads the per-orb rotation texel (x=3), and computes
/// `r` from the SDF sample. Both variants share the falloff / compositing, so the
/// #242 algorithm switch applies to every shape the template serves.
const ORB_WGSL_TEMPLATE: &str = include_str!("orb.wgsl");

/// The orb (analytic circle) variant of [`ORB_WGSL_TEMPLATE`]. No SDF bindings,
/// no rotation; the DISTANCE SOURCE block inlines the analytic circle distance
/// (`dist = distance(...); r = dist / radius`) — the same two lines the WebGL
/// orb arm uses, so the orb output matches the legacy WebGL look (#242).
/// Built once (`OnceLock`, MSRV 1.78 — `LazyLock` is 1.80); the resulting
/// `&'static str` doubles as the stable pipeline-cache key. The additive bleed
/// geometry (`//!ORB_AQUA_BLEED_GEOM`) is the single continuous space-blur; when
/// no aqua params are set the layer is structurally inert (byte-identical to plain
/// orb).
///
/// The watercolor-bleed constants + functions (`AQUA_*`, `aqua_seed_dir`,
/// `blurred_coverage`, `aqua_character`) are no longer inline: they come from the
/// shared `aquarelle::AQUA_BLEED_WGSL` fragment, substituted at the
/// `//!ORB_AQUA_BLEED_SHARED` marker (orber#250 Phase 2). The fragment is byte-equivalent
/// to the previous inline copy, so the rendered output is unchanged.
/// にじみ（空間ブラー）の multi-tap 数の既定値（#265）。`aquarelle` 共有 WGSL は
/// 静止画 PoC 前提で `AQUA_BLUR_TAPS = 48u`（コメント「重くてよい」）を採るが、orber は
/// #253 でにじみを常時オン化し、24fps×8s×4本=768 フレームの動画とモバイル GPU に
/// 乗せた結果、48 タップの一撃描画がモバイル GPU のウォッチドッグを超えて
/// **デバイスロスト→タブクラッシュ**を起こした（ゲーミングスマホでも落ちた）。
/// ぼかし（falloff）が見た目を支配し、にじみ（タップ平均の質）の寄与は小さい
/// （kako-jun 確認）ので、タップを大幅に削って軽量化する。値は強め（最広ブラー）の
/// 数値計測で確定した床。CLI `--bleed-taps` で上書き可（既定はこの値）。
///
/// #265 当初 8 → kako-jun「もっと下げていい」＋個数「多め」の重さ対策で **5** に再削減。
/// 標準 count・強めでも 48 比 目視差画素 0.45% / PSNR 52dB でほぼ識別不能、t4 で初めて
/// 3% にザラつくのでその手前。原 48 比で約 10 倍軽い。
const DEFAULT_AQUA_TAPS: u32 = 5;

/// `aquarelle::AQUA_BLEED_WGSL` が宣言するタップ数 const の行（#265）。orber は共有
/// crate 本体を触らず、連結後の WGSL 文字列でこの行のタップ数だけ差し替える
/// （additive / blueprinter は 48 のまま＝爆発半径ゼロ）。crate 側で宣言が変わると
/// この置換は黙って no-op になるので、[`substitute_aqua_taps`] が debug_assert で検出する。
const AQUA_TAPS_DECL_48: &str = "const AQUA_BLUR_TAPS: u32 = 48u;";

/// 連結済み WGSL のにじみタップ数を `taps` に差し替える（#265）。`aquarelle` を
/// `//!ORB_AQUA_BLEED_SHARED` に展開した**後**の文字列に対して呼ぶこと（展開前は
/// 宣言行が無い）。`taps == 48` のときは恒等（48→48）。
fn substitute_aqua_taps(src: String, taps: u32) -> String {
    debug_assert!(
        src.contains(AQUA_TAPS_DECL_48),
        "aquarelle AQUA_BLUR_TAPS declaration not found; the crate changed it — update AQUA_TAPS_DECL_48 (#265)"
    );
    src.replace(
        AQUA_TAPS_DECL_48,
        &format!("const AQUA_BLUR_TAPS: u32 = {taps}u;"),
    )
}

/// orb variant の WGSL を `taps` タップで組む（[`orb_wgsl`] の汎用版・#265）。
fn build_orb_wgsl(taps: u32) -> String {
    let s = ORB_WGSL_TEMPLATE
        .replace("//!ORB_EXTRA_BINDINGS", ORB_EXTRA_BINDINGS_NONE)
        .replace("//!ORB_LOAD", ORB_LOAD_ORB)
        .replace("//!ORB_HELPERS", ORB_HELPERS_NONE)
        .replace("//!ORB_COVERAGE", ORB_COVERAGE_CIRCLE)
        .replace("//!ORB_ANGLE", ORB_ANGLE_NONE)
        .replace("//!ORB_AQUA_BLEED_SHARED", aquarelle::AQUA_BLEED_WGSL)
        .replace("//!ORB_AQUA_BLEED_GEOM", aqua_bleed_geom());
    substitute_aqua_taps(s, taps)
}

/// SDF variant の WGSL を `taps` タップで組む（[`orb_sdf_wgsl`] の汎用版・#265）。
fn build_orb_sdf_wgsl(taps: u32) -> String {
    let s = ORB_WGSL_TEMPLATE
        .replace("//!ORB_EXTRA_BINDINGS", ORB_EXTRA_BINDINGS_SDF)
        .replace("//!ORB_LOAD", ORB_LOAD_ORB_WITH_ROT)
        .replace("//!ORB_HELPERS", ORB_HELPERS_SDF)
        .replace("//!ORB_COVERAGE", ORB_COVERAGE_SDF)
        .replace("//!ORB_ANGLE", ORB_ANGLE_SDF)
        .replace("//!ORB_AQUA_BLEED_SHARED", aquarelle::AQUA_BLEED_WGSL)
        .replace("//!ORB_AQUA_BLEED_GEOM", aqua_bleed_geom());
    substitute_aqua_taps(s, taps)
}

/// 既定タップ（[`DEFAULT_AQUA_TAPS`]）で組んだ orb variant WGSL。#265 以降、本番の
/// render パスはレンダラ構築時の `orb_shader`（`with_aqua_taps` 反映済み）を使うので、
/// この静的版はテンプレート構造を pin するテスト専用。
#[cfg(test)]
fn orb_wgsl() -> &'static str {
    use std::sync::OnceLock;
    static ORB: OnceLock<String> = OnceLock::new();
    ORB.get_or_init(|| build_orb_wgsl(DEFAULT_AQUA_TAPS))
}

/// The SDF (glyph / image, #235) variant of [`ORB_WGSL_TEMPLATE`]. Adds the
/// `R8Unorm` SDF texture + bilinear sampler (bindings 2/3) and the rotation
/// helper, reads the rotation texel, and computes `r` from the SDF sample
/// (rotation applied **before** sampling; `CONTENT_SPAN` clip + `sdf_size` texel
/// remap preserved). The DISTANCE SOURCE is the only difference from the orb
/// variant; the falloff / breath / compositing are the **same** orb math, so
/// glyph / image now blur exactly like orb (no bleed/halo). Built once (`OnceLock`).
///
/// Like [`orb_wgsl`], the watercolor-bleed fragment comes from the shared
/// `aquarelle::AQUA_BLEED_WGSL` const, substituted at `//!ORB_AQUA_BLEED_SHARED`
/// (orber#250 Phase 2; byte-equivalent to the former inline copy).
///
/// [`orb_wgsl`] と同じく、#265 以降は静的既定タップ版＝テスト専用。
#[cfg(test)]
fn orb_sdf_wgsl() -> &'static str {
    use std::sync::OnceLock;
    static SDF: OnceLock<String> = OnceLock::new();
    SDF.get_or_init(|| build_orb_sdf_wgsl(DEFAULT_AQUA_TAPS))
}

/// #239: the WGSL fragment substituted into `//!ORB_AQUA_BLEED_GEOM`. The bleed is
/// a plain Gaussian-blur amount (the silhouette coverage is spatially averaged over
/// a disk of radius ∝ bleed — the star blurs *as a star*, no circle morph). It only
/// defines the blur-radius scale `let`; it does not morph the distance field toward
/// a circle, add a rim/halo/blob, nor warp with noise. The blur path is only reached
/// when `aqua_bleed > 0`, so `bleed == 0` is still byte-identical to plain orb. The
/// #239 Phase 1 redesign dropped the wider-blur Blob A/B variant, leaving this single
/// continuous geometry.
fn aqua_bleed_geom() -> &'static str {
    ORB_AQUA_BLEED_CONTINUOUS
}

/// Continuous bleed: 標準のブラー半径（multi-tap 空間平均の disk 半径スケール 1.0）。
/// `bleed=0` でブラー経路に入らないので消える。距離場を円へモーフしない。
const ORB_AQUA_BLEED_CONTINUOUS: &str = "let aqua_blur_scale = 1.0;";

/// No extra bindings (orb variant): the analytic circle needs only the params
/// uniform (0) and the orb data-texture (1) the template already declares.
const ORB_EXTRA_BINDINGS_NONE: &str = "";

/// SDF variant bindings: the glyph / image SDF (`R8Unorm`, bilinear-filterable)
/// at binding 2 and a filtering sampler at binding 3.
const ORB_EXTRA_BINDINGS_SDF: &str = "\
// glyph/image SDF（R8Unorm, 単一文字 / 単一シルエット）と bilinear sampler。\n\
@group(0) @binding(2) var sdf_tex: texture_2d<f32>;\n\
@group(0) @binding(3) var sdf_samp: sampler;";

/// Orb variant `load_orb`: reads only color / phase / misc (3 texels), matching
/// the old `orb_circle.wgsl` layout (no `rot` read). The load itself is unchanged,
/// but the downstream compositing is no longer the old shader's: #242 replaced it
/// with the old WebGL float straight Source-Over (`composite_straight`).
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

/// Orb variant ANGLE block: no rotation. `angle` is unused by the circle coverage,
/// but the loop passes it to `coverage_at` (shared signature), so define it `0.0`.
/// The orb variant must NOT read `o.rot` (structure pin), so this is the only place
/// `angle` originates and it is a constant here.
const ORB_ANGLE_NONE: &str = "        let angle = 0.0;";

/// SDF variant ANGLE block: the per-orb rotation angle, read from `o.rot` here (the
/// only place the SDF variant touches the rotation texel) so it can be handed to
/// `coverage_at` for **every** tap (plain single-tap and the blur's multi-tap share
/// one rotation). Rotation is applied before SDF sampling, exactly as before.
const ORB_ANGLE_SDF: &str = "        let angle = glyph_rotation_angle(o.rot.x, o.rot.y);";

/// Orb variant `coverage_at`: the analytic circle coverage. `r = distance(sp,center)
/// / radius` (the same circle distance the WebGL orb arm computes), fed to the shared
/// `falloff_curve`. For the plain path `sp == sample_px`, so this is **bit-identical**
/// to the old inlined `dist / radius` → `falloff_curve` body. `angle` is unused (the
/// circle has no orientation). The blur path re-evaluates this at jittered `sp`.
const ORB_COVERAGE_CIRCLE: &str = "\
fn coverage_at(\n\
    style_bit: f32,\n\
    sp: vec2<f32>,\n\
    cx: f32,\n\
    cy: f32,\n\
    radius: f32,\n\
    blur: f32,\n\
    opacity: f32,\n\
    angle: f32,\n\
) -> vec2<f32> {\n\
    let r = distance(sp, vec2<f32>(cx, cy)) / radius;\n\
    return falloff_curve(style_bit, r, blur, opacity);\n\
}";

/// SDF variant `coverage_at` (glyph / image, #235): rotate the sample offset by the
/// per-orb `angle` **before** sampling, map to the SDF UV (CONTENT_SPAN clip + the
/// `sdf_size` texel remap that cancels the sampler's half-texel offset), bilinear
/// sample the signed distance, convert to `r = 1 - signed_unit`, and feed the shared
/// `falloff_curve`. **Out-of-box UVs return coverage 0** (the orb does not cover that
/// sample) — for the plain single tap this is the old `continue` (alpha 0 ⇒ the
/// compositor skips it, byte-identical); for the blur's multi-tap it simply means
/// that tap contributes 0 coverage, so the star's spikes blur *as a star* and dissolve
/// naturally under a strong blur. **No morph toward a circle distance** (the rejected
/// `mix(r, r_circle)` is gone). The form is the only difference from the orb variant.
const ORB_COVERAGE_SDF: &str = "\
fn coverage_at(\n\
    style_bit: f32,\n\
    sp: vec2<f32>,\n\
    cx: f32,\n\
    cy: f32,\n\
    radius: f32,\n\
    blur: f32,\n\
    opacity: f32,\n\
    angle: f32,\n\
) -> vec2<f32> {\n\
    let cos_a = cos(angle);\n\
    let sin_a = sin(angle);\n\
    let dx = sp.x - cx;\n\
    let dy = sp.y - cy;\n\
    let rx = cos_a * dx - sin_a * dy;\n\
    let ry = sin_a * dx + cos_a * dy;\n\
    let u = rx / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;\n\
    let v = ry / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;\n\
    // SDF サンプル箱(square)の外＝この sp は形に覆われない → 被覆 0。距離場を円へ\n\
    // モーフしない（前回 NG を撤去）。ブラーでは箱外タップが 0 を寄せるだけで、星の\n\
    // トゲは星のままぼけ、強ブラーで自然に平均化されて formless 化する。\n\
    if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) {\n\
        return vec2<f32>(0.0, 0.0);\n\
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
    let r = 1.0 - signed_unit;\n\
    return falloff_curve(style_bit, r, blur, opacity);\n\
}";

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
    // row 4: shadow_strength + pad (WGSL uniform structs round up to 16-byte rows)
    /// Thin-shadow strength (#241): rgb darkening factor (0..1) applied in the
    /// outermost fade segment of the shared `falloff_curve`. `0.0` is bit-identical
    /// to the post-#242 (no-shadow) output; `1.0` matches the darkness of the
    /// removed Skia-lowp rgb→0 fade. Read from pack header[13]; every shape on the
    /// unified template (orb / glyph / image) gets it.
    shadow_strength: f32,
    // row 5: additive watercolor bleed params (#239) — the layer's 4 sliders.
    /// #239: にじみ 4 パラメータ（各 0..1）を統一機構の上の加算レイヤーへ流す。
    /// bleed=0 のとき WGSL の watercolor 経路に入らず、出力は plain orb と byte 一致
    /// する（非回帰ゲート）。にじみを使わない描画では `pack_orb_frame` /
    /// `pack_sdf_frame` が 0 を入れるので不変。幾何は唯一の continuous space-blur
    /// （`//!ORB_AQUA_BLEED_GEOM` 置換）なので mode フラグは持たない。
    aqua_bleed: f32,
    aqua_bloom: f32,
    aqua_offset: f32,
    aqua_halo: f32,
    // WGSL rounds the uniform struct up to a 16-byte row boundary; `aqua_halo` ends
    // at byte 84, so pad to 96 to match the shader's expected binding size.
    _pad: [f32; 3],
}

/// One orb as the shaders see it: four `vec4`s mirroring `struct Orb` in
/// `orb.wgsl` (color+weight, phase quartet, misc, rotation). Filled from the
/// `pack_render_data` per-orb words. One `GpuOrb` packs to one row of
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
    /// にじみ（空間ブラー）の multi-tap 数（#265）。既定 [`DEFAULT_AQUA_TAPS`]、
    /// [`Self::with_aqua_taps`] で上書き。シェーダ文字列は構築時に一度だけ組むので
    /// （`orb_shader` / `orb_sdf_shader`）、毎フレームの replace コストは無い。
    aqua_taps: u32,
    /// orb variant の WGSL（`aqua_taps` タップで構築済み）。render パスはこの文字列を
    /// `pipeline()` に渡す。pipeline cache はシェーダ文字列キーなので、タップ数違いは
    /// 自然に別エントリになる。
    orb_shader: String,
    /// SDF variant（glyph / image）の WGSL（`aqua_taps` タップで構築済み）。
    orb_sdf_shader: String,
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
            render_guard: std::sync::Mutex::new(()),
            aqua_taps: DEFAULT_AQUA_TAPS,
            orb_shader: build_orb_wgsl(DEFAULT_AQUA_TAPS),
            orb_sdf_shader: build_orb_sdf_wgsl(DEFAULT_AQUA_TAPS),
        }
    }

    /// にじみ（空間ブラー）の multi-tap 数を上書きする（#265・既定
    /// [`DEFAULT_AQUA_TAPS`]）。`taps` は最低 1 にクランプ。シェーダ文字列を組み直す
    /// ので、最初の render より前に呼ぶこと（builder スタイル）。CLI の `--bleed-taps`
    /// がこの seam を使う。web（wasm）は既定値のまま＝モバイルでも軽い。
    #[must_use]
    pub fn with_aqua_taps(mut self, taps: u32) -> Self {
        let taps = taps.max(1);
        self.aqua_taps = taps;
        self.orb_shader = build_orb_wgsl(taps);
        self.orb_sdf_shader = build_orb_sdf_wgsl(taps);
        self
    }

    /// 現在のにじみタップ数（#265）。CLI の検証 / テスト用。
    #[must_use]
    pub fn aqua_taps(&self) -> u32 {
        self.aqua_taps
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
    /// The per-orb data is computed by [`pack_render_data`] — the same
    /// arithmetic the wasm data-supply pack uses — so the orb positions /
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
    /// here; the caller routes `Glyph` to `render_frame_glyph` and `Image` to
    /// `render_frame_image` (all GPU).
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
        let pack = Self::pack_orb_frame(clusters, opts, width, height, t);
        // #239 PoC: `opts.aqua` rides the orb (no SDF) variant of the unified shader.
        // `None` (the production default) is the plain orb path, byte-identical to
        // pre-#239.
        self.render_packed_inner(&pack, width, height, t, None, opts.aqua)
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
        let pack = Self::pack_orb_frame(clusters, opts, width, height, t);
        // #239 PoC: `opts.aqua` rides the orb variant; `None` is the plain orb path.
        self.render_packed_inner_to_view(&pack, width, height, t, None, opts.aqua, view, format);
    }

    /// Build the plain orb pack buffer for one frame: derive the pack-buffer
    /// scalars exactly as the Web wasm entry (`build_gpu_render_inputs` →
    /// `resolve_frame`) does, reuse [`pack_render_data`] (so the per-orb arithmetic
    /// is never reimplemented), then re-apply per-orb saturation. Shared by the
    /// read-back ([`Self::render_frame`]) and to_view
    /// ([`Self::render_frame_to_view`]) paths.
    fn pack_orb_frame(
        clusters: &[Cluster],
        opts: &AnimateOptions,
        width: u32,
        height: u32,
        _t: f32,
    ) -> Vec<f32> {
        // Still-image input: per-orb colors come straight from the extracted
        // clusters and stay fixed for the whole loop (animation is purely the
        // unified `orb.wgsl`'s job).
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
        // plain orb shader ignores them. Pass orb defaults. shadow_strength (#241)
        // is read by every shape on the unified template, the orb included.
        let mut pack = pack_render_data(
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
            opts.shadow_strength,
        );

        // `pack_render_data` is shared with the Web wasm path and must NOT
        // bake in saturation (the web side has no saturation knob). The native CLI
        // has no separate saturation knob either, so we apply
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
    /// SDF. The per-orb arithmetic reuses [`pack_render_data`] (so
    /// positions / radii / rotation match the Web wasm path), saturation is re-applied
    /// per orb (the native CLI has no separate saturation knob), the glyph SDF is
    /// uploaded as an `R8Unorm` texture, and the SDF orb shader bilinear-samples it.
    ///
    /// # Unified orb mechanism (#235)
    ///
    /// Since #235 the glyph is just a different silhouette fed to the orb mechanism:
    /// the SDF sample becomes the normalized distance `r = 1 - signed_unit`, which
    /// feeds the **same** `falloff_curve` / 3-axis breath / straight-alpha float
    /// Source-Over compositing (#242) the plain orb uses (`orb.wgsl`, SDF variant).
    /// It is **one** pass —
    /// the old aquarelle-derived bleed/halo 2nd pass group is removed, so glyph now
    /// blurs exactly like an orb (a `●` glyph looks like an orb; a `▲` blurs while
    /// keeping its triangular form). "Bleed" is now the領分 of the optional additive
    /// [`crate::animate::AquaBleedConfig`] layer (which rides any shape).
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
        match self.prepare_glyph_frame(clusters, opts, width, height, t) {
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
                opts.aqua,
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
        match self.prepare_glyph_frame(clusters, opts, width, height, t) {
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
                opts.aqua,
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
        t: f32,
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
                clusters, opts, width, height, 0, t,
            ));
        };

        let n_orbs = Self::resolved_orb_count(clusters, opts);
        let pack = Self::pack_sdf_frame(clusters, opts, width, height, n_orbs, t);
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
    /// [`pack_render_data`] and saturation is re-applied per orb, exactly
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
        match self.prepare_image_frame(clusters, opts, width, height, t) {
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
                opts.aqua,
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
        match self.prepare_image_frame(clusters, opts, width, height, t) {
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
                opts.aqua,
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
        t: f32,
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
                clusters, opts, width, height, 0, t,
            ));
        }

        let n_orbs = Self::resolved_orb_count(clusters, opts);
        let pack = Self::pack_sdf_frame(clusters, opts, width, height, n_orbs, t);
        let sdf_view = self.upload_image_sdf(sdf_size, &sdf);
        SdfFramePack::Sdf {
            pack,
            sdf_view,
            size: sdf_size,
        }
    }

    /// Resolved orb count: `count.unwrap_or(clusters.len())` clamped to
    /// [`MAX_ORB_COUNT`], at least 1 if there are clusters (mirrors the count
    /// resolution in [`pack_render_data`]). Shared by every pack builder.
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
        _t: f32,
    ) -> Vec<f32> {
        // Still-image input: glyph / image per-orb colors come straight from the
        // extracted clusters and stay fixed for the whole loop.
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
        let mut pack = pack_render_data(
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
            opts.shadow_strength,
        );
        apply_saturation_to_pack(&mut pack, opts.saturation.max(0.0), n_orbs);
        pack
    }

    /// Render one **plain orb** frame from a raw `pack_render_data` buffer.
    ///
    /// `pack` must be the header(16) + per-orb(16 × n_orbs) layout produced by
    /// [`pack_render_data`]. `t` is the normalized time written into the
    /// shader's `u_t`; it is clamped to `0.0..=1.0`. Glyph / image rendering uses the
    /// private `render_packed_inner` with an SDF binding instead.
    ///
    /// Native only (#229): read-back path; the wasm/browser path uses
    /// [`Self::render_packed_to_view`] instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_packed(&self, pack: &[f32], width: u32, height: u32, t: f32) -> RgbaImage {
        // No glyph SDF, no aqua bleed layer: the plain orb path (byte-identical to
        // pre-#239 output).
        self.render_packed_inner(pack, width, height, t, None, None)
    }

    /// Render one **plain orb** frame from a raw `pack_render_data` buffer
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
        self.render_packed_inner_to_view(pack, width, height, t, None, None, view, format);
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
        aqua: Option<crate::animate::AquaBleedConfig>,
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

        let (params_buffer, orb_view) =
            self.upload_packed_frame(pack, width, height, t, &glyph, aqua);

        // Pipeline (shader compile) cached per (shader source, target format);
        // target / read-back cached per size; orb texture grows as needed. Only the
        // small params uniform / bind group are rebuilt per frame. The SDF variant
        // selects a different shader source + adds the SDF texture / sampler
        // bindings (2/3) but draws the same single pass. The read-back path always
        // targets `Rgba8Unorm`. #239: the additive bleed layer stays inert when aqua
        // params are 0 (byte-identical to plain orb).
        let (shader, is_glyph) = match &glyph {
            Some(_) => (self.orb_sdf_shader.as_str(), true),
            None => (self.orb_shader.as_str(), false),
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
        aqua: Option<crate::animate::AquaBleedConfig>,
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

        let (params_buffer, orb_view) =
            self.upload_packed_frame(pack, width, height, t, &glyph, aqua);

        // Both orb and SDF draw straight into the caller's view, so the pipeline
        // targets `format` either way (the SDF variant just binds the SDF
        // texture / sampler and runs a different shader source). #239: the additive
        // bleed layer stays inert when aqua params are 0.
        let (shader, is_glyph) = match &glyph {
            Some(_) => (self.orb_sdf_shader.as_str(), true),
            None => (self.orb_shader.as_str(), false),
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
        aqua: Option<crate::animate::AquaBleedConfig>,
    ) -> (wgpu::Buffer, wgpu::TextureView) {
        assert!(
            pack.len() >= HEADER_WORDS,
            "pack buffer too short: {} < {HEADER_WORDS}",
            pack.len()
        );

        // #239: the watercolor bleed sliders. `None` (every existing path: plain orb /
        // glyph / image with no aqua) ⇒ all 0, so the watercolor path is gated off
        // (aqua_bleed == 0) and the output is byte-identical to plain orb. Clamp to
        // 0..=1 (the WGSL morph/ramp assume that range; out-of-range would over-spread).
        let aqua = aqua.unwrap_or(crate::animate::AquaBleedConfig {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        });

        // Header → Params. Layout per `pack_render_data` doc-comment.
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
            // header[13] = shadow_strength (#241): thin-shadow rgb darkening for the
            // outermost fade segment, shared by every shape on the unified template.
            // The packer already clamps to 0..=1.
            shadow_strength: pack[13],
            // #239 PoC: the additive bleed layer's 4 sliders. 0 (the default `None`
            // path) keeps the layer inert (byte-identical to plain orb).
            aqua_bleed: aqua.bleed.clamp(0.0, 1.0),
            aqua_bloom: aqua.bloom.clamp(0.0, 1.0),
            aqua_offset: aqua.offset.clamp(0.0, 1.0),
            aqua_halo: aqua.halo.clamp(0.0, 1.0),
            _pad: [0.0; 3],
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
                // #255: misc.w = cross-axis centroid drift delta（0 = 位置トラック無し＝従来と一致）。
                // `.get().unwrap_or(0.0)` で short-row guard（off+13 > len）に当たる
                // 短い pack でも 0.0 にフォールバックし、guard とその検証テストを変えずに通す。
                misc: [
                    pack[off + 8],
                    pack[off + 9],
                    pack[off + 10],
                    pack.get(off + 13).copied().unwrap_or(0.0),
                ],
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
    /// browser maps buffers asynchronously). Core's wasm path never reads back —
    /// it draws into the surface view and presents. (orber-wasm has its own
    /// async-map read-back on top of `*_to_view` for transparent export, #245.)
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
/// `pack_render_data` buffer, in place (native GPU path only).
///
/// `pack_render_data` is shared with the Web wasm path and intentionally
/// leaves saturation out (the web side has no saturation knob), so the native GPU
/// path re-applies the `adjust_saturation_pub` transform per orb here instead.
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

/// Debug-guard for the to_view paths (#229): the shaders emit already-sRGB-encoded
/// values and write them raw into a Unorm target (see the module's compositing
/// contract), so an sRGB view format would apply the sRGB encoding a second time.
/// `debug_assert` so the release/browser hot path pays nothing; called from the
/// internal to_view funnel (`render_packed_inner_to_view`) that every `*_to_view`
/// entry point routes through.
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
    use crate::animate::{AnimateOptions, MotionDirection, MotionSpeed, SHADOW_STRENGTH_DEFAULT};
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
            aqua: None,
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
            shadow_strength: SHADOW_STRENGTH_DEFAULT,
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
        let mut pack = pack_render_data(
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
            SHADOW_STRENGTH_DEFAULT,
        );
        // Lie in the header: claim more orbs than the buffer actually carries.
        // The buffer still only has `real_orbs` per-orb rows, so rows
        // [real_orbs..claimed) must zero-fill rather than read OOB or panic.
        let claimed = 40usize;
        pack[8] = claimed as f32;

        // Must not panic; the extra claimed rows are zeroed (alpha-0, no contribution).
        let short = renderer.render_packed(&pack, w, h, 0.3);

        // Oracle: an honest pack that declares exactly its real orb count.
        let honest = pack_render_data(
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
            SHADOW_STRENGTH_DEFAULT,
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
        let mut pack = pack_render_data(
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
            SHADOW_STRENGTH_DEFAULT,
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

    /// #235 DISTANCE SOURCE 土台: the orb variant's loop body must inline the
    /// **analytic circle distance** — the same two lines the WebGL orb arm
    /// computes — `let dist = distance(...);` then `let r = dist / radius;`, and
    /// must NOT carry any SDF binding, rotation read, or rotation helper. If this
    /// drifts, the orb variant no longer feeds the shared falloff / compositing
    /// the analytic circle distance and the orb look diverges from the WebGL
    /// reference (#242). Locks the DISTANCE SOURCE inlining + the absence of
    /// SDF-only machinery in one place.
    #[test]
    fn orb_variant_wgsl_inlines_analytic_circle_distance() {
        let wgsl = orb_wgsl();
        // #239: the analytic circle distance now lives in the orb variant's `coverage_at`
        // (so the plain single tap and the blur's multi-tap share one coverage function),
        // but it is still the same `distance(sp, center) / radius` the WebGL orb arm used.
        assert!(
            wgsl.contains("let r = distance(sp, vec2<f32>(cx, cy)) / radius;"),
            "orb variant's coverage_at must compute the analytic circle distance r = distance(sp,center)/radius"
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
    /// `textureSampleLevel` would mean an SDF leaked into the orb shader).
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
            wgsl.contains("r = 1.0 - signed_unit;"),
            "SDF variant must convert the signed SDF sample to r = 1 - signed_unit"
        );
        // Mirror of the orb variant's residue check: every template marker must be
        // substituted away in the SDF variant too. A marker typo (e.g. `//!ORB_LOAD`
        // misspelled in the template or in `orb_sdf_wgsl()`) would silently leave the
        // marker in the compiled shader instead of breaking loudly — guard against it.
        assert!(
            !wgsl.contains("//!ORB"),
            "every template marker must be substituted away in the SDF variant"
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
            wgsl.contains("(u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0)"),
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

    // ---- #242: float straight Source-Over 合成（旧 WebGL 1:1）------

    /// WGSL ソースから `//` 行コメントを落とし、コード部分だけを返す。#242 の
    /// lowp 撤去ピンが「撤去の経緯を説明する doc コメント内の語」（例: 冒頭の
    /// 「u8 premultiply div255 合成…は #242 で撤去」）に誤反応しないための足場。
    fn wgsl_code_only(src: &str) -> String {
        src.lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #242 裁定の死守（撤去側）: 統一テンプレート（orb / glyph / image）の**コード**に、
    /// 撤去した Skia lowp 合成の痕跡 — `div255` / `to_u8_rgb` / `to_u8_a` /
    /// `composite_premul`、および premul→straight finalize の中間アキュムレータ
    /// `acc_a8` — が一切残っていないこと。どれか 1 つでも再混入すると orb 機構の
    /// 合成が float straight Source-Over でなくなり、最外周 rgb→0 フェードの
    /// 暗部沈み（#242 で撤去した症状）が再発し得る。doc コメントは経緯として
    /// これらの語に言及してよいので、コメントを剥いだコードだけを見る。
    /// 両 variant も同時に確認する（variant は marker 置換でしか差が出ないが、
    /// 置換断片からの再混入も防ぐ）。
    ///
    /// 注（#241）: `shadow_strength` による最外周フェード帯の意図的な rgb 暗化
    /// （falloff の `mix(1.0, 1.0-u, s)` と合成の `* fall.y`）は **lowp 復活では
    /// ない** — u8 量子化も premultiply/div255 も finalize も伴わない float 乗算
    /// 1 個で、s=0 で #242 と bit 同一に退化する。よって本ピンの needle には
    /// 含めない（含まれていたら誤検知）。
    #[test]
    fn orb_wgsl_template_has_no_lowp_composite_residue() {
        for (name, src) in [
            ("template", ORB_WGSL_TEMPLATE),
            ("orb variant", orb_wgsl()),
            ("sdf variant", orb_sdf_wgsl()),
        ] {
            let code = wgsl_code_only(src);
            for needle in [
                "div255",
                "to_u8_rgb",
                "to_u8_a",
                "composite_premul",
                "acc_a8",
            ] {
                assert!(
                    !code.contains(needle),
                    "{name}: lowp composite residue `{needle}` must not appear in code (#242)"
                );
            }
        }
    }

    /// #242 裁定の死守（採用側、#241 で影スケール込みに更新）: 統一テンプレートが
    /// 旧 WebGL の straight alpha float Source-Over をそのまま持つこと —
    /// 合成 2 式（rgb / a）と、Rim の mid_a 係数 `80.0 / 255.0`（raw float のまま、
    /// u8 量子化なし）。rgb 式の `fall.y` は #241 の薄い影スケール
    /// （`shadow_strength=0` で恒等 1.0 = #242 と bit 同一）であって、lowp 合成の
    /// 復活ではない（量子化も premultiply も伴わない float 乗算 1 個）。
    /// 式の字面が変わったら GLSL 1:1 移植（+ #241 影項）が崩れた合図。
    #[test]
    fn orb_wgsl_template_pins_straight_source_over_equations() {
        let wgsl = ORB_WGSL_TEMPLATE;
        assert!(
            wgsl.contains("let one_minus_a = 1.0 - alpha;"),
            "template must compute one_minus_a for the straight Source-Over"
        );
        // The rgb composite is GLSL 1:1 + #241 thin shadow. `rgb_scale` comes from the
        // coverage result `cov.y` (the #241 shadow scale, single-tap for the plain path
        // or the alpha-weighted average over the #239 blur taps), so for the plain orb
        // path (aqua_bleed == 0) this is still exactly `src.rgb * fall.y * a + dst.rgb * (1 - a)`.
        assert!(
            wgsl.contains("let rgb_scale = cov.y;"),
            "rgb_scale must come from the coverage result cov.y (plain single-tap or blurred avg)"
        );
        // #261: the source rgb starts as the **hue-pulsed** base color (procedural color
        // pulse — a per-orb slow hue rotation that is always on, on every shape, derived
        // from no input). It is then run through `aqua_character` (bloom/halo) **only when
        // にじみが engage している (`aqua_bleed > 0`)** — character rides the bleed. (Before
        // #261 src_rgb defaulted to the raw `o.color.rgb`; the procedural pulse intentionally
        // replaced that crisp identity so still images also get a living color over time.)
        assert!(
            wgsl.contains("var src_rgb = hue_rotate(")
                && wgsl.contains(
                    "HUE_PULSE_AMP * sin(TAU * (HUE_PULSE_CYCLES * t_frac + phi_opacity))"
                ),
            "src rgb must start from the #261 hue-pulsed base color (hue_rotate of o.color.rgb)"
        );
        assert!(
            wgsl.contains("if (params.aqua_bleed > 0.0) {")
                && wgsl.contains(
                    "src_rgb = aqua_character(src_rgb, alpha, params.aqua_bloom, params.aqua_halo);"
                ),
            "aqua_character must be gated on aqua_bleed > 0 and take the (hue-pulsed) src_rgb"
        );
        assert!(
            wgsl.contains("acc_rgb = src_rgb * rgb_scale * alpha + acc_rgb * one_minus_a;"),
            "template must composite rgb as (src.rgb * shadow_scale) * a + dst.rgb * (1 - a) \
             (GLSL 1:1 + #241 thin shadow; src.rgb = aqua_character output)"
        );
        assert!(
            wgsl.contains("acc_a = alpha + acc_a * one_minus_a;"),
            "template must composite alpha as a + dst.a * (1 - a) (GLSL 1:1)"
        );
        assert!(
            wgsl.contains("opacity * (80.0 / 255.0)"),
            "rim mid_a must stay the raw float opacity * 80/255 (no u8 quantization)"
        );
        // #241: 影は最外周セグメントの mix(1.0, 1.0-u, s) に限る（旧式の係数化）。
        // 新しいカーブ（pow / smoothstep 等）をここに足したら裁定からの逸脱。
        assert!(
            wgsl.contains("mix(1.0, 1.0 - u, params.shadow_strength)"),
            "the #241 thin shadow must be the strength-scaled old-lowp fade \
             mix(1.0, 1.0-u, s), not a new curve"
        );
    }

    /// #260 / #261: the procedural ambient effects (input-independent, on every shape) are
    /// pinned in the template so they are not silently dropped. Position wobble = a per-orb
    /// sine on the cross axis; color pulse = a per-orb slow hue rotation. Both use
    /// `sin(TAU * (<integer> * t_frac + <per-orb phase>))` so they are loop-periodic.
    #[test]
    fn orb_wgsl_template_pins_procedural_wobble_and_hue_pulse() {
        let wgsl = ORB_WGSL_TEMPLATE;
        // #260 position wobble: added to the cross axis, NOT a video-derived drift.
        assert!(
            wgsl.contains("const WOBBLE_AMP")
                && wgsl.contains(
                    "let wobble = WOBBLE_AMP * sin(TAU * (WOBBLE_CYCLES * t_frac + phase));"
                ),
            "template must carry the #260 procedural cross-axis sine wobble"
        );
        assert!(
            wgsl.contains("ny = cross_axis + wobble;")
                && wgsl.contains("nx = cross_axis + wobble;"),
            "the wobble must be added to the cross axis (LR/RL → ny, TB/BT → nx); \
             no video-derived cross_drift term remains"
        );
        assert!(
            !wgsl.contains("cross_drift"),
            "the removed video-input cross_drift must be gone from the template"
        );
        // #261 color pulse: per-orb slow hue rotation of the extracted color.
        assert!(
            wgsl.contains("const HUE_PULSE_AMP")
                && wgsl.contains("fn hue_rotate(")
                && wgsl.contains(
                    "HUE_PULSE_AMP * sin(TAU * (HUE_PULSE_CYCLES * t_frac + phi_opacity))"
                ),
            "template must carry the #261 procedural hue pulse"
        );
    }

    /// #265: the orber-local tap-count substitution must actually rewrite the shared
    /// `aquarelle` `AQUA_BLUR_TAPS` declaration. The replace is string-coupled to the
    /// crate's exact declaration, so a silent no-op (crate changed the line) would
    /// drop us back to the heavy 48-tap path and re-introduce the mobile crash. Pin
    /// that the built shaders carry the requested tap count (and the default static
    /// carries `DEFAULT_AQUA_TAPS`), for both the orb and SDF variants.
    #[test]
    fn aqua_blur_taps_substitution_rewrites_shared_declaration() {
        // The shared declaration the substitution targets must still be present in the
        // crate fragment (this is what `substitute_aqua_taps` debug_asserts on).
        assert!(
            aquarelle::AQUA_BLEED_WGSL.contains(AQUA_TAPS_DECL_48),
            "aquarelle no longer declares `{AQUA_TAPS_DECL_48}`; update AQUA_TAPS_DECL_48 (#265)"
        );
        for taps in [4u32, 8, 16] {
            for src in [build_orb_wgsl(taps), build_orb_sdf_wgsl(taps)] {
                assert!(
                    src.contains(&format!("const AQUA_BLUR_TAPS: u32 = {taps}u;")),
                    "built shader must carry the requested {taps} taps"
                );
                assert!(
                    !src.contains(AQUA_TAPS_DECL_48),
                    "built shader must not keep the heavy 48-tap declaration (taps={taps})"
                );
            }
        }
        // The no-arg statics ship the lightweight default.
        let default_decl = format!("const AQUA_BLUR_TAPS: u32 = {DEFAULT_AQUA_TAPS}u;");
        assert!(
            orb_wgsl().contains(&default_decl) && orb_sdf_wgsl().contains(&default_decl),
            "default statics must build with DEFAULT_AQUA_TAPS ({DEFAULT_AQUA_TAPS})"
        );
    }

    /// #260 / #261: the procedural effect **amplitudes must be non-zero**. The loop-close
    /// test (t=0 == t=1) would still pass if `WOBBLE_AMP` / `HUE_PULSE_AMP` were accidentally
    /// set to `0.0` (a no-op effect), so guard the actual const values here by parsing them
    /// out of the template. blink-tuned values: wobble 0.010, hue pulse 0.25 rad.
    #[test]
    fn orb_wgsl_procedural_amplitudes_are_nonzero() {
        let wgsl = ORB_WGSL_TEMPLATE;
        for (name, prefix) in [
            ("WOBBLE_AMP", "const WOBBLE_AMP: f32 = "),
            ("HUE_PULSE_AMP", "const HUE_PULSE_AMP: f32 = "),
        ] {
            let line = wgsl
                .lines()
                .map(str::trim_start)
                .find(|l| l.starts_with(prefix))
                .unwrap_or_else(|| panic!("{name} const not found in template"));
            let val: f32 = line
                .strip_prefix(prefix)
                .and_then(|rest| rest.trim().trim_end_matches(';').trim().parse().ok())
                .unwrap_or_else(|| panic!("could not parse {name} value from `{line}`"));
            assert!(
                val > 0.0,
                "{name} must be > 0 — a 0 amplitude makes the procedural effect a no-op"
            );
        }
    }

    /// #260 / #261: the procedural effects are loop-periodic — a plain still-image orb render
    /// at t=0 and t=1 must be **byte-identical**, so the output video seam (t=1 → t=0) does
    /// not jump. Both effects (and breathing / conveyor) return to their t=0 value at t=1
    /// because `t_frac = fract(t)` is 0 at both ends and the conveyor cycle is integer.
    /// (Replaces the removed video-keyframe loop-close e2e; the loop guarantee now rides the
    /// procedural ambient effects that every input gets.)
    #[test]
    fn gpu_procedural_effects_loop_close_t0_eq_t1() {
        let Some(renderer) = require_or_skip_renderer("gpu_procedural_effects_loop_close_t0_eq_t1")
        else {
            return;
        };
        let clusters = sample_clusters();
        // Mid speed = integer conveyor cycles ⇒ the belt itself loop-closes, leaving the
        // procedural wobble/hue as the only thing that could break the seam.
        let opts = orb_opts(64, 48, MotionDirection::LeftToRight, MotionSpeed::Mid);
        let at0 = renderer.render_frame(&clusters, &opts, 0.0);
        let at1 = renderer.render_frame(&clusters, &opts, 1.0);
        let max_diff =
            assert_within_tolerance(&at1, &at0, "procedural wobble+hue loop close (t=0 vs t=1)");
        assert_eq!(
            max_diff, 0,
            "procedural ambient effects must close the loop (t=0 == t=1) byte-exact"
        );
    }

    // ---- #239: additive watercolor bleed layer (structure + non-regression) ----

    /// #239 structure: the bleed geometry substitutes the `//!ORB_AQUA_BLEED_GEOM`
    /// marker away (a leftover marker would not compile) and defines the single
    /// blur-radius scale `aqua_blur_scale` the multi-tap blur reads — checked on both
    /// the orb and SDF variants so the bleed rides every shape the unified template
    /// serves. The bleed is a REAL spatial blur (multi-tap average of the silhouette
    /// coverage). #239 Phase 1 collapsed to the single continuous geometry (the wider
    /// Blob A/B variant was dropped), so the injected scale is always 1.0.
    #[test]
    fn aqua_bleed_substitutes_geometry_marker() {
        for (name, src) in [("orb", orb_wgsl()), ("sdf", orb_sdf_wgsl())] {
            assert!(
                !src.contains("//!ORB_AQUA_BLEED_GEOM"),
                "{name}: the aqua bleed geometry marker must be substituted away"
            );
            assert!(
                src.contains("fn blurred_coverage("),
                "{name}: the multi-tap blur function must be present"
            );
            assert!(
                src.contains("let aqua_blur_scale = 1.0;"),
                "{name}: the geometry marker must define the continuous blur-radius scale 1.0"
            );
        }
    }

    /// #239 non-regression + correctness structure: the bleed is a REAL spatial blur
    /// (multi-tap average of the silhouette coverage), gated on `aqua_bleed > 0` so the
    /// plain path (and aqua=None, which sets aqua_bleed=0) takes the single-tap
    /// `coverage_at` — byte-identical to the inline DISTANCE SOURCE (the byte-match
    /// gate's compile-time half). It must NOT morph the silhouette toward a circle (the
    /// rejected "always becomes round" bug) and must NOT add an edge ring / halo / blob
    /// (the rejected "frame" bug). This catches a regression before the GPU byte test.
    #[test]
    fn aqua_bleed_is_gated_real_blur_no_circle_morph() {
        let wgsl = ORB_WGSL_TEMPLATE;
        // Gated on bleed > 0; bleed == 0 falls to the single-tap plain path (byte-match).
        assert!(
            wgsl.contains("if (params.aqua_bleed > 0.0) {"),
            "the watercolor blur must be gated on aqua_bleed > 0 (else byte-match breaks)"
        );
        // bleed == 0 path: single-tap coverage_at == the plain inline DISTANCE SOURCE.
        assert!(
            wgsl.contains(
                "cov = coverage_at(style_bit, sample_px, cx, cy, radius, blur, opacity, angle);"
            ),
            "the plain (bleed=0) path must be the single-tap coverage_at (byte-match half)"
        );
        // bleed > 0 path: the multi-tap spatial blur.
        assert!(
            wgsl.contains("cov = blurred_coverage("),
            "the bleed>0 path must call the multi-tap blurred_coverage"
        );
        // ★ regression guard (kako-jun「丸の形に近づけるな」): the blur must NOT morph the
        // silhouette toward a circle. The rejected version interpolated r → r_circle.
        assert!(
            !wgsl.contains("r_circle") && !wgsl.contains("roundness"),
            "the bleed must be a real blur, never a SDF→circle morph (rejected: always rounds)"
        );
        // ★ regression guard (kako-jun「目立つ枠にすぎない」): no edge ring / halo / blob frame.
        assert!(
            !wgsl.contains("halo_term") && !wgsl.contains("watercolor_bleed("),
            "the bleed must not add an edge ring / halo (rejected: looks like a frame)"
        );
    }

    /// #239 character axes (bloom / halo / offset) structure pins. Each axis must be
    /// gated on its coef `> 0` so coef=0 is the strict identity (the all-zero byte-match
    /// half), and none may morph the silhouette or build an alpha ring (kako-jun's
    /// rejected "丸化" / "目立つ枠"). The offset only biases the blur disk ORIGIN
    /// (`bias_px`), it does not touch the coverage distance, so the star stays a star.
    #[test]
    fn aqua_character_axes_gated_and_shape_safe() {
        // The bleed fragment (`aqua_character` / `blurred_coverage` definitions) now lives in
        // the shared `aquarelle::AQUA_BLEED_WGSL`, substituted into the `//!ORB_AQUA_BLEED_SHARED`
        // marker (orber#250 Phase 2). Assert on the assembled orb shader so this structural pin
        // still covers the actually-compiled bleed code.
        let wgsl = orb_wgsl();
        // bloom / halo each gated on > 0 inside aqua_character (coef=0 ⇒ identity).
        assert!(
            wgsl.contains("if (bloom > 0.0) {") && wgsl.contains("if (halo > 0.0) {"),
            "bloom and halo must each be gated on coef > 0 (so coef=0 is the identity)"
        );
        // halo must NOT add alpha — it only rescales chroma around the (constant) luma.
        // The whole function operates on rgb (vec3) and is fed into the existing `alpha`
        // path; assert it returns rgb only (no alpha term named here) and boosts saturation.
        assert!(
            wgsl.contains("fn aqua_character(color: vec3<f32>, cov_a: f32, bloom: f32, halo: f32) -> vec3<f32>"),
            "aqua_character must transform rgb only (no alpha ring): vec3 in, vec3 out"
        );
        assert!(
            wgsl.contains("let sat_gain ="),
            "halo must boost saturation (chroma), not paint an outline"
        );
        // offset biases the blur disk origin only; it must NOT enter coverage_at's
        // distance (no circle morph). The bias is applied to the disk center.
        assert!(
            wgsl.contains("params.aqua_offset * AQUA_OFFSET_BIAS")
                && wgsl.contains("let center = sample_px + bias_px;"),
            "offset must bias the blur disk origin (center), never the coverage distance"
        );
        assert!(
            !wgsl.contains("r_circle") && !wgsl.contains("roundness"),
            "no character axis may morph the silhouette toward a circle"
        );
    }

    /// #239 ★最重要ゲート: with **all four aqua params = 0**, the additive-layer
    /// shader output is **byte-identical** to the plain orb (`aqua: None`) output —
    /// across several seeds and cluster counts. This is the structural non-regression
    /// guarantee: the new layer can never change an existing orb / glyph / image
    /// render unless a slider is moved.
    #[test]
    fn aqua_zero_params_byte_match_plain_orb() {
        let Some(renderer) = require_or_skip_renderer("aqua_zero_params_byte_match_plain_orb")
        else {
            return;
        };
        let (w, h) = (64u32, 48u32);
        // Vary seed and cluster count: a different RNG stream / orb population must
        // not break the identity (the additive layer is consumption-free).
        for &seed in &[0u64, 42, 99] {
            for &n in &[1usize, 8, 64] {
                let clusters: Vec<Cluster> = (0..n)
                    .map(|i| {
                        let f = i as f32 / n.max(1) as f32;
                        cluster(
                            [
                                (40 + (i * 37) % 200) as u8,
                                (60 + (i * 53) % 180) as u8,
                                (80 + (i * 71) % 160) as u8,
                            ],
                            0.15 + 0.7 * f,
                            0.2 + 0.6 * ((i * 13 % 100) as f32 / 100.0),
                            0.2 + 0.3 * ((i % 5) as f32 / 5.0),
                        )
                    })
                    .collect();
                let mut base = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Mid);
                base.seed = seed;
                base.count = Some(n);

                let plain = renderer.render_frame(&clusters, &base, 0.37);

                let mut zeroed = base.clone();
                zeroed.aqua = Some(crate::animate::AquaBleedConfig {
                    bleed: 0.0,
                    bloom: 0.0,
                    offset: 0.0,
                    halo: 0.0,
                });
                let via_aqua = renderer.render_frame(&clusters, &zeroed, 0.37);
                assert_eq!(
                    via_aqua.as_raw(),
                    plain.as_raw(),
                    "aqua zero must be byte-identical to plain orb (seed={seed}, n={n})"
                );
            }
        }
    }

    /// #239 question 7: character (bloom / halo / offset) rides the bleed. With
    /// **`bleed = 0` but the three character coefs non-zero**, the output must still be
    /// **byte-identical** to the plain orb — `aqua_character` is gated on `aqua_bleed > 0`,
    /// so water-off stays crisp regardless of bloom/halo, and `offset` only biases the
    /// blur disk origin (unreachable when the blur path itself is gated off). This pins
    /// the gate that keeps bloom/halo/offset inert whenever `bleed` is 0.
    #[test]
    fn aqua_bleed_zero_with_character_byte_match_plain_orb() {
        let Some(renderer) =
            require_or_skip_renderer("aqua_bleed_zero_with_character_byte_match_plain_orb")
        else {
            return;
        };
        let (w, h) = (64u32, 48u32);
        for &seed in &[0u64, 42, 99] {
            for &n in &[1usize, 8, 64] {
                let clusters: Vec<Cluster> = (0..n)
                    .map(|i| {
                        let f = i as f32 / n.max(1) as f32;
                        cluster(
                            [
                                (40 + (i * 37) % 200) as u8,
                                (60 + (i * 53) % 180) as u8,
                                (80 + (i * 71) % 160) as u8,
                            ],
                            0.15 + 0.7 * f,
                            0.2 + 0.6 * ((i * 13 % 100) as f32 / 100.0),
                            0.2 + 0.3 * ((i % 5) as f32 / 5.0),
                        )
                    })
                    .collect();
                let mut base = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Mid);
                base.seed = seed;
                base.count = Some(n);

                let plain = renderer.render_frame(&clusters, &base, 0.37);

                let mut characterful = base.clone();
                // bleed = 0 but bloom / offset / halo all non-zero. With the gate,
                // character must not engage ⇒ plain.
                characterful.aqua = Some(crate::animate::AquaBleedConfig {
                    bleed: 0.0,
                    bloom: 0.5,
                    offset: 0.5,
                    halo: 0.5,
                });
                let via_aqua = renderer.render_frame(&clusters, &characterful, 0.37);
                assert_eq!(
                    via_aqua.as_raw(),
                    plain.as_raw(),
                    "bleed=0 with character>0 must stay byte-identical to plain orb \
                     (character rides the bleed; seed={seed}, n={n})"
                );
            }
        }
    }

    /// #239 PoC: with a **non-zero** slider the additive layer must actually change
    /// the output (so the byte-match gate above is not passing by a dead code path).
    /// A positive `halo` adds an outer glow, so the lit-pixel count must rise versus
    /// the zero-param render. Verified on the orb shape (circle silhouette).
    #[test]
    fn aqua_nonzero_params_change_output() {
        let Some(renderer) = require_or_skip_renderer("aqua_nonzero_params_change_output") else {
            return;
        };
        let (w, h) = (64u32, 48u32);
        let clusters = sample_clusters();
        let base = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Mid);
        let plain = renderer.render_frame(&clusters, &base, 0.37);

        let mut lit = base.clone();
        lit.aqua = Some(crate::animate::AquaBleedConfig {
            bleed: 0.7,
            bloom: 0.5,
            offset: 0.3,
            halo: 0.6,
        });
        let via_aqua = renderer.render_frame(&clusters, &lit, 0.37);
        assert_ne!(
            via_aqua.as_raw(),
            plain.as_raw(),
            "non-zero aqua must change the output (else the layer is dead)"
        );
        assert!(
            lit_vs_bg(&via_aqua, base.background, 8) > lit_vs_bg(&plain, base.background, 8),
            "the additive glow must light more pixels than plain orb"
        );
    }

    /// #239 character axes: a single centered orb on a dark bg, blur engaged
    /// (`bleed=0.3`), so each axis can be isolated. Returns the rendered frame.
    #[cfg(test)]
    fn aqua_axis_frame(
        renderer: &GpuRenderer,
        w: u32,
        h: u32,
        bloom: f32,
        offset: f32,
        halo: f32,
    ) -> RgbaImage {
        // one bright saturated orb dead-center, big enough to cover the frame middle.
        let clusters = vec![cluster([230, 40, 40], 0.5, 0.5, 0.9)];
        let mut opts = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Mid);
        opts.count = Some(1);
        opts.background = [10, 10, 14, 255];
        opts.aqua = Some(crate::animate::AquaBleedConfig {
            bleed: 0.3,
            bloom,
            offset,
            halo,
        });
        // t chosen so the single orb sits near frame center (LR motion, phase 0).
        renderer.render_frame(&clusters, &opts, 0.0)
    }

    /// Peak luma among lit pixels (the interior/center brightening proxy for bloom:
    /// bloom pushes the high-coverage interior toward white, so the brightest pixel
    /// climbs). Restricted to lit pixels so the dark background is excluded.
    #[cfg(test)]
    fn lit_peak_luma(img: &RgbaImage, bg: [u8; 4], thresh: u8) -> f32 {
        let mut peak = 0.0f32;
        for p in img.pixels() {
            let lit = (0..3).any(|c| p.0[c].abs_diff(bg[c]) > thresh);
            if !lit {
                continue;
            }
            let luma = 0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32;
            peak = peak.max(luma);
        }
        peak
    }

    /// Mean chroma (max-min over RGB) of lit pixels — a saturation proxy for halo.
    /// Restricted to pixels that differ from the background so the flat bg (chroma~0)
    /// does not dilute the measure.
    #[cfg(test)]
    fn lit_mean_chroma(img: &RgbaImage, bg: [u8; 4], thresh: u8) -> f32 {
        let mut sum = 0.0f32;
        let mut n = 0u32;
        for p in img.pixels() {
            let lit = (0..3).any(|c| p.0[c].abs_diff(bg[c]) > thresh);
            if !lit {
                continue;
            }
            let mx = p.0[0].max(p.0[1]).max(p.0[2]) as f32;
            let mn = p.0[0].min(p.0[1]).min(p.0[2]) as f32;
            sum += mx - mn;
            n += 1;
        }
        sum / n.max(1) as f32
    }

    /// #239 bloom: a positive `bloom` brightens the orb's center (pushes it toward
    /// white) — the peak luma of lit pixels must rise versus the bleed-only (bloom=0)
    /// render. The "coef=0 ⇒ identity" half is **not** asserted here: `base` and a
    /// re-run with the same `0.0` args are byte-identical by construction (same inputs),
    /// so that assert was a tautology (`X == X`). The real byte担保 is the dedicated
    /// all-zero gate `aqua_zero_params_byte_match_plain_orb` (bloom=0 ⇒ no white-mix,
    /// gated on `bloom > 0` in WGSL).
    #[test]
    fn aqua_bloom_brightens_center() {
        let Some(renderer) = require_or_skip_renderer("aqua_bloom_brightens_center") else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let bg = [10u8, 10, 14, 255];
        let base = aqua_axis_frame(renderer, w, h, 0.0, 0.0, 0.0);
        let bloomed = aqua_axis_frame(renderer, w, h, 0.8, 0.0, 0.0);
        assert!(
            lit_peak_luma(&bloomed, bg, 8) > lit_peak_luma(&base, bg, 8) + 3.0,
            "bloom>0 must brighten the interior toward white: peak {} vs base {}",
            lit_peak_luma(&bloomed, bg, 8),
            lit_peak_luma(&base, bg, 8)
        );
    }

    /// #239 halo: a positive `halo` boosts peripheral **saturation** (chroma) without
    /// adding an alpha ring — the lit-pixel count must NOT grow meaningfully (no frame),
    /// while the mean chroma of lit pixels rises. The "halo=0 ⇒ identity" half is not
    /// asserted here (it would be `X == X` against a same-args re-run); the all-zero
    /// gate `aqua_zero_params_byte_match_plain_orb` is the real byte担保 (halo gated on
    /// `halo > 0` in WGSL).
    #[test]
    fn aqua_halo_boosts_saturation_without_ring() {
        let Some(renderer) = require_or_skip_renderer("aqua_halo_boosts_saturation_without_ring")
        else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let bg = [10u8, 10, 14, 255];
        let base = aqua_axis_frame(renderer, w, h, 0.0, 0.0, 0.0);
        let haloed = aqua_axis_frame(renderer, w, h, 0.0, 0.0, 0.9);
        // chroma of lit pixels must rise (more saturated edge).
        assert!(
            lit_mean_chroma(&haloed, bg, 8) > lit_mean_chroma(&base, bg, 8) + 1.0,
            "halo>0 must raise lit-pixel chroma: {} vs base {}",
            lit_mean_chroma(&haloed, bg, 8),
            lit_mean_chroma(&base, bg, 8)
        );
        // no alpha ring: lit-pixel count must not jump (saturation tweak, not a frame).
        let base_lit = lit_vs_bg(&base, bg, 8) as f32;
        let halo_lit = lit_vs_bg(&haloed, bg, 8) as f32;
        assert!(
            halo_lit <= base_lit * 1.05 + 4.0,
            "halo must not add an alpha ring (lit count {halo_lit} vs base {base_lit})"
        );
    }

    /// #239 offset: a positive `offset` biases the blur disk origin in a per-orb seed
    /// direction, making the smear asymmetric — the output must differ from the
    /// symmetric (offset=0) render. The lit-pixel count stays of the same order (the
    /// shape is not morphed/rounded). The "offset=0 ⇒ identity" half is not asserted
    /// here (it would be `X == X` against a same-args re-run); the all-zero gate
    /// `aqua_zero_params_byte_match_plain_orb` is the real byte担保 (offset=0 ⇒ bias_px=0).
    #[test]
    fn aqua_offset_makes_smear_asymmetric() {
        let Some(renderer) = require_or_skip_renderer("aqua_offset_makes_smear_asymmetric") else {
            return;
        };
        let (w, h) = (96u32, 96u32);
        let bg = [10u8, 10, 14, 255];
        let base = aqua_axis_frame(renderer, w, h, 0.0, 0.0, 0.0);
        let shifted = aqua_axis_frame(renderer, w, h, 0.0, 0.9, 0.0);
        assert_ne!(
            shifted.as_raw(),
            base.as_raw(),
            "offset>0 must change the smear (asymmetric blur)"
        );
        // shape is not blown up: lit count stays within a small band of the symmetric blur.
        let base_lit = lit_vs_bg(&base, bg, 8) as f32;
        let off_lit = lit_vs_bg(&shifted, bg, 8) as f32;
        assert!(
            off_lit > base_lit * 0.5 && off_lit < base_lit * 1.6,
            "offset must shift, not explode/round the silhouette (lit {off_lit} vs base {base_lit})"
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
            aqua: None,
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
            shadow_strength: SHADOW_STRENGTH_DEFAULT,
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
        let full = pack_render_data(
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
            SHADOW_STRENGTH_DEFAULT,
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
        let mut pack = pack_render_data(
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
            SHADOW_STRENGTH_DEFAULT,
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
            None,
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
        let pack = GpuRenderer::pack_orb_frame(&clusters, &opts, w, h, 0.3);
        let reference = renderer.render_packed(&pack, w, h, 0.3);
        let via_view = readback_via_view(renderer, w, h, format, |view| {
            renderer.render_packed_to_view(&pack, w, h, 0.3, view, format);
        });
        assert!(
            lit_vs_bg(&reference, opts.background, 8) > 0,
            "orb reference must have lit pixels"
        );
        assert_eq!(
            reference.as_raw(),
            via_view.as_raw(),
            "orb to_view bytes must match the read-back path"
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
    /// for the Image SDF shape (the Glyph-shared SDF pipeline via
    /// `render_frame_image_to_view`). Completes the per-shape to_view ↔
    /// read-back identity started by `to_view_matches_readback_orb_and_glyph`.
    /// Lit pixels are asserted first so the identity is not trivially satisfied
    /// by an empty frame.
    #[test]
    fn to_view_matches_readback_image() {
        let Some(renderer) = require_or_skip_renderer("to_view_matches_readback_image") else {
            return;
        };
        let (w, h) = (96u32, 64u32);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let clusters = sample_clusters();

        // Image, via the frame-level seam (shares the Glyph SDF pipeline).
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
        let orb = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let glyph = glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true);
        let image = image_opts(w, h);
        type ToView<'a> = Box<dyn Fn(&wgpu::TextureView) + 'a>;
        let cases: Vec<(&str, [u8; 4], RgbaImage, ToView<'_>)> = vec![
            (
                "orb",
                orb.background,
                renderer.render_frame(&clusters, &orb, 0.3),
                Box::new(|view: &wgpu::TextureView| {
                    renderer.render_frame_to_view(&clusters, &orb, 0.3, view, format);
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
        let pack = GpuRenderer::pack_orb_frame(&clusters, &opts, w, h, 0.3);
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
            "orb fallback reference must have lit pixels"
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
        let orb = orb_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow);
        let glyph = glyph_opts(w, h, MotionDirection::LeftToRight, MotionSpeed::Slow, true);

        // Solo (uncontended) oracles on the shared renderer. to_view ↔ read-back
        // byte identity at Rgba8Unorm is pinned separately, so the read-back
        // frames also serve as the to_view threads' oracles.
        let oracle_orb = renderer.render_frame(&clusters, &orb, 0.3);
        let oracle_glyph = renderer.render_frame_glyph(&clusters, &glyph, 0.3);

        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            // Read-back legs.
            handles.push(scope.spawn(|| {
                for _ in 0..3 {
                    let img = renderer.render_frame(&clusters, &orb, 0.3);
                    assert_eq!(
                        oracle_orb.as_raw(),
                        img.as_raw(),
                        "concurrent orb read-back must match its solo render"
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
                        renderer.render_frame_to_view(&clusters, &orb, 0.3, view, format);
                    });
                    assert_eq!(
                        oracle_orb.as_raw(),
                        img.as_raw(),
                        "concurrent orb to_view must match the solo read-back render"
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
        let mut pack = pack_render_data(
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
            SHADOW_STRENGTH_DEFAULT,
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
            None,
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
    ///
    /// #242 裁定（旧 WebGL の見た目を正と採用）による基線引き直し: the float
    /// straight Source-Over keeps the orb color constant across the outer falloff
    /// segment (no rgb→0 fade), so the orb's faint outer ring now crosses the
    /// `px_lit` threshold a few px further out, while the `●`'s SDF mask still
    /// clips at the silhouette edge — the union grows and the IoU drops by design,
    /// not by regression. Measured IoU on A18 Pro: 0.957 under the old lowp
    /// composite, 0.827 under #242. The `>= 0.75` floor keeps the "same blob,
    /// same place" guarantee with headroom for per-GPU edge antialiasing.
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
        // #242 裁定（旧 WebGL の見た目を正と採用）による基線引き直し: 0.85 → 0.75
        // （旧 lowp 合成での実測 0.957 → #242 float 合成での実測 0.827。doc コメント参照）。
        assert!(
            iou >= 0.75,
            "● glyph and orb footprints must overlap heavily (IoU >= 0.75); got {iou:.3} \
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
            None,
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

    // ---- #242/#241: float straight Source-Over + 薄い影の CPU 参照ピン（実 GPU 描画）
    //
    // orb.wgsl の falloff_curve + straight Source-Over（#242）に #241 の影項
    // （最外周フェード帯の rgb 暗化、強度 s）を含めてテスト内 Rust（f32）で
    // 再実装し、既知ジオメトリの単一 / 二重 orb フレームを `render_packed` で実際に
    // GPU 描画して、対象画素を ±1（GPU float 丸め差の許容）でピンする。
    // ジオメトリは [`centered_single_orb_pack`] と同じ流儀（phase 逆算・phi_*=0・
    // t=0 → breath 係数 1 / blur_delta 0）だが、色 / bg / blur / alpha_mul / style /
    // 位置を観点ごとに変えるため専用ヘルパを持つ。共通設定: direction=LR、
    // H=95（奇数 → cy = 0.5*95 = 47.5 が行 y=47 の sample 中心に一致し dy=0、
    // dist が dx だけで決まる）、nx = 32.5/W（cx = 32.5 → dx が整数になる）。
    // s は引数で陽に渡す（既存ピンは production 定数、#241 の新ピンは 0.0 / 1.0）。

    /// CPU 参照側の 1 orb 指定。色は 0..1 正規化で直接持つ（pack の per-orb 色
    /// words をこの値で上書きする）。
    #[derive(Clone, Copy)]
    struct RefOrb {
        color: [f32; 3],
        weight: f32,
        /// 目標中心 x（0..1）。direction=LR・t=0 で phase から逆算する。
        nx: f32,
        /// 目標中心 y（0..1）= per-orb cross_axis そのもの。
        ny: f32,
        /// 0.0 = Rim / 1.0 = Soft。
        style_bit: f32,
    }

    /// `nx` に中心が来る phase を逆算する（direction=LR / t=0、
    /// [`centered_single_orb_pack`] と同式）。f32 の往復誤差は ~1e-7 で、CPU 参照側
    /// も同じ f32 連鎖（[`ref_pixel_straight`]）を通すので誤差ごと一致する。
    fn phase_for_nx(nx: f32, base_radius: f32, weight: f32, width: u32) -> f32 {
        let r_pixels_max = base_radius * weight.sqrt() * BREATH_RADIUS_MAX_FACTOR;
        let r_norm = r_pixels_max / width as f32;
        let extent = 1.0 + 2.0 * r_norm;
        (nx + r_norm) / extent
    }

    /// `RefOrb` 列から direction=LR の手組み pack を作る（production packer で
    /// header を作り、per-orb words を全て決定値で上書き）。phi_* = 0 / 回転なし
    /// なので t=0 の breath 係数は radius=1 / blur_delta=0 / opacity_factor=1。
    /// `shadow_strength` は #241 の影強度（header[13]）をピンごとに陽に選ぶ。
    fn straight_test_pack(
        orbs: &[RefOrb],
        bg: [u8; 4],
        base_radius: f32,
        base_blur: f32,
        alpha_mul: f32,
        shadow_strength: f32,
        width: u32,
    ) -> Vec<f32> {
        let clusters: Vec<Cluster> = orbs
            .iter()
            .map(|_| cluster([255, 255, 255], 0.5, 0.5, 1.0))
            .collect();
        let mut pack = pack_render_data(
            &clusters,
            bg,
            base_radius,
            base_blur,
            0.0, // direction = LR
            MotionSpeed::Slow.cycle_count() as f32,
            7,
            orbs.len(),
            alpha_mul,
            0.0,   // shape_id (Orb)
            false, // glyph_rotate (Orb は無視)
            0.5,   // edge_softness (Orb は無視)
            shadow_strength,
        );
        for (i, o) in orbs.iter().enumerate() {
            let off = HEADER_WORDS + PER_ORB_WORDS * i;
            pack[off] = o.color[0];
            pack[off + 1] = o.color[1];
            pack[off + 2] = o.color[2];
            pack[off + 3] = o.weight;
            pack[off + 4] = phase_for_nx(o.nx, base_radius, o.weight, width);
            pack[off + 5] = 0.0; // phi_radius → radius_factor = 1
            pack[off + 6] = 0.0; // phi_blur → blur_delta = 0
            pack[off + 7] = 0.0; // phi_opacity → opacity_factor = 1
            pack[off + 8] = o.ny; // cross_axis
            pack[off + 9] = o.style_bit;
            pack[off + 10] = 1.0; // speed_mult（t=0 なので不問）
            pack[off + 11] = 0.0; // base_angle（Orb は無視）
            pack[off + 12] = 0.0; // rot_speed_signed（Orb は無視）
        }
        pack
    }

    /// orb.wgsl `falloff_curve` の 1:1 CPU 再実装（テスト内オラクル、f32）。
    /// 返り値は `(alpha, rgb_scale)`。`rgb_scale` は #241 の影項で、最外周フェード
    /// セグメントのみ `mix(1.0, 1.0-u, shadow)`、内側は常に 1.0（WGSL と同式）。
    /// `shadow = 0.0` のとき `rgb_scale = 1.0*1.0 + (1-u)*0.0 = 1.0` は f32 で厳密
    /// （丸めなし）なので、このオラクルは #242 直後（影なし）のオラクルと bit 同一に
    /// 退化する（`ref_falloff_shadow_zero_degenerates_bitwise` で検証）。
    fn ref_falloff_curve(
        style_bit: f32,
        r_in: f32,
        blur: f32,
        opacity: f32,
        shadow: f32,
    ) -> (f32, f32) {
        if opacity <= 0.0 || r_in >= 1.0 {
            return (0.0, 0.0);
        }
        let r = r_in.max(0.0);
        // mix(a, b, t) = a*(1-t) + b*t（WGSL mix と同式・同オペランド順）。
        let mix = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
        if style_bit < 0.5 {
            let center_a = opacity;
            let mid_a = opacity * (80.0 / 255.0);
            let mid_stop = (1.0 - blur * 0.8).clamp(0.05, 0.95);
            if r <= mid_stop {
                let u = if mid_stop > 0.0 { r / mid_stop } else { 1.0 };
                return (center_a + (mid_a - center_a) * u, 1.0); // mix(center_a, mid_a, u)
            }
            let denom = (1.0 - mid_stop).max(1e-6);
            let u = (r - mid_stop) / denom;
            return (mid_a * (1.0 - u), mix(1.0, 1.0 - u, shadow)); // mix(mid_a, 0, u)
        }
        let hold_stop = (1.0 - blur).clamp(0.05, 0.95);
        if r <= hold_stop {
            return (opacity, 1.0);
        }
        let denom = (1.0 - hold_stop).max(1e-6);
        let u = (r - hold_stop) / denom;
        (opacity * (1.0 - u), mix(1.0, 1.0 - u, shadow)) // mix(opacity, 0, u)
    }

    /// 0..1 straight 値を Rgba8Unorm 書き込みと同じ round(v*255) で u8 に量子化。
    fn q8(v: f32) -> u8 {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    /// orb.wgsl `composite_straight` の 1:1 CPU 再実装で画素 `(px, py)` の期待値を
    /// 出す（direction=LR / t=0 / phi_*=0 固定）。shader と同じ f32 連鎖
    /// （phase→pos→cx、distance、falloff（影項込み）、Source-Over）を通すので、
    /// GPU との差は浮動小数の丸め（±1/255 未満）だけになる。`shadow` は #241 の
    /// 影強度（pack 側の `straight_test_pack` に渡した値と揃えること）。
    #[allow(clippy::too_many_arguments)]
    fn ref_pixel_straight(
        orbs: &[RefOrb],
        bg: [u8; 4],
        base_radius: f32,
        base_blur: f32,
        alpha_mul: f32,
        shadow: f32,
        width: u32,
        height: u32,
        px: u32,
        py: u32,
    ) -> [u8; 4] {
        let sx = px as f32 + 0.5;
        let sy = py as f32 + 0.5;
        let mut acc = [
            bg[0] as f32 / 255.0,
            bg[1] as f32 / 255.0,
            bg[2] as f32 / 255.0,
            bg[3] as f32 / 255.0,
        ];
        for o in orbs {
            let r_pixels_max = base_radius * o.weight.sqrt() * BREATH_RADIUS_MAX_FACTOR;
            let r_norm = r_pixels_max / width as f32; // progress_axis = width (LR)
            let extent = 1.0 + 2.0 * r_norm;
            let phase = phase_for_nx(o.nx, base_radius, o.weight, width);
            let raw = phase * extent; // advance_steps = 0 (t=0)
            let pos = (raw - extent * (raw / extent).floor()) - r_norm;
            let cx = pos * width as f32;
            let cy = o.ny * height as f32;
            let radius = base_radius * o.weight.sqrt(); // radius_factor = 1 (t=0)
            if radius <= 0.0 {
                continue;
            }
            let blur = base_blur.clamp(0.0, 1.0); // blur_delta = 0 (t=0)
            let opacity = alpha_mul.clamp(0.0, 1.0); // opacity_factor = 1 (t=0)
            let dist = ((sx - cx) * (sx - cx) + (sy - cy) * (sy - cy)).sqrt();
            let r = dist / radius;
            let (alpha, rgb_scale) = ref_falloff_curve(o.style_bit, r, blur, opacity, shadow);
            if alpha > 0.0 {
                let inv = 1.0 - alpha;
                for (a, c) in acc.iter_mut().zip(o.color.iter()) {
                    // #241: src rgb は影スケール込み（WGSL: o.color.rgb * fall.y * alpha）。
                    *a = c * rgb_scale * alpha + *a * inv;
                }
                acc[3] = alpha + acc[3] * inv;
            }
        }
        [q8(acc[0]), q8(acc[1]), q8(acc[2]), q8(acc[3])]
    }

    /// `img` の (x,y) が `want` と全 4ch ±1 で一致すること（GPU float 丸め差の許容）。
    fn assert_px_close(img: &RgbaImage, x: u32, y: u32, want: [u8; 4], ctx: &str) {
        let got = img.get_pixel(x, y).0;
        for c in 0..4 {
            assert!(
                got[c].abs_diff(want[c]) <= 1,
                "{ctx}: pixel ({x},{y}) channel {c}: got {got:?}, want {want:?} (±1)"
            );
        }
    }

    /// #242: 中央 1 orb（Soft・blur 0.5・t=0・α=0.6）の中心画素が CPU 参照
    /// `round((orb·α + bg·(1−α))·255)` と ±1 で一致する — 新合成（straight float
    /// Source-Over）の数式そのもののピン。α < 1 にして「orb 色がそのまま出る」
    /// だけでは通らないようにしてある（合成式を実際に観測する）。
    #[test]
    fn gpu_straight_center_pixel_matches_cpu_source_over_reference() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_center_pixel_matches_cpu_source_over_reference")
        else {
            return;
        };
        let (w, h) = (64u32, 95u32);
        let bg = [10u8, 20, 30, 255];
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 64.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        }];
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 0.6f32);
        let pack = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img = renderer.render_packed(&pack, w, h, 0.0);
        // 中心画素 (32, 47): sample (32.5, 47.5) = orb 中心 → r ≈ 0 → α = alpha_mul。
        let want = ref_pixel_straight(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
            h,
            32,
            47,
        );
        assert!(
            want[0] < 250,
            "reference must actually blend (α < 1), got {want:?}"
        );
        assert_px_close(
            &img,
            32,
            47,
            want,
            "center pixel vs CPU straight Source-Over",
        );
    }

    /// #242 暗部沈み回帰の狙い撃ち（#241 影項込みに更新）: 外周フェードセグメント内
    /// （hold_stop < r < 1）の画素は production 影強度の straight 参照（orb 色 rgb が
    /// `mix(1, 1-u, s)` 倍に薄く暗化 + α フェード）と一致し、撤去した旧 lowp の
    /// 「rgb→0 フル フェード」参照値（= s=1 相当の暗い側）とは依然大きく不一致で
    /// あること。#241 の薄い影は**意図的な**部分暗化であり、lowp 暗部沈みの再発
    /// （フル フェード）とは別物 — その弁別をピクセルで検出する。
    #[test]
    fn gpu_straight_outer_fade_keeps_orb_color_not_lowp_dark() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_outer_fade_keeps_orb_color_not_lowp_dark")
        else {
            return;
        };
        let (w, h) = (96u32, 95u32);
        let bg = [10u8, 20, 30, 255];
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 96.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        }];
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 1.0f32);
        let pack = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img = renderer.render_packed(&pack, w, h, 0.0);

        // 画素 (56, 47): dist = 24 → r = 0.75（hold_stop 0.5 < r < 1 のフェード内）。
        let want = ref_pixel_straight(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
            h,
            56,
            47,
        );
        assert_px_close(&img, 56, 47, want, "outer-fade pixel vs straight reference");

        // 旧 lowp の最外周 rgb→0 フル フェード参照値（撤去済みの挙動の近似、量子化
        // 抜きの float 式 = #241 の s=1 と同式）: src_rgb を (1−u) で 0 へ落として
        // から同じ α で合成した値。production 影（s=0.2）はこれより十分明るい
        // （got − lowp = (1−s)·u·α·255 ≈ 白 orb・u=0.5・α=0.5 で 51）ことまで主張
        // して、フル暗化（lowp 暗部沈み）の再発をピクセルで検出する。
        // 注: この ≥16 マージンは s ≤ 0.74 を前提とする（(1−s)·63.75 ≥ 16）。
        // kako-jun 選定で SHADOW_STRENGTH_DEFAULT を 0.75 以上へ上げる場合は、
        // このテストの主張自体（薄い影 vs フル lowp の弁別）を見直すこと。
        let hold_stop = (1.0f32 - base_blur).clamp(0.05, 0.95);
        let r = 24.0f32 / base_radius; // = 0.75
        let u = (r - hold_stop) / (1.0 - hold_stop).max(1e-6);
        let alpha = alpha_mul * (1.0 - u);
        let got = img.get_pixel(56, 47).0;
        for c in 0..3 {
            let faded = 1.0f32 * (1.0 - u); // 旧 lowp は orb 色自体を 0 までフェードさせていた
            let old_lowp = q8(faded * alpha + (bg[c] as f32 / 255.0) * (1.0 - alpha));
            assert!(
                got[c].abs_diff(old_lowp) >= 16,
                "outer-fade pixel must NOT match the removed lowp full rgb→0 fade \
                 (channel {c}: got {got:?}, lowp ref {old_lowp})"
            );
        }
    }

    /// #242: Soft の plateau（α = opacity）は hold_stop **ちょうど**まで届き（`<=`）、
    /// 1px 先で初めて落ちる。dist = 15 / 16 / 17（radius 32、hold_stop 0.5 →
    /// 境界 dist = 16）の 3 画素を CPU 参照 ±1 でピンし、境界の向きも明示する。
    #[test]
    fn gpu_straight_soft_hold_stop_plateau_extends_to_boundary() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_soft_hold_stop_plateau_extends_to_boundary")
        else {
            return;
        };
        let (w, h) = (64u32, 95u32);
        let bg = [0u8, 0, 0, 255];
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 64.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        }];
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 1.0f32);
        let pack = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img = renderer.render_packed(&pack, w, h, 0.0);

        // y=47 行で dist = 15 / 16 / 17 → r = hold_stop−ε / ちょうど / +ε。
        for (x, label) in [
            (47u32, "r = hold_stop − ε (plateau)"),
            (48, "r = hold_stop exactly (boundary, <=)"),
            (49, "r = hold_stop + ε (fade start)"),
        ] {
            let want = ref_pixel_straight(
                &orbs,
                bg,
                base_radius,
                base_blur,
                alpha_mul,
                SHADOW_STRENGTH_DEFAULT,
                w,
                h,
                x,
                47,
            );
            assert_px_close(&img, x, 47, want, label);
        }
        // 境界の向き: plateau は dist=16（r = hold_stop）まで full opacity のまま。
        assert!(
            img.get_pixel(47, 47).0[0] >= 254 && img.get_pixel(48, 47).0[0] >= 254,
            "plateau must include the hold_stop boundary itself (<=)"
        );
        assert!(
            img.get_pixel(49, 47).0[0] <= 245,
            "one pixel past hold_stop must already fade (expected ≈236 with the #241 shadow)"
        );
    }

    /// #242: r = 1 の縁は**完全に透明**（falloff の早期 return `r_in >= 1.0` の
    /// 実画素版）。dist = 31 / 32 / 33（radius 32）で α = 微小 / 0 / 0 を CPU 参照
    /// ±1 でピンする。
    #[test]
    fn gpu_straight_r_one_edge_is_fully_transparent() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_r_one_edge_is_fully_transparent")
        else {
            return;
        };
        let (w, h) = (96u32, 95u32);
        let bg = [0u8, 0, 0, 255];
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 96.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        }];
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 1.0f32);
        let pack = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img = renderer.render_packed(&pack, w, h, 0.0);

        // y=47 行で dist = 31 / 32 / 33 → r = 1−ε / 1.0 / 1+ε。
        for (x, label) in [
            (63u32, "r = 1 − ε (faintly lit, α ≈ 0.0625)"),
            (64, "r = 1 exactly (transparent, >=)"),
            (65, "r = 1 + ε (outside)"),
        ] {
            let want = ref_pixel_straight(
                &orbs,
                bg,
                base_radius,
                base_blur,
                alpha_mul,
                SHADOW_STRENGTH_DEFAULT,
                w,
                h,
                x,
                47,
            );
            assert_px_close(&img, x, 47, want, label);
        }
        // 縁の内側 1px は確かに点いている（テストが空虚でないこと）。#241 の影で
        // α=0.0625 の縁は rgb_scale ≈ 0.81 も掛かり ≈13（影なし時代の ≈16 より暗い）。
        assert!(
            img.get_pixel(63, 47).0[0] >= 9,
            "just inside the edge must still be faintly lit (≈13 with the #241 shadow)"
        );
        // r >= 1 は背景のまま（黒 bg なので R = 0 ± 量子化）。
        assert!(
            img.get_pixel(64, 47).0[0] <= 1 && img.get_pixel(65, 47).0[0] <= 1,
            "at and beyond r = 1 the orb must contribute nothing"
        );
    }

    /// #242: Rim の mid_stop 境界は連続（`mix(center_a, mid_a, 1) == mid_a` ==
    /// 外側セグメントの `mix(mid_a, 0, 0)`）。dist = 23 / 24 / 25（radius 40、
    /// blur 0.5 → mid_stop 0.6 → 境界 dist = 24）の 3 画素を CPU 参照 ±1 でピンし、
    /// 境界をまたいで単調減少（段差なし）であることも見る。
    #[test]
    fn gpu_straight_rim_mid_stop_is_continuous() {
        let Some(renderer) = require_or_skip_renderer("gpu_straight_rim_mid_stop_is_continuous")
        else {
            return;
        };
        let (w, h) = (96u32, 95u32);
        let bg = [0u8, 0, 0, 255];
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 96.0,
            ny: 0.5,
            style_bit: 0.0, // Rim
        }];
        let (base_radius, base_blur, alpha_mul) = (40.0f32, 0.5f32, 1.0f32);
        let pack = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img = renderer.render_packed(&pack, w, h, 0.0);

        for (x, label) in [
            (55u32, "r = mid_stop − ε (center→mid segment)"),
            (56, "r = mid_stop exactly (boundary, mix(...,1) == mid_a)"),
            (57, "r = mid_stop + ε (mid→0 segment)"),
        ] {
            let want = ref_pixel_straight(
                &orbs,
                bg,
                base_radius,
                base_blur,
                alpha_mul,
                SHADOW_STRENGTH_DEFAULT,
                w,
                h,
                x,
                47,
            );
            assert_px_close(&img, x, 47, want, label);
        }
        // 連続性: 境界画素は両隣の間（単調減少）に収まる。境界で式が食い違って
        // いれば（mix(center,mid,1) != mid）、ここに段差が出る。
        let (r55, r56, r57) = (
            img.get_pixel(55, 47).0[0],
            img.get_pixel(56, 47).0[0],
            img.get_pixel(57, 47).0[0],
        );
        assert!(
            r55 >= r56 && r56 >= r57,
            "alpha must decrease monotonically across mid_stop (continuity): \
             got {r55} / {r56} / {r57}"
        );
    }

    /// #242: 半透明背景（bg.a < 1）での出力 α チャネルは straight Source-Over の
    /// `α_src + bg_a·(1−α_src)`（premul→straight finalize 撤去後の素の式）。
    /// bg.a = 128/255・α_src = 0.6 で A ≈ 204 を CPU 参照 ±1 でピンする。
    #[test]
    fn gpu_straight_alpha_channel_composites_over_translucent_bg() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_alpha_channel_composites_over_translucent_bg")
        else {
            return;
        };
        let (w, h) = (64u32, 95u32);
        let bg = [0u8, 0, 0, 128]; // 半透明背景
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 64.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        }];
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 0.6f32);
        let pack = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img = renderer.render_packed(&pack, w, h, 0.0);
        let want = ref_pixel_straight(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
            h,
            32,
            47,
        );
        assert_px_close(&img, 32, 47, want, "alpha channel vs straight Source-Over");
        // α_out = 0.6 + (128/255)·0.4 ≈ 0.8008 → 204。255（finalize で不透明化）でも
        // 128（bg 素通し）でもない、合成された中間値であることを明示する。
        let a = img.get_pixel(32, 47).0[3];
        assert!(
            (203..=205).contains(&a),
            "output alpha must be α_src + bg_a·(1−α_src) ≈ 204, got {a}"
        );
    }

    /// #242: 2 orb の重なりは pack 順の Source-Over（後段の orb が上）。同位置の
    /// 赤 → 青（各 α=0.6）で中心画素が「青が上」の CPU 参照と一致し、逆順参照とは
    /// 大きく異なることをピンする。
    #[test]
    fn gpu_straight_two_orbs_composite_in_pack_order() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_two_orbs_composite_in_pack_order")
        else {
            return;
        };
        let (w, h) = (64u32, 95u32);
        let bg = [0u8, 0, 0, 255];
        let red = RefOrb {
            color: [1.0, 0.0, 0.0],
            weight: 1.0,
            nx: 32.5 / 64.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        };
        let blue = RefOrb {
            color: [0.0, 0.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 64.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        };
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 0.6f32);
        let orbs = [red, blue]; // 青が後 = 上
        let pack = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img = renderer.render_packed(&pack, w, h, 0.0);

        // 期待値（赤の上に青）: R = 0.6·0.4 = 0.24 → 61、B = 0.6 → 153。
        let want = ref_pixel_straight(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
            h,
            32,
            47,
        );
        assert_px_close(&img, 32, 47, want, "two-orb pack-order Source-Over");

        // 逆順（青の上に赤 = R 153 / B 61）とは明確に違う = 順序が観測されている。
        let reversed = [orbs[1], orbs[0]];
        let rev = ref_pixel_straight(
            &reversed,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
            h,
            32,
            47,
        );
        let got = img.get_pixel(32, 47).0;
        assert!(
            got[0].abs_diff(rev[0]) > 2 && got[2].abs_diff(rev[2]) > 2,
            "pack order must matter (later orb on top): got {got:?}, reversed ref {rev:?}"
        );
    }

    // ---- #241: 薄い影（shadow_strength）の境界ピン --------------------------------

    /// #241 (a) CPU 側: `shadow = 0.0` のオラクルが **#242 直後（影なし）の式に
    /// bitwise で退化する**こと。影項は `mix(1.0, 1.0-u, s) = 1.0*(1-s) + (1-u)*s`
    /// で、s=0 なら `1.0*1.0 + (1-u)*0.0 = 1.0` — f32 で丸めなしの厳密な恒等。
    /// rgb への乗算も `c * 1.0 == c`（IEEE で厳密）なので、s=0 の出力は #242 の
    /// 出力と bit 同一になる。ここでは #242 当時の alpha-only オラクルをテスト内に
    /// 再掲し、α の bit 一致と rgb_scale == 1.0（厳密）を全域スイープで固定する。
    /// GPU 側の実画素ピンは `gpu_straight_shadow_zero_equals_no_shadow_reference`。
    #[test]
    fn ref_falloff_shadow_zero_degenerates_bitwise() {
        // #242 直後の falloff（影項導入前のオラクル、当時の実装の再掲）。
        fn ref_falloff_242(style_bit: f32, r_in: f32, blur: f32, opacity: f32) -> f32 {
            if opacity <= 0.0 || r_in >= 1.0 {
                return 0.0;
            }
            let r = r_in.max(0.0);
            if style_bit < 0.5 {
                let center_a = opacity;
                let mid_a = opacity * (80.0 / 255.0);
                let mid_stop = (1.0 - blur * 0.8).clamp(0.05, 0.95);
                if r <= mid_stop {
                    let u = if mid_stop > 0.0 { r / mid_stop } else { 1.0 };
                    return center_a + (mid_a - center_a) * u;
                }
                let denom = (1.0 - mid_stop).max(1e-6);
                let u = (r - mid_stop) / denom;
                return mid_a * (1.0 - u);
            }
            let hold_stop = (1.0 - blur).clamp(0.05, 0.95);
            if r <= hold_stop {
                return opacity;
            }
            let denom = (1.0 - hold_stop).max(1e-6);
            let u = (r - hold_stop) / denom;
            opacity * (1.0 - u)
        }

        for style_bit in [0.0f32, 1.0] {
            for blur in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                for opacity in [0.0f32, 0.1, 0.55, 1.0] {
                    for i in 0..=120 {
                        let r = i as f32 / 100.0; // 0.00..=1.20（縁の外側も含む）
                        let (alpha, scale) = ref_falloff_curve(style_bit, r, blur, opacity, 0.0);
                        let alpha_242 = ref_falloff_242(style_bit, r, blur, opacity);
                        assert_eq!(
                            alpha.to_bits(),
                            alpha_242.to_bits(),
                            "alpha must be bitwise identical to the #242 oracle at s=0 \
                             (style={style_bit}, r={r}, blur={blur}, opacity={opacity})"
                        );
                        if alpha > 0.0 {
                            assert_eq!(
                                scale.to_bits(),
                                1.0f32.to_bits(),
                                "rgb_scale must be exactly 1.0 at s=0 (identity multiply) \
                                 (style={style_bit}, r={r}, blur={blur}, opacity={opacity})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// #241 (a) GPU 側: `shadow_strength = 0.0` の実描画が **影なし（#242 直後）の
    /// CPU 参照**と全帯域（plateau / フェード帯 / 縁の外）で ±1 一致し、production
    /// 影（s=0.2）の描画とはフェード帯で実際に差が出ること（ノブが効いている =
    /// テストが空虚でない）。s=0 の式恒等（bit 同一）は
    /// `ref_falloff_shadow_zero_degenerates_bitwise` が CPU で厳密に固定済みで、
    /// ここは「その退化が実 GPU・実パイプラインまで通る」ことの画素ピン
    /// （GPU float 丸めの ±1 のみ許容）。
    #[test]
    fn gpu_straight_shadow_zero_equals_no_shadow_reference() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_shadow_zero_equals_no_shadow_reference")
        else {
            return;
        };
        let (w, h) = (96u32, 95u32);
        let bg = [10u8, 20, 30, 255];
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 96.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        }];
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 1.0f32);
        let pack0 = straight_test_pack(&orbs, bg, base_radius, base_blur, alpha_mul, 0.0, w);
        let img0 = renderer.render_packed(&pack0, w, h, 0.0);

        // y=47 行を中心 (32) から縁の外 (66) までスキャン: plateau（r <= 0.5）、
        // フェード帯（0.5 < r < 1）、r >= 1 の外側を全部 s=0 参照でピンする。
        for x in 32..=66u32 {
            let want = ref_pixel_straight(
                &orbs,
                bg,
                base_radius,
                base_blur,
                alpha_mul,
                0.0,
                w,
                h,
                x,
                47,
            );
            assert_px_close(&img0, x, 47, want, "s=0 must match the no-shadow reference");
        }

        // 同条件で production 影と比較: フェード帯（x=56, r=0.75）には差が出る。
        // s=0 が「たまたま」一致しているのではなく、ノブが実際に観測されている根拠。
        // 注: この可視差主張（> +4）は s ≥ 0.08 を前提とする（差 ≈ 63.75·s）。
        // kako-jun 選定で SHADOW_STRENGTH_DEFAULT を大きく下げる場合は見直すこと。
        let pack_prod = straight_test_pack(
            &orbs,
            bg,
            base_radius,
            base_blur,
            alpha_mul,
            SHADOW_STRENGTH_DEFAULT,
            w,
        );
        let img_prod = renderer.render_packed(&pack_prod, w, h, 0.0);
        let (no_shadow, with_shadow) = (img0.get_pixel(56, 47).0, img_prod.get_pixel(56, 47).0);
        assert!(
            no_shadow[0] > with_shadow[0].saturating_add(4),
            "production shadow must visibly darken the outer fade band vs s=0 \
             (got s=0 {no_shadow:?} vs s={SHADOW_STRENGTH_DEFAULT} {with_shadow:?})"
        );
        // plateau（x=40, r=0.25）は影の対象外なので s に依らず一致する。
        assert_eq!(
            img0.get_pixel(40, 47),
            img_prod.get_pixel(40, 47),
            "inner segments must be untouched by shadow_strength"
        );
    }

    /// #241 (b): `shadow_strength = 1.0` の実描画が、#242 で撤去した**旧 lowp の
    /// 最外周 rgb→0 フェードの CPU 再現（量子化抜き float 式）**と外周フェード帯で
    /// ±1 一致すること。旧 lowp は straight color を (1−u) 倍してから合成していた:
    ///   out_c = c·(1−u)·α + bg_c·(1−α)
    /// これは #241 の s=1（rgb_scale = mix(1, 1−u, 1) = 1−u）と同式 — 「s=1 ≒ 旧
    /// lowp の暗さ」の式レベルの根拠をそのまま画素で観測する（u8 量子化・
    /// premultiply 由来の ±数カウントは復元しないので、一致は float 式に対して）。
    #[test]
    fn gpu_straight_shadow_one_matches_old_lowp_outer_fade() {
        let Some(renderer) =
            require_or_skip_renderer("gpu_straight_shadow_one_matches_old_lowp_outer_fade")
        else {
            return;
        };
        let (w, h) = (96u32, 95u32);
        let bg = [10u8, 20, 30, 255];
        let orbs = [RefOrb {
            color: [1.0, 1.0, 1.0],
            weight: 1.0,
            nx: 32.5 / 96.0,
            ny: 0.5,
            style_bit: 1.0, // Soft
        }];
        let (base_radius, base_blur, alpha_mul) = (32.0f32, 0.5f32, 1.0f32);
        let pack = straight_test_pack(&orbs, bg, base_radius, base_blur, alpha_mul, 1.0, w);
        let img = renderer.render_packed(&pack, w, h, 0.0);

        // フェード帯（hold_stop 0.5 < r < 1）の 3 点: dist = 20 / 24 / 28 →
        // u = 0.25 / 0.5 / 0.75。旧 lowp 式をテスト内で独立に組んで比較する
        // （ref_pixel_straight 経由ではなく撤去前式の直接再現、というのが本ピンの主張）。
        let hold_stop = (1.0f32 - base_blur).clamp(0.05, 0.95);
        for x in [52u32, 56, 60] {
            let dist = (x as f32 + 0.5) - 32.5;
            let r = dist / base_radius;
            assert!(
                r > hold_stop && r < 1.0,
                "test pixel must be in the fade band"
            );
            let u = (r - hold_stop) / (1.0 - hold_stop).max(1e-6);
            let alpha = alpha_mul * (1.0 - u);
            let got = img.get_pixel(x, 47).0;
            for c in 0..3 {
                let faded = 1.0f32 * (1.0 - u); // 旧 lowp: orb 色自体を 0 へフェード
                let old_lowp = q8(faded * alpha + (bg[c] as f32 / 255.0) * (1.0 - alpha));
                assert!(
                    got[c].abs_diff(old_lowp) <= 1,
                    "s=1 must reproduce the removed lowp rgb→0 fade (float form) at x={x} \
                     channel {c}: got {got:?}, old lowp ref {old_lowp}"
                );
            }
        }
    }
}
