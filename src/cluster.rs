//! 入力画像から代表色クラスタを抽出するモジュール。
//!
//! LAB 色空間で k-means を回し、各クラスタについて代表色（sRGB）、
//! 重心位置（正規化座標）、占有比を返す。後続の orb 配置（#3 以降）に
//! 渡せるデータ構造として `Cluster` を提供する。

use image::RgbImage;
use kmeans_colors::{get_kmeans_hamerly, Kmeans};
use palette::cast::from_component_slice;
use palette::{FromColor, IntoColor, Lab, Srgb};

/// 1 個の色クラスタ。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cluster {
    /// 代表色（sRGB の 0-255）
    pub color: [u8; 3],
    /// クラスタ重心の正規化座標 (x, y) ∈ [0, 1]^2
    pub centroid: (f32, f32),
    /// 全ピクセルに対する占有比 [0, 1]
    pub weight: f32,
}

/// `extract_clusters` のエラー。
#[derive(Debug)]
pub enum ClusterError {
    /// 入力画像が 0 ピクセル。
    EmptyImage,
    /// k に 0 が指定された。
    KZero,
}

/// k-means の内部パラメータ。
const KMEANS_RUNS: usize = 3;
const KMEANS_MAX_ITER: usize = 20;
const KMEANS_CONVERGE: f32 = 5.0;
const KMEANS_SEED: u64 = 0;

