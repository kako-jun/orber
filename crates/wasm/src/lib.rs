//! WASM bindings for orber-core. Exposes the rendering pipeline to browsers.
//!
//! 画像デコードは JS 側に任せる: 呼び出し側は `<canvas>` / `ImageData` で
//! 生 RGB バイトを取り出して `WasmParams.source_rgb` に詰めて渡す。core クレート
//! は wasm バンドルサイズ削減のため PNG デコード以外を積まない。
//!
//! ## API の責務分離（#225 以降）
//!
//! CPU 描画は撲滅され、wasm は **データ供給と WGSL canvas 描画**を担う。
//!
//! - `get_glyph_sdf`: グリフ 1 文字の SDF テクスチャ（`Uint8Array`）を返す。
//! - `glyph_supported`: 同梱フォントに文字が収録されているかの判定。
//!
//! ## WebGPU canvas present 経路（#230 / #231）
//!
//! [`gpu`] モジュール（wasm32 専用）が `gpu_init` / `gpu_set_render_data` /
//! `gpu_render` / `gpu_resize` を公開する。spec / preset / kmeans の解決は
//! [`resolve_frame`] に集約し、orb / glyph / image の 3 shape を
//! `OrbShape` まで解決して [`build_gpu_render_inputs`] が clusters +
//! `AnimateOptions`（+ orb 用 pack）を構築する。描画は orber-core の
//! `GpuRenderer`（WGSL）が `opts.shape` 別の `render_packed_to_view` /
//! `render_frame_*_to_view` で canvas surface の frame view に直接行う。
//! WebGPU 必須・fallback 無し（#207 方針）。

// #247: MAX_DIM を読む validate_params は供給系（wasm32 / test 限定）に閉じたため対象外化。
#[cfg(any(target_arch = "wasm32", test))]
const MAX_DIM: u32 = 8192;

use orber_core::animate::{MotionDirection, MotionSpeed, SHADOW_STRENGTH_DEFAULT};
// core の pack ヘルパは #247 で pack_render_data に改名されたが、wasm 側にも同名の
// 薄いラッパ（下記）があるため import せず完全修飾 orber_core::animate::pack_render_data
// で呼んで名前衝突を避ける。
// AnimateOptions / image_rgba_to_sdf / Arc は GPU(WGSL) 経路専用（wasm32 / test のみ）。
#[cfg(any(target_arch = "wasm32", test))]
use orber_core::animate::AnimateOptions;
// #239 Phase 1: 製品の 3 段にじみボタンを GPU(WGSL) 経路の AnimateOptions.aqua に
// 流すための型。GPU 経路専用（wasm32 / test のみ）。
#[cfg(any(target_arch = "wasm32", test))]
use orber_core::animate::AquaBleedConfig;
use orber_core::cluster::Cluster;
// kmeans 系（derive_background_rgba / drop_dominant / extract_clusters）は供給系
// get_or_build_clusters（wasm32 / test 限定）からのみ使う（#247）。
#[cfg(any(target_arch = "wasm32", test))]
use orber_core::cluster::{derive_background_rgba, drop_dominant, extract_clusters};
#[cfg(any(target_arch = "wasm32", test))]
use orber_core::glyph::image_rgba_to_sdf;
use orber_core::glyph::{has_glyph, render_glyph_sdf, GlyphFontId};
// OrbShape は形状解決（parse_shape / resolve_orb_shape、wasm32 / test 限定）から
// のみ使う（#247）。
#[cfg(any(target_arch = "wasm32", test))]
use orber_core::orb::OrbShape;
use orber_core::style::SoftnessPreset;
// spec 再構築・video 領域の固定割当（供給系 resolve_frame / spec_idx ヘルパ、
// wasm32 / test 限定）からのみ使う（#247）。
#[cfg(any(target_arch = "wasm32", test))]
use orber_core::variations::{
    random_batch_specs, VariationSpec, GUI_VIDEO_COUNT_DEFAULT, GUI_VIDEO_DIRECTIONS,
    GUI_VIDEO_SPEEDS,
};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(any(target_arch = "wasm32", test))]
use std::sync::Arc;
use std::sync::OnceLock;
use wasm_bindgen::prelude::*;

/// #230: ブラウザ WebGPU canvas present 経路。wasm32 専用 cfg なので native の
/// `cargo test -p orber-wasm` ではコンパイルされない（web-sys / wgpu も
/// target dependency で wasm32 にしか張られていない）。
#[cfg(target_arch = "wasm32")]
pub mod gpu;

/// orb 数の上限。core::animate::MAX_ORB_COUNT と一致させる必要がある。
/// #247: get_render_data 撤去後、この定数を読む供給系（resolve_frame）は
/// WGSL canvas-present（wasm32）と test だけ。native build では対象外にして
/// dead_code を避ける（後続の供給系ヘルパも同方針）。
#[cfg(any(target_arch = "wasm32", test))]
const MAX_ORB_COUNT: usize = 1024;

/// per-orb pack の本数の上限（旧来の固定 uniform-array レンダラ由来の 64）。
/// ここで超過を早期エラーにし、黙って切り詰められるのを防ぐ。GUI の
/// `random_ranges::COUNT_MAX = 50` を網羅する余裕として 64 を採る。
/// WGSL canvas-present 経路はデータテクスチャ経路なのでこの上限を必要としないが、
/// GUI の count 上限（high=24, #265）を十分上回るため当面同一バリデーションで揃えておく。
/// #247: 読む経路（resolve_frame）が wasm32 / test 限定になったため同様に対象外化。
#[cfg(any(target_arch = "wasm32", test))]
const GL_RENDERER_MAX_ORBS: usize = 64;

/// パニック時にブラウザコンソールへスタックトレースを出すためのフック。
#[wasm_bindgen(start)]
pub fn init_panic_hook() {
    console_error_panic_hook::set_once();
}

/// JS 側から渡されるレンダリングパラメータ。
///
/// `source_rgb` は **正確に** `source_width * source_height * 3` バイトの
/// 行優先 (R,G,B,R,G,B,...) でなければならない。
/// `seed` は f64 で受ける（JS の Number 互換、BigInt を強制したくない）。
/// 内部で `as u64` キャスト。実用上 `Number.MAX_SAFE_INTEGER` までは無損失。
#[derive(Deserialize)]
pub struct WasmParams {
    pub source_rgb: Vec<u8>,
    pub source_width: u32,
    pub source_height: u32,
    pub k: usize,
    pub width: u32,
    pub height: u32,
    pub seed: f64,
    pub direction: String,
    pub speed: String,
    pub count: usize,
    pub orb_size: f32,
    pub blur: f32,
    pub shape: String,
    /// Glyph 文字（`shape == "glyph"` のときのみ意味を持つ）。Unicode scalar
    /// 1 文字。空文字や複数 scalar は呼び出し側で reject すること
    /// （wasm 側では先頭 char をそのまま採用する）。Phase B (#55) で追加。
    /// 既存呼び出しの後方互換のため `default = ""`、`default` で `'☆'` を採用しない
    /// （Glyph 経路に入る前提で呼び出し側が必ず指定するため）。
    #[serde(default)]
    pub glyph_char: String,
    /// `count` の preset 上書き。`""` で無視（spec.count を使う）。
    /// Phase B (#55) で追加。`"low" | "mid" | "high"` のいずれかなら
    /// 10/20/24 を spec.count に上書きしてからレンダリングする（#265: high 30→24）。
    #[serde(default)]
    pub count_preset: String,
    /// `speed` の preset 上書き。`""` で無視（spec.speed と GUI_VIDEO_SPEEDS を使う）。
    /// Phase B (#55) で追加。`"slow" | "mid" | "fast"` のみ受け付ける。
    #[serde(default)]
    pub speed_preset: String,
    /// `softness` の preset。`""` で `Mid` (既存挙動と同値)。Phase B (#55) で追加。
    #[serde(default)]
    pub softness_preset: String,
    /// #239 Phase 1: にじみ (watercolor bleed) の製品 3 段ボタン preset。
    /// CLI の `--bleed weak|mid|strong` と同義で、`"weak" | "mid" | "strong"` を
    /// 内部 `aqua_bleed` 0.15 / 0.3 / 0.5 に写像し、orb / glyph / image どの shape
    /// にも空間ブラーを乗せる。`""`（既定）は「にじみオフ＝くっきり」で、
    /// `AnimateOptions.aqua` を `None` に保つ＝従来の Web 出力と byte-identical
    /// （非リグレッションゲート）。
    /// bloom / offset / halo は各軸の専用 3 段ボタン（下記）で決まる。`bleed_preset`
    /// が `""`（にじみオフ）なら 3 軸とも無視され 0 になる。
    #[serde(default)]
    pub bleed_preset: String,
    /// #239 Phase 1: ブルーム（芯の光）の製品 3 段ボタン preset。CLI の
    /// `--bloom weak|mid|strong` と同義で、`"weak" | "mid" | "strong"` を内部
    /// `aqua_bloom` 0.3 / 0.6 / 0.9 に写像する。`""`（既定）はその軸オフ＝0。にじみ
    /// （`bleed_preset != ""`）のときだけ効く（にじみ無しでは `aqua = None` なので
    /// 3 軸とも無視される）。数字は出さない製品 UI ではこの対応表だけが強さの正本。
    #[serde(default)]
    pub bloom_preset: String,
    /// #239 Phase 1: ハロー（縁の彩度）の製品 3 段ボタン preset。CLI の
    /// `--halo weak|mid|strong` と同義。`"weak" | "mid" | "strong"` → `aqua_halo`
    /// 0.3 / 0.6 / 0.9。`""`（既定）はオフ＝0。`bloom_preset` と同じ依存条件
    /// （にじみ engage 時のみ有効）。
    #[serde(default)]
    pub halo_preset: String,
    /// #239 Phase 1: オフセット（にじみのかたより）の製品 3 段ボタン preset。CLI の
    /// `--offset weak|mid|strong` と同義。`"weak" | "mid" | "strong"` → `aqua_offset`
    /// 0.3 / 0.6 / 0.9。`""`（既定）はオフ＝0。`bloom_preset` と同じ依存条件
    /// （にじみ engage 時のみ有効）。
    #[serde(default)]
    pub offset_preset: String,
    /// Glyph 形状時に per-orb 回転をアニメーションさせるか（#136）。
    /// `true` で従来挙動、`false` で全 t において base_angle を保つ静止描画。
    /// Orb 形状では使われない。`#[serde(default = "default_glyph_rotate")]`
    /// で省略時は `true`（従来挙動互換）。既存の wasm caller が `glyph_rotate`
    /// フィールドを送っていなくても `true` でデシリアライズされるため影響を受けない。
    #[serde(default = "default_glyph_rotate")]
    pub glyph_rotate: bool,
    /// 画像マスク RGBA（`shape == "image"` のときのみ意味を持つ、#231）。
    /// `image_mask_width * image_mask_height * 4` バイトの行優先
    /// (R,G,B,A,...)。core の [`image_rgba_to_sdf`] でシルエット SDF に変換して
    /// WGSL の SDF orb 経路に食わせる（#219 / #235、単パス）。`shape == "image"`
    /// のときだけ必須・検証する。CLI の `--image-mask`（デコード後の RGBA）に対応。
    /// Web の WebGL 経路（`generateImageSdf`）はこのフィールドを使わない（不変）。
    #[serde(default)]
    pub image_mask_rgba: Vec<u8>,
    /// 画像マスクの幅（px、#231）。`shape == "image"` のときのみ意味を持つ。
    #[serde(default)]
    pub image_mask_width: u32,
    /// 画像マスクの高さ（px、#231）。`shape == "image"` のときのみ意味を持つ。
    #[serde(default)]
    pub image_mask_height: u32,
    /// JS 側で生成した glyph SDF（`shape == "glyph"` のフォールバック、#231）。
    /// 同梱フォント（Noto Sans Symbols 2 サブセット＝記号のみ）に収録されていない
    /// 文字（ひらがな・漢字・絵文字）は wasm の `get_glyph_sdf` で描けないため、Web は
    /// `generateJsGlyphSdf`（OffscreenCanvas + OS フォントスタック、`web/src/lib/jsGlyphSdf.ts`）
    /// で `glyph_sdf_size * glyph_sdf_size` バイトの SDF を作ってここに乗せる（#159 の
    /// WebGL フォールバックと同じ設計＝「ユーザーが入れた字を尊重して描画する」）。非空なら
    /// `resolve_orb_shape` がこの SDF をシルエットとして [`OrbShape::Image`] に解決する
    /// （core 統一機構では glyph も image も同じ SDF 経路、#235）。空なら従来どおり core
    /// フォント経路（[`OrbShape::Glyph`]）。spec / preset を解決する [`resolve_frame`] は
    /// このフィールドを読まない（不変）。
    #[serde(default)]
    pub glyph_sdf: Vec<u8>,
    /// `glyph_sdf` の一辺サイズ（px、#231）。`glyph_sdf.len() == glyph_sdf_size *
    /// glyph_sdf_size` でなければ `resolve_orb_shape` が Err にする。`get_glyph_sdf` と
    /// 同じ 16..=1024 の範囲のみ受理する。`glyph_sdf` が空のときは無視される。
    #[serde(default)]
    pub glyph_sdf_size: u32,
    /// #241「薄い影」強度（0..1）の **dev チューニング上書き**。省略時は製品定数
    /// [`SHADOW_STRENGTH_DEFAULT`]（= 製品と同じ見た目）。0.0..=1.0 の範囲外は
    /// `validate_params` が reject する（0.0 と 1.0 は inclusive で受理）。
    /// gpu-lab（`web/src/pages/gpu-lab.astro`）のスライダー専用 — 本番 Studio は
    /// このフィールドを送らない（製品は定数で固定、kako-jun の実機選定の足場）。
    /// orb 機構の全 shape（orb / glyph / image）に効く。
    #[serde(default = "default_shadow_strength")]
    pub shadow_strength: f32,
    /// 透過 export（#56 / #245）: `true` なら解決済み背景色の alpha を 0 に
    /// 上書きして「透過背景でレンダリングしてくれ」とシェーダに依頼する。
    /// 旧 WebGL 経路で worker が pack header word 3（bg.a）を JS 側で 0 に
    /// 書き換えていた `withTransparentBackground` の wasm 入口版（WGSL 経路は
    /// pack が wasm 内部で完結するため、JS からは触れない）。orb は pack
    /// header[3]、glyph / image は `AnimateOptions.background[3]`
    /// 経由で効く。serde default は `false`（既存呼び出しはバイト列不変）。
    #[serde(default)]
    pub transparent_background: bool,
}

/// `glyph_rotate` の serde default。既存呼び出しが省略しても従来挙動を保つために `true`。
fn default_glyph_rotate() -> bool {
    true
}

/// `shadow_strength` の serde default（#241）。省略時は製品定数 = 製品と同じ見た目。
fn default_shadow_strength() -> f32 {
    SHADOW_STRENGTH_DEFAULT
}

/// #241: 製品定数 [`SHADOW_STRENGTH_DEFAULT`] を JS に公開する。gpu-lab の shadow
/// スライダーが既定値をこの値に同期するためだけの dev 向け export（値の正本は
/// core::animate の定数 1 箇所のまま。HTML 側にハードコードして drift させない）。
#[wasm_bindgen]
pub fn shadow_strength_default() -> f32 {
    SHADOW_STRENGTH_DEFAULT
}

