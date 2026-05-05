// orber#128 — Footer (Solid island)
// 公開後の継続接点 (GH Sponsors / Amazon affiliate / QR / Copyright /
// Nostalgic Counter) を 1 コンポーネントに集約する。#86 (About / Donate) の
// プライバシー note もここに置き、フッター = 「最後に読まれる場所」 として
// orber の境界条件 (画像はブラウザ内処理) を改めて宣言する。
//
// レイアウト方針 (DESIGN.md §3 + 本ファイル冒頭で扱う):
//   - モバイル: 縦積み (flex-col)
//   - デスクトップ (md:): 2 列 — 左 = Sponsor + Amazon、右 = QR + Copyright + Counter
//   - sticky ではなく自然なフロー (Studio の最後に着地)
//   - glass-bg + hairline で本体から分離 (border-t border-hairline)
//
// ハードコード禁止: カラーは tailwind.config.mjs の token (bg / fg / fgMuted /
// fgSubtle / hairline / glassBg / glassBgHover / glassBorder / focusRing) のみ。
// 生 #fff / rgba() を class に書き込まない。
//
// Web Components:
//   <nostalgic-counter id="..." type="total" format="text" />
// は Custom Element なので JSX intrinsic に存在しない。Solid 1.x は
// 任意のタグを許容するが TypeScript が落ちるため `as any` キャストで通す。
// 別案として `web-components.d.ts` を切る方法もあるが、orber でこれが唯一の
// Web Component なら 1 行のキャストの方が変更面が小さい。

import { onMount } from 'solid-js';
import { t } from '../lib/strings';
import {
  AFFILIATE_PRODUCTS,
  amazonUrl,
} from '../data/affiliateProducts';

// #128: Nostalgic Counter の実 ID は kako-jun が
// https://nostalgic.llll-ll.com/ のダッシュボードで取得後に置換する。
// ear-sky の ID 形式 ("ear-sky-eaae1797") を踏襲し、orber では
// "orber-XXXXXXXX" の placeholder にしておく。
// TODO(kako-jun): 実 ID に置換 (例 "orber-xxxxxxxx")。
const NOSTALGIC_COUNTER_ID = 'orber-PLACEHOLDER';

// placeholder の間は Counter ブロック自体を非表示にする (review nit-1)。
// embed.js が "Counter not found" 等のテキストを表示しないように完全 mount しない。
const NOSTALGIC_COUNTER_ENABLED = !NOSTALGIC_COUNTER_ID.endsWith('PLACEHOLDER');

// embed.js は CSP / order に敏感ではないので Footer の onMount で動的注入する
// (Base.astro を触らずに済み、フッターが visible になるまで XHR が走らない)。
// 同 URL の二重注入を避けるため data-orber-nostalgic フラグで idempotent に。
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

