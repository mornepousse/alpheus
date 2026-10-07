//! The desktop's light/dark preference (themes spec § 2–3). With `mode = "auto"`, the portal's
//! `org.freedesktop.appearance` `color-scheme` is read once (`ReadOne`: 2 s deadline, auto-start
//! off), then followed (`SettingChanged`, through one match rule narrowed to that key).
//!
//! It rides the tray's session-bus connection when the tray module is on. Otherwise it uses a
//! `PortalLink` of its own. Either way there is one connection, opened only when the mode is
//! `auto` or the tray is on.
//!
//! Calls and decoding reuse the tray's typed allowlist (`Call::ReadOne`, `codec`): one pending
//! table, one deadline rule, and this file never names the dbus crate. Never name a function
//! `method_call` here: the guardrails count that substring.

use crate::config::ModuleId;
use crate::log::write as log;
use crate::tray::bus::{Call, CallError, CallId, Incoming, Outbox, SessionBus, refusal};
use crate::tray::codec::{self, Reply, Signal};
use crate::tray::router::TrayOutput;
use crate::wayland::App;
use alpheus_commun::theme::Mode;
use alpheus_commun::theme::mode_file::{MODE_DIR, mode_path, mode_text};
use alpheus_commun::theme::settings::{ModeSetting, effective_mode, portal_mode};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode as Trigger, PostAction};
use std::fs::DirBuilder;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::Instant;

#[cfg(test)]
use crate::tray::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_NAME, PORTAL_PATH, SETTINGS_INTERFACE};

/// The one match rule: `SettingChanged` from whoever owns the portal's name, for color-scheme only.
pub const SETTING_RULE: &str = "type='signal',sender='org.freedesktop.portal.Desktop',path='/org/freedesktop/portal/desktop',interface='org.freedesktop.portal.Settings',member='SettingChanged',arg0='org.freedesktop.appearance',arg1='color-scheme'";

fn name(m: Mode) -> &'static str {
    match m {
        Mode::Dark => "dark",
        Mode::Light => "light",
    }
}

fn why(e: &CallError) -> String {
    match e {
        CallError::Timeout => "no answer within 2 s".into(),
        CallError::Disconnected => "the session bus closed".into(),
        CallError::Remote(name) => name.clone(),
        CallError::BadReply(what) => what.clone(),
    }
}

/// What the portal said about color-scheme, and the read still waiting for its answer.
#[derive(Debug, Default)]
pub struct Appearance {
    /// The ReadOne in flight: None once answered, failed or timed out.
    read: Option<CallId>,
    /// The portal's last word; None until it speaks, and for good when it is absent.
    portal: Option<Mode>,
}

impl Appearance {
    /// The match rule first (sent with no reply: nothing pending for it), so no change slips between
    /// the read and the rule; then the one ReadOne (pending, 2 s deadline).
    pub fn start(out: &mut impl Outbox) -> Appearance {
        out.send(Call::AddMatch { rule: SETTING_RULE.to_string() });
        let read = out.call(Call::ReadOne);
        Appearance { read: Some(read), portal: None }
    }

    pub fn owns(&self, id: CallId) -> bool {
        self.read == Some(id)
    }

    /// ReadOne's answer, or its timeout: Some(mode) when the portal's word changes. A failure is
    /// one log line and None: the effective mode stays dark (`effective_mode(Auto, None)`).
    pub fn answer(&mut self, id: CallId, result: Result<Reply, CallError>) -> Option<Mode> {
        if !self.owns(id) {
            return None;
        }
        self.read = None;
        match result {
            Ok(Reply::ColorScheme(value)) => self.changed(value),
            Ok(other) => {
                log(&format!("appearance: no portal (unexpected answer {other:?}); dark"));
                None
            }
            Err(e) => {
                log(&format!("appearance: no portal ({}); dark", why(&e)));
                None
            }
        }
    }

    /// A color-scheme value (ReadOne or SettingChanged): Some(mode) only when it changes.
    pub fn changed(&mut self, value: u32) -> Option<Mode> {
        let mode = portal_mode(value);
        if self.portal == Some(mode) {
            return None;
        }
        self.portal = Some(mode);
        log(&format!("appearance: the portal says {}", name(mode)));
        Some(mode)
    }

    pub fn portal(&self) -> Option<Mode> {
        self.portal
    }
}

/// Whether the mode file needs writing: `now` when it differs from what was last written.
pub fn mode_to_publish(written: Option<Mode>, now: Mode) -> Option<Mode> {
    (written != Some(now)).then_some(now)
}

