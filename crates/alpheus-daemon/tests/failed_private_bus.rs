//! The user manager's failed-units count on a private session bus (`common::PrivateBus`, nothing
//! activatable), with `common::FakeSystemd` standing for `systemd --user`: never the workstation's
//! buses. Ignored by default:
//! `nice -n 19 nix develop -c env CARGO_BUILD_JOBS=4 cargo test -p alpheus-daemon --test failed_private_bus -- --ignored`

mod common;

use alpheus_daemon::link::{LinkOutput, SessionLink};
use alpheus_daemon::session_bus::bus::SessionBus;
use alpheus_daemon::session_bus::router::{Tray, TrayOutput};
use common::{FakeSystemd, PrivateBus};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

/// Pumps `link` as the bar's loop does (readable, then any due deadline) for `within`, collecting
/// the user counts it reports, until `done` holds on them.
fn pump(link: &mut SessionLink, within: Duration, seen: &mut Vec<Option<u32>>, done: impl Fn(&[Option<u32>]) -> bool) {
    let end = Instant::now() + within;
    while !done(seen) && Instant::now() < end {
        let mut out = link.on_readable();
        if out == LinkOutput::default() && link.next_deadline().is_some_and(|d| d <= Instant::now()) {
            out = link.on_deadline(Instant::now());
        }
        seen.extend(out.user_units);
        thread::sleep(Duration::from_millis(5));
    }
}

fn link_on(bus: &PrivateBus, subscribe: bool) -> SessionLink {
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    link.start_failed(subscribe);
    link
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_user_count_follows_a_subscribed_user_manager() {
    let bus = PrivateBus::start("failed-follow");
    let systemd = FakeSystemd::serve(&bus, 1);
    let mut link = link_on(&bus, true);
    let mut seen = Vec::new();
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| s == [Some(1)]);
    assert_eq!(seen, [Some(1)], "read through the owner's unique name");
    systemd.wait_until("the subscription", |s| s.subscribers() == 1);
    systemd.set_failed(3);
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| s.len() == 2);
    assert_eq!(seen, [Some(1), Some(3)], "pushed by PropertiesChanged: it only comes to a subscriber");
    assert_eq!(systemd.calls("GetAll"), 0, "the Manager's Environment is never asked for");
    assert_eq!(systemd.calls("Unsubscribe"), 0);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_user_manager_restart_resubscribes() {
    let bus = PrivateBus::start("failed-restart");
    let systemd = FakeSystemd::serve(&bus, 1);
    let mut link = link_on(&bus, true);
    let mut seen = Vec::new();
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| !s.is_empty());
    systemd.wait_until("the subscription", |s| s.subscribers() == 1);
    systemd.restart();
    // The new owner has no subscriber until we subscribe again.
    pump(&mut link, Duration::from_secs(5), &mut seen, |_| systemd.subscribers() == 1);
    assert_eq!(systemd.subscribers(), 1, "subscribed again to the new owner");
    systemd.set_failed(5);
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| s.last() == Some(&Some(5)));
    assert_eq!(seen.last(), Some(&Some(5)), "and pushed again: {seen:?}");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_refused_subscribe_still_reads_and_a_system_read_follows_it() {
    let bus = PrivateBus::start("failed-refused");
    let systemd = FakeSystemd::serve_refusing(&bus, 1);
    let mut link = link_on(&bus, true);
    let mut seen = Vec::new();
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| !s.is_empty());
    assert_eq!(seen, [Some(1)]);
    assert_eq!(systemd.subscribers(), 0);
    systemd.set_failed(2);
    pump(&mut link, Duration::from_millis(500), &mut seen, |_| false);
    assert_eq!(seen, [Some(1)], "nothing is pushed to a client that is not subscribed");
    // The system count was read (`Msg::UnitsRead`): the user count is asked for again.
    link.failed_do(|u, out| u.on_units_read(out));
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| s.len() == 2);
    assert_eq!(seen, [Some(1), Some(2)]);
    assert_eq!(systemd.calls("Subscribe"), 1, "refused once for this owner, never asked again");
}

/// The `failed-refresh` half that runs on the loop (`App::failed_refresh` also pokes the D-Bus
/// thread, covered with the system bus): with the subscription off, `set_failed` shows nothing
/// until the loop asks.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn failed_refresh_rereads_the_user_count_unsubscribed() {
    let bus = PrivateBus::start("failed-refresh");
    let systemd = FakeSystemd::serve(&bus, 0);
    let mut link = link_on(&bus, false);
    let mut seen = Vec::new();
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| !s.is_empty());
    assert_eq!(seen, [Some(0)]);
    assert_eq!(systemd.calls("Subscribe"), 0, "[failed] subscribe = false");
    systemd.set_failed(1);
    pump(&mut link, Duration::from_millis(500), &mut seen, |_| false);
    assert_eq!(seen, [Some(0)], "no subscription, no push");
    link.failed_do(|u, out| u.on_units_read(out));
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| s.len() == 2);
    assert_eq!(seen, [Some(0), Some(1)]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn no_user_manager_is_unknown_until_it_arrives() {
    let bus = PrivateBus::start("failed-absent");
    let mut link = link_on(&bus, true);
    let mut seen = Vec::new();
    pump(&mut link, Duration::from_millis(500), &mut seen, |_| false);
    assert_eq!(seen, [None], "the first answer is told, unknown: the notice waits for it, then nothing more");
    let systemd = FakeSystemd::serve(&bus, 2);
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| s.len() == 2);
    assert_eq!(seen, [None, Some(2)], "its NameOwnerChanged is followed");
    systemd.wait_until("the subscription", |s| s.subscribers() == 1);
    drop(systemd);
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| s.len() == 3);
    assert_eq!(seen, [None, Some(2), None], "gone: unknown");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn on_the_tray_s_connection_too() {
    let bus = PrivateBus::start("failed-tray");
    let systemd = FakeSystemd::serve(&bus, 1);
    let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), PathBuf::from("/proc"));
    tray.start_failed(true);
    let mut seen = Vec::new();
    let mut pump = |seen: &mut Vec<Option<u32>>, n: usize| {
        let end = Instant::now() + Duration::from_secs(5);
        while seen.len() < n && Instant::now() < end {
            let mut out: TrayOutput = tray.on_readable();
            if tray.next_deadline().is_some_and(|d| d <= Instant::now()) {
                out.merge(tray.on_deadline(Instant::now()));
            }
            out.merge(tray.take_deferred());
            seen.extend(out.user_units);
            thread::sleep(Duration::from_millis(5));
        }
    };
    pump(&mut seen, 1);
    assert_eq!(seen, [Some(1)]);
    systemd.wait_until("the subscription", |s| s.subscribers() == 1);
    systemd.set_failed(2);
    pump(&mut seen, 2);
    assert_eq!(seen, [Some(1), Some(2)]);
}

/// The two connections never both carry it: one `start_failed` on a link that already follows it
/// asks nothing more (the bar's `host` picks one bus; this is the once-only guard).
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn start_failed_twice_is_one_follower() {
    let bus = PrivateBus::start("failed-twice");
    let systemd = FakeSystemd::serve(&bus, 1);
    let mut link = link_on(&bus, true);
    link.start_failed(true);
    let mut seen = Vec::new();
    pump(&mut link, Duration::from_secs(5), &mut seen, |s| !s.is_empty());
    systemd.wait_until("the subscription", |s| s.subscribers() == 1);
    assert_eq!(systemd.calls("Get"), 1, "one read: {:?}", systemd.calls("Get"));
}
