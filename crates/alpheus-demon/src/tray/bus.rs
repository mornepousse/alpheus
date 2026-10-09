//! The tray's connection to the session bus (dbus-rs, libdbus's low-level
//! `Channel`), never blocking after the connect: calls are sent and
//! remembered by serial, answers matched when they arrive, each call gives
//! up after `TIMEOUT` (`expire`, driven by a `DeadlineTimer` armed only
//! while something is pending). Every call is a typed `Call` from the
//! allowlist (spec § 3), built in ONE place, `message`, with auto-start off:
//! nothing outside the list can reach an application, and no call can
//! start a service.

use super::{
    APPEARANCE_NAMESPACE, CAPABILITIES, COLOR_SCHEME_KEY, ITEM_INTERFACE, MENU_INTERFACE, MPRIS_INTERFACE, MPRIS_PATH,
    MPRIS_PLAYER_INTERFACE, NOTIFICATIONS_INTERFACE, NOTIFICATIONS_NAME, NOTIFICATIONS_PATH, Orientation, PORTAL_NAME, PORTAL_PATH,
    PROPERTIES_INTERFACE, Prop, SERVER_NAME, SERVER_VENDOR, SETTINGS_INTERFACE, SPEC_VERSION, WATCHER_INTERFACE, WATCHER_NAME,
    WATCHER_PATH,
};
use crate::media::Control;
/// Re-exported so the notifications consumer (tray/notifications.rs) passes messages through
/// without naming the dbus crate (guardrails: DBUS_FILES).
pub use dbus::Message;
use dbus::arg::Variant;
use dbus::channel::Channel;
use dbus::message::MessageType;
use dbus::strings::ErrorName;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::Path;
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(2);
pub const DBUS_NAME_FLAG_DO_NOT_QUEUE: u32 = 4;
/// RequestName's flags for the notification server: none. It queues behind an owner (mako) instead
/// of failing, never takes the name from it, and never lets a later client take it from us.
pub const NOTIFICATIONS_NAME_FLAGS: u32 = 0;
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = PROPERTIES_INTERFACE;

/// The call allowlist (spec § 3 + controller decision 3): nothing else can be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Activate,
    SecondaryActivate,
    ContextMenu,
    Scroll,
    AboutToShow,
    GetLayout,
    Event,
    RequestName,
    GetNameOwner,
    GetConnectionUnixProcessID,
    ListNames,
    AddMatch,
    RemoveMatch,
    RegisterStatusNotifierHost,
    /// The portal's color-scheme (themes task 6).
    ReadOne,
    /// A player's Player properties, at once (media spec § 3).
    GetAll,
    /// The media controls, sent without a reply.
    PlayPause,
    Next,
    Previous,
}

impl Method {
    pub const ALL: [Method; 20] = [
        Method::Get,
        Method::Activate,
        Method::SecondaryActivate,
        Method::ContextMenu,
        Method::Scroll,
        Method::AboutToShow,
        Method::GetLayout,
        Method::Event,
        Method::RequestName,
        Method::GetNameOwner,
        Method::GetConnectionUnixProcessID,
        Method::ListNames,
        Method::AddMatch,
        Method::RemoveMatch,
        Method::RegisterStatusNotifierHost,
        Method::ReadOne,
        Method::GetAll,
        Method::PlayPause,
        Method::Next,
        Method::Previous,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Method::Get => "Get",
            Method::Activate => "Activate",
            Method::SecondaryActivate => "SecondaryActivate",
            Method::ContextMenu => "ContextMenu",
            Method::Scroll => "Scroll",
            Method::AboutToShow => "AboutToShow",
            Method::GetLayout => "GetLayout",
            Method::Event => "Event",
            Method::RequestName => "RequestName",
            Method::GetNameOwner => "GetNameOwner",
            Method::GetConnectionUnixProcessID => "GetConnectionUnixProcessID",
            Method::ListNames => "ListNames",
            Method::AddMatch => "AddMatch",
            Method::RemoveMatch => "RemoveMatch",
            Method::RegisterStatusNotifierHost => "RegisterStatusNotifierHost",
            Method::ReadOne => "ReadOne",
            Method::GetAll => "GetAll",
            Method::PlayPause => "PlayPause",
            Method::Next => "Next",
            Method::Previous => "Previous",
        }
    }

    pub fn interface(self) -> &'static str {
        match self {
            Method::Get | Method::GetAll => PROPERTIES,
            Method::Activate | Method::SecondaryActivate | Method::ContextMenu | Method::Scroll => ITEM_INTERFACE,
            Method::AboutToShow | Method::GetLayout | Method::Event => MENU_INTERFACE,
            Method::RequestName
            | Method::GetNameOwner
            | Method::GetConnectionUnixProcessID
            | Method::ListNames
            | Method::AddMatch
            | Method::RemoveMatch => DBUS,
            Method::RegisterStatusNotifierHost => WATCHER_INTERFACE,
            Method::ReadOne => SETTINGS_INTERFACE,
            Method::PlayPause | Method::Next | Method::Previous => MPRIS_PLAYER_INTERFACE,
        }
    }
}

