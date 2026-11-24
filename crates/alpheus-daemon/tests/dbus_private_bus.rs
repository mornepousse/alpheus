//! DbusRsBus against a PRIVATE dbus-daemon started by the test, its socket
//! in a temporary directory: never the session bus, never the system bus.
//! `nix develop -c cargo test -p alpheus-daemon --test dbus_private_bus -- --ignored`

mod common;

use alpheus_daemon::sources::dbus::UnitsTap;
use alpheus_daemon::sources::dbus::bt::BtTap;
use alpheus_daemon::sources::dbus::dbusrs_bus::{DbusRsBus, WIFI_RULES};
use alpheus_daemon::sources::dbus::units::UnitsOptions;
use alpheus_daemon::sources::dbus::vpn::VpnTap;
use alpheus_daemon::sources::dbus::wifi::WifiTap;
use alpheus_daemon::sources::dbus::{Bus, DOMAINS, Domain, DomainLog, NM};
use alpheus_daemon::system::{
    ActiveVpn, BtState, NetState, PowerState, Profile, ProfileState, SavedWifi, Security, VpnKind, VpnScope, WifiScope,
};
use common::{
    FakeAp, FakeNetworkManager, FakePowerProfiles, FakeSystemd, FakeVpn, NmChange, Peer, PrivateBus, UUID_HOME, UUID_NEW, Workstation,
    process_alive, vpn_uuid,
};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, mpsc};
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
    let r = common::session_plain(
        &mut bus,
        &DOMAINS,
        &mut DomainLog::default(),
        &WifiTap::default(),
        &BtTap::default(),
        &VpnTap::default(),
        &mut |s| {
            first = Some(s);
            false
        },
    );
    assert!(r.is_ok());
    let s = first.expect("an initial state");
    assert_eq!((s.network, s.bluetooth, s.battery), (Some(NetState::Absent), Some(BtState::Absent), Some(PowerState::default())));
    assert_eq!(s.power, Some(ProfileState::default()), "no power-profiles-daemon: the row hidden");
}

/// Quick spec § 7: the profile is read at start, follows a `PropertiesChanged`, and an owner going
/// away hides the row — over the wire, through the real connection and its match rules.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_power_profile_is_read_followed_and_hidden_on_a_private_bus() {
    let b = PrivateBus::start("power");
    let daemon = FakePowerProfiles::serve(&b, "balanced");
    let mut bus = DbusRsBus::at(&b.address, &[Domain::Power]).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = common::session_plain(
            &mut bus,
            &[Domain::Power],
            &mut DomainLog::default(),
            &WifiTap::default(),
            &BtTap::default(),
            &VpnTap::default(),
            &mut |s| tx.send(s.power).is_ok(),
        );
    });
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a publication");
    let all = Profile::ALL.to_vec();
    assert_eq!(next(), Some(ProfileState { active: Some(Profile::Balanced), offered: all.clone() }), "read at start");
    daemon.set_active("performance");
    assert_eq!(next(), Some(ProfileState { active: Some(Profile::Performance), offered: all }), "followed");
    drop(daemon);
    assert_eq!(next(), Some(ProfileState::default()), "its owner gone: the row hides");
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

/// "UPower is the one service the daemon may start" (docs/decisions.md): the start call is well-formed on the wire (the daemon answers
/// ServiceUnknown, not InvalidArgs: nothing is activatable on this bus), and UPower appearing
/// afterwards brings the battery in through NameOwnerChanged.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn upower_absent_is_asked_for_once_and_the_battery_comes_in_when_it_appears() {
    let b = PrivateBus::start("upower-late");
    let mut probe = DbusRsBus::at(&b.address, &[Domain::Battery]).unwrap();
    let e = probe.start_upower().unwrap_err();
    assert!(e.to_string().contains("ServiceUnknown"), "{e}");
    drop(probe);
    let mut bus = DbusRsBus::at(&b.address, &[Domain::Battery]).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = common::session_plain(
            &mut bus,
            &[Domain::Battery],
            &mut DomainLog::default(),
            &WifiTap::default(),
            &BtTap::default(),
            &VpnTap::default(),
            &mut |s| tx.send(s.battery).is_ok(),
        );
    });
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a publication");
    assert_eq!(next(), Some(PowerState::default()), "nothing before UPower is there");
    let _workstation = Workstation::serve(&b);
    let laptop = (0..4).find_map(|_| next().and_then(|p| p.laptop));
    assert!(laptop.is_some(), "the battery appears with UPower");
}

