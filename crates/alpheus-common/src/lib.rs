//! alpheus-common — shared foundation for the TUI (`alpheus`) and the
//! resident daemon (`alpheus-demon`): injectable executor, configuration,
//! theme, backends, displays. No display dependency (neither ratatui nor
//! Wayland): checked by tests/boundaries.rs.
//!
//! Never:
//! - depend on a display library: neither ratatui nor Wayland may appear in this crate's tree
//!   (`the_foundation_carries_no_display_dependency`, `the_tui_does_not_carry_wayland`,
//!   `the_daemon_does_not_carry_ratatui`, `dbus_stays_in_the_daemon`, in tests/boundaries.rs);
//! - run a tool directly: everything goes through the injectable executor, which the tests replace
//!   with a fake.
//!
//! See: docs/ARCHITECTURE.md ("Crates", "The foundation and the client"), AGENTS.md ("The code").

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
