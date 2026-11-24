//! The quick panel's Shield detail: the USB devices the daemon read as blocked, and the keys and
//! clicks that allow one.
//!
//! This is the pure half of the detail. It holds what the detail knows (`Listing`), the tags (`…`
//! while a device's `allow` command runs, `failed` for `FAILED_SHOWN`) and the notes shown in place
//! of a list ("Reading…", "No blocked device", and so on). What a key asks (`ShieldAsk`) is the
//! wiring's, in `shield/app.rs`; the panel's own glue is `quick/app.rs`.
//!
//! Never:
//! - write or create a file, or make a raw system call (guardrail
//!   `the_quick_model_and_its_state_reader_only_read`);
//! - run a command or start a thread (guardrail `the_tray_spawns_no_thread`, through its
//!   `NO_THREAD_NOR_COMMAND` list), or arm a timer (the table of timer and thread tokens in
//!   `nothing_resident_while_the_launcher_is_closed`);
//! - read a clock or a file;
//! - put a device's name, id or `vid:pid` in a log line (guardrail `the_shield_logs_name_nothing`);
//! - panic: a USB name is text from outside (guardrail `the_shield_files_have_no_panicking_path`).
//!
//! See: docs/features/quick-settings.md,
//! and docs/features/shield.md.

use crate::actions::Button;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::keys::Key;
pub use crate::quick::FAILED_SHOWN;
use crate::quick::{FAILED_TAG, Line, QuickAction, RUNNING_TAG, moved, nav, note_row, text_row};
use crate::shield::ShieldView;
use crate::shield::usb::UsbDevice;
use std::time::Instant;

/// The most device rows the detail lists (the Bluetooth detail's bound).
pub const ROWS_MAX: usize = 32;
pub const NOTE_READING: &str = "Reading…";
pub const NOTE_NONE: &str = "No blocked device";
pub const NOTE_FAILED: &str = "Cannot list the devices";
pub const NOTE_OFF: &str = "Shield is off";
pub const NOTE_UNKNOWN: &str = "Shield state unknown";
/// The tag column: `vid:pid` (nine cells) is the widest tag.
const TAG_ROOM: u16 = 9;

/// What a key or a click on the Shield row or its detail asks of the wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShieldAsk {
    /// Switch to the other state (`App::shield_switch`).
    Toggle,
    /// Allow the blocked device with this USBGuard id (`App::shield_allow`).
    Allow(u32),
}

/// What the detail knows of the blocked devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Reading,
    Devices(Vec<UsbDevice>),
    Failed,
    /// The mode is off: nothing is run, nothing listed.
    Off,
    /// The flag cannot be read: nothing is run, nothing listed, and the note does not say "off".
    Unknown,
}

impl Listing {
    /// What the detail says while nothing is listed: off, or unknown (never "off" on a guess).
    fn idle(state: Option<bool>) -> Listing {
        if state.is_none() { Listing::Unknown } else { Listing::Off }
    }
}

/// The detail's own state, dropped with the panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShieldPanel {
    pub open: bool,
    pub listing: Option<Listing>,
    /// The selected device row.
    pub pick: usize,
    /// The device whose `allow` runs.
    pub allowing: Option<u32>,
    /// The device whose `allow` failed, shown until then.
    pub failed: Option<(u32, Instant)>,
    /// `allow` is configured: device rows are selectable (`fit`'s copy of the view's).
    pub can_allow: bool,
    /// A read of the devices is wanted: taken by the wiring (`App::shield_panel_sync`, in `shield/app.rs`).
    pub read_wanted: bool,
    /// The row's own `failed` ends at this instant (the view's `failed_until`, copied by `fit`).
    row_failed: Option<Instant>,
    /// The row's `failed` ended: the wiring clears the model's and republishes.
    pub row_failed_due: bool,
}

impl ShieldPanel {
    fn devices(&self) -> &[UsbDevice] {
        match &self.listing {
            Some(Listing::Devices(d)) => d.get(..ROWS_MAX).unwrap_or(d),
            _ => &[],
        }
    }

