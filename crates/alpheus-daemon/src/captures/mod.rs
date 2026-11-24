//! Native screenshots: the area selector, and the pieces the capture flow is built from.
//!
//! This file only declares the feature's modules and the one type they share, `Rect`. The
//! selector's model is `select.rs`; its layer surface is `select_app.rs`.
//!
//! Never:
//! - hold anything at rest: the selector's surface and buffers live only while a selection is open;
//! - put a geometry in a log line.
//!
//! See: docs/features/captures.md.

pub mod select;
pub mod select_app;

/// A rectangle of an output in logical pixels, origin at the output's top left. `x` and `y` are
/// never negative when it comes from the selector, and `width` and `height` are never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
