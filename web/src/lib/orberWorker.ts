// orber#75 / #112 — wasm + WebGL2 描画 + WebCodecs エンコードを Worker スレッドで実行する。
//
// メインスレッドは UI / DOM / Solid signal だけに集中させ、重い計算は全部
// ここに逃がす。これによりスマホでも生成中にスクロール / タップが死なない。
//
// アーキテクチャ:
//   main → postMessage({ kind, id, ... }) → worker
//   worker → wasm.get_render_data → WebGL2 (OffscreenCanvas) 描画 →
//            convertToBlob (PNG) or VideoEncoder (mp4) → main
//
// データ転送:
//   - PNG / mp4 の ArrayBuffer は Transferable で zero-copy 返却
//   - source RGB は `setSource` で 1 度だけ送って worker 側にキャッシュする
//
// 互換性: OffscreenCanvas + WebGL2 + VideoEncoder/VideoFrame in Worker が要る。
// iOS Safari 16.4+ / Android Chrome / 最近の Firefox。古い端末は対象外。

import init, * as wasm from '../wasm/orber_wasm.js';
import { encodeAnimationFromCanvas } from './encodeMp4';
import { createGlRenderer, type GlRenderer } from './orberGl';

let initialized = false;
let initPromise: Promise<void> | null = null;
function ensureInit(): Promise<void> {
  if (initialized) return Promise.resolve();
  if (!initPromise) {
    initPromise = init().then(() => {
      initialized = true;
    });
  }
  return initPromise;
}

let cachedSource: { rgb: Uint8Array; width: number; height: number } | null = null;

// OffscreenCanvas + GlRenderer は (width, height) ごとに 1 個だけ作って使い回す。
// 192 frame の動画化中はもちろん、アスペクト切替や preview / hi-res の解像度
// 切替でも、同じサイズなら再利用したい。WebGL の context 生成は重い。
let cachedCanvas: { canvas: OffscreenCanvas; renderer: GlRenderer; width: number; height: number } | null = null;

function getRenderer(width: number, height: number): { canvas: OffscreenCanvas; renderer: GlRenderer } {
  if (cachedCanvas && cachedCanvas.width === width && cachedCanvas.height === height) {
    return { canvas: cachedCanvas.canvas, renderer: cachedCanvas.renderer };
  }
  if (cachedCanvas) {
    cachedCanvas.renderer.dispose();
    cachedCanvas = null;
  }
  const canvas = new OffscreenCanvas(width, height);
  const renderer = createGlRenderer(canvas);
  renderer.setResolution(width, height);
  cachedCanvas = { canvas, renderer, width, height };
  return { canvas, renderer };
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

function mergeParams(p: BaseParams) {
  if (!cachedSource) {
    throw new Error('source not set — call setSource before generate/animate');
  }
  return {
    ...p,
    source_rgb: cachedSource.rgb,
    source_width: cachedSource.width,
    source_height: cachedSource.height,
  };
}

type Req =
  | { kind: 'init'; id: number }
  | {
      kind: 'setSource';
      id: number;
      rgb: Uint8Array;
      width: number;
      height: number;
    }
  | { kind: 'generateOne'; id: number; params: BaseParams; n: number; index: number }
  | {
      kind: 'animateOne';
      id: number;
      params: BaseParams;
      n: number;
      index: number;
      totalFrames: number;
    };

function post(msg: unknown, transfers: Transferable[] = []) {
  (self as unknown as Worker).postMessage(msg, transfers);
}

self.addEventListener('message', async (e: MessageEvent<Req>) => {
  const req = e.data;
  try {
    await ensureInit();
    switch (req.kind) {
      case 'init': {
        post({ id: req.id, ok: true });
        break;
      }
      case 'setSource': {
        cachedSource = { rgb: req.rgb, width: req.width, height: req.height };
        post({ id: req.id, ok: true });
        break;
      }
      case 'generateOne': {
        const params = mergeParams(req.params);
        const data = wasm.get_render_data(params, req.n, req.index);
        const { canvas, renderer } = getRenderer(req.params.width, req.params.height);
        renderer.setRenderData(data);
        renderer.renderFrame(0);
        const blob = await canvas.convertToBlob({ type: 'image/png' });
        const buf = await blob.arrayBuffer();
        post({ id: req.id, ok: true, data: buf }, [buf]);
        break;
      }
      case 'animateOne': {
        const params = mergeParams(req.params);
        const data = wasm.get_render_data(params, req.n, req.index);
        const width = req.params.width;
        const height = req.params.height;
        const { canvas, renderer } = getRenderer(width, height);
        renderer.setRenderData(data);
        const PROGRESS_STRIDE = 4;
        const blob = await encodeAnimationFromCanvas(
          canvas,
          (t) => renderer.renderFrame(t),
          req.totalFrames,
          width,
          height,
          (frame, total) => {
            if (frame !== total && frame % PROGRESS_STRIDE !== 0) return;
            post({ kind: 'animateProgress', id: req.id, frame, total });
          },
        );
        const buf = await blob.arrayBuffer();
        post({ id: req.id, ok: true, data: buf }, [buf]);
        break;
      }
      default: {
        const exhaustive: never = req;
        throw new Error(`unknown req kind: ${JSON.stringify(exhaustive)}`);
      }
    }
  } catch (err) {
    post({ id: (req as { id?: number }).id ?? -1, ok: false, error: String(err) });
  }
});
