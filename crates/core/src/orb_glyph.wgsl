// orber #212 Phase 1b — Glyph orb の production WGSL（native CLI / wgpu）。
//
// CPU 経路 `crate::glyph::render_glyph_orb`（= bleed pass 前の塗り）を WGSL に
// 1:1 で写したもの。各 orb は埋め込みフォント 1 文字の SDF を bilinear sampling し、
//   signed_unit = sdf01 * 2 - 1
//   r = 1 - signed_unit
//   alpha = falloff_curve(profile, r, blur, opacity)   ← Circle と同じ Rim/Soft profile
// で塗り、straight-alpha の source-over で合成する。回転(#136)は per-orb の
// (base_angle, rot_speed_signed) を data-texture の x=3 texel から読み、CPU の
// glyph_rotation_angle と同式で角度を出して SDF サンプル座標を回す。
//
// パリティ範囲（過大主張しない）:
//   - bit-exact は課さない。CPU 経路は全 orb 描画後に aquarelle bleed pass を 1 回
//     かける（#195）が、この WGSL は **bleed 前の塗り** だけを描く。よって lit 被覆 /
//     エッジ位置 / softness 連動 / 回転角は CPU と緩い許容で一致するが、bleed 由来の
//     滲み差は期待値として許容する（別 slice 2.5 で実装予定）。
//   - falloff_curve は style.rs の raw-float 版（u8 量子化なし）を写す。Circle の
//     orb_circle.wgsl の falloff（Skia lowp の u8 量子化版）とは別物なので
//     共有しない。
//   - blend は CPU の blend_source_over（straight rgba を per-orb で u8 量子化しながら
//     source-over）を写し、finalize（a<255 で rgb を a で割って straight 化）まで
//     再現する。背景塗りは不透明前提で premul==straight。
//
// 座標系:
//   @builtin(position) は top-left のピクセル座標（中心 +0.5）。CPU も px = x+0.5 /
//   py = y+0.5 で評価するので flip 不要。SDF テクスチャは R8Unorm、filterable な
//   bilinear sampler で読む（WebGL2 portable、Phase 2）。

const TAU: f32 = 6.28318530718;
const BREATH_RADIUS_MAX_FACTOR: f32 = 1.10;
// 1/√2。crate::glyph::GLYPH_SDF_CONTENT_SPAN と同期。Rust 側から override せず
// ここに定数で持つ（CPU と同値であることをテストで担保する）。
const GLYPH_SDF_CONTENT_SPAN: f32 = 0.70710678;

struct Params {
    resolution: vec2<f32>, // (width, height) px
    t: f32,                // [0, 1)
    base_radius: f32,      // px = min(w,h) * 0.25 * orb_size
    bg: vec4<f32>,         // straight rgba (0..1)
    base_blur: f32,        // 0..1
    direction: f32,        // 0=LR, 1=RL, 2=TB, 3=BT
    cycle: f32,            // 1=VerySlow, 2=Slow, 3=Mid, 4=Fast
    n_orbs: f32,           // 整数を f32 で
    alpha_mul: f32,        // softness.alpha_mul
    glyph_rotate: f32,     // #136: 1.0=ON / 0.0=OFF
    edge_softness: f32,    // #205: 現 fill では未使用（予約）
    sdf_size: f32,         // glyph SDF の一辺（texel 数）。CPU の bilinear 規約を写すため
};

// per-orb のパック（data-texture, 幅 4 texel × 高さ N の Rgba32Float）。
//   x=0: color = (r, g, b, weight)
//   x=1: phase = (phase, phi_radius, phi_blur, phi_opacity)
//   x=2: misc  = (cross_axis, style_bit, speed_mult, _)
//   x=3: rot   = (base_angle, rot_speed_signed, _, _)   ← #136、Glyph だけが読む
// textureLoad（point fetch）で読むので orb_tex は filterable:false で良い。
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var orb_tex: texture_2d<f32>;
// glyph SDF（R8Unorm, 単一文字 / 単一サイズ）と bilinear sampler。
@group(0) @binding(2) var glyph_sdf: texture_2d<f32>;
@group(0) @binding(3) var glyph_samp: sampler;

struct Orb {
    color: vec4<f32>, // (r, g, b, weight)
    phase: vec4<f32>, // (phase, phi_radius, phi_blur, phi_opacity)
    misc: vec4<f32>,  // (cross_axis, style_bit, speed_mult, _)
    rot: vec4<f32>,   // (base_angle, rot_speed_signed, _, _)
};

fn load_orb(i: u32) -> Orb {
    let row = i32(i);
    var o: Orb;
    o.color = textureLoad(orb_tex, vec2<i32>(0, row), 0);
    o.phase = textureLoad(orb_tex, vec2<i32>(1, row), 0);
    o.misc = textureLoad(orb_tex, vec2<i32>(2, row), 0);
    o.rot = textureLoad(orb_tex, vec2<i32>(3, row), 0);
    return o;
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var xy = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(xy[vi], 0.0, 1.0);
    return out;
}

