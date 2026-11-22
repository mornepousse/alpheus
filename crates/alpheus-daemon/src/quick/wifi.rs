//! The quick panel's Wi-Fi: the row, the list of networks behind it, the password field and the
//! connection attempt.
//!
//! This is the pure half of the Wi-Fi list; the wiring is in `app.rs`. It holds the row's glyph and
//! value, the detail's networks (one per SSID, at its strongest access point, ordered, at most
//! `NETWORKS_MAX`), the keys and clicks of the detail, the exact `nmcli` commands they ask for (as
//! `QuickAction`s), the password field (its bytes live in `secret.rs`) and the connection attempt
//! (`Attempt`), which NetworkManager's states and the clock follow to its end. The states come from
//! the D-Bus thread (`sources/dbus/wifi.rs`) only while the panel is open, or while an attempt on a
//! new network is still followed after the panel closed (`WifiFollow` in `app.rs`). `mod.rs` hands
//! this file its turn and the helpers the lists share.
//!
//! Never:
//! - write or create a file, or make a raw system call (guardrail
//!   `the_quick_model_and_its_state_reader_only_read`);
//! - run a command or start a thread (guardrail `the_tray_spawns_no_thread`, through its
//!   `NO_THREAD_NOR_COMMAND` list), or arm a timer (the table of timer and thread tokens in
//!   `nothing_resident_while_the_launcher_is_closed`): a command is handed back as a
//!   `QuickAction`, and the wiring arms the panel's one timer from `Quick::deadline`, which
//!   includes `Wifi::deadline` (after a close, `WifiFollow::deadline`);
//! - format the password, a key or the field with `Debug` (guardrail
//!   `the_wifi_password_goes_to_stdin_only_and_is_wiped_after`);
//! - panic (guardrail `the_quick_panel_has_no_panicking_path`);
//! - read a clock: the caller passes `now` (no guardrail checks it).
//!
//! See: docs/specs/2026-10-11-alpheus-quick-settings-wifi-design.md, docs/features/quick-settings.md,
//! and docs/decisions.md ("A Wi-Fi attempt on a new network outlives the quick panel", "The Wi-Fi
//! password lives in one boxed buffer that is wiped", "An SSID that nmcli would misread is never
//! passed to it", "A failed Wi-Fi attempt deletes its new profile only when sure", "A row under
//! connection reads `…`").

use crate::actions::Button;
use crate::config::QuickConfig;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::keys::Key;
use crate::list;
pub use crate::quick::FAILED_SHOWN;
use crate::quick::secret::Secret;
use crate::quick::{FAILED_TAG, LOCK, Line, QuickAction, RUNNING_TAG, argv, moved, nav, note_row, run, text_row};
use crate::session;
use crate::system::{AccessPoint, SavedWifi, Security, WifiDevice, WifiScope, WifiState};
use crate::text;
use std::cmp::Reverse;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

/// The most networks the detail lists (quick Wi-Fi spec § 2).
pub const NETWORKS_MAX: usize = 32;
/// NMDeviceState 100: the device is activated (connected).
pub const ACTIVATED: u32 = 100;
/// The row's value: at most this many cells of the connected SSID.
pub const VALUE_CELLS: u16 = 16;
/// The row's glyphs, from Nerd Font's Material Design set: `wifi-strength-1` to `wifi-strength-4`
/// for the connected network's four signal steps.
pub const STRENGTH: [char; 4] = ['\u{f091f}', '\u{f0922}', '\u{f0925}', '\u{f0928}'];
/// `wifi-strength-outline`: the radio is on and the device is not connected.
pub const WIFI_ON: char = '\u{f092f}';
/// `wifi-strength-off-outline`: the radio is off.
pub const WIFI_OFF: char = '\u{f092e}';
/// The detail's notes, shown in place of the list.
pub const OFF_NOTE: &str = "Wi-Fi is off";
pub const SEARCHING: &str = "Searching…";
pub const NO_NETWORKS: &str = "No networks";
/// The row's value on a connected network whose SSID shows nothing.
pub const HIDDEN: &str = "hidden";
/// NMDeviceState 40 to 90, from "prepare" to "secondaries": an activation is under way.
pub const ACTIVATING: RangeInclusive<u32> = 40..=90;
/// NMDeviceState 120: the activation failed.
pub const FAILED: u32 = 120;
/// No word from NetworkManager this long after Enter: failed (quick Wi-Fi spec § 2).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// An attempt still activating on its network at `CONNECT_TIMEOUT` waits up to this long from its
/// start, because a slow activation may outlast NetworkManager's default 45 s DHCP timeout (commit
/// b2223e0); then it ends with its profile kept.
pub const ACTIVATION_BOUND: Duration = Duration::from_secs(60);
/// How long an open password field outlives its network leaving the list: an access point that
/// aged out while the password was being typed.
pub const FIELD_KEPT: Duration = Duration::from_secs(10);
/// The password's first cell: after "Password" and two spaces.
pub const PASSWORD_AT: u16 = 13;
/// The field after a character that did not fit.
pub const TOO_LONG: &str = "too long";
/// Why an attempt failed, as the one log line says it. Fixed words, so an SSID never reaches the
/// log.
const ACTIVATION_FAILED: &str = "the activation failed";
const NO_ANSWER: &str = "no answer in 20 s";
const STILL_ACTIVATING: &str = "still activating after 60 s";
/// Why an attempt ended with NetworkManager gone. `pub(crate)`: visible inside the daemon only (the
/// VPN list ends its attempt with the same words).
pub(crate) const NM_GONE: &str = "NetworkManager went away";
/// Why an attempt failed on a state kept through another domain's outage: its profile is kept. See
/// docs/decisions.md, "A failed Wi-Fi attempt deletes its new profile only when sure".
const STALE: &str = "no fresh state since an outage";
/// The four bars of a network row, from the weakest step to the strongest.
const BARS: [char; 4] = ['▂', '▄', '▆', '█'];
/// A saved network's tag.
const SAVED_TAG: &str = "saved";

