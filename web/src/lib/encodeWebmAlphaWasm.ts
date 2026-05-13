// orber#184 — 透過 WebM エンコードを ffmpeg.wasm + libvpx-vp9 (yuva420p) で行う。
//
// 旧 `encodeWebmAlpha.ts` は WebCodecs の `VideoEncoder({codec:'vp09.00.10.08',
// alpha:'keep'})` を使っていたが、Edge / Android Chrome 等の大多数の環境で
// `VideoEncoder.isConfigSupported({codec, alpha:'keep'})` が `supported:false`
// を返し実用にならなかった (#56 / session 412)。
//
// ここでは ffmpeg.wasm を導入し、libvpx-vp9 のソフトウェアエンコーダで
// 透過 WebM (yuva420p) を生成する。wasm 内部に libvpx-vp9 を内蔵しているため
// ブラウザ / OS / GPU の codec backend に一切依存せず、全環境で確実に出力できる。
//
// 設計:
//   - main thread で動かす (worker 内では `FFmpeg` class が更に内部 worker を
//     spawn するため nested worker 互換性問題を避ける。single-threaded core は
//     COOP/COEP ヘッダ不要なので Nostalgic Counter iframe 等を壊さない)
//   - `FFmpeg` インスタンスは module 内シングルトンで lazy 初期化
//     (透過 DL を複数回使ってもロードは 1 度だけ)
//   - core は同一オリジン `/ffmpeg/ffmpeg-core.{js,wasm}` から取得
//     (CDN ロードを避け外部依存を増やさない)
//   - 1 frame ずつ PNG として virtual FS に書き込み、`ffmpeg.exec()` で
//     `-c:v libvpx-vp9 -pix_fmt yuva420p -auto-alt-ref 0` で WebM 化
//
// `-auto-alt-ref 0` は VP9 alpha と同時に使えない (libvpx 制約) ので必須。

import { FFmpeg } from '@ffmpeg/ffmpeg';

import { ANIM_FPS } from './encodeMp4';

let ffmpegSingleton: FFmpeg | null = null;
let ffmpegLoadPromise: Promise<FFmpeg> | null = null;

// orber#184: シングルトン FFmpeg は内部 virtual FS を 1 つしか持たないため、
// 並行に encodeAnimationAlphaWasm を呼ぶと writeFile / deleteFile / exec が
// 同じファイル名 (`frame-%04d.png` / `out.webm`) を奪い合って壊れる。
// module-level の serial mutex で呼び出しを必ず直列化する。
let encodeMutex: Promise<void> = Promise.resolve();

// frame ファイル名のヘルパー (`-i frame-%04d.png` と桁数を同期させる目的)。
const FRAME_PAD = 4;
const frameName = (i: number) =>
  `frame-${String(i).padStart(FRAME_PAD, '0')}.png`;

/**
 * ffmpeg.wasm をロードする (シングルトン)。
 *
 * 失敗時はネットワーク断 / 配信欠落と判断できるよう Error を伝播する。
 * 呼び出し側 (Studio) は `alphaEncoderLoadFailed` 文言の UI で通知する。
 */
export async function loadFfmpegAlphaEncoder(): Promise<FFmpeg> {
  if (ffmpegSingleton) return ffmpegSingleton;
  if (ffmpegLoadPromise) return ffmpegLoadPromise;
  const ffmpeg = new FFmpeg();
  ffmpegLoadPromise = (async () => {
    await ffmpeg.load({
      coreURL: '/ffmpeg/ffmpeg-core.js',
      wasmURL: '/ffmpeg/ffmpeg-core.wasm',
    });
    ffmpegSingleton = ffmpeg;
    return ffmpeg;
  })();
  try {
    return await ffmpegLoadPromise;
  } catch (e) {
    // ロード失敗時は singleton を立てず、次回 retry できる状態にする。
    ffmpegLoadPromise = null;
    throw e;
  }
}

/**
 * 透過 PNG フレーム列 (1 frame = 1 PNG ArrayBuffer) を受け取り、
 * libvpx-vp9 + yuva420p で透過 WebM Blob を返す。
 *
 * `onProgress(frame, total)` は ffmpeg.wasm の progress event を中継する。
 * 書き込み / readFile / 削除のフェーズも含めると ffmpeg progress は途中で
 * 90% に達して止まったりするので、UI 側は近似インジケータと割り切る。
 *
 * frames の各 ArrayBuffer は呼び出し後の参照は保持しない (writeFile で
 * 内部コピーされる)。
 *
 * シングルトン共有 virtual FS のため内部で直列化される (`encodeMutex`)。
 */
