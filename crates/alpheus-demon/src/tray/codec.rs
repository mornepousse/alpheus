//! What the tray reads from the session bus, decoded in place with libdbus's
//! iterators (`dbus::arg::Iter`), bounded while decoding: applications are
//! untrusted peers. Replies are read by the call they answer (its kind
//! decides the expected body); a body of the wrong type is `BadReply`, never
//! a panic; icon pixmaps, menu icons and shortcuts are never decoded — the
//! tooltip's pixmap array is stepped over with `next()`.

use super::bus::{Call, CallError, Property};
use super::{
    APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, ITEM_INTERFACE, MAX_ITEMS, MAX_MENU_DEPTH, MAX_MENU_ENTRIES, MAX_TEXT, MENU_INTERFACE,
    PORTAL_PATH, Prop, PropSet, PropValue, SETTINGS_INTERFACE, Status, WATCHER_INTERFACE,
};
use crate::sources::dbus::dbusrs_bus::sig;
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
        | Call::Event { .. } => Ok(Reply::Unit),
        Call::Get { property: Property::Item(p), .. } => item_property(*p, &mut it).map(Reply::Prop),
        Call::Get { property: Property::RegisteredItems, .. } => {
            let mut v = it.recurse(ArgType::Variant).ok_or_else(|| bad("RegisteredStatusNotifierItems: not a variant"))?;
            names(&mut v).map(Reply::Names)
        }
        Call::ListNames => names(&mut it).map(Reply::Names),
        Call::GetNameOwner { .. } => read::<&str>(&mut it, "GetNameOwner").map(|s| Reply::Owner(s.to_string())),
        Call::GetConnectionUnixProcessID { .. } => read::<u32>(&mut it, "GetConnectionUnixProcessID").map(Reply::Pid),
        Call::RequestName => read::<u32>(&mut it, "RequestName").map(Reply::RequestName),
        Call::AboutToShow { .. } => read::<bool>(&mut it, "AboutToShow").map(Reply::NeedUpdate),
        Call::GetLayout { .. } => layout(&mut it).map(Reply::Layout),
        Call::ReadOne => color_scheme(&mut it).map(Reply::ColorScheme).ok_or_else(|| bad(format!("color-scheme is {:?}", sig(&mut it)))),
    }
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
    /// Anything else, or a body that does not parse.
    Other,
}

/// `m` decoded; `NameOwnerChanged` is believed from the bus driver only.
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
        ("org.freedesktop.DBus", "NameOwnerChanged") if sender != "org.freedesktop.DBus" => Signal::Other,
        ("org.freedesktop.DBus", "NameOwnerChanged") => match m.read3::<&str, &str, &str>() {
            Ok((name, old, new)) => Signal::NameOwnerChanged { name: name.into(), old: old.into(), new: new.into() },
            Err(_) => Signal::Other,
        },
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

/// Text from an application, drawable on one line: `\n`, `\r`, `\t` → a space, every other control
/// character and the invisible formatting characters (U+200B–U+200F, U+202A–U+202E, U+2066–U+2069)
/// removed, at most MAX_TEXT characters.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() || invisible(c) => None,
            c => Some(c),
        })
        .take(MAX_TEXT)
        .collect()
}

/// Zero-width and bidirectional formatting characters: drawn as nothing,
/// they could hide or reorder what the bar shows (a title posing as
/// another). U+200C (ZWNJ) and U+200D (ZWJ) are excluded: they are ordinary
/// text shaping (Arabic/Indic ligature control, emoji ZWJ sequences), not
/// spoofing tools, so removing them would corrupt otherwise legible text.
fn invisible(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
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
                })
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
mod tests {
    use super::*;
    use crate::tray::DEFAULT_ITEM_PATH;
    use dbus::arg::{IterAppend, Variant};
    use dbus::strings::Signature;

