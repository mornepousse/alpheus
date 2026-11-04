//! The notifications module on the bar's side: where the server starts
//! (the tray's connection, or the bar's own link), the bell's view, do-not-disturb (the one path
//! for every change, persisted), Clear all, and the `alpheus-launch` verbs. Runs on the loop, no
//! thread, no command, no timer. The bubbles and the panel reach the server
//! through `App::notify_do` (tray_bridge.rs).

use crate::config::ModuleId;
use crate::link::{Host, LinkOutput, LinkUser, host};
use crate::log::write as log;
use crate::notify::{Center, NotifyView};
use crate::session_bus::notifications::Notifications;
use crate::session_bus::router::TrayOutput;
use crate::state::Msg;
use crate::state_file;
use crate::tray_bridge::notifications_of;
use crate::wayland::App;
use alpheus_commun::launcher_ipc::{DndVerb, Reply, Request};
use alpheus_commun::paths;

/// Do-not-disturb after `verb`, from `now`.
pub fn dnd_after(verb: DndVerb, now: bool) -> bool {
    match verb {
        DndVerb::On => true,
        DndVerb::Off => false,
        DndVerb::Toggle => !now,
    }
}

/// The bell's log lines for a new view: what changed (both, for the first view). The headless
/// tests wait on them.
pub fn bell_lines(before: Option<NotifyView>, after: NotifyView) -> Vec<&'static str> {
    let mut lines = Vec::new();
    if before.map(|b| b.serving) != Some(after.serving) {
        lines.push(if after.serving { "bell: serving" } else { "bell: not serving" });
    }
    if before.map(|b| b.dnd) != Some(after.dnd) {
        lines.push(if after.dnd { "bell: dnd on" } else { "bell: dnd off" });
    }
    lines
}

impl App {
    /// The notifications server, once, at startup (`wanted`: in `modules`). It goes on the tray's
    /// connection when the tray runs; otherwise on the bar's link, opened here or joined. Without the
    /// module: nothing, no RequestName.
    pub(crate) fn start_notifications(&mut self, wanted: bool) {
        match host(wanted, self.buses()) {
            Host::Nothing => {}
            Host::NoBus => {
                log("notifications: no session bus");
                self.bell_without_server();
            }
            Host::OnTray => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.start_notifications(&self.bar.config.notifications, self.dnd);
                }
                log("notifications: on the tray's session bus");
                // Its RequestName is pending (the tray's deadline timer), its first view deferred.
                self.tray_output(TrayOutput::default());
            }
            Host::OnLink | Host::OwnBus => {
                if self.link.session.is_none() && !self.open_link(LinkUser::Notifications) {
                    self.bell_without_server();
                    return;
                }
                if let Some(link) = self.link.session.as_mut() {
                    link.start_notifications(&self.bar.config.notifications, self.dnd);
                }
                log("notifications: on the bar's session bus");
                // Its first view waits in the link (`has_ready`): `link_sync` routes it from an idle.
                self.link_output(LinkOutput::default());
            }
        }
    }

    /// In `modules` with no server (no session bus, or its link failed): the bell is drawn muted
    /// ("not serving"), its middle click still toggling do-not-disturb, its cross following it.
    fn bell_without_server(&mut self) {
        self.notify_view(NotifyView { unread: 0, dnd: self.dnd, serving: false });
    }

    /// The server, wherever it lives (`notifications_of`, kept free for split borrows).
    pub(crate) fn notifications(&self) -> Option<&Notifications> {
        notifications_of(self.tray.as_ref(), self.link.session.as_ref())
    }

    /// The bell's new view: repainted when the bar shows a change, one line per changed state.
    pub(crate) fn notify_view(&mut self, view: NotifyView) {
        let before = self.bar.notifications;
        if self.bar.apply(Msg::Notifications(view)) {
            self.dirty = true;
        }
        for line in bell_lines(before, view) {
            log(line);
        }
    }

    /// The one path for every do-not-disturb change (the verb, the bell's middle click, the panel's
    /// row): persisted ($XDG_STATE_HOME/alpheus/dnd), then given to the server. Nothing when it does
    /// not change.
    pub(crate) fn set_dnd(&mut self, on: bool) {
        if self.dnd == on {
            return;
        }
        self.dnd = on;
        // Logged whether a server runs or not (no session bus, mako holding the name): the bell's own
        // line needs a server.
        log(if on { "notifications: do not disturb on" } else { "notifications: do not disturb off" });
        if let Err(e) = state_file::save_dnd(&paths::state_dir(), on) {
            log(&format!("notifications: do not disturb could not be saved ({e})"));
        }
        self.notify_act(move |n| {
            n.act(|c| {
                c.set_dnd(on);
                Vec::new()
            })
        });
        // No server: the muted bell shows the change itself.
        if self.notifications().is_none() && self.bar.notifications.is_some() {
            self.bell_without_server();
        }
        // The quick panel's row follows, whichever of the four paths changed it (quick spec § 2).
        self.quick_refresh();
    }

    /// Clear all: every notification goes (their NotificationClosed sent while the name is held).
    pub(crate) fn clear_notifications(&mut self) {
        self.notify_act(|n| n.act(Center::clear));
        log("notifications: cleared");
    }

    /// `alpheus-launch --dnd …` / `--clear-notifications`, answered before the launcher's guard
    /// (`launcher/socket_glue.rs`, `watch_connection`). Done, even with no session bus (the file is still written); an error when the
    /// module is not in `modules`.
    pub(crate) fn notifications_verb(&mut self, request: &Request) -> Reply {
        if !self.bar.config.modules.contains(&ModuleId::Notifications) {
            return Reply::Error("the notifications module is not in modules".into());
        }
        match request {
            Request::Dnd(verb) => self.set_dnd(dnd_after(*verb, self.dnd)),
            Request::ClearNotifications => self.clear_notifications(),
            Request::Toggle
            | Request::Close
            | Request::Dmenu { .. }
            | Request::Osd(_)
            | Request::Quick(_)
            | Request::Outputs
            | Request::OutputsTry(_)
            | Request::OutputsKeep
            | Request::OutputsRevert => {}
        }
        Reply::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnd_verbs() {
        for now in [false, true] {
            assert!(dnd_after(DndVerb::On, now));
            assert!(!dnd_after(DndVerb::Off, now));
            assert_eq!(dnd_after(DndVerb::Toggle, now), !now);
        }
    }

    #[test]
    fn the_bell_logs_only_what_changed() {
        let v = |serving, dnd| NotifyView { unread: 0, dnd, serving };
        assert_eq!(bell_lines(None, v(false, false)), ["bell: not serving", "bell: dnd off"], "the first view says both");
        assert_eq!(bell_lines(Some(v(false, false)), v(true, false)), ["bell: serving"]);
        assert_eq!(bell_lines(Some(v(true, false)), v(true, true)), ["bell: dnd on"]);
        assert_eq!(bell_lines(Some(v(true, true)), v(false, false)), ["bell: not serving", "bell: dnd off"]);
        assert!(bell_lines(Some(v(true, true)), NotifyView { unread: 3, ..v(true, true) }).is_empty(), "the count alone is no news");
    }
}