// Pure parsers/validators return String errors so they can be unit-tested on
// the host (non-wasm) target where JsError can't be constructed.

/// Phase B (#55): preset 文字列を `Option<MotionSpeed>` に変換。
///
/// UI 経路は `slow` / `mid` / `fast` の **3 値のみ** を受理する。空文字だけが
/// identity（= 上書きしない、`Ok(None)`）で、`spec.speed` と `GUI_VIDEO_SPEEDS`
/// の固定割当を温存する。明示選択時は:
/// - `slow` => `VerySlow`
/// - `mid` => `Slow`
/// - `fast` => `Mid`
/// - `Fast` は CLI 専用に格下げされ、GUI では露出しない。
#[cfg(any(target_arch = "wasm32", test))]
fn parse_speed_preset(s: &str) -> Result<Option<MotionSpeed>, String> {
    match s {
        // identity: spec.speed / GUI_VIDEO_SPEEDS を温存
        "" => Ok(None),
        "slow" => Ok(Some(MotionSpeed::VerySlow)),
        "mid" => Ok(Some(MotionSpeed::Slow)),
        "fast" => Ok(Some(MotionSpeed::Mid)),
        other => Err(format!(
            "invalid speed_preset: {other} (expected one of '' / slow / mid / fast)"
        )),
    }
}

/// Phase B (#55): count preset 文字列を絶対値に変換。`""` は `Ok(None)` で
/// 「上書きしない（spec.count を使う）」を意味する。値は GUI 仕様に合わせて
/// low=10 / mid=20 / high=24 で固定（#265: 「多め」=30 がモバイルで少し重かった
/// ため 30→24 に下げた。標準 20 より明確に多いが、48→5 タップ削減と併せて軽量化）。
#[cfg(any(target_arch = "wasm32", test))]
fn parse_count_preset(s: &str) -> Result<Option<usize>, String> {
    match s {
        "" => Ok(None),
        "low" => Ok(Some(10)),
        "mid" => Ok(Some(20)),
        "high" => Ok(Some(24)),
        other => Err(format!(
            "invalid count_preset: {other} (expected one of '' / low / mid / high)"
        )),
    }
}

/// Phase B (#55): softness preset 文字列を `SoftnessPreset` に変換。空文字 /
/// "mid" は既存挙動と完全同値の `Mid`。
#[cfg(any(target_arch = "wasm32", test))]
fn parse_softness_preset(s: &str) -> Result<SoftnessPreset, String> {
    match s {
        "" | "mid" => Ok(SoftnessPreset::Mid),
        "low" => Ok(SoftnessPreset::Low),
        "high" => Ok(SoftnessPreset::High),
        other => Err(format!(
            "invalid softness_preset: {other} (expected one of '' / low / mid / high)"
        )),
    }
}

/// #239 Phase 1: にじみ (watercolor bleed) preset 文字列を内部 `aqua_bleed` 量へ
/// 写像する。`""`（既定）は識別子なし＝「にじみオフ（くっきり）」で `Ok(None)`、
/// `AnimateOptions.aqua` が `None` のまま従来の Web 出力を byte-identical に保つ
/// （非リグレッションゲート）。`weak | mid | strong` を CLI の
/// `CliBleedPreset::to_bleed` と同じ 0.15 / 0.3 / 0.5 に写す（数字を出さない製品
/// UI ではこの対応表だけがにじみ強さの正本）。GPU(WGSL) 経路専用（wasm32 / test
/// のみコンパイル）。
#[cfg(any(target_arch = "wasm32", test))]
fn parse_bleed_preset(s: &str) -> Result<Option<f32>, String> {
    match s {
        // identity: にじみオフ（くっきり）。aqua = None を保つ。
        "" => Ok(None),
        "weak" => Ok(Some(0.15)),
        "mid" => Ok(Some(0.3)),
        "strong" => Ok(Some(0.5)),
        other => Err(format!(
            "invalid bleed_preset: {other} (expected one of '' / weak / mid / strong)"
        )),
    }
}

/// #239 Phase 1: bloom / halo / offset の character 3 段ボタン preset 文字列を内部
/// 係数へ写像する。にじみと同形の `weak | mid | strong` を CLI の
/// `CliCharacterPreset::to_coef` と同じ 0.3 / 0.6 / 0.9 に写す。`""`（既定）はその軸
/// オフ＝0.0（`Ok(0.0)`）で、にじみが engage していても当該軸は無効。`label` は
/// エラー文言用（"bloom" / "halo" / "offset"）。GPU(WGSL) 経路専用（wasm32 / test
/// のみコンパイル）。
#[cfg(any(target_arch = "wasm32", test))]
fn parse_character_preset(s: &str, label: &str) -> Result<f32, String> {
    match s {
        "" => Ok(0.0),
        "weak" => Ok(0.3),
        "mid" => Ok(0.6),
        "strong" => Ok(0.9),
        other => Err(format!(
            "invalid {label}_preset: {other} (expected one of '' / weak / mid / strong)"
        )),
    }
}

/// Phase B (#55): "glyph" 形状時の文字列から先頭 char を取り出す。空文字なら
/// エラー。複数 char でも先頭の Unicode scalar のみ採用する
/// （UI 側で 1 文字制限済みの想定）。
fn first_char_of(s: &str) -> Result<char, String> {
    s.chars()
        .next()
        .ok_or_else(|| "glyph_char is empty (expected exactly 1 character)".to_string())
}

/// shape 文字列を [`OrbShape`] に解決する（#231）。
///
/// `image` は画像マスク RGBA + 幅 / 高さを必要とするため、[`WasmParams`] 全体を受ける
/// [`resolve_orb_shape`] で解決する（`parse_shape` の引数は文字列だけなので image を
/// 直接扱えない）。`parse_shape` 内で `"image"` を受けると Err にして、必ず
/// `resolve_orb_shape` 経由を強制する。
#[cfg(any(target_arch = "wasm32", test))]
fn parse_shape(s: &str, glyph_char: &str) -> Result<OrbShape, String> {
    match s {
        "orb" => Ok(OrbShape::Orb),
        "glyph" => {
            let ch = first_char_of(glyph_char)?;
            Ok(OrbShape::Glyph {
                ch,
                font: GlyphFontId::NotoSymbols2,
            })
        }
        "image" => Err(
            "shape 'image' must be resolved via resolve_orb_shape (needs image_mask_rgba)"
                .to_string(),
        ),
        other => Err(format!(
            "invalid shape: {other} (expected 'orb' / 'glyph' / 'image')"
        )),
    }
}

/// [`WasmParams`] から GPU(WGSL) 経路用の [`OrbShape`] を解決する（#231）。
///
/// `orb` は [`parse_shape`] に委譲する。`image` は
/// `image_mask_rgba` (+ width / height) を core の [`image_rgba_to_sdf`]（#219、Web の
/// `generateImageSdf` と同フォーマット）でシルエット SDF に変換して
/// [`OrbShape::Image`] を作る。SDF サイズは [`DEFAULT_GLYPH_SDF_SIZE`] = 256（CLI の
/// `resolve_image_shape` と同値）。マスクが空 / 単色でコントラストが取れない場合は Err。
///
/// `glyph` は 2 経路: (1) `glyph_sdf` が非空なら JS フォールバック（同梱フォント外の
/// ひらがな・漢字・絵文字を `generateJsGlyphSdf` で SDF 化したもの）を
/// [`resolve_glyph_sdf_shape`] で検証して [`OrbShape::Image`] に解決する（core 統一機構
/// では glyph も image も同じ SDF シルエット経路、#235）。(2) 空なら従来どおり core
/// フォント経路（[`parse_shape`] → [`OrbShape::Glyph`]）。
///
/// GPU(WGSL) 経路専用（wasm32 / test のみコンパイル）。
#[cfg(any(target_arch = "wasm32", test))]
fn resolve_orb_shape(p: &mut WasmParams) -> Result<OrbShape, String> {
    if p.shape == "image" {
        return resolve_image_shape(p);
    }
    // #231: shape == "glyph" で JS フォールバック SDF が供給されていれば、同梱フォントに
    // 無い字（ひらがな・漢字・絵文字）でも image と同じ SDF 経路で描く。
    if p.shape == "glyph" && !p.glyph_sdf.is_empty() {
        return resolve_glyph_sdf_shape(p);
    }
    parse_shape(&p.shape, &p.glyph_char)
}

/// `shape == "glyph"` で JS 生成 SDF が供給されたときの解決（#231）。
///
/// `glyph_sdf` は Web の [`generateJsGlyphSdf`](web/src/lib/jsGlyphSdf.ts) が
/// OffscreenCanvas + OS フォントスタックで作った `glyph_sdf_size * glyph_sdf_size` バイトの
/// SDF（`get_glyph_sdf` / core の `render_glyph_sdf` と同じ符号・正規化）。core 統一機構
/// （#235）では glyph も image も同じ SDF シルエットなので、ここでは検証してから
/// [`OrbShape::Image`] として解決する（`resolve_image_shape` と同じ shape を使う）。
///
/// 検証は `get_glyph_sdf` と同じ `16..=1024` の size 範囲、`glyph_sdf.len() ==
/// size * size`。SDF バイト列は clone せず `std::mem::take` で奪う（image_mask と同方針。
/// resolve_frame は glyph_sdf を読まないので奪っても安全）。
///
/// GPU(WGSL) 経路専用（wasm32 / test のみコンパイル）。
#[cfg(any(target_arch = "wasm32", test))]
fn resolve_glyph_sdf_shape(p: &mut WasmParams) -> Result<OrbShape, String> {
    let size = p.glyph_sdf_size;
    if !(16..=1024).contains(&size) {
        return Err(format!("glyph_sdf_size must be in [16, 1024], got {size}"));
    }
    let expected = (size as usize) * (size as usize);
    if p.glyph_sdf.len() != expected {
        return Err(format!(
            "glyph_sdf length {} does not match glyph_sdf_size * glyph_sdf_size ({expected})",
            p.glyph_sdf.len()
        ));
    }
    // image_mask と同じく clone せず take で奪う（このあと resolve_frame は glyph_sdf を
    // 読まないので安全）。
    let sdf = std::mem::take(&mut p.glyph_sdf);
    Ok(OrbShape::Image {
        sdf: Arc::from(sdf),
        size,
    })
}

/// `shape == "image"` の解決: `image_mask_rgba` (w × h × 4 バイト) を
/// [`OrbShape::Image`] のシルエット SDF に変換する（#231）。CLI の
/// `resolve_image_shape`（`image_rgba_to_sdf` を叩く）と同じ heuristic で、
/// Web の WebGL 経路（`generateImageSdf`）とも同フォーマットの SDF を作る。
///
/// GPU(WGSL) 経路専用（wasm32 / test のみコンパイル）。
#[cfg(any(target_arch = "wasm32", test))]
fn resolve_image_shape(p: &mut WasmParams) -> Result<OrbShape, String> {
    if p.image_mask_width == 0 || p.image_mask_height == 0 {
        return Err(
            "shape 'image' requires image_mask_width / image_mask_height > 0 (the silhouette mask)"
                .to_string(),
        );
    }
    let expected = (p.image_mask_width as usize) * (p.image_mask_height as usize) * 4;
    if p.image_mask_rgba.len() != expected {
        return Err(format!(
            "image_mask_rgba length {} does not match image_mask_width * image_mask_height * 4 ({expected})",
            p.image_mask_rgba.len()
        ));
    }
    // p を所有する呼び出し元（build_gpu_render_inputs）から &mut で受けているので、
    // mask バイト列は clone せず take で奪う（このあと resolve_frame は image_mask_rgba を
    // 読まないので奪っても安全。レビュー指摘の余計な alloc 解消）。
    let rgba = image::RgbaImage::from_raw(
        p.image_mask_width,
        p.image_mask_height,
        std::mem::take(&mut p.image_mask_rgba),
    )
    .ok_or_else(|| "image_mask_rgba could not be wrapped as an RgbaImage".to_string())?;
    let size = orber_core::glyph::DEFAULT_GLYPH_SDF_SIZE;
    // この Err 文言の "no usable silhouette contrast" は Web worker が sentinel
    // `image-shape-no-contrast` へのマップ判定に includes で使う（#169 型の
    // 文字列 drift を防ぐため、変えるなら両方同時に変えること。固定テスト:
    // image_no_contrast_error_wording_is_pinned）。
    // SYNC WITH web/src/lib/orberWorker.ts::setRenderData
    let sdf = image_rgba_to_sdf(&rgba, size).ok_or_else(|| {
        "image_mask has no usable silhouette contrast (it is blank or a single flat color); \
         provide a mask with a distinct subject vs. background"
            .to_string()
    })?;
    Ok(OrbShape::Image {
        sdf: Arc::from(sdf),
        size,
    })
}

#[cfg(any(target_arch = "wasm32", test))]
fn build_source_image(p: &mut WasmParams) -> Result<image::RgbImage, String> {
    let rgb = std::mem::take(&mut p.source_rgb);
    image::RgbImage::from_raw(p.source_width, p.source_height, rgb).ok_or_else(|| {
        "source_rgb length does not match source_width * source_height * 3".to_string()
    })
}

/// kmeans 結果のキャッシュ。同じソース画像 + 同じ K なら kmeans を skip する。
///
/// Android 計測 (kako-jun, 2026-05-01) で `extract_clusters` が 1 spec あたり
/// ~3 秒かかり、12 stills + 4 mp4 = 16 呼び出しで合計 ~50 秒のロスになって
/// いた（PC では合計 ~1 秒）。kmeans 結果はソース画像が変わらない限り同じ
/// なので、(source_rgb の長さ + 4 隅 8 byte サンプル + width + height + k)
/// を fingerprint にして再利用する。
///
/// レビュー S1: 旧実装は `static mut Option<CachedClusters>` で
/// `#[allow(static_mut_refs)]` を必要としていた。Rust 2024 以降の lint 強化で
/// 将来の事故源になるため `OnceLock<WasmSingleThreadCell<...>>` に移行する。
/// **wasm は single-threaded** なので `RefCell` の borrow 衝突は構造的に
/// 起きないが、`RefCell` 自体が `Sync` ではないため、wasm 専用の薄い
/// ラッパで `Sync`/`Send` を手動 impl する（unsafe 境界は **このラッパ
/// 1 か所だけ**に閉じ込める）。worker を複数起動しても各 worker は独立した
/// wasm モジュールインスタンスを持つので static 共有は発生しない。
#[cfg(any(target_arch = "wasm32", test))]
struct CachedClusters {
    fingerprint: u64,
    clusters_full: Vec<Cluster>,
    bg: [u8; 4],
    clusters: Vec<Cluster>,
}

/// wasm シングルスレッド前提の `RefCell` ラッパ。
///
/// `OnceLock<T>` の `T` は `Sync` を要求するが、`RefCell<T>` は `Sync` を
/// 提供しないため、そのままでは `OnceLock<RefCell<...>>` を `static` に
/// 置けない。wasm32 ターゲットでは static の共有が実質スレッド境界を
/// 越えないので、ここで `Sync`/`Send` を手動 impl する。これで以後は
/// `static mut` も `#[allow(static_mut_refs)]` も不要になる。
struct WasmSingleThreadCell<T>(RefCell<T>);
// SAFETY: wasm32 はシングルスレッド (Web Worker は別 wasm インスタンスを持つ)。
// 同一 wasm インスタンス内で `RefCell` を多スレッドから同時アクセスすることは
// 構造的に起きない。borrow 衝突は通常の RefCell ルールで実行時に検出される。
unsafe impl<T> Sync for WasmSingleThreadCell<T> {}
unsafe impl<T> Send for WasmSingleThreadCell<T> {}

