use super::*;
use crate::clock::LocalTime;
use crate::compositor::example;
use crate::config::{BarConfig, BatteryConfig, BatteryVisibility};
use crate::grid::Grid;
use crate::layout::lay_out;
use crate::notify::NotifyView;
use crate::privacy::{Capture, Indicator};
use crate::session_bus::ItemKey;
use crate::session_bus::icons::{DISCORD, STEAM, TELEGRAM};
use crate::state::{Bar, Msg};
use crate::system::{Battery, BtDevice, Charge, Peripheral, PeripheralKind, PowerState, Resources, Sample, Volume};

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
fn resources(r: &Resources) -> Option<Segment> {
    super::resources(r, ModuleColors::None)
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
    Feed::Ready(SoundState { sink: Some(Volume { percent, muted }), ..SoundState::default() })
}

fn online(link: Link, vpns: usize, limited: bool) -> Feed<NetState> {
    Feed::Ready(NetState::Online { link, vpns, limited })
}

fn on(devices: &[(&str, Option<u8>)]) -> Feed<BtState> {
    Feed::Ready(BtState::On { devices: devices.iter().map(|(n, b)| BtDevice { name: n.to_string(), battery: *b }).collect() })
}

#[test]
fn volume_icon_and_percent() {
    // Icon and number only (the stair gauge said the same thing twice); no padding.
    assert_eq!(draw(&volume(&sound(62, false)).unwrap()), ("\u{f0580} 62%".into(), ".....".into()));
    assert_eq!(draw(&volume(&sound(0, false)).unwrap()), ("\u{f057f} 0%".into(), "....".into()));
    assert_eq!(draw(&volume(&sound(150, false)).unwrap()), ("\u{f057e} 150%".into(), "......".into()));
    assert_eq!(
        draw(&volume(&sound(62, true)).unwrap()),
        ("\u{f0581} muted".into(), "mmmmmmm".into()),
        "muted: crossed out, dimmed, no figure"
    );
    let f = sound(62, false);
    assert_eq!(draw(&super::volume(&f, ModuleColors::None).unwrap()).1, ".....");
    assert_eq!(draw(&super::volume(&f, ICONS).unwrap()).1, "BB...", "icon style under icons");
    assert_eq!(draw(&super::volume(&f, FULL).unwrap()).1, "BBBBB", "icon and text style under full");
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
    assert!(volume(&Feed::Ready(SoundState::default())).is_none());
    assert_eq!(draw(&volume(&Feed::Down).unwrap()), ("\u{f057e} –".into(), "mmm".into()));
}

