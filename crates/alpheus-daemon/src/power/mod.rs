//! The ⏻ power menu's pure half: its rows, the selection, the keys, and the grid it draws.
//!
//! A `Power` holds the rows (the `[quick]` session commands that are set, in the order Lock,
//! Log out, Reboot, Power off, unless `[power] builtins = false`; then the `[[power.entry]]` rows
//! in file order), which one is selected, and the question a row that asks (Reboot, Power off, a
//! user row with `confirm`) shows before it runs, and the countdown a row with one shows before it
//! runs (`Countdown`). `key`, `hover`, `press` and `due` change it and answer with a `PowerAction`
//! (nothing to do, redraw, close, or run a row and close); `render` turns it into the grid. The
//! rule for the question (when a second activation confirms, when the question expires) is
//! `session::ask` and `session::expire`, shared with the quick panel's session buttons. The other
//! half, `app.rs`, opens the popup (`PopupBody::Power`), arms the timer and spawns the commands.
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
use crate::grid::{Grid, Style, Tint, text_width};
use crate::keys::{Key, Mods};
use crate::list::{self, Boxed, Row, Shape, ShapeKind};
use crate::session::{self, Activation, Session};
use std::borrow::Cow;
use std::time::{Duration, Instant};

/// The rows that carry a digit capsule: 1 to 9, the keys that pick them.
pub const DIGITS: usize = 9;

/// evdev's `KEY_1` and `KEY_9` (linux/input-event-codes.h): the top row's digit keys, by position.
const KEY_1: u32 = 2;
const KEY_9: u32 = 10;

/// The digit a top-row key stands for by its position (evdev `KEY_1` to `KEY_9`, the event's
/// `raw_code`), whatever character the layout types there, so AZERTY's `&` key is 1 without Shift.
/// None for any other key, and with Ctrl, Alt or Super held (as on the typed path, which reads no
/// character then); Shift does not matter. The keypad keeps the typed path (its digits with
/// NumLock). Only the power menu reads keys this way. See docs/decisions.md, "A digit picks a power
/// row, and the same digit confirms".
pub fn position_digit(raw_code: u32, mods: Mods) -> Option<Key> {
    if mods.ctrl || mods.alt || mods.logo || !(KEY_1..=KEY_9).contains(&raw_code) {
        return None;
    }
    // `from_digit` gives Some for 1 to 9 (`raw_code - 1`), which the range above ensures.
    char::from_digit(raw_code - 1, 10).map(Key::Char)
}

/// What a digit adds to a row's cells, past its text's trailing blank: a gap outside the
/// selection band, then the capsule's three cells (blank, digit, blank).
const CAPSULE: u16 = 4;

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
    /// The seconds counted before it runs (`Countdown`); 0: it runs at once.
    pub countdown: u8,
}

impl PowerRow {
    /// The question while it waits for its second activation, drawn in warn: "󰜉 Reboot? ⏎".
    fn question(&self) -> String {
        format!("{} {}? ⏎", self.glyph, self.label)
    }

    /// Its countdown's line with `left` seconds to go, drawn in warn: "⏻ Power off in 5 s · Esc".
    fn countdown_line(&self, left: u8) -> String {
        format!("{} {} in {} s · Esc", self.glyph, self.label, left)
    }

    /// The cells its longest text takes: the glyph, a space and the label; its question when it
    /// asks; its countdown's line ("⏻ Power off in 30 s · Esc") when it has one.
    fn text_cells(&self) -> u16 {
        let mut cells = text_width(&format!("{} {}", self.glyph, self.label));
        if self.asks {
            cells = cells.max(text_width(&self.question()));
        }
        if self.countdown > 0 {
            cells = cells.max(text_width(&self.countdown_line(self.countdown)));
        }
        cells
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
    /// The row counting down before it runs ("⏻ Power off in 5 s · Esc"), if one is.
    pub counting: Option<Countdown>,
}

/// A row counting down before it runs: armed by its confirming activation (by its activation for a
/// row that does not ask), it runs at `ends`; `left` is the whole seconds drawn. It lives in
/// `Power`, so it goes with the menu. See docs/decisions.md, "The power menu's countdown lives and
/// dies with the menu".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Countdown {
    pub entry: PowerEntry,
    pub ends: Instant,
    pub left: u8,
}