/// 1 to 4 steps of signal: 0–24 %, 25–49 %, 50–74 %, 75–100 %.
pub fn steps(strength: u8) -> usize {
    1 + usize::from(strength >= 25) + usize::from(strength >= 50) + usize::from(strength >= 75)
}

/// "▂▄▆█", "▂▄▆_", "▂▄__", "▂___".
pub fn bars(strength: u8) -> String {
    (0..BARS.len()).map(|i| if i < steps(strength) { BARS[i] } else { '_' }).collect()
}

/// An SSID as shown: lossy UTF-8, cleaned like any text from outside.
pub fn label(ssid: &[u8]) -> String {
    text::clean(&String::from_utf8_lossy(ssid))
}

/// An SSID the detail cannot show: zeros only (a hidden network's), or nothing left once cleaned.
pub fn hidden(ssid: &[u8]) -> bool {
    ssid.iter().all(|b| *b == 0) || label(ssid).is_empty()
}

/// An SSID that cannot be one nmcli argument as it is: empty, holding a NUL, starting with `-`
/// (read as an option), not UTF-8, or one nmcli reads as something else: `h`, `he`, `hel`, `help`
/// (the word help), or six hexadecimal groups (a BSSID). See docs/decisions.md, "An SSID that nmcli
/// would misread is never passed to it".
pub fn refused(ssid: &[u8]) -> bool {
    ssid.is_empty()
        || ssid.contains(&0)
        || ssid.first() == Some(&b'-')
        || std::str::from_utf8(ssid).is_err()
        || b"help".starts_with(ssid)
        || looks_like_bssid(ssid)
}

/// Six groups of one or two hexadecimal digits, joined by ':' or '-' (as libnm parses a BSSID).
fn looks_like_bssid(ssid: &[u8]) -> bool {
    // Tried once for each separator; `groups` counts the groups seen while `all` checks each one.
    b":-".iter().any(|sep| {
        let mut groups = 0;
        ssid.split(|b| b == sep).all(|g| {
            groups += 1;
            (1..=2).contains(&g.len()) && g.iter().all(u8::is_ascii_hexdigit)
        }) && groups == 6
    })
}

/// Whether `what` (the short word an action carries for the log) is one of the actions that start
/// an attempt (`Wifi::enter` and `send_password` build them).
pub fn starts_attempt(what: &str) -> bool {
    what.starts_with("wifi connect")
}

/// `[quick] wifi_more`, resolved at startup; unresolved (what a test's default config gives) or
/// empty: there is no `More…`.
pub fn more(config: &QuickConfig) -> &[String] {
    config.wifi_more.as_deref().unwrap_or(&[])
}

/// `More…`: `wifi_more` as given, then the panel closes; nothing when it is empty.
fn run_more(config: &QuickConfig) -> QuickAction {
    crate::quick::run_and_close(more(config), "wifi more")
}

/// What Enter does on a network (quick Wi-Fi spec § 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The one the device is on: disconnect, by its active connection's uuid (None: not known).
    Connected(Option<String>),
    /// A saved profile (by uuid): whatever its SSID.
    Saved(String),
    /// No key, an SSID nmcli can take.
    Open,
    /// WPA personal, not saved: the password field.
    Secured,
    /// 802.1X, WEP, other, or an SSID that cannot be an argument: `More…` instead.
    Unsupported,
}

/// One row of the detail: an SSID at its strongest access point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub ssid: Vec<u8>,
    pub label: String,
    pub strength: u8,
    pub security: Security,
    /// The first saved profile's uuid for this SSID.
    pub saved: Option<String>,
    pub connected: bool,
}

/// The detail's selection, kept by identity (the SSID), not by position: the list reorders as
/// signals change.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pick {
    #[default]
    None,
    Network(Vec<u8>),
    /// The password field, under its network.
    Password,
    More,
}

/// The password field, under the network `ap` (quick Wi-Fi spec § 3).
///
/// Its `Debug` is derived, which is safe: `Secret` has its own `Debug` that shows no byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Its network as listed when the field opened: listed still while the field waits for it.
    pub ap: AccessPoint,
    pub secret: Secret,
    /// A character did not fit: Enter is refused until Ctrl+U.
    pub overflow: bool,
    /// Since when its network is missing from the access points read: FIELD_KEPT later, the field
    /// goes.
    pub missing: Option<Instant>,
}

