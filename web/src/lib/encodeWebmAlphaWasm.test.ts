// orber#184 — encodeWebmAlphaWasm の単体テスト。
//
// @ffmpeg/ffmpeg を vi.mock で完全置換し、libvpx-vp9 引数生成・
// シングルトン挙動・progress 中継・後片付けまわりの仕様をピン留めする。
// 実 wasm はロードしない (jsdom では動かない)。

import { beforeEach, describe, expect, it, vi } from 'vitest';

// 各テストの "this run" 中に最後に生成された FFmpeg instance に触れたいので
// グローバル参照を hoisted で持つ。vi.mock factory もここから参照する。
type ProgressHandler = (e: { progress: number; time: number }) => void;
interface MockFFmpeg {
  load: ReturnType<typeof vi.fn>;
  writeFile: ReturnType<typeof vi.fn>;
  exec: ReturnType<typeof vi.fn>;
  readFile: ReturnType<typeof vi.fn>;
  deleteFile: ReturnType<typeof vi.fn>;
  listDir: ReturnType<typeof vi.fn>;
  on: ReturnType<typeof vi.fn>;
  off: ReturnType<typeof vi.fn>;
  __progressHandlers: ProgressHandler[];
}

const mockState = vi.hoisted(() => {
  return {
    instances: [] as MockFFmpeg[],
    // テスト側で次回 load() の挙動を差し替える。
    nextLoadImpl: null as null | (() => Promise<void>),
    // exec の挙動を差し替える (進捗発火・失敗注入)。
    nextExecImpl: null as null | ((ff: MockFFmpeg, args: string[]) => Promise<void>),
    // readFile の返り値を差し替える。
    nextReadFileImpl: null as null | (() => Promise<Uint8Array | string>),
  };
});

// orber#184 hotfix: `@ffmpeg/util` の `toBlobURL` を fake 化する。
// 実装は CDN を fetch して blob: URL を作るため、jsdom では走らせられない。
// `coreURL` / `wasmURL` の引数 URL とプレフィックスだけ検証できれば十分。
vi.mock('@ffmpeg/util', () => {
  return {
    toBlobURL: vi.fn(async (url: string, mime: string) => {
      if (mime === 'text/javascript') return 'blob:mock-core';
      if (mime === 'application/wasm') return 'blob:mock-wasm';
      return `blob:mock-${mime}`;
    }),
  };
});

vi.mock('@ffmpeg/ffmpeg', () => {
  const FFmpeg = vi.fn().mockImplementation(() => {
    const inst: MockFFmpeg = {
      load: vi.fn().mockImplementation(() => {
        if (mockState.nextLoadImpl) {
          const impl = mockState.nextLoadImpl;
          mockState.nextLoadImpl = null;
          return impl();
        }
        return Promise.resolve();
      }),
      writeFile: vi.fn().mockResolvedValue(undefined),
      exec: vi.fn().mockImplementation((args: string[]) => {
        if (mockState.nextExecImpl) {
          const impl = mockState.nextExecImpl;
          mockState.nextExecImpl = null;
          return impl(inst, args);
        }
        return Promise.resolve();
      }),
      readFile: vi.fn().mockImplementation(() => {
        if (mockState.nextReadFileImpl) {
          const impl = mockState.nextReadFileImpl;
          mockState.nextReadFileImpl = null;
          return impl();
        }
        return Promise.resolve(new Uint8Array([1, 2, 3]));
      }),
      deleteFile: vi.fn().mockResolvedValue(undefined),
      listDir: vi.fn().mockResolvedValue([]),
      on: vi.fn().mockImplementation((event: string, handler: ProgressHandler) => {
        if (event === 'progress') inst.__progressHandlers.push(handler);
      }),
      off: vi.fn().mockImplementation((event: string, handler: ProgressHandler) => {
        if (event !== 'progress') return;
        const idx = inst.__progressHandlers.indexOf(handler);
        if (idx >= 0) inst.__progressHandlers.splice(idx, 1);
      }),
      __progressHandlers: [],
    };
    mockState.instances.push(inst);
    return inst;
  });
  return { FFmpeg };
});

