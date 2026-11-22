//! The bar's side of the tray: what the bar does with what the tray reports, and what it asks of it.
//!
//! `tray_output` applies a callback's `TrayOutput` (a redraw, the bus closing, a menu asked for or
//! closed). `tray_sync` runs after every callback: it queues the tray's deferred answers, keeps its
//! deadline timer and asks for one frame callback. `tray_gesture_at` sends a click or a wheel step on
//! an icon to its item. `mpris_do`, `notify_do`, `mpris_of` and `notifications_of` reach the media
//! and notification consumers, which live on the tray's connection or, without the tray, on the
//! bar's own `SessionLink`.
//!
//! Never:
//! - start a thread, a process or a command: everything here runs on the bar's event loop;
//! - talk to D-Bus directly: every call goes through `Tray` or `SessionLink`;
//! - route an answer from inside the call that sent it: the answers a flush drained are routed from
//!   an idle callback (`tray_sync` queues it), so no callback runs inside another.
//!
//! See: docs/specs/2026-10-05-alpheus-tray-design.md, docs/features/tray.md, and docs/decisions.md
//! ("A burst of tray changes is redrawn once per frame callback", "A closed session bus is never
//! reconnected", "A failed Activate opens the menu of the icon that was pressed").

use crate::actions::Button;
use crate::grid::Target;
use crate::link::SessionLink;
use crate::log::write as log;
use crate::painter;
use crate::popup::{Anchor as PopupAnchor, PopupBody, PopupClose};
use crate::session_bus::bus::SessionBus;
use crate::session_bus::mpris::Mpris;
use crate::session_bus::notifications::Notifications;
use crate::session_bus::router::{Tray, TrayOutput};
use crate::session_bus::{Gesture, ItemKey, icons};
use crate::state::Msg;
use crate::wayland::{App, eligible_for_frame};

use smithay_client_toolkit::shell::WaylandSurface;
use std::time::Instant;

impl App {
    /// Applies what a tray callback reported: the portal's colour scheme; the bus closing (logged
    /// once, the tray dropped and left empty); the media, notification, failed-units and bubble
    /// views the tray carries for the other modules; a redraw (the icons rebuilt from the tray's
    /// items and `[tray.icons]`); a menu closed, asked for, or changed. It ends by refreshing the
    /// media popup and the notification panel, and by `tray_sync` (the frame request and the
    /// deadline timer).
    pub(crate) fn tray_output(&mut self, out: TrayOutput) {
        if let Some(mode) = out.appearance {
            self.portal_said(mode);
        }
        if out.bus_closed {
            // A closed bus is never reconnected: the tray stays empty. See docs/decisions.md,
            // "A closed session bus is never reconnected". The other consumers on this connection
            // say that they went with it.
            log("tray: the session bus closed; the tray stays empty");
            if self.tray.as_ref().is_some_and(|t| t.media().is_some()) {
                log("media: the session bus closed");
            }
            if self.tray.as_ref().is_some_and(|t| t.notifications().is_some()) {
                log("notifications: the session bus closed");
            }
            if self.tray.as_ref().is_some_and(|t| t.failed().is_some()) {
                log("failed: the session bus closed");
            }
            self.tray = None;
        }
        if let Some(view) = out.media {
            self.media_view(view);
        }
        if let Some(view) = out.notifications {
            self.notify_view(view);
        }
        if let Some(count) = out.user_units {
            self.failed_user(count);
        }
        if out.bubbles {
            self.bubbles_sync();
        }
        if out.redraw || out.bus_closed {
            // An item leaving the tray closes its tooltip at once, whatever else changed, so this
            // comes first and does not depend on the grid built below.
            self.tooltip_item_gone();
            let shown = match &self.tray {
                Some(t) => icons::icons(t.items(), &self.bar.config.tray.icons),
                None => Vec::new(),
            };
            if shown.len() != self.bar.tray.len() {
                log(&format!("tray: icons shown: {}", shown.len()));
            }
            // `out.redraw` is set when any property of any item changes (a messenger's unread
            // counter, an icon that animates faster than twice a second), and most of those leave
            // the drawn grid as it was. Only a real change of the grid (`apply` returns true) can
            // have moved or dropped the zone of the icon the pointer rests on, so the hover is
            // cancelled only then, and only when `hover_target_moved` confirms that this very zone
            // is the one that changed.
            let changed = self.bar.apply(Msg::Tray(shown));
            if changed {
                self.dirty = true;
                if self.hover_target_moved() {
                    let c = self.hover.cancel();
                    self.hover_apply(c);
                }
            }
        }
        if out.bus_closed && self.menu.target.is_some() {
            // The menu's session went with the bus (the tray is already dropped): a menu on screen
            // closes, and a request still waiting for its layout ends.
            if matches!(self.popup_body(), Some(PopupBody::Menu(_))) {
                self.popup_close(PopupClose::ItemGone);
            }
            self.menu_end();
        }
        // A menu closed comes before a menu opened. Both can arrive in one batch (one message ends
        // the menu of an item, another opens the menu of a different item), and handling the open
        // first would let this close, meant for the old request, end the new one that the open
        // just put in its place (commit 40be9a6).
        if out.close_menu && self.menu.target.is_some() {
            // The item is gone when the tray no longer has it (or no tray is left).
            let gone = match &self.menu.target {
                Some((key, _)) => match &self.tray {
                    Some(tray) => tray.item(key).is_none(),
                    None => true,
                },
                None => false,
            };
            let why = if gone {
                PopupClose::ItemGone
            } else if out.menu_path_changed {
                PopupClose::MenuMoved
            } else {
                PopupClose::Timeout
            };
            if matches!(self.popup_body(), Some(PopupBody::Menu(_))) {
                self.popup_close(why);
            } else {
                // The menu was never shown (no layout came in time): only the request is left to end.
                self.menu_end();
                log(&format!("tray: menu not shown ({})", why.text()));
            }
        }
        if let Some(key) = out.open_menu {
            // The remembered press is used up by the answer that opens its menu. A second, late
            // answer, or one that arrives after Escape closed the menu, finds nothing and opens
            // nothing: it must not reopen a menu from a press that was already used.
            match take_icon_for(&mut self.menu.last_icon, &key) {
                Some((key, anchor, button)) => self.menu_requested(key, anchor, button),
                None => log("tray: a menu was asked for an icon no longer under the pointer"),
            }
        }
        if out.menu_changed {
            self.menu_layout_changed();
        }
        self.media_popup_refresh();
        self.panel_refresh();
        self.tray_sync();
    }

