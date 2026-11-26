use super::*;
use crate::launcher::config::LauncherConfig;
use crate::launcher::tools::catalog::Context;
use crate::launcher::tools::diag::TARGET_PROMPT;

const CFG: &str = r#"
[tools]
builtins = false
[[tools.category]]
name = "Nix"
[[tools.category]]
name = "Empty"
[[tools.category]]
name = "Zed"
[[tools.entry]]
name = "Update"
category = "Nix"
run = ["x"]
[[tools.entry]]
name = "Collect"
category = "Nix"
run = ["x"]
[[tools.entry]]
name = "Home thing"
run = ["x"]
[[tools.entry]]
name = "Pick"
category = "Zed"
run = ["x"]
[[tools.entry]]
name = "Nixify"
category = "Zed"
run = ["x"]
"#;

/// Every built-in shown but "Stop recording" and the lock, which `recent_skips_…` relies on.
fn context() -> Context {
    Context {
        quick: true,
        notifications: true,
        timer: true,
        keep_awake: true,
        night_light: true,
        lock: false,
        session: true,
        clipboard: true,
        diagnostics: true,
        nmap: true,
        usb_reset: true,
        shutdown: true,
        captures: true,
        ..Context::default()
    }
}

fn catalog(text: &str) -> Catalog {
    let config = LauncherConfig::from_text(text).unwrap();
    Catalog::build(&config.tools, &context(), &mut |_| {})
}

fn entry(cat: &Catalog, name: &str) -> usize {
    cat.entries.iter().position(|e| e.name == name).unwrap_or_else(|| panic!("{name}"))
}

/// The panel's rows as text: a section label in brackets, a tool by its name.
fn shown(cat: &Catalog, panel: &Panel) -> Vec<String> {
    panel
        .rows
        .iter()
        .map(|r| match *r {
            PanelRow::Section(Label::Pinned) => "[Pinned]".to_string(),
            PanelRow::Section(Label::Recent) => "[Recent]".to_string(),
            PanelRow::Section(Label::Category(k)) => format!("[{}]", cat.categories[k].name),
            PanelRow::Tool(e) => cat.entries[e].name.clone(),
        })
        .collect()
}

fn names(cat: &Catalog, entries: &[usize]) -> Vec<String> {
    entries.iter().map(|&e| cat.entries[e].name.clone()).collect()
}

fn keys(list: &[&str]) -> Vec<String> {
    list.iter().map(|k| (*k).to_string()).collect()
}

#[test]
fn all_lists_pinned_recent_then_categories() {
    let cat = catalog(CFG);
    let panel = Panel::build(&cat, &[entry(&cat, "Nixify")], 0);
    assert_eq!(panel.active, 0);
    assert_eq!(
        shown(&cat, &panel),
        ["[Pinned]", "Home thing", "[Recent]", "Nixify", "[Nix]", "Update", "Collect", "[Zed]", "Pick", "Nixify"]
    );
    let nix = cat.categories.iter().position(|k| k.name == "Nix").unwrap();
    let zed = cat.categories.iter().position(|k| k.name == "Zed").unwrap();
    assert_eq!(panel.tabs, [Tab::All, Tab::Category(nix), Tab::Category(zed)]);
    assert_eq!(names(&cat, &panel.tools()), ["Home thing", "Nixify", "Update", "Collect", "Pick", "Nixify"]);
}

/// The built-in catalog: Pinned holds `[tools] root`, the categories follow their order.
#[test]
fn all_follows_the_category_order_with_the_built_ins() {
    let cat = catalog("[tools]\nroot = [\"notes\"]\n");
    let panel = Panel::build(&cat, &[], 0);
    let sections: Vec<String> = shown(&cat, &panel).into_iter().filter(|s| s.starts_with('[')).collect();
    assert_eq!(
        sections,
        ["[Pinned]", "[Quick settings]", "[Control center]", "[Utilities]", "[Captures]", "[Diagnostics]", "[System]", "[Session]"]
    );
    assert_eq!(shown(&cat, &panel)[1], "Notes");
}

