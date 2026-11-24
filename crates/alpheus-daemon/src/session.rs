//! The session entries shared by the quick panel's session row and the ⏻ power menu (power spec
//! § 4): lock, log out, reboot and power off — their glyph, their words and their `[quick]` command
//! — and the reboot and power-off question: asked by a first activation, confirmed by a second
//! within CONFIRM, never before CONFIRM_AFTER. Pure: nothing here runs a command or arms a timer.
//!
//! Never:
//! - run a command or arm a timer: the panel and the menu do that, each with its own
//!   (`the_quick_panel_has_no_panicking_path` covers this file, and
//!   `every_power_menu_activation_is_a_configured_session` pins what may be run);
//! - confirm too early or too late: a second activation counts only from `CONFIRM_AFTER` and until
//!   `CONFIRM`, so a double click and a held Enter never reboot (docs/decisions.md, "Reboot and
//!   power off ask first, and a second activation confirms from 0.4 s to 3 s").
//!
//! See: docs/features/power.md, docs/features/quick-settings.md ("Reboot and power off"),
//! docs/configuration.md (`[quick]`'s four commands).

use crate::config::QuickConfig;
use std::time::{Duration, Instant};

// The quick spec's glyphs (§ 2): Nerd Font Material Design icons, and the power symbol.
pub const LOCK: char = '\u{f033e}';
pub const LOGOUT: char = '\u{f0343}';
pub const REBOOT: char = '\u{f0709}';
pub const POWER: char = '\u{23fb}';
/// How long "Reboot? ⏎" waits for its second activation (quick spec § 2).
pub const CONFIRM: Duration = Duration::from_secs(3);
/// The second activation counts only this long after the question showed (2026-10-11): a
/// double click or a double Enter never confirms.
pub const CONFIRM_AFTER: Duration = Duration::from_millis(400);

/// A session entry, in the order the quick panel's buttons and the power menu's rows show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Lock,
    Logout,
    Reboot,
    Poweroff,
}

impl Session {
    pub const ALL: [Session; 4] = [Session::Lock, Session::Logout, Session::Reboot, Session::Poweroff];

    /// Its configured command; empty: the entry is not shown.
    pub fn command(self, c: &QuickConfig) -> &[String] {
        match self {
            Session::Lock => &c.lock,
            Session::Logout => &c.logout,
            Session::Reboot => &c.reboot,
            Session::Poweroff => &c.poweroff,
        }
    }

    pub fn glyph(self) -> char {
        match self {
            Session::Lock => LOCK,
            Session::Logout => LOGOUT,
            Session::Reboot => REBOOT,
            Session::Poweroff => POWER,
        }
    }

    /// Its label in the power menu, and the question's word.
    pub fn prompt(self) -> &'static str {
        match self {
            Session::Lock => "Lock",
            Session::Logout => "Log out",
            Session::Reboot => "Reboot",
            Session::Poweroff => "Power off",
        }
    }

    /// The question while it waits for its second activation, drawn in warn: "󰜉 Reboot? ⏎".
    pub fn question(self) -> String {
        format!("{} {}? ⏎", self.glyph(), self.prompt())
    }

    /// Reboot and power off ask once more first (quick spec § 2, power spec § 2).
    pub fn asks(self) -> bool {
        matches!(self, Session::Reboot | Session::Poweroff)
    }

    /// In the log: "quick: power off", "power: power off".
    pub fn word(self) -> &'static str {
        match self {
            Session::Lock => "lock",
            Session::Logout => "log out",
            Session::Reboot => "reboot",
            Session::Poweroff => "power off",
        }
    }

    /// Its key in `[quick]`: the power menu's "shown" log line names its entries by it.
    pub fn setting(self) -> &'static str {
        match self {
            Session::Lock => "lock",
            Session::Logout => "logout",
            Session::Reboot => "reboot",
            Session::Poweroff => "poweroff",
        }
    }
}

/// The entries whose command is set, in `Session::ALL`'s order: the quick panel's buttons, the
/// power menu's rows (power spec § 2, § 3).
pub fn configured(c: &QuickConfig) -> Vec<Session> {
    Session::ALL.into_iter().filter(|s| !s.command(c).is_empty()).collect()
}

