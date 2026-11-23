use super::*;
use crate::media::Control;
use crate::notify::{Action, Incoming, MAX_ACTIONS, MAX_APP, MAX_BODY, MAX_LABEL, MAX_SUMMARY, Urgency};
use crate::session_bus::DEFAULT_ITEM_PATH;
use crate::session_bus::{NOTIFICATIONS_NAME, WATCHER_NAME};
use dbus::arg::{IterAppend, Variant};
use dbus::strings::Signature;

fn signature(s: &str) -> Signature<'static> {
    Signature::new(s).unwrap()
}

/// A message whose body `f` writes (its type does not matter to `reply`).
fn body(f: impl FnOnce(&mut IterAppend<'_>)) -> Message {
    let mut m = Message::new_method_call(":1.7", DEFAULT_ITEM_PATH, "org.example", "Reply").unwrap();
    f(&mut IterAppend::new(&mut m));
    m
}

fn get(p: Prop) -> Call {
    Call::Get { destination: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), property: Property::Item(p) }
}

fn layout_call() -> Call {
    Call::GetLayout { destination: ":1.7".into(), path: "/MenuBar".into(), parent: 0 }
}

/// The `(sa(iiay)ss)` tooltip, its pixmap array of type `pixmaps` holding `bytes` bytes.
fn tooltip(title: &str, pixmaps: &str, bytes: usize) -> Message {
    let inner = format!("(s{pixmaps}ss)");
    body(|i| {
        i.append_variant(&signature(&inner), |v| {
            v.append_struct(|s| {
                s.append("tooltip-icon");
                let element = &pixmaps[1..];
                s.append_array(&signature(element), |a| {
                    a.append_struct(|e| {
                        e.append(16i32);
                        e.append(16i32);
                        if element == "(iiay)" {
                            e.append(vec![7u8; bytes]);
                        } else {
                            e.append("x".repeat(bytes));
                        }
                    });
                });
                s.append(title);
                s.append("description");
            });
        });
    })
}

#[derive(Clone)]
enum V {
    S(&'static str),
    B(bool),
    I(i32),
    Bytes(usize),
}

struct T {
    id: i32,
    props: Vec<(&'static str, V)>,
    children: Vec<T>,
}

fn t(id: i32, props: &[(&'static str, V)], children: Vec<T>) -> T {
    T { id, props: props.to_vec(), children }
}

fn append_node(i: &mut IterAppend<'_>, n: &T) {
    i.append_struct(|s| {
        s.append(n.id);
        s.append_dict(&signature("s"), &signature("v"), |d| {
            for (k, v) in &n.props {
                d.append_dict_entry(|e| {
                    e.append(*k);
                    match v {
                        V::S(x) => e.append(Variant(*x)),
                        V::B(x) => e.append(Variant(*x)),
                        V::I(x) => e.append(Variant(*x)),
                        V::Bytes(n) => e.append(Variant(vec![0u8; *n])),
                    }
                });
            }
        });
        s.append_array(&signature("v"), |a| {
            for c in &n.children {
                a.append_variant(&signature("(ia{sv}av)"), |v| append_node(v, c));
            }
        });
    });
}

fn layout_reply(revision: u32, root: &T) -> Message {
    body(|i| {
        i.append(revision);
        append_node(i, root);
    })
}

fn decoded(m: &Message) -> Layout {
    match reply(&layout_call(), m) {
        Ok(Reply::Layout(l)) => l,
        other => panic!("{other:?}"),
    }
}

/// A chain `depth` levels deep under the root, one node per level.
fn chain(depth: usize) -> T {
    let mut n = t(depth as i32, &[("label", V::S("leaf"))], vec![]);
    for id in (1..depth as i32).rev() {
        n = t(id, &[], vec![n]);
    }
    t(0, &[], vec![n])
}

fn signal_from(sender: &str, path: &str, interface: &str, member: &str, f: impl FnOnce(&mut IterAppend<'_>)) -> Signal {
    let mut m = Message::new_signal(path, interface, member).unwrap();
    m.set_sender(Some(sender.into()));
    f(&mut IterAppend::new(&mut m));
    signal(&m)
}

#[test]
fn read_one_decodes_the_color_scheme_plain_or_nested() {
    assert_eq!(reply(&Call::ReadOne, &body(|i| i.append(Variant(1u32)))), Ok(Reply::ColorScheme(1)));
    assert_eq!(
        reply(&Call::ReadOne, &body(|i| i.append(Variant(Variant(2u32))))),
        Ok(Reply::ColorScheme(2)),
        "v(v(u)): some portal backends wrap it twice"
    );
    assert!(matches!(reply(&Call::ReadOne, &body(|i| i.append(Variant("dark")))), Err(CallError::BadReply(_))));
    assert!(matches!(reply(&Call::ReadOne, &body(|i| i.append(1u32))), Err(CallError::BadReply(_))), "not in a variant");
}

#[test]
fn setting_changed_is_read_for_color_scheme_only() {
    use crate::session_bus::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_PATH, SETTINGS_INTERFACE};
    let changed = |path: &str, ns: &str, key: &str, f: &dyn Fn(&mut IterAppend<'_>)| {
        signal_from(":1.4", path, SETTINGS_INTERFACE, "SettingChanged", |i| {
            i.append(ns);
            i.append(key);
            f(i);
        })
    };
    assert_eq!(changed(PORTAL_PATH, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant(2u32))), Signal::ColorScheme(2));
    assert_eq!(changed(PORTAL_PATH, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant(Variant(1u32)))), Signal::ColorScheme(1));
    assert_eq!(changed(PORTAL_PATH, APPEARANCE_NAMESPACE, "contrast", &|i| i.append(Variant(1u32))), Signal::Other, "another key");
    assert_eq!(
        changed(PORTAL_PATH, "org.gnome.desktop.interface", COLOR_SCHEME_KEY, &|i| i.append(Variant("prefer-dark"))),
        Signal::Other,
        "another namespace"
    );
    assert_eq!(
        changed("/elsewhere", APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant(2u32))),
        Signal::Other,
        "not the portal's object"
    );
    assert_eq!(changed(PORTAL_PATH, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, &|i| i.append(Variant("light"))), Signal::Other, "not a u32");
}

#[test]
fn a_unicast_setting_changed_is_never_the_portals() {
    use crate::session_bus::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_PATH, SETTINGS_INTERFACE};
    let mut m = Message::new_signal(PORTAL_PATH, SETTINGS_INTERFACE, "SettingChanged").unwrap();
    m.set_sender(Some(":1.4".into()));
    let mut m = m.append3(APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, Variant(2u32));
    assert_eq!(signal(&m), Signal::ColorScheme(2), "broadcast: read");
    m.set_destination(Some(":1.9".into()));
    assert_eq!(signal(&m), Signal::Other, "addressed to us alone: a peer's, not the rule's");
}

