use super::*;
use crate::session_bus::bus::Method;
use crate::session_bus::bus::testing::FakeOutbox;
use std::cell::RefCell;

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn note(s: &str) {
    LINES.with(|l| l.borrow_mut().push(s.to_string()));
}

fn lines(pattern: &str) -> usize {
    LINES.with(|l| l.borrow().iter().filter(|x| x.contains(pattern)).count())
}

const USER: &str = ":1.30";

fn started(subscribe: bool) -> (FakeOutbox, UserUnits) {
    LINES.with(|l| l.borrow_mut().clear());
    let mut out = FakeOutbox::default();
    let u = UserUnits::start_logged(&mut out, subscribe, note);
    (out, u)
}

fn owner_call() -> Call {
    Call::GetNameOwner { name: SYSTEMD_NAME.into() }
}

fn count_call(dest: &str) -> Call {
    Call::Get { destination: dest.into(), path: SYSTEMD_PATH.into(), property: Property::NFailedUnits }
}

fn subscribe_call(dest: &str) -> Call {
    Call::SubscribeUnits { dest: dest.into() }
}

/// The id of the latest call equal to `call`.
fn id(out: &FakeOutbox, call: &Call) -> CallId {
    out.calls.iter().rev().find(|(_, c)| c == call).map(|(id, _)| *id).unwrap_or_else(|| panic!("never asked: {call:?}"))
}

fn asked(out: &FakeOutbox, call: &Call) -> usize {
    out.calls.iter().filter(|(_, c)| c == call).count()
}

fn owner_changed(old: &str, new: &str) -> Signal {
    Signal::NameOwnerChanged { name: SYSTEMD_NAME.into(), old: old.into(), new: new.into() }
}

/// The manager is found and its count read: `n`.
fn running(subscribe: bool, n: u32) -> (FakeOutbox, UserUnits) {
    let (mut out, mut u) = started(subscribe);
    let who = id(&out, &owner_call());
    u.answer(&mut out, who, Ok(Reply::Owner(USER.into())));
    let get = id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(n)));
    (out, u)
}

#[test]
fn user_units_ask_owner_then_get() {
    let (mut out, mut u) = started(false);
    // The two rules first, with no reply (nothing pending for them), then the owner.
    assert_eq!(out.sent, [Call::AddMatch { rule: SYSTEMD_OWNER_RULE.into() }, Call::AddMatch { rule: SYSTEMD_PROPERTIES_RULE.into() }]);
    assert_eq!(out.calls.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [owner_call()]);
    assert_eq!(u.count(), None, "nothing known yet");
    let who = id(&out, &owner_call());
    assert!(u.owns(who));
    u.answer(&mut out, who, Ok(Reply::Owner(USER.into())));
    assert_eq!(asked(&out, &count_call(USER)), 1, "the count, from the owner's unique name");
    let get = id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(2)));
    assert_eq!((u.count(), u.take_change()), (Some(2), Some(Some(2))));
    assert_eq!(u.take_change(), None, "told once");
    assert!(!u.owns(get), "answered");
}

#[test]
fn no_user_manager_is_unknown() {
    let (mut out, mut u) = started(true);
    let who = id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Remote("org.freedesktop.DBus.Error.NameHasNoOwner".into())));
    assert_eq!(u.count(), None);
    assert_eq!(u.take_change(), None, "unknown stays unknown: nothing to tell");
    assert_eq!(out.calls.len(), 1, "no Get, no Subscribe");
    // A timeout is unknown too, and one fixed line says so.
    let (mut out, mut u) = started(true);
    let who = id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Timeout));
    assert_eq!(u.count(), None);
    assert_eq!(lines("user manager"), 1);
}

#[test]
fn subscribes_unless_turned_off() {
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
    let get = id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(3)));
    assert_eq!(u.take_change(), Some(Some(3)));
    // A peer's PropertiesChanged on the same names is not the manager's.
    let before = out.calls.len();
    assert!(!u.signal(&mut out, &Signal::UnitsChanged { sender: ":1.99".into() }), "not the owner");
    assert_eq!(out.calls.len(), before);
    // The same count again changes nothing.
    assert!(u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() }));
    let get = id(&out, &count_call(USER));
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
    let get = id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(2)));
    assert_eq!(asked(&out, &count_call(USER)), first + 1, "the change that came meanwhile is read after");
}

