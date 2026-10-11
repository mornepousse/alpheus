//! The system bus through dbus-rs (the `dbus` crate over libdbus, its
//! blocking `Channel`): the ONLY file of the daemon that knows the D-Bus
//! library (tests/guardrails.rs). One connection, on the D-Bus thread, and
//! no thread of its own (libdbus has none): a method call waits for its
//! reply at most `TIMEOUT`, `next_signal` sleeps on the socket with no
//! timeout at all — nothing wakes the thread while the bus is quiet.
//! Replies are read in place with libdbus's iterators: a type the bar
//! doesn't read is skipped, never decoded.

use super::props::{Objects, Prop, Props};
use super::{Bus, Domain, Signal, UPOWER, bluez, network, upower};
use dbus::Message;
use dbus::arg::{ArgType, Iter};
use dbus::channel::{BusType, Channel};
use dbus::message::MessageType;
use dbus::strings::{BusName, ErrorName, Interface, Path, Signature};
use std::collections::BTreeMap;
use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
/// A service that doesn't answer blocks this thread at most this long.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// The most dictionaries kept from an `aa{sv}` (power-profiles-daemon offers three profiles).
pub const MAX_DICTS: usize = 16;

/// Every method the daemon may call: reads, and the subscription to
/// signals (`Hello` is libdbus's own, sent once when connecting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    GetNameOwner,
    AddMatch,
    GetAll,
    GetManagedObjects,
    EnumerateDevices,
    GetDevices,
}

impl Method {
    pub const ALL: [Method; 6] =
        [Method::GetNameOwner, Method::AddMatch, Method::GetAll, Method::GetManagedObjects, Method::EnumerateDevices, Method::GetDevices];

    pub fn name(self) -> &'static str {
        match self {
            Method::GetNameOwner => "GetNameOwner",
            Method::AddMatch => "AddMatch",
            Method::GetAll => "GetAll",
            Method::GetManagedObjects => "GetManagedObjects",
            Method::EnumerateDevices => "EnumerateDevices",
            Method::GetDevices => "GetDevices",
        }
    }

    fn interface(self) -> &'static str {
        match self {
            Method::GetNameOwner | Method::AddMatch => DBUS,
            Method::GetAll => "org.freedesktop.DBus.Properties",
            Method::GetManagedObjects => "org.freedesktop.DBus.ObjectManager",
            Method::EnumerateDevices => UPOWER,
            Method::GetDevices => network::INTERFACE,
        }
    }
}

/// The signals the bar listens to, by domain: a connection subscribes to
/// its domains' rules only. The bus sends us nothing else (but the
/// NameAcquired every client receives). BlueZ's PropertiesChanged is split
/// by interface (arg0): a headset's volume (MediaTransport1) or track
/// (MediaPlayer1) never reaches the daemon.
pub const MATCH_RULES: [(Domain, &str); 13] = [
    (
        Domain::Network,
        "type='signal',sender='org.freedesktop.NetworkManager',path='/org/freedesktop/NetworkManager',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged'",
    ),
    (
        Domain::Network,
        "type='signal',sender='org.freedesktop.NetworkManager',path_namespace='/org/freedesktop/NetworkManager/ActiveConnection',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged'",
    ),
    (
        Domain::Battery,
        // Narrowed to the device objects (M5, review): UPower's own root
        // object never sends PropertiesChanged the bar would read. This
        // still matches BAT0 and the line power supplies — a path_namespace
        // can't exclude a few device names — so `Signal::domain` drops
        // those itself (`upower_device_ignored`).
        "type='signal',sender='org.freedesktop.UPower',path_namespace='/org/freedesktop/UPower/devices',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged'",
    ),
    (Domain::Battery, "type='signal',sender='org.freedesktop.UPower',interface='org.freedesktop.UPower'"),
    (
        Domain::Bluetooth,
        "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.bluez.Adapter1'",
    ),
    (
        Domain::Bluetooth,
        "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.bluez.Device1'",
    ),
    (
        Domain::Bluetooth,
        "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.bluez.Battery1'",
    ),
    (Domain::Bluetooth, "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.ObjectManager'"),
    (
        Domain::Power,
        "type='signal',sender='net.hadess.PowerProfiles',path='/net/hadess/PowerProfiles',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='net.hadess.PowerProfiles'",
    ),
    (
        Domain::Network,
        "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.NetworkManager'",
    ),
    (
        Domain::Battery,
        "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.UPower'",
    ),
    (
        Domain::Bluetooth,
        "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.bluez'",
    ),
    (
        Domain::Power,
        "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='net.hadess.PowerProfiles'",
    ),
];

