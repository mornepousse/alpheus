use super::*;
use crate::notify::{Action, Incoming, Urgency};
use crate::tray::bus::testing::FakeOutbox;
use crate::tray::{NOTIFICATIONS_INTERFACE, WATCHER_NAME};

fn note(summary: &str) -> Incoming {
    Incoming {
        app_name: "Fictional".into(),
        replaces_id: 0,
        summary: summary.into(),
        body: String::new(),
        actions: Vec::new(),
        urgency: Urgency::Normal,
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: None,
    }
}

/// A call to the Notifications object as the bus delivers it (a serial, a sender).
fn call(member: &str) -> Message {
    let mut m = Message::new_method_call(NOTIFICATIONS_NAME, NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, member).unwrap();
    m.set_serial(7);
    m.set_sender(Some(":1.42".into()));
    m
}

fn error_name(mut m: Message) -> String {
    m.as_result().err().and_then(|e| e.name().map(String::from)).unwrap_or_default()
}

fn started(dnd: bool) -> (FakeOutbox, Notifications) {
    let mut out = FakeOutbox::default();
    let n = Notifications::start(&mut out, &NotificationsConfig::default(), dnd);
    (out, n)
}

fn held() -> Notifications {
    let (out, mut n) = started(false);
    n.answer(out.calls[0].0, Ok(Reply::RequestName(1)));
    n
}

#[test]
fn it_asks_for_the_name_once_and_owns_that_answer_only() {
    let (out, n) = started(false);
    assert_eq!(out.calls.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::RequestNotifications]);
    assert!(out.sent.is_empty(), "no rule: NameAcquired and NameLost are unicast, they need none");
    let id = out.calls[0].0;
    assert!(n.owns(id));
    assert!(!n.owns(CallId(id.0 + 1)));
    assert!(!n.held(), "nothing served before the bus says so");
}

#[test]
fn primary_or_already_owner_holds_queued_does_not() {
    for (code, holds) in [(1, true), (4, true), (2, false)] {
        let (out, mut n) = started(false);
        n.answer(out.calls[0].0, Ok(Reply::RequestName(code)));
        assert_eq!(n.held(), holds, "RequestName → {code}");
        assert!(!n.owns(out.calls[0].0), "answered once");
    }
    let (out, mut n) = started(false);
    n.answer(out.calls[0].0, Err(CallError::Timeout));
    assert!(!n.held(), "a lost answer: NameAcquired still decides");
    assert!(n.signal(&Signal::NameAcquired(NOTIFICATIONS_NAME.into())));
    assert!(n.held());
}

#[test]
fn only_our_names_signals_count() {
    let (_, mut n) = started(false);
    assert!(!n.signal(&Signal::NameAcquired(WATCHER_NAME.into())), "the watcher's name is the tray's business");
    assert!(!n.held());
    assert!(n.signal(&Signal::NameAcquired(NOTIFICATIONS_NAME.into())));
    assert!(n.held());
    assert!(!n.signal(&Signal::NameLost(WATCHER_NAME.into())));
    assert!(n.held(), "another name lost: still ours");
    assert!(!n.signal(&Signal::Other));
}

#[test]
fn name_lost_hides_every_bubble_and_stops_serving() {
    let mut n = held();
    let now = Instant::now();
    n.center_mut().notify(note("brief"), now, 600);
    n.center_mut().notify(Incoming { urgency: Urgency::Critical, ..note("sticky") }, now, 600);
    assert_eq!(n.center().bubbles().len(), 2);
    assert!(n.center().next_expiry().is_some());
    n.take_change();
    n.take_bubbles_change();
    assert!(n.signal(&Signal::NameLost(NOTIFICATIONS_NAME.into())));
    assert!(!n.held());
    assert!(n.center().bubbles().is_empty(), "no bubble once the name is gone");
    assert_eq!(n.center().next_expiry(), None, "no timer");
    assert_eq!(n.center().history().count(), 2, "the history stays for the panel");
    assert_eq!(n.take_change().map(|v| v.serving), Some(false), "the bell dims");
    assert!(n.take_bubbles_change());
    let mut m = call("GetCapabilities");
    assert_eq!(error_name(n.handle(&m, now, || 600).unwrap()), "org.freedesktop.DBus.Error.UnknownMethod", "refused again");
    m.set_no_reply(true);
    assert!(n.handle(&m, now, || 600).is_none(), "nothing sent when no reply is wanted");
}

