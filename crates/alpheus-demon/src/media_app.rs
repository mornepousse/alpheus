//! The media popup's wiring: the bar's media view, the popup's toggle, actions and pin, its rows
//! refreshed after each host output, and the 1 s tick (`media_timer`) armed only while the popup
//! (or the panel's card) shows and the active player plays.

use crate::actions::Button;
use crate::keys::Key;
use crate::log::write as log;
use crate::media::{MediaView, Players};
use crate::media_popup::{self, MediaAction, MediaPopup};
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer, no_hook};
use crate::state::Msg;
use crate::tray::mpris::Mpris;
use crate::tray_bridge::mpris_of;
use crate::wayland::App;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::Instant;

/// The media popup's row of `popup::POPUPS`.
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Media,
    toggle: |app, qh, anchor, _| app.media_toggle(qh, anchor),
    key: App::media_key,
    pointer: App::media_pointer,
    flush: no_hook,
    closed: App::media_closed,
};

impl App {
    /// The media module's view from either connection. The bars repaint only when what they show
    /// changes (`Bar::apply`), and one line marks the module appearing or going.
    pub(crate) fn media_view(&mut self, view: Option<MediaView>) {
        let (before, after) = (self.bar.media.is_some(), view.is_some());
        if self.bar.apply(Msg::Media(view)) {
            self.dirty = true;
        }
        if before != after {
            log(if after { "media: shown" } else { "media: hidden" });
        }
    }

    /// Left press on the media module: toggles its popup under it (a second click closes it).
    pub(crate) fn media_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if self.media_shown() {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        let now = Instant::now();
        let built = mpris_of(self.tray.as_ref(), self.link.session.as_ref())
            .map(Mpris::players)
            .filter(|p| p.shown().next().is_some())
            .map(|players| {
                let popup = MediaPopup::new(players);
                let rows = media_popup::rows(players, &popup, now, media_popup::WIDTH);
                let heading = media_popup::heading(players, &rows);
                (popup, rows, heading)
            });
        let Some((popup, rows, heading)) = built else { return };
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Media(popup), columns, height, Input::Grab) {
            self.popup_paint(rows);
            self.mpris_do(|m, out| m.popup_opened(out));
            self.media_timer_sync();
            // What it shows on top, as drawn (the headless tests read it).
            log(&format!("media popup shown: {heading}"));
        }
    }

    pub(crate) fn media_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Media(_)))
    }

    /// `f` on the open media popup and the players; Nothing when either is missing.
    pub(crate) fn media_with(&mut self, f: impl FnOnce(&mut MediaPopup, &Players) -> MediaAction) -> MediaAction {
        let App { tray, link, popup, .. } = self;
        match (mpris_of(tray.as_ref(), link.session.as_ref()), popup.as_mut().map(|p| &mut p.body)) {
            (Some(m), Some(PopupBody::Media(p))) => f(p, m.players()),
            _ => MediaAction::Nothing,
        }
    }

    pub(crate) fn media_action(&mut self, action: MediaAction) {
        match action {
            MediaAction::Nothing => {}
            MediaAction::Redraw => self.media_popup_refresh(),
            MediaAction::Close => self.popup_close(PopupClose::Escape),
            // The player answers with PropertiesChanged, which repaints; the refresh shows the
            // selection the press moved.
            MediaAction::Control(c) => {
                self.mpris_do(|m, out| m.control(out, c));
                self.media_popup_refresh();
            }
            MediaAction::Pin(name) => {
                self.media_pin(&name);
                self.media_popup_refresh();
            }
        }
    }

    /// Pins `name` (the media popup's or the panel's card). A pin changes the active player with no
    /// D-Bus traffic to carry the bar's view: it is taken here, at once (the one place it changes
    /// outside an output).
    pub(crate) fn media_pin(&mut self, name: &str) {
        let mut change = None;
        self.mpris_do(|m, out| {
            m.pin(name);
            m.resync_active(out);
            change = m.take_view_change();
        });
        if let Some(view) = change {
            self.media_view(view);
        }
    }

    /// After anything that may change what the popup shows (a host's output, a tick, a key): the
    /// rows rebuilt and painted (`popup_draw` commits nothing when they did not change), the tick
    /// re-synced. The bar's view is not rebuilt here (once a tick, per output): the outputs carry
    /// it (`take_view_change`), and the pin takes it itself.
    /// No player shown any more, or no host: the popup closes (player gone). Popup closed: only
    /// the timer is synced (to none).
    pub(crate) fn media_popup_refresh(&mut self) {
        if !self.media_shown() {
            self.media_timer_sync();
            return;
        }
        let now = Instant::now();
        let rows = {
            let App { tray, link, popup, .. } = &mut *self;
            match (mpris_of(tray.as_ref(), link.session.as_ref()).map(Mpris::players), popup.as_mut().map(|o| &mut o.body)) {
                (Some(players), Some(PopupBody::Media(p))) if players.shown().next().is_some() => {
                    p.fit(players);
                    Some(media_popup::rows(players, p, now, media_popup::WIDTH))
                }
                _ => None,
            }
        };
        let Some(rows) = rows else {
            self.popup_close(PopupClose::PlayerGone);
            return;
        };
        self.popup_paint(rows);
        self.media_timer_sync();
    }

    /// The tick follows `media_popup::player_tick`: armed only while the popup is open (or the panel
    /// showing its card) and the active player plays, its position known, before the end of its track; removed otherwise. An armed
    /// tick still ahead is kept (`next_deadline`).
    pub(crate) fn media_timer_sync(&mut self) {
        let now = Instant::now();
        let open = self.media_shown() || self.panel_shown();
        let due = mpris_of(self.tray.as_ref(), self.link.session.as_ref())
            .and_then(|m| m.players().active())
            .and_then(|p| media_popup::player_tick(open, p, now));
        let at = media_popup::next_deadline(due, self.media_timer.deadline(), now);
        self.media_timer.sync_opt(self.loop_handle.as_ref(), at, App::media_tick);
    }

    /// The tick fired (calloop callback; its source is dropped).
    pub(crate) fn media_tick(app: &mut App, _: Instant) {
        app.media_timer.fired();
        app.media_popup_refresh();
        app.panel_refresh();
    }

    /// ↑ ↓ ← →, Enter, Escape (spec § 2).
    fn media_key(&mut self, k: Key) {
        let action = self.media_with(|p, players| media_popup::key(p, players, k));
        self.media_action(action);
    }

    /// A left press on a button or a player does what Enter does; nothing else reacts.
    fn media_pointer(&mut self, cell: Option<(u16, u16)>, kind: PopupPointer) {
        if let (PopupPointer::Press(Button::Left), Some(c)) = (kind, cell) {
            let action = self.media_with(|p, players| media_popup::press(p, players, c, media_popup::WIDTH));
            self.media_action(action);
        }
    }

    /// The media popup or the panel (its card follows the same Seeked rule and tick) went, whatever
    /// closed it (`popup_close`): the Seeked rule goes and the tick with it — `media_timer_sync` sees
    /// no popup now.
    pub(crate) fn media_closed(&mut self) {
        self.mpris_do(|m, out| m.popup_closed(out));
        self.media_timer_sync();
    }
}
