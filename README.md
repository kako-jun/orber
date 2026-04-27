# orber

Turn photos and videos into abstract **orb mood** output — colorful, blurry light spheres drifting slowly. Useful as video backgrounds, streaming wait screens, social story backgrounds, wallpapers, or just to obfuscate a personal photo into a vibe.

> **Status:** prototype. PNG output and vertical-format video (`mp4` via libx264, `webm` via libvpx-vp9) are implemented end-to-end. Vector / CSS outputs are still placeholders.

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

## Usage

PNG output is implemented and produces a 1080×1920 vertical orb image:

```bash
orber --input photo.jpg --output orb.png
orber --input photo.jpg --output orb.png --blur 0.9 --orb-size 1.5 --saturation 1.4
orber --input white_paper.jpg --output orb.png --background auto
orber --input photo.jpg --output orb.png --background "#1a1a1a"
orber --input photo.jpg --output orb.svg --background transparent
```

Static PNG, vertical-format video (`mp4` via libx264, `webm` via libvpx-vp9), static SVG, and CSS background snippets are implemented. Only `webp` is accepted by the CLI but not yet rendered — it exits with `not yet implemented`. The output format is inferred from the extension. CLI flags cover orb size, blur, conveyor `--direction` and `--speed`, orb shape (circle / aquarelle bleed), saturation, clip duration, and background color (`black` / `white` / `auto` / `transparent` / `#RRGGBB[AA]`; default `auto` picks a dimmed average color of the input image). See all flags via `orber --help`.

### Motion model

Animated outputs use a **one-way conveyor belt**: every orb in a clip drifts in the
same direction, exits one edge, and re-enters from the opposite edge (toroidal wrap).
A single clip flows in exactly one of `lr` (left→right), `rl`, `tb`, or `bt`. Pick the
direction and pace with:

```bash
orber --input photo.jpg --output drift.mp4 --direction lr --speed slow
orber --input photo.jpg --output drift.mp4 --direction tb --speed very-slow --duration-ms 10000
```

`--speed` chooses how many full screen-crosses fit into the whole clip (1 / 2 / 3 for
`very-slow` / `slow` / `medium`). Combined with a long `--duration-ms`, this gives the
characteristic gentle drift. Every orb also gets a tiny breathing pulse (±10% radius)
applied automatically — there is no opt-in flag for that.

### Variation preset

To explore looks for a single input, batch out a curated set of 10 alternates:

```bash
orber --input photo.jpg --variations 10 --output-dir out/
```

Each preset varies hue, lightness, k-means cluster count, conveyor direction, and
speed so the ten outputs are visibly distinct (4 stills + 6 animations). Use
`--variations-mode still` or `--variations-mode video` to filter.

## Build

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Installation

```bash
cargo install orber
```

Or download a prebuilt binary from GitHub Releases once `v0.1.0+` tags are published.

## Release

`orber` is prepared as a Rust CLI crate with:
- `cargo install orber`
- GitHub Actions CI on pushes and pull requests
- a tag-driven GitHub Releases workflow for Linux, macOS, and Windows artifacts

The first public crate/release target is `v0.1.0`.

## License

MIT
