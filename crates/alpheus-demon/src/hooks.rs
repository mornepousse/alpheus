//! The event hooks' pure core (hooks spec §§ 2 and 4): which events exist, the edges worked out from
//! what the sources already publish (the first value of each is a seed, never an edge), the quiet
//! window after start and the per-event token bucket. Nothing here reads a file, starts a thread or
//! runs a command: `hooks_app` hands a `Fire` to the spawner.

use crate::config::{HooksConfig, ModuleId};
use crate::modules::Needs;
use crate::privacy::Capture;
use crate::state::Msg;
use crate::system::{Battery, BtState, Charge, Link, NetState, PowerState, Profile, ProfileState};
use crate::text;
use alpheus_commun::theme::Mode;
use std::time::{Duration, Instant};

pub const COUNT: usize = 11;
/// After start, an edge only moves its seed: the portal's first answer, the first graph, a profile
/// applied at start. `Started` alone ignores it.
pub const QUIET: Duration = Duration::from_secs(3);
/// A hook may run this many times in a burst, then once per `REFILL`.
pub const BURST: u32 = 5;
pub const REFILL: Duration = Duration::from_secs(3);
/// The most bytes of a value handed to a hook (a device or profile name).
pub const VALUE_MAX: usize = 255;

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
}

impl HookId {
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
        }
    }

    /// Whether the configured `modules` read what this event comes from.
    pub fn reads(self, modules: &[ModuleId]) -> bool {
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
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    Microphone,
    Camera,
    Screen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtChange {
    On,
    Off,
    Connected(String),
    Disconnected(String),
}

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
}

/// `text::clean`, then at most `VALUE_MAX` bytes cut on a character boundary.
pub fn clean_value(s: &str) -> String {
    let mut v = text::clean(s);
    if v.len() > VALUE_MAX {
        let mut end = VALUE_MAX;
        while !v.is_char_boundary(end) {
            end -= 1;
        }
        v.truncate(end);
    }
    v
}

fn on_off(b: bool) -> String {
    if b { "on" } else { "off" }.to_string()
}

fn charge_word(c: Charge) -> &'static str {
    match c {
        Charge::Charging => "charging",
        Charge::Discharging => "discharging",
        Charge::Full => "full",
        Charge::PendingCharge => "pending",
        Charge::Unknown => "unknown",
    }
}

