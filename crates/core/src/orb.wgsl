// orber #207 / #235 / #242 / #241 — orb の production WGSL（native CLI / wgpu）。
//
// **orb の機構を全 shape の唯一の機構にしたもの（#235）。** 元は orb_circle.wgsl
// （`web/src/lib/orberGl.ts` の orb アームを 1:1 で WGSL 化）だったが、#235 で
// 「orb に別のシルエットを食わせる」形へ一般化した: 各ピクセルの "形からの距離" を
// 正規化した r を、3 軸呼吸・falloff 曲線・straight alpha の float Source-Over 合成
// （#242 で旧 WebGL = orberGl.ts のアルゴリズムを 1:1 採用）へ食わせる。
// #241 で最外周フェードセグメントに「薄い影」= rgb 暗化を強度係数
// `params.shadow_strength`（0..1）付きで再導入した（kako-jun 裁定「旧ベース +
// 新のようなアレンジを薄く重ねる」）。s=0 で #242 直後（影なし）と bit 同一、
// s=1 で #242 が撤去した旧 lowp の rgb→0 フェードと同等の暗さ。式は旧 lowp の
// straight-color フェード（rgb_scale = 1-u）を `mix(1.0, 1.0-u, s)` に係数化した
// だけで、新規のカーブは導入していない。
// この距離の出どころ（DISTANCE SOURCE ブロック）だけが shape ごとに差し替わる:
//   - orb   : 解析的な円距離（`distance(px, center) / radius`）。
//   - glyph / image : SDF サンプル（SDF はまさに "形からの距離" そのもの）。回転を
//             SDF サンプル前に適用し、signed 距離 → 正規化 r に変換して **同じ**
//             falloff_curve / composite_straight へ渡す。にじみ（bleed/halo）の追加
//             パスは持たない（#235 で撲滅。にじみは aquarelle shape の領分）。
//
// このシェーダは Rust 側（gpu.rs::orb_wgsl / orb_sdf_wgsl）で DISTANCE SOURCE ブロック /
// 追加 binding を文字列合成して 2 variant を生成する。共通部（header / per-orb 算術 /
// falloff / 合成）は完全に共有され、「orb に別のものを食わせる」が文字どおり
// の実装になっている。orb variant は SDF binding を一切持ち込まない（dummy binding
// 不可）ので、2 variant の差は DISTANCE SOURCE だけに限定される。
// DISTANCE SOURCE ブロックは loop 本体に**インライン展開**される。
//
// per-orb データは `pack_render_data_for_webgl`（= WebGL 経路と同一）が詰めた
// header(16 words) + per-orb(16 words × n_orbs) を gpu.rs が data-texture に詰め替え、
// このシェーダは textureLoad で読む。パラメータ算術は再実装せず、gpu.rs 側で pack
// 出力を vec4 にほどいて texture へ upload する。
//
// orb 上限について（#210 Phase 1a）:
//   - このシェーダは per-orb データを uniform 固定配列ではなく **data-texture**
//     (Rgba32Float, textureLoad) で読むため、**64 制限を持たない**。動的 count
//     (= params.n_orbs、≤ MAX_ORB_COUNT(1024)) までフラグメントループで描ける。
//   - 旧 64 制限は WebGL GLSL 経路（web/src/lib/orberGl.ts::MAX_ORBS /
//     crates/wasm/src/lib.rs::GL_RENDERER_MAX_ORBS）だけのもので、この WGSL とは無関係。
//     誤って 64 を同期させないこと。
//
// WebGL (orberGl.ts) との対応（#242 裁定: 旧 WebGL の合成アルゴリズムが正）:
//   - falloff_curve（stop alpha は raw float）と straight alpha の float Source-Over
//     合成を GLSL fragment shader から 1:1 移植。
//     かつての Skia lowp 再現（stop alpha の u8 量子化・最外周セグメントの rgb→0
//     フェード・u8 premultiply div255 合成・premul→straight finalize）は #242 で撤去:
//     最外周フェードの rgb→0 が暗部を一様に沈め、旧 WebGL より暗い出力になっていた
//     （mean_signed R+30.7/G+23.2/B+24.2、diff は輝度と逆相関 corr=−0.962）。
//   - #241 で最外周セグメントの rgb フェードだけを強度係数 shadow_strength 付きで
//     薄く再導入（falloff_curve の doc 参照）。s>0 では WGSL が旧 WebGL より外周帯で
//     僅かに暗く（= 影が乗って）なるのが正。u8 量子化や premultiply 合成は復活しない。
//   - saturation は pack_render_data_for_webgl では掛けず（WebGL と共有のため）、
//     native GPU 側 gpu.rs::render_frame が adjust_saturation_pub を後段適用して揃える。
//     color/keyframe tracks は GPU pack 未対応で cluster 列経由。
//
// 座標系:
//   GLSL は gl_FragCoord(bottom-left) を `px.y = H - px.y` で top-left に直してから
//   出力（image::RgbaImage, top-left）と合わせていた。WGSL の @builtin(position) は
//   既に top-left（pixel 中心 +0.5）で、読み戻しも top-left のままなので **flip 不要**。
//   よって `in.pos.xy` をそのまま GLSL の flip 後 `px` として使う。
//
// 仕様の数式（GLSL と一致）:
//   - r_pixels_max = base_radius * sqrt(weight) * 1.10
//   - r_normalized = r_pixels_max / progress_axis_pixels
//   - extent = 1 + 2 * r_normalized
//   - advance_steps = fract(cycle * speed_mult * t)
//   - pos = mod(phase*extent + advance_steps*extent, extent) - r_normalized
//   - 3 軸独立呼吸: radius ±10%, blur ±15, opacity ±5%（sin(TAU*fract(t)+phi)）
//   - rim: 3-stop（center=opacity, mid=opacity*80/255, mid_stop=clamp(1-blur*0.8,.05,.95)）
//   - soft: 2-stop（hold=opacity, hold_stop=clamp(1-blur,.05,.95)）
//   - 影（#241、GLSL には無い上乗せ）: 最外周セグメントで rgb_scale = mix(1, 1-u, s)
//   - Source-Over（straight alpha）