#[test]
fn network_icons_only_and_vpn_count() {
    assert_eq!(draw(&network(&online(Link::Ethernet, 2, false)).unwrap()), ("\u{f0200} \u{f0582} 2".into(), ".aaaa".into()));
    assert_eq!(draw(&network(&online(Link::Wifi, 1, false)).unwrap()), ("\u{f05a9} \u{f0582}".into(), ".aa".into()), "one VPN: no count");
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
    assert_eq!(t.matches("  ·  ").count(), 1, "no \"· ·\" without a player: {t}");
    b.apply(Msg::Media(Some(song(true, "Song"))));
    let t = b.grid("HDMI-A-1", 60).text();
    assert_eq!(t.matches("  ·  ").count(), 2, "{t}");
    assert!(t.contains("  ·  \u{f040a} Song "), "{t}");
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

fn shown(mic: bool, camera: bool, screen: bool) -> Privacy {
    let held = |on: bool| Indicator { names: Vec::new(), unnamed: on };
    let mut p = Privacy::new(Vec::new());
    p.set_sound(Some(&Capture { mic: held(mic), camera: held(camera), screen: held(screen) }), false, std::time::Instant::now());
    p
}

/// Privacy spec § 1: each active glyph in its fixed hue, whatever `module_colors` says.
#[test]
fn the_privacy_segment_shows_each_active_glyph_in_its_hue() {
    for mc in [ModuleColors::None, ICONS, FULL, ModuleColors::Pills] {
        let s = super::privacy(&shown(true, true, true)).unwrap();
        assert_eq!(draw(&s), ("\u{f036c} \u{f0100} \u{f0379}".into(), "oggrr".into()), "{mc:?}");
        assert_eq!((s.module, &s.target, s.withhold_hue), (ModuleId::Privacy, &Some(Target::Module(ModuleId::Privacy)), true));
    }
    assert_eq!(draw(&super::privacy(&shown(false, true, false)).unwrap()), ("\u{f0100}".into(), "g".into()), "the camera alone, no join");
    assert_eq!(draw(&super::privacy(&shown(false, false, true)).unwrap()), ("\u{f0379}".into(), "r".into()));
    assert_eq!(draw(&super::privacy(&shown(true, false, true)).unwrap()), ("\u{f036c} \u{f0379}".into(), "orr".into()));
}

#[test]
fn the_muted_microphone_is_dim_and_only_it() {
    let mut p = shown(true, true, false);
    p.set_sound(
        Some(&Capture {
            mic: Indicator { names: Vec::new(), unnamed: true },
            camera: Indicator { names: Vec::new(), unnamed: true },
            screen: Indicator::default(),
        }),
        true,
        std::time::Instant::now(),
    );
    assert!(p.mic_muted());
    assert_eq!(draw(&super::privacy(&p).unwrap()), ("\u{f036c} \u{f0100}".into(), "mgg".into()));
}

#[test]
fn the_privacy_segment_is_hidden_when_nothing_is_shown() {
    assert!(super::privacy(&Privacy::new(Vec::new())).is_none());
    assert!(super::privacy(&shown(false, false, false)).is_none());
}

#[test]
fn media_glyphs_are_one_cell_each() {
    assert_eq!([PLAY, PAUSE, SKIP_PREVIOUS, SKIP_NEXT], ['\u{f040a}', '\u{f03e4}', '\u{f04ae}', '\u{f04ad}']);
    for g in [PLAY, PAUSE, SKIP_PREVIOUS, SKIP_NEXT] {
        assert_eq!(crate::grid::text_width(&g.to_string()), 1, "{g:?}");
    }
}

fn bell(unread: u16, dnd: bool, serving: bool) -> NotifyView {
    NotifyView { unread, dnd, serving }
}

#[test]
fn the_bell_counts_the_unread_in_the_accent() {
    let s = super::notifications(Some(&bell(0, false, true))).unwrap();
    assert_eq!(draw(&s), ("\u{f009a}".into(), ".".into()));
    assert_eq!((s.module, &s.target), (ModuleId::Notifications, &Some(Target::Module(ModuleId::Notifications))), "one click zone");
    assert_eq!(draw(&super::notifications(Some(&bell(3, false, true))).unwrap()), ("\u{f009a} 3".into(), ".aa".into()));
    assert_eq!(draw(&super::notifications(Some(&bell(42, false, true))).unwrap()).0, "\u{f009a} 42");
    assert_eq!(draw(&super::notifications(Some(&bell(99, false, true))).unwrap()).0, "\u{f009a} 99");
    assert_eq!(draw(&super::notifications(Some(&bell(150, false, true))).unwrap()).0, "\u{f009a} 99+", "never wider than 99+");
}

#[test]
fn do_not_disturb_crosses_the_bell_and_not_serving_dims_it() {
    let d = |v: NotifyView| draw(&super::notifications(Some(&v)).unwrap());
    assert_eq!(d(bell(0, true, true)), ("\u{f009b}".into(), "m".into()));
    assert_eq!(d(bell(2, true, true)), ("\u{f009b} 2".into(), "mmm".into()), "the count stays, dimmed");
    assert_eq!(d(bell(0, false, false)), ("\u{f009a}".into(), "m".into()), "another server holds the name: a dim bell");
    assert_eq!(d(bell(5, false, false)), ("\u{f009a} 5".into(), "mmm".into()), "with the history it already has");
    assert_eq!(d(bell(1, true, false)), ("\u{f009b} 1".into(), "mmm".into()));
    assert!(super::notifications(None).is_none(), "nothing heard yet: no segment");
}

#[test]
fn bell_glyphs_are_one_cell_each() {
    assert_eq!([BELL, BELL_OFF], ['\u{f009a}', '\u{f009b}']);
    for g in [BELL, BELL_OFF] {
        assert_eq!(crate::grid::text_width(&g.to_string()), 1, "{g:?}");
    }
}

fn three_modules_bar() -> Bar {
    let mut b = Bar::new(BarConfig::default());
    b.config.modules =
        vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Volume, ModuleId::Network, ModuleId::Bluetooth, ModuleId::Clock];
    b.apply(Msg::Compositor(Some(example())));
    b.apply(Msg::Clock(SUN));
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
    b.apply(Msg::Network(Some(NetState::Online { link: Link::Ethernet, vpns: 2, limited: false })));
    b.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(72) }] })));
    b
}

