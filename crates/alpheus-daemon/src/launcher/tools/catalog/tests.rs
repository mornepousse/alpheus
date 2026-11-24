use super::*;
use crate::launcher::config::LauncherConfig;
use alpheus_common::launcher_ipc::CaptureVerb;

fn tools(text: &str) -> ToolsConfig {
    LauncherConfig::from_text(text).unwrap().tools
}

/// Everything shown.
fn all() -> Context {
    Context {
        quick: true,
        notifications: true,
        timer: true,
        keep_awake: true,
        night_light: true,
        lock: true,
        session: true,
        reload: true,
        shield: true,
        clipboard: true,
        diagnostics: true,
        nmap: true,
        usb_reset: true,
        shutdown: true,
        shutdown_left: None,
        captures: true,
        recording: true,
    }
}

fn build(text: &str, c: &Context) -> (Catalog, Vec<String>) {
    let mut notes = Vec::new();
    let cat = Catalog::build(&tools(text), c, &mut |n| notes.push(n.to_string()));
    (cat, notes)
}

fn ids_shown(c: &Context) -> Vec<&'static str> {
    BUILTINS.iter().filter(|b| shown(b, c)).map(|b| b.id).collect()
}

fn names(cat: &Catalog, rows: &[usize]) -> Vec<String> {
    rows.iter().map(|&i| cat.entries[i].name.clone()).collect()
}

fn category_names(cat: &Catalog, items: &[Item]) -> Vec<String> {
    items
        .iter()
        .map(|i| match *i {
            Item::Category(c) => format!("[{}]", cat.categories[c].name),
            Item::Entry(e) => cat.entries[e].name.clone(),
        })
        .collect()
}

