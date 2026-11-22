//! Tests of the user manager's count (`app.rs`): the calls it makes, what it keeps and the log
//! lines it writes, against a fake outbox. No bus, no daemon.

use super::*;
use crate::session_bus::bus::Method;
use crate::session_bus::bus::testing::FakeOutbox;
use std::cell::RefCell;

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The log sink the client under test writes to: the lines are kept per test thread.
fn record_line(s: &str) {
    LINES.with(|l| l.borrow_mut().push(s.to_string()));
}

/// How many of the lines logged so far contain `pattern`.
fn lines_logged(pattern: &str) -> usize {
    LINES.with(|l| l.borrow().iter().filter(|x| x.contains(pattern)).count())
}

/// The unique name the fake user manager has on the bus.
const USER: &str = ":1.30";

/// A client just started: its two rules sent and its owner question asked.
fn started(subscribe: bool) -> (FakeOutbox, UserUnits) {
    LINES.with(|l| l.borrow_mut().clear());
    let mut out = FakeOutbox::default();
    let u = UserUnits::start_logged(&mut out, subscribe, record_line);
    (out, u)
}

/// The question that finds the manager's owner.
fn owner_call() -> Call {
    Call::GetNameOwner { name: SYSTEMD_NAME.into() }
}

/// The read of the count, addressed to `dest`.
fn count_call(dest: &str) -> Call {
    Call::Get { destination: dest.into(), path: SYSTEMD_PATH.into(), property: Property::NFailedUnits }
}

/// The subscription, addressed to `dest`.
fn subscribe_call(dest: &str) -> Call {
    Call::SubscribeUnits { dest: dest.into() }
}

/// The id of the latest call equal to `call`.
fn latest_call_id(out: &FakeOutbox, call: &Call) -> CallId {
    out.calls.iter().rev().find(|(_, c)| c == call).map(|(id, _)| *id).unwrap_or_else(|| panic!("never asked: {call:?}"))
}

/// How many times `call` was made.
fn asked(out: &FakeOutbox, call: &Call) -> usize {
    out.calls.iter().filter(|(_, c)| c == call).count()
}

/// The bus's notice that the manager's name changed hands.
fn owner_changed(old: &str, new: &str) -> Signal {
    Signal::NameOwnerChanged { name: SYSTEMD_NAME.into(), old: old.into(), new: new.into() }
}

/// The manager is found and its count read: `n`.
fn running(subscribe: bool, n: u32) -> (FakeOutbox, UserUnits) {
    let (mut out, mut u) = started(subscribe);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Ok(Reply::Owner(USER.into())));
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(n)));
    (out, u)
}

#[test]
fn the_client_asks_for_the_owner_then_reads_the_count() {
    let (mut out, mut u) = started(false);
    // The two rules first, with no reply (nothing pending for them), then the owner.
    assert_eq!(out.sent, [Call::AddMatch { rule: SYSTEMD_OWNER_RULE.into() }, Call::AddMatch { rule: SYSTEMD_PROPERTIES_RULE.into() }]);
    assert_eq!(out.calls.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [owner_call()]);
    assert_eq!(u.count(), None, "nothing known yet");
    let who = latest_call_id(&out, &owner_call());
    assert!(u.owns(who));
    u.answer(&mut out, who, Ok(Reply::Owner(USER.into())));
    assert_eq!(asked(&out, &count_call(USER)), 1, "the count, from the owner's unique name");
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(2)));
    assert_eq!((u.count(), u.take_change()), (Some(2), Some(Some(2))));
    assert_eq!(u.take_change(), None, "told once");
    assert!(!u.owns(get), "answered");
}

#[test]
fn no_user_manager_leaves_the_count_unknown() {
    let (mut out, mut u) = started(true);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Remote("org.freedesktop.DBus.Error.NameHasNoOwner".into())));
    assert_eq!(u.count(), None);
    assert_eq!(u.take_change(), Some(None), "the first answer is told, even unknown: the notice waits for it");
    u.lost();
    assert_eq!(u.take_change(), None, "unknown stays unknown: nothing more to tell");
    assert_eq!(out.calls.len(), 1, "no Get, no Subscribe");
    // A timeout is unknown too, and one fixed line says so.
    let (mut out, mut u) = started(true);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Timeout));
    assert_eq!(u.count(), None);
    assert_eq!(lines_logged("user manager"), 1);
}

