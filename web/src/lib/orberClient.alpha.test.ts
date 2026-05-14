// orber#184 — workerAnimateOneAlpha の単体テスト。
//
// - `./orberWorker?worker` を vi.mock で fake Worker constructor に置換
// - `./encodeAlphaVideoWasm` を vi.mock で encode 関数を spy 化
//   (実 wasm はロードしない)
//
// 観点: worker への post 内容、frame 集約の順序保証、欠損検出、worker 失敗時、
// 進捗中継。

import { beforeEach, describe, expect, it, vi } from 'vitest';

// ---- Fake Worker ----------------------------------------------------------
// `new OrberWorker()` で生成される Worker 互換 stub。
// postMessage(msg) を受け取ったらテスト側で `__inst.dispatch({...})` を呼んで
// main 側のリスナにメッセージを流せる。
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

// ---- encode mock -----------------------------------------------------------
const encodeState = vi.hoisted(() => ({
  spy: null as null | ReturnType<typeof vi.fn>,
  // 渡された frames を保持 (snapshot 用)
  lastFrames: null as null | Uint8Array[],
  lastWidth: 0,
  lastHeight: 0,
  lastFps: 0,
  lastOnProgress: undefined as undefined | ((f: number, t: number) => void),
}));

vi.mock('./encodeAlphaVideoWasm', () => ({
  encodeAnimationAlphaWasm: vi.fn(
    async (
      frames: Uint8Array[],
      width: number,
      height: number,
      fps: number,
      onProgress?: (f: number, t: number) => void,
    ) => {
      // 渡された frames を snapshot として保存 (後でテスト側が中身を点検する)。
      encodeState.lastFrames = frames.map((f) => new Uint8Array(f));
      encodeState.lastWidth = width;
      encodeState.lastHeight = height;
      encodeState.lastFps = fps;
      encodeState.lastOnProgress = onProgress;
      // 進捗が中継できるかテスト用に 1 発発火
      onProgress?.(1, frames.length);
      return new Blob([new Uint8Array([0xff])], { type: 'video/webm' });
    },
  ),
}));

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  workerState.instances.length = 0;
  encodeState.lastFrames = null;
  encodeState.lastWidth = 0;
  encodeState.lastHeight = 0;
  encodeState.lastFps = 0;
  encodeState.lastOnProgress = undefined;
});

function baseParams(width = 64, height = 48) {
  return {
    k: 1,
    width,
    height,
    seed: 0,
    direction: 'up',
    speed: 'medium',
    count: 1,
    orb_size: 1,
    blur: 0,
    shape: 'circle',
  };
}

// テスト用のフレーム順番をディスパッチするヘルパ。
function dispatchFrames(
  inst: FakeWorker,
  id: number,
  order: number[],
  total: number,
): void {
  for (const f of order) {
    inst.dispatch({
      kind: 'alphaFrame',
      id,
      frame: f,
      total,
      data: new Uint8Array([f + 1]).buffer,
    });
  }
}

// 本体応答 (ok:true) を投げる。
function dispatchOk(inst: FakeWorker, id: number): void {
  inst.dispatch({ id, ok: true });
}

