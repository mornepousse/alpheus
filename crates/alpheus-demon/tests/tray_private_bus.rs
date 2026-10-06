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
fn a_call_after_the_bus_closed_is_disconnected_with_no_timer() {
    let mut bus = PrivateBus::start("tray-call-after-close");
    let silent = Peer::connect(&bus);
    let mut tray = SessionBus::at(&bus.address).unwrap();
    bus.kill();
    // Drain until the close is detected (sets the internal `closed` flag).
    let closed_batch = read_until(&mut tray, 1, Duration::from_secs(5));
    assert!(closed_batch.iter().any(|i| matches!(i, Incoming::Closed)), "{closed_batch:?}");
    assert_eq!(tray.pending(), 0);
    let id = tray.call(get(&silent.name, Prop::Title));
    assert_eq!(tray.pending(), 0, "a call on a closed bus is never pending: no deadline timer arms for it");
    assert_eq!(tray.next_deadline(), None);
    let got = answers(tray.read());
    assert_eq!(got, [(id, get(&silent.name, Prop::Title), format!("{:?}", CallError::Disconnected))]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
#[cfg_attr(debug_assertions, should_panic(expected = "send_message"))]
fn send_message_refuses_a_method_call() {
    let bus = PrivateBus::start("tray-send-message-guard");
    let mut tray = SessionBus::at(&bus.address).unwrap();
    let mut call = Message::new_method_call(":1.3", "/x", "org.example", "Do").unwrap();
    call.set_serial(9);
    tray.send_message(call);
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

// ---- The fakes themselves (task 3), driven by a plain Peer ----

use alpheus_demon::tray::{DEFAULT_ITEM_PATH, ITEM_INTERFACE, MENU_INTERFACE, WATCHER_INTERFACE, WATCHER_NAME, WATCHER_PATH};
use common::sni::{FakeItem, FakeMenu, FakeWatcher, Registration, Spec, Value};
use dbus::Message;

/// `method` on `destination` at `path`, waited for at most `timeout`.
fn ask(
    peer: &Peer,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    args: impl FnOnce(Message) -> Message,
    timeout: Duration,
) -> Result<Message, String> {
    let m = args(Message::new_method_call(destination, path, interface, member).unwrap());
    peer.channel.send_with_reply_and_block(m, timeout).map_err(|e| e.name().unwrap_or("").to_string())
}

fn property(peer: &Peer, destination: &str, path: &str, interface: &str, name: &str) -> Result<Message, String> {
    ask(peer, destination, path, "org.freedesktop.DBus.Properties", "Get", |m| m.append2(interface, name), Duration::from_secs(2))
}

/// The next signal named `member` the peer receives (it must have a match rule for it).
fn next_signal(peer: &Peer, member: &str, timeout: Duration) -> Option<Message> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if let Ok(Some(m)) = peer.channel.blocking_pop_message(Duration::from_millis(20))
            && m.msg_type() == dbus::message::MessageType::Signal
            && m.member().as_deref() == Some(member)
        {
            return Some(m);
        }
    }
    None
}

fn add_match(peer: &Peer, rule: &str) {
    let m = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch")
        .unwrap()
        .append1(rule);
    peer.call(m).unwrap();
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn fakes_answer_like_the_apps_they_model() {
    let bus = PrivateBus::start("fakes-answer");
    let peer = Peer::connect(&bus);
    let nimbus = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::kde(1) });
    let steam = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::steam() });
    let lumen = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::chromium() });
    let title = property(&peer, &nimbus.unique, "/StatusNotifierItem", ITEM_INTERFACE, "Title").unwrap();
    assert_eq!(title.read1::<Variant<String>>().unwrap().0, "Nimbus");
    let tooltip = property(&peer, &lumen.unique, "/StatusNotifierItem", ITEM_INTERFACE, "ToolTip").unwrap();
    assert_eq!(tooltip.read1::<Variant<(String, Vec<(i32, i32, Vec<u8>)>, String, String)>>().unwrap().0.2, "Lumen");
    let steam_path = "/org/ayatana/NotificationItem/steam";
    assert_eq!(
        property(&peer, &steam.unique, steam_path, ITEM_INTERFACE, "ToolTip").unwrap_err(),
        "org.freedesktop.DBus.Error.UnknownProperty"
    );
    assert_eq!(
        property(&peer, &lumen.unique, "/StatusNotifierItem", ITEM_INTERFACE, "IconName").unwrap_err(),
        "org.freedesktop.DBus.Error.Failed"
    );
    let activate = |item: &FakeItem, path: &str| {
        ask(&peer, &item.unique, path, ITEM_INTERFACE, "Activate", |m| m.append2(3i32, 4i32), Duration::from_secs(2))
    };
    assert_eq!(activate(&steam, steam_path).unwrap_err(), "org.freedesktop.DBus.Error.UnknownMethod", "Steam has no Activate");
    assert!(activate(&nimbus, "/StatusNotifierItem").is_ok());
    let introspect = ask(
        &peer,
        &lumen.unique,
        "/StatusNotifierItem",
        "org.freedesktop.DBus.Introspectable",
        "Introspect",
        |m| m,
        Duration::from_secs(2),
    );
    assert_eq!(introspect.unwrap_err(), "org.freedesktop.DBus.Error.UnknownMethod", "Chromium is not introspectable");
    assert_eq!(
        nimbus.calls(),
        ["org.freedesktop.DBus.Properties.Get(org.kde.StatusNotifierItem, Title)", "org.kde.StatusNotifierItem.Activate(3, 4)"]
    );
    assert!(steam.wait_call("Activate", Duration::from_secs(1)).is_some(), "refused calls are recorded too");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_silent_fake_never_answers() {
    let bus = PrivateBus::start("fakes-silent");
    let _watcher = FakeWatcher::start(&bus);
    let peer = Peer::connect(&bus);
    let silent = FakeItem::start(&bus, Spec::silent());
    let started = Instant::now();
    let r = ask(
        &peer,
        &silent.unique,
        "/StatusNotifierItem",
        "org.freedesktop.DBus.Properties",
        "Get",
        |m| m.append2(ITEM_INTERFACE, "Title"),
        Duration::from_millis(300),
    );
    assert_eq!(r.unwrap_err(), "org.freedesktop.DBus.Error.NoReply");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(silent.calls().is_empty(), "it never even read the call");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_fake_menu_serves_lazy_layouts_and_records_events() {
    let bus = PrivateBus::start("fakes-menu");
    let peer = Peer::connect(&bus);
    add_match(&peer, &format!("type='signal',interface='{MENU_INTERFACE}'"));
    let lumen = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::chromium() });
    let menu = "/org/chromium/DbusMenu";
    let layout = |parent: i32| {
        let r = ask(
            &peer,
            &lumen.unique,
            menu,
            MENU_INTERFACE,
            "GetLayout",
            |m| m.append3(parent, 1i32, vec!["label"]),
            Duration::from_secs(2),
        )
        .unwrap();
        let mut it = r.iter_init();
        it.next();
        let mut root = it.recurse(dbus::arg::ArgType::Struct).unwrap();
        let id: i32 = root.read().unwrap();
        root.next();
        let mut children = root.recurse(dbus::arg::ArgType::Array).unwrap();
        let mut n = 0;
        while children.arg_type() == dbus::arg::ArgType::Variant {
            n += 1;
            children.next();
        }
        (id, n)
    };
    assert_eq!(layout(0), (0, 0), "lazy: nothing before AboutToShow");
    let about =
        |id: i32| ask(&peer, &lumen.unique, menu, MENU_INTERFACE, "AboutToShow", |m| m.append1(id), Duration::from_secs(2)).unwrap();
    assert!(about(0).read1::<bool>().unwrap(), "needUpdate");
    let updated = next_signal(&peer, "LayoutUpdated", Duration::from_secs(2)).expect("LayoutUpdated follows");
    assert_eq!(updated.read2::<u32, i32>().unwrap().1, 0);
    assert_eq!(layout(0), (0, 8), "built now");
    assert!(!about(0).read1::<bool>().unwrap(), "built once");
    assert_eq!(layout(8), (8, 2), "the submenu's two children, on demand");
    let mut event = Message::new_method_call(lumen.unique.as_str(), menu, MENU_INTERFACE, "Event")
        .unwrap()
        .append3(9i32, "clicked", Variant(0i32))
        .append1(77u32);
    event.set_no_reply(true);
    peer.channel.send(event).unwrap();
    peer.channel.flush();
    assert_eq!(lumen.wait_call("Event", Duration::from_secs(2)).as_deref(), Some("com.canonical.dbusmenu.Event(9, clicked, 0, 77)"));
    let kettle = FakeItem::start(
        &bus,
        Spec { registration: Registration::Never, menu: Some(FakeMenu { answers_event: true, ..FakeMenu::sample() }), ..Spec::qt() },
    );
    let answered = ask(
        &peer,
        &kettle.unique,
        "/MenuBar",
        MENU_INTERFACE,
        "Event",
        |m| m.append3(1i32, "clicked", Variant(0i32)).append1(1u32),
        Duration::from_secs(2),
    );
    assert!(answered.is_ok(), "this one answers Event");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_fake_watcher_lists_and_signals() {
    let bus = PrivateBus::start("fakes-watcher");
    let watcher = FakeWatcher::start(&bus);
    let peer = Peer::connect(&bus);
    add_match(&peer, &format!("type='signal',interface='{WATCHER_INTERFACE}'"));
    let is_host = || {
        property(&peer, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE, "IsStatusNotifierHostRegistered")
            .unwrap()
            .read1::<Variant<bool>>()
            .unwrap()
            .0
    };
    assert!(!is_host());
    let lumen = FakeItem::start(&bus, Spec::chromium());
    assert!(watcher.items().is_empty(), "Chromium gives up without a host");
    ask(
        &peer,
        WATCHER_NAME,
        WATCHER_PATH,
        WATCHER_INTERFACE,
        "RegisterStatusNotifierHost",
        |m| m.append1(peer.name.as_str()),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(next_signal(&peer, "StatusNotifierHostRegistered", Duration::from_secs(2)).is_some());
    assert_eq!(watcher.hosts(), vec![peer.name.clone()]);
    assert!(is_host());
    // lumen never calls register() again (that would be a no-op once it has given up, per its
    // doc comment): it retries on its own, because it watches for this very signal, same as
    // StatusNotifierHostRegistered just did for this test's own peer.
    let deadline = Instant::now() + Duration::from_secs(2);
    while watcher.items().is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!watcher.items().is_empty(), "lumen must retry on the host-registered signal alone");
    let nimbus = FakeItem::start(&bus, Spec::kde(1));
    let quill = FakeItem::start(&bus, Spec::ayatana());
    let expected = [
        format!("{}/StatusNotifierItem", lumen.unique),
        "org.kde.StatusNotifierItem-4242-1/StatusNotifierItem".to_string(),
        format!("{}/org/ayatana/NotificationItem/quill", quill.unique),
    ];
    assert_eq!(watcher.items(), expected);
    let list = property(&peer, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE, "RegisteredStatusNotifierItems").unwrap();
    assert_eq!(list.read1::<Variant<Vec<String>>>().unwrap().0, expected);
    let registered = next_signal(&peer, "StatusNotifierItemRegistered", Duration::from_secs(2)).unwrap();
    assert_eq!(registered.read1::<&str>().unwrap(), expected[0]);
    assert_eq!(
        property(&peer, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE, "ProtocolVersion").unwrap().read1::<Variant<i32>>().unwrap().0,
        0
    );
    nimbus.kill();
    let unregistered = next_signal(&peer, "StatusNotifierItemUnregistered", Duration::from_secs(2)).expect("its owner left");
    assert_eq!(unregistered.read1::<&str>().unwrap(), expected[1]);
    assert_eq!(watcher.items(), [expected[0].clone(), expected[2].clone()]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_killed_fake_emits_name_owner_changed() {
    let bus = PrivateBus::start("fakes-killed");
    let peer = Peer::connect(&bus);
    add_match(&peer, "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged'");
    let nimbus = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::kde(2) });
    let unique = nimbus.unique.clone();
    nimbus.set("Title", Value::Str("Nimbus 2".into()));
    nimbus.kill();
    let mut gone = Vec::new();
    while let Some(m) = next_signal(&peer, "NameOwnerChanged", Duration::from_secs(1)) {
        let (name, old, new): (String, String, String) = m.read3().unwrap();
        if new.is_empty() {
            gone.push((name, old));
        }
    }
    gone.sort();
    assert_eq!(gone, [(unique.clone(), unique.clone()), ("org.kde.StatusNotifierItem-4242-2".to_string(), unique)]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_registration_never_silent_fake_never_reads_at_all() {
    let bus = PrivateBus::start("fakes-silent-never");
    let peer = Peer::connect(&bus);
    let silent = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::silent() });
    let started = Instant::now();
    let r = ask(
        &peer,
        &silent.unique,
        "/StatusNotifierItem",
        "org.freedesktop.DBus.Properties",
        "Get",
        |m| m.append2(ITEM_INTERFACE, "Title"),
        Duration::from_millis(300),
    );
    assert_eq!(r.unwrap_err(), "org.freedesktop.DBus.Error.NoReply");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(silent.calls().is_empty(), "silent + Registration::Never: not even the first call is ever read");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn kde_checks_protocol_version_before_registering() {
    let bus = PrivateBus::start("fakes-kde-protocol");
    let peer = Peer::connect(&bus);
    peer.own(WATCHER_NAME);
    let served = thread::spawn(move || {
        let call = peer.next_call().unwrap();
        assert_eq!((call.interface().as_deref(), call.member().as_deref()), (Some("org.freedesktop.DBus.Properties"), Some("Get")));
        let (_, name): (&str, &str) = call.read2().unwrap();
        assert_eq!(name, "ProtocolVersion");
        peer.channel.send(call.method_return().append1(Variant(1i32))).unwrap();
        peer.channel.flush();
        // A real KDE item gives up silently once ProtocolVersion is not 0: nothing else must ever arrive.
        peer.channel.blocking_pop_message(Duration::from_millis(300)).unwrap()
    });
    let _nimbus = FakeItem::start(&bus, Spec::kde(1));
    let extra = served.join().unwrap();
    assert!(extra.is_none(), "no RegisterStatusNotifierItem after a non-zero ProtocolVersion: {extra:?}");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn chromiums_signals_are_doubled_on_the_freedesktop_interface() {
    let bus = PrivateBus::start("fakes-chromium-double");
    let peer = Peer::connect(&bus);
    add_match(&peer, "type='signal'");
    let lumen = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::chromium() });
    lumen.emit("NewIcon");
    let kde_side = next_signal(&peer, "NewIcon", Duration::from_secs(1)).expect("the kde-interface copy");
    assert_eq!(kde_side.interface().as_deref(), Some(ITEM_INTERFACE));
    let freedesktop_side = next_signal(&peer, "NewIcon", Duration::from_secs(1)).expect("the freedesktop-interface copy");
    assert_eq!(freedesktop_side.interface().as_deref(), Some("org.freedesktop.StatusNotifierItem"));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn chromiums_register_is_a_no_op_after_a_failed_precheck() {
    let bus = PrivateBus::start("fakes-chromium-no-op");
    let peer = Peer::connect(&bus);
    peer.own(WATCHER_NAME);
    let served = thread::spawn(move || {
        let call = peer.next_call().unwrap();
        assert_eq!(call.member().as_deref(), Some("Get"));
        peer.channel.send(call.method_return().append1(Variant(false))).unwrap();
        peer.channel.flush();
        // A second, explicit register() call must not touch the bus again: nothing else arrives.
        peer.channel.blocking_pop_message(Duration::from_millis(300)).unwrap()
    });
    let lumen = FakeItem::start(&bus, Spec::chromium());
    lumen.register();
    let extra = served.join().unwrap();
    assert!(extra.is_none(), "a repeated register() after a failed pre-check must be a no-op: {extra:?}");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_fake_watcher_refuses_an_ownerless_registration() {
    let bus = PrivateBus::start("fakes-watcher-ownerless");
    let watcher = FakeWatcher::start(&bus);
    let peer = Peer::connect(&bus);
    let r = ask(
        &peer,
        WATCHER_NAME,
        WATCHER_PATH,
        WATCHER_INTERFACE,
        "RegisterStatusNotifierItem",
        |m| m.append1("org.example.NoSuchOwner"),
        Duration::from_secs(2),
    );
    assert!(r.is_err(), "a name nobody owns must be refused: {r:?}");
    assert!(watcher.items().is_empty());
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_fake_watcher_does_not_re_announce_a_duplicate_registration() {
    let bus = PrivateBus::start("fakes-watcher-dup");
    let watcher = FakeWatcher::start(&bus);
    let peer = Peer::connect(&bus);
    add_match(&peer, &format!("type='signal',interface='{WATCHER_INTERFACE}'"));
    let register = || {
        ask(
            &peer,
            WATCHER_NAME,
            WATCHER_PATH,
            WATCHER_INTERFACE,
            "RegisterStatusNotifierItem",
            |m| m.append1(peer.name.as_str()),
            Duration::from_secs(2),
        )
        .unwrap()
    };
    register();
    let first = next_signal(&peer, "StatusNotifierItemRegistered", Duration::from_secs(2));
    assert!(first.is_some());
    register();
    let second = next_signal(&peer, "StatusNotifierItemRegistered", Duration::from_millis(300));
    assert!(second.is_none(), "a duplicate registration is acknowledged, never announced twice: {second:?}");
    assert_eq!(watcher.items().len(), 1);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_fake_watcher_drops_a_departed_host() {
    let bus = PrivateBus::start("fakes-watcher-host-departs");
    let watcher = FakeWatcher::start(&bus);
    let peer = Peer::connect(&bus);
    ask(
        &peer,
        WATCHER_NAME,
        WATCHER_PATH,
        WATCHER_INTERFACE,
        "RegisterStatusNotifierHost",
        |m| m.append1(peer.name.as_str()),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(watcher.hosts(), vec![peer.name.clone()]);
    drop(peer);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !watcher.hosts().is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(watcher.hosts().is_empty(), "the departed host must be dropped");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_fake_watcher_answers_get_all() {
    let bus = PrivateBus::start("fakes-watcher-getall");
    let watcher = FakeWatcher::start(&bus);
    let peer = Peer::connect(&bus);
    let _nimbus = FakeItem::start(&bus, Spec::kde(1));
    let r = ask(
        &peer,
        WATCHER_NAME,
        WATCHER_PATH,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        |m| m.append1(WATCHER_INTERFACE),
        Duration::from_secs(2),
    )
    .unwrap();
    let mut it = r.iter_init();
    let mut dict = it.recurse(dbus::arg::ArgType::Array).unwrap();
    let (mut items, mut is_host, mut version) = (None, None, None);
    while let Some(mut entry) = dict.recurse(dbus::arg::ArgType::DictEntry) {
        let key: &str = entry.read().unwrap();
        let mut v = entry.recurse(dbus::arg::ArgType::Variant).unwrap();
        match key {
            "RegisteredStatusNotifierItems" => items = v.get::<Vec<String>>(),
            "IsStatusNotifierHostRegistered" => is_host = v.get::<bool>(),
            "ProtocolVersion" => version = v.get::<i32>(),
            _ => {}
        }
        dict.next();
    }
    assert_eq!(items, Some(vec![format!("org.kde.StatusNotifierItem-4242-1{DEFAULT_ITEM_PATH}")]));
    assert_eq!(is_host, Some(false));
    assert_eq!(version, Some(0));
    assert_eq!(watcher.items().len(), 1, "sanity: matches the plain Get-based assertions used elsewhere");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn an_unknown_path_answers_unknown_object() {
    let bus = PrivateBus::start("fakes-unknown-path");
    let peer = Peer::connect(&bus);
    let nimbus = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::kde(1) });
    let r = ask(&peer, &nimbus.unique, "/NoSuchObject", ITEM_INTERFACE, "Activate", |m| m.append2(1i32, 2i32), Duration::from_secs(2));
    assert_eq!(r.unwrap_err(), "org.freedesktop.DBus.Error.UnknownObject");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn an_icon_pixmap_value_is_available_though_never_read() {
    let bus = PrivateBus::start("fakes-iconpixmap");
    let peer = Peer::connect(&bus);
    let kettle = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::qt() });
    let r = property(&peer, &kettle.unique, "/StatusNotifierItem", ITEM_INTERFACE, "IconPixmap").unwrap();
    let mut it = r.iter_init();
    let mut v = it.recurse(dbus::arg::ArgType::Variant).unwrap();
    assert_eq!(v.signature().to_string(), "a(iiay)");
}

// ---- The watcher and the router (task 5) ----

use alpheus_demon::tray::ItemKey;
use alpheus_demon::tray::host::Item;
use alpheus_demon::tray::router::{Role, Tray, TrayOutput};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The router on a thread of its own, pumped like the bar's loop pumps it: a poll on its fd, bounded
/// by its next deadline; every non-empty output kept for the test.
struct Pump {
    tray: Arc<Mutex<Tray>>,
    outputs: Arc<Mutex<Vec<TrayOutput>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Pump {
    fn start(bus: &PrivateBus) -> Pump {
        let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), std::path::PathBuf::from("/proc"));
        let fd = tray.watch_fd().unwrap();
        let tray = Arc::new(Mutex::new(tray));
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (t, o, s) = (tray.clone(), outputs.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                let wait = t.lock().unwrap().next_deadline().map(|d| d.saturating_duration_since(Instant::now()));
                let ms = wait.map(|w| w.as_millis().min(20) as i32).unwrap_or(20);
                let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: poll(2) on one pollfd this thread owns.
                unsafe { libc::poll(&mut p, 1, ms) };
                let mut tray = t.lock().unwrap();
                let mut out = tray.on_readable();
                if tray.next_deadline().is_some_and(|d| d <= Instant::now()) {
                    out.merge(tray.on_deadline(Instant::now()));
                }
                if out != TrayOutput::default() {
                    let closed = out.bus_closed;
                    o.lock().unwrap().push(out);
                    if closed {
                        return;
                    }
                }
            }
        });
        Pump { tray, outputs, stop, thread: Some(thread) }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Tray) -> R) -> R {
        f(&mut self.tray.lock().unwrap())
    }

    /// Waits until `cond` holds (5 s at most), or panics with `what`.
    fn until(&self, what: &str, cond: impl Fn(&Tray) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !self.with(|t| cond(t)) {
            assert!(Instant::now() < end, "never: {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn ready(&self) -> Vec<Item> {
        self.with(|t| t.items().iter().filter(|i| i.is_ready()).cloned().collect())
    }

    fn redraws(&self) -> usize {
        self.outputs.lock().unwrap().iter().filter(|o| o.redraw).count()
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn ready_count(n: usize) -> impl Fn(&Tray) -> bool {
    move |t: &Tray| t.items().iter().filter(|i| i.is_ready()).count() == n
}

fn item_key(owner: &str, path: &str) -> ItemKey {
    ItemKey { owner: owner.into(), path: path.into() }
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_watcher_serves_what_real_clients_check() {
    let bus = PrivateBus::start("router-serves");
    let pump = Pump::start(&bus);
    pump.until("the watcher's name is ours", |t| *t.role() == Role::Watcher);
    let peer = Peer::connect(&bus);
    let is_host = property(&peer, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE, "IsStatusNotifierHostRegistered").unwrap();
    assert!(is_host.read1::<Variant<bool>>().unwrap().0);
    let version = property(&peer, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE, "ProtocolVersion").unwrap();
    assert_eq!(version.read1::<Variant<i32>>().unwrap().0, 0);
    let set = ask(
        &peer,
        WATCHER_NAME,
        WATCHER_PATH,
        "org.freedesktop.DBus.Properties",
        "Set",
        |m| m.append3(WATCHER_INTERFACE, "ProtocolVersion", Variant(1i32)),
        Duration::from_secs(2),
    );
    assert_eq!(set.unwrap_err(), "org.freedesktop.DBus.Error.PropertyReadOnly");
    let bogus = ask(&peer, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE, "Bogus", |m| m, Duration::from_secs(2));
    assert_eq!(bogus.unwrap_err(), "org.freedesktop.DBus.Error.UnknownMethod");
    let xml =
        ask(&peer, WATCHER_NAME, WATCHER_PATH, "org.freedesktop.DBus.Introspectable", "Introspect", |m| m, Duration::from_secs(2)).unwrap();
    assert!(xml.read1::<&str>().unwrap().contains("RegisterStatusNotifierItem"));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn chromium_registers_only_when_a_host_is_registered() {
    let bus = PrivateBus::start("router-chromium");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let lumen = FakeItem::start(&bus, Spec::chromium());
    pump.until("Lumen shown", ready_count(1));
    let item = &pump.ready()[0];
    assert_eq!(item.key, item_key(&lumen.unique, "/StatusNotifierItem"));
    assert_eq!((item.id.as_deref(), item.title.as_deref(), item.icon_name.as_deref()), (Some("chrome_status_icon_1"), None, None));
    assert_eq!(
        (item.tooltip.as_deref(), item.menu.as_deref()),
        (Some("Lumen"), Some("/org/chromium/DbusMenu")),
        "its 300 KB pixmap never decoded"
    );
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn registration_forms_and_duplicates_over_the_wire() {
    let bus = PrivateBus::start("router-forms");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let nimbus = FakeItem::start(&bus, Spec::kde(1));
    let quill = FakeItem::start(&bus, Spec::ayatana());
    let ferry = FakeItem::start(&bus, Spec::combined());
    let kettle = FakeItem::start(&bus, Spec::qt());
    nimbus.register();
    kettle.register();
    pump.until("four items", ready_count(4));
    thread::sleep(Duration::from_millis(100));
    // Sorted: a well-known name is keyed only once its owner is known, so the order of arrival is not
    // the order of registration.
    let mut keys: Vec<ItemKey> = pump.with(|t| t.items().iter().map(|i| i.key.clone()).collect());
    keys.sort_by(|a, b| (&a.owner, &a.path).cmp(&(&b.owner, &b.path)));
    let mut expected = [
        item_key(&nimbus.unique, "/StatusNotifierItem"),
        item_key(&quill.unique, "/org/ayatana/NotificationItem/quill"),
        item_key(&ferry.unique, "/StatusNotifierItem"),
        item_key(&kettle.unique, "/StatusNotifierItem"),
    ];
    expected.sort_by(|a, b| (&a.owner, &a.path).cmp(&(&b.owner, &b.path)));
    assert_eq!(keys, expected, "keyed by unique name, duplicates are one");
    let peer = Peer::connect(&bus);
    let list = property(&peer, WATCHER_NAME, WATCHER_PATH, WATCHER_INTERFACE, "RegisteredStatusNotifierItems").unwrap();
    let mut list = list.read1::<Variant<Vec<String>>>().unwrap().0;
    list.sort();
    let mut expected = [
        "org.kde.StatusNotifierItem-4242-1/StatusNotifierItem".to_string(),
        format!("{}/org/ayatana/NotificationItem/quill", quill.unique),
        "org.kde.StatusNotifierItem-4343-1/StatusNotifierItem".to_string(),
        format!("{}/StatusNotifierItem", kettle.unique),
    ];
    expected.sort();
    assert_eq!(list, expected);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_steam_like_item_without_properties_is_shown() {
    let bus = PrivateBus::start("router-steam");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let _steam = FakeItem::start(&bus, Spec::steam());
    pump.until("Steam shown", ready_count(1));
    let item = &pump.ready()[0];
    assert_eq!((item.title.as_deref(), item.tooltip.as_deref(), item.item_is_menu), (Some("Steam"), None, false));
    assert_eq!(item.menu.as_deref(), Some("/org/ayatana/NotificationItem/steam/Menu"));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn an_item_leaving_is_removed_and_unregistered() {
    let bus = PrivateBus::start("router-leaving");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let peer = Peer::connect(&bus);
    add_match(&peer, &format!("type='signal',interface='{WATCHER_INTERFACE}'"));
    let nimbus = FakeItem::start(&bus, Spec::kde(1));
    pump.until("Nimbus shown", ready_count(1));
    let entry = "org.kde.StatusNotifierItem-4242-1/StatusNotifierItem";
    assert_eq!(next_signal(&peer, "StatusNotifierItemRegistered", Duration::from_secs(2)).unwrap().read1::<&str>().unwrap(), entry);
    nimbus.kill();
    pump.until("Nimbus gone", |t| t.items().is_empty());
    assert_eq!(next_signal(&peer, "StatusNotifierItemUnregistered", Duration::from_secs(2)).unwrap().read1::<&str>().unwrap(), entry);
    assert!(pump.redraws() >= 2, "shown, then gone");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn host_only_when_the_name_is_taken_then_takeover() {
    let bus = PrivateBus::start("router-takeover");
    let other = FakeWatcher::start(&bus);
    let nimbus = FakeItem::start(&bus, Spec::kde(1));
    let pump = Pump::start(&bus);
    pump.until("host only", |t| matches!(t.role(), Role::HostOnly { watcher } if *watcher == other.unique));
    pump.until("Nimbus, from the other watcher's list", ready_count(1));
    let quill = FakeItem::start(&bus, Spec::ayatana());
    pump.until("Quill, from the other watcher's signal", ready_count(2));
    assert_eq!(other.hosts().len(), 1, "registered as a host on it");
    let peer = Peer::connect(&bus);
    let owner = |peer: &Peer| {
        let m = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner")
            .unwrap()
            .append1(WATCHER_NAME);
        peer.call(m).unwrap().read1::<String>().unwrap()
    };
    assert_eq!(owner(&peer), other.unique, "never taken from its owner, even one that allows it");
    let host = other.hosts()[0].clone();
    let refused =
        ask(&peer, &host, WATCHER_PATH, WATCHER_INTERFACE, "RegisterStatusNotifierItem", |m| m.append1(":1.99"), Duration::from_secs(2));
    assert_eq!(refused.unwrap_err(), "org.freedesktop.DBus.Error.UnknownMethod", "not serving while host only");
    other.kill();
    pump.until("takeover", |t| *t.role() == Role::Watcher);
    assert_eq!(owner(&peer), host, "the name is ours now");
    nimbus.register();
    quill.register();
    let kettle = FakeItem::start(&bus, Spec::qt());
    pump.until("Kettle registered with us", ready_count(3));
    assert!(pump.with(|t| t.item(&item_key(&kettle.unique, "/StatusNotifierItem")).is_some()));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn host_only_follows_a_watcher_replacing_another() {
    let bus = PrivateBus::start("router-replaced");
    let first = FakeWatcher::start(&bus);
    let pump = Pump::start(&bus);
    pump.until("host only on the first", |t| matches!(t.role(), Role::HostOnly { watcher } if *watcher == first.unique));
    let listed = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::kde(3) });
    let signalled = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::kde(4) });
    // A second watcher takes the name straight from the first (which allows it): one NameOwnerChanged
    // (name, first, second), never an empty owner in between.
    let second = Peer::connect(&bus);
    let take = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName")
        .unwrap()
        .append2(WATCHER_NAME, 2u32 | 4); // REPLACE_EXISTING | DO_NOT_QUEUE
    assert_eq!(second.call(take).unwrap().read1::<u32>().unwrap(), 1, "the second watcher owns the name");
    pump.until("host only on the second", |t| matches!(t.role(), Role::HostOnly { watcher } if *watcher == second.name));
    // The second watcher, by hand: we register as a host on it and read its list.
    let entry = "org.kde.StatusNotifierItem-4242-3/StatusNotifierItem";
    let (mut host, mut list) = (false, false);
    let end = Instant::now() + Duration::from_secs(3);
    while !(host && list) {
        assert!(Instant::now() < end, "never asked by the daemon: host {host}, list {list}");
        let Ok(Some(m)) = second.channel.blocking_pop_message(Duration::from_millis(20)) else { continue };
        if m.msg_type() != dbus::message::MessageType::MethodCall {
            continue;
        }
        match m.member().as_deref() {
            Some("RegisterStatusNotifierHost") => {
                host = true;
                second.channel.send(m.method_return()).unwrap();
            }
            Some("Get") => {
                list = true;
                second.channel.send(m.method_return().append1(Variant(vec![entry.to_string()]))).unwrap();
            }
            _ => {}
        }
        second.channel.flush();
    }
    pump.until("the item of the second watcher's list", ready_count(1));
    assert_eq!(pump.ready()[0].key, item_key(&listed.unique, "/StatusNotifierItem"));
    let registered = Message::new_signal(WATCHER_PATH, WATCHER_INTERFACE, "StatusNotifierItemRegistered")
        .unwrap()
        .append1("org.kde.StatusNotifierItem-4242-4/StatusNotifierItem");
    second.channel.send(registered).unwrap();
    second.channel.flush();
    pump.until("the item of the second watcher's signal", ready_count(2));
    assert!(pump.with(|t| t.item(&item_key(&signalled.unique, "/StatusNotifierItem")).is_some()));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn an_item_started_before_the_bar_is_found_by_the_inventory() {
    let bus = PrivateBus::start("router-inventory");
    let nimbus = FakeItem::start(&bus, Spec { registration: Registration::Never, ..Spec::kde(3) });
    let pump = Pump::start(&bus);
    pump.until("found at startup", ready_count(1));
    assert_eq!(pump.ready()[0].key, item_key(&nimbus.unique, "/StatusNotifierItem"));
    assert_eq!(pump.ready()[0].service, "org.kde.StatusNotifierItem-4242-3");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_burst_of_signals_gives_one_reread_and_one_redraw() {
    let bus = PrivateBus::start("router-burst");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let nimbus = FakeItem::start(&bus, Spec::kde(1));
    pump.until("shown", ready_count(1));
    let reads = |f: &FakeItem| f.calls().iter().filter(|c| c.ends_with("Get(org.kde.StatusNotifierItem, IconName)")).count();
    assert_eq!(reads(&nimbus), 1);
    let redraws = pump.redraws();
    nimbus.set("IconName", Value::Str("nimbus-busy".into()));
    nimbus.burst("NewIcon", 50);
    pump.until("marked dirty", |t| t.wants_frame());
    thread::sleep(Duration::from_millis(200));
    pump.with(|t| t.on_frame());
    pump.until("re-read", |t| t.items()[0].icon_name.as_deref() == Some("nimbus-busy"));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(reads(&nimbus), 2, "50 signals, one Get");
    assert_eq!(pump.redraws(), redraws + 1, "one redraw");
    assert!(!pump.with(|t| t.wants_frame()));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_65th_fake_is_refused_and_a_hostile_title_is_cut() {
    let bus = PrivateBus::start("router-bounds");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let hostile = FakeItem::start(&bus, Spec::hostile());
    let mut fakes: Vec<FakeItem> = (0..63).map(|_| FakeItem::start(&bus, Spec::qt())).collect();
    pump.until("64 items", ready_count(64));
    let last = FakeItem::start(&bus, Spec::qt());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(pump.with(|t| t.items().len()), 64, "the 65th refused");
    assert!(last.calls().is_empty(), "nothing asked of it");
    let title = pump.with(|t| t.item(&item_key(&hostile.unique, "/StatusNotifierItem")).unwrap().title.clone().unwrap());
    assert_eq!(title.chars().count(), 256);
    assert!(title.chars().all(|c| !c.is_control()), "{title:?}");
    fakes.clear();
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_silent_item_gets_its_letter_after_2_s() {
    let bus = PrivateBus::start("router-silent");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let started = Instant::now();
    let _silent = FakeItem::start(&bus, Spec::silent());
    pump.until("registered", |t| t.items().len() == 1);
    thread::sleep(Duration::from_millis(1500).saturating_sub(started.elapsed()));
    assert!(pump.ready().is_empty(), "not shown before its Gets time out");
    pump.until("its letter, once its Gets time out", ready_count(1));
    let item = &pump.ready()[0];
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_eq!((item.title.as_deref(), item.id.as_deref()), (None, None), "its letter: nothing known");
    assert_eq!(pump.with(|t| t.next_deadline()), None, "nothing pending: no timer");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn nothing_outside_the_allowlist_reaches_a_fake_app() {
    let bus = PrivateBus::start("router-allowlist");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let fakes =
        [Spec::kde(1), Spec::qt(), Spec::chromium(), Spec::steam(), Spec::ayatana(), Spec::combined()].map(|s| FakeItem::start(&bus, s));
    pump.until("all shown", ready_count(fakes.len()));
    for f in &fakes {
        f.burst("NewToolTip", 3);
        f.emit_status("NeedsAttention");
    }
    thread::sleep(Duration::from_millis(200));
    pump.with(|t| t.on_frame());
    thread::sleep(Duration::from_millis(200));
    let allowed = [
        "org.freedesktop.DBus.Properties.Get(",
        "org.kde.StatusNotifierItem.Activate(",
        "org.kde.StatusNotifierItem.SecondaryActivate(",
        "org.kde.StatusNotifierItem.ContextMenu(",
        "org.kde.StatusNotifierItem.Scroll(",
        "com.canonical.dbusmenu.AboutToShow(",
        "com.canonical.dbusmenu.GetLayout(",
        "com.canonical.dbusmenu.Event(",
    ];
    for f in &fakes {
        let calls = f.calls();
        assert!(calls.len() >= 9, "{calls:?}");
        for c in calls {
            assert!(allowed.iter().any(|a| c.starts_with(a)), "outside the allowlist: {c}");
            assert!(!c.contains("GetAll") && !c.contains("Introspect"), "{c}");
        }
    }
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_registration_naming_our_own_unique_name_is_refused() {
    // Task 4 review: the router must never treat its own connection as one of the items it hosts
    // (a hostile or buggy RegisterStatusNotifierItem naming the daemon's own unique name).
    let bus = PrivateBus::start("router-self-registration");
    let pump = Pump::start(&bus);
    pump.until("watcher", |t| *t.role() == Role::Watcher);
    let peer = Peer::connect(&bus);
    let owner = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner")
        .unwrap()
        .append1(WATCHER_NAME);
    let own = peer.call(owner).unwrap().read1::<String>().unwrap();
    let reply = ask(
        &peer,
        WATCHER_NAME,
        WATCHER_PATH,
        WATCHER_INTERFACE,
        "RegisterStatusNotifierItem",
        |m| m.append1(own.as_str()),
        Duration::from_secs(2),
    );
    assert!(reply.is_ok(), "the watcher still answers at once, even to a hostile registration");
    // Deterministic, not a race: a registration naming a unique name is added to the host
    // synchronously, before any Get it triggers leaves the socket (`route`'s `flush` comes after
    // `inbound_call`'s `host.register`); the registration's reply itself is only flushed then too.
    // So by the time this blocking `ask` has its reply in hand, an unrefused registration would
    // already be in `items()` — this must never be observed, with or without a timing margin.
    assert!(pump.with(|t| t.items().is_empty()), "never an item under our own unique name, right after the reply");
    thread::sleep(Duration::from_millis(200));
    assert!(pump.with(|t| t.items().is_empty()), "still never, once any self-directed Get would have settled");
    let nimbus = FakeItem::start(&bus, Spec::kde(1));
    pump.until("the router still works afterwards", ready_count(1));
    assert_eq!(pump.ready()[0].key, item_key(&nimbus.unique, "/StatusNotifierItem"));
}
