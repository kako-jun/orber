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
| 7 | 動画入力対応（ffmpeg でフレーム抽出） | ✅ 完了（色トラック方式 — 位置固定 / 色だけ時間変化） |
| 8 | にじみ処理を `crates/core/src/aquarelle/` に隔離 | ✅ 完了（外部 crate `aquarelle = "0.2"` に分離） |
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
| 19 | 動画タイル毎の mp4 化進捗表示（フレーム単位リング） | ✅ 完了（#95 / PR #97） |

prototype はここまで。順番は前後してよいが、3 までは早めに通して「何かしら出力が見える」状態を作る。

## 将来 Issue（prototype 後）

- Web フロントの追加機能（#38 で 10 枚バッチ生成 GUI 完成、#61 で 12 枚統一に更新済。今後は動画入力・パラメータ調整 UI を検討。描画は WebGL2 fragment shader、データ供給は wasm の `get_render_data` / `get_glyph_sdf`）
- 動画化を静止画生成と並走させる設計（#92 で worker channel を 'still' / 'video' に分割して一度実装したが、worker 2 本起動 + RGB 2 回 clone + wasm 2 回 init のオーバーヘッドで静止画 1〜12 の表示が遅延、並走によるレースで「9 の進捗が完了しても再生されず 10 の進捗が先に出る」「完成済み静止画に shimmer が残る」等のリグレッションが出たため #99 でロールバック。再挑戦するなら overhead を抑える別アプローチが必要）
- aquarelle を独立 crate に切り出し（blueprinter と共有）
- crates.io 公開（バイナリと crate）
- 宣伝記事（Zenn / X / Reddit）

### overlay 用途のチューニング（オープン）

- #53 1 画像内 orb 速度を ×1/×2/×3 の 3 段階混在に拡張
- #77 タイル全体スクロール速度を遅く（ループ長 4s → 8s 等）
- #78 orb 縁のコントラストを下げる（文字オーバーレイの可読性）
- #79 ドロップエリアの border を破線 → 丸ドット周回（orb との視覚統一）

### #55 アドバンストモード

- **Phase A (CLI / core 拡張) 完了 ✅** — 5 commits（`ab1fea5` / `18f17eb` / `8719117` / `6c5ba82` / `acf9f96`）
  - フォント資産: `crates/core/assets/fonts/NotoSansSymbols2-Regular.ttf`（177KB subset, `include_bytes!` 埋込）
  - core: `OrbShape::Glyph { ch, font: GlyphFontId }`、`MotionSpeed::{Mid, Fast}`、`SoftnessPreset { Low, Mid, High }` 追加。`OrbShape: Copy` を維持するため `OnceLock<Face<'static>>` グローバルキャッシュ + ID enum 方式（#217 で `OrbShape::Image` の `Arc<[u8]>` 導入により `Copy`→`Clone` に緩和）
  - cli: `--shape glyph` / `--glyph-char <CHAR>` / `--count-preset {low,mid,high}` / `--speed {mid,fast}` / `--softness {low,mid,high}` を追加。既存挙動は `--softness mid` で完全同値（regression test 済）
  - 依存追加: `ttf-parser = "0.25"`
- **Phase B (Web GUI + WebGL Glyph 描画) 完了 ✅** — `55-phase-b-web-gui-glyph` ブランチで実装
  - wasm: `WasmParams` に `glyph_char` / `count_preset` / `speed_preset` / `softness_preset` を追加。`MotionSpeed::Mid` / `Fast` を wasm 入口に露出（panic を撤去）。新 API `get_glyph_sdf(ch, size)` / `glyph_supported(ch)` を公開
  - core: glyph backend は後続 #132 で `render_glyph_alpha_mask` から `render_glyph_sdf(font, ch, size) -> Vec<u8>` へ差し替え。worker / wasm 側で `(font, ch, size)` キャッシュ
  - WebGL fragment shader: `u_glyph_sdf: sampler2D` (R8) / `u_shape_id` / `u_alpha_mul` を追加。#132 で per-orb `base_angle` / `rot_speed_signed` も追加され、Glyph は SDF → 共通 falloff → animated rotation の経路になった
  - Studio.tsx: #131 でさらに整理し、aspect / shape / count / speed / softness の全ボタン即生成、フラットな常時表示 row、IME-safe glyph 入力、symbol picker、下端の小さい 🔄 ボタンへ着地
  - DESIGN.md §13 Control Rows を更新

### ネイティブ描画の GPU(WGSL) 化 — Phase 1〜1.5 完了 ✅

