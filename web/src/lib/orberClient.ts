// orber#75 — orberWorker のメインスレッド側クライアント。
//
// Worker をシングルトンで起動し、postMessage を Promise 化した RPC を提供する。
// id 採番で複数の in-flight 呼び出しを区別し、各 Promise の resolve / reject
// を pending Map で照合する。
//
// 設計上の選択:
// - Worker は channel ごとにシングルトンを使い回す（wasm 初期化コストを償却）
// - source RGB は `workerSetSource` で 1 度だけ送り、以降の呼び出しは
//   index と spec パラメータだけ送る（毎回 RGB 数 MB を送らない）
// - mp4 / PNG の ArrayBuffer は Transferable で worker → main を zero-copy
//
// #92: 静止画生成と動画化を並走させるため worker を 2 本（'still' / 'video'）に分割。
// channel 'still' = 静止画（generateOne）、channel 'video' = 動画化（animateOne）。
// 各 worker は独立に wasm 初期化 + source RGB キャッシュを持つので、
// `workerInit` / `workerSetSource` は両方に同じ内容を流す（Promise.all）。
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

// #92: チャンネル識別子。'still' は静止画、'video' は動画化に固定割当。
type Channel = 'still' | 'video';

interface ChannelState {
  worker: Worker | null;
  nextId: number;
  pending: Map<number, PendingResolver>;
}

const channels: Record<Channel, ChannelState> = {
  still: { worker: null, nextId: 0, pending: new Map() },
  video: { worker: null, nextId: 0, pending: new Map() },
};

// レビュー M3 + N4: Worker クラッシュで再生成すると wasm 未初期化 +
// cachedSource 未設定の状態に戻る。main 側で「同じ画像なら setSource
// 再送しない」最適化が破綻するので、クラッシュ通知を Studio に流して
// `lastSourceRef` をリセットさせる。同時に UI にエラー表示する導線にも使う。
// #92: どちらの channel が落ちても発火する。意味は今までと同じ「source
// キャッシュをリセットしろ」（両 worker が同じ source を持つ前提が崩れるため）。
// 現状は両 channel どちらが落ちても同じ callback を発火する。将来 channel 別
// の最適化（例: video 側だけ落ちても still 側は生かす）を入れるなら、
// callback 引数に channel を渡すよう拡張する余地を残してある。
type CrashCallback = () => void;
const crashCallbacks: CrashCallback[] = [];
export function onWorkerCrash(cb: CrashCallback): () => void {
  crashCallbacks.push(cb);
  return () => {
    const idx = crashCallbacks.indexOf(cb);
    if (idx >= 0) crashCallbacks.splice(idx, 1);
  };
}

// レビュー M1: ensureWorker の error ハンドラと workerSetSource の
// 片側失敗リカバリで同じ「pending 全 reject + worker terminate + worker = null
// + crashCallbacks 発火」を行うため、共通関数として切り出した。
function invalidateWorker(ch: Channel): void {
  const st = channels[ch];
  for (const [, p] of st.pending) p.reject(new Error('worker crashed'));
  st.pending.clear();
  if (st.worker) {
    try {
      st.worker.terminate();
    } catch (err) {
      console.error(`orber worker[${ch}] terminate failed`, err);
    }
    st.worker = null;
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
}

function ensureWorker(ch: Channel): Worker {
  const st = channels[ch];
  if (st.worker) return st.worker;
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
      const pp = st.pending.get(msg.id);
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
    const p = st.pending.get(id);
    if (!p) return;
    st.pending.delete(id);
    if (ok) p.resolve(data);
    else p.reject(new Error(error ?? 'worker error (no message)'));
  });
  w.addEventListener('error', (e) => {
    // Worker 全体が落ちると個別の id 照合では拾えないので、pending 全部に
    // reject を流して呼び出し側が例外で気づけるようにする。
    console.error(`orber worker[${ch}] fatal error`, e);
    invalidateWorker(ch);
  });
  st.worker = w;
  return w;
}

function call<T>(
  ch: Channel,
  req: Record<string, unknown>,
  transfers: Transferable[] = [],
  onProgress?: (frame: number, total: number) => void,
): Promise<T> {
  const w = ensureWorker(ch);
  const st = channels[ch];
  const id = ++st.nextId;
  return new Promise<T>((resolve, reject) => {
    st.pending.set(id, {
      resolve: resolve as (v: unknown) => void,
      reject: reject as (e: unknown) => void,
      onProgress,
    });
    w.postMessage({ ...req, id }, transfers);
  });
}

/** Worker を起動して wasm を初期化する。複数回呼んでも安全（worker 側で冪等）。 */
export async function workerInit(): Promise<void> {
  // #92: 両 worker を並行で初期化。どちらかが失敗したら reject される。
  await Promise.all([
    call<void>('still', { kind: 'init' }),
    call<void>('video', { kind: 'init' }),
  ]);
}

/**
 * 入力画像の RGB バッファを Worker にキャッシュ。
 * 以降の `workerGenerateOne` / `workerAnimateOne` はこのキャッシュを使う。
 *
 * postMessage の structuredClone で worker 側に複製される（Transferable は
 * 使わない）。main 側 `decoded()` signal の整合性を壊さないため。コピーは
 * 1 画像につき 1 度しか発生しないので RGB 数 MB のコピーは許容コスト。
 *
 * #92: 両 worker に同じ RGB を送る（structuredClone で別コピーになる）。
 */
export async function workerSetSource(
  rgb: Uint8Array,
  width: number,
  height: number,
): Promise<void> {
  // レビュー M1: Promise.all は片方が reject してももう片方は走り続けるので、
  // 「片側だけ source が乗った」状態になり整合性が壊れる（次回の generate /
  // animate でどちらかだけ古い source / 未設定で動く）。失敗時は両 channel
  // を invalidate して、次回 ensureWorker で再生成 + crashCallbacks 経由で
  // Studio 側 lastSourceRef リセット → setSource 再送 を発火させる。
  try {
    await Promise.all([
      call<void>('still', { kind: 'setSource', rgb, width, height }),
      call<void>('video', { kind: 'setSource', rgb, width, height }),
    ]);
  } catch (e) {
    invalidateWorker('still');
    invalidateWorker('video');
    throw e;
  }
}

/** 1 タイル分の PNG を hi-res / lo-res のどちらでも返す。 */
export async function workerGenerateOne(
  params: BaseParams,
  n: number,
  index: number,
): Promise<Uint8Array> {
  // #92: 静止画は channel 'still' に固定。
  const buf = await call<ArrayBuffer>('still', { kind: 'generateOne', params, n, index });
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
  // #92: 動画化は channel 'video' に固定。channel 'still' の静止画ループと並走する。
  // #95: onProgress を渡すと encodeAnimationToMp4 のフレームループから
  // フレーム単位の進捗が流れてくる。省略時は従来どおり何も発火しない。
  const buf = await call<ArrayBuffer>(
    'video',
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
