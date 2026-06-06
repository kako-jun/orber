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
  buildAbCaptureMeta,
  buildSyntheticSourceRgb,
  isAllBlackOrTransparent,
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

// ---- #242 キャプチャモードの純粋ロジック ----------------------------------

describe('buildSyntheticSourceRgb()（#242 Rust ab_harness と SYNC）', () => {
  it('S1: 既知座標のバイトを固定する（Rust synthetic_source_rgb と同値であること）', () => {
    // SYNC WITH crates/wasm/src/ab_harness.rs の synthetic_source_pins_known_bytes
    // テスト。式を変えるときは両側のピンを同時に更新する。
    const rgb = buildSyntheticSourceRgb(96, 96);
    expect(rgb.length).toBe(96 * 96 * 3);
    // (x=0, y=0)
    expect([rgb[0], rgb[1], rgb[2]]).toEqual([0, 0, 0]);
    // (x=1, y=0): r=7, g=11, b=13
    expect([rgb[3], rgb[4], rgb[5]]).toEqual([7, 11, 13]);
    // (x=0, y=1): r=13, g=5, b=7
    const row1 = 96 * 3;
    expect([rgb[row1], rgb[row1 + 1], rgb[row1 + 2]]).toEqual([13, 5, 7]);
    // (x=95, y=95): r=(95*7+95*13)%256=108, g=(95*11+95*5)%256=240, b=108
    const last = (95 * 96 + 95) * 3;
    expect([rgb[last], rgb[last + 1], rgb[last + 2]]).toEqual([108, 240, 108]);
  });

  it('S2: 決定的（同じ引数なら同じバイト列）', () => {
    expect(buildSyntheticSourceRgb(8, 8)).toEqual(buildSyntheticSourceRgb(8, 8));
  });
});

describe('buildAbCaptureMeta()（#242 ab-params.json の組立）', () => {
  it('M1: バイナリ（source_rgb / image_mask_rgba / glyph_sdf）を除外し n/spec_idx/t を付与', () => {
    const params = {
      source_rgb: new Uint8Array([1, 2, 3]),
      image_mask_rgba: new Uint8Array([4]),
      glyph_sdf: new Uint8Array([5]),
      width: 270,
      height: 480,
      seed: 42,
      shape: 'orb',
    };
    const meta = buildAbCaptureMeta(params, 12, 8, 0);
    expect(meta.source_rgb).toBeUndefined();
    expect(meta.image_mask_rgba).toBeUndefined();
    expect(meta.glyph_sdf).toBeUndefined();
    expect(meta.width).toBe(270);
    expect(meta.height).toBe(480);
    expect(meta.seed).toBe(42);
    expect(meta.shape).toBe('orb');
    expect(meta.n).toBe(12);
    expect(meta.spec_idx).toBe(8);
    expect(meta.t).toBe(0);
  });
});

describe('isAllBlackOrTransparent()（#242 キャプチャ失敗ガード）', () => {
  it('G1: 全画素 不透明黒 → true / 全透明 → true / 混在も true', () => {
    // 不透明黒 + 全透明（RGB は何でも良い）の混在は「失敗扱い」のまま。
    const black = new Uint8ClampedArray([0, 0, 0, 255, 0, 0, 0, 255]);
    const transparent = new Uint8ClampedArray([9, 9, 9, 0, 1, 2, 3, 0]);
    const mixed = new Uint8ClampedArray([0, 0, 0, 255, 7, 7, 7, 0]);
    expect(isAllBlackOrTransparent(black)).toBe(true);
    expect(isAllBlackOrTransparent(transparent)).toBe(true);
    expect(isAllBlackOrTransparent(mixed)).toBe(true);
  });

  it('G2: 1 画素でも非黒・非透明があれば false（正常キャプチャ）', () => {
    const lit = new Uint8ClampedArray([0, 0, 0, 255, 10, 0, 0, 255]);
    expect(isAllBlackOrTransparent(lit)).toBe(false);
  });
});
