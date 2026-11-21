//! The hook points for the parallel deliveries (tools spec § 8): the only file that names the
//! clipboard, the shield and the diagnostics tab for the Tools menu. One function each for
//! "shown?" and "open it". The clipboard's is real; shield and diagnostics answer "not shown"
//! until their branches merge.

use crate::log::write as log;
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

/// Whether the shield's detail can be opened. Stub: once `feat/shield` merges this becomes its
/// availability check (`[shield]` configured).
pub fn shield_available(_: &App) -> bool {
    false
}

/// Opens the shield's detail. Stub, never reached while `shield_available` is false: once
/// `feat/shield` merges this becomes `app.shield_open_detail()`.
pub fn open_shield(_: &mut App) {
    log("tools: the shield is not built in");
}

/// The control center tab of the diagnostics page, if this build has one. `None` until
/// `feat/diagnostics` merges, then `Some("diagnostics")`.
pub const DIAGNOSTICS_TAB: Option<&str> = None;
