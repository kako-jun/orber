// orber#62 — i18n module.
// 言語切替 UI は持たない。`detectLang()` でブラウザ言語を見て ja / en を返し、
// `t(key, vars?)` で文字列を取り出す。SolidJS signal `lang` を更新すると
// 全ての `t()` 呼び出しが reactive に再評価される。
//
// 文字列は subtitle / dropZoneLabel ... など 20 弱のキーを集約する。
// aria-label / title / プレースホルダ・エラー文言までここに含める。

import { createSignal } from 'solid-js';

export type Lang = 'ja' | 'en';

export const STRINGS = {
  subtitle: {
    ja: '画像から街の光を抽出。配信や動画の背景に。',
    en: 'Extract city lights from any image. Use as a video or stream background.',
  },
  dropZoneLabel: {
    ja: '画像ファイル選択 / ドラッグ&ドロップ',
    en: 'Choose or drop an image',
  },
  dropZonePlaceholder: {
    ja: '画像をドロップ / クリック',
    en: 'Drop or click an image',
  },
  replaceImageHint: { ja: '差し替え', en: 'Replace' },
  pickedThumbAlt: {
    ja: '選択した画像: {name}',
    en: 'Picked image: {name}',
  },
  aspectPortrait: { ja: '縦長', en: 'Portrait' },
  aspectPortraitTitle: {
    ja: '縦長 9:16（プレビュー 360×640 / DL 1080×1920）',
    en: 'Portrait 9:16 (preview 360×640, DL 1080×1920)',
  },
  aspectLandscape: { ja: '横長', en: 'Landscape' },
  aspectLandscapeTitle: {
    ja: '横長 16:9（プレビュー 640×360 / DL 1920×1080）',
    en: 'Landscape 16:9 (preview 640×360, DL 1920×1080)',
  },
  rerollLabel: { ja: '同じ画像でガチャ', en: 'Roll again' },
  rerollTitle: {
    ja: '同じ画像でもう一度ガチャ',
    en: 'Roll again with the same image',
  },
  shapeLabel: { ja: '形状', en: 'Shape' },
  // #174 (二次対応): 'Circle' は orber 専用のぼかし経路で描画される (SDF を
  // 介さず純粋な soft circle gradient)。Glyph の ● は generateJsGlyphSdf を
  // 通る汎用 SDF 経路でエッジが立つ。両者は意図的に挙動が違うため、UI 上は
  // 専用経路であることが伝わる 'Orb' / 'オーブ' に改名する。
  shapeOptionCircle: { ja: 'オーブ', en: 'Orb' },
  shapeOptionGlyph: { ja: '文字', en: 'Glyph' },
  shapeOptionImage: { ja: '画像', en: 'Image' },
  // #160: Image shape (任意の画像をシルエット化して orb として使う) の UI。
  imageShapeLabel: { ja: '画像', en: 'Image' },
  imageShapePick: { ja: '画像を選択', en: 'Choose image' },
  imageShapeLoadFailed: {
    ja: '画像を読み込めませんでした',
    en: 'Failed to load image',
  },
  imageShapePickHint: {
    ja: '画像を選択してください',
    en: 'Pick an image first',
  },
  // #181: imageShapeInvert (#170) はトグルごと削除済み。
  imageShapeNoContrast: {
    ja: 'この画像にはコントラストがありません',
    en: 'This image has no contrast',
  },
  glyphCharLabel: { ja: '文字', en: 'Character' },
  // #159 後は任意の Unicode 1 文字を受け付ける (絵文字 / 漢字 / 記号)。
  // placeholder は文字種の多様性を例示してユーザーに「何でも入る」ことを
  // 伝える。実装詳細 (Noto / 同梱フォント / SDF / OS フォントスタック) は
  // ユーザーには関係しないため、placeholder にも警告にも露出させない。
  glyphCharPlaceholder: { ja: '☆, A, 漢, 🐱, ♪', en: '☆, A, 漢, 🐱, ♪' },
  // #136: Glyph 回転 ON/OFF。雷 ⚡ など回転すると違和感のある記号は既定 OFF にする。
  glyphRotateLabel: {
    ja: '回転させる',
    en: 'Animate rotation',
  },
  countLabel: { ja: '数', en: 'Count' },
  countOptionLow: { ja: '少なめ', en: 'Few' },
  countOptionMid: { ja: '標準', en: 'Standard' },
  countOptionHigh: { ja: '多め', en: 'Many' },
  speedLabel: { ja: '速さ', en: 'Speed' },
  speedOptionSlow: { ja: 'ゆっくり', en: 'Slow' },
  speedOptionMid: { ja: '標準', en: 'Standard' },
  speedOptionFast: { ja: '速め', en: 'Fast' },
  softnessLabel: { ja: 'ぼかし', en: 'Softness' },
  softnessOptionLow: { ja: '弱め', en: 'Low' },
  softnessOptionMid: { ja: '標準', en: 'Standard' },
  softnessOptionHigh: { ja: '強め', en: 'High' },
  wasmLoadFailed: {
    ja: 'wasm の読み込みに失敗しました',
    en: 'Failed to load wasm',
  },
  decoding: { ja: '画像をデコード中…', en: 'Decoding image…' },
  generating: { ja: '生成中…', en: 'Generating…' },
  animating: { ja: '動画化中…', en: 'Animating…' },
  // #124: 生成完了後、進捗行を空白にせず長押し拡大の操作ヒントとして再利用する。
  // 用語は DESIGN.md §4 PreviewOverlay と既存コード（LONG_PRESS_MS / isLongPress）に
  // 合わせて "Long-press" / 「長押し」を採用。"Long tap" は touch を強く示唆するため
  // マウス/トラックパッドでも動く現実装には不適。
  longPressEnlargeHint: {
    ja: '長押しで拡大',
    en: 'Long-press to enlarge',
  },
  // N2: 末尾の "…" は文字列内包に統一する（呼び出し側で `{t('...')}…` と
  // 重ねると i18n が壊れたとき suffix だけ残る事故が起きる）。
  videoPendingBadge: { ja: '動画化中…', en: 'Animating…' },
  animateError: {
    ja: '動画生成に失敗したタイルがあります',
    en: 'Some tiles failed to encode to video',
  },
  // #94: 部分失敗 warning 用。fatal 風に響かないよう「一部のタイルは
  // 静止画のままです」という事実ベースの言い回しに分けてある。
  animatePartialFailure: {
    ja: '一部のタイルは動画化できず、静止画のままです',
    en: 'Some tiles could not be animated and remain as stills',
  },
  downloadSelected: { ja: '選択を DL', en: 'Download selected' },
  downloadAll: { ja: '全 {n} 枚 DL', en: 'Download all {n}' },
  preparingDownload: {
    ja: '高解像度版を準備中… {done} / {total}',
    en: 'Rendering high-res… {done} / {total}',
  },
  downloadFailed: {
    ja: 'ダウンロード準備に失敗しました',
    en: 'Failed to prepare download',
  },
  // #56: ZIP DL に透過版 (PNG / WebP / WebM VP9 alpha) を同梱するかの checkbox。
  // 既定 OFF（既存 byte-exact 出力を保つため）。disabled tooltip は Safari など
  // VP9 alpha encoding 非対応ブラウザでのフォールバック用。
  includeAlphaLabel: {
    ja: '透過版を DL に含める',
    en: 'Include transparent versions',
  },
  includeAlphaDisabledTitle: {
    ja: 'お使いのブラウザは透過動画 (WebM VP9 alpha) のエンコードに対応していません',
    en: "This browser cannot encode transparent video (WebM VP9 alpha)",
  },
  variationAlt: { ja: 'バリエーション {n}', en: 'Variation {n}' },
  variationAnimatedAlt: {
    ja: 'バリエーション {n} (動画)',
    en: 'Variation {n} (animated)',
  },
  // #128 / #156: Footer (Amazon affiliate / QR / Copyright / Counter)。
  // sponsorLabel / sponsorTitle は #156 で大きい GH Sponsors button を削除して
  // 以来未使用なので削除済み。authorSiteLabel / sponsorTextLabel も #174 で
  // テキストリンク → アイコン化に伴い未使用になり削除済み (置換キー:
  // authorSiteAriaLabel / repoLinkAriaLabel / sponsorAriaLabel)。
  //
  // affiliate 文言:
  //   - User 「本やゲームは機材じゃない」「Amazon と 2 回言う必要はない」
  //     「サイト維持のため、購入を検討くださいみたいな表現に」「文章が長くて改行されてしまう」
  //   - heading: 「機材」だと書籍 / ゲームに合わないので「kako-jun のおすすめ」に
  //   - disclosure: 1 行に収まる短さに圧縮。Amazon Associate の開示は維持
  //     (en は FTC 推奨テンプレ、ja は短文 + Associate キーワードを残す)
  // #174: Footer 内の他要素 (© kako-jun / アイコンリンク) で著者は自明なため、
  // 'kako-jun の' / "kako-jun's" は冗長として削除。
  affiliateHeading: {
    ja: 'おすすめ',
    en: 'Picks',
  },
  affiliateDisclosure: {
    ja: '購入が orber 維持の支援になります (Amazon アソシエイト)',
    en: 'As an Amazon Associate I earn from qualifying purchases.',
  },
  // #146: QR の補助コピー (qrLabel / "Open on phone") は廃止。alt のみ残す。
  qrAlt: {
    ja: 'orber.llll-ll.com を開く QR コード',
    en: 'QR code to open orber.llll-ll.com',
  },
  privacyNote: {
    ja: '画像はブラウザ内で処理されます。サーバーへの送信はありません。',
    en: 'All processing happens in your browser — no images leave your device.',
  },
  // #128 / #146: ja は「閲覧数: {n}」、en は「{n} views」で語順を言語ごとに分ける。
  // {n} は <nostalgic-counter> がレンダリングする数値で置換する。
  viewsLabelPrefix: { ja: '閲覧数: ', en: '' },
  viewsLabelSuffix: { ja: '', en: ' views' },
  // #146 review S2: Footer 全体の aria-label を i18n 化。
  footerAriaLabel: { ja: 'orber フッター', en: 'orber footer' },
  // Footer 末尾の小さい link 行。#174 でテキストリンクからアイコン並びに変更
  // (osaka-kenpo / sasso / agasteer と統一)。© kako-jun は whitespace-nowrap で
  // ハイフンを跨ぐ改行を防ぐ (旧: `kako-` で改行され `jun` だけ次行に落ちる)。
  // 各アイコンリンクの aria-label / title。
  authorSiteAriaLabel: { ja: 'kako-jun のサイト', en: "kako-jun's site" },
  repoLinkAriaLabel: { ja: 'GitHub リポジトリ', en: 'GitHub repository' },
  sponsorAriaLabel: { ja: 'GitHub Sponsors', en: 'GitHub Sponsors' },
  // #146: About 見出し / aboutBody / aboutBuiltWith / repoLinkLabel は Footer から外した。
  // privacyNote だけ「画像はブラウザ内で処理される」境界条件として残す。
  // #148: PWA install prompt (machigai-salad と同パターンの toast)。
  installPromptBody: {
    ja: 'orber をホーム画面に追加できます',
    en: 'Install orber on your device',
  },
  installBtn: { ja: 'インストール', en: 'Install' },
  installDismiss: { ja: '閉じる', en: 'Dismiss' },
} as const;

