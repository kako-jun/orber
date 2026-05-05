// orber#128: Amazon affiliate products.
// アソシエイト ID は kako-jun の `ultimate-battle-22`。`amazonUrl(asin)` が
// `tag=ultimate-battle-22` を必ず付ける唯一の出口。Footer から呼び出す。
//
// `asin` / `imageUrl` は kako-jun が実 ASIN に置き換えるまでの placeholder。
// テーマは「写真・配信・ライティング」(orber は配信背景・動画背景の生成ツールなので
// 周辺機材＋関連書籍を 3 枠並べる)。
// TODO(kako-jun): 各 ASIN を実商品に差し替える (PLACEHOLDER_* と _SL250_PLACEHOLDER を)。

export interface AffiliateProduct {
  /** Amazon ASIN。'PLACEHOLDER_*' は仮値。 */
  asin: string;
  /** 表示名。短く。ja/en どちらでもよいが orber は片寄せ運用。 */
  title: string;
  /** 商品サムネ。Amazon CDN の `_SL250_` 系 dynamic image を推奨。 */
  imageUrl: string;
  /** 一言コメント。strings.ts には入れない短文 (ja/en の片方でよい)。 */
  caption: string;
}

export const AFFILIATE_PRODUCTS: AffiliateProduct[] = [
  {
    asin: 'PLACEHOLDER_WEBCAM', // TODO(kako-jun): 実 ASIN に置換
    title: 'Logicool StreamCam',
    imageUrl: 'https://m.media-amazon.com/images/I/_SL250_PLACEHOLDER.jpg',
    caption: '配信用 Webカメラ',
  },
  {
    asin: 'PLACEHOLDER_RINGLIGHT', // TODO(kako-jun): 実 ASIN に置換
    title: 'リングライト 18inch',
    imageUrl: 'https://m.media-amazon.com/images/I/_SL250_PLACEHOLDER.jpg',
    caption: '撮影・配信向き',
  },
  {
    asin: 'PLACEHOLDER_PHOTOBOOK', // TODO(kako-jun): 実 ASIN に置換
    title: '光と影の写真集',
    imageUrl: 'https://m.media-amazon.com/images/I/_SL250_PLACEHOLDER.jpg',
    caption: 'ボケ・光・抽象 系',
  },
];

export const AFFILIATE_TAG = 'ultimate-battle-22';

/** Amazon 商品ページ URL。`tag=ultimate-battle-22` を必ず付ける。 */
export const amazonUrl = (asin: string): string =>
  `https://www.amazon.co.jp/dp/${asin}/?tag=${AFFILIATE_TAG}`;
