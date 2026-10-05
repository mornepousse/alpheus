//! DbusRsBus against a PRIVATE dbus-daemon started by the test, its socket
//! in a temporary directory: never the session bus, never the system bus.
//! `nix develop -c cargo test -p alpheus-demon --test dbus_private_bus -- --ignored`

mod common;

use alpheus_demon::sources::dbus::dbusrs_bus::DbusRsBus;
use alpheus_demon::sources::dbus::{Bus, DOMAINS, Domain, DomainLog, NM, session};
use alpheus_demon::system::{BtState, NetState, PowerState};
use common::{Peer, PrivateBus, Workstation, process_alive};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn reads_and_absences_on_a_private_bus() {
    let b = PrivateBus::start("reads");
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let driver = bus.owner("org.freedesktop.DBus").unwrap().expect("the bus driver owns its own name");
    assert_eq!(bus.owner(NM).unwrap(), None, "no NetworkManager on a private bus: None, not an error");
    let p = bus.get_all(&driver, "/org/freedesktop/DBus", "org.freedesktop.DBus").unwrap();
    assert!(p.0.contains_key("Features") && p.0.contains_key("Interfaces"), "{p:?}");
    let e = bus.get_all(":1.999", "/org/freedesktop/NetworkManager", "org.freedesktop.NetworkManager").unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
    assert_eq!(bus.managed_objects(":1.999").unwrap_err().kind(), io::ErrorKind::NotFound);
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_service_appearing_is_heard_then_the_bus_going_away_ends_the_wait() {
    let mut b = PrivateBus::start("signals");
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
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
    let other = Peer::connect(&b);
    other.own(NM);
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
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(s) = bus.next_signal() {
            if tx.send(s.is_wake()).is_err() {
                return;
            }
        }
    });
    let other = Peer::connect(&b);
    // LOGIN1_RULE matches sender='org.freedesktop.login1': the bus resolves
    // it against the name's current owner, so the peer must hold it.
    other.own("org.freedesktop.login1");
    other.emit("/org/freedesktop/login1", "org.freedesktop.login1.Manager", "PrepareForSleep", false);
    // The NameAcquired every new client receives comes first: skipped as
    // `session` would (not a wake). Nothing within 5 s: the rule is broken.
    let wake = loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(true) => break true,
            Ok(false) => continue,
            Err(_) => break false,
        }
    };
    assert!(wake, "PrepareForSleep(false), heard through LOGIN1_RULE");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn nothing_can_be_activated_on_the_private_bus() {
    let b = PrivateBus::start("no-activation");
    let peer = Peer::connect(&b);
    let m = dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ListActivatableNames")
        .unwrap();
    let names: Vec<String> = peer.call(m).unwrap().read1().unwrap();
    assert_eq!(names, ["org.freedesktop.DBus"], "a service directory leaked into the test bus's configuration");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_session_on_an_empty_bus_hides_everything() {
    let b = PrivateBus::start("session");
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let mut first = None;
    let r = session(&mut bus, &DOMAINS, &mut DomainLog::default(), &mut |s| {
        first = Some(s);
        false
    });
    assert!(r.is_ok());
    let s = first.expect("an initial state");
    assert_eq!((s.network, s.bluetooth, s.battery), (Some(NetState::Absent), Some(BtState::Absent), Some(PowerState::default())));
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_silent_service_is_an_outage_after_the_timeout() {
    let b = PrivateBus::start("silent");
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let silent = Peer::connect(&b); // connected, never reads: no reply ever comes
    let (tx, rx) = mpsc::channel();
    let name = silent.name.clone();
    std::thread::spawn(move || {
        let t = Instant::now();
        let r = bus.get_all(&name, "/", "org.example.Nothing");
        let _ = tx.send((r.map(|_| ()), t.elapsed()));
    });
    let (r, elapsed) = rx.recv_timeout(Duration::from_secs(5)).expect("a call never waits more than its timeout");
    let e = r.unwrap_err();
    assert_ne!(e.kind(), io::ErrorKind::NotFound, "a timeout is an outage, not an absence: {e}");
    assert!(elapsed >= Duration::from_millis(1900), "{elapsed:?}");
    drop(silent);
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_connection_is_close_on_exec() {
    let b = PrivateBus::start("cloexec");
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let fd = bus.socket_fd();
    // SAFETY: fcntl(2) F_GETFD only reads a descriptor's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "fd {fd}: a child would inherit the system bus connection");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_call_to_the_bar_is_refused_never_queued() {
    let b = PrivateBus::start("refused");
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let me = bus.unique_name().to_string();
    std::thread::spawn(move || while bus.next_signal().is_ok() {});
    let caller = Peer::connect(&b);
    let call = dbus::Message::new_method_call(me.as_str(), "/", "org.freedesktop.DBus.Peer", "Ping").unwrap();
    // Answered at once with an error, never left waiting (`Peer::call`
    // would time out after 5 s).
    let e = caller.call(call).unwrap_err();
    assert_eq!(e.name(), Some("org.freedesktop.DBus.Error.UnknownMethod"), "{e:?}");
}

/// The recorded workstation, over the wire: the same state as the unit
/// tests' `FakeBus::workstation()` (mod.rs, `workstation_network` and
/// `workstation_battery`), read through the real connection.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_recorded_workstation_reads_the_same_over_the_wire() {
    use alpheus_demon::system::{Battery, Charge, Link, Peripheral, PeripheralKind};
    let b = PrivateBus::start("workstation");
    let _workstation = Workstation::serve(&b);
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let mut first = None;
    let r = session(&mut bus, &DOMAINS, &mut DomainLog::default(), &mut |s| {
        first = Some(s);
        false
    });
    assert!(r.is_ok(), "{r:?}");
    let s = first.expect("an initial state");
    assert_eq!(s.network, Some(NetState::Online { link: Link::Ethernet, vpns: 2, limited: false }));
    assert_eq!(s.bluetooth, Some(BtState::On { devices: vec![] }));
    assert_eq!(
        s.battery,
        Some(PowerState {
            laptop: Some(Battery { percent: 92, charge: Charge::Charging }),
            peripherals: vec![Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: None, low: false }],
        })
    );
}

/// I3 carry-over (tray plan 1 review): `PrivateBus::spawn`'s failure path
/// (the readiness line never printed) must kill and wait on the child
/// before panicking, never leave it running. `fake-dbus-daemon.sh` stands
/// in for a dbus-daemon that never becomes ready, printing something else
/// and staying alive — unlike the real binary, which exits on its own on
/// every failure we could otherwise produce here, leaving nothing to prove
/// the fix against.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_failed_readiness_read_kills_and_waits_the_child_never_leaking_it() {
    let directory = std::env::temp_dir().join(format!("alpheus-spawn-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let fake = directory.join("fake-dbus-daemon.sh");
    std::fs::write(&fake, "#!/bin/sh\necho not-the-real-address\nexec sleep 100\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let address = "unix:path=/nonexistent/whatever-address";
    let (pid, line) = PrivateBus::spawn_never_ready_for_test(fake.to_str().unwrap(), &directory, address).unwrap_err();
    assert!(!line.starts_with(address), "the fake never prints the real address: {line:?}");
    assert!(!process_alive(pid), "pid {pid}: the failed child must be killed and reaped, never leaked");
    let _ = std::fs::remove_dir_all(&directory);
}

/// I4 carry-over (tray plan 1 review): the bus going away AFTER a peer
/// received a call but BEFORE it replied must end that call as an outage,
/// never an absence, and quickly — then leave `next_signal` unable to read
/// anything further either.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_disconnect_mid_call_is_an_outage_never_an_absence() {
    let mut b = PrivateBus::start("mid-call");
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let silent = Peer::connect(&b); // receives the call, never replies
    let name = silent.name.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t = Instant::now();
        let call_result = bus.get_all(&name, "/", "org.example.Nothing");
        let _ = tx.send((call_result.err().map(|e| e.kind()), t.elapsed()));
        // A signal already queued before the disconnect (NameAcquired, sent
        // to every new connection) may still come first as an ordinary Ok:
        // drain until the connection's own end is reached.
        let next_kind = loop {
            match bus.next_signal() {
                Ok(_) => continue,
                Err(e) => break e.kind(),
            }
        };
        let _ = tx.send((Some(next_kind), t.elapsed()));
    });
    let call = silent.next_call().expect("the call reaches the peer");
    assert_eq!(call.member().as_deref(), Some("GetAll"));
    // The bus goes away with the call still unanswered: never replied.
    b.kill();
    let (kind, elapsed) = rx.recv_timeout(Duration::from_secs(5)).expect("the call must return");
    assert_ne!(kind, Some(io::ErrorKind::NotFound), "a disconnect mid-call is an outage, not an absence: {kind:?}");
    assert!(kind.is_some(), "the call must fail, not silently succeed");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    let (next_kind, _) = rx.recv_timeout(Duration::from_secs(5)).expect("next_signal must also return");
    assert!(next_kind.is_some(), "next_signal must be Err once the bus is gone");
}
