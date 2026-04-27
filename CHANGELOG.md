# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.3.0] - 2026-04-28

### Added
- `ColorMod` module: HSL-based color modulation with hue shift, lightness bias, saturation, and dominant cluster rotation (#41)
- `VariationSpec` extended with `hue_shift_deg`, `lightness_bias`, `cluster_count`, and `dominant_rotation` fields (#41)

### Changed
- **BREAKING**: `DEFAULT_VARIATIONS` preset rebuilt from scratch. All 10 entries now have distinct color-axis values so a single input produces visibly different outputs. Output filenames change accordingly (`warm_glow`, `cool_mist`, `hi_key`, `dark_mood`, `drift_diagonal`, `breathe_deep`, `twinkle`, `wander_warm`, `aurora`, `dream`).
- **BREAKING**: Animation boundary mode switched from `clamp` to wrap (`rem_euclid`). Orbs now disappear off one edge and re-enter from the opposite edge instead of sticking to the frame.
- Motion preset amplitudes rebalanced: `Subtle` 0.02 → 0.06, `Slow` 0.06 → 0.15, `Lively` 0.12 → 0.25. `Lively` `freq_scale` lowered from 2 to 1.
- `Breathe` radius modulation widened from ±2·amp to ±5·amp.
- `Twinkle` is now a brightness-only flicker (radius held at 1.0); lightness amplitude widened to `amp_color * 6`.
- Animated variation duration extended from 4000 ms to 6000 ms.

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