/// Which property a `Get` reads: an item's, or another watcher's item list (host-only mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Property {
    Item(Prop),
    RegisteredItems,
}

/// One call, with its typed arguments. `destination` is always a unique name (":1.N").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Get {
        destination: String,
        path: String,
        property: Property,
    },
    Activate {
        destination: String,
        path: String,
        x: i32,
        y: i32,
    },
    SecondaryActivate {
        destination: String,
        path: String,
        x: i32,
        y: i32,
    },
    ContextMenu {
        destination: String,
        path: String,
        x: i32,
        y: i32,
    },
    Scroll {
        destination: String,
        path: String,
        delta: i32,
        orientation: Orientation,
    },
    AboutToShow {
        destination: String,
        path: String,
        id: i32,
    },
    /// Depth 1 and the fixed MENU_PROPERTIES (controller decision 6).
    GetLayout {
        destination: String,
        path: String,
        parent: i32,
    },
    /// `Event(id, "clicked", v(i32 0), timestamp)`.
    Event {
        destination: String,
        path: String,
        id: i32,
        timestamp: u32,
    },
    /// org.kde.StatusNotifierWatcher with DO_NOT_QUEUE.
    RequestName,
    /// `RequestName(NOTIFICATIONS_NAME, 0)`: queued behind any owner (notifications spec § 1).
    RequestNotifications,
    GetNameOwner {
        name: String,
    },
    GetConnectionUnixProcessID {
        name: String,
    },
    ListNames,
    AddMatch {
        rule: String,
    },
    RemoveMatch {
        rule: String,
    },
    /// On another watcher (its unique name), registering our unique name as a host.
    RegisterStatusNotifierHost {
        destination: String,
        host: String,
    },
    /// `ReadOne(APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY)` on the portal: nothing else of the portal can be read.
    ReadOne,
    /// `GetAll(MPRIS_PLAYER_INTERFACE)` on a player's MPRIS object (media spec § 3).
    PlayerGetAll {
        dest: String,
    },
    /// `Get(MPRIS_INTERFACE, "Identity")`: the player's name.
    PlayerIdentity {
        dest: String,
    },
    /// `Get(MPRIS_PLAYER_INTERFACE, "Metadata")`: re-read after it came invalidated.
    PlayerMetadata {
        dest: String,
    },
    /// `Get(MPRIS_PLAYER_INTERFACE, "Position")`: the popup's resync.
    PlayerPosition {
        dest: String,
    },
    /// PlayPause, Next or Previous, through `Outbox::send` (no reply).
    PlayerControl {
        dest: String,
        member: Control,
    },
}

impl Call {
    pub fn method(&self) -> Method {
        match self {
            Call::Get { .. } => Method::Get,
            Call::Activate { .. } => Method::Activate,
            Call::SecondaryActivate { .. } => Method::SecondaryActivate,
            Call::ContextMenu { .. } => Method::ContextMenu,
            Call::Scroll { .. } => Method::Scroll,
            Call::AboutToShow { .. } => Method::AboutToShow,
            Call::GetLayout { .. } => Method::GetLayout,
            Call::Event { .. } => Method::Event,
            Call::RequestName | Call::RequestNotifications => Method::RequestName,
            Call::GetNameOwner { .. } => Method::GetNameOwner,
            Call::GetConnectionUnixProcessID { .. } => Method::GetConnectionUnixProcessID,
            Call::ListNames => Method::ListNames,
            Call::AddMatch { .. } => Method::AddMatch,
            Call::RemoveMatch { .. } => Method::RemoveMatch,
            Call::RegisterStatusNotifierHost { .. } => Method::RegisterStatusNotifierHost,
            Call::ReadOne => Method::ReadOne,
            Call::PlayerGetAll { .. } => Method::GetAll,
            Call::PlayerIdentity { .. } | Call::PlayerMetadata { .. } | Call::PlayerPosition { .. } => Method::Get,
            Call::PlayerControl { member: Control::PlayPause, .. } => Method::PlayPause,
            Call::PlayerControl { member: Control::Next, .. } => Method::Next,
            Call::PlayerControl { member: Control::Previous, .. } => Method::Previous,
        }
    }

