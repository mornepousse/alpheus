//! The desktop's light/dark preference (themes spec § 2–3). With `mode = "auto"`, the portal's
//! `org.freedesktop.appearance` `color-scheme` is read once (`ReadOne`: 2 s deadline, auto-start
//! off), then followed (`SettingChanged`, through one match rule narrowed to that key).
//!
//! It rides the tray's session-bus connection when the tray module is on. Otherwise it rides the
//! bar's own session link (link.rs), which it may share with the media module. Either way there is
//! one connection.
//!
//! Calls and decoding reuse the tray's typed allowlist (`Call::ReadOne`, `codec`): one pending
//! table, one deadline rule, and this file never names the dbus crate. Never name a function
//! `method_call` here: the guardrails count that substring.

use crate::link::{Host, LinkOutput, LinkUser, host};
use crate::log::write as log;
use crate::tray::bus::{Call, CallError, CallId, Outbox};
use crate::tray::codec::Reply;
use crate::tray::router::TrayOutput;
use crate::wayland::App;
use alpheus_commun::theme::Mode;
use alpheus_commun::theme::mode_file::{MODE_DIR, mode_path, mode_text};
use alpheus_commun::theme::settings::{ModeSetting, effective_mode, portal_mode};
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

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

/// Whether `dir` (`runtime/alpheus`) may hold the mode file: a real directory (never a symlink),
/// owned by us, closed to group and others. Missing is fine (it is created 0700).
fn dir_is_ours(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    // SAFETY: geteuid(2) cannot fail.
    let me = unsafe { libc::geteuid() };
    if !meta.is_dir() {
        Err(io::Error::other(format!("{} is not a real directory", dir.display())))
    } else if meta.uid() != me {
        Err(io::Error::other(format!("{} is not ours", dir.display())))
    } else if meta.mode() & 0o077 != 0 {
        Err(io::Error::other(format!("{} is open to others", dir.display())))
    } else {
        Ok(())
    }
}

