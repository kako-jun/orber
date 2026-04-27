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
//! - **色は変えない**。同じ入力画像から作る複数バリエーションでは入力色をそのまま
//!   使う（warm / cool 等の色ラベル軸は廃止）。差別化軸は方向 4 / 速度 3 / count /
//!   orb_size / blur のみ。
//! - クラスタ数（kmeans の K）は呼び出し側で 5 固定（`main.rs` の
//!   `VARIATIONS_KMEANS_K`）。spec ごとに動かさない（パレット汚しを避けるため）。
//!   `count` は K 色を **N 個に展開** する数で、画面の約 7 割を埋めるのが狙い。

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
/// 差別化軸は方向 4 / 速度 2 / count / orb_size / blur のみ。色は spec 内に持たない
/// （入力画像の kmeans 結果をそのまま使う）。
#[derive(Debug, Clone, Copy)]
pub struct VariationSpec {
    pub label: &'static str,
    pub kind: VariationKind,
    /// 流れる方向。Png では「t=0 フレームを切り取った構図」に使われる。
    pub direction: MotionDirection,
    /// 動画用の速度（Png でも phase 散らばりに使われるが、cycle 自体は t=0 で意味なし）。
    pub speed: MotionSpeed,
    /// 同時可視 orb の総数。クラスタ K 色を N 個に展開する数（画面を約 7 割埋める）。
    pub count: usize,
    pub orb_size: f32,
    pub blur: f32,
    pub seed: u64,
    /// 動画用の長さ（ms）。Png では参照されない。
    pub duration_ms: u64,
}

/// デフォルト 10 案セット (v0.3.0 preset)。
///
/// 構成: 静止 4 + 動画 6。差別化軸は方向 4 / 速度 2 / count / orb_size / blur のみ。
/// 色は入力画像から拾った kmeans 結果をそのまま使い、preset では一切改変しない。
/// ラベルは `kind_direction_特徴` 形式（snapshot_* / flow_* prefix）。warm / cool /
/// aurora / dream / hi_key / dark_mood のような色ラベルは廃止。
///
/// 各 spec の数値根拠は GitHub Issue #41 の preset 表を参照。
pub const DEFAULT_VARIATIONS: &[VariationSpec] = &[
    VariationSpec {
        label: "snapshot_lr_dense",
        kind: VariationKind::Png,
        direction: MotionDirection::LeftToRight,
        speed: MotionSpeed::Slow,
        count: 25,
        orb_size: 3.0,
        blur: 0.5,
        seed: 1,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "snapshot_rl_huge",
        kind: VariationKind::Png,
        direction: MotionDirection::RightToLeft,
        speed: MotionSpeed::VerySlow,
        count: 12,
        orb_size: 4.5,
        blur: 0.6,
        seed: 2,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "snapshot_tb_fine",
        kind: VariationKind::Png,
        direction: MotionDirection::TopToBottom,
        speed: MotionSpeed::Slow,
        count: 30,
        orb_size: 2.5,
        blur: 0.4,
        seed: 3,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "snapshot_bt_blurry",
        kind: VariationKind::Png,
        direction: MotionDirection::BottomToTop,
        speed: MotionSpeed::VerySlow,
        count: 20,
        orb_size: 3.5,
        blur: 0.8,
        seed: 4,
        duration_ms: 6000,
    },
    VariationSpec {
        label: "flow_lr_slow",
        kind: VariationKind::Mp4,
        direction: MotionDirection::LeftToRight,
        speed: MotionSpeed::Slow,
        count: 20,
        orb_size: 3.0,
        blur: 0.5,
        seed: 5,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "flow_rl_very_slow",
        kind: VariationKind::Mp4,
        direction: MotionDirection::RightToLeft,
        speed: MotionSpeed::VerySlow,
        count: 15,
        orb_size: 3.8,
        blur: 0.6,
        seed: 6,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "flow_tb_dense",
        kind: VariationKind::Mp4,
        direction: MotionDirection::TopToBottom,
        speed: MotionSpeed::Slow,
        count: 28,
        orb_size: 2.8,
        blur: 0.5,
        seed: 7,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "flow_bt_blurry",
        kind: VariationKind::Mp4,
        direction: MotionDirection::BottomToTop,
        speed: MotionSpeed::VerySlow,
        count: 18,
        orb_size: 3.5,
        blur: 0.7,
        seed: 8,
        duration_ms: 8000,
    },
    VariationSpec {
        // 「小粒・高密度・LR」の特徴的な spec。flow_lr_slow (count 20 / size 3.0
        // / blur 0.5) との差別化を強めるため、count を倍以上に増やし orb サイズ
        // と blur を半分に絞った別物にする。
        label: "flow_lr_dense_small",
        kind: VariationKind::Mp4,
        direction: MotionDirection::LeftToRight,
        speed: MotionSpeed::Slow,
        count: 50,
        orb_size: 1.5,
        blur: 0.3,
        seed: 9,
        duration_ms: 8000,
    },
    VariationSpec {
        label: "flow_rl_huge",
        kind: VariationKind::Mp4,
        direction: MotionDirection::RightToLeft,
        speed: MotionSpeed::Slow,
        count: 10,
        orb_size: 5.0,
        blur: 0.6,
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
    fn labels_have_no_color_axis() {
        // warm / cool / aurora / dream / hi_key / dark_mood / drift / glow / mist 等の
        // 旧色ラベル / 旧 prefix が残っていないことを保証する。差別化軸は kind /
        // direction / 特徴量のみ。
        const FORBIDDEN: &[&str] = &[
            "warm",
            "cool",
            "aurora",
            "dream",
            "hi_key",
            "dark_mood",
            "drift",
            "glow",
            "mist",
        ];
        for s in DEFAULT_VARIATIONS {
            for tag in FORBIDDEN {
                assert!(
                    !s.label.contains(tag),
                    "label {:?} contains forbidden color/legacy tag {tag:?}",
                    s.label
                );
            }
        }
    }

    #[test]
    fn count_is_in_screen_filling_range() {
        // 「画面を 7 割埋める」狙いをデフォルトとしつつ、特徴的な spec は
        // 50 個の小粒・高密度なども許容する。10..=50 のレンジで運用する。
        for s in DEFAULT_VARIATIONS {
            assert!(
                (10..=50).contains(&s.count),
                "spec {:?} has count {} outside the 10..=50 range",
                s.label,
                s.count
            );
        }
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
