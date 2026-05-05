//! 動画入力（#7）用の色トラック補間モジュール。
//!
//! 動画から N サンプルのフレームを抜き取り、各サンプルごとに k クラスタの色を
//! 抽出すると、cluster あたり N 個の色サンプル列（= 色トラック）ができる。
//! このモジュールは「正規化時刻 `t ∈ [0, 1]` を受けて 1 本の色トラックから
//! RGB を線形補間する関数」を提供する。
//!
//! トラックそのものを作る側（=フレームサンプリング + LAB マッチング）は
//! 入力 I/O に依存するので CLI 側 (`crates/cli/src/video_input.rs`) に置く。
//! ここではあくまで補間ロジックだけを純粋関数として持ち、ユニットテストの
//! ピン止め先にする（Issue #7 完了条件「補間関数を単独でカバー」）。

/// 1 本の色トラック上で `t ∈ [0, 1]` の RGB を線形補間する。
///
/// - `track.len() == 0`: 黒 `[0, 0, 0]` を返す。空トラックは「色情報が無い」
///   ことを意味するので、呼び出し側で cluster.color へフォールバックさせる
///   想定（呼び出し側が track を `Some` で渡してきている時点で空 track には
///   ならない設計だが、防衛のためにこの値を定義する）。
/// - `track.len() == 1`: 全 t で同じ色（端点 clamp の自明退化）。
/// - `t <= 0.0`: `track[0]` にクランプ。
/// - `t >= 1.0`: `track[track.len() - 1]` にクランプ。
/// - それ以外: 隣接 2 サンプルの線形補間（チャンネルごとに u8 でラウンド）。
///
/// `t` が NaN の場合は安全側で `track[0]` を返す（NaN が入る経路は想定外だが
/// panic させない）。
///
/// # 設計メモ
///
/// - 補間は sRGB 空間で線形に行う。LAB に持っていって補間するべきだが、
///   v0 は実装の簡潔さを優先する。後で knee が出たら LAB 補間に差し替える。
/// - 端点 clamp（wrap でない）にしているのは、入力動画の冒頭と末尾で色が
///   不連続な場合に「t=1 直前から t=0 直後にかけて急激に色が飛ぶ」のを避ける
///   ため。orb の wrap ループとは別軸。
pub fn interpolate_color_track(track: &[[u8; 3]], t: f32) -> [u8; 3] {
    if track.is_empty() {
        return [0, 0, 0];
    }
    if track.len() == 1 {
        return track[0];
    }
    // NaN 防衛。is_nan() は f32 の組み込みなので no_std でも使える。
    if t.is_nan() {
        return track[0];
    }
    if t <= 0.0 {
        return track[0];
    }
    if t >= 1.0 {
        return track[track.len() - 1];
    }
    // 区間 [i, i+1] 上に t をマッピング: scaled = t * (N-1)。
    let n = track.len();
    let scaled = t * (n - 1) as f32;
    let i = scaled.floor() as usize;
    // 境界保険: floor が n-1 を返すケース（t がほぼ 1.0 だが t < 1.0）。
    if i >= n - 1 {
        return track[n - 1];
    }
    let frac = scaled - i as f32;
    let a = track[i];
    let b = track[i + 1];
    let lerp_u8 = |x: u8, y: u8| -> u8 {
        let v = x as f32 + (y as f32 - x as f32) * frac;
        v.round().clamp(0.0, 255.0) as u8
    };
    [
        lerp_u8(a[0], b[0]),
        lerp_u8(a[1], b[1]),
        lerp_u8(a[2], b[2]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_color_track_endpoints_clamp() {
        // 端点クランプ: t=0 で先頭、t=1 で末尾。
        let track = vec![[10, 20, 30], [40, 50, 60], [70, 80, 90]];
        assert_eq!(interpolate_color_track(&track, 0.0), [10, 20, 30]);
        assert_eq!(interpolate_color_track(&track, 1.0), [70, 80, 90]);
        // 範囲外も端点。
        assert_eq!(interpolate_color_track(&track, -0.5), [10, 20, 30]);
        assert_eq!(interpolate_color_track(&track, 1.5), [70, 80, 90]);
    }

    #[test]
    fn interpolate_color_track_midpoint() {
        // t=0.5 で中央サンプルが返る（3 サンプルなら正確に track[1]）。
        let track = vec![[0, 0, 0], [100, 150, 200], [255, 255, 255]];
        assert_eq!(interpolate_color_track(&track, 0.5), [100, 150, 200]);
    }

    #[test]
    fn interpolate_color_track_quarter_two_samples() {
        // 2 サンプルだけのとき、t=0.25 で frac=0.25 の線形補間。
        // [0,0,0] と [200, 100, 50] の 0.25 補間 = [50, 25, 13] 前後。
        let track = vec![[0, 0, 0], [200, 100, 50]];
        let mid = interpolate_color_track(&track, 0.25);
        // 期待値: round(200*0.25)=50, round(100*0.25)=25, round(50*0.25)=13
        assert_eq!(mid, [50, 25, 13]);
    }

    #[test]
    fn interpolate_color_track_single_color() {
        // track 長 1 なら全 t で同じ色。
        let track = vec![[123, 45, 200]];
        for t in [0.0_f32, 0.1, 0.5, 0.9, 1.0] {
            assert_eq!(interpolate_color_track(&track, t), [123, 45, 200]);
        }
    }

    #[test]
    fn interpolate_color_track_empty_returns_default() {
        // 空 track は黒で defined。panic しない。
        let track: Vec<[u8; 3]> = vec![];
        assert_eq!(interpolate_color_track(&track, 0.0), [0, 0, 0]);
        assert_eq!(interpolate_color_track(&track, 0.5), [0, 0, 0]);
        assert_eq!(interpolate_color_track(&track, 1.0), [0, 0, 0]);
    }

    #[test]
    fn interpolate_color_track_nan_t_does_not_panic() {
        let track = vec![[10, 20, 30], [40, 50, 60]];
        // NaN は track[0] を返す（panic させない）。
        assert_eq!(interpolate_color_track(&track, f32::NAN), [10, 20, 30]);
    }

    #[test]
    fn interpolate_color_track_t_just_below_one() {
        // t = 0.9999... のときに index out of bounds にならない。
        let track = vec![[0, 0, 0], [100, 100, 100], [200, 200, 200]];
        let v = interpolate_color_track(&track, 0.99999);
        // 末尾サンプル付近。R チャネルは 199..=200 の範囲。
        assert!(v[0] >= 199, "expected near 200, got {}", v[0]);
    }

    #[test]
    fn interpolate_color_track_monotonic_red_gradient() {
        // 単調増加のトラックは補間結果も単調非減少。
        let track = vec![[0, 0, 0], [128, 128, 128], [255, 255, 255]];
        let mut prev_r = 0u8;
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let v = interpolate_color_track(&track, t);
            assert!(
                v[0] >= prev_r,
                "monotonicity broken at t={t}: prev={prev_r} got={}",
                v[0]
            );
            prev_r = v[0];
        }
    }
}
