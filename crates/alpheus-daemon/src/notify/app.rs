//! The notifications module on the bar's side: where the server starts
//! (the tray's connection, or the bar's own link), the bell's view, do-not-disturb (the one path
//! for every change, persisted, timed or not), Clear all, and the `alpheus-launch` verbs. Runs on
//! the loop, no thread, no command; one timer, armed only while a timed do-not-disturb is on. The bubbles and the panel reach the server
//! through `App::notify_do` (tray_bridge.rs).

use crate::config::ModuleId;
use crate::deadline::DeadlineTimer;
use crate::link::{Host, LinkOutput, LinkUser, host};
use crate::log::write as log;
use crate::notify::battery_alert::incoming;
use crate::notify::dnd::{Dnd, parse_saved, saved_text};
use crate::notify::{Center, NotifyView};
use crate::session_bus::notifications::{Notifications, minute_now};
use crate::session_bus::router::TrayOutput;
use crate::state::Msg;
use crate::state_file;
use crate::system::Battery;
use crate::tray::bridge::notifications_of;
use crate::wayland::App;
use alpheus_common::launcher_ipc::{DND_MAX_SECS, DND_MIN_SECS, DndEnd, DndVerb, Reply, Request};
use alpheus_common::paths;
use std::time::{Duration, Instant};

/// The timed do-not-disturb's state: the model, the timer of its end, and (delivery 7) whether a
/// capture was showing at the last look.
pub(crate) struct DndTimed {
    pub(crate) model: Dnd,
    pub(crate) timer: DeadlineTimer,
    pub(crate) sharing_seen: bool,
}

impl DndTimed {
    pub(crate) fn new() -> DndTimed {
        DndTimed { model: Dnd::default(), timer: DeadlineTimer::new(), sharing_seen: false }
    }
}

/// The slack past the longest `--for` that an `--until` may reach: a local `HH:MM` tomorrow is at
/// most 24 h away, and a clock change an hour either way.
const DND_UNTIL_SLACK_SECS: u64 = 3_600;

/// The log line of a change: the cause when there is one, else the end of a timed one.
fn dnd_line(next: Dnd, why: &str, now: u64) -> String {
    let state = if next.on { "on" } else { "off" };
    match (why, next.until) {
        ("", Some(t)) if next.on => {
            format!("notifications: do not disturb {state} (ends in {} min)", t.saturating_sub(now).div_ceil(60))
        }
        ("", _) => format!("notifications: do not disturb {state}"),
        _ => format!("notifications: do not disturb {state} ({why})"),
    }
}

