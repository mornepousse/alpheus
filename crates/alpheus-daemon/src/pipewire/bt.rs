//! The bluez5 devices and their profiles, for the headset row (quick Bluetooth spec § 4).

use super::*;

/// The most bluez5 devices kept, and the most profiles kept of each (quick Bluetooth spec § 4).
pub const MAX_BT_DEVICES: usize = 8;
pub const MAX_PROFILES: usize = 32;
/// A bluez5 device as the headset row reads it: what a person reads, its profiles, the active one.
#[derive(Debug, Default)]
pub(super) struct BtDevice {
    pub(super) label: Option<String>,
    pub(super) profiles: Vec<BtProfile>,
    pub(super) active: Option<u32>,
}

/// One profile: its index (what `wpctl set-profile` takes), the mode its name says (`a2dp-sink*`:
/// Hi-Fi, `headset-head-unit*`: Mic; anything else, `off` and `bap-sink` among them, neither), its
/// priority and whether it is available.
#[derive(Debug)]
pub(super) struct BtProfile {
    pub(super) index: u32,
    pub(super) mode: Option<HeadsetMode>,
    pub(super) priority: i64,
    pub(super) available: bool,
}

impl BtProfile {
    pub(super) fn new(p: &ProfileParam) -> Option<BtProfile> {
        let name = p.name.as_deref()?;
        let mode = if name.starts_with("a2dp-sink") {
            Some(HeadsetMode::HiFi)
        } else if name.starts_with("headset-head-unit") {
            Some(HeadsetMode::Mic)
        } else {
            None
        };
        Some(BtProfile { index: p.index?, mode, priority: p.priority.unwrap_or(i64::MIN), available: p.available.as_deref() != Some("no") })
    }
}

/// The highest-priority available profile of `mode`, the lowest index on a tie.
pub(super) fn pick(profiles: &[BtProfile], mode: HeadsetMode) -> Option<u32> {
    profiles
        .iter()
        .filter(|p| p.mode == Some(mode) && p.available)
        .min_by_key(|p| (std::cmp::Reverse(p.priority), p.index))
        .map(|p| p.index)
}

impl Model {
    /// A device object: kept when bluez5 (or known already and re-dumped without its API), its
    /// description and profiles as far as this dump carries them; another API drops it.
    pub(super) fn device(&mut self, id: u32, info: Info) {
        let Info { props, params } = info;
        match props.device_api.as_deref() {
            Some("bluez5") => {}
            Some(_) => {
                self.bt.remove(&id);
                return;
            }
            None if self.bt.contains_key(&id) => {}
            None => return,
        }
        if !self.bt.contains_key(&id) && self.bt.len() >= MAX_BT_DEVICES {
            return;
        }
        let d = self.bt.entry(id).or_default();
        if let Some(label) = props.device_description.as_deref().map(drawable).filter(|l| !l.is_empty()) {
            d.label = Some(label);
        }
        if !params.enum_profile.is_empty() {
            d.profiles = params.enum_profile.iter().filter_map(BtProfile::new).take(MAX_PROFILES).collect();
        }
        if let Some(active) = params.profile.iter().find_map(|p| p.index) {
            d.active = Some(active);
        }
    }

    /// The default sink's bluez5 device when it offers both kinds of profile and is on one of them
    /// (quick Bluetooth spec § 4): the highest-priority available profile of each kind, by name
    /// prefix (`bap-sink` is neither).
    pub fn headset(&self) -> Option<Headset> {
        let named = self.default_sink.as_ref().filter(|n| !n.gone)?;
        let node = self.resolve(Kind::Sink, &named.name)?;
        let device = node.device?;
        let d = self.bt.get(&device)?;
        let (hifi, mic) = (pick(&d.profiles, HeadsetMode::HiFi)?, pick(&d.profiles, HeadsetMode::Mic)?);
        let active = d.profiles.iter().find(|p| Some(p.index) == d.active)?.mode?;
        Some(Headset { device, label: d.label.clone().unwrap_or_else(|| node.label.clone()), active, hifi, mic })
    }

    /// bluez5 devices kept (tests).
    #[cfg(test)]
    pub(super) fn bt_devices(&self) -> usize {
        self.bt.len()
    }

    /// Profiles kept of one bluez5 device (tests).
    #[cfg(test)]
    pub(super) fn profiles(&self, device: u32) -> usize {
        self.bt.get(&device).map_or(0, |d| d.profiles.len())
    }
}
