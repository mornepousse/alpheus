//! A fake BlueZ on a `PrivateBus`, for the tests only. It owns `org.bluez` and serves the one
//! adapter's tree (`hci0`, powered; `CasqueBT` paired, not connected) through `GetManagedObjects`
//! at `/` and `Properties.GetAll`; an object it does not hold gets UnknownObject, any other member
//! UnknownMethod. It is one `Peer` served by its own thread (20 ms poll, like support/portal.rs),
//! speaks raw D-Bus and never the daemon's code; dropped, the thread is joined and the name loses
//! its owner. Every call is recorded as `"<Member> <path> <args>"`.

use super::busctl_wire::push_reply;
use super::{Peer, PrivateBus};
use dbus::Message;
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
    Add(u8, String, i16),
    Remove(u8),
    Connected(u8, bool),
    Stop,
}

/// path -> interface -> property -> busctl's `{"type", "data"}`.
type Tree = BTreeMap<String, BTreeMap<String, serde_json::Map<String, Value>>>;

pub struct FakeBluez {
    commands: Sender<Command>,
    calls: Arc<Mutex<Vec<String>>>,
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
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (commands, rx) = mpsc::channel();
        let c = calls.clone();
        let thread = thread::spawn(move || serve(peer, initial_tree(), c, rx));
        FakeBluez { commands, calls, name, thread: Some(thread) }
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
        self.calls.lock().unwrap().clone()
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
        Command::Stop => {}
    }
}

fn answer(call: &Message, tree: &Tree, calls: &Mutex<Vec<String>>) -> Message {
    let p = call.path().map(|p| p.to_string()).unwrap_or_default();
    let member = call.member().map(|m| m.to_string()).unwrap_or_default();
    let interface = call.interface().map(|i| i.to_string()).unwrap_or_default();
    let asked = call.read1::<&str>().unwrap_or("").to_string();
    calls.lock().unwrap().push(format!("{member} {p} {asked}").trim_end().to_string());
    match (interface.as_str(), member.as_str()) {
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
    }
}

fn serve(peer: Peer, mut tree: Tree, calls: Arc<Mutex<Vec<String>>>, commands: Receiver<Command>) {
    loop {
        while let Ok(c) = commands.try_recv() {
            if matches!(c, Command::Stop) {
                return;
            }
            emit(&peer, c, &mut tree);
        }
        match peer.channel.blocking_pop_message(POLL) {
            Ok(Some(m)) if m.msg_type() == MessageType::MethodCall => send(&peer, answer(&m, &tree, &calls)),
            Ok(_) => {}
            Err(_) => return,
        }
    }
}
