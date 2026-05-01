// 後半タイルのアニメーションを WebCodecs + mp4-muxer で h264 mp4 化する。
//
// #112 の WebGL 経路では OffscreenCanvas (WebGL2) に 1 frame 描画して
// `new VideoFrame(canvas)` で直接エンコーダに食わせる。RGBA バッファの GPU→CPU
// readback が消えるので、CPU 経路 (wasm + ImageData → ImageBitmap →
// VideoFrame) と比べて 1080×1920 × 192 frame で大幅に速い。
//
// 互換性: VideoEncoder / VideoFrame(canvas) が要る。
// Chrome 94+ / Safari 16.4+ / Firefox 130+。非対応ブラウザでは throw する
// （Studio 側で catch して静止画フォールバックする）。

import { Muxer, ArrayBufferTarget } from 'mp4-muxer';

export const ANIM_FPS = 24;
// #77: 8 秒ぶん。背景・配信オーバーレイ用途では「ほとんど動いていないように
// 見える」レベルの遅さが理想。VerySlow (cycle_count=1) なら 1 cross / 8s で
// 画面端から端まで 8 秒かけて drift する。orber の motion model は t=0 と
// t=1 がピクセル一致するので `<video loop>` で継ぎ目なくループする。
// 4 → 8 で frame 数が倍になるため worker 側 mp4 化時間も倍だが、worker
// 経由なので main thread はブロックしない（#75）。
export const ANIM_DURATION_SECONDS = 8;
export const ANIM_TOTAL_FRAMES = ANIM_FPS * ANIM_DURATION_SECONDS;

export function isWebCodecsSupported(): boolean {
  return typeof VideoEncoder !== 'undefined' && typeof VideoFrame !== 'undefined';
}

/**
 * OffscreenCanvas (WebGL2) を消費して 1 本の mp4 Blob を返す。
 *
 * `renderFrame(t)` を呼ぶと canvas に t における 1 フレームが描画される前提。
 * このループ側で `t = i / totalFrames` (i = 0..totalFrames) を順に流し、
 * 各 frame について `new VideoFrame(canvas)` を作って encoder に渡す。
 *
 * canvas を直接 VideoFrame に渡すので RGBA の readback / ImageData 経由が消え、
 * GPU 経路ではエンコード時間が encoder 側律速まで詰まる（#112）。
 *
 * エンコード中の例外は再 throw する（呼び出し側で catch）。
 */
export async function encodeAnimationFromCanvas(
  canvas: OffscreenCanvas,
  renderFrame: (t: number) => void,
  totalFrames: number,
  width: number,
  height: number,
  onProgress?: (frame: number, total: number) => void,
): Promise<Blob> {
  if (!isWebCodecsSupported()) {
    throw new Error('WebCodecs (VideoEncoder/VideoFrame) is not available');
  }
  if (totalFrames <= 0) {
    throw new Error('totalFrames must be > 0');
  }

  const muxer = new Muxer({
    target: new ArrayBufferTarget(),
    video: {
      codec: 'avc',
      width,
      height,
      frameRate: ANIM_FPS,
    },
    fastStart: 'in-memory',
  });

  let firstError: unknown = null;
  const encoder = new VideoEncoder({
    output: (chunk, meta) => muxer.addVideoChunk(chunk, meta),
    error: (e) => {
      // 直接 throw すると WebCodecs の async 境界で握りつぶされるので、
      // 状態として保持して flush 後に再 throw する。
      if (firstError === null) firstError = e;
    },
  });
  // 解像度に応じて AVC level を選択する。
  //   - Level 3.1 (0x1F): coded area 上限 921,600px（≈ 720p まで）
  //   - Level 4.2 (0x2A): coded area 上限 2,228,224px（1080p で余裕）
  const codedArea = Math.ceil(width / 16) * 16 * Math.ceil(height / 16) * 16;
  const codecString = codedArea <= 921_600 ? 'avc1.42E01F' : 'avc1.42E02A';
  encoder.configure({
    codec: codecString,
    width,
    height,
    framerate: ANIM_FPS,
    bitrate: 2_000_000,
    hardwareAcceleration: 'prefer-hardware',
  });

  const microsecondsPerFrame = 1_000_000 / ANIM_FPS;

  for (let i = 0; i < totalFrames; i++) {
    if (firstError !== null) break;
    // t は [0, 1) の範囲を順に。t=1 は出さない（loop closure で t=0 と一致）。
    const t = i / totalFrames;
    renderFrame(t);
    // canvas 直渡し: VideoFrame コンストラクタは canvas のピクセルをスナップ
    // ショットして取り込む。drawArrays 直後でも GL のキューイング順序は
    // 保たれているので、あとから読み出した時に未描画のフレームが入る心配は
    // ない（WebGL → VideoFrame の同一スレッド内は順序保証あり）。
    let frame: VideoFrame;
    try {
      frame = new VideoFrame(canvas as unknown as CanvasImageSource, {
        timestamp: Math.round(i * microsecondsPerFrame),
        duration: Math.round(microsecondsPerFrame),
      });
    } catch (e) {
      if (firstError === null) firstError = e;
      break;
    }
    try {
      // 1 秒ごとにキーフレームを入れてシーク・ループ頭出しを安定させる。
      encoder.encode(frame, { keyFrame: i % ANIM_FPS === 0 });
    } catch (e) {
      if (firstError === null) firstError = e;
      frame.close();
      break;
    }
    frame.close();
    // #95: フレーム単位の進捗を呼び出し側に通知。
    onProgress?.(i + 1, totalFrames);
  }

  await encoder.flush();
  encoder.close();

  if (firstError !== null) throw firstError;

  muxer.finalize();
  const buffer = muxer.target.buffer;
  return new Blob([buffer], { type: 'video/mp4' });
}
