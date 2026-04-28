// orber#62 — Subtitle (Solid island)
// index.astro 直下のサブタイトルを i18n reactive にするための薄いコンポーネント。
// Studio と同じく onMount で setLang(detectLang()) を呼ぶ。両方 setLang するが
// 同じ値を入れるだけなので問題ない。Subtitle が先に hydrate することで、
// ロゴ周辺がすぐに翻訳された状態になる。

import { onMount } from 'solid-js';
import { t, lang, setLang, detectLang } from '../lib/strings';

export default function Subtitle() {
  onMount(() => {
    setLang(detectLang());
    if (typeof document !== 'undefined') {
      document.documentElement.lang = detectLang();
    }
  });
  // `data-lang={lang()}` で signal を読むことで再描画トリガにする。
  // Solid の reactive は JSX 内の signal 呼び出しで成立するため、
  // {t('subtitle')} だけだと初期評価で固定されてしまう。
  return (
    <p class="font-display text-sm tracking-wide text-fgMuted text-center mt-3 mb-10">
      <span data-lang={lang()}>{t('subtitle')}</span>
    </p>
  );
}
