//! The tray as one object for the wiring (`Tray`): it owns the session
//! connection, the watcher and the host, and routes everything the bus
//! delivers — answers to whoever asked (the host, or the router's own calls
//! for the watcher's name), signals to the host or the name-takeover logic,
//! incoming calls to the watcher or refused. The protocol around the name
//! (spec § 1): try to own `org.kde.StatusNotifierWatcher` without replacing
//! anyone; if another watcher has it, be a host only and follow its list;
//! when it leaves, take the name.

use super::bus::{Call, CallError, CallId, Incoming, Outbox, Property, SessionBus};
use super::codec::{self, Reply, Signal};
use super::host::{Host, Item, owner_rule, split_service};
use super::watcher::Watcher;
use super::{ItemKey, MAX_ITEMS, WATCHER_INTERFACE, WATCHER_NAME, WATCHER_PATH};
use crate::log::write as log;
use dbus::Message;
use std::collections::HashMap;
use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// RequestName sent, no answer yet.
    Starting,
    /// We own WATCHER_NAME: watcher and host.
    Watcher,
    /// Another watcher (its unique name) owns it: host only, following its list and signals.
    HostOnly { watcher: String },
}

/// What the wiring must do after a tray callback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayOutput {
    /// The items changed: rebuild Msg::Tray and redraw the bars.
    pub redraw: bool,
    /// Open this item's menu (right click, ItemIsMenu, a failed Activate — task 10; acted on in task 17).
    pub open_menu: Option<ItemKey>,
    /// The open menu's layout changed (arrived, grafted, patched — task 6): repaint it (task 17).
    pub menu_changed: bool,
    /// The open menu must close (2 s without an answer, its item gone — task 6).
    pub close_menu: bool,
    /// The session bus closed: the tray is empty for good (the wiring logs once and drops the Tray).
    pub bus_closed: bool,
}

impl TrayOutput {
    pub fn merge(&mut self, other: TrayOutput) {
        self.redraw |= other.redraw;
        if other.open_menu.is_some() {
            self.open_menu = other.open_menu;
        }
        self.menu_changed |= other.menu_changed;
        self.close_menu |= other.close_menu;
        self.bus_closed |= other.bus_closed;
    }
}

/// The router's own calls, around the watcher's name. Not `Copy`: task 10 adds
/// `Activate(ItemKey)` (a gesture's answer) and its arm in `answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Own {
    RequestName,
    /// Who owns the name: it was taken (`retry`: ask for it again if nobody does), or our
    /// RequestName failed (`!retry`: never loop on a bus that refuses it).
    WatcherOwner {
        retry: bool,
    },
    /// The other watcher's RegisteredStatusNotifierItems.
    WatcherItems,
    /// AddMatch, RemoveMatch, RegisterStatusNotifierHost: nothing to read.
    Ignore,
}

/// The other watcher's signals, while we are a host only.
fn watcher_rule(watcher: &str) -> String {
    format!("type='signal',sender='{watcher}',interface='{WATCHER_INTERFACE}'")
}

pub struct Tray {
    bus: SessionBus,
    watcher: Watcher,
    host: Host,
    own: HashMap<CallId, Own>,
    role: Role,
    /// A RequestName is in flight (never two).
    requesting: bool,
}

impl Tray {
    /// Starts on `bus`: RequestName (DO_NOT_QUEUE), the narrowed NameOwnerChanged rule for WATCHER_NAME,
    /// the host's inventory. Never blocks.
    pub fn start(bus: SessionBus, proc_root: PathBuf) -> Tray {
        let mut t =
            Tray { bus, watcher: Watcher::new(), host: Host::new(proc_root), own: HashMap::new(), role: Role::Starting, requesting: false };
        t.call(Call::AddMatch { rule: owner_rule(WATCHER_NAME) }, Own::Ignore);
        t.request_name();
        t.host.start(&mut t.bus);
        t.bus.flush();
        t
    }

    fn call(&mut self, call: Call, own: Own) {
        let id = self.bus.call(call);
        self.own.insert(id, own);
    }

    fn request_name(&mut self) {
        if !self.requesting {
            self.requesting = true;
            self.role = Role::Starting;
            self.call(Call::RequestName, Own::RequestName);
        }
    }

    pub fn watch_fd(&mut self) -> io::Result<OwnedFd> {
        self.bus.watch_fd()
    }

    /// The fd is readable: everything queued is routed (answers by owner, signals to host / watcher logic
    /// / menu, incoming calls to the watcher or refused), then flushed.
    pub fn on_readable(&mut self) -> TrayOutput {
        let incoming = self.bus.read();
        self.route(incoming)
    }

    /// The deadline timer fired: expired calls answered with Timeout and routed.
    pub fn on_deadline(&mut self, now: Instant) -> TrayOutput {
        let incoming = self.bus.expire(now);
        self.route(incoming)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.bus.next_deadline()
    }

    /// A frame callback: the host's dirty properties re-read.
    pub fn on_frame(&mut self) -> TrayOutput {
        self.host.on_frame(&mut self.bus);
        self.bus.flush();
        TrayOutput::default()
    }

    pub fn wants_frame(&self) -> bool {
        self.host.wants_frame()
    }

    pub fn items(&self) -> &[Item] {
        self.host.items()
    }

    pub fn item(&self, key: &ItemKey) -> Option<&Item> {
        self.host.item(key)
    }

    pub fn role(&self) -> &Role {
        &self.role
    }

