//! A fake BlueZ on a `PrivateBus`, for the tests only. It owns `org.bluez` and serves the one
//! adapter's tree (`hci0`, powered; `CasqueBT` paired, not connected) through `GetManagedObjects`
//! at `/` and `Properties.GetAll`; an object it does not hold gets UnknownObject, any other member
//! UnknownMethod. It is one `Peer` served by its own thread (20 ms poll, like support/portal.rs),
//! speaks raw D-Bus and never the daemon's code; dropped, the thread is joined and the name loses
//! its owner. Every call is recorded as `"<Member> <path> <args>"`. Any other call to it (the acting
//! connection's: `Set`, `Connect`, `Pair`…) is answered with an empty return, unless `fail` or `hold`
//! said otherwise, and its destination is kept (`destinations`). The sender and path of the last
//! `RegisterAgent` are kept: `ask_passkey`, `ask_confirmation` and `cancel` call that agent from
//! the fake's thread, and its answers are recorded (`replies`), matched by serial.

use super::busctl_wire::push_reply;
use super::{Peer, PrivateBus};
use dbus::Message;
use dbus::arg::{ArgType, RefArg};
use dbus::message::MessageType;
use dbus::strings::Path;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(20);
const ADAPTER: &str = "/org/bluez/hci0";
const DEVICE: &str = "org.bluez.Device1";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";

/// The device `n`'s object path (the address is documentation-range, fictional).
pub fn device_path(n: u8) -> String {
    format!("{ADAPTER}/dev_AA_BB_CC_DD_EE_{n:02X}")
}

enum Command {
    /// An agent call to send; its answer is matched by serial.
    Ask(Message),
    Add(u8, String, i16),
    Remove(u8),
    Connected(u8, bool),
    Release,
    Stop,
}

/// What the test and the serving thread share.
#[derive(Default)]
struct Shared {
    calls: Vec<String>,
    /// The destination of each acting call, in arrival order.
    destinations: Vec<String>,
    /// (member, error name): the next call of that member gets the error.
    failures: Vec<(String, String)>,
    /// Calls of this member stay unanswered until `release`.
    hold: Option<String>,
    /// The sender and object path of the last `RegisterAgent`.
    agent: Option<(String, String)>,
    /// The agent's answers: `"<Member> ok"` or `"<Member> <error name>"`.
    replies: Vec<String>,
}

/// path -> interface -> property -> busctl's `{"type", "data"}`.
type Tree = BTreeMap<String, BTreeMap<String, serde_json::Map<String, Value>>>;

pub struct FakeBluez {
    commands: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    name: String,
    thread: Option<JoinHandle<()>>,
}

fn prop(kind: &str, data: Value) -> Value {
    json!({ "type": kind, "data": data })
}

fn map(pairs: Vec<(&str, Value)>) -> serde_json::Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn device_props(alias: &str, paired: bool, extra: Vec<(&str, Value)>) -> serde_json::Map<String, Value> {
    let mut m = map(vec![
        ("Name", prop("s", alias.into())),
        ("Alias", prop("s", alias.into())),
        ("Paired", prop("b", paired.into())),
        ("Trusted", prop("b", paired.into())),
        ("Connected", prop("b", false.into())),
        ("Adapter", prop("o", ADAPTER.into())),
    ]);
    m.extend(extra.into_iter().map(|(k, v)| (k.to_string(), v)));
    m
}

fn initial_tree() -> Tree {
    let adapter =
        map(vec![("Powered", prop("b", true.into())), ("PowerState", prop("s", "on".into())), ("Discovering", prop("b", false.into()))]);
    let headset = device_props("CasqueBT", true, vec![("Icon", prop("s", "audio-headset".into()))]);
    let mut tree = Tree::new();
    tree.insert(ADAPTER.into(), [("org.bluez.Adapter1".to_string(), adapter)].into());
    tree.insert(device_path(2), [(DEVICE.to_string(), headset)].into());
    tree
}

impl FakeBluez {
    /// Owns `org.bluez`: hci0 powered, `CasqueBT` paired.
    pub fn serve(bus: &PrivateBus) -> FakeBluez {
        let peer = Peer::connect(bus);
        peer.own("org.bluez");
        let name = peer.name.clone();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (commands, rx) = mpsc::channel();
        let c = shared.clone();
        let thread = thread::spawn(move || serve(peer, initial_tree(), c, rx));
        FakeBluez { commands, shared, name, thread: Some(thread) }
    }

    /// A device scanning found: unpaired, named `alias`, advertising at `rssi`; `InterfacesAdded` goes out.
    pub fn add_nearby(&self, n: u8, alias: &str, rssi: i16) {
        let _ = self.commands.send(Command::Add(n, alias.into(), rssi));
    }

    /// The device `n` goes: `InterfacesRemoved` goes out.
    pub fn remove(&self, n: u8) {
        let _ = self.commands.send(Command::Remove(n));
    }

