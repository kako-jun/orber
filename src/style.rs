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
pub const STYLE_WIDTH: u32 = 1080;
/// SVG viewBox 高さ。PNG / 動画と揃える。
pub const STYLE_HEIGHT: u32 = 1920;

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
}

impl Default for StyleOptions {
    fn default() -> Self {
        Self {
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
        }
    }
}

/// クラスタ列を SVG 文字列として描画する。
///
/// 出力は viewBox `0 0 1080 1920` の自己完結 SVG。背景は黒い `<rect>`、
/// 各 cluster は `<radialGradient>` と `<circle>` のペアになる。
pub fn render_svg(clusters: &[Cluster], opts: &StyleOptions) -> String {
    let blur = opts.blur.clamp(0.0, 1.0);
    let saturation = opts.saturation.max(0.0);
    let orb_size = opts.orb_size.max(0.0);

    let width = STYLE_WIDTH as f32;
    let height = STYLE_HEIGHT as f32;
    let base_radius_unit = width.min(height) * 0.25 * orb_size;

    // mid_offset: blur=0 で外寄り（中心の不透明領域が広い）、blur=1 で中心寄り。
    // PNG 側 (1.0 - blur*0.8) と意味的に整合させ、% 表記の中間 stop を作る。
    let mid_pct = ((1.0 - blur * 0.8).clamp(0.05, 0.95) * 100.0).round() as i32;

    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {STYLE_WIDTH} {STYLE_HEIGHT}\" width=\"{STYLE_WIDTH}\" height=\"{STYLE_HEIGHT}\">\n"
    ));
    s.push_str("  <rect width=\"100%\" height=\"100%\" fill=\"black\"/>\n");

    // <defs> 内に gradient 定義。空 clusters の場合でも有効な SVG として残す。
    s.push_str("  <defs>\n");
    for (i, cluster) in clusters.iter().enumerate() {
        let [r, g, b] = adjust_saturation(cluster.color, saturation);
        s.push_str(&format!(
            "    <radialGradient id=\"orb-{i}\" cx=\"50%\" cy=\"50%\" r=\"50%\">\n"
        ));
        s.push_str(&format!(
            "      <stop offset=\"0%\" stop-color=\"rgb({r},{g},{b})\" stop-opacity=\"1\"/>\n"
        ));
        s.push_str(&format!(
            "      <stop offset=\"{mid_pct}%\" stop-color=\"rgb({r},{g},{b})\" stop-opacity=\"0.5\"/>\n"
        ));
        s.push_str(&format!(
            "      <stop offset=\"100%\" stop-color=\"rgb({r},{g},{b})\" stop-opacity=\"0\"/>\n"
        ));
        s.push_str("    </radialGradient>\n");
    }
    s.push_str("  </defs>\n");

    for (i, cluster) in clusters.iter().enumerate() {
        let weight = cluster.weight.max(0.0);
        let radius = base_radius_unit * weight.sqrt();
        if radius <= 0.0 {
            continue;
        }
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
    let blur = opts.blur.clamp(0.0, 1.0);
    let saturation = opts.saturation.max(0.0);
    let orb_size = opts.orb_size.max(0.0);

    // mid stop の位置（%）: blur=0 で外寄り、blur=1 で中心寄り。SVG と同じ式。
    let mid_pct = ((10.0 + blur * 40.0).clamp(1.0, 99.0)).round() as i32;

    let mut s = String::new();
    s.push_str("/* orber-generated background.\n");
    s.push_str("   Apply to <body> or any block element:\n");
    s.push_str("       body {\n");
    s.push_str("           margin: 0;\n");
    s.push_str("           min-height: 100vh;\n");
    s.push_str("           background-color: black;\n");
    s.push_str("           background-image: var(--orber-bg);\n");
    s.push_str("       }\n");
    s.push_str("   Generated as a CSS variable; reference with var(--orber-bg). */\n");
    s.push_str(":root {\n");
    s.push_str("    --orber-bg:");

    // 有効な gradient だけ集めてから書き出す（最後のカンマを抑制するため）。
    let mut gradients: Vec<String> = Vec::new();
    for cluster in clusters.iter() {
        let weight = cluster.weight.max(0.0);
        if weight <= 0.0 {
            continue;
        }
        let [r, g, b] = adjust_saturation(cluster.color, saturation);
        let x = (cluster.centroid.x.clamp(0.0, 1.0) * 100.0).round() as i32;
        let y = (cluster.centroid.y.clamp(0.0, 1.0) * 100.0).round() as i32;
        // PNG 側 radius = min(W,H) * 0.25 * orb_size * sqrt(weight) → 画面短辺の最大 25%。
        // CSS は背景全体に対する % 指定。短辺の 25% は viewBox 換算で約 12.5% 相当だが、
        // gradient end は radial の半径終端で円が透明になる位置。視覚一致を狙って
        // sqrt(weight) * 30% * orb_size を採用し、1..=100 で clamp する。
        let end = (weight.sqrt() * 30.0 * orb_size).clamp(1.0, 100.0).round() as i32;
        gradients.push(format!(
            "radial-gradient(circle at {x}% {y}%, rgba({r},{g},{b},1) 0%, rgba({r},{g},{b},0.5) {mid_pct}%, rgba({r},{g},{b},0) {end}%)"
        ));
    }

    if gradients.is_empty() {
        // 空でも valid な CSS にする（": ;" は参照側で no-op になる）。
        s.push(' ');
    } else {
        for (i, g) in gradients.iter().enumerate() {
            s.push_str("\n        ");
            s.push_str(g);
            if i + 1 < gradients.len() {
                s.push(',');
            }
        }
    }
    s.push_str(";\n");
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
        assert!(svg.contains("<rect width=\"100%\" height=\"100%\" fill=\"black\""));
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

    #[test]
    fn empty_clusters_produces_valid_svg() {
        let svg = render_svg(&[], &StyleOptions::default());
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("<rect")); // 背景は残る
        assert_eq!(svg.matches("<radialGradient").count(), 0);
        assert_eq!(svg.matches("<circle ").count(), 0);

        let css = render_css(&[], &StyleOptions::default());
        assert!(css.contains("--orber-bg:"));
        assert!(css.contains(";"));
        assert_eq!(css.matches("radial-gradient(").count(), 0);
    }
}