    /// Runs `f` on the media consumer with its connection's outbox, wherever the consumer lives.
    /// This is the one path for every media call: a click's control, and the popup's `Seeked`
    /// rule, position reads and pin. With no media module, nothing happens.
    ///
    /// Nothing is routed from inside this call. What the flush drained is kept: by the tray, for the
    /// idle callback that `tray_sync` queues; by the link, for an idle `link_readable`.
    pub(crate) fn mpris_do(&mut self, f: impl FnOnce(&mut Mpris, &mut SessionBus)) {
        if let Some(tray) = self.tray.as_mut() {
            if tray.media().is_some() {
                tray.media_do(f);
                self.tray_sync();
                return;
            }
        }
        let Some(link) = self.link.session.as_mut() else { return };
        if link.media().is_none() {
            return;
        }
        link.media_do(f);
        // What the flush drained is routed from an idle callback (`link_sync`).
        self.link_sync();
    }

    /// Runs `f` on the notification server, wherever it lives. This is the one path for every
    /// change the bar makes to it: the bubbles' timer and clicks, the panel, the verbs and
    /// do-not-disturb. The server's signals leave at once; its view and bubbles come back through
    /// an idle callback (the tray's deferred output, or the link's `has_ready`, each queued by its
    /// own sync function), never from inside this call. With no server, nothing happens.
    pub(crate) fn notify_do(&mut self, f: impl FnOnce(&mut Notifications)) {
        if let Some(tray) = self.tray.as_mut() {
            if tray.notifications().is_some() {
                tray.notifications_do(f);
                // `tray_sync` queues the idle callback that delivers the deferred view and bubbles
                // now; without it they would wait for the next event on the tray's connection.
                self.tray_sync();
                return;
            }
        }
        let Some(link) = self.link.session.as_mut() else { return };
        if link.notifications().is_none() {
            return;
        }
        link.notifications_do(f);
        self.link_sync();
    }

    /// Runs after every tray callback. It queues an idle callback for the answers the tray kept
    /// (`has_deferred`) and for the answers left waiting with no fd event to announce them
    /// (`has_ready`: a burst past the settle rounds); it syncs the deadline timer with the pending
    /// calls (none pending, no timer); and, when properties are dirty, it asks for one frame
    /// callback on the first bar that can take one, where they are read again once.
    pub(crate) fn tray_sync(&mut self) {
        if !self.tray_deferred_queued && self.tray.as_ref().is_some_and(Tray::has_deferred) {
            if let Some(handle) = &self.loop_handle {
                self.tray_deferred_queued = true;
                handle.insert_idle(|app| {
                    app.tray_deferred_queued = false;
                    let Some(tray) = app.tray.as_mut() else { return };
                    let out = tray.take_deferred();
                    app.tray_output(out);
                });
            }
        }
        // The answers left in `ready` after a burst past the settle rounds: the socket was read
        // already, so no fd event will come for them, and an idle callback reads them instead.
        if !self.tray_ready_queued
            && self.tray.as_ref().is_some_and(Tray::has_ready)
            && let Some(handle) = &self.loop_handle
        {
            self.tray_ready_queued = true;
            handle.insert_idle(|app| {
                app.tray_ready_queued = false;
                app.tray_readable();
            });
        }
        let deadline = match &self.tray {
            Some(tray) => tray.next_deadline(),
            None => None,
        };
        self.tray_timer.sync_opt(self.loop_handle.as_ref(), deadline, App::tray_deadline);
        if self.tray_frame_pending.is_some() || !self.tray.as_ref().is_some_and(Tray::wants_frame) {
            return;
        }
        let Some(bar) = self.surfaces.iter().find(|s| eligible_for_frame(s.configured, !s.buffers.is_empty(), s.placing)) else {
            return;
        };
        let surface = bar.layer.wl_surface().clone();
        surface.frame(&self.qh, surface.clone());
        // One pixel of buffer damage, on every compositor: a commit with no damage may get no
        // frame callback at all (Hyprland) or a throttled one (niri, about 1 s). The buffer already
        // attached is not sent again. See docs/decisions.md, "A burst of tray changes is redrawn
        // once per frame callback".
        surface.damage_buffer(0, 0, 1, 1);
        surface.commit();
        self.tray_frame_pending = Some(surface);
    }

