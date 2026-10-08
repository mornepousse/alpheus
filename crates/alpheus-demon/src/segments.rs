//! Segments of the system modules (spec § 2): volume, network, Bluetooth,
//! battery, resources, the tray. Each returns None when it has nothing to
//! show — waiting for its first reading, or nothing to report; a source that
//! is down shows its icon then "–". The whole segment is one click zone,
//! except the tray's: one zone per icon.

use crate::config::{BatteryConfig, BatteryVisibility, ModuleId, Passive, ResourcesConfig, TrayConfig};
use crate::grid::{Hue, Style, Target, truncate};
use crate::layout::{Piece, Segment, piece};
use crate::media::MediaView;
use crate::state::Feed;
use crate::system::{Battery, BtState, Charge, Link, NetState, PeripheralKind, PowerState, Resources, SoundState};
use crate::tray::Status;
use crate::tray::icons::{Icon, TrayIcon};
use alpheus_commun::theme::settings::ModuleColors;
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
/// The launcher's button, left of the workspaces.
pub const LAUNCHER: char = '\u{f0349}'; // md-magnify
/// The media module (media spec § 1, § 2): the bar's mark, the popup's buttons.
pub const PLAY: char = '\u{f040a}'; // md-play
pub const PAUSE: char = '\u{f03e4}'; // md-pause
pub const SKIP_PREVIOUS: char = '\u{f04ae}'; // md-skip_previous
pub const SKIP_NEXT: char = '\u{f04ad}'; // md-skip_next

/// The volume's mini-gauge (spec § 2): one cell per sixth.
pub const GAUGE: [char; 6] = ['▁', '▃', '▅', '▆', '▇', '▇'];
/// Below this, a battery is low (warn tint).
pub const LOW: u8 = 20;
/// Below this a battery is critical (themes spec § 4: error), below LOW only low (warn).
pub const CRITICAL: u8 = 10;

/// A battery's state tint by its charge: error when critical, warn when low, None otherwise.
fn charge_state(percent: u8) -> Option<Style> {
    if percent < CRITICAL {
        Some(Style::ERROR)
    } else if percent < LOW {
        Some(Style::WARN)
    } else {
        None
    }
}

/// A battery's tint by its charge: its state when there is one, `normal` otherwise.
fn charge_style(percent: u8, normal: Style) -> Style {
    charge_state(percent).unwrap_or(normal)
}
/// Longest Bluetooth device name, in cells (then "…").
pub const DEVICE_MAX: u16 = 16;

/// A module's icon: its fixed hue (themes spec § 9) when `module_colors` is `icons` or `full`,
/// `base` otherwise. Only for a module in its normal state: warn, error and the dimmed states
/// never go through here.
pub(crate) fn icon_style(mc: ModuleColors, hue: Hue, base: Style) -> Style {
    if mc == ModuleColors::None { base } else { Style::hue(hue) }
}

/// A module's text: its hue only under `module_colors = "full"` or `"pills"`, `base` otherwise. The
/// grid of `pills` is the grid of `full`: the block styles' painter puts the hue behind the text and
/// picks the text's colour by contrast (shapes spec § 1); `flat` and `underline` show it as `full`.
pub(crate) fn text_style(mc: ModuleColors, hue: Hue, base: Style) -> Style {
    if matches!(mc, ModuleColors::Full | ModuleColors::Pills) { Style::hue(hue) } else { base }
}

fn module_segment(module: ModuleId, pieces: Vec<Piece>) -> Segment {
    Segment { module, pieces, elastic: false, target: Some(Target::Module(module)), withhold_hue: false }
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

/// The launcher's button: always shown, one click zone (spec § 2).
pub(crate) fn launcher() -> Segment {
    module_segment(ModuleId::Launcher, vec![piece(LAUNCHER.to_string(), Style::NORMAL)])
}

pub(crate) fn volume(feed: &Feed<SoundState>, mc: ModuleColors) -> Option<Segment> {
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
            piece(format!("{icon} "), icon_style(mc, Hue::Blue, Style::NORMAL)),
            piece(GAUGE[..lit].iter().collect::<String>(), Style::ACCENT),
            piece(GAUGE[lit..].iter().collect::<String>(), Style::MUTED),
            piece(format!(" {:>3}%", v.percent), text_style(mc, Hue::Blue, Style::NORMAL)),
        ]
    };
    Some(module_segment(ModuleId::Volume, pieces))
}