/// Resuming from sleep (not suspending): every shown domain may have
/// changed while the machine slept. Added once, regardless of which
/// domains are shown — `Signal::is_wake` marks it, `session` re-reads all
/// of `domains`. Reading only: never a call to logind (Suspend, Hibernate,
/// PowerOff… the guardrail already forbids those words in this directory).
pub const LOGIN1_RULE: &str = "type='signal',sender='org.freedesktop.login1',path='/org/freedesktop/login1',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'";

/// D-Bus errors meaning "nothing there": the service or the object is gone.
const ABSENT: [&str; 4] = [
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
    "org.freedesktop.DBus.Error.UnknownObject",
    "org.freedesktop.DBus.Error.UnknownMethod",
];

pub fn kind_of(error_name: &str) -> io::ErrorKind {
    if ABSENT.contains(&error_name) { io::ErrorKind::NotFound } else { io::ErrorKind::Other }
}

fn unexpected(what: &str) -> io::Error {
    io::Error::other(format!("unexpected reply: {what}"))
}

/// A failed call, by the D-Bus error libdbus reports: an absence
/// (`kind_of`), a timeout (`NoReply`), the connection gone
/// (`Disconnected`), or anything else — an outage.
fn error(e: dbus::Error) -> io::Error {
    let name = e.name().unwrap_or("");
    let kind = match name {
        "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => io::ErrorKind::TimedOut,
        "org.freedesktop.DBus.Error.Disconnected" => io::ErrorKind::UnexpectedEof,
        _ => kind_of(name),
    };
    io::Error::new(kind, format!("{name}: {}", e.message().unwrap_or("")))
}

/// A failed call's outcome, `error` reduced further by whether the
/// connection itself is still there afterwards. A disconnect while a call
/// is outstanding can complete it with `NoReply` — the same error name a
/// silent-but-alive service's timeout also uses (`errors_are_told_by_their_name`)
/// — which `error` alone would read back as `TimedOut`, an outage
/// `read_all` would blame on just that one domain (`Outage::Domain`).
/// `connected` wins over the error's own name: not connected always means
/// `UnexpectedEof`, so `read_all` reports `Outage::Bus` instead — the
/// whole state is stale, not just this call's.
fn call_error(e: dbus::Error, connected: bool) -> io::Error {
    if connected { error(e) } else { io::Error::new(io::ErrorKind::UnexpectedEof, "system bus closed mid-call") }
}

