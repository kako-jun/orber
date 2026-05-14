// orber#148 — Service Worker
//
// 設計方針 (machigai-salad/public/sw.js を起点に、review S1/S2/S5 を反映):
//
//   - CACHE_NAME に build 日付を含めて、新版デプロイで自動 invalidate する
//     (`__BUILD_DATE__` は build 時に Node の 1 行スクリプトで dist/sw.js に置換)
//   - precache は最小 (`/` と `/manifest.webmanifest` のみ)
//   - `/_astro/*` (Astro/Vite が content-hash で吐く immutable asset) は
//     **CacheFirst** — ファイル名が変われば中身が違うので、一度乗ったら
//     ネット往復ゼロで返せる。orber の wasm (~700KB-1MB) を毎回 fetch しない
//     ための重要施策 (review S1)
//   - その他は **network-first** で、レスポンスが ok なら同名キャッシュに追記
//   - **navigation fallback**: `request.mode === 'navigate'` のときに network /
//     キャッシュ両方失敗したら precache した `/` を返す (PWA shell 戦略、review S2)
//   - blob: / data: URL は intercept しない (生成結果の DL を阻害しない)
//   - GET 以外は intercept しない
//   - cache.put は `event.waitUntil(...)` で SW lifetime に縛り、レスポンス返却
//     後にバックグラウンド継続を保証する (review S5)
//
// 既知の制約: クリーン状態 + 即オフラインで再訪した場合、`/_astro/*` (wasm/JS)
// が cache に乗っていないため真っ黒画面になる。最初の online 訪問が完走すれば
// 以後はオフラインで安定する。

const CACHE_NAME = 'orber-__BUILD_DATE__';

// orber#184/#192: 透過動画は JS-only MOV muxer になり ffmpeg.wasm / jsdelivr
// CDN への依存が消えたため、旧 `ffmpeg-core-v<ver>` CacheFirst 経路と
// `__FFMPEG_CORE_VERSION__` ステンシルは撤去済。古い ffmpeg-core キャッシュは
// activate ハンドラで一括破棄される (CACHE_NAME 不一致で全 stale が掃除される)。

const PRECACHE_URLS = ['/', '/manifest.webmanifest'];

// Astro/Vite が出す content-hash 付き asset の prefix。`/_astro/foo.HASH.js`
// 等は immutable なので CacheFirst で返してよい。
function isImmutableAsset(url) {
  return url.pathname.startsWith('/_astro/');
}

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
            // 現行 orber-{date} 以外は全て破棄。#192 以前の `ffmpeg-core-v*`
            // キャッシュ (jsdelivr 経由の wasm core ~30MB) もここで自動掃除される。
            .filter((name) => name !== CACHE_NAME)
            .map((name) => caches.delete(name))
        )
      )
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') return;

  let url;
  try {
    url = new URL(request.url);
  } catch (_) {
    return;
  }

  // blob: / data: は intercept しない (生成結果の DL を阻害しない)
  if (url.protocol === 'blob:' || url.protocol === 'data:') return;

  if (isImmutableAsset(url)) {
    // CacheFirst: hit したらそれを返し、miss だったら network → cache に積む。
    event.respondWith(cacheFirst(request));
    return;
  }

  event.respondWith(networkFirst(request, event));
});

async function cacheFirst(request) {
  const cached = await caches.match(request);
  if (cached) return cached;
  try {
    const response = await fetch(request);
    if (response && response.ok) {
      const cache = await caches.open(CACHE_NAME);
      // 同期的な put は respondWith の Promise 解決とは独立なので waitUntil は
      // 不要 (await でこの async 関数の lifetime に既に縛られている)。
      cache.put(request, response.clone());
    }
    return response;
  } catch (err) {
    return new Response('Offline', { status: 503 });
  }
}

async function networkFirst(request, event) {
  try {
    const response = await fetch(request);
    if (response && response.ok) {
      const clone = response.clone();
      // SW lifetime に縛って、respondWith 解決後も cache.put の完了を保証する。
      event.waitUntil(
        caches.open(CACHE_NAME).then((cache) => cache.put(request, clone))
      );
    }
    return response;
  } catch (err) {
    const cached = await caches.match(request);
    if (cached) return cached;
    // navigation 要求がオフラインでキャッシュにも無い場合は、precache した
    // `/` を返してアプリシェルで起動できるようにする (PWA shell 戦略)。
    if (request.mode === 'navigate') {
      const shell = await caches.match('/');
      if (shell) return shell;
    }
    return new Response('Offline', { status: 503 });
  }
}
