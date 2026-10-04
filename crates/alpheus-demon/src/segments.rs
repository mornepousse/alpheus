//! Segments of the system modules (spec § 2): volume, network, Bluetooth
//! (battery and resources in task 7). Each returns None when it has nothing
//! to show — waiting for its first reading, or nothing to report; a source
//! that is down shows its icon then "–". The whole segment is one click zone.

use crate::config::ModuleId;
use crate::grid::{Style, Target, truncate};
use crate::layout::{Piece, Segment, piece};
use crate::state::Feed;
use crate::system::{BtState, Link, NetState, SoundState};

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
        vec![piece(format!("{VOLUME_OFF} "), Style::MUTED), piece(gauge, Style::MUTED), piece(format!(" {}%", v.percent), Style::MUTED)]
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
            piece(format!(" {}%", v.percent), Style::NORMAL),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalTime;
    use crate::compositor::example;
    use crate::config::BarConfig;
    use crate::grid::Grid;
    use crate::layout::lay_out;
    use crate::state::{Bar, Msg};
    use crate::system::{BtDevice, Volume};

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
        assert_eq!(draw(&volume(&sound(84, false)).unwrap()), ("\u{f057e} ▁▃▅▆▇▇ 84%".into(), "..aaaaaa....".into()));
        assert_eq!(draw(&volume(&sound(50, false)).unwrap()), ("\u{f0580} ▁▃▅▆▇▇ 50%".into(), "..aaammm....".into()));
        assert_eq!(draw(&volume(&sound(0, false)).unwrap()), ("\u{f057f} ▁▃▅▆▇▇ 0%".into(), "..mmmmmm...".into()));
        assert_eq!(
            draw(&volume(&sound(84, true)).unwrap()),
            ("\u{f0581} ▁▃▅▆▇▇ 84%".into(), "mmmmmmmmmmmm".into()),
            "muted: crossed out, dimmed"
        );
        assert_eq!(draw(&volume(&sound(120, false)).unwrap()).0, "\u{f057e} ▁▃▅▆▇▇ 120%", "above 100 %: full gauge, real value");
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
        assert!(t.contains(" │ \u{f057e} ▁▃▅▆▇▇ 84% │ \u{f0200} \u{f0582} 2 │ \u{f00b1} CasqueBT 72% │ Sun 4 Oct 14:05 "), "{t}");
        for (icon, module, width) in
            [(VOLUME_HIGH, ModuleId::Volume, 12u16), (ETHERNET, ModuleId::Network, 5), (BLUETOOTH_CONNECTED, ModuleId::Bluetooth, 14)]
        {
            let start = g.cells().iter().position(|c| c.ch == icon).unwrap() as u16;
            assert_eq!(g.target_at(start), Some(&Target::Module(module)), "first cell of {module:?}");
            assert_eq!(g.target_at(start + width - 1), Some(&Target::Module(module)), "last cell of {module:?}");
            assert_eq!(g.target_at(start + width), None, "separator after {module:?}");
            assert_eq!(g.target_at(start - 1), None, "separator before {module:?}");
        }
        insta::assert_snapshot!("sound_network_bluetooth_100", snapshot(&g));
    }
}
