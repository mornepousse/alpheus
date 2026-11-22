//! alpheus-commun — shared foundation for the TUI (`alpheus`) and the
//! resident daemon (`alpheus-demon`): injectable executor, configuration,
//! theme, backends, displays. No display dependency (neither ratatui nor
//! Wayland): checked by tests/boundaries.rs.

pub mod backends;
pub mod compositor;
pub mod config;
pub mod diagnostics;
pub mod displays;
pub mod exec;
pub mod launcher_ipc;
pub mod local_time;
pub mod paths;
pub mod shutdown;
#[cfg(any(test, feature = "testing"))]
pub mod temp_path;
pub mod text;
pub mod theme;
