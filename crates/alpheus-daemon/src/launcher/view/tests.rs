use super::*;
use crate::compositor::Toplevel;
use crate::keys::Key;
use crate::launcher::config::{LauncherConfig, SourceConfig};
use crate::launcher::desktop::{DesktopAction, DesktopEntry};
use crate::launcher::history::History;
use crate::launcher::scan::Catalog;
use crate::launcher::sources::{Entries, SourceEntry};
use crate::list::Edge;
use alpheus_common::launcher_ipc::DmenuFlags;
use alpheus_common::theme::{Rgb, Roles};
use std::path::PathBuf;

const NOW: u64 = 1_800_000_000;

fn app(id: &str, name: &str, generic: Option<&str>, exec: &str) -> DesktopEntry {
    DesktopEntry {
        id: id.into(),
        path: PathBuf::from(format!("/usr/share/applications/{id}")),
        name: name.into(),
        generic_name: generic.map(String::from),
        comment: None,
        keywords: vec![],
        icon: None,
        exec: exec.into(),
        terminal: false,
        startup_wm_class: None,
        actions: vec![],
    }
}

fn catalog() -> Catalog {
    let mut firefox = app("org.mozilla.firefox.desktop", "Firefox", Some("Web Browser"), "firefox %u");
    firefox.comment = Some("Browse the World Wide Web with a fast, private and free browser".into());
    firefox.actions = vec![
        DesktopAction { key: "new-window".into(), name: "New Window".into(), exec: "firefox --new-window %u".into() },
        DesktopAction { key: "new-private-window".into(), name: "New Private Window".into(), exec: "firefox --private-window %u".into() },
    ];
    Catalog {
        apps: vec![
            firefox,
            app("org.gnome.Nautilus.desktop", "Files", Some("File Manager"), "nautilus --new-window"),
            app("Alacritty.desktop", "Alacritty", Some("Terminal"), "foot"),
            app("btop.desktop", "btop++", None, "btop"),
        ],
        commands: vec!["btop".into(), "htop".into(), "nvim".into()],
    }
}

fn windows() -> Vec<Toplevel> {
    let w = |selector: &str, class: &str, title: &str, workspace: &str| Toplevel {
        selector: selector.into(),
        class: class.into(),
        title: title.into(),
        workspace: workspace.into(),
        focused: false,
    };
    vec![w("id:1", "Alacritty", "~/projects — zsh", "3"), w("id:2", "firefox", "Web page — Mozilla Firefox", "2")]
}

fn launcher() -> Launcher {
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    l.set_catalog(catalog());
    l
}

fn keys(l: &mut Launcher, ks: &[Key]) {
    for k in ks {
        l.key(*k);
    }
}

fn typed(l: &mut Launcher, text: &str) {
    for c in text.chars() {
        l.key(Key::Char(c));
    }
}

/// Lays out for an output of `columns` × 40 cells, like the launcher host does:
/// the preview panel reserved for the app launcher, whatever the selection.
fn draw(l: &mut Launcher, columns: u16) -> (BoxLayout, Boxed) {
    let b = layout(columns, 40, l.config.rows, !l.has_modes(), !l.is_bare());
    l.set_list_rows(usize::from(b.list_rows));
    let rows = render(l, &b);
    for (i, r) in rows.rows.iter().enumerate() {
        assert_eq!(r.width(), b.columns - 2, "row {i} of {}", snapshot(&rows));
    }
    assert_eq!(rows.rows.len(), usize::from(b.rows) - 2, "the ring's two rows are dropped");
    (b, rows)
}

#[test]
fn all_mode_lists_apps_then_windows() {
    let mut l = launcher();
    let (b, rows) = draw(&mut l, 100);
    // The user hasn't moved the selection yet, so the catalog's arrival (inside
    // `launcher()`) put it back on top (2026-10-05): the first app, not the window
    // `for_apps` started on. The panel still fills with its preview either way.
    assert_eq!((b.columns, b.preview_columns), (97, PREVIEW_COLUMNS), "the panel reserved, filled: the selected app's preview");
    insta::assert_snapshot!("all", snapshot(&rows));
}

#[test]
fn apps_mode_with_the_preview() {
    let mut l = launcher();
    keys(&mut l, &[Key::Tab, Key::Down]); // Apps: Alacritty, btop++, Files, Firefox
    keys(&mut l, &[Key::Down, Key::Down]);
    let (b, rows) = draw(&mut l, 120);
    assert_eq!((b.columns, b.preview_columns), (97, PREVIEW_COLUMNS), "Firefox has actions: its preview fills the panel");
    insta::assert_snapshot!("apps_preview", snapshot(&rows));
}

#[test]
fn windows_mode() {
    let mut l = launcher();
    keys(&mut l, &[Key::Tab, Key::Tab]);
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("windows", snapshot(&rows));
}

#[test]
fn commands_mode_entered_by_its_prefix() {
    let mut l = launcher();
    typed(&mut l, ">top");
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("commands", snapshot(&rows));
}

#[test]
fn script_mode_with_and_without_prompt() {
    let lines = vec!["Scan rapide".to_string(), "Scan complet".to_string(), "Ports".to_string()];
    let mut l = Launcher::for_script(LauncherConfig::default(), Some("nmap: ".into()), lines.clone());
    typed(&mut l, "scan");
    let (b, rows) = draw(&mut l, 100);
    assert_eq!(b.list_top, 2, "no modes row");
    insta::assert_snapshot!("script_prompt", snapshot(&rows));
    let mut l = Launcher::for_script(LauncherConfig::default(), None, lines);
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("script", snapshot(&rows));
}

/// `--password`: one `•` per character typed (not per byte), never the text; no list, not
/// even "no match".
#[test]
fn password_mode_draws_dots_and_no_list() {
    let flags = DmenuFlags { index: false, password: true, visible: None };
    let mut l = Launcher::for_dmenu(LauncherConfig::default(), Some("Key: ".into()), vec!["saved".into()], flags);
    typed(&mut l, "pä ss");
    let (b, rows) = draw(&mut l, 100);
    let text = snapshot(&rows);
    assert!(text.contains("Key: •••••"), "{text}");
    assert!(!text.contains("••••••"), "one dot per character: {text}");
    let input = rows.rows.iter().map(|r| r.text()).find(|t| t.contains("Key:")).expect("the input row");
    assert!(!input.contains(['p', 'ä', 's']), "typed text shown: {input}");
    assert!(!text.contains("saved") && !text.contains("no match"), "{text}");
    let list: Vec<String> = rows.rows[usize::from(b.list_top) - 1..][..usize::from(b.list_rows)].iter().map(|r| r.text()).collect();
    assert!(list.iter().all(|r| r.trim().is_empty() || r.trim() == "│"), "the list stays blank: {list:?}");
}

/// Helper shared with other tests: the preview panel's text, one entry per interior row.
fn panel(rows: &Boxed, b: &BoxLayout) -> Vec<String> {
    let start = usize::from(b.columns - b.preview_columns - 2);
    rows.rows.iter().map(|r| r.text().chars().skip(start).take(usize::from(b.preview_columns)).collect()).collect()
}

/// 2026-10-05, after seeing the real rendering: the box never changes width as the
/// selection moves, and the panel — once reserved — always shows the selected result; it
/// is never blank while one exists. `Left`/`Right` (the model has no toggle for them to drive, and no `Ctrl+Space`) are still
/// recognised keys,
/// simply ignored — pressing them changes nothing here either.
#[test]
fn the_panel_always_shows_the_selected_result() {
    let blank = " ".repeat(usize::from(PREVIEW_COLUMNS));
    let mut l = launcher();
    let (b, rows) = draw(&mut l, 120);
    // Not moved yet: the top app (Alacritty), not the window `for_apps` started on.
    assert!(panel(&rows, &b).iter().any(|t| t.contains("Terminal")), "an app selected: its generic name\n{}", snapshot(&rows));
    assert!(!panel(&rows, &b).iter().all(|t| *t == blank), "never blank while a result is selected");
    keys(&mut l, &[Key::Tab, Key::End]); // Apps: Firefox, with actions
    let (b2, rows) = draw(&mut l, 120);
    assert_eq!(b2, b, "the same box");
    assert!(panel(&rows, &b).iter().any(|t| t.contains("New Private Window")), "{}", snapshot(&rows));
    keys(&mut l, &[Key::Up]); // Files, no actions: still shows its own info
    let (b3, rows) = draw(&mut l, 120);
    assert_eq!(b3, b, "never narrower");
    assert!(panel(&rows, &b).iter().any(|t| t.contains("File Manager")), "no actions: still filled\n{}", snapshot(&rows));
    l.key(Key::Left); // ignored now (it used to hide the preview)
    let (b4, rows) = draw(&mut l, 120);
    assert_eq!(b4, b, "still the same box");
    assert!(panel(&rows, &b).iter().any(|t| t.contains("File Manager")), "Left no longer blanks it\n{}", snapshot(&rows));
}

/// Via the ranker's window → app link (`matching::app_for_window`) when the
/// window's class names a known app, or the class itself otherwise.
#[test]
fn window_preview_names_its_app_via_the_class_link_or_the_class_itself() {
    // Scoped to the preview panel itself (`panel`, not the whole box): the result list on
    // the left also shows app names, which would make a substring check on the whole
    // snapshot pass even if the preview picked the wrong text entirely.
    let mut l = launcher();
    // Not moved yet: the top app is selected first (Alacritty). Past the 4 apps, the
    // Alacritty window: its class is also an app's id.
    keys(&mut l, &[Key::Down, Key::Down, Key::Down, Key::Down]);
    let (b, rows) = draw(&mut l, 120);
    let text = panel(&rows, &b);
    assert!(text.iter().any(|t| t.contains("~/projects — zsh")), "the full title\n{}", snapshot(&rows));
    assert!(text.iter().any(|t| t.contains("Alacritty")), "linked to the Alacritty app by class\n{}", snapshot(&rows));
    l.key(Key::Down); // the firefox window: class "firefox" links to the Firefox app
    let (b, rows) = draw(&mut l, 120);
    let text = panel(&rows, &b);
    assert!(text.iter().any(|t| t.contains("Firefox")), "its app's name, not the raw class \"firefox\"\n{}", snapshot(&rows));
    assert!(!text.iter().any(|t| t.trim() == "firefox"), "not the raw lower-case class\n{}", snapshot(&rows));
    let mut w = windows();
    w.push(Toplevel {
        selector: "id:9".into(),
        class: "unknown-app".into(),
        title: "Mystery".into(),
        workspace: "9".into(),
        focused: false,
    });
    let mut l2 = Launcher::for_apps(LauncherConfig::default(), w, History::empty(200), NOW);
    l2.set_catalog(catalog());
    // Not moved yet: past the 4 apps and the first 2 windows, the third: "Mystery", linked
    // to no app.
    keys(&mut l2, &[Key::Down, Key::Down, Key::Down, Key::Down, Key::Down, Key::Down]);
    let (b, rows) = draw(&mut l2, 120);
    let text = panel(&rows, &b);
    assert!(text.iter().any(|t| t.contains("unknown-app")), "no linked app: the class itself\n{}", snapshot(&rows));
}

/// 2026-10-05: "its description and binary" even without a `.desktop` Action.
#[test]
fn apps_mode_preview_without_actions_still_shows_description_and_binary() {
    let mut c = catalog();
    c.apps[1].comment = Some("Browse and manage the files on this computer".into()); // Files: no actions
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    l.set_catalog(c);
    keys(&mut l, &[Key::Tab, Key::Down, Key::Down]); // Apps: Alacritty, btop++, Files
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::App(1)), "Files");
    let (_, rows) = draw(&mut l, 120);
    insta::assert_snapshot!("apps_preview_no_actions", snapshot(&rows));
}

/// The comment is capped, not the actions — a long comment loses its own
/// tail (ending with "…") rather than pushing "exec:" and the actions off the panel.
#[test]
fn a_long_comment_is_capped_so_the_actions_stay_visible() {
    let mut c = catalog();
    c.apps[0].comment = Some("word ".repeat(60)); // Firefox: many more wrapped lines than the panel has rows
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    l.set_catalog(c);
    keys(&mut l, &[Key::Tab, Key::End]); // Apps: Firefox (last, alphabetically)
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::App(0)));
    let (b, rows) = draw(&mut l, 120);
    let text = panel(&rows, &b);
    assert!(text.iter().any(|t| t.contains("New Private Window")), "the action stays visible\n{}", snapshot(&rows));
    assert!(text.iter().any(|t| t.contains("exec: firefox")), "exec stays visible too\n{}", snapshot(&rows));
    assert!(text.iter().any(|t| t.trim_end().ends_with('…')), "the comment's cut is marked\n{}", snapshot(&rows));
}

/// The render-level safety net: `capped_wrap` only bounds the one part of
/// the preview that can grow without limit (a comment, a title); a *fixed* tail longer
/// than the panel itself — many `.desktop` Actions at the smallest `rows` configuration —
/// is not something it caps. `render`'s own check must still mark that cut, not drop it
/// silently.
#[test]
fn an_overflowing_fixed_tail_is_marked_not_dropped_silently() {
    let mut c = catalog();
    c.apps[0].actions = (0..10).map(|i| DesktopAction { key: format!("a{i}"), name: format!("Action {i}"), exec: "x".into() }).collect();
    // interior_rows at its minimum (4): far fewer than the 10 actions alone.
    let config = LauncherConfig { rows: 1, ..LauncherConfig::default() };
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    l.set_catalog(c);
    keys(&mut l, &[Key::Tab, Key::End]); // Apps: Firefox (last, alphabetically), now 10 actions
    assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::App(0)));
    let (b, rows) = draw(&mut l, 120);
    let text = panel(&rows, &b);
    assert!(text.iter().any(|t| t.trim_end().ends_with('…')), "the overflow is marked\n{}", snapshot(&rows));
}