    /// Where the call goes: an application's unique name and object, or the bus driver.
    fn target(&self) -> (&str, &str) {
        match self {
            Call::Get { destination, path, .. }
            | Call::Activate { destination, path, .. }
            | Call::SecondaryActivate { destination, path, .. }
            | Call::ContextMenu { destination, path, .. }
            | Call::Scroll { destination, path, .. }
            | Call::AboutToShow { destination, path, .. }
            | Call::GetLayout { destination, path, .. }
            | Call::Event { destination, path, .. } => (destination, path),
            Call::RegisterStatusNotifierHost { destination, .. } => (destination, WATCHER_PATH),
            Call::ReadOne => (PORTAL_NAME, PORTAL_PATH),
            Call::PlayerGetAll { dest }
            | Call::PlayerIdentity { dest }
            | Call::PlayerMetadata { dest }
            | Call::PlayerPosition { dest }
            | Call::PlayerControl { dest, .. } => (dest, MPRIS_PATH),
            Call::RequestName
            | Call::RequestNotifications
            | Call::GetNameOwner { .. }
            | Call::GetConnectionUnixProcessID { .. }
            | Call::ListNames
            | Call::AddMatch { .. }
            | Call::RemoveMatch { .. } => (DBUS, DBUS_PATH),
        }
    }
}

/// The dbusmenu properties `GetLayout` asks for (never icon-name/icon-data).
pub const MENU_PROPERTIES: [&str; 7] = ["type", "label", "enabled", "visible", "toggle-type", "toggle-state", "children-display"];

/// A call's identity: the serial libdbus gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// No answer within TIMEOUT.
    Timeout,
    /// The connection closed before the answer.
    Disconnected,
    /// A D-Bus error reply, by name ("org.freedesktop.DBus.Error.UnknownMethod"…).
    Remote(String),
    /// An answer whose body is not what the call expects (the codec).
    BadReply(String),
}

/// What the connection delivers, in arrival order.
#[derive(Debug)]
pub enum Incoming {
    /// The answer to `id` (method return or error) — or its timeout, from `expire`.
    Answer {
        id: CallId,
        call: Call,
        result: Result<Message, CallError>,
    },
    Signal(Message),
    /// A method call addressed to the daemon (the watcher serves it; anything else is refused).
    MethodCall(Message),
    /// The connection is gone (bus died): the tray empties, no retry.
    Closed,
}

/// Where tray logic sends calls: the real `SessionBus`, or `FakeOutbox` in unit tests.
pub trait Outbox {
    /// Sends `call` (auto-start off) and expects an answer within TIMEOUT.
    fn call(&mut self, call: Call) -> CallId;
    /// Sends `call` with NO_REPLY_EXPECTED: no pending entry, no timer — calls whose answer nobody
    /// reads: dbusmenu `Event`, `RemoveMatch`, the appearance's `AddMatch` (themes task 6), the `ContextMenu`, `SecondaryActivate` and
    /// `Scroll` gestures.
    fn send(&mut self, call: Call);
}

