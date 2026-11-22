//! systemd's failed-units count (diagnostics spec § 6): `NFailedUnits` of the Manager object, read
//! through the owner's unique name like every other domain, and nothing else. Never `GetAll` (the
//! Manager's own carries its `Environment`, which may hold secrets), never a unit name: the daemon
//! counts, it does not list.
//!
//! Systemd sends the Manager's `PropertiesChanged` only while some client has called `Subscribe`,
//! so the session subscribes once after its first successful read (`UnitsLink`), only when
//! `[failed] subscribe` is on (it is off by default). The triggers that hold whatever the subscription (resume, a poke,
//! a piggyback on another domain's wake-up) re-read it all the same.

use super::{Bus, SYSTEMD1};
use crate::log::write as log;
use std::io;
use std::time::{Duration, Instant};

pub const PATH: &str = "/org/freedesktop/systemd1";
pub const MANAGER: &str = "org.freedesktop.systemd1.Manager";
/// The one property read.
pub const PROPERTY: &str = "NFailedUnits";
/// How long after the last read another domain's wake-up reads the count once more.
pub const PIGGYBACK: Duration = Duration::from_secs(60);

/// What the session is told about the count (`[failed]`; tests pass a short interval).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitsOptions {
    /// Call `Subscribe` once per owner so that systemd sends the change at once (`[failed]
    /// subscribe`, off by default).
    pub subscribe: bool,
    /// The piggyback interval.
    pub piggyback: Duration,
}

impl Default for UnitsOptions {
    fn default() -> Self {
        UnitsOptions { subscribe: false, piggyback: PIGGYBACK }
    }
}

/// The system manager's count of failed units; None: nobody owns `org.freedesktop.systemd1`.
/// `Get` of that one property, never `GetAll`.
pub fn read(bus: &mut dyn Bus) -> io::Result<Option<u32>> {
    let Some(owner) = bus.owner(SYSTEMD1)? else { return Ok(None) };
    bus.get_u32(&owner, PATH, MANAGER, PROPERTY).map(Some)
}

/// Whether a wake-up for another domain reads the count once more: it was never read, or last
/// read `interval` ago or more.
pub fn should_piggyback(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= interval)
}

/// What a session keeps about the count: when it was last read and whether systemd has been told
/// to send its signals.
#[derive(Debug)]
pub struct UnitsLink {
    opts: UnitsOptions,
    last: Option<Instant>,
    sub: Sub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    /// Not sent for this owner yet.
    Unsent,
    /// Sent and accepted.
    Done,
    /// Sent and refused or timed out: not asked again for this owner.
    Refused,
}

impl UnitsLink {
    pub fn new(opts: UnitsOptions) -> UnitsLink {
        UnitsLink { opts, last: None, sub: Sub::Unsent }
    }

    /// The count was just read (`count`: None when nobody owns systemd1) at `now`: remembered, and
    /// `Subscribe` sent when it is wanted, the owner is there and it has not been for this owner.
    /// Err: the bus went away.
    pub fn read_done(&mut self, bus: &mut dyn Bus, count: Option<u32>, now: Instant) -> io::Result<()> {
        self.last = Some(now);
        if count.is_none() || !self.opts.subscribe || self.sub != Sub::Unsent {
            return Ok(());
        }
        let Some(owner) = bus.owner(SYSTEMD1)? else { return Ok(()) };
        match bus.subscribe_units(&owner) {
            Ok(()) => self.sub = Sub::Done,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
            // A `daemon-reexec` keeps our subscription: the second `Subscribe` is told so.
            Err(e) if e.to_string().starts_with("org.freedesktop.systemd1.AlreadySubscribed") => self.sub = Sub::Done,
            Err(_) => {
                self.sub = Sub::Refused;
                log("D-Bus Units: Subscribe refused");
            }
        }
        Ok(())
    }

    /// The count's read failed at `now`: remembered like a read, so a piggyback waits its interval
    /// before asking a systemd that does not answer again.
    pub fn attempted(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// systemd1 changed owner (a `daemon-reexec`, a restart): the new one has no subscriber.
    pub fn new_owner(&mut self) {
        self.sub = Sub::Unsent;
    }

    /// Whether another domain's wake-up reads the count now.
    pub fn due(&self, now: Instant) -> bool {
        should_piggyback(self.last, now, self.opts.piggyback)
    }

    /// Whether `Subscribe` was accepted for the current owner.
    pub fn subscribed(&self) -> bool {
        self.sub == Sub::Done
    }
}