#[test]
fn the_client_subscribes_once_unless_subscribe_is_off() {
    let (out, _) = running(true, 1);
    assert_eq!(asked(&out, &subscribe_call(USER)), 1, "after the first read, once");
    let (out, _) = running(false, 1);
    assert_eq!(asked(&out, &subscribe_call(USER)), 0, "[failed] subscribe = false");
}

#[test]
fn the_managers_signal_reads_the_count_again() {
    let (mut out, mut u) = running(true, 1);
    u.take_change();
    assert!(u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() }));
    assert_eq!(asked(&out, &count_call(USER)), 2);
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(3)));
    assert_eq!(u.take_change(), Some(Some(3)));
    // A peer's PropertiesChanged on the same names is not the manager's.
    let before = out.calls.len();
    assert!(!u.signal(&mut out, &Signal::UnitsChanged { sender: ":1.99".into() }), "not the owner");
    assert_eq!(out.calls.len(), before);
    // The same count again changes nothing.
    assert!(u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() }));
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(3)));
    assert_eq!(u.take_change(), None);
}

#[test]
fn a_change_while_a_read_is_in_flight_reads_once_more() {
    let (mut out, mut u) = running(false, 1);
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    let first = asked(&out, &count_call(USER));
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    assert_eq!(asked(&out, &count_call(USER)), first, "one read at a time");
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(2)));
    assert_eq!(asked(&out, &count_call(USER)), first + 1, "the change that came meanwhile is read after");
}

#[test]
fn a_read_of_the_system_count_reads_the_user_count_again() {
    let (mut out, mut u) = running(false, 1);
    u.on_units_read(&mut out);
    assert_eq!(asked(&out, &count_call(USER)), 2, "one Get per read of the system count");
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(4)));
    assert_eq!(u.take_change(), Some(Some(4)), "unsubscribed: this is how the user count follows");
    // With no manager known, a read asks for its owner again.
    let (mut out, mut u) = started(false);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Remote("org.freedesktop.DBus.Error.NameHasNoOwner".into())));
    u.on_units_read(&mut out);
    assert_eq!(asked(&out, &owner_call()), 2);
}

#[test]
fn a_new_user_manager_owner_resubscribes() {
    let (mut out, mut u) = running(true, 1);
    let sub = latest_call_id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Ok(Reply::Unit));
    assert!(u.subscribed());
    assert!(u.signal(&mut out, &owner_changed(USER, ":1.55")));
    assert!(!u.subscribed(), "the new owner has no subscriber");
    assert_eq!(asked(&out, &count_call(":1.55")), 1, "read again, from the new owner");
    let get = latest_call_id(&out, &count_call(":1.55"));
    u.answer(&mut out, get, Ok(Reply::Count(1)));
    assert_eq!(asked(&out, &subscribe_call(":1.55")), 1, "and subscribed again");
    // Another name's owner change is not ours.
    let other = Signal::NameOwnerChanged { name: "org.example".into(), old: "".into(), new: ":1.7".into() };
    assert!(!u.signal(&mut out, &other));
    // The manager leaving: unknown until it comes back.
    assert!(u.signal(&mut out, &owner_changed(":1.55", "")));
    assert_eq!((u.count(), u.take_change()), (None, Some(None)));
}

#[test]
fn a_stale_answer_from_the_old_owner_is_dropped() {
    let (mut out, mut u) = running(false, 1);
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    let stale = latest_call_id(&out, &count_call(USER));
    u.signal(&mut out, &owner_changed(USER, ":1.55"));
    assert!(!u.owns(stale), "its read was for the owner that left");
}

