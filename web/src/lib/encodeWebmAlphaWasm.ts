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
//   - core は jsdelivr CDN (`cdn.jsdelivr.net/npm/@ffmpeg/core@<ver>/dist/umd`)
//     から取得。Cloudflare Pages の単一ファイル上限 25 MiB に
//     `ffmpeg-core.wasm` (~31 MB) が引っかかるため、同一オリジン配信を諦め
//     CDN 経由にする。バージョンを pin して immutable cache を効かせ、
//     Service Worker (`public/sw.js`) で CacheFirst により初回後はオフラインで
//     再利用できる経路を維持する。
//   - 1 frame ずつ PNG として virtual FS に書き込み、`ffmpeg.exec()` で
//     `-c:v libvpx-vp9 -pix_fmt yuva420p -auto-alt-ref 0` で WebM 化
//
// `-auto-alt-ref 0` は VP9 alpha と同時に使えない (libvpx 制約) ので必須。

import { FFmpeg } from '@ffmpeg/ffmpeg';
import { toBlobURL } from '@ffmpeg/util';

import { ANIM_FPS } from './encodeMp4';

// `@ffmpeg/core` のバージョンは package.json の devDependencies に pin。
// 数字は `node_modules/@ffmpeg/core/package.json` の `version` と同期する。
// jsdelivr は Fastly + Cloudflare の二重ミラーで unpkg より安定 & immutable
// cache が効くため CDN として採用。
export const FFMPEG_CORE_VERSION = '0.12.10';
// `@ffmpeg/ffmpeg` 0.12.15 は `type: "module"` Worker を spawn し、Worker 内で
// `importScripts` が使えないため fallback の `import(coreURL)` が走る。
// UMD ビルドは ES module ではないため `import()` が失敗する (本番再現:
// `failed to import ffmpeg-core.js`)。ESM ビルドを使う必要がある。
export const FFMPEG_CORE_CDN_BASE = `https://cdn.jsdelivr.net/npm/@ffmpeg/core@${FFMPEG_CORE_VERSION}/dist/esm`;
export const FFMPEG_CORE_URL = `${FFMPEG_CORE_CDN_BASE}/ffmpeg-core.js`;
export const FFMPEG_WASM_URL = `${FFMPEG_CORE_CDN_BASE}/ffmpeg-core.wasm`;

let ffmpegSingleton: FFmpeg | null = null;
let ffmpegLoadPromise: Promise<FFmpeg> | null = null;
// orber#184 review S1: in-flight プリフェッチを保持する。`loadFfmpegAlphaEncoder`
// が呼ばれた時点でプリフェッチがまだ走っていれば完了を待ち、SW cache に積まれた
// 状態から `ffmpeg.load(...)` に入ることで二重 fetch race (プリフェッチ + 実ロード
// が両方走って同じ 31 MB を重複ダウンロードする) を解消する。
// プリフェッチ失敗 (offline 等) は無視して通常 load 経路に進む。
// 一度 resolve したら null に戻さない (再発火しない設計)。SW がバージョン bump で
// 旧キャッシュを破棄した直後の同一セッションでは `await prefetchPromise` が即
// resolve するだけで、実 fetch は `ffmpeg.load` 側が CDN に取りに行く。
let prefetchPromise: Promise<void> | null = null;

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
 * ffmpeg-core (~31 MB) を Service Worker の CacheFirst に先取りで温めるための
 * プリフェッチ。Studio の onMount から `requestIdleCallback` 経由で呼び、
 * ユーザーがオーブを設計している間に裏で fetch が完走するようにする。
 *
 * - シングルトン (`ffmpegSingleton` / `ffmpegLoadPromise`) には**一切触れない**。
 *   純粋に HTTP fetch を走らせて SW (`public/sw.js` の `ffmpegCoreCacheFirst`)
 *   に拾わせるだけ。失敗しても透過 DL を実行した時に普通に再 fetch される。
 * - レビュー M1: `mode: 'cors'` (= デフォルト、`mode` を指定しない) で走らせる。
 *   旧 `mode: 'no-cors'` は opaque response を返し、それが SW の
 *   `ffmpeg-core-v<version>` cache に焼き付くと後続の `importScripts(ffmpeg-core.js)`
 *   や WebAssembly streaming compile が CORS チェックで失敗するリスクがある。
 *   jsdelivr は CORS を許可しているので普通の cors fetch で問題なく通る。
 * - レビュー S1: in-flight Promise を `prefetchPromise` に保持し、`loadFfmpegAlphaEncoder`
 *   が裏で待てるようにする。これで「プリフェッチ in-flight 中に DL ボタン押下」の
 *   ケースでも 1 回の DL に収束する。
 * - 例外は静かに握りつぶす (UI / console に出さない)。fetch は同期 throw しない
 *   ため外側 try は不要 (レビュー N3)。
 */
