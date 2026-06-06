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
  segToggleDisabled,
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

  it('S3: 非正方（4×2）は行優先（Rust 側ピンと同一バイト列）', () => {
    // SYNC WITH crates/wasm/src/ab_harness.rs の synthetic_source_non_square_is_row_major
    // テスト。期待バイトは両側で同一値。width/height の取り違え（列優先化）を
    // 双方向で防ぐ。
    const rgb = buildSyntheticSourceRgb(4, 2);
    expect(Array.from(rgb)).toEqual([
      0, 0, 0, 7, 11, 13, 14, 22, 26, 21, 33, 39, // y=0: x=0..3
      13, 5, 7, 20, 16, 20, 27, 27, 33, 34, 38, 46, // y=1: x=0..3
    ]);
  });

  it('S4: 幅または高さが 0 なら長さ 0（縮退ピン）', () => {
    expect(buildSyntheticSourceRgb(0, 5).length).toBe(0);
    expect(buildSyntheticSourceRgb(5, 0).length).toBe(0);
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

  it('M2: 未知フィールド（バイナリ 3 種以外）はそのまま透過する', () => {
    // Rust 側ハーネス（AbParams）は未知フィールドを serde 既定で無視するので、
    // web 側で params が増えても落とさず素通しして良い（前方互換の対）。
    const params = {
      source_rgb: new Uint8Array([1]),
      width: 270,
      aquarelle_bleed: 0.7,
      some_future_field: 'x',
    };
    const meta = buildAbCaptureMeta(params, 12, 8, 0);
    expect(meta.aquarelle_bleed).toBe(0.7);
    expect(meta.some_future_field).toBe('x');
    expect(meta.source_rgb).toBeUndefined();
  });

  it('M3: params に既存の n / spec_idx / t キーがあっても引数が勝つ', () => {
    // ループで params を写してから引数を代入する順序のピン。逆順になると
    // 「実際に描画した n/spec_idx/t」と違うメタが落ちて CLI 再現が狂う。
    const params = { width: 270, n: 99, spec_idx: 99, t: 0.9 };
    const meta = buildAbCaptureMeta(params, 12, 8, 0);
    expect(meta.n).toBe(12);
    expect(meta.spec_idx).toBe(8);
    expect(meta.t).toBe(0);
  });

  it('M4: 入力 params オブジェクトを破壊しない（読み取り専用）', () => {
    const sourceRgb = new Uint8Array([1, 2, 3]);
    const params: Record<string, unknown> = {
      source_rgb: sourceRgb,
      width: 270,
      n: 99,
    };
    buildAbCaptureMeta(params, 12, 8, 0);
    // キーの増減・上書きが無いこと（source_rgb も params 側には残る）。
    expect(Object.keys(params).sort()).toEqual(['n', 'source_rgb', 'width']);
    expect(params.source_rgb).toBe(sourceRgb);
    expect(params.width).toBe(270);
    expect(params.n).toBe(99);
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

  it('G3: 空配列 → true（縮退ピン: 画素ゼロは「何も拾えていない」= 失敗扱い）', () => {
    expect(isAllBlackOrTransparent(new Uint8ClampedArray([]))).toBe(true);
  });

  it('G4: 半透明黒 [0,0,0,1] → true（黒判定は A を見ない = 安全側に広く失敗検出）', () => {
    // docstring どおり「RGB=0 なら A 不問で黒扱い」。失敗検出を広めに取る意図的な
    // 実装で、A=255 限定に狭めると半透明黒のキャプチャ失敗を見逃す。
    expect(isAllBlackOrTransparent(new Uint8ClampedArray([0, 0, 0, 1]))).toBe(true);
  });

  it('G5: 透明優先 [1,0,0,0] → true / 不透明な非黒 [1,0,0,1] → false', () => {
    // A=0 なら RGB が何であっても透明として失敗側、A>0 で RGB≠0 なら正常画素。
    expect(isAllBlackOrTransparent(new Uint8ClampedArray([1, 0, 0, 0]))).toBe(true);
    expect(isAllBlackOrTransparent(new Uint8ClampedArray([1, 0, 0, 1]))).toBe(false);
  });
});

// ---- #242 E1: segmented toggle の disabled 条件（AbPanel から純移動） --------
//
// AbPanel.tsx の JSX 式と同値であることを全組合せで固定する（DT-2 の真理値表）。

describe('segToggleDisabled()（A/B segmented toggle の disabled 条件）', () => {
  const bools = [false, true];

  it('T1: captureMode=false は元式と同値（webgl: !running / wgsl: !running || !webgpuOk）', () => {
    for (const running of bools)
      for (const captured of bools)
        for (const webgpuOk of bools) {
          expect(segToggleDisabled('webgl', false, captured, running, webgpuOk)).toBe(!running);
          expect(segToggleDisabled('wgsl', false, captured, running, webgpuOk)).toBe(
            !running || !webgpuOk,
          );
        }
  });

  it('T2: captureMode=true は !captured ベース（wgsl は || !webgpuOk）', () => {
    for (const running of bools)
      for (const captured of bools)
        for (const webgpuOk of bools) {
          expect(segToggleDisabled('webgl', true, captured, running, webgpuOk)).toBe(!captured);
          expect(segToggleDisabled('wgsl', true, captured, running, webgpuOk)).toBe(
            !captured || !webgpuOk,
          );
        }
  });
});
