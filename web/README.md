# orber web

Astro 4 + Solid.js + Tailwind + WASM frontend for orber.

## UI flow

画像をドロップ → worker に `source_rgb` を 1 回送る →
wasm の WebGPU(WGSL) 経路（`gpu_init_offscreen(OffscreenCanvas)` →
`gpu_set_render_data` → `gpu_render(t)`、CLI と同一の core WGSL シェーダ）で
12 枚の静止プレビュー (PNG) を順次生成 →
後半 4 タイル（#59, GUI_VIDEO_COUNT_DEFAULT）を同じ経路 + WebCodecs
`VideoEncoder` で H.264 mp4 化 →
**4 枚揃った時点で一斉に `<video>.play()` を発火**（#61）→ コーナーマーカー
トグルで気に入ったタイルを選択 → DL（1 枚は拡張子に応じた直接 DL、複数は
ZIP に PNG / MP4 が混在）。

UI 上の操作は「画像ドロップ」「アスペクト切替（縦長 / 横長アイコン）」
「ガチャ（同じ画像で再ロール）」「タイルの ✓ トグル」「DL ボタン」のみ。
パラメータスライダーは置かない。バリエーションは
`orber_core::variations::random_batch_specs(seed, total, still_count)` で
**ドロップごとに毎回ランダム**に振られる（前半 8 枚は静止画 PNG、後半 4 枚は
MP4 枠という枠だけ固定し、direction / speed / count / orb_size / blur / seed /
duration_ms はすべて呼び出しごとに `random_ranges` から一様サンプル）。タイル
枚数は縦長・横長を問わず **12 枚で統一** (#61)。12 は 1/2/3/4/6/12 で割り
切れるためスマホ幅でも余りなくグリッドが組める。CLI の `--variations` は
`DEFAULT_VARIATIONS` 決定論プリセットのままなので、再現性が必要な用途は
CLI 側で扱う。

## 動画化

後半 4 タイルは `<video muted playsinline loop>` でグリッド内で勝手に動く
（autoplay は #61 で外し、4 枚揃ってから `play()` を一斉発火する方針に切替）。
direction は wasm 側 `direction_for_spec_idx` / `GUI_VIDEO_DIRECTIONS` が
**LR / RL / TB / BT を 1 枚ずつ重複なく固定割当**
する（#59）。フロー:

1. `wasm.gpu_set_render_data(params, n, spec_idx)` で 1 spec 分の描画データを wasm に渡す
2. worker 内の WGSL 経路が `gpu_render(t)` を 96 回 OffscreenCanvas に描き、
   各 frame を `VideoFrame` にして WebCodecs に流す
3. `mp4-muxer` の `ArrayBufferTarget` に詰めて `finalize()` → mp4 Blob
4. Solid signal 経由で該当タイルの `videoBlobUrl` を埋めて `<video>` を mount
   (この時点では autoplay 無し、静止状態で待機)
5. 4 枚すべての mp4 化が終わったら `await yieldFrame()` で DOM mount を待ち、
   `videoRefs` に集めた `<video>` 要素 4 つに対して `play()` を一斉発火 (#61)

`orber` の motion model は最初から `t=0 ≡ t=1` のピクセル一致ループに設計
されているので、`<video loop>` で継ぎ目なくエンドレス再生される
（`AnimationCursor` は `t = i / total_frames` (i=0..total_frames) を返し、
`t=1` を出さないことでループ閉鎖を担保）。

WebCodecs 非対応ブラウザでは静止画 PNG のまま表示される（フォールバック）。

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
