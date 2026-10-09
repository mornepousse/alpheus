//! A fake notification client and a fake mako on a `PrivateBus`, for the tests only (notifications
//! plan task 4). Raw D-Bus, never the daemon's code: the names are written out here on purpose, as a
//! cross-check of the daemon's constants. Fictional applications and texts only.
//!
//! `FakeClient` is a plain `Peer` driven by the test thread: blocking calls (`Notify`,
//! `CloseNotification`, `GetCapabilities`, `GetServerInformation`), raw sends for bursts, and the
//! two broadcast signals it hears (`NotificationClosed`, `ActionInvoked`), kept in arrival order.
//!
//! `FakeMako` owns `org.freedesktop.Notifications` with flags 0, as mako does, on its own thread.
//! It answers like a server (its ids start at 1000, so a test can tell who served), records every
//! call, and on command gives the name up (`ReleaseName`) or disconnects (`kill`). `FakeMako::queue`
//! starts one that waits behind whoever holds the name: mako restarting while alpheus holds it.

use super::{Peer, PrivateBus};
use dbus::Message;
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::message::MessageType;
use dbus::strings::{BusName, ErrorName};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const INTERFACE: &str = "org.freedesktop.Notifications";
const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
/// How long a fake waits on its socket before looking at its commands (as support/portal.rs).
const POLL: Duration = Duration::from_millis(20);
/// FakeMako's first id: far above anything alpheus hands out in these tests.
pub const MAKO_FIRST_ID: u32 = 1000;

/// One hint value, in the type a client sends it as.
#[derive(Debug, Clone)]
pub enum Hint {
    Text(String),
    Byte(u8),
    U32(u32),
    I32(i32),
    Bool(bool),
    /// `image-data` `(iiibiiay)` with this many bytes of pixels: the server must skip it unread.
    Image(usize),
}

/// One `Notify` call's arguments.
#[derive(Debug, Clone)]
pub struct Note {
    pub app: String,
    pub replaces: u32,
    pub summary: String,
    pub body: String,
    /// (key, label) pairs, sent flattened as the spec's `as`.
    pub actions: Vec<(String, String)>,
    pub hints: Vec<(String, Hint)>,
    pub timeout: i32,
}

impl Note {
    pub fn new(app: &str, summary: &str) -> Note {
        Note {
            app: app.into(),
            replaces: 0,
            summary: summary.into(),
            body: String::new(),
            actions: Vec::new(),
            hints: Vec::new(),
            timeout: -1,
        }
    }

    pub fn body(mut self, body: &str) -> Note {
        self.body = body.into();
        self
    }

    pub fn replaces(mut self, id: u32) -> Note {
        self.replaces = id;
        self
    }

    pub fn action(mut self, key: &str, label: &str) -> Note {
        self.actions.push((key.into(), label.into()));
        self
    }

    pub fn hint(mut self, key: &str, value: Hint) -> Note {
        self.hints.push((key.into(), value));
        self
    }

    pub fn timeout(mut self, ms: i32) -> Note {
        self.timeout = ms;
        self
    }

    /// `urgency` 2 as a byte, the spec's type.
    pub fn critical(self) -> Note {
        self.hint("urgency", Hint::Byte(2))
    }

    /// `Notify(susssasa{sv}i)` to `destination` (the well-known name or a unique one).
    pub fn message(&self, destination: &str) -> Message {
        let actions: Vec<&str> = self.actions.iter().flat_map(|(k, l)| [k.as_str(), l.as_str()]).collect();
        let mut hints = PropMap::new();
        for (key, value) in &self.hints {
            let v: Box<dyn RefArg> = match value {
                Hint::Text(s) => Box::new(s.clone()),
                Hint::Byte(b) => Box::new(*b),
                Hint::U32(n) => Box::new(*n),
                Hint::I32(n) => Box::new(*n),
                Hint::Bool(b) => Box::new(*b),
                Hint::Image(bytes) => Box::new((1i32, 1i32, 4i32, true, 8i32, 4i32, vec![0u8; *bytes])),
            };
            hints.insert(key.clone(), Variant(v));
        }
        Message::new_method_call(destination, PATH, INTERFACE, "Notify")
            .unwrap()
            .append3(self.app.as_str(), self.replaces, "")
            .append3(self.summary.as_str(), self.body.as_str(), actions)
            .append2(hints, self.timeout)
    }
}

