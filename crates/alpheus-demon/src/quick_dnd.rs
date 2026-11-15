//! The quick panel's do-not-disturb detail (small features 2 spec § 3), pure: six choices (off, a
//! duration, until a clock time, until turned off), the ● on the one the state matches, its keys and
//! clicks. What a choice does is the wiring's (`quick_app::dnd_request`): nothing here reads a
//! clock, runs a command or starts anything.

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

/// The entry carrying the ●: off marks `Off`; on and untimed (sharing included) `Until I turn it
/// off`; a timed end whose local time is `dnd_until` the `Until` one; anything else (a duration, an
/// end elsewhere) nothing; an unknown state nothing.
pub fn marked(on: Option<bool>, end: Option<(u8, u8)>, until: (u32, u32)) -> Option<usize> {
    let index = |c: DndChoice| CHOICES.iter().position(|x| *x == c);
    match (on?, end) {
        (false, _) => index(DndChoice::Off),
        (true, None) => index(DndChoice::Untimed),
        (true, Some((h, m))) => ((u32::from(h), u32::from(m)) == until).then(|| index(DndChoice::Until)).flatten(),
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
        self.selected = marked(live.dnd, live.dnd_end, live.config.dnd_until_hm()).filter(|i| *i != 0).unwrap_or(DEFAULT_AT);
    }

    /// The header, a separator, the six entries.
    pub fn lines(&self) -> Vec<Line> {
        [Line::DndHeader, Line::Separator].into_iter().chain((0..CHOICES.len()).map(Line::DndChoice)).collect()
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
                let right = live.dnd_end.map(|(h, m)| format!("until {h:02}:{m:02}")).unwrap_or_default();
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