impl Power {
    /// The menu at open; None when there is no row (the button is not shown then).
    pub fn new(c: &BarConfig) -> Option<Power> {
        let rows = rows(c);
        if rows.is_empty() {
            return None;
        }
        Some(Power { rows, selected: None, question: None, counting: None })
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
    /// 3 s later). A row with a countdown starts it where it would run (`run`). An index past the
    /// rows does nothing.
    fn activate(&mut self, i: usize, now: Instant) -> PowerAction {
        let Some(row) = self.rows.get(i) else { return PowerAction::Nothing };
        let (entry, asks, secs) = (row.entry, row.asks, row.countdown);
        if !asks {
            self.question = None;
            return self.run(entry, secs, now);
        }
        match session::ask(&mut self.question, entry, now) {
            Activation::Run => self.run(entry, secs, now),
            Activation::Ask => PowerAction::Redraw,
            Activation::Wait => PowerAction::Nothing,
        }
    }

    /// `entry` is to run: at once without a countdown (`secs` 0); else its countdown starts, the
    /// question gone, and only `due` runs it, at its end.
    fn run(&mut self, entry: PowerEntry, secs: u8, now: Instant) -> PowerAction {
        if secs == 0 {
            return PowerAction::RunAndClose(entry);
        }
        self.question = None;
        self.counting = Some(Countdown { entry, ends: now + Duration::from_secs(u64::from(secs)), left: secs });
        PowerAction::Redraw
    }

    /// A key: ↑ ↓ Tab Shift+Tab (and Ctrl+P, Ctrl+N) move, Enter and Space activate, Escape closes,
    /// a digit 1 to 9 selects its row and activates it (the same digit again confirms a row that
    /// asks). Any other key cancels the question. During a countdown, Escape and every other key
    /// but Enter and Space cancel it and do nothing else. `repeat` is true for the keyboard's
    /// auto-repeat, which only the moves follow (see docs/decisions.md, "A held key repeats only to
    /// move, in the power menu", "A digit picks a power row, and the same digit confirms", "The
    /// power menu's countdown lives and dies with the menu").
    pub fn key(&mut self, k: Key, repeat: bool, now: Instant) -> PowerAction {
        let moves = matches!(k, Key::Up | Key::Down | Key::Tab | Key::BackTab | Key::CtrlP | Key::CtrlN);
        if repeat && !moves {
            return PowerAction::Nothing;
        }
        // A countdown takes every key: Enter and Space do nothing (the key that confirmed must not
        // cut it short), any other cancels it, and nothing more; a second Escape then closes.
        if self.counting.is_some() {
            if matches!(k, Key::Enter | Key::Char(' ')) {
                return PowerAction::Nothing;
            }
            self.counting = None;
            return PowerAction::Redraw;
        }
        // A digit on a row: that row's own activation. Moving onto it drops another row's question
        // (`select`); the same row's question stays for its confirming press. Past the rows, a
        // digit is any other key.
        if let Key::Char(c @ '1'..='9') = k {
            let i = usize::from(c as u8 - b'1');
            if i < self.rows.len() {
                let moved = self.select(i);
                return match self.activate(i, now) {
                    PowerAction::Nothing => moved,
                    action => action,
                };
            }
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

    /// The pointer over content row `row` (one menu row per grid row): selects it; on another row
    /// than a countdown's, that cancels the countdown. A row past the rows does nothing.
    pub fn hover(&mut self, row: u16) -> PowerAction {
        let i = usize::from(row);
        if i >= self.rows.len() {
            return PowerAction::Nothing;
        }
        if let Some(action) = self.pointer_on_countdown(i) {
            return action;
        }
        self.select(i)
    }

    /// A left press on content row `row`: selects that row and activates it. During a countdown it
    /// only cancels it (on another row, which it selects): it never runs a second row. A row past
    /// the rows does nothing.
    pub fn press(&mut self, row: u16, now: Instant) -> PowerAction {
        let i = usize::from(row);
        if i >= self.rows.len() {
            return PowerAction::Nothing;
        }
        if let Some(action) = self.pointer_on_countdown(i) {
            return action;
        }
        self.select(i);
        self.activate(i, now)
    }

    /// During a countdown, the pointer on row `i`: on the counting row, Nothing; on another, the
    /// countdown cancelled and row `i` selected (Redraw). None when nothing counts.
    fn pointer_on_countdown(&mut self, i: usize) -> Option<PowerAction> {
        let c = self.counting?;
        if self.rows.get(i).is_some_and(|r| r.entry == c.entry) {
            return Some(PowerAction::Nothing);
        }
        self.counting = None;
        self.select(i);
        Some(PowerAction::Redraw)
    }

    /// The menu's next deadline: the earlier of the question's end and the countdown's next whole
    /// second (its end, at 1 s left). The timer is armed only while this gives a time, so a menu
    /// with neither has none, and a countdown wakes it once a second.
    pub fn deadline(&self) -> Option<Instant> {
        // `map` changes what is inside a Some and leaves a None as it is; the closure takes the
        // instant out of the pair (the row asking, the end).
        let question = self.question.map(|(_, at)| at);
        // With `left` seconds drawn, the next change is `left - 1` seconds before the end.
        let tick = self.counting.map(|c| c.ends.checked_sub(Duration::from_secs(u64::from(c.left.saturating_sub(1)))).unwrap_or(c.ends));
        match (question, tick) {
            (Some(q), Some(t)) => Some(q.min(t)),
            (q, t) => q.or(t),
        }
    }

    /// At `now`: the question gone once its 3 s passed (Redraw); the countdown's row run at its end
    /// (RunAndClose, the countdown gone), or redrawn when its whole seconds left changed; else
    /// Nothing.
    pub fn due(&mut self, now: Instant) -> PowerAction {
        let mut action = if session::expire(&mut self.question, now) { PowerAction::Redraw } else { PowerAction::Nothing };
        if let Some(c) = self.counting.as_mut() {
            if now >= c.ends {
                let entry = c.entry;
                self.counting = None;
                return PowerAction::RunAndClose(entry);
            }
            // The seconds left, rounded up: 5 at the start, 4 once a whole second has passed.
            let rest = c.ends.saturating_duration_since(now);
            let left = u8::try_from(rest.as_secs() + u64::from(rest.subsec_nanos() > 0)).unwrap_or(u8::MAX);
            if left != c.left {
                c.left = left;
                action = PowerAction::Redraw;
            }
        }
        action
    }

    /// The menu's cells across, the same in every state so the popup keeps its size: the longest
    /// row's text (`PowerRow::text_cells`: its label, question or countdown line) after a pad
    /// cell and before a blank (both on the selection band), then the digit's gap and capsule (the
    /// first row always has one) and the pad at the right edge.
    pub fn width(&self) -> u16 {
        let text = self.rows.iter().map(PowerRow::text_cells).max().unwrap_or(0);
        1 + text + 1 + CAPSULE + 1
    }

    /// The rows, `width()` cells wide, drawn inside `list::boxed`'s ring as the tray menus are: the
    /// glyph in the accent then the label; the selection on a band; the row asking, its question
    /// in the warning colour instead, with no band, as the row counting down its countdown line.
    /// Rows 1 to 9 end with their digit, dim, in a
    /// capsule (`ShapeKind::Chip(Tint::Pill)`, painted under the cells by `paint_boxed`) outside
    /// the band, one pad cell from the edge.
    pub fn render(&self) -> Boxed {
        let columns = self.width() + 2;
        // In the ring's coordinates: the band and the text end before the gap; the capsule's three
        // cells follow the gap; then the pad and the ring's column.
        let band_end = columns.saturating_sub(1 + CAPSULE + 1);
        let capsule_at = band_end + 1;
        let mut out = vec![Grid::empty(columns)];
        let mut shapes = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            let mut g = Grid::empty(columns);
            match (self.counting, self.question) {
                // The `if` after the pattern: only the row counting shows its line.
                (Some(c), _) if c.entry == r.entry => {
                    g.write(2, band_end, &r.countdown_line(c.left), Style::WARN);
                }
                // Only the row that asked shows its question.
                (_, Some((asking, _))) if asking == r.entry => {
                    g.write(2, band_end, &r.question(), Style::WARN);
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
                    list::row(&mut g, 1, band_end, &row);
                }
            }
            // `char::from_digit` gives Some for 1 to 9, the rows that have a digit.
            if let Some(d) = char::from_digit(i as u32 + 1, 10).filter(|_| i < DIGITS) {
                g.write(capsule_at + 1, capsule_at + 2, &d.to_string(), Style::MUTED);
                // `boxed` drops the ring's top row and left column: the shape is in its coordinates.
                shapes.push(Shape { row: i, col: capsule_at - 1, width: 3, rows: 1, kind: ShapeKind::Chip(Tint::Pill), glyph: None });
            }
            out.push(g);
        }
        out.push(Grid::empty(columns));
        let mut b = list::boxed(&out, &[], None);
        b.shapes = shapes;
        b
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

    /// The text of each row, without its trailing blanks nor its capsule's digit (no label of
    /// these tests ends with a digit).
    fn row_texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().trim_end_matches(|c: char| c.is_ascii_digit()).trim_end().to_string()).collect()
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
            assert_eq!(b.cells(), (p.width(), want.len() as u16), "mask {mask:04b}");
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
        let w = usize::from(p.width());
        assert_eq!(b.rows[3].styles()[..w - 5], "S".repeat(w - 5), "the selection band across the row up to its capsule");
        assert_eq!(&b.rows[3].styles()[w - 5..], "..m..", "the gap, the capsule and the pad stay off the band (the digit muted)");
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

    /// A digit key, pressed (not held).
    fn digit(p: &mut Power, d: char, at: Instant) -> PowerAction {
        p.key(Key::Char(d), false, at)
    }

    /// The top row by position: evdev 2 to 10 are 1 to 9 whatever the layout types (AZERTY's `&`,
    /// Shift or not); 11 (`0`), 1 (Escape), the keypad's codes and a held Ctrl, Alt or Super are not.
    #[test]
    fn the_top_rows_keys_are_digits_by_position() {
        let none = Mods::default();
        for code in 2..=10u32 {
            assert_eq!(position_digit(code, none), char::from_digit(code - 1, 10).map(Key::Char), "code {code}");
        }
        let shift = Mods { shift: true, ..none };
        assert_eq!(position_digit(2, shift), Some(Key::Char('1')), "Shift does not matter");
        for code in [0, 1, 11, 12, 16, 79, 80, 81] {
            assert_eq!(position_digit(code, none), None, "code {code}");
        }
        for mods in [Mods { ctrl: true, ..none }, Mods { alt: true, ..none }, Mods { logo: true, ..none }] {
            assert_eq!(position_digit(2, mods), None, "{mods:?}");
        }
    }

    /// `1` on Lock, which does not ask: it runs at once, from nothing selected.
    #[test]
    fn a_digit_runs_a_row_that_does_not_ask() {
        let t = Instant::now();
        let mut p = Power::new(&BarConfig::default()).unwrap();
        assert_eq!(digit(&mut p, '1', t), PowerAction::RunAndClose(PowerEntry::Session(Session::Lock)));
        let mut p = Power::new(&with_rows(vec![user("Quit", false)])).unwrap();
        assert_eq!(digit(&mut p, '4', t), PowerAction::RunAndClose(PowerEntry::User(0)), "a user row by its digit");
    }

    /// `3` on Reboot asks and selects it; the same digit 0.1 s later does nothing, 0.4 s later runs
    /// it; another digit asks its own row's question, never confirms the first.
    #[test]
    fn a_digit_on_a_row_that_asks_needs_the_same_digit_again() {
        let t = Instant::now();
        let reboot = PowerEntry::Session(Session::Reboot);
        let mut p = Power::new(&all_four_commands_set()).unwrap();
        assert_eq!(digit(&mut p, '3', t), PowerAction::Redraw, "the first press asks");
        assert_eq!((p.selected, p.question), (Some(2), Some((reboot, t + CONFIRM))));
        assert_eq!(digit(&mut p, '3', t + Duration::from_millis(100)), PowerAction::Nothing, "a quick second press never confirms");
        assert_eq!(p.question, Some((reboot, t + CONFIRM)), "the question stays, its 3 s unchanged");
        assert_eq!(digit(&mut p, '3', t + CONFIRM_AFTER), PowerAction::RunAndClose(reboot), "0.4 s on: it runs");
        let mut p = Power::new(&all_four_commands_set()).unwrap();
        digit(&mut p, '3', t);
        let later = t + Duration::from_millis(500);
        assert_eq!(digit(&mut p, '4', later), PowerAction::Redraw, "another digit asks its own row");
        assert_eq!((p.selected, p.question), (Some(3), Some((PowerEntry::Session(Session::Poweroff), later + CONFIRM))));
        // Enter confirms the question a digit asked, as for a press.
        assert_eq!(p.key(Key::Enter, false, later + CONFIRM_AFTER), PowerAction::RunAndClose(PowerEntry::Session(Session::Poweroff)));
        // Escape still closes, its question gone with it.
        let mut p = Power::new(&all_four_commands_set()).unwrap();
        digit(&mut p, '4', t);
        assert_eq!(p.key(Key::Escape, false, t), PowerAction::Close);
    }

    /// A digit past the rows, and `0`, do nothing (and drop a pending question, as any other key).
    #[test]
    fn a_digit_past_the_rows_does_nothing() {
        let t = Instant::now();
        let mut p = Power::new(&BarConfig::default()).unwrap();
        for d in ['4', '9', '0'] {
            assert_eq!(digit(&mut p, d, t), PowerAction::Nothing, "{d}");
        }
        assert_eq!((p.selected, p.question), (None, None));
        digit(&mut p, '3', t);
        assert!(p.question.is_some());
        assert_eq!(digit(&mut p, '7', t + CONFIRM_AFTER), PowerAction::Redraw, "past the rows: the question dropped");
        assert_eq!(p.question, None);
    }

    /// A held digit never acts: neither the first activation nor the confirming one.
    #[test]
    fn a_held_digit_never_acts() {
        let t = Instant::now();
        let mut p = Power::new(&BarConfig::default()).unwrap();
        assert_eq!(p.key(Key::Char('1'), true, t), PowerAction::Nothing, "a held 1 never locks");
        assert_eq!(p.key(Key::Char('3'), true, t), PowerAction::Nothing, "nor asks");
        assert_eq!((p.selected, p.question), (None, None));
        digit(&mut p, '3', t);
        assert_eq!(p.key(Key::Char('3'), true, t + CONFIRM_AFTER), PowerAction::Nothing, "the repeat never confirms");
        assert!(p.question.is_some(), "and leaves the question");
    }

    /// Twelve rows (the four fixed, eight of the user's): a capsule with its digit at the right of
    /// rows 1 to 9 only, each on its own row.
    #[test]
    fn rows_past_nine_have_no_capsule() {
        let mut c = all_four_commands_set();
        c.power.entry = (0..8).map(|i| user(&format!("Row {i}"), false)).collect();
        let p = Power::new(&c).unwrap();
        assert_eq!(p.rows.len(), 12);
        let b = p.render();
        assert_eq!(b.shapes.len(), 9, "{:?}", b.shapes);
        for (i, s) in b.shapes.iter().enumerate() {
            assert_eq!((s.row, s.width, s.rows, s.kind), (i, 3, 1, ShapeKind::Chip(Tint::Pill)), "row {i}");
            let digit = b.rows[i].text().chars().nth(usize::from(s.col) + 1);
            assert_eq!(digit, char::from_digit(i as u32 + 1, 10), "row {i}");
            assert_eq!(usize::from(s.col + s.width + 1), usize::from(p.width()), "one pad cell after it, row {i}");
        }
        for (i, row) in b.rows.iter().enumerate().take(12).skip(9) {
            assert_eq!(row.text().trim_end(), format!(" {USER_GLYPH} Row {}", i - 4), "row {i}: its text alone");
        }
        // Selected, a row past nine has its band end where every other row's does.
        let mut p = p;
        p.selected = Some(10);
        let w = usize::from(p.width());
        let styles = p.render().rows[10].styles();
        assert_eq!((&styles[..w - 5], &styles[w - 5..]), ("S".repeat(w - 5).as_str(), "....."), "row 11's band");
    }

    /// The width follows the longest text: a 24-character user name shows whole, its capsule after
    /// it; the default rows are as wide as Power off's question; a row's countdown line counts.
    #[test]
    fn the_width_fits_the_longest_row() {
        let name = "Twenty-four characters!!";
        assert_eq!(name.chars().count(), crate::config::POWER_NAME_MAX);
        let p = Power::new(&with_rows(vec![user(name, false)])).unwrap();
        // The pad, "• " and the name, a blank, a gap, the capsule, the pad.
        assert_eq!(p.width(), 1 + 2 + 24 + 1 + 1 + 3 + 1);
        let b = p.render();
        assert_eq!(b.cells().0, p.width());
        assert!(b.rows[3].text().contains(name), "not cut: {}", b.rows[3].text());
        let d = Power::new(&BarConfig::default()).unwrap();
        assert_eq!(d.width(), 1 + 14 + 1 + 1 + 3 + 1, "\"⏻ Power off? ⏎\" (14 cells) sets it");
        // Asking, the width does not change (the popup keeps its size).
        let mut q = Power::new(&BarConfig::default()).unwrap();
        digit(&mut q, '3', Instant::now());
        assert_eq!(q.render().cells(), d.render().cells());
        // A countdown's line, "⏻ Power off in 30 s · Esc" (25 cells), is longer than the question.
        let mut c = BarConfig::default();
        c.power.countdown = 30;
        assert_eq!(Power::new(&c).unwrap().width(), 1 + 25 + 1 + 1 + 3 + 1);
    }

    /// The menu as drawn: Lock, Log out, Reboot, Power off and one user row; Reboot selected, Power
    /// off asking; the digits in their capsules.
    #[test]
    fn power_menu_with_digits_snapshot() {
        let t = Instant::now();
        let mut c = all_four_commands_set();
        c.power.entry = vec![user("Quit", false)];
        let mut p = Power::new(&c).unwrap();
        digit(&mut p, '4', t);
        p.selected = Some(2);
        let b = p.render();
        let mut out: Vec<String> = b.rows.iter().map(|g| format!("{}\n{}", g.text(), g.styles())).collect();
        out.extend(b.shapes.iter().map(|s| format!("{s:?}")));
        insta::assert_snapshot!("power_menu_with_digits", out.join("\n"));
    }

    /// The default rows with a global countdown of `secs`.
    fn counting(secs: u8) -> BarConfig {
        let mut c = BarConfig::default();
        c.power.countdown = secs;
        c
    }

    const SECOND: Duration = Duration::from_secs(1);

    /// Power off asked by `3`, then confirmed by `3` at `t + CONFIRM_AFTER`: the answer of the
    /// confirming press, and the instant it came.
    fn confirm_power_off(p: &mut Power, t: Instant) -> (PowerAction, Instant) {
        assert_eq!(digit(p, '3', t), PowerAction::Redraw, "asked");
        let at = t + CONFIRM_AFTER;
        (digit(p, '3', at), at)
    }

    /// With `countdown = 5`, Power off's confirming activation starts the countdown instead of
    /// running: a redraw, 5 s left, the question gone, the row drawn as its countdown line in warn.
    #[test]
    fn the_confirming_activation_starts_the_countdown() {
        let t = Instant::now();
        let mut p = Power::new(&counting(5)).unwrap();
        let (action, at) = confirm_power_off(&mut p, t);
        assert_eq!(action, PowerAction::Redraw, "no RunAndClose");
        let poweroff = PowerEntry::Session(Session::Poweroff);
        assert_eq!(p.counting, Some(Countdown { entry: poweroff, ends: at + 5 * SECOND, left: 5 }));
        assert_eq!(p.question, None);
        let b = p.render();
        assert_eq!(row_texts(&b)[2], format!(" {POWER} Power off in 5 s · Esc"));
        assert_eq!(&b.rows[2].styles()[1..25], "w".repeat(24), "in warn, glyph to Esc");
        assert_eq!(b.cells(), Power::new(&counting(5)).unwrap().render().cells(), "the popup keeps its size");
        // Enter at once, as the same digit: the countdown is not the question, nothing runs.
        assert_eq!(p.key(Key::Enter, false, at + CONFIRM_AFTER), PowerAction::Nothing);
    }

    /// The deadline is each whole second, then the end; `due` redraws as `left` falls and runs the
    /// row at the end, never before.
    #[test]
    fn the_countdown_steps_each_second_then_runs() {
        let t = Instant::now();
        let mut p = Power::new(&counting(5)).unwrap();
        let (_, at) = confirm_power_off(&mut p, t);
        for n in 1..5u32 {
            assert_eq!(p.deadline(), Some(at + n * SECOND), "the {n}th second");
            assert_eq!(p.due(at + n * SECOND - Duration::from_millis(1)), PowerAction::Nothing, "not yet");
            assert_eq!(p.due(at + n * SECOND), PowerAction::Redraw);
            assert_eq!(p.counting.map(|c| c.left), Some(5 - n as u8));
        }
        assert_eq!(row_texts(&p.render())[2], format!(" {POWER} Power off in 1 s · Esc"));
        assert_eq!(p.deadline(), Some(at + 5 * SECOND), "then its end");
        assert_eq!(p.due(at + 5 * SECOND - Duration::from_millis(1)), PowerAction::Nothing);
        assert_eq!(p.due(at + 5 * SECOND), PowerAction::RunAndClose(PowerEntry::Session(Session::Poweroff)));
        assert_eq!((p.counting, p.deadline()), (None, None), "and nothing left to wait for");
        // A late tick (the loop was busy) still lands on the right second.
        let mut p = Power::new(&counting(5)).unwrap();
        let (_, at) = confirm_power_off(&mut p, t);
        assert_eq!(p.due(at + 2 * SECOND + Duration::from_millis(300)), PowerAction::Redraw);
        assert_eq!(p.counting.map(|c| c.left), Some(3));
        assert_eq!(p.deadline(), Some(at + 3 * SECOND));
        assert_eq!(p.due(at + 9 * SECOND), PowerAction::RunAndClose(PowerEntry::Session(Session::Poweroff)), "very late: runs");
    }

    /// Escape cancels the countdown and nothing runs, the menu stays (Power off drawn plain); a
    /// second Escape closes it.
    #[test]
    fn esc_cancels_the_countdown_and_runs_nothing() {
        let t = Instant::now();
        let mut p = Power::new(&counting(3)).unwrap();
        let (_, at) = confirm_power_off(&mut p, t);
        assert!(p.counting.is_some());
        assert_eq!(p.key(Key::Escape, false, at + SECOND), PowerAction::Redraw, "cancelled, the menu open");
        assert_eq!((p.counting, p.question, p.deadline()), (None, None, None));
        assert_eq!(row_texts(&p.render())[2], format!(" {POWER} Power off"));
        assert_eq!(p.due(at + 3 * SECOND), PowerAction::Nothing, "its end runs nothing");
        assert_eq!(p.key(Key::Escape, false, at + SECOND), PowerAction::Close, "a second Escape closes");
    }

    /// Enter and Space, pressed or held, do nothing to a countdown; neither does a held key.
    #[test]
    fn enter_and_space_during_the_countdown_do_nothing() {
        let t = Instant::now();
        let mut p = Power::new(&counting(3)).unwrap();
        let (_, at) = confirm_power_off(&mut p, t);
        let running = p.counting;
        for (k, repeat) in [(Key::Enter, false), (Key::Char(' '), false), (Key::Enter, true), (Key::Char('3'), true), (Key::Escape, true)] {
            assert_eq!(p.key(k, repeat, at + SECOND), PowerAction::Nothing, "{k:?} repeat {repeat}");
            assert_eq!(p.counting, running, "{k:?} repeat {repeat}");
        }
        assert_eq!(p.hover(2), PowerAction::Nothing, "hovering its own row keeps it");
        assert_eq!(p.press(2, at + SECOND), PowerAction::Nothing, "so does a press on it");
        assert_eq!(p.counting, running);
    }

    /// Any other key cancels it, and only cancels (the key does nothing else); so do a hover and a
    /// press on another row, the hover selecting that row, the press running nothing.
    #[test]
    fn any_other_key_or_a_hover_elsewhere_cancels_it() {
        let t = Instant::now();
        let start = || {
            let mut p = Power::new(&counting(3)).unwrap();
            let (_, at) = confirm_power_off(&mut p, t);
            (p, at + SECOND)
        };
        for k in [Key::Up, Key::Down, Key::Tab, Key::Char('x'), Key::Char('1'), Key::Char('3'), Key::Left] {
            let (mut p, now) = start();
            assert_eq!(p.key(k, false, now), PowerAction::Redraw, "{k:?}");
            assert_eq!((p.counting, p.question, p.selected), (None, None, Some(2)), "{k:?}: cancelled, nothing else");
        }
        let (mut p, _) = start();
        assert_eq!(p.hover(0), PowerAction::Redraw);
        assert_eq!((p.counting, p.selected), (None, Some(0)), "a hover elsewhere: cancelled, that row selected");
        let (mut p, now) = start();
        assert_eq!(p.press(0, now), PowerAction::Redraw, "a press elsewhere cancels and never runs Lock");
        assert_eq!((p.counting, p.selected), (None, Some(0)));
        assert_eq!(p.hover(9), PowerAction::Nothing, "past the rows: nothing");
    }

    /// A row's own countdown overrides the global one, and counts for a row that does not ask
    /// (at its first activation); a confirm row without its own takes the global one.
    #[test]
    fn a_rows_own_countdown_overrides_the_global_one() {
        let t = Instant::now();
        let mut own = user("Own", false);
        own.countdown = Some(2);
        let mut asks = user("Asks", true);
        asks.countdown = Some(7);
        let mut c = with_rows(vec![own, asks, user("Global", true)]);
        c.power.countdown = 4;
        let mut p = Power::new(&c).unwrap();
        assert_eq!(digit(&mut p, '4', t), PowerAction::Redraw, "no question: its countdown at once");
        assert_eq!(p.counting, Some(Countdown { entry: PowerEntry::User(0), ends: t + 2 * SECOND, left: 2 }));
        let mut p = Power::new(&c).unwrap();
        digit(&mut p, '5', t);
        assert_eq!(digit(&mut p, '5', t + CONFIRM_AFTER), PowerAction::Redraw);
        assert_eq!(p.counting.map(|c| (c.entry, c.left)), Some((PowerEntry::User(1), 7)), "its own 7 over the global 4");
        let mut p = Power::new(&c).unwrap();
        digit(&mut p, '6', t);
        digit(&mut p, '6', t + CONFIRM_AFTER);
        assert_eq!(p.counting.map(|c| (c.entry, c.left)), Some((PowerEntry::User(2), 4)), "the global one");
        // A row's own 0 turns the global one off for it.
        let mut zero = user("Zero", true);
        zero.countdown = Some(0);
        let mut c = with_rows(vec![zero]);
        c.power.countdown = 4;
        let mut p = Power::new(&c).unwrap();
        digit(&mut p, '4', t);
        assert_eq!(digit(&mut p, '4', t + CONFIRM_AFTER), PowerAction::RunAndClose(PowerEntry::User(0)));
    }

    /// `countdown = 0` (the default): the confirming activation runs at once, as before.
    #[test]
    fn countdown_zero_runs_at_once() {
        let t = Instant::now();
        let mut p = Power::new(&counting(0)).unwrap();
        let (action, _) = confirm_power_off(&mut p, t);
        assert_eq!(action, PowerAction::RunAndClose(PowerEntry::Session(Session::Poweroff)));
        assert_eq!((p.counting, p.deadline()), (None, None));
        // Lock does not ask and has no countdown of its own: the global one leaves it alone.
        let mut p = Power::new(&counting(5)).unwrap();
        assert_eq!(digit(&mut p, '1', t), PowerAction::RunAndClose(PowerEntry::Session(Session::Lock)));
    }
}
