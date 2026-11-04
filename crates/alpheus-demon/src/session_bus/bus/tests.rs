use super::testing::FakeOutbox;
use super::*;
use dbus::arg::ArgType;

fn app(method: Method) -> Call {
    let (destination, path) = (":1.7".to_string(), "/StatusNotifierItem".to_string());
    match method {
        Method::Get => Call::Get { destination, path, property: Property::Item(Prop::Title) },
        Method::Activate => Call::Activate { destination, path, x: 10, y: 20 },
        Method::SecondaryActivate => Call::SecondaryActivate { destination, path, x: 10, y: 20 },
        Method::ContextMenu => Call::ContextMenu { destination, path, x: 10, y: 20 },
        Method::Scroll => Call::Scroll { destination, path, delta: -120, orientation: Orientation::Horizontal },
        Method::AboutToShow => Call::AboutToShow { destination, path, id: 3 },
        Method::GetLayout => Call::GetLayout { destination, path, parent: 3 },
        Method::Event => Call::Event { destination, path, id: 5, timestamp: 77 },
        Method::RequestName => Call::RequestName,
        Method::GetNameOwner => Call::GetNameOwner { name: "org.kde.StatusNotifierItem-1-1".into() },
        Method::GetConnectionUnixProcessID => Call::GetConnectionUnixProcessID { name: ":1.7".into() },
        Method::ListNames => Call::ListNames,
        Method::AddMatch => Call::AddMatch { rule: "type='signal'".into() },
        Method::RemoveMatch => Call::RemoveMatch { rule: "type='signal'".into() },
        Method::RegisterStatusNotifierHost => Call::RegisterStatusNotifierHost { destination, host: ":1.3".into() },
        Method::ReadOne => Call::ReadOne,
        Method::GetAll => Call::PlayerGetAll { dest: destination },
        Method::PlayPause => Call::PlayerControl { dest: destination, member: Control::PlayPause },
        Method::Next => Call::PlayerControl { dest: destination, member: Control::Next },
        Method::Previous => Call::PlayerControl { dest: destination, member: Control::Previous },
    }
}

/// The signature of every argument of `m`, in order.
fn signatures(m: &Message) -> Vec<String> {
    let mut it = m.iter_init();
    let mut out = Vec::new();
    while it.arg_type() != ArgType::Invalid {
        out.push(it.signature().to_string());
        it.next();
    }
    out
}

#[test]
fn read_one_reads_one_setting_of_the_portal() {
    use crate::session_bus::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_NAME, PORTAL_PATH, SETTINGS_INTERFACE};
    let m = message(&Call::ReadOne);
    assert_eq!(
        (m.destination().as_deref(), m.path().as_deref(), m.interface().as_deref(), m.member().as_deref()),
        (Some(PORTAL_NAME), Some(PORTAL_PATH), Some(SETTINGS_INTERFACE), Some("ReadOne"))
    );
    assert_eq!(signatures(&m), ["s", "s"]);
    assert_eq!(m.read2::<&str, &str>().unwrap(), (APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY));
    assert!(!m.get_auto_start(), "never starts the portal");
    assert!(!m.get_no_reply(), "its answer is read");
}

#[test]
fn every_tray_call_is_no_auto_start() {
    for method in Method::ALL {
        let call = app(method);
        assert_eq!(call.method(), method);
        let m = message(&call);
        assert!(!m.get_auto_start(), "{method:?}");
        assert_eq!(m.member().as_deref(), Some(method.name()), "{method:?}");
        assert_eq!(m.interface().as_deref(), Some(method.interface()), "{method:?}");
    }
}

