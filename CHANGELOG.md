# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Added
- Web GUI control rows (#131). The old collapsible Advanced section is removed; the Studio surface now shows flat always-visible rows for Shape / Count / Speed / Softness directly under the aspect toggles. Every control immediately reruns the batch. Glyph mode now includes an inline IME-safe single-character input plus a symbol picker filtered through `glyph_supported()`. The old large "Roll / ガチャを引く" chip is gone; only a small reload icon remains at the bottom, and it spins while decoding / generating / animating. (#131)
- Glyph rendering quality pass (#132). The old binary alpha-mask glyph path is replaced with a cached `R8` signed-distance field (`render_glyph_sdf` / `get_glyph_sdf`), so glyphs now use the same rim/soft falloff logic as circle orbs instead of a hard text fill. The WebGL path also gains per-orb animated rotation (`base_angle`, `rot_speed_signed`) derived deterministically alongside `speed_mult` without perturbing the legacy circle RNG stream, keeping `t = 0 ≡ 1` loop closure intact while avoiding batches of perfectly aligned symbols. (#132)
- `WasmParams` gains four optional string fields used by the Web GUI: `glyph_char` (1 char, required when `shape == "glyph"`), `count_preset` (`""` / `"low"` / `"mid"` / `"high"` → respect / 10 / 20 / 30), `speed_preset` (`""` / `"slow"` / `"mid"` / `"fast"` — empty keeps the spec's original speed split; explicit GUI choices map to VerySlow / Slow / Mid), and `softness_preset` (`""` / `"low"` / `"mid"` / `"high"` — empty / mid is `SoftnessPreset::Mid` identity). All four default to `""` via `#[serde(default)]` so existing wasm callers stay byte-compatible. The previous `shape: "circle"` only restriction is lifted — `"glyph"` is now accepted alongside it. (#55 Phase B, #131)
- WebGL2 fragment shader (`web/src/lib/orberGl.ts`) now samples `u_glyph_sdf: sampler2D` for Glyph mode, computes `r` from the signed-distance value, and feeds the same rim/soft falloff curve used by Circle mode. The per-orb render-data slots now also carry `base_angle` and `rot_speed_signed`; the renderer uploads the glyph SDF exactly once per shape change via `GlRenderer.setGlyphSdf(mask, size)`, and subsequent frames only update `u_t`. (#55 Phase B, #132)

- `--shape glyph` — render each orb as a single text character instead of a round blob. Pick the character with `--glyph-char <CHAR>` (default `☆`, exactly one character). Glyphs are drawn via `ttf-parser` outline extraction against a bundled **Noto Sans Symbols 2 subset** (~177 KB, `include_bytes!` from `crates/core/assets/fonts/NotoSansSymbols2-Regular.ttf`, **© Google Inc., licensed under SIL Open Font License 1.1**; full license text shipped at `crates/core/assets/fonts/OFL.txt`) covering ASCII, digits, punctuation, arrows, geometric shapes, Dingbats, and supplemental symbols. Hiragana / kanji / emoji and other characters outside the subset are silently skipped instead of rendering tofu, and the CLI emits a one-shot stderr warning at startup when `--glyph-char` falls outside the bundled coverage. Glyph rendering now uses a cached SDF path, so `--blur` and `--softness` affect it with the same soft-edge meaning as circle orbs. New `OrbShape::Glyph { ch, font: GlyphFontId }` + `GlyphFontId` enum keep `OrbShape: Copy` by routing the `ttf_parser::Face<'static>` through a process-global `OnceLock` cache keyed by enum variant (rather than per-orb `Arc<Face>`). New dep `ttf-parser = "0.25"`. (#55, #132)
- `--count-preset low|mid|high` — shorthand alternative to `--count <N>`, mapped to `10 / 20 / 30`. Mutually exclusive with `--count`. (#55, #131)
- `--speed mid` and `--speed fast` — extend the existing `very-slow` / `slow` axis. New `MotionSpeed::Mid` / `MotionSpeed::Fast` variants map to integer cycle counts `3` / `4` (existing `VerySlow` = 1, `Slow` = 2 unchanged), so the per-orb `1x` / `2x` multiplier still resolves to integer × integer cycles per clip and the `t = 0 ≡ t = 1` pixel-exact loop closure is preserved. (#55)
- `--softness low|mid|high` — single-axis preset that bundles alpha, blur, and edge softness. `low` = sharper / less blur, `high` = softer / more blur, `mid` = identity. A core regression test pins the `mid = identity` invariant. New `SoftnessPreset { Low, Mid, High }` enum lives in `crates/core/src/style.rs`. (`--contrast` renamed before release, so no compatibility baggage.) (#55, #131)
- Drop-zone thumbnail long-press preview: pressing and holding the drop-zone for ~400ms opens a full-viewport overlay showing the source image at up to 90vh × 90vw (`object-contain`). Releasing closes it. A short tap is treated as a normal click and still opens the file picker. The long-press path uses `setPointerCapture` so the gesture stays bound to the label even if the finger slides outside, and the overlay itself is `pointer-events: none` so it never steals the eventual `pointerup`. iOS callout / loupe / drag are suppressed via `select-none touch-none draggable=false -webkit-touch-callout: none` on the thumbnail. Documented in DESIGN.md §4 PreviewOverlay. (#57)

### Changed
- Web GUI tile count unified to **12** for both portrait and landscape (`BATCH_TILE_COUNT = 12`, replaces the old portrait-10 / landscape-9 split). 12 was picked because it is divisible by 1/2/3/4/6/12, so the grid lays out cleanly across phone widths. 8 stills + 4 videos. (#61)
- Web GUI video tiles now start playing **simultaneously** once all four mp4 encodes finish, rather than each tile starting whenever its own encode completes. `<video autoplay>` is removed; references are collected via `ref` callbacks into `videoRefs`, and a single `play()` burst is fired at the end of the run after `await yieldFrame()` flushes the DOM mounts. PNG remains underneath as a still backdrop while encoding is in progress, so tiles read as paused snapshots until the simultaneous start. (#61)
- Web GUI video tiles reduced from 5 to 4 (`GUI_VIDEO_COUNT_DEFAULT = 4`). The 4 video tiles are now pinned to LR / RL / TB / BT respectively — every batch always shows all four motion axes side by side regardless of aspect (portrait splits 6+4, landscape splits 5+4). Direction assignment lives in the new `orber_core::variations::GUI_VIDEO_DIRECTIONS: [MotionDirection; GUI_VIDEO_COUNT_DEFAULT]` constant; `crates/wasm::start_animation_for_batch_spec` indexes it by `spec_idx - still_count`. The array length is type-locked to `GUI_VIDEO_COUNT_DEFAULT`, so changing the constant without extending the array becomes a compile error. Supersedes the earlier "last 5 tiles" entry below. (#59)
- Repository restructured into a Cargo workspace with two crates: `orber-core` (pure rendering library, `crates/core/`) and `orber` (CLI binary, `crates/cli/`). The split is internal-only — there are no user-facing CLI changes, no flag changes, and no output-format changes. `orber-core` builds for `wasm32-unknown-unknown` to unblock future GUI / Web frontends. (#35)
- Web GUI batch generation switched from the hand-tuned `DEFAULT_VARIATIONS` preset to per-call random spec generation via the new `orber_core::variations::random_batch_specs(seed, total, still_count)`. Dropping the same image now produces a different layout every time, instead of 10 layouts that share orb positions and only differ by color. The only fixed framing is "first half is `VariationKind::Png` (still), second half is `VariationKind::Mp4` (still rendered as PNG until the animated-output GUI lands in #50)"; direction / speed / count / orb_size / blur / seed / duration_ms are all uniformly sampled per call from `random_ranges`. Tile thumbnails now follow the chosen aspect ratio instead of square-cropping, and landscape mode renders 9 tiles (3×3) instead of 10 to match the grid. The CLI's `--variations` keeps the deterministic preset for reproducibility. (#48)
- Web GUI `generate_batch` now pins the **last 5 tiles** to `VariationKind::Mp4` regardless of total count (`still_count = total - 5`). Both the portrait (10 tiles) and landscape (9 tiles) layouts therefore produce 5 video-track tiles in the second half — the still/video ratio no longer drifts when the layout changes.

### Added
- `orber_core::animate::AnimationCursor` — owning iterator that yields RGBA frames at `t = i / total_frames` for `i = 0..total_frames`. Wraps `precompute_orb_params` once and calls `render_frame_with_params` per frame, so the t=0 ≡ t=1 loop closure is preserved (the sequence never emits t=1, making the next loop iteration ピクセル一致 with the first frame). (#50)
- `orber-wasm` exports `AnimationHandle` (a wasm-bindgen wrapper over `AnimationCursor`) and `start_animation_for_batch_spec(params, n, spec_idx, total_frames)`. The JS frontend can pull RGBA frames one at a time via `next_frame()` and feed them into `VideoEncoder` without holding the full sequence in memory. (#50)
- Web GUI animates the last 5 tiles in-place: after the static PNG previews land, each `Mp4`-kind tile is encoded to H.264 / `avc1.42E01F` at 24fps × 4s via WebCodecs `VideoEncoder` + `mp4-muxer`, then swapped from `<img>` to a muted-autoplay-loop `<video>`. The tile keeps its preview PNG as `poster` so the swap is seamless. Per-tile encode failures don't block the rest. (#50)
- The selected/all-DL flow now picks the right payload per tile: still tiles → `.png`, finished video tiles → `.mp4`. ZIPs mix both extensions. (#50)

### Added
- `orber_core::batch::generate_batch` — given a source image, k, canvas size, shape, and a list of `VariationSpec`, returns one PNG byte buffer per spec. Used by the upcoming GUI / WASM frontend; the CLI's `--variations` mode will eventually be a thin wrapper around this. (#35)
- New workspace crate `orber-wasm` (`crates/wasm/`) — wasm-bindgen wrapper around `orber-core` for browsers. Exposes `generate_single` (1 PNG), `generate_batch` (N PNGs from `DEFAULT_VARIATIONS`), and `generate_svg` (SVG string). Image decoding is left to the JS side: callers pass raw RGB bytes from `<canvas>` / `ImageData`, keeping the wasm bundle small. Includes a minimal `crates/wasm/test.html` demo that can be served alongside `wasm-pack build --target web` output. (#36)
- Web frontend scaffold under `web/` — Astro 4 static build with a Solid.js island and Tailwind CSS. The scaffold loads `orber-wasm` via `wasm-pack --target web` output and confirms `init_panic_hook` runs on mount; the UI is a placeholder file picker. Deployed via `wrangler pages deploy dist` (no SSR adapter needed for `output: 'static'`). `npm run wasm:build` rebuilds `crates/wasm` into `web/src/wasm/`. (#37)
- Web frontend 10-image batch generation GUI. Drop an image to auto-generate 10 PNG previews via `orber-wasm.generate_batch` (driven by `DEFAULT_VARIATIONS`), shown as a responsive grid. Heart-toggle to select tiles; download single (direct PNG) or multiple (ZIP via JSZip). The only user control besides drop is an aspect toggle (縦長 540×960 / 横長 960×540). New helper `web/src/lib/decodeImage.ts` decodes `File` → RGB bytes via canvas. (#38)

## [0.3.0] - 2026-04-28

### Added
- New CLI flag `--count <N>` (1..=200, default 20) that controls how many orbs are visible on screen at once. The K colors picked by k-means are *expanded* into N orbs by weight-proportional color sampling and per-orb cross-axis scattering, so a single image can fill roughly 70% of the frame at the default count. (#41)
- All-orb breathing modulation in **three independent axes**: radius ±10%, blur ±15%, opacity ±5%. Each orb's three axes are decorrelated by separate seed-derived phase offsets, and each axis loops once per clip duration. (#41)
- `OrbStyle` enum (`Rim` / `Soft`) and `render_one_orb` per-orb rendering helper. Each orb is assigned a style deterministically from the seed (≈50:50), so a single frame mixes the rim-emphasized look with plain soft gradients. (#41)
- Per-orb integer speed multipliers (`1x` / `2x`) assigned deterministically from the seed. Combined with the `MotionSpeed` cycle count (`VerySlow` / `Slow` = 1 / 2), effective traversal counts spread over `{1, 2, 4}` per clip — orbs visibly move at varied paces while pixel-exact loop closure at `t=0 ≡ t=1` is preserved (integer × integer cycles). (#41)
- Off-screen wrap buffer: each orb's progress range is extended from `[0, 1]` to `[-r, 1+r]` (where `r` is its radius normalized by the progress-axis length). Orbs now spawn and despawn fully off-screen, so the wrap moment is invisible — the seam at `pos = 1+r → -r` happens beyond the canvas edges. (#41)
- New cluster helpers `cluster::derive_background_rgba` and `cluster::drop_dominant`. The dominant (highest-weight) cluster becomes the canvas color and is dropped from the orb pool, so the input image's most prevalent color is no longer drawn as an orb on top of itself. (#41)

### Changed
- **BREAKING**: Motion model rebuilt as a one-way conveyor belt. Each clip flows in a single direction (left→right / right→left / top→bottom / bottom→top) with all orbs traveling the same way. Orbs no longer reflect or oscillate; they exit one edge and re-enter from the opposite edge (wrap loop). (#41)
- **BREAKING**: `MotionShape` (`Still`, `Lissajous`, `Vertical`, `Horizontal`, `Diagonal`, `Breathe`, `Twinkle`) is **removed**. The standalone `Breathe` / `Twinkle` modes are gone — breathing is now an automatic effect applied to every clip.
- **BREAKING**: Old `MotionSpeed` variants (`Subtle` / `Slow` / `Lively`) are **removed** in favor of `VerySlow` / `Slow`, defined as integer screen-cross counts per clip (1 / 2). Pixel-exact loop closure at `t=0 ≡ t=1` is preserved.
- **BREAKING**: New `MotionDirection` enum (`LeftToRight`, `RightToLeft`, `TopToBottom`, `BottomToTop`) added.
- **BREAKING**: CLI flags `--motion`, `--motion-shape`, `--motion-speed` are **removed** and replaced with `--direction <lr|rl|tb|bt>` and `--speed <very-slow|slow>`.
- **BREAKING**: Animation boundary mode switched from `clamp` to wrap (`rem_euclid`).
- **BREAKING**: `DEFAULT_VARIATIONS` preset rebuilt around direction × speed × `count` × `orb_size` × `blur` (color is no longer a preset axis). Output filenames change to `snapshot_lr_dense`, `snapshot_rl_huge`, `snapshot_tb_fine`, `snapshot_bt_blurry`, `flow_lr_slow`, `flow_rl_very_slow`, `flow_tb_dense`, `flow_bt_blurry`, `flow_lr_dense_small`, `flow_rl_huge`. (#41)
- **BREAKING**: `VariationSpec` now carries `count: usize` instead of the old color/cluster fields. `VariationSpec.shape` / `VariationSpec.speed` (old types) replaced with `direction` and the new `speed`.
- PNG single-output path now goes through `animate::render_frame(t=0)` instead of `render_static` so `--count` takes effect for stills as well as videos.
- Animated variation duration extended from 4000 ms to 8000 ms so the slow conveyor pacing reads as gentle.

### Removed
- **BREAKING**: `ColorMod` module is **deleted**. Hue shift, lightness bias, saturation modulation, and dominant cluster rotation are gone. The premise — that a single input photo should yield multiple recolored variations — was wrong: if you want different colors, feed in a different image. K-means palette colors are now used unmodified end-to-end. (#41)
- **BREAKING**: `VariationSpec` fields `hue_shift_deg`, `lightness_bias`, `saturation`, `dominant_rotation`, and `cluster_count` are **removed**. The `VariationSpec::color_mod()` method is also gone. The k-means K used by the variations path is fixed internally at 5.
- **BREAKING**: CLI flag `--background` is **removed**, along with the entire `background` module (`Background` enum, `resolve`, `BackgroundParseError`). The background color is now derived automatically from the input image: the dominant (highest-weight) k-means cluster becomes the canvas color, and the remaining clusters become the orb pool. A nightscape gives a black canvas with bright points; a daytime sky gives a sky-blue canvas with floating points; a beige interior gives a beige canvas with small accents. To change colors, change the input image. (#41)
- The `--background transparent` rejection branch for mp4/webm is gone (auto-derived backgrounds are always opaque, so the rejection branch became unreachable). (#41)

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
