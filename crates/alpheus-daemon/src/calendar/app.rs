//! The calendar popup's wiring: opens it under the clock, moves the month, closes it.
//!
//! A left press on the clock's zone toggles the popup. A `Calendar` (`mod.rs`) is built fresh at
//! each open from the locale's first day and the bar's `[clock]`, and dropped on close. The month
//! moves with the arrow keys and the wheel (`calendar_key`, `calendar_pointer`); Escape closes it.
//! `OPS` is this popup's row in the table of popups (`popup::POPUPS`), which is how the shared
//! popup code reaches the functions below.
//!
//! Never:
//! - read the locale at start or keep anything while closed: the locale is read at each open (see
//!   docs/decisions.md, "The calendar's first day comes from the locale, read at each open");
//! - start a thread or a process.
//!
//! See: docs/features/calendar.md.

use crate::calendar::{Calendar, locale_first_day};
use crate::clock::LocalTime;
use crate::keys::Key;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer, no_hook};
use crate::wayland::App;
use smithay_client_toolkit::reexports::client::QueueHandle;

/// The calendar's row of `popup::POPUPS`: what the shared popup code calls for it. `flush` and
/// `closed` have nothing to do (`no_hook`): the calendar keeps no timer and no state.
// `pub(crate)` means the item is visible inside the daemon, and nowhere outside it.
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Calendar,
    toggle: |app, qh, anchor, _| app.calendar_toggle(qh, anchor),
    key: App::calendar_key,
    pointer: App::calendar_pointer,
    flush: no_hook,
    closed: no_hook,
};

impl App {
    /// Left press on the clock zone: toggles the calendar under it (a second click closes it).
    pub(crate) fn calendar_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if matches!(self.popup_body(), Some(PopupBody::Calendar(_))) {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        // The locale's first day, read at each open: never at startup, and nothing kept while
        // closed.
        let calendar = Calendar::new(&LocalTime::now().0, locale_first_day(), self.bar.config.clock.week_numbers);
        let rows = crate::list::padded(&calendar.render());
        let month = crate::popup::calendar_month(&calendar);
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Calendar(calendar), columns, height, Input::Grab) {
            self.popup_paint(rows);
            log(&format!("calendar shown {month}"));
        }
    }

    /// ← and → change the month, Escape closes. Any other key does nothing.
    fn calendar_key(&mut self, k: Key) {
        match k {
            Key::Left => self.calendar_scroll(-1),
            Key::Right => self.calendar_scroll(1),
            Key::Escape => self.popup_close(PopupClose::Escape),
            _ => {}
        }
    }

    /// The wheel changes month.
    fn calendar_pointer(&mut self, _: Option<(u16, u16)>, kind: PopupPointer) {
        if let PopupPointer::Scroll(steps) = kind {
            self.calendar_scroll(steps);
        }
    }

    /// Shows the month `months` later (earlier when negative), and repaints. The popup's height
    /// follows the month's number of weeks, and `popup_paint` places the popup again when the size
    /// changed.
    fn calendar_scroll(&mut self, months: i32) {
        // `let ... else` leaves the function when the open popup is not a calendar.
        let Some(PopupBody::Calendar(c)) = self.popup_body_mut() else { return };
        // No move (a scroll of 0, or the end of the range): nothing to repaint.
        if !c.scroll(months) {
            return;
        }
        let rows = crate::list::padded(&c.render());
        let month = crate::popup::calendar_month(c);
        self.popup_paint(rows);
        log(&format!("calendar shows {month}"));
    }
}