describe('workerAnimateOneAlpha', () => {
  it('worker に renderAlphaFrames kind の message が post される', async () => {
    const { workerAnimateOneAlpha } = await import('./orberClient');
    const params = baseParams();
    const total = 2;
    const p = workerAnimateOneAlpha(params, 1, 0, total);
    // postMessage が呼ばれているはず
    const inst = workerState.instances[0];
    expect(inst.postMessage).toHaveBeenCalledTimes(1);
    const sent = inst.postMessage.mock.calls[0][0] as {
      kind: string;
      params: unknown;
      n: number;
      index: number;
      totalFrames: number;
      id: number;
    };
    expect(sent.kind).toBe('renderAlphaFrames');
    expect(sent.totalFrames).toBe(total);
    expect(sent.n).toBe(1);
    expect(sent.index).toBe(0);
    // 完走させる
    dispatchFrames(inst, sent.id, [0, 1], total);
    dispatchOk(inst, sent.id);
    await p;
  });

  it('frame 0..N-1 を順番に受けると encode に N 個の Uint8Array が順序通りで渡る', async () => {
    const { workerAnimateOneAlpha } = await import('./orberClient');
    const total = 4;
    const p = workerAnimateOneAlpha(baseParams(), 1, 0, total);
    const inst = workerState.instances[0];
    const sent = inst.postMessage.mock.calls[0][0] as { id: number };
    dispatchFrames(inst, sent.id, [0, 1, 2, 3], total);
    dispatchOk(inst, sent.id);
    await p;
    expect(encodeState.lastFrames?.length).toBe(total);
    for (let i = 0; i < total; i++) {
      expect(encodeState.lastFrames![i][0]).toBe(i + 1);
    }
  });

  it('frame がランダム順 (3→0→2→1) で来ても encode 内の順序は frame index 通り', async () => {
    const { workerAnimateOneAlpha } = await import('./orberClient');
    const total = 4;
    const p = workerAnimateOneAlpha(baseParams(), 1, 0, total);
    const inst = workerState.instances[0];
    const sent = inst.postMessage.mock.calls[0][0] as { id: number };
    dispatchFrames(inst, sent.id, [3, 0, 2, 1], total);
    dispatchOk(inst, sent.id);
    await p;
    expect(encodeState.lastFrames?.length).toBe(total);
    for (let i = 0; i < total; i++) {
      expect(encodeState.lastFrames![i][0]).toBe(i + 1);
    }
  });

  it('frame=2 が欠けたまま ok:true が来ると "missing alpha frame" で reject (silent skip 防止)', async () => {
    const { workerAnimateOneAlpha } = await import('./orberClient');
    const total = 4;
    const p = workerAnimateOneAlpha(baseParams(), 1, 0, total);
    const inst = workerState.instances[0];
    const sent = inst.postMessage.mock.calls[0][0] as { id: number };
    dispatchFrames(inst, sent.id, [0, 1, 3], total); // frame 2 抜け
    dispatchOk(inst, sent.id);
    await expect(p).rejects.toThrow(/missing alpha frame 2\/4/);
  });

  it('worker が {ok:false, error} を返すと reject し encode は呼ばれない', async () => {
    const { workerAnimateOneAlpha } = await import('./orberClient');
    const encMod = await import('./encodeAlphaVideoWasm');
    const total = 2;
    const p = workerAnimateOneAlpha(baseParams(), 1, 0, total);
    const inst = workerState.instances[0];
    const sent = inst.postMessage.mock.calls[0][0] as { id: number };
    inst.dispatch({ id: sent.id, ok: false, error: 'render boom' });
    await expect(p).rejects.toThrow(/render boom/);
    expect(encMod.encodeAnimationAlphaWasm).not.toHaveBeenCalled();
  });

  it('onProgress は worker 経路 (animateProgress) と ffmpeg 経路の両方で発火する', async () => {
    const { workerAnimateOneAlpha } = await import('./orberClient');
    const total = 2;
    const seen: Array<[number, number]> = [];
    const p = workerAnimateOneAlpha(baseParams(), 1, 0, total, (f, t) =>
      seen.push([f, t]),
    );
    const inst = workerState.instances[0];
    const sent = inst.postMessage.mock.calls[0][0] as { id: number };
    // worker からの中間進捗
    inst.dispatch({
      kind: 'animateProgress',
      id: sent.id,
      frame: 1,
      total,
    });
    dispatchFrames(inst, sent.id, [0, 1], total);
    dispatchOk(inst, sent.id);
    await p;
    // worker 経由で 1 回、encode mock 経由で 1 回 (mock 内で onProgress(1, total))
    expect(seen.length).toBeGreaterThanOrEqual(2);
    expect(seen.some(([f, t]) => f === 1 && t === total)).toBe(true);
  });
});