#[test]
fn message_types_every_argument() {
    let get = message(&app(Method::Get));
    assert_eq!((get.destination().as_deref(), get.path().as_deref()), (Some(":1.7"), Some("/StatusNotifierItem")));
    assert_eq!(get.read2::<&str, &str>().unwrap(), (ITEM_INTERFACE, "Title"));
    let other = message(&Call::Get { destination: ":1.9".into(), path: WATCHER_PATH.into(), property: Property::RegisteredItems });
    assert_eq!(other.read2::<&str, &str>().unwrap(), (WATCHER_INTERFACE, "RegisteredStatusNotifierItems"));
    assert_eq!(message(&app(Method::Activate)).read2::<i32, i32>().unwrap(), (10, 20));
    assert_eq!(signatures(&message(&app(Method::SecondaryActivate))), ["i", "i"]);
    assert_eq!(signatures(&message(&app(Method::ContextMenu))), ["i", "i"]);
    assert_eq!(message(&app(Method::Scroll)).read2::<i32, &str>().unwrap(), (-120, "horizontal"));
    assert_eq!(message(&app(Method::AboutToShow)).read1::<i32>().unwrap(), 3);
    let layout = message(&app(Method::GetLayout));
    assert_eq!(signatures(&layout), ["i", "i", "as"]);
    assert_eq!(layout.read3::<i32, i32, Vec<String>>().unwrap(), (3, 1, MENU_PROPERTIES.map(String::from).to_vec()));
    let event = message(&app(Method::Event));
    assert_eq!(signatures(&event), ["i", "s", "v", "u"]);
    let (id, kind, data, timestamp) = event.read4::<i32, &str, Variant<i32>, u32>().unwrap();
    assert_eq!((id, kind, data.0, timestamp), (5, "clicked", 0, 77));
    let request = message(&Call::RequestName);
    assert_eq!((request.destination().as_deref(), request.path().as_deref()), (Some(DBUS), Some(DBUS_PATH)));
    assert_eq!(request.read2::<&str, u32>().unwrap(), (WATCHER_NAME, 4));
    assert_eq!(message(&app(Method::GetNameOwner)).read1::<&str>().unwrap(), "org.kde.StatusNotifierItem-1-1");
    assert_eq!(message(&app(Method::GetConnectionUnixProcessID)).read1::<&str>().unwrap(), ":1.7");
    assert!(signatures(&message(&Call::ListNames)).is_empty());
    assert_eq!(message(&app(Method::AddMatch)).read1::<&str>().unwrap(), "type='signal'");
    assert_eq!(message(&app(Method::RemoveMatch)).member().as_deref(), Some("RemoveMatch"));
    let host = message(&app(Method::RegisterStatusNotifierHost));
    assert_eq!((host.destination().as_deref(), host.path().as_deref()), (Some(":1.7"), Some(WATCHER_PATH)));
    assert_eq!(host.read1::<&str>().unwrap(), ":1.3");
}

/// Every MPRIS call goes to the player's unique name and MPRIS object, never starting anything.
#[test]
fn player_calls_name_the_mpris_object_and_never_start_a_player() {
    let dest = || ":1.42".to_string();
    let head = |m: &Message| {
        (
            m.destination().map(|d| d.to_string()),
            m.path().map(|p| p.to_string()),
            m.interface().map(|i| i.to_string()),
            m.member().map(|x| x.to_string()),
        )
    };
    let some = |s: &str| Some(s.to_string());
    let get_all = message(&Call::PlayerGetAll { dest: dest() });
    assert_eq!(head(&get_all), (some(":1.42"), some(MPRIS_PATH), some(PROPERTIES_INTERFACE), some("GetAll")));
    assert_eq!(signatures(&get_all), ["s"]);
    assert_eq!(get_all.read1::<&str>().unwrap(), MPRIS_PLAYER_INTERFACE);
    for (call, interface, property) in [
        (Call::PlayerIdentity { dest: dest() }, MPRIS_INTERFACE, "Identity"),
        (Call::PlayerMetadata { dest: dest() }, MPRIS_PLAYER_INTERFACE, "Metadata"),
        (Call::PlayerPosition { dest: dest() }, MPRIS_PLAYER_INTERFACE, "Position"),
    ] {
        let m = message(&call);
        assert_eq!(head(&m), (some(":1.42"), some(MPRIS_PATH), some(PROPERTIES_INTERFACE), some("Get")), "{call:?}");
        assert_eq!(signatures(&m), ["s", "s"], "{call:?}");
        assert_eq!(m.read2::<&str, &str>().unwrap(), (interface, property), "{call:?}");
        assert!(!m.get_auto_start() && !m.get_no_reply(), "{call:?}: its answer is read");
    }
    for (member, name) in [(Control::PlayPause, "PlayPause"), (Control::Next, "Next"), (Control::Previous, "Previous")] {
        let m = message(&Call::PlayerControl { dest: dest(), member });
        assert_eq!(head(&m), (some(":1.42"), some(MPRIS_PATH), some(MPRIS_PLAYER_INTERFACE), some(name)));
        assert!(signatures(&m).is_empty(), "{name}: no argument");
        assert!(!m.get_auto_start(), "{name}: never starts a player");
    }
    assert!(!get_all.get_auto_start());
}

