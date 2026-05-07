// orber#148 — PWA Install prompt (Solid island)
//
// `beforeinstallprompt` を捕まえて、画面下部に「インストール / 閉じる」のミニ
// トーストを出す。machigai-salad/components/PwaInstallPrompt.tsx と同じ UX
// パターン (Solid 版)。
//
//   - browser が install 可能と判定したときだけ visible になる
//   - sessionStorage で 1 セッション中の dismiss を覚える (毎ロードで連発しない)
//   - `appinstalled` を捕まえてトーストを閉じる
//   - 文字列は strings.ts の `installPromptBody` / `installBtn` / `installDismiss`
//
// 配色は orber 本体の token (bg / fg / fgMuted / glassBg / glassBorder /
// hairline / focusRing) に揃え、Footer の glass button と同じ形にする。

import { onCleanup, onMount, createSignal } from 'solid-js';
import { t } from '../lib/strings';

interface BeforeInstallPromptEvent extends Event {
  prompt: () => Promise<void>;
  userChoice: Promise<{ outcome: 'accepted' | 'dismissed' }>;
}

const DISMISS_KEY = 'orber-pwa-dismissed';

export default function PwaInstallPrompt() {
  const [deferred, setDeferred] = createSignal<BeforeInstallPromptEvent | null>(null);
  const [visible, setVisible] = createSignal(false);

  function hide() {
    setVisible(false);
    setDeferred(null);
    try {
      sessionStorage.setItem(DISMISS_KEY, '1');
    } catch {
      /* noop — strict modes (private mode etc) may throw, ignore */
    }
  }

  onMount(() => {
    try {
      if (sessionStorage.getItem(DISMISS_KEY)) return;
    } catch {
      /* noop */
    }

    const onBeforeInstall = (e: Event) => {
      e.preventDefault();
      setDeferred(e as BeforeInstallPromptEvent);
      setVisible(true);
    };
    const onInstalled = () => hide();

    window.addEventListener('beforeinstallprompt', onBeforeInstall);
    window.addEventListener('appinstalled', onInstalled);

    onCleanup(() => {
      window.removeEventListener('beforeinstallprompt', onBeforeInstall);
      window.removeEventListener('appinstalled', onInstalled);
    });
  });

  async function handleInstall() {
    const ev = deferred();
    if (!ev) return;
    await ev.prompt();
    await ev.userChoice;
    hide();
  }

  return (
    <>
      {visible() && deferred() && (
        <div
          class="fixed bottom-4 left-4 right-4 mx-auto z-50 max-w-md flex items-center gap-3 rounded-md border border-glassBorder bg-glassBg backdrop-blur-glass px-4 py-3 shadow-lg fade-in"
          role="dialog"
          aria-live="polite"
        >
          <span class="flex-1 text-sm text-fg">{t('installPromptBody')}</span>
          <button
            type="button"
            onClick={handleInstall}
            class="whitespace-nowrap rounded-md border border-glassBorder bg-glassBg hover:bg-glassBgHover px-3 py-1.5 text-xs text-fg transition-colors duration-200 ease-out focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
          >
            {t('installBtn')}
          </button>
          <button
            type="button"
            onClick={hide}
            class="text-xs text-fgSubtle hover:text-fg transition-colors duration-200 ease-out focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing focus-visible:ring-offset-2 focus-visible:ring-offset-bg"
            aria-label={t('installDismiss')}
          >
            ×
          </button>
        </div>
      )}
    </>
  );
}
