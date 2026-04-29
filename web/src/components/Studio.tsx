import { createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { decodeImageToRgb, type DecodedImage } from '../lib/decodeImage';
import {
  ANIM_TOTAL_FRAMES,
  encodeAnimationToMp4,
  isWebCodecsSupported,
} from '../lib/encodeMp4';
import { t, lang } from '../lib/strings';

type WasmModule = typeof import('../wasm/orber_wasm.js');

type Aspect = 'portrait' | 'landscape';
type Phase = 'idle' | 'decoding' | 'generating' | 'animating' | 'done' | 'error';

interface Tile {
  // 静止画フレーム（前半 still と、後半 video の poster 兼フォールバック）。
  blob: Blob;
  blobUrl: string;
  // タイルの種別。後半 4 枚 = video（#59 で 5 → 4、4 方向揃い踏み）。
  kind: 'still' | 'video';
  // 動画タイル限定: WebCodecs で生成した mp4。動画化が完了するまで undefined。
  videoBlob?: Blob;
  videoBlobUrl?: string;
  selected: boolean;
}

// 縦長は 5 列 × 2 行 = 10 枚、横長は 3 列 × 3 行 = 9 枚で綺麗に割り切れる。
const BATCH_PORTRAIT = 10;
const BATCH_LANDSCAPE = 9;
// `crates/core/src/variations.rs::GUI_VIDEO_COUNT_DEFAULT` と一致させる。
// wasm バインディング経由で値を引っ張る方法もあるが、コンパイル時定数で済む
// 軽い値なのでミラーする。#59 で 5 → 4 に変更（4 方向 LR/RL/TB/BT を
// 1 枚ずつ重複なく見せる、wasm 側の start_animation_for_batch_spec が固定割当）。
const VIDEO_TILE_COUNT = 4;

export default function Studio() {
  const [wasmStatus, setWasmStatus] = createSignal<'loading' | 'ready' | 'error'>('loading');
  const [wasmErr, setWasmErr] = createSignal<string>('');
  const [aspect, setAspect] = createSignal<Aspect>('portrait');
  const [decoded, setDecoded] = createSignal<DecodedImage | null>(null);
  const [pickedName, setPickedName] = createSignal<string>('');
  // ドロップエリアに表示するサムネイル用の object URL。差し替えで revoke する。
  const [pickedThumbUrl, setPickedThumbUrl] = createSignal<string>('');
  const [phase, setPhase] = createSignal<Phase>('idle');
  const [progress, setProgress] = createSignal<number>(0);
  const [errorMsg, setErrorMsg] = createSignal<string>('');
  const [tiles, setTiles] = createSignal<Tile[]>([]);
  const [dragOver, setDragOver] = createSignal(false);

  let wasm: WasmModule | null = null;
  let fileInput: HTMLInputElement | undefined;
  // 同時実行中の runBatch を区別するための世代カウンタ。
  // 進行中のループは自分の世代と現世代を比較して食い違ったら抜ける。
  let runGen = 0;

  // タイル枚数はアスペクトで決まる（縦長 10 / 横長 9）。runBatch / 進捗表示の
  // 両方から参照するので一箇所に括っておく。
  const batchN = createMemo(() =>
    aspect() === 'portrait' ? BATCH_PORTRAIT : BATCH_LANDSCAPE,
  );

  // lang 同期 (setLang + document.documentElement.lang) は Subtitle.tsx に集約。
  // pre-hydration では Base.astro の inline script が <html lang> を確定済み。
  onMount(async () => {
    try {
      const mod = await import('../wasm/orber_wasm.js');
      await mod.default();
      wasm = mod;
      setWasmStatus('ready');
    } catch (e) {
      console.error('failed to load orber-wasm', e);
      setWasmErr(String(e));
      setWasmStatus('error');
    }
  });

  onCleanup(() => {
    for (const t of tiles()) {
      URL.revokeObjectURL(t.blobUrl);
      if (t.videoBlobUrl) URL.revokeObjectURL(t.videoBlobUrl);
    }
    if (pickedThumbUrl()) URL.revokeObjectURL(pickedThumbUrl());
  });

  const clearTiles = () => {
    for (const t of tiles()) {
      URL.revokeObjectURL(t.blobUrl);
      if (t.videoBlobUrl) URL.revokeObjectURL(t.videoBlobUrl);
    }
    setTiles([]);
  };

  // 1 frame ぶん描画を挟む（setTimeout(0) より意図が明確）。
  const yieldFrame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));

  const runBatch = async () => {
    const src = decoded();
    if (!src) return;
    if (!wasm) {
      setErrorMsg('wasm not ready');
      setPhase('error');
      return;
    }

    runGen += 1;
    const myGen = runGen;

    clearTiles();
    setErrorMsg('');
    setProgress(0);
    setPhase('generating');

    const [w, h] = aspect() === 'portrait' ? [540, 960] : [960, 540];
    // 2**48 までは JS Number で無損失。呼び出しごとに新しい base seed を引く
    // ことで、ドラッグするたびに N 枚すべての direction / count / orb_size /
    // blur / 配置がランダムに変わる（GUI 要件）。
    const baseSeed = Math.floor(Math.random() * 2 ** 48);
    const params = {
      source_rgb: src.rgb,
      source_width: src.width,
      source_height: src.height,
      k: 5,
      width: w,
      height: h,
      seed: baseSeed,
      direction: 'lr',
      speed: 'slow',
      count: 20,
      orb_size: 3.0,
      blur: 0.5,
      shape: 'circle',
    };

    // 重い WASM コール前に 1 フレーム描画させる
    await yieldFrame();
    if (myGen !== runGen) return;

    let pngs: Uint8Array[];
    try {
      const result = wasm.generate_batch(params, batchN());
      pngs = result as unknown as Uint8Array[];
    } catch (e) {
      if (myGen !== runGen) return;
      setErrorMsg(String(e));
      setPhase('error');
      return;
    }

    const total = pngs.length;
    const stillCount = Math.max(0, total - VIDEO_TILE_COUNT);

    try {
      for (let i = 0; i < pngs.length; i++) {
        if (myGen !== runGen) return;
        const png = pngs[i];
        const blob = new Blob([png], { type: 'image/png' });
        const blobUrl = URL.createObjectURL(blob);
        const kind: Tile['kind'] = i < stillCount ? 'still' : 'video';
        setTiles((prev) => [...prev, { blob, blobUrl, kind, selected: false }]);
        setProgress((n) => n + 1);
        await yieldFrame();
      }
      if (myGen !== runGen) return;
    } catch (e) {
      if (myGen !== runGen) return;
      setErrorMsg(String(e));
      setPhase('error');
      return;
    }

    // 後半 4 タイルを WebCodecs で mp4 化する。終わったタイルから順次 <video>
    // に差し替わる。WebCodecs 非対応ブラウザでは static PNG のまま表示される。
    if (!isWebCodecsSupported()) {
      setPhase('done');
      return;
    }

    setPhase('animating');
    let firstAnimErr: unknown = null;
    for (let i = stillCount; i < total; i++) {
      if (myGen !== runGen) return;
      try {
        const handle = wasm.start_animation_for_batch_spec(
          params,
          batchN(),
          i,
          ANIM_TOTAL_FRAMES,
        );
        try {
          const mp4Blob = await encodeAnimationToMp4(handle);
          // 並走 run が始まっていたら自分のフレームは捨てる。先行 run の
          // VideoEncoder / mp4-muxer は close 済み（encodeAnimationToMp4 が
          // 完了した時点で内部で finalize されている）なので、ここで blob
          // を流しても整合性は壊れない。ただ古い tile に書き込むのは無意味
          // なのでそのまま return。
          if (myGen !== runGen) return;
          const videoBlobUrl = URL.createObjectURL(mp4Blob);
          setTiles((prev) =>
            prev.map((t, idx) => {
              if (idx !== i) return t;
              // 既存 videoBlobUrl があれば revoke してから差し替える（再ロール
              // 時の防御。現状フローでは clearTiles が先に走るので発生しないが、
              // 将来の挙動変更で漏れないように）。
              if (t.videoBlobUrl) URL.revokeObjectURL(t.videoBlobUrl);
              return { ...t, videoBlob: mp4Blob, videoBlobUrl };
            }),
          );
        } finally {
          // free() は wasm-bindgen 自動生成。AnimationHandle 内部の
          // wasm 線形メモリを解放する。
          handle.free?.();
        }
      } catch (e) {
        // 1 タイル分の失敗は残りタイルの動画化を止めない。
        // 最初のエラーだけ表示して continue する。
        console.error('mp4 encode failed for tile', i, e);
        if (firstAnimErr === null) firstAnimErr = e;
      }
    }
    if (myGen !== runGen) return;
    if (firstAnimErr !== null) {
      setErrorMsg(`${t('animateError')}: ${String(firstAnimErr)}`);
    }
    setPhase('done');
  };

  const acceptFile = async (file: File) => {
    setErrorMsg('');
    setPickedName(file.name);
    // サムネイル URL を差し替え。前回分は revoke してメモリリークを防ぐ。
    const prevThumbUrl = pickedThumbUrl();
    setPickedThumbUrl(URL.createObjectURL(file));
    if (prevThumbUrl) URL.revokeObjectURL(prevThumbUrl);
    setPhase('decoding');
    try {
      const dec = await decodeImageToRgb(file);
      setDecoded(dec);
      await runBatch();
    } catch (e) {
      console.error('decode failed', e);
      setErrorMsg(String(e));
      setPhase('error');
      // 失敗した画像を「成功扱い」のサムネとしてドロップエリアに残さない。
      const failedThumbUrl = pickedThumbUrl();
      if (failedThumbUrl) URL.revokeObjectURL(failedThumbUrl);
      setPickedThumbUrl('');
      setPickedName('');
    }
  };

  const acceptFiles = (files: FileList | null) => {
    if (!files || files.length === 0) return;
    void acceptFile(files[0]);
  };

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    acceptFiles(e.dataTransfer?.files ?? null);
  };

  const onDragOver = (e: DragEvent) => {
    e.preventDefault();
    setDragOver(true);
  };

  const onDragLeave = (e: DragEvent) => {
    // 子要素間移動で発火する dragleave を握りつぶしてハイライトの点滅を防ぐ。
    const related = e.relatedTarget as Node | null;
    const current = e.currentTarget as Node | null;
    if (related && current && current.contains(related)) return;
    setDragOver(false);
  };

  const setAspectAndMaybeRerun = (a: Aspect) => {
    if (aspect() === a) return;
    setAspect(a);
    if (decoded()) void runBatch();
  };

  const toggleTile = (idx: number) => {
    setTiles((prev) =>
      prev.map((t, i) => (i === idx ? { ...t, selected: !t.selected } : t)),
    );
  };

  const selectedCount = () => tiles().filter((t) => t.selected).length;

  const triggerDownload = (blob: Blob, name: string) => {
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  };

  // 動画タイルなら mp4 が出来ていれば mp4、まだなら静止フォールバック PNG。
  // 静止タイルは常に PNG。
  const tilePayload = (t: Tile): { blob: Blob; ext: 'png' | 'mp4' } => {
    if (t.kind === 'video' && t.videoBlob) {
      return { blob: t.videoBlob, ext: 'mp4' };
    }
    return { blob: t.blob, ext: 'png' };
  };

  const downloadTiles = async (chosen: Tile[]) => {
    if (chosen.length === 0) return;
    if (chosen.length === 1) {
      const { blob, ext } = tilePayload(chosen[0]);
      triggerDownload(blob, `orber.${ext}`);
      return;
    }
    // jszip は ZIP 化する瞬間にしか使わないので、初回 DL 時に動的読み込みする。
    // 訪問しただけのユーザーに 30KB 余分な JS を読ませない。
    const { default: JSZip } = await import('jszip');
    const zip = new JSZip();
    chosen.forEach((t, i) => {
      const { blob, ext } = tilePayload(t);
      zip.file(`orber_${String(i + 1).padStart(2, '0')}.${ext}`, blob);
    });
    const zipBlob = await zip.generateAsync({ type: 'blob' });
    triggerDownload(zipBlob, 'orber.zip');
  };

  const downloadSelected = () => {
    void downloadTiles(tiles().filter((t) => t.selected));
  };

  const downloadAll = () => {
    void downloadTiles(tiles());
  };

  // glass スタイル統一トークン — DESIGN.md §1, §4
  // ボタン / トグル / ガチャ / DL ボタンに共通で使う。padding は DESIGN.md §4 (8px / 14px)。
  const GLASS_BTN =
    'px-3.5 py-2 rounded inline-flex items-center justify-center ' +
    'bg-glassBg backdrop-blur-glass border border-glassBorder text-fg ' +
    'hover:bg-glassBgHover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing ' +
    'transition-colors duration-200 ease-out ' +
    'active:opacity-80 disabled:opacity-40 disabled:cursor-not-allowed';
  // toggled (アスペクト ON 等) で重ねる class — DESIGN.md §4 Toggle.
  const GLASS_BTN_TOGGLED = 'bg-glassBgHover';

  return (
    <section class="space-y-4" data-lang={lang()}>
      <label
        aria-label={
          pickedThumbUrl()
            ? `${t('dropZoneLabel')} — ${t('replaceImageHint')}`
            : t('dropZoneLabel')
        }
        onDrop={onDrop}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        class={
          'group relative block cursor-pointer rounded-xl border border-dashed py-10 px-8 text-center transition-colors duration-200 ease-out focus-within:border-focusRing ' +
          (dragOver()
            ? 'border-fg bg-glassBg'
            : 'border-hairline hover:border-fgMuted')
        }
      >
        {/* sr-only で input を視覚的に隠しつつフォーカス可能に保つ。
            display:none (旧 class="hidden") にすると Tab で focus できず
            focus-within も発火しないため使わない。 */}
        <input
          ref={fileInput}
          type="file"
          accept="image/*"
          class="sr-only"
          onChange={(e) => {
            const target = e.currentTarget;
            acceptFiles(target.files);
            // 同じファイルを連続で選んだときも change が発火するように value をクリア。
            target.value = '';
          }}
        />
        {pickedThumbUrl() ? (
          <div class="relative">
            <img
              src={pickedThumbUrl()}
              alt={t('pickedThumbAlt', { name: pickedName() })}
              class="mx-auto max-h-40 object-contain"
            />
            {/* 差し替え overlay — hover / focus (group) で暗幕 + ラベル fade-in。
                dragOver 時は薄い白オーバーレイで強調 (DESIGN.md §4 Filled state)。
                opacity 値 (bg/40, fg/5) は §4 Filled state に明記済み。 */}
            <div
              class={
                'pointer-events-none absolute inset-0 flex items-center justify-center transition-opacity duration-200 ease-out ' +
                (dragOver()
                  ? 'opacity-100 bg-fg/5'
                  : 'opacity-0 bg-bg/40 group-hover:opacity-100 group-focus-within:opacity-100')
              }
              aria-hidden="true"
            >
              <span class="font-display text-sm tracking-wide text-fg">
                {t('replaceImageHint')}
              </span>
            </div>
          </div>
        ) : (
          <span class="text-fgMuted">{t('dropZonePlaceholder')}</span>
        )}
      </label>

      <div class="flex items-center justify-center gap-2">
        <button
          type="button"
          aria-pressed={aspect() === 'portrait'}
          aria-label={t('aspectPortrait')}
          title={t('aspectPortraitTitle')}
          onClick={() => setAspectAndMaybeRerun('portrait')}
          class={GLASS_BTN + (aspect() === 'portrait' ? ' ' + GLASS_BTN_TOGGLED : '')}
        >
          {/* 縦長を示すシルエット (角丸縦長方形) */}
          <svg
            viewBox="0 0 24 24"
            width="20"
            height="20"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <rect x="8" y="3" width="8" height="18" rx="1.5" />
          </svg>
        </button>
        <button
          type="button"
          aria-pressed={aspect() === 'landscape'}
          aria-label={t('aspectLandscape')}
          title={t('aspectLandscapeTitle')}
          onClick={() => setAspectAndMaybeRerun('landscape')}
          class={GLASS_BTN + (aspect() === 'landscape' ? ' ' + GLASS_BTN_TOGGLED : '')}
        >
          {/* 横長を示すシルエット (角丸横長方形) */}
          <svg
            viewBox="0 0 24 24"
            width="20"
            height="20"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <rect x="3" y="8" width="18" height="8" rx="1.5" />
          </svg>
        </button>
        <button
          type="button"
          onClick={() => void runBatch()}
          disabled={!decoded() || phase() === 'decoding' || phase() === 'generating' || phase() === 'animating'}
          aria-label={t('rerollLabel')}
          title={t('rerollTitle')}
          class={GLASS_BTN}
        >
          {/* リロード (循環矢印) — アイコンのみ。テキストラベルは廃止 */}
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M3 12a9 9 0 0 1 15.5-6.3L21 8" />
            <path d="M21 3v5h-5" />
            <path d="M21 12a9 9 0 0 1-15.5 6.3L3 16" />
            <path d="M3 21v-5h5" />
          </svg>
        </button>
      </div>

      <Show when={wasmStatus() === 'error'}>
        <div class="rounded border border-hairline bg-glassBg p-3 text-sm text-fg">
          {t('wasmLoadFailed')}
          <pre class="mt-2 text-xs whitespace-pre-wrap text-fgMuted">{wasmErr()}</pre>
        </div>
      </Show>

      <Show when={phase() === 'decoding'}>
        <p class="text-sm text-fgMuted">{t('decoding')}</p>
      </Show>
      <Show when={phase() === 'generating'}>
        <p class="text-sm text-fgMuted">{t('generating')} {progress()} / {batchN()}</p>
      </Show>
      <Show when={phase() === 'animating'}>
        <p class="text-sm text-fgMuted">{t('animating')}</p>
      </Show>

      <Show when={errorMsg() && phase() === 'error'}>
        <div class="rounded border border-hairline bg-glassBg p-3 text-sm text-fg">
          {errorMsg()}
        </div>
      </Show>

      <Show when={tiles().length > 0}>
        <div
          class={
            'grid gap-2 ' +
            (aspect() === 'portrait'
              ? 'grid-cols-3 sm:grid-cols-4 md:grid-cols-5'
              : 'grid-cols-1 sm:grid-cols-2 md:grid-cols-3')
          }
        >
          <For each={tiles()}>
            {(tile, i) => (
              <button
                type="button"
                onClick={() => toggleTile(i())}
                class="group relative block w-full overflow-hidden rounded focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing"
                style={{
                  'aspect-ratio': aspect() === 'portrait' ? '540 / 960' : '960 / 540',
                }}
              >
                <Show
                  when={tile.kind === 'video' && tile.videoBlobUrl}
                  fallback={
                    <img
                      src={tile.blobUrl}
                      alt={t('variationAlt', { n: i() + 1 })}
                      class="block h-full w-full object-cover"
                    />
                  }
                >
                  <video
                    src={tile.videoBlobUrl}
                    poster={tile.blobUrl}
                    autoplay
                    muted
                    playsinline
                    loop
                    class="block h-full w-full object-cover"
                    aria-label={t('variationAnimatedAlt', { n: i() + 1 })}
                  />
                </Show>
                {/* 4-corner L marker — DESIGN.md §4 SelectionMarker */}
                <span
                  class={
                    'pointer-events-none absolute inset-0 text-fg transition-opacity duration-200 ease-out ' +
                    (tile.selected ? 'opacity-100' : 'opacity-0 group-hover:opacity-30')
                  }
                  aria-hidden="true"
                >
                  {/* top-left */}
                  <svg
                    class="absolute top-1 left-1"
                    width="14"
                    height="14"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                  >
                    <path d="M2 5 V2 H5" />
                  </svg>
                  {/* top-right */}
                  <svg
                    class="absolute top-1 right-1"
                    width="14"
                    height="14"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                  >
                    <path d="M9 2 H12 V5" />
                  </svg>
                  {/* bottom-left */}
                  <svg
                    class="absolute bottom-1 left-1"
                    width="14"
                    height="14"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                  >
                    <path d="M2 9 V12 H5" />
                  </svg>
                  {/* bottom-right */}
                  <svg
                    class="absolute bottom-1 right-1"
                    width="14"
                    height="14"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                  >
                    <path d="M9 12 H12 V9" />
                  </svg>
                </span>
              </button>
            )}
          </For>
        </div>

        <div class="flex flex-wrap items-center justify-center gap-2 pt-2">
          <button
            type="button"
            onClick={downloadSelected}
            disabled={selectedCount() === 0}
            class={GLASS_BTN + ' text-sm'}
          >
            {t('downloadSelected')} ({selectedCount()})
          </button>
          <button
            type="button"
            onClick={downloadAll}
            disabled={phase() === 'generating' || phase() === 'animating' || tiles().length === 0}
            class={GLASS_BTN + ' text-sm'}
          >
            {t('downloadAll', { n: tiles().length })}
          </button>
        </div>
      </Show>
    </section>
  );
}