/// `Some(now)` when a capture started or ended since `seen`; None when nothing changed.
pub fn sharing_edge(seen: bool, now: bool) -> Option<bool> {
    (seen != now).then_some(now)
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The Unix time a requested end falls at, `now` being the Unix time. The wire is not trusted:
/// a `For` outside 1 minute to 24 hours and an `Until` that is past, or more than 25 hours ahead,
/// are refused (an `Until` in the past turns nothing on). Saturating: no overflow.
pub fn dnd_end_unix(end: DndEnd, now: u64) -> Result<u64, String> {
    match end {
        DndEnd::For(s) if (DND_MIN_SECS..=DND_MAX_SECS).contains(&s) => Ok(now.saturating_add(u64::from(s))),
        DndEnd::For(_) => Err("do-not-disturb lasts 1 minute to 24 hours".into()),
        DndEnd::Until(t) if t > now && t - now <= u64::from(DND_MAX_SECS) + DND_UNTIL_SLACK_SECS => Ok(t),
        DndEnd::Until(_) => Err("the end must be within the next 24 hours".into()),
    }
}

/// How long to wait for an end at Unix time `until`: never more than the longest legal end (a file
/// holding a far one is re-checked then, `dnd_tick`), so `Instant + wait` cannot overflow.
fn dnd_wait(until: u64, now: u64) -> Duration {
    Duration::from_secs(until.saturating_sub(now).min(u64::from(DND_MAX_SECS) + DND_UNTIL_SLACK_SECS))
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
        // The name was just acquired: an alert held back for want of a server goes out now.
        if view.serving && !before.is_some_and(|b| b.serving) {
            self.battery_deliver(None);
            self.failed_deliver(true);
        }
    }

    /// Every user path (the bell's middle click, the panel's row, the quick row): untimed, and the
    /// user's choice wins over a capture (decision 7).
    pub(crate) fn set_dnd(&mut self, on: bool) {
        self.dnd_apply(Dnd::user(on), "");
    }

    /// The one path for every do-not-disturb change: the state kept, logged (`why` names a
    /// non-user cause), persisted ($XDG_STATE_HOME/alpheus/dnd), given to the server when on or off
    /// changes, shown by the muted bell without a server, and the end's timer moved. Nothing when it
    /// does not change.
    pub(crate) fn dnd_apply(&mut self, next: Dnd, why: &str) {
        let before = self.dnd_timed.model;
        if before == next {
            return;
        }
        self.dnd_timed.model = next;
        self.dnd = next.on;
        // Saved before the line is logged, so whoever waits for the line finds the file written.
        if let Err(e) = state_file::save_dnd(&paths::state_dir(), next.saved().map(saved_text).as_deref()) {
            log(&format!("notifications: do not disturb could not be saved ({e})"));
        }
        // Logged whether a server runs or not (no session bus, mako holding the name): the bell's own
        // line needs a server.
        log(&dnd_line(next, why, unix_now()));
        if before.on != next.on {
            let on = next.on;
            self.notify_act(move |n| {
                n.act(|c| {
                    c.set_dnd(on);
                    Vec::new()
                })
            });
            self.hooks_dnd(on);
        }
        // No server: the muted bell shows the change itself.
        if self.notifications().is_none() && self.bar.notifications.is_some() {
            self.bell_without_server();
        }
        // The quick panel's row follows, whichever of the paths changed it (quick spec § 2).
        self.quick_refresh();
        self.dnd_timer_sync();
    }

    /// The option is on and both modules it needs are.
    fn dnd_sharing_enabled(&self) -> bool {
        let cfg = &self.bar.config;
        cfg.notifications.dnd_while_sharing && cfg.modules.contains(&ModuleId::Privacy) && cfg.modules.contains(&ModuleId::Notifications)
    }

    /// `[notifications] dnd_while_sharing`: on a capture's start or end (an edge, never a level),
    /// do-not-disturb follows through `Dnd::after_sharing`, which keeps the user's choice (decision 7).
    /// Nothing without the option, the `privacy` module or the `notifications` module (the state is
    /// loaded and served only with it).
    pub(crate) fn dnd_sharing_follow(&mut self) {
        if !self.dnd_sharing_enabled() {
            return;
        }
        let Some(now) = sharing_edge(self.dnd_timed.sharing_seen, self.bar.privacy.shown().screen.active()) else {
            return;
        };
        self.dnd_timed.sharing_seen = now;
        let why = if now { "screen shared" } else { "screen sharing ended" };
        self.dnd_apply(self.dnd_timed.model.after_sharing(now), why);
    }

    /// The state as it was left (startup, with the loop handle made): an end already passed, or a
    /// capture's turning on, reads as off, and the file is rewritten (or removed) to say so.
    pub(crate) fn dnd_load(&mut self, now: u64) {
        let dir = paths::state_dir();
        let read = state_file::read_dnd(&dir).map(|t| parse_saved(&t));
        let loaded = Dnd::loaded(read, now);
        if loaded.saved() != read {
            if let Err(e) = state_file::save_dnd(&dir, loaded.saved().map(saved_text).as_deref()) {
                log(&format!("notifications: do not disturb could not be saved ({e})"));
            }
        }
        self.dnd_timed.model = loaded;
        self.dnd = loaded.on;
        self.dnd_timer_sync();
    }

    /// The end's timer: armed at the end while a timed do-not-disturb is on, removed otherwise.
    fn dnd_timer_sync(&mut self) {
        let now = unix_now();
        let at = self.dnd_timed.model.until.and_then(|u| Instant::now().checked_add(dnd_wait(u, now)));
        self.dnd_timed.timer.sync_opt(self.loop_handle.as_ref(), at, App::dnd_tick);
    }

    /// The timer fired: calloop dropped it (`fired` first), then the wall clock decides.
    fn dnd_tick(app: &mut App, _: Instant) {
        app.dnd_timed.timer.fired();
        app.dnd_check_clock();
    }

    /// Whether the wall clock has reached the end: do-not-disturb goes off (or, during a share with the option on, hands over to sharing); if not yet (the
    /// monotonic timer is early, or a far end was clamped) the timer is armed for the rest. Called
    /// by the timer and by every clock tick (a suspended laptop's timer is late, decision 8).
    pub(crate) fn dnd_check_clock(&mut self) {
        let model = self.dnd_timed.model;
        if model.until.is_none() {
            return;
        }
        let sharing = self.dnd_sharing_enabled() && self.dnd_timed.sharing_seen;
        let next = model.expired(unix_now(), sharing);
        if next != model {
            self.dnd_apply(next, if sharing { "screen shared" } else { "timer ended" });
        } else if !self.dnd_timed.timer.is_armed() {
            self.dnd_timer_sync();
        }
    }

    /// A power reading (before the bar keeps it): the laptop's battery against `[battery] warn`.
    pub(crate) fn battery_feed(&mut self, m: &Msg) {
        if let Msg::Battery(Some(state)) = m {
            self.battery_deliver(Some(state.laptop));
        }
    }

    /// Feeds a reading (None: the last one again, when the server just started serving). An alert
    /// counts as given only while this daemon holds the name; before that it stays armed.
    fn battery_deliver(&mut self, reading: Option<Option<Battery>>) {
        let serving = self.notifications().is_some_and(|n| n.held());
        let Some(alerts) = self.battery_alerts.as_mut() else { return };
        let laptop = reading.unwrap_or_else(|| alerts.last());
        let Some(alert) = alerts.feed(laptop, serving) else { return };
        let n = incoming(alert);
        self.notify_act(move |s| {
            s.act(|c| {
                c.notify(n, Instant::now(), minute_now());
                c.take_effects()
            })
        });
        log(&format!("battery: alert at {} %", alert.threshold));
    }

    /// The failed units' total changed (`Msg::Units` or the user manager's): the notice, when
    /// `[failed] notify` is on and the total rose above the last one notified.
    pub(crate) fn failed_notify(&mut self) {
        self.failed_deliver(false);
    }

    /// Feeds the notice the bar's total (`again`: the server just started, so the last total seen
    /// once more). Counts as given only while this daemon holds the name; before that it stays armed.
    fn failed_deliver(&mut self, again: bool) {
        let serving = self.notifications().is_some_and(|n| n.held());
        let Some(notice) = self.failed_notice.as_mut() else { return };
        let total = if again { notice.last() } else { crate::failed::total(&self.bar.failed) };
        let Some(n) = notice.feed(total, serving) else { return };
        let incoming = crate::failed::incoming(n);
        self.notify_act(move |s| {
            s.act(|c| {
                c.notify(incoming, Instant::now(), minute_now());
                c.take_effects()
            })
        });
        log("failed: notice");
    }

    /// A Shield notice (shield spec § 8), through the server only while it serves: app `alpheus`,
    /// normal urgency; under do-not-disturb it goes to the history like every notice.
    pub(crate) fn shield_notice(&mut self, kind: crate::shield::NoticeKind) {
        let Some(sh) = self.shield.as_ref() else { return };
        if !self.notifications().is_some_and(|n| n.held()) {
            log("shield: no notification (not serving)");
            return;
        }
        let n = crate::shield::app::incoming(&kind, &sh.config);
        self.notify_act(move |s| {
            s.act(|c| {
                c.notify(n, Instant::now(), minute_now());
                c.take_effects()
            })
        });
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
            Request::Dnd(verb) => self.dnd_apply(self.dnd_timed.model.after_verb(*verb, None), ""),
            Request::DndOn(end) => match dnd_end_unix(*end, unix_now()) {
                Ok(t) => self.dnd_apply(self.dnd_timed.model.after_verb(DndVerb::On, Some(t)), ""),
                Err(msg) => return Reply::Error(msg),
            },
            Request::ClearNotifications => self.clear_notifications(),
            Request::Toggle
            | Request::Close
            | Request::Calc
            | Request::Windows
            | Request::Dmenu { .. }
            | Request::FailedRefresh
            | Request::Osd(_)
            | Request::Quick(_)
            | Request::Timer(_)
            | Request::Notes
            | Request::Clipboard
            | Request::ClipboardClear
            | Request::Tools
            | Request::Power
            | Request::Outputs
            | Request::OutputsTry(_)
            | Request::OutputsKeep
            | Request::OutputsRevert
            | Request::Shield(_) => {}
        }
        Reply::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnd_ends_are_bounded() {
        use alpheus_common::launcher_ipc::DndEnd::{For, Until};
        let duration = Err("do-not-disturb lasts 1 minute to 24 hours".to_string());
        let end = Err("the end must be within the next 24 hours".to_string());
        assert_eq!(dnd_end_unix(For(60), 1000), Ok(1060));
        assert_eq!(dnd_end_unix(For(86_400), 1000), Ok(87_400));
        assert_eq!(dnd_end_unix(For(59), 1000), duration);
        assert_eq!(dnd_end_unix(For(0), 1000), duration);
        assert_eq!(dnd_end_unix(For(86_401), 1000), duration);
        assert_eq!(dnd_end_unix(For(u32::MAX), u64::MAX - 5), duration, "refused before any addition");
        assert_eq!(dnd_end_unix(For(60), u64::MAX - 5), Ok(u64::MAX), "saturates, never overflows");
        assert_eq!(dnd_end_unix(Until(1000), 1000), end, "now is the past");
        assert_eq!(dnd_end_unix(Until(5), 1000), end, "a past end turns nothing on");
        assert_eq!(dnd_end_unix(Until(1001), 1000), Ok(1001));
        assert_eq!(dnd_end_unix(Until(1000 + 90_000), 1000), Ok(91_000));
        assert_eq!(dnd_end_unix(Until(1000 + 90_001), 1000), end);
        assert_eq!(dnd_end_unix(Until(u64::MAX), 1000), end);
    }

    #[test]
    fn the_timer_never_overflows_an_instant() {
        assert_eq!(dnd_wait(100, 90), Duration::from_secs(10));
        assert_eq!(dnd_wait(90, 100), Duration::ZERO, "an end in the past fires now");
        assert_eq!(dnd_wait(u64::MAX, 0), Duration::from_secs(u64::from(DND_MAX_SECS) + 3_600), "a hostile file's end is clamped");
    }

    #[test]
    fn sharing_reacts_to_edges_only() {
        assert_eq!(sharing_edge(false, true), Some(true));
        assert_eq!(sharing_edge(true, false), Some(false));
        assert_eq!(sharing_edge(true, true), None);
        assert_eq!(sharing_edge(false, false), None);
    }

    #[test]
    fn the_log_names_the_cause_before_the_end() {
        let sharing = Dnd { on: true, until: None, by_sharing: true };
        assert_eq!(dnd_line(sharing, "screen shared", 0), "notifications: do not disturb on (screen shared)");
        assert_eq!(dnd_line(Dnd::default(), "screen sharing ended", 0), "notifications: do not disturb off (screen sharing ended)");
        let timed = Dnd { on: true, until: Some(600), by_sharing: false };
        assert_eq!(dnd_line(timed, "", 0), "notifications: do not disturb on (ends in 10 min)");
        assert_eq!(dnd_line(timed, "odd cause", 0), "notifications: do not disturb on (odd cause)", "a cause is never swallowed");
        assert_eq!(dnd_line(Dnd::user(true), "", 0), "notifications: do not disturb on");
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
