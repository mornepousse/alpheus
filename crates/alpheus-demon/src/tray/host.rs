//! The StatusNotifierItem host, a pure state machine over `Outbox` (no
//! socket, no D-Bus library): which items exist, what the bar knows of each,
//! and the calls that keep it current — one `Get` per property (never
//! `GetAll`, never a pixmap), well-known names resolved once, items keyed by
//! (unique name, path), departures by `NameOwnerChanged` only, signals
//! coalesced to one re-read per frame, every bound applied.

use super::bus::{Call, CallError, CallId, Outbox, Property};
use super::codec::{Reply, Signal};
use super::{
    DEFAULT_ITEM_PATH, Gesture, ITEM_INTERFACE, ITEM_NAME_PREFIXES, ItemKey, MAX_ITEMS, MAX_ITEMS_PER_OWNER, Prop, PropSet, PropValue,
    Status,
};
use crate::log::write as log;
use crate::text::clean;
use std::collections::HashMap;
use std::path::PathBuf;

/// What the bar knows of one item. Texts already `clean`ed and bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub key: ItemKey,
    /// The bus name it registered with (well-known or unique), for the watcher's list.
    pub service: String,
    pub id: Option<String>,
    pub title: Option<String>,
    pub status: Status,
    pub icon_name: Option<String>,
    pub attention_icon_name: Option<String>,
    /// ToolTip's title.
    pub tooltip: Option<String>,
    /// dbusmenu object path.
    pub menu: Option<String>,
    pub item_is_menu: bool,
    /// /proc/<pid>/comm of its owner (GetConnectionUnixProcessID), cleaned.
    pub process: Option<String>,
    /// First Gets not answered yet.
    first: PropSet,
    /// Failed or timed out: not asked again until a signal names them.
    failed: PropSet,
    /// Named by a signal, to re-read at the next frame.
    dirty: PropSet,
    /// Gets in flight.
    asking: PropSet,
    /// GetConnectionUnixProcessID not answered yet (part of being ready: the fallback letter needs it).
    awaiting_pid: bool,
    /// First Gets answered by a D-Bus error.
    errors: usize,
}

impl Item {
    fn new(key: ItemKey, service: String) -> Item {
        Item {
            key,
            service,
            id: None,
            title: None,
            status: Status::Active,
            icon_name: None,
            attention_icon_name: None,
            tooltip: None,
            menu: None,
            item_is_menu: false,
            process: None,
            first: PropSet::ALL,
            failed: PropSet::default(),
            dirty: PropSet::default(),
            asking: PropSet::ALL,
            awaiting_pid: true,
            errors: 0,
        }
    }

    /// Every first Get and the process id have answered, failed or timed out (the process name is waited for too, so a fallback letter never changes once drawn).
    pub fn is_ready(&self) -> bool {
        self.first.is_empty() && !self.awaiting_pid
    }

    /// Ready and not dropped as a bogus registration: drawn.
    fn is_shown(&self) -> bool {
        self.is_ready() && self.errors < Prop::ALL.len()
    }

    /// ToolTip title, else Title, else Id (spec § 2).
    pub fn tooltip_text(&self) -> Option<&str> {
        self.tooltip.as_deref().or(self.title.as_deref()).or(self.id.as_deref())
    }

    /// "<service><path>" (Plasma's format) for RegisteredStatusNotifierItems and the watcher's signals.
    pub fn watcher_entry(&self) -> String {
        format!("{}{}", self.service, self.key.path)
    }

