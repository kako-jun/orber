//! 入力画像から代表色クラスタを抽出するモジュール。
//!
//! LAB 色空間で k-means を回し、各クラスタについて代表色（sRGB）、
//! 重心位置（正規化座標）、占有比を返す。後続の orb 配置（#3 以降）に
//! 渡せるデータ構造として `Cluster` を提供する。
//!
//! # 設計メモ
//!
//! - 色空間は LAB 固定。RGB 切り替えオプションは現時点では用意しない
//!   （知覚的に近い色をまとめたいので LAB が妥当、という方針）。
//! - `k > 実色数` の場合は実 k に縮約され、結果クラスタ数は要求未満になる。
//!   呼び出し側で警告を出すかどうかは #3 以降で判断する（このモジュールでは
//!   ただ縮約して返す）。

use image::RgbImage;
use kmeans_colors::{get_kmeans_hamerly, Kmeans};
use palette::cast::from_component_slice;
use palette::{FromColor, IntoColor, Lab, Srgb};

/// クラスタ重心の正規化座標 ∈ [0, 1]^2。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Centroid {
    pub x: f32,
    pub y: f32,
}

/// 1 個の色クラスタ。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cluster {
    /// 代表色（sRGB の 0-255）
    pub color: [u8; 3],
    /// クラスタ重心の正規化座標
    pub centroid: Centroid,
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

impl std::fmt::Display for ClusterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyImage => write!(f, "input image is empty (0x0)"),
            Self::KZero => write!(f, "k must be at least 1"),
        }
    }
}

impl std::error::Error for ClusterError {}

/// k-means の内部パラメータ。
const KMEANS_RUNS: usize = 3;
const KMEANS_MAX_ITER: usize = 20;
/// 収束判定の閾値（LAB ΔE のおおよそのスケール）。
const KMEANS_CONVERGE: f32 = 5.0;
const KMEANS_SEED: u64 = 0;

/// kmeans 入力としてのターゲット長辺 (px)。これより大きい画像は等比縮小される。
///
/// kmeans の支配色判定はサンプリング系処理で、長辺 256 程度のサンプルでも
/// 視覚的に同等の結果が得られる（ImageMagick 等の palette tool の経験則）。
/// フル解像度（数千 px）で kmeans を回すと iteration コストが線形に増えるが
/// 出力品質は変わらないため、安全な下限まで縮小する。
///
/// CLI: 4000px 級の写真で kmeans が数百ms → 10ms オーダーに短縮。
/// wasm GUI 経路: JS 側で既に 256 まで縮小しているのでこの再縮小は no-op。
const KMEANS_TARGET_LONG_EDGE: u32 = 256;
/// 極端アスペクト（10000×100 のパノラマ等）で短辺が 1-3 px に潰れて kmeans
/// サンプル枯渇するのを防ぐ下限。K=5..8 程度なら 8 px 平方で十分なサンプル。
///
/// 注: アスペクト保持より枯渇回避を優先する。10000×100 → 256×3 だと kmeans
/// サンプル数 768 px で K=8 に対しても十分だが、`max(8)` で 256×8 にクランプ
/// するため厳密にはアスペクトが歪む。極端パノラマ専用の安全弁で実害なし。
const KMEANS_MIN_EDGE: u32 = 8;

/// kmeans のために画像を縮小する（必要なときだけ）。
///
/// 既に長辺が `KMEANS_TARGET_LONG_EDGE` 以下なら借用したまま返す（コピーなし）。
/// それ以上のサイズなら Triangle filter で等比縮小する。Triangle はバイリニア
/// 相当で、kmeans の支配色判定には十分かつ高速。
fn downsample_for_kmeans(img: &RgbImage) -> std::borrow::Cow<'_, RgbImage> {
    use std::borrow::Cow;
    let (w, h) = img.dimensions();
    let longest = w.max(h);
    if longest <= KMEANS_TARGET_LONG_EDGE {
        return Cow::Borrowed(img);
    }
    let scale = KMEANS_TARGET_LONG_EDGE as f32 / longest as f32;
    let dw = ((w as f32 * scale).round() as u32).max(KMEANS_MIN_EDGE);
    let dh = ((h as f32 * scale).round() as u32).max(KMEANS_MIN_EDGE);
    let resized = image::imageops::resize(img, dw, dh, image::imageops::FilterType::Triangle);
    Cow::Owned(resized)
}

