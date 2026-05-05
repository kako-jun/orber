//! 動画入力（#7）モジュール。
//!
//! 入力動画から `ffprobe` で長さを取り、`ffmpeg` を per-sample で起動して
//! N 枚のフレームを均等区間でサンプリングする。各サンプルから k クラスタを
//! 抽出し、先頭フレームを「テンプレート」として LAB 距離マッチングで色を
//! 対応付け、cluster あたり N 個の色サンプル列（=色トラック）を作る。
//!
//! # 設計メモ
//!
//! - サンプルは `tempfile::TempDir` 配下に PNG として書き出してから
//!   `image::open` でデコードする。RAII で関数終了時に自動クリーンアップ。
//! - per-sample に `-ss T -frames:v 1` を 1 ループずつ呼ぶ（精度優先）。
//!   `select=...` 一発の方が速いが、サンプル時刻が動画の長さに対して
//!   厳密に均等になる per-sample 方式を採る。
//! - 動画長は `ffprobe -show_entries format=duration` で取る。`ffprobe` が
//!   PATH に無い場合は `FfprobeNotFound`、長さが 0 / NaN なら `ZeroDuration`。
//! - LAB 距離マッチングは greedy 最近傍。完全な assignment problem を解かない
//!   のは、6 クラスタ程度なら greedy で十分実用品質、コードも単純なため。
//!   （Hungarian を入れたい場合は v0 リリース後に検討。）

use image::RgbImage;
use orber_core::cluster::{extract_clusters, Cluster};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 1 枚のサンプルフレーム（動画の特定時刻から抜き出した画像）。
#[derive(Debug, Clone)]
pub struct VideoSample {
    /// ffmpeg が書き出した PNG をデコードした RGB バッファ。
    pub frame: RgbImage,
    /// `[0.0, 1.0]` の正規化時刻（動画頭からの相対位置）。
    /// サンプリング戦略により等間隔に並ぶ：`t_i = i / (N - 1)` （N == 1 なら 0.0）。
    pub t: f32,
}

/// 動画入力に関するエラー。
#[derive(Debug)]
pub enum VideoInputError {
    /// `ffmpeg` バイナリが見つからない。
    FfmpegNotFound,
    /// `ffprobe` バイナリが見つからない。
    FfprobeNotFound,
    /// ffprobe が動画長を取得できなかった（破損ファイル等）。
    DurationProbeFailed { stderr: String },
    /// 動画長が 0 / NaN / 負（破損または音声のみのファイル）。
    ZeroDuration,
    /// `ffmpeg` が非ゼロで終了した（破損ファイル等）。
    FfmpegFailed { stderr: String },
    /// `n == 0` でサンプル要求された。
    ZeroSamples,
    /// 入力ファイルが存在しない / 読めない。
    InputNotReadable { path: PathBuf },
    /// PNG デコード失敗（ffmpeg が出力した一時ファイルが壊れている等）。
    DecodeError(image::ImageError),
    /// 1 枚もフレームを抜けなかった（動画が空、または ffmpeg がサイレント失敗）。
    NoFramesExtracted,
    /// I/O エラー（一時ディレクトリ作成失敗等）。
    Io(io::Error),
}

impl std::fmt::Display for VideoInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FfmpegNotFound => write!(
                f,
                "ffmpeg not found in PATH; install ffmpeg (e.g. apt install ffmpeg / brew install ffmpeg) and retry"
            ),
            Self::FfprobeNotFound => write!(
                f,
                "ffprobe not found in PATH; install ffmpeg (e.g. apt install ffmpeg / brew install ffmpeg) and retry"
            ),
            Self::DurationProbeFailed { stderr } => {
                write!(f, "ffprobe failed to read duration: {stderr}")
            }
            Self::ZeroDuration => write!(
                f,
                "input video has zero / invalid duration (corrupted file or audio-only stream)"
            ),
            Self::FfmpegFailed { stderr } => write!(f, "ffmpeg failed: {stderr}"),
            Self::ZeroSamples => write!(f, "n must be >= 1 (at least one sample is required)"),
            Self::InputNotReadable { path } => {
                write!(f, "input file not readable: {}", path.display())
            }
            Self::DecodeError(e) => write!(f, "PNG decode failed: {e}"),
            Self::NoFramesExtracted => write!(
                f,
                "ffmpeg produced no frames (video may be empty or fully corrupted)"
            ),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for VideoInputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::DecodeError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for VideoInputError {
    fn from(e: io::Error) -> Self {
        VideoInputError::Io(e)
    }
}

