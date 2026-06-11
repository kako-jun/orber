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

`web/` の vitest は #163 で導入。新機能追加時は `web/src/lib/` の同階層に
`*.test.ts` を増やしていく方針 (jsdom 環境、`vitest.config.ts` 参照)。
strings / encodeMp4 / jsGlyphSdf / orberClient / workerLogic 等を
単体テスト化済み。worker / Solid コンポーネントに埋まったロジックは、純移動で
`lib/` の純粋関数に切り出してからテストする（#245 workerLogic.ts の流儀）。

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
    │       ├── animate.rs      # フレーム parameters（AnimateOptions / pack_render_data 等）
    │       ├── glyph.rs        # フォント/画像 → SDF（ttf-parser + zeno、#223）
    │       ├── gpu.rs          # GPU(WGSL, wgpu) レンダラ — 唯一のレンダラ（#207〜#225）
    │       ├── orb.wgsl（#235 統一テンプレ: orb/glyph/image 共通。#239 で にじみ(bleed)=空間ブラー
    │       │              （blurred_coverage）＋ character 3 軸（bloom/halo/offset）をこの上に実装。
    │       │              旧 orb_aquarelle.wgsl（radial 4 層 = --shape aquarelle）は #239 Phase 1 で撤去）
    │       ├── style.rs        # CSS / SVG 静的書き出し
    │       └── variations.rs   # バリエーション spec 定義
    │                           # にじみ処理は外部 crate `aquarelle = "0.2"` に分離済み（旧 src/aquarelle/ は撤去）
    ├── cli/                # orber: CLI バイナリ（image::open / ffmpeg / tempfile）
    │   ├── Cargo.toml      #   [[bin]] name = "orber", path = "src/main.rs"
    │   └── src/
    │       ├── main.rs         # CLI パース（clap）。`Cli` / `Motion` / `Shape` 定義。
    │       │                   #   #239 水彩 3 段ボタン: --bleed <weak|mid|strong>（=0.15/0.3/0.5、全 shape の
    │       │                   #   にじみ＝orb.wgsl の空間ブラー）＋ --bloom/--halo/--offset <weak|mid|strong>
    │       │                   #   （=0.3/0.6/0.9、requires=bleed）。にじみは continuous 一本（#239 で blob 変種と
    │       │                   #   内部 --aquarelle-bleed-mode を撤去）。--shape は orb/glyph/image のみ（aquarelle 撤去）
    │       └── video.rs        # 連番フレーム → MP4/WebM（ffmpeg 子プロセス）
    └── wasm/               # orber-wasm: ブラウザ向け wasm-bindgen ラッパー（#36）
        ├── Cargo.toml      #   crate-type = ["cdylib", "rlib"]。wasm32 専用 target dependency で
        │                   #   gpu 経路（orber-core gpu feature + wgpu + web-sys）を常時有効化（#230）
        └── src/
            ├── lib.rs          # データ供給（#225 で CPU 描画 generate_* は撲滅）。
            │                   #   本番 Web のデータ供給は gpu_set_render_data（WGSL 経路: build_gpu_render_inputs →
            │                   #   core の pack_render_data）。#247 で旧 WebGL 供給 export get_render_data /
            │                   #   build_render_pack / webgl_shape_id は削除済み /
            │                   #   get_glyph_sdf（フォント文字 → SDF）。本番 Web の実描画は gpu.rs の WGSL 経路（#245）
            └── gpu.rs          # WebGPU canvas present 経路（#230、wasm32 専用 cfg）。
                                #   gpu_init（HTMLCanvasElement: gpu-lab dev ページ）/
                                #   gpu_init_offscreen（OffscreenCanvas: Worker 本番経路、#245）/
                                #   gpu_set_render_data / gpu_render / gpu_resize /
                                #   gpu_render_rgba（透過 export 用 straight-alpha 非同期 readback、#245）。
                                #   core の GpuRenderer(WGSL) が canvas surface に直接描く。
                                #   全 shape 配線済み（#231: orb / glyph / image を opts.shape で
                                #   render_packed_to_view / render_frame_*_to_view へ分岐。aquarelle は #239 撤去）
                                #   glyph は同梱フォント外の字（漢字/絵文字）を JS の generateJsGlyphSdf で
                                #   SDF 化し WasmParams.glyph_sdf で受けて OrbShape::Image に解決（#159 と同設計）

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
    ├── lib/orberWorker.ts      # #75 wasm 描画 + WebCodecs を実行する Worker 本体。
    │                           # #245 で旧 GPU 経路 → wasm WebGPU 経路
    │                           # （gpu_init_offscreen / gpu_set_render_data / gpu_render /
    │                           #  透過は transparent_background + gpu_render_rgba）に配線替え。
    │                           # 本番出力は #242+#241 の確定ルック（CLI と同一 WGSL）
    ├── lib/orberClient.ts      # #75 main 側 Worker クライアント（postMessage を Promise 化）
    ├── lib/workerLogic.ts      # #245 worker / Studio の純粋ロジック切り出し
    │                           # （buildWasmParams 引数DI / computeMaskSize /
    │                           #  formatRunBatchError。vitest 対象）
    ├── lib/strings.ts          # i18n 文言集約 + ja/en 自動切替（#62）
    └── wasm/                   # wasm-pack 出力先（gitignore、.gitkeep のみ追跡）