#[test]
fn losing_the_bus_forgets_the_subscription_and_the_count() {
    let (mut out, mut u) = running(true, 2);
    let sub = latest_call_id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Ok(Reply::Unit));
    u.take_change();
    assert!(u.subscribed());
    u.lost();
    assert!(!u.subscribed(), "the next link subscribes again");
    assert_eq!((u.count(), u.take_change()), (None, Some(None)));
    assert!(!u.owns(sub));
}

/// A user-side read that fails is logged like the system side's: one line per episode (until a
/// read succeeds), with fixed words and no error text.
#[test]
fn a_failed_user_read_logs_once_per_episode() {
    let (mut out, mut u) = running(false, 1);
    for _ in 0..3 {
        u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
        let get = latest_call_id(&out, &count_call(USER));
        u.answer(&mut out, get, Err(CallError::Remote("org.freedesktop.DBus.Error.NoMemory".into())));
    }
    assert_eq!((u.count(), lines_logged("count could not be read")), (None, 1));
    // A read that succeeds ends the episode: the next failure is a new line.
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(2)));
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Err(CallError::Timeout));
    assert_eq!(lines_logged("count could not be read"), 2);
    // The connection going away is not a read that failed.
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Err(CallError::Disconnected));
    assert_eq!(lines_logged("count could not be read"), 2);
}

#[test]
fn a_refused_user_subscribe_logs_once_per_owner() {
    let (mut out, mut u) = running(true, 1);
    let sub = latest_call_id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Err(CallError::Remote("org.freedesktop.DBus.Error.AccessDenied".into())));
    assert!(!u.subscribed());
    assert_eq!(lines_logged("Subscribe refused"), 1);
    // The same owner: a re-read does not ask again, nor log again.
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(1)));
    assert_eq!(asked(&out, &subscribe_call(USER)), 1);
    assert_eq!(lines_logged("Subscribe refused"), 1);
    // A new owner is asked, and refusing again is a new line.
    u.signal(&mut out, &owner_changed(USER, ":1.55"));
    let get = latest_call_id(&out, &count_call(":1.55"));
    u.answer(&mut out, get, Ok(Reply::Count(1)));
    let sub = latest_call_id(&out, &subscribe_call(":1.55"));
    u.answer(&mut out, sub, Err(CallError::Timeout));
    assert_eq!(lines_logged("Subscribe refused"), 2);
    // No line names an owner or a unit.
    LINES.with(|l| assert!(l.borrow().iter().all(|x| !x.contains(":1.") && !x.contains(".service"))));
}

#[test]
fn an_already_subscribed_manager_counts_as_subscribed() {
    let (mut out, mut u) = running(true, 1);
    let sub = latest_call_id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Err(CallError::Remote("org.freedesktop.systemd1.AlreadySubscribed".into())));
    assert!(u.subscribed());
    assert_eq!(lines_logged("Subscribe refused"), 0, "a daemon-reexec kept our subscription: not a refusal");
}

#[test]
fn a_failed_read_makes_the_count_unknown() {
    let (mut out, mut u) = running(false, 2);
    u.take_change();
    u.on_units_read(&mut out);
    let get = latest_call_id(&out, &count_call(USER));
    u.answer(&mut out, get, Err(CallError::Timeout));
    assert_eq!((u.count(), u.take_change()), (None, Some(None)));
    assert_eq!(Call::SubscribeUnits { dest: USER.into() }.method(), Method::Subscribe);
}

#[test]
fn a_failed_owner_lookup_is_one_line_per_episode() {
    let (mut out, mut u) = started(true);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Timeout));
    assert_eq!(lines_logged("could not be found"), 1);
    // Asked again by the next system read: the same failure says nothing more.
    u.on_units_read(&mut out);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Timeout));
    assert_eq!(lines_logged("could not be found"), 1);
    // A manager found ends the episode; losing it and failing again is a new one.
    u.on_units_read(&mut out);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Ok(Reply::Owner(USER.into())));
    u.signal(&mut out, &owner_changed(USER, ""));
    u.on_units_read(&mut out);
    let who = latest_call_id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Timeout));
    assert_eq!(lines_logged("could not be found"), 2);
}
