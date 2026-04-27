use clap::{Parser, ValueEnum};
use orber::animate::{MotionDirection, MotionSpeed};
use orber::aquarelle::AquarelleParams;
use orber::cluster::{derive_background_rgba, drop_dominant, extract_clusters, Cluster};
use orber::orb::{OrbShape, RenderOptions};
use orber::output_mode::OutputMode;
use orber::style::{render_css, render_svg, StyleOptions};
use orber::variations::{select_specs, VariationKind, VariationMode, VariationSpec};
use orber::video::{render_video, VideoCodec, VideoOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Conveyor-belt direction (`--direction`). All orbs flow the same way for the entire clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliDirection {
    /// Left to right.
    Lr,
    /// Right to left.
    Rl,
    /// Top to bottom.
    Tb,
    /// Bottom to top.
    Bt,
}

impl From<CliDirection> for MotionDirection {
    fn from(d: CliDirection) -> Self {
        match d {
            CliDirection::Lr => MotionDirection::LeftToRight,
            CliDirection::Rl => MotionDirection::RightToLeft,
            CliDirection::Tb => MotionDirection::TopToBottom,
            CliDirection::Bt => MotionDirection::BottomToTop,
        }
    }
}

/// Conveyor-belt speed (`--speed`). Coarse 2-step preset; per-orb phase scatter is automatic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliSpeed {
    /// One screen-cross over the whole clip (most calm).
    VerySlow,
    /// Two screen-crosses over the whole clip (default).
    Slow,
}

impl From<CliSpeed> for MotionSpeed {
    fn from(s: CliSpeed) -> Self {
        match s {
            CliSpeed::VerySlow => MotionSpeed::VerySlow,
            CliSpeed::Slow => MotionSpeed::Slow,
        }
    }
}

/// `--variations-mode` の選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliVariationMode {
    Still,
    Video,
    Mixed,
}

impl From<CliVariationMode> for VariationMode {
    fn from(m: CliVariationMode) -> Self {
        match m {
            CliVariationMode::Still => VariationMode::Still,
            CliVariationMode::Video => VariationMode::Video,
            CliVariationMode::Mixed => VariationMode::Mixed,
        }
    }
}

/// Shape used to render each orb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Shape {
    /// Plain circular orb (default).
    Circle,
    /// Cel-painted nightscape texture set: bleed + bloom + offset + halo.
    Aquarelle,
}

impl Shape {
    fn to_orb_shape(self, params: AquarelleParams) -> OrbShape {
        match self {
            Shape::Circle => OrbShape::Circle,
            Shape::Aquarelle => OrbShape::Aquarelle(params),
        }
    }
}

/// f32 のパース + 有限性 + 範囲チェックを 1 つにまとめた値パーサ。
///
/// NaN / 無限大は弾く。clap の `value_parser` 互換シグネチャ。
fn parse_f32_in_range(min: f32, max: f32) -> impl Fn(&str) -> Result<f32, String> + Clone {
    move |s: &str| {
        let v: f32 = s
            .parse()
            .map_err(|e: std::num::ParseFloatError| e.to_string())?;
        if !v.is_finite() {
            return Err(format!("must be a finite number (not NaN/inf), got {v}"));
        }
        if v < min || v > max {
            return Err(format!("must be in {min}..={max}, got {v}"));
        }
        Ok(v)
    }
}

fn parse_orb_size(s: &str) -> Result<f32, String> {
    parse_f32_in_range(0.0, 10.0)(s)
}
fn parse_count(s: &str) -> Result<usize, String> {
    let v: usize = s
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    if !(1..=200).contains(&v) {
        return Err(format!("must be in 1..=200, got {v}"));
    }
    Ok(v)
}
fn parse_unit_interval(s: &str) -> Result<f32, String> {
    parse_f32_in_range(0.0, 1.0)(s)
}
fn parse_saturation(s: &str) -> Result<f32, String> {
    parse_f32_in_range(0.0, 4.0)(s)
}

#[derive(Debug, Parser)]
#[command(name = "orber")]
#[command(version)]
#[command(about = "Turn photos and videos into abstract orb mood output")]
struct Cli {
    /// Input image or video file.
    #[arg(short, long)]
    input: PathBuf,

