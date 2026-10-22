//! Bluetooth's acting connection (quick settings delivery 3, spec § 5): the only place alpheus
//! changes anything on BlueZ. Its own private connection to the system bus, never blocking after
//! the connect, opened only while the panel's Bluetooth detail is open or a call is in flight. Every
//! call is a typed `ActCall`, built in ONE place (`build`) from the pinned `ActMethod` list, sent to
//! `org.bluez`'s unique name (read first with `GetNameOwner`, calls queued meanwhile) with
//! auto-start off: nothing outside the list can reach BlueZ, and no call can start it. Answers are
//! matched by reply serial; the read connection (`bt.rs`) stays read only.

use crate::pipewire::drawable;
use dbus::Message;
use dbus::arg::Variant;
use dbus::channel::{BusType, Channel};
use dbus::message::MessageType;
use dbus::strings::{BusName, ErrorName, Interface, Member, Path};
use std::collections::{BTreeMap, VecDeque};
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
/// Where `AgentManager1` lives.
const AGENT_MANAGER_PATH: &str = "/org/bluez";
/// The one object this connection serves (spec § 3), to BlueZ's unique name only.
pub const AGENT_PATH: &str = "/org/alpheus/bluetooth/agent";
/// The agent's IO capability: it shows a code and confirms one, never types one in.
pub const CAPABILITY: &str = "KeyboardDisplay";
/// The one interface served, at AGENT_PATH.
const AGENT_INTERFACE: &str = "org.bluez.Agent1";
/// What any sender but BlueZ's unique name gets: it learns nothing, not even what is served.
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";
/// What BlueZ gets for any other path, interface or member.
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
/// The agent's refusal (a code declined, a passkey out of range, a held call evicted or closed).
const REJECTED: &str = "org.bluez.Error.Rejected";
/// Agent calls waiting for the panel's answer; beyond it the oldest is rejected.
const MAX_HELD: usize = 4;
/// A legacy PIN, in characters, once cleaned.
const MAX_PIN: usize = 16;
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

/// An agent call waiting for `BtAct::reply`: the incoming call's serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentId(pub u32);

/// The agent's answer to one of BlueZ's requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentReply {
    Ok,
    Rejected,
}

/// What BlueZ asks the agent (`org.bluez.Agent1`); `device` is an object path (never logged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRequest {
    DisplayPasskey { device: String, passkey: u32 },
    DisplayPinCode { device: String, pincode: String },
    RequestConfirmation { device: String, passkey: u32 },
    RequestAuthorization { device: String },
    RequestPinCode { device: String },
    RequestPasskey { device: String },
    AuthorizeService { device: String },
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

/// An agent call, read with its member's exact signature; None for any other member or a
/// signature that does not match. A PIN is cleaned (`drawable`) and capped at MAX_PIN characters.
pub(crate) fn agent_request(m: &Message) -> Option<AgentRequest> {
    use AgentRequest::*;
    let device = |p: Path| p.to_string();
    Some(match &*m.member()? {
        "DisplayPasskey" => {
            let (d, passkey, _entered) = m.read3::<Path, u32, u16>().ok()?;
            DisplayPasskey { device: device(d), passkey }
        }
        "DisplayPinCode" => {
            let (d, pin) = m.read2::<Path, &str>().ok()?;
            DisplayPinCode { device: device(d), pincode: drawable(pin).chars().take(MAX_PIN).collect() }
        }
        "RequestConfirmation" => {
            let (d, passkey) = m.read2::<Path, u32>().ok()?;
            RequestConfirmation { device: device(d), passkey }
        }
        "RequestAuthorization" => RequestAuthorization { device: device(m.read1::<Path>().ok()?) },
        "RequestPinCode" => RequestPinCode { device: device(m.read1::<Path>().ok()?) },
        "RequestPasskey" => RequestPasskey { device: device(m.read1::<Path>().ok()?) },
        "AuthorizeService" => AuthorizeService { device: device(m.read2::<Path, &str>().ok()?.0) },
        "Cancel" => Cancel,
        "Release" => Release,
        _ => return None,
    })
}

