//! Segments of the system modules (spec § 2): volume, network, Bluetooth,
//! battery, resources, the tray. Each returns None when it has nothing to
//! show — waiting for its first reading, or nothing to report; a source that
//! is down shows its icon then "–". The whole segment is one click zone,
//! except the tray's: one zone per icon.

use crate::config::{BatteryConfig, BatteryVisibility, ModuleId, Passive, QuickConfig, ResourceItem, TrayConfig};
use crate::grid::{Hue, Style, Target, truncate};
use crate::layout::{Piece, Segment, piece};
use crate::media::MediaView;
use crate::notify::NotifyView;
use crate::privacy::{CAMERA, MIC, Privacy, SCREEN};
use crate::session::{self, POWER};
use crate::session_bus::Status;
use crate::session_bus::icons::{Icon, TrayIcon};
use crate::state::Feed;
use crate::system::{Battery, BtState, Charge, Link, NetState, PeripheralKind, PowerState, Resources, SoundState};
use alpheus_commun::theme::settings::ModuleColors;

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
/// The notifications bell (notifications spec § 4), checked in the 3.4.0 post table.
pub const BELL: char = '\u{f009a}'; // md-bell
pub const BELL_OFF: char = '\u{f009b}'; // md-bell_off (crossed)
/// The most unread notifications the bell counts; beyond, "99+".
pub const UNREAD_MAX: u16 = 99;

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

/// The launcher's button: always shown, one click zone (spec § 2).
pub(crate) fn launcher() -> Segment {
    module_segment(ModuleId::Launcher, vec![piece(LAUNCHER.to_string(), Style::NORMAL)])
}

/// The power button (power spec § 1): ⏻ alone, one click zone; none when `[quick]`'s lock, logout,
/// reboot and poweroff are all empty (its menu would have no entry).
pub(crate) fn power(c: &QuickConfig) -> Option<Segment> {
    session::any_configured(c).then(|| module_segment(ModuleId::Power, vec![piece(POWER.to_string(), Style::NORMAL)]))
}

/// The privacy indicators (privacy spec § 1): a glyph per held device, joined by one space, each in
/// its own hue (orange, green, red) whatever `module_colors` says; the microphone dim while the
/// default source is muted. None when nothing is shown. One click zone, no block hue.
pub(crate) fn privacy(p: &Privacy) -> Option<Segment> {
    let shown = p.shown();
    let mic = if p.mic_muted() { Style::MUTED } else { Style::hue(Hue::Orange) };
    let mut pieces: Vec<Piece> = Vec::new();
    for (on, glyph, style) in [
        (shown.mic.active(), MIC, mic),
        (shown.camera.active(), CAMERA, Style::hue(Hue::Green)),
        (shown.screen.active(), SCREEN, Style::hue(Hue::Red)),
    ] {
        if on {
            let sep = if pieces.is_empty() { "" } else { " " };
            pieces.push(piece(format!("{sep}{glyph}"), style));
        }
    }
    if pieces.is_empty() {
        return None;
    }
    let mut segment = module_segment(ModuleId::Privacy, pieces);
    segment.withhold_hue = true;
    Some(segment)
}

