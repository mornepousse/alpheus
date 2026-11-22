//! The ⏻ power menu (power spec § 2), pure: its entries (the `[quick]` session commands that are
//! set, in the order Lock, Log out, Reboot, Power off), the selection, the keys, hover and press,
//! the reboot and power-off question (`session::activate`) and its expiry, and the rows drawn. The
//! popup (`PopupBody::Power`), the timer and the commands are the wiring's (`power_app.rs`): nothing
//! here reads a file, starts a thread or runs a command.

use crate::config::QuickConfig;
use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::session::{self, Activation, Session};
use std::time::Instant;

/// A row's cells, its pad cell at each end included: the longest text, "⏻ Power off? ⏎"
/// (14 cells), and the two pads.
pub const WIDTH: u16 = 16;

/// What a key, the pointer or the timer asks of the wiring (power spec § 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Nothing,
    Redraw,
    /// Escape.
    Close,
    /// Spawn this entry's `[quick]` command, then close the menu.
    RunAndClose(Session),
}

/// The open menu's own state: dropped whole with its popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Power {
    /// The entries shown, top to bottom; never empty.
    pub entries: Vec<Session>,
    /// The selected entry's index; None at open, as a tray menu: ↓ selects the first, ↑ the last.
    pub selected: Option<usize>,
    /// "Power off? ⏎" (or "Reboot? ⏎") until that instant.
    pub question: Option<(Session, Instant)>,
}

impl Power {
    /// The menu at open; None when no entry has a command (the button is not shown then).
    pub fn new(config: &QuickConfig) -> Option<Power> {
        let entries = session::configured(config);
        (!entries.is_empty()).then_some(Power { entries, selected: None, question: None })
    }

    /// The selection on entry `to`; moving it cancels the question (power spec § 2).
    fn select(&mut self, to: usize) -> PowerAction {
        if self.selected == Some(to) {
            return PowerAction::Nothing;
        }
        self.selected = Some(to);
        self.question = None;
        PowerAction::Redraw
    }

    /// ↑ ↓: none selected yet, the last or the first entry; then clamped, never wrapping (`list::step`).
    fn step(&mut self, delta: i32) -> PowerAction {
        let len = self.entries.len();
        let to = match self.selected {
            Some(i) => list::step(i, delta, len, |_| true),
            None if delta > 0 => 0,
            None => len.saturating_sub(1),
        };
        self.select(to)
    }

    /// Entry `i` activated (Enter, Space, a left press): `session::activate`'s rule.
    fn activate(&mut self, i: usize, now: Instant) -> PowerAction {
        let Some(&s) = self.entries.get(i) else { return PowerAction::Nothing };
        match session::activate(&mut self.question, s, now) {
            Activation::Run => PowerAction::RunAndClose(s),
            Activation::Ask => PowerAction::Redraw,
            Activation::Wait => PowerAction::Nothing,
        }
    }

    /// A key (power spec § 2): ↑ ↓ Tab Shift+Tab (and Ctrl+P, Ctrl+N) move, Enter and Space activate,
    /// Escape closes. Any key but Enter and Space cancels the question. `repeat`: the keyboard's
    /// auto-repeat, which only the moves follow: a held Enter acts once, so it never confirms its own
    /// question 0.4 s later.
    pub fn key(&mut self, k: Key, repeat: bool, now: Instant) -> PowerAction {
        let moves = matches!(k, Key::Up | Key::Down | Key::Tab | Key::BackTab | Key::CtrlP | Key::CtrlN);
        if repeat && !moves {
            return PowerAction::Nothing;
        }
        let cancelled = !matches!(k, Key::Enter | Key::Char(' ')) && self.question.take().is_some();
        let action = match k {
            Key::Escape => PowerAction::Close,
            Key::Up | Key::BackTab | Key::CtrlP => self.step(-1),
            Key::Down | Key::Tab | Key::CtrlN => self.step(1),
            Key::Enter | Key::Char(' ') => match self.selected {
                Some(i) => self.activate(i, now),
                None => PowerAction::Nothing,
            },
            _ => PowerAction::Nothing,
        };
        if cancelled && action == PowerAction::Nothing { PowerAction::Redraw } else { action }
    }

