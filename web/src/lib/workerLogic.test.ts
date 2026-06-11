// orber#245 — workerLogic.ts（worker / Studio から切り出した純粋ロジック）の
// 単体テスト。
//
// - buildWasmParams: WasmParams 組立てのデシジョンテーブル（source 必須 /
//   image マスク必須 / glyph SDF フォールバックとキャッシュ / orb 素通し /
//   transparent_background キーの有無）。wasm / OffscreenCanvas は
//   ロードせず、glyphSupported / generateSdf を vi.fn で DI する
// - bleedDerivedParams (#253): 単一「にじみ」ノブのレベル語が
//   bleed/bloom/halo/offset の 4 preset を「すべて同じ語で」駆動する
//   （ロックステップ）ことを固定する。
// - computeMaskSize: 長辺 1024 縮小の境界とアスペクト比保持
// - formatRunBatchError: sentinel → i18n キーのマップ（t() は fake を DI）。
//   worker が `String(err)` で post する 'Error: ' 前置きでも includes
//   マッチが生きる契約も固定する

import { describe, expect, it, vi } from 'vitest';
import {
  bleedDerivedParams,
  buildWasmParams,
  computeMaskSize,
  formatRunBatchError,
  GLYPH_SDF_SIZE,
  IMAGE_MASK_TARGET_LONG_EDGE,
  softnessToBleedLevel,
  type BaseParams,
  type BleedLevel,
  type BuildWasmParamsDeps,
  type SoftnessLevel,
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

  it('#239: bleed_preset は素通しで wasm params に乗る（orb / glyph / image どの shape でも）', () => {
    // buildWasmParams は BaseParams を `...p` で展開するので、bleed_preset を
    // 渡せば wasm 側 WasmParams.bleed_preset へそのまま流れる。wasm 側で
    // 'weak'|'mid'|'strong' → aqua_bleed 0.15/0.3/0.5 に写像される（Rust 側で固定）。
    for (const preset of ['weak', 'mid', 'strong'] as const) {
      const params = buildWasmParams(baseParams({ bleed_preset: preset }), makeDeps());
      expect(params.bleed_preset).toBe(preset);
    }
  });

  it('buildWasmParams は preset を合成しない（BaseParams に無いキーは素通しで生やさない）', () => {
    // buildWasmParams 自体は与えられた preset を素通しするだけで、欠けたキーを
    // 補完しない（#253 で にじみは Studio 側で常時 'weak'/'mid'/'strong' を必ず
    // 渡すようになったが、buildWasmParams は依然として汎用の素通しヘルパ）。
    const params = buildWasmParams(baseParams(), makeDeps());
    expect('bleed_preset' in params).toBe(false);
    expect('bloom_preset' in params).toBe(false);
    expect('halo_preset' in params).toBe(false);
    expect('offset_preset' in params).toBe(false);
  });

  it('#239: bloom / halo / offset の preset も素通しで wasm params に乗る', () => {
    // にじみと同じく `...p` 展開で wasm 側 WasmParams.{bloom,halo,offset}_preset へ
    // そのまま流れる。wasm 側で 'weak'|'mid'|'strong' → 0.3/0.6/0.9 に写像される。
    for (const preset of ['weak', 'mid', 'strong'] as const) {
      const params = buildWasmParams(
        baseParams({
          bleed_preset: 'mid',
          bloom_preset: preset,
          halo_preset: preset,
          offset_preset: preset,
        }),
        makeDeps(),
      );
      expect(params.bloom_preset).toBe(preset);
      expect(params.halo_preset).toBe(preset);
      expect(params.offset_preset).toBe(preset);
    }
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

describe('bleedDerivedParams (#253: にじみノブのロックステップ)', () => {
  // #253: session605(#239 Phase 1) で出した にじみ/芯の光/縁の彩度/かたより の
  // 4 軸を単一「にじみ」ノブに畳んだ。レベル（弱/中/強）が bleed/bloom/halo/offset
  // の 4 preset フィールドを「すべて同じ語で」駆動する＝ロックステップなのが新仕様。
  it('レベル語が bleed/bloom/halo/offset の 4 フィールドを同じ語で駆動する', () => {
    for (const level of ['weak', 'mid', 'strong'] as const) {
      const d = bleedDerivedParams(level);
      // 4 フィールドすべてが同じレベル語であること（ロックステップ）。
      expect(d.bleed_preset).toBe(level);
      expect(d.bloom_preset).toBe(level);
      expect(d.halo_preset).toBe(level);
      expect(d.offset_preset).toBe(level);
      // 重複も含め、4 フィールドの値が全て一致する（横並びの不一致を弾く）。
      const values = [d.bleed_preset, d.bloom_preset, d.halo_preset, d.offset_preset];
      expect(new Set(values).size).toBe(1);
      // 余計なキーを生やさない（param 組み立てに混入させないため）。
      expect(Object.keys(d).sort()).toEqual(
        ['bleed_preset', 'bloom_preset', 'halo_preset', 'offset_preset'].sort(),
      );
    }
  });

  it('導出した 4 preset を BaseParams に展開すると buildWasmParams 経由でも同じ語のまま乗る', () => {
    // Studio の param 組み立て（#265 で `softnessToBleedLevel(softnessPreset())` 経由）を
    // 再現し、buildWasmParams の素通しを通っても 4 フィールドがロックステップを保つことを固定。
    const level: BleedLevel = 'mid';
    const params = buildWasmParams(baseParams({ ...bleedDerivedParams(level) }), makeDeps());
    expect(params.bleed_preset).toBe(level);
    expect(params.bloom_preset).toBe(level);
    expect(params.halo_preset).toBe(level);
    expect(params.offset_preset).toBe(level);
  });
});

describe('softnessToBleedLevel (#265: にじみをぼかしへ統合)', () => {
  // #265: にじみ独立ノブを撤去し、ぼかし(softness)レベルが にじみ(aqua_bleed)も
  // 駆動する。ぼかし 3 段（low/mid/high、`''`=標準）→ にじみ 3 段（weak/mid/strong）。
  it('ぼかしレベルを にじみレベルへ写す（弱め→弱 / 標準→中 / 強め→強）', () => {
    expect(softnessToBleedLevel('low')).toBe('weak');
    expect(softnessToBleedLevel('mid')).toBe('mid');
    expect(softnessToBleedLevel('high')).toBe('strong');
  });

  it('ぼかし未指定（標準）は にじみ mid（=標準）になる＝「デフォルトは標準」を満たす', () => {
    // Studio の softnessPreset 既定は `''`。これが にじみ mid に落ちることで、
    // にじみだけ既定 weak だった以前の例外が消える。
    expect(softnessToBleedLevel('')).toBe('mid');
  });

  it('全 SoftnessLevel が有効な BleedLevel を返す（にじみ常時オン＝必ず非空）', () => {
    const levels: SoftnessLevel[] = ['', 'low', 'mid', 'high'];
    for (const s of levels) {
      const b = softnessToBleedLevel(s);
      expect(['weak', 'mid', 'strong']).toContain(b);
    }
  });

  it('ぼかし → にじみ → 4 preset まで通すと、ぼかしレベルが 4 フィールドを一括駆動する', () => {
    // #265 の実 param 経路（softness → softnessToBleedLevel → bleedDerivedParams）を固定。
    const d = bleedDerivedParams(softnessToBleedLevel('high'));
    expect(d.bleed_preset).toBe('strong');
    expect(new Set([d.bleed_preset, d.bloom_preset, d.halo_preset, d.offset_preset]).size).toBe(1);
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