#[test]
fn get_replies_decode_every_property() {
    let text = |p: Prop, s: &'static str| reply(&get(p), &body(|i| i.append(Variant(s))));
    assert_eq!(text(Prop::Id, "nimbus"), Ok(Reply::Prop(PropValue::Text("nimbus".into()))));
    assert_eq!(text(Prop::Title, "Nimbus"), Ok(Reply::Prop(PropValue::Text("Nimbus".into()))));
    assert_eq!(text(Prop::IconName, "nimbus-tray"), Ok(Reply::Prop(PropValue::Text("nimbus-tray".into()))));
    assert_eq!(text(Prop::AttentionIconName, "nimbus-alert"), Ok(Reply::Prop(PropValue::Text("nimbus-alert".into()))));
    assert_eq!(text(Prop::Status, "NeedsAttention"), Ok(Reply::Prop(PropValue::Status(Status::NeedsAttention))));
    let menu = reply(&get(Prop::Menu), &body(|i| i.append(Variant(Path::new("/MenuBar").unwrap()))));
    assert_eq!(menu, Ok(Reply::Prop(PropValue::Path("/MenuBar".into()))));
    assert_eq!(reply(&get(Prop::ItemIsMenu), &body(|i| i.append(Variant(true)))), Ok(Reply::Prop(PropValue::Bool(true))));
    assert_eq!(
        reply(&get(Prop::ToolTip), &tooltip("Nimbus: 3 new", "a(iiay)", 64)),
        Ok(Reply::Prop(PropValue::Text("Nimbus: 3 new".into())))
    );
    let others = Call::Get { destination: ":1.9".into(), path: "/StatusNotifierWatcher".into(), property: Property::RegisteredItems };
    let list = body(|i| i.append(Variant(vec![":1.7/StatusNotifierItem", "org.kde.StatusNotifierItem-4-1/StatusNotifierItem"])));
    assert_eq!(
        reply(&others, &list),
        Ok(Reply::Names(vec![":1.7/StatusNotifierItem".into(), "org.kde.StatusNotifierItem-4-1/StatusNotifierItem".into()]))
    );
    assert_eq!(
        reply(&Call::ListNames, &body(|i| i.append(vec!["org.freedesktop.DBus", ":1.7"]))),
        Ok(Reply::Names(vec!["org.freedesktop.DBus".into(), ":1.7".into()]))
    );
    assert_eq!(reply(&Call::GetNameOwner { name: "x.y".into() }, &body(|i| i.append(":1.7"))), Ok(Reply::Owner(":1.7".into())));
    assert_eq!(reply(&Call::GetConnectionUnixProcessID { name: ":1.7".into() }, &body(|i| i.append(4242u32))), Ok(Reply::Pid(4242)));
    assert_eq!(reply(&Call::RequestName, &body(|i| i.append(3u32))), Ok(Reply::RequestName(3)));
    assert_eq!(reply(&Call::RequestNotifications, &body(|i| i.append(2u32))), Ok(Reply::RequestName(2)), "2: queued behind mako");
    let about = Call::AboutToShow { destination: ":1.7".into(), path: "/MenuBar".into(), id: 0 };
    assert_eq!(reply(&about, &body(|i| i.append(true))), Ok(Reply::NeedUpdate(true)));
    let activate = Call::Activate { destination: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), x: 1, y: 2 };
    assert_eq!(reply(&activate, &body(|_| {})), Ok(Reply::Unit));
    let mut call = Message::new_method_call(":1.3", "/", "org.example", "X").unwrap();
    call.set_serial(4);
    let refused = call.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), c"no Activate");
    assert_eq!(reply(&activate, &refused), Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into())));
    assert_eq!(error(&refused), CallError::Remote("org.freedesktop.DBus.Error.UnknownMethod".into()));
}

#[test]
fn names_skip_a_string_longer_than_max_service() {
    let longest = "a".repeat(MAX_SERVICE);
    let too_long = "a".repeat(MAX_SERVICE + 1);
    let list = body(|i| i.append(vec![longest.as_str(), too_long.as_str(), "org.freedesktop.DBus"]));
    assert_eq!(
        reply(&Call::ListNames, &list),
        Ok(Reply::Names(vec![longest, "org.freedesktop.DBus".into()])),
        "exactly MAX_SERVICE bytes kept, one byte over skipped (not cut)"
    );
}

#[test]
fn a_menu_path_longer_than_max_service_is_a_bad_reply() {
    let longest = format!("/{}", "a".repeat(MAX_SERVICE - 1));
    assert!(longest.len() == MAX_SERVICE);
    let ok = reply(&get(Prop::Menu), &body(|i| i.append(Variant(Path::new(longest.as_str()).unwrap()))));
    assert_eq!(ok, Ok(Reply::Prop(PropValue::Path(longest))));
    let too_long = format!("/{}", "a".repeat(MAX_SERVICE));
    let bad = reply(&get(Prop::Menu), &body(|i| i.append(Variant(Path::new(too_long.as_str()).unwrap()))));
    assert!(matches!(bad, Err(CallError::BadReply(_))), "{bad:?}");
}

#[test]
fn tooltip_title_without_decoding_pixmaps() {
    // 300 KB of pixmaps whose elements are not even (iiay): stepped over, never read.
    let big = tooltip("Kettle", "a(iis)", 300_000);
    assert_eq!(reply(&get(Prop::ToolTip), &big), Ok(Reply::Prop(PropValue::Text("Kettle".into()))));
    let wrong = body(|i| i.append(Variant((7i32, "no title"))));
    assert!(matches!(reply(&get(Prop::ToolTip), &wrong), Err(CallError::BadReply(_))));
}

#[test]
fn wrong_types_are_bad_replies_not_panics() {
    let bad_reply = |r: Result<Reply, CallError>| matches!(r, Err(CallError::BadReply(_)));
    assert!(bad_reply(reply(&get(Prop::Title), &body(|i| i.append(Variant(42i32))))));
    assert!(bad_reply(reply(&get(Prop::Status), &body(|i| i.append(Variant(true))))));
    assert!(bad_reply(reply(&get(Prop::Menu), &body(|i| i.append(Variant("/MenuBar"))))), "a string is not a path");
    assert!(bad_reply(reply(&get(Prop::ItemIsMenu), &body(|i| i.append(Variant("true"))))));
    assert!(bad_reply(reply(&get(Prop::Title), &body(|i| i.append("not in a variant")))));
    assert!(bad_reply(reply(&Call::ListNames, &body(|i| i.append(vec![1u32, 2])))));
    assert!(bad_reply(reply(&Call::GetNameOwner { name: "a.b".into() }, &body(|i| i.append(7u32)))));
    assert!(bad_reply(reply(&layout_call(), &body(|i| i.append((7u32, "no layout"))))));
}

#[test]
fn empty_or_cut_bodies_never_panic() {
    let empty = body(|_| {});
    let call = |c: Call| reply(&c, &empty);
    for p in Prop::ALL {
        assert!(matches!(call(get(p)), Err(CallError::BadReply(_))), "{p:?}");
    }
    let others = Call::Get { destination: ":1.9".into(), path: "/".into(), property: Property::RegisteredItems };
    for c in [
        others,
        Call::ListNames,
        Call::GetNameOwner { name: "a.b".into() },
        Call::GetConnectionUnixProcessID { name: ":1.7".into() },
        Call::RequestName,
        Call::RequestNotifications,
        Call::AboutToShow { destination: ":1.7".into(), path: "/m".into(), id: 0 },
        layout_call(),
        Call::ReadOne,
        Call::PlayerGetAll { dest: ":1.42".into() },
        Call::PlayerIdentity { dest: ":1.42".into() },
        Call::PlayerMetadata { dest: ":1.42".into() },
        Call::PlayerPosition { dest: ":1.42".into() },
    ] {
        assert!(matches!(call(c.clone()), Err(CallError::BadReply(_))), "{c:?}");
    }
    let revision_only = body(|i| i.append(3u32));
    assert!(matches!(reply(&layout_call(), &revision_only), Err(CallError::BadReply(_))), "GetLayout cut after its revision");
    for (sender, interface, member) in [
        ("org.freedesktop.DBus", "org.freedesktop.DBus", "NameOwnerChanged"),
        ("org.freedesktop.DBus", "org.freedesktop.DBus", "NameAcquired"),
        ("org.freedesktop.DBus", "org.freedesktop.DBus", "NameLost"),
        (":1.9", WATCHER_INTERFACE, "StatusNotifierItemRegistered"),
        (":1.9", WATCHER_INTERFACE, "StatusNotifierItemUnregistered"),
        (":1.7", MENU_INTERFACE, "LayoutUpdated"),
        (":1.7", MENU_INTERFACE, "ItemsPropertiesUpdated"),
    ] {
        assert_eq!(signal_from(sender, "/x", interface, member, |_| {}), Signal::Other, "{member} with an empty body");
    }
    let cut = signal_from(":1.7", "/m", MENU_INTERFACE, "ItemsPropertiesUpdated", |i| {
        i.append_array(&signature("(ia{sv})"), |_| {});
    });
    assert_eq!(cut, Signal::Other, "ItemsPropertiesUpdated without its removed list");
    let cut =
        signal_from("org.freedesktop.DBus", "/", "org.freedesktop.DBus", "NameOwnerChanged", |i| i.append("org.kde.StatusNotifierWatcher"));
    assert_eq!(cut, Signal::Other, "NameOwnerChanged with one argument");
}

