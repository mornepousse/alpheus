use super::*;
use crate::session_bus::bus::Method;
use crate::session_bus::bus::testing::FakeOutbox;
use crate::session_bus::codec::{MenuNode, MenuProps};

fn key() -> ItemKey {
    ItemKey { owner: ":1.7".into(), path: "/StatusNotifierItem".into() }
}

fn node(id: i32, label: &str, submenu: bool, children: Vec<MenuNode>) -> MenuNode {
    let props = MenuProps { label: Some(label.into()), submenu: submenu.then_some(true), ..MenuProps::default() };
    MenuNode { id, props, children }
}

/// Open, More ▸ (children not fetched), Quit.
fn layout(revision: u32) -> Reply {
    let root = node(0, "", true, vec![node(1, "Open", false, vec![]), node(8, "More", true, vec![]), node(11, "Quit", false, vec![])]);
    Reply::Layout(Layout { revision, root })
}

fn more(children: Vec<MenuNode>) -> Reply {
    Reply::Layout(Layout { revision: 2, root: node(8, "More", true, children) })
}

/// The last call recorded (its id and itself).
fn last(out: &FakeOutbox) -> (CallId, Call) {
    out.calls.last().cloned().unwrap()
}

/// A session whose root layout arrived; the outbox emptied.
fn opened(out: &mut FakeOutbox) -> MenuSession {
    let mut s = MenuSession::open(key(), "/MenuBar".into(), out);
    let about = out.ids(Method::AboutToShow)[0];
    s.answer(about, Ok(Reply::NeedUpdate(false)), out);
    let get = out.ids(Method::GetLayout)[0];
    assert_eq!(s.answer(get, Ok(layout(1)), out), MenuUpdate::Changed);
    out.take();
    s
}

fn get_layout(parent: i32) -> Call {
    Call::GetLayout { destination: ":1.7".into(), path: "/MenuBar".into(), parent }
}

#[test]
fn about_to_show_goes_before_get_layout() {
    let mut out = FakeOutbox::default();
    let mut s = MenuSession::open(key(), "/MenuBar".into(), &mut out);
    let calls: Vec<Method> = out.calls.iter().map(|(_, c)| c.method()).collect();
    assert_eq!(calls, [Method::AddMatch, Method::AboutToShow], "no GetLayout before AboutToShow's answer");
    assert_eq!(last(&out).1, Call::AboutToShow { destination: ":1.7".into(), path: "/MenuBar".into(), id: 0 });
    assert!(s.layout().is_none());
    let about = last(&out).0;
    assert_eq!(s.answer(about, Ok(Reply::NeedUpdate(true)), &mut out), MenuUpdate::Nothing);
    assert_eq!(last(&out).1, get_layout(0));
    assert_eq!(s.answer(last(&out).0, Ok(layout(1)), &mut out), MenuUpdate::Changed);
    assert_eq!(s.layout().unwrap().len(), 3);
}

#[test]
fn about_to_show_failing_still_reads_the_layout() {
    let mut out = FakeOutbox::default();
    let mut s = MenuSession::open(key(), "/MenuBar".into(), &mut out);
    let about = last(&out).0;
    s.answer(about, Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into())), &mut out);
    assert_eq!(last(&out).1, get_layout(0));
}

#[test]
fn submenus_are_fetched_only_when_opened() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    assert!(out.calls.is_empty(), "the root only, one level deep");
    s.fetch_submenu(8, &mut out);
    s.fetch_submenu(8, &mut out);
    assert_eq!(out.calls.len(), 1, "asked once");
    assert_eq!(last(&out).1, Call::AboutToShow { destination: ":1.7".into(), path: "/MenuBar".into(), id: 8 });
    s.answer(last(&out).0, Ok(Reply::NeedUpdate(false)), &mut out);
    assert_eq!(last(&out).1, get_layout(8), "its children are not known yet");
    let children = vec![node(9, "Settings", false, vec![]), node(10, "About", false, vec![])];
    assert_eq!(s.answer(last(&out).0, Ok(more(children)), &mut out), MenuUpdate::Changed);
    assert_eq!(s.layout().unwrap().node(8).unwrap().children.len(), 2, "grafted");
    out.take();
    s.fetch_submenu(8, &mut out);
    s.answer(last(&out).0, Ok(Reply::NeedUpdate(false)), &mut out);
    assert_eq!(out.calls.len(), 1, "known and unchanged: no GetLayout again");
}

