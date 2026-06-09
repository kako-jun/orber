// orber#245 — orberWorker.ts / Studio.tsx の純粋ロジック切り出し。
//
// Worker のクロージャ / Solid コンポーネントに埋まっていた純粋ロジックを、
// 単体テスト可能にするためここへ切り出した（#232 abLogic.ts / #242 と同じ
// 流儀。挙動は元実装と 1 ビットも変えていない＝純粋な移動）:
//
// - `buildWasmParams`: BaseParams + worker キャッシュ → WasmParams 組立て。
//   worker のモジュール状態（source / image mask / glyph SDF キャッシュ）と
//   wasm 関数（glyph_supported）/ JS SDF 生成（generateJsGlyphSdf）は
//   引数 DI（実 wasm をロードせずにテストできる）。
// - `computeMaskSize`: image マスクデコード時の縮小寸法計算
//   （OffscreenCanvas への描画自体は worker 側 `decodeBitmapToMask` に残る）。
// - `formatRunBatchError`: worker エラー → i18n 文言キーのマップ（Studio.tsx
//   から移動。`t()` は引数 DI ＝ strings.ts（solid-js signal）を worker
//   バンドルへ引き込まない）。

// SDF テクスチャの一辺 (px)。core の DEFAULT_GLYPH_SDF_SIZE = 256 と同値で、
// wasm の get_glyph_sdf / glyph_sdf_size 検証 (16..=1024) にも収まる。
// （旧 orberGl.ts の export を引き継いだ定数。#245）
// SYNC WITH crates/core/src/glyph.rs::DEFAULT_GLYPH_SDF_SIZE
export const GLYPH_SDF_SIZE = 256;

// shape='image' のマスクを worker 内でデコードするときの長辺上限 (px)。
// SDF は最終的に GLYPH_SDF_SIZE (256) へ contain リサンプルされる
// (core の image_rgba_to_sdf) ので、4 倍の 1024 あればシルエット品質は
// 落ちない。フル解像度 (数千 px) のまま持つと、タイルごとの
// gpu_set_render_data で数十 MB の RGBA が wasm へコピーされ続けるため、
// デコード時に 1 度だけ縮めて転送量と変換コストを固定する。
export const IMAGE_MASK_TARGET_LONG_EDGE = 1024;

export interface BaseParams {
  k: number;
  width: number;
  height: number;
  seed: number;
  direction: string;
  speed: string;
  count: number;
  orb_size: number;
  blur: number;
  shape: string;
  // Phase B (#55): UI から流れる advanced 軸。空文字は "未指定（既存挙動）"。
  glyph_char?: string;
  count_preset?: string;
  speed_preset?: string;
  softness_preset?: string;
  // #136: Glyph 回転 ON/OFF。`true` 既定で従来挙動、`false` で静止描画。
  glyph_rotate?: boolean;
}

/** `setSource` で worker にキャッシュされる入力画像 RGB。 */
export interface SourceCache {
  rgb: Uint8Array;
  width: number;
  height: number;
}

/** `setImageShape` で worker にキャッシュされる shape='image' マスク RGBA。 */
export interface ImageMaskCache {
  rgba: Uint8Array;
  width: number;
  height: number;
}

/**
 * 同梱フォント外の字の JS 生成 SDF キャッシュ（ch 単位）。worker のモジュール
 * 変数だったものを、`buildWasmParams` が読み書きできる可変リファレンスとして
 * DI する（`current` の差し替えが元実装の `cachedJsGlyphSdf = {...}` 代入と
 * 同じ意味になる）。
 */
export interface GlyphSdfCacheRef {
  current: { ch: string; sdf: Uint8Array } | null;
}

/** `buildWasmParams` が依存する worker 状態 + 関数の束（引数 DI 用）。 */
export interface BuildWasmParamsDeps {
  source: SourceCache | null;
  imageMask: ImageMaskCache | null;
  /** wasm の `glyph_supported(ch)`（同梱フォント収録判定）。 */
  glyphSupported: (ch: string) => boolean;
  /** JS フォールバック SDF 生成（`generateJsGlyphSdf`）。 */
  generateSdf: (ch: string, size: number) => Uint8Array;
  glyphSdfCache: GlyphSdfCacheRef;
}

