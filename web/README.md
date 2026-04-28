# orber web

Astro 4 + Solid.js + Tailwind + WASM frontend for orber.

## UI flow

画像をドロップ → `orber-wasm.generate_batch` で N 枚プレビューを生成 → ❤ で
気に入ったタイルを選択 → DL（1 枚は PNG 直接、複数は ZIP）。

UI 上の操作は「画像ドロップ」「アスペクト切替（縦長 540×960 / 横長 960×540）」
「タイルの ❤ トグル」「DL ボタン」のみ。パラメータスライダーは置かない。
バリエーションは `orber_core::variations::random_batch_specs(seed, total, still_count)`
で **ドロップごとに毎回ランダム**に振られる（前半は静止画 PNG、後半は MP4 枠と
いう枠だけ固定し、direction / speed / count / orb_size / blur / seed /
duration_ms はすべて呼び出しごとに `random_ranges` から一様サンプル）。タイル
枚数は縦長 10 枚（5×2）、横長 9 枚（3×3）でグリッドが綺麗に揃うように切り
替える。CLI の `--variations` は `DEFAULT_VARIATIONS` 決定論プリセットのままな
ので、再現性が必要な用途は CLI 側で扱う。

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

`build:cf` script は `my-font-craft` のパターンを踏襲し、以下を順に npm
script chain で実行する:

1. **wasm:ensure-rust**: `rustup` が PATH に無ければ `curl https://sh.rustup.rs | sh`
   で stable toolchain を導入し、`wasm32-unknown-unknown` ターゲットを追加
2. **wasm:install**: `wasm-pack` が無ければ `cargo install wasm-pack --locked`
3. **wasm:build**: `crates/wasm` を wasm-pack でビルドし `web/src/wasm/` に出力
4. **astro build**: `dist/` に静的アセットを生成

`. $HOME/.cargo/env` を都度 source し直しているのは、npm script chain の
`&&` が新しいシェルプロセスを起こす都合で `cargo` が PATH に居なくなるため。

Rust toolchain は workspace ルートの `rust-toolchain.toml` で `stable` +
`wasm32-unknown-unknown` ターゲットに固定されている。

### 手動デプロイ（緊急時）

```sh
npm run build
npx wrangler pages deploy dist
```