pub(crate) fn network(feed: &Feed<NetState>, mc: ModuleColors) -> Option<Segment> {
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
            let mut v = if *limited {
                vec![piece(format!("{icon} !"), Style::ERROR)]
            } else {
                vec![piece(icon.to_string(), icon_style(mc, Hue::Cyan, Style::NORMAL))]
            };
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

pub(crate) fn bluetooth(feed: &Feed<BtState>, mc: ModuleColors) -> Option<Segment> {
    let pieces = match feed {
        Feed::Waiting | Feed::Ready(BtState::Absent | BtState::Off) => return None,
        Feed::Down => return Some(down(ModuleId::Bluetooth, BLUETOOTH)),
        Feed::Ready(BtState::On { devices }) => match devices.first() {
            None => vec![piece(BLUETOOTH.to_string(), Style::MUTED)],
            Some(d) => {
                let name = trim_name(&d.name);
                let icon = icon_style(mc, Hue::Purple, Style::ACCENT);
                let mut v = if name.is_empty() {
                    vec![piece(BLUETOOTH_CONNECTED.to_string(), icon)]
                } else {
                    vec![
                        piece(format!("{BLUETOOTH_CONNECTED} "), icon),
                        piece(truncate(name, DEVICE_MAX), text_style(mc, Hue::Purple, Style::NORMAL)),
                    ]
                };
                if let Some(b) = d.battery {
                    v.push(piece(format!(" {b}%"), charge_style(b, Style::MUTED)));
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

/// Whether a peripheral is shown under `visibility`, given whether it
/// currently counts as low (spec `[battery]`, Mae 2026-10-05).
fn shown(visibility: BatteryVisibility, low: bool) -> bool {
    match visibility {
        BatteryVisibility::Always => true,
        BatteryVisibility::Low => low,
        BatteryVisibility::Never => false,
    }
}

pub(crate) fn battery(feed: &Feed<PowerState>, config: &BatteryConfig, mc: ModuleColors) -> Option<Segment> {
    let state = match feed {
        Feed::Waiting => return None,
        Feed::Down => return Some(down(ModuleId::Battery, BATTERY_FULL)),
        Feed::Ready(s) => s,
    };
    let mut pieces = Vec::new();
    if let Some(b) = state.laptop {
        // A charging battery never alarms: the accent without hues (the old look), green with them,
        // at any level (Mae, 2026-10-07). Warn and error apply only while discharging.
        let state = match b.charge {
            Charge::Charging if mc == ModuleColors::None => Some(Style::ACCENT),
            Charge::Charging => None,
            _ => charge_state(b.percent),
        };
        pieces.push(piece(format!("{} ", battery_icon(b)), state.unwrap_or_else(|| icon_style(mc, Hue::Green, Style::NORMAL))));
        pieces.push(piece(format!("{}%", b.percent), state.unwrap_or_else(|| text_style(mc, Hue::Green, Style::NORMAL))));
    }
    // Every peripheral with a real percentage follows `[battery] peripherals`
    // (default "always", warn when low, error when critical); one with only a coarse `BatteryLevel`
    // (Mae, 2026-10-05) follows `[battery] coarse` (default "low": hidden
    // except Low or Critical), shown as the bare icon, warn when low: UPower's Low and Critical are one bool here.
    for p in &state.peripherals {
        let space = if pieces.is_empty() { "" } else { " " };
        match p.percent {
            Some(percent) if shown(config.peripherals, percent < LOW) => {
                let state = charge_state(percent);
                pieces.push(piece(
                    format!("{space}{} ", peripheral_icon(p.kind)),
                    state.unwrap_or_else(|| icon_style(mc, Hue::Green, Style::NORMAL)),
                ));
                pieces.push(piece(format!("{percent}%"), state.unwrap_or_else(|| text_style(mc, Hue::Green, Style::NORMAL))));
            }
            Some(_) => {}
            None if shown(config.coarse, p.low) => {
                let style = if p.low { Style::WARN } else { icon_style(mc, Hue::Green, Style::NORMAL) };
                pieces.push(piece(format!("{space}{}", peripheral_icon(p.kind)), style));
            }
            None => {}
        }
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

pub(crate) fn resources(r: &Resources, c: &ResourcesConfig, mc: ModuleColors) -> Option<Segment> {
    let s = r.last?;
    let mut pieces = Vec::new();
    if c.graph > 0 {
        // The graph is the CPU's icon: its orange takes the accent's place.
        pieces.push(piece(format!("{} ", graph(&r.history, c.graph)), icon_style(mc, Hue::Orange, Style::ACCENT)));
    }
    pieces.push(piece(
        match s.cpu {
            Some(p) => format!("{p:>3}%"),
            None => "  …%".to_string(),
        },
        text_style(mc, Hue::Orange, Style::NORMAL),
    ));
    if let Some(m) = s.memory {
        // Tenths of GiB, one decimal; the whole part padded to 2 so the
        // total stays 4 wide (the decimal part is always ".D", Q5).
        pieces.push(piece(format!(" {MEMORY}"), icon_style(mc, Hue::Yellow, Style::NORMAL)));
        pieces.push(piece(format!(" {:>2}.{}G", m / 10, m % 10), text_style(mc, Hue::Yellow, Style::NORMAL)));
    }
    if let Some(t) = s.temperature {
        if t >= HOT {
            pieces.push(piece(format!(" {THERMOMETER} {t:>2}°"), Style::ERROR));
        } else {
            pieces.push(piece(format!(" {THERMOMETER}"), icon_style(mc, Hue::Red, Style::NORMAL)));
            pieces.push(piece(format!(" {t:>2}°"), text_style(mc, Hue::Red, Style::NORMAL)));
        }
    }
    Some(module_segment(ModuleId::Resources, pieces))
}

/// The media module (media spec § 1): `▶ Title` while playing, in the purple as `module_colors`
/// says; `⏸ Title` while paused, dimmed whatever it says (a state wins over the hue); None without
/// a player playing or paused. The whole segment is one click zone.
pub(crate) fn media(view: Option<&MediaView>, mc: ModuleColors) -> Option<Segment> {
    let v = view?;
    let (mark, icon, text) = if v.playing {
        (PLAY, icon_style(mc, Hue::Purple, Style::NORMAL), text_style(mc, Hue::Purple, Style::NORMAL))
    } else {
        (PAUSE, Style::MUTED, Style::MUTED)
    };
    let pieces = if v.title.is_empty() {
        vec![piece(mark.to_string(), icon)]
    } else {
        vec![piece(format!("{mark} "), icon), piece(v.title.clone(), text)]
    };
    let mut segment = module_segment(ModuleId::Media, pieces);
    segment.withhold_hue = !v.playing;
    Some(segment)
}

/// One tray icon (spec § 1 Icons): a glyph NORMAL, a letter BOLD_ACCENT (the
/// clock's style: "bold accent colour, no background"); NeedsAttention ERROR
/// whatever it is; Passive MUTED (`dim`), as Active (`show`) or not drawn
/// (`hide`). Its own zone: the gestures go to that item.
fn tray_piece(i: &TrayIcon, passive: Passive) -> Option<Piece> {
    let (text, style) = match &i.icon {
        Icon::Glyph(g) => (g.clone(), Style::NORMAL),
        Icon::Letter(c) => (c.to_string(), Style::BOLD_ACCENT),
    };
    let style = match (i.status, passive) {
        (Status::NeedsAttention, _) => Style::ERROR,
        (Status::Passive, Passive::Hide) => return None,
        (Status::Passive, Passive::Dim) => Style::MUTED,
        _ => style,
    };
    Some(Piece { text, style, target: Some(Target::TrayItem(i.key.clone())) })
}

/// None when nothing is drawn (no icon, or all passive and hidden). One
/// piece per icon, separated by one blank cell (question 9), each its own
/// zone (Target::TrayItem); no zone over the whole segment.
pub(crate) fn tray(icons: &[TrayIcon], config: &TrayConfig) -> Option<Segment> {
    let mut pieces = Vec::new();
    for p in icons.iter().filter_map(|i| tray_piece(i, config.passive)) {
        if !pieces.is_empty() {
            pieces.push(piece(" ", Style::NORMAL));
        }
        pieces.push(p);
    }
    if pieces.is_empty() {
        return None;
    }
    Some(Segment { module: ModuleId::Tray, pieces, elastic: false, target: None, withhold_hue: false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalTime;
    use crate::compositor::example;
    use crate::config::{BarConfig, BatteryConfig, BatteryVisibility, ResourcesConfig};
    use crate::grid::Grid;
    use crate::layout::lay_out;
    use crate::state::{Bar, Msg};
    use crate::system::{Battery, BtDevice, Charge, Peripheral, PeripheralKind, PowerState, Resources, Sample, Volume};
    use crate::tray::ItemKey;
    use crate::tray::icons::{DISCORD, STEAM, TELEGRAM};
    use std::collections::VecDeque;

    // The segments under `module_colors = "none"` (delivery 1's look), shadowing the real ones: the
    // tests of the modules' own shapes stay as they were; the hue tests call `super::…` with a setting.
    fn volume(f: &Feed<SoundState>) -> Option<Segment> {
        super::volume(f, ModuleColors::None)
    }
    fn network(f: &Feed<NetState>) -> Option<Segment> {
        super::network(f, ModuleColors::None)
    }
    fn bluetooth(f: &Feed<BtState>) -> Option<Segment> {
        super::bluetooth(f, ModuleColors::None)
    }
    fn battery(f: &Feed<PowerState>, c: &BatteryConfig) -> Option<Segment> {
        super::battery(f, c, ModuleColors::None)
    }
    fn resources(r: &Resources, c: &ResourcesConfig) -> Option<Segment> {
        super::resources(r, c, ModuleColors::None)
    }

    const SUN: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };

    /// A segment alone, laid out: its text (margins removed) and one style code per cell.
    fn draw(s: &Segment) -> (String, String) {
        let w = s.width();
        let g = lay_out(std::slice::from_ref(s), w + 2, None);
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
            ("\u{f00b1} CasqueBT 15% +1".into(), "aa........wwwwmmm".into()),
            "low battery in the warn tint, the other devices counted"
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
        // Warn tint strictly below LOW (20): 19 % is low, 20 % is not (see
        // the `b < LOW` comparison in `bluetooth`).
        assert_eq!(draw(&bluetooth(&on(&[("CasqueBT", Some(19))])).unwrap()).1, "aa........wwww");
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

    #[test]
    fn launcher_button_is_one_glyph_and_one_click_zone() {
        let s = launcher();
        assert_eq!(draw(&s), ("\u{f0349}".into(), ".".into()));
        assert_eq!(s.width(), 1, "one cell, like the other icons");
        assert_eq!(s.target, Some(Target::Module(ModuleId::Launcher)));
        assert_eq!(crate::grid::text_width(&LAUNCHER.to_string()), 1);
    }

    fn song(playing: bool, title: &str) -> MediaView {
        MediaView { playing, title: title.into() }
    }

    #[test]
    fn media_shows_its_mark_and_title() {
        let s = super::media(Some(&song(true, "Bohemian Rhapsody")), ModuleColors::None).unwrap();
        assert_eq!(draw(&s), ("\u{f040a} Bohemian Rhapsody".into(), ".".repeat(19)));
        assert_eq!((s.module, &s.target), (ModuleId::Media, &Some(Target::Module(ModuleId::Media))), "one click zone");
        let paused = super::media(Some(&song(false, "Bohemian Rhapsody")), ModuleColors::None).unwrap();
        assert_eq!(draw(&paused), ("\u{f03e4} Bohemian Rhapsody".into(), "m".repeat(19)), "paused: the pause mark, dimmed");
        let bare = super::media(Some(&song(true, "")), ModuleColors::None).unwrap();
        assert_eq!((draw(&bare).0.as_str(), bare.width()), ("\u{f040a}", 1), "no title: the mark alone, no trailing space");
    }

    #[test]
    fn media_is_hidden_without_a_player() {
        for mc in [ModuleColors::None, ICONS, FULL, ModuleColors::Pills] {
            assert!(super::media(None, mc).is_none(), "{mc:?}");
        }
        let mut b = Bar::new(BarConfig::default());
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Media, ModuleId::Clock];
        b.apply(Msg::Compositor(Some(example())));
        b.apply(Msg::Clock(SUN));
        let t = b.grid("HDMI-A-1", 60).text();
        assert_eq!(t.matches('│').count(), 1, "no \"│ │\" without a player: {t}");
        b.apply(Msg::Media(Some(song(true, "Song"))));
        let t = b.grid("HDMI-A-1", 60).text();
        assert_eq!(t.matches('│').count(), 2, "{t}");
        assert!(t.contains(" │ \u{f040a} Song "), "{t}");
    }

    #[test]
    fn media_takes_the_purple_by_module_colours_and_paused_stays_dim() {
        let at = |playing, mc| draw(&super::media(Some(&song(playing, "Song")), mc).unwrap()).1;
        assert_eq!(at(true, ModuleColors::None), "......");
        assert_eq!(at(true, ICONS), "pp....", "the purple mark (and its space)");
        assert_eq!(at(true, FULL), "pppppp", "and the title");
        assert_eq!(at(true, ModuleColors::Pills), "pppppp", "pills lays out as full");
        for mc in [ModuleColors::None, ICONS, FULL, ModuleColors::Pills] {
            assert_eq!(at(false, mc), "mmmmmm", "paused stays dim under {mc:?}");
            let flags = |playing| super::media(Some(&song(playing, "Song")), mc).unwrap().withhold_hue;
            assert_eq!((flags(true), flags(false)), (false, true), "only a paused block withholds its hue ({mc:?})");
        }
    }

    #[test]
    fn media_glyphs_are_one_cell_each() {
        assert_eq!([PLAY, PAUSE, SKIP_PREVIOUS, SKIP_NEXT], ['\u{f040a}', '\u{f03e4}', '\u{f04ae}', '\u{f04ad}']);
        for g in [PLAY, PAUSE, SKIP_PREVIOUS, SKIP_NEXT] {
            assert_eq!(crate::grid::text_width(&g.to_string()), 1, "{g:?}");
        }
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
        let d = &BatteryConfig::default();
        assert_eq!(draw(&battery(&laptop(93, Charge::Charging), d).unwrap()), ("\u{f008b} 93%".into(), "aaaaa".into()));
        assert_eq!(draw(&battery(&laptop(55, Charge::Discharging), d).unwrap()), ("\u{f007f} 55%".into(), ".....".into()));
        assert_eq!(draw(&battery(&laptop(15, Charge::Discharging), d).unwrap()), ("\u{f007b} 15%".into(), "wwwww".into()));
        assert_eq!(draw(&battery(&laptop(15, Charge::Charging), d).unwrap()).1, "aaaaa", "charging wins over low");
        assert_eq!(draw(&battery(&laptop(100, Charge::Full), d).unwrap()), ("\u{f0079} 100%".into(), "......".into()));
    }

    #[test]
    fn a_charging_battery_is_green_under_icons_and_full() {
        let laptop = |percent, charge| Feed::Ready(PowerState { laptop: Some(Battery { percent, charge }), peripherals: vec![] });
        let d = &BatteryConfig::default();
        let at = |mc, percent, charge| draw(&super::battery(&laptop(percent, charge), d, mc).unwrap()).1;
        assert_eq!(at(ModuleColors::None, 93, Charge::Charging), "aaaaa", "None: the accent, as before");
        assert_eq!(at(ModuleColors::Icons, 93, Charge::Charging), "gg...", "Icons: the green icon, the text normal");
        assert_eq!(at(ModuleColors::Full, 93, Charge::Charging), "ggggg", "Full: green icon and text");
        assert_eq!(at(ModuleColors::Icons, 93, Charge::Charging), at(ModuleColors::Icons, 93, Charge::Discharging));
        assert_eq!(at(ModuleColors::Full, 15, Charge::Charging), "ggggg", "charging is green at any level");
        assert_eq!(at(ModuleColors::Icons, 5, Charge::Charging), "gg..");
        assert_eq!(at(ModuleColors::Full, 15, Charge::Discharging), "wwwww", "discharging low still warns");
    }

    #[test]
    fn battery_tint_boundary_at_19_and_20() {
        // Warn tint strictly below LOW (20): 19 % is low, 20 % is not (Mae,
        // 2026-10-04); see the `b.percent < LOW` comparison in `battery`.
        let laptop =
            |percent| Feed::Ready(PowerState { laptop: Some(Battery { percent, charge: Charge::Discharging }), peripherals: vec![] });
        let d = &BatteryConfig::default();
        assert_eq!(draw(&battery(&laptop(19), d).unwrap()).1, "wwwww", "19%: warn");
        assert_eq!(draw(&battery(&laptop(20), d).unwrap()).1, ".....", "20%: normal");
    }

    #[test]
    fn peripheral_tint_boundary_at_19_and_20() {
        // Same threshold as the laptop battery (Q7), on each peripheral
        // independently: the first (19 %) errs, the second (20 %) doesn't.
        let mouse = |percent| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: Some(percent), low: false };
        let s = Feed::Ready(PowerState { laptop: None, peripherals: vec![mouse(19), mouse(20)] });
        assert_eq!(draw(&battery(&s, &BatteryConfig::default()).unwrap()).1, "wwwww......");
    }

    #[test]
    fn a_coarse_peripheral_is_hidden_unless_low_or_critical() {
        // Mae, 2026-10-05: BatteryLevel not None means Percentage is not to
        // be trusted (UPower's own words). Default `[battery] coarse = "low"`:
        // hidden, except Low/Critical (`low`), shown as the bare icon in the
        // warn tint.
        let coarse = |low| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: None, low };
        let s = |peripherals| Feed::Ready(PowerState { laptop: None, peripherals });
        let d = &BatteryConfig::default();
        assert!(battery(&s(vec![coarse(false)]), d).is_none(), "hidden: no percentage and not low");
        assert_eq!(draw(&battery(&s(vec![coarse(true)]), d).unwrap()), ("\u{f037d}".into(), "w".into()), "low: bare icon, warn tint");
        // Alongside a laptop battery and a real-percentage peripheral.
        let laptop = Some(Battery { percent: 93, charge: Charge::Charging });
        let real = Peripheral { kind: PeripheralKind::Keyboard, name: "Clavier".into(), percent: Some(80), low: false };
        assert_eq!(
            draw(&battery(&s2(laptop, vec![real, coarse(true)]), d).unwrap()),
            ("\u{f008b} 93% \u{f030c} 80% \u{f037d}".into(), "aaaaa......ww".into()),
            "a coarse Low peripheral keeps its place after a real one"
        );

        fn s2(laptop: Option<Battery>, peripherals: Vec<Peripheral>) -> Feed<PowerState> {
            Feed::Ready(PowerState { laptop, peripherals })
        }
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
        let mouse = |percent| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: Some(percent), low: false };
        let s = |laptop, peripherals| Feed::Ready(PowerState { laptop, peripherals });
        let charging = Some(Battery { percent: 93, charge: Charge::Charging });
        let d = &BatteryConfig::default();
        assert_eq!(
            draw(&battery(&s(charging, vec![mouse(15)]), d).unwrap()),
            ("\u{f008b} 93% \u{f037d} 15%".into(), "aaaaawwwwww".into()),
            "low: warn tint"
        );
        assert_eq!(
            draw(&battery(&s(charging, vec![mouse(100)]), d).unwrap()),
            ("\u{f008b} 93% \u{f037d} 100%".into(), "aaaaa.......".into()),
            "always shown (default [battery] peripherals = \"always\"), normal above 20 %"
        );
        assert_eq!(draw(&battery(&s(None, vec![mouse(80)]), d).unwrap()).0, "\u{f037d} 80%", "desktop: the peripheral alone");
        assert!(battery(&s(None, vec![]), d).is_none(), "desktop, no peripheral: hidden");
        assert!(battery(&Feed::Waiting, d).is_none());
        assert_eq!(draw(&battery(&Feed::Down, d).unwrap()), ("\u{f0079} –".into(), "mmm".into()));
    }

    #[test]
    fn peripherals_visibility_always_low_never() {
        // `[battery] peripherals` on a real-percentage peripheral (spec,
        // Mae 2026-10-05): "always" (default) shows it whatever the level,
        // "low" only below 20 %, "never" hides it outright.
        let mouse = |percent| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: Some(percent), low: false };
        let s = |percent| Feed::Ready(PowerState { laptop: None, peripherals: vec![mouse(percent)] });
        let c = |peripherals| BatteryConfig { peripherals, ..BatteryConfig::default() };

        let always = c(BatteryVisibility::Always);
        assert_eq!(draw(&battery(&s(80), &always).unwrap()).0, "\u{f037d} 80%", "always: shown above 20 %");
        assert_eq!(draw(&battery(&s(15), &always).unwrap()).0, "\u{f037d} 15%", "always: shown below 20 % too");

        let low = c(BatteryVisibility::Low);
        assert!(battery(&s(80), &low).is_none(), "low: hidden above 20 %");
        assert_eq!(draw(&battery(&s(15), &low).unwrap()).0, "\u{f037d} 15%", "low: shown below 20 %");

        let never = c(BatteryVisibility::Never);
        assert!(battery(&s(80), &never).is_none(), "never: hidden above 20 %");
        assert!(battery(&s(15), &never).is_none(), "never: hidden below 20 % too");
    }

    #[test]
    fn coarse_visibility_always_low_never() {
        // `[battery] coarse` on a peripheral with only a coarse `BatteryLevel`
        // (spec, Mae 2026-10-05): "always" shows the bare icon whatever the
        // level (warn tint only when low), "low" (default) only when Low or
        // Critical, "never" hides it outright.
        let device = |low| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: None, low };
        let s = |low| Feed::Ready(PowerState { laptop: None, peripherals: vec![device(low)] });
        let c = |coarse| BatteryConfig { coarse, ..BatteryConfig::default() };

        let always = c(BatteryVisibility::Always);
        assert_eq!(draw(&battery(&s(false), &always).unwrap()), ("\u{f037d}".into(), ".".into()), "always: shown, normal tint");
        assert_eq!(draw(&battery(&s(true), &always).unwrap()), ("\u{f037d}".into(), "w".into()), "always: shown, warn tint when low");

        let low = c(BatteryVisibility::Low);
        assert!(battery(&s(false), &low).is_none(), "low: hidden when not low");
        assert_eq!(draw(&battery(&s(true), &low).unwrap()).1, "w", "low: shown, warn tint when low");

        let never = c(BatteryVisibility::Never);
        assert!(battery(&s(false), &never).is_none(), "never: hidden when not low");
        assert!(battery(&s(true), &never).is_none(), "never: hidden when low too");
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

    // ---- module colours (themes spec § 9) ----

    const ICONS: ModuleColors = ModuleColors::Icons;
    const FULL: ModuleColors = ModuleColors::Full;

    fn discharging(percent: u8) -> Feed<PowerState> {
        Feed::Ready(PowerState { laptop: Some(Battery { percent, charge: Charge::Discharging }), peripherals: vec![] })
    }

    #[test]
    fn none_is_the_look_of_delivery_1_and_icons_tints_only_the_icons() {
        let v = sound(84, false);
        assert_eq!(draw(&super::volume(&v, ModuleColors::None).unwrap()).1, "..aaaaaa.....");
        assert_eq!(draw(&super::volume(&v, ICONS).unwrap()).1, "BBaaaaaa.....", "blue icon (and its space), gauge and number as before");
        assert_eq!(draw(&super::volume(&v, FULL).unwrap()).1, "BBaaaaaaBBBBB", "the number takes the blue too");
        let n = online(Link::Wifi, 1, false);
        assert_eq!(draw(&super::network(&n, ModuleColors::None).unwrap()).1, ".aa");
        assert_eq!(draw(&super::network(&n, ICONS).unwrap()).1, "caa", "cyan icon, the VPN keeps its accent");
        assert_eq!(draw(&super::network(&n, FULL).unwrap()).1, "caa", "no text beside the icons");
        let b = on(&[("CasqueBT", Some(72))]);
        assert_eq!(draw(&super::bluetooth(&b, ModuleColors::None).unwrap()).1, "aa........mmmm");
        assert_eq!(draw(&super::bluetooth(&b, ICONS).unwrap()).1, "pp........mmmm", "purple icon in the accent's place");
        assert_eq!(draw(&super::bluetooth(&b, FULL).unwrap()).1, "ppppppppppmmmm", "and the name; the battery stays dim");
    }

    #[test]
    fn battery_and_resources_take_their_hues() {
        let d = &BatteryConfig::default();
        let f = discharging(55);
        assert_eq!(draw(&super::battery(&f, d, ModuleColors::None).unwrap()).1, ".....");
        assert_eq!(draw(&super::battery(&f, d, ICONS).unwrap()).1, "gg...", "green icon");
        assert_eq!(draw(&super::battery(&f, d, FULL).unwrap()).1, "ggggg", "green icon and number");
        let c = ResourcesConfig::default();
        let s = Sample { cpu: Some(50), memory: Some(61), temperature: Some(67) };
        let r = resources_with(&[12, 90], s);
        let none = draw(&super::resources(&r, &c, ModuleColors::None).unwrap());
        let (text, icons) = draw(&super::resources(&r, &c, ICONS).unwrap());
        let (_, full) = draw(&super::resources(&r, &c, FULL).unwrap());
        assert_eq!(text, none.0, "the text never changes");
        assert_eq!(none.1, "aaaaaaaaaaaaaaaaaaaaa..................", "none: the graph accent, nothing else");
        // The graph (20 cells and its space) is the CPU's icon: orange in the accent's place.
        assert_eq!(icons, "ooooooooooooooooooooo....yy......rr....", "graph, memory and thermometer icons");
        assert_eq!(full, "oooooooooooooooooooooooooyyyyyyyyrrrrrr", "icons and texts, each in its hue");
    }

    #[test]
    fn states_win_over_the_hues() {
        let d = &BatteryConfig::default();
        for mc in [ICONS, FULL, ModuleColors::Pills] {
            assert_eq!(draw(&super::battery(&discharging(15), d, mc).unwrap()).1, "wwwww", "low battery stays warn under {mc:?}");
            assert_eq!(draw(&super::battery(&discharging(9), d, mc).unwrap()).1, "eeee", "critical stays error under {mc:?}");
            let charging = Feed::Ready(PowerState { laptop: Some(Battery { percent: 93, charge: Charge::Charging }), peripherals: vec![] });
            let want = match mc {
                ModuleColors::None => "aaaaa",
                ModuleColors::Icons => "gg...",
                ModuleColors::Full | ModuleColors::Pills => "ggggg",
            };
            assert_eq!(draw(&super::battery(&charging, d, mc).unwrap()).1, want, "charging: accent under None, green otherwise ({mc:?})");
            let hot = Sample { cpu: Some(50), memory: None, temperature: Some(85) };
            let c = ResourcesConfig { graph: 0, ..ResourcesConfig::default() };
            assert_eq!(
                draw(&super::resources(&resources_with(&[], hot), &c, mc).unwrap()).1[4..],
                *"eeeeee",
                "hot temperature stays error under {mc:?}"
            );
            assert_eq!(
                draw(&super::network(&online(Link::Wifi, 0, true), mc).unwrap()).1,
                "eee",
                "limited connectivity stays error under {mc:?}"
            );
            assert_eq!(draw(&super::volume(&sound(84, true), mc).unwrap()).1, "mmmmmmmmmmmmm", "muted stays dim under {mc:?}");
            assert_eq!(draw(&super::network(&Feed::Ready(NetState::Offline), mc).unwrap()).1, "m", "offline stays dim under {mc:?}");
            assert_eq!(draw(&super::bluetooth(&Feed::Down, mc).unwrap()).1, "mmm", "a source that is down stays dim under {mc:?}");
        }
    }

    /// The same bar under each setting: the representative snapshots. The battery discharges (a
    /// charging one is green too, or accent under None), so the green shows.
    fn hue_bar(mc: ModuleColors) -> Bar {
        let mut b = full_bar();
        b.apply(Msg::Battery(Some(PowerState { laptop: Some(Battery { percent: 55, charge: Charge::Discharging }), peripherals: vec![] })));
        b.module_colors = mc;
        b
    }

    /// `pills` lays out exactly as `full`: only the block styles' painter tells them apart.
    #[test]
    fn the_grid_of_pills_is_the_grid_of_full() {
        for w in [160u16, 120, 60] {
            let (pills, full) = (hue_bar(ModuleColors::Pills).grid("HDMI-A-1", w), hue_bar(FULL).grid("HDMI-A-1", w));
            assert_eq!(snapshot(&pills), snapshot(&full), "{w} columns");
        }
        let v = sound(84, false);
        assert_eq!(draw(&super::volume(&v, ModuleColors::Pills).unwrap()).1, "BBaaaaaaBBBBB", "the number in blue, as under full");
    }

    #[test]
    fn a_full_bar_under_each_module_colours_setting() {
        insta::assert_snapshot!("hues_none_160", snapshot(&hue_bar(ModuleColors::None).grid("HDMI-A-1", 160)));
        insta::assert_snapshot!("hues_icons_160", snapshot(&hue_bar(ICONS).grid("HDMI-A-1", 160)));
        insta::assert_snapshot!("hues_full_160", snapshot(&hue_bar(FULL).grid("HDMI-A-1", 160)));
    }

    #[test]
    fn narrow_screens_drop_modules_in_order() {
        // Paths spelled out: `Battery`, `Resources`, `Volume` are also system types here.
        let (window, volume, network, bluetooth, battery, resources) =
            (ModuleId::Window, ModuleId::Volume, ModuleId::Network, ModuleId::Bluetooth, ModuleId::Battery, ModuleId::Resources);
        let launcher = ModuleId::Launcher;
        let b = full_bar();
        let marks = [
            (launcher, "\u{f0349}"),
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
        // The launcher button and its separator (4 cells) leave last of
        // all: every other threshold sits 4 cells further than without it.
        const BUTTON: u16 = 4;
        assert_eq!(shown(160 + BUTTON), [launcher, window, volume, network, bluetooth, battery, resources]);
        assert_eq!(shown(120 + BUTTON), [launcher, window, volume, network, bluetooth, battery], "resources leave first");
        // Each width pins one more step of REMOVAL's order; a swap inside it
        // (title/Bluetooth, Bluetooth/network, volume/battery or
        // battery/launcher) turns one of these red (see the "prove it bites"
        // mutations).
        assert_eq!(shown(90 + BUTTON), [launcher, volume, network, bluetooth, battery], "then the title");
        assert_eq!(shown(70 + BUTTON), [launcher, volume, network, battery], "then Bluetooth");
        assert_eq!(shown(60 + BUTTON), [launcher, volume, battery], "then network");
        assert_eq!(shown(50 + BUTTON), [launcher, battery], "then volume");
        assert_eq!(shown(40 + BUTTON), [launcher], "then battery");
        assert_eq!(shown(40), [launcher], "the button still fits whole");
        assert_eq!(shown(39), Vec::<ModuleId>::new(), "the launcher button last");
        for w in [160u16, 120, 90, 70, 60, 50, 40].map(|w| w + BUTTON).into_iter().chain([40, 39]) {
            let g = b.grid("HDMI-A-1", w);
            assert_eq!(g.width(), w);
            let t = g.text();
            assert!(w < 40 || t.starts_with(" \u{f0349} │ "), "launcher button whole and first at {w}: {t}");
            assert!(t.ends_with(" 14:05 ") && t.contains(" 4!····+ "), "workspaces and clock whole at {w}: {t}");
        }
        insta::assert_snapshot!("full_160", snapshot(&b.grid("HDMI-A-1", 160)));
        insta::assert_snapshot!("full_120", snapshot(&b.grid("HDMI-A-1", 120)));
    }

    fn tray_icon(n: u32, icon: Icon, status: Status) -> TrayIcon {
        TrayIcon { key: ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() }, icon, status }
    }

    fn glyph(c: char) -> Icon {
        Icon::Glyph(c.to_string())
    }

    #[test]
    fn tray_glyphs_letters_and_attention() {
        let icons = [
            tray_icon(1, glyph(STEAM), Status::Active),
            tray_icon(2, Icon::Letter('K'), Status::Active),
            tray_icon(3, glyph(DISCORD), Status::NeedsAttention),
            tray_icon(4, Icon::Letter('N'), Status::NeedsAttention),
        ];
        let s = tray(&icons, &TrayConfig::default()).unwrap();
        assert_eq!(
            draw(&s),
            ("\u{f04d3} K \u{f066f} N".into(), "..G.e.e".into()),
            "glyph normal, letter bold accent, attention red whatever it is, one blank cell between"
        );
        insta::assert_snapshot!("tray_glyphs_letters_and_attention", snapshot(&lay_out(std::slice::from_ref(&s), s.width() + 2, None)));
    }

    #[test]
    fn passive_dim_show_hide() {
        let icons = [
            tray_icon(1, glyph(STEAM), Status::Passive),
            tray_icon(2, Icon::Letter('K'), Status::Active),
            tray_icon(3, Icon::Letter('N'), Status::Passive),
        ];
        let with = |passive| tray(&icons, &TrayConfig { passive, ..TrayConfig::default() });
        let dim = draw(&with(Passive::Dim).unwrap());
        let show = draw(&with(Passive::Show).unwrap());
        let hide = draw(&with(Passive::Hide).unwrap());
        assert_eq!(dim, ("\u{f04d3} K N".into(), "m.G.m".into()), "dim: muted");
        assert_eq!(show, ("\u{f04d3} K N".into(), "..G.G".into()), "show: as active");
        assert_eq!(hide, ("K".into(), "G".into()), "hide: not drawn, no blank cell left behind");
        let only_passive = [tray_icon(1, glyph(STEAM), Status::Passive)];
        assert!(tray(&only_passive, &TrayConfig { passive: Passive::Hide, ..TrayConfig::default() }).is_none(), "nothing left");
        let lines = [("dim", dim), ("show", show), ("hide", hide)].map(|(m, (t, st))| format!("{m:4} {t}\n{m:4} {st}"));
        insta::assert_snapshot!("passive_dim_show_hide", lines.join("\n"));
    }

    #[test]
    fn an_override_two_cells_wide() {
        let icons = [tray_icon(1, Icon::Glyph("日".into()), Status::Active), tray_icon(2, Icon::Letter('K'), Status::Active)];
        let s = tray(&icons, &TrayConfig::default()).unwrap();
        assert_eq!(s.width(), 4);
        let g = lay_out(std::slice::from_ref(&s), 6, None);
        let first = Target::TrayItem(icons[0].key.clone());
        assert_eq!((g.target_at(1), g.target_at(2)), (Some(&first), Some(&first)), "both cells of the wide glyph");
        assert_eq!(g.target_at(3), None, "the blank cell");
        assert_eq!(g.target_at(4), Some(&Target::TrayItem(icons[1].key.clone())));
        assert_eq!(g.target_at(5), None, "the right margin");
        insta::assert_snapshot!("an_override_two_cells_wide", snapshot(&g));
    }

    #[test]
    fn a_fallback_letter_can_be_two_cells_wide() {
        // U+65E5 (日) is both alphanumeric and East Asian wide: a plausible
        // fallback letter for an unrecognised CJK application, drawn over
        // two cells, both bold accent like any other letter.
        let icons = [tray_icon(1, Icon::Letter('日'), Status::Active), tray_icon(2, Icon::Letter('K'), Status::Active)];
        let s = tray(&icons, &TrayConfig::default()).unwrap();
        assert_eq!(s.width(), 4, "日 (2 cells) + a blank + K (1 cell)");
        let g = lay_out(std::slice::from_ref(&s), s.width() + 2, None);
        let first = Target::TrayItem(icons[0].key.clone());
        assert_eq!((g.target_at(1), g.target_at(2)), (Some(&first), Some(&first)), "both cells of the wide letter target the item");
        assert_eq!(&g.styles()[1..3], "GG", "a letter is bold accent, whatever its width");
    }

    #[test]
    fn empty_tray_draws_no_segment() {
        assert!(tray(&[], &TrayConfig::default()).is_none());
        let mut b = Bar::new(BarConfig::default());
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Tray, ModuleId::Clock];
        b.apply(Msg::Compositor(Some(example())));
        b.apply(Msg::Clock(SUN));
        let t = b.grid("HDMI-A-1", 60).text();
        assert_eq!(t.matches('│').count(), 1, "no \"│ │\" for an empty tray: {t}");
        b.config.tray.passive = Passive::Hide;
        b.apply(Msg::Tray(vec![tray_icon(1, glyph(STEAM), Status::Passive)]));
        let t = b.grid("HDMI-A-1", 60).text();
        assert_eq!(t.matches('│').count(), 1, "every icon hidden: no segment either: {t}");
        assert!(!t.contains(STEAM));
    }

    #[test]
    fn tray_zones_target_each_item() {
        let mut b = Bar::new(BarConfig::default());
        b.config.modules = vec![ModuleId::Tray, ModuleId::Clock];
        b.apply(Msg::Clock(SUN));
        let icons = vec![
            tray_icon(1, glyph(STEAM), Status::Active),
            tray_icon(2, Icon::Letter('K'), Status::Passive),
            tray_icon(3, glyph(TELEGRAM), Status::NeedsAttention),
        ];
        b.apply(Msg::Tray(icons.clone()));
        let g = b.grid("HDMI-A-1", 40);
        assert!(g.text().starts_with(" \u{f04d3} K \u{f2c6}"), "{}", g.text());
        for (col, i) in [(1u16, 0usize), (3, 1), (5, 2)] {
            assert_eq!(g.target_at(col), Some(&Target::TrayItem(icons[i].key.clone())), "icon {i} at {col}");
        }
        for blank in [0u16, 2, 4, 6] {
            assert_eq!(g.target_at(blank), None, "no zone on the margin, the blank cells or the separator ({blank})");
        }
        // Counted among the tray's own targets only: other modules may have zones of their own
        // (task 18 gives the clock one).
        let tray_zones = g.zones().iter().filter(|z| matches!(z.target, Target::TrayItem(_))).count();
        assert_eq!(tray_zones, 3, "one zone per icon");
        assert!(!g.zones().iter().any(|z| z.target == Target::Module(ModuleId::Tray)), "none over the whole segment");
        insta::assert_snapshot!("tray_zones_target_each_item", snapshot(&g));
    }

    fn full_bar_with_a_tray() -> Bar {
        let mut b = full_bar();
        b.apply(Msg::Tray(vec![
            tray_icon(1, glyph(STEAM), Status::Active),
            tray_icon(2, glyph(DISCORD), Status::NeedsAttention),
            tray_icon(3, Icon::Letter('K'), Status::Passive),
        ]));
        b
    }

    #[test]
    fn narrow_output_drops_the_tray_in_order() {
        // Widths measured on this bar (with the launcher button ahead of
        // the workspaces): every module is present at 150 columns and
        // above; the tray stays down to 108; the title stays down to
        // 100 — truncated below that, not removed. The window title's
        // "Web page" mark alone needs at least 11 title cells, so a
        // narrowing title can lose the mark well before the module
        // itself is dropped (seen here too, briefly, just under 150);
        // the widths below are chosen clear of that truncation band.
        const WIDE: u16 = 160;
        const NO_RESOURCES: u16 = 130;
        const NO_TRAY: u16 = 105;
        const NO_TITLE: u16 = 80;
        let (window, tray, battery, resources) = (ModuleId::Window, ModuleId::Tray, ModuleId::Battery, ModuleId::Resources);
        let b = full_bar_with_a_tray();
        let marks = [(window, "Web page"), (tray, "\u{f04d3}"), (battery, "\u{f008b}"), (resources, "\u{f035b}")];
        let shown = |w: u16| {
            let t = b.grid("HDMI-A-1", w).text();
            marks.iter().filter(|(_, s)| t.contains(s)).map(|(m, _)| *m).collect::<Vec<_>>()
        };
        assert_eq!(shown(WIDE), [window, tray, battery, resources]);
        assert_eq!(shown(NO_RESOURCES), [window, tray, battery], "resources leave first");
        assert_eq!(shown(NO_TRAY), [window, battery], "then the tray, before the title");
        assert_eq!(shown(NO_TITLE), [battery], "then the title");
        for w in [WIDE, NO_RESOURCES, NO_TRAY, NO_TITLE] {
            let g = b.grid("HDMI-A-1", w);
            assert_eq!(g.width(), w);
            assert!(g.text().ends_with(" 14:05 "), "the clock whole at {w}: {}", g.text());
        }
    }

    #[test]
    fn a_full_bar_with_a_tray_on_the_hdmi_screen() {
        let g = full_bar_with_a_tray().grid("HDMI-A-1", 160);
        assert!(g.text().contains(" │ \u{f04d3} \u{f066f} K │ "), "between resources and the clock: {}", g.text());
        insta::assert_snapshot!("full_tray_160", snapshot(&g));
    }

    /// Spec § 4: low battery is warn, critical (below CRITICAL, 10 %) is error — the laptop, a
    /// peripheral with a percentage, a Bluetooth device alike. Charging still wins.
    #[test]
    fn battery_low_is_warn_and_critical_is_error() {
        let laptop = |percent, charge| Feed::Ready(PowerState { laptop: Some(Battery { percent, charge }), peripherals: vec![] });
        let d = &BatteryConfig::default();
        assert_eq!(draw(&battery(&laptop(10, Charge::Discharging), d).unwrap()).1, "wwwww", "10 %: still only low");
        assert_eq!(draw(&battery(&laptop(9, Charge::Discharging), d).unwrap()).1, "eeee", "9 %: critical");
        assert_eq!(draw(&battery(&laptop(9, Charge::Charging), d).unwrap()).1, "aaaa", "charging wins over critical");
        let mouse = |percent| Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: Some(percent), low: false };
        let s = Feed::Ready(PowerState { laptop: None, peripherals: vec![mouse(9), mouse(10)] });
        assert_eq!(draw(&battery(&s, d).unwrap()).1, "eeeewwwwww", "each peripheral on its own");
        assert_eq!(draw(&bluetooth(&on(&[("CasqueBT", Some(9))])).unwrap()).1, "aa........eee", "Bluetooth: critical");
        assert_eq!(CRITICAL, 10);
    }
}
