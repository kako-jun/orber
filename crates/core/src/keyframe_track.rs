//! 動画入力（#33）用のキーフレーム補間モジュール。
//!
//! [`crate::color_track`] (#7) は「色だけを時間軸で補間する」 純粋関数を提供する
//! のに対し、こちらは「色 + 位置 + 重み」の 3 値をまとめて時間軸で補間する。
//! 入力動画から N キーフレームをサンプリングして cluster 抽出 → 各 cluster の
//! 時間軸キー列（[`KeyframeClusterPoint`] の Vec）を作る経路と、その列を `t ∈ [0, 1]`
//! で補間する関数を分離している。
//!
//! トラックそのものを作る側（=フレームサンプリング + LAB マッチング）は
//! 入力 I/O に依存するので CLI 側 (`crates/cli/src/video_input.rs`) に置く。
//! ここではあくまで補間ロジックだけを純粋関数として持ち、ユニットテストの
//! ピン止め先にする。

use crate::cluster::Centroid;

/// 1 個のキーフレームから抽出した「あるクラスタ」の color + position + weight。
///
/// `time` は [0, 1] でこのキーフレームが入力動画のどの正規化時刻に対応するか
/// （`VideoSample::t` と同じ意味）。`time` は track 内で昇順を仮定する。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyframeClusterPoint {
    /// 代表色（sRGB の 0-255）
    pub color: [u8; 3],
    /// クラスタ重心の正規化座標
    pub centroid: Centroid,
    /// 全ピクセルに対する占有比 [0, 1]
    pub weight: f32,
    /// このキーフレームの正規化時刻 [0, 1]
    pub time: f32,
}

