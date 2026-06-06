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