/// A signal a client heard on the Notifications interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// NotificationClosed(id, reason).
    Closed(u32, u32),
    /// ActionInvoked(id, key).
    Invoked(u32, String),
}

fn method(destination: &str, member: &str) -> Message {
    Message::new_method_call(destination, PATH, INTERFACE, member).unwrap()
}

pub struct FakeClient {
    pub peer: Peer,
    heard: RefCell<Vec<Heard>>,
}

impl FakeClient {
    /// Connects and listens to both broadcast signals (one rule on the interface and path).
    pub fn connect(bus: &PrivateBus) -> FakeClient {
        let peer = Peer::connect(bus);
        let rule = format!("type='signal',path='{PATH}',interface='{INTERFACE}'");
        peer.call(Message::new_method_call(DBUS, DBUS_PATH, DBUS, "AddMatch").unwrap().append1(rule)).unwrap();
        FakeClient { peer, heard: RefCell::new(Vec::new()) }
    }

    /// A blocking call (5 s at most): the reply, or the error's name. Signals that arrived meanwhile
    /// are kept (libdbus leaves them queued).
    fn call(&self, m: Message) -> Result<Message, String> {
        let r = self.peer.call(m).map_err(|e| e.name().unwrap_or("").to_string());
        self.listen(Duration::ZERO);
        r
    }

    pub fn notify(&self, n: &Note) -> Result<u32, String> {
        self.notify_at(NAME, n)
    }

    pub fn notify_at(&self, destination: &str, n: &Note) -> Result<u32, String> {
        self.call(n.message(destination)).and_then(|r| r.read1::<u32>().map_err(|e| format!("{e:?}")))
    }

    /// CloseNotification(id): the number of arguments in the reply (0: the spec's empty reply).
    pub fn close(&self, id: u32) -> Result<usize, String> {
        self.call(method(NAME, "CloseNotification").append1(id)).map(|r| r.iter_init().count())
    }

    pub fn capabilities(&self) -> Result<Vec<String>, String> {
        self.capabilities_at(NAME)
    }

    pub fn capabilities_at(&self, destination: &str) -> Result<Vec<String>, String> {
        self.call(method(destination, "GetCapabilities")).and_then(|r| r.read1::<Vec<String>>().map_err(|e| format!("{e:?}")))
    }

    pub fn server_information(&self) -> Result<(String, String, String, String), String> {
        self.call(method(NAME, "GetServerInformation"))
            .and_then(|r| r.read4::<String, String, String, String>().map_err(|e| format!("{e:?}")))
    }

    /// Who owns `name` now (GetNameOwner), None when nobody does.
    pub fn owner(&self, name: &str) -> Option<String> {
        let m = Message::new_method_call(DBUS, DBUS_PATH, DBUS, "GetNameOwner").unwrap().append1(name);
        self.peer.call(m).ok().and_then(|r| r.read1::<String>().ok())
    }

    /// Sends without waiting: the call's serial, for a burst read back with `replies`.
    pub fn send(&self, m: Message) -> u32 {
        let serial = self.peer.channel.send(m).unwrap();
        self.peer.channel.flush();
        serial
    }

