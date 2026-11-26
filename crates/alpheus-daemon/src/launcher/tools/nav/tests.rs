use super::*;
use crate::keys::Key;
use crate::launcher::config::LauncherConfig;
use crate::launcher::history::History;
use crate::launcher::matching::{Mode, Source};
use crate::launcher::model::{Activation, Launcher, Outcome};
use crate::launcher::tools::catalog::{Catalog, Context};
use std::time::{Duration, Instant};

const MS: Duration = Duration::from_millis(1);

const CFG: &str = r#"
[tools]
builtins = false
[[tools.category]]
name = "Nix"
[[tools.category]]
name = "Zed"
[[tools.entry]]
name = "Update"
category = "Nix"
run = ["x", "update"]
[[tools.entry]]
name = "Collect"
category = "Nix"
run = ["x", "gc"]
confirm = true
[[tools.entry]]
name = "Generation"
category = "Nix"
run = ["x", "{choice}"]
choices = ["list", "diff"]
[[tools.entry]]
name = "Pick"
category = "Zed"
run = ["x", "{choice}"]
choices_cmd = ["lines"]
[[tools.entry]]
name = "Home thing"
run = ["x", "home"]
state = ["s"]
[[tools.entry]]
name = "Nixify"
category = "Zed"
run = ["x", "nixify"]
"#;

fn context() -> Context {
    Context { quick: true, notifications: true, timer: true, ..Context::default() }
}

fn catalog(text: &str) -> (LauncherConfig, Catalog) {
    let config = LauncherConfig::from_text(text).unwrap();
    let catalog = Catalog::build(&config.tools, &context(), &mut |_| {});
    (config, catalog)
}

fn tools(text: &str) -> Launcher {
    let (config, catalog) = catalog(text);
    Launcher::for_tools(config, catalog, History::empty(200), 1_800_000_000)
}

/// A Mod+Space launcher whose Tools catalog is set (the `/` prefix works).
fn apps(text: &str) -> Launcher {
    let (config, catalog) = catalog(text);
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), 1_800_000_000);
    l.set_tools(catalog);
    l
}

fn typed(l: &mut Launcher, text: &str) {
    for c in text.chars() {
        l.key(Key::Char(c));
    }
}

/// The texts of the rows the list shows, in order.
fn texts(l: &Launcher) -> Vec<String> {
    l.results
        .iter()
        .map(|h| match h.source {
            Source::Tool(i) => l.tool_rows()[i].text.clone(),
            other => panic!("not a tool row: {other:?}"),
        })
        .collect()
}

fn selected_text(l: &Launcher) -> String {
    texts(l)[l.selected].clone()
}

/// Selects the row named `name` (keys only: Home, then Down).
fn select(l: &mut Launcher, name: &str) {
    let at = texts(l).iter().position(|t| t == name).unwrap_or_else(|| panic!("{name} in {:?}", texts(l)));
    l.key(Key::Home);
    for _ in 0..at {
        l.key(Key::Down);
    }
    assert_eq!(selected_text(l), name);
}

/// A key at `t`.
fn key_at(l: &mut Launcher, k: Key, t: Instant) -> Outcome {
    l.set_now(t);
    l.key(k)
}

fn breadcrumb(l: &Launcher) -> String {
    l.tools().unwrap().breadcrumb()
}

#[test]
fn enter_and_right_drill_left_and_esc_go_up() {
    let mut l = tools(CFG);
    assert_eq!(texts(&l), ["Home thing", "Nix", "Zed"], "the root: categories and root entries by order, then name");
    assert_eq!(breadcrumb(&l), "Tools");
    select(&mut l, "Nix");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    assert_eq!(texts(&l), ["Update", "Collect", "Generation"]);
    assert_eq!(breadcrumb(&l), "Tools › Nix");
    assert_eq!(l.key(Key::Left), Outcome::Redraw);
    assert_eq!(breadcrumb(&l), "Tools");
    select(&mut l, "Zed");
    assert_eq!(l.key(Key::Right), Outcome::Redraw, "→ drills like Enter");
    assert_eq!(texts(&l), ["Pick", "Nixify"]);
    assert_eq!(l.key(Key::Escape), Outcome::Redraw, "Esc goes up below the root");
    assert_eq!(breadcrumb(&l), "Tools");
    assert_eq!(l.key(Key::Left), Outcome::Nothing, "← at the root does nothing");
    assert_eq!(l.key(Key::Escape), Outcome::Close, "Esc at the root closes");
}

#[test]
fn right_does_nothing_on_a_plain_entry_and_left_with_a_query_nothing() {
    let mut l = tools(CFG);
    select(&mut l, "Nix");
    l.key(Key::Enter);
    select(&mut l, "Update");
    assert_eq!(l.key(Key::Right), Outcome::Nothing);
    typed(&mut l, "up");
    assert_eq!(l.key(Key::Left), Outcome::Nothing, "← with a typed query does nothing");
    assert_eq!(breadcrumb(&l), "Tools › Nix");
}

#[test]
fn esc_at_the_root_closes_even_with_a_query() {
    let mut l = tools(CFG);
    typed(&mut l, "ni");
    assert_eq!(l.key(Key::Escape), Outcome::Close);
}

#[test]
fn esc_below_the_root_drops_the_query_and_goes_up() {
    let mut l = tools(CFG);
    select(&mut l, "Nix");
    l.key(Key::Enter);
    typed(&mut l, "col");
    assert_eq!(l.key(Key::Escape), Outcome::Redraw);
    assert_eq!((breadcrumb(&l), l.query.as_str()), ("Tools".to_string(), ""));
}

