use super::*;
use crate::launcher::calc::Answer;
use crate::launcher::config::SourceConfig;
use crate::launcher::desktop::{DesktopAction, DesktopEntry};
use crate::launcher::sources::SourceEntry;
use std::path::PathBuf;

const NOW: u64 = 1_800_000_000;

fn app(id: &str, name: &str, actions: &[&str]) -> DesktopEntry {
    DesktopEntry {
        id: id.into(),
        path: PathBuf::from(format!("/usr/share/applications/{id}")),
        name: name.into(),
        generic_name: None,
        comment: None,
        keywords: vec![],
        icon: None,
        exec: name.to_lowercase(),
        terminal: false,
        startup_wm_class: None,
        actions: actions
            .iter()
            .map(|a| DesktopAction { key: a.to_lowercase().replace(' ', "-"), name: a.to_string(), exec: "x".into() })
            .collect(),
    }
}

fn catalog() -> Catalog {
    Catalog {
        apps: vec![
            app("org.mozilla.firefox.desktop", "Firefox", &["New Window", "New Private Window"]),
            app("org.gnome.Nautilus.desktop", "Files", &[]),
            app("fish.desktop", "Fish", &[]),
        ],
        commands: vec!["fd".into(), "fish".into(), "htop".into()],
    }
}

fn window(selector: &str, class: &str, title: &str) -> Toplevel {
    Toplevel { selector: selector.into(), class: class.into(), title: title.into(), workspace: "1".into(), focused: false }
}

fn windows() -> Vec<Toplevel> {
    vec![window("id:1", "foot", "fish shell"), window("id:2", "firefox", "Mail"), window("id:3", "foot", "notes")]
}

fn launcher() -> Launcher {
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    l.set_catalog(catalog());
    l
}

fn typed(l: &mut Launcher, text: &str) {
    for c in text.chars() {
        l.key(Key::Char(c));
    }
}

fn sources(l: &Launcher) -> Vec<Source> {
    l.results.iter().map(|h| h.source).collect()
}

fn activate(a: Activation) -> Outcome {
    Outcome::Activate(a)
}

#[test]
fn opens_on_windows_before_the_catalog_arrives() {
    let l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    assert_eq!((l.mode, l.query.as_str(), l.selected, l.top, l.catalog.is_none()), (Mode::All, "", 0, 0, true));
    assert_eq!(sources(&l), [Source::Window(0), Source::Window(1), Source::Window(2)]);
    assert!(!l.is_script());
}

/// The bug of 2026-10-05: the catalog arrives after the first window has been
/// auto-selected; without this fix, identity-preserving reselection kept that window
/// selected even though apps now rank above it, so a hasty Enter focused a window instead
/// of launching the most-used app.
#[test]
fn a_catalog_arriving_before_the_user_moves_selects_the_top_result() {
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(0)), "windows only: the first one");
    l.set_catalog(catalog());
    assert_eq!(
        l.selected_hit().map(|h| h.source),
        Some(Source::App(1)),
        "apps now rank above windows (empty query): the top one, not the window kept by identity"
    );
}

#[test]
fn once_the_user_has_moved_a_catalog_update_keeps_the_chosen_item() {
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    assert_eq!(l.key(Key::Down), Outcome::Redraw);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(1)));
    l.set_catalog(catalog());
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(1)), "moved already: kept by identity, as before");
}

/// A click is navigation too, like the keys `move_by` covers (2026-10-05's rule):
/// only `query_changed` (typing, a mode switch) resets to the top, never a click.
#[test]
fn clicking_counts_as_moved_a_catalog_update_keeps_the_chosen_item() {
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    l.click(1, false);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(1)));
    assert!(l.moved, "a click counts as navigation");
    l.set_catalog(catalog());
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(1)), "clicked already: kept by identity");
}

/// Typing never counts as moved, even when it reshuffles the results (2026-10-05):
/// only navigation does. Discriminates from "kept by identity" by giving the selected
/// window a new, non-zero position after the update: a third window sorts before it once
/// it arrives, so "kept by identity" and "back to the top" name different windows.
#[test]
fn typing_does_not_count_as_moved_an_update_still_lands_on_top() {
    let w1 = window("a", "term", "abc-zzz");
    let w2 = window("b", "term", "abc-aaa");
    let mut l = Launcher::for_apps(LauncherConfig::default(), vec![w1.clone(), w2.clone()], History::empty(200), NOW);
    typed(&mut l, "abc");
    assert_eq!(sources(&l), [Source::Window(1), Source::Window(0)], "abc-aaa ranks first by name");
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(1)), "typing selects the new top result");
    let w3 = window("c", "term", "abc-000");
    l.set_windows(vec![w1, w2, w3]);
    assert_eq!(sources(&l), [Source::Window(2), Source::Window(1), Source::Window(0)], "abc-000 now ranks first");
    assert_eq!(
        l.selected_hit().map(|h| h.source),
        Some(Source::Window(2)),
        "typing never counts as moved: the update lands back on the new top, not kept by identity"
    );
}

/// Down pins a window; then typing refilters — which resets `moved` — so a catalog that
/// arrives afterwards lands back on top, not on the window the Down had pinned.
#[test]
fn down_then_typing_then_a_catalog_update_gives_the_top_result() {
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    assert_eq!(l.key(Key::Down), Outcome::Redraw);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(1)), "Down pins the second window");
    typed(&mut l, "f");
    l.set_catalog(catalog());
    assert_eq!(l.selected, 0, "typing reset `moved`: the catalog update lands on top, not on the pinned window");
}

/// `move_by` must not set `moved` on an empty list: there is nothing to navigate to, and a
/// stray `moved = true` would make the very next catalog update keep a now-meaningless
/// selection index instead of landing on top.
#[test]
fn up_on_an_empty_list_does_not_set_moved() {
    let mut l = Launcher::for_apps(LauncherConfig::default(), vec![], History::empty(200), NOW);
    assert!(l.results.is_empty(), "no windows, no catalog yet");
    assert_eq!(l.key(Key::Up), Outcome::Nothing);
    assert!(!l.moved, "nothing to navigate: moved must stay false");
}

/// A clamped move (already at the top or bottom result: `to == self.selected`) is a no-op
/// and must not set `moved` either, or a later catalog/window update would wrongly treat a
/// boundary bump as a deliberate choice and stop landing on top (2026-10-05).
#[test]
fn a_clamped_move_at_an_edge_does_not_set_moved() {
    let mut l = launcher(); // 3 apps + 3 windows, selection starts on top
    assert_eq!(l.key(Key::Up), Outcome::Nothing, "already at the top: no wrap");
    assert!(!l.moved, "a clamped move changed nothing: must not count as navigation");
    assert_eq!(l.key(Key::End), Outcome::Redraw, "End actually moves the selection");
    assert!(l.moved, "a real move: counts");
    assert_eq!(l.key(Key::Down), Outcome::Nothing, "already at the bottom: no wrap");
}

#[test]
fn typing_filters_resets_the_selection_and_clears_the_message() {
    let mut l = launcher();
    l.key(Key::Down);
    l.show_message("firefox: not found".into());
    assert_eq!(l.key(Key::Char('f')), Outcome::Redraw);
    assert_eq!((l.query.as_str(), l.selected, l.message.as_deref()), ("f", 0, None));
    typed(&mut l, "iref");
    assert_eq!(sources(&l), [Source::Window(1), Source::App(0)], "the Firefox window, then the app");
    assert_eq!(l.key(Key::Backspace), Outcome::Redraw);
    assert_eq!(l.query, "fire");
}

#[test]
fn prefixes_switch_mode_and_backspace_returns_to_all() {
    let mut l = launcher();
    assert_eq!(l.key(Key::Char('>')), Outcome::Redraw);
    assert_eq!((l.mode, l.query.as_str()), (Mode::Commands, ""), "the prefix is consumed");
    assert_eq!(sources(&l), [Source::Command(0), Source::Command(1), Source::Command(2)]);
    typed(&mut l, ">");
    assert_eq!((l.mode, l.query.as_str()), (Mode::Commands, ">"), "no second jump: a plain character now");
    l.key(Key::Backspace);
    assert_eq!(l.key(Key::Backspace), Outcome::Redraw);
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, ""), "Backspace on the empty query after a prefix: back to All");
    assert_eq!(l.key(Key::Backspace), Outcome::Nothing, "already in All");
    typed(&mut l, "w");
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, "w"), "\"w\" alone filters");
    typed(&mut l, " ");
    assert_eq!((l.mode, l.query.as_str()), (Mode::Windows, ""), "\"w \" jumps to Windows");
    l.key(Key::Backspace);
    l.key(Key::Tab);
    assert_eq!(l.mode, Mode::Apps);
    typed(&mut l, ">");
    assert_eq!((l.mode, l.query.as_str()), (Mode::Apps, ">"), "prefixes only jump from All");
}

