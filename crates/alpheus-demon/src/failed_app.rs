//! The user manager's count of failed units (diagnostics spec § 6), the session bus's half of the
//! `failed` module: `NFailedUnits` of `org.freedesktop.systemd1` on the session bus, read through
//! the owner's unique name, `Subscribe`d to unless `[failed] subscribe` is off, and read again
//! whenever the system count was read (`Msg::UnitsRead`: a poke, a resume, a piggyback), so with
//! the subscription off the user count follows the same triggers.
//!
//! It rides the tray's connection, else the bar's own link (link.rs), exactly as the media module
//! does: one connection. Calls and decoding are the tray's typed allowlist (a `Get` of
//! the count property, the subscription, the owner's lookup), so this file never
//! names the dbus crate and never reads anything but that one property: no whole-interface read (the
//! Manager's carries its environment), no unit name, and the log lines are fixed words. Never
//! name a function `method_call` here: the guardrails count that substring.

use crate::link::{Host, LinkOutput, LinkUser, host};
use crate::log::write as log;
use crate::session_bus::bus::SessionBus;
use crate::session_bus::bus::{Call, CallError, CallId, Outbox, Property};
use crate::session_bus::codec::{Reply, Signal};
use crate::session_bus::router::TrayOutput;
use crate::session_bus::{SYSTEMD_NAME, SYSTEMD_OWNER_RULE, SYSTEMD_PATH, SYSTEMD_PROPERTIES_RULE};
use crate::state::Msg;
use crate::wayland::App;
use alpheus_commun::launcher_ipc::{Reply as IpcReply, Request};

/// What `Subscribe` answers when a client already subscribed (a `daemon-reexec` keeps it).
const ALREADY_SUBSCRIBED: &str = "org.freedesktop.systemd1.AlreadySubscribed";

/// The user manager's count and the calls in flight for it.
#[derive(Debug)]
pub struct UserUnits {
    /// Where the fixed log lines go (tests capture them).
    say: fn(&str),
    /// `[failed] subscribe`.
    subscribe: bool,
    /// The user manager's unique name; None: nobody owns the name (or not asked yet).
    owner: Option<String>,
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
    /// The two rules first (sent with no reply: nothing pending for them), so no change slips
    /// between the read and the rules; then the owner is asked, which starts the read.
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

    pub fn count(&self) -> Option<u32> {
        self.count
    }

    pub fn subscribed(&self) -> bool {
        self.subscribed
    }

    pub fn owns(&self, id: CallId) -> bool {
        [self.owner_call, self.get_call, self.sub_call].contains(&Some(id))
    }

    /// The count when it changed since the last call (Some(None): it became unknown, or the first
    /// answer is that nobody runs the manager).
    pub fn take_change(&mut self) -> Option<Option<u32>> {
        std::mem::take(&mut self.dirty).then_some(self.count)
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

    fn set_count(&mut self, count: Option<u32>) {
        if self.count != count || !self.answered {
            self.count = count;
            self.dirty = true;
            self.answered = true;
        }
    }

    /// The owner is no longer there (or not at all): unknown, the owner's calls forgotten.
    fn forget_owner(&mut self) {
        self.owner = None;
        self.subscribed = false;
        self.get_call = None;
        self.sub_call = None;
        self.reread = false;
        self.set_count(None);
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
                // Nobody runs it: unknown, and the rule says when it comes.
                Err(CallError::Remote(name)) if name == "org.freedesktop.DBus.Error.NameHasNoOwner" => self.forget_owner(),
                Ok(_) | Err(_) => {
                    if !std::mem::replace(&mut self.lookup_logged, true) {
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
                    // Fixed words, like the system side's: never the error's text.
                    if !matches!(other, Err(CallError::Disconnected)) && !std::mem::replace(&mut self.read_logged, true) {
                        (self.say)("failed: the user manager's count could not be read");
                    }
                    self.set_count(None);
                }
            }
            if std::mem::take(&mut self.reread) {
                self.read(out);
            }
        } else if self.sub_call == Some(id) {
            self.sub_call = None;
            match result {
                Ok(_) => self.subscribed = true,
                Err(CallError::Remote(name)) if name == ALREADY_SUBSCRIBED => self.subscribed = true,
                Err(_) => {
                    if self.refused_for != self.owner {
                        self.refused_for = self.owner.clone();
                        (self.say)("failed: user manager Subscribe refused");
                    }
                }
            }
        }
    }

    /// After a read: `Subscribe` once per owner, when wanted and not refused for this owner.
    fn subscribe_if_wanted(&mut self, out: &mut impl Outbox) {
        let Some(owner) = self.owner.clone() else { return };
        if self.subscribe && !self.subscribed && self.sub_call.is_none() && self.refused_for.as_ref() != Some(&owner) {
            self.sub_call = Some(out.call(Call::SubscribeUnits { dest: owner }));
        }
    }

    /// A signal: true when it is the user manager's (the name's owner, or its count's change from
    /// its owner), which then is not the host's.
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
            Signal::UnitsChanged { sender } if self.owner.as_ref() == Some(sender) => {
                self.read(out);
                true
            }
            _ => false,
        }
    }

    /// A new owner: unsubscribed, its calls forgotten; the count stays until it is read again.
    fn forget_owner_keeping_count(&mut self) {
        self.owner = None;
        self.subscribed = false;
        self.get_call = None;
        self.sub_call = None;
        self.reread = false;
    }

    /// The system count was just read (`Msg::UnitsRead`): the user count once more, which is what
    /// makes it follow a `failed-refresh`, a resume and the piggyback when the subscription is off.
    /// With no manager known, its owner is asked for again.
    pub fn on_units_read(&mut self, out: &mut impl Outbox) {
        if self.owner.is_some() {
            self.read(out);
        } else {
            self.ask_owner(out);
        }
    }

    /// The session bus closed: unknown, nothing subscribed. The link is never reconnected, so the bar
    /// keeps the count it last had (an unknown count is not a state) until the daemon restarts.
    pub fn lost(&mut self) {
        self.owner_call = None;
        self.forget_owner();
    }
}

impl App {
    /// The user manager's count, once, at startup (`wanted`: a module asks for it). It goes on the
    /// tray's connection when the tray runs; otherwise on the bar's link, opened here or joined.
    /// Without it: nothing, no rule, no call.
    pub(crate) fn start_failed(&mut self, wanted: bool) {
        let subscribe = self.bar.config.failed.subscribe;
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

    /// The user manager's count, from either connection: the bar repaints only when it changed.
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
    /// consumer, nothing.
    fn failed_do(&mut self, f: impl FnOnce(&mut UserUnits, &mut SessionBus)) {
        if let Some(tray) = self.tray.as_mut().filter(|t| t.failed().is_some()) {
            tray.failed_do(f);
            self.tray_sync();
            return;
        }
        let Some(link) = self.link.session.as_mut().filter(|l| l.failed().is_some()) else { return };
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

    /// Both counts read now, whatever the subscription. The system one by a poke of the D-Bus
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
