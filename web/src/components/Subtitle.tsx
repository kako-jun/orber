// orber#62 — Subtitle (Solid island)
// index.astro 直下のサブタイトルを i18n reactive にするための薄いコンポーネント。
// Studio と Subtitle のどちらが先に hydrate されてもよいが、lang 同期の責務は
// こちらに集約している (Studio.tsx の onMount からは setLang を外している)。

import { onMount } from 'solid-js';
import { t, setLang, detectLang } from '../lib/strings';

export default function Subtitle() {
  onMount(() => {
    setLang(detectLang());
    if (typeof document !== 'undefined') {
      document.documentElement.lang = detectLang();
    }
  });
  // Solid の JSX コンパイラは {expr} を effect でラップするため、
  // t() 内部の lang() 呼び出しで自動的に reactive 化される。
  // 明示的なトリガ (data-lang 等) は不要。
  return (
    <p class="font-display text-sm tracking-wide text-fgMuted text-center mt-3 mb-10">
      {t('subtitle')}
    </p>
  );
}
