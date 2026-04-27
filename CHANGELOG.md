# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.2.0] - 2026-04-27

### Added
- Configurable background color via `--background` (#21)
- Motion pattern extensions with orthogonal shape × speed parameters (#22)
- Batch variation generator CLI for producing multiple outputs at once (#23)
- Aquarelle night-mood cell-shading set: bleed, bloom, offset, halo (#8)
- CLI flag range validation with clear error messages (#15)

### Changed
- `OutputMode` errors are now strongly typed via `thiserror` (#14)

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
