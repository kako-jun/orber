// orber#245 — workerLogic.ts（worker / Studio から切り出した純粋ロジック）の
// 単体テスト。
//
// - buildWasmParams: WasmParams 組立てのデシジョンテーブル（source 必須 /
//   image マスク必須 / glyph SDF フォールバックとキャッシュ / orb・aquarelle
//   素通し / transparent_background キーの有無）。wasm / OffscreenCanvas は
//   ロードせず、glyphSupported / generateSdf を vi.fn で DI する
// - computeMaskSize: 長辺 1024 縮小の境界とアスペクト比保持
// - formatRunBatchError: sentinel → i18n キーのマップ（t() は fake を DI）。
//   worker が `String(err)` で post する 'Error: ' 前置きでも includes
//   マッチが生きる契約も固定する

import { describe, expect, it, vi } from 'vitest';
import {
  buildWasmParams,
  computeMaskSize,
  formatRunBatchError,
  GLYPH_SDF_SIZE,
  IMAGE_MASK_TARGET_LONG_EDGE,
  type BaseParams,
  type BuildWasmParamsDeps,
} from './workerLogic';

function baseParams(overrides: Partial<BaseParams> = {}): BaseParams {
  return {
    k: 4,
    width: 270,
    height: 480,
    seed: 42,
    direction: 'lr',
    speed: 'slow',
    count: 10,
    orb_size: 3,
    blur: 0.5,
    shape: 'orb',
    ...overrides,
  };
}

function makeDeps(overrides: Partial<BuildWasmParamsDeps> = {}): BuildWasmParamsDeps {
  return {
    source: { rgb: new Uint8Array([1, 2, 3]), width: 1, height: 1 },
    imageMask: null,
    glyphSupported: vi.fn(() => true),
    generateSdf: vi.fn((_ch: string, size: number) => new Uint8Array(size * size)),
    glyphSdfCache: { current: null },
    ...overrides,
  };
}

describe('buildWasmParams', () => {
  it('source 未設定なら throw する（setSource 前の generate ガード）', () => {
    const deps = makeDeps({ source: null });
    expect(() => buildWasmParams(baseParams(), deps)).toThrow(/source not set/);
  });

  it("shape='image' で imageMask 未設定なら throw する（setImageShape 必須）", () => {
    const deps = makeDeps({ imageMask: null });
    expect(() => buildWasmParams(baseParams({ shape: 'image' }), deps)).toThrow(
      /image shape requires setImageShape/,
    );
  });

  it("shape='image' でキャッシュ済みマスクが image_mask_* として付与される", () => {
    const rgba = new Uint8Array(2 * 3 * 4);
    const deps = makeDeps({ imageMask: { rgba, width: 2, height: 3 } });
    const params = buildWasmParams(baseParams({ shape: 'image' }), deps);
    expect(params.image_mask_rgba).toBe(rgba);
    expect(params.image_mask_width).toBe(2);
    expect(params.image_mask_height).toBe(3);
  });

  it("shape='glyph' の同梱フォント収録字は SDF を生成しない（core フォント経路に任せる）", () => {
    const deps = makeDeps({ glyphSupported: vi.fn(() => true) });
    const params = buildWasmParams(baseParams({ shape: 'glyph', glyph_char: '☆' }), deps);
    expect(deps.generateSdf).not.toHaveBeenCalled();
    expect('glyph_sdf' in params).toBe(false);
    expect('glyph_sdf_size' in params).toBe(false);
    // 文字自体は params にそのまま乗る（wasm 側 OrbShape::Glyph 解決用）。
    expect(params.glyph_char).toBe('☆');
  });

  it("shape='glyph' の同梱フォント外の字は SDF を生成して glyph_sdf / glyph_sdf_size に乗せる", () => {
    const deps = makeDeps({ glyphSupported: vi.fn(() => false) });
    const params = buildWasmParams(baseParams({ shape: 'glyph', glyph_char: '漢' }), deps);
    expect(deps.generateSdf).toHaveBeenCalledExactlyOnceWith('漢', GLYPH_SDF_SIZE);
    expect(params.glyph_sdf).toBeInstanceOf(Uint8Array);
    expect(params.glyph_sdf_size).toBe(GLYPH_SDF_SIZE);
    // キャッシュも同じ ch / sdf で更新される（次回 hit の前提）。
    expect(deps.glyphSdfCache.current?.ch).toBe('漢');
    expect(deps.glyphSdfCache.current?.sdf).toBe(params.glyph_sdf);
  });

  it('同じ ch のキャッシュ hit では SDF を再生成しない', () => {
    const cached = new Uint8Array(GLYPH_SDF_SIZE * GLYPH_SDF_SIZE);
    const deps = makeDeps({
      glyphSupported: vi.fn(() => false),
      glyphSdfCache: { current: { ch: '漢', sdf: cached } },
    });
    const params = buildWasmParams(baseParams({ shape: 'glyph', glyph_char: '漢' }), deps);
    expect(deps.generateSdf).not.toHaveBeenCalled();
    expect(params.glyph_sdf).toBe(cached);
  });

  it('別の ch ではキャッシュを使わず再生成してキャッシュを差し替える', () => {
    const stale = new Uint8Array(GLYPH_SDF_SIZE * GLYPH_SDF_SIZE);
    const deps = makeDeps({
      glyphSupported: vi.fn(() => false),
      glyphSdfCache: { current: { ch: '漢', sdf: stale } },
    });
    const params = buildWasmParams(baseParams({ shape: 'glyph', glyph_char: '字' }), deps);
    expect(deps.generateSdf).toHaveBeenCalledExactlyOnceWith('字', GLYPH_SDF_SIZE);
    expect(params.glyph_sdf).not.toBe(stale);
    expect(deps.glyphSdfCache.current?.ch).toBe('字');
  });

  it("shape='glyph' でも glyph_char が空なら SDF 経路に入らない（収録判定も呼ばない）", () => {
    const deps = makeDeps({ glyphSupported: vi.fn(() => false) });
    const params = buildWasmParams(baseParams({ shape: 'glyph', glyph_char: '' }), deps);
    expect(deps.glyphSupported).not.toHaveBeenCalled();
    expect(deps.generateSdf).not.toHaveBeenCalled();
    expect('glyph_sdf' in params).toBe(false);
  });

  it("shape='orb' は素通し（source_* だけ足して mask / SDF キーは付けない）", () => {
    const deps = makeDeps();
    const p = baseParams();
    const params = buildWasmParams(p, deps);
    expect(params.shape).toBe('orb');
    expect(params.seed).toBe(p.seed);
    expect(params.source_rgb).toBe(deps.source?.rgb);
    expect(params.source_width).toBe(1);
    expect(params.source_height).toBe(1);
    expect('image_mask_rgba' in params).toBe(false);
    expect('glyph_sdf' in params).toBe(false);
    expect(deps.glyphSupported).not.toHaveBeenCalled();
  });

  it("shape='aquarelle' も素通し（mask / SDF キー無し・SDF 生成も呼ばない）", () => {
    const deps = makeDeps();
    const params = buildWasmParams(baseParams({ shape: 'aquarelle' }), deps);
    expect(params.shape).toBe('aquarelle');
    expect('image_mask_rgba' in params).toBe(false);
    expect('glyph_sdf' in params).toBe(false);
    expect(deps.glyphSupported).not.toHaveBeenCalled();
    expect(deps.generateSdf).not.toHaveBeenCalled();
  });

  it('transparentBackground=true で transparent_background: true が付与される', () => {
    const params = buildWasmParams(baseParams(), makeDeps(), { transparentBackground: true });
    expect(params.transparent_background).toBe(true);
  });

  it('transparentBackground=false / opts 省略では transparent_background キー自体が無い（serde default に任せる）', () => {
    // `false` を明示的に詰めると将来 serde 側の default 変更と二重管理になる
    // ため、「キーが無い」ことまで固定する。
    const omitted = buildWasmParams(baseParams(), makeDeps());
    expect('transparent_background' in omitted).toBe(false);
    const explicitFalse = buildWasmParams(baseParams(), makeDeps(), {
      transparentBackground: false,
    });
    expect('transparent_background' in explicitFalse).toBe(false);
  });
});

