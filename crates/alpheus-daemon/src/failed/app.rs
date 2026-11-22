//! The user manager's count of failed units: the session bus's half of the `failed` module.
//!
//! `UserUnits` reads `NFailedUnits` of `org.freedesktop.systemd1` on the session bus, through the
//! owner's unique name. It `Subscribe`s to systemd only when `[failed] subscribe` is on, and it reads
//! the count again whenever the system count was read (`Msg::UnitsRead`: a poke, a resume, a read
//! added to another domain's wake-up), so with the subscription off the user count follows the same
//! triggers as the system count. The `impl App` block at the end ties it to the daemon: it starts
//! the client, hands the count to the bar, the hooks and the notice, and answers
//! `alpheus-launch --failed-refresh`.
//!
//! It rides the tray's session-bus connection, else the bar's own link (`link.rs`), exactly as the
//! media module does: one connection, never one of its own. Calls and decoding are the tray's typed
//! allowlist (a `Get` of the count property, the subscription, the owner's lookup).
//!
//! Never:
//! - name the dbus crate or build a call by hand: the tray's typed calls and codec do that;
//! - read anything but that one property: no whole-interface read (that of the Manager carries its
//!   environment, which may hold secrets), and no unit name;
//! - log anything but fixed words (no owner, no unit name, no path);
//! - start a thread or a process.
//!
//! A guardrail counts the calls this file builds and looks for the word `method_call` in it, so no
//! function here is named with it.
//!
//! See: docs/specs/2026-11-21-alpheus-diagnostics-design.md (§ 6), docs/features/failed.md, and
//! docs/decisions.md ("The failed-units count is asked for one property, never the whole
//! interface", "A source that is down is not a state").

use crate::link::{Host, LinkOutput, LinkUser, host};
use crate::log::write as log;
use crate::session_bus::bus::SessionBus;
use crate::session_bus::bus::{Call, CallError, CallId, Outbox, Property};
use crate::session_bus::codec::{Reply, Signal};
use crate::session_bus::router::TrayOutput;
use crate::session_bus::{SYSTEMD_NAME, SYSTEMD_OWNER_RULE, SYSTEMD_PATH, SYSTEMD_PROPERTIES_RULE};
use crate::state::Msg;
use crate::wayland::App;
use alpheus_common::launcher_ipc::{Reply as IpcReply, Request};

/// What `Subscribe` answers when a client already subscribed (a `daemon-reexec` keeps it).
const ALREADY_SUBSCRIBED: &str = "org.freedesktop.systemd1.AlreadySubscribed";

/// The user manager's count and the calls in flight for it.
// `pub` makes the type visible outside the crate too: the integration tests use the library.
#[derive(Debug)]
pub struct UserUnits {
    /// Where the fixed log lines go (tests capture them).
    say: fn(&str),
    /// `[failed] subscribe`.
    subscribe: bool,
    /// The user manager's unique name; None: nobody owns the name (or not asked yet).
    owner: Option<String>,
    /// The last count read; None: unknown.
    count: Option<u32>,
    /// `Subscribe` was accepted for the current owner.
    subscribed: bool,
    /// The failed owner lookup was logged: one line per episode (until a manager is found).
    lookup_logged: bool,
    /// A failed read was logged: one line per episode (until a read succeeds).
    read_logged: bool,
    /// The owner whose refusal was logged: one line per owner.
    refused_for: Option<String>,
    /// The calls in flight (at most one of each): the owner, the count, the subscription.
    owner_call: Option<CallId>,
    get_call: Option<CallId>,
    sub_call: Option<CallId>,
    /// A change came while a read was in flight: read once more after it.
    reread: bool,
    /// `count` changed since `take_change`, or this is its first answer (even an unknown one: the
    /// notice waits for it).
    dirty: bool,
    /// The count was set at least once.
    answered: bool,
}

