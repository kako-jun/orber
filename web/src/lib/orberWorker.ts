// orber#75 — wasm 描画 + WebCodecs エンコードを Worker スレッドで実行する。
//
// メインスレッドは UI / DOM / Solid signal だけに集中させ、重い計算は全部
// ここに逃がす。これによりスマホでも生成中にスクロール / タップが死なない。
//
// アーキテクチャ:
//   main → postMessage({ kind, id, ... }) → worker
//   worker → wasm 呼び出し → postMessage({ id, ok, data }) → main
//
// データ転送:
//   - PNG / mp4 の ArrayBuffer は Transferable で zero-copy 返却
//   - source RGB は `setSource` で 1 度だけ送って worker 側にキャッシュする
//     （runBatch / DL / アスペクト切替で使い回し）
//
// 互換性: OffscreenCanvas / VideoEncoder / VideoFrame in Worker が要る。
// iOS Safari 16.4+ / Android Chrome / 最近の Firefox。古い端末は対象外
// （isWebCodecsSupported() のメイン側チェックで弾く前提）。

import init, * as wasm from '../wasm/orber_wasm.js';
import { encodeAnimationToMp4 } from './encodeMp4';

// レビュー M6: 複数メッセージが同時に到着すると ensureInit が並行に走り、
// init() が重複実行される。Promise をキャッシュして 1 度きりにする。
let initialized = false;
let initPromise: Promise<void> | null = null;
function ensureInit(): Promise<void> {
  if (initialized) return Promise.resolve();
  if (!initPromise) {
    initPromise = init().then(() => {
      initialized = true;
    });
  }
  return initPromise;
}

// 入力画像の RGB バッファを worker 側にキャッシュ。同じ画像で複数回 wasm を
// 呼ぶ（12 枚生成 / DL hi-res 12 枚）ので、毎回 postMessage で送るのは無駄。
// `setSource` で 1 度送って、以降の generateOne / animateOne はキャッシュを
// 自動で混ぜ込む。
let cachedSource: { rgb: Uint8Array; width: number; height: number } | null = null;

interface BaseParams {
  k: number;
  width: number;
  height: number;
  seed: number;
  direction: string;
  speed: string;
  count: number;
  orb_size: number;
  blur: number;
  shape: string;
}

function mergeParams(p: BaseParams) {
  if (!cachedSource) {
    throw new Error('source not set — call setSource before generate/animate');
  }
  return {
    ...p,
    source_rgb: cachedSource.rgb,
    source_width: cachedSource.width,
    source_height: cachedSource.height,
  };
}

type Req =
  | { kind: 'init'; id: number }
  | {
      kind: 'setSource';
      id: number;
      rgb: Uint8Array;
      width: number;
      height: number;
    }
  | { kind: 'generateOne'; id: number; params: BaseParams; n: number; index: number }
  | {
      kind: 'animateOne';
      id: number;
      params: BaseParams;
      n: number;
      index: number;
      totalFrames: number;
    };

function post(msg: unknown, transfers: Transferable[] = []) {
  (self as unknown as Worker).postMessage(msg, transfers);
}

self.addEventListener('message', async (e: MessageEvent<Req>) => {
  const req = e.data;
  try {
    await ensureInit();
    switch (req.kind) {
      case 'init': {
        post({ id: req.id, ok: true });
        break;
      }
      case 'setSource': {
        cachedSource = { rgb: req.rgb, width: req.width, height: req.height };
        post({ id: req.id, ok: true });
        break;
      }
      case 'generateOne': {
        const params = mergeParams(req.params);
        const png = wasm.generate_one_at_index(params, req.n, req.index);
        // png.buffer は wasm 線形メモリへの view → そのまま postMessage で
        // Transferable に渡すと wasm 側のメモリが detach されて壊れる。
        // slice() で新規 ArrayBuffer を作って Transferable で main に渡す。
        const buf = png.slice().buffer;
        post({ id: req.id, ok: true, data: buf }, [buf]);
        break;
      }
      case 'animateOne': {
        const params = mergeParams(req.params);
        const handle = wasm.start_animation_for_batch_spec(
          params,
          req.n,
          req.index,
          req.totalFrames,
        );
        try {
          // #95: フレーム単位の進捗を main に流す。本体応答（id + ok）と
          // 別 kind で送るので、main 側は pending を消さずに onProgress
          // だけ発火させる経路で受ける。
          const blob = await encodeAnimationToMp4(handle, (frame, total) => {
            post({ kind: 'animateProgress', id: req.id, frame, total });
          });
          const buf = await blob.arrayBuffer();
          post({ id: req.id, ok: true, data: buf }, [buf]);
        } finally {
          handle.free?.();
        }
        break;
      }
      default: {
        // 網羅性チェック。型システムが req: never を要求する。
        const exhaustive: never = req;
        throw new Error(`unknown req kind: ${JSON.stringify(exhaustive)}`);
      }
    }
  } catch (err) {
    post({ id: (req as { id?: number }).id ?? -1, ok: false, error: String(err) });
  }
});