impl<T> WasmSingleThreadCell<T> {
    fn new(v: T) -> Self {
        Self(RefCell::new(v))
    }
    fn borrow_mut(&self) -> std::cell::RefMut<'_, T> {
        self.0.borrow_mut()
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn source_cache() -> &'static WasmSingleThreadCell<Option<CachedClusters>> {
    static CELL: OnceLock<WasmSingleThreadCell<Option<CachedClusters>>> = OnceLock::new();
    CELL.get_or_init(|| WasmSingleThreadCell::new(None))
}

#[cfg(any(target_arch = "wasm32", test))]
fn fingerprint(rgb: &[u8], w: u32, h: u32, k: usize) -> u64 {
    // 完全一致は不要。長さ + dims + k + 4 隅サンプルで衝突は実用上ゼロ。
    let mut acc: u64 = 0xcbf29ce484222325; // FNV offset basis
    let mix = |acc: u64, b: u64| acc.wrapping_mul(0x100000001b3).wrapping_add(b);
    acc = mix(acc, rgb.len() as u64);
    acc = mix(acc, w as u64);
    acc = mix(acc, h as u64);
    acc = mix(acc, k as u64);
    if rgb.len() >= 12 {
        for i in 0..3 {
            acc = mix(acc, rgb[i] as u64);
            acc = mix(acc, rgb[rgb.len() / 2 + i] as u64);
            acc = mix(acc, rgb[rgb.len() - 1 - i] as u64);
            acc = mix(acc, rgb[rgb.len() / 4 + i] as u64);
        }
    }
    acc
}

/// kmeans 結果（clusters_full / bg / clusters）を取得する。同じソース画像なら
/// キャッシュヒットして O(1)、違う画像なら kmeans 実行 + キャッシュ更新。
///
/// レビュー S1: `static mut SOURCE_CACHE` を `OnceLock<WasmSingleThreadCell<...>>`
/// 経由に切り替え。`unsafe` ブロックも `#[allow(static_mut_refs)]` も不要になる。
#[cfg(any(target_arch = "wasm32", test))]
type ClustersBundle = (Vec<Cluster>, [u8; 4], Vec<Cluster>);

#[cfg(any(target_arch = "wasm32", test))]
fn get_or_build_clusters(p: &mut WasmParams) -> Result<ClustersBundle, String> {
    let fp = fingerprint(&p.source_rgb, p.source_width, p.source_height, p.k);
    {
        let cache = source_cache().borrow_mut();
        if let Some(c) = cache.as_ref() {
            if c.fingerprint == fp {
                return Ok((c.clusters_full.clone(), c.bg, c.clusters.clone()));
            }
        }
    }
    let source = build_source_image(p)?;
    let clusters_full =
        extract_clusters(&source, p.k).map_err(|e| format!("cluster extraction failed: {e}"))?;
    let bg = derive_background_rgba(&clusters_full);
    let clusters = drop_dominant(&clusters_full);
    let cached = CachedClusters {
        fingerprint: fp,
        clusters_full: clusters_full.clone(),
        bg,
        clusters: clusters.clone(),
    };
    *source_cache().borrow_mut() = Some(cached);
    Ok((clusters_full, bg, clusters))
}

// #247: JsValue を受ける入口。呼び出し元は `gpu` モジュール（wasm32 専用）だけに
// なったため wasm32 限定でコンパイルする（native では JsValue 経路自体が無い）。
#[cfg(target_arch = "wasm32")]
fn deserialize_params(params_js: JsValue) -> Result<WasmParams, String> {
    let p: WasmParams = serde_wasm_bindgen::from_value(params_js)
        .map_err(|e| format!("failed to parse params: {e}"))?;
    validate_params(&p)?;
    Ok(p)
}

#[cfg(any(target_arch = "wasm32", test))]
fn validate_params(p: &WasmParams) -> Result<(), String> {
    if !p.seed.is_finite() || p.seed < 0.0 {
        return Err(format!(
            "seed must be a non-negative finite number, got {}",
            p.seed
        ));
    }
    if p.source_width == 0 || p.source_height == 0 {
        return Err("source_width / source_height must be > 0".to_string());
    }
    if p.width == 0 || p.height == 0 {
        return Err("width / height must be > 0".to_string());
    }
    if p.width > MAX_DIM || p.height > MAX_DIM {
        return Err(format!(
            "width / height must be <= {MAX_DIM}, got {}x{}",
            p.width, p.height
        ));
    }
    if p.source_width > MAX_DIM || p.source_height > MAX_DIM {
        return Err(format!(
            "source_width / source_height must be <= {MAX_DIM}, got {}x{}",
            p.source_width, p.source_height
        ));
    }
    // #231 review: image マスク（shape == "image" のときだけ意味を持つが、フィールド
    // 自体は常に来うる）も上限なし alloc を防ぐため source_rgb と同流儀で MAX_DIM を課す。
    // resolve_image_shape は image_mask_width * image_mask_height * 4 で RgbaImage を
    // 確保するので、ここで早期に弾けば過大確保を未然に防げる。
    if p.image_mask_width > MAX_DIM || p.image_mask_height > MAX_DIM {
        return Err(format!(
            "image_mask_width / image_mask_height must be <= {MAX_DIM}, got {}x{}",
            p.image_mask_width, p.image_mask_height
        ));
    }
    // #241: 影強度は 0.0..=1.0 のみ（両端 inclusive）。NaN は比較が false になり
    // !contains で reject される。範囲外を黙ってクランプせず明示エラーにする
    // （dev チューニングノブなので、誤入力は気づける方が良い）。
    if !(0.0..=1.0).contains(&p.shadow_strength) {
        return Err(format!(
            "shadow_strength must be in [0.0, 1.0], got {}",
            p.shadow_strength
        ));
    }
    Ok(())
}

fn err_to_js(s: String) -> JsError {
    JsError::new(&s)
}

/// バッチ index に対応する direction を返す。
///
/// 静止画タイル領域 (`spec_idx < still_count`) では spec の direction を
/// そのまま使い、動画タイル領域 (`spec_idx >= still_count`) では
/// `GUI_VIDEO_DIRECTIONS` の対応 index で上書きする。これにより GUI 経路
/// では動画 4 枚に LR/RL/TB/BT が 1 枚ずつ重複なく割り当てられる。
/// [`resolve_frame`] から呼ばれ、各 spec_idx の direction を決める。
#[cfg(any(target_arch = "wasm32", test))]
fn direction_for_spec_idx(
    spec_idx: usize,
    still_count: usize,
    spec: &VariationSpec,
) -> MotionDirection {
    if spec_idx >= still_count {
        let video_idx = spec_idx - still_count;
        debug_assert!(video_idx < GUI_VIDEO_COUNT_DEFAULT);
        GUI_VIDEO_DIRECTIONS[video_idx]
    } else {
        spec.direction
    }
}

/// バッチ index に対応する speed を返す。
///
/// 静止画タイル領域では spec.speed をそのまま使い、動画タイル領域では
/// `GUI_VIDEO_SPEEDS` の対応 index で上書きする。これにより GUI 経路の
/// 動画 4 枚は VerySlow / Slow / VerySlow / Slow と必ずばらけて、
/// 「4 つ全部速い / 全部遅い」のガチャ感低下を防ぐ (#77)。
///
/// `direction_for_spec_idx` と同じ責務分担で、[`resolve_frame`] から呼ばれる。
#[cfg(any(target_arch = "wasm32", test))]
fn speed_for_spec_idx(spec_idx: usize, still_count: usize, spec: &VariationSpec) -> MotionSpeed {
    if spec_idx >= still_count {
        let video_idx = spec_idx - still_count;
        debug_assert!(video_idx < GUI_VIDEO_COUNT_DEFAULT);
        GUI_VIDEO_SPEEDS[video_idx]
    } else {
        spec.speed
    }
}

