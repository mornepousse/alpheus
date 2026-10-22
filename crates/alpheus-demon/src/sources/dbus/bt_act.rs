//! Bluetooth's acting connection (quick settings delivery 3, spec § 5): the only place alpheus
//! changes anything on BlueZ. Its own private connection to the system bus, never blocking after
//! the connect, opened only while the panel's Bluetooth detail is open or a call is in flight. Every
//! call is a typed `ActCall`, built in ONE place (`build`) from the pinned `ActMethod` list, sent to
//! `org.bluez`'s unique name (read first with `GetNameOwner`, calls queued meanwhile) with
//! auto-start off: nothing outside the list can reach BlueZ, and no call can start it. Answers are
//! matched by reply serial; the read connection (`bt.rs`) stays read only.

use dbus::Message;
use dbus::arg::Variant;
use dbus::channel::{BusType, Channel};
use dbus::message::MessageType;
use dbus::strings::{BusName, ErrorName, Interface, Member, Path};
use std::collections::BTreeMap;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::time::Duration;

const BLUEZ: &str = "org.bluez";
const BUS_DRIVER: &str = "org.freedesktop.DBus";
const BUS_DRIVER_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const AGENT_MANAGER: &str = "org.bluez.AgentManager1";
const LOCAL: &str = "org.freedesktop.DBus.Local";
/// What a method call to this connection gets (Task 5 serves the agent's).
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
/// A call libdbus refused to build (an object path that is not one).
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";

/// Every method this connection may call, exhaustively (spec § 5; pinned by tests/guardrails.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActMethod {
    GetNameOwner,
    Set,
    StartDiscovery,
    StopDiscovery,
    RemoveDevice,
    Connect,
    Disconnect,
    Pair,
    CancelPairing,
    RegisterAgent,
    UnregisterAgent,
}

impl ActMethod {
    pub const ALL: [ActMethod; 11] = [
        ActMethod::GetNameOwner,
        ActMethod::Set,
        ActMethod::StartDiscovery,
        ActMethod::StopDiscovery,
        ActMethod::RemoveDevice,
        ActMethod::Connect,
        ActMethod::Disconnect,
        ActMethod::Pair,
        ActMethod::CancelPairing,
        ActMethod::RegisterAgent,
        ActMethod::UnregisterAgent,
    ];

    /// The D-Bus member.
    pub fn name(self) -> &'static str {
        match self {
            ActMethod::GetNameOwner => "GetNameOwner",
            ActMethod::Set => "Set",
            ActMethod::StartDiscovery => "StartDiscovery",
            ActMethod::StopDiscovery => "StopDiscovery",
            ActMethod::RemoveDevice => "RemoveDevice",
            ActMethod::Connect => "Connect",
            ActMethod::Disconnect => "Disconnect",
            ActMethod::Pair => "Pair",
            ActMethod::CancelPairing => "CancelPairing",
            ActMethod::RegisterAgent => "RegisterAgent",
            ActMethod::UnregisterAgent => "UnregisterAgent",
        }
    }

    fn interface(self) -> &'static str {
        match self {
            ActMethod::GetNameOwner => BUS_DRIVER,
            ActMethod::Set => PROPERTIES,
            ActMethod::StartDiscovery | ActMethod::StopDiscovery | ActMethod::RemoveDevice => ADAPTER,
            ActMethod::Connect | ActMethod::Disconnect | ActMethod::Pair | ActMethod::CancelPairing => DEVICE,
            ActMethod::RegisterAgent | ActMethod::UnregisterAgent => AGENT_MANAGER,
        }
    }
}

/// One action on BlueZ, as the panel asks it; paths are BlueZ object paths (never logged: they
/// embed the device's address).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActCall {
    /// The adapter's `Powered`.
    Powered {
        adapter: String,
        on: bool,
    },
    /// A device's `Trusted`, only ever set to true.
    Trusted {
        device: String,
    },
    StartDiscovery {
        adapter: String,
    },
    StopDiscovery {
        adapter: String,
    },
    /// Forgets a device (unpairs it).
    RemoveDevice {
        adapter: String,
        device: String,
    },
    Connect {
        device: String,
    },
    Disconnect {
        device: String,
    },
    Pair {
        device: String,
    },
    CancelPairing {
        device: String,
    },
}

