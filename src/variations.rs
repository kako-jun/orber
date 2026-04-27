//! バリエーション一括生成のプリセット。
//!
//! `--variations N --output-dir DIR` 経由で 1 つの入力から複数案を一気に書き出す
//! ための「良い感じの 10 案セット」をハードコードする。完全ランダムだとハズレ案が
//! 混ざるので、まずは編集者が手で選んだセットから始める方針。
//!
//! # 設計メモ
//!
//! - `VariationSpec` は静止 (Png) / 動画 (Mp4) いずれかに展開する。動画の direction /
//!   speed / duration_ms は spec 側に持たせ、PNG では direction / speed は静止画でも
//!   「t=0 のフレームを切り取った構図」を作るのに使われるが、duration_ms は無視される。
//! - フィルタリング時 (`--variations-mode still|video|mixed`) は VariationKind で
//!   絞り込む。`mixed` がデフォルト
//! - 出力ファイル名は `{idx:02}_{label}.{ext}` 形式。label は ASCII / underscore
//!   のみで構成し、シェル安全に保つ

use crate::animate::{MotionDirection, MotionSpeed};

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
///
/// 色まわりは 4 軸（hue / lightness / saturation / dominant_rotation）で動かし、
/// クラスタ数は spec ごとに変える（少なめだと 1 色が支配的、多めだと粒立った印象）。
/// 動きは direction（4 方向）× speed（VerySlow/Slow/Medium）の組で表現する。
#[derive(Debug, Clone, Copy)]
pub struct VariationSpec {
    pub label: &'static str,
    pub kind: VariationKind,
    /// 流れる方向。Png では「t=0 フレームを切り取った構図」に使われる。
    pub direction: MotionDirection,
    /// 動画用の速度（Png でも phase 散らばりに使われるが、cycle 自体は t=0 で意味なし）。
    pub speed: MotionSpeed,
    pub orb_size: f32,
    pub blur: f32,
    /// HSL 彩度倍率（既存）。`ColorMod::saturation` に渡される。
    pub saturation: f32,
    /// 色相回転（度）。`ColorMod::hue_shift_deg` に渡される。
    pub hue_shift_deg: f32,
    /// HSL 明度に対する加算バイアス（-0.5..0.5 想定）。`ColorMod::lightness_bias`。
    pub lightness_bias: f32,
    /// k-means クラスタ数。少ないほどベタ寄り、多いほど粒立つ。
    pub cluster_count: usize,
    /// 支配色ローテーション（weight 降順 cluster の右回転 N）。
    pub dominant_rotation: usize,
    pub seed: u64,
    /// 動画用の長さ（ms）。Png では参照されない。
    pub duration_ms: u64,
}

impl VariationSpec {
    /// この spec から色モジュレーション設定を取り出す。
    pub fn color_mod(&self) -> crate::color_mod::ColorMod {
        crate::color_mod::ColorMod {
            hue_shift_deg: self.hue_shift_deg,
            lightness_bias: self.lightness_bias,
            saturation: self.saturation,
            dominant_rotation: self.dominant_rotation,
        }
    }
}