/// WGSL canvas-present 経路（`gpu::gpu_set_render_data`）が保持する解決済み入力（#231）。
///
/// `clusters` + `opts` を core の `render_frame_*_to_view`（shape 別）へそのまま渡す。
/// `pack` は **orb shape 専用**: orb の見た目を #230 から一切変えないため、orb は
/// pack 経由の `render_packed_to_view` を温存する（pack は core の
/// `pack_render_data` 出力で、saturation を焼かない＝web に saturation ノブが
/// 無いのと整合する。core の `render_frame_to_view` は saturation を再適用するため、
/// 万一の HSL 往復誤差で #230 と差が出るのを避ける狙い）。glyph / image は
/// `opts` 経由で core の専用経路に乗せる。
// GPU(WGSL) canvas-present 経路は wasm32 専用（gpu.rs が `#[cfg(target_arch =
// "wasm32")]`）。native の `cargo test` でも検証できるよう `cfg(any(wasm32, test))`
// で共有する（純粋関数なので GPU アダプタは不要）。これにより native の `cargo build`
// では未使用にならず（コンパイル対象外）、dead_code 警告も出ない。
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) struct GpuRenderInputs {
    pub clusters: Vec<Cluster>,
    pub opts: AnimateOptions,
    /// orb shape の pack（`render_packed_to_view` 用）。orb 以外では未使用だが、
    /// shape を opts に持つので分岐は `gpu_render` 側で行う。
    pub pack: Vec<f32>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl ResolvedFrame {
    /// 解決済みスカラを [`AnimateOptions`] に組み替える（#231）。core の
    /// `pack_sdf_frame` が同じ base_radius_unit /
    /// base_blur / direction / speed / seed / n_orbs を再計算できるよう、
    /// 元のスカラと逆算可能な形（orb_size / blur / softness / count）で詰める。
    ///
    /// `saturation = 1.0`（web は saturation ノブを持たない＝恒等）。入力は静止画
    /// のみ（per-orb 色は抽出クラスタ固定）。
    fn to_animate_options(&self, shape: OrbShape) -> AnimateOptions {
        AnimateOptions {
            width: self.width,
            height: self.height,
            // #239 Phase 1: 製品の 3 段にじみボタンを Web GPU 経路へ流す。`aqua_bleed`
            // が `Some` のときだけ additive ブラーレイヤを engage する（CLI の poc_aqua と
            // 同じ責務）。`None`（にじみオフ）のとき従来の Web 出力と byte-identical
            // （非リグレッションゲート）。bloom/offset/halo は各軸の専用 3 段ボタン
            // （`aqua_bloom`/`aqua_offset`/`aqua_halo`、未指定 = 0）で決まり、幾何は唯一の
            // 製品ジオメトリ continuous。3 軸とも未指定なら `bleed weak` の出力が CLI の
            // `--bleed weak`（character 軸オフ）と一致する。
            aqua: self.aqua_bleed.map(|bleed| AquaBleedConfig {
                bleed,
                bloom: self.aqua_bloom,
                offset: self.aqua_offset,
                halo: self.aqua_halo,
            }),
            orb_size: self.orb_size,
            blur: self.blur,
            saturation: 1.0,
            direction: self.direction,
            speed: self.speed,
            seed: self.spec_seed,
            count: Some(self.n_orbs),
            background: self.bg,
            shape,
            softness: self.softness,
            glyph_rotate: self.glyph_rotate,
            shadow_strength: self.shadow_strength,
        }
    }
}

/// WGSL canvas-present 経路の入力を構築する（#231）。[`resolve_frame`] で
/// spec / preset / kmeans を解決し、形状は `resolve_orb_shape` で
/// 全 shape（orb / glyph / image）に解決する。
///
/// orb の pack は shape_id=0.0 で焼く（#230 / #242 のルックを温存する固定バイト列）。
/// glyph / image は `opts` で core の専用経路に分岐させる（pack は未使用だが、
/// orb 用に常に作っておく＝分岐は描画時 1 箇所だけにする）。
#[cfg(any(target_arch = "wasm32", test))]
fn build_gpu_render_inputs(
    mut p: WasmParams,
    n: u32,
    spec_idx: u32,
) -> Result<GpuRenderInputs, String> {
    // 形状解決は resolve_frame が p を move する前に済ませる（image は mask bytes が要る）。
    // resolve_orb_shape は image マスクを clone せず take で奪うため &mut で渡す
    // （奪われた image_mask_rgba は resolve_frame では読まれない）。
    let shape = resolve_orb_shape(&mut p)?;
    let resolved = resolve_frame(p, n, spec_idx)?;

    // orb pack は shape_id=0.0 の固定バイト列（#230 / #242 のルックを温存）。
    // #230 の `render_packed_to_view` 経路をそのまま温存するための pack。
    let pack = pack_render_data(
        &resolved.clusters,
        resolved.bg,
        resolved.base_radius_unit,
        resolved.base_blur,
        resolved.direction_id,
        resolved.cycle,
        resolved.spec_seed,
        resolved.n_orbs,
        resolved.alpha_mul,
        0.0, // shape_id = Orb（pack は orb 専用）
        resolved.glyph_rotate,
        resolved.edge_softness,
        resolved.shadow_strength,
    );

    let opts = resolved.to_animate_options(shape);
    Ok(GpuRenderInputs {
        clusters: resolved.clusters,
        opts,
        pack,
    })
}

/// `build_gpu_render_inputs`（WGSL canvas-present 経路）が使う「1 タイルの
/// 決定論解決」結果（#231 で切り出し）。
///
/// spec 列の再構築・preset 上書き・kmeans キャッシュ・`GL_RENDERER_MAX_ORBS` の
/// 早期エラーまでを 1 か所に集約し、WGSL は [`AnimateOptions`] + clusters
/// （+ orb 用 pack）に組み替えるだけにする。spec 解決ロジックを 1 か所に集約
/// することで、`build_gpu_render_inputs_*` のテストが守る pack / 入力の不変性を
/// 単一の経路で保証する。
///
/// 一部フィールド（direction / speed / softness / width / height / orb_size / blur /
/// direction_id）は GPU 経路の `to_animate_options` だけが読む。native の `cargo build`
/// （wasm32 でも test でもない）では GPU 経路がコンパイル対象外で未読になるため、
/// その構成に限り dead_code を許可する（wasm32 / test では全フィールドが読まれる）。
#[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
struct ResolvedFrame {
    clusters: Vec<Cluster>,
    bg: [u8; 4],
    base_radius_unit: f32,
    base_blur: f32,
    direction: MotionDirection,
    direction_id: f32,
    speed: MotionSpeed,
    cycle: f32,
    spec_seed: u64,
    n_orbs: usize,
    alpha_mul: f32,
    softness: SoftnessPreset,
    glyph_rotate: bool,
    edge_softness: f32,
    /// #241「薄い影」強度（0..1）。`WasmParams::shadow_strength`（既定 = 製品定数
    /// [`SHADOW_STRENGTH_DEFAULT`]）をそのまま運ぶ。orb は pack header[13]、
    /// glyph / image は `AnimateOptions` 経由で core の pack に乗る。
    shadow_strength: f32,
    width: u32,
    height: u32,
    orb_size: f32,
    blur: f32,
    /// #239 Phase 1: 製品の 3 段にじみボタン（`WasmParams::bleed_preset`）が解決した
    /// 内部 `aqua_bleed` 量。`None`（既定 = にじみオフ）のとき `to_animate_options`
    /// は `aqua: None` を保ち、従来の Web 出力と byte-identical（非リグレッション
    /// ゲート）。`Some(bleed)` のとき continuous の空間ブラー additive レイヤを engage
    /// する（CLI の `--bleed` と同じ写像）。
    aqua_bleed: Option<f32>,
    /// #239 Phase 1: bloom / offset / halo の character 軸係数（`WasmParams` の
    /// `bloom_preset`/`offset_preset`/`halo_preset` が解決した値）。未指定軸は 0.0
    /// （その軸オフ）。`aqua_bleed` が `Some` のときだけ `to_animate_options` が
    /// `AquaBleedConfig` に流す。にじみオフ（`aqua_bleed == None`）なら 3 軸とも
    /// 無視される。CLI の `--bloom`/`--halo`/`--offset` と同じ 0.3 / 0.6 / 0.9 写像。
    aqua_bloom: f32,
    aqua_offset: f32,
    aqua_halo: f32,
}

/// 1 タイルの spec / preset / kmeans / orb 数を解決する（WGSL canvas-present 経路、#231）。
///
/// 形状解決（OrbShape）は呼び出し側（`build_gpu_render_inputs` の `resolve_orb_shape`）
/// に委ねる。本関数は形状非依存の per-orb スカラだけを返す。
#[cfg(any(target_arch = "wasm32", test))]
fn resolve_frame(mut p: WasmParams, n: u32, spec_idx: u32) -> Result<ResolvedFrame, String> {
    let count_override = parse_count_preset(&p.count_preset)?;
    let speed_override = parse_speed_preset(&p.speed_preset)?;
    let softness = parse_softness_preset(&p.softness_preset)?;
    // #239 Phase 1: にじみの 3 段ボタン。`""` は None（くっきり）で従来挙動と byte
    // 一致。weak/mid/strong は 0.15/0.3/0.5。`Some` のとき to_animate_options が
    // additive レイヤを engage する（CLI の poc_aqua と同じ責務分割）。
    let aqua_bleed = parse_bleed_preset(&p.bleed_preset)?;
    // #239 Phase 1: bloom / halo / offset の character 軸。`""` は 0.0（その軸オフ）、
    // weak/mid/strong は 0.3/0.6/0.9（CLI の --bloom/--halo/--offset と同じ）。にじみ
    // オフ（aqua_bleed == None）なら to_animate_options が 3 軸ごと無視する。
    let aqua_bloom = parse_character_preset(&p.bloom_preset, "bloom")?;
    let aqua_halo = parse_character_preset(&p.halo_preset, "halo")?;
    let aqua_offset = parse_character_preset(&p.offset_preset, "offset")?;

    let total = (n as usize).clamp(1, 50);
    let spec_idx = spec_idx as usize;
    if spec_idx >= total {
        return Err(format!("spec_idx {spec_idx} is out of range [0, {total})"));
    }
    let still_count = total.saturating_sub(GUI_VIDEO_COUNT_DEFAULT);

    // kmeans は同じソース画像なら同じ結果になるのでキャッシュする。
    // Android では kmeans が ~3 秒かかり、これがタイル毎に走ることで
    // 12 stills + 4 mp4 = 16 呼び出しで合計 ~50 秒の律速になっていた。
    let (clusters_full, mut bg, clusters) = get_or_build_clusters(&mut p)?;
    let _ = clusters_full; // 現在は未使用だが将来 spec に diversity 等で使う可能性

    // #245: 透過 export。キャッシュ取得後のローカル値だけを書き換える
    // （kmeans キャッシュの bg は不透明のまま汚さない）。
    if p.transparent_background {
        bg[3] = 0;
    }

    let specs = random_batch_specs(p.seed as u64, total, still_count);
    let spec = specs[spec_idx];
    let direction = direction_for_spec_idx(spec_idx, still_count, &spec);
    // Phase B (#55): UI から speed_preset が来ていれば、video 領域の
    // GUI_VIDEO_SPEEDS 固定割当も無視してユーザ指定値で全タイル統一する。
    // none なら従来どおり (still=spec.speed, video=GUI_VIDEO_SPEEDS)。
    let speed = match speed_override {
        Some(s) => s,
        None => speed_for_spec_idx(spec_idx, still_count, &spec),
    };

    let direction_id: f32 = match direction {
        MotionDirection::LeftToRight => 0.0,
        MotionDirection::RightToLeft => 1.0,
        MotionDirection::TopToBottom => 2.0,
        MotionDirection::BottomToTop => 3.0,
    };
    let cycle = speed.cycle_count() as f32;

    // Phase B (#55): count_preset があれば spec.count を上書きする。
    // 未指定なら従来どおり spec.count（random_ranges から 10..=50 一様抽選）。
    let effective_count = count_override.unwrap_or(spec.count);
    let n_orbs = effective_count
        .min(MAX_ORB_COUNT)
        .max(if clusters.is_empty() { 0 } else { 1 });

    // review S2: 旧来の固定 uniform-array レンダラの上限を超えると黙って切り詰め
    // られて視覚パリティが壊れていた。発見が遅れないよう wasm 側で早期 throw する。
    // count_preset (high=24, #265) は GL_RENDERER_MAX_ORBS=64 未満。将来 high を 64 超に
    // 上げるならここを更新する。WGSL canvas-present 経路はデータテクスチャ経路で
    // この上限を必要としないが、GUI の count 上限 24 (#265) を十分上回るため当面同一
    // バリデーションで揃えておく。
    if n_orbs > GL_RENDERER_MAX_ORBS {
        return Err(format!(
            "n_orbs {n_orbs} exceeds renderer orb-count cap {GL_RENDERER_MAX_ORBS}"
        ));
    }

    let base_radius_unit = (p.width.min(p.height) as f32) * 0.25 * spec.orb_size.max(0.0);
    // Phase B (#55): softness.blur_offset() を base_blur に積算（core/animate と同式）。
    // #205 以降 Mid は +0.25 で blurry 寄りの新 default。
    let base_blur = (spec.blur + softness.blur_offset()).clamp(0.0, 1.0);
    let alpha_mul = softness.alpha_mul().clamp(0.0, 1.0);
    // #205: Glyph/image アーム smoothstep 幅を softness 連動。Orb は参照しない。
    let edge_softness = softness.edge_softness();

    Ok(ResolvedFrame {
        clusters,
        bg,
        base_radius_unit,
        base_blur,
        direction,
        direction_id,
        speed,
        cycle,
        spec_seed: spec.seed,
        n_orbs,
        alpha_mul,
        softness,
        glyph_rotate: p.glyph_rotate,
        edge_softness,
        // #241: 省略時は serde default = 製品定数。gpu-lab のスライダーだけが
        // 非定数値を送る（validate_params が 0.0..=1.0 を保証済み）。
        shadow_strength: p.shadow_strength,
        width: p.width,
        height: p.height,
        orb_size: spec.orb_size.max(0.0),
        blur: spec.blur,
        // #239 Phase 1: にじみ量（None = くっきり）。engage 判定は to_animate_options
        // 側で行う。
        aqua_bleed,
        // #239 Phase 1: character 軸（未指定 = 0）。にじみオフ時は to_animate_options
        // が 3 軸ごと無視する。
        aqua_bloom,
        aqua_offset,
        aqua_halo,
    })
}

/// core 側と共有の `generate_orb_params` 出力を使って、ヘッダ + per-orb
/// フィールドを Float32 ベクタに詰める。
///
/// wasm 経路が core のアニメーションと別 RNG 列を持たないよう、乱数列は
/// ここで再実装せず `orber_core::animate::generate_orb_params` に委譲する
/// （pack の本体は core の [`orber_core::animate::pack_render_data`]）。
// TODO(orber#future): pack_render_data の引数が 12 個に達した (#205 で edge_softness 追加)。
// Phase C で orb 形状軸が更に増えるなら struct で受けるリファクタを検討する。
// #247: 唯一の呼び出し元 build_gpu_render_inputs が wasm32 / test 限定のため同様に対象外化。
#[cfg(any(target_arch = "wasm32", test))]
#[allow(clippy::too_many_arguments)]
fn pack_render_data(
    clusters: &[Cluster],
    bg: [u8; 4],
    base_radius_unit: f32,
    base_blur: f32,
    direction_id: f32,
    cycle: f32,
    seed: u64,
    n_orbs: usize,
    alpha_mul: f32,
    shape_id: f32,
    glyph_rotate: bool,
    edge_softness: f32,
    shadow_strength: f32,
) -> Vec<f32> {
    // 完全修飾で呼ぶ（この関数自体が同名 `pack_render_data` のため import すると衝突する）。
    // 入力は静止画のみ。off+13 は core 側で常に 0.0 に保たれる（`misc.w` 未使用）。
    orber_core::animate::pack_render_data(
        clusters,
        bg,
        base_radius_unit,
        base_blur,
        direction_id,
        cycle,
        seed,
        n_orbs,
        alpha_mul,
        shape_id,
        glyph_rotate,
        edge_softness,
        shadow_strength,
    )
}

/// Glyph 1 文字の SDF texture を JS 側に返す wasm wrapper。
///
/// 実体は [`orber_core::glyph::render_glyph_sdf`] を参照。本関数は
/// その上に `(font, ch, size)` キャッシュ + size validation + JS 型変換だけを
/// 加える。size は `[16, 1024]` の範囲のみ受理（GUI は 256 固定の想定）。
/// 戻り値は長さ `size * size` の `Uint8Array`（行優先 SDF 0..255）。
/// 同梱フォントに無い文字は全 0 を返し panic しない。
#[wasm_bindgen]
pub fn get_glyph_sdf(ch: &str, size: u32) -> Result<js_sys::Uint8Array, JsError> {
    // 入力 validation。size は 16..=1024 の範囲を許可（GUI は 256 を使う想定）。
    if !(16..=1024).contains(&size) {
        return Err(JsError::new(&format!(
            "size must be in [16, 1024], got {size}"
        )));
    }
    let ch = first_char_of(ch).map_err(err_to_js)?;
    let bytes = glyph_sdf_bytes(GlyphFontId::NotoSymbols2, ch, size);
    Ok(js_sys::Uint8Array::from(&bytes[..]))
}

/// `has_glyph(NotoSymbols2, ch)` の wasm 公開ラッパ。UI の警告表示で使う。
/// 空文字や複数 char の場合は先頭 char のみ判定する（UI 側で 1 char 制限想定）。
#[wasm_bindgen]
pub fn glyph_supported(ch: &str) -> bool {
    match ch.chars().next() {
        Some(c) => has_glyph(GlyphFontId::NotoSymbols2, c),
        None => false,
    }
}

/// `(font, ch, size) -> Vec<u8>` の同一プロセス内キャッシュ。
/// HashMap キーは `(font, ch as u32, size)`。
///
/// レビュー S2: worker の `getRenderer` は (w, h) 切替時に renderer を作り直すが
/// この wasm 側 `glyph_sdf_cache` は wasm モジュール再ロード（HMR / 再起動）まで
/// 残る。同一 size + 同一 ch なら毎回同じ bytes が返るので決定論的に問題は
/// 無いが、開発時 HMR で再ロードしない場合に古いキャッシュエントリが残ったまま
/// になる点だけ注意。実運用では size=GLYPH_SDF_SIZE=256 に固定なので、
/// glyph 文字数 ×サイズ 1 通りで メモリ上限は数十エントリ程度。
type GlyphSdfKey = (GlyphFontId, u32, u32);

/// レビュー S1: `static mut CACHE` を `OnceLock<WasmSingleThreadCell<HashMap<...>>>`
/// に置換。`#[allow(static_mut_refs)]` を削除し、`unsafe` も無くなる。
fn glyph_sdf_cache() -> &'static WasmSingleThreadCell<HashMap<GlyphSdfKey, Vec<u8>>> {
    static CELL: OnceLock<WasmSingleThreadCell<HashMap<GlyphSdfKey, Vec<u8>>>> = OnceLock::new();
    CELL.get_or_init(|| WasmSingleThreadCell::new(HashMap::new()))
}

fn glyph_sdf_bytes(font: GlyphFontId, ch: char, size: u32) -> Vec<u8> {
    let key = (font, ch as u32, size);
    {
        let cache = glyph_sdf_cache().borrow_mut();
        if let Some(v) = cache.get(&key) {
            return v.clone();
        }
    }
    let v = render_glyph_sdf(font, ch, size);
    glyph_sdf_cache().borrow_mut().insert(key, v.clone());
    v
}

// ---- #245: gpu_render_rgba（gpu.rs）の readback 行 padding 純関数 ----------
//
// texture → buffer copy は bytes_per_row の 256 byte alignment が必須なので、
// padded 行長の計算と「行ごとに padding を落として詰め直す」変換を gpu.rs の
// async 経路から切り出して native テスト可能にする（gpu.rs 自体は wasm32 専用
// cfg のため、native の `cargo test` から届かない）。`cfg(any(wasm32, test))`
// は `resolve_orb_shape` 等と同じ共有パターン。

/// WebGPU texture→buffer readback の行アライメント（bytes）。
/// `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`（= 256）と同値。native の test target は
/// wgpu を直接依存に持たない（wasm32 専用 target dependency）ため同値の定数を
/// ここに置き、wasm32 ビルドでは const assert で wgpu 本体との一致を担保する
/// （drift したらコンパイルエラー）。
#[cfg(any(target_arch = "wasm32", test))]
const COPY_ROW_ALIGN: u32 = 256;
#[cfg(target_arch = "wasm32")]
const _: () = assert!(COPY_ROW_ALIGN == wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);

/// readback 1 ピクセルのバイト数（`Rgba8Unorm` 固定 = 4。gpu_render_rgba は
/// surface format と独立に常に Rgba8Unorm で読み戻す）。
#[cfg(any(target_arch = "wasm32", test))]
const READBACK_BYTES_PER_PIXEL: u32 = 4;

/// `width` px の RGBA 1 行を texture→buffer copy するときの padded 行長
/// （bytes）。`width * 4` を 256 の倍数に切り上げる。
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn padded_bytes_per_row(width: u32) -> u32 {
    (width * READBACK_BYTES_PER_PIXEL).div_ceil(COPY_ROW_ALIGN) * COPY_ROW_ALIGN
}

