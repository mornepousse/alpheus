//! The bar's side of the tray: applying a callback's `TrayOutput`, the deferred idles and the
//! deadline timer (`tray_sync`), gestures on an icon, and the consumers (media, notifications) that
//! live on the tray's connection or on the bar's own `SessionLink`.

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
    /// Applies a tray callback's output: redraw (Msg::Tray rebuilt from the
    /// tray's items and `[tray.icons]`), bus closed (logged once, the tray
    /// dropped and emptied, never retried), a menu asked (`open_menu`: opened
    /// from `menu.last_icon`), the layout changed; then the frame request and the deadline timer.
    pub(crate) fn tray_output(&mut self, out: TrayOutput) {
        if let Some(mode) = out.appearance {
            self.portal_said(mode);
        }
        if out.bus_closed {
            log("tray: the session bus closed; the tray stays empty");
            if self.tray.as_ref().is_some_and(|t| t.media().is_some()) {
                log("media: the session bus closed");
            }
            if self.tray.as_ref().is_some_and(|t| t.notifications().is_some()) {
                log("notifications: the session bus closed");
            }
            self.tray = None;
        }
        if let Some(view) = out.media {
            self.media_view(view);
        }
        if let Some(view) = out.notifications {
            self.notify_view(view);
        }
        if out.bubbles {
            self.bubbles_sync();
        }
        if out.redraw || out.bus_closed {
            // The hovered item leaving the tray closes its tooltip outright (ItemGone), whatever
            // else changed; checked first, independent of the grid below.
            self.tooltip_item_gone();
            let shown = match &self.tray {
                Some(t) => icons::icons(t.items(), &self.bar.config.tray.icons),
                None => Vec::new(),
            };
            if shown.len() != self.bar.tray.len() {
                log(&format!("tray: icons shown: {}", shown.len()));
            }
            // `out.redraw` fires for ANY property of ANY item
            // changing (a messenger's unread counter, an icon animating faster than ½ s) — most of
            // which leave the drawn grid untouched. Only an actual display change (`bar.apply`
            // returns true) can possibly have moved or dropped the hovered icon's own zone, so the
            // hover (armed, or already shown as a tooltip) is cancelled then, and only if
            // `hover_target_moved` confirms THAT zone is the one that changed — an unrelated icon
            // changing elsewhere leaves it alone.
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
            // The session went with the bus (the tray is already dropped): a menu shown closes, a
            // request still waiting dies.
            if matches!(self.popup_body(), Some(PopupBody::Menu(_))) {
                self.popup_close(PopupClose::ItemGone);
            }
            self.menu_end();
        }
        // Close_menu before open_menu — both can land in the
        // SAME batch (one incoming message closes the menu stale under it, another opens a new
        // one for a different item), and processing open_menu first would let this close_menu,
        // meant for the OLD request, wrongly end the NEW one `open_menu` just replaced it with.
        if out.close_menu && self.menu.target.is_some() {
            let gone = self.menu.target.as_ref().is_some_and(|(k, _)| self.tray.as_ref().is_none_or(|t| t.item(k).is_none()));
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
                // Never shown (no layout within 2 s, question 8): nothing to close but the request.
                self.menu_end();
                log(&format!("tray: menu not shown ({})", why.text()));
            }
        }
        if let Some(key) = out.open_menu {
            // Consumed: a late second answer, or one arriving after Escape closed
            // the menu, must not open it again from a press that was already used.
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

    /// Runs `f` on the media consumer with its connection's outbox, wherever it lives: the one path
    /// for every media call (a click's control here; the popup's Seeked rule, position reads and pin).
    /// Nothing is routed re-entrantly: the tray keeps what the flush drained for its
    /// deferred idle (`tray_sync` queues it), the link for an idle `link_readable`. With no module, it
    /// is nothing.
    pub(crate) fn mpris_do(&mut self, f: impl FnOnce(&mut Mpris, &mut SessionBus)) {
        if let Some(tray) = self.tray.as_mut().filter(|t| t.media().is_some()) {
            tray.media_do(f);
            self.tray_sync();
            return;
        }
        let Some(link) = self.link.session.as_mut().filter(|l| l.media().is_some()) else { return };
        link.media_do(f);
        // What the flush drained is routed from an idle (`link_sync`).
        self.link_sync();
    }

    /// Runs `f` on the notifications server wherever it lives, the one path for every change the bar
    /// makes (the bubbles' timer and clicks, the panel, the verbs, do-not-disturb). Its signals leave
    /// at once; its view and bubbles come back through an idle callback (the tray's deferred output,
    /// the link's `has_ready`, both queued by their `*_sync`), never re-entrantly. With no server,
    /// nothing.
    pub(crate) fn notify_do(&mut self, f: impl FnOnce(&mut Notifications)) {
        if let Some(tray) = self.tray.as_mut().filter(|t| t.notifications().is_some()) {
            tray.notifications_do(f);
            // The one prompt delivery of the tray's deferred view and bubbles (the idle `tray_sync`
            // queues): without it they would wait for the next event on the tray's connection.
            self.tray_sync();
            return;
        }
        let Some(link) = self.link.session.as_mut().filter(|l| l.notifications().is_some()) else { return };
        link.notifications_do(f);
        self.link_sync();
    }

    /// After any tray callback: the deadline timer follows the pending calls
    /// (none: no timer), and dirty properties ask for one frame callback on
    /// the first configured bar (a commit with no new buffer and one pixel
    /// of damage), where they are re-read once.
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
        // Answers left in `ready` (a burst past the settle rounds): no fd event will come for them.
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
        let deadline = self.tray.as_ref().and_then(Tray::next_deadline);
        self.tray_timer.sync_opt(self.loop_handle.as_ref(), deadline, App::tray_deadline);
        if self.tray_frame_pending.is_some() || !self.tray.as_ref().is_some_and(Tray::wants_frame) {
            return;
        }
        let Some(surface) = self
            .surfaces
            .iter()
            .find(|s| eligible_for_frame(s.configured, !s.buffers.is_empty(), s.placing))
            .map(|s| s.layer.wl_surface().clone())
        else {
            return;
        };
        surface.frame(&self.qh, surface.clone());
        // 1×1 of buffer damage on every compositor:
        // an undamaged commit may get no frame callback at all (Hyprland) or a
        // throttled one (niri, ≈ 1 s). The attached buffer is not re-sent.
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

    /// The deadline timer fired (calloop callback; its source is dropped).
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
        match self.surfaces[i].last.as_ref()?.target_at(col)? {
            Target::TrayItem(key) => Some(key.clone()),
            _ => None,
        }
    }

    /// A gesture at `position` (surface-local, logical) on bar `i`, from button `b`: sent to the
    /// tray icon there, with that position; false when no icon is there (the bar's
    /// own click handling then applies).
    pub(crate) fn tray_gesture_at(&mut self, i: usize, position: (f64, f64), g: Gesture, b: Button) -> bool {
        let Some(key) = self.tray_key_at(i, position.0) else { return false };
        // The icon's cells, where its menu will hang, and the button that pressed it
        // — read back by `menu_requested` once the menu
        // actually opens, which may be later (a failed Activate's error answers asynchronously).
        self.menu.last_icon = self.icon_anchor(i, position.0).map(|(k, a)| (k, a, b));
        if let Some(tray) = self.tray.as_mut() {
            let out = tray.gesture(&key, g, (position.0 as i32, position.1 as i32));
            self.tray_output(out);
        }
        true
    }

    /// The tray icon under `x` on bar `i`, and its cells.
    fn icon_anchor(&mut self, i: usize, x: f64) -> Option<(ItemKey, PopupAnchor)> {
        let e = self.surfaces[i].scale;
        let column = painter::column_under(x, e, self.fonts.advance(e));
        let s = &self.surfaces[i];
        let zone = s.last.as_ref()?.zone_at(column)?;
        match &zone.target {
            Target::TrayItem(key) => Some((key.clone(), PopupAnchor { output: s.name.clone(), start: zone.start, end: zone.end })),
            _ => None,
        }
    }
}

/// The media consumer wherever it lives: on the tray's connection, else on the bar's link.
pub(crate) fn mpris_of<'a>(tray: Option<&'a Tray>, link: Option<&'a SessionLink>) -> Option<&'a Mpris> {
    tray.and_then(Tray::media).or_else(|| link.and_then(SessionLink::media))
}

/// The notifications consumer wherever it lives: on the tray's connection, else on the
/// bar's link.
pub(crate) fn notifications_of<'a>(tray: Option<&'a Tray>, link: Option<&'a SessionLink>) -> Option<&'a Notifications> {
    tray.and_then(Tray::notifications).or_else(|| link.and_then(SessionLink::notifications))
}

/// The remembered press (`menu.last_icon`) for `key`, taken out: whatever its key, it is spent after this
/// (a different icon's press is stale too — the answer that asked for `key` found it replaced).
fn take_icon_for(last: &mut Option<(ItemKey, PopupAnchor, Button)>, key: &ItemKey) -> Option<(ItemKey, PopupAnchor, Button)> {
    last.take().filter(|(k, _, _)| k == key)
}

#[cfg(test)]
mod tests;