#[test]
fn each_module_segment_is_one_click_zone() {
    let g = three_modules_bar().grid("HDMI-A-1", 100);
    let t = g.text();
    assert!(t.contains("  ·  \u{f057e} 84%  ·  \u{f0200} \u{f0582} 2  ·  \u{f00b1} CasqueBT 72%  ·  Sun 4 Oct 14:05 "), "{t}");
    for (icon, module, width) in
        [(VOLUME_HIGH, ModuleId::Volume, 5u16), (ETHERNET, ModuleId::Network, 5), (BLUETOOTH_CONNECTED, ModuleId::Bluetooth, 14)]
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
    let laptop = |percent| Feed::Ready(PowerState { laptop: Some(Battery { percent, charge: Charge::Discharging }), peripherals: vec![] });
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

fn resources_with(last: Sample) -> Resources {
    Resources { last: Some(last) }
}

#[test]
fn resources_cpu_memory_temperature() {
    let s = Sample { cpu: Some(12), memory: Some(61), temperature: Some(45) };
    assert_eq!(
        draw(&resources(&resources_with(s)).unwrap()),
        ("\u{f061a} 12%   \u{f035b} 6.1G   \u{f050f} 45\u{b0}".into(), "......................".into())
    );
    let first = Sample { cpu: None, memory: None, temperature: Some(91) };
    let (t, st) = draw(&resources(&resources_with(first)).unwrap());
    assert!(t.starts_with("\u{f061a} \u{2026}%"), "first reading: no CPU yet, no padding: {t}");
    assert!(st.ends_with("eeeeeeee"), "hot: error tint ({st})");
    assert!(resources(&Resources::default()).is_none(), "nothing read yet");
    let small = Sample { cpu: Some(9), memory: Some(5), temperature: None };
    assert_eq!(draw(&resources(&resources_with(small)).unwrap()).0, "\u{f061a} 9%   \u{f035b} 0.5G");
}

#[test]
fn resources_temperature_tint_boundary_at_84_and_85() {
    // The error tint from HOT (85 \u{b0}C, waybar's own critical threshold,
    // Mae 2026-10-04): 84\u{b0} is still normal, 85\u{b0} already errs.
    let normal = Sample { cpu: Some(50), memory: None, temperature: Some(84) };
    let hot = Sample { cpu: Some(50), memory: None, temperature: Some(85) };
    assert_eq!(
        draw(&resources(&resources_with(normal)).unwrap()),
        ("\u{f061a} 50%   \u{f050f} 84\u{b0}".into(), ".............".into()),
        "84\u{b0}: normal"
    );
    assert_eq!(
        draw(&resources(&resources_with(hot)).unwrap()),
        ("\u{f061a} 50%   \u{f050f} 85\u{b0}".into(), ".....eeeeeeee".into()),
        "85\u{b0}: error"
    );
}

fn full_bar() -> Bar {
    let mut b = Bar::new(BarConfig::default());
    b.apply(Msg::Compositor(Some(example())));
    b.apply(Msg::Clock(SUN));
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
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
    assert_eq!(draw(&super::volume(&v, ModuleColors::None).unwrap()).1, ".....");
    assert_eq!(draw(&super::volume(&v, ICONS).unwrap()).1, "BB...", "blue icon (and its space), the number as before");
    assert_eq!(draw(&super::volume(&v, FULL).unwrap()).1, "BBBBB", "the number takes the blue too");
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
    let s = Sample { cpu: Some(50), memory: Some(61), temperature: Some(67) };
    let r = resources_with(s);
    let none = draw(&super::resources(&r, ModuleColors::None).unwrap());
    let (text, icons) = draw(&super::resources(&r, ICONS).unwrap());
    let (_, full) = draw(&super::resources(&r, FULL).unwrap());
    assert_eq!(text, none.0, "the text never changes");
    assert_eq!(none.1, "......................", "none: no hue at all");
    assert_eq!(icons, "oo...yyyy.....rrrr....", "the three icons (their leading spaces too)");
    assert_eq!(full, "oooooyyyyyyyyyrrrrrrrr", "icons and texts, each in its hue");
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
        assert_eq!(
            draw(&super::resources(&resources_with(hot), mc).unwrap()).1[5..],
            *"eeeeeeee",
            "hot temperature stays error under {mc:?}"
        );
        assert_eq!(
            draw(&super::network(&online(Link::Wifi, 0, true), mc).unwrap()).1,
            "eee",
            "limited connectivity stays error under {mc:?}"
        );
        assert_eq!(draw(&super::volume(&sound(84, true), mc).unwrap()).1, "mmmmmmm", "muted stays dim under {mc:?}");
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
    assert_eq!(draw(&super::volume(&v, ModuleColors::Pills).unwrap()).1, "BBBBB", "the number in blue, as under full");
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
        (resources, "\u{f061a}"),
    ];
    let shown = |w: u16| {
        let t = b.grid("HDMI-A-1", w).text();
        marks.iter().filter(|(_, s)| t.contains(s)).map(|(m, _)| *m).collect::<Vec<_>>()
    };
    // Widths measured on this bar (the separator is 5 cells wide, the volume and the resources
    // carry no gauge or graph): each width sits clear of the title's truncation band, which
    // hides the "Web page" mark before the module itself leaves.
    assert_eq!(shown(160), [launcher, window, volume, network, bluetooth, battery, resources]);
    assert_eq!(shown(125), [launcher, window, volume, network, bluetooth, battery], "resources leave first");
    // Each width pins one more step of REMOVAL's order; a swap inside it
    // (title/Bluetooth, Bluetooth/network, volume/battery or
    // battery/launcher) turns one of these red (see the "prove it bites"
    // mutations).
    assert_eq!(shown(105), [launcher, volume, network, bluetooth, battery], "then the title");
    assert_eq!(shown(90), [launcher, volume, network, battery], "then Bluetooth");
    assert_eq!(shown(75), [launcher, volume, battery], "then network");
    assert_eq!(shown(65), [launcher, battery], "then volume");
    assert_eq!(shown(55), [launcher], "then battery");
    assert_eq!(shown(50), [launcher], "the button still fits whole");
    assert_eq!(shown(49), Vec::<ModuleId>::new(), "the launcher button last but for ⏻");
    for w in [160u16, 125, 105, 90, 75, 65, 55, 50, 49] {
        let g = b.grid("HDMI-A-1", w);
        assert_eq!(g.width(), w);
        let t = g.text();
        assert!(w < 50 || t.starts_with(" \u{f0349}  ·  "), "launcher button whole and first at {w}: {t}");
        assert!(t.ends_with(" 14:05  ·  \u{23fb} ") && t.contains(" 4!····+ "), "workspaces, clock and ⏻ whole at {w}: {t}");
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
    assert_eq!(t.matches("  ·  ").count(), 1, "no \"· ·\" for an empty tray: {t}");
    b.config.tray.passive = Passive::Hide;
    b.apply(Msg::Tray(vec![tray_icon(1, glyph(STEAM), Status::Passive)]));
    let t = b.grid("HDMI-A-1", 60).text();
    assert_eq!(t.matches("  ·  ").count(), 1, "every icon hidden: no segment either: {t}");
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
    // Counted among the tray's own targets only: other modules may have zones of their own.
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
    // Widths measured on this bar (with the launcher button ahead of the workspaces, the 5-cell
    // separators and no gauge or graph): every module is present from 152 columns; the tray
    // stays down to 125; the title stays down to 115 — truncated below that, not removed. The
    // window title's "Web page" mark alone needs at least 11 title cells, so a narrowing title
    // can lose the mark well before the module itself is dropped (seen here too, briefly, just
    // under 152); the widths below are chosen clear of that truncation band.
    const WIDE: u16 = 160;
    const NO_RESOURCES: u16 = 135;
    const NO_TRAY: u16 = 118;
    const NO_TITLE: u16 = 90;
    let (window, tray, battery, resources) = (ModuleId::Window, ModuleId::Tray, ModuleId::Battery, ModuleId::Resources);
    let b = full_bar_with_a_tray();
    let marks = [(window, "Web page"), (tray, "\u{f04d3}"), (battery, "\u{f008b}"), (resources, "\u{f061a}")];
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
        assert!(g.text().ends_with(" 14:05  ·  \u{23fb} "), "the clock and ⏻ whole at {w}: {}", g.text());
    }
}

#[test]
fn a_full_bar_with_a_tray_on_the_hdmi_screen() {
    let g = full_bar_with_a_tray().grid("HDMI-A-1", 160);
    assert!(g.text().contains("  ·  \u{f04d3} \u{f066f} K  ·  "), "between resources and the clock: {}", g.text());
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