/// padded な readback バッファ（`padded_bytes_per_row * height` bytes）から
/// 行末 padding を落とし、`width * height * 4` bytes の行優先 RGBA に詰め直す。
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn unpad_rows(
    data: &[u8],
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) -> Vec<u8> {
    let unpadded_bytes_per_row = (width * READBACK_BYTES_PER_PIXEL) as usize;
    let mut out = Vec::with_capacity(unpadded_bytes_per_row * height as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        out.extend_from_slice(&data[start..start + unpadded_bytes_per_row]);
    }
    out
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// `source_cache()` のグローバル `RefCell` を触るテストを直列化するためのガード。
    /// native の `cargo test` は既定でテストを並列実行するため、複数テストが同時に
    /// `get_or_build_clusters()` → `borrow_mut()` すると `BorrowMutError` になる
    /// (#220)。production(wasm) は single-thread なのでこの直列化はテスト専用。
    static CACHE_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_speed_preset_handles_empty_and_values() {
        // 空文字だけが identity（None）。明示選択時は GUI の 3 段を
        // VerySlow / Slow / Mid にマップする。
        assert!(matches!(parse_speed_preset(""), Ok(None)));
        assert!(matches!(
            parse_speed_preset("slow"),
            Ok(Some(MotionSpeed::VerySlow))
        ));
        assert!(matches!(
            parse_speed_preset("mid"),
            Ok(Some(MotionSpeed::Slow))
        ));
        assert!(matches!(
            parse_speed_preset("fast"),
            Ok(Some(MotionSpeed::Mid))
        ));
        // M2: very-slow は UI 経路では受け付けない（CLI 専用、parse_speed が担当）。
        assert!(parse_speed_preset("very-slow").is_err());
        assert!(parse_speed_preset("xxx").is_err());
    }

    /// M1: count_preset='' のとき effective_count == spec.count を保つ。
    /// `parse_count_preset` が `None` を返し、`resolve_frame` 内で
    /// `count_override.unwrap_or(spec.count)` がそのまま spec.count を採用する。
    #[test]
    fn count_preset_empty_or_unspecified_uses_spec_count() {
        let count_override = parse_count_preset("").unwrap();
        assert!(count_override.is_none());
        // identity 不変条件: count_override.unwrap_or(spec_count) == spec_count
        let spec_count: usize = 27;
        assert_eq!(count_override.unwrap_or(spec_count), spec_count);
    }

    /// M1: speed_preset='' のとき speed_for_spec_idx の戻り値（
    /// still=spec.speed / video=GUI_VIDEO_SPEEDS）を温存する。
    #[test]
    fn speed_preset_empty_uses_spec_idx() {
        let speed_override = parse_speed_preset("").unwrap();
        assert!(
            speed_override.is_none(),
            "speed_preset='' must be identity (None)"
        );
        // identity 経路: resolve_frame の match arm が
        // `speed_for_spec_idx(spec_idx, still_count, &spec)` を採用する。
        let still_count = 8;
        let total = 12;
        let mut spec = synth_spec(MotionDirection::TopToBottom);
        spec.speed = MotionSpeed::Slow;
        // still 領域は spec.speed を保つ。
        for spec_idx in 0..still_count {
            assert_eq!(
                speed_for_spec_idx(spec_idx, still_count, &spec),
                MotionSpeed::Slow
            );
        }
        // video 領域は GUI_VIDEO_SPEEDS の固定割当を保つ。
        for spec_idx in still_count..total {
            assert_eq!(
                speed_for_spec_idx(spec_idx, still_count, &spec),
                GUI_VIDEO_SPEEDS[spec_idx - still_count]
            );
        }
    }

    /// M1: softness_preset='' のとき SoftnessPreset::Mid と一致する（identity）。
    #[test]
    fn softness_preset_empty_is_mid_identity() {
        assert_eq!(parse_softness_preset("").unwrap(), SoftnessPreset::Mid);
        assert_eq!(parse_softness_preset("mid").unwrap(), SoftnessPreset::Mid);
        // Mid は alpha_mul=1.0 / blur_offset=0.0 で既存挙動と完全同値であることが
        // crates/core/src/style.rs の regression test で固定されている。
    }

    #[test]
    fn parse_count_preset_table() {
        assert_eq!(parse_count_preset("").unwrap(), None);
        assert_eq!(parse_count_preset("low").unwrap(), Some(10));
        assert_eq!(parse_count_preset("mid").unwrap(), Some(20));
        assert_eq!(parse_count_preset("high").unwrap(), Some(24));
        assert!(parse_count_preset("xxx").is_err());
    }

    #[test]
    fn parse_softness_preset_table() {
        assert_eq!(parse_softness_preset("").unwrap(), SoftnessPreset::Mid);
        assert_eq!(parse_softness_preset("mid").unwrap(), SoftnessPreset::Mid);
        assert_eq!(parse_softness_preset("low").unwrap(), SoftnessPreset::Low);
        assert_eq!(parse_softness_preset("high").unwrap(), SoftnessPreset::High);
        assert!(parse_softness_preset("xxx").is_err());
    }

    /// #239 Phase 1: にじみ preset 文字列 → 内部 `aqua_bleed` 量の写像。`""` は
    /// `None`（にじみオフ＝くっきり）で、CLI の `--bleed` 未指定（`poc_aqua == None`）
    /// と同じく従来の Web 出力を byte-identical に保つ。weak/mid/strong は CLI の
    /// `CliBleedPreset::to_bleed` と同じ 0.15/0.3/0.5。製品 UI は数字を出さないので
    /// この対応表が唯一の正本。
    #[test]
    fn parse_bleed_preset_table() {
        assert_eq!(parse_bleed_preset("").unwrap(), None);
        assert_eq!(parse_bleed_preset("weak").unwrap(), Some(0.15));
        assert_eq!(parse_bleed_preset("mid").unwrap(), Some(0.3));
        assert_eq!(parse_bleed_preset("strong").unwrap(), Some(0.5));
        assert!(parse_bleed_preset("xxx").is_err());
    }

    #[test]
    fn parse_shape_orb_glyph() {
        assert!(matches!(parse_shape("orb", ""), Ok(OrbShape::Orb)));
        // glyph では glyph_char が必須。空はエラー。
        assert!(parse_shape("glyph", "").is_err());
        let g = parse_shape("glyph", "☆").unwrap();
        assert!(matches!(g, OrbShape::Glyph { ch, .. } if ch == '☆'));
        // #231: image は parse_shape では受けない（resolve_orb_shape 経由を強制）。
        assert!(parse_shape("image", "").is_err());
        assert!(parse_shape("", "").is_err());
        // aquarelle は #239 Phase 1 で削除済み。未知 shape として弾く。
        assert!(parse_shape("aquarelle", "").is_err());
    }

    /// #231: resolve_orb_shape の image 経路は mask 入力を検証する。サイズ不一致 /
    /// 空マスクは Err、コントラストある RGBA は OrbShape::Image に解決する。
    #[test]
    fn resolve_orb_shape_image_validates_mask() {
        // width/height 0 は Err。
        let mut p = base_params();
        p.shape = "image".into();
        assert!(resolve_orb_shape(&mut p).is_err());

        // 長さ不一致は Err。
        let mut p = base_params();
        p.shape = "image".into();
        p.image_mask_width = 2;
        p.image_mask_height = 2;
        p.image_mask_rgba = vec![0; 3]; // 2*2*4=16 ではない
        assert!(resolve_orb_shape(&mut p).is_err());

        // 単色（全 0）はコントラスト無しで Err（image_rgba_to_sdf が None）。
        let mut p = base_params();
        p.shape = "image".into();
        p.image_mask_width = 4;
        p.image_mask_height = 4;
        p.image_mask_rgba = vec![0; 4 * 4 * 4];
        assert!(resolve_orb_shape(&mut p).is_err());

        // 上半分不透明 / 下半分透明のマスクはシルエットがあり Image に解決する。
        let mut p = base_params();
        p.shape = "image".into();
        let (w, h) = (16u32, 16u32);
        p.image_mask_width = w;
        p.image_mask_height = h;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let a = if y < h / 2 { 255 } else { 0 };
                rgba[i] = 255;
                rgba[i + 1] = 255;
                rgba[i + 2] = 255;
                rgba[i + 3] = a;
            }
        }
        p.image_mask_rgba = rgba;
        match resolve_orb_shape(&mut p) {
            Ok(OrbShape::Image { size, sdf }) => {
                assert_eq!(size, orber_core::glyph::DEFAULT_GLYPH_SDF_SIZE);
                assert_eq!(sdf.len(), (size * size) as usize);
            }
            other => panic!("expected Ok(OrbShape::Image), got {other:?}"),
        }
    }

    #[test]
    fn glyph_sdf_paints_known_char() {
        // ☆ は同梱 NotoSansSymbols2 にある。inside 側サンプルが一定数以上あること
        // （少なくとも全ピクセルの 5% は 0.5 超になる想定）。
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '☆', 64);
        assert_eq!(bytes.len(), 64 * 64);
        let lit = bytes.iter().filter(|&&b| b > 127).count();
        assert!(
            lit > 64 * 64 / 20,
            "rendering ☆ at 64x64 should produce >=5% inside pixels, got {lit}"
        );
    }

    #[test]
    fn glyph_sdf_unknown_char_returns_empty() {
        // 絵文字 (Symbols 2 subset 外) は全 0 を返す。
        let bytes = render_glyph_sdf(GlyphFontId::NotoSymbols2, '\u{1F355}', 32);
        assert_eq!(bytes.len(), 32 * 32);
        assert!(bytes.iter().all(|&b| b == 0));
    }

    fn base_params() -> WasmParams {
        WasmParams {
            source_rgb: vec![0; 4 * 4 * 3],
            source_width: 4,
            source_height: 4,
            k: 4,
            width: 64,
            height: 64,
            seed: 42.0,
            direction: "lr".into(),
            speed: "slow".into(),
            count: 10,
            orb_size: 3.0,
            blur: 0.5,
            shape: "orb".into(),
            // Phase B (#55): 既存挙動互換のため空文字。
            glyph_char: String::new(),
            count_preset: String::new(),
            speed_preset: String::new(),
            softness_preset: String::new(),
            // #239 Phase 1: 既定はにじみオフ（くっきり）。aqua = None で従来挙動互換。
            bleed_preset: String::new(),
            // #239 Phase 1: character 軸も既定は空（各軸オフ = 0）。
            bloom_preset: String::new(),
            halo_preset: String::new(),
            offset_preset: String::new(),
            glyph_rotate: true,
            // #231: image 入力。既定では未使用（shape="orb"）。
            image_mask_rgba: Vec::new(),
            image_mask_width: 0,
            image_mask_height: 0,
            // #231: glyph SDF フォールバック入力。既定では未使用（同梱フォント経路）。
            glyph_sdf: Vec::new(),
            glyph_sdf_size: 0,
            // #241: 影強度。既定は製品定数（serde default と同じ）。
            shadow_strength: SHADOW_STRENGTH_DEFAULT,
            // #245: 透過 export。既定は不透過（serde default と同じ）。
            transparent_background: false,
        }
    }

    #[test]
    fn validate_rejects_negative_seed() {
        let mut p = base_params();
        p.seed = -1.0;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_rejects_nan_seed() {
        let mut p = base_params();
        p.seed = f64::NAN;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_rejects_zero_dimensions() {
        let mut p = base_params();
        p.width = 0;
        assert!(validate_params(&p).is_err());

        let mut p = base_params();
        p.source_height = 0;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_rejects_oversize_dimensions() {
        let mut p = base_params();
        p.width = MAX_DIM + 1;
        assert!(validate_params(&p).is_err());

        let mut p = base_params();
        p.source_width = 100_000;
        assert!(validate_params(&p).is_err());
    }

    #[test]
    fn validate_accepts_reasonable_params() {
        assert!(validate_params(&base_params()).is_ok());
    }

    /// #241 (c): `shadow_strength` の serde 省略 default。フィールドを送らない
    /// 既存呼び出し（本番 Studio / 旧 ab-params.json）は製品定数 = 製品と同じ
    /// 見た目にデシリアライズされること。serde_json は serde_wasm_bindgen と同じ
    /// `Deserialize` 実装を通るので、`#[serde(default = ...)]` の検証として等価。
    #[test]
    fn shadow_strength_serde_defaults_to_production_constant() {
        let json = r#"{
            "source_rgb": [0, 0, 0],
            "source_width": 1,
            "source_height": 1,
            "k": 1,
            "width": 8,
            "height": 8,
            "seed": 1.0,
            "direction": "lr",
            "speed": "slow",
            "count": 1,
            "orb_size": 1.0,
            "blur": 0.5,
            "shape": "orb"
        }"#;
        let p: WasmParams = serde_json::from_str(json).expect("params without shadow_strength");
        assert_eq!(
            p.shadow_strength, SHADOW_STRENGTH_DEFAULT,
            "omitted shadow_strength must default to the production constant"
        );
        // 明示指定はそのまま通る（serde 層は範囲を見ない。範囲は validate_params の担務）。
        let json_explicit = json.replace(
            r#""shape": "orb""#,
            r#""shape": "orb", "shadow_strength": 0.85"#,
        );
        let p: WasmParams = serde_json::from_str(&json_explicit).expect("explicit shadow_strength");
        assert_eq!(p.shadow_strength, 0.85);
    }

    /// #241 (c): `shadow_strength` の範囲検証。0.0 と 1.0 は **inclusive** で受理、
    /// 範囲外（負 / 1 超 / NaN）は reject。dev チューニングノブなので黙って
    /// クランプせず明示エラーにする仕様（validate_params）。
    #[test]
    fn validate_shadow_strength_range_inclusive_bounds() {
        // 両端 inclusive。
        let mut p = base_params();
        p.shadow_strength = 0.0;
        assert!(
            validate_params(&p).is_ok(),
            "0.0 must be accepted (inclusive)"
        );

        let mut p = base_params();
        p.shadow_strength = 1.0;
        assert!(
            validate_params(&p).is_ok(),
            "1.0 must be accepted (inclusive)"
        );

        // 範囲外は reject。
        let mut p = base_params();
        p.shadow_strength = -0.001;
        assert!(validate_params(&p).is_err(), "negative must be rejected");

        let mut p = base_params();
        p.shadow_strength = 1.001;
        assert!(validate_params(&p).is_err(), "above 1.0 must be rejected");

        let mut p = base_params();
        p.shadow_strength = f32::NAN;
        assert!(validate_params(&p).is_err(), "NaN must be rejected");
    }

    /// #231 review: image マスク次元も source_rgb と同流儀で MAX_DIM を課す。
    /// 境界（MAX_DIM ちょうど=OK / MAX_DIM+1=Err）を 1 テストで固定する。
    /// validate_params は次元比較だけで mask bytes を確保しないため、MAX_DIM
    /// ちょうどでも巨大 alloc は起きない（過大確保の早期遮断が狙い）。
    #[test]
    fn build_gpu_render_inputs_image_mask_too_large_errors() {
        // MAX_DIM ちょうどは OK（width 側・height 側どちらも）。
        let mut p = base_params();
        p.image_mask_width = MAX_DIM;
        p.image_mask_height = MAX_DIM;
        assert!(
            validate_params(&p).is_ok(),
            "image_mask dims == MAX_DIM must pass"
        );

        // MAX_DIM + 1 は Err（width 側）。
        let mut p = base_params();
        p.image_mask_width = MAX_DIM + 1;
        assert!(
            validate_params(&p).is_err(),
            "image_mask_width > MAX_DIM must error"
        );

        // MAX_DIM + 1 は Err（height 側）。
        let mut p = base_params();
        p.image_mask_height = MAX_DIM + 1;
        assert!(
            validate_params(&p).is_err(),
            "image_mask_height > MAX_DIM must error"
        );
    }

    fn synth_spec(direction: MotionDirection) -> VariationSpec {
        VariationSpec {
            direction,
            speed: MotionSpeed::Slow,
            count: 10,
            orb_size: 3.0,
            blur: 0.5,
            seed: 1,
            duration_ms: 0,
            kind: orber_core::variations::VariationKind::Png,
            label: "test",
        }
    }

    /// 静止画タイル領域では spec.direction をそのまま使う。
    #[test]
    fn direction_for_spec_idx_returns_spec_direction_for_still_range() {
        let still_count = 8;
        let spec = synth_spec(MotionDirection::TopToBottom);
        for spec_idx in 0..still_count {
            assert_eq!(
                direction_for_spec_idx(spec_idx, still_count, &spec),
                MotionDirection::TopToBottom,
                "still tile {spec_idx} must inherit spec.direction"
            );
        }
    }

    /// 動画タイル領域 (8..12) では GUI_VIDEO_DIRECTIONS で上書きされる。
    /// LR / RL / TB / BT が重複なく 1 枚ずつ割り当てられる。
    #[test]
    fn direction_for_spec_idx_overrides_video_range_with_gui_directions() {
        let still_count = 8;
        let total = 12;
        // spec.direction は何を入れても video 領域では無視される。
        let spec = synth_spec(MotionDirection::LeftToRight);
        let mut seen: Vec<MotionDirection> = Vec::new();
        for spec_idx in still_count..total {
            let dir = direction_for_spec_idx(spec_idx, still_count, &spec);
            assert_eq!(dir, GUI_VIDEO_DIRECTIONS[spec_idx - still_count]);
            seen.push(dir);
        }
        assert_eq!(seen.len(), GUI_VIDEO_COUNT_DEFAULT);
        // 重複がない（4 方向揃い踏み）。
        let mut sorted = seen.clone();
        sorted.sort_by_key(|d| format!("{d:?}"));
        sorted.dedup_by_key(|d| format!("{d:?}"));
        assert_eq!(sorted.len(), GUI_VIDEO_COUNT_DEFAULT);
    }

    /// 境界: spec_idx == still_count - 1 は静止 (spec.direction)、
    /// spec_idx == still_count は video (GUI_VIDEO_DIRECTIONS[0])。
    #[test]
    fn direction_for_spec_idx_boundary() {
        let still_count = 8;
        let spec = synth_spec(MotionDirection::BottomToTop);
        assert_eq!(
            direction_for_spec_idx(still_count - 1, still_count, &spec),
            MotionDirection::BottomToTop,
            "spec_idx == still_count - 1 is still range"
        );
        assert_eq!(
            direction_for_spec_idx(still_count, still_count, &spec),
            GUI_VIDEO_DIRECTIONS[0],
            "spec_idx == still_count is video range index 0"
        );
    }

    /// #77: 動画タイル領域の speed は GUI_VIDEO_SPEEDS で固定割当される。
    /// VerySlow / Slow が必ず 2 枚ずつ（4 タイルがガチャ感を保つ最小条件）。
    #[test]
    fn speed_for_spec_idx_overrides_video_range_with_assigned_speeds() {
        let still_count = 8;
        let total = 12;
        // spec.speed は何を入れても video 領域では無視される。
        let mut spec = synth_spec(MotionDirection::LeftToRight);
        spec.speed = MotionSpeed::Slow;
        let mut very_slow = 0;
        let mut slow = 0;
        for spec_idx in still_count..total {
            let s = speed_for_spec_idx(spec_idx, still_count, &spec);
            assert_eq!(s, GUI_VIDEO_SPEEDS[spec_idx - still_count]);
            match s {
                MotionSpeed::VerySlow => very_slow += 1,
                MotionSpeed::Slow => slow += 1,
                // GUI_VIDEO_SPEEDS は現在 VerySlow / Slow しか含まないので
                // この arm に来たら GUI_VIDEO_SPEEDS の定義変更ミス。
                // (Phase B でも GUI_VIDEO_SPEEDS は変更していない)
                MotionSpeed::Mid | MotionSpeed::Fast => {
                    panic!("GUI_VIDEO_SPEEDS unexpectedly contains Mid/Fast: {s:?}")
                }
            }
        }
        // 4 タイルで 2 + 2 のばらけが固定で保証される。
        assert_eq!(very_slow, 2, "must have exactly 2 VerySlow tiles");
        assert_eq!(slow, 2, "must have exactly 2 Slow tiles");
    }

    /// 静止画タイル領域では spec.speed をそのまま使う。
    #[test]
    fn speed_for_spec_idx_returns_spec_speed_for_still_range() {
        let still_count = 8;
        let mut spec = synth_spec(MotionDirection::TopToBottom);
        spec.speed = MotionSpeed::VerySlow;
        for spec_idx in 0..still_count {
            assert_eq!(
                speed_for_spec_idx(spec_idx, still_count, &spec),
                MotionSpeed::VerySlow,
                "still tile {spec_idx} must inherit spec.speed"
            );
        }
    }

    #[test]
    fn pack_render_data_matches_core_pack_helper() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = base_params();
        p.k = 2;
        p.source_width = 2;
        p.source_height = 2;
        p.source_rgb = vec![
            255, 0, 0, 255, 0, 0, //
            0, 0, 255, 0, 0, 255,
        ];
        let total = 12usize;
        let spec_idx = 3usize;
        let still_count = total - GUI_VIDEO_COUNT_DEFAULT;
        let (_, bg, clusters) = get_or_build_clusters(&mut p).expect("clusters should build");
        let specs = random_batch_specs(42, total, still_count);
        let spec = specs[spec_idx];
        let speed = speed_for_spec_idx(spec_idx, still_count, &spec);
        let direction = direction_for_spec_idx(spec_idx, still_count, &spec);
        let direction_id = match direction {
            MotionDirection::LeftToRight => 0.0,
            MotionDirection::RightToLeft => 1.0,
            MotionDirection::TopToBottom => 2.0,
            MotionDirection::BottomToTop => 3.0,
        };
        let softness = parse_softness_preset("").unwrap();
        let n_orbs = spec.count.clamp(1, MAX_ORB_COUNT);
        let buf = pack_render_data(
            &clusters,
            bg,
            (64f32.min(64.0)) * 0.25 * spec.orb_size.max(0.0),
            (spec.blur + softness.blur_offset()).clamp(0.0, 1.0),
            direction_id,
            speed.cycle_count() as f32,
            spec.seed,
            n_orbs,
            softness.alpha_mul().clamp(0.0, 1.0),
            1.0,
            true,
            softness.edge_softness(),
            SHADOW_STRENGTH_DEFAULT,
        );
        let expected = orber_core::animate::pack_render_data(
            &clusters,
            bg,
            (64f32.min(64.0)) * 0.25 * spec.orb_size.max(0.0),
            (spec.blur + softness.blur_offset()).clamp(0.0, 1.0),
            direction_id,
            speed.cycle_count() as f32,
            spec.seed,
            n_orbs,
            softness.alpha_mul().clamp(0.0, 1.0),
            1.0,
            true,
            softness.edge_softness(),
            SHADOW_STRENGTH_DEFAULT,
        );
        assert_eq!(buf, expected);
    }

    /// #205: pack header[12] に softness.edge_softness() がそのまま
    /// 入っていることを担保する（wasm pack_render_data ラッパ経由）。
    #[test]
    fn pack_render_data_header_includes_edge_softness() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = base_params();
        p.k = 2;
        p.source_width = 2;
        p.source_height = 2;
        p.source_rgb = vec![
            255, 0, 0, 255, 0, 0, //
            0, 0, 255, 0, 0, 255,
        ];
        let total = 12usize;
        let spec_idx = 0usize;
        let still_count = total - GUI_VIDEO_COUNT_DEFAULT;
        let (_, bg, clusters) = get_or_build_clusters(&mut p).expect("clusters should build");
        let specs = random_batch_specs(42, total, still_count);
        let spec = specs[spec_idx];
        let speed = speed_for_spec_idx(spec_idx, still_count, &spec);
        // Low / Mid / High それぞれで header[12] が edge_softness() と一致すること。
        for preset in [
            SoftnessPreset::Low,
            SoftnessPreset::Mid,
            SoftnessPreset::High,
        ] {
            let n_orbs = spec.count.clamp(1, MAX_ORB_COUNT);
            let buf = pack_render_data(
                &clusters,
                bg,
                (64f32) * 0.25 * spec.orb_size.max(0.0),
                (spec.blur + preset.blur_offset()).clamp(0.0, 1.0),
                0.0,
                speed.cycle_count() as f32,
                spec.seed,
                n_orbs,
                preset.alpha_mul().clamp(0.0, 1.0),
                1.0,
                true,
                preset.edge_softness(),
                SHADOW_STRENGTH_DEFAULT,
            );
            assert!((buf[12] - preset.edge_softness()).abs() < 1e-6);
        }
    }

    // ---- #230 / #247: build_gpu_render_inputs（WGSL canvas-present 経路）の
    //      orb pack 不変条件。旧 WebGL 入口 build_render_pack の削除（#247）で、
    //      この pack 契約は生きている WGSL 経路（build_gpu_render_inputs().pack）
    //      に対するテストとして引き継ぐ ----

    /// #230 のテスト共通: kmeans が決定的に 2 クラスタへ収束する 2x2 赤/青
    /// ソース（`pack_render_data_matches_core_pack_helper` と同じソース）。
    /// 呼ぶたびに同一値を新規構築する（`WasmParams` は Clone ではないため、
    /// 「同一 params で 2 回呼ぶ」テストはこのヘルパを 2 回呼んで実現する）。
    fn small_source_params() -> WasmParams {
        let mut p = base_params();
        p.k = 2;
        p.source_width = 2;
        p.source_height = 2;
        p.source_rgb = vec![
            255, 0, 0, 255, 0, 0, //
            0, 0, 255, 0, 0, 255,
        ];
        p
    }

    /// #230 (A1) → #247: orb pack は同一 params + n + spec_idx に対して
    /// 完全に決定論的（2 回呼んで `Vec<f32>` が要素単位で完全一致する）。
    /// 生きている WGSL 経路（`gpu_set_render_data` が使う `build_gpu_render_inputs`）
    /// の orb pack に対してこの不変条件を固定する。
    #[test]
    fn gpu_orb_pack_is_deterministic_for_same_inputs() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let a = build_gpu_render_inputs(small_source_params(), 12, 3)
            .expect("gpu inputs should build")
            .pack;
        // 注: 2 回目は cluster キャッシュヒット。kmeans 再計算の決定論は実証範囲外
        // （spec 再構築 / per-orb RNG / pack エンコードの leg は真に再実行される）。
        let b = build_gpu_render_inputs(small_source_params(), 12, 3)
            .expect("gpu inputs should build")
            .pack;
        assert!(!a.is_empty(), "pack must not be empty");
        assert_eq!(
            a, b,
            "same params + n + spec_idx must yield an identical pack"
        );
    }

    /// #230 (A2) → #247: spec_idx の範囲チェック境界。n=12（total=12）で 0 / 11 は
    /// Ok、12 / 100 は Err。エラー文言は `"spec_idx {i} is out of range [0, {total})"`
    /// を一字一句維持する。境界検証は `resolve_frame` にあり、生きている WGSL 経路
    /// （`build_gpu_render_inputs`）がそれを通すので、そちらに対して固定する。
    #[test]
    fn gpu_render_inputs_spec_idx_boundary_and_error_wording() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // Ok 側境界: 0（先頭）と 11（total - 1）。
        assert!(build_gpu_render_inputs(small_source_params(), 12, 0).is_ok());
        assert!(build_gpu_render_inputs(small_source_params(), 12, 11).is_ok());
        // Err 側境界: 12（== total）と範囲外 100。文言まで一致すること。
        // GpuRenderInputs は Debug 非実装なので unwrap_err は使わず Err を直接取り出す。
        assert_eq!(
            build_gpu_render_inputs(small_source_params(), 12, 12).err(),
            Some("spec_idx 12 is out of range [0, 12)".to_string())
        );
        assert_eq!(
            build_gpu_render_inputs(small_source_params(), 12, 100).err(),
            Some("spec_idx 100 is out of range [0, 12)".to_string())
        );
    }

    /// #230 (A4) → #247: still/video タイル境界（still_count = total -
    /// GUI_VIDEO_COUNT_DEFAULT）。n=12 なら spec_idx 7 が最後の still、8 が
    /// 最初の video。両方 orb pack が生成でき、count_preset="low" で n_orbs を
    /// 両者 10 に固定すれば長さは同一・内容は異なる（video 側は direction /
    /// speed が GUI_VIDEO_* 固定割当に切り替わり、spec 自体の seed も違う）。
    /// 生きている WGSL 経路（`build_gpu_render_inputs`）の orb pack で固定する。
    #[test]
    fn gpu_orb_pack_still_video_boundary_same_len_different_content() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let total = 12u32;
        let still_count = total as usize - GUI_VIDEO_COUNT_DEFAULT; // 8
        let params_low = || {
            let mut p = small_source_params();
            // n_orbs を spec.count（10..=50 抽選）でなく 10 に固定し、
            // 「長さ同一」の比較を per-orb 数の偶然に依存させない。
            p.count_preset = "low".into();
            p
        };
        let last_still = build_gpu_render_inputs(params_low(), total, (still_count - 1) as u32)
            .expect("last still tile inputs should build")
            .pack;
        let first_video = build_gpu_render_inputs(params_low(), total, still_count as u32)
            .expect("first video tile inputs should build")
            .pack;
        assert_eq!(
            last_still.len(),
            first_video.len(),
            "count_preset=low pins both tiles to n_orbs=10, so pack lengths must match"
        );
        assert_ne!(
            last_still, first_video,
            "still/video boundary tiles must differ in content (direction/speed/seed)"
        );
    }

    // ---- #231: build_gpu_render_inputs（WGSL canvas-present 経路の入力構築） ----

    /// #231: orb shape の WGSL 入力。`opts.shape` が Orb に解決され、orb pack が
    /// 非空であることを固定する（#230 / #242 のルックを温存する shape_id=0.0 pack）。
    #[test]
    fn build_gpu_render_inputs_orb_resolves_shape_and_packs() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let gpu = build_gpu_render_inputs(small_source_params(), 12, 3).expect("gpu inputs");
        assert!(matches!(gpu.opts.shape, OrbShape::Orb));
        assert!(!gpu.pack.is_empty(), "orb pack must not be empty");
        assert_eq!(
            gpu.pack[10], 0.0,
            "orb pack header shape_id must be 0 (Orb)"
        );
    }

    /// #231: glyph shape の WGSL 入力。opts.shape が Glyph に解決され、glyph_rotate が
    /// 伝播すること（#136）。clusters / opts は core の render_frame_glyph_to_view に渡る。
    #[test]
    fn build_gpu_render_inputs_glyph_resolves_shape() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "☆".into();
        p.glyph_rotate = false;
        let gpu = build_gpu_render_inputs(p, 12, 0).expect("gpu inputs");
        match gpu.opts.shape {
            OrbShape::Glyph { ch, .. } => assert_eq!(ch, '☆'),
            other => panic!("expected Glyph, got {other:?}"),
        }
        assert!(
            !gpu.opts.glyph_rotate,
            "glyph_rotate must propagate to opts"
        );
    }

    /// #239 Phase 1: 製品の 3 段にじみボタン（`bleed_preset`）が Web GPU 経路の
    /// `AnimateOptions.aqua` に伝播する。weak/mid/strong → `aqua_bleed` 0.15/0.3/0.5、
    /// bloom/offset/halo は未指定なら 0（オフ）。orb / glyph / image いずれの shape でも
    /// engage することを固定する。
    #[test]
    fn bleed_preset_propagates_to_animate_options() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        for shape in ["orb", "glyph", "image"] {
            for (preset, want) in [("weak", 0.15_f32), ("mid", 0.3), ("strong", 0.5)] {
                let mut p = small_source_params();
                p.shape = shape.into();
                p.bleed_preset = preset.into();
                // glyph / image は shape 解決に追加入力が要る。orb は不要。
                if shape == "glyph" {
                    p.glyph_char = "☆".into();
                } else if shape == "image" {
                    let (w, h) = (16u32, 16u32);
                    p.image_mask_width = w;
                    p.image_mask_height = h;
                    let mut rgba = vec![0u8; (w * h * 4) as usize];
                    for y in 0..h {
                        for x in 0..w {
                            let i = ((y * w + x) * 4) as usize;
                            let a = if y < h / 2 { 255 } else { 0 };
                            rgba[i] = 255;
                            rgba[i + 1] = 255;
                            rgba[i + 2] = 255;
                            rgba[i + 3] = a;
                        }
                    }
                    p.image_mask_rgba = rgba;
                }
                let gpu = build_gpu_render_inputs(p, 12, 0)
                    .unwrap_or_else(|e| panic!("gpu inputs for {shape}/{preset}: {e}"));
                let aqua = gpu.opts.aqua.unwrap_or_else(|| {
                    panic!("bleed_preset {preset} on shape {shape} must engage aqua")
                });
                assert_eq!(
                    aqua.bleed, want,
                    "{shape}/{preset}: aqua_bleed must map to {want}"
                );
                // #239: character 軸 preset を渡していないので 3 軸とも 0（オフ）。
                assert_eq!(
                    aqua.bloom, 0.0,
                    "{shape}/{preset}: bloom off without bloom_preset"
                );
                assert_eq!(
                    aqua.offset, 0.0,
                    "{shape}/{preset}: offset off without offset_preset"
                );
                assert_eq!(
                    aqua.halo, 0.0,
                    "{shape}/{preset}: halo off without halo_preset"
                );
            }
        }
    }

    /// #239 Phase 1: bloom / halo / offset の character 3 段ボタンが Web GPU 経路の
    /// `AnimateOptions.aqua` の各フィールドへ独立に伝播する。weak/mid/strong →
    /// 0.3/0.6/0.9（CLI の --bloom/--halo/--offset と同じ）。にじみ（bleed_preset）が
    /// engage していることが前提。
    #[test]
    fn character_presets_propagate_to_animate_options() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        for (axis, want) in [("weak", 0.3_f32), ("mid", 0.6), ("strong", 0.9)] {
            // bloom 軸だけ指定（にじみは mid で engage）。
            let mut p = small_source_params();
            p.bleed_preset = "mid".into();
            p.bloom_preset = axis.into();
            let aqua = build_gpu_render_inputs(p, 12, 0)
                .expect("inputs")
                .opts
                .aqua
                .expect("bleed engaged");
            assert_eq!(aqua.bloom, want, "bloom {axis} -> {want}");
            assert_eq!(aqua.halo, 0.0, "halo stays off");
            assert_eq!(aqua.offset, 0.0, "offset stays off");

            // halo 軸だけ指定。
            let mut p = small_source_params();
            p.bleed_preset = "mid".into();
            p.halo_preset = axis.into();
            let aqua = build_gpu_render_inputs(p, 12, 0)
                .expect("inputs")
                .opts
                .aqua
                .expect("bleed engaged");
            assert_eq!(aqua.halo, want, "halo {axis} -> {want}");
            assert_eq!(aqua.bloom, 0.0, "bloom stays off");
            assert_eq!(aqua.offset, 0.0, "offset stays off");

            // offset 軸だけ指定。
            let mut p = small_source_params();
            p.bleed_preset = "mid".into();
            p.offset_preset = axis.into();
            let aqua = build_gpu_render_inputs(p, 12, 0)
                .expect("inputs")
                .opts
                .aqua
                .expect("bleed engaged");
            assert_eq!(aqua.offset, want, "offset {axis} -> {want}");
            assert_eq!(aqua.bloom, 0.0, "bloom stays off");
            assert_eq!(aqua.halo, 0.0, "halo stays off");
        }
    }

    /// #239 Phase 1: character 軸 preset の写像表（`""` = 0 / weak/mid/strong =
    /// 0.3/0.6/0.9 / 不正値はエラー）を固定する。
    #[test]
    fn parse_character_preset_table() {
        assert_eq!(parse_character_preset("", "bloom").unwrap(), 0.0);
        assert_eq!(parse_character_preset("weak", "bloom").unwrap(), 0.3);
        assert_eq!(parse_character_preset("mid", "halo").unwrap(), 0.6);
        assert_eq!(parse_character_preset("strong", "offset").unwrap(), 0.9);
        assert!(parse_character_preset("ultra", "bloom").is_err());
    }

    /// #239 Phase 1: にじみオフ（`bleed_preset == ""`）なら character 軸を指定しても
    /// `aqua` は `None`（にじみが無いと 3 軸とも効かない設計）。従来 Web 出力と
    /// byte-identical を保つ。
    #[test]
    fn character_presets_ignored_without_bleed() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.bloom_preset = "strong".into();
        p.halo_preset = "strong".into();
        p.offset_preset = "strong".into();
        let gpu = build_gpu_render_inputs(p, 12, 0).expect("inputs");
        assert!(
            gpu.opts.aqua.is_none(),
            "without bleed, character axes must keep aqua None (byte-identical)"
        );
    }

    /// #239 Phase 1: 無効な character 軸 preset は `build_gpu_render_inputs` がエラーに
    /// する（parse_character_preset の Err が resolve_frame 経由で伝播）。
    #[test]
    fn character_preset_invalid_value_errors() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.bleed_preset = "mid".into();
        p.bloom_preset = "ultra".into();
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "invalid bloom_preset must be rejected"
        );
    }

    /// #239 Phase 1 ★byte ゲート: character 軸 preset も orb pack には一切触れない
    /// （character はすべて aqua 経由）。`bleed`/`bloom`/`halo`/`offset` を全部盛っても
    /// orb pack は byte-identical。
    #[test]
    fn character_presets_do_not_touch_orb_pack() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let pack_off = build_gpu_render_inputs(small_source_params(), 12, 3)
            .expect("inputs (off)")
            .pack;
        let mut on = small_source_params();
        on.bleed_preset = "strong".into();
        on.bloom_preset = "strong".into();
        on.halo_preset = "strong".into();
        on.offset_preset = "strong".into();
        let pack_on = build_gpu_render_inputs(on, 12, 3)
            .expect("inputs (all on)")
            .pack;
        assert_eq!(
            pack_off, pack_on,
            "orb pack must be byte-identical regardless of character presets (they ride aqua, not the pack)"
        );
    }

    /// #239 Phase 1 ★非リグレッションゲート: `bleed_preset` を送らない（既定 `""`）と
    /// `AnimateOptions.aqua` は `None` のまま。orb / glyph / image どの shape でも
    /// 従来の Web 出力と byte-identical（aqua = None = にじみオフ）。
    #[test]
    fn bleed_preset_absent_keeps_aqua_none() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // orb（追加入力不要）。
        let gpu = build_gpu_render_inputs(small_source_params(), 12, 0).expect("orb inputs");
        assert!(
            gpu.opts.aqua.is_none(),
            "without bleed_preset, orb must keep aqua None (byte-identical)"
        );
        // glyph。
        let mut g = small_source_params();
        g.shape = "glyph".into();
        g.glyph_char = "☆".into();
        let gpu = build_gpu_render_inputs(g, 12, 0).expect("glyph inputs");
        assert!(
            gpu.opts.aqua.is_none(),
            "without bleed_preset, glyph must keep aqua None (byte-identical)"
        );
    }

    /// #239 Phase 1: 無効な `bleed_preset` は `build_gpu_render_inputs` がエラーにする
    /// （parse_bleed_preset の Err が resolve_frame 経由で伝播）。
    #[test]
    fn bleed_preset_invalid_value_errors() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.bleed_preset = "ultra".into();
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "invalid bleed_preset must be rejected"
        );
    }

    /// #239 Phase 1 ★最重要 byte ゲート: `bleed_preset` を送らない既定の orb pack /
    /// 入力は、にじみ機能追加の前後で何も変わらない。`bleed_preset` フィールドは
    /// `aqua` だけを制御し、orb pack（header + per-orb スカラ）には一切触れない。
    /// `weak` を送っても orb pack は byte-identical（にじみは pack ではなく
    /// `AnimateOptions.aqua` 経由でシェーダに乗るため）。
    #[test]
    fn bleed_preset_does_not_touch_orb_pack() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let pack_off = build_gpu_render_inputs(small_source_params(), 12, 3)
            .expect("inputs (bleed off)")
            .pack;
        let mut on = small_source_params();
        on.bleed_preset = "weak".into();
        let pack_on = build_gpu_render_inputs(on, 12, 3)
            .expect("inputs (bleed weak)")
            .pack;
        assert_eq!(
            pack_off, pack_on,
            "orb pack must be byte-identical regardless of bleed_preset (bleed rides aqua, not the pack)"
        );
    }

    /// #247: orb pack が `glyph_sdf` を読まない回帰ガード（旧 WebGL `build_render_pack`
    /// の不変条件を、生きている WGSL 経路 `build_gpu_render_inputs` の `.pack` へ移植）。
    /// shape="glyph" に有効な `glyph_sdf`（非空・16..=1024 サイズ・len 整合）を与えると
    /// `opts.shape` は SDF シルエット経路（`OrbShape::Image`）に変わるが、orb pack
    /// （header + per-orb スカラ）は `resolve_frame` 由来で glyph_sdf を一切読まないため、
    /// glyph_sdf の有無で `.pack` は byte-identical でなければならない（漏れれば落ちる）。
    #[test]
    fn gpu_orb_pack_ignores_glyph_sdf() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // glyph_sdf 無し（core フォント経路の glyph）。
        let mut without = small_source_params();
        without.shape = "glyph".into();
        without.glyph_char = "☆".into();
        let pack_without = build_gpu_render_inputs(without, 12, 0)
            .expect("inputs (no glyph_sdf)")
            .pack;

        // glyph_sdf 有り（JS フォールバック SDF を載せると shape は Image に解決されるが、
        // orb pack は glyph_sdf を読まないので不変）。
        let mut with = small_source_params();
        with.shape = "glyph".into();
        with.glyph_char = "☆".into();
        let size = 256u32;
        with.glyph_sdf_size = size;
        with.glyph_sdf = vec![128u8; (size * size) as usize];
        let pack_with = build_gpu_render_inputs(with, 12, 0)
            .expect("inputs (with glyph_sdf)")
            .pack;

        assert_eq!(
            pack_without, pack_with,
            "orb pack must be byte-identical with or without glyph_sdf (glyph_sdf is shape-only)"
        );
    }

    // ---- #245: transparent_background（透過 export の wasm 入口） ----

    /// #245 (a): `transparent_background = true` は orb pack header word 3（bg.a）
    /// **だけ**を 0 にする。他の全 word は不透過版と完全一致（旧 WebGL worker の
    /// `withTransparentBackground`＝「word 3 のみ 0 上書き」と同じ契約を wasm
    /// 入口で固定する）。既定 `false` は従来とバイト列不変。生きている WGSL 経路
    /// （`build_gpu_render_inputs`）の orb pack で固定する（#247）。
    #[test]
    fn transparent_background_zeroes_only_pack_bg_alpha() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let opaque = build_gpu_render_inputs(small_source_params(), 12, 3)
            .expect("opaque inputs")
            .pack;
        let mut p = small_source_params();
        p.transparent_background = true;
        let transparent = build_gpu_render_inputs(p, 12, 3)
            .expect("transparent inputs")
            .pack;
        assert_eq!(opaque.len(), transparent.len());
        assert_ne!(opaque[3], 0.0, "derived bg must be opaque by default");
        assert_eq!(transparent[3], 0.0, "bg.a must be zeroed");
        for (i, (a, b)) in opaque.iter().zip(transparent.iter()).enumerate() {
            if i == 3 {
                continue;
            }
            assert_eq!(a, b, "pack word {i} must be unchanged by transparency");
        }
    }

    /// #245 (b): WGSL 経路（glyph / image が読む `AnimateOptions`）
    /// にも透過が伝播する: `opts.background[3] == 0`。orb pack 側（header[3]）と
    /// 二経路が同時に透過になることを固定する。
    #[test]
    fn transparent_background_propagates_to_animate_options() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "☆".into();
        p.transparent_background = true;
        let gpu = build_gpu_render_inputs(p, 12, 0).expect("gpu inputs");
        assert_eq!(gpu.opts.background[3], 0, "opts.background alpha must be 0");
        assert_eq!(gpu.pack[3], 0.0, "orb pack header bg.a must be 0 too");
    }

    /// shape="image" の有効マスク（上半分不透明 / 下半分透明 16×16 RGBA）が
    /// WGSL 入口を通って `opts.shape == OrbShape::Image` に解決し、SDF サイズが
    /// DEFAULT_GLYPH_SDF_SIZE(256)・`sdf.len() == size * size` であることを固定する
    /// （resolve_orb_shape の image 解決が build_gpu_render_inputs 経由でも生きること）。
    ///
    /// 併せて `inputs.clusters` が非空であることを assert する。`clusters` は
    /// `gpu_set_render_data`（wasm32 専用）からしか読まれず native の test target では
    /// dead_code になるため、ここでテストが実際に読むことで
    /// `cargo clippy --all-targets -- -D warnings` を通す（allow(dead_code) で誤魔化さない）。
    #[test]
    fn build_gpu_render_inputs_image_resolves_shape() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "image".into();
        let (w, h) = (16u32, 16u32);
        p.image_mask_width = w;
        p.image_mask_height = h;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let a = if y < h / 2 { 255 } else { 0 };
                rgba[i] = 255;
                rgba[i + 1] = 255;
                rgba[i + 2] = 255;
                rgba[i + 3] = a;
            }
        }
        p.image_mask_rgba = rgba;
        let gpu = build_gpu_render_inputs(p, 12, 0).expect("image GPU inputs should build");
        match gpu.opts.shape {
            OrbShape::Image { size, sdf } => {
                assert_eq!(size, orber_core::glyph::DEFAULT_GLYPH_SDF_SIZE);
                assert_eq!(sdf.len(), (size * size) as usize);
            }
            other => panic!("expected OrbShape::Image, got {other:?}"),
        }
        // clusters は render_frame_image_to_view（wasm32）へ渡る本物のデータ。
        // native test がこのフィールドを読むことで dead_code 警告を解消する。
        assert!(
            !gpu.clusters.is_empty(),
            "kmeans should yield at least one cluster for the source image"
        );
    }

    /// shape="image" の無効マスクは WGSL 入口で Err になる（resolve_image_shape の
    /// 検証が build_gpu_render_inputs 経由でも生きること）。(a) w/h=0・(b) rgba 長さ
    /// 不一致・(c) 単色（無コントラスト）の 3 ケース。
    #[test]
    fn build_gpu_render_inputs_image_rejects_invalid_mask() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // (a) width / height = 0（small_source_params 既定のまま shape だけ image）。
        let mut p = small_source_params();
        p.shape = "image".into();
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "image with width/height=0 must error"
        );

        // (b) rgba 長さが width * height * 4 と一致しない。
        let mut p = small_source_params();
        p.shape = "image".into();
        p.image_mask_width = 2;
        p.image_mask_height = 2;
        p.image_mask_rgba = vec![0; 3]; // 2*2*4=16 ではない
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "image with mismatched rgba length must error"
        );

        // (c) 単色（全 0）はコントラストが取れず image_rgba_to_sdf が None。
        let mut p = small_source_params();
        p.shape = "image".into();
        p.image_mask_width = 4;
        p.image_mask_height = 4;
        p.image_mask_rgba = vec![0; 4 * 4 * 4];
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "image with a single flat color (no contrast) must error"
        );
    }

    /// 未知 shape は WGSL 入口で Err になる（resolve_orb_shape → parse_shape の
    /// 不正 shape reject が生きること）。"bogus" と "" の両方。
    #[test]
    fn build_gpu_render_inputs_rejects_unknown_shape() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "bogus".into();
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "shape='bogus' must error"
        );

        let mut p = small_source_params();
        p.shape = "".into();
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "shape='' must error"
        );
    }

    /// #231: shape="glyph" で JS フォールバック SDF（`glyph_sdf` / `glyph_sdf_size`）が
    /// 供給されたら、同梱フォント外の字でも `OrbShape::Image` に解決する（core 統一機構
    /// で glyph も image も同じ SDF 経路、#235）。size=256・len=256*256 の有効 SDF を渡し、
    /// `opts.shape == OrbShape::Image{size}`・`sdf.len() == size*size`・clusters 非空を固定する。
    /// glyph_char は同梱フォント外（漢字）を入れても、SDF 供給時は char を見ずに解決する。
    #[test]
    fn build_gpu_render_inputs_glyph_sdf_supplied_resolves_image() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "glyph".into();
        // 同梱 NotoSansSymbols2 に無い漢字。SDF 供給時は char を引かず SDF を直接使う。
        p.glyph_char = "字".into();
        let size = 256u32;
        p.glyph_sdf_size = size;
        // 中央付近を inside（255）にした非自明な SDF。中身の値は解決に関与しない
        // （len と size の整合だけ見る）が、全 0 でも len さえ合えば解決する。
        p.glyph_sdf = vec![128u8; (size * size) as usize];
        let gpu = build_gpu_render_inputs(p, 12, 0).expect("glyph SDF GPU inputs should build");
        match gpu.opts.shape {
            OrbShape::Image { size: got, sdf } => {
                assert_eq!(got, size, "resolved SDF size must match glyph_sdf_size");
                assert_eq!(sdf.len(), (size * size) as usize);
            }
            other => panic!("expected OrbShape::Image (glyph SDF fallback), got {other:?}"),
        }
        // clusters は render_frame_image_to_view（wasm32）へ渡る本物のデータ。
        assert!(
            !gpu.clusters.is_empty(),
            "kmeans should yield at least one cluster for the source image"
        );
    }

    /// #231: glyph SDF フォールバックの size 検証。`get_glyph_sdf` と同じ `16..=1024` で、
    /// 境界 16/1024 ちょうどは OK、15/1025 は Err（`>=`/`>` 取り違え狙い撃ち）。len 不一致も
    /// Err。境界 OK ケースは len も size*size に合わせて与える。
    #[test]
    fn build_gpu_render_inputs_glyph_sdf_validates() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        // 境界下限 16 ちょうどは OK（len = 16*16）。
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "字".into();
        p.glyph_sdf_size = 16;
        p.glyph_sdf = vec![0u8; 16 * 16];
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_ok(),
            "glyph_sdf_size == 16 (lower bound) must be accepted"
        );

        // 境界上限 1024 ちょうどは OK（len = 1024*1024）。
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "字".into();
        p.glyph_sdf_size = 1024;
        p.glyph_sdf = vec![0u8; 1024 * 1024];
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_ok(),
            "glyph_sdf_size == 1024 (upper bound) must be accepted"
        );

        // 15（下限未満）は Err（len は size*size に合わせても size 範囲で弾く）。
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "字".into();
        p.glyph_sdf_size = 15;
        p.glyph_sdf = vec![0u8; 15 * 15];
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "glyph_sdf_size == 15 (below lower bound) must error"
        );

        // 1025（上限超過）は Err。
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "字".into();
        p.glyph_sdf_size = 1025;
        p.glyph_sdf = vec![0u8; 1025 * 1025];
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "glyph_sdf_size == 1025 (above upper bound) must error"
        );

        // len 不一致（size は範囲内だが len != size*size）は Err。
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "字".into();
        p.glyph_sdf_size = 256;
        p.glyph_sdf = vec![0u8; 256 * 256 - 1]; // 1 バイト不足
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "glyph_sdf length != size*size must error"
        );
    }

    /// shape="glyph" + glyph_char="" は WGSL 入口で Err になる（first_char_of の
    /// 空文字 reject が resolve_orb_shape 経由でも維持されること）。
    #[test]
    fn build_gpu_render_inputs_glyph_empty_char_errors() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "glyph".into();
        p.glyph_char = "".into();
        assert!(
            build_gpu_render_inputs(p, 12, 0).is_err(),
            "glyph with an empty glyph_char must error"
        );
    }

    /// #231（仕様確定）: shape="glyph" + glyph_char="" でも、非空で有効な `glyph_sdf` が
    /// 供給されていれば char 検証をスキップして SDF を真とする（SDF がシルエットの真。
    /// char は不要、#235 の core 統一機構）。`resolve_orb_shape` は glyph_sdf 非空時に
    /// `resolve_glyph_sdf_shape` へ分岐し、`first_char_of` を一切呼ばないため、空 char でも
    /// `OrbShape::Image` に解決して Ok になる。`build_gpu_render_inputs_glyph_empty_char_errors`
    /// （glyph_sdf 空のとき空 char は Err）の対になる仕様固定。
    #[test]
    fn build_gpu_render_inputs_glyph_sdf_skips_char_validation() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "glyph".into();
        // char は空。SDF 供給時は char を見ないので、これでも解決する。
        p.glyph_char = "".into();
        let size = 256u32;
        p.glyph_sdf_size = size;
        p.glyph_sdf = vec![128u8; (size * size) as usize];
        let gpu = build_gpu_render_inputs(p, 12, 0)
            .expect("empty glyph_char + valid glyph_sdf must resolve (char skipped)");
        match gpu.opts.shape {
            OrbShape::Image { size: got, sdf } => {
                assert_eq!(got, size, "resolved SDF size must match glyph_sdf_size");
                assert_eq!(sdf.len(), (size * size) as usize);
            }
            other => panic!("expected OrbShape::Image (SDF is the truth), got {other:?}"),
        }
    }

    /// #55 / i18n: first_char_of は文字列の先頭 Unicode スカラを 1 つ採用する。
    /// "☆★" → '☆'、"🍕"（サロゲートペアになる絵文字）→ '🍕'、結合文字列でも
    /// 先頭スカラ（基底文字）を採る。マルチバイト境界をバイトでなく char で割ること。
    #[test]
    fn first_char_of_takes_first_scalar_multibyte() {
        assert_eq!(first_char_of("☆★").unwrap(), '☆');
        // 🍕 は U+1F355（BMP 外、UTF-16 ではサロゲートペア）。Rust char は
        // 単一スカラなので先頭で正しく取れること。
        assert_eq!(first_char_of("🍕").unwrap(), '🍕');
        // 結合文字列 "e" + U+0301（combining acute）は 2 スカラ。先頭は基底 'e'。
        assert_eq!(first_char_of("e\u{0301}").unwrap(), 'e');
    }

    // ---- #245: transparent_background の shape 別伝播 / キャッシュ非汚染 /
    //      serde default ----

    /// 上半分不透明 / 下半分透明のマスク RGBA（コントラストあり = Image に解決
    /// 可能）。`resolve_orb_shape_image_validates_mask` 等のインラインパターンと
    /// 同一の共有ヘルパ（#245 で追加テストが増えたため切り出し）。
    fn half_opaque_mask_rgba(w: u32, h: u32) -> Vec<u8> {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let a = if y < h / 2 { 255 } else { 0 };
                rgba[i] = 255;
                rgba[i + 1] = 255;
                rgba[i + 2] = 255;
                rgba[i + 3] = a;
            }
        }
        rgba
    }

    /// #245 (R1): shape="image" + transparent_background=true で、image が読む
    /// `AnimateOptions` 経路（render_frame_image_to_view）にも透過が伝播する
    /// （opts.background[3] == 0）。glyph は既存テストが押さえているので、
    /// ここでは image の経路を固定する。
    #[test]
    fn transparent_background_image_propagates_to_animate_options() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "image".into();
        p.image_mask_width = 16;
        p.image_mask_height = 16;
        p.image_mask_rgba = half_opaque_mask_rgba(16, 16);
        p.transparent_background = true;
        let gpu = build_gpu_render_inputs(p, 12, 0).expect("image GPU inputs");
        assert_eq!(
            gpu.opts.background[3], 0,
            "image opts.background alpha must be 0 when transparent_background is set"
        );
    }

    /// #245 (R3): 透過 → 不透過の順で同一 source を build しても、2 回目の
    /// bg.a は不透明のまま。`resolve_frame` は kmeans キャッシュ取得後の
    /// ローカル bg だけを書き換える設計で、キャッシュの bg を透過で汚すと
    /// この逆順（既存テストは不透過 → 透過の順だけ）で初めて露出する —
    /// その死角を regression として固定する。
    #[test]
    fn transparent_then_opaque_does_not_poison_cluster_cache() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.transparent_background = true;
        let transparent = build_gpu_render_inputs(p, 12, 3)
            .expect("transparent inputs")
            .pack;
        assert_eq!(
            transparent[3], 0.0,
            "first (transparent) build must have bg.a == 0"
        );
        // 2 回目: 同一 source（fingerprint 一致 = キャッシュヒット）の不透過 build。
        let opaque = build_gpu_render_inputs(small_source_params(), 12, 3)
            .expect("opaque inputs")
            .pack;
        assert_ne!(
            opaque[3], 0.0,
            "second (opaque) build must keep the cached bg opaque (cache must not be poisoned)"
        );
    }

    /// #245 (R4): `transparent_background` の serde 省略 default は `false`
    /// （フィールドを送らない既存呼び出しはバイト列不変）。明示 true はそのまま
    /// 通る。serde_json は serde_wasm_bindgen と同じ `Deserialize` 実装を通る
    /// ので `#[serde(default)]` の検証として等価（shadow_strength の前例
    /// `shadow_strength_serde_defaults_to_production_constant` と同型）。
    #[test]
    fn transparent_background_serde_defaults_to_false() {
        let json = r#"{
            "source_rgb": [0, 0, 0],
            "source_width": 1,
            "source_height": 1,
            "k": 1,
            "width": 8,
            "height": 8,
            "seed": 1.0,
            "direction": "lr",
            "speed": "slow",
            "count": 1,
            "orb_size": 1.0,
            "blur": 0.5,
            "shape": "orb"
        }"#;
        let p: WasmParams =
            serde_json::from_str(json).expect("params without transparent_background");
        assert!(
            !p.transparent_background,
            "omitted transparent_background must default to false"
        );
        // 明示指定はそのまま通る。
        let json_explicit = json.replace(
            r#""shape": "orb""#,
            r#""shape": "orb", "transparent_background": true"#,
        );
        let p: WasmParams =
            serde_json::from_str(&json_explicit).expect("explicit transparent_background");
        assert!(p.transparent_background);
    }

    // ---- #245: readback 行 padding 純関数（gpu_render_rgba の土台） ----

    /// #245 (R5): `padded_bytes_per_row` は width*4 を 256 byte alignment に
    /// 切り上げる。63 (252B → 256) / 64 (256B ちょうど) / 65 (260B → 512) の
    /// 3 点境界で切り上げ方向を固定する。
    #[test]
    fn padded_bytes_per_row_boundaries() {
        assert_eq!(padded_bytes_per_row(63), 256);
        assert_eq!(padded_bytes_per_row(64), 256);
        assert_eq!(padded_bytes_per_row(65), 512);
    }

    /// #245 (R6): width=63（行末 4B の padding あり）の `unpad_rows` で、
    /// padding バイトが出力に混入せず、出力長が width*height*4 になる。
    #[test]
    fn unpad_rows_drops_row_padding() {
        let (width, height) = (63u32, 3u32);
        let padded = padded_bytes_per_row(width);
        assert_eq!(padded, 256);
        let unpadded = (width * 4) as usize; // 252
                                             // 各行: payload は行番号+1、padding は 0xEE のマーカー。
        let mut data = vec![0xEEu8; padded as usize * height as usize];
        for row in 0..height as usize {
            data[row * padded as usize..row * padded as usize + unpadded].fill(row as u8 + 1);
        }
        let out = unpad_rows(&data, width, height, padded);
        assert_eq!(out.len(), unpadded * height as usize);
        assert!(
            out.iter().all(|&b| b != 0xEE),
            "row padding must not leak into the output"
        );
        for (row, chunk) in out.chunks_exact(unpadded).enumerate() {
            assert!(
                chunk.iter().all(|&b| b == row as u8 + 1),
                "row {row} payload must be preserved in order"
            );
        }
    }

    /// #245 (R7): width=64 は padded == unpadded（256B ちょうど）で、
    /// `unpad_rows` は恒等コピーになる。
    #[test]
    fn unpad_rows_width64_is_identity_copy() {
        let (width, height) = (64u32, 2u32);
        let padded = padded_bytes_per_row(width);
        assert_eq!(padded, width * 4, "width=64 must have no padding");
        let data: Vec<u8> = (0..(padded * height) as usize)
            .map(|i| (i % 251) as u8)
            .collect();
        let out = unpad_rows(&data, width, height, padded);
        assert_eq!(out, data, "no-padding input must round-trip byte-identical");
    }

    /// #245 (R8): 最小値 width=1 / height=1。1px 行（4B）が 256B に pad され、
    /// `unpad_rows` は先頭 4B だけを返す。
    #[test]
    fn unpad_rows_minimal_1x1() {
        let (width, height) = (1u32, 1u32);
        let padded = padded_bytes_per_row(width);
        assert_eq!(padded, 256);
        let mut data = vec![0xEEu8; 256];
        data[..4].copy_from_slice(&[1, 2, 3, 4]);
        let out = unpad_rows(&data, width, height, padded);
        assert_eq!(out, vec![1, 2, 3, 4]);
    }

    /// #245 (R9): image の無コントラスト Err 文言固定。Web worker
    /// （orberWorker.ts::setRenderData）は 'no usable silhouette contrast' の
    /// includes で sentinel `image-shape-no-contrast` にマップするため、この
    /// 部分文字列が変わると Studio の i18n エラー表示が silent に素通り文言へ
    /// 化ける（#169 型の文字列 drift 事故防止）。
    /// SYNC WITH web/src/lib/orberWorker.ts::setRenderData
    #[test]
    fn image_no_contrast_error_wording_is_pinned() {
        let _guard = CACHE_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let mut p = small_source_params();
        p.shape = "image".into();
        p.image_mask_width = 4;
        p.image_mask_height = 4;
        p.image_mask_rgba = vec![0; 4 * 4 * 4]; // 単色 = コントラスト無し
                                                // GpuRenderInputs は Debug を持たないので unwrap_err でなく match で取り出す。
        let err = match build_gpu_render_inputs(p, 12, 0) {
            Err(e) => e,
            Ok(_) => panic!("a flat (no-contrast) mask must error"),
        };
        assert!(
            err.contains("no usable silhouette contrast"),
            "worker sentinel mapping depends on this wording, got: {err}"
        );
    }
}
