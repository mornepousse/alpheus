//! The resources popup's wiring: opens the popup, repaints it on each tick, runs `Monitor…`.
//!
//! A left press on the resources module toggles the popup (`PopupBody::Resources`, whose model is
//! in `mod.rs`). Each tick of the bar's resources timer goes through `resources_tick`, which
//! applies the new reading to the bar as before and then repaints the popup, but only while the
//! popup is the one that is open. `Monitor…` runs `[actions] resources` as written, handed to the
//! daemon's executor (the object every program the daemon runs goes through, which tests replace
//! with a fake) and never waited for. `OPS` is this popup's row in the table of popups
//! (`popup::POPUPS`), which is how the shared popup code reaches the functions below.
//!
//! Never:
//! - own a timer: the bar's resources timer is the only clock, so nothing here wakes the daemon
//!   (see docs/decisions.md, "The resources popup has no timer of its own");
//! - wait for the command, or log it: the log line says `resources: monitor`, because the command's
//!   program or arguments may name the user (see docs/decisions.md, "Logs carry fixed words and
//!   numbers only");
//! - panic (a guardrail checks).
//!
//! See: docs/features/resources.md.

use crate::clock::LocalTime;
use crate::keys::Key;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer, no_hook};
use crate::resources::{ResourcesAction, ResourcesPopup};
use crate::startup::on_message;
use crate::state::Msg;
use crate::system::Sample;
use crate::wayland::App;
use alpheus_common::exec::Executor;
use smithay_client_toolkit::reexports::client::QueueHandle;

/// The resources popup's row of `popup::POPUPS`: what the shared popup code calls for it.
///
/// (`pub(crate)` means the item is visible inside the daemon, and nowhere outside it.)
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Resources,
    toggle: |app, qh, anchor, _| app.resources_toggle(qh, anchor),
    key: App::resources_key,
    pointer: App::resources_pointer,
    // The popup owns no timer (it repaints on the bar's resources tick): nothing to flush or release.
    flush: no_hook,
    closed: no_hook,
};

/// Runs `argv` (`[actions] resources`, as written), never waiting for it, and returns the log line
/// to write: "resources: monitor", or "resources: monitor could not start (<kind word>)". The line
/// never holds the command itself, whose program or arguments may name the user. None for an empty
/// command, since there is nothing to run.
pub(crate) fn spawn_monitor(exec: &dyn Executor, argv: &[String]) -> Option<String> {
    // `split_first` gives the program and the arguments; on an empty list the `?` ends the
    // function at once with None.
    let (program, args) = argv.split_first()?;
    // The executor takes the arguments as `&str`, so the `String`s are borrowed into a list.
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match exec.spawn(program, &args) {
        Ok(()) => "resources: monitor".to_string(),
        Err(e) => format!("resources: monitor could not start ({})", crate::log::exec_word(&e)),
    })
}

