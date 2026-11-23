//! A fake MPRIS player on a `PrivateBus`, for the tests only.
//!
//! It owns `org.mpris.MediaPlayer2.<suffix>`, allowing replacement, so that a second fake can take
//! the name over in ONE `NameOwnerChanged(name, old, new)`. That is how a restart through a
//! launcher looks on the bus.
//!
//! It serves `org.freedesktop.DBus.Properties.GetAll` and `Get` for the Player and root interfaces,
//! and `PlayPause`, `Next` and `Previous`. It records every call. `PlayPause` flips the status and
//! signals it. On command it emits `PropertiesChanged`, including an invalidated `Metadata`, and
//! `Seeked`.
//!
//! It is one `Peer` served by its own thread, like support/portal.rs. It speaks raw D-Bus and never
//! uses the daemon's code; its names are written out here on purpose, as a cross-check of the daemon's
//! constants. Fictional players and tracks only.

use super::{Peer, PrivateBus};
use dbus::Message;
use dbus::arg::IterAppend;
use dbus::message::MessageType;
use dbus::strings::{ErrorName, Path, Signature};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const PREFIX: &str = "org.mpris.MediaPlayer2.";
pub const OBJECT: &str = "/org/mpris/MediaPlayer2";
pub const ROOT: &str = "org.mpris.MediaPlayer2";
pub const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// How long the fake waits on its socket before looking at its commands (as support/portal.rs).
const POLL: Duration = Duration::from_millis(20);
/// RequestName flags (D-Bus specification).
const ALLOW_REPLACEMENT: u32 = 1;
const REPLACE_EXISTING: u32 = 2;

/// One property value, as the fake sends it inside its variant.
#[derive(Debug, Clone, PartialEq)]
pub enum Prop {
    Str(String),
    Strs(Vec<String>),
    Bool(bool),
    F64(f64),
    I64(i64),
    ObjPath(String),
    /// `a{sv}`: the Metadata map.
    Dict(Vec<(String, Prop)>),
}

pub fn text(s: &str) -> Prop {
    Prop::Str(s.into())
}

/// `title` by Marlowe Vane, 3:20 long (fictional).
pub fn metadata(title: &str) -> Prop {
    Prop::Dict(vec![
        ("mpris:trackid".into(), Prop::ObjPath("/org/mpris/MediaPlayer2/Track/1".into())),
        ("xesam:title".into(), text(title)),
        ("xesam:artist".into(), Prop::Strs(vec!["Marlowe Vane".into()])),
        ("mpris:length".into(), Prop::I64(200_000_000)),
    ])
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// After "org.mpris.MediaPlayer2.".
    pub suffix: String,
    /// The root interface's Identity; None: Get(Identity) answers UnknownProperty.
    pub identity: Option<String>,
    /// The Player interface's properties, in GetAll order.
    pub player: Vec<(String, Prop)>,
    /// Every Properties call answers this error name instead (ServiceUnknown, UnknownMethod…).
    pub fails_with: Option<&'static str>,
}

fn set(props: &mut Vec<(String, Prop)>, key: &str, v: Prop) {
    match props.iter_mut().find(|(k, _)| k == key) {
        Some((_, old)) => *old = v,
        None => props.push((key.into(), v)),
    }
}

impl Spec {
    /// Playing `title`, every control allowed, Identity the capitalised suffix.
    pub fn playing(suffix: &str, title: &str) -> Spec {
        let mut identity = suffix.to_string();
        if let Some(first) = identity.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        Spec {
            suffix: suffix.into(),
            identity: Some(identity),
            player: vec![
                ("PlaybackStatus".into(), text("Playing")),
                ("Metadata".into(), metadata(title)),
                ("Rate".into(), Prop::F64(1.0)),
                ("Position".into(), Prop::I64(42_000_000)),
                ("CanPlay".into(), Prop::Bool(true)),
                ("CanPause".into(), Prop::Bool(true)),
                ("CanGoNext".into(), Prop::Bool(true)),
                ("CanGoPrevious".into(), Prop::Bool(true)),
                ("CanControl".into(), Prop::Bool(true)),
            ],
            fails_with: None,
        }
    }

    pub fn paused(suffix: &str, title: &str) -> Spec {
        Spec::playing(suffix, title).with("PlaybackStatus", text("Paused"))
    }

    pub fn with(mut self, key: &str, v: Prop) -> Spec {
        set(&mut self.player, key, v);
        self
    }

    pub fn failing(mut self, error: &'static str) -> Spec {
        self.fails_with = Some(error);
        self
    }
}

fn signature(s: &str) -> Signature<'static> {
    Signature::new(s).unwrap()
}