    /// Output file. Format inferred from extension: png, webp, mp4, webm, svg, css.
    /// Required for the single-output mode (omitted when --variations is set).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Generate N variations of the input under --output-dir instead of a single file.
    /// Requires --output-dir. Variations are picked from a curated preset table
    /// (4 still snapshots + 6 mp4 flows = up to 10).
    #[arg(long)]
    variations: Option<usize>,

    /// Output directory for --variations mode. Created if it does not exist.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Filter for --variations: only stills, only videos, or mixed (default).
    #[arg(long, value_enum, default_value_t = CliVariationMode::Mixed)]
    variations_mode: CliVariationMode,

    /// Random seed for reproducible output.
    #[arg(long)]
    seed: Option<u64>,

    /// Orb size as a relative multiplier (0.0..=10.0; 1.0 = default).
    #[arg(long, default_value_t = 1.0, value_parser = parse_orb_size)]
    orb_size: f32,

    /// Blur strength (0.0..=1.0).
    #[arg(long, default_value_t = 0.5, value_parser = parse_unit_interval)]
    blur: f32,

    /// Conveyor-belt direction. All orbs flow the same way for the entire clip.
    #[arg(long, value_enum, default_value_t = CliDirection::Lr)]
    direction: CliDirection,

    /// Conveyor-belt speed. Coarse 2-step preset over the whole clip.
    #[arg(long, value_enum, default_value_t = CliSpeed::Slow)]
    speed: CliSpeed,

    /// Number of orbs visible on screen at once (1..=200, default 20).
    /// Clusters are expanded to this count by weight-proportional color sampling
    /// and per-orb scattering on the cross axis. Higher count fills more of the
    /// frame; ~20 fills roughly 70% on the default size.
    #[arg(long, default_value_t = 20, value_parser = parse_count)]
    count: usize,

    /// Orb rendering shape.
    #[arg(long, value_enum, default_value_t = Shape::Circle)]
    shape: Shape,

    /// Saturation multiplier (0.0..=4.0; 1.0 = unchanged).
    #[arg(long, default_value_t = 1.0, value_parser = parse_saturation)]
    saturation: f32,

    /// Animated output duration in milliseconds (1000..=600000, i.e. 1s..=10min).
    #[arg(long, default_value_t = 5000, value_parser = clap::value_parser!(u64).range(1000..=600_000))]
    duration_ms: u64,

    /// Aquarelle: bleed strength (0.0..=1.0). Only used with --shape aquarelle.
    #[arg(long, default_value_t = 0.5, value_parser = parse_unit_interval)]
    aquarelle_bleed: f32,

    /// Aquarelle: blown-out core strength (0.0..=1.0). Only used with --shape aquarelle.
    #[arg(long, default_value_t = 0.5, value_parser = parse_unit_interval)]
    aquarelle_bloom: f32,

    /// Aquarelle: gradient center offset (0.0..=1.0). Only used with --shape aquarelle.
    #[arg(long, default_value_t = 0.5, value_parser = parse_unit_interval)]
    aquarelle_offset: f32,

    /// Aquarelle: peripheral saturation (halo) (0.0..=1.0). Only used with --shape aquarelle.
    #[arg(long, default_value_t = 0.5, value_parser = parse_unit_interval)]
    aquarelle_halo: f32,
}

impl Cli {
    fn aquarelle_params(&self) -> AquarelleParams {
        AquarelleParams {
            bleed: self.aquarelle_bleed,
            bloom: self.aquarelle_bloom,
            offset: self.aquarelle_offset,
            halo: self.aquarelle_halo,
        }
    }

    fn orb_shape(&self) -> OrbShape {
        self.shape.to_orb_shape(self.aquarelle_params())
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Some(n) = cli.variations {
        return render_variations(&cli, n);
    }

    let output = match &cli.output {
        Some(p) => p.clone(),
        None => {
            eprintln!("orber: either --output FILE or --variations N --output-dir DIR is required");
            return ExitCode::from(2);
        }
    };

    let mode = match OutputMode::from_path(&output) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("orber: {e}");
            return ExitCode::from(2);
        }
    };

    if let Some(codec) = VideoCodec::from_output_mode(mode) {
        return render_video_path(&cli, &output, codec);
    }

    match mode {
        OutputMode::Png => render_png(&cli, &output),
        OutputMode::Svg | OutputMode::Css => render_style_path(&cli, &output, mode),
        _ => {
            eprintln!("orber: output mode {mode:?} is not yet implemented");
            ExitCode::from(1)
        }
    }
}