    /// Dirty properties no Get is reading yet.
    fn to_read(&self) -> impl Iterator<Item = Prop> + '_ {
        self.dirty.iter().filter(|p| !self.asking.contains(*p))
    }

    /// Stores `v` as `p`; whether anything changed. An empty, or whitespace-only, text is absent
    /// (`codec::clean` turns a tooltip of just "\n" into a single
    /// space, which `tooltip_row` would otherwise draw as a blank floating box). Compares only the
    /// one field a property maps to — never the whole item (no clone).
    fn apply(&mut self, p: Prop, v: PropValue) -> bool {
        fn set<T: PartialEq>(field: &mut T, value: T) -> bool {
            let changed = *field != value;
            *field = value;
            changed
        }
        let text = |v: PropValue| match v {
            PropValue::Text(t) if !t.trim().is_empty() => Some(t),
            _ => None,
        };
        match (p, v) {
            (Prop::Id, v) => set(&mut self.id, text(v)),
            (Prop::Title, v) => set(&mut self.title, text(v)),
            (Prop::IconName, v) => set(&mut self.icon_name, text(v)),
            (Prop::AttentionIconName, v) => set(&mut self.attention_icon_name, text(v)),
            (Prop::ToolTip, v) => set(&mut self.tooltip, text(v)),
            (Prop::Status, PropValue::Status(s)) => set(&mut self.status, s),
            // "/" and Qt's "/NO_DBUSMENU" (QSystemTrayIcon without a context menu): no dbusmenu object
            // behind them, so a right click sends ContextMenu instead of opening a menu.
            (Prop::Menu, PropValue::Path(m)) => set(&mut self.menu, Some(m).filter(|m| m != "/" && m != "/NO_DBUSMENU")),
            (Prop::ItemIsMenu, PropValue::Bool(b)) => set(&mut self.item_is_menu, b),
            _ => false,
        }
    }
}

/// What changed since the last `take_changes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Shown items changed: the bars redraw.
    pub redraw: bool,
    /// Watcher entries accepted / gone (the watcher emits ItemRegistered / ItemUnregistered).
    pub registered: Vec<String>,
    pub unregistered: Vec<String>,
}