/// The signature of the argument at `it`, `None` past the last one: dbus
/// 0.9.12's `Iter::signature` panics there (`Signature::new("").unwrap()` on
/// an empty string) — an empty or short body, which any local process may
/// send the bar as a unicast signal. Returns the `Signature` itself, not a
/// `String`: comparing it against a literal (`sig(it).as_deref() ==
/// Some("a{sv}")`) goes through its `Deref<Target = str>`, no copy beyond
/// the one `Iter::signature` already makes.
pub(crate) fn sig(it: &mut Iter<'_>) -> Option<Signature<'static>> {
    if it.arg_type() == ArgType::Invalid { None } else { Some(it.signature()) }
}

/// A `Path`'s string, with the one allocation `into_cstring` already makes
/// (`Path::to_string()` goes through `Display`, which copies again).
fn path_string(p: Path<'_>) -> String {
    p.into_cstring().into_string().unwrap_or_default()
}

/// One property (`v` points inside its variant), reduced to what the bar
/// reads (same mapping as the busctl fixtures' `props::busctl`): only these
/// types are ever decoded.
pub fn prop(v: &mut Iter<'_>) -> Prop {
    let int = |n: Option<i64>| n.map(Prop::Int).unwrap_or(Prop::Other);
    match v.arg_type() {
        ArgType::Boolean => v.get::<bool>().map(Prop::Bool).unwrap_or(Prop::Other),
        ArgType::Byte => int(v.get::<u8>().map(i64::from)),
        ArgType::Int16 => int(v.get::<i16>().map(i64::from)),
        ArgType::UInt16 => int(v.get::<u16>().map(i64::from)),
        ArgType::Int32 => int(v.get::<i32>().map(i64::from)),
        ArgType::UInt32 => int(v.get::<u32>().map(i64::from)),
        ArgType::Int64 => int(v.get::<i64>()),
        ArgType::UInt64 => int(v.get::<u64>().and_then(|n| i64::try_from(n).ok())),
        ArgType::Double => v.get::<f64>().map(Prop::Float).unwrap_or(Prop::Other),
        ArgType::String => v.get::<&str>().map(|s| Prop::Str(s.to_string())).unwrap_or(Prop::Other),
        ArgType::ObjectPath => v.get::<Path>().map(|p| Prop::Path(path_string(p))).unwrap_or(Prop::Other),
        ArgType::Array => match sig(v).as_deref() {
            Some("ao") => v.get::<Vec<Path>>().map(|ps| Prop::Paths(ps.into_iter().map(path_string).collect())).unwrap_or(Prop::Other),
            Some("ay") => v.get::<&[u8]>().map(|b| Prop::Bytes(b.to_vec())).unwrap_or(Prop::Other),
            Some("aa{sv}") => dicts(v),
            _ => Prop::Other,
        },
        ArgType::Variant => v.recurse(ArgType::Variant).map(|mut inner| prop(&mut inner)).unwrap_or(Prop::Other),
        _ => Prop::Other,
    }
}

/// `a{sv}` at `it`: each property, inserted directly into the map (no
/// intermediate `Vec`).
fn props(it: &mut Iter<'_>) -> io::Result<Props> {
    if sig(it).as_deref() != Some("a{sv}") {
        return Err(unexpected("not a{sv}"));
    }
    let mut out = BTreeMap::new();
    let Some(mut entries) = it.recurse(ArgType::Array) else { return Ok(Props(out)) };
    while let Some(mut entry) = entries.recurse(ArgType::DictEntry) {
        let key: &str = entry.read().map_err(|e| unexpected(&e.to_string()))?;
        let mut value = entry.recurse(ArgType::Variant).ok_or_else(|| unexpected("a property without a variant"))?;
        out.insert(key.to_string(), prop(&mut value));
        entries.next();
    }
    Ok(Props(out))
}

/// `aa{sv}` at `v`: each dictionary read as `props` reads a GetAll reply, at most MAX_DICTS; one
/// that does not parse makes the whole value `Other` (never read).
fn dicts(v: &mut Iter<'_>) -> Prop {
    let Some(mut items) = v.recurse(ArgType::Array) else { return Prop::Other };
    let mut out = Vec::new();
    while out.len() < MAX_DICTS && items.arg_type() != ArgType::Invalid {
        match props(&mut items) {
            Ok(p) => out.push(p),
            Err(_) => return Prop::Other,
        }
        items.next();
    }
    Prop::Dicts(out)
}

/// A `GetAll` reply (`a{sv}`).
pub fn get_all_reply(m: &Message) -> io::Result<Props> {
    props(&mut m.iter_init())
}

/// A `GetManagedObjects` reply (`a{oa{sa{sv}}}`), keeping only the
/// interfaces the readers (bluez.rs) ever look at: Introspectable, Media1,
/// LEAdvertisingManager1… are skipped, never decoded.
pub fn objects_reply(m: &Message) -> io::Result<Objects> {
    const READ: [&str; 3] = [bluez::ADAPTER, bluez::DEVICE, bluez::BATTERY];
    let mut it = m.iter_init();
    if sig(&mut it).as_deref() != Some("a{oa{sa{sv}}}") {
        return Err(unexpected("not a{oa{sa{sv}}}"));
    }
    let mut objects = Objects::new();
    let Some(mut paths) = it.recurse(ArgType::Array) else { return Ok(objects) };
    while let Some(mut entry) = paths.recurse(ArgType::DictEntry) {
        let path: Path = entry.read().map_err(|e| unexpected(&e.to_string()))?;
        let mut read = BTreeMap::new();
        if let Some(mut interfaces) = entry.recurse(ArgType::Array) {
            while let Some(mut i) = interfaces.recurse(ArgType::DictEntry) {
                let name: &str = i.read().map_err(|e| unexpected(&e.to_string()))?;
                if READ.contains(&name) {
                    read.insert(name.to_string(), props(&mut i)?);
                }
                interfaces.next();
            }
        }
        objects.insert(path_string(path), read);
        paths.next();
    }
    Ok(objects)
}

/// A reply carrying object paths (`ao`): `EnumerateDevices`. The signature
/// is checked first: dbus 0.9.12's `Vec<T>::get` stops silently at the
/// first mismatching element, so a reply typed `as`/`ai`/`a{sv}`… would
/// otherwise read back as "no devices" instead of the outage it is.
pub fn paths_reply(m: &Message) -> io::Result<Vec<String>> {
    let mut it = m.iter_init();
    if sig(&mut it).as_deref() != Some("ao") {
        return Err(unexpected("not ao"));
    }
    let v: Vec<Path> = it.read().map_err(|e| unexpected(&e.to_string()))?;
    Ok(v.into_iter().map(path_string).collect())
}

/// The keys of the `a{sv}` (or `a{sa{sv}}`) at `it`, values skipped.
fn keys(it: &mut Iter<'_>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(mut entries) = it.recurse(ArgType::Array) {
        while let Some(mut entry) = entries.recurse(ArgType::DictEntry) {
            if let Some(k) = entry.get::<&str>() {
                out.push(k.to_string());
            }
            entries.next();
        }
    }
    out
}