export default function Footer() {
  onMount(() => {
    if (NOSTALGIC_COUNTER_ENABLED) {
      ensureNostalgicEmbed();
    }
  });

  return (
    <footer
      class="mt-16 border-t border-hairline bg-glassBg backdrop-blur-glass"
      aria-label="orber footer"
    >
      <div class="mx-auto max-w-3xl px-4 py-10 grid gap-10 md:grid-cols-2">
        {/* 左列: Sponsor + Amazon */}
        <div class="space-y-6">
          {/* A. GH Sponsors */}
          <div>
            <a
              href="https://github.com/sponsors/kako-jun"
              target="_blank"
              rel="noopener noreferrer"
              title={t('sponsorTitle')}
              class="inline-flex items-center gap-2 rounded-md border border-glassBorder bg-glassBg hover:bg-glassBgHover px-3 py-2 text-sm text-fg transition-colors duration-200 ease-out focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
            >
              {/* GitHub heart icon — DESIGN.md §7 (inline SVG, stroke 1.5, currentColor) */}
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
          </div>

          {/* B. Amazon affiliate × 3 */}
          <section aria-label={t('affiliateHeading')}>
            <h2 class="text-xs text-fgMuted mb-3 tracking-wide">
              {t('affiliateHeading')}
            </h2>
            <ul class="grid grid-cols-3 gap-2">
              {AFFILIATE_PRODUCTS.map((p) => (
                <li>
                  <a
                    href={amazonUrl(p.asin)}
                    target="_blank"
                    rel="noopener noreferrer sponsored nofollow"
                    title={p.title}
                    class="block rounded-md border border-glassBorder bg-glassBg hover:bg-glassBgHover p-2 transition-colors duration-200 ease-out focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
                  >
                    <div class="aspect-square w-full mb-2 bg-glassBg rounded-sm overflow-hidden flex items-center justify-center">
                      <img
                        src={p.imageUrl}
                        alt={p.title}
                        loading="lazy"
                        decoding="async"
                        width="120"
                        height="120"
                        class="max-h-full max-w-full object-contain"
                        onError={(e) => {
                          // placeholder URL は 404 になるため、エラー時は
                          // 画像要素を非表示にして枠だけ残す (UX 上の
                          // broken icon を消す)。実 ASIN 投入後は普通に
                          // 表示される。
                          (e.currentTarget as HTMLImageElement).style.visibility = 'hidden';
                        }}
                      />
                    </div>
                    <div class="text-xs text-fg leading-tight truncate">
                      {p.title}
                    </div>
                    <div class="text-xs text-fgSubtle leading-tight truncate mt-0.5">
                      {p.caption}
                    </div>
                  </a>
                </li>
              ))}
            </ul>
            <p class="text-xs text-fgSubtle mt-2">
              {t('affiliateDisclosure')}
            </p>
          </section>
        </div>

        {/* 右列: QR + Privacy + Copyright + Counter */}
        <div class="space-y-6 md:text-right">
          {/* C. QR */}
          <div class="md:flex md:justify-end">
            <div class="inline-flex flex-col items-center gap-1">
              <img
                src="/orber-qr.svg"
                alt={t('qrAlt')}
                width="120"
                height="120"
                class="block rounded-sm border border-hairline bg-bg"
              />
              <span class="text-xs text-fgSubtle">{t('qrLabel')}</span>
            </div>
          </div>

          {/* #86 統合 — About + Privacy + Source link を 1 段にまとめる。
              orber が何 / どこのソースか / 何で作っているか + 画像はサーバーに
              送られない、を最後に読ませる。 */}
          <section
            aria-label={t('aboutHeading')}
            class="space-y-2 text-xs leading-relaxed"
          >
            <p class="text-fgMuted">{t('aboutBody')}</p>
            <p class="text-fgMuted">{t('privacyNote')}</p>
            <p class="text-fgSubtle">
              <a
                href="https://github.com/kako-jun/orber"
                target="_blank"
                rel="noopener noreferrer"
                class="underline decoration-hairline underline-offset-2 hover:text-fg focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
              >
                {t('repoLinkLabel')}
              </a>
              {' · '}
              <span>{t('aboutBuiltWith')}</span>
            </p>
          </section>

          {/* E. Nostalgic Counter — placeholder ID の間は表示しない (review nit-1) */}
          {NOSTALGIC_COUNTER_ENABLED && (
            <div class="text-xs text-fgSubtle">
              {/* ja: 「閲覧数: {n}」 / en: 「{n} views」 で語順を切替 (review nit-4)。
                  Solid の JSX intrinsic は env.d.ts で nostalgic-counter を拡張済み。 */}
              <span>{t('viewsLabelPrefix')}</span>
              <nostalgic-counter
                id={NOSTALGIC_COUNTER_ID}
                type="total"
                format="text"
              />
              <span>{t('viewsLabelSuffix')}</span>
            </div>
          )}

          {/* D. Copyright */}
          <p class="font-display font-light text-xs text-fgSubtle">
            © 2026 kako-jun
          </p>
        </div>
      </div>
    </footer>
  );
}
