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

  test('alphaVideoUnsupportedNotice キーが ja/en 両方定義されている (#56 後続)', async () => {
    // VP9 alpha 非対応環境で checkbox 直下に出す警告 (Studio.tsx)。
    // i18n キー漏れで本番ビルドに `{key}` がそのまま出るリグレッションを防ぐ。
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('ja');
    expect(t('alphaVideoUnsupportedNotice')).toMatch(/透過動画/);
    setLang('en');
    expect(t('alphaVideoUnsupportedNotice')).toMatch(/transparent video/);
  });

  test('viewsLabelPrefix / Suffix の語順が言語ごとに切り替わる (Footer counter)', async () => {
    // ja は「閲覧数: {n}」(prefix のみ)、en は「{n} views」(suffix のみ) の
    // 非対称構成。Footer の <nostalgic-counter> をこの 2 キーで挟む設計
    // (#128 / #146) のため、語順切替を直接押さえる。
    vi.stubGlobal('navigator', { language: 'en-US' });
    const { setLang, t } = await import('./strings');
    await Promise.resolve();
    setLang('ja');
    expect(t('viewsLabelPrefix')).toBe('閲覧数: ');
    expect(t('viewsLabelSuffix')).toBe('');
    setLang('en');
    expect(t('viewsLabelPrefix')).toBe('');
    expect(t('viewsLabelSuffix')).toBe(' views');
  });
});