/// What becomes of a method call made to this connection.
#[derive(Debug, PartialEq, Eq)]
enum Screen {
    /// Held until the panel's `reply`.
    Deliver(AgentRequest),
    /// Answered at once, then told to the panel.
    AckAndDeliver(AgentRequest),
    /// Answered with this error name; the panel never hears of it.
    Refuse(&'static str),
}

/// Sorts an incoming call: the sender first (BlueZ's unique name, `owner`, or ACCESS_DENIED: no
/// owner known means nobody is BlueZ), then the path and interface, the member and its signature
/// (UNKNOWN_METHOD), then the passkey's six digits (REJECTED).
fn screen(m: &Message, owner: Option<&str>) -> Screen {
    let sender = m.sender();
    if owner.is_none() || sender.as_deref() != owner {
        return Screen::Refuse(ACCESS_DENIED);
    }
    if m.path().as_deref() != Some(AGENT_PATH) || m.interface().as_deref() != Some(AGENT_INTERFACE) {
        return Screen::Refuse(UNKNOWN_METHOD);
    }
    match agent_request(m) {
        None => Screen::Refuse(UNKNOWN_METHOD),
        Some(AgentRequest::DisplayPasskey { passkey, .. } | AgentRequest::RequestConfirmation { passkey, .. }) if passkey > 999_999 => {
            Screen::Refuse(REJECTED)
        }
        Some(r @ (AgentRequest::Cancel | AgentRequest::Release)) => Screen::AckAndDeliver(r),
        Some(r) => Screen::Deliver(r),
    }
}

/// Keeps `m` for its answer; beyond MAX_HELD the oldest comes back, to be rejected.
fn hold(held: &mut VecDeque<(AgentId, Message)>, id: AgentId, m: Message) -> Option<Message> {
    held.push_back((id, m));
    if held.len() > MAX_HELD { held.pop_front().map(|(_, m)| m) } else { None }
}

/// Removes the held call `id`: None when unknown or already answered.
fn take(held: &mut VecDeque<(AgentId, Message)>, id: AgentId) -> Option<Message> {
    let i = held.iter().position(|(h, _)| *h == id)?;
    held.remove(i).map(|(_, m)| m)
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
    /// Agent calls waiting for `reply`, oldest first (at most MAX_HELD).
    held: VecDeque<(AgentId, Message)>,
    /// `RegisterAgent` was sent: `close` unregisters.
    registered: bool,
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
        let mut act = BtAct {
            channel,
            owner: Owner::Absent,
            queued: Vec::new(),
            pending: BTreeMap::new(),
            ready: Vec::new(),
            held: VecDeque::new(),
            registered: false,
            closed: false,
        };
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
                MessageType::MethodCall => posted |= self.agent_call(m, out),
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
                // First: one connection's messages are handled in order, so BlueZ has the agent
                // before any `Pair`.
                self.registered = self.agent_manager(&owner, ActMethod::RegisterAgent);
                let sent = self.registered || !queued.is_empty();
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

    /// The panel's answer to the held agent call `id`; an unknown (or already answered) `id` does
    /// nothing. `flush` writes it.
    pub fn reply(&mut self, id: AgentId, reply: AgentReply) {
        let Some(m) = take(&mut self.held, id) else { return };
        let outcome = match reply {
            AgentReply::Ok => Ok(()),
            AgentReply::Rejected => Err(REJECTED),
        };
        self.respond(&m, outcome);
    }

    /// Posts `answer(m, outcome)` unless `m` expects no reply; true when posted.
    fn respond(&mut self, m: &Message, outcome: Result<(), &'static str>) -> bool {
        !m.get_no_reply() && self.post(answer(m, outcome)).is_some()
    }

    /// `AgentManager1.RegisterAgent` or `UnregisterAgent` for AGENT_PATH, to `owner`; no reply
    /// expected (a refused registration shows up as a failed `Pair`). True when posted.
    fn agent_manager(&mut self, owner: &str, method: ActMethod) -> bool {
        let Ok(agent) = Path::new(AGENT_PATH) else { return false };
        let Some(m) = build(owner, AGENT_MANAGER_PATH, method) else { return false };
        let mut m = if method == ActMethod::RegisterAgent { m.append2(agent, CAPABILITY) } else { m.append1(agent) };
        m.set_no_reply(true);
        self.post(m).is_some()
    }

    /// An agent call: screened; refused, acknowledged and told, or held and told.
    fn agent_call(&mut self, m: Message, out: &mut Vec<BtEvent>) -> bool {
        let owner = match &self.owner {
            Owner::Known(owner) => Some(owner.clone()),
            _ => None,
        };
        let Some(serial) = m.get_serial() else { return false };
        let id = AgentId(serial);
        match screen(&m, owner.as_deref()) {
            Screen::Refuse(name) => self.respond(&m, Err(name)),
            Screen::AckAndDeliver(request) => {
                let posted = self.respond(&m, Ok(()));
                out.push(BtEvent::Agent { id, request });
                posted
            }
            Screen::Deliver(request) => {
                let posted = match hold(&mut self.held, id, m) {
                    Some(evicted) => self.respond(&evicted, Err(REJECTED)),
                    None => false,
                };
                out.push(BtEvent::Agent { id, request });
                posted
            }
        }
    }

    /// Rejects every held agent call, unregisters the agent, writes what is queued, then closes
    /// the connection.
    pub fn close(mut self) {
        for (_, m) in std::mem::take(&mut self.held) {
            self.respond(&m, Err(REJECTED));
        }
        if let (true, Owner::Known(owner)) = (self.registered, &self.owner) {
            let owner = owner.clone();
            self.agent_manager(&owner, ActMethod::UnregisterAgent);
        }
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

    const BLUEZ: &str = ":1.7";

    fn agent_call(member: &str) -> Message {
        let mut m = Message::new_method_call(":1.99", AGENT_PATH, "org.bluez.Agent1", member).unwrap();
        m.set_sender(Some(BLUEZ.into()));
        m.set_serial(41);
        m
    }

    fn dev() -> dbus::Path<'static> {
        dbus::Path::from(DEV)
    }

    #[test]
    fn agent_request_decodes_each_member() {
        use AgentRequest::*;
        let d = || DEV.to_string();
        let rows: Vec<(Message, AgentRequest)> = vec![
            (agent_call("DisplayPasskey").append3(dev(), 482_913u32, 0u16), DisplayPasskey { device: d(), passkey: 482_913 }),
            (agent_call("DisplayPinCode").append2(dev(), "0000"), DisplayPinCode { device: d(), pincode: "0000".into() }),
            (agent_call("RequestConfirmation").append2(dev(), 7u32), RequestConfirmation { device: d(), passkey: 7 }),
            (agent_call("RequestAuthorization").append1(dev()), RequestAuthorization { device: d() }),
            (agent_call("RequestPinCode").append1(dev()), RequestPinCode { device: d() }),
            (agent_call("RequestPasskey").append1(dev()), RequestPasskey { device: d() }),
            (agent_call("AuthorizeService").append2(dev(), "0000110b-0000-1000-8000-00805f9b34fb"), AuthorizeService { device: d() }),
            (agent_call("Cancel"), Cancel),
            (agent_call("Release"), Release),
        ];
        for (m, want) in rows {
            assert_eq!(agent_request(&m), Some(want));
        }
        assert_eq!(agent_request(&agent_call("DisplayPasskey").append1(dev())), None, "missing arguments");
        assert_eq!(agent_request(&agent_call("RequestConfirmation").append2("not o", 7u32)), None, "wrong signature");
        assert_eq!(agent_request(&agent_call("Explode")), None);
    }

    #[test]
    fn screen_answers_only_bluez_at_the_agent_path() {
        let owner = Some(BLUEZ);
        let confirm = || agent_call("RequestConfirmation").append2(dev(), 482_913u32);
        assert_eq!(screen(&confirm(), owner), Screen::Deliver(AgentRequest::RequestConfirmation { device: DEV.into(), passkey: 482_913 }));
        assert_eq!(screen(&agent_call("Cancel"), owner), Screen::AckAndDeliver(AgentRequest::Cancel));
        assert_eq!(screen(&agent_call("Release"), owner), Screen::AckAndDeliver(AgentRequest::Release));
        let mut stranger = confirm();
        stranger.set_sender(Some(":1.100".into()));
        assert_eq!(screen(&stranger, owner), Screen::Refuse(ACCESS_DENIED));
        assert_eq!(screen(&confirm(), None), Screen::Refuse(ACCESS_DENIED), "owner unknown: nobody is BlueZ");
        let mut elsewhere = Message::new_method_call(":1.99", "/org/alpheus/other", "org.bluez.Agent1", "Cancel").unwrap();
        elsewhere.set_sender(Some(BLUEZ.into()));
        assert_eq!(screen(&elsewhere, owner), Screen::Refuse(UNKNOWN_METHOD));
        let mut introspect = Message::new_method_call(":1.99", AGENT_PATH, "org.freedesktop.DBus.Introspectable", "Introspect").unwrap();
        introspect.set_sender(Some(BLUEZ.into()));
        assert_eq!(screen(&introspect, owner), Screen::Refuse(UNKNOWN_METHOD));
        assert_eq!(screen(&agent_call("Explode"), owner), Screen::Refuse(UNKNOWN_METHOD));
    }

    #[test]
    fn bad_passkey_rejected() {
        let owner = Some(BLUEZ);
        assert_eq!(screen(&agent_call("DisplayPasskey").append3(dev(), 1_000_000u32, 0u16), owner), Screen::Refuse(REJECTED));
        assert_eq!(screen(&agent_call("RequestConfirmation").append2(dev(), 1_000_000u32), owner), Screen::Refuse(REJECTED));
        assert!(matches!(screen(&agent_call("RequestConfirmation").append2(dev(), 999_999u32), owner), Screen::Deliver(_)));
    }

    #[test]
    fn pin_capped_and_cleaned() {
        let m = agent_call("DisplayPinCode").append2(dev(), "\u{202e}12\u{7}34567890123456789XYZ");
        assert_eq!(agent_request(&m), Some(AgentRequest::DisplayPinCode { device: DEV.into(), pincode: "1234567890123456".into() }));
    }

    #[test]
    fn a_held_call_is_taken_once() {
        let mut held = VecDeque::new();
        hold(&mut held, AgentId(1), agent_call("Cancel"));
        hold(&mut held, AgentId(2), agent_call("Cancel"));
        assert!(take(&mut held, AgentId(1)).is_some());
        assert!(take(&mut held, AgentId(1)).is_none(), "already answered");
        assert!(take(&mut held, AgentId(9)).is_none(), "unknown");
        assert_eq!(held.iter().map(|(id, _)| id.0).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn at_most_four_agent_calls_are_held() {
        let mut held = VecDeque::new();
        for serial in 1..=4 {
            assert!(hold(&mut held, AgentId(serial), agent_call("Cancel")).is_none());
        }
        let evicted = hold(&mut held, AgentId(5), agent_call("Cancel"));
        assert!(evicted.is_some());
        assert_eq!(held.iter().map(|(id, _)| id.0).collect::<Vec<_>>(), [2, 3, 4, 5]);
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
