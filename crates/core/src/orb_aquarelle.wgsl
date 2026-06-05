// orber #216 Phase 1c — Aquarelle orb の production WGSL（native CLI / wgpu）。
//
// `aquarelle::render_aquarelle_orb`（per-orb・seed = orb index で決定論的）の 4 層
// （offset / main 3-stop radial / 0..3 bleed satellites / bloom core）を WGSL で評価し、
// Skia lowp と同じ流儀（u8 量子化 → premultiply(div255) → source_over(div255)）で
// SourceOver 合成する。描画順は main → satellites → bloom。
//
// ChaCha8 はシェーダに持ち込まない（#216 方針 Option A）:
//   - offset 角・satellite の位置/半径・bloom 有無は seed=orb index から決定論的に
//     決まるので、gpu.rs の pack 段で `ChaCha8Rng::seed_from_u64(i)` を
//     render_aquarelle_orb と完全同順に回して算出し、専用 data-texture に積む。
//   - boost_saturation(HSL)/mix_with_white の色も pack 段（ホスト側）で算出し u8 色として積む
//     （HSL を WGSL 再実装して divergence を生むのを避ける）。
//   - よってこのシェーダは「積まれた中心・半径・色で 3-stop radial を最大 5 回
//     （main + ≤3 satellites + bloom）評価して SourceOver 合成する」だけ。
//
// Skia lowp との対応（パリティ範囲は狭い。過大主張しない）:
//   - 残差は Skia lowp の anti-alias 塗り(fill_path)と WGSL の解析的 radial の差のみ
//     = 構造完全一致・AA だけ緩い許容（Circle と同じ性質。Circle は ±2/ch）。
//   - 3-stop radial 各 stop の straight 色は Skia lowp `Color::from_rgba8` の u8
//     量子化に合わせて pack 段で算出済み（このシェーダは 0..1 float をそのまま補間）。
//
// data-texture レイアウト（Rgba32Float, 幅 AQUARELLE_TEX_WIDTH=9 texel × 高さ N orbs、
//   textureLoad で読む。gpu.rs::ORB の Circle 経路とは別テクスチャ・別 binding）:
//   x=0: main = (main_cx, main_cy, main_radius, sat_count)         px 座標・px 半径
//   x=1: inner = (inner_r, inner_g, inner_b, bloom_flag)           main の中心色 @255 / bloom 有無
//   x=2: halo  = (halo_r, halo_g, halo_b, _)                       main の mid@128 / edge@0 色
//   x=3: bloom_geom = (bloom_cx, bloom_cy, bloom_core_radius, _)
//   x=4: bloom_col  = (bloom_r, bloom_g, bloom_b, _)               mix_with_white 済み色
//   x=5: bleed_col   = (bleed_r, bleed_g, bleed_b, _)              satellite 共通色
//   x=6: sat0 = (sat0_cx, sat0_cy, sat0_radius, _)
//   x=7: sat1 = (sat1_cx, sat1_cy, sat1_radius, _)
//   x=8: sat2 = (sat2_cx, sat2_cy, sat2_radius, _)
//   y  : orb index
//
// sampler は持たず textureLoad（texelFetch 相当）のみで読むので filtering 非依存
// （gpu.rs の sample_type は Float{ filterable: false }）。Rgba32Float が filtering
// 非対応な環境でも動き、storage buffer 不使用なので WebGL2 backend でも可搬。

