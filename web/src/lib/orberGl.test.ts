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
    // else ブロックに dist/radius の Circle 計算が残っていること。
    // Glyph 側の r_euclid も同じ式だが、Circle 側は `float r = dist / radius;` の
    // 形のままなのでこちらで識別できる。
    expect(_FS_FOR_TEST).toContain('float r = dist / radius;');
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
});