/// The recorded workstation, over the wire: the same state as the unit
/// tests' `FakeBus::workstation()` (mod.rs, `workstation_network` and
/// `workstation_battery`), read through the real connection.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_recorded_workstation_reads_the_same_over_the_wire() {
    use alpheus_daemon::system::{Battery, Charge, Link, Peripheral, PeripheralKind};
    let b = PrivateBus::start("workstation");
    let _workstation = Workstation::serve(&b);
    let mut bus = DbusRsBus::at(&b.address, &DOMAINS).unwrap();
    let mut first = None;
    let r = common::session_plain(
        &mut bus,
        &DOMAINS,
        &mut DomainLog::default(),
        &WifiTap::default(),
        &BtTap::default(),
        &VpnTap::default(),
        &mut |s| {
            first = Some(s);
            false
        },
    );
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

/// `PrivateBus::spawn`'s failure path
/// (the readiness line never printed) must kill and wait on the child
/// before panicking, never leave it running. `fake-dbus-daemon.sh` stands
/// in for a dbus-daemon that never becomes ready, printing something else
/// and staying alive — unlike the real binary, which exits on its own on
/// every failure we could otherwise produce here, leaving nothing to prove
/// the fix against.
// No dbus-daemon involved (`fake-dbus-daemon.sh` stands in for it): this
// test needs no `#[ignore]`.
#[test]
fn a_failed_readiness_read_kills_and_waits_the_child_never_leaking_it() {
    let directory = std::env::temp_dir().join(format!("alpheus-spawn-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let fake = directory.join("fake-dbus-daemon.sh");
    // `fs::write` opens, writes and closes the file before `set_permissions`
    // and, further below, before the child ever execs it: no fd of ours
    // stays open for writing into it, so the exec never meets ETXTBSY.
    std::fs::write(&fake, "#!/bin/sh\necho not-the-real-address\nexec sleep 100\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let address = "unix:path=/nonexistent/whatever-address";
    let (pid, line) = PrivateBus::spawn_never_ready_for_test(fake.to_str().unwrap(), &directory, address).unwrap_err();
    assert!(!line.starts_with(address), "the fake never prints the real address: {line:?}");
    assert!(!process_alive(pid), "pid {pid}: the failed child must be killed, never leaked running");
    // Proves the WAIT, not just the kill: a
    // killed-but-never-waited child stays a zombie in /proc/<pid> (state
    // 'Z', which `process_alive` above already treats as "not alive") for
    // as long as this test binary runs — nobody else reaps it. Only `wait`
    // removes it from /proc entirely.
    let reaped = (0..100).any(|i| {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::metadata(format!("/proc/{pid}")).is_err()
    });
    assert!(reaped, "pid {pid}: still in /proc after the call returned — killed but never waited, a zombie leaked");
    let _ = std::fs::remove_dir_all(&directory);
}

/// The bus going away AFTER a peer
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
    // A second channel for `next_signal`'s own `io::ErrorKind`:
    // sent as itself, never wrapped in `Some(...)` — the
    // loop below only ever exits through its `Err` arm, so wrapping it in
    // an `Option` made the old `kind.is_some()` assertion unfalsifiable.
    let (tx2, rx2) = mpsc::channel();
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
        let _ = tx2.send((next_kind, t.elapsed()));
    });
    let call = silent.next_call().expect("the call reaches the peer");
    assert_eq!(call.member().as_deref(), Some("GetAll"));
    // The bus goes away with the call still unanswered: never replied.
    b.kill();
    let (kind, elapsed) = rx.recv_timeout(Duration::from_secs(5)).expect("the call must return");
    // Measured every run: killing the whole private bus mid-call makes
    // libdbus complete the pending call with `Disconnected`, not
    // `NoReply` (`call_error`'s unit test, dbusrs_bus.rs, pins down the
    // `NoReply` case this assertion alone cannot reach) — `UnexpectedEof`
    // either way, never an absence.
    assert_eq!(kind, Some(io::ErrorKind::UnexpectedEof), "a disconnect mid-call must be tagged UnexpectedEof: {kind:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    let (next_kind, _) = rx2.recv_timeout(Duration::from_secs(5)).expect("next_signal must also return");
    assert_ne!(next_kind, io::ErrorKind::NotFound, "next_signal after a disconnect must be an outage, not an absence: {next_kind:?}");
}

/// Quick Wi-Fi spec § 5, § 7: closed, nothing of the Wi-Fi is read; the loop's word wakes the thread
/// asleep on a quiet bus, which reads the row (past the wired device); the radio is followed; closed
/// again, the state goes. Over the wire, through the real connection and its poll.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_wifi_row_is_read_on_the_taps_word_and_followed_on_a_private_bus() {
    let b = PrivateBus::start("wifi-row");
    let nm = FakeNetworkManager::serve(&b, vec![FakeAp::new("Maison-Test", 80, true)], vec![(UUID_HOME.into(), "Maison-Test".into())]);
    nm.change(NmChange::Connect(Some((1, UUID_HOME.into()))));
    std::thread::sleep(Duration::from_millis(200)); // applied before the session's first read
    let tap = Arc::new(WifiTap::new().unwrap());
    let mut bus = DbusRsBus::at(&b.address, &[Domain::Network]).unwrap().waking(tap.clone());
    let (tx, rx) = mpsc::channel();
    let t = tap.clone();
    std::thread::spawn(move || {
        let _ = common::session_plain(
            &mut bus,
            &[Domain::Network],
            &mut DomainLog::default(),
            &t,
            &BtTap::default(),
            &VpnTap::default(),
            &mut |s| tx.send(s.wifi).is_ok(),
        );
    });
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a publication");
    assert_eq!(next(), None, "closed: no Wi-Fi read");
    assert!(!nm.calls().iter().any(|c| c.starts_with("GetDevices")), "{:?}", nm.calls());
    tap.set(WifiScope::Row);
    let row = next().expect("open: the row");
    assert!(row.enabled);
    let device = row.device.expect("the Wi-Fi device, past the wired one");
    assert_eq!((device.state, device.active.map(|a| a.ssid)), (100, Some(b"Maison-Test".to_vec())));
    assert_eq!(device.active_uuid.as_deref(), Some(UUID_HOME));
    assert_eq!((device.access_points, device.saved), (None, None), "the row reads no list");
    // The tap's byte was drained: the thread sleeps again, nothing more is asked of NetworkManager.
    let calls = nm.calls().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(nm.calls().len(), calls, "no re-read at rest: {:?}", &nm.calls()[calls..]);
    nm.change(NmChange::Radio(false));
    assert_eq!(next().map(|w| w.enabled), Some(false), "the radio followed");
    tap.set(WifiScope::Closed);
    assert_eq!(next(), None, "closed: the state goes");
}