export function prefetchFfmpegCore(): void {
  if (typeof fetch === 'undefined') return;
  // 既に走っているプリフェッチがあれば再発火しない (idempotent)。
  if (prefetchPromise) return;
  const coreP = fetch(FFMPEG_CORE_URL, { credentials: 'omit' }).catch(() => {});
  const wasmP = fetch(FFMPEG_WASM_URL, { credentials: 'omit' }).catch(() => {});
  prefetchPromise = Promise.all([coreP, wasmP]).then(() => {});
}

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
    // レビュー S1: プリフェッチが in-flight ならその完走を待ってから load する。
    // SW (`ffmpegCoreCacheFirst`) がプリフェッチ Response を cache に積み終わって
    // いれば、`ffmpeg.load` 内部の fetch は cache hit になり 31 MB の二重 DL を回避。
    // プリフェッチ失敗 (offline / CORS 等) は無視して通常 load 経路に進む。
    if (prefetchPromise) {
      await prefetchPromise.catch(() => {});
    }
    // orber#184 hotfix: `@ffmpeg/ffmpeg` v0.12 系は `load({coreURL, wasmURL})`
    // 内部で classic Worker を spawn し、Worker 内で `importScripts(coreURL)` を
    // 呼ぶ。`coreURL` が cross-origin (jsdelivr CDN) だと、サーバ側で
    // `Access-Control-Allow-Origin` が許可されていても Worker spec の制約で
    // importScripts が失敗するケースがある (本番 orber.llll-ll.com で再現:
    // `failed to import ffmpeg-core.js`)。ffmpeg.wasm 公式 README の標準パターン
    // である `@ffmpeg/util` の `toBlobURL` で blob: URL に変換してから渡す:
    // Worker の importScripts は same-origin の blob: URL になるので CORS 制約
    // から外れる。
    //
    // プリフェッチとの互換: `toBlobURL` 内部 fetch は SW (`ffmpegCoreCacheFirst`)
    // のキャッシュにヒットするので、prefetch で温めた状態なら 31 MB の再 DL は
    // 発生しない。プリフェッチの効果は維持される。
    const [coreBlobURL, wasmBlobURL] = await Promise.all([
      toBlobURL(FFMPEG_CORE_URL, 'text/javascript'),
      toBlobURL(FFMPEG_WASM_URL, 'application/wasm'),
    ]);
    await ffmpeg.load({
      coreURL: coreBlobURL,
      wasmURL: wasmBlobURL,
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

  // #184 diagnostic: ffmpeg 内部ログを捕捉する。`memory access out of bounds`
  // 等の wasm trap が起きた時に、ffmpeg 側がどこまで進んでいたか / どんな
  // メッセージを出していたかを Error 文に同梱して console / UI に伝えるため。
  const logLines: string[] = [];
  const logHandler = ({ message }: { message: string; type: string }) => {
    logLines.push(message);
    if (logLines.length > 80) logLines.shift();
    // 本番ではブラウザ console にも直接出す (deploy 中の調査用、後で外す)
    // eslint-disable-next-line no-console
    console.log('[ffmpeg]', message);
  };
  ffmpeg.on('log', logHandler);

  try {
    // 透過 WebM (libvpx-vp9 + yuva420p)。
    // - `-auto-alt-ref 0`: VP9 alpha と同時に使えない libvpx の制約。必須。
    // - codec: `libvpx` (VP8) に切替。`libvpx-vp9` + `yuva420p` は単スレッド
    //   wasm ffmpeg.wasm 環境で根本的に不安定 (本番で複数パターン試行も全滅)。
    //   VP8 alpha は枯れていて wasm 動作実績豊富、容器は同じ .webm、
    //   NLE 互換性も同じ。
    // - `-pix_fmt yuva420p`: alpha plane を保持する 4:2:0 形式。VP8 で
    //   yuva420p を指定するとは libvpx 内部で 2 つの encoder instance を起動して
    //   YUV プレーンと alpha プレーンを別々に encode し、WebM の BlockAdditional
    //   経由でアルファトラックを格納する (`alpha_mode=1` タグ自動付与)。
    // - VP9 用フラグ (`-auto-alt-ref` / `-lag-in-frames`) は撤去:
    //   VP8 で渡すと libvpx の alpha encoder セットアップを壊して空アルファに
    //   なる現象を実機で確認済み。VP8 のデフォルト挙動に任せる。
    try {
      await ffmpeg.exec([
        '-framerate',
        String(fps),
        '-i',
        inputPattern,
        '-c:v',
        'libvpx',
        '-pix_fmt',
        'yuva420p',
        '-b:v',
        '2M',
        '-s',
        `${width}x${height}`,
        outputName,
      ]);
    } catch (e) {
      const tail = logLines.slice(-30).join('\n');
      const orig = e instanceof Error ? e.message : String(e);
      throw new Error(
        `ffmpeg.exec failed (${width}x${height}, ${total}f): ${orig}\n--- ffmpeg log tail ---\n${tail}`,
      );
    }
  } finally {
    ffmpeg.off('progress', progressHandler);
    ffmpeg.off('log', logHandler);
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
