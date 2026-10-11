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