/// Quick Wi-Fi spec § 5, § 7: the detail reads every access point and the saved profiles; a
/// strength change and a new profile are followed; back to the row, its rules are gone: the next
/// change reaches nothing, not even a call. No secret is ever asked.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_wifi_detail_is_read_followed_and_let_go_on_a_private_bus() {
    let b = PrivateBus::start("wifi-detail");
    let aps = vec![FakeAp::new("Maison-Test", 80, true), FakeAp::new("Bureau-Test", 60, true), FakeAp::new("Cafe-Libre", 40, false)];
    let nm = FakeNetworkManager::serve(&b, aps, vec![(UUID_HOME.into(), "Maison-Test".into())]);
    let tap = Arc::new(WifiTap::new().unwrap());
    let mut bus = DbusRsBus::at(&b.address, &[Domain::Network]).unwrap().waking(tap.clone());
    let (tx, rx) = mpsc::channel();
    let t = tap.clone();
    std::thread::spawn(move || {
        let _ = common::session_plain(
            &mut bus,
            &[Domain::Network],
            &mut DomainLog::default(),
            &t,
            &BtTap::default(),
            &VpnTap::default(),
            &mut |s| tx.send(s.wifi).is_ok(),
        );
    });
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a publication");
    let device = |w: Option<alpheus_daemon::system::WifiState>| w.and_then(|w| w.device).expect("the Wi-Fi device");
    assert_eq!(next(), None);
    tap.set(WifiScope::Detail);
    let d = device(next());
    let seen: Vec<(String, u8, Security)> =
        d.access_points.expect("listed").iter().map(|a| (String::from_utf8_lossy(&a.ssid).into_owned(), a.strength, a.security)).collect();
    assert_eq!(
        seen,
        [("Maison-Test".into(), 80, Security::Psk), ("Bureau-Test".into(), 60, Security::Psk), ("Cafe-Libre".into(), 40, Security::Open)]
    );
    assert_eq!(d.saved, Some(vec![SavedWifi { uuid: UUID_HOME.into(), ssid: b"Maison-Test".to_vec() }]));
    // The detail read, the thread sleeps again: nothing more is asked of NetworkManager at rest.
    let calls = nm.calls().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(nm.calls().len(), calls, "no re-read at rest: {:?}", &nm.calls()[calls..]);
    nm.change(NmChange::Strength(2, 20));
    let d = device(next());
    assert_eq!(d.access_points.unwrap()[1].strength, 20, "followed");
    nm.change(NmChange::AddSaved(UUID_NEW.into(), "Bureau-Test".into()));
    assert_eq!(device(next()).saved.map(|s| s.len()), Some(2), "a new profile followed");
    let rules = || detail_rules(&b);
    assert_eq!(rules(), 5, "the detail's five rules on the bus");
    tap.set(WifiScope::Row);
    let d = device(next());
    assert_eq!((d.access_points, d.saved), (None, None));
    assert_eq!(rules(), 0, "removed with the detail");
    let before = nm.calls().len();
    nm.change(NmChange::Strength(2, 70));
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "no publication: the rule is gone");
    assert_eq!(nm.calls().len(), before, "not even a call: {:?}", &nm.calls()[before..]);
    assert!(nm.calls().iter().all(|c| !c.contains("Secret")), "never a secret: {:?}", nm.calls());
}