fn type_of(p: &Prop) -> &'static str {
    match p {
        Prop::Str(_) => "s",
        Prop::Strs(_) => "as",
        Prop::Bool(_) => "b",
        Prop::F64(_) => "d",
        Prop::I64(_) => "x",
        Prop::ObjPath(_) => "o",
        Prop::Dict(_) => "a{sv}",
    }
}

fn append_bare(i: &mut IterAppend<'_>, p: &Prop) {
    match p {
        Prop::Str(s) => i.append(s.as_str()),
        Prop::Strs(v) => i.append(v.clone()),
        Prop::Bool(b) => i.append(*b),
        Prop::F64(x) => i.append(*x),
        Prop::I64(x) => i.append(*x),
        Prop::ObjPath(o) => i.append(Path::new(o.as_str()).unwrap()),
        Prop::Dict(entries) => append_dict(i, entries),
    }
}

fn append_variant(i: &mut IterAppend<'_>, p: &Prop) {
    i.append_variant(&signature(type_of(p)), |v| append_bare(v, p));
}

fn append_dict(i: &mut IterAppend<'_>, entries: &[(String, Prop)]) {
    i.append_dict(&signature("s"), &signature("v"), |d| {
        for (k, v) in entries {
            d.append_dict_entry(|e| {
                e.append(k.as_str());
                append_variant(e, v);
            });
        }
    });
}

fn error(call: &Message, name: &str) -> Message {
    call.error(&ErrorName::new(name.to_string()).unwrap(), c"fake player")
}

enum Command {
    Set(String, Prop),
    Invalidate(Prop),
    Seeked(i64),
    Stop,
}

