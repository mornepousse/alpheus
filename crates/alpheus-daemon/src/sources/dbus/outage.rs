//! Outages: why a session ended (`Outage`, tagged on the `io::Error`), a service gone, and the
//! per-domain log rate limit.

use super::*;

/// The service a call went to is gone (ServiceUnknown, NameHasNoOwner): an absence (NotFound) like
/// any other, marked so a reader can tell it from one object gone (UnknownObject).
#[derive(Debug)]
struct ServiceGone(String);

impl std::fmt::Display for ServiceGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceGone {}

pub fn service_gone(text: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, ServiceGone(text))
}

/// `e` says the service itself is gone (`service_gone`), not one of its objects.
pub fn is_service_gone(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|r| r.is::<ServiceGone>())
}

/// An object gone between two reads (a connection going down): nothing to
/// read, not an outage. `pub`: its callers are the readers.
pub fn optional(r: io::Result<Props>) -> io::Result<Option<Props>> {
    match r {
        Ok(p) => Ok(Some(p)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Why a session ended (I2): a single domain's read failing, its
/// own "–" already published while the others keep their last published
/// value — versus the bus itself going away (the connection lost, or never
/// made), which blanks all three: nothing can be trusted then. Carried by
/// the `io::Error` `session` returns, as an explicit tag (`tag`,
/// `outage_of`), never by matching the error's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outage {
    Domain(Domain),
    Bus,
}

/// Ties an `Outage` to the real error without losing it: `Display` and
/// `source` both delegate to it, only `outage_of` reads the tag back.
#[derive(Debug)]
struct Tagged {
    outage: Outage,
    source: io::Error,
}

impl std::fmt::Display for Tagged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for Tagged {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(super) fn tag(source: io::Error, outage: Outage) -> io::Error {
    io::Error::new(source.kind(), Tagged { outage, source })
}

/// The `Outage` an `io::Error` from `session` carries; `Bus` if it isn't
/// tagged (`next_signal` failing, the connection never coming up — nothing
/// ties those to one domain).
pub(super) fn outage_of(e: &io::Error) -> Outage {
    e.get_ref().and_then(|r| r.downcast_ref::<Tagged>()).map_or(Outage::Bus, |t| t.outage)
}

/// Whether a read failure is worth a log line: a real failure
/// is; an absence (NotFound: the service left after `GetNameOwner`, its
/// `NameOwnerChanged` follows and re-reads it) isn't — logging it on every
/// such ordinary comings and goings of a service would be noise. Rate
/// limiting for a failure that keeps recurring stays in `supervise`
/// (`first || succeeded`), not duplicated here.
pub(super) fn worth_logging(kind: io::ErrorKind) -> bool {
    kind != io::ErrorKind::NotFound
}

/// `d`'s index into `DomainLog`'s per-domain array.
fn domain_index(d: Domain) -> usize {
    match d {
        Domain::Network => 0,
        Domain::Bluetooth => 1,
        Domain::Battery => 2,
        Domain::Power => 3,
        Domain::Units => 4,
    }
}

/// Per-domain log rate limiting (follow-up, re-review): `read` runs again,
/// fresh, on every backoff round — a new `session`, a new `SystemState` —
/// so without state that outlives one round, a domain stuck down would
/// repeat the same log line every round, forever. One `DomainLog` lives for
/// as long as the D-Bus thread does (owned by `start`'s closure, across
/// every `run_supervised` round), not reset by a fresh `SystemState`.
#[derive(Debug, Default)]
pub struct DomainLog([bool; DOMAINS.len()]);

impl DomainLog {
    /// Whether this failure of `d` is worth a line: the first one, or the
    /// first since its last success. Records the new state either way.
    pub(super) fn note_failure(&mut self, d: Domain) -> bool {
        let i = domain_index(d);
        let first = !self.0[i];
        self.0[i] = true;
        first
    }

    /// `d` just succeeded: its next failure is worth a line again.
    pub(super) fn note_success(&mut self, d: Domain) {
        self.0[domain_index(d)] = false;
    }
}
