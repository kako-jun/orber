//! #242: A/B 三者画素比較（CLI / ブラウザWGSL / ブラウザWebGL）の dev ハーネス。
//!
//! ブラウザの A/B パネル（`web/src/components/AbPanel.tsx`、`?ab=1&abcap=1`）が
//! 落とす `ab-params.json`（source_rgb 以外の全 params + n + spec_idx + t）と
//! `ab-source.bin`（source_rgb の生バイト）から、**wasm の `gpu_set_render_data`
//! が通るのと同じ core 入口**（[`crate::build_gpu_render_inputs`] → 共有の
//! `resolve_frame`）で render data を組み、orber-core の **readback 経路**
//! （[`orber_core::gpu::GpuRenderer::render_packed`] = browser present 経路
//! `render_packed_to_view` の readback 版）で同条件の PNG を出力する。
//!
//! これで CLI / WGSL / WebGL の 3 枚が「同一 params・同一ソース・同一 t」で
//! 揃い、present 経路（surface format / alphaMode / サイズ / 量子化+合成順）の
//! 容疑者を画素差分で切り分けられる。**調査用の足場であり、シェーダや present
//! 経路の挙動は一切変えない**（読むだけ・描くだけ）。
//!
//! 既知の限界（A チャネル）: WGSL 側キャプチャ（WebGPU surface は
//! alphaMode=Opaque、canvas の drawImage snapshot）は A が常に 255 になる一方、
//! WebGL 側の readPixels は生の straight alpha を返す。bg 不透明の orb ゲートでは
//! 実害なしだが、translucent な bg を比較すると A に偽差分が出る（dev 足場の限界）。
//!
//! ## 使い方（公開 CLI のサブコマンド / フラグは増やさない: `#[ignore]` テスト）
//!
//! ブラウザキャプチャの再現 PNG を出力:
//!
//! ```text
//! ORBER_AB_PARAMS=ab-params.json ORBER_AB_SOURCE=ab-source.bin ORBER_AB_OUT=ab-cli.png \
//!   cargo test --release -p orber-wasm ab_dump -- --ignored --nocapture
//! ```
//!
//! `ORBER_AB_SOURCE` を省略すると、ブラウザのキャプチャモードと同一式の合成
//! ソース（[`synthetic_source_rgb`]）を params の source_width/height から生成
//! する（= abcap の決定的ソースを CLI 単独で再現できる）。
//!
//! 2 枚の PNG を比較（完全一致 / per-channel 差分 / 空間分布 / 分類ヒント）:
//!
//! ```text
//! ORBER_AB_A=ab-wgsl.png ORBER_AB_B=ab-cli.png \
//!   cargo test --release -p orber-wasm ab_diff -- --ignored --nocapture
//! ```
//!
//! native test 専用。GPU readback（blocking poll）と std::fs を使うため
//! wasm32 には存在しない（lib.rs の `#[cfg(all(test, not(wasm32)))]` 宣言）。
//! ワークスペースの「std::fs は crates/cli だけ」ルールは production コードの
//! wasm ビルド可能性を守るためのもので、この native test 専用モジュールは
//! 対象外（wasm32 ではコンパイルされない）。

use std::env;
use std::fs;

use image::RgbaImage;
use orber_core::gpu::GpuRenderer;
use orber_core::orb::OrbShape;

use crate::{build_gpu_render_inputs, validate_params, WasmParams};

/// ブラウザのキャプチャモードと**完全に同一の式**で合成 RGB ソースを生成する
/// （行優先 R,G,B,...）。`ab-source.bin` が無くても CLI 単独で同じソースを再現
/// できるようにするための実装。
///
/// 合成式（x, y は 0 始まりの画素座標、% は非負剰余）:
///   r = (x * 7  + y * 13) % 256
///   g = (x * 11 + y * 5)  % 256
///   b = (x * 13 + y * 7)  % 256
// SYNC WITH web/src/lib/abLogic.ts::buildSyntheticSourceRgb
fn synthetic_source_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((width as usize) * (height as usize) * 3);
    for y in 0..height {
        for x in 0..width {
            rgb.push(((x * 7 + y * 13) % 256) as u8);
            rgb.push(((x * 11 + y * 5) % 256) as u8);
            rgb.push(((x * 13 + y * 7) % 256) as u8);
        }
    }
    rgb
}

/// `ab-params.json` のスキーマ（`web/src/lib/abLogic.ts::buildAbCaptureMeta` の
/// 出力）。バイナリ列（source_rgb / image_mask_rgba / glyph_sdf）は JSON に
/// 含まれない（source_rgb は ab-source.bin、他はハーネス対象外 = orb 専用）。
/// 未知フィールドは serde_json の既定で無視する。
#[derive(serde::Deserialize)]
struct AbParams {
    source_width: u32,
    source_height: u32,
    k: usize,
    width: u32,
    height: u32,
    seed: f64,
    direction: String,
    speed: String,
    count: usize,
    orb_size: f32,
    blur: f32,
    shape: String,
    #[serde(default)]
    glyph_char: String,
    #[serde(default = "default_true")]
    glyph_rotate: bool,
    #[serde(default)]
    count_preset: String,
    #[serde(default)]
    speed_preset: String,
    #[serde(default)]
    softness_preset: String,
    n: u32,
    spec_idx: u32,
    t: f32,
}

