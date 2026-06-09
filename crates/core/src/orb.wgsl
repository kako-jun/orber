// orber #207 / #235 / #242 / #241 — orb の production WGSL（native CLI / wgpu）。
//
// **orb の機構を全 shape の唯一の機構にしたもの（#235）。** 元は orb_circle.wgsl
// （旧 WebGL の orb アームを 1:1 で WGSL 化）だったが、#235 で
// 「orb に別のシルエットを食わせる」形へ一般化した: 各ピクセルの "形からの距離" を
// 正規化した r を、3 軸呼吸・falloff 曲線・straight alpha の float Source-Over 合成
// （#242 で旧 WebGL のアルゴリズムを 1:1 採用）へ食わせる。
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
//             falloff_curve / composite_straight へ渡す。#239 で aquarelle のにじみ
//             （bleed=シルエットが溶ける量）を統一機構の上の watercolor モーフとして
//             全 shape に乗せた（後述 watercolor_bleed）。bleed=0 では経路に入らないので
//             plain orb / glyph / image の既存描画は byte 不変（非回帰ゲート）。
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
//   - 旧 64 制限は固定 uniform-array レンダラ
//     （crates/wasm/src/lib.rs::GL_RENDERER_MAX_ORBS）だけのもので、この WGSL とは無関係。
//     誤って 64 を同期させないこと。
//
// 旧 WebGL との対応（#242 裁定: 旧 WebGL の合成アルゴリズムが正）:
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
    // #239: aquarelle のにじみ。`aqua_bleed` は「シルエットが溶ける量」の連続値で、
    // 0=素のグリフ（plain orb と byte 一致）→ 1=形のない柔らかい色の雲 を連続モーフする。
    // **bleed=0 のとき watercolor 経路に入らない**ので非回帰ゲートを満たす。aquarelle
    // 以外の shape（orb / glyph / image）の既存描画では gpu.rs が 0 を流すので不変。
    // bloom / offset / halo は受け口だけ維持の no-op（今回は溶け一本に集中）。
    aqua_bleed: f32,       // シルエットが溶ける量（0=素の形 → 1=formless な雲）
    aqua_bloom: f32,       // 予約（現状 no-op）
    aqua_offset: f32,      // 予約（現状 no-op）
    aqua_halo: f32,        // 予約（現状 no-op）
};

// per-orb のパック（data-texture, #210 / #212 Phase 1b で 4 texel 化）。
// pack_render_data_for_webgl の per-orb 16 words を gpu.rs が
// **幅4 texel × 高さ N** の Rgba32Float テクスチャへ詰める。texel レイアウトは
//   x=0: color = (r, g, b, weight)
//   x=1: phase = (phase, phi_radius, phi_blur, phi_opacity)
//   x=2: misc  = (cross_axis, style_bit, speed_mult, _)
//   x=3: rot   = (base_angle, rot_speed_signed, _, _)  ← SDF source（glyph/image）専用、orb は無視
//   y  : orb index
// 並びは旧 WebGL の u_orb_color / u_orb_phase / u_orb_misc と同じ。
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

// 旧 WebGL の falloff_curve を 1:1 移植した falloff（#242 裁定）に、
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

