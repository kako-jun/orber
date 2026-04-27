//! Output mode detection from a target file path.
//!
//! orber decides what kind of pipeline to run based on the output file
//! extension. The mapping is intentionally narrow: only the extensions
//! listed in [`OutputMode`] are accepted, anything else is an error so the
//! user gets immediate feedback instead of running a full render and
//! producing a file the rest of the toolchain cannot consume.

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Output rendering mode inferred from the output file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Static PNG raster.
    Png,
    /// Static WebP raster.
    Webp,
    /// MP4 video (vertical 9:16 by default).
    Mp4,
    /// WebM video (vertical 9:16 by default).
    Webm,
    /// Static SVG vector.
    Svg,
    /// CSS gradient / `@keyframes` style snippet.
    Css,
}

const SUPPORTED_EXTS: &str = "png, webp, mp4, webm, svg, css";

/// Errors returned by [`OutputMode::from_path`].
///
/// Carrying the offending value (path / extension) instead of a free-form
/// `String` lets call sites pattern-match on the failure mode rather than
/// scraping error messages.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OutputModeError {
    /// Path has no usable extension at all (e.g. `"noext"`, `".png"` hidden
    /// files where `Path::extension()` returns `None`).
    #[error("output path {} has no extension; expected one of {SUPPORTED_EXTS}", path.display())]
    MissingExtension { path: PathBuf },
    /// Extension is present but does not match a supported output format.
    #[error("unsupported output extension {ext}; expected one of {SUPPORTED_EXTS}")]
    UnsupportedExtension { ext: String },
}

impl OutputMode {
    /// Infer the [`OutputMode`] from a file path's extension.
    ///
    /// Matching is case-insensitive. Returns [`OutputModeError`] if the
    /// extension is missing or not one of the supported formats.
    ///
    /// ```
    /// use orber::output_mode::OutputMode;
    /// use std::path::Path;
    ///
    /// assert_eq!(OutputMode::from_path(Path::new("clip.mp4")).unwrap(), OutputMode::Mp4);
    /// assert!(OutputMode::from_path(Path::new(".png")).is_err()); // hidden file: extension is None
    /// assert!(OutputMode::from_path(Path::new("noext")).is_err());
    /// ```
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self, OutputModeError> {
        let path = path.as_ref();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| OutputModeError::MissingExtension {
                path: path.to_path_buf(),
            })?
            .to_ascii_lowercase();

        match ext.as_str() {
            "png" => Ok(OutputMode::Png),
            "webp" => Ok(OutputMode::Webp),
            "mp4" => Ok(OutputMode::Mp4),
            "webm" => Ok(OutputMode::Webm),
            "svg" => Ok(OutputMode::Svg),
            "css" => Ok(OutputMode::Css),
            other => Err(OutputModeError::UnsupportedExtension {
                ext: other.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_lowercase() {
        assert_eq!(OutputMode::from_path("a.png"), Ok(OutputMode::Png));
    }

    #[test]
    fn png_uppercase() {
        assert_eq!(OutputMode::from_path("a.PNG"), Ok(OutputMode::Png));
    }

    #[test]
    fn webp() {
        assert_eq!(OutputMode::from_path("a.webp"), Ok(OutputMode::Webp));
    }

    #[test]
    fn mp4() {
        assert_eq!(OutputMode::from_path("a.mp4"), Ok(OutputMode::Mp4));
    }

    #[test]
    fn webm() {
        assert_eq!(OutputMode::from_path("a.webm"), Ok(OutputMode::Webm));
    }

    #[test]
    fn svg() {
        assert_eq!(OutputMode::from_path("a.svg"), Ok(OutputMode::Svg));
    }

    #[test]
    fn css() {
        assert_eq!(OutputMode::from_path("a.css"), Ok(OutputMode::Css));
    }

    #[test]
    fn unsupported_extension() {
        let err = OutputMode::from_path("a.gif").unwrap_err();
        match err {
            OutputModeError::UnsupportedExtension { ext } => assert_eq!(ext, "gif"),
            other => panic!("expected UnsupportedExtension, got {other:?}"),
        }
    }

    #[test]
    fn missing_extension() {
        let err = OutputMode::from_path("noext").unwrap_err();
        match err {
            OutputModeError::MissingExtension { path } => {
                assert_eq!(path.to_str().unwrap(), "noext");
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }

    #[test]
    fn nested_path_uppercase_mp4() {
        assert_eq!(
            OutputMode::from_path("dir/sub/clip.MP4"),
            Ok(OutputMode::Mp4)
        );
    }

    #[test]
    fn nested_path_no_extension() {
        let err = OutputMode::from_path("dir/sub/clip").unwrap_err();
        assert!(matches!(err, OutputModeError::MissingExtension { .. }));
    }

    #[test]
    fn trailing_dot() {
        // "foo." yields Some("") from Path::extension(), so it falls through
        // to the UnsupportedExtension branch (not MissingExtension).
        let err = OutputMode::from_path("foo.").unwrap_err();
        match err {
            OutputModeError::UnsupportedExtension { ext } => assert_eq!(ext, ""),
            other => panic!("expected UnsupportedExtension, got {other:?}"),
        }
    }

    #[test]
    fn multi_dot_unsupported_extension() {
        // Only the final ".bak" counts as the extension.
        let err = OutputMode::from_path("foo.PNG.bak").unwrap_err();
        match err {
            OutputModeError::UnsupportedExtension { ext } => assert_eq!(ext, "bak"),
            other => panic!("expected UnsupportedExtension, got {other:?}"),
        }
    }

    #[test]
    fn display_messages_remain_user_facing() {
        // ユーザー向けメッセージが拡張子と「expected one of ...」を含むことを保証する
        // （main.rs での表示と互換）。
        let missing = OutputMode::from_path("noext").unwrap_err().to_string();
        assert!(missing.contains("no extension"), "got: {missing}");
        assert!(missing.contains("png"), "should list supported: {missing}");

        let bad = OutputMode::from_path("a.gif").unwrap_err().to_string();
        assert!(bad.contains("gif"), "got: {bad}");
        assert!(bad.contains("png"), "should list supported: {bad}");
    }
}
