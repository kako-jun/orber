import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { decodeImageToRgb, type DecodedImage } from '../lib/decodeImage';
import { ANIM_TOTAL_FRAMES, isWebCodecsSupported } from '../lib/encodeMp4';
import {
  hasInFlight,
  onWorkerCrash,
  terminateAndRespawn,
  workerAnimateOne,
  workerAnimateOneAlpha,
  workerGenerateOne,
  workerGenerateOneAlpha,
  workerGlyphSupported,
  workerInit,
  workerSetImageShape,
  workerSetSource,
  workerVp9AlphaSupported,
} from '../lib/orberClient';
import { t, lang } from '../lib/strings';

type Aspect = 'portrait' | 'landscape';
type Phase = 'idle' | 'decoding' | 'generating' | 'animating' | 'done' | 'error';
// #131: 4 軸の preset 値。表示は strings.ts 経由、内部値は
// wasm の WasmParams.{count,speed,softness}_preset と 1:1 対応する文字列。
//
// レビュー M1: 「未指定（identity）」を空文字 `''` で表現する。`'mid'` を
// デフォルトに据えると wasm 側で `count_preset='mid' → count=20 固定` /
// `speed_preset='mid' → MotionSpeed::Mid 固定` で全タイル同一値になり、
// Phase A の `random_batch_specs` ばらけ（count 10..=50, video=GUI_VIDEO_SPEEDS）
// が壊れる。`''` を渡せば wasm 側 `parse_*_preset` が `Ok(None)` を返して
// spec.count / spec.speed / GUI_VIDEO_SPEEDS の identity 経路に乗る。
// UI では `'' | 'mid'` のどちらでも「標準」ボタンを押下扱いにする。
// count / softness は `'mid'` でも実質 identity、speed は `''` だけが identity で
// `'mid'` を明示選択すると `Slow` に固定される（#131 仕様）。
type ShapeChoice = 'circle' | 'glyph' | 'image';
type CountPreset = '' | 'low' | 'mid' | 'high';
type SpeedPreset = '' | 'slow' | 'mid' | 'fast';
type SoftnessPreset = '' | 'low' | 'mid' | 'high';

// 9 列 × 2 段の picker 配置を取るため候補リストの順序と数を整える。
// 旧 `♦` (ダイヤ・スートマーク) はユーザー指示で除外、`◆` (黒ダイヤ) も
// 同様の意味重複を避けて落とす。最終 18 文字を想定。
const SYMBOL_PICKER_DEFAULT = [
  '☆', '★', '♥', '○', '●', '■', '□', '▲', '△',
  '✓', '✕', '✿', '❀', '✦', '☀', '☁', '⚡', '→',
];

// #136: glyph 文字ごとに「回転を既定で ON / OFF どちらにするか」のテーブル。
// 雷 ⚡ や太陽 ☀ のように、現実世界で回転しない記号は OFF を既定にすると
// 違和感が減る。テーブルに無い文字は ON 既定（`?? true` で fallback）。
//
// glyph 切替時にこのテーブルを参照して checkbox 状態を上書きするので、
// 「⚡ に切り替えたら回転が止まる」「☆ に切り替えたら回転が戻る」という
// 直感的な挙動になる。ユーザーがその後 checkbox を手動で外しても、その
// session 中は手動選択を尊重し、次の glyph 切替で再度 default が適用される。
const GLYPH_DEFAULT_ROTATE: Record<string, boolean> = {
  '⚡': false,
  '☀': false,
};

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

// 解像度トークン (#73)。プレビューは選別用に軽量、DL は実用解像度。
// すべて 9:16 / 16:9 を厳守。`generate_one_at_index` / `start_animation_for_batch_spec`
// は同じ baseSeed + (total, index) で同じ spec を再現するので、width/height だけ
// 上げれば「同じバリエーションの高解像版」が得られる（決定論性は wasm 側で担保）。
//
// #99: プレビュー解像度を 540x960 (518,400px) → 360x640 (230,400px) に
// 下げる（≒ 44% コスト削減）。DL 時は #73 の hi-res 再描画で 1080x1920
// を出すので最終成果物は変わらない。タイル UI は max 200dvh 程度の
// グリッドで縮小表示されるため 360x640 でも視認上の差は小さい。
const PREVIEW_W_PORTRAIT = 360;
const PREVIEW_H_PORTRAIT = 640;
const PREVIEW_W_LANDSCAPE = 640;
const PREVIEW_H_LANDSCAPE = 360;
const DL_W_PORTRAIT = 1080;
const DL_H_PORTRAIT = 1920;
const DL_W_LANDSCAPE = 1920;
const DL_H_LANDSCAPE = 1080;