/// How many of the Wi-Fi detail's rules the bus holds, every connection together (the bus driver's
/// own Debug.Stats, asked by a peer of the test).
fn detail_rules(b: &PrivateBus) -> usize {
    let peer = Peer::connect(b);
    let m = dbus::Message::new_method_call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus.Debug.Stats",
        "GetAllMatchRules",
    )
    .unwrap();
    let all: std::collections::HashMap<String, Vec<String>> = peer.call(m).expect("Debug.Stats on the private bus").read1().unwrap();
    // The driver prints a rule's parts in its own order: compared as sets (no value holds a comma).
    let parts = |r: &str| {
        let mut p: Vec<String> = r.split(',').map(String::from).collect();
        p.sort();
        p
    };
    let wanted: Vec<Vec<String>> = WIFI_RULES.iter().map(|r| parts(r)).collect();
    all.values().flatten().filter(|r| wanted.contains(&parts(r))).count()
}

/// Quick VPN spec § 5: closed, no VPN is read; the loop's word reads the two profiles and the
/// actives; an active connection's change is followed; closed again, the state goes and nothing more
/// is asked of NetworkManager. Over the wire, through the real connection and its poll.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_vpn_reading_on_a_private_bus() {
    let b = PrivateBus::start("vpn-row");
    let vpns = vec![
        FakeVpn { name: "Travail-Test".into(), wireguard: false, asks: true },
        FakeVpn { name: "Maison-Test".into(), wireguard: true, asks: false },
    ];
    let nm = FakeNetworkManager::serve_with(&b, vec![], vec![(UUID_HOME.into(), "Maison-Test".into())], vpns);
    let tap = Arc::new(VpnTap::new().unwrap());
    let mut bus = DbusRsBus::at(&b.address, &[Domain::Network]).unwrap().waking(tap.clone());
    let (tx, rx) = mpsc::channel();
    let t = tap.clone();
    std::thread::spawn(move || {
        let _ = common::session_plain(
            &mut bus,
            &[Domain::Network],
            &mut DomainLog::default(),
            &WifiTap::default(),
            &BtTap::default(),
            &t,
            &mut |s| tx.send(s.vpn).is_ok(),
        );
    });
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a publication");
    assert_eq!(next(), None, "closed: no VPN read");
    assert!(!nm.calls().iter().any(|c| c.starts_with("ListConnections")), "{:?}", nm.calls());
    tap.set(VpnScope::Row);
    let row = next().expect("open: the row");
    let seen: Vec<(&str, VpnKind, bool, u64)> = row.profiles.iter().map(|p| (p.name.as_str(), p.kind, p.asks, p.last_used)).collect();
    assert_eq!(seen, [("Travail-Test", VpnKind::Plugin, true, 1), ("Maison-Test", VpnKind::WireGuard, false, 2)]);
    assert_eq!(row.profiles[1].uuid, vpn_uuid(2));
    assert!(row.active.is_empty());
    // The tap's byte was drained: nothing more is asked at rest.
    let calls = nm.calls().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(nm.calls().len(), calls, "no re-read at rest: {:?}", &nm.calls()[calls..]);
    nm.change(NmChange::Vpn(1, Some(2)));
    let up = next().expect("followed");
    assert_eq!(up.active, [ActiveVpn { uuid: vpn_uuid(1), state: 2 }]);
    tap.set(VpnScope::Closed);
    assert_eq!(next(), None, "closed: the state goes");
    std::thread::sleep(Duration::from_millis(300));
    let calls = nm.calls().len();
    // One active connection remains (activating). The change is two signals (the root's
    // ActiveConnections, the active connection's State); the domain reads the root and that active
    // connection once for each. A VPN reader left open would read both again after each.
    nm.change(NmChange::Vpn(1, Some(1)));
    std::thread::sleep(Duration::from_millis(500));
    let since = nm.calls()[calls..].to_vec();
    let lists: Vec<&String> = since.iter().filter(|c| c.starts_with("ListConnections") || c.starts_with("GetSettings")).collect();
    assert!(lists.is_empty(), "closed: no VPN call after a further change: {lists:?}");
    let root = since.iter().filter(|c| c.as_str() == "GetAll /org/freedesktop/NetworkManager org.freedesktop.NetworkManager").count();
    let actives = since.iter().filter(|c| c.starts_with("GetAll ") && c.contains("/ActiveConnection/")).count();
    assert_eq!((root, actives), (2, 2), "closed: the domain's own reads only, one per signal: {since:?}");
    assert!(nm.calls().iter().all(|c| !c.starts_with("GetSecrets")));
}