impl Event {
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
        }
    }

    /// `ALPHEUS_EVENT` first, then the event's variables (spec § 2's order).
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![("ALPHEUS_EVENT", self.id().key().to_string())];
        match self {
            Event::Started => {}
            Event::ThemeMode(m) => env.push(("ALPHEUS_MODE", if *m == Mode::Dark { "dark" } else { "light" }.into())),
            Event::Dnd(b) => env.push(("ALPHEUS_DND", on_off(*b))),
            Event::BatteryCharge(b) | Event::BatteryLevel(b) => {
                env.push(("ALPHEUS_CHARGE", charge_word(b.charge).into()));
                env.push(("ALPHEUS_PERCENT", b.percent.to_string()));
            }
            Event::PowerProfile(p) => env.push(("ALPHEUS_POWER_PROFILE", p.name().into())),
            Event::Network(link) => {
                env.push(("ALPHEUS_NETWORK", if link.is_some() { "online" } else { "offline" }.into()));
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
                let (word, device) = match change {
                    BtChange::On => ("on", None),
                    BtChange::Off => ("off", None),
                    BtChange::Connected(n) => ("connected", Some(n)),
                    BtChange::Disconnected(n) => ("disconnected", Some(n)),
                };
                env.push(("ALPHEUS_BLUETOOTH", word.into()));
                if let Some(n) = device {
                    env.push(("ALPHEUS_DEVICE", clean_value(n)));
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Run(Fire),
    /// The bucket ran dry: said once per episode, for one log line.
    Throttled(HookId),
    Dropped,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: u32,
    at: Instant,
    throttled: bool,
}

pub struct Hooks {
    commands: [Vec<String>; COUNT],
    quiet_until: Instant,
    buckets: [Bucket; COUNT],
    network: Option<Option<Link>>,
    vpn: Option<bool>,
    /// Powered, the connected devices' names sorted.
    bluetooth: Option<(bool, Vec<String>)>,
    charge: Option<Charge>,
    percent: Option<u8>,
    profile: Option<Profile>,
    mode: Option<Mode>,
    capture: [bool; 3],
}

impl Hooks {
    /// None when no hook is set: the daemon then keeps nothing of this module.
    pub fn new(cfg: &HooksConfig, now: Instant) -> Option<Hooks> {
        if cfg.is_empty() {
            return None;
        }
        let all = cfg.commands();
        Some(Hooks {
            commands: std::array::from_fn(|i| all.get(i).map(|(_, c)| c.to_vec()).unwrap_or_default()),
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
        })
    }

    pub fn wants(&self, id: HookId) -> bool {
        self.commands.get(id.index()).is_some_and(|c| !c.is_empty())
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
            Msg::Network(Some(state)) => {
                let (link, vpn) = match state {
                    NetState::Absent => return out,
                    NetState::Offline => (None, false),
                    NetState::Online { link, vpns, .. } => (Some(*link), *vpns > 0),
                };
                if self.wants(HookId::Network) && self.network.replace(link).is_some_and(|old| old != link) {
                    out.push(Event::Network(link));
                }
                if self.wants(HookId::Vpn) && self.vpn.replace(vpn).is_some_and(|old| old != vpn) {
                    out.push(Event::Vpn(vpn));
                }
            }
            Msg::Bluetooth(Some(state)) if self.wants(HookId::Bluetooth) => {
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
                    if is_on && !was_on {
                        out.push(Event::Bluetooth(BtChange::On));
                    }
                    // A multiset difference: two devices named alike count twice.
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
                    if !is_on && was_on {
                        out.push(Event::Bluetooth(BtChange::Off));
                    }
                }
            }
            Msg::Battery(Some(PowerState { laptop: Some(b), .. })) => {
                if self.wants(HookId::BatteryCharge)
                    && b.charge != Charge::Unknown
                    && self.charge.replace(b.charge).is_some_and(|old| old != b.charge)
                {
                    out.push(Event::BatteryCharge(*b));
                }
                if self.wants(HookId::BatteryLevel) && self.percent.replace(b.percent).is_some_and(|old| old != b.percent) {
                    out.push(Event::BatteryLevel(*b));
                }
            }
            Msg::Profile(Some(ProfileState { active: Some(p), .. }))
                if self.wants(HookId::PowerProfile) && self.profile.replace(*p).is_some_and(|old| old != *p) =>
            {
                out.push(Event::PowerProfile(*p));
            }
            _ => {}
        }
        out
    }

    /// The mode in effect: a seed, then an edge on each change.
    pub fn on_mode(&mut self, mode: Mode) -> Option<Event> {
        if !self.wants(HookId::ThemeMode) {
            return None;
        }
        self.mode.replace(mode).filter(|old| *old != mode).map(|_| Event::ThemeMode(mode))
    }

    /// The indicators against the last ones (all off at start), microphone, camera, screen.
    pub fn on_capture(&mut self, c: &Capture) -> Vec<Event> {
        if !self.wants(HookId::Privacy) {
            return Vec::new();
        }
        let now = [c.mic.active(), c.camera.active(), c.screen.active()];
        let kinds = [CaptureKind::Microphone, CaptureKind::Camera, CaptureKind::Screen];
        let mut out = Vec::new();
        for ((kind, was), is) in kinds.into_iter().zip(self.capture).zip(now) {
            if was != is {
                out.push(Event::Privacy(kind, is));
            }
        }
        self.capture = now;
        out
    }

    /// Whether `e` runs now: its hook set, past the quiet window (`Started` apart), a token left.
    pub fn fire(&mut self, e: Event, now: Instant) -> Verdict {
        let id = e.id();
        if !self.wants(id) || (id != HookId::Started && now < self.quiet_until) {
            return Verdict::Dropped;
        }
        let Some(bucket) = self.buckets.get_mut(id.index()) else { return Verdict::Dropped };
        if bucket.tokens < BURST {
            let whole = (now.saturating_duration_since(bucket.at).as_nanos() / REFILL.as_nanos()).min(u128::from(BURST)) as u32;
            bucket.tokens = (bucket.tokens + whole).min(BURST);
            bucket.at += REFILL * whole;
        }
        if bucket.tokens == BURST {
            bucket.at = now;
        }
        if bucket.tokens == 0 {
            return if std::mem::replace(&mut bucket.throttled, true) { Verdict::Dropped } else { Verdict::Throttled(id) };
        }
        bucket.tokens -= 1;
        bucket.throttled = false;
        let argv = self.commands.get(id.index()).cloned().unwrap_or_default();
        Verdict::Run(Fire { id, argv, env: e.env() })
    }
}

#[cfg(test)]
mod tests;