#[test]
fn backspace_on_an_empty_root_query_goes_back_to_all_only_from_mod_space() {
    let mut l = tools(CFG);
    assert_eq!(l.key(Key::Backspace), Outcome::Nothing, "a --tools open has no All to go back to");
    assert_eq!(l.mode, Mode::Tools);
    let mut l = apps(CFG);
    typed(&mut l, "/");
    assert_eq!((l.mode, l.query.as_str()), (Mode::Tools, ""));
    select(&mut l, "Nix");
    l.key(Key::Enter);
    assert_eq!(l.key(Key::Backspace), Outcome::Redraw, "below the root: up one level");
    assert_eq!((l.mode, breadcrumb(&l)), (Mode::Tools, "Tools".to_string()));
    assert_eq!(l.key(Key::Backspace), Outcome::BackToAll);
    assert_eq!(l.mode, Mode::All);
}

#[test]
fn going_up_restores_the_selection() {
    let mut l = tools(CFG);
    select(&mut l, "Zed");
    l.key(Key::Enter);
    l.key(Key::Down);
    assert_eq!(l.selected, 1);
    l.key(Key::Left);
    assert_eq!(selected_text(&l), "Zed", "the parent's selection comes back");
    l.key(Key::Enter);
    assert_eq!(l.selected, 0, "going down selects the first row");
}

#[test]
fn a_level_change_empties_the_query() {
    let mut l = tools(CFG);
    typed(&mut l, "generation");
    assert_eq!(selected_text(&l), "Generation", "the flat search at the root");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "an entry with choices drills");
    assert_eq!(l.query, "");
    assert_eq!(texts(&l), ["list", "diff"]);
    assert_eq!(breadcrumb(&l), "Tools › Nix › Generation");
}

#[test]
fn the_query_filters_the_current_level() {
    let mut l = tools(CFG);
    select(&mut l, "Nix");
    l.key(Key::Enter);
    typed(&mut l, "col");
    assert_eq!(texts(&l), ["Collect"], "only the level's entries, not the whole catalog");
    typed(&mut l, "zzz");
    assert!(texts(&l).is_empty());
    // An entry of another category is not reached from here (the flat search is the root's).
    l.key(Key::CtrlU);
    typed(&mut l, "nixify");
    assert!(texts(&l).is_empty(), "{:?}", texts(&l));
    l.key(Key::CtrlU);
    l.key(Key::Left);
    typed(&mut l, "nixify");
    assert_eq!(texts(&l), ["Nixify"], "at the root it is");
}

#[test]
fn root_search_ranks_entry_hits_before_category_hits() {
    let mut l = tools(CFG);
    typed(&mut l, "nix");
    assert_eq!(
        texts(&l),
        ["Nixify", "Update", "Collect", "Generation", "Generation · list", "Generation · diff"],
        "the entry that names it first, then what only its category matches, in catalog order"
    );
}

#[test]
fn rows_have_the_right_trails() {
    let mut l = tools(CFG);
    let trail = |l: &Launcher, name: &str| {
        let i = texts(l).iter().position(|t| t == name).unwrap();
        match l.results[i].source {
            Source::Tool(r) => l.tool_rows()[r].trail.clone(),
            _ => unreachable!(),
        }
    };
    assert_eq!(trail(&l, "Home thing"), Trail::None, "no state read yet: nothing");
    assert_eq!(trail(&l, "Nix"), Trail::Chevron);
    let home = l.tools().unwrap().catalog.entries.iter().position(|e| e.name == "Home thing").unwrap();
    l.adopt_state(home, Some(true));
    assert_eq!(trail(&l, "Home thing"), Trail::Switch(true));
    l.adopt_state(home, Some(false));
    assert_eq!(trail(&l, "Home thing"), Trail::Switch(false));
    l.adopt_state(home, None);
    assert_eq!(trail(&l, "Home thing"), Trail::None, "unknown: no switch");
    select(&mut l, "Nix");
    l.key(Key::Enter);
    assert_eq!(trail(&l, "Generation"), Trail::Chevron);
    assert_eq!(trail(&l, "Update"), Trail::None);
    select(&mut l, "Generation");
    l.key(Key::Enter);
    assert_eq!(trail(&l, "list"), Trail::None);
}

#[test]
fn confirm_asks_waits_runs_and_expires() {
    let mut l = tools(CFG);
    select(&mut l, "Nix");
    l.key(Key::Enter);
    select(&mut l, "Collect");
    let collect = l.tools().unwrap().catalog.entries.iter().position(|e| e.name == "Collect").unwrap();
    let t0 = Instant::now();
    assert_eq!(l.tools().unwrap().next_deadline(), None, "no question, no deadline");
    assert_eq!(key_at(&mut l, Key::Enter, t0), Outcome::Asked);
    assert!(l.tools().unwrap().asking(RowId::Entry(collect)));
    assert_eq!(l.tools().unwrap().next_deadline(), Some(t0 + session::CONFIRM));
    assert_eq!(key_at(&mut l, Key::Enter, t0 + 300 * MS), Outcome::Nothing, "a double Enter never confirms");
    assert_eq!(
        key_at(&mut l, Key::Enter, t0 + 500 * MS),
        Outcome::Activate(Activation::Tool(ToolAction::User { entry: collect, choice: None, text: None }))
    );
    assert_eq!(l.tools().unwrap().next_deadline(), None, "answered: no deadline");
    // Asked again, then left alone: 3 s expires it (the host redraws).
    assert_eq!(key_at(&mut l, Key::Enter, t0 + 10_000 * MS), Outcome::Asked);
    assert!(!l.tools_expire(t0 + 12_999 * MS));
    assert!(l.tools_expire(t0 + 13_000 * MS));
    assert_eq!(l.tools().unwrap().next_deadline(), None);
    assert_eq!(key_at(&mut l, Key::Enter, t0 + 20_000 * MS), Outcome::Asked, "an expired question never answers");
}

