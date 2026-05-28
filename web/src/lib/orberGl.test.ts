// orber#198 → #201 → #203 — fragment shader の Glyph アームが
// 「SDF マスク × Circle profile」の乗算合成で構成されていることを最小限の
// boilerplate で検査する。視覚パリティ (Circle と Glyph='●' がぱっと見区別つかない)
// は kako-jun の手目視で確認する前提なので、ここでは「shader source が想定通りの
// 形で書かれているか」だけを押さえる。
//
// 直接 createGlRenderer を vitest (jsdom) で叩くと WebGL2 context が取れず
// throw するため、`_FS_FOR_TEST` でエクスポートした raw shader source を
// 文字列マッチで検証する。

import { describe, expect, test } from 'vitest';

import { _FS_FOR_TEST, GLYPH_SDF_SIZE } from './orberGl';

describe('orberGl fragment shader (#203 mask × profile)', () => {
  test('Glyph アームで SDF を smoothstep マスクに変換している', () => {
    // SDF マスクの宣言と smoothstep 適用が両方残っていること。0.05 の閾値は
    // SDF 解像度 256 に対する経験則値で、これが書き換えられたらレビュー対象。
    expect(_FS_FOR_TEST).toContain('float sdf_mask;');
    expect(_FS_FOR_TEST).toContain('smoothstep(-0.05, 0.05, signed_unit);');
  });

  test('Glyph アームの radial_alpha は Circle と同じ falloff_curve(r_euclid)', () => {
    expect(_FS_FOR_TEST).toContain('float r_euclid = dist / radius;');
    expect(_FS_FOR_TEST).toContain(
      'float radial_alpha = falloff_curve(style_bit, r_euclid, blur, opacity);',
    );
  });

  test('Glyph アームの合成は radial_alpha * sdf_mask の乗算 (#203)', () => {
    // この行が #203 の核心。max(...) や ぼやけた合成式に退行していないことを担保する。
    expect(_FS_FOR_TEST).toContain('alpha = radial_alpha * sdf_mask;');
  });

  test('UV 範囲外では sdf_mask = 0 で透明確定', () => {
    // grafer 外側のテクスチャ参照を無効化するガード。
    expect(_FS_FOR_TEST).toContain('sdf_mask = 0.0;');
  });

  test('旧仕様 (r-max / alpha-max) の痕跡が残っていない', () => {
    // #198 / #201 の合成式が復活していないこと。alpha_sdf / alpha_euclid という
    // 旧変数名は使わない方針。
    expect(_FS_FOR_TEST).not.toContain('float alpha_sdf');
    expect(_FS_FOR_TEST).not.toContain('float alpha_euclid');
    expect(_FS_FOR_TEST).not.toContain('max(alpha_sdf, alpha_euclid)');
    // r_sdf 変数自体が #203 で消滅したのでリテラル代入も無い。
    expect(_FS_FOR_TEST).not.toMatch(/\br_sdf\s*=/);
  });

  test('Circle アーム (u_shape_id == 0) は従来式 r = dist / radius のまま', () => {
    // else ブロック内に Circle 固有の dist 宣言 + r = dist / radius シーケンスが残っていること。
    // Glyph アームの r_euclid と同じ式だが、Circle 側はローカル変数名 `dist` を経由した
    // この 2 行シーケンスが識別子になる。
    expect(_FS_FOR_TEST).toMatch(/float dist = distance\(px, vec2\(cx, cy\)\);\s*\n\s*float r = dist \/ radius;/);
  });

  test('falloff_curve の定義は 1 つだけ (Glyph と Circle で共有)', () => {
    const defCount = (_FS_FOR_TEST.match(/float falloff_curve\(/g) ?? []).length;
    expect(defCount).toBe(1);
  });

  test('falloff_curve の実呼び出しは Glyph 1 + Circle 1 = 2 回 (#203 で旧 3 回から削減)', () => {
    // #201 では Glyph アームで alpha_sdf / alpha_euclid と 2 回呼んでいたが、
    // #203 では radial_alpha 1 本に統合したので Glyph 経路の呼び出しは 1 回に減る。
    const callSites = _FS_FOR_TEST.match(/falloff_curve\(style_bit,/g) ?? [];
    expect(callSites.length).toBe(2);
  });

  test('Circle アームの falloff_curve(style_bit, r, blur, opacity) は 1 回だけ', () => {
    // Glyph 経路は r_euclid 引数なのでこのリテラルは Circle アームでのみ出現する。
    const calls = _FS_FOR_TEST.match(/falloff_curve\(style_bit, r, blur, opacity\)/g) ?? [];
    expect(calls.length).toBe(1);
  });

  test('GLYPH_SDF_CONTENT_SPAN の GLSL 宣言が TS 定数と整合している', () => {
    // 既存の sanity check。#147 で導入された定数同期を保つ。
    expect(_FS_FOR_TEST).toContain('const float GLYPH_SDF_CONTENT_SPAN = 0.70710678;');
    // GLYPH_SDF_SIZE は shader 内では使わないが export 経由で固定値を健全性確認。
    expect(GLYPH_SDF_SIZE).toBe(256);
  });

  test('shader source に #198 / #201 / #203 の試行履歴コメントが残っている', () => {
    // 将来「なぜ mask × profile を取っているのか」が分からず削除される事故予防として、
    // shader source 内に試行履歴の参照が残っていることを担保する。
    expect(_FS_FOR_TEST).toMatch(/#198/);
    expect(_FS_FOR_TEST).toMatch(/#201/);
    expect(_FS_FOR_TEST).toMatch(/#203/);
  });
});
