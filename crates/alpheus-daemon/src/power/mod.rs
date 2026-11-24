//! The ⏻ power menu's pure half: its entries, the selection, the keys, and the rows it draws.
//!
//! A `Power` holds the entries (the `[quick]` session commands that are set, in the order Lock,
//! Log out, Reboot, Power off), which one is selected, and the question a reboot or a power off
//! asks before it runs. `key`, `hover`, `press` and `due` change it and answer with a
//! `PowerAction` (nothing to do, redraw, close, or run an entry and close); `render` turns it into
//! the rows. The rule for the question (when a second activation confirms, when the question
//! expires) is `session::activate` and `session::expire`, shared with the quick panel's session
//! buttons. The other half, `app.rs`, opens the popup (`PopupBody::Power`), arms the timer and
//! spawns the commands.
//!
//! Never:
//! - read a clock: the caller passes `now` to every call that needs it;
//! - read a file, start a thread or run a command (a guardrail checks the thread and the command,
//!   none checks the file);
//! - panic (a guardrail checks).
//!
//! See: docs/features/power.md and
//! docs/decisions.md ("The power menu runs the `[quick]` commands and has no setting of its own",
//! "Reboot and power off ask first, and a second activation confirms from 0.4 s to 3 s", "Nothing
//! is selected when the power menu opens", "A held key repeats only to move, in the power menu").

use crate::config::QuickConfig;
use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::session::{self, Activation, Session};
use std::time::Instant;

/// A row's cells, its pad cell at each end included: the longest text, "⏻ Power off? ⏎"
/// (14 cells), and the two pads.
pub const WIDTH: u16 = 16;

/// What a key, the pointer or the timer asks of the wiring (`app.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    /// The menu did not change.
    Nothing,
    /// The menu changed: draw its rows again.
    Redraw,
    /// Escape: close the menu.
    Close,
    /// Spawn this entry's `[quick]` command, then close the menu.
    RunAndClose(Session),
}

/// The open menu's own state: dropped whole with its popup, so nothing of it outlives the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Power {
    /// The entries shown, top to bottom; never empty.
    pub entries: Vec<Session>,
    /// The selected entry's index; None at open, as in a tray menu: ↓ selects the first, ↑ the
    /// last. See docs/decisions.md, "Nothing is selected when the power menu opens".
    pub selected: Option<usize>,
    /// The entry that is asking ("Power off? ⏎" or "Reboot? ⏎") and the instant its question ends.
    pub question: Option<(Session, Instant)>,
}

impl Power {
    /// The menu at open; None when no entry has a command (the button is not shown then).
    pub fn new(config: &QuickConfig) -> Option<Power> {
        let entries = session::configured(config);
        if entries.is_empty() {
            return None;
        }
        Some(Power { entries, selected: None, question: None })
    }

    /// The selection on entry `to`; moving it cancels the question, since the question belongs to
    /// the entry that was selected when it was asked.
    fn select(&mut self, to: usize) -> PowerAction {
        if self.selected == Some(to) {
            return PowerAction::Nothing;
        }
        self.selected = Some(to);
        self.question = None;
        PowerAction::Redraw
    }

    /// ↑ ↓: with none selected yet, the last or the first entry; then one step, clamped, never
    /// wrapping (`list::step`). The last argument says which rows can be selected: every entry can.
    fn step(&mut self, delta: i32) -> PowerAction {
        let len = self.entries.len();
        let to = match self.selected {
            Some(i) => list::step(i, delta, len, |_| true),
            None if delta > 0 => 0,
            None => len.saturating_sub(1),
        };
        self.select(to)
    }

    /// Entry `i` activated (Enter, Space, a left press): `session::activate`'s rule. An index past
    /// the entries does nothing.
    fn activate(&mut self, i: usize, now: Instant) -> PowerAction {
        let Some(&s) = self.entries.get(i) else { return PowerAction::Nothing };
        match session::activate(&mut self.question, s, now) {
            Activation::Run => PowerAction::RunAndClose(s),
            Activation::Ask => PowerAction::Redraw,
            Activation::Wait => PowerAction::Nothing,
        }
    }

