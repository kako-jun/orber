// orber #214 Phase 1b.5 — Glyph orb の aquarelle bleed/halo パスを WGSL 化。
//
// CPU 経路（`crate::animate::render_frame` が全 orb 描画後に 1 回かける
// `aquarelle::render_aquarelle_bleed_pass`、default params seed=0）を 2nd pass 群
// として GPU に写したもの。glyph fill（#212、straight RGBA を中間テクスチャに描画）
// を入力に、premultiply → 分離 box-blur×3 → halo saturation → compose → finalize
// の順で backbuffer に straight RGBA を出力する。
//
// aquarelle 0.2 `render_aquarelle_bleed_pass`（default: radius=3.0, intensity=0.5,
// halo=0.3）のアルゴリズム:
//   1. premult RGBA スナップショット original → blurred = original.clone()
//   2. box_radius = round(radius*1.15).max(1) = 3 の 分離 box-blur を ×3
//      （各回 H→V）。box-blur は clamp-to-edge、window = 2r+1 の単純平均。
//      crate は各 H/V パスごとに u8 へ量子化する（中間が Rgba8Unorm なので一致）。
//   3. halo: boost_saturation_buffer(blurred, 1.0 + 0.6*halo) = ×1.18。
//      un-premult → sRGB→HSL → saturation*factor を clamp(0,1) → HSL→sRGB → re-premult。
//      a==0 は hue 未定義なのでスキップ。
//   4. paper-grain noise（ChaCha8Rng(seed=0) をピクセル順消費、振幅 ±0.1*intensity）。
//      **GPU では省略する。** ChaCha8 のピクセル順消費は並列 GPU で bit 一致再現が
//      非現実的で、効果も faint（±0.05）。loose parity の確定方針。CPU との差は
//      期待値（将来 WGSL hash で近似可）。
//   5. compose: dst = original*(1-intensity) + blurred*intensity（全 channel・premult）。
//      intensity = 0.5。
//
// すべて premult RGBA（0..1）で処理し、最後の compose で finalize（un-premult して
// straight 化）する。box-blur は textureLoad（整数 texel fetch）で隣接 texel を読み、
// CPU の格子点平均と一致させる（sampler 補間は使わない）。clamp-to-edge は
// clamp(coord, 0, dim-1) で実装。
//
// パリティ範囲: bit-exact は課さない。構造 + 緩い許容（halo が lit cluster 周囲に出る /
// lit pixel が bleed 後も visible / 空 clusters・weight0 は背景黒）。noise 省略ぶんと
// HSL 実装差は CPU と差が出るが期待値。

struct BleedParams {
    resolution: vec2<f32>, // (width, height) px
    radius: f32,           // box-blur 半径（texel）。default 3。
    premultiply: f32,      // 1.0 = 入力を straight とみなし読み込み時に premult 化（iter0 H のみ）
    halo_factor: f32,      // saturation 倍率（1.0 + 0.6*halo = 1.18）
    intensity: f32,        // compose の混合率（0..1、default 0.5）
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> params: BleedParams;
@group(0) @binding(1) var src: texture_2d<f32>;
// compose だけが 2 枚目（blurred）を読む。他パスは binding 2 を src と同じ view に
// バインドして無視する（bind group layout を 1 つに統一するため）。
@group(0) @binding(2) var blurred_tex: texture_2d<f32>;

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

fn clampi(x: i32, lo: i32, hi: i32) -> i32 {
    return min(max(x, lo), hi);
}

// src texel を読む。premultiply=1.0 のときは straight とみなして rgb *= a する。
fn load_premult(ix: i32, iy: i32) -> vec4<f32> {
    let c = textureLoad(src, vec2<i32>(ix, iy), 0);
    if (params.premultiply > 0.5) {
        return vec4<f32>(c.rgb * c.a, c.a);
    }
    return c;
}

// 量子化（CPU の各 box-blur パス末尾 `(v).clamp(0,255).round() as u8` を写す）。
// 中間ターゲットは Rgba8Unorm なので store で同じ丸めが起きるが、明示しておく。
fn quantize8(v: vec4<f32>) -> vec4<f32> {
    return floor(clamp(v, vec4<f32>(0.0), vec4<f32>(1.0)) * 255.0 + 0.5) / 255.0;
}

// 水平 box-blur（clamp-to-edge、window = 2r+1 の単純平均）。
// CPU box_blur_horizontal: radius を width-1 で cap、左右端は端 texel にクランプ。
@fragment
fn fs_blur_h(in: VsOut) -> @location(0) vec4<f32> {
    let w = i32(params.resolution.x);
    let h = i32(params.resolution.y);
    let x = clampi(i32(floor(in.pos.x)), 0, w - 1);
    let y = clampi(i32(floor(in.pos.y)), 0, h - 1);
    let r = min(i32(params.radius), w - 1);
    let window = f32(2 * r + 1);
    var sum = vec4<f32>(0.0);
    for (var k: i32 = -r; k <= r; k = k + 1) {
        let sx = clampi(x + k, 0, w - 1);
        sum = sum + load_premult(sx, y);
    }
    return quantize8(sum / window);
}

// 垂直 box-blur。CPU box_blur_vertical の写し。premultiply は H パスだけが立てる
// （V パスの入力は既に premult なので premultiply=0）。
@fragment
fn fs_blur_v(in: VsOut) -> @location(0) vec4<f32> {
    let w = i32(params.resolution.x);
    let h = i32(params.resolution.y);
    let x = clampi(i32(floor(in.pos.x)), 0, w - 1);
    let y = clampi(i32(floor(in.pos.y)), 0, h - 1);
    let r = min(i32(params.radius), h - 1);
    let window = f32(2 * r + 1);
    var sum = vec4<f32>(0.0);
    for (var k: i32 = -r; k <= r; k = k + 1) {
        let sy = clampi(y + k, 0, h - 1);
        sum = sum + load_premult(x, sy);
    }
    return quantize8(sum / window);
}

// sRGB（encoded、0..1）→ HSL。palette の Hsl::from_color(Srgb) は encoded 値を
// 直接 HSL 化する（linearize しない）ので、標準 RGB→HSL をそのまま使う。
// 返り値 = (h[0,1), s[0,1], l[0,1])。
fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let maxc = max(rgb.r, max(rgb.g, rgb.b));
    let minc = min(rgb.r, min(rgb.g, rgb.b));
    let l = (maxc + minc) * 0.5;
    let delta = maxc - minc;
    var h = 0.0;
    var s = 0.0;
    if (delta > 0.0) {
        if (l < 0.5) {
            s = delta / (maxc + minc);
        } else {
            s = delta / (2.0 - maxc - minc);
        }
        if (maxc == rgb.r) {
            h = (rgb.g - rgb.b) / delta;
            if (rgb.g < rgb.b) {
                h = h + 6.0;
            }
        } else if (maxc == rgb.g) {
            h = (rgb.b - rgb.r) / delta + 2.0;
        } else {
            h = (rgb.r - rgb.g) / delta + 4.0;
        }
        h = h / 6.0;
    }
    return vec3<f32>(h, s, l);
}

