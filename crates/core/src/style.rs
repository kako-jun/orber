//! SVG / CSS の静的書き出しモジュール。
//!
//! ラスタを焼かず、ベクター（SVG）または CSS 背景グラデーションとして
//! orb 配置を出力する。動的化（@keyframes・SMIL アニメ等）は将来 Issue。
//!
//! # 設計メモ
//!
//! - viewBox は PNG/動画と揃えて 1080x1920。CSS は % 指定なので解像度を
//!   持たない（`StyleOptions` も解像度フィールドを持たない）
//! - 色は [`crate::orb::adjust_saturation`] を共有して使う。HSL 経路で
//!   彩度を変えるルールも PNG と一致
//! - SVG は `<radialGradient>` の 3 stop（0% / mid% / 100%）で減衰を表現。
//!   CSS も 3 stop の `radial-gradient(...)` を `--orber-bg` 変数に合成
//! - 文字列組み立てのみ。crate 依存は追加しない

use crate::cluster::Cluster;
use crate::orb::adjust_saturation;

/// SVG viewBox 幅。PNG / 動画と揃える。
pub(crate) const STYLE_WIDTH: u32 = 1080;
/// SVG viewBox 高さ。PNG / 動画と揃える。
pub(crate) const STYLE_HEIGHT: u32 = 1920;

/// orb の見た目の「ぼかし具合」を 1 軸 3 段階で制御する preset (#55)。
///
/// - `Low`: ぼかし弱め / 縁シャープ
/// - `Mid`: 既存の振る舞いと同じ（regression なし、デフォルト）
/// - `High`: ぼかし強め / 縁ソフト / alpha 控えめ
///
/// 内部効果:
/// - alpha 倍率: Low=1.0, Mid=1.0, High=0.55
/// - blur オフセット: Low=-0.25, Mid=0.0, High=+0.25
///
/// PNG (animate / render_static) と SVG / CSS の全経路で同じ意味で適用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum SoftnessPreset {
    /// ぼかし弱め。縁シャープ。
    Low,
    /// 既存の振る舞いと完全同値（デフォルト）。
    #[default]
    Mid,
    /// ぼかし強め。縁ソフトで alpha も控えめ。
    High,
}

impl SoftnessPreset {
    /// orb 中心の不透明度に掛ける倍率。Mid = 1.0（既存と同値）。
    pub fn alpha_mul(self) -> f32 {
        match self {
            SoftnessPreset::Low => 1.0,
            SoftnessPreset::Mid => 1.0,
            SoftnessPreset::High => 0.55,
        }
    }

    /// blur パラメータに足すオフセット。最終 blur は呼び出し側で `[0, 1]` に clamp する。
    /// Mid = 0.0（既存と同値）。Low は -0.25（よりシャープ）、High は +0.25（よりソフト）。
    pub fn blur_offset(self) -> f32 {
        match self {
            SoftnessPreset::Low => -0.25,
            SoftnessPreset::Mid => 0.0,
            SoftnessPreset::High => 0.25,
        }
    }
}

/// orb の減衰プロファイル。shape ではなく「alpha をどう落とすか」を表す。
///
/// `Rim` は中間 stop を 1 つ持つ輪郭強調型、`Soft` は中心保持の単純フェード型。
/// Circle / Glyph のどちらでも同じプロファイルを使えるよう、shape から分離して
/// style モジュールに置く。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FalloffProfile {
    Rim,
    Soft,
}

/// Rim プロファイルの中間 stop 位置。blur=0 で外寄り、blur=1 で中心寄り。
#[inline]
pub fn rim_mid_stop(blur: f32) -> f32 {
    (1.0 - blur.clamp(0.0, 1.0) * 0.8).clamp(0.05, 0.95)
}

/// Soft プロファイルの中心保持終端。blur=0 で外寄り、blur=1 で中心寄り。
#[inline]
pub fn soft_hold_stop(blur: f32) -> f32 {
    (1.0 - blur.clamp(0.0, 1.0)).clamp(0.05, 0.95)
}

