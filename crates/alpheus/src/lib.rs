//! The `alpheus` crate: the terminal control center (sub-project 1, see docs/SCOPE.md).
//!
//! `app` is the state and logic, `ui` the drawing, `theme` and `theme_file` the colours and the one
//! file the Theme tab writes, `external_tools` the hand-over of the terminal to a tool such as
//! wiremix, and `fuzzy` the search used by the palette and the lists. `exec`, `config`, `backends`,
//! `diagnostics` and `displays` live in alpheus-common and are re-exported here, so that
//! `crate::displays::...` and `alpheus::backends::...` stay valid.
//!
//! Never:
//! - launch a daemon or leave a process behind on exit (docs/decisions.md "The control center runs a fixed list of commands, and systemctl only to read or to restart a unit").
//!
//! See: docs/features/control-center.md,
//! docs/decisions.md ("The control center runs a fixed list of commands, and systemctl only to read or to restart a unit").

pub use alpheus_common::{backends, config, diagnostics, displays, exec};

pub mod app;
pub mod external_tools;
pub mod fuzzy;
pub mod theme;
pub mod theme_file;
pub mod ui;
