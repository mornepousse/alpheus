use super::*;

fn f(system: Option<u32>, user: Option<u32>) -> Failed {
    Failed { system, user }
}

#[test]
fn the_total_adds_the_known_counts() {
    assert_eq!(total(&f(Some(2), Some(1))), Some(3));
    assert_eq!(total(&f(Some(2), None)), Some(2), "a user manager that is not there: the system count alone");
    assert_eq!(total(&f(None, Some(1))), Some(1));
    assert_eq!(total(&f(Some(0), Some(0))), Some(0));
    assert_eq!(total(&f(None, None)), None, "neither known: no state");
    assert_eq!(total(&f(Some(u32::MAX), Some(1))), Some(u32::MAX), "saturates");
}

#[test]
fn a_notice_goes_on_a_rise_and_is_rearmed_at_zero() {
    let mut n = Notice::default();
    assert_eq!(n.feed(Some(0), true), None);
    assert_eq!(n.feed(Some(2), true), Some(2));
    assert_eq!(n.feed(Some(2), true), None, "the same total: once");
    assert_eq!(n.feed(Some(1), true), None, "a fall is no notice");
    assert_eq!(n.feed(Some(2), true), None, "a rise back to what was notified is not above it");
    assert_eq!(n.feed(Some(3), true), Some(3));
    assert_eq!(n.feed(Some(0), true), None);
    assert_eq!(n.feed(Some(1), true), Some(1), "re-armed at zero");
}

#[test]
fn a_notice_waits_for_the_server() {
    let mut n = Notice::default();
    assert_eq!(n.feed(Some(2), false), None, "nobody to show it: still armed");
    assert_eq!(n.last(), Some(2));
    assert_eq!(n.feed(n.last(), true), Some(2), "offered again when the server starts");
    assert_eq!(n.feed(n.last(), true), None);
}

#[test]
fn an_unknown_total_changes_nothing() {
    let mut n = Notice::default();
    assert_eq!(n.feed(Some(2), true), Some(2));
    assert_eq!(n.feed(None, true), None);
    assert_eq!(n.last(), Some(2));
    assert_eq!(n.feed(Some(2), true), None);
}

#[test]
fn the_notice_text() {
    assert_eq!(incoming(1).summary, "1 unit failed");
    assert_eq!(incoming(3).summary, "3 units failed");
    let i = incoming(3);
    assert_eq!((i.app_name.as_str(), i.body.as_str(), i.urgency), ("alpheus", "Open Diag in the control center", Urgency::Normal));
    assert_eq!(i.expire_ms, -1, "the server's default timeout");
    assert!(i.actions.is_empty());
}