struct Params {
    resolution: vec2<f32>, // (width, height) px
    n_orbs: f32,           // 整数を f32 で（uniform alignment 簡略化のため）
    _pad0: f32,
    bg: vec4<f32>,         // straight rgba (0..1)
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var orb_tex: texture_2d<f32>;

struct AquaOrb {
    main: vec4<f32>,       // (main_cx, main_cy, main_radius, sat_count)
    inner: vec4<f32>,      // (inner_rgb, bloom_flag)
    halo: vec4<f32>,       // (halo_rgb, _)
    bloom_geom: vec4<f32>, // (bloom_cx, bloom_cy, bloom_core_radius, _)
    bloom_col: vec4<f32>,  // (bloom_rgb, _)
    bleed_col: vec4<f32>,  // (bleed_rgb, _)
    sat0: vec4<f32>,       // (sat0_cx, sat0_cy, sat0_radius, _)
    sat1: vec4<f32>,       // (sat1_cx, sat1_cy, sat1_radius, _)
    sat2: vec4<f32>,       // (sat2_cx, sat2_cy, sat2_radius, _)
};

fn load_orb(i: u32) -> AquaOrb {
    let row = i32(i);
    var o: AquaOrb;
    o.main = textureLoad(orb_tex, vec2<i32>(0, row), 0);
    o.inner = textureLoad(orb_tex, vec2<i32>(1, row), 0);
    o.halo = textureLoad(orb_tex, vec2<i32>(2, row), 0);
    o.bloom_geom = textureLoad(orb_tex, vec2<i32>(3, row), 0);
    o.bloom_col = textureLoad(orb_tex, vec2<i32>(4, row), 0);
    o.bleed_col = textureLoad(orb_tex, vec2<i32>(5, row), 0);
    o.sat0 = textureLoad(orb_tex, vec2<i32>(6, row), 0);
    o.sat1 = textureLoad(orb_tex, vec2<i32>(7, row), 0);
    o.sat2 = textureLoad(orb_tex, vec2<i32>(8, row), 0);
    return o;
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

// フルスクリーン三角形（頂点バッファ無し）。orb.wgsl と同形。
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

// Skia lowp の div255: (v + 255) >> 8 == floor((v + 255) / 256)。
// 入力 v は u8*u8 積（0..65025 程度）を float で持つ。orb.wgsl と同一。
fn div255(v: f32) -> f32 {
    return floor((v + 255.0) / 256.0);
}

// straight float (0..1) を Skia lowp の lowp 量子化で u8 (0..255 float) にする。
// rgb は normalize(clamp 0..1) 後に *255+0.5 を floor。alpha は clamp 無し。
// orb.wgsl の to_u8_rgb / to_u8_a と完全に同一（aquarelle の alpha は
// すべて N/255 の定数を mix した [0,1] 値なので clamp は元から no-op）。
fn to_u8_rgb(c: f32) -> f32 {
    return floor(clampf(c, 0.0, 1.0) * 255.0 + 0.5);
}
fn to_u8_a(c: f32) -> f32 {
    return floor(c * 255.0 + 0.5);
}

// 1 つの 3-stop radial（aquarelle::draw_radial 相当）を評価し、straight rgba (0..1)
// を返す。pixel `px` が中心 (cx,cy)・半径 `radius` の円の **radius*1.5 の外** なら
// 描かない（draw_radial は半径×1.5 の円を fill するため）。`.a == 0` で「無描画」。
//
// stop は [0→(inner_rgb, inner_a), mid_stop→(mid_rgb, mid_a), 1→(edge_rgb, edge_a)]。
// Skia lowp (SpreadMode::Pad) は半径外（r>=1）では edge stop の色で clamp する。
// straight 色 / alpha とも線形補間する（aquarelle は inner→mid→edge で rgb も変わる）。
fn eval_radial(
    px: vec2<f32>,
    cx: f32,
    cy: f32,
    radius: f32,
    inner_rgb: vec3<f32>,
    inner_a: f32,
    mid_rgb: vec3<f32>,
    mid_a: f32,
    edge_rgb: vec3<f32>,
    edge_a: f32,
    mid_stop_in: f32,
) -> vec4<f32> {
    if (radius <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let dist = distance(px, vec2<f32>(cx, cy));
    // draw_radial は半径 × 1.5 の円を fill する。その外は描画なし。
    if (dist > radius * 1.5) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let r = dist / radius;
    // mid_stop は Skia lowp 同様 0.05..0.95 にクランプ（GradientStop の clamp）。
    let mid_stop = clampf(mid_stop_in, 0.05, 0.95);

    var rgb: vec3<f32>;
    var a: f32;
    if (r <= 0.0) {
        rgb = inner_rgb;
        a = inner_a;
    } else if (r >= 1.0) {
        // SpreadMode::Pad: 半径外は edge 色で clamp。
        rgb = edge_rgb;
        a = edge_a;
    } else if (r <= mid_stop) {
        var u = 1.0;
        if (mid_stop > 0.0) {
            u = r / mid_stop;
        }
        rgb = mix(inner_rgb, mid_rgb, u);
        a = mix(inner_a, mid_a, u);
    } else {
        let denom = max(1.0 - mid_stop, 1e-6);
        let u = (r - mid_stop) / denom;
        rgb = mix(mid_rgb, edge_rgb, u);
        a = mix(mid_a, edge_a, u);
    }
    return vec4<f32>(rgb, a);
}

// straight rgba (0..1) の src を premultiplied u8 アキュムレータ acc へ SourceOver 合成。
// Skia lowp パイプラインを bit-exact に再現:
//   1. gradient straight color = (rgb, a) を u8 量子化
//   2. premultiply: pr = div255(sr_u8 * sa_u8)
//   3. source_over: out = src_premul + div255(dst_premul * (255 - sa_u8))
// acc は (premul_r, premul_g, premul_b, a8) を 0..255 float で持つ。
fn source_over(acc: vec4<f32>, src: vec4<f32>) -> vec4<f32> {
    let sa8 = to_u8_a(src.a);
    if (sa8 <= 0.0) {
        return acc;
    }
    let sr8 = to_u8_rgb(src.r);
    let sg8 = to_u8_rgb(src.g);
    let sb8 = to_u8_rgb(src.b);
    let pr = div255(sr8 * sa8);
    let pg = div255(sg8 * sa8);
    let pb = div255(sb8 * sa8);
    let inv_sa = 255.0 - sa8;
    return vec4<f32>(
        pr + div255(acc.r * inv_sa),
        pg + div255(acc.g * inv_sa),
        pb + div255(acc.b * inv_sa),
        sa8 + div255(acc.a * inv_sa),
    );
}

// サブピクセル位置 `px` での 1 サンプルを premultiplied u8 (0..255 float) で合成する。
// 背景 → 全 orb（各 orb は main → satellites → bloom）まで SourceOver。
fn composite_premul(px: vec2<f32>) -> vec4<f32> {
    // 背景塗り Pixmap::fill(Color::from_rgba8(...)) は straight 入力を
    // premultiply(div255(c_u8 * a_u8)) で格納する。
    let bg_a8 = to_u8_a(params.bg.a);
    var acc = vec4<f32>(
        div255(to_u8_rgb(params.bg.r) * bg_a8),
        div255(to_u8_rgb(params.bg.g) * bg_a8),
        div255(to_u8_rgb(params.bg.b) * bg_a8),
        bg_a8,
    );

    let count = u32(params.n_orbs + 0.5);
    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let o = load_orb(i);
        let main_radius = o.main.z;
        if (main_radius <= 0.0) {
            continue;
        }
        let inner_rgb = o.inner.rgb;
        let halo_rgb = o.halo.rgb;

        // 1+2. main radial: inner=色@255 / mid=halo色@128 @0.55 / edge=halo色@0。
        let main_col = eval_radial(
            px, o.main.x, o.main.y, main_radius,
            inner_rgb, 255.0 / 255.0,
            halo_rgb, 128.0 / 255.0,
            halo_rgb, 0.0,
            0.55,
        );
        acc = source_over(acc, main_col);

        // 3. bleed satellites: 0..3 個。各 alpha=(100,50,0)、mid_stop=0.5、色=bleed_col。
        let sat_count = u32(o.main.w + 0.5);
        let bleed_rgb = o.bleed_col.rgb;
        if (sat_count >= 1u) {
            let s = eval_radial(
                px, o.sat0.x, o.sat0.y, o.sat0.z,
                bleed_rgb, 100.0 / 255.0,
                bleed_rgb, 50.0 / 255.0,
                bleed_rgb, 0.0,
                0.5,
            );
            acc = source_over(acc, s);
        }
        if (sat_count >= 2u) {
            let s = eval_radial(
                px, o.sat1.x, o.sat1.y, o.sat1.z,
                bleed_rgb, 100.0 / 255.0,
                bleed_rgb, 50.0 / 255.0,
                bleed_rgb, 0.0,
                0.5,
            );
            acc = source_over(acc, s);
        }
        if (sat_count >= 3u) {
            let s = eval_radial(
                px, o.sat2.x, o.sat2.y, o.sat2.z,
                bleed_rgb, 100.0 / 255.0,
                bleed_rgb, 50.0 / 255.0,
                bleed_rgb, 0.0,
                0.5,
            );
            acc = source_over(acc, s);
        }

        // 4. bloom: bloom_flag>0 のとき内側 30% に白寄りコア。
        //    色=mix_with_white(color,0.7)、alpha=(255,128,0)、mid_stop=0.55。
        if (o.inner.w > 0.5) {
            let core_radius = o.bloom_geom.z;
            if (core_radius > 0.0) {
                let bloom_rgb = o.bloom_col.rgb;
                let b = eval_radial(
                    px, o.bloom_geom.x, o.bloom_geom.y, core_radius,
                    bloom_rgb, 255.0 / 255.0,
                    bloom_rgb, 128.0 / 255.0,
                    bloom_rgb, 0.0,
                    0.55,
                );
                acc = source_over(acc, b);
            }
        }
    }

    return acc / 255.0;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // @builtin(position) は既に top-left のピクセル座標（中心 +0.5）。
    // Skia lowp の radial gradient は pixel 中心で point sampling される。
    let pm = composite_premul(in.pos.xy); // premultiplied 0..1
    let acc_rgb8 = pm.rgb * 255.0;
    let acc_a8 = pm.a * 255.0;

    // finalize_pixmap 相当: premultiplied u8 → straight に戻す。
    if (acc_a8 <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    if (acc_a8 >= 255.0) {
        return vec4<f32>(pm.rgb, 1.0);
    }
    let inv = 255.0 / acc_a8;
    let straight = floor(acc_rgb8 * inv + 0.5) / 255.0;
    return vec4<f32>(clamp(straight, vec3<f32>(0.0), vec3<f32>(1.0)), acc_a8 / 255.0);
}
