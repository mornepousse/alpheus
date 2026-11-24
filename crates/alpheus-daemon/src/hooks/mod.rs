//! The event hooks' pure half: which events exist, when each one fires, and what it hands to the hook.
//!
//! A hook is a command the user wrote in `[hooks]`, run when something happens in the shell. This
//! file works out the events from what the sources already publish: `Hooks::on_msg` and its
//! siblings turn a message into edges (the changes), where each event's first value is a seed and
//! never an edge; `Hooks::fire` says whether an event runs now, after the quiet window that follows
//! the start and through the event's token bucket; `Event::env` gives the variables the hook is
//! handed. The other half, `app.rs`, feeds this file and starts the command: it reads the clock,
//! the `$PATH`, and finds out whether a systemd user manager runs, and hands this file every instant
//! it needs.
//!
//! Never:
//! - read a clock: the caller passes `now`, so a test can pick any instant;
//! - read a file, or start anything (a thread, a process, a timer);
//! - hand a hook anything but the variables of `Event::env`: event data is never an argument, and
//!   a device or profile name is cleaned and cut first (`clean_value`);
//! - panic: no unwrap, expect or panic macro in its production code (a guardrail checks).
//!
//! See: docs/features/hooks.md and
//! docs/decisions.md ("A hook fires on a change, never for the state at start", "A hook's data
//! reaches it through the environment, never the arguments", "Each hook has a token bucket",
//! "A source that is down is not a state").

use crate::config::{HooksConfig, ModuleId};
use crate::modules::Needs;
use crate::privacy::Capture;
use crate::state::{Failed, Msg};
use crate::system::{Battery, BtState, Charge, Link, NetState, PowerState, Profile, ProfileState};
use crate::text;
use alpheus_common::theme::Mode;
use std::time::{Duration, Instant};

/// How many events there are: the length of `HookId::ALL`.
pub const COUNT: usize = 13;
/// After start, a change only moves its seed, so that the state at start never fires a hook: the
/// portal's first answer, the first graph, a profile applied at start. `HookId::Started` alone
/// ignores it. See docs/decisions.md, "A hook fires on a change, never for the state at start".
pub const QUIET: Duration = Duration::from_secs(3);
/// A hook may run this many times in a burst, then once per `REFILL`. See docs/decisions.md, "Each
/// hook has a token bucket".
pub const BURST: u32 = 5;
/// The time it takes the bucket of one event to give back one run.
pub const REFILL: Duration = Duration::from_secs(3);
/// The most bytes of a value handed to a hook (a device or profile name).
pub const VALUE_MAX: usize = 255;

/// The events a hook can be set on. Each one is a key of `[hooks]` (`HookId::key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookId {
    Started,
    ThemeMode,
    Dnd,
    BatteryCharge,
    BatteryLevel,
    PowerProfile,
    Network,
    Vpn,
    Bluetooth,
    Privacy,
    Displays,
    Shield,
    FailedUnits,
}

impl HookId {
    /// Every event, in the order of `HooksConfig::commands` (a test checks it).
    pub const ALL: [HookId; COUNT] = [
        HookId::Started,
        HookId::ThemeMode,
        HookId::Dnd,
        HookId::BatteryCharge,
        HookId::BatteryLevel,
        HookId::PowerProfile,
        HookId::Network,
        HookId::Vpn,
        HookId::Bluetooth,
        HookId::Privacy,
        HookId::Displays,
        HookId::Shield,
        HookId::FailedUnits,
    ];