fn default_true() -> bool {
    true
}

/// `AbParams` + source_rgb から `WasmParams` を組む。バイナリ系フィールドは
/// 空（orb ゲート専用ハーネスなので image_mask / glyph_sdf は使わない）。
fn to_wasm_params(ab: &AbParams, source_rgb: Vec<u8>) -> WasmParams {
    WasmParams {
        source_rgb,
        source_width: ab.source_width,
        source_height: ab.source_height,
        k: ab.k,
        width: ab.width,
        height: ab.height,
        seed: ab.seed,
        direction: ab.direction.clone(),
        speed: ab.speed.clone(),
        count: ab.count,
        orb_size: ab.orb_size,
        blur: ab.blur,
        shape: ab.shape.clone(),
        glyph_char: ab.glyph_char.clone(),
        count_preset: ab.count_preset.clone(),
        speed_preset: ab.speed_preset.clone(),
        softness_preset: ab.softness_preset.clone(),
        glyph_rotate: ab.glyph_rotate,
        image_mask_rgba: Vec::new(),
        image_mask_width: 0,
        image_mask_height: 0,
        aquarelle_bleed: 0.5,
        aquarelle_bloom: 0.5,
        aquarelle_offset: 0.5,
        aquarelle_halo: 0.5,
        glyph_sdf: Vec::new(),
        glyph_sdf_size: 0,
    }
}

/// ブラウザキャプチャ（ab-params.json + ab-source.bin）を CLI 側（readback
/// 経路）で再現して PNG を書く。dev 専用 `#[ignore]` テスト（モジュール doc の
/// コマンド参照）。shape は orb のみ（#232 ゲート対象。glyph / image /
/// aquarelle は対象外なので明確に落とす）。
#[test]
#[ignore = "dev harness (#242): ORBER_AB_PARAMS=ab-params.json [ORBER_AB_SOURCE=ab-source.bin] [ORBER_AB_OUT=ab-cli.png] cargo test --release -p orber-wasm ab_dump -- --ignored --nocapture"]
fn ab_dump() {
    let params_path =
        env::var("ORBER_AB_PARAMS").expect("set ORBER_AB_PARAMS=path/to/ab-params.json");
    let out_path = env::var("ORBER_AB_OUT").unwrap_or_else(|_| "ab-cli.png".to_string());

    let json = fs::read_to_string(&params_path)
        .unwrap_or_else(|e| panic!("failed to read {params_path}: {e}"));
    let ab: AbParams = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("failed to parse {params_path}: {e}"));
    assert_eq!(
        ab.shape, "orb",
        "ab_dump supports shape=orb only (#232 gate target), got {}",
        ab.shape
    );

    // ソース: ORBER_AB_SOURCE があれば生バイト、無ければ合成式（ブラウザの
    // abcap と同一式）で params の source_width/height から再現する。
    let source_rgb = match env::var("ORBER_AB_SOURCE") {
        Ok(p) => fs::read(&p).unwrap_or_else(|e| panic!("failed to read {p}: {e}")),
        Err(_) => synthetic_source_rgb(ab.source_width, ab.source_height),
    };
    let expected_len = (ab.source_width as usize) * (ab.source_height as usize) * 3;
    assert_eq!(
        source_rgb.len(),
        expected_len,
        "source bytes {} != source_width * source_height * 3 ({expected_len})",
        source_rgb.len()
    );

    let p = to_wasm_params(&ab, source_rgb);
    validate_params(&p).expect("params failed validate_params");

    // get_or_build_clusters のグローバル cache を触るので、lib.rs の単体テスト群と
    // 同じガードで直列化する（`--include-ignored` 並走時の BorrowMutError 防止）。
    let inputs = {
        let _guard = crate::tests::CACHE_TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        build_gpu_render_inputs(p, ab.n, ab.spec_idx).expect("build_gpu_render_inputs failed")
    };
    assert!(
        matches!(inputs.opts.shape, OrbShape::Orb),
        "resolved shape is not Orb"
    );

    // ブラウザの gpu_render(Orb) は render_packed_to_view(pack, w, h, t)。その
    // readback 版 render_packed で同じ pack / 同じ寸法 / 同じ t を描く。
    let renderer = GpuRenderer::new()
        .expect("no GPU adapter available (ab_dump needs a native GPU, e.g. Metal)");
    let img = renderer.render_packed(&inputs.pack, inputs.opts.width, inputs.opts.height, ab.t);
    img.save(&out_path)
        .unwrap_or_else(|e| panic!("failed to write {out_path}: {e}"));
    eprintln!(
        "ab_dump: wrote {out_path} ({}x{}, t={}, n={}, spec_idx={}, adapter={})",
        inputs.opts.width,
        inputs.opts.height,
        ab.t,
        ab.n,
        ab.spec_idx,
        renderer.adapter_name()
    );
}

