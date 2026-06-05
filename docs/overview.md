# orber overview

`orber` turns a photo or short video into an abstract **orb mood** rendition — colorful, blurry light spheres that drift slowly. The original subject is intentionally lost; what survives is the *vibe* of the colors.

## Pipeline

```
input image / video
  ├─ (video only) extract representative frames via ffmpeg
  ├─ extract color clusters       → N representative colors  [implemented]
  ├─ place orbs                   → position, size, base color per orb  [implemented]
  ├─ (glyph / image) build SDF    → font outline or image silhouette → signed-distance field  [implemented]
  ├─ render frame(s)              → RGBA buffer, drawn on the GPU via WGSL (wgpu)  [implemented]
  ├─ (animated) interpolate       → frame sequence over time t  [implemented]
  └─ encode                       → PNG / MP4 / WebM / SVG / CSS  [PNG / MP4 / WebM / SVG / CSS implemented]
```

Since #225 the **GPU (WGSL via `wgpu`) is the only renderer**: every PNG / video /
variation frame the CLI emits is drawn on the GPU. There is no CPU pixel path and
no `--renderer` flag; if no GPU adapter is available the CLI exits with an error.
SVG / CSS output stays a separate vector / style export (no rasterization).

## Output formats

|              | Static            | Animated                            |
| ------------ | ----------------- | ----------------------------------- |
| **Raster**   | PNG, WebP         | MP4, WebM (vertical 9:16 by default)|
| **Style**    | CSS gradient (implemented) | CSS gradient + `@keyframes` (planned) |
| **Vector**   | SVG (implemented) | —                                   |

CSS / SVG output is attractive because it is essentially zero-byte, infinitely loopable, resolution-independent, and cheap to render in a browser compared to a video element.

## Parameters

The CLI exposes the following flags (run `orber --help` for the authoritative list):

- `--orb-size` — relative orb size multiplier (small = many tiny orbs, large = few soft blobs)
- `--blur` — blur intensity in 0.0..=1.0 (sharp ↔ fully diffused). In Glyph mode this controls the same edge falloff width used by plain orbs.
- `--count` — orbs visible on screen at once (1..=1024, default 20)
- `--count-preset` — `low` / `mid` / `high` shorthand (= 10 / 20 / 30). Mutually exclusive with `--count`.
- `--direction` — conveyor flow direction: `lr` / `rl` / `tb` / `bt`
- `--speed` — conveyor pace: `very-slow` / `slow` / `mid` / `fast` (cross counts per clip = 1 / 2 / 3 / 4)
- `--shape` — `orb`, `aquarelle` (watercolor bleed), `glyph` (text character), or `image` (silhouette from `--image-mask`)
- `--glyph-char` — single character used when `--shape glyph` (default `☆`)
- `--image-mask` — silhouette image used when `--shape image` (the *shape* source; `--input` stays the *color* source). Raster only (PNG/JPEG/…); SVG is web-only
- `--softness` — blur/edge-softness preset: `low` / `mid` / `high` (default `mid`, existing behavior)
- `--saturation` — saturation multiplier
- `--duration-ms` — clip duration for animated outputs
- `--seed` — random seed for reproducibility
- `--variations N --output-dir DIR` — emit a curated set of N alternate looks for the same input (direction × speed × count × size × blur combinations)

Background color is not a CLI flag — it is derived from the input image (see "Background derivation" below).

## Background derivation (v0.3.0)

There is no `--background` flag. The background color is **derived automatically**
from the k-means clusters of the input image:

- the dominant cluster (highest weight) becomes the canvas color (alpha = 255)
- the remaining K − 1 clusters become the orb pool
- if k-means returns zero clusters (degenerate input), the canvas falls back to
  opaque black

Internally `extract_clusters` downsamples the input to a longest-edge of 256 px
(Triangle filter, aspect preserved, with a minimum-edge floor of 8 px) before
running k-means. The output (centroids in normalized [0,1] coordinates, LAB
colors, weight ratios) is scale-invariant, so this purely reduces compute cost
on large input photos without changing the visual result. Both the CLI and the
web GUI share this path; the web GUI additionally pre-resizes on the JS side
to keep the JS→Worker→wasm RGB transfer constant.

Concretely:

- a nightscape (mostly dark sky) → black canvas + bright neon points
- a daytime sky → sky-blue canvas + clouds / silhouettes drifting on it
- a beige interior → beige canvas + small accent-color orbs

The dominant color is the most "this is what the photo looks like" channel, so
making it the canvas and letting the sub-colors float as orbs produces a
composition that already feels right without parameter tuning. To get a
different canvas, feed in a different image — the design intentionally has no
override path.

Auto-derived backgrounds are always opaque (alpha = 255), so animated outputs
(`mp4` / `webm`) never collide with `yuv420p`'s lack of alpha.

## Motion model (v0.3.0)

Animated outputs use a **one-way conveyor belt**. The whole clip flows in exactly one
direction (`lr` / `rl` / `tb` / `bt`); orbs do not reflect, oscillate, or return to
their start. When an orb exits one edge, a fresh orb enters from the opposite edge
— but the seam happens **fully off-screen**: each orb's progress range is `[-r, 1+r]`
where `r` is its radius normalized by the progress-axis length, so orbs spawn and
despawn beyond the canvas edge and never visibly pop in or out. Each orb has a
randomized initial phase so the field looks scattered rather than synchronized.

A baseline breathing is applied to every orb automatically — there is no opt-in flag.
The breathing has **three independent axes**, each driven by its own seed-derived
phase offset and looping once per clip duration:

- radius: ±10%
- blur: ±15%
- opacity: ±5%

Each orb is also assigned an integer **speed multiplier** (`1x` / `2x` / `3x`)
deterministically from the seed, so individual orbs visibly travel at different
paces inside the same clip. Combined with the global `--speed` cycle count
(`very-slow` / `slow` / `mid` / `fast` = 1 / 2 / 3 / 4), per-orb effective
traversal counts spread over `{1×cycle, 2×cycle, 3×cycle}` per clip. Because
every factor is an integer, the loop closure at `t = 0 ≡ t = 1` remains
pixel-exact regardless of which cycle count is chosen — Phase A added the
`Mid` (3) and `Fast` (4) variants without breaking that invariant.

