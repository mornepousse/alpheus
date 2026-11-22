//! The hook points for the parallel deliveries (tools spec § 8): the only file that names the
//! clipboard, the shield and the diagnostics tab for the Tools menu. One function each for
//! "shown?" and "open it"; the diagnostics tab is a constant.

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
/// accepts.
pub const DIAGNOSTICS_TAB: Option<&str> = Some("diagnostics");
