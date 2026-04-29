# orber - Abstract Orb Mood Renderer

写真や動画から抽象的な光の玉（orb）のムード画像/動画を生成する Rust CLI。

## ビルド・テスト

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

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
    │   └── src/
    │       ├── lib.rs
    │       ├── output_mode.rs  # 出力拡張子 → OutputMode 判定
    │       ├── cluster.rs      # 入力画像 → 代表色クラスタ抽出
    │       ├── orb.rs          # orb 1 個の描画（円形ぼかし）
    │       ├── animate.rs      # 時間 t におけるフレーム生成
    │       ├── style.rs        # CSS / SVG 静的書き出し
    │       ├── variations.rs   # バリエーション spec 定義
    │       ├── batch.rs        # generate_batch — 1 入力から複数 PNG を一括生成（GUI / WASM 用）
    │       └── aquarelle/      # にじみ形状（将来別 crate に分離する境界）
    │           └── mod.rs
    ├── cli/                # orber: CLI バイナリ（image::open / ffmpeg / tempfile）
    │   ├── Cargo.toml      #   [[bin]] name = "orber", path = "src/main.rs"
    │   └── src/
    │       ├── main.rs         # CLI パース（clap）。`Cli` / `Motion` / `Shape` 定義
    │       └── video.rs        # 連番フレーム → MP4/WebM（ffmpeg 子プロセス）
    └── wasm/               # orber-wasm: ブラウザ向け wasm-bindgen ラッパー（#36）
        ├── Cargo.toml      #   crate-type = ["cdylib", "rlib"]
        ├── test.html       #   ブラウザ確認用デモ（pkg/ を fetch）
        └── src/
            └── lib.rs          # generate_single / generate_batch / generate_svg /
                                # generate_one_at_index (#73) /
                                # start_animation_for_batch_spec (#52)

web/                        # Web フロントエンド (#37, #38)
├── astro.config.mjs        #   Astro 4 / output: 'static' / Solid + Tailwind
├── package.json            #   npm scripts: wasm:build / dev / build / deploy（jszip 依存）
├── wrangler.toml           #   Cloudflare Pages 設定（pages_build_output_dir = "dist"）
└── src/
    ├── pages/index.astro       # トップページ（ロゴ + Subtitle + Studio）
    ├── layouts/Base.astro      # 共通レイアウト（Space Grotesk + lang 自動切替, #62 /
    │                           # skeleton & skeleton-soft shimmer #71 #80）
    ├── components/Studio.tsx   # Solid アイランド。バッチ生成 GUI
    │                           # (#38, #62 glass, #61 12 枚統一 + 動画一斉再生,
    │                           #  #71 skeleton 先出し, #73 hi-res DL,
    │                           #  #75 worker 経由化, #80 video pending overlay)
    ├── components/Subtitle.tsx # Solid アイランド。用途提案サブタイトル（i18n, #62）
    ├── lib/decodeImage.ts      # File → RGB バイト列デコード（#38）
    ├── lib/encodeMp4.ts        # WebCodecs + mp4-muxer で MP4 化（#52, #75 で worker 内利用）
    ├── lib/orberWorker.ts      # #75 wasm 描画 + WebCodecs を実行する Worker 本体
    ├── lib/orberClient.ts      # #75 main 側 Worker クライアント（postMessage を Promise 化）
    ├── lib/strings.ts          # i18n 文言集約 + ja/en 自動切替（#62）
    └── wasm/                   # wasm-pack 出力先（gitignore、.gitkeep のみ追跡）
```

`std::fs` / `std::process::Command` / `tempfile` を使うのは `crates/cli/` だけ。`crates/core/` は wasm32-unknown-unknown でもビルド通る（getrandom 0.3 の wasm_js バックエンドを `.cargo/config.toml` で有効化済み）。

## 主要な設計判断

- **prototype 段階はローカル Rust バイナリ単体で完結する** — Web フロント・WASM・crate.io 公開は将来 Issue
- **入力 → 静的 PNG が出るところまで先に通す** — 動画化はその後
- **にじみ処理は `src/aquarelle/` に隔離する** — 将来 `aquarelle` crate として独立させる前提でモジュール境界を切る。orber 本体（円形 orb）はにじみ処理に依存しない
- **動画書き出しは ffmpeg 子プロセス呼び出し** — 自前エンコードはやらない
- **動画入力対応も ffmpeg でフレーム抽出** — 抽出後は静止画パイプラインに合流させる
- **`--seed` で再現可能** — 同じ入力 + 同じ seed で同じ出力
- **`Motion` / `Shape` enum は当面 `main.rs` に置く** — `animate.rs`（#4）で必要になった時点で `pub mod` に昇格させる。今は CLI パース直後にしか使わないので main.rs ローカルで十分
- **`duration_ms` は `u64` を採用** — `u32` でも 49 日分入って実用上は問題ないが、後段でのフレーム数計算（`duration_ms * fps / 1000` 等）でのオーバーフローを避けるため広めに取っておく
- **描画バックエンドは tiny-skia** — pure Rust で外部 C ライブラリ不要、`RadialGradient` をネイティブで持っており orb 表現に向く。`Pixmap` は **premultiplied alpha** なので、`RgbaImage` (straight alpha) に変換する際は un-premultiply が必要
- **アニメーション軌道はリサジュー曲線** — `animate.rs` の `render_frame` は `(sin(2π·a·t·s + φx), sin(2π·b·t·s + φy))` を採用。周波数比 `(a, b)` は整数比候補 `[(1,2),(2,3),(3,4),(1,3),(2,5)]` から `seed` で決定的に選ぶ。`(a · t · s).fract()` で位相を巻き戻すことで、t=0 と t=1 のフレームが浮動小数点誤差なく完全一致する（ループ性保証）。色揺らぎは HSL の S/L に追加倍率として乗せ、`saturation` フラグの倍率と二重掛けにならないよう独立させる
- **`animate.rs` は `orb::render_static` を再利用** — 位置と色を変調した `Cluster` 列を作って渡すだけ。orb 側に新 API を増やさない
- **Web GUI の wasm は Worker で動かす（#75）** — メインスレッドは UI / DOM / Solid signal だけにして、wasm 描画 + WebCodecs エンコード + mp4-muxer は全部 `orberWorker.ts` 内で完結させる。スマホで生成中もタップ・スクロールが反応するためのコア施策。フォールバックパスは作らない（最新ブラウザ前提、死コード化を防ぐ）
- **プレビューと DL は別解像度で焼き分ける（#73）** — プレビュー 540×960、DL 時に worker で 1080×1920 に再描画。`random_batch_specs(seed, total, still_count)` の決定論性で同じバリエーションを別解像度で再現できる。比率 9:16 / 16:9 厳守
- **進行は skeleton で 2 段階表現（#71 #80）** — 強い shimmer (`.skeleton`) = タイル未生成、弱い shimmer (`.skeleton-soft`) = 静止 PNG は出たが mp4 化待ち。レイアウトは最初から 12 枚分確定させて伸縮しない

## 関連プロジェクト

- [aquarelle](https://github.com/kako-jun/aquarelle)（将来作る予定）— にじみエンジンを独立 crate 化したもの。orber と blueprinter から共有依存される

## 技術ルール

- コミットメッセージに Co-Authored-By を付けない