/// CLI の `--direction` / `--speed` を内部表現に変換する。
fn resolve_motion(cli: &Cli) -> (MotionDirection, MotionSpeed) {
    (cli.direction.into(), cli.speed.into())
}

/// orb プールが空になった（K=1 / 単色画像）ときに stderr で警告し、処理は継続する。
///
/// `drop_dominant` でドミナントクラスタを 1 個落としたあと、残りが 0 個になる
/// ケースを検出する。フォールバックは入れない（背景塗りだけの出力になる）。
fn warn_if_orb_pool_empty(orb_clusters: &[Cluster]) {
    if orb_clusters.is_empty() {
        eprintln!(
            "orber: warning: input image yielded only 1 cluster; orb pool is empty (output will be background only)"
        );
    }
}

/// `--shape aquarelle` と `--count` の組合せを使ったときに stderr で警告する。
///
/// Aquarelle 経路は cluster 数だけ orb を描画する設計（per-orb の独立揺らぎを
/// 入れると bleed/bloom/halo の質感セットが壊れるため）。CLI からは aquarelle の
/// ときだけ count が無視される事実が見えないので、ここで明示的に教える。
fn warn_if_aquarelle_count_ignored(cli: &Cli) {
    if matches!(cli.shape, Shape::Aquarelle) {
        eprintln!(
            "orber: warning: aquarelle shape ignores --count (rendering one orb per k-means cluster from the palette)"
        );
    }
}

fn render_style_path(cli: &Cli, output: &Path, mode: OutputMode) -> ExitCode {
    // 1. 入力画像を読み込み RGB8 に正規化。
    let img = match image::open(&cli.input) {
        Ok(img) => img.to_rgb8(),
        Err(e) => {
            eprintln!("orber: failed to read input {}: {e}", cli.input.display());
            return ExitCode::from(2);
        }
    };

    // 2. 代表色クラスタ抽出（k=6 固定。後の Issue で CLI 化検討）。
    let clusters = match extract_clusters(&img, 6) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("orber: cluster extraction failed: {e}");
            return ExitCode::from(2);
        }
    };

    // 3. 背景色を最大 weight クラスタから自動派生し、orb プールはそれを除いた残り。
    let background = derive_background_rgba(&clusters);
    let orb_clusters = drop_dominant(&clusters);
    warn_if_orb_pool_empty(&orb_clusters);

    // 4. style オプション構築。
    let opts = StyleOptions {
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        background,
    };

    // 5. mode で書き出しを分岐。
    let content = match mode {
        OutputMode::Svg => render_svg(&orb_clusters, &opts),
        OutputMode::Css => render_css(&orb_clusters, &opts),
        _ => unreachable!("render_style_path called with non-style mode {mode:?}"),
    };

    if let Err(e) = std::fs::write(output, content) {
        eprintln!("orber: failed to write output {}: {e}", output.display());
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", output.display());
    ExitCode::SUCCESS
}

fn render_video_path(cli: &Cli, output: &Path, codec: VideoCodec) -> ExitCode {
    // 1. 入力画像を読み込み RGB8 に正規化。
    let img = match image::open(&cli.input) {
        Ok(img) => img.to_rgb8(),
        Err(e) => {
            eprintln!("orber: failed to read input {}: {e}", cli.input.display());
            return ExitCode::from(2);
        }
    };

    // 2. 代表色クラスタ抽出（k=6 固定。後の Issue で CLI 化検討）。
    let clusters = match extract_clusters(&img, 6) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("orber: cluster extraction failed: {e}");
            return ExitCode::from(2);
        }
    };

    // 3. 背景色を自動派生し、orb プールはドミナントを除いた残り。
    let background = derive_background_rgba(&clusters);
    let orb_clusters = drop_dominant(&clusters);
    warn_if_orb_pool_empty(&orb_clusters);
    warn_if_aquarelle_count_ignored(cli);

    // 4. ビデオオプション構築。解像度は固定。
    let (direction, speed) = resolve_motion(cli);
    let opts = VideoOptions {
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        direction,
        speed,
        seed: cli.seed.unwrap_or(0),
        count: Some(cli.count),
        background,
        shape: cli.orb_shape(),
    };

    // 5. 動画書き出し。進捗とフレーム数の検証は render_video が担当する。
    if let Err(e) = render_video(&orb_clusters, &opts, output, cli.duration_ms, codec) {
        eprintln!("orber: video render failed: {e}");
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", output.display());
    ExitCode::SUCCESS
}