/// デフォルト 10 案セット (v0.3.0 preset)。
///
/// 構成: 静止 4 + 動画 6。動きは「左→右」「右→左」「上→下」「下→上」の 4 方向を
/// 散らし、速度は VerySlow / Slow / Medium で散らす。色は hue_shift × lightness_bias ×
/// saturation × cluster_count の 4 軸で散らし、同じ入力でも 10 通りの異なる印象を作る。
///
/// 各 spec の数値根拠は GitHub Issue #41 の preset 表を参照。
pub const DEFAULT_VARIATIONS: &[VariationSpec] = &[
    VariationSpec {
        label: "warm_glow_lr",
        kind: VariationKind::Png,
        direction: MotionDirection::LeftToRight,
        speed: MotionSpeed::Slow,
        orb_size: 3.0,
        blur: 0.5,
        saturation: 1.4,
        hue_shift_deg: 25.0,
        lightness_bias: 0.05,
        cluster_count: 4,
        dominant_rotation: 0,
        seed: 1,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "cool_mist_rl",
        kind: VariationKind::Png,
        direction: MotionDirection::RightToLeft,
        speed: MotionSpeed::VerySlow,
        orb_size: 3.5,
        blur: 0.6,
        saturation: 1.0,
        hue_shift_deg: -35.0,
        lightness_bias: -0.05,
        cluster_count: 4,
        dominant_rotation: 1,
        seed: 2,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "hi_key_tb",
        kind: VariationKind::Png,
        direction: MotionDirection::TopToBottom,
        speed: MotionSpeed::Slow,
        orb_size: 2.8,
        blur: 0.4,
        saturation: 1.4,
        hue_shift_deg: 0.0,
        lightness_bias: 0.20,
        cluster_count: 4,
        dominant_rotation: 2,
        seed: 3,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "dark_mood_bt",
        kind: VariationKind::Png,
        direction: MotionDirection::BottomToTop,
        speed: MotionSpeed::VerySlow,
        orb_size: 3.2,
        blur: 0.6,
        saturation: 0.7,
        hue_shift_deg: 0.0,
        lightness_bias: -0.20,
        cluster_count: 4,
        dominant_rotation: 0,
        seed: 4,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "drift_lr_slow",
        kind: VariationKind::Mp4,
        direction: MotionDirection::LeftToRight,
        speed: MotionSpeed::Slow,
        orb_size: 3.0,
        blur: 0.5,
        saturation: 1.1,
        hue_shift_deg: 10.0,
        lightness_bias: 0.0,
        cluster_count: 4,
        dominant_rotation: 1,
        seed: 5,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "drift_rl_very_slow",
        kind: VariationKind::Mp4,
        direction: MotionDirection::RightToLeft,
        speed: MotionSpeed::VerySlow,
        orb_size: 4.0,
        blur: 0.6,
        saturation: 1.2,
        hue_shift_deg: 0.0,
        lightness_bias: 0.0,
        cluster_count: 3,
        dominant_rotation: 2,
        seed: 6,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "drift_tb_slow",
        kind: VariationKind::Mp4,
        direction: MotionDirection::TopToBottom,
        speed: MotionSpeed::Slow,
        orb_size: 2.8,
        blur: 0.4,
        saturation: 1.3,
        hue_shift_deg: -20.0,
        lightness_bias: 0.0,
        cluster_count: 5,
        dominant_rotation: 0,
        seed: 7,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "drift_bt_slow",
        kind: VariationKind::Mp4,
        direction: MotionDirection::BottomToTop,
        speed: MotionSpeed::Slow,
        orb_size: 3.2,
        blur: 0.5,
        saturation: 1.0,
        hue_shift_deg: 20.0,
        lightness_bias: 0.0,
        cluster_count: 4,
        dominant_rotation: 1,
        seed: 8,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "aurora_rl",
        kind: VariationKind::Mp4,
        direction: MotionDirection::RightToLeft,
        speed: MotionSpeed::VerySlow,
        orb_size: 3.5,
        blur: 0.7,
        saturation: 1.3,
        hue_shift_deg: -25.0,
        lightness_bias: 0.05,
        cluster_count: 4,
        dominant_rotation: 2,
        seed: 9,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "dream_lr",
        kind: VariationKind::Mp4,
        direction: MotionDirection::LeftToRight,
        speed: MotionSpeed::Medium,
        orb_size: 2.8,
        blur: 0.5,
        saturation: 0.9,
        hue_shift_deg: 40.0,
        lightness_bias: 0.10,
        cluster_count: 5,
        dominant_rotation: 0,
        seed: 10,
        duration_ms: 8000,
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
    fn default_set_covers_all_directions() {
        // 4 方向すべてが少なくとも 1 つは preset に含まれていることを保証する。
        let mut seen_lr = false;
        let mut seen_rl = false;
        let mut seen_tb = false;
        let mut seen_bt = false;
        for s in DEFAULT_VARIATIONS {
            match s.direction {
                MotionDirection::LeftToRight => seen_lr = true,
                MotionDirection::RightToLeft => seen_rl = true,
                MotionDirection::TopToBottom => seen_tb = true,
                MotionDirection::BottomToTop => seen_bt = true,
            }
        }
        assert!(seen_lr, "LeftToRight not represented");
        assert!(seen_rl, "RightToLeft not represented");
        assert!(seen_tb, "TopToBottom not represented");
        assert!(seen_bt, "BottomToTop not represented");
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
