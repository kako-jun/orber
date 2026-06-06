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
    /// 動画用の長さ（ms）。Png では参照されない（Png は 0 がカノニカル）。
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
        duration_ms: 0,
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
        duration_ms: 0,
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
        duration_ms: 0,
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
        duration_ms: 0,
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

/// GUI のバッチ後半を `VariationKind::Mp4` 枠にする件数の既定値。
///
/// `crates/wasm` の `get_render_data`（`direction_for_spec_idx` / `speed_for_spec_idx`）と
/// Web フロント (`web/src/components/Studio.tsx`) の両方が参照する。GUI は
/// #61 で 12 枚統一になったため、後半 4 タイル (= 12 - 4 = 8 枚静止 + 4 枚
/// 動画) を動画枠で固定する。この定数を軸に各層が
/// `still_count = total - GUI_VIDEO_COUNT_DEFAULT` を計算する。
///
/// 4 にしている理由（#59）: 動画タイル 4 枚に LR / RL / TB / BT を **重複なく
/// 1 枚ずつ** 割り当てて「全方向揃い踏み」の見せ場にするため。direction の
/// 上書きは `direction_for_spec_idx` が `video_idx = spec_idx -
/// still_count` を [`GUI_VIDEO_DIRECTIONS`] で引いて決定的に行う。
pub const GUI_VIDEO_COUNT_DEFAULT: usize = 4;

/// GUI バッチ後半 (= 動画タイル) の direction 並び。`video_idx = spec_idx -
/// still_count` で添字すると LR / RL / TB / BT が 1 枚ずつ重複なく取れる。
///
/// 配列長は `GUI_VIDEO_COUNT_DEFAULT` と一致するよう型で固定している。定数を
/// 増やしたら配列長も合わせる必要があり、ズレるとコンパイルが通らなくなる
/// ので「マッチアームの取りこぼし」を構造的に防ぐ目的（#59）。
pub const GUI_VIDEO_DIRECTIONS: [crate::animate::MotionDirection; GUI_VIDEO_COUNT_DEFAULT] = [
    crate::animate::MotionDirection::LeftToRight,
    crate::animate::MotionDirection::RightToLeft,
    crate::animate::MotionDirection::TopToBottom,
    crate::animate::MotionDirection::BottomToTop,
];

/// GUI バッチ後半 (= 動画タイル) の speed 並び。`video_idx = spec_idx -
/// still_count` で添字すると VerySlow / Slow が必ず 2 枚ずつ含まれる。
///
/// 元々は `random_batch_specs` で speed をランダムに引いていたが、それだと
/// 「4 つ全部速い run / 全部遅い run」が偶然発生してガチャ感が薄れる。
/// direction と同様に固定割当して、4 タイルが必ず速度のばらつきを持つよう
/// にする (#77)。alternating 配列にすることで隣接タイルの体感差を最大化。
pub const GUI_VIDEO_SPEEDS: [crate::animate::MotionSpeed; GUI_VIDEO_COUNT_DEFAULT] = [
    crate::animate::MotionSpeed::VerySlow,
    crate::animate::MotionSpeed::Slow,
    crate::animate::MotionSpeed::VerySlow,
    crate::animate::MotionSpeed::Slow,
];

/// GUI バッチ生成用のランダム範囲。
///
/// CLI の固定 preset (`DEFAULT_VARIATIONS`) では各位置の系統が決まっているが、
/// GUI では「ドラッグするたびに違う 12 枚」が欲しい。各軸を一様サンプルする
/// レンジをここに集約しておく（テスト・ドキュメントから参照しやすい形に）。
pub mod random_ranges {
    pub const COUNT_MIN: usize = 10;
    pub const COUNT_MAX: usize = 50;
    pub const ORB_SIZE_MIN: f32 = 1.5;
    pub const ORB_SIZE_MAX: f32 = 5.0;
    // #78: 縁のコントラストを抑え、文字オーバーレイ時の可読性を上げるため
    // ランダム生成の blur 下限を 0.3 → 0.5 に上げる（点光源寄りに偏らせ
    // 中間 stop が中心寄りになるよう促す）。上限は 0.8 → 0.85 にわずかに
    // 拡張して柔らかい側のバリエーションを増やす。
    pub const BLUR_MIN: f32 = 0.5;
    pub const BLUR_MAX: f32 = 0.85;
    pub const DURATION_MS_MIN: u64 = 6000;
    pub const DURATION_MS_MAX: u64 = 10000;
}

