// orber#75 / #112 / #245 — wasm + WebGPU(WGSL) 描画 + WebCodecs エンコードを
// Worker スレッドで実行する。
//
// メインスレッドは UI / DOM / Solid signal だけに集中させ、重い計算は全部
// ここに逃がす。これによりスマホでも生成中にスクロール / タップが死なない。
//
// アーキテクチャ (#245 で WebGL2 → WebGPU(WGSL) に配線替え):
//   main → postMessage({ kind, id, ... }) → worker
//   worker → wasm.gpu_set_render_data → WebGPU (OffscreenCanvas surface,
//            orber-core の WGSL シェーダ) 描画 →
//            convertToBlob (PNG) or VideoEncoder (mp4) → main
//
// 旧 WebGL2 経路 (orberGl.ts / get_render_data) との違い:
//   - 描画は wasm 内部の core WGSL（CLI と同一シェーダ）。pack / SDF の面倒は
//     全部 wasm 側が見るので、worker は params を渡して t を回すだけ
//   - glyph / image の入力は #231 の WasmParams 経路（同梱フォント外の字は
//     glyph_sdf / glyph_sdf_size、image は image_mask_rgba / width / height）。
//     SDF テクスチャの GPU upload を worker が直接行うことはもう無い
//   - 透過 export は WasmParams.transparent_background + gpu_render_rgba
//     （canvas 非経由の straight-alpha readback）。WebGPU canvas の alphaMode は
//     opaque / premultiplied しかなく、旧 WebGL の「straight alpha のまま
//     convertToBlob」が成立しないため
//   - 出力ルックは #242（旧の明るさ）+ #241（薄い影 s=0.2）の確定ルックに変わる
//     （#245 の目的そのもの）
//
// データ転送:
//   - PNG / mp4 の ArrayBuffer は Transferable で zero-copy 返却
//   - source RGB は `setSource` で 1 度だけ送って worker 側にキャッシュする
//
// 互換性: OffscreenCanvas + WebGPU + VideoEncoder/VideoFrame in Worker が要る。
// WebGPU 非対応ブラウザはエラー表示で生成不可（#207 裁定: fallback 無し）。
// エラーは sentinel `webgpu-unsupported` を前置して main へ返し、Studio 側で
// i18n 文言（strings.ts `webgpuUnsupported`）にマップする。

import init, * as wasm from '../wasm/orber_wasm.js';
import { encodeAnimationFromCanvas } from './encodeMp4';
import { generateJsGlyphSdf } from './jsGlyphSdf';

// SDF テクスチャの一辺 (px)。core の DEFAULT_GLYPH_SDF_SIZE = 256 と同値で、
// wasm の get_glyph_sdf / glyph_sdf_size 検証 (16..=1024) にも収まる。
// （旧 orberGl.ts の export を引き継いだ worker ローカル定数。#245）
// SYNC WITH crates/core/src/glyph.rs::DEFAULT_GLYPH_SDF_SIZE
const GLYPH_SDF_SIZE = 256;

// shape='image' のマスクを worker 内でデコードするときの長辺上限 (px)。
// SDF は最終的に GLYPH_SDF_SIZE (256) へ contain リサンプルされる
// (core の image_rgba_to_sdf) ので、4 倍の 1024 あればシルエット品質は
// 落ちない。フル解像度 (数千 px) のまま持つと、タイルごとの
// gpu_set_render_data で数十 MB の RGBA が wasm へコピーされ続けるため、
// デコード時に 1 度だけ縮めて転送量と変換コストを固定する。
const IMAGE_MASK_TARGET_LONG_EDGE = 1024;

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

// WebGPU surface を張った OffscreenCanvas。gpu_init_offscreen は adapter /
// device の取得を伴い重いので 1 度だけ行い、アスペクト切替や preview / hi-res
// の解像度切替は canvas 属性の変更 + gpu_resize（surface 再 configure）で済ます。
let gpuCanvas: { canvas: OffscreenCanvas; width: number; height: number } | null = null;

// 同梱フォント外の字 (絵文字 / 漢字 / 任意 Unicode) の JS 生成 SDF キャッシュ。
// generateJsGlyphSdf (OS フォントスタックでラスタライズ → EDT) は 1 文字
// ~数ms だが、バッチ 16 タイルで毎回作り直さないよう ch 単位で持つ。
// wasm へは WasmParams.glyph_sdf として毎回コピーされる (64KB、許容コスト)。
let cachedJsGlyphSdf: { ch: string; sdf: Uint8Array } | null = null;