    /// The `[hooks]` key.
    pub fn key(self) -> &'static str {
        match self {
            HookId::Started => "started",
            HookId::ThemeMode => "theme_mode",
            HookId::Dnd => "dnd",
            HookId::BatteryCharge => "battery_charge",
            HookId::BatteryLevel => "battery_level",
            HookId::PowerProfile => "power_profile",
            HookId::Network => "network",
            HookId::Vpn => "vpn",
            HookId::Bluetooth => "bluetooth",
            HookId::Privacy => "privacy",
            HookId::Displays => "displays",
            HookId::Shield => "shield",
            HookId::FailedUnits => "failed_units",
        }
    }

    /// Whether the configured `modules` (or `[shield]`) read what this event comes from. A hook
    /// never adds a source: an event nothing reads never fires. See docs/decisions.md, "A hook
    /// never adds a source".
    pub fn reads(self, modules: &[ModuleId], shield_configured: bool) -> bool {
        // A closure that captures `modules`: `Needs::of` works out what the modules read.
        let needs = || Needs::of(modules);
        match self {
            HookId::Started | HookId::ThemeMode => true,
            HookId::Dnd => modules.contains(&ModuleId::Notifications),
            HookId::BatteryCharge | HookId::BatteryLevel => needs().contains(Needs::BATTERY),
            HookId::PowerProfile => needs().contains(Needs::POWER_PROFILE),
            HookId::Network | HookId::Vpn => needs().contains(Needs::NETWORK),
            HookId::Bluetooth => needs().contains(Needs::BLUETOOTH),
            HookId::Privacy => modules.contains(&ModuleId::Privacy),
            HookId::Displays => modules.contains(&ModuleId::Displays),
            // The flag is watched by `ShieldApp`, which exists with a configured `[shield]` alone.
            HookId::Shield => shield_configured,
            HookId::FailedUnits => needs().contains(Needs::UNITS),
        }
    }

    /// What the log says is missing when `reads` is false.
    pub fn needs(self) -> &'static str {
        match self {
            HookId::Started | HookId::ThemeMode => "",
            // Every path to `dnd_apply` needs the module: the bell, the quick panel's row (shown only
            // with it), the verb and `dnd_while_sharing`.
            HookId::Dnd => "notifications",
            HookId::BatteryCharge | HookId::BatteryLevel => "battery",
            HookId::PowerProfile => "quick",
            HookId::Network | HookId::Vpn => "network or quick",
            HookId::Bluetooth => "bluetooth or quick",
            HookId::Privacy => "privacy",
            HookId::Displays => "displays",
            HookId::Shield => "[shield]",
            HookId::FailedUnits => "failed",
        }
    }

    /// The event's place in `ALL`, which is also its place in the arrays of `Hooks`.
    fn index(self) -> usize {
        self as usize
    }
}

/// The three things the privacy indicators watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    Microphone,
    Camera,
    Screen,
}

/// What changed about Bluetooth: the adapter, or one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtChange {
    On,
    Off,
    /// A device connected: its name.
    Connected(String),
    /// A device disconnected: its name.
    Disconnected(String),
}

/// One change that may fire a hook, with the data its variables are made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Started,
    ThemeMode(Mode),
    Dnd(bool),
    BatteryCharge(Battery),
    BatteryLevel(Battery),
    PowerProfile(Profile),
    /// The primary connection's kind; None: offline.
    Network(Option<Link>),
    Vpn(bool),
    Bluetooth(BtChange),
    Privacy(CaptureKind, bool),
    /// The applied profile's name.
    Displays(String),
    /// Shield mode turned on (true) or off.
    Shield(bool),
    /// The failed units' total changed; each manager's count when known.
    FailedUnits {
        total: u32,
        system: Option<u32>,
        user: Option<u32>,
    },
}

/// Every variable a hook may be given. One an event does not set is removed from the hook's
/// environment (`unset_for`), so an inherited one never passes for the event's.
pub const VARS: [&str; 18] = [
    "ALPHEUS_EVENT",
    "ALPHEUS_MODE",
    "ALPHEUS_DND",
    "ALPHEUS_CHARGE",
    "ALPHEUS_PERCENT",
    "ALPHEUS_POWER_PROFILE",
    "ALPHEUS_NETWORK",
    "ALPHEUS_LINK",
    "ALPHEUS_VPN",
    "ALPHEUS_BLUETOOTH",
    "ALPHEUS_DEVICE",
    "ALPHEUS_CAPTURE",
    "ALPHEUS_STATE",
    "ALPHEUS_DISPLAY_PROFILE",
    "ALPHEUS_SHIELD",
    "ALPHEUS_FAILED",
    "ALPHEUS_FAILED_SYSTEM",
    "ALPHEUS_FAILED_USER",
];