impl ActCall {
    pub fn method(&self) -> ActMethod {
        match self {
            ActCall::Powered { .. } | ActCall::Trusted { .. } => ActMethod::Set,
            ActCall::StartDiscovery { .. } => ActMethod::StartDiscovery,
            ActCall::StopDiscovery { .. } => ActMethod::StopDiscovery,
            ActCall::RemoveDevice { .. } => ActMethod::RemoveDevice,
            ActCall::Connect { .. } => ActMethod::Connect,
            ActCall::Disconnect { .. } => ActMethod::Disconnect,
            ActCall::Pair { .. } => ActMethod::Pair,
            ActCall::CancelPairing { .. } => ActMethod::CancelPairing,
        }
    }

    /// The action's word in the log line `quick: bluetooth <word> failed (…)`.
    pub fn word(&self) -> &'static str {
        match self {
            ActCall::Powered { .. } => "power",
            ActCall::Trusted { .. } => "trust",
            ActCall::StartDiscovery { .. } => "discovery",
            ActCall::StopDiscovery { .. } => "discovery stop",
            ActCall::RemoveDevice { .. } => "forget",
            ActCall::Connect { .. } => "connect",
            ActCall::Disconnect { .. } => "disconnect",
            ActCall::Pair { .. } => "pair",
            ActCall::CancelPairing { .. } => "cancel pairing",
        }
    }
}

/// Why a call did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActError {
    /// BlueZ (or the bus) answered this D-Bus error name.
    Remote(String),
    /// BlueZ is not there, or the connection went: the action ends silently.
    Gone,
}

/// The agent object's registration (Task 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentId(pub u32);

/// The agent's answer to one of BlueZ's requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentReply {
    Ok,
    Rejected,
}

/// What BlueZ asks the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRequest {
    Cancel,
    Release,
}

/// What `BtAct::read` delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtEvent {
    Answer {
        call: ActCall,
        result: Result<(), ActError>,
    },
    Agent {
        id: AgentId,
        request: AgentRequest,
    },
    /// The connection went; every call in flight was answered `Gone` first.
    Closed,
}

/// Builds every call of this file: one `ActMethod`, auto-start off. None: libdbus refused a name
/// or a path (each is checked here first: dbus-rs's `From<&str>` conversions panic on an invalid one).
fn build(destination: &str, path: &str, method: ActMethod) -> Option<Message> {
    let destination = BusName::new(destination).ok()?;
    let path = Path::new(path).ok()?;
    let (interface, member) = (Interface::new(method.interface()).ok()?, Member::new(method.name()).ok()?);
    let mut m = Message::new_method_call(destination, path, interface, member).ok()?;
    m.set_auto_start(false);
    Some(m)
}

/// The bus driver's `GetNameOwner("org.bluez")`.
fn name_owner() -> Option<Message> {
    Some(build(BUS_DRIVER, BUS_DRIVER_PATH, ActMethod::GetNameOwner)?.append1(BLUEZ))
}

/// `call` as a message to `owner`, BlueZ's unique name. None: libdbus refused a path (never a panic).
pub fn message(call: &ActCall, owner: &str) -> Option<Message> {
    let set = |path: &str, interface: &str, property: &str, value: bool| {
        Some(build(owner, path, ActMethod::Set)?.append3(interface, property, Variant(value)))
    };
    match call {
        ActCall::Powered { adapter, on } => set(adapter, ADAPTER, "Powered", *on),
        ActCall::Trusted { device } => set(device, DEVICE, "Trusted", true),
        ActCall::RemoveDevice { adapter, device } => {
            let device = Path::new(device.as_str()).ok()?;
            Some(build(owner, adapter, ActMethod::RemoveDevice)?.append1(device))
        }
        ActCall::StartDiscovery { adapter } | ActCall::StopDiscovery { adapter } => build(owner, adapter, call.method()),
        ActCall::Connect { device } | ActCall::Disconnect { device } | ActCall::Pair { device } | ActCall::CancelPairing { device } => {
            build(owner, device, call.method())
        }
    }
}