impl UserUnits {
    /// Starts the client: the two match rules first (sent with no reply, so nothing is pending for
    /// them), so no change slips in between the read and the rules; then the owner is asked, and its
    /// answer starts the read.
    pub fn start(out: &mut impl Outbox, subscribe: bool) -> UserUnits {
        Self::start_logged(out, subscribe, log)
    }

    fn start_logged(out: &mut impl Outbox, subscribe: bool, say: fn(&str)) -> UserUnits {
        out.send(Call::AddMatch { rule: SYSTEMD_OWNER_RULE.to_string() });
        out.send(Call::AddMatch { rule: SYSTEMD_PROPERTIES_RULE.to_string() });
        let mut u = UserUnits {
            say,
            subscribe,
            owner: None,
            count: None,
            subscribed: false,
            lookup_logged: false,
            read_logged: false,
            refused_for: None,
            owner_call: None,
            get_call: None,
            sub_call: None,
            reread: false,
            dirty: false,
            answered: false,
        };
        u.ask_owner(out);
        u
    }

    /// The last count read; None: unknown.
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    /// Whether `Subscribe` was accepted for the current owner.
    pub fn subscribed(&self) -> bool {
        self.subscribed
    }

    /// Whether `id` is one of this client's calls in flight (the owner's, the count's or the
    /// subscription's).
    pub fn owns(&self, id: CallId) -> bool {
        [self.owner_call, self.get_call, self.sub_call].contains(&Some(id))
    }

    /// The count when it changed since the last call. Two layers of `Option`: the outer says
    /// whether it changed, the inner is the count, so `Some(None)` means it became unknown, or the
    /// first answer is that nobody runs the manager.
    pub fn take_change(&mut self) -> Option<Option<u32>> {
        // `std::mem::take` returns the flag and leaves `false` in its place: a change is told once.
        let changed = std::mem::take(&mut self.dirty);
        if !changed {
            return None;
        }
        Some(self.count)
    }

    fn ask_owner(&mut self, out: &mut impl Outbox) {
        if self.owner_call.is_none() {
            self.owner_call = Some(out.call(Call::GetNameOwner { name: SYSTEMD_NAME.to_string() }));
        }
    }

    /// One `Get` of the count at a time; a request that comes meanwhile is read after the answer.
    fn read(&mut self, out: &mut impl Outbox) {
        let Some(owner) = self.owner.clone() else { return };
        if self.get_call.is_some() {
            self.reread = true;
            return;
        }
        self.get_call = Some(out.call(Call::Get { destination: owner, path: SYSTEMD_PATH.to_string(), property: Property::NFailedUnits }));
    }

    /// Keeps `count`, and marks it changed when it differs from the one kept or is the first
    /// answer (even an unknown one: the notice waits for it).
    fn set_count(&mut self, count: Option<u32>) {
        if self.count != count || !self.answered {
            self.count = count;
            self.dirty = true;
            self.answered = true;
        }
    }

    /// The owner is no longer there (or not at all): unknown, the owner's calls forgotten.
    fn forget_owner(&mut self) {
        self.forget_owner_keeping_count();
        self.set_count(None);
    }

    /// A new owner: unsubscribed, its calls forgotten; the count stays until it is read again.
    fn forget_owner_keeping_count(&mut self) {
        self.owner = None;
        self.subscribed = false;
        self.get_call = None;
        self.sub_call = None;
        self.reread = false;
    }

