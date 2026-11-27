//! The ⏻ power menu's wiring: opens the menu, passes it keys and the pointer, runs its entries.
//!
//! A left press on the power button, or the `alpheus-launch --power` verb, toggles the popup
//! (`PopupBody::Power`, whose model is in `mod.rs`). An entry runs the `[quick]` command that is
//! set for it, handed to the daemon's executor (the object every program the daemon runs goes
//! through, which tests replace with a fake) exactly as written and never waited for. One
//! `DeadlineTimer` (alpheus's one-shot timer on the event loop), `power_timer`, is armed only while
//! a question is pending (to its end) or a countdown runs (once a second), and goes with the menu
//! whatever closes it (`power_closed`). `OPS` is this popup's row in the table of popups
//! (`popup::POPUPS`), which is how the shared popup code reaches the functions below.
//!
//! Never:
//! - wait for a command, or log it: the log line names the entry (`power: power off`), because the
//!   command's program or arguments may name the user (see docs/decisions.md, "Logs carry fixed
//!   words and numbers only");
//! - run anything but the entry's own `[quick]` command (a guardrail checks);
//! - leave the timer armed once the menu is gone, or keep a countdown anywhere but in the menu's
//!   `Power` (`power_countdown_closed_popup_disarms_the_timer`).
//!
//! See: docs/features/power.md and docs/decisions.md ("The power menu has its own timer",
//! "`alpheus-launch --power` opens the menu without a grab").

use crate::actions::Button;
use crate::config::BarConfig;
use crate::keys::Key;
use crate::launcher::surface::CloseReason;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer, no_hook};
use crate::power::{Power, PowerAction, PowerEntry};
use crate::quick::app::verb_debounced;
use crate::wayland::App;
use alpheus_common::exec::Executor;
use alpheus_common::launcher_ipc::{Reply, Request};
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::Instant;

/// The power menu's row of `popup::POPUPS`: what the shared popup code calls for it.
///
/// (`pub(crate)` means the item is visible inside the daemon, and nowhere outside it.)
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Power,
    toggle: |app, qh, anchor, _| app.power_toggle(qh, anchor, Input::Grab),
    key: App::power_key,
    pointer: App::power_pointer,
    flush: no_hook,
    // Whatever closed the menu, the question's timer goes with it.
    closed: App::power_closed,
};

/// What `--power` answers when the menu has nothing to show (no `[quick]` session command shown
/// and no `[[power.entry]]`).
pub(crate) fn power_refusal(config: &BarConfig) -> Option<&'static str> {
    if !crate::power::any_rows(config) {
        return Some("no power menu row is set ([quick] lock, logout, reboot, poweroff, or [[power.entry]])");
    }
    None
}

/// What `--power` answers when it cannot open the menu now: no command, or no bar (`has_anchor`
/// false). Decided before anything is closed, so a refused verb leaves an open launcher alone.
pub(crate) fn power_open_refusal(config: &BarConfig, has_anchor: bool) -> Option<&'static str> {
    if let Some(refusal) = power_refusal(config) {
        return Some(refusal);
    }
    if !has_anchor {
        return Some("no bar to open the power menu under");
    }
    None
}

/// Runs `entry`'s command `argv` (`[quick]`'s or its `[[power.entry]]` `run`, as given), never
/// waiting for it, and returns the log line to write: "power: <word>", or "power: <word> could not
/// start (<kind word>)", the word "power off" for a fixed row and "entry 2" for a user row. The line
/// never holds the command itself, whose program or arguments may name the user (a path in a home
/// directory), nor a user row's name. None when `argv` is empty, since there is nothing to run.
pub(crate) fn spawn_session(exec: &dyn Executor, argv: &[String], entry: PowerEntry) -> Option<String> {
    // `split_first` gives the program and the arguments; on an empty list the `?` ends the
    // function at once with None.
    let (program, args) = argv.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match exec.spawn(program, &args) {
        Ok(()) => format!("power: {}", entry.word()),
        Err(e) => format!("power: {} could not start ({})", entry.word(), crate::log::exec_word(&e)),
    })
}

