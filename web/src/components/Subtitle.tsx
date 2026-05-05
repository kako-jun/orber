// orber#62 — Subtitle (Solid island)
// index.astro 直下のサブタイトルを i18n reactive にするための薄いコンポーネント。
// Studio と Subtitle のどちらが先に hydrate されてもよいが、lang 同期の責務は
// こちらに集約している (Studio.tsx の onMount からは setLang を外している)。
//
// orber#134 — lang signal は strings.ts のモジュール init 時点で detectLang() で
// 初期化済み (SSR は navigator 未定義のため en フォールバック)。ここでの
// setLang / document.documentElement.lang 更新は safety belt として残す
// (detectLang は冪等で副作用なし、Base.astro inline script との二重保険)。

import { onMount } from 'solid-js';
import { t, setLang, detectLang } from '../lib/strings';

export default function Subtitle() {
  onMount(() => {
    // safety belt: 既に strings.ts module init で設定済みだが、SSR→hydration
    // の境界で食い違いがあれば再同期する。冪等。
    setLang(detectLang());
    if (typeof document !== 'undefined') {
      document.documentElement.lang = detectLang();
    }
  });
  // Solid の JSX コンパイラは {expr} を effect でラップするため、
  // t() 内部の lang() 呼び出しで自動的に reactive 化される。
  // 明示的なトリガ (data-lang 等) は不要。
  // Subtitle は SSR で既に描画されており above-the-fold で常時可視のため、
  // .fade-in は付けない (hydration で再フェードするとちらつく)。
  // これは DESIGN.md §6 「.fade-in は新規マウント時の出現演出」の対象外。
  return (
    <p class="font-display text-sm tracking-wide text-fgMuted text-center mt-3 mb-10">
      {t('subtitle')}
    </p>
  );
}