beforeEach(() => {
  // シングルトン状態をリセットするため毎テストで動的 import し直す。
  vi.resetModules();
  vi.clearAllMocks();
  mockState.instances.length = 0;
  mockState.nextLoadImpl = null;
  mockState.nextExecImpl = null;
  mockState.nextReadFileImpl = null;
});

function makeFrame(byte: number, size = 8): Uint8Array {
  return new Uint8Array(size).fill(byte);
}

describe('encodeAnimationAlphaWasm', () => {
  it('1 フレーム入力で video/webm の Blob を返す (正常系)', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    const blob = await encodeAnimationAlphaWasm([makeFrame(1)], 32, 32);
    expect(blob).toBeInstanceOf(Blob);
    expect(blob.type).toBe('video/webm');
  });

  it('frames.length === 0 では Error を投げ ffmpeg.load を呼ばない', async () => {
    const mod = await import('./encodeWebmAlphaWasm');
    await expect(mod.encodeAnimationAlphaWasm([], 16, 16)).rejects.toThrow(
      /frames must be > 0/,
    );
    // load が一度も走っていない
    expect(mockState.instances.length).toBe(0);
  });

  it('ffmpeg.load には blob: URL が渡され、toBlobURL に jsdelivr CDN の coreURL / wasmURL が渡される (#184 hotfix: cross-origin importScripts 回避)', async () => {
    const { loadFfmpegAlphaEncoder, FFMPEG_CORE_VERSION, FFMPEG_CORE_URL, FFMPEG_WASM_URL } =
      await import('./encodeWebmAlphaWasm');
    const utilMod = await import('@ffmpeg/util');
    const toBlobURLMock = utilMod.toBlobURL as unknown as ReturnType<typeof vi.fn>;
    await loadFfmpegAlphaEncoder();
    const inst = mockState.instances[0];
    const arg = inst.load.mock.calls[0][0] as {
      coreURL: string;
      wasmURL: string;
    };
    // ffmpeg.load に渡るのは blob: URL (same-origin になり Worker の importScripts
    // が CORS 制約を受けない)
    expect(arg.coreURL).toMatch(/^blob:/);
    expect(arg.wasmURL).toMatch(/^blob:/);

    // toBlobURL の引数として jsdelivr CDN の URL + MIME が渡される
    const toBlobCalls = toBlobURLMock.mock.calls as Array<[string, string]>;
    expect(toBlobCalls.length).toBeGreaterThanOrEqual(2);
    const coreCall = toBlobCalls.find((c) => c[0] === FFMPEG_CORE_URL);
    const wasmCall = toBlobCalls.find((c) => c[0] === FFMPEG_WASM_URL);
    expect(coreCall).toBeDefined();
    expect(wasmCall).toBeDefined();
    expect(coreCall![1]).toBe('text/javascript');
    expect(wasmCall![1]).toBe('application/wasm');
    expect(coreCall![0]).toContain('cdn.jsdelivr.net/npm/@ffmpeg/core@');
    expect(coreCall![0]).toContain(FFMPEG_CORE_VERSION);
    expect(coreCall![0]).toMatch(/\/dist\/esm\/ffmpeg-core\.js$/);
    expect(wasmCall![0]).toContain('cdn.jsdelivr.net/npm/@ffmpeg/core@');
    expect(wasmCall![0]).toContain(FFMPEG_CORE_VERSION);
    expect(wasmCall![0]).toMatch(/\/dist\/esm\/ffmpeg-core\.wasm$/);
    // 同一オリジン `/ffmpeg/...` 配信に戻していないこと
    expect(coreCall![0]).not.toMatch(/^\/ffmpeg\//);
    expect(wasmCall![0]).not.toMatch(/^\/ffmpeg\//);
  });

  it('loadFfmpegAlphaEncoder を 2 回連続呼出しても new FFmpeg() / .load() は 1 回のみ', async () => {
    const { loadFfmpegAlphaEncoder } = await import('./encodeWebmAlphaWasm');
    await loadFfmpegAlphaEncoder();
    await loadFfmpegAlphaEncoder();
    expect(mockState.instances.length).toBe(1);
    expect(mockState.instances[0].load).toHaveBeenCalledTimes(1);
  });

  it('await せず並行に 2 回呼んでも load promise を共有する (FFmpeg は 1 回のみ生成)', async () => {
    const { loadFfmpegAlphaEncoder } = await import('./encodeWebmAlphaWasm');
    let resolveLoad: () => void = () => {};
    mockState.nextLoadImpl = () =>
      new Promise<void>((res) => {
        resolveLoad = res;
      });
    const p1 = loadFfmpegAlphaEncoder();
    const p2 = loadFfmpegAlphaEncoder();
    // この時点では 1 個しか生成されていないはず
    expect(mockState.instances.length).toBe(1);
    // hotfix #184: load 経路は `await toBlobURL(...)` を 2 つ通ってから
    // `ffmpeg.load` を呼ぶようになったため、`nextLoadImpl` が `resolveLoad`
    // を捕まえるまでに microtask を数回進める必要がある。
    await new Promise((r) => setTimeout(r, 0));
    resolveLoad();
    const a = await p1;
    const b = await p2;
    expect(a).toBe(b);
    expect(mockState.instances.length).toBe(1);
  });

  it('load 失敗後に再度呼ぶと new FFmpeg() が再走する (retry 可能状態へ戻る)', async () => {
    const { loadFfmpegAlphaEncoder } = await import('./encodeWebmAlphaWasm');
    mockState.nextLoadImpl = () => Promise.reject(new Error('net down'));
    await expect(loadFfmpegAlphaEncoder()).rejects.toThrow(/net down/);
    // retry
    await expect(loadFfmpegAlphaEncoder()).resolves.toBeDefined();
    expect(mockState.instances.length).toBe(2);
  });

  it('1 回目失敗 / 2 回目成功で singleton が確立される', async () => {
    const { loadFfmpegAlphaEncoder } = await import('./encodeWebmAlphaWasm');
    mockState.nextLoadImpl = () => Promise.reject(new Error('boom'));
    await expect(loadFfmpegAlphaEncoder()).rejects.toThrow();
    const ff1 = await loadFfmpegAlphaEncoder();
    const ff2 = await loadFfmpegAlphaEncoder();
    expect(ff1).toBe(ff2);
    // 1 回目 reject 後 + 2 回目以降は instances[1] が共有される
    expect(mockState.instances.length).toBe(2);
    expect(mockState.instances[1].load).toHaveBeenCalledTimes(1);
  });

  it('ffmpeg.exec に libvpx-vp9 / yuva420p / auto-alt-ref 0 / -s WxH / -framerate fps が含まれる', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    await encodeAnimationAlphaWasm([makeFrame(1)], 128, 64, 30);
    const inst = mockState.instances[0];
    const args = inst.exec.mock.calls[0][0] as string[];
    // 個別フラグの存在チェック
    expect(args).toEqual(expect.arrayContaining(['-c:v', 'libvpx-vp9']));
    expect(args).toEqual(expect.arrayContaining(['-pix_fmt', 'yuva420p']));
    expect(args).toEqual(expect.arrayContaining(['-auto-alt-ref', '0']));
    expect(args).toEqual(expect.arrayContaining(['-s', '128x64']));
    expect(args).toEqual(expect.arrayContaining(['-framerate', '30']));
  });

  it('width / height が `-s WxH` に正確に埋め込まれる (256x384)', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    await encodeAnimationAlphaWasm([makeFrame(1)], 256, 384);
    const args = mockState.instances[0].exec.mock.calls[0][0] as string[];
    const i = args.indexOf('-s');
    expect(i).toBeGreaterThanOrEqual(0);
    expect(args[i + 1]).toBe('256x384');
  });

  it('fps 省略時は ANIM_FPS (24) が使われる', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    const { ANIM_FPS } = await import('./encodeMp4');
    await encodeAnimationAlphaWasm([makeFrame(1)], 16, 16);
    const args = mockState.instances[0].exec.mock.calls[0][0] as string[];
    const i = args.indexOf('-framerate');
    expect(args[i + 1]).toBe(String(ANIM_FPS));
  });

  it('on("progress") の通知が onProgress(round(p*total), total) で中継される', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    const seen: Array<[number, number]> = [];
    mockState.nextExecImpl = async (ff) => {
      for (const h of ff.__progressHandlers) {
        h({ progress: 0.5, time: 0 });
      }
    };
    await encodeAnimationAlphaWasm([makeFrame(1), makeFrame(2)], 16, 16, 24, (f, t) => {
      seen.push([f, t]);
    });
    expect(seen.length).toBeGreaterThan(0);
    // total = 2, progress=0.5 → frame = round(1) = 1
    expect(seen[0]).toEqual([1, 2]);
  });

  it('progress 値は [0, total] にクランプされる (負値 / 超過時も)', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    const seen: Array<[number, number]> = [];
    mockState.nextExecImpl = async (ff) => {
      for (const h of ff.__progressHandlers) {
        h({ progress: 0, time: 0 });
        h({ progress: 0.5, time: 0 });
        h({ progress: 1, time: 0 });
        h({ progress: 1.5, time: 0 });
        h({ progress: -0.2, time: 0 });
      }
    };
    const total = 4;
    await encodeAnimationAlphaWasm(
      [makeFrame(1), makeFrame(2), makeFrame(3), makeFrame(4)],
      16,
      16,
      24,
      (f, t) => seen.push([f, t]),
    );
    for (const [f, t] of seen) {
      expect(t).toBe(total);
      expect(f).toBeGreaterThanOrEqual(0);
      expect(f).toBeLessThanOrEqual(total);
    }
    // 期待値 (round(p*4)): [0, 2, 4, 4 (clamped), 0 (clamped)]
    expect(seen).toEqual([
      [0, 4],
      [2, 4],
      [4, 4],
      [4, 4],
      [0, 4],
    ]);
  });

  it('onProgress 未指定でも progress event で throw しない', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    mockState.nextExecImpl = async (ff) => {
      for (const h of ff.__progressHandlers) {
        h({ progress: 0.5, time: 0 });
      }
    };
    await expect(
      encodeAnimationAlphaWasm([makeFrame(1)], 16, 16),
    ).resolves.toBeInstanceOf(Blob);
  });

  it('成功時に各フレームの deleteFile + outputName deleteFile が呼ばれる', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    const N = 3;
    await encodeAnimationAlphaWasm(
      [makeFrame(1), makeFrame(2), makeFrame(3)],
      16,
      16,
    );
    const inst = mockState.instances[0];
    const deletedNames = inst.deleteFile.mock.calls.map((c) => c[0] as string);
    // pre-cleanup + post-cleanup の両方で呼ばれているはず → 2N + 2 回 (out.webm 含む)
    for (let i = 0; i < N; i++) {
      const name = `frame-${String(i).padStart(4, '0')}.png`;
      expect(deletedNames.filter((n) => n === name).length).toBeGreaterThanOrEqual(1);
    }
    expect(deletedNames).toContain('out.webm');
  });

  it('成功時に ffmpeg.off("progress", handler) が finally で呼ばれる', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    await encodeAnimationAlphaWasm([makeFrame(1)], 16, 16);
    const inst = mockState.instances[0];
    expect(inst.off).toHaveBeenCalledWith('progress', expect.any(Function));
    // on と off の handler が一致 (購読が解除されている)
    const onHandler = inst.on.mock.calls.find((c) => c[0] === 'progress')?.[1];
    const offHandler = inst.off.mock.calls.find((c) => c[0] === 'progress')?.[1];
    expect(onHandler).toBe(offHandler);
  });

  it('exec が throw しても ffmpeg.off("progress", ...) が呼ばれる', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    mockState.nextExecImpl = () => Promise.reject(new Error('exec fail'));
    await expect(
      encodeAnimationAlphaWasm([makeFrame(1)], 16, 16),
    ).rejects.toThrow(/exec fail/);
    const inst = mockState.instances[0];
    expect(inst.off).toHaveBeenCalledWith('progress', expect.any(Function));
  });

  it('readFile が string を返したら Error を投げる (型ガード)', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    mockState.nextReadFileImpl = () => Promise.resolve('unexpected string');
    await expect(
      encodeAnimationAlphaWasm([makeFrame(1)], 16, 16),
    ).rejects.toThrow(/unexpected string/);
  });

  it('pre-cleanup の deleteFile が reject しても全体は成功する', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeWebmAlphaWasm');
    // 最初の load を済ませて、その後 deleteFile を reject に差し替える。
    // (load 内では deleteFile を呼ばないので影響しない)
    // この時点で reset 済みなので 1 回 encode 呼んだ後の状態で reject させたい。
    // → 最初の呼び出しで instance を確保しつつ、その instance の deleteFile を
    //   後付けで reject に変更する方が確実だが、現時点では encodeAnimation の
    //   1 回目 (singleton 構築 + clean pass) で reject させたい。
    // 簡単のため、新 instance 取得直後に直接 deleteFile を rejectMock に置く方式
    // を取る: まず loadFfmpegAlphaEncoder を呼んでから差し替える。
    const { loadFfmpegAlphaEncoder } = await import('./encodeWebmAlphaWasm');
    const ff = (await loadFfmpegAlphaEncoder()) as unknown as MockFFmpeg;
    // listDir で残骸を 1 件返し、pre-cleanup で deleteFile が呼ばれる経路を作る
    ff.listDir.mockResolvedValue([
      { name: 'frame-0000.png', isDir: false },
    ]);
    ff.deleteFile.mockRejectedValue(new Error('not found'));
    await expect(
      encodeAnimationAlphaWasm([makeFrame(1)], 16, 16),
    ).resolves.toBeInstanceOf(Blob);
  });

  it('listDir が落ちても想定名ループで pre-cleanup を継続する (fallback)', async () => {
    const { encodeAnimationAlphaWasm, loadFfmpegAlphaEncoder } =
      await import('./encodeWebmAlphaWasm');
    const ff = (await loadFfmpegAlphaEncoder()) as unknown as MockFFmpeg;
    ff.listDir.mockRejectedValue(new Error('listDir not supported'));
    await expect(
      encodeAnimationAlphaWasm([makeFrame(1), makeFrame(2)], 16, 16),
    ).resolves.toBeInstanceOf(Blob);
    // 想定名 (frame-0000.png / frame-0001.png) で deleteFile が呼ばれている
    const names = ff.deleteFile.mock.calls.map((c) => c[0] as string);
    expect(names).toContain('frame-0000.png');
    expect(names).toContain('frame-0001.png');
  });

  it('listDir で発見した frame-*.png 残骸を pre-cleanup で削除する', async () => {
    const { encodeAnimationAlphaWasm, loadFfmpegAlphaEncoder } =
      await import('./encodeWebmAlphaWasm');
    const ff = (await loadFfmpegAlphaEncoder()) as unknown as MockFFmpeg;
    // 前回 5 フレーム、今回 2 フレームのケース。古い 3 本も消えてほしい。
    ff.listDir.mockResolvedValueOnce([
      { name: 'frame-0000.png', isDir: false },
      { name: 'frame-0001.png', isDir: false },
      { name: 'frame-0002.png', isDir: false },
      { name: 'frame-0003.png', isDir: false },
      { name: 'frame-0004.png', isDir: false },
      { name: 'unrelated.txt', isDir: false },
      { name: 'tmpdir', isDir: true },
    ]);
    await encodeAnimationAlphaWasm([makeFrame(1), makeFrame(2)], 16, 16);
    const deleted = ff.deleteFile.mock.calls.map((c) => c[0] as string);
    expect(deleted).toContain('frame-0002.png');
    expect(deleted).toContain('frame-0003.png');
    expect(deleted).toContain('frame-0004.png');
    // 関係ないファイル / ディレクトリには触らない
    expect(deleted).not.toContain('unrelated.txt');
    expect(deleted).not.toContain('tmpdir');
  });

  it('並行に 2 回呼んでも内部 mutex で直列化される (2 つ目の writeFile は 1 つ目の deleteFile より後)', async () => {
    const { encodeAnimationAlphaWasm, loadFfmpegAlphaEncoder } =
      await import('./encodeWebmAlphaWasm');
    const ff = (await loadFfmpegAlphaEncoder()) as unknown as MockFFmpeg;

    // 操作順をタイムラインに記録する。
    const timeline: string[] = [];
    ff.writeFile.mockImplementation(async (name: string) => {
      timeline.push(`write:${name}`);
    });
    ff.deleteFile.mockImplementation(async (name: string) => {
      timeline.push(`delete:${name}`);
    });
    // 1 つ目の exec を 1 tick 遅延させて、もし直列化されていなければ
    // 2 つ目の writeFile が割り込めるようにする。
    let firstExec = true;
    ff.exec.mockImplementation(async () => {
      if (firstExec) {
        firstExec = false;
        await new Promise((r) => setTimeout(r, 5));
      }
      timeline.push('exec');
    });

    const p1 = encodeAnimationAlphaWasm([makeFrame(1)], 16, 16);
    const p2 = encodeAnimationAlphaWasm([makeFrame(2)], 16, 16);
    await Promise.all([p1, p2]);

    // 2 つ目の最初の writeFile (frame-0000.png) のインデックスは
    // 1 つ目の post-cleanup delete (frame-0000.png) より後でなければならない。
    // 1 つ目の post-cleanup 内に "delete:frame-0000.png" が 2 回現れる
    // (pre-cleanup は listDir が [] を返すので走らない) ので "delete:out.webm"
    // をマーカーにする: これが 1 つ目の最後の操作。
    const firstOutDeleteIdx = timeline.indexOf('delete:out.webm');
    const lastWriteIdx = timeline.lastIndexOf('write:frame-0000.png');
    expect(firstOutDeleteIdx).toBeGreaterThanOrEqual(0);
    expect(lastWriteIdx).toBeGreaterThan(firstOutDeleteIdx);
  });
});