#[test]
fn served_calls_get_their_replies_and_close_by_call_emits_reason_3() {
    let mut n = held();
    let now = Instant::now();
    let info = n.handle(&call("GetServerInformation"), now, || 600).unwrap();
    assert_eq!(info.read4::<&str, &str, &str, &str>().unwrap(), ("alpheus", "alpheus", env!("CARGO_PKG_VERSION"), "1.3"));
    let caps = n.handle(&call("GetCapabilities"), now, || 600).unwrap();
    assert!(caps.read1::<Vec<String>>().unwrap().contains(&"actions".to_string()));
    let id = n.center_mut().notify(note("retract me"), now, 600);
    let closed = n.handle(&call("CloseNotification").append1(id), now, || 600).unwrap();
    assert_eq!(closed.iter_init().count(), 0, "an empty reply");
    let effects = n.take_effects();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].member().as_deref(), Some("NotificationClosed"));
    assert_eq!(effects[0].read2::<u32, u32>().unwrap(), (id, 3), "closed by a call: reason 3");
    assert!(effects[0].destination().is_none(), "broadcast");
    let unknown = n.handle(&call("CloseNotification").append1(4_000_000u32), now, || 600).unwrap();
    assert_eq!(unknown.iter_init().count(), 0, "an unknown id: empty too");
    assert!(n.take_effects().is_empty(), "and silent");
    assert_eq!(error_name(n.handle(&call("Bogus"), now, || 600).unwrap()), "org.freedesktop.DBus.Error.UnknownMethod");
}

/// While the name is held too, a call that wants no reply gets none (a no-reply Notify is still
/// ingested: `replies_always_arrive_even_when_no_bubble_is_shown`, on a private bus).
#[test]
fn a_held_server_answers_nothing_when_no_reply_is_wanted() {
    let mut n = held();
    let mut caps = call("GetCapabilities");
    assert!(n.handle(&caps, Instant::now(), || 600).is_some(), "the control case");
    caps.set_no_reply(true);
    assert!(n.handle(&caps, Instant::now(), || 600).is_none(), "no reply wanted: none sent");
}

/// A flood of criticals: the one `notify` demotes to the history goes out as NotificationClosed
/// (reason 1, expired), like any bubble that ends.
#[test]
fn a_critical_demoted_by_a_flood_is_signalled_closed() {
    let mut n = held();
    let ids: Vec<u32> = (0..9)
        .map(|i| {
            let reply = n.handle(&crate::tray::bus::testing::critical_notify_call(&format!("alert {i}")), Instant::now(), || 600).unwrap();
            reply.read1::<u32>().unwrap()
        })
        .collect();
    let effects = n.take_effects();
    assert_eq!(effects.len(), 1, "the ninth demotes the first");
    assert_eq!(effects[0].member().as_deref(), Some("NotificationClosed"));
    assert_eq!(effects[0].read2::<u32, u32>().unwrap(), (ids[0], 1));
}

/// Task 4's review: the local time is read only for a Notify served while the name is held,
/// never for a refused call nor for the other methods.
#[test]
fn the_minute_is_read_only_for_a_served_notify() {
    let never = || -> u32 { panic!("the clock read for a call that needs no time") };
    let notify = || crate::tray::bus::testing::notify_call("hello");
    let (_, mut queued) = started(false);
    assert!(queued.handle(&notify(), Instant::now(), never).is_some(), "refused while not held, no clock read");
    let mut n = held();
    for member in ["GetCapabilities", "GetServerInformation", "Bogus"] {
        assert!(n.handle(&call(member), Instant::now(), never).is_some(), "{member}");
    }
    assert!(n.handle(&call("CloseNotification").append1(4_000_000u32), Instant::now(), never).is_some());
    let read = std::cell::Cell::new(0);
    assert!(
        n.handle(&notify(), Instant::now(), || {
            read.set(read.get() + 1);
            14 * 60 + 2
        })
        .is_some()
    );
    assert_eq!(read.get(), 1, "read once for a served Notify");
    assert_eq!(n.center().history().next().map(|e| e.at), Some(14 * 60 + 2));
}

#[test]
fn effects_go_out_only_while_the_name_is_held() {
    let (_, mut n) = started(false);
    let now = Instant::now();
    let id = n.center_mut().notify(note("unseen"), now, 600);
    n.act(|c| c.close(id, CloseReason::Dismissed, now));
    assert!(n.take_effects().is_empty(), "nobody to tell while another server has the name");
    let mut n = held();
    let open = Action { key: "default".into(), label: "Open".into() };
    let id = n.center_mut().notify(Incoming { actions: vec![open], ..note("open me") }, now, 600);
    let revision = n.center().history().find(|e| e.id == id).map(|e| e.revision).unwrap();
    n.act(|c| c.invoke(id, revision, "default", now));
    let effects = n.take_effects();
    assert_eq!(effects[0].member().as_deref(), Some("ActionInvoked"));
    assert_eq!(effects[0].read2::<u32, &str>().unwrap(), (id, "default"));
    assert!(effects[0].destination().is_none(), "broadcast");
    assert!(n.take_effects().is_empty(), "taken once");
}

