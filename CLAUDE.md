# orber - Abstract Orb Mood Renderer

写真や動画から抽象的な光の玉（orb）のムード画像/動画を生成する Rust CLI。

## ビルド・テスト

Rust 側 (CLI / core / wasm):
```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

CLI は GPU がハード依存（#225 で GPU(WGSL) が唯一のレンダラ）。`cargo build -p orber`
でそのまま GPU 経路がビルドされ、`--features gpu` は不要。`orber-core` 単体で GPU 経路を
clippy する場合のみ `cargo clippy -p orber-core --features gpu --all-targets` のように feature を渡す。
`cargo test` は GPU 構造テスト（lit-pixel 有無・決定論・cache 再利用・ループ閉じ・空 cluster は背景のみ）も走る
（GPU アダプタが無い環境では該当テストは skip）。

Web GUI 側 (Astro + Solid):
```bash
cd web
npm run build       # wasm rebuild + astro build + sw.js stamp
npm test            # vitest (strings.ts の lang signal 経路など)
npm run test:watch  # 開発時の watch モード
```

`web/` の vitest は #163 で導入。現状は `web/src/lib/strings.test.ts` のみ
(detectLang() / lang signal / t() 補間の回帰テスト 8 件)。新機能追加時は
同階層に `*.test.ts` を増やしていく方針 (jsdom 環境、`vitest.config.ts` 参照)。

## ドキュメント

| ファイル | 内容 | 言語 |
|---|---|---|
| `README.md` | エンドユーザー向けの使い方 | 英語（マスター） |
| `docs/overview.md` | 設計思想・処理パイプライン | 英語 |
| `docs/roadmap.md` | 完了済み・残タスク（内部運用メモ） | 日本語 |
| `CLAUDE.md` | AI 向け内部ドキュメント | 日本語 |

### 言語ルール

- README は英語マスターのみ
- docs/overview.md は英語
- docs/roadmap.md と CLAUDE.md は日本語（内部用）

## ソース構成

`v0.3.0` から Cargo workspace 構成。GUI / WASM フロントエンドが純粋描画コアだけを依存できるよう、I/O と子プロセスを CLI 側に隔離している（Issue #35）。

```
orber/
├── Cargo.toml              # [workspace] members = ["crates/core", "crates/cli", "crates/wasm"]
├── .cargo/config.toml      # wasm32-unknown-unknown 用の getrandom_backend cfg
└── crates/
    ├── core/               # orber-core: 純粋描画コア（wasm ビルド可能・I/O 一切なし）
    │   ├── Cargo.toml      #   crate-type = ["cdylib", "rlib"]
    │   ├── assets/fonts/   #   NotoSansSymbols2-Regular.ttf（glyph 用 subset）+ OFL.txt
    │   └── src/
    │       ├── lib.rs
    │       ├── output_mode.rs  # 出力拡張子 → OutputMode 判定
    │       ├── cluster.rs      # 入力画像 → 代表色クラスタ抽出
    │       ├── orb.rs          # 形状/スタイルの型 + 彩度調整（描画は GPU 側、#225 で CPU 描画は撲滅）
    │       ├── animate.rs      # フレーム parameters（AnimateOptions / pack_render_data_for_webgl 等）
    │       ├── glyph.rs        # フォント/画像 → SDF（ttf-parser + zeno、#223）
    │       ├── gpu.rs          # GPU(WGSL, wgpu) レンダラ — 唯一のレンダラ（#207〜#225）
    │       ├── orb_circle.wgsl / orb_glyph.wgsl / orb_glyph_bleed.wgsl / orb_aquarelle.wgsl
    │       ├── style.rs        # CSS / SVG 静的書き出し
    │       └── variations.rs   # バリエーション spec 定義
    │                           # にじみ処理は外部 crate `aquarelle = "0.2"` に分離済み（旧 src/aquarelle/ は撤去）
    ├── cli/                # orber: CLI バイナリ（image::open / ffmpeg / tempfile）
    │   ├── Cargo.toml      #   [[bin]] name = "orber", path = "src/main.rs"
    │   └── src/
    │       ├── main.rs         # CLI パース（clap）。`Cli` / `Motion` / `Shape` 定義
    │       └── video.rs        # 連番フレーム → MP4/WebM（ffmpeg 子プロセス）
    └── wasm/               # orber-wasm: ブラウザ向け wasm-bindgen ラッパー（#36）
        ├── Cargo.toml      #   crate-type = ["cdylib", "rlib"]。wasm32 専用 target dependency で
        │                   #   gpu 経路（orber-core gpu feature + wgpu + web-sys）を常時有効化（#230）
        └── src/
            ├── lib.rs          # データ供給（#225 で CPU 描画 generate_* は撲滅）。
            │                   #   get_render_data（per-orb パラメータの pack）/
            │                   #   get_glyph_sdf（フォント文字 → SDF）。実描画は WebGL2 fragment shader 側
            └── gpu.rs          # WebGPU canvas present 経路（#230、wasm32 専用 cfg）。
                                #   gpu_init / gpu_set_render_data / gpu_render / gpu_resize。
                                #   core の GpuRenderer(WGSL) が canvas surface に直接描く。Circle のみ（#231 で拡張）