export async function encodeAnimationAlphaWasm(
  frames: Uint8Array[],
  width: number,
  height: number,
  fps: number = ANIM_FPS,
  onProgress?: (frame: number, total: number) => void,
): Promise<Blob> {
  if (frames.length === 0) {
    throw new Error('frames must be > 0');
  }
  // 直列化: 先行 encode が完了 (resolve / reject 問わず) してから走る。
  // 自分の完了を mutex の next-tail にする。
  const prev = encodeMutex;
  let release: () => void = () => {};
  encodeMutex = new Promise<void>((res) => {
    release = res;
  });
  try {
    await prev.catch(() => {});
    return await runEncode(frames, width, height, fps, onProgress);
  } finally {
    release();
  }
}

async function runEncode(
  frames: Uint8Array[],
  width: number,
  height: number,
  fps: number,
  onProgress?: (frame: number, total: number) => void,
): Promise<Blob> {
  const ffmpeg = await loadFfmpegAlphaEncoder();
  const total = frames.length;

  const inputPattern = `frame-%0${FRAME_PAD}d.png`;
  const outputName = 'out.webm';

  // 既存ファイルの掃除 (前回 encode 残骸が virtual FS に残っている可能性あり)。
  // singleton 再利用時、`listDir('/')` で実際に残っている `frame-*.png` だけを
  // 総当り削除する。前回 ≤ 今回フレーム数なら問題ないが、前回 > 今回のとき
  // 想定名ループだけでは古い余剰フレームが残り `-i frame-%04d.png` が拾って
  // しまう恐れがあるため。`listDir` 自体が落ちた場合 (古い API 等) は想定名
  // ループにフォールバックする。
  let cleanupNames: string[] | null = null;
  try {
    const entries = await ffmpeg.listDir('/');
    cleanupNames = entries
      .filter((e) => !e.isDir && /^frame-\d+\.png$/.test(e.name))
      .map((e) => e.name);
  } catch {
    cleanupNames = null;
  }
  if (cleanupNames) {
    for (const name of cleanupNames) {
      try {
        await ffmpeg.deleteFile(name);
      } catch {
        /* 不存在は無視 */
      }
    }
  } else {
    for (let i = 0; i < total; i++) {
      try {
        await ffmpeg.deleteFile(frameName(i));
      } catch {
        /* 初回 / 不存在は無視 */
      }
    }
  }
  try {
    await ffmpeg.deleteFile(outputName);
  } catch {
    /* 初回 / 不存在は無視 */
  }

  // 1 frame ずつ書き込む。Promise.all すると wasm 内部のシリアル処理に
  // 並べ替えが入ってメモリ使用量がピークで膨らむため、await ループで進める。
  for (let i = 0; i < total; i++) {
    await ffmpeg.writeFile(frameName(i), frames[i]);
  }

  const progressHandler = ({ progress }: { progress: number; time: number }) => {
    // ffmpeg.wasm の progress は 0..1 の近似値。frame に換算して中継する。
    if (!onProgress) return;
    const f = Math.min(total, Math.max(0, Math.round(progress * total)));
    onProgress(f, total);
  };
  ffmpeg.on('progress', progressHandler);

  try {
    // 透過 WebM (libvpx-vp9 + yuva420p)。
    // - `-auto-alt-ref 0`: VP9 alpha と同時に使えない libvpx の制約。必須。
    // - `-pix_fmt yuva420p`: alpha plane を保持する 4:2:0 形式。
    // - bitrate 2M: encodeMp4 / 旧 encodeWebmAlpha と揃える。
    await ffmpeg.exec([
      '-framerate',
      String(fps),
      '-i',
      inputPattern,
      '-c:v',
      'libvpx-vp9',
      '-pix_fmt',
      'yuva420p',
      '-b:v',
      '2M',
      '-auto-alt-ref',
      '0',
      '-s',
      `${width}x${height}`,
      outputName,
    ]);
  } finally {
    ffmpeg.off('progress', progressHandler);
  }

  const data = await ffmpeg.readFile(outputName);
  if (typeof data === 'string') {
    // 我々は encoding を指定していないので Uint8Array が返るはずだが、
    // 型を絞り込むためのガード。
    throw new Error('unexpected string result from ffmpeg.readFile');
  }

  // 後片付け (virtual FS のメモリ解放)。
  for (let i = 0; i < total; i++) {
    try {
      await ffmpeg.deleteFile(frameName(i));
    } catch {
      /* ignore */
    }
  }
  try {
    await ffmpeg.deleteFile(outputName);
  } catch {
    /* ignore */
  }

  // Uint8Array<ArrayBufferLike> → Blob 互換のため buffer view を明示。
  return new Blob([new Uint8Array(data)], { type: 'video/webm' });
}
