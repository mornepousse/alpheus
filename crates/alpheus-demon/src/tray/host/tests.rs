use super::*;
use crate::tray::WATCHER_NAME;
use crate::tray::bus::Method;
use crate::tray::bus::testing::FakeOutbox;
use std::fs;

const NIMBUS: &str = "org.kde.StatusNotifierItem-4242-1";
/// The daemon's own unique name, as `Host::new` would be given it — distinct from every
/// sender used elsewhere in this module.
const OWN: &str = ":1.0";

/// A temporary /proc with one process, 4242, named "nimbus"; removed on drop.
struct Proc(PathBuf);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn host(name: &str) -> (Host, FakeOutbox, Proc) {
    let root = std::env::temp_dir().join(format!("alpheus-proc-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("4242")).unwrap();
    fs::write(root.join("4242/comm"), "nimbus\n").unwrap();
    (Host::new(root.clone(), OWN.into()), FakeOutbox::default(), Proc(root))
}

/// A well-behaved KDE-like item's answers; the bus driver's for the rest.
fn nimbus(call: &Call) -> Result<Reply, CallError> {
    Ok(match call {
        Call::Get { property: Property::Item(p), .. } => Reply::Prop(match p {
            Prop::Id => PropValue::Text("nimbus".into()),
            Prop::Title => PropValue::Text("Nimbus".into()),
            Prop::Status => PropValue::Status(Status::Active),
            Prop::IconName => PropValue::Text("nimbus-tray".into()),
            Prop::AttentionIconName => PropValue::Text(String::new()),
            Prop::ToolTip => PropValue::Text("Nimbus: 3 new".into()),
            Prop::Menu => PropValue::Path("/MenuBar".into()),
            Prop::ItemIsMenu => PropValue::Bool(false),
        }),
        Call::GetConnectionUnixProcessID { .. } => Reply::Pid(4242),
        Call::GetNameOwner { .. } => Reply::Owner(":1.7".into()),
        Call::ListNames => Reply::Names(vec![]),
        _ => Reply::Unit,
    })
}

/// Answers every recorded call with `f`, then the calls those answers made, until none is left;
/// every call answered, in order.
fn settle(h: &mut Host, out: &mut FakeOutbox, f: impl Fn(&Call) -> Result<Reply, CallError>) -> Vec<Call> {
    let mut all = Vec::new();
    loop {
        let calls = out.take();
        if calls.is_empty() {
            return all;
        }
        for (id, call) in calls {
            h.answer(id, f(&call), out);
            all.push(call);
        }
    }
}

fn gets(calls: &[Call]) -> Vec<(String, Prop)> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Get { destination, property: Property::Item(p), .. } => Some((destination.clone(), *p)),
            _ => None,
        })
        .collect()
}

fn key(owner: &str) -> ItemKey {
    ItemKey { owner: owner.into(), path: DEFAULT_ITEM_PATH.into() }
}

fn changed(owner: &str, p: Prop) -> Signal {
    let mut props = PropSet::default();
    props.insert(p);
    Signal::ItemChanged { sender: owner.into(), path: DEFAULT_ITEM_PATH.into(), props }
}

fn gone(name: &str, old: &str) -> Signal {
    Signal::NameOwnerChanged { name: name.into(), old: old.into(), new: String::new() }
}

#[test]
fn each_property_is_one_get_never_getall() {
    let (mut h, mut out, _proc) = host("one-get");
    h.register(":1.7", ":1.7", &mut out);
    let calls: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
    let props: Vec<Prop> = gets(&calls).into_iter().map(|(_, p)| p).collect();
    assert_eq!(props, Prop::ALL, "one Get per property, all sent at once");
    let methods: Vec<Method> = calls.iter().map(Call::method).collect();
    assert_eq!(methods.iter().filter(|m| **m == Method::GetConnectionUnixProcessID).count(), 1);
    assert_eq!(methods.iter().filter(|m| **m == Method::AddMatch).count(), 2);
    assert_eq!(methods.len(), 11, "nothing else: {methods:?}");
    assert!(!h.items()[0].is_ready(), "shown once the Gets answered");
}

