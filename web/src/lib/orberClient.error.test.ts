// orber#245 — orberClient のエラー伝播テスト。
//
// worker は catch したエラーを `String(err)` で `{ ok: false, error }` として
// post し（orberWorker.ts）、orberClient はそれを `new Error(error)` に包んで
// reject する。Studio の formatRunBatchError はこの Error.message に対する
// `includes('webgpu-unsupported')` で i18n 文言へマップするため、RPC 境界を
// 越えても sentinel が message 中に残ることをここで固定する
// （orberClient.alpha.test.ts と同じ fake Worker 流儀。実 wasm はロードしない）。

import { beforeEach, describe, expect, it, vi } from 'vitest';

// ---- Fake Worker ----------------------------------------------------------
// `new OrberWorker()` で生成される Worker 互換 stub（orberClient.alpha.test.ts
// と同形。vi.mock はファイル単位なのでここにも最小構成で持つ）。
type Listener = (e: { data: unknown }) => void;
interface FakeWorker {
  postMessage: ReturnType<typeof vi.fn>;
  addEventListener: (type: string, l: Listener) => void;
  removeEventListener: (type: string, l: Listener) => void;
  terminate: ReturnType<typeof vi.fn>;
  dispatch: (data: unknown) => void;
  __listeners: { message: Listener[]; error: Listener[] };
}

const workerState = vi.hoisted(() => ({
  instances: [] as FakeWorker[],
}));

vi.mock('./orberWorker?worker', () => {
  const ctor = vi.fn().mockImplementation(() => {
    const inst: FakeWorker = {
      __listeners: { message: [], error: [] },
      postMessage: vi.fn(),
      addEventListener(type: string, l: Listener) {
        if (type === 'message' || type === 'error') inst.__listeners[type].push(l);
      },
      removeEventListener(type: string, l: Listener) {
        if (type !== 'message' && type !== 'error') return;
        const arr = inst.__listeners[type];
        const i = arr.indexOf(l);
        if (i >= 0) arr.splice(i, 1);
      },
      terminate: vi.fn(),
      dispatch(data: unknown) {
        for (const l of inst.__listeners.message) l({ data });
      },
    };
    workerState.instances.push(inst);
    return inst;
  });
  return { default: ctor };
});

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  workerState.instances.length = 0;
});

function baseParams() {
  return {
    k: 1,
    width: 64,
    height: 48,
    seed: 0,
    direction: 'lr',
    speed: 'slow',
    count: 1,
    orb_size: 1,
    blur: 0,
    shape: 'orb',
  };
}

describe('orberClient のエラー伝播 (#245)', () => {
  it("worker の {ok:false, error:'Error: webgpu-unsupported: …'} は reject した Error.message に sentinel が残る", async () => {
    const { workerGenerateOne } = await import('./orberClient');
    const p = workerGenerateOne(baseParams(), 12, 0);
    const inst = workerState.instances[0];
    const sent = inst.postMessage.mock.calls[0][0] as { id: number };
    // worker 側 `post({ id, ok: false, error: String(err) })` の実形を再現:
    // `String(new Error('webgpu-unsupported: …'))` は 'Error: ' が前置される。
    inst.dispatch({
      id: sent.id,
      ok: false,
      error: 'Error: webgpu-unsupported: navigator.gpu is not available in this worker',
    });
    let caught: unknown;
    try {
      await p;
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(Error);
    // formatRunBatchError の includes 照合が成立する前提条件。
    expect((caught as Error).message).toContain('webgpu-unsupported');
  });
});
