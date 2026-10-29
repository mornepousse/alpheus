//! One session on one connection: the initial reads, the publish, then each signal.

use super::*;

/// Re-reads `d` into `s`. Err: the read failed for real (a timeout…), the
/// domain is now None, tagged `Outage::Domain(d)` (I2) so a caller
/// can blank just that domain rather than the whole state — unless the
/// failure is the bus itself going away mid-call (`UnexpectedEof`), tagged
/// `Outage::Bus` instead: nothing can be
/// trusted then, not just `d`. An absence (NotFound) also gives None, but is
/// not an error.
pub(super) fn read(bus: &mut dyn Bus, d: Domain, s: &mut SystemState, log_state: &mut DomainLog) -> io::Result<()> {
    let r = match d {
        Domain::Network => network::read(bus).map(|v| s.network = Some(v)),
        Domain::Bluetooth => bluez::read(bus).map(|v| s.bluetooth = Some(v)),
        Domain::Battery => upower::read(bus).map(|v| s.battery = Some(v)),
        Domain::Power => power::read(bus).map(|v| s.power = Some(v)),
    };
    let Err(e) = r else {
        log_state.note_success(d);
        return Ok(());
    };
    match d {
        Domain::Network => s.network = None,
        Domain::Bluetooth => s.bluetooth = None,
        Domain::Battery => s.battery = None,
        Domain::Power => s.power = None,
    }
    // Rate limited per domain (follow-up, re-review): NotFound never logs
    // (worth_logging), and a short-circuiting `&&` means it never touches
    // `log_state` either — an absence doesn't arm or disarm the limiter.
    if worth_logging(e.kind()) && log_state.note_failure(d) {
        log(&format!("D-Bus {d:?}: {e}"));
    }
    if e.kind() == io::ErrorKind::NotFound {
        return Ok(());
    }
    // A bus disconnect mid-call is the
    // whole connection going away, not this domain alone: tagging it
    // `Outage::Domain(d)` would leave the other domains showing their last
    // value for a whole backoff round after nothing can be trusted any more.
    let outage = if e.kind() == io::ErrorKind::UnexpectedEof { Outage::Bus } else { Outage::Domain(d) };
    Err(tag(e, outage))
}

/// Reads every domain in `domains` into `s`; the first real failure (not an
/// absence), if any — unless a later failure is the bus itself going away
/// (`Outage::Bus`): that one always wins over an
/// earlier `Outage::Domain`, stale the moment the connection itself is
/// gone. Used both for the initial state and for logind's
/// `PrepareForSleep(false)`, which re-reads all of them at once.
pub(super) fn read_all(bus: &mut dyn Bus, domains: &[Domain], s: &mut SystemState, log_state: &mut DomainLog) -> Option<io::Error> {
    let mut failed: Option<io::Error> = None;
    for d in domains {
        if let Err(e) = read(bus, *d, s, log_state) {
            let already_bus = failed.as_ref().is_some_and(|f| outage_of(f) == Outage::Bus);
            if failed.is_none() || (!already_bus && outage_of(&e) == Outage::Bus) {
                failed = Some(e);
            }
        }
    }
    failed
}

/// Reads the wanted domains, publishes, then waits for signals: each one of
/// a wanted domain re-reads it and publishes the whole state. Returns Err
/// when the bus goes away or a domain's read fails (after publishing it as
/// "–"): the supervisor shows "–" and retries with a growing delay.
/// `log_state` rate-limits each domain's own failure log line (follow-up,
/// re-review) — it must outlive this one call to do that, so it's the
/// caller's (`start`'s closure persists across every backoff round;
/// `session`'s own locals, like `s`, do not).
/// Calls `publish` only when `s` differs from the last state actually
/// published: a signal that passed `relevant`'s allowlist
/// can still leave every field `session` tracks unchanged (two different
/// allowed properties parsing to the same `SystemState`, a duplicate
/// signal…), and publishing it anyway would reach the bar's channel, and
/// cost a redraw, for nothing. `last` starts at `None`, which no real
/// `SystemState` equals, so the very first call always publishes — the bar
/// must leave "…" (Waiting) even when every domain reads as absent.
/// Returns what `publish` returned, or `true` (nothing to report) when
/// skipped; `last` is only updated on an actual, accepted publish.
pub(super) fn publish_if_changed(last: &mut Option<SystemState>, s: &SystemState, publish: &mut dyn FnMut(SystemState) -> bool) -> bool {
    if last.as_ref() == Some(s) {
        return true;
    }
    let ok = publish(s.clone());
    if ok {
        *last = Some(s.clone());
    }
    ok
}

