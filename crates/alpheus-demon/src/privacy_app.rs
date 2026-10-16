//! The privacy indicators' wiring (privacy spec §§ 4–6): the loop feeds `Bar.privacy` (`privacy_at`
//! then `privacy_changed`), the off-delay's `DeadlineTimer` (`privacy_timer`) follows
//! `Privacy::deadline` whether the popup is open or not, and the popup, opened by a left press on
//! the module, follows what is shown and closes once nothing is.

use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, PopupBody, PopupClose};
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

impl App {
    pub(crate) fn privacy_shown(&self) -> bool {
        matches!(self.popup.as_ref().map(|p| &p.body), Some(PopupBody::Privacy))
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
        if let Some(handle) = self.loop_handle.clone() {
            self.privacy_timer.sync(&handle, at, App::privacy_tick);
        }
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

    /// Privacy spec § 5 (Review Focus 4): while open, the popup follows `shown`; once it is empty
    /// (the off-delay passed) it closes; closed, nothing is opened.
    #[test]
    fn the_popup_closes_when_nothing_is_captured_after_the_delay() {
        assert_eq!(after_change(true, true), Follow::Close);
        assert_eq!(after_change(false, true), Follow::Refresh);
        assert_eq!(after_change(true, false), Follow::Nothing);
        assert_eq!(after_change(false, false), Follow::Nothing);
    }
}
