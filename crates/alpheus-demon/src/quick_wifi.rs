//! The quick panel's Wi-Fi (quick Wi-Fi spec), pure: the row's glyph and value, the detail with the
//! visible networks (one per SSID, its strongest access point, ordered, at most NETWORKS_MAX), its
//! keys and clicks, the password field, the exact `nmcli` commands to run, and the connection
//! attempt they start, followed to its end (`Attempt`). The state comes from the D-Bus thread
//! (`sources/dbus/wifi.rs`) only while the panel is open, or while an attempt on a new network is
//! still followed after it closed (`quick_app::WifiFollow`, Mae's ruling P1).

use crate::actions::Button;
use crate::config::QuickConfig;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::keys::Key;
use crate::list;
use crate::quick::{LOCK, Line, QuickAction, argv, run, text_row};
use crate::quick_secret::Secret;
use crate::system::{AccessPoint, SavedWifi, Security, WifiDevice, WifiScope, WifiState};
use crate::text;
use std::cmp::Reverse;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

/// The most networks the detail lists (quick Wi-Fi spec § 2).
pub const NETWORKS_MAX: usize = 32;
/// NMDeviceState: activated.
pub const ACTIVATED: u32 = 100;
/// The row's value: at most this many cells of the connected SSID.
pub const VALUE_CELLS: u16 = 16;
/// Material Design (Nerd Font): wifi-strength-1 to 4; wifi-strength-outline (on, not connected);
/// wifi-strength-off-outline (decision 5).
pub const STRENGTH: [char; 4] = ['\u{f091f}', '\u{f0922}', '\u{f0925}', '\u{f0928}'];
pub const WIFI_ON: char = '\u{f092f}';
pub const WIFI_OFF: char = '\u{f092e}';
/// The detail's notes (decision 11).
pub const OFF_NOTE: &str = "Wi-Fi is off";
pub const SEARCHING: &str = "Searching…";
pub const NO_NETWORKS: &str = "No networks";
/// The row's value on a connected network whose SSID shows nothing (Task 5 review).
pub const HIDDEN: &str = "hidden";
/// NMDeviceState: an activation under way (prepare to secondaries); failed.
pub const ACTIVATING: RangeInclusive<u32> = 40..=90;
pub const FAILED: u32 = 120;
/// No word from NetworkManager this long after Enter: failed (quick Wi-Fi spec § 2).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// An attempt still activating on its network at CONNECT_TIMEOUT waits up to this long from its
/// start (beyond NetworkManager's default 45 s DHCP timeout); then it ends, its profile kept.
pub const ACTIVATION_BOUND: Duration = Duration::from_secs(60);
/// How long a failed network shows `failed`.
pub const FAILED_SHOWN: Duration = Duration::from_secs(4);
/// How long an open password field outlives its network leaving the list (an access point aged
/// out while typing; controller reconciliation P12).
pub const FIELD_KEPT: Duration = Duration::from_secs(10);
/// The password's first cell: after "Password" and two spaces.
pub const PASSWORD_AT: u16 = 13;
/// The field after a character that did not fit (Review Focus 3).
pub const TOO_LONG: &str = "too long";
/// Why an attempt failed, for the log (never an SSID).
const ACTIVATION_FAILED: &str = "the activation failed";
const NO_ANSWER: &str = "no answer in 20 s";
const STILL_ACTIVATING: &str = "still activating after 60 s";
pub(crate) const NM_GONE: &str = "NetworkManager went away";
/// A failure whose last state predates an outage of another domain (F4): its profile kept.
const STALE: &str = "no fresh state since an outage";
const BARS: [char; 4] = ['▂', '▄', '▆', '█'];
/// A network row's tags; the widest one sets the room every row keeps for its tag.
const CONNECTING: &str = "…";
const FAILED_TAG: &str = "failed";
const SAVED_TAG: &str = "saved";

/// 1 to 4 steps of signal: 0–24 %, 25–49 %, 50–74 %, 75–100 % (decision 5).
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

/// An SSID that cannot be one nmcli argument as is (quick Wi-Fi spec § 5, decision 7): empty, a
/// NUL, a leading '-' (read as an option), not UTF-8, or one nmcli reads as something else: `h`,
/// `he`, `hel`, `help` (help), or six hexadecimal groups (a BSSID).
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
    b":-".iter().any(|sep| {
        let mut groups = 0;
        ssid.split(|b| b == sep).all(|g| {
            groups += 1;
            (1..=2).contains(&g.len()) && g.iter().all(u8::is_ascii_hexdigit)
        }) && groups == 6
    })
}

/// The actions that start an attempt (`Wifi::enter`, `send_password`): their `what`.
pub fn starts_attempt(what: &str) -> bool {
    what.starts_with("wifi connect")
}

/// `[quick] wifi_more`, resolved at startup; unresolved (a test's default) or empty: no `More…`.
pub fn more(config: &QuickConfig) -> &[String] {
    config.wifi_more.as_deref().unwrap_or(&[])
}

/// `More…`: `wifi_more` as given, then the panel closes (decision 7); nothing when it is empty.
fn run_more(config: &QuickConfig) -> QuickAction {
    let argv = more(config).to_vec();
    if argv.is_empty() { QuickAction::Nothing } else { QuickAction::RunAndClose { argv, what: "wifi more".into() } }
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

/// The detail's selection, by identity.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Its network as listed when the field opened: listed still while the field waits for it
    /// (controller reconciliation P12).
    pub ap: AccessPoint,
    pub secret: Secret,
    /// A character did not fit: Enter is refused until Ctrl+U.
    pub overflow: bool,
    /// Since when its network is missing from the access points read: FIELD_KEPT later, the field
    /// goes.
    pub missing: Option<Instant>,
}

/// A connection under way (quick Wi-Fi spec § 2, decisions 8–9), followed by the panel (`Wifi`) or,
/// once the panel closed, by `quick_app::WifiFollow` (Mae's ruling P1): it needs nothing of the
/// panel, only the states read and the clock.
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

/// The saved profiles in `state`, when read.
fn saved_in(state: Option<&WifiState>) -> Option<&Vec<SavedWifi>> {
    state?.device.as_ref()?.saved.as_ref()
}

impl Attempt {
    /// Enter on `ssid` at `now`, with `state` the last read: the saved profiles noted.
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
        let Some(d) = state.and_then(|s| s.device.as_ref()) else { return Outcome::Gone };
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