    fn signature(s: &str) -> Signature<'static> {
        Signature::new(s).unwrap()
    }

    /// A message whose body `f` writes (its type does not matter to `reply`).
    fn body(f: impl FnOnce(&mut IterAppend<'_>)) -> Message {
        let mut m = Message::new_method_call(":1.7", DEFAULT_ITEM_PATH, "org.example", "Reply").unwrap();
        f(&mut IterAppend::new(&mut m));
        m
    }

    fn get(p: Prop) -> Call {
        Call::Get { destination: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), property: Property::Item(p) }
    }

    fn layout_call() -> Call {
        Call::GetLayout { destination: ":1.7".into(), path: "/MenuBar".into(), parent: 0 }
    }

    /// The `(sa(iiay)ss)` tooltip, its pixmap array of type `pixmaps` holding `bytes` bytes.
    fn tooltip(title: &str, pixmaps: &str, bytes: usize) -> Message {
        let inner = format!("(s{pixmaps}ss)");
        body(|i| {
            i.append_variant(&signature(&inner), |v| {
                v.append_struct(|s| {
                    s.append("tooltip-icon");
                    let element = &pixmaps[1..];
                    s.append_array(&signature(element), |a| {
                        a.append_struct(|e| {
                            e.append(16i32);
                            e.append(16i32);
                            if element == "(iiay)" {
                                e.append(vec![7u8; bytes]);
                            } else {
                                e.append("x".repeat(bytes));
                            }
                        })
                    });
                    s.append(title);
                    s.append("description");
                })
            })
        })
    }

    #[derive(Clone)]
    enum V {
        S(&'static str),
        B(bool),
        I(i32),
        Bytes(usize),
    }

    struct T {
        id: i32,
        props: Vec<(&'static str, V)>,
        children: Vec<T>,
    }

    fn t(id: i32, props: &[(&'static str, V)], children: Vec<T>) -> T {
        T { id, props: props.to_vec(), children }
    }

    fn append_node(i: &mut IterAppend<'_>, n: &T) {
        i.append_struct(|s| {
            s.append(n.id);
            s.append_dict(&signature("s"), &signature("v"), |d| {
                for (k, v) in &n.props {
                    d.append_dict_entry(|e| {
                        e.append(*k);
                        match v {
                            V::S(x) => e.append(Variant(*x)),
                            V::B(x) => e.append(Variant(*x)),
                            V::I(x) => e.append(Variant(*x)),
                            V::Bytes(n) => e.append(Variant(vec![0u8; *n])),
                        }
                    });
                }
            });
            s.append_array(&signature("v"), |a| {
                for c in &n.children {
                    a.append_variant(&signature("(ia{sv}av)"), |v| append_node(v, c));
                }
            });
        });
    }

    fn layout_reply(revision: u32, root: &T) -> Message {
        body(|i| {
            i.append(revision);
            append_node(i, root);
        })
    }

    fn decoded(m: &Message) -> Layout {
        match reply(&layout_call(), m) {
            Ok(Reply::Layout(l)) => l,
            other => panic!("{other:?}"),
        }
    }

    /// A chain `depth` levels deep under the root, one node per level.
    fn chain(depth: usize) -> T {
        let mut n = t(depth as i32, &[("label", V::S("leaf"))], vec![]);
        for id in (1..depth as i32).rev() {
            n = t(id, &[], vec![n]);
        }
        t(0, &[], vec![n])
    }

    fn signal_from(sender: &str, path: &str, interface: &str, member: &str, f: impl FnOnce(&mut IterAppend<'_>)) -> Signal {
        let mut m = Message::new_signal(path, interface, member).unwrap();
        m.set_sender(Some(sender.into()));
        f(&mut IterAppend::new(&mut m));
        signal(&m)
    }

    #[test]
    fn read_one_decodes_the_color_scheme_plain_or_nested() {
        assert_eq!(reply(&Call::ReadOne, &body(|i| i.append(Variant(1u32)))), Ok(Reply::ColorScheme(1)));
        assert_eq!(
            reply(&Call::ReadOne, &body(|i| i.append(Variant(Variant(2u32))))),
            Ok(Reply::ColorScheme(2)),
            "v(v(u)): some portal backends wrap it twice"
        );
        assert!(matches!(reply(&Call::ReadOne, &body(|i| i.append(Variant("dark")))), Err(CallError::BadReply(_))));
        assert!(matches!(reply(&Call::ReadOne, &body(|i| i.append(1u32))), Err(CallError::BadReply(_))), "not in a variant");
    }

    #[test]
    fn setting_changed_is_read_for_color_scheme_only() {
        use crate::tray::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_PATH, SETTINGS_INTERFACE};
        let changed = |path: &str, ns: &str, key: &str, f: &dyn Fn(&mut IterAppend<'_>)| {
            signal_from(":1.4", path, SETTINGS_INTERFACE, "SettingChanged", |i| {
                i.append(ns);
                i.append(key);
                f(i);
            })
        };
        assert_eq!(changed(PORTAL_PATH, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant(2u32))), Signal::ColorScheme(2));
        assert_eq!(
            changed(PORTAL_PATH, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant(Variant(1u32)))),
            Signal::ColorScheme(1)
        );
        assert_eq!(changed(PORTAL_PATH, APPEARANCE_NAMESPACE, "contrast", &|i| i.append(Variant(1u32))), Signal::Other, "another key");
        assert_eq!(
            changed(PORTAL_PATH, "org.gnome.desktop.interface", COLOR_SCHEME_KEY, &|i| i.append(Variant("prefer-dark"))),
            Signal::Other,
            "another namespace"
        );
        assert_eq!(
            changed("/elsewhere", APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant(2u32))),
            Signal::Other,
            "not the portal's object"
        );
        assert_eq!(
            changed(PORTAL_PATH, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant("light"))),
            Signal::Other,
            "not a u32"
        );
    }

    #[test]
    fn get_replies_decode_every_property() {
        let text = |p: Prop, s: &'static str| reply(&get(p), &body(|i| i.append(Variant(s))));
        assert_eq!(text(Prop::Id, "nimbus"), Ok(Reply::Prop(PropValue::Text("nimbus".into()))));
        assert_eq!(text(Prop::Title, "Nimbus"), Ok(Reply::Prop(PropValue::Text("Nimbus".into()))));
        assert_eq!(text(Prop::IconName, "nimbus-tray"), Ok(Reply::Prop(PropValue::Text("nimbus-tray".into()))));
        assert_eq!(text(Prop::AttentionIconName, "nimbus-alert"), Ok(Reply::Prop(PropValue::Text("nimbus-alert".into()))));
        assert_eq!(text(Prop::Status, "NeedsAttention"), Ok(Reply::Prop(PropValue::Status(Status::NeedsAttention))));
        let menu = reply(&get(Prop::Menu), &body(|i| i.append(Variant(Path::new("/MenuBar").unwrap()))));
        assert_eq!(menu, Ok(Reply::Prop(PropValue::Path("/MenuBar".into()))));
        assert_eq!(reply(&get(Prop::ItemIsMenu), &body(|i| i.append(Variant(true)))), Ok(Reply::Prop(PropValue::Bool(true))));
        assert_eq!(
            reply(&get(Prop::ToolTip), &tooltip("Nimbus: 3 new", "a(iiay)", 64)),
            Ok(Reply::Prop(PropValue::Text("Nimbus: 3 new".into())))
        );
        let others = Call::Get { destination: ":1.9".into(), path: "/StatusNotifierWatcher".into(), property: Property::RegisteredItems };
        let list = body(|i| i.append(Variant(vec![":1.7/StatusNotifierItem", "org.kde.StatusNotifierItem-4-1/StatusNotifierItem"])));
        assert_eq!(
            reply(&others, &list),
            Ok(Reply::Names(vec![":1.7/StatusNotifierItem".into(), "org.kde.StatusNotifierItem-4-1/StatusNotifierItem".into()]))
        );
        assert_eq!(
            reply(&Call::ListNames, &body(|i| i.append(vec!["org.freedesktop.DBus", ":1.7"]))),
            Ok(Reply::Names(vec!["org.freedesktop.DBus".into(), ":1.7".into()]))
        );
        assert_eq!(reply(&Call::GetNameOwner { name: "x.y".into() }, &body(|i| i.append(":1.7"))), Ok(Reply::Owner(":1.7".into())));
        assert_eq!(reply(&Call::GetConnectionUnixProcessID { name: ":1.7".into() }, &body(|i| i.append(4242u32))), Ok(Reply::Pid(4242)));
        assert_eq!(reply(&Call::RequestName, &body(|i| i.append(3u32))), Ok(Reply::RequestName(3)));
        let about = Call::AboutToShow { destination: ":1.7".into(), path: "/MenuBar".into(), id: 0 };
        assert_eq!(reply(&about, &body(|i| i.append(true))), Ok(Reply::NeedUpdate(true)));
        let activate = Call::Activate { destination: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), x: 1, y: 2 };
        assert_eq!(reply(&activate, &body(|_| {})), Ok(Reply::Unit));
        let mut call = Message::new_method_call(":1.3", "/", "org.example", "X").unwrap();
        call.set_serial(4);
        let refused = call.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), c"no Activate");
        assert_eq!(reply(&activate, &refused), Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into())));
        assert_eq!(error(&refused), CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into()));
    }

    #[test]
    fn names_skip_a_string_longer_than_max_service() {
        let longest = "a".repeat(MAX_SERVICE);
        let too_long = "a".repeat(MAX_SERVICE + 1);
        let list = body(|i| i.append(vec![longest.as_str(), too_long.as_str(), "org.freedesktop.DBus"]));
        assert_eq!(
            reply(&Call::ListNames, &list),
            Ok(Reply::Names(vec![longest, "org.freedesktop.DBus".into()])),
            "exactly MAX_SERVICE bytes kept, one byte over skipped (not cut)"
        );
    }

    #[test]
    fn a_menu_path_longer_than_max_service_is_a_bad_reply() {
        let longest = format!("/{}", "a".repeat(MAX_SERVICE - 1));
        assert!(longest.len() == MAX_SERVICE);
        let ok = reply(&get(Prop::Menu), &body(|i| i.append(Variant(Path::new(longest.as_str()).unwrap()))));
        assert_eq!(ok, Ok(Reply::Prop(PropValue::Path(longest))));
        let too_long = format!("/{}", "a".repeat(MAX_SERVICE));
        let bad = reply(&get(Prop::Menu), &body(|i| i.append(Variant(Path::new(too_long.as_str()).unwrap()))));
        assert!(matches!(bad, Err(CallError::BadReply(_))), "{bad:?}");
    }

    #[test]
    fn tooltip_title_without_decoding_pixmaps() {
        // 300 KB of pixmaps whose elements are not even (iiay): stepped over, never read.
        let big = tooltip("Kettle", "a(iis)", 300_000);
        assert_eq!(reply(&get(Prop::ToolTip), &big), Ok(Reply::Prop(PropValue::Text("Kettle".into()))));
        let wrong = body(|i| i.append(Variant((7i32, "no title"))));
        assert!(matches!(reply(&get(Prop::ToolTip), &wrong), Err(CallError::BadReply(_))));
    }

    #[test]
    fn wrong_types_are_bad_replies_not_panics() {
        let bad_reply = |r: Result<Reply, CallError>| matches!(r, Err(CallError::BadReply(_)));
        assert!(bad_reply(reply(&get(Prop::Title), &body(|i| i.append(Variant(42i32))))));
        assert!(bad_reply(reply(&get(Prop::Status), &body(|i| i.append(Variant(true))))));
        assert!(bad_reply(reply(&get(Prop::Menu), &body(|i| i.append(Variant("/MenuBar"))))), "a string is not a path");
        assert!(bad_reply(reply(&get(Prop::ItemIsMenu), &body(|i| i.append(Variant("true"))))));
        assert!(bad_reply(reply(&get(Prop::Title), &body(|i| i.append("not in a variant")))));
        assert!(bad_reply(reply(&Call::ListNames, &body(|i| i.append(vec![1u32, 2])))));
        assert!(bad_reply(reply(&Call::GetNameOwner { name: "a.b".into() }, &body(|i| i.append(7u32)))));
        assert!(bad_reply(reply(&layout_call(), &body(|i| i.append((7u32, "no layout"))))));
    }

    #[test]
    fn empty_or_cut_bodies_never_panic() {
        let empty = body(|_| {});
        let call = |c: Call| reply(&c, &empty);
        for p in Prop::ALL {
            assert!(matches!(call(get(p)), Err(CallError::BadReply(_))), "{p:?}");
        }
        let others = Call::Get { destination: ":1.9".into(), path: "/".into(), property: Property::RegisteredItems };
        for c in [
            others,
            Call::ListNames,
            Call::GetNameOwner { name: "a.b".into() },
            Call::GetConnectionUnixProcessID { name: ":1.7".into() },
            Call::RequestName,
            Call::AboutToShow { destination: ":1.7".into(), path: "/m".into(), id: 0 },
            layout_call(),
            Call::ReadOne,
        ] {
            assert!(matches!(call(c.clone()), Err(CallError::BadReply(_))), "{c:?}");
        }
        let revision_only = body(|i| i.append(3u32));
        assert!(matches!(reply(&layout_call(), &revision_only), Err(CallError::BadReply(_))), "GetLayout cut after its revision");
        for (sender, interface, member) in [
            ("org.freedesktop.DBus", "org.freedesktop.DBus", "NameOwnerChanged"),
            (":1.9", WATCHER_INTERFACE, "StatusNotifierItemRegistered"),
            (":1.9", WATCHER_INTERFACE, "StatusNotifierItemUnregistered"),
            (":1.7", MENU_INTERFACE, "LayoutUpdated"),
            (":1.7", MENU_INTERFACE, "ItemsPropertiesUpdated"),
        ] {
            assert_eq!(signal_from(sender, "/x", interface, member, |_| {}), Signal::Other, "{member} with an empty body");
        }
        let cut = signal_from(":1.7", "/m", MENU_INTERFACE, "ItemsPropertiesUpdated", |i| {
            i.append_array(&signature("(ia{sv})"), |_| {});
        });
        assert_eq!(cut, Signal::Other, "ItemsPropertiesUpdated without its removed list");
        let cut = signal_from("org.freedesktop.DBus", "/", "org.freedesktop.DBus", "NameOwnerChanged", |i| {
            i.append("org.kde.StatusNotifierWatcher")
        });
        assert_eq!(cut, Signal::Other, "NameOwnerChanged with one argument");
    }

    #[test]
    fn signals_decode_or_are_other() {
        let owner_from = |sender: &str| {
            signal_from(sender, "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", |i| {
                i.append(":1.7");
                i.append(":1.7");
                i.append("");
            })
        };
        assert_eq!(
            owner_from("org.freedesktop.DBus"),
            Signal::NameOwnerChanged { name: ":1.7".into(), old: ":1.7".into(), new: String::new() }
        );
        assert_eq!(owner_from(":1.9"), Signal::Other, "a peer faking the bus driver's departure signal");
        for (member, prop) in [
            ("NewTitle", Prop::Title),
            ("NewIcon", Prop::IconName),
            ("NewAttentionIcon", Prop::AttentionIconName),
            ("NewToolTip", Prop::ToolTip),
        ] {
            let mut props = PropSet::default();
            props.insert(prop);
            assert_eq!(
                signal_from(":1.7", DEFAULT_ITEM_PATH, ITEM_INTERFACE, member, |_| {}),
                Signal::ItemChanged { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), props },
                "{member}"
            );
        }
        let chromium_double = signal_from(":1.7", DEFAULT_ITEM_PATH, "org.freedesktop.StatusNotifierItem", "NewIcon", |_| {});
        assert_eq!(chromium_double, Signal::Other, "Chromium's copy on the freedesktop interface is not read twice");
        let registered = signal_from(":1.9", "/StatusNotifierWatcher", WATCHER_INTERFACE, "StatusNotifierItemRegistered", |i| {
            i.append(":1.7/StatusNotifierItem")
        });
        assert_eq!(registered, Signal::ItemRegistered(":1.7/StatusNotifierItem".into()));
        let gone =
            signal_from(":1.9", "/StatusNotifierWatcher", WATCHER_INTERFACE, "StatusNotifierItemUnregistered", |i| i.append(":1.7/x"));
        assert_eq!(gone, Signal::ItemUnregistered(":1.7/x".into()));
        let service =
            |s: &str| signal_from(":1.9", "/StatusNotifierWatcher", WATCHER_INTERFACE, "StatusNotifierItemRegistered", |i| i.append(s));
        let longest = format!(":1.7/{}", "a".repeat(1019));
        assert_eq!(service(&longest), Signal::ItemRegistered(longest.clone()), "1024 bytes: passed whole, never cut");
        assert_eq!(service(&format!("{longest}a")), Signal::Other, "1025 bytes: refused");
        let layout = signal_from(":1.7", "/MenuBar", MENU_INTERFACE, "LayoutUpdated", |i| {
            i.append(12u32);
            i.append(5i32);
        });
        assert_eq!(layout, Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 12, parent: 5 });
        let props = signal_from(":1.7", "/MenuBar", MENU_INTERFACE, "ItemsPropertiesUpdated", |i| {
            i.append_array(&signature("(ia{sv})"), |a| {
                a.append_struct(|s| {
                    s.append(3i32);
                    s.append_dict(&signature("s"), &signature("v"), |d| {
                        d.append_dict_entry(|e| {
                            e.append("label");
                            e.append(Variant("_Mute"));
                        });
                        d.append_dict_entry(|e| {
                            e.append("toggle-state");
                            e.append(Variant(1i32));
                        });
                        d.append_dict_entry(|e| {
                            e.append("icon-data");
                            e.append(Variant(vec![0u8; 512]));
                        });
                    });
                })
            });
            i.append_array(&signature("(ias)"), |a| {
                a.append_struct(|s| {
                    s.append(4i32);
                    s.append(vec!["enabled", "icon-name"]);
                })
            });
        });
        let mute = MenuProps { label: Some("Mute".into()), toggle_state: Some(1), ..MenuProps::default() };
        assert_eq!(
            props,
            Signal::ItemsPropertiesUpdated {
                sender: ":1.7".into(),
                path: "/MenuBar".into(),
                updated: vec![(3, mute)],
                removed: vec![(4, vec!["enabled".into()])]
            }
        );
        assert_eq!(signal_from(":1.7", "/", "org.example", "Anything", |i| i.append(1u8)), Signal::Other);
    }

    #[test]
    fn new_status_carries_its_value() {
        let status = signal_from(":1.7", DEFAULT_ITEM_PATH, ITEM_INTERFACE, "NewStatus", |i| i.append("Passive"));
        assert_eq!(status, Signal::ItemStatus { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), status: Status::Passive });
        let mut props = PropSet::default();
        props.insert(Prop::Status);
        let bare = signal_from(":1.7", DEFAULT_ITEM_PATH, ITEM_INTERFACE, "NewStatus", |_| {});
        assert_eq!(
            bare,
            Signal::ItemChanged { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), props },
            "without its argument: re-read"
        );
    }

    #[test]
    fn layout_decodes_checkmarks_radios_separators_submenus() {
        let root = t(
            0,
            &[("children-display", V::S("submenu"))],
            vec![
                t(1, &[("label", V::S("_Open")), ("icon-data", V::Bytes(4096)), ("shortcut", V::S("ignored"))], vec![]),
                t(2, &[("label", V::S("Mute")), ("toggle-type", V::S("checkmark")), ("toggle-state", V::I(1))], vec![]),
                t(3, &[("label", V::S("Low")), ("toggle-type", V::S("radio")), ("toggle-state", V::I(0))], vec![]),
                t(4, &[("type", V::S("separator"))], vec![]),
                t(5, &[("label", V::S("Disabled")), ("enabled", V::B(false))], vec![]),
                t(6, &[("label", V::S("Hidden")), ("visible", V::B(false))], vec![]),
                t(7, &[("label", V::S("More")), ("children-display", V::S("submenu"))], vec![]),
                t(8, &[("label", V::I(5)), ("enabled", V::S("yes"))], vec![]),
            ],
        );
        let l = decoded(&layout_reply(9, &root));
        assert_eq!((l.revision, l.len(), l.depth()), (9, 8, 1));
        let n = |id| l.node(id).unwrap();
        assert_eq!(n(1).label(), "Open");
        assert_eq!(n(1).props, MenuProps { label: Some("Open".into()), ..MenuProps::default() }, "icon-data and shortcut never decoded");
        assert_eq!((n(2).props.toggle, n(2).props.toggle_state), (Some(Toggle::Checkmark), Some(1)));
        assert_eq!((n(3).props.toggle, n(3).props.toggle_state), (Some(Toggle::Radio), Some(0)));
        assert!(n(4).is_separator() && !n(1).is_separator());
        assert!(!n(5).is_enabled() && n(5).is_visible());
        assert!(!n(6).is_visible() && n(6).is_enabled());
        assert!(n(7).has_submenu() && n(7).children.is_empty(), "a submenu announced, its children fetched when opened");
        assert!(!n(1).has_submenu());
        assert_eq!(n(8).props, MenuProps::default(), "values of the wrong type are absent, the spec defaults apply");
        assert!(l.node(99).is_none());
    }

    #[test]
    fn layout_bounded_to_500_entries_and_depth_8() {
        let wide = t(0, &[], (1..=501).map(|id| t(id, &[("label", V::S("entry"))], vec![])).collect());
        let l = decoded(&layout_reply(1, &wide));
        assert_eq!(l.len(), MAX_MENU_ENTRIES);
        assert!(l.node(500).is_some() && l.node(501).is_none(), "the first 500 kept, in order");
        let huge = t(0, &[], (1..=10_000).map(|id| t(id, &[], vec![])).collect());
        let huge = decoded(&layout_reply(1, &huge));
        assert_eq!(huge.len(), MAX_MENU_ENTRIES);
        assert!(huge.root.children.capacity() <= 2 * MAX_MENU_ENTRIES, "never 10,000 allocated");
        let deep = decoded(&layout_reply(1, &chain(9)));
        assert_eq!(deep.depth(), MAX_MENU_DEPTH);
        assert!(deep.node(8).is_some() && deep.node(9).is_none(), "depth 9 dropped");
        assert_eq!(decoded(&layout_reply(1, &chain(8))).depth(), 8, "depth 8 kept whole");
    }

    #[test]
    fn graft_and_patch_keep_the_bounds() {
        let mut l = decoded(&layout_reply(1, &t(0, &[], (1..=499).map(|id| t(id, &[("label", V::S("x"))], vec![])).collect())));
        let sub = decoded(&layout_reply(2, &t(7, &[("label", V::S("More"))], (1000..1010).map(|id| t(id, &[], vec![])).collect())));
        assert!(l.graft(sub.root));
        assert_eq!(l.len(), MAX_MENU_ENTRIES, "499 + 10 grafted, cut to 500");
        assert_eq!(l.node(7).unwrap().label(), "More");
        assert!(l.node(1009).is_some(), "the grafted submenu whole, in document order");
        assert!(l.node(490).is_some() && l.node(491).is_none(), "the last nine top entries dropped");
        assert!(!l.graft(MenuNode { id: 4242, props: MenuProps::default(), children: vec![] }), "unknown parent");
        let mut deep = decoded(&layout_reply(1, &chain(7)));
        let below = decoded(&layout_reply(1, &chain(4)));
        let mut grafted = below.root;
        grafted.id = 7;
        assert!(deep.graft(grafted));
        assert_eq!(deep.depth(), MAX_MENU_DEPTH, "a graft deep down is cut at depth 8");
        let mut p = decoded(&layout_reply(1, &t(0, &[], vec![t(1, &[("label", V::S("Mute")), ("enabled", V::B(false))], vec![])])));
        let update = MenuProps { label: Some("Unmute".into()), toggle: Some(Toggle::Checkmark), ..MenuProps::default() };
        p.patch(&[(1, update), (99, MenuProps::default())], &[(1, vec!["enabled".into()])]);
        let n = p.node(1).unwrap();
        assert_eq!((n.label(), n.props.toggle, n.is_enabled()), ("Unmute", Some(Toggle::Checkmark), true));
    }

    #[test]
    fn graft_keeps_fetched_submenus_across_a_reread() {
        let more = |children| t(7, &[("label", V::S("More")), ("children-display", V::S("submenu"))], children);
        let mut l = decoded(&layout_reply(1, &t(0, &[], vec![t(1, &[("label", V::S("Open"))], vec![]), more(vec![])])));
        let sub = decoded(&layout_reply(1, &more((70..72).map(|id| t(id, &[("label", V::S("x"))], vec![])).collect())));
        assert!(l.graft(sub.root));
        assert!(l.node(70).is_some(), "submenu 7 fetched");
        // LayoutUpdated(parent 0): the root re-read at depth 1 announces 7 again, without children.
        let reread = decoded(&layout_reply(2, &t(0, &[], vec![t(1, &[("label", V::S("Open"))], vec![]), more(vec![])])));
        assert!(l.graft(reread.root));
        assert_eq!(l.node(7).unwrap().children.len(), 2, "the fetched children carried over");
        assert_eq!(l.len(), 4);
        // 7 is no longer a submenu: what was fetched under it goes.
        let flat = decoded(&layout_reply(3, &t(0, &[], vec![t(7, &[("label", V::S("More"))], vec![])])));
        assert!(l.graft(flat.root));
        assert!(l.node(70).is_none() && l.len() == 1, "not carried over to a plain entry");
    }

    #[test]
    fn labels_lose_mnemonics_and_controls() {
        assert_eq!(label("_File"), "File");
        assert_eq!(label("Save __as"), "Save _as");
        assert_eq!(label("a\u{7}b"), "ab");
        assert_eq!(label("_Quit _Now"), "Quit Now");
        assert_eq!(clean("tab\there\u{1b}[31m red"), "tab here[31m red", "escape sequences lose their ESC");
        assert_eq!(clean("one\ntwo\r\nthree"), "one two  three", "line breaks become spaces");
        assert_eq!(clean("two  spaces"), "two  spaces");
        assert_eq!(clean("ab\u{202e}cd\u{200b}e\u{2066}f\u{2069}"), "abcdef", "bidi overrides and zero-width characters removed");
        assert_eq!(
            clean("a\u{200c}b\u{200d}c"),
            "a\u{200c}b\u{200d}c",
            "ZWNJ/ZWJ are legitimate text shaping (Arabic, emoji sequences), kept"
        );
        assert_eq!(clean("a\u{61c}b"), "ab", "the Arabic Letter Mark is a bidi control too, removed");
    }

    #[test]
    fn long_text_is_cut_to_256_characters() {
        let hostile: String = "é\u{1}x\n".repeat(2_600);
        assert!(hostile.len() > 10_000);
        let title = reply(&get(Prop::Title), &body(|i| i.append(Variant(hostile.as_str()))));
        let Ok(Reply::Prop(PropValue::Text(t))) = title else { panic!("{title:?}") };
        assert_eq!(t.chars().count(), MAX_TEXT);
        assert!(t.chars().all(|c| c == 'é' || c == 'x' || c == ' '), "controls removed, line breaks spaced");
        assert_eq!(label(&"_x".repeat(10_000)), "x".repeat(MAX_TEXT), "labels too");
    }
}