#[test]
fn signals_decode_or_are_other() {
    let owner_from = |sender: &str| {
        signal_from(sender, "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", |i| {
            i.append(":1.7");
            i.append(":1.7");
            i.append("");
        })
    };
    assert_eq!(
        owner_from("org.freedesktop.DBus"),
        Signal::NameOwnerChanged { name: ":1.7".into(), old: ":1.7".into(), new: String::new() }
    );
    assert_eq!(owner_from(":1.9"), Signal::Other, "a peer faking the bus driver's departure signal");
    for (member, prop) in [
        ("NewTitle", Prop::Title),
        ("NewIcon", Prop::IconName),
        ("NewAttentionIcon", Prop::AttentionIconName),
        ("NewToolTip", Prop::ToolTip),
    ] {
        let mut props = PropSet::default();
        props.insert(prop);
        assert_eq!(
            signal_from(":1.7", DEFAULT_ITEM_PATH, ITEM_INTERFACE, member, |_| {}),
            Signal::ItemChanged { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), props },
            "{member}"
        );
    }
    let chromium_double = signal_from(":1.7", DEFAULT_ITEM_PATH, "org.freedesktop.StatusNotifierItem", "NewIcon", |_| {});
    assert_eq!(chromium_double, Signal::Other, "Chromium's copy on the freedesktop interface is not read twice");
    let registered = signal_from(":1.9", "/StatusNotifierWatcher", WATCHER_INTERFACE, "StatusNotifierItemRegistered", |i| {
        i.append(":1.7/StatusNotifierItem");
    });
    assert_eq!(registered, Signal::ItemRegistered(":1.7/StatusNotifierItem".into()));
    let gone = signal_from(":1.9", "/StatusNotifierWatcher", WATCHER_INTERFACE, "StatusNotifierItemUnregistered", |i| i.append(":1.7/x"));
    assert_eq!(gone, Signal::ItemUnregistered(":1.7/x".into()));
    let service =
        |s: &str| signal_from(":1.9", "/StatusNotifierWatcher", WATCHER_INTERFACE, "StatusNotifierItemRegistered", |i| i.append(s));
    let longest = format!(":1.7/{}", "a".repeat(1019));
    assert_eq!(service(&longest), Signal::ItemRegistered(longest.clone()), "1024 bytes: passed whole, never cut");
    assert_eq!(service(&format!("{longest}a")), Signal::Other, "1025 bytes: refused");
    let layout = signal_from(":1.7", "/MenuBar", MENU_INTERFACE, "LayoutUpdated", |i| {
        i.append(12u32);
        i.append(5i32);
    });
    assert_eq!(layout, Signal::LayoutUpdated { sender: ":1.7".into(), path: "/MenuBar".into(), revision: 12, parent: 5 });
    let props = signal_from(":1.7", "/MenuBar", MENU_INTERFACE, "ItemsPropertiesUpdated", |i| {
        i.append_array(&signature("(ia{sv})"), |a| {
            a.append_struct(|s| {
                s.append(3i32);
                s.append_dict(&signature("s"), &signature("v"), |d| {
                    d.append_dict_entry(|e| {
                        e.append("label");
                        e.append(Variant("_Mute"));
                    });
                    d.append_dict_entry(|e| {
                        e.append("toggle-state");
                        e.append(Variant(1i32));
                    });
                    d.append_dict_entry(|e| {
                        e.append("icon-data");
                        e.append(Variant(vec![0u8; 512]));
                    });
                });
            });
        });
        i.append_array(&signature("(ias)"), |a| {
            a.append_struct(|s| {
                s.append(4i32);
                s.append(vec!["enabled", "icon-name"]);
            });
        });
    });
    let mute = MenuProps { label: Some("Mute".into()), toggle_state: Some(1), ..MenuProps::default() };
    assert_eq!(
        props,
        Signal::ItemsPropertiesUpdated {
            sender: ":1.7".into(),
            path: "/MenuBar".into(),
            updated: vec![(3, mute)],
            removed: vec![(4, vec!["enabled".into()])]
        }
    );
    assert_eq!(signal_from(":1.7", "/", "org.example", "Anything", |i| i.append(1u8)), Signal::Other);
}

#[test]
fn new_status_carries_its_value() {
    let status = signal_from(":1.7", DEFAULT_ITEM_PATH, ITEM_INTERFACE, "NewStatus", |i| i.append("Passive"));
    assert_eq!(status, Signal::ItemStatus { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), status: Status::Passive });
    let mut props = PropSet::default();
    props.insert(Prop::Status);
    let bare = signal_from(":1.7", DEFAULT_ITEM_PATH, ITEM_INTERFACE, "NewStatus", |_| {});
    assert_eq!(bare, Signal::ItemChanged { sender: ":1.7".into(), path: DEFAULT_ITEM_PATH.into(), props }, "without its argument: re-read");
}

#[test]
fn layout_decodes_checkmarks_radios_separators_submenus() {
    let root = t(
        0,
        &[("children-display", V::S("submenu"))],
        vec![
            t(1, &[("label", V::S("_Open")), ("icon-data", V::Bytes(4096)), ("shortcut", V::S("ignored"))], vec![]),
            t(2, &[("label", V::S("Mute")), ("toggle-type", V::S("checkmark")), ("toggle-state", V::I(1))], vec![]),
            t(3, &[("label", V::S("Low")), ("toggle-type", V::S("radio")), ("toggle-state", V::I(0))], vec![]),
            t(4, &[("type", V::S("separator"))], vec![]),
            t(5, &[("label", V::S("Disabled")), ("enabled", V::B(false))], vec![]),
            t(6, &[("label", V::S("Hidden")), ("visible", V::B(false))], vec![]),
            t(7, &[("label", V::S("More")), ("children-display", V::S("submenu"))], vec![]),
            t(8, &[("label", V::I(5)), ("enabled", V::S("yes"))], vec![]),
        ],
    );
    let l = decoded(&layout_reply(9, &root));
    assert_eq!((l.revision, l.len(), l.depth()), (9, 8, 1));
    let n = |id| l.node(id).unwrap();
    assert_eq!(n(1).label(), "Open");
    assert_eq!(n(1).props, MenuProps { label: Some("Open".into()), ..MenuProps::default() }, "icon-data and shortcut never decoded");
    assert_eq!((n(2).props.toggle, n(2).props.toggle_state), (Some(Toggle::Checkmark), Some(1)));
    assert_eq!((n(3).props.toggle, n(3).props.toggle_state), (Some(Toggle::Radio), Some(0)));
    assert!(n(4).is_separator() && !n(1).is_separator());
    assert!(!n(5).is_enabled() && n(5).is_visible());
    assert!(!n(6).is_visible() && n(6).is_enabled());
    assert!(n(7).has_submenu() && n(7).children.is_empty(), "a submenu announced, its children fetched when opened");
    assert!(!n(1).has_submenu());
    assert_eq!(n(8).props, MenuProps::default(), "values of the wrong type are absent, the spec defaults apply");
    assert!(l.node(99).is_none());
}

