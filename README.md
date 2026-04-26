# orber

Turn photos and videos into abstract **orb mood** output — colorful, blurry light spheres drifting slowly. Useful as video backgrounds, streaming wait screens, social story backgrounds, wallpapers, or just to obfuscate a personal photo into a vibe.

> **Status:** prototype. CLI scaffold only — rendering not yet implemented.

## Concept

```
Input image / video
  → Extract color clusters (this area is red, that area is blue, ...)
  → Convert each cluster into a light orb
  → Animate orbs drifting slowly with smooth color transitions
  → Output as still image, vertical video, or pure CSS/SVG
```

## Output formats (planned)

|              | Static                       | Animated                       |
| ------------ | ---------------------------- | ------------------------------ |
| **Raster**   | PNG / WebP                   | MP4 / WebM (vertical 9:16)     |
| **Style**    | CSS gradient                 | CSS gradient + `@keyframes`    |
| **Vector**   | SVG                          | —                              |

## Usage (planned)

```bash
orber --input photo.jpg --output orb.png
orber --input photo.jpg --output orb.mp4 --seed 42
```

The output format is inferred from the extension (`png`, `webp`, `mp4`, `webm`, `svg`, `css`). CLI flags cover orb size, blur, motion speed, shape (circle / aquarelle bleed), saturation, and clip duration. See all flags via `orber --help`.

## Build

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## License

MIT