/// A D-Bus object path: "/" or "/a/b_c", nothing else (libdbus refuses to address anything else).
fn is_object_path(p: &str) -> bool {
    p == "/"
        || (p.len() <= 1024
            && p.starts_with('/')
            && p[1..].split('/').all(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')))
}

/// A D-Bus bus name: ":1.42", or "org.example.App-1" (two elements or more, none starting with a digit).
fn is_bus_name(n: &str) -> bool {
    // The length first, in O(1): a megabyte of dots must never be walked, let alone collected.
    if n.len() > 255 {
        return false;
    }
    let (unique, rest) = match n.strip_prefix(':') {
        Some(r) => (true, r),
        None => (false, n),
    };
    let mut elements = 0;
    for e in rest.split('.') {
        let valid = !e.is_empty()
            && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && (unique || !e.as_bytes()[0].is_ascii_digit());
        if !valid {
            return false;
        }
        elements += 1;
    }
    elements >= 2
}

/// The three registration forms (spec § 1): a bus name → (name, DEFAULT_ITEM_PATH); a path alone →
/// (sender, path); "name/path" → split at the first '/'. None: empty, or a path with no sender —
/// and a name or path D-Bus would not accept (nothing is ever sent to it).
pub fn split_service(service: &str, sender: &str) -> Option<(String, String)> {
    let (name, path) = match service.find('/') {
        Some(0) => (sender, service),
        Some(i) => (&service[..i], &service[i..]),
        None => (service, DEFAULT_ITEM_PATH),
    };
    (is_bus_name(name) && is_object_path(path)).then(|| (name.to_string(), path.to_string()))
}

/// The NameOwnerChanged rule narrowed to one name (arg0), and the per-item signal rule (sender, interface).
pub fn owner_rule(name: &str) -> String {
    format!("type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{name}'")
}

pub fn item_rule(owner: &str) -> String {
    format!("type='signal',sender='{owner}',interface='{ITEM_INTERFACE}'")
}

/// What one of the host's calls was for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Names,
    Owner { name: String, path: String },
    Prop(ItemKey, Prop),
    Pid(ItemKey),
    Rule,
}

pub struct Host {
    /// In registration order.
    items: Vec<Item>,
    calls: HashMap<CallId, Pending>,
    proc_root: PathBuf,
    changes: Changes,
    refusal_logged: bool,
    /// Whether a self-registration refusal has already been logged once: a looping signal from a misbehaving watcher — the same self-naming entry in its
    /// list on every `NameOwnerChanged`, say — must never flood the log.
    self_registration_logged: bool,
    /// Our own unique name: never one of the items we host (checked in `add`, on the resolved
    /// owner, so a registration naming our own well-known name, an entry of
    /// another watcher's list, or that watcher's ItemRegistered signal are all caught too, not
    /// only a direct RegisterStatusNotifierItem naming us literally).
    own: String,
}

impl Host {
    /// `proc_root`: "/proc" (tests: a temporary directory with <pid>/comm files). `own`: our
    /// connection's unique name, known once at startup (`SessionBus::unique_name`).
    pub fn new(proc_root: PathBuf, own: String) -> Host {
        Host {
            items: Vec::new(),
            calls: HashMap::new(),
            proc_root,
            changes: Changes::default(),
            refusal_logged: false,
            self_registration_logged: false,
            own,
        }
    }

    fn call(&mut self, call: Call, pending: Pending, out: &mut dyn Outbox) {
        let id = out.call(call);
        self.calls.insert(id, pending);
    }

    /// Startup inventory (spec § 1): ListNames, then every name with an ITEM_NAME_PREFIXES prefix is
    /// registered as if it had called RegisterStatusNotifierItem(name) — at DEFAULT_ITEM_PATH.
    pub fn start(&mut self, out: &mut dyn Outbox) {
        self.call(Call::ListNames, Pending::Names, out);
    }

    /// A registration from our watcher (`sender` = the caller's unique name) or from the other
    /// watcher's list or signal (`sender` = ""). Resolves a well-known name once (GetNameOwner), then
    /// keys by (unique, path); a known key is a no-op. On a new item: owner_rule + item_rule added,
    /// eight Gets and one GetConnectionUnixProcessID sent at once. The 65th item is refused (logged once),
    /// counting the well-known names being resolved: never more GetNameOwner in flight than room.
    pub fn register(&mut self, service: &str, sender: &str, out: &mut dyn Outbox) {
        let Some((name, path)) = split_service(service, sender) else { return };
        if name.starts_with(':') {
            // From our own watcher (`sender` known), a unique name is accepted only when it is the
            // caller's own: Electron and libappindicator register their own connection, and
            // nothing else may point us at somebody else's. An entry of another watcher's list or
            // signal (`sender` empty) has no caller to compare with.
            if !sender.is_empty() && name != sender {
                return;
            }
            self.add(ItemKey { owner: name.clone(), path }, name, out);
            return;
        }
        // `sender` is the call's real unique name (set by the bus daemon, never spoofable): an item
        // already known under it, even by a different well-known alias, needs no resolving.
        let known = self.items.iter().any(|i| (i.service == name || i.key.owner == sender) && i.key.path == path);
        let asked = self.calls.values().any(|p| matches!(p, Pending::Owner { name: n, path: q } if *n == name && *q == path));
        let resolving = self.calls.values().filter(|p| matches!(p, Pending::Owner { .. })).count();
        if !known && !asked && !self.refused(resolving) {
            self.call(Call::GetNameOwner { name: name.clone() }, Pending::Owner { name, path }, out);
        }
    }

    /// No room for one more item beside the `resolving` names (bounds: MAX_ITEMS); the first refusal is logged.
    fn refused(&mut self, resolving: usize) -> bool {
        if self.items.len() + resolving < MAX_ITEMS {
            return false;
        }
        if !self.refusal_logged {
            log(&format!("tray: more than {MAX_ITEMS} items, the next ones are not shown"));
            self.refusal_logged = true;
        }
        true
    }

    fn add(&mut self, key: ItemKey, service: String, out: &mut dyn Outbox) {
        // Our own connection is never one of the items we host (`key.owner` is always the resolved unique name by this point,
        // whether `register` had it directly or asked GetNameOwner to get it).
        if key.owner == self.own {
            if !self.self_registration_logged {
                log(&format!("tray: refusing a registration naming our own unique name ({service:?})"));
                self.self_registration_logged = true;
            }
            return;
        }
        // A duplicate registration of a known item: real clients re-register on their own (a
        // reconnect, a retry once a watcher appears) and no StatusNotifierItem signal exists for
        // Menu — so this is the only chance to notice an item that registered with none (or
        // "/NO_DBUSMENU") and has since gained a real one.
        if self.items.iter().any(|i| i.key == key) {
            self.reread_menu(&key, out);
            return;
        }
        if self.refused(0) {
            return;
        }
        // One application cannot take the whole tray.
        if self.items.iter().filter(|i| i.key.owner == key.owner).count() >= MAX_ITEMS_PER_OWNER {
            return;
        }
        if !self.items.iter().any(|i| i.key.owner == key.owner) {
            self.call(Call::AddMatch { rule: owner_rule(&key.owner) }, Pending::Rule, out);
            self.call(Call::AddMatch { rule: item_rule(&key.owner) }, Pending::Rule, out);
        }
        for p in Prop::ALL {
            let get = Call::Get { destination: key.owner.clone(), path: key.path.clone(), property: Property::Item(p) };
            self.call(get, Pending::Prop(key.clone(), p), out);
        }
        self.call(Call::GetConnectionUnixProcessID { name: key.owner.clone() }, Pending::Pid(key.clone()), out);
        let item = Item::new(key, service);
        self.changes.registered.push(item.watcher_entry());
        self.items.push(item);
    }

    /// One Get(Menu), with the same deadline as any other property Get; skipped while one is
    /// already in flight, so a burst of duplicate registrations asks only once.
    fn reread_menu(&mut self, key: &ItemKey, out: &mut dyn Outbox) {
        let Some(item) = self.items.iter_mut().find(|i| i.key == *key) else { return };
        if item.asking.contains(Prop::Menu) {
            return;
        }
        item.asking.insert(Prop::Menu);
        let get = Call::Get { destination: key.owner.clone(), path: key.path.clone(), property: Property::Item(Prop::Menu) };
        self.call(get, Pending::Prop(key.clone(), Prop::Menu), out);
    }

    fn remove(&mut self, index: usize, out: &mut dyn Outbox) {
        let item = self.items.remove(index);
        if !self.items.iter().any(|i| i.key.owner == item.key.owner) {
            // No answer to read: sent without a pending entry (`Outbox::send`).
            out.send(Call::RemoveMatch { rule: owner_rule(&item.key.owner) });
            out.send(Call::RemoveMatch { rule: item_rule(&item.key.owner) });
        }
        self.changes.unregistered.push(item.watcher_entry());
        self.changes.redraw |= item.is_shown();
    }

    /// Whether `id` is one of the host's calls (the router asks before routing).
    pub fn owns(&self, id: CallId) -> bool {
        self.calls.contains_key(&id)
    }

    pub fn answer(&mut self, id: CallId, result: Result<Reply, CallError>, out: &mut dyn Outbox) {
        let Some(pending) = self.calls.remove(&id) else { return };
        match (pending, result) {
            (Pending::Names, Ok(Reply::Names(names))) => {
                for name in names.iter().filter(|n| ITEM_NAME_PREFIXES.iter().any(|p| n.starts_with(p))) {
                    self.register(name, "", out);
                }
            }
            // The bus driver answers GetNameOwner("org.freedesktop.DBus") with that very name, not a
            // unique one: never an owner to key an item on, never a destination to Get from.
            (Pending::Owner { name, path }, Ok(Reply::Owner(unique))) if unique.starts_with(':') => {
                self.add(ItemKey { owner: unique, path }, name, out)
            }
            (Pending::Pid(key), result) => {
                let Some(index) = self.items.iter().position(|i| i.key == key) else { return };
                if let Ok(Reply::Pid(pid)) = result {
                    // Bytes, not a string: a comm with a non-UTF-8 byte (read_to_string would refuse
                    // the whole file) still yields the letter a clean prefix can give.
                    let bytes = std::fs::read(self.proc_root.join(pid.to_string()).join("comm")).unwrap_or_default();
                    let comm = String::from_utf8_lossy(&bytes);
                    self.items[index].process = Some(clean(comm.trim_end_matches('\n'))).filter(|p| !p.is_empty());
                }
                // Answered, failed or timed out: no longer awaited (asked once per item).
                self.items[index].awaiting_pid = false;
                self.first_answered(index, out);
            }
            (Pending::Prop(key, p), result) => self.property(&key, p, result, out),
            _ => {}
        }
    }

    fn property(&mut self, key: &ItemKey, p: Prop, result: Result<Reply, CallError>, out: &mut dyn Outbox) {
        let Some(index) = self.items.iter().position(|i| i.key == *key) else { return };
        let item = &mut self.items[index];
        item.asking.remove(p);
        let first = item.first.contains(p);
        item.first.remove(p);
        let mut changed = false;
        match result {
            Ok(Reply::Prop(v)) => {
                item.failed.remove(p);
                changed = item.apply(p, v);
            }
            Err(CallError::Remote(_)) if first => {
                item.errors += 1;
                item.failed.insert(p);
            }
            _ => item.failed.insert(p),
        }
        if first {
            self.first_answered(index, out);
        } else if changed && item.is_ready() {
            self.changes.redraw = true;
        }
    }

    /// After one of the first answers (a first Get or the process id): once the item is ready it is
    /// drawn — or dropped, when every first Get failed with a D-Bus error (a bogus registration: no
    /// such object, no such interface; never shown, never retried).
    fn first_answered(&mut self, index: usize, out: &mut dyn Outbox) {
        let item = &self.items[index];
        if !item.is_ready() {
            return;
        }
        if item.errors == Prop::ALL.len() {
            self.remove(index, out);
        } else {
            self.changes.redraw = true;
        }
    }

    /// NameOwnerChanged (departures: an empty new owner removes every item of that unique name, and
    /// its rules), ItemChanged (marks dirty, asks for a frame), ItemStatus (applied at once).
    pub fn signal(&mut self, s: &Signal, out: &mut dyn Outbox) {
        match s {
            Signal::NameOwnerChanged { name, new, .. } if new.is_empty() => {
                while let Some(index) = self.items.iter().position(|i| i.key.owner == *name) {
                    self.remove(index, out);
                }
            }
            Signal::ItemChanged { sender, path, props } => {
                if let Some(item) = self.items.iter_mut().find(|i| i.key.owner == *sender && i.key.path == *path) {
                    for p in props.iter() {
                        item.dirty.insert(p);
                        // Named again: this is its fresh chance, not the one that failed.
                        item.failed.remove(p);
                    }
                }
            }
            Signal::ItemStatus { sender, path, status } => {
                if let Some(item) = self.items.iter_mut().find(|i| i.key.owner == *sender && i.key.path == *path) {
                    let changed = item.status != *status;
                    item.status = *status;
                    item.failed.remove(Prop::Status);
                    item.dirty.remove(Prop::Status);
                    self.changes.redraw |= changed && item.is_ready();
                }
            }
            _ => {}
        }
    }

    /// The other watcher's ItemUnregistered (host-only mode).
    pub fn unregister(&mut self, service: &str, out: &mut dyn Outbox) {
        let Some((name, path)) = split_service(service, "") else { return };
        while let Some(index) = self.items.iter().position(|i| (i.service == name || i.key.owner == name) && i.key.path == path) {
            self.remove(index, out);
        }
    }

    /// A frame callback came: one Get per dirty property (previously failed ones included: being
    /// dirty here means a signal cleared `failed` and named them again).
    pub fn on_frame(&mut self, out: &mut dyn Outbox) {
        let mut gets = Vec::new();
        for item in &mut self.items {
            let props: Vec<Prop> = item.to_read().collect();
            for p in props {
                debug_assert!(!item.failed.contains(p), "a signal clears failed before on_frame re-reads it");
                item.dirty.remove(p);
                item.asking.insert(p);
                gets.push((item.key.clone(), p));
            }
        }
        for (key, p) in gets {
            let get = Call::Get { destination: key.owner.clone(), path: key.path.clone(), property: Property::Item(p) };
            self.call(get, Pending::Prop(key, p), out);
        }
    }

    /// Some property is dirty and no Get is reading it yet: the wiring asks for one frame (and keeps
    /// its own "frame pending" flag meanwhile).
    pub fn wants_frame(&self) -> bool {
        self.items.iter().any(|i| i.to_read().next().is_some())
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, key: &ItemKey) -> Option<&Item> {
        self.items.iter().find(|i| i.key == *key)
    }

    pub fn take_changes(&mut self) -> Changes {
        std::mem::take(&mut self.changes)
    }

    /// The bus closed: everything forgotten (no call possible).
    pub fn clear(&mut self) -> Changes {
        let mut changes = std::mem::take(&mut self.changes);
        changes.redraw |= self.items.iter().any(Item::is_shown);
        changes.unregistered.extend(self.items.iter().map(Item::watcher_entry));
        self.items.clear();
        self.calls.clear();
        changes
    }
}

/// What a gesture on a tray icon asks of the wiring (spec § 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureOutcome {
    Nothing,
    /// Right click with a Menu, or a left click on an ItemIsMenu item.
    OpenMenu,
    /// Activate sent: its answer decides (an error reply with a Menu → open the menu).
    Activated(CallId),
}

