//! One session on one connection: the initial reads, the publish, then each signal.

use super::*;
use crate::system::PanelScope;
use tap::UnitsTap;
use units::{UnitsLink, UnitsOptions};

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
        Domain::Units => units::read(bus).map(|v| s.units = v),
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
        Domain::Units => s.units = None,
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
/// `units_ok` is set to whether the count's own read succeeded (an absence is a success): the
/// aggregate error may belong to another domain.
pub(super) fn read_all(
    bus: &mut dyn Bus,
    domains: &[Domain],
    s: &mut SystemState,
    log_state: &mut DomainLog,
    units_ok: &mut bool,
) -> Option<io::Error> {
    let mut failed: Option<io::Error> = None;
    *units_ok = false;
    for d in domains {
        let r = read(bus, *d, s, log_state);
        if *d == Domain::Units {
            *units_ok = r.is_ok();
            // A failed count is not a failed session (diagnostics spec § 6): the count is unknown
            // for this read, logged under the rate limit, and the other domains carry on.
            if r.as_ref().is_err_and(is_units_error) {
                continue;
            }
        }
        if let Err(e) = r {
            let already_bus = failed.as_ref().is_some_and(|f| outage_of(f) == Outage::Bus);
            if failed.is_none() || (!already_bus && outage_of(&e) == Outage::Bus) {
                failed = Some(e);
            }
        }
    }
    failed
}

/// Whether `e` is the failed-units count's own read error (not the bus going away, which is
/// tagged `Outage::Bus` and stays fatal).
fn is_units_error(e: &io::Error) -> bool {
    outage_of(e) == Outage::Domain(Domain::Units)
}