    /// The pointer over content row `row` (one entry per row): selects it.
    pub fn hover(&mut self, row: u16) -> PowerAction {
        let i = usize::from(row);
        if i >= self.entries.len() {
            return PowerAction::Nothing;
        }
        self.select(i)
    }

    /// A left press on content row `row`: selects that entry and activates it.
    pub fn press(&mut self, row: u16, now: Instant) -> PowerAction {
        let i = usize::from(row);
        if i >= self.entries.len() {
            return PowerAction::Nothing;
        }
        self.select(i);
        self.activate(i, now)
    }

    /// The question's end, if one is pending: the only deadline the menu has.
    pub fn deadline(&self) -> Option<Instant> {
        self.question.map(|(_, at)| at)
    }

    /// At `now`: the question gone once its 3 s passed (Redraw), else Nothing.
    pub fn due(&mut self, now: Instant) -> PowerAction {
        if session::expire(&mut self.question, now) { PowerAction::Redraw } else { PowerAction::Nothing }
    }

    /// The rows, `WIDTH` cells wide, drawn inside `list::boxed`'s ring as the tray menus are: the
    /// glyph in the accent then the word; the selection on a band; the entry asking, its question
    /// in warn instead.
    pub fn render(&self) -> Boxed {
        let columns = WIDTH + 2;
        let mut out = vec![Grid::empty(columns)];
        for (i, s) in self.entries.iter().enumerate() {
            let mut g = Grid::empty(columns);
            match self.question {
                Some((asking, _)) if asking == *s => {
                    g.write(2, columns - 1, &s.question(), Style::WARN);
                }
                _ => {
                    let row = Row {
                        icon: Some(s.glyph()),
                        icon_style: Style::ACCENT,
                        text: s.prompt(),
                        right: "",
                        style: Style::NORMAL,
                        selected: self.selected == Some(i),
                    };
                    list::row(&mut g, 1, columns - 1, &row);
                }
            }
            out.push(g);
        }
        out.push(Grid::empty(columns));
        list::boxed(&out, &[], None)
    }

    /// For the log line "power menu shown (…)": the entries by their `[quick]` key.
    pub fn describe(&self) -> String {
        self.entries.iter().map(|s| s.setting()).collect::<Vec<_>>().join(" ")
    }
}