#[test]
fn controls_are_sent_and_reads_are_called() {
    let mut out = FakeOutbox::default();
    out.send(Call::PlayerControl { dest: ":1.42".into(), member: Control::Next });
    let id = out.call(Call::PlayerGetAll { dest: ":1.42".into() });
    assert_eq!(out.sent, [Call::PlayerControl { dest: ":1.42".into(), member: Control::Next }], "no pending entry, no deadline");
    assert_eq!(out.ids(Method::GetAll), [id]);
    assert_eq!(Call::PlayerPosition { dest: ":1.42".into() }.method(), Method::Get, "Identity, Metadata and Position are Gets");
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "unique name")]
fn a_player_call_to_its_well_known_name_is_a_bug() {
    // The name may change hands between two calls; and a well-known name could start a player.
    message(&Call::PlayerGetAll { dest: "org.mpris.MediaPlayer2.spotify".into() });
}

#[test]
fn event_expects_no_reply() {
    for method in Method::ALL {
        assert_eq!(message(&app(method)).get_no_reply(), method == Method::Event, "{method:?}");
    }
}

#[test]
fn request_name_only_ever_asks_the_watcher_name_without_replacing() {
    let (name, flags) = message(&Call::RequestName).read2::<String, u32>().unwrap();
    assert_eq!(name, WATCHER_NAME);
    assert_eq!(flags, DBUS_NAME_FLAG_DO_NOT_QUEUE);
    assert_eq!(flags & 2, 0, "never REPLACE_EXISTING");
    assert_eq!(flags & 1, 0, "never ALLOW_REPLACEMENT: an owner may not take it from us either");
}

/// Queued behind mako, never taking the name, never letting it be taken (spec § 1).
#[test]
fn the_notifications_name_is_requested_queued_never_replacing() {
    let m = message(&Call::RequestNotifications);
    assert_eq!(
        (m.destination().as_deref(), m.path().as_deref(), m.interface().as_deref(), m.member().as_deref()),
        (Some(DBUS), Some(DBUS_PATH), Some(DBUS), Some("RequestName"))
    );
    assert_eq!(signatures(&m), ["s", "u"]);
    let (name, flags) = m.read2::<String, u32>().unwrap();
    assert_eq!((name.as_str(), flags), (NOTIFICATIONS_NAME, NOTIFICATIONS_NAME_FLAGS));
    assert_eq!(flags, 0);
    assert_eq!(flags & DBUS_NAME_FLAG_DO_NOT_QUEUE, 0, "queued behind the owner, never refused");
    assert_eq!(flags & 2, 0, "never REPLACE_EXISTING");
    assert_eq!(flags & 1, 0, "never ALLOW_REPLACEMENT");
    assert!(!m.get_auto_start() && !m.get_no_reply(), "its answer is read");
    assert_eq!(Call::RequestNotifications.method(), Method::RequestName, "the same member: a second allowed name");
    let (watcher, _) = message(&Call::RequestName).read2::<String, u32>().unwrap();
    assert_eq!(watcher, WATCHER_NAME, "the watcher's request is unchanged");
}

fn notify_call() -> Message {
    let mut call = Message::new_method_call(":1.1", NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, "Notify").unwrap();
    call.set_serial(21); // as received: libdbus aborts on a reply to serial 0
    call
}

