//! What the tray reads from the session bus, decoded in place with libdbus's
//! iterators (`dbus::arg::Iter`), bounded while decoding: applications are
//! untrusted peers. Replies are read by the call they answer (its kind
//! decides the expected body); a body of the wrong type is `BadReply`, never
//! a panic; icon pixmaps, menu icons and shortcuts are never decoded — the
//! tooltip's pixmap array is stepped over with `next()`.

use super::bus::{Call, CallError, Property};
use super::{
    APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, ITEM_INTERFACE, MAX_ITEMS, MAX_MENU_DEPTH, MAX_MENU_ENTRIES, MAX_TEXT, MENU_INTERFACE,
    MPRIS_PATH, MPRIS_PLAYER_INTERFACE, NOTIFICATIONS_INTERFACE, NOTIFICATIONS_PATH, PORTAL_PATH, PROPERTIES_INTERFACE, Prop, PropSet,
    PropValue, SETTINGS_INTERFACE, SYSTEMD_MANAGER, SYSTEMD_PATH, Status, WATCHER_INTERFACE,
};
use crate::media::Value;
use crate::notify::{self, Action, Incoming, Urgency};
use crate::sources::dbus::BUS_DRIVER;
use crate::sources::dbus::dbusrs_bus::sig;
/// Moved to `text` (shared with the sound model); kept here for the codec and its callers.
pub use crate::text::{clean, invisible};
use dbus::Message;
use dbus::arg::{ArgType, Iter};
use dbus::message::MessageType;
use dbus::strings::Path;

/// Most names read from one `ListNames` reply or another watcher's list (a
/// session has a few hundred; the rest is never allocated).
const MAX_NAMES: usize = 64 * MAX_ITEMS;

/// Longest registration string another watcher's signal may carry, in bytes
/// (a bus name is at most 255 bytes, plus an object path): passed uncut for
/// `host::split_service` to validate, refused beyond.
pub const MAX_SERVICE: usize = 1024;

/// Most entries read from one `a{sv}` (a player's properties, its metadata): the rest is never decoded.
pub const MAX_ENTRIES: usize = 64;
/// Most strings read from one `as` (artists, invalidated names).
pub const MAX_LIST: usize = 32;
/// Most (key, label) pairs read from Notify's `actions`: 200 actions cost 32 reads.
pub const MAX_ACTION_PAIRS: usize = 32;

/// A decoded answer, by the call it answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Activate, SecondaryActivate, ContextMenu, Scroll, AddMatch, RemoveMatch, RegisterStatusNotifierHost, Event.
    Unit,
    Prop(PropValue),
    /// ListNames, or another watcher's RegisteredStatusNotifierItems.
    Names(Vec<String>),
    Owner(String),
    Pid(u32),
    /// RequestName's reply code (1 primary owner, 3 exists, …).
    RequestName(u32),
    /// AboutToShow's needUpdate.
    NeedUpdate(bool),
    Layout(Layout),
    /// ReadOne's color-scheme (0 no preference, 1 dark, 2 light; any other value is kept, `portal_mode` maps it).
    ColorScheme(u32),
    /// A player's properties: GetAll's, or `[("Metadata", Map)]` from PlayerMetadata.
    Props(Vec<(String, Value)>),
    /// PlayerIdentity's name, cleaned.
    Text(String),
    /// PlayerPosition's µs (as sent: the model clamps it).
    Position(i64),
    /// The user manager's `NFailedUnits`.
    Count(u32),
}

fn bad(what: impl Into<String>) -> CallError {
    CallError::BadReply(what.into())
}

/// The current argument, of type `T`, then the next one.
fn read<'a, T: dbus::arg::Get<'a> + dbus::arg::Arg>(it: &mut Iter<'a>, what: &str) -> Result<T, CallError> {
    it.read::<T>().map_err(|e| bad(format!("{what}: {e}")))
}

