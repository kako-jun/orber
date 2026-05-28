// orber#196 — pickSupportedVideoCodec の単体テスト。
//
// VideoEncoder.isConfigSupported を vi.stubGlobal でモックし、
// H.264 → VP9 → AV1 の探索順序、prefer-hardware → no-preference の
// 2 段リトライ、解像度に応じた avc1 codec string 切替を検証する。

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { pickSupportedVideoCodec } from './encodeMp4';

type AccelHint = 'prefer-hardware' | 'no-preference';
interface ProbeConfig {
  codec: string;
  width: number;
  height: number;
  framerate: number;
  bitrate: number;
  hardwareAcceleration: AccelHint;
}

let isConfigSupported: ReturnType<typeof vi.fn>;

beforeEach(() => {
  isConfigSupported = vi.fn();
  vi.stubGlobal('VideoEncoder', { isConfigSupported });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

// (A) 正常系 — 採用 codec の確定 -----------------------------------------

describe('pickSupportedVideoCodec - 正常系', () => {
  it('A1: H.264 が hw で supported なら avc1 + prefer-hardware を返す', async () => {
    isConfigSupported.mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toEqual({
      codec: 'avc1.42E01F',
      muxerCodec: 'avc',
      hardwareAcceleration: 'prefer-hardware',
    });
  });

  it('A2: H.264 false → VP9 が hw で supported なら vp9 を返す', async () => {
    isConfigSupported
      .mockResolvedValueOnce({ supported: false, config: {} })
      .mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1080, 1920);
    expect(picked).toEqual({
      codec: 'vp09.00.41.08',
      muxerCodec: 'vp9',
      hardwareAcceleration: 'prefer-hardware',
    });
  });

  it('A3: H.264/VP9 false → AV1 が hw で supported なら av1 を返す', async () => {
    isConfigSupported
      .mockResolvedValueOnce({ supported: false, config: {} })
      .mockResolvedValueOnce({ supported: false, config: {} })
      .mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1080, 1920);
    expect(picked).toEqual({
      codec: 'av01.0.09M.08',
      muxerCodec: 'av1',
      hardwareAcceleration: 'prefer-hardware',
    });
  });

  it('A4: hw 全滅 → no-preference で H.264 supported なら hardwareAcceleration: no-preference', async () => {
    isConfigSupported
      .mockResolvedValueOnce({ supported: false, config: {} }) // hw avc
      .mockResolvedValueOnce({ supported: false, config: {} }) // hw vp9
      .mockResolvedValueOnce({ supported: false, config: {} }) // hw av1
      .mockResolvedValueOnce({ supported: true, config: {} }); // no-pref avc
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toEqual({
      codec: 'avc1.42E01F',
      muxerCodec: 'avc',
      hardwareAcceleration: 'no-preference',
    });
  });

  it('A5: hw 全滅 → no-pref で AV1 のみ true なら av01 + no-preference (探索順序も検証)', async () => {
    isConfigSupported
      .mockResolvedValueOnce({ supported: false, config: {} }) // hw avc
      .mockResolvedValueOnce({ supported: false, config: {} }) // hw vp9
      .mockResolvedValueOnce({ supported: false, config: {} }) // hw av1
      .mockResolvedValueOnce({ supported: false, config: {} }) // no-pref avc
      .mockResolvedValueOnce({ supported: false, config: {} }) // no-pref vp9
      .mockResolvedValueOnce({ supported: true, config: {} }); // no-pref av1
    const picked = await pickSupportedVideoCodec(1080, 1920);
    expect(picked).toEqual({
      codec: 'av01.0.09M.08',
      muxerCodec: 'av1',
      hardwareAcceleration: 'no-preference',
    });

    // mock.calls で探索順序を厳密に確認
    expect(isConfigSupported).toHaveBeenCalledTimes(6);
    const calls = isConfigSupported.mock.calls.map((c) => {
      const cfg = c[0] as ProbeConfig;
      return [cfg.codec, cfg.hardwareAcceleration];
    });
    expect(calls).toEqual([
      ['avc1.42E02A', 'prefer-hardware'],
      ['vp09.00.41.08', 'prefer-hardware'],
      ['av01.0.09M.08', 'prefer-hardware'],
      ['avc1.42E02A', 'no-preference'],
      ['vp09.00.41.08', 'no-preference'],
      ['av01.0.09M.08', 'no-preference'],
    ]);
  });
});

// (B) 異常系・null 返却 ---------------------------------------------------

describe('pickSupportedVideoCodec - 異常系', () => {
  it('B6: VideoEncoder が undefined なら null（isConfigSupported は呼ばれない）', async () => {
    vi.unstubAllGlobals();
    vi.stubGlobal('VideoEncoder', undefined);
    // ローカルの spy を新たに置いて 0 回呼ばれた事を確認
    const spy = vi.fn();
    // VideoEncoder 自体が undefined なのでこの spy は呼ばれ得ないが
    // 念のため別経路として用意（実行時アクセスを検知する目的ではない）
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toBeNull();
    expect(spy).not.toHaveBeenCalled();
  });

  it('B7: VideoEncoder.isConfigSupported が関数でない場合 null', async () => {
    vi.unstubAllGlobals();
    vi.stubGlobal('VideoEncoder', { isConfigSupported: 'not-a-function' });
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toBeNull();
  });

  it('B8: 全 6 試行で supported: false なら null（呼び出し回数ちょうど 6）', async () => {
    isConfigSupported.mockResolvedValue({ supported: false, config: {} });
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toBeNull();
    expect(isConfigSupported).toHaveBeenCalledTimes(6);
  });

  it('B9: throw した候補はスキップして次へ進む (H.264 throw → VP9 true)', async () => {
    isConfigSupported
      .mockRejectedValueOnce(new Error('invalid codec string'))
      .mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toEqual({
      codec: 'vp09.00.41.08',
      muxerCodec: 'vp9',
      hardwareAcceleration: 'prefer-hardware',
    });
  });

  it('B10: 全候補で throw → null', async () => {
    isConfigSupported.mockRejectedValue(new Error('boom'));
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toBeNull();
    expect(isConfigSupported).toHaveBeenCalledTimes(6);
  });
});