#[test]
fn every_application_call_goes_to_a_unique_name() {
    let (mut h, mut out, _proc) = host("unique");
    h.register(NIMBUS, ":1.7", &mut out);
    let mut calls = settle(&mut h, &mut out, nimbus);
    h.signal(&changed(":1.7", Prop::Title), &mut out);
    h.on_frame(&mut out);
    calls.extend(settle(&mut h, &mut out, nimbus));
    let to_apps = gets(&calls);
    assert_eq!(to_apps.len(), 9);
    assert!(to_apps.iter().all(|(d, _)| d == ":1.7"), "{to_apps:?}");
}

#[test]
fn a_get_name_owner_answer_must_be_a_unique_name() {
    let (mut h, mut out, _proc) = host("owner-not-unique");
    h.register("org.freedesktop.DBus", "", &mut out);
    let id = out.take().into_iter().next().unwrap().0;
    // The bus driver owns itself literally: it answers GetNameOwner on its own name with that
    // very name, never a unique one (":1.N").
    h.answer(id, Ok(Reply::Owner("org.freedesktop.DBus".into())), &mut out);
    assert!(h.items().is_empty(), "never trusted as an owner");
    assert!(out.take().is_empty(), "no Get ever reaches the bus driver");
}

#[test]
fn a_well_known_name_is_resolved_once_then_keyed_by_unique_name() {
    let (mut h, mut out, _proc) = host("resolved");
    h.register(NIMBUS, ":1.7", &mut out);
    h.register(NIMBUS, ":1.7", &mut out);
    assert_eq!(out.ids(Method::GetNameOwner).len(), 1, "one question in flight");
    settle(&mut h, &mut out, nimbus);
    let item = &h.items()[0];
    assert_eq!((&item.key, item.service.as_str()), (&key(":1.7"), NIMBUS));
    assert_eq!(item.watcher_entry(), format!("{NIMBUS}/StatusNotifierItem"));
    h.register(NIMBUS, ":1.7", &mut out);
    assert!(out.take().is_empty(), "resolved once: a re-registration asks nothing");
    assert_eq!(h.items().len(), 1);
}

