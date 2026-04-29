import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
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
  // skeleton 表示中は null（runBatch 冒頭で 12 個先出しするため）。
  blob: Blob | null;
  blobUrl: string;
  // タイルの種別。後半 4 枚 = video（#59 で 5 → 4、4 方向揃い踏み）。
  kind: 'still' | 'video';
  // 動画タイル限定: WebCodecs で生成した mp4。動画化が完了するまで undefined。
  videoBlob?: Blob;
  videoBlobUrl?: string;
  selected: boolean;
}

// 縦長 / 横長どちらも 12 枚で統一する (#61)。12 は 1/2/3/4/6/12 で
// 割り切れるため、スマホからデスクトップまでどの幅でも綺麗にグリッドが
// 揃う最大公約数の大きい数字。前半 8 枚が静止画、後半 4 枚が動画
// (GUI_VIDEO_COUNT_DEFAULT = 4)。
const BATCH_TILE_COUNT = 12;
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
  // #57: ドロップエリア長押し中だけ拡大プレビュー。
  const [previewVisible, setPreviewVisible] = createSignal(false);

  let wasm: WasmModule | null = null;
  let fileInput: HTMLInputElement | undefined;
  // 同時実行中の runBatch を区別するための世代カウンタ。
  // 進行中のループは自分の世代と現世代を比較して食い違ったら抜ける。
  let runGen = 0;
  // #61: 動画タイル <video> の参照を tile index で集める。すべての mp4 化が
  // 完了した時点で一斉に play() を呼び、4 枚の動き始めを揃える。
  let videoRefs: (HTMLVideoElement | undefined)[] = [];
  // #57: 長押し検出。pointerdown から 400ms 経つと拡大プレビューを開く。
  // タイマーが発火した = 長押し成立した時に isLongPress を立て、
  // 続いて発火する click を抑止してファイル選択ダイアログが開かないようにする。
  const LONG_PRESS_MS = 400;
  let longPressTimer: number | undefined;
  let isLongPress = false;

  // タイル枚数は #61 から縦長 / 横長を問わず 12 枚で統一。依存 signal が
  // ないので createMemo は不要 (コスト払うだけ)。プレーンな関数で揃える。
  const batchN = () => BATCH_TILE_COUNT;

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
    // #57 — 走行中の長押しタイマーを止める。コンポーネントが消えた後に
    // setPreviewVisible が呼ばれるのを防ぐ。
    if (longPressTimer !== undefined) clearTimeout(longPressTimer);
  });

  const clearTiles = () => {
    for (const t of tiles()) {
      if (t.blobUrl) URL.revokeObjectURL(t.blobUrl);
      if (t.videoBlobUrl) URL.revokeObjectURL(t.videoBlobUrl);
    }
    setTiles([]);
  };

  // runBatch 冒頭で呼ぶ: 既存タイルの URL を revoke し、新しい 12 個の
  // skeleton で置き換える。clearTiles → setTiles と分けると一瞬グリッドが
  // 空になって視覚的にちらつくので、1 アクションで差し替える。
  const seedSkeletons = () => {
    for (const t of tiles()) {
      if (t.blobUrl) URL.revokeObjectURL(t.blobUrl);
      if (t.videoBlobUrl) URL.revokeObjectURL(t.videoBlobUrl);
    }
    const total = batchN();
    const stillCount = total - VIDEO_TILE_COUNT;
    setTiles(
      Array.from({ length: total }, (_, i) => ({
        blob: null,
        blobUrl: '',
        kind: i < stillCount ? 'still' : 'video',
        selected: false,
      })),
    );
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

    seedSkeletons();
    setErrorMsg('');
    setProgress(0);
    setPhase('generating');
    // #61: 新しい run の開始でビデオ参照テーブルもリセット。
    videoRefs = [];

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
      // skeleton を残すと無限に shimmer して紛らわしい。エラー時は片付ける。
      clearTiles();
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
        // skeleton 12 個は seedSkeletons で先置きしてあるので、index で
        // 差し替える。push にすると skeleton が消えずに重なってしまう。
        setTiles((prev) =>
          prev.map((t, idx) =>
            idx === i ? { ...t, blob, blobUrl, kind } : t,
          ),
        );
        setProgress((n) => n + 1);
        await yieldFrame();
      }
      if (myGen !== runGen) return;
    } catch (e) {
      if (myGen !== runGen) return;
      // skeleton を残すと無限に shimmer して紛らわしい。エラー時は片付ける。
      clearTiles();
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

    // #61: 4 枚揃ってから一斉に play()。<video autoplay> を切ってあるので
    // ここまでは静止 (PNG 下敷きが見える) で待機し、全 mp4 化が終わった瞬間
    // に 4 枚同時に動き始める。yieldFrame で setTiles → DOM mount → ref 確定
    // のサイクルを 1 フレーム回してから play() を呼ぶ。
    // Promise.all で全 play() が解決するまで待つことで、4 枚の readyState
    // 解消タイミングを揃える (個々の play() は内部的に readyState 待ちを
    // 含むため、await を挟まないとタイル間のずれが見える可能性)。
    await yieldFrame();
    if (myGen !== runGen) return;
    await Promise.all(
      videoRefs.map((v) =>
        // play() は user gesture 要件等で reject しうる。muted な <video> なら
        // 通るはずだが、保険で握りつぶす (無音動画が視覚的に静止しても許容)。
        v ? v.play().catch(() => {}) : Promise.resolve(),
      ),
    );
    if (myGen !== runGen) return;

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

  // #57 — 長押しで拡大プレビュー。
  // pointerdown 時に LONG_PRESS_MS のタイマーを仕掛け、満了したらオーバーレイを
  // 開きつつ isLongPress を立てる。pointerup / cancel で常にタイマーをクリア
  // しオーバーレイを閉じる。pointerleave は使わない (S1: 押下中に指がラベル外
  // に少しずれただけで閉じる UX を避けるため)。代わりに pointerdown で
  // setPointerCapture を取り、指がラベル外に移動しても pointerup が必ず
  // ラベルに届くようにする。
  // click は label のネイティブ動作でファイル選択を起動するので、isLongPress
  // が立っていたら preventDefault で抑止する。サムネイルが無い (空ドロップ
  // エリア) ときは何もしない。
  const endLongPress = () => {
    if (longPressTimer !== undefined) {
      clearTimeout(longPressTimer);
      longPressTimer = undefined;
    }
    setPreviewVisible(false);
  };
  const onDropZonePointerDown = (e: PointerEvent) => {
    if (!pickedThumbUrl()) return;
    isLongPress = false;
    // ジェスチャ全体を label に閉じ込める。指が外にスライドしても
    // pointerup / pointercancel が必ず label に届く。
    const target = e.currentTarget as HTMLElement | null;
    target?.setPointerCapture?.(e.pointerId);
    longPressTimer = window.setTimeout(() => {
      isLongPress = true;
      setPreviewVisible(true);
      longPressTimer = undefined;
    }, LONG_PRESS_MS);
  };
  const onDropZonePointerEnd = () => {
    endLongPress();
  };
  const onDropZoneClick = (e: MouseEvent) => {
    if (isLongPress) {
      e.preventDefault();
      e.stopPropagation();
      // click は pointerup の後に来る一発限り。次の操作のために即リセット。
      isLongPress = false;
    }
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
  // 静止タイルは常に PNG。skeleton 中（blob === null）は呼び出し側で除外
  // 済みの想定: downloadAll は generating/animating 中は disabled、
  // downloadSelected は !blob のタイルがそもそも select できない。
  const tilePayload = (t: Tile): { blob: Blob; ext: 'png' | 'mp4' } | null => {
    if (t.kind === 'video' && t.videoBlob) {
      return { blob: t.videoBlob, ext: 'mp4' };
    }
    if (!t.blob) return null;
    return { blob: t.blob, ext: 'png' };
  };

  const downloadTiles = async (chosen: Tile[]) => {
    const ready = chosen
      .map((t) => tilePayload(t))
      .filter((p): p is { blob: Blob; ext: 'png' | 'mp4' } => p !== null);
    if (ready.length === 0) return;
    if (ready.length === 1) {
      triggerDownload(ready[0].blob, `orber.${ready[0].ext}`);
      return;
    }
    // jszip は ZIP 化する瞬間にしか使わないので、初回 DL 時に動的読み込みする。
    // 訪問しただけのユーザーに 30KB 余分な JS を読ませない。
    const { default: JSZip } = await import('jszip');
    const zip = new JSZip();
    ready.forEach(({ blob, ext }, i) => {
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
        onPointerDown={onDropZonePointerDown}
        onPointerUp={onDropZonePointerEnd}
        onPointerCancel={onDropZonePointerEnd}
        onClick={onDropZoneClick}
        class={
          'group relative block cursor-pointer touch-manipulation rounded-xl border border-dashed py-10 px-8 text-center transition-colors duration-200 ease-out focus-within:border-focusRing ' +
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
        {/* `Show keyed` で URL が変わると <img> が unmount → 再 mount され、
            CSS animation `.fade-in` が再発火する (#60 セルフレビュー S1 対応)。
            通常の三項演算子だと <img> ノードは同一のまま src が更新されるので
            アニメーションが 1 回目しか走らない。 */}
        <Show
          when={pickedThumbUrl()}
          keyed
          fallback={<span class="text-fgMuted">{t('dropZonePlaceholder')}</span>}
        >
          {(url) => (
            <div class="relative">
              {/* select-none / touch-none / draggable=false で iOS の長押し
                  callout・拡大鏡・テキスト選択・ドラッグを抑止 (#57)。 */}
              <img
                src={url}
                alt={t('pickedThumbAlt', { name: pickedName() })}
                draggable={false}
                class="fade-in mx-auto max-h-40 object-contain select-none touch-none"
                style={{ '-webkit-touch-callout': 'none' }}
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
          )}
        </Show>
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
        <div class="fade-in rounded border border-hairline bg-glassBg p-3 text-sm text-fg">
          {t('wasmLoadFailed')}
          <pre class="mt-2 text-xs whitespace-pre-wrap text-fgMuted">{wasmErr()}</pre>
        </div>
      </Show>

      <Show when={phase() === 'decoding'}>
        <p class="fade-in text-sm text-fgMuted">{t('decoding')}</p>
      </Show>
      <Show when={phase() === 'generating'}>
        {/* progress() の数字が変わっても <p> はマウントされたままなので
            .fade-in は最初の 1 回 (Show が真になった瞬間) しか走らない。
            毎刻みフェードすると目障りなので、これは意図通り。 */}
        <p class="fade-in text-sm text-fgMuted">{t('generating')} {progress()} / {batchN()}</p>
      </Show>
      <Show when={phase() === 'animating'}>
        <p class="fade-in text-sm text-fgMuted">{t('animating')}</p>
      </Show>

      <Show when={errorMsg() && phase() === 'error'}>
        <div class="fade-in rounded border border-hairline bg-glassBg p-3 text-sm text-fg">
          {errorMsg()}
        </div>
      </Show>

      <Show when={tiles().length > 0}>
        {/* 12 枚 = 1/2/3/4/6/12 で割り切れるので、どの列数でも余りが出ない。
            縦長 (tall) と横長 (wide) でセル幅が違うため列数を別系統にしてある。 */}
        <div
          class={
            'grid gap-2 ' +
            (aspect() === 'portrait'
              ? 'grid-cols-2 sm:grid-cols-3 md:grid-cols-4'
              : 'grid-cols-1 sm:grid-cols-2 md:grid-cols-3')
          }
        >
          <For each={tiles()}>
            {(tile, i) => (
              <button
                type="button"
                onClick={() => tile.blob && toggleTile(i())}
                disabled={!tile.blob}
                aria-busy={!tile.blob}
                class="group relative block w-full overflow-hidden rounded focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing disabled:cursor-default"
                style={{
                  'aspect-ratio': aspect() === 'portrait' ? '540 / 960' : '960 / 540',
                }}
              >
                {/* tile.blob が null の間は skeleton shimmer。runBatch 冒頭で
                    12 個先出しすることでグリッド形状を確定させ、wasm の
                    generate_batch（モバイルで数秒ブロッキング）の最中も
                    ユーザーに「動いている」感を与える。blob 確定後はこの
                    Show 内に切り替わり、ネイティブ <img> は .fade-in で
                    入ってくる（β 案: 1 枚ずつ揃っていく）。 */}
                <Show
                  when={tile.blob}
                  fallback={
                    <div
                      class="skeleton block h-full w-full"
                      aria-hidden="true"
                    />
                  }
                >
                  {/* 静止 PNG は常に下敷きとして表示し続ける。動画タイルでは
                      videoBlobUrl が来たら <video> を上に絶対配置して fade-in
                      させる (#60)。下敷きを残すことで差し替えの瞬間に空白が
                      出ない。 */}
                  <img
                    src={tile.blobUrl}
                    alt={t('variationAlt', { n: i() + 1 })}
                    class="fade-in block h-full w-full object-cover"
                  />
                  <Show when={tile.kind === 'video' && tile.videoBlobUrl}>
                    {/* poster は冗長 (下敷き <img> が同等の役割) なので付けない。
                        autoplay は #61 で外し、4 枚揃ってから runBatch 末尾で
                        一斉に play() する (動き始めを揃えるため)。 */}
                    <video
                      ref={(el) => {
                        // unmount 時に null/undefined が来るケースを除外して
                        // 古いスロットを上書きしないようガード (#61 セルフ
                        // レビュー S4)。リセットは runBatch 冒頭で一括行う。
                        if (el) videoRefs[i()] = el;
                      }}
                      src={tile.videoBlobUrl}
                      muted
                      playsinline
                      loop
                      class="fade-in absolute inset-0 block h-full w-full object-cover"
                      aria-label={t('variationAnimatedAlt', { n: i() + 1 })}
                    />
                  </Show>
                </Show>
                {/* 4-corner L marker — DESIGN.md §4 SelectionMarker
                    skeleton 中は disabled なので hover も発火しない。 */}
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

      {/* #57 — 長押し中だけ表示する拡大プレビュー (DESIGN.md §4 PreviewOverlay)。
          pointer-events-none で下のドロップエリアが pointerup を受けられる。
          .fade-in (#60) を流用して 200ms フェードイン。 */}
      <Show when={previewVisible() && pickedThumbUrl()}>
        <div
          class="fade-in pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-bg/80"
          aria-hidden="true"
        >
          <img
            src={pickedThumbUrl()}
            alt={t('pickedThumbAlt', { name: pickedName() })}
            draggable={false}
            class="max-h-[90vh] max-w-[90vw] object-contain select-none touch-none"
          />
        </div>
      </Show>
    </section>
  );
}