/// The log line of a change of the menu's countdown, from the row counting `before` a call to the
/// one `after` it, given the call's `action`: "power: countdown power off" (or "entry 2") when one
/// starts, "power: countdown cancelled" when one goes without running. Its end logs nothing here:
/// the row's run is logged by `spawn_session`. Fixed words only, never a name or a command.
pub(crate) fn countdown_log(before: Option<PowerEntry>, after: Option<PowerEntry>, action: PowerAction) -> Option<String> {
    match (before, after) {
        (None, Some(e)) => Some(format!("power: countdown {}", e.word())),
        (Some(_), None) if !matches!(action, PowerAction::RunAndClose(_)) => Some("power: countdown cancelled".into()),
        _ => None,
    }
}

/// The pointer on the open menu: one entry per row, the column never matters, and a cell's row
/// counts from the first entry (`list::boxed` drops the ring). Hovering selects, a left press
/// activates; anything else does nothing, and so does a pointer off every cell (`cell` is None).
pub(crate) fn pointer(menu: &mut Power, cell: Option<(u16, u16)>, kind: PopupPointer, now: Instant) -> PowerAction {
    let Some((_, row)) = cell else { return PowerAction::Nothing };
    match kind {
        PopupPointer::Motion => menu.hover(row),
        PopupPointer::Press(Button::Left) => menu.press(row, now),
        _ => PowerAction::Nothing,
    }
}

