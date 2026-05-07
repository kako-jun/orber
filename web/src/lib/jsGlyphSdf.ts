// orber#159 — JS-side glyph SDF fallback.
//
// crates/core/src/glyph.rs の `render_glyph_sdf` と互換な Uint8Array(size*size) を
// ブラウザ側で生成する。OffscreenCanvas 2D で 1 文字をラスタライズ → alpha
// チャンネルから binary mask → 2D Euclidean Distance Transform → SDF byte 配列。
//
// 用途: ユーザーが入力した文字が wasm の `glyph_supported(ch)` で false (= 同梱
// フォントで扱えない) のときの fallback。color emoji は alpha 抽出でシルエット
// 化され、orber の monochrome 出力に自然に乗る。
//
// Rust 経路 (`crates/core/src/glyph.rs`) と数値が一致するように:
// - GLYPH_SDF_MAX_DIST_FACTOR = 0.06 (norm = size * 0.06)
// - inside (alpha >= 128) は signed_px = sqrt(dist_to_outside) - 0.5
// - outside (alpha < 128) は signed_px = 0.5 - sqrt(dist_to_inside)
// - 最終バイト = ((signed_unit * 0.5 + 0.5) * 255).round().clamp(0, 255)
//
// この定数は Rust 側 `GLYPH_SDF_MAX_DIST_FACTOR` と同期させること。値を変える
// なら `crates/core/src/glyph.rs:36` も同時更新。

export const GLYPH_SDF_MAX_DIST_FACTOR = 0.06;

// 絵文字 / 漢字 / 任意 Unicode を OS フォントスタックでラスタライズするための
// font 文字列。Apple / Microsoft / Google の color emoji を順に試し、最後に
// 同梱 Noto Sans Symbols 2 (Base.astro で読込) と system-ui に落ちる。
// Canvas 2D の fillText は color emoji も alpha チャンネルに乗せてくれる
// (実体は color glyph の不透明領域 = silhouette)。
const GLYPH_FONT_STACK =
  '"Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji", "Noto Sans Symbols 2", system-ui, sans-serif';

/**
 * 1 文字 `ch` を `size`×`size` の SDF Uint8Array にラスタライズする。
 * Worker / main thread のどちらでも呼べる (`OffscreenCanvas` 必須)。
 *
 * 戻り値は wasm `get_glyph_sdf` と同じ符号・正規化のバイト列で、そのまま
 * `renderer.setGlyphSdf(out, size)` に渡せる。
 *
 * 文字が描画できない (alpha が全 0) 場合は全 0 の配列を返す (Rust 側と同挙動)。
 */
export function generateJsGlyphSdf(ch: string, size: number): Uint8Array {
  const s = Math.max(1, size | 0);
  if (typeof OffscreenCanvas === 'undefined') {
    // SSR / 古いブラウザでは描画できないので空 SDF を返す。実機運用では
    // worker / 最近のブラウザのみがこの関数に到達する。
    return new Uint8Array(s * s);
  }
  const canvas = new OffscreenCanvas(s, s);
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  if (!ctx) return new Uint8Array(s * s);

  // 背景は透明 (clearRect でリセット)。文字は白で描画 → alpha = 不透明領域。
  ctx.clearRect(0, 0, s, s);
  // フォントサイズは「size の 75%」程度。Rust 側 ttf-parser ベースの content
  // span (1/√2 ≈ 0.707) と近い値で、shader UV と整合する。
  const fontPx = Math.max(8, Math.round(s * 0.75));
  ctx.font = `${fontPx}px ${GLYPH_FONT_STACK}`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = '#ffffff';
  ctx.fillText(ch, s / 2, s / 2);

  const img = ctx.getImageData(0, 0, s, s);
  const data = img.data; // Uint8ClampedArray RGBA

  // alpha >= 128 を inside とみなす binary mask。
  const inside = new Uint8Array(s * s);
  let anyInside = false;
  for (let i = 0; i < s * s; i++) {
    const a = data[i * 4 + 3];
    if (a >= 128) {
      inside[i] = 1;
      anyInside = true;
    }
  }
  if (!anyInside) return new Uint8Array(s * s);
  return computeSdfFromMask(inside, s);
}

/**
 * #160 結果型。`sdf` は SDF Uint8Array、`ok` は false ならシルエット抽出に
 * 失敗したことを示す (#169: 単色塗りなどで inside / outside の分離が
 * 取れない画像)。呼び出し側は `ok=false` のとき UI に「コントラスト不足」を
 * 通知する。
 */
