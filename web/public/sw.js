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

// orber#184 — ffmpeg-core (~31 MB) は CF Pages の単一ファイル上限 25 MiB を
// 超えるため、`@ffmpeg/core` を jsdelivr CDN から配信する。バージョンを
// pin することで immutable cache を効かせ、SW で CacheFirst して初回 fetch 後
// はオフラインでも透過動画エンコードができる状態を維持する。
// バージョン更新時は `encodeWebmAlphaWasm.ts` の `FFMPEG_CORE_VERSION` から
// `package.json` の `scripts.stamp:sw` で build 時に `__FFMPEG_CORE_VERSION__`
// を置換する (二重定義回避、レビュー N1)。古いキャッシュ名は activate で破棄。
// 注: `astro dev` は stamp:sw を実行しないため、開発中は sentinel 文字列
// `__FFMPEG_CORE_VERSION__` のまま登録され、cache 名も `ffmpeg-core-v__...__`
// となる。動作上は他キャッシュと衝突しないので問題ないが debug 時に注意。
const FFMPEG_CORE_VERSION = '__FFMPEG_CORE_VERSION__';
const FFMPEG_CORE_CACHE = `ffmpeg-core-v${FFMPEG_CORE_VERSION}`;
const FFMPEG_CORE_URL_PREFIX = 'https://cdn.jsdelivr.net/npm/@ffmpeg/core@';

const PRECACHE_URLS = ['/', '/manifest.webmanifest'];

function isFfmpegCoreRequest(url) {
  return url.startsWith(FFMPEG_CORE_URL_PREFIX);
}

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
            // 現行 orber-{date} キャッシュ、現行 ffmpeg-core-vX.Y.Z 以外は破棄。
            // バージョン更新時に古い ffmpeg-core-* 領域を確実に解放する。
            .filter(
              (name) => name !== CACHE_NAME && name !== FFMPEG_CORE_CACHE
            )
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

  // ffmpeg-core (jsdelivr CDN, cross-origin) は CacheFirst で永続化する。
  // バージョン pin により immutable とみなせる。レビュー M1: opaque response
  // (`mode: 'no-cors'` 時) は importScripts / WebAssembly streaming compile が
  // CORS で落ちる原因になるため cache に積まない (下記 ffmpegCoreCacheFirst 内)。
  if (isFfmpegCoreRequest(request.url)) {
    event.respondWith(ffmpegCoreCacheFirst(event, request));
    return;
  }

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

async function ffmpegCoreCacheFirst(event, request) {
  const cache = await caches.open(FFMPEG_CORE_CACHE);
  // レビュー S2: 過剰防御として ignoreVary を付ける。M1 で `mode: 'cors'` 統一
  // されたので Vary 不整合は理論上発生しないが、将来 jsdelivr が Vary ヘッダを
  // 変えても cache hit が外れて再 DL になる事故を防ぐ。
  const cached = await cache.match(request, { ignoreVary: true });
  if (cached) return cached;
  try {
    // ffmpeg.wasm 側は CORS 付き普通の fetch で取りに来る。jsdelivr は CORS
    // 許可済みなので通常 fetch で通る。
    const response = await fetch(request);
    // レビュー M1: opaque response はキャッシュに積まない。
    //   過去に `mode: 'no-cors'` で焼かれた opaque 既存キャッシュは将来
    //   `cache.match` でヒットしてもこのチェックを通ったクリーンな response
    //   で上書きされるため自然に置き換わる。
    // レビュー M2: cache.put は async / SW lifetime 外で完走させるべきなので
    //   event.waitUntil で SW に「処理継続」を宣言する。
    if (response && response.ok && response.type !== 'opaque') {
      event.waitUntil(cache.put(request, response.clone()));
    }
    return response;
  } catch {
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