/// Decodes `m`, the method return answering `call` (its kind decides the expected body).
/// An error message answers `Err(error(m))`.
pub fn reply(call: &Call, m: &Message) -> Result<Reply, CallError> {
    if m.msg_type() == MessageType::Error {
        return Err(error(m));
    }
    let mut it = m.iter_init();
    match call {
        Call::Activate { .. }
        | Call::SecondaryActivate { .. }
        | Call::ContextMenu { .. }
        | Call::Scroll { .. }
        | Call::AddMatch { .. }
        | Call::RemoveMatch { .. }
        | Call::RegisterStatusNotifierHost { .. }
        | Call::Event { .. }
        | Call::PlayerControl { .. }
        | Call::SubscribeUnits { .. } => Ok(Reply::Unit),
        Call::Get { property: Property::Item(p), .. } => item_property(*p, &mut it).map(Reply::Prop),
        Call::Get { property: Property::RegisteredItems, .. } => {
            let mut v = it.recurse(ArgType::Variant).ok_or_else(|| bad("RegisteredStatusNotifierItems: not a variant"))?;
            names(&mut v).map(Reply::Names)
        }
        Call::Get { property: Property::NFailedUnits, .. } => match variant_u32(&mut it) {
            Some(n) => Ok(Reply::Count(n)),
            None => Err(bad("NFailedUnits: not a u32 in a variant")),
        },
        Call::ListNames => names(&mut it).map(Reply::Names),
        Call::GetNameOwner { .. } => read::<&str>(&mut it, "GetNameOwner").map(|s| Reply::Owner(s.to_string())),
        Call::GetConnectionUnixProcessID { .. } => read::<u32>(&mut it, "GetConnectionUnixProcessID").map(Reply::Pid),
        Call::RequestName | Call::RequestNotifications => read::<u32>(&mut it, "RequestName").map(Reply::RequestName),
        Call::AboutToShow { .. } => read::<bool>(&mut it, "AboutToShow").map(Reply::NeedUpdate),
        Call::GetLayout { .. } => layout(&mut it).map(Reply::Layout),
        Call::ReadOne => color_scheme(&mut it).map(Reply::ColorScheme).ok_or_else(|| bad(format!("color-scheme is {:?}", sig(&mut it)))),
        Call::PlayerGetAll { .. } => props(&mut it, true).map(Reply::Props).ok_or_else(|| bad(format!("GetAll is {:?}", sig(&mut it)))),
        Call::PlayerMetadata { .. } => match variant(&mut it) {
            Some(map @ Value::Map(_)) => Ok(Reply::Props(vec![("Metadata".into(), map)])),
            _ => Err(bad("Metadata: not an a{sv} in a variant")),
        },
        Call::PlayerIdentity { .. } => match variant(&mut it) {
            Some(Value::Str(name)) => Ok(Reply::Text(name)),
            _ => Err(bad("Identity: not a string in a variant")),
        },
        Call::PlayerPosition { .. } => match variant(&mut it) {
            Some(Value::Int(us)) => Ok(Reply::Position(us)),
            _ => Err(bad("Position: not an integer in a variant")),
        },
    }
}

/// The u32 inside the variant at `it`; None when it holds anything else.
fn variant_u32(it: &mut Iter<'_>) -> Option<u32> {
    it.recurse(ArgType::Variant)?.get::<u32>()
}

/// The value inside the variant at `it` (a `Get` reply); None when `it` holds no variant.
fn variant(it: &mut Iter<'_>) -> Option<Value> {
    let mut v = it.recurse(ArgType::Variant)?;
    Some(value(&mut v, true))
}

/// One property value at `it`, inside its variant (media spec § 3, Tolerance): `s` and `o` as
/// cleaned strings, every integer type as `Int` (a `t` beyond i64::MAX is `Other`), `d` as `Float`,
/// `b`, `as` (at most MAX_LIST, cleaned), and `a{sv}` as a `Map` only where `maps` allows it (the
/// `Metadata` property, one level down, nothing else); anything else `Other`, never an error.
fn value(it: &mut Iter<'_>, maps: bool) -> Value {
    let v = match sig(it).as_deref() {
        Some("s") => it.get::<&str>().map(|s| Value::Str(clean(s))),
        Some("o") => it.get::<Path>().map(|p| Value::Str(clean(&p))),
        Some("x") => it.get::<i64>().map(Value::Int),
        Some("i") => it.get::<i32>().map(|n| Value::Int(n.into())),
        Some("u") => it.get::<u32>().map(|n| Value::Int(n.into())),
        Some("t") => it.get::<u64>().and_then(|n| i64::try_from(n).ok()).map(Value::Int),
        Some("n") => it.get::<i16>().map(|n| Value::Int(n.into())),
        Some("q") => it.get::<u16>().map(|n| Value::Int(n.into())),
        Some("y") => it.get::<u8>().map(|n| Value::Int(n.into())),
        Some("d") => it.get::<f64>().map(Value::Float),
        Some("b") => it.get::<bool>().map(Value::Bool),
        Some("as") => strings(it).map(Value::StrList),
        Some("a{sv}") if maps => props(it, false).map(Value::Map),
        _ => None,
    };
    v.unwrap_or(Value::Other)
}

/// `as` at `it`: at most MAX_LIST strings, each cleaned.
fn strings(it: &mut Iter<'_>) -> Option<Vec<String>> {
    let mut a = it.recurse(ArgType::Array)?;
    let mut out = Vec::new();
    while out.len() < MAX_LIST
        && let Some(s) = a.get::<&str>()
    {
        out.push(clean(s));
        a.next();
    }
    Some(out)
}

