//! alpheus-commun — shared foundation for the TUI (`alpheus`) and the
//! resident daemon (`alpheus-demon`): injectable executor, configuration,
//! theme, backends, displays. No display dependency (neither ratatui nor
//! Wayland): checked by tests/boundaries.rs.

pub mod backends;
pub mod config;
pub mod displays;
pub mod exec;
pub mod launcher_ipc;
pub mod paths;
pub mod shutdown;
pub mod theme;
