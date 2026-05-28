// 画面右上に fixed 配置する JA / EN 言語トグル。
// Subtitle 内のサブタイトル直下に置いていたものを画面右上に分離した。

import { onMount } from 'solid-js';
import { setLang, detectLang, lang } from '../lib/strings';

export default function LangToggle() {
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
    <div class="fixed top-3 right-3 z-50 flex items-center gap-1 text-xs text-fgSubtle">
      <button
        type="button"
        onClick={() => switchLang('ja')}
        class="font-display rounded px-1.5 py-0.5 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing"
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
        class="font-display rounded px-1.5 py-0.5 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing"
        classList={{ 'text-fg font-semibold': lang() === 'en', 'text-fgSubtle': lang() !== 'en' }}
        aria-label="English"
        aria-pressed={lang() === 'en'}
      >
        EN
      </button>
    </div>
  );
}
