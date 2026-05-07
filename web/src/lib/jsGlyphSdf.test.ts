// orber#159 — jsGlyphSdf の回帰テスト。
//
// jsdom には OffscreenCanvas が無いため `typeof OffscreenCanvas === 'undefined'`
// 経路と、`OffscreenCanvas` を最低限 stub したときの SDF 出力フォーマットを
// 押さえる。実 OS のフォントレンダリングまでは jsdom で再現できないので、
// 「描画ピクセルがあれば SDF が non-trivial」「無ければ全 0」の 2 パターンに
// 絞る。

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

import { GLYPH_SDF_MAX_DIST_FACTOR, generateJsGlyphSdf } from './jsGlyphSdf';

describe('generateJsGlyphSdf()', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  test('OffscreenCanvas 未定義環境では全 0 の Uint8Array(size*size) を返す', () => {
    vi.stubGlobal('OffscreenCanvas', undefined);
    const out = generateJsGlyphSdf('A', 16);
    expect(out).toBeInstanceOf(Uint8Array);
    expect(out.length).toBe(16 * 16);
    expect(out.every((v) => v === 0)).toBe(true);
  });

  test('alpha が全 0 の (= 描画されない) 文字なら全 0 を返す', () => {
    // alpha チャンネルが全部 0 を返す ImageData stub を OffscreenCanvas に注入。
    class StubCanvas {
      width: number;
      height: number;
      constructor(w: number, h: number) {
        this.width = w;
        this.height = h;
      }
      getContext() {
        return {
          clearRect: () => {},
          fillText: () => {},
          getImageData: (x: number, y: number, w: number, h: number) => ({
            data: new Uint8ClampedArray(w * h * 4), // 全 0 → alpha 全 0
            width: w,
            height: h,
          }),
          set font(_v: string) {},
          set textAlign(_v: string) {},
          set textBaseline(_v: string) {},
          set fillStyle(_v: string) {},
        };
      }
    }
    vi.stubGlobal('OffscreenCanvas', StubCanvas);
    const out = generateJsGlyphSdf('🐱', 8);
    expect(out.length).toBe(64);
    expect(out.every((v) => v === 0)).toBe(true);
  });

  test('synthetic 1 ピクセル inside で EDT 距離が Rust 公式と一致する', () => {
    // 8×8 で (3,3) だけ alpha=255 の入力。
    //   inside[3,3] = 1、それ以外 outside。
    //   distOutside[3,3] = 1 (最寄り outside は (2,3)/(3,2)/(3,4)/(4,3) のどれか)
    //   distInside[3,2] = 1、distInside[2,2] = 2、distInside[0,0] = 18、…
    //
    // Rust 公式 (`crates/core/src/glyph.rs:295`) と完全一致する byte が出るかを
    // 直接検査する。size=8 のとき norm は `(size * 0.06).max(1.0)` で 1.0 に
    // 固定 (Rust 側と同じ floor)。よって signed_unit = signed_px がそのまま入り、
    // 各セルで以下の値が期待される:
    //   (3,3) inside: signed_px = sqrt(1) - 0.5 = +0.5  → byte = 191
    //   (3,2) outside dist²=1: signed_px = 0.5 - 1 = -0.5 → byte =  64
    //   (2,2) outside dist²=2: signed_px = 0.5 - √2 ≈ -0.914 → byte = 11
    //   (0,0) outside dist²=18: signed_px ≈ -3.74 → clamp(-1) → byte = 0
    const SIZE = 8;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    data[(3 * SIZE + 3) * 4 + 3] = 255;
    class StubCanvas {
      width: number;
      height: number;
      constructor(w: number, h: number) {
        this.width = w;
        this.height = h;
      }
      getContext() {
        return {
          clearRect: () => {},
          fillText: () => {},
          getImageData: () => ({ data, width: SIZE, height: SIZE }),
          set font(_v: string) {},
          set textAlign(_v: string) {},
          set textBaseline(_v: string) {},
          set fillStyle(_v: string) {},
        };
      }
    }
    vi.stubGlobal('OffscreenCanvas', StubCanvas);
    const out = generateJsGlyphSdf('A', SIZE);
    expect(out.length).toBe(SIZE * SIZE);
    // 内側 1 セル
    expect(out[3 * SIZE + 3]).toBe(191);
    // 上下左右の隣接 outside (4-way) は対称: dist²=1 → byte=64
    expect(out[3 * SIZE + 2]).toBe(64);
    expect(out[3 * SIZE + 4]).toBe(64);
    expect(out[2 * SIZE + 3]).toBe(64);
    expect(out[4 * SIZE + 3]).toBe(64);
    // 対角 (2,2): dist²=2 → byte=11
    expect(out[2 * SIZE + 2]).toBe(11);
    // 端 (0,0): clamp -1 → byte=0
    expect(out[0]).toBe(0);
  });

  test('GLYPH_SDF_MAX_DIST_FACTOR は Rust 側 (crates/core/src/glyph.rs) の 0.06 と同値', () => {
    // 値変更時の同期忘れを防ぐ guard。Rust 側を変えたら同じ値にする。
    expect(GLYPH_SDF_MAX_DIST_FACTOR).toBe(0.06);
  });
});