/// 画像から最大 k 個の代表色クラスタを抽出する。
///
/// k > ピクセル数の場合は実際のクラスタ数が k 未満になることがある。
/// 戻り値は `weight` 降順にソート済み。
///
/// 入力画像が大きい場合は内部で長辺 `KMEANS_TARGET_LONG_EDGE` まで縮小して
/// から kmeans を実行する。centroid は正規化座標 [0,1] で返るためスケールに
/// 依存せず、color / weight も比率なので解像度非依存。
pub fn extract_clusters(img: &RgbImage, k: usize) -> Result<Vec<Cluster>, ClusterError> {
    if k == 0 {
        return Err(ClusterError::KZero);
    }
    let (orig_w, orig_h) = img.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return Err(ClusterError::EmptyImage);
    }

    // 大きい画像は kmeans 用にダウンサンプル。CLI で 4000px 級写真の処理時間が
    // 大幅短縮される。wasm GUI 経路は JS 側で既に 256 化しているので no-op。
    let downsampled = downsample_for_kmeans(img);
    let img = downsampled.as_ref();
    let (width, height) = img.dimensions();

    // ピクセルバッファを Lab に変換。
    let raw: &[u8] = img.as_raw();
    let lab: Vec<Lab> = from_component_slice::<Srgb<u8>>(raw)
        .iter()
        .map(|x| x.into_linear::<f32>().into_color())
        .collect();

    // ピクセル数より大きい k を要求された場合は実際のサンプル数に丸める。
    let effective_k = k.min(lab.len());

    // 複数 run のうちベストスコアを採用（k-means++ 初期化のばらつき対策）。
    // Kmeans::new() は score=f32::MAX で初期化されるため、1 回目の run は必ず best として採用される。
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
            // palette 0.7 の `Srgb` は non-linear (gamma-encoded) sRGB の型エイリアスなので、
            // `from_color` で既に gamma-encoded が得られている。
            let srgb: Srgb = Srgb::from_color(*lab_centroid);
            let rgb_u8: Srgb<u8> = srgb.into_format();

            Some(Cluster {
                color: [rgb_u8.red, rgb_u8.green, rgb_u8.blue],
                centroid: Centroid { x: cx, y: cy },
                weight: (count as f64 / total_pixels as f64) as f32,
            })
        })
        .collect();

    // weight 降順。
    clusters.sort_by(|a, b| b.weight.total_cmp(&a.weight));

    Ok(clusters)
}

/// クラスタ列から背景 RGBA を派生する。
///
/// 最大 weight のクラスタの色（sRGB 8bit）を取り、alpha=255 を付けて返す。
/// クラスタが空の場合は黒 `[0, 0, 0, 255]`。
///
/// # 設計メモ
///
/// 入力写真のドミナント色をそのまま背景にする。`extract_clusters` の戻り値は
/// weight 降順にソート済みなので「先頭」が最大 weight だが、この関数は順序に
/// 依存せず `max_by` で明示的に最大を取り直す（呼び出し側がソート済みを
/// 保証しなくても動くようにするため）。
pub fn derive_background_rgba(clusters: &[Cluster]) -> [u8; 4] {
    clusters
        .iter()
        .max_by(|a, b| a.weight.total_cmp(&b.weight))
        .map(|c| {
            let [r, g, b] = c.color;
            [r, g, b, 255]
        })
        .unwrap_or([0, 0, 0, 255])
}