/// `a{sv}` at `it`: at most MAX_ENTRIES entries read (a skipped one counts too), a key longer than
/// MAX_TEXT bytes skipped, each value decoded by `value` (`maps`: whether the `Metadata` value may
/// itself be an `a{sv}`; any other key's never is).
fn props(it: &mut Iter<'_>, maps: bool) -> Option<Vec<(String, Value)>> {
    if sig(it).as_deref() != Some("a{sv}") {
        return None;
    }
    let mut entries = it.recurse(ArgType::Array)?;
    let mut out = Vec::new();
    for _ in 0..MAX_ENTRIES {
        let Some(mut entry) = entries.recurse(ArgType::DictEntry) else { break };
        let key = entry.read::<&str>().ok()?;
        let mut v = entry.recurse(ArgType::Variant)?;
        if key.len() <= MAX_TEXT {
            out.push((key.to_string(), value(&mut v, maps && key == "Metadata")));
        }
        entries.next();
    }
    Some(out)
}

/// PropertiesChanged's `sa{sv}as` for the Player interface: its changed properties and the names it
/// invalidated (absent: none). Another interface → None.
#[allow(clippy::type_complexity)]
fn player_props(it: &mut Iter<'_>) -> Option<(Vec<(String, Value)>, Vec<String>)> {
    if it.get::<&str>()? != MPRIS_PLAYER_INTERFACE {
        return None;
    }
    it.next();
    let changed = props(it, true)?;
    it.next();
    let invalidated = if sig(it).as_deref() == Some("as") { strings(it)? } else { Vec::new() };
    Some((changed, invalidated))
}

/// A D-Bus error message → CallError::Remote(its name).
pub fn error(m: &Message) -> CallError {
    let mut copy = m.duplicate().ok();
    let name = copy.as_mut().and_then(|c| c.as_result().err()).and_then(|e| e.name().map(String::from));
    CallError::Remote(name.unwrap_or_default())
}

/// `as` at `it`, at most MAX_NAMES, never more allocated.
fn names(it: &mut Iter<'_>) -> Result<Vec<String>, CallError> {
    if sig(it).as_deref() != Some("as") {
        return Err(bad(format!("names: {:?}", sig(it))));
    }
    let mut out = Vec::new();
    let mut a = it.recurse(ArgType::Array).ok_or_else(|| bad("names: not an array"))?;
    while out.len() < MAX_NAMES
        && let Some(s) = a.get::<&str>()
    {
        if s.len() <= MAX_SERVICE {
            out.push(s.to_string());
        }
        a.next();
    }
    Ok(out)
}

/// One item property, inside its `Get` variant.
fn item_property(p: Prop, it: &mut Iter<'_>) -> Result<PropValue, CallError> {
    let mut v = it.recurse(ArgType::Variant).ok_or_else(|| bad(format!("{}: not a variant", p.name())))?;
    let found = sig(&mut v);
    let wrong = || bad(format!("{} is {found:?}", p.name()));
    match p {
        Prop::Id | Prop::Title | Prop::IconName | Prop::AttentionIconName => {
            v.get::<&str>().map(|s| PropValue::Text(clean(s))).ok_or_else(wrong)
        }
        Prop::Status => v.get::<&str>().map(|s| PropValue::Status(Status::parse(s))).ok_or_else(wrong),
        Prop::Menu => v.get::<Path>().filter(|o| o.len() <= MAX_SERVICE).map(|o| PropValue::Path(o.to_string())).ok_or_else(wrong),
        Prop::ItemIsMenu => v.get::<bool>().map(PropValue::Bool).ok_or_else(wrong),
        Prop::ToolTip => tooltip_title(&mut v).map(|t| PropValue::Text(clean(t))).ok_or_else(wrong),
    }
}

/// `(sa(iiay)ss)`: the third field, the title. The icon name is not read and
/// the pixmap array is stepped over whatever it holds — never decoded.
fn tooltip_title<'a>(v: &mut Iter<'a>) -> Option<&'a str> {
    let mut fields = v.recurse(ArgType::Struct)?;
    fields.get::<&str>()?;
    fields.next();
    fields.next();
    fields.get::<&str>()
}

