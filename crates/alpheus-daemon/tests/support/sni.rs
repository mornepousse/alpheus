//! Fake tray applications on a `PrivateBus`, for the tests only: SNI items,
//! their dbusmenu servers and another watcher, each answering like the
//! client it models (research: spike-rustsni-rustbus.md § 3–4) — Chromium's
//! host pre-check and failing `Get(IconName)`, KDE's `ProtocolVersion`
//! check, libayatana's path-only registration, Steam's missing `Activate`,
//! Electron's lazy menus. Fictional applications only (Nimbus, Kettle,
//! Lumen, Quill, Ferry…): never a capture of a real tray. Each fake is one
//! `Peer` served by its own thread (test process only); the fakes speak raw
//! D-Bus and never use the daemon's tray code.

use super::{Peer, PrivateBus};
use alpheus_daemon::session_bus::{DEFAULT_ITEM_PATH, ITEM_INTERFACE, MENU_INTERFACE, WATCHER_INTERFACE, WATCHER_NAME, WATCHER_PATH};
use dbus::Message;
use dbus::arg::{ArgType, Iter, IterAppend, Variant};
use dbus::message::MessageType;
use dbus::strings::{ErrorName, Path, Signature};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// Chromium's signals go out twice, on this interface as well (chromium.cc).
const FREEDESKTOP_ITEM: &str = "org.freedesktop.StatusNotifierItem";
/// How long a fake waits on its socket before looking at its commands: slow enough not to load the
/// machine the budget tests measure on, fast enough for any test's deadline.
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Bool(bool),
    Path(String),
    Int(i32),
    ToolTip {
        title: String,
        pixmap_bytes: usize,
    },
    /// `IconPixmap`/`AttentionIconPixmap` (`a(iiay)`, one 32x32 entry of `bytes` bytes): the daemon
    /// never reads it (spec § 3), but a real item may still offer it alongside (or instead of) an
    /// icon name.
    IconPixmap(usize),
}

