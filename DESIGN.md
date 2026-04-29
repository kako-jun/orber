# DESIGN.md

orber — Design System

## 1. Visual Theme & Atmosphere

Black-canvas gothic with glass-only buttons. The studio surface is pure black so generated orbs read at full chroma without UI competition. Typography is gothic and quietly confident, not decorative. Only interactive controls (buttons, toggles, segmented controls) carry a glassmorphism treatment — frosted translucency over the black, hairline borders, no fills. Tiles, drop area, status text, and background remain flat. The interface is silent until you touch it.

Inspirations: Apple visionOS controls (frosted chip overlay), Bauhaus poster typography (low-weight wide-tracked lowercase), professional video tools like DaVinci Resolve / Final Cut viewer chrome (black canvas, restrained chrome).

## 2. Color Palette & Roles

No accent color. The generated artwork supplies all color; the chrome stays in monochrome.

| Token            | Value                       | Usage                                                   |
| ---------------- | --------------------------- | ------------------------------------------------------- |
| `bg`             | `#000000`                   | Page / canvas background                                |
| `fg`             | `#FFFFFF`                   | Primary text, logo, active control text                 |
| `fg-muted`       | `rgba(255,255,255,0.55)`    | Subtitle, status text, inactive control text            |
| `fg-subtle`      | `rgba(255,255,255,0.32)`    | Placeholder text, disabled label                        |
| `hairline`       | `rgba(255,255,255,0.12)`    | Drop-area dashed border, separators                     |
| `glass-bg`       | `rgba(255,255,255,0.06)`    | Default button / toggle / segmented surface             |
| `glass-bg-hover` | `rgba(255,255,255,0.10)`    | Hover state for glass surfaces                          |
| `glass-blur`     | `blur(12px)`                | `backdrop-filter` on glass surfaces                     |
| `glass-border`   | `1px solid rgba(255,255,255,0.12)` | Outline on glass surfaces                        |
| `focus-ring`     | `1px solid rgba(255,255,255,0.7)` | `focus-visible` ring for all interactive elements |

No hue-based accent (no emerald / coral / amber). State is communicated by opacity steps: default 55% → hover 100% → pressed/selected 100% with glass background bumped to 10%.

### Error / Warning States

Deliberately no red / amber / yellow. Error and warning surfaces share the glass tokens (`glass-bg` + `glass-border` + `fg`) and rely on the contextual icon, prefix word, or surrounding copy to convey state. This preserves the monochrome gothic discipline. If a future feature truly needs hue-based alarm (e.g. data loss), introduce a dedicated `state-danger` token here first — never reach for raw `red-*` / `amber-*` Tailwind classes inline.

## 3. Typography Rules

### Font Families

```
display: "Space Grotesk", system-ui, sans-serif   # logo only
body:    system-ui, -apple-system, "Segoe UI", "Hiragino Sans",
         "Yu Gothic", Meiryo, sans-serif           # everything else
```

Space Grotesk is loaded from Google Fonts CDN with `preconnect` to `fonts.googleapis.com` and `fonts.gstatic.com`. Weights pulled: 300, 400, 500.

### Type Scale

| Element       | Size            | Weight | Tracking  | Notes                                       |
| ------------- | --------------- | ------ | --------- | ------------------------------------------- |
| Logo (h1)     | 3rem (48px)     | 300    | `0.4em`   | `font-display`, lowercase, color `fg`. Compensate the trailing tracking with `pl-[0.4em]` so the visual center aligns with the page axis. |
| Subtitle      | 0.875rem (14px) | 400    | normal    | `font-display`, color `fg-muted`, 1 line     |
| Status        | 0.875rem (14px) | 400    | normal    | system sans, color `fg-muted`               |
| Button label  | 0.875rem (14px) | 400    | normal    | system sans, color `fg` / `fg-muted`        |
| Placeholder   | 0.875rem (14px) | 400    | normal    | color `fg-subtle`                           |

No bold anywhere. Headers are deliberately light.

## 4. Component Stylings

### DropArea

- Flat, **not** glass (acts as content surface, not control)
- Background: transparent
- Border: `1px dashed hairline`, radius `0.75rem`
- Padding: `2.5rem` (40px) vertical, `2rem` (32px) horizontal
- Hover state (empty): border swaps to `fg-muted`
- Drag-over state: border swaps to `fg`, fill `glass-bg`
- Text: placeholder uses `fg-muted`, picked filename uses `fg`
- Cursor: `pointer` (always — even when filled with a thumbnail; the area
  remains an active drop target / file picker trigger)
- Focus: when the inner `<input type=file>` is focused, the surrounding
  `<label>` gets `focus-within:border-focusRing`

#### Filled state (thumbnail)

After a file is picked, the drop area shows the source image as a thumbnail
**without losing its drop-target framing**. The dashed border, padding, and
cursor are kept exactly as in the empty state — only the inner content swaps.

