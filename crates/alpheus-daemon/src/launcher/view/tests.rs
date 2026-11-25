use super::*;
use crate::compositor::Toplevel;
use crate::keys::Key;
use crate::launcher::config::{LauncherConfig, SourceConfig};
use crate::launcher::desktop::{DesktopAction, DesktopEntry};
use crate::launcher::history::History;
use crate::launcher::scan::Catalog;
use crate::launcher::sources::{Entries, SourceEntry};
use alpheus_common::launcher_ipc::DmenuFlags;
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
    insta::assert_snapshot!("tools_category", snapshot(&rows));
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
    assert!(text.iter().any(|t| t.contains("⏎ run · esc back")), "the prompt's own hint: {text:?}");
}

#[test]
fn tools_flat_search_snapshot() {
    let mut l = tools_box(TOOLS_CFG);
    typed(&mut l, "gen");
    let (_, rows) = draw(&mut l, 100);
    insta::assert_snapshot!("tools_flat", snapshot(&rows));
    // The category is dimmed after the entry, two cells on; a root entry has none.
    let text = tool_text(&rows);
    assert!(text.iter().any(|t| t.contains("Generation  Nix")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("Generation · list  Nix")), "{text:?}");
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
    assert!(tool_text(&rows).iter().any(|t| t.contains("⏎ open/run · esc close")));
    assert!(!tool_text(&rows).iter().any(|t| t.contains("⌫") || t.contains("← back")), "← does nothing at the root");
    keys(&mut l, &[Key::Down, Key::Enter]);
    let (_, rows) = draw(&mut l, 100);
    assert!(tool_text(&rows).iter().any(|t| t.contains("⏎ open/run · ← esc back")));
    assert!(!tool_text(&rows).iter().any(|t| t.contains("esc close")));
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
    assert!(tool_text(&rows).iter().any(|t| t.contains("⏎ open/run · ⌫ all · esc close")), "{}", snapshot(&rows));
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
