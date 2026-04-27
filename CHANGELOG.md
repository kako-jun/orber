# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.3.0] - 2026-04-28

### Added
- New CLI flag `--count <N>` (1..=200, default 20) that controls how many orbs are visible on screen at once. The K colors picked by k-means are *expanded* into N orbs by weight-proportional color sampling and per-orb cross-axis scattering, so a single image can fill roughly 70% of the frame at the default count. (#41)
- All-orb breathing modulation in **three independent axes**: radius ±10%, blur ±15%, opacity ±5%. Each orb's three axes are decorrelated by separate seed-derived phase offsets, and each axis loops once per clip duration. (#41)
- `OrbStyle` enum (`Rim` / `Soft`) and `render_one_orb` per-orb rendering helper. Each orb is assigned a style deterministically from the seed (≈50:50), so a single frame mixes the rim-emphasized look with plain soft gradients. (#41)

### Changed
- **BREAKING**: Motion model rebuilt as a one-way conveyor belt. Each clip flows in a single direction (left→right / right→left / top→bottom / bottom→top) with all orbs traveling the same way. Orbs no longer reflect or oscillate; they exit one edge and re-enter from the opposite edge (wrap loop). (#41)
- **BREAKING**: `MotionShape` (`Still`, `Lissajous`, `Vertical`, `Horizontal`, `Diagonal`, `Breathe`, `Twinkle`) is **removed**. The standalone `Breathe` / `Twinkle` modes are gone — breathing is now an automatic effect applied to every clip.
- **BREAKING**: Old `MotionSpeed` variants (`Subtle` / `Slow` / `Lively`) are **removed** in favor of `VerySlow` / `Slow` / `Medium`, defined as integer screen-cross counts per clip (1 / 2 / 3). Pixel-exact loop closure at `t=0 ≡ t=1` is preserved.
- **BREAKING**: New `MotionDirection` enum (`LeftToRight`, `RightToLeft`, `TopToBottom`, `BottomToTop`) added.
- **BREAKING**: CLI flags `--motion`, `--motion-shape`, `--motion-speed` are **removed** and replaced with `--direction <lr|rl|tb|bt>` and `--speed <very-slow|slow|medium>`.
- **BREAKING**: Animation boundary mode switched from `clamp` to wrap (`rem_euclid`).
- **BREAKING**: `DEFAULT_VARIATIONS` preset rebuilt around direction × speed × `count` × `orb_size` × `blur` (color is no longer a preset axis). Output filenames change to `snapshot_lr_dense`, `snapshot_rl_huge`, `snapshot_tb_fine`, `snapshot_bt_blurry`, `flow_lr_slow`, `flow_rl_very_slow`, `flow_tb_dense`, `flow_bt_blurry`, `flow_lr_medium`, `flow_rl_huge`. (#41)
- **BREAKING**: `VariationSpec` now carries `count: usize` instead of the old color/cluster fields. `VariationSpec.shape` / `VariationSpec.speed` (old types) replaced with `direction` and the new `speed`.
- PNG single-output path now goes through `animate::render_frame(t=0)` instead of `render_static` so `--count` takes effect for stills as well as videos.
- Animated variation duration extended from 4000 ms to 8000 ms so the slow conveyor pacing reads as gentle.

### Removed
- **BREAKING**: `ColorMod` module is **deleted**. Hue shift, lightness bias, saturation modulation, and dominant cluster rotation are gone. The premise — that a single input photo should yield multiple recolored variations — was wrong: if you want different colors, feed in a different image. K-means palette colors are now used unmodified end-to-end. (#41)
- **BREAKING**: `VariationSpec` fields `hue_shift_deg`, `lightness_bias`, `saturation`, `dominant_rotation`, and `cluster_count` are **removed**. The `VariationSpec::color_mod()` method is also gone. The k-means K used by the variations path is fixed internally at 5.
- **BREAKING**: CLI flag `--background` is **removed**, along with the entire `background` module (`Background` enum, `resolve`, `BackgroundParseError`). The background color is now derived automatically from the input image: the dominant (highest-weight) k-means cluster becomes the canvas color, and the remaining clusters become the orb pool. A nightscape gives a black canvas with bright points; a daytime sky gives a sky-blue canvas with floating points; a beige interior gives a beige canvas with small accents. To change colors, change the input image. (#41)
- **BREAKING**: New helpers `cluster::derive_background_rgba` and `cluster::drop_dominant` added; the `--background transparent` rejection branch for mp4/webm is gone (auto-derived backgrounds are always opaque). (#41)

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
