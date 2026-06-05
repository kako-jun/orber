use aquarelle::AquarelleParams;
use clap::{Parser, ValueEnum};
use orber_core::animate::{MotionDirection, MotionSpeed};
use orber_core::cluster::{derive_background_rgba, drop_dominant, extract_clusters, Cluster};
use orber_core::glyph::{has_glyph, GlyphFontId};
use orber_core::orb::{OrbShape, RenderOptions};
use orber_core::output_mode::OutputMode;
use orber_core::style::{render_css, render_svg, SoftnessPreset, StyleOptions};
use orber_core::variations::{select_specs, VariationKind, VariationMode, VariationSpec};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod video;
mod video_input;
use video::{render_video, VideoCodec, VideoOptions};
use video_input::{build_color_tracks, build_keyframe_tracks, is_video_path, sample_video_frames};

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

/// Conveyor-belt speed (`--speed`). Four-step preset; per-orb phase scatter is automatic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliSpeed {
    /// One screen-cross over the whole clip (most calm).
    VerySlow,
    /// Two screen-crosses over the whole clip (previous default).
    Slow,
    /// Three screen-crosses over the whole clip (#55, default).
    Mid,
    /// Four screen-crosses over the whole clip (#55, lively).
    Fast,
}

impl From<CliSpeed> for MotionSpeed {
    fn from(s: CliSpeed) -> Self {
        match s {
            CliSpeed::VerySlow => MotionSpeed::VerySlow,
            CliSpeed::Slow => MotionSpeed::Slow,
            CliSpeed::Mid => MotionSpeed::Mid,
            CliSpeed::Fast => MotionSpeed::Fast,
        }
    }
}

/// Visual softness preset (`--softness`). Single axis, three steps (#55).
/// #205 で全体が blurry 方向にシフトされた。旧 Low は廃止、旧 Mid→新 Low、
/// 旧 High→新 Mid (default)、新 High が追加された。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliSoftness {
    /// Crisper baseline (was the legacy default before #205): less blur, sharper edges.
    Low,
    /// Orb-like softness (new default; same look as the previous "high" preset).
    Mid,
    /// Maximum blur — use under text overlays or for cinematic mood.
    High,
}

impl From<CliSoftness> for SoftnessPreset {
    fn from(c: CliSoftness) -> Self {
        match c {
            CliSoftness::Low => SoftnessPreset::Low,
            CliSoftness::Mid => SoftnessPreset::Mid,
            CliSoftness::High => SoftnessPreset::High,
        }
    }
}

/// Coarse `--count-preset` for users who do not want a numeric value.
/// Mutually exclusive with `--count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliCountPreset {
    /// Fewer orbs (10).
    Low,
    /// Medium (20).
    Mid,
    /// Many (30).
    High,
}

impl CliCountPreset {
    fn to_count(self) -> usize {
        match self {
            CliCountPreset::Low => 10,
            CliCountPreset::Mid => 20,
            CliCountPreset::High => 30,
        }
    }
}

/// `--input-mode` の選択肢。動画入力時の処理パスを切り替える。
///
/// - `ColorTrack` (#7): 位置固定 + 色だけ時間軸補間。`--keyframes` は無視され、
///   `VIDEO_INPUT_N_SAMPLES` 個のサンプル列から色トラックを作る。
/// - `Keyframe` (#33): 色 + 位置 + 重みを `--keyframes` 個のキーから時間軸補間。
///
/// 静止画入力ではどちらも従来挙動（時間軸補間なし）。`Keyframe` を静止画に渡すと
/// 明示エラーで弾く（後段の `run_video_input_keyframe` に到達しないため UI 上の
/// 矛盾を起こさない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
enum CliInputMode {
    /// 色トラックのみ。位置固定、色だけ時間変化（#7、画像入力でも default）。
    #[default]
    ColorTrack,
    /// キーフレーム補間。色 + 位置 + 重みを時間軸キーから補間（#33、video のみ）。
    Keyframe,
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
    /// One bundled-font glyph filled per orb (#55). Pick the glyph with --glyph-char.
    Glyph,
    /// Image silhouette filled per orb (#217). Provide the silhouette with
    /// --image-mask (a raster PNG/JPG/...; SVG is web-only, image crate is raster).
    Image,
}