export function detectLang(): Lang {
  // SSR 検出は window の有無で行う。Node 22+ は global navigator を持つため
  // navigator では SSR を判別できない (ビルドホストの $LANG が漏れて SSR HTML
  // が誤った言語で出力される事故が起きる)。window は SSR 環境では未定義。
  if (typeof window === 'undefined') return 'en';
  const nav = window.navigator;
  if (!nav || typeof nav.language !== 'string') return 'en';
  return nav.language.toLowerCase().startsWith('ja') ? 'ja' : 'en';
}

// orber#161 — Solid hydration mismatch 対策。
// 以前はモジュール init 時に detectLang() を呼んで signal を初期化していたが、
// それだと SSR 評価値 ('en') と client 評価値 ('ja') が食い違い、Solid の
// hydration が「DOM 既存 → 再レンダリングしない」最適化に引っかかって
// SSR の EN テキストが残留する島が発生していた (一部だけ JP、他は EN という
// 混在状態)。
//
// 対策: signal は SSR と同じ 'en' で初期化し、hydration 完了後の microtask で
// setLang(detectLang()) を呼んで reactive 再評価をトリガする。これにより
// 全 t() 呼び出しが (各島で) 同時に再評価され、言語が確実に揃う。
//
// 各 Solid island が同一モジュールチャンクを共有するか別かは Vite の chunk
// 分割に依る。共有なら 1 度の setLang で全島更新、別なら各島の microtask が
// それぞれ自島の signal を更新するため、どちらの構成でも結果は同じ。
const [lang, setLang] = createSignal<Lang>('en');
if (typeof window !== 'undefined') {
  // microtask は hydration 用 top-level コードの直後・Subtitle.tsx の onMount より
  // 前に走ることを期待。`createSignal` はデフォルトで `===` 等値ガード付きなので
  // en ブラウザで `setLang('en')` を呼んでも no-op (購読側 effect は再走しない)。
  queueMicrotask(() => setLang(detectLang()));
}
export { lang, setLang };

export type StringKey = keyof typeof STRINGS;

export function t<K extends StringKey>(
  key: K,
  vars?: Record<string, string | number>,
): string {
  let s: string = STRINGS[key][lang()];
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}
