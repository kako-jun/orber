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
import { encodeAnimationAlphaWasm } from './encodeAlphaVideoWasm';
import { ANIM_FPS } from './encodeMp4';

interface PendingResolver {
  resolve: (v: unknown) => void;
  reject: (e: unknown) => void;
  // #95: animateOne のフレーム単位進捗。worker からは本体応答とは別の
  // 'animateProgress' kind の message が届くので、その経路で発火する。
  // pending は消さない（resolve は本体メッセージで行う）。
  onProgress?: (frame: number, total: number) => void;
  // #184: 透過動画用の per-frame PNG buffer を受け取る callback。
  // worker は `alphaFrame` kind の message で 1 枚ずつ送ってくる。
  onAlphaFrame?: (frame: number, total: number, data: ArrayBuffer) => void;
  // #108: 元リクエストの kind。`hasInFlight()` で init / setSource を
  // 「ユーザー操作の進行中扱い」から除外して、再ガチャ判定の精度を上げる
  // ために使う。init は wasm 起動だけ、setSource はキャッシュ更新だけで
  // 旧 run の 12 個生成とは無関係。
  kind: string;
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
  // Phase B (#55): UI から流れる advanced 軸。空文字 / 省略は "未指定（既存挙動）"。
  glyph_char?: string;
  count_preset?: string;
  speed_preset?: string;
  softness_preset?: string;
  // #136: Glyph 回転 ON/OFF。`true` 既定で従来挙動、`false` で静止描画。
  glyph_rotate?: boolean;
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

// #108 (review s1): pending を全件 reject + clear する内部ヘルパ。
// crash パスと terminateAndRespawn の両方で使い、reject 文言と
// 副作用順序の食い違いを防ぐ。
function drainPending(reason: string): void {
  for (const [, p] of pending) p.reject(new Error(reason));
  pending.clear();
}

function ensureWorker(): Worker {
  if (worker) return worker;
  const w = new OrberWorker();
  w.addEventListener('message', (e: MessageEvent) => {
    // #95: 応答メッセージは 2 種類の union。
    //   - 本体応答: `kind` プロパティなし。`{ id, ok, data?, error? }`
    //   - 進捗通知: `{ kind: 'animateProgress', id, frame, total }`
    // 将来 kind が増えるなら明示分岐を追加すること。
    type RespResult = { id: number; ok: boolean; data?: unknown; error?: string };
    type RespProgress = { kind: 'animateProgress'; id: number; frame: number; total: number };
    type RespAlphaFrame = {
      kind: 'alphaFrame';
      id: number;
      frame: number;
      total: number;
      data: ArrayBuffer;
    };
    type Resp = RespResult | RespProgress | RespAlphaFrame;
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
    if ('kind' in msg && msg.kind === 'alphaFrame') {
      const pp = pending.get(msg.id);
      if (pp && pp.onAlphaFrame) {
        try {
          pp.onAlphaFrame(msg.frame, msg.total, msg.data);
        } catch (err) {
          console.error('orber onAlphaFrame callback failed', err);
        }
      }
      return;
    }
    const { id, ok, data, error } = msg as RespResult;
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
    drainPending('worker crashed');
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
  req: Record<string, unknown> & { kind: string },
  transfers: Transferable[] = [],
  onProgress?: (frame: number, total: number) => void,
  onAlphaFrame?: (frame: number, total: number, data: ArrayBuffer) => void,
): Promise<T> {
  const w = ensureWorker();
  const id = ++nextId;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, {
      resolve: resolve as (v: unknown) => void,
      reject: reject as (e: unknown) => void,
      onProgress,
      onAlphaFrame,
      kind: req.kind,
    });
    w.postMessage({ ...req, id }, transfers);
  });
}

/** Worker を起動して wasm を初期化する。複数回呼んでも安全（worker 側で冪等）。 */
export async function workerInit(): Promise<void> {
  await call<void>({ kind: 'init' });
}

/**
 * runBatch の 12 個生成系 RPC（generateOne / animateOne）が in-flight
 * かどうか。init / setSource は除外する。
 *
 * #108 review m1: onMount の `workerInit()` が解決する前に decode が
 * 終わって runBatch に入るレース（軽い PNG + 低速デバイス等）で、
 * 単純な `pending.size > 0` だと init pending を巻き添え reject して
 * しまい wasmStatus が 'error' で固着する。kind フィルタで「実作業」
 * の有無だけを判定する。
 */