- Thumbnail: `<img>`, centered, `object-contain`, `max-h-40` (160px) so the
  drop frame keeps a consistent height regardless of source aspect
- Replace overlay: a fade-in layer above the thumbnail
  - Hover (group): `opacity 0 → 1`, fill `bg/40` (40% black scrim), centered
    `Replace` / `差し替え` label in `font-display`, `text-sm`, `tracking-wide`,
    color `fg`
  - Drag-over: overlay fully opaque with fill `fg/5` (faint white wash) and
    no label — the active drop affordance is the white border and the wash
- The overlay is `pointer-events: none` so the underlying `<label>` keeps
  receiving drag / click events

### Button (Glass)

- Background: `glass-bg` + `backdrop-filter: glass-blur`
- Border: `glass-border`
- Radius: `0.5rem` (8px)
- Padding: `0.5rem 0.875rem` (8px 14px)
- Text: `fg`, system sans, 14px / 400
- Hover: background → `glass-bg-hover`
- Pressed / disabled: `opacity: 0.4` (no extra color)
- Focus-visible: outline `focus-ring`, offset `2px`
- Transition: `opacity 200ms ease-out, background-color 200ms ease-out`

### Toggle (Aspect — Portrait / Landscape)

- Same glass base as Button
- Icon-only (existing rectangle silhouette SVG, 20px)
- `aria-pressed="true"` → text/icon color `fg`, glass bg `glass-bg-hover`
- `aria-pressed="false"` → icon color `fg-muted`
- No emerald accent; selection is communicated by opacity + bg shift

### SegmentedControl

Not used in this iteration; reserved. The aspect Portrait / Landscape pair is intentionally implemented as **two independent glass Toggles** rather than a SegmentedControl: only two states, each carries a distinct silhouette icon, and visual independence reads better against the black canvas. If a future control needs three or more mutually-exclusive states, introduce a SegmentedControl here using the same glass tokens — a group of buttons with shared border-radius on the outer edges.

### Tile

- Flat. **No glass, no ring, no rounded corners beyond `0.125rem` (2px) for sub-pixel cleanup**
- `aspect-ratio` follows current canvas (540/960 portrait, 960/540 landscape)
- Background: `#000` while loading
- `<img>` / `<video>` fills with `object-cover`
- Cursor: `pointer`

### SelectionMarker

- 4 corner L-shaped marks, 12px arms, stroke 1.5px, color `fg` (white)
- Inset 6px from tile edge
- Implementation: absolutely-positioned inline SVG, single SVG with 4 paths
- Hidden by default: `opacity: 0`, `transition: opacity 200ms ease-out`
- `selected` → `opacity: 1`
- The previous top-right `✓` glyph is removed

### StatusText

- Centered or left-aligned, system sans 14px, color `fg-muted`
- One line; truncates with ellipsis on overflow

### Reroll Button (Gacha)

- Glass button base, **icon-only** (curved arrows / reload glyph, 16px stroke 1.5)
- No "ガチャ" / "Roll again" text in the visual; full label lives in `aria-label` and `title` only

### PreviewOverlay

