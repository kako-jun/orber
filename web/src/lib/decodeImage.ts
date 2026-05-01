export interface DecodedImage {
  rgb: Uint8Array;
  width: number;
  height: number;
}

// orber-wasm 側 `validate_params` が辺長 8192 を超える入力を reject するので、
// JS 側でも先に弾いてブラウザのメモリ圧迫を避ける。
const MAX_DIM = 8192;

/**
 * kmeans 用に画像を縮小する目標サイズ（長辺の最大値）。
 *
 * orber は元画像の RGB を kmeans の入力としてしか使わない（描画は WebGL で
 * クラスタの色だけ参照）。kmeans の色抽出は十分なサンプル点があれば高解像度を
 * 必要としないので、長辺 256 まで縮小して以下を全て同時に解決する:
 *
 * - JS→Worker→wasm の RGB 転送量が固定（≤ 196KB）になる。元 4032×3024 の写真は
 *   36MB あり、Android では毎タイル wasm に渡るたびに数百ms のロスになっていた
 *   (kako-jun 実機計測 2026-05-01)
 * - kmeans 自体（3 runs × 20 iter）が 65k pixel 以下で動くので速い
 * - キャッシュ fingerprint も RGB 長が固定で安定
 *
 * 256 は ImageMagick / 各種 palette tool の経験則。これより小さくすると支配色
 * 判定がブレる可能性。アスペクトは維持。
 */
const KMEANS_TARGET_LONG_EDGE = 256;

/**
 * `File` をデコードして RGB バイト列にする。
 *
 * orber-wasm の kmeans に渡すため、長辺 `KMEANS_TARGET_LONG_EDGE` 以下に
 * 縮小する。アスペクトは維持。元画像のフルサイズを保持しないのは、描画には
 * RGB 自体が不要（クラスタの色だけ使う）ため。これにより wasm への転送量が
 * ソース解像度に依存しなくなる。
 */
export async function decodeImageToRgb(file: File): Promise<DecodedImage> {
  if (!file.type.startsWith('image/')) {
    throw new Error(`not an image: ${file.type || 'unknown'}`);
  }
  const bitmap = await createImageBitmap(file);
  if (bitmap.width > MAX_DIM || bitmap.height > MAX_DIM) {
    bitmap.close?.();
    throw new Error(
      `image too large: ${bitmap.width}x${bitmap.height} (max ${MAX_DIM} per side)`,
    );
  }
  try {
    const longest = Math.max(bitmap.width, bitmap.height);
    const scale = Math.min(1, KMEANS_TARGET_LONG_EDGE / longest);
    // review S2: 極端アスペクト (10000×100 のパノラマ等) で短辺が 1-3 px に
    // 潰れて kmeans サンプル数が枯れるのを防ぐ。8 px は kmeans K=5 のとき
    // サンプル枯渇の安全側下限。
    const MIN_EDGE = 8;
    const dw = Math.max(MIN_EDGE, Math.round(bitmap.width * scale));
    const dh = Math.max(MIN_EDGE, Math.round(bitmap.height * scale));

    const canvas = document.createElement('canvas');
    canvas.width = dw;
    canvas.height = dh;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('canvas 2d context unavailable');
    // review S1: imageSmoothingQuality の既定は実装依存 (Chromium 'low')。
    // kmeans 入力としては low でもサンプル分布はほぼ保たれるが、明示的に
    // 'medium' を立てて将来のブラウザ既定変更に左右されないようにする。
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = 'medium';
    ctx.drawImage(bitmap, 0, 0, dw, dh);
    const imgData = ctx.getImageData(0, 0, dw, dh);
    const px = dw * dh;
    const rgb = new Uint8Array(px * 3);
    for (let i = 0, j = 0; i < imgData.data.length; i += 4, j += 3) {
      rgb[j] = imgData.data[i];
      rgb[j + 1] = imgData.data[i + 1];
      rgb[j + 2] = imgData.data[i + 2];
    }
    return { rgb, width: dw, height: dh };
  } finally {
    bitmap.close?.();
  }
}