    /// The session bus's fd is readable (calloop callback).
    pub(crate) fn tray_readable(&mut self) {
        let Some(tray) = self.tray.as_mut() else { return };
        let out = tray.on_readable();
        self.tray_output(out);
    }

    /// The deadline timer fired (calloop callback; the timer's source is dropped by the loop).
    pub(crate) fn tray_deadline(app: &mut App, now: Instant) {
        app.tray_timer.fired();
        let Some(tray) = app.tray.as_mut() else { return };
        let out = tray.on_deadline(now);
        app.tray_output(out);
    }

    /// The tray icon under `x` on bar `i`, if any.
    pub(crate) fn tray_key_at(&mut self, i: usize, x: f64) -> Option<ItemKey> {
        let e = self.surfaces[i].scale;
        let col = painter::column_under(x, e, self.fonts.advance(e));
        // Each `?` ends the function with None when there is no grid yet or no zone at that column.
        match self.surfaces[i].last.as_ref()?.target_at(col)? {
            Target::TrayItem(key) => Some(key.clone()),
            _ => None,
        }
    }

    /// A gesture at `position` (surface-local, logical pixels) on bar `i`, from button `b`: sent to
    /// the tray icon there, with that position. False when no icon is there, so that the bar's own
    /// click handling applies.
    pub(crate) fn tray_gesture_at(&mut self, i: usize, position: (f64, f64), g: Gesture, b: Button) -> bool {
        let Some(key) = self.tray_key_at(i, position.0) else { return false };
        // Remember the icon's cells (where its menu will hang) and the button that pressed it.
        // `menu_requested` reads them back once the menu opens, which can be later: a failed
        // Activate answers asynchronously. See docs/decisions.md, "A failed Activate opens the
        // menu of the icon that was pressed".
        self.menu.last_icon = self.icon_anchor(i, position.0).map(|(k, a)| (k, a, b));
        if let Some(tray) = self.tray.as_mut() {
            let out = tray.gesture(&key, g, (position.0 as i32, position.1 as i32));
            self.tray_output(out);
        }
        true
    }

    /// The tray icon under `x` on bar `i`, and the cells it occupies.
    fn icon_anchor(&mut self, i: usize, x: f64) -> Option<(ItemKey, PopupAnchor)> {
        let e = self.surfaces[i].scale;
        let column = painter::column_under(x, e, self.fonts.advance(e));
        let s = &self.surfaces[i];
        // Each `?` ends the function with None when there is no grid yet or no zone at that column.
        let zone = s.last.as_ref()?.zone_at(column)?;
        match &zone.target {
            Target::TrayItem(key) => Some((key.clone(), PopupAnchor { output: s.name.clone(), start: zone.start, end: zone.end })),
            _ => None,
        }
    }
}

/// The media consumer wherever it lives: on the tray's connection, else on the bar's own link.
pub(crate) fn mpris_of<'a>(tray: Option<&'a Tray>, link: Option<&'a SessionLink>) -> Option<&'a Mpris> {
    if let Some(tray) = tray {
        if let Some(media) = tray.media() {
            return Some(media);
        }
    }
    match link {
        Some(link) => link.media(),
        None => None,
    }
}

/// The notification consumer wherever it lives: on the tray's connection, else on the bar's own
/// link.
pub(crate) fn notifications_of<'a>(tray: Option<&'a Tray>, link: Option<&'a SessionLink>) -> Option<&'a Notifications> {
    if let Some(tray) = tray {
        if let Some(server) = tray.notifications() {
            return Some(server);
        }
    }
    match link {
        Some(link) => link.notifications(),
        None => None,
    }
}

/// The remembered press (`menu.last_icon`) if it is `key`'s, taken out of `last`. Whatever its key,
/// the remembered press is spent by this call: a press for a different icon is stale too, since the
/// answer that asked for `key` found it replaced.
fn take_icon_for(last: &mut Option<(ItemKey, PopupAnchor, Button)>, key: &ItemKey) -> Option<(ItemKey, PopupAnchor, Button)> {
    match last.take() {
        Some(press) if press.0 == *key => Some(press),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