#[test]
fn the_view_and_the_bubbles_are_reported_only_when_they_change() {
    let (out, mut n) = started(false);
    assert_eq!(n.take_change(), Some(NotifyView { unread: 0, dnd: false, serving: false }), "the bell's first word");
    assert_eq!(n.take_change(), None);
    n.answer(out.calls[0].0, Ok(Reply::RequestName(1)));
    assert_eq!(n.take_change().map(|v| v.serving), Some(true));
    n.signal(&Signal::NameAcquired(NOTIFICATIONS_NAME.into()));
    assert_eq!(n.take_change(), None, "the same view twice: nothing");
    assert!(!n.take_bubbles_change(), "no bubble yet");
    let now = Instant::now();
    n.center_mut().notify(note("one"), now, 600);
    assert!(n.take_bubbles_change());
    assert!(!n.take_bubbles_change(), "taken");
    assert_eq!(n.take_change().map(|v| v.unread), Some(1));
    n.act(|c| {
        c.set_dnd(true);
        Vec::new()
    });
    assert!(n.take_bubbles_change(), "do not disturb takes a normal bubble down");
    n.center_mut().notify(note("two"), now, 600);
    assert!(!n.take_bubbles_change(), "a normal one under do not disturb: no bubble, nothing to repaint");
}

/// Constraints, Lightness (after task 2's review): the bubble timer follows every live bubble with
/// a finite timeout, hidden by do-not-disturb or not. A hidden one replaced moves the earliest
/// expiry without changing what shows: still a change for the host, which re-arms the timer.
#[test]
fn a_hidden_bubbles_new_expiry_is_a_change_too() {
    let mut n = held();
    let now = Instant::now();
    let id = n.center_mut().notify(note("one"), now, 600);
    assert!(n.take_bubbles_change(), "shown");
    n.act(|c| {
        c.set_dnd(true);
        Vec::new()
    });
    assert!(n.take_bubbles_change(), "hidden");
    let later = now + std::time::Duration::from_secs(1);
    n.center_mut().notify(Incoming { replaces_id: id, ..note("one, again") }, later, 600);
    assert!(n.center().bubbles().is_empty(), "still hidden");
    assert!(n.take_bubbles_change(), "its expiry moved: the timer must follow");
    assert!(!n.take_bubbles_change(), "taken");
}

/// The host draws after each batch (`take_bubbles_change`): six in one batch, only four drew,
/// and the two waiting get their whole timeout when they show; one at a time, each drew, and
/// the one pushed back still expires on time.
#[test]
fn a_bubble_drawn_keeps_its_timer_one_never_drawn_waits() {
    let secs = std::time::Duration::from_secs;
    let (mut n, t0) = (held(), Instant::now());
    for s in ["a", "b", "c", "d", "e", "f"] {
        n.center_mut().notify(note(s), t0, 600);
    }
    assert!(n.take_bubbles_change());
    assert_eq!(n.center_mut().expire(t0 + secs(5)).len(), 4, "the four drawn");
    assert!(n.take_bubbles_change());
    assert_eq!(n.center().next_expiry(), Some(t0 + secs(10)), "the two waiting: a whole timeout from now");
    let (mut n, t0) = (held(), Instant::now());
    for (i, s) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
        n.center_mut().notify(note(s), t0 + secs(i as u64 / 4), 600);
        n.take_bubbles_change();
    }
    assert_eq!(n.center().bubbles().len(), 4, "a pushed back by e");
    assert_eq!(n.center_mut().expire(t0 + secs(5)).len(), 4, "a drew: it expires with b, c and d");
}

#[test]
fn a_lost_bus_is_a_lost_name() {
    let mut n = held();
    n.center_mut().notify(note("brief"), Instant::now(), 600);
    n.take_change();
    n.take_bubbles_change();
    n.lost();
    assert!(!n.held());
    assert!(n.center().bubbles().is_empty());
    assert_eq!(n.center().next_expiry(), None);
    assert_eq!(n.take_change().map(|v| v.serving), Some(false));
    assert!(n.take_bubbles_change());
    assert!(n.take_effects().is_empty(), "no signal waits for a dead bus");
}

#[test]
fn only_the_notifications_path_is_ours() {
    assert!(serves(&call("Notify")));
    let watcher = Message::new_method_call(":1.1", crate::tray::WATCHER_PATH, crate::tray::WATCHER_INTERFACE, "Bogus").unwrap();
    assert!(!serves(&watcher));
    let below = Message::new_method_call(":1.1", "/org/freedesktop/Notifications/x", NOTIFICATIONS_INTERFACE, "Notify").unwrap();
    assert!(!serves(&below), "only the object itself");
}

#[test]
fn the_minute_counts_the_days_since_1970() {
    let day = minute_now() / crate::clock::DAY_MINUTES;
    assert!(i64::from(day) >= crate::clock::days_from_civil(2026, 1, 1), "day {day}: the date, not only the time of day");
}