impl App {
    pub(crate) fn power_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Power(_)))
    }

    /// ↑ ↓ Tab Shift+Tab, Enter, Space, Escape; the keyboard's `repeat` flag tells the model a key
    /// is held, which it follows only to move.
    fn power_key(&mut self, k: Key) {
        let (repeat, now) = (self.keyboard.repeat, Instant::now());
        let action = self.power_with(|p| p.key(k, repeat, now));
        self.power_action(action);
    }

    /// One entry per row: hovering selects, a left press activates.
    fn power_pointer(&mut self, cell: Option<(u16, u16)>, kind: PopupPointer) {
        let now = Instant::now();
        let action = self.power_with(|p| pointer(p, cell, kind, now));
        self.power_action(action);
    }

    /// `alpheus-launch --power`, answered before the launcher's guard (`launcher/socket_glue.rs`):
    /// toggles the menu under the bar's right end on the focused output, the keyboard taken without
    /// a grab. An open launcher is closed first; a verb within the debounce of the verb's own open
    /// is ignored; an error when no `[quick]` session command is set or no bar can anchor the menu.
    /// See docs/decisions.md, "`alpheus-launch --power` opens the menu without a grab".
    pub(crate) fn power_verb(&mut self, qh: &QueueHandle<App>, request: &Request) -> Reply {
        if !matches!(request, Request::Power) {
            return Reply::Done;
        }
        let anchor = self.quick_anchor();
        if let Some(refusal) = power_open_refusal(&self.bar.config, anchor.is_some() || self.power_shown()) {
            return Reply::Error(refusal.into());
        }
        if self.power_shown() {
            if verb_debounced(self.power_verb_opened, Instant::now()) {
                log("power: toggle debounced");
            } else {
                self.popup_close(PopupClose::Verb);
            }
            return Reply::Done;
        }
        // Everything that can refuse is known: only now is an open launcher closed, so a refused
        // verb never closes it.
        if let Some(anchor) = anchor {
            if self.launcher.is_some() {
                self.launcher_guarded("power", |app| app.launcher_close(CloseReason::Replaced));
            }
            self.power_toggle(qh, anchor, Input::Keys);
        }
        Reply::Done
    }

    /// A left press on the power button (`Input::Grab`) or the verb (`Input::Keys`): toggles its
    /// menu under it (a second press closes it).
    pub(crate) fn power_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor, input: Input) {
        if self.power_shown() {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        let Some(menu) = Power::new(&self.bar.config) else { return };
        let (rows, entries) = (menu.render(), menu.describe());
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Power(menu), columns, height, input) {
            // Only a menu opened by the verb is debounced against a second verb: a click's menu
            // has no verb press to repeat.
            if input == Input::Keys {
                self.power_verb_opened = Some(Instant::now());
            } else {
                self.power_verb_opened = None;
            }
            self.popup_paint(rows);
            log(&format!("power menu shown ({entries})"));
        }
    }

    /// Runs `f` on the open menu and gives its answer, logging a countdown that starts or is
    /// cancelled (`countdown_log`); Nothing when the open popup is not the power menu.
    pub(crate) fn power_with(&mut self, f: impl FnOnce(&mut Power) -> PowerAction) -> PowerAction {
        match self.popup_body_mut() {
            Some(PopupBody::Power(p)) => {
                let before = p.counting.map(|c| c.entry);
                let action = f(p);
                if let Some(line) = countdown_log(before, p.counting.map(|c| c.entry), action) {
                    log(&line);
                }
                action
            }
            _ => PowerAction::Nothing,
        }
    }

    pub(crate) fn power_action(&mut self, action: PowerAction) {
        match action {
            PowerAction::Nothing => {}
            PowerAction::Redraw => self.power_refresh(),
            PowerAction::Close => self.popup_close(PopupClose::Escape),
            PowerAction::RunAndClose(entry) => {
                self.power_spawn(entry);
                self.popup_close(PopupClose::Activated);
            }
        }
    }

    /// The row's own command (`[quick]`'s for a fixed row, its `run` for a user row), and nothing
    /// else.
    fn power_spawn(&mut self, entry: PowerEntry) {
        if let Some(line) = spawn_session(self.exec.as_ref(), entry.command(&self.bar.config), entry) {
            log(&line);
        }
    }

    /// The open menu repainted (`popup_draw` commits nothing when its rows did not change), its
    /// timer re-synced.
    fn power_refresh(&mut self) {
        let rows = match self.popup_body() {
            Some(PopupBody::Power(p)) => p.render(),
            _ => return,
        };
        self.popup_paint(rows);
        self.power_timer_sync();
    }

    /// The timer follows the menu's deadline (the question's end, the countdown's next second), and
    /// is disarmed without one, so a menu with neither has no timer.
    pub(crate) fn power_timer_sync(&mut self) {
        let at = match self.popup_body() {
            Some(PopupBody::Power(p)) => p.deadline(),
            _ => None,
        };
        self.power_timer.sync_opt(self.loop_handle.as_ref(), at, App::power_tick);
    }

    /// The timer fired (calloop, the event loop, calls this; the timer's source is already dropped):
    /// the question's 3 s are up, or the countdown's next second (or end) came. The timer is synced
    /// again even when nothing changed, so a countdown never stalls on a tick that changed nothing;
    /// once the menu is gone (its row ran) the sync finds no deadline and arms nothing.
    pub(crate) fn power_tick(app: &mut App, now: Instant) {
        app.power_timer.fired();
        let action = app.power_with(|p| p.due(now));
        app.power_action(action);
        app.power_timer_sync();
    }

    /// The menu went, whatever closed it (`popup_close`): its timer goes too, and the verb's
    /// debounce is forgotten.
    pub(crate) fn power_closed(&mut self) {
        self.power_verb_opened = None;
        self.power_timer.sync_opt(self.loop_handle.as_ref(), None, App::power_tick);
    }
}

#[cfg(test)]
mod tests {
    //! Tests of the power menu's wiring that need no compositor: when `--power` is refused, what a
    //! spawned row logs, and where the pointer's rows land. The popup itself is checked by the
    //! guardrails and the headless tests.

    use super::*;
    use crate::config::{PowerEntryConfig, QuickConfig};
    use crate::session::Session;
    use alpheus_common::exec::ExecError;
    use alpheus_common::exec::fake::FakeExecutor;