    /// The device `n` connects or disconnects: `PropertiesChanged` of `Connected` goes out.
    pub fn set_connected(&self, n: u8, on: bool) {
        let _ = self.commands.send(Command::Connected(n, on));
    }

    /// Every method call received, `"<Member> <path> <args>"`, in arrival order.
    pub fn calls(&self) -> Vec<String> {
        self.shared.lock().unwrap().calls.clone()
    }

    /// The destination of every acting call (not the read side's), in arrival order.
    pub fn destinations(&self) -> Vec<String> {
        self.shared.lock().unwrap().destinations.clone()
    }

    /// The next call of `member` is answered with the error `error`.
    pub fn fail(&self, member: &str, error: &str) {
        self.shared.lock().unwrap().failures.push((member.into(), error.into()));
    }

    /// Calls of `member` stay unanswered until `release`.
    pub fn hold(&self, member: &str) {
        self.shared.lock().unwrap().hold = Some(member.into());
    }

    /// Answers the held calls (an empty return each) and holds no more.
    pub fn release(&self) {
        self.shared.lock().unwrap().hold = None;
        let _ = self.commands.send(Command::Release);
    }

    /// The sender of the last `RegisterAgent` (the agent the fake calls).
    pub fn agent_owner(&self) -> Option<String> {
        self.shared.lock().unwrap().agent.as_ref().map(|(sender, _)| sender.clone())
    }

    /// BlueZ shows the device `n` a passkey: `DisplayPasskey(device, passkey, 0)`.
    pub fn ask_passkey(&self, n: u8, passkey: u32) {
        self.ask(|m| m.append3(path(&device_path(n)), passkey, 0u16), "DisplayPasskey");
    }

    /// BlueZ asks to confirm the device `n`'s passkey: `RequestConfirmation(device, passkey)`.
    pub fn ask_confirmation(&self, n: u8, passkey: u32) {
        self.ask(|m| m.append2(path(&device_path(n)), passkey), "RequestConfirmation");
    }

    /// BlueZ cancels the agent's pending request: `Cancel()`.
    pub fn cancel(&self) {
        self.ask(|m| m, "Cancel");
    }

    /// BlueZ releases the agent (it unregistered it itself): `Release()`.
    pub fn release_agent(&self) {
        self.ask(|m| m, "Release");
    }

    /// The agent's answers, in arrival order.
    pub fn replies(&self) -> Vec<String> {
        self.shared.lock().unwrap().replies.clone()
    }

    fn ask(&self, args: impl FnOnce(Message) -> Message, member: &str) {
        let (owner, at) = self.shared.lock().unwrap().agent.clone().expect("an agent registered");
        let m = Message::new_method_call(owner, at, "org.bluez.Agent1", member).unwrap();
        let _ = self.commands.send(Command::Ask(args(m)));
    }

    /// The unique name that owns `org.bluez`.
    pub fn unique_name(&self) -> String {
        self.name.clone()
    }
}

impl Drop for FakeBluez {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn path(p: &str) -> Path<'static> {
    Path::new(p.to_string()).expect("an object path")
}

fn dict(interface: &str, props: &serde_json::Map<String, Value>) -> Value {
    json!({ interface: props })
}

fn send(peer: &Peer, m: Message) {
    let _ = peer.channel.send(m);
    peer.channel.flush();
}

fn emit(peer: &Peer, command: Command, tree: &mut Tree) {
    match command {
        Command::Add(n, alias, rssi) => {
            let p = device_path(n);
            let props = device_props(&alias, false, vec![("RSSI", prop("n", rssi.into()))]);
            let mut m = Message::new_signal("/", OBJECT_MANAGER, "InterfacesAdded").unwrap().append1(path(&p));
            push_reply(&mut m, &json!({ "type": "a{sa{sv}}", "data": [dict(DEVICE, &props)] }).to_string());
            tree.insert(p, [(DEVICE.to_string(), props)].into());
            send(peer, m);
        }
        Command::Remove(n) => {
            let p = device_path(n);
            tree.remove(&p);
            let mut m = Message::new_signal("/", OBJECT_MANAGER, "InterfacesRemoved").unwrap().append1(path(&p));
            push_reply(&mut m, &json!({ "type": "as", "data": [[DEVICE]] }).to_string());
            send(peer, m);
        }
        Command::Connected(n, on) => {
            let p = device_path(n);
            if let Some(d) = tree.get_mut(&p).and_then(|i| i.get_mut(DEVICE)) {
                d.insert("Connected".into(), prop("b", on.into()));
            }
            let mut m = Message::new_signal(&p, PROPERTIES, "PropertiesChanged").unwrap();
            push_reply(&mut m, &json!({ "type": "s", "data": [DEVICE] }).to_string());
            push_reply(&mut m, &json!({ "type": "a{sv}", "data": [{ "Connected": prop("b", on.into()) }] }).to_string());
            push_reply(&mut m, &json!({ "type": "as", "data": [[]] }).to_string());
            send(peer, m);
        }
        Command::Ask(_) | Command::Release | Command::Stop => {}
    }
}

