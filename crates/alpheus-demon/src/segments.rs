//! Segments of the system modules (spec § 2): volume, network, Bluetooth,
//! battery, resources. Each returns None when it has nothing to show —
//! waiting for its first reading, or nothing to report; a source that is
//! down shows its icon then "–". The whole segment is one click zone.

use crate::config::{ModuleId, ResourcesConfig};
use crate::grid::{Style, Target, truncate};
use crate::layout::{Piece, Segment, piece};
use crate::state::Feed;
use crate::system::{Battery, BtState, Charge, Link, NetState, PeripheralKind, PowerState, Resources, SoundState};
use std::collections::VecDeque;

// Material Design glyphs of the Nerd Font, checked by name in the post table
// of JetBrainsMonoNerdFontMono-Regular.ttf (3.4.0).
pub const VOLUME_HIGH: char = '\u{f057e}'; // md-volume_high
pub const VOLUME_MEDIUM: char = '\u{f0580}'; // md-volume_medium
pub const VOLUME_LOW: char = '\u{f057f}'; // md-volume_low
pub const VOLUME_OFF: char = '\u{f0581}'; // md-volume_off (crossed out)
pub const WIFI: char = '\u{f05a9}'; // md-wifi
pub const ETHERNET: char = '\u{f0200}'; // md-ethernet
/// Down: a neutral link icon, never Wi-Fi-specific (a wired machine has no
/// radio to show while the source hasn't said which link it has).
pub const NETWORK_DOWN: char = '\u{f0317}'; // md-lan
pub const VPN: char = '\u{f0582}'; // md-vpn
pub const NETWORK_OTHER: char = '\u{f059f}'; // md-web
pub const OFFLINE: char = '\u{f0c9b}'; // md-network_off
pub const BLUETOOTH: char = '\u{f00af}'; // md-bluetooth
pub const BLUETOOTH_CONNECTED: char = '\u{f00b1}'; // md-bluetooth_connect

/// The volume's mini-gauge (spec § 2): one cell per sixth.
pub const GAUGE: [char; 6] = ['▁', '▃', '▅', '▆', '▇', '▇'];
/// Below this, a battery is low (error tint).
pub const LOW: u8 = 20;
/// Longest Bluetooth device name, in cells (then "…").
pub const DEVICE_MAX: u16 = 16;

fn module_segment(module: ModuleId, pieces: Vec<Piece>) -> Segment {
    Segment { module, pieces, elastic: false, target: Some(Target::Module(module)) }
}

/// A source that is down: its icon then "–", dimmed (spec § 3).
fn down(module: ModuleId, icon: char) -> Segment {
    module_segment(module, vec![piece(format!("{icon} –"), Style::MUTED)])
}

/// Zero-width characters a hostile or placeholder name could pad itself
/// with: not `char::is_whitespace` nor `char::is_control`, so `trim_name`
/// would otherwise leave them in place at the edges.
fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200d}' | '\u{feff}' | '\u{2060}')
}

/// Leading/trailing whitespace, controls and zero-width characters trimmed
/// before truncating: a name made only of these shows nothing, not a blank
/// name piece or a stray space.
fn trim_name(name: &str) -> &str {
    name.trim_matches(|c: char| c.is_whitespace() || c.is_control() || is_zero_width(c))
}

/// Gauge cells lit for `percent` (0 to 6, full beyond 100 %).
pub fn gauge_cells(percent: u8) -> usize {
    (usize::from(percent) * GAUGE.len()).div_ceil(100).min(GAUGE.len())
}

pub(crate) fn volume(feed: &Feed<SoundState>) -> Option<Segment> {
    let v = match feed {
        Feed::Waiting | Feed::Ready(SoundState { sink: None }) => return None,
        Feed::Down => return Some(down(ModuleId::Volume, VOLUME_HIGH)),
        Feed::Ready(SoundState { sink: Some(v) }) => *v,
    };
    let gauge: String = GAUGE.iter().collect();
    let pieces = if v.muted {
        vec![piece(format!("{VOLUME_OFF} "), Style::MUTED), piece(gauge, Style::MUTED), piece(format!(" {:>3}%", v.percent), Style::MUTED)]
    } else {
        let icon = if v.percent < 34 {
            VOLUME_LOW
        } else if v.percent < 67 {
            VOLUME_MEDIUM
        } else {
            VOLUME_HIGH
        };
        let lit = gauge_cells(v.percent);
        vec![
            piece(format!("{icon} "), Style::NORMAL),
            piece(GAUGE[..lit].iter().collect::<String>(), Style::ACCENT),
            piece(GAUGE[lit..].iter().collect::<String>(), Style::MUTED),
            piece(format!(" {:>3}%", v.percent), Style::NORMAL),
        ]
    };
    Some(module_segment(ModuleId::Volume, pieces))
}