export function hasInFlight(): boolean {
  for (const [, p] of pending) {
    if (p.kind === 'generateOne' || p.kind === 'animateOne') return true;
  }
  return false;
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
  drainPending('worker terminated for new run');
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

/**
 * Phase B (#55): UI が glyph 文字の収録状況を確認するための問い合わせ。
 * `glyph_supported(NotoSymbols2, ch)` をそのまま返す。空文字や複数 char は
 * 先頭 char のみで判定される（UI 側で 1 char 制限済みの想定）。
 */
export async function workerGlyphSupported(ch: string): Promise<boolean> {
  return await call<boolean>({ kind: 'glyphSupported', ch });
}

/**
 * #160: shape='image' で使う画像を worker に渡してキャッシュさせる。
 * `file` は structured-clone で worker に複製される (Transferable は使わ
 * ない)。これでメインスレッド側に File への参照が残り、worker クラッシュ
 * 後の再 upload が可能になる。worker 側で `createImageBitmap(file)` を
 * 呼んで ImageBitmap 化し、古い bitmap は close() される。
 *
 * #181: 旧 invert 引数 (#170) は #174 のレタボ修正後に効果が視認できない
 * レベルになり削除。auto-polarity (= 少数派 = 被写体) のみで判定する。
 */
export async function workerSetImageShape(file: File): Promise<void> {
  await call<void>({ kind: 'setImageShape', file });
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

/**
 * #56: 1 タイルぶんの透過 PNG または透過 WebP を返す。non-alpha 版と同じ params /
 * n / index を渡すと、worker 側で bg.a だけを 0 に上書きしてレンダリングする。
 * `format = 'png'` は lossless、`'webp'` は quality 0.9 lossy（30-50% 圧縮）。
 */
export async function workerGenerateOneAlpha(
  params: BaseParams,
  n: number,
  index: number,
  format: 'png' | 'webp',
): Promise<Blob> {
  const buf = await call<ArrayBuffer>({
    kind: 'generateOneAlpha',
    params,
    n,
    index,
    format,
  });
  const mime = format === 'webp' ? 'image/webp' : 'image/png';
  return new Blob([buf], { type: mime });
}

/**
 * #184: 1 タイルぶんの透過 WebM (libvpx-vp9 + yuva420p) を返す。
 *
 * worker は wasm + OffscreenCanvas で各 frame を透過 PNG として描画し、
 * `alphaFrame` message で 1 枚ずつ main に流す。main 側で集めた PNG を
 * ffmpeg.wasm に投入し libvpx-vp9 + yuva420p で muxing する。
 *
 * 旧 WebCodecs `VideoEncoder({codec:'vp09.00.10.08', alpha:'keep'})` 経路は
 * Edge / Android Chrome 等多くの環境で supported:false を返したため撤去。
 * ffmpeg.wasm は内蔵 libvpx-vp9 を使うため、ブラウザ / OS / GPU の codec
 * backend に依存せず全環境で確実に出力できる。
 *
 * ffmpeg.wasm のロード失敗 (ネットワーク断 / 配信欠落) は Error が伝播する。
 * 呼び出し側は `alphaEncoderLoadFailed` 文言の UI で通知すること。
 */
export async function workerAnimateOneAlpha(
  params: BaseParams,
  n: number,
  index: number,
  totalFrames: number,
  onProgress?: (frame: number, total: number) => void,
): Promise<Blob> {
  // PNG フレームを順序保証付きで収集する。worker は serial に送ってくる
  // 想定 (worker case 'renderAlphaFrames' の for ループは index 順) なので
  // 配列の単純 push で OK。順序の念のための保険として frame index も保存する。
  const frames: Uint8Array[] = new Array(totalFrames);
  await call<void>(
    {
      kind: 'renderAlphaFrames',
      params,
      n,
      index,
      totalFrames,
    },
    [],
    onProgress,
    (frame, _total, data) => {
      frames[frame] = new Uint8Array(data);
    },
  );
  // 欠損チェック (worker が途中で post を取りこぼした場合のセーフティ)。
  for (let i = 0; i < totalFrames; i++) {
    if (!frames[i]) {
      throw new Error(
        `missing alpha frame ${i}/${totalFrames} from worker (animateOneAlpha)`,
      );
    }
  }
  return await encodeAnimationAlphaWasm(
    frames,
    params.width,
    params.height,
    ANIM_FPS,
    onProgress,
  );
}