const TAU: f32 = 6.28318530718;
const BREATH_RADIUS_MAX_FACTOR: f32 = 1.10;
// 1/√2。crate::glyph::GLYPH_SDF_CONTENT_SPAN と同期（SDF DISTANCE SOURCE 用）。Rust 側
// から override せずここに定数で持つ（Rust 側の値と同値であることを gpu.rs のテストで担保）。
const GLYPH_SDF_CONTENT_SPAN: f32 = 0.70710678;

struct Params {
    resolution: vec2<f32>, // (width, height) px
    t: f32,                // [0, 1)
    base_radius: f32,      // px = min(w,h) * 0.25 * orb_size
    bg: vec4<f32>,         // straight rgba (0..1)
    base_blur: f32,        // 0..1
    direction: f32,        // 0=LR, 1=RL, 2=TB, 3=BT
    cycle: f32,            // 1=VerySlow, 2=Slow, 3=Mid, 4=Fast
    n_orbs: f32,           // 整数を f32 で（uniform alignment 簡略化のため）
    alpha_mul: f32,        // softness.alpha_mul
    glyph_rotate: f32,     // #136: 1.0=ON / 0.0=OFF（SDF source のみ使用、orb は無視）
    edge_softness: f32,    // #205: 予約（現状未使用）
    sdf_size: f32,         // glyph/image SDF の一辺（texel 数）。orb は 0。
    shadow_strength: f32,  // #241: 最外周フェードの rgb 暗化強度（0..1）。0 = #242 と bit 同一
};

