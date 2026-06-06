// orber#232 — abLogic.ts（A/B 検証足場の純粋ロジック）の単体テスト。
//
// AbPanel.tsx のクロージャから切り出した 2 関数を押さえる:
//   - abCanStart(): 開始ガードの真理値表（source 必須・image shape は file も必須の
//     否定入れ子が核心）
//   - buildAbBaseParams(): 両レンダラ共通 params の組立て（固定 seed/サイズ等の定数、
//     shape 依存の glyph_char 透過/空、プリセットの透過）
//
// 検証足場なので Phase 3 で AbPanel.tsx と一緒に削除する。

import { describe, expect, it } from 'vitest';
import type { DecodedImage } from './decodeImage';
import {
  abCanStart,
  buildAbBaseParams,
  AB_SEED,
  CANVAS_W,
  CANVAS_H,
} from './abLogic';

// ---- abCanStart() 真理値表 ----------------------------------------------
// 元実装: hasSource && !(shape === 'image' && imageFile === null)
//   = abCanStart(shape, hasSource, hasImageFile)
//   = hasSource && !(shape === 'image' && !hasImageFile)

describe('abCanStart() 真理値表', () => {
  it('C1: orb + source あり → true', () => {
    expect(abCanStart('orb', true, false)).toBe(true);
  });

  it('C2: orb + source なし → false（source は常に必須）', () => {
    expect(abCanStart('orb', false, false)).toBe(false);
  });

  it('C3: glyph + source あり → true（glyph は file 不要）', () => {
    expect(abCanStart('glyph', true, false)).toBe(true);
  });

  it('C4: image + source + file あり → true', () => {
    expect(abCanStart('image', true, true)).toBe(true);
  });

  it('C5: image + source あり + file なし → false（否定入れ子の核心）', () => {
    // image shape だけは file が無いと image_mask が組めず開始不可。
    expect(abCanStart('image', true, false)).toBe(false);
  });

  it('C6: image + source なし + file あり → false（source 欠落が優先で false）', () => {
    expect(abCanStart('image', false, true)).toBe(false);
  });
});

// ---- buildAbBaseParams() -------------------------------------------------

const SRC: DecodedImage = {
  rgb: new Uint8Array([1, 2, 3, 4, 5, 6]),
  width: 2,
  height: 1,
};

// 引数順: src, shape, glyphChar, glyphRotate, countPreset, speedPreset, softnessPreset
function build(
  shape: 'orb' | 'glyph' | 'image',
  glyphChar = 'X',
  glyphRotate = false,
  count = 'mid',
  speed = 'mid',
  softness = 'mid',
) {
  return buildAbBaseParams(SRC, shape, glyphChar, glyphRotate, count, speed, softness);
}

describe('buildAbBaseParams() 固定値', () => {
  it('P1: seed / width / height / k 等が export 定数と一致した固定値', () => {
    const p = build('orb');
    expect(p.seed).toBe(AB_SEED);
    expect(p.seed).toBe(42);
    expect(p.width).toBe(CANVAS_W);
    expect(p.width).toBe(270);
    expect(p.height).toBe(CANVAS_H);
    expect(p.height).toBe(480);
    expect(p.k).toBe(5);
    // spec で上書きされる必須フィールドの既定値も固定で送る。
    expect(p.direction).toBe('lr');
    expect(p.speed).toBe('slow');
    expect(p.count).toBe(20);
    expect(p.orb_size).toBe(3.0);
    expect(p.blur).toBe(0.5);
    // source は src からそのまま透過。
    expect(p.source_rgb).toBe(SRC.rgb);
    expect(p.source_width).toBe(2);
    expect(p.source_height).toBe(1);
  });
});

describe('buildAbBaseParams() glyph_char の shape 依存', () => {
  it('P2: shape !== glyph なら glyph_char は空文字（orb / image で透過しない）', () => {
    expect(build('orb', '★').glyph_char).toBe('');
    expect(build('image', '★').glyph_char).toBe('');
  });

  it('P3: shape === glyph なら glyph_char がそのまま透過', () => {
    expect(build('glyph', '★').glyph_char).toBe('★');
    expect(build('glyph', '🐱').glyph_char).toBe('🐱');
  });
});

describe('buildAbBaseParams() プリセット透過', () => {
  it('P4: count/speed/softness プリセットと glyph_rotate がそのまま透過', () => {
    const p = build('glyph', 'A', true, 'high', 'low', 'mid');
    expect(p.count_preset).toBe('high');
    expect(p.speed_preset).toBe('low');
    expect(p.softness_preset).toBe('mid');
    expect(p.glyph_rotate).toBe(true);
  });
});
