//! 背景色解決モジュール。
//!
//! `--background` フラグの値（`black` / `white` / `auto` / `transparent` /
//! `#RRGGBB` / `#RRGGBBAA`）を [`Background`] enum に正規化し、入力画像から
//! 最終的な RGBA 8bit を決める [`resolve`] を提供する。
//!
//! # 設計メモ
//!
//! - `auto` は入力を 64x64 にダウンサンプルした平均色を取り、HSL の彩度を
//!   半分・明度を 0.85 倍 + 下限 0.05 / 上限 0.7 に押し込めて純白／純黒を避ける。
//!   写真の支配色を「やや暗めの背景」に寄せるのが狙い
//! - `transparent` は alpha=0。動画は yuv420p 制約で alpha 不可なので、呼び出し
//!   側で `transparent + 動画モード` を弾く責務を負う（このモジュールでは
//!   そこまで踏み込まない）

use image::RgbImage;

/// 背景指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Black,
    White,
    Auto,
    Hex([u8; 4]),
    Transparent,
}

impl Background {
    /// 透過が要求されているかどうか。動画経路の事前バリデーションに使う。
    pub fn is_transparent(self) -> bool {
        match self {
            Self::Transparent => true,
            Self::Hex([_, _, _, a]) => a == 0,
            _ => false,
        }
    }
}

/// `--background` 値の文字列パースエラー。
#[derive(Debug, PartialEq, Eq)]
pub enum BackgroundParseError {
    InvalidHex(String),
    Unknown(String),
}

impl std::fmt::Display for BackgroundParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHex(s) => {
                write!(f, "invalid hex color {s:?} (expected #RRGGBB or #RRGGBBAA)")
            }
            Self::Unknown(s) => write!(
                f,
                "unknown --background value {s:?} (expected black|white|auto|transparent|#RRGGBB)"
            ),
        }
    }
}

impl std::error::Error for BackgroundParseError {}

impl std::str::FromStr for Background {
    type Err = BackgroundParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.trim().to_ascii_lowercase();
        match lower.as_str() {
            "black" => Ok(Self::Black),
            "white" => Ok(Self::White),
            "auto" => Ok(Self::Auto),
            "transparent" | "none" => Ok(Self::Transparent),
            _ => parse_hex(&lower).ok_or_else(|| {
                if lower.starts_with('#') {
                    BackgroundParseError::InvalidHex(s.to_string())
                } else {
                    BackgroundParseError::Unknown(s.to_string())
                }
            }),
        }
    }
}

fn parse_hex(s: &str) -> Option<Background> {
    let hex = s.strip_prefix('#')?;
    let pair = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        6 => Some(Background::Hex([pair(0)?, pair(2)?, pair(4)?, 255])),
        8 => Some(Background::Hex([pair(0)?, pair(2)?, pair(4)?, pair(6)?])),
        _ => None,
    }
}

/// `Background` を入力画像と組み合わせて最終的な RGBA 8bit に解決する。
///
/// `auto` のみ入力画像に依存する。それ以外の variant は入力を参照しない
/// （引数に渡されても無視される）。
pub fn resolve(input: &RgbImage, bg: Background) -> [u8; 4] {
    match bg {
        Background::Black => [0, 0, 0, 255],
        Background::White => [255, 255, 255, 255],
        Background::Hex(rgba) => rgba,
        Background::Transparent => [0, 0, 0, 0],
        Background::Auto => auto_color(input),
    }
}