/// Parse the `--glyph-char` argument: must be exactly one Unicode scalar value.
fn parse_single_char(s: &str) -> Result<char, String> {
    let mut chars = s.chars();
    let first = chars
        .next()
        .ok_or_else(|| "must be a single character (got empty)".to_string())?;
    if chars.next().is_some() {
        return Err(format!(
            "must be a single character (got {} chars in {s:?})",
            s.chars().count()
        ));
    }
    Ok(first)
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
fn parse_variations_n(s: &str) -> Result<usize, String> {
    // --variations 0 は「該当無し」エラーで誤解を招くので、value parser で
    // 1.. を強制する（preset 上限の 10 はメッセージで伝えるため弾かない）。
    let v: usize = s
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    if v == 0 {
        return Err("must be >= 1 (use --variations N with N >= 1)".to_string());
    }
    Ok(v)
}

fn parse_count(s: &str) -> Result<usize, String> {
    let v: usize = s
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    // ライブラリ層の MAX_ORB_COUNT (1024) と揃える。
    if !(1..=1024).contains(&v) {
        return Err(format!("must be in 1..=1024, got {v}"));
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
    /// Requires --output-dir (N >= 1). Variations are picked from a curated preset
    /// table (4 still snapshots + 6 mp4 flows = up to 10).
    #[arg(long, value_parser = parse_variations_n)]
    variations: Option<usize>,

    /// Output directory for --variations mode. Created if it does not exist.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Filter for --variations: only stills, only videos, or mixed (default).
    #[arg(long, value_enum, default_value_t = CliVariationMode::Mixed)]
    variations_mode: CliVariationMode,

    /// Random seed for reproducible output (default 0).
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Orb size as a relative multiplier (0.0..=10.0; 1.0 = default).
    #[arg(long, default_value_t = 1.0, value_parser = parse_orb_size)]
    orb_size: f32,

    /// Blur strength (0.0..=1.0).
    #[arg(long, default_value_t = 0.5, value_parser = parse_unit_interval)]
    blur: f32,

    /// Conveyor-belt direction. All orbs flow the same way for the entire clip.
    #[arg(long, value_enum, default_value_t = CliDirection::Lr)]
    direction: CliDirection,

    /// Conveyor-belt speed. 4-step preset (very-slow / slow / mid / fast)
    /// controlling cycle count per clip.
    #[arg(long, value_enum, default_value_t = CliSpeed::Slow)]
    speed: CliSpeed,

    /// Number of orbs visible on screen at once (1..=1024, default 20).
    /// Use `--count-preset` for a 3-tier shorthand (low=10 / mid=20 / high=30).
    /// Clusters are expanded to this count by weight-proportional color sampling
    /// and per-orb scattering on the cross axis. Higher count fills more of the
    /// frame; ~20 fills roughly 70% on the default size.
    /// Mutually exclusive with --count-preset.
    #[arg(long, default_value_t = 20, value_parser = parse_count, conflicts_with = "count_preset")]
    count: usize,

    /// Coarse orb-count preset (#55): low=10 / mid=20 / high=30.
    /// Mutually exclusive with --count.
    #[arg(long, value_enum)]
    count_preset: Option<CliCountPreset>,

    /// Orb rendering shape.
    #[arg(long, value_enum, default_value_t = Shape::Circle)]
    shape: Shape,

    /// Glyph character used when --shape glyph (#55). Must be a single character.
    /// Defaults to ☆ (U+2606). Use a character covered by Noto Sans Symbols 2.
    #[arg(long, default_value = "\u{2606}", value_parser = parse_single_char)]
    glyph_char: char,

    /// Silhouette image used when --shape image (#217). A raster image (PNG / JPG /
    /// etc.; SVG is web-only since the image crate decodes raster only). This is the
    /// SHAPE source and is separate from --input (the COLOR source). The silhouette
    /// is auto-detected: transparent images use alpha, opaque images use luminance
    /// with auto-polarity (minority = subject). A flat / contrast-free mask is
    /// rejected with an explicit error. Required when --shape image.
    #[arg(long)]
    image_mask: Option<PathBuf>,

    /// Visual softness preset (#55, #205): low / mid / high.
    /// Low = crisper baseline (was the legacy default before #205),
    /// Mid = orb-like softness (new default; same look as the previous "high" preset),
    /// High = maximum blur (use under text overlays or for cinematic mood).
    #[arg(long, value_enum, default_value_t = CliSoftness::Mid)]
    softness: CliSoftness,

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

    /// Input processing mode (#7 / #33). Only meaningful for video input.
    /// `color-track` = #7 (position fixed, color tracks over time, default).
    /// `keyframe` = #33 (color + position + weight all interpolated between keyframes).
    /// Passing `keyframe` with a still image input yields an explicit error.
    #[arg(long = "input-mode", value_enum, default_value_t = CliInputMode::ColorTrack)]
    input_mode: CliInputMode,

    /// Number of keyframes to sample from the video (#33). Default 8. Used only when
    /// `--input-mode keyframe`. Minimum 2 (a single keyframe cannot be interpolated;
    /// values <2 are clamped to 2).
    #[arg(long, default_value_t = 8)]
    keyframes: u32,
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

    /// Resolve the `OrbShape` for this run. For `--shape image` this performs I/O:
    /// it decodes `--image-mask` (CLI-side, default image features) and runs the
    /// silhouette → SDF heuristic ([`orber_core::glyph::image_rgba_to_sdf`]). Returns
    /// `Err(message)` — never panics — when `--image-mask` is missing, unreadable, or
    /// has no usable contrast, so callers can `eprintln!` and exit cleanly.
    fn orb_shape(&self) -> Result<OrbShape, String> {
        match self.shape {
            Shape::Circle => Ok(OrbShape::Circle),
            Shape::Aquarelle => Ok(OrbShape::Aquarelle(self.aquarelle_params())),
            Shape::Glyph => Ok(OrbShape::Glyph {
                ch: self.glyph_char,
                font: GlyphFontId::NotoSymbols2,
            }),
            Shape::Image => self.resolve_image_shape(),
        }
    }

    /// `--shape image` の解決: `--image-mask` をデコードして画像シルエット SDF を作る。
    fn resolve_image_shape(&self) -> Result<OrbShape, String> {
        let path = self.image_mask.as_ref().ok_or_else(|| {
            "--shape image requires --image-mask PATH (the silhouette image; \
             --input is the color source, --image-mask is the shape source)"
                .to_string()
        })?;
        // CLI 側は default features の image crate でデコード（PNG/JPG/WebP 等のラスタ）。
        // SVG はラスタデコーダに無いので非対応（web のみ）。
        let img = image::open(path).map_err(|e| {
            format!(
                "failed to read --image-mask {}: {e} (raster images only; SVG is web-only)",
                path.display()
            )
        })?;
        let rgba = img.to_rgba8();
        let size = orber_core::glyph::DEFAULT_GLYPH_SDF_SIZE;
        let sdf = orber_core::glyph::image_rgba_to_sdf(&rgba, size).ok_or_else(|| {
            format!(
                "--image-mask {} has no usable silhouette contrast (it is blank or a single flat \
                 color); provide an image with a distinct subject vs. background",
                path.display()
            )
        })?;
        Ok(OrbShape::Image {
            sdf: std::sync::Arc::from(sdf),
            size,
        })
    }

    /// Resolved orb count. `--count-preset` wins over `--count` when both are present
    /// (clap already enforces mutual exclusion via `conflicts_with`).
    fn resolved_count(&self) -> usize {
        match self.count_preset {
            Some(p) => p.to_count(),
            None => self.count,
        }
    }

    /// Resolved softness preset for any rendering path.
    fn resolved_softness(&self) -> SoftnessPreset {
        self.softness.into()
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    warn_if_glyph_char_unsupported(&cli);

    // #33 review S3: --input-mode keyframe は動画入力専用。静止画 + variations の
    // 組合せでは silent skip にせず、--variations 分岐より先に明示エラーで弾く。
    if cli.input_mode == CliInputMode::Keyframe && !is_video_path(&cli.input) {
        eprintln!(
            "orber: --input-mode keyframe requires video input (got still image: {})",
            cli.input.display()
        );
        return ExitCode::from(2);
    }

    if let Some(n) = cli.variations {
        // #7 review M1: video + --variations は未対応経路。`render_variations` は
        // `image::open` を直に叩くので動画を渡すと「decoder error」で落ち、
        // ユーザーには「動画が壊れている」かのように見えてしまう。明示エラーで弾く。
        if is_video_path(&cli.input) {
            eprintln!(
                "orber: --variations is not supported with video input; use --output FILE.mp4 / .webm / .png instead"
            );
            return ExitCode::from(2);
        }
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

    // #7: 入力ファイルが動画なら動画入力経路へ分岐。
    // 静止画入力は従来どおり既存パスを通る。
    if is_video_path(&cli.input) {
        return run_video_input(&cli, &output, mode);
    }

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

/// 単一フレーム描画のバックエンド。GPU(WGSL) を唯一のレンダラとする (#225)。
///
/// 全 shape（Circle / Glyph / Aquarelle / Image）が GPU 上で描かれる。count は 1024 まで
/// GPU が data-texture 経路で直接描く（#210 Phase 1a で 64 制限を撤去）。Glyph は #212
/// Phase 1b で WGSL 化（SDF fill + 回転）、Image は同じ SDF shader を共有（#217）、Aquarelle
/// は #216 Phase 1c で WGSL 化（4 層を解析 radial で評価、RNG/色は pack で算出）。GPU アダプタ
/// が取得できない場合は CPU にフォールバックせず、`new` が `None` を返すので呼び出し側が
/// 明示エラーで終了する（CPU 描画は撲滅済み）。
struct FrameRenderer {
    gpu: Box<orber_core::gpu::GpuRenderer>,
}

impl FrameRenderer {
    /// GPU レンダラを初期化する。アダプタが取れなければ `None`（呼び出し側で error 終了）。
    /// CPU フォールバックは持たない（#225 で撲滅）。
    fn new() -> Option<Self> {
        let gpu = orber_core::gpu::GpuRenderer::new()?;
        eprintln!(
            "orber: using gpu renderer (adapter: {})",
            gpu.adapter_name()
        );
        Some(FrameRenderer { gpu: Box::new(gpu) })
    }

    /// 1 フレームを GPU で描画する。shape ごとに専用 WGSL 経路へ dispatch する。
    fn render(
        &self,
        clusters: &[Cluster],
        opts: &orber_core::animate::AnimateOptions,
        t: f32,
    ) -> image::RgbaImage {
        match &opts.shape {
            // Glyph uses the dedicated WGSL glyph path (#212); Image the same SDF
            // path with a supplied texture (#217); Aquarelle the dedicated WGSL
            // four-layer path (#216); Circle the default.
            OrbShape::Glyph { .. } => self.gpu.render_frame_glyph(clusters, opts, t),
            OrbShape::Image { .. } => self.gpu.render_frame_image(clusters, opts, t),
            OrbShape::Aquarelle(_) => self.gpu.render_frame_aquarelle(clusters, opts, t),
            _ => self.gpu.render_frame(clusters, opts, t),
        }
    }
}

/// GPU レンダラを初期化し、アダプタが取れなければ stderr に出して `Err(ExitCode)` を返す。
/// CLI は GPU を唯一のレンダラとするため、CPU フォールバックはしない (#225)。
fn init_renderer() -> Result<FrameRenderer, ExitCode> {
    FrameRenderer::new().ok_or_else(|| {
        eprintln!(
            "orber: no GPU adapter available; orber renders only on the GPU (#225). \
             Install a working GPU driver / Vulkan ICD and retry."
        );
        ExitCode::from(1)
    })
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
/// `--shape glyph` で指定された `--glyph-char` が同梱フォント (Noto Sans Symbols 2)
/// に収録されていなければ stderr で警告する。出力は走るが Glyph 描画は静かに
/// スキップされるため、「絵文字を入れたら何も出ない」という挙動の理由が
/// CLI 利用者には分からない。起動直後に 1 回だけ出すことで原因を明示する。
fn warn_if_glyph_char_unsupported(cli: &Cli) {
    if !matches!(cli.shape, Shape::Glyph) {
        return;
    }
    let ch = cli.glyph_char;
    if !has_glyph(GlyphFontId::NotoSymbols2, ch) {
        eprintln!(
            "orber: warning: '{}' (U+{:04X}) は同梱フォントに収録されていません。Glyph 描画はスキップされます。",
            ch, ch as u32
        );
    }
}

fn warn_if_aquarelle_count_ignored(cli: &Cli) {
    if matches!(cli.shape, Shape::Aquarelle) {
        eprintln!(
            "orber: warning: aquarelle shape ignores --count (rendering one orb per k-means cluster from the palette)"
        );
    }
}

/// `cli.orb_shape()` を解決し、エラー（`--shape image` の mask 欠如 / 読込失敗 /
/// コントラスト無し等）なら stderr に出して `Err(ExitCode)` を返す。各 render 経路の
/// 先頭で 1 度だけ呼び、Image の I/O デコードを多重に走らせない。panic はしない。
fn resolve_orb_shape(cli: &Cli) -> Result<OrbShape, ExitCode> {
    cli.orb_shape().map_err(|msg| {
        eprintln!("orber: {msg}");
        ExitCode::from(2)
    })
}

fn render_style_path(cli: &Cli, output: &Path, mode: OutputMode) -> ExitCode {
    // SVG / CSS 出力は静的な radial-gradient のみで orb 形状を持たないため、
    // `--shape`（と `--shape image` の `--image-mask`）は無視される。黙って円に
    // 落ちると驚くので、circle 以外を指定したら警告する（PNG / video 出力では効く）。
    if cli.shape != Shape::Circle {
        eprintln!(
            "orber: warning: --shape {:?} is ignored for {:?} output (SVG/CSS render circles only); use a PNG or video output to apply the orb shape",
            cli.shape, mode
        );
    }

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
        softness: cli.resolved_softness(),
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

    // shape を 1 度だけ解決（--shape image の mask デコードはここで 1 回）。
    let orb_shape = match resolve_orb_shape(cli) {
        Ok(s) => s,
        Err(code) => return code,
    };

    // 4. ビデオオプション構築。解像度は固定。
    let (direction, speed) = resolve_motion(cli);
    let opts = VideoOptions {
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        direction,
        speed,
        seed: cli.seed,
        count: Some(cli.resolved_count()),
        background,
        shape: orb_shape,
        softness: cli.resolved_softness(),
        color_tracks: None,
        keyframe_tracks: None,
    };

    // 5. 動画書き出し。進捗とフレーム数の検証は render_video が担当する。
    //    #225: フレームは GPU で描く。renderer は 1 本だけ作ってフレーム間で使い回す。
    let renderer = match init_renderer() {
        Ok(r) => r,
        Err(code) => return code,
    };
    if let Err(e) = render_video(
        &renderer,
        &orb_clusters,
        &opts,
        output,
        cli.duration_ms,
        codec,
    ) {
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

    // shape を 1 度だけ解決（--shape image の mask デコードはここで 1 回）。
    let orb_shape = match resolve_orb_shape(cli) {
        Ok(s) => s,
        Err(code) => return code,
    };

    // 4. PNG は「コンベアベルトの一瞬」（t=0）として GPU レンダラの 1 フレーム描画
    //    （`renderer.render(.., t=0.0)`）で出力する。AnimateOptions に `count` を
    //    渡すので --count による orb 数の展開が単発 PNG でも効く（pack_render_data_for_webgl
    //    が count を解釈する）。
    let (direction, speed) = resolve_motion(cli);
    let frame_opts = orber_core::animate::AnimateOptions {
        width: RenderOptions::default().width,
        height: RenderOptions::default().height,
        orb_size: cli.orb_size,
        blur: cli.blur,
        saturation: cli.saturation,
        direction,
        speed,
        seed: cli.seed,
        count: Some(cli.resolved_count()),
        background,
        shape: orb_shape.clone(),
        softness: cli.resolved_softness(),
        glyph_rotate: true,
        color_tracks: None,
        keyframe_tracks: None,
    };
    // #225: GPU を唯一のレンダラとして 1 枚描く。全 shape（Circle/Glyph/Aquarelle/
    // Image）に対応（count は 1024 まで data-texture 経路で GPU が直接描く）。
    let renderer = match init_renderer() {
        Ok(r) => r,
        Err(code) => return code,
    };
    let out = renderer.render(&orb_clusters, &frame_opts, 0.0);

    // 4. 保存。
    if let Err(e) = out.save(output) {
        eprintln!("orber: failed to write output {}: {e}", output.display());
        return ExitCode::from(2);
    }
    eprintln!("orber: wrote {}", output.display());
    ExitCode::SUCCESS
}

/// 動画入力（#7）に使うサンプル数。
///
/// 色変化を「ある程度滑らかに乗せる」目的で 20 枚に固定。
/// per-sample に ffmpeg を 1 回ずつ起動するので、増やすと前処理時間が線形に伸びる。
const VIDEO_INPUT_N_SAMPLES: usize = 20;

/// 動画入力（#33）の最低キーフレーム数。
///
/// 1 枚では補間できないので、`--keyframes` の値が 2 未満の場合は 2 にクランプする。
const MIN_KEYFRAMES: u32 = 2;

/// 動画入力（#33）の k-means cluster 数。color_track 経路と揃える（k=6）。
const VIDEO_INPUT_KEYFRAME_K: usize = 6;

/// 動画入力経路の dispatch。`--input-mode` で #7 / #33 を切り替える。
fn run_video_input(cli: &Cli, output: &Path, mode: OutputMode) -> ExitCode {
    match cli.input_mode {
        CliInputMode::ColorTrack => run_video_input_color_track(cli, output, mode),
        CliInputMode::Keyframe => run_video_input_keyframe(cli, output, mode),
    }
}

/// 動画入力（#7）経路。
///
/// - `ffprobe` / `ffmpeg` でフレーム N 枚をサンプリング
/// - 先頭フレーム k=6 で位置・重み・テンプレート色を決め、各サンプルとの LAB マッチングで color tracks を作る
/// - orb の位置 / 個数は時間で動かない（先頭フレームの k-means 結果で固定）
/// - 出力時刻 t ∈ [0, 1] は出力動画の t（duration_ms で決まる）にマップされ、
///   `interpolate_color_track` が「入力動画時刻」ごとの色を引いてくる
///
/// 出力長は `--duration-ms` で独立に決まる（入力動画の長さは色サンプル列の
/// サイズだけに影響する）。
fn run_video_input_color_track(cli: &Cli, output: &Path, mode: OutputMode) -> ExitCode {
    eprintln!(
        "orber: sampling {} frames from {}...",
        VIDEO_INPUT_N_SAMPLES,
        cli.input.display()
    );
    let samples = match sample_video_frames(&cli.input, VIDEO_INPUT_N_SAMPLES) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("orber: video input failed: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!("orber: extracted {} sample frame(s)", samples.len());

    // k=6 で先頭フレームを基準にしてテンプレートクラスタとトラックを構築。
    let color_tracks_data = match build_color_tracks(&samples, 6) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("orber: color track build failed: {e}");
            return ExitCode::from(2);
        }
    };

    // 既存の静止画経路と挙動を揃えるため、derive_background + drop_dominant を適用。
    // 同じ index フィルタを tracks にも適用しないと、cluster_idx と tracks の対応が
    // 崩れるので、`drop_dominant` 相当を template_clusters と tracks に同時に効かせる。
    let template_clusters = color_tracks_data.template_clusters.clone();
    let dominant_idx = template_clusters
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.weight.total_cmp(&b.weight))
        .map(|(i, _)| i);
    let background = orber_core::cluster::derive_background_rgba(&template_clusters);

    let (orb_clusters, orb_tracks): (Vec<_>, Vec<_>) = template_clusters
        .iter()
        .zip(color_tracks_data.tracks.iter())
        .enumerate()
        .filter(|(i, _)| Some(*i) != dominant_idx)
        .map(|(_, (c, t))| (*c, t.clone()))
        .unzip();
    warn_if_orb_pool_empty(&orb_clusters);
    warn_if_aquarelle_count_ignored(cli);

    // shape を 1 度だけ解決（--shape image の mask デコードはここで 1 回）。
    let orb_shape = match resolve_orb_shape(cli) {
        Ok(s) => s,
        Err(code) => return code,
    };

    // #225: PNG / 動画どちらの経路も GPU で描く。renderer は 1 本だけ作る。
    let renderer = match init_renderer() {
        Ok(r) => r,
        Err(code) => return code,
    };

    // 出力モードで分岐。動画 (mp4 / webm)、静止画 (png)、その他はエラー。
    if let Some(codec) = VideoCodec::from_output_mode(mode) {
        let (direction, speed) = resolve_motion(cli);
        let opts = VideoOptions {
            orb_size: cli.orb_size,
            blur: cli.blur,
            saturation: cli.saturation,
            direction,
            speed,
            seed: cli.seed,
            count: Some(cli.resolved_count()),
            background,
            shape: orb_shape,
            softness: cli.resolved_softness(),
            color_tracks: Some(orb_tracks),
            keyframe_tracks: None,
        };
        if let Err(e) = render_video(
            &renderer,
            &orb_clusters,
            &opts,
            output,
            cli.duration_ms,
            codec,
        ) {
            eprintln!("orber: video render failed: {e}");
            return ExitCode::from(2);
        }
        eprintln!("orber: wrote {}", output.display());
        return ExitCode::SUCCESS;
    }

    match mode {
        OutputMode::Png => {
            // PNG は t=0 の 1 枚（=入力動画の先頭フレームの色）。
            let (direction, speed) = resolve_motion(cli);
            let frame_opts = orber_core::animate::AnimateOptions {
                width: RenderOptions::default().width,
                height: RenderOptions::default().height,
                orb_size: cli.orb_size,
                blur: cli.blur,
                saturation: cli.saturation,
                direction,
                speed,
                seed: cli.seed,
                count: Some(cli.resolved_count()),
                background,
                shape: orb_shape,
                softness: cli.resolved_softness(),
                glyph_rotate: true,
                color_tracks: Some(orb_tracks),
                keyframe_tracks: None,
            };
            let img = renderer.render(&orb_clusters, &frame_opts, 0.0);
            if let Err(e) = img.save(output) {
                eprintln!("orber: failed to write output {}: {e}", output.display());
                return ExitCode::from(2);
            }
            eprintln!("orber: wrote {}", output.display());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "orber: video input + output mode {mode:?} is not supported (use mp4 / webm / png)"
            );
            ExitCode::from(1)
        }
    }
}

/// 動画入力（#33）経路。色 + 位置 + 重みをキーフレーム間で時間軸補間する。
///
/// - `ffprobe` / `ffmpeg` で N キーフレームを均等区間でサンプリング
/// - 各キーで k-means → LAB ΔE76 greedy マッチングで cluster を時間軸追跡
/// - 先頭キーを「テンプレート」として orb プールを構築（dominant 色は drop）
/// - render 時に `keyframe_tracks` 経由で各 cluster の color / centroid / weight を t で補間
///
/// 出力長は `--duration-ms` で独立に決まる（入力動画の長さは N 枚抽出の時刻計算に
/// だけ影響する）。
fn run_video_input_keyframe(cli: &Cli, output: &Path, mode: OutputMode) -> ExitCode {
    // #33 review N3: --keyframes 1 は補間に最低 2 枚必要なので無言で 2 にクランプ
    // すると挙動と CLI 値が乖離する。ユーザーに気付けるよう warning を出してから clamp。
    if cli.keyframes < MIN_KEYFRAMES {
        eprintln!(
            "orber: warning: --keyframes {} is too few, clamping to {} (need at least 2 to interpolate)",
            cli.keyframes, MIN_KEYFRAMES
        );
    }
    let n_keys = cli.keyframes.max(MIN_KEYFRAMES) as usize;
    eprintln!(
        "orber: sampling {} keyframe(s) from {}...",
        n_keys,
        cli.input.display()
    );
    let samples = match sample_video_frames(&cli.input, n_keys) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("orber: video input failed: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!("orber: extracted {} keyframe(s)", samples.len());

    // k=6 で先頭キーフレームを基準にしてテンプレートクラスタとキー列を構築。
    let kf_data = match build_keyframe_tracks(&samples, VIDEO_INPUT_KEYFRAME_K) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("orber: keyframe track build failed: {e}");
            return ExitCode::from(2);
        }
    };

    // color_track 経路と同じく derive_background + drop_dominant を template に適用。
    // tracks も同じ index で間引かないと cluster_idx と tracks の対応が崩れる。
    let template_clusters = kf_data.template_clusters.clone();
    let dominant_idx = template_clusters
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.weight.total_cmp(&b.weight))
        .map(|(i, _)| i);
    let background = orber_core::cluster::derive_background_rgba(&template_clusters);

    let (orb_clusters, orb_kf_tracks): (Vec<_>, Vec<_>) = template_clusters
        .iter()
        .zip(kf_data.tracks.iter())
        .enumerate()
        .filter(|(i, _)| Some(*i) != dominant_idx)
        .map(|(_, (c, t))| (*c, t.clone()))
        .unzip();
    warn_if_orb_pool_empty(&orb_clusters);
    warn_if_aquarelle_count_ignored(cli);

    // shape を 1 度だけ解決（--shape image の mask デコードはここで 1 回）。
    let orb_shape = match resolve_orb_shape(cli) {
        Ok(s) => s,
        Err(code) => return code,
    };

    // #225: PNG / 動画どちらの経路も GPU で描く。renderer は 1 本だけ作る。
    let renderer = match init_renderer() {
        Ok(r) => r,
        Err(code) => return code,
    };

    // 出力モードで分岐。動画 (mp4 / webm)、静止画 (png)、その他はエラー。
    if let Some(codec) = VideoCodec::from_output_mode(mode) {
        let (direction, speed) = resolve_motion(cli);
        let opts = VideoOptions {
            orb_size: cli.orb_size,
            blur: cli.blur,
            saturation: cli.saturation,
            direction,
            speed,
            seed: cli.seed,
            count: Some(cli.resolved_count()),
            background,
            shape: orb_shape,
            softness: cli.resolved_softness(),
            color_tracks: None,
            keyframe_tracks: Some(orb_kf_tracks),
        };
        if let Err(e) = render_video(
            &renderer,
            &orb_clusters,
            &opts,
            output,
            cli.duration_ms,
            codec,
        ) {
            eprintln!("orber: video render failed: {e}");
            return ExitCode::from(2);
        }
        eprintln!("orber: wrote {}", output.display());
        return ExitCode::SUCCESS;
    }

    match mode {
        OutputMode::Png => {
            // PNG は t=0 の 1 枚。`interpolate_keyframe_track` の端点 clamp 仕様で
            // 入力動画の先頭キーフレームの色 + 位置 + 重みになる。
            let (direction, speed) = resolve_motion(cli);
            let frame_opts = orber_core::animate::AnimateOptions {
                width: RenderOptions::default().width,
                height: RenderOptions::default().height,
                orb_size: cli.orb_size,
                blur: cli.blur,
                saturation: cli.saturation,
                direction,
                speed,
                seed: cli.seed,
                count: Some(cli.resolved_count()),
                background,
                shape: orb_shape,
                softness: cli.resolved_softness(),
                glyph_rotate: true,
                color_tracks: None,
                keyframe_tracks: Some(orb_kf_tracks),
            };
            let img = renderer.render(&orb_clusters, &frame_opts, 0.0);
            if let Err(e) = img.save(output) {
                eprintln!("orber: failed to write output {}: {e}", output.display());
                return ExitCode::from(2);
            }
            eprintln!("orber: wrote {}", output.display());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "orber: video input + output mode {mode:?} is not supported (use mp4 / webm / png)"
            );
            ExitCode::from(1)
        }
    }
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

    // shape を 1 度だけ解決（--shape image の mask デコードはここで 1 回）。各 spec
    // で同じ shape を使い回すので、ループ前に 1 回だけ作る。
    let orb_shape = match resolve_orb_shape(cli) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let softness = cli.resolved_softness();
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

    // #225: 全 spec を GPU で描く。renderer は 1 本だけ作ってループで使い回す。
    let renderer = match init_renderer() {
        Ok(r) => r,
        Err(code) => return code,
    };

    for (i, spec) in specs.iter().enumerate() {
        let idx = i + 1;
        let filename = format!("{idx:02}_{}.{}", spec.label, spec.kind.ext());
        let out_path = dir.join(&filename);
        eprintln!("orber: variation {idx}/{total} ({filename})");

        let result = render_one_variation(
            &renderer,
            &orb_clusters,
            spec,
            &out_path,
            background,
            &orb_shape,
            softness,
        );
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
    renderer: &FrameRenderer,
    clusters: &[Cluster],
    spec: &VariationSpec,
    out_path: &std::path::Path,
    bg_rgba: [u8; 4],
    orb_shape: &OrbShape,
    softness: SoftnessPreset,
) -> Result<(), String> {
    // saturation は preset で揺らさない（同一画像から作る複数バリエーションでは
    // 入力色をそのまま使う方針）。CLI の単発経路と揃えるため 1.0 固定。
    match spec.kind {
        VariationKind::Png => {
            // 静止画は「コンベアベルトの一瞬」。t=0 のフレームを 1 枚だけレンダリングする。
            // phase 由来で orb が画面全体に散らばり、画面端で半分欠ける構図が自然に出る。
            let frame_opts = orber_core::animate::AnimateOptions {
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
                shape: orb_shape.clone(),
                softness,
                glyph_rotate: true,
                color_tracks: None,
                keyframe_tracks: None,
            };
            let img = renderer.render(clusters, &frame_opts, 0.0);
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
                shape: orb_shape.clone(),
                softness,
                color_tracks: None,
                keyframe_tracks: None,
            };
            render_video(
                renderer,
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
    use crate::video::MAX_DURATION_MS;
    use orber_core::animate::AnimateOptions;

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
        assert!(try_parse(&["--count", "1025"]).is_err());
        assert!(try_parse(&["--count", "abc"]).is_err());
        assert!(try_parse(&["--count", "1"]).is_ok());
        assert!(try_parse(&["--count", "20"]).is_ok());
        assert!(try_parse(&["--count", "1024"]).is_ok());
    }

    #[test]
    fn count_default_is_twenty() {
        let cli = Cli::parse_from(["orber", "--input", "x", "--output", "x.png"]);
        assert_eq!(cli.count, 20);
    }

    #[test]
    fn shape_glyph_with_default_char() {
        // --shape glyph で --glyph-char 省略時は ☆ (U+2606) になる。
        let cli = Cli::try_parse_from([
            "orber", "--input", "x", "--output", "x.png", "--shape", "glyph",
        ])
        .expect("--shape glyph should parse");
        assert_eq!(cli.glyph_char, '☆');
        match cli.orb_shape() {
            Ok(OrbShape::Glyph { ch, .. }) => assert_eq!(ch, '☆'),
            other => panic!("expected Ok(OrbShape::Glyph), got {other:?}"),
        }
    }

    #[test]
    fn shape_image_without_mask_errors_not_panics() {
        // #217: --shape image で --image-mask 無し → orb_shape() が Err（panic しない）。
        let cli = Cli::try_parse_from([
            "orber", "--input", "x", "--output", "x.png", "--shape", "image",
        ])
        .expect("--shape image should parse (clap level)");
        let result = cli.orb_shape();
        assert!(
            result.is_err(),
            "--shape image without --image-mask must be an explicit Err, got {result:?}"
        );
    }

    #[test]
    fn shape_image_with_unreadable_mask_errors_not_panics() {
        // #217: --image-mask が存在しないファイル → Err（panic しない）。
        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--shape",
            "image",
            "--image-mask",
            "/nonexistent/definitely/not/here.png",
        ])
        .expect("clap should parse");
        assert!(
            cli.orb_shape().is_err(),
            "unreadable --image-mask must be an explicit Err"
        );
    }

    #[test]
    fn shape_image_with_valid_mask_resolves_to_image() {
        // #217: 有効なシルエット PNG を書き出して --image-mask に渡すと
        // OrbShape::Image に解決される（size は DEFAULT_GLYPH_SDF_SIZE = 256）。
        let dir = tempfile::TempDir::new().expect("tempdir");
        let mask_path = dir.path().join("silhouette.png");
        // 透明背景に中央不透明ブロックの 64x64 PNG を作る。
        let mut img = image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 0]));
        for y in 18..46 {
            for x in 18..46 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        img.save(&mask_path).expect("write mask png");

        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--shape",
            "image",
            "--image-mask",
            mask_path.to_str().unwrap(),
        ])
        .expect("clap should parse");
        match cli.orb_shape() {
            Ok(OrbShape::Image { size, sdf }) => {
                assert_eq!(size, orber_core::glyph::DEFAULT_GLYPH_SDF_SIZE);
                assert_eq!(sdf.len(), (size as usize) * (size as usize));
            }
            other => panic!("expected Ok(OrbShape::Image), got {other:?}"),
        }
    }

    #[test]
    fn shape_image_with_no_contrast_mask_errors() {
        // #217: フラットな単色 PNG → コントラスト無しで Err（panic しない）。
        let dir = tempfile::TempDir::new().expect("tempdir");
        let mask_path = dir.path().join("flat.png");
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([100, 100, 100, 255]));
        img.save(&mask_path).expect("write flat png");
        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--shape",
            "image",
            "--image-mask",
            mask_path.to_str().unwrap(),
        ])
        .expect("clap should parse");
        assert!(
            cli.orb_shape().is_err(),
            "flat single-color mask has no contrast → Err"
        );
    }

    #[test]
    fn shape_image_with_svg_or_undecodable_mask_errors() {
        // #217: 実在するがラスタデコード不可なファイル（SVG テキスト等の bytes）を
        // --image-mask に渡すと、image::open が失敗して orb_shape() が Err（panic しない）。
        // 「存在しないパス」(shape_image_with_unreadable_mask_errors_not_panics) とは別の
        // 経路: ファイルは読めるがデコードに失敗するケース。SVG は web 専用で raster
        // デコーダに無い、という CLI の制約も同時に守る。
        let dir = tempfile::TempDir::new().expect("tempdir");
        let svg_path = dir.path().join("silhouette.svg");
        std::fs::write(
            &svg_path,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">
  <rect x="16" y="16" width="32" height="32" fill="black"/>
</svg>"#,
        )
        .expect("write svg");

        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--shape",
            "image",
            "--image-mask",
            svg_path.to_str().unwrap(),
        ])
        .expect("clap should parse");
        let result = cli.orb_shape();
        assert!(
            result.is_err(),
            "an existing but undecodable mask (SVG text) must be an explicit Err (decode failure path), got {result:?}"
        );
    }

    #[test]
    fn glyph_char_rejects_multi_char() {
        // --glyph-char には 1 文字しか入れられない。
        assert!(try_parse(&["--glyph-char", "abc"]).is_err());
        assert!(try_parse(&["--glyph-char", ""]).is_err());
        assert!(try_parse(&["--glyph-char", "a"]).is_ok());
        assert!(try_parse(&["--glyph-char", "♪"]).is_ok());
    }

    #[test]
    fn count_preset_resolves_to_table() {
        let cli = Cli::parse_from(["orber", "--input", "x", "--output", "x.png"]);
        // デフォルトは数値 20。
        assert_eq!(cli.resolved_count(), 20);

        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--count-preset",
            "low",
        ])
        .unwrap();
        assert_eq!(cli.resolved_count(), 10);

        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--count-preset",
            "mid",
        ])
        .unwrap();
        assert_eq!(
            cli.resolved_count(),
            20,
            "--count-preset mid must map to 20"
        );

        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--count-preset",
            "high",
        ])
        .unwrap();
        assert_eq!(cli.resolved_count(), 30);
    }

    #[test]
    fn count_and_count_preset_are_mutually_exclusive() {
        // clap の conflicts_with で --count と --count-preset の同時指定は弾かれる。
        let res = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--count",
            "30",
            "--count-preset",
            "low",
        ]);
        assert!(res.is_err(), "--count + --count-preset must be rejected");
    }

    #[test]
    fn speed_mid_and_fast_parse() {
        let cli = Cli::try_parse_from([
            "orber", "--input", "x", "--output", "x.png", "--speed", "mid",
        ])
        .unwrap();
        assert!(matches!(cli.speed, CliSpeed::Mid));
        assert_eq!(MotionSpeed::from(cli.speed), MotionSpeed::Mid);

        let cli = Cli::try_parse_from([
            "orber", "--input", "x", "--output", "x.png", "--speed", "fast",
        ])
        .unwrap();
        assert!(matches!(cli.speed, CliSpeed::Fast));
        assert_eq!(MotionSpeed::from(cli.speed), MotionSpeed::Fast);
    }

    #[test]
    fn softness_default_is_mid() {
        let cli = Cli::parse_from(["orber", "--input", "x", "--output", "x.png"]);
        assert!(matches!(cli.softness, CliSoftness::Mid));
        assert_eq!(cli.resolved_softness(), SoftnessPreset::Mid);
    }

    #[test]
    fn softness_low_high_parse() {
        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--softness",
            "low",
        ])
        .unwrap();
        assert_eq!(cli.resolved_softness(), SoftnessPreset::Low);

        let cli = Cli::try_parse_from([
            "orber",
            "--input",
            "x",
            "--output",
            "x.png",
            "--softness",
            "high",
        ])
        .unwrap();
        assert_eq!(cli.resolved_softness(), SoftnessPreset::High);
    }

    #[test]
    fn aquarelle_params_out_of_range_rejected() {
        assert!(try_parse(&["--aquarelle-bleed", "1.5"]).is_err());
        assert!(try_parse(&["--aquarelle-bloom", "-0.1"]).is_err());
        assert!(try_parse(&["--aquarelle-offset", "0.7"]).is_ok());
        assert!(try_parse(&["--aquarelle-halo", "0.0"]).is_ok());
    }

    /// #212 (#11) / #225: `FrameRenderer::render()` must dispatch by `opts.shape` —
    /// Glyph opts go to `render_frame_glyph`, Circle opts to `render_frame`. This
    /// pins the `render()` match arm (main.rs) that picks the GPU sub-path: the two
    /// sub-paths produce visibly different images for the same parameters (a star
    /// SDF fill vs round orbs), so the dispatch is observable.
    ///
    /// GPU-required-aware: with `ORBER_REQUIRE_GPU=1` a missing adapter is a hard
    /// failure; otherwise it SKIPs. The CLI always builds with the GPU renderer
    /// (#225), so this test is no longer feature-gated.
    #[test]
    fn frame_renderer_render_dispatches_glyph_vs_circle() {
        use orber_core::cluster::{Centroid, Cluster};
        use orber_core::gpu::GpuRenderer;

        let what = "frame_renderer_render_dispatches_glyph_vs_circle";
        let Some(gpu) = GpuRenderer::new() else {
            if std::env::var("ORBER_REQUIRE_GPU").as_deref() == Ok("1") {
                panic!("{what}: ORBER_REQUIRE_GPU=1 but no GPU adapter available");
            }
            eprintln!("SKIP {what}: no GPU adapter available");
            return;
        };
        eprintln!("{what} running on adapter: {}", gpu.adapter_name());

        let clusters = vec![
            Cluster {
                color: [220, 60, 60],
                centroid: Centroid { x: 0.3, y: 0.4 },
                weight: 0.5,
            },
            Cluster {
                color: [60, 120, 220],
                centroid: Centroid { x: 0.7, y: 0.6 },
                weight: 0.3,
            },
            Cluster {
                color: [200, 200, 80],
                centroid: Centroid { x: 0.5, y: 0.2 },
                weight: 0.2,
            },
        ];

        // Shared parameters; only `shape` differs between the two cases.
        let base = AnimateOptions {
            width: 96,
            height: 72,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            direction: MotionDirection::LeftToRight,
            speed: MotionSpeed::Slow,
            seed: 7,
            count: Some(6),
            background: [10, 12, 20, 255],
            shape: OrbShape::Circle,
            softness: SoftnessPreset::Mid,
            glyph_rotate: true,
            color_tracks: None,
            keyframe_tracks: None,
        };

        let glyph_opts = AnimateOptions {
            shape: OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2,
            },
            ..base.clone()
        };

        // Reference sub-path outputs computed directly off the GpuRenderer.
        let glyph_ref = gpu.render_frame_glyph(&clusters, &glyph_opts, 0.0);
        let circle_ref_for_glyph_params = gpu.render_frame(&clusters, &glyph_opts, 0.0);
        let circle_ref = gpu.render_frame(&clusters, &base, 0.0);

        // Sanity: the two GPU sub-paths really differ for these glyph params, so the
        // dispatch assertion below is meaningful (not trivially satisfied).
        assert_ne!(
            glyph_ref, circle_ref_for_glyph_params,
            "glyph and circle GPU sub-paths must differ for the same params \
             (otherwise the dispatch test proves nothing)"
        );

        let renderer = FrameRenderer { gpu: Box::new(gpu) };

        // Glyph opts → render() must equal the glyph sub-path, not the circle one.
        let glyph_dispatched = renderer.render(&clusters, &glyph_opts, 0.0);
        assert_eq!(
            glyph_dispatched, glyph_ref,
            "FrameRenderer::render must dispatch Glyph opts to render_frame_glyph"
        );
        assert_ne!(
            glyph_dispatched, circle_ref_for_glyph_params,
            "FrameRenderer::render must NOT send Glyph opts to the circle render_frame"
        );

        // Circle opts → render() must equal the circle sub-path.
        let circle_dispatched = renderer.render(&clusters, &base, 0.0);
        assert_eq!(
            circle_dispatched, circle_ref,
            "FrameRenderer::render must dispatch Circle opts to render_frame"
        );
        eprintln!("{what}: glyph→glyph, circle→circle dispatch confirmed");
    }
}