fn render_png(cli: &Cli, output: &Path) -> ExitCode {
    // 1. 入力画像を読み込み RGB8 に正規化。
    let img = match image::open(&cli.input) {
        Ok(img) => img.to_rgb8(),
        Err(e) => {
            eprintln!("orber: failed to read input {}: {e}", cli.input.display());
            return ExitCode::from(2);
        }
    };

    // 2. 代表色クラスタ抽出（k=6 固定。後の Issue で CLI 化検討）。
    let clusters = match extract_clusters(&img, 6) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("orber: cluster extraction failed: {e}");
            return ExitCode::from(2);
        }
    };

    // 3. 背景色を自動派生し、orb プールはドミナントを除いた残り。
    let background = derive_background_rgba(&clusters);
    let orb_clusters = drop_dominant(&clusters);
    warn_if_orb_pool_empty(&orb_clusters);
    warn_if_aquarelle_count_ignored(cli);

    // 4. PNG は「コンベアベルトの一瞬」（t=0）として animate::render_frame 経由で
    //    描画する。これで --count による orb 数の展開が単発出力でも効く。
    //    （render_static は count を解釈しないので、count=clusters.len() に固定された
    //     旧経路にしないよう animate::render_frame を一律に使う。）
    let (direction, speed) = resolve_motion(cli);
    let frame_opts = orber::animate::AnimateOptions {
        width: RenderOptions::default().width,
        height: RenderOptions::default().height,
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        direction,
        speed,
        seed: cli.seed.unwrap_or(0),
        count: Some(cli.count),
        background,
        shape: cli.orb_shape(),
    };
    let out = orber::animate::render_frame(&orb_clusters, &frame_opts, 0.0);

    // 4. 保存。
    if let Err(e) = out.save(output) {
        eprintln!("orber: failed to write output {}: {e}", output.display());
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", output.display());
    ExitCode::SUCCESS
}

