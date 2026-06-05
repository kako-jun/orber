//! orb 形状・スタイルの型定義と彩度調整。
//!
//! #225 で CPU のピクセル描画（`render_static` / `render_one_orb`）は撲滅され、
//! 実描画は GPU(WGSL, [`crate::gpu`]) が担う。このモジュールに残るのは GPU / pack /
//! SVG・CSS が共有する型と純粋な色変換だけ:
//!
//! - [`OrbShape`] / [`OrbStyle`]: 形状・スタイルの enum（CLI / wasm / GPU が参照）
//! - [`RenderOptions`]: 解像度・レイアウト定数の入れ物（既定 1080×1920）
//! - [`adjust_saturation_pub`]: sRGB を HSL 経由で彩度補正する純関数（GPU の per-orb
//!   色補正と完全同値。彩度のフラグは「CSS 的な見た目の彩度」に合わせるため LAB ではなく
//!   HSL 経路を使う）

use crate::glyph::GlyphFontId;
use crate::style::SoftnessPreset;
use aquarelle::AquarelleParams;
use palette::{FromColor, Hsl, IntoColor, Srgb};
use std::sync::Arc;

/// 個別 orb の描画スタイル。1 フレーム内で混在させる前提。
///
/// `Rim` は中心明 → 中間で少し落として外周フェードのリング感、`Soft` は中心明 →
/// 外周フェードの単純グラデーション。どちらを使うかは seed 由来で orb ごとに割り当て、
/// pack 経由で GPU(WGSL) の falloff カーブ（`style_bit`）に反映される。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrbStyle {
    /// リム強調（中間 stop で alpha を一段落として輪郭感を出す）。
    #[default]
    Rim,
    /// 単純ソフト（中間 stop なし、中心 → 透明への単調減衰）。
    Soft,
}

/// orb 描画形式。`Orb` は単一の radial gradient（解析的な円距離 → falloff）、
/// `Aquarelle` はセル画夜景の質感セット（[`aquarelle`] crate）、`Glyph` は同梱フォント
/// 1 文字のアウトライン塗りを有効にする。`Image` (#217) は外部から供給された画像
/// シルエット SDF を描く。
///
/// #235 で `Glyph` / `Image` は orb と同じ機構に統一された: orb の WGSL に「別の
/// シルエット（SDF）を食わせる」だけで、ぼやけ方・呼吸・rim/soft・合成は orb と完全に
/// 共通になり、独自のにじみ（bleed/halo）は撲滅した。形（SDF が表す "形からの距離"）
/// だけが違い、三角の記号は三角のまま orb のぼやけ方で描かれる。にじみは `Aquarelle`
/// shape だけの領分。
///
/// `Image` が `Arc<[u8]>`（画像シルエットの SDF テクスチャ）を持つため、`OrbShape`
/// は `Copy` ではなく `Clone`（`Arc` の参照カウント複製は安価）。`Glyph` のフォントは
/// [`GlyphFontId`] enum で識別し、実体の `Face` パースはモジュール側の `OnceLock`
/// キャッシュに任せる。`Image` の SDF も `image_rgba_to_sdf` で 1 度作って `Arc` で
/// 共有するだけなので、各 shape は依然として重い state を inline で持たない。
#[derive(Debug, Clone, Default)]
pub enum OrbShape {
    #[default]
    Orb,
    Aquarelle(AquarelleParams),
    /// 1 文字のグリフを orb として描く。`ch` は描画する文字、`font` は同梱フォント識別子。
    Glyph {
        ch: char,
        font: GlyphFontId,
    },
    /// 画像シルエットを orb として描く（#217）。`sdf` は [`crate::glyph::mask_to_sdf`]
    /// と同フォーマット（`size × size`、128≈edge）の SDF テクスチャ、`size` はその辺長。
    /// SDF の生成（`image_rgba_to_sdf`）は CLI / web 側で 1 度だけ行い、`Arc` で共有する。
    Image {
        sdf: Arc<[u8]>,
        size: u32,
    },
}

impl PartialEq for OrbShape {
    // Aquarelle 内部のパラメータ (AquarelleParams) は比較対象から外す。
    // ここでの "等価" は「形が同じか」だけを判定する用途を想定している。
    // Glyph は文字とフォント識別子まで含めて比較する（軽い値なので）。
    // Image は Aquarelle と同様に内部 SDF（重い Arc<[u8]>）を比較対象から外し、
    // 「Image == Image なら true」とする。「形が同じか」の用途では SDF バイト列の
    // 完全一致まで問わない（バリエーション選別等で同一 shape 判定に使うだけ）方針に揃える。
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (OrbShape::Orb, OrbShape::Orb) => true,
            (OrbShape::Aquarelle(_), OrbShape::Aquarelle(_)) => true,
            (OrbShape::Glyph { ch: a, font: fa }, OrbShape::Glyph { ch: b, font: fb }) => {
                a == b && fa == fb
            }
            (OrbShape::Image { .. }, OrbShape::Image { .. }) => true,
            _ => false,
        }
    }
}