/// A received signal, reduced to what the tray reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// Only from the bus driver (sender `org.freedesktop.DBus`): any peer may send a signal by that name.
    NameOwnerChanged {
        name: String,
        old: String,
        new: String,
    },
    /// NewTitle, NewIcon, NewAttentionIcon, NewToolTip (and NewStatus without an argument): the props to re-read.
    ItemChanged {
        sender: String,
        path: String,
        props: PropSet,
    },
    /// NewStatus(s): carries the value, nothing to re-read.
    ItemStatus {
        sender: String,
        path: String,
        status: Status,
    },
    /// Another watcher's signals (host-only mode): the service string as it sent it, uncut
    /// (`host::split_service` validates it); longer than MAX_SERVICE bytes → Other.
    ItemRegistered(String),
    ItemUnregistered(String),
    LayoutUpdated {
        sender: String,
        path: String,
        revision: u32,
        parent: i32,
    },
    ItemsPropertiesUpdated {
        sender: String,
        path: String,
        updated: Vec<(i32, MenuProps)>,
        removed: Vec<(i32, Vec<String>)>,
    },
    /// The portal's SettingChanged for (APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY) on PORTAL_PATH; any other key → Other.
    ColorScheme(u32),
    /// A player's PropertiesChanged on MPRIS_PATH for the Player interface; the sender (its unique
    /// name) maps it to the player.
    PlayerProps {
        sender: String,
        changed: Vec<(String, Value)>,
        invalidated: Vec<String>,
    },
    /// A player's Seeked on MPRIS_PATH: its new position, in µs.
    Seeked {
        sender: String,
        us: i64,
    },
    /// The user manager's `PropertiesChanged` for its Manager interface on its object: the count is to be
    /// read again (the dict is never decoded: it may carry the Environment).
    UnitsChanged {
        sender: String,
    },
    /// The bus driver's NameAcquired, sent to us alone: that name is ours now (the consumer keeps
    /// the one it asked for: the watcher's comes on this connection too).
    NameAcquired(String),
    /// The bus driver's NameLost, sent to us alone: that name is no longer ours.
    NameLost(String),
    /// Anything else, or a body that does not parse.
    Other,
}

/// `m` decoded; `NameOwnerChanged` is believed from the bus driver only.
// the unicast guards stay explicit before the broadcast rows, though they answer as the fallback does.
#[allow(clippy::match_same_arms)]
pub fn signal(m: &Message) -> Signal {
    let text = |s: Option<String>| s.unwrap_or_default();
    let sender = text(m.sender().map(|s| s.to_string()));
    let path = text(m.path().map(|p| p.to_string()));
    let interface = text(m.interface().map(|i| i.to_string()));
    let member = text(m.member().map(|x| x.to_string()));
    let mut it = m.iter_init();
    let changed = |p: Prop| {
        let mut props = PropSet::default();
        props.insert(p);
        Signal::ItemChanged { sender: sender.clone(), path: path.clone(), props }
    };
    match (interface.as_str(), member.as_str()) {
        (BUS_DRIVER, "NameOwnerChanged") if sender != BUS_DRIVER => Signal::Other,
        (BUS_DRIVER, "NameOwnerChanged") => match m.read3::<&str, &str, &str>() {
            Ok((name, old, new)) => Signal::NameOwnerChanged { name: name.into(), old: old.into(), new: new.into() },
            Err(_) => Signal::Other,
        },
        // Unicast from the bus driver only: a peer's signal by these names, or a broadcast, is not it.
        (BUS_DRIVER, "NameAcquired" | "NameLost") if sender != BUS_DRIVER || m.destination().is_none() => Signal::Other,
        (BUS_DRIVER, "NameAcquired") => it.get::<&str>().map_or(Signal::Other, |n| Signal::NameAcquired(n.into())),
        (BUS_DRIVER, "NameLost") => it.get::<&str>().map_or(Signal::Other, |n| Signal::NameLost(n.into())),
        (ITEM_INTERFACE, "NewTitle") => changed(Prop::Title),
        (ITEM_INTERFACE, "NewIcon") => changed(Prop::IconName),
        (ITEM_INTERFACE, "NewAttentionIcon") => changed(Prop::AttentionIconName),
        (ITEM_INTERFACE, "NewToolTip") => changed(Prop::ToolTip),
        (ITEM_INTERFACE, "NewStatus") => match it.get::<&str>() {
            Some(s) => Signal::ItemStatus { sender, path, status: Status::parse(s) },
            None => changed(Prop::Status),
        },
        (WATCHER_INTERFACE, "StatusNotifierItemRegistered") => {
            service(&mut it).map(|s| Signal::ItemRegistered(s.to_string())).unwrap_or(Signal::Other)
        }
        (WATCHER_INTERFACE, "StatusNotifierItemUnregistered") => {
            service(&mut it).map(|s| Signal::ItemUnregistered(s.to_string())).unwrap_or(Signal::Other)
        }
        (MENU_INTERFACE, "LayoutUpdated") => match m.read2::<u32, i32>() {
            Ok((revision, parent)) => Signal::LayoutUpdated { sender, path, revision, parent },
            Err(_) => Signal::Other,
        },
        // A unicast signal (destination set) reaches us whatever the match rules say: any peer could
        // send one. Only a broadcast, through the sender-filtered rule, is the portal's.
        (SETTINGS_INTERFACE, "SettingChanged") if m.destination().is_some() => Signal::Other,
        (SETTINGS_INTERFACE, "SettingChanged") if path == PORTAL_PATH => {
            let namespace = it.get::<&str>();
            it.next();
            let key = it.get::<&str>();
            it.next();
            match (namespace, key) {
                (Some(APPEARANCE_NAMESPACE), Some(COLOR_SCHEME_KEY)) => {
                    color_scheme(&mut it).map(Signal::ColorScheme).unwrap_or(Signal::Other)
                }
                _ => Signal::Other,
            }
        }
        // Like the portal's: a unicast signal reaches us whatever the rules say; only broadcasts count.
        (PROPERTIES_INTERFACE, "PropertiesChanged") | (MPRIS_PLAYER_INTERFACE, "Seeked") if m.destination().is_some() => Signal::Other,
        (PROPERTIES_INTERFACE, "PropertiesChanged") if path == MPRIS_PATH => match player_props(&mut it) {
            Some((changed, invalidated)) => Signal::PlayerProps { sender, changed, invalidated },
            None => Signal::Other,
        },
        (PROPERTIES_INTERFACE, "PropertiesChanged") if path == SYSTEMD_PATH => match it.get::<&str>() {
            Some(SYSTEMD_MANAGER) => Signal::UnitsChanged { sender },
            _ => Signal::Other,
        },
        (MPRIS_PLAYER_INTERFACE, "Seeked") if path == MPRIS_PATH => match it.get::<i64>() {
            Some(us) => Signal::Seeked { sender, us },
            None => Signal::Other,
        },
        (MENU_INTERFACE, "ItemsPropertiesUpdated") => match properties_updated(&mut it) {
            Some((updated, removed)) => Signal::ItemsPropertiesUpdated { sender, path, updated, removed },
            None => Signal::Other,
        },
        _ => Signal::Other,
    }
}