/// 2 枚の PNG の差分レポートをテキストで出す。dev 専用 `#[ignore]` テスト
/// （モジュール doc のコマンド参照）。分析本体は純粋関数 [`diff_report`]。
#[test]
#[ignore = "dev harness (#242): ORBER_AB_A=a.png ORBER_AB_B=b.png cargo test --release -p orber-wasm ab_diff -- --ignored --nocapture"]
fn ab_diff() {
    let path_a = env::var("ORBER_AB_A").expect("set ORBER_AB_A=path/to/first.png");
    let path_b = env::var("ORBER_AB_B").expect("set ORBER_AB_B=path/to/second.png");
    let a = image::open(&path_a)
        .unwrap_or_else(|e| panic!("failed to open {path_a}: {e}"))
        .to_rgba8();
    let b = image::open(&path_b)
        .unwrap_or_else(|e| panic!("failed to open {path_b}: {e}"))
        .to_rgba8();
    eprintln!("ab_diff: A={path_a} B={path_b}");
    eprintln!("{}", diff_report(&a, &b));
}

/// per-channel 絶対差のヒストグラムバケット境界（`0 / 1 / 2-3 / 4-7 / 8-15 /
/// 16-31 / 32-63 / 64+`）。ガンマ系（中間調で数〜十数）と量子化系（±1）と
/// 破綻系（64+）をひと目で区別するための対数様スケール。
const HIST_LABELS: [&str; 8] = ["0", "1", "2-3", "4-7", "8-15", "16-31", "32-63", "64+"];

fn hist_bucket(d: u8) -> usize {
    match d {
        0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=7 => 3,
        8..=15 => 4,
        16..=31 => 5,
        32..=63 => 6,
        _ => 7,
    }
}

