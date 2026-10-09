//! The notifications module on the bar's side (notifications task 5): where the server starts
//! (the tray's connection, or the bar's own link), the bell's view, do-not-disturb (the one path
//! for every change, persisted), Clear all, and the `alpheus-launch` verbs. Runs on the loop, no
//! thread, no command, no timer. The bubbles (task 6) and the panel (task 7) reach the server
//! through `App::notify_do` (wayland.rs).

use crate::config::ModuleId;
use crate::link::{Host, LinkOutput, LinkUser, SessionLink, host};
use crate::log::write as log;
use crate::notify::{Center, NotifyView};
use crate::state::Msg;
use crate::state_file;
use crate::tray::notifications::Notifications;
use crate::tray::router::{Tray, TrayOutput};
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
            Host::NoBus => log("notifications: no session bus"),
            Host::OnTray => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.start_notifications(&self.bar.config.notifications, self.dnd);
                }
                log("notifications: on the tray's session bus");
                // Its RequestName is pending (the tray's deadline timer), its first view deferred.
                self.tray_output(TrayOutput::default());
            }
            Host::OnLink | Host::OwnBus => {
                if self.link.is_none() && !self.open_link(LinkUser::Notifications) {
                    return;
                }
                if let Some(link) = self.link.as_mut() {
                    link.start_notifications(&self.bar.config.notifications, self.dnd);
                }
                log("notifications: on the bar's session bus");
                // Its first view waits in the link (`has_ready`): `link_sync` routes it from an idle.
                self.link_output(LinkOutput::default());
            }
        }
    }

    /// The server, wherever it lives (tasks 6 and 7 read its centre).
    #[allow(dead_code)] // its first readers are the bubbles (task 6) and the panel (task 7)
    pub(crate) fn notifications(&self) -> Option<&Notifications> {
        self.tray.as_ref().and_then(Tray::notifications).or_else(|| self.link.as_ref().and_then(SessionLink::notifications))
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
        if let Err(e) = state_file::save_dnd(&paths::state_dir(), on) {
            log(&format!("notifications: do not disturb could not be saved ({e})"));
        }
        self.notify_do(move |n| {
            n.act(|c| {
                c.set_dnd(on);
                Vec::new()
            })
        });
    }

    /// Clear all: every notification goes (their NotificationClosed sent while the name is held).
    pub(crate) fn clear_notifications(&mut self) {
        self.notify_do(|n| n.act(Center::clear));
        log("notifications: cleared");
    }

    /// `alpheus-launch --dnd …` / `--clear-notifications`, answered before the launcher's guard
    /// (launch.rs). Done, even with no session bus (the file is still written); an error when the
    /// module is not in `modules`.
    pub(crate) fn notifications_verb(&mut self, request: &Request) -> Reply {
        if !self.bar.config.modules.contains(&ModuleId::Notifications) {
            return Reply::Error("the notifications module is not in modules".into());
        }
        match request {
            Request::Dnd(verb) => self.set_dnd(dnd_after(*verb, self.dnd)),
            Request::ClearNotifications => self.clear_notifications(),
            Request::Toggle | Request::Close | Request::Dmenu { .. } => {}
        }
        Reply::Done
    }

    /// The bubbles to show changed. A stub: task 6 deletes it and defines the real one in
    /// bubbles_surface.rs (create, redraw, drop the Overlay surface; arm `bubble_timer` for the
    /// earliest expiry). Called after the tray's or the link's output.
    pub(crate) fn bubbles_sync(&mut self) {}
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
