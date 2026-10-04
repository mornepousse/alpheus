//! alpheus — centre de contrôle TUI (sous-projet 1, voir docs/CADRAGE.md).
//! `exec`, `config`, `backends` et `displays` vivent dans alpheus-commun ;
//! ré-exportés ici pour que `crate::displays::…` et `alpheus::backends::…`
//! restent valables.

pub use alpheus_commun::{backends, config, displays, exec};

pub mod app;
pub mod externes;
pub mod fuzzy;
pub mod theme;
pub mod ui;