    /// The clock: no word by `until`, failed; nothing deleted when the device was last seen
    /// activated on what may be the new profile (its uuid or its access point unread, never shown
    /// to be another network's).
    /// Still activating on its network (or on an access point unread) at the timeout: a slow
    /// activation, never one to lose its profile; the deadline moves to ACTIVATION_BOUND from its
    /// start, and there the attempt ends with nothing deleted.
    pub fn expired(&mut self, state: Option<&WifiState>, now: Instant) -> Outcome {
        if self.until > now {
            return Outcome::Waiting;
        }
        let activating = state
            .and_then(|s| s.device.as_ref())
            .is_some_and(|d| ACTIVATING.contains(&d.state) && d.active.as_ref().is_none_or(|ap| ap.ssid == self.ssid));
        if activating {
            let bound = self.started + ACTIVATION_BOUND;
            if now < bound {
                self.until = bound;
                return Outcome::Waiting;
            }
            return Outcome::Failed { why: STILL_ACTIVATING, cleanup: None };
        }
        let maybe_ours = state.and_then(|s| s.device.as_ref()).is_some_and(|d| {
            d.state == ACTIVATED
                && match &d.active_uuid {
                    Some(uuid) => self.created.as_ref() == Some(uuid),
                    None => d.active.as_ref().is_none_or(|ap| ap.ssid == self.ssid),
                }
        });
        match self.failure(state, NO_ANSWER) {
            Outcome::Failed { why, .. } if maybe_ours => Outcome::Failed { why, cleanup: None },
            failed => failed,
        }
    }

    /// Failed: its new profile deleted, only when the attempt could make one (never a `connection
    /// up`'s) and while `state` still lists it (nmcli may have deleted it). Never on a state read
    /// before another domain's outage (F4: NetworkManager may have connected since): kept, and
    /// the log says so.
    fn failure(&self, state: Option<&WifiState>, why: &'static str) -> Outcome {
        let still_saved = |uuid: &str| saved_in(state).is_some_and(|s| s.iter().any(|p| p.uuid == uuid));
        let cleanup = self
            .created
            .as_deref()
            .filter(|u| self.creates && still_saved(u))
            .map(|u| run(&["nmcli", "connection", "delete", "uuid", u], "wifi delete the failed profile"));
        if cleanup.is_some() && state.is_some_and(|s| s.stale) {
            return Outcome::Failed { why: STALE, cleanup: None };
        }
        Outcome::Failed { why, cleanup }
    }
}

/// The panel's Wi-Fi: dropped whole with the panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Wifi {
    /// What the D-Bus thread last read; None: nothing yet, or NetworkManager gone.
    pub state: Option<WifiState>,
    /// The detail shows in place of the rows.
    pub open: bool,
    pub pick: Pick,
    /// Opened from the network module or `--quick wifi` (decision 12): the first state opens the detail.
    pub focus_pending: bool,
    /// The password field, while open.
    pub field: Option<Field>,
    /// The connection under way.
    pub attempt: Option<Attempt>,
    /// The network that just failed, shown `failed` until then.
    pub failed: Option<(Vec<u8>, Instant)>,
    /// The detail was searching for its list: a `More…` chosen meanwhile gives way to the first
    /// network once the list arrives.
    pub searching: bool,
}

impl Wifi {
    fn device(&self) -> Option<&WifiDevice> {
        self.state.as_ref()?.device.as_ref()
    }

    /// The row shows: a Wi-Fi device.
    pub fn shows(&self) -> bool {
        self.device().is_some()
    }

