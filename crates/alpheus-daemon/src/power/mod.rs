//! The ⏻ power menu's pure half: its rows, the selection, the keys, and the grid it draws.
//!
//! A `Power` holds the rows (the `[quick]` session commands that are set, in the order Lock,
//! Log out, Reboot, Power off, unless `[power] builtins = false`; then the `[[power.entry]]` rows
//! in file order), which one is selected, and the question a row that asks (Reboot, Power off, a
//! user row with `confirm`) shows before it runs. `key`, `hover`, `press` and `due` change it and
//! answer with a `PowerAction` (nothing to do, redraw, close, or run a row and close); `render`
//! turns it into the grid. The rule for the question (when a second activation confirms, when the
//! question expires) is `session::ask` and `session::expire`, shared with the quick panel's
//! session buttons. The other half, `app.rs`, opens the popup (`PopupBody::Power`), arms the timer
//! and spawns the commands.
//!
//! Never:
//! - read a clock: the caller passes `now` to every call that needs it;
//! - read a file, start a thread or run a command (a guardrail checks the thread and the command,
//!   none checks the file);
//! - panic (a guardrail checks).
//!
//! See: docs/features/power.md and
//! docs/decisions.md ("The power menu runs the `[quick]` commands, and `[power]` adds rows", "A
//! user row of the power menu never sleeps", "Reboot and power off ask first, and a second
//! activation confirms from 0.4 s to 3 s", "Nothing is selected when the power menu opens", "A
//! held key repeats only to move, in the power menu").

use crate::config::BarConfig;
use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::session::{self, Activation, Session};
use std::borrow::Cow;
use std::time::Instant;

/// A row's cells, its pad cell at each end included: the longest built-in text, "⏻ Power off? ⏎"
/// (14 cells), and the two pads. A longer user row is cut at it.
pub const WIDTH: u16 = 16;

/// The glyph of a user row without an `icon`.
pub const USER_GLYPH: char = '•';

/// What a row runs: a fixed session entry (its `[quick]` command) or a user row (its
/// `[[power.entry]]`, by index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEntry {
    Session(Session),
    /// The index into `PowerConfig::entry`.
    User(usize),
}

impl PowerEntry {
    /// Its command: the `[quick]` one for a fixed row, the `run` of its `[[power.entry]]` for a
    /// user row (empty past the entries, so nothing runs).
    pub fn command(self, c: &BarConfig) -> &[String] {
        match self {
            PowerEntry::Session(s) => s.command(&c.quick),
            PowerEntry::User(i) => c.power.entry.get(i).map_or(&[], |e| e.run.as_slice()),
        }
    }

    /// In the log: "power off" for a fixed row, "entry 2" (its position, from 1) for a user row,
    /// whose name may name the user.
    pub fn word(self) -> Cow<'static, str> {
        match self {
            PowerEntry::Session(s) => Cow::Borrowed(s.word()),
            PowerEntry::User(i) => Cow::Owned(format!("entry {}", i + 1)),
        }
    }

    /// In the "power menu shown" line: a fixed row by its `[quick]` key, a user row as "entry N".
    fn setting(self) -> Cow<'static, str> {
        match self {
            PowerEntry::Session(s) => Cow::Borrowed(s.setting()),
            PowerEntry::User(_) => self.word(),
        }
    }
}

/// One row of the menu, as built from the config at open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerRow {
    pub entry: PowerEntry,
    pub glyph: char,
    pub label: String,
    /// Asks first (Reboot, Power off, a user row with `confirm`).
    pub asks: bool,
    /// The seconds counted before it runs; 0: none. Stored, not used yet.
    pub countdown: u8,
}

impl PowerRow {
    /// The question while it waits for its second activation, drawn in warn: "󰜉 Reboot? ⏎".
    fn question(&self) -> String {
        format!("{} {}? ⏎", self.glyph, self.label)
    }
}

/// The rows of the menu: the fixed ones whose `[quick]` command is set (unless `builtins` is
/// false), then the user rows in file order.
pub fn rows(c: &BarConfig) -> Vec<PowerRow> {
    let p = &c.power;
    let fixed = if p.builtins { session::configured(&c.quick) } else { Vec::new() };
    let fixed = fixed.into_iter().map(|s| PowerRow {
        entry: PowerEntry::Session(s),
        glyph: s.glyph(),
        label: s.prompt().to_string(),
        asks: s.asks(),
        countdown: if s.asks() { p.countdown } else { 0 },
    });
    let user = p.entry.iter().enumerate().map(|(i, e)| PowerRow {
        entry: PowerEntry::User(i),
        glyph: e.icon.as_deref().and_then(|s| s.chars().next()).unwrap_or(USER_GLYPH),
        label: e.name.clone(),
        asks: e.confirm,
        countdown: e.countdown.unwrap_or(if e.confirm { p.countdown } else { 0 }),
    });
    fixed.chain(user).collect()
}