#[test]
fn the_table_is_the_specs() {
    let ids: Vec<&str> = BUILTINS.iter().map(|b| b.id).collect();
    assert_eq!(
        ids,
        [
            "quick.sound",
            "quick.wifi",
            "quick.bluetooth",
            "quick.vpn",
            "quick.shield",
            "dnd",
            "keep_awake",
            "night_light",
            "tab.home",
            "tab.sound",
            "tab.network",
            "tab.bluetooth",
            "tab.power",
            "tab.displays",
            "tab.appearance",
            "tab.diagnostics",
            "notes",
            "timer",
            "clipboard",
            "emoji",
            "capture.full",
            "capture.area",
            "capture.area_save",
            "capture.annotate",
            "capture.record",
            "capture.record_area",
            "capture.stop",
            "capture.pick",
            "capture.ocr",
            "shutdown",
            "process.kill",
            "lock",
            "power",
            "reload",
            "diag.scan",
            "diag.usb_events",
            "diag.usb_reset",
            "diag.preset.errors",
            "diag.preset.usb",
            "diag.preset.suspend",
            "diag.preset.oom",
            "diag.preset.thermal",
            "diag.preset.network",
            "diag.preset.live",
            "diag.preset.service",
        ]
    );
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "ids are unique");
    let labels: Vec<&str> = BUILTINS.iter().map(|b| b.label).collect();
    assert_eq!(
        labels,
        [
            "Sound",
            "Wi-Fi",
            "Bluetooth",
            "VPN",
            "Shield",
            "Do not disturb",
            "Keep awake",
            "Night light",
            "Home",
            "Sound",
            "Network",
            "Bluetooth",
            "Power",
            "Displays",
            "Theme",
            "Diagnostics",
            "Notes",
            "Timer",
            "Clipboard",
            "Emoji",
            "Screenshot",
            "Area",
            "Area (save)",
            "Annotate",
            "Record screen",
            "Record area",
            "Stop recording",
            "Colour picker",
            "Text from area (OCR)",
            "Schedule shutdown",
            "Kill a process",
            "Lock",
            "Power menu",
            "Reload the bar",
            "Network scan",
            "USB events",
            "Reset USB (xHCI)",
            "Errors this boot",
            "USB",
            "Suspend and resume",
            "Out of memory",
            "Thermal and throttling",
            "NetworkManager",
            "Live journal",
            "Logs of a service…",
        ]
    );
    let categories: Vec<&str> = BUILTINS.iter().map(|b| b.category).collect();
    let [quick, control, utilities, captures, system, session, diagnostics] = BUILTIN_CATEGORIES;
    assert_eq!(captures, "Captures");
    let mut expected = vec![quick; 8];
    expected.extend([control; 8]);
    expected.extend([utilities; 4]);
    expected.extend([captures; 9]);
    expected.extend([system; 2]);
    expected.extend([session; 3]);
    expected.extend([diagnostics; 11]);
    assert_eq!(categories, expected);
    // System sits beside Session: 40 is the Captures category's.
    let orders: Vec<(&str, u16)> = BUILTIN_CATEGORIES.into_iter().zip(BUILTIN_ORDERS).collect();
    assert_eq!(orders, [(quick, 10), (control, 20), (utilities, 30), (captures, 40), (system, 80), (session, 90), (diagnostics, 70)]);
    let slugs: Vec<&str> = BUILTINS.iter().filter_map(|b| b.tab_slug()).collect();
    assert_eq!(slugs, ["home", "sound", "network", "bluetooth", "power", "displays", "appearance", "diagnostics"]);
    assert!(BUILTINS.iter().all(|b| b.glyph != ' ' && !b.glyph.is_ascii()), "every built-in has a Nerd Font glyph");
    // The verbs they replay.
    let action = |id: &str| BUILTINS.iter().find(|b| b.id == id).unwrap().action;
    assert_eq!(action("quick.wifi"), BuiltinAction::Quick(QuickVerb::Wifi));
    assert_eq!(action("quick.sound"), BuiltinAction::Quick(QuickVerb::Sound));
    assert_eq!(action("quick.bluetooth"), BuiltinAction::Quick(QuickVerb::Bluetooth));
    assert_eq!(action("quick.vpn"), BuiltinAction::Quick(QuickVerb::Vpn));
    assert_eq!(action("keep_awake"), BuiltinAction::Toggle(Toggle::KeepAwake));
    assert_eq!(action("night_light"), BuiltinAction::Toggle(Toggle::NightLight));
    assert_eq!(action("tab.network"), BuiltinAction::Tab);
    assert_eq!(action("quick.shield"), BuiltinAction::Shield);
    assert_eq!(action("process.kill"), BuiltinAction::Kill);
    // Each capture entry replays the verb of its own name.
    for (id, verb) in [
        ("capture.full", CaptureVerb::Full),
        ("capture.area", CaptureVerb::Area),
        ("capture.area_save", CaptureVerb::AreaSave),
        ("capture.annotate", CaptureVerb::Annotate),
        ("capture.record", CaptureVerb::Record),
        ("capture.record_area", CaptureVerb::RecordArea),
        ("capture.stop", CaptureVerb::Stop),
        ("capture.pick", CaptureVerb::Pick),
        ("capture.ocr", CaptureVerb::Ocr),
    ] {
        assert_eq!(action(id), BuiltinAction::Capture(verb), "{id}");
    }
}

