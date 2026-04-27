//! バリエーション一括生成のプリセット。
//!
//! `--variations N --output-dir DIR` 経由で 1 つの入力から複数案を一気に書き出す
//! ための「良い感じの 10 案セット」をハードコードする。完全ランダムだとハズレ案が
//! 混ざるので、まずは編集者が手で選んだセットから始める方針。
//!
//! # 設計メモ
//!
//! - `VariationSpec` は静止 (Png) / 動画 (Mp4) いずれかに展開する。動画の shape /
//!   speed / duration_ms は spec 側に持たせ、PNG では shape / speed / duration_ms は
//!   参照されない（無視）
//! - フィルタリング時 (`--variations-mode still|video|mixed`) は VariationKind で
//!   絞り込む。`mixed` がデフォルト
//! - 出力ファイル名は `{idx:02}_{label}.{ext}` 形式。label は ASCII / underscore
//!   のみで構成し、シェル安全に保つ

use crate::animate::{MotionShape, MotionSpeed};

/// バリエーションが書き出す出力種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariationKind {
    Png,
    Mp4,
}

impl VariationKind {
    /// 出力ファイルの拡張子（drop-in）。
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Mp4 => "mp4",
        }
    }
}

/// `--variations-mode` の選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariationMode {
    Still,
    Video,
    Mixed,
}

impl VariationMode {
    /// この mode で許される `VariationKind` か。
    pub fn accepts(self, kind: VariationKind) -> bool {
        match self {
            Self::Mixed => true,
            Self::Still => kind == VariationKind::Png,
            Self::Video => kind == VariationKind::Mp4,
        }
    }
}

/// 1 つのバリエーション案。
#[derive(Debug, Clone, Copy)]
pub struct VariationSpec {
    pub label: &'static str,
    pub kind: VariationKind,
    /// 動画用の軌道形（Png では参照されない）。
    pub shape: MotionShape,
    /// 動画用の速度（Png では参照されない）。
    pub speed: MotionSpeed,
    pub orb_size: f32,
    pub blur: f32,
    pub saturation: f32,
    pub seed: u64,
    /// 動画用の長さ（ms）。Png では参照されない。
    pub duration_ms: u64,
}

/// デフォルト 10 案セット。
///
/// 編集者の手選別: 静止 ×3 / 微動 ×4 / 呼吸 ×1 / リサジュー ×2 = 10。
/// 動画は 4 秒で短尺寄り。手元で N 案見て採用したいものを選ぶ UX を想定。
pub const DEFAULT_VARIATIONS: &[VariationSpec] = &[
    VariationSpec {
        label: "still_warm",
        kind: VariationKind::Png,
        shape: MotionShape::Still,
        speed: MotionSpeed::Slow,
        orb_size: 1.0,
        blur: 0.5,
        saturation: 1.2,
        seed: 1,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "still_cool",
        kind: VariationKind::Png,
        shape: MotionShape::Still,
        speed: MotionSpeed::Slow,
        orb_size: 1.2,
        blur: 0.7,
        saturation: 0.8,
        seed: 2,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "still_punch",
        kind: VariationKind::Png,
        shape: MotionShape::Still,
        speed: MotionSpeed::Slow,
        orb_size: 0.8,
        blur: 0.3,
        saturation: 1.5,
        seed: 3,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "drift_vertical_subtle",
        kind: VariationKind::Mp4,
        shape: MotionShape::Vertical,
        speed: MotionSpeed::Subtle,
        orb_size: 1.2,
        blur: 0.6,
        saturation: 1.0,
        seed: 4,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "drift_horizontal_subtle",
        kind: VariationKind::Mp4,
        shape: MotionShape::Horizontal,
        speed: MotionSpeed::Subtle,
        orb_size: 1.0,
        blur: 0.5,
        saturation: 1.0,
        seed: 5,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "drift_diagonal_subtle",
        kind: VariationKind::Mp4,
        shape: MotionShape::Diagonal,
        speed: MotionSpeed::Subtle,
        orb_size: 1.1,
        blur: 0.5,
        saturation: 1.1,
        seed: 6,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "twinkle_subtle",
        kind: VariationKind::Mp4,
        shape: MotionShape::Twinkle,
        speed: MotionSpeed::Subtle,
        orb_size: 1.0,
        blur: 0.5,
        saturation: 1.0,
        seed: 7,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "breathe_slow",
        kind: VariationKind::Mp4,
        shape: MotionShape::Breathe,
        speed: MotionSpeed::Slow,
        orb_size: 1.0,
        blur: 0.5,
        saturation: 1.0,
        seed: 8,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "lissajous_slow",
        kind: VariationKind::Mp4,
        shape: MotionShape::Lissajous,
        speed: MotionSpeed::Slow,
        orb_size: 1.0,
        blur: 0.5,
        saturation: 1.1,
        seed: 9,
        duration_ms: 4000,
    },
    VariationSpec {
        label: "lissajous_lively",
        kind: VariationKind::Mp4,
        shape: MotionShape::Lissajous,
        speed: MotionSpeed::Lively,
        orb_size: 0.9,
        blur: 0.4,
        saturation: 1.2,
        seed: 10,
        duration_ms: 4000,
    },
];

/// 上限 N と mode で `DEFAULT_VARIATIONS` から実際に書き出す spec を選び出す。
///
/// `mode` で受け付ける kind だけを残し、上から `n` 件取る。要求された n が
/// 残り件数より多い場合はあるだけ返す（要求数を満たせなかったかは呼び出し側で判定）。
pub fn select_specs(n: usize, mode: VariationMode) -> Vec<VariationSpec> {
    DEFAULT_VARIATIONS
        .iter()
        .copied()
        .filter(|s| mode.accepts(s.kind))
        .take(n)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_set_has_ten_specs() {
        assert_eq!(DEFAULT_VARIATIONS.len(), 10);
    }

    #[test]
    fn default_set_balance() {
        // 静止と動画の混合になっていること（mixed UX が成立する）。
        let png = DEFAULT_VARIATIONS
            .iter()
            .filter(|s| s.kind == VariationKind::Png)
            .count();
        let mp4 = DEFAULT_VARIATIONS
            .iter()
            .filter(|s| s.kind == VariationKind::Mp4)
            .count();
        assert!(png >= 1, "expected at least 1 still variation");
        assert!(mp4 >= 1, "expected at least 1 video variation");
    }

    #[test]
    fn labels_unique_and_ascii_safe() {
        let mut seen = std::collections::HashSet::new();
        for s in DEFAULT_VARIATIONS {
            assert!(seen.insert(s.label), "duplicate label: {}", s.label);
            for ch in s.label.chars() {
                assert!(
                    ch.is_ascii_alphanumeric() || ch == '_',
                    "non shell-safe char in label {:?}: {ch:?}",
                    s.label
                );
            }
        }
    }

    #[test]
    fn select_specs_respects_mode() {
        let still = select_specs(10, VariationMode::Still);
        assert!(still.iter().all(|s| s.kind == VariationKind::Png));
        let video = select_specs(10, VariationMode::Video);
        assert!(video.iter().all(|s| s.kind == VariationKind::Mp4));
        let mixed = select_specs(10, VariationMode::Mixed);
        assert_eq!(mixed.len(), 10);
    }

    #[test]
    fn select_specs_respects_n() {
        let three = select_specs(3, VariationMode::Mixed);
        assert_eq!(three.len(), 3);
        let zero = select_specs(0, VariationMode::Mixed);
        assert_eq!(zero.len(), 0);
    }
}
