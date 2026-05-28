// orber#62 — Subtitle (Solid island)
// index.astro 直下のサブタイトルを i18n reactive にするための薄いコンポーネント。
// 言語トグルは LangToggle.tsx に分離し、画面右上に fixed 配置している。

import { t } from '../lib/strings';

export default function Subtitle() {
  return (
    <p class="font-display text-sm tracking-wide text-fgMuted text-center mt-3 mb-10">
      {t('subtitle')}
    </p>
  );
}