#[test]
fn any_other_key_cancels_the_question() {
    let mut l = tools(CFG);
    select(&mut l, "Nix");
    l.key(Key::Enter);
    select(&mut l, "Collect");
    let t0 = Instant::now();
    assert_eq!(key_at(&mut l, Key::Enter, t0), Outcome::Asked);
    assert_eq!(key_at(&mut l, Key::Down, t0 + 100 * MS), Outcome::Redraw);
    assert_eq!(key_at(&mut l, Key::Up, t0 + 200 * MS), Outcome::Redraw);
    assert_eq!(l.tools().unwrap().next_deadline(), None);
    assert_eq!(key_at(&mut l, Key::Enter, t0 + 600 * MS), Outcome::Asked, "asks again instead of running");
    // Another row's activation answers no question of this one.
    l.key(Key::Up);
    assert_eq!(selected_text(&l), "Update");
    assert!(matches!(key_at(&mut l, Key::Enter, t0 + 1_200 * MS), Outcome::Activate(_)));
}

#[test]
fn a_confirm_choice_asks_with_its_own_row() {
    let text = CFG.replace("choices = [\"list\", \"diff\"]", "choices = [\"list\", \"diff\"]\nconfirm = true");
    let mut l = tools(&text);
    select(&mut l, "Nix");
    l.key(Key::Enter);
    select(&mut l, "Generation");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "the entry itself only drills");
    select(&mut l, "diff");
    let t0 = Instant::now();
    assert_eq!(key_at(&mut l, Key::Enter, t0), Outcome::Asked);
    let generation = l.tools().unwrap().catalog.entries.iter().position(|e| e.name == "Generation").unwrap();
    assert!(l.tools().unwrap().asking(RowId::Choice(generation, 1)));
    assert!(!l.tools().unwrap().asking(RowId::Choice(generation, 0)), "the question is the row's, not the level's");
    assert_eq!(
        key_at(&mut l, Key::Enter, t0 + 500 * MS),
        Outcome::Activate(Activation::Tool(ToolAction::User { entry: generation, choice: Some("diff".into()), text: None }))
    );
}

#[test]
fn a_held_enter_does_nothing_in_tools() {
    let mut l = tools(CFG);
    select(&mut l, "Nix");
    assert_eq!(l.key_repeat(Key::Enter, true), Outcome::Nothing);
    assert_eq!(breadcrumb(&l), "Tools", "a repeat never drills");
    l.key(Key::Enter);
    select(&mut l, "Update");
    assert_eq!(l.key_repeat(Key::Enter, true), Outcome::Nothing, "nor runs");
    assert!(matches!(l.key(Key::Enter), Outcome::Activate(_)), "the first press does");
    let t0 = Instant::now();
    select(&mut l, "Collect");
    l.set_now(t0);
    assert_eq!(l.key_repeat(Key::Enter, true), Outcome::Nothing, "nor asks");
    assert_eq!(l.tools().unwrap().next_deadline(), None);
}

#[test]
fn ctrl_enter_and_tab_do_nothing_in_tools() {
    let mut l = tools(CFG);
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Nothing);
    assert_eq!(l.key(Key::Tab), Outcome::Nothing);
    assert_eq!(l.key(Key::BackTab), Outcome::Nothing);
    assert_eq!(l.mode, Mode::Tools);
}

#[test]
fn a_choices_cmd_entry_drills_with_a_job_and_reading() {
    let mut l = tools(CFG);
    select(&mut l, "Zed");
    l.key(Key::Enter);
    select(&mut l, "Pick");
    let o = l.key(Key::Enter);
    let Outcome::ReadChoices(Job::Choices { entry, position, argv, token }) = o else { panic!("{o:?}") };
    assert_eq!((position, argv), (4, vec!["lines".to_string()]));
    assert_eq!(breadcrumb(&l), "Tools › Zed › Pick");
    assert!(l.tools().unwrap().reading);
    assert!(texts(&l).is_empty(), "nothing selectable while reading");
    assert!(l.adopt_choices(entry, token, vec!["alpha".into(), "beta".into()], false));
    assert!(!l.tools().unwrap().reading);
    assert_eq!(texts(&l), ["alpha", "beta"]);
    select(&mut l, "beta");
    assert_eq!(l.key(Key::Enter), Outcome::Activate(Activation::Tool(ToolAction::User { entry, choice: Some("beta".into()), text: None })));
}

#[test]
fn a_failed_read_leaves_the_level_empty_and_says_so() {
    let mut l = tools(CFG);
    select(&mut l, "Zed");
    l.key(Key::Enter);
    select(&mut l, "Pick");
    let Outcome::ReadChoices(Job::Choices { entry, token, .. }) = l.key(Key::Enter) else { panic!() };
    assert!(l.adopt_choices(entry, token, Vec::new(), true));
    assert!(texts(&l).is_empty());
    assert_eq!(l.message.as_deref(), Some("could not read the choices"));
    assert!(!l.tools().unwrap().reading);
}

