//! Output mode detection from a target file path.
//!
//! orber decides what kind of pipeline to run based on the output file
//! extension. The mapping is intentionally narrow: only the extensions
//! listed in [`OutputMode`] are accepted, anything else is an error so the
//! user gets immediate feedback instead of running a full render and
//! producing a file the rest of the toolchain cannot consume.

use std::path::Path;

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

impl OutputMode {
    /// Infer the [`OutputMode`] from a file path's extension.
    ///
    /// Matching is case-insensitive. Returns an `Err` describing the
    /// problem if the extension is missing or not one of the supported
    /// formats.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| {
                format!(
                    "output path {:?} has no extension; expected one of png, webp, mp4, webm, svg, css",
                    path.display()
                )
            })?
            .to_ascii_lowercase();

        match ext.as_str() {
            "png" => Ok(OutputMode::Png),
            "webp" => Ok(OutputMode::Webp),
            "mp4" => Ok(OutputMode::Mp4),
            "webm" => Ok(OutputMode::Webm),
            "svg" => Ok(OutputMode::Svg),
            "css" => Ok(OutputMode::Css),
            other => Err(format!(
                "unsupported output extension {:?}; expected one of png, webp, mp4, webm, svg, css",
                other
            )),
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
        assert!(
            err.contains("gif"),
            "error should mention the bad ext: {err}"
        );
    }

    #[test]
    fn missing_extension() {
        let err = OutputMode::from_path("noext").unwrap_err();
        assert!(err.contains("no extension"), "got: {err}");
    }

    #[test]
    fn nested_path_uppercase_mp4() {
        assert_eq!(
            OutputMode::from_path("dir/sub/clip.MP4"),
            Ok(OutputMode::Mp4)
        );
    }
}