    /// A key: ↑ ↓ Tab Shift+Tab (and Ctrl+P, Ctrl+N) move, Enter and Space activate, Escape closes.
    /// Any key but Enter and Space cancels the question. `repeat` is true for the keyboard's
    /// auto-repeat, which only the moves follow (see docs/decisions.md, "A held key repeats only to
    /// move, in the power menu").
    pub fn key(&mut self, k: Key, repeat: bool, now: Instant) -> PowerAction {
        let moves = matches!(k, Key::Up | Key::Down | Key::Tab | Key::BackTab | Key::CtrlP | Key::CtrlN);
        if repeat && !moves {
            return PowerAction::Nothing;
        }
        // A key that is not an activation drops the question; `cancelled` remembers that one was
        // pending, so the row is drawn plain again even when the key itself changes nothing else.
        let mut cancelled = false;
        if !matches!(k, Key::Enter | Key::Char(' ')) {
            cancelled = self.question.take().is_some();
        }
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
        if cancelled && action == PowerAction::Nothing {
            return PowerAction::Redraw;
        }
        action
    }

    /// The pointer over content row `row` (one entry per row): selects it. A row past the entries
    /// does nothing.
    pub fn hover(&mut self, row: u16) -> PowerAction {
        let i = usize::from(row);
        if i >= self.entries.len() {
            return PowerAction::Nothing;
        }
        self.select(i)
    }

    /// A left press on content row `row`: selects that entry and activates it. A row past the
    /// entries does nothing.
    pub fn press(&mut self, row: u16, now: Instant) -> PowerAction {
        let i = usize::from(row);
        if i >= self.entries.len() {
            return PowerAction::Nothing;
        }
        self.select(i);
        self.activate(i, now)
    }

    /// The question's end, if one is pending: the only deadline the menu has, so the timer is
    /// armed only while this gives a time.
    pub fn deadline(&self) -> Option<Instant> {
        // `map` changes what is inside a Some and leaves a None as it is; the closure takes the
        // instant out of the pair (the entry asking, the end).
        self.question.map(|(_, at)| at)
    }

    /// At `now`: the question gone once its 3 s passed (Redraw), else Nothing.
    pub fn due(&mut self, now: Instant) -> PowerAction {
        if session::expire(&mut self.question, now) { PowerAction::Redraw } else { PowerAction::Nothing }
    }

