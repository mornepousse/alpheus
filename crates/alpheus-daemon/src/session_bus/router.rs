//! The tray as one object for the wiring (`Tray`): it owns the session
//! connection, the watcher and the host, and routes everything the bus
//! delivers — answers to whoever asked (the host, or the router's own calls
//! for the watcher's name), signals to the host or the name-takeover logic,
//! incoming calls to the watcher or refused. The protocol around the name
//! (spec § 1): try to own `org.kde.StatusNotifierWatcher` without replacing
//! anyone; if another watcher has it, be a host only and follow its list;
//! when it leaves, take the name.

use super::bus::{Call, CallError, CallId, Incoming, Outbox, Property, SessionBus, refusal};
use super::codec::{self, Reply, Signal};
use super::host::{GestureOutcome, Host, Item, owner_rule};
use super::menu_session::{MenuSession, MenuUpdate};
use super::mpris::Mpris;
use super::notifications::{self, Notifications};
use super::watcher::Watcher;
use super::{Gesture, ItemKey, MAX_ITEMS, WATCHER_INTERFACE, WATCHER_NAME, WATCHER_PATH};
use crate::appearance::Appearance;
use crate::config::{MediaConfig, NotificationsConfig};
use crate::failed::app::UserUnits;
use crate::log::write as log;
use crate::media::MediaView;
use crate::notify::NotifyView;
use alpheus_common::theme::Mode;
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
    /// Open this item's menu (right click, ItemIsMenu, a failed Activate).
    pub open_menu: Option<ItemKey>,
    /// The open menu's layout changed (arrived, grafted, patched): repaint it.
    pub menu_changed: bool,
    /// The open menu must close (2 s without an answer, its item gone).
    pub close_menu: bool,
    /// `close_menu` is for its item's `Menu` path moving under it (a duplicate registration),
    /// never a real timeout — the wiring must not log it as one.
    pub menu_path_changed: bool,
    /// The session bus closed: the tray is empty for good (the wiring logs once and drops the Tray).
    pub bus_closed: bool,
    /// The portal's color-scheme changed (`mode = "auto"`): the wiring re-resolves.
    pub appearance: Option<Mode>,
    /// The media module's view changed: Some(None) hides it, Some(Some(v)) shows v.
    pub media: Option<Option<MediaView>>,
    /// The notifications server's view changed: the bell repaints.
    pub notifications: Option<NotifyView>,
    /// The bubbles to show changed (shown, replaced, expired, all gone): the wiring re-syncs them.
    pub bubbles: bool,
    /// The user manager's failed-units count changed: Some(None) is unknown.
    pub user_units: Option<Option<u32>>,
}

impl TrayOutput {
    pub fn merge(&mut self, other: TrayOutput) {
        self.redraw |= other.redraw;
        if other.open_menu.is_some() {
            self.open_menu = other.open_menu;
        }
        self.menu_changed |= other.menu_changed;
        self.close_menu |= other.close_menu;
        self.menu_path_changed |= other.menu_path_changed;
        self.bus_closed |= other.bus_closed;
        if other.appearance.is_some() {
            self.appearance = other.appearance;
        }
        if other.media.is_some() {
            self.media = other.media;
        }
        if other.notifications.is_some() {
            self.notifications = other.notifications;
        }
        self.bubbles |= other.bubbles;
        if other.user_units.is_some() {
            self.user_units = other.user_units;
        }
    }
}

/// The router's own calls, around the watcher's name. Not `Copy`: `Activate` carries
/// the gesture's `ItemKey`.
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
    /// An Activate sent by `gesture`: its answer may open the item's menu
    Activate(ItemKey),
}

/// The other watcher's signals, while we are a host only.
fn watcher_rule(watcher: &str) -> String {
    format!("type='signal',sender='{watcher}',interface='{WATCHER_INTERFACE}'")
}

pub struct Tray {
    bus: SessionBus,
    watcher: Watcher,
    host: Host,
    /// The open menu (one at a time).
    menu: Option<MenuSession>,
    own: HashMap<CallId, Own>,
    role: Role,
    /// A RequestName is in flight (never two).
    requesting: bool,
    /// What answers drained by a flush made happen during a call that returns no `TrayOutput`
    /// (`open_menu`, `menu_*`, `close_menu`): merged into the next output, or taken with
    /// `take_deferred`.
    deferred: TrayOutput,
    /// The portal's color-scheme, followed on this same connection; None unless asked.
    appearance: Option<Appearance>,
    /// The MPRIS consumer on this same connection; None unless asked.
    media: Option<Mpris>,
    /// The notifications server on this same connection; None unless asked.
    notifications: Option<Notifications>,
    /// The user manager's failed-units count on this same connection; None unless asked.
    failed: Option<UserUnits>,
}

