//! The `alpheus` crate: the terminal control center (sub-project 1, see docs/SCOPE.md).
//!
//! `app` is the state and logic, `ui` the drawing, `theme` and `theme_file` the colours and the one
//! file the Theme tab writes, `external_tools` the hand-over of the terminal to a tool such as
//! wiremix, and `fuzzy` the search used by the palette and the lists. `exec`, `config`, `backends`,
//! `diagnostics` and `displays` live in alpheus-common and are re-exported here, so that
//! `crate::displays::...` and `alpheus::backends::...` stay valid.
//!
//! Never:
//! - launch a daemon or leave a process behind on exit (the spec's § 6, "Guardrails").
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md, docs/features/control-center.md.

pub use alpheus_common::{backends, config, diagnostics, displays, exec};

pub mod app;
pub mod external_tools;
pub mod fuzzy;
pub mod theme;
pub mod theme_file;
pub mod ui;
