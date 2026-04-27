# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.3.0] - 2026-04-28

### Added
- `ColorMod` module: HSL-based color modulation with hue shift, lightness bias, saturation, and dominant cluster rotation (#41)
- `VariationSpec` extended with `hue_shift_deg`, `lightness_bias`, `cluster_count`, and `dominant_rotation` fields (#41)
- All-orb breathing modulation: every orb now subtly pulses (±10% radius) over the clip duration as a baseline ambient effect (#41)

### Changed
- **BREAKING**: Motion model rebuilt as a one-way conveyor belt. Each clip flows in a single direction (left→right / right→left / top→bottom / bottom→top) with all orbs traveling the same way. Orbs no longer reflect or oscillate; they exit one edge and re-enter from the opposite edge (wrap loop). (#41)
- **BREAKING**: `MotionShape` (`Still`, `Lissajous`, `Vertical`, `Horizontal`, `Diagonal`, `Breathe`, `Twinkle`) is **removed**. The standalone `Breathe` / `Twinkle` modes are gone — breathing is now an automatic effect applied to every clip.
- **BREAKING**: Old `MotionSpeed` variants (`Subtle` / `Slow` / `Lively`) are **removed** in favor of `VerySlow` / `Slow` / `Medium`, defined as integer screen-cross counts per clip (1 / 2 / 3). Pixel-exact loop closure at `t=0 ≡ t=1` is preserved.
- **BREAKING**: New `MotionDirection` enum (`LeftToRight`, `RightToLeft`, `TopToBottom`, `BottomToTop`) added.
- **BREAKING**: CLI flags `--motion`, `--motion-shape`, `--motion-speed` are **removed** and replaced with `--direction <lr|rl|tb|bt>` and `--speed <very-slow|slow|medium>`.
- **BREAKING**: Animation boundary mode switched from `clamp` to wrap (`rem_euclid`).
- **BREAKING**: `DEFAULT_VARIATIONS` preset rebuilt to express direction × speed × color combinations. Output filenames change to `warm_glow_lr`, `cool_mist_rl`, `hi_key_tb`, `dark_mood_bt`, `drift_lr_slow`, `drift_rl_very_slow`, `drift_tb_slow`, `drift_bt_slow`, `aurora_rl`, `dream_lr`.
- **BREAKING**: `VariationSpec.shape` / `VariationSpec.speed` (old types) replaced with `direction` and the new `speed`.
- Orb size in `DEFAULT_VARIATIONS` increased to 2.8–4.0 so the largest orbs occupy roughly half the short screen edge. Cropping at the edges is part of the intended look.
- Animated variation duration extended from 4000 ms to 8000 ms so the slow conveyor pacing reads as gentle.
- Still variations now render the `t = 0` frame of the conveyor (instead of `render_static` directly), so PNG outputs share the phase-scattered, edge-cropped composition of the videos.

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