pub(crate) fn network(feed: &Feed<NetState>) -> Option<Segment> {
    let pieces = match feed {
        Feed::Waiting | Feed::Ready(NetState::Absent) => return None,
        Feed::Down => return Some(down(ModuleId::Network, NETWORK_DOWN)),
        Feed::Ready(NetState::Offline) => vec![piece(OFFLINE.to_string(), Style::MUTED)],
        // Icons only (Q1): the primary link's, then the VPN count.
        Feed::Ready(NetState::Online { link, vpns, limited }) => {
            let icon = match link {
                Link::Wifi => WIFI,
                Link::Ethernet => ETHERNET,
                Link::Vpn => VPN,
                Link::Other => NETWORK_OTHER,
            };
            let mut v =
                if *limited { vec![piece(format!("{icon} !"), Style::ERROR)] } else { vec![piece(icon.to_string(), Style::NORMAL)] };
            match *vpns {
                0 => {}
                1 => v.push(piece(format!(" {VPN}"), Style::ACCENT)),
                n => v.push(piece(format!(" {VPN} {n}"), Style::ACCENT)),
            }
            v
        }
    };
    Some(module_segment(ModuleId::Network, pieces))
}

pub(crate) fn bluetooth(feed: &Feed<BtState>) -> Option<Segment> {
    let pieces = match feed {
        Feed::Waiting | Feed::Ready(BtState::Absent | BtState::Off) => return None,
        Feed::Down => return Some(down(ModuleId::Bluetooth, BLUETOOTH)),
        Feed::Ready(BtState::On { devices }) => match devices.first() {
            None => vec![piece(BLUETOOTH.to_string(), Style::MUTED)],
            Some(d) => {
                let name = trim_name(&d.name);
                let mut v = if name.is_empty() {
                    vec![piece(BLUETOOTH_CONNECTED.to_string(), Style::ACCENT)]
                } else {
                    vec![piece(format!("{BLUETOOTH_CONNECTED} "), Style::ACCENT), piece(truncate(name, DEVICE_MAX), Style::NORMAL)]
                };
                if let Some(b) = d.battery {
                    v.push(piece(format!(" {b}%"), if b < LOW { Style::ERROR } else { Style::MUTED }));
                }
                if devices.len() > 1 {
                    v.push(piece(format!(" +{}", devices.len() - 1), Style::MUTED));
                }
                v
            }
        },
    };
    Some(module_segment(ModuleId::Bluetooth, pieces))
}

pub const BATTERY_FULL: char = '\u{f0079}'; // md-battery
/// By tens (index = (percent + 5) / 10): outline, 10 … 90, full.
const BATTERY: [char; 11] = [
    '\u{f008e}', // md-battery_outline
    '\u{f007a}', // md-battery_10
    '\u{f007b}', // md-battery_20
    '\u{f007c}', // md-battery_30
    '\u{f007d}', // md-battery_40
    '\u{f007e}', // md-battery_50
    '\u{f007f}', // md-battery_60
    '\u{f0080}', // md-battery_70
    '\u{f0081}', // md-battery_80
    '\u{f0082}', // md-battery_90
    BATTERY_FULL,
];
const BATTERY_CHARGING: [char; 11] = [
    '\u{f089c}', // md-battery_charging_10 (no "charging_0")
    '\u{f089c}', // md-battery_charging_10
    '\u{f0086}', // md-battery_charging_20
    '\u{f0087}', // md-battery_charging_30
    '\u{f0088}', // md-battery_charging_40
    '\u{f089d}', // md-battery_charging_50
    '\u{f0089}', // md-battery_charging_60
    '\u{f089e}', // md-battery_charging_70
    '\u{f008a}', // md-battery_charging_80
    '\u{f008b}', // md-battery_charging_90
    '\u{f0085}', // md-battery_charging_100
];
/// A peripheral of a kind we have no specific icon for: a generic device,
/// distinct from the laptop battery's full icon (checked in the font's post
/// table like the others: advance 600, same as the other Material glyphs
/// here — one cell wide).
const DEVICE: char = '\u{f0fb0}'; // md-devices
pub const MEMORY: char = '\u{f035b}'; // md-memory
pub const THERMOMETER: char = '\u{f050f}'; // md-thermometer
/// CPU graph, one block per point (no braille in the bar's font, Q4).
pub const GRAPH: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// From this temperature (°C), the error tint (waybar's critical threshold).
pub const HOT: i16 = 85;