/// A Wi-Fi read's result into `s`. Err when the bus itself went away, tagged so (every domain
/// blanked, never the network domain alone), or when a detail rule could not be removed: the
/// network domain's outage, so the session ends and the reconnection drops
/// every rule, the other modules kept meanwhile.
pub(super) fn wifi_into(r: io::Result<Option<WifiState>>, s: &mut SystemState) -> io::Result<()> {
    match r {
        Ok(w) => {
            s.wifi = w;
            Ok(())
        }
        Err(e) => {
            s.wifi = None;
            let outage = if e.kind() == io::ErrorKind::UnexpectedEof { Outage::Bus } else { Outage::Domain(Domain::Network) };
            Err(tag(e, outage))
        }
    }
}

/// A Bluetooth read's result into `s`, as `wifi_into`: the bus going away is a bus outage, any other
/// error the Bluetooth domain's (the view dropped either way).
pub(super) fn bt_into(r: io::Result<Option<BtView>>, s: &mut SystemState) -> io::Result<()> {
    match r {
        Ok(v) => {
            s.bt = v;
            Ok(())
        }
        Err(e) => {
            s.bt = None;
            let outage = if e.kind() == io::ErrorKind::UnexpectedEof { Outage::Bus } else { Outage::Domain(Domain::Bluetooth) };
            Err(tag(e, outage))
        }
    }
}

