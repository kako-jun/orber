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
//
// #242 キャプチャ足場（これも Phase 3 でパネルごと削除）:
//   - `?ab=1&abcap=1` = キャプチャモード。ファイル選択不要の合成ソース
//     （abLogic.buildSyntheticSourceRgb、Rust ab_harness と同一式）で shape=orb
//     固定・t=0 固定の 1 フレームだけを WGSL / WebGL 各 1 枚描画し、
//     ab-wgsl.png / ab-webgl.png / ab-params.json / ab-source.bin を自動 DL する。
//     rAF ループは回さない（CLI の still PNG が t=0 なのに合わせる。
//     crates/cli/src/main.rs の OutputMode::Png 分岐参照）。
//   - 通常の `?ab=1` 実行中にも「Capture t=0」ボタンで実画像の同 4 ファイルを
//     落とせる（kako-jun の実機確認用）。
//   - 画素取得は描画と同一タスク内で行う: WebGL は preserveDrawingBuffer=false
//     （orberGl.ts）なので readPixels を renderFrame 直後に同期で叩き、WebGPU は
//     gpu_render 直後に drawImage で 2D canvas へ snapshot する（どちらも同一
//     タスクならクリア前のバッファを確実に拾える）。全画素が真っ黒/透明なら
//     キャプチャ失敗としてエラー表示する（無言で成功扱いしない）。
//   - 既存の blink 動作（abcap なし）は一切変えない（キャプチャは独立リソース）。

import { createEffect, createSignal, on, onCleanup, onMount, Show } from 'solid-js';
import type { DecodedImage } from '../lib/decodeImage';
import { createGlRenderer, GLYPH_SDF_SIZE, type GlRenderer } from '../lib/orberGl';
import { generateImageSdf, generateJsGlyphSdf } from '../lib/jsGlyphSdf';
import { isWebGpuSupported } from '../lib/webgpu';
import {
  abCanStart,
  buildAbBaseParams,
  buildAbCaptureMeta,
  buildSyntheticSourceRgb,
  isAllBlackOrTransparent,
  segToggleDisabled,
  AB_CAPTURE_SOURCE_W,
  AB_CAPTURE_SOURCE_H,
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

// ---- #242 キャプチャ用 DOM ヘルパ（Phase 3 でパネルごと削除） ----------------

// WebGL canvas の現在の drawing buffer を ImageData として読む。
// orberGl.ts は preserveDrawingBuffer=false で context を作るため、合成後に
// バッファがクリアされうる。renderFrame と**同一タスク内**で同期 readPixels
// すればクリア前の画素を確実に拾える（toBlob のコールバック経由より確実）。
// `getContext('webgl2')` は 2 回目以降は既存 context をそのまま返す（attributes
// は無視される）ので、orberGl.ts の context をそのまま借りられる。
function readWebGlPixels(canvas: HTMLCanvasElement): ImageData {
  const gl = canvas.getContext('webgl2') as WebGL2RenderingContext | null;
  if (!gl) throw new Error('webgl2 context unavailable for capture');
  const w = canvas.width;
  const h = canvas.height;
  const raw = new Uint8Array(w * h * 4);
  gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, raw);
  // readPixels は左下原点。PNG / CLI（top-left, image::RgbaImage）に合わせて
  // 行を上下反転する。
  const flipped = new Uint8ClampedArray(w * h * 4);
  for (let y = 0; y < h; y++) {
    flipped.set(raw.subarray((h - 1 - y) * w * 4, (h - y) * w * 4), y * w * 4);
  }
  return new ImageData(flipped, w, h);
}

// WebGPU canvas を 2D canvas へ snapshot して ImageData として読む。
// WebGPU の canvas image は「現在テクスチャへの submit 済み書き込み」を同一
// タスク内なら drawImage で拾える（タスクをまたぐと present 済みテクスチャが
// expire して空になる）。gpu_render 直後に呼ぶこと。
function snapshotCanvasPixels(canvas: HTMLCanvasElement): ImageData {
  const c = document.createElement('canvas');
  c.width = canvas.width;
  c.height = canvas.height;
  const ctx = c.getContext('2d');
  if (!ctx) throw new Error('canvas 2d context unavailable for capture');
  ctx.drawImage(canvas, 0, 0);
  return ctx.getImageData(0, 0, c.width, c.height);
}