web/                        # Web フロントエンド (#37, #38)
├── astro.config.mjs        #   Astro 4 / output: 'static' / Solid + Tailwind
├── package.json            #   npm scripts: wasm:build / dev / build / deploy（jszip 依存）
├── wrangler.toml           #   Cloudflare Pages 設定（pages_build_output_dir = "dist"）
└── src/
    ├── pages/index.astro       # トップページ（ロゴ + Subtitle + Studio）
    ├── pages/gpu-lab.astro     # WebGPU(WGSL) 検証ページ（#230、開発用・本番導線からリンクしない）
    ├── layouts/Base.astro      # 共通レイアウト（Space Grotesk + lang 自動切替, #62 /
    │                           # skeleton & skeleton-soft shimmer #71 #80）
    ├── components/Studio.tsx   # Solid アイランド。バッチ生成 GUI
    │                           # (#38, #62 glass, #61 12 枚統一 + 動画一斉再生,
    │                           #  #71 skeleton 先出し, #73 hi-res DL,
    │                           #  #75 worker 経由化, #80 video pending overlay)
    ├── components/Subtitle.tsx # Solid アイランド。用途提案サブタイトル（i18n, #62）
    ├── lib/decodeImage.ts      # File → RGB バイト列デコード（#38）
    ├── lib/encodeMp4.ts        # WebCodecs + mp4-muxer で MP4 化（#52）。
    │                           # encodeAnimationToMp4 本体は worker 側で呼ばれる (#75)。
    │                           # ANIM_TOTAL_FRAMES / isWebCodecsSupported は main 側からも import される。
    ├── lib/orberWorker.ts      # #75 wasm 描画 + WebCodecs を実行する Worker 本体
    ├── lib/orberClient.ts      # #75 main 側 Worker クライアント（postMessage を Promise 化）
    ├── lib/strings.ts          # i18n 文言集約 + ja/en 自動切替（#62）
    └── wasm/                   # wasm-pack 出力先（gitignore、.gitkeep のみ追跡）
