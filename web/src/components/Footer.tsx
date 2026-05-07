// orber#146 / #152 — Footer (Solid island, redesigned)
//
// 旧 #128 実装は左右 2 列・glass コンテナ・自己説明文 (about / built with /
// repo link / "Open on phone") を詰め込んでおり、orber 本体の引き算 UI から
// 浮いていた。#146 で以下に再設計し、#152 で Amazon affiliate を実商品 +
// orb/glow カード (AffiliateGrid) に切り出した。
//
//   - 中央揃えに統一
//   - glass コンテナを廃し、border-t のみで穏やかに区切る
//   - 自己説明文 (aboutBody / aboutBuiltWith / repoLinkLabel / qrLabel) を削除
//   - Copyright から年号を外す (`© kako-jun`)
//   - QR は build 時生成の SVG ではなく、別途指定する PNG (`/orber-qr.png`) を使う
//   - Footer の入口にオーブモチーフ (●) をサイズ違いで縦 5 個並べ、
//     「これは orber」の視覚サインを置く (DESIGN.md §14)
//   - Nostalgic Counter と build version (`v2026-05-07` 等) を 1 行に並べる
//     (machigai-salad の VisitorCounter と同じパターン)
//   - Amazon affiliate は `<AffiliateGrid />` (Sponsor の直下、#152 で切り出し)
//
// ハードコード禁止: カラーは tailwind token (bg / fg / fgMuted / fgSubtle /
// hairline / glassBg / glassBgHover / glassBorder / focusRing) のみ。
//
// Web Components: <nostalgic-counter> は env.d.ts で IntrinsicElements に追加済み。

import { onMount } from 'solid-js';
import { t } from '../lib/strings';
import AffiliateGrid from './AffiliateGrid';

// #128: Nostalgic Counter の実 ID は kako-jun が
// https://nostalgic.llll-ll.com/ のダッシュボードで取得後に置換する。
// TODO(kako-jun): 実 ID に置換 (例 "orber-xxxxxxxx")。
const NOSTALGIC_COUNTER_ID = 'orber-PLACEHOLDER';

// placeholder の間は Counter 部分を非表示にする。embed.js が "Counter not found"
// 等のテキストを表示しないように完全 mount しない。
const NOSTALGIC_COUNTER_ENABLED = !NOSTALGIC_COUNTER_ID.endsWith('PLACEHOLDER');

const NOSTALGIC_EMBED_SRC = 'https://nostalgic.llll-ll.com/components/visit.js';

function ensureNostalgicEmbed(): void {
  if (typeof document === 'undefined') return;
  if (document.querySelector('script[data-orber-nostalgic]')) return;
  const s = document.createElement('script');
  s.src = NOSTALGIC_EMBED_SRC;
  s.async = true;
  s.dataset.orberNostalgic = '1';
  document.head.appendChild(s);
}

// machigai-salad/components/VisitorCounter.tsx と同じパターン: counter mount 後に
// テキスト (`12345`) を `toLocaleString()` でカンマ区切り (`12,345`) に整形する。
// 5 秒以内に値が入らなければ諦める (max 50 回 × 100ms)。
const MAX_POLL_ATTEMPTS = 50;

function formatCounterAfterMount(root: HTMLElement): void {
  let attempts = 0;
  const timer = window.setInterval(() => {
    attempts += 1;
    if (attempts >= MAX_POLL_ATTEMPTS) {
      window.clearInterval(timer);
      return;
    }
    const counter = root.querySelector('nostalgic-counter');
    const txt = counter?.textContent ?? '';
    if (txt && txt !== '0') {
      const num = txt.replace(/,/g, '');
      if (/^\d+$/.test(num) && counter) {
        counter.textContent = parseInt(num, 10).toLocaleString();
      }
      window.clearInterval(timer);
    }
  }, 100);
}

// Footer 入口のオーブモチーフ。●をサイズ違いで縦 5 個。
// 中央 (3 つ目) を最大にし、上下に向かって縮ませることで奥行きを出す。
// fg トークンを使い、opacity だけで濃淡を作る (色トークン外しない)。
const ORB_DOTS: { size: number; opacity: number }[] = [
  { size: 6, opacity: 0.35 },
  { size: 12, opacity: 0.55 },
  { size: 22, opacity: 0.85 },
  { size: 12, opacity: 0.55 },
  { size: 6, opacity: 0.35 },
];

export default function Footer() {
  let counterRootRef: HTMLDivElement | undefined;

  onMount(() => {
    if (NOSTALGIC_COUNTER_ENABLED) {
      ensureNostalgicEmbed();
      if (counterRootRef) {
        formatCounterAfterMount(counterRootRef);
      }
    }
  });

  // #146: vite.define で build 時に literal 置換される。
  const buildDate = __BUILD_DATE__;

  return (
    <footer
      class="mt-16 border-t border-hairline"
      aria-label={t('footerAriaLabel')}
    >
      <div class="mx-auto max-w-3xl px-4 py-10 flex flex-col items-center text-center gap-8">
        {/* Orb motif — 縦 5 個のドット (DESIGN.md §14) */}
        <div
          class="flex flex-col items-center gap-2 py-2"
          aria-hidden="true"
        >
          {ORB_DOTS.map((dot) => (
            <span
              class="block rounded-full bg-fg"
              style={{
                width: `${dot.size}px`,
                height: `${dot.size}px`,
                opacity: dot.opacity,
              }}
            />
          ))}
        </div>

        {/* A. GH Sponsors */}
        <a
          href="https://github.com/sponsors/kako-jun"
          target="_blank"
          rel="noopener noreferrer"
          title={t('sponsorTitle')}
          class="inline-flex items-center gap-2 rounded-md border border-glassBorder bg-glassBg hover:bg-glassBgHover px-3 py-2 text-sm text-fg transition-colors duration-200 ease-out focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
        >
          <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
          </svg>
          <span>{t('sponsorLabel')}</span>
        </a>

        {/* B. Amazon affiliate × 3 — #152 で AffiliateGrid に切り出し。
            データ層 (web/src/data/affiliateProducts.ts) と UI 層を分離し、
            他 PWA にコピペで横展開できる pattern にしている。 */}
        <AffiliateGrid />

        {/* C. QR — 別途指定する PNG (`/orber-qr.png`) を使う。補助コピーは置かない。 */}
        <img
          src="/orber-qr.png"
          alt={t('qrAlt')}
          width="120"
          height="120"
          class="block rounded-sm border border-hairline bg-bg"
        />

        {/* Privacy — orber の境界条件 (画像はブラウザ内処理) はここに残す。 */}
        <p class="text-xs text-fgMuted leading-relaxed max-w-md">
          {t('privacyNote')}
        </p>

        {/* Counter + version (1 行、tabular-nums)。
            #146 review S1: Counter 非表示時に version 単独になるため justify-center で
            親の text-center に揃えておく。 */}
        <div
          ref={counterRootRef}
          class="text-xs text-fgSubtle flex items-center justify-center gap-3"
          style={{ 'font-variant-numeric': 'tabular-nums' }}
        >
          {NOSTALGIC_COUNTER_ENABLED && (
            <span>
              <span>{t('viewsLabelPrefix')}</span>
              <nostalgic-counter
                id={NOSTALGIC_COUNTER_ID}
                type="total"
                format="text"
              />
              <span>{t('viewsLabelSuffix')}</span>
            </span>
          )}
          <span>v{buildDate}</span>
        </div>

        {/* D. Copyright — 年号なし */}
        <p class="font-display font-light text-xs text-fgSubtle">
          © kako-jun
        </p>
      </div>
    </footer>
  );
}
