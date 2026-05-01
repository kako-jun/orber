// orber#75 — orberWorker のメインスレッド側クライアント。
//
// Worker をシングルトンで起動し、postMessage を Promise 化した RPC を提供する。
// id 採番で複数の in-flight 呼び出しを区別し、各 Promise の resolve / reject
// を pending Map で照合する。
//
// 設計上の選択:
// - Worker はシングルトンを使い回す（wasm 初期化コストを償却）
// - source RGB は `workerSetSource` で 1 度だけ送り、以降の呼び出しは
//   index と spec パラメータだけ送る（毎回 RGB 数 MB を送らない）
// - mp4 / PNG の ArrayBuffer は Transferable で worker → main を zero-copy
//
// #99: 一時期 #92 で worker を 'still' / 'video' に分割して並走させていたが、
// 実機テストで「静止画 1〜12 の表示が遅くなる」「並走によるレース」等の
// リグレッションが出たためロールバック。本ファイルは worker 1 本のシングルトン
// 構成に戻している。動画化中の進捗 message（#95）と onProgress 経路、
// および応答メッセージの type union は維持する。
//
// レビュー N2: `call()` の `transfers` 引数は現状未使用だが、将来の最適化
// （main → worker への大きな ArrayBuffer を zero-copy で渡す等）のため
// signature は維持する。

import OrberWorker from './orberWorker?worker';

interface PendingResolver {
  resolve: (v: unknown) => void;
  reject: (e: unknown) => void;
  // #95: animateOne のフレーム単位進捗。worker からは本体応答とは別の
  // 'animateProgress' kind の message が届くので、その経路で発火する。
  // pending は消さない（resolve は本体メッセージで行う）。
  onProgress?: (frame: number, total: number) => void;
}

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

let worker: Worker | null = null;
let nextId = 0;
const pending = new Map<number, PendingResolver>();

// レビュー M3 + N4: Worker クラッシュで再生成すると wasm 未初期化 +
// cachedSource 未設定の状態に戻る。main 側で「同じ画像なら setSource
// 再送しない」最適化が破綻するので、クラッシュ通知を Studio に流して
// `lastSourceRef` をリセットさせる。同時に UI にエラー表示する導線にも使う。
type CrashCallback = () => void;
const crashCallbacks: CrashCallback[] = [];
export function onWorkerCrash(cb: CrashCallback): () => void {
  crashCallbacks.push(cb);
  return () => {
    const idx = crashCallbacks.indexOf(cb);
    if (idx >= 0) crashCallbacks.splice(idx, 1);
  };
}

function ensureWorker(): Worker {
  if (worker) return worker;
  const w = new OrberWorker();
  w.addEventListener('message', (e: MessageEvent) => {
    // #95: 応答メッセージは 2 種類の union。
    //   - 本体応答: `kind` プロパティなし。`{ id, ok, data?, error? }`
    //   - 進捗通知: `{ kind: 'animateProgress', id, frame, total }`
    // 将来 kind が増えるなら明示分岐を追加すること。
    type Resp =
      | { id: number; ok: boolean; data?: unknown; error?: string }
      | { kind: 'animateProgress'; id: number; frame: number; total: number };
    const msg = e.data as Resp;
    if ('kind' in msg && msg.kind === 'animateProgress') {
      const pp = pending.get(msg.id);
      if (pp && pp.onProgress) {
        try {
          pp.onProgress(msg.frame, msg.total);
        } catch (err) {
          console.error('orber onProgress callback failed', err);
        }
      }
      return;
    }
    const { id, ok, data, error } = msg;
    const p = pending.get(id);
    if (!p) return;
    pending.delete(id);
    if (ok) p.resolve(data);
    else p.reject(new Error(error ?? 'worker error (no message)'));
  });
  w.addEventListener('error', (e) => {
    // Worker 全体が落ちると個別の id 照合では拾えないので、pending 全部に
    // reject を流して呼び出し側が例外で気づけるようにする。
    console.error('orber worker fatal error', e);
    for (const [, p] of pending) p.reject(new Error('worker crashed'));
    pending.clear();
    if (worker) {
      try {
        worker.terminate();
      } catch (err) {
        console.error('orber worker terminate failed', err);
      }
      worker = null;
    }
    // クラッシュ通知。次回 ensureWorker で新 worker が立つので、購読者は
    // setSource キャッシュなどのリセットを行うこと。
    for (const cb of crashCallbacks) {
      try {
        cb();
      } catch (err) {
        console.error('orber worker crash callback failed', err);
      }
    }
  });
  worker = w;
  return w;
}