/// A connection under way (quick Wi-Fi spec § 2), followed by the panel (`Wifi`) or, once the panel
/// closed, by `WifiFollow` in `app.rs`: it needs nothing of the panel, only the states read and the
/// clock. See docs/decisions.md, "A Wi-Fi attempt on a new network outlives the quick panel".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub ssid: Vec<u8>,
    /// Enter, when it began.
    pub started: Instant,
    /// No word by then: failed (or, still activating on its network, ACTIVATION_BOUND from `started`).
    pub until: Instant,
    /// An activating state was seen: leaving it for anything but activated on this SSID is a failure.
    pub activating: bool,
    /// The saved profiles' uuids when it began (None: not read): a profile that existed then is
    /// never deleted.
    pub known: Option<Vec<String>>,
    /// The profile that appeared for this SSID during the attempt.
    pub created: Option<String>,
    /// `nmcli device wifi connect` (an open network, a password), which may leave a new profile
    /// behind: followed after the panel closes. A saved network's `connection up` is not.
    pub creates: bool,
}

/// Where an attempt stands after a state or the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Waiting,
    /// Activated on its SSID: done.
    Connected,
    /// NetworkManager (or its Wi-Fi device) went: nothing left to clean.
    Gone,
    /// Why, for the log, and the cleanup to run: its new profile, while still saved.
    Failed {
        why: &'static str,
        cleanup: Option<QuickAction>,
    },
}

/// The Wi-Fi device in `state`, if there is a state and a device.
fn device_in(state: Option<&WifiState>) -> Option<&WifiDevice> {
    // `?` after an `Option` ends the function with `None` when there is nothing.
    state?.device.as_ref()
}

/// The saved profiles in `state`: `None` when there is no device or the list was not read (it is
/// read only while the detail is open).
fn saved_in(state: Option<&WifiState>) -> Option<&Vec<SavedWifi>> {
    device_in(state)?.saved.as_ref()
}

impl Attempt {
    /// Enter on `ssid` at `now`, with `state` the last read: the saved profiles' uuids are noted
    /// (`known`), so that a profile that existed before the attempt is never taken for its own.
    pub fn new(ssid: &[u8], creates: bool, state: Option<&WifiState>, now: Instant) -> Attempt {
        let known = saved_in(state).map(|s| s.iter().map(|p| p.uuid.clone()).collect());
        Attempt { ssid: ssid.to_vec(), started: now, until: now + CONNECT_TIMEOUT, activating: false, known, created: None, creates }
    }

    /// NetworkManager's word: activated on its SSID ends it; an activating state on its SSID is
    /// noted (one on another network, an autoconnect, is not ours); failed (120), or, once ours was
    /// activating, any other state but one whose access point could not be read, is a failure;
    /// NetworkManager gone ends it. The new profile is the first saved for its SSID that was not
    /// there when it began, both lists read whole (`known` None: never one).
    pub fn observe(&mut self, state: Option<&WifiState>) -> Outcome {
        let Some(d) = device_in(state) else { return Outcome::Gone };
        if self.created.is_none()
            && let (Some(known), Some(saved)) = (&self.known, &d.saved)
        {
            self.created = saved.iter().find(|p| p.ssid == self.ssid && !known.contains(&p.uuid)).map(|p| p.uuid.clone());
        }
        // Activated on the profile it made, its access point unread: connected all the same.
        if d.state == ACTIVATED && d.active_uuid.is_some() && d.active_uuid == self.created {
            return Outcome::Connected;
        }
        let on = d.active.as_ref().map(|ap| ap.ssid == self.ssid);
        if d.state == FAILED {
            return self.failure(state, ACTIVATION_FAILED);
        }
        if d.state == ACTIVATED || ACTIVATING.contains(&d.state) {
            match on {
                Some(true) if d.state == ACTIVATED => return Outcome::Connected,
                Some(true) => self.activating = true,
                // Another network took the device after ours was under way.
                Some(false) if self.activating => return self.failure(state, ACTIVATION_FAILED),
                // Its access point unread, or not ours before ours began: no word yet.
                _ => {}
            }
            return Outcome::Waiting;
        }
        if self.activating {
            return self.failure(state, ACTIVATION_FAILED);
        }
        Outcome::Waiting
    }

    /// The clock. Before `until`, nothing. At `until`, with the device still activating on this
    /// network (or on an access point not read), the activation is slow, not failed: the deadline
    /// moves to `ACTIVATION_BOUND` from the start, and there the attempt ends with nothing deleted.
    /// Otherwise no word came in time: failed, and nothing deleted either when the device was last
    /// seen activated on what may be the new profile.
    pub fn expired(&mut self, state: Option<&WifiState>, now: Instant) -> Outcome {
        if self.until > now {
            return Outcome::Waiting;
        }
        if self.still_activating(state) {
            let bound = self.started + ACTIVATION_BOUND;
            if now < bound {
                self.until = bound;
                return Outcome::Waiting;
            }
            return Outcome::Failed { why: STILL_ACTIVATING, cleanup: None };
        }
        let outcome = self.failure(state, NO_ANSWER);
        if let Outcome::Failed { why, .. } = outcome
            && self.may_be_on_its_profile(state)
        {
            return Outcome::Failed { why, cleanup: None };
        }
        outcome
    }

    /// The device is mid-activation on this attempt's network, or on an access point not read.
    fn still_activating(&self, state: Option<&WifiState>) -> bool {
        let Some(d) = device_in(state) else { return false };
        if !ACTIVATING.contains(&d.state) {
            return false;
        }
        match &d.active {
            Some(ap) => ap.ssid == self.ssid,
            None => true,
        }
    }