#[test]
fn a_second_alias_from_the_same_sender_skips_get_name_owner() {
    let (mut h, mut out, _proc) = host("alias-same-sender");
    h.register(NIMBUS, ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    assert_eq!(h.items().len(), 1);
    // A different well-known alias, but the same real sender: its owner is already known.
    h.register("org.kde.StatusNotifierItem-9999-1", ":1.7", &mut out);
    assert!(out.take().is_empty(), "already known by its unique name: no GetNameOwner");
    assert_eq!(h.items().len(), 1, "still the one item");
}

#[test]
fn add_never_treats_our_own_unique_name_as_an_item() {
    let (mut h, mut out, _proc) = host("self-unique");
    // The bare-unique-name form of `register` calls `add` directly, with no GetNameOwner
    // round trip: this is what a RegisterStatusNotifierItem naming us literally, or an
    // entry already keyed by a unique name in another watcher's list or signal, looks like.
    // `sender` is irrelevant on this path — `add`'s guard checks `key.owner` (the name
    // itself), never `sender` — so an arbitrary, unrelated value proves that here on purpose
    // (task 5 fix round 2 review: a prior version of this comment read as if the sender were
    // part of what gets checked).
    h.register(OWN, "irrelevant-sender", &mut out);
    assert!(h.items().is_empty());
    assert!(out.take().is_empty(), "nothing asked of our own connection");
}

#[test]
fn add_never_treats_a_well_known_name_resolving_to_us_as_an_item() {
    let (mut h, mut out, _proc) = host("self-well-known");
    // A registration naming our own well-known name (task 5 review): `split_service` alone
    // cannot see this is us, only the GetNameOwner round trip it triggers can — the same
    // round trip `register` takes for any well-known name, so this also covers an entry of
    // another watcher's list or its ItemRegistered signal (both call `register` the same
    // way, sender ""), not only a direct call to our own watcher object.
    h.register(WATCHER_NAME, "", &mut out);
    let id = out.ids(Method::GetNameOwner).into_iter().next().unwrap();
    out.take(); // the GetNameOwner call itself; only what `add` would ask next matters below
    h.answer(id, Ok(Reply::Owner(OWN.into())), &mut out);
    assert!(h.items().is_empty());
    assert!(out.take().is_empty(), "no Get ever reaches our own connection");
}

#[test]
fn registration_forms() {
    let split = |s: &str, sender: &str| split_service(s, sender);
    let pair = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
    assert_eq!(split(NIMBUS, ":1.7"), pair(NIMBUS, "/StatusNotifierItem"));
    assert_eq!(split("/org/ayatana/NotificationItem/quill", ":1.8"), pair(":1.8", "/org/ayatana/NotificationItem/quill"));
    assert_eq!(
        split("org.kde.StatusNotifierItem-4343-1/StatusNotifierItem", ":1.9"),
        pair("org.kde.StatusNotifierItem-4343-1", "/StatusNotifierItem")
    );
    assert_eq!(split(":1.10", ":1.10"), pair(":1.10", "/StatusNotifierItem"));
    assert_eq!(split("org.telegram.desktop", ":1.11"), pair("org.telegram.desktop", "/StatusNotifierItem"), "dots are not paths");
    assert_eq!(
        split(":1.12/org/ayatana/NotificationItem/x_1", ""),
        pair(":1.12", "/org/ayatana/NotificationItem/x_1"),
        "the other watcher's entry"
    );
    for bad in ["", "/x", "nodots", "1org.x", "org..x", ":1.7 x", "org.x/a//b", "org.x/a/", "org.x/a b", "/a\u{7}b"] {
        assert_eq!(split(bad, ""), None, "{bad:?}");
    }
    assert_eq!(split("/bad path", ":1.7"), None);
}

#[test]
fn a_huge_name_is_refused_by_its_length_alone() {
    // Valid element by element: only the 255-byte bound can refuse it.
    let dotted = "a.".repeat(512 * 1024);
    assert_eq!(dotted.len(), 1 << 20);
    assert_eq!(split_service(&dotted, ":1.7"), None, "1 MB of short elements");
    assert_eq!(split_service(&format!("{dotted}/x"), ":1.7"), None, "with a path");
    let flat = "a".repeat(1 << 20);
    assert_eq!(split_service(&flat, ":1.7"), None, "1 MB without a dot");
    let longest = format!("org.{}", "a".repeat(251));
    assert_eq!(longest.len(), 255);
    assert!(split_service(&longest, ":1.7").is_some(), "255 bytes still pass");
    assert_eq!(split_service(&format!("{longest}a"), ":1.7"), None, "256 do not");
}

#[test]
fn a_bare_unique_name_must_be_the_callers_own() {
    let (mut h, mut out, _proc) = host("unique-sender");
    h.register(":1.9", ":1.7", &mut out);
    h.register(":1.9/StatusNotifierItem", ":1.7", &mut out);
    assert!(h.items().is_empty() && out.take().is_empty(), "somebody else's connection: ignored");
    h.register(":1.7", ":1.7", &mut out);
    assert_eq!(h.items().len(), 1, "its own: accepted");
    h.register(":1.12/StatusNotifierItem", "", &mut out);
    assert_eq!(h.items().len(), 2, "another watcher's entry has no caller to compare with");
}

#[test]
fn one_owner_holds_at_most_16_items() {
    let (mut h, mut out, _proc) = host("per-owner");
    for n in 0..MAX_ITEMS_PER_OWNER + 4 {
        h.register(&format!("/item{n}"), ":1.7", &mut out);
    }
    assert_eq!(h.items().len(), MAX_ITEMS_PER_OWNER);
    h.register(":1.8", ":1.8", &mut out);
    assert_eq!(h.items().len(), MAX_ITEMS_PER_OWNER + 1, "another owner is not affected");
}

#[test]
fn duplicate_registration_is_one_item() {
    let (mut h, mut out, _proc) = host("duplicate");
    h.register(":1.7", ":1.7", &mut out);
    h.register(":1.7", ":1.7", &mut out);
    h.register("/StatusNotifierItem", ":1.7", &mut out);
    h.register(":1.7/StatusNotifierItem", "", &mut out);
    assert_eq!(gets(&out.take().into_iter().map(|(_, c)| c).collect::<Vec<_>>()).len(), 8);
    assert_eq!(h.items().len(), 1);
    assert_eq!(h.take_changes().registered, [":1.7/StatusNotifierItem"]);
}

#[test]
fn a_duplicate_registration_rereads_the_menu() {
    // No StatusNotifierItem signal exists for Menu: an item that first registered with none
    // (or "/NO_DBUSMENU") and gains one later is only ever noticed through a duplicate
    // RegisterStatusNotifierItem (fix round, 2026-10-06).
    let (mut h, mut out, _proc) = host("reread-menu");
    h.register(":1.7", ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    assert_eq!(h.items()[0].menu.as_deref(), Some("/MenuBar"), "the fake's first answer");
    h.register(":1.7", ":1.7", &mut out);
    let calls: Vec<(CallId, Call)> = out.take();
    let gets: Vec<Prop> = gets(&calls.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>()).into_iter().map(|(_, p)| p).collect();
    assert_eq!(gets, [Prop::Menu], "one Get, Menu only: {gets:?}");
    // A second duplicate while the first Get is still in flight asks nothing more.
    h.register(":1.7", ":1.7", &mut out);
    assert!(out.take().is_empty(), "already asking: no second Get");
    let (id, _) = calls[0].clone();
    h.answer(id, Ok(Reply::Prop(PropValue::Path("/NewMenu".into()))), &mut out);
    assert_eq!(h.items()[0].menu.as_deref(), Some("/NewMenu"), "the gained menu is applied");
}

#[test]
fn departure_by_name_owner_changed_only() {
    let (mut h, mut out, _proc) = host("departure");
    h.register(":1.7", ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    h.take_changes();
    h.signal(&Signal::NameOwnerChanged { name: ":1.7".into(), old: String::new(), new: ":1.7".into() }, &mut out);
    h.signal(&Signal::ItemUnregistered(":1.7/StatusNotifierItem".into()), &mut out);
    h.signal(&changed(":1.7", Prop::Title), &mut out);
    h.on_frame(&mut out);
    settle(&mut h, &mut out, |_| Err(CallError::Timeout));
    assert_eq!(h.items().len(), 1, "only an owner leaving removes an item");
    h.signal(&gone(":1.7", ":1.7"), &mut out);
    assert!(h.items().is_empty());
    let c = h.take_changes();
    assert!(c.redraw);
    assert_eq!(c.unregistered, [":1.7/StatusNotifierItem"]);
}

#[test]
fn a_well_known_owner_leaving_removes_its_item_once() {
    let (mut h, mut out, _proc) = host("well-known-leaves");
    h.register(NIMBUS, ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    h.take_changes();
    h.signal(&gone(NIMBUS, ":1.7"), &mut out);
    assert_eq!(h.items().len(), 1, "the well-known name keys nothing");
    h.signal(&gone(":1.7", ":1.7"), &mut out);
    assert!(h.items().is_empty());
    assert_eq!(h.take_changes().unregistered, [format!("{NIMBUS}/StatusNotifierItem")], "once");
}

#[test]
fn per_item_match_rules_added_and_removed() {
    let (mut h, mut out, _proc) = host("rules");
    h.register("/a", ":1.7", &mut out);
    h.register("/b", ":1.7", &mut out);
    h.register(":1.8", ":1.8", &mut out);
    let rules = |out: &mut FakeOutbox, m: Method| -> Vec<String> {
        out.take()
            .into_iter()
            .filter(|(_, c)| c.method() == m)
            .map(|(_, c)| match c {
                Call::AddMatch { rule } | Call::RemoveMatch { rule } => rule,
                _ => unreachable!(),
            })
            .collect()
    };
    let added = rules(&mut out, Method::AddMatch);
    assert_eq!(added, [owner_rule(":1.7"), item_rule(":1.7"), owner_rule(":1.8"), item_rule(":1.8")], "one pair per owner");
    assert_eq!(
        owner_rule(":1.7"),
        "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0=':1.7'"
    );
    assert_eq!(item_rule(":1.7"), "type='signal',sender=':1.7',interface='org.kde.StatusNotifierItem'");
    h.signal(&gone(":1.7", ":1.7"), &mut out);
    let removed: Vec<String> = std::mem::take(&mut out.sent)
        .into_iter()
        .map(|c| match c {
            Call::RemoveMatch { rule } => rule,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(removed, [owner_rule(":1.7"), item_rule(":1.7")], "removed with the owner's last item, no answer awaited");
    assert_eq!(h.items().len(), 1);
}

#[test]
fn a_burst_marks_dirty_and_rereads_once_per_frame() {
    let (mut h, mut out, _proc) = host("burst");
    h.register(":1.7", ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    assert!(!h.wants_frame());
    for _ in 0..50 {
        h.signal(&changed(":1.7", Prop::IconName), &mut out);
    }
    assert!(out.take().is_empty(), "a signal sends nothing by itself");
    assert!(h.wants_frame());
    h.on_frame(&mut out);
    let reread = out.take();
    assert_eq!(gets(&reread.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>()), [(":1.7".to_string(), Prop::IconName)]);
    assert!(!h.wants_frame(), "its Get is in flight");
    for _ in 0..50 {
        h.signal(&changed(":1.7", Prop::IconName), &mut out);
    }
    h.on_frame(&mut out);
    assert!(out.take().is_empty(), "never two Gets of one property in flight");
    h.answer(reread[0].0, nimbus(&reread[0].1), &mut out);
    assert!(h.wants_frame(), "named again meanwhile: one more read at the next frame");
    h.on_frame(&mut out);
    assert_eq!(out.take().len(), 1);
}

#[test]
fn new_status_carries_its_value_no_call() {
    let (mut h, mut out, _proc) = host("status");
    h.register(":1.7", ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    h.take_changes();
    h.signal(&Signal::ItemStatus { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), status: Status::NeedsAttention }, &mut out);
    assert_eq!(h.items()[0].status, Status::NeedsAttention);
    assert!(h.take_changes().redraw);
    assert!(out.take().is_empty() && !h.wants_frame(), "no call, no frame");
    h.signal(&Signal::ItemStatus { sender: ":1.9".into(), path: DEFAULT_ITEM_PATH.into(), status: Status::Passive }, &mut out);
    assert_eq!(h.items()[0].status, Status::NeedsAttention, "another sender's signal changes nothing");
}

#[test]
fn failed_properties_are_not_asked_again_until_a_signal_names_them() {
    let (mut h, mut out, _proc) = host("failed");
    h.register(":1.7", ":1.7", &mut out);
    let no_icon = |c: &Call| match c {
        Call::Get { property: Property::Item(Prop::IconName | Prop::ToolTip), .. } => {
            Err(CallError::Remote("org.freedesktop.DBus.Error.Failed".into()))
        }
        other => nimbus(other),
    };
    settle(&mut h, &mut out, no_icon);
    assert!(h.items()[0].is_ready() && h.items()[0].icon_name.is_none());
    h.on_frame(&mut out);
    assert!(out.take().is_empty() && !h.wants_frame(), "failed: not asked again unprompted");
    h.signal(&changed(":1.7", Prop::ToolTip), &mut out);
    h.on_frame(&mut out);
    let calls: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
    assert_eq!(gets(&calls), [(":1.7".to_string(), Prop::ToolTip)], "only what the signal named");
}

#[test]
fn a_silent_item_is_shown_after_its_gets_time_out() {
    let (mut h, mut out, _proc) = host("silent");
    h.register(":1.7", ":1.7", &mut out);
    let mut calls = out.take();
    for (id, call) in calls.drain(..) {
        let r = if matches!(call, Call::Get { .. }) { Err(CallError::Timeout) } else { nimbus(&call) };
        h.answer(id, r, &mut out);
    }
    let item = &h.items()[0];
    assert!(item.is_ready(), "shown, with its letter");
    assert_eq!((item.title.as_deref(), item.process.as_deref()), (None, Some("nimbus")));
    assert!(h.take_changes().redraw);
    h.on_frame(&mut out);
    assert!(out.take().is_empty(), "nothing asked again until a signal");
    h.signal(&changed(":1.7", Prop::Title), &mut out);
    h.on_frame(&mut out);
    assert_eq!(out.take().len(), 1);
}

#[test]
fn an_item_whose_every_get_errors_is_dropped() {
    let (mut h, mut out, _proc) = host("bogus");
    h.register("/no/such/object", ":1.7", &mut out);
    let calls = settle(&mut h, &mut out, |c| match c {
        Call::Get { .. } => Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownObject".into())),
        other => nimbus(other),
    });
    assert!(h.items().is_empty(), "never shown");
    assert!(out.sent.iter().any(|c| c.method() == Method::RemoveMatch), "its rules go with it");
    assert!(!calls.iter().any(|c| c.method() == Method::RemoveMatch), "sent, not called: nobody reads the answer");
    let c = h.take_changes();
    assert!(!c.redraw, "it was never drawn");
    assert_eq!(c.unregistered, [":1.7/no/such/object"]);
    h.register(":1.8", ":1.8", &mut out);
    settle(&mut h, &mut out, |c| match c {
        Call::Get { property: Property::Item(Prop::Id), .. } => Err(CallError::Timeout),
        Call::Get { .. } => Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into())),
        other => nimbus(other),
    });
    assert_eq!(h.items().len(), 1, "one timeout: maybe slow, kept with its letter");
}

#[test]
fn a_bogus_sibling_is_dropped_but_its_owners_rules_stay() {
    let (mut h, mut out, _proc) = host("bogus-sibling");
    h.register("/a", ":1.7", &mut out);
    h.register("/b", ":1.7", &mut out);
    let calls = settle(&mut h, &mut out, |c| match c {
        Call::Get { path, .. } if path == "/b" => Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownObject".into())),
        other => nimbus(other),
    });
    assert_eq!(h.items().len(), 1, "/b dropped, /a stays");
    assert_eq!(h.items()[0].key.path, "/a");
    assert!(!calls.iter().any(|c| c.method() == Method::RemoveMatch) && out.sent.is_empty(), "the owner still has /a: rules stay");
}

#[test]
fn the_65th_item_is_refused() {
    let (mut h, mut out, _proc) = host("bounds");
    for n in 1..=MAX_ITEMS + 1 {
        h.register(&format!(":1.{n}"), "", &mut out);
    }
    assert_eq!(h.items().len(), MAX_ITEMS);
    let calls: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
    assert!(!gets(&calls).iter().any(|(d, _)| *d == format!(":1.{}", MAX_ITEMS + 1)), "nothing asked of the 65th");
    h.signal(&gone(":1.1", ":1.1"), &mut out);
    h.register(&format!(":1.{}", MAX_ITEMS + 1), "", &mut out);
    assert_eq!(h.items().len(), MAX_ITEMS, "room again: it may come in");
    assert_eq!(h.items().last().unwrap().key.owner, format!(":1.{}", MAX_ITEMS + 1));
    let (mut h, mut out, _proc) = host("bounds-names");
    for n in 1..=MAX_ITEMS + 1 {
        h.register(&format!("org.kde.StatusNotifierItem-{n}-1"), "", &mut out);
    }
    assert_eq!(out.ids(Method::GetNameOwner).len(), MAX_ITEMS, "never more names resolving than room");
}

#[test]
fn inventory_probes_sni_names_only() {
    let (mut h, mut out, _proc) = host("inventory");
    h.start(&mut out);
    let names = [
        "org.freedesktop.DBus",
        ":1.3",
        "org.kde.StatusNotifierItem-12-1",
        "org.freedesktop.StatusNotifierItem-13-1",
        "org.kde.StatusNotifierWatcher",
        "org.kde.kdeconnect",
    ];
    let calls = settle(&mut h, &mut out, |c| match c {
        Call::ListNames => Ok(Reply::Names(names.map(String::from).to_vec())),
        Call::GetNameOwner { name } if name.ends_with("12-1") => Ok(Reply::Owner(":1.12".into())),
        Call::GetNameOwner { .. } => Ok(Reply::Owner(":1.13".into())),
        other => nimbus(other),
    });
    let asked: Vec<&Call> = calls.iter().filter(|c| c.method() == Method::GetNameOwner).collect();
    assert_eq!(
        asked,
        [
            &Call::GetNameOwner { name: "org.kde.StatusNotifierItem-12-1".into() },
            &Call::GetNameOwner { name: "org.freedesktop.StatusNotifierItem-13-1".into() }
        ]
    );
    let keys: Vec<&ItemKey> = h.items().iter().map(|i| &i.key).collect();
    assert_eq!(keys, [&key(":1.12"), &key(":1.13")], "at the default path");
}

#[test]
fn process_name_from_proc_comm() {
    let (mut h, mut out, _proc) = host("comm");
    h.register(":1.7", ":1.7", &mut out);
    h.register(":1.8", ":1.8", &mut out);
    settle(&mut h, &mut out, |c| match c {
        Call::GetConnectionUnixProcessID { name } if name == ":1.8" => Ok(Reply::Pid(999_999)),
        other => nimbus(other),
    });
    assert_eq!(h.item(&key(":1.7")).unwrap().process.as_deref(), Some("nimbus"));
    assert_eq!(h.item(&key(":1.8")).unwrap().process, None, "no such process: none");
}

#[test]
fn a_non_utf8_comm_still_yields_a_letter() {
    let (mut h, mut out, proc) = host("non-utf8-comm");
    fs::write(proc.0.join("4242/comm"), b"ni\xffmbus\n").unwrap();
    h.register(":1.7", ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    let process = h.item(&key(":1.7")).unwrap().process.clone();
    assert!(process.as_deref().is_some_and(|p| p.starts_with("ni")), "{process:?}");
}

#[test]
fn an_item_waits_for_its_process_name_before_it_is_shown() {
    let (mut h, mut out, _proc) = host("pid-last");
    h.register(":1.7", ":1.7", &mut out);
    let (pid, rest): (Vec<_>, Vec<_>) = out.take().into_iter().partition(|(_, c)| c.method() == Method::GetConnectionUnixProcessID);
    for (id, call) in rest {
        h.answer(id, nimbus(&call), &mut out);
    }
    assert!(!h.items()[0].is_ready(), "its fallback letter may still change: not shown yet");
    assert!(!h.take_changes().redraw);
    h.answer(pid[0].0, Err(CallError::Timeout), &mut out);
    assert!(h.items()[0].is_ready(), "a process id that never comes holds nothing back");
    assert!(h.take_changes().redraw);
}

#[test]
fn a_menu_path_meaning_none_is_no_menu() {
    let mut i = Item::new(key(":1.7"), ":1.7".into());
    for none in ["/", "/NO_DBUSMENU"] {
        i.apply(Prop::Menu, PropValue::Path(none.into()));
        assert_eq!(i.menu, None, "{none}: no dbusmenu behind it");
    }
    assert!(i.apply(Prop::Menu, PropValue::Path("/MenuBar".into())));
    assert_eq!(i.menu.as_deref(), Some("/MenuBar"));
    assert!(i.apply(Prop::Menu, PropValue::Path("/NO_DBUSMENU".into())), "a menu withdrawn");
    assert_eq!(i.menu, None);
}

#[test]
fn the_host_calls_only_its_part_of_the_allowlist() {
    let (mut h, mut out, _proc) = host("allowlist");
    h.start(&mut out);
    h.register(NIMBUS, ":1.7", &mut out);
    h.register("/org/ayatana/NotificationItem/quill", ":1.8", &mut out);
    let mut calls = settle(&mut h, &mut out, nimbus);
    h.signal(&changed(":1.7", Prop::ToolTip), &mut out);
    h.on_frame(&mut out);
    h.signal(&gone(":1.8", ":1.8"), &mut out);
    h.unregister(&format!("{NIMBUS}/StatusNotifierItem"), &mut out);
    calls.extend(settle(&mut h, &mut out, nimbus));
    assert!(h.items().is_empty());
    let allowed = [Method::Get, Method::GetNameOwner, Method::GetConnectionUnixProcessID, Method::ListNames, Method::AddMatch];
    for c in &calls {
        assert!(allowed.contains(&c.method()), "{c:?}");
    }
    assert!(out.sent.iter().all(|c| c.method() == Method::RemoveMatch), "the host sends only the rules' removal, unanswered");
}

#[test]
fn clear_forgets_everything() {
    let (mut h, mut out, _proc) = host("clear");
    h.register(":1.7", ":1.7", &mut out);
    settle(&mut h, &mut out, nimbus);
    h.take_changes();
    h.register(":1.8", ":1.8", &mut out);
    let in_flight = out.take().first().unwrap().0;
    let c = h.clear();
    assert!(c.redraw && h.items().is_empty());
    assert_eq!(c.unregistered, [":1.7/StatusNotifierItem", ":1.8/StatusNotifierItem"]);
    assert!(!h.owns(in_flight), "a call in flight when the bus closed is forgotten too");
}

#[test]
fn tooltip_text_falls_back_to_title_then_id() {
    let mut i = Item::new(key(":1.7"), ":1.7".into());
    assert_eq!(i.tooltip_text(), None);
    i.id = Some("chrome_status_icon_1".into());
    assert_eq!(i.tooltip_text(), Some("chrome_status_icon_1"));
    i.title = Some("Lumen".into());
    assert_eq!(i.tooltip_text(), Some("Lumen"));
    i.tooltip = Some("Lumen: 2 unread".into());
    assert_eq!(i.tooltip_text(), Some("Lumen: 2 unread"));
}

/// Fix round 2026-10-06, item 5: `codec::clean` turns a ToolTip of just "\n" into a single
/// space — stored as absent, so `tooltip_text` falls back instead of a blank floating box.
#[test]
fn a_whitespace_only_tooltip_is_stored_as_absent() {
    let mut i = Item::new(key(":1.7"), ":1.7".into());
    i.title = Some("Lumen".into());
    assert!(!i.apply(Prop::ToolTip, PropValue::Text(" ".into())), "already absent: nothing changed");
    assert_eq!(i.tooltip, None);
    assert_eq!(i.tooltip_text(), Some("Lumen"), "falls back to Title");
    assert!(i.apply(Prop::ToolTip, PropValue::Text("Lumen: 2 unread".into())));
    assert!(i.apply(Prop::ToolTip, PropValue::Text("  ".into())), "a real tooltip withdrawn to whitespace");
    assert_eq!(i.tooltip, None);
}
