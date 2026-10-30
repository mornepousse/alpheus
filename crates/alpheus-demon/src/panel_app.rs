//! The notification panel's wiring: a left press on the bell toggles it, its actions (the card's
//! controls, invoking, removing, do-not-disturb, clearing) and its refresh after each change.

use crate::log::write as log;
use crate::media::Players;
use crate::media_popup::MediaAction;
use crate::notify::Center;
use crate::panel::{self, Panel, PanelAction};
use crate::popup::{Anchor as PopupAnchor, Input, PopupBody, PopupClose};
use crate::tray::mpris::Mpris;
use crate::tray_bridge::{mpris_of, notifications_of};
use crate::wayland::App;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::Instant;

impl App {
    /// Left press on the bell: toggles the panel under it (a second click closes it). Opening marks
    /// everything read (spec § 4); the card's Seeked rule and tick start as the media popup's do.
    pub(crate) fn panel_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if self.panel_shown() {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        let Some(i) = self.surfaces.iter().position(|s| s.name == anchor.output) else { return };
        let room = self.popup_room(i).1.saturating_sub(1);
        let now = Instant::now();
        let built = {
            let App { tray, link, .. } = &*self;
            let players = mpris_of(tray.as_ref(), link.as_ref()).map(Mpris::players);
            notifications_of(tray.as_ref(), link.as_ref()).map(|n| {
                let mut p = Panel::new(n.center(), players, room);
                panel::scroll_into_view(&mut p, n.center(), players);
                let rows = panel::rows(n.center(), players, &p, now, crate::tray::notifications::minute_now());
                let card = panel::items(n.center(), players).first() == Some(&panel::Item::Card);
                (p, rows, card)
            })
        };
        // No consumer (no session bus): nothing to show.
        let Some((p, rows, card)) = built else { return };
        let entries = p.entries;
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Panel(p), columns, height, Input::Grab) {
            self.popup_paint(rows);
            // Only once it shows: a refused open leaves the count on the bell.
            self.notify_do(|n| n.center_mut().mark_read());
            self.mpris_do(|m, out| m.popup_opened(out));
            self.media_timer_sync();
            log(&format!("panel shown (entries: {entries}, card: {})", if card { "yes" } else { "no" }));
        }
    }

    pub(crate) fn panel_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Panel(_)))
    }

    /// `f` on the open panel, the notifications center and the players; Nothing when the panel or the
    /// consumer is missing.
    pub(crate) fn panel_with(&mut self, f: impl FnOnce(&mut Panel, &Center, Option<&Players>) -> PanelAction) -> PanelAction {
        let App { tray, link, popup, .. } = self;
        let players = mpris_of(tray.as_ref(), link.as_ref()).map(Mpris::players);
        match (notifications_of(tray.as_ref(), link.as_ref()), popup.as_mut().map(|p| &mut p.body)) {
            (Some(n), Some(PopupBody::Panel(p))) => f(p, n.center(), players),
            _ => PanelAction::Nothing,
        }
    }

    /// A panel action: the card's go as the media popup's; the notifications' through `notify_act`
    /// (signals sent, the bell and the bubbles follow, the panel repainted).
    pub(crate) fn panel_action(&mut self, action: PanelAction) {
        match action {
            PanelAction::Nothing => {}
            PanelAction::Redraw => self.panel_refresh(),
            PanelAction::Close => self.popup_close(PopupClose::Escape),
            PanelAction::Media(m) => match m {
                MediaAction::Nothing => {}
                MediaAction::Redraw => self.panel_refresh(),
                MediaAction::Close => self.popup_close(PopupClose::Escape),
                MediaAction::Control(c) => {
                    self.mpris_do(|m, out| m.control(out, c));
                    self.panel_refresh();
                }
                MediaAction::Pin(name) => {
                    self.media_pin(&name);
                    self.panel_refresh();
                }
            },
            PanelAction::Invoke(id, revision, _) => self.notify_activate(id, revision),
            PanelAction::Remove(id) => {
                self.notify_act(|n| n.center_mut().remove(id));
                log("panel: entry removed");
            }
            // The one path: persisted, given to the server, then (notify_act) the bubbles and
            // this panel follow.
            PanelAction::ToggleDnd => {
                let on = !self.dnd;
                self.set_dnd(on);
                log(&format!("panel: do not disturb {}", if on { "on" } else { "off" }));
            }
            PanelAction::Clear => {
                self.clear_notifications();
                log("panel: cleared");
            }
        }
    }

    /// After anything that may change what the panel shows (a host's output, a tick, a key, a
    /// notification action): whatever arrived is marked read while it shows (spec § 4), the window
    /// follows the selection, the rows are rebuilt and painted (`popup_draw` commits nothing when they
    /// did not change), and the card's tick re-synced. The consumer gone (its bus closed): the panel
    /// closes.
    pub(crate) fn panel_refresh(&mut self) {
        if !self.panel_shown() {
            return;
        }
        if notifications_of(self.tray.as_ref(), self.link.as_ref()).is_some_and(|n| n.center().view(n.held()).unread > 0) {
            self.notify_do(|n| n.center_mut().mark_read());
        }
        let now = Instant::now();
        let built = {
            let App { tray, link, popup, .. } = &mut *self;
            let players = mpris_of(tray.as_ref(), link.as_ref()).map(Mpris::players);
            match (notifications_of(tray.as_ref(), link.as_ref()), popup.as_mut().map(|o| &mut o.body)) {
                (Some(n), Some(PopupBody::Panel(p))) => {
                    panel::scroll_into_view(p, n.center(), players);
                    let before = p.entries;
                    p.entries = n.center().history().count();
                    Some((panel::rows(n.center(), players, p, now, crate::tray::notifications::minute_now()), before, p.entries))
                }
                _ => None,
            }
        };
        let Some((rows, before, after)) = built else {
            self.popup_close(PopupClose::ItemGone);
            return;
        };
        if before != after {
            log(&format!("panel: entries {after}"));
        }
        self.popup_paint(rows);
        self.media_timer_sync();
    }
}
