// orber#232 — isWebGpuSupported() の単体テスト。
//
// A/B 比較パネル（WebGL↔WGSL トグル）が WGSL ボタンを enable できるかを決める
// 最小ユーティリティ。判定は `typeof navigator !== 'undefined' && 'gpu' in navigator`
// で、navigator.gpu の値の truthy までは見ない（adapter 取得の成否は gpu_init 側で
// 扱う）。この「存在チェックだけ」の契約をそのまま固定する。
//
// stub の流儀は encodeMp4.test.ts に倣う（vi.stubGlobal + afterEach で unstub）。
// SSR 想定 (navigator 未定義) は strings.test.ts の `vi.stubGlobal('window', undefined)`
// と同じく、stubGlobal に undefined を渡すと `typeof navigator === 'undefined'` に
// なることを利用する。

import { afterEach, describe, expect, it, vi } from 'vitest';
import { isWebGpuSupported } from './webgpu';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('isWebGpuSupported()', () => {
  it('W1: navigator.gpu が存在すれば true', () => {
    vi.stubGlobal('navigator', { gpu: {} });
    expect(isWebGpuSupported()).toBe(true);
  });

  it('W2: navigator が空オブジェクト (gpu なし) なら false', () => {
    vi.stubGlobal('navigator', {});
    expect(isWebGpuSupported()).toBe(false);
  });

  it('W3: navigator 未定義 (SSR 想定) なら false', () => {
    // jsdom では navigator が常に定義されるため、stubGlobal に undefined を渡して
    // `typeof navigator === 'undefined'` 経路 (SSR) を再現する。
    vi.stubGlobal('navigator', undefined);
    expect(typeof navigator).toBe('undefined');
    expect(isWebGpuSupported()).toBe(false);
  });

  it('W4: { gpu: undefined } でも true (存在チェックのみ・値の truthy は見ない仕様)', () => {
    // 契約は `'gpu' in navigator`。キーが存在すれば値が undefined でも true を返す。
    // 「navigator.gpu があってもアダプタ取得に失敗する環境」は gpu_init 側の reject で
    // 扱う設計（webgpu.ts コメント参照）なので、ここでは値を見ない契約を固定する。
    vi.stubGlobal('navigator', { gpu: undefined });
    expect(isWebGpuSupported()).toBe(true);
  });
});