fn clampf(x: f32, a: f32, b: f32) -> f32 {
    return min(max(x, a), b);
}

// style.rs::falloff_curve を raw-float のまま写す（u8 量子化なし）。
//   r: 0=中心/深部、1=edge、>1=外側。返り値は straight alpha (0..1)。
//   style_bit < 0.5 が Rim、それ以外が Soft。
fn falloff_curve(style_bit: f32, r_in: f32, blur: f32, opacity_in: f32) -> f32 {
    let opacity = clampf(opacity_in, 0.0, 1.0);
    if (opacity <= 0.0) {
        return 0.0;
    }
    let r = max(r_in, 0.0);
    if (r >= 1.0) {
        return 0.0;
    }
    let b = clampf(blur, 0.0, 1.0);
    if (style_bit < 0.5) {
        // Rim: mid_a = opacity * 80/255、mid_stop = clamp(1 - blur*0.8, .05, .95)。
        let mid_a = opacity * (80.0 / 255.0);
        let mid_stop = clampf(1.0 - b * 0.8, 0.05, 0.95);
        if (r <= mid_stop) {
            var u = 1.0;
            if (mid_stop > 0.0) {
                u = r / mid_stop;
            }
            return opacity + (mid_a - opacity) * u;
        }
        let denom = max(1.0 - mid_stop, 1e-6);
        let u = (r - mid_stop) / denom;
        return mid_a * (1.0 - u);
    }
    // Soft: hold_stop = clamp(1 - blur, .05, .95)。
    let hold_stop = clampf(1.0 - b, 0.05, 0.95);
    if (r <= hold_stop) {
        return opacity;
    }
    let denom = max(1.0 - hold_stop, 1e-6);
    let u = (r - hold_stop) / denom;
    return opacity * (1.0 - u);
}