describe('computeMaskSize', () => {
  it('長辺 1023 / 1024 / 1025 の境界（1024 ちょうどまでは縮小しない）', () => {
    expect(computeMaskSize(1023, 500, IMAGE_MASK_TARGET_LONG_EDGE)).toEqual({
      width: 1023,
      height: 500,
    });
    expect(computeMaskSize(1024, 512, IMAGE_MASK_TARGET_LONG_EDGE)).toEqual({
      width: 1024,
      height: 512,
    });
    expect(computeMaskSize(1025, 512, IMAGE_MASK_TARGET_LONG_EDGE)).toEqual({
      width: 1024,
      height: 512,
    });
  });

  it('縮小時にアスペクト比を保持する（4096×2048 → 1024×512）', () => {
    expect(computeMaskSize(4096, 2048, IMAGE_MASK_TARGET_LONG_EDGE)).toEqual({
      width: 1024,
      height: 512,
    });
  });

  it('極端なアスペクト比でも短辺は最小 1px（5000×3 → 1024×1）', () => {
    expect(computeMaskSize(5000, 3, IMAGE_MASK_TARGET_LONG_EDGE)).toEqual({
      width: 1024,
      height: 1,
    });
  });
});

describe('formatRunBatchError', () => {
  // i18n の実体（strings.ts / solid-js signal）は引かず、キーが分かる fake を DI。
  const fakeT = (key: string) => `i18n:${key}`;

  it('image-shape-no-contrast sentinel は imageShapeNoContrast にマップされる', () => {
    expect(formatRunBatchError(new Error('image-shape-no-contrast'), fakeT)).toBe(
      'i18n:imageShapeNoContrast',
    );
  });

  it("'Error: ' 前置きの webgpu-unsupported（worker → orberClient 往復後の形）も webgpuUnsupported にマップされる", () => {
    const e = new Error('Error: webgpu-unsupported: navigator.gpu is not available in this worker');
    expect(formatRunBatchError(e, fakeT)).toBe('i18n:webgpuUnsupported');
  });

  it('Error でない文字列 throw も String(e) 経由で sentinel を拾う', () => {
    expect(formatRunBatchError('webgpu-unsupported: adapter denied', fakeT)).toBe(
      'i18n:webgpuUnsupported',
    );
  });

  it('どちらの sentinel でもないエラーは文言を素通しする', () => {
    expect(formatRunBatchError(new Error('decode failed: bad PNG'), fakeT)).toBe(
      'decode failed: bad PNG',
    );
  });

  it("worker の post 形式 String(new Error(...)) は 'Error: ' が前置されるが includes マッチは生きる（契約固定）", () => {
    // worker は catch した err を `String(err)` で main に返し（orberWorker.ts）、
    // orberClient が `new Error(error)` に包み直す。この 2 段の変形を経ても
    // sentinel が message 中に残り、includes 照合が成立することを固定する。
    const workerSide = String(new Error('webgpu-unsupported: x'));
    expect(workerSide).toBe('Error: webgpu-unsupported: x');
    expect(formatRunBatchError(new Error(workerSide), fakeT)).toBe('i18n:webgpuUnsupported');
  });
});