/// A call's arguments as the recorded string has them: `s`/`o` as is, `b` as `true`/`false`, `v`
/// unwrapped, separated by single spaces.
fn args(m: &Message) -> String {
    fn word(a: &dyn RefArg) -> String {
        match a.arg_type() {
            ArgType::Variant => a.as_iter().and_then(|mut i| i.next().map(word)).unwrap_or_default(),
            ArgType::Boolean => (a.as_u64() == Some(1)).to_string(),
            _ => a.as_str().map(String::from).unwrap_or_else(|| format!("{a:?}")),
        }
    }
    let mut it = m.iter_init();
    let mut out = Vec::new();
    while let Some(a) = it.get_refarg() {
        out.push(word(&*a));
        it.next();
    }
    out.join(" ")
}

/// An acting call: recorded with its arguments and destination; answered at once (empty, or the
/// error `fail` named), or None while its member is held.
fn act(call: &Message, shared: &Mutex<Shared>) -> Option<Message> {
    let p = call.path().map(|p| p.to_string()).unwrap_or_default();
    let member = call.member().map(|m| m.to_string()).unwrap_or_default();
    let mut s = shared.lock().unwrap();
    s.calls.push(format!("{member} {p} {}", args(call)).trim_end().to_string());
    if member == "RegisterAgent" {
        let at = call.read1::<Path>().map(|p| p.to_string()).unwrap_or_default();
        s.agent = Some((call.sender().map(|n| n.to_string()).unwrap_or_default(), at));
    }
    s.destinations.push(call.destination().map(|d| d.to_string()).unwrap_or_default());
    if let Some(i) = s.failures.iter().position(|(m, _)| *m == member) {
        let (_, error) = s.failures.remove(i);
        return Some(call.error(&error.into(), c"failed"));
    }
    if s.hold.as_deref() == Some(member.as_str()) {
        return None;
    }
    Some(call.method_return())
}

/// The read side's calls; None: an acting call (`act`).
fn answer(call: &Message, tree: &Tree, shared: &Mutex<Shared>) -> Option<Message> {
    let p = call.path().map(|p| p.to_string()).unwrap_or_default();
    let member = call.member().map(|m| m.to_string()).unwrap_or_default();
    let interface = call.interface().map(|i| i.to_string()).unwrap_or_default();
    if !matches!((interface.as_str(), member.as_str()), (OBJECT_MANAGER, "GetManagedObjects") | (PROPERTIES, "GetAll")) {
        return None;
    }
    let asked = call.read1::<&str>().unwrap_or("").to_string();
    shared.lock().unwrap().calls.push(format!("{member} {p} {asked}").trim_end().to_string());
    Some(match (interface.as_str(), member.as_str()) {
        (OBJECT_MANAGER, "GetManagedObjects") if p == "/" => {
            let all: serde_json::Map<String, Value> = tree.iter().map(|(p, i)| (p.clone(), json!(i))).collect();
            let mut r = call.method_return();
            push_reply(&mut r, &json!({ "type": "a{oa{sa{sv}}}", "data": [all] }).to_string());
            r
        }
        (PROPERTIES, "GetAll") => match tree.get(&p).and_then(|i| i.get(&asked)) {
            Some(props) => {
                let mut r = call.method_return();
                push_reply(&mut r, &json!({ "type": "a{sv}", "data": [props] }).to_string());
                r
            }
            None => call.error(&"org.freedesktop.DBus.Error.UnknownObject".into(), c"no such object"),
        },
        _ => call.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), c"no such method"),
    })
}

fn serve(peer: Peer, mut tree: Tree, shared: Arc<Mutex<Shared>>, commands: Receiver<Command>) {
    let mut held = Vec::new();
    // The agent calls sent, by serial: their member.
    let mut asked = BTreeMap::new();
    loop {
        while let Ok(c) = commands.try_recv() {
            match c {
                Command::Stop => return,
                Command::Ask(m) => {
                    let member = m.member().map(|m| m.to_string()).unwrap_or_default();
                    if let Ok(serial) = peer.channel.send(m) {
                        asked.insert(serial, member);
                    }
                    peer.channel.flush();
                }
                Command::Release => held.drain(..).for_each(|m: Message| send(&peer, m.method_return())),
                c => emit(&peer, c, &mut tree),
            }
        }
        match peer.channel.blocking_pop_message(POLL) {
            Ok(Some(m)) if m.msg_type() == MessageType::MethodCall => match answer(&m, &tree, &shared).or_else(|| act(&m, &shared)) {
                Some(r) if !m.get_no_reply() => send(&peer, r),
                Some(_) => {}
                None => held.push(m),
            },
            Ok(Some(mut m)) => {
                if let Some(member) = m.get_reply_serial().and_then(|s| asked.remove(&s)) {
                    let outcome = match m.as_result() {
                        Ok(_) => "ok".to_string(),
                        Err(e) => e.name().unwrap_or("").to_string(),
                    };
                    shared.lock().unwrap().replies.push(format!("{member} {outcome}"));
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
}
