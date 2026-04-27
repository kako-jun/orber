use clap::{Parser, ValueEnum};
use orber::animate::{MotionPreset, MotionShape, MotionSpeed};
use orber::aquarelle::AquarelleParams;
use orber::background::{resolve as resolve_background, Background};
use orber::cluster::{extract_clusters, Cluster};
use orber::color_mod::apply_color_mod;
use orber::orb::{render_static, OrbShape, RenderOptions};
use orber::output_mode::OutputMode;
use orber::style::{render_css, render_svg, StyleOptions};
use orber::variations::{select_specs, VariationKind, VariationMode, VariationSpec};
use orber::video::{render_video, VideoCodec, VideoOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Back-compat motion preset (`--motion`). Equivalent to a fixed (shape, speed) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Motion {
    /// No movement (shape=still).
    Still,
    /// Slow Lissajous drift (default).
    Slow,
    /// Lively Lissajous drift.
    Lively,
}

impl From<Motion> for MotionPreset {
    fn from(m: Motion) -> Self {
        match m {
            Motion::Still => MotionPreset::Still,
            Motion::Slow => MotionPreset::Slow,
            Motion::Lively => MotionPreset::Lively,
        }
    }
}

/// Orbit shape (`--motion-shape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliMotionShape {
    Still,
    Lissajous,
    Vertical,
    Horizontal,
    Diagonal,
    Breathe,
    Twinkle,
}

impl From<CliMotionShape> for MotionShape {
    fn from(s: CliMotionShape) -> Self {
        match s {
            CliMotionShape::Still => MotionShape::Still,
            CliMotionShape::Lissajous => MotionShape::Lissajous,
            CliMotionShape::Vertical => MotionShape::Vertical,
            CliMotionShape::Horizontal => MotionShape::Horizontal,
            CliMotionShape::Diagonal => MotionShape::Diagonal,
            CliMotionShape::Breathe => MotionShape::Breathe,
            CliMotionShape::Twinkle => MotionShape::Twinkle,
        }
    }
}

/// Motion speed (`--motion-speed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliMotionSpeed {
    Subtle,
    Slow,
    Lively,
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

impl From<CliMotionSpeed> for MotionSpeed {
    fn from(s: CliMotionSpeed) -> Self {
        match s {
            CliMotionSpeed::Subtle => MotionSpeed::Subtle,
            CliMotionSpeed::Slow => MotionSpeed::Slow,
            CliMotionSpeed::Lively => MotionSpeed::Lively,
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
    /// (still ×3, drift ×4, breathe ×1, lissajous ×2 = up to 10).
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

    /// Back-compat drift preset for animated outputs. Equivalent to a fixed
    /// (motion-shape, motion-speed) pair: still→(still,slow), slow→(lissajous,slow),
    /// lively→(lissajous,lively). Overridden if --motion-shape or --motion-speed
    /// is also passed.
    #[arg(long, value_enum, default_value_t = Motion::Slow)]
    motion: Motion,

    /// Orbit shape independent of speed. Overrides the shape implied by --motion.
    #[arg(long, value_enum)]
    motion_shape: Option<CliMotionShape>,

    /// Motion speed/amplitude independent of shape. Overrides the speed implied by --motion.
    #[arg(long, value_enum)]
    motion_speed: Option<CliMotionSpeed>,

    /// Orb rendering shape.
    #[arg(long, value_enum, default_value_t = Shape::Circle)]
    shape: Shape,

    /// Saturation multiplier (0.0..=4.0; 1.0 = unchanged).
    #[arg(long, default_value_t = 1.0, value_parser = parse_saturation)]
    saturation: f32,

    /// Animated output duration in milliseconds (1000..=600000, i.e. 1s..=10min).
    #[arg(long, default_value_t = 5000, value_parser = clap::value_parser!(u64).range(1000..=600_000))]
    duration_ms: u64,

    /// Background color: black, white, auto, transparent, or #RRGGBB(AA).
    /// `auto` picks a dimmed average color of the input image.
    /// `transparent` is rejected for mp4/webm (yuv420p has no alpha).
    #[arg(long, default_value = "auto")]
    background: String,

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

    let bg: Background = match cli.background.parse() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("orber: {e}");
            return ExitCode::from(2);
        }
    };