/// `wifi`, `bt`: what the open quick panel asks (quick Wi-Fi spec § 5): read when the session starts,
/// again on each of its pokes and with every network re-read; Closed reads nothing.
pub fn session(
    bus: &mut dyn Bus,
    domains: &[Domain],
    log_state: &mut DomainLog,
    wifi: &WifiTap,
    bt: &BtTap,
    publish: &mut dyn FnMut(SystemState) -> bool,
) -> io::Result<()> {
    let mut s = SystemState::default();
    let mut reader = WifiReader::default();
    let mut bt_reader = BtReader::default();
    let mut last = None;
    let mut failed = read_all(bus, domains, &mut s, log_state);
    // A panel already open when this session starts (a reconnection): its Wi-Fi read now. A word
    // left waiting from the previous session is answered by this read: drained, never read twice.
    wifi.drain();
    let (scope, open) = wifi.snapshot();
    s.wifi_open = open;
    if let Err(e) = wifi_into(reader.rescope(bus, scope), &mut s) {
        failed = Some(e);
    }
    bt.drain();
    let (bt_scope, bt_now) = bt.snapshot();
    s.bt_open = bt_now;
    if let Err(e) = bt_into(bt_reader.rescope(bus, bt_scope), &mut s) {
        failed = Some(e);
    }
    if !publish_if_changed(&mut last, &s, publish) {
        return Ok(());
    }
    // Right after the initial publish (follow-up, re-review): the startup
    // GetAll/GetManagedObjects/EnumerateDevices replies are done allocating
    // by now, and `malloc_trim` is cheap (a few µs) — freeing here, rather
    // than waiting for a first signal that may be seconds or minutes away,
    // gives the lower startup footprint (budget: 15 MB) sooner.
    release_memory();
    if let Some(e) = failed {
        return Err(e);
    }
    // A second, equally cheap call once the first signal's own read off the
    // socket (inside `next_signal`) has had its chance to grow a buffer of
    // its own — kept alongside the one above rather than instead of it.
    let mut drained = false;
    loop {
        let signal = bus.next_signal()?;
        if !drained {
            release_memory();
            drained = true;
        }
        if signal.is_poke() {
            // A close and a reopen read as one poke (the scope unchanged) are still a new open:
            // closed first, so everything is read again; its state tagged with the new open, and so
            // published even when it reads the same.
            let (scope, open) = wifi.snapshot();
            let reopened = open != s.wifi_open && scope != WifiScope::Closed;
            s.wifi_open = open;
            let r = if reopened {
                reader.rescope(bus, WifiScope::Closed).and_then(|_| reader.rescope(bus, scope))
            } else {
                reader.rescope(bus, scope)
            };
            let r = wifi_into(r, &mut s);
            // The same for Bluetooth, read whichever tap spoke (a poke does not say which).
            let (bscope, bopen) = bt.snapshot();
            let breopened = bopen != s.bt_open && bscope != BtScope::Closed;
            s.bt_open = bopen;
            let b = if breopened {
                bt_reader.rescope(bus, BtScope::Closed).and_then(|_| bt_reader.rescope(bus, bscope))
            } else {
                bt_reader.rescope(bus, bscope)
            };
            let b = bt_into(b, &mut s);
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            // The panel closed: what this thread's arena held for it goes back, as at startup.
            if reader.scope() == WifiScope::Closed && bt_reader.scope() == BtScope::Closed {
                release_memory();
            }
            r?;
            b?;
            continue;
        }
        if signal.is_wake() {
            // The machine just resumed: anything may have changed while it
            // slept (a VPN reconnected, a Bluetooth device, the battery) —
            // re-read every shown domain, not just the one a match rule
            // would otherwise point to.
            let mut failed = read_all(bus, domains, &mut s, log_state);
            if let Err(e) = wifi_into(reader.reread(bus), &mut s) {
                failed = Some(e);
            }
            if let Err(e) = bt_into(bt_reader.reread(bus), &mut s) {
                failed = Some(e);
            }
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            if let Some(e) = failed {
                return Err(e);
            }
            continue;
        }
        // A detail signal (its rules are on only while the detail is open): never a domain's
        // re-read, an access point's change reads that access point only.
        let change = signal.wifi_change();
        if change.is_some() || under(&signal.path, wifi::AP_ROOT) {
            if let Some(change) = change
                && reader.scope() == WifiScope::Detail
            {
                let r = wifi_into(reader.changed(bus, &change), &mut s);
                if !publish_if_changed(&mut last, &s, publish) {
                    return Ok(());
                }
                r?;
            }
            continue;
        }
        // A Bluetooth panel signal: never `continue`d (Connected matters to the bar as well).
        let change = signal.bt_change();
        let b = match &change {
            // `Ok(None)`: nothing read (the row, a device outside the detail): `s.bt` is left as it is.
            Some(c) => match bt_reader.changed(bus, c) {
                Ok(None) => Ok(()),
                r => bt_into(r, &mut s),
            },
            None => Ok(()),
        };
        let Some(d) = signal.domain().filter(|d| domains.contains(d)) else {
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            b?;
            continue;
        };
        let r = read(bus, d, &mut s, log_state);
        let w = if d == Domain::Network { wifi_into(reader.reread(bus), &mut s) } else { Ok(()) };
        // BlueZ itself changed (its owner, the adapter's or a battery's properties): the panel's
        // view is read again on the owner now there.
        let b = if d == Domain::Bluetooth && change.is_none() { bt_into(bt_reader.reread(bus), &mut s).and(b) } else { b };
        if !publish_if_changed(&mut last, &s, publish) {
            return Ok(());
        }
        // The Wi-Fi read fails only with the bus itself gone: that outage wins over the domain's.
        w?;
        b?;
        r?;
    }
}

/// The bar's messages for a published state; None (the whole bus down): "–" for every module, no
/// power profile, no Wi-Fi.
pub fn messages(s: Option<SystemState>) -> [Msg; 6] {
    match s {
        Some(s) => [
            Msg::Network(s.network),
            Msg::Bluetooth(s.bluetooth),
            Msg::Battery(s.battery),
            Msg::Profile(s.power),
            Msg::Wifi { open: Some(s.wifi_open), state: s.wifi },
            Msg::Bt { open: Some(s.bt_open), state: s.bt },
        ],
        None => [
            Msg::Network(None),
            Msg::Bluetooth(None),
            Msg::Battery(None),
            Msg::Profile(None),
            Msg::Wifi { open: None, state: None },
            Msg::Bt { open: None, state: None },
        ],
    }
}