Press-and-hold preview for the drop-zone thumbnail (#57). Triggered by a long
press of ~400ms, dismissed when the pointer is released.

- `position: fixed`, `inset: 0`, `z-50`, full viewport
- Background: `bg/80` (80% black scrim)
- Content: the source image, centered, `max-h-[90vh] max-w-[90vw]`,
  `object-contain` so it never crops
- `pointer-events: none` — the overlay never intercepts the user's release;
  the original drop-zone `<label>` keeps owning the gesture
- Mounts via `.fade-in` (DESIGN.md §6) so the overlay rises smoothly
- The thumbnail `<img>` underneath wears `select-none touch-none` (and an
  inline `-webkit-touch-callout: none`) to suppress iOS callout / loupe and
  text selection during the long press
- A short tap (under 400ms) is treated as a normal label click and opens the
  file picker; only the long-press path opens the overlay. The synthetic click
  fired after a successful long-press is suppressed by an `isLongPress` flag
  in the click handler.

## 5. Spacing & Layout

Tailwind spacing scale (4px base):

| Token | Value |
| ----- | ----- |
| 1     | 4px   |
| 2     | 8px   |
| 3     | 12px  |
| 4     | 16px  |
| 6     | 24px  |
| 8     | 32px  |

- Page main: `max-w-3xl` centered, `p-8` (32px)
- Logo block → subtitle: `mt-2` (8px)
- Subtitle → controls: `mt-8` (32px)
- Stack between major sections: `space-y-4` (16px)
- Tile grid gap: `0.5rem` (8px)
- Button group gap: `0.5rem` (8px)
- Tile batch count: **12** (1/2/3/4/6/12 で割り切れる最大公約数の大きい数字、
  どの幅でも余りなくグリッドが組める)。前半 8 枚静止 + 後半 4 枚動画 (#59, #61)
- Tile grid columns: portrait (tall cells) = `grid-cols-2 sm:grid-cols-3 md:grid-cols-4`、
  landscape (wide cells) = `grid-cols-1 sm:grid-cols-2 md:grid-cols-3`

## 6. Motion

Single transition idiom, used everywhere. The **source of truth** for duration / easing is the CSS variables `--orb-motion-duration` / `--orb-motion-easing` defined in `Base.astro`. The Tailwind utilities `duration-200 ease-out` happen to match those values today and are used inline because Tailwind v3 cannot consume CSS variables in `transition-duration` / `transition-timing-function` shorthand. If the variables ever change, update both places (the variables and the Tailwind utilities, or migrate to a Tailwind theme that reads the variables). In code, the transition reads:

```css
transition: opacity 200ms ease-out;
```

Extended for background/color changes on glass surfaces:

```css
transition:
  opacity 200ms ease-out,
  background-color 200ms ease-out,
  border-color 200ms ease-out;
```

- New tiles **fade in** (`opacity 0 → 1`, 200ms ease-out) as they arrive from the wasm batch loop, using the shared `.fade-in` class below (#60).
- Selection marker fades (`opacity 0 → 1`, 200ms ease-out) on toggle.
- No translate / scale / rotate transitions. No spring. No keyframes other than the single `orb-fade-in` defined below.

### `.fade-in` (mount-time fade)

For elements that **appear** in the DOM (rather than toggle visibility), use the shared `.fade-in` class instead of a `transition`. It is one CSS animation, defined once globally in `Base.astro`, and reused everywhere a node is freshly mounted (drop-zone thumbnail, generated tile button, status text, error box, video swap on top of the still PNG, subtitle, future long-press preview overlay).

```css
:root {
  --orb-motion-duration: 200ms;
  --orb-motion-easing: ease-out;
}
@keyframes orb-fade-in {
  from { opacity: 0; }
  to   { opacity: 1; }
}
.fade-in {
  animation: orb-fade-in var(--orb-motion-duration) var(--orb-motion-easing) both;
}
```

Rules:
- Only `opacity` is animated (no `transform`, no layout). Compositor-only — cheap on mobile.
- Duration / easing live in CSS variables so the whole house tunes from one place.
- Use `.fade-in` for **appearance**; use `transition: opacity ...` for **toggling**. Both share the 200ms / ease-out idiom.
- For PNG → MP4 swap on a video tile: the PNG stays mounted as the bottom layer, and the `<video>` is added above with `.fade-in` so the swap is a crossfade rather than a cut.

Reduced motion: respect `prefers-reduced-motion: reduce` by clamping all transitions and the `.fade-in` animation to `0ms` (already wired in `Base.astro` via a media query that overrides `--orb-motion-duration` and `animation-duration`). Tailwind's `motion-reduce:transition-none` may be added per-element as a belt-and-suspenders.

## 7. Iconography

- Inline SVG only (no icon-font, no third-party library)
- Size: 16px (inline / button-internal) or 20px (toggle silhouettes)
- Stroke: `1.5`
- Stroke linecap / linejoin: `round`
- Fill: `none`, `stroke="currentColor"` so they inherit text color (`fg` or `fg-muted`)
- Existing aspect silhouettes (rectangles) and reroll arrows are kept; they already match this spec after a stroke-width adjustment to 1.5

## 8. Accessibility

- All interactive elements have `aria-label`. Icon-only buttons additionally carry `title` for hover affordance
- `focus-visible` ring: `1px solid rgba(255,255,255,0.7)` with `2px` offset on every button / toggle / drop label / tile
- Contrast: `fg` on `bg` = 21:1; `fg-muted` (55%) on `bg` ≈ 9.6:1; `fg-subtle` (32%) on `bg` ≈ 5.0:1 — all clear of WCAG AA for text ≥14px
- Tile selection state is exposed via `aria-pressed` so screen readers don't depend on the corner marker alone
- `<html lang>` is updated **pre-hydration** by an `is:inline` script in `Base.astro` that reads `navigator.language` synchronously, so screen readers pick the correct voice from first paint. The Solid `lang` signal is then synchronized **post-hydration** by `Subtitle.tsx` (`onMount → setLang(detectLang())`) for reactive UI text. The two paths are intentionally separated: the document attribute is a11y-critical and must be early; the signal is reactivity-only
- Reduced motion is honored (see §6)
- Hit targets: every button is at least 32×32 px

---

## Agent Quick Reference

When generating UI for orber:

- Black background, white text, no hue accent — period.
- Buttons are the only glass elements. Tiles, drop area, status text, and the background are flat.
- Logo is `font-display` (Space Grotesk), `font-light`, `lowercase`, wide tracking.
- Selection on tiles = 4 corner L-marks fading in. Never use a check mark, never use a colored ring.
- All transitions are 200ms ease-out on opacity (and optionally background-color / border-color on glass).
- All visible strings come from `web/src/lib/strings.ts` via `t('key')`. Never hard-code Japanese or English.
- Language is auto-detected from `navigator.language` on mount; no language picker exists.
