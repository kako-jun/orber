# orber web

Astro 4 + Solid.js + Tailwind + WASM frontend scaffold for orber.

## Stack

- Astro 4 (`output: 'static'`)
- Solid.js (island via `client:load`)
- Tailwind CSS 3
- `orber-wasm` (built from `../crates/wasm` via wasm-pack)

No SSR adapter is wired — Cloudflare Pages serves the static `dist/` directly.

## Prereqs

- Node.js 20+
- [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) on PATH (`cargo install wasm-pack`)

## Develop

```sh
npm install
npm run wasm:build   # builds crates/wasm into src/wasm/
npm run dev          # also runs wasm:build first
```

## Build

```sh
npm run build
ls dist/
```

## Deploy (Cloudflare Pages)

```sh
npm run deploy:dry   # validate without uploading
npm run deploy       # actual upload (requires wrangler auth)
```