/// Whether the menu has any row: the ⏻ button, `--power` and the Tools built-in exist only then.
/// `!rows(c).is_empty()` without building the list (the bar asks at every layout).
pub fn any_rows(c: &BarConfig) -> bool {
    (c.power.builtins && session::any_configured(&c.quick)) || !c.power.entry.is_empty()
}

/// What a key, the pointer or the timer asks of the wiring (`app.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    /// The menu did not change.
    Nothing,
    /// The menu changed: draw its rows again.
    Redraw,
    /// Escape: close the menu.
    Close,
    /// Spawn this row's command, then close the menu.
    RunAndClose(PowerEntry),
}

/// The open menu's own state: dropped whole with its popup, so nothing of it outlives the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Power {
    /// The rows shown, top to bottom; never empty.
    pub rows: Vec<PowerRow>,
    /// The selected row's index; None at open, as in a tray menu: ↓ selects the first, ↑ the
    /// last. See docs/decisions.md, "Nothing is selected when the power menu opens".
    pub selected: Option<usize>,
    /// The row that is asking ("Power off? ⏎") and the instant its question ends.
    pub question: Option<(PowerEntry, Instant)>,
}

impl Power {
    /// The menu at open; None when there is no row (the button is not shown then).
    pub fn new(c: &BarConfig) -> Option<Power> {
        let rows = rows(c);
        if rows.is_empty() {
            return None;
        }
        Some(Power { rows, selected: None, question: None })
    }

    /// The selection on row `to`; moving it cancels the question, since the question belongs to
    /// the row that was selected when it was asked.
    fn select(&mut self, to: usize) -> PowerAction {
        if self.selected == Some(to) {
            return PowerAction::Nothing;
        }
        self.selected = Some(to);
        self.question = None;
        PowerAction::Redraw
    }

    /// ↑ ↓: with none selected yet, the last or the first row; then one step, clamped, never
    /// wrapping (`list::step`). The last argument says which rows can be selected: every row can.
    fn step(&mut self, delta: i32) -> PowerAction {
        let len = self.rows.len();
        let to = match self.selected {
            Some(i) => list::step(i, delta, len, |_| true),
            None if delta > 0 => 0,
            None => len.saturating_sub(1),
        };
        self.select(to)
    }