#[test]
fn configured_prefixes_are_honoured() {
    let mut config = LauncherConfig::default();
    config.prefixes.commands = "!".into();
    config.prefixes.windows = ",".into();
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    typed(&mut l, ">");
    assert_eq!((l.mode, l.query.as_str()), (Mode::All, ">"), "the default prefix is no longer one");
    l.key(Key::CtrlU);
    typed(&mut l, "!");
    assert_eq!(l.mode, Mode::Commands);
    l.key(Key::Backspace);
    typed(&mut l, ",");
    assert_eq!(l.mode, Mode::Windows);
}

#[test]
fn ctrl_u_clears_and_ctrl_w_deletes_the_last_word() {
    let mut l = launcher();
    assert_eq!(l.key(Key::CtrlU), Outcome::Nothing, "already empty");
    assert_eq!(l.key(Key::CtrlW), Outcome::Nothing);
    typed(&mut l, "git log  ");
    assert_eq!(l.key(Key::CtrlW), Outcome::Redraw);
    assert_eq!(l.query, "git ");
    l.key(Key::CtrlW);
    assert_eq!(l.query, "");
    typed(&mut l, "abc");
    assert_eq!(l.key(Key::CtrlU), Outcome::Redraw);
    assert_eq!(l.query, "");
}

#[test]
fn moving_clamps_without_wrapping_and_keeps_the_selection_visible() {
    let mut l = launcher(); // 3 apps + 3 windows
    l.key(Key::Home); // already the top result since the catalog arrived untouched; explicit here
    l.set_list_rows(2);
    assert_eq!(l.key(Key::Up), Outcome::Nothing, "no wrap at the top");
    assert_eq!(l.key(Key::CtrlP), Outcome::Nothing);
    assert_eq!(l.key(Key::Down), Outcome::Redraw);
    assert_eq!((l.selected, l.top), (1, 0));
    l.key(Key::CtrlN);
    assert_eq!((l.selected, l.top), (2, 1), "scrolled to keep it visible");
    l.key(Key::PageDown);
    assert_eq!((l.selected, l.top), (4, 3), "a page = the visible rows");
    l.key(Key::End);
    assert_eq!((l.selected, l.top), (5, 4));
    assert_eq!(l.key(Key::Down), Outcome::Nothing, "no wrap at the bottom");
    l.key(Key::PageUp);
    assert_eq!((l.selected, l.top), (3, 3));
    l.key(Key::Home);
    assert_eq!((l.selected, l.top), (0, 0));
    l.set_list_rows(10);
    l.key(Key::End);
    assert_eq!((l.selected, l.top), (5, 0), "everything fits: never scrolled");
}

#[test]
fn tab_cycles_modes_but_not_in_script_mode() {
    let mut l = launcher();
    typed(&mut l, "f");
    assert_eq!(l.key(Key::Tab), Outcome::Redraw);
    assert_eq!((l.mode, l.query.as_str()), (Mode::Apps, "f"), "the query is kept");
    assert_eq!(sources(&l), [Source::App(1), Source::App(0), Source::App(2)]);
    l.key(Key::Tab);
    l.key(Key::Tab);
    assert_eq!(l.mode, Mode::Commands);
    l.key(Key::Tab);
    assert_eq!(l.mode, Mode::All, "wraps");
    l.key(Key::BackTab);
    assert_eq!(l.mode, Mode::Commands);
    let mut s = Launcher::for_script(LauncherConfig::default(), None, vec!["a".into()]);
    assert_eq!(s.key(Key::Tab), Outcome::Nothing);
    assert_eq!(s.key(Key::BackTab), Outcome::Nothing);
    assert_eq!(s.click_mode(Mode::Apps), Outcome::Nothing);
    assert!(s.is_script());
}

/// The preview panel always shows the selected result now (view.rs, 2026-10-05): the
/// `Left`/`Right` keys that used to toggle it are ignored — recognised by `keys.rs`, but
/// a no-op here, not even clearing a message (plain `Outcome::Nothing`, nothing to redraw).
#[test]
fn left_and_right_are_ignored() {
    let mut l = launcher();
    l.key(Key::Tab); // Apps: Files, Firefox, Fish
    assert_eq!(l.key(Key::Right), Outcome::Nothing);
    assert_eq!(l.key(Key::Left), Outcome::Nothing);
    assert_eq!((l.mode, l.selected), (Mode::Apps, 0), "nothing else changed either");
}

#[test]
fn enter_activates_the_selection_escape_closes() {
    let mut l = launcher();
    // Default selection: the top result once the catalog arrives — the window
    // `for_apps` started on is not kept, since the user hasn't moved yet
    // (2026-10-05, "until the user moves, the selection stays on top").
    assert_eq!(l.key(Key::Enter), activate(Activation::App { index: 1, action: None }), "Files, the top app");
    l.key(Key::Tab);
    l.key(Key::Down);
    assert_eq!(l.key(Key::Enter), activate(Activation::App { index: 0, action: None }), "Firefox, main Exec");
    typed(&mut l, "zzz");
    assert_eq!(l.key(Key::Enter), Outcome::Nothing, "nothing selected");
    assert_eq!(l.key(Key::Escape), Outcome::Close);
}

#[test]
fn ctrl_enter_runs_the_first_desktop_action_else_the_main_exec() {
    let mut l = launcher();
    l.key(Key::Tab);
    l.key(Key::Down); // Firefox
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::App { index: 0, action: Some(0) }));
    l.key(Key::Up); // Files
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::App { index: 1, action: None }));
    l.key(Key::BackTab); // All: apps first (default selection), then windows
    for _ in 0..3 {
        l.key(Key::Down); // past the 3 apps
    }
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::Window(0)), "a window: as Enter");
}

#[test]
fn commands_mode_runs_the_selection_or_the_typed_text() {
    let mut l = launcher();
    typed(&mut l, ">");
    typed(&mut l, "hto");
    assert_eq!(l.key(Key::Enter), activate(Activation::Command { index: 2, terminal: true }), "htop selected: held in the terminal");
    assert_eq!(
        l.key(Key::CtrlEnter),
        activate(Activation::Command { index: 2, terminal: false }),
        "detached, by itself: never through sh -c"
    );
    typed(&mut l, "p -d 5");
    assert_eq!(
        l.key(Key::Enter),
        activate(Activation::Shell { text: "htop -d 5".into(), terminal: true }),
        "whitespace: the shell, held in the terminal"
    );
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::Shell { text: "htop -d 5".into(), terminal: false }), "detached");
    l.key(Key::CtrlU);
    typed(&mut l, "zzzz");
    assert_eq!(l.key(Key::Enter), activate(Activation::Shell { text: "zzzz".into(), terminal: true }), "no selection: the shell");
    l.key(Key::CtrlU);
    typed(&mut l, "  ");
    assert_eq!(l.key(Key::Enter), Outcome::Nothing, "blanks only: nothing to run");
    let mut empty = Launcher::for_apps(LauncherConfig::default(), vec![], History::empty(200), NOW);
    typed(&mut empty, ">");
    assert_eq!(empty.key(Key::Enter), Outcome::Nothing, "empty query, no catalog: nothing to run");
}

