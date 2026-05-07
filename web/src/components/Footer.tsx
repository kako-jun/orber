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
      class="mt-16 border-t border-hairline"
      aria-label={t('footerAriaLabel')}
    >
      {/* レビュー: 下のスペースは main の p-8 (32px) のみに頼り、footer 自体の
          bottom padding は削る (pb-0)。これで © kako-jun の下のスペースが
          ヘッダ「orber」の上のスペースと揃う。 */}
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

        {/* D. Copyright + 関連リンク (machigai-salad パターン)。
            [llll-ll.com] · [GitHub Sponsors テキスト] · © kako-jun を 1 行で。
            上の大きな Sponsor button は CTA として残しつつ、ここでも小さい
            テキストリンクを再掲して machigai-salad の終端 UI と揃える。 */}
        <div class="flex flex-wrap items-center justify-center gap-3 text-xs text-fgSubtle">
          <a
            href="https://llll-ll.com"
            target="_blank"
            rel="noopener noreferrer"
            class="underline decoration-hairline underline-offset-2 hover:text-fg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
          >
            {t('authorSiteLabel')}
          </a>
          <span aria-hidden="true">·</span>
          <a
            href="https://github.com/sponsors/kako-jun"
            target="_blank"
            rel="noopener noreferrer"
            class="underline decoration-hairline underline-offset-2 hover:text-fg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
          >
            {t('sponsorTextLabel')}
          </a>
          <span aria-hidden="true">·</span>
          <span class="font-display font-light">© kako-jun</span>
        </div>
      </div>
    </footer>
  );
}
