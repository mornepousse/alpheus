//! The privacy indicators' wiring (privacy spec §§ 4–6): the loop feeds `Bar.privacy` (`privacy_at`
//! then `privacy_changed`), the off-delay's `DeadlineTimer` (`privacy_timer`) follows
//! `Privacy::deadline` whether the popup is open or not, and the popup, opened by a left press on
//! the module, follows what is shown and closes once nothing is.

use crate::keys::Key;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, no_hook};
use crate::state::Msg;
use crate::wayland::App;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::Instant;

/// What the popup does after a change of what is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Follow {
    Refresh,
    Close,
    Nothing,
}

/// `shown_empty`: every indicator is off (the off-delay already passed); `open`: the privacy popup
/// is the open one.
pub(crate) fn after_change(shown_empty: bool, open: bool) -> Follow {
    match (open, shown_empty) {
        (false, _) => Follow::Nothing,
        (true, true) => Follow::Close,
        (true, false) => Follow::Refresh,
    }
}

/// The privacy popup's row of `popup::POPUPS`.
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Privacy,
    toggle: |app, qh, anchor, _| app.privacy_toggle(qh, anchor),
    key: App::privacy_key,
    // Nothing on it reacts: a click or the wheel inside does nothing (privacy spec § 5).
    pointer: |_, _, _| {},
    flush: no_hook,
    // Its off-delay's timer is the bar's (`privacy_timer`), never the popup's.
    closed: no_hook,
};

impl App {
    pub(crate) fn privacy_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Privacy))
    }

    /// Not selectable: Escape closes it, every other key does nothing (privacy spec § 5).
    fn privacy_key(&mut self, k: Key) {
        if k == Key::Escape {
            self.popup_close(PopupClose::Escape);
        }
    }

    /// A left press on the module: toggles its popup under it (a second press closes it).
    pub(crate) fn privacy_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if self.privacy_shown() {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        if self.bar.privacy.shown().is_empty() {
            return;
        }
        let rows = self.bar.privacy.render();
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Privacy, columns, height, Input::Grab) {
            self.popup_paint(rows);
            log("privacy popup shown");
        }
    }

    /// A sound or compositor message, read before the bar keeps it (privacy spec § 4): a change
    /// redraws the bar and follows the popup and the off-delay's timer. An indicator going off
    /// changes nothing shown yet, only the deadline: the timer follows it all the same.
    pub(crate) fn privacy_feed(&mut self, m: &Msg) {
        if self.bar.privacy_at(m, Instant::now()) {
            self.privacy_changed();
        } else {
            self.privacy_timer_sync();
        }
    }

    /// What is shown changed (`privacy_at` or the off-delay): the bar redrawn after this pass, the
    /// open popup repainted or closed, the timer re-synced.
    pub(crate) fn privacy_changed(&mut self) {
        self.dirty = true;
        match after_change(self.bar.privacy.shown().is_empty(), self.privacy_shown()) {
            Follow::Refresh => {
                let rows = self.bar.privacy.render();
                self.popup_paint(rows);
            }
            Follow::Close => self.popup_close(PopupClose::NothingCaptured),
            Follow::Nothing => {}
        }
        self.privacy_timer_sync();
    }

    /// The timer follows the earliest pending off; none without one (privacy spec § 4).
    pub(crate) fn privacy_timer_sync(&mut self) {
        let at = self.bar.privacy.deadline();
        self.privacy_timer.sync_opt(self.loop_handle.as_ref(), at, App::privacy_tick);
    }

    /// The timer fired (calloop callback; its source is dropped): what is due goes off.
    pub(crate) fn privacy_tick(app: &mut App, now: Instant) {
        app.privacy_timer.fired();
        if app.bar.privacy.due(now) {
            app.privacy_changed();
        } else {
            app.privacy_timer_sync();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Privacy spec § 5: while open, the popup follows `shown`; once it is empty
    /// (the off-delay passed) it closes; closed, nothing is opened.
    #[test]
    fn the_popup_closes_when_nothing_is_captured_after_the_delay() {
        assert_eq!(after_change(true, true), Follow::Close);
        assert_eq!(after_change(false, true), Follow::Refresh);
        assert_eq!(after_change(true, false), Follow::Nothing);
        assert_eq!(after_change(false, false), Follow::Nothing);
    }
}