/// Whether any entry has a command: `!configured(c).is_empty()` without building the list (the bar
/// asks at every layout).
pub fn any_configured(c: &QuickConfig) -> bool {
    Session::ALL.iter().any(|s| !s.command(c).is_empty())
}

/// What an activation of an entry does (power spec § 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// Run its command now: lock and log out at once, reboot and power off on the confirming one.
    Run,
    /// The question shows (or shows again): the entry asks first.
    Ask,
    /// Too soon after the question (a double click, a double Enter): nothing; the question stays.
    Wait,
}

/// An activation of `s` at `now`, `question` the one pending ("Reboot? ⏎" until that instant):
/// lock and log out run at once; reboot and power off ask (`ask`).
pub fn activate(question: &mut Option<(Session, Instant)>, s: Session, now: Instant) -> Activation {
    if !s.asks() {
        *question = None;
        return Activation::Run;
    }
    ask(question, s, now)
}

/// A question `q` at `now`, `question` the one pending until that instant: asked on a first
/// activation, answered on a second within CONFIRM, one inside CONFIRM_AFTER ignored. Another
/// question, or an expired one, never answers it: `q` asks its own.
pub fn ask<Q: PartialEq + Clone>(question: &mut Option<(Q, Instant)>, q: Q, now: Instant) -> Activation {
    match question {
        Some((c, until)) if *c == q && now < *until && until.saturating_duration_since(now) > CONFIRM - CONFIRM_AFTER => Activation::Wait,
        Some((c, until)) if *c == q && now < *until => {
            *question = None;
            Activation::Run
        }
        _ => {
            *question = Some((q, now + CONFIRM));
            Activation::Ask
        }
    }
}

