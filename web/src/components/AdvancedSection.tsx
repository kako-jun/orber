import { Show, createUniqueId, type JSX } from 'solid-js';
import { t } from '../lib/strings';

// orber#55 Phase B / PR #130 review S4: Studio.tsx から advanced 折りたたみ
// セクションを切り出した。Studio.tsx 側の signal はそのまま所有させ、
// accessor + setter を props で受け渡すだけにすることで、状態の所有権を
// 動かさずに JSX だけ移植する。S5: aria-controls を `createUniqueId()` に
// 切り替え、複数 Studio がマウントされた場合の id 衝突を構造的に防ぐ。

// Studio.tsx 側と一致させた preset 文字列型。`''` は initial identity を表す。
type ShapeChoice = 'circle' | 'glyph';
type CountPreset = '' | 'low' | 'mid' | 'high';
type SpeedPreset = '' | 'slow' | 'mid' | 'fast';
type ContrastPreset = '' | 'low' | 'mid' | 'high';

export interface AdvancedSectionProps {
  // Shape
  shape: () => ShapeChoice;
  setShape: (v: ShapeChoice) => void;
  // Glyph 文字 (shape='glyph' のときのみ表示される行)
  glyphChar: () => string;
  setGlyphChar: (v: string) => void;
  glyphCharSupported: () => boolean;
  // Preset 軸
  countPreset: () => CountPreset;
  setCountPreset: (v: CountPreset) => void;
  speedPreset: () => SpeedPreset;
  setSpeedPreset: (v: SpeedPreset) => void;
  contrastPreset: () => ContrastPreset;
  setContrastPreset: (v: ContrastPreset) => void;
  // 折りたたみ開閉
  open: () => boolean;
  setOpen: (v: boolean) => void;
  // glass スタイル統一トークン (Studio.tsx と一致させて UI に regression が
  // 出ないよう、定義箇所を 1 つに保つ意味で props で受け取る)。
  GLASS_BTN: string;
  GLASS_BTN_TOGGLED: string;
}

