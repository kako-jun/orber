//! バッチ PNG 生成 API（GUI / WASM フロントエンドが叩く想定）。
//!
//! 1 枚の入力画像と複数の [`VariationSpec`] を受け取り、各 spec ごとに
//! `t = 0` フレームを描画して PNG バイト列に直して返す。CLI 側の
//! `--variations` は最終的にこの API のラッパーになる予定だが、現時点では
//! まだ呼ばれていない。
//!
//! 画像 I/O や子プロセス起動を一切しないので wasm32 ターゲットでも動く。

use crate::animate::{render_frame, AnimateOptions};
use crate::cluster::{derive_background_rgba, drop_dominant, extract_clusters, Cluster, ClusterError};
use crate::orb::OrbShape;
use crate::variations::VariationSpec;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbImage};

/// バッチ描画の入力。
pub struct BatchInput {
    /// 元画像（RGB）。kmeans の入力。
    pub source: RgbImage,
    /// kmeans の k。
    pub k: usize,
    /// 出力キャンバス幅。
    pub width: u32,
    /// 出力キャンバス高さ。
    pub height: u32,
    /// 描画形状（全 spec 共通。VariationSpec 自身は shape を持たない）。
    pub shape: OrbShape,
    /// 描画する spec 群。各 spec ごとに 1 枚の PNG が返る。
    pub specs: Vec<VariationSpec>,
}

/// 各 spec について 1 枚ずつ PNG を生成する。
///
/// 戻り値の長さは `input.specs.len()` と等しい。`Vec<u8>` はそのまま
/// `<img src="data:image/png;base64,...">` 等で使える PNG ファイルの中身。
///
/// # パニック
///
/// PNG エンコードに失敗した場合パニックする。`tiny-skia` の出力は常に
/// RGBA8 なのでフォーマット由来の失敗は実用上起きない。
pub fn generate_batch(input: BatchInput) -> Result<Vec<Vec<u8>>, ClusterError> {
    let clusters_full = extract_clusters(&input.source, input.k)?;
    let bg = derive_background_rgba(&clusters_full);
    let clusters: Vec<Cluster> = drop_dominant(&clusters_full);

    Ok(input
        .specs
        .iter()
        .map(|spec| {
            let opts = AnimateOptions {
                width: input.width,
                height: input.height,
                seed: spec.seed,
                direction: spec.direction,
                speed: spec.speed,
                count: Some(spec.count),
                orb_size: spec.orb_size,
                blur: spec.blur,
                saturation: 1.0,
                background: bg,
                shape: input.shape,
            };
            let frame = render_frame(&clusters, &opts, 0.0);
            let mut buf = Vec::new();
            let encoder = PngEncoder::new(&mut buf);
            encoder
                .write_image(
                    frame.as_raw(),
                    frame.width(),
                    frame.height(),
                    ExtendedColorType::Rgba8,
                )
                .expect("PNG encode of RGBA8 frame should not fail");
            buf
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::variations::DEFAULT_VARIATIONS;
    use image::{ImageBuffer, Rgb};

    fn synthetic_source() -> RgbImage {
        // 4x4 で 4 色を散らした入力。kmeans が縮退しない程度に色を分ける。
        let pixels: [[u8; 3]; 16] = [
            [255, 0, 0], [200, 30, 30], [255, 0, 0], [200, 30, 30],
            [0, 200, 0], [30, 220, 30], [0, 200, 0], [30, 220, 30],
            [0, 0, 200], [30, 30, 220], [0, 0, 200], [30, 30, 220],
            [240, 240, 240], [200, 200, 200], [240, 240, 240], [200, 200, 200],
        ];
        let mut img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(4, 4);
        for (i, p) in pixels.iter().enumerate() {
            let x = (i as u32) % 4;
            let y = (i as u32) / 4;
            img.put_pixel(x, y, Rgb(*p));
        }
        img
    }

    #[test]
    fn generate_batch_returns_one_png_per_spec() {
        let specs = DEFAULT_VARIATIONS.iter().take(2).copied().collect::<Vec<_>>();
        let n = specs.len();
        let input = BatchInput {
            source: synthetic_source(),
            k: 4,
            width: 64,
            height: 64,
            shape: OrbShape::Circle,
            specs,
        };
        let pngs = generate_batch(input).expect("kmeans should succeed on 4x4 RGB input");
        assert_eq!(pngs.len(), n, "1 PNG per spec");
        const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        for (i, png) in pngs.iter().enumerate() {
            assert!(
                png.starts_with(PNG_MAGIC),
                "spec {i} output does not start with PNG magic bytes"
            );
            assert!(png.len() > PNG_MAGIC.len(), "spec {i} PNG is suspiciously small");
        }
    }
}