#[test]
fn a_stale_choices_delivery_is_refused() {
    let mut l = tools(CFG);
    select(&mut l, "Zed");
    l.key(Key::Enter);
    select(&mut l, "Pick");
    let Outcome::ReadChoices(Job::Choices { entry, token, .. }) = l.key(Key::Enter) else { panic!() };
    assert!(!l.adopt_choices(entry, token + 1, vec!["x".into()], false), "another token");
    assert!(l.tools().unwrap().reading, "still waiting for the right one");
    // The level was left: its delivery is dropped.
    l.key(Key::Left);
    assert!(!l.adopt_choices(entry, token, vec!["x".into()], false));
    // Opened again: a new token, the old delivery still refused.
    select(&mut l, "Pick");
    let Outcome::ReadChoices(Job::Choices { token: second, .. }) = l.key(Key::Enter) else { panic!() };
    assert_ne!(second, token);
    assert!(!l.adopt_choices(entry, token, vec!["old".into()], false));
    assert!(l.adopt_choices(entry, second, vec!["new".into()], false));
    assert_eq!(texts(&l), ["new"]);
}

#[test]
fn has_modes_is_false_in_tools_and_is_bare_unchanged() {
    let l = tools(CFG);
    assert!(!l.has_modes() && !l.is_bare());
    assert!(Launcher::for_calc(LauncherConfig::default()).is_bare());
    assert!(Launcher::for_script(LauncherConfig::default(), None, Vec::new()).is_bare());
    assert!(Launcher::for_clipboard(LauncherConfig::default(), Vec::new()).is_bare());
    // Entered from Mod+Space, the box keeps the row count it was laid out with.
    let mut l = apps(CFG);
    assert!(l.has_modes());
    typed(&mut l, "/");
    assert_eq!(l.mode, Mode::Tools);
    assert!(l.has_modes(), "the layout is decided once per open");
}

// -- Mod+Space's search, the `/` prefix --

fn catalog_entry(l: &Launcher, name: &str) -> usize {
    l.tools().unwrap().catalog.entries.iter().position(|e| e.name == name).unwrap()
}

fn tool_hits(l: &Launcher) -> Vec<String> {
    l.results.iter().filter_map(|h| if let Source::Tool(i) = h.source { Some(l.tool_rows()[i].text.clone()) } else { None }).collect()
}

#[test]
fn tool_rows_join_a_typed_all_query_only() {
    let mut l = apps(CFG);
    assert!(tool_hits(&l).is_empty(), "an empty query shows what it shows today");
    typed(&mut l, "upd");
    assert_eq!(tool_hits(&l), ["Update"]);
    for (prefix, other) in [("?", "?upd"), (":", ":upd"), ("$", "$upd"), (">", ">upd")] {
        let mut l = apps(CFG);
        typed(&mut l, other);
        assert!(tool_hits(&l).is_empty(), "{prefix}: a prefix lists only its own rows");
    }
    let mut l = apps(CFG);
    l.key(Key::Tab); // Apps
    typed(&mut l, "upd");
    assert!(tool_hits(&l).is_empty(), "Apps");
    l.key(Key::Tab); // Windows
    assert!(tool_hits(&l).is_empty(), "Windows");
    l.key(Key::Tab); // Commands
    assert!(tool_hits(&l).is_empty(), "Commands");
}

#[test]
fn the_slash_prefix_jumps_into_tools_and_backspace_returns() {
    let mut l = apps(CFG);
    let o = l.key(Key::Char('/'));
    let Outcome::EnterTools(Some(Job::States(jobs, _))) = o else { panic!("{o:?}") };
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].position, jobs[0].argv.clone()), (5, vec!["s".to_string()]), "Home thing, the entry that has a state");
    assert_eq!((l.mode, l.query.as_str()), (Mode::Tools, ""));
    assert_eq!(texts(&l), ["Home thing", "Nix", "Zed"]);
    assert_eq!(l.key(Key::Backspace), Outcome::BackToAll);
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, ""));
    // Typed again: the jump is back at the root, whatever was left open.
    l.key(Key::Char('/'));
    select(&mut l, "Nix");
    l.key(Key::Enter);
    l.key(Key::Escape);
    l.key(Key::Escape);
    assert_eq!(l.mode, Mode::Tools, "Esc at the Tools root closes the whole box, it does not go back");
}

#[test]
fn a_prefix_jump_without_a_state_reads_nothing() {
    let mut l = apps("[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Plain\"\nrun = [\"x\"]\n");
    assert_eq!(l.key(Key::Char('/')), Outcome::EnterTools(None), "the host still reads the built-ins' states");
}

#[test]
fn an_empty_catalog_installs_nothing_and_the_slash_stays_text() {
    let mut l = apps("[tools]\nbuiltins = false\n");
    assert!(l.tools().is_none(), "no rows, no Tools state");
    assert_eq!(l.key(Key::Char('/')), Outcome::Redraw);
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, "/"), "the query is kept");
}

#[test]
fn the_query_setter_never_jumps_by_prefix() {
    let mut l = apps("[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Plain\"\nrun = [\"x\"]\n");
    l.config.prefixes.tools = ":".into();
    l.set_query(":");
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, ":"));
}

#[test]
fn with_the_prefix_off_a_slash_is_plain_text() {
    let mut l = apps(&format!("[prefixes]\ntools = \"\"\n{CFG}"));
    typed(&mut l, "/x");
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, "/x"));
    // A catalog never set: the jump has nothing to show, so the slash stays text.
    let mut l = Launcher::for_apps(LauncherConfig::default(), Vec::new(), History::empty(200), 1_800_000_000);
    typed(&mut l, "/");
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, "/"));
}