describe('prefetchFfmpegCore', () => {
  it('呼ぶと FFMPEG_CORE_URL / FFMPEG_WASM_URL に対して cors-mode (= mode 未指定) で fetch を発火する (#184 review M1: opaque を焼かない)', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response('', { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    try {
      const { prefetchFfmpegCore, FFMPEG_CORE_URL, FFMPEG_WASM_URL } = await import(
        './encodeWebmAlphaWasm'
      );
      prefetchFfmpegCore();
      expect(fetchMock).toHaveBeenCalledTimes(2);
      const urls = fetchMock.mock.calls.map((c) => c[0]);
      expect(urls).toContain(FFMPEG_CORE_URL);
      expect(urls).toContain(FFMPEG_WASM_URL);
      for (const call of fetchMock.mock.calls) {
        const init = (call[1] ?? {}) as RequestInit;
        // mode は明示指定せず、ブラウザ既定 (cors) に委ねる。
        // 'no-cors' で opaque response を SW cache に焼くと
        // importScripts / streaming compile が壊れる (#184 review M1)。
        expect(init.mode).toBeUndefined();
        expect(init.credentials).toBe('omit');
      }
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('プリフェッチが pending 中に loadFfmpegAlphaEncoder を呼ぶと、プリフェッチ完了後に ffmpeg.load が走る (#184 review S1: 二重 fetch race 解消)', async () => {
    // プリフェッチ fetch を pending のまま手で解決できるよう deferred を作る。
    let resolveCore: (r: Response) => void = () => {};
    let resolveWasm: (r: Response) => void = () => {};
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url.endsWith('.wasm')) {
        return new Promise<Response>((res) => {
          resolveWasm = res;
        });
      }
      return new Promise<Response>((res) => {
        resolveCore = res;
      });
    });
    vi.stubGlobal('fetch', fetchMock);
    try {
      const { prefetchFfmpegCore, loadFfmpegAlphaEncoder } = await import(
        './encodeWebmAlphaWasm'
      );
      prefetchFfmpegCore();
      expect(fetchMock).toHaveBeenCalledTimes(2);

      // ffmpeg.load を一度 pending にして、プリフェッチ完了前に解決していない
      // ことをタイムラインで確認する。
      const timeline: string[] = [];
      let resolveLoad: () => void = () => {};
      mockState.nextLoadImpl = () => {
        timeline.push('load:start');
        return new Promise<void>((res) => {
          resolveLoad = () => {
            timeline.push('load:resolved');
            res();
          };
        });
      };

      // プリフェッチが pending のまま load を呼ぶ。
      const loadP = loadFfmpegAlphaEncoder();
      // microtask を 1 回挟んで load 内部の await prefetchPromise が走る機会を作る。
      await new Promise((r) => setTimeout(r, 0));
      // この時点では `ffmpeg.load` はまだ呼ばれていない (プリフェッチ完走待ち)。
      expect(timeline).not.toContain('load:start');

      // プリフェッチ完了 → 続いて ffmpeg.load が走る。
      resolveCore(new Response('', { status: 200 }));
      resolveWasm(new Response('', { status: 200 }));
      // microtask を進めて load 開始まで到達させる。
      await new Promise((r) => setTimeout(r, 0));
      await new Promise((r) => setTimeout(r, 0));
      expect(timeline).toContain('load:start');

      resolveLoad();
      await loadP;
      expect(timeline).toEqual(['load:start', 'load:resolved']);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('fetch が reject してもエラーを伝播しない (catch で握りつぶす)', async () => {
    const fetchMock = vi.fn().mockRejectedValue(new Error('offline'));
    vi.stubGlobal('fetch', fetchMock);
    try {
      const { prefetchFfmpegCore } = await import('./encodeWebmAlphaWasm');
      // throw しなければ OK。reject Promise が unhandled になる前に await で吸う。
      expect(() => prefetchFfmpegCore()).not.toThrow();
      // catch ハンドラが回るのを待つ
      await new Promise((r) => setTimeout(r, 0));
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('シングルトン状態 (ffmpegLoadPromise / ffmpegSingleton) には影響しない — 直後の load も new instance を作る', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response('', { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    try {
      const { prefetchFfmpegCore, loadFfmpegAlphaEncoder } = await import(
        './encodeWebmAlphaWasm'
      );
      prefetchFfmpegCore();
      // プリフェッチでは FFmpeg インスタンスは作られない
      expect(mockState.instances.length).toBe(0);
      // 直後に loadFfmpegAlphaEncoder を呼ぶと改めて 1 つ作られる (プリフェッチが
      // singleton を奪っていない証拠)
      await loadFfmpegAlphaEncoder();
      expect(mockState.instances.length).toBe(1);
      expect(mockState.instances[0].load).toHaveBeenCalledTimes(1);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