    /// Enter, → or a click on the row: the detail, with one read asked while the mode is on. Nothing
    /// when `blocked` is `[]`.
    pub fn open_detail(&mut self, view: &ShieldView) -> QuickAction {
        if !view.can_list {
            return QuickAction::Nothing;
        }
        self.open = true;
        self.pick = 0;
        self.allowing = None;
        self.failed = None;
        self.can_allow = view.can_allow;
        if view.state == Some(true) {
            self.listing = Some(Listing::Reading);
            self.read_wanted = true;
        } else {
            self.listing = Some(Listing::idle(view.state));
        }
        QuickAction::Redraw
    }

    /// Back on the row: nothing of the detail is kept.
    pub fn close_detail(&mut self) {
        let row_failed = self.row_failed;
        *self = ShieldPanel { row_failed, ..ShieldPanel::default() };
    }

    /// Before each use, with the view the panel reads: the row's `failed` end noted; an open detail
    /// follows the mode (on: one read; off: the list dropped) and keeps its selection on a drawn row.
    pub fn fit(&mut self, view: Option<&ShieldView>) {
        self.row_failed = match view {
            Some(v) => v.failed_until,
            None => None,
        };
        let Some(v) = view else { return };
        self.can_allow = v.can_allow;
        if !self.open {
            return;
        }
        match (v.state == Some(true), &self.listing) {
            (true, None | Some(Listing::Off | Listing::Unknown)) => {
                self.listing = Some(Listing::Reading);
                self.read_wanted = true;
            }
            (false, l) if *l != Some(Listing::idle(v.state)) => {
                self.listing = Some(Listing::idle(v.state));
                self.pick = 0;
            }
            _ => {}
        }
        self.pick = self.pick.min(self.devices().len().saturating_sub(1));
    }

    /// The read's result, for the open detail while the mode is on; anything else is dropped (false).
    pub fn set_listing(&mut self, view: &ShieldView, result: Result<Vec<UsbDevice>, ()>) -> bool {
        if !self.open || view.state != Some(true) {
            return false;
        }
        self.listing = Some(match result {
            Ok(d) => Listing::Devices(d),
            Err(()) => Listing::Failed,
        });
        self.pick = self.pick.min(self.devices().len().saturating_sub(1));
        true
    }

    /// An `allow` ended: its tag goes; a failure shows `failed` on that row for FAILED_SHOWN.
    pub fn allow_done(&mut self, id: u32, failed: bool, now: Instant) {
        if self.allowing == Some(id) {
            self.allowing = None;
        }
        self.failed = if failed { Some((id, now + FAILED_SHOWN)) } else { None };
    }

    /// The header, a separator, then the devices or one note.
    pub fn lines(&self) -> Vec<Line> {
        let mut v = vec![Line::ShieldHeader, Line::Separator];
        match &self.listing {
            Some(Listing::Devices(d)) if !d.is_empty() => v.extend((0..self.devices().len()).map(Line::ShieldDevice)),
            Some(Listing::Devices(_)) => v.push(Line::ShieldNote(NOTE_NONE)),
            Some(Listing::Failed) => v.push(Line::ShieldNote(NOTE_FAILED)),
            Some(Listing::Off) => v.push(Line::ShieldNote(NOTE_OFF)),
            Some(Listing::Unknown) => v.push(Line::ShieldNote(NOTE_UNKNOWN)),
            Some(Listing::Reading) | None => v.push(Line::ShieldNote(NOTE_READING)),
        }
        v
    }

    /// Whether `line` is the selected one: a device, while `allow` is configured.
    pub fn is_selected(&self, line: Line, can_allow: bool) -> bool {
        matches!(line, Line::ShieldDevice(i) if can_allow && i == self.pick)
    }