/// `track` を `t ∈ [0, 1]` で補間して 1 つの (color, centroid, weight) を返す。
///
/// - `track.is_empty()`: デフォルト値（黒、中央、weight=0）。
/// - `track.len() == 1`: 全 t でその唯一の値。
/// - `t <= track[0].time`: 先頭値にクランプ。
/// - `t >= track[last].time`: 末尾値にクランプ。
/// - それ以外: `t` を含む隣接 2 キー (k0, k1) を線形に挟み込み、
///   `α = (t - k0.time) / (k1.time - k0.time)` で各成分を線形補間。
///
/// `t` が NaN の場合は安全側で先頭値を返す（panic させない）。
///
/// # 設計メモ
///
/// - 補間は sRGB / 正規化座標 / 重みの「素直な線形補間」。LAB 補間でないのは
///   #7 と同じ判断（ナイーブで実用品質、テストが書きやすい）。
/// - track 内の `time` は `VideoSample::t` 由来で必ず昇順だが、防衛として
///   隣接ペアの time が一致 / 逆転する場合は α=0 にフォールバックして
///   k0 をそのまま返す（divide-by-zero 回避）。
/// - `time` フィールドが厳密に `i / (N-1)` でなくてもよい設計（不均等時刻
///   サンプリングや、後段で hold-last したキーが入っても破綻しない）。
pub fn interpolate_keyframe_track(
    track: &[KeyframeClusterPoint],
    t: f32,
) -> ([u8; 3], Centroid, f32) {
    let default = ([0, 0, 0], Centroid { x: 0.5, y: 0.5 }, 0.0);
    if track.is_empty() {
        return default;
    }
    if track.len() == 1 {
        let p = track[0];
        return (p.color, p.centroid, p.weight);
    }
    // NaN 防衛。
    if t.is_nan() {
        let p = track[0];
        return (p.color, p.centroid, p.weight);
    }
    // 端点 clamp。
    if t <= track[0].time {
        let p = track[0];
        return (p.color, p.centroid, p.weight);
    }
    let last = track.len() - 1;
    if t >= track[last].time {
        let p = track[last];
        return (p.color, p.centroid, p.weight);
    }
    // 隣接 2 キーを線形探索（N が小さい前提なので二分探索しない）。
    // track[i].time <= t < track[i+1].time となる i を探す。
    let mut i = 0usize;
    for j in 0..last {
        if t >= track[j].time && t < track[j + 1].time {
            i = j;
            break;
        }
    }
    let k0 = track[i];
    let k1 = track[i + 1];
    let dt = k1.time - k0.time;
    let alpha = if dt > 0.0 {
        ((t - k0.time) / dt).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let lerp_u8 = |a: u8, b: u8| -> u8 {
        let v = a as f32 + (b as f32 - a as f32) * alpha;
        v.round().clamp(0.0, 255.0) as u8
    };
    let color = [
        lerp_u8(k0.color[0], k1.color[0]),
        lerp_u8(k0.color[1], k1.color[1]),
        lerp_u8(k0.color[2], k1.color[2]),
    ];
    let centroid = Centroid {
        x: k0.centroid.x + (k1.centroid.x - k0.centroid.x) * alpha,
        y: k0.centroid.y + (k1.centroid.y - k0.centroid.y) * alpha,
    };
    let weight = k0.weight + (k1.weight - k0.weight) * alpha;
    (color, centroid, weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kp(color: [u8; 3], cx: f32, cy: f32, weight: f32, time: f32) -> KeyframeClusterPoint {
        KeyframeClusterPoint {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight,
            time,
        }
    }

    fn approx(a: f32, b: f32, eps: f32, label: &str) {
        assert!(
            (a - b).abs() < eps,
            "{label}: expected ~{b}, got {a} (eps={eps})"
        );
    }

    #[test]
    fn interpolate_keyframe_track_endpoints_clamp() {
        // 端点クランプ: t<0 / t>1 は端点。
        let track = vec![
            kp([10, 20, 30], 0.1, 0.2, 0.3, 0.0),
            kp([200, 100, 50], 0.9, 0.8, 0.7, 1.0),
        ];
        let (c, cen, w) = interpolate_keyframe_track(&track, -0.5);
        assert_eq!(c, [10, 20, 30]);
        assert_eq!(cen, Centroid { x: 0.1, y: 0.2 });
        approx(w, 0.3, 1e-6, "weight clamp lo");

        let (c, cen, w) = interpolate_keyframe_track(&track, 1.5);
        assert_eq!(c, [200, 100, 50]);
        assert_eq!(cen, Centroid { x: 0.9, y: 0.8 });
        approx(w, 0.7, 1e-6, "weight clamp hi");
    }

    #[test]
    fn interpolate_keyframe_track_midpoint_two_keys() {
        // 2 キー、t=0.5 で中点。
        let track = vec![
            kp([0, 0, 0], 0.0, 0.0, 0.0, 0.0),
            kp([200, 100, 50], 1.0, 1.0, 1.0, 1.0),
        ];
        let (c, cen, w) = interpolate_keyframe_track(&track, 0.5);
        assert_eq!(c, [100, 50, 25]);
        approx(cen.x, 0.5, 1e-6, "centroid.x");
        approx(cen.y, 0.5, 1e-6, "centroid.y");
        approx(w, 0.5, 1e-6, "weight");
    }

    #[test]
    fn interpolate_keyframe_track_three_keys_finds_correct_segment() {
        // 3 キー、t=0.25 は key0(0.0)–key1(0.5) 区間の真ん中。
        let track = vec![
            kp([0, 0, 0], 0.0, 0.0, 0.0, 0.0),
            kp([200, 100, 50], 1.0, 1.0, 1.0, 0.5),
            kp([255, 255, 255], 0.0, 0.0, 0.5, 1.0),
        ];
        let (c, cen, w) = interpolate_keyframe_track(&track, 0.25);
        // alpha = (0.25 - 0.0) / (0.5 - 0.0) = 0.5
        assert_eq!(c, [100, 50, 25]);
        approx(cen.x, 0.5, 1e-6, "centroid.x mid of seg0");
        approx(cen.y, 0.5, 1e-6, "centroid.y mid of seg0");
        approx(w, 0.5, 1e-6, "weight mid of seg0");
    }

    #[test]
    fn interpolate_keyframe_track_single_key_constant() {
        // 1 キーなら全 t で同じ値。
        let track = vec![kp([123, 45, 200], 0.4, 0.6, 0.8, 0.0)];
        for t in [0.0_f32, 0.1, 0.5, 0.9, 1.0] {
            let (c, cen, w) = interpolate_keyframe_track(&track, t);
            assert_eq!(c, [123, 45, 200]);
            assert_eq!(cen, Centroid { x: 0.4, y: 0.6 });
            approx(w, 0.8, 1e-6, "single-key weight");
        }
    }

    #[test]
    fn interpolate_keyframe_track_empty_returns_default() {
        // 空 track はデフォルト値。
        let track: Vec<KeyframeClusterPoint> = vec![];
        for t in [0.0_f32, 0.5, 1.0] {
            let (c, cen, w) = interpolate_keyframe_track(&track, t);
            assert_eq!(c, [0, 0, 0]);
            assert_eq!(cen, Centroid { x: 0.5, y: 0.5 });
            approx(w, 0.0, 1e-6, "default weight");
        }
    }

    #[test]
    fn interpolate_keyframe_track_uneven_time_intervals() {
        // 不均等な time でも区間が正しく検出される。
        // key0 at 0.0, key1 at 0.2, key2 at 1.0
        // t=0.6 は key1–key2 区間 (幅 0.8) の (0.6 - 0.2) / 0.8 = 0.5 = 中点。
        let track = vec![
            kp([0, 0, 0], 0.0, 0.0, 0.0, 0.0),
            kp([100, 100, 100], 0.5, 0.5, 0.5, 0.2),
            kp([200, 200, 200], 1.0, 1.0, 1.0, 1.0),
        ];
        let (c, cen, w) = interpolate_keyframe_track(&track, 0.6);
        assert_eq!(c, [150, 150, 150]);
        approx(cen.x, 0.75, 1e-6, "centroid.x uneven mid");
        approx(cen.y, 0.75, 1e-6, "centroid.y uneven mid");
        approx(w, 0.75, 1e-6, "weight uneven mid");
    }

    #[test]
    fn interpolate_keyframe_track_color_position_weight_lerp() {
        // 色・位置・weight が独立に補間されることを確認する
        // （色だけ動いて位置は固定、なども成立する）。
        let track = vec![
            kp([0, 100, 200], 0.2, 0.8, 0.0, 0.0),
            kp([200, 100, 0], 0.8, 0.2, 1.0, 1.0),
        ];
        let (c, cen, w) = interpolate_keyframe_track(&track, 0.25);
        // 色: [50, 100, 150]
        assert_eq!(c[0], 50);
        assert_eq!(c[1], 100);
        assert_eq!(c[2], 150);
        // 位置: x: 0.2 + 0.6*0.25 = 0.35、y: 0.8 - 0.6*0.25 = 0.65
        approx(cen.x, 0.35, 1e-6, "centroid.x");
        approx(cen.y, 0.65, 1e-6, "centroid.y");
        // weight: 0.25
        approx(w, 0.25, 1e-6, "weight");
    }

    #[test]
    fn interpolate_keyframe_track_determinism() {
        // 同じ入力 + 同じ t は同じ結果。
        let track = vec![
            kp([10, 20, 30], 0.1, 0.2, 0.3, 0.0),
            kp([40, 50, 60], 0.4, 0.5, 0.6, 0.4),
            kp([70, 80, 90], 0.7, 0.8, 0.9, 1.0),
        ];
        let a = interpolate_keyframe_track(&track, 0.37);
        let b = interpolate_keyframe_track(&track, 0.37);
        assert_eq!(a, b);
    }

    #[test]
    fn interpolate_keyframe_track_nan_t_does_not_panic() {
        // NaN は先頭値（panic させない）。
        let track = vec![
            kp([10, 20, 30], 0.1, 0.2, 0.3, 0.0),
            kp([40, 50, 60], 0.4, 0.5, 0.6, 1.0),
        ];
        let (c, cen, w) = interpolate_keyframe_track(&track, f32::NAN);
        assert_eq!(c, [10, 20, 30]);
        assert_eq!(cen, Centroid { x: 0.1, y: 0.2 });
        approx(w, 0.3, 1e-6, "nan -> head weight");
    }

    #[test]
    fn interpolate_keyframe_track_zero_dt_segment_does_not_div_by_zero() {
        // 同 time の隣接ペアが入っても div-by-zero しない（α=0 で k0 を返す）。
        let track = vec![
            kp([0, 0, 0], 0.0, 0.0, 0.0, 0.0),
            kp([100, 100, 100], 0.5, 0.5, 0.5, 0.5),
            kp([200, 200, 200], 1.0, 1.0, 1.0, 0.5), // 同じ time
            kp([255, 255, 255], 1.0, 1.0, 1.0, 1.0),
        ];
        // t=0.5 は最初に見つかる region で track[1] を返す（端点 clamp ではない）。
        // 確実に panic しないことだけ保証する。
        let _ = interpolate_keyframe_track(&track, 0.5);
        let _ = interpolate_keyframe_track(&track, 0.7);
    }
}