#[test]
fn script_mode_enter_returns_typed_text_when_nothing_matches() {
    let mut s = Launcher::for_script(LauncherConfig::default(), Some("Target".into()), vec![]);
    assert_eq!(s.prompt.as_deref(), Some("Target"));
    assert_eq!(s.key(Key::Enter), Outcome::Nothing, "no line, nothing typed");
    typed(&mut s, "10.0.0.1");
    assert_eq!(s.key(Key::Enter), choose("10.0.0.1", None), "free text (tools-menu's prompts)");
    let mut s = Launcher::for_script(LauncherConfig::default(), None, vec!["alpha".into(), "beta".into()]);
    assert_eq!(sources(&s), [Source::Line(0), Source::Line(1)]);
    typed(&mut s, "bet");
    assert_eq!(s.key(Key::Enter), choose("beta", Some(1)), "the matching line, as shown");
    typed(&mut s, "zzz");
    assert_eq!(s.key(Key::Enter), choose("betzzz", None), "nothing matches: the typed text");
    assert_eq!(s.key(Key::CtrlEnter), choose("betzzz", None), "Ctrl+Enter as Enter");
    assert_eq!(s.key(Key::Escape), Outcome::Close);
}

fn choose(text: &str, line: Option<usize>) -> Outcome {
    activate(Activation::Choose { text: text.into(), line })
}

/// `--index`: the line's position in the client's order survives the filtering and the
/// ranking (a fuzzy score puts "gamma" first here, at results[0]).
#[test]
fn a_chosen_line_keeps_its_position_on_stdin_through_filtering() {
    let lines = ["alpha", "beta", "gamma", "delta", "gamma"].map(String::from).to_vec();
    let mut s = Launcher::for_dmenu(LauncherConfig::default(), None, lines, DmenuFlags { index: true, password: false, visible: None });
    typed(&mut s, "gam");
    assert_eq!(sources(&s), [Source::Line(2), Source::Line(4)], "the two gammas, in input order");
    assert_eq!(s.key(Key::Down), Outcome::Redraw);
    assert_eq!(s.key(Key::Enter), choose("gamma", Some(4)), "the second gamma: duplicates told apart by position");
    let mut s = Launcher::for_dmenu(LauncherConfig::default(), None, ["a b", "b"].map(String::from).to_vec(), DmenuFlags::default());
    typed(&mut s, "b");
    assert_eq!(sources(&s), [Source::Line(1), Source::Line(0)], "the exact match ranks first");
    assert_eq!(s.click(0, false), choose("b", Some(1)), "a click too: the first result is the second line");
}

/// `--visible N` (sources spec § 4): the first N lines on an empty query, every line once typing.
#[test]
fn visible_lists_the_first_lines_on_an_empty_query_and_searches_them_all() {
    let lines = ["Captures", "Affichage", "Captures · Zone", "Affichage · Luminosité · 50%"].map(String::from).to_vec();
    let flags = |index, visible| DmenuFlags { index, password: false, visible };
    let mut s = Launcher::for_dmenu(LauncherConfig::default(), None, lines.clone(), flags(false, Some(2)));
    assert_eq!(sources(&s), [Source::Line(0), Source::Line(1)]);
    typed(&mut s, "lumi");
    assert_eq!(sources(&s), [Source::Line(3)], "typing searches every line");
    s.key(Key::CtrlU);
    assert_eq!(sources(&s), [Source::Line(0), Source::Line(1)], "back to the first two");
    typed(&mut s, " ");
    assert_eq!(sources(&s), [Source::Line(0), Source::Line(1)], "a lone space is no search (no atom): still the first two");
    s.key(Key::CtrlU);
    typed(&mut s, "a");
    assert_eq!(s.results.len(), 4, "a typed query matching more than N lines lists all of them");
    let mut s = Launcher::for_dmenu(LauncherConfig::default(), None, lines.clone(), flags(true, Some(2)));
    typed(&mut s, "zone");
    assert_eq!(s.key(Key::Enter), choose("Captures · Zone", Some(2)), "its position among all the lines");
    let none = Launcher::for_dmenu(LauncherConfig::default(), None, lines.clone(), flags(false, Some(0)));
    assert!(none.results.is_empty(), "--visible 0: nothing until typing");
    let all = Launcher::for_dmenu(LauncherConfig::default(), None, lines, flags(false, Some(99)));
    assert_eq!(all.results.len(), 4, "N above the line count: every line");
}

/// `--password`: no list (the lines are dropped at open, nothing is ranked or shown), Enter
/// returns the typed text as is, spaces included; nothing typed → nothing.
#[test]
fn password_mode_has_no_list_and_returns_the_typed_text() {
    let lines = ["alpha", "beta"].map(String::from).to_vec();
    let mut s = Launcher::for_dmenu(
        LauncherConfig::default(),
        Some("Key: ".into()),
        lines,
        DmenuFlags { index: false, password: true, visible: None },
    );
    assert!(s.password && s.lines.is_empty() && s.results.is_empty(), "no list");
    assert_eq!(s.key(Key::Enter), Outcome::Nothing, "nothing typed");
    typed(&mut s, " al pha");
    assert!(s.results.is_empty(), "nothing filtered either: {:?}", sources(&s));
    assert_eq!(s.ranker.preparations(), 0, "never ranked: no nucleo pattern built from the secret");
    assert_eq!(s.key(Key::Down), Outcome::Nothing);
    assert_eq!(s.key(Key::Enter), choose(" al pha", None));
    assert_eq!(s.key(Key::CtrlEnter), choose(" al pha", None));
}

#[test]
fn catalog_arriving_after_typing_refilters_and_clamps() {
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    typed(&mut l, "fi");
    assert_eq!(sources(&l), [Source::Window(0), Source::Window(1)], "windows only before the scan");
    assert_eq!(l.key(Key::Down), Outcome::Redraw);
    assert_eq!(l.key(Key::Down), Outcome::Nothing, "two results: clamped");
    l.key(Key::Up); // back on "fish shell"
    l.set_catalog(catalog());
    assert_eq!(l.query, "fi", "typing kept");
    assert_eq!(l.results.len(), 5, "apps refiltered: {:?}", sources(&l));
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(0)), "the selected window is still selected");
    assert_eq!(l.selected, 4, "now below the apps that outrank it: {:?}", sources(&l));
    l.key(Key::End);
    let last = l.results.len() - 1;
    assert_eq!(l.selected, last);
    l.set_windows(vec![]);
    assert!(l.selected < l.results.len(), "selection in range after the list shrank");
    l.set_list_rows(1);
    l.key(Key::End);
    l.set_catalog(Catalog::default());
    assert_eq!((l.selected, l.top), (0, 0), "nothing left: back to the top");
    assert!(l.results.is_empty());
}

#[test]
fn a_window_list_change_keeps_the_selected_window() {
    let mut l = launcher();
    l.key(Key::Tab);
    l.key(Key::Tab); // Windows mode: only windows listed
    l.key(Key::Down);
    l.key(Key::Down); // Window(2), "notes"
    let mut w = windows();
    w[0].title = "fish — vim".into();
    l.set_windows(w);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(2)));
}

/// The selection is kept by identity (`Toplevel::selector`), not by its old index: a
/// window opening before the selected one shifts every later window's index by one, which
/// an index-based `Source::Window(2)` comparison would silently land on the wrong window.
#[test]
fn a_window_inserted_before_the_selected_one_keeps_it_selected() {
    let mut l = launcher();
    l.key(Key::Tab);
    l.key(Key::Tab); // Windows mode: only windows listed
    l.key(Key::Down);
    l.key(Key::Down); // Window(2), "notes" (selector "id:3")
    let mut w = windows();
    w.insert(0, window("id:0", "foot", "zzz-new"));
    l.set_windows(w);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(3)), "shifted by the insertion, but still \"notes\"");
}

/// `set_windows` must call `ranker.forget_windows()`, not the full `forget()`: a title
/// edit with the window count unchanged still has to refresh `window_name_rank` (the tie
/// break below), without re-walking the whole history for the catalog's own frecency.
/// Both windows share a class so their class score ties exactly: only the name decides.
#[test]
fn a_title_change_refreshes_the_name_order_with_the_same_window_count() {
    let mut l = Launcher::for_apps(
        LauncherConfig::default(),
        vec![window("id:1", "term", "bbb-window"), window("id:2", "term", "aaa-window")],
        History::empty(200),
        NOW,
    );
    typed(&mut l, "term");
    assert_eq!(sources(&l), [Source::Window(1), Source::Window(0)], "aaa-window ranks first by name");
    let mut w = l.windows.clone();
    w[1].title = "zzz-window".into();
    l.set_windows(w);
    assert_eq!(sources(&l), [Source::Window(0), Source::Window(1)], "same count: the renamed window now sorts after its new title");
}