/// The names of `VARS` that `env` does not set.
pub fn unset_for(env: &[(&'static str, String)]) -> Vec<&'static str> {
    let mut unset = Vec::new();
    for var in VARS {
        let is_set = env.iter().any(|(name, _)| *name == var);
        if !is_set {
            unset.push(var);
        }
    }
    unset
}

/// Whether `s` is six hex pairs joined by `sep`.
fn six_hex_pairs(s: &str, sep: char) -> bool {
    let mut pairs = 0;
    for part in s.split(sep) {
        pairs += 1;
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    pairs == 6
}

/// Whether `s` is a Bluetooth address (six hex pairs joined by `:` or by `-`). An address is never
/// handed to a hook: what a hook gets holds no address (docs/decisions.md, "A hook's data reaches it through the
/// environment, never the arguments"), and a device with
/// no name of its own is shown by its address.
fn address_like(s: &str) -> bool {
    six_hex_pairs(s, ':') || six_hex_pairs(s, '-')
}

/// `text::clean`, then at most `VALUE_MAX` bytes cut on a character boundary.
pub fn clean_value(s: &str) -> String {
    let mut value = text::clean(s);
    if value.len() > VALUE_MAX {
        // A cut in the middle of a character would not be valid text: step back to the start of
        // that character. Index 0 is always a boundary, so this ends.
        let mut end = VALUE_MAX;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

/// The word a hook reads for a switch.
fn on_off(on: bool) -> String {
    if on { "on".to_string() } else { "off".to_string() }
}

/// The word a hook reads for a charge state.
fn charge_word(c: Charge) -> &'static str {
    match c {
        Charge::Charging => "charging",
        Charge::Discharging => "discharging",
        Charge::Full => "full",
        Charge::PendingCharge => "pending",
        Charge::Unknown => "unknown",
    }
}

/// Keeps `new` as the event's seed, and says whether it is a change: true only when a seed was
/// already there and differs from `new`. The first value (no seed yet) is never a change.
///
/// It stores the seed whatever it answers, so callers put it last in an `&&`: the seed then moves
/// only when the checks before it pass.
///
/// (The `<T: Copy + PartialEq>` after the name says: any type that can be copied and compared.)
/// It is generic because six events (network, VPN, battery charge, battery level, power profile and
/// theme mode) keep a seed of six different types and need this one comparison rule, instead of six
/// copies of it.
fn changed<T: Copy + PartialEq>(seed: &mut Option<T>, new: T) -> bool {
    match seed.replace(new) {
        Some(old) => old != new,
        None => false,
    }
}

impl Event {
    /// Which hook this event runs.
    pub fn id(&self) -> HookId {
        match self {
            Event::Started => HookId::Started,
            Event::ThemeMode(_) => HookId::ThemeMode,
            Event::Dnd(_) => HookId::Dnd,
            Event::BatteryCharge(_) => HookId::BatteryCharge,
            Event::BatteryLevel(_) => HookId::BatteryLevel,
            Event::PowerProfile(_) => HookId::PowerProfile,
            Event::Network(_) => HookId::Network,
            Event::Vpn(_) => HookId::Vpn,
            Event::Bluetooth(_) => HookId::Bluetooth,
            Event::Privacy(..) => HookId::Privacy,
            Event::Displays(_) => HookId::Displays,
            Event::Shield(_) => HookId::Shield,
            Event::FailedUnits { .. } => HookId::FailedUnits,
        }
    }

    /// The variables the hook is given: `ALPHEUS_EVENT` first, then the event's own (the order of
    /// the variables table in docs/features/hooks.md).
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![("ALPHEUS_EVENT", self.id().key().to_string())];
        match self {
            Event::Started => {}
            Event::ThemeMode(m) => {
                let word = if *m == Mode::Dark { "dark" } else { "light" };
                env.push(("ALPHEUS_MODE", word.into()));
            }
            Event::Dnd(b) => env.push(("ALPHEUS_DND", on_off(*b))),
            Event::BatteryCharge(b) | Event::BatteryLevel(b) => {
                env.push(("ALPHEUS_CHARGE", charge_word(b.charge).into()));
                env.push(("ALPHEUS_PERCENT", b.percent.to_string()));
            }
            Event::PowerProfile(p) => env.push(("ALPHEUS_POWER_PROFILE", p.name().into())),
            Event::Network(link) => {
                let online = if link.is_some() { "online" } else { "offline" };
                env.push(("ALPHEUS_NETWORK", online.into()));
                let word = match link {
                    Some(Link::Wifi) => "wifi",
                    Some(Link::Ethernet) => "ethernet",
                    Some(Link::Vpn) => "vpn",
                    Some(Link::Other) => "other",
                    None => "none",
                };
                env.push(("ALPHEUS_LINK", word.into()));
            }
            Event::Vpn(b) => env.push(("ALPHEUS_VPN", on_off(*b))),
            Event::Bluetooth(change) => {
                // The device's name goes with a connection or a disconnection only.
                let (word, device) = match change {
                    BtChange::On => ("on", None),
                    BtChange::Off => ("off", None),
                    BtChange::Connected(n) => ("connected", Some(n)),
                    BtChange::Disconnected(n) => ("disconnected", Some(n)),
                };
                env.push(("ALPHEUS_BLUETOOTH", word.into()));
                if let Some(name) = device
                    && !address_like(name)
                {
                    env.push(("ALPHEUS_DEVICE", clean_value(name)));
                }
            }
            Event::Privacy(kind, b) => {
                let word = match kind {
                    CaptureKind::Microphone => "microphone",
                    CaptureKind::Camera => "camera",
                    CaptureKind::Screen => "screen",
                };
                env.push(("ALPHEUS_CAPTURE", word.into()));
                env.push(("ALPHEUS_STATE", on_off(*b)));
            }
            Event::Displays(name) => env.push(("ALPHEUS_DISPLAY_PROFILE", clean_value(name))),
            Event::Shield(b) => env.push(("ALPHEUS_SHIELD", on_off(*b))),
            Event::FailedUnits { total, system, user } => {
                env.push(("ALPHEUS_FAILED", total.to_string()));
                // A manager's count is given only once that manager has answered.
                if let Some(n) = system {
                    env.push(("ALPHEUS_FAILED_SYSTEM", n.to_string()));
                }
                if let Some(n) = user {
                    env.push(("ALPHEUS_FAILED_USER", n.to_string()));
                }
            }
        }
        env
    }
}

/// What to run: the hook's argv and the event's variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fire {
    pub id: HookId,
    pub argv: Vec<String>,
    pub env: Vec<(&'static str, String)>,
    /// The hook variables `env` leaves unset: removed from the inherited environment.
    pub unset: Vec<&'static str>,
}

/// What `Hooks::fire` decides about one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Run(Fire),
    /// The bucket ran dry: said once per episode, for one log line.
    Throttled(HookId),
    /// Nothing to do: no hook set, inside the quiet window, or already said to be throttled.
    Dropped,
}

/// The token bucket of one event.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Runs left: at most `BURST`.
    tokens: u32,
    /// The instant the tokens were last counted; a token comes back each `REFILL` after it.
    at: Instant,
    /// The drop of this episode was already said (until the next run).
    throttled: bool,
}

/// The hooks' state: the commands, the seeds of every event, the quiet window and the buckets.
pub struct Hooks {
    /// The configured argv of each event, in the order of `HookId::ALL`; empty: no hook.
    commands: [Vec<String>; COUNT],
    quiet_until: Instant,
    buckets: [Bucket; COUNT],
    // The seeds of the `Option` fields below: the last value seen of each event. None until the
    // first one arrives, which is then a seed and never a change.
    network: Option<Option<Link>>,
    vpn: Option<bool>,
    /// Powered, the connected devices' names sorted.
    bluetooth: Option<(bool, Vec<String>)>,
    charge: Option<Charge>,
    percent: Option<u8>,
    profile: Option<Profile>,
    mode: Option<Mode>,
    /// The indicators last seen, in the order microphone, camera, screen.
    capture: [bool; 3],
    /// The sound source and the compositor are down: their part of the capture is no state.
    sound_down: bool,
    compositor_down: bool,
    /// `quiet_over` already said so.
    quiet_said: bool,
    /// The last count of each manager that was known, and the total last seen.
    failed: Failed,
    failed_total: Option<u32>,
}

impl Hooks {
    /// None when no hook is set: the daemon then keeps nothing of this module.
    pub fn new(cfg: &HooksConfig, now: Instant) -> Option<Hooks> {
        if cfg.is_empty() {
            return None;
        }
        // `cfg.commands()` lists the commands in the order of `HookId::ALL`.
        let mut commands: [Vec<String>; COUNT] = Default::default();
        for (slot, (_, command)) in commands.iter_mut().zip(cfg.commands()) {
            *slot = command.to_vec();
        }
        Some(Hooks {
            commands,
            quiet_until: now + QUIET,
            buckets: [Bucket { tokens: BURST, at: now, throttled: false }; COUNT],
            network: None,
            vpn: None,
            bluetooth: None,
            charge: None,
            percent: None,
            profile: None,
            mode: None,
            capture: [false; 3],
            sound_down: false,
            compositor_down: false,
            quiet_said: false,
            failed: Failed::default(),
            failed_total: None,
        })
    }

    /// Whether a hook is set for `id`.
    pub fn wants(&self, id: HookId) -> bool {
        match self.commands.get(id.index()) {
            Some(command) => !command.is_empty(),
            None => false,
        }
    }

    /// How many hooks are configured.
    pub fn set(&self) -> usize {
        self.commands.iter().filter(|c| !c.is_empty()).count()
    }

    /// The edges a source's message makes, in the order they happened; the first value is a seed.
    /// A source going down, absent or unreadable is not a state: nothing, the seed stays.
    pub fn on_msg(&mut self, m: &Msg) -> Vec<Event> {
        let mut out = Vec::new();
        match m {
            Msg::Sound(s) => self.sound_down = s.is_none(),
            Msg::Compositor(c) => self.compositor_down = c.is_none(),
            Msg::Network(Some(state)) => {
                // The kind of the primary connection (None: offline), and whether a VPN is on.
                let (link, vpn) = match state {
                    NetState::Absent => return out,
                    NetState::Offline => (None, false),
                    NetState::Online { link, vpns, .. } => (Some(*link), *vpns > 0),
                };
                if self.wants(HookId::Network) && changed(&mut self.network, link) {
                    out.push(Event::Network(link));
                }
                if self.wants(HookId::Vpn) && changed(&mut self.vpn, vpn) {
                    out.push(Event::Vpn(vpn));
                }
            }
            Msg::Bluetooth(Some(state)) if self.wants(HookId::Bluetooth) => {
                // Whether the adapter is powered, and the connected devices' names, sorted.
                let now = match state {
                    BtState::Absent => return out,
                    BtState::Off => (false, Vec::new()),
                    BtState::On { devices } => {
                        let mut names: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
                        names.sort();
                        (true, names)
                    }
                };
                if let Some((was_on, was)) = self.bluetooth.replace(now.clone()) {
                    let (is_on, names) = now;
                    // The adapter powering on comes before its devices connecting.
                    if is_on && !was_on {
                        out.push(Event::Bluetooth(BtChange::On));
                    }
                    // A multiset difference: two devices named alike count twice. A device that was
                    // connected and no longer is has disconnected; what is left of `added`
                    // after every old device was looked for is the devices that connected.
                    let mut added = names;
                    for old in was {
                        match added.iter().position(|n| *n == old) {
                            Some(i) => {
                                added.remove(i);
                            }
                            None => out.push(Event::Bluetooth(BtChange::Disconnected(old))),
                        }
                    }
                    out.extend(added.into_iter().map(|n| Event::Bluetooth(BtChange::Connected(n))));
                    // The adapter powering off comes after its devices disconnecting.
                    if !is_on && was_on {
                        out.push(Event::Bluetooth(BtChange::Off));
                    }
                }
            }
            Msg::Battery(Some(PowerState { laptop: Some(b), .. })) => {
                // An unknown charge is no state: it neither fires nor moves the seed.
                if self.wants(HookId::BatteryCharge) && b.charge != Charge::Unknown && changed(&mut self.charge, b.charge) {
                    out.push(Event::BatteryCharge(*b));
                }
                // The charge comes before the level when one reading changes both.
                if self.wants(HookId::BatteryLevel) && changed(&mut self.percent, b.percent) {
                    out.push(Event::BatteryLevel(*b));
                }
            }
            // A count that is down (None) is no state: the last known one stays.
            Msg::Units(Some(n)) if self.wants(HookId::FailedUnits) => {
                let first = self.failed.system.replace(*n).is_none();
                out.extend(self.failed_edge(first));
            }
            Msg::UserUnits(Some(n)) if self.wants(HookId::FailedUnits) => {
                let first = self.failed.user.replace(*n).is_none();
                out.extend(self.failed_edge(first));
            }
            // The condition after `if` (a guard) also stores the seed, but only when the hook is set.
            Msg::Profile(Some(ProfileState { active: Some(p), .. }))
                if self.wants(HookId::PowerProfile) && changed(&mut self.profile, *p) =>
            {
                out.push(Event::PowerProfile(*p));
            }
            _ => {}
        }
        out
    }

    /// The total of the known counts against the last one seen. A manager's first answer (`seed`),
    /// whenever it arrives, only moves the seed: it is no change, just one more count known.
    fn failed_edge(&mut self, seed: bool) -> Option<Event> {
        // `?` after a value that may be None ends the function with None: no count known yet.
        let total = crate::failed::total(&self.failed)?;
        let old = self.failed_total.replace(total);
        if seed {
            return None;
        }
        match old {
            Some(old) if old != total => Some(Event::FailedUnits { total, system: self.failed.system, user: self.failed.user }),
            _ => None,
        }
    }

    /// The mode in effect: a seed, then an edge on each change. `settled`: the mode is the user's
    /// choice or the portal has answered; before that, `auto` holds a fallback that is no state.
    pub fn on_mode(&mut self, mode: Mode, settled: bool) -> Option<Event> {
        if !settled || !self.wants(HookId::ThemeMode) {
            return None;
        }
        if changed(&mut self.mode, mode) { Some(Event::ThemeMode(mode)) } else { None }
    }

    /// The indicators against the last ones (all off at start), microphone, camera, screen. While
    /// the sound source is down the microphone, the camera and the screen are kept, while the
    /// compositor is down the screen: a source down is not a state (docs/decisions.md,
    /// "A source that is down is not a state"), and they are compared again once it is back.
    pub fn on_capture(&mut self, c: &Capture) -> Vec<Event> {
        if !self.wants(HookId::Privacy) {
            return Vec::new();
        }
        let now = [c.mic.active(), c.camera.active(), c.screen.active()];
        let kinds = [CaptureKind::Microphone, CaptureKind::Camera, CaptureKind::Screen];
        let mut out = Vec::new();
        // Which indicators keep their last value because their source is down.
        let kept = [self.sound_down, self.sound_down, self.sound_down || self.compositor_down];
        let mut next = self.capture;
        for (i, kind) in kinds.into_iter().enumerate() {
            if kept[i] {
                continue;
            }
            if next[i] != now[i] {
                out.push(Event::Privacy(kind, now[i]));
            }
            next[i] = now[i];
        }
        self.capture = next;
        out
    }

    /// True once, at the first call from `QUIET` after start on: the log's marker that the changes
    /// after it count (the headless tests tell a change inside the window from one past it).
    pub fn quiet_over(&mut self, now: Instant) -> bool {
        if now < self.quiet_until || self.quiet_said {
            return false;
        }
        self.quiet_said = true;
        true
    }

    /// Whether `e` runs now: its hook set, past the quiet window (`Started` apart), a token left.
    pub fn fire(&mut self, e: Event, now: Instant) -> Verdict {
        let id = e.id();
        if !self.wants(id) || (id != HookId::Started && now < self.quiet_until) {
            return Verdict::Dropped;
        }
        let Some(bucket) = self.buckets.get_mut(id.index()) else { return Verdict::Dropped };
        // Give back the tokens earned since `bucket.at`: one per whole `REFILL`, never past `BURST`
        // (the `min` also keeps the count small enough to convert to `u32`).
        if bucket.tokens < BURST {
            let whole = (now.saturating_duration_since(bucket.at).as_nanos() / REFILL.as_nanos()).min(u128::from(BURST)) as u32;
            bucket.tokens = (bucket.tokens + whole).min(BURST);
            bucket.at += REFILL * whole;
        }
        // A full bucket counts from now.
        if bucket.tokens == BURST {
            bucket.at = now;
        }
        if bucket.tokens == 0 {
            // The first drop of an episode is said once; the next ones are dropped in silence.
            if bucket.throttled {
                return Verdict::Dropped;
            }
            bucket.throttled = true;
            return Verdict::Throttled(id);
        }
        bucket.tokens -= 1;
        bucket.throttled = false;
        let argv = self.commands.get(id.index()).cloned().unwrap_or_default();
        let env = e.env();
        let unset = unset_for(&env);
        Verdict::Run(Fire { id, argv, env, unset })
    }
}

pub mod app;

#[cfg(test)]
mod tests;
