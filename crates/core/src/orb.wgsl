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
//             （bleed=ふつうのブラー量）を**本物の空間ブラー**（multi-tap 平均）として
//             全 shape に乗せた（後述 coverage_at / blurred coverage）。bleed=0 では
//             ブラー経路に入らないので plain orb / glyph / image の既存描画は byte
//             不変（非回帰ゲート）。星はブラーで星のままぼけ、強ブラーで自然に溶ける。
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
    // #239: aquarelle のにじみ。`aqua_bleed` は**ふつうのガウスブラー量**で、
    // 0=素のグリフ（plain orb と byte 一致）→ 1=強ブラーで形が溶けた formless な雲。
    // シルエットの被覆 alpha を blur 半径∝bleed の disk 内で multi-tap 空間平均する
    // （= 画像編集のブラー）。**星はブラーで星のままぼけ、距離場を円へモーフしない**。
    // **bleed=0 のときブラー経路に入らない**ので非回帰ゲートを満たす。aquarelle
    // 以外の shape（orb / glyph / image）の既存描画では gpu.rs が 0 を流すので不変。
    //
    // bloom / offset / halo はにじみ(bleed)の上に薄く乗せる「少しだけ複雑な見た目」の
    // character 軸（各 0..1。各 coef=0 で厳密に消え、全0 で plain と byte 一致）:
    //   - bloom : ブラー後の被覆の中心側（avg_a が高い内部）で色を白へ寄せて柔らかい明るい
    //             コアを作る。控えめ（強い白飛びはしない）。term = bloom * centerness。
    //   - halo  : ブラーの外側の柔らかい縁（avg_a が低い帯）で色の**彩度を上げる**だけ。
    //             alpha の枠リングは作らない（kako-jun 却下）。色味だけ鮮やかにする。
    //   - offset: ブラーの disk 原点を per-orb seed 方向へ少しずらし、滲みを左右非対称・
    //             有機的にする。**形は壊さない**（円へモーフしない・星は星のまま）。
    aqua_bleed: f32,       // ブラー量（0=素の形 → 1=強ブラーで formless な雲）
    aqua_bloom: f32,       // 中心の柔らかい明るいコア（0=無し）
    aqua_offset: f32,      // ブラー原点の seed 方向バイアス＝非対称な滲み（0=対称）
    aqua_halo: f32,        // 外周の彩度ブースト（0=無し。枠リングは作らない）
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

