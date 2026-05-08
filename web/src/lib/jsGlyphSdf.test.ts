// orber#159 — jsGlyphSdf の回帰テスト。
//
// jsdom には OffscreenCanvas が無いため `typeof OffscreenCanvas === 'undefined'`
// 経路と、`OffscreenCanvas` を最低限 stub したときの SDF 出力フォーマットを
// 押さえる。実 OS のフォントレンダリングまでは jsdom で再現できないので、
// 「描画ピクセルがあれば SDF が non-trivial」「無ければ全 0」の 2 パターンに
// 絞る。

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

import {
  GLYPH_SDF_MAX_DIST_FACTOR,
  generateImageSdf,
  generateJsGlyphSdf,
} from './jsGlyphSdf';

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

describe('generateImageSdf()', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  // 画像 → SDF パイプの出力フォーマット検証。jsdom で実画像はデコードできないが、
  // ImageBitmap-like / OffscreenCanvas-like の stub を渡すことで内部 EDT 経路を
  // 通せる。

  // 共通 stub
  const makeStubCanvas = (data: Uint8ClampedArray, size: number) => {
    return class StubCanvas {
      width: number;
      height: number;
      constructor(w: number, h: number) {
        this.width = w;
        this.height = h;
      }
      getContext() {
        return {
          clearRect: () => {},
          drawImage: () => {},
          getImageData: () => ({ data, width: size, height: size }),
        };
      }
    };
  };

  test('OffscreenCanvas 未定義環境では ok=false で全 0 を返す', () => {
    vi.stubGlobal('OffscreenCanvas', undefined);
    const fakeBitmap = { width: 16, height: 16 } as unknown as ImageBitmap;
    const r = generateImageSdf(fakeBitmap, 16);
    expect(r.ok).toBe(false);
    expect(r.sdf.length).toBe(16 * 16);
    expect(r.sdf.every((v) => v === 0)).toBe(true);
  });

  test('完全透過な画像 (alpha 全 0) は ok=false', () => {
    const SIZE = 8;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    vi.stubGlobal('OffscreenCanvas', makeStubCanvas(data, SIZE));
    const fakeBitmap = { width: SIZE, height: SIZE } as unknown as ImageBitmap;
    const r = generateImageSdf(fakeBitmap, SIZE);
    expect(r.ok).toBe(false);
    expect(r.sdf.every((v) => v === 0)).toBe(true);
  });

  test('透過画像 (alpha=255 中央 1px、他 alpha=0) は alpha 経路で文字版と同じ SDF を生成', () => {
    // alpha<255 のピクセルが 63/64 = 98% > 1% → alpha 経路 (#171)。
    // generateJsGlyphSdf と同じ EDT で同じ byte が出る (regression guard)。
    const SIZE = 8;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    data[(3 * SIZE + 3) * 4 + 3] = 255;
    vi.stubGlobal('OffscreenCanvas', makeStubCanvas(data, SIZE));
    const fakeBitmap = { width: SIZE, height: SIZE } as unknown as ImageBitmap;
    const r = generateImageSdf(fakeBitmap, SIZE);
    expect(r.ok).toBe(true);
    expect(r.sdf[3 * SIZE + 3]).toBe(191);
    expect(r.sdf[3 * SIZE + 2]).toBe(64);
    expect(r.sdf[2 * SIZE + 2]).toBe(11);
    expect(r.sdf[0]).toBe(0);
  });

  test('不透明画像でも輝度しきい値で二値化される', () => {
    // 全ピクセル alpha=255、中央 1 px だけ白 (輝度高)、他は黒。
    // alphaPixelCount=0 → 不透明経路 → 少数派 (中央 1px) が inside。
    const SIZE = 8;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    for (let i = 0; i < SIZE * SIZE; i++) data[i * 4 + 3] = 255;
    data[(3 * SIZE + 3) * 4 + 0] = 255;
    data[(3 * SIZE + 3) * 4 + 1] = 255;
    data[(3 * SIZE + 3) * 4 + 2] = 255;
    vi.stubGlobal('OffscreenCanvas', makeStubCanvas(data, SIZE));
    const fakeBitmap = { width: SIZE, height: SIZE } as unknown as ImageBitmap;
    const r = generateImageSdf(fakeBitmap, SIZE);
    expect(r.ok).toBe(true);
    expect(r.sdf[3 * SIZE + 3]).toBe(191);
    expect(r.sdf[3 * SIZE + 2]).toBe(64);
  });

  test('#169 単色塗り画像 (全画素同輝度) は ok=false でコントラスト不足を通知', () => {
    // 全画素 RGB=(128,128,128), alpha=255 → 平均輝度=128、全画素が avgY と
    // 同値で「未満」が 0 → insideIsDark=true、`y < avgY` は false 全部 →
    // insideCount=0 → ok=false。
    const SIZE = 8;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    for (let i = 0; i < SIZE * SIZE; i++) {
      data[i * 4] = 128;
      data[i * 4 + 1] = 128;
      data[i * 4 + 2] = 128;
      data[i * 4 + 3] = 255;
    }
    vi.stubGlobal('OffscreenCanvas', makeStubCanvas(data, SIZE));
    const fakeBitmap = { width: SIZE, height: SIZE } as unknown as ImageBitmap;
    const r = generateImageSdf(fakeBitmap, SIZE);
    expect(r.ok).toBe(false);
    expect(r.sdf.every((v) => v === 0)).toBe(true);
  });

  test('#170 invert=true で inside/outside が反転する', () => {
    // 不透明・中央 1px 白 (= 自動判定で inside=中央)。invert=true で
    // 中央以外が inside になり、SDF byte 値が大きく変わる。
    const SIZE = 8;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    for (let i = 0; i < SIZE * SIZE; i++) data[i * 4 + 3] = 255;
    data[(3 * SIZE + 3) * 4 + 0] = 255;
    data[(3 * SIZE + 3) * 4 + 1] = 255;
    data[(3 * SIZE + 3) * 4 + 2] = 255;
    vi.stubGlobal('OffscreenCanvas', makeStubCanvas(data, SIZE));
    const fakeBitmap = { width: SIZE, height: SIZE } as unknown as ImageBitmap;
    const direct = generateImageSdf(fakeBitmap, SIZE, false);
    const inverted = generateImageSdf(fakeBitmap, SIZE, true);
    // 中央: direct は inside (signed_px=+0.5 → byte=191)、
    //       invert は outside (signed_px=-0.5 → byte=64)。
    expect(direct.sdf[3 * SIZE + 3]).toBe(191);
    expect(inverted.sdf[3 * SIZE + 3]).toBe(64);
  });

  test('#174 非正方形画像 — レタボックスの透明領域に引きずられず被写体輪郭を抽出する', () => {
    // 16×8 の不透明 (alpha=255) 画像を 16×16 SDF サイズへ contain 描画する想定。
    // bitmap.width=16, bitmap.height=8 → scale=1, dw=16, dh=8, dx=0, dy=4。
    // 描画矩形 = rows 4..11、それ以外 (rows 0..3 と 12..15) はレタボ alpha=0。
    //
    // 旧実装は s*s 全体で alpha 集計したため、レタボ 8 行分 (128 px)で
    // hasMeaningfulAlpha=true → alpha 経路に倒れ、描画矩形全 128 px が inside と
    // 判定されて結果が「16×8 の矩形シルエット」になっていた (#174)。
    //
    // 新実装は描画矩形 (0..16, 4..12) 内だけで alpha を集計する。drawn pixel は
    // 全 alpha=255 なので alphaPixelCount=0 → 輝度経路 → 「少数派 = 被写体」で
    // 中央 2×2 の暗パッチだけが inside、白背景は outside になる。
    const SIZE = 16;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    // レタボ部分は alpha=0 のまま (clearRect 後の initial state を再現)。
    // 描画矩形 (rows 4..11) は白背景 alpha=255 + 中央 2×2 を黒で塗る。
    for (let y = 4; y < 12; y++) {
      for (let x = 0; x < 16; x++) {
        const i = y * SIZE + x;
        data[i * 4] = 255;
        data[i * 4 + 1] = 255;
        data[i * 4 + 2] = 255;
        data[i * 4 + 3] = 255;
      }
    }
    // 中央 2×2 の暗パッチ (= 被写体)。位置 (7,7), (8,7), (7,8), (8,8)。
    for (const [x, y] of [
      [7, 7],
      [8, 7],
      [7, 8],
      [8, 8],
    ] as const) {
      const i = y * SIZE + x;
      data[i * 4] = 0;
      data[i * 4 + 1] = 0;
      data[i * 4 + 2] = 0;
    }
    vi.stubGlobal('OffscreenCanvas', makeStubCanvas(data, SIZE));
    const fakeBitmap = { width: 16, height: 8 } as unknown as ImageBitmap;
    const r = generateImageSdf(fakeBitmap, SIZE);
    expect(r.ok).toBe(true);
    // 中央 2×2 の暗パッチ ((7,7) etc.) は inside (= byte > 127)。
    expect(r.sdf[7 * SIZE + 7]).toBeGreaterThan(127);
    // 描画矩形内の白背景 ((2,7) など、被写体と同じ row だが背景側) は outside。
    expect(r.sdf[7 * SIZE + 2]).toBeLessThan(127);
    // レタボ部分 (row 0 や row 15) も outside。旧実装ではここも inside 判定
    // された結果矩形シルエットになっていた。
    expect(r.sdf[0 * SIZE + 0]).toBeLessThan(127);
    expect(r.sdf[15 * SIZE + 8]).toBeLessThan(127);
  });

  test('#171 alpha<255 が 1px だけの「実質不透明」画像は alpha 経路に入らない', () => {
    // 8×8 の隅 1 px だけ alpha=128、他 alpha=255、輝度差は中央 1 px に持たせる。
    // alphaPixelCount = 1、s*s = 64、1*100 = 100、64 と比べて 100 > 64 → true。
    // つまり SIZE=8 の場合 1 px でも > 1% を超えてしまう。テストは輝度経路を
    // ちゃんとピックアップする SIZE=32 で行う (1 px / 1024 = 0.097% < 1%)。
    const SIZE = 32;
    const data = new Uint8ClampedArray(SIZE * SIZE * 4);
    for (let i = 0; i < SIZE * SIZE; i++) data[i * 4 + 3] = 255;
    data[3] = 128; // (0,0) alpha=128 (1 px だけ alpha < 255)
    // 中央 (16,16) を白く
    const c = (16 * SIZE + 16) * 4;
    data[c] = 255;
    data[c + 1] = 255;
    data[c + 2] = 255;
    vi.stubGlobal('OffscreenCanvas', makeStubCanvas(data, SIZE));
    const fakeBitmap = { width: SIZE, height: SIZE } as unknown as ImageBitmap;
    const r = generateImageSdf(fakeBitmap, SIZE);
    expect(r.ok).toBe(true);
    // 輝度経路に入ったなら中央 (16,16) が inside で byte が高く、
    // (0,0) は outside で 0 寄り。alpha 経路だと逆 (alpha=128 なら inside)。
    expect(r.sdf[16 * SIZE + 16]).toBeGreaterThan(127);
    expect(r.sdf[0]).toBeLessThan(127);
  });
});
