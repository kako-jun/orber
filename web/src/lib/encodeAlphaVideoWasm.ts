// orber#192 — JS-only MOV muxer による透過動画出力。
//
// 経緯: orber#184 でアルファ動画を ffmpeg.wasm + libvpx (VP8/VP9) yuva420p で
// エンコードしようとしたが、単一スレッド wasm では実機でメモリ枯渇 OOB / 空アルファ
// の問題に当たり、最終的に `-c:v png -f mov` (PNG-in-MOV) で着地。実エンコードを
// 行わず PNG bytes をそのまま MOV container に並べるだけの構成だった。
//
// #192: ffmpeg.wasm は事実上 MOV muxer 役にしかなっていなかったため、JS だけで
// MOV atom tree を書く `movMuxer.ts` に置き換え、`@ffmpeg/ffmpeg` / `@ffmpeg/util`
// / `@ffmpeg/core` (~30MB CDN ロード) 依存を撤去した。jsdelivr 経由の cross-origin
// fetch、Service Worker の `ffmpegCoreCacheFirst`、`prefetchFfmpegCore` のアイドル
// 投機ロード、`saveData` / `2g`/`3g` saver guard 等の周辺コードもすべて不要。
//
// 出力形式 (前後互換):
//   - alpha/*-alpha.mov、`video/quicktime`
//   - PNG codec (lossless rgba)、解像度 540×960 / 960×540
//   - 192 frames @ 24fps (= 8 秒、ANIM_FPS / ANIM_TOTAL_FRAMES 既定)
//   - NLE 取り込み可 (Premiere / DaVinci / After Effects、VLC)

import { ANIM_FPS } from './encodeMp4';
import { muxPngFramesToMov } from './movMuxer';

/**
 * 透過 PNG フレーム列 (1 frame = 1 PNG ArrayBuffer) を MOV container に詰めて
 * `video/quicktime` Blob を返す。
 *
 * 実エンコードは行わない (PNG bytes をそのまま mdat に並べる)。
 *
 * `onProgress(frame, total)` は呼出し側 API の互換のため受け取るが、muxing は
 * メモリ内シングルパス処理で 1 タイル数十 MB 程度であれば即完了する。開始 / 完了
 * の 2 点だけ通知して UI 側の進捗 UI を破綻させない。
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
  if (onProgress) onProgress(0, frames.length);
  const mov = muxPngFramesToMov(frames, width, height, fps);
  if (onProgress) onProgress(frames.length, frames.length);
  return new Blob([mov], { type: 'video/quicktime' });
}