/// `r`（0=中心/深部、1=edge、>1=外側）から alpha を返す共通 falloff。
///
/// WebGL Glyph SDF 経路は、glyph 形状から得た signed-distance をこの `r` に変換して
/// Circle と同じ減衰式へ流し込む。`opacity` は中心 alpha の倍率。
#[inline]
pub fn falloff_curve(profile: FalloffProfile, r: f32, blur: f32, opacity: f32) -> f32 {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return 0.0;
    }
    let r = r.max(0.0);
    if r >= 1.0 {
        return 0.0;
    }
    match profile {
        FalloffProfile::Rim => {
            let mid_a = opacity * (80.0 / 255.0);
            let mid_stop = rim_mid_stop(blur);
            if r <= mid_stop {
                let u = if mid_stop > 0.0 { r / mid_stop } else { 1.0 };
                opacity + (mid_a - opacity) * u
            } else {
                let denom = (1.0 - mid_stop).max(1e-6);
                let u = (r - mid_stop) / denom;
                mid_a * (1.0 - u)
            }
        }
        FalloffProfile::Soft => {
            let hold_stop = soft_hold_stop(blur);
            if r <= hold_stop {
                opacity
            } else {
                let denom = (1.0 - hold_stop).max(1e-6);
                let u = (r - hold_stop) / denom;
                opacity * (1.0 - u)
            }
        }
    }
}

/// SVG / CSS 描画オプション。
///
/// 解像度は SVG では viewBox 固定、CSS では % 指定なのでフィールドを持たない。
#[derive(Debug, Clone)]
pub struct StyleOptions {
    /// orb サイズ倍率（1.0 = デフォルト）
    pub orb_size: f32,
    /// ぼかし強度 0.0..=1.0
    pub blur: f32,
    /// 彩度倍率（1.0 = unchanged）
    pub saturation: f32,
    /// 背景 RGBA。alpha=0 で透過 SVG / `background-color: transparent`。
    pub background: [u8; 4],
    /// ぼかし preset（#55）。Mid で既存挙動と完全同値。
    pub softness: SoftnessPreset,
}

impl Default for StyleOptions {
    fn default() -> Self {
        Self {
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            background: [0, 0, 0, 255],
            softness: SoftnessPreset::Mid,
        }
    }
}

/// クラスタ列を SVG 文字列として描画する。
///
/// 出力は viewBox `0 0 1080 1920` の自己完結 SVG。背景は黒い `<rect>`、
/// 各 cluster は `<radialGradient>` と `<circle>` のペアになる。
pub fn render_svg(clusters: &[Cluster], opts: &StyleOptions) -> String {
    // softness offset を blur に積算してから clamp。Mid なら既存と完全同値。
    let blur = (opts.blur + opts.softness.blur_offset()).clamp(0.0, 1.0);
    let saturation = opts.saturation.max(0.0);
    let orb_size = opts.orb_size.max(0.0);
    let alpha_mul = opts.softness.alpha_mul().clamp(0.0, 1.0);

    let width = STYLE_WIDTH as f32;
    let height = STYLE_HEIGHT as f32;
    let base_radius_unit = width.min(height) * 0.25 * orb_size;

    // mid_offset: blur=0 で外寄り（中心の不透明領域が広い）、blur=1 で中心寄り。
    // PNG 側 (1.0 - blur*0.8) と意味的に整合させ、% 表記の中間 stop を作る。
    let mid_pct = (rim_mid_stop(blur) * 100.0).round() as i32;
    // softness 軸: alpha 全体に倍率を掛ける（0% は 1.0×alpha_mul、mid は 0.5×alpha_mul、外周 0）。
    let stop0_a = alpha_mul;
    let stop_mid_a = 0.5 * alpha_mul;

    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {STYLE_WIDTH} {STYLE_HEIGHT}\" width=\"{STYLE_WIDTH}\" height=\"{STYLE_HEIGHT}\">\n"
    ));
    let [bg_r, bg_g, bg_b, bg_a] = opts.background;
    if bg_a > 0 {
        if bg_a == 255 {
            s.push_str(&format!(
                "  <rect width=\"100%\" height=\"100%\" fill=\"rgb({bg_r},{bg_g},{bg_b})\"/>\n"
            ));
        } else {
            let opacity = bg_a as f32 / 255.0;
            s.push_str(&format!(
                "  <rect width=\"100%\" height=\"100%\" fill=\"rgb({bg_r},{bg_g},{bg_b})\" fill-opacity=\"{opacity:.3}\"/>\n"
            ));
        }
    }

    // 描画対象 cluster だけ事前に絞り込んでから defs と circle の両方に使う。
    // weight=0 の cluster で空の gradient ID が defs に残らないようにする。
    let visible: Vec<(usize, &Cluster, f32)> = clusters
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let w = c.weight.max(0.0);
            let r = base_radius_unit * w.sqrt();
            if r > 0.0 {
                Some((i, c, r))
            } else {
                None
            }
        })
        .collect();

    s.push_str("  <defs>\n");
    for (i, cluster, _) in &visible {
        let [r, g, b] = adjust_saturation(cluster.color, saturation);
        s.push_str(&format!(
            "    <radialGradient id=\"orb-{i}\" cx=\"50%\" cy=\"50%\" r=\"50%\">\n"
        ));
        s.push_str(&format!(
            "      <stop offset=\"0%\" stop-color=\"rgb({r},{g},{b})\" stop-opacity=\"{stop0_a:.3}\"/>\n"
        ));
        s.push_str(&format!(
            "      <stop offset=\"{mid_pct}%\" stop-color=\"rgb({r},{g},{b})\" stop-opacity=\"{stop_mid_a:.3}\"/>\n"
        ));
        s.push_str(&format!(
            "      <stop offset=\"100%\" stop-color=\"rgb({r},{g},{b})\" stop-opacity=\"0\"/>\n"
        ));
        s.push_str("    </radialGradient>\n");
    }
    s.push_str("  </defs>\n");

    for (i, cluster, radius) in &visible {
        let cx = (cluster.centroid.x.clamp(0.0, 1.0) * width).round() as i32;
        let cy = (cluster.centroid.y.clamp(0.0, 1.0) * height).round() as i32;
        let r_px = radius.round() as i32;
        s.push_str(&format!(
            "  <circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r_px}\" fill=\"url(#orb-{i})\"/>\n"
        ));
    }

    s.push_str("</svg>\n");
    s
}