    /// The device is activated on what may be the profile this attempt made: its active connection
    /// is that profile, or (its uuid not read) its access point is this network's or unread. Never
    /// true when the device is shown to be on another network.
    fn may_be_on_its_profile(&self, state: Option<&WifiState>) -> bool {
        let Some(d) = device_in(state) else { return false };
        if d.state != ACTIVATED {
            return false;
        }
        match &d.active_uuid {
            Some(uuid) => self.created.as_ref() == Some(uuid),
            None => match &d.active {
                Some(ap) => ap.ssid == self.ssid,
                None => true,
            },
        }
    }

    /// Failed with `why`. The cleanup deletes the profile that appeared for this network during the
    /// attempt, only when the attempt could make one (`nmcli device wifi connect`, never a
    /// `connection up`) and while `state` still lists it (nmcli may have deleted it itself). On a
    /// state kept through another domain's outage nothing is deleted, because NetworkManager may
    /// have connected since; the log says so (`STALE`, when a profile would otherwise have been
    /// deleted).
    fn failure(&self, state: Option<&WifiState>, why: &'static str) -> Outcome {
        let mut cleanup = None;
        if self.creates
            && let Some(uuid) = &self.created
            && is_saved(state, uuid)
        {
            cleanup = Some(run(&["nmcli", "connection", "delete", "uuid", uuid], "wifi delete the failed profile"));
        }
        if cleanup.is_some() && state.is_some_and(|s| s.stale) {
            return Outcome::Failed { why: STALE, cleanup: None };
        }
        Outcome::Failed { why, cleanup }
    }
}

/// `uuid` is among the saved profiles in `state`.
fn is_saved(state: Option<&WifiState>, uuid: &str) -> bool {
    match saved_in(state) {
        Some(saved) => saved.iter().any(|p| p.uuid == uuid),
        None => false,
    }
}

/// The panel's Wi-Fi, one field of `Quick`: dropped whole with the panel (an attempt that must
/// outlive it is first handed to `WifiFollow`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Wifi {
    /// What the D-Bus thread last read; None: nothing yet, or NetworkManager gone.
    pub state: Option<WifiState>,
    /// The detail shows in place of the rows.
    pub open: bool,
    pub pick: Pick,
    /// Opened from the network module or `--quick wifi`: the first state opens the detail.
    pub focus_pending: bool,
    /// The password field, while it is open.
    pub field: Option<Field>,
    /// The connection under way.
    pub attempt: Option<Attempt>,
    /// The network whose attempt just failed, tagged `failed` until that instant.
    pub failed: Option<(Vec<u8>, Instant)>,
    /// The detail was waiting for its list (`fit` sets it): a `More…` chosen meanwhile gives way to
    /// the first network once the list arrives.
    pub searching: bool,
}

impl Wifi {
    /// The Wi-Fi device in the last state read, if there is one.
    fn device(&self) -> Option<&WifiDevice> {
        self.state.as_ref()?.device.as_ref()
    }

    /// The row shows: a Wi-Fi device.
    pub fn shows(&self) -> bool {
        self.device().is_some()
    }