/// 動画入力経路の対応拡張子（小文字比較）。
///
/// `mp4` / `m4v` は MPEG-4、`mov` は QuickTime、`webm` は Matroska 派生、
/// `mkv` は Matroska、`avi` は AVI。ffmpeg がデコードできる主要なコンテナを
/// CLI 引数の拡張子だけで分岐したいので、ここで明示的に列挙している。
const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "mkv", "m4v", "avi"];

/// 入力ファイルの拡張子から「動画として扱うべきか」を判定する。
///
/// 大文字 / 小文字は無視。拡張子が無い場合は `false`（=画像扱い）。
pub fn is_video_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext_lower = ext.to_ascii_lowercase();
    VIDEO_EXTS.iter().any(|v| *v == ext_lower)
}

/// `ffprobe` で動画の長さ（秒）を取得する。
///
/// 失敗時は [`VideoInputError::FfprobeNotFound`] / [`VideoInputError::DurationProbeFailed`] /
/// [`VideoInputError::ZeroDuration`] のいずれか。
fn probe_duration_seconds(video_path: &Path) -> Result<f64, VideoInputError> {
    let result = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(video_path)
        .output();
    let out = match result {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(VideoInputError::FfprobeNotFound);
        }
        Err(e) => return Err(VideoInputError::Io(e)),
    };
    if !out.status.success() {
        return Err(VideoInputError::DurationProbeFailed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    let dur: f64 = trimmed.parse().map_err(|_| VideoInputError::DurationProbeFailed {
        stderr: format!("could not parse duration: {trimmed:?}"),
    })?;
    if !dur.is_finite() || dur <= 0.0 {
        return Err(VideoInputError::ZeroDuration);
    }
    Ok(dur)
}

/// 動画から N 枚のフレームを均等区間でサンプリングする。
///
/// サンプリング時刻は `t_i = (i / (N - 1)) * duration` （i = 0..N、N == 1 なら 0.0）。
/// 各時刻で `ffmpeg -ss T -i video -frames:v 1 -vf "scale=512:-1" out_NN.png` を呼ぶ。
/// 一時ディレクトリは関数終了時に自動削除される。
///
/// # エラー
///
/// - `ffmpeg` / `ffprobe` が PATH に無い → `FfmpegNotFound` / `FfprobeNotFound`
/// - 動画長 0 / 破損 → `ZeroDuration` / `DurationProbeFailed`
/// - 1 枚も抜けなかった → `NoFramesExtracted`
/// - `n == 0` → `ZeroSamples`
/// - 入力ファイルが存在しない → `InputNotReadable`
pub fn sample_video_frames(
    video_path: &Path,
    n: usize,
) -> Result<Vec<VideoSample>, VideoInputError> {
    if n == 0 {
        return Err(VideoInputError::ZeroSamples);
    }
    if !video_path.exists() {
        return Err(VideoInputError::InputNotReadable {
            path: video_path.to_path_buf(),
        });
    }

    let duration = probe_duration_seconds(video_path)?;
    let temp_dir = tempfile::TempDir::new()?;

    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        // N == 1 なら冒頭、それ以上なら [0, duration] を均等分割。
        // 注: 末尾ぴったりは ffmpeg で frame 取れない場合があるので、
        // 直前位置に少しだけ寄せる（duration * 0.999）。
        let t_norm = if n == 1 {
            0.0
        } else {
            i as f32 / (n - 1) as f32
        };
        let mut t_seconds = t_norm as f64 * duration;
        if t_seconds >= duration {
            t_seconds = (duration - 0.001).max(0.0);
        }

        let out_path = temp_dir.path().join(format!("sample_{i:03}.png"));
        let result = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error"])
            .arg("-ss")
            .arg(format!("{t_seconds:.6}"))
            .arg("-i")
            .arg(video_path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=512:-1",
                "-pix_fmt",
                "rgb24",
            ])
            .arg(&out_path)
            .output();
        let out = match result {
            Ok(o) => o,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(VideoInputError::FfmpegNotFound);
            }
            Err(e) => return Err(VideoInputError::Io(e)),
        };
        if !out.status.success() {
            return Err(VideoInputError::FfmpegFailed {
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        if !out_path.exists() {
            // ffmpeg が exit 0 だがファイルを生成しなかったケース（末尾近辺で
            // 偶発的に起こる）。このサンプルだけスキップする。
            continue;
        }
        let dyn_img = image::open(&out_path).map_err(VideoInputError::DecodeError)?;
        let frame = dyn_img.to_rgb8();
        samples.push(VideoSample { frame, t: t_norm });
    }

    if samples.is_empty() {
        return Err(VideoInputError::NoFramesExtracted);
    }
    Ok(samples)
}

/// N サンプル × k クラスタの色トラック。
///
/// `template_clusters` は先頭フレームから抽出した k クラスタで、後段の orb 配置の
/// 「位置 / 重み」基準として使う（位置固定の根拠）。`tracks` は `template_clusters[i]`
/// に対応する色サンプル列（`tracks[i].len() == samples.len()`）。`sample_times` は
/// 各サンプルの正規化時刻 `[0, 1]`。
///
/// # 設計メモ
///
/// - `template_clusters` の color は先頭サンプルのもの。サンプル列の先頭値と
///   一致するよう、`tracks[i][0] == template_clusters[i].color` を保つ。
/// - サンプルが 1 枚しか抜けなかった場合、`tracks[i].len() == 1` の単色トラックに
///   なる（補間関数は `len == 1` を全 t で同色で扱う）。
#[derive(Debug, Clone)]
pub struct ColorTracks {
    pub template_clusters: Vec<Cluster>,
    pub tracks: Vec<Vec<[u8; 3]>>,
    /// 各サンプルの正規化時刻 [0, 1]。現状の補間関数は `t` から index を引くので
    /// `sample_times` は使わないが、将来 unequal sampling や hot-fix で使う想定で
    /// データ自体は保持する（API として隣接情報を残すため）。
    #[allow(dead_code)]
    pub sample_times: Vec<f32>,
}

/// 先頭サンプルの k クラスタを「テンプレート」として、各サンプルの k クラスタを
/// LAB 距離 greedy マッチングで対応付ける。
///
/// 戻り値の `tracks[i].len()` は `samples.len()` と一致する。マッチング失敗（クラスタ数
/// 不足）した場合は、その時刻のトラック値はテンプレート色にフォールバックする。
pub fn build_color_tracks(
    samples: &[VideoSample],
    k: usize,
) -> Result<ColorTracks, VideoInputError> {
    if samples.is_empty() {
        return Err(VideoInputError::NoFramesExtracted);
    }
    if k == 0 {
        // K 0 は意味が無い。呼び出し側が CLI と同じ k=6 を渡す前提だが防衛。
        return Err(VideoInputError::ZeroSamples);
    }

    let template_clusters = extract_clusters(&samples[0].frame, k)
        .map_err(|e| VideoInputError::DurationProbeFailed { stderr: e.to_string() })?;

    let n_clusters = template_clusters.len();
    let n_samples = samples.len();

    // tracks[i][s] = sample s での template_clusters[i] の色。
    // 初期値はテンプレート色で埋め、各 sample で上書きする（マッチング失敗時の
    // フォールバックを兼ねる）。
    let mut tracks: Vec<Vec<[u8; 3]>> = template_clusters
        .iter()
        .map(|c| vec![c.color; n_samples])
        .collect();

    let mut sample_times: Vec<f32> = Vec::with_capacity(n_samples);
    for (s_idx, sample) in samples.iter().enumerate() {
        sample_times.push(sample.t);
        // 先頭サンプルはテンプレートそのもの。
        if s_idx == 0 {
            for (i, c) in template_clusters.iter().enumerate() {
                tracks[i][0] = c.color;
            }
            continue;
        }
        let sample_clusters = match extract_clusters(&sample.frame, k) {
            Ok(c) => c,
            Err(_) => continue, // このサンプルだけテンプレート色フォールバック。
        };
        // greedy 最近傍マッチング。template index → sample index の使用済みフラグを
        // 持って、template_clusters の順に最近傍 sample cluster を取り、両方を
        // 使用済みにする。クラスタ数が template と異なるケース（k より少ない実色
        // しかなかった等）でも、余り側はそのまま残す。
        let mut sample_used = vec![false; sample_clusters.len()];
        for (t_idx, t_c) in template_clusters.iter().enumerate() {
            let mut best_idx: Option<usize> = None;
            let mut best_d = f32::MAX;
            for (s_c_idx, s_c) in sample_clusters.iter().enumerate() {
                if sample_used[s_c_idx] {
                    continue;
                }
                let d = lab_distance_rgb(t_c.color, s_c.color);
                if d < best_d {
                    best_d = d;
                    best_idx = Some(s_c_idx);
                }
            }
            if let Some(idx) = best_idx {
                tracks[t_idx][s_idx] = sample_clusters[idx].color;
                sample_used[idx] = true;
            }
            // 見つからなかった場合は初期化値（テンプレート色）のまま残る。
        }
        let _ = n_clusters; // 将来の整合チェック用（warning 抑止）。
    }

    Ok(ColorTracks {
        template_clusters,
        tracks,
        sample_times,
    })
}

/// 2 つの sRGB 色を LAB 空間に変換し、ΔE (ユークリッド) を返す。
///
/// CLI crate は palette を依存していないので、変換は自前で実装する
/// （sRGB → linear → XYZ → CIE Lab、D65、CIE 1931 標準観測者）。距離は
/// ΔE76 相当（単純ユークリッド）。greedy マッチングの並べ替えに使うだけなので、
/// より精緻な ΔE2000 までは要らない。
fn lab_distance_rgb(a: [u8; 3], b: [u8; 3]) -> f32 {
    let a_lab = rgb_to_lab(a);
    let b_lab = rgb_to_lab(b);
    let dl = a_lab[0] - b_lab[0];
    let da = a_lab[1] - b_lab[1];
    let db = a_lab[2] - b_lab[2];
    (dl * dl + da * da + db * db).sqrt()
}

/// sRGB (0-255) を CIE Lab (D65, 2°) に変換する。
///
/// sRGB → linear sRGB の gamma 解除 → XYZ → Lab (Bradford は使わず素直な D65)。
/// 結果は [L, a, b]。ΔE 計算用なので 0..100 / -128..127 等の厳密な範囲には縛らない。
fn rgb_to_lab(rgb: [u8; 3]) -> [f32; 3] {
    let r = srgb_to_linear(rgb[0] as f32 / 255.0);
    let g = srgb_to_linear(rgb[1] as f32 / 255.0);
    let b = srgb_to_linear(rgb[2] as f32 / 255.0);

    // sRGB linear → XYZ (D65)
    let x = r * 0.4124564 + g * 0.3575761 + b * 0.1804375;
    let y = r * 0.2126729 + g * 0.7151522 + b * 0.0721750;
    let z = r * 0.0193339 + g * 0.119_192 + b * 0.9503041;

    // D65 reference white
    const XN: f32 = 0.95047;
    const YN: f32 = 1.00000;
    const ZN: f32 = 1.08883;

    let fx = lab_f(x / XN);
    let fy = lab_f(y / YN);
    let fz = lab_f(z / ZN);

    let l = 116.0 * fy - 16.0;
    let a = 500.0 * (fx - fy);
    let b_ = 200.0 * (fy - fz);
    [l, a, b_]
}

#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn lab_f(t: f32) -> f32 {
    const DELTA: f32 = 6.0 / 29.0;
    if t > DELTA * DELTA * DELTA {
        t.cbrt()
    } else {
        t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn is_video_path_detects_common_exts() {
        assert!(is_video_path(Path::new("a.mp4")));
        assert!(is_video_path(Path::new("a.MP4")));
        assert!(is_video_path(Path::new("foo/bar.webm")));
        assert!(is_video_path(Path::new("a.mov")));
        assert!(is_video_path(Path::new("a.mkv")));
        assert!(is_video_path(Path::new("a.m4v")));
        assert!(is_video_path(Path::new("a.avi")));
        // 静止画は false。
        assert!(!is_video_path(Path::new("a.png")));
        assert!(!is_video_path(Path::new("a.jpg")));
        assert!(!is_video_path(Path::new("a.webp")));
        // 拡張子無し。
        assert!(!is_video_path(Path::new("noext")));
    }

    #[test]
    fn sample_video_frames_zero_n_returns_error() {
        let path = PathBuf::from("/tmp/does-not-matter.mp4");
        match sample_video_frames(&path, 0) {
            Err(VideoInputError::ZeroSamples) => {}
            other => panic!("expected ZeroSamples, got {other:?}"),
        }
    }

    #[test]
    fn sample_video_frames_missing_file_returns_error() {
        let path = PathBuf::from("/tmp/orber-test-missing-9f3c1d2.mp4");
        // 存在しないパスを渡すと InputNotReadable。
        match sample_video_frames(&path, 4) {
            Err(VideoInputError::InputNotReadable { .. }) => {}
            other => panic!("expected InputNotReadable, got {other:?}"),
        }
    }

    #[test]
    fn lab_distance_zero_for_same_color() {
        let d = lab_distance_rgb([100, 50, 200], [100, 50, 200]);
        assert!(d < 1e-3, "same color must have ~0 distance, got {d}");
    }

    #[test]
    fn lab_distance_red_blue_large() {
        // 赤と青は LAB 上で十分離れている (ΔE > 50)。
        let d = lab_distance_rgb([255, 0, 0], [0, 0, 255]);
        assert!(d > 50.0, "red vs blue should be far in LAB, got {d}");
    }

    #[test]
    fn build_color_tracks_empty_samples_errors() {
        let res = build_color_tracks(&[], 6);
        assert!(matches!(res, Err(VideoInputError::NoFramesExtracted)));
    }

    #[test]
    fn build_color_tracks_single_sample_yields_unit_tracks() {
        // 1 枚のサンプルだけ与えたとき、各 cluster の track が長さ 1 の単色列に
        // なる。後段の interpolate_color_track は len==1 で全 t 同色を返すので、
        // 「動画 1 フレームしか抜けなかった」場合でも色が固定される。
        let frame = image::ImageBuffer::from_fn(64, 64, |_, _| image::Rgb([200u8, 100, 50]));
        let samples = vec![VideoSample { frame, t: 0.0 }];
        let tracks = build_color_tracks(&samples, 1).expect("should produce tracks");
        assert_eq!(tracks.tracks.len(), 1, "1 cluster expected");
        assert_eq!(tracks.tracks[0].len(), 1, "1 sample → track len 1");
        assert_eq!(tracks.sample_times, vec![0.0]);
    }

    #[test]
    fn build_color_tracks_two_samples_color_changes() {
        // 2 枚のサンプルで、色が違うものを与えたとき、track[0][0] と track[0][1] が
        // 異なる色になる（greedy マッチングが両方を埋める）。
        let frame_a =
            image::ImageBuffer::from_fn(64, 64, |_, _| image::Rgb([220u8, 30, 30]));
        let frame_b = image::ImageBuffer::from_fn(64, 64, |_, _| image::Rgb([30u8, 30, 220]));
        let samples = vec![
            VideoSample {
                frame: frame_a,
                t: 0.0,
            },
            VideoSample {
                frame: frame_b,
                t: 1.0,
            },
        ];
        let tracks = build_color_tracks(&samples, 1).expect("tracks");
        assert_eq!(tracks.tracks.len(), 1);
        assert_eq!(tracks.tracks[0].len(), 2);
        // track[0][0] は赤系、track[0][1] は青系のはず。
        let t0 = tracks.tracks[0][0];
        let t1 = tracks.tracks[0][1];
        assert!(t0[0] > t0[2], "first sample should be red-dominant: {t0:?}");
        assert!(t1[2] > t1[0], "second sample should be blue-dominant: {t1:?}");
    }
}
