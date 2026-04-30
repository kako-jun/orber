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
    ja: '縦長 9:16（プレビュー 540×960 / DL 1080×1920）',
    en: 'Portrait 9:16 (preview 540×960, DL 1080×1920)',
  },
  aspectLandscape: { ja: '横長', en: 'Landscape' },
  aspectLandscapeTitle: {
    ja: '横長 16:9（プレビュー 960×540 / DL 1920×1080）',
    en: 'Landscape 16:9 (preview 960×540, DL 1920×1080)',
  },
  rerollLabel: { ja: '同じ画像でガチャ', en: 'Roll again' },
  rerollTitle: {
    ja: '同じ画像でもう一度ガチャ',
    en: 'Roll again with the same image',
  },
  wasmLoadFailed: {
    ja: 'wasm の読み込みに失敗しました',
    en: 'Failed to load wasm',
  },
  decoding: { ja: '画像をデコード中…', en: 'Decoding image…' },
  generating: { ja: '生成中…', en: 'Generating…' },
  animating: { ja: '動画化中…', en: 'Animating…' },
  videoPendingBadge: { ja: '動画化中', en: 'Animating' },
  animateError: {
    ja: '動画生成に失敗したタイルがあります',
    en: 'Some tiles failed to encode to video',
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
