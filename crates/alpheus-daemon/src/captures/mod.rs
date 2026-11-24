//! Captures: the daemon takes screenshots natively.
//!
//! This folder holds the capture pipeline. `screencopy.rs` is the only file that speaks the
//! screencopy protocol and turns one output into a `Frame`; `encode.rs` writes a PNG.
//! The pure flow and the glue come in later files of this folder.
//!
//! Never:
//! - keep a frame or a buffer once its capture is done;
//! - log a path, a geometry or an output name.
//!
//! See: docs/features/captures.md (when it lands), docs/decisions.md.

pub mod encode;
pub mod screencopy;

/// A rectangle of whole pixels, `x` and `y` of its top-left corner. A logical area when it comes
/// from the selector, a buffer area after `screencopy::map_area`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}
