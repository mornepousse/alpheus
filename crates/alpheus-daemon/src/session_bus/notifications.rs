//! The `org.freedesktop.Notifications` server's D-Bus side. It owns no
//! connection: it rides whichever `Outbox` hosts it, the tray's or the bar's own session link.
//!
//! The name is requested with flags 0: queued behind mako, never replacing it, never allowing
//! replacement. It is held on reply 1 or 4, or on the bus driver's NameAcquired; it is lost on
//! NameLost or when the bus closes. While it is not held, every call on the Notifications path is
//! refused (UnknownMethod) and nothing is ingested.
//!
//! Calls are decoded raw by the codec (`codec::inbound`, image-data skipped unread) and answered by
//! the builders in bus.rs. The model is `notify::Center`; its effects become the two broadcast
//! signals, sent by the host after each call or action. This file never names the dbus crate, and
//! never names a function `method_call`: the guardrails count that substring.
//!
//! Never:
//! - replace another server: the name is asked with flags 0, so alpheus queues behind mako and
//!   takes over when it lets go (docs/decisions.md, "The notification server queues behind another
//!   server and never replaces it");
//! - read an image or a sound hint: they are stepped over while decoding
//!   (docs/features/notifications.md);
//! - panic on a call (`the_notification_server_has_no_panicking_path`), or open a connection of its
//!   own.
//!
//! See: docs/features/notifications.md ("The server"), docs/measurements.md.

use super::bus::{self, Call, CallError, CallId, Message, Outbox, refusal};
use super::codec::{self, Inbound, Reply, Signal};
use super::{NOTIFICATIONS_NAME, NOTIFICATIONS_PATH};
use crate::clock::{LocalTime, local_minutes};
use crate::config::NotificationsConfig;
use crate::log::write as log;
use crate::notify::{Center, CloseReason, Effect, NotifyView};
use std::time::Instant;

/// Whether `call` is addressed to the Notifications object: the hosts route it here, before the
/// watcher, whether the name is held or not.
pub fn serves(call: &Message) -> bool {
    call.path().is_some_and(|p| &*p == NOTIFICATIONS_PATH)
}

/// The local time in minutes since 1970 (`clock::local_minutes`), for "14:02" or "Oct 8": read
/// for a Notify served, and when the bubbles or the panel are drawn.
pub fn minute_now() -> u32 {
    let (t, _) = LocalTime::now();
    local_minutes(&t)
}

pub struct Notifications {
    center: Center,
    /// The name is ours: calls are served and effects sent.
    held: bool,
    /// The RequestName in flight.
    request: Option<CallId>,
    /// NotificationClosed / ActionInvoked waiting for the host to send them.
    effects: Vec<Message>,
    /// What `take_change` last handed out.
    shown: Option<NotifyView>,
    /// Something the view depends on may have moved since `shown`.
    stale: bool,
    /// (id, revision, critical) of the bubbles last reported.
    bubbles: Vec<(u32, u32, bool)>,
    /// The earliest expiry last reported (hidden bubbles included: the timer follows them too).
    expiry: Option<Instant>,
    /// Something the bubbles depend on may have moved.
    bubbles_stale: bool,
}

impl Notifications {
    /// Asks for the name (flags 0). No match rule: NameAcquired and NameLost are unicast from the
    /// bus driver.
    pub fn start(out: &mut impl Outbox, cfg: &NotificationsConfig, dnd: bool) -> Notifications {
        let id = out.call(Call::RequestNotifications);
        Notifications {
            center: Center::new(cfg, dnd),
            held: false,
            request: Some(id),
            effects: Vec::new(),
            shown: None,
            stale: true,
            bubbles: Vec::new(),
            expiry: None,
            bubbles_stale: false,
        }
    }

    pub fn owns(&self, id: CallId) -> bool {
        self.request == Some(id)
    }

    /// The RequestName's answer: 1 (primary owner) or 4 (already ours) hold the name; 2 means
    /// queued behind another server, dormant until NameAcquired. A lost answer changes nothing:
    /// the bus driver's NameAcquired still decides.
    pub fn answer(&mut self, id: CallId, reply: Result<Reply, CallError>) {
        if self.request != Some(id) {
            return;
        }
        self.request = None;
        match reply {
            Ok(Reply::RequestName(1 | 4)) => self.hold(true),
            Ok(Reply::RequestName(2)) => log("notifications: another server holds the name; waiting in the queue"),
            Ok(Reply::RequestName(code)) => log(&format!("notifications: the name was refused ({code})")),
            Ok(_) => {}
            Err(e) => log(&format!("notifications: the name could not be requested ({word})", word = e.word())),
        }
    }