```

`std::fs` / `std::process::Command` / `tempfile` を使うのは `crates/cli/` だけ。`crates/core/` は wasm32-unknown-unknown でもビルド通る（getrandom 0.3 の wasm_js バックエンドを `.cargo/config.toml` で有効化済み）。

## 主要な設計判断

- **prototype 段階はローカル Rust バイナリ単体で完結する** — Web フロント・WASM・crate.io 公開は将来 Issue
- **入力 → 静的 PNG が出るところまで先に通す** — 動画化はその後
- **にじみ処理は外部 crate `aquarelle` に切り出し済み** — `Cargo.toml` で `aquarelle = "0.2"` を依存。`OrbShape::Aquarelle` は per-orb のにじみ描画に、`OrbShape::Glyph` は全 orb 描画後の bleed pass 1 回に、それぞれ利用する。`OrbShape::Circle` はにじみ処理を呼ばない
- **動画書き出しは ffmpeg 子プロセス呼び出し** — 自前エンコードはやらない
- **動画入力対応も ffmpeg でフレーム抽出** — 抽出後は静止画パイプラインに合流させる
- **`--seed` で再現可能** — 同じ入力 + 同じ seed で同じ出力
- **`Motion` / `Shape` enum は当面 `main.rs` に置く** — `animate.rs`（#4）で必要になった時点で `pub mod` に昇格させる。今は CLI パース直後にしか使わないので main.rs ローカルで十分
- **`duration_ms` は `u64` を採用** — `u32` でも 49 日分入って実用上は問題ないが、後段でのフレーム数計算（`duration_ms * fps / 1000` 等）でのオーバーフローを避けるため広めに取っておく
- **描画バックエンドは GPU(WGSL, wgpu) が唯一（#225 で tiny-skia 撲滅）** — ネイティブ CLI は `crates/core/src/gpu.rs` の `GpuRenderer` が全 shape（Circle / Glyph / Image / Aquarelle）を WGSL で描く。CPU(tiny-skia) ピクセル描画・CPU↔GPU parity オラクル・`--renderer cpu`・CPU フォールバックは削除済み。GPU アダプタが取れなければ `GpuRenderer::new` が `None` を返し、CLI は error 終了する（フォールバック無し）。tiny-skia は外部 crate `aquarelle` 経由の推移依存としてのみ残る（orber 自身のコード/マニフェストは tiny-skia フリー）。Skia lowp 互換の合成は WGSL 内で u8 量子化 → premultiply → source_over を再現する
- **GpuRenderer は wasm32 + gpu でもビルド可能（#229）** — 出力経路は 2 本: readback 系（`render_frame*` / `render_packed` → `RgbaImage`。blocking poll を使うため native 専用 cfg）と **to_view 系**（`*_to_view`: 外部から渡された `wgpu::TextureView` + `TextureFormat` に全 shape を描いて submit。browser の surface present 用 seam）。core は web-sys / canvas を一切知らず、surface の作成・configure・present は呼び出し側（orber-wasm, #230）が握る。初期化は wasm では async の `new_async()`（`new()` は pollster の native 専用ラッパー）。pipeline cache は `(shader, target format)` キー、glyph bleed の中間テクスチャは両経路とも `Rgba8Unorm` のまま最終 pass だけ format 可変。wasm のバックエンドは wgpu default feature の **webgpu のみ**（`webgl` feature は採らない = WebGPU 必須・fallback 無し）。CI に `cargo build --target wasm32-unknown-unknown -p orber-core --features gpu` あり
- **per-orb パラメータと WebGL 経路を共有する** — `animate.rs::pack_render_data_for_webgl` が header + per-orb 列を 1 本の `f32` バッファに詰め、ネイティブ GPU(`gpu.rs`) も Web の WebGL2 fragment shader も同じ pack を読む。算術は再実装しない（彩度だけはネイティブ側で後段適用、WebGL は独自ノブ）
- **アニメーション軌道は一方通行コンベア（#41）** — 位相は `seed` から決定論的に散らし、`(cycle * speed_mult * t).fract()` で巻き戻して t=0 と t=1 のフレームをループ閉じさせる（`cycle * speed_mult` が整数なので浮動小数点誤差なく一致）。orb 位置/色の変調は `animate.rs::aquarelle_modulated_clusters` 等で `Cluster` 列を作って pack に渡すだけで、形状側に新 API を増やさない
- **Web GUI の wasm は Worker で動かす（#75）** — メインスレッドは UI / DOM / Solid signal だけにして、wasm 描画 + WebCodecs エンコード + mp4-muxer は全部 `orberWorker.ts` 内で完結させる。スマホで生成中もタップ・スクロールが反応するためのコア施策。フォールバックパスは作らない（最新ブラウザ前提、死コード化を防ぐ）
- **プレビューと DL は別解像度で焼き分ける（#73）** — プレビュー 540×960、DL 時に worker で 1080×1920 に再描画。`random_batch_specs(seed, total, still_count)` の決定論性で同じバリエーションを別解像度で再現できる。比率 9:16 / 16:9 厳守
- **進行は skeleton で 2 段階表現（#71 #80）** — 強い shimmer (`.skeleton`) = タイル未生成、弱い shimmer (`.skeleton-soft`) = 静止 PNG は出たが mp4 化待ち。レイアウトは最初から 12 枚分確定させて伸縮しない
- **PWA は手書き Service Worker (#148)** — `web/public/sw.js` を直接書き、`@vite-pwa/astro` 等の追加依存は入れない。machigai-salad と同じく `CACHE_NAME = 'orber-__BUILD_DATE__'`、precache は `['/', '/manifest.webmanifest']` のみ。`/_astro/*` (Astro/Vite content-hash 付き immutable asset) は **CacheFirst**、それ以外は **network-first** + キャッシュ fallback。navigation がキャッシュ miss + オフラインなら precache した `/` を返す (shell 戦略)。`blob:` / `data:` は intercept しない（生成結果の DL を握り潰さないため）。`cache.put` は `event.waitUntil()` で SW lifetime に縛る。`npm run build` の `stamp:sw` 段で `dist/sw.js` の `__BUILD_DATE__` を JST 日付に Node 1 行スクリプトで literal 置換する。詳細は DESIGN.md §15
- **AffiliateGrid は横展開パターン (#152)** — Footer の Sponsor 直下に置く 3 商品 Amazon affiliate グリッドは、データ層 (`web/src/data/affiliateProducts.ts`) と UI 層 (`web/src/components/AffiliateGrid.tsx`) を分離し、**他 PWA リポにコピペで横展開する**前提で書く (npm パッケージ化はしない)。商品 URL は **amzn.to 短縮リンク** (Associates ダッシュボードで生成) を `url` フィールドに直接入れ、tag を URL に露出しない。商品カードは円形 mask + inset shadow + outer glow の orb スタイルで orber 本体と連続性を持たせる。詳細は DESIGN.md §16

## 関連プロジェクト

- [aquarelle](https://github.com/kako-jun/aquarelle)（v0.2 として独立済み）— にじみエンジンを独立 crate 化したもの。orber は `aquarelle = "0.2"` を依存し、`OrbShape::Aquarelle`（per-orb の `render_aquarelle_orb`）と `OrbShape::Glyph` / `OrbShape::Image`（全体 bleed pass `render_aquarelle_bleed_pass`）の **参照アルゴリズム** として使う。実描画は GPU(WGSL) でこれらを再現する（aquarelle は tiny-skia を内部で使うため、orber へは推移依存として残る）。blueprinter からも共有依存される想定

## 技術ルール

- コミットメッセージに Co-Authored-By を付けない
