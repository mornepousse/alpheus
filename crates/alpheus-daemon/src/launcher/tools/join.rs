//! The hook points of the Tools menu for the clipboard, the shield and the diagnostics tab: the only
//! file that names them.
//!
//! Each of the three is a feature of its own, built in parallel with the menu and merged into it. The
//! menu's other files do not know them; this one gives, for each, what the menu needs: a function that
//! says whether its row shows, and one that opens it (the diagnostics tab, which is a page of the
//! control center, is a constant instead).
//!
//! Never:
//! - hold any state or start anything: each function asks the app or hands it a request;
//! - let another file of the menu name these three (a guardrail,
//!   `the_parallel_branches_are_named_only_in_tools_join`, reads their production code).
//!
//! See: docs/specs/2026-11-21-alpheus-tools-menu-design.md (§ 8), docs/features/tools-menu.md, and
//! docs/decisions.md ("The Tools menu reaches the clipboard, the shield and the diagnostics tab
//! through one file").

use crate::wayland::App;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::Instant;

/// Whether the clipboard history can be opened: `[clipboard] enabled` and a data-control
/// protocol bound.
pub fn clipboard_available(app: &App) -> bool {
    app.clipboard_availability().is_ok()
}

/// Opens the clipboard history through the launcher's own opener (it replaces the Tools box).
pub fn open_clipboard(app: &mut App, qh: &QueueHandle<App>) {
    if clipboard_available(app) {
        app.launcher_open_clipboard(qh, Instant::now());
    }
}

/// Whether the shield's detail can be opened: `[shield]` is configured and the `quick` module is
/// on (the panel holds the detail).
pub fn shield_available(app: &App) -> bool {
    app.shield.is_some() && app.quick_enabled()
}

/// Opens the quick panel on the shield's detail (a no-op when `shield_available` is false).
pub fn open_shield(app: &mut App) {
    app.shield_open_detail();
}

/// The control center tab of the diagnostics page, if this build has one: the slug `alpheus --tab`
/// accepts. `None` would hide the row.
pub const DIAGNOSTICS_TAB: Option<&str> = Some("diagnostics");