/// At the minimum width, a long kind must not overwrite the icon.
#[test]
fn the_kind_column_never_overwrites_the_icon_at_minimum_width() {
    let mut w = windows();
    w[0].workspace = "many-many-many-characters".into(); // "window · many-many-…" would reach past the icon
    let mut l = Launcher::for_apps(LauncherConfig::default(), w, History::empty(200), NOW);
    l.set_catalog(catalog());
    let (b, rows) = draw(&mut l, MIN_COLUMNS);
    let row = &rows.rows[usize::from(b.list_top) - 1];
    let icon = icons::glyph(&l.windows[0].class);
    assert_eq!(row.text().chars().nth(1), Some(icon), "the icon must survive the kind column: {:?}", row.text());
}

/// A title made only of zero-width characters (bidi marks here) is as
/// empty as `""` would be — it must fall back to the class, like `describe` already does
/// for a truly empty title.
#[test]
fn a_title_made_only_of_zero_width_characters_falls_back_to_the_class() {
    let mut w = windows();
    w[0].title = "\u{200e}\u{200f}\u{2066}\u{2069}".into();
    let mut l = Launcher::for_apps(LauncherConfig::default(), w, History::empty(200), NOW);
    l.set_catalog(catalog());
    let (b, rows) = draw(&mut l, 100);
    let row = &rows.rows[usize::from(b.list_top) - 1];
    assert!(row.text().contains("Alacritty"), "falls back to the class: {:?}", row.text());
}

/// `s`'s own trailing ellipsis, right at the cut point, must survive —
/// only the one `tail` itself adds should ever be stripped.
#[test]
fn tail_strips_exactly_one_ellipsis_not_the_users_own() {
    let s = format!("{}…AAA", "X".repeat(10));
    assert_eq!(tail(&s, 5), "……AAA");
}

/// A "word" that displays as nothing must not force a space around it,
/// nor stand as a line on its own.
#[test]
fn wrap_skips_zero_width_words() {
    assert_eq!(wrap("foo \u{200e} bar", 10), vec!["foo bar".to_string()]);
    assert_eq!(wrap("\u{200e} \u{200f}", 10), Vec::<String>::new(), "only invisible words: no blank line");
}

#[test]
fn commands_mode_preview_shows_the_full_path_when_known() {
    let dir = crate::launcher::testing::TempDir::new("view-command-path");
    std::fs::write(dir.join("btop"), b"").unwrap();
    let mut l = launcher();
    l.path_dirs = vec![dir.to_path_buf()];
    typed(&mut l, ">top");
    let (_, rows) = draw(&mut l, 100);
    let text = snapshot(&rows);
    let expected = dir.join("btop").display().to_string();
    assert!(!text.contains("command on $PATH"), "a real path was resolved\n{text}");
    // The panel is only ~34 cells wide: a long $TMPDIR path is truncated, but its
    // prefix (well under that width) must still appear verbatim.
    assert!(text.contains(&expected[..20.min(expected.len())]), "{text}\nexpected a prefix of {expected:?}");
}

#[test]
fn narrow_output_drops_the_preview_and_truncates() {
    let mut l = launcher();
    keys(&mut l, &[Key::Tab, Key::End]); // a result selected: the panel would fill if there were room
    let (b, rows) = draw(&mut l, 40);
    assert_eq!((b.columns, b.preview_columns), (38, 0));
    insta::assert_snapshot!("narrow", snapshot(&rows));
}

/// The help line must still read in full in the narrow box (no preview panel reserved,
/// `BOX_COLUMNS` — the floor a wide, preview-reserving box also shrinks to when there
/// isn't room for the panel): the Commands help line used to be longer than what
/// `render`'s `truncate` leaves it, silently losing its tail (2026-10-05).
#[test]
fn every_mode_help_line_fits_the_narrow_box() {
    let mut l = launcher();
    let b = layout(200, 40, l.config.rows, false, false);
    assert_eq!(b.columns, BOX_COLUMNS, "a wide enough output: the narrow (non-preview) box");
    let limit = usize::from(b.pane_end().saturating_sub(3));
    for mode in [Mode::All, Mode::Apps, Mode::Windows, Mode::Commands] {
        l.mode = mode;
        let text = help_text(&l);
        assert!(text.chars().count() <= limit, "{mode:?}: {text:?} is {} chars, limit {limit}", text.chars().count());
    }
    let s = Launcher::for_script(LauncherConfig::default(), None, vec![]);
    let text = help_text(&s);
    assert!(text.chars().count() <= limit, "Script: {text:?} is {} chars, limit {limit}", text.chars().count());
    let c = Launcher::for_calc(LauncherConfig::default());
    let text = help_text(&c);
    assert!(text.chars().count() <= limit, "Calc: {text:?} is {} chars, limit {limit}", text.chars().count());
}

/// Windows mode never has a `.desktop` action to run: Ctrl+Enter on a window is just Enter
/// again (`secondary`'s catch-all falls back to `primary`), so the generic "ctrl+enter
/// action" line (true for Apps, and for All's app results) would be misleading here.
#[test]
fn windows_mode_help_does_not_promise_an_action() {
    let mut l = launcher();
    l.mode = Mode::Windows;
    assert!(!help_text(&l).contains("action"), "{:?}", help_text(&l));
}

#[test]
fn no_match_scanning_and_a_message() {
    let mut l = launcher();
    typed(&mut l, "zzzz");
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("no_match", snapshot(&rows));
    let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
    typed(&mut l, "zzzz");
    let (_, rows) = draw(&mut l, 100);
    assert!(rows.rows[2].text().contains("scanning…"), "{}", snapshot(&rows));
    let mut l = launcher();
    l.show_message("firefox: program not found".into());
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("message", snapshot(&rows));
}

#[test]
fn hostile_names_never_misalign_the_box() {
    let hostile = [
        "x".repeat(300),
        "tab\there\nnewline\u{1b}[31m escape".to_string(),
        "\u{202e}reversed\u{200e} bidi \u{2066}isolate\u{2069}".to_string(),
        "日本語のアプリケーション名前がとても長い場合".to_string(),
        "🦀🦀🦀 crab 🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀".to_string(),
        "\u{2028}line\u{2029}para".to_string(),
    ];
    let mut c = catalog();
    for (i, h) in hostile.iter().enumerate() {
        let mut e = app(&format!("h{i}.desktop"), h, Some(h), h);
        e.comment = Some(h.clone());
        e.actions = vec![DesktopAction { key: "a".into(), name: h.clone(), exec: "x".into() }];
        c.apps.push(e);
        c.commands.push(h.clone());
    }
    let mut w = windows();
    for h in &hostile {
        w.push(Toplevel { selector: "id:9".into(), class: h.clone(), title: h.clone(), workspace: h.clone(), focused: false });
    }
    let lines = hostile.to_vec();
    for columns in [24u16, 40, 61, 99, 200] {
        for mode_keys in [&[][..], &[Key::Tab][..], &[Key::Tab, Key::Tab][..], &[Key::BackTab][..]] {
            let mut l = Launcher::for_apps(LauncherConfig::default(), w.clone(), History::empty(200), NOW);
            l.set_catalog(c.clone());
            keys(&mut l, mode_keys);
            for _ in 0..l.results.len() {
                check(&mut l, columns);
                l.key(Key::Down);
            }
        }
        let mut s = Launcher::for_script(LauncherConfig::default(), Some(hostile[1].clone()), lines.clone());
        typed(&mut s, &hostile[0]);
        check(&mut s, columns);
        s.key(Key::CtrlU);
        check(&mut s, columns);
    }

    fn check(l: &mut Launcher, columns: u16) {
        let (b, rows) = draw(l, columns);
        for r in &rows.rows {
            assert_eq!(r.width(), b.columns - 2);
            assert!(r.cells().iter().all(|c| !c.ch.is_control() || c.ch == crate::grid::CONT), "{:?}", r.text());
            assert!(!r.text().contains(['\u{2028}', '\u{2029}']), "{:?}", r.text());
        }
        // Every field this fixture makes hostile (window class, title and
        // workspace; command names; action names; app name, generic name and comment; the
        // dmenu prompt and its lines) is covered right here, not only by the six now-redundant
        // single-field tests this replaces.
        assert_no_raw_hostile_characters(&rows);
    }
}

#[test]
fn layout_on_tiny_and_huge_outputs() {
    assert_eq!(layout(200, 60, 10, false, false), BoxLayout { columns: 60, rows: 15, list_top: 3, list_rows: 10, preview_columns: 0 });
    assert_eq!(layout(200, 60, 10, true, false), BoxLayout { columns: 60, rows: 14, list_top: 2, list_rows: 10, preview_columns: 0 });
    assert_eq!(layout(200, 60, 10, false, true).columns, 97, "60 + 36 + 1");
    assert_eq!(layout(99, 60, 10, false, true).preview_columns, PREVIEW_COLUMNS, "97 + 2 just fits");
    assert_eq!(layout(98, 60, 10, false, true), BoxLayout { columns: 60, rows: 15, list_top: 3, list_rows: 10, preview_columns: 0 });
    assert_eq!(layout(40, 60, 10, false, false).columns, 38, "output - 2");
    assert_eq!(layout(25, 60, 10, false, false).columns, 24, "never under MIN_COLUMNS");
    assert_eq!(layout(10, 60, 10, false, false).columns, 10, "unless the output is smaller");
    assert_eq!(layout(0, 0, 10, false, false), BoxLayout { columns: 0, rows: 6, list_top: 3, list_rows: 1, preview_columns: 0 });
    assert_eq!(layout(200, 12, 10, false, false).list_rows, 7, "the height leaves 7 rows");
    assert_eq!(layout(200, 3, 30, false, false).list_rows, 1, "at least one");
    assert_eq!(layout(u16::MAX, u16::MAX, 30, false, true).columns, 97);
    let mut l = launcher();
    for (c, r) in [(0u16, 0u16), (1, 1), (10, 3), (u16::MAX, u16::MAX)] {
        let b = layout(c, r, 10, false, true);
        l.set_list_rows(usize::from(b.list_rows));
        assert!(render(&l, &b).rows.iter().all(|g| g.width() == b.columns.saturating_sub(2)), "{c}×{r}");
    }
}

#[test]
fn hit_finds_results_modes_and_the_outside() {
    let mut l = launcher();
    let (b, _) = draw(&mut l, 100);
    assert_eq!(hit(&l, &b, 5, b.list_top), Some(Click::Result(0)));
    assert_eq!(hit(&l, &b, 58, b.list_top + 5), Some(Click::Result(5)), "last cell of the pane");
    assert_eq!(hit(&l, &b, 5, b.list_top + 6), Some(Click::Inside), "below the last result");
    assert_eq!(hit(&l, &b, 0, b.list_top), Some(Click::Inside), "the frame");
    assert_eq!(hit(&l, &b, 1, 1), Some(Click::Mode(Mode::All)));
    assert_eq!(hit(&l, &b, 6, 1), Some(Click::Mode(Mode::Apps)));
    assert_eq!(hit(&l, &b, 12, 1), Some(Click::Mode(Mode::Windows)));
    assert_eq!(hit(&l, &b, 25, 1), Some(Click::Mode(Mode::Commands)));
    assert_eq!(hit(&l, &b, 40, 1), Some(Click::Inside), "after the labels");
    assert_eq!(hit(&l, &b, 10, 2), Some(Click::Inside), "the input line");
    assert_eq!(hit(&l, &b, b.columns, 3), None, "right of the box");
    assert_eq!(hit(&l, &b, 3, b.rows), None, "below the box");
    l.key(Key::PageDown);
    l.set_list_rows(2);
    assert_eq!(hit(&l, &b, 5, b.list_top), Some(Click::Result(l.top)), "absolute index once scrolled");
    let mut s = Launcher::for_script(LauncherConfig::default(), None, vec!["a".into()]);
    let (b, _) = draw(&mut s, 100);
    assert_eq!(hit(&s, &b, 3, 1), Some(Click::Inside), "script: row 1 is the input line, no modes");
    assert_eq!(hit(&s, &b, 3, 2), Some(Click::Result(0)));
}

/// Every string the view shows must have its
/// control characters (`\n`, `\r`, `\t`, other C0/C1) and bidi override marks neutralised
/// before a row is shaped — never shown literally, never breaking the row's width. The
/// mechanism lives in `Grid::write` (controls become spaces; zero-width marks, including
/// bidi overrides, are dropped). `hostile_names_never_misalign_the_box`'s
/// `check` now asserts this too, for every field its fixture makes hostile at once (window
/// class/title/workspace, command names, action names, app name/generic name/comment, the
/// dmenu prompt and its lines) — so the six single-field tests this helper used to serve
/// are redundant and were folded into that one combinatorial test.
fn assert_no_raw_hostile_characters(rows: &Boxed) {
    const HOSTILE: [char; 6] = ['\n', '\r', '\t', '\u{202e}', '\u{200e}', '\u{2066}'];
    for r in &rows.rows {
        let t = r.text();
        assert!(!t.contains(HOSTILE), "a control character or bidi override leaked into a row: {t:?}");
    }
}

#[test]
fn an_entry_row_shows_its_path_dimmed_two_cells_after_it() {
    let mut g = Grid::empty(40);
    entry_row(&mut g, 0, 40, "Vibrant", Some("Apparence › Schéma couleurs"), false);
    assert_eq!(g.text(), format!(" Vibrant  Apparence › Schéma couleurs{}", " ".repeat(3)));
    assert_eq!(g.styles(), format!("{}{}{}", ".".repeat(10), "m".repeat(27), ".".repeat(3)));
    let mut g = Grid::empty(40);
    entry_row(&mut g, 0, 40, "Verrouiller", None, true);
    assert_eq!(g.text().trim_end(), " Verrouiller", "no path: the entry alone, no glyph");
    assert_eq!(g.styles(), "S".repeat(40), "selected: the whole span, as list::row");
}