// per-orb のパック（data-texture, #210 / #212 Phase 1b で 4 texel 化）。
// pack_render_data_for_webgl の per-orb 16 words を gpu.rs が
// **幅4 texel × 高さ N** の Rgba32Float テクスチャへ詰める。texel レイアウトは
//   x=0: color = (r, g, b, weight)
//   x=1: phase = (phase, phi_radius, phi_blur, phi_opacity)
//   x=2: misc  = (cross_axis, style_bit, speed_mult, _)
//   x=3: rot   = (base_angle, rot_speed_signed, _, _)  ← SDF source（glyph/image）専用、orb は無視
//   y  : orb index
// 並びは orberGl.ts の u_orb_color / u_orb_phase / u_orb_misc と同じ。
//
// sampler は持たず textureLoad（texelFetch 相当）のみで読むので linear filtering に
// 依存しない（gpu.rs の sample_type は Float{ filterable: false }）。これにより
// Rgba32Float が filtering 非対応な環境でも動き、storage buffer 不使用なので
// wgpu の WebGL2 backend でも可搬（Phase 2）。
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var orb_tex: texture_2d<f32>;
//!ORB_EXTRA_BINDINGS

// orb i の vec4 群を data-texture から読む（mip 0、point fetch）。Orb 構造体と load_orb
// は variant ごとに差し替わる（DISTANCE SOURCE が必要とするフィールドだけを読む）:
//   - orb : color / phase / misc の 3 texel（rot は読まない）
//   - SDF : 上記 + rot (x=3) texel（回転に base_angle / rot_speed_signed を使う）
//!ORB_LOAD

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

// フルスクリーン三角形（頂点バッファ無し）。crossfade.wgsl と同形。
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

//!ORB_HELPERS

// 旧 WebGL（orberGl.ts）の falloff_curve を 1:1 移植した falloff（#242 裁定）に、
// #241 の「薄い影」（最外周フェードの rgb 暗化、強度係数付き）を重ねたもの。
//
// 返り値 `.x` = straight alpha (0..1) の **raw float**。stop alpha の u8 量子化は
// しない（GLSL は center_a = opacity / mid_a = opacity * 80/255 を float のまま
// 線形補間する）。
// 返り値 `.y` = orb 色に掛ける rgb スケール (0..1)。内側セグメントでは常に 1.0
// （orb 色一定 = #242 の旧 WebGL 式そのまま）。**最外周フェードセグメントだけ**
// `mix(1.0, 1.0 - u, params.shadow_strength)` — 旧 lowp（#242 で撤去）は同じ場所で
// rgb_scale = 1-u と rgb を 0 へフェードさせており、これが「新のくっきりした影」の
// 正体だった。#241 はその旧式を強度係数 s で係数化しただけ:
//   s=0 → scale = 1.0（#242 直後と bit 同一: mix(1,x,0)=1 の乗算は恒等）
//   s=1 → scale = 1-u（旧 lowp の straight-color フェードと同等の暗さ。u8 量子化はしない）
// Rim / Soft とも旧 lowp と同じく最外周セグメントのみ暗化する（内側は不変）。
//
//   Rim  stops: [0→center_a, mid_stop→mid_a, 1→0]（alpha 補間 + 外周帯 rgb 暗化）
//   Soft stops: [0→opacity, hold_stop→opacity, 1→0]（同上）
//
// style_bit < 0.5 が Rim、それ以外が Soft。
//
// SDF DISTANCE SOURCE（glyph/image）でも **同じこの falloff_curve** を使う（#235）。
// 形（r の出どころ）だけが違い、ぼやけ方・rim/soft・合成・影は orb と完全に共通
// （= 影は quad 矩形ではなく r ベースで自動的にシルエット沿いになる）。
fn falloff_curve(style_bit: f32, r_in: f32, blur: f32, opacity: f32) -> vec2<f32> {
    if (opacity <= 0.0 || r_in >= 1.0) {
        return vec2<f32>(0.0, 0.0);
    }
    let r = max(r_in, 0.0);
    if (style_bit < 0.5) {
        let center_a = opacity;
        let mid_a = opacity * (80.0 / 255.0);
        let mid_stop = clampf(1.0 - blur * 0.8, 0.05, 0.95);
        if (r <= mid_stop) {
            var u = 1.0;
            if (mid_stop > 0.0) {
                u = r / mid_stop;
            }
            return vec2<f32>(mix(center_a, mid_a, u), 1.0);
        }
        let denom = max(1.0 - mid_stop, 1e-6);
        let u = (r - mid_stop) / denom;
        return vec2<f32>(mix(mid_a, 0.0, u), mix(1.0, 1.0 - u, params.shadow_strength));
    }
    let hold_stop = clampf(1.0 - blur, 0.05, 0.95);
    if (r <= hold_stop) {
        return vec2<f32>(opacity, 1.0);
    }
    let denom = max(1.0 - hold_stop, 1e-6);
    let u = (r - hold_stop) / denom;
    return vec2<f32>(mix(opacity, 0.0, u), mix(1.0, 1.0 - u, params.shadow_strength));
}

