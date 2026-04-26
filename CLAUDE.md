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

## ソース構成（予定）

```
src/
├── main.rs         # CLI パース（clap）
├── lib.rs          # モジュール宣言
├── cluster.rs      # 入力画像 → 代表色クラスタ抽出
├── orb.rs          # orb 1 個の描画（円形ぼかし）
├── animate.rs      # 時間 t におけるフレーム生成
├── video.rs        # 連番フレーム → MP4/WebM（ffmpeg 子プロセス）
├── style.rs        # CSS / SVG 静的書き出し
└── aquarelle/      # にじみ形状の orb（将来別 crate に分離する境界）
    └── mod.rs
```

## 主要な設計判断

- **prototype 段階はローカル Rust バイナリ単体で完結する** — Web フロント・WASM・crate.io 公開は将来 Issue
- **入力 → 静的 PNG が出るところまで先に通す** — 動画化はその後
- **にじみ処理は `src/aquarelle/` に隔離する** — 将来 `aquarelle` crate として独立させる前提でモジュール境界を切る。orber 本体（円形 orb）はにじみ処理に依存しない
- **動画書き出しは ffmpeg 子プロセス呼び出し** — 自前エンコードはやらない
- **動画入力対応も ffmpeg でフレーム抽出** — 抽出後は静止画パイプラインに合流させる
- **`--seed` で再現可能** — 同じ入力 + 同じ seed で同じ出力

## 関連プロジェクト

- [aquarelle](https://github.com/kako-jun/aquarelle)（将来作る予定）— にじみエンジンを独立 crate 化したもの。orber と blueprinter から共有依存される

## 技術ルール

- コミットメッセージに Co-Authored-By を付けない