```

`std::fs` / `std::process::Command` / `tempfile` を使うのは `crates/cli/` だけ。`crates/core/` は wasm32-unknown-unknown でもビルド通る（getrandom 0.3 の wasm_js バックエンドを `.cargo/config.toml` で有効化済み）。

## 主要な設計判断

- **prototype 段階はローカル Rust バイナリ単体で完結する** — Web フロント・WASM・crate.io 公開は将来 Issue
- **入力 → 静的 PNG が出るところまで先に通す** — 動画化はその後
- **「にじみ(bleed)」は #239 で orb.wgsl の本物の空間ブラーに作り直した（別シェーダでない方向）** — `--bleed <weak|mid|strong>` は **全 shape（orb / glyph / image）共通**で、`orb.wgsl` の `blurred_coverage`（被覆 alpha を blur 半径 ∝ bleed の disk 内で黄金角スパイラル＋per-pixel hash21 ディザで multi-tap 平均）で滲ませる。**タップ数は #265 で 48→既定 5 に削減**（共有 `aquarelle` の `AQUA_BLUR_TAPS=48` を orber 側 `substitute_aqua_taps` で差し替え。crate 本体は不変＝additive/blueprinter は 48 のまま）: 48 タップの一撃描画がモバイル GPU のウォッチドッグを超えてタブをクラッシュさせたため。ぼかし(falloff)が見た目を支配しタップ品質の寄与は小さく、強めでも 5≈48（標準count・強めで 48 比 目視差画素 0.45%/PSNR52dB、t4 で初めて 3% ザラつき。当初 8→kako-jun「もっと下げていい」で 5）。`GpuRenderer::with_aqua_taps` / CLI `--bleed-taps` で可変、web(wasm) は既定 5。なお個数プリセット「多め(high)」も #265 で **30→25** に下げた（モバイルで「多め」が少し重かった＝orb 数が主因、タップとは別軸。kako-jun 確定値 25）。星は星のままぼけ、強ブラーで自然に溶ける（**距離場を円へモーフしない**）。被覆評価は variant 別の `coverage_at`（orb=円距離 / SDF=サンプル距離）に関数化し plain 1 タップとブラー multi-tap で共有。上に控えめな character 3 軸 `--bloom`/`--halo`/`--offset`（`Params.aqua_bloom`=中心の芯 BLOOM_MAX=0.45 / `aqua_halo`=外周の彩度・枠リング無し / `aqua_offset`=ブラー原点の seed 方向バイアス）を加算。**全パラメータ=0（特に bleed=0）で plain orb / glyph / image と byte 一致**（`aqua_zero_params_byte_match_plain_orb`）。**#235 由来の旧 bleed pass（aquarelle crate 経由の glyph/image にじみ）はもう無い** — `OrbShape::Glyph` / `OrbShape::Image` は #235 で orb 機構（`orb.wgsl` の SDF variant）に統一され独自の bleed pass を持たない
- **旧 `--shape aquarelle`（`orb_aquarelle.wgsl` の radial 4 層）は #239 Phase 1 で撤去済み** — shape モデルは orb/glyph/image の 3 択＋水彩(bleed)軸に一本化。`aquarelle = "0.2"` 依存は **温存**（共有にじみエンジン crate。blueprinter も使用。orber の新 bleed をここへ切り出す計画 #250/#251。orber-core は `pub use aquarelle;` で再エクスポート）
- **動画書き出しは ffmpeg 子プロセス呼び出し** — 自前エンコードはやらない
- **動画入力対応も ffmpeg でフレーム抽出** — 抽出後は静止画パイプラインに合流させる
- **`--seed` で再現可能** — 同じ入力 + 同じ seed で同じ出力
- **`Motion` / `Shape` enum は当面 `main.rs` に置く** — `animate.rs`（#4）で必要になった時点で `pub mod` に昇格させる。今は CLI パース直後にしか使わないので main.rs ローカルで十分
- **`duration_ms` は `u64` を採用** — `u32` でも 49 日分入って実用上は問題ないが、後段でのフレーム数計算（`duration_ms * fps / 1000` 等）でのオーバーフローを避けるため広めに取っておく
- **描画バックエンドは GPU(WGSL, wgpu) が唯一（#225 で tiny-skia 撲滅）** — ネイティブ CLI は `crates/core/src/gpu.rs` の `GpuRenderer` が全 shape（Orb / Glyph / Image）を WGSL で描く。#235 で Orb / Glyph / Image は統一テンプレ `orb.wgsl` の 2 variant（orb=解析距離 / SDF=glyph・image）に集約され、Glyph / Image は単パスで bleed/halo を持たない。CPU(tiny-skia) ピクセル描画・CPU↔GPU parity オラクル・`--renderer cpu`・CPU フォールバックは削除済み。GPU アダプタが取れなければ `GpuRenderer::new` が `None` を返し、CLI は error 終了する（フォールバック無し）。tiny-skia は外部 crate `aquarelle` 経由の推移依存としてのみ残る（orber 自身のコード/マニフェストは tiny-skia フリー）。orb 機構（orb/glyph/image, `orb.wgsl`）の合成は #242 裁定で**旧 WebGL の straight alpha float Source-Over を 1:1 移植**したもの（旧来の Skia lowp 再現 = u8 量子化 → premultiply → source_over は暗部が沈むため撤去。WebGL レンダラ実体は #245 PR-B で削除済み、アルゴリズムだけが正として WGSL に残る）。#241 でその上に**「薄い影」**を重ねた: 最外周フェードセグメントだけ orb 色 rgb を `mix(1.0, 1.0-u, shadow_strength)` 倍に暗化する（旧 lowp の rgb→0 フェードの強度係数化。s=0 で #242 と bit 同一、s=1 ≒ 旧 lowp の暗さ。falloff の r に乗るので全 shape シルエット沿い）。強度は `core::animate::SHADOW_STRENGTH_DEFAULT`（製品定数 0.2 = kako-jun 実機選定・session595）に 1 箇所集約され、pack header[13] → Params uniform で WGSL に届く。チューニングは gpu-lab の shadow スライダー（`WasmParams.shadow_strength`、0..=1 外 reject）のみで、CLI フラグ・Studio UI は無い
- **GpuRenderer は wasm32 + gpu でもビルド可能（#229）** — 出力経路は 2 本: readback 系（`render_frame*` / `render_packed` → `RgbaImage`。blocking poll を使うため native 専用 cfg）と **to_view 系**（`*_to_view`: 外部から渡された `wgpu::TextureView` + `TextureFormat` に全 shape を描いて submit。browser の surface present 用 seam）。core は web-sys / canvas を一切知らず、surface の作成・configure・present は呼び出し側（orber-wasm, #230）が握る。初期化は wasm では async の `new_async()`（`new()` は pollster の native 専用ラッパー）。pipeline cache は `(shader, target format)` キー、glyph bleed の中間テクスチャは両経路とも `Rgba8Unorm` のまま最終 pass だけ format 可変。wasm のバックエンドは wgpu default feature の **webgpu のみ**（`webgl` feature は採らない = WebGPU 必須・fallback 無し）。CI に `cargo build --target wasm32-unknown-unknown -p orber-core --features gpu` あり
- **per-orb パラメータの pack は CLI と Web で共有する** — `animate.rs::pack_render_data`（#247 で旧称 `pack_render_data_for_webgl` から改名。WebGL 撤去後、これは core GPU / CLI の正規 pack ヘルパで WebGL 専用ではない）が header + per-orb 列を 1 本の `f32` バッファに詰め、ネイティブ GPU(`gpu.rs`) と Web の wasm 経路が同じ pack を読む。算術は再実装しない（彩度だけはネイティブ側で後段適用）。**#245 で Web 本番（Worker）は WGSL 経路に統一済み**: 旧 WebGL2 fragment shader レンダラ（orberGl.ts）と A/B 足場は PR-B で削除した。pack の JS 返し export `get_render_data`（および `build_render_pack` / `webgl_shape_id`）は #247 で削除済み。本番のデータ供給は `gpu_set_render_data`（`build_gpu_render_inputs` → `pack_render_data` の WGSL 経路）が握る
- **アニメーション軌道は一方通行コンベア（#41）** — 位相は `seed` から決定論的に散らし、`(cycle * speed_mult * t).fract()` で巻き戻して t=0 と t=1 のフレームをループ閉じさせる（`cycle * speed_mult` が整数なので浮動小数点誤差なく一致）。orb 位置/色の変調は `Cluster` 列を作って pack に渡すだけで、形状側に新 API を増やさない（動画の色/キーフレーム track 変調は #239 で旧 aquarelle 経路と共に dead 化、統一レンダラへの再配線は #251）
- **Web GUI の wasm は Worker で動かす（#75）** — メインスレッドは UI / DOM / Solid signal だけにして、wasm 描画 + WebCodecs エンコード + mp4-muxer は全部 `orberWorker.ts` 内で完結させる。スマホで生成中もタップ・スクロールが反応するためのコア施策。フォールバックパスは作らない（最新ブラウザ前提、死コード化を防ぐ）。**#245 で Worker の描画は WebGPU(WGSL) のみ**: `gpu_init_offscreen(OffscreenCanvas)` → `gpu_set_render_data` → `gpu_render(t)`、透過 export は `transparent_background` + `gpu_render_rgba`（straight-alpha readback）。WebGPU 非対応ブラウザは sentinel `webgpu-unsupported` → Studio が `webgpuUnsupported` 文言を表示して生成不可（#207 裁定）
- **プレビューと DL は別解像度で焼き分ける（#73）** — プレビュー 540×960、DL 時に worker で 1080×1920 に再描画。`random_batch_specs(seed, total, still_count)` の決定論性で同じバリエーションを別解像度で再現できる。比率 9:16 / 16:9 厳守
- **進行は skeleton で 2 段階表現（#71 #80）** — 強い shimmer (`.skeleton`) = タイル未生成、弱い shimmer (`.skeleton-soft`) = 静止 PNG は出たが mp4 化待ち。レイアウトは最初から 12 枚分確定させて伸縮しない
- **PWA は手書き Service Worker (#148)** — `web/public/sw.js` を直接書き、`@vite-pwa/astro` 等の追加依存は入れない。machigai-salad と同じく `CACHE_NAME = 'orber-__BUILD_DATE__'`、precache は `['/', '/manifest.webmanifest']` のみ。`/_astro/*` (Astro/Vite content-hash 付き immutable asset) は **CacheFirst**、それ以外は **network-first** + キャッシュ fallback。navigation がキャッシュ miss + オフラインなら precache した `/` を返す (shell 戦略)。`blob:` / `data:` は intercept しない（生成結果の DL を握り潰さないため）。`cache.put` は `event.waitUntil()` で SW lifetime に縛る。`npm run build` の `stamp:sw` 段で `dist/sw.js` の `__BUILD_DATE__` を JST 日付に Node 1 行スクリプトで literal 置換する。詳細は DESIGN.md §15
- **AffiliateGrid は横展開パターン (#152)** — Footer の Sponsor 直下に置く 3 商品 Amazon affiliate グリッドは、データ層 (`web/src/data/affiliateProducts.ts`) と UI 層 (`web/src/components/AffiliateGrid.tsx`) を分離し、**他 PWA リポにコピペで横展開する**前提で書く (npm パッケージ化はしない)。商品 URL は **amzn.to 短縮リンク** (Associates ダッシュボードで生成) を `url` フィールドに直接入れ、tag を URL に露出しない。商品カードは円形 mask + inset shadow + outer glow の orb スタイルで orber 本体と連続性を持たせる。詳細は DESIGN.md §16

## 関連プロジェクト

- [aquarelle](https://github.com/kako-jun/aquarelle)（v0.2 として独立済み）— にじみエンジンの独立 crate。**共有にじみエンジン**で blueprinter が `render_aquarelle_bleed_pass` を本番採用済み。orber は `aquarelle = "0.2"` を依存（**温存**: 旧 `OrbShape::Aquarelle` radial は #239 Phase 1 で撤去したが、依存は切り出し先として残し orber-core が `pub use aquarelle;` で再エクスポート）。**計画: orber の新 bleed（`orb.wgsl` の空間ブラー、今は inline・この crate を経由しない）を aquarelle へ切り出し、blueprinter と収束させる（#250/#251）**。`OrbShape::Glyph` / `OrbShape::Image` は #235 で orb 機構に統一済み。aquarelle crate は tiny-skia を内部で使うため orber へは推移依存として残る

## 技術ルール

- コミットメッセージに Co-Authored-By を付けない