/// On the selected row the path takes the treatment `list::row` gives its
/// secondary text — SELECTED, like the rest of the band.
#[test]
fn a_selected_entry_row_draws_its_path_as_list_row_draws_secondary_text() {
    let mut g = Grid::empty(40);
    entry_row(&mut g, 0, 40, "Vibrant", Some("Apparence › Schéma couleurs"), true);
    assert_eq!(g.text(), format!(" Vibrant  Apparence › Schéma couleurs{}", " ".repeat(3)));
    assert_eq!(g.styles(), "S".repeat(40), "the path undimmed on the band");
    let mut r = Grid::empty(40);
    let row = Row { icon: None, icon_style: Style::ACCENT, text: "Vibrant", right: "kind", style: Style::NORMAL, selected: true };
    list::row(&mut r, 0, 40, &row);
    assert_eq!(r.styles(), "S".repeat(40), "list::row's secondary text on a selected row: the same");
}

#[test]
fn a_long_path_is_cut_to_fit_the_row() {
    let mut g = Grid::empty(24);
    entry_row(&mut g, 0, 24, "Vibrant", Some("Apparence › Schéma couleurs"), false);
    let t = g.text();
    assert!(t.starts_with(" Vibrant  Appar"), "{t:?}");
    assert!(t.trim_end().ends_with('…'), "{t:?}");
    assert!(text_width(t.trim_end()) <= 23, "one cell kept for the margin: {t:?}");
}

#[test]
fn a_typed_query_lists_the_entry_with_its_path_and_previews_it() {
    let mut l = launcher();
    let source = SourceConfig { name: "Outils".into(), list: vec!["x".into()], run: vec!["x".into(), "{line}".into()] };
    l.set_entries(Entries { sources: vec![source], items: vec![SourceEntry::new(0, "Apparence · Schéma couleurs · Vibrant")] });
    typed(&mut l, "vibrant");
    let (b, rows) = draw(&mut l, 100);
    let first = rows.rows[usize::from(b.list_top) - 1].text();
    assert!(first.contains("Vibrant  Apparence › Schéma couleurs"), "{first:?}");
    let all = snapshot(&rows);
    assert!(all.contains("enter  run"), "the preview says what Enter does: {all}");
}

#[test]
fn the_calculator_row_reads_equals_and_calculator() {
    let mut l = launcher();
    typed(&mut l, "2+2");
    let (b, rows) = draw(&mut l, 100);
    let row = rows.rows[usize::from(b.list_top) - 1].text();
    assert!(row.contains("= 4") && row.contains("calculator"), "{row}");
    assert!(row.contains(CALC_ICON), "{row}");
    // The preview: the query, the answer, then both actions.
    let lines: Vec<String> = panel(&rows, &b).iter().map(|t| t.trim().to_string()).collect();
    assert_eq!(lines[0], "2+2", "{lines:?}");
    assert_eq!(lines[1], "= 4");
    assert!(lines.iter().any(|t| t == "enter  copy") && lines.iter().any(|t| t == "ctrl+enter  copy expression"), "{lines:?}");
}

#[test]
fn an_error_row_reads_error_in_the_error_style() {
    let mut l = launcher();
    typed(&mut l, "1/0");
    let (b, rows) = draw(&mut l, 100);
    let row = &rows.rows[usize::from(b.list_top) - 1];
    assert!(row.text().contains("error: division by zero"), "{}", row.text());
    let lines = panel(&rows, &b);
    assert!(lines.iter().all(|t| !t.starts_with("enter")), "nothing to copy: {lines:?}");
}

#[test]
fn an_empty_copy_leaves_the_actions_out_of_the_preview_and_the_help() {
    let config = LauncherConfig { copy: vec![], ..LauncherConfig::default() };
    let mut l = Launcher::for_apps(config.clone(), windows(), History::empty(200), NOW);
    typed(&mut l, "2+2");
    let (b, rows) = draw(&mut l, 100);
    let lines = panel(&rows, &b);
    assert!(lines.iter().all(|t| !t.contains("copy")), "{lines:?}");
    let mut c = Launcher::for_calc(config);
    typed(&mut c, "2+2");
    assert_eq!(help_text(&c), "esc close");
}

#[test]
fn the_calculator_mode_has_no_modes_row_and_its_own_help() {
    let mut l = Launcher::for_calc(LauncherConfig::default());
    let (b, rows) = draw(&mut l, 100);
    assert_eq!(b.list_top, 2, "no modes row");
    assert_eq!(b.preview_columns, 0, "no preview");
    assert!(!rows.rows.iter().any(|r| r.text().contains("All")), "{}", snapshot(&rows));
    assert!(rows.rows[usize::from(b.list_top) - 1].text().contains("type an expression"), "{}", snapshot(&rows));
    assert!(!snapshot(&rows).contains("scanning"));
    assert_eq!(help_text(&l), "enter copy · ctrl+enter copy expression · esc close");
    typed(&mut l, "6*7");
    let (b, rows) = draw(&mut l, 100);
    assert!(rows.rows[usize::from(b.list_top) - 1].text().contains("= 42"), "{}", snapshot(&rows));
    // Clicking where the modes row would be reaches no mode.
    assert_eq!(hit(&l, &b, 3, 1), Some(Click::Inside));
}

#[test]
fn the_search_row_and_its_help() {
    let mut l = launcher();
    typed(&mut l, "? rust");
    let (b, rows) = draw(&mut l, 100);
    let row = rows.rows[usize::from(b.list_top) - 1].text();
    assert!(row.contains("Search the web: rust") && row.contains("web"), "{row}");
    assert!(row.contains(SEARCH_ICON), "{row}");
    assert_eq!(help_text(&l), "enter search · esc close");
    let lines: Vec<String> = panel(&rows, &b).iter().map(|t| t.trim().to_string()).collect();
    assert_eq!(lines[0], "rust", "{lines:?}");
    assert!(lines[1].starts_with("https://duckduckgo.com/?q=rust"), "{lines:?}");
    assert!(lines.iter().any(|t| t == "enter  search"), "{lines:?}");
}

#[test]
fn a_bare_prefix_shows_its_hint_not_scanning() {
    for (query, hint) in [("?", "type to search the web"), (":", "type an emoji name"), ("$", "type a command to run in a shell")] {
        let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
        assert!(l.catalog.is_none());
        typed(&mut l, query);
        let (b, rows) = draw(&mut l, 100);
        let row = rows.rows[usize::from(b.list_top) - 1].text();
        assert!(row.contains(hint), "{query}: {}", snapshot(&rows));
        assert!(!snapshot(&rows).contains("scanning"), "{query}");
    }
    let mut l = launcher();
    typed(&mut l, ":zzqx");
    let (_, rows) = draw(&mut l, 100);
    assert!(snapshot(&rows).contains("no match"));
}

#[test]
fn an_emoji_row_keeps_its_name_visible() {
    let mut l = launcher();
    typed(&mut l, ":flag: France");
    let (b, rows) = draw(&mut l, 100);
    let row = rows.rows[usize::from(b.list_top) - 1].text();
    assert!(row.contains("flag: France") && row.contains("\u{1f1eb}\u{1f1f7}"), "{row}");
    assert_eq!(help_text(&l), "enter copy \u{b7} ctrl+enter copy name \u{b7} esc close");
    let lines: Vec<String> = panel(&rows, &b).iter().map(|t| t.trim().to_string()).collect();
    assert_eq!(lines[0], "flag: France", "{lines:?}");
    assert!(lines.iter().any(|t| t == "\u{1f1eb}\u{1f1f7}"), "{lines:?}");
    assert!(lines.iter().any(|t| t == "enter  copy") && lines.iter().any(|t| t == "ctrl+enter  copy name"), "{lines:?}");
    let config = LauncherConfig { copy: vec![], ..LauncherConfig::default() };
    let mut c = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    typed(&mut c, ":flag");
    assert_eq!(help_text(&c), "esc close");
}

#[test]
fn the_shell_row_and_its_help() {
    let mut l = launcher();
    typed(&mut l, "$ ls -la");
    let (b, rows) = draw(&mut l, 100);
    let row = rows.rows[usize::from(b.list_top) - 1].text();
    assert!(row.contains("Run in a shell: ls -la") && row.contains("shell"), "{row}");
    assert_eq!(help_text(&l), "enter run \u{b7} ctrl+enter run in terminal \u{b7} esc close");
    let lines: Vec<String> = panel(&rows, &b).iter().map(|t| t.trim().to_string()).collect();
    assert_eq!(lines[0], "ls -la", "{lines:?}");
    assert!(lines.iter().any(|t| t == "enter  run in background"), "{lines:?}");
    assert!(lines.iter().any(|t| t == "ctrl+enter  run in terminal"), "{lines:?}");
}

#[test]
fn the_switcher_help_and_empty_text() {
    let mut l = Launcher::for_windows(LauncherConfig::default(), windows());
    assert_eq!(help_text(&l), "enter focus · tab next · esc close");
    l = Launcher::for_windows(LauncherConfig::default(), vec![]);
    let (_, rows) = draw(&mut l, 100);
    let text = snapshot(&rows);
    assert!(text.contains("no window open"), "{text}");
    assert!(!text.contains("scanning"), "{text}");
}

/// Launcher 2 review: the switcher is windows only, so no All/Apps/Windows/Commands row, and no
/// click on where it was.
#[test]
fn the_switcher_has_no_modes_row() {
    let mut l = Launcher::for_windows(LauncherConfig::default(), windows());
    let (b, rows) = draw(&mut l, 100);
    let text = snapshot(&rows);
    for label in ["All", "Apps", "Windows", "Commands"] {
        assert!(!text.contains(label), "{label} in {text}");
    }
    assert!(rows.rows[0].text().contains('\u{276f}'), "the input row comes first: {text}");
    assert_eq!(hit(&l, &b, 3, 1), Some(Click::Inside), "row 1 is the input now");
    assert!(!l.has_modes());
}

// ---- the clipboard history (clipboard history) ----

fn clip_row(id: u64, preview: &str, first: &str, more_lines: usize, pinned: bool) -> crate::clipboard::history::ClipRow {
    crate::clipboard::history::ClipRow { id, preview: preview.into(), first: first.into(), more_lines, pinned }
}

fn clip_box(rows: Vec<crate::clipboard::history::ClipRow>) -> Launcher {
    Launcher::for_clipboard(LauncherConfig::default(), rows)
}

fn result_text(rows: &Boxed, b: &BoxLayout, i: u16) -> String {
    rows.rows[usize::from(b.list_top - 1 + i)].text()
}

#[test]
fn the_empty_history_shows_its_hint() {
    let mut l = clip_box(vec![]);
    let (b, rows) = draw(&mut l, 100);
    assert!(result_text(&rows, &b, 0).contains("the clipboard history is empty"), "{}", snapshot(&rows));
    assert_eq!(b.preview_columns, 0, "no panel for the history alone");
    assert_eq!(help_text(&l), "esc close", "nothing to act on");
    let mut l = clip_box(vec![clip_row(1, "milk", "milk", 0, false)]);
    typed(&mut l, "zzqx");
    let (b, rows) = draw(&mut l, 100);
    assert!(result_text(&rows, &b, 0).contains("no match"));
}

#[test]
fn a_multi_line_row_shows_its_first_line_and_more_lines_and_a_pinned_one_its_glyph() {
    let mut l = clip_box(vec![
        clip_row(1, "fn main second line", "fn main", 2, true),
        clip_row(2, "plain", "plain", 0, false),
        clip_row(3, "one more", "one more", 1, false),
    ]);
    let (b, rows) = draw(&mut l, 100);
    let (r0, r1, r2) = (result_text(&rows, &b, 0), result_text(&rows, &b, 1), result_text(&rows, &b, 2));
    assert!(r0.contains("fn main") && r0.contains("+2 lines") && r0.contains(PIN_ICON), "{r0}");
    assert!(!r0.contains("second line"), "only the first line is shown: {r0}");
    assert!(r1.contains("plain") && !r1.contains("lines") && !r1.contains(PIN_ICON), "{r1}");
    assert!(r2.contains("+1 line") && !r2.contains("+1 lines"), "{r2}");
}

#[test]
fn the_clear_row_reads_its_question() {
    let mut l = clip_box(vec![clip_row(1, "milk", "milk", 0, false)]);
    let (b, rows) = draw(&mut l, 100);
    let clear = result_text(&rows, &b, 1);
    assert!(clear.contains("Clear history") && clear.contains("pins kept"), "{}", snapshot(&rows));
    assert!(clear.trim_end().ends_with("pins kept"), "dim, on the right: {clear}");
    l.selected = 1;
    l.key(Key::Enter);
    let (b, rows) = draw(&mut l, 100);
    assert!(result_text(&rows, &b, 1).contains("Clear? ⏎"), "{}", snapshot(&rows));
    assert!(!result_text(&rows, &b, 1).contains("Clear history"), "the question replaces the label");
}

#[test]
fn the_history_help_lines() {
    let l = clip_box(vec![clip_row(1, "milk", "milk", 0, false)]);
    assert_eq!(help_text(&l), "enter copy · ctrl+enter pin · del forget · esc close");
    let mut l = launcher();
    l.clipboard = true;
    typed(&mut l, "\"");
    l.set_clips(vec![clip_row(1, "milk", "milk", 0, false)]);
    assert_eq!(help_text(&l), "enter copy · ctrl+enter pin · del forget · esc close");
}