// per-pixel ハッシュ（0..1）。#239 multi-tap ブラーのスパイラル初期角を画素ごとに
// ずらし、orb 内でコヒーレントな規則スパイラル（=トゲ状アーティファクト）を画素ごとに
// バラして滑らかなノイズ（ディザ）に散らす。強ブラーでも雲が滑らか＆有機的になる。
fn hash21(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
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

// === COVERAGE 関数（variant 固有。下のマーカー行で gpu.rs が展開）===
// `coverage_at(style_bit, sp, cx, cy, radius, blur, opacity, angle) -> vec2<f32>` を定義する。
// シルエット距離 r を sp（任意のサンプル位置）から求め、共通 falloff_curve に通して
// (straight alpha, rgb_scale) を返す。これが「#235: orb に別のシルエットを食わせる」の
// 唯一の差分点。orb variant=円距離、SDF variant=回転後 SDF サンプル（箱外は alpha=0）。
// plain 経路は sp=sample_px の 1 タップ、ブラー経路は blurred_coverage が複数タップで平均する。
//!ORB_COVERAGE
// === /COVERAGE 関数 ===

// #239 — aquarelle にじみ（bleed = ふつうのガウスブラー量）。
//
// 設計（kako-jun 確定訂正 spec #239）: bleed は**画像編集のガウスブラー量**そのもの。
// 「シルエットの被覆 alpha を空間的にぼかすだけ」。距離場を円形へモーフしていた
// 前回版（kako-jun 却下: 「丸の形に近づけるな」）は **完全削除**。星はブラーで星のままぼけ
// （トゲがブラーで滑らかに広がる）、強ブラーで自然にトゲが平均化されて formless な
// 柔らかい雲になる（REF_original_aquarelle）。丸へモーフではなく、ブラーの帰結。
//   - bleed=0      : 素のシルエット（星なら星）。plain orb と **byte 一致**（非回帰ゲート）
//   - bleed 小0.15 : **明確にぼけた星**（くっきりではない。普通のブラーで柔らかい。星とわかる）
//   - bleed 中0.5  : もっとぼけて星が柔らかく広がる。トゲは星のまま滑らかに溶ける（丸い輪郭は出ない）
//   - bleed 大1.0  : ブラーが強く形が溶けて formless な柔らかい雲
//
// 実装（本物の空間ブラー = multi-tap box/disk 平均）:
//   被覆 `coverage_at(sp, ...)`（= シルエット距離 r → falloff_curve → straight alpha）を
//   `sample_px` を中心とする blur 半径 `blur_px ∝ bleed`（px 単位、radius にスケール）の
//   disk 内に黄金角スパイラルで分布させた N タップ位置 `sample_px + offset_k` で評価し、
//   **単純平均**する。これがガウス近似ブラー。タップ原点 phase を per-orb で回し、規則的
//   パターンを避ける（離散の粒は作らない。ブラーは本質的に滑らかなので担保できる）。
//   falloff の式自体は plain と同じ。被覆を空間平均するだけ。SDF variant は coverage_at 内で
//   offset 位置の SDF をサンプルし、箱外は被覆 0 扱い（円へモーフしない）。
//
// **常にぼやけ**: bleed>0 では blur_px に下限を設け、最小 bleed でも明確にぼける（crisp 不可）。
//
// A/B 2 変種（AQUA_BLEED_GEOM マーカーが gpu.rs で差し替わる）はブラー係数だけ違う:
//   - A=continuous: 標準のブラー半径（blur_scale=1.0）
//   - B=blob       : やや強いブラー半径（blur_scale=1.4。blink 比較用）。
// どちらも距離場をモーフしない（丸へ寄せる係数は持たない）。
// bloom / offset / halo は今回 no-op（受け口だけ維持。全0 で plain の byte 一致ゲートのため）。

// ブラーのタップ数。静止画 PoC なので品質優先で多めに取る（重くてよい）。
const AQUA_BLUR_TAPS: u32 = 48u;
// 黄金角（ラジアン）。disk 内に等密度でタップを撒くスパイラルに使う（規則格子を避ける）。
const AQUA_GOLDEN_ANGLE: f32 = 2.39996323;
// bloom の白へ寄せる上限（offset=1 でもこの比までしか白くしない）。控えめにして
// 強い白飛びを禁止する（kako-jun「控えめに」）。
const BLOOM_MAX: f32 = 0.45;
// halo の彩度ゲイン係数（halo=1・縁で luma 軸からの距離をこの倍まで伸ばす上限）。
const HALO_SAT_GAIN: f32 = 0.6;
// offset の disk 原点バイアス量（blur_px に対する比。offset=1 でこの比だけ seed 方向へ
// ずらす）。形は壊さず滲みを非対称にする程度に控えめ。
const AQUA_OFFSET_BIAS: f32 = 0.6;

// #239 ブラー経路の空間早期カル用の「シルエット最大到達」（radius 倍）。被覆が確実に 0 に
// なる距離の上限を **安全側に大きく** 取るための定数。出力は一切変えない（カルするのは
// もともと alpha=0 になる画素だけ）。両 variant の最大到達を包む保守値:
//   - orb variant   : r=distance/radius、coverage_at は r>=1 で 0 → 到達 = radius（係数 1.0）。
//   - SDF variant   : サンプル箱は半幅 radius/CONTENT_SPAN（CONTENT_SPAN=1/√2）の正方形。
//                     中心からの最遠点は角で radius/CONTENT_SPAN * √2 = radius*2 → 係数 2.0。
// 共有 composite_straight は両 variant に展開されるので、大きい方（2.0）を採る。
const AQUA_REACH_RADIUS_FACTOR: f32 = 2.0;
// タップが sample_px から離れうる最大比（blur_px 倍）。disk 半径 blur_px + offset bias
// （最大 AQUA_OFFSET_BIAS*blur_px=0.6*blur_px）→ 1.0 + 0.6 = 1.6。
const AQUA_REACH_BLUR_FACTOR: f32 = 1.6;
// カル境界に足す安全マージン（px）。丸め・補間の縁を確実に内側へ寄せ、寄与しうる画素を
// 絶対にカルしないための保険。出力は不変なのでいくら大きくても正しさは保たれる。
const AQUA_CULL_SAFETY_PX: f32 = 4.0;

// per-orb seed（phase 由来）から決定論的な単位方向ベクトルを作る。offset 軸が
// ブラーの disk 原点をこの向きへずらして滲みを左右非対称・有機的にするのに使う。
// hash で角度を散らすだけなので「形」は一切作らない（円へモーフしない）。
fn aqua_seed_dir(seed: f32) -> vec2<f32> {
    let a = hash21(vec2<f32>(seed * 12.9898, seed * 78.233 + 4.1)) * TAU;
    return vec2<f32>(cos(a), sin(a));
}

// 被覆 alpha を blur 半径 `blur_px` の disk 内で multi-tap 空間平均する（= ガウス近似ブラー）。
// `coverage_at` は variant ごとに差し替わる（orb=円距離 / SDF=サンプル距離）。返り値は
// plain と同じ (straight alpha, rgb_scale) の vec2。形は変えず被覆を空間平均するだけなので
// 星は星のままぼけ、強ブラーで自然に formless 化する（丸へモーフしない）。
// `seed` は per-orb（phase 由来）。スパイラルの初期角をずらして規則パターンを避ける。
//
// #239 offset 軸: `bias_px` は disk 原点（タップを撒く中心）に加える per-orb seed 方向の
// ずれ。**サンプル位置（cx,cy への距離評価）はそのまま**で、ブラーの“筆の置きどころ”だけ
// を seed 方向へ寄せるので、滲みが左右非対称になる。形（coverage_at の距離場）は不変＝
// 星は星のまま・円へモーフしない。bias_px=0（offset=0）で従来の対称ブラーと厳密に一致。
fn blurred_coverage(
    style_bit: f32,
    sample_px: vec2<f32>,
    cx: f32,
    cy: f32,
    radius: f32,
    blur: f32,
    opacity: f32,
    angle: f32,
    blur_px: f32,
    seed: f32,
    bias_px: vec2<f32>,
) -> vec2<f32> {
    // タップ 0 は中心。残りは黄金角スパイラルで disk(半径 blur_px) に等密度散布。
    var sum_a = 0.0;
    var sum_scaled = 0.0; // alpha * rgb_scale の総和（重み付き平均の分子）
    let n = AQUA_BLUR_TAPS;
    let nf = f32(n);
    // 初期角は per-orb seed に加え **per-pixel ハッシュ**でずらす。これで隣接画素が
    // 別のタップ位置を踏み、コヒーレントなスパイラルのトゲがディザされて滑らかになる。
    let ang0 = seed * AQUA_GOLDEN_ANGLE * 7.0 + hash21(sample_px) * TAU;
    // disk の中心を offset 軸の bias 分だけ seed 方向へずらす（offset=0 で bias_px=0）。
    let center = sample_px + bias_px;
    for (var k: u32 = 0u; k < n; k = k + 1u) {
        // r ∝ sqrt(k/n) で disk 内一様面積分布、角は黄金角で回す。
        let kf = f32(k);
        let rr = blur_px * sqrt((kf + 0.5) / nf);
        let th = ang0 + kf * AQUA_GOLDEN_ANGLE;
        let off = vec2<f32>(cos(th) * rr, sin(th) * rr);
        let sp = center + off;
        let cov = coverage_at(style_bit, sp, cx, cy, radius, blur, opacity, angle);
        sum_a = sum_a + cov.x;
        sum_scaled = sum_scaled + cov.x * cov.y;
    }
    let avg_a = sum_a / nf;
    var avg_scale = 1.0;
    if (sum_a > 0.0) {
        avg_scale = sum_scaled / sum_a; // alpha 重み付き平均の rgb_scale
    }
    return vec2<f32>(avg_a, avg_scale);
}

// #239 bloom/halo 軸（ブラー後の色味補正。各 coef=0 で恒等）。被覆 alpha `cov_a`
// （ブラー後）を中心度の代理にして、`color` を控えめに加工する:
//   - bloom: 内部（cov_a 高）で色を白へ寄せて柔らかい明るいコアにする。
//            t = bloom * smoothstep(0.18, 0.5, cov_a) を白との mix 比に使う（閾値は
//            ブラー後 alpha の実効レンジ基準。最大でも BLOOM_MAX=0.45 までしか白へ
//            寄せない＝強い白飛びを禁止）。
//   - halo : 外周の柔らかい縁（cov_a 低～中）で**彩度だけ**を上げる。枠（alpha）は作らない。
//            彩度ブースト量 = halo * edgeness。edgeness = smoothstep(0.45,0.05,cov_a) で
//            内部ほど 0、縁ほど 1。彩度は luma 軸からの距離を係数 (1+halo*k) 倍にする。
// 返り値は加工後の straight rgb。cov_a=0 の画素はそもそも合成側 `alpha>0` で弾かれる。
fn aqua_character(color: vec3<f32>, cov_a: f32, bloom: f32, halo: f32) -> vec3<f32> {
    var rgb = color;
    // ブラー後の被覆 alpha は orb の opacity（≈0.5 程度）で頭打ちになるため、中心度/縁度は
    // その実効レンジに合わせて閾値を取る（絶対 1.0 基準だと内部でもほぼ発火しない）。
    // --- halo: 外周の彩度ブースト（色味だけ。alpha リングは作らない）---
    if (halo > 0.0) {
        let edgeness = smoothstep(0.45, 0.05, cov_a); // 内部=0 → 縁=1
        let luma = dot(rgb, vec3<f32>(0.299, 0.587, 0.114));
        let sat_gain = 1.0 + halo * HALO_SAT_GAIN * edgeness; // 1.0 で恒等
        rgb = clamp(vec3<f32>(luma) + (rgb - vec3<f32>(luma)) * sat_gain, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    // --- bloom: 中心の柔らかい明るいコア（控えめに白へ）---
    if (bloom > 0.0) {
        let centerness = smoothstep(0.18, 0.5, cov_a); // 縁=0 → 中心=1（実効レンジ基準）
        let t = bloom * BLOOM_MAX * centerness; // 最大 BLOOM_MAX までしか白へ寄せない
        rgb = mix(rgb, vec3<f32>(1.0), t);
    }
    return rgb;
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

        // === ANGLE ブロック（variant 固有、loop 本体にインライン展開）===
        // SDF variant は per-orb 回転角を coverage_at へ渡すためここで計算する。
        // orb variant は回転を持たないので `let angle = 0.0;`（coverage_at で未使用）。
        // ここでだけ回転テクセル（rot）を読む（orb variant は読まない＝構造ピンの担保）。
        //!ORB_ANGLE
        // === /ANGLE ブロック ===

        // 被覆評価は variant 固有の `coverage_at`（上のマーカー行で展開）に一本化した。
        // orb = 円距離 / SDF = サンプル距離 → 同じ falloff_curve → (straight alpha, rgb_scale)。
        // plain（bleed=0）は sample_px の 1 タップ＝従来のインライン DISTANCE SOURCE と
        // **bit 一致**（SDF の箱外 `continue` は coverage_at が alpha=0 を返すのと同義：
        // 合成側 `if (alpha > 0.0)` が同様に寄与をスキップする）。
        var cov: vec2<f32>;
        if (params.aqua_bleed > 0.0) {
            // #239: ふつうのガウスブラー。被覆 alpha を blur 半径 blur_px∝bleed の disk 内で
            // multi-tap 空間平均する（= 画像編集のブラー）。星はブラーで星のままぼけ、強ブラーで
            // 自然に formless 化する。距離場を円へモーフしない（前回 NG を撤去済み）。
            //!ORB_AQUA_BLEED_GEOM
            // blur_px は radius にスケール。bleed>0 で必ず下限（min_px）を持たせ「常にぼやけ」を担保
            // （crisp 寄りの中間を作らない）。bleed=1 でフルブラー（radius スケール）まで広げる。
            // kako-jun「0.5まででいいくらい」: 上限は formless な雲ではなく「ほどよくぼけた
            // 星」に収める。旧 max(radius*1.15)=bleed1.0 で完全溶解だったが、旧 bleed=0.5 相当
            // （mix(0.18,1.15,0.5)=0.66*radius）を新しい上限にし、スライダー全域を使える良域に。
            let min_px = radius * 0.18;
            let max_px = radius * 0.66;
            let blur_px = mix(min_px, max_px, params.aqua_bleed) * aqua_blur_scale;
            // offset 軸: disk 原点を per-orb seed 方向へ blur_px 比でずらす（offset=0 で bias=0）。
            let bias_px = aqua_seed_dir(phase) * (params.aqua_offset * AQUA_OFFSET_BIAS * blur_px);
            // 空間早期カル（出力不変・48 タップ節約）: このブラー画素が踏みうる全タップは
            // sample_px から高々 AQUA_REACH_BLUR_FACTOR*blur_px（disk 半径 + offset bias）の範囲。
            // そのどのタップも orb 中心 (cx,cy) のシルエット最大到達 AQUA_REACH_RADIUS_FACTOR*radius
            // より遠ければ coverage_at は全タップ 0 を返す → blurred_coverage は alpha=0 → 合成側
            // `if (alpha > 0.0)` が寄与をスキップ。よって sample_px が
            //   到達上限 = AQUA_REACH_RADIUS_FACTOR*radius + AQUA_REACH_BLUR_FACTOR*blur_px + 安全マージン
            // より遠い画素は multi-tap を回さず被覆 0 扱いにできる（= もともと alpha=0 の画素だけ
            // をスキップするので **byte 完全不変**）。境界は安全側に大きく取る。
            let reach = AQUA_REACH_RADIUS_FACTOR * radius
                + AQUA_REACH_BLUR_FACTOR * blur_px
                + AQUA_CULL_SAFETY_PX;
            let dx = sample_px.x - cx;
            let dy = sample_px.y - cy;
            if (dx * dx + dy * dy > reach * reach) {
                // 確実に寄与 0。48 タップを省いて被覆 0（合成スキップ）にする。
                cov = vec2<f32>(0.0, 0.0);
            } else {
                cov = blurred_coverage(
                    style_bit, sample_px, cx, cy, radius, blur, opacity, angle, blur_px, phase, bias_px
                );
            }
        } else {
            // plain 経路（byte 一致ゲート）: sample_px の単一タップ。
            cov = coverage_at(style_bit, sample_px, cx, cy, radius, blur, opacity, angle);
        }
        let alpha = cov.x;
        let rgb_scale = cov.y;

        if (alpha > 0.0) {
            // #239 bloom/halo 軸（ブラー後の色味補正。各 coef=0 で恒等＝plain と byte 一致）。
            // 被覆 alpha を中心度の代理に、色を控えめに加工（中心は白へ寄せ、縁は彩度ブースト）。
            // bloom=halo=0 で aqua_character は color をそのまま返すので非回帰ゲートを満たす。
            //
            // **にじみ(bleed)が前提**: character はにじみの上に乗る装飾なので、`aqua_bleed > 0`
            // でゲートする。bleed=0（水彩オフ）のときは bloom/halo が何であろうと素の色のまま＝
            // plain と crisp に byte 一致。これで内部上級者 flag が bleed=0 のまま bloom/halo を
            // 0.5 に流しても（製品 UI も「にじみなし」のとき 3 軸 disabled）crisp が保たれる。
            var src_rgb = o.color.rgb;
            if (params.aqua_bleed > 0.0) {
                src_rgb = aqua_character(o.color.rgb, alpha, params.aqua_bloom, params.aqua_halo);
            }
            // Source-Over（straight alpha）。GLSL と同式 + #241 影スケール:
            //   out.rgb = (src.rgb * shadow_scale) * src.a + out.rgb * (1 - src.a)
            //   out.a   = src.a + out.a * (1 - src.a)
            // ブラー経路では rgb_scale は disk 内の alpha 重み付き平均（影は被覆と一緒に平均される）。
            let one_minus_a = 1.0 - alpha;
            acc_rgb = src_rgb * rgb_scale * alpha + acc_rgb * one_minus_a;
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
