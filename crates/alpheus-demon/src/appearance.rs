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
use crate::state_file;
use crate::tray::bus::{Call, CallError, CallId, Outbox};
use crate::tray::codec::Reply;
use crate::tray::router::TrayOutput;
use crate::wayland::App;
use alpheus_commun::theme::Mode;
use alpheus_commun::theme::mode_file::{MODE_DIR, MODE_FILE, mode_path, mode_text};
use alpheus_commun::theme::settings::{ModeSetting, effective_mode, portal_mode};
use std::io;
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

/// The mode in effect to `runtime/alpheus/mode`, for the TUI, through the hardened writer
/// (`state_file::write_private`): an owner-only directory, a fresh temporary file never followed,
/// renamed over the old one, so a reader sees the old text or the new one. Err: the caller logs
/// one line.
pub fn write_mode(runtime: &Path, mode: Mode) -> io::Result<()> {
    state_file::write_private(&runtime.join(MODE_DIR), MODE_FILE, mode_text(mode).as_bytes())
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
mod tests;
