//! The system bus through zbus (blocking API): the ONLY file of the daemon
//! that knows zbus (tests/guardrails.rs). One connection, method calls with
//! a 2 s timeout, the signals of `MATCH_RULES` received in order — zbus's
//! blocking API spawns an executor thread per `Connection` (survey, zbus
//! 5.19's source), so a second one would cost a second thread.

use super::props::{Objects, Prop, Props};
use super::{Bus, Domain, Signal, UPOWER, bluez, upower};
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Value};

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
/// A service that doesn't answer blocks this thread at most this long.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// Messages zbus queues for us (its default: 64). Past it, zbus stops
/// reading the socket until we catch up — the replies of our own re-reads
/// included, which would then time out ("–"). The match rules keep the
/// traffic low; this is the margin for a burst.
pub const MAX_QUEUED: usize = 256;

/// Every method the daemon may call: reads, and the subscription to signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    GetNameOwner,
    AddMatch,
    GetAll,
    GetManagedObjects,
    EnumerateDevices,
}

impl Method {
    pub const ALL: [Method; 5] =
        [Method::GetNameOwner, Method::AddMatch, Method::GetAll, Method::GetManagedObjects, Method::EnumerateDevices];

    pub fn name(self) -> &'static str {
        match self {
            Method::GetNameOwner => "GetNameOwner",
            Method::AddMatch => "AddMatch",
            Method::GetAll => "GetAll",
            Method::GetManagedObjects => "GetManagedObjects",
            Method::EnumerateDevices => "EnumerateDevices",
        }
    }

    fn interface(self) -> &'static str {
        match self {
            Method::GetNameOwner | Method::AddMatch => DBUS,
            Method::GetAll => "org.freedesktop.DBus.Properties",
            Method::GetManagedObjects => "org.freedesktop.DBus.ObjectManager",
            Method::EnumerateDevices => UPOWER,
        }
    }
}

