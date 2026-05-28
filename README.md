# orber

<a href="https://nostalgic.llll-ll.com"><img src="https://api.nostalgic.llll-ll.com/visit?action=get&id=github-20b16cd9&format=image" alt="Visitors" align="middle"></a>
<a href="https://nostalgic.llll-ll.com/yokoso"><img src="https://api.nostalgic.llll-ll.com/yokoso?action=get&id=github-20b16cd9&format=image" alt="Welcome" align="middle"></a>

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

Static PNG, vertical-format video (`mp4` via libx264, `webm` via libvpx-vp9), static SVG, and CSS background snippets are implemented. Only `webp` is accepted by the CLI but not yet rendered — it exits with `not yet implemented`. The output format is inferred from the extension. CLI flags cover orb size, blur, conveyor `--direction` and `--speed`, orb shape (circle / aquarelle bleed / glyph), `--glyph-char`, `--count` (or `--count-preset`), `--softness`, saturation, and clip duration. See all flags via `orber --help`.

```bash
orber --input photo.jpg --output star.png --shape glyph --glyph-char "☆" --softness low
orber --input photo.jpg --output dense.png --count-preset high --speed fast
```

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
# Video input: keyframe interpolation (#33) — color + position + weight all lerp between keyframes
# centroid drift is visible on both Aquarelle (full) and Circle (50% blended with per-orb scatter).
orber --input video.mp4 --output orb.mp4 --input-mode keyframe --keyframes 8 --duration-ms 10000
```

`--speed` is the **global** cycle count (`very-slow` / `slow` / `mid` / `fast`
= 1 / 2 / 3 / 4 screen-crosses per clip for the slowest orbs). Each orb also gets
a per-orb integer **speed multiplier** (`1x` / `2x` / `3x`) assigned deterministically
from the seed, so individual orbs visibly travel at different paces inside the
same clip — effective traversal counts spread over `{cycle, 2×cycle, 3×cycle}` per clip.
All factors are integers, so the loop closure at `t = 0 ≡ t = 1` stays pixel-exact.
Combined with a long `--duration-ms`, this gives the characteristic gentle, layered
drift. Every orb also gets three independent breathing pulses (radius ±10%,
blur ±15%, opacity ±5%) applied automatically — there is no opt-in flag for that.

> Note: the aquarelle shape uses the legacy `[0, 1]` wrap (its bleed / bloom / halo
> textures clip cleanly enough that the off-screen buffer would interfere with the
> rendered halo). The off-screen wrap buffer described above applies to the
> `circle` and `glyph` shapes only.

### Orb count

Use `--count <N>` (1..=1024, default 20) to control how many orbs are visible on screen
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

`--count-preset low|mid|high` is a shorthand alternative to `--count <N>` (mapped
to 10 / 20 / 30). The two flags are mutually exclusive — pass one or the other.

> Note: the aquarelle shape ignores `--count` (palette-only rendering). It always
> renders one orb per cluster from the k-means palette so the bleed / bloom / halo
> texture set stays coherent.

### Glyph shape

`--shape glyph` swaps the round orb for a **glyph character** (default `☆`). Pick
the character with `--glyph-char <CHAR>` (exactly one character):

```bash
orber --input photo.jpg --output stars.png --shape glyph --glyph-char "★"
orber --input photo.jpg --output arrows.mp4 --shape glyph --glyph-char "→" --direction lr
```

Glyphs are rendered from a bundled **Noto Sans Symbols 2 subset** (~177 KB,
embedded via `include_bytes!`) covering ASCII, digits, punctuation, arrows,
geometric shapes, Dingbats, and supplemental symbols. Hiragana, kanji, emoji
and other characters outside this subset are silently skipped instead of
drawing tofu. The glyph outline is converted to a cached **signed-distance
field**, so `--blur` and `--softness` now affect glyphs with the same visual
meaning as circle orbs: soft edge falloff, not a hard text fill. Glyphs also
get a seed-derived base angle so stills are not a wall of identically oriented
symbols; animated outputs continue rotating per orb from that base angle.

> **Font credit:** Noto Sans Symbols 2 © Google Inc., licensed under SIL Open
> Font License 1.1. See `crates/core/assets/fonts/OFL.txt` for the full license
> text shipped alongside the TTF.

### Softness

`--softness low|mid|high` (default `mid`) is a single axis that bundles alpha,
blur, and edge softness:

- `low` — default alpha + weak blur + sharper edges. Tuned for a crisper plate
  or wallpaper look.
- `mid` — identical to the previous default. Existing renders are unchanged.
- `high` — weaker alpha + stronger blur + softer edges. Tuned for sitting
  **underneath text overlays** so the orbs read as ambient color.

```bash
orber --input photo.jpg --output wallpaper.png --softness low
orber --input photo.jpg --output backdrop.png --softness high
```

### Variation preset

To explore looks for a single input, batch out a curated set of 10 alternates:

```bash
orber --input photo.jpg --variations 10 --output-dir out/
```

The preset table varies conveyor direction, speed, orb count, orb size, and blur to
produce ten visibly distinct outputs (4 stills + 6 animations). Colors come straight
from the k-means palette of the input image — variations never recolor the photo.
Use `--variations-mode still` or `--variations-mode video` to filter the table.

### Web GUI

A drag-and-drop browser GUI is published at <https://orber.llll-ll.com/>. Drop an
image and the page generates a fresh batch of **12 tiles** every time, regardless
of aspect (portrait 540×960 or landscape 960×540). 12 was picked because its
divisor count (1/2/3/4/6/12) lets the grid lay out cleanly across phone widths.
Unlike the CLI's fixed `--variations` preset, the GUI samples direction / speed /
count / orb size / blur randomly per drop, so the same image yields a different
layout each time. The first **8 tiles** are static PNGs; the **last 4 tiles**
are H.264 mp4 loops generated client-side via WebCodecs and inlined as
`<video muted playsinline loop>`. Each of the 4 video tiles flows in a
different direction (left→right, right→left, top→bottom, bottom→top), shown in
that order so every batch always offers all four motion axes side by side. The
4 videos start playing **simultaneously** once all encodes finish, so the field
animates as a single coordinated burst rather than staggered pop-ins. Pick
favorites with the corner-marker toggle and download single (PNG / MP4) or
multi (mixed-extension ZIP). After drop, the source image stays in the drop
zone as a thumbnail; hover (or drag a new file over it) to swap it out without
touching any other control. Long-press the thumbnail (~400ms) to peek at the
source image full-size in an overlay, release to close — handy for verifying
which photo is loaded without losing the rest of the GUI.

The GUI runs entirely client-side. The `orber-wasm` crate handles rendering
(measured ≈ 220 KB gzipped at v0.3.0); H.264 encoding is done in the browser via
the WebCodecs API (Chrome 94+ / Safari 16.4+ / Firefox 130+). On older browsers
the video tiles fall back to the static PNG. Source: `web/` (Astro + Solid +
Tailwind). The visual language and component conventions are documented in
[`DESIGN.md`](./DESIGN.md). UI text is auto-localized: Japanese for `ja-*`
browsers, English everywhere else, with no language picker.

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