// The failed-units count over the wire (diagnostics spec § 6): a fake systemd that, like the real
// one, sends the Manager's `PropertiesChanged` only while a client has called `Subscribe`.

/// A session over `domains` on the bus, publishing the count it sees; the tap that pokes it.
fn units_session(b: &PrivateBus, domains: Vec<Domain>, opts: UnitsOptions) -> (mpsc::Receiver<Option<u32>>, Arc<UnitsTap>) {
    let tap = Arc::new(UnitsTap::new().unwrap());
    let mut bus = DbusRsBus::at(&b.address, &domains).unwrap().waking(tap.clone());
    let (tx, rx) = mpsc::channel();
    let seen = tap.clone();
    std::thread::spawn(move || {
        let _ = alpheus_daemon::sources::dbus::session(
            &mut bus,
            &domains,
            &mut DomainLog::default(),
            &WifiTap::default(),
            &BtTap::default(),
            &VpnTap::default(),
            &seen,
            opts,
            &mut |s| tx.send(s.units).is_ok(),
            &mut || true,
        );
    });
    (rx, tap)
}

/// The next publication's count within `secs`.
fn next_units(rx: &mpsc::Receiver<Option<u32>>, secs: u64) -> Option<Option<u32>> {
    rx.recv_timeout(Duration::from_secs(secs)).ok()
}