#[test]
fn the_server_replies_with_exact_signatures() {
    let call = notify_call();
    let check = |m: &Message, want: &[&str]| {
        assert_eq!(m.msg_type(), MessageType::MethodReturn);
        assert_eq!(m.get_reply_serial(), Some(21));
        assert_eq!(signatures(m), want);
    };
    let r = notify_reply(&call, 7);
    check(&r, &["u"]);
    assert_eq!(r.read1::<u32>().unwrap(), 7);
    let r = capabilities_reply(&call);
    check(&r, &["as"]);
    assert_eq!(r.read1::<Vec<String>>().unwrap(), CAPABILITIES);
    let r = server_info_reply(&call);
    check(&r, &["s", "s", "s", "s"]);
    assert_eq!(r.read4::<&str, &str, &str, &str>().unwrap(), ("alpheus", "alpheus", env!("CARGO_PKG_VERSION"), "1.3"));
    check(&empty_reply(&call), &[]);
}

#[test]
fn closed_and_invoked_are_broadcast_signals() {
    let head = |m: &Message| {
        (m.msg_type(), m.path().map(|p| p.to_string()), m.interface().map(|i| i.to_string()), m.member().map(|x| x.to_string()))
    };
    let some = |s: &str| Some(s.to_string());
    let closed = closed_signal(7, 2).unwrap();
    assert_eq!(head(&closed), (MessageType::Signal, some(NOTIFICATIONS_PATH), some(NOTIFICATIONS_INTERFACE), some("NotificationClosed")));
    assert!(closed.destination().is_none(), "broadcast, as mako does");
    assert_eq!(signatures(&closed), ["u", "u"]);
    assert_eq!(closed.read2::<u32, u32>().unwrap(), (7, 2));
    let invoked = invoked_signal(7, "default").unwrap();
    assert_eq!(head(&invoked), (MessageType::Signal, some(NOTIFICATIONS_PATH), some(NOTIFICATIONS_INTERFACE), some("ActionInvoked")));
    assert!(invoked.destination().is_none());
    assert_eq!(signatures(&invoked), ["u", "s"]);
    assert_eq!(invoked.read2::<u32, &str>().unwrap(), (7, "default"));
}

#[test]
fn fake_outbox_records_calls_in_order() {
    let mut out = FakeOutbox::default();
    let a = out.call(Call::ListNames);
    let b = out.call(app(Method::Get));
    out.send(app(Method::Event));
    let c = out.call(app(Method::Get));
    assert_eq!((a, b, c), (CallId(1), CallId(2), CallId(3)));
    assert_eq!(out.ids(Method::Get), [b, c]);
    assert_eq!(out.sent, [app(Method::Event)]);
    assert_eq!(out.take().len(), 3);
    let d = out.call(Call::ListNames);
    assert_eq!(out.take(), [(d, Call::ListNames)], "only what came since the last take");
    assert_eq!(d, CallId(4), "ids keep counting");
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "unique name")]
fn a_call_to_a_well_known_name_is_a_bug() {
    // Items are keyed by their owner's unique name: a well-known name could
    // change hands between two calls (and auto-start is off anyway).
    message(&Call::Get {
        destination: "org.kde.StatusNotifierItem-1-1".into(),
        path: "/StatusNotifierItem".into(),
        property: Property::Item(Prop::Title),
    });
}

#[test]
fn a_call_to_the_daemon_is_refused_with_unknown_method() {
    let mut call = Message::new_method_call(":1.3", "/x", "org.example", "Do").unwrap();
    call.set_serial(9); // as received: libdbus aborts on a reply to serial 0
    let mut r = refusal(&call);
    assert_eq!(r.msg_type(), MessageType::Error);
    assert_eq!(r.get_reply_serial(), Some(9));
    assert_eq!(r.as_result().unwrap_err().name(), Some("org.freedesktop.DBus.Error.UnknownMethod"));
}

