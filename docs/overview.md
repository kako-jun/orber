# orber overview

`orber` turns a photo or short video into an abstract **orb mood** rendition — colorful, blurry light spheres that drift slowly. The original subject is intentionally lost; what survives is the *vibe* of the colors.

## Pipeline

```
input image / video
  ├─ (video only) extract representative frames via ffmpeg
  ├─ extract color clusters       → N representative colors  [implemented]
  ├─ place orbs                   → position, size, base color per orb  [implemented for static PNG]
  ├─ render frame(s)              → RGBA buffer with radial-gradient orbs  [implemented via tiny-skia]
  ├─ (animated) interpolate       → frame sequence over time t  [implemented]
  └─ encode                       → PNG / MP4 / WebM / SVG / CSS  [PNG / MP4 / WebM / SVG / CSS implemented]
```

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
- `--blur` — blur intensity in 0.0..=1.0 (sharp ↔ fully diffused)
- `--count` — orbs visible on screen at once (1..=200, default 20)
- `--direction` — conveyor flow direction: `lr` / `rl` / `tb` / `bt`
- `--speed` — conveyor pace: `very-slow` / `slow` (cross counts per clip)
- `--shape` — `circle` or `aquarelle` (watercolor bleed)
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

Each orb is also assigned an integer **speed multiplier** (`1x` / `2x`)
deterministically from the seed, so individual orbs visibly travel at different
paces inside the same clip. Combined with the global `--speed` cycle count
(`very-slow` / `slow` = 1 / 2), per-orb effective traversal counts spread over
`{1, 2, 4}` per clip. Because every factor is an integer, the loop closure at
`t = 0 ≡ t = 1` remains pixel-exact.

`--speed` itself is the global cycle count (1 / 2 screen-crosses per clip for
the slowest orbs). Real-time pacing is set by `--duration-ms`: `--speed slow
--duration-ms 8000` means the slowest orbs cross the screen twice in 8 seconds
(4 s/cross), with `2x` orbs proportionally faster.

> Note: the aquarelle shape uses the legacy `[0, 1]` wrap. Its bleed / bloom / halo
> textures clip cleanly enough that the off-screen wrap buffer would interfere with
> the halo rendering. The `[-r, 1+r]` off-screen wrap described above applies to
> the `circle` shape only.

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

Stills are not pure `render_static` snapshots — they are the `t = 0` frame of the
conveyor, so orbs are phase-scattered and the off-screen wrap buffer means a fraction
of the requested `--count` will sit just outside the visible area, matching the
visual language of the videos.

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

The aquarelle (watercolor bleed) shape generator will eventually be split out into its own crate, shared between `orber` (irregular orb shapes) and `blueprinter` (sumi / watercolor diagram themes). For the prototype it lives inside `orber-core` under `crates/core/src/aquarelle/` so the module boundary is already in place.

## Workspace layout

Since `v0.3.0` (Issue #35) the repository is a Cargo workspace with two crates:

- **`orber-core`** (`crates/core/`) — pure rendering library: cluster extraction, orb rendering, animation frames, CSS / SVG output, and the `batch::generate_batch` helper. No filesystem I/O and no subprocess. Builds for `wasm32-unknown-unknown` so a future Web frontend can call it directly.
- **`orber`** (`crates/cli/`) — the CLI binary. Owns `image::open`, `tempfile`, and the `ffmpeg` subprocess used for video output. Depends on `orber-core` for all rendering.

User-facing CLI behavior is unchanged.

## Web GUI rendering pipeline

The web frontend (`web/`) renders 12 tiles per drop (8 stills + 4 animated) via
the WASM bindings. The pipeline is split between a **main thread** (UI + DOM)
and a **dedicated Worker thread** (wasm + WebCodecs):

```
[main thread]                          [worker thread (orberWorker.ts)]
  Studio.tsx                             wasm-bindgen loaded once
   ├ runBatch                            ├ generate_one_at_index × 12
   │   └ workerGenerateOne(i) ────────→  │   └→ PNG bytes (Transferable)
   ├ animate phase                       ├ start_animation_for_batch_spec × 4
   │   └ workerAnimateOne(i) ─────────→  │   ├ next_frame loop
   │                                     │   ├ WebCodecs VideoEncoder
   │                                     │   └→ mp4 Blob (Transferable)
   └ DL high-res                         └ same APIs, with width/height = 1080×1920
       └ workerGenerateOne / workerAnimateOne (per selected index)
```

The source RGB buffer is uploaded once via `workerSetSource` and cached in the
Worker; subsequent `generateOne` / `animateOne` calls reference that cache so
multi-megabyte arrays are not copied per call.

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
same logic to both the preview PNG (`generate_one_at_index`) and the
animation cursor (`start_animation_for_batch_spec`), so the t=0 frame and the
mp4 are guaranteed to match.

**Clip duration.** Animated tiles are **8 seconds long at 24 fps** (192 frames).
Combined with the assigned speeds above, VerySlow tiles cross the screen once
in 8 s — slow enough to feel "drifting", appropriate for use as overlay /
background plates beneath text.

**Browser requirements.** OffscreenCanvas / VideoEncoder / VideoFrame in Worker
context. iOS Safari 16.4+, current Android Chrome / Firefox 130+. There is no
fallback path for older browsers — the GUI shows an error if WebCodecs is
unavailable.

**Progressive UX.** While the Worker is busy:

- An empty grid of 12 **skeleton tiles** appears the moment the user drops an
  image, so the layout is fixed before any pixel is rendered.
- Stills replace their skeleton one by one as PNG bytes arrive from the Worker.
- Video tiles show a softer shimmer (`.skeleton-soft`) plus an "Animating" badge
  on top of the still PNG until the mp4 is delivered, signalling that they will
  start moving shortly.

**Re-roll cancellation.** When the user re-rolls (or drops a new image / flips
aspect) while the previous batch is still in flight, `runBatch` terminates the
Worker (`worker.terminate()`) and respawns it with a fresh wasm instance. A
logical generation guard (`runGen` / `myGen`) alone is not enough because the
in-flight wasm calls (`generate_one_at_index`) and the WebCodecs encode loop
keep running to completion otherwise, doubling CPU usage and delaying the new
batch. After respawn the cached source RGB is invalidated and re-uploaded on
the next `workerSetSource`. The cost (a few hundred ms of wasm re-init) is paid
only when re-rolling mid-batch; single, sequential runs see no overhead.

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
post-hydration by `Subtitle.tsx` for reactive UI text.
