import { defineConfig } from 'vitest/config';

// orber#163 — vitest 設定。lang signal / detectLang() 周りの回帰テストを
// jsdom 環境で実行する。Astro / Solid hydration 自体のテストは対象外で、
// `web/src/lib/` 配下の純粋ロジックを単体テスト化するのが現状の目的。
export default defineConfig({
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.ts'],
    // strings.ts は import 時に `queueMicrotask` を仕込む。各 test ファイルで
    // モジュールキャッシュを使い回すと前のテストの microtask 遅延が次に漏れる
    // ことがあるため、isolation はデフォルト (file 単位) のままにする。
  },
  resolve: {
    conditions: ['development', 'browser'],
  },
});
