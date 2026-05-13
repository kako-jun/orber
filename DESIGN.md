# DESIGN.md

orber — Design System

## 1. Visual Theme & Atmosphere

Black-canvas gothic with glass-only buttons. The studio surface is pure black so generated orbs read at full chroma without UI competition. Typography is gothic and quietly confident, not decorative. Only interactive controls (buttons, toggles, segmented controls) carry a glassmorphism treatment — frosted translucency over the black, hairline borders, no fills. Tiles, drop area, status text, and background remain flat. The interface is silent until you touch it.

Inspirations: Apple visionOS controls (frosted chip overlay), Bauhaus poster typography (low-weight wide-tracked lowercase), professional video tools like DaVinci Resolve / Final Cut viewer chrome (black canvas, restrained chrome).

## 2. Color Palette & Roles

No accent color. The generated artwork supplies all color; the chrome stays in monochrome.

| Token            | Value                       | Usage                                                   |
| ---------------- | --------------------------- | ------------------------------------------------------- |
| `bg`             | `#040404`                   | Page / canvas background. PWA splash / theme-color / manifest と同値（icon 右上 1px の実測値で SOT 集約） |
| `fg`             | `#FFFFFF`                   | Primary text, logo, active control text                 |
| `fg-muted`       | `rgba(255,255,255,0.55)`    | Subtitle, status text, inactive control text            |
| `fg-subtle`      | `rgba(255,255,255,0.32)`    | Placeholder text, disabled label                        |
| `hairline`       | `rgba(255,255,255,0.12)`    | Separators, glass borders                               |
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

UI 全体で Space Grotesk に統一する (Latin)。CJK 文字 (日本語) は Space Grotesk
に収録されていないので、OS の日本語フォントへ自動フォールバックさせる。

```
sans (default for body / labels / buttons / status):
  "Space Grotesk", system-ui, -apple-system, "Segoe UI",
  "Hiragino Sans", "Yu Gothic", Meiryo, sans-serif

display (h1 ロゴ・大型表示専用):
  "Space Grotesk", system-ui, sans-serif
```

`tailwind.config.mjs` で `fontFamily.sans` を上記に上書きしているので、
特に何も class を付けない要素 (`text-sm` 等) も自動で Space Grotesk を
拾う。`font-display` はロゴ等の意味的に「大型表示」な場面でのみ使う。

Space Grotesk is loaded from Google Fonts CDN with `preconnect` to `fonts.googleapis.com` and `fonts.gstatic.com`. Weights pulled: 300, 400, 500.

### Type Scale

| Element       | Size            | Weight | Tracking  | Notes                                       |
| ------------- | --------------- | ------ | --------- | ------------------------------------------- |
| Logo (h1)     | 3rem (48px)     | 300    | `0.4em`   | `font-display`, lowercase, color `fg`. Compensate the trailing tracking with `pl-[0.4em]` so the visual center aligns with the page axis. |
| Subtitle      | 0.875rem (14px) | 400    | normal    | `font-display` 明示、color `fg-muted`、1 line |
| Status        | 0.875rem (14px) | 400    | normal    | sans (Space Grotesk)、color `fg-muted`      |
| Button label  | 0.875rem (14px) | 400    | normal    | sans (Space Grotesk)、color `fg` / `fg-muted` |
| Placeholder   | 0.875rem (14px) | 400    | normal    | sans (Space Grotesk)、color `fg-subtle`     |

No bold anywhere. Headers are deliberately light.

## 4. Component Stylings

### DropArea

- Flat, **not** glass (acts as content surface, not control)
- Background: transparent
- Border: dotted ring (SVG `stroke-dasharray="0 14"` + `stroke-linecap="round"`),
  default color `fg-subtle` (`rgba(255,255,255,0.32)`), radius `0.75rem`
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

### Checkbox (Glass)

