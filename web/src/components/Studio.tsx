import { createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { decodeImageToRgb, type DecodedImage } from '../lib/decodeImage';
import {
  ANIM_TOTAL_FRAMES,
  encodeAnimationToMp4,
  isWebCodecsSupported,
} from '../lib/encodeMp4';

type WasmModule = typeof import('../wasm/orber_wasm.js');

type Aspect = 'portrait' | 'landscape';
type Phase = 'idle' | 'decoding' | 'generating' | 'animating' | 'done' | 'error';

interface Tile {
  // 静止画フレーム（前半 still と、後半 video の poster 兼フォールバック）。
  blob: Blob;
  blobUrl: string;
  // タイルの種別。後半 5 枚 = video。
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
// 軽い値なのでミラーする。
const VIDEO_TILE_COUNT = 5;

export default function Studio() {
  const [wasmStatus, setWasmStatus] = createSignal<'loading' | 'ready' | 'error'>('loading');
  const [wasmErr, setWasmErr] = createSignal<string>('');
  const [aspect, setAspect] = createSignal<Aspect>('portrait');
  const [decoded, setDecoded] = createSignal<DecodedImage | null>(null);
  const [pickedName, setPickedName] = createSignal<string>('');
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

    // 後半 5 タイルを WebCodecs で mp4 化する。終わったタイルから順次 <video>
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
      setErrorMsg(`動画生成に失敗したタイルがあります: ${String(firstAnimErr)}`);
    }
    setPhase('done');
  };

  const acceptFile = async (file: File) => {
    setErrorMsg('');
    setPickedName(file.name);
    setPhase('decoding');
    try {
      const dec = await decodeImageToRgb(file);
      setDecoded(dec);
      await runBatch();
    } catch (e) {
      console.error('decode failed', e);
      setErrorMsg(String(e));
      setPhase('error');
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

  return (
    <section class="space-y-4">
      <label
        aria-label="画像ファイル選択 / ドラッグ&ドロップ"
        onDrop={onDrop}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        class={
          'block cursor-pointer rounded-3xl border-2 border-dashed p-10 text-center transition-colors ' +
          (dragOver()
            ? 'border-zinc-300 bg-zinc-900'
            : 'border-zinc-700 hover:border-zinc-500')
        }
      >
        <input
          ref={fileInput}
          type="file"
          accept="image/*"
          class="hidden"
          onChange={(e) => {
            const target = e.currentTarget;
            acceptFiles(target.files);
            // 同じファイルを連続で選んだときも change が発火するように value をクリア。
            target.value = '';
          }}
        />
        {pickedName() ? (
          <span class="text-zinc-200">{pickedName()}</span>
        ) : (
          <span class="text-zinc-500">画像を 1 つドロップ / クリックして選択</span>
        )}
      </label>

      <div class="flex items-center justify-center gap-2">
        <button
          type="button"
          aria-pressed={aspect() === 'portrait'}
          aria-label="縦長"
          title="縦長 540×960"
          onClick={() => setAspectAndMaybeRerun('portrait')}
          class={
            'px-3 py-1.5 rounded border inline-flex items-center justify-center ' +
            (aspect() === 'portrait'
              ? 'border-zinc-200 bg-zinc-800 text-zinc-100'
              : 'border-zinc-700 text-zinc-400 hover:border-zinc-500')
          }
        >
          {/* 縦長を示すシルエット (角丸縦長方形) */}
          <svg
            viewBox="0 0 24 24"
            width="20"
            height="20"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <rect x="8" y="3" width="8" height="18" rx="1.5" />
          </svg>
        </button>
        <button
          type="button"
          aria-pressed={aspect() === 'landscape'}
          aria-label="横長"
          title="横長 960×540"
          onClick={() => setAspectAndMaybeRerun('landscape')}
          class={
            'px-3 py-1.5 rounded border inline-flex items-center justify-center ' +
            (aspect() === 'landscape'
              ? 'border-zinc-200 bg-zinc-800 text-zinc-100'
              : 'border-zinc-700 text-zinc-400 hover:border-zinc-500')
          }
        >
          {/* 横長を示すシルエット (角丸横長方形) */}
          <svg
            viewBox="0 0 24 24"
            width="20"
            height="20"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
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
          aria-label="同じ画像でガチャ"
          title="同じ画像でもう一度ガチャ"
          class="px-3 py-1.5 rounded text-sm border border-zinc-700 text-zinc-300 hover:border-zinc-500 disabled:opacity-40 disabled:cursor-not-allowed inline-flex items-center gap-1.5"
        >
          {/* リロード (循環矢印) */}
          <svg
            viewBox="0 0 24 24"
            width="16"
            height="16"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M3 12a9 9 0 0 1 15.5-6.3L21 8" />
            <path d="M21 3v5h-5" />
            <path d="M21 12a9 9 0 0 1-15.5 6.3L3 16" />
            <path d="M3 21v-5h5" />
          </svg>
          ガチャ
        </button>
      </div>

      <Show when={wasmStatus() === 'error'}>
        <div class="rounded border border-red-700 bg-red-950/40 p-3 text-sm text-red-300">
          wasm の読み込みに失敗しました
          <pre class="mt-2 text-xs whitespace-pre-wrap">{wasmErr()}</pre>
        </div>
      </Show>

      <Show when={phase() === 'decoding'}>
        <p class="text-sm text-zinc-400">画像をデコード中…</p>
      </Show>
      <Show when={phase() === 'generating'}>
        <p class="text-sm text-zinc-400">生成中… {progress()} / {batchN()}</p>
      </Show>
      <Show when={phase() === 'animating'}>
        <p class="text-sm text-zinc-400">動画化中…</p>
      </Show>

      <Show when={errorMsg() && phase() === 'error'}>
        <div class="rounded border border-red-700 bg-red-950/40 p-3 text-sm text-red-300">
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
                class={
                  'group relative block w-full overflow-hidden rounded ' +
                  (tile.selected ? 'ring-2 ring-emerald-400' : 'ring-1 ring-zinc-800')
                }
                style={{
                  'aspect-ratio': aspect() === 'portrait' ? '540 / 960' : '960 / 540',
                }}
              >
                <Show
                  when={tile.kind === 'video' && tile.videoBlobUrl}
                  fallback={
                    <img
                      src={tile.blobUrl}
                      alt={`orber variation ${i() + 1}`}
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
                    aria-label={`orber variation ${i() + 1} (animated)`}
                  />
                </Show>
                <span
                  class={
                    'absolute top-1 right-1 text-lg leading-none font-bold ' +
                    (tile.selected
                      ? 'text-emerald-400'
                      : 'text-zinc-500 opacity-0 group-hover:opacity-100')
                  }
                  aria-hidden="true"
                >
                  ✓
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
            class="px-3 py-1.5 rounded text-sm border border-emerald-500 text-emerald-300 hover:bg-emerald-950/40 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            選択を DL ({selectedCount()})
          </button>
          <button
            type="button"
            onClick={downloadAll}
            disabled={phase() === 'generating' || phase() === 'animating' || tiles().length === 0}
            class="px-3 py-1.5 rounded text-sm border border-zinc-600 text-zinc-200 hover:border-zinc-400 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            全 {tiles().length} 枚 DL
          </button>
        </div>
      </Show>
    </section>
  );
}