/// `seed` から再現可能な `total` 件の `VariationSpec` をランダム生成する。
///
/// 枠（不変）:
/// - 前半 `still_count` 件は `VariationKind::Png`
/// - 残り (`total - still_count`) 件は `VariationKind::Mp4`
///
/// `still_count > total` なら `total` にクランプされる（全件 Png）。
///
/// 各 spec の direction / speed / count / orb_size / blur / seed / duration_ms
/// は [`random_ranges`] の範囲から一様サンプル。`label` は `random_NN` 形式。
///
/// 同じ `seed` なら同じ spec 列が返る（再現性）。GUI からは
/// `Math.random() * 2**48` を渡してドラッグごとに違う spec 列を引かせる想定。
///
/// ## プラットフォーム非依存性（#242）
///
/// 同じ seed で **wasm32（ブラウザ）と native（dev ハーネス）が同じ spec 列を
/// 返す**ことを保証する。rand 0.8 の `usize` 抽選は 32bit / 64bit ターゲットで
/// 消費バイト数も値も変わる（`UniformInt<usize>` が wasm32 では u32、native では
/// u64 を引く）ため、整数レンジの抽選は幅固定の `u32` で行う。wasm32 では
/// `usize` = u32 なので、この固定はブラウザの既存出力と bit-exact 同一
/// （= 本番 GUI の見た目は一切変わらない）。#242 の三者画素比較ハーネスで、
/// native 側がブラウザと別シーンを描いてしまう divergence の修正。
pub fn random_batch_specs(seed: u64, total: usize, still_count: usize) -> Vec<VariationSpec> {
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    let still_count = still_count.min(total);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    // ラベルは leak して 'static にする（VariationSpec.label が &'static str のため）。
    // 呼び出し側は `total <= 50` で運用される（`crates/wasm` で clamp 済み）ので
    // 1 ドロップあたり最大 50 個 × 12 byte 程度の leak。永続実行でも数 MB に
    // 達するには数百万ドロップが必要で、ブラウザのページ寿命では実害なし。
    (0..total)
        .map(|i| {
            let kind = if i < still_count {
                VariationKind::Png
            } else {
                VariationKind::Mp4
            };
            let direction = match rng.gen_range(0..4u8) {
                0 => MotionDirection::LeftToRight,
                1 => MotionDirection::RightToLeft,
                2 => MotionDirection::TopToBottom,
                _ => MotionDirection::BottomToTop,
            };
            let speed = if rng.gen_bool(0.5) {
                MotionSpeed::Slow
            } else {
                MotionSpeed::VerySlow
            };
            // #242: usize 抽選は 32bit/64bit で RNG 列が割れる（doc コメント参照）。
            // u32 固定で wasm32（ブラウザ）と native（dev ハーネス）を一致させる。
            let count = rng
                .gen_range(random_ranges::COUNT_MIN as u32..=random_ranges::COUNT_MAX as u32)
                as usize;
            let orb_size = rng.gen_range(random_ranges::ORB_SIZE_MIN..=random_ranges::ORB_SIZE_MAX);
            let blur = rng.gen_range(random_ranges::BLUR_MIN..=random_ranges::BLUR_MAX);
            let spec_seed: u64 = rng.gen();
            let duration_ms = match kind {
                VariationKind::Png => 0,
                VariationKind::Mp4 => {
                    rng.gen_range(random_ranges::DURATION_MS_MIN..=random_ranges::DURATION_MS_MAX)
                }
            };
            let label: &'static str = Box::leak(format!("random_{:02}", i + 1).into_boxed_str());
            VariationSpec {
                label,
                kind,
                direction,
                speed,
                count,
                orb_size,
                blur,
                seed: spec_seed,
                duration_ms,
            }
        })
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

    #[test]
    fn random_batch_specs_keeps_kind_split() {
        // この split=5 は random_batch_specs 関数の汎用挙動 (前半 still_count 件が
        // Png、それ以降が Mp4) を確認しているだけで、GUI の既定値
        // GUI_VIDEO_COUNT_DEFAULT (=4) とは独立。
        let specs = random_batch_specs(42, 10, 5);
        assert_eq!(specs.len(), 10);
        for s in &specs[..5] {
            assert_eq!(
                s.kind,
                VariationKind::Png,
                "first 5 (still_count) must be PNG"
            );
            assert_eq!(s.duration_ms, 0, "PNG specs must have duration_ms=0");
        }
        for s in &specs[5..] {
            assert_eq!(s.kind, VariationKind::Mp4, "remaining must be MP4");
            assert!(
                (random_ranges::DURATION_MS_MIN..=random_ranges::DURATION_MS_MAX)
                    .contains(&s.duration_ms),
                "MP4 spec duration_ms {} out of range",
                s.duration_ms
            );
        }
    }

    #[test]
    fn gui_video_directions_are_unique_and_cover_all_axes() {
        // #59: 動画タイル 4 枚に LR / RL / TB / BT が 1 枚ずつ重複なく割当てられる。
        // 配列長は型で GUI_VIDEO_COUNT_DEFAULT に同期されている。
        assert_eq!(GUI_VIDEO_DIRECTIONS.len(), GUI_VIDEO_COUNT_DEFAULT);
        use crate::animate::MotionDirection::*;
        assert_eq!(GUI_VIDEO_DIRECTIONS[0], LeftToRight);
        assert_eq!(GUI_VIDEO_DIRECTIONS[1], RightToLeft);
        assert_eq!(GUI_VIDEO_DIRECTIONS[2], TopToBottom);
        assert_eq!(GUI_VIDEO_DIRECTIONS[3], BottomToTop);
        let mut sorted = GUI_VIDEO_DIRECTIONS.to_vec();
        sorted.sort_by_key(|d| match d {
            LeftToRight => 0,
            RightToLeft => 1,
            TopToBottom => 2,
            BottomToTop => 3,
        });
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            GUI_VIDEO_COUNT_DEFAULT,
            "all 4 directions must appear exactly once"
        );
    }

    #[test]
    fn random_batch_specs_in_range() {
        let specs = random_batch_specs(123, 10, 5);
        for s in &specs {
            assert!(
                (random_ranges::COUNT_MIN..=random_ranges::COUNT_MAX).contains(&s.count),
                "count {} out of range",
                s.count
            );
            assert!(
                s.orb_size >= random_ranges::ORB_SIZE_MIN
                    && s.orb_size <= random_ranges::ORB_SIZE_MAX,
                "orb_size {} out of range",
                s.orb_size
            );
            assert!(
                s.blur >= random_ranges::BLUR_MIN && s.blur <= random_ranges::BLUR_MAX,
                "blur {} out of range",
                s.blur
            );
        }
    }

    #[test]
    fn random_batch_specs_is_deterministic_per_seed() {
        let a = random_batch_specs(7, 10, 5);
        let b = random_batch_specs(7, 10, 5);
        assert_eq!(a.len(), b.len());
        for (l, r) in a.iter().zip(b.iter()) {
            assert_eq!(l.kind, r.kind);
            assert_eq!(l.direction, r.direction);
            assert_eq!(l.speed, r.speed);
            assert_eq!(l.count, r.count);
            assert_eq!(l.seed, r.seed);
            assert_eq!(l.duration_ms, r.duration_ms);
        }
    }

    #[test]
    fn random_batch_specs_differ_across_seeds() {
        let a = random_batch_specs(1, 10, 5);
        let b = random_batch_specs(2, 10, 5);
        // 配置の根幹となる spec.seed が 10 件すべて違うことだけ保証する
        // （direction や count は離散レンジが狭いので偶然一致しうる）。
        let any_seed_diff = a.iter().zip(b.iter()).any(|(l, r)| l.seed != r.seed);
        assert!(
            any_seed_diff,
            "different base seed must produce different spec seeds"
        );
    }

    #[test]
    fn random_batch_specs_total_one_still_one_returns_png() {
        // random_batch_specs(_, 1, 1) は 1 件 Png を返す、という関数自体の
        // 決定論的挙動を担保（wasm 側の still_count 計算ルールとは独立）。
        let specs = random_batch_specs(1, 1, 1);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].kind, VariationKind::Png);
    }

    #[test]
    fn random_batch_specs_clamps_still_count_to_total() {
        let specs = random_batch_specs(1, 3, 999);
        assert_eq!(specs.len(), 3);
        for s in &specs {
            assert_eq!(s.kind, VariationKind::Png, "all-still expected");
        }
    }

    #[test]
    fn random_batch_specs_count_extremes_are_inclusive() {
        // 整数レンジ (COUNT_MIN..=COUNT_MAX = 10..=50) は inclusive で呼んでいる
        // 前提。多めの seed 試行で MIN / MAX を踏めることを確認する（将来
        // `..COUNT_MAX` (exclusive) に変えたら 50 が見えなくなって気付く）。
        // 浮動小数のレンジ境界は連続値で確率的にヒットしないため対象外。
        let mut hit_max = false;
        let mut hit_min = false;
        for seed in 0u64..2000 {
            for s in random_batch_specs(seed, 10, 5) {
                if s.count == random_ranges::COUNT_MAX {
                    hit_max = true;
                }
                if s.count == random_ranges::COUNT_MIN {
                    hit_min = true;
                }
            }
        }
        assert!(
            hit_max,
            "COUNT_MAX never reached — range may have become exclusive"
        );
        assert!(hit_min, "COUNT_MIN never reached");
    }

    /// #242: wasm32（ブラウザ）と native で同じ spec 列が出ることのピン留め。
    ///
    /// rand 0.8 の `usize` 抽選は 32bit/64bit ターゲットで divergent なため、
    /// `random_batch_specs` は count 抽選を u32 固定にしている。このピンは
    /// **wasm32 が従来から返していた値**（= ブラウザの A/B キャプチャ
    /// seed=42 / n=12 / still=8 実測条件）を native でも返すことを固定する。
    /// 値を変える変更（レンジ変更・抽選順変更）をしたら、wasm32 側と同時に
    /// ここも更新すること（プラットフォーム間で割れたらこのテストでは
    /// 気づけない点に注意。割れの検出は #242 の ab_dump / ab_diff ハーネス）。
    #[test]
    fn random_batch_specs_pins_wasm32_sequence() {
        let specs = random_batch_specs(42, 12, 8);
        // A/B ハーネスが使う video spec（spec_idx=8）: 配置の根幹 spec.seed と count。
        assert_eq!(specs[8].count, 38);
        assert_eq!(specs[8].seed, 0x10cc204f3db50cf9);
        // 先頭 spec も押さえる（列全体の開始位置がずれたら即検出）。
        assert_eq!(specs[0].count, 27);
        assert_eq!(specs[0].seed, 0x49e149d8bcb642b0);
    }

    #[test]
    fn random_batch_specs_directions_diversify() {
        // seed 42 / 10 件で 4 方向のうち少なくとも 2 種類は出る（多様性の最低ライン）。
        let specs = random_batch_specs(42, 10, 5);
        let mut seen = [false; 4];
        for s in &specs {
            let idx = match s.direction {
                MotionDirection::LeftToRight => 0,
                MotionDirection::RightToLeft => 1,
                MotionDirection::TopToBottom => 2,
                MotionDirection::BottomToTop => 3,
            };
            seen[idx] = true;
        }
        let unique = seen.iter().filter(|b| **b).count();
        assert!(
            unique >= 2,
            "expected at least 2 distinct directions, got {unique}"
        );
    }
}
