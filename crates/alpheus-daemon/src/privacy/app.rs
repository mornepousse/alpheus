//! The privacy indicators' wiring on the bar: the feed, the off-delay's timer and the popup.
//!
//! The loop feeds each sound or compositor message to `Bar.privacy` before the bar keeps it
//! (`privacy_feed`); a change redraws the bar and follows the popup (`privacy_changed`). The
//! off-delay's deadline timer (`privacy_timer`) follows `Privacy::deadline` whether the popup is
//! open or not. The popup, opened by a left press on the module, follows what is shown and closes
//! once nothing is. The model is `mod.rs`; the camera watch, which also feeds `Bar.privacy`, is
//! `camera_app.rs`.
//!
//! Never:
//! - let the popup own the timer: closing the popup leaves it alone (`popup_close` names no timer,
//!   and the popup's row in `popup::POPUPS` has no hook for it);
//! - start a thread, a process or a command, or panic: a guardrail checks that no `unwrap`, `expect`
//!   or `panic!` is in this file or in `mod.rs`.
//!
//! See: docs/specs/2026-10-16-alpheus-privacy-design.md, docs/features/privacy.md, and
//! docs/decisions.md ("The privacy popup lists apps and selects nothing", "An indicator going off
//! stays shown for half a second").

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

/// What the popup does after a change. `shown_empty` is true when every indicator is off (the
/// off-delay has already passed); `open` is true when the privacy popup is the open one. A popup
/// that is open follows what is shown, and closes once nothing is shown; a closed one is never
/// opened by a change.
pub(crate) fn after_change(shown_empty: bool, open: bool) -> Follow {
    match (open, shown_empty) {
        (false, _) => Follow::Nothing,
        (true, true) => Follow::Close,
        (true, false) => Follow::Refresh,
    }
}

/// The privacy popup's row in the table of popups (`popup::POPUPS`).
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Privacy,
    toggle: |app, qh, anchor, _| app.privacy_toggle(qh, anchor),
    key: App::privacy_key,
    // Nothing on it reacts: a click or the wheel inside does nothing (privacy spec § 5; see
    // docs/decisions.md, "The privacy popup lists apps and selects nothing").
    pointer: |_, _, _| {},
    flush: no_hook,
    // The off-delay's timer is the bar's (`privacy_timer`), never the popup's.
    closed: no_hook,
};

impl App {
    pub(crate) fn privacy_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Privacy))
    }

    /// The popup selects nothing: Escape closes it, and every other key does nothing (privacy spec
    /// § 5).
    fn privacy_key(&mut self, k: Key) {
        if k == Key::Escape {
            self.popup_close(PopupClose::Escape);
        }
    }

    /// A left press on the module: toggles its popup under it (a second press closes it). With
    /// nothing shown there is nothing to list, so nothing opens.
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

    /// A sound or compositor message, read before the bar keeps it (privacy spec § 4). A change
    /// redraws the bar and follows the popup and the off-delay's timer. A message that changes
    /// nothing shown can still start an off-delay, which moves only the deadline; the timer follows
    /// it all the same, or the glyph would stay lit until the next message.
    pub(crate) fn privacy_feed(&mut self, m: &Msg) {
        if self.bar.privacy_at(m, Instant::now()) {
            self.privacy_changed();
        } else {
            self.privacy_timer_sync();
        }
    }

    /// What is shown changed (a message, a scan or the off-delay): the bar is redrawn after this
    /// pass, the open popup is repainted or closed, the timer is synced, and do-not-disturb and the
    /// hooks are told. The timer is synced first, then do-not-disturb (a guardrail pins the order).
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
        self.dnd_sharing_follow();
        self.hooks_capture();
    }

    /// The timer follows the earliest pending off, and there is none without one (privacy spec § 4).
    pub(crate) fn privacy_timer_sync(&mut self) {
        let at = self.bar.privacy.deadline();
        self.privacy_timer.sync_opt(self.loop_handle.as_ref(), at, App::privacy_tick);
    }

    /// The timer fired (calloop callback; the loop dropped its source): what is due goes off. The
    /// dropped source is forgotten first, so that the sync afterwards arms a new one.
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

    /// While open, the popup follows what is shown; once nothing is shown (the off-delay passed) it
    /// closes; closed, no change opens it (privacy spec § 5).
    #[test]
    fn the_popup_closes_when_nothing_is_captured_after_the_delay() {
        assert_eq!(after_change(true, true), Follow::Close);
        assert_eq!(after_change(false, true), Follow::Refresh);
        assert_eq!(after_change(true, false), Follow::Nothing);
        assert_eq!(after_change(false, false), Follow::Nothing);
    }
}
