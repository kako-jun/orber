//! orber-core — pure rendering core (wasm-buildable, no I/O, no subprocess).

pub mod animate;
pub mod batch;
pub mod cluster;
pub mod color_track;
pub mod glyph;
/// wgpu (Rust + WGSL) production render path (#207, Phase 0). Native CLI only;
/// behind the `gpu` feature so the wasm32 build stays minimal. Renders the
/// **Circle** orb path in WGSL and matches the CPU (tiny-skia) oracle within
/// ±2/channel.
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod keyframe_track;
pub mod orb;
pub mod output_mode;
pub mod style;
pub mod variations;