impl Host {
    /// Left → Activate(x, y) (OpenMenu when ItemIsMenu); Right → OpenMenu with a Menu, else
    /// ContextMenu(x, y); Middle → SecondaryActivate(x, y); Scroll → Scroll(delta, orientation).
    /// Nothing for an unknown key. Only Activate expects an answer; the others are sent with
    /// NO_REPLY_EXPECTED (nothing depends on them: no pending call, no timer).
    pub fn gesture(&mut self, key: &ItemKey, g: Gesture, at: (i32, i32), out: &mut dyn Outbox) -> GestureOutcome {
        let Some(item) = self.item(key) else { return GestureOutcome::Nothing };
        let (destination, path, (x, y)) = (key.owner.clone(), key.path.clone(), at);
        match g {
            Gesture::Left if !item.item_is_menu => GestureOutcome::Activated(out.call(Call::Activate { destination, path, x, y })),
            // ItemIsMenu: the item only has a context menu, like a right click.
            Gesture::Left | Gesture::Right if item.menu.is_some() => GestureOutcome::OpenMenu,
            Gesture::Left | Gesture::Right => {
                out.send(Call::ContextMenu { destination, path, x, y });
                GestureOutcome::Nothing
            }
            Gesture::Middle => {
                out.send(Call::SecondaryActivate { destination, path, x, y });
                GestureOutcome::Nothing
            }
            Gesture::Scroll { delta, orientation } => {
                out.send(Call::Scroll { destination, path, delta, orientation });
                GestureOutcome::Nothing
            }
        }
    }

