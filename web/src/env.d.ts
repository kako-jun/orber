/// <reference path="../.astro/types.d.ts" />
/// <reference types="astro/client" />

// orber#146 — Vite の define で source 内の `__BUILD_DATE__` を
// `"YYYY-MM-DD"` 文字列リテラルに build 時置換する (astro.config.mjs)。
declare const __BUILD_DATE__: string;

// orber#128 — Web Component intrinsic for Nostalgic Counter (Footer.tsx).
// solid-js の JSX intrinsic を拡張し、`<nostalgic-counter id="..." type="..."
// format="..." />` を SolidJS 経路で型エラーなく書けるようにする。
// 属性は kebab-case のまま (Web Components の規約)。Solid は HTML 属性を
// そのまま渡すので id / type / format は string で十分。
declare module 'solid-js' {
  namespace JSX {
    interface IntrinsicElements {
      'nostalgic-counter': {
        id: string;
        type?: 'total' | 'today' | 'yesterday' | 'week' | 'month';
        format?: 'text' | 'image' | 'interactive';
      };
    }
  }
}
