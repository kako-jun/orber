// orber#232 — WebGL ↔ WGSL A/B 比較パネル（検証足場）。
//
// ★ Phase 3 で削除する検証足場 ★
//   一重化（#207）の核心検証用。既存 WebGL 版（Worker + orberGl.ts）と新 WGSL 版
//   （main thread + wasm wgpu）を、同一パラメータ・同一 seed・同一位置でトグル
//   切替（ブリンク比較）し、見た目の一致を目視確認するためだけのパネル。
//   Phase 3 で orberGl.ts を撤去するとき、このコンポーネント・lib/webgpu.ts・
//   strings.ts の ab* キー・Studio.tsx の組み込み（`?ab=1` Show）ごと削除する。
//
// 設計（gpu-lab.astro / 実装方針 #232 に準拠）:
//   - Studio のプレビューは Worker 事前焼き PNG/MP4 でライブ描画が無いため、
//     A/B 専用にライブ描画パネルを新設する（既存生成経路は一切触らない）。
//   - `?ab=1` のときだけ Studio から <Show> でマウントされる（本番 UI 不汚染）。
//   - 同一位置にスタックした canvas 2 枚（WebGL / WGSL）を absolute で重ね、
//     トグルで表示を切替える。非表示側は rAF を止める。t は wall-clock 由来
//     `(now % PERIOD_MS) / PERIOD_MS` なので、切替後も両者で同位相。
//   - 同一入力は Studio の現在状態（source RGB / shape / glyph / image / presets）。
//     seed は固定定数 42（再現性優先）。n=12 / spec_idx=8（gpu-lab と同じ video spec）。
//   - GPU init ms（WebGL=createGlRenderer+setRenderData まで / WGSL=gpu_init）と
//     現在表示側の FPS（1 秒毎更新）を小さく表示する。
//   - WebGPU 非対応ブラウザでは WGSL ボタン disabled + 非対応表示（WebGL は従来どおり）。
//
// 一致期待（#235 前提）:
//   - orb: 新旧で同一見た目のはず → パリティ確認（ゲート）
//   - glyph / image: 新（WGSL=orb 質感）と旧（WebGL=旧 bleed 質感）は一致しないのが正
//   - aquarelle: 対象外（Studio に入らない）

import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import type { DecodedImage } from '../lib/decodeImage';
import { createGlRenderer, GLYPH_SDF_SIZE, type GlRenderer } from '../lib/orberGl';
import { generateImageSdf, generateJsGlyphSdf } from '../lib/jsGlyphSdf';
import { isWebGpuSupported } from '../lib/webgpu';
import {
  abCanStart,
  buildAbBaseParams,
  CANVAS_W,
  CANVAS_H,
  PERIOD_MS,
  AB_N,
  AB_SPEC_IDX,
  type ShapeChoice,
} from '../lib/abLogic';
import { t } from '../lib/strings';
import initWasm, {
  get_render_data,
  get_glyph_sdf,
  glyph_supported,
  gpu_init,
  gpu_render,
  gpu_set_render_data,
} from '../wasm/orber_wasm.js';

type Side = 'webgl' | 'wgsl';

// Studio が保持する現在状態を accessor で受け取る（A/B は読むだけ・書き込まない）。
export interface AbPanelProps {
  decoded: () => DecodedImage | null;
  shape: () => ShapeChoice;
  glyphChar: () => string;
  glyphRotate: () => boolean;
  countPreset: () => string;
  speedPreset: () => string;
  softnessPreset: () => string;
  // shape='image' のときに使う元 File（Studio の image-shape 入力と同じもの）。
  imageShapeFile: () => File | null;
}

// 主スレッド wasm init は 1 回だけ（gpu-lab と同じ多重 init ガード）。
// Studio の Worker 側 wasm とは別インスタンス（main thread に閉じる）。
let wasmInitPromise: Promise<unknown> | null = null;
function ensureWasm(): Promise<unknown> {
  if (!wasmInitPromise) wasmInitPromise = initWasm();
  return wasmInitPromise;
}

