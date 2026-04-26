use clap::{Parser, ValueEnum};
use orber::output_mode::OutputMode;
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

    /// Animated output duration in milliseconds.
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

    // TODO(#2): replace this debug dump with the real pipeline dispatch
    eprintln!(
        "orber: input={} output={} mode={:?} seed={:?} orb_size={} blur={} motion={:?} shape={:?} saturation={} duration_ms={}",
        cli.input.display(),
        cli.output.display(),
        mode,
        cli.seed,
        cli.orb_size,
        cli.blur,
        cli.motion,
        cli.shape,
        cli.saturation,
        cli.duration_ms,
    );
    eprintln!("not yet implemented");
    ExitCode::from(1)
}
