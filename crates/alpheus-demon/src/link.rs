//! The bar's own session-bus connection, for when the tray module is off (media spec § 4, amended
//! 2026-10-08).
//!
//! It is ONE connection. It hosts the portal's follower (`mode = "auto"`), the media module and the
//! notifications server, in any combination. Whichever needs it first opens it, and the others join:
//! never two connections.
//!
//! It is driven by the bar's loop (no thread), like the tray's connection. Once opened it stays open:
//! no retry and no teardown, since an idle connection costs nothing at rest.
//!
//! Calls and decoding are the tray's typed allowlist (`session_bus::bus`, `session_bus::codec`), so this file never
//! names the dbus crate. Never name a function `method_call` here: the guardrails count that
//! substring.

use crate::appearance::Appearance;
use crate::config::{MediaConfig, ModuleId, NotificationsConfig};
use crate::deadline::DeadlineTimer;
use crate::failed_app::UserUnits;
use crate::log::write as log;
use crate::media::MediaView;
use crate::notify::NotifyView;
use crate::session_bus::bus::{Incoming, SessionBus, refusal};
use crate::session_bus::codec::{self, Signal};
use crate::session_bus::mpris::Mpris;
use crate::session_bus::notifications::{self, Notifications};
use crate::session_bus::router::TrayOutput;
use crate::wayland::App;
use alpheus_commun::theme::Mode;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode as Trigger, PostAction};
use std::io;
use std::os::fd::OwnedFd;
use std::time::Instant;

/// How many times a flush's drained answers are routed before the rest waits for the next read (the
/// tray's bound, router.rs): a chatty player cannot spin the loop.
const SETTLE_ROUNDS: usize = 8;

/// Where a session-bus client lives (the portal's follower, the media module), decided purely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// Not wanted (for the portal: not `auto`, or already tried).
    Nothing,
    /// The tray runs: its connection carries this client too.
    OnTray,
    /// The bar's link is open (the other client opened it): joined.
    OnLink,
    /// No tray module, no link yet: the bar's link is opened.
    OwnBus,
    /// The tray module is on but its bus failed or closed, or the link was tried and is gone: no
    /// second attempt.
    NoBus,
}

/// The session buses as they stand (`App::buses`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buses {
    pub tray_wanted: bool,
    pub tray_running: bool,
    pub link_running: bool,
    pub link_tried: bool,
}

pub fn host(wanted: bool, b: Buses) -> Host {
    if !wanted {
        Host::Nothing
    } else if b.tray_running {
        Host::OnTray
    } else if b.tray_wanted {
        Host::NoBus
    } else if b.link_running {
        Host::OnLink
    } else if b.link_tried {
        Host::NoBus
    } else {
        Host::OwnBus
    }
}

/// What a link callback changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkOutput {
    /// The portal's word changed.
    pub mode: Option<Mode>,
    /// The media module's view changed: Some(None) hides it.
    pub media: Option<Option<MediaView>>,
    /// The notifications server's view changed: the bell repaints.
    pub notifications: Option<NotifyView>,
    /// The bubbles to show changed: the wiring re-syncs them.
    pub bubbles: bool,
    /// The user manager's failed-units count changed: Some(None) is unknown.
    pub user_units: Option<Option<u32>>,
    /// The session bus closed: the wiring drops the link (never reconnected).
    pub closed: bool,
}

impl LinkOutput {
    pub fn merge(&mut self, other: LinkOutput) {
        if other.mode.is_some() {
            self.mode = other.mode;
        }
        if other.media.is_some() {
            self.media = other.media;
        }
        if other.notifications.is_some() {
            self.notifications = other.notifications;
        }
        self.bubbles |= other.bubbles;
        if other.user_units.is_some() {
            self.user_units = other.user_units;
        }
        self.closed |= other.closed;
    }
}

/// The bar's own session link as `App` holds it: the connection, its timer and its bookkeeping.
pub(crate) struct LinkState {
    /// When the tray module is off and mode = "auto" or the media module needs the session bus, ONE
    /// connection for both. None otherwise, or once its bus closed (never reconnected).
    pub(crate) session: Option<SessionLink>,
    /// Armed only while one of the link's calls is pending (2 s); never at rest.
    pub(crate) timer: DeadlineTimer,
    /// `open_link` already ran, successfully or not: never a second connection nor a second attempt.
    pub(crate) tried: bool,
    /// An idle callback is queued to route what waits in the link's `ready` (see `link_sync`).
    pub(crate) ready_queued: bool,
}

