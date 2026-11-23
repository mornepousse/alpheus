//! The supervisor of the D-Bus thread: sessions again with a growing delay.
//!
//! Never:
//! - retry without a delay: the delay grows to its cap, so a bus that is down costs nothing;
//! - keep a thread while nothing is wanted: the thread exists only while a module needs a domain
//!   (`nothing_resident_while_the_launcher_is_closed` counts it).
//!
//! See: docs/ARCHITECTURE.md ("Threads"), docs/decisions.md ("A source that is down is not a
//! state").

use super::*;

/// Like `sources::supervise`, specialized for the D-Bus source: the "–" shown during the backoff delay depends on why the
/// session ended — a single domain's outage (`Outage::Domain`) keeps the
/// others' last published value (the session already showed "–" for just
/// that one domain), the bus itself going away (`Outage::Bus`) blanks all
/// three, since nothing can be trusted then. Otherwise the same rules as
/// `sources::supervise`: growing delay, a panic handled like an outage, a
/// stable session resets the backoff.
pub(super) fn run_supervised(
    mut session: impl FnMut(&mut dyn FnMut(SystemState) -> bool) -> io::Result<()>,
    send: &(impl Fn(Option<SystemState>) -> bool + ?Sized),
    mut sleep: impl FnMut(Duration),
    mut now: impl FnMut() -> Instant,
    mut log: impl FnMut(&str),
) {
    let mut backoff = Backoff::new();
    let mut first = true;
    let mut last = SystemState::default();
    loop {
        let mut publications = 0u32;
        let mut closed = false;
        let start = now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            session(&mut |s: SystemState| {
                publications += 1;
                last = s.clone();
                let ok = send(Some(s));
                closed |= !ok;
                ok
            })
        }));
        let r: io::Result<()> = result.unwrap_or_else(|payload| Err(io::Error::other(format!("panic: {}", panic_text(&*payload)))));
        if closed {
            return;
        }
        let elapsed = now().saturating_duration_since(start);
        let succeeded = publications >= 2 || (publications >= 1 && elapsed >= STABLE_DURATION);
        if succeeded {
            backoff.on_success();
        }
        let down = match &r {
            Err(e) => {
                if first || succeeded {
                    log(&format!("source down: {e}"));
                }
                match outage_of(e) {
                    // Never the Wi-Fi last read on NetworkManager's own outage:
                    // a detail's lists, stale by the time the session is back; the panel
                    // hears none until it is. Another domain's outage keeps it, marked
                    // stale: read before the outage, it may no longer hold by the backoff's
                    // end, so an attempt never deletes on it; the next session reads it afresh.
                    // The VPN likewise: read on NetworkManager, none until the next session reads it.
                    Outage::Domain(Domain::Network) => Some(SystemState { wifi: None, vpn: None, ..last.clone() }),
                    // BlueZ's own outage: the panel's Bluetooth view is dropped (nothing to trust
                    // until the next session reads it), the Wi-Fi kept stale as any other's.
                    Outage::Domain(Domain::Bluetooth) => {
                        let mut kept = SystemState { bt: None, ..last.clone() };
                        if let Some(w) = kept.wifi.as_mut() {
                            w.stale = true;
                        }
                        Some(kept)
                    }
                    Outage::Domain(_) => {
                        let mut kept = last.clone();
                        if let Some(w) = kept.wifi.as_mut() {
                            w.stale = true;
                        }
                        Some(kept)
                    }
                    Outage::Bus => None,
                }
            }
            Ok(()) => None,
        };
        first = false;
        if !send(down) {
            return;
        }
        sleep(backoff.next_delay());
    }
}