/// What `Signal::domain` needs from a received signal: its path, interface
/// and member, and the first string argument and the names of the few
/// members it reads. A body that doesn't parse gives nothing (the signal
/// counts). Changed values are never decoded.
pub fn signal(m: &Message) -> Signal {
    let member = m.member().map(|x| x.to_string()).unwrap_or_default();
    let mut it = m.iter_init();
    let (arg0, names) = match member.as_str() {
        "NameOwnerChanged" => (it.get::<&str>().map(String::from), Vec::new()),
        "PropertiesChanged" if sig(&mut it).as_deref() == Some("s") => {
            let interface = it.get::<&str>().map(String::from);
            it.next();
            if sig(&mut it).as_deref() != Some("a{sv}") {
                (None, Vec::new())
            } else {
                let mut names = keys(&mut it);
                it.next();
                names.extend(it.get::<Vec<String>>().unwrap_or_default());
                (interface, names)
            }
        }
        "InterfacesAdded" if sig(&mut it).as_deref() == Some("o") => {
            it.next();
            (None, if sig(&mut it).as_deref() == Some("a{sa{sv}}") { keys(&mut it) } else { Vec::new() })
        }
        "InterfacesRemoved" => (None, m.read2::<Path, Vec<String>>().map(|(_, i)| i).unwrap_or_default()),
        // logind's bool argument, kept as "true"/"false": `Signal::is_wake` reads it.
        "PrepareForSleep" => (it.get::<bool>().map(|b| b.to_string()), Vec::new()),
        _ => (None, Vec::new()),
    };
    Signal {
        path: m.path().map(|p| p.to_string()).unwrap_or_default(),
        interface: m.interface().map(|i| i.to_string()).unwrap_or_default(),
        member,
        arg0,
        names,
    }
}

/// The message of a call, never allowed to auto-start its destination: a
/// second lock on D-Bus activation, besides calling unique names only.
/// `Message::new_method_call` takes its arguments through `Into<BusName>` /
/// `Into<Path>` / `Into<Interface>`, whose `&str` impl *unwraps* (dbus-rs
/// 0.9.12, `strings.rs`): validating here first turns a malformed
/// `destination` or `path` into an `io::Error`, never a panic of the D-Bus
/// thread.
pub fn message(destination: &str, path: &str, m: Method, arg: Option<&str>) -> io::Result<Message> {
    let destination = BusName::new(destination).map_err(io::Error::other)?;
    let path = Path::new(path).map_err(io::Error::other)?;
    let interface = Interface::new(m.interface()).map_err(io::Error::other)?;
    let mut msg = Message::new_method_call(destination, path, interface, m.name()).map_err(io::Error::other)?;
    msg.set_auto_start(false);
    Ok(match arg {
        Some(a) => msg.append1(a),
        None => msg,
    })
}

/// The answer to a method call addressed to the bar: nobody may call it.
pub fn refusal(call: &Message) -> Message {
    let name = ErrorName::from("org.freedesktop.DBus.Error.UnknownMethod");
    call.error(&name, c"the bar answers no call")
}

pub struct DbusRsBus {
    channel: Channel,
}

impl DbusRsBus {
    /// The system bus — libdbus reads `$DBUS_SYSTEM_BUS_ADDRESS`, else the
    /// standard socket — subscribed to the signals of `domains` only.
    pub fn system(domains: &[Domain]) -> io::Result<DbusRsBus> {
        Self::subscribe(Channel::get_private(BusType::System).map_err(error)?, domains)
    }