    /// The rows, `WIDTH` cells wide, drawn inside `list::boxed`'s ring as the tray menus are: the
    /// glyph in the accent then the word; the selection on a band; the entry asking, its question
    /// in the warning colour instead, with no band.
    pub fn render(&self) -> Boxed {
        let columns = WIDTH + 2;
        let mut out = vec![Grid::empty(columns)];
        for (i, s) in self.entries.iter().enumerate() {
            let mut g = Grid::empty(columns);
            match self.question {
                // The `if` after the pattern: only the entry that asked shows its question.
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

    /// For the log line "power menu shown (…)": the entries by their `[quick]` key, never the
    /// commands, which may name the user.
    pub fn describe(&self) -> String {
        self.entries.iter().map(|s| s.setting()).collect::<Vec<_>>().join(" ")
    }
}

pub mod app;

#[cfg(test)]
mod tests {
    //! Tests of the power menu's model: the rows for every set of commands, the keys, the question
    //! and its expiry, hover and press. No clock (each test passes its instants), no command run.

    use super::*;
    use crate::session::{CONFIRM, CONFIRM_AFTER, LOCK, LOGOUT, POWER, REBOOT};
    use std::time::Duration;

    /// All four commands set (Log out is empty by default).
    fn all_four_commands_set() -> QuickConfig {
        QuickConfig { logout: vec!["fake-logout".into()], ..QuickConfig::default() }
    }

    /// The text of each row, without its trailing blanks.
    fn row_texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
    }

    /// One row per entry whose command is set, for every combination, in the order Lock, Log out,
    /// Reboot, Power off; with all four empty there is no menu.
    #[test]
    fn the_rows_follow_the_commands_in_order_and_none_means_no_menu() {
        let d = Power::new(&QuickConfig::default()).expect("lock, reboot, power off by default");
        assert_eq!(d.entries, [Session::Lock, Session::Reboot, Session::Poweroff], "logout is empty by default");
        assert_eq!(row_texts(&d.render()), [format!(" {LOCK} Lock"), format!(" {REBOOT} Reboot"), format!(" {POWER} Power off")]);
        assert_eq!(d.describe(), "lock reboot poweroff");
        assert_eq!(
            row_texts(&Power::new(&all_four_commands_set()).unwrap().render())[1],
            format!(" {LOGOUT} Log out"),
            "log out once set, second"
        );
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
            for (row, s) in row_texts(&b).iter().zip(&want) {
                assert_eq!(*row, format!(" {} {}", s.glyph(), s.prompt()), "mask {mask:04b}");
            }
        }
    }

    #[test]
    fn the_keys_move_clamped_from_nothing_selected() {
        let t = Instant::now();
        let mut p = Power::new(&all_four_commands_set()).unwrap();
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
        let mut p = Power::new(&all_four_commands_set()).unwrap();
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
        let mut p = Power::new(&all_four_commands_set()).unwrap();
        p.key(Key::Down, false, t);
        assert_eq!(p.key(Key::Enter, false, t), PowerAction::RunAndClose(Session::Lock));
        p.key(Key::Down, false, t);
        assert_eq!(p.key(Key::Char(' '), false, t), PowerAction::RunAndClose(Session::Logout), "Space too");
    }

    /// Reboot and power off ask; a second activation counts from 0.4 s to 3 s; the question is in
    /// the warning colour; its timer exists only while it is pending.
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
        assert_eq!(row_texts(&b)[2], format!(" {POWER} Power off? ⏎"));
        assert_eq!(&b.rows[2].styles()[1..15], "w".repeat(14), "in warn, glyph to ⏎");
        assert_eq!(p.key(Key::Enter, false, t + Duration::from_millis(100)), PowerAction::Nothing, "a double Enter never confirms");
        assert_eq!(p.press(2, t + Duration::from_millis(200)), PowerAction::Nothing, "nor a double click");
        assert_eq!(p.question, Some((Session::Poweroff, t + CONFIRM)), "the question stays, its 3 s unchanged");
        assert_eq!(p.key(Key::Enter, false, t + CONFIRM_AFTER), PowerAction::RunAndClose(Session::Poweroff), "0.4 s on: it runs");
        let mut p = Power::new(&QuickConfig::default()).unwrap();
        assert_eq!(p.press(1, t), PowerAction::Redraw, "a press on reboot asks");
        assert_eq!(row_texts(&p.render())[1], format!(" {REBOOT} Reboot? ⏎"));
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

    /// Moving the selection, another entry, a key or 3 s passing cancels the question.
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
        assert_eq!(row_texts(&p.render())[2], format!(" {POWER} Power off"), "the row back");
        assert_eq!(p.key(Key::Enter, false, t + CONFIRM), PowerAction::Redraw, "the next Enter asks again, never runs");
    }

    /// Escape with a question pending still closes the menu: cancelling the question only asks for
    /// a redraw when the key would otherwise do nothing.
    #[test]
    fn escape_closes_the_menu_even_while_a_question_is_pending() {
        let t = Instant::now();
        let mut p = Power::new(&QuickConfig::default()).unwrap();
        p.key(Key::Up, false, t);
        p.key(Key::Enter, false, t);
        assert!(p.question.is_some());
        assert_eq!(p.key(Key::Escape, false, t), PowerAction::Close);
        assert_eq!(p.question, None, "and the question is gone with it");
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
