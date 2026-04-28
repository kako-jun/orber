import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { decodeImageToRgb, type DecodedImage } from '../lib/decodeImage';

type WasmModule = typeof import('../wasm/orber_wasm.js');

type Aspect = 'portrait' | 'landscape';
type Phase = 'idle' | 'decoding' | 'generating' | 'done' | 'error';

interface Tile {
  blob: Blob;
  blobUrl: string;
  selected: boolean;
}

const BATCH_N = 10;

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
    for (const t of tiles()) URL.revokeObjectURL(t.blobUrl);
  });

  const clearTiles = () => {
    for (const t of tiles()) URL.revokeObjectURL(t.blobUrl);
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
    // ことで、ドラッグするたびに 10 枚すべての direction / count / orb_size /
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
      const result = wasm.generate_batch(params, BATCH_N);
      pngs = result as unknown as Uint8Array[];
    } catch (e) {
      if (myGen !== runGen) return;
      setErrorMsg(String(e));
      setPhase('error');
      return;
    }

    try {
      for (const png of pngs) {
        if (myGen !== runGen) return;
        const blob = new Blob([png], { type: 'image/png' });
        const blobUrl = URL.createObjectURL(blob);
        setTiles((prev) => [...prev, { blob, blobUrl, selected: false }]);
        setProgress((n) => n + 1);
        await yieldFrame();
      }
      if (myGen !== runGen) return;
      setPhase('done');
    } catch (e) {
      if (myGen !== runGen) return;
      setErrorMsg(String(e));
      setPhase('error');
    }
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

  const downloadTiles = async (chosen: Tile[]) => {
    if (chosen.length === 0) return;
    if (chosen.length === 1) {
      triggerDownload(chosen[0].blob, 'orber.png');
      return;
    }
    // jszip は ZIP 化する瞬間にしか使わないので、初回 DL 時に動的読み込みする。
    // 訪問しただけのユーザーに 30KB 余分な JS を読ませない。
    const { default: JSZip } = await import('jszip');
    const zip = new JSZip();
    chosen.forEach((t, i) => {
      zip.file(`orber_${String(i + 1).padStart(2, '0')}.png`, t.blob);
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
          'block cursor-pointer rounded border-2 border-dashed p-8 text-center transition-colors ' +
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

      <div class="flex items-center gap-2">
        <span class="text-zinc-500 text-sm mr-1">アスペクト:</span>
        <button
          type="button"
          aria-pressed={aspect() === 'portrait'}
          onClick={() => setAspectAndMaybeRerun('portrait')}
          class={
            'px-3 py-1 rounded text-sm border ' +
            (aspect() === 'portrait'
              ? 'border-zinc-200 bg-zinc-800 text-zinc-100'
              : 'border-zinc-700 text-zinc-400 hover:border-zinc-500')
          }
        >
          縦長 540×960
        </button>
        <button
          type="button"
          aria-pressed={aspect() === 'landscape'}
          onClick={() => setAspectAndMaybeRerun('landscape')}
          class={
            'px-3 py-1 rounded text-sm border ' +
            (aspect() === 'landscape'
              ? 'border-zinc-200 bg-zinc-800 text-zinc-100'
              : 'border-zinc-700 text-zinc-400 hover:border-zinc-500')
          }
        >
          横長 960×540
        </button>
      </div>

      <div class="text-xs">
        <span class="text-zinc-500 mr-2">wasm:</span>
        <span
          class={
            wasmStatus() === 'ready'
              ? 'text-green-400'
              : wasmStatus() === 'error'
              ? 'text-red-400'
              : 'text-yellow-400'
          }
        >
          {wasmStatus()}
        </span>
        <Show when={wasmStatus() === 'error'}>
          <pre class="mt-2 text-xs text-red-300 whitespace-pre-wrap">{wasmErr()}</pre>
        </Show>
      </div>

      <Show when={phase() === 'decoding'}>
        <p class="text-sm text-zinc-400">画像をデコード中…</p>
      </Show>
      <Show when={phase() === 'generating'}>
        <p class="text-sm text-zinc-400">生成中… {progress()} / {BATCH_N}</p>
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
                  (tile.selected ? 'ring-2 ring-pink-400' : 'ring-1 ring-zinc-800')
                }
                style={{
                  'aspect-ratio': aspect() === 'portrait' ? '540 / 960' : '960 / 540',
                }}
              >
                <img
                  src={tile.blobUrl}
                  alt={`orber variation ${i() + 1}`}
                  class="block h-full w-full object-cover"
                />
                <span
                  class={
                    'absolute top-1 right-1 text-lg leading-none ' +
                    (tile.selected
                      ? 'text-pink-400'
                      : 'text-zinc-500 opacity-0 group-hover:opacity-100')
                  }
                  aria-hidden="true"
                >
                  ♥
                </span>
              </button>
            )}
          </For>
        </div>

        <div class="flex flex-wrap items-center gap-2 pt-2">
          <button
            type="button"
            onClick={downloadSelected}
            disabled={selectedCount() === 0}
            class="px-3 py-1.5 rounded text-sm border border-pink-500 text-pink-300 hover:bg-pink-950/40 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            選択を DL ({selectedCount()})
          </button>
          <button
            type="button"
            onClick={downloadAll}
            disabled={phase() === 'generating' || tiles().length === 0}
            class="px-3 py-1.5 rounded text-sm border border-zinc-600 text-zinc-200 hover:border-zinc-400 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            全 {tiles().length} 枚 DL
          </button>
        </div>
      </Show>
    </section>
  );
}