// ImageData を PNG Blob にエンコードする。画素は取得済みなので、この encode は
// 同一タスクである必要はない（toBlob のコールバックは非同期で良い）。
function imageDataToPngBlob(img: ImageData): Promise<Blob> {
  return new Promise((resolve, reject) => {
    const c = document.createElement('canvas');
    c.width = img.width;
    c.height = img.height;
    const ctx = c.getContext('2d');
    if (!ctx) {
      reject(new Error('canvas 2d context unavailable for png encode'));
      return;
    }
    ctx.putImageData(img, 0, 0);
    c.toBlob(
      (b) => (b ? resolve(b) : reject(new Error('toBlob returned null'))),
      'image/png',
    );
  });
}

// Blob をファイル名付きでダウンロードさせる（dev 足場の素朴な <a download>）。
// 4 ファイル連続 DL はブラウザが「複数ファイルの DL を許可しますか」を 1 度
// 聞くことがある（許可すれば以後は全部落ちる）。
function downloadBlob(filename: string, blob: Blob): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // revoke は DL 開始後で良い（即時 revoke は一部ブラウザで DL が空振りする）。
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

export default function AbPanel(props: AbPanelProps) {
  const webgpuOk = isWebGpuSupported();

  // #242: `?ab=1&abcap=1` = キャプチャモード（合成ソース・t=0 固定・rAF なし）。
  // Studio が client:only でマウントするので window は常にあるが、SSR 安全の
  // 流儀（Studio.tsx の showAbPanel と同じ）に揃えて typeof チェックを置く。
  const captureMode =
    typeof window !== 'undefined' &&
    new URLSearchParams(window.location.search).get('abcap') === '1';

  let webglCanvas: HTMLCanvasElement | undefined;
  let wgslCanvas: HTMLCanvasElement | undefined;

  const [active, setActive] = createSignal<Side>('webgl');
  const [running, setRunning] = createSignal(false);
  // start() が in-flight（冒頭 stop() で running()=false になってから commit まで）の間 true。
  // この間 Start ボタンを押せると、並走した古い start の中断 cleanup が勝者 start の
  // 生きたリソースを巻き込んで破棄してしまう。Start の disabled に pending() を加え、
  // かつ中断パスを own-resource 化することで二重に防ぐ。
  const [pending, setPending] = createSignal(false);
  const [webglInitMs, setWebglInitMs] = createSignal<number | null>(null);
  const [wgslInitMs, setWgslInitMs] = createSignal<number | null>(null);
  const [fps, setFps] = createSignal<number | null>(null);
  const [errorMsg, setErrorMsg] = createSignal('');
  // #242: キャプチャ進行中ガード（ボタン disabled）と完了メッセージ。
  const [capBusy, setCapBusy] = createSignal(false);
  const [capMsg, setCapMsg] = createSignal('');
  // #242: キャプチャモードで 1 度でもキャプチャ成功したか（成功後は canvas に
  // 両側の t=0 フレームが残っているので、segmented toggle で目視比較できる）。
  const [captured, setCaptured] = createSignal(false);

  // 起動済みリソース。stop / re-start で作り直す（props 変更時の自動再初期化、
  // および手動 Stop ボタンで dispose → 再 setup する）。
  let glRenderer: GlRenderer | null = null;
  let wgslReady = false;
  let rafId: number | undefined;
  // #242: キャプチャモード専用の WebGL renderer。blink 用 glRenderer とは独立に
  // 持つ（キャプチャは start/stop の世代管理に乗せない。再キャプチャ時に
  // dispose → 作り直し、onCleanup でも畳む）。
  let capGl: GlRenderer | null = null;

  // start 再入ガード（世代カウンタ）。start は await を含む async なので、
  // 連続トリガ（props が立て続けに変わる等）で複数の start が並走し得る。
  // start 冒頭で generation を ++ して myGen に snapshot し、各 await 後に
  // myGen !== generation なら「より新しい start が走った」とみなして中断する。
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

  // 停止処理（全域）: rAF を止め、グローバルリソースを破棄して計測をリセットする。
  // ユーザー操作（Stop ボタン）・onCleanup・rAF エラー経路に限定して呼ぶ。
  // start() の中断パスからは呼ばない（own-cleanup を使う。下記 start 参照）。
  //
  // generation を進めるのが肝: in-flight な start は各 await 後に
  // myGen !== generation で自分が古いと判断して中断するので、Stop を押すと
  // 走行中の start も確実に殺せる（Stop の意味が「全部止める」になる）。
  function stop() {
    generation++;
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
    setPending(false);
    setFps(null);
    setWebglInitMs(null);
    setWgslInitMs(null);
  }

  onCleanup(() => {
    stop();
    // #242: キャプチャモード専用リソースも畳む。
    if (capGl) {
      capGl.dispose();
      capGl = null;
    }
  });

  // running 中に Studio 側の入力（shape / source / image File / glyph / 各プリセット）が
  // 変わったら、stop → start で作り直して新しい入力を反映する。
  //   - on(..., { defer: true }): マウント時の初回 run は走らせない（手動 Start を待つ）。
  //   - start は内部で stop() を呼んでから作り直すので、ここでは start() だけ呼ぶ。
  //   - start は async + 世代カウンタなので、props が立て続けに変わっても
  //     最新の start のみが有効になる（古い setup が後勝ちで残らない）。
  //   - 早期 return は !running() && !pending()：in-flight（pending）中の props 変更も
  //     拾って再 start する。これにより「in-flight 中の props 変更が黙って捨てられる」
  //     問題は解消される。並走した start のうち generation により最新だけが commit され、
  //     loser の中断 cleanup は own-resource 化されているので勝者を巻き込まない。
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
        // 停止中（手動 Start を待つ状態）は無視。ただし in-flight（pending）中の
        // props 変更は拾う＝最新の入力で最終的に立ち上がる。
        if (!running() && !pending()) return;
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
    // 既存リソースを必ず畳んでから作り直す（自動再初期化の stop→start 経路でも、
    // 手動 Start の二度押し抑止が外れた経路でも、二重 setup を起こさない）。
    // stop() は generation を進めるので、in-flight だった前の start もここで死ぬ。
    stop();
    // stop() の後に自分の世代を myGen に snapshot する（stop の generation++ を含めて確定させる）。
    // これ以降に新しい start / stop が走ると generation が変わるので、各 await の後で
    // 「自分が最新か」を確認できる。
    const myGen = ++generation;
    // pending: この invocation が commit / 中断 / エラーのいずれかで終わるまで true。
    // この間 Start ボタンは disabled になる（再入を防ぐ）。finally で必ず false に戻す。
    setPending(true);
    // 自分の invocation がローカルに立てたリソース（中断時はこれだけ畳む）。
    let ownGl: GlRenderer | null = null;
    // 中断パス（own-cleanup）: 全域 stop() を呼ぶと勝者 start の生きたリソース
    // （glRenderer / rafId / wgslReady）を巻き込んで破棄するので、自分が立てた
    // リソースだけを畳む。中断が起きる前提＝自分より新しい start か stop が走った：
    //   - WebGL: 自分の setupWebgl が作った GlRenderer。グローバル glRenderer が
    //     まだ自分のものなら（勝者が差し替えていなければ）dispose して null に戻す。
    //     勝者が既に差し替え済みなら自分の参照だけを dispose し、グローバルは触らない。
    //   - WGSL: gpu context は wgslCanvas 上の単一インスタンス。中断契機の stop()/
    //     新 start() の冒頭 stop() が既に global wgslReady=false にしている（勝者が
    //     再 setupWgsl したならその true は勝者の所有物）。よって loser は global
    //     wgslReady を一切触らない（触ると勝者の生きた true を消す）。
    const cleanupOwn = () => {
      if (ownGl) {
        if (glRenderer === ownGl) glRenderer = null;
        ownGl.dispose();
        ownGl = null;
      }
    };
    try {
      const src = props.decoded();
      if (!src) return;
      setErrorMsg('');
      await ensureWasm();
      if (myGen !== generation) {
        cleanupOwn();
        return; // より新しい start / stop が走った → 中断（own のみ畳む）
      }

      const params = buildBaseParams(src);

      // image shape: 元 File を ImageBitmap → RGBA（WGSL image_mask）+ SDF（WebGL）。
      let imageBitmap: ImageBitmap | null = null;
      if (props.shape() === 'image') {
        const file = props.imageShapeFile();
        if (!file) throw new Error('image shape requires a file');
        const dec = await decodeImageFile(file);
        if (myGen !== generation) {
          cleanupOwn();
          return; // より新しい start / stop が走った → 中断（own のみ畳む）
        }
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
      // setupWebgl は glRenderer に書くので、自分の own 参照にも控える。
      const webglMs = setupWebgl(params, imageBitmap);
      ownGl = glRenderer;
      setWebglInitMs(Number(webglMs.toFixed(1)));
      if (webgpuOk) {
        const ms = await setupWgsl(params);
        if (myGen !== generation) {
          // await 中に新しい start / stop が走った。自分が立てた own リソースだけ畳む
          // （全域 stop() を呼ぶと勝者の生きた glRenderer/rafId/wgslReady を巻き込む）。
          // global wgslReady は中断契機の stop() / 新 start 冒頭の stop() が既に管理
          // 済みなので loser からは触らない（cleanupOwn 参照）。
          cleanupOwn();
          return;
        }
        setWgslInitMs(Number(ms.toFixed(1)));
      }

      setRunning(true);
      startLoop();
    } catch (e) {
      // loser（より新しい start / stop に追い越された世代）の失敗は自分の残骸だけ畳む。
      // 全域 stop() を呼ぶと並走中の勝者の生きたセッションを巻き込んで止めてしまう。
      if (myGen !== generation) {
        cleanupOwn();
        return;
      }
      console.error('[ab-panel]', e);
      setErrorMsg(e instanceof Error ? e.message : String(e));
      // エラー経路は全域 stop()。上の世代ガードを抜けた時点で自分が最新世代＝
      // 並走中の勝者は存在せず、自分が最後の起動者なので全域を畳んでよい。
      stop();
    } finally {
      // commit / 中断 / エラーすべての経路で pending を必ず解除する。
      // ただし自分が古い（中断された）場合、勝者が pending を立てている可能性があるので、
      // 自分が最新世代のときだけ落とす（勝者の pending を消さない）。
      if (myGen === generation) setPending(false);
    }
  };

  // ---- #242 キャプチャ（三者画素比較の足場。Phase 3 でパネルごと削除） ------

  // t=0 を両側 1 フレームずつ描画して画素を取得する。**この関数は await を
  // 挟まない 1 タスクで完結させること**: WebGL は preserveDrawingBuffer=false の
  // ためタスクをまたぐとバッファがクリアされ、WebGPU は present 済みテクスチャが
  // expire して drawImage が空になる。全画素 真っ黒/透明 はキャプチャ失敗として
  // throw する（無言で成功扱いしない。※ ソースが完全な黒画像のときは正しい出力
  // でも引っかかり得るが、dev 足場の誤検知として許容する）。
  function captureBothAtT0(gl: GlRenderer): { webglImg: ImageData; wgslImg: ImageData } {
    if (!webglCanvas || !wgslCanvas) throw new Error('canvases not mounted');
    gl.renderFrame(0);
    const webglImg = readWebGlPixels(webglCanvas);
    gpu_render(0);
    const wgslImg = snapshotCanvasPixels(wgslCanvas);
    if (isAllBlackOrTransparent(webglImg.data)) {
      throw new Error('WebGL capture is all black/transparent (capture failed?)');
    }
    if (isAllBlackOrTransparent(wgslImg.data)) {
      throw new Error('WGSL capture is all black/transparent (capture failed?)');
    }
    return { webglImg, wgslImg };
  }

  // キャプチャ 4 ファイルを自動ダウンロードする。ab-params.json は source_rgb
  // 以外の全 params + n + spec_idx + t（バイナリは ab-source.bin に分離）。
  // Rust 側ハーネス（crates/wasm/src/ab_harness.rs）がこの 2 ファイルから
  // CLI（readback 経路）の同条件 PNG を再現する。
  async function downloadCaptureFiles(
    webglImg: ImageData,
    wgslImg: ImageData,
    params: Record<string, unknown>,
    sourceRgb: Uint8Array,
  ): Promise<void> {
    const [webglBlob, wgslBlob] = await Promise.all([
      imageDataToPngBlob(webglImg),
      imageDataToPngBlob(wgslImg),
    ]);
    const meta = buildAbCaptureMeta(params, AB_N, AB_SPEC_IDX, 0);
    downloadBlob('ab-wgsl.png', wgslBlob);
    downloadBlob('ab-webgl.png', webglBlob);
    downloadBlob(
      'ab-params.json',
      new Blob([JSON.stringify(meta, null, 2)], { type: 'application/json' }),
    );
    // Blob 化はコピーを取る（型を ArrayBuffer に確定させ、元バッファの寿命と
    // 切り離す。96×96×3 = 27KB / 実画像でも長辺 256 縮小済みなので軽い）。
    downloadBlob(
      'ab-source.bin',
      new Blob([new Uint8Array(sourceRgb).buffer], { type: 'application/octet-stream' }),
    );
  }

  // キャプチャモード（?ab=1&abcap=1）: 合成ソース + orb 固定 + t=0 固定で
  // WGSL / WebGL を各 1 フレーム描画して 4 ファイルを落とす。blink の
  // start/stop 機構には乗せない（rAF ループは回さない・独立リソース）。
  // shape は orb 固定（#232 orb ゲートの対象）、その他 params は
  // buildAbBaseParams の定数（270×480 / seed=42 / k=5）+ Studio デフォルトの
  // preset（'' = count: spec 値 / speed: 固定割当 / softness: Mid）+
  // glyph_rotate=true（Studio デフォルト）。n=12 / spec_idx=8。
  async function captureSynthetic(): Promise<void> {
    setErrorMsg('');
    setCapMsg('');
    setCapBusy(true);
    try {
      if (!webgpuOk) throw new Error(t('abWebGpuUnavailable'));
      if (!webglCanvas || !wgslCanvas) throw new Error('canvases not mounted');
      await ensureWasm();

      // 合成ソース（決定的・decode 差排除）。式は abLogic.buildSyntheticSourceRgb
      // = Rust ab_harness::synthetic_source_rgb と同一。
      const rgb = buildSyntheticSourceRgb(AB_CAPTURE_SOURCE_W, AB_CAPTURE_SOURCE_H);
      const src: DecodedImage = {
        rgb,
        width: AB_CAPTURE_SOURCE_W,
        height: AB_CAPTURE_SOURCE_H,
      };
      const params = buildAbBaseParams(src, 'orb', '', true, '', '', '');

      // WebGL 側: blink 用 setupWebgl は props.shape() を読んで SDF を足すため
      // 使わず、orb 固定の専用 renderer をここで立てる（再キャプチャ時は作り直し）。
      if (capGl) {
        capGl.dispose();
        capGl = null;
      }
      const gl = createGlRenderer(webglCanvas);
      capGl = gl;
      gl.setResolution(CANVAS_W, CANVAS_H);
      gl.setRenderData(get_render_data(params, AB_N, AB_SPEC_IDX));

      // WGSL 側: gpu_init は再 init 安全（旧 surface を先に drop する）。
      // blink 機構の wgslReady は触らない（キャプチャは gpu_render を直接叩く。
      // キャプチャモードでは blink の Start/Stop UI 自体を出さない）。
      await gpu_init(wgslCanvas);
      gpu_set_render_data(params, AB_N, AB_SPEC_IDX);

      // 描画 + 画素取得（同一タスク）→ エンコード + DL（非同期で良い）。
      const { webglImg, wgslImg } = captureBothAtT0(gl);
      await downloadCaptureFiles(webglImg, wgslImg, params, rgb);
      setCaptured(true);
      setCapMsg(t('abCapDone'));
    } catch (e) {
      console.error('[ab-panel capture]', e);
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setCapBusy(false);
    }
  }

  // 通常モード（?ab=1）実行中の手動キャプチャ: 実画像 + 現在の Studio 状態で
  // 同じ 4 ファイルを落とす（kako-jun の実機確認用）。rAF ループは止めない:
  // t=0 の 1 フレームが一瞬表示されるが、次の rAF が wall-clock t で上書きする
  // （画素は取得済みなので結果に影響しない）。
  // 注: ab-params.json は buildBaseParams のスカラのみ。image shape の
  // image_mask_width/height 等は含まれない（Rust ハーネスは orb 専用なので不要）。
  async function captureT0(): Promise<void> {
    const src = props.decoded();
    if (!src || !running() || !glRenderer || !wgslReady) return;
    setErrorMsg('');
    setCapMsg('');
    setCapBusy(true);
    try {
      const params = buildBaseParams(src);
      const { webglImg, wgslImg } = captureBothAtT0(glRenderer);
      await downloadCaptureFiles(webglImg, wgslImg, params, src.rgb);
      setCapMsg(t('abCapDone'));
    } catch (e) {
      console.error('[ab-panel capture]', e);
      setErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setCapBusy(false);
    }
  }

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
        <p class="text-xs text-fgMuted leading-relaxed">
          {captureMode ? t('abCapNote') : t('abPanelNote')}
        </p>
      </div>

      {/* renderer 切替 segmented control（WebGL / WGSL）。#242: キャプチャモード
          では rAF が無いので、キャプチャ成功後（両 canvas に t=0 が残った状態）に
          有効化して目視比較に使う。通常モードの条件（!running()）は不変。
          disabled 条件は abLogic.segToggleDisabled に切り出し済み（純移動）。 */}
      <div class="inline-flex w-full rounded-md overflow-hidden border border-glassBorder">
        <button
          type="button"
          aria-pressed={active() === 'webgl'}
          onClick={() => toggleTo('webgl')}
          disabled={segToggleDisabled('webgl', captureMode, captured(), running(), webgpuOk)}
          class={segBtn(
            active() === 'webgl',
            segToggleDisabled('webgl', captureMode, captured(), running(), webgpuOk),
          )}
        >
          {t('abRendererWebGL')}
        </button>
        <button
          type="button"
          aria-pressed={active() === 'wgsl'}
          onClick={() => toggleTo('wgsl')}
          disabled={segToggleDisabled('wgsl', captureMode, captured(), running(), webgpuOk)}
          title={!webgpuOk ? t('abWebGpuUnavailable') : undefined}
          class={
            'border-l border-glassBorder ' +
            segBtn(
              active() === 'wgsl',
              segToggleDisabled('wgsl', captureMode, captured(), running(), webgpuOk),
            )
          }
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

      {/* 計測表示（init ms / FPS）。キャプチャモードでは rAF が無く無意味なので
          非表示（#242）。 */}
      <Show when={!captureMode}>
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
      </Show>

      {/* #242 キャプチャモード: 合成ソース・t=0 のキャプチャ実行ボタンだけを
          出す（blink の Start/Stop は出さない＝rAF ループは回さない）。 */}
      <Show when={captureMode}>
        <div class="flex items-center justify-center gap-3">
          <button
            type="button"
            onClick={() => void captureSynthetic()}
            disabled={capBusy() || !webgpuOk}
            title={!webgpuOk ? t('abWebGpuUnavailable') : undefined}
            class={CTRL_BTN}
          >
            {t('abCapRun')}
          </button>
        </div>
      </Show>

      <Show when={!captureMode}>
        <div class="flex items-center justify-center gap-3">
          <button
            type="button"
            onClick={() => void start()}
            disabled={running() || pending() || !canStart()}
            title={!canStart() ? t('abNeedSource') : undefined}
            class={CTRL_BTN}
          >
            {t('abStart')}
          </button>
          {/* Stop は pending 中も押せてよい。stop() が generation を進めるので、
              in-flight な start も確実に中断される（Stop=「全部止める」）。 */}
          <button
            type="button"
            onClick={() => stop()}
            disabled={!running() && !pending()}
            class={CTRL_BTN}
          >
            {t('abStop')}
          </button>
          {/* #242: 実行中の実画像でも同じ 4 ファイルを落とせる手動キャプチャ。
              WGSL 側 PNG が必須なので WebGPU 非対応ブラウザでは disabled。 */}
          <button
            type="button"
            onClick={() => void captureT0()}
            disabled={!running() || capBusy() || !webgpuOk}
            title={!webgpuOk ? t('abWebGpuUnavailable') : undefined}
            class={CTRL_BTN}
          >
            {t('abCaptureT0')}
          </button>
          <Show when={!canStart()}>
            <span class="text-xs text-fgMuted">{t('abNeedSource')}</span>
          </Show>
        </div>
      </Show>

      <Show when={capMsg()}>
        <p class="text-xs text-fgMuted">{capMsg()}</p>
      </Show>

      <Show when={errorMsg()}>
        <p role="alert" class="text-xs text-fg">
          {t('abError', { msg: errorMsg() })}
        </p>
      </Show>
    </section>
  );
}
