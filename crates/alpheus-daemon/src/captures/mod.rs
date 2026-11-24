//! Captures: the daemon takes screenshots natively.
//!
//! This folder holds the capture pipeline. `screencopy.rs` is the only file that speaks the
//! screencopy protocol and turns one output into a `Frame`; `encode.rs` writes a PNG; `select.rs`
//! and `select_app.rs` are the area selector (a pure model and its layer surface). The pure flow
//! and the glue come in later files of this folder.
//!
//! Never:
//! - hold anything at rest: a frame, a buffer or the selector's surface lives only while its
//!   capture is open;
//! - log a path, a geometry or an output name.
//!
//! See: docs/features/captures.md (when it lands), docs/decisions.md.

pub mod encode;
pub mod screencopy;
pub mod select;
pub mod select_app;

/// A rectangle of whole pixels, `x` and `y` of its top-left corner. A logical area (origin at the
/// output's top left, never negative, never empty) when it comes from the selector, a buffer area
/// after `screencopy::map_area`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