fn s(text: &str) -> Value {
    Value::Str(text.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    /// Its well-known name (KDE).
    Name,
    /// Its object path only (libayatana, Steam).
    Path,
    /// "name/path" in one string (older Electron).
    Combined,
    /// Its unique name (Qt, Chromium).
    Unique,
    /// Never registers (found only by the startup inventory).
    Never,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i32,
    pub props: Vec<(String, Value)>,
    pub children: Vec<Entry>,
}

fn entry(id: i32, props: &[(&str, Value)], children: Vec<Entry>) -> Entry {
    Entry { id, props: props.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), children }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FakeMenu {
    pub path: String,
    pub root: Vec<Entry>,
    /// AboutToShow answers needUpdate = true, the layout is built then, LayoutUpdated follows (Electron).
    pub lazy: bool,
    /// Event gets a reply (many servers never answer it).
    pub answers_event: bool,
}

impl FakeMenu {
    /// "Open" (normal), "Mute" (checkmark on), "Low/High" (radios), a separator, "Disabled", a hidden
    /// entry, "More ▸" (children-display = submenu, two children fetched only on demand).
    pub fn sample() -> FakeMenu {
        let radio =
            |id, label: &str, on| entry(id, &[("label", s(label)), ("toggle-type", s("radio")), ("toggle-state", Value::Int(on))], vec![]);
        FakeMenu {
            path: "/MenuBar".into(),
            root: vec![
                entry(1, &[("label", s("_Open"))], vec![]),
                entry(2, &[("label", s("Mute")), ("toggle-type", s("checkmark")), ("toggle-state", Value::Int(1))], vec![]),
                radio(3, "Low", 0),
                radio(4, "High", 1),
                entry(5, &[("type", s("separator"))], vec![]),
                entry(6, &[("label", s("Disabled")), ("enabled", Value::Bool(false))], vec![]),
                entry(7, &[("label", s("Hidden")), ("visible", Value::Bool(false))], vec![]),
                entry(
                    8,
                    &[("label", s("More")), ("children-display", s("submenu"))],
                    vec![entry(9, &[("label", s("Settings"))], vec![]), entry(10, &[("label", s("About"))], vec![])],
                ),
            ],
            lazy: false,
            answers_event: false,
        }
    }

    pub fn huge(entries: usize) -> FakeMenu {
        let root = (1..=entries as i32).map(|id| entry(id, &[("label", s(&format!("Entry {id}")))], vec![])).collect();
        FakeMenu { path: "/MenuBar".into(), root, lazy: false, answers_event: false }
    }

    pub fn deep(depth: usize) -> FakeMenu {
        let mut node = entry(depth as i32, &[("label", s("Bottom"))], vec![]);
        for id in (1..depth as i32).rev() {
            node = entry(id, &[("label", s(&format!("Level {id}"))), ("children-display", s("submenu"))], vec![node]);
        }
        FakeMenu { path: "/MenuBar".into(), root: vec![node], lazy: false, answers_event: false }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// Well-known name to own first (KDE: "org.kde.StatusNotifierItem-<pid>-<n>").
    pub name: Option<String>,
    pub path: String,
    pub registration: Registration,
    /// Served by Get; any other property → org.freedesktop.DBus.Error.UnknownProperty
    /// (Chromium-like: org.freedesktop.DBus.Error.Failed). GetAll answers too (and is recorded).
    pub properties: Vec<(String, Value)>,
    /// Methods it answers on ITEM_INTERFACE; others → UnknownMethod (Steam has no Activate).
    pub methods: Vec<String>,
    pub menu: Option<FakeMenu>,
    /// Connected, never reads its socket.
    pub silent: bool,
    /// Registers only if the watcher's IsStatusNotifierHostRegistered is true (Chromium).
    pub needs_host: bool,
    /// Chromium's other quirks, independent of the host pre-check above: its signals go out
    /// doubled, on `org.freedesktop.StatusNotifierItem` as well, and an unknown property answers
    /// `Failed` rather than `UnknownProperty`.
    pub chromium_quirks: bool,
}

fn props(list: &[(&str, Value)]) -> Vec<(String, Value)> {
    list.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn methods(list: &[&str]) -> Vec<String> {
    list.iter().map(|m| m.to_string()).collect()
}

const ALL_METHODS: [&str; 4] = ["Activate", "SecondaryActivate", "ContextMenu", "Scroll"];

impl Spec {
    /// A KDE-like item (`KStatusNotifierItem`): its own well-known name, every property.
    pub fn kde(n: u32) -> Spec {
        Spec {
            name: Some(format!("org.kde.StatusNotifierItem-4242-{n}")),
            path: DEFAULT_ITEM_PATH.into(),
            registration: Registration::Name,
            properties: props(&[
                ("Id", s("nimbus")),
                ("Title", s("Nimbus")),
                ("Status", s("Active")),
                ("IconName", s("nimbus-tray")),
                ("AttentionIconName", s("nimbus-alert")),
                ("ToolTip", Value::ToolTip { title: "Nimbus".into(), pixmap_bytes: 0 }),
                ("Menu", Value::Path("/MenuBar".into())),
                ("ItemIsMenu", Value::Bool(false)),
            ]),
            methods: methods(&ALL_METHODS),
            menu: Some(FakeMenu::sample()),
            silent: false,
            needs_host: false,
            chromium_quirks: false,
        }
    }

    /// A Qt-like item (`QSystemTrayIcon`): its unique name, an empty IconName (a pixmap instead).
    pub fn qt() -> Spec {
        Spec {
            name: None,
            path: DEFAULT_ITEM_PATH.into(),
            registration: Registration::Unique,
            properties: props(&[
                ("Id", s("kettle")),
                ("Title", s("Kettle")),
                ("Status", s("Active")),
                ("IconName", s("")),
                ("IconPixmap", Value::IconPixmap(4096)),
                ("ToolTip", Value::ToolTip { title: "Kettle".into(), pixmap_bytes: 4096 }),
                ("Menu", Value::Path("/MenuBar".into())),
                ("ItemIsMenu", Value::Bool(false)),
            ]),
            methods: methods(&ALL_METHODS),
            menu: Some(FakeMenu::sample()),
            silent: false,
            needs_host: false,
            chromium_quirks: false,
        }
    }

    /// Qt without a context menu: Menu is "/NO_DBUSMENU", no dbusmenu object (a right click).
    pub fn qt_without_menu() -> Spec {
        let mut spec = Spec::qt();
        spec.properties.retain(|(k, _)| k != "Menu");
        spec.properties.push(("Menu".into(), Value::Path("/NO_DBUSMENU".into())));
        spec.menu = None;
        spec
    }

    /// A Chromium/Electron-like item: Id chrome_status_icon_1, an empty Title, no IconName (Get fails
    /// with Failed), a lazy menu at /org/chromium/DbusMenu; registers only once a host is registered.
    pub fn chromium() -> Spec {
        let mut menu = FakeMenu::sample();
        menu.path = "/org/chromium/DbusMenu".into();
        menu.lazy = true;
        Spec {
            name: None,
            path: DEFAULT_ITEM_PATH.into(),
            registration: Registration::Unique,
            properties: props(&[
                ("Id", s("chrome_status_icon_1")),
                ("Title", s("")),
                ("Status", s("Active")),
                ("ToolTip", Value::ToolTip { title: "Lumen".into(), pixmap_bytes: 300_000 }),
                ("Menu", Value::Path("/org/chromium/DbusMenu".into())),
                ("ItemIsMenu", Value::Bool(false)),
            ]),
            methods: methods(&ALL_METHODS),
            menu: Some(menu),
            silent: false,
            needs_host: true,
            chromium_quirks: true,
        }
    }

    /// A libayatana-like item: path-only registration, no ItemIsMenu, no ContextMenu.
    pub fn ayatana() -> Spec {
        let mut menu = FakeMenu::sample();
        menu.path = "/org/ayatana/NotificationItem/quill/Menu".into();
        Spec {
            name: None,
            path: "/org/ayatana/NotificationItem/quill".into(),
            registration: Registration::Path,
            properties: props(&[
                ("Id", s("quill")),
                ("Title", s("Quill")),
                ("Status", s("Active")),
                ("IconName", s("quill-panel")),
                ("ToolTip", Value::ToolTip { title: "Quill".into(), pixmap_bytes: 0 }),
                ("Menu", Value::Path("/org/ayatana/NotificationItem/quill/Menu".into())),
            ]),
            methods: methods(&["Activate", "SecondaryActivate", "Scroll"]),
            menu: Some(menu),
            silent: false,
            needs_host: false,
            chromium_quirks: false,
        }
    }

    /// A Steam-like item (Canonical libappindicator): path-only, no Activate, no ToolTip, no ItemIsMenu.
    pub fn steam() -> Spec {
        let mut menu = FakeMenu::sample();
        menu.path = "/org/ayatana/NotificationItem/steam/Menu".into();
        Spec {
            name: None,
            path: "/org/ayatana/NotificationItem/steam".into(),
            registration: Registration::Path,
            properties: props(&[
                ("Id", s("steam")),
                ("Title", s("Steam")),
                ("Status", s("Active")),
                ("IconName", s("steam_tray_mono")),
                ("Menu", Value::Path("/org/ayatana/NotificationItem/steam/Menu".into())),
            ]),
            methods: methods(&["Scroll", "SecondaryActivate", "XAyatanaSecondaryActivate"]),
            menu: Some(menu),
            silent: false,
            needs_host: false,
            chromium_quirks: false,
        }
    }

    /// An older-Electron-like item registering "name/path" in one string.
    pub fn combined() -> Spec {
        Spec {
            name: Some("org.kde.StatusNotifierItem-4343-1".into()),
            path: DEFAULT_ITEM_PATH.into(),
            registration: Registration::Combined,
            properties: props(&[("Id", s("ferry")), ("Title", s("Ferry")), ("Status", s("Active")), ("IconName", s("ferry"))]),
            methods: methods(&ALL_METHODS),
            menu: None,
            silent: false,
            needs_host: false,
            chromium_quirks: false,
        }
    }

    /// Registers with its unique name, then never reads its socket again.
    pub fn silent() -> Spec {
        Spec {
            name: None,
            path: DEFAULT_ITEM_PATH.into(),
            registration: Registration::Unique,
            properties: props(&[("Id", s("mute-app")), ("Title", s("Mute App"))]),
            methods: methods(&ALL_METHODS),
            menu: None,
            silent: true,
            needs_host: false,
            chromium_quirks: false,
        }
    }

    /// A 10 KB title full of control characters, a 501-entry menu.
    pub fn hostile() -> Spec {
        let title: String = "Hostile\u{7}\u{1b}[31m\n".repeat(700);
        Spec {
            name: None,
            path: DEFAULT_ITEM_PATH.into(),
            registration: Registration::Unique,
            properties: props(&[("Id", s("hostile")), ("Title", Value::Str(title)), ("Menu", Value::Path("/MenuBar".into()))]),
            methods: methods(&ALL_METHODS),
            menu: Some(FakeMenu::huge(501)),
            silent: false,
            needs_host: false,
            chromium_quirks: false,
        }
    }
}

fn signature(s: &str) -> Signature<'static> {
    Signature::new(s).unwrap()
}

/// Appends `v` inside a variant.
fn append_value(i: &mut IterAppend<'_>, v: &Value) {
    match v {
        Value::Str(x) => i.append(Variant(x.as_str())),
        Value::Bool(x) => i.append(Variant(*x)),
        Value::Int(x) => i.append(Variant(*x)),
        Value::Path(x) => i.append(Variant(Path::new(x.as_str()).unwrap())),
        Value::ToolTip { title, pixmap_bytes } => i.append_variant(&signature("(sa(iiay)ss)"), |v| {
            v.append_struct(|st| {
                st.append("");
                st.append_array(&signature("(iiay)"), |a| {
                    if *pixmap_bytes > 0 {
                        a.append_struct(|px| {
                            px.append(64i32);
                            px.append((*pixmap_bytes / 256) as i32);
                            px.append(vec![0x5au8; *pixmap_bytes]);
                        });
                    }
                });
                st.append(title.as_str());
                st.append("");
            })
        }),
        Value::IconPixmap(bytes) => i.append_variant(&signature("a(iiay)"), |v| {
            v.append_array(&signature("(iiay)"), |a| {
                a.append_struct(|px| {
                    px.append(32i32);
                    px.append(32i32);
                    px.append(vec![0x5au8; *bytes]);
                });
            })
        }),
    }
}

/// One argument as text, for the call recorder.
fn render(it: &mut Iter<'_>) -> String {
    match it.arg_type() {
        ArgType::String => it.get::<&str>().unwrap_or_default().to_string(),
        ArgType::ObjectPath => it.get::<Path>().map(|p| p.to_string()).unwrap_or_default(),
        ArgType::Int32 => it.get::<i32>().unwrap_or_default().to_string(),
        ArgType::UInt32 => it.get::<u32>().unwrap_or_default().to_string(),
        ArgType::Boolean => it.get::<bool>().unwrap_or_default().to_string(),
        ArgType::Variant => it.recurse(ArgType::Variant).map(|mut v| render(&mut v)).unwrap_or_default(),
        ArgType::Array => {
            let mut parts = Vec::new();
            if let Some(mut a) = it.recurse(ArgType::Array) {
                while a.arg_type() != ArgType::Invalid {
                    parts.push(render(&mut a));
                    a.next();
                }
            }
            format!("[{}]", parts.join(", "))
        }
        _ => "?".into(),
    }
}

/// "Interface.Member(arg, arg…)".
fn recorded(m: &Message) -> String {
    let mut it = m.iter_init();
    let mut args = Vec::new();
    while it.arg_type() != ArgType::Invalid {
        args.push(render(&mut it));
        it.next();
    }
    format!(
        "{}.{}({})",
        m.interface().map(|i| i.to_string()).unwrap_or_default(),
        m.member().map(|x| x.to_string()).unwrap_or_default(),
        args.join(", ")
    )
}

fn error(call: &Message, name: &str) -> Message {
    call.error(&ErrorName::new(name.to_string()).unwrap(), c"fake application")
}

fn find(entries: &[Entry], id: i32) -> Option<&Entry> {
    entries.iter().find_map(|e| if e.id == id { Some(e) } else { find(&e.children, id) })
}

fn find_mut(entries: &mut [Entry], id: i32) -> Option<&mut Entry> {
    entries.iter_mut().find_map(|e| if e.id == id { Some(e) } else { find_mut(&mut e.children, id) })
}

/// `(ia{sv}av)` for `id` with its props filtered by `names` (all when empty), children `depth` levels deep (-1: all).
fn append_node(i: &mut IterAppend<'_>, id: i32, props: &[(String, Value)], children: &[Entry], depth: i32, names: &[String]) {
    i.append_struct(|st| {
        st.append(id);
        st.append_dict(&signature("s"), &signature("v"), |d| {
            for (k, v) in props.iter().filter(|(k, _)| names.is_empty() || names.contains(k)) {
                d.append_dict_entry(|e| {
                    e.append(k.as_str());
                    append_value(e, v);
                });
            }
        });
        st.append_array(&signature("v"), |a| {
            if depth != 0 {
                for c in children {
                    a.append_variant(&signature("(ia{sv}av)"), |v| append_node(v, c.id, &c.props, &c.children, depth - 1, names));
                }
            }
        });
    });
}

enum Command {
    Register(Sender<bool>),
    Emit(String),
    Status(String),
    Burst(String, usize),
    Set(String, Value),
    MenuSet(i32, String, Value),
    LayoutUpdated(u32, i32),
    Stop,
}

enum Waiting {
    ProtocolVersion(Sender<bool>),
    HostCheck(Sender<bool>),
    Register(Sender<bool>),
}

/// The serving thread's state: everything the fake answers with.
struct Served {
    peer: Peer,
    spec: Spec,
    calls: Arc<Mutex<Vec<String>>>,
    waiting: HashMap<u32, Waiting>,
    /// The lazy menu's layout exists (built at the first AboutToShow).
    built: bool,
    revision: u32,
    /// A pre-check (KDE's `ProtocolVersion`, Chromium's host check) already failed once: like the
    /// real client, a further `register()` is then a no-op — it does not poll the watcher blindly.
    /// Cleared by `StatusNotifierHostRegistered` (watched for when `needs_host`), which is the real
    /// signal real clients retry on; a new watcher owner would equally justify trying again.
    gave_up: bool,
}

impl Served {
    fn send(&self, m: Message) -> u32 {
        let serial = self.peer.channel.send(m).unwrap_or(0);
        self.peer.channel.flush();
        serial
    }

    fn call_watcher(&mut self, member: &str, args: impl FnOnce(Message) -> Message, then: Waiting) {
        let interface = if member == "Get" { PROPERTIES } else { WATCHER_INTERFACE };
        let m = args(Message::new_method_call(WATCHER_NAME, WATCHER_PATH, interface, member).unwrap());
        let serial = self.send(m);
        self.waiting.insert(serial, then);
    }

    fn register(&mut self, ack: Sender<bool>) {
        if self.gave_up {
            let _ = ack.send(false);
            return;
        }
        match self.spec.registration {
            Registration::Never => {
                let _ = ack.send(false);
            }
            // KDE first checks the watcher's ProtocolVersion is 0 (else it falls back to XEmbed).
            Registration::Name => {
                self.call_watcher("Get", |m| m.append2(WATCHER_INTERFACE, "ProtocolVersion"), Waiting::ProtocolVersion(ack))
            }
            // Chromium gives up unless a host is registered.
            _ if self.spec.needs_host => {
                self.call_watcher("Get", |m| m.append2(WATCHER_INTERFACE, "IsStatusNotifierHostRegistered"), Waiting::HostCheck(ack))
            }
            _ => self.register_now(ack),
        }
    }

    fn register_now(&mut self, ack: Sender<bool>) {
        let name = self.spec.name.clone().unwrap_or_default();
        let service = match self.spec.registration {
            Registration::Name => name,
            Registration::Path => self.spec.path.clone(),
            Registration::Combined => format!("{name}{}", self.spec.path),
            Registration::Unique | Registration::Never => self.peer.name.clone(),
        };
        self.call_watcher("RegisterStatusNotifierItem", |m| m.append1(service), Waiting::Register(ack));
    }

    fn answered(&mut self, m: Message) {
        let Some(waiting) = m.get_reply_serial().and_then(|s| self.waiting.remove(&s)) else { return };
        let ok = m.msg_type() == MessageType::MethodReturn;
        match waiting {
            Waiting::ProtocolVersion(ack) if ok && m.read1::<Variant<i32>>().map(|v| v.0) == Ok(0) => self.register_now(ack),
            Waiting::HostCheck(ack) if ok && m.read1::<Variant<bool>>().map(|v| v.0) == Ok(true) => self.register_now(ack),
            Waiting::ProtocolVersion(ack) | Waiting::HostCheck(ack) => {
                self.gave_up = true;
                let _ = ack.send(false);
            }
            Waiting::Register(ack) => {
                let _ = ack.send(ok);
            }
        }
    }

    fn emit(&self, path: &str, interface: &str, member: &str, args: impl Fn(Message) -> Message) {
        let _ = self.peer.channel.send(args(Message::new_signal(path, interface, member).unwrap()));
        if self.spec.chromium_quirks && interface == ITEM_INTERFACE {
            let _ = self.peer.channel.send(args(Message::new_signal(path, FREEDESKTOP_ITEM, member).unwrap()));
        }
    }

    fn command(&mut self, c: Command) -> bool {
        let path = self.spec.path.clone();
        let menu_path = self.spec.menu.as_ref().map(|m| m.path.clone()).unwrap_or_default();
        match c {
            Command::Register(ack) => self.register(ack),
            Command::Emit(member) => self.emit(&path, ITEM_INTERFACE, &member, |m| m),
            Command::Status(status) => self.emit(&path, ITEM_INTERFACE, "NewStatus", |m| m.append1(status.as_str())),
            Command::Burst(member, n) => {
                for _ in 0..n {
                    self.emit(&path, ITEM_INTERFACE, &member, |m| m);
                }
            }
            // No menu: no object path to emit from (an empty path would not even build a signal).
            Command::MenuSet(..) | Command::LayoutUpdated(..) if menu_path.is_empty() => {}
            Command::Set(property, v) => match self.spec.properties.iter_mut().find(|(k, _)| *k == property) {
                Some(p) => p.1 = v,
                None => self.spec.properties.push((property, v)),
            },
            Command::MenuSet(id, property, v) => {
                if let Some(e) = self.spec.menu.as_mut().and_then(|m| find_mut(&mut m.root, id)) {
                    e.props.retain(|(k, _)| *k != property);
                    e.props.push((property.clone(), v.clone()));
                }
                let mut m = Message::new_signal(menu_path.as_str(), MENU_INTERFACE, "ItemsPropertiesUpdated").unwrap();
                let mut i = IterAppend::new(&mut m);
                i.append_array(&signature("(ia{sv})"), |a| {
                    a.append_struct(|st| {
                        st.append(id);
                        st.append_dict(&signature("s"), &signature("v"), |d| {
                            d.append_dict_entry(|e| {
                                e.append(property.as_str());
                                append_value(e, &v);
                            })
                        });
                    })
                });
                i.append_array(&signature("(ias)"), |_| {});
                let _ = self.peer.channel.send(m);
            }
            Command::LayoutUpdated(revision, parent) => {
                self.revision = revision;
                self.emit(&menu_path, MENU_INTERFACE, "LayoutUpdated", |m| m.append2(revision, parent));
            }
            Command::Stop => return false,
        }
        self.peer.channel.flush();
        true
    }

    /// The answer to `call`, None when it expects none.
    fn answer(&mut self, call: &Message) -> Option<Message> {
        let interface = call.interface().map(|i| i.to_string()).unwrap_or_default();
        let member = call.member().map(|x| x.to_string()).unwrap_or_default();
        let path = call.path().map(|p| p.to_string()).unwrap_or_default();
        let unknown_property =
            if self.spec.chromium_quirks { "org.freedesktop.DBus.Error.Failed" } else { "org.freedesktop.DBus.Error.UnknownProperty" };
        let menu = self.spec.menu.clone().filter(|m| m.path == path);
        if path != self.spec.path && menu.is_none() {
            let reply = error(call, "org.freedesktop.DBus.Error.UnknownObject");
            return (!call.get_no_reply()).then_some(reply);
        }
        let reply = match (interface.as_str(), member.as_str()) {
            (PROPERTIES, "Get") if path == self.spec.path => {
                let (iface, name): (&str, &str) = call.read2().ok()?;
                match self.spec.properties.iter().find(|(k, _)| k == name).filter(|_| iface == ITEM_INTERFACE) {
                    Some((_, v)) => {
                        let mut r = call.method_return();
                        append_value(&mut IterAppend::new(&mut r), v);
                        r
                    }
                    None => error(call, unknown_property),
                }
            }
            (PROPERTIES, "GetAll") if path == self.spec.path => {
                let mut r = call.method_return();
                IterAppend::new(&mut r).append_dict(&signature("s"), &signature("v"), |d| {
                    for (k, v) in &self.spec.properties {
                        d.append_dict_entry(|e| {
                            e.append(k.as_str());
                            append_value(e, v);
                        })
                    }
                });
                r
            }
            (ITEM_INTERFACE, m) if path == self.spec.path && self.spec.methods.iter().any(|x| x == m) => call.method_return(),
            (MENU_INTERFACE, "AboutToShow") if menu.is_some() => {
                let lazy = menu.as_ref().is_some_and(|m| m.lazy) && !self.built;
                self.built = true;
                let r = call.method_return().append1(lazy);
                if lazy {
                    // Electron: the layout changed, LayoutUpdated follows the answer.
                    let _ = self.peer.channel.send(r);
                    self.revision += 1;
                    let revision = self.revision;
                    self.emit(&path, MENU_INTERFACE, "LayoutUpdated", |m| m.append2(revision, 0i32));
                    return None;
                }
                r
            }
            (MENU_INTERFACE, "GetLayout") if menu.is_some() => {
                let menu = menu.unwrap();
                let (parent, depth, names): (i32, i32, Vec<String>) = call.read3().ok()?;
                let empty = menu.lazy && !self.built;
                let children: Vec<Entry> = if empty { vec![] } else { menu.root.clone() };
                let root_props = vec![("children-display".to_string(), s("submenu"))];
                let (props, kids) = if parent == 0 {
                    (root_props, children)
                } else {
                    match find(&children, parent) {
                        Some(e) => (e.props.clone(), e.children.clone()),
                        None => return (!call.get_no_reply()).then(|| error(call, "org.freedesktop.DBus.Error.InvalidArgs")),
                    }
                };
                let mut r = call.method_return();
                let mut i = IterAppend::new(&mut r);
                i.append(self.revision);
                append_node(&mut i, parent, &props, &kids, depth, &names);
                r
            }
            (MENU_INTERFACE, "Event") if menu.is_some() => {
                if !menu.unwrap().answers_event {
                    return None;
                }
                call.method_return()
            }
            _ => error(call, "org.freedesktop.DBus.Error.UnknownMethod"),
        };
        (!call.get_no_reply()).then_some(reply)
    }
}

fn serve(mut served: Served, commands: Receiver<Command>) {
    // `silent` alone still reads long enough to complete its own registration (the round trip
    // `register()` waits on) before going quiet; combined with `Registration::Never`, there is no
    // registration to wait for, so it must never read at all, from the very first message.
    let mut reading = !(served.spec.silent && served.spec.registration == Registration::Never);
    loop {
        if !reading {
            // Silent: connected, never reads its socket again; only told to stop.
            match commands.recv() {
                Ok(Command::Stop) | Err(_) => return,
                Ok(_) => continue,
            }
        }
        while let Ok(c) = commands.try_recv() {
            if !served.command(c) {
                return;
            }
        }
        match served.peer.channel.blocking_pop_message(POLL) {
            Ok(Some(m)) => match m.msg_type() {
                MessageType::MethodReturn | MessageType::Error => {
                    served.answered(m);
                    if served.spec.silent && served.waiting.is_empty() {
                        reading = false;
                    }
                }
                MessageType::MethodCall => {
                    served.calls.lock().unwrap().push(recorded(&m));
                    if let Some(r) = served.answer(&m) {
                        let _ = served.peer.channel.send(r);
                        served.peer.channel.flush();
                    }
                }
                // The real retry trigger (not an explicit register() call, a no-op once gave_up): a
                // host just appeared, so a gave-up Chromium-like item tries again.
                MessageType::Signal if m.member().as_deref() == Some("StatusNotifierHostRegistered") && served.gave_up => {
                    served.gave_up = false;
                    let (ack, _never_read) = mpsc::channel();
                    served.register(ack);
                }
                _ => {}
            },
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

pub struct FakeItem {
    /// ":1.N".
    pub unique: String,
    commands: Sender<Command>,
    calls: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl FakeItem {
    /// Connects, owns `spec.name`, serves, then registers as `spec.registration` says (waiting for the
    /// watcher's reply; Registration::Never registers nothing).
    pub fn start(bus: &PrivateBus, spec: Spec) -> FakeItem {
        let peer = Peer::connect(bus);
        if let Some(name) = &spec.name {
            peer.own(name);
        }
        if spec.needs_host {
            // The signal a real client retries on once it has given up (see `register`'s doc
            // comment): broadcast, no destination, so a plain AddMatch is enough to receive it.
            let rule = format!("type='signal',interface='{WATCHER_INTERFACE}',member='StatusNotifierHostRegistered'");
            let add_match = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch")
                .unwrap()
                .append1(rule);
            let _ = peer.call(add_match);
        }
        let unique = peer.name.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (commands, rx) = mpsc::channel();
        let never = spec.registration == Registration::Never;
        let served = Served { peer, spec, calls: calls.clone(), waiting: HashMap::new(), built: false, revision: 1, gave_up: false };
        let thread = thread::spawn(move || serve(served, rx));
        let item = FakeItem { unique, commands, calls, thread: Some(thread) };
        if !never {
            item.register();
        }
        item
    }

    /// Registers again (duplicates). A no-op once a pre-check (KDE's `ProtocolVersion`, Chromium's
    /// host check) has already failed: like the real client, this fake does not poll the watcher
    /// blindly. The way back is the signal a real client watches for instead —
    /// `StatusNotifierHostRegistered`, handled automatically in `serve` — not a fresh call here; a
    /// new watcher owner (a different unique name taking `org.kde.StatusNotifierWatcher`) would
    /// equally justify trying again. Waits for the watcher's answer (5 s at most).
    pub fn register(&self) {
        let (ack, done) = mpsc::channel();
        let _ = self.commands.send(Command::Register(ack));
        let _ = done.recv_timeout(Duration::from_secs(5));
    }

    /// Emits `member` (NewIcon, NewTitle, NewToolTip, NewAttentionIcon) on ITEM_INTERFACE, no argument.
    pub fn emit(&self, member: &str) {
        let _ = self.commands.send(Command::Emit(member.into()));
    }

    /// NewStatus(status).
    pub fn emit_status(&self, status: &str) {
        let _ = self.commands.send(Command::Status(status.into()));
    }

    /// `n` emissions of `member` back to back.
    pub fn burst(&self, member: &str, n: usize) {
        let _ = self.commands.send(Command::Burst(member.into(), n));
    }

    pub fn set(&self, property: &str, v: Value) {
        let _ = self.commands.send(Command::Set(property.into(), v));
    }

    /// Changes one menu entry's property and emits ItemsPropertiesUpdated for it.
    pub fn menu_set(&self, id: i32, property: &str, v: Value) {
        let _ = self.commands.send(Command::MenuSet(id, property.into(), v));
    }

    pub fn menu_layout_updated(&self, revision: u32, parent: i32) {
        let _ = self.commands.send(Command::LayoutUpdated(revision, parent));
    }

    /// Every method call received, "Interface.Member(args)" in arrival order (the call recorder).
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Waits for a call whose member is `member`; its text.
    pub fn wait_call(&self, member: &str, timeout: Duration) -> Option<String> {
        let needle = format!(".{member}(");
        let end = Instant::now() + timeout;
        loop {
            if let Some(c) = self.calls().into_iter().find(|c| c.contains(&needle)) {
                return Some(c);
            }
            if Instant::now() >= end {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Drops the connection (NameOwnerChanged with an empty new owner, for both names).
    pub fn kill(self) {
        drop(self);
    }
}

impl Drop for FakeItem {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Another watcher (DMS during a hybrid session): owns WATCHER_NAME, serves RegisterStatusNotifierHost,
/// RegisterStatusNotifierItem, the three properties, emits ItemRegistered/HostRegistered, and
/// ItemUnregistered when an item's owner leaves the bus (as Plasma's watcher does).
pub struct FakeWatcher {
    pub unique: String,
    hosts: Arc<Mutex<Vec<String>>>,
    items: Arc<Mutex<Vec<String>>>,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl FakeWatcher {
    pub fn start(bus: &PrivateBus) -> FakeWatcher {
        let peer = Peer::connect(bus);
        // Owned with ALLOW_REPLACEMENT (1), as a watcher may: the daemon must still never take it.
        let request = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName")
            .unwrap()
            .append2(WATCHER_NAME, 1u32);
        assert_eq!(peer.call(request).unwrap().read1::<u32>().unwrap(), 1, "the fake watcher owns the name");
        // Departures, as Plasma's watcher sees them: an entry goes when its owner leaves the bus.
        let departures = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch")
            .unwrap()
            .append1("type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged'");
        peer.call(departures).unwrap();
        let unique = peer.name.clone();
        let (hosts, items) = (Arc::new(Mutex::new(Vec::<String>::new())), Arc::new(Mutex::new(Vec::<String>::new())));
        let (h, i) = (hosts.clone(), items.clone());
        let (stop, stopped) = mpsc::channel();
        let thread = thread::spawn(move || {
            let signal = |member: &str| Message::new_signal(WATCHER_PATH, WATCHER_INTERFACE, member).unwrap();
            while stopped.try_recv().is_err() {
                let Ok(m) = peer.channel.blocking_pop_message(POLL) else { return };
                let Some(m) = m else { continue };
                if m.msg_type() == MessageType::Signal {
                    if m.member().as_deref() == Some("NameOwnerChanged")
                        && let Ok((name, _, new)) = m.read3::<String, String, String>()
                        && new.is_empty()
                    {
                        // Entries are "<service><path>": the service is everything before the first '/'.
                        let mut list = i.lock().unwrap();
                        let gone: Vec<String> = list.iter().filter(|e| e.split('/').next() == Some(name.as_str())).cloned().collect();
                        list.retain(|e| !gone.contains(e));
                        drop(list);
                        for entry in gone {
                            let _ = peer.channel.send(signal("StatusNotifierItemUnregistered").append1(entry));
                        }
                        // A departed host is dropped too (no signal of its own: hosts have none).
                        h.lock().unwrap().retain(|host| host != &name);
                        peer.channel.flush();
                    }
                    continue;
                }
                if m.msg_type() != MessageType::MethodCall {
                    continue;
                }
                let call = m;
                let member = call.member().map(|x| x.to_string()).unwrap_or_default();
                let sender = call.sender().map(|x| x.to_string()).unwrap_or_default();
                let reply = match member.as_str() {
                    "RegisterStatusNotifierHost" => {
                        let host: String = call.read1().unwrap_or_default();
                        h.lock().unwrap().push(host);
                        let _ = peer.channel.send(call.method_return());
                        Some(signal("StatusNotifierHostRegistered"))
                    }
                    "RegisterStatusNotifierItem" => {
                        let service: String = call.read1().unwrap_or_default();
                        // A path-only or combined registration names no separate bus name: the
                        // sender's own unique name already backs it. A bare well-known name must
                        // actually be owned (by anyone — real watchers accept registering another
                        // connection's name) or the registration is refused, never recorded.
                        let owned = service.starts_with('/') || service.contains('/') || {
                            let owner = Message::new_method_call(
                                "org.freedesktop.DBus",
                                "/org/freedesktop/DBus",
                                "org.freedesktop.DBus",
                                "GetNameOwner",
                            )
                            .unwrap()
                            .append1(service.as_str());
                            peer.call(owner).is_ok()
                        };
                        if !owned {
                            let _ = peer.channel.send(error(&call, "org.freedesktop.DBus.Error.NameHasNoOwner"));
                            None
                        } else {
                            // Plasma's "<service><path>".
                            let entry = if service.starts_with('/') {
                                format!("{sender}{service}")
                            } else if service.contains('/') {
                                service
                            } else {
                                format!("{service}{DEFAULT_ITEM_PATH}")
                            };
                            let mut list = i.lock().unwrap();
                            let is_new = !list.contains(&entry);
                            if is_new {
                                list.push(entry.clone());
                            }
                            drop(list);
                            let _ = peer.channel.send(call.method_return());
                            // A duplicate registration is acknowledged but never announced again.
                            is_new.then(|| signal("StatusNotifierItemRegistered").append1(entry))
                        }
                    }
                    "Get" => {
                        let (_, name): (String, String) = call.read2().unwrap_or_default();
                        let r = match name.as_str() {
                            "RegisteredStatusNotifierItems" => call.method_return().append1(Variant(i.lock().unwrap().clone())),
                            "IsStatusNotifierHostRegistered" => call.method_return().append1(Variant(!h.lock().unwrap().is_empty())),
                            "ProtocolVersion" => call.method_return().append1(Variant(0i32)),
                            _ => error(&call, "org.freedesktop.DBus.Error.UnknownProperty"),
                        };
                        let _ = peer.channel.send(r);
                        None
                    }
                    "GetAll" => {
                        let mut r = call.method_return();
                        IterAppend::new(&mut r).append_dict(&signature("s"), &signature("v"), |d| {
                            d.append_dict_entry(|e| {
                                e.append("RegisteredStatusNotifierItems");
                                e.append(Variant(i.lock().unwrap().clone()));
                            });
                            d.append_dict_entry(|e| {
                                e.append("IsStatusNotifierHostRegistered");
                                e.append(Variant(!h.lock().unwrap().is_empty()));
                            });
                            d.append_dict_entry(|e| {
                                e.append("ProtocolVersion");
                                e.append(Variant(0i32));
                            });
                        });
                        let _ = peer.channel.send(r);
                        None
                    }
                    _ => {
                        let _ = peer.channel.send(error(&call, "org.freedesktop.DBus.Error.UnknownMethod"));
                        None
                    }
                };
                if let Some(s) = reply {
                    let _ = peer.channel.send(s);
                }
                peer.channel.flush();
            }
        });
        FakeWatcher { unique, hosts, items, stop, thread: Some(thread) }
    }

    pub fn hosts(&self) -> Vec<String> {
        self.hosts.lock().unwrap().clone()
    }

    pub fn items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }

    pub fn kill(self) {
        drop(self);
    }
}

impl Drop for FakeWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