/// 2 枚の RGBA 画像の差分レポート（純粋関数）。
///
/// - 完全一致判定（`IDENTICAL`）
/// - per-channel（R/G/B/A）の最大差・平均絶対差・符号付き平均（A−B）・相違画素数
/// - 絶対差ヒストグラム（全チャネル合算）
/// - **空間分布**: 縁からの距離バンド（0-2px / 3-7px / 8px+）と 3×3 グリッドの
///   平均絶対差。縁に集中＝smear / サイズ不一致系、全面一様＝ガンマ / encode 系
///   の切り分けに使う
/// - 分類ヒント（`IDENTICAL` / `EDGE-CONCENTRATED` / `UNIFORM-SHIFT` / `MIXED`）
///
/// 寸法不一致はそれ自体が所見（サイズ不一致の容疑そのもの）なので、画素比較は
/// せずその旨だけ返す。
fn diff_report(a: &RgbaImage, b: &RgbaImage) -> String {
    if a.dimensions() != b.dimensions() {
        return format!(
            "DIMENSION MISMATCH: A={}x{} B={}x{}\n\
             hint: サイズ不一致はそれ自体が容疑（core to_view 契約: view は width×height に正確に一致が必要）",
            a.width(),
            a.height(),
            b.width(),
            b.height()
        );
    }
    let (w, h) = a.dimensions();

    // per-channel 統計。
    let mut max_d = [0u8; 4];
    let mut abs_sum = [0f64; 4];
    let mut signed_sum = [0f64; 4]; // A - B
    let mut diff_px_count = [0u64; 4];
    let mut hist = [0u64; 8];

    // 空間分布: 縁からの距離バンド（0-2 / 3-7 / 8+ px）と 3×3 グリッド。
    // 画素単位の差は「4 チャネル中の最大絶対差」で代表させる（どのチャネルの
    // 異常も拾う）。
    let mut band_sum = [0f64; 3];
    let mut band_count = [0u64; 3];
    let mut grid_sum = [[0f64; 3]; 3];
    let mut grid_count = [[0u64; 3]; 3];

    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y).0;
            let pb = b.get_pixel(x, y).0;
            let mut px_max = 0u8;
            for c in 0..4 {
                let d = pa[c].abs_diff(pb[c]);
                if d > 0 {
                    diff_px_count[c] += 1;
                }
                max_d[c] = max_d[c].max(d);
                abs_sum[c] += f64::from(d);
                signed_sum[c] += f64::from(pa[c]) - f64::from(pb[c]);
                hist[hist_bucket(d)] += 1;
                px_max = px_max.max(d);
            }
            let edge_dist = x.min(y).min(w - 1 - x).min(h - 1 - y);
            let band = match edge_dist {
                0..=2 => 0,
                3..=7 => 1,
                _ => 2,
            };
            band_sum[band] += f64::from(px_max);
            band_count[band] += 1;
            let gx = ((x as usize) * 3 / (w as usize)).min(2);
            let gy = ((y as usize) * 3 / (h as usize)).min(2);
            grid_sum[gy][gx] += f64::from(px_max);
            grid_count[gy][gx] += 1;
        }
    }

    let total_px = f64::from(w) * f64::from(h);
    let total_abs: f64 = abs_sum.iter().sum();
    if total_abs == 0.0 {
        return format!("IDENTICAL: {w}x{h} byte-exact match");
    }

    let mut out = String::new();
    out.push_str(&format!("NOT IDENTICAL: {w}x{h}\n"));
    out.push_str("per-channel (A-B):\n");
    for (c, name) in ["R", "G", "B", "A"].iter().enumerate() {
        out.push_str(&format!(
            "  {name}: max={} mean_abs={:.4} mean_signed={:+.4} diff_px={} ({:.2}%)\n",
            max_d[c],
            abs_sum[c] / total_px,
            signed_sum[c] / total_px,
            diff_px_count[c],
            100.0 * (diff_px_count[c] as f64) / total_px
        ));
    }
    out.push_str("abs-diff histogram (all channels):\n");
    for (i, label) in HIST_LABELS.iter().enumerate() {
        out.push_str(&format!("  {label:>5}: {}\n", hist[i]));
    }

    let band_mean: Vec<f64> = (0..3)
        .map(|i| {
            if band_count[i] == 0 {
                0.0
            } else {
                band_sum[i] / (band_count[i] as f64)
            }
        })
        .collect();
    out.push_str(&format!(
        "edge bands (mean of per-pixel max diff): 0-2px={:.4} 3-7px={:.4} 8px+={:.4}\n",
        band_mean[0], band_mean[1], band_mean[2]
    ));
    out.push_str("3x3 grid (mean of per-pixel max diff):\n");
    let mut grid_means = Vec::with_capacity(9);
    for gy in 0..3 {
        out.push_str("  ");
        for gx in 0..3 {
            let m = if grid_count[gy][gx] == 0 {
                0.0
            } else {
                grid_sum[gy][gx] / (grid_count[gy][gx] as f64)
            };
            grid_means.push(m);
            out.push_str(&format!("{m:>9.4} "));
        }
        out.push('\n');
    }

    // 分類ヒント。閾値は dev 足場の経験則（決定打ではなく一次切り分け用）:
    //   - EDGE-CONCENTRATED: 縁 0-2px の差が内部（8px+）の 3 倍超 + 0.5 で
    //     優越 → smear / サイズ不一致系（core to_view 契約）を疑う
    //   - UNIFORM-SHIFT: 3×3 全セルが全体平均の 0.5〜2 倍に収まり、かつ符号付き
    //     平均が絶対平均の 8 割超（ほぼ一方向のズレ）→ ガンマ / sRGB encode 系
    //     （二重 / 欠落 encode）を疑う
    let edge = band_mean[0];
    let interior = band_mean[2];
    let overall_grid_mean: f64 = grid_means.iter().sum::<f64>() / 9.0;
    let grid_uniform = overall_grid_mean > 0.0
        && grid_means
            .iter()
            .all(|&m| m >= overall_grid_mean * 0.5 && m <= overall_grid_mean * 2.0);
    let rgb_abs: f64 = abs_sum[..3].iter().sum();
    let rgb_signed: f64 = signed_sum[..3].iter().sum();
    let one_sided = rgb_abs > 0.0 && rgb_signed.abs() / rgb_abs > 0.8;

    let hint = if edge > interior * 3.0 + 0.5 {
        "EDGE-CONCENTRATED → 縁に差が集中: smear / サイズ不一致系（core to_view 契約・surface 構成）を疑う"
    } else if grid_uniform && one_sided {
        "UNIFORM-SHIFT → 全面一様な一方向のズレ: ガンマ / sRGB encode 系（二重 / 欠落 encode）を疑う"
    } else {
        "MIXED → 単純な縁集中でも全面一様でもない: 量子化 + 合成順 / 複合要因を個別に追う"
    };
    out.push_str(&format!("hint: {hint}\n"));
    out
}

// ---- 純粋部分のユニットテスト（GPU 不要・通常の cargo test で走る） --------

