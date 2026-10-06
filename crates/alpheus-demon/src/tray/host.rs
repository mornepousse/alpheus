//! The StatusNotifierItem host, a pure state machine over `Outbox` (no
//! socket, no D-Bus library): which items exist, what the bar knows of each,
//! and the calls that keep it current — one `Get` per property (never
//! `GetAll`, never a pixmap), well-known names resolved once, items keyed by
//! (unique name, path), departures by `NameOwnerChanged` only, signals
//! coalesced to one re-read per frame, every bound applied.

use super::bus::{Call, CallError, CallId, Outbox, Property};
use super::codec::{Reply, Signal, clean};
use super::{DEFAULT_ITEM_PATH, Gesture, ITEM_INTERFACE, ITEM_NAME_PREFIXES, ItemKey, MAX_ITEMS, Prop, PropSet, PropValue, Status};
use crate::log::write as log;
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
    /// First Gets not answered yet (controller decision 8).
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

    /// Every first Get and the process id have answered, failed or timed out (controller decision 8;
    /// the process name is waited for too, so a fallback letter never changes once drawn).
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

    /// Stores `v` as `p`; whether anything changed. An empty text is absent. Compares only the one
    /// field a property maps to — never the whole item (no clone).
    fn apply(&mut self, p: Prop, v: PropValue) -> bool {
        fn set<T: PartialEq>(field: &mut T, value: T) -> bool {
            let changed = *field != value;
            *field = value;
            changed
        }
        let text = |v: PropValue| match v {
            PropValue::Text(t) if !t.is_empty() => Some(t),
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
            // behind them, so a right click sends ContextMenu instead of opening a menu (task 10).
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
    let (unique, rest) = match n.strip_prefix(':') {
        Some(r) => (true, r),
        None => (false, n),
    };
    let elements: Vec<&str> = rest.split('.').collect();
    n.len() <= 255
        && elements.len() >= 2
        && elements.iter().all(|e| {
            !e.is_empty()
                && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && (unique || !e.as_bytes()[0].is_ascii_digit())
        })
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
    /// Whether a self-registration refusal has already been logged once (task 5 fix round 2
    /// review): a looping signal from a misbehaving watcher — the same self-naming entry in its
    /// list on every `NameOwnerChanged`, say — must never flood the log.
    self_registration_logged: bool,
    /// Our own unique name: never one of the items we host (checked in `add`, on the resolved
    /// owner — task 5 review — so a registration naming our own well-known name, an entry of
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
        // Our own connection is never one of the items we host (task 4 review, moved here and
        // generalized in task 5's: `key.owner` is always the resolved unique name by this point,
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
        // "/NO_DBUSMENU") and has since gained a real one (fix round, 2026-10-06).
        if self.items.iter().any(|i| i.key == key) {
            self.reread_menu(&key, out);
            return;
        }
        if self.refused(0) {
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
            self.call(Call::RemoveMatch { rule: owner_rule(&item.key.owner) }, Pending::Rule, out);
            self.call(Call::RemoveMatch { rule: item_rule(&item.key.owner) }, Pending::Rule, out);
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
    /// Activate sent: its answer decides (an error reply with a Menu → open the menu, decision 5).
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

    /// Activate's answer (controller decision 5): a D-Bus error reply (Steam's
    /// libappindicator has no Activate: UnknownMethod) opens the item's menu when it
    /// has one; a timeout is not a failure (a slow application may have acted), a
    /// success needs nothing more.
    pub(crate) fn opens_menu_after_activate(&self, key: &ItemKey, result: &Result<Reply, CallError>) -> bool {
        matches!(result, Err(CallError::Remote(_))) && self.item(key).is_some_and(|i| i.menu.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::WATCHER_NAME;
    use crate::tray::bus::Method;
    use crate::tray::bus::testing::FakeOutbox;
    use std::fs;

    const NIMBUS: &str = "org.kde.StatusNotifierItem-4242-1";
    /// The daemon's own unique name, as `Host::new` would be given it — distinct from every
    /// sender used elsewhere in this module.
    const OWN: &str = ":1.0";

    /// A temporary /proc with one process, 4242, named "nimbus"; removed on drop.
    struct Proc(PathBuf);

    impl Drop for Proc {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn host(name: &str) -> (Host, FakeOutbox, Proc) {
        let root = std::env::temp_dir().join(format!("alpheus-proc-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("4242")).unwrap();
        fs::write(root.join("4242/comm"), "nimbus\n").unwrap();
        (Host::new(root.clone(), OWN.into()), FakeOutbox::default(), Proc(root))
    }

    /// A well-behaved KDE-like item's answers; the bus driver's for the rest.
    fn nimbus(call: &Call) -> Result<Reply, CallError> {
        Ok(match call {
            Call::Get { property: Property::Item(p), .. } => Reply::Prop(match p {
                Prop::Id => PropValue::Text("nimbus".into()),
                Prop::Title => PropValue::Text("Nimbus".into()),
                Prop::Status => PropValue::Status(Status::Active),
                Prop::IconName => PropValue::Text("nimbus-tray".into()),
                Prop::AttentionIconName => PropValue::Text(String::new()),
                Prop::ToolTip => PropValue::Text("Nimbus: 3 new".into()),
                Prop::Menu => PropValue::Path("/MenuBar".into()),
                Prop::ItemIsMenu => PropValue::Bool(false),
            }),
            Call::GetConnectionUnixProcessID { .. } => Reply::Pid(4242),
            Call::GetNameOwner { .. } => Reply::Owner(":1.7".into()),
            Call::ListNames => Reply::Names(vec![]),
            _ => Reply::Unit,
        })
    }

    /// Answers every recorded call with `f`, then the calls those answers made, until none is left;
    /// every call answered, in order.
    fn settle(h: &mut Host, out: &mut FakeOutbox, f: impl Fn(&Call) -> Result<Reply, CallError>) -> Vec<Call> {
        let mut all = Vec::new();
        loop {
            let calls = out.take();
            if calls.is_empty() {
                return all;
            }
            for (id, call) in calls {
                h.answer(id, f(&call), out);
                all.push(call);
            }
        }
    }

    fn gets(calls: &[Call]) -> Vec<(String, Prop)> {
        calls
            .iter()
            .filter_map(|c| match c {
                Call::Get { destination, property: Property::Item(p), .. } => Some((destination.clone(), *p)),
                _ => None,
            })
            .collect()
    }

    fn key(owner: &str) -> ItemKey {
        ItemKey { owner: owner.into(), path: DEFAULT_ITEM_PATH.into() }
    }

    fn changed(owner: &str, p: Prop) -> Signal {
        let mut props = PropSet::default();
        props.insert(p);
        Signal::ItemChanged { sender: owner.into(), path: DEFAULT_ITEM_PATH.into(), props }
    }

    fn gone(name: &str, old: &str) -> Signal {
        Signal::NameOwnerChanged { name: name.into(), old: old.into(), new: String::new() }
    }

    #[test]
    fn each_property_is_one_get_never_getall() {
        let (mut h, mut out, _proc) = host("one-get");
        h.register(":1.7", ":1.7", &mut out);
        let calls: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
        let props: Vec<Prop> = gets(&calls).into_iter().map(|(_, p)| p).collect();
        assert_eq!(props, Prop::ALL, "one Get per property, all sent at once");
        let methods: Vec<Method> = calls.iter().map(Call::method).collect();
        assert_eq!(methods.iter().filter(|m| **m == Method::GetConnectionUnixProcessID).count(), 1);
        assert_eq!(methods.iter().filter(|m| **m == Method::AddMatch).count(), 2);
        assert_eq!(methods.len(), 11, "nothing else: {methods:?}");
        assert!(!h.items()[0].is_ready(), "shown once the Gets answered");
    }

    #[test]
    fn every_application_call_goes_to_a_unique_name() {
        let (mut h, mut out, _proc) = host("unique");
        h.register(NIMBUS, ":1.7", &mut out);
        let mut calls = settle(&mut h, &mut out, nimbus);
        h.signal(&changed(":1.7", Prop::Title), &mut out);
        h.on_frame(&mut out);
        calls.extend(settle(&mut h, &mut out, nimbus));
        let to_apps = gets(&calls);
        assert_eq!(to_apps.len(), 9);
        assert!(to_apps.iter().all(|(d, _)| d == ":1.7"), "{to_apps:?}");
    }

    #[test]
    fn a_get_name_owner_answer_must_be_a_unique_name() {
        let (mut h, mut out, _proc) = host("owner-not-unique");
        h.register("org.freedesktop.DBus", "", &mut out);
        let id = out.take().into_iter().next().unwrap().0;
        // The bus driver owns itself literally: it answers GetNameOwner on its own name with that
        // very name, never a unique one (":1.N").
        h.answer(id, Ok(Reply::Owner("org.freedesktop.DBus".into())), &mut out);
        assert!(h.items().is_empty(), "never trusted as an owner");
        assert!(out.take().is_empty(), "no Get ever reaches the bus driver");
    }

    #[test]
    fn a_well_known_name_is_resolved_once_then_keyed_by_unique_name() {
        let (mut h, mut out, _proc) = host("resolved");
        h.register(NIMBUS, ":1.7", &mut out);
        h.register(NIMBUS, ":1.7", &mut out);
        assert_eq!(out.ids(Method::GetNameOwner).len(), 1, "one question in flight");
        settle(&mut h, &mut out, nimbus);
        let item = &h.items()[0];
        assert_eq!((&item.key, item.service.as_str()), (&key(":1.7"), NIMBUS));
        assert_eq!(item.watcher_entry(), format!("{NIMBUS}/StatusNotifierItem"));
        h.register(NIMBUS, ":1.7", &mut out);
        assert!(out.take().is_empty(), "resolved once: a re-registration asks nothing");
        assert_eq!(h.items().len(), 1);
    }

    #[test]
    fn a_second_alias_from_the_same_sender_skips_get_name_owner() {
        let (mut h, mut out, _proc) = host("alias-same-sender");
        h.register(NIMBUS, ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        assert_eq!(h.items().len(), 1);
        // A different well-known alias, but the same real sender: its owner is already known.
        h.register("org.kde.StatusNotifierItem-9999-1", ":1.7", &mut out);
        assert!(out.take().is_empty(), "already known by its unique name: no GetNameOwner");
        assert_eq!(h.items().len(), 1, "still the one item");
    }

    #[test]
    fn add_never_treats_our_own_unique_name_as_an_item() {
        let (mut h, mut out, _proc) = host("self-unique");
        // The bare-unique-name form of `register` calls `add` directly, with no GetNameOwner
        // round trip: this is what a RegisterStatusNotifierItem naming us literally, or an
        // entry already keyed by a unique name in another watcher's list or signal, looks like.
        // `sender` is irrelevant on this path — `add`'s guard checks `key.owner` (the name
        // itself), never `sender` — so an arbitrary, unrelated value proves that here on purpose
        // (task 5 fix round 2 review: a prior version of this comment read as if the sender were
        // part of what gets checked).
        h.register(OWN, "irrelevant-sender", &mut out);
        assert!(h.items().is_empty());
        assert!(out.take().is_empty(), "nothing asked of our own connection");
    }

    #[test]
    fn add_never_treats_a_well_known_name_resolving_to_us_as_an_item() {
        let (mut h, mut out, _proc) = host("self-well-known");
        // A registration naming our own well-known name (task 5 review): `split_service` alone
        // cannot see this is us, only the GetNameOwner round trip it triggers can — the same
        // round trip `register` takes for any well-known name, so this also covers an entry of
        // another watcher's list or its ItemRegistered signal (both call `register` the same
        // way, sender ""), not only a direct call to our own watcher object.
        h.register(WATCHER_NAME, "", &mut out);
        let id = out.ids(Method::GetNameOwner).into_iter().next().unwrap();
        out.take(); // the GetNameOwner call itself; only what `add` would ask next matters below
        h.answer(id, Ok(Reply::Owner(OWN.into())), &mut out);
        assert!(h.items().is_empty());
        assert!(out.take().is_empty(), "no Get ever reaches our own connection");
    }

    #[test]
    fn registration_forms() {
        let split = |s: &str, sender: &str| split_service(s, sender);
        let pair = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
        assert_eq!(split(NIMBUS, ":1.7"), pair(NIMBUS, "/StatusNotifierItem"));
        assert_eq!(split("/org/ayatana/NotificationItem/quill", ":1.8"), pair(":1.8", "/org/ayatana/NotificationItem/quill"));
        assert_eq!(
            split("org.kde.StatusNotifierItem-4343-1/StatusNotifierItem", ":1.9"),
            pair("org.kde.StatusNotifierItem-4343-1", "/StatusNotifierItem")
        );
        assert_eq!(split(":1.10", ":1.10"), pair(":1.10", "/StatusNotifierItem"));
        assert_eq!(split("org.telegram.desktop", ":1.11"), pair("org.telegram.desktop", "/StatusNotifierItem"), "dots are not paths");
        assert_eq!(
            split(":1.12/org/ayatana/NotificationItem/x_1", ""),
            pair(":1.12", "/org/ayatana/NotificationItem/x_1"),
            "the other watcher's entry"
        );
        for bad in ["", "/x", "nodots", "1org.x", "org..x", ":1.7 x", "org.x/a//b", "org.x/a/", "org.x/a b", "/a\u{7}b"] {
            assert_eq!(split(bad, ""), None, "{bad:?}");
        }
        assert_eq!(split("/bad path", ":1.7"), None);
    }

    #[test]
    fn duplicate_registration_is_one_item() {
        let (mut h, mut out, _proc) = host("duplicate");
        h.register(":1.7", ":1.7", &mut out);
        h.register(":1.7", ":1.7", &mut out);
        h.register("/StatusNotifierItem", ":1.7", &mut out);
        h.register(":1.7/StatusNotifierItem", "", &mut out);
        assert_eq!(gets(&out.take().into_iter().map(|(_, c)| c).collect::<Vec<_>>()).len(), 8);
        assert_eq!(h.items().len(), 1);
        assert_eq!(h.take_changes().registered, [":1.7/StatusNotifierItem"]);
    }

    #[test]
    fn a_duplicate_registration_rereads_the_menu() {
        // No StatusNotifierItem signal exists for Menu: an item that first registered with none
        // (or "/NO_DBUSMENU") and gains one later is only ever noticed through a duplicate
        // RegisterStatusNotifierItem (fix round, 2026-10-06).
        let (mut h, mut out, _proc) = host("reread-menu");
        h.register(":1.7", ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        assert_eq!(h.items()[0].menu.as_deref(), Some("/MenuBar"), "the fake's first answer");
        h.register(":1.7", ":1.7", &mut out);
        let calls: Vec<(CallId, Call)> = out.take();
        let gets: Vec<Prop> = gets(&calls.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>()).into_iter().map(|(_, p)| p).collect();
        assert_eq!(gets, [Prop::Menu], "one Get, Menu only: {gets:?}");
        // A second duplicate while the first Get is still in flight asks nothing more.
        h.register(":1.7", ":1.7", &mut out);
        assert!(out.take().is_empty(), "already asking: no second Get");
        let (id, _) = calls[0].clone();
        h.answer(id, Ok(Reply::Prop(PropValue::Path("/NewMenu".into()))), &mut out);
        assert_eq!(h.items()[0].menu.as_deref(), Some("/NewMenu"), "the gained menu is applied");
    }

    #[test]
    fn departure_by_name_owner_changed_only() {
        let (mut h, mut out, _proc) = host("departure");
        h.register(":1.7", ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        h.take_changes();
        h.signal(&Signal::NameOwnerChanged { name: ":1.7".into(), old: String::new(), new: ":1.7".into() }, &mut out);
        h.signal(&Signal::ItemUnregistered(":1.7/StatusNotifierItem".into()), &mut out);
        h.signal(&changed(":1.7", Prop::Title), &mut out);
        h.on_frame(&mut out);
        settle(&mut h, &mut out, |_| Err(CallError::Timeout));
        assert_eq!(h.items().len(), 1, "only an owner leaving removes an item");
        h.signal(&gone(":1.7", ":1.7"), &mut out);
        assert!(h.items().is_empty());
        let c = h.take_changes();
        assert!(c.redraw);
        assert_eq!(c.unregistered, [":1.7/StatusNotifierItem"]);
    }

    #[test]
    fn a_well_known_owner_leaving_removes_its_item_once() {
        let (mut h, mut out, _proc) = host("well-known-leaves");
        h.register(NIMBUS, ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        h.take_changes();
        h.signal(&gone(NIMBUS, ":1.7"), &mut out);
        assert_eq!(h.items().len(), 1, "the well-known name keys nothing");
        h.signal(&gone(":1.7", ":1.7"), &mut out);
        assert!(h.items().is_empty());
        assert_eq!(h.take_changes().unregistered, [format!("{NIMBUS}/StatusNotifierItem")], "once");
    }

    #[test]
    fn per_item_match_rules_added_and_removed() {
        let (mut h, mut out, _proc) = host("rules");
        h.register("/a", ":1.7", &mut out);
        h.register("/b", ":1.7", &mut out);
        h.register(":1.8", ":1.8", &mut out);
        let rules = |out: &mut FakeOutbox, m: Method| -> Vec<String> {
            out.take()
                .into_iter()
                .filter(|(_, c)| c.method() == m)
                .map(|(_, c)| match c {
                    Call::AddMatch { rule } | Call::RemoveMatch { rule } => rule,
                    _ => unreachable!(),
                })
                .collect()
        };
        let added = rules(&mut out, Method::AddMatch);
        assert_eq!(added, [owner_rule(":1.7"), item_rule(":1.7"), owner_rule(":1.8"), item_rule(":1.8")], "one pair per owner");
        assert_eq!(
            owner_rule(":1.7"),
            "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0=':1.7'"
        );
        assert_eq!(item_rule(":1.7"), "type='signal',sender=':1.7',interface='org.kde.StatusNotifierItem'");
        h.signal(&gone(":1.7", ":1.7"), &mut out);
        assert_eq!(rules(&mut out, Method::RemoveMatch), [owner_rule(":1.7"), item_rule(":1.7")], "removed with the owner's last item");
        assert_eq!(h.items().len(), 1);
    }

    #[test]
    fn a_burst_marks_dirty_and_rereads_once_per_frame() {
        let (mut h, mut out, _proc) = host("burst");
        h.register(":1.7", ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        assert!(!h.wants_frame());
        for _ in 0..50 {
            h.signal(&changed(":1.7", Prop::IconName), &mut out);
        }
        assert!(out.take().is_empty(), "a signal sends nothing by itself");
        assert!(h.wants_frame());
        h.on_frame(&mut out);
        let reread = out.take();
        assert_eq!(gets(&reread.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>()), [(":1.7".to_string(), Prop::IconName)]);
        assert!(!h.wants_frame(), "its Get is in flight");
        for _ in 0..50 {
            h.signal(&changed(":1.7", Prop::IconName), &mut out);
        }
        h.on_frame(&mut out);
        assert!(out.take().is_empty(), "never two Gets of one property in flight");
        h.answer(reread[0].0, nimbus(&reread[0].1), &mut out);
        assert!(h.wants_frame(), "named again meanwhile: one more read at the next frame");
        h.on_frame(&mut out);
        assert_eq!(out.take().len(), 1);
    }

    #[test]
    fn new_status_carries_its_value_no_call() {
        let (mut h, mut out, _proc) = host("status");
        h.register(":1.7", ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        h.take_changes();
        h.signal(&Signal::ItemStatus { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), status: Status::NeedsAttention }, &mut out);
        assert_eq!(h.items()[0].status, Status::NeedsAttention);
        assert!(h.take_changes().redraw);
        assert!(out.take().is_empty() && !h.wants_frame(), "no call, no frame");
        h.signal(&Signal::ItemStatus { sender: ":1.9".into(), path: DEFAULT_ITEM_PATH.into(), status: Status::Passive }, &mut out);
        assert_eq!(h.items()[0].status, Status::NeedsAttention, "another sender's signal changes nothing");
    }

    #[test]
    fn failed_properties_are_not_asked_again_until_a_signal_names_them() {
        let (mut h, mut out, _proc) = host("failed");
        h.register(":1.7", ":1.7", &mut out);
        let no_icon = |c: &Call| match c {
            Call::Get { property: Property::Item(Prop::IconName | Prop::ToolTip), .. } => {
                Err(CallError::Remote("org.freedesktop.DBus.Error.Failed".into()))
            }
            other => nimbus(other),
        };
        settle(&mut h, &mut out, no_icon);
        assert!(h.items()[0].is_ready() && h.items()[0].icon_name.is_none());
        h.on_frame(&mut out);
        assert!(out.take().is_empty() && !h.wants_frame(), "failed: not asked again unprompted");
        h.signal(&changed(":1.7", Prop::ToolTip), &mut out);
        h.on_frame(&mut out);
        let calls: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
        assert_eq!(gets(&calls), [(":1.7".to_string(), Prop::ToolTip)], "only what the signal named");
    }

    #[test]
    fn a_silent_item_is_shown_after_its_gets_time_out() {
        let (mut h, mut out, _proc) = host("silent");
        h.register(":1.7", ":1.7", &mut out);
        let mut calls = out.take();
        for (id, call) in calls.drain(..) {
            let r = if matches!(call, Call::Get { .. }) { Err(CallError::Timeout) } else { nimbus(&call) };
            h.answer(id, r, &mut out);
        }
        let item = &h.items()[0];
        assert!(item.is_ready(), "shown, with its letter");
        assert_eq!((item.title.as_deref(), item.process.as_deref()), (None, Some("nimbus")));
        assert!(h.take_changes().redraw);
        h.on_frame(&mut out);
        assert!(out.take().is_empty(), "nothing asked again until a signal");
        h.signal(&changed(":1.7", Prop::Title), &mut out);
        h.on_frame(&mut out);
        assert_eq!(out.take().len(), 1);
    }

    #[test]
    fn an_item_whose_every_get_errors_is_dropped() {
        let (mut h, mut out, _proc) = host("bogus");
        h.register("/no/such/object", ":1.7", &mut out);
        let calls = settle(&mut h, &mut out, |c| match c {
            Call::Get { .. } => Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownObject".into())),
            other => nimbus(other),
        });
        assert!(h.items().is_empty(), "never shown");
        assert!(calls.iter().any(|c| c.method() == Method::RemoveMatch), "its rules go with it");
        let c = h.take_changes();
        assert!(!c.redraw, "it was never drawn");
        assert_eq!(c.unregistered, [":1.7/no/such/object"]);
        h.register(":1.8", ":1.8", &mut out);
        settle(&mut h, &mut out, |c| match c {
            Call::Get { property: Property::Item(Prop::Id), .. } => Err(CallError::Timeout),
            Call::Get { .. } => Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into())),
            other => nimbus(other),
        });
        assert_eq!(h.items().len(), 1, "one timeout: maybe slow, kept with its letter");
    }

    #[test]
    fn a_bogus_sibling_is_dropped_but_its_owners_rules_stay() {
        let (mut h, mut out, _proc) = host("bogus-sibling");
        h.register("/a", ":1.7", &mut out);
        h.register("/b", ":1.7", &mut out);
        let calls = settle(&mut h, &mut out, |c| match c {
            Call::Get { path, .. } if path == "/b" => Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownObject".into())),
            other => nimbus(other),
        });
        assert_eq!(h.items().len(), 1, "/b dropped, /a stays");
        assert_eq!(h.items()[0].key.path, "/a");
        assert!(!calls.iter().any(|c| c.method() == Method::RemoveMatch), "the owner still has /a: rules stay");
    }

    #[test]
    fn the_65th_item_is_refused() {
        let (mut h, mut out, _proc) = host("bounds");
        for n in 1..=MAX_ITEMS + 1 {
            h.register(&format!(":1.{n}"), "", &mut out);
        }
        assert_eq!(h.items().len(), MAX_ITEMS);
        let calls: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
        assert!(!gets(&calls).iter().any(|(d, _)| *d == format!(":1.{}", MAX_ITEMS + 1)), "nothing asked of the 65th");
        h.signal(&gone(":1.1", ":1.1"), &mut out);
        h.register(&format!(":1.{}", MAX_ITEMS + 1), "", &mut out);
        assert_eq!(h.items().len(), MAX_ITEMS, "room again: it may come in");
        assert_eq!(h.items().last().unwrap().key.owner, format!(":1.{}", MAX_ITEMS + 1));
        let (mut h, mut out, _proc) = host("bounds-names");
        for n in 1..=MAX_ITEMS + 1 {
            h.register(&format!("org.kde.StatusNotifierItem-{n}-1"), "", &mut out);
        }
        assert_eq!(out.ids(Method::GetNameOwner).len(), MAX_ITEMS, "never more names resolving than room");
    }

    #[test]
    fn inventory_probes_sni_names_only() {
        let (mut h, mut out, _proc) = host("inventory");
        h.start(&mut out);
        let names = [
            "org.freedesktop.DBus",
            ":1.3",
            "org.kde.StatusNotifierItem-12-1",
            "org.freedesktop.StatusNotifierItem-13-1",
            "org.kde.StatusNotifierWatcher",
            "org.kde.kdeconnect",
        ];
        let calls = settle(&mut h, &mut out, |c| match c {
            Call::ListNames => Ok(Reply::Names(names.map(String::from).to_vec())),
            Call::GetNameOwner { name } if name.ends_with("12-1") => Ok(Reply::Owner(":1.12".into())),
            Call::GetNameOwner { .. } => Ok(Reply::Owner(":1.13".into())),
            other => nimbus(other),
        });
        let asked: Vec<&Call> = calls.iter().filter(|c| c.method() == Method::GetNameOwner).collect();
        assert_eq!(
            asked,
            [
                &Call::GetNameOwner { name: "org.kde.StatusNotifierItem-12-1".into() },
                &Call::GetNameOwner { name: "org.freedesktop.StatusNotifierItem-13-1".into() }
            ]
        );
        let keys: Vec<&ItemKey> = h.items().iter().map(|i| &i.key).collect();
        assert_eq!(keys, [&key(":1.12"), &key(":1.13")], "at the default path");
    }

    #[test]
    fn process_name_from_proc_comm() {
        let (mut h, mut out, _proc) = host("comm");
        h.register(":1.7", ":1.7", &mut out);
        h.register(":1.8", ":1.8", &mut out);
        settle(&mut h, &mut out, |c| match c {
            Call::GetConnectionUnixProcessID { name } if name == ":1.8" => Ok(Reply::Pid(999_999)),
            other => nimbus(other),
        });
        assert_eq!(h.item(&key(":1.7")).unwrap().process.as_deref(), Some("nimbus"));
        assert_eq!(h.item(&key(":1.8")).unwrap().process, None, "no such process: none");
    }

    #[test]
    fn a_non_utf8_comm_still_yields_a_letter() {
        let (mut h, mut out, proc) = host("non-utf8-comm");
        fs::write(proc.0.join("4242/comm"), b"ni\xffmbus\n").unwrap();
        h.register(":1.7", ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        let process = h.item(&key(":1.7")).unwrap().process.clone();
        assert!(process.as_deref().is_some_and(|p| p.starts_with("ni")), "{process:?}");
    }

    #[test]
    fn an_item_waits_for_its_process_name_before_it_is_shown() {
        let (mut h, mut out, _proc) = host("pid-last");
        h.register(":1.7", ":1.7", &mut out);
        let (pid, rest): (Vec<_>, Vec<_>) = out.take().into_iter().partition(|(_, c)| c.method() == Method::GetConnectionUnixProcessID);
        for (id, call) in rest {
            h.answer(id, nimbus(&call), &mut out);
        }
        assert!(!h.items()[0].is_ready(), "its fallback letter may still change: not shown yet");
        assert!(!h.take_changes().redraw);
        h.answer(pid[0].0, Err(CallError::Timeout), &mut out);
        assert!(h.items()[0].is_ready(), "a process id that never comes holds nothing back");
        assert!(h.take_changes().redraw);
    }

    #[test]
    fn a_menu_path_meaning_none_is_no_menu() {
        let mut i = Item::new(key(":1.7"), ":1.7".into());
        for none in ["/", "/NO_DBUSMENU"] {
            i.apply(Prop::Menu, PropValue::Path(none.into()));
            assert_eq!(i.menu, None, "{none}: no dbusmenu behind it");
        }
        assert!(i.apply(Prop::Menu, PropValue::Path("/MenuBar".into())));
        assert_eq!(i.menu.as_deref(), Some("/MenuBar"));
        assert!(i.apply(Prop::Menu, PropValue::Path("/NO_DBUSMENU".into())), "a menu withdrawn");
        assert_eq!(i.menu, None);
    }

    #[test]
    fn the_host_calls_only_its_part_of_the_allowlist() {
        let (mut h, mut out, _proc) = host("allowlist");
        h.start(&mut out);
        h.register(NIMBUS, ":1.7", &mut out);
        h.register("/org/ayatana/NotificationItem/quill", ":1.8", &mut out);
        let mut calls = settle(&mut h, &mut out, nimbus);
        h.signal(&changed(":1.7", Prop::ToolTip), &mut out);
        h.on_frame(&mut out);
        h.signal(&gone(":1.8", ":1.8"), &mut out);
        h.unregister(&format!("{NIMBUS}/StatusNotifierItem"), &mut out);
        calls.extend(settle(&mut h, &mut out, nimbus));
        assert!(h.items().is_empty());
        let allowed = [
            Method::Get,
            Method::GetNameOwner,
            Method::GetConnectionUnixProcessID,
            Method::ListNames,
            Method::AddMatch,
            Method::RemoveMatch,
        ];
        for c in &calls {
            assert!(allowed.contains(&c.method()), "{c:?}");
        }
        assert!(out.sent.is_empty(), "the host sends nothing without an answer");
    }

    #[test]
    fn clear_forgets_everything() {
        let (mut h, mut out, _proc) = host("clear");
        h.register(":1.7", ":1.7", &mut out);
        settle(&mut h, &mut out, nimbus);
        h.take_changes();
        h.register(":1.8", ":1.8", &mut out);
        let in_flight = out.take().first().unwrap().0;
        let c = h.clear();
        assert!(c.redraw && h.items().is_empty());
        assert_eq!(c.unregistered, [":1.7/StatusNotifierItem", ":1.8/StatusNotifierItem"]);
        assert!(!h.owns(in_flight), "a call in flight when the bus closed is forgotten too");
    }

    #[test]
    fn tooltip_text_falls_back_to_title_then_id() {
        let mut i = Item::new(key(":1.7"), ":1.7".into());
        assert_eq!(i.tooltip_text(), None);
        i.id = Some("chrome_status_icon_1".into());
        assert_eq!(i.tooltip_text(), Some("chrome_status_icon_1"));
        i.title = Some("Lumen".into());
        assert_eq!(i.tooltip_text(), Some("Lumen"));
        i.tooltip = Some("Lumen: 2 unread".into());
        assert_eq!(i.tooltip_text(), Some("Lumen: 2 unread"));
    }
}

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
        // Qt's QSystemTrayIcon without a context menu says Menu = "/NO_DBUSMENU" (task 4: no menu).
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
