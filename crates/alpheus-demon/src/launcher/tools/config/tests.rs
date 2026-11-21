use super::*;
use crate::launcher::config::LauncherConfig;

fn parse(text: &str) -> Result<LauncherConfig, String> {
    LauncherConfig::from_text(text)
}

/// A `[[tools.entry]]` named "Zeta" running `x`, plus `body` (more keys of the same table).
fn entry(body: &str) -> String {
    format!("[[tools.entry]]\nname = \"Zeta\"\nrun = [\"x\"]\n{body}")
}

fn v(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| x.to_string()).collect()
}

#[test]
fn defaults_are_builtins_on_and_nothing_else() {
    let d = ToolsConfig::default();
    assert!(d.builtins);
    assert!(d.hide.is_empty() && d.root.is_empty() && d.reload.is_empty());
    assert!(d.categories.is_empty() && d.entries.is_empty());
    let c = parse("").unwrap();
    assert_eq!(c.tools, d);
    assert!(c.notes.is_empty());
    assert_eq!(parse("[tools]\n").unwrap().tools, d, "an empty [tools] is the default");
    assert_eq!(parse("[tools]\n").unwrap(), parse("").unwrap());
}

#[test]
fn a_full_entry_parses() {
    let text = r#"
[tools]
builtins = false
hide = ["tab.power", "nope"]
root = ["keep_awake"]
reload = ["shell-ctl", "reload-bar"]

[[tools.category]]
name = "Nix"
icon = "󰏖"
order = 40

[[tools.entry]]
name = "Update"
category = "Nix"
icon = "󰚰"
order = 7
run = ["atya", "update"]
terminal = false
hold = true
confirm = true

[[tools.entry]]
name = "Profile"
category = "Nix"
run = ["pp", "{choice}"]
choices = ["a b", "c"]

[[tools.entry]]
name = "Kill"
run = ["kill-it", "{choice}"]
choices_cmd = ["ps-list"]

[[tools.entry]]
name = "Game"
run = ["game"]
terminal = true
state = ["is-game"]
"#;
    let t = parse(text).unwrap().tools;
    assert!(!t.builtins);
    assert_eq!((t.hide, t.root, t.reload), (v(&["tab.power", "nope"]), v(&["keep_awake"]), v(&["shell-ctl", "reload-bar"])));
    assert_eq!(t.categories, [CategoryConfig { name: "Nix".into(), icon: Some("󰏖".into()), order: Some(40) }]);
    let e = &t.entries;
    assert_eq!(e.len(), 4);
    assert_eq!(
        e[0],
        EntryConfig {
            name: "Update".into(),
            category: Some("Nix".into()),
            icon: Some("󰚰".into()),
            order: Some(7),
            run: v(&["atya", "update"]),
            hold: true,
            confirm: true,
            position: 1,
            ..EntryConfig::default()
        }
    );
    assert_eq!((e[1].choices.clone(), e[1].run.clone()), (v(&["a b", "c"]), v(&["pp", "{choice}"])));
    assert_eq!(e[2].choices_cmd, v(&["ps-list"]));
    assert!(e[3].terminal && e[3].category.is_none());
    assert_eq!(e[3].state, v(&["is-game"]));
}

