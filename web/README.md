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

**Run `wasm:build` first** — `src/wasm/` is gitignored, so a fresh clone has no
wasm bindings until `wasm-pack` produces them. The `dev` and `build` scripts
both auto-trigger `wasm:build`.

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

## Deploy (Cloudflare Pages, Git integration)

GitHub にプッシュすれば Cloudflare Pages が自動でビルド & デプロイする構成。

### Cloudflare ダッシュボードでやる初回セットアップ

1. **Workers & Pages** → **Create** → **Pages** → **Connect to Git** で
   `kako-jun/orber` リポジトリを接続
2. ビルド設定:
   - **Production branch**: `main`
   - **Framework preset**: なし（カスタム）
   - **Build command**: `npm run build:cf`
   - **Build output directory**: `web/dist`
   - **Root directory**: `web`
3. 環境変数（必要に応じて）:
   - 特になし。`build:cf` が wasm-pack を自動インストールする
4. **カスタムドメイン**: Pages プロジェクト → **Custom domains** → `orber.llll-ll.com`
   を追加。`llll-ll.com` ゾーン側に CNAME が自動で入る

### ビルドフロー

`build:cf` は `web/scripts/cf-build.sh` を呼び、以下を順に実行する:

1. **rustup**: CF のビルド環境に `cargo` が無いので `curl https://sh.rustup.rs | sh` で導入
2. **wasm32 ターゲット追加**: `rustup target add wasm32-unknown-unknown`
3. **wasm-pack**: 未インストールなら `cargo install wasm-pack --locked` で導入
4. **wasm:build**: `crates/wasm` を wasm-pack でビルドし `web/src/wasm/` に出力
5. **astro build**: `dist/` に静的アセットを生成

Rust toolchain は workspace ルートの `rust-toolchain.toml` で `stable` +
`wasm32-unknown-unknown` ターゲットに固定されている（rustup が読む）。

### 手動デプロイ（緊急時）

```sh
npm run build
npx wrangler pages deploy dist
```
