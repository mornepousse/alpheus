//! The tray on a private session bus (`common::PrivateBus`, nothing
//! activatable): never the workstation's buses. Ignored by default:
//! `nice -n 19 nix develop -c env CARGO_BUILD_JOBS=4 cargo test -p alpheus-demon --test tray_private_bus -- --ignored`

mod common;

use alpheus_demon::tray::Prop;
use alpheus_demon::tray::bus::{Call, CallError, CallId, Incoming, Outbox, Property, SessionBus, TIMEOUT, session_address};
use common::{Peer, PrivateBus};
use dbus::arg::Variant;
use std::ffi::OsString;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

fn get(destination: &str, prop: Prop) -> Call {
    Call::Get { destination: destination.into(), path: "/StatusNotifierItem".into(), property: Property::Item(prop) }
}

/// Reads `bus` until `n` messages came or `timeout` passed.
fn read_until(bus: &mut SessionBus, n: usize, timeout: Duration) -> Vec<Incoming> {
    let end = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < end {
        out.extend(bus.read());
        thread::sleep(Duration::from_millis(5));
    }
    out
}

/// The answers among `incoming`: (id, call, the reply's string), errors as "error".
fn answers(incoming: Vec<Incoming>) -> Vec<(CallId, Call, String)> {
    incoming
        .into_iter()
        .filter_map(|i| match i {
            Incoming::Answer { id, call, result } => {
                let text = match result {
                    Ok(m) => m.read1::<Variant<String>>().map(|v| v.0).unwrap_or_else(|_| "error".into()),
                    Err(e) => format!("{e:?}"),
                };
                Some((id, call, text))
            }
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn calls_and_answers_matched_by_serial() {
    let bus = PrivateBus::start("tray-serial");
    let peer = Peer::connect(&bus);
    let app = peer.name.clone();
    let served = thread::spawn(move || {
        // Answers the second call first: the bus matches by reply serial, not by order.
        let first = peer.next_call().unwrap();
        let second = peer.next_call().unwrap();
        for call in [second, first] {
            let (_, name): (&str, &str) = call.read2().unwrap();
            let reply = call.method_return().append1(Variant(format!("value of {name}")));
            peer.channel.send(reply).unwrap();
        }
        peer.channel.flush();
        peer
    });
    let mut tray = SessionBus::at(&bus.address).unwrap();
    assert!(tray.unique_name().starts_with(":1."));
    let id = tray.call(get(&app, Prop::Id));
    let title = tray.call(get(&app, Prop::Title));
    tray.flush();
    assert_eq!(tray.pending(), 2);
    let got = answers(read_until(&mut tray, 2, Duration::from_secs(5)));
    let _peer = served.join().unwrap();
    assert_eq!(got, [(title, get(&app, Prop::Title), "value of Title".into()), (id, get(&app, Prop::Id), "value of Id".into())]);
    assert_eq!(tray.pending(), 0);
    assert_eq!(tray.next_deadline(), None);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_silent_peer_times_out_after_2_s_then_nothing_is_pending() {
    let bus = PrivateBus::start("tray-silent");
    let silent = Peer::connect(&bus); // never reads its socket
    let mut tray = SessionBus::at(&bus.address).unwrap();
    let sent = Instant::now();
    let id = tray.call(get(&silent.name, Prop::Title));
    tray.flush();
    let deadline = tray.next_deadline().expect("a pending call has a deadline");
    assert!(deadline >= sent + TIMEOUT && deadline <= Instant::now() + TIMEOUT);
    assert!(tray.expire(deadline - Duration::from_millis(1)).is_empty(), "not yet");
    assert_eq!(answers(read_until(&mut tray, 1, Duration::from_millis(500))), []);
    let expired = answers(tray.expire(deadline));
    assert_eq!(expired, [(id, get(&silent.name, Prop::Title), format!("{:?}", CallError::Timeout))], "due at its deadline exactly");
    assert_eq!((tray.pending(), tray.next_deadline()), (0, None), "nothing left: the timer can go");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_late_answer_is_dropped() {
    let bus = PrivateBus::start("tray-late");
    let peer = Peer::connect(&bus);
    let app = peer.name.clone();
    let mut tray = SessionBus::at(&bus.address).unwrap();
    tray.call(get(&app, Prop::Title));
    tray.flush();
    let late = peer.next_call().unwrap();
    let expired = tray.expire(Instant::now() + TIMEOUT);
    assert_eq!(expired.len(), 1, "timed out");
    peer.channel.send(late.method_return().append1(Variant("too late".to_string()))).unwrap();
    peer.channel.flush();
    let after = read_until(&mut tray, 1, Duration::from_millis(500));
    assert!(after.iter().all(|i| !matches!(i, Incoming::Answer { .. })), "the late answer is dropped: {after:?}");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_bus_going_away_is_closed_once() {
    let mut bus = PrivateBus::start("tray-closed");
    let silent = Peer::connect(&bus);
    let mut tray = SessionBus::at(&bus.address).unwrap();
    tray.call(get(&silent.name, Prop::Id));
    tray.flush();
    bus.kill();
    let got = read_until(&mut tray, 2, Duration::from_secs(5));
    let closed = got.iter().filter(|i| matches!(i, Incoming::Closed)).count();
    assert_eq!(closed, 1, "{got:?}");
    assert!(
        got.iter().any(|i| matches!(i, Incoming::Answer { result: Err(CallError::Disconnected), .. })),
        "the pending call ends as Disconnected: {got:?}"
    );
    assert_eq!(tray.pending(), 0);
    assert!(read_until(&mut tray, 1, Duration::from_millis(200)).is_empty(), "closed once, then silent");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_connection_is_close_on_exec() {
    use std::os::fd::AsRawFd;
    let bus = PrivateBus::start("tray-cloexec");
    let mut tray = SessionBus::at(&bus.address).unwrap();
    let fd = tray.watch_fd().unwrap();
    // SAFETY: fcntl(2) F_GETFD on an fd this test owns.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "a child the daemon starts must not inherit the session bus");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_xdg_runtime_bus_socket_is_found() {
    let bus = PrivateBus::start("tray-runtime");
    let socket = Path::new(bus.address.strip_prefix("unix:path=").unwrap()).to_path_buf();
    let runtime = socket.parent().unwrap().to_path_buf();
    assert_eq!(socket.file_name().unwrap(), "bus", "the private bus listens on <dir>/bus, like a session");
    let var = |k: &str| (k == "XDG_RUNTIME_DIR").then(|| OsString::from(runtime.as_os_str()));
    let address = session_address(var, |p| p.exists()).expect("the socket is there");
    assert_eq!(address, format!("unix:path={}", socket.display()));
    let tray = SessionBus::at(&address).unwrap();
    assert!(tray.unique_name().starts_with(":1."));
}
