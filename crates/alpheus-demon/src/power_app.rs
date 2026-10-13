//! The ⏻ power menu's wiring (power spec § 2, § 3, § 4): opened by a left press on the power
//! button, its entries spawned through the daemon's executor with `[quick]`'s commands as given,
//! never waited for. One `DeadlineTimer` (`power_timer`) is armed only while the reboot or
//! power-off question is pending, and goes with the menu whatever closes it (`power_closed`).

use crate::actions::Button;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, PopupBody, PopupClose, PopupPointer};
use crate::power::{Power, PowerAction};
use crate::session::Session;
use crate::wayland::App;
use alpheus_commun::exec::Executor;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::Instant;

/// Spawns `s`'s command `argv` (`[quick]`'s, as given), never waiting (power spec § 3). The log
/// line: "power: <word>", or "power: <word> could not start (<error>)"; never the command itself,
/// whose program or arguments may name the user (a path in a home directory).
pub(crate) fn spawn_session(exec: &dyn Executor, argv: &[String], s: Session) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match exec.spawn(program, &args) {
        Ok(()) => format!("power: {}", s.word()),
        Err(e) => format!("power: {} could not start ({e:?})", s.word()),
    })
}

/// The pointer on the open menu (power spec § 2): one entry per row, the column never matters,
/// and a cell's row counts from the first entry (`list::boxed` drops the ring). Hovering selects,
/// a left press activates; anything else does nothing.
pub(crate) fn pointer(menu: &mut Power, cell: Option<(u16, u16)>, kind: PopupPointer, now: Instant) -> PowerAction {
    match (kind, cell.map(|(_, row)| row)) {
        (PopupPointer::Motion, Some(r)) => menu.hover(r),
        (PopupPointer::Press(Button::Left), Some(r)) => menu.press(r, now),
        _ => PowerAction::Nothing,
    }
}

impl App {
    pub(crate) fn power_shown(&self) -> bool {
        matches!(self.popup.as_ref().map(|p| &p.body), Some(PopupBody::Power(_)))
    }

    /// A left press on the power button: toggles its menu under it (a second press closes it).
    pub(crate) fn power_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if self.power_shown() {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        let Some(menu) = Power::new(&self.bar.config.quick) else { return };
        let (rows, entries) = (menu.render(), menu.describe());
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Power(menu), columns, height, Input::Grab) {
            self.popup_paint(rows);
            log(&format!("power menu shown ({entries})"));
        }
    }

    /// `f` on the open menu; Nothing when it is not open.
    pub(crate) fn power_with(&mut self, f: impl FnOnce(&mut Power) -> PowerAction) -> PowerAction {
        match self.popup.as_mut().map(|p| &mut p.body) {
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

    /// The entry's `[quick]` command, and nothing else (power spec § 3).
    fn power_spawn(&mut self, s: Session) {
        if let Some(line) = spawn_session(self.exec.as_ref(), s.command(&self.bar.config.quick), s) {
            log(&line);
        }
    }

    /// The open menu repainted (`popup_draw` commits nothing when its rows did not change), its
    /// timer re-synced.
    fn power_refresh(&mut self) {
        let rows = match self.popup.as_ref().map(|p| &p.body) {
            Some(PopupBody::Power(p)) => p.render(),
            _ => return,
        };
        self.popup_paint(rows);
        self.power_timer_sync();
    }

    /// The timer follows the pending question's end; none without one (power spec § 4).
    pub(crate) fn power_timer_sync(&mut self) {
        let at = match self.popup.as_ref().map(|p| &p.body) {
            Some(PopupBody::Power(p)) => p.deadline(),
            _ => None,
        };
        if let Some(handle) = self.loop_handle.clone() {
            self.power_timer.sync(&handle, at, App::power_tick);
        }
    }

    /// The timer fired (calloop callback; its source is dropped): the question's 3 s are up.
    pub(crate) fn power_tick(app: &mut App, now: Instant) {
        app.power_timer.fired();
        let action = app.power_with(|p| p.due(now));
        app.power_action(action);
    }

    /// The menu went, whatever closed it (`popup_close`): its timer goes too.
    pub(crate) fn power_closed(&mut self) {
        if let Some(handle) = self.loop_handle.clone() {
            self.power_timer.sync(&handle, None, App::power_tick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuickConfig;
    use alpheus_commun::exec::ExecError;
    use alpheus_commun::exec::fake::FakeExecutor;

    /// Power spec § 3: the command as given, spawned once; the log line names the entry, never the
    /// command (a configured path may hold the user's name).
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

    /// Power spec § 2: the pointer's cell rows count from the first entry (the box's ring is not a
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