    /// The answer to one of our calls (`owns(id)`).
    pub fn answer(&mut self, out: &mut impl Outbox, id: CallId, result: Result<Reply, CallError>) {
        if self.owner_call == Some(id) {
            self.owner_call = None;
            match result {
                Ok(Reply::Owner(owner)) => {
                    self.owner = Some(owner);
                    self.lookup_logged = false;
                    self.read(out);
                }
                // Nobody runs it: unknown, and the owner rule says when it comes.
                Err(CallError::Remote(name)) if name == "org.freedesktop.DBus.Error.NameHasNoOwner" => self.forget_owner(),
                // Any other answer: one log line per episode, then unknown.
                Ok(_) | Err(_) => {
                    if !self.lookup_logged {
                        self.lookup_logged = true;
                        (self.say)("failed: the user manager could not be found");
                    }
                    self.forget_owner();
                }
            }
        } else if self.get_call == Some(id) {
            self.get_call = None;
            match result {
                Ok(Reply::Count(n)) => {
                    self.read_logged = false;
                    self.set_count(Some(n));
                    self.subscribe_if_wanted(out);
                }
                other => {
                    // The log line is fixed words, like the system side's: never the error's text.
                    // A closed connection logs nothing here.
                    let disconnected = matches!(other, Err(CallError::Disconnected));
                    if !disconnected && !self.read_logged {
                        self.read_logged = true;
                        (self.say)("failed: the user manager's count could not be read");
                    }
                    self.set_count(None);
                }
            }
            // A change came while this read was in flight: read once more.
            if self.reread {
                self.reread = false;
                self.read(out);
            }
        } else if self.sub_call == Some(id) {
            self.sub_call = None;
            match result {
                Ok(_) => self.subscribed = true,
                Err(CallError::Remote(name)) if name == ALREADY_SUBSCRIBED => self.subscribed = true,
                // Refused or timed out: the client carries on unsubscribed (the triggers still
                // read the count), with one log line per owner.
                Err(_) => {
                    if self.refused_for != self.owner {
                        self.refused_for = self.owner.clone();
                        (self.say)("failed: user manager Subscribe refused");
                    }
                }
            }
        }
    }

    /// After a read: `Subscribe` once per owner, when wanted, not yet done and not refused for this
    /// owner.
    fn subscribe_if_wanted(&mut self, out: &mut impl Outbox) {
        let Some(owner) = self.owner.clone() else { return };
        if self.subscribe && !self.subscribed && self.sub_call.is_none() && self.refused_for.as_ref() != Some(&owner) {
            self.sub_call = Some(out.call(Call::SubscribeUnits { dest: owner }));
        }
    }

    /// Handles a signal; true when it is the user manager's own (a change of the name's owner, or
    /// a change of its count sent by its owner), which then is not the host's to handle.
    pub fn signal(&mut self, out: &mut impl Outbox, s: &Signal) -> bool {
        match s {
            Signal::NameOwnerChanged { name, new, .. } if name == SYSTEMD_NAME => {
                // A new owner (a restart, a `daemon-reexec`) has no subscriber, and its calls are
                // ours alone: the old owner's are forgotten.
                self.forget_owner_keeping_count();
                if new.is_empty() {
                    self.set_count(None);
                } else {
                    self.owner = Some(new.clone());
                    self.read(out);
                }
                true
            }
            // The count changed (only an owner we know counts: another sender's signal is not ours).
            Signal::UnitsChanged { sender } if self.owner.as_ref() == Some(sender) => {
                self.read(out);
                true
            }
            _ => false,
        }
    }

    /// The system count was just read (`Msg::UnitsRead`): reads the user count once more, which is
    /// what makes it follow a `failed-refresh`, a resume and a read added to another domain's
    /// wake-up when the subscription is off. With no manager known, its owner is asked for again.
    pub fn on_units_read(&mut self, out: &mut impl Outbox) {
        if self.owner.is_some() {
            self.read(out);
        } else {
            self.ask_owner(out);
        }
    }

    /// The session bus closed: the count is unknown here and nothing is subscribed. The link is
    /// never reconnected, so the bar keeps the count it last had until the daemon restarts (a source
    /// that is down is not a state: see docs/decisions.md).
    pub fn lost(&mut self) {
        self.owner_call = None;
        self.forget_owner();
    }
}