impl LinkState {
    /// No connection, nothing armed. Written out (not derived) so the guardrail that lists every
    /// timer still counts this one.
    pub(crate) fn new() -> LinkState {
        LinkState { session: None, timer: DeadlineTimer::new(), tried: false, ready_queued: false }
    }
}

pub struct SessionLink {
    bus: SessionBus,
    appearance: Option<Appearance>,
    media: Option<Mpris>,
    notifications: Option<Notifications>,
    failed: Option<UserUnits>,
    /// What `notifications_do` changed, delivered by the next `route` (an idle `link_readable`).
    deferred: LinkOutput,
}

impl SessionLink {
    pub fn new(bus: SessionBus) -> SessionLink {
        SessionLink { bus, appearance: None, media: None, notifications: None, failed: None, deferred: LinkOutput::default() }
    }

    /// The portal's rule and its one ReadOne, sent at once. Once: a second call is a no-op.
    pub fn start_appearance(&mut self) {
        if self.appearance.is_none() {
            self.appearance = Some(Appearance::start(&mut self.bus));
            self.bus.flush();
        }
    }

    /// The MPRIS rules and the one ListNames, sent at once. Once: a second call is a no-op.
    pub fn start_media(&mut self, cfg: &MediaConfig) {
        if self.media.is_none() {
            self.media = Some(Mpris::start(&mut self.bus, cfg));
            self.bus.flush();
        }
    }

    /// The user manager's failed-units count: two rules and the owner asked for, sent at once.
    /// Once: a second call is a no-op.
    pub fn start_failed(&mut self, subscribe: bool) {
        if self.failed.is_none() {
            self.failed = Some(UserUnits::start(&mut self.bus, subscribe));
            self.bus.flush();
        }
    }

    pub fn failed(&self) -> Option<&UserUnits> {
        self.failed.as_ref()
    }

    /// Runs `f` on the user manager's count with this connection, then flushes. What the flush
    /// drained waits in `ready` (`has_ready`), routed from an idle `link_readable`.
    pub fn failed_do(&mut self, f: impl FnOnce(&mut UserUnits, &mut SessionBus)) {
        if let Some(failed) = self.failed.as_mut() {
            f(failed, &mut self.bus);
            self.bus.flush();
        }
    }

    pub fn watch_fd(&mut self) -> io::Result<OwnedFd> {
        self.bus.watch_fd()
    }

    pub fn on_readable(&mut self) -> LinkOutput {
        let incoming = self.bus.read();
        self.route(incoming)
    }

    /// The deadline timer fired. What already arrived is read first, so an answer waiting in the
    /// queue is never reported as a timeout (the tray does the same). Then the calls due are answered
    /// with Timeout.
    pub fn on_deadline(&mut self, now: Instant) -> LinkOutput {
        let mut incoming = self.bus.read();
        incoming.extend(self.bus.expire(Instant::now().max(now)));
        self.route(incoming)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.bus.next_deadline()
    }

    pub fn pending(&self) -> usize {
        self.bus.pending()
    }

    pub fn portal(&self) -> Option<Mode> {
        self.appearance.as_ref().and_then(Appearance::portal)
    }

    pub fn follows_portal(&self) -> bool {
        self.appearance.is_some()
    }

    pub fn media(&self) -> Option<&Mpris> {
        self.media.as_ref()
    }

    /// Runs `f` on the media consumer with this connection (a control, the popup's Seeked rule and
    /// position reads, a pin), then flushes. What the flush drained waits in `ready` (`has_ready`):
    /// `App::mpris_do` routes it from an idle `link_readable`, never re-entrantly.
    pub fn media_do(&mut self, f: impl FnOnce(&mut Mpris, &mut SessionBus)) {
        if let Some(media) = self.media.as_mut() {
            f(media, &mut self.bus);
            self.bus.flush();
        }
    }

    /// The notifications server: RequestName with flags 0, sent at once. Once: a second call is a
    /// no-op. The bell's first view waits for the next read.
    pub fn start_notifications(&mut self, cfg: &NotificationsConfig, dnd: bool) {
        if self.notifications.is_none() {
            self.notifications = Some(Notifications::start(&mut self.bus, cfg, dnd));
            self.bus.flush();
            let out = self.notifications_output(false);
            self.deferred.merge(out);
        }
    }