pub fn battery_icon(b: Battery) -> char {
    let i = (usize::from(b.percent.min(100)) + 5) / 10;
    if b.charge == Charge::Charging { BATTERY_CHARGING[i] } else { BATTERY[i] }
}

fn peripheral_icon(k: PeripheralKind) -> char {
    match k {
        PeripheralKind::Mouse => '\u{f037d}',    // md-mouse
        PeripheralKind::Keyboard => '\u{f030c}', // md-keyboard
        PeripheralKind::Headset => '\u{f02ce}',  // md-headset
        PeripheralKind::Gamepad => '\u{f0296}',  // md-gamepad
        PeripheralKind::Other => DEVICE,
    }
}

pub(crate) fn battery(feed: &Feed<PowerState>) -> Option<Segment> {
    let state = match feed {
        Feed::Waiting => return None,
        Feed::Down => return Some(down(ModuleId::Battery, BATTERY_FULL)),
        Feed::Ready(s) => s,
    };
    let mut pieces = Vec::new();
    if let Some(b) = state.laptop {
        let style = if b.charge == Charge::Charging {
            Style::ACCENT
        } else if b.percent < LOW {
            Style::ERROR
        } else {
            Style::NORMAL
        };
        pieces.push(piece(format!("{} {}%", battery_icon(b), b.percent), style));
    }
    // Every peripheral, always (Q7); red when low.
    for p in &state.peripherals {
        let space = if pieces.is_empty() { "" } else { " " };
        let style = if p.percent < LOW { Style::ERROR } else { Style::NORMAL };
        pieces.push(piece(format!("{space}{} {}%", peripheral_icon(p.kind), p.percent), style));
    }
    if pieces.is_empty() {
        return None;
    }
    Some(module_segment(ModuleId::Battery, pieces))
}

/// The last `width` points of `history`, right-aligned in `width` cells.
pub fn graph(history: &VecDeque<u8>, width: u16) -> String {
    let w = usize::from(width);
    let skip = history.len().saturating_sub(w);
    let pad = w.saturating_sub(history.len());
    std::iter::repeat_n(' ', pad).chain(history.iter().skip(skip).map(|p| GRAPH[(usize::from(*p) * 8 / 101).min(7)])).collect()
}