    fn route(&mut self, incoming: Vec<Incoming>) -> TrayOutput {
        let mut out = TrayOutput::default();
        for i in incoming {
            match i {
                Incoming::Answer { id, call, result } => {
                    let result = result.and_then(|m| codec::reply(&call, &m));
                    out.merge(self.answer(id, result));
                }
                Incoming::Signal(m) => out.merge(self.signal(&m)),
                Incoming::MethodCall(m) => self.inbound_call(m),
                Incoming::Closed => {
                    let changes = self.host.clear();
                    self.watcher.set_serving(false);
                    out.redraw |= changes.redraw;
                    out.bus_closed = true;
                }
            }
        }
        let changes = self.host.take_changes();
        out.redraw |= changes.redraw;
        for m in self.watcher.publish(&changes) {
            self.bus.send_message(m);
        }
        self.bus.flush();
        out
    }

    fn answer(&mut self, id: CallId, result: Result<Reply, CallError>) -> TrayOutput {
        if self.host.owns(id) {
            self.host.answer(id, result, &mut self.bus);
            return TrayOutput::default();
        }
        match (self.own.remove(&id), result) {
            (Some(Own::RequestName), r) => {
                self.requesting = false;
                match r {
                    // 1: primary owner; 4: already ours.
                    Ok(Reply::RequestName(1 | 4)) => self.become_watcher(),
                    // 3: exists (DO_NOT_QUEUE never queues us): host only.
                    Ok(Reply::RequestName(_)) => {
                        self.call(Call::GetNameOwner { name: WATCHER_NAME.into() }, Own::WatcherOwner { retry: true })
                    }
                    // Refused or unanswered: still follow a watcher if there is one.
                    Err(e) => {
                        log(&format!("tray: the watcher's name could not be requested ({e:?})"));
                        self.call(Call::GetNameOwner { name: WATCHER_NAME.into() }, Own::WatcherOwner { retry: false });
                    }
                    Ok(_) => {}
                }
            }
            // A RequestName that timed out may still have made the name ours.
            (Some(Own::WatcherOwner { .. }), Ok(Reply::Owner(watcher))) if watcher == self.bus.unique_name() => self.become_watcher(),
            (Some(Own::WatcherOwner { .. }), Ok(Reply::Owner(watcher))) => self.follow(watcher),
            // The other watcher left between our two questions: try again.
            (Some(Own::WatcherOwner { retry: true }), _) => self.request_name(),
            (Some(Own::WatcherItems), Ok(Reply::Names(entries))) => {
                // The other watcher's list, bounded like our own.
                for entry in entries.into_iter().take(MAX_ITEMS) {
                    self.host.register(&entry, "", &mut self.bus);
                }
            }
            _ => {}
        }
        TrayOutput::default()
    }

    fn become_watcher(&mut self) {
        self.role = Role::Watcher;
        self.watcher.set_serving(true);
        let signal = self.watcher.host_registered();
        self.bus.send_message(signal);
    }

    /// Host only on `watcher`: its signals first (none missed between them and its list), then us as
    /// a host on it, then its list.
    fn follow(&mut self, watcher: String) {
        self.call(Call::AddMatch { rule: watcher_rule(&watcher) }, Own::Ignore);
        let me = self.bus.unique_name().to_string();
        self.call(Call::RegisterStatusNotifierHost { destination: watcher.clone(), host: me }, Own::Ignore);
        let items = Call::Get { destination: watcher.clone(), path: WATCHER_PATH.into(), property: Property::RegisteredItems };
        self.call(items, Own::WatcherItems);
        self.role = Role::HostOnly { watcher };
    }

    fn signal(&mut self, m: &Message) -> TrayOutput {
        let s = codec::signal(m);
        let sender = m.sender().map(|s| s.to_string()).unwrap_or_default();
        match &s {
            Signal::NameOwnerChanged { name, new, .. } if name == WATCHER_NAME => match self.role.clone() {
                Role::Watcher => {}
                // Takeover (spec § 1): the other watcher left; applications re-register with us.
                Role::HostOnly { watcher } if new.is_empty() => {
                    self.call(Call::RemoveMatch { rule: watcher_rule(&watcher) }, Own::Ignore);
                    self.request_name();
                }
                // Another watcher took it over directly (the old one allowed replacement): follow it.
                Role::HostOnly { watcher } if *new != watcher && new != self.bus.unique_name() => {
                    self.call(Call::RemoveMatch { rule: watcher_rule(&watcher) }, Own::Ignore);
                    self.follow(new.clone());
                }
                Role::HostOnly { .. } => {}
                Role::Starting if new.is_empty() => self.request_name(),
                Role::Starting => {}
            },
            Signal::NameOwnerChanged { .. } | Signal::ItemChanged { .. } | Signal::ItemStatus { .. } => self.host.signal(&s, &mut self.bus),
            Signal::ItemRegistered(service) if matches!(&self.role, Role::HostOnly { watcher } if *watcher == sender) => {
                self.host.register(service, "", &mut self.bus)
            }
            Signal::ItemUnregistered(service) if matches!(&self.role, Role::HostOnly { watcher } if *watcher == sender) => {
                self.host.unregister(service, &mut self.bus)
            }
            _ => {}
        }
        TrayOutput::default()
    }

    fn inbound_call(&mut self, m: Message) {
        // Served while we own the name; refused (never left unanswered) otherwise.
        for r in self.watcher.handle(m) {
            self.bus.send_message(r);
        }
        // A registration naming our own unique name as its service (bare, or as the name half of
        // a "name/path" pair) is never an application: refused, never reaches the host (task 4
        // review — our own connection is never one of the items we host).
        let own = self.bus.unique_name().to_string();
        for (service, sender) in self.watcher.take_registrations() {
            match split_service(&service, &sender) {
                Some((name, _)) if name == own => {
                    log(&format!("tray: refusing a registration naming our own unique name ({service:?})"));
                }
                _ => self.host.register(&service, &sender, &mut self.bus),
            }
        }
    }
}