/// クラスタ列を CSS スニペットとして書き出す。
///
/// `--orber-bg` カスタムプロパティに、各 cluster を 1 つの
/// `radial-gradient(...)` として合成した値を入れる。利用側は
/// `background-image: var(--orber-bg);` で参照する。
pub fn render_css(clusters: &[Cluster], opts: &StyleOptions) -> String {
    // softness offset を blur に積算してから clamp。Mid なら既存と完全同値。
    let blur = (opts.blur + opts.softness.blur_offset()).clamp(0.0, 1.0);
    let saturation = opts.saturation.max(0.0);
    let orb_size = opts.orb_size.max(0.0);
    let alpha_mul = opts.softness.alpha_mul().clamp(0.0, 1.0);

    // mid_factor: PNG/SVG と同じ意味で「中間 stop が gradient 終端からどの程度内側か」。
    // blur=0 → mid=end の 95% （中心の不透明領域が広い、急峻に縁が落ちる）
    // blur=1 → mid=end の 20% （中心が点に近く、緩やかに減衰）
    let mid_factor = rim_mid_stop(blur);

    let [bg_r, bg_g, bg_b, bg_a] = opts.background;
    let bg_css = if bg_a == 0 {
        "transparent".to_string()
    } else if bg_a == 255 {
        format!("rgb({bg_r}, {bg_g}, {bg_b})")
    } else {
        let opacity = bg_a as f32 / 255.0;
        format!("rgba({bg_r}, {bg_g}, {bg_b}, {opacity:.3})")
    };

    let mut s = String::new();
    s.push_str("/* orber-generated background.\n");
    s.push_str("   Apply to <body> or any block element:\n");
    s.push_str("       body {\n");
    s.push_str("           margin: 0;\n");
    s.push_str("           min-height: 100vh;\n");
    s.push_str("           background-color: var(--orber-bg-color);\n");
    s.push_str("           background-image: var(--orber-bg);\n");
    s.push_str("       }\n");
    s.push_str("   Generated as CSS variables; reference with var(--orber-bg) and var(--orber-bg-color). */\n");
    s.push_str(":root {\n");
    s.push_str(&format!("    --orber-bg-color: {bg_css};\n"));

    // 有効な gradient だけ集めてから書き出す（最後のカンマを抑制するため）。
    let mut gradients: Vec<String> = Vec::new();
    for cluster in clusters {
        let weight = cluster.weight.max(0.0);
        if weight <= 0.0 {
            continue;
        }
        let [r, g, b] = adjust_saturation(cluster.color, saturation);
        let x = (cluster.centroid.x.clamp(0.0, 1.0) * 100.0).round() as i32;
        let y = (cluster.centroid.y.clamp(0.0, 1.0) * 100.0).round() as i32;
        // PNG 側 radius = min(W,H) * 0.25 * orb_size * sqrt(weight)。
        // CSS は背景全体に対する % 指定なので、視覚的に近づくよう
        // sqrt(weight) * 30% * orb_size を採用。orb_size を大きくして 100% を超えると
        // PNG ならキャンバス外まではみ出すが、CSS は背景比なので 100% で頭打ちになる。
        let end_f = (weight.sqrt() * 30.0 * orb_size).clamp(2.0, 100.0);
        let end_pct = end_f.round() as i32;
        // mid_pct は end_pct の mid_factor 倍。round 後の衝突を最終ガードで防ぐ
        // （mid_pct < end_pct を構造的に担保する）。
        let mid_pct = (end_f * mid_factor).round() as i32;
        let mid_pct = mid_pct.clamp(0, end_pct - 1);
        // softness 軸: alpha 全体に倍率を掛ける。Mid なら 1.0/0.5/0.0 のまま（regression なし）。
        let stop0 = alpha_mul;
        let stop_mid = 0.5 * alpha_mul;
        gradients.push(format!(
            "radial-gradient(circle at {x}% {y}%, rgba({r},{g},{b},{stop0}) 0%, rgba({r},{g},{b},{stop_mid}) {mid_pct}%, rgba({r},{g},{b},0) {end_pct}%)"
        ));
    }

    if gradients.is_empty() {
        // 描画対象が無いときは `none` を明示する。`var(--orber-bg)` 参照側でも
        // background-image: none と同義になり、空値プロパティ ": ;" 経由の
        // IACVT フォールバックより意図が明確。
        s.push_str("    --orber-bg: none;\n");
    } else {
        s.push_str("    --orber-bg:\n");
        for (i, g) in gradients.iter().enumerate() {
            s.push_str("        ");
            s.push_str(g);
            if i + 1 < gradients.len() {
                s.push_str(",\n");
            } else {
                s.push_str(";\n");
            }
        }
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{Centroid, Cluster};

    fn cluster(color: [u8; 3], cx: f32, cy: f32, weight: f32) -> Cluster {
        Cluster {
            color,
            centroid: Centroid { x: cx, y: cy },
            weight,
        }
    }

    fn six_clusters() -> Vec<Cluster> {
        vec![
            cluster([200, 100, 50], 0.1, 0.1, 0.3),
            cluster([50, 200, 100], 0.5, 0.2, 0.2),
            cluster([100, 50, 200], 0.9, 0.3, 0.15),
            cluster([255, 255, 0], 0.2, 0.7, 0.15),
            cluster([0, 255, 255], 0.5, 0.8, 0.1),
            cluster([255, 0, 255], 0.8, 0.9, 0.1),
        ]
    }

    #[test]
    fn svg_contains_expected_elements() {
        let svg = render_svg(&six_clusters(), &StyleOptions::default());
        assert_eq!(svg.matches("<radialGradient").count(), 6);
        assert_eq!(svg.matches("<circle ").count(), 6);
        assert!(svg.contains("viewBox=\"0 0 1080 1920\""));
        assert!(svg.contains("<rect width=\"100%\" height=\"100%\" fill=\"rgb(0,0,0)\""));
        assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(svg.ends_with("</svg>\n"));
    }

    #[test]
    fn css_contains_expected_gradients() {
        let css = render_css(&six_clusters(), &StyleOptions::default());
        assert_eq!(css.matches("radial-gradient(").count(), 6);
        assert!(css.contains("--orber-bg:"));
        // 最後の gradient の後にカンマが無い（valid CSS）。
        // ";" の直前は ")" のはず。
        let semi = css.find(";\n}").expect("CSS must end with ;\\n}");
        let before_semi: char = css[..semi].chars().next_back().unwrap();
        assert_eq!(
            before_semi, ')',
            "last char before ';' must be ')', got {before_semi:?}"
        );
    }

    #[test]
    fn deterministic_svg() {
        let clusters = six_clusters();
        let opts = StyleOptions::default();
        let a = render_svg(&clusters, &opts);
        let b = render_svg(&clusters, &opts);
        assert_eq!(a, b);
    }

    #[test]
    fn deterministic_css() {
        let clusters = six_clusters();
        let opts = StyleOptions::default();
        let a = render_css(&clusters, &opts);
        let b = render_css(&clusters, &opts);
        assert_eq!(a, b);
    }

    #[test]
    fn falloff_curve_rim_matches_expected_stops() {
        let blur = 0.5;
        let opacity = 1.0;
        assert_eq!(falloff_curve(FalloffProfile::Rim, 0.0, blur, opacity), 1.0);
        assert_eq!(falloff_curve(FalloffProfile::Rim, 1.0, blur, opacity), 0.0);
        let mid = rim_mid_stop(blur);
        let alpha_mid = falloff_curve(FalloffProfile::Rim, mid, blur, opacity);
        assert!((alpha_mid - (80.0 / 255.0)).abs() < 1e-6);
    }

    #[test]
    fn falloff_curve_soft_holds_then_fades() {
        let blur = 0.25;
        let opacity = 0.8;
        let hold = soft_hold_stop(blur);
        assert!((falloff_curve(FalloffProfile::Soft, 0.0, blur, opacity) - opacity).abs() < 1e-6);
        assert!((falloff_curve(FalloffProfile::Soft, hold, blur, opacity) - opacity).abs() < 1e-6);
        assert_eq!(falloff_curve(FalloffProfile::Soft, 1.0, blur, opacity), 0.0);
    }

    #[test]
    fn saturation_zero_grays_colors_svg() {
        // saturation=0 のとき、stop-color="rgb(R,G,B)" の R==G==B な色が少なくとも 1 つ含まれる。
        let opts = StyleOptions {
            saturation: 0.0,
            ..StyleOptions::default()
        };
        let svg = render_svg(&six_clusters(), &opts);
        // rgb(R,G,B) を抽出して R==G==B が少なくとも 1 件あること。
        let mut found_gray = false;
        for line in svg.lines() {
            if let Some(start) = line.find("rgb(") {
                let rest = &line[start + 4..];
                if let Some(end) = rest.find(')') {
                    let nums = &rest[..end];
                    let parts: Vec<&str> = nums.split(',').collect();
                    if parts.len() == 3 {
                        let r: i32 = parts[0].trim().parse().unwrap();
                        let g: i32 = parts[1].trim().parse().unwrap();
                        let b: i32 = parts[2].trim().parse().unwrap();
                        if (r - g).abs() <= 1 && (g - b).abs() <= 1 && (r - b).abs() <= 1 {
                            found_gray = true;
                            break;
                        }
                    }
                }
            }
        }
        assert!(found_gray, "saturation=0 should produce a grayscale stop");
    }

    /// CSS の各 gradient から `0% / mid% / end%` の 3 stop を抽出する。
    /// 各 stop は `rgba(...,A) N%` 形式で書かれているので、`A` の値ごとに
    /// 後続の数値を拾う簡易パーサ。
    fn extract_css_stops(css: &str) -> Vec<(i32, i32, i32)> {
        fn read_pct_after(hay: &str, marker: &str, from: usize) -> Option<(i32, usize)> {
            let pos = hay[from..].find(marker)? + from;
            let after = pos + marker.len();
            // skip spaces, read digits until '%'
            let bytes = hay.as_bytes();
            let mut i = after;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == start || i >= bytes.len() || bytes[i] != b'%' {
                return None;
            }
            let n = hay[start..i].parse::<i32>().ok()?;
            Some((n, i + 1))
        }

        let mut out = Vec::new();
        let mut cursor = 0usize;
        while let Some(rel) = css[cursor..].find("radial-gradient(") {
            let base = cursor + rel + "radial-gradient(".len();
            let (s0, p1) = match read_pct_after(css, ",1) ", base) {
                Some(v) => v,
                None => break,
            };
            let (mid, p2) = match read_pct_after(css, ",0.5) ", p1) {
                Some(v) => v,
                None => break,
            };
            let (end, p3) = match read_pct_after(css, ",0) ", p2) {
                Some(v) => v,
                None => break,
            };
            out.push((s0, mid, end));
            cursor = p3;
        }
        out
    }

    #[test]
    fn css_stops_strictly_monotonic_default() {
        let css = render_css(&six_clusters(), &StyleOptions::default());
        let stops = extract_css_stops(&css);
        assert_eq!(stops.len(), 6);
        for (s0, m, e) in stops {
            assert_eq!(s0, 0, "first stop must be 0%");
            assert!(m < e, "mid ({m}) < end ({e}) must hold");
            assert!(m >= 0, "mid ({m}) must be >= 0");
        }
    }

    #[test]
    fn css_stops_strictly_monotonic_boundary_values() {
        // weight=1.0 / blur=0.0 / orb_size=2.0 の極端なケースで mid/end の
        // 衝突が起きないことを保証する。
        let extreme_clusters = vec![
            cluster([200, 100, 50], 0.5, 0.5, 1.0),   // 最大 weight
            cluster([50, 200, 100], 0.5, 0.5, 0.001), // 微小 weight
        ];
        for blur in [0.0_f32, 0.5, 1.0] {
            for orb_size in [0.1_f32, 1.0, 2.0, 4.0] {
                let opts = StyleOptions {
                    orb_size,
                    blur,
                    saturation: 1.0,
                    ..Default::default()
                };
                let css = render_css(&extreme_clusters, &opts);
                let stops = extract_css_stops(&css);
                assert!(!stops.is_empty(), "blur={blur} orb_size={orb_size}");
                for (s0, m, e) in &stops {
                    assert_eq!(*s0, 0);
                    assert!(
                        m < e,
                        "monotonic stops violated at blur={blur} orb_size={orb_size}: 0/{m}/{e}"
                    );
                }
            }
        }
    }

    #[test]
    fn softness_mid_is_regression_compatible_svg() {
        // softness=Mid を明示的に渡しても、デフォルト（=Mid）と完全一致。
        // これが回帰の最後の砦。
        let clusters = six_clusters();
        let opts_default = StyleOptions::default();
        let opts_mid = StyleOptions {
            softness: SoftnessPreset::Mid,
            ..StyleOptions::default()
        };
        assert_eq!(
            render_svg(&clusters, &opts_default),
            render_svg(&clusters, &opts_mid),
            "explicit Mid must match default StyleOptions for SVG"
        );
        assert_eq!(
            render_css(&clusters, &opts_default),
            render_css(&clusters, &opts_mid),
            "explicit Mid must match default StyleOptions for CSS"
        );
    }

    #[test]
    fn softness_low_high_differ_from_mid_svg() {
        // Low / High は Mid と異なる文字列を生成する。
        let clusters = six_clusters();
        let mid = render_svg(
            &clusters,
            &StyleOptions {
                softness: SoftnessPreset::Mid,
                ..StyleOptions::default()
            },
        );
        let low = render_svg(
            &clusters,
            &StyleOptions {
                softness: SoftnessPreset::Low,
                ..StyleOptions::default()
            },
        );
        let high = render_svg(
            &clusters,
            &StyleOptions {
                softness: SoftnessPreset::High,
                ..StyleOptions::default()
            },
        );
        assert_ne!(low, mid, "Low must produce different SVG than Mid");
        assert_ne!(high, mid, "High must produce different SVG than Mid");
        assert_ne!(low, high, "Low and High must differ from each other");
    }

    #[test]
    fn softness_alpha_mul_and_blur_offset_table() {
        // SoftnessPreset の数値テーブルが仕様通りであることを担保する回帰テスト。
        // Mid は alpha 1.0 / blur offset 0.0（既存挙動）。
        assert!((SoftnessPreset::Mid.alpha_mul() - 1.0).abs() < 1e-6);
        assert!((SoftnessPreset::Mid.blur_offset() - 0.0).abs() < 1e-6);
        // Low は blur を弱める（よりシャープ）。
        assert!((SoftnessPreset::Low.alpha_mul() - SoftnessPreset::Mid.alpha_mul()).abs() < 1e-6);
        assert!(SoftnessPreset::Low.blur_offset() < SoftnessPreset::Mid.blur_offset());
        // High は alpha を弱め、blur を強める（よりソフト）。
        assert!(SoftnessPreset::High.alpha_mul() < SoftnessPreset::Mid.alpha_mul());
        assert!(SoftnessPreset::High.blur_offset() > SoftnessPreset::Mid.blur_offset());
    }

    #[test]
    fn empty_clusters_produces_valid_svg() {
        let svg = render_svg(&[], &StyleOptions::default());
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("<rect")); // 背景は残る
        assert_eq!(svg.matches("<radialGradient").count(), 0);
        assert_eq!(svg.matches("<circle ").count(), 0);

        let css = render_css(&[], &StyleOptions::default());
        assert!(css.contains("--orber-bg: none"));
        assert_eq!(css.matches("radial-gradient(").count(), 0);
    }
}