pub(crate) fn resources(r: &Resources, c: &ResourcesConfig) -> Option<Segment> {
    let s = r.last?;
    let mut pieces = Vec::new();
    if c.graph > 0 {
        pieces.push(piece(format!("{} ", graph(&r.history, c.graph)), Style::ACCENT));
    }
    pieces.push(piece(
        match s.cpu {
            Some(p) => format!("{p:>3}%"),
            None => "  …%".to_string(),
        },
        Style::NORMAL,
    ));
    if let Some(m) = s.memory {
        // Tenths of GiB, one decimal; the whole part padded to 2 so the
        // total stays 4 wide (the decimal part is always ".D", Q5).
        pieces.push(piece(format!(" {MEMORY} {:>2}.{}G", m / 10, m % 10), Style::NORMAL));
    }
    if let Some(t) = s.temperature {
        pieces.push(piece(format!(" {THERMOMETER} {t:>2}°"), if t >= HOT { Style::ERROR } else { Style::NORMAL }));
    }
    Some(module_segment(ModuleId::Resources, pieces))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalTime;
    use crate::compositor::example;
    use crate::config::{BarConfig, ResourcesConfig};
    use crate::grid::Grid;
    use crate::layout::lay_out;
    use crate::state::{Bar, Msg};
    use crate::system::{Battery, BtDevice, Charge, Peripheral, PeripheralKind, PowerState, Resources, Sample, Volume};
    use std::collections::VecDeque;

    const SUN: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };

    /// A segment alone, laid out: its text (margins removed) and one style code per cell.
    fn draw(s: &Segment) -> (String, String) {
        let w = s.width();
        let g = lay_out(std::slice::from_ref(s), w + 2);
        (g.text()[1..].trim_end().to_string(), g.styles()[1..1 + w as usize].to_string())
    }

    fn snapshot(g: &Grid) -> String {
        format!("{}\n{}", g.text(), g.styles())
    }

    fn sound(percent: u8, muted: bool) -> Feed<SoundState> {
        Feed::Ready(SoundState { sink: Some(Volume { percent, muted }) })
    }

    fn online(link: Link, vpns: usize, limited: bool) -> Feed<NetState> {
        Feed::Ready(NetState::Online { link, vpns, limited })
    }

    fn on(devices: &[(&str, Option<u8>)]) -> Feed<BtState> {
        Feed::Ready(BtState::On { devices: devices.iter().map(|(n, b)| BtDevice { name: n.to_string(), battery: *b }).collect() })
    }

    #[test]
    fn volume_icon_gauge_and_percent() {
        // Fixed width (A-m5, review): the number is always 3 wide, padded
        // with spaces, so the modules to its right never shift with the
        // volume level (5 %, 84 %, 100 % used to draw 4, 4, 5 cells wide).
        assert_eq!(draw(&volume(&sound(84, false)).unwrap()), ("\u{f057e} ▁▃▅▆▇▇  84%".into(), "..aaaaaa.....".into()));
        assert_eq!(draw(&volume(&sound(50, false)).unwrap()), ("\u{f0580} ▁▃▅▆▇▇  50%".into(), "..aaammm.....".into()));
        assert_eq!(draw(&volume(&sound(0, false)).unwrap()), ("\u{f057f} ▁▃▅▆▇▇   0%".into(), "..mmmmmm.....".into()));
        assert_eq!(
            draw(&volume(&sound(84, true)).unwrap()),
            ("\u{f0581} ▁▃▅▆▇▇  84%".into(), "mmmmmmmmmmmmm".into()),
            "muted: crossed out, dimmed"
        );
        assert_eq!(
            draw(&volume(&sound(120, false)).unwrap()).0,
            "\u{f057e} ▁▃▅▆▇▇ 120%",
            "above 100 %: full gauge, real value, same width as a 2-digit one"
        );
        assert_eq!([0, 1, 16, 17, 50, 83, 84, 100, 255].map(gauge_cells), [0, 1, 1, 2, 3, 5, 6, 6, 6]);
    }

    #[test]
    fn volume_icon_boundaries() {
        // Low/medium at 33/34, medium/high at 66/67 (see the `<` chain in
        // `volume`; each edge is covered by its own case below).
        assert_eq!(draw(&volume(&sound(33, false)).unwrap()).0.chars().next(), Some(VOLUME_LOW));
        assert_eq!(draw(&volume(&sound(34, false)).unwrap()).0.chars().next(), Some(VOLUME_MEDIUM));
        assert_eq!(draw(&volume(&sound(66, false)).unwrap()).0.chars().next(), Some(VOLUME_MEDIUM));
        assert_eq!(draw(&volume(&sound(67, false)).unwrap()).0.chars().next(), Some(VOLUME_HIGH));
    }

    #[test]
    fn volume_hidden_while_waiting_or_without_sink_dash_when_down() {
        assert!(volume(&Feed::Waiting).is_none());
        assert!(volume(&Feed::Ready(SoundState { sink: None })).is_none());
        assert_eq!(draw(&volume(&Feed::Down).unwrap()), ("\u{f057e} –".into(), "mmm".into()));
    }

    #[test]
    fn network_icons_only_and_vpn_count() {
        assert_eq!(draw(&network(&online(Link::Ethernet, 2, false)).unwrap()), ("\u{f0200} \u{f0582} 2".into(), ".aaaa".into()));
        assert_eq!(
            draw(&network(&online(Link::Wifi, 1, false)).unwrap()),
            ("\u{f05a9} \u{f0582}".into(), ".aa".into()),
            "one VPN: no count"
        );
        assert_eq!(draw(&network(&online(Link::Wifi, 0, false)).unwrap()), ("\u{f05a9}".into(), ".".into()), "no name (Q1)");
        assert_eq!(draw(&network(&online(Link::Vpn, 0, false)).unwrap()).0, "\u{f0582}");
        assert_eq!(draw(&network(&online(Link::Other, 0, false)).unwrap()).0, "\u{f059f}");
        assert_eq!(
            draw(&network(&online(Link::Wifi, 0, true)).unwrap()),
            ("\u{f05a9} !".into(), "eee".into()),
            "limited connectivity: error tint"
        );
        assert_eq!(draw(&network(&Feed::Ready(NetState::Offline)).unwrap()), ("\u{f0c9b}".into(), "m".into()));
        assert!(network(&Feed::Ready(NetState::Absent)).is_none());
        assert!(network(&Feed::Waiting).is_none());
        assert_eq!(
            draw(&network(&Feed::Down).unwrap()),
            ("\u{f0317} –".into(), "mmm".into()),
            "down: a neutral icon (not Wi-Fi-specific — the link isn't known while down)"
        );
    }

    #[test]
    fn bluetooth_device_battery_and_others() {
        assert_eq!(draw(&bluetooth(&on(&[("CasqueBT", Some(72))])).unwrap()), ("\u{f00b1} CasqueBT 72%".into(), "aa........mmmm".into()));
        assert_eq!(
            draw(&bluetooth(&on(&[("CasqueBT", Some(15)), ("ClavierBT", None)])).unwrap()),
            ("\u{f00b1} CasqueBT 15% +1".into(), "aa........eeeemmm".into()),
            "low battery in the error tint, the other devices counted"
        );
        assert_eq!(draw(&bluetooth(&on(&[("ClavierBT", None)])).unwrap()).0, "\u{f00b1} ClavierBT");
        assert_eq!(draw(&bluetooth(&on(&[])).unwrap()), ("\u{f00af}".into(), "m".into()), "on, nothing connected");
        for hidden in [Feed::Waiting, Feed::Ready(BtState::Off), Feed::Ready(BtState::Absent)] {
            assert!(bluetooth(&hidden).is_none(), "{hidden:?}");
        }
        assert_eq!(draw(&bluetooth(&Feed::Down).unwrap()), ("\u{f00af} –".into(), "mmm".into()));
    }

    #[test]
    fn bluetooth_battery_tint_boundary_at_19_and_20() {
        // Error tint strictly below LOW (20): 19 % is low, 20 % is not (see
        // the `b < LOW` comparison in `bluetooth`).
        assert_eq!(draw(&bluetooth(&on(&[("CasqueBT", Some(19))])).unwrap()).1, "aa........eeee");
        assert_eq!(draw(&bluetooth(&on(&[("CasqueBT", Some(20))])).unwrap()).1, "aa........mmmm");
    }

    #[test]
    fn hostile_names_truncated_without_controls() {
        // The only name the bar shows: a Bluetooth device's (the network shows icons only, Q1).
        let name = format!("a\tb\n\u{1b}[31m🦀 {}", "x".repeat(40));
        let b = bluetooth(&Feed::Ready(BtState::On { devices: vec![BtDevice { name, battery: None }] })).unwrap();
        let (t, _) = draw(&b);
        assert!(!t.contains(['\t', '\n', '\u{1b}']), "{t:?}");
        assert!(t.ends_with('…'), "{t:?}");
        assert_eq!(b.width(), 2 + DEVICE_MAX, "icon, space, then the name cut at {DEVICE_MAX} cells");
    }

    #[test]
    fn device_name_trimmed_before_truncation() {
        // Surrounding whitespace, controls and zero-width characters are
        // trimmed before truncating, so they never pad the shown name.
        let padded = bluetooth(&on(&[("  \t CasqueBT \n ", None)])).unwrap();
        assert_eq!(draw(&padded).0, "\u{f00b1} CasqueBT");

        // Nothing left after the trim: no name piece at all (icon alone),
        // not a blank.
        let blank = bluetooth(&on(&[("\u{200b}\u{feff}\t\u{2060}", None)])).unwrap();
        assert_eq!(draw(&blank).0, "\u{f00b1}");
        assert_eq!(blank.width(), 1, "icon alone, no leading space either");
    }

    fn three_modules_bar() -> Bar {
        let mut b = Bar::new(BarConfig::default());
        b.config.modules =
            vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Volume, ModuleId::Network, ModuleId::Bluetooth, ModuleId::Clock];
        b.apply(Msg::Compositor(Some(example())));
        b.apply(Msg::Clock(SUN));
        b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }) })));
        b.apply(Msg::Network(Some(NetState::Online { link: Link::Ethernet, vpns: 2, limited: false })));
        b.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(72) }] })));
        b
    }

    #[test]
    fn each_module_segment_is_one_click_zone() {
        let g = three_modules_bar().grid("HDMI-A-1", 100);
        let t = g.text();
        assert!(t.contains(" │ \u{f057e} ▁▃▅▆▇▇  84% │ \u{f0200} \u{f0582} 2 │ \u{f00b1} CasqueBT 72% │ Sun 4 Oct 14:05 "), "{t}");
        for (icon, module, width) in
            [(VOLUME_HIGH, ModuleId::Volume, 13u16), (ETHERNET, ModuleId::Network, 5), (BLUETOOTH_CONNECTED, ModuleId::Bluetooth, 14)]
        {
            let start = g.cells().iter().position(|c| c.ch == icon).unwrap() as u16;
            assert_eq!(g.target_at(start), Some(&Target::Module(module)), "first cell of {module:?}");
            assert_eq!(g.target_at(start + width - 1), Some(&Target::Module(module)), "last cell of {module:?}");
            assert_eq!(g.target_at(start + width), None, "separator after {module:?}");
            assert_eq!(g.target_at(start - 1), None, "separator before {module:?}");
        }
        insta::assert_snapshot!("sound_network_bluetooth_100", snapshot(&g));
    }

    #[test]
    fn battery_icon_follows_level_and_charge() {
        let b = |percent, charge| Battery { percent, charge };
        assert_eq!(battery_icon(b(93, Charge::Charging)), '\u{f008b}'); // md-battery_charging_90
        assert_eq!(battery_icon(b(93, Charge::Discharging)), '\u{f0082}'); // md-battery_90
        assert_eq!(battery_icon(b(100, Charge::Full)), '\u{f0079}'); // md-battery
        assert_eq!(battery_icon(b(55, Charge::Discharging)), '\u{f007f}'); // md-battery_60
        assert_eq!(battery_icon(b(15, Charge::Discharging)), '\u{f007b}'); // md-battery_20
        assert_eq!(battery_icon(b(3, Charge::Discharging)), '\u{f008e}'); // md-battery_outline
        assert_eq!(battery_icon(b(4, Charge::Charging)), '\u{f089c}'); // md-battery_charging_10
        assert_eq!(battery_icon(b(250, Charge::Unknown)), '\u{f0079}', "out of range: no panic");
    }

    #[test]
    fn battery_colours_charging_normal_low() {
        let laptop = |percent, charge| Feed::Ready(PowerState { laptop: Some(Battery { percent, charge }), peripherals: vec![] });
        assert_eq!(draw(&battery(&laptop(93, Charge::Charging)).unwrap()), ("\u{f008b} 93%".into(), "aaaaa".into()));
        assert_eq!(draw(&battery(&laptop(55, Charge::Discharging)).unwrap()), ("\u{f007f} 55%".into(), ".....".into()));
        assert_eq!(draw(&battery(&laptop(15, Charge::Discharging)).unwrap()), ("\u{f007b} 15%".into(), "eeeee".into()));
        assert_eq!(draw(&battery(&laptop(15, Charge::Charging)).unwrap()).1, "aaaaa", "charging wins over low");
        assert_eq!(draw(&battery(&laptop(100, Charge::Full)).unwrap()), ("\u{f0079} 100%".into(), "......".into()));
    }

    #[test]
    fn battery_tint_boundary_at_19_and_20() {
        // Error tint strictly below LOW (20): 19 % is low, 20 % is not (Mae,
        // 2026-10-04); see the `b.percent < LOW` comparison in `battery`.
        let laptop =
            |percent| Feed::Ready(PowerState { laptop: Some(Battery { percent, charge: Charge::Discharging }), peripherals: vec![] });
        assert_eq!(draw(&battery(&laptop(19)).unwrap()).1, "eeeee", "19%: error");
        assert_eq!(draw(&battery(&laptop(20)).unwrap()).1, ".....", "20%: normal");
    }

    #[test]
    fn peripheral_tint_boundary_at_19_and_20() {
        // Same threshold as the laptop battery (Q7), on each peripheral
        // independently: the first (19 %) errs, the second (20 %) doesn't.
        let mouse = |percent| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent };
        let s = Feed::Ready(PowerState { laptop: None, peripherals: vec![mouse(19), mouse(20)] });
        assert_eq!(draw(&battery(&s).unwrap()).1, "eeeee......");
    }

    #[test]
    fn peripheral_icon_other_is_a_generic_device_not_the_laptop_battery() {
        // Distinct from BATTERY_FULL (md-battery): a bare "Other" peripheral
        // should not look like a laptop at 100 %.
        assert_eq!(peripheral_icon(PeripheralKind::Other), DEVICE);
        assert_ne!(DEVICE, BATTERY_FULL);
    }

    #[test]
    fn peripherals_after_the_battery_hidden_without_anything_to_show() {
        let mouse = |percent| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent };
        let s = |laptop, peripherals| Feed::Ready(PowerState { laptop, peripherals });
        let charging = Some(Battery { percent: 93, charge: Charge::Charging });
        assert_eq!(
            draw(&battery(&s(charging, vec![mouse(15)])).unwrap()),
            ("\u{f008b} 93% \u{f037d} 15%".into(), "aaaaaeeeeee".into()),
            "low: error tint"
        );
        assert_eq!(
            draw(&battery(&s(charging, vec![mouse(100)])).unwrap()),
            ("\u{f008b} 93% \u{f037d} 100%".into(), "aaaaa.......".into()),
            "always shown (Q7), normal above 20 %"
        );
        assert_eq!(draw(&battery(&s(None, vec![mouse(80)])).unwrap()).0, "\u{f037d} 80%", "desktop: the peripheral alone");
        assert!(battery(&s(None, vec![])).is_none(), "desktop, no peripheral: hidden");
        assert!(battery(&Feed::Waiting).is_none());
        assert_eq!(draw(&battery(&Feed::Down).unwrap()), ("\u{f0079} –".into(), "mmm".into()));
    }

    fn resources_with(history: &[u8], last: Sample) -> Resources {
        Resources { history: history.iter().copied().collect(), last: Some(last) }
    }

    #[test]
    fn resources_graph_cpu_memory_temperature() {
        let c = ResourcesConfig { graph: 6, ..ResourcesConfig::default() };
        let s = Sample { cpu: Some(90), memory: Some(61), temperature: Some(67) };
        assert_eq!(
            draw(&resources(&resources_with(&[12, 38, 55, 90], s), &c).unwrap()),
            ("  ▁▄▅█  90% \u{f035b}  6.1G \u{f050f} 67°".into(), "aaaaaaa..................".into())
        );
        let first = Sample { cpu: None, memory: None, temperature: Some(91) };
        assert_eq!(
            draw(&resources(&resources_with(&[], first), &ResourcesConfig { graph: 0, ..c.clone() }).unwrap()),
            ("  …% \u{f050f} 91°".into(), "....eeeeee".into()),
            "first reading: no CPU yet; hot: error tint; graph = 0: no graph"
        );
        assert!(resources(&Resources::default(), &c).is_none(), "nothing read yet");
        let full: VecDeque<u8> = (0..60).map(|i| if i < 54 { 0 } else { 100 }).collect();
        assert_eq!(graph(&full, 6), "██████", "only the last points");
        assert_eq!(graph(&VecDeque::from([0, 100]), 4), "  ▁█", "right-aligned, fixed width");
        let big = Sample { cpu: Some(9), memory: Some(105), temperature: None };
        assert_eq!(
            draw(&resources(&resources_with(&[9], big), &ResourcesConfig { graph: 0, ..c }).unwrap()).0,
            "  9% \u{f035b} 10.5G",
            "same width above 10 GiB"
        );
    }

    #[test]
    fn resources_temperature_tint_boundary_at_84_and_85() {
        // The error tint from HOT (85 °C, waybar's own critical threshold,
        // Mae 2026-10-04): 84° is still normal, 85° already errs.
        let c = ResourcesConfig { graph: 0, ..ResourcesConfig::default() };
        let normal = Sample { cpu: Some(50), memory: None, temperature: Some(84) };
        let hot = Sample { cpu: Some(50), memory: None, temperature: Some(85) };
        assert_eq!(
            draw(&resources(&resources_with(&[], normal), &c).unwrap()),
            (" 50% \u{f050f} 84°".into(), "..........".into()),
            "84°: normal"
        );
        assert_eq!(
            draw(&resources(&resources_with(&[], hot), &c).unwrap()),
            (" 50% \u{f050f} 85°".into(), "....eeeeee".into()),
            "85°: error"
        );
    }

    fn full_bar() -> Bar {
        let mut b = Bar::new(BarConfig::default());
        b.apply(Msg::Compositor(Some(example())));
        b.apply(Msg::Clock(SUN));
        b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }) })));
        b.apply(Msg::Network(Some(NetState::Online { link: Link::Ethernet, vpns: 2, limited: false })));
        b.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(72) }] })));
        b.apply(Msg::Battery(Some(PowerState { laptop: Some(Battery { percent: 93, charge: Charge::Charging }), peripherals: vec![] })));
        for cpu in [12, 38, 55, 90] {
            b.apply(Msg::Resources(Sample { cpu: Some(cpu), memory: Some(61), temperature: Some(67) }));
        }
        b
    }

    #[test]
    fn narrow_screens_drop_modules_in_order() {
        // Paths spelled out: `Battery`, `Resources`, `Volume` are also system types here.
        let (window, volume, network, bluetooth, battery, resources) =
            (ModuleId::Window, ModuleId::Volume, ModuleId::Network, ModuleId::Bluetooth, ModuleId::Battery, ModuleId::Resources);
        let b = full_bar();
        let marks = [
            (window, "Web page"),
            (volume, "\u{f057e}"),
            (network, "\u{f0200}"),
            (bluetooth, "\u{f00b1}"),
            (battery, "\u{f008b}"),
            (resources, "\u{f035b}"),
        ];
        let shown = |w: u16| {
            let t = b.grid("HDMI-A-1", w).text();
            marks.iter().filter(|(_, s)| t.contains(s)).map(|(m, _)| *m).collect::<Vec<_>>()
        };
        assert_eq!(shown(160), [window, volume, network, bluetooth, battery, resources]);
        assert_eq!(shown(120), [window, volume, network, bluetooth, battery], "resources leave first");
        // Each width pins one more step of REMOVAL's order; a swap inside it
        // (title/Bluetooth, Bluetooth/network or volume/battery) turns one
        // of these red (see the "prove it bites" mutations).
        assert_eq!(shown(90), [volume, network, bluetooth, battery], "then the title");
        assert_eq!(shown(70), [volume, network, battery], "then Bluetooth");
        assert_eq!(shown(60), [volume, battery], "then network");
        assert_eq!(shown(50), [battery], "then volume");
        assert_eq!(shown(40), Vec::<ModuleId>::new(), "then battery");
        for w in [160u16, 120, 90, 70, 60, 50, 40] {
            let g = b.grid("HDMI-A-1", w);
            assert_eq!(g.width(), w);
            assert!(g.text().ends_with(" 14:05 ") && g.text().contains(" 4!····+ "), "workspaces and clock whole at {w}: {}", g.text());
        }
        insta::assert_snapshot!("full_160", snapshot(&b.grid("HDMI-A-1", 160)));
        insta::assert_snapshot!("full_120", snapshot(&b.grid("HDMI-A-1", 120)));
    }
}
