//! 縦長動画出力モジュール。
//!
//! クラスタ列とアニメーションオプションを受け取り、一時ディレクトリに
//! 連番 PNG を書き出してから `ffmpeg` を子プロセス起動して mp4 / webm に
//! まとめる。
//!
//! # 設計メモ
//!
//! - 解像度は 1080x1920、fps は 30 で固定（`VIDEO_WIDTH` / `VIDEO_HEIGHT`
//!   / `VIDEO_FPS`）。`AnimateOptions` の width/height は無視され、必ず
//!   ビデオ用の値で上書きされる。
//! - フレーム時刻は `t = i / total` （i ∈ 0..total）で計算し、`t = 1.0`
//!   は含めない。`render_frame` が `t=0` と `t=1` で同一フレームになる
//!   ループ性を持つので、両端を含めるとループ繋ぎ目で 1 フレーム重複する。
//! - ffmpeg が PATH に無い場合は [`VideoError::FfmpegNotFound`] を返す。
//!   ユーザー側でインストール案内を出す前提。

use crate::animate::{render_frame, AnimateOptions};
use crate::cluster::Cluster;
use crate::output_mode::OutputMode;
use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// 動画の幅（縦長 9:16）。
pub const VIDEO_WIDTH: u32 = 1080;
/// 動画の高さ（縦長 9:16）。
pub const VIDEO_HEIGHT: u32 = 1920;
/// 動画のフレームレート（fps）。
pub const VIDEO_FPS: u32 = 30;

/// 動画コーデック。
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

/// `render_video` のエラー。
#[derive(Debug)]
pub enum VideoError {
    /// ffmpeg バイナリが見つからない。
    FfmpegNotFound,
    /// ffmpeg が非ゼロ終了した。
    FfmpegFailed {
        status: ExitStatus,
        stderr: String,
    },
    /// I/O エラー（テンポラリディレクトリ作成失敗、PNG 書き出し失敗等）。
    Io(io::Error),
    /// duration_ms = 0 等で有効なフレーム数が算出できない。
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
            Self::InvalidDuration => write!(f, "duration_ms must be > 0"),
        }
    }
}

impl std::error::Error for VideoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
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
/// 1 フレーム未満になる極端に短い長さでも 1 を返す（duration_ms > 0 の場合）。
/// duration_ms = 0 のときは [`VideoError::InvalidDuration`]。
pub(crate) fn calc_frame_count(duration_ms: u64) -> Result<usize, VideoError> {
    if duration_ms == 0 {
        return Err(VideoError::InvalidDuration);
    }
    let n = (duration_ms * VIDEO_FPS as u64) / 1000;
    Ok((n.max(1)) as usize)
}

/// 連番 PNG を一時ディレクトリに書き出して ffmpeg で動画に結合する。
///
/// `opts.width` / `opts.height` は [`VIDEO_WIDTH`] / [`VIDEO_HEIGHT`] で
/// 上書きされたコピーを使う（呼び出し側 `opts` は変更されない）。
///
/// フレーム時刻は `t = i / total` （i ∈ 0..total）。ループ繋ぎ目で
/// フレームが重複しないよう、`t = 1.0` は含めない。
pub fn render_video(
    clusters: &[Cluster],
    opts: &AnimateOptions,
    output: &Path,
    duration_ms: u64,
    codec: VideoCodec,
) -> Result<(), VideoError> {
    let total = calc_frame_count(duration_ms)?;

    // ビデオ用に解像度を強制上書きしたコピーを作る。
    let mut video_opts = opts.clone();
    video_opts.width = VIDEO_WIDTH;
    video_opts.height = VIDEO_HEIGHT;

    let temp_dir = tempfile::TempDir::new()?;

    // フレーム書き出し（逐次）。
    for i in 0..total {
        let t = i as f32 / total as f32;
        let frame = render_frame(clusters, &video_opts, t);
        let path = temp_dir.path().join(format!("frame_{:05}.png", i));
        frame.save(&path).map_err(|e| {
            // image::ImageError -> io::Error の自然な変換は無いので Other で包む。
            VideoError::Io(io::Error::new(io::ErrorKind::Other, e.to_string()))
        })?;
    }

    // ffmpeg コマンド組み立て。
    let pattern = temp_dir.path().join("frame_%05d.png");
    let fps_str = VIDEO_FPS.to_string();

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
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
                "-r",
            ])
            .arg(&fps_str);
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
                "-r",
            ])
            .arg(&fps_str);
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
        // duration_ms = 33 -> (33*30)/1000 = 0.99 -> max(1) -> 1
        assert_eq!(calc_frame_count(33).unwrap(), 1);
        // duration_ms = 0 -> InvalidDuration
        match calc_frame_count(0) {
            Err(VideoError::InvalidDuration) => {}
            other => panic!("expected InvalidDuration, got {other:?}"),
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
}