pub mod app;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{CONFIRM, CONFIRM_AFTER, LOCK, LOGOUT, POWER, REBOOT};
    use std::time::Duration;

    /// All four commands set.
    fn all_four() -> QuickConfig {
        QuickConfig { logout: vec!["fake-logout".into()], ..QuickConfig::default() }
    }

    fn texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
    }

    /// Power spec § 2, § 5: one row per entry whose command is set, for every combination, in the
    /// order Lock, Log out, Reboot, Power off; all four empty, no menu.
    #[test]
    fn the_rows_follow_the_commands_in_order_and_none_means_no_menu() {
        let d = Power::new(&QuickConfig::default()).expect("lock, reboot, power off by default");
        assert_eq!(d.entries, [Session::Lock, Session::Reboot, Session::Poweroff], "logout is empty by default");
        assert_eq!(texts(&d.render()), [format!(" {LOCK} Lock"), format!(" {REBOOT} Reboot"), format!(" {POWER} Power off")]);
        assert_eq!(d.describe(), "lock reboot poweroff");
        assert_eq!(texts(&Power::new(&all_four()).unwrap().render())[1], format!(" {LOGOUT} Log out"), "log out once set, second");
        for mask in 0..16u8 {
            let set = |bit: u8| if mask & bit != 0 { vec!["fake".to_string()] } else { Vec::new() };
            let c = QuickConfig { lock: set(1), logout: set(2), reboot: set(4), poweroff: set(8), ..QuickConfig::default() };
            let want: Vec<Session> = Session::ALL.into_iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, s)| s).collect();
            let Some(p) = Power::new(&c) else {
                assert_eq!(mask, 0, "only all four empty means no menu");
                continue;
            };
            assert_eq!(p.entries, want, "mask {mask:04b}");
            let b = p.render();
            assert_eq!(b.cells(), (WIDTH, want.len() as u16), "mask {mask:04b}");
            for (row, s) in texts(&b).iter().zip(&want) {
                assert_eq!(*row, format!(" {} {}", s.glyph(), s.prompt()), "mask {mask:04b}");
            }
        }
    }

    #[test]
    fn the_keys_move_clamped_from_nothing_selected() {
        let t = Instant::now();
        let mut p = Power::new(&all_four()).unwrap();
        assert_eq!(p.key(Key::Enter, false, t), PowerAction::Nothing, "nothing selected at open: Enter does nothing");
        assert_eq!(p.key(Key::Down, false, t), PowerAction::Redraw);
        assert_eq!(p.selected, Some(0), "↓ from nothing: the first");
        assert_eq!(p.key(Key::Up, false, t), PowerAction::Nothing, "the top: no wrap");
        for k in [Key::Tab, Key::Down, Key::CtrlN] {
            assert_eq!(p.key(k, false, t), PowerAction::Redraw, "{k:?} moves down");
        }
        assert_eq!(p.selected, Some(3));
        assert_eq!(p.key(Key::Tab, false, t), PowerAction::Nothing, "the end: no wrap");
        assert_eq!(p.key(Key::BackTab, false, t), PowerAction::Redraw);
        assert_eq!(p.selected, Some(2), "Shift+Tab moves up");
        let mut p = Power::new(&all_four()).unwrap();
        assert_eq!(p.key(Key::Up, false, t), PowerAction::Redraw);
        assert_eq!(p.selected, Some(3), "↑ from nothing: the last");
        assert_eq!(p.key(Key::Escape, false, t), PowerAction::Close);
        let b = p.render();
        assert_eq!(b.rows[3].styles(), "S".repeat(usize::from(WIDTH)), "the selection band across the row");
        assert_eq!(&b.rows[0].styles()[1..2], "a", "the glyph in the accent");
    }

    #[test]
    fn lock_and_log_out_run_at_once() {
        let t = Instant::now();
        let mut p = Power::new(&all_four()).unwrap();
        p.key(Key::Down, false, t);
        assert_eq!(p.key(Key::Enter, false, t), PowerAction::RunAndClose(Session::Lock));
        p.key(Key::Down, false, t);
        assert_eq!(p.key(Key::Char(' '), false, t), PowerAction::RunAndClose(Session::Logout), "Space too");
    }

    /// Power spec § 2 and Mae's ruling: reboot and power off ask; a second activation counts from
    /// 0.4 s to 3 s; the question in warn; its timer only while it is pending.
    #[test]
    fn power_off_asks_and_a_second_activation_confirms_it() {
        let t = Instant::now();
        let mut p = Power::new(&QuickConfig::default()).unwrap();
        p.key(Key::Up, false, t);
        assert_eq!(p.deadline(), None, "no question: no timer");
        assert_eq!(p.key(Key::Enter, false, t), PowerAction::Redraw, "the first activation asks");
        assert_eq!(p.question, Some((Session::Poweroff, t + CONFIRM)));
        assert_eq!(p.deadline(), Some(t + CONFIRM), "the timer waits for it");
        let b = p.render();
        assert_eq!(texts(&b)[2], format!(" {POWER} Power off? ⏎"));
        assert_eq!(&b.rows[2].styles()[1..15], "w".repeat(14), "in warn, glyph to ⏎");
        assert_eq!(p.key(Key::Enter, false, t + Duration::from_millis(100)), PowerAction::Nothing, "a double Enter never confirms");
        assert_eq!(p.press(2, t + Duration::from_millis(200)), PowerAction::Nothing, "nor a double click");
        assert_eq!(p.question, Some((Session::Poweroff, t + CONFIRM)), "the question stays, its 3 s unchanged");
        assert_eq!(p.key(Key::Enter, false, t + CONFIRM_AFTER), PowerAction::RunAndClose(Session::Poweroff), "0.4 s on: it runs");
        let mut p = Power::new(&QuickConfig::default()).unwrap();
        assert_eq!(p.press(1, t), PowerAction::Redraw, "a press on reboot asks");
        assert_eq!(texts(&p.render())[1], format!(" {REBOOT} Reboot? ⏎"));
        assert_eq!(p.press(1, t + Duration::from_secs(2)), PowerAction::RunAndClose(Session::Reboot), "a second press inside 3 s runs it");
    }

    #[test]
    fn a_held_enter_acts_once() {
        let t = Instant::now();
        let mut p = Power::new(&QuickConfig::default()).unwrap();
        p.key(Key::Up, false, t);
        p.key(Key::Enter, false, t);
        assert_eq!(p.key(Key::Enter, true, t + Duration::from_millis(600)), PowerAction::Nothing, "the repeat never confirms");
        assert_eq!(p.key(Key::Char(' '), true, t + Duration::from_millis(650)), PowerAction::Nothing);
        assert!(p.question.is_some(), "and leaves the question");
        assert_eq!(p.key(Key::Up, true, t + Duration::from_millis(700)), PowerAction::Redraw, "a held arrow moves (and cancels)");
        assert_eq!(p.question, None);
    }

    /// Power spec § 2: moving the selection, another entry, a key or 3 s passing cancels the question.
    #[test]
    fn moving_another_entry_a_key_or_3_s_cancel_the_question() {
        let t = Instant::now();
        let asking = || {
            let mut p = Power::new(&QuickConfig::default()).unwrap();
            p.key(Key::Up, false, t);
            p.key(Key::Enter, false, t);
            p
        };
        let mut p = asking();
        assert_eq!(p.key(Key::Up, false, t), PowerAction::Redraw, "↑");
        assert_eq!((p.question, p.selected), (None, Some(1)), "moved, cancelled");
        let mut p = asking();
        assert_eq!(p.key(Key::Down, false, t), PowerAction::Redraw, "↓ at the end: no move, but cancelled");
        assert_eq!(p.question, None);
        let mut p = asking();
        assert_eq!(p.key(Key::Char('x'), false, t), PowerAction::Redraw, "any other key");
        assert_eq!(p.question, None);
        let mut p = asking();
        assert_eq!(p.hover(2), PowerAction::Nothing, "hovering its own row keeps it");
        assert!(p.question.is_some());
        assert_eq!(p.hover(0), PowerAction::Redraw, "hovering another row moves the selection");
        assert_eq!(p.question, None);
        let mut p = asking();
        assert_eq!(p.press(1, t + CONFIRM_AFTER), PowerAction::Redraw, "a press on reboot asks reboot's own question");
        assert_eq!(p.question, Some((Session::Reboot, t + CONFIRM_AFTER + CONFIRM)));
        let mut p = asking();
        assert_eq!(p.press(0, t + CONFIRM_AFTER), PowerAction::RunAndClose(Session::Lock), "lock runs at once");
        let mut p = asking();
        assert_eq!(p.due(t + CONFIRM - Duration::from_millis(1)), PowerAction::Nothing);
        assert_eq!(p.due(t + CONFIRM), PowerAction::Redraw, "3 s later the question goes");
        assert_eq!((p.question, p.deadline()), (None, None), "and its timer");
        assert_eq!(texts(&p.render())[2], format!(" {POWER} Power off"), "the row back");
        assert_eq!(p.key(Key::Enter, false, t + CONFIRM), PowerAction::Redraw, "the next Enter asks again, never runs");
    }

    #[test]
    fn the_pointer_selects_and_presses_entries_only() {
        let t = Instant::now();
        let mut p = Power::new(&QuickConfig::default()).unwrap();
        assert_eq!(p.hover(1), PowerAction::Redraw);
        assert_eq!(p.selected, Some(1));
        assert_eq!(p.hover(1), PowerAction::Nothing, "the same row: no repaint");
        assert_eq!(p.hover(3), PowerAction::Nothing, "below the last entry");
        assert_eq!(p.press(3, t), PowerAction::Nothing);
        assert_eq!(p.selected, Some(1));
        assert_eq!(p.press(2, t), PowerAction::Redraw, "a press on power off asks");
        assert_eq!(p.selected, Some(2), "and selects its row");
        assert_eq!(p.press(0, t), PowerAction::RunAndClose(Session::Lock), "a left press activates");
    }
}