    pub fn notifications(&self) -> Option<&Notifications> {
        self.notifications.as_ref()
    }

    /// Runs `f` on the notifications server: its signals sent and flushed, its view and bubbles kept
    /// for the next read (`has_ready`), never routed re-entrantly (`App::notify_do`).
    pub fn notifications_do(&mut self, f: impl FnOnce(&mut Notifications)) {
        if let Some(n) = self.notifications.as_mut() {
            f(n);
            for m in n.take_effects() {
                self.bus.send_message(m);
            }
            self.bus.flush();
            let out = self.notifications_output(false);
            self.deferred.merge(out);
        }
    }

    /// What the bar must hear from the notifications server: its view when it changed, whether the
    /// bubbles moved. `closed`: the bus went, and the name with it.
    fn notifications_output(&mut self, closed: bool) -> LinkOutput {
        let Some(n) = self.notifications.as_mut() else { return LinkOutput::default() };
        if closed {
            n.lost();
        }
        LinkOutput { notifications: n.take_change(), bubbles: n.take_bubbles_change(), ..LinkOutput::default() }
    }

    /// Something waits for an idle `link_readable`: answers a flush drained, or what a
    /// `notifications_do` changed.
    pub fn has_ready(&self) -> bool {
        self.bus.has_ready() || self.deferred != LinkOutput::default()
    }

    /// Routes `incoming`, then flushes and routes what the flush drained (bounded: what the last
    /// round's flush drained waits in `ready`, for `App::link_sync`'s idle).
    fn route(&mut self, incoming: Vec<Incoming>) -> LinkOutput {
        let mut out = std::mem::take(&mut self.deferred);
        out.merge(self.dispatch(incoming));
        let mut rounds = 0;
        loop {
            self.bus.flush();
            if !self.bus.has_ready() || rounds == SETTLE_ROUNDS {
                break;
            }
            rounds += 1;
            let ready = self.bus.take_ready();
            out.merge(self.dispatch(ready));
        }
        // The media module's text: once per callback, only when what the bar shows changed; the bus
        // gone, the module goes with it.
        if let Some(m) = self.media.as_mut() {
            if out.closed {
                out.media = Some(None);
            } else if let Some(view) = m.take_view_change() {
                out.media = Some(view);
            }
        }
        // The user manager's count, when it changed; the bus gone, it is unknown.
        if let Some(f) = self.failed.as_mut() {
            if out.closed {
                f.lost();
            }
            if let Some(count) = f.take_change() {
                out.user_units = Some(count);
            }
        }
        // The notifications server: its view and bubbles when they changed; the bus gone, the name
        // goes with it (no bubble, no timer).
        let closed = out.closed;
        out.merge(self.notifications_output(closed));
        out
    }

    fn dispatch(&mut self, incoming: Vec<Incoming>) -> LinkOutput {
        let mut out = LinkOutput::default();
        for i in incoming {
            match i {
                Incoming::Answer { id, call, result } => {
                    let result = result.and_then(|m| codec::reply(&call, &m));
                    if let Some(a) = self.appearance.as_mut().filter(|a| a.owns(id)) {
                        if let Some(mode) = a.answer(id, result) {
                            out.mode = Some(mode);
                        }
                    } else if let Some(m) = self.media.as_mut().filter(|m| m.owns(id)) {
                        m.answer(&mut self.bus, id, result);
                    } else if let Some(f) = self.failed.as_mut().filter(|f| f.owns(id)) {
                        f.answer(&mut self.bus, id, result);
                    } else if let Some(n) = self.notifications.as_mut().filter(|n| n.owns(id)) {
                        n.answer(id, result);
                    }
                }
                Incoming::Signal(m) => match codec::signal(&m) {
                    Signal::ColorScheme(value) => {
                        if let Some(mode) = self.appearance.as_mut().and_then(|a| a.changed(value)) {
                            out.mode = Some(mode);
                        }
                    }
                    s => {
                        let ours = self.notifications.as_mut().is_some_and(|n| n.signal(&s));
                        let ours = ours || self.failed.as_mut().is_some_and(|f| f.signal(&mut self.bus, &s));
                        if !ours && let Some(media) = self.media.as_mut() {
                            media.signal(&mut self.bus, &s);
                        }
                    }
                },
                // The Notifications path is served while the name is held (and refused otherwise);
                // anything else is refused, never left unanswered.
                Incoming::MethodCall(m) => {
                    let reply = match self.notifications.as_mut().filter(|_| notifications::serves(&m)) {
                        Some(n) => n.handle(&m, Instant::now(), notifications::minute_now),
                        None => (!m.get_no_reply()).then(|| refusal(&m)),
                    };
                    if let Some(r) = reply {
                        self.bus.send_message(r);
                    }
                    if let Some(n) = self.notifications.as_mut() {
                        for s in n.take_effects() {
                            self.bus.send_message(s);
                        }
                    }
                }
                Incoming::Closed => out.closed = true,
            }
        }
        out
    }
}