#[test]
fn shown_follows_the_context() {
    let none = Context::default();
    assert_eq!(
        ids_shown(&none),
        [
            "tab.home",
            "tab.sound",
            "tab.network",
            "tab.bluetooth",
            "tab.power",
            "tab.displays",
            "tab.appearance",
            "notes",
            "emoji",
            "process.kill",
            "diag.usb_events"
        ],
        "the seven tabs (not diagnostics), notes, emoji, the kill list and the USB events"
    );
    type Flip = (fn(&mut Context), &'static [&'static str]);
    let flips: [Flip; 15] = [
        (|c| c.quick = true, &["quick.sound", "quick.wifi", "quick.bluetooth", "quick.vpn"]),
        (|c| c.notifications = true, &["dnd"]),
        (|c| c.timer = true, &["timer"]),
        (|c| c.keep_awake = true, &["keep_awake"]),
        (|c| c.night_light = true, &["night_light"]),
        (|c| c.lock = true, &["lock"]),
        (|c| c.session = true, &["power"]),
        (|c| c.reload = true, &["reload"]),
        (|c| c.shield = true, &["quick.shield"]),
        (|c| c.clipboard = true, &["clipboard"]),
        (
            |c| c.diagnostics = true,
            &[
                "tab.diagnostics",
                "diag.preset.errors",
                "diag.preset.usb",
                "diag.preset.suspend",
                "diag.preset.oom",
                "diag.preset.thermal",
                "diag.preset.network",
                "diag.preset.live",
                "diag.preset.service",
            ],
        ),
        (|c| c.nmap = true, &["diag.scan"]),
        (|c| c.usb_reset = true, &["diag.usb_reset"]),
        (|c| c.shutdown = true, &["shutdown"]),
        (
            |c| {
                c.captures = true;
                c.recording = true;
            },
            &["capture.full", "capture.area", "capture.area_save"],
        ),
    ];
    for (flip, rows) in flips {
        let mut c = Context::default();
        flip(&mut c);
        let extra: Vec<&str> = ids_shown(&c).into_iter().filter(|i| !ids_shown(&none).contains(i)).collect();
        assert_eq!(extra, rows);
        let mut off = all();
        // The same flag off alone hides exactly those rows.
        let before = ids_shown(&off);
        flip_off(&mut off, rows);
        assert_eq!(before.len() - ids_shown(&off).len(), rows.len());
    }
    assert_eq!(ids_shown(&all()).len(), 39, "the table's 45, less the six captures not built yet");
}

/// `all()` with the flag behind `rows` turned off.
fn flip_off(c: &mut Context, rows: &[&str]) {
    match rows[0] {
        "quick.sound" => c.quick = false,
        "dnd" => c.notifications = false,
        "timer" => c.timer = false,
        "keep_awake" => c.keep_awake = false,
        "night_light" => c.night_light = false,
        "lock" => c.lock = false,
        "power" => c.session = false,
        "reload" => c.reload = false,
        "quick.shield" => c.shield = false,
        "clipboard" => c.clipboard = false,
        "tab.diagnostics" => c.diagnostics = false,
        "diag.scan" => c.nmap = false,
        "diag.usb_reset" => c.usb_reset = false,
        "shutdown" => c.shutdown = false,
        "capture.full" => c.captures = false,
        other => panic!("{other}"),
    }
}

fn builtin_ids(cat: &Catalog) -> Vec<&'static str> {
    cat.entries
        .iter()
        .filter_map(|e| match e.kind {
            EntryKind::Builtin(i) => BUILTINS.get(i).map(|b| b.id),
            EntryKind::User(_) => None,
        })
        .collect()
}

#[test]
fn hide_and_builtins_false() {
    let (cat, notes) = build("", &all());
    assert_eq!(builtin_ids(&cat).len(), 39);
    assert!(notes.is_empty());
    let (cat, _) = build("[tools]\nhide = [\"tab.power\", \"notes\"]\n", &all());
    let ids = builtin_ids(&cat);
    assert_eq!(ids.len(), 37);
    assert!(!ids.contains(&"tab.power") && !ids.contains(&"notes"));
    let (cat, _) = build("[tools]\nbuiltins = false\nroot = [\"notes\"]\n[[tools.entry]]\nname = \"Mine\"\nrun = [\"x\"]\n", &all());
    assert!(builtin_ids(&cat).is_empty(), "no built-in, not even a root one");
    assert_eq!(names(&cat, &cat.root), Vec::<String>::new());
    assert_eq!(cat.entries.len(), 1, "user entries stay");
}

