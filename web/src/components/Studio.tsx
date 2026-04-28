import { createSignal, onMount } from 'solid-js';

export default function Studio() {
  const [status, setStatus] = createSignal<'loading' | 'ready' | 'error'>('loading');
  const [errorMsg, setErrorMsg] = createSignal<string>('');

  onMount(async () => {
    try {
      // wasm-pack output is at ../wasm/orber_wasm.js
      // (relative to this component file: web/src/components/)
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
      <div>
        <label class="block text-sm text-zinc-400 mb-1">image (placeholder, not wired yet)</label>
        <input type="file" accept="image/*" disabled class="text-sm" />
      </div>
    </section>
  );
}