fn auto_color(input: &RgbImage) -> [u8; 4] {
    use image::imageops::{resize, FilterType};
    use palette::{FromColor, Hsl, IntoColor, Srgb};

    // 入力が極小で 64x64 に拡大される場合でも結果は安定する（平均色は変わらない）。
    let small = if input.width() == 0 || input.height() == 0 {
        return [0, 0, 0, 255];
    } else {
        resize(input, 64, 64, FilterType::Triangle)
    };

    let mut sr: u64 = 0;
    let mut sg: u64 = 0;
    let mut sb: u64 = 0;
    let mut n: u64 = 0;
    for px in small.pixels() {
        sr += px[0] as u64;
        sg += px[1] as u64;
        sb += px[2] as u64;
        n += 1;
    }
    let n = n.max(1) as f32;
    let srgb = Srgb::new(
        sr as f32 / n / 255.0,
        sg as f32 / n / 255.0,
        sb as f32 / n / 255.0,
    );
    let mut hsl: Hsl = Hsl::from_color(srgb);
    hsl.saturation = (hsl.saturation * 0.5).clamp(0.0, 1.0);
    hsl.lightness = (hsl.lightness * 0.85).clamp(0.05, 0.7);
    let out: Srgb = hsl.into_color();
    [
        (out.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;
    use std::str::FromStr;

    #[test]
    fn parse_named() {
        assert_eq!(Background::from_str("black").unwrap(), Background::Black);
        assert_eq!(Background::from_str("WHITE").unwrap(), Background::White);
        assert_eq!(Background::from_str("auto").unwrap(), Background::Auto);
        assert_eq!(
            Background::from_str("transparent").unwrap(),
            Background::Transparent
        );
        assert_eq!(
            Background::from_str("none").unwrap(),
            Background::Transparent
        );
    }

    #[test]
    fn parse_hex_rgb() {
        assert_eq!(
            Background::from_str("#1a2b3c").unwrap(),
            Background::Hex([0x1a, 0x2b, 0x3c, 0xff])
        );
        assert_eq!(
            Background::from_str("#FF00FF").unwrap(),
            Background::Hex([0xff, 0x00, 0xff, 0xff])
        );
    }

    #[test]
    fn parse_hex_rgba() {
        assert_eq!(
            Background::from_str("#11223344").unwrap(),
            Background::Hex([0x11, 0x22, 0x33, 0x44])
        );
    }

    #[test]
    fn parse_invalid_hex() {
        assert!(matches!(
            Background::from_str("#zzzzzz"),
            Err(BackgroundParseError::InvalidHex(_))
        ));
        assert!(matches!(
            Background::from_str("#abc"),
            Err(BackgroundParseError::InvalidHex(_))
        ));
    }

    #[test]
    fn parse_unknown() {
        assert!(matches!(
            Background::from_str("magenta"),
            Err(BackgroundParseError::Unknown(_))
        ));
    }

    #[test]
    fn resolve_named_colors() {
        let img = RgbImage::new(2, 2);
        assert_eq!(resolve(&img, Background::Black), [0, 0, 0, 255]);
        assert_eq!(resolve(&img, Background::White), [255, 255, 255, 255]);
        assert_eq!(resolve(&img, Background::Transparent), [0, 0, 0, 0]);
        assert_eq!(
            resolve(&img, Background::Hex([10, 20, 30, 200])),
            [10, 20, 30, 200]
        );
    }

    #[test]
    fn resolve_auto_red_image_yields_reddish_dim_color() {
        // 全ピクセル赤の画像 → auto は赤系の暗めの色になる（彩度半減・明度抑制）。
        let mut img = RgbImage::new(8, 8);
        for px in img.pixels_mut() {
            *px = image::Rgb([255, 0, 0]);
        }
        let [r, g, b, a] = resolve(&img, Background::Auto);
        assert_eq!(a, 255);
        assert!(
            r > g && r > b,
            "auto of red should remain red-dominant: {r} {g} {b}"
        );
        // 純赤 (255,0,0) より暗くなっている（明度 0.7 上限）。
        assert!(r < 255, "auto should dim pure red, got {r}");
    }

    #[test]
    fn is_transparent_logic() {
        assert!(Background::Transparent.is_transparent());
        assert!(Background::Hex([0, 0, 0, 0]).is_transparent());
        assert!(!Background::Hex([0, 0, 0, 1]).is_transparent());
        assert!(!Background::Black.is_transparent());
        assert!(!Background::Auto.is_transparent());
    }
}