/// The count's read, made non-fatal: `Ok(true)` it was read, `Ok(false)` it failed for real (the
/// count is None, already logged; the attempt is remembered so a piggyback does not retry a
/// systemd that times out at every wake-up), any other error is returned as it is.
fn units_soft(r: io::Result<()>, link: &mut UnitsLink, now: Instant) -> io::Result<bool> {
    match r {
        Ok(()) => Ok(true),
        Err(e) if is_units_error(&e) => {
            link.attempted(now);
            Ok(false)
        }
        Err(e) => Err(e),
    }
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
fn publish_if_changed(last: &mut Option<SystemState>, s: &SystemState, publish: &mut dyn FnMut(SystemState) -> bool) -> bool {
    if last.as_ref() == Some(s) {
        return true;
    }
    let ok = publish(s.clone());
    if ok {
        *last = Some(s.clone());
    }
    ok
}

/// A panel reading's result into `slot`. Err when the bus itself went away, tagged so (every domain
/// blanked, never `domain` alone), or on any other error: `domain`'s outage, so the session ends and
/// the reconnection drops every rule, the other modules kept meanwhile. The reading is dropped
/// either way.
fn panel_into<T>(r: io::Result<Option<T>>, slot: &mut Option<T>, domain: Domain) -> io::Result<()> {
    r.map(|v| *slot = v).map_err(|e| {
        *slot = None;
        let outage = if e.kind() == io::ErrorKind::UnexpectedEof { Outage::Bus } else { Outage::Domain(domain) };
        tag(e, outage)
    })
}

/// A Wi-Fi read's result into `s`: the network domain's outage (a detail rule that could not be
/// removed).
pub(super) fn wifi_into(r: io::Result<Option<WifiState>>, s: &mut SystemState) -> io::Result<()> {
    panel_into(r, &mut s.wifi, Domain::Network)
}

/// A Bluetooth read's result into `s`: the Bluetooth domain's outage.
pub(super) fn bt_into(r: io::Result<Option<BtView>>, s: &mut SystemState) -> io::Result<()> {
    panel_into(r, &mut s.bt, Domain::Bluetooth)
}

/// A VPN read's result into `s`: the network domain's outage (NetworkManager is where it is read).
pub(super) fn vpn_into(r: io::Result<Option<VpnState>>, s: &mut SystemState) -> io::Result<()> {
    panel_into(r, &mut s.vpn, Domain::Network)
}

/// What a tap asks (`Tap::snapshot`), read through `rescope`; `open` the open last read for. A close
/// and a reopen read as one poke (the scope unchanged) are still a new open: closed first, so
/// everything is read again; its state tagged with the new open, and so published even when it reads
/// the same. A reader still at its default reads nothing when closed.
fn asked<T>(
    (scope, now): (PanelScope, u64),
    open: &mut u64,
    mut rescope: impl FnMut(PanelScope) -> io::Result<Option<T>>,
) -> io::Result<Option<T>> {
    let reopened = now != *open && scope != PanelScope::Closed;
    *open = now;
    if reopened { rescope(PanelScope::Closed).and_then(|_| rescope(scope)) } else { rescope(scope) }
}

/// What the session does after a read of the failed-units count (`domains` holds `Units` and the
/// read did not fail): the read is remembered for the piggyback, systemd is subscribed to when
/// wanted, and the loop is told the count was just read (`units_read`, outside the compared
/// state: two reads of the same count are two words, one publication). False: the loop is gone.
fn units_done(link: &mut UnitsLink, bus: &mut dyn Bus, count: Option<u32>, units_read: &mut dyn FnMut() -> bool) -> io::Result<bool> {
    let now = bus.now();
    link.read_done(bus, count, now)?;
    Ok(units_read())
}

/// Whether a tap's (scope, opens) differs from what was last read for it, which then catches up.
fn spoke(now: (PanelScope, u64), seen: &mut (PanelScope, u64)) -> bool {
    std::mem::replace(seen, now) != now
}

/// `wifi`, `bt`: what the open quick panel asks (quick Wi-Fi spec § 5): read when the session starts,
/// again on each of its pokes and with every network re-read; Closed reads nothing. `units`: the
/// loop's word "read the failed-units count now"; `opts`: whether to `Subscribe` to systemd and
/// how long another domain's wake-up waits before it reads the count too; `units_read`: told
/// after each successful read of the count (never part of the published state).
#[allow(clippy::too_many_arguments)]
pub fn session(
    bus: &mut dyn Bus,
    domains: &[Domain],
    log_state: &mut DomainLog,
    wifi: &WifiTap,
    bt: &BtTap,
    vpn: &VpnTap,
    units: &UnitsTap,
    opts: UnitsOptions,
    publish: &mut dyn FnMut(SystemState) -> bool,
    units_read: &mut dyn FnMut() -> bool,
) -> io::Result<()> {
    let mut s = SystemState::default();
    let mut link = UnitsLink::new(opts);
    let mut units_seen = units.pokes();
    let mut reader = WifiReader::default();
    let mut bt_reader = BtReader::default();
    let mut vpn_reader = VpnReader::default();
    let mut last = None;
    let mut initial_units_ok = false;
    let mut failed = read_all(bus, domains, &mut s, log_state, &mut initial_units_ok);
    if domains.contains(&Domain::Units) && !initial_units_ok && failed.is_none() {
        link.attempted(bus.now());
    }
    // A panel already open when this session starts (a reconnection): its Wi-Fi read now. A word
    // left waiting from the previous session is answered by this read: drained, never read twice.
    wifi.drain();
    let mut wifi_seen = wifi.snapshot();
    let r = asked(wifi_seen, &mut s.wifi_open, |scope| reader.rescope(bus, scope));
    if let Err(e) = wifi_into(r, &mut s) {
        failed = Some(e);
    }
    bt.drain();
    let mut bt_seen = bt.snapshot();
    let r = asked(bt_seen, &mut s.bt_open, |scope| bt_reader.rescope(bus, scope));
    if let Err(e) = bt_into(r, &mut s) {
        failed = Some(e);
    }
    vpn.drain();
    let mut vpn_seen = vpn.snapshot();
    let r = asked(vpn_seen, &mut s.vpn_open, |scope| vpn_reader.rescope(bus, scope));
    if let Err(e) = vpn_into(r, &mut s) {
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
    // The count was read: the loop is told, and systemd subscribed to (after the publish, so the
    // wait for the bus never delays the bar's first state).
    if initial_units_ok {
        match units_done(&mut link, bus, s.units, units_read) {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(e) => failed = Some(e),
        }
    }
    if let Some(e) = failed {
        return Err(e);
    }
    // The one activation (module doc): after the publish, so its wait never delays the others.
    if domains.contains(&Domain::Battery) {
        upower::activate_if_absent(bus, &mut |m| log(m));
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
            // A poke does not say which tap spoke: each is read only when its own scope or its
            // opens moved since it was last read, so the failed-units word re-reads no panel.
            let snap = wifi.snapshot();
            let r = if spoke(snap, &mut wifi_seen) {
                wifi_into(asked(snap, &mut s.wifi_open, |scope| reader.rescope(bus, scope)), &mut s)
            } else {
                Ok(())
            };
            let snap = bt.snapshot();
            let b = if spoke(snap, &mut bt_seen) {
                bt_into(asked(snap, &mut s.bt_open, |scope| bt_reader.rescope(bus, scope)), &mut s)
            } else {
                Ok(())
            };
            let snap = vpn.snapshot();
            let v = if spoke(snap, &mut vpn_seen) {
                vpn_into(asked(snap, &mut s.vpn_open, |scope| vpn_reader.rescope(bus, scope)), &mut s)
            } else {
                Ok(())
            };
            // The loop's `failed-refresh`: the count again, now.
            let poked = domains.contains(&Domain::Units) && units.poked_since(&mut units_seen);
            let mut u = poked.then(|| units_soft(read(bus, Domain::Units, &mut s, log_state), &mut link, bus.now()));
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            match u {
                Some(Ok(true)) => match units_done(&mut link, bus, s.units, units_read) {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(e) => u = Some(Err(e)),
                },
                // A failed or skipped system read: the user manager's count is its own link's, so
                // `--failed-refresh` still reads it (the loop's word, no D-Bus state touched).
                Some(_) if !units_read() => return Ok(()),
                _ => {}
            }
            // The panel closed: what this thread's arena held for it goes back, as at startup.
            if reader.scope() == WifiScope::Closed && bt_reader.scope() == BtScope::Closed && vpn_reader.scope() == VpnScope::Closed {
                release_memory();
            }
            r?;
            b?;
            v?;
            u.transpose()?;
            continue;
        }
        if signal.is_wake() {
            // The machine just resumed: anything may have changed while it
            // slept (a VPN reconnected, a Bluetooth device, the battery) —
            // re-read every shown domain, not just the one a match rule
            // would otherwise point to.
            let mut units_ok = false;
            let mut failed = read_all(bus, domains, &mut s, log_state, &mut units_ok);
            if domains.contains(&Domain::Units) && !units_ok && failed.is_none() {
                link.attempted(bus.now());
            }
            if let Err(e) = wifi_into(reader.reread(bus), &mut s) {
                failed = Some(e);
            }
            if let Err(e) = bt_into(bt_reader.reread(bus), &mut s) {
                failed = Some(e);
            }
            if let Err(e) = vpn_into(vpn_reader.reread(bus), &mut s) {
                failed = Some(e);
            }
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            if units_ok {
                match units_done(&mut link, bus, s.units, units_read) {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(e) => failed = Some(e),
                }
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
        // A new owner of systemd1 (a `daemon-reexec`, a restart) has no subscriber yet.
        if d == Domain::Units && signal.member == "NameOwnerChanged" {
            link.new_owner();
        }
        let mut units_read_ok = false;
        let r = read(bus, d, &mut s, log_state);
        let r = if d == Domain::Units { units_soft(r, &mut link, bus.now()).map(|ok| units_read_ok = ok) } else { r };
        // The count rides another domain's wake-up when it was last read `opts.piggyback` ago or
        // more: no timer, no wake-up of its own. Not when subscribed: systemd then pushes it.
        let piggyback = d != Domain::Units && domains.contains(&Domain::Units) && !link.subscribed() && link.due(bus.now());
        let p = piggyback.then(|| units_soft(read(bus, Domain::Units, &mut s, log_state), &mut link, bus.now()));
        let w = if d == Domain::Network { wifi_into(reader.reread(bus), &mut s) } else { Ok(()) };
        // The same re-read for an open VPN reading (the actives, not the list at the row).
        let v = if d == Domain::Network { vpn_into(vpn_reader.reread(bus), &mut s) } else { Ok(()) };
        // BlueZ itself changed (its owner, the adapter's or a battery's properties): the panel's
        // view is read again on the owner now there.
        let b = if d == Domain::Bluetooth && change.is_none() { bt_into(bt_reader.reread(bus), &mut s).and(b) } else { b };
        if !publish_if_changed(&mut last, &s, publish) {
            return Ok(());
        }
        let mut units_err = None;
        if units_read_ok || matches!(p, Some(Ok(true))) {
            match units_done(&mut link, bus, s.units, units_read) {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(e) => units_err = Some(e),
            }
        }
        // The Wi-Fi read fails only with the bus itself gone: that outage wins over the domain's.
        w?;
        v?;
        b?;
        r?;
        p.transpose()?;
        if let Some(e) = units_err {
            return Err(e);
        }
    }
}

/// The bar's messages for a published state; None (the whole bus down): "–" for every module, no
/// power profile, no Wi-Fi, no VPN, no failed-units count.
pub fn messages(s: Option<SystemState>) -> [Msg; 8] {
    match s {
        Some(s) => [
            Msg::Network(s.network),
            Msg::Bluetooth(s.bluetooth),
            Msg::Battery(s.battery),
            Msg::Profile(s.power),
            Msg::Wifi { open: Some(s.wifi_open), state: s.wifi },
            Msg::Bt { open: Some(s.bt_open), state: s.bt },
            Msg::Vpn { open: Some(s.vpn_open), state: s.vpn },
            Msg::Units(s.units),
        ],
        None => [
            Msg::Network(None),
            Msg::Bluetooth(None),
            Msg::Battery(None),
            Msg::Profile(None),
            Msg::Wifi { open: None, state: None },
            Msg::Bt { open: None, state: None },
            Msg::Vpn { open: None, state: None },
            Msg::Units(None),
        ],
    }
}
