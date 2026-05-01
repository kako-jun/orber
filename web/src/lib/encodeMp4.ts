// 後半タイルのアニメーションを WebCodecs + mp4-muxer で h264 mp4 化する。
//
// orber-wasm から渡される `AnimationHandle` は RGBA フレームを 1 枚ずつ吐く
// 反復子なので、それを順番に `VideoEncoder.encode` に流して mp4-muxer に
// 詰め込む。フレームを保持しないことでメモリピークを 1 枚ぶん（≈ 0.9MB / 360×640、
// hi-res DL 時は ≈ 8MB / 1080×1920）に抑える設計。
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
 * `AnimationHandle` を消費して 1 本の mp4 Blob を返す。
 *
 * 呼び出し後、handle はすべての frame を吐き終えており、`free()` 可能。
 * エンコード中の例外は再 throw する（呼び出し側で catch）。
 */
export async function encodeAnimationToMp4(
  handle: AnimationHandleLike,
  onProgress?: (frame: number, total: number) => void,
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
  // 解像度に応じて AVC level を選択する。
  //   - Level 3.1 (0x1F): coded area 上限 921,600px（≈ 720p まで）
  //   - Level 4.2 (0x2A): coded area 上限 2,228,224px（1080p で余裕）
  // hi-res DL 1080×1920 は coded area が 1088×1920 = 2,088,960 になり 3.1
  // を超えるため、preview 解像度より広い場合は 4.2 を使う。Baseline profile
  // の互換性維持のまま level だけ上げる: avc1.42E0{XX}。
  // bitrate 2Mbps は 24fps でも余裕がありつつファイル ~1MB に収まる目安。
  // 仕様上 `configure` は同期で完了し、直後の `encode` 呼び出しは合法。
  const codedArea = Math.ceil(width / 16) * 16 * Math.ceil(height / 16) * 16;
  const codecString = codedArea <= 921_600 ? 'avc1.42E01F' : 'avc1.42E02A';
  encoder.configure({
    codec: codecString,
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
    // #95: フレーム単位の進捗を呼び出し側に通知。encode は非同期だが、
    // ここではループ内で「キューに投入し終えたフレーム数」を進捗として
    // 報告する（実エンコード完了を待たないため、UI 上のリングはやや
    // 早めに 100% に達する可能性があるが、体感としては十分滑らか）。
    onProgress?.(i + 1, totalFrames);
  }

  await encoder.flush();
  encoder.close();

  if (firstError !== null) throw firstError;

  muxer.finalize();
  const buffer = muxer.target.buffer;
  return new Blob([buffer], { type: 'video/mp4' });
}
