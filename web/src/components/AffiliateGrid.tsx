// orber#152: Amazon affiliate × 3 grid (Solid island).
//
// 共通コンポーネントとして他 PWA でもコピー使用する想定で書く:
//   - データ (AFFILIATE_PRODUCTS) は `web/src/data/affiliateProducts.ts` に分離
//   - 本ファイル (UI 層) は他リポにそのまま貼り付けても動くよう、
//     i18n key 名 (`affiliateHeading` / `affiliateDisclosure`) と
//     tailwind token (fg / fgMuted / fgSubtle / hairline / glassBorder /
//     glassBgHover / focusRing) のみ参照する
//   - 商品画像は Amazon CDN の生 URL を使い、円形 mask で orb 化する
//     (#152 review 要件: 四角サムネイルのままにしない、orb/glow に寄せる)
//
// orb/glow カードの作り:
//   - 商品画像は `aspect-square` + `rounded-full` で円形に切り抜く
//   - inset shadow (内側に向かう暗み) で球面の落ち込みを暗示
//   - outer glow (柔らかい白い halo) で浮遊感を出す
//   - hover で halo 強化 + scale 微増、orber 本体の orb と同じ「触ると光る」感
//   - title / caption は下に小さく、orb 本体の主役感を保つ

import { t } from '../lib/strings';
import { AFFILIATE_PRODUCTS } from '../data/affiliateProducts';

export default function AffiliateGrid() {
  return (
    <section aria-label={t('affiliateHeading')} class="w-full">
      <h2 class="text-xs text-fgMuted mb-4 tracking-wide text-center">
        {t('affiliateHeading')}
      </h2>
      {/* #174: max-w-xl (36rem) を外して Footer の max-w-3xl いっぱい
          (= ドロップエリアと同じ幅) まで広げる。スマホ窮屈問題の解消。
          gap はスマホ gap-3, sm 以上 gap-4 で caption の折り返しを抑える。 */}
      <ul class="grid grid-cols-3 gap-3 sm:gap-4 w-full">
        {AFFILIATE_PRODUCTS.map((p) => (
          <li>
            <a
              href={p.url}
              target="_blank"
              rel="noopener noreferrer sponsored nofollow"
              title={p.title}
              class="group block focus-visible:outline-none"
            >
              {/* Orb body — 円形 mask + inset shadow + outer glow */}
              <div
                class="
                  relative aspect-square w-full mx-auto rounded-full
                  overflow-hidden bg-bg
                  transition-all duration-200 ease-out
                  group-hover:scale-105
                  group-focus-visible:ring-1 group-focus-visible:ring-focusRing
                  group-focus-visible:ring-offset-2 group-focus-visible:ring-offset-bg
                "
                style={{
                  // Outer halo (subtle white glow) + inset spherical shadow.
                  // hover で halo を強くする (group-hover で再上書き)。
                  'box-shadow':
                    'inset 0 0 14px rgba(0,0,0,0.55), 0 0 12px rgba(255,255,255,0.06)',
                }}
              >
                <img
                  src={p.imageUrl}
                  alt={p.title}
                  loading="lazy"
                  decoding="async"
                  width="120"
                  height="120"
                  class="absolute inset-0 w-full h-full object-cover scale-110 transition-transform duration-200 ease-out group-hover:scale-[1.18]"
                  onError={(e) => {
                    // 画像が落ちても枠 (円) と halo は残るよう、img だけ非表示にする。
                    (e.currentTarget as HTMLImageElement).style.visibility = 'hidden';
                  }}
                />
                {/* hover halo overlay — group-hover で発光感を強化 */}
                <div
                  aria-hidden="true"
                  class="
                    absolute inset-0 rounded-full opacity-0 group-hover:opacity-100
                    transition-opacity duration-200 ease-out pointer-events-none
                  "
                  style={{
                    'box-shadow':
                      '0 0 22px rgba(255,255,255,0.18), inset 0 0 8px rgba(255,255,255,0.08)',
                  }}
                />
              </div>
              {/* Caption (kako-jun 直筆の一言) と title を下に小さく */}
              <div class="mt-2 text-center">
                {p.caption && (
                  <div class="text-xs text-fgMuted leading-tight">
                    {p.caption}
                  </div>
                )}
                <div class="text-xs text-fgSubtle leading-tight mt-0.5">
                  {p.title}
                </div>
              </div>
            </a>
          </li>
        ))}
      </ul>
    </section>
  );
}