    const REFUSED: &str = "no power menu row is set ([quick] lock, logout, reboot, poweroff, or [[power.entry]])";

    /// A bar config whose four `[quick]` session commands are empty.
    fn no_session() -> BarConfig {
        let quick =
            QuickConfig { lock: Vec::new(), logout: Vec::new(), reboot: Vec::new(), poweroff: Vec::new(), ..QuickConfig::default() };
        BarConfig { quick, ..BarConfig::default() }
    }

    /// A user row named `name` running `run`.
    fn user(name: &str, run: &[&str]) -> PowerEntryConfig {
        PowerEntryConfig {
            name: name.into(),
            icon: None,
            run: run.iter().map(|s| s.to_string()).collect(),
            confirm: false,
            countdown: None,
        }
    }

    /// The verb cannot open the menu with no bar to anchor it under: refused, and (the host's
    /// glue) the open launcher is left alone.
    #[test]
    fn the_verb_is_refused_without_a_bar_to_open_under() {
        let c = BarConfig::default();
        assert_eq!(power_open_refusal(&c, true), None);
        assert_eq!(power_open_refusal(&c, false), Some("no bar to open the power menu under"));
        // The rows' refusal comes first.
        let none = no_session();
        assert_eq!(power_open_refusal(&none, false), power_refusal(&none));
    }

    /// `--power` is refused only with no row at all; one `[quick]` command is enough to open.
    #[test]
    fn the_verb_is_refused_only_without_any_session_command() {
        let mut c = BarConfig::default();
        assert_eq!(power_refusal(&c), None);
        c = no_session();
        assert_eq!(power_refusal(&c), Some(REFUSED));
        c.quick.logout = vec!["leave".into()];
        assert_eq!(power_refusal(&c), None);
    }

    /// `builtins = false` and one user row: the button shows and `--power` opens; with the
    /// built-ins hidden and no user row, neither.
    #[test]
    fn the_button_shows_with_user_rows_only() {
        let mut c = BarConfig::default();
        c.power.builtins = false;
        assert_eq!(power_refusal(&c), Some(REFUSED), "the [quick] commands hidden: no row");
        assert!(crate::segments::power(&c).is_none());
        c.power.entry = vec![user("Quit", &["fake-quit"])];
        assert_eq!(power_refusal(&c), None);
        assert!(crate::segments::power(&c).is_some(), "the ⏻ button shows");
        let mut none = no_session();
        none.power.entry = vec![user("Quit", &["fake-quit"])];
        assert_eq!(power_refusal(&none), None, "a user row alone is enough");
    }

    /// The command as given, spawned once; the log line names the entry, never the command (a
    /// configured path may hold the user's name).
    #[test]
    fn an_entry_spawns_its_command_and_logs_its_word_only() {
        let c = BarConfig::default();
        let (poweroff, lock, logout) =
            (PowerEntry::Session(Session::Poweroff), PowerEntry::Session(Session::Lock), PowerEntry::Session(Session::Logout));
        let fake = FakeExecutor::new();
        fake.ok("systemctl poweroff", "");
        assert_eq!(spawn_session(&fake, poweroff.command(&c), poweroff), Some("power: power off".into()));
        assert_eq!(fake.calls(), ["systemctl poweroff"]);
        let fake = FakeExecutor::new();
        let mine = vec!["/home/someone/bin/leave".to_string(), "--user".into(), "someone".into()];
        let line = spawn_session(&fake, &mine, logout).unwrap_or_default();
        assert_eq!(line, "power: log out could not start (not found)");
        assert!(!line.contains("someone"), "never the command: {line}");
        assert_eq!(fake.calls(), ["/home/someone/bin/leave --user someone"]);
        let fake = FakeExecutor::new();
        fake.respond("loginctl lock-session", Err(ExecError::Io("busy".into())));
        assert_eq!(spawn_session(&fake, lock.command(&c), lock), Some("power: lock could not start (io)".into()));
        let fake = FakeExecutor::new();
        assert_eq!(spawn_session(&fake, logout.command(&c), logout), None, "empty: nothing to spawn");
        assert!(fake.calls().is_empty());
    }

