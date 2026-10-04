//! ZbusBus against a PRIVATE dbus-daemon started by the test, its socket in
//! a temporary directory: never the session bus, never the system bus.
//! `cargo test -p alpheus-demon --test dbus_private_bus -- --ignored` (nix develop).

mod common;

use alpheus_demon::sources::dbus::zbus_bus::ZbusBus;
use alpheus_demon::sources::dbus::{Bus, DOMAINS, Domain, NM, session};
use alpheus_demon::system::{BtState, NetState, PowerState};
use common::PrivateBus;
use std::io;
use std::sync::mpsc;
use std::time::Duration;

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn reads_and_absences_on_a_private_bus() {
    let b = PrivateBus::start("reads");
    let mut bus = ZbusBus::at(&b.address, &DOMAINS).unwrap();
    let driver = bus.owner("org.freedesktop.DBus").unwrap().expect("the bus driver owns its own name");
    assert_eq!(bus.owner(NM).unwrap(), None, "no NetworkManager on a private bus: None, not an error");
    let p = bus.get_all(&driver, "/org/freedesktop/DBus", "org.freedesktop.DBus").unwrap();
    assert!(p.0.contains_key("Features") && p.0.contains_key("Interfaces"), "{p:?}");
    // A unique name nobody holds (a service gone after GetNameOwner): NotFound.
    let e = bus.get_all(":1.999", "/org/freedesktop/NetworkManager", "org.freedesktop.NetworkManager").unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
    assert_eq!(bus.managed_objects(":1.999").unwrap_err().kind(), io::ErrorKind::NotFound);
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_service_appearing_is_heard_then_the_bus_going_away_ends_the_wait() {
    let mut b = PrivateBus::start("signals");
    let mut bus = ZbusBus::at(&b.address, &DOMAINS).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let r = bus.next_signal();
            let end = r.is_err();
            if tx.send(r.map(|s| s.domain())).is_err() || end {
                return;
            }
        }
    });
    // Another client takes NetworkManager's name: our NameOwnerChanged rule
    // lets it through (after the NameAcquired every new client receives).
    let other = zbus::blocking::connection::Builder::address(b.address.as_str()).unwrap().build().unwrap();
    other.request_name(NM).unwrap();
    let heard = (0..5).find_map(|_| rx.recv_timeout(Duration::from_secs(5)).expect("a signal").ok().flatten());
    assert_eq!(heard, Some(Domain::Network));
    b.kill();
    loop {
        if rx.recv_timeout(Duration::from_secs(5)).expect("the wait must end").is_err() {
            break;
        }
    }
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_simulated_resume_is_recognized_as_a_wake() {
    let b = PrivateBus::start("resume");
    let mut bus = ZbusBus::at(&b.address, &DOMAINS).unwrap();
    let other = zbus::blocking::connection::Builder::address(b.address.as_str()).unwrap().build().unwrap();
    // Our rule matches on sender='org.freedesktop.login1' (a well-known
    // name): the bus only resolves it against whoever currently owns that
    // name, so `other` must hold it for the signal below to match at all
    // (deviation from the brief's literal snippet, which hung here: nobody
    // owned the name, so the daemon never delivered the signal to `bus`).
    other.request_name("org.freedesktop.login1").unwrap();
    // Stands in for logind, which would send this to every client (survey:
    // nothing but a dbus-daemon runs on the private bus).
    other.emit_signal(None::<&str>, "/org/freedesktop/login1", "org.freedesktop.login1.Manager", "PrepareForSleep", &(false,)).unwrap();
    // `bus`'s own connection receives an unsolicited NameAcquired for its own
    // unique name first (same as the other test, `a_service_appearing…`):
    // filtered out here exactly as `session`'s main loop would (`continue`
    // on a signal that isn't a wake and points to no domain).
    let wake = (0..5).find_map(|_| bus.next_signal().ok().map(|s| s.is_wake()).filter(|&w| w));
    assert_eq!(wake, Some(true), "PrepareForSleep(false), heard through LOGIN1_RULE");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_session_on_an_empty_bus_hides_everything() {
    let b = PrivateBus::start("session");
    let mut bus = ZbusBus::at(&b.address, &DOMAINS).unwrap();
    let mut first = None;
    let r = session(&mut bus, &DOMAINS, &mut |s| {
        first = Some(s);
        false
    });
    assert!(r.is_ok());
    let s = first.expect("an initial state");
    assert_eq!((s.network, s.bluetooth, s.battery), (Some(NetState::Absent), Some(BtState::Absent), Some(PowerState::default())));
}
