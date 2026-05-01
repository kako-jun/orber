// orber#62 — i18n module.
// 言語切替 UI は持たない。`detectLang()` でブラウザ言語を見て ja / en を返し、
// `t(key, vars?)` で文字列を取り出す。SolidJS signal `lang` を更新すると
// 全ての `t()` 呼び出しが reactive に再評価される。
//
// 文字列は subtitle / dropZoneLabel ... など 20 弱のキーを集約する。
// aria-label / title / プレースホルダ・エラー文言までここに含める。

import { createSignal } from 'solid-js';

export type Lang = 'ja' | 'en';

export const STRINGS = {
  subtitle: {
    ja: '画像から街の光を抽出。配信や動画の背景に。',
    en: 'Extract city lights from any image. Use as a video or stream background.',
  },
  dropZoneLabel: {
    ja: '画像ファイル選択 / ドラッグ&ドロップ',
    en: 'Choose or drop an image',
  },
  dropZonePlaceholder: {
    ja: '画像をドロップ / クリック',
    en: 'Drop or click an image',
  },
  replaceImageHint: { ja: '差し替え', en: 'Replace' },
  pickedThumbAlt: {
    ja: '選択した画像: {name}',
    en: 'Picked image: {name}',
  },
  aspectPortrait: { ja: '縦長', en: 'Portrait' },
  aspectPortraitTitle: {
    ja: '縦長 9:16（プレビュー 360×640 / DL 1080×1920）',
    en: 'Portrait 9:16 (preview 360×640, DL 1080×1920)',
  },
  aspectLandscape: { ja: '横長', en: 'Landscape' },
  aspectLandscapeTitle: {
    ja: '横長 16:9（プレビュー 640×360 / DL 1920×1080）',
    en: 'Landscape 16:9 (preview 640×360, DL 1920×1080)',
  },
  rerollLabel: { ja: '同じ画像でガチャ', en: 'Roll again' },
  rerollTitle: {
    ja: '同じ画像でもう一度ガチャ',
    en: 'Roll again with the same image',
  },
  // Phase B (#55): ガチャを唯一の生成トリガーに昇格。aspect 即生成は廃止。
  gachaLabel: { ja: 'ガチャを引く', en: 'Roll' },
  gachaTitle: {
    ja: '現在の設定でガチャを引いて 12 枚生成',
    en: 'Roll a new batch of 12 with the current settings',
  },
  // Phase B (#55): アドバンスト折りたたみセクション。
  advancedHeading: { ja: 'アドバンスト', en: 'Advanced' },
  shapeLabel: { ja: '形状', en: 'Shape' },
  shapeOptionCircle: { ja: '円', en: 'Circle' },
  shapeOptionGlyph: { ja: '文字', en: 'Glyph' },
  glyphCharLabel: { ja: '文字', en: 'Character' },
  glyphCharPlaceholder: { ja: '例: ☆', en: 'e.g., ☆' },
  glyphCharUnsupported: {
    ja: '同梱フォントに収録されていません',
    en: 'Not in bundled font',
  },
  countLabel: { ja: '数', en: 'Count' },
  countOptionLow: { ja: '少なめ', en: 'Few' },
  countOptionMid: { ja: '標準', en: 'Standard' },
  countOptionHigh: { ja: '多め', en: 'Many' },
  speedLabel: { ja: '速さ', en: 'Speed' },
  speedOptionSlow: { ja: 'ゆっくり', en: 'Slow' },
  speedOptionMid: { ja: '標準', en: 'Standard' },
  speedOptionFast: { ja: '速め', en: 'Fast' },
  contrastLabel: { ja: 'コントラスト', en: 'Contrast' },
  contrastOptionLow: { ja: '弱め', en: 'Soft' },
  contrastOptionMid: { ja: '標準', en: 'Standard' },
  contrastOptionHigh: { ja: '強め', en: 'Strong' },
  wasmLoadFailed: {
    ja: 'wasm の読み込みに失敗しました',
    en: 'Failed to load wasm',
  },
  decoding: { ja: '画像をデコード中…', en: 'Decoding image…' },
  generating: { ja: '生成中…', en: 'Generating…' },
  animating: { ja: '動画化中…', en: 'Animating…' },
  // #124: 生成完了後、進捗行を空白にせず長押し拡大の操作ヒントとして再利用する。
  // 用語は DESIGN.md §4 PreviewOverlay と既存コード（LONG_PRESS_MS / isLongPress）に
  // 合わせて "Long-press" / 「長押し」を採用。"Long tap" は touch を強く示唆するため
  // マウス/トラックパッドでも動く現実装には不適。
  longPressEnlargeHint: {
    ja: '長押しで拡大',
    en: 'Long-press to enlarge',
  },
  videoPendingBadge: { ja: '動画化中', en: 'Animating' },
  animateError: {
    ja: '動画生成に失敗したタイルがあります',
    en: 'Some tiles failed to encode to video',
  },
  // #94: 部分失敗 warning 用。fatal 風に響かないよう「一部のタイルは
  // 静止画のままです」という事実ベースの言い回しに分けてある。
  animatePartialFailure: {
    ja: '一部のタイルは動画化できず、静止画のままです',
    en: 'Some tiles could not be animated and remain as stills',
  },
  downloadSelected: { ja: '選択を DL', en: 'Download selected' },
  downloadAll: { ja: '全 {n} 枚 DL', en: 'Download all {n}' },
  preparingDownload: {
    ja: '高解像度版を準備中… {done} / {total}',
    en: 'Rendering high-res… {done} / {total}',
  },
  downloadFailed: {
    ja: 'ダウンロード準備に失敗しました',
    en: 'Failed to prepare download',
  },
  variationAlt: { ja: 'バリエーション {n}', en: 'Variation {n}' },
  variationAnimatedAlt: {
    ja: 'バリエーション {n} (動画)',
    en: 'Variation {n} (animated)',
  },
} as const;

export function detectLang(): Lang {
  if (typeof navigator === 'undefined') return 'en';
  return navigator.language.toLowerCase().startsWith('ja') ? 'ja' : 'en';
}

// SSR-safe: Astro の SSR 段階では navigator が無いので en で初期化される。
// hydration 後 onMount で setLang(detectLang()) を呼ぶことで ja に切り替わる。
const [lang, setLang] = createSignal<Lang>('en');
export { lang, setLang };

export type StringKey = keyof typeof STRINGS;

export function t<K extends StringKey>(
  key: K,
  vars?: Record<string, string | number>,
): string {
  let s: string = STRINGS[key][lang()];
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}
