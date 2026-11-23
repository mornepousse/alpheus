//! The ⏻ power menu's wiring: opens the menu, passes it keys and the pointer, runs its entries.
//!
//! A left press on the power button, or the `alpheus-launch --power` verb, toggles the popup
//! (`PopupBody::Power`, whose model is in `mod.rs`). An entry runs the `[quick]` command that is
//! set for it, handed to the daemon's executor (the object every program the daemon runs goes
//! through, which tests replace with a fake) exactly as written and never waited for. One
//! `DeadlineTimer` (alpheus's one-shot timer on the event loop), `power_timer`, is armed only while
//! a reboot or power-off question is pending, and goes with the menu whatever closes it
//! (`power_closed`). `OPS` is this popup's row in the table of popups (`popup::POPUPS`), which is
//! how the shared popup code reaches the functions below.
//!
//! Never:
//! - wait for a command, or log it: the log line names the entry (`power: power off`), because the
//!   command's program or arguments may name the user (see docs/decisions.md, "Logs carry fixed
//!   words and numbers only");
//! - run anything but the entry's own `[quick]` command (a guardrail checks);
//! - leave the timer armed once the menu is gone.
//!
//! See: docs/specs/2026-10-13-alpheus-power-menu-design.md (§ 2, § 3, § 4),
//! docs/specs/2026-11-21-alpheus-tools-menu-design.md (§ 7, the verb), docs/features/power.md, and
//! docs/decisions.md ("The power menu has its own timer", "`alpheus-launch --power` opens the menu
//! without a grab").

use crate::actions::Button;
use crate::keys::Key;
use crate::launcher::surface::CloseReason;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer, no_hook};
use crate::power::{Power, PowerAction};
use crate::quick::app::verb_debounced;
use crate::session::Session;
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

/// What `--power` answers when the menu has nothing to show (no `[quick]` session command at all).
pub(crate) fn power_refusal(config: &crate::config::QuickConfig) -> Option<&'static str> {
    if crate::session::configured(config).is_empty() {
        return Some("no session command is set ([quick] lock, logout, reboot, poweroff)");
    }
    None
}

/// What `--power` answers when it cannot open the menu now: no command, or no bar (`has_anchor`
/// false). Decided before anything is closed, so a refused verb leaves an open launcher alone.
pub(crate) fn power_open_refusal(config: &crate::config::QuickConfig, has_anchor: bool) -> Option<&'static str> {
    if let Some(refusal) = power_refusal(config) {
        return Some(refusal);
    }
    if !has_anchor {
        return Some("no bar to open the power menu under");
    }
    None
}

