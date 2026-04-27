# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.1.0] - 2026-04-27

### Added
- CLI for converting photos into abstract orb mood output
- Static PNG rendering
- Vertical video export to MP4 and WebM via `ffmpeg`
- Static SVG export
- CSS background snippet export
- Color clustering via `kmeans_colors` + `palette`
- Deterministic seeded animation via `--seed`
- Parameters for orb size, blur, motion, shape, saturation, and duration
- GitHub Actions CI and release workflow

### Notes
- Video input extraction is not implemented yet.
- WebP is accepted by the CLI but not rendered yet.