/// The signals the bar listens to, by domain: a connection subscribes to
/// its domains' rules only. The bus sends us nothing else (but the
/// NameAcquired every client receives). BlueZ's PropertiesChanged is split
/// by interface (arg0): a headset's volume (MediaTransport1) or track
/// (MediaPlayer1) never reaches the daemon.
pub const MATCH_RULES: [(Domain, &str); 11] = [
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

fn error(e: zbus::Error) -> io::Error {
    use zbus::fdo::Error as Fdo;
    let kind = match &e {
        zbus::Error::MethodError(name, _, _) => kind_of(name.as_str()),
        zbus::Error::FDO(f) => match **f {
            Fdo::ServiceUnknown(_) | Fdo::NameHasNoOwner(_) | Fdo::UnknownObject(_) | Fdo::UnknownMethod(_) => io::ErrorKind::NotFound,
            _ => io::ErrorKind::Other,
        },
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, e.to_string())
}

pub fn prop(v: &Value<'_>) -> Prop {
    match v {
        Value::Bool(b) => Prop::Bool(*b),
        Value::U8(n) => Prop::Int(i64::from(*n)),
        Value::I16(n) => Prop::Int(i64::from(*n)),
        Value::U16(n) => Prop::Int(i64::from(*n)),
        Value::I32(n) => Prop::Int(i64::from(*n)),
        Value::U32(n) => Prop::Int(i64::from(*n)),
        Value::I64(n) => Prop::Int(*n),
        Value::U64(n) => i64::try_from(*n).map(Prop::Int).unwrap_or(Prop::Other),
        Value::F64(f) => Prop::Float(*f),
        Value::Str(s) => Prop::Str(s.as_str().to_string()),
        Value::ObjectPath(p) => Prop::Path(p.as_str().to_string()),
        Value::Value(inner) => prop(inner),
        Value::Array(a) if *a.element_signature() == "o" => Prop::Paths(
            a.iter()
                .filter_map(|x| match x {
                    Value::ObjectPath(p) => Some(p.as_str().to_string()),
                    _ => None,
                })
                .collect(),
        ),
        Value::Array(a) if *a.element_signature() == "y" => Prop::Bytes(
            a.iter()
                .filter_map(|x| match x {
                    Value::U8(b) => Some(*b),
                    _ => None,
                })
                .collect(),
        ),
        _ => Prop::Other,
    }
}

fn props(map: &HashMap<String, OwnedValue>) -> Props {
    Props(map.iter().map(|(k, v)| (k.clone(), prop(v))).collect())
}

pub struct ZbusBus {
    conn: Connection,
    signals: MessageIterator,
}

impl ZbusBus {
    /// The system bus ($DBUS_SYSTEM_BUS_ADDRESS, else /run/dbus/system_bus_socket),
    /// subscribed to the signals of `domains` only.
    pub fn system(domains: &[Domain]) -> io::Result<ZbusBus> {
        Self::connect(Builder::system().map_err(error)?, domains)
    }

    /// The bus at `address` (tests: a private dbus-daemon).
    pub fn at(address: &str, domains: &[Domain]) -> io::Result<ZbusBus> {
        Self::connect(Builder::address(address).map_err(error)?, domains)
    }

    fn connect(b: Builder<'_>, domains: &[Domain]) -> io::Result<ZbusBus> {
        let conn = b.method_timeout(TIMEOUT).max_queued(MAX_QUEUED).build().map_err(error)?;
        // Before the rules: no signal lost between subscribing and reading.
        let signals = MessageIterator::from(&conn);
        let bus = ZbusBus { conn, signals };
        for (_, rule) in MATCH_RULES.iter().filter(|(d, _)| domains.contains(d)) {
            bus.call(DBUS, DBUS_PATH, Method::AddMatch, &(*rule,))?;
        }
        // Not tied to one Domain: a resume re-reads every domain in `domains`.
        if !domains.is_empty() {
            bus.call(DBUS, DBUS_PATH, Method::AddMatch, &(LOGIN1_RULE,))?;
        }
        Ok(bus)
    }

    /// The only way out of this file: a `Method`, never a free method name.
    fn call<B: serde::Serialize + DynamicType>(&self, service: &str, path: &str, m: Method, body: &B) -> io::Result<zbus::Message> {
        self.conn.call_method(Some(service), path, Some(m.interface()), m.name(), body).map_err(error)
    }
}

impl Bus for ZbusBus {
    fn owner(&mut self, service: &str) -> io::Result<Option<String>> {
        match self.call(DBUS, DBUS_PATH, Method::GetNameOwner, &(service,)) {
            Ok(m) => m.body().deserialize::<String>().map(Some).map_err(error),
            // NameHasNoOwner: nobody runs it — and asking the bus driver
            // never starts it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn get_all(&mut self, destination: &str, path: &str, interface: &str) -> io::Result<Props> {
        let m = self.call(destination, path, Method::GetAll, &(interface,))?;
        let map: HashMap<String, OwnedValue> = m.body().deserialize().map_err(error)?;
        Ok(props(&map))
    }

    fn managed_objects(&mut self, destination: &str) -> io::Result<Objects> {
        let m = self.call(destination, "/", Method::GetManagedObjects, &())?;
        let map: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> = m.body().deserialize().map_err(error)?;
        // Only the interfaces the readers (bluez.rs) ever look at: skip
        // Introspectable, Media1, LEAdvertisingManager1… (cheap, review).
        const READ: [&str; 3] = [bluez::ADAPTER, bluez::DEVICE, bluez::BATTERY];
        Ok(map
            .iter()
            .map(|(p, interfaces)| {
                (
                    p.as_str().to_string(),
                    interfaces.iter().filter(|(i, _)| READ.contains(&i.as_str())).map(|(i, v)| (i.clone(), props(v))).collect(),
                )
            })
            .collect())
    }

    fn enumerate_devices(&mut self, destination: &str) -> io::Result<Vec<String>> {
        let m = self.call(destination, upower::PATH, Method::EnumerateDevices, &())?;
        let v: Vec<OwnedObjectPath> = m.body().deserialize().map_err(error)?;
        Ok(v.iter().map(|p| p.as_str().to_string()).collect())
    }

    fn next_signal(&mut self) -> io::Result<Signal> {
        loop {
            let m = match self.signals.next() {
                None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "system bus closed")),
                Some(r) => r.map_err(error)?,
            };
            if m.message_type() != MessageType::Signal {
                continue; // a method reply also passes through here
            }
            let h = m.header();
            let member = h.member().map(|x| x.as_str().to_string()).unwrap_or_default();
            let body = m.body();
            // What `Signal::domain` needs to tell a change that matters from
            // noise; a body that doesn't parse gives nothing (the signal counts).
            let (arg0, names) = match member.as_str() {
                "NameOwnerChanged" => (body.deserialize::<(String, String, String)>().ok().map(|(n, _, _)| n), Vec::new()),
                // Only the changed properties' names matter to `Signal::domain`
                // (never their values): borrowed `Value`s, never turned into
                // `OwnedValue` (an extra clone each) just to be discarded
                // (cheap, review).
                "PropertiesChanged" => match body.deserialize::<(String, HashMap<String, Value<'_>>, Vec<String>)>() {
                    Ok((interface, changed, invalidated)) => (Some(interface), changed.into_keys().chain(invalidated).collect()),
                    Err(_) => (None, Vec::new()),
                },
                "InterfacesAdded" => {
                    let added = body.deserialize::<(OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>)>();
                    (None, added.map(|(_, interfaces)| interfaces.into_keys().collect()).unwrap_or_default())
                }
                "InterfacesRemoved" => (None, body.deserialize::<(OwnedObjectPath, Vec<String>)>().map(|(_, i)| i).unwrap_or_default()),
                // logind's bool argument, kept as "true"/"false": `Signal::is_wake` reads it.
                "PrepareForSleep" => (body.deserialize::<bool>().ok().map(|b| b.to_string()), Vec::new()),
                _ => (None, Vec::new()),
            };
            return Ok(Signal {
                path: h.path().map(|p| p.as_str().to_string()).unwrap_or_default(),
                interface: h.interface().map(|i| i.as_str().to_string()).unwrap_or_default(),
                member,
                arg0,
                names,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::ObjectPath;

    #[test]
    fn values_converted_to_props() {
        assert_eq!(prop(&Value::from(true)), Prop::Bool(true));
        assert_eq!(prop(&Value::from(42u32)), Prop::Int(42));
        assert_eq!(prop(&Value::from(72u8)), Prop::Int(72));
        assert_eq!(prop(&Value::from(1_791_131_213u64)), Prop::Int(1_791_131_213));
        assert_eq!(prop(&Value::from(u64::MAX)), Prop::Other, "beyond i64: never read");
        assert_eq!(prop(&Value::from(92.5f64)), Prop::Float(92.5));
        assert_eq!(prop(&Value::from("MON_WIFI_1")), Prop::Str("MON_WIFI_1".into()));
        assert_eq!(prop(&Value::from(ObjectPath::try_from("/a").unwrap())), Prop::Path("/a".into()));
        assert_eq!(
            prop(&Value::from(vec![ObjectPath::try_from("/a").unwrap(), ObjectPath::try_from("/b").unwrap()])),
            Prop::Paths(vec!["/a".into(), "/b".into()])
        );
        assert_eq!(prop(&Value::from(vec![77u8, 79])), Prop::Bytes(b"MO".to_vec()));
        assert_eq!(prop(&Value::Value(Box::new(Value::from(0.5f64)))), Prop::Float(0.5), "a variant inside a variant");
        assert_eq!(prop(&Value::from(vec!["a", "b"])), Prop::Other);
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
}