/// Runs `s`'s command `argv` (`[quick]`'s, as given), never waiting for it, and returns the log
/// line to write: "power: <word>", or "power: <word> could not start (<error>)". The line never
/// holds the command itself, whose program or arguments may name the user (a path in a home
/// directory). None when `argv` is empty, since there is nothing to run.
pub(crate) fn spawn_session(exec: &dyn Executor, argv: &[String], s: Session) -> Option<String> {
    // `split_first` gives the program and the arguments; on an empty list the `?` ends the
    // function at once with None.
    let (program, args) = argv.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match exec.spawn(program, &args) {
        Ok(()) => format!("power: {}", s.word()),
        Err(e) => format!("power: {} could not start ({e:?})", s.word()),
    })
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
        if let Some(refusal) = power_open_refusal(&self.bar.config.quick, anchor.is_some() || self.power_shown()) {
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
        let Some(menu) = Power::new(&self.bar.config.quick) else { return };
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

    /// Runs `f` on the open menu and gives its answer; Nothing when the open popup is not the
    /// power menu.
    pub(crate) fn power_with(&mut self, f: impl FnOnce(&mut Power) -> PowerAction) -> PowerAction {
        match self.popup_body_mut() {
            Some(PopupBody::Power(p)) => f(p),
            _ => PowerAction::Nothing,
        }
    }

    pub(crate) fn power_action(&mut self, action: PowerAction) {
        match action {
            PowerAction::Nothing => {}
            PowerAction::Redraw => self.power_refresh(),
            PowerAction::Close => self.popup_close(PopupClose::Escape),
            PowerAction::RunAndClose(s) => {
                self.power_spawn(s);
                self.popup_close(PopupClose::Activated);
            }
        }
    }

    /// The entry's `[quick]` command, and nothing else.
    fn power_spawn(&mut self, s: Session) {
        if let Some(line) = spawn_session(self.exec.as_ref(), s.command(&self.bar.config.quick), s) {
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

    /// The timer follows the pending question's end, and is disarmed without one, so a menu with no
    /// question has no timer.
    pub(crate) fn power_timer_sync(&mut self) {
        let at = match self.popup_body() {
            Some(PopupBody::Power(p)) => p.deadline(),
            _ => None,
        };
        self.power_timer.sync_opt(self.loop_handle.as_ref(), at, App::power_tick);
    }

    /// The timer fired (calloop, the event loop, calls this; the timer's source is already dropped):
    /// the question's 3 s are up.
    pub(crate) fn power_tick(app: &mut App, now: Instant) {
        app.power_timer.fired();
        let action = app.power_with(|p| p.due(now));
        app.power_action(action);
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
    //! spawned entry logs, and where the pointer's rows land. The popup itself is checked by the
    //! guardrails and the headless tests.

    use super::*;
    use crate::config::QuickConfig;
    use alpheus_common::exec::ExecError;
    use alpheus_common::exec::fake::FakeExecutor;

    /// The verb cannot open the menu with no bar to anchor it under: refused, and (the host's
    /// glue) the open launcher is left alone.
    #[test]
    fn the_verb_is_refused_without_a_bar_to_open_under() {
        let c = QuickConfig::default();
        assert_eq!(power_open_refusal(&c, true), None);
        assert_eq!(power_open_refusal(&c, false), Some("no bar to open the power menu under"));
        // The session commands' refusal comes first.
        let none = QuickConfig { lock: Vec::new(), logout: Vec::new(), reboot: Vec::new(), poweroff: Vec::new(), ..QuickConfig::default() };
        assert_eq!(power_open_refusal(&none, false), power_refusal(&none));
    }

    /// `--power` is refused only with all four `[quick]` commands empty; one is enough to open.
    #[test]
    fn the_verb_is_refused_only_without_any_session_command() {
        let mut c = QuickConfig::default();
        assert_eq!(power_refusal(&c), None);
        (c.lock, c.logout, c.reboot, c.poweroff) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        assert_eq!(power_refusal(&c), Some("no session command is set ([quick] lock, logout, reboot, poweroff)"));
        c.logout = vec!["leave".into()];
        assert_eq!(power_refusal(&c), None);
    }

    /// The command as given, spawned once; the log line names the entry, never the command (a
    /// configured path may hold the user's name).
    #[test]
    fn an_entry_spawns_its_command_and_logs_its_word_only() {
        let c = QuickConfig::default();
        let fake = FakeExecutor::new();
        fake.ok("systemctl poweroff", "");
        assert_eq!(spawn_session(&fake, Session::Poweroff.command(&c), Session::Poweroff), Some("power: power off".into()));
        assert_eq!(fake.calls(), ["systemctl poweroff"]);
        let fake = FakeExecutor::new();
        let mine = vec!["/home/someone/bin/leave".to_string(), "--user".into(), "someone".into()];
        let line = spawn_session(&fake, &mine, Session::Logout).unwrap_or_default();
        assert_eq!(line, "power: log out could not start (NotFound)");
        assert!(!line.contains("someone"), "never the command: {line}");
        assert_eq!(fake.calls(), ["/home/someone/bin/leave --user someone"]);
        let fake = FakeExecutor::new();
        fake.respond("loginctl lock-session", Err(ExecError::Io("busy".into())));
        assert_eq!(
            spawn_session(&fake, Session::Lock.command(&c), Session::Lock),
            Some("power: lock could not start (Io(\"busy\"))".into())
        );
        let fake = FakeExecutor::new();
        assert_eq!(spawn_session(&fake, Session::Logout.command(&c), Session::Logout), None, "empty: nothing to spawn");
        assert!(fake.calls().is_empty());
    }

    /// The pointer's cell rows count from the first entry (the box's ring is not a
    /// cell), so row 0 is the first entry: hovering it selects it, a left press runs it; another
    /// button, the wheel, the pointer leaving or off every cell do nothing.
    #[test]
    fn the_pointers_first_row_is_the_first_entry() {
        let c = QuickConfig::default();
        let mut menu = Power::new(&c).expect("the default entries");
        assert_eq!(menu.render().cells().1 as usize, menu.entries.len(), "one cell row per entry, no ring row");
        let now = Instant::now();
        assert_eq!(pointer(&mut menu, Some((3, 0)), PopupPointer::Motion, now), PowerAction::Redraw);
        assert_eq!(menu.selected, Some(0), "row 0: the first entry");
        for kind in [PopupPointer::Press(Button::Right), PopupPointer::Press(Button::Middle), PopupPointer::Scroll(1), PopupPointer::Leave]
        {
            assert_eq!(pointer(&mut menu, Some((3, 0)), kind, now), PowerAction::Nothing, "{kind:?}");
        }
        assert_eq!(pointer(&mut menu, None, PopupPointer::Press(Button::Left), now), PowerAction::Nothing, "off every cell");
        assert_eq!(pointer(&mut menu, Some((3, 0)), PopupPointer::Press(Button::Left), now), PowerAction::RunAndClose(menu.entries[0]));
        assert_eq!(menu.entries[0], Session::Lock);
    }
}