    /// The radio is on (`WirelessEnabled`); false until a state is read.
    pub fn enabled(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.enabled)
    }

    /// The network the device is on: activated, with an access point.
    pub fn connected(&self) -> Option<&AccessPoint> {
        let d = self.device()?;
        if d.state == ACTIVATED { d.active.as_ref() } else { None }
    }

    /// The row's glyph: off, the connected network's signal, or on without a connection.
    pub fn glyph(&self) -> char {
        // The first arm has no pattern to test: its guard, the radio being off, comes before the
        // connected network is looked at.
        match self.connected() {
            _ if !self.enabled() => WIFI_OFF,
            Some(ap) => STRENGTH[steps(ap.strength) - 1],
            None => WIFI_ON,
        }
    }

    /// The row's value: the connected SSID, "on" or "off" (quick Wi-Fi spec § 1).
    pub fn value(&self) -> String {
        // The radio off comes first, as in `glyph`.
        match self.connected() {
            _ if !self.enabled() => "off".into(),
            Some(ap) if hidden(&ap.ssid) => HIDDEN.into(),
            Some(ap) => truncate(&label(&ap.ssid), VALUE_CELLS),
            None => "on".into(),
        }
    }

    /// The detail's networks (quick Wi-Fi spec § 2): one per SSID, at its strongest access point;
    /// the connected network is included even when the list misses it, and so is the open field's
    /// for `FIELD_KEPT` once the list misses it; hidden ones are left out. The order is the
    /// connected one, then the saved ones by signal, then the others by signal (then by label), cut
    /// to `NETWORKS_MAX`. Empty with the radio off.
    pub fn networks(&self) -> Vec<Network> {
        let Some(d) = self.device() else { return Vec::new() };
        if !self.enabled() {
            return Vec::new();
        }
        let connected = self.connected().map(|a| a.ssid.as_slice());
        let mut best: Vec<&AccessPoint> = Vec::new();
        // The access points read (`iter().flatten()` yields nothing when they were not read), then the
        // connected one, so that it is listed even when the list misses it.
        for ap in d.access_points.iter().flatten().chain(self.connected()) {
            if hidden(&ap.ssid) {
                continue;
            }
            match best.iter_mut().find(|b| b.ssid == ap.ssid) {
                Some(b) if ap.strength > b.strength => *b = ap,
                Some(_) => {}
                None => best.push(ap),
            }
        }
        if let Some(f) = &self.field
            && !best.iter().any(|b| b.ssid == f.ap.ssid)
        {
            best.push(&f.ap);
        }
        let saved = |ssid: &[u8]| d.saved.iter().flatten().find(|s| s.ssid == ssid).map(|s| s.uuid.clone());
        let mut v: Vec<Network> = best
            .into_iter()
            .map(|ap| Network {
                ssid: ap.ssid.clone(),
                label: label(&ap.ssid),
                strength: ap.strength,
                security: ap.security,
                saved: saved(&ap.ssid),
                connected: connected == Some(ap.ssid.as_slice()),
            })
            .collect();
        // Each network's sort key is a tuple, compared field by field: `false` sorts before `true`,
        // so the connected and then the saved come first; `Reverse` puts the strongest first.
        v.sort_by(|a, b| {
            (!a.connected, a.saved.is_none(), Reverse(a.strength), &a.label, &a.ssid).cmp(&(
                !b.connected,
                b.saved.is_none(),
                Reverse(b.strength),
                &b.label,
                &b.ssid,
            ))
        });
        v.truncate(NETWORKS_MAX);
        v
    }

    /// What Enter does on `n`: by its state first (connected, saved), then by whether nmcli can take
    /// its SSID, then by its security.
    pub fn kind(&self, n: &Network) -> Kind {
        if n.connected {
            return Kind::Connected(self.device().and_then(|d| d.active_uuid.clone()));
        }
        if let Some(uuid) = &n.saved {
            return Kind::Saved(uuid.clone());
        }
        if refused(&n.ssid) {
            return Kind::Unsupported;
        }
        match n.security {
            Security::Open => Kind::Open,
            Security::Psk => Kind::Secured,
            Security::Enterprise | Security::Other => Kind::Unsupported,
        }
    }

    /// Space on the row: the radio the other way.
    pub fn radio(&self) -> QuickAction {
        if self.enabled() { run(&["nmcli", "radio", "wifi", "off"], "wifi off") } else { run(&["nmcli", "radio", "wifi", "on"], "wifi on") }
    }

    /// Enter, →, a click on the row: the detail, with one rescan when the radio is on.
    pub fn open_detail(&mut self) -> QuickAction {
        self.open = true;
        self.pick = Pick::None;
        if self.enabled() { run(&["nmcli", "device", "wifi", "rescan"], "wifi rescan") } else { QuickAction::Redraw }
    }

    /// Back to the rows: the field closes, its buffer wiped.
    pub fn close_detail(&mut self) {
        self.open = false;
        self.searching = false;
        self.pick = Pick::None;
        self.field = None;
    }

    /// What the D-Bus thread is to read for the open panel: the detail's lists while it is open, or
    /// while an attempt runs (its new profile must be seen).
    pub fn scope(&self) -> WifiScope {
        if self.open || self.attempt.is_some() { WifiScope::Detail } else { WifiScope::Row }
    }

    /// The D-Bus thread's state (`Quick::set_wifi`): kept, the field's network looked for, and the
    /// attempt followed (`Attempt::observe`). Returns the action to run (the cleanup of a failed
    /// attempt, else a repaint) and, when the attempt failed or went, why (for the log).
    pub fn set_state(&mut self, state: Option<WifiState>, now: Instant) -> (QuickAction, Option<&'static str>) {
        self.state = state;
        self.follow_field(now);
        let Some(a) = self.attempt.as_mut() else { return (QuickAction::Redraw, None) };
        match a.observe(self.state.as_ref()) {
            Outcome::Waiting => (QuickAction::Redraw, None),
            Outcome::Connected => {
                self.attempt = None;
                (QuickAction::Redraw, None)
            }
            Outcome::Gone => {
                self.attempt = None;
                self.failed = None;
                (QuickAction::Redraw, Some(NM_GONE))
            }
            Outcome::Failed { why, cleanup } => {
                self.mark_failed(now);
                (cleanup.unwrap_or(QuickAction::Redraw), Some(why))
            }
        }
    }

    /// The attempt failed at `now`: its network shows `failed` for FAILED_SHOWN.
    fn mark_failed(&mut self, now: Instant) {
        if let Some(a) = self.attempt.take() {
            self.failed = Some((a.ssid, now + FAILED_SHOWN));
        }
    }

    /// Notes since when the open field's network is missing from what was read, so that `due` can
    /// close the field `FIELD_KEPT` later.
    fn follow_field(&mut self, now: Instant) {
        let listed = match &self.field {
            Some(f) => self.listed(&f.ap.ssid),
            None => return,
        };
        // The first look is done on `&self.field` so that `listed` can borrow all of `self`; the
        // field is changed only here, once that borrow has ended.
        if let Some(f) = self.field.as_mut() {
            f.missing = if listed { None } else { Some(f.missing.unwrap_or(now)) };
        }
    }

    /// `ssid` among the access points read, or the one the device is on.
    fn listed(&self, ssid: &[u8]) -> bool {
        self.device().is_some_and(|d| d.access_points.iter().flatten().any(|a| a.ssid == ssid))
            || self.connected().is_some_and(|a| a.ssid == ssid)
    }

    /// At `now` (the panel's one timer): the attempt timed out (the cleanup to run, why), the
    /// `failed` mark gone, the field whose network stayed away `FIELD_KEPT` closed. The third value
    /// is whether to repaint.
    pub fn due(&mut self, now: Instant) -> (Vec<QuickAction>, Option<&'static str>, bool) {
        let (mut run, mut failed, mut redraw) = (Vec::new(), None, false);
        if let Some(attempt) = self.attempt.as_mut()
            && let Outcome::Failed { why, cleanup } = attempt.expired(self.state.as_ref(), now)
        {
            self.mark_failed(now);
            run.extend(cleanup);
            (failed, redraw) = (Some(why), true);
        }
        redraw |= session::expire(&mut self.failed, now);
        if self.field_expired(now) {
            self.close_field();
            redraw = true;
        }
        (run, failed, redraw)
    }

    /// The open field's network has been missing from the access points read for `FIELD_KEPT`.
    fn field_expired(&self, now: Instant) -> bool {
        let Some(f) = &self.field else { return false };
        match f.missing {
            Some(since) => since + FIELD_KEPT <= now,
            None => false,
        }
    }

    /// The attempt's timeout, the `failed` mark's end or the missing field's, whichever first.
    pub fn deadline(&self) -> Option<Instant> {
        // The three candidates as `Option`s: `flatten` drops the empty ones, `min` takes the first
        // due (and is `None` when there is none).
        [
            self.attempt.as_ref().map(|a| a.until),
            self.failed.as_ref().map(|(_, at)| *at),
            self.field.as_ref().and_then(|f| f.missing).map(|at| at + FIELD_KEPT),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The attempt's nmcli was never started (not found): it ends at once, nothing to follow or
    /// clean, no `failed` mark (the log says why).
    pub fn never_started(&mut self) {
        self.attempt = None;
    }

    /// The open field's secret, for the wiring to hand to nmcli (`quick_spawn_secret` in `app.rs`).
    pub fn password(&self) -> Option<&Secret> {
        self.field.as_ref().map(|f| &f.secret)
    }

    /// The wiring handed the password over (or could not): the field closes, its buffer wiped.
    pub fn password_sent(&mut self) {
        self.close_field();
    }

    /// The field closes (dropped: its buffer wiped), the selection back on its network.
    fn close_field(&mut self) {
        if let Some(f) = self.field.take() {
            self.pick = Pick::Network(f.ap.ssid.clone());
        }
    }

    /// The attempt on `ssid` begins: the saved profiles noted, no `failed` mark.
    fn begin(&mut self, ssid: &[u8], creates: bool, now: Instant) {
        self.attempt = Some(Attempt::new(ssid, creates, self.state.as_ref(), now));
        self.failed = None;
    }

    /// The picks in order: the networks (the field under its own), then `More…` when shown.
    fn picks(&self, nets: &[Network], config: &QuickConfig) -> Vec<Pick> {
        let mut v = Vec::new();
        for n in nets {
            v.push(Pick::Network(n.ssid.clone()));
            if self.field.as_ref().is_some_and(|f| f.ap.ssid == n.ssid) {
                v.push(Pick::Password);
            }
        }
        if !more(config).is_empty() {
            v.push(Pick::More);
        }
        v
    }

    /// Before each use: with no device the detail closes; the selection goes back on a pick that
    /// exists (the first one when its own went; none while the list is being waited for).
    pub fn fit(&mut self, config: &QuickConfig) {
        if !self.shows() {
            self.close_detail();
            return;
        }
        if !self.open {
            return;
        }
        let nets = self.networks();
        if self.field.as_ref().is_some_and(|f| !nets.iter().any(|n| n.ssid == f.ap.ssid)) {
            self.field = None;
        }
        // Searching (the list not read yet, no network known): the selection waits for the list,
        // never left on `More…` once it arrives.
        let searching = nets.is_empty() && self.enabled() && self.device().is_some_and(|d| d.access_points.is_none());
        let was_searching = self.searching;
        self.searching = searching;
        if was_searching
            && !searching
            && self.pick == Pick::More
            && let Some(first) = nets.first()
        {
            self.pick = Pick::Network(first.ssid.clone());
        }
        let picks = self.picks(&nets, config);
        if !picks.contains(&self.pick) {
            self.pick = if searching { Pick::None } else { picks.into_iter().next().unwrap_or_default() };
        }
    }

    /// The detail's lines: its header, a separator, the networks (or a note), and `More…` after a
    /// separator when configured.
    pub fn lines(&self, config: &QuickConfig) -> Vec<Line> {
        let mut v = vec![Line::WifiHeader, Line::Separator];
        let nets = self.networks();
        let listed = self.device().is_some_and(|d| d.access_points.is_some());
        if !self.enabled() {
            v.push(Line::WifiNote(OFF_NOTE));
        } else if nets.is_empty() {
            v.push(Line::WifiNote(if listed { NO_NETWORKS } else { SEARCHING }));
        } else {
            for (i, n) in nets.iter().enumerate() {
                v.push(Line::Network(i));
                if self.field.as_ref().is_some_and(|f| f.ap.ssid == n.ssid) {
                    v.push(Line::Password);
                }
            }
        }
        if !more(config).is_empty() {
            v.push(Line::Separator);
            v.push(Line::WifiMore);
        }
        v
    }

    /// Whether `line` is the selected one (`nets` is `networks()`, which the caller already has).
    pub fn is_selected(&self, line: Line, nets: &[Network]) -> bool {
        match (line, &self.pick) {
            (Line::Network(i), Pick::Network(s)) => nets.get(i).is_some_and(|n| n.ssid == *s),
            (Line::WifiMore, Pick::More) => true,
            (Line::Password, Pick::Password) => true,
            _ => false,
        }
    }

    /// Keys in the detail: ↑ ↓ Home End over the networks and `More…` (never wrapping), Enter acts,
    /// Escape or ← go back to the rows. The open field takes them first (`field_key`).
    pub fn key(&mut self, config: &QuickConfig, k: Key, now: Instant) -> QuickAction {
        if self.field.is_some() {
            return self.field_key(config, k, now);
        }
        let nets = self.networks();
        let picks = self.picks(&nets, config);
        let at = picks.iter().position(|p| *p == self.pick);
        match k {
            Key::Escape | Key::Left => {
                self.close_detail();
                return QuickAction::Redraw;
            }
            Key::Enter => return self.activate(config, &nets, now),
            _ => {}
        }
        let Some(to) = nav(k, at, picks.len()) else { return QuickAction::Nothing };
        match picks.into_iter().nth(to) {
            Some(pick) => moved(&mut self.pick, pick),
            None => QuickAction::Nothing,
        }
    }

    /// Keys while the field is open: characters, Backspace, Ctrl+U edit; Enter sends; Escape closes
    /// the field (the detail stays); ↑ ↓ Home End close it and move.
    fn field_key(&mut self, config: &QuickConfig, k: Key, now: Instant) -> QuickAction {
        let Some(f) = self.field.as_mut() else { return QuickAction::Nothing };
        match k {
            Key::Char(c) => {
                if !f.secret.push(c) {
                    f.overflow = true;
                }
                QuickAction::Redraw
            }
            Key::Backspace => {
                if f.secret.pop() {
                    QuickAction::Redraw
                } else {
                    QuickAction::Nothing
                }
            }
            Key::CtrlU => {
                f.secret.clear();
                f.overflow = false;
                QuickAction::Redraw
            }
            Key::Enter => self.send_password(now),
            Key::Escape => {
                self.close_field();
                QuickAction::Redraw
            }
            Key::Up | Key::Down | Key::CtrlP | Key::CtrlN | Key::Home | Key::End => {
                self.close_field();
                self.key(config, k, now)
            }
            _ => QuickAction::Nothing,
        }
    }

    /// Enter in the field: `nmcli --ask device wifi connect <ssid>`, the password for its standard
    /// input (`QuickAction::RunSecret`); refused empty, after an overflow, or while an attempt runs.
    fn send_password(&mut self, now: Instant) -> QuickAction {
        let Some(f) = &self.field else { return QuickAction::Nothing };
        if f.overflow || f.secret.is_empty() || self.attempt.is_some() {
            return QuickAction::Nothing;
        }
        let Ok(ssid) = std::str::from_utf8(&f.ap.ssid) else { return QuickAction::Nothing };
        let argv = argv(&["nmcli", "--ask", "device", "wifi", "connect", ssid]);
        let target = f.ap.ssid.clone();
        self.begin(&target, true, now);
        QuickAction::RunSecret { argv, what: "wifi connect (password)".into() }
    }

    /// Enter on the selection: `More…` runs, a network is entered, the password row does nothing
    /// (Enter in the field is `field_key`'s).
    fn activate(&mut self, config: &QuickConfig, nets: &[Network], now: Instant) -> QuickAction {
        match self.pick.clone() {
            Pick::More => run_more(config),
            Pick::Network(ssid) => match nets.iter().find(|n| n.ssid == ssid) {
                Some(n) => self.enter(config, n, now),
                None => QuickAction::Nothing,
            },
            Pick::None | Pick::Password => QuickAction::Nothing,
        }
    }

    /// Enter on `n` (quick Wi-Fi spec § 2): the exact nmcli command for its kind, the attempt it
    /// starts followed; the password field for a new secured one; `More…` chosen for one the panel
    /// does not handle. One attempt at a time: only `More…` goes on meanwhile.
    fn enter(&mut self, config: &QuickConfig, n: &Network, now: Instant) -> QuickAction {
        let kind = self.kind(n);
        if self.attempt.is_some() && kind != Kind::Unsupported {
            return QuickAction::Nothing;
        }
        match kind {
            Kind::Connected(Some(uuid)) => run(&["nmcli", "connection", "down", "uuid", &uuid], "wifi disconnect"),
            Kind::Connected(None) => QuickAction::Nothing,
            Kind::Saved(uuid) => {
                self.begin(&n.ssid, false, now);
                run(&["nmcli", "connection", "up", "uuid", &uuid], "wifi connect (saved)")
            }
            Kind::Open => match std::str::from_utf8(&n.ssid) {
                Ok(ssid) => {
                    let action = run(&["nmcli", "device", "wifi", "connect", ssid], "wifi connect (open)");
                    self.begin(&n.ssid, true, now);
                    action
                }
                Err(_) => QuickAction::Nothing,
            },
            Kind::Secured => {
                let ap = AccessPoint { ssid: n.ssid.clone(), strength: n.strength, security: n.security };
                self.field = Some(Field { ap, secret: Secret::new(), overflow: false, missing: None });
                self.pick = Pick::Password;
                QuickAction::Redraw
            }
            Kind::Unsupported if !more(config).is_empty() => {
                self.pick = Pick::More;
                QuickAction::Redraw
            }
            Kind::Unsupported => QuickAction::Nothing,
        }
    }

    /// A press on a detail line (the left button only): a network is chosen and entered; `More…`
    /// runs; the header goes back. A press anywhere but the field closes it.
    pub fn press(&mut self, config: &QuickConfig, line: Line, b: Button, now: Instant) -> QuickAction {
        if b != Button::Left {
            return QuickAction::Nothing;
        }
        if line != Line::Password {
            self.close_field();
        }
        match line {
            Line::WifiHeader => {
                self.close_detail();
                QuickAction::Redraw
            }
            Line::Network(i) => {
                let nets = self.networks();
                let Some(n) = nets.get(i) else { return QuickAction::Nothing };
                let before = self.pick.clone();
                self.pick = Pick::Network(n.ssid.clone());
                let a = self.enter(config, n, now);
                if a == QuickAction::Nothing && self.pick != before { QuickAction::Redraw } else { a }
            }
            Line::WifiMore => {
                self.pick = Pick::More;
                run_more(config)
            }
            _ => QuickAction::Nothing,
        }
    }

    /// The tag after a network's marks: `…` during its attempt, `failed` (warn) for `FAILED_SHOWN`
    /// after, else `saved` (dimmed) or nothing. See docs/decisions.md, "A row under connection reads
    /// `…`".
    fn tag(&self, n: &Network) -> (&'static str, Style) {
        if self.attempt.as_ref().is_some_and(|a| a.ssid == n.ssid) {
            (RUNNING_TAG, Style::MUTED)
        } else if self.failed.as_ref().is_some_and(|(s, _)| *s == n.ssid) {
            (FAILED_TAG, Style::WARN)
        } else if n.saved.is_some() {
            (SAVED_TAG, Style::MUTED)
        } else {
            ("", Style::NORMAL)
        }
    }

    /// One detail line into `g` (`WIDTH + 2` wide, a pad cell each side).
    pub fn draw(&self, g: &mut Grid, line: Line, nets: &[Network], selected: bool) {
        match line {
            Line::WifiHeader => text_row(g, None, "‹ Wi-Fi", "", Style::NORMAL, false),
            Line::WifiNote(note) => note_row(g, note),
            Line::WifiMore => text_row(g, Some(' '), "More…", "", Style::NORMAL, selected),
            Line::Network(i) => {
                if let Some(n) = nets.get(i) {
                    let (tag, style) = self.tag(n);
                    network_row(g, n, tag, style, selected);
                }
            }
            Line::Password => {
                if let Some(f) = &self.field {
                    password_row(g, f, selected);
                }
            }
            _ => {}
        }
    }
}

/// "● HomeNet-5G  ▂▄▆█ 🔒 saved" (quick Wi-Fi spec § 2): ● on the connected one in the accent,
/// the SSID cut to fit, then the signal, the lock for a secured one, and the tag. The signal and
/// the lock sit in one column on every row: the tag's room is the widest tag's, one cell from the
/// end. Selected: the band across the whole row.
fn network_row(g: &mut Grid, n: &Network, tag: &str, tag_style: Style, selected: bool) {
    let end = g.width();
    let lock = if n.security.secured() { LOCK } else { ' ' };
    let signal = bars(n.strength);
    // The widest tag sets the room every row keeps for its tag.
    let tag_room = [RUNNING_TAG, FAILED_TAG, SAVED_TAG].into_iter().map(text_width).max().unwrap_or(0);
    // "▂▄▆█ 🔒 " then the tag's room.
    let width = BARS.len() as u16 + 3 + tag_room;
    let label = truncate(&n.label, end.saturating_sub(width + 2).saturating_sub(3));
    let mark = if n.connected { '●' } else { ' ' };
    text_row(g, Some(mark), &label, "", Style::NORMAL, selected);
    let pick = |s: Style| if selected { Style::SELECTED } else { s };
    let x = g.write(end.saturating_sub(width + 1), end, &signal, pick(Style::NORMAL));
    let x = g.write(x, end, " ", pick(Style::NORMAL));
    let x = g.write(x, end, &lock.to_string(), pick(Style::NORMAL));
    if !tag.is_empty() {
        let x = g.write(x, end, " ", pick(Style::NORMAL));
        g.write(x, end, tag, pick(tag_style));
    }
}

/// "   Password  ••••••••" (quick Wi-Fi spec § 3): one dot per character, the end kept when longer
/// than the row; `too long` in warn after a character that did not fit. Never a byte of the secret.
fn password_row(g: &mut Grid, f: &Field, selected: bool) {
    let end = g.width();
    text_row(g, Some(' '), "Password", "", Style::NORMAL, selected);
    let pick = |s: Style| if selected { Style::SELECTED } else { s };
    if f.overflow {
        g.write(PASSWORD_AT, end.saturating_sub(1), TOO_LONG, pick(Style::WARN));
    } else {
        let dots = list::tail(&"•".repeat(f.secret.chars()), end.saturating_sub(PASSWORD_AT + 1));
        g.write(PASSWORD_AT, end.saturating_sub(1), &dots, pick(Style::NORMAL));
    }
}

#[cfg(test)]
mod tests;