/// The mode in effect to `runtime/alpheus/mode`, for the TUI. The directory is created owner-only
/// (0700) when missing, like the launcher's state directory; the text goes to a temporary file in
/// that directory, then is renamed over the old one, so a reader sees the old text or the new one,
/// never half of one. Err: the caller logs one line.
pub fn write_mode(runtime: &Path, mode: Mode) -> io::Result<()> {
    let dir = runtime.join(MODE_DIR);
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let tmp = dir.join(format!(".mode.{}", std::process::id()));
    std::fs::write(&tmp, mode_text(mode))?;
    std::fs::rename(&tmp, mode_path(runtime)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// What a link callback changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkOutput {
    /// The portal's word changed.
    pub mode: Option<Mode>,
    /// The session bus closed: the wiring drops the link (never reconnected; the mode stays).
    pub closed: bool,
}

/// The session bus for the portal alone (tray module off): one `SessionBus`, routed here, driven by
/// the bar's loop (no thread), like the tray's.
pub struct PortalLink {
    bus: SessionBus,
    appearance: Appearance,
}

impl PortalLink {
    /// The rule and the ReadOne leave at once (flushed). Never blocks after the connect.
    pub fn start(mut bus: SessionBus) -> PortalLink {
        let appearance = Appearance::start(&mut bus);
        bus.flush();
        PortalLink { bus, appearance }
    }

    pub fn watch_fd(&mut self) -> io::Result<OwnedFd> {
        self.bus.watch_fd()
    }

    pub fn on_readable(&mut self) -> LinkOutput {
        let incoming = self.bus.read();
        self.route(incoming)
    }

    /// The deadline timer fired: what already arrived is read first (an answer waiting in the queue is
    /// never reported as a timeout, as the tray does), then an unanswered ReadOne ends as Timeout
    /// (logged once, dark).
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
        self.appearance.portal()
    }

    fn route(&mut self, incoming: Vec<Incoming>) -> LinkOutput {
        let mut out = LinkOutput::default();
        for i in incoming {
            match i {
                Incoming::Answer { id, call, result } => {
                    let result = result.and_then(|m| codec::reply(&call, &m));
                    if let Some(mode) = self.appearance.answer(id, result) {
                        out.mode = Some(mode);
                    }
                }
                Incoming::Signal(m) => {
                    if let Signal::ColorScheme(value) = codec::signal(&m)
                        && let Some(mode) = self.appearance.changed(value)
                    {
                        out.mode = Some(mode);
                    }
                }
                // Nothing is served here: refused, never left unanswered.
                Incoming::MethodCall(m) => self.bus.send_message(refusal(&m)),
                Incoming::Closed => out.closed = true,
            }
        }
        self.bus.flush();
        out
    }
}

/// How to follow the portal, decided purely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalStart {
    /// Not `auto`, or already tried: nothing.
    Nothing,
    /// The tray runs: its connection carries the portal too.
    OnTray,
    /// The tray module is off: a connection of our own.
    OwnBus,
    /// The tray module is on but its bus failed or closed: no second attempt.
    NoBus,
}

pub fn portal_start(mode: ModeSetting, tried: bool, tray_wanted: bool, tray_running: bool) -> PortalStart {
    if mode != ModeSetting::Auto || tried {
        PortalStart::Nothing
    } else if tray_running {
        PortalStart::OnTray
    } else if tray_wanted {
        PortalStart::NoBus
    } else {
        PortalStart::OwnBus
    }
}

/// Whether the portal moving to `after` changes the mode in effect (a repaint), from `before`.
pub fn repaint_needed(setting: ModeSetting, before: Option<Mode>, after: Mode) -> bool {
    effective_mode(setting, before) != effective_mode(setting, Some(after))
}