#[test]
fn layout_bounded_to_500_entries_and_depth_8() {
    let wide = t(0, &[], (1..=501).map(|id| t(id, &[("label", V::S("entry"))], vec![])).collect());
    let l = decoded(&layout_reply(1, &wide));
    assert_eq!(l.len(), MAX_MENU_ENTRIES);
    assert!(l.node(500).is_some() && l.node(501).is_none(), "the first 500 kept, in order");
    let huge = t(0, &[], (1..=10_000).map(|id| t(id, &[], vec![])).collect());
    let huge = decoded(&layout_reply(1, &huge));
    assert_eq!(huge.len(), MAX_MENU_ENTRIES);
    assert!(huge.root.children.capacity() <= 2 * MAX_MENU_ENTRIES, "never 10,000 allocated");
    let deep = decoded(&layout_reply(1, &chain(9)));
    assert_eq!(deep.depth(), MAX_MENU_DEPTH);
    assert!(deep.node(8).is_some() && deep.node(9).is_none(), "depth 9 dropped");
    assert_eq!(decoded(&layout_reply(1, &chain(8))).depth(), 8, "depth 8 kept whole");
}

#[test]
fn graft_and_patch_keep_the_bounds() {
    let mut l = decoded(&layout_reply(1, &t(0, &[], (1..=499).map(|id| t(id, &[("label", V::S("x"))], vec![])).collect())));
    let sub = decoded(&layout_reply(2, &t(7, &[("label", V::S("More"))], (1000..1010).map(|id| t(id, &[], vec![])).collect())));
    assert!(l.graft(sub.root));
    assert_eq!(l.len(), MAX_MENU_ENTRIES, "499 + 10 grafted, cut to 500");
    assert_eq!(l.node(7).unwrap().label(), "More");
    assert!(l.node(1009).is_some(), "the grafted submenu whole, in document order");
    assert!(l.node(490).is_some() && l.node(491).is_none(), "the last nine top entries dropped");
    assert!(!l.graft(MenuNode { id: 4242, props: MenuProps::default(), children: vec![] }), "unknown parent");
    let mut deep = decoded(&layout_reply(1, &chain(7)));
    let below = decoded(&layout_reply(1, &chain(4)));
    let mut grafted = below.root;
    grafted.id = 7;
    assert!(deep.graft(grafted));
    assert_eq!(deep.depth(), MAX_MENU_DEPTH, "a graft deep down is cut at depth 8");
    let mut p = decoded(&layout_reply(1, &t(0, &[], vec![t(1, &[("label", V::S("Mute")), ("enabled", V::B(false))], vec![])])));
    let update = MenuProps { label: Some("Unmute".into()), toggle: Some(Toggle::Checkmark), ..MenuProps::default() };
    p.patch(&[(1, update), (99, MenuProps::default())], &[(1, vec!["enabled".into()])]);
    let n = p.node(1).unwrap();
    assert_eq!((n.label(), n.props.toggle, n.is_enabled()), ("Unmute", Some(Toggle::Checkmark), true));
}

#[test]
fn graft_keeps_fetched_submenus_across_a_reread() {
    let more = |children| t(7, &[("label", V::S("More")), ("children-display", V::S("submenu"))], children);
    let mut l = decoded(&layout_reply(1, &t(0, &[], vec![t(1, &[("label", V::S("Open"))], vec![]), more(vec![])])));
    let sub = decoded(&layout_reply(1, &more((70..72).map(|id| t(id, &[("label", V::S("x"))], vec![])).collect())));
    assert!(l.graft(sub.root));
    assert!(l.node(70).is_some(), "submenu 7 fetched");
    // LayoutUpdated(parent 0): the root re-read at depth 1 announces 7 again, without children.
    let reread = decoded(&layout_reply(2, &t(0, &[], vec![t(1, &[("label", V::S("Open"))], vec![]), more(vec![])])));
    assert!(l.graft(reread.root));
    assert_eq!(l.node(7).unwrap().children.len(), 2, "the fetched children carried over");
    assert_eq!(l.len(), 4);
    // 7 is no longer a submenu: what was fetched under it goes.
    let flat = decoded(&layout_reply(3, &t(0, &[], vec![t(7, &[("label", V::S("More"))], vec![])])));
    assert!(l.graft(flat.root));
    assert!(l.node(70).is_none() && l.len() == 1, "not carried over to a plain entry");
}

#[test]
fn labels_lose_mnemonics_and_controls() {
    assert_eq!(label("_File"), "File");
    assert_eq!(label("Save __as"), "Save _as");
    assert_eq!(label("a\u{7}b"), "ab");
    assert_eq!(label("_Quit _Now"), "Quit Now");
    assert_eq!(clean("tab\there\u{1b}[31m red"), "tab here[31m red", "escape sequences lose their ESC");
    assert_eq!(clean("one\ntwo\r\nthree"), "one two  three", "line breaks become spaces");
    assert_eq!(clean("two  spaces"), "two  spaces");
    assert_eq!(clean("ab\u{202e}cd\u{200b}e\u{2066}f\u{2069}"), "abcdef", "bidi overrides and zero-width characters removed");
    assert_eq!(clean("a\u{200c}b\u{200d}c"), "a\u{200c}b\u{200d}c", "ZWNJ/ZWJ are legitimate text shaping (Arabic, emoji sequences), kept");
    assert_eq!(clean("a\u{61c}b"), "ab", "the Arabic Letter Mark is a bidi control too, removed");
}

// ---- MPRIS (media spec § 3) ----