/// `v(u)`, or `v(v(u))` as some portal backends wrap it: the u32; anything else None.
fn color_scheme(it: &mut Iter<'_>) -> Option<u32> {
    let mut v = it.recurse(ArgType::Variant)?;
    if v.arg_type() == ArgType::Variant {
        v = v.recurse(ArgType::Variant)?;
    }
    v.get::<u32>()
}

/// A registration string, as sent (never cleaned: `host::split_service`
/// validates it as a bus name and an object path), at most MAX_SERVICE bytes.
fn service<'a>(it: &mut Iter<'a>) -> Option<&'a str> {
    it.get::<&str>().filter(|s| s.len() <= MAX_SERVICE)
}

/// `a(ia{sv})a(ias)`, each list at most MAX_MENU_ENTRIES long.
#[allow(clippy::type_complexity)]
fn properties_updated(it: &mut Iter<'_>) -> Option<(Vec<(i32, MenuProps)>, Vec<(i32, Vec<String>)>)> {
    if sig(it).as_deref() != Some("a(ia{sv})") {
        return None;
    }
    let mut updated = Vec::new();
    let mut a = it.recurse(ArgType::Array)?;
    while updated.len() < MAX_MENU_ENTRIES
        && let Some(mut entry) = a.recurse(ArgType::Struct)
    {
        let id = entry.read::<i32>().ok()?;
        updated.push((id, menu_props(&mut entry)?));
        a.next();
    }
    it.next();
    if sig(it).as_deref() != Some("a(ias)") {
        return None;
    }
    let mut removed = Vec::new();
    let mut a = it.recurse(ArgType::Array)?;
    while removed.len() < MAX_MENU_ENTRIES
        && let Some(mut entry) = a.recurse(ArgType::Struct)
    {
        let id = entry.read::<i32>().ok()?;
        let mut names = Vec::new();
        let mut n = entry.recurse(ArgType::Array)?;
        while names.len() < MENU_PROPERTY_NAMES.len()
            && let Some(s) = n.get::<&str>()
        {
            if MENU_PROPERTY_NAMES.contains(&s) {
                names.push(s.to_string());
            }
            n.next();
        }
        removed.push((id, names));
        a.next();
    }
    Some((updated, removed))
}

/// A method call to the notification server, decoded raw (notifications spec § 1, amended): never
/// through crossroads' typed handlers, which would decode `image-data` into dynamic values.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Notify(Incoming),
    Close(u32),
    Capabilities,
    ServerInfo,
    /// Not a call to the server, another member, or arguments of other types: refused.
    Unknown,
}

/// `m` decoded, when it is a method call on NOTIFICATIONS_PATH for NOTIFICATIONS_INTERFACE:
/// `Notify` exactly `susssasa{sv}` and an optional `i`, `CloseNotification(u)`, `GetCapabilities()`,
/// `GetServerInformation()`; anything else `Unknown`. A call with no interface header (D-Bus allows
/// one) is refused too, on purpose: libnotify, GDBus and Chromium always send the interface, and a
/// bare member could otherwise reach the server from any interface's namesake.
pub fn inbound(m: &Message) -> Inbound {
    let ours = m.msg_type() == MessageType::MethodCall
        && m.path().is_some_and(|p| &*p == NOTIFICATIONS_PATH)
        && m.interface().is_some_and(|i| &*i == NOTIFICATIONS_INTERFACE);
    if !ours {
        return Inbound::Unknown;
    }
    let mut it = m.iter_init();
    let empty = it.arg_type() == ArgType::Invalid;
    match m.member().as_deref() {
        Some("Notify") => notify_call(&mut it).map_or(Inbound::Unknown, Inbound::Notify),
        Some("CloseNotification") => match it.read::<u32>() {
            Ok(id) if it.arg_type() == ArgType::Invalid => Inbound::Close(id),
            _ => Inbound::Unknown,
        },
        Some("GetCapabilities") if empty => Inbound::Capabilities,
        Some("GetServerInformation") if empty => Inbound::ServerInfo,
        _ => Inbound::Unknown,
    }
}