/// JS（web/src/lib/abLogic.ts buildSyntheticSourceRgb）側のピンと同値であること。
/// SYNC WITH web/src/lib/abLogic.test.ts の S1 テスト。式を変えるときは両側の
/// ピンを同時に更新する。
#[test]
fn synthetic_source_pins_known_bytes() {
    let rgb = synthetic_source_rgb(96, 96);
    assert_eq!(rgb.len(), 96 * 96 * 3);
    // (x=0, y=0)
    assert_eq!(&rgb[0..3], &[0, 0, 0]);
    // (x=1, y=0): r=7, g=11, b=13
    assert_eq!(&rgb[3..6], &[7, 11, 13]);
    // (x=0, y=1): r=13, g=5, b=7
    let row1 = 96 * 3;
    assert_eq!(&rgb[row1..row1 + 3], &[13, 5, 7]);
    // (x=95, y=95): r=(95*7+95*13)%256=108, g=(95*11+95*5)%256=240, b=108
    let last = (95 * 96 + 95) * 3;
    assert_eq!(&rgb[last..last + 3], &[108, 240, 108]);
}

/// 同一画像は IDENTICAL と報告される。
#[test]
fn diff_report_identical_images() {
    let img = RgbaImage::from_fn(16, 16, |x, y| image::Rgba([x as u8, y as u8, 100, 255]));
    let report = diff_report(&img, &img.clone());
    assert!(report.starts_with("IDENTICAL"), "got: {report}");
}

/// 縁 2px だけ明るさが違う画像は EDGE-CONCENTRATED に分類される
/// （smear / サイズ不一致系の切り分けヒント）。
#[test]
fn diff_report_flags_edge_concentration() {
    let a = RgbaImage::from_pixel(32, 32, image::Rgba([100, 100, 100, 255]));
    let b = RgbaImage::from_fn(32, 32, |x, y| {
        let edge_dist = x.min(y).min(31 - x).min(31 - y);
        if edge_dist <= 2 {
            image::Rgba([140, 140, 140, 255])
        } else {
            image::Rgba([100, 100, 100, 255])
        }
    });
    let report = diff_report(&a, &b);
    assert!(report.contains("EDGE-CONCENTRATED"), "got: {report}");
}

/// 全面一様な一方向のズレ（+10）は UNIFORM-SHIFT に分類される
/// （ガンマ / sRGB encode 系の切り分けヒント）。
#[test]
fn diff_report_flags_uniform_shift() {
    let a = RgbaImage::from_pixel(32, 32, image::Rgba([110, 110, 110, 255]));
    let b = RgbaImage::from_pixel(32, 32, image::Rgba([100, 100, 100, 255]));
    let report = diff_report(&a, &b);
    assert!(report.contains("UNIFORM-SHIFT"), "got: {report}");
    // 符号付き平均は A-B = +10。
    assert!(report.contains("mean_signed=+10.0000"), "got: {report}");
}

/// 寸法不一致は画素比較せず、それ自体を所見として返す。
#[test]
fn diff_report_dimension_mismatch() {
    let a = RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255]));
    let b = RgbaImage::from_pixel(9, 8, image::Rgba([0, 0, 0, 255]));
    let report = diff_report(&a, &b);
    assert!(report.starts_with("DIMENSION MISMATCH"), "got: {report}");
}

// ---- #242 観点テスト（serde / validate / diff_report 境界） -----------------

/// `AbParams` の全必須フィールドを持つ基準 JSON。各テストはこれを改変して使う。
fn base_ab_params_json() -> serde_json::Value {
    serde_json::json!({
        "source_width": 8,
        "source_height": 8,
        "k": 5,
        "width": 32,
        "height": 48,
        "seed": 42.0,
        "direction": "lr",
        "speed": "slow",
        "count": 12,
        "orb_size": 1.0,
        "blur": 0.5,
        "shape": "orb",
        "glyph_char": "",
        "glyph_rotate": true,
        "count_preset": "",
        "speed_preset": "",
        "softness_preset": "",
        "n": 12,
        "spec_idx": 8,
        "t": 0.0
    })
}

/// ブラウザ側 `buildAbCaptureMeta` は params の未知フィールド（aquarelle_* 等）を
/// そのまま透過する。Rust 側は serde_json 既定の「未知フィールド無視」で受け流し、
/// パースは成功すること（将来 web 側に params が増えてもハーネスが壊れない）。
#[test]
fn ab_params_ignores_unknown_fields() {
    let mut v = base_ab_params_json();
    v["aquarelle_bleed"] = serde_json::json!(0.7);
    v["image_mask_width"] = serde_json::json!(4);
    v["some_future_field"] = serde_json::json!("x");
    let ab: AbParams = serde_json::from_value(v).expect("unknown fields must be ignored");
    assert_eq!(ab.width, 32);
    assert_eq!(ab.shape, "orb");
    assert_eq!(ab.n, 12);
}

