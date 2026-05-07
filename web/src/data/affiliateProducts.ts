// orber#152: Amazon affiliate products.
//
// 3 商品グリッドは Footer の Sponsor ボタンの下に置き、他 PWA でも同じ
// pattern を横展開する。商品はリポごとに違うので、本ファイル (データ層) を
// リポごとに用意し、`AffiliateGrid` (UI 層) を横コピーで再利用する想定。
//
// アソシエイト ID は kako-jun の `ultimate-battle-22`。osaka-kenpo と同じく
// **amzn.to 短縮リンク** を使う方針 (Associates ダッシュボードで生成)。
// tag を URL に露出せず、Amazon 側の redirect で計測される。
// `amazonUrl(asin)` ヘルパは amzn.to 化に伴い廃止。
//
// 画像 URL は Amazon CDN の `m.media-amazon.com/images/I/{IMAGEID}.jpg` を
// そのまま使う。`_SL250_` 等のリサイズ指定は付けず、`<img width height>` で
// 表示サイズだけ縛り、CDN は元解像度を返す (Retina 対応)。

export interface AffiliateProduct {
  /** 商品ページへの amzn.to 短縮 URL (Associates ダッシュボードで生成)。 */
  url: string;
  /** Amazon 商品の正式タイトル (短縮可)。 */
  title: string;
  /** Amazon CDN の商品メイン画像 URL。 */
  imageUrl: string;
  /** kako-jun が商品ごとに書く一言コメント。orber 視点での選定理由など。 */
  caption: string;
}

export const AFFILIATE_PRODUCTS: AffiliateProduct[] = [
  {
    url: 'https://amzn.to/4nbQcSF',
    title: 'ドラゴンクエストIII そして伝説へ…',
    imageUrl: 'https://m.media-amazon.com/images/I/61mJHMLthgL.jpg',
    caption: 'シルバーオーブだけ難しすぎる！',
  },
  {
    url: 'https://amzn.to/4tpzi4L',
    title: 'HG オオワシアカツキガンダム 1/144',
    imageUrl: 'https://m.media-amazon.com/images/I/51tZBx3ATSL.jpg',
    caption: 'オーブの目立つ秘密兵器！',
  },
  {
    url: 'https://amzn.to/4nfDjHr',
    title: '行って眺めて撮る 巨大工場探訪ガイド',
    imageUrl: 'https://m.media-amazon.com/images/I/51i4p4hJLYL.jpg',
    caption: '写真を撮ってorberで加工しよう！',
  },
];