/// orb 描画の解像度・レイアウト定数を束ねる入れ物。実描画は GPU(WGSL) が
/// [`crate::animate::AnimateOptions`] 経由で行うため、これは主に既定値
/// （`Default` = 1080×1920）の供給と CLI 既定値の検証基準として使う。
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// 出力幅（ピクセル）
    pub width: u32,
    /// 出力高さ（ピクセル）
    pub height: u32,
    /// orb サイズ倍率（1.0 = デフォルト）
    pub orb_size: f32,
    /// ぼかし強度 0.0..=1.0（0=シャープ、1=完全ぼかし）
    pub blur: f32,
    /// 彩度倍率（1.0 = unchanged）
    pub saturation: f32,
    /// 背景 RGBA。alpha=0 で透過。デフォルトは黒不透明。
    pub background: [u8; 4],
    /// orb の描画形式。Orb なら単一 radial gradient、Aquarelle ならセル画夜景の質感セット。
    pub shape: OrbShape,
    /// ぼかし (Softness) preset（#55, #131 で改名）。Mid で既存挙動と完全同値。
    pub softness: SoftnessPreset,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            orb_size: 1.0,
            blur: 0.5,
            saturation: 1.0,
            background: [0, 0, 0, 255],
            shape: OrbShape::Orb,
            softness: SoftnessPreset::Mid,
        }
    }
}

/// sRGB 0-255 を HSL に変換し、彩度を `factor` 倍してから sRGB に戻す。
///
/// 彩度調整は HSL 経路で行う。cluster 抽出は LAB（知覚距離）を使うが、
/// 彩度のフラグは「CSS 的な見た目の彩度」に合わせるほうが UI 直感に近いため、
/// 意図的に色空間を分けている。
pub fn adjust_saturation_pub(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    adjust_saturation(rgb, factor)
}

pub(crate) fn adjust_saturation(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    // 1.0001 等の浮動小数点誤差レベルの入力でも fast path に入るよう、緩めの 1e-4
    // 閾値を使う（f32::EPSILON ≈ 1.19e-7 だと CLI 入力では実用上ほぼ通らない）。
    if (factor - 1.0).abs() < 1e-4 {
        return rgb;
    }
    let srgb = Srgb::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    let mut hsl: Hsl = Hsl::from_color(srgb);
    hsl.saturation = (hsl.saturation * factor).clamp(0.0, 1.0);
    let out: Srgb = hsl.into_color();
    [
        (out.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjust_saturation_identity_passthrough() {
        // factor==1.0（と誤差レベル）は入力をそのまま返す fast path。
        assert_eq!(adjust_saturation([220, 30, 40], 1.0), [220, 30, 40]);
        assert_eq!(adjust_saturation([220, 30, 40], 1.00005), [220, 30, 40]);
    }

    #[test]
    fn adjust_saturation_zero_is_grayscale() {
        // saturation=0.0 で R==G==B（グレースケール）になる。
        let [r, g, b] = adjust_saturation([220, 30, 40], 0.0);
        let (r, g, b) = (r as i32, g as i32, b as i32);
        assert!(
            (r - g).abs() <= 2 && (g - b).abs() <= 2 && (r - b).abs() <= 2,
            "saturation=0 should produce grayscale, got R={r} G={g} B={b}"
        );
    }

    #[test]
    fn adjust_saturation_boost_increases_chroma() {
        // 彩度を上げると R チャネルがより支配的になる（赤寄りの入力）。
        let base = [180u8, 90, 90];
        let boosted = adjust_saturation(base, 2.0);
        let spread = |c: [u8; 3]| c[0] as i32 - c[1] as i32;
        assert!(
            spread(boosted) >= spread(base),
            "boosting saturation must not reduce R-G spread: base={base:?} boosted={boosted:?}"
        );
    }

    #[test]
    fn adjust_saturation_pub_matches_internal() {
        // 公開ラッパ adjust_saturation_pub は内部 adjust_saturation と完全同値。
        for factor in [0.0_f32, 0.5, 1.0, 2.0, 4.0] {
            assert_eq!(
                adjust_saturation_pub([120, 200, 60], factor),
                adjust_saturation([120, 200, 60], factor)
            );
        }
    }

    /// #217: OrbShape の PartialEq は「形が同じか」だけを見る（Image の SDF バイト列は
    /// 比較対象外）。GPU / variations の同一 shape 判定で使う契約。
    #[test]
    fn orb_shape_eq_is_shape_only() {
        let a = OrbShape::Image {
            sdf: Arc::from(vec![1u8; 256 * 256]),
            size: 256,
        };
        let b = OrbShape::Image {
            sdf: Arc::from(vec![0u8; 256 * 256]),
            size: 256,
        };
        assert_eq!(a, b, "Image == Image must be true regardless of SDF bytes");
        assert_ne!(a, OrbShape::Orb);
        assert_eq!(
            OrbShape::Aquarelle(AquarelleParams::default()),
            OrbShape::Aquarelle(AquarelleParams {
                bleed: 0.1,
                bloom: 0.9,
                offset: 0.2,
                halo: 0.8,
            }),
            "Aquarelle == Aquarelle ignores params (shape-only eq)"
        );
        assert_eq!(
            OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2
            },
            OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2
            }
        );
        assert_ne!(
            OrbShape::Glyph {
                ch: '☆',
                font: GlyphFontId::NotoSymbols2
            },
            OrbShape::Glyph {
                ch: '★',
                font: GlyphFontId::NotoSymbols2
            }
        );
    }
}