export interface ImageSdfResult {
  sdf: Uint8Array;
  ok: boolean;
}

/**
 * #160: 任意の `ImageBitmap` (PNG / JPG / WebP / SVG decode 結果) を
 * `size`×`size` の SDF Uint8Array にラスタライズする。
 *
 * しきい値:
 * - 透過画像 (alpha < 255 のピクセルが画像全体の **1% 以上**): alpha >= 128
 *   を inside とする (#171: 単発 stray 透過 1px で経路が暴れる問題を回避)
 * - 不透明画像 (上記以外): 輝度 (Y = 0.299R + 0.587G + 0.114B) で二値化。
 *   平均輝度を境界に **inside ピクセルが少数派** になる側を採用する (典型的
 *   な被写体は背景より小領域である前提のヒューリスティック)
 *
 * #170: `invert` が true なら inside / outside を強制的に逆転させる
 * (被写体が画面の半分以上を占める画像で自動判定が反転するときの救済)。
 *
 * #169: シルエットが抽出できない (= inside ピクセルが 0、または全画素が
 * inside でコントラストが無い) 場合は `ok: false` を返し、呼び出し側で UI
 * 通知を行う。`sdf` は念のため全 0 を入れる。
 *
 * 出力フォーマットは `generateJsGlyphSdf` と同一 (R8 size×size)、
 * `renderer.setGlyphSdf` にそのまま渡せる。
 */
export function generateImageSdf(
  bitmap: ImageBitmap,
  size: number,
  invert: boolean = false,
): ImageSdfResult {
  const s = Math.max(1, size | 0);
  if (typeof OffscreenCanvas === 'undefined') {
    return { sdf: new Uint8Array(s * s), ok: false };
  }
  const canvas = new OffscreenCanvas(s, s);
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  if (!ctx) return { sdf: new Uint8Array(s * s), ok: false };

  // 元画像のアスペクトを保ったまま s×s に「contain」リサンプル (上下/左右に
  // 余白を入れる)。シルエットが歪まないようにするため。
  ctx.clearRect(0, 0, s, s);
  const bw = bitmap.width || 1;
  const bh = bitmap.height || 1;
  const scale = Math.min(s / bw, s / bh);
  const dw = Math.max(1, Math.round(bw * scale));
  const dh = Math.max(1, Math.round(bh * scale));
  const dx = Math.round((s - dw) / 2);
  const dy = Math.round((s - dh) / 2);
  ctx.drawImage(bitmap, 0, 0, bw, bh, dx, dy, dw, dh);

  const img = ctx.getImageData(0, 0, s, s);
  const data = img.data;

  // #171: 透過判定は「alpha < 255 のピクセルが画像全体の 1% 以上」で初めて
  // 透過画像とみなす。1 px 単位の混入 (JPEG → PNG 変換のロス由来など) で
  // alpha 経路に倒れて輝度ベースの被写体抽出が走らなくなる事故を防ぐ。
  let alphaPixelCount = 0;
  for (let i = 0; i < s * s; i++) {
    if (data[i * 4 + 3] < 255) alphaPixelCount++;
  }
  const hasMeaningfulAlpha = alphaPixelCount * 100 > s * s;

  const inside = new Uint8Array(s * s);
  let insideCount = 0;
  if (hasMeaningfulAlpha) {
    // alpha しきい値経路
    for (let i = 0; i < s * s; i++) {
      if (data[i * 4 + 3] >= 128) {
        inside[i] = 1;
        insideCount++;
      }
    }
  } else {
    // 輝度しきい値経路 (auto-polarity: 少数派 = 被写体)
    let sumY = 0;
    const yBuf = new Float32Array(s * s);
    for (let i = 0; i < s * s; i++) {
      const r = data[i * 4];
      const g = data[i * 4 + 1];
      const b = data[i * 4 + 2];
      const y = 0.299 * r + 0.587 * g + 0.114 * b;
      yBuf[i] = y;
      sumY += y;
    }
    const avgY = sumY / (s * s);
    let darkCount = 0;
    for (let i = 0; i < s * s; i++) {
      if (yBuf[i] < avgY) darkCount++;
    }
    const insideIsDark = darkCount < s * s / 2;
    for (let i = 0; i < s * s; i++) {
      const isInside = insideIsDark ? yBuf[i] < avgY : yBuf[i] >= avgY;
      if (isInside) {
        inside[i] = 1;
        insideCount++;
      }
    }
  }

  // #170: invert が true なら inside/outside を flip。auto-polarity で
  // 反転誤判定された画像の救済。
  if (invert) {
    for (let i = 0; i < s * s; i++) inside[i] = inside[i] ? 0 : 1;
    insideCount = s * s - insideCount;
  }

  // #169: 全 inside でも全 outside でもコントラスト 0 として扱う。
  if (insideCount === 0 || insideCount === s * s) {
    return { sdf: new Uint8Array(s * s), ok: false };
  }

  return { sdf: computeSdfFromMask(inside, s), ok: true };
}

