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
import {
  encodeAnimationAlphaFromCanvas,
  isVp9AlphaSupported,
} from './encodeWebmAlpha';
import { createGlRenderer, GLYPH_SDF_SIZE, type GlRenderer } from './orberGl';
import { generateImageSdf, generateJsGlyphSdf } from './jsGlyphSdf';

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

// Glyph SDF の wasm 生成 + GPU upload を 1 度だけにする
// ためのキャッシュ。同じ (ch, size) なら再 upload しない。worker は固定 size
// (GLYPH_SDF_SIZE) でしか呼ばないので size をキーから外しても良いが、将来
// 切替の余地を残すために含める。getRenderer で renderer を作り直したときも
// invalidate する必要がある（テクスチャも一緒に dispose されるため）。
//
// #160: shape='image' のときは ch ではなく ImageBitmap を SDF 元にするので、
// kind を分けてキャッシュする。bitmap は Studio から `setImageShape` で 1 度
// 送られて worker にキャッシュされる (cachedImageBitmap)。
let cachedGlyph:
  | { kind: 'char'; ch: string; size: number }
  | { kind: 'image'; bitmap: ImageBitmap; size: number }
  | null = null;
let cachedImageBitmap: ImageBitmap | null = null;

function getRenderer(width: number, height: number): { canvas: OffscreenCanvas; renderer: GlRenderer } {
  if (cachedCanvas && cachedCanvas.width === width && cachedCanvas.height === height) {
    return { canvas: cachedCanvas.canvas, renderer: cachedCanvas.renderer };
  }
  if (cachedCanvas) {
    cachedCanvas.renderer.dispose();
    cachedCanvas = null;
    // renderer を作り直すとテクスチャも消えるので Glyph / Image
    // キャッシュも無効化する（次の ensure*SdfUploaded で再 upload）。
    cachedGlyph = null;
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
  // Phase B (#55): UI から流れる advanced 軸。空文字は "未指定（既存挙動）"。
  glyph_char?: string;
  count_preset?: string;
  speed_preset?: string;
  softness_preset?: string;
  // #136: Glyph 回転 ON/OFF。`true` 既定で従来挙動、`false` で静止描画。
  glyph_rotate?: boolean;
}

function ensureGlyphSdfUploaded(renderer: GlRenderer, ch: string): void {
  const size = GLYPH_SDF_SIZE;
  if (
    cachedGlyph &&
    cachedGlyph.kind === 'char' &&
    cachedGlyph.ch === ch &&
    cachedGlyph.size === size
  )
    return;
  // #159: wasm 側で同梱フォントから SDF を作れる字 (☆ 等) は wasm 経路を使う
  // (高速 / 環境非依存)。それ以外 (絵文字 / 漢字 / 任意 Unicode) は worker 内
  // OffscreenCanvas で OS フォントスタックでラスタライズして SDF 化する。
  // 後者は端末ごとに見た目が変わり得る (Mac の 🐱 と Windows の 🐱 は別形状)
  // が、これは「ユーザーが入れた字を尊重して描画する」を優先するための
  // 仕様。両経路とも出力フォーマット (R8 size×size) は一致しているので
  // renderer 側の取り扱いは共通で良い。
  let sdf: Uint8Array;
  if (wasm.glyph_supported(ch)) {
    sdf = wasm.get_glyph_sdf(ch, size);
  } else {
    sdf = generateJsGlyphSdf(ch, size);
  }
  renderer.setGlyphSdf(sdf, size);
  cachedGlyph = { kind: 'char', ch, size };
}

// #160: shape='image' 用。Studio 側で `setImageShape(bitmap)` 経由で送られて
// `cachedImageBitmap` に入っている画像をシルエット化 → SDF 化 → upload。
// 同じ bitmap reference なら再生成しない。
function ensureImageSdfUploaded(renderer: GlRenderer): void {
  const size = GLYPH_SDF_SIZE;
  if (!cachedImageBitmap) {
    throw new Error('image shape requires setImageShape before generate');
  }
  if (
    cachedGlyph &&
    cachedGlyph.kind === 'image' &&
    cachedGlyph.bitmap === cachedImageBitmap &&
    cachedGlyph.size === size
  )
    return;
  const sdf = generateImageSdf(cachedImageBitmap, size);
  renderer.setGlyphSdf(sdf, size);
  cachedGlyph = { kind: 'image', bitmap: cachedImageBitmap, size };
}

function mergeParams(p: BaseParams) {
  if (!cachedSource) {
    throw new Error('source not set — call setSource before generate/animate');
  }
  // #160: UI shape='image' は wasm からは shape='glyph' (= SDF テクスチャを
  // サンプルする) として見せる。glyph_char は wasm 内部の SDF キャッシュ
  // キーになるが、こちらでテクスチャを上書き upload するので値は問われない。
  // 念のため非空ダミー ('A') を入れて wasm の glyph_char バリデーションが
  // 弾かないようにする。
  const wasmShape = p.shape === 'image' ? 'glyph' : p.shape;
  const wasmGlyphChar =
    p.shape === 'image' ? 'A' : p.glyph_char;
  return {
    ...p,
    shape: wasmShape,
    glyph_char: wasmGlyphChar,
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
    }
  // #56: 透過 PNG または透過 WebP を返す静止画 alpha 経路。`format` で出し分ける。
  // wasm の get_render_data 出力をそのまま流用し、bg.a だけを 0 に上書きして
  // canvas を描画する。Circle / Glyph どちらの shape でも straight alpha が残る。
  | {
      kind: 'generateOneAlpha';
      id: number;
      params: BaseParams;
      n: number;
      index: number;
      format: 'png' | 'webp';
    }
  // #56: 透過 WebM (VP9 alpha 'keep') を返す動画 alpha 経路。`encodeMp4.ts` と
  // 同じ frame loop だが、エンコーダ・muxer が VP9 + WebM になる。Safari は
  // VP9 alpha 非対応なので、呼び出し側は `vp9AlphaSupported` で事前 probe する。
  | {
      kind: 'animateOneAlpha';
      id: number;
      params: BaseParams;
      n: number;
      index: number;
      totalFrames: number;
    }
  // #56: VP9 alpha encode が使えるかの probe。Safari 検出用。
  | { kind: 'vp9AlphaSupported'; id: number }
  // Phase B (#55): UI が typed-in glyph 文字が同梱フォントに収録されているか
  // 警告表示するための問い合わせ。wasm の has_glyph(NotoSymbols2, ch) を呼ぶ。
  | { kind: 'glyphSupported'; id: number; ch: string }
  // #160: shape='image' で使う画像 (ImageBitmap) を worker にキャッシュする。
  // bitmap は Transferable で zero-copy 転送される (Studio.tsx 側で transfer)。
  | { kind: 'setImageShape'; id: number; bitmap: ImageBitmap };

/// #56: wasm get_render_data の Float32Array header word 3 (= bg.a in 0..1) を 0 に
/// 上書きして「透過背景でレンダリングしてくれ」と shader に依頼する。元 buffer は
/// 破壊しないよう新しい Float32Array を返す（同じ params を続けて非透過レンダリング
/// する将来の経路を壊さないため）。
function withTransparentBackground(data: Float32Array): Float32Array {
  const out = new Float32Array(data.length);
  out.set(data);
  out[3] = 0;
  return out;
}

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
      case 'setImageShape': {
        // 古い bitmap は close して GPU メモリを解放する。新 bitmap を保持。
        if (cachedImageBitmap && cachedImageBitmap !== req.bitmap) {
          cachedImageBitmap.close();
        }
        cachedImageBitmap = req.bitmap;
        // 既存の image SDF キャッシュを invalidate (新 bitmap で再生成させる)。
        if (cachedGlyph && cachedGlyph.kind === 'image') {
          cachedGlyph = null;
        }
        post({ id: req.id, ok: true });
        break;
      }
      case 'generateOne': {
        const params = mergeParams(req.params);
        const data = wasm.get_render_data(params, req.n, req.index);
        const { canvas, renderer } = getRenderer(req.params.width, req.params.height);
        // Glyph 形状なら SDF を 1 度アップロードする。
        // setRenderData の前に呼ぶことで shape_id=1 の uniform が立つ前から
        // テクスチャは正しい状態になる（順序依存はないが、明示的に先に行う）。
        if (req.params.shape === 'image') {
          ensureImageSdfUploaded(renderer);
        } else if (req.params.shape === 'glyph' && req.params.glyph_char) {
          ensureGlyphSdfUploaded(renderer, req.params.glyph_char);
        }
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
        if (req.params.shape === 'image') {
          ensureImageSdfUploaded(renderer);
        } else if (req.params.shape === 'glyph' && req.params.glyph_char) {
          ensureGlyphSdfUploaded(renderer, req.params.glyph_char);
        }
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
      case 'glyphSupported': {
        // 1 char の Unicode scalar 1 つを判定する軽量パス。wasm は init 済み
        // なので I/O コストはゼロに近い。
        const ok = wasm.glyph_supported(req.ch);
        post({ id: req.id, ok: true, data: ok });
        break;
      }
      case 'generateOneAlpha': {
        // #56: 静止画 alpha 経路。non-alpha 経路と同じ wasm get_render_data → setRenderData
        // を辿るが、bg.a だけ 0 に上書きする。`format` で PNG / WebP を出し分ける。
        const params = mergeParams(req.params);
        const data = wasm.get_render_data(params, req.n, req.index);
        const { canvas, renderer } = getRenderer(req.params.width, req.params.height);
        if (req.params.shape === 'image') {
          ensureImageSdfUploaded(renderer);
        } else if (req.params.shape === 'glyph' && req.params.glyph_char) {
          ensureGlyphSdfUploaded(renderer, req.params.glyph_char);
        }
        renderer.setRenderData(withTransparentBackground(data));
        renderer.renderFrame(0);
        const mime = req.format === 'webp' ? 'image/webp' : 'image/png';
        // WebP は quality 指定で alpha 込みでもサイズが大きく変わる。0.9 を選んだ
        // のは「視覚劣化が体感上識別不能」と「PNG 比でファイルサイズ ~30-50% 削減」
        // の落としどころ。PNG は lossless で quality 引数が無視される。
        const blob =
          req.format === 'webp'
            ? await canvas.convertToBlob({ type: mime, quality: 0.9 })
            : await canvas.convertToBlob({ type: mime });
        const buf = await blob.arrayBuffer();
        post({ id: req.id, ok: true, data: buf }, [buf]);
        break;
      }
      case 'animateOneAlpha': {
        // #56: 動画 alpha 経路。frame loop は encodeMp4 と同形だが、エンコーダ・muxer
        // を VP9 alpha + WebM に差し替える。Safari は VP9 alpha 非対応なので、
        // 呼び出し側で `vp9AlphaSupported` を probe して落とすこと。
        const params = mergeParams(req.params);
        const data = wasm.get_render_data(params, req.n, req.index);
        const width = req.params.width;
        const height = req.params.height;
        const { canvas, renderer } = getRenderer(width, height);
        if (req.params.shape === 'image') {
          ensureImageSdfUploaded(renderer);
        } else if (req.params.shape === 'glyph' && req.params.glyph_char) {
          ensureGlyphSdfUploaded(renderer, req.params.glyph_char);
        }
        renderer.setRenderData(withTransparentBackground(data));
        const PROGRESS_STRIDE = 4;
        const blob = await encodeAnimationAlphaFromCanvas(
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
      case 'vp9AlphaSupported': {
        const ok = await isVp9AlphaSupported();
        post({ id: req.id, ok: true, data: ok });
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