/// What an error name means for the action: BlueZ or the connection gone, or a remote error kept
/// by name (`NoReply` included: a timeout shows `failed`; BlueZ leaving is its name owner change).
fn act_error(name: &str) -> ActError {
    match name {
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.Disconnected" => ActError::Gone,
        _ => ActError::Remote(name.to_string()),
    }
}

/// The reply to a method call made to this connection: a plain return, or the error `outcome`
/// names (a plain return only if libdbus refused that name; the names are constants).
fn answer(call: &Message, outcome: Result<(), &'static str>) -> Message {
    match outcome.err().and_then(|name| ErrorName::new(name).ok()) {
        Some(name) => call.error(&name, c"alpheus serves no such call"),
        None => call.method_return(),
    }
}

/// Who answers for `org.bluez`.
#[derive(Debug)]
enum Owner {
    /// `GetNameOwner` sent with this serial; calls wait in `queued`.
    Asking(u32),
    /// BlueZ's unique name: every call goes there.
    Known(String),
    /// No BlueZ on the bus: every call is answered `Gone`.
    Absent,
}

/// The acting connection (see the module's doc). Calls are queued in libdbus until `flush`;
/// answers come from `read`.
pub struct BtAct {
    channel: Channel,
    owner: Owner,
    /// Calls made while the owner was still asked for, in order.
    queued: Vec<ActCall>,
    /// Calls sent, by serial.
    pending: BTreeMap<u32, ActCall>,
    /// Events `read` delivers first.
    ready: Vec<BtEvent>,
    closed: bool,
}

impl BtAct {
    /// The system bus ($DBUS_SYSTEM_BUS_ADDRESS, else libdbus's default socket). Blocking connect
    /// and Hello, once.
    pub fn system() -> io::Result<BtAct> {
        BtAct::start(Channel::get_private(BusType::System).map_err(failed)?)
    }

    /// The bus at `address` (tests: a PrivateBus).
    pub fn at(address: &str) -> io::Result<BtAct> {
        let mut channel = Channel::open_private(address).map_err(failed)?;
        channel.register().map_err(failed)?;
        BtAct::start(channel)
    }

    fn start(mut channel: Channel) -> io::Result<BtAct> {
        channel.set_watch_enabled(true);
        let mut act =
            BtAct { channel, owner: Owner::Absent, queued: Vec::new(), pending: BTreeMap::new(), ready: Vec::new(), closed: false };
        let serial = name_owner().and_then(|m| act.post(m)).ok_or_else(|| io::Error::other("GetNameOwner not sent"))?;
        act.owner = Owner::Asking(serial);
        Ok(act)
    }

    /// A CLOEXEC duplicate of the connection's fd, for a calloop Generic source (READ, level).
    pub fn watch_fd(&mut self) -> io::Result<OwnedFd> {
        let fd = self.channel.watch().fd;
        // SAFETY: `fd` is libdbus's socket, open as long as `self.channel`
        // lives; it is only borrowed for the duplication (F_DUPFD_CLOEXEC).
        unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()
    }

    /// The only place a message leaves; Some(its serial) once libdbus queued it.
    fn post(&mut self, m: Message) -> Option<u32> {
        self.channel.send(m).ok()
    }