    /// NameAcquired / NameLost for our name (the codec has already checked they come from the bus
    /// driver, to us): true when the signal was ours.
    pub fn signal(&mut self, s: &Signal) -> bool {
        match s {
            Signal::NameAcquired(name) if name == NOTIFICATIONS_NAME => {
                self.hold(true);
                true
            }
            Signal::NameLost(name) if name == NOTIFICATIONS_NAME => {
                if self.held {
                    log("notifications: the name was lost");
                }
                self.hold(false);
                true
            }
            _ => false,
        }
    }

    /// The session bus closed: the name went with it.
    pub fn lost(&mut self) {
        self.request = None;
        self.hold(false);
    }

    fn hold(&mut self, held: bool) {
        if held && !self.held {
            log("notifications: serving");
        }
        self.held = held;
        if !held {
            self.center.hide_all();
            self.effects.clear();
        }
        self.touched();
    }

    fn touched(&mut self) {
        self.stale = true;
        self.bubbles_stale = true;
    }

    /// One call on the Notifications path (`serves`): the reply to send, None when the call asked
    /// for none. Refused while the name is not held. `minute` (the local time, `minute_now`) is read
    /// only for a Notify served.
    pub fn handle(&mut self, call: &Message, now: Instant, minute: impl FnOnce() -> u32) -> Option<Message> {
        let wanted = !call.get_no_reply();
        if !self.held {
            return wanted.then(|| refusal(call));
        }
        let reply = match codec::inbound(call) {
            Inbound::Notify(n) => {
                let id = self.center.notify(n, now, minute());
                // A critical demoted by a flood ends like any bubble: NotificationClosed (1).
                let effects = self.center.take_effects();
                self.emit(effects);
                bus::notify_reply(call, id)
            }
            Inbound::Close(id) => {
                let effects = self.center.close(id, CloseReason::Closed, now);
                self.emit(effects);
                bus::empty_reply(call)
            }
            Inbound::Capabilities => bus::capabilities_reply(call),
            Inbound::ServerInfo => bus::server_info_reply(call),
            Inbound::Unknown => refusal(call),
        };
        wanted.then_some(reply)
    }

    /// The bar acts on the centre (a click, the bubble timer, the panel, a verb): its effects become
    /// signals while the name is held.
    pub fn act(&mut self, f: impl FnOnce(&mut Center) -> Vec<Effect>) {
        let effects = f(&mut self.center);
        self.emit(effects);
    }

    fn emit(&mut self, effects: Vec<Effect>) {
        if self.held {
            self.effects.extend(effects.into_iter().filter_map(|e| match e {
                Effect::Closed(id, reason) => bus::closed_signal(id, reason as u32),
                Effect::Invoked(id, key) => bus::invoked_signal(id, &key),
            }));
        }
        self.touched();
    }

    /// The signals to broadcast, taken.
    pub fn take_effects(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.effects)
    }

    /// The bell's view, when it changed since the last call (the first call always gives one).
    pub fn take_change(&mut self) -> Option<NotifyView> {
        if !std::mem::take(&mut self.stale) {
            return None;
        }
        let view = self.center.view(self.held);
        if self.shown == Some(view) {
            return None;
        }
        self.shown = Some(view);
        Some(view)
    }

    /// Whether the bubbles to show (a new one, a replaced one, one gone), or the earliest expiry (a
    /// bubble hidden by do-not-disturb replaced: the timer follows it), changed since the last call.
    /// Called once per batch of calls and per act, right before the host draws: the bubbles timed
    /// now have shown (`Center::seen`) and keep their expiry when pushed back.
    pub fn take_bubbles_change(&mut self) -> bool {
        if !std::mem::take(&mut self.bubbles_stale) {
            return false;
        }
        self.center.seen();
        let now: Vec<(u32, u32, bool)> = self.center.bubbles().iter().map(|s| (s.entry.id, s.entry.revision, s.critical)).collect();
        let expiry = self.center.next_expiry();
        if now == self.bubbles && expiry == self.expiry {
            return false;
        }
        self.bubbles = now;
        self.expiry = expiry;
        true
    }

    pub fn center(&self) -> &Center {
        &self.center
    }

    /// The centre, for a change that emits nothing; the view and the bubbles are re-checked.
    pub fn center_mut(&mut self) -> &mut Center {
        self.touched();
        &mut self.center
    }

    pub fn held(&self) -> bool {
        self.held
    }
}

#[cfg(test)]
mod tests;