/// Notify's arguments at `it`, one by one, each cleaned and capped by `notify`; the icon is checked
/// for its type and dropped; a missing `expire_timeout` is -1. None for a missing argument, one of
/// another type, or one too many.
fn notify_call(it: &mut Iter<'_>) -> Option<Incoming> {
    let app = it.read::<&str>().ok()?;
    let replaces_id = it.read::<u32>().ok()?;
    it.read::<&str>().ok()?;
    let summary = it.read::<&str>().ok()?;
    let body = it.read::<&str>().ok()?;
    if sig(it).as_deref() != Some("as") {
        return None;
    }
    let actions = actions(it)?;
    it.next();
    if sig(it).as_deref() != Some("a{sv}") {
        return None;
    }
    let h = hints(it)?;
    it.next();
    let expire_ms = match sig(it).as_deref() {
        None => -1,
        Some("i") => it.read::<i32>().ok()?,
        Some(_) => return None,
    };
    if sig(it).is_some() {
        return None;
    }
    Some(Incoming {
        app_name: notify::app_text(app),
        replaces_id,
        summary: notify::summary_text(summary),
        body: notify::body_text(body),
        actions,
        urgency: h.urgency,
        expire_ms,
        desktop_entry: h.desktop_entry,
        category: h.category,
        transient: h.transient,
        resident: h.resident,
        tag: h.tag,
    })
}

/// Notify's `as` at `it`, read as (key, label) pairs: at most MAX_ACTION_PAIRS pairs read, each
/// through `notify::action` (a refused key or one already kept: skipped), at most MAX_ACTIONS kept;
/// a last key without its label dropped.
fn actions(it: &mut Iter<'_>) -> Option<Vec<Action>> {
    let mut a = it.recurse(ArgType::Array)?;
    let mut out: Vec<Action> = Vec::new();
    for _ in 0..MAX_ACTION_PAIRS {
        let Some(key) = a.get::<&str>() else { break };
        if !a.next() {
            break;
        }
        let Some(label) = a.get::<&str>() else { break };
        if let Some(action) = notify::action(key, label)
            && !out.iter().any(|x| x.key == action.key)
        {
            out.push(action);
        }
        if out.len() == notify::MAX_ACTIONS || !a.next() {
            break;
        }
    }
    Some(out)
}

/// The hints alpheus reads (spec § 2).
struct Hints {
    urgency: Urgency,
    category: Option<String>,
    desktop_entry: Option<String>,
    transient: bool,
    resident: bool,
    tag: Option<String>,
}

/// Notify's `a{sv}` at `it`: at most MAX_ENTRIES entries; only the six hints read here are entered.
/// Every other one (`image-data`, `image_data`, `icon_data`, `image-path`, sound, `sender-pid`,
/// `x`, `y`…) is stepped over, its variant never recursed into: an image is never read.
fn hints(it: &mut Iter<'_>) -> Option<Hints> {
    let mut h = Hints { urgency: Urgency::Normal, category: None, desktop_entry: None, transient: false, resident: false, tag: None };
    let mut entries = it.recurse(ArgType::Array)?;
    for _ in 0..MAX_ENTRIES {
        let Some(mut entry) = entries.recurse(ArgType::DictEntry) else { break };
        let key = entry.read::<&str>().ok()?;
        let read = matches!(key, "urgency" | "transient" | "resident" | "category" | "desktop-entry" | "x-canonical-private-synchronous");
        if read && let Some(mut v) = entry.recurse(ArgType::Variant) {
            match key {
                "urgency" => h.urgency = integer(&mut v).map_or(h.urgency, Urgency::from_level),
                "transient" => h.transient = flag(&mut v).unwrap_or(h.transient),
                "resident" => h.resident = flag(&mut v).unwrap_or(h.resident),
                "category" => h.category = v.get::<&str>().and_then(notify::hint_text),
                "desktop-entry" => h.desktop_entry = v.get::<&str>().and_then(notify::hint_text),
                "x-canonical-private-synchronous" => h.tag = v.get::<&str>().and_then(notify::hint_text),
                _ => {}
            }
        }
        entries.next();
    }
    Some(h)
}