    /// The bus at `address` (tests: a private dbus-daemon).
    pub fn at(address: &str, domains: &[Domain]) -> io::Result<DbusRsBus> {
        let mut channel = Channel::open_private(address).map_err(error)?;
        channel.register().map_err(error)?;
        Self::subscribe(channel, domains)
    }

    /// Connecting and `Hello` are blocking (libdbus's own deadlines); then
    /// the match rules, each with `TIMEOUT`.
    fn subscribe(channel: Channel, domains: &[Domain]) -> io::Result<DbusRsBus> {
        let bus = DbusRsBus { channel };
        for (_, rule) in MATCH_RULES.iter().filter(|(d, _)| domains.contains(d)) {
            bus.call(DBUS, DBUS_PATH, Method::AddMatch, Some(rule))?;
        }
        // Not tied to one Domain: a resume re-reads every domain in `domains`.
        if !domains.is_empty() {
            bus.call(DBUS, DBUS_PATH, Method::AddMatch, Some(LOGIN1_RULE))?;
        }
        Ok(bus)
    }

    /// This connection's unique name on the bus.
    // Test-only: used by `tests/dbus_private_bus.rs` to address a call at
    // the bar itself (a call it must refuse). No production code reads it.
    #[doc(hidden)]
    pub fn unique_name(&self) -> &str {
        self.channel.unique_name().unwrap_or("")
    }

    /// For tests only: the connection's socket, which the private bus test
    /// checks is close-on-exec (libdbus opens it with SOCK_CLOEXEC).
    // Test-only: see above.
    #[doc(hidden)]
    pub fn socket_fd(&mut self) -> RawFd {
        self.channel.set_watch_enabled(true);
        let fd = self.channel.watch().fd;
        self.channel.set_watch_enabled(false);
        fd
    }

    /// The only way out of this file: a `Method`, never a free method name.
    fn call(&self, destination: &str, path: &str, m: Method, arg: Option<&str>) -> io::Result<Message> {
        self.channel
            .send_with_reply_and_block(message(destination, path, m, arg)?, TIMEOUT)
            .map_err(|e| call_error(e, self.channel.is_connected()))
    }
}

