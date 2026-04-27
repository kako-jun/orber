use clap::{Parser, ValueEnum};
use orber::animate::MotionPreset;
use orber::cluster::extract_clusters;
use orber::orb::{render_static, RenderOptions};
use orber::output_mode::OutputMode;
use orber::style::{render_css, render_svg, StyleOptions};
use orber::video::{render_video, VideoCodec, VideoOptions};
use std::path::PathBuf;
use std::process::ExitCode;

/// Drift speed of orbs in animated outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Motion {
    /// No movement.
    Still,
    /// Slow, leisurely drift (default).
    Slow,
    /// Lively, faster drift.
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

/// Shape used to render each orb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Shape {
    /// Plain circular orb (default).
    Circle,
    /// Watercolor-style irregular bleed.
    Aquarelle,
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
    #[arg(short, long)]
    output: PathBuf,

    /// Random seed for reproducible output.
    #[arg(long)]
    seed: Option<u64>,

    /// Orb size as a relative multiplier (1.0 = default).
    #[arg(long, default_value_t = 1.0)]
    orb_size: f32,

    /// Blur strength in 0.0..=1.0.
    #[arg(long, default_value_t = 0.5)]
    blur: f32,

    /// Drift speed for animated outputs.
    #[arg(long, value_enum, default_value_t = Motion::Slow)]
    motion: Motion,

    /// Orb rendering shape.
    #[arg(long, value_enum, default_value_t = Shape::Circle)]
    shape: Shape,

    /// Saturation multiplier (1.0 = unchanged).
    #[arg(long, default_value_t = 1.0)]
    saturation: f32,

    /// Animated output duration in milliseconds (1000..=600000, i.e. 1s..=10min).
    #[arg(long, default_value_t = 5000)]
    duration_ms: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let mode = match OutputMode::from_path(&cli.output) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("orber: {e}");
            return ExitCode::from(2);
        }
    };

    if let Some(codec) = VideoCodec::from_output_mode(mode) {
        return render_video_path(&cli, codec);
    }

    match mode {
        OutputMode::Png => render_png(&cli),
        OutputMode::Svg | OutputMode::Css => render_style_path(&cli, mode),
        _ => {
            eprintln!("orber: output mode {mode:?} is not yet implemented");
            ExitCode::from(1)
        }
    }
}

fn render_style_path(cli: &Cli, mode: OutputMode) -> ExitCode {
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
    };

    // 4. mode で書き出しを分岐。
    let content = match mode {
        OutputMode::Svg => render_svg(&clusters, &opts),
        OutputMode::Css => render_css(&clusters, &opts),
        _ => unreachable!("render_style_path called with non-style mode {mode:?}"),
    };

    if let Err(e) = std::fs::write(&cli.output, content) {
        eprintln!(
            "orber: failed to write output {}: {e}",
            cli.output.display()
        );
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", cli.output.display());
    ExitCode::SUCCESS
}

fn render_video_path(cli: &Cli, codec: VideoCodec) -> ExitCode {
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
    let opts = VideoOptions {
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        motion: cli.motion.into(),
        seed: cli.seed.unwrap_or(0),
    };

    // 4. 動画書き出し。進捗とフレーム数の検証は render_video が担当する。
    if let Err(e) = render_video(&clusters, &opts, &cli.output, cli.duration_ms, codec) {
        eprintln!("orber: video render failed: {e}");
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", cli.output.display());
    ExitCode::SUCCESS
}

fn render_png(cli: &Cli) -> ExitCode {
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
        ..RenderOptions::default()
    };

    // 4. 静的描画。
    let out = render_static(&clusters, &opts);

    // 5. 保存。
    if let Err(e) = out.save(&cli.output) {
        eprintln!(
            "orber: failed to write output {}: {e}",
            cli.output.display()
        );
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", cli.output.display());
    ExitCode::SUCCESS
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
        let motion: MotionPreset = cli.motion.into();
        assert_eq!(motion, a.motion, "motion default mismatch");
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
}
