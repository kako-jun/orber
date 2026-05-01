//! 縦長動画出力モジュール。
//!
//! クラスタ列とビデオ用オプションを受け取り、一時ディレクトリに連番 PNG を
//! 書き出してから `ffmpeg` を子プロセス起動して mp4 / webm にまとめる。
//!
//! # 設計メモ
//!
//! - 解像度は 1080x1920、fps は 30 で固定（`VIDEO_WIDTH` / `VIDEO_HEIGHT`
//!   / `VIDEO_FPS`）。動画モジュールでは解像度を CLI から受け付けず、
//!   常にビデオ用の値を使う（[`VideoOptions`] に width/height は無い）。
//! - フレーム時刻は `t = i / total` （i ∈ 0..total）で計算し、`t = 1.0`
//!   は含めない。`render_frame` が `t=0` と `t=1` で同一フレームになる
//!   ループ性を持つので、両端を含めるとループ繋ぎ目で 1 フレーム重複する。
//! - ffmpeg が PATH に無い場合は [`VideoError::FfmpegNotFound`] を返す。
//!   ユーザー側でインストール案内を出す前提。

use orber_core::animate::{
    precompute_orb_params, render_frame_with_params, AnimateOptions, MotionDirection, MotionSpeed,
};
use orber_core::cluster::Cluster;
use orber_core::orb::OrbShape;
use orber_core::output_mode::OutputMode;
use orber_core::style::ContrastPreset;
use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// 動画の幅（縦長 9:16）。
///
/// yuv420p は色差を 2x2 サブサンプルするため、幅・高さ共に 2 で割り切れる
/// 必要がある。`VIDEO_WIDTH` / `VIDEO_HEIGHT` を変更する際は両方とも偶数で
/// あることを維持すること。
pub const VIDEO_WIDTH: u32 = 1080;
/// 動画の高さ（縦長 9:16）。
///
/// yuv420p は色差を 2x2 サブサンプルするため、幅・高さ共に 2 で割り切れる
/// 必要がある。`VIDEO_WIDTH` / `VIDEO_HEIGHT` を変更する際は両方とも偶数で
/// あることを維持すること。
pub const VIDEO_HEIGHT: u32 = 1920;
/// 動画のフレームレート（fps）。
pub const VIDEO_FPS: u32 = 30;
/// duration_ms の最大値（10 分）。
///
/// これを超える長さは想定外として [`VideoError::InvalidDuration`] を返す。
/// 一時ディレクトリに連番 PNG を書き出す方式なので、長すぎると
/// ディスク容量も消費するため上限を設けている。
pub const MAX_DURATION_MS: u64 = 600_000;

/// 動画コーデック。
///
/// std の `IpAddr::V4` 等の TitleCase 慣習に揃えて variant を命名する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 (libx264)、mp4 コンテナ向け。
    H264,
    /// VP9 (libvpx-vp9)、webm コンテナ向け。
    Vp9,
}

impl VideoCodec {
    /// [`OutputMode`] から対応するコーデックを引く。
    ///
    /// 動画でないモード（Png/Webp/Svg/Css）は `None`。
    pub fn from_output_mode(mode: OutputMode) -> Option<Self> {
        match mode {
            OutputMode::Mp4 => Some(VideoCodec::H264),
            OutputMode::Webm => Some(VideoCodec::Vp9),
            _ => None,
        }
    }
}

/// 動画 1 本書き出しのオプション。
///
/// 解像度は [`VIDEO_WIDTH`] / [`VIDEO_HEIGHT`] で固定なので持たない。
/// ([`AnimateOptions`] の width/height を黙って捨てるのを避けるため、
/// ビデオ向けには専用構造体を切っている。)
#[derive(Debug, Clone)]
pub struct VideoOptions {
    pub orb_size: f32,
    pub blur: f32,
    pub saturation: f32,
    pub direction: MotionDirection,
    pub speed: MotionSpeed,
    pub seed: u64,
    /// 同時可視 orb 数。None なら cluster 数（後方互換）。
    pub count: Option<usize>,
    /// 背景 RGBA。動画は yuv420p 制約で alpha 不可なので呼び出し側で透過を弾くこと。
    pub background: [u8; 4],
    /// orb の描画形式。
    pub shape: OrbShape,
    /// コントラスト preset (#55)。Mid なら既存挙動と完全同値。
    pub contrast: ContrastPreset,
}

impl Default for VideoOptions {
    fn default() -> Self {
        let a = AnimateOptions::default();
        Self {
            orb_size: a.orb_size,
            blur: a.blur,
            saturation: a.saturation,
            direction: a.direction,
            speed: a.speed,
            seed: a.seed,
            count: a.count,
            background: a.background,
            shape: a.shape,
            contrast: a.contrast,
        }
    }
}

