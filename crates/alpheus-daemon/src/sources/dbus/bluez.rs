//! BlueZ: the adapter (powered or not) and the connected devices with their
//! battery, in one GetManagedObjects.

use super::{BLUEZ, Bus};
use crate::system::{BtDevice, BtState};
use std::io;

pub const ADAPTER: &str = "org.bluez.Adapter1";
pub const DEVICE: &str = "org.bluez.Device1";
pub const BATTERY: &str = "org.bluez.Battery1";

pub fn read(bus: &mut dyn Bus) -> io::Result<BtState> {
    // To bluetoothd's unique name: if the user stopped it meanwhile, the call
    // fails instead of starting it again.
    let Some(bz) = bus.owner(BLUEZ)? else { return Ok(BtState::Absent) };
    let objects = bus.managed_objects(&bz)?;
    // The first adapter in path order (hci0).
    let Some(adapter) = objects.values().find_map(|i| i.get(ADAPTER)) else { return Ok(BtState::Absent) };
    if adapter.bool("Powered") != Some(true) {
        return Ok(BtState::Off);
    }
    let mut devices: Vec<BtDevice> = objects
        .values()
        .filter_map(|i| {
            let d = i.get(DEVICE)?;
            if d.bool("Connected") != Some(true) {
                return None;
            }
            let name = d.str("Alias").or(d.str("Name")).or(d.str("Address")).unwrap_or("?").to_string();
            let battery = i.get(BATTERY).and_then(|b| b.int("Percentage")).map(|p| p.clamp(0, 100) as u8);
            Some(BtDevice { name, battery })
        })
        .collect();
    // The one that reports a battery first (the headset rather than a
    // keyboard: Q8), then by name.
    devices.sort_by(|a, b| b.battery.is_some().cmp(&a.battery.is_some()).then_with(|| a.name.cmp(&b.name)));
    Ok(BtState::On { devices })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::dbus::fake::FakeBus;
    use crate::sources::dbus::props::Prop;

    const HCI0: &str = "/org/bluez/hci0";
    const HEADSET: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02";
    const KEYBOARD: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_04";

    #[test]
    fn the_workstation_on_with_nothing_connected() {
        assert_eq!(read(&mut FakeBus::workstation()).unwrap(), BtState::On { devices: vec![] });
    }

    #[test]
    fn connected_devices_sorted_with_their_battery() {
        let mut bus = FakeBus::workstation();
        bus.connect_bluetooth();
        assert_eq!(
            read(&mut bus).unwrap(),
            BtState::On {
                devices: vec![
                    BtDevice { name: "CasqueBT".into(), battery: Some(72) },
                    BtDevice { name: "ClavierBT".into(), battery: None }
                ]
            }
        );
        // The device that reports a battery comes first, whatever its name (Q8).
        bus.bluez.get_mut(KEYBOARD).unwrap().get_mut(DEVICE).unwrap().0.insert("Alias".into(), Prop::Str("Aa clavier".into()));
        let BtState::On { devices } = read(&mut bus).unwrap() else { panic!("on") };
        assert_eq!(devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["CasqueBT", "Aa clavier"]);
    }

    #[test]
    fn name_falls_back_to_name_then_address() {
        let mut bus = FakeBus::workstation();
        bus.connect_bluetooth();
        let d = bus.bluez.get_mut(HEADSET).unwrap().get_mut(DEVICE).unwrap();
        // Alias still "CasqueBT", Name set too: Alias wins (added, not in the brief: the
        // brief's version never has both present at once, so a swapped priority stays green).
        d.0.insert("Name".into(), Prop::Str("Name only".into()));
        let BtState::On { devices } = read(&mut bus).unwrap() else { panic!("on") };
        assert_eq!(devices[0].name, "CasqueBT", "Alias wins over Name when both are present");
        let d = bus.bluez.get_mut(HEADSET).unwrap().get_mut(DEVICE).unwrap();
        d.0.remove("Alias");
        let BtState::On { devices } = read(&mut bus).unwrap() else { panic!("on") };
        assert_eq!(devices[0].name, "Name only", "the headset, first: it reports a battery");
        bus.bluez.get_mut(HEADSET).unwrap().get_mut(DEVICE).unwrap().0.remove("Name");
        let BtState::On { devices } = read(&mut bus).unwrap() else { panic!("on") };
        assert_eq!(devices[0].name, "AA:BB:CC:DD:EE:02");
    }

    #[test]
    fn powered_off_no_adapter_or_no_bluez() {
        let mut bus = FakeBus::workstation();
        bus.bluez.get_mut(HCI0).unwrap().get_mut(ADAPTER).unwrap().0.insert("Powered".into(), Prop::Bool(false));
        assert_eq!(read(&mut bus).unwrap(), BtState::Off);
        bus.bluez.remove(HCI0);
        assert_eq!(read(&mut bus).unwrap(), BtState::Absent, "rfkill-blocked or no controller");
        let mut bus = FakeBus::empty();
        assert_eq!(read(&mut bus).unwrap(), BtState::Absent);
        assert_eq!(bus.calls, ["GetNameOwner org.bluez"]);
    }
}
