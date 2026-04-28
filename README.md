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
orber --input photo.jpg --output orb.svg
```

Static PNG, vertical-format video (`mp4` via libx264, `webm` via libvpx-vp9), static SVG, and CSS background snippets are implemented. Only `webp` is accepted by the CLI but not yet rendered — it exits with `not yet implemented`. The output format is inferred from the extension. CLI flags cover orb size, blur, conveyor `--direction` and `--speed`, orb shape (circle / aquarelle bleed), saturation, and clip duration. See all flags via `orber --help`.

### Background color

The background color is **derived automatically from the input image**: the dominant (highest-weight) k-means cluster becomes the canvas color, and the remaining clusters become the orb pool. A nightscape gives a black canvas with bright points; a daytime sky gives a sky-blue canvas with floating points; a beige interior gives a beige canvas with small accents. There is no `--background` flag — to change colors, change the input image.

### Motion model

Animated outputs use a **one-way conveyor belt**: every orb in a clip drifts in the
same direction, exits one edge, and re-enters from the opposite edge. The seam happens
**fully off-screen** — each orb's wrap range is `[-r, 1+r]` (where `r` is its radius
normalized by the progress-axis length), so orbs are spawned and despawned beyond the
canvas edge instead of popping in or out at the edge. A single clip flows in exactly
one of `lr` (left→right), `rl`, `tb`, or `bt`. Pick the direction and pace with:

```bash
orber --input photo.jpg --output drift.mp4 --direction lr --speed slow
orber --input photo.jpg --output drift.mp4 --direction tb --speed very-slow --duration-ms 10000
```

`--speed` is the **global** cycle count (`very-slow` / `slow` = 1 / 2 screen-crosses
per clip for the slowest orbs). Each orb also gets a per-orb integer **speed
multiplier** (`1x` / `2x`) assigned deterministically from the seed, so individual
orbs visibly travel at different paces inside the same clip — effective traversal
counts spread over `{1, 2, 4}` per clip. All factors are integers, so the loop
closure at `t = 0 ≡ t = 1` stays pixel-exact. Combined with a long `--duration-ms`,
this gives the characteristic gentle, layered drift. Every orb also gets three
independent breathing pulses (radius ±10%, blur ±15%, opacity ±5%) applied
automatically — there is no opt-in flag for that.

> Note: the aquarelle shape uses the legacy `[0, 1]` wrap (its bleed / bloom / halo
> textures clip cleanly enough that the off-screen buffer would interfere with the
> rendered halo). The off-screen wrap buffer described above applies to the
> `circle` shape only.

### Orb count

Use `--count <N>` (1..=200, default 20) to control how many orbs are visible on screen
at once. The K colors picked from the input image (by k-means) are *expanded* into N
orbs by weight-proportional color sampling and per-orb scattering on the cross axis.
Higher counts produce a denser, more screen-filling composition; lower counts leave
more breathing room around each orb.

```bash
orber --input photo.jpg --output dense.png --count 40 --orb-size 2.5
orber --input photo.jpg --output sparse.png --count 8 --orb-size 4.5
```

`--count` is purely a deterministic renderer knob; randomization (e.g. picking a
random count in the GUI) is the caller's responsibility, not a CLI feature. Each orb
is also assigned one of two visual styles (rim or soft) deterministically from the
seed, so a single frame mixes the rim-emphasized look with plain soft gradients.

> Note: the aquarelle shape ignores `--count` (palette-only rendering). It always
> renders one orb per cluster from the k-means palette so the bleed / bloom / halo
> texture set stays coherent.

### Variation preset

To explore looks for a single input, batch out a curated set of 10 alternates:

```bash
orber --input photo.jpg --variations 10 --output-dir out/
```

The preset table varies conveyor direction, speed, orb count, orb size, and blur to
produce ten visibly distinct outputs (4 stills + 6 animations). Colors come straight
from the k-means palette of the input image — variations never recolor the photo.
Use `--variations-mode still` or `--variations-mode video` to filter the table.

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