/// A property value, written inside its variant by `write`.
#[derive(Clone)]
enum W {
    S(String),
    O(&'static str),
    As(Vec<String>),
    X(i64),
    I(i32),
    U(u32),
    T(u64),
    D(f64),
    B(bool),
    Bytes(usize),
    Map(Vec<(String, W)>),
}

fn st(s: &str) -> W {
    W::S(s.into())
}

fn map(entries: &[(&str, W)]) -> W {
    W::Map(entries.iter().map(|(k, w)| (k.to_string(), w.clone())).collect())
}

fn write(e: &mut IterAppend<'_>, w: &W) {
    match w {
        W::S(s) => e.append(Variant(s.as_str())),
        W::O(p) => e.append(Variant(Path::new(*p).unwrap())),
        W::As(v) => e.append(Variant(v.clone())),
        W::X(n) => e.append(Variant(*n)),
        W::I(n) => e.append(Variant(*n)),
        W::U(n) => e.append(Variant(*n)),
        W::T(n) => e.append(Variant(*n)),
        W::D(n) => e.append(Variant(*n)),
        W::B(b) => e.append(Variant(*b)),
        W::Bytes(n) => e.append(Variant(vec![0u8; *n])),
        W::Map(entries) => e.append_variant(&signature("a{sv}"), |v| dict(v, entries)),
    }
}

/// An `a{sv}`.
fn dict<K: AsRef<str>>(i: &mut IterAppend<'_>, entries: &[(K, W)]) {
    i.append_dict(&signature("s"), &signature("v"), |d| {
        for (key, w) in entries {
            d.append_dict_entry(|e| {
                e.append(key.as_ref());
                write(e, w);
            });
        }
    });
}

/// A player's answer to one of our calls: a method return whose body `f` writes.
fn returned(f: impl FnOnce(&mut IterAppend<'_>)) -> Message {
    let mut call = Message::new_method_call(":1.42", MPRIS_PATH, PROPERTIES_INTERFACE, "GetAll").unwrap();
    call.set_serial(7);
    let mut r = call.method_return();
    f(&mut IterAppend::new(&mut r));
    r
}

fn get_all() -> Call {
    Call::PlayerGetAll { dest: ":1.42".into() }
}

/// A player's PropertiesChanged, broadcast from :1.42.
fn changed_signal(f: impl FnOnce(&mut IterAppend<'_>)) -> Message {
    let mut m = Message::new_signal(MPRIS_PATH, PROPERTIES_INTERFACE, "PropertiesChanged").unwrap();
    m.set_sender(Some(":1.42".into()));
    f(&mut IterAppend::new(&mut m));
    m
}

#[test]
fn get_all_decodes_a_player() {
    let m = returned(|i| {
        dict(
            i,
            &[
                ("PlaybackStatus", st("Playing")),
                ("Rate", W::D(1.0)),
                ("CanGoNext", W::B(true)),
                ("Position", W::X(42_000_000)),
                (
                    "Metadata",
                    map(&[
                        ("mpris:trackid", W::O("/org/mpris/MediaPlayer2/Track/1")),
                        ("xesam:title", st("Bohemian Rhapsody")),
                        ("xesam:artist", W::As(vec!["Queen".into()])),
                        ("mpris:length", W::X(355_000_000)),
                        ("mpris:artUrl", W::Bytes(16)),
                    ]),
                ),
            ],
        );
    });
    let text = |s: &str| Value::Str(s.into());
    let want = vec![
        ("PlaybackStatus".to_string(), text("Playing")),
        ("Rate".into(), Value::Float(1.0)),
        ("CanGoNext".into(), Value::Bool(true)),
        ("Position".into(), Value::Int(42_000_000)),
        (
            "Metadata".into(),
            Value::Map(vec![
                ("mpris:trackid".into(), text("/org/mpris/MediaPlayer2/Track/1")),
                ("xesam:title".into(), text("Bohemian Rhapsody")),
                ("xesam:artist".into(), Value::StrList(vec!["Queen".into()])),
                ("mpris:length".into(), Value::Int(355_000_000)),
                ("mpris:artUrl".into(), Value::Other),
            ]),
        ),
    ];
    assert_eq!(reply(&get_all(), &m), Ok(Reply::Props(want)));
}

/// The codec keeps the type it found; `media::Track` decides what it means (spec § 3, Tolerance).
#[test]
fn every_integer_type_is_an_int_and_odd_types_are_other() {
    let length = |w: W| {
        let m = returned(|i| dict(i, &[("Metadata", map(&[("mpris:length", w)]))]));
        match reply(&get_all(), &m) {
            Ok(Reply::Props(p)) => match &p[..] {
                [(_, Value::Map(meta))] => meta[0].1.clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(length(W::X(-5)), Value::Int(-5), "kept as sent: the model calls it unknown");
    assert_eq!(length(W::I(7)), Value::Int(7));
    assert_eq!(length(W::U(8)), Value::Int(8));
    assert_eq!(length(W::T(9)), Value::Int(9));
    assert_eq!(length(W::T(u64::MAX)), Value::Other, "beyond i64");
    assert_eq!(length(W::D(1.5)), Value::Float(1.5));
    assert_eq!(length(st("355000000")), Value::Str("355000000".into()), "a string stays a string; the model: unknown");
    assert_eq!(length(W::Bytes(4)), Value::Other);
    assert_eq!(length(map(&[("deeper", W::B(true))])), Value::Other, "no map below the metadata");
    assert_eq!(length(W::As(vec![])), Value::StrList(vec![]), "an empty artist list stays empty");
}

/// A 100 KB title is cut while decoding; lists, maps and keys are bounded.
#[test]
fn player_strings_lists_and_maps_are_bounded() {
    let huge = "é\n".repeat(50_000);
    assert!(huge.len() >= 100_000);
    let artists: Vec<String> = (0..1000).map(|i| format!("Artist {i}")).collect();
    let m = returned(|i| {
        let mut entries =
            vec![("Metadata".to_string(), map(&[("xesam:title", W::S(huge.clone())), ("xesam:artist", W::As(artists.clone()))]))];
        entries.extend((0..200).map(|n| (format!("X-{n}"), W::B(true))));
        dict(i, &entries);
    });
    let Ok(Reply::Props(p)) = reply(&get_all(), &m) else { panic!("not decoded") };
    assert_eq!(p.len(), MAX_ENTRIES, "the first 64 entries");
    let Value::Map(meta) = &p[0].1 else { panic!("{:?}", p[0]) };
    let Value::Str(title) = &meta[0].1 else { panic!("{:?}", meta[0]) };
    assert_eq!(title.chars().count(), MAX_TEXT);
    assert!(!title.contains('\n'), "line breaks spaced");
    assert_eq!(meta[1].1, Value::StrList((0..MAX_LIST).map(|i| format!("Artist {i}")).collect()), "the first 32 artists");
    let long_key = "k".repeat(MAX_TEXT + 1);
    let m = returned(|i| dict(i, &[(long_key.as_str(), W::B(true)), ("CanPlay", W::B(true))]));
    assert_eq!(reply(&get_all(), &m), Ok(Reply::Props(vec![("CanPlay".into(), Value::Bool(true))])), "a key too long: skipped, never cut");
}

/// Only `Metadata` may be a map; an `a{sv}` under any other key is not read into one.
#[test]
fn only_metadata_is_read_as_a_map() {
    let m = returned(|i| {
        dict(
            i,
            &[("Metadata".to_string(), map(&[("xesam:title", W::S("Song".into()))])), ("X-Extra".to_string(), map(&[("k", W::B(true))]))],
        );
    });
    let Ok(Reply::Props(p)) = reply(&get_all(), &m) else { panic!("not decoded") };
    assert_eq!(p[0], ("Metadata".into(), Value::Map(vec![("xesam:title".into(), Value::Str("Song".into()))])));
    assert_eq!(p[1], ("X-Extra".into(), Value::Other), "another key's map is not read");
}

/// The cap counts entries read, not entries kept, so a reply full of keys
/// too long to keep stops after MAX_ENTRIES of them instead of being walked to its end.
#[test]
fn the_entry_cap_counts_what_is_read_not_what_is_kept() {
    let long_key = "k".repeat(MAX_TEXT + 1);
    let m = returned(|i| {
        let mut entries: Vec<(String, W)> = (0..MAX_ENTRIES).map(|n| (format!("{long_key}{n}"), W::B(true))).collect();
        entries.push(("CanPlay".into(), W::B(true)));
        dict(i, &entries);
    });
    assert_eq!(reply(&get_all(), &m), Ok(Reply::Props(vec![])), "64 entries read, all skipped: the 65th is never reached");
}

#[test]
fn player_gets_decode_their_one_value() {
    let dest = || ":1.42".to_string();
    let meta = returned(|i| write(i, &map(&[("xesam:title", st("Song"))])));
    assert_eq!(
        reply(&Call::PlayerMetadata { dest: dest() }, &meta),
        Ok(Reply::Props(vec![("Metadata".into(), Value::Map(vec![("xesam:title".into(), Value::Str("Song".into()))]))]))
    );
    assert_eq!(reply(&Call::PlayerIdentity { dest: dest() }, &returned(|i| write(i, &st("Spotify")))), Ok(Reply::Text("Spotify".into())));
    let position = |w: W| reply(&Call::PlayerPosition { dest: dest() }, &returned(|i| write(i, &w)));
    assert_eq!(position(W::X(102_000_000)), Ok(Reply::Position(102_000_000)));
    assert_eq!(position(W::U(5)), Ok(Reply::Position(5)), "any integer type");
    let bad_reply = |r: Result<Reply, CallError>| matches!(r, Err(CallError::BadReply(_)));
    assert!(bad_reply(position(st("1:42"))));
    assert!(bad_reply(reply(&Call::PlayerPosition { dest: dest() }, &returned(|i| i.append(5i64)))), "not in a variant");
    assert!(bad_reply(reply(&Call::PlayerMetadata { dest: dest() }, &returned(|i| write(i, &st("not a map"))))));
    assert!(bad_reply(reply(&Call::PlayerIdentity { dest: dest() }, &returned(|i| write(i, &W::I(3))))));
    assert!(bad_reply(reply(&get_all(), &returned(|i| i.append("Playing")))));
    assert_eq!(reply(&Call::PlayerControl { dest: dest(), member: Control::Next }, &returned(|_| {})), Ok(Reply::Unit));
    let mut call = Message::new_method_call(":1.42", MPRIS_PATH, PROPERTIES_INTERFACE, "GetAll").unwrap();
    call.set_serial(3);
    let gone = call.error(&"org.freedesktop.DBus.Error.ServiceUnknown".into(), c"gone");
    assert_eq!(
        reply(&get_all(), &gone),
        Err(CallError::Remote("org.freedesktop.DBus.Error.ServiceUnknown".into())),
        "the consumer removes the player"
    );
}

#[test]
fn properties_changed_is_read_for_the_player_interface_only() {
    let m = changed_signal(|i| {
        i.append(MPRIS_PLAYER_INTERFACE);
        dict(i, &[("PlaybackStatus", st("Paused"))]);
        i.append(vec!["Metadata"]);
    });
    assert_eq!(
        signal(&m),
        Signal::PlayerProps {
            sender: ":1.42".into(),
            changed: vec![("PlaybackStatus".into(), Value::Str("Paused".into()))],
            invalidated: vec!["Metadata".into()]
        }
    );
    let short = changed_signal(|i| {
        i.append(MPRIS_PLAYER_INTERFACE);
        dict(i, &[("Rate", W::D(1.0))]);
    });
    assert_eq!(
        signal(&short),
        Signal::PlayerProps { sender: ":1.42".into(), changed: vec![("Rate".into(), Value::Float(1.0))], invalidated: vec![] },
        "without its invalidated list"
    );
    let root = changed_signal(|i| {
        i.append(crate::session_bus::MPRIS_INTERFACE);
        dict(i, &[("Identity", st("Spotify"))]);
        i.append(Vec::<&str>::new());
    });
    assert_eq!(signal(&root), Signal::Other, "the root interface is not read");
    let elsewhere = signal_from(":1.42", "/org/mpris/MediaPlayer2/other", PROPERTIES_INTERFACE, "PropertiesChanged", |i| {
        i.append(MPRIS_PLAYER_INTERFACE);
        dict(i, &[("Rate", W::D(1.0))]);
        i.append(Vec::<&str>::new());
    });
    assert_eq!(elsewhere, Signal::Other, "not the MPRIS object");
    assert_eq!(signal(&changed_signal(|i| i.append(MPRIS_PLAYER_INTERFACE))), Signal::Other, "no changed properties");
}

/// Like the portal's SettingChanged: a unicast PropertiesChanged or Seeked is a peer's, not a broadcast.
#[test]
fn a_unicast_player_signal_is_refused() {
    let mut m = changed_signal(|i| {
        i.append(MPRIS_PLAYER_INTERFACE);
        dict(i, &[("PlaybackStatus", st("Playing"))]);
        i.append(Vec::<&str>::new());
    });
    assert!(matches!(signal(&m), Signal::PlayerProps { .. }), "broadcast: read");
    m.set_destination(Some(":1.9".into()));
    assert_eq!(signal(&m), Signal::Other, "addressed to us alone");
    let mut s = Message::new_signal(MPRIS_PATH, MPRIS_PLAYER_INTERFACE, "Seeked").unwrap().append1(5_000_000i64);
    s.set_sender(Some(":1.42".into()));
    assert_eq!(signal(&s), Signal::Seeked { sender: ":1.42".into(), us: 5_000_000 }, "broadcast: read");
    s.set_destination(Some(":1.9".into()));
    assert_eq!(signal(&s), Signal::Other, "addressed to us alone");
}

#[test]
fn seeked_carries_its_position() {
    let seeked = |path: &str, f: &dyn Fn(&mut IterAppend<'_>)| signal_from(":1.42", path, MPRIS_PLAYER_INTERFACE, "Seeked", f);
    assert_eq!(seeked(MPRIS_PATH, &|i| i.append(-3i64)), Signal::Seeked { sender: ":1.42".into(), us: -3 }, "as sent: the model clamps");
    assert_eq!(seeked(MPRIS_PATH, &|i| i.append(3u32)), Signal::Other, "not an x");
    assert_eq!(seeked(MPRIS_PATH, &|_| {}), Signal::Other, "empty");
    assert_eq!(seeked("/elsewhere", &|i| i.append(3i64)), Signal::Other, "not the MPRIS object");
}

// ---- the notification server (notifications spec § 1, § 2) ----

/// A hint value, written in its variant.
enum H {
    S(&'static str),
    Y(u8),
    U(u32),
    I(i32),
    B(bool),
    /// `image-data`'s `(iiibiiay)`, its array holding this many bytes.
    Image(usize),
}

fn hint(e: &mut IterAppend<'_>, h: &H) {
    match h {
        H::S(s) => e.append(Variant(*s)),
        H::Y(n) => e.append(Variant(*n)),
        H::U(n) => e.append(Variant(*n)),
        H::I(n) => e.append(Variant(*n)),
        H::B(b) => e.append(Variant(*b)),
        H::Image(bytes) => e.append_variant(&signature("(iiibiiay)"), |v| {
            v.append_struct(|s| {
                s.append(64i32);
                s.append(64i32);
                s.append(256i32);
                s.append(true);
                s.append(8i32);
                s.append(4i32);
                s.append(vec![7u8; *bytes]);
            });
        }),
    }
}

/// An `a{sv}` of hints.
fn hints_dict(i: &mut IterAppend<'_>, hints: &[(&str, H)]) {
    i.append_dict(&signature("s"), &signature("v"), |d| {
        for (k, h) in hints {
            d.append_dict_entry(|e| {
                e.append(*k);
                hint(e, h);
            });
        }
    });
}

/// A call to the server as a client sends it (serial set, as received), its body written by `f`.
fn server_call(member: &str, f: impl FnOnce(&mut IterAppend<'_>)) -> Message {
    let mut m = Message::new_method_call(":1.1", NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, member).unwrap();
    m.set_serial(11);
    m.set_sender(Some(":1.42".into()));
    f(&mut IterAppend::new(&mut m));
    m
}

/// `Notify(app, replaces, "dialog-information", summary, body, actions, hints[, expire])`.
fn notify_msg(app: &str, replaces: u32, summary: &str, body: &str, actions: &[&str], hints: &[(&str, H)], expire: Option<i32>) -> Message {
    server_call("Notify", |i| {
        i.append(app);
        i.append(replaces);
        i.append("dialog-information");
        i.append(summary);
        i.append(body);
        i.append(actions.to_vec());
        hints_dict(i, hints);
        if let Some(t) = expire {
            i.append(t);
        }
    })
}

fn notified(m: &Message) -> Incoming {
    match inbound(m) {
        Inbound::Notify(n) => n,
        other => panic!("{other:?}"),
    }
}

#[test]
fn notify_decodes_what_notify_send_sends() {
    let m = notify_msg(
        "notify-send",
        0,
        "Hello",
        "<b>World</b> &amp; all",
        &["default", "Open", "later", "Later"],
        &[
            ("urgency", H::Y(2)),
            ("category", H::S("im.received")),
            ("desktop-entry", H::S("org.example.Chat")),
            ("x-canonical-private-synchronous", H::S("volume")),
            ("sender-pid", H::I(4242)),
        ],
        Some(3_000),
    );
    assert_eq!(
        notified(&m),
        Incoming {
            app_name: "notify-send".into(),
            replaces_id: 0,
            summary: "Hello".into(),
            body: "World & all".into(),
            actions: vec![Action { key: "default".into(), label: "Open".into() }, Action { key: "later".into(), label: "Later".into() }],
            urgency: Urgency::Critical,
            expire_ms: 3_000,
            desktop_entry: Some("org.example.Chat".into()),
            category: Some("im.received".into()),
            transient: false,
            resident: false,
            tag: Some("volume".into()),
        }
    );
    assert_eq!(notified(&notify_msg("", 7, "s", "", &[], &[], Some(-1))).replaces_id, 7, "the portal's empty app name, a replaces_id");
}

#[test]
fn a_missing_expire_timeout_is_minus_one() {
    assert_eq!(notified(&notify_msg("a", 0, "s", "", &[], &[], None)).expire_ms, -1);
    assert_eq!(notified(&notify_msg("a", 0, "s", "", &[], &[], Some(0))).expire_ms, 0);
}

#[test]
fn urgency_is_read_as_y_u_or_i_and_clamped() {
    let urgency = |h: H| notified(&notify_msg("a", 0, "s", "", &[], &[("urgency", h)], None)).urgency;
    assert_eq!(urgency(H::Y(0)), Urgency::Low);
    assert_eq!(urgency(H::Y(1)), Urgency::Normal);
    assert_eq!(urgency(H::Y(2)), Urgency::Critical);
    assert_eq!(urgency(H::U(2)), Urgency::Critical, "u, as some clients send it");
    assert_eq!(urgency(H::I(1)), Urgency::Normal, "i, as others do");
    assert_eq!(urgency(H::Y(200)), Urgency::Critical, "clamped to 2");
    assert_eq!(urgency(H::I(-4)), Urgency::Low, "clamped to 0");
    assert_eq!(urgency(H::S("critical")), Urgency::Normal, "a string: ignored");
    assert_eq!(notified(&notify_msg("a", 0, "s", "", &[], &[], None)).urgency, Urgency::Normal, "absent: normal");
}

#[test]
fn transient_and_resident_are_a_bool_or_an_integer() {
    let flags = |hs: &[(&str, H)]| {
        let n = notified(&notify_msg("a", 0, "s", "", &[], hs, None));
        (n.transient, n.resident)
    };
    assert_eq!(flags(&[("transient", H::B(true))]), (true, false));
    assert_eq!(flags(&[("transient", H::I(1))]), (true, false), "an integer");
    assert_eq!(flags(&[("transient", H::I(0))]), (false, false));
    assert_eq!(flags(&[("transient", H::U(7)), ("resident", H::B(true))]), (true, true));
    assert_eq!(flags(&[("resident", H::Y(1))]), (false, true));
    assert_eq!(flags(&[("transient", H::S("yes"))]), (false, false), "a string: ignored");
}

#[test]
fn image_data_is_stepped_over_and_the_hints_after_it_still_read() {
    for key in ["image-data", "image_data", "icon_data"] {
        let m = notify_msg(
            "a",
            0,
            "s",
            "",
            &[],
            &[(key, H::Image(64 * 1024)), ("image-path", H::S("/tmp/x.png")), ("urgency", H::Y(2)), ("desktop-entry", H::S("firefox"))],
            None,
        );
        let n = notified(&m);
        assert_eq!((n.urgency, n.desktop_entry.as_deref()), (Urgency::Critical, Some("firefox")), "{key}");
    }
}

#[test]
fn actions_come_in_pairs_at_most_6() {
    let keys = |a: &[&str]| notified(&notify_msg("a", 0, "s", "", a, &[], None)).actions.into_iter().map(|x| x.key).collect::<Vec<_>>();
    assert_eq!(keys(&["default", "Open", "dangling"]), ["default"], "an odd count: the last key has no label");
    assert_eq!(keys(&["default"]), Vec::<String>::new());
    assert_eq!(keys(&["", "Empty", "ok", "OK"]), ["ok"], "an empty key is skipped");
    assert_eq!(keys(&["a", "1", "a", "2"]), ["a"], "a key once");
    let mut late: Vec<&str> = std::iter::repeat_n(["", "x"], 40).flatten().collect();
    late.extend(["late", "Late"]);
    assert_eq!(keys(&late), Vec::<String>::new(), "only the first 32 pairs are ever read");
}

/// A Notify carrying 200 actions keeps the first 6.
#[test]
fn a_notify_with_200_actions_keeps_6() {
    let pairs: Vec<String> = (0..200).flat_map(|i| [format!("k{i}"), format!("Label {i}")]).collect();
    let refs: Vec<&str> = pairs.iter().map(String::as_str).collect();
    let n = notified(&notify_msg("a", 0, "s", "", &refs, &[], None));
    assert_eq!(n.actions.len(), MAX_ACTIONS);
    assert_eq!(n.actions[5], Action { key: "k5".into(), label: "Label 5".into() }, "the first 6, in order");
}

#[test]
fn notify_fields_are_cleaned_and_capped_while_decoding() {
    let huge = "é".repeat(60_000);
    let summary = format!("<b>{huge}</b>");
    let body = format!("line 1\n<i>{huge}</i>");
    let m = notify_msg(&huge, 0, &summary, &body, &["default", huge.as_str()], &[("desktop-entry", H::S(" firefox\n"))], None);
    let n = notified(&m);
    assert_eq!(n.app_name.len(), MAX_APP);
    assert_eq!(n.summary.len(), MAX_SUMMARY);
    assert!(n.body.starts_with("line 1\né") && n.body.len() <= MAX_BODY, "{}", n.body.len());
    assert_eq!(n.actions[0].label.len(), MAX_LABEL);
    assert_eq!(n.desktop_entry.as_deref(), Some("firefox"));
}

fn head(i: &mut IterAppend<'_>) {
    i.append("app");
    i.append(0u32);
    i.append("icon");
    i.append("summary");
    i.append("body");
}

fn no_hints(i: &mut IterAppend<'_>) {
    i.append_dict(&signature("s"), &signature("v"), |_| {});
}

#[test]
fn a_call_of_the_wrong_shape_is_refused() {
    let none: Vec<&str> = Vec::new();
    let ok = server_call("Notify", |i| {
        head(i);
        i.append(none.clone());
        no_hints(i);
        i.append(-1i32);
    });
    assert!(matches!(inbound(&ok), Inbound::Notify(_)), "the control case");
    let refused = [
        (
            "replaces_id as i",
            server_call("Notify", |i| {
                i.append("app");
                i.append(-1i32);
                i.append("icon");
                i.append("s");
                i.append("b");
                i.append(none.clone());
                no_hints(i);
                i.append(-1i32);
            }),
        ),
        (
            "actions as a string",
            server_call("Notify", |i| {
                head(i);
                i.append("default");
                no_hints(i);
                i.append(-1i32);
            }),
        ),
        (
            "hints as a{ss}",
            server_call("Notify", |i| {
                head(i);
                i.append(none.clone());
                i.append_dict(&signature("s"), &signature("s"), |d| {
                    d.append_dict_entry(|e| {
                        e.append("urgency");
                        e.append("2");
                    });
                });
                i.append(-1i32);
            }),
        ),
        (
            "no hints",
            server_call("Notify", |i| {
                head(i);
                i.append(none.clone());
            }),
        ),
        (
            "expire as u",
            server_call("Notify", |i| {
                head(i);
                i.append(none.clone());
                no_hints(i);
                i.append(5u32);
            }),
        ),
        (
            "a trailing argument",
            server_call("Notify", |i| {
                head(i);
                i.append(none.clone());
                no_hints(i);
                i.append(-1i32);
                i.append("extra");
            }),
        ),
        ("too few", server_call("Notify", head)),
        ("Close with a string", server_call("CloseNotification", |i| i.append("7"))),
        ("Close without its id", server_call("CloseNotification", |_| {})),
        (
            "Close with a trailing argument",
            server_call("CloseNotification", |i| {
                i.append(7u32);
                i.append("extra");
            }),
        ),
        ("GetCapabilities with an argument", server_call("GetCapabilities", |i| i.append("x"))),
        ("GetServerInformation with an argument", server_call("GetServerInformation", |i| i.append(1u32))),
        ("an unknown member", server_call("Bogus", |_| {})),
    ];
    for (what, m) in &refused {
        assert_eq!(inbound(m), Inbound::Unknown, "{what}");
    }
    assert_eq!(inbound(&server_call("CloseNotification", |i| i.append(7u32))), Inbound::Close(7));
    assert_eq!(inbound(&server_call("GetCapabilities", |_| {})), Inbound::Capabilities);
    assert_eq!(inbound(&server_call("GetServerInformation", |_| {})), Inbound::ServerInfo);
    let mut elsewhere =
        Message::new_method_call(":1.1", "/org/freedesktop/Notifications/x", NOTIFICATIONS_INTERFACE, "GetCapabilities").unwrap();
    elsewhere.set_serial(2);
    assert_eq!(inbound(&elsewhere), Inbound::Unknown, "another object");
    let mut introspect = Message::new_method_call(":1.1", NOTIFICATIONS_PATH, "org.freedesktop.DBus.Introspectable", "Introspect").unwrap();
    introspect.set_serial(3);
    assert_eq!(inbound(&introspect), Inbound::Unknown, "another interface");
    let signal = Message::new_signal(NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, "GetCapabilities").unwrap();
    assert_eq!(inbound(&signal), Inbound::Unknown, "a signal is no call");
}

#[test]
fn name_acquired_and_lost_come_from_the_bus_driver_to_us_alone() {
    let named = |member: &str, sender: &str, dest: Option<&str>, f: &dyn Fn(&mut IterAppend<'_>)| {
        let mut m = Message::new_signal("/org/freedesktop/DBus", "org.freedesktop.DBus", member).unwrap();
        m.set_sender(Some(sender.into()));
        m.set_destination(dest.map(|d| d.into()));
        f(&mut IterAppend::new(&mut m));
        signal(&m)
    };
    let driver = "org.freedesktop.DBus";
    assert_eq!(
        named("NameAcquired", driver, Some(":1.5"), &|i| i.append(NOTIFICATIONS_NAME)),
        Signal::NameAcquired(NOTIFICATIONS_NAME.into())
    );
    assert_eq!(named("NameLost", driver, Some(":1.5"), &|i| i.append(NOTIFICATIONS_NAME)), Signal::NameLost(NOTIFICATIONS_NAME.into()));
    assert_eq!(
        named("NameAcquired", ":1.9", Some(":1.5"), &|i| i.append(NOTIFICATIONS_NAME)),
        Signal::Other,
        "a peer posing as the bus driver"
    );
    assert_eq!(named("NameLost", driver, None, &|i| i.append(NOTIFICATIONS_NAME)), Signal::Other, "a broadcast is not addressed to us");
    assert_eq!(named("NameAcquired", driver, Some(":1.5"), &|i| i.append(5u32)), Signal::Other, "not a name");
    assert_eq!(named("NameLost", driver, Some(":1.5"), &|_| {}), Signal::Other, "an empty body, sent to us");
    assert_eq!(
        named("NameAcquired", driver, Some(":1.5"), &|i| i.append(WATCHER_NAME)),
        Signal::NameAcquired(WATCHER_NAME.into()),
        "the watcher's comes too: the consumer reads arg0"
    );
}

#[test]
fn long_text_is_cut_to_256_characters() {
    let hostile: String = "é\u{1}x\n".repeat(2_600);
    assert!(hostile.len() > 10_000);
    let title = reply(&get(Prop::Title), &body(|i| i.append(Variant(hostile.as_str()))));
    let Ok(Reply::Prop(PropValue::Text(t))) = title else { panic!("{title:?}") };
    assert_eq!(t.chars().count(), MAX_TEXT);
    assert!(t.chars().all(|c| c == 'é' || c == 'x' || c == ' '), "controls removed, line breaks spaced");
    assert_eq!(label(&"_x".repeat(10_000)), "x".repeat(MAX_TEXT), "labels too");
}

#[test]
fn the_failed_count_is_one_u32_in_a_variant() {
    use crate::session_bus::SYSTEMD_PATH;
    let call = Call::Get { destination: ":1.30".into(), path: SYSTEMD_PATH.into(), property: Property::NFailedUnits };
    assert_eq!(reply(&call, &body(|i| i.append(Variant(3u32)))), Ok(Reply::Count(3)));
    assert!(matches!(reply(&call, &body(|i| i.append(Variant("3")))), Err(CallError::BadReply(_))), "not a u32");
    assert!(matches!(reply(&call, &body(|i| i.append(3u32))), Err(CallError::BadReply(_))), "not in a variant");
    assert_eq!(reply(&Call::SubscribeUnits { dest: ":1.30".into() }, &body(|_| {})), Ok(Reply::Unit));
}

#[test]
fn the_managers_properties_changed_is_the_units_signal_and_nothing_else() {
    use crate::session_bus::{PROPERTIES_INTERFACE, SYSTEMD_MANAGER, SYSTEMD_PATH};
    // Never decoded beyond the interface name: the dict may carry the Environment.
    let changed = |path: &str, interface: &str| {
        signal_from(":1.30", path, PROPERTIES_INTERFACE, "PropertiesChanged", |i| {
            i.append(interface);
            i.append(std::collections::HashMap::<String, Variant<u32>>::from([("NFailedUnits".to_string(), Variant(2u32))]));
            i.append(Vec::<String>::new());
        })
    };
    assert_eq!(changed(SYSTEMD_PATH, SYSTEMD_MANAGER), Signal::UnitsChanged { sender: ":1.30".into() });
    assert_eq!(changed(SYSTEMD_PATH, "org.freedesktop.systemd1.Unit"), Signal::Other, "another interface");
    assert_eq!(changed("/org/freedesktop/systemd1/unit/a", SYSTEMD_MANAGER), Signal::Other, "another object");
    let mut m = Message::new_signal(SYSTEMD_PATH, PROPERTIES_INTERFACE, "PropertiesChanged").unwrap();
    m.set_sender(Some(":1.30".into()));
    let mut m = m.append1(SYSTEMD_MANAGER);
    m.set_destination(Some(":1.9".into()));
    assert_eq!(signal(&m), Signal::Other, "unicast: a peer's, not the rule's");
}