impl App {
    pub(crate) fn buses(&self) -> Buses {
        Buses {
            tray_wanted: self.bar.config.modules.contains(&ModuleId::Tray),
            tray_running: self.tray.is_some(),
            link_running: self.link.session.is_some(),
            link_tried: self.link.tried,
        }
    }

    /// The media module, once, at startup (`wanted`: in `modules`). It goes on the tray's connection
    /// when the tray runs; otherwise on the bar's link, opened here or joined.
    pub(crate) fn start_media(&mut self, wanted: bool) {
        match host(wanted, self.buses()) {
            Host::Nothing => {}
            Host::NoBus => log("media: no session bus"),
            Host::OnTray => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.start_media(&self.bar.config.media);
                }
                log("media: on the tray's session bus");
                // Its ListNames is pending: the tray's deadline timer is armed for it.
                self.tray_output(TrayOutput::default());
            }
            Host::OnLink | Host::OwnBus => {
                if self.link.session.is_none() && !self.open_link(LinkUser::Media) {
                    return;
                }
                if let Some(link) = self.link.session.as_mut() {
                    link.start_media(&self.bar.config.media);
                }
                log("media: on the bar's session bus");
                self.link_output(LinkOutput::default());
            }
        }
    }

    /// Opens the bar's session link, once whatever happens (`link.tried`), for `who` (its failure
    /// lines are that user's: `link_failed`). Its fd goes in the loop, level-triggered like the tray's, and the
    /// source removes itself once the link is gone (bus closed).
    pub(crate) fn open_link(&mut self, who: LinkUser) -> bool {
        self.link.tried = true;
        let Some(handle) = self.loop_handle.clone() else { return false };
        let bus = match SessionBus::session() {
            Ok(b) => b,
            Err(e) => {
                log(&link_failed(who, &format!("no session bus ({e})")));
                return false;
            }
        };
        let mut link = SessionLink::new(bus);
        let fd = match link.watch_fd() {
            Ok(fd) => fd,
            Err(e) => {
                log(&link_failed(who, &format!("connected, but could not watch the session bus's fd ({e})")));
                return false;
            }
        };
        let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Trigger::Level), |_, _, app: &mut App| {
            app.link_readable();
            Ok(if app.link.session.is_some() { PostAction::Continue } else { PostAction::Remove })
        });
        if inserted.is_err() {
            log(&link_failed(who, "event loop refused the session bus source"));
            return false;
        }
        self.link.session = Some(link);
        true
    }

    /// The link's fd is readable (calloop callback).
    pub(crate) fn link_readable(&mut self) {
        let Some(link) = self.link.session.as_mut() else { return };
        let out = link.on_readable();
        self.link_output(out);
    }

    /// Its deadline timer fired (calloop callback; the source is dropped).
    pub(crate) fn link_deadline(app: &mut App, now: Instant) {
        app.link.timer.fired();
        let Some(link) = app.link.session.as_mut() else { return };
        let out = link.on_deadline(now);
        app.link_output(out);
    }

    pub(crate) fn link_output(&mut self, out: LinkOutput) {
        if out.closed
            && let Some(link) = self.link.session.take()
        {
            if link.follows_portal() {
                log("appearance: the session bus closed; the mode stays as it is");
            }
            if link.media().is_some() {
                log("media: the session bus closed");
            }
            if link.notifications().is_some() {
                log("notifications: the session bus closed");
            }
            if link.failed().is_some() {
                log("failed: the session bus closed");
            }
        }
        if let Some(mode) = out.mode {
            self.portal_said(mode);
        }
        if let Some(view) = out.media {
            self.media_view(view);
        }
        if let Some(view) = out.notifications {
            self.notify_view(view);
        }
        if let Some(count) = out.user_units {
            self.failed_user(count);
        }
        if out.bubbles {
            self.bubbles_sync();
        }
        self.media_popup_refresh();
        self.panel_refresh();
        self.link_sync();
    }

    /// The timer follows the link's pending calls: none pending, no timer. Answers waiting in the
    /// link's `ready` — drained by a `media_do`'s flush, or left by the settle rounds after a burst —
    /// are routed from one idle `link_readable`: no fd event will announce them.
    /// `pub(crate)`: `App::mpris_do` (tray_bridge.rs) calls it after a `media_do`.
    pub(crate) fn link_sync(&mut self) {
        if !self.link.ready_queued
            && self.link.session.as_ref().is_some_and(SessionLink::has_ready)
            && let Some(handle) = &self.loop_handle
        {
            self.link.ready_queued = true;
            handle.insert_idle(|app| {
                app.link.ready_queued = false;
                app.link_readable();
            });
        }
        let deadline = self.link.session.as_ref().and_then(SessionLink::next_deadline);
        self.link.timer.sync_opt(self.loop_handle.as_ref(), deadline, App::link_deadline);
    }
}

