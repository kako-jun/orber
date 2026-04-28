import { createSignal, onMount } from 'solid-js';

const ACCEPTED = 'image/*,video/*';

function isAcceptedFile(file: File): boolean {
  return file.type.startsWith('image/') || file.type.startsWith('video/');
}

export default function Studio() {
  const [status, setStatus] = createSignal<'loading' | 'ready' | 'error'>('loading');
  const [errorMsg, setErrorMsg] = createSignal<string>('');
  const [picked, setPicked] = createSignal<File | null>(null);
  const [dragOver, setDragOver] = createSignal(false);

  onMount(async () => {
    try {
      const mod = await import('../wasm/orber_wasm.js');
      await mod.default();
      // init_panic_hook is auto-called via #[wasm_bindgen(start)].
      console.log('orber-wasm loaded');
      setStatus('ready');
    } catch (e) {
      console.error('failed to load orber-wasm', e);
      setErrorMsg(String(e));
      setStatus('error');
    }
  });

  const acceptFiles = (files: FileList | null) => {
    if (!files || files.length === 0) return;
    // 仕様: 入力は画像か動画を 1 つ。複数渡されても先頭だけ採用する。
    const first = files[0];
    if (!isAcceptedFile(first)) {
      setErrorMsg(`unsupported type: ${first.type || 'unknown'}`);
      return;
    }
    setErrorMsg('');
    setPicked(first);
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
    e.preventDefault();
    setDragOver(false);
  };

  return (
    <section class="space-y-4">
      <div>
        <span class="text-zinc-500 mr-2">wasm:</span>
        <span
          class={
            status() === 'ready'
              ? 'text-green-400'
              : status() === 'error'
              ? 'text-red-400'
              : 'text-yellow-400'
          }
        >
          {status()}
        </span>
        {status() === 'error' && (
          <pre class="mt-2 text-xs text-red-300 whitespace-pre-wrap">{errorMsg()}</pre>
        )}
      </div>

      <label
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
          type="file"
          accept={ACCEPTED}
          class="hidden"
          onChange={(e) => acceptFiles(e.currentTarget.files)}
        />
        {picked() ? (
          <span class="text-zinc-200">{picked()!.name}</span>
        ) : (
          <span class="text-zinc-500">画像 or 動画を 1 つドロップ / クリックして選択</span>
        )}
      </label>

      {errorMsg() && status() !== 'error' && (
        <p class="text-xs text-red-300">{errorMsg()}</p>
      )}
    </section>
  );
}