/// How many times a flush's drained answers are routed (and what that sends flushed again) before
/// the rest waits for the next read: a bound, so a chatty application cannot spin the loop.
const SETTLE_ROUNDS: usize = 8;

impl Tray {
    /// Starts on `bus`: RequestName (DO_NOT_QUEUE), the narrowed NameOwnerChanged rule for WATCHER_NAME,
    /// the host's inventory. Never blocks.
    pub fn start(bus: SessionBus, proc_root: PathBuf) -> Tray {
        let unique = bus.unique_name().to_string();
        let mut t = Tray {
            bus,
            watcher: Watcher::new(),
            host: Host::new(proc_root, unique),
            menu: None,
            own: HashMap::new(),
            role: Role::Starting,
            requesting: false,
            deferred: TrayOutput::default(),
            appearance: None,
            media: None,
            notifications: None,
            failed: None,
        };
        t.call(Call::AddMatch { rule: owner_rule(WATCHER_NAME) }, Own::Ignore);
        t.request_name();
        t.host.start(&mut t.bus);
        let out = t.settle();
        t.deferred.merge(out);
        t
    }

    /// Follows the portal's color-scheme on this connection: the match rule, then one ReadOne
    /// (pending, its 2 s deadline on the tray's timer). Once: a second call is a no-op.
    pub fn start_appearance(&mut self) {
        if self.appearance.is_none() {
            self.appearance = Some(Appearance::start(&mut self.bus));
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    /// Follows the MPRIS players on this connection: two rules, then one ListNames (pending, on
    /// the tray's timer). Once: a second call is a no-op.
    pub fn start_media(&mut self, cfg: &MediaConfig) {
        if self.media.is_none() {
            self.media = Some(Mpris::start(&mut self.bus, cfg));
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    pub fn media(&self) -> Option<&Mpris> {
        self.media.as_ref()
    }

    /// Follows the user manager's failed-units count on this connection: two rules, then the
    /// owner asked for (pending, on the tray's timer). Once: a second call is a no-op.
    pub fn start_failed(&mut self, subscribe: bool) {
        if self.failed.is_none() {
            self.failed = Some(UserUnits::start(&mut self.bus, subscribe));
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    pub fn failed(&self) -> Option<&UserUnits> {
        self.failed.as_ref()
    }

    /// Runs `f` on the user manager's count with this connection (the system count was just read, a
    /// `failed-refresh`): flushed, and what the flush drained kept for `take_deferred`, as
    /// `media_do` does.
    pub fn failed_do(&mut self, f: impl FnOnce(&mut UserUnits, &mut SessionBus)) {
        if let Some(failed) = self.failed.as_mut() {
            f(failed, &mut self.bus);
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    /// Runs `f` on the media consumer with this connection (a control, the popup's Seeked rule and
    /// position reads, a pin): flushed, and what the flush drained kept for `take_deferred`, as
    /// `menu_click` does — never routed re-entrantly (`App::mpris_do` is its one caller).
    pub fn media_do(&mut self, f: impl FnOnce(&mut Mpris, &mut SessionBus)) {
        if let Some(media) = self.media.as_mut() {
            f(media, &mut self.bus);
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    /// Serves notifications on this connection: RequestName with flags 0 (queued behind another
    /// server, never replacing it). Once: a second call is a no-op. The bell's first view waits in
    /// `deferred`.
    pub fn start_notifications(&mut self, cfg: &NotificationsConfig, dnd: bool) {
        if self.notifications.is_none() {
            self.notifications = Some(Notifications::start(&mut self.bus, cfg, dnd));
            let mut out = self.settled();
            out.merge(self.notifications_output(false));
            self.deferred.merge(out);
        }
    }

    pub fn notifications(&self) -> Option<&Notifications> {
        self.notifications.as_ref()
    }

    /// Runs `f` on the notifications server (an expiry, a click, the panel, a verb): its signals sent
    /// and flushed, its view and bubbles kept for `take_deferred`, never routed re-entrantly
    /// (`App::notify_do` is its one caller).
    pub fn notifications_do(&mut self, f: impl FnOnce(&mut Notifications)) {
        if let Some(n) = self.notifications.as_mut() {
            f(n);
            for m in n.take_effects() {
                self.bus.send_message(m);
            }
            let mut out = self.settled();
            out.merge(self.notifications_output(false));
            self.deferred.merge(out);
        }
    }

    /// What the bar must hear from the notifications server after a batch: its view when it changed,
    /// whether the bubbles moved. `closed`: the bus went, and the name with it.
    fn notifications_output(&mut self, closed: bool) -> TrayOutput {
        let Some(n) = self.notifications.as_mut() else { return TrayOutput::default() };
        if closed {
            n.lost();
        }
        TrayOutput { notifications: n.take_change(), bubbles: n.take_bubbles_change(), ..TrayOutput::default() }
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

    /// The deadline timer fired (`now` is the instant calloop scheduled, possibly earlier than the
    /// real time): what already arrived is read first, so an answer waiting in the queue is never
    /// reported as a timeout; then the calls due by the real time are answered with Timeout, a burst
    /// of them costing one wake-up, and routed.
    pub fn on_deadline(&mut self, now: Instant) -> TrayOutput {
        let mut incoming = self.bus.read();
        incoming.extend(self.bus.expire(Instant::now().max(now)));
        self.route(incoming)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.bus.next_deadline()
    }

    /// A frame callback: the host's dirty properties re-read.
    pub fn on_frame(&mut self) -> TrayOutput {
        self.host.on_frame(&mut self.bus);
        if let Some(menu) = self.menu.as_mut() {
            menu.on_frame(&mut self.bus);
        }
        self.settled()
    }

    /// Something is dirty (an item's property, the open menu's layout): the wiring asks for one frame.
    pub fn wants_frame(&self) -> bool {
        self.host.wants_frame() || self.menu.as_ref().is_some_and(MenuSession::wants_frame)
    }

    /// Answers a flush drained that no routing has taken yet: after a burst past `SETTLE_ROUNDS`
    /// they wait with no fd event to announce them (`App::tray_sync` routes them from an idle).
    pub fn has_ready(&self) -> bool {
        self.bus.has_ready()
    }

    /// What answers drained during `open_menu`, `menu_submenu`, `menu_click` or `close_menu` made
    /// happen, when no other callback has returned it yet. Rare (it takes an answer arriving while
    /// a call is flushed); every other callback's output includes it already.
    /// Whether a flush drained answers that no caller has routed yet (`take_deferred`).
    pub fn has_deferred(&self) -> bool {
        self.deferred != TrayOutput::default()
    }

    pub fn take_deferred(&mut self) -> TrayOutput {
        std::mem::take(&mut self.deferred)
    }

    /// Flushes, routes what the flush drained (bounded), and returns it with anything deferred.
    fn settled(&mut self) -> TrayOutput {
        let mut out = std::mem::take(&mut self.deferred);
        out.merge(self.settle());
        out
    }

    /// Flush, then route the answers the flush read into libdbus's queue (the fd no longer reads as
    /// ready for them), flushing again what that sends, `SETTLE_ROUNDS` times at most.
    fn settle(&mut self) -> TrayOutput {
        let mut out = TrayOutput::default();
        for _ in 0..SETTLE_ROUNDS {
            self.bus.flush();
            if !self.bus.has_ready() {
                return out;
            }
            let ready = self.bus.take_ready();
            out.merge(self.dispatch(ready));
        }
        self.bus.flush();
        out
    }

    pub fn items(&self) -> &[Item] {
        self.host.items()
    }

    pub fn item(&self, key: &ItemKey) -> Option<&Item> {
        self.host.item(key)
    }

    /// Runs the gesture; TrayOutput.open_menu at once (OpenMenu) or when Activate's answer is an
    /// error reply (`on_readable`). The calls leave at once (flushed).
    pub fn gesture(&mut self, key: &ItemKey, g: Gesture, at: (i32, i32)) -> TrayOutput {
        let mut out = TrayOutput::default();
        match self.host.gesture(key, g, at, &mut self.bus) {
            GestureOutcome::Nothing => {}
            GestureOutcome::OpenMenu => out.open_menu = Some(key.clone()),
            GestureOutcome::Activated(id) => {
                self.own.insert(id, Own::Activate(key.clone()));
            }
        }
        out.merge(self.settled());
        out
    }

    /// After `Incoming::Closed` (`TrayOutput::bus_closed`), this is stale: serving has already
    /// stopped (the watcher is `set_serving(false)`, the host cleared) but `self.role` itself is
    /// left as it was, since nothing reads it afterward — the wiring drops the whole `Tray` the
    /// moment it sees `bus_closed`, never calling `role()` again. No new `Role` variant for a
    /// state nothing observes; `bus_closed` is the one signal that matters once the bus is gone.
    pub fn role(&self) -> &Role {
        &self.role
    }

    /// Routes `incoming`, then flushes and routes what the flush drained.
    fn route(&mut self, incoming: Vec<Incoming>) -> TrayOutput {
        let mut out = std::mem::take(&mut self.deferred);
        out.merge(self.dispatch(incoming));
        out.merge(self.settle());
        out
    }

    /// Routes `incoming` to its owners; sends what that makes, flushing nothing.
    fn dispatch(&mut self, incoming: Vec<Incoming>) -> TrayOutput {
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
        // Its item gone (its application left), or its path stale (a duplicate registration
        // re-read Menu and it changed or disappeared — host.rs): the menu
        // closes. No StatusNotifierItem signal exists for Menu, so this is the only place that
        // notices; checked once per batch, not per event.
        if let Some(m) = self.menu.as_ref() {
            let gone = self.host.item(&m.key).is_none();
            let moved = !gone && self.host.item(&m.key).and_then(|i| i.menu.as_deref()) != Some(m.path.as_str());
            if gone || moved {
                self.drop_menu();
                out.close_menu = true;
                out.menu_path_changed = moved;
            }
        }
        let changes = self.host.take_changes();
        out.redraw |= changes.redraw;
        for m in self.watcher.publish(&changes) {
            self.bus.send_message(m);
        }
        // The media module's text: once per batch, only when what the bar shows changed; the bus
        // gone, the module goes with it.
        if let Some(m) = self.media.as_mut() {
            if out.bus_closed {
                out.media = Some(None);
            } else if let Some(view) = m.take_view_change() {
                out.media = Some(view);
            }
        }
        // The user manager's count: once per batch, when it changed; the bus gone, it is unknown.
        if let Some(f) = self.failed.as_mut() {
            if out.bus_closed {
                f.lost();
            }
            if let Some(count) = f.take_change() {
                out.user_units = Some(count);
            }
        }
        // The notifications server: once per batch, its view and bubbles when they changed; the bus
        // gone, the name goes with it (no bubble, no timer).
        let closed = out.bus_closed;
        out.merge(self.notifications_output(closed));
        out
    }

    fn answer(&mut self, id: CallId, result: Result<Reply, CallError>) -> TrayOutput {
        if let Some(n) = self.notifications.as_mut().filter(|n| n.owns(id)) {
            n.answer(id, result);
            return TrayOutput::default();
        }
        if let Some(a) = self.appearance.as_mut().filter(|a| a.owns(id)) {
            return TrayOutput { appearance: a.answer(id, result), ..TrayOutput::default() };
        }
        if let Some(f) = self.failed.as_mut().filter(|f| f.owns(id)) {
            f.answer(&mut self.bus, id, result);
            return TrayOutput::default();
        }
        if let Some(m) = self.media.as_mut().filter(|m| m.owns(id)) {
            m.answer(&mut self.bus, id, result);
            return TrayOutput::default();
        }
        if self.host.owns(id) {
            self.host.answer(id, result, &mut self.bus);
            return TrayOutput::default();
        }
        if let Some(menu) = self.menu.as_mut().filter(|m| m.owns(id)) {
            let update = menu.answer(id, result, &mut self.bus);
            return self.menu_update(update);
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
            // An error reply from an item with a menu (Steam has no Activate): its menu instead.
            (Some(Own::Activate(key)), r) if self.host.opens_menu_after_activate(&key, &r) => {
                return TrayOutput { open_menu: Some(key), ..TrayOutput::default() };
            }
            // A refused RequestName (`retry: false`) whose GetNameOwner also found nobody owning
            // the name (an error, or a timeout) falls here: no retry loop, by design — the same
            // "never loop on a bus that refuses us" rule as a RequestName failure alone. `role()`
            // stays `Starting` for good; the host's own one-shot ListNames inventory (already
            // issued in `start`) still ran, so items present at startup are still found — an
            // inventory-only host, just never live-updated through a watcher.
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
        // MPRIS names, Player properties and Seeked are the consumer's, never the host's.
        // Every other NameOwnerChanged (the watcher's name, items' owners) goes on as before.
        // NameAcquired / NameLost for org.freedesktop.Notifications; the
        // watcher's own NameAcquired goes on as before.
        if let Some(n) = self.notifications.as_mut()
            && n.signal(&s)
        {
            return TrayOutput::default();
        }
        if let Some(media) = self.media.as_mut()
            && media.signal(&mut self.bus, &s)
        {
            return TrayOutput::default();
        }
        if let Some(failed) = self.failed.as_mut()
            && failed.signal(&mut self.bus, &s)
        {
            return TrayOutput::default();
        }
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
            Signal::ColorScheme(value) => {
                if let Some(a) = self.appearance.as_mut() {
                    return TrayOutput { appearance: a.changed(*value), ..TrayOutput::default() };
                }
            }
            Signal::LayoutUpdated { .. } | Signal::ItemsPropertiesUpdated { .. } => {
                if let Some(menu) = self.menu.as_mut() {
                    let update = menu.signal(&s);
                    return self.menu_update(update);
                }
            }
            _ => {}
        }
        TrayOutput::default()
    }

    fn menu_update(&mut self, update: MenuUpdate) -> TrayOutput {
        match update {
            MenuUpdate::Nothing => TrayOutput::default(),
            MenuUpdate::Changed => TrayOutput { menu_changed: true, ..TrayOutput::default() },
            MenuUpdate::Close => {
                self.drop_menu();
                TrayOutput { close_menu: true, ..TrayOutput::default() }
            }
        }
    }

    /// Opens `key`'s menu session (closing any other: one menu at a time); false when the item is gone
    /// or has no Menu.
    pub fn open_menu(&mut self, key: &ItemKey) -> bool {
        let Some(path) = self.host.item(key).and_then(|i| i.menu.clone()) else { return false };
        self.drop_menu();
        self.menu = Some(MenuSession::open(key.clone(), path, &mut self.bus));
        let out = self.settled();
        self.deferred.merge(out);
        true
    }

    pub fn menu(&self) -> Option<&MenuSession> {
        self.menu.as_ref()
    }

    pub fn menu_submenu(&mut self, id: i32) {
        if let Some(menu) = self.menu.as_mut() {
            menu.fetch_submenu(id, &mut self.bus);
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    pub fn menu_click(&mut self, id: i32, timestamp: u32) {
        if let Some(menu) = self.menu.as_mut() {
            menu.click(id, timestamp, &mut self.bus);
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    pub fn close_menu(&mut self) {
        if self.menu.is_some() {
            self.drop_menu();
            let out = self.settled();
            self.deferred.merge(out);
        }
    }

    /// The open menu closed (its rule removed), flushing nothing: the caller's flush follows.
    fn drop_menu(&mut self) {
        if let Some(menu) = self.menu.take() {
            menu.close(&mut self.bus);
        }
    }

    fn inbound_call(&mut self, m: Message) {
        // The Notifications path first: served while the name is held,
        // refused otherwise; refused too when the module is off, as any path we do not serve. Its
        // signals (NotificationClosed for a CloseNotification) leave right after the reply.
        if notifications::serves(&m) {
            let reply = match self.notifications.as_mut() {
                Some(n) => n.handle(&m, Instant::now(), notifications::minute_now),
                None => (!m.get_no_reply()).then(|| refusal(&m)),
            };
            if let Some(r) = reply {
                self.bus.send_message(r);
            }
            if let Some(n) = self.notifications.as_mut() {
                for s in n.take_effects() {
                    self.bus.send_message(s);
                }
            }
            return;
        }
        // Served while we own the name; refused (never left unanswered) otherwise.
        for r in self.watcher.handle(m) {
            self.bus.send_message(r);
        }
        // `Host::add` refuses a registration naming our own unique name, checked on the resolved
        // owner: moved there from an early check once made here, so it also
        // covers an entry of another watcher's list and that watcher's ItemRegistered signal
        // (`follow`/`signal` below call `register` the very same way) — not only a direct
        // RegisterStatusNotifierItem naming us, the only path this router-level check used to see.
        for (service, sender) in self.watcher.take_registrations() {
            self.host.register(&service, &sender, &mut self.bus);
        }
    }
}