impl App {
    pub(crate) fn resources_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Resources(_)))
    }

    /// A left press on the module: toggles the popup under it (a second press closes it). The
    /// popup has the `Monitor…` row only when `[actions] resources` is not empty.
    pub(crate) fn resources_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if self.resources_shown() {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        let popup = ResourcesPopup::new(!self.bar.config.actions.resources.is_empty());
        let rows = popup.render(&self.bar.resources, &self.bar.config.resources);
        // The items shown by their `[resources] show` names, for the log line.
        let shown = self.bar.config.resources.show.iter().map(|i| i.name()).collect::<Vec<_>>().join(" ");
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Resources(popup), columns, height, Input::Grab) {
            self.popup_paint(rows);
            log(&format!("resources popup shown ({shown})"));
        }
    }

    /// ↓ ↑ Tab Shift+Tab Home End, Enter, Space, Escape.
    fn resources_key(&mut self, k: Key) {
        let action = self.resources_with(|p| p.key(k));
        self.resources_action(action);
    }

    /// Hovering `Monitor…` selects it, a left press runs it; a cell's row counts from the header.
    fn resources_pointer(&mut self, cell: Option<(u16, u16)>, kind: PopupPointer) {
        // Taking the two fields out of `self` at once lets the borrow checker see that the popup
        // (changed) and the bar's config (only read) are different things.
        let App { popup, bar, .. } = self;
        // The cell's column never matters: only its row is passed on (`map` changes what is inside
        // a Some and leaves a None as it is).
        let row = cell.map(|(_, r)| r);
        let action = match popup.as_mut() {
            Some(open) => match &mut open.body {
                PopupBody::Resources(p) => p.pointer(row, kind, &bar.config.resources),
                _ => ResourcesAction::Nothing,
            },
            None => ResourcesAction::Nothing,
        };
        self.resources_action(action);
    }

    /// Runs `f` on the open popup and gives its answer; Nothing when the open popup is not the
    /// resources popup.
    fn resources_with(&mut self, f: impl FnOnce(&mut ResourcesPopup) -> ResourcesAction) -> ResourcesAction {
        match self.popup_body_mut() {
            Some(PopupBody::Resources(p)) => f(p),
            _ => ResourcesAction::Nothing,
        }
    }

    fn resources_action(&mut self, action: ResourcesAction) {
        match action {
            ResourcesAction::Nothing => {}
            ResourcesAction::Redraw => self.resources_refresh(),
            ResourcesAction::Close => self.popup_close(PopupClose::Escape),
            ResourcesAction::Monitor => {
                if let Some(line) = spawn_monitor(self.exec.as_ref(), &self.bar.config.actions.resources) {
                    log(&line);
                }
                self.popup_close(PopupClose::Activated);
            }
        }
    }

    /// The open popup repainted with the bar's current figures (`popup_draw` commits nothing when
    /// its rows did not change).
    fn resources_refresh(&mut self) {
        let rows = match self.popup_body() {
            Some(PopupBody::Resources(p)) => p.render(&self.bar.resources, &self.bar.config.resources),
            _ => return,
        };
        self.popup_paint(rows);
    }

    /// The timer's reading `s`: applied to the bar as before, then the open popup repainted, so the
    /// popup never shows a figure older than the bar's.
    pub(crate) fn resources_tick(&mut self, s: Sample) {
        // The closure is handed to `on_message` for it to ask the local time when it needs it.
        on_message(self, Msg::Resources(s), || LocalTime::now().0);
        if self.resources_shown() {
            self.resources_refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests of the resources popup's wiring that need no compositor: what `Monitor…` spawns and
    //! what it logs. The tick's order is checked by a guardrail, and the popup by the headless tests.

    use super::*;
    use alpheus_common::exec::ExecError;
    use alpheus_common::exec::fake::FakeExecutor;

    /// The command as given, spawned once; the log line says "monitor", never the command (a
    /// configured path may hold the user's name).
    #[test]
    fn spawn_monitor_logs_without_the_command() {
        let argv = vec!["fake-monitor".to_string(), "--x".into()];
        let fake = FakeExecutor::new();
        fake.ok("fake-monitor --x", "");
        let line = spawn_monitor(&fake, &argv);
        assert_eq!(line.as_deref(), Some("resources: monitor"));
        assert_eq!(fake.calls(), ["fake-monitor --x"]);
        let fake = FakeExecutor::new();
        fake.respond("fake-monitor --x", Err(ExecError::Io("busy".into())));
        let line = spawn_monitor(&fake, &argv).unwrap_or_default();
        assert_eq!(line, "resources: monitor could not start (io)");
        let fake = FakeExecutor::new();
        let line = spawn_monitor(&fake, &argv).unwrap_or_default();
        assert_eq!(line, "resources: monitor could not start (not found)");
        assert!(!line.contains("fake-monitor"), "never the command: {line}");
        assert_eq!(spawn_monitor(&fake, &[]), None, "empty: nothing to spawn");
    }
}