#[test]
fn the_prefix_in_all_fills_the_panel_with_the_whole_preview() {
    let mut l = launcher();
    l.clipboard = true;
    l.set_clips(vec![clip_row(1, "alpha beta gamma delta", "alpha", 3, false)]);
    typed(&mut l, "\"");
    let (b, rows) = draw(&mut l, 120);
    assert!(b.preview_columns > 0);
    let lines: Vec<String> = panel(&rows, &b).iter().map(|t| t.trim().to_string()).collect();
    assert!(lines.iter().any(|t| t.contains("alpha beta gamma delta")), "{lines:?}");
    assert!(lines.iter().any(|t| t == "enter  copy") && lines.iter().any(|t| t == "ctrl+enter  pin"), "{lines:?}");
    // The Clear row's panel says what it does.
    l.selected = 1;
    let (b, rows) = draw(&mut l, 120);
    assert!(panel(&rows, &b).iter().any(|t| t.contains("enter  clear")), "{:?}", panel(&rows, &b));
}

#[test]
fn the_clipboard_mode_has_its_placeholder_until_something_is_typed() {
    let mut l = clip_box(vec![clip_row(1, "milk", "milk", 0, false)]);
    let (b, rows) = draw(&mut l, 100);
    let input = rows.rows[usize::from(b.list_top) - 2].text();
    assert!(input.contains("type to filter the clipboard history"), "{input}");
    typed(&mut l, "m");
    let (b, rows) = draw(&mut l, 100);
    assert!(!rows.rows[usize::from(b.list_top) - 2].text().contains("type to filter"));
}

#[test]
fn the_panel_wipes_its_text_temporaries_and_the_row_text_is_borrowed() {
    let src = include_str!("../view.rs");
    let render = src.split("pub fn render(").nth(1).and_then(|s| s.split("\n}\n").next()).unwrap();
    assert!(render.contains("wipe(&mut cut)") && render.contains("wipe(text)"), "render must wipe the panel's temporaries");
    let describe = src.split("fn describe(").nth(1).and_then(|s| s.split("\n}\n").next()).unwrap();
    assert!(describe.contains("Cow::Borrowed(c.first.as_str())"), "a history row's text is borrowed, never cloned");
}

// -- The Tools menu --

const TOOLS_CFG: &str = r#"
[tools]
builtins = false
[[tools.category]]
name = "Nix"
icon = "N"
[[tools.entry]]
name = "Update"
category = "Nix"
icon = "U"
run = ["x", "update"]
state = ["s"]
[[tools.entry]]
name = "Collect garbage"
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
category = "Nix"
run = ["x", "{choice}"]
choices_cmd = ["lines"]
[[tools.entry]]
name = "Lock now"
icon = "L"
run = ["x", "lock"]
"#;

fn tools_box(text: &str) -> Launcher {
    use crate::launcher::tools::catalog::{Catalog, Context};
    let config = LauncherConfig::from_text(text).unwrap();
    let catalog = Catalog::build(&config.tools, &Context::default(), &mut |_| {});
    Launcher::for_tools(config, catalog, History::empty(200), NOW)
}

fn tool_text(rows: &Boxed) -> Vec<String> {
    rows.rows.iter().map(Grid::text).collect()
}

#[test]
fn tools_root_snapshot() {
    let mut l = tools_box(TOOLS_CFG);
    let (b, rows) = draw(&mut l, 100);
    assert_eq!(b.list_top, 2, "no modes row");
    assert!(rows.rows[0].text().contains("Tools"), "the breadcrumb is the prompt");
    insta::assert_snapshot!("tools_root", snapshot(&rows));
}

#[test]
fn tools_category_snapshot_with_switch_and_chevron() {
    let mut l = tools_box(TOOLS_CFG);
    l.key(Key::Down); // Nix (after "Lock now")
    l.key(Key::Enter);
    let update = l.tools().unwrap().catalog.entries.iter().position(|e| e.name == "Update").unwrap();
    l.adopt_state(update, Some(true));
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("tools_category", snapshot_with_shapes(&rows));
    l.adopt_state(update, Some(false));
    let (_, rows) = draw(&mut l, 100);
    assert!(tool_text(&rows).iter().any(|t| t.contains("Update") && t.contains("○ off")), "{}", snapshot(&rows));
    l.adopt_state(update, None);
    let (_, rows) = draw(&mut l, 100);
    assert!(!tool_text(&rows).iter().any(|t| t.contains("off ") || t.contains("ON")), "an unknown state shows no switch");
}

#[test]
fn the_prompt_shows_its_label_the_typed_text_and_its_own_hint() {
    let cfg = "[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Ping\"\nrun = [\"ping\", \"{text}\"]\nask = \"Host to ping\"\n";
    let mut l = tools_box(cfg);
    l.key(Key::Enter);
    typed(&mut l, "10.0.0.1");
    let (_, rows) = draw(&mut l, 100);
    let text = tool_text(&rows);
    assert!(text[0].contains("Tools › Ping") && text[0].contains("10.0.0.1"), "breadcrumb and typed text: {text:?}");
    assert!(text.iter().any(|t| t.contains("Host to ping")), "the label is the one row: {text:?}");
    assert!(text.iter().any(|t| t.contains("⏎  Run") && t.contains("Esc  Back")), "the prompt's own hint: {text:?}");
}

#[test]
fn tools_flat_search_snapshot() {
    let mut l = tools_box(TOOLS_CFG);
    typed(&mut l, "gen");
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("tools_flat", snapshot(&rows));
    // The category is dim at the right; a choice comes right after its entry.
    let text = tool_text(&rows);
    let at = text.iter().position(|t| t.contains("Generation ") && !t.contains(" · ")).unwrap_or_else(|| panic!("{text:?}"));
    assert!(text[at].trim_end().ends_with("Nix"), "{text:?}");
    assert!(text[at + 1].contains("Generation · list") && text[at + 1].trim_end().ends_with("Nix"), "{text:?}");
}

#[test]
fn tools_breadcrumb_is_cut_from_the_left() {
    let long = "Category with a rather long name indeed";
    let text = format!(
        "[tools]\nbuiltins = false\n[[tools.category]]\nname = {long:?}\n[[tools.entry]]\nname = \"An entry with a long name too\"\ncategory = {long:?}\nrun = [\"x\", \"{{choice}}\"]\nchoices = [\"a\"]\n"
    );
    let mut l = tools_box(&text);
    keys(&mut l, &[Key::Enter, Key::Enter]);
    let (b, rows) = draw(&mut l, 100);
    let input = rows.rows[usize::from(b.list_top) - 2].text();
    assert!(input.contains("…") && input.contains("entry with a long name too"), "{input:?}");
    assert!(!input.contains("Tools ›"), "cut from the left, the end kept: {input:?}");
    insta::assert_snapshot!("tools_breadcrumb_cut", snapshot(&rows));
    // And short breadcrumbs are whole.
    let mut l = tools_box(TOOLS_CFG);
    keys(&mut l, &[Key::Down, Key::Enter]);
    let (b, rows) = draw(&mut l, 100);
    assert!(rows.rows[usize::from(b.list_top) - 2].text().contains("Tools › Nix"));
}

#[test]
fn tools_question_snapshot() {
    let mut l = tools_box(TOOLS_CFG);
    keys(&mut l, &[Key::Down, Key::Enter, Key::Down]); // Collect garbage
    l.set_now(std::time::Instant::now());
    assert_eq!(l.key(Key::Enter), crate::launcher::model::Outcome::Asked);
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("tools_question", snapshot(&rows));
    assert!(tool_text(&rows).iter().any(|t| t.contains("Collect garbage? ⏎")));
}

#[test]
fn tools_reading_snapshot() {
    let mut l = tools_box(TOOLS_CFG);
    keys(&mut l, &[Key::Down, Key::Enter]);
    select_row(&mut l, "Pick");
    l.key(Key::Enter);
    let (b, rows) = draw(&mut l, 100);
    assert!(l.results.is_empty(), "not selectable");
    assert!(tool_text(&rows).iter().any(|t| t.contains("reading…")));
    insta::assert_snapshot!("tools_reading", snapshot(&rows));
    // Done and empty: "nothing here"; a typed query with no hit: "no match".
    let Some(t) = l.tools() else { panic!() };
    let _ = (t, b);
}

#[test]
fn tools_empty_level_says_nothing_here_and_a_missed_search_no_match() {
    let mut l = tools_box("[tools]\nbuiltins = false\n");
    let (_, rows) = draw(&mut l, 100);
    assert!(tool_text(&rows).iter().any(|t| t.contains("nothing here")), "{}", snapshot(&rows));
    let mut l = tools_box(TOOLS_CFG);
    typed(&mut l, "zzzz");
    let (_, rows) = draw(&mut l, 100);
    assert!(tool_text(&rows).iter().any(|t| t.contains("no match")), "{}", snapshot(&rows));
}

#[test]
fn tools_help_rows() {
    let mut l = tools_box(TOOLS_CFG);
    let (_, rows) = draw(&mut l, 100);
    assert!(tool_text(&rows).iter().any(|t| t.contains("⏎  Open    Esc  Close")), "{}", snapshot(&rows));
    assert!(!tool_text(&rows).iter().any(|t| t.contains("⌫") || t.contains("Back")), "← does nothing at the root");
    keys(&mut l, &[Key::Down, Key::Enter]);
    let (_, rows) = draw(&mut l, 100);
    assert!(tool_text(&rows).iter().any(|t| t.contains("⏎  Open    ←   Esc  Back")), "{}", snapshot(&rows));
    assert!(!tool_text(&rows).iter().any(|t| t.contains("Close")));
}

#[test]
fn the_help_row_of_an_apps_origin_root_offers_backspace_to_all() {
    use crate::launcher::tools::catalog::{Catalog, Context};
    let config = LauncherConfig::from_text(TOOLS_CFG).unwrap();
    let catalog = Catalog::build(&config.tools, &Context::default(), &mut |_| {});
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), NOW);
    l.set_tools(catalog);
    typed(&mut l, "/");
    let (_, rows) = draw(&mut l, 100);
    assert!(tool_text(&rows).iter().any(|t| t.contains("⏎  Open    ⌫  All    Esc  Close")), "{}", snapshot(&rows));
}

#[test]
fn a_switch_row_cuts_a_long_name_before_the_switch() {
    let name = "A very long entry name that cannot fit at all";
    let text = format!("[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"{name}\"\nrun = [\"x\"]\nstate = [\"s\"]\n");
    let mut l = tools_box(&text);
    l.adopt_state(0, Some(true));
    let (_, rows) = draw(&mut l, 40);
    let line = tool_text(&rows).into_iter().find(|t| t.contains("A very")).unwrap_or_default();
    let (switch, _) = quick::switch(Some(true));
    assert!(line.contains(&format!("…  {switch}")) || line.contains(&format!("… {switch}")), "{line:?}");
    assert!(!line.contains("all"), "the name is cut, not drawn over the switch: {line:?}");
}

#[test]
fn a_click_on_a_tools_row_maps_to_it() {
    let mut l = tools_box(TOOLS_CFG);
    let (b, _) = draw(&mut l, 100);
    assert_eq!(hit(&l, &b, 5, b.list_top + 1), Some(Click::Result(1)));
    assert_eq!(hit(&l, &b, 5, b.list_top + 9), Some(Click::Inside), "below the last row");
}

/// Selects the row named `name` (the model's keys only).
fn select_row(l: &mut Launcher, name: &str) {
    let at = (0..l.results.len())
        .find(|&i| matches!(l.results[i].source, Source::Tool(r) if l.tool_rows()[r].text == name))
        .unwrap_or_else(|| panic!("{name}"));
    l.key(Key::Home);
    for _ in 0..at {
        l.key(Key::Down);
    }
}

// --- the kill list ---

fn kill_box() -> Launcher {
    use crate::launcher::processes::ProcRow;
    let row = |pid, name: &str, cmdline: &str, rss_kb, cpu_pct| ProcRow {
        pid,
        start: 1,
        name: name.into(),
        cmdline: cmdline.into(),
        rss_kb,
        cpu_pct,
    };
    Launcher::for_processes(
        LauncherConfig::default(),
        vec![
            row(4242, "firefox", "/nix/store/x-firefox/bin/firefox --new-window", 151_552, Some(12.3)),
            row(77, "sleep", "sleep 600", 640, None),
            row(9, "browser", "", 2_097_152, Some(0.0)),
        ],
    )
}

fn row_texts(rows: &Boxed, b: &BoxLayout) -> Vec<String> {
    let top = usize::from(b.list_top) - 1;
    rows.rows[top..top + 3].iter().map(Grid::text).collect()
}

#[test]
fn a_kill_list_row_reads_name_command_line_pid_cpu_and_memory() {
    let mut l = kill_box();
    let (b, rows) = draw(&mut l, 100);
    assert!(!l.has_modes() && l.is_bare(), "no modes row and no preview panel");
    let texts = row_texts(&rows, &b);
    let first = &texts[0];
    assert!(first.contains("firefox") && first.contains("/nix/store/x-firefox"), "{first:?}");
    assert!(first.contains("4242") && first.contains("12.3 %") && first.contains("148 MB"), "{first:?}");
    assert!(
        texts[1].contains("sleep 600") && texts[1].contains("77") && texts[1].contains("-") && texts[1].contains("640 KB"),
        "{:?}",
        texts[1]
    );
    assert!(texts[2].contains("2.0 GB") && texts[2].contains("0.0 %"), "{:?}", texts[2]);
}

#[test]
fn the_command_line_is_dim_on_the_row_and_the_stats_too() {
    let mut l = kill_box();
    l.key(Key::Down);
    let (b, rows) = draw(&mut l, 100);
    let top = usize::from(b.list_top) - 1;
    let styles = rows.rows[top].styles();
    // Row 0 is not selected: the name is normal text, the command line and the stats are muted.
    let text = rows.rows[top].text();
    let at = text.find("/nix/store").unwrap();
    let name_at = text.find("firefox").unwrap();
    let style_at = |col: usize| styles.chars().nth(col).unwrap();
    assert_ne!(style_at(name_at), style_at(at), "the command line has its own, dimmer style");
}