// #239 — aquarelle にじみ（bleed = 「シルエットが溶ける量」の連続値）。
//
// 設計（kako-jun 確定 spec #239）: bleed 1 本で「素のグリフ → formless な柔らかい色の雲」を
// **連続モーフ**する。前回の「輪郭に枠リングを足す」加算レイヤー（halo/bloom/blob）は
// kako-jun に却下されたため**撤去**した。今回は溶け（bleed）一本に集中する。
//   - bleed=0      : 素のシルエット（星なら星）。plain orb と **byte 一致**（非回帰ゲート）
//   - bleed 小0.2  : シルエットが多少ぼやける（形は明確）
//   - bleed 中0.5  : かなり柔らかく、形が崩れ始める
//   - bleed 大1.0  : 形が完全に消え、中心が濃く外へ柔らかく拡散する formless な色の雲
//
// モデル（3 段 + 有機ノイズ）:
//   1. モーフ（形を溶かす）: 円距離 r_circle を用意し r_eff = mix(r, r_circle, roundness)。
//      roundness = smoothstep(0,1,bleed) で bleed が上がるほど SDF（星形）→ 円へ。星のトゲが消える。
//   2. ぼかし広げ（柔らかく）: bleed が上がるほど falloff の遷移帯 spread を広げ、中心 r_eff→0 を
//      濃く、外へ連続的に薄く 0 へ落とす単調減少 falloff。**輪郭 r≈1 にピークを作らない**（枠バグ厳禁）。
//   3. 有機的な不規則さ: per-orb seed（phase）由来の滑らかな value noise / fbm で r_eff を
//      domain-warp し、にじみの縁を不規則に揺らす。離散の粒は作らない。warp 量も bleed に比例。
//
// 返り値: watercolor 経路の straight alpha（0..1）。中心が濃く外へ単調に薄れる雲の被覆。
// 合成側（composite_straight）が ramp(bleed) で plain falloff の alpha と mix する:
//   ramp(0)=0 → bleed=0 は完全に plain（byte 一致）。bleed 小は plain にわずかに watercolor を
//   混ぜた=少しぼやけた星になり連続的。
//
// bleed の幾何は A/B 2 変種でモーフ係数だけ違う（AQUA_BLEED_GEOM マーカーが gpu.rs で差し替わる）:
//   - A=continuous: roundness を bleed の smoothstep で滑らかに上げる（標準）
//   - B=blob       : roundness を弱め、noise の warp を強めて縁を粒立たせ気味にする（blink 比較用）。
// bloom / offset / halo は今回 no-op（受け口だけ維持。全0 で plain の byte 一致ゲートのため）。

// 2D value noise（hash → bilinear smoothstep 補間）。seed をオフセットに混ぜて per-orb で変える。
fn aqua_hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}
fn aqua_value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let a = aqua_hash2(i + vec2<f32>(0.0, 0.0));
    let b = aqua_hash2(i + vec2<f32>(1.0, 0.0));
    let c = aqua_hash2(i + vec2<f32>(0.0, 1.0));
    let d = aqua_hash2(i + vec2<f32>(1.0, 1.0));
    let u = f * f * (3.0 - 2.0 * f); // smoothstep
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}
// fbm（2 オクターブ）。-1..1 程度に正規化して domain-warp の変位に使う。
fn aqua_fbm(p: vec2<f32>) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var pp = p;
    for (var o: u32 = 0u; o < 3u; o = o + 1u) {
        v = v + amp * (aqua_value_noise(pp) * 2.0 - 1.0);
        pp = pp * 2.0;
        amp = amp * 0.5;
    }
    return v;
}

