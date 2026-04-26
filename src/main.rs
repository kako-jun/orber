use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "orber")]
#[command(version)]
#[command(about = "Turn photos and videos into abstract orb mood output")]
struct Cli {
    /// Input image or video file
    #[arg(short, long)]
    input: PathBuf,

    /// Output file (PNG / MP4 / SVG inferred from extension)
    #[arg(short, long)]
    output: PathBuf,

    /// Random seed for reproducible output
    #[arg(long)]
    seed: Option<u64>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    eprintln!(
        "orber: input={} output={} seed={:?}",
        cli.input.display(),
        cli.output.display(),
        cli.seed
    );
    eprintln!("not yet implemented");
    ExitCode::from(1)
}