#[test]
fn layout_updated_while_open_rereads_in_place() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    let unopened = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 4, parent: 8 };
    assert_eq!(s.signal(&unopened), MenuUpdate::Nothing);
    assert!(out.calls.is_empty(), "More never opened: read when it opens");
    let updated = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 5, parent: 0 };
    assert_eq!(s.signal(&updated), MenuUpdate::Nothing);
    s.signal(&updated);
    assert!(s.wants_frame() && out.calls.is_empty(), "marked dirty, read at the frame");
    s.on_frame(&mut out);
    assert!(!s.wants_frame());
    assert_eq!(out.calls.len(), 1, "one re-read in flight");
    assert_eq!(last(&out).1, get_layout(0));
    let newer = Layout { revision: 5, root: node(0, "", true, vec![node(1, "Open", false, vec![]), node(12, "Pause", false, vec![])]) };
    assert_eq!(s.answer(last(&out).0, Ok(Reply::Layout(newer)), &mut out), MenuUpdate::Changed);
    let l = s.layout().unwrap();
    assert_eq!((l.revision, l.len(), l.node(12).unwrap().label()), (5, 2, "Pause"));
}

#[test]
fn items_properties_updated_patches_in_place() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    let patch = Signal::ItemsPropertiesUpdated {
        sender: ":1.7".into(),
        path: "/MenuBar".into(),
        updated: vec![(1, MenuProps { label: Some("Open again".into()), enabled: Some(false), ..MenuProps::default() })],
        removed: vec![],
    };
    assert_eq!(s.signal(&patch), MenuUpdate::Changed);
    let n = s.layout().unwrap().node(1).unwrap();
    assert_eq!((n.label(), n.is_enabled()), ("Open again", false));
    assert!(out.calls.is_empty(), "no call: the signal carries the values");
    let elsewhere = Signal::ItemsPropertiesUpdated {
        sender: ":1.7".into(),
        path: "/MenuBar".into(),
        updated: vec![(99, MenuProps { label: Some("Ghost".into()), ..MenuProps::default() })],
        removed: vec![(98, vec!["label".into()])],
    };
    assert_eq!(s.signal(&elsewhere), MenuUpdate::Nothing, "no entry of this menu named: nothing to repaint");
}

#[test]
fn a_root_reread_refetches_open_submenus() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    s.fetch_submenu(8, &mut out);
    s.answer(last(&out).0, Ok(Reply::NeedUpdate(false)), &mut out);
    s.answer(last(&out).0, Ok(more(vec![node(9, "Settings", false, vec![])])), &mut out);
    out.take();
    let updated = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 6, parent: 0 };
    s.signal(&updated);
    s.on_frame(&mut out);
    assert_eq!(s.answer(last(&out).0, Ok(layout(6)), &mut out), MenuUpdate::Changed);
    assert_eq!(last(&out).1, get_layout(8), "More's children, carried over by the graft, read again");
    assert_eq!(s.answer(last(&out).0, Ok(more(vec![node(9, "Settings", false, vec![])])), &mut out), MenuUpdate::Changed);
    assert_eq!(s.layout().unwrap().node(8).unwrap().children.len(), 1);
    out.take();
    let without = Layout { revision: 7, root: node(0, "", true, vec![node(1, "Open", false, vec![])]) };
    s.signal(&updated);
    s.on_frame(&mut out);
    s.answer(last(&out).0, Ok(Reply::Layout(without)), &mut out);
    assert_eq!(out.calls.len(), 1, "More gone: the root re-read only");
    out.take();
    s.signal(&updated);
    s.on_frame(&mut out);
    s.answer(last(&out).0, Ok(layout(8)), &mut out);
    assert_eq!(out.calls.len(), 1, "More back, never opened since: forgotten, not fetched");
}