#[test]
fn a_force_kill_question_replaces_the_row_and_the_help_line() {
    let mut l = kill_box();
    l.set_now(std::time::Instant::now());
    assert_eq!(l.key(Key::CtrlEnter), crate::launcher::model::Outcome::Asked);
    let (b, rows) = draw(&mut l, 100);
    let texts = row_texts(&rows, &b);
    assert!(texts[0].contains("Force kill firefox? Enter again"), "{:?}", texts[0]);
    assert!(texts[1].contains("sleep 600"), "the other rows stay");
    let help = rows.rows[usize::from(b.list_top) - 1 + usize::from(b.list_rows)].text();
    assert!(help.contains("enter again to force kill"), "{help:?}");
}

#[test]
fn the_kill_list_has_its_own_help_empty_states_and_a_filter_hint() {
    let mut l = kill_box();
    assert_eq!(help_text(&l), "enter quit · ctrl+enter force kill · esc close");
    let (b, rows) = draw(&mut l, 100);
    assert!(rows.rows[usize::from(b.list_top) - 2].text().contains("type to filter the processes"));
    typed(&mut l, "zzzz");
    let (b, rows) = draw(&mut l, 100);
    assert!(rows.rows[usize::from(b.list_top) - 1].text().contains("no match"));
    let empty = Launcher::for_processes(LauncherConfig::default(), vec![]);
    assert_eq!(help_text(&empty), "esc close");
    let mut empty = empty;
    let (b, rows) = draw(&mut empty, 100);
    assert!(rows.rows[usize::from(b.list_top) - 1].text().contains("no process"));
}

#[test]
fn the_kill_list_fits_the_narrowest_box() {
    let mut l = kill_box();
    for columns in [MIN_COLUMNS, 30, 40] {
        let _ = draw(&mut l, columns);
    }
    let limit = usize::from(layout(200, 40, l.config.rows, true, false).pane_end().saturating_sub(3));
    for text in ["enter again to force kill · any other key cancels", "enter quit · ctrl+enter force kill · esc close"] {
        assert!(text.chars().count() <= limit, "{text:?} is {} chars, limit {limit}", text.chars().count());
    }
}

#[test]
fn a_hostile_process_name_never_misaligns_the_box() {
    use crate::launcher::processes::ProcRow;
    let hostile = ProcRow {
        pid: 5,
        start: 1,
        name: "bad\u{1b}[31m\u{200b}name日本".into(),
        cmdline: "a\tb\u{7}c".into(),
        rss_kb: 1,
        cpu_pct: Some(1.0),
    };
    let mut l = Launcher::for_processes(LauncherConfig::default(), vec![hostile]);
    let (_, rows) = draw(&mut l, 60);
    assert_no_raw_hostile_characters(&rows);
}

#[test]
fn a_signalled_row_is_dim_and_says_what_it_was_sent() {
    use crate::launcher::processes::Sig;
    let mut l = kill_box();
    l.signal_result(4242, Sig::Term, Ok(()));
    let (b, rows) = draw(&mut l, 100);
    let texts = row_texts(&rows, &b);
    assert!(texts[0].contains("firefox") && texts[0].contains("asked to quit"), "{:?}", texts[0]);
    assert!(!texts[0].contains("/nix/store"), "the marked row drops its stats and command line: {:?}", texts[0]);
    l.signal_result(77, Sig::Kill, Ok(()));
    let (b, rows) = draw(&mut l, 100);
    assert!(row_texts(&rows, &b)[1].contains("killed"));
}

#[test]
fn with_nothing_selected_no_row_is_highlighted() {
    let mut l = kill_box();
    l.key(Key::Down);
    let (b, rows) = draw(&mut l, 100);
    let top = usize::from(b.list_top) - 1;
    let selected_cells = |r: &Grid| r.styles().chars().filter(|c| *c == Style::SELECTED.code()).count();
    assert!(selected_cells(&rows.rows[top + 1]) > 0, "the second row is highlighted");
    use crate::launcher::processes::ProcRow;
    let row = |pid, name: &str| ProcRow { pid, start: 1, name: name.into(), cmdline: String::new(), rss_kb: 1, cpu_pct: Some(0.0) };
    l.set_procs(vec![row(4242, "firefox"), row(9, "browser")]);
    assert!(l.selection_lost());
    let (b, rows) = draw(&mut l, 100);
    let top = usize::from(b.list_top) - 1;
    assert!((0..2).all(|i| selected_cells(&rows.rows[top + i]) == 0), "no highlight when nothing is selected");
}

// --- the Super+X panel's root (today's rows until the panel's own) ---

/// Every built-in shown but "Stop recording".
fn every_builtin() -> crate::launcher::tools::catalog::Context {
    crate::launcher::tools::catalog::Context {
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
        recording: false,
        annotate: true,
        ocr: true,
        record: true,
    }
}

/// A `--tools` open of `text`'s launcher.toml with every built-in, its panel opened in the room a
/// 232×49 output leaves, and the box `tools_layout` sizes for it.
fn panel_box(text: &str) -> (Launcher, BoxLayout) {
    use crate::launcher::tools::catalog::Catalog;
    let config = LauncherConfig::from_text(text).unwrap();
    let catalog = Catalog::build(&config.tools, &every_builtin(), &mut |_| {});
    let mut l = Launcher::for_tools(config, catalog, History::empty(200), NOW);
    let (columns, rows) = panel_room(232, 49);
    l.open_panel(columns, rows);
    let b = tools_layout(232, 49, l.panel_size().expect("the panel is opened"));
    l.set_list_rows(model_rows(&l, &b));
    (l, b)
}

/// Wi-Fi and the USB reset pinned at the root, where their capsules show.
const PINNED: &str = "[tools]\nroot = [\"quick.wifi\", \"diag.usb_reset\"]\n";

/// The letter capsules on the list's rows: not the search field's, not the footer's.
fn capsules(b: &Boxed) -> Vec<&Shape> {
    b.shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Chip(Tint::Pill) && s.width == 3 && s.rows == 1 && s.row > 0 && s.row + 1 < b.rows.len())
        .collect()
}

/// Renders `l` in `b`, every row as wide as the box less its ring.
fn draw_in(l: &Launcher, b: &BoxLayout) -> Boxed {
    let rows = render(l, b);
    for (i, r) in rows.rows.iter().enumerate() {
        assert_eq!(r.width(), b.columns - 2, "row {i} of {}", snapshot(&rows));
    }
    rows
}

/// The snapshot, the separators, then the shapes one per line.
fn snapshot_with_shapes(b: &Boxed) -> String {
    let shapes: Vec<String> = b
        .shapes
        .iter()
        .map(|s| {
            let glyph = s.glyph.map_or_else(String::new, |(c, _, ink)| format!(" glyph {c:?} in {ink:?}"));
            format!("{:?} at row {} col {} width {} rows {}{glyph}", s.kind, s.row, s.col, s.width, s.rows)
        })
        .collect();
    format!("{}\nseparators: {:?}\n\nshapes:\n{}", snapshot(b), b.separators, shapes.join("\n"))
}

// --- the panel drawn (spec "Layout") ---

/// The list pane's tool tiles: 2 × 3 pill chips (a card's tile is right of the list).
fn tiles(b: &Boxed) -> Vec<&Shape> {
    b.shapes.iter().filter(|s| s.kind == ShapeKind::Chip(Tint::Pill) && s.width == 3 && s.rows == 2).collect()
}

fn switches(b: &Boxed) -> Vec<&Shape> {
    b.shapes.iter().filter(|s| matches!(s.kind, ShapeKind::Switch(_))).collect()
}

/// The detail card's shape.
fn card(b: &Boxed) -> &Shape {
    b.shapes.iter().find(|s| s.kind == ShapeKind::Card).unwrap_or_else(|| panic!("a detail card: {}", snapshot_with_shapes(b)))
}

/// The text of the card's rows, cut to its columns.
fn card_text(b: &Boxed) -> Vec<String> {
    let c = card(b);
    b.rows[c.row..c.row + usize::from(c.rows)]
        .iter()
        .map(|r| r.text().chars().skip(usize::from(c.col)).take(usize::from(c.width)).collect::<String>().trim().to_string())
        .collect()
}

/// The text of every row left of the detail card: the list pane and what is above and below it.
fn list_text(b: &Boxed) -> Vec<String> {
    let c = card(b);
    b.rows.iter().map(|r| r.text().chars().take(usize::from(c.col)).collect()).collect()
}

/// Selects the first row of the open tab showing `name`.
fn select_tool(l: &mut Launcher, name: &str) {
    let e = entry_named(l, name);
    let at = l.tools().unwrap().panel().unwrap().tools().position(|t| t == e).unwrap_or_else(|| panic!("{name} in the tab"));
    while l.selected != at {
        l.key(if l.selected < at { Key::Down } else { Key::Up });
    }
}

/// The glyphs the tiles of `b` carry (the list's, then the card's), and whether every tile's cells
/// are blank: the painter draws the glyph, centred in the shape.
fn tile_glyphs(b: &Boxed) -> (Vec<Option<char>>, bool) {
    let tiles = tiles(b);
    let blank =
        tiles.iter().all(|t| b.rows[t.row..t.row + 2].iter().all(|r| r.text().chars().skip(usize::from(t.col)).take(3).all(|c| c == ' ')));
    (tiles.iter().map(|t| t.glyph.map(|(c, ..)| c)).collect(), blank)
}

#[test]
fn a_user_entry_without_icon_shows_its_category_s_icon() {
    let cfg = "[tools]\nbuiltins = false\n[[tools.category]]\nname = \"Nix\"\nicon = \"N\"\n[[tools.entry]]\nname = \"Rebuild\"\ncategory = \"Nix\"\nrun = [\"x\"]\n";
    let (l, b) = panel_box(cfg);
    let rows = draw_in(&l, &b);
    let (glyphs, blank) = tile_glyphs(&rows);
    assert_eq!(glyphs, [Some('N'), Some('N')], "the list's tile and the card's: {}", snapshot_with_shapes(&rows));
    assert!(blank, "the tiles' cells stay blank: {}", snapshot_with_shapes(&rows));
}

#[test]
fn an_entry_s_icon_falls_back_to_its_category_s_then_the_generic_glyph() {
    use crate::launcher::tools::catalog::FOLDER;
    let fallbacks = |cfg: &str| {
        let (l, b) = panel_box(cfg);
        let rows = draw_in(&l, &b);
        tiles(&rows).iter().map(|t| t.glyph.map(|(_, f, _)| f)).collect::<Vec<_>>()
    };
    let nix = "[tools]\nbuiltins = false\n[[tools.category]]\nname = \"Nix\"\nicon = \"N\"\n[[tools.entry]]\nname = \"Fire\"\nicon = \"🔥\"\ncategory = \"Nix\"\nrun = [\"x\"]\n";
    assert_eq!(fallbacks(nix), [Some('N'), Some('N')], "the category's icon behind the entry's");
    let root = "[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Fire\"\nicon = \"🔥\"\nrun = [\"x\"]\n";
    assert_eq!(fallbacks(root), [Some(FOLDER), Some(FOLDER)], "no category: the generic glyph");
}

#[test]
fn a_user_entry_without_icon_or_category_shows_the_generic_glyph() {
    use crate::launcher::tools::catalog::FOLDER;
    let (l, b) = panel_box("[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Rebuild\"\nrun = [\"x\"]\n");
    let rows = draw_in(&l, &b);
    let (glyphs, blank) = tile_glyphs(&rows);
    assert_eq!(glyphs, [Some(FOLDER), Some(FOLDER)], "the list's tile and the card's: {}", snapshot_with_shapes(&rows));
    assert!(blank, "the tiles' cells stay blank: {}", snapshot_with_shapes(&rows));
}

#[test]
fn panel_home_snapshot() {
    let (l, b) = panel_box("");
    assert_eq!((b.columns, b.rows), (106, 30), "the panel's box on a wide output");
    let rows = draw_in(&l, &b);
    let text = tool_text(&rows);
    assert!(text[0].contains("Search tools"), "the search field first: {text:#?}");
    assert!(text[1].contains(" All ") && text[1].contains(" Captures "), "the tabs: {text:#?}");
    assert!(text[3].contains("● QUICK SETTINGS"), "no root tool: the first category's label first: {text:#?}");
    insta::assert_snapshot!("panel_home", snapshot_with_shapes(&rows));
}

/// `l` drawn in `b` and painted at `scale` with the default theme, in a surface sized as the host
/// sizes it (`surface::box_frame`, `surface::layout_size`): the pixels (r, g, b), the width, the
/// height, the frame as painted, and the x span of the search field's cursor's cell.
fn painted(
    fonts: &mut crate::painter::Fonts,
    l: &Launcher,
    b: &BoxLayout,
    scale: i32,
) -> (Vec<Rgb>, u32, u32, crate::painter::Frame, (u32, u32)) {
    use crate::config::DEFAULT_HEIGHT;
    use crate::launcher::surface::{box_frame, layout_size};
    let rows = draw_in(l, b);
    let (e, roles) = (scale as u32, Roles::default());
    let rh = DEFAULT_HEIGHT * e;
    let advance = fonts.advance(scale);
    let frame = box_frame(fonts, scale, l);
    let (w, h) = layout_size(b, advance, rh, scale, &frame);
    let (w, h) = (w * e, h * e);
    let mut px = vec![0u8; (w * h * 4) as usize];
    fonts.paint_boxed(&rows, &roles, &mut px, w, h, rh, scale, FrameInk::Border);
    let frame = frame.fitted(rows.cells(), advance, rh, (w, h));
    let col = rows.rows[0].text().chars().position(|c| c == '▏').expect("the search field's cursor") as f32;
    let cursor = (frame.side + (col * advance).round() as u32, frame.side + ((col + 1.0) * advance).round() as u32);
    (px.chunks(4).map(|p| Rgb(p[2], p[1], p[0])).collect(), w, h, frame, cursor)
}