The full `--speed × per-orb multiplier` matrix of effective screen crossings
per clip:

| `--speed` (cycle) | `1x` | `2x` | `3x` |
|---|---|---|---|
| `very-slow` (1) | 1 | 2 | 3 |
| `slow` (2)      | 2 | 4 | 6 |
| `mid` (3)       | 3 | 6 | 9 |
| `fast` (4)      | 4 | 8 | 12 |

All twelve cells are integer products, so each is independently a valid loop
period of the clip; the union is also an integer-period system, which is what
guarantees pixel-exact wrap at `t = 1`.

`--speed` itself is the global cycle count (1 / 2 / 3 / 4 screen-crosses per
clip for the slowest orbs). Real-time pacing is set by `--duration-ms`:
`--speed slow --duration-ms 8000` means the slowest orbs cross the screen twice
in 8 seconds (4 s/cross), with `2x` orbs proportionally faster.

> Note: the aquarelle shape uses the legacy `[0, 1]` wrap. Its bleed / bloom / halo
> textures clip cleanly enough that the off-screen wrap buffer would interfere with
> the halo rendering. The `[-r, 1+r]` off-screen wrap described above applies to
> the `orb`, `glyph`, and `image` shapes.

## Orb count and visual mix (v0.3.0)

The k-means palette gives K colors (5 in the variations path). `--count <N>`
*expands* those K colors into N orbs by:

1. weight-proportional color sampling (more dominant clusters spawn more orbs)
2. per-orb cross-axis scattering (orbs spread across the full width/height instead of
   sticking to cluster centroids)

Each orb is also assigned one of two visual styles deterministically from the seed:

- `Rim` — a mid stop drops the gradient to half-alpha, producing a ring-emphasized look
- `Soft` — center → transparent monotonic fade with no mid stop

The two styles mix roughly 50:50 inside a single frame, so some orbs look like
ring-haloed lights and others like plain soft glows.

> Note: the aquarelle shape ignores `--count` (palette-only rendering). It renders
> one orb per k-means cluster so the bleed / bloom / halo texture set stays coherent.

## Variation preset (v0.3.0)

The `--variations` mode draws from a 10-entry hand-tuned preset that combines five
independent axes — conveyor direction, conveyor speed, orb count, orb size, and blur.
Color is **not** an axis: the input image's k-means palette is used unchanged across
all ten outputs. Differentiation comes from layout (count / size / blur) and motion
(direction / speed).

- 4 stills: `snapshot_lr_dense`, `snapshot_rl_huge`, `snapshot_tb_fine`,
  `snapshot_bt_blurry`
- 6 animations (8 s each): `flow_lr_slow`, `flow_rl_very_slow`, `flow_tb_dense`,
  `flow_bt_blurry`, `flow_lr_dense_small`, `flow_rl_huge`

Stills are not a separate static-only code path — they are the `t = 0` frame of the
conveyor, so orbs are phase-scattered and the off-screen wrap buffer means a fraction
of the requested `--count` will sit just outside the visible area, matching the
visual language of the videos.

## Video input — color track (#7)