#[test]
fn a_window_list_change_refreshes_what_each_window_belongs_to() {
    let mut l = launcher();
    typed(&mut l, "files");
    assert_eq!(sources(&l), [Source::App(1)], "no window of Files yet");
    let mut w = windows();
    w[1].class = "org.gnome.Nautilus".into(); // same length, another app
    l.set_windows(w);
    assert_eq!(sources(&l), [Source::Window(1), Source::App(1)], "the window now belongs to Files");
    let mut c = catalog();
    c.apps[1].name = "Fichiers".into();
    c.apps[1].exec = "nautilus".into();
    l.set_catalog(c);
    assert!(sources(&l).is_empty(), "a new catalog, same length: matched again ({:?})", sources(&l));
}

#[test]
fn clicks_and_wheel() {
    let mut l = launcher();
    assert_eq!(l.click(1, false), activate(Activation::App { index: 0, action: None }), "result 1: Firefox (All, empty query)");
    assert_eq!(l.selected, 1);
    assert_eq!(l.click(1, true), activate(Activation::App { index: 0, action: Some(0) }), "secondary click: its action");
    assert_eq!(l.click(99, false), Outcome::Nothing, "below the last result");
    assert_eq!(l.click_mode(Mode::Commands), Outcome::Redraw);
    assert_eq!(l.mode, Mode::Commands);
    assert_eq!(l.click_mode(Mode::Commands), Outcome::Nothing, "already there");
    assert_eq!(l.click_mode(Mode::Script), Outcome::Nothing, "never into Script");
    assert_eq!(sources(&l)[0], Source::Command(0), "fd, alphabetically first");
    assert_eq!(l.click(0, false), activate(Activation::Command { index: 0, terminal: true }), "clicking a command row runs it");
    assert_eq!(l.scroll(2), Outcome::Redraw);
    assert_eq!(l.selected, 2);
    assert_eq!(l.scroll(10), Outcome::Nothing, "clamped at the last command");
    l.scroll(-10);
    assert_eq!(l.selected, 0);
}

#[test]
fn a_message_lasts_until_the_next_key() {
    let mut l = launcher();
    l.key(Key::Home); // the top result, whatever it is (ignores the default-selection quirk above)
    l.show_message("firefox: not found".into());
    assert_eq!(l.message.as_deref(), Some("firefox: not found"));
    assert_eq!(l.key(Key::Up), Outcome::Redraw, "nothing moved, but the message goes");
    assert_eq!(l.message, None);
    assert_eq!(l.key(Key::Up), Outcome::Nothing);
}

/// `click`, like `key`, must turn a `Nothing` into a `Redraw` when it clears a message:
/// otherwise the error line would linger on screen until some other key is pressed, even
/// though the click itself already invalidated it. A blank query in Commands mode gives a
/// click that selects a real row yet still activates nothing (`primary`'s own `Shell`
/// branch, blank text: Enter activates nothing) — the one way to reach `Nothing`
/// from `click` without the index guard itself already rejecting it.
#[test]
fn clicking_clears_a_message_and_redraws_even_when_nothing_else_changes() {
    let mut l = launcher();
    typed(&mut l, ">"); // Commands mode
    typed(&mut l, "  "); // blank query: still lists every command (the empty-query order)
    l.show_message("oops".into());
    assert_eq!(l.click(0, false), Outcome::Redraw, "nothing to run on a blank query, but the message still clears");
    assert_eq!(l.message, None);
}

fn entries(lines: &[&str]) -> Entries {
    let outils = SourceConfig {
        name: "Outils".into(),
        list: vec!["tools-menu".into(), "--list".into()],
        run: vec!["tools-menu".into(), "{line}".into()],
    };
    Entries { sources: vec![outils], items: lines.iter().map(|l| SourceEntry::new(0, l)).collect() }
}

#[test]
fn entries_show_on_a_typed_query_and_enter_runs_them() {
    let mut l = launcher();
    l.set_entries(entries(&["Apparence · Schéma couleurs · Vibrant", "Captures · Zone", "Verrouiller"]));
    assert!(!sources(&l).iter().any(|s| matches!(s, Source::Entry(_))), "empty query: today's list");
    typed(&mut l, "vibrant");
    assert_eq!(sources(&l), [Source::Entry(0)]);
    assert_eq!(l.key(Key::Enter), activate(Activation::Entry(0)));
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::Entry(0)), "Ctrl+Enter does the same");
    l.key(Key::CtrlU);
    typed(&mut l, ">vibrant");
    assert!(sources(&l).is_empty(), "behind a prefix: never");
}

#[test]
fn entries_arriving_after_typing_join_the_results() {
    let mut l = launcher();
    typed(&mut l, "zone");
    assert!(sources(&l).is_empty());
    l.set_entries(entries(&["Captures · Zone"]));
    assert_eq!(sources(&l), [Source::Entry(0)]);
}

#[test]
fn a_moved_selection_stays_on_its_entry_when_entries_arrive_again() {
    let mut l = launcher();
    l.set_entries(entries(&["Zone A", "Zone B"]));
    typed(&mut l, "zone");
    assert_eq!(sources(&l), [Source::Entry(0), Source::Entry(1)]);
    l.key(Key::Down);
    l.set_entries(entries(&["Zone B", "Zone A"]));
    assert_eq!(sources(&l), [Source::Entry(1), Source::Entry(0)], "ranked again: \"Zone A\" (now index 1) first");
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Entry(0)), "still \"Zone B\", now index 0");
}

fn with_calculator_app() -> Launcher {
    let mut l = Launcher::for_apps(LauncherConfig::default(), vec![], History::empty(200), NOW);
    l.set_catalog(Catalog { apps: vec![app("calc.desktop", "Calculator 2+2", &[])], commands: vec![] });
    l
}

#[test]
fn a_math_query_puts_the_calculator_first() {
    let mut l = with_calculator_app();
    typed(&mut l, "2+2");
    assert_eq!(l.results[0].source, Source::Calc);
    assert_eq!(l.calc, Some(Ok(Answer { text: "4".into(), bases: None })));
    assert!(l.results.iter().any(|h| h.source == Source::App(0)), "the app is still listed after it: {:?}", sources(&l));
    assert_eq!(l.selected, 0);
}

#[test]
fn a_plain_word_or_number_shows_no_calculator_row() {
    for query in ["firefox", "2048", "e", "7zip", "Steam 2", "Doom 2", "gimp-2.10", "x264", "-5"] {
        let mut l = launcher();
        typed(&mut l, query);
        assert!(!sources(&l).contains(&Source::Calc), "{query}: {:?}", sources(&l));
        assert_eq!(l.calc, None, "{query}");
    }
}

#[test]
fn enter_on_the_calculator_row_copies_the_result() {
    let mut l = launcher();
    typed(&mut l, "2+2");
    assert_eq!(l.key(Key::Enter), activate(Activation::Copy { text: "4".into() }));
}

#[test]
fn ctrl_enter_copies_the_expression_and_the_result() {
    let mut l = launcher();
    typed(&mut l, " 2+2 ");
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::Copy { text: "2+2 = 4".into() }));
}

#[test]
fn an_error_row_activates_nothing() {
    let mut l = launcher();
    typed(&mut l, "1/0");
    assert_eq!(l.results[0].source, Source::Calc);
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Nothing);
}

#[test]
fn an_empty_copy_hides_the_action() {
    let config = LauncherConfig { copy: vec![], ..LauncherConfig::default() };
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    l.set_catalog(catalog());
    typed(&mut l, "2+2");
    assert_eq!(l.results[0].source, Source::Calc);
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Nothing);
}

#[test]
fn no_calculator_in_the_commands_mode() {
    let mut l = launcher();
    typed(&mut l, ">");
    assert_eq!(l.mode, Mode::Commands);
    typed(&mut l, "2+2");
    assert!(!sources(&l).contains(&Source::Calc));
    assert_eq!(l.calc, None);
}