/// The panel's outermost content (the search field's chip at the top, the footer's capsules at
/// the bottom, the chips and the band one cell in at the sides) is as far from the frame on all
/// four sides, within a pixel, at both scales: the frame's margins are measured to shapes there,
/// not to letters (the project's rule: the same visual margin on every side).
#[test]
fn the_panel_s_content_is_as_far_from_the_frame_on_every_side() {
    use crate::config::{DEFAULT_HEIGHT, FontConfig};
    let (mut fonts, _) = crate::painter::Fonts::load(&FontConfig::default(), DEFAULT_HEIGHT).expect("a monospace font (nix develop)");
    let roles = Roles::default();
    let (bg, line) = (roles.popup_bg, roles.border_over(roles.popup_bg));
    for scale in [1, 2] {
        let (l, b) = panel_box(PINNED);
        let (px, w, h, frame, cursor) = painted(&mut fonts, &l, &b, scale);
        // Neither the surface nor a line (the frame's stroke, the separators run up to it).
        let content = |x: u32, y: u32| ![bg, line].contains(&px[(y * w + x) as usize]);
        let (st, r) = (frame.stroke, frame.radius);
        // The cursor's bar is the font's full line, taller than the field's chip: not a margin.
        let xs: Vec<u32> = (st + r..w - st - r).filter(|x| !(cursor.0..cursor.1).contains(x)).collect();
        let ys = st + r..h - st - r;
        let left = ys.clone().filter_map(|y| (st..w).find(|&x| content(x, y))).min().unwrap() - st;
        let right = (w - st - 1) - ys.filter_map(|y| (0..w - st).rev().find(|&x| content(x, y))).max().unwrap();
        let top = xs.iter().filter_map(|&x| (st..h).find(|&y| content(x, y))).min().unwrap() - st;
        let bottom = (h - st - 1) - xs.iter().filter_map(|&x| (0..h - st).rev().find(|&y| content(x, y))).max().unwrap();
        let margins = [left, right, top, bottom];
        let (lo, hi) = (margins.iter().min().unwrap(), margins.iter().max().unwrap());
        assert!(hi - lo <= 1, "scale {scale}: left {left}, right {right}, top {top}, bottom {bottom} px");
        assert!(*lo > 4 * scale as u32, "scale {scale}: a real margin, not the stroke ({lo} px)");
    }
}

/// Mod+Space's box (and every box but a `--tools` open's) keeps the frame measured to its letters.
#[test]
fn only_a_tools_open_measures_its_frame_to_shapes() {
    use crate::config::{DEFAULT_HEIGHT, FontConfig};
    use crate::launcher::surface::box_frame;
    let (mut fonts, _) = crate::painter::Fonts::load(&FontConfig::default(), DEFAULT_HEIGHT).expect("a monospace font (nix develop)");
    let mut apps = launcher();
    let config = LauncherConfig::from_text(TOOLS_CFG).unwrap();
    apps.set_tools(crate::launcher::tools::catalog::Catalog::build(&config.tools, &every_builtin(), &mut |_| {}));
    typed(&mut apps, "/");
    assert_eq!(apps.mode, Mode::Tools, "the `/` jump");
    for scale in [1, 2] {
        let text = fonts.frame(scale);
        assert_eq!(box_frame(&mut fonts, scale, &launcher()), text, "scale {scale}: Mod+Space");
        assert_eq!(box_frame(&mut fonts, scale, &apps), text, "scale {scale}: Mod+Space's Tools jump");
        assert_ne!(box_frame(&mut fonts, scale, &panel_box("").0), text, "scale {scale}: the panel");
    }
    assert_eq!(render(&launcher(), &layout(100, 40, 8, false, true)).edge, Edge::Text);
    let (l, b) = panel_box("");
    assert_eq!(render(&l, &b).edge, Edge::Shapes);
}

#[test]
fn a_category_tab_snapshot() {
    let (mut l, b) = panel_box("");
    l.key(Key::Tab);
    let rows = draw_in(&l, &b);
    let active: Vec<&Shape> = rows.shapes.iter().filter(|s| s.kind == ShapeKind::Chip(Tint::AccentContainer)).collect();
    assert_eq!(active.len(), 1, "one active tab: {}", snapshot_with_shapes(&rows));
    assert!(!tool_text(&rows).iter().any(|t| t.contains("PINNED")), "no label on a category tab");
    insta::assert_snapshot!("panel_category", snapshot_with_shapes(&rows));
}

#[test]
fn the_detail_card_of_a_toggle_a_confirm_and_a_choices_tool() {
    let (mut l, b) = panel_box(PINNED);
    // Do not disturb: a toggle with a letter, in its category's section.
    select_tool(&mut l, "Do not disturb");
    let rows = draw_in(&l, &b);
    let text = card_text(&rows);
    assert!(text.iter().any(|t| t == "Do not disturb"), "the name: {text:#?}");
    assert!(text.iter().any(|t| t.starts_with("Quick settings · built-in")), "its category, dim: {text:#?}");
    assert!(text.iter().any(|t| t.starts_with("Turn on/off") && t.ends_with('⏎')), "{text:#?}");
    assert!(text.iter().any(|t| t.starts_with("Quick key") && t.ends_with('d')), "{text:#?}");
    assert!(!text.iter().any(|t| t == "Pinned"), "not pinned: {text:#?}");
    let dnd = entry_named(&l, "Do not disturb");
    l.adopt_state(dnd, Some(true));
    assert!(card_text(&draw_in(&l, &b)).iter().any(|t| t.starts_with("Turn off")), "the state known, the word follows it");
    let name = rows.rows.iter().position(|r| r.text().contains("Turn on/off")).unwrap();
    let c = card(&rows);
    let keys: Vec<&Shape> = rows.shapes.iter().filter(|s| s.row == name && matches!(s.kind, ShapeKind::Chip(_)) && s.col > c.col).collect();
    assert_eq!(keys.len(), 1, "one key capsule: {}", snapshot_with_shapes(&rows));
    assert_eq!(keys[0].col + keys[0].width, c.col + c.width - 2, "at the card's right, inside its padding");
    // Reset USB: a confirm.
    select_tool(&mut l, "Reset USB (xHCI)");
    let rows = draw_in(&l, &b);
    let text = card_text(&rows);
    let confirm = rows.rows.iter().position(|r| r.text().contains("Confirm")).unwrap_or_else(|| panic!("{text:#?}"));
    let keys = rows.shapes.iter().filter(|s| s.row == confirm && matches!(s.kind, ShapeKind::Chip(_)) && s.col > card(&rows).col).count();
    assert_eq!(keys, 2, "Enter twice: {}", snapshot_with_shapes(&rows));
    assert!(text.iter().any(|t| t == "Pinned"), "the hint of a pinned tool: {text:#?}");
    // Schedule shutdown: choices.
    let (mut l, b) = panel_box("");
    l.key(Key::Tab);
    while !l.tools().unwrap().panel().unwrap().tools().any(|t| t == entry_named(&l, "Schedule shutdown")) {
        l.key(Key::Tab);
    }
    select_tool(&mut l, "Schedule shutdown");
    let rows = draw_in(&l, &b);
    let text = card_text(&rows);
    assert!(text.iter().any(|t| t.starts_with("Choose") && t.ends_with('→')), "{text:#?}");
    assert!(!text.iter().any(|t| t.starts_with("Run")), "choosing is what Enter does: {text:#?}");
    assert!(!text.iter().any(|t| t == "Pinned"), "not pinned: {text:#?}");
}

/// The card's rhythm (the P1 render): its tile, the name right under it, the category, the
/// description, the line, the actions; no blank line between them.
#[test]
fn the_detail_card_stacks_its_lines_without_gaps() {
    let (mut l, b) = panel_box(PINNED);
    select_tool(&mut l, "Night light");
    let rows = draw_in(&l, &b);
    let (c, text) = (card(&rows), card_text(&rows));
    let tile = rows
        .shapes
        .iter()
        .find(|s| s.rows == 2 && s.col > c.col)
        .unwrap_or_else(|| panic!("the card's tile: {}", snapshot_with_shapes(&rows)));
    let first = tile.row + 2 - c.row;
    let lines: Vec<&str> = text[first..first + 5].iter().map(String::as_str).collect();
    assert_eq!(lines[0], "Night light", "the name right under the tile: {text:#?}");
    assert_eq!(lines[1], "Quick settings · built-in", "{text:#?}");
    assert_eq!(lines[2], "Turn warmer screen colours on or off", "{text:#?}");
    assert!(lines[3].chars().all(|c| c == '─') && !lines[3].is_empty(), "the line under the description: {text:#?}");
    assert!(lines[4].starts_with("Turn on/off"), "then the actions: {text:#?}");
}

#[test]
fn the_band_covers_both_lines_of_a_row() {
    let (mut l, b) = panel_box(PINNED);
    select_tool(&mut l, "Reset USB (xHCI)");
    let rows = draw_in(&l, &b);
    let bands: Vec<&Shape> = rows.shapes.iter().filter(|s| s.kind == ShapeKind::Band).collect();
    assert_eq!(bands.len(), 1, "{}", snapshot_with_shapes(&rows));
    let band = bands[0];
    assert_eq!(band.rows, 2, "both lines: {band:?}");
    assert!(rows.rows[band.row].text().contains("Reset USB"), "the name on its first line");
    let description = l.tools().unwrap().catalog.entries[entry_named(&l, "Reset USB (xHCI)")].description.clone().unwrap();
    assert!(rows.rows[band.row + 1].text().contains(description.as_ref()), "the description on its second");
    // Under the row's tile and capsule, in the shapes' order.
    let at = rows.shapes.iter().position(|s| std::ptr::eq(s, band)).unwrap();
    let tile = rows.shapes.iter().position(|s| tiles(&rows).iter().any(|t| std::ptr::eq(s, *t)) && s.row == band.row).unwrap();
    assert!(at < tile, "the tile lies on the band");
    // Every cell of the row keeps the surface's background: the band shows through.
    for r in [band.row, band.row + 1] {
        assert!(rows.rows[r].cells().iter().all(|c| c.style.bg == Tint::Bg), "row {r}");
    }
}

/// Sixteen user categories, one entry each.
fn many_categories() -> String {
    let mut text = String::from("[tools]\nbuiltins = false\n");
    for k in 0..16 {
        text.push_str(&format!("[[tools.category]]\nname = \"Category {k:02}\"\n"));
        text.push_str(&format!("[[tools.entry]]\nname = \"Entry {k:02}\"\ncategory = \"Category {k:02}\"\nrun = [\"x\"]\n"));
    }
    text
}

#[test]
fn the_active_tab_stays_visible() {
    let (mut l, b) = panel_box(&many_categories());
    let rows = draw_in(&l, &b);
    let strip = rows.rows[1].text();
    assert!(strip.trim_end().ends_with('›') && !strip.contains('‹'), "cut at the right only: {strip:?}");
    l.key(Key::BackTab);
    let rows = draw_in(&l, &b);
    let strip = rows.rows[1].text();
    assert!(strip.contains(" Category 15 "), "the last tab is drawn: {strip:?}");
    assert!(strip.trim_start().starts_with('‹') && !strip.contains('›'), "cut at the left: {strip:?}");
    let active = rows.shapes.iter().find(|s| s.kind == ShapeKind::Chip(Tint::AccentContainer)).expect("the active pill");
    let label: String = strip.chars().skip(usize::from(active.col)).take(usize::from(active.width)).collect();
    assert_eq!(label, " Category 15 ", "{strip:?}");
    assert!(active.col + active.width <= b.columns - 2);
}

const LATE_STATE_CFG: &str = r#"
[tools]
builtins = false
[[tools.category]]
name = "Local"
[[tools.entry]]
name = "A long tool name that reaches its switch's slot"
category = "Local"
run = ["x"]
state = ["s"]
"#;

#[test]
fn a_late_state_fills_its_slot_without_moving_the_row() {
    let name = "A long tool name that reaches its switch's slot";
    let (mut l, b) = panel_box(LATE_STATE_CFG);
    // A box narrow enough for the name to reach the slot.
    let b = BoxLayout { columns: 90, ..b };
    let e = entry_named(&l, name);
    let before = draw_in(&l, &b);
    assert!(switches(&before).is_empty(), "no switch before the state is in");
    l.adopt_state(e, Some(true));
    let after = draw_in(&l, &b);
    assert_eq!(list_text(&before), list_text(&after), "the name and the letter stay where they were");
    let new = switches(&after);
    assert_eq!(new.len(), 1, "{}", snapshot_with_shapes(&after));
    assert_eq!(new[0].kind, ShapeKind::Switch(true));
    let line = &after.rows[new[0].row];
    let cut = line.text();
    assert!(cut.contains('…'), "the long name is cut before the slot: {cut:?}");
    let cells: String = line.cells()[usize::from(new[0].col)..usize::from(new[0].col) + 2].iter().map(|c| c.ch).collect();
    assert_eq!(cells, "  ", "the switch lies on the reserved blank cells: {cut:?}");
    assert!(tiles(&after).iter().any(|t| t.row == new[0].row), "on the tool's first line");
}

#[test]
fn a_hung_state_leaves_the_slot_empty() {
    let name = "A long tool name that reaches its switch's slot";
    let (mut l, b) = panel_box(LATE_STATE_CFG);
    // A box narrow enough for the name to reach the slot.
    let b = BoxLayout { columns: 90, ..b };
    let e = entry_named(&l, name);
    let before = draw_in(&l, &b);
    l.adopt_state(e, None);
    let after = draw_in(&l, &b);
    assert!(switches(&after).is_empty(), "never a guess");
    assert_eq!(list_text(&before), list_text(&after), "the slot stays blank");
}

