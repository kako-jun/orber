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

  test('にじみをぼかしへ統合し、にじみ専用キー(bleed*)も STRINGS から消える / softness は残存 (#265)', async () => {
    // #253 で 4 軸を単一「にじみ」ノブに畳み、#265 でそのにじみ独立ノブ自体を撤去して
    // 「ぼかし」(softness)へ統合した（にじみは softnessToBleedLevel で連動）。bloom/halo/
    // offset 系（#253 で削除済み）に加え、にじみ専用 bleedLabel/bleedOption* も strings.ts
    // から消した。これらのキーが復活すると Studio.tsx で旧にじみ UI を生やせてしまい
    // 二重管理・退行になるため、STRINGS に無いことを直接押さえる。逆に統合後も使う
    // softness（ぼかし）系は ja/en とも残ることを担保する。
    const { STRINGS } = await import('./strings');

    // 削除済み: bloom / halo / offset 系（#253）＋ にじみ専用 bleed*（#265）
    const removed = [
      'bloomLabel',
      'bloomOptionOff',
      'bloomOptionWeak',
      'bloomOptionMid',
      'bloomOptionStrong',
      'haloLabel',
      'haloOptionOff',
      'haloOptionWeak',
      'haloOptionMid',
      'haloOptionStrong',
      'offsetLabel',
      'offsetOptionOff',
      'offsetOptionWeak',
      'offsetOptionMid',
      'offsetOptionStrong',
      'bleedOptionOff',
      'bleedLabel',
      'bleedOptionWeak',
      'bleedOptionMid',
      'bleedOptionStrong',
    ];
    const stillPresent = removed.filter((key) => key in STRINGS);
    expect(stillPresent).toEqual([]);

    // 残存: にじみを駆動する softness（ぼかし）。ja/en 両方を持つこと。
    const retained = [
      'softnessLabel',
      'softnessOptionLow',
      'softnessOptionMid',
      'softnessOptionHigh',
    ] as const;
    for (const key of retained) {
      const entry = (STRINGS as Record<string, { ja?: unknown; en?: unknown }>)[key];
      expect(entry, `${key} が STRINGS に存在しない`).toBeDefined();
      expect(typeof entry.ja, `${key}.ja`).toBe('string');
      expect(typeof entry.en, `${key}.en`).toBe('string');
    }
  });

  test('全 STRINGS キーが ja / en 両方の string 値を持つ (#239 翻訳漏れ検出)', async () => {
    // 個別キーを 1 つずつ押さえると新規キー追加時にテストが伴走せず翻訳漏れを
    // 取りこぼす（#239 で bleed/bloom/halo/offset の 20 キーを足したが parity の
    // 自動テストが無かった）。STRINGS 全体を走査して、各キーが ja と en の
    // 両方を string 型で持つことを 1 本で担保する。将来どのキーを足しても、片言語の
    // 定義漏れ（undefined / 型違い）をここで即検出できる。空文字は意図的なものが
    // ある（viewsLabelPrefix は counter を挟む空 prefix）ため許容し、存在のみ問う。
    const { STRINGS } = await import('./strings');
    const missing: string[] = [];
    for (const [key, value] of Object.entries(STRINGS)) {
      for (const lang of ['ja', 'en'] as const) {
        const v = (value as Record<string, unknown>)[lang];
        if (typeof v !== 'string') {
          missing.push(`${key}.${lang}`);
        }
      }
    }
    expect(missing).toEqual([]);
  });
});