/// The only place a tray call message is built (auto-start off; no_reply for `Event`).
/// Destinations are unique names from the bus and paths are checked when an
/// item registers (`host::split_service`) or typed `o` on the wire: a call
/// that libdbus would refuse to build is a bug, not an application's doing.
/// So is a call to a well-known name (it could change hands between two
/// calls): debug builds assert it never happens.
/// The portal is the one well-known destination (themes task 6): a one-shot read with auto-start off;
/// if the name changes hands, `SettingChanged` (a rule on the well-known sender) follows the new owner.
pub fn message(call: &Call) -> Message {
    let method = call.method();
    let (destination, path) = call.target();
    debug_assert!(
        destination.starts_with(':') || destination == DBUS || destination == PORTAL_NAME,
        "tray call {method:?} to {destination}: not a unique name, the bus driver nor the portal"
    );
    let m = Message::new_method_call(destination, path, method.interface(), method.name())
        .unwrap_or_else(|e| panic!("tray call {method:?} to {destination} {path}: {e}"));
    let mut m = match call {
        Call::Get { property: Property::Item(p), .. } => m.append2(ITEM_INTERFACE, p.name()),
        Call::Get { property: Property::RegisteredItems, .. } => m.append2(WATCHER_INTERFACE, "RegisteredStatusNotifierItems"),
        Call::Activate { x, y, .. } | Call::SecondaryActivate { x, y, .. } | Call::ContextMenu { x, y, .. } => m.append2(*x, *y),
        Call::Scroll { delta, orientation, .. } => m.append2(*delta, orientation.name()),
        Call::AboutToShow { id, .. } => m.append1(*id),
        Call::GetLayout { parent, .. } => m.append3(*parent, 1i32, &MENU_PROPERTIES[..]),
        Call::Event { id, timestamp, .. } => m.append3(*id, "clicked", Variant(0i32)).append1(*timestamp),
        Call::RequestName => m.append2(WATCHER_NAME, DBUS_NAME_FLAG_DO_NOT_QUEUE),
        Call::RequestNotifications => m.append2(NOTIFICATIONS_NAME, NOTIFICATIONS_NAME_FLAGS),
        Call::GetNameOwner { name } | Call::GetConnectionUnixProcessID { name } => m.append1(name.as_str()),
        Call::ListNames => m,
        Call::AddMatch { rule } | Call::RemoveMatch { rule } => m.append1(rule.as_str()),
        Call::RegisterStatusNotifierHost { host, .. } => m.append1(host.as_str()),
        Call::ReadOne => m.append2(APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY),
        Call::PlayerGetAll { .. } => m.append1(MPRIS_PLAYER_INTERFACE),
        Call::PlayerIdentity { .. } => m.append2(MPRIS_INTERFACE, "Identity"),
        Call::PlayerMetadata { .. } => m.append2(MPRIS_PLAYER_INTERFACE, "Metadata"),
        Call::PlayerPosition { .. } => m.append2(MPRIS_PLAYER_INTERFACE, "Position"),
        Call::PlayerControl { .. } => m,
    };
    m.set_auto_start(false);
    if method == Method::Event {
        m.set_no_reply(true);
    }
    m
}

/// UnknownMethod answer to a call the daemon does not serve (never left unanswered).
pub fn refusal(call: &Message) -> Message {
    call.error(&ErrorName::from("org.freedesktop.DBus.Error.UnknownMethod"), c"alpheus serves no such call")
}

/// Notify's answer: the id (notifications spec § 1), sent even when no bubble shows.
pub fn notify_reply(call: &Message, id: u32) -> Message {
    call.method_return().append1(id)
}

/// GetCapabilities's answer: CAPABILITIES, an `as`.
pub fn capabilities_reply(call: &Message) -> Message {
    call.method_return().append1(&CAPABILITIES[..])
}

/// GetServerInformation's answer: `(SERVER_NAME, SERVER_VENDOR, the crate's version, SPEC_VERSION)`.
pub fn server_info_reply(call: &Message) -> Message {
    call.method_return().append2(SERVER_NAME, SERVER_VENDOR).append2(env!("CARGO_PKG_VERSION"), SPEC_VERSION)
}

/// An answer with no body: CloseNotification's, whatever the id.
pub fn empty_reply(call: &Message) -> Message {
    call.method_return()
}

/// `NotificationClosed(id, reason)`, broadcast as mako does (1 expired, 2 dismissed, 3 closed by call).
pub fn closed_signal(id: u32, reason: u32) -> Message {
    notifications_signal("NotificationClosed").append2(id, reason)
}

/// `ActionInvoked(id, key)`, broadcast: the key as the client sent it.
pub fn invoked_signal(id: u32, key: &str) -> Message {
    notifications_signal("ActionInvoked").append2(id, key)
}

fn notifications_signal(member: &str) -> Message {
    Message::new_signal(NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, member).expect("a constant path, interface and member")
}