export default function Studio() {
  const [wasmStatus, setWasmStatus] = createSignal<'loading' | 'ready' | 'error'>('loading');
  const [wasmErr, setWasmErr] = createSignal<string>('');
  const [aspect, setAspect] = createSignal<Aspect>('portrait');
  // PR #130 review Q1: 「現在表示中のタイルが生成されたときの aspect」を別 signal で
  // 持つ。aspect トグル → ガチャ未実行 → DL の順で操作されると、aspect() は新値
  // (=表示中タイルと食い違う) なのに renderHiResForIndices が aspect() を読んで
  // hi-res 解像度を新 aspect でレンダリングしてしまう。tilesAspect は runBatch
  // 開始時に aspect() のスナップショットを取り、DL 経路はこちらを使うことで
  // プレビューと DL の aspect を必ず一致させる。
  const [tilesAspect, setTilesAspect] = createSignal<Aspect>('portrait');
  // #131: 4 軸は常時展開で、どのボタンも押した瞬間に runBatch を起動する。
  // 初期値は empty identity を維持し、既存 output regression を防ぐ。
  const [shape, setShape] = createSignal<ShapeChoice>('circle');
  // 初期値は空文字。User: 「最初から ☆ が入力されてるせいでプレイスホルダ
  // を見られない」。空にすればプレイスホルダ (例: emoji) が見え、ユーザーが
  // 自由入力欄であることに気付ける。glyph shape を選んでも何も入れなければ
  // line 700 の guard で runBatch が走らないので落ちない。
  const [glyphChar, setGlyphChar] = createSignal<string>('');
  // #136: Glyph 回転 ON/OFF。glyph_char 切替時に GLYPH_DEFAULT_ROTATE で上書き。
  // ユーザーが checkbox を切替えるとその session 中は尊重し、次の glyph 切替で
  // 再度 default が適用される。既定 true（既存挙動互換）。
  const [glyphRotate, setGlyphRotate] = createSignal<boolean>(true);
  // #56: 透過版を ZIP DL に同梱するかの checkbox。既定 OFF を厳守
  // （OFF なら既存挙動と byte-exact identity を保つ）。Safari などで VP9 alpha
  // encode が使えないと vp9AlphaSupported が false になり、checkbox は disabled。
  const [includeAlpha, setIncludeAlpha] = createSignal<boolean>(false);
  const [vp9AlphaSupported, setVp9AlphaSupported] = createSignal<boolean>(true);
  const [supportedGlyphChoices, setSupportedGlyphChoices] =
    createSignal<string[]>(SYMBOL_PICKER_DEFAULT);
  const [isGlyphComposing, setIsGlyphComposing] = createSignal(false);
  // #160: shape='image' のときに使う画像のローカル参照。File を worker に
  // structured-clone で送るため、main 側で File 参照を保持しておくと
  // worker クラッシュ / terminateAndRespawn 後に再 upload できる。
  const [imageShapeName, setImageShapeName] = createSignal<string>('');
  const [imageShapeUrl, setImageShapeUrl] = createSignal<string>('');
  const [imageShapeReady, setImageShapeReady] = createSignal<boolean>(false);
  // #170: シルエット反転トグル。auto-polarity (= 少数派 = 被写体) が外れる
  // 画像 (証明写真など被写体が画面の半分以上) の救済。
  const [imageShapeInvert, setImageShapeInvert] = createSignal<boolean>(false);
  // setImageShape を再送するための File ref。runBatch / crash 経路で参照。
  let lastImageFileRef: File | null = null;
  // M1: 初期値は `''`（identity）。UI 側で「標準」ボタンが `aria-pressed` 状態に
  // 見えるが、内部 signal は empty identity を保つ。spec.count / spec.speed /
  // GUI_VIDEO_SPEEDS / SoftnessPreset::Mid を温存し、Phase A の見た目を正確に再現する。
  const [countPreset, setCountPreset] = createSignal<CountPreset>('');
  const [speedPreset, setSpeedPreset] = createSignal<SpeedPreset>('');
  const [softnessPreset, setSoftnessPreset] = createSignal<SoftnessPreset>('');
  const [decoded, setDecoded] = createSignal<DecodedImage | null>(null);
  const [pickedName, setPickedName] = createSignal<string>('');
  // ドロップエリアに表示するサムネイル用の object URL。差し替えで revoke する。
  const [pickedThumbUrl, setPickedThumbUrl] = createSignal<string>('');
  const [phase, setPhase] = createSignal<Phase>('idle');
  const [progress, setProgress] = createSignal<number>(0);
  const [errorMsg, setErrorMsg] = createSignal<string>('');
  // #94: fatal な errorMsg と分けて、部分失敗（動画化 4 枚のうち一部だけ
  // mp4 化に失敗、他は完走）を弱めの通知で出すための signal。phase が
  // 'done' でも表示できるよう、専用の Show ブロックで描画する。
  const [warningMsg, setWarningMsg] = createSignal<string>('');
  const [tiles, setTiles] = createSignal<Tile[]>([]);
  // #95 + flicker fix: 動画タイルの mp4 化進捗を tiles とは別の signal で
  // 持つ。tiles に animProgress を埋めると 1 フレームごとに setTiles で
  // タイル参照が変わり、Solid の <For> がボタン全体を unmount/remount して
  // <img class="fade-in"> の CSS アニメーションが再発火 → 静止 PNG が
  // 点滅して見える。進捗だけ別 signal にすれば tile 参照は不変のまま、
  // SVG リング部分だけが反応的に再描画される。Map のキーはタイル index。
  const [animProgressMap, setAnimProgressMap] = createSignal<
    Map<number, { frame: number; total: number }>
  >(new Map());
  const [dragOver, setDragOver] = createSignal(false);
  // #57: ドロップエリア長押し中だけ拡大プレビュー。
  const [previewVisible, setPreviewVisible] = createSignal(false);
  // 出力 orb タイルの長押し拡大プレビュー（入力サムネ #57 と同じ UX）。
  // null = 非表示。number = 該当タイル index を全画面プレビュー中。
  const [tilePreviewIdx, setTilePreviewIdx] = createSignal<number | null>(null);
  // プレビュー対象タイルを `createMemo` で集約し、Show 側で IIFE を避ける。
  // tile.blob が無いタイル (skeleton) は対象外。
  const previewTile = createMemo(() => {
    const idx = tilePreviewIdx();
    if (idx === null) return null;
    const tile = tiles()[idx];
    return tile?.blob ? tile : null;
  });
  // #73: DL 時の hi-res 再描画進捗。downloading=true の間 DL ボタンを
  // ロックし、進捗テキスト「高解像度版を準備中… {done} / {total}」を出す。
  const [downloading, setDownloading] = createSignal(false);
  const [dlProgress, setDlProgress] = createSignal<{ done: number; total: number }>({
    done: 0,
    total: 0,
  });

  let fileInput: HTMLInputElement | undefined;
  // #75: 直近 workerSetSource した DecodedImage の参照。同じ画像で reroll
  // するときは setSource を再送しない（worker 側のキャッシュをそのまま使う）。
  let lastSourceRef: DecodedImage | null = null;
  // 同時実行中の runBatch を区別するための世代カウンタ。
  // 進行中のループは自分の世代と現世代を比較して食い違ったら抜ける。
  let runGen = 0;
  // #73: 直近の runBatch の baseSeed と aspect を保持する。DL 時に
  // hi-res で再描画するときに同じ baseSeed を使うことで、プレビューと
  // 同じバリエーション（spec 列）を解像度違いで再現する。
  // null は「まだ runBatch していない / 失敗した」状態。
  let lastBaseSeed: number | null = null;
  // #61: 動画タイル <video> の参照を tile index で集める。
  // 動画タイル毎の mp4 化完了直後に該当 ref を play() する（#88, #92）。
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
      // #75: wasm は worker 内でロード・実行する。ここで起動して初期化を
      // 待つことで、初回ドロップ時の体感を「即生成開始」に保つ。
      await workerInit();
      setWasmStatus('ready');
      // #56: VP9 alpha encode は Safari (現時点で WebCodecs 非対応分岐) では
      // 使えないので、checkbox を disabled に倒すために事前 probe する。失敗
      // しても致命ではない（false に倒すだけ）。worker 内でキャッシュされる。
      try {
        const ok = await workerVp9AlphaSupported();
        setVp9AlphaSupported(ok);
        if (!ok) setIncludeAlpha(false);
      } catch (probeErr) {
        console.warn('vp9 alpha probe failed', probeErr);
        setVp9AlphaSupported(false);
        setIncludeAlpha(false);
      }
    } catch (e) {
      console.error('failed to init orber worker', e);
      setWasmErr(String(e));
      setWasmStatus('error');
    }
    // レビュー M3: Worker がクラッシュ → 自動再生成された場合、wasm 未初期化
    // + cachedSource 未設定の状態に戻る。lastSourceRef をリセットして
    // 次の runBatch で setSource を再送させる。エラー UI も出して操作不能
    // 感を抑える（リロードを促すサインになる）。
    const offCrash = onWorkerCrash(() => {
      lastSourceRef = null;
      // #160: shape='image' の bitmap は worker 側で消えている。ロックして
      // 次の runBatch で再 upload を必須にする。File ref が残っていれば
      // 自動再 upload を裏で試みる (silent: triggerRun=false)。
      if (lastImageFileRef) {
        setImageShapeReady(false);
        void onImageShapePick(lastImageFileRef, false).catch(() => {});
      }
      setErrorMsg(t('wasmLoadFailed'));
      setWasmStatus('error');
    });
    onCleanup(offCrash);
    // #160: shape='image' のサムネイル URL をコンポーネント unmount 時に
    // revoke する (新ファイル選択時の revoke と対称)。
    onCleanup(() => {
      const u = imageShapeUrl();
      if (u) URL.revokeObjectURL(u);
    });
  });

  // #131 / #159: シンボルピッカーに並べる候補は、wasm 同梱フォントで描画できる
  // ものだけに絞り込む (端末非依存で見た目が安定するため)。任意の Unicode は
  // 入力欄の自由入力で受け付ける (#159 の OS フォントスタックラスタライズ経路)。
  // wasm 未起動時は候補をそのまま見せ、起動後に filter する。
  createEffect(() => {
    if (wasmStatus() !== 'ready') return;
    void Promise.all(
      SYMBOL_PICKER_DEFAULT.map(async (ch) => ((await workerGlyphSupported(ch)) ? ch : null)),
    )
      .then((symbols) => {
        const filtered = symbols.filter((ch): ch is string => ch !== null);
        if (filtered.length > 0) setSupportedGlyphChoices(filtered);
      })
      .catch((err) => {
        console.warn('failed to validate glyph picker symbols', err);
      });
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
    setAnimProgressMap(new Map());
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
    // S1: 前回 run の stale な進捗が新タイルに表示されるのを防ぐ。
    // clearTiles 経由でない直接 runBatch 連打パスでも確実にリセット。
    setAnimProgressMap(new Map());
  };

  // 1 frame ぶん描画を挟む（setTimeout(0) より意図が明確）。
  const yieldFrame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));

  // #169: runBatch から伝播してくる worker エラーを i18n 文言にマップする。
  // image-shape-no-contrast は generateImageSdf でシルエット抽出に失敗した
  // ことを示す内部 sentinel。`Error` インスタンスなら .message を見て、それ
  // 以外は String(e) で文字列化する (N2)。
  const formatRunBatchError = (e: unknown): string => {
    const msg = e instanceof Error ? e.message : String(e);
    if (msg.includes('image-shape-no-contrast')) {
      return t('imageShapeNoContrast');
    }
    return msg;
  };

  const runBatch = async () => {
    const src = decoded();
    if (!src) return;

    runGen += 1;
    const myGen = runGen;

    // #108: 前の run が走っている最中なら worker を物理的に殺して立て直す。
    // 論理的中断（myGen ガード）だけでは旧 12 個の wasm 同期呼び出しと
    // WebCodecs encode ループが完走するまで止まらず、CPU が二重に走り
    // 新 run の開始が遅延する。runGen は既に進めた後なので、reject で
    // 投げられる旧 run の例外は catch 内の myGen ガードで安全に吸収される。
    // 連打しなければ通常コストはかからない（hasInFlight=false で no-op）。
    if (hasInFlight()) {
      try {
        await terminateAndRespawn();
      } catch (e) {
        if (myGen !== runGen) return;
        clearTiles();
        setErrorMsg(formatRunBatchError(e));
        setPhase('error');
        return;
      }
      if (myGen !== runGen) return;
      // worker を作り直したので worker 側の cachedSource も消えている。
      // 次の workerSetSource を再送させる（onWorkerCrash 経路と同じ後始末）。
      lastSourceRef = null;
      // #160: shape='image' の bitmap も worker 側で消えているので、File ref
      // が残っていればここで再送する (Studio onWorkerCrash 経路と同じ思想)。
      if (lastImageFileRef && shape() === 'image') {
        try {
          await workerSetImageShape(lastImageFileRef);
        } catch (err) {
          console.warn('failed to re-upload image shape after respawn', err);
          setImageShapeReady(false);
        }
      }
    }

    seedSkeletons();
    setErrorMsg('');
    setWarningMsg('');
    setProgress(0);
    setPhase('generating');
    // Q1: ここで「タイル群が生成されるときの aspect」をスナップショット。
    // 以後 aspect() が変わっても tilesAspect() は変わらないので、DL の
    // hi-res 再描画は表示中タイルと一致した aspect で必ず描かれる。
    setTilesAspect(aspect());
    // #61: 新しい run の開始でビデオ参照テーブルもリセット。
    videoRefs = [];

    // #75: 入力画像が変わっていれば worker にソースをアップロードする。
    // 同じ画像で reroll するだけなら再送しない（worker 側キャッシュ流用）。
    if (lastSourceRef !== src) {
      try {
        await workerSetSource(src.rgb, src.width, src.height);
        if (myGen !== runGen) return;
        lastSourceRef = src;
      } catch (e) {
        if (myGen !== runGen) return;
        clearTiles();
        setErrorMsg(formatRunBatchError(e));
        setPhase('error');
        return;
      }
    }

    const [w, h] =
      aspect() === 'portrait'
        ? [PREVIEW_W_PORTRAIT, PREVIEW_H_PORTRAIT]
        : [PREVIEW_W_LANDSCAPE, PREVIEW_H_LANDSCAPE];
    // 2**48 までは JS Number で無損失。呼び出しごとに新しい base seed を引く
    // ことで、ドラッグするたびに N 枚すべての direction / count / orb_size /
    // blur / 配置がランダムに変わる（GUI 要件）。
    const baseSeed = Math.floor(Math.random() * 2 ** 48);
    // DL 時の hi-res 再描画で同じ spec 列を再現できるよう保存（#73）。
    lastBaseSeed = baseSeed;
    // worker 側で source_* がキャッシュから自動マージされるので、ここでは
    // spec パラメータだけ渡す（毎回 RGB を送らない）。direction/speed/count/
    // orb_size/blur は generate_one_at_index ではどのみち spec で上書きされる。
    // #131: shape / glyph_char / count_preset / speed_preset / softness_preset
    // を常時展開 UI から流す。empty identity は既存挙動と同値。
    const params = {
      k: 5,
      width: w,
      height: h,
      seed: baseSeed,
      direction: 'lr',
      speed: 'slow',
      count: 20,
      orb_size: 3.0,
      blur: 0.5,
      shape: shape(),
      glyph_char: shape() === 'glyph' ? glyphChar() : '',
      count_preset: countPreset(),
      speed_preset: speedPreset(),
      softness_preset: softnessPreset(),
      // #136: glyph_rotate=false で per-orb 回転を抑止。Circle 経路では未使用。
      glyph_rotate: glyphRotate(),
    };

    const total = batchN();
    const stillCount = total - VIDEO_TILE_COUNT;

    // #75: 12 枚を 1 枚ずつ worker に投げる。1 タイル分の wasm 呼び出しは
    // 数百 ms なので、各呼び出し完了ごとに main 側 setTiles → DOM 反映 →
    // 次の postMessage が走る。worker スレッドで動いているのでメインの
    // タップ・スクロールはブロックされない。
    //
    // #99: 一時期 #92 で「動画タイルの静止 PNG が出来た直後に動画化を
    // fire-and-forget で並走させる」設計を試したが、worker 2 本起動 +
    // RGB 2 回 clone + wasm 2 回 init のオーバーヘッドで静止画 1〜12 の
    // 表示が遅くなり、並走によるレースで「9 の進捗が完了しても再生されず
    // 10 の進捗が先に出る」「完成済み静止画に shimmer が残る」等の
    // リグレッションが出たためロールバック。
    // 現在は post-loop 直列構成: 静止画ループを完走 → 動画化ループを直列。
    // 各タイル mp4 完成直後に play() する #88 のロジックは内側で維持する。
    try {
      for (let i = 0; i < total; i++) {
        if (myGen !== runGen) return;
        const png = await workerGenerateOne(params, total, i);
        if (myGen !== runGen) return;
        const blob = new Blob([new Uint8Array(png)], { type: 'image/png' });
        const blobUrl = URL.createObjectURL(blob);
        const kind: Tile['kind'] = i < stillCount ? 'still' : 'video';
        setTiles((prev) =>
          prev.map((t, idx) =>
            idx === i ? { ...t, blob, blobUrl, kind } : t,
          ),
        );
        setProgress((n) => n + 1);
        await yieldFrame();
      }
    } catch (e) {
      if (myGen !== runGen) return;
      // レビュー M4: 静止画ループでこの catch に入った時点で、ガードのため
      // runGen を進めて in-flight の `myGen !== runGen` を発火させる。
      // post-loop 直列構成では並走 inner async は無いが、将来どこかで
      // setTimeout 等が動いていても抑止できるよう保守的に維持する。
      runGen += 1;
      clearTiles();
      setErrorMsg(formatRunBatchError(e));
      setPhase('error');
      return;
    }

    if (myGen !== runGen) return;

    if (!isWebCodecsSupported()) {
      setPhase('done');
      return;
    }

    // #88 + #99: 動画化ループ（直列、できた順に play）。
    // 1 タイル分の mp4 が出来た時点で <video> に反映 + play() し、
    // 次のタイルの動画化に進む。
    setPhase('animating');
    let firstAnimErr: unknown = null;
    for (let i = stillCount; i < total; i++) {
      if (myGen !== runGen) return;
      try {
        const mp4Blob = await workerAnimateOne(
          params,
          total,
          i,
          ANIM_TOTAL_FRAMES,
          // #95: フレーム単位の進捗を該当タイルに反映する。
          // 古い世代の更新は無視する（runGen ガード）。
          (frame, totalFrames) => {
            if (myGen !== runGen) return;
            setAnimProgressMap((prev) => {
              const next = new Map(prev);
              next.set(i, { frame, total: totalFrames });
              return next;
            });
          },
        );
        if (myGen !== runGen) return;
        const videoBlobUrl = URL.createObjectURL(mp4Blob);
        setTiles((prev) =>
          prev.map((t, idx) => {
            if (idx !== i) return t;
            if (t.videoBlobUrl) URL.revokeObjectURL(t.videoBlobUrl);
            return { ...t, videoBlob: mp4Blob, videoBlobUrl };
          }),
        );
        // #95: 完成と同時に進捗リングをクリア。
        setAnimProgressMap((prev) => {
          if (!prev.has(i)) return prev;
          const next = new Map(prev);
          next.delete(i);
          return next;
        });
        // #61 + #88: setTiles → DOM mount → ref 確定 のサイクルを
        // 1 フレーム回してから play() を呼ぶ。myGen check は yieldFrame
        // 後にも入れて、再 run で世代が進んだら play() を抑止する。
        await yieldFrame();
        if (myGen !== runGen) return;
        // レビュー S3: setTiles 直後の 1 frame 待ちでも ref が確定して
        // いないことが稀にある（Solid の reconcile タイミング差）。
        // 1 度だけ追加で yieldFrame して再取得するリトライを入れる。
        let videoEl = videoRefs[i];
        if (!videoEl) {
          await yieldFrame();
          if (myGen !== runGen) return;
          videoEl = videoRefs[i];
        }
        if (!videoEl) {
          console.warn('video ref still missing for tile after retry', i);
        } else {
          videoEl.play().catch((err) => {
            // play() は user gesture 要件等で reject しうる。muted な
            // <video> なら通るはずだが、保険で warn のみ（無音動画が
            // 視覚的に静止しても許容）。
            console.warn('play() rejected for tile', i, err);
          });
        }
      } catch (e) {
        // 1 タイル分の失敗は残りタイルの動画化を止めない。
        // 最初のエラーだけ後段で表示する。
        console.error('mp4 encode failed for tile', i, e);
        if (firstAnimErr === null) firstAnimErr = e;
        // #95 レビュー Q2: 失敗パスでも進捗リングをクリア
        // （残ったままだと「中途半端な進捗」が固まる）。
        if (myGen === runGen) {
          setAnimProgressMap((prev) => {
            if (!prev.has(i)) return prev;
            const next = new Map(prev);
            next.delete(i);
            return next;
          });
        }
      }
    }

    if (myGen !== runGen) return;
    if (firstAnimErr !== null) {
      // #94: 動画化 4 枚のうち一部失敗は fatal ではない（残りタイルは
      // 静止画として完成済み、phase も 'done' に遷移する）。errorMsg
      // ではなく warningMsg に入れて、'done' 状態でも見える弱めの
      // 通知バナーで表示する。エラー詳細（DOMException メッセージ等）
      // は end user に意味が薄いので console に留め、UI には事実
      // ベースの warning 文言だけを出す。
      console.error('animation partial failure detail:', firstAnimErr);
      setWarningMsg(t('animatePartialFailure'));
    }
    setPhase('done');
  };

  const acceptFile = async (file: File) => {
    // #73: hi-res 再描画中に新しい画像を受け付けると baseSeed が
    // 上書きされて生成中の DL ジョブと食い違う。完了まで弾く。
    if (downloading()) return;
    // レビュー Q1: worker 起動失敗時はドロップを受け付けても runBatch が
    // workerSetSource で reject されるだけ。最初の段階で弾いてエラー UI
    // を据え置く。
    if (wasmStatus() === 'error') return;
    // #174 invariant: 画像差し替え時は shape / glyphChar / glyphRotate /
    // imageShapeInvert / countPreset / speedPreset / softnessPreset / aspect
    // のオプション signal を一切リセットしない。ユーザーが新しい画像を
    // ドロップした際、下に並ぶ調整ボタンの選択状態は前画像の操作を継承する。
    // (acceptFile が触るのは pickedName / pickedThumbUrl / phase / decoded /
    // errorMsg のみで、UI 4 軸 + shape は無関係に維持される。)
    //
    // imageShapeInvert は shape='image' で使う「画像シルエット」ファイル側の
    // 極性反転トグルで、ドロップエリアに入れる「ソース画像」とは別の File
    // 経路 (onImageShapePick / onImageShapeInvertChange) に紐づく。
    // ソース画像を差し替えてもシルエットファイルは worker キャッシュに残る
    // ため、invert 状態を継承するのが正しい (ユーザーの意図的選択)。
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
      setErrorMsg(formatRunBatchError(e));
      setPhase('error');
      // 失敗した画像を「成功扱い」のサムネとしてドロップエリアに残さない。
      const failedThumbUrl = pickedThumbUrl();
      if (failedThumbUrl) URL.revokeObjectURL(failedThumbUrl);
      setPickedThumbUrl('');
      setPickedName('');
      // レビュー M4: decoded() を更新せずに失敗すると、ドロップエリアは
      // 「画像未選択」表示なのにガチャボタンが前画像で動いて UI と内部
      // 状態が食い違う。decoded を null に戻して整合させる。
      setDecoded(null);
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

  // 出力 orb タイル長押し: 入力サムネ #57 と同じ UX。
  // 400ms 押し続けたら該当タイルを全画面プレビュー、release で閉じる。
  // 通常クリック（toggleTile による選択切替）は短いクリックでのみ発火。
  // 単一ポインタ前提（複数指で同時に複数タイルを掴むケースは想定外、
  // 後発の pointerdown で前のタイマーは上書きされ自然に最後の操作が勝つ）。
  let tileLongPressTimer: number | undefined;
  let isTileLongPress = false;
  const endTileLongPress = () => {
    if (tileLongPressTimer !== undefined) {
      clearTimeout(tileLongPressTimer);
      tileLongPressTimer = undefined;
    }
    if (tilePreviewIdx() !== null) setTilePreviewIdx(null);
  };
  const onTilePointerDown = (e: PointerEvent, idx: number) => {
    // 生成済みタイルでのみ長押しを受け付ける。skeleton 中は何もしない
    // （button disabled でほぼ届かないが念のため）。
    if (!tiles()[idx]?.blob) return;
    const target = e.currentTarget as HTMLElement | null;
    target?.setPointerCapture?.(e.pointerId);
    isTileLongPress = false;
    tileLongPressTimer = window.setTimeout(() => {
      isTileLongPress = true;
      setTilePreviewIdx(idx);
      tileLongPressTimer = undefined;
    }, LONG_PRESS_MS);
  };
  const onTilePointerEnd = () => {
    endTileLongPress();
  };
  const onTileClick = (e: MouseEvent, idx: number) => {
    if (isTileLongPress) {
      e.preventDefault();
      e.stopPropagation();
      isTileLongPress = false;
      return;
    }
    const tile = tiles()[idx];
    if (tile?.blob) toggleTile(idx);
  };

  const runBatchIfReady = () => {
    if (!decoded() || downloading()) return;
    if (shape() === 'glyph' && glyphChar().trim().length === 0) return;
    if (shape() === 'image' && !imageShapeReady()) return;
    void runBatch();
  };

  // #160: shape='image' 用の画像読込。File を worker に送って worker 側で
  // createImageBitmap させる。main 側は File 参照を保持して crash 後の
  // 再 upload に備える。第 2 引数 `triggerRun` を false にすると runBatch を
  // 走らせず、worker recovery 後の silent 再 upload に使える。
  const onImageShapePick = async (file: File, triggerRun = true) => {
    try {
      lastImageFileRef = file;
      await workerSetImageShape(file, imageShapeInvert());
      const oldUrl = imageShapeUrl();
      if (oldUrl) URL.revokeObjectURL(oldUrl);
      setImageShapeUrl(URL.createObjectURL(file));
      setImageShapeName(file.name);
      setImageShapeReady(true);
      if (triggerRun) runBatchIfReady();
    } catch (err) {
      console.warn('failed to load image shape', err);
      setErrorMsg(t('imageShapeLoadFailed'));
      setImageShapeReady(false);
    }
  };

  // #170: invert トグル切替時に worker 側 SDF を再生成する。File ref が
  // 残っていれば再 upload を試み、失敗時は signal をロールバックして UI
  // と worker 状態の整合を保つ (S2)。スタイルは onImageShapePick の async
  // /try-catch に揃える (N1)。
  const onImageShapeInvertChange = async (next: boolean) => {
    setImageShapeInvert(next);
    if (!lastImageFileRef || shape() !== 'image') return;
    try {
      await workerSetImageShape(lastImageFileRef, next);
      runBatchIfReady();
    } catch (err) {
      console.warn('failed to re-upload image shape on invert toggle', err);
      setImageShapeInvert(!next);
      setErrorMsg(formatRunBatchError(err));
    }
  };

  const onAspectClick = (a: Aspect) => {
    setAspect(a);
    runBatchIfReady();
  };

  const onShapeClick = (next: ShapeChoice) => {
    setShape(next);
    runBatchIfReady();
  };

  const onCountPresetClick = (next: CountPreset) => {
    setCountPreset(next);
    runBatchIfReady();
  };

  const onSpeedPresetClick = (next: SpeedPreset) => {
    setSpeedPreset(next);
    runBatchIfReady();
  };

  const onSoftnessPresetClick = (next: SoftnessPreset) => {
    setSoftnessPreset(next);
    runBatchIfReady();
  };

  const applyGlyphChar = (raw: string) => {
    const first = [...raw][0] ?? '';
    setGlyphChar(first);
    // glyphRotate は glyph 切替時に再設定しない (ユーザー指示: 「グリフを変える
    // たびに回転させるが復活している。独自管理であるべき」)。GLYPH_DEFAULT_ROTATE
    // による自動上書きは廃止し、checkbox は完全にユーザー操作のみで動く。
    if (first.length > 0) {
      runBatchIfReady();
    }
    return first;
  };

  const onGlyphPickerClick = (sym: string) => {
    if (sym === glyphChar()) {
      runBatchIfReady();
      return;
    }
    setGlyphChar(sym);
    runBatchIfReady();
  };

  const onGlyphRotateChange = (next: boolean) => {
    setGlyphRotate(next);
    runBatchIfReady();
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

  // #122: ローカル時刻ベースの YYYYMMDD-HHMMSS。連続 DL で上書き確認が
  // 出ないよう毎回ユニークなファイル名にする。zip 内のエントリ名にも
  // 同じ ts を埋めて、複数 zip を同じフォルダに展開しても衝突しないようにする。
  const downloadTimestamp = (d = new Date()) => {
    const p = (n: number) => String(n).padStart(2, '0');
    return (
      `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}` +
      `-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
    );
  };

  // #73: DL 時の hi-res 再描画。プレビュー（#99 で 360×640）とは別に、同じ
  // baseSeed + (total, index) で `generate_one_at_index` / `start_animation_for_batch_spec`
  // を呼び、1080×1920 の PNG / mp4 を作る。プレビューと同じバリエーションが
  // 再現される（決定論性は wasm 側 random_batch_specs が担保）。
  //
  // indices は **ソース配列内の元 index**。並びを保つために Map で持ち回し、
  // 呼び出し側でソートする。
  const renderHiResForIndices = async (
    indices: number[],
  ): Promise<Map<number, { blob: Blob; ext: 'png' | 'mp4' }>> => {
    const out = new Map<number, { blob: Blob; ext: 'png' | 'mp4' }>();
    if (indices.length === 0) return out;
    if (lastBaseSeed === null || lastSourceRef === null) {
      throw new Error('cannot render hi-res: missing seed / source');
    }

    // Q1: aspect() ではなく tilesAspect() を使う。プレビュー生成時の aspect
    // をスナップショットしてあるので、aspect トグル後に DL してもタイル群と
    // 食い違った解像度で hi-res 再描画されない。
    const a = tilesAspect();
    const [hiW, hiH] =
      a === 'portrait'
        ? [DL_W_PORTRAIT, DL_H_PORTRAIT]
        : [DL_W_LANDSCAPE, DL_H_LANDSCAPE];
    const total = batchN();
    const stillCount = total - VIDEO_TILE_COUNT;
    const useWebCodecs = isWebCodecsSupported();

    // worker 側に source RGB がキャッシュ済みなので、ここでは spec パラメータ
    // だけ渡す。direction/speed/count/orb_size/blur は generate_one_at_index
    // ではどのみち spec で上書きされる。
    // #131: hi-res 再描画でも UI の 4 軸を踏襲する
    // （DL がプレビューと別形状になったら困るため）。
    const hiParams = {
      k: 5,
      width: hiW,
      height: hiH,
      seed: lastBaseSeed,
      direction: 'lr',
      speed: 'slow',
      count: 20,
      orb_size: 3.0,
      blur: 0.5,
      shape: shape(),
      glyph_char: shape() === 'glyph' ? glyphChar() : '',
      count_preset: countPreset(),
      speed_preset: speedPreset(),
      softness_preset: softnessPreset(),
      // #136: hi-res 再描画でも UI の glyph_rotate を踏襲。プレビューと DL の
      // 形状不変条件（同じ baseSeed + 同じ params で同じ spec が再現）を保つ。
      glyph_rotate: glyphRotate(),
    };

    // #56: dlProgress.total は呼び出し側 (downloadIndices) で先に立てる。
    // alpha 同梱時は indices.length * 2 にしたいので、ここで上書きしない。
    for (const i of indices) {
      if (i < stillCount || !useWebCodecs) {
        // 静止タイル、または WebCodecs 非対応環境では hi-res の t=0 PNG。
        const png = await workerGenerateOne(hiParams, total, i);
        out.set(i, {
          blob: new Blob([new Uint8Array(png)], { type: 'image/png' }),
          ext: 'png',
        });
      } else {
        // 動画タイルは hi-res で 96 フレーム再描画 → WebCodecs で h264 mp4 化。
        // worker 内で完結するので main 側はメッセージを待つだけ。
        const mp4 = await workerAnimateOne(hiParams, total, i, ANIM_TOTAL_FRAMES);
        out.set(i, { blob: mp4, ext: 'mp4' });
      }
      setDlProgress((p) => ({ ...p, done: p.done + 1 }));
      await yieldFrame();
    }
    return out;
  };

  // #56: 透過 alpha 版を hi-res で再レンダリングする。non-alpha と完全に同じ
  // hiParams / 同じ total で worker に投げ、worker 側で bg.a だけを 0 に上書き
  // して描画する（spec 列・解像度・形状はピクセルレベルで一致）。返り値は:
  //   - 静止タイル (i < stillCount): PNG + WebP の 2 ファイル
  //   - 動画タイル: WebM (VP9 alpha 'keep') 1 ファイル
  // 各 yield ごとに dlProgress.done を進めるので、UI 側で「N/Total」表示が
  // alpha 経路の進捗も含めて連続的に伸びる。
  const renderAlphaForIndices = async (
    indices: number[],
  ): Promise<Map<number, { still?: { png: Blob; webp: Blob }; video?: Blob }>> => {
    const out = new Map<number, { still?: { png: Blob; webp: Blob }; video?: Blob }>();
    if (indices.length === 0) return out;
    if (lastBaseSeed === null || lastSourceRef === null) {
      throw new Error('cannot render alpha: missing seed / source');
    }

    const a = tilesAspect();
    const [hiW, hiH] =
      a === 'portrait'
        ? [DL_W_PORTRAIT, DL_H_PORTRAIT]
        : [DL_W_LANDSCAPE, DL_H_LANDSCAPE];
    const total = batchN();
    const stillCount = total - VIDEO_TILE_COUNT;
    const useWebCodecs = isWebCodecsSupported();

    const hiParams = {
      k: 5,
      width: hiW,
      height: hiH,
      seed: lastBaseSeed,
      direction: 'lr',
      speed: 'slow',
      count: 20,
      orb_size: 3.0,
      blur: 0.5,
      shape: shape(),
      glyph_char: shape() === 'glyph' ? glyphChar() : '',
      count_preset: countPreset(),
      speed_preset: speedPreset(),
      softness_preset: softnessPreset(),
      glyph_rotate: glyphRotate(),
    };

    for (const i of indices) {
      if (i < stillCount) {
        // 静止タイル: 透過 PNG + 透過 WebP の 2 種を出力。
        const [png, webp] = await Promise.all([
          workerGenerateOneAlpha(hiParams, total, i, 'png'),
          workerGenerateOneAlpha(hiParams, total, i, 'webp'),
        ]);
        out.set(i, { still: { png, webp } });
      } else if (useWebCodecs && vp9AlphaSupported()) {
        // 動画タイル: 透過 WebM (VP9 alpha 'keep')。
        const webm = await workerAnimateOneAlpha(
          hiParams,
          total,
          i,
          ANIM_TOTAL_FRAMES,
        );
        out.set(i, { video: webm });
      }
      // VP9 alpha 非対応 + 動画タイル の組合せは silently skip。checkbox を
      // disabled で塞ぐ実装なので通常パスでは到達しない（防御的 fallback）。
      setDlProgress((p) => ({ ...p, done: p.done + 1 }));
      await yieldFrame();
    }
    return out;
  };

  const downloadIndices = async (indices: number[]) => {
    if (indices.length === 0) return;
    setDownloading(true);
    setErrorMsg('');
    try {
      // #56: alpha 同梱時は hi-res 経路を 2 周走らせるので、進捗 total は 2x。
      // renderHiResForIndices 冒頭で setDlProgress({done:0, total:indices.length})
      // していたのを上書きするため、ここで先に正しい total を立てる。
      // User: 「透過版を含めるチェックを入れたのに含まれていなかった」を反映。
      // 旧 `wantAlpha = includeAlpha() && vp9AlphaSupported()` は VP9 alpha
      // 非対応ブラウザ (Safari 等) で alpha 経路が一切走らない状態だった。
      // PNG / WebP 透過は VP9 と無関係なのでチェックされたら必ず走らせ、
      // WebM 透過は renderAlphaForIndices 内 (line 915) で個別に VP9 ガードする。
      const wantAlpha = includeAlpha();
      setDlProgress({
        done: 0,
        total: wantAlpha ? indices.length * 2 : indices.length,
      });
      const rendered = await renderHiResForIndices(indices);
      // index 順を保ってファイル名を 01, 02, ... に振る。
      const sorted = Array.from(rendered.entries()).sort((a, b) => a[0] - b[0]);
      const ts = downloadTimestamp();
      // #56: alpha OFF + 単一選択のときだけ単発 DL（裸ファイルが降る）。
      // alpha ON のときは 1 枚選択でも zip 経路に落として `alpha/` サブフォルダを
      // 同梱する（裸ファイル + フォルダの混在が出来ないため zip にまとめる）。
      if (sorted.length === 1 && !wantAlpha) {
        triggerDownload(sorted[0][1].blob, `orber-${ts}.${sorted[0][1].ext}`);
        return;
      }
      const { default: JSZip } = await import('jszip');
      const zip = new JSZip();
      // sorted は [origIdx, {blob,ext}] の昇順。連番 (01, 02, ...) は配列順で振る。
      // alpha 経路でも同じ連番を使うので、orig→seq の対応 Map を作る。
      const seqByOrig = new Map<number, number>();
      sorted.forEach(([orig, { blob, ext }], n) => {
        const seq = n + 1;
        seqByOrig.set(orig, seq);
        zip.file(`orber-${ts}_${String(seq).padStart(2, '0')}.${ext}`, blob);
      });
      if (wantAlpha) {
        // dlProgress を引き継ぐ（renderHiResForIndices で done = indices.length に
        // 達している）。renderAlphaForIndices は yield 毎に done++。
        const alpha = await renderAlphaForIndices(indices);
        const alphaFolder = zip.folder('alpha');
        if (alphaFolder) {
          for (const [orig, parts] of alpha) {
            const seq = seqByOrig.get(orig);
            if (seq === undefined) continue;
            const padded = String(seq).padStart(2, '0');
            if (parts.still) {
              alphaFolder.file(
                `orber-${ts}_${padded}-alpha.png`,
                parts.still.png,
              );
              alphaFolder.file(
                `orber-${ts}_${padded}-alpha.webp`,
                parts.still.webp,
              );
            }
            if (parts.video) {
              alphaFolder.file(
                `orber-${ts}_${padded}-alpha.webm`,
                parts.video,
              );
            }
          }
        }
      }
      const zipBlob = await zip.generateAsync({ type: 'blob' });
      triggerDownload(zipBlob, `orber-${ts}.zip`);
    } catch (e) {
      console.error('hi-res download failed', e);
      setErrorMsg(`${t('downloadFailed')}: ${String(e)}`);
    } finally {
      setDownloading(false);
      setDlProgress({ done: 0, total: 0 });
    }
  };

  const downloadSelected = () => {
    const indices = tiles()
      .map((t, i) => ({ t, i }))
      .filter(({ t }) => t.selected && t.blob)
      .map(({ i }) => i);
    void downloadIndices(indices);
  };

  const downloadAll = () => {
    const indices = tiles()
      .map((t, i) => ({ t, i }))
      .filter(({ t }) => t.blob)
      .map(({ i }) => i);
    void downloadIndices(indices);
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
  // #133: Segmented control の各セルが使う class を index と total から組み立てる。
  // - 連結ピル化: 中間 cell は角丸を消し、左右端のみ rounded-l / rounded-r-md を付ける
  // - 等幅: flex-1 で row 全体を占有し、2 択 row でも 3 択 row でも左右端が揃う
  // - 区切り: 2 個目以降は left hairline (border-l border-glassBorder) で隣接 cell と分ける
  // - active: bg-fg/15 (白 15%) + text-fg、非選択 (text-fgMuted) と差を明確化
  //   (現状の glass-bg-toggled = 10% より白寄りで、選択中が一目で判る)
  // SEG_GROUP は <div> wrapper 側に付けるトークン。row の最大幅は max-w-md で
  // aspect / shape / count / speed / softness すべて揃え、左右端を一致させる。
  const SEG_GROUP =
    'inline-flex w-full max-w-md mx-auto rounded-md overflow-hidden border border-glassBorder';
  const SEG_BTN = (i: number, total: number, active: boolean) => {
    const radius =
      total === 1
        ? 'rounded-md'
        : i === 0
          ? 'rounded-l-md'
          : i === total - 1
            ? 'rounded-r-md'
            : 'rounded-none';
    const sep = i > 0 ? 'border-l border-glassBorder' : '';
    const state = active
      ? 'bg-fg/15 text-fg'
      : 'bg-glassBg text-fgMuted hover:text-fg hover:bg-glassBgHover';
    return [
      'flex-1 h-9 px-2 text-sm flex items-center justify-center transition-colors duration-200 ease-out',
      'focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing',
      'disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:bg-glassBg disabled:hover:text-fgMuted',
      radius,
      sep,
      state,
    ]
      .filter(Boolean)
      .join(' ');
  };
  // #133: 旧 GLASS_INPUT (h-9 w-20 ...) は glyph 入力欄の専用品だったが、
  // segmented row 化で w-full + glyph-symbol-text を持たせて inline 展開
  // したため不要になった。将来 width 違いの input が増えたら token として
  // 復活させる。
  // #136: 再利用可能な glass checkbox ラベル + input スタイル。
  // 後の #56（透過DL checkbox）が同じトークンを踏襲する想定。
  // - GLASS_CHECKBOX_LABEL: <label> 全体のクリック領域・disabled 連動を担う
  // - GLASS_CHECKBOX_INPUT: <input type="checkbox"> 本体の glass 風見た目
  // ブラウザ既定の青塗りに頼らず、accent-fg + 1.5px hairline + glass-bg で
  // ボタン群と同じ視覚言語を保つ。
  const GLASS_CHECKBOX_LABEL =
    'inline-flex items-center gap-2 cursor-pointer text-sm text-fg ' +
    'has-[:disabled]:opacity-40 has-[:disabled]:cursor-not-allowed';
  // #174: 旧 `bg-glassBg` を削除。iOS Safari / Android Chrome では半透明白
  // 背景の上に accent-fg (白) でチェックを描画すると checked 状態でも
  // 視覚変化が乏しく、ユーザーから「チェックマークが出ていない」と
  // 報告されていた。背景指定を外して OS ネイティブの accent-color 塗り
  // (白塗り + 黒/濃グレーのチェックマーク) に委ねる。
  // unchecked 状態は border-glassBorder (1.5px hairline) で枠を確保するので、
  // デスクトップ Firefox/Chrome の dark theme でも box 自体は背景から見分け
  // られる。bg を指定しないことで透けるのは UA 既定 (薄い灰塗り) で
  // 一貫したチェック描画が得られる。
  const GLASS_CHECKBOX_INPUT =
    'h-4 w-4 rounded-sm border border-glassBorder ' +
    'accent-fg ' +
    'focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing ' +
    'disabled:cursor-not-allowed';
  const isRunning = () =>
    phase() === 'decoding' || phase() === 'generating' || phase() === 'animating';

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
          // touch-pan-y: 縦スクロールはブラウザに任せる（指を縦に動かすと
          // pointercancel が来て endLongPress が走る → スクロール開始）。
          // 静止押下のみ 400ms タイマーで長押しオーバーレイ起動。
          // 旧 touch-none は Android で「ドロップエリアからのフリックで
          // ページがスクロールできない」副作用があったため pan-y に変更。
          'group relative block cursor-pointer touch-pan-y rounded-xl py-10 px-8 text-center transition-colors duration-200 ease-out focus-within:text-focusRing ' +
          (dragOver()
            ? 'text-fg bg-glassBg'
            : 'text-fgSubtle hover:text-fgMuted')
        }
      >
        {/* #79: 丸ドット周回ボーダー — orb との視覚統一。
            stroke-dasharray="0 14" + stroke-linecap="round" で完全な円ドット
            になる（dash 0 + 全体 stroke-width で round caps が circle 化）。
            stroke は currentColor を参照するので、親 label の text-{color}
            の状態切替（hairline / hover:fgMuted / focus:focusRing / dragOver:fg）
            がそのまま色変化として伝わる。 */}
        <svg
          aria-hidden="true"
          class="pointer-events-none absolute inset-0 h-full w-full overflow-visible"
        >
          <rect
            x="1.5"
            y="1.5"
            width="calc(100% - 3px)"
            height="calc(100% - 3px)"
            rx="10.5"
            ry="10.5"
            fill="none"
            stroke="currentColor"
            stroke-width="3"
            stroke-dasharray="0 14"
            stroke-linecap="round"
          />
        </svg>
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
              {/* select-none / touch-none / draggable=false / oncontextmenu /
                  pointer-events-none で iOS の長押し callout・拡大鏡・
                  テキスト選択・ドラッグ・Android の画像保存メニューを
                  全て抑止し (#57 / #87)、pointerdown を確実に親 label の
                  onPointerDown に届けて 400ms 長押しタイマーを発火させる。 */}
              <img
                src={url}
                alt={t('pickedThumbAlt', { name: pickedName() })}
                draggable={false}
                onContextMenu={(e) => e.preventDefault()}
                class="fade-in pointer-events-none mx-auto max-h-40 object-contain select-none touch-none"
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

      {/* #133: aspect / shape / count / speed / softness すべてを 1 つの grid に
          載せて、各 row の左右端が完全に揃うようにする。
          - 列定義: 左 = label (auto)、右 = segmented control (1fr)
          - aspect は label のない 2 列 span (左右端が他 row より少し広く張り出す)
          - 各 segmented control は SEG_GROUP の `w-full` で右列いっぱいに広がる
          - SEG_BTN の `flex-1` で 2 択でも 3 択でも cell 幅が揃う */}
      <div class="mx-auto grid max-w-md grid-cols-[auto_minmax(0,1fr)] items-center gap-x-3 gap-y-2">
        {/* aspect: label なしで 2 列を span。other row と同じグリッド内なので
            左右端が grid 幅 (max-w-md) で一致する。 */}
        <div class="col-span-2">
          <div class={SEG_GROUP}>
            <button
              type="button"
              aria-pressed={aspect() === 'portrait'}
              aria-label={t('aspectPortrait')}
              title={t('aspectPortraitTitle')}
              onClick={() => onAspectClick('portrait')}
              disabled={!decoded() || downloading()}
              class={SEG_BTN(0, 2, aspect() === 'portrait')}
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
              onClick={() => onAspectClick('landscape')}
              disabled={!decoded() || downloading()}
              class={SEG_BTN(1, 2, aspect() === 'landscape')}
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
          </div>
        </div>

        <label class="justify-self-end text-sm text-fgMuted">{t('shapeLabel')}:</label>
        <div class={SEG_GROUP}>
          <button
            type="button"
            aria-pressed={shape() === 'circle'}
            onClick={() => onShapeClick('circle')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(0, 3, shape() === 'circle')}
          >
            {t('shapeOptionCircle')}
          </button>
          <button
            type="button"
            aria-pressed={shape() === 'glyph'}
            onClick={() => onShapeClick('glyph')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(1, 3, shape() === 'glyph')}
          >
            {t('shapeOptionGlyph')}
          </button>
          <button
            type="button"
            aria-pressed={shape() === 'image'}
            onClick={() => onShapeClick('image')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(2, 3, shape() === 'image')}
          >
            {t('shapeOptionImage')}
          </button>
        </div>

        {/* #133 / #174: glyph 入力欄。旧 datalist combobox は iOS Safari /
            Android Chrome でドロップダウンが表示されない (PWA モードでは更に
            不安定) という問題があったため、#174 で datalist と list 属性を
            削除した。下に並ぶ 9×2 picker grid が代替 UI として既に存在し、
            機能的に等価。input は素の text 入力として残し、IME 経由で emoji
            なども自由入力できる挙動は維持する。
            glyph-symbol-text class で picker / input 表示を Noto Sans Symbols 2
            に揃え、⚡ などの白ベタ描画を実現する (Base.astro)。 */}
        <Show when={shape() === 'glyph'}>
          <>
            <span />
            <input
              type="text"
              aria-label={t('glyphCharLabel')}
              value={glyphChar()}
              placeholder={t('glyphCharPlaceholder')}
              maxLength={16}
              disabled={!decoded() || downloading()}
              onCompositionStart={() => setIsGlyphComposing(true)}
              onCompositionEnd={(e) => {
                setIsGlyphComposing(false);
                const first = applyGlyphChar(e.currentTarget.value);
                e.currentTarget.value = first;
              }}
              onInput={(e) => {
                if (isGlyphComposing()) return;
                const first = applyGlyphChar(e.currentTarget.value);
                if (e.currentTarget.value !== first) e.currentTarget.value = first;
              }}
              class={
                // glass token を踏襲しつつ、segmented row 内では w-full にして
                // 右列いっぱいに広げる。glyph-symbol-text で Noto Sans Symbols 2
                // を当て、⚡ などをモノクロ描画する (Base.astro)。
                // GLASS_INPUT は固定幅 (w-20) なのでここでは展開して再利用しない。
                'glyph-symbol-text h-9 w-full rounded border border-glassBorder bg-glassBg px-2 text-center text-sm text-fg ' +
                'backdrop-blur-glass placeholder:text-fgSubtle placeholder:tracking-wide focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing ' +
                'disabled:opacity-40 disabled:cursor-not-allowed'
              }
            />
            <span />
            {/* 9 列 × 2 段の固定グリッド。各ボタンは w-full でセル幅を埋め、
                右端が他の segmented control 行とぴったり揃う。 */}
            <div class="grid grid-cols-9 gap-1">
              <For each={supportedGlyphChoices()}>
                {(sym) => (
                  <button
                    type="button"
                    aria-pressed={glyphChar() === sym}
                    onClick={() => onGlyphPickerClick(sym)}
                    disabled={!decoded() || downloading()}
                    class={
                      GLASS_BTN +
                      ' glyph-symbol-text h-9 w-full px-0 text-base leading-none ' +
                      (glyphChar() === sym ? GLASS_BTN_TOGGLED : '')
                    }
                    title={sym}
                  >
                    {/* #133 review Q2: U+FE0E (text variation selector) を付けて
                        Safari/iOS で ⚡ などが OS 絵文字フォントに resolve されるのを防ぎ、
                        Noto Sans Symbols 2 のモノクロ描画を強制する。Chromium は
                        font-variant-emoji: text で対応済み。selector は display 用で、
                        value (sym) には付けないので状態管理は影響を受けない。
                        Noto Sans Symbols 2 はフォント独自の baseline でボタン中央
                        からズレるので、span を flex で h-full 化して再度
                        items-center / justify-center を当てて強制センタリングする。 */}
                    <span class="flex h-full w-full items-center justify-center leading-none">
                      {sym + '︎'}
                    </span>
                  </button>
                )}
              </For>
            </div>
            {/* #136: Glyph 回転 ON/OFF の checkbox。glyph 形状時のみ表示する。
                glyph picker の下に置くことで「文字を選ぶ → 回転を決める」の
                論理的な流れを維持する。Sample default テーブルにより ⚡ や ☀
                を選ぶと自動で OFF になる。 */}
            <span />
            <label class={GLASS_CHECKBOX_LABEL}>
              <input
                type="checkbox"
                class={GLASS_CHECKBOX_INPUT}
                checked={glyphRotate()}
                onChange={(e) => onGlyphRotateChange(e.currentTarget.checked)}
                disabled={!decoded() || downloading()}
              />
              <span>{t('glyphRotateLabel')}</span>
            </label>
          </>
        </Show>

        {/* #160: Image shape 用の画像入力 row。shape='image' のときだけ
            表示する。ファイル選択 input + プレビューサムネイル + 名前。
            画像はメインスレッドで ImageBitmap 化 → worker に transfer する。 */}
        <Show when={shape() === 'image'}>
          <>
            <label class="justify-self-end text-sm text-fgMuted">
              {t('imageShapeLabel')}:
            </label>
            <div class="flex items-center gap-2 min-w-0">
              <label
                class={
                  'inline-flex items-center justify-center cursor-pointer h-9 px-3 text-sm rounded-md border border-glassBorder bg-glassBg text-fgMuted hover:text-fg hover:bg-glassBgHover transition-colors duration-200 ' +
                  'focus-within:outline-none focus-within:ring-1 focus-within:ring-focusRing ' +
                  (!decoded() || downloading() ? 'opacity-40 cursor-not-allowed pointer-events-none' : '')
                }
              >
                <span>{t('imageShapePick')}</span>
                <input
                  type="file"
                  accept="image/*"
                  class="hidden"
                  disabled={!decoded() || downloading()}
                  onChange={(e) => {
                    const file = e.currentTarget.files?.[0];
                    if (file) void onImageShapePick(file);
                    e.currentTarget.value = '';
                  }}
                />
              </label>
              <Show when={imageShapeUrl()}>
                <img
                  src={imageShapeUrl()}
                  alt={imageShapeName() || t('imageShapeLabel')}
                  class="h-9 w-9 rounded border border-glassBorder object-contain bg-bg"
                />
              </Show>
              <Show when={imageShapeName()}>
                <span class="truncate text-xs text-fgMuted min-w-0">
                  {imageShapeName()}
                </span>
              </Show>
            </div>
            {/* #170: シルエット反転トグル。auto-polarity が外れる画像
                (被写体が画面の半分以上を占める証明写真風など) の救済。 */}
            <span />
            <label class={GLASS_CHECKBOX_LABEL}>
              <input
                type="checkbox"
                class={GLASS_CHECKBOX_INPUT}
                checked={imageShapeInvert()}
                onChange={(e) => void onImageShapeInvertChange(e.currentTarget.checked)}
                disabled={!decoded() || downloading() || !imageShapeReady()}
              />
              <span>{t('imageShapeInvert')}</span>
            </label>
          </>
        </Show>

        <label class="justify-self-end text-sm text-fgMuted">{t('countLabel')}:</label>
        <div class={SEG_GROUP}>
          <button
            type="button"
            aria-pressed={countPreset() === 'low'}
            onClick={() => onCountPresetClick('low')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(0, 3, countPreset() === 'low')}
          >
            {t('countOptionLow')}
          </button>
          <button
            type="button"
            aria-pressed={countPreset() === '' || countPreset() === 'mid'}
            onClick={() => onCountPresetClick('mid')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(1, 3, countPreset() === '' || countPreset() === 'mid')}
          >
            {t('countOptionMid')}
          </button>
          <button
            type="button"
            aria-pressed={countPreset() === 'high'}
            onClick={() => onCountPresetClick('high')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(2, 3, countPreset() === 'high')}
          >
            {t('countOptionHigh')}
          </button>
        </div>

        <label class="justify-self-end text-sm text-fgMuted">{t('speedLabel')}:</label>
        <div class={SEG_GROUP}>
          <button
            type="button"
            aria-pressed={speedPreset() === 'slow'}
            onClick={() => onSpeedPresetClick('slow')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(0, 3, speedPreset() === 'slow')}
          >
            {t('speedOptionSlow')}
          </button>
          <button
            type="button"
            aria-pressed={speedPreset() === '' || speedPreset() === 'mid'}
            onClick={() => onSpeedPresetClick('mid')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(1, 3, speedPreset() === '' || speedPreset() === 'mid')}
          >
            {t('speedOptionMid')}
          </button>
          <button
            type="button"
            aria-pressed={speedPreset() === 'fast'}
            onClick={() => onSpeedPresetClick('fast')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(2, 3, speedPreset() === 'fast')}
          >
            {t('speedOptionFast')}
          </button>
        </div>

        <label class="justify-self-end text-sm text-fgMuted">{t('softnessLabel')}:</label>
        <div class={SEG_GROUP}>
          <button
            type="button"
            aria-pressed={softnessPreset() === 'low'}
            onClick={() => onSoftnessPresetClick('low')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(0, 3, softnessPreset() === 'low')}
          >
            {t('softnessOptionLow')}
          </button>
          <button
            type="button"
            aria-pressed={softnessPreset() === '' || softnessPreset() === 'mid'}
            onClick={() => onSoftnessPresetClick('mid')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(1, 3, softnessPreset() === '' || softnessPreset() === 'mid')}
          >
            {t('softnessOptionMid')}
          </button>
          <button
            type="button"
            aria-pressed={softnessPreset() === 'high'}
            onClick={() => onSoftnessPresetClick('high')}
            disabled={!decoded() || downloading()}
            class={SEG_BTN(2, 3, softnessPreset() === 'high')}
          >
            {t('softnessOptionHigh')}
          </button>
        </div>
      </div>

      <Show when={wasmStatus() === 'error'}>
        <div class="fade-in rounded border border-hairline bg-glassBg p-3 text-sm text-fg">
          {t('wasmLoadFailed')}
          <pre class="mt-2 text-xs whitespace-pre-wrap text-fgMuted">{wasmErr()}</pre>
        </div>
      </Show>

      {/* #135: reroll は control rows の直後・進捗行の直前に置く。`!decoded()`
          のときも他 controls と同じく disabled で視覚的に弱まる。 */}
      <div class="flex items-center justify-center pt-1">
        <button
          type="button"
          onClick={runBatchIfReady}
          disabled={
            !decoded() ||
            downloading() ||
            (shape() === 'glyph' && glyphChar().trim() === '') ||
            (shape() === 'image' && !imageShapeReady())
          }
          aria-label={t('rerollLabel')}
          title={
            shape() === 'image' && !imageShapeReady()
              ? t('imageShapePickHint')
              : t('rerollTitle')
          }
          class={GLASS_BTN + ' h-10 w-10 px-0 active:scale-95'}
        >
          <svg
            viewBox="0 0 24 24"
            width="18"
            height="18"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
            classList={{ 'orb-spin': isRunning() }}
            style={{ 'transform-origin': '50% 50%' }}
          >
            <path d="M3 12a9 9 0 0 1 15.5-6.3L21 8" />
            <path d="M21 3v5h-5" />
            <path d="M21 12a9 9 0 0 1-15.5 6.3L3 16" />
            <path d="M3 21v-5h5" />
          </svg>
        </button>
      </div>

      {/* #121: 進捗行は常に同じ高さを確保し、phase 完了後も消さない（消すと
          下のサムネイルグリッドがガクッと上に詰まる）。idle/error では中身を
          空にして高さだけ残し、テキストはセンタリングする。
          #124: 生成完了 (phase === 'done') 時のみ、空白の代わりに「長押しで拡大」
          の操作ヒントを表示し、進捗行を導線として再利用する。error の時は
          下のエラーバナーと意味的に衝突するため hint を出さない（前回タイルが
          残っていても「いま失敗した画像」のヒントだとユーザーが誤認するため）。 */}
      <Show
        when={
          phase() === 'decoding' ||
          phase() === 'generating' ||
          phase() === 'animating' ||
          tiles().length > 0
        }
      >
        <p
          class="fade-in text-center text-sm text-fgMuted h-5 leading-5"
          aria-live="polite"
        >
          <Show when={phase() === 'decoding'}>{t('decoding')}</Show>
          <Show when={phase() === 'generating'}>
            {t('generating')} {progress()} / {batchN()}
          </Show>
          <Show when={phase() === 'animating'}>{t('animating')}</Show>
          <Show when={phase() === 'done' && tiles().length > 0}>
            {t('longPressEnlargeHint')}
          </Show>
        </p>
      </Show>

      <Show when={errorMsg() && phase() === 'error'}>
        <div
          role="alert"
          class="fade-in rounded border border-hairline bg-glassBg p-3 text-sm text-fg"
        >
          {errorMsg()}
        </div>
      </Show>

      {/* #94: 部分失敗の弱め通知。fatal な error ではないので
          phase !== 'error' のとき表示する（'done' に限らず将来の
          他フェーズでも残す）。fatal が同時発生した場合は error バナーが
          優先されて warning は隠れる。 */}
      <Show when={warningMsg() && phase() !== 'error'}>
        <div
          role="status"
          class="fade-in rounded border border-hairline bg-glassBg p-3 text-sm text-fgMuted"
        >
          {warningMsg()}
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
                onClick={(e) => onTileClick(e, i())}
                onPointerDown={(e) => onTilePointerDown(e, i())}
                onPointerUp={onTilePointerEnd}
                onPointerCancel={onTilePointerEnd}
                onContextMenu={(e) => e.preventDefault()}
                disabled={!tile.blob}
                aria-busy={!tile.blob}
                class="group relative block w-full overflow-hidden rounded touch-pan-y focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-focusRing disabled:cursor-default"
                style={{
                  'aspect-ratio': aspect() === 'portrait' ? '9 / 16' : '16 / 9',
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
                        autoplay は #61 で外し、runBatch ループ内で各 mp4 完成
                        直後に明示 .play()（#88 でできた順、#92 で worker B
                        並走）。 */}
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
                  {/* 動画タイル限定: 静止 PNG は出たが mp4 がまだ来てない間、
                      コーナーバッジ + #95 進捗リングを重ねて「これから動く」
                      ことを示す。以前は skeleton-soft の shimmer も重ねていたが、
                      フレーム単位の進捗リングが出来た今は shimmer が点滅に
                      見えるため除去（静止 PNG は既に出ているので点滅させる
                      必然性がない）。
                      レビュー S3: WebCodecs 非対応環境では videoBlobUrl が
                      永遠に来ないので、バッジを出すと「処理中」が固着して
                      しまう。環境チェックで gating する。 */}
                  <Show
                    when={
                      tile.kind === 'video' &&
                      !tile.videoBlobUrl &&
                      isWebCodecsSupported()
                    }
                  >
                    {/* レビュー N10/N11: text サイズは DESIGN.md の type scale
                        最小 (text-xs = 12px) に揃える。aria-label と表示テキスト
                        の二重指定はスクリーンリーダーで二重読みになるので、
                        表示テキストだけ残して aria-label を外す。 */}
                    <span class="fade-in absolute bottom-1 right-1 rounded bg-glassBg backdrop-blur-glass border border-glassBorder px-2 py-0.5 text-xs tracking-wide text-fg">
                      {/* N2: "…" は strings.ts 側に内包済み。Studio.tsx で重ねない。 */}
                      {t('videoPendingBadge')}
                    </span>
                    {/* #95: フレーム単位の mp4 化進捗をリングで表示。
                        accent color なし、currentColor + text-fgMuted で淡く
                        重ねる。orb と被らない右上配置。SVG だけなので
                        glass-bg の塗りつぶしは付けない。 */}
                    <Show when={animProgressMap().get(i())}>
                      {(progress) => {
                        const pct = () =>
                          progress().total > 0
                            ? Math.min(1, Math.max(0, progress().frame / progress().total))
                            : 0;
                        const r = 10;
                        const c = 2 * Math.PI * r;
                        return (
                          <svg
                            viewBox="0 0 24 24"
                            class="fade-in pointer-events-none absolute right-1 top-1 h-6 w-6 text-fgMuted"
                            role="progressbar"
                            aria-valuenow={Math.floor(pct() * 100)}
                            aria-valuemin="0"
                            aria-valuemax="100"
                            aria-label={t('animating')}
                          >
                            <circle
                              cx="12"
                              cy="12"
                              r={r}
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1.5"
                              stroke-opacity="0.2"
                            />
                            <circle
                              cx="12"
                              cy="12"
                              r={r}
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1.5"
                              stroke-dasharray={String(c)}
                              stroke-dashoffset={String(c * (1 - pct()))}
                              stroke-linecap="round"
                              transform="rotate(-90 12 12)"
                            />
                          </svg>
                        );
                      }}
                    </Show>
                  </Show>
                </Show>
                {/* 4-corner L marker — DESIGN.md §4 SelectionMarker
                    skeleton 中は disabled なので hover も発火しない。
                    User: マークが小さすぎ / 影が薄すぎ / アニメ見えない、を反映:
                      - サイズ 14 → 24 (約 1.7x 拡大)
                      - stroke 1.5 → 2.5 (太く)
                      - drop-shadow を 6px + 2px の二重影で濃く (黒 100%)
                      - orb-selected-pulse の opacity 振幅を 100%↔65% → 100%↔30% に強化 */}
                <span
                  class={
                    'pointer-events-none absolute inset-0 text-fg transition-opacity duration-200 ease-out ' +
                    (tile.selected ? 'opacity-100 orb-selected-pulse' : 'opacity-0 group-hover:opacity-30')
                  }
                  style={{
                    filter:
                      'drop-shadow(0 0 6px rgba(0,0,0,1)) drop-shadow(0 0 2px rgba(0,0,0,1))',
                  }}
                  aria-hidden="true"
                >
                  {/* top-left */}
                  <svg
                    class="absolute top-1.5 left-1.5"
                    width="24"
                    height="24"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2.5"
                    stroke-linecap="round"
                  >
                    <path d="M2 5 V2 H5" />
                  </svg>
                  {/* top-right */}
                  <svg
                    class="absolute top-1.5 right-1.5"
                    width="24"
                    height="24"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2.5"
                    stroke-linecap="round"
                  >
                    <path d="M9 2 H12 V5" />
                  </svg>
                  {/* bottom-left */}
                  <svg
                    class="absolute bottom-1.5 left-1.5"
                    width="24"
                    height="24"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2.5"
                    stroke-linecap="round"
                  >
                    <path d="M2 9 V12 H5" />
                  </svg>
                  {/* bottom-right */}
                  <svg
                    class="absolute bottom-1.5 right-1.5"
                    width="24"
                    height="24"
                    viewBox="0 0 14 14"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2.5"
                    stroke-linecap="round"
                  >
                    <path d="M9 12 H12 V9" />
                  </svg>
                </span>
              </button>
            )}
          </For>
        </div>

        {/* #56 / 配置調整: 透過版同梱 checkbox は DL ボタン行の直上に置く。
            VP9 alpha 非対応ブラウザ (Safari 等) でも checkbox はクリック可能に
            し、tooltip で「WebM 透過は出ない (PNG/WebP のみ)」と説明する。
            disabled の条件は !decoded() / downloading() のみ (ユーザー指示で
            灰色不可活状態を解除)。 */}
        <div class="flex justify-center pt-2">
          <label
            class={GLASS_CHECKBOX_LABEL}
            title={!vp9AlphaSupported() ? t('includeAlphaDisabledTitle') : ''}
          >
            <input
              type="checkbox"
              class={GLASS_CHECKBOX_INPUT}
              checked={includeAlpha()}
              onChange={(e) => setIncludeAlpha(e.currentTarget.checked)}
              disabled={!decoded() || downloading()}
            />
            <span>{t('includeAlphaLabel')}</span>
          </label>
        </div>

        <div class="flex flex-wrap items-center justify-center gap-2">
          <button
            type="button"
            onClick={downloadSelected}
            disabled={
              selectedCount() === 0 ||
              downloading() ||
              phase() === 'generating' ||
              phase() === 'animating'
            }
            class={GLASS_BTN + ' text-sm'}
          >
            {t('downloadSelected')} ({selectedCount()})
          </button>
          <button
            type="button"
            onClick={downloadAll}
            disabled={
              phase() === 'generating' ||
              phase() === 'animating' ||
              downloading() ||
              tiles().length === 0
            }
            class={GLASS_BTN + ' text-sm'}
          >
            {t('downloadAll', { n: tiles().length })}
          </button>
        </div>
        {/* #73: hi-res 再描画の進捗。downloading=true の間表示。 */}
        <Show when={downloading()}>
          <p class="fade-in text-center text-sm text-fgMuted">
            {t('preparingDownload', {
              done: dlProgress().done,
              total: dlProgress().total,
            })}
          </p>
        </Show>
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

      {/* 出力 orb タイル長押し時の全画面プレビュー。動画タイルで mp4 が
          完成済みなら video を、それ以外は静止 PNG を表示する。
          pointer-events-none で下のボタンが pointerup を受けられる。 */}
      <Show when={previewTile()}>
        {(tile) => (
          <div
            class="fade-in pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-bg/80"
            aria-hidden="true"
          >
            <Show
              when={tile().kind === 'video' && tile().videoBlobUrl}
              fallback={
                <img
                  src={tile().blobUrl}
                  alt=""
                  draggable={false}
                  class="max-h-[90vh] max-w-[90vw] object-contain select-none touch-none"
                />
              }
            >
              <video
                src={tile().videoBlobUrl}
                muted
                playsinline
                loop
                autoplay
                class="max-h-[90vh] max-w-[90vw] object-contain select-none touch-none"
              />
            </Show>
          </div>
        )}
      </Show>
    </section>
  );
}