    pub fn enabled(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.enabled)
    }

    /// The network the device is on: activated, with an access point.
    pub fn connected(&self) -> Option<&AccessPoint> {
        let d = self.device()?;
        if d.state == ACTIVATED { d.active.as_ref() } else { None }
    }

    pub fn glyph(&self) -> char {
        match self.connected() {
            _ if !self.enabled() => WIFI_OFF,
            Some(ap) => STRENGTH[steps(ap.strength) - 1],
            None => WIFI_ON,
        }
    }

    /// The row's value: the connected SSID, "on" or "off" (quick Wi-Fi spec § 1).
    pub fn value(&self) -> String {
        match self.connected() {
            _ if !self.enabled() => "off".into(),
            Some(ap) if hidden(&ap.ssid) => HIDDEN.into(),
            Some(ap) => truncate(&label(&ap.ssid), VALUE_CELLS),
            None => "on".into(),
        }
    }

    /// The detail's networks (quick Wi-Fi spec § 2): one per SSID (its strongest access point, the
    /// connected one included even when the list misses it, and the open field's for FIELD_KEPT
    /// once the list misses it), hidden ones left out; the connected
    /// one, the saved ones by signal, the others by signal (then by label); at most NETWORKS_MAX.
    /// None with the radio off.
    pub fn networks(&self) -> Vec<Network> {
        let Some(d) = self.device() else { return Vec::new() };
        if !self.enabled() {
            return Vec::new();
        }
        let connected = self.connected().map(|a| a.ssid.as_slice());
        let mut best: Vec<&AccessPoint> = Vec::new();
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
    /// while an attempt runs (decision 10: its new profile must be seen).
    pub fn scope(&self) -> WifiScope {
        if self.open || self.attempt.is_some() { WifiScope::Detail } else { WifiScope::Row }
    }

    /// The D-Bus thread's state (`Quick::set_wifi`): kept, the field's network looked for, and the
    /// attempt followed (`Attempt::observe`). Returns the cleanup to run and why it failed.
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

    /// Notes since when the open field's network is missing from what was read (P12).
    fn follow_field(&mut self, now: Instant) {
        let listed = match &self.field {
            Some(f) => self.listed(&f.ap.ssid),
            None => return,
        };
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
    /// `failed` mark gone, the field whose network stayed away FIELD_KEPT closed (a repaint each).
    pub fn due(&mut self, now: Instant) -> (Vec<QuickAction>, Option<&'static str>, bool) {
        let (mut run, mut failed, mut redraw) = (Vec::new(), None, false);
        if let Some(Outcome::Failed { why, cleanup }) = self.attempt.as_mut().map(|a| a.expired(self.state.as_ref(), now)) {
            self.mark_failed(now);
            run.extend(cleanup);
            (failed, redraw) = (Some(why), true);
        }
        if self.failed.as_ref().is_some_and(|(_, at)| *at <= now) {
            self.failed = None;
            redraw = true;
        }
        if self.field.as_ref().and_then(|f| f.missing).is_some_and(|at| at + FIELD_KEPT <= now) {
            self.close_field();
            redraw = true;
        }
        (run, failed, redraw)
    }

    /// The attempt's timeout, the `failed` mark's end or the missing field's, whichever first.
    pub fn deadline(&self) -> Option<Instant> {
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

    /// The open field's secret, for the wiring to hand to nmcli (`quick_spawn_secret`).
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

    /// Before each use: no device, the detail closes; the selection back on a pick that exists (the
    /// first one when its own went; none while the list is being searched for).
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
        if std::mem::replace(&mut self.searching, searching)
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
        let last = picks.len().saturating_sub(1);
        let to = match k {
            Key::Escape | Key::Left => {
                self.close_detail();
                return QuickAction::Redraw;
            }
            Key::Enter => return self.activate(config, &nets, now),
            Key::Up | Key::CtrlP => at.map_or(0, |a| a.saturating_sub(1)),
            Key::Down | Key::CtrlN => at.map_or(0, |a| (a + 1).min(last)),
            Key::Home => 0,
            Key::End => last,
            _ => return QuickAction::Nothing,
        };
        match picks.get(to) {
            Some(p) if *p != self.pick => {
                self.pick = p.clone();
                QuickAction::Redraw
            }
            _ => QuickAction::Nothing,
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
    /// does not handle. One attempt at a time (decision 8): only `More…` goes on meanwhile.
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

    /// The tag after a network's marks: "…" during its attempt (Mae's short tags, 2026-10-12), "failed" (warn) for
    /// FAILED_SHOWN after, else "saved" (dimmed) or nothing.
    fn tag(&self, n: &Network) -> (&'static str, Style) {
        if self.attempt.as_ref().is_some_and(|a| a.ssid == n.ssid) {
            (CONNECTING, Style::MUTED)
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
        let end = g.width();
        match line {
            Line::WifiHeader => text_row(g, None, "‹ Wi-Fi", "", Style::NORMAL, false),
            Line::WifiNote(note) => {
                g.write(3, end.saturating_sub(1), note, Style::MUTED);
            }
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
    let tag_room = [CONNECTING, FAILED_TAG, SAVED_TAG].into_iter().map(text_width).max().unwrap_or(0);
    // "▂▄▆█ 🔒 " then the tag's room.
    let width = BARS.len() as u16 + 3 + tag_room;
    let label = truncate(&n.label, end.saturating_sub(width + 2).saturating_sub(3));
    let mark = if n.connected { '●' } else { ' ' };
    list::row(
        g,
        0,
        end,
        &list::Row { icon: Some(mark), icon_style: Style::ACCENT, text: &label, right: "", style: Style::NORMAL, selected },
    );
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
    list::row(
        g,
        0,
        end,
        &list::Row { icon: Some(' '), icon_style: Style::ACCENT, text: "Password", right: "", style: Style::NORMAL, selected },
    );
    let pick = |s: Style| if selected { Style::SELECTED } else { s };
    if f.overflow {
        g.write(PASSWORD_AT, end.saturating_sub(1), TOO_LONG, pick(Style::WARN));
    } else {
        let dots = list::tail(&"•".repeat(f.secret.chars()), end.saturating_sub(PASSWORD_AT + 1));
        g.write(PASSWORD_AT, end.saturating_sub(1), &dots, pick(Style::NORMAL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quick_app::WifiFollow;
    use crate::quick_secret::CAPACITY;
    use crate::system::{SavedWifi, WifiDevice};
    use std::time::Duration;

    const HOME: &str = "11111111-2222-4333-8444-555555555555";
    const CAFE: &str = "33333333-4444-4555-8666-777777777777";

    fn ap(ssid: &str, strength: u8, security: Security) -> AccessPoint {
        AccessPoint { ssid: ssid.as_bytes().to_vec(), strength, security }
    }

    /// Connected to Maison-Test (80 %, WPA2, saved); Labo-Test (70 %, 802.1X), Bureau-Test (60 %,
    /// WPA2) and Cafe-Libre (40 %, open) around (fictional networks).
    fn state() -> WifiState {
        let home = ap("Maison-Test", 80, Security::Psk);
        WifiState {
            enabled: true,
            device: Some(WifiDevice {
                state: ACTIVATED,
                active: Some(home.clone()),
                active_uuid: Some(HOME.into()),
                access_points: Some(vec![
                    ap("Cafe-Libre", 40, Security::Open),
                    home,
                    ap("Bureau-Test", 60, Security::Psk),
                    ap("Labo-Test", 70, Security::Enterprise),
                ]),
                saved: Some(vec![SavedWifi { uuid: HOME.into(), ssid: b"Maison-Test".to_vec() }]),
            }),
            stale: false,
        }
    }

    fn device(s: &mut WifiState) -> &mut WifiDevice {
        s.device.as_mut().expect("a device")
    }

    fn config() -> QuickConfig {
        QuickConfig { wifi_more: Some(vec!["fake-more".into()]), ..QuickConfig::default() }
    }

    /// The detail open on `s`, fitted (the first network chosen).
    fn open(s: WifiState) -> Wifi {
        let mut w = Wifi { state: Some(s), ..Wifi::default() };
        w.open_detail();
        w.fit(&config());
        w
    }

    fn labels(w: &Wifi) -> Vec<String> {
        w.networks().iter().map(|n| n.label.clone()).collect()
    }

    fn run(parts: &[&str], what: &str) -> QuickAction {
        QuickAction::Run { argv: parts.iter().map(|s| s.to_string()).collect(), what: what.into() }
    }

    fn pick(w: &mut Wifi, label: &str) {
        w.pick = Pick::Network(label.as_bytes().to_vec());
    }

    #[test]
    fn four_steps_of_signal() {
        let cases = [
            (0, 1, "▂___"),
            (24, 1, "▂___"),
            (25, 2, "▂▄__"),
            (49, 2, "▂▄__"),
            (50, 3, "▂▄▆_"),
            (74, 3, "▂▄▆_"),
            (75, 4, "▂▄▆█"),
            (100, 4, "▂▄▆█"),
        ];
        for (strength, n, drawn) in cases {
            assert_eq!((steps(strength), bars(strength).as_str()), (n, drawn), "{strength} %");
        }
    }

    #[test]
    fn the_detail_orders_dedups_caps_and_hides() {
        assert_eq!(labels(&open(state())), ["Maison-Test", "Labo-Test", "Bureau-Test", "Cafe-Libre"], "connected, then by signal");
        let mut s = state();
        device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
        device(&mut s).access_points.as_mut().expect("listed").extend([ap("Bureau-Test", 90, Security::Open), ap("", 99, Security::Open)]);
        let w = open(s);
        assert_eq!(
            labels(&w),
            ["Maison-Test", "Cafe-Libre", "Bureau-Test", "Labo-Test"],
            "saved before stronger; one row per SSID; no hidden"
        );
        let bureau = &w.networks()[2];
        assert_eq!((bureau.strength, bureau.security), (90, Security::Open), "the strongest access point's");
        let mut s = state();
        device(&mut s).access_points = Some((0..40).map(|n| ap(&format!("Reseau-Test-{n:02}"), 50, Security::Psk)).collect());
        let w = open(s);
        assert_eq!(w.networks().len(), NETWORKS_MAX);
        assert_eq!(labels(&w)[0], "Maison-Test", "the connected one, even when the list misses it");
    }

    #[test]
    fn the_marks_and_the_rows() {
        let w = open(state());
        let nets = w.networks();
        let row = |line: Line, selected: bool| {
            let mut g = Grid::empty(crate::quick::WIDTH + 2);
            w.draw(&mut g, line, &nets, selected);
            g
        };
        assert_eq!(row(Line::Network(0), false).text().trim_end(), format!(" ● Maison-Test      ▂▄▆█ {LOCK} saved"));
        assert_eq!(row(Line::Network(1), false).text().trim_end(), format!("   Labo-Test        ▂▄▆_ {LOCK}"));
        assert_eq!(row(Line::Network(3), false).text().trim_end(), "   Cafe-Libre       ▂▄__", "open: no lock");
        let g = row(Line::Network(0), false);
        assert_eq!((&g.styles()[1..2], &g.styles()[27..32]), ("a", "mmmmm"), "● in the accent, saved dimmed");
        assert!(row(Line::Network(2), true).styles().chars().all(|c| c == 'S'), "selected: the band across the row");
        assert_eq!(row(Line::WifiHeader, false).text().trim_end(), " ‹ Wi-Fi");
        assert_eq!(row(Line::WifiMore, false).text().trim_end(), "   More…");
        assert_eq!(row(Line::WifiNote(OFF_NOTE), false).text().trim(), OFF_NOTE);
    }

    #[test]
    fn enter_on_each_kind_of_network() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Maison-Test");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "down", "uuid", HOME], "wifi disconnect"), "connected: down");
        let mut s = state();
        let d = device(&mut s);
        (d.state, d.active, d.active_uuid) = (30, None, None);
        let mut w = open(s);
        pick(&mut w, "Maison-Test");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", HOME], "wifi connect (saved)"));
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)"));
        pick(&mut w, "Labo-Test");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::More, "802.1X: More… instead");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::RunAndClose { argv: vec!["fake-more".into()], what: "wifi more".into() });
        let mut w = open(state());
        pick(&mut w, "Labo-Test");
        assert_eq!(w.key(&QuickConfig { wifi_more: Some(vec![]), ..config() }, Key::Enter, t), QuickAction::Nothing, "no More…");
    }

    /// Review Focus 2: an SSID that cannot be one nmcli argument as is (a leading '-', a NUL, not
    /// UTF-8) is shown cleaned and never passed: More… unsaved, by uuid saved.
    #[test]
    fn refused_ssids_go_to_more_unless_saved() {
        let latin1 = vec![0x43, 0x61, 0x66, 0xE9];
        assert!(refused(b"-x") && refused(b"a\0b") && refused(&latin1) && refused(b""));
        assert!(!refused(b"Cafe-Libre") && !refused("Café".as_bytes()) && !refused(b"a-b"));
        let (c, t) = (config(), Instant::now());
        let mut s = state();
        let latin = AccessPoint { ssid: latin1.clone(), strength: 45, security: Security::Open };
        device(&mut s).access_points.as_mut().expect("listed").extend([ap("-x", 50, Security::Open), latin]);
        device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: latin1.clone() });
        let mut w = open(s);
        assert!(labels(&w).contains(&"Caf\u{fffd}".to_string()), "shown cleaned: {:?}", labels(&w));
        pick(&mut w, "-x");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::More, "never `nmcli device wifi connect -x`");
        w.pick = Pick::Network(latin1);
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", CAFE], "wifi connect (saved)"), "saved: by uuid");
    }

    #[test]
    fn arrows_move_over_the_networks_and_more_never_wrapping() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the first network");
        assert_eq!(w.key(&c, Key::Up, t), QuickAction::Nothing);
        for _ in 0..3 {
            assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
        }
        assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
        assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::More);
        assert_eq!(w.key(&c, Key::Down, t), QuickAction::Nothing, "the end: no wrap");
        assert_eq!(w.key(&c, Key::Home, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()));
        assert_eq!(w.key(&c, Key::End, t), QuickAction::Redraw);
        assert_eq!(w.key(&c, Key::Left, t), QuickAction::Redraw);
        assert!(!w.open, "← back to the rows");
    }

    #[test]
    fn the_detail_lines_and_its_notes() {
        let c = config();
        assert_eq!(
            open(state()).lines(&c),
            [
                Line::WifiHeader,
                Line::Separator,
                Line::Network(0),
                Line::Network(1),
                Line::Network(2),
                Line::Network(3),
                Line::Separator,
                Line::WifiMore
            ]
        );
        let mut s = state();
        let d = device(&mut s);
        (d.state, d.active, d.access_points) = (30, None, None);
        assert_eq!(open(s.clone()).lines(&c)[2], Line::WifiNote(SEARCHING), "before the list arrives");
        device(&mut s).access_points = Some(vec![]);
        assert_eq!(open(s).lines(&c)[2], Line::WifiNote(NO_NETWORKS));
        let mut off = state();
        off.enabled = false;
        let w = open(off);
        assert_eq!(w.lines(&c)[2..], [Line::WifiNote(OFF_NOTE), Line::Separator, Line::WifiMore], "the radio off: the list emptied");
        assert_eq!(w.pick, Pick::More);
        assert_eq!(open(state()).lines(&QuickConfig::default()).last(), Some(&Line::Network(3)), "no wifi_more: no More…, no separator");
    }

    #[test]
    fn networkmanager_going_away_closes_the_detail() {
        let t = Instant::now();
        let mut w = open(state());
        w.set_state(None, t);
        w.fit(&config());
        assert!(!w.open && !w.shows() && w.pick == Pick::None);
        let mut w = open(state());
        w.set_state(Some(WifiState { enabled: true, device: None, stale: false }), t);
        w.fit(&config());
        assert!(!w.open, "the device gone");
    }

    #[test]
    fn a_press_acts_on_its_line() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        assert_eq!(
            w.press(&c, Line::Network(3), Button::Left, t),
            run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)")
        );
        assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
        assert_eq!(w.press(&c, Line::Network(1), Button::Left, t), QuickAction::Redraw, "802.1X: More… chosen");
        assert_eq!(w.pick, Pick::More);
        assert_eq!(w.press(&c, Line::Network(0), Button::Right, t), QuickAction::Nothing, "the left button only");
        assert_eq!(
            w.press(&c, Line::WifiMore, Button::Left, t),
            QuickAction::RunAndClose { argv: vec!["fake-more".into()], what: "wifi more".into() }
        );
        assert_eq!(w.press(&c, Line::WifiHeader, Button::Left, t), QuickAction::Redraw);
        assert!(!w.open, "the header goes back");
    }

    const NEW: &str = "22222222-3333-4444-8555-666666666666";

    fn typed(w: &mut Wifi, text: &str, t: Instant) {
        for c in text.chars() {
            w.key(&config(), Key::Char(c), t);
        }
    }

    /// `w`'s state with the device at `state`, on `active` (an access point of that SSID) or none.
    fn device_at(w: &mut Wifi, state: u32, active: Option<&str>, t: Instant) -> (QuickAction, Option<&'static str>) {
        let mut s = w.state.clone().expect("a state");
        let d = device(&mut s);
        d.state = state;
        d.active = active.map(|a| ap(a, 60, Security::Psk));
        w.set_state(Some(s), t)
    }

    fn saved_added(w: &mut Wifi, uuid: &str, ssid: &str, t: Instant) {
        let mut s = w.state.clone().expect("a state");
        device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: uuid.into(), ssid: ssid.as_bytes().to_vec() });
        w.set_state(Some(s), t);
    }

    /// The text of `label`'s row as drawn.
    fn row_of(w: &Wifi, label: &str) -> Grid {
        let nets = w.networks();
        let i = nets.iter().position(|n| n.label == label).expect("listed");
        let mut g = Grid::empty(crate::quick::WIDTH + 2);
        w.draw(&mut g, Line::Network(i), &nets, false);
        g
    }

    fn password_row_text(w: &Wifi) -> String {
        let nets = w.networks();
        let mut g = Grid::empty(crate::quick::WIDTH + 2);
        w.draw(&mut g, Line::Password, &nets, false);
        g.text().trim_end().to_string()
    }

    fn delete(uuid: &str) -> QuickAction {
        run(&["nmcli", "connection", "delete", "uuid", uuid], "wifi delete the failed profile")
    }

    #[test]
    fn a_new_secured_network_asks_for_its_password_in_a_field() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
        assert_eq!(w.pick, Pick::Password);
        assert_eq!(w.lines(&c)[4..6], [Line::Network(2), Line::Password], "the field under its network");
        typed(&mut w, "fake-pass", t);
        assert_eq!(password_row_text(&w), format!("   Password  {}", "•".repeat(9)), "one dot per character, never the text");
        let send = w.key(&c, Key::Enter, t);
        assert_eq!(
            send,
            QuickAction::RunSecret {
                argv: argv(&["nmcli", "--ask", "device", "wifi", "connect", "Bureau-Test"]),
                what: "wifi connect (password)".into()
            }
        );
        assert!(!format!("{send:?}").contains("fake-pass"), "never in the action, its argv or its Debug");
        assert_eq!(w.password().map(|s| s.bytes()), Some(&b"fake-pass"[..]), "kept for the wiring to hand over");
        w.password_sent();
        assert!(w.field.is_none() && w.password().is_none(), "closed: its buffer wiped as it drops");
        assert_eq!(w.pick, Pick::Network(b"Bureau-Test".to_vec()));
        assert!(row_of(&w, "Bureau-Test").text().trim_end().ends_with(" …"));
    }

    #[test]
    fn the_field_s_keys() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "empty: nothing sent");
        typed(&mut w, "abc", t);
        assert_eq!(w.key(&c, Key::Backspace, t), QuickAction::Redraw);
        assert_eq!(w.password().map(|s| s.bytes()), Some(&b"ab"[..]));
        assert_eq!(w.key(&c, Key::CtrlU, t), QuickAction::Redraw);
        assert!(w.password().is_some_and(|s| s.is_empty()));
        typed(&mut w, "x y", t);
        assert_eq!(w.password().map(|s| s.bytes()), Some(&b"x y"[..]), "Space is a character here");
        assert_eq!(w.key(&c, Key::Escape, t), QuickAction::Redraw);
        assert!(w.field.is_none() && w.open, "Escape closes the field and keeps the detail");
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        typed(&mut w, "z", t);
        assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
        assert!(w.field.is_none(), "moving away closes it");
        assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        w.close_detail();
        assert!(w.field.is_none(), "leaving the detail closes it");
    }

    /// A press on the field keeps it; a press anywhere else closes it (its buffer wiped), as moving
    /// away does.
    #[test]
    fn a_press_elsewhere_closes_the_field() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        typed(&mut w, "fake-pass", t);
        assert_eq!(w.press(&c, Line::Password, Button::Left, t), QuickAction::Nothing);
        assert!(w.field.is_some(), "a press on the field keeps it");
        assert_eq!(
            w.press(&c, Line::Network(3), Button::Left, t),
            run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)")
        );
        assert!(w.field.is_none() && w.password().is_none(), "another network pressed: the field gone");
    }

    /// Review Focus 3: characters, not bytes; a whole character per Backspace; a character that
    /// does not fit refuses the attempt until Ctrl+U: never a password sent cut short.
    #[test]
    fn the_password_field_counts_characters_and_refuses_an_overflow() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        typed(&mut w, "é🔑a", t);
        assert_eq!(password_row_text(&w), format!("   Password  {}", "•".repeat(3)), "three characters, seven bytes");
        w.key(&c, Key::Backspace, t);
        w.key(&c, Key::Backspace, t);
        assert_eq!(w.password().map(|s| s.bytes()), Some("é".as_bytes()));
        w.key(&c, Key::CtrlU, t);
        typed(&mut w, &"a".repeat(CAPACITY), t);
        assert!(w.field.as_ref().is_some_and(|f| !f.overflow), "256 bytes fit");
        assert!(password_row_text(&w).starts_with("   Password  …"), "the end of the dots: {}", password_row_text(&w));
        w.key(&c, Key::Char('b'), t);
        assert_eq!(password_row_text(&w), format!("   Password  {TOO_LONG}"));
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "never sent cut short");
        w.key(&c, Key::Backspace, t);
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "still refused until Ctrl+U");
        w.key(&c, Key::CtrlU, t);
        typed(&mut w, "fake-pass", t);
        assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::RunSecret { .. }));
    }

    #[test]
    fn connecting_then_connected() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)"));
        assert!(row_of(&w, "Cafe-Libre").text().trim_end().ends_with(" …"));
        assert_eq!(w.deadline(), Some(t + CONNECT_TIMEOUT));
        assert_eq!(device_at(&mut w, ACTIVATED, Some("Maison-Test"), t), (QuickAction::Redraw, None), "still on the old one: waiting");
        assert_eq!(device_at(&mut w, 50, Some("Cafe-Libre"), t), (QuickAction::Redraw, None), "activating");
        assert_eq!(device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t), (QuickAction::Redraw, None));
        assert!(w.attempt.is_none() && w.deadline().is_none(), "activated on that SSID: done");
        assert!(row_of(&w, "Cafe-Libre").text().starts_with(" ● Cafe-Libre"), "● once NetworkManager says so");
    }

    /// Spec § 2's mockup: the bars and the lock sit in one column whatever the tag; the tag's
    /// room is the widest tag's, `failed` (6 cells; Mae's short tags, 2026-10-12: `…` while
    /// connecting), so an SSID keeps 16 cells.
    #[test]
    fn the_bars_and_the_lock_sit_in_one_column() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        let column = |label: &str| {
            let chars: Vec<char> = row_of(&w, label).text().chars().collect();
            let bars = chars.iter().position(|ch| BARS.contains(ch)).expect("bars");
            (bars, chars[bars + 5])
        };
        assert!(row_of(&w, "Cafe-Libre").text().trim_end().ends_with(" …"), "{:?}", row_of(&w, "Cafe-Libre").text());
        let (at, _) = column("Cafe-Libre");
        assert_eq!(column("Labo-Test"), (at, LOCK), "untagged: the same column");
        assert_eq!(column("Maison-Test"), (at, LOCK), "saved: the same column");
        let end = usize::from(crate::quick::WIDTH + 2);
        assert_eq!(at, end - 1 - (4 + 1 + 1 + 1 + 6), "room for the widest tag, `failed`");
        let mut long = state();
        device(&mut long).access_points.as_mut().expect("aps")[0].ssid = b"Sixteen-Cells-Id-and-more".to_vec();
        let w = open(long);
        let row = row_of(&w, "Sixteen-Cells-Id-and-more").text();
        assert!(row.starts_with("   Sixteen-Cells-I… "), "16 cells for an SSID: {row:?}");
    }

    #[test]
    fn a_failed_new_network_shows_failed_and_its_new_profile_is_deleted() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        typed(&mut w, "fake-pass", t);
        assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::RunSecret { .. }));
        w.password_sent();
        device_at(&mut w, 60, Some("Bureau-Test"), t);
        saved_added(&mut w, NEW, "Bureau-Test", t);
        let later = t + Duration::from_secs(3);
        assert_eq!(device_at(&mut w, 30, None, later), (delete(NEW), Some("the activation failed")));
        let g = row_of(&w, "Bureau-Test");
        assert!(g.text().trim_end().ends_with("failed"));
        assert!(g.styles().trim_end_matches('.').ends_with("wwwwww"), "in warn: {}", g.styles());
        assert_eq!(w.deadline(), Some(later + FAILED_SHOWN));
        assert_eq!(w.due(later + FAILED_SHOWN), (vec![], None, true), "4 s later: gone");
        assert!(w.failed.is_none() && w.deadline().is_none());
    }

    #[test]
    fn state_120_fails_at_once_and_twenty_seconds_without_an_answer_too() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        assert_eq!(
            device_at(&mut w, FAILED, None, t),
            (QuickAction::Redraw, Some("the activation failed")),
            "no new profile: nothing to delete"
        );
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        saved_added(&mut w, NEW, "Cafe-Libre", t);
        assert_eq!(w.due(t + CONNECT_TIMEOUT - Duration::from_millis(1)), (vec![], None, false));
        assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![delete(NEW)], Some("no answer in 20 s"), true));
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        assert_eq!(w.set_state(None, t), (QuickAction::Redraw, Some("NetworkManager went away")));
        assert!(w.attempt.is_none() && w.failed.is_none());
    }

    /// Review Focus 4: one attempt at a time; More… still works.
    #[test]
    fn one_attempt_at_a_time() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }));
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "Enter again: one nmcli");
        pick(&mut w, "Maison-Test");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "a disconnect waits too");
        pick(&mut w, "Bureau-Test");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "no field while connecting");
        assert!(w.field.is_none());
        pick(&mut w, "Labo-Test");
        assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
        assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::RunAndClose { .. }), "More… still runs");
    }

    /// Review Focus 5: the cleanup deletes only a profile that appeared during the attempt, and only
    /// while it is still saved.
    #[test]
    fn the_cleanup_deletes_only_the_profile_the_attempt_made() {
        let (c, t) = (config(), Instant::now());
        let mut s = state();
        device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
        let mut w = open(s);
        pick(&mut w, "Cafe-Libre");
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", CAFE], "wifi connect (saved)"));
        device_at(&mut w, 50, Some("Cafe-Libre"), t);
        assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, Some("the activation failed")), "an old profile: kept");
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        device_at(&mut w, 50, Some("Cafe-Libre"), t);
        saved_added(&mut w, NEW, "Cafe-Libre", t);
        let mut gone = w.state.clone().expect("a state");
        device(&mut gone).saved.as_mut().expect("saved").retain(|p| p.uuid != NEW);
        device(&mut gone).state = 30;
        assert_eq!(
            w.set_state(Some(gone), t),
            (QuickAction::Redraw, Some("the activation failed")),
            "nmcli deleted it first: nothing to delete"
        );
    }

    /// Decision 10: the detail's reading outlives the detail while connecting.
    #[test]
    fn the_detail_s_reading_outlives_the_detail_while_connecting() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        w.close_detail();
        assert_eq!(w.scope(), WifiScope::Detail, "the new profile must still be seen");
        device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t);
        assert_eq!(w.scope(), WifiScope::Row);
    }

    /// The panel closing on `w` with its attempt under way: the follower it leaves (Mae's ruling P1).
    fn closed_during(w: &mut Wifi) -> Option<WifiFollow> {
        WifiFollow::leaving(w)
    }

    /// `base` with its device at `state` on `active`, and `saved` added.
    fn followed(base: &WifiState, state: u32, active: Option<&str>, saved: &[(&str, &str)]) -> WifiState {
        let mut s = base.clone();
        let d = device(&mut s);
        d.state = state;
        d.active = active.map(|a| ap(a, 60, Security::Psk));
        for (uuid, ssid) in saved {
            d.saved.get_or_insert_with(Vec::new).push(SavedWifi { uuid: (*uuid).into(), ssid: ssid.as_bytes().to_vec() });
        }
        s
    }

    /// Mae's ruling P1: closing the panel during an attempt on a new network leaves a follower that
    /// still deletes the profile a failure leaves behind, by NetworkManager's word or the timeout.
    #[test]
    fn closing_during_a_failing_new_network_attempt_still_deletes_its_profile() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        typed(&mut w, "fake-pass", t);
        w.key(&c, Key::Enter, t);
        w.password_sent();
        let mut f = closed_during(&mut w).expect("a new network: followed");
        assert!(w.attempt.is_none(), "moved out of the panel");
        assert_eq!((f.scope(), f.deadline()), (WifiScope::Detail, t + CONNECT_TIMEOUT));
        let base = state();
        assert_eq!(f.set_state(Some(followed(&base, 60, Some("Bureau-Test"), &[(NEW, "Bureau-Test")]))), Outcome::Waiting);
        assert_eq!(
            f.set_state(Some(followed(&base, 30, None, &[(NEW, "Bureau-Test")]))),
            Outcome::Failed { why: "the activation failed", cleanup: Some(delete(NEW)) }
        );
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        let mut f = closed_during(&mut w).expect("an open network: followed");
        assert_eq!(f.set_state(Some(followed(&base, ACTIVATED, Some("Maison-Test"), &[(NEW, "Cafe-Libre")]))), Outcome::Waiting);
        assert_eq!(f.due(t + CONNECT_TIMEOUT - Duration::from_millis(1)), Outcome::Waiting);
        assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Failed { why: "no answer in 20 s", cleanup: Some(delete(NEW)) });
    }

    /// Mae's ruling P1: a follower whose attempt succeeds deletes nothing; a saved network's
    /// attempt is dropped with the panel; a panel opened again takes the attempt back.
    #[test]
    fn a_follower_deletes_nothing_on_success_and_a_saved_network_is_not_followed() {
        let (c, t) = (config(), Instant::now());
        let base = state();
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        let mut f = closed_during(&mut w).expect("followed");
        assert_eq!(f.set_state(Some(followed(&base, 50, Some("Cafe-Libre"), &[(NEW, "Cafe-Libre")]))), Outcome::Waiting);
        assert_eq!(f.set_state(Some(followed(&base, ACTIVATED, Some("Cafe-Libre"), &[(NEW, "Cafe-Libre")]))), Outcome::Connected);
        assert_eq!(f.set_state(None), Outcome::Gone, "NetworkManager gone: nothing left to clean");
        let mut s = state();
        let d = device(&mut s);
        (d.state, d.active, d.active_uuid) = (30, None, None);
        let mut w = open(s);
        pick(&mut w, "Maison-Test");
        assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }), "connection up uuid");
        assert!(closed_during(&mut w).is_none(), "a saved network leaves no profile behind: not followed");
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        let f = closed_during(&mut w).expect("followed");
        let mut again = open(state());
        f.resume(&mut again);
        assert!(again.attempt.as_ref().is_some_and(|a| a.ssid == b"Cafe-Libre"), "the reopened panel inherits it");
        assert_eq!(again.scope(), WifiScope::Detail);
        pick(&mut again, "Bureau-Test");
        assert_eq!(again.key(&c, Key::Enter, t), QuickAction::Nothing, "no second attempt");
    }

    /// F4: the state the D-Bus thread resends through another domain's outage (BlueZ, UPower, the
    /// power profile) was read before it, and nothing new comes for the backoff: an attempt that
    /// expires on it never deletes (NetworkManager may have connected meanwhile); a state read
    /// after the outage, still disconnected, deletes as before. The display stays as it was.
    #[test]
    fn an_attempt_expiring_on_a_state_from_before_an_outage_keeps_its_profile() {
        let (c, t) = (config(), Instant::now());
        let base = state();
        // Between the old network's deactivation and ours preparing, the new profile listed.
        let between = followed(&base, 30, None, &[(NEW, "Bureau-Test")]);
        let stale = WifiState { stale: true, ..between.clone() };
        let attempt = || {
            let mut w = open(state());
            pick(&mut w, "Bureau-Test");
            w.key(&c, Key::Enter, t);
            typed(&mut w, "fake-pass", t);
            w.key(&c, Key::Enter, t);
            w.password_sent();
            w
        };
        // The follower.
        let mut f = closed_during(&mut attempt()).expect("followed");
        assert_eq!(f.set_state(Some(between.clone())), Outcome::Waiting);
        assert_eq!(f.set_state(Some(stale.clone())), Outcome::Waiting);
        assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Failed { why: "no fresh state since an outage", cleanup: None });
        let mut f = closed_during(&mut attempt()).expect("followed");
        assert_eq!(f.set_state(Some(stale.clone())), Outcome::Waiting);
        assert_eq!(f.set_state(Some(between.clone())), Outcome::Waiting, "read after the outage");
        assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Failed { why: "no answer in 20 s", cleanup: Some(delete(NEW)) });
        // The panel: `failed` shown all the same, the profile kept.
        let mut w = attempt();
        w.set_state(Some(between.clone()), t);
        let shown = (w.value(), w.networks());
        w.set_state(Some(stale.clone()), t);
        assert_eq!((w.value(), w.networks()), shown, "the display unchanged");
        let (run, failed, redraw) = w.due(t + CONNECT_TIMEOUT);
        assert_eq!((run, failed, redraw), (vec![], Some("no fresh state since an outage"), true));
        assert!(w.failed.as_ref().is_some_and(|(ssid, _)| ssid == b"Bureau-Test"), "shown failed");
        let mut w = attempt();
        w.set_state(Some(stale), t);
        w.set_state(Some(between), t);
        assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![delete(NEW)], Some("no answer in 20 s"), true));
    }

    /// Controller reconciliation P12: an open field whose network leaves the list (an access point
    /// aged out while typing) stays, its row with it, for FIELD_KEPT; then it goes, wiped.
    #[test]
    fn a_field_outlives_its_network_leaving_the_list_for_a_while() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        typed(&mut w, "fake-pass", t);
        let mut s = state();
        device(&mut s).access_points.as_mut().expect("listed").retain(|a| a.ssid != b"Bureau-Test");
        w.set_state(Some(s.clone()), t);
        w.fit(&c);
        assert!(w.field.is_some() && labels(&w).contains(&"Bureau-Test".to_string()), "kept, its row too");
        assert_eq!(w.deadline(), Some(t + FIELD_KEPT));
        assert_eq!(w.due(t + FIELD_KEPT - Duration::from_millis(1)), (vec![], None, false));
        w.set_state(Some(state()), t + Duration::from_secs(5));
        assert_eq!(w.deadline(), None, "back in the list: no deadline");
        w.set_state(Some(s.clone()), t + Duration::from_secs(6));
        assert_eq!(w.deadline(), Some(t + Duration::from_secs(6) + FIELD_KEPT), "a new absence, counted again");
        assert_eq!(w.due(t + Duration::from_secs(6) + FIELD_KEPT), (vec![], None, true));
        w.fit(&c);
        assert!(w.field.is_none() && !labels(&w).contains(&"Bureau-Test".to_string()), "gone after FIELD_KEPT");
        assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the first pick");
    }

    /// Task 5 review: an SSID of zeros or one that cleans to nothing is hidden; a connected hidden
    /// network shows `hidden` as the row's value.
    #[test]
    fn hidden_ssids_are_left_out_and_a_connected_one_says_so() {
        let mut s = state();
        let aps = device(&mut s).access_points.as_mut().expect("listed");
        aps.push(AccessPoint { ssid: vec![0, 0, 0], strength: 99, security: Security::Open });
        aps.push(AccessPoint { ssid: b"\x07\x1b".to_vec(), strength: 98, security: Security::Open });
        assert_eq!(labels(&open(s)), ["Maison-Test", "Labo-Test", "Bureau-Test", "Cafe-Libre"]);
        let mut s = state();
        device(&mut s).active = Some(AccessPoint { ssid: vec![0; 8], strength: 80, security: Security::Psk });
        let w = open(s);
        assert_eq!(w.value(), "hidden");
        assert!(!labels(&w).iter().any(|l| l.is_empty()), "{:?}", labels(&w));
    }

    /// Task 5 review: nmcli reads a BSSID-looking SSID as a BSSID, and h, he, hel, help as help.
    #[test]
    fn bssid_and_help_lookalikes_are_refused() {
        for ssid in ["00:00:5E:00:53:01", "00-00-5e-00-53-01", "0:0:5E:0:53:1", "h", "he", "hel", "help"] {
            assert!(refused(ssid.as_bytes()), "{ssid}");
        }
        for ssid in ["helpdesk", "Help", "00:00:5E:00:53", "00:00:5E:00:53:0G", "Maison-Test:01"] {
            assert!(!refused(ssid.as_bytes()), "{ssid}");
        }
    }

    /// Task 5 review: the selection is an SSID, not a position: it follows its network across a
    /// reorder.
    /// Task 7 (headless): the detail opens before its list is read (`Searching…`); the selection
    /// waits for the list, then lands on the first network, never left on `More…`. An empty list
    /// read, or the radio off: `More…` is the only pick.
    #[test]
    fn the_selection_waits_for_the_list_then_takes_the_first_network() {
        let mut unread = state();
        let d = device(&mut unread);
        (d.state, d.active, d.active_uuid, d.access_points) = (30, None, None, None);
        let mut w = open(unread);
        assert_eq!(w.pick, Pick::None, "searching: nothing picked yet");
        w.state = Some(state());
        w.fit(&config());
        assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the list read: its first network");
        let mut empty = state();
        device(&mut empty).access_points = Some(vec![]);
        device(&mut empty).active = None;
        assert_eq!(open(empty).pick, Pick::More, "no networks: More…");
        let mut off = state();
        off.enabled = false;
        assert_eq!(open(off).pick, Pick::More, "off: More…");
    }

    /// Fix round 1 (minor a): a Down while searching picks `More…` (the only pick then); the first
    /// list read moves it to the first network, once: `More…` chosen after the list stays.
    #[test]
    fn more_chosen_while_searching_gives_way_to_the_first_network() {
        let mut unread = state();
        let d = device(&mut unread);
        (d.state, d.active, d.active_uuid, d.access_points) = (30, None, None, None);
        let mut w = open(unread);
        let t = Instant::now();
        w.key(&config(), Key::Down, t);
        assert_eq!(w.pick, Pick::More, "searching: More… the only pick");
        w.state = Some(state());
        w.fit(&config());
        assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the list read: its first network");
        w.key(&config(), Key::End, t);
        w.fit(&config());
        assert_eq!(w.pick, Pick::More, "chosen once the list is read: kept");
    }

    #[test]
    fn the_pick_follows_its_ssid_across_a_reorder() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        let mut s = state();
        for a in device(&mut s).access_points.as_mut().expect("listed") {
            if a.ssid == b"Cafe-Libre" {
                a.strength = 75;
            }
        }
        w.set_state(Some(s), t);
        w.fit(&c);
        assert_eq!(labels(&w)[1], "Cafe-Libre", "moved up");
        assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
        let nets = w.networks();
        assert!(w.is_selected(Line::Network(1), &nets) && !w.is_selected(Line::Network(3), &nets));
        assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)"));
    }

    /// Fix round 1 (I2): an attempt begun before the saved list was read whole (a failed listing,
    /// more than MAX_SAVED profiles: the reader sends None) knows no profile, so it deletes none,
    /// even one of its SSID that shows later.
    #[test]
    fn an_attempt_begun_without_the_whole_saved_list_deletes_nothing() {
        let (c, t) = (config(), Instant::now());
        let mut s = state();
        device(&mut s).saved = None;
        let mut w = open(s);
        pick(&mut w, "Cafe-Libre");
        assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }));
        assert_eq!(w.attempt.as_ref().map(|a| a.known.clone()), Some(None));
        let mut listed = state();
        device(&mut listed).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
        w.set_state(Some(listed), t);
        device_at(&mut w, 50, Some("Cafe-Libre"), t);
        assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, Some("the activation failed")), "an older profile, never deleted");
    }

    /// Fix round 1 (minor 1): a saved network's `connection up` never deletes, even a profile of its
    /// SSID that appeared meanwhile.
    #[test]
    fn a_connection_up_attempt_never_deletes() {
        let (c, t) = (config(), Instant::now());
        let mut s = state();
        device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
        let mut w = open(s);
        pick(&mut w, "Cafe-Libre");
        assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }));
        saved_added(&mut w, NEW, "Cafe-Libre", t);
        device_at(&mut w, 50, Some("Cafe-Libre"), t);
        assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, Some("the activation failed")));
    }

    /// Fix round 1 (minor 2): an activation on another network (an autoconnect) is not ours; an
    /// activated device whose access point could not be read is no word yet; another network taking
    /// the device once ours was under way is a failure.
    #[test]
    fn only_its_own_network_counts_for_an_attempt() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        assert_eq!(device_at(&mut w, 50, Some("Bureau-Test"), t), (QuickAction::Redraw, None), "an autoconnect elsewhere");
        assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, None), "ours never began: still waiting");
        assert!(w.attempt.as_ref().is_some_and(|a| !a.activating));
        assert_eq!(device_at(&mut w, 60, None, t), (QuickAction::Redraw, None), "unread access point: not ours yet");
        device_at(&mut w, 60, Some("Cafe-Libre"), t);
        assert_eq!(device_at(&mut w, ACTIVATED, None, t), (QuickAction::Redraw, None), "activated, its access point unread");
        assert!(w.attempt.is_some());
        assert_eq!(device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t), (QuickAction::Redraw, None));
        assert!(w.attempt.is_none(), "connected");
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        device_at(&mut w, 70, Some("Cafe-Libre"), t);
        assert_eq!(
            device_at(&mut w, 50, Some("Bureau-Test"), t),
            (QuickAction::Redraw, Some("the activation failed")),
            "another took over"
        );
    }

    /// Fix round 2: activated on the profile the attempt made counts even with its access point
    /// unread; and a timeout while the device was last seen activated deletes nothing.
    #[test]
    fn an_activated_device_is_never_cleaned_up() {
        let (c, t) = (config(), Instant::now());
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        saved_added(&mut w, NEW, "Cafe-Libre", t);
        let mut s = w.state.clone().expect("a state");
        let d = device(&mut s);
        (d.state, d.active, d.active_uuid) = (ACTIVATED, None, Some(NEW.into()));
        assert_eq!(w.set_state(Some(s), t), (QuickAction::Redraw, None));
        assert!(w.attempt.is_none(), "activated on its new profile: connected");
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        saved_added(&mut w, NEW, "Cafe-Libre", t);
        let mut s = w.state.clone().expect("a state");
        let d = device(&mut s);
        (d.state, d.active, d.active_uuid) = (ACTIVATED, None, None);
        w.set_state(Some(s), t);
        assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![], Some("no answer in 20 s"), true), "last seen activated: nothing deleted");
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        saved_added(&mut w, NEW, "Cafe-Libre", t);
        let mut s = w.state.clone().expect("a state");
        device(&mut s).active = None;
        w.set_state(Some(s), t);
        assert_eq!(
            w.due(t + CONNECT_TIMEOUT).0,
            vec![delete(NEW)],
            "activated on another profile's uuid (the old one): a failure, cleaned"
        );
    }

    /// Fix round 3 (controller ruling): a slow activation never loses its profile. Still activating
    /// on its network at 20 s: the deadline moves to 60 s from the start; still activating then: it
    /// ends, nothing deleted; activated meanwhile: connected. The follower keeps the same deadline.
    #[test]
    fn a_slow_activation_keeps_its_profile() {
        let (c, t) = (config(), Instant::now());
        let slow = |active: Option<&str>| {
            let mut w = open(state());
            pick(&mut w, "Cafe-Libre");
            w.key(&c, Key::Enter, t);
            saved_added(&mut w, NEW, "Cafe-Libre", t);
            device_at(&mut w, 70, active, t);
            w
        };
        let mut w = slow(Some("Cafe-Libre"));
        assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![], None, false), "20 s, still activating: no delete");
        assert_eq!(w.deadline(), Some(t + ACTIVATION_BOUND));
        assert!(row_of(&w, "Cafe-Libre").text().trim_end().ends_with(" …"));
        assert_eq!(w.due(t + ACTIVATION_BOUND), (vec![], Some("still activating after 60 s"), true), "60 s: ends, its profile kept");
        assert!(w.attempt.is_none());
        let mut w = slow(None);
        assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![], None, false), "its access point unread: waits too");
        let mut w = slow(Some("Cafe-Libre"));
        w.due(t + CONNECT_TIMEOUT);
        assert_eq!(device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t + Duration::from_secs(30)), (QuickAction::Redraw, None));
        assert!(w.attempt.is_none() && w.failed.is_none(), "connected at 30 s, nothing deleted");
        let mut w = slow(Some("Bureau-Test"));
        assert_eq!(w.due(t + CONNECT_TIMEOUT).0, vec![delete(NEW)], "activating another network: a failure, cleaned");
        let mut w = slow(Some("Cafe-Libre"));
        let mut f = WifiFollow::leaving(&mut w).expect("followed");
        assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Waiting);
        assert_eq!(f.deadline(), t + ACTIVATION_BOUND, "the follower's timer moves too");
        assert_eq!(f.due(t + ACTIVATION_BOUND), Outcome::Failed { why: "still activating after 60 s", cleanup: None });
    }
}
