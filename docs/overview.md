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
  └─ encode                       → PNG / MP4 / WebM / SVG / CSS  [PNG / MP4 / WebM implemented]
```

## Output formats

|              | Static            | Animated                            |
| ------------ | ----------------- | ----------------------------------- |
| **Raster**   | PNG, WebP         | MP4, WebM (vertical 9:16 by default)|
| **Style**    | CSS gradient      | CSS gradient + `@keyframes`         |
| **Vector**   | SVG               | —                                   |

CSS / SVG output is attractive because it is essentially zero-byte, infinitely loopable, resolution-independent, and cheap to render in a browser compared to a video element.

## Parameters

The CLI exposes the following flags (run `orber --help` for the authoritative list):

- `--orb-size` — relative orb size multiplier (small = many tiny orbs, large = few soft blobs)
- `--blur` — blur intensity in 0.0..=1.0 (sharp ↔ fully diffused)
- `--motion` — `still` / `slow` / `lively` drift speed
- `--shape` — `circle` or `aquarelle` (watercolor bleed)
- `--saturation` — saturation multiplier
- `--duration-ms` — clip duration for animated outputs
- `--seed` — random seed for reproducibility

## Use cases

- Background plates for video edits
- Streaming "be right back" idle screens
- Social story / TikTok / Reels backgrounds
- Phone or desktop wallpapers from your own photos
- Privacy-friendly mood snapshot of a place (looks nothing like the original)

## Non-goals (for the prototype)

- Web frontend (planned later as a separate effort)
- WASM build (planned later)
- Publishing on crates.io (planned later)
- Realtime / interactive editing (CLI-only for now)

## Relationship to aquarelle

The aquarelle (watercolor bleed) shape generator will eventually be split out into its own crate, shared between `orber` (irregular orb shapes) and `blueprinter` (sumi / watercolor diagram themes). For the prototype it lives inside `orber` under `src/aquarelle/` so the module boundary is already in place.