    /// ↑ ↓ Home End move over the devices (when `allow` is configured), Enter allows the selected
    /// one, Esc and ← go back.
    pub fn key(&mut self, view: &ShieldView, k: Key, _now: Instant) -> QuickAction {
        match k {
            Key::Escape | Key::Left => {
                self.close_detail();
                QuickAction::Redraw
            }
            Key::Enter => self.allow(view, self.pick),
            _ if view.can_allow && !self.devices().is_empty() => match nav(k, Some(self.pick), self.devices().len()) {
                Some(to) => moved(&mut self.pick, to),
                None => QuickAction::Nothing,
            },
            _ => QuickAction::Nothing,
        }
    }

    /// A left press: the header goes back, a device is allowed. The other buttons do nothing.
    pub fn press(&mut self, view: &ShieldView, line: Line, b: Button, _now: Instant) -> QuickAction {
        if b != Button::Left {
            return QuickAction::Nothing;
        }
        match line {
            Line::ShieldHeader => {
                self.close_detail();
                QuickAction::Redraw
            }
            Line::ShieldDevice(i) => self.allow(view, i),
            _ => QuickAction::Nothing,
        }
    }

    /// Allows the i-th device: only with `allow` configured, once at a time.
    fn allow(&mut self, view: &ShieldView, i: usize) -> QuickAction {
        if !view.can_allow || self.allowing.is_some() {
            return QuickAction::Nothing;
        }
        let Some(device) = self.devices().get(i) else { return QuickAction::Nothing };
        let id = device.id;
        self.pick = i;
        self.allowing = Some(id);
        self.failed = None;
        QuickAction::Shield(ShieldAsk::Allow(id))
    }

    /// The earliest `failed` to clear (a device's, or the row's own).
    pub fn deadline(&self) -> Option<Instant> {
        // `flatten` drops the ends that are None, `min` keeps the earliest of the rest.
        [self.failed.map(|(_, at)| at), self.row_failed].into_iter().flatten().min()
    }

    /// At `now`: the `failed` tags that ended cleared; true when something must be redrawn.
    pub fn due(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        if self.failed.is_some_and(|(_, at)| at <= now) {
            self.failed = None;
            redraw = true;
        }
        if self.row_failed.is_some_and(|at| at <= now) {
            self.row_failed = None;
            self.row_failed_due = true;
            redraw = true;
        }
        redraw
    }

    /// One detail line into `g` (`WIDTH + 2` wide, a pad cell each side).
    pub fn draw(&self, g: &mut Grid, line: Line, view: &ShieldView) {
        match line {
            Line::ShieldHeader => text_row(g, None, "‹ Shield", "", Style::NORMAL, false),
            Line::ShieldNote(note) => note_row(g, note),
            Line::ShieldDevice(i) => {
                if let Some(d) = self.devices().get(i) {
                    let (tag, style) = self.tag(d);
                    device_row(g, d, &tag, style, self.is_selected(line, view.can_allow));
                }
            }
            _ => {}
        }
    }

    /// `…` while its `allow` runs, `failed` after one that failed, else its `vid:pid`, muted.
    fn tag(&self, d: &UsbDevice) -> (String, Style) {
        if self.allowing == Some(d.id) {
            (RUNNING_TAG.into(), Style::MUTED)
        } else if self.failed.is_some_and(|(id, _)| id == d.id) {
            (FAILED_TAG.into(), Style::WARN)
        } else {
            (d.vid_pid.clone(), Style::MUTED)
        }
    }
}

/// "Test Stick   1234:5678": the name cut to fit, the tag column ending one cell before the row's
/// end whatever the name; the name keeps one blank cell before it.
fn device_row(g: &mut Grid, d: &UsbDevice, tag: &str, tag_style: Style, selected: bool) {
    let end = g.width();
    // The mark and its space (3 cells with the pad), the gap, the tag column, the last cell.
    let cells = end.saturating_sub(3 + 1 + TAG_ROOM + 1);
    text_row(g, Some(' '), &truncate(&d.name, cells), "", Style::NORMAL, selected);
    let w = text_width(tag).min(TAG_ROOM);
    g.write(end.saturating_sub(TAG_ROOM + 1) + (TAG_ROOM - w), end, tag, if selected { Style::SELECTED } else { tag_style });
}

#[cfg(test)]
mod tests;
