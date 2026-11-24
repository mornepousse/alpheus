//! The quick panel's do-not-disturb detail: the six choices behind the row, and what a key or a click
//! on them asks for.
//!
//! The choices are off, a duration (30 minutes, 1 hour, 2 hours), until a clock time
//! (`[quick] dnd_until`) and until turned off. The `●` sits on the choice the current state matches.
//! This file is the pure half: what a choice does is the wiring's (`dnd_request` in `app.rs`).
//!
//! Never:
//! - write or create a file, or make a raw system call (guardrail
//!   `the_quick_model_and_its_state_reader_only_read`);
//! - run a command or start a thread (guardrail `the_tray_spawns_no_thread`, through its
//!   `NO_THREAD_NOR_COMMAND` list), or arm a timer (the table of timer and thread tokens in
//!   `nothing_resident_while_the_launcher_is_closed`);
//! - read a clock or a file;
//! - panic (guardrail `the_quick_panel_has_no_panicking_path`).
//!
//! See: docs/features/quick-settings.md,
//! and docs/decisions.md ("The do-not-disturb detail marks and opens by the current state").

use crate::actions::Button;
use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::quick::{Line, Live, QuickAction, moved, nav, text_row};

/// One entry of the detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndChoice {
    Off,
    /// On for this many seconds.
    For(u32),
    /// On until `[quick] dnd_until`'s next local time.
    Until,
    Untimed,
}

/// The entries, top to bottom.
pub const CHOICES: [DndChoice; 6] =
    [DndChoice::Off, DndChoice::For(1800), DndChoice::For(3600), DndChoice::For(7200), DndChoice::Until, DndChoice::Untimed];

/// The entry `open` falls back on when the state marks none, or `Off`: `30 minutes`.
const DEFAULT_AT: usize = 1;

/// An entry's text; `until` is `[quick] dnd_until`.
pub fn label(c: DndChoice, until: (u32, u32)) -> String {
    match c {
        DndChoice::Off => "Off".into(),
        DndChoice::For(1800) => "30 minutes".into(),
        DndChoice::For(3600) => "1 hour".into(),
        DndChoice::For(7200) => "2 hours".into(),
        DndChoice::For(s) => format!("{} minutes", s / 60),
        DndChoice::Until => format!("Until {:02}:{:02}", until.0, until.1),
        DndChoice::Untimed => "Until I turn it off".into(),
    }
}

/// Where `choice` is in `CHOICES`.
fn position(choice: DndChoice) -> Option<usize> {
    CHOICES.iter().position(|c| *c == choice)
}

/// The entry carrying the ●: off marks `Off`; on and untimed (sharing included) `Until I turn it
/// off`; a timed end whose local time is `dnd_until` the `Until` one; anything else (a duration, an
/// end elsewhere) nothing; an unknown state nothing. See docs/decisions.md, "The do-not-disturb detail
/// marks and opens by the current state".
pub fn marked(on: Option<bool>, end: Option<(u8, u8)>, until: (u32, u32)) -> Option<usize> {
    // `?` after a value that is None ends the function with None: an unknown state marks nothing.
    let on = on?;
    match (on, end) {
        (false, _) => position(DndChoice::Off),
        (true, None) => position(DndChoice::Untimed),
        (true, Some((h, m))) => {
            if (u32::from(h), u32::from(m)) == until {
                position(DndChoice::Until)
            } else {
                None
            }
        }
    }
}

/// The open detail's own state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DndDetail {
    pub open: bool,
    pub selected: usize,
}

impl DndDetail {
    /// Opened on the ● entry, else on `30 minutes` (also when that entry is `Off`: nothing to turn off).
    pub fn open(&mut self, live: &Live) {
        self.open = true;
        self.selected = match marked(live.dnd, live.dnd_end, live.config.dnd_until_hm()) {
            // Entry 0 is `Off`: nothing to turn off, so the fallback is chosen instead.
            Some(i) if i != 0 => i,
            _ => DEFAULT_AT,
        };
    }

    /// The header, a separator, the six entries.
    pub fn lines(&self) -> Vec<Line> {
        let mut lines = vec![Line::DndHeader, Line::Separator];
        lines.extend((0..CHOICES.len()).map(Line::DndChoice));
        lines
    }

    /// ↑ ↓ Home End move; Enter applies the entry, Esc and ← go back; Space and the rest do nothing.
    pub fn key(&mut self, _live: &Live, k: Key) -> QuickAction {
        match k {
            Key::Escape | Key::Left => {
                self.open = false;
                QuickAction::Redraw
            }
            Key::Enter => self.apply(self.selected),
            _ => match nav(k, Some(self.selected), CHOICES.len()) {
                Some(to) => moved(&mut self.selected, to),
                None => QuickAction::Nothing,
            },
        }
    }

    /// A left press: the header goes back, an entry applies. The right button does nothing.
    pub fn press(&mut self, _live: &Live, line: Line, b: Button) -> QuickAction {
        match (line, b) {
            (Line::DndHeader, Button::Left) => {
                self.open = false;
                QuickAction::Redraw
            }
            (Line::DndChoice(i), Button::Left) => self.apply(i),
            _ => QuickAction::Nothing,
        }
    }

    /// The entry chosen: the detail closes (the panel is back on its row), the wiring applies it.
    fn apply(&mut self, i: usize) -> QuickAction {
        let Some(c) = CHOICES.get(i).copied() else { return QuickAction::Nothing };
        self.selected = i;
        self.open = false;
        QuickAction::Dnd(c)
    }

    /// One line as drawn.
    pub fn draw(&self, g: &mut Grid, live: &Live, line: Line) {
        let until = live.config.dnd_until_hm();
        match line {
            Line::DndHeader => {
                // The end of a timed do-not-disturb, in two digits ("until 07:05"), else nothing.
                let right = match live.dnd_end {
                    Some((h, m)) => format!("until {h:02}:{m:02}"),
                    None => String::new(),
                };
                text_row(g, None, "‹ Do not disturb", &right, Style::MUTED, false);
            }
            Line::DndChoice(i) => {
                let Some(c) = CHOICES.get(i) else { return };
                let mark = if marked(live.dnd, live.dnd_end, until) == Some(i) { '●' } else { ' ' };
                text_row(g, Some(mark), &label(*c, until), "", Style::NORMAL, self.selected == i);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