- **#207 Phase 0–1c**: ネイティブ CLI / core の描画を `wgpu` + WGSL に移行。Circle
  (`orb_circle.wgsl`) → Glyph (`orb_glyph.wgsl`, #212) → Glyph bleed 2nd pass
  (`orb_glyph_bleed.wgsl`, #214) → Aquarelle (`orb_aquarelle.wgsl`, #216 Phase 1c) を順に WGSL 化。
  Image は Glyph と同じ SDF shader を共有（#217）。orb 上限は data-texture 経路で 1024 まで（#210 Phase 1a）。
- **#235 orb 機構統一**: 上記の `orb_circle.wgsl` / `orb_glyph.wgsl` / `orb_glyph_bleed.wgsl`
  を `orb.wgsl` 1 本に統合（DISTANCE SOURCE だけ差し替えの 2 variant: orb=解析距離 / glyph・image=SDF 距離）。
  glyph / image を「orb に別シルエットを食わせる」単パスに純化し、bleed/halo を撲滅（にじみは aquarelle 専用に）。
  名称も circle→orb に統一（CLI `--shape orb`・`OrbShape::Orb`・wasm・Web・docs）。orb 出力は byte-exact 不変
  （当時。#242 で合成を旧 WebGL 式へ置換し基線引き直し）。
  WebGL（orberGl.ts）は不触で Phase 3 削除予定。
- **#223**: グリフ font→SDF を Skia 系から `zeno`（pure Rust, wasm 可）に置換。
- **#225 撲滅完了**: CPU(tiny-skia) ピクセル描画 / CPU↔GPU parity オラクル /
  `--renderer cpu` / CPU フォールバック / wasm `generate_single/batch/svg/one_at_index/start_animation_for_batch_spec` /
  `crates/core/src/batch.rs` / `tiny-skia` 直接依存を削除。**GPU(WGSL) が唯一のレンダラ**。
  no-adapter は error（フォールバック無し）。tiny-skia は外部 crate `aquarelle` 経由の推移依存としてのみ残る。
  検証は GPU 構造テスト（lit-pixel 有無・決定論・cache 再利用・ループ閉じ・空 cluster は背景のみ）＋実機目視。

### ブラウザ WebGPU 化 — #207 Phase 2 進行中

- [x] **#229**: core `GpuRenderer` の wasm32 対応 — `new_async()` を pub 化（pollster /
  `new()` は native 専用 cfg）、`*_to_view` 経路を追加（外部 `wgpu::TextureView` +
  `TextureFormat` に全 shape を描く surface present 用 seam。pipeline cache は
  `(shader, target format)` キー、glyph bleed は最終 compose pass だけ format 可変）、
  readback 系（`render_frame*` / `render_packed`）を native 専用に cfg 整理。
  wasm32 + gpu ビルドを CI で常時検証。バックエンドは webgpu のみ
  （`webgl` feature は採らない = WebGPU 必須・fallback 無し）
- [x] **#230**: orber-wasm に WebGPU 最小経路（canvas surface + Orb）— wasm32 専用
  target dependency で gpu 経路を常時有効化（wasm-pack / CI ともフラグ不要）。
  `gpu_init`（canvas surface + compatible_surface adapter、async）/
  `gpu_set_render_data`（`get_render_data` と同一 pack 経路を共有）/
  `gpu_render(t)`（`render_packed_to_view` → present）/ `gpu_resize`。core には
  `GpuRenderer::from_device_queue` seam を追加。surface format は non-sRGB
  （Bgra8Unorm / Rgba8Unorm）明示選択・alpha は Opaque 優先。検証は
  `web/src/pages/gpu-lab.astro`（開発用、本番導線に出さない）。main thread 配置
  （Worker 配線の要否は Phase 3 で判断）。wasm バンドル 649KB → 896KB（+247KB）
- [x] **#231**: Glyph / Image / Aquarelle をブラウザ WGSL に配線 — `ensure_gpu_supported_shape`
  を撤去し、`gpu_render` を shape 別に core の `render_packed_to_view`（orb）/
  `render_frame_glyph_to_view` / `render_frame_image_to_view` / `render_frame_aquarelle_to_view`
  へディスパッチ（CLI と同一分岐）。`WasmParams` に aquarelle 4 パラメータ
  （bleed / bloom / offset / halo、既定 0.5、wasm 入口に初登場）と image マスク入力
  （`image_mask_rgba` / `width` / `height`、core の `image_rgba_to_sdf` で SDF 化 256）を追加。
  gpu-lab に shape 切替 / glyph 文字入力（rotate トグル）/ image アップロード / aquarelle
  スライダー 4 本を追加。WebGL 経路（`webgl_shape_id`）は aquarelle を明示 reject・
  `get_render_data` バイト列不変
- [x] **#232**: Studio に WebGL↔WGSL トグル A/B 比較パネル — `?ab=1` クエリでのみ
  表示される検証パネル（`web/src/components/AbPanel.tsx`、本番 UI / 生成経路は不汚染）。
  canvas 2 枚スタックで WebGL（`GlRenderer` main thread）↔ WGSL（`gpu_init` / `gpu_render`）を
  瞬時に切替（blink 比較、wall-clock `t` で同位相）し、同一入力（Studio の source / shape /
  プリセット、固定 seed=42 / n=12 / spec_idx=8、定数は `web/src/lib/abLogic.ts`）で見比べる。
  GPU init ms / FPS を計測表示し、`isWebGpuSupported()`（`web/src/lib/webgpu.ts`）が false の環境では
  WGSL 側を disabled にする。A/B の意味は shape 別: orb=パリティゲート / glyph・image=新旧見比べ
  （#235 で機構が変わったため一致しないのが正）/ aquarelle=対象外。**Phase 3 で WebGL（orberGl.ts）を
  撤去するとき AbPanel.tsx / lib/webgpu.ts / abLogic.ts / strings.ts の ab* キーごと削除する足場**。
  テスト 19 件追加（webgpu 4 / abLogic 10 / strings 5、計 104）。
  orb のパリティゲートは #242 で達成済み（kako-jun の実機 blink サインオフ待ち）
- [x] **#242**: WGSL present パリティ — #232 blink の「WGSL が全体に暗い・灰色の枠」を三者画素比較
  （CLI readback / ブラウザ WGSL / ブラウザ WebGL。`?ab=1&abcap=1` キャプチャ +
  `crates/wasm/src/ab_harness.rs` の ab_dump / ab_diff）で診断し、present 経路はシロ
  （ブラウザ WGSL = CLI readback）・**旧 WebGL が core より一様に明るい**（lowp 合成の
  rgb→0 フェードが暗部を沈めていた）ことを特定。kako-jun 裁定「旧の明るさが良い」で
  旧 WebGL（orberGl.ts）の GLSL 合成を `orb.wgsl` へ 1:1 移植（raw float stop alpha +
  straight-alpha float Source-Over。aquarelle は参照が aquarelle crate なので lowp 維持）し、
  ブラウザ WGSL ↔ WebGL = 2 サンプル ±1 / 518,400 の実質 byte-exact パリティ達成
  （Apple A18 Pro / Metal）。副産物として `random_batch_specs` の usize 抽選が
  wasm32/native で RNG 列が割れるプラットフォーム依存バグも u32 固定で修正（ブラウザ出力は不変）
- [ ] **#241**: SDF 距離の正規化を局所太さ（medial 深さ）基準に — ● glyph が orb と厳密に同式
  （r = dist/radius）へ退化し、細い線・画像の細部も各部の中心線が最大輝度で光る
  （「黒い記号・黒い画像」の解消）。あわせて #242 裁定の補足「旧ベース + 新のアレンジを薄く重ねる」=
  控えめ・シルエット沿いの影の上乗せもここで調整する
- [ ] **#233**: Aquarelle Web 初公開 → #239 に統合 close（aquarelle は A/B 対象外）

## 公開準備

- [x] GitHub Releases workflow（tag `v*` で Linux/macOS/Windows artifact を生成）
- [x] CHANGELOG.md 作成
- [x] `v0.2.0` リリース（背景色 / motion / variations / aquarelle / range validation）
- [x] `v0.3.0` リリース（#41: motion を一方通行コンベアベルトに刷新、direction × speed × count × orb_size × blur の 5 軸で variations を再構築。色軸は廃止し入力画像の kmeans 結果をそのまま使う。blur / opacity を独立呼吸軸に追加。OrbStyle Rim / Soft をフレーム内に混在。`--count` CLI を追加。各 orb に整数倍速度 multiplier 1x/2x を seed 由来で割当て、wrap 周期を [-r, 1+r] に拡張して画面外で orb が出入りするようにした。MotionSpeed は VerySlow / Slow の 2 段に絞り、最高速側を 2 重にカット）
- [ ] `cargo publish`

## 関連リポジトリ

- [blueprinter](https://github.com/kako-jun/blueprinter) — 同じ aquarelle 連携を予定する手書き風 SVG レンダラー