    /// The answers to `serials`, in their order (5 s at most): the reply, or the error's name.
    pub fn replies(&self, serials: &[u32]) -> Vec<Result<Message, String>> {
        let mut got: HashMap<u32, Message> = HashMap::new();
        let end = Instant::now() + Duration::from_secs(5);
        while got.len() < serials.len() && Instant::now() < end {
            if let Ok(Some(m)) = self.peer.channel.blocking_pop_message(POLL) {
                match m.get_reply_serial() {
                    Some(s) if serials.contains(&s) => {
                        got.insert(s, m);
                    }
                    _ => self.keep(&m),
                }
            }
        }
        serials
            .iter()
            .map(|s| match got.remove(s) {
                None => Err(format!("no answer to call {s}")),
                Some(mut m) => {
                    let failed = m.as_result().err().map(|e| e.name().unwrap_or("").to_string());
                    match failed {
                        Some(name) => Err(name),
                        None => Ok(m),
                    }
                }
            })
            .collect()
    }

    /// A stranger's copy of the bus driver's `member` (NameAcquired or NameLost) for our name:
    /// once to `to` only, once broadcast. The bus stamps it with the stranger's unique name.
    pub fn forge(&self, member: &str, to: &str) {
        let mut m = Message::new_signal(DBUS_PATH, DBUS, member).unwrap().append1(NAME);
        m.set_destination(Some(BusName::new(to.to_string()).unwrap()));
        self.peer.channel.send(m).unwrap();
        self.peer.channel.send(Message::new_signal(DBUS_PATH, DBUS, member).unwrap().append1(NAME)).unwrap();
        self.peer.channel.flush();
    }

    fn keep(&self, m: &Message) {
        if m.msg_type() != MessageType::Signal || m.interface().as_deref() != Some(INTERFACE) {
            return;
        }
        let heard = match m.member().as_deref() {
            Some("NotificationClosed") => m.read2::<u32, u32>().ok().map(|(id, reason)| Heard::Closed(id, reason)),
            Some("ActionInvoked") => m.read2::<u32, String>().ok().map(|(id, key)| Heard::Invoked(id, key)),
            _ => None,
        };
        self.heard.borrow_mut().extend(heard);
    }

    /// Reads the socket for `d` (at least everything already queued), keeping every signal heard.
    fn listen(&self, d: Duration) {
        let end = Instant::now() + d;
        loop {
            let wait = if d.is_zero() { Duration::ZERO } else { POLL };
            if let Ok(Some(m)) = self.peer.channel.blocking_pop_message(wait) {
                self.keep(&m);
                continue;
            }
            if Instant::now() >= end {
                return;
            }
        }
    }

    /// Waits (5 s at most) until `want` was heard; true if it was. It is taken from the list.
    pub fn wait_heard(&self, want: &Heard) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let mut heard = self.heard.borrow_mut();
                if let Some(i) = heard.iter().position(|h| h == want) {
                    heard.remove(i);
                    return true;
                }
            }
            if Instant::now() >= end {
                return false;
            }
            self.listen(POLL);
        }
    }

    /// Everything heard so far and within `d`, taken.
    pub fn heard_within(&self, d: Duration) -> Vec<Heard> {
        self.listen(d);
        std::mem::take(&mut *self.heard.borrow_mut())
    }
}

enum Command {
    Release,
    Stop,
}