#[test]
fn a_storm_of_layout_updates_is_one_read_per_frame() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    for revision in 2..60 {
        let updated = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision, parent: 0 };
        s.signal(&updated);
    }
    assert!(out.calls.is_empty(), "nothing asked between frames");
    assert!(s.wants_frame());
    s.on_frame(&mut out);
    assert_eq!(out.ids(Method::GetLayout).len(), 1, "58 signals, one GetLayout");
    s.on_frame(&mut out);
    assert_eq!(out.calls.len(), 1, "and nothing more on the next frame");
    assert!(!s.wants_frame());
}

#[test]
fn a_mismatched_root_id_is_a_bad_reply_no_graft() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    let before = s.layout().cloned().unwrap();
    let updated = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 2, parent: 0 };
    assert_eq!(s.signal(&updated), MenuUpdate::Nothing);
    s.on_frame(&mut out);
    assert_eq!(last(&out).1, get_layout(0));
    // The application echoes the id of an existing child (1, "Open") instead of the root it was
    // asked for (0): grafting it blindly would silently replace "Open" with a bogus subtree.
    let wrong = Reply::Layout(Layout { revision: 9, root: node(1, "Hijacked", true, vec![node(99, "Evil", false, vec![])]) });
    assert_eq!(s.answer(last(&out).0, Ok(wrong), &mut out), MenuUpdate::Nothing, "never grafted onto an unrelated node");
    assert_eq!(s.layout().unwrap(), &before, "tree and revision left exactly as they were");
}

#[test]
fn revision_is_not_bumped_when_a_submenus_graft_fails() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    s.fetch_submenu(8, &mut out);
    s.answer(last(&out).0, Ok(Reply::NeedUpdate(false)), &mut out);
    let get8 = last(&out).0;
    // A root re-read removes "More" (id 8) before that submenu's own GetLayout comes back.
    let updated = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 5, parent: 0 };
    s.signal(&updated);
    s.on_frame(&mut out);
    let without = Layout { revision: 5, root: node(0, "", true, vec![node(1, "Open", false, vec![])]) };
    assert_eq!(s.answer(last(&out).0, Ok(Reply::Layout(without)), &mut out), MenuUpdate::Changed);
    assert_eq!(s.layout().unwrap().revision, 5);
    assert!(s.layout().unwrap().node(8).is_none(), "More is gone");
    // The orphaned submenu reply arrives after: its root id (8) still matches what was asked, so it
    // is not a bad reply — but there is no node 8 left to graft onto.
    let stray = more(vec![node(9, "Settings", false, vec![])]);
    assert_eq!(s.answer(get8, Ok(stray), &mut out), MenuUpdate::Nothing);
    assert_eq!(s.layout().unwrap().revision, 5, "a failed graft must never move the revision");
}

#[test]
fn a_submenu_reply_before_the_root_is_known_is_ignored() {
    let mut out = FakeOutbox::default();
    let mut s = MenuSession::open(key(), "/MenuBar".into(), &mut out);
    s.fetch_submenu(8, &mut out);
    let about8 = last(&out).0;
    s.answer(about8, Ok(Reply::NeedUpdate(false)), &mut out);
    let get8 = last(&out).0;
    let reply = more(vec![node(9, "Settings", false, vec![])]);
    assert_eq!(s.answer(get8, Ok(reply), &mut out), MenuUpdate::Nothing, "no root tree yet: ignored, not closed");
    assert!(s.layout().is_none(), "no bogus tree built from a submenu alone");
}

#[test]
fn layout_updated_with_the_same_revision_is_not_refetched() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    let repeat = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 1, parent: 0 };
    assert_eq!(s.signal(&repeat), MenuUpdate::Nothing);
    assert!(out.calls.is_empty(), "already at revision 1: nothing to refetch");
}