impl Bus for DbusRsBus {
    fn owner(&mut self, service: &str) -> io::Result<Option<String>> {
        match self.call(DBUS, DBUS_PATH, Method::GetNameOwner, Some(service)) {
            Ok(m) => m.read1::<&str>().map(|s| Some(s.to_string())).map_err(|e| unexpected(&e.to_string())),
            // NameHasNoOwner: nobody runs it — and asking the bus driver
            // never starts it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn get_all(&mut self, destination: &str, path: &str, interface: &str) -> io::Result<Props> {
        get_all_reply(&self.call(destination, path, Method::GetAll, Some(interface))?)
    }

    fn managed_objects(&mut self, destination: &str) -> io::Result<Objects> {
        objects_reply(&self.call(destination, "/", Method::GetManagedObjects, None)?)
    }

    fn enumerate_devices(&mut self, destination: &str) -> io::Result<Vec<String>> {
        paths_reply(&self.call(destination, upower::PATH, Method::EnumerateDevices, None)?)
    }

    fn get_devices(&mut self, destination: &str) -> io::Result<Vec<String>> {
        paths_reply(&self.call(destination, network::PATH, Method::GetDevices, None)?)
    }

    fn next_signal(&mut self) -> io::Result<Signal> {
        loop {
            while let Some(m) = self.channel.pop_message() {
                match m.msg_type() {
                    MessageType::Signal if m.interface().is_some_and(|i| &*i == "org.freedesktop.DBus.Local") => {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "system bus closed"));
                    }
                    MessageType::Signal => return Ok(signal(&m)),
                    // Refused at once, never left queued (the queue would only grow).
                    MessageType::MethodCall if !m.get_no_reply() => {
                        let _ = self.channel.send(refusal(&m));
                    }
                    // A late reply (its call already timed out): dropped.
                    _ => {}
                }
            }
            // None: asleep on the socket until the bus sends something.
            if self.channel.read_write(None).is_err() || !self.channel.is_connected() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "system bus closed"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::dbus::busctl_wire::push_reply;
    use crate::sources::dbus::props::busctl;

    /// A message carrying a recorded reply, as the service would send it.
    fn recorded(json: &str) -> Message {
        let mut m = Message::new_method_call(":1.10", "/", "org.example.Test", "Get").unwrap();
        push_reply(&mut m, json);
        m
    }

    #[test]
    fn every_recorded_reply_decodes_like_busctl() {
        const GET_ALL: [&str; 9] = [
            include_str!("../../../tests/fixtures/dbus-nm-root.json"),
            include_str!("../../../tests/fixtures/dbus-nm-active-10.json"),
            include_str!("../../../tests/fixtures/dbus-nm-active-9.json"),
            include_str!("../../../tests/fixtures/dbus-nm-active-3.json"),
            include_str!("../../../tests/fixtures/dbus-nm-active-2.json"),
            include_str!("../../../tests/fixtures/dbus-upower-display.json"),
            include_str!("../../../tests/fixtures/dbus-upower-bat0.json"),
            include_str!("../../../tests/fixtures/dbus-upower-mouse.json"),
            include_str!("../../../tests/fixtures/dbus-upower-ac.json"),
        ];
        for json in GET_ALL {
            assert_eq!(get_all_reply(&recorded(json)).unwrap(), busctl::get_all(json), "{json}");
        }
        let devices = include_str!("../../../tests/fixtures/dbus-upower-devices.json");
        assert_eq!(paths_reply(&recorded(devices)).unwrap(), busctl::paths(devices));
        let bluez = include_str!("../../../tests/fixtures/dbus-bluez-objects.json");
        let mut expected = busctl::objects(bluez);
        for interfaces in expected.values_mut() {
            interfaces.retain(|i, _| [bluez::ADAPTER, bluez::DEVICE, bluez::BATTERY].contains(&i.as_str()));
        }
        assert_eq!(objects_reply(&recorded(bluez)).unwrap(), expected);
    }

    #[test]
    fn an_array_of_dictionaries_decodes_like_busctl_and_is_capped() {
        let json = include_str!("../../../tests/fixtures/dbus-powerprofiles.json");
        let p = get_all_reply(&recorded(json)).unwrap();
        assert_eq!(p, busctl::get_all(json));
        let names: Vec<&str> = p.dicts("Profiles").iter().filter_map(|d| d.str("Profile")).collect();
        assert_eq!(names, ["power-saver", "balanced", "performance"]);
        let many = format!(
            r#"{{"type":"a{{sv}}","data":[{{"L":{{"type":"aa{{sv}}","data":[{}]}}}}]}}"#,
            vec![r#"{"K":{"type":"u","data":1}}"#; 20].join(",")
        );
        assert_eq!(get_all_reply(&recorded(&many)).unwrap().dicts("L").len(), MAX_DICTS, "capped");
    }

    #[test]
    fn the_power_profiles_rules_name_their_service() {
        let power: Vec<&str> = MATCH_RULES.iter().filter(|(d, _)| *d == Domain::Power).map(|(_, r)| *r).collect();
        assert_eq!(power.len(), 2);
        assert!(power.iter().all(|r| r.contains("'net.hadess.PowerProfiles'")), "{power:?}");
        assert!(power[0].contains("path='/net/hadess/PowerProfiles'") && power[0].contains("member='PropertiesChanged'"), "{power:?}");
        assert!(power[1].contains("member='NameOwnerChanged'"), "{power:?}");
    }

    #[test]
    fn values_converted_to_props() {
        let p = get_all_reply(&recorded(
            r#"{"type":"a{sv}","data":[{"On":{"type":"b","data":true},"N":{"type":"u","data":42},"Y":{"type":"y","data":72},
            "T":{"type":"t","data":1791131213},"Big":{"type":"t","data":18446744073709551615},"P":{"type":"d","data":92.5},
            "S":{"type":"s","data":"MON_WIFI_1"},"O":{"type":"o","data":"/a"},"L":{"type":"ao","data":["/a","/b"]},
            "B":{"type":"ay","data":[77,79]},"V":{"type":"v","data":{"type":"d","data":0.5}},"Strs":{"type":"as","data":["a","b"]}}]}"#,
        ))
        .unwrap();
        assert_eq!(p.0["On"], Prop::Bool(true));
        assert_eq!(p.0["N"], Prop::Int(42));
        assert_eq!(p.0["Y"], Prop::Int(72));
        assert_eq!(p.0["T"], Prop::Int(1_791_131_213));
        assert_eq!(p.0["Big"], Prop::Other, "beyond i64: never read");
        assert_eq!(p.0["P"], Prop::Float(92.5));
        assert_eq!(p.0["S"], Prop::Str("MON_WIFI_1".into()));
        assert_eq!(p.0["O"], Prop::Path("/a".into()));
        assert_eq!(p.0["L"], Prop::Paths(vec!["/a".into(), "/b".into()]));
        assert_eq!(p.0["B"], Prop::Bytes(b"MO".to_vec()));
        assert_eq!(p.0["V"], Prop::Float(0.5), "a variant inside a variant");
        assert_eq!(p.0["Strs"], Prop::Other);
        assert!(get_all_reply(&recorded(r#"{"type":"ao","data":[["/a"]]}"#)).is_err(), "not a dictionary: an error, not empty props");
    }

    #[test]
    fn absent_services_and_objects_are_not_found() {
        for absent in ["ServiceUnknown", "NameHasNoOwner", "UnknownObject", "UnknownMethod"] {
            assert_eq!(kind_of(&format!("org.freedesktop.DBus.Error.{absent}")), io::ErrorKind::NotFound, "{absent}");
        }
        for outage in ["NoReply", "AccessDenied", "Timeout"] {
            assert_eq!(kind_of(&format!("org.freedesktop.DBus.Error.{outage}")), io::ErrorKind::Other, "{outage}");
        }
    }

    #[test]
    fn errors_are_told_by_their_name() {
        let kind = |name: &str| error(dbus::Error::new_custom(name, "text")).kind();
        assert_eq!(kind("org.freedesktop.DBus.Error.UnknownObject"), io::ErrorKind::NotFound);
        assert_eq!(kind("org.freedesktop.DBus.Error.ServiceUnknown"), io::ErrorKind::NotFound);
        assert_eq!(kind("org.freedesktop.DBus.Error.NoReply"), io::ErrorKind::TimedOut, "a silent service: an outage");
        assert_eq!(kind("org.freedesktop.DBus.Error.Disconnected"), io::ErrorKind::UnexpectedEof);
        assert_eq!(kind("org.freedesktop.DBus.Error.FileNotFound"), io::ErrorKind::Other, "no socket: an outage, not an absence");
        assert_eq!(kind("org.freedesktop.DBus.Error.AccessDenied"), io::ErrorKind::Other);
    }

    #[test]
    fn a_call_to_the_bar_is_answered_with_an_error() {
        let mut call = Message::new_method_call(":1.10", "/", "org.freedesktop.DBus.Peer", "Ping").unwrap();
        call.set_serial(7); // as received: libdbus aborts on a reply to serial 0
        let r = refusal(&call);
        assert_eq!(r.msg_type(), MessageType::Error);
        assert_eq!(r.read1::<&str>().unwrap(), "the bar answers no call");
        assert_eq!(r.get_reply_serial(), Some(7));
    }

    /// The private-bus test (`tests/dbus_private_bus.rs`,
    /// `a_disconnect_mid_call_is_an_outage_never_an_absence`) kills the
    /// whole bus mid-call and always observes libdbus complete the pending
    /// call with `Disconnected` (measured: sub-millisecond, every run) —
    /// already `UnexpectedEof` through `error` alone, so that test cannot
    /// by itself prove the `connected` check in `call_error` does anything.
    /// This test pins down the specific case the check exists for: a
    /// disconnect completing the call with `NoReply` instead (the name a
    /// silent-but-alive service's own timeout also uses) must still read
    /// back as `UnexpectedEof`, because `connected` is false — never the
    /// `TimedOut` that name would otherwise give.
    #[test]
    fn a_mid_call_disconnect_tagged_no_reply_is_still_unexpected_eof() {
        let e = dbus::Error::new_custom("org.freedesktop.DBus.Error.NoReply", "Connection was disconnected before a reply was received");
        assert_eq!(call_error(e, false).kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn message_rejects_an_invalid_path_without_panicking() {
        // "not a path": no leading '/' — `Path::new` rejects it; the old
        // `Into<Path>` conversion would instead unwrap and panic the
        // D-Bus thread.
        let err = message(":1.10", "not a path", Method::GetNameOwner, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn every_call_is_flagged_no_auto_start() {
        for m in Method::ALL {
            assert!(!message(":1.10", "/", m, None).unwrap().get_auto_start(), "{m:?}");
        }
    }

    fn received(interface: &str, member: &str, path: &str, json_args: &[&str]) -> Signal {
        let mut m = Message::new_signal(path, interface, member).unwrap();
        for a in json_args {
            push_reply(&mut m, a);
        }
        signal(&m)
    }

    #[test]
    fn signals_carry_what_domain_needs() {
        let s = received(
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            "/org/bluez/hci0/dev_AA",
            &[
                r#"{"type":"s","data":["org.bluez.Device1"]}"#,
                r#"{"type":"a{sv}","data":[{"RSSI":{"type":"n","data":-60}}]}"#,
                r#"{"type":"as","data":[["Alias"]]}"#,
            ],
        );
        assert_eq!(
            (s.path.as_str(), s.member.as_str(), s.arg0.as_deref()),
            ("/org/bluez/hci0/dev_AA", "PropertiesChanged", Some("org.bluez.Device1"))
        );
        let mut names = s.names.clone();
        names.sort();
        assert_eq!(names, ["Alias", "RSSI"]);
        let wake =
            received("org.freedesktop.login1.Manager", "PrepareForSleep", "/org/freedesktop/login1", &[r#"{"type":"b","data":[false]}"#]);
        assert!(wake.is_wake());
        let removed = received(
            "org.freedesktop.DBus.ObjectManager",
            "InterfacesRemoved",
            "/",
            &[r#"{"type":"o","data":["/org/bluez/hci0"]}"#, r#"{"type":"as","data":[["org.bluez.Adapter1"]]}"#],
        );
        assert_eq!(removed.names, ["org.bluez.Adapter1"]);
        let added = received(
            "org.freedesktop.DBus.ObjectManager",
            "InterfacesAdded",
            "/",
            &[
                r#"{"type":"o","data":["/org/bluez/hci0/dev_AA"]}"#,
                r#"{"type":"a{sa{sv}}","data":[{"org.bluez.Battery1":{"Percentage":{"type":"y","data":72}}}]}"#,
            ],
        );
        assert_eq!(added.names, ["org.bluez.Battery1"]);
        let owner = received(
            "org.freedesktop.DBus",
            "NameOwnerChanged",
            "/org/freedesktop/DBus",
            &[r#"{"type":"s","data":["org.bluez"]}"#, r#"{"type":"s","data":[""]}"#, r#"{"type":"s","data":[":1.12"]}"#],
        );
        assert_eq!(owner.arg0.as_deref(), Some("org.bluez"));
        let odd = received("org.freedesktop.DBus.Properties", "PropertiesChanged", "/x", &[r#"{"type":"u","data":[42]}"#]);
        assert_eq!((odd.arg0, odd.names.len()), (None, 0), "a body that doesn't parse gives nothing (the signal counts)");
        // Empty or cut bodies (any local process may send the bar a unicast
        // signal): nothing, never a panic of the D-Bus thread.
        let nothing = |s: Signal| (s.arg0, s.names);
        for (interface, member) in [
            ("org.freedesktop.DBus", "NameOwnerChanged"),
            ("org.freedesktop.DBus.Properties", "PropertiesChanged"),
            ("org.freedesktop.DBus.ObjectManager", "InterfacesAdded"),
            ("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved"),
            ("org.freedesktop.login1.Manager", "PrepareForSleep"),
        ] {
            assert_eq!(nothing(received(interface, member, "/x", &[])), (None, vec![]), "{member} with an empty body");
        }
        let cut = received("org.freedesktop.DBus.Properties", "PropertiesChanged", "/x", &[r#"{"type":"s","data":["org.bluez.Device1"]}"#]);
        assert_eq!(nothing(cut), (None, vec![]), "PropertiesChanged with its interface only");
        let cut = received("org.freedesktop.DBus.ObjectManager", "InterfacesAdded", "/", &[r#"{"type":"o","data":["/org/bluez/hci0"]}"#]);
        assert_eq!(nothing(cut), (None, vec![]), "InterfacesAdded with its path only");
    }

    #[test]
    fn empty_replies_are_errors() {
        let empty = Message::new_method_call(":1.10", "/", "org.example.Test", "Get").unwrap();
        assert!(get_all_reply(&empty).is_err());
        assert!(objects_reply(&empty).is_err());
        assert!(paths_reply(&empty).is_err());
    }

    #[test]
    fn paths_reply_requires_ao() {
        // dbus 0.9.12's `Vec<T>::get` stops silently at the first
        // mismatching element: without the signature guard, a reply typed
        // `as` would read back as an empty `Vec<Path>` — "no devices" —
        // instead of the outage it actually is.
        assert!(paths_reply(&recorded(r#"{"type":"as","data":[["x"]]}"#)).is_err());
    }
}