export default function AdvancedSection(props: AdvancedSectionProps): JSX.Element {
  // S5: aria-controls 用の安定 ID。`createUniqueId()` は SolidJS が SSR と
  // hydration でも一致する ID を返すので、複数 Studio インスタンスが
  // マウントされても衝突しない。
  const panelId = createUniqueId();

  return (
    <div class="rounded border border-hairline">
      <button
        type="button"
        aria-expanded={props.open()}
        aria-controls={panelId}
        onClick={() => props.setOpen(!props.open())}
        class={
          'flex w-full items-center justify-between px-3 py-2 text-sm text-fgMuted ' +
          'hover:text-fg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing ' +
          'transition-colors duration-200 ease-out'
        }
      >
        <span class="inline-flex items-center gap-2">
          {/* 歯車アイコン (DESIGN.md §7 stroke 1.5 / round) */}
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h0a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v0a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
          </svg>
          {t('advancedHeading')}
        </span>
        {/* 開閉インジケータ ▾ / ▸ — opacity だけで状態切替 */}
        <svg
          viewBox="0 0 24 24"
          width="14"
          height="14"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
          class={
            'transition-transform duration-200 ease-out ' +
            (props.open() ? 'rotate-180' : '')
          }
        >
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>
      <Show when={props.open()}>
        <div
          id={panelId}
          class="fade-in space-y-3 border-t border-hairline px-3 py-3"
        >
          {/* 形状 (Shape) — 2 択 segmented */}
          <div class="flex items-center gap-3">
            <span class="w-20 shrink-0 text-sm text-fgMuted">
              {t('shapeLabel')}
            </span>
            <div class="flex flex-wrap gap-1">
              <button
                type="button"
                aria-pressed={props.shape() === 'circle'}
                onClick={() => props.setShape('circle')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.shape() === 'circle' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('shapeOptionCircle')}
              </button>
              <button
                type="button"
                aria-pressed={props.shape() === 'glyph'}
                onClick={() => props.setShape('glyph')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.shape() === 'glyph' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('shapeOptionGlyph')}
              </button>
            </div>
          </div>

          {/* Glyph 文字入力欄 — Glyph 選択時のみ */}
          <Show when={props.shape() === 'glyph'}>
            <div class="flex items-center gap-3">
              <span class="w-20 shrink-0 text-sm text-fgMuted">
                {t('glyphCharLabel')}
              </span>
              <input
                type="text"
                aria-label={t('glyphCharLabel')}
                value={props.glyphChar()}
                placeholder={t('glyphCharPlaceholder')}
                // N1: surrogate pair (絵文字 1 個 = UTF-16 2 code units) を許容するため
                // maxLength=2。`raw[0]` は UTF-16 code unit で surrogate pair の片方
                // しか取れないので `[...raw][0]` で UTF-32 単位の 1 grapheme を取る。
                maxLength={2}
                onInput={(e) => {
                  const raw = e.currentTarget.value;
                  // [...raw][0] で UTF-32 単位の 1 grapheme を取る (raw[0] は UTF-16
                  // code unit で surrogate pair の片方しか取れない)
                  const first = [...raw][0] ?? '';
                  props.setGlyphChar(first);
                  // 入力欄の表示も同期（サニタイズ反映）。
                  if (raw !== first) e.currentTarget.value = first;
                }}
                class={
                  'w-16 rounded border bg-glassBg backdrop-blur-glass px-2 py-1 text-center text-sm text-fg ' +
                  'focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing ' +
                  (props.glyphCharSupported()
                    ? 'border-glassBorder'
                    : 'border-fgMuted')
                }
              />
              <Show
                when={!props.glyphCharSupported() && props.glyphChar().length > 0}
              >
                <span class="text-xs text-fgMuted">
                  {t('glyphCharUnsupported')}
                </span>
              </Show>
            </div>
          </Show>

          {/* 数 (Count) */}
          <div class="flex items-center gap-3">
            <span class="w-20 shrink-0 text-sm text-fgMuted">{t('countLabel')}</span>
            <div class="flex flex-wrap gap-1">
              <button
                type="button"
                aria-pressed={props.countPreset() === 'low'}
                onClick={() => props.setCountPreset('low')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.countPreset() === 'low' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('countOptionLow')}
              </button>
              <button
                type="button"
                // M1: '' (initial identity) と 'mid' (明示選択) のどちらでも
                // 「標準」を押下表示する。どちらも wasm 側で identity 扱いになる。
                aria-pressed={
                  props.countPreset() === '' || props.countPreset() === 'mid'
                }
                onClick={() => props.setCountPreset('mid')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.countPreset() === '' || props.countPreset() === 'mid'
                    ? props.GLASS_BTN_TOGGLED
                    : '')
                }
              >
                {t('countOptionMid')}
              </button>
              <button
                type="button"
                aria-pressed={props.countPreset() === 'high'}
                onClick={() => props.setCountPreset('high')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.countPreset() === 'high' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('countOptionHigh')}
              </button>
            </div>
          </div>

          {/* 速さ (Speed) */}
          <div class="flex items-center gap-3">
            <span class="w-20 shrink-0 text-sm text-fgMuted">{t('speedLabel')}</span>
            <div class="flex flex-wrap gap-1">
              <button
                type="button"
                aria-pressed={props.speedPreset() === 'slow'}
                onClick={() => props.setSpeedPreset('slow')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.speedPreset() === 'slow' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('speedOptionSlow')}
              </button>
              <button
                type="button"
                // M1: '' / 'mid' どちらも標準扱い。'' は spec.speed / GUI_VIDEO_SPEEDS
                // を温存、'mid' も identity（parse_speed_preset で None）。
                aria-pressed={
                  props.speedPreset() === '' || props.speedPreset() === 'mid'
                }
                onClick={() => props.setSpeedPreset('mid')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.speedPreset() === '' || props.speedPreset() === 'mid'
                    ? props.GLASS_BTN_TOGGLED
                    : '')
                }
              >
                {t('speedOptionMid')}
              </button>
              <button
                type="button"
                aria-pressed={props.speedPreset() === 'fast'}
                onClick={() => props.setSpeedPreset('fast')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.speedPreset() === 'fast' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('speedOptionFast')}
              </button>
            </div>
          </div>

          {/* コントラスト (Contrast) */}
          <div class="flex items-center gap-3">
            <span class="w-20 shrink-0 text-sm text-fgMuted">
              {t('contrastLabel')}
            </span>
            <div class="flex flex-wrap gap-1">
              <button
                type="button"
                aria-pressed={props.contrastPreset() === 'low'}
                onClick={() => props.setContrastPreset('low')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.contrastPreset() === 'low' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('contrastOptionLow')}
              </button>
              <button
                type="button"
                // M1: '' / 'mid' どちらも標準扱い。ContrastPreset::Mid と等価。
                aria-pressed={
                  props.contrastPreset() === '' || props.contrastPreset() === 'mid'
                }
                onClick={() => props.setContrastPreset('mid')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.contrastPreset() === '' || props.contrastPreset() === 'mid'
                    ? props.GLASS_BTN_TOGGLED
                    : '')
                }
              >
                {t('contrastOptionMid')}
              </button>
              <button
                type="button"
                aria-pressed={props.contrastPreset() === 'high'}
                onClick={() => props.setContrastPreset('high')}
                class={
                  props.GLASS_BTN +
                  ' text-sm ' +
                  (props.contrastPreset() === 'high' ? props.GLASS_BTN_TOGGLED : '')
                }
              >
                {t('contrastOptionHigh')}
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