#[test]
fn enter_on_a_plain_tool_runs_it_and_a_builtin_is_handed_out() {
    let mut l = apps(CFG);
    typed(&mut l, "update");
    let update = catalog_entry(&l, "Update");
    assert_eq!(l.key(Key::Enter), Outcome::Activate(Activation::Tool(ToolAction::User { entry: update, choice: None, text: None })));
    // The built-ins join the search as well (a context that shows Notes).
    let (config, catalog) = catalog("");
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), 1_800_000_000);
    l.set_tools(catalog);
    typed(&mut l, "notes");
    let Outcome::Activate(Activation::Tool(ToolAction::Builtin { index: b, .. })) = l.key(Key::Enter) else { panic!() };
    assert_eq!(crate::launcher::tools::catalog::BUILTINS[b].id, "notes");
}

#[test]
fn enter_on_a_tool_with_choices_drills() {
    let mut l = apps(CFG);
    typed(&mut l, "generation");
    assert_eq!(selected_text(&l), "Generation");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    assert_eq!((l.mode, l.origin), (Mode::Tools, crate::launcher::model::Origin::Apps), "origin unchanged");
    assert_eq!(breadcrumb(&l), "Tools › Nix › Generation");
    assert_eq!(texts(&l), ["list", "diff"]);
    assert_eq!(l.key(Key::Backspace), Outcome::Redraw, "up to the category");
    assert_eq!(l.key(Key::Backspace), Outcome::Redraw, "and the root");
    assert_eq!(breadcrumb(&l), "Tools");
    assert_eq!(l.key(Key::Backspace), Outcome::BackToAll);
    // A fixed choice from the flat search runs at once.
    typed(&mut l, "generation diff");
    assert_eq!(selected_text(&l), "Generation · diff");
    let generation = catalog_entry(&l, "Generation");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::User { entry: generation, choice: Some("diff".into()), text: None }))
    );
    // A choices_cmd entry drills with its read.
    let mut l = apps(CFG);
    typed(&mut l, "pick");
    let o = l.key(Key::Enter);
    assert!(matches!(o, Outcome::ReadChoices(Job::Choices { position: 4, .. })), "{o:?}");
    assert_eq!(breadcrumb(&l), "Tools › Zed › Pick");
}

#[test]
fn enter_on_a_confirm_tool_in_all_asks_in_the_tools_mode() {
    let mut l = apps(CFG);
    typed(&mut l, "collect");
    let collect = catalog_entry(&l, "Collect");
    let t0 = Instant::now();
    assert_eq!(key_at(&mut l, Key::Enter, t0), Outcome::Asked);
    assert_eq!(l.mode, Mode::Tools);
    assert_eq!(breadcrumb(&l), "Tools › Nix");
    assert_eq!(selected_text(&l), "Collect");
    assert!(l.tools().unwrap().asking(RowId::Entry(collect)));
    assert_eq!(l.tools_deadline(), Some(t0 + session::CONFIRM));
    assert_eq!(key_at(&mut l, Key::Enter, t0 + 100 * MS), Outcome::Nothing, "a double Enter never confirms");
    assert_eq!(
        key_at(&mut l, Key::Enter, t0 + 500 * MS),
        Outcome::Activate(Activation::Tool(ToolAction::User { entry: collect, choice: None, text: None }))
    );
    // A root confirm entry asks at the root.
    let mut l = apps(&CFG.replace("run = [\"x\", \"home\"]", "run = [\"x\", \"home\"]\nconfirm = true"));
    typed(&mut l, "home");
    assert_eq!(l.key(Key::Enter), Outcome::Asked);
    assert_eq!((breadcrumb(&l), selected_text(&l)), ("Tools".to_string(), "Home thing".to_string()));
}

#[test]
fn a_confirm_choice_from_all_asks_at_its_own_row() {
    let text = CFG.replace("choices = [\"list\", \"diff\"]", "choices = [\"list\", \"diff\"]\nconfirm = true");
    let mut l = apps(&text);
    typed(&mut l, "generation diff");
    let generation = catalog_entry(&l, "Generation");
    assert_eq!(l.key(Key::Enter), Outcome::Asked);
    assert_eq!(breadcrumb(&l), "Tools › Nix › Generation");
    assert_eq!(selected_text(&l), "diff");
    assert!(l.tools().unwrap().asking(RowId::Choice(generation, 1)));
}

#[test]
fn drilling_from_a_flat_search_pushes_the_category_level() {
    for mut l in [tools(CFG), apps(CFG)] {
        if l.origin == crate::launcher::model::Origin::Apps {
            typed(&mut l, "/");
        }
        typed(&mut l, "generation");
        assert_eq!(selected_text(&l), "Generation");
        assert_eq!(l.key(Key::Enter), Outcome::Redraw);
        assert_eq!(breadcrumb(&l), "Tools › Nix › Generation");
        assert_eq!(l.key(Key::Left), Outcome::Redraw);
        assert_eq!(breadcrumb(&l), "Tools › Nix", "← lands on the category the breadcrumb shows");
        assert_eq!(selected_text(&l), "Generation", "on the row it came through");
        assert_eq!(l.key(Key::Escape), Outcome::Redraw);
        assert_eq!(breadcrumb(&l), "Tools");
        assert_eq!(selected_text(&l), "Nix");
    }
}

