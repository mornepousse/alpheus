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
    PROPERTIES_INTERFACE, Prop, SERVER_NAME, SERVER_VENDOR, SETTINGS_INTERFACE, SPEC_VERSION, SYSTEMD_MANAGER, SYSTEMD_PATH,
    WATCHER_INTERFACE, WATCHER_NAME, WATCHER_PATH,
};
use crate::media::Control;
use crate::sources::dbus::BUS_DRIVER;
/// Re-exported so the notifications consumer (session_bus/notifications.rs) passes messages through
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
const DBUS: &str = BUS_DRIVER;
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = PROPERTIES_INTERFACE;

/// The call allowlist (spec § 3): nothing else can be built.
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
    /// The portal's color-scheme.
    ReadOne,
    /// A player's Player properties, at once (media spec § 3).
    GetAll,
    /// The media controls, sent without a reply.
    PlayPause,
    Next,
    Previous,
    /// The user manager's `Subscribe` (the one systemd call: it makes the Manager send its changes).
    Subscribe,
}

impl Method {
    pub const ALL: [Method; 21] = [
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
        Method::Subscribe,
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
            Method::Subscribe => "Subscribe",
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
            Method::Subscribe => SYSTEMD_MANAGER,
        }
    }
}

/// Which property a `Get` reads: an item's, or another watcher's item list (host-only mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Property {
    Item(Prop),
    RegisteredItems,
    /// The user manager's `NFailedUnits` (the Manager interface).
    NFailedUnits,
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
    /// Depth 1 and the fixed MENU_PROPERTIES.
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
    /// `Subscribe()` on the user manager (its unique name), answered: a refusal is read.
    SubscribeUnits {
        dest: String,
    },
}

impl Call {
    // one row per call: the table of the allowlist.
    #[allow(clippy::match_same_arms)]
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
            Call::SubscribeUnits { .. } => Method::Subscribe,
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
            Call::SubscribeUnits { dest } => (dest, SYSTEMD_PATH),
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
    /// reads: dbusmenu `Event`, `RemoveMatch`, the appearance's `AddMatch`, the `ContextMenu`, `SecondaryActivate` and
    /// `Scroll` gestures.
    fn send(&mut self, call: Call);
}

/// The only place a tray call message is built (auto-start off; no_reply for `Event`).
/// Destinations are unique names from the bus and paths are checked when an
/// item registers (`host::split_service`) or typed `o` on the wire: a call
/// that libdbus would refuse to build is a bug, not an application's doing.
/// So is a call to a well-known name (it could change hands between two
/// calls): debug builds assert it never happens.
/// The portal is the one well-known destination: a one-shot read with auto-start off;
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
        Call::Get { property: Property::NFailedUnits, .. } => m.append2(SYSTEMD_MANAGER, "NFailedUnits"),
        Call::Activate { x, y, .. } | Call::SecondaryActivate { x, y, .. } | Call::ContextMenu { x, y, .. } => m.append2(*x, *y),
        Call::Scroll { delta, orientation, .. } => m.append2(*delta, orientation.name()),
        Call::AboutToShow { id, .. } => m.append1(*id),
        Call::GetLayout { parent, .. } => m.append3(*parent, 1i32, &MENU_PROPERTIES[..]),
        Call::Event { id, timestamp, .. } => m.append3(*id, "clicked", Variant(0i32)).append1(*timestamp),
        Call::RequestName => m.append2(WATCHER_NAME, DBUS_NAME_FLAG_DO_NOT_QUEUE),
        Call::RequestNotifications => m.append2(NOTIFICATIONS_NAME, NOTIFICATIONS_NAME_FLAGS),
        Call::GetNameOwner { name } | Call::GetConnectionUnixProcessID { name } => m.append1(name.as_str()),
        Call::AddMatch { rule } | Call::RemoveMatch { rule } => m.append1(rule.as_str()),
        Call::RegisterStatusNotifierHost { host, .. } => m.append1(host.as_str()),
        Call::ReadOne => m.append2(APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY),
        Call::PlayerGetAll { .. } => m.append1(MPRIS_PLAYER_INTERFACE),
        Call::PlayerIdentity { .. } => m.append2(MPRIS_INTERFACE, "Identity"),
        Call::PlayerMetadata { .. } => m.append2(MPRIS_PLAYER_INTERFACE, "Metadata"),
        Call::PlayerPosition { .. } => m.append2(MPRIS_PLAYER_INTERFACE, "Position"),
        Call::ListNames | Call::PlayerControl { .. } | Call::SubscribeUnits { .. } => m,
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
pub fn closed_signal(id: u32, reason: u32) -> Option<Message> {
    Some(notifications_signal("NotificationClosed")?.append2(id, reason))
}

/// `ActionInvoked(id, key)`, broadcast: the key as the client sent it.
pub fn invoked_signal(id: u32, key: &str) -> Option<Message> {
    Some(notifications_signal("ActionInvoked")?.append2(id, key))
}

/// A signal on the Notifications object. Its path, interface and member are constants libdbus
/// accepts; were one ever refused, the signal is dropped (and logged), never a panic.
fn notifications_signal(member: &str) -> Option<Message> {
    Message::new_signal(NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, member)
        .inspect_err(|e| crate::log::write(&format!("notifications: {member} not built: {e}")))
        .ok()
}

/// The session bus address, pure: `var("DBUS_SESSION_BUS_ADDRESS")` if set, non-empty
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
    /// The session bus, never autolaunched: $DBUS_SESSION_BUS_ADDRESS (unless
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
    /// blocking connect. Calls are small; a burst of
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
        let mut m = Message::new_method_call(":1.1", NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, "Notify")
            .unwrap()
            .append3("Fictional", 0u32, "")
            .append3(summary, "", Vec::<String>::new())
            .append2(BTreeMap::<String, Variant<u8>>::new(), -1i32);
        m.set_serial(9);
        m.set_sender(Some(":1.42".into()));
        m
    }

    /// The same Notify, critical (`urgency` 2).
    pub fn critical_notify_call(summary: &str) -> Message {
        let hints: BTreeMap<String, Variant<u8>> = [("urgency".to_string(), Variant(2u8))].into();
        let mut m = Message::new_method_call(":1.1", NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, "Notify")
            .unwrap()
            .append3("Fictional", 0u32, "")
            .append3(summary, "", Vec::<String>::new())
            .append2(hints, -1i32);
        m.set_serial(9);
        m.set_sender(Some(":1.42".into()));
        m
    }
}

#[cfg(test)]
mod tests;