    /// Asks `call`: sent to BlueZ's unique name (queued while it is still asked for), or answered at
    /// the next `read` without a round-trip when there is no BlueZ, no connection or no valid path.
    /// `flush` writes it.
    pub fn call(&mut self, call: ActCall) {
        match &self.owner {
            _ if self.closed => self.ready.push(BtEvent::Answer { call, result: Err(ActError::Gone) }),
            Owner::Absent => self.ready.push(BtEvent::Answer { call, result: Err(ActError::Gone) }),
            Owner::Asking(_) => self.queued.push(call),
            Owner::Known(owner) => {
                let owner = owner.clone();
                self.dispatch(call, &owner);
            }
        }
    }

    /// Builds `call` for `owner` and posts it, kept by serial; an unbuildable one is answered at once.
    fn dispatch(&mut self, call: ActCall, owner: &str) {
        match message(&call, owner).map(|m| self.post(m)) {
            Some(Some(serial)) => {
                self.pending.insert(serial, call);
            }
            Some(None) => self.ready.push(BtEvent::Answer { call, result: Err(ActError::Gone) }),
            None => self.ready.push(BtEvent::Answer { call, result: Err(ActError::Remote(INVALID_ARGS.into())) }),
        }
    }

    /// Stops waiting for `call`: its answer, if it ever comes, is dropped (a queued one is never
    /// sent).
    pub fn forget(&mut self, call: &ActCall) {
        if let Some(serial) = self.pending.iter().find(|(_, c)| *c == call).map(|(s, _)| *s) {
            self.pending.remove(&serial);
        } else if let Some(i) = self.queued.iter().position(|c| c == call) {
            self.queued.remove(i);
        }
    }

    /// Reads without blocking (read_write(Some(0))) and drains every queued message. What `call`
    /// or `flush` left comes first.
    pub fn read(&mut self) -> Vec<BtEvent> {
        let mut out = std::mem::take(&mut self.ready);
        if self.closed {
            return out;
        }
        let lost = self.channel.read_write(Some(Duration::ZERO)).is_err();
        if self.drain(&mut out, lost) {
            self.settle(&mut out);
        }
        out
    }

    /// Writes libdbus's outgoing queue (blocking, as the tray's `SessionBus::flush`: only the bus
    /// daemon reads this socket). libdbus also reads then, so what arrived is drained into what the
    /// next `read` delivers: the caller must `read()` after it, the fd may no longer be readable.
    pub fn flush(&mut self) {
        if self.closed {
            return;
        }
        let mut out = Vec::new();
        self.settle(&mut out);
        self.ready.extend(out);
    }

    /// Flushes and drains until a drain posts nothing more, so no message is left in libdbus's
    /// queue behind a quiet fd.
    fn settle(&mut self, out: &mut Vec<BtEvent>) {
        while !self.closed {
            self.channel.flush();
            if !self.drain(out, false) {
                break;
            }
        }
    }

    /// Pops every message libdbus holds; true when a message was posted meanwhile (a queued call
    /// sent, a refusal). A lost connection answers every call in flight `Gone`, then `Closed`.
    fn drain(&mut self, out: &mut Vec<BtEvent>, mut lost: bool) -> bool {
        let mut posted = false;
        while let Some(mut m) = self.channel.pop_message() {
            match m.msg_type() {
                MessageType::MethodReturn | MessageType::Error => {
                    let serial = m.get_reply_serial();
                    if matches!(self.owner, Owner::Asking(s) if Some(s) == serial) {
                        posted |= self.owned(&m, out);
                    } else if let Some(call) = serial.and_then(|s| self.pending.remove(&s)) {
                        let result = match m.as_result() {
                            Ok(_) => Ok(()),
                            Err(e) => Err(act_error(e.name().unwrap_or(""))),
                        };
                        out.push(BtEvent::Answer { call, result });
                    }
                }
                MessageType::MethodCall => {
                    if !m.get_no_reply() {
                        let refusal = answer(&m, Err(UNKNOWN_METHOD));
                        posted |= self.post(refusal).is_some();
                    }
                }
                MessageType::Signal if m.interface().is_some_and(|i| &*i == LOCAL) => lost = true,
                MessageType::Signal => {}
            }
        }
        if lost || !self.channel.is_connected() {
            self.closed = true;
            let pending = std::mem::take(&mut self.pending).into_values();
            for call in pending.chain(std::mem::take(&mut self.queued)) {
                out.push(BtEvent::Answer { call, result: Err(ActError::Gone) });
            }
            out.push(BtEvent::Closed);
        }
        posted
    }