/// An integer hint of the types clients use: `y`, `u`, `i`, `x`.
fn integer(v: &mut Iter<'_>) -> Option<i64> {
    match sig(v).as_deref() {
        Some("y") => v.get::<u8>().map(i64::from),
        Some("u") => v.get::<u32>().map(i64::from),
        Some("i") => v.get::<i32>().map(i64::from),
        Some("x") => v.get::<i64>(),
        _ => None,
    }
}

/// A flag hint: `b`, or an integer (≠ 0 is true).
fn flag(v: &mut Iter<'_>) -> Option<bool> {
    match sig(v).as_deref() {
        Some("b") => v.get::<bool>(),
        _ => integer(v).map(|n| n != 0),
    }
}

/// A dbusmenu label: one leading-mnemonic `_` removed per occurrence, `__` → `_`, then `clean`.
pub fn label(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '_' {
            out.push(c);
        } else if chars.peek() == Some(&'_') {
            chars.next();
            out.push('_');
        }
        if out.len() > 4 * MAX_TEXT {
            break;
        }
    }
    clean(&out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Checkmark,
    Radio,
}

/// The dbusmenu property names the tray reads (and resets).
const MENU_PROPERTY_NAMES: [&str; 7] = ["label", "enabled", "visible", "type", "toggle-type", "toggle-state", "children-display"];

/// A dbusmenu item's properties; None = absent (the spec default applies).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuProps {
    pub label: Option<String>,
    pub enabled: Option<bool>,
    pub visible: Option<bool>,
    /// type == "separator".
    pub separator: Option<bool>,
    pub toggle: Option<Toggle>,
    /// 1 on, 0 off, anything else indeterminate.
    pub toggle_state: Option<i32>,
    /// children-display == "submenu".
    pub submenu: Option<bool>,
}

impl MenuProps {
    /// Sets every field `update` carries.
    pub fn merge(&mut self, update: &MenuProps) {
        if update.label.is_some() {
            self.label.clone_from(&update.label);
        }
        self.enabled = update.enabled.or(self.enabled);
        self.visible = update.visible.or(self.visible);
        self.separator = update.separator.or(self.separator);
        self.toggle = update.toggle.or(self.toggle);
        self.toggle_state = update.toggle_state.or(self.toggle_state);
        self.submenu = update.submenu.or(self.submenu);
    }

    /// Back to absent for each dbusmenu property name in `names`.
    pub fn reset(&mut self, names: &[String]) {
        for n in names {
            match n.as_str() {
                "label" => self.label = None,
                "enabled" => self.enabled = None,
                "visible" => self.visible = None,
                "type" => self.separator = None,
                "toggle-type" => self.toggle = None,
                "toggle-state" => self.toggle_state = None,
                "children-display" => self.submenu = None,
                _ => {}
            }
        }
    }
}

/// `a{sv}` at `it`: the properties the tray reads; any other key (icon-data,
/// icon-name, shortcut…) and any value of an unexpected type is skipped.
fn menu_props(it: &mut Iter<'_>) -> Option<MenuProps> {
    if sig(it).as_deref() != Some("a{sv}") {
        return None;
    }
    let mut p = MenuProps::default();
    let mut entries = it.recurse(ArgType::Array)?;
    while let Some(mut entry) = entries.recurse(ArgType::DictEntry) {
        let key = entry.read::<&str>().ok()?;
        let mut v = entry.recurse(ArgType::Variant)?;
        match key {
            "label" => p.label = v.get::<&str>().map(label),
            "enabled" => p.enabled = v.get::<bool>(),
            "visible" => p.visible = v.get::<bool>(),
            "type" => p.separator = v.get::<&str>().map(|t| t == "separator"),
            "toggle-type" => {
                p.toggle = v.get::<&str>().and_then(|t| match t {
                    "checkmark" => Some(Toggle::Checkmark),
                    "radio" => Some(Toggle::Radio),
                    _ => None,
                });
            }
            "toggle-state" => p.toggle_state = v.get::<i32>(),
            "children-display" => p.submenu = v.get::<&str>().map(|d| d == "submenu"),
            _ => {}
        }
        entries.next();
    }
    Some(p)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuNode {
    pub id: i32,
    pub props: MenuProps,
    pub children: Vec<MenuNode>,
}

impl MenuNode {
    pub fn label(&self) -> &str {
        self.props.label.as_deref().unwrap_or("")
    }

    pub fn is_visible(&self) -> bool {
        self.props.visible.unwrap_or(true)
    }

    pub fn is_enabled(&self) -> bool {
        self.props.enabled.unwrap_or(true)
    }

    pub fn is_separator(&self) -> bool {
        self.props.separator.unwrap_or(false)
    }

    /// children-display = "submenu", or children already known.
    pub fn has_submenu(&self) -> bool {
        self.props.submenu == Some(true) || !self.children.is_empty()
    }

    fn find(&self, id: i32) -> Option<&MenuNode> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    fn find_mut(&mut self, id: i32) -> Option<&mut MenuNode> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(id))
    }

    fn count(&self) -> usize {
        self.children.iter().map(|c| 1 + c.count()).sum()
    }

    /// After a re-read: a descendant back without children but announced as
    /// a submenu (a depth-1 `GetLayout` never sends grandchildren) keeps the
    /// children fetched before under the same id; `old` are the children
    /// this node had before.
    fn carry_over(&mut self, mut old: Vec<MenuNode>) {
        for child in &mut self.children {
            let Some(i) = old.iter().position(|o| o.id == child.id) else { continue };
            let before = old.swap_remove(i);
            if child.children.is_empty() && child.props.submenu == Some(true) {
                child.children = before.children;
            } else {
                child.carry_over(before.children);
            }
        }
    }

    fn levels(&self) -> usize {
        self.children.iter().map(|c| 1 + c.levels()).max().unwrap_or(0)
    }

    /// Keeps at most `budget` descendants and nothing deeper than
    /// MAX_MENU_DEPTH (this node at `depth`), in document order.
    fn bound(&mut self, depth: usize, budget: &mut usize) {
        if depth >= MAX_MENU_DEPTH {
            self.children.clear();
            return;
        }
        let mut kept = 0;
        for child in &mut self.children {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            kept += 1;
            child.bound(depth + 1, budget);
        }
        self.children.truncate(kept);
    }
}

