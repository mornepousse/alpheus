//! alpheus — TUI control center (sub-project 1, see docs/SCOPE.md).
//! `exec`, `config`, `backends`, `diagnostics` and `displays` live in alpheus-commun;
//! re-exported here so that `crate::displays::…` and `alpheus::backends::…`
//! stay valid.

pub use alpheus_commun::{backends, config, diagnostics, displays, exec};

pub mod app;
pub mod external_tools;
pub mod fuzzy;
pub mod theme;
pub mod theme_file;
pub mod ui;