/// Waits (5 s) for a publication whose count is `want`.
fn units_become(rx: &mpsc::Receiver<Option<u32>>, want: Option<u32>) {
    let until = Instant::now() + Duration::from_secs(5);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(n) if n == want => return,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    panic!("the count never became {want:?}");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn units_follow_a_subscribed_manager() {
    let b = PrivateBus::start("units-follow");
    let systemd = FakeSystemd::serve(&b, 0);
    let (rx, _tap) = units_session(&b, vec![Domain::Units], UnitsOptions { subscribe: true, ..UnitsOptions::default() });
    assert_eq!(next_units(&rx, 5), Some(Some(0)), "read at start");
    systemd.wait_until("the subscription", |s| s.subscribers() == 1);
    systemd.set_failed(2);
    assert_eq!(next_units(&rx, 1), Some(Some(2)), "followed within a second, through the one rule");
    systemd.set_failed(0);
    assert_eq!(next_units(&rx, 1), Some(Some(0)));
    assert_eq!(systemd.calls("GetAll"), 0, "the Manager's GetAll carries its Environment: never asked");
    assert_eq!(systemd.calls("Get "), systemd.calls("Get NFailedUnits"), "no other property was read");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn without_subscribe_a_change_is_seen_on_the_next_trigger() {
    let b = PrivateBus::start("units-unsubscribed");
    let systemd = FakeSystemd::serve(&b, 0);
    let power = FakePowerProfiles::serve(&b, "balanced");
    let opts = UnitsOptions { subscribe: false, piggyback: Duration::from_secs(1) };
    let started = Instant::now();
    let (rx, tap) = units_session(&b, vec![Domain::Power, Domain::Units], opts);
    assert_eq!(next_units(&rx, 5), Some(Some(0)));
    systemd.set_failed(2);
    // Nobody subscribed: systemd sends nothing, and nothing else wakes the thread.
    assert_eq!(next_units(&rx, 1).filter(|n| *n == Some(2)), None, "no signal without a subscriber");
    assert_eq!(systemd.subscribers(), 0);
    assert_eq!(systemd.calls("Subscribe"), 0, "subscribe = false");
    // Another domain wakes the thread after the interval: the count rides along.
    while started.elapsed() < Duration::from_millis(1200) {
        std::thread::sleep(Duration::from_millis(20));
    }
    power.set_active("performance");
    units_become(&rx, Some(2));
    // And a poke from the loop reads it at once.
    systemd.set_failed(4);
    assert_eq!(next_units(&rx, 1).filter(|n| *n == Some(4)), None, "still nothing by itself");
    tap.poke();
    units_become(&rx, Some(4));
}

/// A systemd that answers `Get` with an error leaves the count unknown and the session alive: the
/// other domains keep flowing (diagnostics spec § 6).
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_failing_systemd_leaves_the_other_domains_flowing() {
    let b = PrivateBus::start("units-failing");
    let systemd = FakeSystemd::serve(&b, 3);
    systemd.set_get_error(true);
    let power = FakePowerProfiles::serve(&b, "balanced");
    let domains = vec![Domain::Power, Domain::Units];
    let tap = Arc::new(UnitsTap::new().unwrap());
    let mut bus = DbusRsBus::at(&b.address, &domains).unwrap().waking(tap.clone());
    let (tx, rx) = mpsc::channel();
    let (end_tx, end_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = alpheus_daemon::sources::dbus::session(
            &mut bus,
            &domains,
            &mut DomainLog::default(),
            &WifiTap::default(),
            &BtTap::default(),
            &VpnTap::default(),
            &tap,
            UnitsOptions { subscribe: false, piggyback: Duration::from_secs(1) },
            &mut |s| tx.send(s).is_ok(),
            &mut || true,
        );
        let _ = end_tx.send(r.is_ok());
    });
    let first = rx.recv_timeout(Duration::from_secs(5)).expect("a first state");
    assert_eq!((first.units, first.power.is_some()), (None, true), "the count unknown, the profile read");
    power.set_active("performance");
    let until = Instant::now() + Duration::from_secs(5);
    let mut last = first.clone();
    while last.power == first.power {
        let left = until.checked_duration_since(Instant::now()).expect("the profile change was never published");
        last = rx.recv_timeout(left).expect("the profile change was never published");
    }
    assert_eq!(last.units, None);
    assert!(end_rx.try_recv().is_err(), "the session is still alive");
    // The count comes back once systemd answers again (a piggyback on the next profile change).
    systemd.set_get_error(false);
    std::thread::sleep(Duration::from_millis(1200));
    power.set_active("balanced");
    let until = Instant::now() + Duration::from_secs(5);
    while last.units != Some(3) {
        let left = until.checked_duration_since(Instant::now()).expect("the count never came back");
        last = rx.recv_timeout(left).expect("the count never came back");
    }
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_refused_subscribe_goes_on_and_a_poke_still_reads() {
    let b = PrivateBus::start("units-refused");
    let systemd = FakeSystemd::serve_refusing(&b, 1);
    let (rx, tap) = units_session(&b, vec![Domain::Units], UnitsOptions { subscribe: true, ..UnitsOptions::default() });
    assert_eq!(next_units(&rx, 5), Some(Some(1)));
    systemd.wait_until("the refused call", |s| s.calls("Subscribe") == 1);
    systemd.set_failed(3);
    tap.poke();
    units_become(&rx, Some(3));
    assert_eq!(systemd.calls("Subscribe"), 1, "refused once, not asked again for this owner");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn a_systemd_restart_resubscribes() {
    let b = PrivateBus::start("units-restart");
    let systemd = FakeSystemd::serve(&b, 0);
    let (rx, _tap) = units_session(&b, vec![Domain::Units], UnitsOptions { subscribe: true, ..UnitsOptions::default() });
    assert_eq!(next_units(&rx, 5), Some(Some(0)));
    systemd.wait_until("the first subscription", |s| s.subscribers() == 1);
    systemd.restart();
    systemd.wait_until("the subscription to the new owner", |s| s.calls("Subscribe") >= 2 && s.subscribers() == 1);
    systemd.set_failed(3);
    units_become(&rx, Some(3));
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn units_rules_name_their_service() {
    let b = PrivateBus::start("units-rules");
    let systemd = FakeSystemd::serve(&b, 0);
    let (rx, _tap) = units_session(&b, vec![Domain::Units], UnitsOptions { subscribe: true, ..UnitsOptions::default() });
    assert_eq!(next_units(&rx, 5), Some(Some(0)));
    systemd.wait_until("the subscription", |s| s.subscribers() == 1);
    let reads = systemd.calls("Get NFailedUnits");
    // The same signal from a client that is not systemd1's owner is never delivered: the rule names
    // its sender.
    let intruder = Peer::connect(&b);
    intruder.emit("/org/freedesktop/systemd1", "org.freedesktop.DBus.Properties", "PropertiesChanged", "org.freedesktop.systemd1.Manager");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(systemd.calls("Get NFailedUnits"), reads, "the daemon woke for a signal that is not systemd's");
    assert_eq!(next_units(&rx, 0), None);
}