// サブピクセル位置 `sample_px` での 1 サンプルを **straight alpha の float
// Source-Over**（旧 WebGL = orberGl.ts の GLSL と同式、#242）で合成する。
// 背景 → 全 orb の Source-Over まで。量子化・premultiply・finalize は無い。
fn composite_straight(sample_px: vec2<f32>) -> vec4<f32> {
    // 進行軸長（LR/RL=width, TB/BT=height）。GLSL: u_direction < 1.5。
    var progress_axis = params.resolution.y;
    if (params.direction < 1.5) {
        progress_axis = params.resolution.x;
    }

    // 背景塗り（straight alpha）。GLSL: acc_rgb = u_bg.rgb; acc_a = u_bg.a;
    var acc_rgb = params.bg.rgb;
    var acc_a = params.bg.a;

    // count はヘッダ由来の動的 orb 数（gpu.rs が MAX_ORB_COUNT(1024) まで clamp 済み）。
    // 固定上限ループではなく count まで回す（data-texture は 64 制限を持たない）。
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

        let r_pixels_max = params.base_radius * sqrt(max(weight, 0.0)) * BREATH_RADIUS_MAX_FACTOR;
        var r_normalized = 0.0;
        if (progress_axis > 0.0) {
            r_normalized = r_pixels_max / progress_axis;
        }
        let extent = 1.0 + 2.0 * r_normalized;

        let advance_steps = fract(params.cycle * speed_mult * params.t);
        let raw = phase * extent + advance_steps * extent;
        // WGSL の `x - y*floor(x/y)` を mod 相当として使う（負を返さない、
        // Rust rem_euclid と一致）。GLSL mod() と同義。
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

        // === DISTANCE SOURCE ブロック（loop 本体にインライン展開）===
        // orb = 円距離 / SDF(glyph・image) = サンプル距離。`r`（0=中心/深部、1=edge、
        // >1=外側）をここで定義する。SDF は UV 範囲外で `continue` する。下流
        // （falloff_curve / 合成）は全 shape 共通。orb variant はこのブロックを
        // GLSL の orb アームと同一の 2 行（dist / r）に展開する。
        //!ORB_DISTANCE_SOURCE
        // === /DISTANCE SOURCE ブロック ===

        // .x = straight alpha（raw float）、.y = rgb スケール（#241 の薄い影:
        // 最外周フェード帯のみ < 1。shadow_strength=0 なら常に 1.0 = #242 と bit 同一）。
        let fall = falloff_curve(style_bit, r, blur, opacity);
        let alpha = fall.x;

        if (alpha > 0.0) {
            // Source-Over（straight alpha）。GLSL と同式 + #241 影スケール:
            //   out.rgb = (src.rgb * shadow_scale) * src.a + out.rgb * (1 - src.a)
            //   out.a   = src.a + out.a * (1 - src.a)
            let one_minus_a = 1.0 - alpha;
            acc_rgb = o.color.rgb * fall.y * alpha + acc_rgb * one_minus_a;
            acc_a = alpha + acc_a * one_minus_a;
        }
    }

    // straight rgba (0..1)。GLSL: outColor = vec4(acc_rgb, acc_a)。
    return vec4<f32>(acc_rgb, acc_a);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // @builtin(position) は既に top-left のピクセル座標（中心 +0.5）。
    // GLSL の `px.y = H - px.y` 後の px と同じ意味なので flip しない。
    // straight rgba をそのまま書き出す（premul→straight finalize は #242 で撤去。
    // Rgba8Unorm/Bgra8Unorm ターゲットへの書き込みで round(value*255) に量子化
    // されるのは GLSL → canvas と同じ）。
    return composite_straight(in.pos.xy);
}