/// The mode in effect to `runtime/alpheus/mode`, for the TUI. The directory is created owner-only
/// (0700) when missing and refused when it is a symlink, someone else's or open to others; the text
/// goes to a fresh temporary file in that directory (a stale one removed first, never followed),
/// then is renamed over the old one, so a reader sees the old text or the new one, never half of
/// one. Err: the caller logs one line.
pub fn write_mode(runtime: &Path, mode: Mode) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = runtime.join(MODE_DIR);
    dir_is_ours(&dir)?;
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    dir_is_ours(&dir)?;
    let tmp = dir.join(format!(".mode.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).custom_flags(libc::O_NOFOLLOW).mode(0o600).open(&tmp)?;
    let written = file.write_all(mode_text(mode).as_bytes());
    drop(file);
    written.and_then(|()| std::fs::rename(&tmp, mode_path(runtime))).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// One publication: writes `now` when it differs from `written` and a runtime directory exists.
/// `written` moves only on success (a failed write is tried again at the next change); a failure is
/// logged once per distinct error (`last_error`), never once per toggle.
pub fn publish(
    runtime: Option<&Path>,
    written: &mut Option<Mode>,
    last_error: &mut Option<String>,
    now: Mode,
    write: impl Fn(&Path, Mode) -> io::Result<()>,
    mut say: impl FnMut(&str),
) {
    let Some(mode) = mode_to_publish(*written, now) else { return };
    let Some(runtime) = runtime else { return };
    match write(runtime, mode) {
        Ok(()) => {
            *written = Some(mode);
            *last_error = None;
        }
        Err(e) => {
            let text = format!("appearance: cannot write {} ({e})", mode_path(runtime).display());
            if last_error.as_deref() != Some(text.as_str()) {
                say(&text);
                *last_error = Some(text);
            }
        }
    }
}

/// Whether the portal is to be followed now: mode `auto`, not yet tried.
pub fn portal_wanted(mode: ModeSetting, tried: bool) -> bool {
    mode == ModeSetting::Auto && !tried
}

/// Whether the portal moving to `after` changes the mode in effect (a repaint), from `before`.
pub fn repaint_needed(setting: ModeSetting, before: Option<Mode>, after: Mode) -> bool {
    effective_mode(setting, before) != effective_mode(setting, Some(after))
}

impl App {
    /// With `mode = "auto"`, follows the portal once. It goes on the tray's connection when the tray
    /// runs; otherwise on the bar's link, opened here or joined (the media module may have opened it).
    /// Called after `start_tray` and `start_media` at startup, and in `theme_changed` right after the
    /// new settings are stored. Either way one connection serves everything.
    pub(crate) fn follow_portal_if_auto(&mut self) {
        match host(portal_wanted(self.theme_settings.mode, self.portal_tried), self.buses()) {
            Host::Nothing => return,
            Host::OnTray => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.start_appearance();
                }
                // Its ReadOne is pending: the tray's deadline timer is armed for it.
                self.tray_output(TrayOutput::default());
            }
            Host::NoBus => log("appearance: no session bus; dark"),
            Host::OnLink | Host::OwnBus => {
                if self.link.is_some() || self.open_link(LinkUser::Appearance) {
                    if let Some(link) = self.link.as_mut() {
                        link.start_appearance();
                    }
                    // Its ReadOne is pending: the link's deadline timer is armed for it.
                    self.link_output(LinkOutput::default());
                }
            }
        }
        self.portal_tried = true;
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
        publish(self.mode_runtime.as_deref(), &mut self.mode_written, &mut self.mode_error, now, write_mode, log);
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
    fn the_portal_is_wanted_only_in_auto_and_once() {
        assert!(!portal_wanted(ModeSetting::Dark, false));
        assert!(!portal_wanted(ModeSetting::Light, false));
        assert!(!portal_wanted(ModeSetting::Auto, true), "tried once: never again");
        assert!(portal_wanted(ModeSetting::Auto, false));
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
    fn a_directory_open_to_others_or_a_symlink_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let run = std::env::temp_dir().join(format!("alpheus-{}-mode-unsafe", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        std::fs::create_dir_all(run.join("alpheus")).unwrap();
        std::fs::set_permissions(run.join("alpheus"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(write_mode(&run, Mode::Light).is_err(), "0755 refused");
        assert!(!run.join("alpheus/mode").exists());
        std::fs::set_permissions(run.join("alpheus"), std::fs::Permissions::from_mode(0o700)).unwrap();
        write_mode(&run, Mode::Light).unwrap();
        std::fs::remove_dir_all(run.join("alpheus")).unwrap();
        let elsewhere = run.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, run.join("alpheus")).unwrap();
        assert!(write_mode(&run, Mode::Light).is_err(), "a symlinked directory refused");
        assert!(!elsewhere.join("mode").exists());
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_stale_temporary_symlink_is_never_followed() {
        let run = std::env::temp_dir().join(format!("alpheus-{}-mode-stale", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        std::fs::create_dir_all(&run).unwrap();
        write_mode(&run, Mode::Dark).unwrap();
        let victim = run.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, run.join(format!("alpheus/.mode.{}", std::process::id()))).unwrap();
        write_mode(&run, Mode::Light).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(run.join("alpheus/mode")).unwrap(), "light\n");
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_failed_write_is_retried_and_logged_once_per_error() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let (mut written, mut error) = (None, None);
        let fail = |_: &Path, _: Mode| {
            calls.set(calls.get() + 1);
            Err(io::Error::other("disk"))
        };
        let run = Path::new("/run/x");
        let mut lines = 0;
        publish(Some(run), &mut written, &mut error, Mode::Dark, fail, |_| lines += 1);
        assert_eq!((written, calls.get(), lines), (None, 1, 1), "not marked written");
        publish(Some(run), &mut written, &mut error, Mode::Dark, fail, |_| lines += 1);
        assert_eq!((calls.get(), lines), (2, 1), "tried again, not logged again");
        let first = error.clone();
        assert!(first.is_some());
        publish(Some(run), &mut written, &mut error, Mode::Light, fail, |_| lines += 1);
        assert_eq!(lines, 1, "the same error on the next toggle: still one line");
        assert_eq!(error, first, "same error: same record, not re-logged");
        publish(Some(run), &mut written, &mut error, Mode::Light, |_, _| Ok(()), |_| lines += 1);
        assert_eq!((written, error.clone()), (Some(Mode::Light), None), "success marks and clears");
    }

    #[test]
    fn no_runtime_directory_writes_nothing() {
        let (mut written, mut error) = (None, None);
        publish(None, &mut written, &mut error, Mode::Light, |_, _| panic!("no runtime: no write"), |_| panic!("nothing logged"));
        assert_eq!((written, error), (None, None));
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