/// 省略可能フィールドの serde default をピンする: `glyph_rotate` は `true`
/// （`WasmParams::glyph_rotate` の default と同じ向き）、`glyph_char` / 各 preset
/// は空文字。default の向きが変わると CLI 再現がブラウザと食い違う。
#[test]
fn ab_params_optional_fields_default() {
    let mut v = base_ab_params_json();
    let obj = v.as_object_mut().unwrap();
    for k in [
        "glyph_char",
        "glyph_rotate",
        "count_preset",
        "speed_preset",
        "softness_preset",
    ] {
        obj.remove(k);
    }
    let ab: AbParams = serde_json::from_value(v).expect("optional fields must default");
    assert!(
        ab.glyph_rotate,
        "glyph_rotate must default to true (same as WasmParams)"
    );
    assert_eq!(ab.glyph_char, "");
    assert_eq!(ab.count_preset, "");
    assert_eq!(ab.speed_preset, "");
    assert_eq!(ab.softness_preset, "");
}

/// 必須フィールド（t / n / spec_idx / seed）はどれが欠けてもパース失敗すること。
/// 黙って 0 既定で通ると「別の t / spec の再現 PNG」が無言で出てしまう。
#[test]
fn ab_params_missing_required_field_errors() {
    for key in ["t", "n", "spec_idx", "seed"] {
        let mut v = base_ab_params_json();
        v.as_object_mut().unwrap().remove(key);
        let r: Result<AbParams, _> = serde_json::from_value(v);
        assert!(r.is_err(), "missing required `{key}` must fail to parse");
    }
}

/// seed の f64 round-trip と validate_params の実挙動のピン: 2^48+1（< 2^53 なので
/// f64 で正確に表現できる）は値を変えずに通り、負値は `validate_params` が reject
/// する（serde 自体は通る = reject は validate の担務）。
#[test]
fn ab_params_seed_f64_round_trip_and_negative_reject() {
    let big = 281_474_976_710_657.0_f64; // 2^48 + 1
    let mut v = base_ab_params_json();
    v["seed"] = serde_json::json!(big);
    let ab: AbParams = serde_json::from_value(v).expect("2^48+1 seed must parse");
    assert_eq!(
        ab.seed, big,
        "seed must round-trip losslessly through JSON f64"
    );
    let p = to_wasm_params(&ab, synthetic_source_rgb(ab.source_width, ab.source_height));
    validate_params(&p).expect("non-negative finite seed must validate");

    let mut v = base_ab_params_json();
    v["seed"] = serde_json::json!(-1.0);
    let ab: AbParams =
        serde_json::from_value(v).expect("negative seed parses (reject is validate's job)");
    let p = to_wasm_params(&ab, synthetic_source_rgb(ab.source_width, ab.source_height));
    assert!(
        validate_params(&p).is_err(),
        "negative seed must be rejected by validate_params"
    );
}

/// `hist_bucket` の全 6 境界（1/2, 3/4, 7/8, 15/16, 31/32, 63/64）と両端のピン。
/// 境界が 1 ずれると「量子化系（±1）」と「ガンマ系（数〜十数）」の切り分け表示が
/// 嘘をつく。
#[test]
fn hist_bucket_pins_all_boundaries() {
    assert_eq!(hist_bucket(0), 0);
    assert_eq!(hist_bucket(1), 1);
    assert_eq!(hist_bucket(2), 2);
    assert_eq!(hist_bucket(3), 2);
    assert_eq!(hist_bucket(4), 3);
    assert_eq!(hist_bucket(7), 3);
    assert_eq!(hist_bucket(8), 4);
    assert_eq!(hist_bucket(15), 4);
    assert_eq!(hist_bucket(16), 5);
    assert_eq!(hist_bucket(31), 5);
    assert_eq!(hist_bucket(32), 6);
    assert_eq!(hist_bucket(63), 6);
    assert_eq!(hist_bucket(64), 7);
    assert_eq!(hist_bucket(255), 7);
}

/// 32×32 で「edge_dist がちょうど `ring` の画素だけ R+10」のペアを作る
/// （edge band 境界テスト用）。
fn ring_diff_pair(ring: u32) -> (RgbaImage, RgbaImage) {
    let a = RgbaImage::from_pixel(32, 32, image::Rgba([100, 100, 100, 255]));
    let b = RgbaImage::from_fn(32, 32, |x, y| {
        let edge_dist = x.min(y).min(31 - x).min(31 - y);
        if edge_dist == ring {
            image::Rgba([110, 100, 100, 255])
        } else {
            image::Rgba([100, 100, 100, 255])
        }
    });
    (a, b)
}

