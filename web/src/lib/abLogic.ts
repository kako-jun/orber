// orber#232 — A/B 検証足場の純粋ロジック。
//
// ★ Phase 3 で AbPanel.tsx と一緒に削除する検証足場 ★
//   AbPanel.tsx のクロージャに埋まっていた純粋ロジック（開始ガード条件と
//   両レンダラ共通 params の組立て）を、単体テスト可能にするためここへ切り出した。
//   AbPanel.tsx はこのモジュールを import して使う。挙動は元の .tsx 実装と
//   1 ビットも変えていない（純粋な移動）。Phase 3 で WebGL を撤去するとき、
//   AbPanel.tsx / lib/webgpu.ts / strings.ts の ab* キーごと、このファイルも削除する。
//
// #242 で三者画素比較（CLI / WGSL / WebGL）のキャプチャモード用ロジック
// （合成ソース生成・params メタ組立・黒画面ガード）を追加。これも Phase 3 で
// 本ファイルごと削除する。

import type { DecodedImage } from './decodeImage';

export type ShapeChoice = 'orb' | 'glyph' | 'image';

// gpu-lab と同じ canvas サイズ / VerySlow 1cycle 周期。
export const CANVAS_W = 270;
export const CANVAS_H = 480;
export const PERIOD_MS = 8000;
// 固定 seed（再現性優先・コメントで明記）。Studio 本番経路は毎回乱数 seed を
// 引くが、A/B 比較は「同じ入力で新旧の見た目が一致するか」を見るので固定する。
export const AB_SEED = 42;
// gpu-lab と同じ video spec（direction=LR / speed=VerySlow の固定割当先頭）。
export const AB_N = 12;
export const AB_SPEC_IDX = 8;

// ---- #242 キャプチャモード（?ab=1&abcap=1）の純粋ロジック -----------------
//
// 三者画素比較（CLI / ブラウザ WGSL / ブラウザ WebGL）の足場。ファイル選択や
// ブラウザの decode 差を排除するため、JS の整数式から決定的に合成した RGB
// ソースを使う。Rust 側ハーネス（crates/wasm/src/ab_harness.rs の
// `synthetic_source_rgb`）と**完全に同一の式**で、ab-source.bin が無くても
// CLI 単独で同じソースを再現できる。
//
// 合成式（x, y は 0 始まりの画素座標、% は非負剰余）:
//   r = (x * 7  + y * 13) % 256
//   g = (x * 11 + y * 5)  % 256
//   b = (x * 13 + y * 7)  % 256
// SYNC WITH crates/wasm/src/ab_harness.rs::synthetic_source_rgb
export const AB_CAPTURE_SOURCE_W = 96;
export const AB_CAPTURE_SOURCE_H = 96;

// 決定的な合成 RGB ソースを生成する（行優先 R,G,B,...）。
export function buildSyntheticSourceRgb(width: number, height: number): Uint8Array {
  const rgb = new Uint8Array(width * height * 3);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 3;
      rgb[i] = (x * 7 + y * 13) % 256;
      rgb[i + 1] = (x * 11 + y * 5) % 256;
      rgb[i + 2] = (x * 13 + y * 7) % 256;
    }
  }
  return rgb;
}

// ab-params.json の中身を組む: source_rgb 等のバイナリ列を除いた全 params +
// n + spec_idx + t。バイナリ（source_rgb / image_mask_rgba / glyph_sdf）は
// JSON に入れず、source_rgb は ab-source.bin として別ファイルで落とす
// （image_mask / glyph_sdf はハーネス対象外 = orb ゲート専用なので落とさない）。
// Rust 側ハーネス（ab_harness.rs::AbParams）はこの JSON をそのまま読む。
export function buildAbCaptureMeta(
  params: Record<string, unknown>,
  n: number,
  specIdx: number,
  t: number,
): Record<string, unknown> {
  const meta: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(params)) {
    if (key === 'source_rgb' || key === 'image_mask_rgba' || key === 'glyph_sdf') continue;
    meta[key] = value;
  }
  meta.n = n;
  meta.spec_idx = specIdx;
  meta.t = t;
  return meta;
}

// キャプチャ画像の全画素が黒（RGB=0。A は不問 = 半透明黒も黒扱い）または
// 全透明（A=0）かを判定する。実装は安全側（失敗検出を広めに取る）: A を見ない
// ぶん「ほぼ黒い正常出力」も失敗扱いになり得るが、dev 足場の誤検知として許容する。
// toBlob / drawImage がレンダ結果を拾えなかった失敗を無言で成功扱い
// しないためのガード（WebGPU canvas は同一タスク外で snapshot すると空になる）。
export function isAllBlackOrTransparent(data: Uint8ClampedArray): boolean {
  for (let i = 0; i < data.length; i += 4) {
    const opaqueBlack = data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 0;
    const transparent = data[i + 3] === 0;
    if (!opaqueBlack && !transparent) return false;
  }
  return true;
}

// #242: A/B canvas の WebGL / WGSL segmented toggle の disabled 条件（純関数）。
// AbPanel.tsx の JSX 式から 1:1 で切り出した（挙動は 1 ビットも変えない純移動。
// abCanStart と同じ前例）:
//   - 通常モード（captureMode=false）: blink 実行中だけ有効 = !running で disabled
//   - キャプチャモード（captureMode=true）: キャプチャ成功後（両 canvas に t=0 が
//     残った状態）だけ有効 = !captured で disabled
//   - WGSL 側はさらに WebGPU 非対応ブラウザで常に disabled（|| !webgpuOk）
export function segToggleDisabled(
  side: 'webgl' | 'wgsl',
  captureMode: boolean,
  captured: boolean,
  running: boolean,
  webgpuOk: boolean,
): boolean {
  const base = captureMode ? !captured : !running;
  return side === 'wgsl' ? base || !webgpuOk : base;
}

// 開始可否のガード条件。source（decoded 画像）が必須で、image shape のときは
// 追加で元 File が必須（image_mask が組めないため）。
//   - hasSource: props.decoded() !== null 相当
//   - hasImageFile: props.imageShapeFile() !== null 相当
export function abCanStart(
  shape: ShapeChoice,
  hasSource: boolean,
  hasImageFile: boolean,
): boolean {
  return hasSource && !(shape === 'image' && !hasImageFile);
}

// 現在の Studio 状態から両レンダラ共通の params を組む。
// shape 依存の追加フィールド（image_mask / glyph_sdf）は呼び出し側で足す。
export function buildAbBaseParams(
  src: DecodedImage,
  shape: ShapeChoice,
  glyphChar: string,
  glyphRotate: boolean,
  countPreset: string,
  speedPreset: string,
  softnessPreset: string,
): Record<string, unknown> {
  return {
    source_rgb: src.rgb,
    source_width: src.width,
    source_height: src.height,
    k: 5,
    width: CANVAS_W,
    height: CANVAS_H,
    seed: AB_SEED,
    // direction/speed/count/orb_size/blur は spec で上書きされるが必須なので送る。
    direction: 'lr',
    speed: 'slow',
    count: 20,
    orb_size: 3.0,
    blur: 0.5,
    shape,
    glyph_char: shape === 'glyph' ? glyphChar : '',
    glyph_rotate: glyphRotate,
    count_preset: countPreset,
    speed_preset: speedPreset,
    softness_preset: softnessPreset,
  };
}
