//! The calendar popup's wiring: a left press on the clock zone toggles it under the clock, built
//! fresh at each open from the locale's first day and the bar's `[clock]`, dropped on close.

use crate::calendar::{Calendar, locale_first_day};
use crate::clock::LocalTime;
use crate::keys::Key;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer, no_hook};
use crate::wayland::App;
use smithay_client_toolkit::reexports::client::QueueHandle;

/// The calendar's row of `popup::POPUPS`.
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
        // The locale's first day, read at each open: never at startup, nothing kept
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

    /// ← → change month, Escape closes (spec § 2).
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

    /// The calendar shows `months` later (earlier when negative), repainted: its height follows the month's weeks, `popup_paint`
    /// repositions on a size change.
    fn calendar_scroll(&mut self, months: i32) {
        let Some(PopupBody::Calendar(c)) = self.popup_body_mut() else { return };
        if !c.scroll(months) {
            return;
        }
        let rows = crate::list::padded(&c.render());
        let month = crate::popup::calendar_month(c);
        self.popup_paint(rows);
        log(&format!("calendar shows {month}"));
    }
}
