// orber#232 — A/B 検証足場の純粋ロジック。
//
// ★ Phase 3 で AbPanel.tsx と一緒に削除する検証足場 ★
//   AbPanel.tsx のクロージャに埋まっていた純粋ロジック（開始ガード条件と
//   両レンダラ共通 params の組立て）を、単体テスト可能にするためここへ切り出した。
//   AbPanel.tsx はこのモジュールを import して使う。挙動は元の .tsx 実装と
//   1 ビットも変えていない（純粋な移動）。Phase 3 で WebGL を撤去するとき、
//   AbPanel.tsx / lib/webgpu.ts / strings.ts の ab* キーごと、このファイルも削除する。

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
