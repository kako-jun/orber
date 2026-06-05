//! orber-core — pure rendering core (wasm-buildable, no I/O, no subprocess).

pub mod animate;
pub mod cluster;
pub mod color_track;
pub mod glyph;
/// wgpu (Rust + WGSL) production render path (#207). Native CLI only; behind the
/// `gpu` feature so the wasm32 build stays minimal. Since #225 this is the **only**
/// renderer (the CPU pixel renderer was purged); it draws all four shapes
/// (Circle / Glyph / Aquarelle / Image) in WGSL.
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod keyframe_track;
pub mod orb;
pub mod output_mode;
pub mod style;
pub mod variations;