#[test]
fn the_calculator_mode_shows_only_the_result() {
    let mut l = Launcher::for_calc(LauncherConfig::default());
    assert!(l.is_bare());
    assert!(l.results.is_empty());
    typed(&mut l, "pi");
    assert_eq!(l.results.len(), 1);
    assert_eq!(l.calc, Some(Ok(Answer { text: "3.14159265359".into(), bases: None })));
    l.set_catalog(catalog());
    assert_eq!(l.results.len(), 1);
    assert_eq!(l.key(Key::Tab), Outcome::Nothing);
    assert_eq!(l.key(Key::BackTab), Outcome::Nothing);
    assert_eq!(l.mode, Mode::Calc);
    assert_eq!(l.click_mode(Mode::Apps), Outcome::Nothing);
    assert_eq!(l.mode, Mode::Calc);
    // No prefix jump either.
    l.key(Key::CtrlU);
    typed(&mut l, ">");
    assert_eq!(l.mode, Mode::Calc);
    assert_eq!(l.key(Key::Enter), Outcome::Nothing, "'>' is no expression");
}

#[test]
fn the_calculator_mode_copies_on_enter() {
    let mut l = Launcher::for_calc(LauncherConfig::default());
    typed(&mut l, "2048");
    assert_eq!(l.key(Key::Enter), activate(Activation::Copy { text: "2048".into() }), "no looks_like_math filter in the mode");
}

#[test]
fn a_question_mark_shows_one_search_row() {
    let mut l = launcher();
    typed(&mut l, "? rust");
    assert!(matches!(l.results[..], [Hit { source: Source::Search, .. }]), "{:?}", l.results);
    assert_eq!(l.calc, None);
    let open = activate(Activation::Open { url: "https://duckduckgo.com/?q=rust".into() });
    assert_eq!(l.key(Key::Enter), open);
    assert_eq!(l.key(Key::CtrlEnter), open);
    assert_eq!(l.prefixed().map(|(_, t)| t), Some("rust"));
}

#[test]
fn a_bare_prefix_lists_nothing_and_enter_does_nothing() {
    for query in ["?", "? ", ":", "$"] {
        let mut l = launcher();
        typed(&mut l, query);
        assert!(l.results.is_empty(), "{query:?}: {:?}", l.results);
        assert_eq!(l.key(Key::Enter), Outcome::Nothing, "{query:?}");
        assert_eq!(l.key(Key::CtrlEnter), Outcome::Nothing, "{query:?}");
    }
}

#[test]
fn search_disabled_searches_the_text() {
    for config in [
        LauncherConfig { search: String::new(), ..LauncherConfig::default() },
        LauncherConfig { open: vec![], ..LauncherConfig::default() },
    ] {
        let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
        l.set_catalog(catalog());
        typed(&mut l, "?rust");
        assert!(!sources(&l).contains(&Source::Search), "{:?}", sources(&l));
        assert_eq!(l.prefixed(), None);
    }
}

#[test]
fn prefixes_only_in_all() {
    let mut l = launcher();
    l.key(Key::Tab);
    assert_eq!(l.mode, Mode::Apps);
    typed(&mut l, "? rust");
    assert!(!sources(&l).contains(&Source::Search));
    assert_eq!(l.prefixed(), None);
    let mut c = Launcher::for_calc(LauncherConfig::default());
    typed(&mut c, "? rust");
    assert!(!sources(&c).contains(&Source::Search));
    assert_eq!(c.prefixed(), None);
    let mut s = Launcher::for_script(LauncherConfig::default(), None, vec!["? rust".into()]);
    typed(&mut s, "? rust");
    assert_eq!(sources(&s), [Source::Line(0)], "still matches as a line");
    assert_eq!(s.prefixed(), None);
}

#[test]
fn a_prefix_hides_the_calculator() {
    let mut l = launcher();
    typed(&mut l, "? 2+2");
    assert_eq!(l.calc, None);
    assert_eq!(sources(&l), [Source::Search]);
    let mut l = launcher();
    typed(&mut l, "$ 2+2");
    assert_eq!(l.calc, None);
}

fn emoji_launcher(rows: u16) -> Launcher {
    let config = LauncherConfig { rows, ..LauncherConfig::default() };
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    l.set_catalog(catalog());
    l
}

#[test]
fn a_colon_lists_emoji_and_copies() {
    let mut l = emoji_launcher(5);
    typed(&mut l, ":red heart");
    assert!(!l.results.is_empty() && l.results.len() <= 5, "{:?}", sources(&l));
    assert!(l.results.iter().all(|h| matches!(h.source, Source::Emoji(_))), "{:?}", sources(&l));
    let Source::Emoji(offset) = l.results[0].source else { unreachable!() };
    assert_eq!(emoji::at(offset).map(|(e, _)| e), Some("\u{2764}\u{fe0f}"));
    assert_eq!(l.key(Key::Enter), activate(Activation::Copy { text: "\u{2764}\u{fe0f}".into() }));
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::Copy { text: "red heart".into() }));
}

#[test]
fn a_zwj_emoji_copies_the_whole_sequence() {
    let mut l = emoji_launcher(5);
    typed(&mut l, ":family: man, woman, girl");
    assert_eq!(l.key(Key::Enter), activate(Activation::Copy { text: "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".into() }));
}

#[test]
fn an_empty_copy_copies_no_emoji() {
    let config = LauncherConfig { copy: vec![], ..LauncherConfig::default() };
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    typed(&mut l, ":heart");
    assert!(matches!(l.results[0].source, Source::Emoji(_)));
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Nothing);
}

#[test]
fn a_bare_colon_lists_nothing() {
    let mut l = emoji_launcher(5);
    typed(&mut l, ":");
    assert!(l.results.is_empty());
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
}

#[test]
fn a_configured_mode_prefix_wins_over_a_query_prefix() {
    let config = LauncherConfig::from_text("[prefixes]\ncommands = \"$\"\n").unwrap();
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    l.set_catalog(catalog());
    typed(&mut l, "$");
    assert_eq!((l.mode, l.query.as_str()), (Mode::Commands, ""), "the mode, not the shell prefix");
    assert_eq!(l.prefixed(), None);
    // Anything else typed first stays plain text for the `$` prefix.
    let mut l = launcher();
    typed(&mut l, "$");
    assert_eq!((l.mode, l.prefixed()), (Mode::All, Some((Prefix::Shell, ""))));
}

#[test]
fn a_password_dmenu_never_reads_a_prefix() {
    let flags = DmenuFlags { password: true, ..DmenuFlags::default() };
    let mut l = Launcher::for_dmenu(LauncherConfig::default(), None, vec![], flags);
    typed(&mut l, "? x");
    assert_eq!(l.query, "? x");
    assert_eq!(l.prefixed(), None);
    assert!(l.results.is_empty());
    let mut l = Launcher::for_dmenu(LauncherConfig::default(), None, vec![], flags);
    typed(&mut l, "$ x");
    assert_eq!(l.prefixed(), None);
    assert!(l.results.is_empty());
    assert!(!matches!(l.key(Key::Enter), Outcome::Activate(Activation::Shell { .. })), "a password prompt never runs `$`");
}

#[test]
fn a_dollar_shows_one_shell_row() {
    let mut l = launcher();
    typed(&mut l, "$ sleep 5 && notify-send hi");
    assert_eq!(sources(&l), [Source::Shell]);
    let text = "sleep 5 && notify-send hi".to_string();
    assert_eq!(l.key(Key::Enter), activate(Activation::Shell { text: text.clone(), terminal: false }));
    assert_eq!(l.key(Key::CtrlEnter), activate(Activation::Shell { text: text.clone(), terminal: true }));
    assert_eq!(l.click(0, false), activate(Activation::Shell { text: text.clone(), terminal: false }));
    assert_eq!(l.click(0, true), activate(Activation::Shell { text, terminal: true }));
    // Nothing after the prefix: no row, nothing to run.
    let mut l = launcher();
    typed(&mut l, "$ ");
    assert!(l.results.is_empty());
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
}

#[test]
fn a_configured_dollar_prefix_still_enters_commands() {
    let config = LauncherConfig::from_text("[prefixes]\ncommands = \"$\"\n").unwrap();
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    l.set_catalog(catalog());
    typed(&mut l, "$");
    assert_eq!(l.mode, Mode::Commands);
    assert!(!l.results.iter().any(|h| h.source == Source::Shell));
}

fn focused(mut w: Toplevel) -> Toplevel {
    w.focused = true;
    w
}

/// The backends' order: the previous window first, the focused one last.
fn three() -> Vec<Toplevel> {
    vec![window("id:2", "foot", "prev"), window("id:1", "foot", "older"), focused(window("id:3", "foot", "cur"))]
}

