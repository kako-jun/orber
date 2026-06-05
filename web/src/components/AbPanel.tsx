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
//
// 合成条件の非対称（orb ゲートで微差を見たときの一次容疑者リスト）:
//   - WGSL surface は alpha_mode=Opaque、WebGL canvas は
//     `{alpha:true, premultipliedAlpha:false}` で構成される（非対称）。
//   - orber の bg は通常不透明（u_bg.a=1）なので、合成結果に実害はほぼ無い。
//   - ただしリムの反 alias（半透明エッジ）では合成式の違いで微差が出得る。
//     orb パリティで「縁だけわずかに違う」を見たら、まずこの非対称を疑う。
//   - これは認識の明文化であり、コード挙動（surface/canvas の構成）は変えない。
//
// running 中の props 変更:
//   - 実行中に Studio 側の shape / source / image File / glyph / 各プリセットが
//     変わったら、createEffect（defer）で stop → start を自動でやり直す（後述）。
//   - start は async なので世代カウンタで「最新の start のみ有効」を保証する。

import { createEffect, createSignal, on, onCleanup, onMount, Show } from 'solid-js';
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

  // 起動済みリソース。stop / re-start で作り直す（props 変更時の自動再初期化、
  // および手動 Stop ボタンで dispose → 再 setup する）。
  let glRenderer: GlRenderer | null = null;
  let wgslReady = false;
  let rafId: number | undefined;

  // start 再入ガード（世代カウンタ）。start は await を含む async なので、
  // 連続トリガ（props が立て続けに変わる等）で複数の start が並走し得る。
  // start 冒頭で generation を ++ して captured し、各 await 後に
  // generation !== captured なら「より新しい start が走った」とみなして中断する。
  // これで「最新の start のみ有効」を保証し、古い setup が後勝ちで残るのを防ぐ。
  let generation = 0;

  // FPS 計測用カウンタ（rAF ループと共用）。トグル切替時にリセットして、
  // 切替直後の 1 秒が両側（webgl / wgsl）の混合 FPS にならないようにする。
  let frames = 0;
  let lastFps = 0;
  // FPS 計測をリセットする（active 切替直後・start 直後に呼ぶ）。
  function resetFps() {
    frames = 0;
    lastFps = performance.now();
    setFps(null);
  }

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

  // running 中に Studio 側の入力（shape / source / image File / glyph / 各プリセット）が
  // 変わったら、stop → start で作り直して新しい入力を反映する。
  //   - on(..., { defer: true }): マウント時の初回 run は走らせない（手動 Start を待つ）。
  //   - start は内部で stop() を呼んでから作り直すので、ここでは start() だけ呼ぶ。
  //   - start は async + 世代カウンタなので、props が立て続けに変わっても
  //     最新の start のみが有効になる（古い setup が後勝ちで残らない）。
  createEffect(
    on(
      () => [
        props.shape(),
        props.decoded(),
        props.imageShapeFile(),
        props.glyphChar(),
        props.glyphRotate(),
        props.countPreset(),
        props.speedPreset(),
        props.softnessPreset(),
      ],
      () => {
        if (!running()) return; // 停止中は無視（手動 Start でのみ起動する）。
        void start();
      },
      { defer: true },
    ),
  );

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
    resetFps();
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
    // 世代を進めて自分を captured する。これ以降に新しい start が走ると
    // generation が変わるので、各 await の後で「自分が最新か」を確認できる。
    const myGen = ++generation;
    // 既存リソースを必ず畳んでから作り直す（自動再初期化の stop→start 経路でも、
    // 手動 Start の二度押し抑止が外れた経路でも、二重 setup を起こさない）。
    stop();
    // stop() は generation を触らないので myGen はそのまま有効。
    const src = props.decoded();
    if (!src) return;
    setErrorMsg('');
    try {
      await ensureWasm();
      if (myGen !== generation) return; // より新しい start が走った → 中断

      const params = buildBaseParams(src);

      // image shape: 元 File を ImageBitmap → RGBA（WGSL image_mask）+ SDF（WebGL）。
      let imageBitmap: ImageBitmap | null = null;
      if (props.shape() === 'image') {
        const file = props.imageShapeFile();
        if (!file) throw new Error('image shape requires a file');
        const dec = await decodeImageFile(file);
        if (myGen !== generation) return; // より新しい start が走った → 中断
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
        const ms = await setupWgsl(params);
        if (myGen !== generation) {
          // await 中に新しい start が走った。setupWgsl が立てた wgslReady を畳む。
          stop();
          return;
        }
        setWgslInitMs(Number(ms.toFixed(1)));
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

  // レンダラ切替。切替直後の 1 秒が両側混合 FPS にならないよう FPS をリセットする
  // （t は wall-clock 由来で同位相なので描画位相は連続。リセットするのは計測のみ）。
  function toggleTo(side: Side) {
    if (active() === side) return;
    setActive(side);
    resetFps();
  }

  // glass / segmented control の流儀（DESIGN.md §4）に倣う。検証パネルなので最小限。
  // DESIGN.md §4 の正式実装 SEG_GROUP / SEG_BTN は Studio() 内のローカル定数で
  // export されていない（共有 API になっていない）。本パネルは Phase 3 で
  // orberGl.ts ごと削除する足場であり、Studio.tsx の private 実装を export して
  // 公開面を広げるより、足場側で最小の segBtn を再実装する方が実務上妥当と判断した
  // （コードは共有しない）。Phase 3 でパネルごとこの再実装も消える。
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

  // Start / Stop 共通の glass button クラス（DESIGN.md §4 Button の流儀。検証足場
  // なので最小限・共有 API には依存しない。Phase 3 でパネルごと削除する）。
  const CTRL_BTN =
    'px-3.5 py-2 rounded inline-flex items-center justify-center ' +
    'bg-glassBg backdrop-blur-glass border border-glassBorder text-fg ' +
    'hover:bg-glassBgHover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing ' +
    'transition-colors duration-200 ease-out text-sm ' +
    'disabled:opacity-40 disabled:cursor-not-allowed';

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
          onClick={() => toggleTo('webgl')}
          disabled={!running()}
          class={segBtn(active() === 'webgl', !running())}
        >
          {t('abRendererWebGL')}
        </button>
        <button
          type="button"
          aria-pressed={active() === 'wgsl'}
          onClick={() => toggleTo('wgsl')}
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
          class={CTRL_BTN}
        >
          {t('abStart')}
        </button>
        <button
          type="button"
          onClick={() => stop()}
          disabled={!running()}
          class={CTRL_BTN}
        >
          {t('abStop')}
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