// #160 / #245: shape='image' のマスク RGBA。setImageShape で File →
// ImageBitmap → 2D canvas デコードして保持し、タイルごとに
// WasmParams.image_mask_* として wasm に渡す (SDF 化は core の
// image_rgba_to_sdf、#231 の WGSL 経路と同じ)。
let cachedImageMask: { rgba: Uint8Array; width: number; height: number } | null = null;

// 透過 export 用の 2D スクラッチ canvas (gpu_render_rgba の RGBA → Blob 化)。
let alphaScratch: OffscreenCanvas | null = null;

/**
 * WebGPU surface 付き OffscreenCanvas を返す。初回は gpu_init_offscreen で
 * bring-up し、以降のサイズ変更は canvas 属性 + gpu_resize で追従する。
 *
 * WebGPU 不在 (navigator.gpu 無し / adapter 拒否) は sentinel
 * `webgpu-unsupported` を前置した Error で reject する (#207: fallback 無し)。
 * Studio は formatRunBatchError でこの sentinel を i18n 文言にマップする。
 */
async function ensureGpuCanvas(width: number, height: number): Promise<OffscreenCanvas> {
  if (gpuCanvas) {
    if (gpuCanvas.width !== width || gpuCanvas.height !== height) {
      gpuCanvas.canvas.width = width;
      gpuCanvas.canvas.height = height;
      wasm.gpu_resize(width, height);
      gpuCanvas.width = width;
      gpuCanvas.height = height;
    }
    return gpuCanvas.canvas;
  }
  if (!('gpu' in navigator)) {
    throw new Error('webgpu-unsupported: navigator.gpu is not available in this worker');
  }
  const canvas = new OffscreenCanvas(width, height);
  try {
    await wasm.gpu_init_offscreen(canvas);
  } catch (e) {
    // navigator.gpu はあるが adapter が取れない環境 (ブロック / ドライバ拒否)
    // もここに来る。詳細は残しつつ sentinel を前置して main に渡す。
    throw new Error(`webgpu-unsupported: ${e instanceof Error ? e.message : String(e)}`);
  }
  gpuCanvas = { canvas, width, height };
  return canvas;
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

/**
 * BaseParams + worker キャッシュ (source / image mask / glyph SDF) から
 * wasm の WasmParams を組む。
 *
 * - shape='glyph': 同梱フォント収録字は wasm の core フォント経路
 *   (glyph_char) に任せる。収録外の字は generateJsGlyphSdf で SDF 化して
 *   glyph_sdf / glyph_sdf_size に乗せる (#231 / #159 と同設計)
 * - shape='image': cachedImageMask を image_mask_* に乗せる (#231)。
 *   SDF 化は core の image_rgba_to_sdf
 * - transparentBackground: 透過 export (#56)。wasm 側で bg.a=0 になる
 */
function buildWasmParams(
  p: BaseParams,
  opts?: { transparentBackground?: boolean },
): Record<string, unknown> {
  if (!cachedSource) {
    throw new Error('source not set — call setSource before generate/animate');
  }
  const params: Record<string, unknown> = {
    ...p,
    source_rgb: cachedSource.rgb,
    source_width: cachedSource.width,
    source_height: cachedSource.height,
  };
  if (p.shape === 'image') {
    if (!cachedImageMask) {
      throw new Error('image shape requires setImageShape before generate');
    }
    params.image_mask_rgba = cachedImageMask.rgba;
    params.image_mask_width = cachedImageMask.width;
    params.image_mask_height = cachedImageMask.height;
  } else if (p.shape === 'glyph' && p.glyph_char && !wasm.glyph_supported(p.glyph_char)) {
    // #159 / #231: 同梱フォント (Noto Sans Symbols 2 サブセット) に無い字は
    // worker 内 OffscreenCanvas + OS フォントスタックでラスタライズして SDF 化
    // する。端末ごとに見た目が変わり得るが、「ユーザーが入れた字を尊重して
    // 描画する」を優先する仕様 (WebGL 時代から不変)。
    if (!cachedJsGlyphSdf || cachedJsGlyphSdf.ch !== p.glyph_char) {
      cachedJsGlyphSdf = { ch: p.glyph_char, sdf: generateJsGlyphSdf(p.glyph_char, GLYPH_SDF_SIZE) };
    }
    params.glyph_sdf = cachedJsGlyphSdf.sdf;
    params.glyph_sdf_size = GLYPH_SDF_SIZE;
  }
  if (opts?.transparentBackground) {
    params.transparent_background = true;
  }
  return params;
}

/**
 * gpu_set_render_data の薄いラッパ。wasm 内部のエラー文言を main 側の既存
 * sentinel にマップする: core の image_rgba_to_sdf がコントラスト不足で
 * 失敗したら `image-shape-no-contrast` (#169。Studio が i18n 文言に変換)。
 */
function setRenderData(params: Record<string, unknown>, n: number, specIdx: number): void {
  try {
    wasm.gpu_set_render_data(params, n, specIdx);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    if (msg.includes('no usable silhouette contrast')) {
      throw new Error('image-shape-no-contrast');
    }
    throw e;
  }
}

/**
 * gpu_render_rgba が返した straight-alpha RGBA を PNG / WebP Blob にする。
 *
 * 2D canvas は内部表現が premultiplied のため、putImageData → convertToBlob
 * の往復で 0 < a < 255 の画素の RGB に ±数値の量子化が乗り得る (alpha と
 * 合成結果 rgb×a は u8 精度で保たれるので視覚上は同一)。PNG を完全 exact に
 * したければ wasm 側エンコードに切替える余地があるが、192 frame の透過動画
 * でブラウザネイティブの PNG エンコーダ速度を優先してこの経路を採る。
 */
function rgbaToBlob(
  rgba: Uint8Array,
  width: number,
  height: number,
  type: string,
  quality?: number,
): Promise<Blob> {
  if (!alphaScratch || alphaScratch.width !== width || alphaScratch.height !== height) {
    alphaScratch = new OffscreenCanvas(width, height);
  }
  const ctx = alphaScratch.getContext('2d');
  if (!ctx) throw new Error('2d context unavailable for alpha export');
  // wasm-bindgen が返す Uint8Array は通常の ArrayBuffer 裏打ちの新規コピー。
  // ImageData の型 (Uint8ClampedArray<ArrayBuffer>) に合わせて cast する。
  const img = new ImageData(
    new Uint8ClampedArray(rgba.buffer as ArrayBuffer, rgba.byteOffset, rgba.byteLength),
    width,
    height,
  );
  ctx.putImageData(img, 0, 0);
  return quality === undefined
    ? alphaScratch.convertToBlob({ type })
    : alphaScratch.convertToBlob({ type, quality });
}

/**
 * #160 / #245: File からデコードした ImageBitmap をマスク RGBA に変換する。
 * 長辺 IMAGE_MASK_TARGET_LONG_EDGE まで縮小 (アスペクト維持) して、以降の
 * タイルごとの wasm 転送・SDF 変換コストをソース解像度から切り離す。
 */
function decodeBitmapToMask(bitmap: ImageBitmap): {
  rgba: Uint8Array;
  width: number;
  height: number;
} {
  const longest = Math.max(bitmap.width, bitmap.height);
  const scale = Math.min(1, IMAGE_MASK_TARGET_LONG_EDGE / Math.max(1, longest));
  const w = Math.max(1, Math.round(bitmap.width * scale));
  const h = Math.max(1, Math.round(bitmap.height * scale));
  const canvas = new OffscreenCanvas(w, h);
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  if (!ctx) throw new Error('2d context unavailable for image mask decode');
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = 'medium';
  ctx.drawImage(bitmap, 0, 0, w, h);
  const data = ctx.getImageData(0, 0, w, h).data;
  return { rgba: new Uint8Array(data.buffer), width: w, height: h };
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
  // #245: WasmParams.transparent_background で bg.a=0 にし、gpu_render_rgba
  // (canvas 非経由の straight-alpha readback) で取り出す。
  | {
      kind: 'generateOneAlpha';
      id: number;
      params: BaseParams;
      n: number;
      index: number;
      format: 'png' | 'webp';
    }
  // #184/#192: 透過動画用の PNG フレーム列を返す。worker は wasm 経路で各 frame
  // (透過背景 + orb) を readback → PNG 化 → progress message で 1 枚ずつ main に
  // 流す。main 側は JS-only MOV muxer (`movMuxer.ts`) で PNG-in-MOV に詰める。
  // 責務 (描画 = worker / コンテナ組立 = main) の分離は #184 以来そのまま維持。
  | {
      kind: 'renderAlphaFrames';
      id: number;
      params: BaseParams;
      n: number;
      index: number;
      totalFrames: number;
    }
  // Phase B (#55): UI が typed-in glyph 文字が同梱フォントに収録されているか
  // 警告表示するための問い合わせ。wasm の has_glyph(NotoSymbols2, ch) を呼ぶ。
  | { kind: 'glyphSupported'; id: number; ch: string }
  // #160: shape='image' で使う画像 (File) を worker にキャッシュする。
  // worker 側で createImageBitmap(file) → マスク RGBA 化する。
  // Transferable を使わず structured-clone で渡す ── main 側が File 参照
  // を保持し続けることで、worker クラッシュ / terminateAndRespawn 後の
  // 再 upload が可能になる。
  | { kind: 'setImageShape'; id: number; file: File };

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
        // File を worker 内でデコードしてマスク RGBA 化する (#245: ImageBitmap
        // 保持から RGBA 保持に変更。SDF 化は wasm/core 側)。decode 失敗時は
        // 呼び出し側に error を返す (Studio 側で UI 通知)。
        const bitmap = await createImageBitmap(req.file);
        try {
          cachedImageMask = decodeBitmapToMask(bitmap);
        } finally {
          bitmap.close();
        }
        post({ id: req.id, ok: true });
        break;
      }
      case 'generateOne': {
        const params = buildWasmParams(req.params);
        const canvas = await ensureGpuCanvas(req.params.width, req.params.height);
        setRenderData(params, req.n, req.index);
        // gpu_render → convertToBlob は同一タスク内で行うこと: WebGPU canvas の
        // current texture はタスクをまたぐと present されて expire する
        // (AbPanel のキャプチャ実装と同じ制約。convertToBlob のスナップショット
        // 自体は呼び出し時点で同期に取られる)。
        wasm.gpu_render(0);
        const blob = await canvas.convertToBlob({ type: 'image/png' });
        const buf = await blob.arrayBuffer();
        post({ id: req.id, ok: true, data: buf }, [buf]);
        break;
      }
      case 'animateOne': {
        const params = buildWasmParams(req.params);
        const width = req.params.width;
        const height = req.params.height;
        const canvas = await ensureGpuCanvas(width, height);
        setRenderData(params, req.n, req.index);
        const PROGRESS_STRIDE = 4;
        const blob = await encodeAnimationFromCanvas(
          canvas,
          (t) => wasm.gpu_render(t),
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
        // #56: 静止画 alpha 経路。non-alpha 経路と同じ spec 解決を辿るが、
        // transparent_background で bg.a だけ 0 になる。`format` で PNG / WebP を
        // 出し分ける。
        const params = buildWasmParams(req.params, { transparentBackground: true });
        await ensureGpuCanvas(req.params.width, req.params.height);
        setRenderData(params, req.n, req.index);
        const rgba: Uint8Array = await wasm.gpu_render_rgba(0);
        const mime = req.format === 'webp' ? 'image/webp' : 'image/png';
        // WebP は quality 指定で alpha 込みでもサイズが大きく変わる。0.9 を選んだ
        // のは「視覚劣化が体感上識別不能」と「PNG 比でファイルサイズ ~30-50% 削減」
        // の落としどころ。PNG は lossless で quality 引数が無視される。
        const blob = await rgbaToBlob(
          rgba,
          req.params.width,
          req.params.height,
          mime,
          req.format === 'webp' ? 0.9 : undefined,
        );
        const buf = await blob.arrayBuffer();
        post({ id: req.id, ok: true, data: buf }, [buf]);
        break;
      }
      case 'renderAlphaFrames': {
        // #184: 透過動画用 PNG フレーム列 (worker → main)。各 frame は
        // `alphaFrame` kind の message で 1 枚ずつ Transferable 経由で送る。
        // 全 frame を flat array で抱え込まないことで メモリと postMessage の
        // 一括コピーコストを抑える。main 側は JS-only MOV muxer に投入する。
        // 進捗 UI は `animateProgress` を流用 (encodeMp4 と同形)。
        const params = buildWasmParams(req.params, { transparentBackground: true });
        const width = req.params.width;
        const height = req.params.height;
        await ensureGpuCanvas(width, height);
        setRenderData(params, req.n, req.index);
        const PROGRESS_STRIDE = 4;
        for (let i = 0; i < req.totalFrames; i++) {
          const t = i / req.totalFrames;
          const rgba: Uint8Array = await wasm.gpu_render_rgba(t);
          const blob = await rgbaToBlob(rgba, width, height, 'image/png');
          const buf = await blob.arrayBuffer();
          post(
            {
              kind: 'alphaFrame',
              id: req.id,
              frame: i,
              total: req.totalFrames,
              data: buf,
            },
            [buf],
          );
          if (i + 1 === req.totalFrames || (i + 1) % PROGRESS_STRIDE === 0) {
            post({
              kind: 'animateProgress',
              id: req.id,
              frame: i + 1,
              total: req.totalFrames,
            });
          }
        }
        post({ id: req.id, ok: true });
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
