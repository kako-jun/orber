//! orber-core — pure rendering core (wasm-buildable, no I/O, no subprocess).

pub mod animate;
pub mod cluster;
pub mod color_track;
pub mod glyph;
/// wgpu (Rust + WGSL) production render path (#207). Behind the `gpu` feature so
/// the default wasm32 build stays minimal. Since #225 this is the **only**
/// renderer (the CPU pixel renderer was purged); it draws the three silhouette
/// shapes (orb / glyph / image) in WGSL, with the watercolor bleed as an additive
/// layer on any of them (#239). Since #229 it also builds on
/// wasm32 (WebGPU backend): the async `new_async` + `*_to_view` (surface present)
/// API is the browser path, while the read-back `render_frame*` / `render_packed`
/// API stays native-only.
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod keyframe_track;
pub mod orb;
pub mod output_mode;
pub mod style;
pub mod variations;