pub struct FakeMako {
    /// Its unique name.
    pub name: String,
    commands: Sender<Command>,
    calls: Arc<Mutex<Vec<String>>>,
    holds: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeMako {
    /// Requests the name with flags 0, as mako does; nobody holds it, so the reply must be 1.
    pub fn start(bus: &PrivateBus) -> FakeMako {
        Self::spawn(bus, 1)
    }

    /// Requests the name while alpheus holds it: the reply must be 2 (queued). It serves once the bus
    /// hands it the name (NameAcquired).
    pub fn queue(bus: &PrivateBus) -> FakeMako {
        Self::spawn(bus, 2)
    }

    fn spawn(bus: &PrivateBus, expected: u32) -> FakeMako {
        let peer = Peer::connect(bus);
        let ask = Message::new_method_call(DBUS, DBUS_PATH, DBUS, "RequestName").unwrap().append2(NAME, 0u32);
        assert_eq!(peer.call(ask).unwrap().read1::<u32>().unwrap(), expected, "RequestName({NAME}, 0)");
        let name = peer.name.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let holds = Arc::new(AtomicBool::new(expected == 1));
        let (commands, rx) = mpsc::channel();
        let (c, h) = (calls.clone(), holds.clone());
        let thread = thread::spawn(move || serve(peer, c, h, rx));
        FakeMako { name, commands, calls, holds, thread: Some(thread) }
    }

    /// `systemctl --user stop mako` without disconnecting: ReleaseName.
    pub fn release(&self) {
        let _ = self.commands.send(Command::Release);
    }

    /// Whether it holds the name now.
    pub fn holds(&self) -> bool {
        self.holds.load(Ordering::SeqCst)
    }

    /// Waits until it holds the name; true if it does within `timeout`.
    pub fn wait_holds(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while !self.holds() {
            if Instant::now() >= end {
                return false;
            }
            thread::sleep(POLL);
        }
        true
    }

    /// Every call it received, "Member(summary)" for Notify, "Member()" otherwise.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Disconnects (mako exiting); what it received.
    pub fn kill(mut self) -> Vec<String> {
        self.stop();
        self.calls()
    }

    fn stop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FakeMako {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(peer: Peer, calls: Arc<Mutex<Vec<String>>>, holds: Arc<AtomicBool>, commands: Receiver<Command>) {
    let mut next = MAKO_FIRST_ID;
    loop {
        while let Ok(c) = commands.try_recv() {
            match c {
                Command::Release => {
                    let m = Message::new_method_call(DBUS, DBUS_PATH, DBUS, "ReleaseName").unwrap().append1(NAME);
                    // 1: released.
                    if peer.call(m).ok().and_then(|r| r.read1::<u32>().ok()) == Some(1) {
                        holds.store(false, Ordering::SeqCst);
                    }
                }
                Command::Stop => return,
            }
        }
        match peer.channel.blocking_pop_message(POLL) {
            Ok(Some(m)) if m.msg_type() == MessageType::MethodCall => {
                let member = m.member().map(|x| x.to_string()).unwrap_or_default();
                let reply = match (m.interface().as_deref(), member.as_str()) {
                    (Some(INTERFACE), "Notify") => {
                        let summary = m.read4::<&str, u32, &str, &str>().map(|t| t.3.to_string()).unwrap_or_default();
                        calls.lock().unwrap().push(format!("Notify({summary})"));
                        next += 1;
                        m.method_return().append1(next - 1)
                    }
                    (Some(INTERFACE), "GetServerInformation") => {
                        calls.lock().unwrap().push("GetServerInformation()".into());
                        m.method_return().append2("mako", "emersion").append2("1.9.0", "1.2")
                    }
                    (Some(INTERFACE), "GetCapabilities") => {
                        calls.lock().unwrap().push("GetCapabilities()".into());
                        m.method_return().append1(vec!["body"])
                    }
                    (Some(INTERFACE), "CloseNotification") => {
                        calls.lock().unwrap().push("CloseNotification()".into());
                        m.method_return()
                    }
                    _ => m.error(&ErrorName::from("org.freedesktop.DBus.Error.UnknownMethod"), c"no such method"),
                };
                if !m.get_no_reply() {
                    let _ = peer.channel.send(reply);
                    peer.channel.flush();
                }
            }
            Ok(Some(m)) if m.msg_type() == MessageType::Signal && m.interface().as_deref() == Some(DBUS) => {
                let ours = m.read1::<&str>().ok() == Some(NAME);
                match m.member().as_deref() {
                    Some("NameAcquired") if ours => holds.store(true, Ordering::SeqCst),
                    Some("NameLost") if ours => holds.store(false, Ordering::SeqCst),
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}