#[test]
fn no_session_bus_never_autolaunches() {
    let env =
        |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v));
    let present = |p: &Path| p == Path::new("/run/user/7/bus");
    let absent = |_: &Path| false;
    assert_eq!(
        session_address(env(&[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/x"), ("XDG_RUNTIME_DIR", "/run/user/7")]), present),
        Some("unix:path=/x".into()),
        "the variable wins"
    );
    assert_eq!(
        session_address(env(&[("DBUS_SESSION_BUS_ADDRESS", ""), ("XDG_RUNTIME_DIR", "/run/user/7")]), present),
        Some("unix:path=/run/user/7/bus".into()),
        "an empty variable is no address"
    );
    assert_eq!(session_address(env(&[("XDG_RUNTIME_DIR", "/run/user/7")]), present), Some("unix:path=/run/user/7/bus".into()));
    assert_eq!(session_address(env(&[("XDG_RUNTIME_DIR", "/run/user/7")]), absent), None, "no socket: no bus, never a launch");
    assert_eq!(session_address(env(&[]), present), None, "nothing: no bus");
    assert_eq!(session_address(env(&[("XDG_RUNTIME_DIR", "")]), |_| true), None, "an empty runtime directory is none");
    assert_eq!(
        session_address(env(&[("DBUS_SESSION_BUS_ADDRESS", "autolaunch:"), ("XDG_RUNTIME_DIR", "/run/user/7")]), present),
        Some("unix:path=/run/user/7/bus".into()),
        "an autolaunch address is ignored: libdbus would start dbus-launch"
    );
    assert_eq!(
        session_address(env(&[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/x;autolaunch:"), ("XDG_RUNTIME_DIR", "/run/user/7")]), absent),
        None,
        "an autolaunch entry anywhere in the list: the whole variable is ignored"
    );
}

#[test]
fn the_runtime_socket_address_is_percent_escaped() {
    let env =
        |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v));
    let present = |_: &Path| true;
    assert_eq!(
        session_address(env(&[("XDG_RUNTIME_DIR", "/run/user/7 x,y")]), present),
        Some("unix:path=/run/user/7%20x%2Cy/bus".into()),
        "a space and a comma, both significant to the address grammar, escaped"
    );
    assert_eq!(
        session_address(env(&[("XDG_RUNTIME_DIR", "/run/user/7")]), present),
        Some("unix:path=/run/user/7/bus".into()),
        "ordinary characters pass through unescaped"
    );
}

#[test]
fn session_retries_the_runtime_socket_when_the_variable_is_rejected() {
    let env =
        |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v));
    let present = |p: &Path| p == Path::new("/run/user/7/bus");
    let mut tried = Vec::new();
    let result: io::Result<()> = session_with(
        env(&[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/malformed"), ("XDG_RUNTIME_DIR", "/run/user/7")]),
        present,
        |address| {
            tried.push(address.to_string());
            if address == "unix:path=/malformed" { Err(io::Error::other("rejected")) } else { Ok(()) }
        },
    );
    assert!(result.is_ok());
    assert_eq!(tried, ["unix:path=/malformed", "unix:path=/run/user/7/bus"]);
}

#[test]
fn session_never_retries_when_the_runtime_socket_is_what_it_already_tried() {
    let env =
        |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v));
    let present = |p: &Path| p == Path::new("/run/user/7/bus");
    let mut tried = Vec::new();
    // No DBUS_SESSION_BUS_ADDRESS: session_address already picked the runtime socket, so a
    // failure there must not retry the very same address a second time.
    let result: io::Result<()> = session_with(env(&[("XDG_RUNTIME_DIR", "/run/user/7")]), present, |address| {
        tried.push(address.to_string());
        Err(io::Error::other("rejected"))
    });
    assert!(result.is_err());
    assert_eq!(tried, ["unix:path=/run/user/7/bus"]);
}

#[test]
fn session_gives_up_when_the_variable_fails_and_no_runtime_socket_exists() {
    let env =
        |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v));
    let absent = |_: &Path| false;
    let mut tried = Vec::new();
    let result: io::Result<()> =
        session_with(env(&[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/malformed"), ("XDG_RUNTIME_DIR", "/run/user/7")]), absent, |address| {
            tried.push(address.to_string());
            Err(io::Error::other("rejected"))
        });
    assert!(result.is_err());
    assert_eq!(tried, ["unix:path=/malformed"], "no runtime socket to fall back to: tried once");
}
