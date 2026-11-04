//! The portal's color-scheme on a private session bus (`common::PrivateBus`, nothing activatable),
//! with a fake portal: never the workstation's buses. Ignored by default:
//! `nice -n 19 nix develop -c env CARGO_BUILD_JOBS=4 cargo test -p alpheus-demon --test appearance_private_bus -- --ignored`

mod common;

use alpheus_commun::theme::Mode;
use alpheus_commun::theme::Roles;
use alpheus_commun::theme::presets::preset;
use alpheus_commun::theme::settings::{ModeSetting, ThemeSettings, effective_mode, resolve};
use alpheus_demon::link::{LinkOutput, SessionLink};
use alpheus_demon::session_bus::PORTAL_NAME;
use alpheus_demon::session_bus::bus::{SessionBus, TIMEOUT};
use alpheus_demon::session_bus::router::Tray;
use common::portal::FakePortal;
use common::{Peer, PrivateBus};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn with_the_tray_the_portal_follows_on_its_connection() {
    let bus = PrivateBus::start("appearance-tray");
    let portal = FakePortal::start(&bus, 1);
    let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), PathBuf::from("/proc"));
    tray.start_appearance();
    let mut seen = Vec::new();
    let mut pump = |seen: &mut Vec<Mode>, n: usize| {
        let end = Instant::now() + Duration::from_secs(5);
        while seen.len() < n && Instant::now() < end {
            let mut out = tray.on_readable();
            if tray.next_deadline().is_some_and(|d| d <= Instant::now()) {
                out.merge(tray.on_deadline(Instant::now()));
            }
            seen.extend(out.appearance);
            thread::sleep(Duration::from_millis(5));
        }
    };
    pump(&mut seen, 1);
    assert_eq!(seen, [Mode::Dark], "ReadOne answered 1");
    portal.set(2);
    pump(&mut seen, 2);
    assert_eq!(seen, [Mode::Dark, Mode::Light], "then SettingChanged 2");
    assert_eq!(portal.calls(), ["org.freedesktop.portal.Settings.ReadOne(org.freedesktop.appearance, color-scheme)"]);
}

/// Pumps `link` as the bar's loop does (readable, then any due deadline) until `done` holds or
/// `within` passes; every non-empty output, in order.
fn pump(link: &mut SessionLink, within: Duration, done: impl Fn(&SessionLink) -> bool) -> Vec<LinkOutput> {
    let end = Instant::now() + within;
    let mut outs = Vec::new();
    while !done(link) && Instant::now() < end {
        let mut out = link.on_readable();
        if out == LinkOutput::default() && link.next_deadline().is_some_and(|d| d <= Instant::now()) {
            out = link.on_deadline(Instant::now());
        }
        if out != LinkOutput::default() {
            outs.push(out);
        }
        thread::sleep(Duration::from_millis(5));
    }
    outs
}

/// The bar's link following the portal alone (mode auto, tray off, no media module).
fn portal_link(bus: SessionBus) -> SessionLink {
    let mut link = SessionLink::new(bus);
    link.start_appearance();
    link
}

