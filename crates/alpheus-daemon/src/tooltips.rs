//! The one-line text shown after a rest of the pointer on a module: what its glyphs mean, spelled
//! out.
//!
//! Pure: each function reads the `Bar` and returns the text, None when there is nothing to say (a
//! feed still waiting, a module the bar does not draw at that moment). A feed that is down reads
//! `<Module> unavailable`. Parts are joined by ` · `, percentages read `N %`, from the same figures
//! the bar draws. `modules.rs`'s `tooltip` column points here; the hover path builds the text when
//! the tooltip shows and never refreshes it.
//!
//! Never:
//! - tell more than the bar draws: no network name, no sink or source label, no peripheral's own
//!   name (its kind instead), no app id of a window (decision 7, extended; guarded by
//!   `tooltips_name_no_private_field`). A Bluetooth device's name and a window's title are drawn
//!   (cut), so they show whole;
//! - log, or panic: these run on the loop's thread for every tooltip shown.
//!
//! See: docs/features/bar.md (what each module's tooltip says), docs/decisions.md.

use crate::clock;
use crate::config::ResourceItem;
use crate::segments;
use crate::state::{Bar, Feed};
use crate::system::{BtState, Charge, Link, NetState, PeripheralKind, SoundState};

/// The parts of a tooltip joined as one line.
const JOIN: &str = " · ";

/// `Some("<module> unavailable")` for a feed that is down.
fn unavailable(module: &str) -> Option<String> {
    Some(format!("{module} unavailable"))
}

/// The full date: `Friday 27 November 2026`.
pub(crate) fn clock(bar: &Bar) -> Option<String> {
    let t = bar.clock.as_ref()?;
    Some(clock::format("%A %e %B %Y", t).trim().to_string())
}

/// The output's level, or that it is muted (with the level it comes back to).
pub(crate) fn volume(bar: &Bar) -> Option<String> {
    match &bar.sound {
        Feed::Waiting | Feed::Ready(SoundState { sink: None, .. }) => None,
        Feed::Down => unavailable("Volume"),
        Feed::Ready(SoundState { sink: Some(v), .. }) if v.muted => Some(format!("Volume muted ({} %)", v.percent)),
        Feed::Ready(SoundState { sink: Some(v), .. }) => Some(format!("Volume {} %", v.percent)),
    }
}

/// The primary link's kind, the other VPNs' count and a limited connectivity: what the icons say.
pub(crate) fn network(bar: &Bar) -> Option<String> {
    match &bar.network {
        Feed::Waiting | Feed::Ready(NetState::Absent) => None,
        Feed::Down => unavailable("Network"),
        Feed::Ready(NetState::Offline) => Some("Offline".to_string()),
        Feed::Ready(NetState::Online { link, vpns, limited }) => {
            let mut parts = vec![
                match link {
                    Link::Wifi => "Wi-Fi",
                    Link::Ethernet => "Ethernet",
                    Link::Vpn => "VPN",
                    Link::Other => "Connected",
                }
                .to_string(),
            ];
            match *vpns {
                0 => {}
                1 => parts.push("1 VPN".to_string()),
                n => parts.push(format!("{n} VPNs")),
            }
            if *limited {
                parts.push("limited connectivity".to_string());
            }
            Some(parts.join(JOIN))
        }
    }
}

/// What a peripheral is, never what it is called.
fn kind(k: PeripheralKind) -> &'static str {
    match k {
        PeripheralKind::Mouse => "Mouse",
        PeripheralKind::Keyboard => "Keyboard",
        PeripheralKind::Headset => "Headset",
        PeripheralKind::Gamepad => "Gamepad",
        PeripheralKind::Other => "Device",
    }
}

/// The laptop's charge and state, then each peripheral the bar draws, by kind, under the same
/// `[battery]` settings.
pub(crate) fn battery(bar: &Bar) -> Option<String> {
    let state = match &bar.battery {
        Feed::Waiting => return None,
        Feed::Down => return unavailable("Battery"),
        Feed::Ready(s) => s,
    };
    let config = &bar.config.battery;
    let mut parts = Vec::new();
    if let Some(b) = state.laptop {
        let charge = match b.charge {
            Charge::Charging => ", charging",
            Charge::Discharging => ", discharging",
            Charge::Full => ", full",
            Charge::PendingCharge => ", plugged in",
            Charge::Unknown => "",
        };
        parts.push(format!("Battery {} %{charge}", b.percent));
    }
    for p in &state.peripherals {
        match p.percent {
            Some(percent) if segments::shown(config.peripherals, percent < segments::LOW) => {
                parts.push(format!("{} {percent} %", kind(p.kind)));
            }
            None if segments::shown(config.coarse, p.low) => {
                parts.push(if p.low { format!("{} low", kind(p.kind)) } else { kind(p.kind).to_string() });
            }
            Some(_) | None => {}
        }
    }
    // Every setting hides what there is: the bar draws nothing either.
    (!parts.is_empty()).then(|| parts.join(JOIN))
}