/// 縁バンドの境界画素が正しいバンドに落ちること: edge_dist 2 → 0-2px / 3 → 3-7px
/// （2/3 境界）、7 → 3-7px / 8 → 8px+（7/8 境界）。リング 1 本だけ差を付けた画像で
/// 「そのバンドだけ非ゼロ」をレポート文字列で確認する。
#[test]
fn diff_report_edge_band_boundaries() {
    // (ring, 0-2px がゼロか, 3-7px がゼロか, 8px+ がゼロか)
    let cases = [
        (2u32, false, true, true),
        (3, true, false, true),
        (7, true, false, true),
        (8, true, true, false),
    ];
    for (ring, z0, z1, z2) in cases {
        let (a, b) = ring_diff_pair(ring);
        let report = diff_report(&a, &b);
        let zero = |label: &str| report.contains(&format!("{label}=0.0000"));
        assert_eq!(zero("0-2px"), z0, "ring {ring}: 0-2px band, got: {report}");
        assert_eq!(zero("3-7px"), z1, "ring {ring}: 3-7px band, got: {report}");
        assert_eq!(zero("8px+"), z2, "ring {ring}: 8px+ band, got: {report}");
    }
}

/// 20×20 で「edge_dist ≤ 2 の画素のうち走査順で先頭 `n` 個だけ R+1」のペアを作る。
/// 0-2px バンドは 20²−14² = 204 画素なので、n=102 で縁バンド平均がちょうど 0.5
/// （interior=0 のとき EDGE 閾値 `interior*3 + 0.5` の右辺そのもの）になる。
/// 20×20 を使うのは 8px+（interior）バンドが空にならない最小級サイズのため
/// （小画像だと interior=0 件で別の話になる）。
fn edge_count_pair(n: usize) -> (RgbaImage, RgbaImage) {
    let a = RgbaImage::from_pixel(20, 20, image::Rgba([100, 100, 100, 255]));
    let mut b = a.clone();
    let mut left = n;
    'outer: for y in 0..20u32 {
        for x in 0..20u32 {
            let edge_dist = x.min(y).min(19 - x).min(19 - y);
            if edge_dist <= 2 {
                if left == 0 {
                    break 'outer;
                }
                b.put_pixel(x, y, image::Rgba([101, 100, 100, 255]));
                left -= 1;
            }
        }
    }
    assert_eq!(left, 0, "test setup: not enough ring pixels");
    (a, b)
}

/// EDGE-CONCENTRATED の閾値は strict `>`: 縁バンド平均が**ちょうど** 0.5
/// （= interior*3 + 0.5、interior=0）では MIXED のまま、1 画素足して初めて
/// EDGE に倒れる。閾値が `>=` に変わると「ちょうど」で分類が変わってしまう。
#[test]
fn diff_report_edge_threshold_exactly_at_boundary_is_mixed() {
    let (a, b) = edge_count_pair(102); // 102 / 204 = 0.5 ちょうど
    let report = diff_report(&a, &b);
    assert!(
        report.contains("MIXED"),
        "edge mean exactly 0.5 must stay MIXED (strict >), got: {report}"
    );
    let (a, b) = edge_count_pair(103); // 1 画素ぶんだけ 0.5 を超える
    let report = diff_report(&a, &b);
    assert!(
        report.contains("EDGE-CONCENTRATED"),
        "one more edge pixel must tip to EDGE-CONCENTRATED, got: {report}"
    );
}

/// 18×18 の市松ペア: (x+y) 偶数画素は A−B=+9、奇数画素は A−B=−1（R チャネルのみ、
/// 162 個ずつ）。rgb_signed/rgb_abs = (162·9−162)/(162·9+162) = 0.8 ちょうど。
/// `flip_one` で奇数画素 (1,0) を +9 側に変えると比が 0.8 を超える。
/// 市松なので 3×3 グリッドは全セル平均 5 で一様（uniform 成立済み）、縁も一様で
/// EDGE には倒れない。18×18 を使うのは 8px+ バンドが空にならないため。
fn checkerboard_pair(flip_one: bool) -> (RgbaImage, RgbaImage) {
    let mk = move |a_side: bool| {
        RgbaImage::from_fn(18, 18, move |x, y| {
            let plus9 = (x + y) % 2 == 0 || (flip_one && x == 1 && y == 0);
            let v = match (plus9, a_side) {
                (true, true) => 109,
                (true, false) => 100,
                (false, true) => 100,
                (false, false) => 101,
            };
            image::Rgba([v, 100, 100, 255])
        })
    };
    (mk(true), mk(false))
}

/// UNIFORM-SHIFT の one_sided 閾値は strict `>`: 符号付き/絶対比が**ちょうど** 0.8
/// （+9 と −1 が同数）では MIXED のまま、1 画素 +9 側に倒すと初めて UNIFORM-SHIFT
/// になる（grid 一様・縁非優越は両ケースとも成立済み = 比だけが分かれ目）。
#[test]
fn diff_report_one_sided_threshold_exactly_at_boundary_is_mixed() {
    let (a, b) = checkerboard_pair(false);
    let report = diff_report(&a, &b);
    assert!(
        report.contains("MIXED"),
        "signed/abs ratio exactly 0.8 must stay MIXED (strict >), got: {report}"
    );
    let (a, b) = checkerboard_pair(true);
    let report = diff_report(&a, &b);
    assert!(
        report.contains("UNIFORM-SHIFT"),
        "one more one-sided pixel must tip to UNIFORM-SHIFT, got: {report}"
    );
}

