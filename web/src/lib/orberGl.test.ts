// orber#198 — fragment shader の Glyph アームに SDF + Euclidean の max 合成が
// 入っていることを最小限の boilerplate で検査する。視覚パリティ (Circle と
// Glyph='●' がぱっと見区別つかない) は kako-jun の手目視で確認する前提なので、
// ここでは「shader source が想定通りの形で書かれているか」だけを押さえる。
//
// 直接 createGlRenderer を vitest (jsdom) で叩くと WebGL2 context が取れず
// throw するため、`_FS_FOR_TEST` でエクスポートした raw shader source を
// 文字列マッチで検証する。

import { describe, expect, test } from 'vitest';

import { _FS_FOR_TEST, GLYPH_SDF_SIZE } from './orberGl';

describe('orberGl fragment shader (#198)', () => {
  test('Glyph アームで r_sdf と r_euclid の両方を計算している', () => {
    expect(_FS_FOR_TEST).toContain('float r_euclid = dist / radius;');
    expect(_FS_FOR_TEST).toContain('r_sdf = 1.0 - signed_unit;');
  });

  test('Glyph アームで max(r_sdf, r_euclid) を採用している', () => {
    expect(_FS_FOR_TEST).toContain('float r = max(r_sdf, r_euclid);');
  });

  test('UV 範囲外では r_sdf を 1.0 超に固定する', () => {
    // 値は実装上 2.0。falloff_curve(r >= 1.0) が 0 を返す挙動に乗る。
    expect(_FS_FOR_TEST).toContain('r_sdf = 2.0;');
  });

  test('Circle アーム (u_shape_id == 0) は従来式 r = dist / radius のまま', () => {
    // else ブロック内に Circle 固有の dist 宣言 + r = dist / radius シーケンスが残っていること。
    // Glyph アームの r_euclid と同じ式だが、Circle 側はローカル変数名 `dist` を経由した
    // この 2 行シーケンスが識別子になる。
    expect(_FS_FOR_TEST).toMatch(/float dist = distance\(px, vec2\(cx, cy\)\);\s*\n\s*float r = dist \/ radius;/);
  });

  test('Circle 経路の falloff_curve 呼び出しは Glyph 経路と同じ関数を共有している', () => {
    // falloff_curve がただ 1 つ定義されている (refactor で 2 系統に分裂していない) こと。
    const defCount = (_FS_FOR_TEST.match(/float falloff_curve\(/g) ?? []).length;
    expect(defCount).toBe(1);
  });

  test('GLYPH_SDF_CONTENT_SPAN の GLSL 宣言が TS 定数と整合している', () => {
    // 既存の sanity check。#147 で導入された定数同期を保つ。
    expect(_FS_FOR_TEST).toContain('const float GLYPH_SDF_CONTENT_SPAN = 0.70710678;');
    // GLYPH_SDF_SIZE は shader 内では使わないが export 経由で固定値を健全性確認。
    expect(GLYPH_SDF_SIZE).toBe(256);
  });

  test('shader source に #198 の履歴コメントが残っている', () => {
    // 将来「なぜ max(r_sdf, r_euclid) を取っているのか」が分からず削除される
    // 事故予防として、shader source 内に #198 の参照が残っていることを担保する。
    expect(_FS_FOR_TEST).toMatch(/#198/);
  });

  test('r_sdf へのリテラル代入は 2.0 のみ (defensive 値の逆方向改変を検出)', () => {
    // `r_sdf = 0.0;` や `r_sdf = 0.5;` のようなリテラル代入が混入すると、
    // UV 外で透明にならず描画されてしまう。`r_sdf = 1.0 - signed_unit;` のような
    // 式代入はここでは拾わず、リテラル数値代入だけを検査する。
    const literalAssignments = [..._FS_FOR_TEST.matchAll(/r_sdf\s*=\s*([0-9.]+)\s*;/g)].map(
      (m) => m[1],
    );
    expect(literalAssignments.length).toBeGreaterThan(0);
    for (const val of literalAssignments) {
      expect(val).toBe('2.0');
    }
  });

  test('falloff_curve(style_bit, r, blur, opacity) は Glyph / Circle 両アームで 1 回ずつ呼ばれる', () => {
    // refactor で if-else どちらかから呼び出しを消してしまう事故を検出する。
    const calls = _FS_FOR_TEST.match(/falloff_curve\(style_bit, r, blur, opacity\)/g) ?? [];
    expect(calls.length).toBe(2);
  });
});
