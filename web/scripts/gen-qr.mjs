// orber#128 — Build-time QR code generator.
// 公開 URL を encode した SVG を `web/public/orber-qr.svg` に書き出す。
// `qrcode` パッケージは build 時のみ使い、ランタイム bundle には含めない。
//
// 使い方: `node scripts/gen-qr.mjs`
// ビルド時に `npm run build` の前段で自動実行する (package.json の build script で連結)。
//
// 色は DESIGN.md §2 のトークンに揃える: bg `#040404`, fg `#FFFFFF`。
// ハードコード値だが SVG は CSS variable を解釈しないので literal で持つしかない。
// DESIGN.md と乖離しないよう、token 値を変えたらこのファイルも更新する。

import { writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import QRCode from 'qrcode';

const __dirname = dirname(fileURLToPath(import.meta.url));
const TARGET = 'https://orber.llll-ll.com/';
const OUT_PATH = resolve(__dirname, '../public/orber-qr.svg');

const svg = await QRCode.toString(TARGET, {
  type: 'svg',
  width: 160,
  margin: 1,
  color: {
    dark: '#040404',
    light: '#FFFFFF',
  },
});

writeFileSync(OUT_PATH, svg, 'utf8');
console.log(`[gen-qr] wrote ${OUT_PATH} (target: ${TARGET})`);