#[test]
fn empty_sections_are_not_shown() {
    let text = CFG.replace("[[tools.entry]]\nname = \"Home thing\"\nrun = [\"x\"]\n", "");
    let cat = catalog(&text);
    let panel = Panel::build(&cat, &[], 0);
    assert_eq!(shown(&cat, &panel), ["[Nix]", "Update", "Collect", "[Zed]", "Pick", "Nixify"], "no Pinned, no Recent, no Empty");
    assert_eq!(panel.tabs.len(), 3, "Empty has no tab");
}

#[test]
fn recent_skips_tools_that_do_not_show() {
    let cat = catalog("");
    let history = keys(&["tool:builtin:lock", "tool:user:/Gone", "tool:builtin:notes", "app:firefox.desktop"]);
    assert_eq!(names(&cat, &recent_entries(&cat, &history, 4)), ["Notes"], "the lock is hidden, Gone is no entry");
}

#[test]
fn recent_is_deduplicated_and_skips_pinned() {
    let cat = catalog("[tools]\nroot = [\"notes\"]\n");
    let history = keys(&[
        "tool:builtin:timer",
        "tool:builtin:notes",
        "tool:builtin:timer",
        "tool:builtin:shutdown/1 h",
        "tool:builtin:shutdown",
        "tool:builtin:emoji",
    ]);
    assert_eq!(
        names(&cat, &recent_entries(&cat, &history, 4)),
        ["Timer", "Schedule shutdown", "Emoji"],
        "Notes is pinned; a choice counts as its tool, once"
    );
}

#[test]
fn recent_holds_at_most_four() {
    let cat = catalog("");
    let history = keys(&[
        "tool:builtin:notes",
        "tool:builtin:timer",
        "tool:builtin:emoji",
        "tool:builtin:clipboard",
        "tool:builtin:dnd",
        "tool:builtin:power",
    ]);
    assert_eq!(names(&cat, &recent_entries(&cat, &history, 4)), ["Notes", "Timer", "Emoji", "Clipboard"]);
}

#[test]
fn a_category_tab_lists_its_tools_without_label() {
    let cat = catalog(CFG);
    let zed = cat.categories.iter().position(|k| k.name == "Zed").unwrap();
    let at = Panel::build(&cat, &[], 0).tabs.iter().position(|t| *t == Tab::Category(zed)).unwrap();
    let panel = Panel::build(&cat, &[entry(&cat, "Home thing")], at);
    assert_eq!(panel.active, at);
    assert_eq!(shown(&cat, &panel), ["Pick", "Nixify"]);
    assert_eq!(names(&cat, &panel.tools()), ["Pick", "Nixify"]);
    // A tab past the last is the last.
    assert_eq!(Panel::build(&cat, &[], 99).active, 2);
}

#[test]
fn actions_are_derived_from_the_entry() {
    let text = "[[tools.entry]]\nname = \"Say\"\nrun = [\"x\", \"{text}\"]\nask = \"Words\"\n\
                [[tools.entry]]\nname = \"Plain\"\nrun = [\"x\"]\nkey = \"g\"\n";
    let cat = catalog(text);
    let of = |name: &str| actions(&cat.entries[entry(&cat, name)]);
    assert_eq!(of("Do not disturb"), [Action::Toggle, Action::QuickKey('d')]);
    assert_eq!(of("Reset USB (xHCI)"), [Action::Confirm, Action::QuickKey('x')]);
    assert_eq!(of("Schedule shutdown"), [Action::Choose, Action::QuickKey('s')]);
    assert_eq!(of("Say"), [Action::Type("Words".into())]);
    assert_eq!(of("Network scan"), [Action::Choose, Action::Type(TARGET_PROMPT.into())], "a choice, then the target");
    assert_eq!(of("Wi-Fi"), [Action::Open, Action::QuickKey('w')], "it opens the quick panel");
    assert_eq!(of("Screenshot"), [Action::Run, Action::QuickKey('f')]);
    assert_eq!(of("Plain"), [Action::Run, Action::QuickKey('g')]);
}
