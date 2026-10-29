//! The calendar popup's wiring: a left press on the clock zone toggles it under the clock, built
//! fresh at each open from the locale's first day and the bar's `[clock]`, dropped on close.

use crate::calendar::{Calendar, locale_first_day};
use crate::clock::LocalTime;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, PopupBody, PopupClose};
use crate::wayland::App;
use smithay_client_toolkit::reexports::client::QueueHandle;

impl App {
    /// Left press on the clock zone: toggles the calendar under it (a second click closes it).
    pub(crate) fn calendar_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if matches!(self.popup_body(), Some(PopupBody::Calendar(_))) {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        // The locale's first day, read at each open (decision 11): never at startup, nothing kept
        // while closed.
        let calendar = Calendar::new(&LocalTime::now().0, locale_first_day(), self.bar.config.clock.week_numbers);
        let rows = crate::list::padded(&calendar.render());
        let month = crate::popup::calendar_month(&calendar);
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Calendar(calendar), columns, height, Input::Grab) {
            self.popup_paint(rows);
            log(&format!("calendar shown {month}"));
        }
    }
}