#[test]
fn unknown_ids_are_noted_once() {
    let (cat, notes) = build("[tools]\nhide = [\"nope\", \"also.nope\", \"notes\"]\nroot = [\"nope\", \"zip\"]\n", &all());
    assert_eq!(notes, ["tools: unknown built-in id in [tools] hide", "tools: unknown built-in id in [tools] root"]);
    assert!(!builtin_ids(&cat).contains(&"notes"), "the known one is still hidden");
    let (_, notes) = build("[tools]\nhide = [\"quick.shield\"]\nroot = [\"emoji\"]\n", &Context::default());
    assert!(notes.is_empty(), "a known id is fine even when not shown, and hide of a never-shown row too");
    assert!(notes.iter().all(|n| !n.contains("nope")), "ids are never quoted");
}

#[test]
fn root_rows_order() {
    let text = "
[tools]
root = [\"dnd\", \"keep_awake\", \"notes\"]
[[tools.category]]
name = \"Nix\"
order = 15
[[tools.category]]
name = \"Zed\"
order = 30
[[tools.entry]]
name = \"Alpha\"
order = 30
run = [\"x\"]
[[tools.entry]]
name = \"Beta\"
run = [\"x\"]
[[tools.entry]]
name = \"Inside\"
category = \"Nix\"
run = [\"x\"]
[[tools.entry]]
name = \"InZed\"
category = \"Zed\"
run = [\"x\"]
";
    let (cat, _) = build(text, &all());
    // The listed built-ins first, in the order given; then the categories and root entries by
    // order (10 Quick settings, 15 Nix, 20 Control center, 30 Alpha | Utilities | Zed by name,
    // 40 Captures, 50 Beta, 70 Diagnostics, 80 System, 90 Session), names breaking the tie.
    let rows = cat.root_rows();
    assert_eq!(
        category_names(&cat, &rows),
        [
            "Do not disturb",
            "Keep awake",
            "Notes",
            "[Quick settings]",
            "[Nix]",
            "[Control center]",
            "Alpha",
            "[Utilities]",
            "[Zed]",
            "[Captures]",
            "Beta",
            "[Diagnostics]",
            "[System]",
            "[Session]",
        ]
    );
}

#[test]
fn a_root_built_in_not_shown_is_skipped_and_one_listed_twice_once() {
    let (cat, _) = build("[tools]\nroot = [\"dnd\", \"notes\", \"notes\"]\n", &Context::default());
    assert_eq!(names(&cat, &cat.root), ["Notes"], "dnd needs notifications; notes only once");
}

#[test]
fn a_user_category_restyles_a_builtin_one() {
    let (cat, _) = build("[[tools.category]]\nname = \"Session\"\nicon = \"X\"\norder = 5\n", &all());
    let s = cat.categories.iter().find(|c| c.name == "Session").unwrap();
    assert_eq!((s.icon, s.order), ('X', 5));
    assert_eq!(cat.categories.iter().filter(|c| c.name == "Session").count(), 1, "restyled, not added");
    let first = cat.root_rows()[0];
    assert!(matches!(first, Item::Category(c) if cat.categories[c].name == "Session"), "order 5 now sorts first");
    // The others keep their glyph and order.
    let q = cat.categories.iter().find(|c| c.name == "Quick settings").unwrap();
    assert_eq!(q.order, 10);
}

#[test]
fn user_entries_join_a_builtin_category_after_its_builtins() {
    let text = "
[[tools.entry]]
name = \"Zzz first in file\"
category = \"Session\"
run = [\"x\"]
[[tools.entry]]
name = \"Aaa second in file\"
category = \"Session\"
run = [\"x\"]
";
    let (cat, _) = build(text, &all());
    let session = cat.categories.iter().position(|c| c.name == "Session").unwrap();
    assert_eq!(
        names(&cat, &cat.category_rows(session)),
        ["Lock", "Power menu", "Reload the bar", "Zzz first in file", "Aaa second in file"],
        "its built-ins in the table's order, then the user entries in the file's order"
    );
}

#[test]
fn an_empty_category_is_not_listed() {
    let (cat, _) = build(
        "[tools]\nbuiltins = false\n[[tools.category]]\nname = \"Empty\"\n[[tools.entry]]\nname = \"E\"\ncategory = \"Utilities\"\nrun = [\"x\"]\n",
        &all(),
    );
    assert_eq!(category_names(&cat, &cat.root_rows()), ["[Utilities]"], "only the category with an entry");
    // A context that hides every Session row empties it.
    let c = Context { lock: false, session: false, reload: false, ..all() };
    let (cat, _) = build("", &c);
    assert!(!category_names(&cat, &cat.root_rows()).contains(&"[Session]".to_string()));
}

/// An entry of a category cut by the 16-category
/// bound is dropped, never moved to the root (the config counts it in its note).
#[test]
fn an_entry_naming_a_cut_category_is_dropped() {
    let mut text = String::new();
    for i in 0..17 {
        text.push_str(&format!("[[tools.category]]\nname = \"c{i}\"\n"));
    }
    text.push_str("[[tools.entry]]\nname = \"Lost\"\ncategory = \"c16\"\nrun = [\"x\"]\n");
    let (cat, _) = build(&text, &all());
    assert!(!cat.entries.iter().any(|e| e.name == "Lost"), "not at the root, not anywhere");
}

#[test]
fn flat_lists_entries_then_fixed_choices() {
    let text = "
[[tools.entry]]
name = \"Plain\"
run = [\"x\"]
[[tools.entry]]
name = \"Pick\"
run = [\"x\", \"{choice}\"]
choices = [\"a\", \"b\"]
[[tools.entry]]
name = \"Dyn\"
run = [\"x\", \"{choice}\"]
choices_cmd = [\"ls\"]
";
    let (cat, _) = build(text, &Context::default());
    let flat = cat.flat();
    let n = cat.entries.len();
    let pick = cat.entries.iter().position(|e| e.name == "Pick").unwrap();
    let dynamic = cat.entries.iter().position(|e| e.name == "Dyn").unwrap();
    assert_eq!(flat.len(), n + 2);
    assert_eq!(flat[..n].iter().map(|f| f.entry).collect::<Vec<_>>(), (0..n).collect::<Vec<_>>());
    assert!(flat[..n].iter().all(|f| f.choice.is_none()), "every entry first");
    assert_eq!(flat[n..], [Flat { entry: pick, choice: Some(0) }, Flat { entry: pick, choice: Some(1) }]);
    assert!(flat.iter().all(|f| f.choice.is_none() || f.entry != dynamic), "dynamic choices are never listed");
    assert!(matches!(cat.entries[dynamic].choices, Choices::Command));
    assert!(matches!(cat.entries[pick].choices, Choices::Fixed(ref c) if c == &["a", "b"]));
    assert!(matches!(cat.entries[0].choices, Choices::None));
}

#[test]
fn entries_carry_their_flags() {
    let text = "
[[tools.entry]]
name = \"Danger\"
icon = \"X\"
confirm = true
state = [\"s\"]
run = [\"x\"]
";
    let (cat, _) = build(text, &all());
    let e = cat.entries.last().unwrap();
    assert_eq!((e.name.as_str(), e.icon, e.confirm, e.has_state), ("Danger", Some('X'), true, true));
    assert_eq!(e.kind, EntryKind::User(0));
    let dnd = cat.entries.iter().find(|e| e.name == "Do not disturb").unwrap();
    assert!(dnd.has_state && !dnd.confirm);
    let notes = cat.entries.iter().find(|e| e.name == "Notes").unwrap();
    assert!(!notes.has_state);
}

#[test]
fn history_keys() {
    let (cat, _) = build("", &all());
    let key = |name: &str| cat.entries.iter().find(|e| e.name == name).unwrap().key.clone();
    assert_eq!(key("Notes"), "tool:builtin:notes");
    assert_eq!(key("Wi-Fi"), "tool:builtin:quick.wifi");
    let long_name = "😀".repeat(48);
    let long_category = "😀".repeat(48);
    let long_choice = "😀".repeat(64);
    let text = format!(
        "[[tools.category]]\nname = {long_category:?}\n\
         [[tools.entry]]\nname = \"Update\"\nrun = [\"x\"]\n\
         [[tools.entry]]\nname = \"Nix\"\ncategory = {long_category:?}\nrun = [\"x\", \"{{choice}}\"]\nchoices = [{long_choice:?}]\n\
         [[tools.entry]]\nname = {long_name:?}\ncategory = {long_category:?}\nrun = [\"x\"]\n\
         [[tools.entry]]\nname = \"Sub\"\ncategory = \"Utilities\"\nrun = [\"x\"]\n"
    );
    let (cat, _) = build(&text, &all());
    let user = |name: &str| cat.entries.iter().find(|e| e.name == name).unwrap();
    assert_eq!(user("Update").key, "tool:user:/Update", "an empty category for a root entry");
    assert_eq!(user("Sub").key, "tool:user:Utilities/Sub");
    assert_eq!(
        cat.choice_key(cat.entries.iter().position(|e| e.name == "Nix").unwrap(), &long_choice),
        format!("tool:user:{long_category}/Nix/{long_choice}")
    );
    for e in &cat.entries {
        assert!(e.key.len() <= crate::launcher::history::ID_MAX, "{}", e.key.len());
    }
    let nix = cat.entries.iter().position(|e| e.name == "Nix").unwrap();
    assert!(cat.choice_key(nix, &long_choice).len() <= crate::launcher::history::ID_MAX);
    assert!(cat.entries.iter().any(|e| e.name == long_name && e.key.len() <= crate::launcher::history::ID_MAX));
}

#[test]
fn nmap_missing_hides_the_scan_entry() {
    let ids = |c: &Context| ids_shown(c);
    assert!(ids(&all()).contains(&"diag.scan"));
    let none = Context { nmap: false, ..all() };
    assert!(!ids(&none).contains(&"diag.scan"), "no nmap on the $PATH: no scan entry");
}

#[test]
fn an_empty_usb_reset_hides_it() {
    let shown = |c: &Context| ids_shown(c).contains(&"diag.usb_reset");
    assert!(shown(&all()));
    assert!(!shown(&Context { usb_reset: false, ..all() }), "[tools.builtin] usb_reset = [] hides it");
}

#[test]
fn every_preset_has_a_builtin_and_a_slug_that_round_trips() {
    use alpheus_common::diagnostics::query::Preset;
    for p in Preset::ALL {
        assert_eq!(Preset::from_slug(p.slug()), Some(p));
        let id = format!("diag.preset.{}", p.slug());
        let b = BUILTINS.iter().find(|b| b.id == id).unwrap_or_else(|| panic!("a built-in for {id}"));
        assert_eq!(b.action, BuiltinAction::Preset(p));
        assert_eq!(b.label, p.label(), "the built-in is named as the Diag tab's chip");
        assert_eq!(b.category, DIAGNOSTICS);
    }
    assert_eq!(BUILTINS.iter().filter(|b| matches!(b.action, BuiltinAction::Preset(_))).count(), Preset::ALL.len());
}

#[test]
fn the_scan_entry_lists_the_ten_scans_and_asks_except_for_the_sweep() {
    use crate::launcher::tools::diag::{PORTS_PROMPT, Scan, TARGET_PROMPT};
    let (cat, _) = build("", &all());
    let scan = cat.entries.iter().find(|e| e.name == "Network scan").unwrap();
    let labels: Vec<String> = Scan::ALL.iter().map(|s| s.label().to_string()).collect();
    assert_eq!(scan.choices, Choices::Fixed(labels));
    assert_eq!(scan.category.map(|c| cat.categories[c].name.as_str()), Some("Diagnostics"));
    assert_eq!(scan.ask_for(Some(Scan::Quick.label())), Some(TARGET_PROMPT));
    assert_eq!(scan.ask_for(Some(Scan::Udp.label())), Some(TARGET_PROMPT));
    assert_eq!(scan.ask_for(Some(Scan::Ports.label())), Some(PORTS_PROMPT));
    assert_eq!(scan.ask_for(Some(Scan::HostDiscovery.label())), None, "the sweep runs without a prompt");
    let reset = cat.entries.iter().find(|e| e.name == "Reset USB (xHCI)").unwrap();
    assert!(reset.confirm, "the reset asks for a second Enter");
    let events = cat.entries.iter().find(|e| e.name == "USB events").unwrap();
    assert!(!events.confirm && events.ask_for(None).is_none());
    assert_eq!(scan.key, "tool:builtin:diag.scan");
    assert_eq!(
        cat.choice_key(cat.entries.iter().position(|e| e.name == "Network scan").unwrap(), Scan::Full.label()),
        "tool:builtin:diag.scan/Full scan (all ports)"
    );
}

/// The shutdown built-in's choices: the five durations, and a first one that cancels (with the time
/// left) only while a shutdown is pending.
#[test]
fn the_shutdown_choices_follow_the_pending_state() {
    let choices = |left: Option<u64>| {
        let (cat, _) = build("", &Context { shutdown_left: left, ..all() });
        let e = cat.entries.iter().find(|e| e.name == "Schedule shutdown").expect("the shutdown entry");
        (e.choices.clone(), e.key.clone())
    };
    let (none, key) = choices(None);
    assert_eq!(none, Choices::Fixed(["30 min", "1 h", "1 h 30", "2 h", "3 h"].map(String::from).to_vec()));
    assert_eq!(key, "tool:builtin:shutdown");
    let (pending, _) = choices(Some(80 * 60_000));
    let Choices::Fixed(list) = pending else { panic!("fixed choices") };
    assert_eq!(list[0], "Cancel shutdown (in 1h20)");
    assert_eq!(list.len(), 6);
    // It is in the System category, and its choices are in the flat search.
    let (cat, _) = build("", &all());
    let at = cat.entries.iter().position(|e| e.name == "Schedule shutdown").unwrap();
    assert_eq!(cat.categories[cat.entries[at].category.unwrap()].name, "System");
    assert_eq!(cat.flat().iter().filter(|f| f.entry == at && f.choice.is_some()).count(), 5);
}

/// A compositor without screencopy hides the Captures entries, and with them their category.
#[test]
fn no_screencopy_hides_the_captures() {
    let c = Context { captures: false, ..all() };
    assert!(ids_shown(&c).iter().all(|id| !id.starts_with("capture.")), "{:?}", ids_shown(&c));
    let (cat, _) = build("", &c);
    assert!(!category_names(&cat, &cat.root_rows()).contains(&"[Captures]".to_string()), "no empty category");
    let (cat, _) = build("", &all());
    let at = cat.categories.iter().position(|k| k.name == "Captures").unwrap();
    assert_eq!(names(&cat, &cat.category_rows(at)), ["Screenshot", "Area", "Area (save)"]);
    // The category sits between Utilities and Session.
    let root = category_names(&cat, &cat.root_rows());
    let place = |n: &str| root.iter().position(|r| r == n).unwrap();
    assert!(place("[Utilities]") < place("[Captures]") && place("[Captures]") < place("[Session]"), "{root:?}");
}

/// The entries whose verb is not built yet stay in the table (ids, labels, order) and are hidden,
/// whatever the context says, until their tasks land: no row that answers "not available yet".
#[test]
fn the_unbuilt_captures_are_hidden_until_their_tasks_land() {
    let shown = ids_shown(&all());
    for id in ["capture.annotate", "capture.record", "capture.record_area", "capture.stop", "capture.pick", "capture.ocr"] {
        assert!(BUILTINS.iter().any(|b| b.id == id), "{id} stays in the table");
        assert!(!shown.contains(&id), "{id} is hidden");
    }
    for id in ["capture.full", "capture.area", "capture.area_save"] {
        assert!(shown.contains(&id), "{id} shows");
    }
}

/// The capture rows each have a glyph of their own, the two recordings included.
#[test]
fn every_capture_row_has_its_own_glyph() {
    let glyphs: Vec<char> = BUILTINS.iter().filter(|b| b.id.starts_with("capture.")).map(|b| b.glyph).collect();
    let mut unique = glyphs.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), glyphs.len(), "{glyphs:?}");
}
