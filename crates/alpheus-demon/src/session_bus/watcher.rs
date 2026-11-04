//! The `org.kde.StatusNotifierWatcher` object, served through
//! dbus-crossroads (the only file that knows it): what real clients check
//! before registering (spike-rustsni-rustbus.md § 3) — Chromium reads
//! `IsStatusNotifierHostRegistered`, KDE reads `ProtocolVersion`,
//! libayatana falls back on an error reply — plus `Introspectable` and
//! `Peer`. Registrations are answered at once and queued for the host;
//! nothing is served unless the daemon owns the watcher's name.

use super::bus::refusal;
use super::codec::MAX_SERVICE;
use super::host::Changes;
use super::{WATCHER_INTERFACE, WATCHER_PATH};
use crate::sources::dbus::PROPERTIES;
use dbus::Message;
use dbus::strings::{ErrorName, Path};
use dbus_crossroads::{Crossroads, IfaceBuilder};
use std::cell::RefCell;

/// The object's data, reached by the method handlers (`&mut State`) and by
/// the watcher (`Crossroads::data_mut`).
#[derive(Debug, Default)]
struct State {
    /// RegisteredStatusNotifierItems, Plasma's "<service><path>".
    entries: Vec<String>,
    /// Registrations not taken yet: (service string, sender's unique name).
    registrations: Vec<(String, String)>,
}

/// `org.kde.StatusNotifierWatcher` at WATCHER_PATH: RegisterStatusNotifierItem(s) (answered at once,
/// idempotent), RegisterStatusNotifierHost(s) (accepted), properties RegisteredStatusNotifierItems (as),
/// IsStatusNotifierHostRegistered (b, true while serving), ProtocolVersion (i, 0), Set → PropertyReadOnly;
/// Introspectable on WATCHER_PATH and "/", Peer.
pub struct Watcher {
    cr: Crossroads,
    serving: bool,
}

impl Default for Watcher {
    fn default() -> Self {
        Watcher::new()
    }
}

impl Watcher {
    pub fn new() -> Watcher {
        let mut cr = Crossroads::new();
        let iface = cr.register(WATCHER_INTERFACE, |b: &mut IfaceBuilder<State>| {
            b.method("RegisterStatusNotifierItem", ("service",), (), |ctx, state: &mut State, (service,): (String,)| {
                let sender = ctx.message().sender().map(|s| s.to_string()).unwrap_or_default();
                // Longer than any service string the host would accept: dropped before it is kept.
                if service.len() <= MAX_SERVICE {
                    state.registrations.push((service, sender));
                }
                Ok(())
            });
            // Plasma ignores the argument too: we are the only host that matters here.
            b.method("RegisterStatusNotifierHost", ("service",), (), |_, _, (_,): (String,)| Ok(()));
            b.property("RegisteredStatusNotifierItems").get(|_, state| Ok(state.entries.clone()));
            b.property("IsStatusNotifierHostRegistered").get(|_, _| Ok(true));
            b.property("ProtocolVersion").get(|_, _| Ok(0i32));
            b.signal::<(String,), _>("StatusNotifierItemRegistered", ("service",));
            b.signal::<(String,), _>("StatusNotifierItemUnregistered", ("service",));
            b.signal::<(), _>("StatusNotifierHostRegistered", ());
        });
        cr.insert(WATCHER_PATH, &[iface], State::default());
        Watcher { cr, serving: false }
    }

    /// Serving only while the daemon owns WATCHER_NAME; otherwise every call is refused (UnknownMethod).
    pub fn set_serving(&mut self, serving: bool) {
        self.serving = serving;
    }

    pub fn is_serving(&self) -> bool {
        self.serving
    }

    fn state(&mut self) -> &mut State {
        self.cr.data_mut::<State>(&Path::from(WATCHER_PATH)).expect("the watcher object is inserted in new()")
    }

    /// Handles one incoming method call; the messages to send back (replies, errors).
    pub fn handle(&mut self, call: Message) -> Vec<Message> {
        let set = call.path().is_some_and(|p| &*p == WATCHER_PATH)
            && call.interface().is_some_and(|i| &*i == PROPERTIES)
            && call.member().is_some_and(|m| &*m == "Set");
        if !self.serving || set {
            if call.get_no_reply() {
                return Vec::new();
            }
            let name = ErrorName::from("org.freedesktop.DBus.Error.PropertyReadOnly");
            return vec![if self.serving { call.error(&name, c"the watcher's properties are read-only") } else { refusal(&call) }];
        }
        let sink = RefCell::new(Vec::new());
        let _ = self.cr.handle_message(call, &sink);
        sink.into_inner()
    }