Reusable inline checkbox + label pair, introduced for the Glyph rotation toggle (#136) and intended to be reused by the transparent-DL checkbox (#56) and any future on/off control that does not warrant a full button.

- Wrapper `<label>`: `inline-flex items-center gap-2 cursor-pointer text-sm text-fg`
  - `has-[:disabled]` selectors carry `opacity: 0.4 / cursor: not-allowed` from the inner input, so the entire row dims when the input is disabled (no JS-side gating needed)
- `<input type="checkbox">`: `h-4 w-4` (16px square), `rounded-sm`, `border-glassBorder`, `bg-glassBg`, `accent-fg` (so the native check mark inherits the surface foreground), focus ring matches the rest of the glass system (`focus-visible:ring-1 focus-visible:ring-focusRing`)
- Spacing: 8px gap between the box and its label text. Vertically aligned with `items-center`
- Reactivity: the parent component owns the `boolean` signal; `onChange` re-runs the batch like every other control row (#131 idiom)

The component is exposed in `Studio.tsx` as the constants `GLASS_CHECKBOX_LABEL` (label classes) and `GLASS_CHECKBOX_INPUT` (input classes). Future checkboxes should reuse those tokens rather than re-deriving the look.

The same tokens drive the **transparent-DL checkbox** introduced for #56 ("透過版を DL に含める / Include transparent versions"). It sits directly under the aspect toggles (centred, `col-span-2`) so all download-affecting settings cluster together at the top of the grid. After #184 the encoder pipeline was swapped from WebCodecs (`VideoEncoder({codec:'vp09.00.10.08', alpha:'keep'})`, which most environments rejected) to **ffmpeg.wasm + libvpx-vp9 (yuva420p)**, so the checkbox is now reliable on every environment that can run wasm — the browser-capability probe and the `alphaVideoUnsupportedNotice` warning row are gone (see `docs/overview.md` "Transparent download bundle" for details). The only remaining fallback path is "ffmpeg.wasm core failed to load" (offline / outage); in that case the download is aborted and `alphaEncoderLoadFailed` is shown via `errorMsg`. The OFF state is byte-exact with the pre-#56 download path (the alpha worker calls and ffmpeg.wasm load are simply never made).

### SegmentedControl

Connected pill — used by every mutually-exclusive control row in Studio (#133). Aspect / Shape are 2-segment, Count / Speed / Softness are 3-segment. All five rows share the same outer container, the same per-cell sizing rule, and the same active-state token, so a 2-pick row and a 3-pick row read as the same primitive at different cardinalities.

- Outer wrapper: `inline-flex w-full max-w-md mx-auto rounded-md overflow-hidden border border-glassBorder` — `overflow-hidden` clips the children's square corners against the wrapper radius so the whole row reads as one pill
- Cells: `flex-1 h-9 px-2 text-sm` (equal-width, fixed 36px height)
- Outer corners: only the first cell gets `rounded-l-md`, only the last cell gets `rounded-r-md`; intermediate cells are `rounded-none`
- Inner separator: every cell after the first gets `border-l border-glassBorder` — a single hairline, not a gap
- Default state: `bg-glassBg text-fgMuted hover:text-fg hover:bg-glassBgHover`
- **Active state**: `bg-fg/15 text-fg` (white at 15% over the surface). This is **stronger than the legacy Toggle's `glass-bg-hover` (10%)** so a 3-segment row reads its selection at a glance. The Toggle (`§4 Toggle`) keeps the older 10% value because its 2-state silhouette icons already communicate state via shape; the segmented control needs more chroma since cells are text-only and adjacent
- Focus: matches the rest of the glass system (`focus-visible:ring-1 focus-visible:ring-focusRing`)
- Disabled: `opacity-40 cursor-not-allowed`, applied per-cell so individual cells can independently dim if a future row needs that (today every row dims as a unit via the parent grid's `disabled` propagation)
- Row alignment: every row lives inside one shared `max-w-md` grid (`grid-cols-[auto_minmax(0,1fr)]`). Aspect spans both columns (`col-span-2`) so its segmented control fills the full grid width; the four labeled rows put the segmented control in the right column. The segmented control's `w-full` plus the grid's fixed `max-w-md` means **2-pick and 3-pick rows always end at the same right edge**

The implementation lives in `Studio.tsx` as the constants `SEG_GROUP` (wrapper class string) and the helper `SEG_BTN(i, total, active)` (per-cell class string). Future segmented rows should reuse those rather than open-coding the radius / separator / state logic.

#### Glyph monochrome rendering

When the segmented control's adjacent input is the Glyph picker (`Studio` shape row), the input, the picker buttons, and the `<datalist>` options are all tagged with `glyph-symbol-text`. That class loads `Noto Sans Symbols 2` from Google Fonts and sets `font-variant-emoji: text`, so `⚡` / `☀` / `★` / `←` are drawn as white symbols (matching the rest of the chrome) instead of the OS color-emoji rasters. The font load lives in `Base.astro`; the class is scoped — body text and other UI strings continue using the system sans stack.

For Safari/iOS the picker buttons additionally append `U+FE0E` (text variation selector) after each displayed symbol — `font-variant-emoji: text` is Chromium-only, and Safari can still resolve dual-presentation codepoints like `U+26A1` to the OS color emoji font even when Symbols 2 is listed first. The variation selector is display-only; the underlying signal value (`glyphChar()`) stays as the bare codepoint so state and wasm RPCs are unaffected.

### Tile

- Flat. **No glass, no ring, no rounded corners beyond `0.125rem` (2px) for sub-pixel cleanup**
- `aspect-ratio` follows current canvas (540/960 portrait, 960/540 landscape)
- Background: `bg` token (`#040404`) while loading
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
- `<html lang>` is updated **pre-hydration** by an `is:inline` script in `Base.astro` that reads `navigator.language` synchronously, so screen readers pick the correct voice from first paint. The Solid `lang` signal is initialized to `'en'` (matching the SSR-side `detectLang()` fallback) and flipped to the real `detectLang()` value post-hydration by a `queueMicrotask` in `strings.ts`, which kicks a reactive re-render of every `t()` call across all islands at once. `Subtitle.tsx` `onMount` keeps a safety-belt `setLang` re-sync. The signal is intentionally **not** initialized with `detectLang()` at module init time: doing so would make SSR (`'en'`) and client (`'ja'`) start values disagree, and Solid's hydration "DOM already exists, skip re-render" optimization would leave some islands stuck on the SSR English while others flipped to Japanese (the bug fixed in #161). The two paths (`<html lang>` attribute + Solid `lang` signal) are intentionally separated: the document attribute is a11y-critical and must be early; the signal is reactivity-only
- Reduced motion is honored (see §6)
- Hit targets: every button is at least 32×32 px

## 13. Control Rows (#131)

#131 で Phase B 初版の折りたたみ AdvancedSection は撤去し、Studio の操作系は
**aspect の下にフラットに常時展開される 4 軸 row** に整理された。すべての row は
同じ glass token を使い、**どのボタンを押しても即 `runBatch`** で再生成する。

### レイアウト

- aspect toggle の直下に `mx-auto max-w-2xl` の 2 列 grid を置く
- 左列 = ラベル、右列 = glass button 群 or input
- ラベルは `justify-self-end text-sm text-fgMuted` で右詰めし、コロン位置を縦に揃える
- row 本体は `flex flex-wrap items-center gap-1`
- shape=`glyph` のときだけ 2 行追加:
  - 1 行目: `形状:` の右に `[円] [文字] [入力欄]`
  - 2 行目: ラベル空欄 + シンボルピッカー

### 構成軸

| 軸 | 値（内部） | UI 表示 | 既定 | wasm 引数 |
| --- | --- | --- | --- | --- |
| 形状 | `circle` / `glyph` | 円 / 文字 | `circle` | `shape` |
| Glyph 文字 | 1 Unicode scalar | 入力欄 + picker | `☆` | `glyph_char` |
| 数 | `''` / `low` / `mid` / `high` | 少なめ / 標準 / 多め | `''` | `count_preset` (`low`=10, `mid`=20, `high`=30) |
| 速さ | `''` / `slow` / `mid` / `fast` | ゆっくり / 標準 / 速め | `''` | `speed_preset` (`slow`→VerySlow, `mid`→Slow, `fast`→Mid) |
| ぼかし | `''` / `low` / `mid` / `high` | 弱め / 標準 / 強め | `''` | `softness_preset` (`low`=sharp, `mid`=identity, `high`=soft) |

- count と softness は `''` / `'mid'` のどちらでも見た目は「標準」押下
- speed も見た目は `''` / `'mid'` の両方で「標準」押下だが、**identity は `''` だけ**
  - `''` = spec.speed / `GUI_VIDEO_SPEEDS` を温存
  - `'mid'` = `MotionSpeed::Slow` に固定

### Glyph 入力

- 入力欄は shape row の右端に同居し、下段に落とさない
- `maxLength=16` で IME の中間状態を吸収し、確定時に先頭 1 Unicode scalar に丸める
- `compositionstart` / `compositionend` 中は worker RPC を飛ばさない
- 入力欄は **任意の Unicode 1 文字** を受け付ける (絵文字 / 漢字 / 記号)。実装は
  二段構成: wasm 同梱フォントで描画できる字 (☆ など) は wasm 経路で SDF 化、
  それ以外 (🐱 / 漢字 / 任意 Unicode) は worker 内 OffscreenCanvas で OS フォント
  スタックでラスタライズ → JS 側 EDT で SDF 化 (`web/src/lib/jsGlyphSdf.ts`)。
  両経路とも出力フォーマット (R8 256×256 SDF) は共通。
- color emoji (🐱 等) は alpha チャンネル抽出により **シルエット化** され、
  orber の monochrome 出力に自然に乗る。形は OS のフォントレンダリングに依存
  するため、Mac の 🐱 と Windows の 🐱 は別の輪郭になる ── これは「ユーザーが
  入れた字を尊重して描画する」を優先するための仕様で、UI には実装詳細
  (フォント名 / SDF / OS 差) を一切露出させない
- placeholder は `☆, A, 漢, 🐱, ♪` のように文字種を例示する。"emoji" 1 単語
  だけだと「emoji 限定で特別」と誤読されるため
- シンボルピッカーは見た目を端末非依存に保つため wasm 同梱フォントで描画
  できる字に限定する (任意文字は入力欄経由で受け付ける)
- glyph の描画 backend は alpha mask ではなく **SDF + 共通 falloff**。picker UI は
  変えず、見た目だけ circle と同じ「ぼけた光」に寄せる
- glyph は seed 由来の `base_angle` で始まり、静止画でも向きがばらける
- 動画中の glyph はそこからさらに orb ごとに異なる向き・回転方向・
  回転速度で連続回転する

### Image 入力 (#160)

- shape segmented pill は `Orb / Glyph / Image` の 3 択 (内部値 `circle` /
  `glyph` / `image` は wasm enum と紐付くため不変。UI ラベルのみ #174 で
  Circle → Orb に改名し、専用ぼかし経路と Glyph の汎用 SDF 経路の挙動差を
  明示)
- `Image` を選ぶとシェイプ row の下に画像入力 row が出現する: ファイル選択
  ボタン + 9×9 サムネイル + ファイル名表示
- 入力画像 (PNG / JPG / WebP / GIF / SVG) は **`File` を worker に
  structured-clone で送信** → worker 内で `createImageBitmap` →
  `OffscreenCanvas` に「contain」リサンプル → alpha or 輝度しきい値で二値化 →
  Glyph と同じ EDT で SDF 化 (`web/src/lib/jsGlyphSdf.ts:generateImageSdf`)。
  Transferable を使わない理由は worker クラッシュ / `terminateAndRespawn`
  後にメインスレッドに残った `File` 参照から再 upload するため (#168 M1)
- しきい値ヒューリスティック (#171 で改訂):
  - **透過画像判定**: `alpha < 255` のピクセル数が画像全体の **1% 以上** の
    ときだけ「透過画像」扱いとする。1 px 単位の混入 (JPEG → PNG 変換ロスや
    ICC プロファイルの端ピクセル等) で alpha 経路に倒れる事故を防ぐ
  - **透過画像経路**: `alpha >= 128` を inside
  - **不透明画像経路**: 輝度 `Y = 0.299R + 0.587G + 0.114B` で二値化、
    平均輝度を境界に **少数派ピクセル群を inside** とする (auto-polarity)
- **コントラスト不足検出 (#169)**: シルエット抽出が成功しない (= inside 0 個、
  または全画素 inside) 場合は worker が `image-shape-no-contrast` エラーを
  投げ、UI に「この画像にはコントラストがありません」を表示する
- 画像はアスペクト比を保ったまま 256×256 に「contain」リサンプルされ、
  上下/左右の余白は SDF 上の outside になる。これにより縦長/横長の画像も
  シルエットが歪まない
- カラー画像の色情報は捨てる (orber は monochrome ピペライン)
- shape='image' の wasm 経路は内部的に shape='glyph' として扱い、worker は
  upload する SDF テクスチャだけを差し替える (wasm / `crates/wasm` は無改修)。
  wasm に渡すダミー `glyph_char` は `'☆'` (Noto Sans Symbols 2 同梱で必ず
  glyph_supported になる字) を使い、将来 wasm 側で glyph_char バリデーション
  が厳格化されても silent fail しないよう備える (#172 N2)

### 生成トリガー

- aspect / shape / count / speed / softness / glyph 入力 / symbol picker のすべてが即生成
- 旧「ガチャ」チップは撤去し、**control rows の直後・進捗行の直前** に最小の 🔄 アイコンのみを置く（#135 で位置を移動）
- 🔄 は `decoding | generating | animating` の間だけ `orb-spin` で回転し、`prefers-reduced-motion` では停止

### Source 未投入時 (#135)

- 画像をまだ 1 度もドロップ/選択していない（`decoded()` が `null` の）間は、drop zone 以外の controls をすべて disabled にする
- 対象: aspect / shape / glyph input / symbol picker / count / speed / softness / reroll
- 各 control は `decoded()` が null の間 `disabled:opacity-40 disabled:cursor-not-allowed`（GLASS_BTN / GLASS_INPUT に同梱済み）で視覚的に弱める。画像投入後は通常状態へ戻る

## 14. Footer (#128 / #146)

公開後の継続接点 (GH Sponsors / Amazon affiliate / QR / Copyright / Nostalgic
Counter / version) と #86 のプライバシー note を 1 コンポーネントに集約する。
sticky ではなく **Studio の自然なスクロール末尾に着地** し、`border-t border-hairline`
だけで本体から穏やかに分離する (#146 で glass コンテナは廃止)。

### 構成 (中央揃え基調 / #146 再設計)

縦の出現順:

1. **Orb motif** — `●` をサイズ違い (6 / 12 / 22 / 12 / 6 px) で縦に 5 個並べ、`bg-fg` + opacity (0.35 / 0.55 / 0.85 / 0.55 / 0.35) で奥行きを作る。「これは orber」の視覚サイン。`aria-hidden="true"`
2. **A. GH Sponsors** ボタン — `https://github.com/sponsors/kako-jun` を新規タブで開く glass button (DESIGN.md §4 Button)
3. **B. Amazon affiliate × 3** — `<AffiliateGrid />` (`web/src/components/AffiliateGrid.tsx`、#152 で切り出し)。データ層 (`web/src/data/affiliateProducts.ts`、`AFFILIATE_PRODUCTS: AffiliateProduct[]`) と UI 層が分離されており、**他 PWA にコピペで横展開する pattern**。アソシエイト ID は kako-jun の `ultimate-battle-22`、osaka-kenpo と同じく **amzn.to 短縮リンク** を `url` フィールドに直接入れる方式 (Associates ダッシュボードで生成、tag を URL に露出しない)。商品カードは **円形 mask + inset shadow + outer glow** の orb スタイルで、orber 本体の orb ビジュアルと連続性を持たせる (四角サムネイルにしない・glass 矩形でカード化しない)
4. **C. QR コード** — `/orber-qr.png`。**別途指定する PNG を `web/public/orber-qr.png` に置く方式に変更** (#146)。build 時生成 (`gen-qr.mjs` / `qrcode` パッケージ) は廃止
5. **Privacy note** — 「画像はブラウザ内で処理されます」を 1 文だけ残す。orber の境界条件として最後に読ませる
6. **E. Nostalgic Counter + version** — 1 行に並べる: `閲覧数: {N}  v{date}` (ja) / `{N} views  v{date}` (en)。`tabular-nums` で揃える。Counter は `<nostalgic-counter id="orber-PLACEHOLDER" type="total" format="text" />`、embed は Footer の onMount で `https://nostalgic.llll-ll.com/components/visit.js` を `data-orber-nostalgic` フラグ付きで idempotent に注入。**ID が `PLACEHOLDER` の間は Counter 部分のみ非表示にし、embed.js も注入しない** (version 部分は常に表示)。実 ID 取得後に counter が現れる。`__BUILD_DATE__` は Vite の define で JST 日付に literal 置換 (`astro.config.mjs`)
7. **D. Copyright** — `© kako-jun` (年号なし、`font-display font-light text-xs text-fgSubtle`)

### レイアウト

- 全要素を中央揃え (`flex flex-col items-center text-center gap-8`)
- 内側コンテナ: `mx-auto max-w-3xl px-4 py-10` (本体 `main` の `max-w-3xl p-8` と幅軸を揃える)
- 全カラーは tailwind token (`bg`/`fg`/`fgMuted`/`fgSubtle`/`hairline`/`glassBg`/`glassBgHover`/`glassBorder`/`focusRing`) のみ。`#fff` / `rgba(...)` のハードコード禁止

### 文字列 / i18n

すべての可視文字列は `web/src/lib/strings.ts` 経由 (`sponsorLabel` / `sponsorTitle` / `affiliateHeading` / `affiliateDisclosure` / `qrAlt` / `privacyNote` / `viewsLabelPrefix` / `viewsLabelSuffix`)。`viewsLabel` は ja「閲覧数: {n}」/ en「{n} views」と語順が違うため prefix/suffix 2 キーに分けて Counter の Web Component を挟む。`© kako-jun` と `v{date}` は固有名詞・機械生成扱いで素のテキスト。

**#146 で削除したキー**: `qrLabel` / `aboutHeading` / `aboutBody` / `aboutBuiltWith` / `repoLinkLabel`。Footer から「Open on phone」「about / built with / repo link」の自己説明文を引き算した結果。

### カウンター数値の整形

machigai-salad の `VisitorCounter` と同じパターン: counter の `textContent` が値で埋まるまで 100ms × 50 回までポーリングし、`12345` → `12,345` を `toLocaleString()` で整形する。

### Web Component の型付け

`<nostalgic-counter>` は Custom Element のため Solid の JSX intrinsic に存在しない。`web/src/env.d.ts` で `declare module 'solid-js'` 経由に `IntrinsicElements['nostalgic-counter']` を追加して型を通す。同じ `env.d.ts` で `declare const __BUILD_DATE__: string;` も宣言する。

## 15. PWA (#148)

orber を「manifest だけある static site」から、再訪・オフラインに耐える PWA に
する。実装は machigai-salad と同じ「手書き Service Worker + 1 行の register +
PwaInstallPrompt」の薄い構成で、`@vite-pwa/astro` 等の追加依存は入れない。

### Service Worker (`web/public/sw.js`)

- `CACHE_NAME = 'orber-__BUILD_DATE__'` — `__BUILD_DATE__` は `npm run build` の `stamp:sw` 段で `dist/sw.js` に JST 日付 (`YYYY-MM-DD`) を Node 1 行スクリプトで literal 置換する
- precache は最小: `['/', '/manifest.webmanifest']`
- `/_astro/*` (Astro/Vite が content-hash 付きで吐く immutable asset) は **CacheFirst**。ファイル名が変われば中身が違うため、一度 cache に乗ったらネット往復ゼロで返せる。orber の wasm (~700KB-1MB) を毎回 fetch しないための重要施策
- それ以外は **network-first**。レスポンス ok なら `event.waitUntil()` 経由で SW lifetime に縛って `cache.put`、オフライン時はキャッシュ → 503 フォールバック
- **navigation fallback**: `request.mode === 'navigate'` でキャッシュ miss + オフラインなら、precache した `/` を返してアプリシェルで起動できるようにする (PWA shell 戦略)
- `blob:` / `data:` URL は intercept しない (生成結果の DL を SW が握り潰さないため)
- `install` で `skipWaiting()`、`activate` で旧 `CACHE_NAME` を全削除してから `clients.claim()` — 新版デプロイ後 1 ロードで切り替わる

### 登録 (`web/src/layouts/Base.astro`)

`<script is:inline>` で `window.addEventListener('load', ...)` 後に `navigator.serviceWorker.register('/sw.js', { scope: '/' })` を呼ぶ。失敗は `console.warn` だけ。Solid 島は使わず、本体の hydration よりも前に SW 登録の意思表示だけ済ませる。

### Install Prompt (`web/src/components/PwaInstallPrompt.tsx`)

- Solid アイランド。`beforeinstallprompt` を捕まえ、画面下部 (`fixed bottom-4`) にミニトーストを出す
- 文字列は `installPromptBody` / `installBtn` / `installDismiss` (i18n)
- `sessionStorage` で 1 セッション中の dismiss を覚える (`orber-pwa-dismissed`)
- `appinstalled` でトーストを閉じる
- 配色は Footer の glass button と同じ token (`glassBg` / `glassBorder` / `focusRing`)
- `index.astro` で `client:load` (browser の install 可能判定がいつ来ても捕まえられるように)

### キャッシュ対象の境界

- precache: HTML (`/`) + manifest
- CacheFirst: `/_astro/*` (content-hash 付き immutable JS / CSS / wasm)
- runtime cache (network-first 経由で結果的に乗る): フォント / アイコン / QR PNG / 外部 CDN (Google Fonts / Nostalgic embed)
- intercept しない (= cache されない): `blob:` / `data:` (生成結果の DL)

### オフライン体験の前提

- 「再訪時の起動が安定する」のは **最初の online 訪問が完走した後** の話。クリーン状態 + 即オフラインで再訪した場合は、`/_astro/*` (wasm/JS) が cache に乗っていないため真っ黒画面になる
- 一度でも online で起動すれば、`/_astro/*` が CacheFirst に乗り、navigation fallback で `/` の HTML も precache から返せるため、以後は機内モード等でも安定起動する
- wasm を precache に積む選択もあるが、初回ロードが重くなるトレードオフを避けて runtime cache 任せにしている (review S3 で明示)

### 受け入れ条件 (#148)

- standalone install が現実に使える (manifest + SW + install prompt)
- 再訪時の起動が安定する (上記「オフライン体験の前提」の条件下で)
- update 時は `__BUILD_DATE__` で `CACHE_NAME` が変わり、`activate` の旧キャッシュ削除で新版に切り替わる
- 方針が `DESIGN.md §15` と `CLAUDE.md` に残る

## 16. AffiliateGrid 横展開パターン (#152)

orber の Footer に置く 3 商品 Amazon affiliate グリッドは、**他 PWA でも同じ
pattern を継続採用する** (kako-jun の運用方針)。npm パッケージ化はせず、
**コピペで横展開する** 前提で次のように分離する。

### 構造

- `web/src/data/affiliateProducts.ts` — **データ層 (リポごとに違う)**
  - `AffiliateProduct` interface: `{ url, title, imageUrl, caption }`
  - `AFFILIATE_PRODUCTS: AffiliateProduct[]` — そのリポで売る 3 商品
  - 商品の `url` は **amzn.to 短縮リンク** を直接入れる (Associates ダッシュボードで生成)
- `web/src/components/AffiliateGrid.tsx` — **UI 層 (横展開でコピー)**
  - i18n key (`affiliateHeading` / `affiliateDisclosure`) と tailwind token のみ参照
  - 商品カードは円形 orb 風 (`aspect-square` + `rounded-full` + inset/outer shadow)
  - hover で halo 強化 + scale 微増 (`group-hover` で発光感を上げる)

### 横展開の手順 (新しい PWA リポに足すとき)

1. `affiliateProducts.ts` を新リポにコピーし、商品データを差し替える
2. `AffiliateGrid.tsx` をそのままコピー
3. 新リポの strings に `affiliateHeading` / `affiliateDisclosure` を足す (i18n)
4. Footer の Sponsor ボタンの直下に `<AffiliateGrid />` を mount

### カード視覚仕様 (orb/glow スタイル)

- 円形 mask: `aspect-square w-full rounded-full overflow-hidden`
- inset shadow: 内側に向かう暗み (球面の落ち込み感) — `inset 0 0 14px rgba(0,0,0,0.55)`
- outer glow: 柔らかい白い halo — `0 0 12px rgba(255,255,255,0.06)` → hover 時 `0 0 22px rgba(255,255,255,0.18)`
- 商品画像は `object-cover scale-110` で円から少しはみ出させ、hover で `scale-[1.18]` まで寄る
- 画像が 404 / load 失敗したら `visibility: hidden` で枠 (円 + halo) だけ残す
- title / caption は下に `text-xs` で控えめに、orb 本体の主役感を保つ

## Agent Quick Reference

When generating UI for orber:

- Black background, white text, no hue accent — period.
- Buttons are the only glass elements. Tiles, drop area, status text, and the background are flat.
- Logo is `font-display` (Space Grotesk), `font-light`, `lowercase`, wide tracking.
- Selection on tiles = 4 corner L-marks fading in. Never use a check mark, never use a colored ring.
- All transitions are 200ms ease-out on opacity (and optionally background-color / border-color on glass).
- All visible strings come from `web/src/lib/strings.ts` via `t('key')`. Never hard-code Japanese or English.
- Language is auto-detected from `navigator.language` on mount; no language picker exists.
