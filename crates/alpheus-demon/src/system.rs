//! What the system sources say, normalized for the bar: sound, network,
//! Bluetooth, power, resources. Pure data: the sources fill it, `state`
//! keeps it, `segments` draws it.

pub use alpheus_commun::backends::Volume;
use std::collections::VecDeque;

/// The default output and input (PipeWire), as the bar and the OSD read them (OSD spec § 2).
/// `sink: None`: no output at all (the volume module hidden).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SoundState {
    pub sink: Option<Volume>,
    /// The default source (the microphone); None: none, or its volume not known yet.
    pub source: Option<Volume>,
    /// The default sink's and source's node: what tells a default switched from a level moved
    /// (`osd::Watch`). A default the metadata names before its node is in the dump is that node
    /// already, labelled with its node name, its volume unknown; one whose node went is None.
    pub sink_name: Option<DeviceName>,
    pub source_name: Option<DeviceName>,
    /// Every sink, by label then id, at most `pipewire::MAX_SINKS`: only while the quick panel is
    /// open (`Scope::Panel`); None otherwise, and never kept by the bar.
    pub sinks: Option<Vec<Sink>>,
    /// What the capture graph says is held (privacy spec § 2): `Some` exactly when the `privacy`
    /// module is on, None otherwise.
    pub capture: Option<crate::privacy::Capture>,
    /// The default sink's Bluetooth headset, when it offers both a Hi-Fi and a call profile:
    /// only while the quick panel is open (`Scope::Panel`); None otherwise, and never kept by the
    /// bar.
    pub headset: Option<Headset>,
}

// The headset (quick Bluetooth spec § 4).

/// A Bluetooth headset's two modes: Hi-Fi playback (A2DP) or call mode with a microphone (HSP/HFP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadsetMode {
    HiFi,
    Mic,
}

/// The default sink's bluez5 device and what `wpctl set-profile` takes to switch it: its device id,
/// what a person reads (`pipewire::LABEL_MAX` bytes at most), the active mode, and the profile index
/// of each mode (the highest-priority available one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headset {
    pub device: u32,
    pub label: String,
    pub active: HeadsetMode,
    pub hifi: u32,
    pub mic: u32,
}

/// A default device: its `node.name` (its identity) and what a person reads (`node.description`,
/// else `node.nick`, else the node name; at most `pipewire::LABEL_MAX` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceName {
    pub node: String,
    pub label: String,
}

