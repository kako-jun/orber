// orber#56 — VP9 alpha encoding for the transparent download bundle.
//
// Mirrors `encodeMp4.ts`: drives an OffscreenCanvas frame loop and pushes each
// frame through a `VideoEncoder`, then muxes into WebM. The key differences:
//
//   - codec: `vp09.00.10.08` (VP9 Profile 0, 8-bit) with `alpha: 'keep'` so
//     the encoder allocates a parallel alpha plane and the resulting WebM has
//     a real transparent video track (Chromium / Firefox honour this).
//   - container: `webm-muxer` (same author family as `mp4-muxer`).
//
// Safari currently does not support VP9 alpha encoding via WebCodecs. The
// caller is expected to gate this codepath with `isVp9AlphaSupported()` and
// hide the GUI option (or fall back to PNG/WebP only) on unsupported browsers.

import { Muxer, ArrayBufferTarget } from 'webm-muxer';

import { ANIM_FPS } from './encodeMp4';

const VP9_ALPHA_CODEC = 'vp09.00.10.08';

/**
 * Probe the browser for VP9-with-alpha encode support. Returns `false` on
 * Safari (no WebCodecs VP9 alpha at the time of writing) and on engines that
 * lack `VideoEncoder` entirely.
 *
 * Cached for the lifetime of the worker so repeated checks are zero-cost.
 */
let cachedVp9AlphaSupported: boolean | null = null;
export async function isVp9AlphaSupported(): Promise<boolean> {
  if (cachedVp9AlphaSupported !== null) return cachedVp9AlphaSupported;
  if (typeof VideoEncoder === 'undefined' || typeof VideoFrame === 'undefined') {
    cachedVp9AlphaSupported = false;
    return false;
  }
  try {
    // 320x568 を選んだ理由: 任意の妥当な解像度で probe すれば十分で、本番 (1080x1920)
    // と同じ codec 文字列なので supported か否かは桁の問題ではない。
    const probe = await VideoEncoder.isConfigSupported({
      codec: VP9_ALPHA_CODEC,
      width: 320,
      height: 568,
      framerate: ANIM_FPS,
      bitrate: 2_000_000,
      alpha: 'keep',
    });
    cachedVp9AlphaSupported = !!probe.supported;
  } catch {
    cachedVp9AlphaSupported = false;
  }
  return cachedVp9AlphaSupported;
}

/**
 * OffscreenCanvas (WebGL2, transparent bg) を消費して 1 本の WebM Blob を返す。
 *
 * `renderFrame(t)` は呼び出すと canvas に「透過背景 + orb のみ」を描画する想定。
 * 呼び出し側（worker）は事前に `setRenderData` の bg.a を 0 に上書きしてから
 * 渡すこと。`encodeMp4.ts::encodeAnimationFromCanvas` と同じインターフェースで、
 * 戻り値が WebM Blob である点だけが違う。
 */
export async function encodeAnimationAlphaFromCanvas(
  canvas: OffscreenCanvas,
  renderFrame: (t: number) => void,
  totalFrames: number,
  width: number,
  height: number,
  onProgress?: (frame: number, total: number) => void,
): Promise<Blob> {
  if (!(await isVp9AlphaSupported())) {
    throw new Error('VP9 alpha encoding is not available in this browser');
  }
  if (totalFrames <= 0) {
    throw new Error('totalFrames must be > 0');
  }

  const muxer = new Muxer({
    target: new ArrayBufferTarget(),
    video: {
      codec: 'V_VP9',
      width,
      height,
      frameRate: ANIM_FPS,
      alpha: true,
    },
  });

  let firstError: unknown = null;
  const encoder = new VideoEncoder({
    output: (chunk, meta) => muxer.addVideoChunk(chunk, meta),
    error: (e) => {
      if (firstError === null) firstError = e;
    },
  });
  encoder.configure({
    codec: VP9_ALPHA_CODEC,
    width,
    height,
    framerate: ANIM_FPS,
    bitrate: 2_000_000,
    alpha: 'keep',
  });

  const microsecondsPerFrame = 1_000_000 / ANIM_FPS;

  for (let i = 0; i < totalFrames; i++) {
    if (firstError !== null) break;
    const t = i / totalFrames;
    renderFrame(t);
    let frame: VideoFrame;
    try {
      frame = new VideoFrame(canvas as unknown as CanvasImageSource, {
        timestamp: Math.round(i * microsecondsPerFrame),
        duration: Math.round(microsecondsPerFrame),
        alpha: 'keep',
      });
    } catch (e) {
      if (firstError === null) firstError = e;
      break;
    }
    try {
      encoder.encode(frame, { keyFrame: i % ANIM_FPS === 0 });
    } catch (e) {
      if (firstError === null) firstError = e;
      frame.close();
      break;
    }
    frame.close();
    onProgress?.(i + 1, totalFrames);
  }

  await encoder.flush();
  encoder.close();

  if (firstError !== null) throw firstError;

  muxer.finalize();
  const buffer = muxer.target.buffer;
  return new Blob([buffer], { type: 'video/webm' });
}