#[test]
fn a_jump_from_all_restores_the_row_it_came_through() {
    let mut l = apps(CFG);
    typed(&mut l, "pick");
    l.key(Key::Enter);
    assert_eq!(breadcrumb(&l), "Tools › Zed › Pick");
    l.key(Key::Left);
    assert_eq!(selected_text(&l), "Pick");
    l.key(Key::Left);
    assert_eq!(selected_text(&l), "Zed");
    // An entry with choices that is not first in its category.
    let mut l = apps(CFG);
    typed(&mut l, "generation");
    l.key(Key::Enter);
    l.key(Key::Left);
    assert_eq!((breadcrumb(&l), selected_text(&l)), ("Tools › Nix".to_string(), "Generation".to_string()));
    // A confirm entry asked from All: up lands on its own row too.
    let mut l = apps(CFG);
    typed(&mut l, "collect");
    l.key(Key::Enter);
    l.key(Key::Left);
    assert_eq!((breadcrumb(&l), selected_text(&l)), ("Tools".to_string(), "Nix".to_string()));
}

#[test]
fn a_right_click_on_another_row_cancels_the_question() {
    let mut l = tools(CFG);
    select(&mut l, "Nix");
    l.key(Key::Enter);
    select(&mut l, "Collect");
    let collect = l.selected;
    assert_eq!(l.key(Key::Enter), Outcome::Asked);
    assert_eq!(l.click(collect, true), Outcome::Nothing, "its own row's right click leaves the question");
    assert!(l.tools().unwrap().next_deadline().is_some());
    assert_eq!(l.click(0, true), Outcome::Redraw, "another row's cancels it");
    assert!(l.tools().unwrap().next_deadline().is_none());
}

/// A jump while a read is out drops it: its late delivery is stale.
#[test]
fn a_jump_cancels_the_pending_choices_read() {
    let (config, catalog) = catalog(CFG);
    let pick = catalog.entries.iter().position(|e| e.name == "Pick").unwrap();
    let mut t = ToolsState::new(catalog);
    let Step::Drill(Some(Job::Choices { token, .. })) = t.goto_choices(&config.tools, pick) else { panic!() };
    assert!(t.reading);
    t.goto_entry(pick);
    assert!(!t.reading, "the read is no longer waited for");
    assert!(!t.adopt_choices(pick, token, vec!["late".into()], false));
}

/// Leaving the Tools mode for another drops a pending read, so All never holds a stale level.
#[test]
fn leaving_tools_by_a_mode_click_cancels_the_pending_read() {
    let (config, catalog) = catalog(CFG);
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), 1_800_000_000);
    l.set_tools(catalog);
    l.key(Key::Char('/'));
    select(&mut l, "Zed");
    l.key(Key::Enter);
    select(&mut l, "Pick");
    let Outcome::ReadChoices(Job::Choices { entry, token, .. }) = l.key(Key::Enter) else { panic!() };
    l.click_mode(Mode::All);
    assert!(!l.tools().unwrap().reading);
    assert!(!l.adopt_choices(entry, token, vec!["late".into()], false));
    assert_eq!(l.message, None, "no message nor refilter in All");
}

// ---- the typed text step (`ask`) ----

const ASK: &str = r#"
[tools]
builtins = false
[[tools.category]]
name = "Net"
[[tools.entry]]
name = "Scan"
category = "Net"
run = ["nmap", "{choice}", "{text}"]
choices = ["quick", "full"]
ask = "Target"
[[tools.entry]]
name = "Ping"
category = "Net"
run = ["ping", "-c1", "{text}"]
ask = "Host"
[[tools.entry]]
name = "Plain"
run = ["x"]
"#;

fn ask_entry(l: &Launcher, name: &str) -> usize {
    l.tools().unwrap().catalog.entries.iter().position(|e| e.name == name).unwrap()
}

#[test]
fn an_entry_with_ask_prompts_after_its_choice() {
    let mut l = tools(ASK);
    select(&mut l, "Net");
    l.key(Key::Enter);
    select(&mut l, "Scan");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "the entry drills into its choices first");
    select(&mut l, "full");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "a choice of an ask entry opens the prompt, runs nothing");
    assert_eq!(texts(&l), ["Target"], "one row: the label");
    assert_eq!(breadcrumb(&l), "Tools › Net › Scan › full");
    typed(&mut l, "10.0.0.0/24");
    assert_eq!(texts(&l), ["Target"], "what is typed answers, it never filters the prompt");
    let scan = ask_entry(&l, "Scan");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::User {
            entry: scan,
            choice: Some("full".into()),
            text: Some("10.0.0.0/24".into())
        }))
    );
    assert!(l.query.is_empty(), "the answer is moved out of the box");
}

#[test]
fn an_ask_entry_without_choices_prompts_directly() {
    let mut l = tools(ASK);
    select(&mut l, "Net");
    l.key(Key::Enter);
    select(&mut l, "Ping");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    assert_eq!(texts(&l), ["Host"]);
    assert_eq!(breadcrumb(&l), "Tools › Net › Ping");
    typed(&mut l, "a b; rm -rf /");
    let ping = ask_entry(&l, "Ping");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::User { entry: ping, choice: None, text: Some("a b; rm -rf /".into()) }))
    );
    // A plain entry still runs at once.
    let mut l = tools(ASK);
    select(&mut l, "Plain");
    assert!(matches!(l.key(Key::Enter), Outcome::Activate(_)));
}