struct Served {
    peer: Peer,
    spec: Spec,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Served {
    fn send(&self, m: Message) {
        let _ = self.peer.channel.send(m);
        self.peer.channel.flush();
    }

    /// PropertiesChanged(Player, changed, invalidated).
    fn changed(&self, changed: &[(String, Prop)], invalidated: &[&str]) {
        let mut m = Message::new_signal(OBJECT, PROPERTIES, "PropertiesChanged").unwrap();
        let mut i = IterAppend::new(&mut m);
        i.append(PLAYER);
        append_dict(&mut i, changed);
        i.append(invalidated.iter().map(|s| s.to_string()).collect::<Vec<String>>());
        self.send(m);
    }

    fn answer(&mut self, call: &Message) -> Option<Message> {
        let interface = call.interface().map(|i| i.to_string()).unwrap_or_default();
        let member = call.member().map(|x| x.to_string()).unwrap_or_default();
        let path = call.path().map(|p| p.to_string()).unwrap_or_default();
        let args = call
            .read2::<&str, &str>()
            .map(|(a, b)| format!("{a}, {b}"))
            .or_else(|_| call.read1::<&str>().map(String::from))
            .unwrap_or_default();
        self.calls.lock().unwrap().push(format!("{interface}.{member}({args})"));
        let reply = if path != OBJECT {
            error(call, "org.freedesktop.DBus.Error.UnknownObject")
        } else if let Some(e) = self.spec.fails_with.filter(|_| interface == PROPERTIES) {
            error(call, e)
        } else {
            match (interface.as_str(), member.as_str()) {
                (PROPERTIES, "GetAll") => match call.read1::<&str>() {
                    Ok(PLAYER) => {
                        let mut r = call.method_return();
                        append_dict(&mut IterAppend::new(&mut r), &self.spec.player);
                        r
                    }
                    Ok(ROOT) => {
                        let root: Vec<(String, Prop)> = self.spec.identity.iter().map(|id| ("Identity".to_string(), text(id))).collect();
                        let mut r = call.method_return();
                        append_dict(&mut IterAppend::new(&mut r), &root);
                        r
                    }
                    _ => error(call, "org.freedesktop.DBus.Error.InvalidArgs"),
                },
                (PROPERTIES, "Get") => {
                    let found = match call.read2::<&str, &str>() {
                        Ok((ROOT, "Identity")) => self.spec.identity.as_deref().map(text),
                        Ok((PLAYER, name)) => self.spec.player.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
                        _ => None,
                    };
                    match found {
                        Some(v) => {
                            let mut r = call.method_return();
                            append_variant(&mut IterAppend::new(&mut r), &v);
                            r
                        }
                        None => error(call, "org.freedesktop.DBus.Error.UnknownProperty"),
                    }
                }
                (PLAYER, "PlayPause") => {
                    let playing = self.spec.player.iter().any(|(k, v)| k == "PlaybackStatus" && *v == text("Playing"));
                    let now = text(if playing { "Paused" } else { "Playing" });
                    set(&mut self.spec.player, "PlaybackStatus", now.clone());
                    self.changed(&[("PlaybackStatus".into(), now)], &[]);
                    call.method_return()
                }
                (PLAYER, "Next" | "Previous") => call.method_return(),
                _ => error(call, "org.freedesktop.DBus.Error.UnknownMethod"),
            }
        };
        (!call.get_no_reply()).then_some(reply)
    }
}

fn serve(mut s: Served, commands: Receiver<Command>) {
    loop {
        while let Ok(c) = commands.try_recv() {
            match c {
                Command::Set(k, v) => {
                    set(&mut s.spec.player, &k, v.clone());
                    s.changed(&[(k, v)], &[]);
                }
                Command::Invalidate(meta) => {
                    set(&mut s.spec.player, "Metadata", meta);
                    s.changed(&[], &["Metadata"]);
                }
                Command::Seeked(us) => s.send(Message::new_signal(OBJECT, PLAYER, "Seeked").unwrap().append1(us)),
                Command::Stop => return,
            }
        }
        match s.peer.channel.blocking_pop_message(POLL) {
            Ok(Some(m)) if m.msg_type() == MessageType::MethodCall => {
                if let Some(r) = s.answer(&m) {
                    s.send(r);
                }
            }
            Ok(_) if s.peer.channel.is_connected() => {}
            _ => return,
        }
    }
}

pub struct FakePlayer {
    /// Its unique name (":1.N").
    pub unique: String,
    /// "org.mpris.MediaPlayer2.<suffix>".
    pub name: String,
    commands: Sender<Command>,
    calls: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl FakePlayer {
    /// Owns the name (allowing replacement) and serves `spec`.
    pub fn start(bus: &PrivateBus, spec: Spec) -> FakePlayer {
        Self::spawn(bus, spec, ALLOW_REPLACEMENT)
    }

    /// Takes the name over from the fake that owns it: the bus sends ONE
    /// NameOwnerChanged(name, old, new), as for a player restarted under the same name.
    pub fn replace(bus: &PrivateBus, spec: Spec) -> FakePlayer {
        Self::spawn(bus, spec, ALLOW_REPLACEMENT | REPLACE_EXISTING)
    }

    fn spawn(bus: &PrivateBus, spec: Spec, flags: u32) -> FakePlayer {
        let peer = Peer::connect(bus);
        let name = format!("{PREFIX}{}", spec.suffix);
        let request = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName")
            .unwrap()
            .append2(name.as_str(), flags);
        assert_eq!(peer.call(request).unwrap().read1::<u32>().unwrap(), 1, "primary owner of {name}");
        let unique = peer.name.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (commands, rx) = mpsc::channel();
        let served = Served { peer, spec, calls: calls.clone() };
        let thread = thread::spawn(move || serve(served, rx));
        FakePlayer { unique, name, commands, calls, thread: Some(thread) }
    }

    /// Changes one Player property and signals it.
    pub fn set(&self, key: &str, v: Prop) {
        let _ = self.commands.send(Command::Set(key.into(), v));
    }

    /// Changes the Metadata and signals it by name only (invalidated), as some players do.
    pub fn invalidate_metadata(&self, meta: Prop) {
        let _ = self.commands.send(Command::Invalidate(meta));
    }

    pub fn seeked(&self, us: i64) {
        let _ = self.commands.send(Command::Seeked(us));
    }

    /// Changes PlaybackStatus and signals it.
    pub fn set_status(&self, status: &str) {
        self.set("PlaybackStatus", text(status));
    }

    /// A new track, signalled with its value: `title` by `artist`, `length_us` long, its own trackid.
    pub fn set_track(&self, title: &str, artist: &str, length_us: i64) {
        self.set(
            "Metadata",
            Prop::Dict(vec![
                ("mpris:trackid".into(), Prop::ObjPath(format!("/org/mpris/MediaPlayer2/Track/{length_us}"))),
                ("xesam:title".into(), text(title)),
                ("xesam:artist".into(), Prop::Strs(vec![artist.into()])),
                ("mpris:length".into(), Prop::I64(length_us)),
            ]),
        );
    }

    /// Every method call received, "Interface.Member(arg, arg)", in arrival order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The first recorded call to `member`, waiting `timeout` at most.
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

    /// Drops the connection: NameOwnerChanged with an empty new owner. Returns what it recorded.
    pub fn kill(self) -> Vec<String> {
        let calls = self.calls.clone();
        drop(self);
        calls.lock().unwrap().clone()
    }
}

impl Drop for FakePlayer {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
