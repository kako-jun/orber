// orber#232 — WebGPU 対応判定。Studio の A/B 比較パネル（WebGL↔WGSL トグル）が
// WGSL ボタンを enable できるかを決めるためだけに使う最小ユーティリティ。
//
// 判定は encodeMp4.ts の `isWebCodecsSupported()` と同じ「型が存在するか」だけの
// 軽量チェックに揃える。adapter が実際に取れるか（gpu_init の成否）はここでは
// 見ない — navigator.gpu があってもアダプタ取得に失敗する環境は gpu_init 側の
// reject で扱う（gpu-lab.astro と同設計、#207 fallback 無し方針）。
//
// このファイルは A/B 検証足場の一部であり、Phase 3 で WebGL を撤去するときに
// パネルごと不要になる（CLAUDE.md / AbPanel.tsx の削除予定コメント参照）。

export function isWebGpuSupported(): boolean {
  return typeof navigator !== 'undefined' && 'gpu' in navigator;
}