#[test]
fn esc_in_the_prompt_returns_to_the_choices() {
    let mut l = tools(ASK);
    select(&mut l, "Net");
    l.key(Key::Enter);
    select(&mut l, "Scan");
    l.key(Key::Enter);
    select(&mut l, "full");
    l.key(Key::Enter);
    typed(&mut l, "half");
    assert_eq!(l.key(Key::Escape), Outcome::Redraw);
    assert_eq!(texts(&l), ["quick", "full"], "back on the choices, the typed text dropped");
    assert_eq!(selected_text(&l), "full", "on the choice it came from");
    assert!(l.query.is_empty());
    // And an entry with no choices goes back to its category.
    l.key(Key::Escape);
    select(&mut l, "Ping");
    l.key(Key::Enter);
    assert_eq!(l.key(Key::Escape), Outcome::Redraw);
    assert_eq!(texts(&l), ["Scan", "Ping"]);
    // The choice does not leak into the next prompt.
    select(&mut l, "Ping");
    l.key(Key::Enter);
    typed(&mut l, "h");
    let ping = ask_entry(&l, "Ping");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::User { entry: ping, choice: None, text: Some("h".into()) }))
    );
}

#[test]
fn an_empty_text_runs_nothing() {
    let mut l = tools(ASK);
    select(&mut l, "Net");
    l.key(Key::Enter);
    select(&mut l, "Ping");
    l.key(Key::Enter);
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
    typed(&mut l, "   ");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "blank is empty, and the emptied line is redrawn");
    assert_eq!(texts(&l), ["Host"], "still in the prompt");
}

#[test]
fn the_flat_search_and_mod_space_reach_the_prompt() {
    // Flat search at the Tools root: a fixed choice of an ask entry goes down through its choices.
    let mut l = tools(ASK);
    typed(&mut l, "scan full");
    select(&mut l, "Scan · full");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    assert_eq!(breadcrumb(&l), "Tools › Net › Scan › full");
    l.key(Key::Escape);
    assert_eq!(texts(&l), ["quick", "full"]);
    // The entry itself from the flat search.
    let mut l = tools(ASK);
    typed(&mut l, "ping");
    select(&mut l, "Ping");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    assert_eq!(breadcrumb(&l), "Tools › Net › Ping");
    // Mod+Space: the hit switches the box to the Tools mode, at the prompt.
    let mut l = apps(ASK);
    typed(&mut l, "ping");
    let at = l.results.iter().position(|h| matches!(h.source, Source::Tool(_))).expect("the tool hit");
    l.selected = at;
    assert!(!matches!(l.key(Key::Enter), Outcome::Activate(_)), "it asks first, it never runs");
    assert_eq!(l.mode, Mode::Tools);
    assert_eq!(breadcrumb(&l), "Tools › Net › Ping");
    assert_eq!(texts(&l), ["Host"]);
}

#[test]
fn a_right_arrow_on_an_ask_entry_opens_its_prompt() {
    let mut l = tools(ASK);
    select(&mut l, "Net");
    l.key(Key::Enter);
    select(&mut l, "Ping");
    assert_eq!(l.key(Key::Right), Outcome::Redraw);
    assert_eq!(texts(&l), ["Host"]);
    assert_eq!(l.key(Key::Right), Outcome::Nothing, "→ in the prompt does nothing");
}

/// A Tools box over the built-ins only, with the diagnostics on.
fn diag_box() -> Launcher {
    let config = LauncherConfig::from_text("").unwrap();
    let c = Context { nmap: true, usb_reset: true, diagnostics: true, ..Context::default() };
    let catalog = Catalog::build(&config.tools, &c, &mut |_| {});
    Launcher::for_tools(config, catalog, History::empty(200), 1_800_000_000)
}

fn builtin_index(id: &str) -> usize {
    crate::launcher::tools::catalog::BUILTINS.iter().position(|b| b.id == id).unwrap()
}

fn into_scan_choices(l: &mut Launcher) {
    select(l, "Diagnostics");
    l.key(Key::Enter);
    select(l, "Network scan");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "the scan drills into its ten scans");
    assert_eq!(texts(l).len(), 10);
}

#[test]
fn a_scan_asks_its_target_then_runs_with_it_whole() {
    use crate::launcher::tools::diag::{Scan, TARGET_PROMPT};
    let mut l = diag_box();
    into_scan_choices(&mut l);
    select(&mut l, Scan::Quick.label());
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    assert_eq!(texts(&l), [TARGET_PROMPT]);
    typed(&mut l, "a b; -x");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::Builtin {
            index: builtin_index("diag.scan"),
            choice: Some(Scan::Quick.label().into()),
            text: Some("a b; -x".into()),
        }))
    );
    assert!(l.query.is_empty());
}

#[test]
fn the_sweep_runs_at_once_and_a_port_scan_asks_for_the_ports_too() {
    use crate::launcher::tools::diag::{PORTS_PROMPT, Scan};
    let mut l = diag_box();
    into_scan_choices(&mut l);
    select(&mut l, Scan::HostDiscovery.label());
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::Builtin {
            index: builtin_index("diag.scan"),
            choice: Some(Scan::HostDiscovery.label().into()),
            text: None,
        })),
        "no prompt: the subnet is the box's own"
    );
    let mut l = diag_box();
    into_scan_choices(&mut l);
    select(&mut l, Scan::Ports.label());
    l.key(Key::Enter);
    assert_eq!(texts(&l), [PORTS_PROMPT]);
}

#[test]
fn a_typed_scan_target_never_shows_in_debug_output() {
    let a = ToolAction::Builtin { index: 0, choice: Some("c".into()), text: Some("secret-host".into()) };
    assert!(!format!("{a:?}").contains("secret-host"));
}

#[test]
fn a_flat_search_choice_of_the_scan_asks_too() {
    use crate::launcher::tools::diag::{Scan, TARGET_PROMPT};
    let mut l = diag_box();
    typed(&mut l, "Aggressive");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "the choice's prompt opens");
    assert_eq!(texts(&l), [TARGET_PROMPT]);
    typed(&mut l, "h");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::Builtin {
            index: builtin_index("diag.scan"),
            choice: Some(Scan::Aggressive.label().into()),
            text: Some("h".into()),
        }))
    );
}