/// The first device's whole name (the bar cuts it), its battery, and how many more are connected.
pub(crate) fn bluetooth(bar: &Bar) -> Option<String> {
    let devices = match &bar.bluetooth {
        Feed::Waiting | Feed::Ready(BtState::Absent | BtState::Off) => return None,
        Feed::Down => return unavailable("Bluetooth"),
        Feed::Ready(BtState::On { devices }) => devices,
    };
    let Some(first) = devices.first() else {
        return Some("Bluetooth on, nothing connected".to_string());
    };
    let name = segments::trim_name(&first.name);
    let mut parts = vec![if name.is_empty() { "Bluetooth connected".to_string() } else { name.to_string() }];
    if let Some(b) = first.battery {
        parts.push(format!("{b} %"));
    }
    if devices.len() > 1 {
        parts.push(format!("{} more", devices.len() - 1));
    }
    Some(parts.join(JOIN))
}

/// Each figure of `[resources] show`, in its order, spelled out; an item with no figure is left out,
/// as the bar leaves it out (the CPU alone reads `…` before its first figure).
pub(crate) fn resources(bar: &Bar) -> Option<String> {
    let s = bar.resources.last?;
    let mut parts = Vec::new();
    for &item in &bar.config.resources.show {
        let part = match item {
            ResourceItem::Cpu => Some(s.cpu.map_or_else(|| "CPU …".to_string(), |p| format!("CPU {p} %"))),
            ResourceItem::Memory => s.memory.map(|m| format!("Memory {}.{} GiB", m / 10, m % 10)),
            ResourceItem::Temperature => s.temperature.map(|t| format!("Temperature {t} °C")),
            ResourceItem::Net => s.net.map(|n| format!("Download {}/s, upload {}/s", segments::rate(n.down), segments::rate(n.up))),
            // The fullest listed path, as the bar draws it; never the path itself.
            ResourceItem::Disk => s.disks.iter().flatten().max().map(|d| format!("Disk {d} %")),
            // Usage then degrees when both are read, as the bar draws them (`gpu_metric`).
            ResourceItem::Gpu => match (s.gpu_busy, s.gpu) {
                (Some(b), Some(t)) => Some(format!("GPU {} %, {t} °C", b.percent)),
                (Some(b), None) => Some(format!("GPU {} %", b.percent)),
                (None, t) => t.map(|t| format!("GPU {t} °C")),
            },
        };
        parts.extend(part);
    }
    (!parts.is_empty()).then(|| parts.join(JOIN))
}

/// The focused window's whole title (the bar cuts it); never what program it is.
pub(crate) fn window(bar: &Bar) -> Option<String> {
    match &bar.compositor {
        Feed::Waiting => None,
        Feed::Down => unavailable("Window"),
        Feed::Ready(c) => c.window.as_ref().map(|w| w.title.as_str()).filter(|t| !t.is_empty()).map(str::to_string),
    }
}

/// How many units failed, and under which manager; never which.
pub(crate) fn failed(bar: &Bar) -> Option<String> {
    let total = crate::failed::total(&bar.failed).filter(|n| *n > 0)?;
    let noun = if total == 1 { "unit" } else { "units" };
    let by: Vec<String> = [("system", bar.failed.system), ("user", bar.failed.user)]
        .into_iter()
        .filter_map(|(manager, n)| n.filter(|n| *n > 0).map(|n| format!("{manager} {n}")))
        .collect();
    Some(format!("{total} failed {noun} ({})", by.join(", ")))
}

/// The unread count, exact (the bar caps it), and do-not-disturb.
pub(crate) fn notifications(bar: &Bar) -> Option<String> {
    let v = bar.notifications?;
    if !v.serving {
        return Some("Another notification server is running".to_string());
    }
    let mut parts = vec![if v.unread == 0 { "Nothing unread".to_string() } else { format!("{} unread", v.unread) }];
    if v.dnd {
        parts.push("do not disturb".to_string());
    }
    Some(parts.join(JOIN))
}

#[cfg(test)]
mod tests;