// inside mask (0/1) → Rust 互換の SDF Uint8Array を計算する共通経路。
// generateJsGlyphSdf / generateImageSdf の両方から使う。
function computeSdfFromMask(inside: Uint8Array, s: number): Uint8Array {
  const distInside = edt2d(inside, s, s);
  const outside = new Uint8Array(s * s);
  for (let i = 0; i < s * s; i++) outside[i] = inside[i] ? 0 : 1;
  const distOutside = edt2d(outside, s, s);
  const norm = Math.max(1, s * GLYPH_SDF_MAX_DIST_FACTOR);
  const out = new Uint8Array(s * s);
  for (let i = 0; i < s * s; i++) {
    const signedPx = inside[i]
      ? Math.sqrt(distOutside[i]) - 0.5
      : 0.5 - Math.sqrt(distInside[i]);
    const signedUnit = Math.max(-1, Math.min(1, signedPx / norm));
    out[i] = Math.max(0, Math.min(255, Math.round((signedUnit * 0.5 + 0.5) * 255)));
  }
  return out;
}

// --- Euclidean Distance Transform ---
//
// Felzenszwalb & Huttenlocher (2012) "Distance Transforms of Sampled Functions"。
// 1D EDT を行列に対して row, then column の順で適用すると 2D 距離^2 が出る。
// 入力 mask: 値 1 のセルは "feature" (距離 0)、値 0 は背景 (Infinity 起点)。
//
// 出力は Float32Array で、各セルから最寄りの feature への ユークリッド距離^2。

const INF = 1e20;

function edt2d(mask: Uint8Array, w: number, h: number): Float32Array {
  // f は dist^2 を保持する (入力時は feature=0, 非 feature=∞)。
  const f = new Float32Array(w * h);
  for (let i = 0; i < w * h; i++) f[i] = mask[i] ? 0 : INF;

  // 行ごとに 1D EDT
  const rowBuf = new Float32Array(Math.max(w, h));
  const v = new Int32Array(Math.max(w, h));
  const z = new Float32Array(Math.max(w, h) + 1);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) rowBuf[x] = f[y * w + x];
    edt1d(rowBuf, w, v, z);
    for (let x = 0; x < w; x++) f[y * w + x] = rowBuf[x];
  }
  // 列ごとに 1D EDT
  for (let x = 0; x < w; x++) {
    for (let y = 0; y < h; y++) rowBuf[y] = f[y * w + x];
    edt1d(rowBuf, h, v, z);
    for (let y = 0; y < h; y++) f[y * w + x] = rowBuf[y];
  }
  return f;
}

/**
 * 1D EDT (in-place)。
 * `f[i]` は入力時 dist^2 シード値、出力時に最終 dist^2 になる。
 * `v` (lower envelope の parabola index) と `z` (intersection 位置) は再利用バッファ。
 */
function edt1d(f: Float32Array, n: number, v: Int32Array, z: Float32Array): void {
  let k = 0;
  v[0] = 0;
  z[0] = -INF;
  z[1] = INF;
  for (let q = 1; q < n; q++) {
    let s = ((f[q] + q * q) - (f[v[k]] + v[k] * v[k])) / (2 * (q - v[k]));
    // Rust / 原論文と同じく `k > 0` ガードを入れて k=0 のときの underflow を防ぐ。
    // 現状は `z[0] = -INF` が sentinel なので無くても落ちないが、INF を有限値に
    // 差し替えた瞬間に `v[-1]` を読む潜在バグになるため明示的に守る。
    while (k > 0 && s <= z[k]) {
      k--;
      s = ((f[q] + q * q) - (f[v[k]] + v[k] * v[k])) / (2 * (q - v[k]));
    }
    k++;
    v[k] = q;
    z[k] = s;
    z[k + 1] = INF;
  }
  // src を別バッファに退避してから書き戻す
  const src = new Float32Array(n);
  for (let i = 0; i < n; i++) src[i] = f[i];
  k = 0;
  for (let q = 0; q < n; q++) {
    while (z[k + 1] < q) k++;
    const dq = q - v[k];
    f[q] = dq * dq + src[v[k]];
  }
}