fn hue_channel(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

// HSL → sRGB（encoded、0..1）。標準逆変換。
fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s <= 0.0) {
        return vec3<f32>(l, l, l);
    }
    var q = 0.0;
    if (l < 0.5) {
        q = l * (1.0 + s);
    } else {
        q = l + s - l * s;
    }
    let p = 2.0 * l - q;
    let r = hue_channel(p, q, h + 1.0 / 3.0);
    let g = hue_channel(p, q, h);
    let b = hue_channel(p, q, h - 1.0 / 3.0);
    return vec3<f32>(r, g, b);
}

// halo: boost_saturation_buffer(blurred, halo_factor) を写す。premult を un-premult
// してから HSL saturation を倍率し、re-premult する。a==0 は hue 未定義でスキップ。
@fragment
fn fs_halo(in: VsOut) -> @location(0) vec4<f32> {
    let w = i32(params.resolution.x);
    let h = i32(params.resolution.y);
    let x = clampi(i32(floor(in.pos.x)), 0, w - 1);
    let y = clampi(i32(floor(in.pos.y)), 0, h - 1);
    let c = textureLoad(src, vec2<i32>(x, y), 0); // premult
    let a = c.a;
    if (a <= 0.0) {
        return c;
    }
    // un-premult（CPU は u8/255 を a で割る。ここでは float のまま）。
    let straight = clamp(c.rgb / a, vec3<f32>(0.0), vec3<f32>(1.0));
    var hsl = rgb_to_hsl(straight);
    hsl.y = clamp(hsl.y * params.halo_factor, 0.0, 1.0);
    let out = clamp(hsl_to_rgb(hsl), vec3<f32>(0.0), vec3<f32>(1.0));
    // re-premult。中間が Rgba8Unorm なので store で u8 量子化される。
    return vec4<f32>(out * a, a);
}

// compose + finalize: dst_premult = original*(1-t) + blurred*t、その後 finalize で
// straight 化して出力（backbuffer は Rgba8Unorm）。original は src（glyph fill の
// straight）を premult 化して再現する（CPU の original premult スナップショット相当）。
@fragment
fn fs_compose(in: VsOut) -> @location(0) vec4<f32> {
    let w = i32(params.resolution.x);
    let h = i32(params.resolution.y);
    let x = clampi(i32(floor(in.pos.x)), 0, w - 1);
    let y = clampi(i32(floor(in.pos.y)), 0, h - 1);

    // original = glyph fill（straight）を premult 化。
    let fill = textureLoad(src, vec2<i32>(x, y), 0);
    let orig = vec4<f32>(fill.rgb * fill.a, fill.a);
    // blurred は既に premult。
    let blur = textureLoad(blurred_tex, vec2<i32>(x, y), 0);

    let t = params.intensity;
    let inv = 1.0 - t;
    // CPU は全 channel を *255 round してから premult のまま保持。ここでは float の
    // premult で混合する（compose 後 finalize するので途中量子化は省く）。
    var composed = orig * inv + blur * t;
    composed = clamp(composed, vec4<f32>(0.0), vec4<f32>(1.0));

    // finalize_pixmap 相当: a==0 → 0、それ以外は straight = premult_rgb / a。
    let a = composed.a;
    if (a <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let straight = clamp(composed.rgb / a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(straight, a);
}
