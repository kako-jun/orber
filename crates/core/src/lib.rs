//! orber-core — pure rendering core (wasm-buildable, no I/O, no subprocess).

pub mod animate;
/// Re-export of the standalone [`aquarelle`](https://crates.io/crates/aquarelle)
/// crate so existing call sites can keep writing `orber_core::aquarelle::…`
/// after the v0.4 extraction (closes orber Issue #10).
pub use aquarelle;
pub mod batch;
pub mod cluster;
pub mod color_track;
pub mod glyph;
pub mod keyframe_track;
pub mod orb;
pub mod output_mode;
pub mod style;
pub mod variations;