// CPU の glyph_rotation_angle(cycle, t, base_angle, rot_speed_signed, glyph_rotate)。
//   glyph_rotate=false → base_angle 静止。
//   それ以外 → base_angle + rem_euclid(cycle * rot_speed_signed * t, 1.0) * TAU。
// loop closure: cycle * rot_speed_signed が整数なので t=1 で turns=0 に閉じる。
fn glyph_rotation_angle(base_angle: f32, rot_speed_signed: f32) -> f32 {
    if (params.glyph_rotate < 0.5) {
        return base_angle;
    }
    let x = params.cycle * rot_speed_signed * params.t;
    // rem_euclid(x, 1.0) = x - floor(x)（1.0 は正なので floor 版で一致、負も非負を返す）。
    let turns = x - floor(x);
    return base_angle + turns * TAU;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let px = in.pos.x;
    let py = in.pos.y;

    var progress_axis = params.resolution.y;
    if (params.direction < 1.5) {
        progress_axis = params.resolution.x;
    }

    // CPU 経路は Pixmap を背景色で fill してから blend_source_over を重ねる。
    // 背景は不透明前提（premul==straight）。アキュムレータは straight rgba を u8
    // (0..255 float) で持ち、各 orb で round して量子化する（blend_source_over と同じ）。
    var acc = vec4<f32>(
        floor(clampf(params.bg.r, 0.0, 1.0) * 255.0 + 0.5),
        floor(clampf(params.bg.g, 0.0, 1.0) * 255.0 + 0.5),
        floor(clampf(params.bg.b, 0.0, 1.0) * 255.0 + 0.5),
        floor(clampf(params.bg.a, 0.0, 1.0) * 255.0 + 0.5),
    );

    let count = u32(params.n_orbs + 0.5);
    let t_frac = fract(params.t);

    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let o = load_orb(i);
        let weight = o.color.w;
        let phase = o.phase.x;
        let phi_radius = o.phase.y;
        let phi_blur = o.phase.z;
        let phi_opacity = o.phase.w;
        let cross_axis = o.misc.x;
        let style_bit = o.misc.y; // 0=rim, 1=soft
        let speed_mult = o.misc.z;
        let base_angle = o.rot.x;
        let rot_speed_signed = o.rot.y;

        let r_pixels_max = params.base_radius * sqrt(max(weight, 0.0)) * BREATH_RADIUS_MAX_FACTOR;
        var r_normalized = 0.0;
        if (progress_axis > 0.0) {
            r_normalized = r_pixels_max / progress_axis;
        }
        let extent = 1.0 + 2.0 * r_normalized;

        let advance_steps = fract(params.cycle * speed_mult * params.t);
        let raw = phase * extent + advance_steps * extent;
        let pos = (raw - extent * floor(raw / extent)) - r_normalized;

        var nx: f32;
        var ny: f32;
        if (params.direction < 0.5) {        // LR
            nx = pos;
            ny = cross_axis;
        } else if (params.direction < 1.5) { // RL
            nx = 1.0 - pos;
            ny = cross_axis;
        } else if (params.direction < 2.5) { // TB
            nx = cross_axis;
            ny = pos;
        } else {                             // BT
            nx = cross_axis;
            ny = 1.0 - pos;
        }

        let radius_factor = 1.0 + 0.10 * sin(TAU * t_frac + phi_radius);
        let blur_delta = 0.15 * sin(TAU * t_frac + phi_blur);
        let opacity_factor = 1.0 + 0.05 * sin(TAU * t_frac + phi_opacity);

        let radius = params.base_radius * sqrt(max(weight, 0.0)) * radius_factor;
        if (radius <= 0.0) {
            continue;
        }

        let blur = clampf(params.base_blur + blur_delta, 0.0, 1.0);
        let opacity = clampf(opacity_factor * params.alpha_mul, 0.0, 1.0);

        let cx = nx * params.resolution.x;
        let cy = ny * params.resolution.y;

        // CPU render_glyph_orb と同じ座標変換: orb 中心からの差分を +angle で回し、
        // (2*radius) で割って CONTENT_SPAN を掛けて 0.5 中心の UV にする。
        let angle = glyph_rotation_angle(base_angle, rot_speed_signed);
        let cos_a = cos(angle);
        let sin_a = sin(angle);
        let dx = px - cx;
        let dy = py - cy;
        let rx = cos_a * dx - sin_a * dy;
        let ry = sin_a * dx + cos_a * dy;
        let u = rx / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;
        let v = ry / (2.0 * radius) * GLYPH_SDF_CONTENT_SPAN + 0.5;
        if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) {
            continue;
        }

        // CPU sample_sdf_bilinear の規約に合わせる: CPU は coord = clamp(u,0,1)*(size-1)
        // の格子点を線形補間する。GPU sampler は uv*size-0.5 の texel 空間で補間するので、
        //   uv_gpu = (u*(size-1) + 0.5) / size
        // と remap すると両者が同じ格子点・同じ重みで補間し、半 texel ずれを消せる。
        let s = params.sdf_size;
        let uu = (clampf(u, 0.0, 1.0) * (s - 1.0) + 0.5) / s;
        let vv = (clampf(v, 0.0, 1.0) * (s - 1.0) + 0.5) / s;
        // bilinear sample（sampler が線形補間）。R8Unorm なので .r に SDF が入る。
        let sdf01 = textureSampleLevel(glyph_sdf, glyph_samp, vec2<f32>(uu, vv), 0.0).r;
        let signed_unit = sdf01 * 2.0 - 1.0;
        let r = 1.0 - signed_unit;
        let alpha = falloff_curve(style_bit, r, blur, opacity);
        if (alpha <= 0.0) {
            continue;
        }

        // blend_source_over: dst を straight として扱い、src_rgb = orb_rgb * alpha、
        // out = src + dst*(1-alpha)、out_a = alpha + dst_a*(1-alpha)。各成分を
        // *255 round clamp で u8 量子化してアキュムレータに書き戻す（per-orb）。
        let one_minus_a = 1.0 - alpha;
        let dst_r = acc.r / 255.0;
        let dst_g = acc.g / 255.0;
        let dst_b = acc.b / 255.0;
        let dst_a = acc.a / 255.0;
        let src_r = o.color.r * alpha;
        let src_g = o.color.g * alpha;
        let src_b = o.color.b * alpha;
        acc = vec4<f32>(
            floor(clampf(src_r + dst_r * one_minus_a, 0.0, 1.0) * 255.0 + 0.5),
            floor(clampf(src_g + dst_g * one_minus_a, 0.0, 1.0) * 255.0 + 0.5),
            floor(clampf(src_b + dst_b * one_minus_a, 0.0, 1.0) * 255.0 + 0.5),
            floor(clampf(alpha + dst_a * one_minus_a, 0.0, 1.0) * 255.0 + 0.5),
        );
    }

    // finalize_pixmap 相当: a==0 → rgb=0、a<255 → straight = round(rgb_u8 * 255 / a_u8)、
    // a>=255 → そのまま。出力は straight rgba（read-back は Rgba8Unorm）。
    let a8 = acc.a;
    if (a8 <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    if (a8 >= 255.0) {
        return vec4<f32>(acc.r / 255.0, acc.g / 255.0, acc.b / 255.0, 1.0);
    }
    let inv = 255.0 / a8;
    let sr = floor(clampf(acc.r * inv, 0.0, 255.0) + 0.5);
    let sg = floor(clampf(acc.g * inv, 0.0, 255.0) + 0.5);
    let sb = floor(clampf(acc.b * inv, 0.0, 255.0) + 0.5);
    return vec4<f32>(sr / 255.0, sg / 255.0, sb / 255.0, a8 / 255.0);
}
