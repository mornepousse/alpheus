//! The resources popup's wiring (resources popup spec §§ 1, 6): opened by a left press on the
//! module, repainted on each resources tick while it is open and only then, `Monitor…` spawned
//! through the daemon's executor with `[actions] resources` as given, never waited for. No timer of
//! its own: the bar's resources timer is the only clock.

use crate::clock::LocalTime;
use crate::keys::Key;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer, no_hook};
use crate::resources_popup::{ResourcesAction, ResourcesPopup};
use crate::startup::on_message;
use crate::state::Msg;
use crate::system::Sample;
use crate::wayland::App;
use alpheus_commun::exec::Executor;
use smithay_client_toolkit::reexports::client::QueueHandle;

/// The resources popup's row of `popup::POPUPS`.
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Resources,
    toggle: |app, qh, anchor, _| app.resources_toggle(qh, anchor),
    key: App::resources_key,
    pointer: App::resources_pointer,
    flush: no_hook,
    closed: no_hook,
};

/// Spawns `argv` (`[actions] resources`, as given), never waiting. The log line: "resources:
/// monitor", or "resources: monitor could not start (<error>)"; never the command itself, whose
/// program or arguments may name the user. None for an empty command.
pub(crate) fn spawn_monitor(exec: &dyn Executor, argv: &[String]) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match exec.spawn(program, &args) {
        Ok(()) => "resources: monitor".to_string(),
        Err(e) => format!("resources: monitor could not start ({e:?})"),
    })
}

/// Whether a resources tick repaints the popup: only while it is the open one.
pub(crate) fn after_tick(open: bool) -> bool {
    open
}

impl App {
    pub(crate) fn resources_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Resources(_)))
    }

    /// A left press on the module: toggles the popup under it (a second press closes it).
    pub(crate) fn resources_toggle(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor) {
        if self.resources_shown() {
            self.popup_close(PopupClose::OutsideClick);
            return;
        }
        let popup = ResourcesPopup::default();
        let rows = popup.render(&self.bar.resources, &self.bar.config.resources);
        let shown = self.bar.config.resources.show.iter().map(|i| i.name()).collect::<Vec<_>>().join(" ");
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Resources(popup), columns, height, Input::Grab) {
            self.popup_paint(rows);
            log(&format!("resources popup shown ({shown})"));
        }
    }

    /// ↓ ↑ Tab Shift+Tab Home End, Enter, Space, Escape (resources popup spec § 6).
    fn resources_key(&mut self, k: Key) {
        let action = self.resources_with(|p| p.key(k));
        self.resources_action(action);
    }

    /// Hovering `Monitor…` selects it, a left press runs it; a cell's row counts from the header.
    fn resources_pointer(&mut self, cell: Option<(u16, u16)>, kind: PopupPointer) {
        let App { popup, bar, .. } = self;
        let action = match popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Resources(p)) => p.pointer(cell.map(|(_, r)| r), kind, &bar.config.resources),
            _ => ResourcesAction::Nothing,
        };
        self.resources_action(action);
    }

    /// `f` on the open popup; Nothing when it is not open.
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

    /// The timer's reading: applied as before; the open popup repainted after it.
    pub(crate) fn resources_tick(&mut self, s: Sample) {
        on_message(self, Msg::Resources(s), || LocalTime::now().0);
        if after_tick(self.resources_shown()) {
            self.resources_refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpheus_commun::exec::ExecError;
    use alpheus_commun::exec::fake::FakeExecutor;

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
        assert_eq!(line, "resources: monitor could not start (Io(\"busy\"))");
        let fake = FakeExecutor::new();
        let line = spawn_monitor(&fake, &argv).unwrap_or_default();
        assert!(line.starts_with("resources: monitor could not start ("), "{line}");
        assert!(!line.contains("fake-monitor"), "never the command: {line}");
        assert_eq!(spawn_monitor(&fake, &[]), None, "empty: nothing to spawn");
    }

    /// A tick repaints the popup only while it is the open one.
    #[test]
    fn a_tick_repaints_only_an_open_resources_popup() {
        assert!(after_tick(true));
        assert!(!after_tick(false));
    }
}