/// Every shape inside the cells (columns and rows), at home, on a tab, with many tabs, in a small
/// box and scrolled to the last tool.
#[test]
fn nothing_is_drawn_outside_the_box() {
    let check = |l: &Launcher, b: &BoxLayout| {
        let rows = draw_in(l, b);
        assert_eq!(rows.rows.len(), usize::from(b.rows) - 2);
        for s in &rows.shapes {
            assert!(s.col + s.width <= b.columns - 2, "{s:?} past the last column of {b:?}");
            assert!(s.row + usize::from(s.rows) <= rows.rows.len(), "{s:?} past the last row of {b:?}");
            // The panes' shapes stay above the footer's line.
            let footer = rows.rows.len() - 1;
            assert!(s.row == footer || s.row + usize::from(s.rows) < footer, "{s:?} over the footer's line in {b:?}");
        }
    };
    for text in ["", PINNED, &many_categories()] {
        let (mut l, b) = panel_box(text);
        check(&l, &b);
        l.key(Key::End);
        check(&l, &b);
        l.key(Key::Tab);
        check(&l, &b);
        for (columns, output_rows) in [(80, 30), (70, 20), (232, 14)] {
            let small = tools_layout(columns, output_rows, panel_room(columns, output_rows));
            let mut l = panel_box(text).0;
            l.set_list_rows(model_rows(&l, &small));
            check(&l, &small);
            l.key(Key::End);
            check(&l, &small);
        }
    }
}

#[test]
fn the_box_holds_six_tool_rows_when_the_output_allows() {
    for (columns, output_rows) in [(232, 49), (160, 40), (120, 37)] {
        let room = panel_room(columns, output_rows);
        let b = tools_layout(columns, output_rows, room);
        assert!(b.rows <= output_rows * 8 / 10, "{b:?}");
        // Every section label between the tools: the most a pane can lose to them.
        let (l, _) = panel_box(&many_categories());
        let rows = draw_in(&l, &b);
        assert!(tiles(&rows).len() >= 6, "{columns}×{output_rows}: {}", snapshot_with_shapes(&rows));
    }
    // A short output keeps what fits, inside its 80 %.
    let b = tools_layout(232, 20, panel_room(232, 20));
    assert!(b.rows <= 16, "{b:?}");
}

/// The selection below the pane scrolls the list: the selected tool is drawn whole.
#[test]
fn the_list_scrolls_to_keep_the_selection_whole() {
    let (mut l, b) = panel_box("");
    l.key(Key::End);
    let rows = draw_in(&l, &b);
    let band = rows.shapes.iter().find(|s| s.kind == ShapeKind::Band).expect("the last tool is selected and drawn");
    assert_eq!(band.rows, 2);
    let last = l.tools().unwrap().panel().unwrap().tools().last().unwrap();
    let name = l.tools().unwrap().catalog.entries[last].name.clone();
    assert!(rows.rows[band.row].text().contains(name.as_str()), "{}", snapshot_with_shapes(&rows));
    // A click on its row selects it.
    assert_eq!(hit(&l, &b, 5, u16::try_from(band.row).unwrap() + 2), Some(Click::Result(l.selected)));
}

/// A tool listed at the panel's root shows its letter in a capsule at its row's right, drawn over
/// the selection band; its name is cut before it.
#[test]
fn a_root_tool_shows_its_capsule_over_the_band() {
    let (l, b) = panel_box(PINNED);
    let rows = draw_in(&l, &b);
    let caps = capsules(&rows);
    let reset = tool_text(&rows).iter().position(|t| t.contains("Reset USB")).unwrap();
    assert!(caps.iter().any(|c| c.row == reset), "the reset's: {}", snapshot_with_shapes(&rows));
    let wifi = tool_text(&rows).iter().position(|t| t.contains("Wi-Fi")).unwrap();
    let cap = caps.iter().find(|c| c.row == wifi).unwrap_or_else(|| panic!("on Wi-Fi's row: {}", snapshot_with_shapes(&rows)));
    let line = rows.rows[wifi].text();
    let letter: String = line.chars().skip(usize::from(cap.col)).take(3).collect();
    assert_eq!(letter, " w ", "{line:?}");
    let band = rows.shapes.iter().position(|s| s.kind == ShapeKind::Band && s.row == wifi).expect("Wi-Fi is selected");
    let pane = rows.shapes[band];
    assert_eq!(cap.col + cap.width + 1, pane.col + pane.width, "at the row's right, one cell from the band's edge");
    let at = rows.shapes.iter().position(|s| std::ptr::eq(s, *cap)).unwrap();
    assert!(band < at, "the capsule lies on the band");
    // Not after `/`: the letters are text there.
    let mut l = l;
    l.key(Key::Char('/'));
    assert!(capsules(&draw_in(&l, &b)).is_empty());
}

/// The catalog index of the entry named `name`.
fn entry_named(l: &Launcher, name: &str) -> usize {
    l.tools().unwrap().catalog.entries.iter().position(|e| e.name == name).unwrap()
}

// --- the panel's search (spec "Search") ---

/// The rendered row and cell of `text`'s first character, the first time the list (left of the
/// card) or the tab strip shows it.
fn find(rows: &Boxed, text: &str) -> (usize, usize) {
    list_text(rows)
        .iter()
        .enumerate()
        .find_map(|(y, t)| t.find(text).map(|at| (y, t[..at].chars().count())))
        .unwrap_or_else(|| panic!("{text:?} in {}", snapshot_with_shapes(rows)))
}

/// The chips on rendered row `y`, left to right: (picked, the text over them).
fn chips_on(rows: &Boxed, y: usize) -> Vec<(bool, String)> {
    let mut chips: Vec<&Shape> = rows
        .shapes
        .iter()
        .filter(|s| s.row == y && s.rows == 1 && matches!(s.kind, ShapeKind::Chip(Tint::Pill | Tint::AccentContainer)))
        .collect();
    chips.sort_by_key(|s| s.col);
    let text = rows.rows[y].text();
    chips
        .into_iter()
        .map(|s| {
            let words: String = text.chars().skip(usize::from(s.col)).take(usize::from(s.width)).collect();
            (s.kind == ShapeKind::Chip(Tint::AccentContainer), words.trim().to_string())
        })
        .collect()
}

/// The search lists Schedule shutdown once, its durations as chips on its second line (none
/// picked), under its category's label; → picks the first; the card follows the selection; chips
/// that do not fit are marked at the cut ends.
#[test]
fn fixed_choices_show_as_chips_not_rows() {
    let (mut l, b) = panel_box("");
    typed(&mut l, "/sh");
    let rows = draw_in(&l, &b);
    let text = tool_text(&rows);
    assert!(!text.iter().any(|t| t.contains("Schedule shutdown · ")), "no choice row: {text:#?}");
    let (y, _) = find(&rows, "Schedule shutdown");
    let chips = chips_on(&rows, y + 1);
    let words: Vec<&str> = chips.iter().map(|(_, w)| w.as_str()).collect();
    assert_eq!(words, ["30 min", "1 h", "1 h 30", "2 h", "3 h"], "{}", snapshot_with_shapes(&rows));
    assert!(chips.iter().all(|(picked, _)| !picked), "no chip picked: {chips:?}");
    assert!(text[..y].iter().rev().find(|t| t.contains('●')).is_some_and(|t| t.contains("● SYSTEM")), "under its label: {text:#?}");
    // A row without chips keeps its description.
    let (wy, _) = find(&rows, "Screenshot");
    let description = l.tools().unwrap().catalog.entries[entry_named(&l, "Screenshot")].description.clone().unwrap();
    assert!(chips_on(&rows, wy + 1).is_empty() && text[wy + 1].contains(description.as_ref()), "{text:#?}");
    // On its row, → moves the pick; the card shows the selected tool.
    let shutdown = entry_named(&l, "Schedule shutdown");
    let at = l.results.iter().position(|h| matches!(h.source, Source::Tool(i) if l.tool_rows()[i].id == RowId::Entry(shutdown))).unwrap();
    while l.selected < at {
        l.key(Key::Down);
    }
    l.key(Key::Right);
    let rows = draw_in(&l, &b);
    let (y, _) = find(&rows, "Schedule shutdown");
    let picked: Vec<String> = chips_on(&rows, y + 1).into_iter().filter(|(p, _)| *p).map(|(_, w)| w).collect();
    assert_eq!(picked, ["30 min"]);
    assert!(card_text(&rows).iter().any(|t| t == "Schedule shutdown"), "{:#?}", card_text(&rows));
    insta::assert_snapshot!("panel_search", snapshot_with_shapes(&rows));
    // A click on a search row's name is its result (on a chip, that chip: `a_click_on_a_chip_maps_to_it`).
    let ring = |v: usize| u16::try_from(v + 1).unwrap();
    assert_eq!(hit(&l, &b, ring(8), ring(y)), Some(Click::Result(at)));
    assert_eq!(hit(&l, &b, ring(8), ring(y + 1)), Some(Click::Chip(at, 0)));
    // A picked chip past the line's end: the line starts further right so it shows.
    let scan = entry_named(&l, "Network scan");
    let (sy, _) = find(&rows, "Network scan");
    let line = list_text(&rows)[sy + 1].trim().to_string();
    assert!(line.ends_with('›') && !line.starts_with('‹'), "more chips at the right: {line:?}");
    l.key(Key::Down);
    assert_eq!(
        l.tool_rows()[match l.results[l.selected].source {
            Source::Tool(i) => i,
            other => panic!("{other:?}"),
        }]
        .id,
        RowId::Entry(scan)
    );
    while l.key(Key::Right) == crate::launcher::model::Outcome::Redraw {}
    let rows = draw_in(&l, &b);
    let (y, _) = find(&rows, "Network scan");
    let chips = chips_on(&rows, y + 1);
    assert_eq!(chips.last().cloned(), Some((true, "Aggressive scan -A (sudo)".to_string())), "{}", snapshot_with_shapes(&rows));
    let line = list_text(&rows)[y + 1].trim().to_string();
    assert!(line.starts_with('‹') && !line.ends_with('›'), "more chips at the left: {line:?}");
}

/// `sh` found in `Schedule shutdown`: those two cells in the accent, the rest of the name not.
#[test]
fn matched_characters_are_accented() {
    let (mut l, b) = panel_box("");
    typed(&mut l, "/sh");
    let rows = draw_in(&l, &b);
    let (y, x) = find(&rows, "Schedule shutdown");
    let styles: Vec<char> = rows.rows[y].styles().chars().collect();
    let (accent, normal) = (Style::ACCENT.code(), Style::NORMAL.code());
    let name: Vec<char> = styles[x..x + "Schedule shutdown".len()].to_vec();
    for (i, c) in name.iter().enumerate() {
        let want = if (9..=10).contains(&i) { accent } else { normal };
        assert_eq!(*c, want, "cell {i} of the name: {name:?}");
    }
    // At home no name is accented.
    let (mut l, b) = panel_box("");
    select_tool(&mut l, "Schedule shutdown");
    let rows = draw_in(&l, &b);
    let (y, x) = find(&rows, "Schedule shutdown");
    let name: String = rows.rows[y].styles().chars().skip(x).take("Schedule shutdown".len()).collect();
    assert!(name.chars().all(|c| c == normal), "{name:?}");
}

/// A click on a tab of the strip is that tab, scrolled or not; between two tabs it is nothing.
#[test]
fn a_click_on_a_tab_maps_to_it() {
    use crate::launcher::tools::panel::Tab;
    let (l, b) = panel_box("");
    let rows = draw_in(&l, &b);
    let (y, x) = find(&rows, " Captures ");
    let captures = l.tools().unwrap().catalog.categories.iter().position(|c| c.name == "Captures").unwrap();
    let tab = l.tools().unwrap().panel().unwrap().tabs.iter().position(|t| *t == Tab::Category(captures)).unwrap();
    let ring = |v: usize| u16::try_from(v + 1).unwrap();
    assert_eq!(y, 1, "the strip's row");
    assert_eq!(hit(&l, &b, ring(x + 1), ring(y)), Some(Click::Tab(tab)));
    assert_eq!(hit(&l, &b, ring(x), ring(y)), Some(Click::Tab(tab)), "the pill's own edge");
    assert_eq!(hit(&l, &b, ring(x - 1), ring(y)), Some(Click::Inside), "the gap before it");
    // A scrolled strip: the last tab drawn is the last tab.
    let (mut l, b) = panel_box(&many_categories());
    l.key(Key::BackTab);
    let strip = draw_in(&l, &b).rows[1].text();
    let (y, x) = (1, strip[..strip.find(" Category 15 ").unwrap()].chars().count());
    let last = l.tools().unwrap().panel().unwrap().tabs.len() - 1;
    assert_eq!(hit(&l, &b, ring(x + 2), ring(y)), Some(Click::Tab(last)));
}

#[test]
fn a_pending_question_turns_the_frame_urgent() {
    let mut l = tools_box(TOOLS_CFG);
    assert_eq!(frame_ink(&l), FrameInk::Border);
    keys(&mut l, &[Key::Down, Key::Enter, Key::Down]); // Collect garbage
    l.set_now(std::time::Instant::now());
    assert_eq!(l.key(Key::Enter), crate::launcher::model::Outcome::Asked);
    assert_eq!(frame_ink(&l), FrameInk::Urgent, "while the question is pending");
    let (_, rows) = draw(&mut l, 100);
    let question = tool_text(&rows).into_iter().position(|t| t.contains("Collect garbage? ⏎")).unwrap();
    assert!(rows.rows[question].styles().contains(Style::URGENT.code()), "the question in the urgent tint");
    l.key(Key::Down);
    assert_eq!(frame_ink(&l), FrameInk::Border, "any other key cancels it");
}