fn switcher() -> Launcher {
    Launcher::for_windows(LauncherConfig::default(), three())
}

#[test]
fn the_switcher_opens_on_the_previous_window() {
    let mut l = switcher();
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(0)));
    assert_eq!(l.key(Key::Enter), Outcome::Activate(Activation::Window(0)));
    assert_eq!(l.mode, Mode::Windows);
    assert!(l.is_switcher());
    assert!(l.catalog.is_none());
}

#[test]
fn step_moves_down_and_wraps() {
    let mut l = switcher();
    for want in [1, 2, 0] {
        assert_eq!(l.step(1), Outcome::Redraw);
        assert_eq!(l.selected, want);
    }
    assert_eq!(l.step(-1), Outcome::Redraw);
    assert_eq!(l.selected, 2);
}

#[test]
fn tab_steps_in_the_switcher_and_never_switches_mode() {
    let mut l = switcher();
    assert_eq!(l.key(Key::Tab), Outcome::Redraw);
    assert_eq!(l.selected, 1);
    assert_eq!(l.key(Key::BackTab), Outcome::Redraw);
    assert_eq!(l.selected, 0);
    assert_eq!(l.click_mode(Mode::All), Outcome::Nothing);
    assert_eq!(l.mode, Mode::Windows);
    assert!(l.is_switcher());
}

#[test]
fn stepping_wraps_and_survives_a_window_update() {
    let mut l = switcher();
    let mut w = vec![window("id:4", "foot", "new")];
    w.extend(three());
    l.set_windows(w);
    assert_eq!(l.selected, 0, "before any step, the top");
    l.step(1);
    let selector = |l: &Launcher| match l.selected_hit().map(|h| h.source) {
        Some(Source::Window(i)) => l.windows[i].selector.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(selector(&l), "id:2");
    l.step(1);
    assert_eq!(selector(&l), "id:1");
    let mut w = three();
    w.reverse();
    l.set_windows(w);
    assert_eq!(selector(&l), "id:1", "kept by identity");
    l.set_windows(vec![window("id:2", "foot", "prev"), focused(window("id:3", "foot", "cur"))]);
    assert_eq!(l.selected, 0, "closed: back to the top");
}

#[test]
fn a_switcher_with_no_window_or_one() {
    let mut l = Launcher::for_windows(LauncherConfig::default(), vec![]);
    assert_eq!(l.step(1), Outcome::Nothing);
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
    let mut l = Launcher::for_windows(LauncherConfig::default(), vec![focused(window("id:3", "foot", "cur"))]);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Window(0)));
    assert_eq!(l.step(1), Outcome::Nothing);
}

#[test]
fn step_works_on_the_filtered_results_not_on_every_window() {
    let w = vec![
        window("id:1", "term", "alpha foo"),
        window("id:2", "term", "bravo bar"),
        window("id:3", "term", "charlie foo"),
        window("id:4", "term", "delta bar"),
    ];
    let mut l = Launcher::for_windows(LauncherConfig::default(), w);
    typed(&mut l, "foo");
    assert_eq!(l.results.len(), 2, "two windows match");
    let at = |l: &Launcher| match l.selected_hit().map(|h| h.source) {
        Some(Source::Window(i)) => l.windows[i].selector.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(at(&l), "id:1");
    assert_eq!(l.step(1), Outcome::Redraw);
    assert_eq!(at(&l), "id:3");
    assert_eq!(l.step(1), Outcome::Redraw);
    assert_eq!(at(&l), "id:1", "wraps over the two matches, never visiting id:2 or id:4");
    assert_eq!(l.step(-1), Outcome::Redraw);
    assert_eq!(at(&l), "id:3");
}

#[test]
fn a_typed_query_filters_then_steps() {
    let mut l = switcher();
    typed(&mut l, "old");
    assert_eq!(l.results.len(), 1);
    assert_eq!(l.step(1), Outcome::Nothing);
}

// ---- the clipboard history (clipboard spec, task 5) ----

use crate::clipboard::history::{ClipRow, MAX_PINS};
use std::time::{Duration, Instant};

fn clip(id: u64, text: &str, pinned: bool) -> ClipRow {
    ClipRow { id, preview: text.into(), first: text.lines().next().unwrap_or_default().into(), more_lines: 0, pinned }
}

fn rows3() -> Vec<ClipRow> {
    vec![clip(10, "alpha", true), clip(11, "buy milk", false), clip(12, "token 1234", false)]
}

fn clipboard_box() -> Launcher {
    Launcher::for_clipboard(LauncherConfig::default(), rows3())
}

/// The All launcher with the history on and `rows3`, the query a bare `"`.
fn clip_prefix_box() -> Launcher {
    let mut l = launcher();
    l.clipboard = true;
    l.set_clips(rows3());
    typed(&mut l, "\"");
    l
}

#[test]
fn a_bare_quote_lists_everything_pinned_first_then_clear() {
    for l in [clipboard_box(), clip_prefix_box()] {
        assert_eq!(sources(&l), [Source::Clip(0), Source::Clip(1), Source::Clip(2), Source::ClipClear]);
    }
}

#[test]
fn clear_is_offered_only_with_an_unpinned_row_and_no_filter() {
    let l = Launcher::for_clipboard(LauncherConfig::default(), vec![clip(1, "a", true)]);
    assert_eq!(sources(&l), [Source::Clip(0)], "only pins: nothing to clear");
    let l = Launcher::for_clipboard(LauncherConfig::default(), vec![]);
    assert!(l.results.is_empty());
}

#[test]
fn typing_filters_by_preview() {
    let mut l = clipboard_box();
    typed(&mut l, "milk");
    assert_eq!(sources(&l), [Source::Clip(1)], "no Clear row with a filter");
    let mut l = clip_prefix_box();
    typed(&mut l, "tok");
    assert_eq!(sources(&l), [Source::Clip(2)]);
}

#[test]
fn matching_sees_the_whole_preview_not_only_the_shown_line() {
    let mut row = clip(1, "first second line", false);
    row.first = "first".into();
    row.more_lines = 1;
    let mut l = Launcher::for_clipboard(LauncherConfig::default(), vec![row]);
    typed(&mut l, "second");
    assert_eq!(sources(&l), [Source::Clip(0)]);
}

#[test]
fn the_quote_is_plain_text_without_the_history() {
    let mut l = launcher();
    typed(&mut l, "\"");
    assert_eq!(l.prefixed(), None);
    l.clipboard = true;
    assert_eq!(l.prefixed().map(|p| p.0), Some(Prefix::Clipboard));
}

#[test]
fn a_configured_mode_prefix_wins_over_the_quote() {
    let mut config = LauncherConfig::default();
    config.prefixes.commands = "\"".into();
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    l.clipboard = true;
    typed(&mut l, "\"");
    assert_eq!((l.mode, l.query.as_str()), (Mode::Commands, ""));
}

#[test]
fn enter_activates_clip_with_its_id() {
    let mut l = clipboard_box();
    l.selected = 1;
    assert_eq!(l.key(Key::Enter), activate(Activation::Clip { id: 11 }));
    let mut l = clip_prefix_box();
    assert_eq!(l.key(Key::Enter), activate(Activation::Clip { id: 10 }));
}

#[test]
fn ctrl_enter_pins_and_stays_open() {
    let mut l = clipboard_box();
    l.selected = 1;
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Edit(ClipEdit::Pin(11)));
    assert!(pinned_by_id(&l, 11));
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Edit(ClipEdit::Pin(11)), "again: unpinned");
    assert!(!pinned_by_id(&l, 11));
    // A right click is the same gesture.
    let at = l.clips.iter().position(|c| c.id == 12).unwrap();
    assert_eq!(l.click(at, true), Outcome::Edit(ClipEdit::Pin(12)));
    assert!(pinned_by_id(&l, 12));
}

fn pinned_by_id(l: &Launcher, id: u64) -> bool {
    l.clips.iter().find(|c| c.id == id).unwrap().pinned
}

fn ids(l: &Launcher) -> Vec<u64> {
    l.clips.iter().map(|c| c.id).collect()
}

