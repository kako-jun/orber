// orber#163 — strings.ts 回帰テスト。
//
// #161 で起きた SSR↔client の lang 不一致 → hydration mismatch → ja/en 混在の
// バグは目視で再現するのが難しいため、最低限の signal 経路 (detectLang() の
// 言語判定 + setLang → t() の reactive 更新 + var 補間) を vitest で押さえる。
//
// hydration それ自体のテストは @solidjs/testing-library + Astro の組合せを
// 必要とし重いため、本ファイルでは扱わない。

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

// 全 describe で共通の前後処理: 各テストでモジュールキャッシュをリセットし、
// navigator / window の stub を import タイミングで効かせる。strings.ts は
// import 時に queueMicrotask を仕込むため、stub 確定後に動的 import する。
beforeEach(() => {
  vi.resetModules();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('detectLang()', () => {
  test('navigator.language が "ja" 始まりなら "ja"', async () => {
    vi.stubGlobal('navigator', { language: 'ja-JP' });
    const { detectLang } = await import('./strings');
    expect(detectLang()).toBe('ja');
  });

  test('navigator.language が "ja" でなければ "en"', async () => {
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { detectLang } = await import('./strings');
    expect(detectLang()).toBe('en');
  });

  test('non-ja / non-en (例 fr) は "en" にフォールバック', async () => {
    vi.stubGlobal('navigator', { language: 'fr-FR' });
    const { detectLang } = await import('./strings');
    expect(detectLang()).toBe('en');
  });

  test('navigator が空オブジェクトなら "en"', async () => {
    vi.stubGlobal('navigator', {});
    const { detectLang } = await import('./strings');
    expect(detectLang()).toBe('en');
  });

  test('navigator.language が undefined なら "en" (typeof string チェック)', async () => {
    vi.stubGlobal('navigator', { language: undefined });
    const { detectLang } = await import('./strings');
    expect(detectLang()).toBe('en');
  });

  test('window 未定義 (SSR 想定) なら "en" (#161 SSR フォールバック)', async () => {
    // jsdom 環境では window が常に定義されるため、明示的に削除して SSR を再現する。
    // strings.ts は `typeof window === 'undefined'` のみで判定しているので、
    // globalThis.window を消せば SSR 経路に入る。
    vi.stubGlobal('window', undefined);
    const { detectLang } = await import('./strings');
    expect(detectLang()).toBe('en');
  });
});

describe('lang signal + t()', () => {
  test('en ブラウザで microtask flush 後も lang() === "en"', async () => {
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { lang, t } = await import('./strings');
    // microtask を確実に flush する
    await Promise.resolve();
    expect(lang()).toBe('en');
    expect(t('subtitle')).toBe(
      'Extract city lights from any image. Use as a video or stream background.',
    );
  });

  test('ja ブラウザでは microtask flush 後に lang() === "ja"', async () => {
    vi.stubGlobal('navigator', { language: 'ja-JP' });
    const { lang, t } = await import('./strings');
    await Promise.resolve();
    expect(lang()).toBe('ja');
    expect(t('subtitle')).toBe(
      '画像から街の光を抽出。配信や動画の背景に。',
    );
  });

  test('setLang() で t() が reactive に切り替わる', async () => {
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    expect(t('shapeLabel')).toBe('Shape');
    setLang('ja');
    expect(t('shapeLabel')).toBe('形状');
    setLang('en');
    expect(t('shapeLabel')).toBe('Shape');
  });

  test('t() の vars 補間が動く', async () => {
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { t, setLang } = await import('./strings');
    await Promise.resolve();
    setLang('en');
    expect(t('pickedThumbAlt', { name: 'photo.jpg' })).toBe(
      'Picked image: photo.jpg',
    );
    setLang('ja');
    expect(t('pickedThumbAlt', { name: '写真.jpg' })).toBe(
      '選択した画像: 写真.jpg',
    );
  });

  test('alphaEncodingInProgress キーが ja/en 両方定義されている (#184/#192)', async () => {
    // #192 で MOV muxer 化したため alphaEncoderLoadFailed は削除済。
    // alphaEncodingInProgress は将来 frame 数が増えた時の再利用枠で温存している。
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('ja');
    expect(t('alphaEncodingInProgress')).toMatch(/透過動画/);
    setLang('en');
    expect(t('alphaEncodingInProgress')).toMatch(/transparent video/);
  });

  test('webgpuUnsupported キーが ja/en 両方定義されている (#245)', async () => {
    // #245: worker 本番経路が WebGPU(WGSL) 化され、非対応ブラウザは
    // formatRunBatchError がこの文言にマップして生成不可を表示する
    // (#207 裁定: fallback 無し)。キーが消えると非対応環境のエラーが
    // 生の sentinel 文字列のまま出るため、ja/en 両定義を直接押さえる。
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('ja');
    expect(t('webgpuUnsupported')).toMatch(/WebGPU/);
    setLang('en');
    expect(t('webgpuUnsupported')).toMatch(/WebGPU/);
  });

  test('削除済みキー alphaEncoderLoadFailed は STRINGS に存在しない (#192)', async () => {
    // #192 で ffmpeg.wasm を撤去し外部 CDN ロード失敗のシナリオ自体が消滅した。
    // 文言キーが残っていると Studio.tsx 側で復活させて二重管理になるため、
    // 存在しないことを直接押さえる。
    const { STRINGS } = await import('./strings');
    expect('alphaEncoderLoadFailed' in STRINGS).toBe(false);
  });

  test('削除済みキー alphaVideoUnsupportedNotice は STRINGS に存在しない (#184)', async () => {
    // #184 で透過動画が ffmpeg.wasm 化され全環境で動くようになったため、
    // 旧 unsupported 文言キーを削除した。キー残存があると UI 側に死んだ
    // 文言が紐づき続けるリグレッションになるので、ここで存在しないことを直接押さえる。
    const { STRINGS } = await import('./strings');
    expect('alphaVideoUnsupportedNotice' in STRINGS).toBe(false);
  });

  test('削除済みキー includeAlphaDisabledTitle は STRINGS に存在しない (#184)', async () => {
    // 同上: 「透過動画は非対応」ツールチップ用 title 文言キー。
    const { STRINGS } = await import('./strings');
    expect('includeAlphaDisabledTitle' in STRINGS).toBe(false);
  });

  test('viewsLabelPrefix / Suffix は ja/en とも「{n} views」(suffix 統一) (Footer counter)', async () => {
    // #128 の初期設計は ja「閲覧数: {n}」/ en「{n} views」の非対称構成だったが、
    // 9e0306b「Counter ラベルを ja/en 両方 views で統一」で suffix の ' views' に
    // 一本化された。Footer の <nostalgic-counter> をこの 2 キーで挟む設計
    // (#128 / #146) のため、現行の統一仕様を直接押さえる。
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('ja');
    expect(t('viewsLabelPrefix')).toBe('');
    expect(t('viewsLabelSuffix')).toBe(' views');
    setLang('en');
    expect(t('viewsLabelPrefix')).toBe('');
    expect(t('viewsLabelSuffix')).toBe(' views');
  });

  test('shapeOptionOrb は ja/en とも定義され、旧 shapeOptionCircle は存在しない (#235)', async () => {
    // #235 で内部の shape 名を circle → orb に統一し、文言キーも
    // shapeOptionCircle → shapeOptionOrb に改名した。旧キーが残っていると
    // Studio.tsx 側で復活させて二重管理・表示揺れになるため、新キーが ja/en とも
    // 定義され、旧キーが STRINGS から消えていることを直接押さえる。
    const { STRINGS } = await import('./strings');
    expect('shapeOptionCircle' in STRINGS).toBe(false);
    expect(STRINGS.shapeOptionOrb).toBeDefined();
    expect(STRINGS.shapeOptionOrb.ja).toBe('オーブ');
    expect(STRINGS.shapeOptionOrb.en).toBe('Orb');
  });
});

// #232: A/B 比較パネル（WebGL↔WGSL トグル）の i18n 文言。検証足場だが本番と同じ
// 命名流儀（camelCase キー / ja・en 対称 / vars 補間）に揃っているかを押さえる。
// Phase 3 で WebGL を撤去するときにパネルごと削除されるため、その時このブロックも消す。
describe('ab* 文言 (#232 A/B パネル)', () => {
  // 実物の strings.ts に追加された ab* キーを列挙（#232 の 11 件 + #242 キャプチャ
  // 足場の 4 件 = 15 件）。
  const AB_KEYS = [
    'abPanelTitle',
    'abPanelNote',
    'abRendererWebGL',
    'abRendererWGSL',
    'abWebGpuUnavailable',
    'abNeedSource',
    'abStart',
    'abStop',
    'abInitMs',
    'abFps',
    'abError',
    'abCapNote',
    'abCapRun',
    'abCaptureT0',
    'abCapDone',
  ] as const;

  test('S1: ab* 系の全キーが ja / en とも定義され非空', async () => {
    const { STRINGS } = await import('./strings');
    // STRINGS 内の ab* キーが過不足なくこの一覧と一致することも押さえる
    // （キー追加/削除時にこのテストを更新し忘れない安全網）。
    const actualAbKeys = Object.keys(STRINGS).filter((k) => k.startsWith('ab')).sort();
    expect(actualAbKeys).toEqual([...AB_KEYS].sort());

    for (const key of AB_KEYS) {
      const entry = STRINGS[key];
      expect(entry, `${key} が STRINGS に存在しない`).toBeDefined();
      expect(entry.ja.length, `${key}.ja が空`).toBeGreaterThan(0);
      expect(entry.en.length, `${key}.en が空`).toBeGreaterThan(0);
    }
  });

  test('S2: t("abInitMs", {ms}) が補間される (ja/en とも)', async () => {
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('en');
    expect(t('abInitMs', { ms: '12.3' })).toBe('init 12.3 ms');
    expect(t('abInitMs', { ms: '12.3' })).not.toContain('{ms}');
    setLang('ja');
    expect(t('abInitMs', { ms: '12.3' })).toBe('init 12.3 ms');
  });

  test('S3: t("abFps", {fps}) が補間される (ja/en とも)', async () => {
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('en');
    expect(t('abFps', { fps: '60' })).toBe('60 fps');
    expect(t('abFps', { fps: '60' })).not.toContain('{fps}');
    setLang('ja');
    expect(t('abFps', { fps: '60' })).toBe('60 fps');
  });

  test('S4: t("abError", {msg}) が補間され、ja/en で文言差がある', async () => {
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('en');
    const en = t('abError', { msg: 'boom' });
    expect(en).toBe('Error: boom');
    expect(en).not.toContain('{msg}');
    setLang('ja');
    const ja = t('abError', { msg: 'boom' });
    expect(ja).toBe('エラー: boom');
    // ja / en で prefix の文言差があること（同一文字列ではない）を assert。
    expect(ja).not.toBe(en);
  });

  test('S5: abPanelNote は ja に「検証」系 / en に「dev」系の語を含む（意味対称）', async () => {
    const { STRINGS } = await import('./strings');
    expect(STRINGS.abPanelNote.ja).toMatch(/検証/);
    expect(STRINGS.abPanelNote.en).toMatch(/dev/i);
  });
});