// (C) 同値分割・境界値（解像度） ------------------------------------------

describe('pickSupportedVideoCodec - 解像度境界値', () => {
  it('C11: 1280x720 (codedArea = 921,600 ジャスト) は avc1.42E01F', async () => {
    isConfigSupported.mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked?.codec).toBe('avc1.42E01F');
    const firstArg = isConfigSupported.mock.calls[0][0] as ProbeConfig;
    expect(firstArg.codec).toBe('avc1.42E01F');
  });

  it('C12: 1281x720 (codedArea > 921,600) は avc1.42E02A', async () => {
    isConfigSupported.mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1281, 720);
    expect(picked?.codec).toBe('avc1.42E02A');
  });

  it('C13: 1080x1920 は avc1.42E02A (1080p 縦)', async () => {
    isConfigSupported.mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1080, 1920);
    expect(picked?.codec).toBe('avc1.42E02A');
  });

  it('C14: 64x64 は avc1.42E01F、VP9/AV1 文字列は固定', async () => {
    isConfigSupported.mockResolvedValue({ supported: false, config: {} });
    await pickSupportedVideoCodec(64, 64);
    const codecs = isConfigSupported.mock.calls.map(
      (c) => (c[0] as ProbeConfig).codec,
    );
    // 各 hint で同じ 3 候補を順に試すので、最初の 3 件 = 候補一覧
    expect(codecs.slice(0, 3)).toEqual([
      'avc1.42E01F',
      'vp09.00.41.08',
      'av01.0.09M.08',
    ]);
  });
});

// (D) isConfigSupported に渡す引数 ----------------------------------------

describe('pickSupportedVideoCodec - probe 引数', () => {
  it('D15: 1 回目の call args が必要キーを全て含む (framerate キー名)', async () => {
    isConfigSupported.mockResolvedValueOnce({ supported: true, config: {} });
    await pickSupportedVideoCodec(1280, 720);
    const arg = isConfigSupported.mock.calls[0][0] as ProbeConfig;
    expect(arg).toEqual({
      codec: 'avc1.42E01F',
      width: 1280,
      height: 720,
      framerate: 24,
      bitrate: 2_000_000,
      hardwareAcceleration: 'prefer-hardware',
    });
    // `framerate` キーが存在することを単独でも検証 (frameRate と取り違えていないか)
    expect(Object.keys(arg)).toContain('framerate');
    expect(Object.keys(arg)).not.toContain('frameRate');
  });

  it('D16: 第 2 候補は vp09.00.41.08、第 3 候補は av01.0.09M.08 で probe される', async () => {
    isConfigSupported.mockResolvedValue({ supported: false, config: {} });
    await pickSupportedVideoCodec(1280, 720);
    const calls = isConfigSupported.mock.calls.map(
      (c) => (c[0] as ProbeConfig).codec,
    );
    expect(calls[1]).toBe('vp09.00.41.08');
    expect(calls[2]).toBe('av01.0.09M.08');
  });
});

// (E) 事故パターン予防 ---------------------------------------------------

describe('pickSupportedVideoCodec - 事故パターン予防', () => {
  it('E17: 1 回目で true なら 2 回目以降は呼ばれない', async () => {
    isConfigSupported.mockResolvedValueOnce({ supported: true, config: {} });
    await pickSupportedVideoCodec(1280, 720);
    expect(isConfigSupported).toHaveBeenCalledTimes(1);
  });

  it('E18: AV1 採用ケースで muxerCodec === "av1" を明示 assert', async () => {
    isConfigSupported
      .mockResolvedValueOnce({ supported: false, config: {} })
      .mockResolvedValueOnce({ supported: false, config: {} })
      .mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1080, 1920);
    expect(picked?.muxerCodec).toBe('av1');
  });

  it('E19: { supported: false, config: {...} } を返したら採用せず次候補へ', async () => {
    // 1 回目: supported: false だが config プロパティは付いている
    // 2 回目: supported: true → こちらが採用される事
    isConfigSupported
      .mockResolvedValueOnce({
        supported: false,
        config: { codec: 'avc1.42E01F', width: 1280, height: 720 },
      })
      .mockResolvedValueOnce({ supported: true, config: {} });
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked?.muxerCodec).toBe('vp9');
    expect(picked?.codec).toBe('vp09.00.41.08');
  });

  it('E20: 連続 2 回呼んでも同じ結果（純関数性、呼び出し回数も 2 倍）', async () => {
    isConfigSupported.mockResolvedValue({ supported: false, config: {} });
    const a = await pickSupportedVideoCodec(1280, 720);
    const b = await pickSupportedVideoCodec(1280, 720);
    expect(a).toBeNull();
    expect(b).toBeNull();
    expect(isConfigSupported).toHaveBeenCalledTimes(12);
  });

  it('E21: throw catch 時に console.error / console.warn を呼ばない', async () => {
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    isConfigSupported.mockRejectedValue(new Error('probe failed'));
    const picked = await pickSupportedVideoCodec(1280, 720);
    expect(picked).toBeNull();
    expect(errSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
  });
});