/// One menu's tree, bounded: at most MAX_MENU_ENTRIES nodes (root excluded), depth ≤ MAX_MENU_DEPTH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub revision: u32,
    pub root: MenuNode,
}

impl Layout {
    pub fn node(&self, id: i32) -> Option<&MenuNode> {
        self.root.find(id)
    }

    /// Replaces the children (and props) of the node `node.id` with `node`'s — a submenu fetched or a
    /// LayoutUpdated re-read — keeping the bounds; false when `node.id` is unknown. A descendant that
    /// comes back announced as a submenu but without children (a depth-1 re-read) keeps the children
    /// fetched before under the same id.
    pub fn graft(&mut self, node: MenuNode) -> bool {
        let Some(target) = self.root.find_mut(node.id) else { return false };
        let before = std::mem::replace(target, node);
        target.carry_over(before.children);
        let mut budget = MAX_MENU_ENTRIES;
        self.root.bound(0, &mut budget);
        true
    }

    /// ItemsPropertiesUpdated, in place.
    pub fn patch(&mut self, updated: &[(i32, MenuProps)], removed: &[(i32, Vec<String>)]) {
        for (id, props) in updated {
            if let Some(n) = self.root.find_mut(*id) {
                n.props.merge(props);
            }
        }
        for (id, names) in removed {
            if let Some(n) = self.root.find_mut(*id) {
                n.props.reset(names);
            }
        }
    }

    /// Nodes, root excluded.
    pub fn len(&self) -> usize {
        self.root.count()
    }

    pub fn is_empty(&self) -> bool {
        self.root.children.is_empty()
    }

    pub fn depth(&self) -> usize {
        self.root.levels()
    }
}

/// `GetLayout`'s reply: `u(ia{sv}av)`.
fn layout(it: &mut Iter<'_>) -> Result<Layout, CallError> {
    let revision = read::<u32>(it, "GetLayout revision")?;
    if sig(it).as_deref() != Some("(ia{sv}av)") {
        return Err(bad(format!("GetLayout: {:?}", sig(it))));
    }
    let mut budget = MAX_MENU_ENTRIES;
    let root = node(it, 0, &mut budget).ok_or_else(|| bad("GetLayout: the root does not parse"))?;
    Ok(Layout { revision, root })
}

/// One `(ia{sv}av)` at `it`, `depth` levels below the root, taking at most
/// `budget` descendants: a 10,000-node reply allocates MAX_MENU_ENTRIES.
fn node(it: &mut Iter<'_>, depth: usize, budget: &mut usize) -> Option<MenuNode> {
    let mut fields = it.recurse(ArgType::Struct)?;
    let id = fields.read::<i32>().ok()?;
    let props = menu_props(&mut fields)?;
    fields.next();
    let mut children = Vec::new();
    if depth < MAX_MENU_DEPTH
        && sig(&mut fields).as_deref() == Some("av")
        && let Some(mut a) = fields.recurse(ArgType::Array)
    {
        while *budget > 0
            && let Some(mut v) = a.recurse(ArgType::Variant)
        {
            if sig(&mut v).as_deref() == Some("(ia{sv}av)") {
                *budget -= 1;
                if let Some(child) = node(&mut v, depth + 1, budget) {
                    children.push(child);
                }
            }
            a.next();
        }
    }
    Some(MenuNode { id, props, children })
}

#[cfg(test)]
mod tests;