#[test]
fn a_new_pin_goes_to_the_head_of_the_pinned_group_and_keeps_the_selection() {
    let mut l = clipboard_box(); // [alpha(pinned), beta, gamma]
    l.selected = 2;
    l.key(Key::CtrlEnter);
    assert_eq!(ids(&l), [12, 10, 11], "most recently pinned first, as the history orders it");
    assert_eq!(l.selected, 0);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Clip(0)));
    // Unpinning drops it behind the pinned group, the selection following it.
    l.key(Key::CtrlEnter);
    assert_eq!(ids(&l), [10, 12, 11]);
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::Clip(1)));
    assert!(!l.clips[1].pinned);
}

#[test]
fn an_eleventh_pin_is_refused_with_a_message() {
    let mut rows: Vec<ClipRow> = (0..MAX_PINS as u64).map(|i| clip(i, &format!("pinned {i}"), true)).collect();
    rows.push(clip(99, "loose", false));
    let mut l = Launcher::for_clipboard(LauncherConfig::default(), rows);
    l.selected = MAX_PINS;
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Redraw);
    assert_eq!(l.message.as_deref(), Some("at most 10 pins"));
    assert!(!l.clips[MAX_PINS].pinned);
    // Unpinning one still works at the limit.
    l.selected = 0;
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Edit(ClipEdit::Pin(0)));
}

#[test]
fn delete_removes_the_row_and_stays_open() {
    let mut l = clipboard_box();
    l.selected = 1;
    assert_eq!(l.key(Key::Delete), Outcome::Edit(ClipEdit::Delete(11)));
    assert_eq!(l.clips.iter().map(|c| c.id).collect::<Vec<_>>(), [10, 12]);
    assert_eq!(sources(&l), [Source::Clip(0), Source::Clip(1), Source::ClipClear]);
    assert_eq!(l.selected, 1, "the next row takes the place");
    // The last row deleted: the selection stays in range.
    l.selected = 1;
    assert_eq!(l.key(Key::Delete), Outcome::Edit(ClipEdit::Delete(12)));
    assert!(l.selected < l.results.len());
    // The Clear row and a row-less box: nothing to delete.
    let mut l = clipboard_box();
    l.selected = 3;
    assert_eq!(l.key(Key::Delete), Outcome::Nothing);
    let mut l = launcher();
    assert_eq!(l.key(Key::Delete), Outcome::Nothing);
}

#[test]
fn deleting_a_pinned_row_is_allowed_and_the_last_unpinned_takes_clear_with_it() {
    let mut l = clipboard_box();
    assert_eq!(l.key(Key::Delete), Outcome::Edit(ClipEdit::Delete(10)));
    l.selected = 0;
    l.key(Key::Delete);
    l.key(Key::Delete);
    assert!(l.results.is_empty(), "no row, no Clear row");
    assert_eq!(l.key(Key::Delete), Outcome::Nothing);
}

#[test]
fn clear_asks_then_needs_a_second_enter_after_0_4_s() {
    let t = Instant::now();
    let mut l = clipboard_box();
    l.selected = 3;
    l.set_now(t);
    assert_eq!(l.key(Key::Enter), Outcome::Redraw);
    assert!(l.clear_question());
    l.set_now(t + Duration::from_millis(200));
    assert_eq!(l.key(Key::Enter), Outcome::Nothing, "a double Enter never confirms");
    l.set_now(t + Duration::from_millis(500));
    assert_eq!(l.key(Key::Enter), Outcome::Edit(ClipEdit::Clear));
    assert_eq!(l.clips.iter().map(|c| c.id).collect::<Vec<_>>(), [10], "only the pinned row stays");
    assert_eq!(sources(&l), [Source::Clip(0)]);
    assert!(!l.clear_question());
}

#[test]
fn clear_confirmation_never_expires() {
    let t = Instant::now();
    let mut l = clipboard_box();
    l.selected = 3;
    l.set_now(t);
    l.key(Key::Enter);
    l.set_now(t + Duration::from_secs(10));
    assert_eq!(l.key(Key::Enter), Outcome::Edit(ClipEdit::Clear), "no 3 s expiry: no timer to redraw it");
}

#[test]
fn the_instant_is_single_use() {
    let t = Instant::now();
    let mut l = clipboard_box();
    // A key that never reads the instant must not leave it for the next event.
    l.set_now(t + Duration::from_secs(1000));
    l.key(Key::Home);
    l.selected = 3;
    assert_eq!(l.key(Key::Enter), Outcome::Redraw, "asks at the real clock, not at a stale instant");
    l.set_now(Instant::now() + Duration::from_millis(500));
    assert_eq!(l.key(Key::Enter), Outcome::Edit(ClipEdit::Clear));
}

#[test]
fn the_instant_is_single_use_after_a_click_too() {
    let mut l = clipboard_box();
    // A click that never reads the instant (a pin) must not leave it for the next click.
    l.set_now(Instant::now() + Duration::from_secs(3600));
    l.click(1, true);
    assert_eq!(l.click(3, false), Outcome::Redraw, "the question is asked at the real clock");
    l.set_now(Instant::now() + Duration::from_secs(10));
    assert_eq!(l.click(3, false), Outcome::Edit(ClipEdit::Clear), "a stale instant would still be in the future");
}

#[test]
fn any_other_key_or_click_cancels_the_clear_question() {
    let t = Instant::now();
    for cancel in [Key::Up, Key::Char('x'), Key::CtrlEnter, Key::Delete, Key::Home] {
        let mut l = clipboard_box();
        l.selected = 3;
        l.set_now(t);
        l.key(Key::Enter);
        assert!(l.clear_question());
        let o = l.key(cancel);
        assert_ne!(o, Outcome::Nothing, "{cancel:?}: the cancelled question is redrawn");
        assert!(!l.clear_question(), "{cancel:?}");
    }
    let mut l = clipboard_box();
    l.selected = 3;
    l.set_now(t);
    l.key(Key::Enter);
    l.set_now(t + Duration::from_millis(500));
    l.key(Key::Up);
    l.key(Key::Down);
    assert_ne!(l.key(Key::Enter), Outcome::Edit(ClipEdit::Clear), "a key between the two cancelled it");
    // A click on another row cancels too.
    let mut l = clipboard_box();
    l.set_now(t);
    l.click(3, false);
    assert!(l.clear_question());
    l.click(0, true);
    assert!(!l.clear_question());
}

#[test]
fn clipboard_mode_has_no_modes_row_and_tab_does_nothing() {
    let mut l = clipboard_box();
    assert!(!l.has_modes());
    assert_eq!(l.key(Key::Tab), Outcome::Nothing);
    assert_eq!(l.key(Key::BackTab), Outcome::Nothing);
    assert_eq!(l.mode, Mode::Clipboard);
    assert!(!Mode::CYCLE.contains(&Mode::Clipboard));
    assert_eq!(l.click_mode(Mode::All), Outcome::Nothing);
}

#[test]
fn clipboard_mode_ignores_the_quote_prefix_and_reads_the_query_as_the_filter() {
    let mut l = clipboard_box();
    l.clipboard = true;
    typed(&mut l, "milk");
    assert_eq!(l.prefixed(), None);
    assert_eq!(sources(&l), [Source::Clip(1)]);
}

#[test]
fn dropping_the_model_wipes_the_previews() {
    let mut rows = rows3();
    wipe_clips(&mut rows);
    assert!(rows.iter().all(|r| r.preview.is_empty() && r.first.is_empty()));
    let src = include_str!("../model.rs");
    let drop_impl = src.split("impl Drop for Launcher").nth(1).unwrap();
    assert!(drop_impl.contains("wipe_clips(&mut self.clips)"), "Drop must call wipe_clips");
}

#[test]
fn a_held_enter_on_clear_neither_asks_nor_confirms() {
    let t = Instant::now();
    let mut l = clipboard_box();
    l.selected = 3;
    l.set_now(t);
    assert_eq!(l.key_repeat(Key::Enter, true), Outcome::Nothing);
    assert!(!l.clear_question(), "a repeat never asks");
    l.key(Key::Enter);
    l.set_now(t + Duration::from_millis(700));
    assert_eq!(l.key_repeat(Key::Enter, true), Outcome::Nothing, "the auto-repeat never confirms");
    assert!(l.clear_question(), "and leaves the question standing");
    l.set_now(t + Duration::from_millis(800));
    assert_eq!(l.key(Key::Enter), Outcome::Edit(ClipEdit::Clear), "a fresh press does");
}