/// `render_video` のエラー。
#[derive(Debug)]
pub enum VideoError {
    /// ffmpeg バイナリが見つからない。
    FfmpegNotFound,
    /// ffmpeg が非ゼロ終了した。
    FfmpegFailed { status: ExitStatus, stderr: String },
    /// I/O エラー（テンポラリディレクトリ作成失敗、PNG 書き出し失敗等）。
    Io(io::Error),
    /// PNG エンコード失敗等、フレーム書き出し時の image クレート由来エラー。
    FrameSave(image::ImageError),
    /// duration_ms = 0、上限超過、オーバーフロー等で有効なフレーム数が算出できない。
    InvalidDuration,
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FfmpegNotFound => write!(
                f,
                "ffmpeg not found in PATH; install ffmpeg (e.g. apt install ffmpeg / brew install ffmpeg) and retry"
            ),
            Self::FfmpegFailed { status, stderr } => {
                write!(f, "ffmpeg failed with {status}: {stderr}")
            }
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::FrameSave(e) => write!(f, "failed to encode frame: {e}"),
            Self::InvalidDuration => write!(
                f,
                "duration_ms must be in 1000..={MAX_DURATION_MS} (1s..=10min)"
            ),
        }
    }
}

impl std::error::Error for VideoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::FrameSave(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for VideoError {
    fn from(e: io::Error) -> Self {
        VideoError::Io(e)
    }
}

/// `duration_ms` から書き出すフレーム数を計算する。
///
/// 妥当な範囲は `1000 <= duration_ms <= MAX_DURATION_MS`。
/// 1 秒未満では正味の動画にならないため [`VideoError::InvalidDuration`] を返す。
/// `duration_ms * VIDEO_FPS` がオーバーフローする極端な値も同じく `InvalidDuration`。
pub fn calc_frame_count(duration_ms: u64) -> Result<usize, VideoError> {
    if !(1000..=MAX_DURATION_MS).contains(&duration_ms) {
        return Err(VideoError::InvalidDuration);
    }
    let n = duration_ms
        .checked_mul(VIDEO_FPS as u64)
        .ok_or(VideoError::InvalidDuration)?
        / 1000;
    // 範囲チェックを通った時点で n >= 30 が保証されているため、ここで
    // n が 0 になることはない。usize 変換だけ行う。
    Ok(n as usize)
}

/// 連番 PNG を一時ディレクトリに書き出して ffmpeg で動画に結合する。
///
/// 解像度は常に [`VIDEO_WIDTH`] / [`VIDEO_HEIGHT`]。
/// フレーム時刻は `t = i / total` （i ∈ 0..total）。ループ繋ぎ目で
/// フレームが重複しないよう、`t = 1.0` は含めない。
///
/// 進捗は stderr に出力される（書き出し開始時、10% 毎、ffmpeg 起動時）。
/// CLI バイナリ向けの便宜であり、サイレントに動かしたい場合は呼び出し側で
/// stderr を捨てること。
pub fn render_video(
    clusters: &[Cluster],
    opts: &VideoOptions,
    output: &Path,
    duration_ms: u64,
    codec: VideoCodec,
) -> Result<(), VideoError> {
    let total = calc_frame_count(duration_ms)?;
    eprintln!("orber: rendering {total} frames at {VIDEO_FPS}fps...");

    // ビデオ用の AnimateOptions を組み立てる（解像度は固定）。
    let frame_opts = AnimateOptions {
        width: VIDEO_WIDTH,
        height: VIDEO_HEIGHT,
        orb_size: opts.orb_size,
        blur: opts.blur,
        saturation: opts.saturation,
        direction: opts.direction,
        speed: opts.speed,
        seed: opts.seed,
        count: opts.count,
        background: opts.background,
        shape: opts.shape,
        contrast: opts.contrast,
    };

    let temp_dir = tempfile::TempDir::new()?;

    // OrbParams は seed / count / clusters のみに依存し t は不変なので、ループ前に
    // 1 回だけ計算してフレーム間で使い回す。240 frame なら 240 回の Vec 割当 +
    // RNG 走行を 1 回に圧縮できる。
    let cache = precompute_orb_params(&frame_opts, clusters);

    // フレーム書き出し（逐次）。10% 刻みで stderr に進捗を出す。
    let progress_step = (total / 10).max(1);
    for i in 0..total {
        let t = i as f32 / total as f32;
        let frame = render_frame_with_params(clusters, &frame_opts, &cache, t);
        let path = temp_dir.path().join(format!("frame_{i:05}.png"));
        frame.save(&path).map_err(VideoError::FrameSave)?;
        if i > 0 && i % progress_step == 0 {
            let pct = (i * 100) / total;
            eprintln!("orber: {pct}% ({i}/{total} frames)");
        }
    }

    // ffmpeg コマンド組み立て。
    eprintln!("orber: invoking ffmpeg ({codec:?})...");
    let pattern = temp_dir.path().join("frame_%05d.png");
    let fps_str = VIDEO_FPS.to_string();

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-framerate")
        .arg(&fps_str)
        .arg("-i")
        .arg(&pattern);

    match codec {
        VideoCodec::H264 => {
            cmd.args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ]);
        }
        VideoCodec::Vp9 => {
            cmd.args([
                "-c:v",
                "libvpx-vp9",
                "-pix_fmt",
                "yuv420p",
                "-b:v",
                "0",
                "-crf",
                "32",
            ]);
        }
    }

    cmd.arg(output);

    let result = cmd.output();
    let out = match result {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(VideoError::FfmpegNotFound);
        }
        Err(e) => return Err(VideoError::Io(e)),
    };

    if !out.status.success() {
        return Err(VideoError::FfmpegFailed {
            status: out.status,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_math() {
        // duration_ms = 5000, fps=30 -> 150 frames
        assert_eq!(calc_frame_count(5000).unwrap(), 150);
        // duration_ms = 1000 -> 30 frames
        assert_eq!(calc_frame_count(1000).unwrap(), 30);
        // duration_ms = 999 -> 1 秒未満は InvalidDuration
        match calc_frame_count(999) {
            Err(VideoError::InvalidDuration) => {}
            other => panic!("expected InvalidDuration, got {other:?}"),
        }
        // duration_ms = 33 -> 1 秒未満は InvalidDuration
        match calc_frame_count(33) {
            Err(VideoError::InvalidDuration) => {}
            other => panic!("expected InvalidDuration, got {other:?}"),
        }
        // duration_ms = 0 -> InvalidDuration
        match calc_frame_count(0) {
            Err(VideoError::InvalidDuration) => {}
            other => panic!("expected InvalidDuration, got {other:?}"),
        }
    }

    #[test]
    fn frame_count_max_duration() {
        // 上限ちょうど: 600_000 ms * 30 fps / 1000 = 18,000 frames
        assert_eq!(calc_frame_count(MAX_DURATION_MS).unwrap(), 18_000);
        // 上限超過は InvalidDuration
        match calc_frame_count(MAX_DURATION_MS + 1) {
            Err(VideoError::InvalidDuration) => {}
            other => panic!("expected InvalidDuration for over-cap, got {other:?}"),
        }
    }

    #[test]
    fn frame_count_overflow_safe() {
        // u64::MAX を渡しても panic せず InvalidDuration になる。
        // （現状は範囲チェックで先に弾かれるが、checked_mul の安全網も担保。）
        match calc_frame_count(u64::MAX) {
            Err(VideoError::InvalidDuration) => {}
            other => panic!("expected InvalidDuration for u64::MAX, got {other:?}"),
        }
    }

    #[test]
    fn codec_from_output_mode() {
        assert_eq!(
            VideoCodec::from_output_mode(OutputMode::Mp4),
            Some(VideoCodec::H264)
        );
        assert_eq!(
            VideoCodec::from_output_mode(OutputMode::Webm),
            Some(VideoCodec::Vp9)
        );
        assert_eq!(VideoCodec::from_output_mode(OutputMode::Png), None);
        assert_eq!(VideoCodec::from_output_mode(OutputMode::Webp), None);
        assert_eq!(VideoCodec::from_output_mode(OutputMode::Svg), None);
        assert_eq!(VideoCodec::from_output_mode(OutputMode::Css), None);
    }

    #[test]
    fn video_error_display() {
        let msg = format!("{}", VideoError::FfmpegNotFound);
        assert!(
            msg.contains("ffmpeg"),
            "FfmpegNotFound display should mention ffmpeg: {msg}"
        );
        assert!(
            msg.contains("install"),
            "FfmpegNotFound display should mention install: {msg}"
        );
    }

    #[test]
    fn video_options_default_matches_animate() {
        // VideoOptions::default() は AnimateOptions::default() の対応フィールドと
        // 一致する（解像度を除く）。CLI default の SoT 統一が崩れないか守る。
        let v = VideoOptions::default();
        let a = AnimateOptions::default();
        assert_eq!(v.orb_size, a.orb_size);
        assert_eq!(v.blur, a.blur);
        assert_eq!(v.saturation, a.saturation);
        assert_eq!(v.direction, a.direction);
        assert_eq!(v.speed, a.speed);
        assert_eq!(v.seed, a.seed);
        assert_eq!(v.contrast, a.contrast);
    }
}
