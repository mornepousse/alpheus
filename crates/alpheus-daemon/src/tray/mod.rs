//! The status-notifier tray's wiring on the bar.
//!
//! The tray itself is in `session_bus/`: the connection to the session bus, the watcher and host of
//! status-notifier items (the icons of running applications) and the menu sessions, all owned by
//! `session_bus/router.rs` as `Tray`. This folder holds the two files that connect it to the bar.
//! `bridge.rs` applies what the tray reports (a redraw, a menu asked for, the bus closing), keeps
//! the tray's deadline timer and frame request, sends a click or a wheel step on an icon to its
//! item, and finds the media and notification consumers, which live on the tray's connection or on
//! the bar's own session link. `menu_app.rs` opens a tray menu in a popup and carries out what the
//! user does in it.
//!
//! Never:
//! - start a thread, a process or a command: both files run on the bar's event loop, and the
//!   applications act on their own (the bar only asks them);
//! - talk to D-Bus directly: every call to an item goes through `Tray`.
//!
//! See: docs/specs/2026-10-05-alpheus-tray-design.md, docs/features/tray.md, and docs/decisions.md
//! ("A tray menu is shown only once its layout has arrived", "A failed Activate opens the menu of
//! the icon that was pressed", "A closed session bus is never reconnected").

pub mod bridge;
pub mod menu_app;