    /// `GetNameOwner`'s answer: the queued calls sent to the owner in order, or all answered `Gone`.
    /// True when one was posted.
    fn owned(&mut self, reply: &Message, out: &mut Vec<BtEvent>) -> bool {
        let owner = match reply.msg_type() {
            MessageType::MethodReturn => reply.read1::<&str>().ok().map(String::from),
            _ => None,
        };
        let queued = std::mem::take(&mut self.queued);
        match owner {
            Some(owner) => {
                self.owner = Owner::Known(owner.clone());
                let sent = !queued.is_empty();
                for call in queued {
                    self.dispatch(call, &owner);
                }
                out.append(&mut self.ready);
                sent
            }
            None => {
                self.owner = Owner::Absent;
                out.extend(queued.into_iter().map(|call| BtEvent::Answer { call, result: Err(ActError::Gone) }));
                false
            }
        }
    }

    /// Calls waiting for an answer, queued ones included (`GetNameOwner` is not one).
    pub fn in_flight(&self) -> usize {
        self.queued.len() + self.pending.len()
    }

    /// Writes what is queued, then closes the connection.
    pub fn close(mut self) {
        self.flush();
    }
}

/// A connection error as an io::Error naming it.
fn failed(e: dbus::Error) -> io::Error {
    io::Error::other(e.name().unwrap_or("org.freedesktop.DBus.Error.Failed").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::arg::ArgType;

    const OWNER: &str = ":1.42";
    const HCI: &str = "/org/bluez/hci0";
    const DEV: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";

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
    fn message_shapes() {
        let props = "org.freedesktop.DBus.Properties";
        let rows: [(ActCall, &str, &str, &str, &[&str]); 9] = [
            (ActCall::Powered { adapter: HCI.into(), on: true }, HCI, props, "Set", &["s", "s", "v"]),
            (ActCall::Trusted { device: DEV.into() }, DEV, props, "Set", &["s", "s", "v"]),
            (ActCall::StartDiscovery { adapter: HCI.into() }, HCI, "org.bluez.Adapter1", "StartDiscovery", &[]),
            (ActCall::StopDiscovery { adapter: HCI.into() }, HCI, "org.bluez.Adapter1", "StopDiscovery", &[]),
            (ActCall::RemoveDevice { adapter: HCI.into(), device: DEV.into() }, HCI, "org.bluez.Adapter1", "RemoveDevice", &["o"]),
            (ActCall::Connect { device: DEV.into() }, DEV, "org.bluez.Device1", "Connect", &[]),
            (ActCall::Disconnect { device: DEV.into() }, DEV, "org.bluez.Device1", "Disconnect", &[]),
            (ActCall::Pair { device: DEV.into() }, DEV, "org.bluez.Device1", "Pair", &[]),
            (ActCall::CancelPairing { device: DEV.into() }, DEV, "org.bluez.Device1", "CancelPairing", &[]),
        ];
        for (call, path, interface, member, sig) in rows {
            let m = message(&call, OWNER).unwrap();
            assert_eq!(
                (m.destination().as_deref(), m.path().as_deref(), m.interface().as_deref(), m.member().as_deref()),
                (Some(OWNER), Some(path), Some(interface), Some(member)),
                "{call:?}"
            );
            assert_eq!(signatures(&m), sig, "{call:?}");
            assert!(!m.get_auto_start(), "{call:?}: never starts BlueZ");
            assert!(!m.get_no_reply(), "{call:?}: its answer is read");
            assert_eq!(call.method().name(), member);
        }
        let off = message(&ActCall::Powered { adapter: HCI.into(), on: false }, OWNER).unwrap();
        let (i, p, v) = off.read3::<&str, &str, Variant<bool>>().unwrap();
        assert_eq!((i, p, v.0), ("org.bluez.Adapter1", "Powered", false));
        let trusted = message(&ActCall::Trusted { device: DEV.into() }, OWNER).unwrap();
        let (i, p, v) = trusted.read3::<&str, &str, Variant<bool>>().unwrap();
        assert_eq!((i, p, v.0), ("org.bluez.Device1", "Trusted", true));
        let removed = message(&ActCall::RemoveDevice { adapter: HCI.into(), device: DEV.into() }, OWNER).unwrap();
        assert_eq!(&*removed.read1::<dbus::Path>().unwrap(), DEV);
        assert!(message(&ActCall::Connect { device: "not a path".into() }, OWNER).is_none(), "refused, no panic");
    }

    #[test]
    fn act_method_names_pinned() {
        let names: Vec<&str> = ActMethod::ALL.iter().map(|m| m.name()).collect();
        assert_eq!(
            names,
            [
                "GetNameOwner",
                "Set",
                "StartDiscovery",
                "StopDiscovery",
                "RemoveDevice",
                "Connect",
                "Disconnect",
                "Pair",
                "CancelPairing",
                "RegisterAgent",
                "UnregisterAgent"
            ]
        );
        let interfaces: Vec<&str> = ActMethod::ALL.iter().map(|m| m.interface()).collect();
        assert_eq!(
            interfaces,
            [
                "org.freedesktop.DBus",
                "org.freedesktop.DBus.Properties",
                "org.bluez.Adapter1",
                "org.bluez.Adapter1",
                "org.bluez.Adapter1",
                "org.bluez.Device1",
                "org.bluez.Device1",
                "org.bluez.Device1",
                "org.bluez.Device1",
                "org.bluez.AgentManager1",
                "org.bluez.AgentManager1"
            ]
        );
        let d = || DEV.to_string();
        let words: Vec<&str> = [
            ActCall::Powered { adapter: HCI.into(), on: true },
            ActCall::Trusted { device: d() },
            ActCall::StartDiscovery { adapter: HCI.into() },
            ActCall::StopDiscovery { adapter: HCI.into() },
            ActCall::RemoveDevice { adapter: HCI.into(), device: d() },
            ActCall::Connect { device: d() },
            ActCall::Disconnect { device: d() },
            ActCall::Pair { device: d() },
            ActCall::CancelPairing { device: d() },
        ]
        .iter()
        .map(ActCall::word)
        .collect();
        assert_eq!(words, ["power", "trust", "discovery", "discovery stop", "forget", "connect", "disconnect", "pair", "cancel pairing"]);
    }

    #[test]
    fn gone_is_told_from_a_remote_error() {
        for gone in ["ServiceUnknown", "NameHasNoOwner", "Disconnected"] {
            assert_eq!(act_error(&format!("org.freedesktop.DBus.Error.{gone}")), ActError::Gone, "{gone}");
        }
        for kept in [
            "org.freedesktop.DBus.Error.NoReply",
            "org.bluez.Error.AlreadyConnected",
            "org.bluez.Error.Failed",
            "org.freedesktop.DBus.Error.AccessDenied",
            "org.freedesktop.DBus.Error.UnknownObject",
        ] {
            assert_eq!(act_error(kept), ActError::Remote(kept.into()));
        }
    }

    #[test]
    fn answer_names_the_error() {
        let mut call = Message::new_method_call(":1.99", "/x", "org.example", "Do").unwrap();
        call.set_serial(41);
        let mut r = answer(&call, Err("org.freedesktop.DBus.Error.UnknownMethod"));
        assert_eq!((r.msg_type(), r.get_reply_serial()), (MessageType::Error, Some(41)));
        assert_eq!(r.as_result().unwrap_err().name(), Some("org.freedesktop.DBus.Error.UnknownMethod"));
        assert_eq!(answer(&call, Ok(())).msg_type(), MessageType::MethodReturn);
    }
}