/// `--variations` 経路。`output_dir` を作って各 spec で逐次書き出す。
fn render_variations(cli: &Cli, n: usize) -> ExitCode {
    let dir = match &cli.output_dir {
        Some(d) => d.clone(),
        None => {
            eprintln!("orber: --variations requires --output-dir DIR");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("orber: failed to create output dir {}: {e}", dir.display());
        return ExitCode::from(2);
    }

    // 入力画像は全 spec で共有。
    let img = match image::open(&cli.input) {
        Ok(img) => img.to_rgb8(),
        Err(e) => {
            eprintln!("orber: failed to read input {}: {e}", cli.input.display());
            return ExitCode::from(2);
        }
    };

    let specs = select_specs(n, cli.variations_mode.into());
    if specs.is_empty() {
        eprintln!(
            "orber: no variations matched (requested n={n}, mode={:?})",
            cli.variations_mode
        );
        return ExitCode::from(2);
    }

    let total = specs.len();
    if total < n {
        eprintln!(
            "orber: only {total} variation(s) available for mode {:?} (requested {n})",
            cli.variations_mode
        );
    }

    let orb_shape = cli.orb_shape();
    // クラスタ抽出は preset 全 spec で 1 回だけ（K は VARIATIONS_KMEANS_K で固定）。
    // パレットを spec ごとに変えると入力画像の色が崩れるので、ここはキャッシュではなく
    // 単一パレットを使い回す方針。
    let base_clusters = match extract_clusters(&img, VARIATIONS_KMEANS_K) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("orber: cluster extraction failed: {e}");
            return ExitCode::from(2);
        }
    };

    // 背景色は最大 weight クラスタから自動派生し、orb プールはそれを除いた残り。
    // 全 spec で共通（同じ入力画像から同じ背景・同じ orb プールを使う）。
    let background = derive_background_rgba(&base_clusters);
    let orb_clusters = drop_dominant(&base_clusters);
    warn_if_orb_pool_empty(&orb_clusters);
    warn_if_aquarelle_count_ignored(cli);

    for (i, spec) in specs.iter().enumerate() {
        let idx = i + 1;
        let filename = format!("{idx:02}_{}.{}", spec.label, spec.kind.ext());
        let out_path = dir.join(&filename);
        eprintln!("orber: variation {idx}/{total} ({filename})");

        let result = render_one_variation(&orb_clusters, spec, &out_path, background, orb_shape);
        if let Err(msg) = result {
            eprintln!("orber: variation {idx} ({filename}) failed: {msg}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

/// `--variations` 経路で使うクラスタ数（kmeans の K）。spec ごとには変えない。
///
/// 5 個に固定すると主要色が拾え、かつパレットが崩れにくい。後で動かしたくなったら
/// `VariationSpec` に `palette_k` フィールドを足す形で復活させる。
const VARIATIONS_KMEANS_K: usize = 5;

fn render_one_variation(
    clusters: &[Cluster],
    spec: &VariationSpec,
    out_path: &std::path::Path,
    bg_rgba: [u8; 4],
    orb_shape: OrbShape,
) -> Result<(), String> {
    // saturation は preset で揺らさない（同一画像から作る複数バリエーションでは
    // 入力色をそのまま使う方針）。CLI の単発経路と揃えるため 1.0 固定。
    match spec.kind {
        VariationKind::Png => {
            // 静止画は「コンベアベルトの一瞬」。t=0 のフレームを 1 枚だけレンダリングする。
            // phase 由来で orb が画面全体に散らばり、画面端で半分欠ける構図が自然に出る。
            let frame_opts = orber::animate::AnimateOptions {
                width: RenderOptions::default().width,
                height: RenderOptions::default().height,
                orb_size: spec.orb_size,
                blur: spec.blur,
                saturation: 1.0,
                direction: spec.direction,
                speed: spec.speed,
                seed: spec.seed,
                count: Some(spec.count),
                background: bg_rgba,
                shape: orb_shape,
            };
            let img = orber::animate::render_frame(clusters, &frame_opts, 0.0);
            img.save(out_path).map_err(|e| e.to_string())
        }
        VariationKind::Mp4 => {
            let opts = VideoOptions {
                orb_size: spec.orb_size,
                blur: spec.blur,
                saturation: 1.0,
                direction: spec.direction,
                speed: spec.speed,
                seed: spec.seed,
                count: Some(spec.count),
                background: bg_rgba,
                shape: orb_shape,
            };
            render_video(
                clusters,
                &opts,
                out_path,
                spec.duration_ms,
                VideoCodec::H264,
            )
            .map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orber::animate::AnimateOptions;
    use orber::video::MAX_DURATION_MS;

    #[test]
    fn cli_defaults_match_render_options_defaults() {
        // CLI のデフォルト値（clap の default_value_t）が RenderOptions::default() と
        // 一致していることを保証する。SoT が将来統一されるまでの回帰防止 assert。
        let cli = Cli::parse_from(["orber", "--input", "x", "--output", "x.png"]);
        let defaults = RenderOptions::default();
        assert_eq!(cli.orb_size, defaults.orb_size, "orb_size default mismatch");
        assert_eq!(cli.blur, defaults.blur, "blur default mismatch");
        assert_eq!(
            cli.saturation, defaults.saturation,
            "saturation default mismatch"
        );
        // duration_ms は RenderOptions に対応フィールドが無いので対象外。
    }

    #[test]
    fn cli_defaults_match_animate_options_defaults() {
        // CLI のデフォルトが AnimateOptions::default() と一致することを保証。
        // 動画経路は VideoOptions だが、内部で AnimateOptions を組み立てるため
        // ここで direction/speed/orb_size/blur/saturation の SoT 一致を担保する。
        let cli = Cli::parse_from(["orber", "--input", "x", "--output", "x.mp4"]);
        let a = AnimateOptions::default();
        let (direction, speed) = resolve_motion(&cli);
        assert_eq!(direction, a.direction, "direction default mismatch");
        assert_eq!(speed, a.speed, "speed default mismatch");
        assert_eq!(cli.orb_size, a.orb_size, "orb_size default mismatch");
        assert_eq!(cli.blur, a.blur, "blur default mismatch");
        assert_eq!(cli.saturation, a.saturation, "saturation default mismatch");

        // duration_ms は妥当範囲（>0 かつ <= MAX_DURATION_MS）であること。
        assert!(cli.duration_ms > 0, "duration_ms default must be > 0");
        assert!(
            cli.duration_ms <= MAX_DURATION_MS,
            "duration_ms default must be <= MAX_DURATION_MS, got {}",
            cli.duration_ms
        );
    }

    fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut full = vec!["orber", "--input", "x", "--output", "x.png"];
        full.extend(args);
        Cli::try_parse_from(full)
    }

    #[test]
    fn parse_f32_in_range_helper() {
        // 範囲内 / 範囲外 / NaN / inf / 不正文字列の各分岐をユニットで担保する。
        let p = parse_f32_in_range(0.0, 1.0);
        assert_eq!(p("0.0").unwrap(), 0.0);
        assert_eq!(p("1.0").unwrap(), 1.0);
        assert!(p("1.5").is_err(), "above max should error");
        assert!(p("-0.1").is_err(), "below min should error");
        assert!(p("NaN").is_err(), "NaN should error");
        assert!(p("inf").is_err(), "inf should error");
        assert!(p("xyz").is_err(), "non-numeric should error");
    }

    #[test]
    fn blur_out_of_range_rejected() {
        assert!(try_parse(&["--blur", "1.5"]).is_err());
        assert!(try_parse(&["--blur", "-0.1"]).is_err());
        assert!(try_parse(&["--blur", "NaN"]).is_err());
        assert!(try_parse(&["--blur", "0.5"]).is_ok());
    }

    #[test]
    fn orb_size_out_of_range_rejected() {
        assert!(try_parse(&["--orb-size", "20.0"]).is_err());
        assert!(try_parse(&["--orb-size", "-1.0"]).is_err());
        assert!(try_parse(&["--orb-size", "1.5"]).is_ok());
    }

    #[test]
    fn saturation_out_of_range_rejected() {
        assert!(try_parse(&["--saturation", "5.0"]).is_err());
        assert!(try_parse(&["--saturation", "-0.1"]).is_err());
        assert!(try_parse(&["--saturation", "1.0"]).is_ok());
        assert!(try_parse(&["--saturation", "0.0"]).is_ok());
    }

    #[test]
    fn duration_ms_out_of_range_rejected() {
        assert!(try_parse(&["--duration-ms", "999"]).is_err());
        assert!(try_parse(&["--duration-ms", "600001"]).is_err());
        assert!(try_parse(&["--duration-ms", "1000"]).is_ok());
        assert!(try_parse(&["--duration-ms", "600000"]).is_ok());
    }

    #[test]
    fn count_out_of_range_rejected() {
        assert!(try_parse(&["--count", "0"]).is_err());
        assert!(try_parse(&["--count", "201"]).is_err());
        assert!(try_parse(&["--count", "abc"]).is_err());
        assert!(try_parse(&["--count", "1"]).is_ok());
        assert!(try_parse(&["--count", "20"]).is_ok());
        assert!(try_parse(&["--count", "200"]).is_ok());
    }

    #[test]
    fn count_default_is_twenty() {
        let cli = Cli::parse_from(["orber", "--input", "x", "--output", "x.png"]);
        assert_eq!(cli.count, 20);
    }

    #[test]
    fn aquarelle_params_out_of_range_rejected() {
        assert!(try_parse(&["--aquarelle-bleed", "1.5"]).is_err());
        assert!(try_parse(&["--aquarelle-bloom", "-0.1"]).is_err());
        assert!(try_parse(&["--aquarelle-offset", "0.7"]).is_ok());
        assert!(try_parse(&["--aquarelle-halo", "0.0"]).is_ok());
    }
}