#[test]
fn positions_are_one_based_in_file_order() {
    let text: String = (0..5).map(|i| format!("[[tools.entry]]\nname = \"e{i}\"\nrun = [\"x\"]\n")).collect();
    let t = parse(&text).unwrap().tools;
    assert_eq!(t.entries.iter().map(|e| e.position).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
}

#[test]
fn invalid_rules_make_the_file_invalid() {
    let cat = |name: &str| format!("[[tools.category]]\nname = {name:?}\n");
    let cases: Vec<(String, &str, &str)> = vec![
        // (text, the position named in the message, the rule's words)
        ("[[tools.entry]]\nname = \"Zeta\"\nrun = []\n".into(), "tools.entry 1", "run empty"),
        ("[[tools.entry]]\nname = \"Zeta\"\nrun = [\" \"]\n".into(), "tools.entry 1", "run empty"),
        ("[[tools.entry]]\nname = \"\"\nrun = [\"x\"]\n".into(), "tools.entry 1", "name must be 1 to 48 characters"),
        (
            format!("[[tools.entry]]\nname = \"Zeta{}\"\nrun = [\"x\"]\n", "z".repeat(45)),
            "tools.entry 1",
            "name must be 1 to 48 characters",
        ),
        ("[[tools.entry]]\nname = \"  \"\nrun = [\"x\"]\n".into(), "tools.entry 1", "name must not be blank"),
        (format!("{}{}", entry(""), entry("")), "tools.entry 2", "used twice"),
        ("[[tools.entry]]\nname = \"Zeta\\t\"\nrun = [\"x\"]\n".into(), "tools.entry 1", "control character or U+FFFD"),
        ("[[tools.entry]]\nname = \"Zeta\\uFFFD\"\nrun = [\"x\"]\n".into(), "tools.entry 1", "control character or U+FFFD"),
        (entry("category = \"Nowhere\"\n"), "tools.entry 1", "category names no category"),
        (entry("terminal = true\nhold = true\n"), "tools.entry 1", "terminal and hold"),
        (
            entry("choices = [\"a\"]\nchoices_cmd = [\"l\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"),
            "tools.entry 1",
            "choices and choices_cmd",
        ),
        (
            entry("state = [\"s\"]\nchoices = [\"a\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"),
            "tools.entry 1",
            "state with choices",
        ),
        (
            entry("state = [\"s\"]\nchoices_cmd = [\"l\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"),
            "tools.entry 1",
            "state with choices",
        ),
        (entry("choices = [\"a\"]\n"), "tools.entry 1", "exactly once, as a whole argument"),
        (entry("choices_cmd = [\"l\"]\n"), "tools.entry 1", "exactly once, as a whole argument"),
        (
            entry("choices = [\"a\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\", \"{choice}\"]"),
            "tools.entry 1",
            "exactly once, as a whole argument",
        ),
        (
            entry("choices = [\"a\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"x{choice}\"]"),
            "tools.entry 1",
            "exactly once, as a whole argument",
        ),
        (
            entry("choices = [\"a\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\", \"--n={choice}\"]"),
            "tools.entry 1",
            "exactly once, as a whole argument",
        ),
        (entry("choices = [\"a\"]\n").replace("run = [\"x\"]", "run = [\"{choice}\"]"), "tools.entry 1", "run's program"),
        (entry("").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"), "tools.entry 1", "no choices"),
        (entry("").replace("run = [\"x\"]", "run = [\"x\", \"a{choice}\"]"), "tools.entry 1", "no choices"),
        (entry("icon = \"ab\"\n"), "tools.entry 1", "icon must be one character"),
        (entry("icon = \"\"\n"), "tools.entry 1", "icon must be one character"),
        (entry("order = 1001\n"), "tools.entry 1", "order 1001 past 1000"),
        (
            entry("choices = [\"\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"),
            "tools.entry 1",
            "choice 1 must be 1 to 64 characters",
        ),
        (
            entry(&format!("choices = [\"a\", \"{}\"]\n", "c".repeat(65))).replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"),
            "tools.entry 1",
            "choice 2 must be 1 to 64 characters",
        ),
        (
            entry("choices = [\"a\\u0007\"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"),
            "tools.entry 1",
            "control character or U+FFFD",
        ),
        (entry("state = [\" \"]\n"), "tools.entry 1", "state empty"),
        (entry("choices_cmd = [\" \"]\n").replace("run = [\"x\"]", "run = [\"x\", \"{choice}\"]"), "tools.entry 1", "choices_cmd empty"),
        (cat(""), "tools.category 1", "name must be 1 to 48 characters"),
        (cat("Zeta\t"), "tools.category 1", "control character or U+FFFD"),
        (format!("{}{}", cat("Zeta"), cat("Zeta")), "tools.category 2", "used twice"),
        ("[[tools.category]]\nname = \"Zeta\"\nicon = \"ab\"\n".into(), "tools.category 1", "icon must be one character"),
        ("[[tools.category]]\nname = \"Zeta\"\norder = 1001\n".into(), "tools.category 1", "order 1001 past 1000"),
        ("[tools]\nreload = [\" \"]\n".into(), "tools.reload", "empty program"),
    ];
    for (text, at, rule) in cases {
        let e = parse(&text).unwrap_err();
        assert!(e.contains(at) && e.contains(rule), "{text:?}: {e}");
        assert!(!e.contains("Zeta"), "the message must not name the entry or category: {e}");
    }
    for (text, needle) in [
        ("[tools]\nbuiltin = true\n", "unknown field"),
        ("[[tools.entry]]\nname = \"Zeta\"\nrun = [\"x\"]\nicons = \"a\"\n", "unknown field"),
        ("[[tools.category]]\nname = \"Zeta\"\nrun = [\"x\"]\n", "unknown field"),
    ] {
        let e = parse(text).unwrap_err();
        assert!(e.contains(needle), "{text:?}: {e}");
    }
}

#[test]
fn an_entry_may_name_a_builtin_category() {
    let c = parse(&entry("category = \"Session\"\n")).unwrap();
    assert_eq!(c.tools.entries[0].category.as_deref(), Some("Session"));
    for name in BUILTIN_CATEGORIES {
        assert!(parse(&entry(&format!("category = {name:?}\n"))).is_ok(), "{name}");
    }
    // A user category of the same name restyles it (spec § 3): valid too.
    assert!(parse("[[tools.category]]\nname = \"Session\"\nicon = \"x\"\n").is_ok());
}

#[test]
fn the_same_name_in_two_categories_is_fine() {
    let text = format!("{}{}", entry("category = \"Session\"\n"), entry("category = \"Utilities\"\n"));
    assert!(parse(&text).is_ok());
    assert!(parse(&format!("{}{}", entry("category = \"Session\"\n"), entry(""))).is_ok(), "root and a category");
}

#[test]
fn the_limits_themselves_are_accepted() {
    let long = "é".repeat(NAME_MAX); // 48 characters, 96 bytes: characters, not bytes
    let text = format!("[[tools.entry]]\nname = {long:?}\nrun = [\"x\", \"{{choice}}\"]\nchoices = [{:?}]\n", "é".repeat(CHOICE_MAX));
    assert!(parse(&text).is_ok());
    assert!(parse(&entry("order = 1000\n")).is_ok());
    assert!(parse(&entry("order = 0\nicon = \"󰚰\"\n")).is_ok());
}

#[test]
fn bounds_cut_and_note_once() {
    let mut text = String::new();
    for i in 0..17 {
        text.push_str(&format!("[[tools.category]]\nname = \"c{i}\"\n"));
    }
    for i in 0..130 {
        text.push_str(&format!("[[tools.entry]]\nname = \"e{i}\"\n"));
        match i {
            // 40 choices: entry 1's alone.
            0 => {
                let many: Vec<String> = (0..40).map(|k| format!("{:?}", format!("ch{k}"))).collect();
                text.push_str(&format!("run = [\"x\", \"{{choice}}\"]\nchoices = [{}]\n", many.join(", ")));
            }
            // 18 entries with a state: positions 11 to 28.
            10..=27 => text.push_str("run = [\"x\"]\nstate = [\"s\"]\n"),
            _ => text.push_str("run = [\"x\"]\n"),
        }
    }
    let c = parse(&text).unwrap();
    assert_eq!(
        c.notes,
        [
            "tools: 17 categories in launcher.toml, the first 16 kept",
            "tools: 130 entries in launcher.toml, the first 128 kept",
            "tools: entry 1: 40 choices, the first 32 kept",
            "tools: 18 entries with a state, the first 16 read",
        ]
    );
    let t = c.tools;
    assert_eq!((t.categories.len(), t.entries.len(), t.entries[0].choices.len()), (16, 128, 32));
    assert_eq!(t.entries[0].choices[31], "ch31", "the first ones are kept");
    assert_eq!(t.categories[15].name, "c15");
    assert_eq!(t.entries[127].name, "e127");
    let with_state: Vec<usize> = t.entries.iter().filter(|e| !e.state.is_empty()).map(|e| e.position).collect();
    assert_eq!(with_state, (11..=26).collect::<Vec<_>>(), "the first 16 by file order keep their state");
    assert!(t.entries[26].state.is_empty() && t.entries[27].state.is_empty(), "past the 16th the entry shows with no state");
    // The notes are counts only: no name.
    assert!(c.notes.iter().all(|n| !n.contains("ch3") && !n.contains("e1") && !n.contains("c1")), "{:?}", c.notes);
}

#[test]
fn at_the_bounds_nothing_is_noted() {
    let mut text = String::new();
    for i in 0..16 {
        text.push_str(&format!("[[tools.category]]\nname = \"c{i}\"\n"));
    }
    for i in 0..128 {
        text.push_str(&format!("[[tools.entry]]\nname = \"e{i}\"\nrun = [\"x\"]\n"));
        if i < 16 {
            text.push_str("state = [\"s\"]\n");
        }
    }
    let c = parse(&text).unwrap();
    assert!(c.notes.is_empty(), "{:?}", c.notes);
    assert_eq!((c.tools.categories.len(), c.tools.entries.len()), (16, 128));
}

#[test]
fn a_cut_entry_leaves_the_later_positions_alone() {
    let entries = (1..=3)
        .map(|i| EntryConfig {
            name: format!("e{i}"),
            run: v(&["x", "{choice}"]),
            choices: vec!["a".into(); 40],
            position: i,
            ..EntryConfig::default()
        })
        .collect();
    let cfg = ToolsConfig { entries, ..ToolsConfig::default() };
    let mut notes = Vec::new();
    let out = bounded(cfg, &mut |n| notes.push(n.to_string()));
    assert_eq!(
        notes,
        [
            "tools: entry 1: 40 choices, the first 32 kept",
            "tools: entry 2: 40 choices, the first 32 kept",
            "tools: entry 3: 40 choices, the first 32 kept"
        ]
    );
    assert!(out.entries.iter().all(|e| e.choices.len() == 32));
}

#[test]
fn unknown_hide_and_root_ids_are_valid() {
    let c = parse("[tools]\nhide = [\"quick.nothing\", \"\", \"a b\"]\nroot = [\"from.the.future\"]\n").unwrap();
    assert_eq!(c.tools.hide.len(), 3);
    assert_eq!(c.tools.root, ["from.the.future"]);
    assert!(c.notes.is_empty(), "the catalog notes unknown ids, not the parser");
}

#[test]
fn the_three_prefixes() {
    let d = LauncherConfig::default();
    assert_eq!((d.prefixes.commands.as_str(), d.prefixes.windows.as_str(), d.prefixes.tools.as_str()), (">", "w ", "/"));
    assert_eq!(parse("").unwrap().prefixes.tools, "/");
    assert_eq!(parse("[prefixes]\ntools = \"\"\n").unwrap().prefixes.tools, "", "off");
    // commands and windows are still refused empty.
    for text in ["[prefixes]\ncommands = \"\"", "[prefixes]\nwindows = \"\""] {
        assert!(parse(text).is_err(), "{text}");
    }
    for (text, needle) in [
        ("[prefixes]\ntools = \">\"\ncommands = \">\"", "both"),
        ("[prefixes]\ntools = \">\"", "both"), // the default commands prefix
        ("[prefixes]\ntools = \"w\"\nwindows = \"w \"", "both"),
        ("[prefixes]\ntools = \"w \"", "both"),
        ("[prefixes]\ntools = \">>\"", "both"),
        ("[prefixes]\ntools = \" /\"", "prefixes.tools starts with a space"),
    ] {
        let e = parse(text).unwrap_err();
        assert!(e.contains(needle), "{text:?}: {e}");
    }
    // An empty one is exempt from the pair rules: `commands = "/"` is then fine.
    assert!(parse("[prefixes]\ntools = \"\"\ncommands = \"/\"").is_ok());
    assert!(parse("[prefixes]\ntools = \"t \"\ncommands = \"!\"\nwindows = \"w \"").is_ok());
}
