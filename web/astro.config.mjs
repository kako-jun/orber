import { defineConfig } from 'astro/config';
import solid from '@astrojs/solid-js';
import tailwind from '@astrojs/tailwind';

// Static output: deploy via `wrangler pages deploy dist`.
// No SSR adapter needed; @astrojs/cloudflare is intentionally omitted.
export default defineConfig({
  integrations: [solid(), tailwind()],
  output: 'static',
  site: 'https://orber.llll-ll.com',
  vite: {
    server: {
      fs: {
        // wasm-pack output lives at src/wasm/. Allowing parent paths keeps
        // dev-server access future-proof if the layout moves.
        allow: ['..'],
      },
    },
  },
});