impl App {
    /// Starts the user manager's count, once, at startup (`wanted`: a module asks for it). It goes on
    /// the tray's connection when the tray runs; otherwise on the bar's link, opened here or joined.
    /// Without a module asking for it: nothing, no rule, no call.
    pub(crate) fn start_failed(&mut self, wanted: bool) {
        let subscribe = self.bar.config.failed.subscribe;
        // `host` says where the client lives: on the tray's connection, on the bar's link (already
        // open or opened here), or nowhere (not wanted, or no session bus).
        match host(wanted, self.buses()) {
            Host::Nothing => self.failed_user_never(),
            Host::NoBus => {
                log("failed: no session bus");
                self.failed_user_never();
            }
            Host::OnTray => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.start_failed(subscribe);
                }
                log("failed: the user manager on the tray's session bus");
                // Its owner question is pending: the tray's deadline timer is armed for it.
                self.tray_output(TrayOutput::default());
            }
            Host::OnLink | Host::OwnBus => {
                if self.link.session.is_none() && !self.open_link(LinkUser::Failed) {
                    self.failed_user_never();
                    return;
                }
                if let Some(link) = self.link.session.as_mut() {
                    link.start_failed(subscribe);
                }
                log("failed: the user manager on the bar's session bus");
                self.link_output(LinkOutput::default());
            }
        }
    }

    /// Takes the user manager's count, from either connection: it goes to the hooks, to the bar
    /// (which repaints only when the count changed) and to the notice.
    pub(crate) fn failed_user(&mut self, count: Option<u32>) {
        self.failed_user_settled();
        self.hooks_msg(&Msg::UserUnits(count));
        if self.bar.apply(Msg::UserUnits(count)) {
            self.dirty = true;
        }
        self.failed_notify();
    }

    /// The user manager answered once (an unknown count included): the notice may go.
    fn failed_user_settled(&mut self) {
        if let Some(notice) = self.failed_notice.as_mut() {
            notice.settle_user();
        }
    }

    /// No client will ask the user manager (`user_units = false`, no session bus, no link): the
    /// notice does not wait for it.
    pub(crate) fn failed_user_never(&mut self) {
        self.failed_user_settled();
    }

    /// Runs `f` on the user manager's count wherever it lives, flushed; what the flush drained comes
    /// back through the tray's deferred idle or the link's `has_ready`, never re-entrantly. With no
    /// client of the count, nothing.
    fn failed_do(&mut self, f: impl FnOnce(&mut UserUnits, &mut SessionBus)) {
        // The client lives on the tray's connection if the tray has one.
        if let Some(tray) = self.tray.as_mut() {
            if tray.failed().is_some() {
                tray.failed_do(f);
                self.tray_sync();
                return;
            }
        }
        // Else on the bar's link, if it has one.
        let Some(link) = self.link.session.as_mut() else { return };
        if link.failed().is_none() {
            return;
        }
        link.failed_do(f);
        self.link_sync();
    }

    /// The system count was just read (`Msg::UnitsRead`): the user manager's once more.
    pub(crate) fn failed_units_read(&mut self) {
        self.failed_do(|u, out| u.on_units_read(out));
    }

    /// `alpheus-launch --failed-refresh` (and the diagnostics tab's refresh), answered before the
    /// launcher's guard (`launcher/socket_glue.rs`): Done, also with the module off (nothing is
    /// there to read: no tap, no user client).
    pub(crate) fn failed_verb(&mut self, request: &Request) -> IpcReply {
        if matches!(request, Request::FailedRefresh) {
            self.failed_refresh();
        }
        IpcReply::Done
    }

    /// Reads both counts now, whatever the subscription: the system one by a poke of the D-Bus
    /// thread (its answer is a `UnitsRead`), the user one on this loop.
    pub(crate) fn failed_refresh(&mut self) {
        // With a tap, the thread's read answers `UnitsRead`, which reads the user count: one read
        // each. Without one (the module off the system bus), the user count is read here.
        if let Some(tap) = &self.units_tap {
            tap.poke();
        } else {
            self.failed_units_read();
        }
    }
}

#[cfg(test)]
mod tests;