#[test]
fn layout_updated_revision_zero_never_freezes_updates() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    let zero = Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 0, parent: 0 };
    s.signal(&zero);
    s.on_frame(&mut out);
    assert_eq!(last(&out).1, get_layout(0), "first 0: always trusted as a real change");
    let first = Layout { revision: 0, root: node(0, "", true, vec![node(1, "Open", false, vec![]), node(13, "Resume", false, vec![])]) };
    assert_eq!(s.answer(last(&out).0, Ok(Reply::Layout(first)), &mut out), MenuUpdate::Changed);
    out.take();
    // A second real change, still announced with revision 0 (an app that never versions its
    // layout): without the 0 carve-out, 0 == 0 would look already-seen and this would be dropped.
    s.signal(&zero);
    s.on_frame(&mut out);
    assert_eq!(last(&out).1, get_layout(0), "a second 0 still refetches: never frozen");
}

#[test]
fn signals_of_another_menu_are_ignored() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    let before = s.layout().cloned();
    for (sender, path) in [(":1.8", "/MenuBar"), (":1.7", "/OtherMenu")] {
        let updated = Signal::LayoutUpdated { sender: sender.into(), path: path.into(), revision: 9, parent: 0 };
        let patch = Signal::ItemsPropertiesUpdated {
            sender: sender.into(),
            path: path.into(),
            updated: vec![(1, MenuProps { label: Some("Spoofed".into()), ..MenuProps::default() })],
            removed: vec![],
        };
        assert_eq!(s.signal(&updated), MenuUpdate::Nothing);
        assert_eq!(s.signal(&patch), MenuUpdate::Nothing);
    }
    assert!(out.calls.is_empty());
    assert_eq!(s.layout().cloned(), before);
}

#[test]
fn click_sends_event_with_the_right_id_and_no_reply_expected() {
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    s.click(11, 4242, &mut out);
    assert_eq!(out.sent, [Call::Event { destination: ":1.7".into(), path: "/MenuBar".into(), id: 11, timestamp: 4242 }]);
    assert!(out.calls.is_empty(), "nothing awaited: many applications never answer Event");
}

#[test]
fn no_answer_within_2_s_closes() {
    let mut out = FakeOutbox::default();
    let mut s = MenuSession::open(key(), "/MenuBar".into(), &mut out);
    assert_eq!(s.answer(last(&out).0, Err(CallError::Timeout), &mut out), MenuUpdate::Close);
    let mut out = FakeOutbox::default();
    let mut s = opened(&mut out);
    s.fetch_submenu(8, &mut out);
    s.answer(last(&out).0, Ok(Reply::NeedUpdate(true)), &mut out);
    assert_eq!(s.answer(last(&out).0, Err(CallError::Timeout), &mut out), MenuUpdate::Close, "a submenu's too");
    let mut out = FakeOutbox::default();
    let mut s = MenuSession::open(key(), "/MenuBar".into(), &mut out);
    s.answer(last(&out).0, Ok(Reply::NeedUpdate(false)), &mut out);
    let bad = Err(CallError::BadReply("GetLayout: (s)".into()));
    assert_eq!(s.answer(last(&out).0, bad, &mut out), MenuUpdate::Close, "no layout at all: no menu");
}

#[test]
fn the_menu_match_rule_exists_only_while_open() {
    let mut out = FakeOutbox::default();
    let s = opened(&mut out);
    assert_eq!(menu_rule(":1.7", "/MenuBar"), "type='signal',sender=':1.7',path='/MenuBar',interface='com.canonical.dbusmenu'");
    let mut again = FakeOutbox::default();
    let other = MenuSession::open(key(), "/MenuBar".into(), &mut again);
    assert_eq!(again.calls[0].1, Call::AddMatch { rule: menu_rule(":1.7", "/MenuBar") }, "added at open");
    s.close(&mut out);
    assert_eq!(out.sent, [Call::RemoveMatch { rule: menu_rule(":1.7", "/MenuBar") }], "removed at close");
    assert!(out.calls.is_empty(), "nothing else at rest: no pending entry, no timer");
    other.close(&mut again);
}
