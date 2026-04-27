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
- `--direction` — conveyor flow direction: `lr` / `rl` / `tb` / `bt`
- `--speed` — conveyor pace: `very-slow` / `slow` / `medium` (cross counts per clip)
- `--shape` — `circle` or `aquarelle` (watercolor bleed)
- `--saturation` — saturation multiplier
- `--duration-ms` — clip duration for animated outputs
- `--seed` — random seed for reproducibility
- `--variations N --output-dir DIR` — emit a curated set of N alternate looks for the same input (color-shifted, direction-varied, cluster-count-varied)

## Motion model (v0.3.0)

Animated outputs use a **one-way conveyor belt**. The whole clip flows in exactly one
direction (`lr` / `rl` / `tb` / `bt`); orbs do not reflect, oscillate, or return to
their start. When an orb exits one edge, a fresh orb enters from the opposite edge
(toroidal wrap). Each orb has a randomized initial phase so the field looks scattered
rather than synchronized. A baseline ±10% radius breathing is applied to every orb
automatically — there is no opt-in flag for it.

`--speed` is expressed as integer screen-crosses per clip (1 / 2 / 3), keeping the
loop pixel-exact at `t = 0 ≡ t = 1`. Real-time pacing is set by `--duration-ms`:
`--speed slow --duration-ms 8000` means the conveyor crosses the screen twice in
8 seconds (4 s/cross).

## Variation preset (v0.3.0)

The `--variations` mode draws from a 10-entry hand-tuned preset that combines five
independent axes — hue shift, lightness bias, k-means cluster count, conveyor
direction, and speed — so the same input yields ten visibly distinct outputs:

- 4 stills: `warm_glow_lr`, `cool_mist_rl`, `hi_key_tb`, `dark_mood_bt`
- 6 animations (8 s each): `drift_lr_slow`, `drift_rl_very_slow`, `drift_tb_slow`,
  `drift_bt_slow`, `aurora_rl`, `dream_lr`

Stills are not pure `render_static` snapshots — they are the `t = 0` frame of the
conveyor, so orbs are phase-scattered and a few of them straddle the edges, matching
the visual language of the videos.

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

The aquarelle (watercolor bleed) shape generator will eventually be split out into its own crate, shared between `orber` (irregular orb shapes) and `blueprinter` (sumi / watercolor diagram themes). For the prototype it lives inside `orber` under `src/aquarelle/` so the module boundary is already in place.