    /// Registrations received since the last call: (service string, sender's unique name).
    pub fn take_registrations(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.state().registrations)
    }

    /// Applies the host's changes to RegisteredStatusNotifierItems; the signals to emit
    /// (StatusNotifierItemRegistered / …Unregistered, only while serving).
    pub fn publish(&mut self, changes: &Changes) -> Vec<Message> {
        let serving = self.serving;
        let state = self.state();
        let mut signals = Vec::new();
        for gone in &changes.unregistered {
            if let Some(i) = state.entries.iter().position(|e| e == gone) {
                state.entries.remove(i);
                signals.push(("StatusNotifierItemUnregistered", gone.clone()));
            }
        }
        for new in &changes.registered {
            if !state.entries.contains(new) {
                state.entries.push(new.clone());
                signals.push(("StatusNotifierItemRegistered", new.clone()));
            }
        }
        if !serving {
            return Vec::new();
        }
        signals.into_iter().map(|(member, entry)| signal(member).append1(entry)).collect()
    }

    /// StatusNotifierHostRegistered, emitted once the name is ours (we are our own host).
    pub fn host_registered(&self) -> Message {
        signal("StatusNotifierHostRegistered")
    }
}

fn signal(member: &str) -> Message {
    Message::new_signal(WATCHER_PATH, WATCHER_INTERFACE, member).expect("a constant path, interface and member")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::arg::Variant;
    use dbus::message::MessageType;

    fn call(path: &str, interface: &str, member: &str) -> Message {
        let mut m = Message::new_method_call(":1.1", path, interface, member).unwrap();
        m.set_serial(7); // as received
        m.set_sender(Some(":1.42".into()));
        m
    }

    fn get(w: &mut Watcher, name: &str) -> Message {
        let r = w.handle(call(WATCHER_PATH, "org.freedesktop.DBus.Properties", "Get").append2(WATCHER_INTERFACE, name));
        assert_eq!(r.len(), 1);
        r.into_iter().next().unwrap()
    }

    fn error_name(mut m: Message) -> String {
        m.as_result().err().and_then(|e| e.name().map(String::from)).unwrap_or_default()
    }

    #[test]
    fn the_watcher_answers_what_clients_check() {
        let mut w = Watcher::new();
        w.set_serving(true);
        let r =
            w.handle(call(WATCHER_PATH, WATCHER_INTERFACE, "RegisterStatusNotifierItem").append1("/org/ayatana/NotificationItem/quill"));
        assert_eq!(r.iter().map(|m| m.msg_type()).collect::<Vec<_>>(), [MessageType::MethodReturn], "answered at once");
        assert_eq!(w.take_registrations(), [("/org/ayatana/NotificationItem/quill".to_string(), ":1.42".to_string())]);
        assert!(w.take_registrations().is_empty(), "taken once");
        assert!(get(&mut w, "IsStatusNotifierHostRegistered").read1::<Variant<bool>>().unwrap().0, "Chromium checks it");
        assert_eq!(get(&mut w, "ProtocolVersion").read1::<Variant<i32>>().unwrap().0, 0, "KDE checks it");
        let host = w.handle(call(WATCHER_PATH, WATCHER_INTERFACE, "RegisterStatusNotifierHost").append1("org.example.Host"));
        assert_eq!(host[0].msg_type(), MessageType::MethodReturn);
        let set = w.handle(call(WATCHER_PATH, "org.freedesktop.DBus.Properties", "Set").append3(
            WATCHER_INTERFACE,
            "ProtocolVersion",
            Variant(3i32),
        ));
        assert_eq!(error_name(set.into_iter().next().unwrap()), "org.freedesktop.DBus.Error.PropertyReadOnly");
        let elsewhere =
            w.handle(call("/", "org.freedesktop.DBus.Properties", "Set").append3(WATCHER_INTERFACE, "ProtocolVersion", Variant(3i32)));
        assert_ne!(
            error_name(elsewhere.into_iter().next().unwrap()),
            "org.freedesktop.DBus.Error.PropertyReadOnly",
            "the watcher's properties live at WATCHER_PATH only"
        );
        let introspect = w.handle(call(WATCHER_PATH, "org.freedesktop.DBus.Introspectable", "Introspect"));
        assert!(introspect[0].read1::<&str>().unwrap().contains("RegisterStatusNotifierItem"));
        let root = w.handle(call("/", "org.freedesktop.DBus.Introspectable", "Introspect"));
        assert!(root[0].read1::<&str>().unwrap().contains("StatusNotifierWatcher"), "the root lists the watcher");
        assert_eq!(w.handle(call(WATCHER_PATH, "org.freedesktop.DBus.Peer", "Ping"))[0].msg_type(), MessageType::MethodReturn);
        let bogus = w.handle(call(WATCHER_PATH, WATCHER_INTERFACE, "Bogus"));
        assert_eq!(error_name(bogus.into_iter().next().unwrap()), "org.freedesktop.DBus.Error.UnknownMethod");
        let mut quiet = call(WATCHER_PATH, WATCHER_INTERFACE, "RegisterStatusNotifierItem").append1(":1.42");
        quiet.set_no_reply(true);
        assert!(w.handle(quiet).is_empty(), "no reply when none is expected");
        assert_eq!(w.take_registrations().len(), 1, "but registered");
    }

    #[test]
    fn an_oversized_registration_is_answered_but_never_kept() {
        let mut w = Watcher::new();
        w.set_serving(true);
        let huge = "a.".repeat(512 * 1024);
        let r = w.handle(call(WATCHER_PATH, WATCHER_INTERFACE, "RegisterStatusNotifierItem").append1(huge));
        assert_eq!(r[0].msg_type(), MessageType::MethodReturn, "still answered");
        assert!(w.take_registrations().is_empty(), "never queued for the host");
        let longest = "a".repeat(MAX_SERVICE);
        w.handle(call(WATCHER_PATH, WATCHER_INTERFACE, "RegisterStatusNotifierItem").append1(longest));
        assert_eq!(w.take_registrations().len(), 1, "the bound itself passes");
    }

    #[test]
    fn nothing_is_served_while_another_watcher_owns_the_name() {
        let mut w = Watcher::new();
        assert!(!w.is_serving());
        let r = w.handle(call(WATCHER_PATH, WATCHER_INTERFACE, "RegisterStatusNotifierItem").append1(":1.42"));
        assert_eq!(error_name(r.into_iter().next().unwrap()), "org.freedesktop.DBus.Error.UnknownMethod");
        assert!(w.take_registrations().is_empty());
        let changes = Changes { redraw: true, registered: vec![":1.42/StatusNotifierItem".into()], unregistered: vec![] };
        assert!(w.publish(&changes).is_empty(), "no signal while host-only");
        w.set_serving(true);
        let list = get(&mut w, "RegisteredStatusNotifierItems").read1::<Variant<Vec<String>>>().unwrap().0;
        assert_eq!(list, [":1.42/StatusNotifierItem"], "the list kept up to date meanwhile");
    }

    #[test]
    fn publish_keeps_the_list_and_emits_while_serving() {
        let mut w = Watcher::new();
        w.set_serving(true);
        let entry = "org.kde.StatusNotifierItem-4242-1/StatusNotifierItem".to_string();
        let added = w.publish(&Changes { redraw: false, registered: vec![entry.clone(), entry.clone()], unregistered: vec![] });
        assert_eq!(added.len(), 1, "a duplicate entry is one");
        assert_eq!(added[0].member().as_deref(), Some("StatusNotifierItemRegistered"));
        assert_eq!(added[0].read1::<&str>().unwrap(), entry);
        let removed = w.publish(&Changes { redraw: true, registered: vec![], unregistered: vec![entry.clone(), "unknown".into()] });
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].member().as_deref(), Some("StatusNotifierItemUnregistered"));
        assert!(get(&mut w, "RegisteredStatusNotifierItems").read1::<Variant<Vec<String>>>().unwrap().0.is_empty());
        assert_eq!(w.host_registered().member().as_deref(), Some("StatusNotifierHostRegistered"));
    }
}