/// An audio output the quick panel lists (quick spec § 2): its PipeWire node id (what
/// `wpctl set-default` takes), what a person reads (`pipewire::LABEL_MAX` bytes at most), and
/// whether it is the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sink {
    pub id: u32,
    pub label: String,
    pub default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Wifi,
    Ethernet,
    Vpn,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetState {
    /// NetworkManager is not on the bus: hidden.
    Absent,
    Offline,
    /// The primary connection's kind (the bar shows icons only, Q1);
    /// `vpns`: how many other VPNs are active (Q2).
    Online {
        link: Link,
        vpns: usize,
        limited: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    pub name: String,
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtState {
    /// No bluetoothd, or no adapter: hidden.
    Absent,
    /// Adapter powered off: hidden (spec § 2).
    Off,
    /// Connected devices: those reporting a battery first, then by name (Q8).
    On { devices: Vec<BtDevice> },
}

/// What the quick panel's Bluetooth reading covers (quick settings delivery 3): asked by the loop
/// through `sources::dbus::bt::BtTap`, read by the D-Bus thread. `Row`: the adapter (powered,
/// blocked); `Detail`: the Bluetooth detail is open, the paired devices and the ones seen nearby too.
pub type BtScope = PanelScope;

/// What a device is, from BlueZ's `Icon` (never its address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtKind {
    Headset,
    Keyboard,
    Mouse,
    Speaker,
    Phone,
    Other,
}

/// A device the panel lists. `path` is BlueZ's object path (it embeds the address: never logged);
/// `label` is the alias, cleaned and capped, `?` when nothing is left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtEntry {
    pub path: String,
    pub label: String,
    pub kind: BtKind,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtAdapter {
    pub path: String,
    pub powered: bool,
    /// `PowerState` is `off-blocked`: rfkill holds it off.
    pub blocked: bool,
}

/// The panel's Bluetooth view: the first adapter (None: no BlueZ, or none), the paired devices in
/// path order, the ones nearby in the order the reader listed them (detail only).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BtView {
    pub adapter: Option<BtAdapter>,
    pub paired: Vec<BtEntry>,
    pub nearby: Vec<BtEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    Full,
    /// Plugged in, not charging (charge threshold reached).
    PendingCharge,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charge: Charge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeripheralKind {
    Mouse,
    Keyboard,
    Headset,
    Gamepad,
    Other,
}

/// A device with its own battery known to UPower (a mouse on its receiver…).
/// Some peripherals (Logitech's HID++ receivers among them) only report a
/// coarse `BatteryLevel` — their `Percentage` is not to be trusted ("should
/// be ignored", UPower's own words) — so the charge is one or the other,
/// never both (Mae, 2026-10-05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peripheral {
    pub kind: PeripheralKind,
    pub name: String,
    /// A real, numeric reading: shown always, red below `LOW` (Q7).
    /// `None`: only a coarse level is known — hidden unless `low`.
    pub percent: Option<u8>,
    /// The coarse level is Low or Critical: shown as the bare icon in the
    /// error tint, no number. Meaningless when `percent` is `Some`.
    pub low: bool,
}

/// `laptop: None` and no peripheral: nothing to show (a desktop, or no UPower).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerState {
    pub laptop: Option<Battery>,
    pub peripherals: Vec<Peripheral>,
}

/// A power profile power-profiles-daemon offers (quick spec § 2); only these three are shown or set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    PowerSaver,
    Balanced,
    Performance,
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::PowerSaver, Profile::Balanced, Profile::Performance];

    /// The daemon's name for it: what `ActiveProfile` holds and `powerprofilesctl set` takes.
    pub fn name(self) -> &'static str {
        match self {
            Profile::PowerSaver => "power-saver",
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
        }
    }

    /// What the panel shows (spec § 2).
    pub fn label(self) -> &'static str {
        match self {
            Profile::PowerSaver => "saver",
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
        }
    }

    pub fn parse(s: &str) -> Option<Profile> {
        Profile::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// What power-profiles-daemon says. The default (no active profile, none offered): no daemon on
/// the system bus, the row hidden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileState {
    pub active: Option<Profile>,
    /// In the daemon's order, each once.
    pub offered: Vec<Profile>,
}

/// What a reading for the quick panel covers: asked by the loop through a `sources::dbus::tap::Tap`,
/// read by the D-Bus thread. Ordered by what is read: `Closed` < `Row` < `Detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum PanelScope {
    /// The panel is closed: nothing read, nothing followed, nothing kept.
    #[default]
    Closed,
    /// The panel is open: what the list's row shows.
    Row,
    /// The list's detail is open: everything it lists too.
    Detail,
}

/// What the quick panel's Wi-Fi reading covers (quick Wi-Fi spec § 5), through
/// `sources::dbus::wifi::WifiTap`. `Row`: the radio, the Wi-Fi device and the access point it is on;
/// `Detail`: the detail is open or a connection is under way, every access point and the saved
/// profiles too, and their signals followed.
pub type WifiScope = PanelScope;

/// An access point's security, from NetworkManager's NM80211ApFlags and NM80211ApSecurityFlags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// No key: open, or OWE (opportunistic encryption, no password).
    Open,
    /// WPA or WPA2 personal, WPA3 SAE: a password.
    Psk,
    /// 802.1X (enterprise): left to `More…`.
    Enterprise,
    /// WEP, or anything else secured: left to `More…`.
    Other,
}