    /// A user row runs its own `run` through the executor, exactly that argv and nothing else; the
    /// log says its position, never its name or command.
    #[test]
    fn session_user_row_runs_only_its_argv_through_the_fake_executor() {
        let mut c = BarConfig::default();
        c.power.entry = vec![user("someone-first", &["fake-first"]), user("someone-quit", &["fake-quit", "--user", "someone"])];
        let entry = PowerEntry::User(1);
        let fake = FakeExecutor::new();
        fake.ok("fake-quit --user someone", "");
        let line = spawn_session(&fake, entry.command(&c), entry).unwrap_or_default();
        assert_eq!(line, "power: entry 2");
        assert_eq!(fake.calls(), ["fake-quit --user someone"], "exactly its argv, once");
        let fake = FakeExecutor::new();
        let line = spawn_session(&fake, entry.command(&c), entry).unwrap_or_default();
        assert_eq!(line, "power: entry 2 could not start (not found)");
        assert!(!line.contains("someone") && !line.contains("fake"), "never the name or the command: {line}");
        let fake = FakeExecutor::new();
        let past = PowerEntry::User(2);
        assert_eq!(spawn_session(&fake, past.command(&c), past), None, "past the rows: nothing runs");
        assert!(fake.calls().is_empty());
    }

    /// The pointer's cell rows count from the first row (the box's ring is not a cell), so row 0
    /// is the first row: hovering it selects it, a left press runs it; another button, the wheel,
    /// the pointer leaving or off every cell do nothing.
    #[test]
    fn the_pointers_first_row_is_the_first_entry() {
        let c = BarConfig::default();
        let mut menu = Power::new(&c).expect("the default rows");
        assert_eq!(menu.render().cells().1 as usize, menu.rows.len(), "one cell row per row, no ring row");
        let now = Instant::now();
        assert_eq!(pointer(&mut menu, Some((3, 0)), PopupPointer::Motion, now), PowerAction::Redraw);
        assert_eq!(menu.selected, Some(0), "row 0: the first row");
        for kind in [PopupPointer::Press(Button::Right), PopupPointer::Press(Button::Middle), PopupPointer::Scroll(1), PopupPointer::Leave]
        {
            assert_eq!(pointer(&mut menu, Some((3, 0)), kind, now), PowerAction::Nothing, "{kind:?}");
        }
        assert_eq!(pointer(&mut menu, None, PopupPointer::Press(Button::Left), now), PowerAction::Nothing, "off every cell");
        assert_eq!(pointer(&mut menu, Some((3, 0)), PopupPointer::Press(Button::Left), now), PowerAction::RunAndClose(menu.rows[0].entry));
        assert_eq!(menu.rows[0].entry, PowerEntry::Session(Session::Lock));
    }

    /// The countdown's two lines: its start names the row by its word, its cancel says only that;
    /// its end is the row's own "power: <word>" from `spawn_session`, so it logs nothing here.
    #[test]
    fn the_countdown_logs_its_start_and_its_cancel_in_fixed_words() {
        let (off, entry) = (PowerEntry::Session(Session::Poweroff), PowerEntry::User(1));
        assert_eq!(countdown_log(None, Some(off), PowerAction::Redraw), Some("power: countdown power off".into()));
        assert_eq!(countdown_log(None, Some(entry), PowerAction::Redraw), Some("power: countdown entry 2".into()));
        assert_eq!(countdown_log(Some(off), None, PowerAction::Redraw), Some("power: countdown cancelled".into()));
        assert_eq!(countdown_log(Some(off), None, PowerAction::RunAndClose(off)), None, "its end: the spawn's own line");
        assert_eq!(countdown_log(Some(off), Some(off), PowerAction::Redraw), None, "a second ticked");
        assert_eq!(countdown_log(None, None, PowerAction::Close), None);
    }
}
