// orber#62 — Subtitle (Solid island)
// index.astro 直下のサブタイトルを i18n reactive にするための薄いコンポーネント。
// 言語トグル (JA / EN) もここに内包する。

import { onMount } from 'solid-js';
import { t, setLang, detectLang, lang } from '../lib/strings';

export default function Subtitle() {
  onMount(() => {
    const next = detectLang();
    setLang(next);
    if (typeof document !== 'undefined') {
      document.documentElement.lang = next;
    }
  });

  function switchLang(l: 'ja' | 'en') {
    setLang(l);
    if (typeof document !== 'undefined') {
      document.documentElement.lang = l;
    }
  }

  return (
    <div class="flex flex-col items-center mt-3 mb-10 gap-1">
      <p class="font-display text-sm tracking-wide text-fgMuted text-center">
        {t('subtitle')}
      </p>
      {/* 言語トグル — 小さく、控えめに */}
      <div class="flex items-center gap-1 text-xs text-fgSubtle">
        <button
          type="button"
          onClick={() => switchLang('ja')}
          class="rounded px-1.5 py-0.5 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing"
          classList={{ 'text-fg font-semibold': lang() === 'ja', 'text-fgSubtle': lang() !== 'ja' }}
          aria-label="日本語"
          aria-pressed={lang() === 'ja'}
        >
          JA
        </button>
        <span class="text-fgSubtle" style={{ opacity: 0.4 }}>·</span>
        <button
          type="button"
          onClick={() => switchLang('en')}
          class="rounded px-1.5 py-0.5 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing"
          classList={{ 'text-fg font-semibold': lang() === 'en', 'text-fgSubtle': lang() !== 'en' }}
          aria-label="English"
          aria-pressed={lang() === 'en'}
        >
          EN
        </button>
      </div>
    </div>
  );
}