// watercolor の straight alpha を返す。`r` はシルエット距離（0=中心,1=edge,>1=外側）。
// `r_circle` は同ピクセルの円距離（中心からのユークリッド距離 / radius）。bleed が上がるほど
// r_eff が r_circle 寄りになり（形が溶ける）、falloff の裾が外へ広がる（柔らかく拡散）。
// roundness_boost: A/B 変種で morph の強さを変える係数。
fn watercolor_bleed(
    r: f32,
    r_circle: f32,
    sample_px: vec2<f32>,
    center: vec2<f32>,
    radius: f32,
    phase: f32,
    opacity: f32,
    roundness_boost: f32,
    warp_boost: f32,
) -> f32 {
    let bleed = params.aqua_bleed;
    if (bleed <= 0.0 || radius <= 0.0 || opacity <= 0.0) {
        return 0.0;
    }

    // 1. モーフ: bleed が上がるほど SDF（星形）→ 円。roundness は smoothstep で滑らかに。
    let roundness = clampf(smoothstep(0.0, 1.0, bleed) * roundness_boost, 0.0, 1.0);
    var r_eff = mix(r, r_circle, roundness);

    // 3. 有機的な不規則さ: per-orb seed と方向で fbm を引き、r_eff を warp する。
    // 縁を不規則に揺らす（連続な有機フチ。離散の粒にしない）。warp 量は bleed に比例（bleed=0 で 0）。
    let to_px = sample_px - center;
    let seed = phase * 17.0;
    let warp_freq = 2.2;
    let np = to_px / max(radius, 1.0) * warp_freq + vec2<f32>(seed, seed * 1.7);
    let n = aqua_fbm(np); // -1..1 付近
    let warp_amt = bleed * 0.35 * warp_boost;
    r_eff = max(0.0, r_eff + n * warp_amt);

    // 2. ぼかし広げ: 中心 r_eff→0 が濃く、外へ単調減少して 0 へ。bleed が上がるほど裾 spread を
    // 広げ、外側（r_eff>1）へ薄く拡散させる。**輪郭 r≈1 にピークを作らない**（枠バグ厳禁）。
    // edge0=濃い領域の終端、edge1=完全に 0 になる外端。bleed で edge1 を外へ押し広げる。
    let spread = mix(0.55, 1.7, bleed);
    let edge1 = 1.0 + spread;          // ここで alpha=0
    let falloff = clampf(1.0 - r_eff / edge1, 0.0, 1.0);
    // smoothstep 状の柔らかい裾（指数で中心寄りを膨らませる）。中心ほど濃い単調減少。
    let soft = falloff * falloff * (3.0 - 2.0 * falloff);

    return opacity * soft;
}

// サブピクセル位置 `sample_px` での 1 サンプルを **straight alpha の float
// Source-Over**（旧 WebGL の GLSL と同式、#242）で合成する。
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
        let plain_alpha = fall.x;

        // #239: aquarelle にじみ（溶け）。watercolor の straight alpha を計算し、
        // plain falloff の alpha と ramp(bleed) で **連続モーフ**する。ramp(0)=0 なので
        // bleed=0（および aqua=None で aqua_bleed=0）は完全に plain（byte 一致ゲート）。
        // bleed 小は plain にわずかに watercolor を混ぜた=少しぼやけた星。bleed 大は
        // watercolor 主体の formless な雲。色は orb 色のまま（枠リング・白コアを足さない）。
        // r_circle = 円距離（モーフで星形 SDF → 円へ溶かすのに使う）。
        var alpha = plain_alpha;
        if (params.aqua_bleed > 0.0) {
            let r_circle = distance(sample_px, vec2<f32>(cx, cy)) / radius;
            //!ORB_AQUA_BLEED_GEOM
            let wc_alpha = watercolor_bleed(
                r, r_circle, sample_px, vec2<f32>(cx, cy), radius, phase, opacity,
                aqua_roundness_boost, aqua_warp_boost
            );
            // ramp: bleed=0 で 0（完全に plain=byte一致）、bleed が上がるほど watercolor 主体へ。
            // smoothstep(0,0.75) で **bleed≈0.75 までに plain（くっきりした形）を消し切る**。
            // こうしないと mix に plain_alpha が残り、雲の中に crisp な星の芯が透けてしまう
            // （b=0.8 でも星がはっきり見える不具合）。連続かつ ramp(0)=0 は維持。
            let ramp = smoothstep(0.0, 0.75, params.aqua_bleed);
            alpha = mix(plain_alpha, wc_alpha, ramp);
        }

        if (alpha > 0.0) {
            // Source-Over（straight alpha）。GLSL と同式 + #241 影スケール:
            //   out.rgb = (src.rgb * shadow_scale) * src.a + out.rgb * (1 - src.a)
            //   out.a   = src.a + out.a * (1 - src.a)
            // watercolor 経路では影スケール(fall.y)は使わない（雲は均一な orb 色）。
            var rgb_scale = fall.y;
            if (params.aqua_bleed > 0.0) {
                rgb_scale = mix(fall.y, 1.0, smoothstep(0.0, 0.75, params.aqua_bleed));
            }
            let one_minus_a = 1.0 - alpha;
            acc_rgb = o.color.rgb * rgb_scale * alpha + acc_rgb * one_minus_a;
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