pub(crate) fn volume(feed: &Feed<SoundState>, mc: ModuleColors) -> Option<Segment> {
    let v = match feed {
        Feed::Waiting | Feed::Ready(SoundState { sink: None, .. }) => return None,
        Feed::Down => return Some(down(ModuleId::Volume, VOLUME_HIGH)),
        Feed::Ready(SoundState { sink: Some(v), .. }) => *v,
    };
    let pieces = if v.muted {
        vec![piece(format!("{VOLUME_OFF} "), Style::MUTED), piece("muted".to_string(), Style::MUTED)]
    } else {
        let icon = if v.percent < 34 {
            VOLUME_LOW
        } else if v.percent < 67 {
            VOLUME_MEDIUM
        } else {
            VOLUME_HIGH
        };
        vec![
            piece(format!("{icon} "), icon_style(mc, Hue::Blue, Style::NORMAL)),
            piece(format!("{}%", v.percent), text_style(mc, Hue::Blue, Style::NORMAL)),
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
pub const CPU: char = '\u{f061a}'; // md-chip
pub const THERMOMETER: char = '\u{f050f}'; // md-thermometer
pub const DOWNLOAD: char = '\u{f01da}'; // md-download
pub const UPLOAD: char = '\u{f0552}'; // md-upload
pub const HARDDISK: char = '\u{f02ca}'; // md-harddisk
pub const GPU: char = '\u{f08ae}'; // md-expansion-card
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

/// A byte rate, at most four cells: `0B`..`999B`, then `K`, `M`, `G` (1024 steps, one decimal under
/// 10, none above, `1863G` the widest).
pub(crate) fn rate(bytes_per_s: u64) -> String {
    if bytes_per_s < 1000 {
        return format!("{bytes_per_s}B");
    }
    let mut v = bytes_per_s as f64 / 1024.0;
    for unit in ['K', 'M', 'G'] {
        if unit == 'G' || v.round() < 1000.0 {
            return if (v * 10.0).round() < 100.0 { format!("{v:.1}{unit}") } else { format!("{v:.0}{unit}") };
        }
        v /= 1024.0;
    }
    unreachable!()
}

fn figure(pieces: &mut Vec<Piece>, gap: &str, icon: char, hue: Hue, text: String, mc: ModuleColors) {
    pieces.push(piece(format!("{gap}{icon}"), icon_style(mc, hue, Style::NORMAL)));
    pieces.push(piece(format!(" {text}"), text_style(mc, hue, Style::NORMAL)));
}

/// A temperature: its hue, the error tint as a whole from `HOT`.
fn temperature_figure(pieces: &mut Vec<Piece>, gap: &str, icon: char, t: i16, hue: Hue, mc: ModuleColors) {
    if t >= HOT {
        pieces.push(piece(format!("{gap}{icon} {t}°"), Style::ERROR));
    } else {
        figure(pieces, gap, icon, hue, format!("{t}°"), mc);
    }
}

/// The resources: an icon and a figure each, in the order `show` lists them, nothing padded. An item
/// whose source has no figure is skipped (the CPU alone shows `…%` before its first reading); None
/// when no item is left. The `[resources] graph` setting is still read but ignored.
pub(crate) fn resources(r: &Resources, show: &[ResourceItem], mc: ModuleColors) -> Option<Segment> {
    let s = r.last?;
    let mut pieces: Vec<Piece> = Vec::new();
    for &item in show {
        // The first piece has no leading gap; the others are three cells apart.
        let gap = if pieces.is_empty() { "" } else { "   " };
        let mut item_pieces = |icon: char, hue: Hue, text: String| figure(&mut pieces, gap, icon, hue, text, mc);
        match item {
            ResourceItem::Cpu => {
                let text = s.cpu.map_or_else(|| "…%".to_string(), |p| format!("{p}%"));
                // The CPU's icon owns its trailing space (the bar has always drawn it so).
                pieces.push(piece(format!("{gap}{CPU} "), icon_style(mc, Hue::Orange, Style::NORMAL)));
                pieces.push(piece(text, text_style(mc, Hue::Orange, Style::NORMAL)));
            }
            // Tenths of GiB, one decimal (the decimal part is always ".D", Q5).
            ResourceItem::Memory => {
                if let Some(m) = s.memory {
                    item_pieces(MEMORY, Hue::Yellow, format!("{}.{}G", m / 10, m % 10));
                }
            }
            ResourceItem::Temperature => {
                if let Some(t) = s.temperature {
                    temperature_figure(&mut pieces, gap, THERMOMETER, t, Hue::Red, mc);
                }
            }
            ResourceItem::Net => {
                if let Some(n) = s.net {
                    item_pieces(DOWNLOAD, Hue::Cyan, format!("{} {UPLOAD} {}", rate(n.down), rate(n.up)));
                }
            }
            ResourceItem::Disk => {
                if let Some(d) = s.disk {
                    item_pieces(HARDDISK, Hue::Blue, format!("{d}%"));
                }
            }
            ResourceItem::Gpu => {
                if let Some(t) = s.gpu {
                    temperature_figure(&mut pieces, gap, GPU, t, Hue::Purple, mc);
                }
            }
        }
    }
    if pieces.is_empty() {
        return None;
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

/// The bell (notifications spec § 4, amended): `󰂚`, then ` N` in the accent with N unread; the
/// crossed `󰂛` under do-not-disturb; all dimmed under do-not-disturb or while another server holds
/// the name. Never a hue, whatever `module_colors` says. None before the server has said anything.
/// The whole segment is one click zone.
pub(crate) fn notifications(view: Option<&NotifyView>) -> Option<Segment> {
    let v = view?;
    let glyph = if v.dnd { BELL_OFF } else { BELL };
    let count = match v.unread {
        0 => String::new(),
        n if n > UNREAD_MAX => format!(" {UNREAD_MAX}+"),
        n => format!(" {n}"),
    };
    let pieces = if v.dnd || !v.serving {
        vec![piece(format!("{glyph}{count}"), Style::MUTED)]
    } else if count.is_empty() {
        vec![piece(glyph.to_string(), Style::NORMAL)]
    } else {
        vec![piece(glyph.to_string(), Style::NORMAL), piece(count, Style::ACCENT)]
    };
    Some(module_segment(ModuleId::Notifications, pieces))
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
mod tests;