/**
 * BaseParams + worker キャッシュ (source / image mask / glyph SDF) から
 * wasm の WasmParams を組む。
 *
 * - shape='glyph': 同梱フォント収録字は wasm の core フォント経路
 *   (glyph_char) に任せる。収録外の字は generateSdf で SDF 化して
 *   glyph_sdf / glyph_sdf_size に乗せる (#231 / #159 と同設計)
 * - shape='image': imageMask を image_mask_* に乗せる (#231)。
 *   SDF 化は core の image_rgba_to_sdf
 * - transparentBackground: 透過 export (#56)。wasm 側で bg.a=0 になる。
 *   false / 省略時は `transparent_background` キー自体を付けない
 *   （serde default = false。既存呼び出しのバイト列を変えない）
 */
export function buildWasmParams(
  p: BaseParams,
  deps: BuildWasmParamsDeps,
  opts?: { transparentBackground?: boolean },
): Record<string, unknown> {
  if (!deps.source) {
    throw new Error('source not set — call setSource before generate/animate');
  }
  const params: Record<string, unknown> = {
    ...p,
    source_rgb: deps.source.rgb,
    source_width: deps.source.width,
    source_height: deps.source.height,
  };
  if (p.shape === 'image') {
    if (!deps.imageMask) {
      throw new Error('image shape requires setImageShape before generate');
    }
    params.image_mask_rgba = deps.imageMask.rgba;
    params.image_mask_width = deps.imageMask.width;
    params.image_mask_height = deps.imageMask.height;
  } else if (p.shape === 'glyph' && p.glyph_char && !deps.glyphSupported(p.glyph_char)) {
    // #159 / #231: 同梱フォント (Noto Sans Symbols 2 サブセット) に無い字は
    // worker 内 OffscreenCanvas + OS フォントスタックでラスタライズして SDF 化
    // する。端末ごとに見た目が変わり得るが、「ユーザーが入れた字を尊重して
    // 描画する」を優先する仕様 (WebGL 時代から不変)。
    const cache = deps.glyphSdfCache;
    if (!cache.current || cache.current.ch !== p.glyph_char) {
      cache.current = { ch: p.glyph_char, sdf: deps.generateSdf(p.glyph_char, GLYPH_SDF_SIZE) };
    }
    params.glyph_sdf = cache.current.sdf;
    params.glyph_sdf_size = GLYPH_SDF_SIZE;
  }
  if (opts?.transparentBackground) {
    params.transparent_background = true;
  }
  return params;
}

/**
 * #160 / #245: image マスクの縮小後寸法を計算する（`decodeBitmapToMask` の
 * 寸法計算部）。長辺 `maxLongEdge` まで縮小（アスペクト維持）、それ以下なら
 * 等倍。丸めで 0 にならないよう両軸とも最小 1px。
 */
export function computeMaskSize(
  width: number,
  height: number,
  maxLongEdge: number,
): { width: number; height: number } {
  const longest = Math.max(width, height);
  const scale = Math.min(1, maxLongEdge / Math.max(1, longest));
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

/** `formatRunBatchError` がマップ先に使う i18n キー（strings.ts の部分集合）。 */
export type RunBatchErrorKey = 'imageShapeNoContrast' | 'webgpuUnsupported';

/**
 * #169: runBatch から伝播してくる worker エラーを i18n 文言にマップする。
 * image-shape-no-contrast はシルエット抽出に失敗したことを示す内部 sentinel
 * (#245 以降は core の image_rgba_to_sdf 失敗を worker がこの sentinel に
 * 変換する)。webgpu-unsupported は WebGPU 非対応環境 (#245。#207 裁定で
 * fallback 無し = 生成不可)。`Error` インスタンスなら .message を見て、それ
 * 以外は String(e) で文字列化する (N2)。
 *
 * sentinel 照合は `includes` なので、worker → orberClient の往復で付く
 * `Error: ` 前置き（worker は `String(err)` で post する）が挟まっても生きる。
 */
export function formatRunBatchError(
  e: unknown,
  t: (key: RunBatchErrorKey) => string,
): string {
  const msg = e instanceof Error ? e.message : String(e);
  if (msg.includes('image-shape-no-contrast')) {
    return t('imageShapeNoContrast');
  }
  if (msg.includes('webgpu-unsupported')) {
    return t('webgpuUnsupported');
  }
  return msg;
}
