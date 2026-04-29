// 後半タイルのアニメーションを WebCodecs + mp4-muxer で h264 mp4 化する。
//
// orber-wasm から渡される `AnimationHandle` は RGBA フレームを 1 枚ずつ吐く
// 反復子なので、それを順番に `VideoEncoder.encode` に流して mp4-muxer に
// 詰め込む。フレームを保持しないことでメモリピークを 1 枚ぶん（≈ 2MB / 540×960）
// に抑える設計。
//
// 互換性: VideoEncoder / VideoFrame / ImageData(buf, w, h) が要る。
// Chrome 94+ / Safari 16.4+ / Firefox 130+。非対応ブラウザでは throw する
// （Studio 側で catch して静止画フォールバックする）。

import { Muxer, ArrayBufferTarget } from 'mp4-muxer';

export interface AnimationHandleLike {
  readonly width: number;
  readonly height: number;
  readonly total_frames: number;
  next_frame(): Uint8ClampedArray | null | undefined;
  free?: () => void;
}

export const ANIM_FPS = 24;
// 4 秒ぶん。orber の motion model は t=0 と t=1 がピクセル一致するので、
// `<video loop>` で継ぎ目なくエンドレス再生される。
export const ANIM_DURATION_SECONDS = 4;
export const ANIM_TOTAL_FRAMES = ANIM_FPS * ANIM_DURATION_SECONDS;

export function isWebCodecsSupported(): boolean {
  return typeof VideoEncoder !== 'undefined' && typeof VideoFrame !== 'undefined';
}

/**
 * `AnimationHandle` を消費して 1 本の mp4 Blob を返す。
 *
 * 呼び出し後、handle はすべての frame を吐き終えており、`free()` 可能。
 * エンコード中の例外は再 throw する（呼び出し側で catch）。
 */
export async function encodeAnimationToMp4(
  handle: AnimationHandleLike,
): Promise<Blob> {
  if (!isWebCodecsSupported()) {
    throw new Error('WebCodecs (VideoEncoder/VideoFrame) is not available');
  }

  const width = handle.width;
  const height = handle.height;
  const totalFrames = handle.total_frames;

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
  // avc1.42E01F = H.264 Baseline Level 3.1。540×960 / 960×540 のサイズで
  // 大半の環境がハードウェアデコード対応している。bitrate 2Mbps は 24fps
  // でも余裕がありつつファイル ~1MB に収まる目安。
  // 仕様上 `configure` は同期で完了し、直後の `encode` 呼び出しは合法。
  // 内部的にはハードウェアエンコーダの初期化が走るが、それは encoder の
  // ジョブキューでシリアライズされるので呼び出し側は意識不要。
  encoder.configure({
    codec: 'avc1.42E01F',
    width,
    height,
    framerate: ANIM_FPS,
    bitrate: 2_000_000,
  });

  const microsecondsPerFrame = 1_000_000 / ANIM_FPS;

  for (let i = 0; i < totalFrames; i++) {
    if (firstError !== null) break;
    const rgba = handle.next_frame();
    if (!rgba) break;
    const imageData = new ImageData(rgba, width, height);
    // createImageBitmap でビットマップ化してから VideoFrame に詰める。
    // 一部ブラウザは ImageData → VideoFrame 直接生成に未対応のため。
    const bitmap = await createImageBitmap(imageData);
    const frame = new VideoFrame(bitmap, {
      timestamp: Math.round(i * microsecondsPerFrame),
      duration: Math.round(microsecondsPerFrame),
    });
    bitmap.close();
    // レビュー S8: VideoEncoder.encode は同期 throw する仕様（encoder の
    // 状態異常など）。catch せずに放置すると Worker が die して Studio 側
    // pending が孤児化する。catch して firstError 経由で flush 後に再 throw。
    try {
      // 1 秒ごとにキーフレームを入れてシーク・ループ頭出しを安定させる。
      encoder.encode(frame, { keyFrame: i % ANIM_FPS === 0 });
    } catch (e) {
      if (firstError === null) firstError = e;
      frame.close();
      break;
    }
    frame.close();
  }

  await encoder.flush();
  encoder.close();

  if (firstError !== null) throw firstError;

  muxer.finalize();
  const buffer = muxer.target.buffer;
  return new Blob([buffer], { type: 'video/mp4' });
}
