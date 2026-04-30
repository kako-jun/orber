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
| 6 | SVG / CSS 静的書き出し | ✅ 完了（svg / css を CLI から生成可能） |
| 7 | 動画入力対応（ffmpeg でフレーム抽出） | ⏳ Issue |
| 8 | にじみ処理を `crates/core/src/aquarelle/` に隔離 | ⏳ Issue |
| 9 | workspace 化（orber-core / orber CLI 分離、wasm ビルド対応） | ✅ 完了（#35） |
| 10 | WASM バインディング（`crates/wasm`、wasm-bindgen） | ✅ 完了（#36） |
| 11 | Web フロント scaffold（Astro + Solid + Tailwind + WASM） | ✅ 完了（#37） |
| 12 | Web フロント 10 枚バッチ生成 GUI（ドロップ → ❤ 選択 → DL/ZIP）※後に #61 で 12 枚統一 | ✅ 完了（#38） |
| 13 | デザインシステム整備 + 日英自動切替（DESIGN.md / glass UI / i18n） | ✅ 完了（#62） |
| 14 | バッチ枚数を 12 枚統一 + 動画 4 枚一斉再生（後に #88 で「できた順に再生」へ変更） | ✅ 完了（#61 → #88） |
| 15 | タイルグリッドに skeleton shimmer を先出し（体感速度改善） | ✅ 完了（#71 / PR #72） |
| 16 | DL 時に 1080×1920 で再描画して高解像度版を出す | ✅ 完了（#73 / PR #74） |
| 17 | wasm 描画 + WebCodecs を Web Worker に追い出す（メインスレッドを空ける） | ✅ 完了（#75 / PR #76） |
| 18 | 動画タイルが mp4 化完了するまで soft shimmer + 動画化中バッジ | ✅ 完了（#80 / PR #81） |

prototype はここまで。順番は前後してよいが、3 までは早めに通して「何かしら出力が見える」状態を作る。

## 将来 Issue（prototype 後）

- Web フロントの追加機能（#38 で 10 枚バッチ生成 GUI 完成、#61 で 12 枚統一に更新済。今後は `generate_single` / `generate_svg` 経路や、動画入力・パラメータ調整 UI を検討）
- aquarelle を独立 crate に切り出し（blueprinter と共有）
- crates.io 公開（バイナリと crate）
- 宣伝記事（Zenn / X / Reddit）

### overlay 用途のチューニング（オープン）

- #53 1 画像内 orb 速度を ×1/×2/×3 の 3 段階混在に拡張
- #77 タイル全体スクロール速度を遅く（ループ長 4s → 8s 等）
- #78 orb 縁のコントラストを下げる（文字オーバーレイの可読性）
- #79 ドロップエリアの border を破線 → 丸ドット周回（orb との視覚統一）

## 公開準備

- [x] GitHub Releases workflow（tag `v*` で Linux/macOS/Windows artifact を生成）
- [x] CHANGELOG.md 作成
- [x] `v0.2.0` リリース（背景色 / motion / variations / aquarelle / range validation）
- [x] `v0.3.0` リリース（#41: motion を一方通行コンベアベルトに刷新、direction × speed × count × orb_size × blur の 5 軸で variations を再構築。色軸は廃止し入力画像の kmeans 結果をそのまま使う。blur / opacity を独立呼吸軸に追加。OrbStyle Rim / Soft をフレーム内に混在。`--count` CLI を追加。各 orb に整数倍速度 multiplier 1x/2x を seed 由来で割当て、wrap 周期を [-r, 1+r] に拡張して画面外で orb が出入りするようにした。MotionSpeed は VerySlow / Slow の 2 段に絞り、最高速側を 2 重にカット）
- [ ] `cargo publish`

## 関連リポジトリ

- [blueprinter](https://github.com/kako-jun/blueprinter) — 同じ aquarelle 連携を予定する手書き風 SVG レンダラー