// File → ImageBitmap → RGBA bytes（WGSL image_mask 用）。gpu-lab の素朴
// デコードと同じく、本番 decodeImage.ts のような縮小はしない（A/B は等倍）。
async function decodeImageFile(
  file: File,
): Promise<{ rgba: Uint8Array; width: number; height: number; bitmap: ImageBitmap }> {
  const bitmap = await createImageBitmap(file);
  const c = document.createElement('canvas');
  c.width = bitmap.width;
  c.height = bitmap.height;
  const ctx = c.getContext('2d');
  if (!ctx) throw new Error('canvas 2d context unavailable');
  ctx.drawImage(bitmap, 0, 0);
  const data = ctx.getImageData(0, 0, bitmap.width, bitmap.height).data;
  return {
    rgba: new Uint8Array(data.buffer.slice(0)),
    width: bitmap.width,
    height: bitmap.height,
    bitmap,
  };
}

export default function AbPanel(props: AbPanelProps) {
  const webgpuOk = isWebGpuSupported();

  let webglCanvas: HTMLCanvasElement | undefined;
  let wgslCanvas: HTMLCanvasElement | undefined;

  const [active, setActive] = createSignal<Side>('webgl');
  const [running, setRunning] = createSignal(false);
  const [webglInitMs, setWebglInitMs] = createSignal<number | null>(null);
  const [wgslInitMs, setWgslInitMs] = createSignal<number | null>(null);
  const [fps, setFps] = createSignal<number | null>(null);
  const [errorMsg, setErrorMsg] = createSignal('');

  // 起動済みリソース。stop / re-start で作り直す。
  let glRenderer: GlRenderer | null = null;
  let wgslReady = false;
  let rafId: number | undefined;

  const hasSource = () => props.decoded() !== null;
  // image shape のとき File 未選択なら開始不可（image_mask が組めない）。
  const canStart = () =>
    abCanStart(props.shape(), hasSource(), props.imageShapeFile() !== null);

  // 現在の Studio 状態から両レンダラ共通の params を組む。
  // shape 依存の追加フィールド（image_mask / glyph_sdf）は呼び出し側で足す。
  function buildBaseParams(src: DecodedImage): Record<string, unknown> {
    return buildAbBaseParams(
      src,
      props.shape(),
      props.glyphChar(),
      props.glyphRotate(),
      props.countPreset(),
      props.speedPreset(),
      props.softnessPreset(),
    );
  }

  // 停止処理: rAF を止め、リソースを破棄して計測をリセットする。
  function stop() {
    if (rafId !== undefined) {
      cancelAnimationFrame(rafId);
      rafId = undefined;
    }
    if (glRenderer) {
      glRenderer.dispose();
      glRenderer = null;
    }
    wgslReady = false;
    setRunning(false);
    setFps(null);
    setWebglInitMs(null);
    setWgslInitMs(null);
  }

  onCleanup(() => stop());

  // WebGL 側を初期化する（createGlRenderer + get_render_data → setRenderData、
  // glyph/image は #159 ロジックを main thread で再現）。戻り値 = init ms。
  // `imageBitmap` は shape='image' のとき必須（generateImageSdf 用）。
  function setupWebgl(
    params: Record<string, unknown>,
    imageBitmap: ImageBitmap | null,
  ): number {
    if (!webglCanvas) throw new Error('webgl canvas not mounted');
    const t0 = performance.now();
    const renderer = createGlRenderer(webglCanvas);
    renderer.setResolution(CANVAS_W, CANVAS_H);
    const data = get_render_data(params, AB_N, AB_SPEC_IDX);
    renderer.setRenderData(data);

    // glyph/image の SDF を main thread で再現して setGlyphSdf する。
    const shape = props.shape();
    if (shape === 'glyph') {
      const ch = Array.from(props.glyphChar())[0] ?? '';
      if (ch) {
        // 同梱フォント内なら wasm の get_glyph_sdf、外なら OS フォント fallback。
        const sdf = glyph_supported(ch)
          ? get_glyph_sdf(ch, GLYPH_SDF_SIZE)
          : generateJsGlyphSdf(ch, GLYPH_SDF_SIZE);
        renderer.setGlyphSdf(sdf, GLYPH_SDF_SIZE);
      }
    } else if (shape === 'image' && imageBitmap) {
      const { sdf } = generateImageSdf(imageBitmap, GLYPH_SDF_SIZE);
      renderer.setGlyphSdf(sdf, GLYPH_SDF_SIZE);
    }

    glRenderer = renderer;
    return performance.now() - t0;
  }

  // WGSL 側を初期化する（gpu_init + gpu_set_render_data）。戻り値 = init ms。
  // gpu_init は init ms（adapter/device 取得）を計測する。
  async function setupWgsl(params: Record<string, unknown>): Promise<number> {
    if (!wgslCanvas) throw new Error('wgsl canvas not mounted');
    const t0 = performance.now();
    await gpu_init(wgslCanvas);
    const initMs = performance.now() - t0;
    gpu_set_render_data(params, AB_N, AB_SPEC_IDX);
    wgslReady = true;
    return initMs;
  }

  // 1 フレーム描画する。active な側だけを描く（非表示側は rAF を回さない）。
  function renderActive(now: number) {
    const tNorm = (now % PERIOD_MS) / PERIOD_MS;
    if (active() === 'webgl') {
      glRenderer?.renderFrame(tNorm);
    } else if (wgslReady) {
      gpu_render(tNorm);
    }
  }

  // rAF ループ（FPS は 1 秒毎に更新）。例外は status に流してループ停止する
  // （gpu-lab と同じく無言で死なせない）。
  function startLoop() {
    let frames = 0;
    let lastFps = performance.now();
    const loop = (now: number) => {
      try {
        renderActive(now);
      } catch (e) {
        console.error('[ab-panel]', e);
        setErrorMsg(e instanceof Error ? e.message : String(e));
        stop();
        return;
      }
      frames += 1;
      if (now - lastFps >= 1000) {
        setFps(Number(((frames * 1000) / (now - lastFps)).toFixed(1)));
        frames = 0;
        lastFps = now;
      }
      rafId = requestAnimationFrame(loop);
    };
    rafId = requestAnimationFrame(loop);
  }

  const start = async () => {
    if (running()) return;
    const src = props.decoded();
    if (!src) return;
    setErrorMsg('');
    try {
      await ensureWasm();

      const params = buildBaseParams(src);

      // image shape: 元 File を ImageBitmap → RGBA（WGSL image_mask）+ SDF（WebGL）。
      let imageBitmap: ImageBitmap | null = null;
      if (props.shape() === 'image') {
        const file = props.imageShapeFile();
        if (!file) throw new Error('image shape requires a file');
        const dec = await decodeImageFile(file);
        params.image_mask_rgba = dec.rgba;
        params.image_mask_width = dec.width;
        params.image_mask_height = dec.height;
        imageBitmap = dec.bitmap;
      }

      // glyph で同梱フォント外の字は WGSL 側も JS SDF を渡す（#231 と同設計）。
      if (props.shape() === 'glyph') {
        const ch = Array.from(props.glyphChar())[0] ?? '';
        if (ch && !glyph_supported(ch)) {
          params.glyph_sdf = generateJsGlyphSdf(ch, GLYPH_SDF_SIZE);
          params.glyph_sdf_size = GLYPH_SDF_SIZE;
        }
      }

      // WebGL は常に立てる。WGSL は非対応ブラウザでは立てない（active も webgl 固定）。
      setWebglInitMs(Number(setupWebgl(params, imageBitmap).toFixed(1)));
      if (webgpuOk) {
        setWgslInitMs(Number((await setupWgsl(params)).toFixed(1)));
      }

      setRunning(true);
      startLoop();
    } catch (e) {
      console.error('[ab-panel]', e);
      setErrorMsg(e instanceof Error ? e.message : String(e));
      stop();
    }
  };

  // SSR 安全化: navigator / wasm import は client でのみ意味を持つ。Studio が
  // client:only でマウントするので onMount は client で必ず走る。
  onMount(() => {
    // 非対応ブラウザでは active を webgl に固定する（WGSL ボタンは disabled）。
    if (!webgpuOk) setActive('webgl');
  });

  // glass / segmented control の流儀（DESIGN.md §4）に倣う。検証パネルなので最小限。
  const segBtn = (selected: boolean, disabled: boolean) =>
    [
      'flex-1 h-9 px-2 text-sm flex items-center justify-center transition-colors duration-200 ease-out',
      'focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing',
      'disabled:opacity-40 disabled:cursor-not-allowed',
      selected
        ? 'bg-fg/15 text-fg'
        : 'bg-glassBg text-fgMuted hover:text-fg hover:bg-glassBgHover',
    ]
      .filter(Boolean)
      .join(' ');

  return (
    <section class="mx-auto mt-8 max-w-md rounded-xl border border-glassBorder bg-glassBg p-4 backdrop-blur-glass space-y-3">
      <div class="space-y-1">
        <h2 class="text-sm font-medium text-fg">{t('abPanelTitle')}</h2>
        <p class="text-xs text-fgMuted leading-relaxed">{t('abPanelNote')}</p>
      </div>

      {/* renderer 切替 segmented control（WebGL / WGSL）。 */}
      <div class="inline-flex w-full rounded-md overflow-hidden border border-glassBorder">
        <button
          type="button"
          aria-pressed={active() === 'webgl'}
          onClick={() => setActive('webgl')}
          disabled={!running()}
          class={segBtn(active() === 'webgl', !running())}
        >
          {t('abRendererWebGL')}
        </button>
        <button
          type="button"
          aria-pressed={active() === 'wgsl'}
          onClick={() => setActive('wgsl')}
          disabled={!running() || !webgpuOk}
          title={!webgpuOk ? t('abWebGpuUnavailable') : undefined}
          class={'border-l border-glassBorder ' + segBtn(active() === 'wgsl', !running() || !webgpuOk)}
        >
          {t('abRendererWGSL')}
        </button>
      </div>

      <Show when={!webgpuOk}>
        <p class="text-xs text-fgMuted">{t('abWebGpuUnavailable')}</p>
      </Show>

      {/* 同一位置にスタックした canvas 2 枚（absolute 重ね）。active 側だけ visible。 */}
      <div
        class="relative mx-auto overflow-hidden rounded border border-glassBorder bg-bg"
        style={{ width: `${CANVAS_W}px`, height: `${CANVAS_H}px` }}
      >
        <canvas
          ref={webglCanvas}
          width={CANVAS_W}
          height={CANVAS_H}
          class="absolute inset-0 block h-full w-full"
          style={{ opacity: active() === 'webgl' ? '1' : '0' }}
        />
        <canvas
          ref={wgslCanvas}
          width={CANVAS_W}
          height={CANVAS_H}
          class="absolute inset-0 block h-full w-full"
          style={{ opacity: active() === 'wgsl' ? '1' : '0' }}
        />
      </div>

      {/* 計測表示（init ms / FPS）。 */}
      <div class="flex flex-wrap items-center justify-between gap-2 text-xs text-fgMuted">
        <span>
          {t('abRendererWebGL')}:{' '}
          <Show when={webglInitMs() !== null} fallback="—">
            {t('abInitMs', { ms: String(webglInitMs()) })}
          </Show>
        </span>
        <span>
          {t('abRendererWGSL')}:{' '}
          <Show when={wgslInitMs() !== null} fallback={webgpuOk ? '—' : '×'}>
            {t('abInitMs', { ms: String(wgslInitMs()) })}
          </Show>
        </span>
        <span>
          <Show when={fps() !== null} fallback="—">
            {t('abFps', { fps: String(fps()) })}
          </Show>
        </span>
      </div>

      <div class="flex items-center justify-center gap-3">
        <button
          type="button"
          onClick={() => void start()}
          disabled={running() || !canStart()}
          title={!canStart() ? t('abNeedSource') : undefined}
          class={
            'px-3.5 py-2 rounded inline-flex items-center justify-center ' +
            'bg-glassBg backdrop-blur-glass border border-glassBorder text-fg ' +
            'hover:bg-glassBgHover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing ' +
            'transition-colors duration-200 ease-out text-sm ' +
            'disabled:opacity-40 disabled:cursor-not-allowed'
          }
        >
          {t('abStart')}
        </button>
        <Show when={!canStart()}>
          <span class="text-xs text-fgMuted">{t('abNeedSource')}</span>
        </Show>
      </div>

      <Show when={errorMsg()}>
        <p role="alert" class="text-xs text-fg">
          {t('abError', { msg: errorMsg() })}
        </p>
      </Show>
    </section>
  );
}