/// At `now`: what waits until an instant (a question, a list's `failed` mark) gone once that passed;
/// true when it just went (its row redrawn).
pub fn expire<Q>(question: &mut Option<(Q, Instant)>, now: Instant) -> bool {
    if question.as_ref().is_some_and(|(_, at)| *at <= now) {
        *question = None;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// The four commands set or not by the bits of `mask` (1 lock, 2 logout, 4 reboot, 8 poweroff).
    fn config(mask: u8) -> QuickConfig {
        let set = |bit: u8, argv: &[&str]| if mask & bit != 0 { argv.iter().map(|s| s.to_string()).collect() } else { Vec::new() };
        QuickConfig {
            lock: set(1, &["fake-lock"]),
            logout: set(2, &["fake-logout"]),
            reboot: set(4, &["fake-reboot"]),
            poweroff: set(8, &["fake-poweroff"]),
            ..QuickConfig::default()
        }
    }

    #[test]
    fn the_entries_are_the_commands_set_in_their_order() {
        assert_eq!(configured(&QuickConfig::default()), [Session::Lock, Session::Reboot, Session::Poweroff], "logout empty by default");
        for mask in 0..16u8 {
            let want: Vec<Session> = Session::ALL.into_iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, s)| s).collect();
            assert_eq!(configured(&config(mask)), want, "mask {mask:04b}");
        }
        for mask in 0..16u8 {
            assert_eq!(any_configured(&config(mask)), mask != 0, "mask {mask:04b}");
            assert_eq!(any_configured(&config(mask)), !configured(&config(mask)).is_empty(), "same answer as the list");
        }
        assert_eq!(Session::Poweroff.question(), format!("{POWER} Power off? ⏎"));
        assert_eq!(Session::Reboot.question(), format!("{REBOOT} Reboot? ⏎"));
        assert_eq!(Session::ALL.map(Session::word), ["lock", "log out", "reboot", "power off"]);
        assert_eq!(Session::ALL.map(Session::setting), ["lock", "logout", "reboot", "poweroff"]);
    }

    #[test]
    fn lock_and_log_out_run_at_once_and_clear_another_question() {
        let t = Instant::now();
        let mut q = Some((Session::Reboot, t + CONFIRM));
        assert_eq!(activate(&mut q, Session::Lock, t), Activation::Run);
        assert_eq!(q, None, "another entry: the question goes");
        assert_eq!(activate(&mut q, Session::Logout, t), Activation::Run);
        assert_eq!(q, None, "never a question of their own");
    }

    /// The rule of "Reboot and power off ask first, and a second activation confirms from 0.4 s to 3 s" (docs/decisions.md): the second
    /// activation counts from 0.4 s after the question, until 3 s.
    #[test]
    fn reboot_and_power_off_ask_then_confirm_from_0_4_s_until_3_s() {
        for s in [Session::Reboot, Session::Poweroff] {
            let t = Instant::now();
            let mut q = None;
            assert_eq!(activate(&mut q, s, t), Activation::Ask, "{s:?}: the first activation asks");
            assert_eq!(q, Some((s, t + CONFIRM)));
            assert_eq!(activate(&mut q, s, t + 100 * MS), Activation::Wait, "a double click never confirms");
            assert_eq!(activate(&mut q, s, t + CONFIRM_AFTER - MS), Activation::Wait, "just inside 0.4 s");
            assert_eq!(q, Some((s, t + CONFIRM)), "waiting leaves the question and its 3 s as they were");
            assert_eq!(activate(&mut q, s, t + CONFIRM_AFTER), Activation::Run, "0.4 s on: it counts");
            assert_eq!(q, None, "answered");
            activate(&mut q, s, t);
            assert_eq!(activate(&mut q, s, t + CONFIRM - MS), Activation::Run, "just inside 3 s");
            activate(&mut q, s, t);
            assert_eq!(activate(&mut q, s, t + CONFIRM), Activation::Ask, "3 s on: expired, it asks again");
            assert_eq!(q, Some((s, t + CONFIRM + CONFIRM)));
        }
    }

    #[test]
    fn another_entry_asks_its_own_question() {
        let t = Instant::now();
        let mut q = None;
        assert_eq!(activate(&mut q, Session::Reboot, t), Activation::Ask);
        let later = t + Duration::from_secs(1);
        assert_eq!(activate(&mut q, Session::Poweroff, later), Activation::Ask, "reboot's question never confirms power off");
        assert_eq!(q, Some((Session::Poweroff, later + CONFIRM)));
    }

    /// The generic question (the quick panel's session buttons and its Bluetooth forget): `activate`
    /// for an entry that asks, on any question type.
    #[test]
    fn ask_is_activate_for_a_question_that_asks() {
        let t = Instant::now();
        let mut q: Option<(&str, Instant)> = None;
        assert_eq!(ask(&mut q, "a", t), Activation::Ask);
        assert_eq!(q, Some(("a", t + CONFIRM)));
        assert_eq!(ask(&mut q, "a", t + CONFIRM_AFTER - MS), Activation::Wait, "too soon");
        assert_eq!(ask(&mut q, "b", t + MS), Activation::Ask, "another question asks its own");
        assert_eq!(q, Some(("b", t + MS + CONFIRM)));
        assert_eq!(ask(&mut q, "b", t + MS + CONFIRM_AFTER), Activation::Run);
        assert_eq!(q, None, "answered");
        for s in [Session::Reboot, Session::Poweroff] {
            let (mut by_activate, mut by_ask) = (None, None);
            for ms in [0, 100, 400, 2999, 3000] {
                let at = t + ms * MS;
                assert_eq!(activate(&mut by_activate, s, at), ask(&mut by_ask, s, at), "{s:?} at {ms} ms");
                assert_eq!(by_activate, by_ask, "{s:?} at {ms} ms");
            }
        }
    }

    #[test]
    fn the_question_expires_at_3_s() {
        let t = Instant::now();
        let mut q = Some((Session::Reboot, t + CONFIRM));
        assert!(!expire(&mut q, t + CONFIRM - MS));
        assert_eq!(q, Some((Session::Reboot, t + CONFIRM)), "still asking");
        assert!(expire(&mut q, t + CONFIRM), "3 s: gone, redrawn");
        assert_eq!(q, None);
        assert!(!expire(&mut q, t + CONFIRM), "nothing left to expire");
    }
}
