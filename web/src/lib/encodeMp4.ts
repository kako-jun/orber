// 後半タイルのアニメーションを WebCodecs + mp4-muxer で mp4 化する。
//
// #112 の WebGL 経路では OffscreenCanvas (WebGL2) に 1 frame 描画して
// `new VideoFrame(canvas)` で直接エンコーダに食わせる。RGBA バッファの GPU→CPU
// readback が消えるので、CPU 経路 (wasm + ImageData → ImageBitmap →
// VideoFrame) と比べて 1080×1920 × 192 frame で大幅に速い。
//
// 互換性: VideoEncoder / VideoFrame(canvas) が要る。
// Chrome 94+ / Safari 16.4+ / Firefox 130+。非対応ブラウザでは throw する
// （Studio 側で catch して静止画フォールバックする）。
//
// #196: Linux Chrome / Edge / Firefox は H.264 エンコーダを持たないため、
// `pickSupportedVideoCodec` で H.264 → VP9 → AV1 を順に probe し、最初に
// サポートされた codec を encoder / muxer 両方に流す。mp4-muxer は
// `'avc' | 'vp9' | 'av1'` をサポート済みなので、mp4 拡張子は維持できる。

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

// #196: Linux Chrome / Edge / Firefox は H.264 エンコーダを持たないため
// (`VideoEncoder.isConfigSupported({ codec: 'avc1.*' })` が false で返る)、
// VP9 / AV1 にフォールバックする必要がある。muxer 側は mp4-muxer が
// `'avc' | 'vp9' | 'av1'` をサポートしているのでそのまま乗せる。
export interface PickedVideoCodec {
  /** WebCodecs `VideoEncoder.configure({ codec })` に渡す文字列。 */
  codec: string;
  /** mp4-muxer `Muxer({ video: { codec } })` に渡すタグ。 */
  muxerCodec: 'avc' | 'vp9' | 'av1';
  /** `VideoEncoder.configure({ hardwareAcceleration })` で採用する hint。 */
  hardwareAcceleration: 'prefer-hardware' | 'no-preference';
}

interface CodecCandidate {
  codec: string;
  muxerCodec: 'avc' | 'vp9' | 'av1';
}

function buildCandidates(width: number, height: number): CodecCandidate[] {
  // H.264 codec string は既存ロジックを維持: Level 3.1 (≤ 720p 相当) / 4.2 (1080p)。
  const codedArea = Math.ceil(width / 16) * 16 * Math.ceil(height / 16) * 16;
  const avcCodec = codedArea <= 921_600 ? 'avc1.42E01F' : 'avc1.42E02A';
  // VP9 / AV1 は 1080×1920 で安全に通る Level 4.1 を既定にし、`isConfigSupported`
  // が false を返したら次の候補へ落とす。
  //   - vp09.<profile>.<level>.<bitDepth>  → Profile 0 / Level 4.1 / 8bit
  //   - av01.<profile>.<level><tier>.<bitDepth> → Main / Level 4.1 / Main tier / 8bit
  return [
    { codec: avcCodec, muxerCodec: 'avc' },
    { codec: 'vp09.00.41.08', muxerCodec: 'vp9' },
    { codec: 'av01.0.09M.08', muxerCodec: 'av1' },
  ];
}

/**
 * VideoEncoder.isConfigSupported で H.264 → VP9 → AV1 の順に probe し、
 * 最初にサポートされた codec を返す。`prefer-hardware` で全滅した場合は
 * `no-preference` で再 probe する 2 段リトライ構成。
 *
 * 戻り値は `encoder.configure()` と `Muxer({ video.codec })` の両方に
 * そのまま流し込めるオブジェクト。全 codec で false なら null を返す。
 *
 * #196: Linux Chrome / Edge は H.264 エンコーダ未提供。
 */
export async function pickSupportedVideoCodec(
  width: number,
  height: number,
): Promise<PickedVideoCodec | null> {
  if (typeof VideoEncoder === 'undefined' || typeof VideoEncoder.isConfigSupported !== 'function') {
    return null;
  }
  const candidates = buildCandidates(width, height);
  const accelHints: Array<'prefer-hardware' | 'no-preference'> = [
    'prefer-hardware',
    'no-preference',
  ];
  for (const hardwareAcceleration of accelHints) {
    for (const cand of candidates) {
      try {
        const result = await VideoEncoder.isConfigSupported({
          codec: cand.codec,
          width,
          height,
          framerate: ANIM_FPS,
          bitrate: 2_000_000,
          hardwareAcceleration,
        });
        if (result.supported) {
          return {
            codec: cand.codec,
            muxerCodec: cand.muxerCodec,
            hardwareAcceleration,
          };
        }
      } catch {
        // 不正な codec string などで throw する実装もあるが、その場合は次の
        // 候補に進めばよい。
      }
    }
  }
  return null;
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

  // #196: H.264 → VP9 → AV1 の順で probe。Linux Chrome / Edge / Firefox は
  // H.264 エンコーダを持たないので、ここで VP9 / AV1 に倒れて Studio の
  // partial failure 経路（静止画フォールバック）に巻き込まれずに済む。
  const picked = await pickSupportedVideoCodec(width, height);
  if (picked === null) {
    throw new Error('No supported VideoEncoder codec (tried H.264 / VP9 / AV1)');
  }

  const muxer = new Muxer({
    target: new ArrayBufferTarget(),
    video: {
      codec: picked.muxerCodec,
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
  encoder.configure({
    codec: picked.codec,
    width,
    height,
    framerate: ANIM_FPS,
    bitrate: 2_000_000,
    hardwareAcceleration: picked.hardwareAcceleration,
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