#[test]
fn the_footer_hides_letters_when_they_are_not_live() {
    let (mut l, b) = panel_box(PINNED);
    let footer = |l: &Launcher| draw_in(l, &b).rows.last().unwrap().text();
    let live = footer(&l);
    for word in ["⏎", "Run", "→", "Options", "Tab", "Category", "a-z", "Quick key", "Esc", "Close"] {
        assert!(live.contains(word), "{word} in {live:?}");
    }
    l.key(Key::Char('/'));
    assert!(!l.letters_live());
    let typing = footer(&l);
    assert!(!typing.contains("a-z") && !typing.contains("Quick key"), "{typing:?}");
    assert!(typing.contains("Run") && typing.contains("Close"), "{typing:?}");
    // No tool of the tab has a letter: none to offer.
    let (l, b) = panel_box("[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Plain\"\nrun = [\"x\"]\n");
    let none = draw_in(&l, &b).rows.last().unwrap().text();
    assert!(!none.contains("a-z"), "{none:?}");
}

#[test]
fn a_tool_row_in_all_keeps_the_flat_selection() {
    use crate::launcher::tools::catalog::{Catalog, Context};
    let config = LauncherConfig::from_text(TOOLS_CFG).unwrap();
    let catalog = Catalog::build(&config.tools, &Context::default(), &mut |_| {});
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), NOW);
    l.set_tools(catalog);
    typed(&mut l, "lock now");
    let (b, rows) = draw(&mut l, 100);
    assert!(matches!(l.selected_hit().map(|h| h.source), Some(Source::Tool(_))), "{}", snapshot(&rows));
    let row = &rows.rows[usize::from(b.list_top) - 1];
    assert!(row.text().contains("Lock now") && row.styles().contains(Style::SELECTED.code()), "{}", snapshot(&rows));
    assert!(rows.shapes.is_empty(), "no band outside the Tools mode");
}

/// Tool rows in All (Mod+Space, a typed query) keep their own look: no glyph, the category two
/// cells after the name, the flat selection; the Tools mode's restyle stays in the Tools mode.
#[test]
fn tool_rows_in_all_keep_their_look() {
    use crate::launcher::tools::catalog::{Catalog, Context};
    let config = LauncherConfig::from_text(TOOLS_CFG).unwrap();
    let tools = Catalog::build(&config.tools, &Context::default(), &mut |_| {});
    let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
    l.set_catalog(catalog());
    l.set_tools(tools);
    typed(&mut l, "o");
    let at = (0..l.results.len()).find(|&i| matches!(l.results[i].source, Source::Tool(_))).expect("a tool row");
    for _ in 0..at {
        l.key(Key::Down);
    }
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("tools_in_all", snapshot(&rows));
}

/// A Tools question is asked in the Tools mode only: a confirm tool hit in All moves the box to
/// its level with the question pending, and the frame turns urgent there.
#[test]
fn a_confirm_from_all_asks_in_the_tools_mode_with_an_urgent_frame() {
    use crate::launcher::tools::catalog::{Catalog, Context};
    let config = LauncherConfig::from_text(TOOLS_CFG).unwrap();
    let tools = Catalog::build(&config.tools, &Context::default(), &mut |_| {});
    let mut l = Launcher::for_apps(config, Vec::new(), History::empty(200), NOW);
    l.set_tools(tools);
    typed(&mut l, "collect garbage");
    assert!(matches!(l.selected_hit().map(|h| h.source), Some(Source::Tool(_))));
    assert_eq!(frame_ink(&l), FrameInk::Border);
    l.set_now(std::time::Instant::now());
    l.key(Key::Enter);
    assert_eq!(l.mode, Mode::Tools, "the question lives in the Tools mode");
    assert_eq!(frame_ink(&l), FrameInk::Urgent);
}

// --- the panel's box size (spec "Box") ---

#[test]
fn the_tools_layout_takes_the_panel_s_size_capped_at_90_and_80_percent() {
    let b = tools_layout(232, 49, (105, 20));
    assert_eq!(
        b,
        BoxLayout { columns: 107, rows: 27, list_top: 2, list_rows: 23, preview_columns: 0 },
        "the list and the ring across; the chip, the tabs, a line, the panes, a line and the footer down"
    );
    let capped = tools_layout(80, 30, (200, 100));
    assert_eq!((capped.columns, capped.rows), (72, 24), "90 % of the columns, 80 % of the rows");
    assert_eq!((capped.list_top, capped.list_rows, capped.preview_columns), (2, 20, 0));
}

#[test]
fn the_panel_box_fits_a_small_output() {
    let room = panel_room(80, 30);
    let b = tools_layout(80, 30, room);
    assert!(b.columns <= 72 && b.rows <= 24, "at most 90 % × 80 % of the output: {b:?}");
    assert_eq!((b.columns, b.rows), (room.0 + 2, room.1 + 7), "the room fits the box: no cut");
}

#[test]
fn a_narrow_panel_keeps_today_s_width() {
    assert_eq!(tools_layout(232, 49, (40, 30)).columns, BOX_COLUMNS, "never narrower than today's box");
    assert_eq!(super::tools_box(BOX_COLUMNS + 10, 40, (40, 30)).columns, BOX_COLUMNS, "a refit floors the same");
    assert_eq!(tools_layout(50, 49, (40, 30)).columns, 45, "a narrow output still caps at 90 %");
}

#[test]
fn the_panel_is_three_cards_wide_on_a_wide_output() {
    let room = panel_room(232, 49);
    assert_eq!(room.0, 104, "three cards of 34 and their two gaps, not 90 % of the output");
    assert_eq!(tools_layout(232, 49, room).columns, 106, "about 106 cells (spec \"Box\")");
    assert_eq!(panel_room(80, 30).0, 70, "a smaller output: its 90 % cap, less the ring");
}

#[test]
fn a_panel_drawn_in_a_narrower_box_keeps_its_shapes_inside() {
    let (l, b) = panel_box(PINNED);
    assert!(b.columns > 100, "opened wider than the box it is drawn in: {b:?}");
    let narrow = BoxLayout { columns: 100, ..b };
    let rows = draw_in(&l, &narrow);
    assert!(!capsules(&rows).is_empty());
    for s in &rows.shapes {
        assert!(s.col + s.width <= narrow.columns - 2, "{s:?} past the last column of {narrow:?}");
    }
}

/// The model pages by the tools the pane holds at the panel (two lines each), by its rows elsewhere.
#[test]
fn the_panel_pages_by_its_two_line_tools() {
    let (mut l, b) = panel_box("");
    assert_eq!(model_rows(&l, &b), usize::from(b.rows - 7) / 2);
    l.key(Key::PageDown);
    assert_eq!(l.selected, usize::from(b.rows - 7) / 2);
    let mut apps = launcher();
    let (b, _) = draw(&mut apps, 100);
    assert_eq!(model_rows(&apps, &b), usize::from(b.list_rows));
}

/// A box drawn shorter than planned (a size the compositor imposed) fits the model to it: the USB
/// reset, cut off there, draws no capsule and its letter `x` starts the search.
#[test]
fn a_letter_follows_the_box_drawn() {
    let (mut l, b) = panel_box(PINNED);
    // The ring and chrome, then four lines: Pinned's label and Wi-Fi, the USB reset cut off.
    let short = BoxLayout { rows: 7 + 4, ..b };
    fit(&mut l, &short);
    assert_eq!(capsules(&draw_in(&l, &short)).len(), 1, "Wi-Fi's capsule only");
    let o = l.key(Key::Char('x'));
    assert!(!matches!(o, crate::launcher::model::Outcome::Activate(_)), "{o:?}");
    assert_eq!(l.query, "x");
}

/// The search field keeps the `Super` `X` capsules while its chip has room for them beside the
/// magnifier and the placeholder: a 60-column box shows them, a 59-column one does not.
#[test]
fn the_super_x_capsules_need_room_in_the_search_field() {
    let (l, b) = panel_box("");
    let keys = |columns: u16| {
        let boxed = render(&l, &BoxLayout { columns, ..b });
        // The search field's row, in the ring's coordinates.
        boxed.shapes.iter().filter(|s| s.row == 0 && s.kind == ShapeKind::Chip(Tint::Selection)).count()
    };
    assert_eq!(keys(60), 2);
    assert_eq!(keys(59), 0);
}

// --- the panel's cleanup (Task 13b) ---

/// Opens the tab that lists `name`, from All.
fn tab_with(l: &mut Launcher, name: &str) {
    let e = entry_named(l, name);
    l.key(Key::Tab);
    while !l.tools().unwrap().panel().unwrap().tools().any(|t| t == e) {
        l.key(Key::Tab);
    }
}

/// A level below the root (the choices of Schedule shutdown) lists its rows on one line each in
/// the list pane and keeps the entry's detail card at the right; a click on the card is no row.
#[test]
fn a_level_below_keeps_the_detail_card() {
    let (mut l, b) = panel_box("");
    tab_with(&mut l, "Schedule shutdown");
    select_tool(&mut l, "Schedule shutdown");
    l.key(Key::Enter);
    let shutdown = entry_named(&l, "Schedule shutdown");
    assert_eq!(l.tools().unwrap().level(), crate::launcher::tools::nav::Level::Choices { entry: shutdown });
    let rows = draw_in(&l, &b);
    let text = card_text(&rows);
    assert!(text.iter().any(|t| t == "Schedule shutdown"), "the entry's card: {}", snapshot_with_shapes(&rows));
    let list = list_text(&rows);
    let first = list.iter().position(|t| t.contains("30 min")).unwrap_or_else(|| panic!("{list:#?}"));
    assert!(list[first + 1].contains("1 h"), "one line per choice: {list:#?}");
    let c = card(&rows);
    let band = rows.shapes.iter().find(|s| s.kind == ShapeKind::Band).expect("the selected choice");
    assert!(band.col + band.width <= c.col, "the band stays left of the card: {band:?} {c:?}");
    let ring = |v: usize| u16::try_from(v + 1).unwrap();
    assert_eq!(hit(&l, &b, ring(usize::from(c.col) + 3), ring(first)), Some(Click::Inside), "the card is no row");
    assert_eq!(hit(&l, &b, ring(4), ring(first)), Some(Click::Result(0)));
}

/// A pinned tool's card says `Pinned` on a category tab too.
#[test]
fn the_pinned_hint_shows_on_a_category_tab() {
    let (mut l, b) = panel_box(PINNED);
    tab_with(&mut l, "Reset USB (xHCI)");
    select_tool(&mut l, "Reset USB (xHCI)");
    let text = card_text(&draw_in(&l, &b));
    assert!(text.iter().any(|t| t == "Pinned"), "{text:#?}");
}

/// The rows of the list pane that hold a section label.
fn label_rows(rows: &Boxed) -> Vec<usize> {
    list_text(rows)
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            let t = t.trim_start();
            t.starts_with('●') || t.starts_with("PINNED") || t.starts_with("RECENT")
        })
        .map(|(y, _)| y)
        .collect()
}

/// Whatever the selection, a section label is drawn only with its first tool under it.
#[test]
fn a_section_label_never_sits_alone_at_the_bottom() {
    let (mut l, b) = panel_box(PINNED);
    loop {
        let rows = draw_in(&l, &b);
        let tiles: Vec<usize> = tiles(&rows).iter().filter(|s| s.col < card(&rows).col).map(|s| s.row).collect();
        for y in label_rows(&rows) {
            assert!(tiles.contains(&(y + 1)), "the label on row {y} with no tool under it: {}", snapshot_with_shapes(&rows));
        }
        if l.key(Key::Down) != crate::launcher::model::Outcome::Redraw {
            break;
        }
    }
}

/// A cut strip that starts at the first tab draws it flush, with no room kept for a `‹`.
#[test]
fn a_cut_strip_starts_flush_at_the_first_tab() {
    let (l, b) = panel_box(&many_categories());
    let rows = draw_in(&l, &b);
    let pills: Vec<&Shape> = rows.shapes.iter().filter(|s| s.row == 1 && matches!(s.kind, ShapeKind::Chip(_))).collect();
    let first = pills.iter().map(|s| s.col).min().unwrap();
    let field = rows.shapes.iter().find(|s| s.row == 0 && s.kind == ShapeKind::Chip(Tint::Pill)).expect("the search field");
    assert_eq!(first, field.col, "flush with the search field: {}", snapshot_with_shapes(&rows));
}

/// On a search row with chips and none picked, Enter opens the choices: the footer says Open.
#[test]
fn the_footer_says_open_on_an_unpicked_chip_row() {
    let (mut l, b) = panel_box("");
    typed(&mut l, "/schedule");
    let footer = |l: &Launcher| draw_in(l, &b).rows.last().unwrap().text();
    let unpicked = footer(&l);
    assert!(unpicked.contains("Open") && !unpicked.contains("Run"), "{unpicked:?}");
    l.key(Key::Right);
    let picked = footer(&l);
    assert!(picked.contains("Run") && !picked.contains("Open"), "{picked:?}");
}

/// A click on a chip is that chip of that result; elsewhere on the chip line, the result.
#[test]
fn a_click_on_a_chip_maps_to_it() {
    let (mut l, b) = panel_box("");
    typed(&mut l, "/schedule");
    let rows = draw_in(&l, &b);
    let (y, _) = find(&rows, "Schedule shutdown");
    let ring = |v: usize| u16::try_from(v + 1).unwrap();
    let mut chips: Vec<&Shape> =
        rows.shapes.iter().filter(|s| s.row == y + 1 && s.rows == 1 && matches!(s.kind, ShapeKind::Chip(_))).collect();
    chips.sort_by_key(|s| s.col);
    assert_eq!(chips.len(), 5, "{}", snapshot_with_shapes(&rows));
    for (k, c) in chips.iter().enumerate() {
        assert_eq!(hit(&l, &b, ring(usize::from(c.col) + 1), ring(y + 1)), Some(Click::Chip(0, k)), "chip {k}");
    }
    let past = chips.last().map(|c| c.col + c.width + 2).unwrap();
    assert_eq!(hit(&l, &b, ring(usize::from(past)), ring(y + 1)), Some(Click::Result(0)));
}