/// A Tools launcher with the built-ins, a shutdown pending or not.
fn shutdown_tools(left: Option<u64>) -> Launcher {
    let config = LauncherConfig::from_text("").unwrap();
    let ctx = Context { timer: true, shutdown: true, shutdown_left: left, ..Context::default() };
    let catalog = Catalog::build(&config.tools, &ctx, &mut |_| {});
    Launcher::for_tools(config, catalog, History::empty(200), 1_800_000_000)
}

#[test]
fn a_builtin_choice_activates_the_builtin_with_its_label() {
    let mut l = shutdown_tools(Some(80 * 60_000));
    select(&mut l, "System");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    select(&mut l, "Schedule shutdown");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "an entry with choices drills");
    assert_eq!(texts(&l), ["Cancel shutdown (in 1h20)", "30 min", "1 h", "1 h 30", "2 h", "3 h"]);
    let b = crate::launcher::tools::catalog::BUILTINS.iter().position(|b| b.id == "shutdown").unwrap();
    select(&mut l, "1 h 30");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::Builtin { index: b, choice: Some("1 h 30".into()), text: None }))
    );
    select(&mut l, "Cancel shutdown (in 1h20)");
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::Builtin { index: b, choice: Some("Cancel shutdown (in 1h20)".into()), text: None }))
    );
}

#[test]
fn the_usb_reset_asks_for_a_second_enter() {
    let mut l = diag_box();
    select(&mut l, "Diagnostics");
    l.key(Key::Enter);
    select(&mut l, "Reset USB (xHCI)");
    let t0 = Instant::now();
    assert_eq!(key_at(&mut l, Key::Enter, t0), Outcome::Asked, "the first Enter asks");
    let run = key_at(&mut l, Key::Enter, t0 + 500 * MS);
    assert_eq!(
        run,
        Outcome::Activate(Activation::Tool(ToolAction::Builtin { index: builtin_index("diag.usb_reset"), choice: None, text: None }))
    );
}

#[test]
fn a_mod_space_hit_on_the_sweep_runs_it_and_one_on_a_scan_asks() {
    use crate::launcher::tools::diag::{Scan, TARGET_PROMPT};
    let config = LauncherConfig::from_text("").unwrap();
    let c = Context { nmap: true, ..Context::default() };
    let catalog = Catalog::build(&config.tools, &c, &mut |_| {});
    let mut l = Launcher::for_apps(config.clone(), Vec::new(), History::empty(200), 1_800_000_000);
    l.set_tools(catalog.clone());
    typed(&mut l, "Host discovery");
    let out = l.key(Key::Enter);
    assert_eq!(
        out,
        Outcome::Activate(Activation::Tool(ToolAction::Builtin {
            index: builtin_index("diag.scan"),
            choice: Some(Scan::HostDiscovery.label().into()),
            text: None,
        })),
        "no prompt for the sweep, from Mod+Space either"
    );
    assert_eq!(l.mode, Mode::All, "and the box did not detour through the Tools mode");
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), 1_800_000_000);
    l.set_tools(catalog);
    typed(&mut l, "Aggressive scan");
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "a scan goes to the Tools mode's prompt");
    assert_eq!(texts(&l), [TARGET_PROMPT]);
}

#[test]
fn a_builtin_choice_is_reachable_from_the_flat_search() {
    let mut l = shutdown_tools(None);
    typed(&mut l, "shutdown 2 h");
    let t = selected_text(&l);
    assert!(t.contains("2 h"), "{t}");
    let b = crate::launcher::tools::catalog::BUILTINS.iter().position(|b| b.id == "shutdown").unwrap();
    // The flat row of a fixed choice: Enter drills nowhere, it activates.
    assert_eq!(
        l.key(Key::Enter),
        Outcome::Activate(Activation::Tool(ToolAction::Builtin { index: b, choice: Some("2 h".into()), text: None }))
    );
}

/// A letter runs only from the opened panel's root, and only a tool the root lists: a tool of a
/// category keeps its letter inert there; below the root, every letter is.
#[test]
fn a_letter_runs_only_a_root_tool_of_the_opened_panel() {
    let text = format!("{CFG}key = \"y\"\n[[tools.entry]]\nname = \"Rooted\"\nrun = [\"x\"]\nkey = \"r\"\n");
    let (_, cat) = catalog(&text);
    let nixify = cat.entries.iter().position(|e| e.name == "Nixify").unwrap();
    let rooted = cat.entries.iter().position(|e| e.name == "Rooted").unwrap();
    assert_eq!(cat.entries[nixify].letter, Some('y'), "claimed all the same");
    let mut t = ToolsState::new(cat);
    assert_eq!(t.letter('r'), None, "not opened as the panel");
    assert!(!t.root_letters());
    t.open_panel(104, 30);
    assert_eq!(t.letter('r'), Some(RowId::Entry(rooted)));
    assert_eq!(t.letter('y'), None, "Nixify is in Zed, not at the root");
    assert!(t.root_letters());
    t.open(Level::Category(0), 0);
    assert_eq!(t.letter('r'), None, "below the root");
    assert!(!t.root_letters());
}

#[test]
fn a_panel_with_no_room_is_not_opened() {
    let (_, cat) = catalog(CFG);
    let mut t = ToolsState::new(cat);
    t.open_panel(104, 0);
    assert_eq!(t.panel_size(), None);
    assert!(!t.at_panel());
    t.open_panel(104, 30);
    assert_eq!(t.panel_size(), Some((104, 30)));
}