    /// Row `i` activated (Enter, Space, a left press): a row that does not ask runs at once; one
    /// that asks follows `session::ask` (asked, then confirmed by a second activation 0.4 s to
    /// 3 s later). An index past the rows does nothing.
    fn activate(&mut self, i: usize, now: Instant) -> PowerAction {
        let Some(row) = self.rows.get(i) else { return PowerAction::Nothing };
        let (entry, asks) = (row.entry, row.asks);
        if !asks {
            self.question = None;
            return PowerAction::RunAndClose(entry);
        }
        match session::ask(&mut self.question, entry, now) {
            Activation::Run => PowerAction::RunAndClose(entry),
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

    /// The pointer over content row `row` (one menu row per grid row): selects it. A row past the
    /// rows does nothing.
    pub fn hover(&mut self, row: u16) -> PowerAction {
        let i = usize::from(row);
        if i >= self.rows.len() {
            return PowerAction::Nothing;
        }
        self.select(i)
    }

    /// A left press on content row `row`: selects that row and activates it. A row past the rows
    /// does nothing.
    pub fn press(&mut self, row: u16, now: Instant) -> PowerAction {
        let i = usize::from(row);
        if i >= self.rows.len() {
            return PowerAction::Nothing;
        }
        self.select(i);
        self.activate(i, now)
    }

    /// The question's end, if one is pending: the only deadline the menu has, so the timer is
    /// armed only while this gives a time.
    pub fn deadline(&self) -> Option<Instant> {
        // `map` changes what is inside a Some and leaves a None as it is; the closure takes the
        // instant out of the pair (the row asking, the end).
        self.question.map(|(_, at)| at)
    }

    /// At `now`: the question gone once its 3 s passed (Redraw), else Nothing.
    pub fn due(&mut self, now: Instant) -> PowerAction {
        if session::expire(&mut self.question, now) { PowerAction::Redraw } else { PowerAction::Nothing }
    }

    /// The rows, `WIDTH` cells wide, drawn inside `list::boxed`'s ring as the tray menus are: the
    /// glyph in the accent then the label; the selection on a band; the row asking, its question
    /// in the warning colour instead, with no band.
    pub fn render(&self) -> Boxed {
        let columns = WIDTH + 2;
        let mut out = vec![Grid::empty(columns)];
        for (i, r) in self.rows.iter().enumerate() {
            let mut g = Grid::empty(columns);
            match self.question {
                // The `if` after the pattern: only the row that asked shows its question.
                Some((asking, _)) if asking == r.entry => {
                    g.write(2, columns - 1, &r.question(), Style::WARN);
                }
                _ => {
                    let row = Row {
                        icon: Some(r.glyph),
                        icon_style: Style::ACCENT,
                        text: &r.label,
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

    /// For the log line "power menu shown (…)": the fixed rows by their `[quick]` key, the user
    /// rows as "entry N", never a name or a command, which may name the user.
    pub fn describe(&self) -> String {
        self.rows.iter().map(|r| r.entry.setting()).collect::<Vec<_>>().join(" ")
    }
}

pub mod app;

#[cfg(test)]
mod tests {
    //! Tests of the power menu's model: the rows for every set of commands, the keys, the question
    //! and its expiry, hover and press. No clock (each test passes its instants), no command run.

    use super::*;
    use crate::config::{PowerEntryConfig, QuickConfig};
    use crate::session::{CONFIRM, CONFIRM_AFTER, LOCK, LOGOUT, POWER, REBOOT};
    use std::time::Duration;

    /// All four commands set (Log out is empty by default).
    fn all_four_commands_set() -> BarConfig {
        let mut c = BarConfig::default();
        c.quick.logout = vec!["fake-logout".into()];
        c
    }

    /// The text of each row, without its trailing blanks.
    fn row_texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
    }

    /// The rows' entries, top to bottom.
    fn entries(p: &Power) -> Vec<PowerEntry> {
        p.rows.iter().map(|r| r.entry).collect()
    }

    /// A user row named `name` running `fake-<name>`, asking first when `confirm`.
    fn user(name: &str, confirm: bool) -> PowerEntryConfig {
        PowerEntryConfig { name: name.into(), icon: None, run: vec![format!("fake-{name}")], confirm, countdown: None }
    }

    /// The default `[quick]` rows and the user rows `entry`.
    fn with_rows(entry: Vec<PowerEntryConfig>) -> BarConfig {
        let mut c = BarConfig::default();
        c.power.entry = entry;
        c
    }

    /// No `[power]`: the same rows as before it existed, built from `session::configured`.
    #[test]
    fn power_defaults_change_nothing() {
        let c = BarConfig::default();
        let p = Power::new(&c).unwrap();
        let want: Vec<PowerEntry> = session::configured(&c.quick).into_iter().map(PowerEntry::Session).collect();
        assert_eq!(entries(&p), want);
        for (r, s) in p.rows.iter().zip(session::configured(&c.quick)) {
            assert_eq!((r.glyph, r.label.as_str(), r.asks, r.countdown), (s.glyph(), s.prompt(), s.asks(), 0));
        }
        assert_eq!(p.describe(), "lock reboot poweroff");
    }

    /// `builtins = false` hides the four fixed rows, whatever `[quick]` sets; with no user row
    /// there is no menu.
    #[test]
    fn builtins_false_hides_the_four_rows() {
        let mut c = all_four_commands_set();
        c.power.builtins = false;
        assert_eq!(Power::new(&c), None, "no row: no menu");
        assert!(!any_rows(&c));
        c.power.entry = vec![user("quit", false)];
        let p = Power::new(&c).unwrap();
        assert_eq!(entries(&p), [PowerEntry::User(0)]);
        assert!(any_rows(&c));
        assert_eq!(p.describe(), "entry 1");
    }

    /// The user rows come after the fixed ones, in file order, with their glyph (`•` without an
    /// icon) and label; the question shows on the row that asks.
    #[test]
    fn user_rows_follow_the_fixed_ones_in_file_order() {
        let mut second = user("Hold", true);
        second.icon = Some("x".into());
        let c = with_rows(vec![user("Quit", false), second]);
        let p = Power::new(&c).unwrap();
        assert_eq!(
            entries(&p),
            [
                PowerEntry::Session(Session::Lock),
                PowerEntry::Session(Session::Reboot),
                PowerEntry::Session(Session::Poweroff),
                PowerEntry::User(0),
                PowerEntry::User(1)
            ]
        );
        assert_eq!(row_texts(&p.render())[3..], [format!(" {USER_GLYPH} Quit"), " x Hold".to_string()]);
        assert_eq!(p.describe(), "lock reboot poweroff entry 1 entry 2", "never a name");
        assert_eq!((p.rows[3].asks, p.rows[4].asks), (false, true));
        assert_eq!(PowerEntry::User(1).command(&c), ["fake-Hold"]);
        assert_eq!(PowerEntry::User(2).command(&c), [] as [String; 0], "past the entries: nothing to run");
        assert_eq!(PowerEntry::Session(Session::Lock).command(&c), c.quick.lock.as_slice());
        assert_eq!((PowerEntry::User(1).word(), PowerEntry::Session(Session::Poweroff).word()), ("entry 2".into(), "power off".into()));
    }

    /// The global countdown goes to the rows that ask; a row's own overrides it, asking or not.
    #[test]
    fn countdowns_are_stored_per_row() {
        let mut own = user("Own", false);
        own.countdown = Some(7);
        let mut c = with_rows(vec![user("Plain", false), user("Asks", true), own]);
        c.power.countdown = 5;
        let got: Vec<u8> = rows(&c).iter().map(|r| r.countdown).collect();
        assert_eq!(got, [0, 5, 5, 0, 5, 7], "lock, reboot, power off, plain, asks, own");
    }

    /// A user row with `confirm` asks, ignores a second activation inside 0.4 s, and runs on one
    /// from 0.4 s to 3 s later; one without runs at once.
    #[test]
    fn a_confirm_user_row_asks_then_confirms_0_4_to_3_s_later() {
        let t = Instant::now();
        let c = with_rows(vec![user("Quit", false), user("Hold", true)]);
        let mut p = Power::new(&c).unwrap();
        assert_eq!(p.press(4, t), PowerAction::Redraw, "the first press asks");
        assert_eq!(p.question, Some((PowerEntry::User(1), t + CONFIRM)));
        assert_eq!(row_texts(&p.render())[4], format!(" {USER_GLYPH} Hold? ⏎"));
        assert_eq!(p.press(4, t + Duration::from_millis(100)), PowerAction::Nothing, "a double click never confirms");
        assert_eq!(p.key(Key::Enter, false, t + CONFIRM_AFTER), PowerAction::RunAndClose(PowerEntry::User(1)), "0.4 s on: it runs");
        let mut p = Power::new(&c).unwrap();
        assert_eq!(p.press(4, t), PowerAction::Redraw);
        assert_eq!(p.due(t + CONFIRM), PowerAction::Redraw, "3 s later the question goes");
        assert_eq!(p.press(4, t + CONFIRM), PowerAction::Redraw, "and the next press asks again");
        let mut p = Power::new(&c).unwrap();
        assert_eq!(p.press(3, t), PowerAction::RunAndClose(PowerEntry::User(0)), "a plain row runs at once");
        // Another row's question never confirms this one.
        let mut p = Power::new(&c).unwrap();
        p.press(2, t);
        assert_eq!(p.press(4, t + CONFIRM_AFTER), PowerAction::Redraw, "power off's question is not this row's");
    }

    /// One row per entry whose command is set, for every combination, in the order Lock, Log out,
    /// Reboot, Power off; with all four empty there is no menu.
    #[test]
    fn the_rows_follow_the_commands_in_order_and_none_means_no_menu() {
        let d = Power::new(&BarConfig::default()).expect("lock, reboot, power off by default");
        assert_eq!(entries(&d), [Session::Lock, Session::Reboot, Session::Poweroff].map(PowerEntry::Session), "logout is empty by default");
        assert_eq!(row_texts(&d.render()), [format!(" {LOCK} Lock"), format!(" {REBOOT} Reboot"), format!(" {POWER} Power off")]);
        assert_eq!(d.describe(), "lock reboot poweroff");
        assert_eq!(
            row_texts(&Power::new(&all_four_commands_set()).unwrap().render())[1],
            format!(" {LOGOUT} Log out"),
            "log out once set, second"
        );
        for mask in 0..16u8 {
            let set = |bit: u8| if mask & bit != 0 { vec!["fake".to_string()] } else { Vec::new() };
            let quick = QuickConfig { lock: set(1), logout: set(2), reboot: set(4), poweroff: set(8), ..QuickConfig::default() };
            let c = BarConfig { quick, ..BarConfig::default() };
            let want: Vec<Session> = Session::ALL.into_iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, s)| s).collect();
            let Some(p) = Power::new(&c) else {
                assert_eq!(mask, 0, "only all four empty means no menu");
                continue;
            };
            assert_eq!(entries(&p), want.iter().copied().map(PowerEntry::Session).collect::<Vec<_>>(), "mask {mask:04b}");
            assert_eq!(any_rows(&c), mask != 0, "mask {mask:04b}");
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
        assert_eq!(p.key(Key::Enter, false, t), PowerAction::RunAndClose(PowerEntry::Session(Session::Lock)));
        p.key(Key::Down, false, t);
        assert_eq!(p.key(Key::Char(' '), false, t), PowerAction::RunAndClose(PowerEntry::Session(Session::Logout)), "Space too");
    }

    /// Reboot and power off ask; a second activation counts from 0.4 s to 3 s; the question is in
    /// the warning colour; its timer exists only while it is pending.
    #[test]
    fn power_off_asks_and_a_second_activation_confirms_it() {
        let t = Instant::now();
        let mut p = Power::new(&BarConfig::default()).unwrap();
        p.key(Key::Up, false, t);
        assert_eq!(p.deadline(), None, "no question: no timer");
        assert_eq!(p.key(Key::Enter, false, t), PowerAction::Redraw, "the first activation asks");
        assert_eq!(p.question, Some((PowerEntry::Session(Session::Poweroff), t + CONFIRM)));
        assert_eq!(p.deadline(), Some(t + CONFIRM), "the timer waits for it");
        let b = p.render();
        assert_eq!(row_texts(&b)[2], format!(" {POWER} Power off? ⏎"));
        assert_eq!(&b.rows[2].styles()[1..15], "w".repeat(14), "in warn, glyph to ⏎");
        assert_eq!(p.key(Key::Enter, false, t + Duration::from_millis(100)), PowerAction::Nothing, "a double Enter never confirms");
        assert_eq!(p.press(2, t + Duration::from_millis(200)), PowerAction::Nothing, "nor a double click");
        assert_eq!(p.question, Some((PowerEntry::Session(Session::Poweroff), t + CONFIRM)), "the question stays, its 3 s unchanged");
        assert_eq!(
            p.key(Key::Enter, false, t + CONFIRM_AFTER),
            PowerAction::RunAndClose(PowerEntry::Session(Session::Poweroff)),
            "0.4 s on: it runs"
        );
        let mut p = Power::new(&BarConfig::default()).unwrap();
        assert_eq!(p.press(1, t), PowerAction::Redraw, "a press on reboot asks");
        assert_eq!(row_texts(&p.render())[1], format!(" {REBOOT} Reboot? ⏎"));
        assert_eq!(
            p.press(1, t + Duration::from_secs(2)),
            PowerAction::RunAndClose(PowerEntry::Session(Session::Reboot)),
            "a second press inside 3 s runs it"
        );
    }

    #[test]
    fn a_held_enter_acts_once() {
        let t = Instant::now();
        let mut p = Power::new(&BarConfig::default()).unwrap();
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
            let mut p = Power::new(&BarConfig::default()).unwrap();
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
        assert_eq!(p.question, Some((PowerEntry::Session(Session::Reboot), t + CONFIRM_AFTER + CONFIRM)));
        let mut p = asking();
        assert_eq!(p.press(0, t + CONFIRM_AFTER), PowerAction::RunAndClose(PowerEntry::Session(Session::Lock)), "lock runs at once");
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
        let mut p = Power::new(&BarConfig::default()).unwrap();
        p.key(Key::Up, false, t);
        p.key(Key::Enter, false, t);
        assert!(p.question.is_some());
        assert_eq!(p.key(Key::Escape, false, t), PowerAction::Close);
        assert_eq!(p.question, None, "and the question is gone with it");
    }

    #[test]
    fn the_pointer_selects_and_presses_entries_only() {
        let t = Instant::now();
        let mut p = Power::new(&BarConfig::default()).unwrap();
        assert_eq!(p.hover(1), PowerAction::Redraw);
        assert_eq!(p.selected, Some(1));
        assert_eq!(p.hover(1), PowerAction::Nothing, "the same row: no repaint");
        assert_eq!(p.hover(3), PowerAction::Nothing, "below the last entry");
        assert_eq!(p.press(3, t), PowerAction::Nothing);
        assert_eq!(p.selected, Some(1));
        assert_eq!(p.press(2, t), PowerAction::Redraw, "a press on power off asks");
        assert_eq!(p.selected, Some(2), "and selects its row");
        assert_eq!(p.press(0, t), PowerAction::RunAndClose(PowerEntry::Session(Session::Lock)), "a left press activates");
    }
}
