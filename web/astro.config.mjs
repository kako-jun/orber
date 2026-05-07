import { defineConfig } from 'astro/config';
import solid from '@astrojs/solid-js';
import tailwind from '@astrojs/tailwind';

// #146: ビルド日時を JST (UTC+9) で YYYY-MM-DD に固定し、`__BUILD_DATE__`
// として Vite の define で source 内の識別子を build 時に literal 置換する。
// Footer の version 表示 (`v{date}`) と #148 の sw.js CACHE_NAME 置換で同値を使う。
const BUILD_DATE = new Date(Date.now() + 9 * 60 * 60 * 1000)
  .toISOString()
  .slice(0, 10);

// Static output: deploy via `wrangler pages deploy dist`.
// No SSR adapter needed; @astrojs/cloudflare is intentionally omitted.
export default defineConfig({
  integrations: [solid(), tailwind()],
  output: 'static',
  site: 'https://orber.llll-ll.com',
  vite: {
    define: {
      // dev サーバーでも置換されるので、開発中は dev サーバー起動日が表示される。
      __BUILD_DATE__: JSON.stringify(BUILD_DATE),
    },
    server: {
      fs: {
        // wasm-pack output lives at src/wasm/. Allowing parent paths keeps
        // dev-server access future-proof if the layout moves.
        allow: ['..'],
      },
    },
  },
});