    /// Activate's answer: a D-Bus error reply (Steam's
    /// libappindicator has no Activate: UnknownMethod) opens the item's menu when it
    /// has one; a timeout is not a failure (a slow application may have acted), a
    /// success needs nothing more.
    pub(crate) fn opens_menu_after_activate(&self, key: &ItemKey, result: &Result<Reply, CallError>) -> bool {
        matches!(result, Err(CallError::Remote(_))) && self.item(key).is_some_and(|i| i.menu.is_some())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod gesture_tests {
    use super::*;
    use crate::tray::Orientation;
    use crate::tray::icons::testing::{Fake, host};

    fn key() -> ItemKey {
        ItemKey { owner: ":1.100".into(), path: "/StatusNotifierItem".into() }
    }

    const MENU: (Prop, &str) = (Prop::Menu, "/MenuBar");

    #[test]
    fn left_click_activates_at_the_press_position() {
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "nimbus"), MENU])]);
        let GestureOutcome::Activated(id) = h.gesture(&key(), Gesture::Left, (412, 11), &mut out) else { panic!("not activated") };
        let activate = Call::Activate { destination: ":1.100".into(), path: "/StatusNotifierItem".into(), x: 412, y: 11 };
        assert_eq!(out.calls, vec![(id, activate)], "one call, to the unique name, at the press position");
        assert!(out.sent.is_empty());
    }

    #[test]
    fn left_click_on_item_is_menu_opens_the_menu() {
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::ItemIsMenu, "true"), MENU])]);
        assert_eq!(h.gesture(&key(), Gesture::Left, (5, 5), &mut out), GestureOutcome::OpenMenu);
        assert!(out.calls.is_empty() && out.sent.is_empty(), "no Activate: the menu instead");
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::ItemIsMenu, "true")])]);
        assert_eq!(h.gesture(&key(), Gesture::Left, (5, 6), &mut out), GestureOutcome::Nothing);
        let context = Call::ContextMenu { destination: ":1.100".into(), path: "/StatusNotifierItem".into(), x: 5, y: 6 };
        assert_eq!(out.sent, vec![context], "ItemIsMenu without a Menu: its own context menu");
    }

    #[test]
    fn a_failed_activate_opens_the_menu() {
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "steam"), (Prop::Menu, "/org/ayatana/NotificationItem/steam/Menu")])]);
        assert!(matches!(h.gesture(&key(), Gesture::Left, (1, 1), &mut out), GestureOutcome::Activated(_)));
        let unknown = Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into()));
        assert!(h.opens_menu_after_activate(&key(), &unknown), "Steam: no Activate, its menu instead");
    }

    #[test]
    fn a_timed_out_activate_does_not_open_the_menu() {
        let (h, _) = host(&[Fake::new(&[(Prop::Id, "nimbus"), MENU])]);
        assert!(!h.opens_menu_after_activate(&key(), &Err(CallError::Timeout)), "a slow app may have acted");
        assert!(!h.opens_menu_after_activate(&key(), &Ok(Reply::Unit)), "it acted");
        assert!(!h.opens_menu_after_activate(&key(), &Err(CallError::Disconnected)));
    }

    #[test]
    fn a_failed_activate_without_menu_does_nothing() {
        let (h, _) = host(&[Fake::new(&[(Prop::Id, "quill")])]);
        let failed = Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into()));
        assert!(!h.opens_menu_after_activate(&key(), &failed));
    }

    #[test]
    fn right_click_opens_the_menu_or_context_menu() {
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "nimbus"), MENU])]);
        assert_eq!(h.gesture(&key(), Gesture::Right, (30, 4), &mut out), GestureOutcome::OpenMenu);
        assert!(out.calls.is_empty() && out.sent.is_empty(), "a Menu: never ContextMenu");
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "kettle")])]);
        assert_eq!(h.gesture(&key(), Gesture::Right, (30, 4), &mut out), GestureOutcome::Nothing);
        let context = Call::ContextMenu { destination: ":1.100".into(), path: "/StatusNotifierItem".into(), x: 30, y: 4 };
        assert_eq!((out.calls.len(), out.sent.clone()), (0, vec![context]), "no Menu: ContextMenu, no answer awaited");
    }

    #[test]
    fn a_no_dbusmenu_path_means_context_menu() {
        // Qt's QSystemTrayIcon without a context menu says Menu = "/NO_DBUSMENU" (no menu).
        for none in ["/NO_DBUSMENU", "/"] {
            let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "kettle"), (Prop::Menu, none)])]);
            assert_eq!(h.gesture(&key(), Gesture::Right, (30, 4), &mut out), GestureOutcome::Nothing, "{none}");
            let context = Call::ContextMenu { destination: ":1.100".into(), path: "/StatusNotifierItem".into(), x: 30, y: 4 };
            assert_eq!((out.calls.len(), out.sent.clone()), (0, vec![context]), "{none}: ContextMenu, never an empty menu");
            let failed = Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into()));
            assert!(!h.opens_menu_after_activate(&key(), &failed), "{none}: no menu to open after a failed Activate");
        }
    }

    #[test]
    fn middle_click_secondary_activate() {
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "nimbus"), MENU])]);
        assert_eq!(h.gesture(&key(), Gesture::Middle, (7, 8), &mut out), GestureOutcome::Nothing);
        let secondary = Call::SecondaryActivate { destination: ":1.100".into(), path: "/StatusNotifierItem".into(), x: 7, y: 8 };
        assert_eq!((out.calls.len(), out.sent.clone()), (0, vec![secondary]));
    }

    #[test]
    fn scroll_sends_its_delta_and_orientation() {
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "nimbus")])]);
        for (delta, orientation) in [(120, Orientation::Vertical), (-120, Orientation::Horizontal)] {
            assert_eq!(h.gesture(&key(), Gesture::Scroll { delta, orientation }, (0, 0), &mut out), GestureOutcome::Nothing);
        }
        let path = || "/StatusNotifierItem".to_string();
        assert_eq!(
            out.sent,
            vec![
                Call::Scroll { destination: ":1.100".into(), path: path(), delta: 120, orientation: Orientation::Vertical },
                Call::Scroll { destination: ":1.100".into(), path: path(), delta: -120, orientation: Orientation::Horizontal },
            ]
        );
    }

    #[test]
    fn gestures_on_a_gone_item_do_nothing() {
        let (mut h, mut out) = host(&[Fake::new(&[(Prop::Id, "nimbus"), MENU])]);
        let gone = ItemKey { owner: ":1.999".into(), path: "/StatusNotifierItem".into() };
        for g in [Gesture::Left, Gesture::Right, Gesture::Middle, Gesture::Scroll { delta: 120, orientation: Orientation::Vertical }] {
            assert_eq!(h.gesture(&gone, g, (1, 1), &mut out), GestureOutcome::Nothing, "{g:?}");
        }
        assert!(out.calls.is_empty() && out.sent.is_empty());
        let failed = Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into()));
        assert!(!h.opens_menu_after_activate(&gone, &failed), "gone while Activate was pending: nothing");
    }
}