#[test]
fn user_units_reask_on_units_read() {
    let (mut out, mut u) = running(false, 1);
    u.on_units_read(&mut out);
    assert_eq!(asked(&out, &count_call(USER)), 2, "one Get per read of the system count");
    let get = id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(4)));
    assert_eq!(u.take_change(), Some(Some(4)), "unsubscribed: this is how the user count follows");
    // With no manager known, a read asks for its owner again.
    let (mut out, mut u) = started(false);
    let who = id(&out, &owner_call());
    u.answer(&mut out, who, Err(CallError::Remote("org.freedesktop.DBus.Error.NameHasNoOwner".into())));
    u.on_units_read(&mut out);
    assert_eq!(asked(&out, &owner_call()), 2);
}

#[test]
fn a_new_user_manager_owner_resubscribes() {
    let (mut out, mut u) = running(true, 1);
    let sub = id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Ok(Reply::Unit));
    assert!(u.subscribed());
    assert!(u.signal(&mut out, &owner_changed(USER, ":1.55")));
    assert!(!u.subscribed(), "the new owner has no subscriber");
    assert_eq!(asked(&out, &count_call(":1.55")), 1, "read again, from the new owner");
    let get = id(&out, &count_call(":1.55"));
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
    let stale = id(&out, &count_call(USER));
    u.signal(&mut out, &owner_changed(USER, ":1.55"));
    assert!(!u.owns(stale), "its read was for the owner that left");
}

#[test]
fn lost_resets_subscribed() {
    let (mut out, mut u) = running(true, 2);
    let sub = id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Ok(Reply::Unit));
    u.take_change();
    assert!(u.subscribed());
    u.lost();
    assert!(!u.subscribed(), "the next link subscribes again");
    assert_eq!((u.count(), u.take_change()), (None, Some(None)));
    assert!(!u.owns(sub));
}

#[test]
fn a_refused_user_subscribe_logs_once_per_owner() {
    let (mut out, mut u) = running(true, 1);
    let sub = id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Err(CallError::Remote("org.freedesktop.DBus.Error.AccessDenied".into())));
    assert!(!u.subscribed());
    assert_eq!(lines("Subscribe refused"), 1);
    // The same owner: a re-read does not ask again, nor log again.
    u.signal(&mut out, &Signal::UnitsChanged { sender: USER.into() });
    let get = id(&out, &count_call(USER));
    u.answer(&mut out, get, Ok(Reply::Count(1)));
    assert_eq!(asked(&out, &subscribe_call(USER)), 1);
    assert_eq!(lines("Subscribe refused"), 1);
    // A new owner is asked, and refusing again is a new line.
    u.signal(&mut out, &owner_changed(USER, ":1.55"));
    let get = id(&out, &count_call(":1.55"));
    u.answer(&mut out, get, Ok(Reply::Count(1)));
    let sub = id(&out, &subscribe_call(":1.55"));
    u.answer(&mut out, sub, Err(CallError::Timeout));
    assert_eq!(lines("Subscribe refused"), 2);
    // No line names an owner or a unit.
    LINES.with(|l| assert!(l.borrow().iter().all(|x| !x.contains(":1.") && !x.contains(".service"))));
}

#[test]
fn an_already_subscribed_manager_counts_as_subscribed() {
    let (mut out, mut u) = running(true, 1);
    let sub = id(&out, &subscribe_call(USER));
    u.answer(&mut out, sub, Err(CallError::Remote("org.freedesktop.systemd1.AlreadySubscribed".into())));
    assert!(u.subscribed());
    assert_eq!(lines("Subscribe refused"), 0, "a daemon-reexec kept our subscription: not a refusal");
}

#[test]
fn a_failed_read_makes_the_count_unknown() {
    let (mut out, mut u) = running(false, 2);
    u.take_change();
    u.on_units_read(&mut out);
    let get = id(&out, &count_call(USER));
    u.answer(&mut out, get, Err(CallError::Timeout));
    assert_eq!((u.count(), u.take_change()), (None, Some(None)));
    assert_eq!(Call::SubscribeUnits { dest: USER.into() }.method(), Method::Subscribe);
}
