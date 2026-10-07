//! alpheus — TUI control center (sub-project 1, see docs/CADRAGE.md).
//! `exec`, `config`, `backends` and `displays` live in alpheus-commun;
//! re-exported here so that `crate::displays::…` and `alpheus::backends::…`
//! stay valid.

pub use alpheus_commun::{backends, config, displays, exec};

pub mod app;
pub mod appearance;
pub mod external_tools;
pub mod fuzzy;
pub mod theme;
pub mod ui;