/// The session bus address, pure (decision 1): `var("DBUS_SESSION_BUS_ADDRESS")` if set, non-empty
/// and free of any `autolaunch:` entry, else "unix:path=<XDG_RUNTIME_DIR>/bus" when `exists` says that
/// socket is there, else None. An address list naming `autolaunch:` anywhere is ignored whole: libdbus
/// would fall back to it, i.e. run `dbus-launch`.
pub fn session_address(var: impl Fn(&str) -> Option<OsString>, exists: impl Fn(&Path) -> bool) -> Option<String> {
    let launches = |a: &OsString| a.to_string_lossy().split(';').any(|entry| entry.trim().starts_with("autolaunch:"));
    if let Some(a) = var("DBUS_SESSION_BUS_ADDRESS").filter(|a| !a.is_empty() && !launches(a)) {
        return Some(a.to_string_lossy().into_owned());
    }
    runtime_bus_address(var, exists)
}

/// "unix:path=<XDG_RUNTIME_DIR>/bus" when `exists` says that socket is there, else None: the half of
/// `session_address` that never reads `DBUS_SESSION_BUS_ADDRESS`, reused by `session()` to retry once
/// libdbus rejects the variable's address (a malformed or stale value some other program left behind).
fn runtime_bus_address(var: impl Fn(&str) -> Option<OsString>, exists: impl Fn(&Path) -> bool) -> Option<String> {
    let socket = Path::new(&var("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?).join("bus");
    exists(&socket).then(|| format!("unix:path={}", escape_address_value(&socket.to_string_lossy())))
}

/// Percent-escapes `s` for a D-Bus address's `path=` value (D-Bus Specification, "Server Addresses"):
/// every byte outside `[-0-9A-Za-z_/.]` becomes `%XX`, so a runtime directory with an unusual byte
/// (a space, a comma, anything `;`/`:`/`=`-like that the address grammar itself uses) still yields an
/// address libdbus parses as one path, not several fields.
fn escape_address_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/' | b'.') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `session()`'s logic, with the environment and the connection attempt both injected: testable
/// without a real bus. Tries the variable's (or the runtime socket's, when the variable is absent)
/// address first; if `open` rejects it and the runtime socket exists and differs, retries there once
/// (a malformed or stale `DBUS_SESSION_BUS_ADDRESS` must not strand the tray when the ordinary socket
/// would have worked).
fn session_with<T>(
    var: impl Fn(&str) -> Option<OsString>,
    exists: impl Fn(&Path) -> bool,
    mut open: impl FnMut(&str) -> io::Result<T>,
) -> io::Result<T> {
    let Some(address) = session_address(&var, &exists) else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no usable DBUS_SESSION_BUS_ADDRESS and no $XDG_RUNTIME_DIR/bus"));
    };
    match open(&address) {
        Ok(bus) => Ok(bus),
        Err(e) => match runtime_bus_address(&var, &exists) {
            Some(fallback) if fallback != address => open(&fallback),
            _ => Err(e),
        },
    }
}

pub struct SessionBus {
    channel: Channel,
    pending: BTreeMap<u32, (Call, Instant)>,
    /// Serials for calls libdbus could not queue (out of memory): counted
    /// down from u32::MAX, far from libdbus's own, so they time out normally.
    lost: u32,
    closed: bool,
    /// Answers `read` must deliver before anything else: a call made after the bus already closed
    /// (`Outbox::call` on a closed `SessionBus`) is answered Disconnected here, at once, with no
    /// pending entry and so no deadline timer.
    ready: Vec<Incoming>,
}

impl SessionBus {
    /// The session bus, never autolaunched (controller decision 1): $DBUS_SESSION_BUS_ADDRESS (unless
    /// it names `autolaunch:`), else unix:path=$XDG_RUNTIME_DIR/bus if it exists, else Err(NotFound).
    /// Blocking connect + Hello, once. Retries the runtime socket once if libdbus rejects the
    /// variable's address (`session_with`).
    pub fn session() -> io::Result<SessionBus> {
        session_with(|k| std::env::var_os(k), |p| p.exists(), SessionBus::at)
    }

    /// The bus at `address` (tests: a PrivateBus).
    pub fn at(address: &str) -> io::Result<SessionBus> {
        let failed = |e: dbus::Error| io::Error::other(format!("{}: {}", e.name().unwrap_or(""), e.message().unwrap_or("")));
        let mut channel = Channel::open_private(address).map_err(failed)?;
        channel.register().map_err(failed)?;
        channel.set_watch_enabled(true);
        Ok(SessionBus { channel, pending: BTreeMap::new(), lost: u32::MAX, closed: false, ready: Vec::new() })
    }

    pub fn unique_name(&self) -> &str {
        self.channel.unique_name().unwrap_or("")
    }

    /// A CLOEXEC duplicate of the connection's fd, for a calloop Generic source (READ, level).
    pub fn watch_fd(&mut self) -> io::Result<OwnedFd> {
        let fd = self.channel.watch().fd;
        // SAFETY: `fd` is libdbus's socket, open as long as `self.channel`
        // lives; it is only borrowed for the duplication (F_DUPFD_CLOEXEC).
        unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()
    }

    /// Reads without blocking (read_write(Some(0))) and drains every queued message; answers are
    /// matched by reply serial (a late answer, its call already expired, is dropped). What an earlier
    /// `flush` already drained comes first.
    pub fn read(&mut self) -> Vec<Incoming> {
        let mut out = std::mem::take(&mut self.ready);
        if self.closed {
            return out;
        }
        let lost = self.channel.read_write(Some(Duration::ZERO)).is_err();
        self.drain(&mut out, lost);
        out
    }

    /// Pops every message libdbus holds into `out`, answers matched to their call; a lost or
    /// disconnected connection ends in `Closed`, every pending call answered Disconnected first.
    fn drain(&mut self, out: &mut Vec<Incoming>, mut lost: bool) {
        while let Some(m) = self.channel.pop_message() {
            match m.msg_type() {
                MessageType::MethodReturn | MessageType::Error => {
                    if let Some((serial, (call, _))) = m.get_reply_serial().and_then(|s| self.pending.remove(&s).map(|p| (s, p))) {
                        out.push(Incoming::Answer { id: CallId(serial), call, result: Ok(m) });
                    }
                }
                MessageType::Signal if m.interface().is_some_and(|i| &*i == "org.freedesktop.DBus.Local") => lost = true,
                MessageType::Signal => out.push(Incoming::Signal(m)),
                MessageType::MethodCall => out.push(Incoming::MethodCall(m)),
            }
        }
        if lost || !self.channel.is_connected() {
            self.closed = true;
            for (serial, (call, _)) in std::mem::take(&mut self.pending) {
                out.push(Incoming::Answer { id: CallId(serial), call, result: Err(CallError::Disconnected) });
            }
            out.push(Incoming::Closed);
        }
    }

    /// What `flush` (or a call on a closed bus) left for delivery: taken, in arrival order.
    pub fn take_ready(&mut self) -> Vec<Incoming> {
        std::mem::take(&mut self.ready)
    }

    pub fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    /// Every pending call whose deadline is ≤ now, as Answer { result: Err(Timeout) }.
    pub fn expire(&mut self, now: Instant) -> Vec<Incoming> {
        let due: Vec<u32> = self.pending.iter().filter(|(_, (_, deadline))| *deadline <= now).map(|(s, _)| *s).collect();
        due.into_iter()
            .filter_map(|s| {
                self.pending.remove(&s).map(|(call, _)| Incoming::Answer { id: CallId(s), call, result: Err(CallError::Timeout) })
            })
            .collect()
    }

    /// The earliest pending deadline; None when nothing is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|(_, deadline)| *deadline).min()
    }

    /// Sends a reply, an error or a signal built elsewhere (watcher, router) — never a method call:
    /// those go through `call`/`send`, which track or wave off a reply as the call needs.
    pub fn send_message(&mut self, m: Message) {
        debug_assert_ne!(m.msg_type(), MessageType::MethodCall, "send_message is for replies, errors and signals, not calls");
        let _ = self.channel.send(m);
    }

    /// Writes libdbus's outgoing queue, blocking until it is written. Only the
    /// bus daemon reads this socket — an application that never reads cannot
    /// make it wait (the daemon queues for it, up to its own limits, then
    /// answers with an error) — and the daemon reads its clients promptly; a
    /// stopped daemon would stall the bar here, the same acceptance as the
    /// blocking connect (controller decision 2). Calls are small; a burst of
    /// them fits the socket buffer, so this returns at once in practice.
    ///
    /// With messages queued, libdbus's flush also reads the socket (DO_READING): whatever arrives
    /// is then in its queue while the fd, level-triggered, no longer reads as ready — so it is
    /// drained here into `ready` (`take_ready`), never left behind to be reported as a timeout.
    pub fn flush(&mut self) {
        if self.closed {
            return;
        }
        self.channel.flush();
        let mut drained = Vec::new();
        self.drain(&mut drained, false);
        self.ready.extend(drained);
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

impl Outbox for SessionBus {
    fn call(&mut self, call: Call) -> CallId {
        if self.closed {
            // No socket left to send on: answered at once, never pending, so no deadline timer
            // ever arms for it (`expire`/`next_deadline` never see this id).
            self.lost -= 1;
            let id = CallId(self.lost);
            self.ready.push(Incoming::Answer { id, call, result: Err(CallError::Disconnected) });
            return id;
        }
        let serial = self.channel.send(message(&call)).unwrap_or_else(|()| {
            self.lost -= 1;
            self.lost
        });
        self.pending.insert(serial, (call, Instant::now() + TIMEOUT));
        CallId(serial)
    }

    fn send(&mut self, call: Call) {
        let mut m = message(&call);
        m.set_no_reply(true);
        let _ = self.channel.send(m);
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// Records every call; ids count from 1. The allowlist test reads `calls` and `sent`.
    #[derive(Default)]
    pub struct FakeOutbox {
        pub calls: Vec<(CallId, Call)>,
        pub sent: Vec<Call>,
        last: u32,
    }

    impl Outbox for FakeOutbox {
        fn call(&mut self, call: Call) -> CallId {
            self.last += 1;
            let id = CallId(self.last);
            self.calls.push((id, call));
            id
        }

        fn send(&mut self, call: Call) {
            self.sent.push(call);
        }
    }

    impl FakeOutbox {
        /// Ids of the calls whose method is `m`, in order.
        pub fn ids(&self, m: Method) -> Vec<CallId> {
            self.calls.iter().filter(|(_, c)| c.method() == m).map(|(id, _)| *id).collect()
        }

        /// The calls recorded since the last `take`, removed (ids keep counting).
        pub fn take(&mut self) -> Vec<(CallId, Call)> {
            std::mem::take(&mut self.calls)
        }
    }
    /// A plain `Notify("Fictional", 0, "", summary, "", [], {}, -1)` as the bus delivers it, for the
    /// consumer's tests (notifications.rs names no dbus type).
    pub fn notify_call(summary: &str) -> Message {
        let mut m = Message::new_method_call(":1.1", super::super::NOTIFICATIONS_PATH, super::super::NOTIFICATIONS_INTERFACE, "Notify")
            .unwrap()
            .append3("Fictional", 0u32, "")
            .append3(summary, "", Vec::<String>::new())
            .append2(BTreeMap::<String, Variant<u8>>::new(), -1i32);
        m.set_serial(9);
        m.set_sender(Some(":1.42".into()));
        m
    }
}

#[cfg(test)]
mod tests {
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
        use crate::tray::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_NAME, PORTAL_PATH, SETTINGS_INTERFACE};
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
        let closed = closed_signal(7, 2);
        assert_eq!(
            head(&closed),
            (MessageType::Signal, some(NOTIFICATIONS_PATH), some(NOTIFICATIONS_INTERFACE), some("NotificationClosed"))
        );
        assert!(closed.destination().is_none(), "broadcast, as mako does");
        assert_eq!(signatures(&closed), ["u", "u"]);
        assert_eq!(closed.read2::<u32, u32>().unwrap(), (7, 2));
        let invoked = invoked_signal(7, "default");
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
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
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
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
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
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
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
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
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
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
        let absent = |_: &Path| false;
        let mut tried = Vec::new();
        let result: io::Result<()> = session_with(
            env(&[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/malformed"), ("XDG_RUNTIME_DIR", "/run/user/7")]),
            absent,
            |address| {
                tried.push(address.to_string());
                Err(io::Error::other("rejected"))
            },
        );
        assert!(result.is_err());
        assert_eq!(tried, ["unix:path=/malformed"], "no runtime socket to fall back to: tried once");
    }
}