    if let Some(n) = cli.variations {
        return render_variations(&cli, n, bg);
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
        if bg.is_transparent() {
            eprintln!(
                "orber: --background transparent is not supported for {mode:?} (yuv420p has no alpha channel)"
            );
            return ExitCode::from(2);
        }
        return render_video_path(&cli, &output, codec, bg);
    }

    match mode {
        OutputMode::Png => render_png(&cli, &output, bg),
        OutputMode::Svg | OutputMode::Css => render_style_path(&cli, &output, mode, bg),
        _ => {
            eprintln!("orber: output mode {mode:?} is not yet implemented");
            ExitCode::from(1)
        }
    }
}

/// `--motion` の preset と `--motion-shape` / `--motion-speed` の上書きを統合する。
///
/// 個別フラグが指定されていればそちらを優先、なければ `--motion` 由来の組を使う。
fn resolve_motion(cli: &Cli) -> (MotionShape, MotionSpeed) {
    let preset: MotionPreset = cli.motion.into();
    let (mut shape, mut speed) = preset.split();
    if let Some(s) = cli.motion_shape {
        shape = s.into();
    }
    if let Some(sp) = cli.motion_speed {
        speed = sp.into();
    }
    (shape, speed)
}

fn render_style_path(cli: &Cli, output: &Path, mode: OutputMode, bg: Background) -> ExitCode {
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

    // 3. style オプション構築。
    let opts = StyleOptions {
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        background: resolve_background(&img, bg),
    };

    // 4. mode で書き出しを分岐。
    let content = match mode {
        OutputMode::Svg => render_svg(&clusters, &opts),
        OutputMode::Css => render_css(&clusters, &opts),
        _ => unreachable!("render_style_path called with non-style mode {mode:?}"),
    };

    if let Err(e) = std::fs::write(output, content) {
        eprintln!("orber: failed to write output {}: {e}", output.display());
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", output.display());
    ExitCode::SUCCESS
}

fn render_video_path(cli: &Cli, output: &Path, codec: VideoCodec, bg: Background) -> ExitCode {
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

    // 3. ビデオオプション構築。解像度は固定。
    let (shape, speed) = resolve_motion(cli);
    let opts = VideoOptions {
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        motion_shape: shape,
        motion_speed: speed,
        seed: cli.seed.unwrap_or(0),
        background: resolve_background(&img, bg),
        shape: cli.orb_shape(),
    };

    // 4. 動画書き出し。進捗とフレーム数の検証は render_video が担当する。
    if let Err(e) = render_video(&clusters, &opts, output, cli.duration_ms, codec) {
        eprintln!("orber: video render failed: {e}");
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", output.display());
    ExitCode::SUCCESS
}

fn render_png(cli: &Cli, output: &Path, bg: Background) -> ExitCode {
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

    // 3. 描画オプション構築（解像度はデフォルトの縦長 1080x1920）。
    // width/height は当面デフォルト固定。CLI フラグ化は将来 Issue で対応する。
    let opts = RenderOptions {
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        background: resolve_background(&img, bg),
        shape: cli.orb_shape(),
        ..RenderOptions::default()
    };

    // 4. 静的描画。
    let out = render_static(&clusters, &opts);

    // 5. 保存。
    if let Err(e) = out.save(output) {
        eprintln!("orber: failed to write output {}: {e}", output.display());
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", output.display());
    ExitCode::SUCCESS
}

/// `--variations` 経路。`output_dir` を作って各 spec で逐次書き出す。
fn render_variations(cli: &Cli, n: usize, bg: Background) -> ExitCode {
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

    // 入力画像は全 spec で共有。クラスタ抽出は spec.cluster_count に依存するので
    // spec ごとにキャッシュ付きで行う（cluster_count が同じなら使い回し）。
    let img = match image::open(&cli.input) {
        Ok(img) => img.to_rgb8(),
        Err(e) => {
            eprintln!("orber: failed to read input {}: {e}", cli.input.display());
            return ExitCode::from(2);
        }
    };
    let resolved_bg = resolve_background(&img, bg);

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
    // cluster_count → base clusters のキャッシュ。同じ k は再計算せずに使い回す。
    let mut cluster_cache: Vec<(usize, Vec<Cluster>)> = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let idx = i + 1;
        let filename = format!("{idx:02}_{}.{}", spec.label, spec.kind.ext());
        let out_path = dir.join(&filename);
        eprintln!("orber: variation {idx}/{total} ({filename})");

        let base_clusters = match cluster_cache.iter().find(|(k, _)| *k == spec.cluster_count) {
            Some((_, c)) => c.clone(),
            None => match extract_clusters(&img, spec.cluster_count) {
                Ok(c) => {
                    cluster_cache.push((spec.cluster_count, c.clone()));
                    c
                }
                Err(e) => {
                    eprintln!("orber: cluster extraction failed: {e}");
                    return ExitCode::from(2);
                }
            },
        };
        // spec の色軸（hue / lightness / saturation / dominant_rotation）を適用する。
        // saturation は ColorMod 経由で HSL に乗るため、render 側に渡す saturation は
        // 1.0 に固定する（二重に saturation がかからないように）。
        let modulated = apply_color_mod(base_clusters, &spec.color_mod());

        // 動画 + 透過は不可（yuv420p）。bg が transparent なら black に置換して進める。
        let spec_bg = if spec.kind == VariationKind::Mp4 && resolved_bg[3] == 0 {
            [0, 0, 0, 255]
        } else {
            resolved_bg
        };
        let result = render_one_variation(&modulated, spec, &out_path, spec_bg, orb_shape);
        if let Err(msg) = result {
            eprintln!("orber: variation {idx} ({filename}) failed: {msg}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

fn render_one_variation(
    clusters: &[Cluster],
    spec: &VariationSpec,
    out_path: &std::path::Path,
    bg_rgba: [u8; 4],
    orb_shape: OrbShape,
) -> Result<(), String> {
    // 彩度は ColorMod 経由で HSL に既に乗っているので、render 側では 1.0 固定にする。
    // saturation を二重にかけると意図しない色破綻が起きる。
    match spec.kind {
        VariationKind::Png => {
            let opts = RenderOptions {
                orb_size: spec.orb_size,
                blur: spec.blur,
                saturation: 1.0,
                background: bg_rgba,
                shape: orb_shape,
                ..RenderOptions::default()
            };
            let img = render_static(clusters, &opts);
            img.save(out_path).map_err(|e| e.to_string())
        }
        VariationKind::Mp4 => {
            let opts = VideoOptions {
                orb_size: spec.orb_size,
                blur: spec.blur,
                saturation: 1.0,
                motion_shape: spec.shape,
                motion_speed: spec.speed,
                seed: spec.seed,
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
        // ここで motion/orb_size/blur/saturation の SoT 一致を担保する。
        let cli = Cli::parse_from(["orber", "--input", "x", "--output", "x.mp4"]);
        let a = AnimateOptions::default();
        let (shape, speed) = resolve_motion(&cli);
        assert_eq!(shape, a.motion_shape, "motion_shape default mismatch");
        assert_eq!(speed, a.motion_speed, "motion_speed default mismatch");
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
    fn aquarelle_params_out_of_range_rejected() {
        assert!(try_parse(&["--aquarelle-bleed", "1.5"]).is_err());
        assert!(try_parse(&["--aquarelle-bloom", "-0.1"]).is_err());
        assert!(try_parse(&["--aquarelle-offset", "0.7"]).is_ok());
        assert!(try_parse(&["--aquarelle-halo", "0.0"]).is_ok());
    }
}