function call<T>(
  req: Record<string, unknown>,
  transfers: Transferable[] = [],
  onProgress?: (frame: number, total: number) => void,
): Promise<T> {
  const w = ensureWorker();
  const id = ++nextId;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, {
      resolve: resolve as (v: unknown) => void,
      reject: reject as (e: unknown) => void,
      onProgress,
    });
    w.postMessage({ ...req, id }, transfers);
  });
}

/** Worker を起動して wasm を初期化する。複数回呼んでも安全（worker 側で冪等）。 */
export async function workerInit(): Promise<void> {
  await call<void>({ kind: 'init' });
}

/** in-flight な RPC が 1 つでもあれば true。 */
export function hasInFlight(): boolean {
  return pending.size > 0;
}

/**
 * #108: Worker を物理的に terminate して新しい worker で再初期化する。
 *
 * `runBatch` 連打時に旧 run の wasm 同期呼び出し（generate_one_at_index）と
 * WebCodecs encode ループを **本当に止める** 唯一の確実な手段。論理的中断
 * （runGen ガード）では旧 12 個が完走するまで CPU が二重に走り、新 run の
 * 開始が遅延する。
 *
 * - pending は全て reject し、呼び出し側の await に例外を流す
 *   （呼び出し側は myGen ガードで吸収する）
 * - worker.terminate() で wasm 同期処理含めて殺す
 * - 新しい worker を立てて wasm を再初期化（数百 ms）
 *
 * 注意: terminate 後は worker 側の cachedSource も消えるので、呼び出し側で
 * `lastSourceRef = null` 等のキャッシュ無効化を行うこと（onWorkerCrash の
 * 経路と同じ）。
 */
export async function terminateAndRespawn(): Promise<void> {
  if (!worker) {
    await workerInit();
    return;
  }
  for (const [, p] of pending) {
    p.reject(new Error('worker terminated for new run'));
  }
  pending.clear();
  try {
    worker.terminate();
  } catch (err) {
    console.error('orber worker terminate failed', err);
  }
  worker = null;
  // 連打時のみ払うコスト。新 worker を立てて wasm 再初期化まで終わらせる。
  await workerInit();
}

/**
 * 入力画像の RGB バッファを Worker にキャッシュ。
 * 以降の `workerGenerateOne` / `workerAnimateOne` はこのキャッシュを使う。
 *
 * postMessage の structuredClone で worker 側に複製される（Transferable は
 * 使わない）。main 側 `decoded()` signal の整合性を壊さないため。コピーは
 * 1 画像につき 1 度しか発生しないので RGB 数 MB のコピーは許容コスト。
 */
export async function workerSetSource(
  rgb: Uint8Array,
  width: number,
  height: number,
): Promise<void> {
  await call<void>({ kind: 'setSource', rgb, width, height });
}

/** 1 タイル分の PNG を hi-res / lo-res のどちらでも返す。 */
export async function workerGenerateOne(
  params: BaseParams,
  n: number,
  index: number,
): Promise<Uint8Array> {
  const buf = await call<ArrayBuffer>({ kind: 'generateOne', params, n, index });
  return new Uint8Array(buf);
}

/** 1 タイル分の mp4 を返す（WebCodecs + mp4-muxer で h264 化）。 */
export async function workerAnimateOne(
  params: BaseParams,
  n: number,
  index: number,
  totalFrames: number,
  onProgress?: (frame: number, total: number) => void,
): Promise<Blob> {
  // #95: onProgress を渡すと encodeAnimationToMp4 のフレームループから
  // フレーム単位の進捗が流れてくる。省略時は従来どおり何も発火しない。
  const buf = await call<ArrayBuffer>(
    {
      kind: 'animateOne',
      params,
      n,
      index,
      totalFrames,
    },
    [],
    onProgress,
  );
  return new Blob([buf], { type: 'video/mp4' });
}