impl App {
    /// With `mode = "auto"`, follows the portal once: on the tray's connection when it runs, else on a
    /// connection of its own (tray module off). Called after `start_tray` at startup and in
    /// `theme_changed` right after the new settings are stored (a switch to auto in theme.toml), so a
    /// single connection serves both, at most.
    pub(crate) fn follow_portal_if_auto(&mut self) {
        let tray_wanted = self.bar.config.modules.contains(&ModuleId::Tray);
        match portal_start(self.theme_settings.mode, self.portal_tried, tray_wanted, self.tray.is_some()) {
            PortalStart::Nothing => return,
            PortalStart::OnTray => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.start_appearance();
                }
                // Its ReadOne is pending: the tray's deadline timer is armed for it.
                self.tray_output(TrayOutput::default());
            }
            PortalStart::NoBus => log("appearance: no session bus; dark"),
            PortalStart::OwnBus => self.open_portal_link(),
        }
        self.portal_tried = true;
    }

    fn open_portal_link(&mut self) {
        let Some(handle) = self.loop_handle.clone() else { return };
        let bus = match SessionBus::session() {
            Ok(b) => b,
            Err(e) => {
                log(&format!("appearance: no session bus ({e}); dark"));
                return;
            }
        };
        let mut link = PortalLink::start(bus);
        let fd = match link.watch_fd() {
            Ok(fd) => fd,
            Err(e) => {
                log(&format!("appearance: connected, but could not watch the session bus's fd ({e}); dark"));
                return;
            }
        };
        // Level-triggered, like the tray's; removes itself once the link is gone (bus closed).
        let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Trigger::Level), |_, _, app: &mut App| {
            app.portal_readable();
            Ok(if app.portal.is_some() { PostAction::Continue } else { PostAction::Remove })
        });
        if inserted.is_err() {
            log("appearance: event loop refused the session bus source; dark");
            return;
        }
        self.portal = Some(link);
        self.portal_sync();
    }

    /// The portal link's fd is readable (calloop callback).
    pub(crate) fn portal_readable(&mut self) {
        let Some(link) = self.portal.as_mut() else { return };
        let out = link.on_readable();
        self.portal_output(out);
    }

    /// Its deadline timer fired (calloop callback; the source is dropped).
    pub(crate) fn portal_deadline(app: &mut App, now: Instant) {
        app.portal_timer.fired();
        let Some(link) = app.portal.as_mut() else { return };
        let out = link.on_deadline(now);
        app.portal_output(out);
    }

    fn portal_output(&mut self, out: LinkOutput) {
        if out.closed {
            log("appearance: the session bus closed; the mode stays as it is");
            self.portal = None;
        }
        if let Some(mode) = out.mode {
            self.portal_said(mode);
        }
        self.portal_sync();
    }

    /// The timer follows the pending ReadOne: none pending, no timer.
    fn portal_sync(&mut self) {
        let deadline = self.portal.as_ref().and_then(PortalLink::next_deadline);
        if let Some(handle) = &self.loop_handle {
            self.portal_timer.sync(handle, deadline, App::portal_deadline);
        }
    }

    /// The portal's word, from either connection: re-resolved and repainted (task 5's hook) only when
    /// the mode in effect changes.
    pub(crate) fn portal_said(&mut self, mode: Mode) {
        let repaint = repaint_needed(self.theme_settings.mode, self.portal_mode, mode);
        self.portal_mode = Some(mode);
        if repaint {
            self.theme_changed();
        }
    }

    /// Publishes the mode in effect for the TUI when it changed since the last write (startup, a
    /// reload, the portal's word). No runtime directory: nothing written. A failed write is one line;
    /// the next change tries again.
    pub(crate) fn publish_mode(&mut self) {
        let now = effective_mode(self.theme_settings.mode, self.portal_mode);
        let Some(mode) = mode_to_publish(self.mode_written, now) else { return };
        let Some(runtime) = self.mode_runtime.as_deref() else { return };
        if let Err(e) = write_mode(runtime, mode) {
            log(&format!("appearance: cannot write {} ({e})", mode_path(runtime).display()));
        }
        self.mode_written = Some(mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::bus::testing::FakeOutbox;

    #[test]
    fn the_rule_names_one_signal_of_the_portal_for_one_key() {
        assert_eq!(
            SETTING_RULE,
            format!(
                "type='signal',sender='{PORTAL_NAME}',path='{PORTAL_PATH}',interface='{SETTINGS_INTERFACE}',member='SettingChanged',arg0='{APPEARANCE_NAMESPACE}',arg1='{COLOR_SCHEME_KEY}'"
            )
        );
    }

    #[test]
    fn start_sends_the_rule_then_one_read_one() {
        let mut out = FakeOutbox::default();
        let a = Appearance::start(&mut out);
        assert_eq!(out.sent, [Call::AddMatch { rule: SETTING_RULE.into() }], "the rule: no reply expected, nothing pending");
        assert_eq!(out.calls, [(CallId(1), Call::ReadOne)], "one call, pending with its deadline");
        assert!(a.owns(CallId(1)));
        assert_eq!(a.portal(), None, "nothing known before the answer: effective_mode reads that as dark");
    }

    #[test]
    fn the_answer_gives_the_mode_once() {
        let mut out = FakeOutbox::default();
        let mut a = Appearance::start(&mut out);
        assert_eq!(a.answer(CallId(1), Ok(Reply::ColorScheme(1))), Some(Mode::Dark));
        assert_eq!(a.portal(), Some(Mode::Dark));
        assert!(!a.owns(CallId(1)), "answered: nothing left to wait for");
        assert_eq!(a.answer(CallId(1), Ok(Reply::ColorScheme(2))), None, "the same id again is not ours any more");
        assert_eq!(a.portal(), Some(Mode::Dark));
    }

    #[test]
    fn signals_change_the_mode_only_when_it_changes() {
        let mut a = Appearance::default();
        assert_eq!(a.changed(2), Some(Mode::Light));
        assert_eq!(a.changed(2), None, "the same value again: nothing to repaint");
        assert_eq!(a.changed(0), Some(Mode::Dark), "0, no preference, is dark");
        assert_eq!(a.changed(7), None, "an unknown value is dark too: unchanged");
    }

    #[test]
    fn a_failed_read_leaves_no_mode_and_nothing_pending() {
        for e in [
            CallError::Timeout,
            CallError::Disconnected,
            CallError::Remote("org.freedesktop.DBus.Error.ServiceUnknown".into()),
            CallError::BadReply("color-scheme is Some(\"s\")".into()),
        ] {
            let mut out = FakeOutbox::default();
            let mut a = Appearance::start(&mut out);
            assert_eq!(a.answer(CallId(1), Err(e.clone())), None, "{e:?}");
            assert_eq!(a.portal(), None, "{e:?}");
            assert!(!a.owns(CallId(1)), "{e:?}: nothing pending");
            assert_eq!(effective_mode(ModeSetting::Auto, a.portal()), Mode::Dark, "{e:?}");
        }
    }

    #[test]
    fn another_call_is_not_ours() {
        let mut out = FakeOutbox::default();
        out.call(Call::ListNames);
        let mut a = Appearance::start(&mut out);
        assert!(!a.owns(CallId(1)));
        assert_eq!(a.answer(CallId(1), Ok(Reply::ColorScheme(2))), None);
        assert_eq!(a.portal(), None);
    }

    #[test]
    fn the_portal_starts_only_in_auto_and_once() {
        use PortalStart::*;
        assert_eq!(portal_start(ModeSetting::Dark, false, false, false), Nothing);
        assert_eq!(portal_start(ModeSetting::Light, false, true, true), Nothing);
        assert_eq!(portal_start(ModeSetting::Auto, true, false, false), Nothing, "tried once: never again");
        assert_eq!(portal_start(ModeSetting::Auto, false, true, true), OnTray, "one connection: the tray's");
        assert_eq!(portal_start(ModeSetting::Auto, false, false, false), OwnBus);
        assert_eq!(portal_start(ModeSetting::Auto, false, true, false), NoBus, "the tray's bus failed or closed: no second attempt");
    }

    #[test]
    fn a_repaint_only_when_the_effective_mode_changes() {
        assert!(!repaint_needed(ModeSetting::Auto, None, Mode::Dark), "no portal was already dark");
        assert!(repaint_needed(ModeSetting::Auto, None, Mode::Light));
        assert!(repaint_needed(ModeSetting::Auto, Some(Mode::Light), Mode::Dark));
        assert!(!repaint_needed(ModeSetting::Dark, None, Mode::Light), "a fixed mode ignores the portal");
        assert!(!repaint_needed(ModeSetting::Light, Some(Mode::Dark), Mode::Light));
    }

    #[test]
    fn the_mode_is_published_once_then_on_each_change() {
        assert_eq!(mode_to_publish(None, Mode::Dark), Some(Mode::Dark), "startup: dark is written too");
        assert_eq!(mode_to_publish(Some(Mode::Dark), Mode::Dark), None, "unchanged: nothing written");
        assert_eq!(mode_to_publish(Some(Mode::Dark), Mode::Light), Some(Mode::Light));
    }

    #[test]
    fn the_mode_file_is_written_whole_in_an_owner_only_directory() {
        use alpheus_commun::theme::mode_file::read_mode;
        use std::os::unix::fs::PermissionsExt;
        let run = std::env::temp_dir().join(format!("alpheus-{}-mode-write", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        std::fs::create_dir_all(&run).unwrap();
        write_mode(&run, Mode::Light).unwrap();
        let dir = run.join("alpheus");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "owner-only");
        assert_eq!(std::fs::read_to_string(dir.join("mode")).unwrap(), "light\n");
        write_mode(&run, Mode::Dark).unwrap();
        assert_eq!(read_mode(Some(&run)), Mode::Dark, "replaced");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["mode"], "no temporary file left behind");
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_failed_write_is_an_error_never_a_panic() {
        let run = std::env::temp_dir().join(format!("alpheus-{}-mode-blocked", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("alpheus"), "a file where the directory should be").unwrap();
        assert!(write_mode(&run, Mode::Dark).is_err());
        let _ = std::fs::remove_dir_all(&run);
    }
}
