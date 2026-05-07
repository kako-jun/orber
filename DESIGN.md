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

The same tokens drive the **transparent-DL checkbox** introduced for #56 ("透過版を DL に含める / Include transparent versions"). It sits directly under the aspect toggles (centred, `col-span-2`) so all download-affecting settings cluster together at the top of the grid. When the browser cannot encode VP9 alpha (`VideoEncoder.isConfigSupported({codec:'vp09.00.10.08', alpha:'keep'})` is rejected — currently Safari) the checkbox is forced disabled with a tooltip, rather than offering a partial fallback. The OFF state is byte-exact with the pre-#56 download path (the alpha worker calls are simply never made).

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
- `<html lang>` is updated **pre-hydration** by an `is:inline` script in `Base.astro` that reads `navigator.language` synchronously, so screen readers pick the correct voice from first paint. The Solid `lang` signal is then synchronized **post-hydration** by `Subtitle.tsx` (`onMount → setLang(detectLang())`) for reactive UI text. The two paths are intentionally separated: the document attribute is a11y-critical and must be early; the signal is reactivity-only
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
- `compositionstart` / `compositionend` 中は `glyph_supported()` RPC を飛ばさない
- 同梱フォントに無い文字は小さな muted warning を 1 行出す
- シンボルピッカーは実際に `glyph_supported()` が `true` な文字だけ残す
- glyph の描画 backend は alpha mask ではなく **SDF + 共通 falloff**。picker UI は
  変えず、見た目だけ circle と同じ「ぼけた光」に寄せる
- glyph は seed 由来の `base_angle` で始まり、静止画でも向きがばらける
- 動画中の glyph はそこからさらに orb ごとに異なる向き・回転方向・
  回転速度で連続回転する

### 生成トリガー

- aspect / shape / count / speed / softness / glyph 入力 / symbol picker のすべてが即生成
- 旧「ガチャ」チップは撤去し、**control rows の直後・進捗行の直前** に最小の 🔄 アイコンのみを置く（#135 で位置を移動）
- 🔄 は `decoding | generating | animating` の間だけ `orb-spin` で回転し、`prefers-reduced-motion` では停止

### Source 未投入時 (#135)

- 画像をまだ 1 度もドロップ/選択していない（`decoded()` が `null` の）間は、drop zone 以外の controls をすべて disabled にする
- 対象: aspect / shape / glyph input / symbol picker / count / speed / softness / reroll
- 各 control は `decoded()` が null の間 `disabled:opacity-40 disabled:cursor-not-allowed`（GLASS_BTN / GLASS_INPUT に同梱済み）で視覚的に弱める。画像投入後は通常状態へ戻る

## 14. Footer (#128)

公開後の継続接点 (GH Sponsors / Amazon affiliate / QR / Copyright / Nostalgic
Counter) と #86 (About / Donate のプライバシー note) を 1 コンポーネントに集約する。
sticky ではなく **Studio の自然なスクロール末尾に着地** し、`border-t border-hairline`
+ `bg-glassBg` で本体から穏やかに分離する。

### 構成 (A〜E + Privacy)

