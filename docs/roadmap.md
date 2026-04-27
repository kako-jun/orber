# roadmap

orber prototype の進捗管理。詳細な議論は GitHub Issue で行い、ここでは状態だけ反映する。

## prototype 完成までの順路

ローカル Rust バイナリ単体で「写真 → orb 動画」が出力できるところまでを prototype と定義する。

| 順 | テーマ | 状態 |
|---|---|---|
| 0 | リポジトリ scaffold | ✅ 完了 |
| 1 | CLI 引数定義（入力 / 出力 / パラメータ） | ✅ 完了 |
| 2 | 入力画像から色クラスタ抽出 | ✅ 完了 |
| 3 | orb（円形ぼかし）静的描画 → PNG | ✅ 完了 |
| 4 | orb のゆったり移動アニメーション | ✅ 完了 |
| 5 | 縦長動画出力（連番 PNG → ffmpeg） | ✅ 完了（mp4 / webm を CLI から生成可能） |
| 6 | SVG / CSS 静的書き出し | ⏳ Issue |
| 7 | 動画入力対応（ffmpeg でフレーム抽出） | ⏳ Issue |
| 8 | にじみ処理を `src/aquarelle/` に隔離 | ⏳ Issue |

prototype はここまで。順番は前後してよいが、3 までは早めに通して「何かしら出力が見える」状態を作る。

## 将来 Issue（prototype 後）

- WASM ビルド + Web フロント（SvelteKit / Solid どちらにするか含めて検討）
- aquarelle を独立 crate に切り出し（blueprinter と共有）
- crates.io 公開（バイナリと crate）
- 宣伝記事（Zenn / X / Reddit）

## 関連リポジトリ

- [blueprinter](https://github.com/kako-jun/blueprinter) — 同じ aquarelle 連携を予定する手書き風 SVG レンダラー
