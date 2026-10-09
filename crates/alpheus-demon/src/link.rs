//! The bar's own session-bus connection, for when the tray module is off (media spec § 4, amended
//! 2026-10-08).
//!
//! It is ONE connection. It hosts the portal's follower (`mode = "auto"`), the media module, or both.
//! Whichever needs it first opens it, and the other joins: never two connections.
//!
//! It is driven by the bar's loop (no thread), like the tray's connection. Once opened it stays open:
//! no retry and no teardown, since an idle connection costs nothing at rest.
//!
//! Calls and decoding are the tray's typed allowlist (`tray::bus`, `tray::codec`), so this file never
//! names the dbus crate. Never name a function `method_call` here: the guardrails count that
//! substring.

use crate::appearance::Appearance;
use crate::config::{MediaConfig, ModuleId};
use crate::log::write as log;
use crate::media::MediaView;
use crate::tray::bus::{Incoming, SessionBus, refusal};
use crate::tray::codec::{self, Signal};
use crate::tray::mpris::Mpris;
use crate::tray::router::TrayOutput;
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
        self.closed |= other.closed;
    }
}

pub struct SessionLink {
    bus: SessionBus,
    appearance: Option<Appearance>,
    media: Option<Mpris>,
}

impl SessionLink {
    pub fn new(bus: SessionBus) -> SessionLink {
        SessionLink { bus, appearance: None, media: None }
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
    /// `App::mpris_do` (task 6) routes it from an idle `link_readable`, never re-entrantly.
    pub fn media_do(&mut self, f: impl FnOnce(&mut Mpris, &mut SessionBus)) {
        if let Some(media) = self.media.as_mut() {
            f(media, &mut self.bus);
            self.bus.flush();
        }
    }

    pub fn has_ready(&self) -> bool {
        self.bus.has_ready()
    }

    /// Routes `incoming`, then flushes and routes what the flush drained (bounded: what the last
    /// round's flush drained waits in `ready`, for `App::link_sync`'s idle).
    fn route(&mut self, incoming: Vec<Incoming>) -> LinkOutput {
        let mut out = self.dispatch(incoming);
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
                    }
                }
                Incoming::Signal(m) => match codec::signal(&m) {
                    Signal::ColorScheme(value) => {
                        if let Some(mode) = self.appearance.as_mut().and_then(|a| a.changed(value)) {
                            out.mode = Some(mode);
                        }
                    }
                    s => {
                        if let Some(media) = self.media.as_mut() {
                            media.signal(&mut self.bus, &s);
                        }
                    }
                },
                // Nothing is served here: refused, never left unanswered.
                Incoming::MethodCall(m) => self.bus.send_message(refusal(&m)),
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
            link_running: self.link.is_some(),
            link_tried: self.link_tried,
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
                if self.link.is_none() && !self.open_link(LinkUser::Media) {
                    return;
                }
                if let Some(link) = self.link.as_mut() {
                    link.start_media(&self.bar.config.media);
                }
                log("media: on the bar's session bus");
                self.link_output(LinkOutput::default());
            }
        }
    }

    /// Opens the bar's session link, once whatever happens (`link_tried`), for `who` (its failure
    /// lines are that user's: `link_failed`). Its fd goes in the loop, level-triggered like the tray's, and the
    /// source removes itself once the link is gone (bus closed).
    pub(crate) fn open_link(&mut self, who: LinkUser) -> bool {
        self.link_tried = true;
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
            Ok(if app.link.is_some() { PostAction::Continue } else { PostAction::Remove })
        });
        if inserted.is_err() {
            log(&link_failed(who, "event loop refused the session bus source"));
            return false;
        }
        self.link = Some(link);
        true
    }

    /// The link's fd is readable (calloop callback).
    pub(crate) fn link_readable(&mut self) {
        let Some(link) = self.link.as_mut() else { return };
        let out = link.on_readable();
        self.link_output(out);
    }

    /// Its deadline timer fired (calloop callback; the source is dropped).
    pub(crate) fn link_deadline(app: &mut App, now: Instant) {
        app.link_timer.fired();
        let Some(link) = app.link.as_mut() else { return };
        let out = link.on_deadline(now);
        app.link_output(out);
    }

    pub(crate) fn link_output(&mut self, out: LinkOutput) {
        if out.closed
            && let Some(link) = self.link.take()
        {
            if link.follows_portal() {
                log("appearance: the session bus closed; the mode stays as it is");
            }
            if link.media().is_some() {
                log("media: the session bus closed");
            }
        }
        if let Some(mode) = out.mode {
            self.portal_said(mode);
        }
        if let Some(view) = out.media {
            self.media_view(view);
        }
        self.media_popup_refresh();
        self.link_sync();
    }

    /// The timer follows the link's pending calls: none pending, no timer. Answers waiting in the
    /// link's `ready` — drained by a `media_do`'s flush, or left by the settle rounds after a burst
    /// (fix wave A) — are routed from one idle `link_readable`: no fd event will announce them.
    /// `pub(crate)`: `App::mpris_do` (wayland.rs) calls it after a `media_do`.
    pub(crate) fn link_sync(&mut self) {
        if !self.link_ready_queued
            && self.link.as_ref().is_some_and(SessionLink::has_ready)
            && let Some(handle) = &self.loop_handle
        {
            self.link_ready_queued = true;
            handle.insert_idle(|app| {
                app.link_ready_queued = false;
                app.link_readable();
            });
        }
        let deadline = self.link.as_ref().and_then(SessionLink::next_deadline);
        if let Some(handle) = &self.loop_handle {
            self.link_timer.sync(handle, deadline, App::link_deadline);
        }
    }
}

/// Who opens the bar's session link: its failure lines are that user's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkUser {
    Appearance,
    Media,
}

/// A failure line of `open_link`: the appearance's says the mode falls back to dark; the media
/// module simply is not there.
fn link_failed(who: LinkUser, what: &str) -> String {
    match who {
        LinkUser::Appearance => format!("appearance: {what}; dark"),
        LinkUser::Media => format!("media: {what}"),
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
        let mut a = LinkOutput { mode: Some(Mode::Dark), media: Some(view.clone()), closed: false };
        a.merge(LinkOutput { mode: Some(Mode::Light), ..LinkOutput::default() });
        assert_eq!(a, LinkOutput { mode: Some(Mode::Light), media: Some(view), closed: false }, "media kept");
        a.merge(LinkOutput { media: Some(None), closed: true, ..LinkOutput::default() });
        assert_eq!(a, LinkOutput { mode: Some(Mode::Light), media: Some(None), closed: true });
    }
}