/// 画像から最大 k 個の代表色クラスタを抽出する。
///
/// k > ピクセル数の場合は実際のクラスタ数が k 未満になることがある。
/// 戻り値は `weight` 降順にソート済み。
pub fn extract_clusters(img: &RgbImage, k: usize) -> Result<Vec<Cluster>, ClusterError> {
    if k == 0 {
        return Err(ClusterError::KZero);
    }
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Err(ClusterError::EmptyImage);
    }

    // ピクセルバッファを Lab に変換。
    let raw: &[u8] = img.as_raw();
    let lab: Vec<Lab> = from_component_slice::<Srgb<u8>>(raw)
        .iter()
        .map(|x| x.into_linear::<f32>().into_color())
        .collect();

    // ピクセル数より大きい k を要求された場合は実際のサンプル数に丸める。
    let effective_k = k.min(lab.len());

    // 複数 run のうちベストスコアを採用（k-means++ 初期化のばらつき対策）。
    let mut best = Kmeans::new();
    for i in 0..KMEANS_RUNS {
        let run = get_kmeans_hamerly(
            effective_k,
            KMEANS_MAX_ITER,
            KMEANS_CONVERGE,
            false,
            &lab,
            KMEANS_SEED + i as u64,
        );
        if run.score < best.score {
            best = run;
        }
    }

    // 各クラスタごとにピクセル数と (x, y) の総和を集計。
    let cluster_count = best.centroids.len();
    let mut counts = vec![0u64; cluster_count];
    let mut sum_x = vec![0f64; cluster_count];
    let mut sum_y = vec![0f64; cluster_count];
    let total_pixels = best.indices.len();

    for (i, &idx) in best.indices.iter().enumerate() {
        let idx = idx as usize;
        // 想定外の index（k-means 内部で空クラスタになった場合等）はスキップ。
        if idx >= cluster_count {
            continue;
        }
        let x = (i as u32) % width;
        let y = (i as u32) / width;
        counts[idx] += 1;
        sum_x[idx] += x as f64;
        sum_y[idx] += y as f64;
    }

    // 正規化用の分母。1px 画像の divide-by-zero 回避のため max(1)。
    let denom_x = (width.saturating_sub(1)).max(1) as f64;
    let denom_y = (height.saturating_sub(1)).max(1) as f64;

    let mut clusters: Vec<Cluster> = best
        .centroids
        .iter()
        .enumerate()
        .filter_map(|(idx, lab_centroid)| {
            let count = counts[idx];
            if count == 0 {
                return None;
            }
            let mean_x = sum_x[idx] / count as f64;
            let mean_y = sum_y[idx] / count as f64;
            let cx = (mean_x / denom_x).clamp(0.0, 1.0) as f32;
            let cy = (mean_y / denom_y).clamp(0.0, 1.0) as f32;

            // LAB centroid を sRGB(u8) に戻す。
            let srgb: Srgb = Srgb::from_linear(Srgb::from_color(*lab_centroid).into_linear());
            let rgb_u8: Srgb<u8> = srgb.into_format();

            Some(Cluster {
                color: [rgb_u8.red, rgb_u8.green, rgb_u8.blue],
                centroid: (cx, cy),
                weight: (count as f64 / total_pixels as f64) as f32,
            })
        })
        .collect();

    // weight 降順。
    clusters.sort_by(|a, b| b.weight.total_cmp(&a.weight));

    Ok(clusters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn approx(a: f32, b: f32, eps: f32, label: &str) {
        assert!(
            (a - b).abs() < eps,
            "{}: expected ~{}, got {} (eps={})",
            label,
            b,
            a,
            eps
        );
    }

    #[test]
    fn single_color_k1() {
        let img: RgbImage = ImageBuffer::from_fn(256, 256, |_, _| Rgb([255u8, 0, 0]));
        let clusters = extract_clusters(&img, 1).expect("clusters");
        assert_eq!(clusters.len(), 1);
        let c = clusters[0];
        // 色: 真っ赤に近い（LAB 経由なので多少ズレる可能性に備えて ±10）。
        approx(c.color[0] as f32, 255.0, 10.0, "color.r");
        approx(c.color[1] as f32, 0.0, 10.0, "color.g");
        approx(c.color[2] as f32, 0.0, 10.0, "color.b");
        // 重心: 中心。
        approx(c.centroid.0, 0.5, 0.05, "centroid.x");
        approx(c.centroid.1, 0.5, 0.05, "centroid.y");
        // 占有比: 100%。
        approx(c.weight, 1.0, 1e-4, "weight");
    }

    #[test]
    fn top_red_bottom_blue_k2() {
        let img: RgbImage = ImageBuffer::from_fn(100, 100, |_, y| {
            if y < 50 {
                Rgb([255u8, 0, 0])
            } else {
                Rgb([0u8, 0, 255])
            }
        });
        let clusters = extract_clusters(&img, 2).expect("clusters");
        assert_eq!(clusters.len(), 2);

        // 一方が赤、もう一方が青のはず。色で振り分け。
        let red = clusters
            .iter()
            .find(|c| c.color[0] > c.color[2])
            .expect("red cluster");
        let blue = clusters
            .iter()
            .find(|c| c.color[2] > c.color[0])
            .expect("blue cluster");

        // 色が ±15 で赤・青に近い。
        approx(red.color[0] as f32, 255.0, 15.0, "red.color.r");
        approx(red.color[2] as f32, 0.0, 15.0, "red.color.b");
        approx(blue.color[0] as f32, 0.0, 15.0, "blue.color.r");
        approx(blue.color[2] as f32, 255.0, 15.0, "blue.color.b");

        // 上半分の赤の y 重心 ≈ 0.25（0..49 の平均は 24.5、99 で割って ≈ 0.247）。
        approx(red.centroid.1, 0.247, 0.05, "red.centroid.y");
        // 下半分の青の y 重心 ≈ 0.75（50..99 の平均は 74.5、99 で割って ≈ 0.752）。
        approx(blue.centroid.1, 0.752, 0.05, "blue.centroid.y");

        // 占有比は両方 ≈ 0.5。
        approx(red.weight, 0.5, 0.02, "red.weight");
        approx(blue.weight, 0.5, 0.02, "blue.weight");
    }

    #[test]
    fn empty_image_returns_error() {
        let img: RgbImage = ImageBuffer::new(0, 0);
        match extract_clusters(&img, 3) {
            Err(ClusterError::EmptyImage) => {}
            other => panic!("expected EmptyImage, got {:?}", other),
        }
    }

    #[test]
    fn k_zero_returns_error() {
        let img: RgbImage = ImageBuffer::from_fn(8, 8, |_, _| Rgb([10u8, 20, 30]));
        match extract_clusters(&img, 0) {
            Err(ClusterError::KZero) => {}
            other => panic!("expected KZero, got {:?}", other),
        }
    }
}