When `--input` is a video file (`.mp4` / `.webm` / `.mov` / `.mkv` / `.m4v` / `.avi`),
orber switches to a **color-track** path. The orb **positions stay frozen** (decided
once from the first frame's k-means cluster centroids); only the **colors evolve**
over the output duration.

How it works:

1. `ffprobe` reads the video duration; `ffmpeg` is invoked once per sample to write
   `N = 20` PNG frames evenly spaced in time (`t_i = i / (N-1) * duration`).
   Frames are written to a `tempfile::TempDir` that is deleted on function exit.
2. The first sample is k-means clustered (k = 6) to produce the **template clusters**
   (= position / weight basis for the orb pool).
3. Each subsequent sample is k-means clustered, and its clusters are **greedy-matched
   to the template by LAB ΔE76 distance**. The matched colors form one **color track**
   per template cluster (`tracks[cluster_idx][sample_idx]`).
4. The CLI renders the output through the existing animate pipeline with
   `AnimateOptions.color_tracks = Some(tracks)`. For each frame at output time
   `t ∈ [0, 1]`, every orb's `cluster.color` is replaced by
   `interpolate_color_track(tracks[cluster_idx], t)` (linear lerp between adjacent
   samples, endpoints clamped).

Critically, **input duration only sizes the color sample sequence** — the output
length is set independently by `--duration-ms`. A 3-minute clip rendered as a
30-second orb will play the input's color evolution at 6× speed; a 10-second clip
rendered as a 5-minute orb will play at 0.5× speed. Position and motion (`--speed`
/ `--direction` / `--count`) remain unaffected.

`--output FILE.mp4` / `FILE.webm` produces a video; `FILE.png` produces a single
frame at `t=0` (= first sampled frame's color). Other modes are rejected with a
clear error. Static-image input continues to flow through the unchanged image
path; no regression for existing callers.

## Video input — keyframe interpolation (#33)

`--input-mode keyframe` switches the video pipeline to a **keyframe** path that
interpolates **color + position + weight** between sampled keyframes, rather than
just colors with positions frozen. Pass `--keyframes N` to control how many
keyframes are sampled (default 8, clamped to a minimum of 2 since one keyframe
cannot be interpolated).

How it differs from the color-track path (#7):

1. The video is sampled at `N` evenly-spaced keyframe times rather than 20 fixed
   color samples. Each keyframe is independently k-means clustered (k = 6).
2. Clusters are tracked across keyframes by LAB ΔE76 greedy matching against the
   first keyframe's cluster colors. If a match is missing for some keyframe, the
   previous keyframe's `(color, centroid, weight)` is held in place (**hold-last
   fallback**) so interpolation does not break; the next successful match
   resumes normal lerp.
3. At output time `t ∈ [0, 1]`, each orb's `(color, centroid, weight)` is taken
   from `interpolate_keyframe_track(tracks[cluster_idx], t)` — a pure linear lerp
   between the two adjacent keyframes by the keyframe's stored normalized time
   (endpoints clamped, NaN-safe, divide-by-zero defended).

How `centroid` drift becomes visible depends on the orb shape:

- **Aquarelle** shape uses `cluster.centroid` directly for orb placement, so the
  input video's compositional motion is fully reflected in the output.
- **Orb** shape blends `cluster.centroid` drift with the per-orb seeded
  `cross_axis` at 50:50 to keep the input video's compositional motion visible
  without losing the per-orb scatter that prevents stripe artifacts. With
  `--input-mode color-track` (#7) or still-image input, the orb uses `cross_axis`
  alone (existing behavior preserved).

Output length is still set entirely by `--duration-ms`. A 3-minute clip
rendered as a 10-second orb compresses the input's mood; a 10-second clip
rendered as a 1-minute orb stretches it. Determinism: same input + same
`--duration-ms` + same `--seed` produces the same output bytes.

`--input-mode keyframe` requires video input — passing it with a still image
yields an explicit error rather than silently degrading. The default
`--input-mode color-track` keeps existing #7 behavior.

## Use cases

- Background plates for video edits
- Streaming "be right back" idle screens
- Social story / TikTok / Reels backgrounds
- Phone or desktop wallpapers from your own photos
- Privacy-friendly mood snapshot of a place (looks nothing like the original)

## Non-goals (for the prototype)

- Web frontend (planned later as a separate effort)
- WASM build (planned later)
- Realtime / interactive editing (CLI-only for now)

## Relationship to aquarelle

The aquarelle (watercolor bleed) shape generator now ships as its own external
crate at [`kako-jun/aquarelle`](https://github.com/kako-jun/aquarelle) and is
pulled in via `aquarelle = "0.2"` in `Cargo.toml`. Since #235 it backs **only**
the `OrbShape::Aquarelle` shape:

- **`OrbShape::Aquarelle`** follows the crate's four-layer `render_aquarelle_orb`
  model. The GPU shader (`orb_aquarelle.wgsl`) evaluates those layers analytically;
  the ChaCha8 RNG / HSL color math is run host-side in the parameter pack so it stays
  byte-identical to the crate, and the resulting centers / radii / colors are uploaded.
- **`OrbShape::Glyph` and `OrbShape::Image`** no longer run an aquarelle bleed
  pass. As of #235 they are fed to the **same orb mechanism** as `OrbShape::Orb`:
  the SDF sample becomes the normalized distance `r`, which the unified shader
  (`orb.wgsl`, SDF variant) blurs with the orb's `falloff_curve` / 3-axis breath /
  Skia-lowp compositing in a single pass. The glyph / image silhouette is simply a
  different shape fed to the orb — a `●` glyph looks like a plain orb, a `▲` blurs
  while keeping its triangular form. The old `render_aquarelle_bleed_pass`-derived
  2nd pass (`orb_glyph_bleed.wgsl`) and its bleed/halo are removed; "bleed" is the
  Aquarelle shape's domain only. Both glyph and image share the GPU SDF render path
  (`render_frame_glyph` / `render_frame_image`); the only difference is the SDF
  source (a bundled font glyph vs. an image silhouette from `--image-mask`).

## Workspace layout

Since `v0.3.0` (Issue #35) the repository is a Cargo workspace with two crates:

- **`orber-core`** (`crates/core/`) — pure rendering library: cluster extraction, the GPU (WGSL / `wgpu`) renderer, per-orb parameter packing, glyph / image SDF generation, animation frame parameters, and CSS / SVG output. No filesystem I/O and no subprocess. Builds for `wasm32-unknown-unknown` so the Web frontend can call the parameter / SDF helpers directly (the wasm build supplies data; production rendering on the web currently runs in WebGL2). Since #229 the `gpu` feature also builds on wasm32 (WebGPU backend only — no `webgl` fallback): `GpuRenderer::new_async()` (headless) / `GpuRenderer::from_device_queue()` (surface-compatible bring-up, #230) plus the `*_to_view` methods draw any shape into an externally supplied `wgpu::TextureView` (the browser surface-present seam), while the `RgbaImage` read-back API stays native-only. Since #230 `orber-wasm` ships a minimal WebGPU canvas path on top of that seam (`gpu_init` / `gpu_set_render_data` / `gpu_render` / `gpu_resize`, Orb only; dev page `web/src/pages/gpu-lab.astro`).
- **`orber`** (`crates/cli/`) — the CLI binary. Owns `image::open`, `tempfile`, and the `ffmpeg` subprocess used for video output. Depends on `orber-core` for all rendering.

User-facing CLI behavior is unchanged.

## Web GUI rendering pipeline

The web frontend (`web/`) renders 12 tiles per drop (8 stills + 4 animated)
**entirely on the GPU via WebGL2**. wasm is used only for kmeans color
extraction and for packing per-orb parameters; the per-pixel composition runs
in a fragment shader. The pipeline is split between a **main thread** (UI +
DOM) and a **dedicated Worker thread** (wasm + WebGL2 + WebCodecs):

```
[main thread]                          [worker thread (orberWorker.ts)]
  Studio.tsx                             wasm-bindgen loaded once
   ├ runBatch                            ├ wasm.get_render_data(spec_idx, w, h)
   │   └ workerGenerateOne(i) ────────→  │   └→ Float32Array (orb params)
   │                                     ├ OffscreenCanvas + WebGL2 fragment
   │                                     │  shader renders the t=0 frame
   │                                     ├ canvas.convertToBlob('image/png')
   │                                     │   └→ PNG bytes (Transferable)
   ├ animate phase                       ├ wasm.get_render_data(spec_idx, w, h)
   │   └ workerAnimateOne(i) ─────────→  │   for i in 0..192:
   │                                     │     - shader.renderFrame(t = i/192)
   │                                     │     - new VideoFrame(canvas) → encode
   │                                     ├ WebCodecs VideoEncoder
   │                                     │   (codec probe: H.264 → VP9 → AV1,
   │                                     │    hwAccel probe: prefer-hardware → no-preference; #196)
   │                                     │   └→ mp4 Blob (Transferable)
   └ DL high-res                         └ same APIs, with width/height = 1080×1920
       └ workerGenerateOne / workerAnimateOne (per selected index)
```

The source RGB buffer is uploaded once via `workerSetSource` and cached in the
Worker; subsequent `get_render_data` calls reference the cached kmeans
clusters, so multi-megabyte arrays are not copied per call. The WebGL2 context
and OffscreenCanvas are also cached per resolution and reused across calls.

**Source downsampling for kmeans.** The dropped image is decoded and immediately
resized to a longest-edge of 256 px (aspect preserved) before the RGB buffer is
handed to the Worker. The full-resolution image is never seen by wasm or the
shader, because the renderer only needs the kmeans cluster colors — the actual
canvas dimensions for orbs are controlled by `width` / `height` (preview or
download), not the source size. Downsampling fixes three problems at once:
the JS→Worker→wasm transfer of the RGB array becomes a constant ≤196KB instead
of scaling with the input photo (4032×3024 was 36MB and the per-tile copy cost
dominated on Android), kmeans itself runs on a tiny pixel set, and the wasm-side
`SOURCE_CACHE` fingerprint becomes stable across calls.

**Why WebGL2.** The previous implementation rendered every pixel on the CPU
inside wasm and ran each animation frame through `RGBA → ImageData →
createImageBitmap → VideoFrame` before encoding. At 1080×1920 × 192 frames the
per-pixel CPU cost dominated and a single download tile took several minutes.
The fragment shader runs in parallel on the GPU and `new VideoFrame(canvas)`
hands the rendered surface directly to the encoder, eliminating per-frame
transfer cost entirely. End-to-end download time for one hi-res animated tile
drops to a few seconds (encoder flush dominates; renders themselves are
sub-millisecond).

**Preview vs. download resolution.** Tiles are rendered at **540×960** (portrait)
or **960×540** (landscape) for the preview grid — light enough to keep mobile
generation fast. When the user clicks Download, the Worker re-renders only the
selected tiles at **1080×1920** / **1920×1080** (4× resolution, same 9:16 / 16:9).
Determinism is provided by `random_batch_specs(seed, total, still_count)` in
`crates/core::variations`: the same `baseSeed` reproduces the exact same
variation specs at any resolution.

**Video tile direction & speed assignment.** Each of the 4 video tiles in a
batch gets its `direction` and `speed` deterministically assigned by index, not
randomly. `GUI_VIDEO_DIRECTIONS = [LR, RL, TB, BT]` and
`GUI_VIDEO_SPEEDS = [VerySlow, Slow, VerySlow, Slow]` guarantee that every
roll contains all 4 directions exactly once and a 2:2 mix of slow/very-slow,
so a batch never degenerates into "all 4 fast" or "all 4 slow". Static tiles
keep their spec values; only the animated tiles get the override (#59 / #77).
The wasm helpers `direction_for_spec_idx` / `speed_for_spec_idx` apply the
same logic inside `get_render_data`, so the still tile (rendered at `t = 0`)
and the animated mp4 (rendered at `t ∈ [0, 1)`) are guaranteed to start from
the exact same frame.

**Clip duration.** Animated tiles are **8 seconds long at 24 fps** (192 frames).
Combined with the assigned speeds above, VerySlow tiles cross the screen once
in 8 s — slow enough to feel "drifting", appropriate for use as overlay /
background plates beneath text.

**Browser requirements.** OffscreenCanvas / WebGL2 / VideoEncoder / VideoFrame
in Worker context. iOS Safari 16.4+, current Android Chrome / Firefox 130+.
There is no fallback path for older browsers — the GUI shows an error if
WebCodecs is unavailable. WebGL2 support is a strict superset of WebCodecs in
practice, so any browser that can run the encoder can also run the renderer.
The animated-tile path requires *some* `VideoEncoder` codec to be available:
H.264 → VP9 → AV1 are probed in that order via
`VideoEncoder.isConfigSupported`, falling back from `prefer-hardware` to
`no-preference` per candidate (#196). Linux Chrome / Edge / Firefox ship
without an H.264 encoder but accept VP9 / AV1, so this probe is what keeps the
animated tiles working there. If every candidate is rejected the per-tile
encode throws and Studio surfaces the existing
"some tiles could not be animated" warning while keeping the stills.

**Progressive UX.** While the Worker is busy:

- An empty grid of 12 **skeleton tiles** appears the moment the user drops an
  image, so the layout is fixed before any pixel is rendered.
- Stills replace their skeleton one by one as PNG bytes arrive from the Worker.
- Video tiles show a softer shimmer (`.skeleton-soft`) plus an "Animating" badge
  on top of the still PNG until the mp4 is delivered, signalling that they will
  start moving shortly.

**Re-roll cancellation.** When the user re-rolls (or drops a new image / flips
aspect) while the previous batch is still in flight, `runBatch` terminates the
Worker (`worker.terminate()`) and respawns it with a fresh wasm instance and
WebGL2 context. A logical generation guard (`runGen` / `myGen`) alone is not
enough because the in-flight render + encode loop keeps running to completion
otherwise, doubling GPU/CPU usage and delaying the new batch. After respawn the cached source RGB is invalidated and re-uploaded on
the next `workerSetSource`. The cost (a small wasm re-init) is paid only when
re-rolling mid-batch; single, sequential runs see no overhead. Note the
in-flight check excludes `init` / `setSource` RPCs so a re-roll triggered
during early-mount initialization does not respawn the worker prematurely.

## Design system & i18n (web GUI)

The web GUI (`web/`) follows a single design system documented in `DESIGN.md` at the
repository root. Theme: black-canvas gothic with glass-only buttons (no accent
hue, no shadows, no decorative motion). Tailwind theme tokens in
`web/tailwind.config.mjs` (`fg`, `glass-bg`, `hairline`, etc.) are the only place
where chrome colors are defined; raw `red-*` / `emerald-*` Tailwind classes never
appear in components.

All visible strings live in `web/src/lib/strings.ts` and are accessed via
`t('key')`. Language is auto-detected from `navigator.language`: a Japanese
locale renders Japanese, every other locale renders English. There is no
language picker — users do not choose. The `<html lang>` attribute is set
pre-hydration by an inline script in `Base.astro` so screen readers pick the
right voice from first paint, and the Solid `lang` signal is synced
post-hydration by `Subtitle.tsx` for reactive UI text. The Solid `lang` signal
initializes to `'en'` on both SSR and client (no `window` — note that Node 22+
exposes a global `navigator`, so SSR detection keys off `window` rather than
`navigator`) so the SSR HTML and the post-hydration DOM agree, then a
`queueMicrotask` in `strings.ts` flips the signal to `detectLang()` after
hydration, triggering a reactive re-render of every `t()` call across all
islands at once. This avoids the hydration-mismatch bug (#161) where Solid's
"DOM already exists, skip re-render" optimization left some islands stuck on
the SSR English while others switched to Japanese. `Subtitle.tsx` `onMount`
keeps a safety-belt re-sync.

## Glyph rendering pipeline (Phase A)

`--shape glyph` swaps each orb for a **glyph character** filled with the orb's
color. The pipeline:

1. A bundled font subset — **Noto Sans Symbols 2** (~177 KB, embedded with
   `include_bytes!` from `crates/core/assets/fonts/NotoSansSymbols2-Regular.ttf`,
   © Google Inc., SIL Open Font License 1.1; full license text shipped at
   `crates/core/assets/fonts/OFL.txt`) — covers ASCII, digits, punctuation,
   arrows, geometric shapes, Dingbats, and supplemental symbols. Hiragana,
   kanji, emoji, and anything else outside the subset is **silently skipped**
   rather than drawing tofu / `.notdef`, so unknown inputs never visually break
   a render. The CLI emits a one-shot stderr warning at startup whenever
   `--shape glyph --glyph-char <CH>` is invoked with a `CH` the bundled font
   does not cover, so users see why their output is empty.
2. The font face is parsed once via `ttf-parser` (`0.25`) and cached in a
   process-global `OnceLock<Face<'static>>` per `GlyphFontId` enum variant.
   Going through an enum + global cache (instead of `Arc<Face>` per orb) keeps
   the `Glyph` arm a light, cheaply-cloneable `{ ch, font }` pair. (`OrbShape`
   itself became `Clone` rather than `Copy` once `#217` added an
   `Image { sdf: Arc<[u8]>, size }` arm; the `Arc` clone is a refcount bump, so
   `OrbShape` still flows through the spec / per-orb param paths cheaply.)
3. For each orb's glyph, the outline is extracted via `Face::outline_glyph` and
   walked into a `zeno` path; `zeno::Mask` rasterizes it to an alpha-coverage
   silhouette (pure Rust, wasm-capable — replaced the earlier Skia-based path in
   #223). The silhouette is turned into a cached signed-distance field via the
   shared `mask_to_sdf` (EDT → signed-unit → u8).
4. The glyph is drawn from that SDF on the GPU through the **unified orb
   mechanism** (#235): the SDF orb variant of `orb.wgsl` bilinear-samples the SDF,
   turns the signed-distance value into the normalized distance `r = 1 - signed_unit`,
   and feeds it to the **same** Rim/Soft `falloff_curve` / 3-axis breath /
   Skia-lowp premultiply compositing the plain `orb` shape uses. So **`--blur` and
   `--softness` affect Glyph mode** with the same edge-falloff meaning rather than a
   hard text fill, and the glyph blurs exactly like an orb. Each orb's rotation
   (#136) is applied to the SDF sample coordinates in the shader (before sampling).
5. That is the **only** pass. Since #235 there is no aquarelle bleed/halo 2nd pass
   for Glyph / Image — the old `orb_glyph_bleed.wgsl` group is removed. The glyph /
   image silhouette is just a different shape fed to the orb (a `●` glyph looks like
   a plain orb; a `▲` blurs while keeping its triangular form). "Bleed" is the
   Aquarelle shape's domain only.

The on-disk font asset is the only payload added by Phase A; the `ttf-parser`
dependency itself is small and pure-Rust (no shaping, no FreeType).

## Image rendering pipeline (#217)

`--shape image` reuses the entire Glyph SDF pipeline above — only **where the SDF
comes from** changes. Instead of rasterizing a font glyph, `--image-mask <PATH>`
is decoded (CLI-side, full `image` decoders; raster only, **SVG is web-only**) and
turned into a silhouette SDF by `orber_core::glyph::image_rgba_to_sdf`, a 1:1 port
of the Web GUI's `generateImageSdf`:

1. The mask is letterboxed (aspect-preserving "contain" resample) into a square,
   and only the drawn rectangle is evaluated (letterbox margins stay background).
2. If ≥1% of the drawn rectangle has `alpha < 255`, the **alpha** channel selects
   the silhouette (`alpha >= 128` = inside). Otherwise the image is opaque and
   **luminance** (`Y = 0.299R + 0.587G + 0.114B`) is thresholded at the mean with
   **auto-polarity** (the minority region is the subject, so dark-on-light and
   light-on-dark both work without a flip flag).
3. A blank / single flat-color mask (`inside == 0` or all-inside) has no usable
   contrast and is rejected — the CLI exits with an explicit error (no panic).
4. The resulting binary mask goes through the **shared** `mask_to_sdf` (the same
   EDT → signed-unit → u8 step the glyph SDF uses), so the output is byte-format
   identical to a glyph SDF. From there it rides the same GPU SDF path —
   `render_frame_image` reuses the glyph shader + bleed 2nd pass with the supplied
   SDF texture — so `--blur` / `--softness` / the bleed pass behave identically.

The image SDF is built once and shared via `Arc<[u8]>` inside `OrbShape::Image`.
The Web GUI's `OrbShape` image arm uses the same `shape_id == 1` shader path as
glyphs, which is why CLI and Web stay visually consistent.

## Softness axis

`--softness {low, mid, high}` is a **single user-facing axis** that bundles
three internal knobs (alpha, blur offset, glyph/image edge softness) into a
3-stop preset.

| preset | alpha | blur | edge softness | use case |
|---|---|---|---|---|
| `low` | 1.0 | 0.0 | 0.3 | crisper baseline (was the legacy default before #205) |
| `mid` (default) | 0.55 | +0.25 | 0.6 | new default — orb-like softness across all shapes |
| `high` | 0.35 | +0.5 | 1.0 | maximum blur (sit underneath text overlay or for cinematic mood) |

**#205 — Scale shifted toward blurry.** The original Phase A preset put
`mid` at the legacy default (alpha=1.0 / blur+0.0) and `low` at a sharper
extreme (blur-0.25). After comparing the Web GUI Glyph / image arm against
the Circle arm we settled on "orb-like softness as the new baseline", so
the whole table shifts upward: the old `mid` becomes the new `low`, the old
`high` becomes the new `mid` (default), and a brand-new `high` extends the
soft end. The old sharp `low` (`blur_offset = -0.25`) is **retired** —
nothing in the GUI used the extra sharpness it provided.

The `edge softness` column drives the Glyph / image arm's smoothstep mask
(`smoothstep(-edge_softness, edge_softness * 0.5, signed_unit)`) in the
fragment shader. The Circle arm uses Euclidean distance + `falloff_curve`,
so it is unaffected by this column. Higher values produce a softer
silhouette outline. The full transition width in `signed_unit` space is
`1.5 * edge_softness` (from `-edge_softness` to `0.5 * edge_softness`),
which projects to roughly the following fraction of orb radius:

| preset | edge_softness | full transition width | half-width |
|---|---|---|---|
| `low` | 0.3 | ~7.5% of orb radius | ~3.75% |
| `mid` | 0.6 | ~15% of orb radius | ~7.5% |
| `high` | 1.0 | ~25% of orb radius | ~12.5% |

Note: edge_softness is consumed only by the WebGL2 fragment shader
(Glyph/image arm). The native WGSL Glyph/image path achieves the analogous
softness via `blur_offset` and the post-render aquarelle bleed pass
(#195/#199). The two shader implementations differ slightly but target the
same visual goal.

The preset is implemented as `SoftnessPreset { Low, Mid, High }` in
`crates/core/src/style.rs`. The values flow into the WebGL2 path via
`pack_render_data_for_webgl` header slots 9 (`alpha_mul`), 5 (base blur after
`+ blur_offset`), and 12 (`edge_softness`, #205).

## Phase B — Web GUI parity (#55)

Phase B propagates the four CLI advanced axes to the Studio surface and
WebGL2 fragment-shader path so a browser user can drive shape / count /
speed / softness without dropping to the terminal.

- `WasmParams` gains four optional string fields (`glyph_char`, `count_preset`,
  `speed_preset`, `softness_preset`) all defaulting to `""` (= "use the spec
  / Phase A behaviour"). `parse_shape` accepts `"glyph"` in addition to
  `"orb"` (renamed from `"circle"` in #235).
- The browser now bakes Glyph **signed-distance fields** via
  `orber_core::glyph::render_glyph_sdf(font, ch, size) -> Vec<u8>`. The wasm
  wrapper exports it as `get_glyph_sdf(ch, size) -> Uint8Array` and caches per
  `(font_id, ch as u32, size)`. The browser worker uploads the SDF exactly
  once per `(ch, size)` change to a `gl.R8 / gl.RED` texture and re-uses it
  across the 96-frame `<video>` encode loop; subsequent frames only update
  `u_t`.
- The fragment shader gains `u_glyph_sdf: sampler2D`, `u_shape_id: int`
  (`0=Circle`, `1=Glyph`), and a per-orb rotation lane
  (`base_angle`, `rot_speed_signed`). Circle still computes `r` from
  `distance(center, px) / radius`; Glyph computes `r` from the sampled SDF and
  then feeds the **same rim/soft falloff curve**. Because `rot_speed_signed`
  is an integer multiple of the existing `speed_mult`, glyph rotation stays
  loop-closed at `t = 0 ≡ 1`.
- **#198 → #201 → #203 — Glyph/image softening converges to "SDF mask × Circle
  profile".** The Glyph arm (`u_shape_id == 1`, which also handles uploaded
  images via the shared `jsGlyphSdf` path) originally fed only `r_sdf` into
  `falloff_curve`, so the soft falloff was confined to the SDF's UV box and
  the result looked harder than Circle. Three rounds of revision followed.

  **#198 (r-max)** tried `r = max(r_sdf, r_euclid)` fed into `falloff_curve`
  once, but outside the SDF transition `r_sdf > 1` dominated the max and the
  halo was killed before `r_euclid` could contribute.

  **#201 (alpha-max)** switched to computing two alpha contributions and
  `alpha = max(alpha_sdf, alpha_euclid)`, which restored the Glyph='●' halo,
  but for `A` / image silhouettes the saturating `alpha_euclid` core erased
  the inner Circle-style fade and produced a circular halo around every
  silhouette — destroying the shape's individuality.

  **#203 (mask × profile)** splits responsibilities: the SDF becomes a pure
  shape mask via `sdf_mask = smoothstep(-edge_softness, edge_softness * 0.5,
  signed_unit)` (with mask=0 for UV outside the box), and the
  Circle-identical `radial_alpha = falloff_curve(style_bit, r_euclid, blur,
  opacity)` provides the soft center-to-edge profile. The final alpha is the
  product `radial_alpha * sdf_mask`. The smoothstep half-width was originally
  hard-coded at ±0.05; **#205** replaced it with the
  `u_glyph_edge_softness` uniform driven by `SoftnessPreset::edge_softness()`
  (Low=0.3 / Mid=0.6 / High=1.0). The lower bound is wide, the upper bound is
  pulled back to half that value so the mask still pinches off inside the
  SDF box (no mask=1 leaking far past the silhouette) while the outer
  fall-off broadens proportionally with softness. For Glyph='●' the SDF is a filled disk so the
  mask is 1 inside the silhouette and alpha collapses to
  `falloff_curve(r_euclid)` — visually very close to `shape=Circle` (the
  outermost ~10% fade ring is omitted because the glyph's SDF radius is
  ~0.9 × orb radius, so it is a close approximation rather than a
  byte-identical match). For Glyph='A' or image silhouettes the mask carries
  the shape and the Circle profile carries the soft fade, so no orb-shaped
  halo leaks outside the silhouette and the original individuality is
  preserved. The Circle arm (`u_shape_id == 0`), the `falloff_curve`
  function, and every uniform binding remain unchanged. This is the Web-side
  counterpart to the CLI-side bleed pass added in #195/#199: separate
  implementations, same visual goal of matching Glyph/image softness to Circle.
- `get_render_data`'s 16-word header schema reserves words 9 and 10 for
  `alpha_mul` and `shape_id` (previously zero-filled reserved words), word 11
  for `glyph_rotate` (#136), and word 12 for `edge_softness` (#205, drives
  the Glyph/image smoothstep). The per-orb 16-word slots use words 11 and 12
  for `base_angle` and `rot_speed_signed` (the remaining tail words stay
  reserved).
- Studio UI (#131): the collapsible Advanced section is gone. Instead, the
  four axes are always visible as flat control rows directly under the aspect
  toggles. Every control immediately re-runs the batch:
  - Shape = Orb / Glyph + inline glyph input
  - Count = Few / Standard / Many → 10 / 20 / 30
  - Speed = Slow / Standard / Fast → VerySlow / Slow / Mid
  - Softness = Low / Standard / High → sharp / identity / soft
  The old large "Roll / ガチャを引く" chip is removed; only a small reload
  icon remains at the bottom. The icon spins while decoding / generating /
  animating. For IME safety, glyph input suppresses worker RPCs during
  composition and trims to the first Unicode character on commit. A symbol
  picker is shown under the glyph row and is filtered to characters that the
  bundled wasm font can draw deterministically; the input field above the
  picker accepts **any Unicode codepoint** including emoji, kanji, and
  arbitrary symbols. Characters outside the bundled set are rasterized in the
  worker via `OffscreenCanvas` using the OS font stack (Apple Color Emoji /
  Segoe UI Emoji / Noto Color Emoji / Noto Sans Symbols 2 / system-ui), then
  converted to a Signed Distance Field by a JS-side Felzenszwalb–Huttenlocher
  Euclidean Distance Transform (`web/src/lib/jsGlyphSdf.ts`) so the resulting
  buffer is interchangeable with the wasm SDF path. Color emoji become
  silhouettes via alpha extraction, which lines up with orber's monochrome
  pipeline. The exact glyph shape depends on the OS font renderer, so the
  same 🐱 may differ between Mac and Windows; this is accepted as the trade
  for accepting any character the user types. The shape segmented control
  has a third option, **Image**, which lets users upload an arbitrary picture
  (PNG / JPG / WebP / GIF / SVG); the bitmap is silhouetted in-worker (alpha
  threshold for transparent images, otherwise a luminance threshold whose
  inside/outside polarity is auto-detected by minority count) and pushed
  through the same EDT → SDF pipeline as glyph rasterization, so the wasm
  rendering path is reused unchanged (`web/src/lib/jsGlyphSdf.ts:generateImageSdf`).
  Color information is discarded — orber's monochrome pipeline keeps only
  the shape.

## Transparent download bundle (#56)

The Web GUI offers an opt-in checkbox — "透過版を DL に含める / Include
transparent versions" — that ships transparent renders of every selected
tile alongside the regular background-filled outputs. The toggle sits
directly under the aspect row (it affects the download payload, so it
clusters with the other DL-affecting setting). It is OFF by default; flipping
it ON changes only the download path and never affects the on-screen tiles
(those keep their dominant-colour background fill).

When the checkbox is ON, the resulting `orber-{ts}.zip` looks like:

```
orber-{ts}.zip
├─ orber-{ts}_01.png ... _08.png       (background-filled stills, unchanged)
├─ orber-{ts}_09.mp4 ... _12.mp4       (background-filled videos, unchanged)
└─ alpha/
   ├─ orber-{ts}_01-alpha.png          (transparent PNG, lossless)
   ├─ orber-{ts}_01-alpha.webp         (transparent WebP, quality 0.9)
   ├─ ...
   ├─ orber-{ts}_09-alpha.mov          (transparent PNG-in-MOV, rgba lossless, JS-only muxer)
   ├─ orber-{ts}_10-alpha.mov
   ├─ orber-{ts}_11-alpha.mov
   └─ orber-{ts}_12-alpha.mov
```

Stills get both PNG (lossless reference) and WebP (smaller, lossy q=0.9) so
downstream workflows can pick either. Videos use **PNG codec muxed into a
QuickTime/MOV container** (#184) — the per-frame PNGs are packed as-is into
MOV sample chunks. #184 originally used ffmpeg.wasm purely as a muxer;
#192 replaced that with a **JS-only MOV muxer** (`web/src/lib/movMuxer.ts`,
~280 lines) since no actual encoding was happening anyway. MP4 has no
alpha track in any commonly supported profile, so the transparent video
format is necessarily different from the background-filled `.mp4`. Each
tile lands at ~60–70 MB (rgba 32-bit lossless × 192 frames × 540×960),
which is the trade-off for skipping the encode step entirely; the full
alpha bundle for one 12-tile batch fits in roughly a 250–300 MB ZIP.
Playback: VLC plays the file directly; **Windows Media Player does not**
(it has no PNG-codec demuxer). NLEs decode it natively as a lossless
intermediate.

Implementation notes:

- The alpha render path is bit-for-bit the same render pipeline as the
  background-filled path. The worker reuses the existing
  `wasm.get_render_data(...)` output and only patches header word 3
  (`bg.a` in 0..1) to `0` before calling `setRenderData`. Same `seed`,
  same `spec`, same SDF, same shader — only the background plane changes.
  This guarantees the alpha tile is the *same variation* as the visible
  preview tile, just unfilled.
- Encoders: `OffscreenCanvas.convertToBlob({type:'image/png'})` and
  `convertToBlob({type:'image/webp', quality:0.9})` for stills; for videos
  the worker renders each frame as a transparent PNG (`renderAlphaFrames`
  RPC, frames streamed to main via Transferable `alphaFrame` messages)
  and the main thread feeds them into the **JS-only MOV muxer** in
  `web/src/lib/movMuxer.ts` (#192). The muxer builds a complete
  QuickTime atom tree (`ftyp` / `moov` (`mvhd` / `trak` (`tkhd` /
  `mdia` (`mdhd` / `hdlr` / `minf` (`vmhd` / `dinf` / `stbl`)))) /
  `mdat`) and writes the per-frame PNG bytes into a single `mdat` chunk
  as-is. `stsd` advertises the `png ` sample format with depth 32 and
  `color_table_id = -1`. `stss` (sync sample) is intentionally omitted —
  per QuickTime spec, no `stss` means every sample is a sync sample,
  which is correct for PNG codec (no inter-frame prediction). The
  thin async wrapper `web/src/lib/encodeAlphaVideoWasm.ts` preserves
  the prior `encodeAnimationAlphaWasm` signature for callers and
  returns a `video/quicktime` `Blob`.
- Why PNG-in-MOV instead of VP9 alpha or VP8 alpha (path taken to get
  here): WebCodecs `VideoEncoder` with
  `{codec:'vp09.00.10.08', alpha:'keep'}` reports `supported: false` on
  Edge / Chrome on Windows, Android Chrome, Safari, and many other
  combinations because the underlying decision depends on OS-level codec
  backends, GPU acceleration state, and Chromium build flags. The
  earlier #56 implementation used WebCodecs and probed support at
  startup, but the probe was rejected on the majority of real-world
  devices so the feature was effectively unavailable. #184 first
  attempted ffmpeg.wasm + libvpx-vp9 (`yuva420p`), which hit
  `RuntimeError: memory access out of bounds` in single-threaded wasm
  even at half-res. The fallback to libvpx-vp8 alpha encoded without
  errors but produced silently-empty alpha planes (likely a bug in the
  single-threaded core's vp8 alpha path). PNG-in-MOV sidesteps both
  problems entirely: no encoder runs at all — neither in wasm nor in
  the browser — so neither memory pressure nor codec correctness can
  fail. The result is **reliable on every environment that can run
  JavaScript**, at the cost of much larger files (lossless raster,
  ~60–70 MB per tile).
- Why a hand-rolled JS muxer (#192): #184 used ffmpeg.wasm as the MOV
  muxer, which dragged in `@ffmpeg/ffmpeg` + `@ffmpeg/util` + a ~30 MB
  `@ffmpeg/core` wasm fetched cross-origin from jsdelivr, plus a
  Service Worker `CacheFirst` route, an idle-time `prefetchFfmpegCore`
  speculation, and a `saveData` / `2g` / `3g` saver guard. With no
  actual encoding happening, all of that machinery existed solely to
  pack PNG bytes into a known QuickTime atom layout. The MOV container
  spec is small enough to write directly: `movMuxer.ts` is ~280 lines
  and the atom tree it produces is byte-identical in shape to what
  ffmpeg's `movenc.c` emits for PNG-codec single-track video. The
  payoff: zero cross-origin fetch, zero ~30 MB initial download, no
  Service Worker special-case, no prefetch race / saver guard, no
  cold-start latency before the first transparent download.
- Dependency footprint (#192): the transparent video path is now
  100% in-tree JavaScript. `@ffmpeg/ffmpeg`, `@ffmpeg/util`, and
  `@ffmpeg/core` were removed from `package.json` entirely. No
  cross-origin fetch, no `copy:ffmpeg` script, no special Cloudflare
  Pages handling for the prior ~31 MB `ffmpeg-core.wasm` (which had
  exceeded the 25 MiB per-file upload limit and was the original reason
  the wasm core had to be served from jsdelivr). The Service Worker
  no longer needs a `ffmpeg-core-v<version>` cache route or
  `__FFMPEG_CORE_VERSION__` build-time stencil; the `activate` handler
  sweeps any leftover `ffmpeg-core-*` caches on the next visit so
  existing users automatically reclaim the disk space.
- **Alpha video half-res**: transparent videos (`alpha/*-alpha.mov`) are
  rendered at **540×960** (portrait) / **960×540** (landscape) — half
  the resolution of the non-transparent `mp4` outputs which stay at
  1080×1920 / 1920×1080. Originally introduced to dodge the
  single-threaded ffmpeg.wasm ~2 GB heap ceiling under libvpx-vp9
  `yuva420p`; on the current PNG-in-MOV path (and especially after the
  #192 muxer rewrite that drops the wasm encoder entirely) there is no
  memory pressure left, but half-res is kept for a different reason:
  **ZIP size**. PNG-in-MOV is lossless rgba 32-bit,
  so each tile is already ~60–70 MB at 540×960 × 192 frames; doubling
  each dimension would quadruple per-tile size to ~240–280 MB and push
  the 4-video alpha bundle past 1 GB inside the ZIP, which is not a
  reasonable browser download. Half-res keeps the full 12-tile alpha
  ZIP in the 250–300 MB range. Transparent stills (`alpha/*-alpha.png`
  and `alpha/*-alpha.webp`) are **not** affected and keep the full
  1080×1920 / 1920×1080 resolution. NLE workflows (Premiere, DaVinci,
  After Effects) are expected to scale the alpha MOV up 2× when
  compositing; bilinear / bicubic upscaling is fine because orb renders
  are intentionally blurry and the loss is essentially imperceptible.
- No idle-time prefetch / saver guard (#192): with the JS-only muxer
  the transparent download path has zero external assets to fetch, so
  the prior `requestIdleCallback` → `prefetchFfmpegCore()` speculation,
  the `navigator.connection.saveData` / `effectiveType` opt-out, and
  the `Network Information API` typing have all been deleted from
  `Studio.tsx`. First-click latency on the transparent DL is bound only
  by per-frame rendering + a single-pass in-memory MOV assembly that
  completes in well under 100 ms for a 192-frame tile.
- No loading-failure UX (#192): the old `alphaEncoderLoadFailed` state
  machine and i18n key have been removed because the failure scenario
  it covered (ffmpeg-core wasm fetch dies under network failure) no
  longer exists. The `alphaEncodingInProgress` string is kept for
  potential future use (e.g. if frame counts grow large enough to
  warrant a progress indicator) but is currently unrendered.
- Progress: when the toggle is ON, `dlProgress.total` is set to
  `indices.length * 2` so the existing "Rendering high-res… N / Total"
  text covers both the background-filled pass and the alpha pass with
  monotonically increasing N. No extra UI string is needed.
- OFF path identity: when the checkbox is OFF (default), `downloadIndices`
  never instantiates the alpha helpers and never calls the new worker
  RPCs (`generateOneAlpha` / `renderAlphaFrames`) and never loads
  ffmpeg.wasm. The pre-#56 download is byte-exact identical.

The CLI is unaffected — it already takes file paths, and adding alpha
flags there is out of scope for #56 / #184.