/// grid_uniform の範囲判定は inclusive（`>=` / `<=`）: 18×18 を 3×3 セルに切り、
/// 6 セルを +2・3 セルを +8 にすると、セル平均 2 と 8 が全体平均 4 の**ちょうど**
/// 0.5× と 2×（両端同時等号）に乗る。inclusive なので uniform 成立 → 全面一方向
/// ズレとして UNIFORM-SHIFT になる（strict だったら MIXED に落ちて検知できる）。
#[test]
fn diff_report_grid_uniform_bounds_are_inclusive() {
    let a = RgbaImage::from_fn(18, 18, |_x, y| {
        let d = if y >= 12 { 8 } else { 2 };
        image::Rgba([100 + d, 100, 100, 255])
    });
    let b = RgbaImage::from_pixel(18, 18, image::Rgba([100, 100, 100, 255]));
    let report = diff_report(&a, &b);
    assert!(
        report.contains("UNIFORM-SHIFT"),
        "inclusive grid bounds must keep uniform at exactly 0.5x / 2x, got: {report}"
    );
}

/// 分類の優先順位: edge 優越・grid 一様・one_sided が**同時に**成立するケースでは
/// if-else 連鎖の先頭である EDGE-CONCENTRATED が勝つことをピンする。
/// 構成: 全画素 +6、ただし interior バンド（edge_dist ≥ 8 = 中心 2×2 の 4 画素）
/// だけ差 0 → edge 条件（6 > 0·3+0.5）、grid（中心セルも 32/36·6 ≈ 5.33 で
/// 0.5×〜2× 内）、one_sided（全部 + 方向）が全部 true。
#[test]
fn diff_report_priority_edge_wins_over_uniform_and_one_sided() {
    let a = RgbaImage::from_fn(18, 18, |x, y| {
        let edge_dist = x.min(y).min(17 - x).min(17 - y);
        let d = if edge_dist >= 8 { 0 } else { 6 };
        image::Rgba([100 + d, 100, 100, 255])
    });
    let b = RgbaImage::from_pixel(18, 18, image::Rgba([100, 100, 100, 255]));
    let report = diff_report(&a, &b);
    assert!(
        report.contains("EDGE-CONCENTRATED"),
        "edge check must win the classification chain, got: {report}"
    );
}

/// 1×1 画像でも 0 除算や panic をしない縮退ピン: 同一なら IDENTICAL、差があれば
/// 全画素が 0-2px バンドに落ちるので EDGE-CONCENTRATED に倒れる（現状挙動のピン）。
#[test]
fn diff_report_one_by_one_image_does_not_panic() {
    let a = RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]));
    let report = diff_report(&a, &a.clone());
    assert!(report.starts_with("IDENTICAL"), "got: {report}");
    let b = RgbaImage::from_pixel(1, 1, image::Rgba([99, 20, 30, 255]));
    let report = diff_report(&a, &b);
    assert!(report.starts_with("NOT IDENTICAL"), "got: {report}");
    assert!(
        report.contains("EDGE-CONCENTRATED"),
        "1x1 diff falls into the edge band by construction, got: {report}"
    );
}

/// 全画素 A=0 で RGB だけ違う 2 枚は NOT IDENTICAL。これは**仕様**（意図した現状の
/// ピン）: ハーネスは「見た目の等価」ではなくバイト列の等価を見る。A=0 下の RGB 差
/// は present 経路の premultiply / clear 色の扱い差を示す手がかりになるので、
/// 握り潰さずに数える。
#[test]
fn diff_report_counts_rgb_diff_under_zero_alpha() {
    let a = RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 0]));
    let b = RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 0]));
    let report = diff_report(&a, &b);
    assert!(
        report.starts_with("NOT IDENTICAL"),
        "RGB diffs under A=0 must count (byte equality, by design), got: {report}"
    );
}

/// 非正方（4×2）の行優先ピン。JS（web/src/lib/abLogic.ts buildSyntheticSourceRgb）
/// 側のピンと**同一のバイト列**を両側で固定して、width/height の取り違え
/// （列優先化）を双方向で防ぐ。
// SYNC WITH web/src/lib/abLogic.test.ts の S3 テスト（4×2 の期待バイトは同一値）
#[test]
fn synthetic_source_non_square_is_row_major() {
    let rgb = synthetic_source_rgb(4, 2);
    assert_eq!(
        rgb,
        vec![
            0, 0, 0, 7, 11, 13, 14, 22, 26, 21, 33, 39, // y=0: x=0..3
            13, 5, 7, 20, 16, 20, 27, 27, 33, 34, 38, 46, // y=1: x=0..3
        ]
    );
}
