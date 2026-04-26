# orber overview

`orber` turns a photo or short video into an abstract **orb mood** rendition — colorful, blurry light spheres that drift slowly. The original subject is intentionally lost; what survives is the *vibe* of the colors.

## Pipeline

```
input image / video
  ├─ (video only) extract representative frames via ffmpeg
  ├─ extract color clusters       → N representative colors
  ├─ place orbs                   → position, size, base color per orb
  ├─ render frame(s)              → PNG buffer with circular blur
  ├─ (animated) interpolate       → frame sequence over time t
  └─ encode                       → PNG / MP4 / WebM / SVG / CSS
```

## Output formats

|              | Static            | Animated                            |
| ------------ | ----------------- | ----------------------------------- |
| **Raster**   | PNG, WebP         | MP4, WebM (vertical 9:16 by default)|
| **Style**    | CSS gradient      | CSS gradient + `@keyframes`         |
| **Vector**   | SVG               | —                                   |

CSS / SVG output is attractive because it is essentially zero-byte, infinitely loopable, resolution-independent, and cheap to render in a browser compared to a video element.

## Parameters (planned)

- orb size (small = many tiny orbs, large = few soft blobs)
- blur intensity (sharp ↔ fully diffused)
- motion speed (still ↔ leisurely ↔ lively)
- orb shape (circle / aquarelle bleed)
- saturation and brightness adjustment
- clip duration (animated outputs)
- random seed for reproducibility

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