/// Who opens the bar's session link: its failure lines are that user's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkUser {
    Appearance,
    Media,
    Notifications,
    Failed,
}

/// A failure line of `open_link`: the appearance's says the mode falls back to dark; the media
/// module simply is not there.
fn link_failed(who: LinkUser, what: &str) -> String {
    match who {
        LinkUser::Appearance => format!("appearance: {what}; dark"),
        LinkUser::Media => format!("media: {what}"),
        LinkUser::Notifications => format!("notifications: {what}"),
        LinkUser::Failed => format!("failed: {what}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buses(tray_wanted: bool, tray_running: bool, link_running: bool, link_tried: bool) -> Buses {
        Buses { tray_wanted, tray_running, link_running, link_tried }
    }

    #[test]
    fn one_connection_at_most_whoever_asks_first() {
        use Host::*;
        assert_eq!(host(false, buses(true, true, true, true)), Nothing, "not wanted: nothing");
        assert_eq!(host(true, buses(true, true, false, false)), OnTray, "the tray's connection");
        assert_eq!(host(true, buses(true, false, false, false)), NoBus, "the tray's bus failed or closed: no second attempt");
        assert_eq!(host(true, buses(false, false, false, false)), OwnBus, "no tray, no link: open one");
        assert_eq!(host(true, buses(false, false, true, true)), OnLink, "the link is open: joined, never a second one");
        assert_eq!(host(true, buses(false, false, false, true)), NoBus, "the link was tried and is gone: never again");
    }

    #[test]
    fn a_link_failure_tells_the_appearance_it_stays_dark() {
        assert_eq!(link_failed(LinkUser::Appearance, "no session bus (x)"), "appearance: no session bus (x); dark");
        assert_eq!(link_failed(LinkUser::Media, "no session bus (x)"), "media: no session bus (x)", "the module is simply not there");
    }

    #[test]
    fn link_outputs_merge_the_latest_word() {
        let view = Some(MediaView { playing: true, title: "Harbor Lights".into() });
        let mut a = LinkOutput { mode: Some(Mode::Dark), media: Some(view.clone()), closed: false, ..LinkOutput::default() };
        a.merge(LinkOutput { mode: Some(Mode::Light), ..LinkOutput::default() });
        assert_eq!(a, LinkOutput { mode: Some(Mode::Light), media: Some(view), closed: false, ..LinkOutput::default() }, "media kept");
        a.merge(LinkOutput { media: Some(None), closed: true, ..LinkOutput::default() });
        assert_eq!(a, LinkOutput { mode: Some(Mode::Light), media: Some(None), closed: true, ..LinkOutput::default() });
    }

    #[test]
    fn link_outputs_merge_the_bell_and_the_bubbles() {
        let view = |unread| Some(NotifyView { unread, dnd: false, serving: true });
        let mut a = LinkOutput { notifications: view(1), bubbles: true, ..LinkOutput::default() };
        a.merge(LinkOutput { notifications: view(2), ..LinkOutput::default() });
        assert_eq!((a.notifications, a.bubbles), (view(2), true), "the latest view; a bubble change is never lost");
        a.merge(LinkOutput::default());
        assert_eq!((a.notifications, a.bubbles), (view(2), true));
    }
}
