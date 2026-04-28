export interface DecodedImage {
  rgb: Uint8Array;
  width: number;
  height: number;
}

// orber-wasm 側 `validate_params` が辺長 8192 を超える入力を reject するので、
// JS 側でも先に弾いてブラウザのメモリ圧迫を避ける。
const MAX_DIM = 8192;

/**
 * `File` をデコードして RGB バイト列にする。
 *
 * orber-wasm の `generate_batch` には RGBA ではなく RGB を渡す必要があるため、
 * canvas 経由で取り出した RGBA から alpha を落とす。
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
    const canvas = document.createElement('canvas');
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('canvas 2d context unavailable');
    ctx.drawImage(bitmap, 0, 0);
    const imgData = ctx.getImageData(0, 0, canvas.width, canvas.height);
    const px = canvas.width * canvas.height;
    const rgb = new Uint8Array(px * 3);
    for (let i = 0, j = 0; i < imgData.data.length; i += 4, j += 3) {
      rgb[j] = imgData.data[i];
      rgb[j + 1] = imgData.data[i + 1];
      rgb[j + 2] = imgData.data[i + 2];
    }
    return { rgb, width: canvas.width, height: canvas.height };
  } finally {
    bitmap.close?.();
  }
}