/// The palette in effect for `mode = "auto"` and this portal word: catppuccin (the default), no colour files.
fn palette(portal: Option<Mode>) -> Roles {
    let dir = std::env::temp_dir().join(format!("alpheus-appearance-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    resolve(&ThemeSettings::default(), effective_mode(ModeSetting::Auto, portal), &dir).0
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn read_one_then_setting_changed_turns_the_palette_light() {
    let bus = PrivateBus::start("appearance-follow");
    let portal = FakePortal::start(&bus, 1);
    let mut link = portal_link(SessionBus::at(&bus.address).unwrap());
    let outs = pump(&mut link, Duration::from_secs(5), |l| l.pending() == 0);
    assert_eq!(outs, [LinkOutput { mode: Some(Mode::Dark), ..LinkOutput::default() }]);
    assert_eq!(palette(link.portal()), preset("catppuccin").unwrap().dark);
    portal.set(2);
    let outs = pump(&mut link, Duration::from_secs(5), |l| l.portal() == Some(Mode::Light));
    assert_eq!(outs, [LinkOutput { mode: Some(Mode::Light), ..LinkOutput::default() }]);
    assert_eq!(palette(link.portal()), preset("catppuccin").unwrap().light.unwrap(), "the palette turns light");
    assert_eq!(
        portal.calls(),
        ["org.freedesktop.portal.Settings.ReadOne(org.freedesktop.appearance, color-scheme)"],
        "one read, nothing else"
    );
    assert_eq!((link.pending(), link.next_deadline()), (0, None), "nothing left pending: no timer at rest");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_nested_variant_reads_the_same() {
    let bus = PrivateBus::start("appearance-nested");
    let _portal = FakePortal::start_nested(&bus, 2);
    let mut link = portal_link(SessionBus::at(&bus.address).unwrap());
    let outs = pump(&mut link, Duration::from_secs(5), |l| l.pending() == 0);
    assert_eq!(outs, [LinkOutput { mode: Some(Mode::Light), ..LinkOutput::default() }]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn another_key_or_namespace_changes_nothing() {
    let bus = PrivateBus::start("appearance-unrelated");
    let portal = FakePortal::start(&bus, 2);
    let mut link = portal_link(SessionBus::at(&bus.address).unwrap());
    pump(&mut link, Duration::from_secs(5), |l| l.pending() == 0);
    assert_eq!(link.portal(), Some(Mode::Light));
    portal.emit_unrelated();
    let outs = pump(&mut link, Duration::from_millis(500), |_| false);
    assert_eq!(outs, [], "contrast, or GNOME's own color-scheme: never ours");
    assert_eq!(link.portal(), Some(Mode::Light));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn no_portal_is_dark_with_nothing_pending() {
    let bus = PrivateBus::start("appearance-none");
    let mut link = portal_link(SessionBus::at(&bus.address).unwrap());
    let started = Instant::now();
    let outs = pump(&mut link, Duration::from_secs(5), |l| l.pending() == 0);
    assert!(started.elapsed() < TIMEOUT, "auto-start off: the bus answers ServiceUnknown at once, no 2 s wait");
    assert_eq!(outs, [], "no mode from an absent portal");
    assert_eq!(link.portal(), None);
    assert_eq!(effective_mode(ModeSetting::Auto, link.portal()), Mode::Dark);
    assert_eq!((link.pending(), link.next_deadline()), (0, None), "nothing pending, no timer");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_silent_portal_times_out_once_then_dark() {
    let bus = PrivateBus::start("appearance-silent");
    let silent = Peer::connect(&bus);
    silent.own(PORTAL_NAME); // owns the name, never reads its socket again
    let mut link = portal_link(SessionBus::at(&bus.address).unwrap());
    let started = Instant::now();
    let outs = pump(&mut link, Duration::from_secs(4), |l| l.pending() == 0);
    assert!(started.elapsed() >= TIMEOUT, "given up at its deadline, not before");
    assert_eq!(outs, []);
    assert_eq!((link.portal(), link.pending(), link.next_deadline()), (None, 0, None));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_peer_cannot_set_the_mode_with_a_unicast_signal() {
    use alpheus_demon::session_bus::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_PATH, SETTINGS_INTERFACE};
    let bus = PrivateBus::start("appearance-spoof");
    let session = SessionBus::at(&bus.address).unwrap();
    let ours = session.unique_name().to_string();
    let mut link = portal_link(session);
    pump(&mut link, Duration::from_secs(5), |l| l.pending() == 0); // no portal: answered at once
    let spoofer = Peer::connect(&bus);
    let mut spoof = dbus::Message::new_signal(PORTAL_PATH, SETTINGS_INTERFACE, "SettingChanged").unwrap().append3(
        APPEARANCE_NAMESPACE,
        COLOR_SCHEME_KEY,
        dbus::arg::Variant(2u32),
    );
    spoof.set_destination(Some(ours.as_str().into()));
    spoofer.channel.send(spoof).unwrap();
    spoofer.channel.flush();
    let outs = pump(&mut link, Duration::from_millis(500), |_| false);
    assert_eq!(outs, [], "addressed to us alone: not the portal's word");
    assert_eq!(link.portal(), None);
}