- **A. GH Sponsors** ボタン — `https://github.com/sponsors/kako-jun` を新規タブで開く glass button (DESIGN.md §4 Button)
- **B. Amazon affiliate × 3** — アソシエイト ID `ultimate-battle-22`。商品データは `web/src/data/affiliateProducts.ts` に集約され、`amazonUrl(asin)` が `?tag=ultimate-battle-22` を必ず付ける唯一の出口
- **C. QR コード** — `/orber-qr.svg`。`web/scripts/gen-qr.mjs` が `qrcode` パッケージで build 時に再生成 (`npm run dev` / `npm run build` / `build:cf` の前段に `npm run gen:qr` を連結済み)、bg `#040404` / fg `#FFFFFF`。`https://orber.llll-ll.com/` を指す
- **D. Copyright** — `© 2026 kako-jun` (`font-display font-light text-xs text-fgSubtle`)
- **E. Nostalgic Counter** — `<nostalgic-counter id="orber-PLACEHOLDER" type="total" format="text" />`。embed は Footer の onMount で `https://nostalgic.llll-ll.com/components/visit.js` を `data-orber-nostalgic` フラグ付きで idempotent に注入。**ID が `PLACEHOLDER` の間は Counter ブロック自体を非表示にし、embed.js も注入しない** (実 ID 取得後に表示開始)。kako-jun がダッシュボードで取得した値に置換 (TODO コメント明記)
- **About + Privacy + Source (#86 統合)** — 「orber が何を作るか / 画像は端末内処理 / GitHub repo link / ビルド技術スタック」を `text-fgMuted` 4 行で右列に集約。privacyNote 単体ではなく、About + Privacy + Source link を 1 セクションに束ねることで「最後に読まれる場所」として境界条件・OSS・作者を同時に宣言する

### レイアウト

- モバイル: 縦積み (`flex-col` 相当の `grid gap-10`)
- デスクトップ (`md:`): 2 列 — 左 = A (Sponsor) + B (Amazon)、右 = C (QR) + Privacy + E (Counter) + D (Copyright)
- 内側コンテナ: `mx-auto max-w-3xl px-4 py-10` (本体 `main` の `max-w-3xl p-8` と幅軸を揃える)
- 全カラーは tailwind token (`bg`/`fg`/`fgMuted`/`fgSubtle`/`hairline`/`glassBg`/`glassBgHover`/`glassBorder`/`focusRing`) のみ。`#fff` / `rgba(...)` のハードコード禁止

### 文字列 / i18n

すべての可視文字列は `web/src/lib/strings.ts` 経由 (`sponsorLabel` / `sponsorTitle` / `affiliateHeading` / `affiliateDisclosure` / `qrLabel` / `qrAlt` / `privacyNote` / `viewsLabelPrefix` / `viewsLabelSuffix` / `aboutHeading` / `aboutBody` / `aboutBuiltWith` / `repoLinkLabel`)。`viewsLabel` は ja「閲覧数: {n}」/ en「{n} views」と語順が違うため prefix/suffix 2 キーに分けて Counter の Web Component を挟む。`© 2026 kako-jun` は固有名詞扱いで素のテキスト。

### Web Component の型付け

`<nostalgic-counter>` は Custom Element のため Solid の JSX intrinsic に存在しない。`web/src/env.d.ts` で `declare module 'solid-js'` 経由に `IntrinsicElements['nostalgic-counter']` を追加して型を通す。

## 15. PWA (#148)

orber を「manifest だけある static site」から、再訪・オフラインに耐える PWA に
する。実装は machigai-salad と同じ「手書き Service Worker + 1 行の register +
PwaInstallPrompt」の薄い構成で、`@vite-pwa/astro` 等の追加依存は入れない。

### Service Worker (`web/public/sw.js`)

- `CACHE_NAME = 'orber-__BUILD_DATE__'` — `__BUILD_DATE__` は `npm run build` の `stamp:sw` 段で `dist/sw.js` に JST 日付 (`YYYY-MM-DD`) を Node 1 行スクリプトで literal 置換する
- precache は最小: `['/', '/manifest.webmanifest']`
- fetch は **network-first**。レスポンス ok ならキャッシュに追記、オフライン時はキャッシュ → 503 フォールバック
- `blob:` / `data:` URL は intercept しない (生成結果の DL を SW が握り潰さないため)
- `install` で `skipWaiting()`、`activate` で旧 `CACHE_NAME` を全削除して `clients.claim()` — 新版デプロイ後 1 ロードで切り替わる

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

- precache: HTML + manifest
- runtime cache (network-first 経由で結果的に乗る): JS / CSS / wasm / フォント / アイコン / QR PNG
- intercept しない (= cache されない): `blob:` / `data:` (生成結果の DL)、外部 CDN (Google Fonts / Nostalgic embed) は SW の経路に乗るので runtime キャッシュに乗るが、一時的な network 失敗で古いものが返るだけで実害はない

### 受け入れ条件 (#148)

- standalone install が現実に使える (manifest + SW + install prompt)
- 再訪時の起動が安定する (precache + network-first fallback)
- update 時は `__BUILD_DATE__` で `CACHE_NAME` が変わり、`activate` の旧キャッシュ削除で新版に切り替わる
- 方針が `DESIGN.md §15` と `CLAUDE.md` に残る

## Agent Quick Reference

When generating UI for orber:

- Black background, white text, no hue accent — period.
- Buttons are the only glass elements. Tiles, drop area, status text, and the background are flat.
- Logo is `font-display` (Space Grotesk), `font-light`, `lowercase`, wide tracking.
- Selection on tiles = 4 corner L-marks fading in. Never use a check mark, never use a colored ring.
- All transitions are 200ms ease-out on opacity (and optionally background-color / border-color on glass).
- All visible strings come from `web/src/lib/strings.ts` via `t('key')`. Never hard-code Japanese or English.
- Language is auto-detected from `navigator.language` on mount; no language picker exists.