#[test]
fn held_ctrl_enter_and_delete_do_nothing_on_a_history_row_but_arrows_repeat() {
    let mut l = clipboard_box();
    l.selected = 1;
    assert_eq!(l.key_repeat(Key::CtrlEnter, true), Outcome::Nothing);
    assert_eq!(l.key_repeat(Key::Delete, true), Outcome::Nothing);
    assert_eq!(l.clips.len(), 3);
    assert!(!l.clips[1].pinned);
    assert_eq!(l.key_repeat(Key::Down, true), Outcome::Redraw, "movement still repeats");
    assert!(matches!(l.key_repeat(Key::Delete, false), Outcome::Edit(ClipEdit::Delete(_))));
}

#[test]
fn the_wheel_cancels_the_clear_question() {
    let mut l = clipboard_box();
    l.selected = 3;
    l.set_now(Instant::now());
    l.key(Key::Enter);
    assert!(l.clear_question());
    assert_ne!(l.scroll(-1), Outcome::Nothing);
    assert!(!l.clear_question());
    l.selected = 3;
    l.set_now(Instant::now());
    l.key(Key::Enter);
    assert_eq!(l.scroll(1), Outcome::Redraw, "clamped at the end, yet the cancelled question is redrawn");
    assert!(!l.clear_question());
}

#[test]
fn unpinning_puts_the_row_where_the_history_lists_it() {
    let mut l = clipboard_box();
    // alpha pinned; 11, 12 not. Pin 12 then unpin it: the history lists it first among the rest.
    l.selected = 2;
    l.key(Key::CtrlEnter);
    assert_eq!(l.clips.iter().map(|c| c.id).collect::<Vec<_>>(), [12, 10, 11]);
    let i = l.clips.iter().position(|c| c.id == 12).unwrap();
    l.selected = l.results.iter().position(|h| h.source == Source::Clip(i)).unwrap();
    l.key(Key::CtrlEnter);
    assert_eq!(l.clips.iter().map(|c| c.id).collect::<Vec<_>>(), [10, 12, 11], "pinned first, then the unpinned head");
}

/// Tools spec § 1: a new Tools catalog forgets what the ranker had prepared, or its frecency and
/// name ranks would index the old rows.
#[test]
fn set_tools_forgets_the_prepared_catalog() {
    use crate::launcher::tools::catalog::{Catalog as ToolsCatalog, Context};
    let build = |text: &str| {
        let config = LauncherConfig::from_text(text).unwrap();
        ToolsCatalog::build(&config.tools, &Context::default(), &mut |_| {})
    };
    let one = "[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Alpha\"\nrun = [\"x\"]\n";
    let other = "[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Omega\"\nrun = [\"x\"]\n";
    let mut l = launcher();
    l.set_tools(build(one));
    typed(&mut l, "a");
    let after_first = l.ranker.preparations();
    typed(&mut l, "l");
    assert_eq!(l.ranker.preparations(), after_first, "a keystroke prepares nothing again");
    l.set_tools(build(other));
    assert_eq!(l.ranker.preparations(), after_first + 1, "the new catalog is prepared once");
    assert!(sources(&l).iter().all(|s| !matches!(s, Source::Tool(_))), "Omega does not match \"al\": no stale row from Alpha");
}

const FORGET_CFG: &str = r#"
[tools]
builtins = false
[[tools.category]]
name = "Nix"
[[tools.entry]]
name = "One"
category = "Nix"
run = ["x"]
[[tools.entry]]
name = "Two"
category = "Nix"
run = ["x"]
[[tools.entry]]
name = "Three"
category = "Nix"
run = ["x"]
[[tools.entry]]
name = "Root a"
run = ["x"]
[[tools.entry]]
name = "Root b"
run = ["x"]
"#;

fn tools_launcher() -> Launcher {
    use crate::launcher::tools::catalog::{Catalog as ToolsCatalog, Context};
    let config = LauncherConfig::from_text(FORGET_CFG).unwrap();
    let catalog = ToolsCatalog::build(&config.tools, &Context::default(), &mut |_| {});
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), 1_800_000_000);
    l.set_tools(catalog);
    l
}

#[test]
fn going_up_in_tools_forgets_the_prepared_facts() {
    let mut l = tools_launcher();
    typed(&mut l, "/");
    // The root lists Nix, Root a, Root b; Nix lists One, Two, Three: three rows each.
    assert_eq!(l.results.len(), 3);
    l.key(Key::Enter);
    assert_eq!(l.results.len(), 3);
    let before = l.ranker.preparations();
    l.key(Key::Left);
    assert_eq!(l.ranker.preparations(), before + 1, "the root's list is another list with as many rows");
}

#[test]
fn leaving_tools_by_a_mode_click_forgets_the_prepared_facts() {
    let mut l = tools_launcher();
    typed(&mut l, "/");
    typed(&mut l, "o");
    let before = l.ranker.preparations();
    l.click_mode(Mode::All);
    assert_eq!(l.mode, Mode::All);
    assert!(l.ranker.preparations() > before, "the Tools list's facts are not All's");
}

/// A state the daemon read itself (a built-in's switch) is adopted by the model after the `/` jump
/// into Tools, as after a `--tools` open: the jump's outcome asks for it, the model keeps it.
#[test]
fn a_state_adopted_after_the_slash_jump_sticks() {
    let mut l = tools_launcher();
    assert!(matches!(l.key(Key::Char('/')), Outcome::EnterTools(_)), "the jump asks for the daemon's own states");
    let entry = 3; // Root a
    assert_eq!(l.tools().and_then(|t| t.state_of(entry)), None, "unknown until read");
    l.adopt_state(entry, Some(true));
    assert_eq!(l.tools().and_then(|t| t.state_of(entry)), Some(true));
    l.key(Key::Enter); // into the category and back: the state is the catalog's, not the level's
    l.key(Key::Left);
    assert_eq!(l.tools().and_then(|t| t.state_of(entry)), Some(true));
}

const CHOICES_CFG: &str = r#"
[tools]
builtins = false
[[tools.category]]
name = "Nix"
[[tools.entry]]
name = "One"
category = "Nix"
run = ["x"]
[[tools.entry]]
name = "Root a"
run = ["x"]
[[tools.entry]]
name = "Root b"
run = ["x", "{choice}"]
choices = ["p", "q"]
"#;

fn choices_launcher() -> Launcher {
    use crate::launcher::tools::catalog::{Catalog as ToolsCatalog, Context};
    let config = LauncherConfig::from_text(CHOICES_CFG).unwrap();
    let catalog = ToolsCatalog::build(&config.tools, &Context::default(), &mut |_| {});
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), 1_800_000_000);
    l.set_tools(catalog);
    l
}

/// Drilling from a typed query of the root restores the entry's real row on the way up, not 0.
#[test]
fn drilling_from_a_typed_query_restores_the_entrys_real_position() {
    let mut l = choices_launcher();
    typed(&mut l, "/");
    typed(&mut l, "root b");
    l.key(Key::Enter);
    l.key(Key::Left);
    // The root lists Nix, Root a, Root b.
    assert_eq!(l.selected, 2, "Root b's row in the root");
}

/// The level's rows are built on a level change or a delivery, not on every keystroke.
#[test]
fn typing_in_a_tools_level_does_not_rebuild_its_rows() {
    let mut l = choices_launcher();
    typed(&mut l, "/");
    let built = l.tool_level_builds;
    typed(&mut l, "ab");
    l.key(Key::Backspace);
    l.key(Key::Backspace);
    assert_eq!(l.tool_level_builds, built, "keystrokes only rank");
    l.key(Key::Enter); // into Nix
    assert!(l.tool_level_builds > built, "a level change rebuilds");
    // Inside a category the level's rows are cached the same way (the flat search is the root's).
    let built = l.tool_level_builds;
    typed(&mut l, "up");
    l.key(Key::Backspace);
    l.key(Key::Backspace);
    assert_eq!(l.tool_level_builds, built, "keystrokes inside a category only rank");
    l.key(Key::Left); // back to the root
    assert!(l.tool_level_builds > built, "leaving the category rebuilds");
}

#[test]
fn a_state_delivery_rebuilds_the_level() {
    let mut l = choices_launcher();
    typed(&mut l, "/");
    let built = l.tool_level_builds;
    l.adopt_state(1, Some(true));
    assert!(l.tool_level_builds > built);
}