/// 最大 weight のクラスタを 1 個だけ取り除いた新しい Vec を返す。
///
/// orb プールの色バリエーションから「背景に使うドミナント色」を抜くために使う。
/// 例えば夜景 (黒が支配的) なら、黒を除いた残りの色が orb として浮かぶ。
///
/// 元の順序は保たれる（`extract_clusters` は weight 降順で返すので、結果は
/// weight 降順から先頭 1 個を抜いた列になる）。クラスタが空の場合は空 Vec。
/// 同 weight が複数ある場合は `max_by` の規定に従い最後尾を最大として落とすが、
/// 後段の orb 配置は重み比例の重み付き抽選なので実害なし。
pub fn drop_dominant(clusters: &[Cluster]) -> Vec<Cluster> {
    let dominant_idx = clusters
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.weight.total_cmp(&b.weight))
        .map(|(i, _)| i);
    clusters
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != dominant_idx)
        .map(|(_, c)| *c)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn approx(a: f32, b: f32, eps: f32, label: &str) {
        assert!(
            (a - b).abs() < eps,
            "{label}: expected ~{b}, got {a} (eps={eps})"
        );
    }

    #[test]
    fn single_color_k1() {
        let img: RgbImage = ImageBuffer::from_fn(256, 256, |_, _| Rgb([255u8, 0, 0]));
        let clusters = extract_clusters(&img, 1).expect("clusters");
        assert_eq!(clusters.len(), 1);
        let c = clusters[0];
        // 色: 真っ赤に近い（LAB 往復の丸め誤差のみなので ±2 で締める）。
        approx(c.color[0] as f32, 255.0, 2.0, "color.r");
        approx(c.color[1] as f32, 0.0, 2.0, "color.g");
        approx(c.color[2] as f32, 0.0, 2.0, "color.b");
        // 重心: 中心。
        approx(c.centroid.x, 0.5, 0.05, "centroid.x");
        approx(c.centroid.y, 0.5, 0.05, "centroid.y");
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

        // 色が ±2 で赤・青に近い（LAB 往復の丸め誤差のみ）。
        approx(red.color[0] as f32, 255.0, 2.0, "red.color.r");
        approx(red.color[2] as f32, 0.0, 2.0, "red.color.b");
        approx(blue.color[0] as f32, 0.0, 2.0, "blue.color.r");
        approx(blue.color[2] as f32, 255.0, 2.0, "blue.color.b");

        // x 軸はどちらも画像中央付近に収束する（上半分赤・下半分青なので左右対称）。
        approx(red.centroid.x, 0.5, 0.05, "red.centroid.x");
        approx(blue.centroid.x, 0.5, 0.05, "blue.centroid.x");

        // 上半分の赤の y 重心 ≈ 0.25（0..49 の平均は 24.5、99 で割って ≈ 0.247）。
        approx(red.centroid.y, 0.247, 0.05, "red.centroid.y");
        // 下半分の青の y 重心 ≈ 0.75（50..99 の平均は 74.5、99 で割って ≈ 0.752）。
        approx(blue.centroid.y, 0.752, 0.05, "blue.centroid.y");

        // 占有比は両方 ≈ 0.5。
        approx(red.weight, 0.5, 0.02, "red.weight");
        approx(blue.weight, 0.5, 0.02, "blue.weight");
    }

    #[test]
    fn single_pixel_image_k1() {
        // 1x1 画像は denom = (1-1).max(1) = 1、x=y=0 なので centroid = (0.0, 0.0)。
        // divide-by-zero せず 1 クラスタが返ることを確認。
        let img: RgbImage = ImageBuffer::from_fn(1, 1, |_, _| Rgb([128u8, 64, 200]));
        let clusters = extract_clusters(&img, 1).expect("clusters");
        assert_eq!(clusters.len(), 1);
        let c = clusters[0];
        // 1px しかないので centroid は (0.0, 0.0)。
        approx(c.centroid.x, 0.0, 1e-6, "centroid.x");
        approx(c.centroid.y, 0.0, 1e-6, "centroid.y");
        // 占有比は 100%。
        approx(c.weight, 1.0, 1e-4, "weight");
        // 色は概ね入力に近い（LAB 往復の丸めのみ）。
        approx(c.color[0] as f32, 128.0, 2.0, "color.r");
        approx(c.color[1] as f32, 64.0, 2.0, "color.g");
        approx(c.color[2] as f32, 200.0, 2.0, "color.b");
    }

    #[test]
    fn empty_image_returns_error() {
        let img: RgbImage = ImageBuffer::new(0, 0);
        match extract_clusters(&img, 3) {
            Err(ClusterError::EmptyImage) => {}
            other => panic!("expected EmptyImage, got {other:?}"),
        }
    }

    #[test]
    fn k_zero_returns_error() {
        let img: RgbImage = ImageBuffer::from_fn(8, 8, |_, _| Rgb([10u8, 20, 30]));
        match extract_clusters(&img, 0) {
            Err(ClusterError::KZero) => {}
            other => panic!("expected KZero, got {other:?}"),
        }
    }

    #[test]
    fn derive_background_picks_max_weight_cluster() {
        // 最大 weight のクラスタの色が alpha=255 で返ることを確認。
        let clusters = vec![
            Cluster {
                color: [10, 20, 30],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.2,
            },
            Cluster {
                color: [200, 100, 50],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.7,
            },
            Cluster {
                color: [0, 0, 0],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.1,
            },
        ];
        assert_eq!(derive_background_rgba(&clusters), [200, 100, 50, 255]);
    }

    #[test]
    fn derive_background_unsorted_input() {
        // weight 降順でなくても max_by で最大を引けることを確認。
        let clusters = vec![
            Cluster {
                color: [200, 100, 50],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.7,
            },
            Cluster {
                color: [10, 20, 30],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.2,
            },
        ];
        assert_eq!(derive_background_rgba(&clusters), [200, 100, 50, 255]);
    }

    #[test]
    fn derive_background_unsorted_three_clusters() {
        // 3 要素 unsorted で、中央に位置する要素が最大 weight のときも
        // 正しく拾えること（sorted 前提に依存していないことの追加担保）。
        let clusters = vec![
            Cluster {
                color: [10, 10, 10],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.2,
            },
            Cluster {
                color: [20, 20, 20],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.5, // dominant
            },
            Cluster {
                color: [30, 30, 30],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.3,
            },
        ];
        assert_eq!(derive_background_rgba(&clusters), [20, 20, 20, 255]);
    }

    #[test]
    fn derive_background_empty_clusters_yields_black() {
        let clusters: Vec<Cluster> = vec![];
        assert_eq!(derive_background_rgba(&clusters), [0, 0, 0, 255]);
    }

    #[test]
    fn drop_dominant_removes_max_weight() {
        // 最大 weight のクラスタが除外され、残り 2 個になることを確認。
        let clusters = vec![
            Cluster {
                color: [10, 20, 30],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.2,
            },
            Cluster {
                color: [200, 100, 50],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.7,
            },
            Cluster {
                color: [0, 0, 0],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.1,
            },
        ];
        let rest = drop_dominant(&clusters);
        assert_eq!(rest.len(), 2);
        assert!(rest.iter().all(|c| c.color != [200, 100, 50]));
        // 順序保存（先頭 weight=0.2、次 weight=0.1）。
        assert_eq!(rest[0].color, [10, 20, 30]);
        assert_eq!(rest[1].color, [0, 0, 0]);
    }

    #[test]
    fn drop_dominant_on_sorted_input_returns_tail() {
        // extract_clusters 由来の weight 降順入力では先頭が落ちて tail が返る。
        let clusters = vec![
            Cluster {
                color: [200, 100, 50],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.6,
            },
            Cluster {
                color: [10, 20, 30],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.3,
            },
            Cluster {
                color: [0, 0, 0],
                centroid: Centroid { x: 0.5, y: 0.5 },
                weight: 0.1,
            },
        ];
        let rest = drop_dominant(&clusters);
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].color, [10, 20, 30]);
        assert_eq!(rest[1].color, [0, 0, 0]);
    }

    #[test]
    fn drop_dominant_empty_input() {
        let clusters: Vec<Cluster> = vec![];
        assert!(drop_dominant(&clusters).is_empty());
    }

    #[test]
    fn drop_dominant_single_cluster_yields_empty() {
        let clusters = vec![Cluster {
            color: [200, 100, 50],
            centroid: Centroid { x: 0.5, y: 0.5 },
            weight: 1.0,
        }];
        assert!(drop_dominant(&clusters).is_empty());
    }

    /// 大きい画像の自動ダウンサンプル（PR #119）の回帰テスト。
    ///
    /// フル解像度と pre-shrunk の入力で kmeans 結果がほぼ一致することを確認する。
    /// 厳密一致は kmeans のシード位置が変わるため期待できないが、weight 序列・
    /// color の差・centroid の差が許容範囲に収まることをチェックする。
    /// これが壊れたら kako-jun 視点で「視覚パリティが崩れた」を検知できる。
    #[test]
    fn downsample_matches_pre_shrunk_visually() {
        // 縦に 4 色の縞模様を持つ大きい画像を作る (1600×1600)。各色が 25% 占有。
        let big: RgbImage = ImageBuffer::from_fn(1600, 1600, |_, y| match y / 400 {
            0 => Rgb([200u8, 50, 50]),  // 赤系
            1 => Rgb([50u8, 200, 50]),  // 緑系
            2 => Rgb([50u8, 50, 200]),  // 青系
            _ => Rgb([200u8, 200, 50]), // 黄系
        });
        // 同じ画像を pre-shrunk (long edge 256) で作る → ダウンサンプル経路と
        // pre-shrunk が一致すれば、core 内ダウンサンプルが正しく動いている。
        let small: RgbImage = ImageBuffer::from_fn(256, 256, |_, y| match y / 64 {
            0 => Rgb([200u8, 50, 50]),
            1 => Rgb([50u8, 200, 50]),
            2 => Rgb([50u8, 50, 200]),
            _ => Rgb([200u8, 200, 50]),
        });

        let clusters_big = extract_clusters(&big, 4).expect("big clusters");
        let clusters_small = extract_clusters(&small, 4).expect("small clusters");

        assert_eq!(clusters_big.len(), 4, "big should yield 4 clusters");
        assert_eq!(clusters_small.len(), 4, "small should yield 4 clusters");

        // weight 序列はだいたい 0.25 ずつ。順序は kmeans 初期化次第なので
        // ソート済み (weight 降順) の対応する index 同士で比較する。
        for i in 0..4 {
            approx(
                clusters_big[i].weight,
                clusters_small[i].weight,
                0.05,
                &format!("weight[{i}]"),
            );
            // color は LAB centroid → sRGB 往復で ±数 LSB 誤差はある。
            // ダウンサンプルでさらに数 LSB 増えるが許容内（±15）。
            for ch in 0..3 {
                let diff =
                    (clusters_big[i].color[ch] as i32 - clusters_small[i].color[ch] as i32).abs();
                assert!(
                    diff <= 15,
                    "color[{i}][{ch}] diff {diff} too large (big={} small={})",
                    clusters_big[i].color[ch],
                    clusters_small[i].color[ch],
                );
            }
        }
    }

    /// 小さい画像 (≤256) では downsample が no-op で借用パスを通ることを確認する。
    /// 出力が「フル解像度を直接 kmeans した場合」と完全一致するかは保証しない
    /// （kmeans は元々シード位置依存で run-to-run 微変動するため）が、結果が
    /// 妥当なクラスタ列であることをチェック。
    #[test]
    fn small_image_passes_through_unchanged() {
        let img: RgbImage = ImageBuffer::from_fn(100, 100, |_, _| Rgb([128u8, 64, 32]));
        let clusters = extract_clusters(&img, 1).expect("clusters");
        assert_eq!(clusters.len(), 1);
        approx(clusters[0].weight, 1.0, 1e-3, "single cluster weight");
    }
}
