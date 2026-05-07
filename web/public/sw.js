// orber#148 — Service Worker
//
// 設計方針 (machigai-salad/public/sw.js と同パターン):
//
//   - CACHE_NAME に build 日付を含めて、新版デプロイで自動 invalidate する
//     (`__BUILD_DATE__` は build 時に Node の 1 行スクリプトで dist/sw.js に置換)
//   - precache は最小 (`/` と `/manifest.webmanifest` のみ)
//     再訪時に start_url を即返してインストール体験を破壊しない
//   - fetch は **network-first** で、レスポンスが ok なら同名キャッシュに追記
//     オフライン時はキャッシュにフォールバック、無ければ 503 を返す
//   - blob: / data: URL は intercept しない (生成結果の DL を阻害しない)
//   - GET 以外は intercept しない
//
// orber は wasm / worker / 生成物が大きいため、最初のロード後は network 優先で
// 鮮度を取りつつ、失敗時のフォールバックでオフラインでも起動できる状態にする。
// 生成結果 (PNG / WebM / ZIP) はユーザーがダウンロードして消費する一過性データ
// なので、SW の cache に積極的に乗せる必要はない (fetch 経路に乗ったら結果的に
// cache されるが、blob: / data: は intercept しないので DL は素通しになる)。

const CACHE_NAME = 'orber-__BUILD_DATE__';

const PRECACHE_URLS = ['/', '/manifest.webmanifest'];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => cache.addAll(PRECACHE_URLS))
  );
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((names) =>
        Promise.all(
          names
            .filter((name) => name !== CACHE_NAME)
            .map((name) => caches.delete(name))
        )
      )
  );
  self.clients.claim();
});

// Network-first: always try network first, fall back to cache when offline.
self.addEventListener('fetch', (event) => {
  if (event.request.method !== 'GET') return;
  const url = event.request.url;
  // Don't intercept blob: or data: URLs (used for downloads of generated PNG /
  // WebM / ZIP). These never hit the network anyway.
  if (url.startsWith('blob:') || url.startsWith('data:')) return;

  event.respondWith(
    fetch(event.request)
      .then((response) => {
        if (response.ok) {
          const clone = response.clone();
          caches.open(CACHE_NAME).then((cache) => cache.put(event.request, clone));
        }
        return response;
      })
      .catch(() =>
        caches
          .match(event.request)
          .then((cached) => cached || new Response('Offline', { status: 503 }))
      )
  );
});
