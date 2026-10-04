//! UPower: the laptop's battery (its "display device", the aggregate UPower
//! shows in panels) and the peripherals with their own battery.

use super::{Bus, UPOWER, optional};
use crate::sources::dbus::props::Props;
use crate::system::{Battery, Charge, Peripheral, PeripheralKind, PowerState};
use std::io;

pub const PATH: &str = "/org/freedesktop/UPower";
pub const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
pub const DEVICE: &str = "org.freedesktop.UPower.Device";

// UpDeviceKind.
const LINE_POWER: i64 = 1;
const BATTERY: i64 = 2;
const MOUSE: i64 = 5;
const KEYBOARD: i64 = 6;
const GAMING_INPUT: i64 = 12;
const HEADSET: i64 = 17;
const HEADPHONES: i64 = 19;

/// UpDeviceState.
fn charge(state: Option<i64>) -> Charge {
    match state {
        Some(1) => Charge::Charging,
        Some(2 | 3 | 6) => Charge::Discharging,
        Some(4) => Charge::Full,
        Some(5) => Charge::PendingCharge,
        _ => Charge::Unknown,
    }
}

fn percent(p: &Props) -> u8 {
    p.float("Percentage").unwrap_or(0.0).round().clamp(0.0, 100.0) as u8
}

fn kind(t: i64) -> PeripheralKind {
    match t {
        MOUSE => PeripheralKind::Mouse,
        KEYBOARD => PeripheralKind::Keyboard,
        HEADSET | HEADPHONES => PeripheralKind::Headset,
        GAMING_INPUT => PeripheralKind::Gamepad,
        _ => PeripheralKind::Other,
    }
}

pub fn read(bus: &mut dyn Bus) -> io::Result<PowerState> {
    // Every call below goes to UPower's unique name: never an activation.
    let Some(up) = bus.owner(UPOWER)? else { return Ok(PowerState::default()) };
    let d = bus.get_all(&up, DISPLAY_DEVICE, DEVICE)?;
    let laptop = (d.bool("IsPresent") == Some(true) && d.int("Type") == Some(BATTERY))
        .then(|| Battery { percent: percent(&d), charge: charge(d.int("State")) });
    let mut peripherals = Vec::new();
    for path in bus.enumerate_devices(&up)? {
        let Some(p) = optional(bus.get_all(&up, &path, DEVICE))? else { continue };
        let t = p.int("Type").unwrap_or(0);
        if t == LINE_POWER || t == BATTERY || p.bool("PowerSupply") == Some(true) || p.bool("IsPresent") == Some(false) {
            continue;
        }
        // A Bluetooth device: its battery is the Bluetooth module's.
        if p.str("NativePath").is_some_and(|n| n.starts_with("/org/bluez/")) {
            continue;
        }
        peripherals.push(Peripheral { kind: kind(t), name: p.str("Model").unwrap_or("").to_string(), percent: percent(&p) });
    }
    peripherals.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(PowerState { laptop, peripherals })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::dbus::fake::FakeBus;
    use crate::sources::dbus::props::Prop;

    const MOUSE: &str = "/org/freedesktop/UPower/devices/battery_hidpp_battery_1";

    #[test]
    fn the_workstation_battery_charging_and_its_mouse() {
        let mut bus = FakeBus::workstation();
        assert_eq!(
            read(&mut bus).unwrap(),
            PowerState {
                laptop: Some(Battery { percent: 92, charge: Charge::Charging }),
                peripherals: vec![Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: 100 }],
            }
        );
    }

    #[test]
    fn charge_states_and_out_of_range_levels() {
        let mut bus = FakeBus::workstation();
        for (state, charge) in [
            (1, Charge::Charging),
            (2, Charge::Discharging),
            (3, Charge::Discharging),
            (4, Charge::Full),
            (5, Charge::PendingCharge),
            (6, Charge::Discharging),
            (0, Charge::Unknown),
        ] {
            bus.props_mut(DISPLAY_DEVICE, DEVICE).0.insert("State".into(), Prop::Int(state));
            assert_eq!(read(&mut bus).unwrap().laptop.map(|b| b.charge), Some(charge), "State {state}");
        }
        for (raw, percent) in [(100.4, 100), (-3.0, 0), (19.5, 20), (250.0, 100)] {
            bus.props_mut(DISPLAY_DEVICE, DEVICE).0.insert("Percentage".into(), Prop::Float(raw));
            assert_eq!(read(&mut bus).unwrap().laptop.map(|b| b.percent), Some(percent), "Percentage {raw}");
        }
    }

    #[test]
    fn a_desktop_has_no_laptop_battery_bluetooth_devices_are_left_out() {
        let mut bus = FakeBus::workstation();
        bus.props_mut(DISPLAY_DEVICE, DEVICE).0.insert("IsPresent".into(), Prop::Bool(false));
        assert_eq!(read(&mut bus).unwrap().laptop, None);
        bus.props_mut(MOUSE, DEVICE).0.insert("NativePath".into(), Prop::Str("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into()));
        assert!(read(&mut bus).unwrap().peripherals.is_empty(), "shown by the Bluetooth module");
    }

    #[test]
    fn upower_absent_only_asks_who_owns_the_name() {
        let mut bus = FakeBus::empty();
        assert_eq!(read(&mut bus).unwrap(), PowerState::default());
        assert_eq!(bus.calls, ["GetNameOwner org.freedesktop.UPower"]);
    }
}
