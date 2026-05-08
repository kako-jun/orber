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

// #128 / #148: Nostalgic Counter ID。
// `https://api.nostalgic.llll-ll.com/visit?action=create` POST で発行済み。
// URL: https://orber.llll-ll.com、token は kako-jun 統一値 (`ekumetoteroesu`)。
const NOSTALGIC_COUNTER_ID = 'orber-11532f39';

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
  { size: 3, opacity: 0.35 },
  { size: 6, opacity: 0.55 },
  { size: 10, opacity: 0.85 },
  { size: 6, opacity: 0.55 },
  { size: 3, opacity: 0.35 },
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
      class="mt-16"
      aria-label={t('footerAriaLabel')}
    >
      {/* #174: 旧 border-t border-hairline は削除。区切り線をやめてオーブ
          モチーフ (●×5) の上下が同サイズの余白になるよう、上端 pt-10 を据える。
          下端は main の p-8 (32px) のみに頼り、footer 自体の bottom padding
          は pb-0 で削る (旧実装と同じ意図)。 */}
      <div class="mx-auto max-w-3xl px-4 pt-10 pb-0 flex flex-col items-center text-center gap-8">
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

        {/* A. GH Sponsors の大きな glass button は削除 (User: GH Sponsors が
            Footer 末尾の link 行と二重になるため)。寄付導線は最終行の
            text link [GitHub Sponsors] に集約する (machigai-salad と同パターン)。 */}

        {/* B. Amazon affiliate × 3 — #152 で AffiliateGrid に切り出し。
            データ層 (web/src/data/affiliateProducts.ts) と UI 層を分離し、
            他 PWA にコピペで横展開できる pattern にしている。 */}
        <AffiliateGrid />

        {/* C. QR — 別途指定する PNG (`/orber-qr.png`) を使う。補助コピーは置かない。
            PNG は事前に `magick -negate` 済みで、白モジュール + 透明背景。
            bg-bg (#040404) 上で白モジュールが見える形 (orber テーマカラーに合わせ済み)。
            border / bg-bg は削除済み: 透明背景ごと bg をそのまま透かすため不要。
            User: 「うっすらと白い四角い枠」がモジュール領域以外に見えるのを解消。 */}
        <img
          src="/orber-qr.png"
          alt={t('qrAlt')}
          width="120"
          height="120"
          class="block"
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

        {/* #174: D. Copyright 行 — テキストリンク (`More by kako-jun` /
            `GitHub Sponsors`) を SVG アイコン並びに置換。osaka-kenpo / sasso /
            agasteer で使われる home / GitHub / Sponsors アイコンと同形式。
            旧版は `GitHub Sponsors` の長さでスマホ幅に収まらず、`© kako-jun`
            がハイフン直後で改行されて `kako-` のみ前行に残る不格好な崩れが
            発生していた。アイコン化で行が短くなり、さらに `© kako-jun` 自体に
            whitespace-nowrap を当てて折り返しを禁じる。 */}
        <div class="flex flex-wrap items-center justify-center gap-4 text-xs text-fgSubtle">
          <a
            href="https://llll-ll.com"
            target="_blank"
            rel="noopener noreferrer"
            aria-label={t('authorSiteAriaLabel')}
            title={t('authorSiteAriaLabel')}
            class="hover:text-fg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg rounded"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M3 12l2-2m0 0l7-7 7 7M5 10v10a1 1 0 001 1h3m10-11l2 2m-2-2v10a1 1 0 01-1 1h-3m-6 0a1 1 0 001-1v-4a1 1 0 011-1h2a1 1 0 011 1v4a1 1 0 001 1m-6 0h6" />
            </svg>
          </a>
          <a
            href="https://github.com/kako-jun/orber"
            target="_blank"
            rel="noopener noreferrer"
            aria-label={t('repoLinkAriaLabel')}
            title={t('repoLinkAriaLabel')}
            class="hover:text-fg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg rounded"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="currentColor"
              aria-hidden="true"
            >
              <path d="M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.58.11.79-.25.79-.56 0-.27-.01-1.01-.02-1.98-3.2.69-3.87-1.54-3.87-1.54-.52-1.32-1.27-1.67-1.27-1.67-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.18 1.76 1.18 1.02 1.76 2.69 1.25 3.34.96.1-.74.4-1.25.72-1.54-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.29 1.18-3.09-.12-.29-.51-1.46.11-3.04 0 0 .97-.31 3.18 1.18a11 11 0 015.79 0c2.21-1.5 3.18-1.18 3.18-1.18.62 1.58.23 2.75.11 3.04.74.8 1.18 1.83 1.18 3.09 0 4.42-2.69 5.4-5.25 5.68.41.36.78 1.06.78 2.13 0 1.54-.01 2.78-.01 3.16 0 .31.21.68.79.56C20.21 21.39 23.5 17.08 23.5 12 23.5 5.65 18.35.5 12 .5z" />
            </svg>
          </a>
          <a
            href="https://github.com/sponsors/kako-jun"
            target="_blank"
            rel="noopener noreferrer"
            aria-label={t('sponsorAriaLabel')}
            title={t('sponsorAriaLabel')}
            class="hover:text-fg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg rounded"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="currentColor"
              aria-hidden="true"
            >
              <path d="M12 21s-7-4.5-9.5-9C.5 8 3 4 7 4c2 0 3.5 1 5 3 1.5-2 3-3 5-3 4 0 6.5 4 4.5 8C19 16.5 12 21 12 21z" />
            </svg>
          </a>
          <span aria-hidden="true">·</span>
          <span class="font-display font-light whitespace-nowrap">© kako-jun</span>
        </div>
      </div>
    </footer>
  );
}