const AP_PRIVACY: u32 = 0x1;
const KEY_MGMT_PSK: u32 = 0x100;
const KEY_MGMT_802_1X: u32 = 0x200;
const KEY_MGMT_SAE: u32 = 0x400;
const KEY_MGMT_OWE: u32 = 0x800;
const KEY_MGMT_OWE_TM: u32 = 0x1000;
const KEY_MGMT_EAP_SUITE_B_192: u32 = 0x2000;

impl Security {
    /// `flags`: the access point's Flags; `wpa`, `rsn`: its WpaFlags and RsnFlags. 802.1X first: a
    /// network offering it beside a password is still one the panel cannot join with a password only.
    pub fn from_flags(flags: u32, wpa: u32, rsn: u32) -> Security {
        let keys = wpa | rsn;
        if keys & (KEY_MGMT_802_1X | KEY_MGMT_EAP_SUITE_B_192) != 0 {
            Security::Enterprise
        } else if keys & (KEY_MGMT_PSK | KEY_MGMT_SAE) != 0 {
            Security::Psk
        } else if keys & (KEY_MGMT_OWE | KEY_MGMT_OWE_TM) != 0 {
            Security::Open
        } else if flags & AP_PRIVACY != 0 {
            Security::Other
        } else {
            Security::Open
        }
    }

    /// The detail's lock mark.
    pub fn secured(self) -> bool {
        self != Security::Open
    }
}

/// A visible access point as the panel uses it (its hardware address is never kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPoint {
    /// 1 to 32 bytes, as broadcast: not always UTF-8.
    pub ssid: Vec<u8>,
    /// Percent.
    pub strength: u8,
    pub security: Security,
}

/// A saved Wi-Fi profile: its uuid and SSID only, never its secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedWifi {
    pub uuid: String,
    pub ssid: Vec<u8>,
}

/// The first Wi-Fi device NetworkManager lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiDevice {
    /// NMDeviceState: 100 activated, 40 to 90 activating, 120 failed, 30 disconnected…
    pub state: u32,
    /// The access point it is on (ActiveAccessPoint); None: "/".
    pub active: Option<AccessPoint>,
    /// Its active connection's uuid (`connection down uuid …`).
    pub active_uuid: Option<String>,
    /// Some only with the detail open (`WifiScope::Detail`): at most 64 read, hidden ones left out.
    pub access_points: Option<Vec<AccessPoint>>,
    /// Some only with the detail open: the saved Wi-Fi profiles, at most 64 connections read.
    pub saved: Option<Vec<SavedWifi>>,
}

/// The quick panel's Wi-Fi (quick Wi-Fi spec § 5), only while the panel is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WifiState {
    /// NetworkManager's WirelessEnabled.
    pub enabled: bool,
    /// None: no Wi-Fi device, no NetworkManager, or its read failed (the row hides).
    pub device: Option<WifiDevice>,
    /// Read before an outage of another domain (BlueZ, UPower, the power profile) and resent
    /// unchanged for its backoff (F4): shown as is, never enough to delete a profile.
    pub stale: bool,
}

/// One reading of /proc and /sys. `cpu: None` on the first one (it takes two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sample {
    /// Percent, since the previous reading.
    pub cpu: Option<u8>,
    /// Memory in use like htop (caches left out, shared memory kept; see
    /// `sources::resources::parse_meminfo`), in tenths of GiB: 61 = 6.1G (Q5).
    pub memory: Option<u16>,
    /// °C.
    pub temperature: Option<i16>,
}

/// Points of CPU history kept (spec § 3: graph histories capped at 60).
pub const HISTORY: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resources {
    pub history: VecDeque<u8>,
    pub last: Option<Sample>,
}

impl Resources {
    /// Adds a reading; true if the display may have changed.
    pub fn push(&mut self, s: Sample) -> bool {
        if let Some(c) = s.cpu {
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(c);
        }
        let changed = s.cpu.is_some() || self.last != Some(s);
        self.last = Some(s);
        changed
    }
}
