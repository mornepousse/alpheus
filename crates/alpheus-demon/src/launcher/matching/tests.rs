use super::*;
use std::path::PathBuf;

const NOW: u64 = 1_800_000_000;

fn app(id: &str, name: &str, generic: Option<&str>, keywords: &[&str], exec: &str) -> DesktopEntry {
    DesktopEntry {
        id: id.into(),
        path: PathBuf::from(format!("/usr/share/applications/{id}")),
        name: name.into(),
        generic_name: generic.map(String::from),
        comment: None,
        keywords: keywords.iter().map(|k| k.to_string()).collect(),
        icon: None,
        exec: exec.into(),
        terminal: false,
        startup_wm_class: None,
        actions: vec![],
    }
}

fn window(selector: &str, class: &str, title: &str) -> Toplevel {
    Toplevel { selector: selector.into(), class: class.into(), title: title.into(), workspace: "1".into(), focused: false }
}

fn apps() -> Vec<DesktopEntry> {
    vec![
        app("org.mozilla.firefox.desktop", "Firefox", Some("Web Browser"), &["Internet", "WWW"], "firefox %u"),
        app("org.gnome.Nautilus.desktop", "Files", Some("File Manager"), &["folder", "explorer"], "nautilus --new-window"),
        app("Alacritty.desktop", "Alacritty", Some("Terminal"), &["shell", "prompt"], "foot"),
        app("codium.desktop", "VSCodium", Some("Text Editor"), &["vscode"], "\"/opt/VS Codium/bin/codium\" %F"),
        app("cafe.desktop", "Café Noir", None, &[], "noir"),
    ]
}

fn history(uses: &[(&str, u32)]) -> History {
    let mut h = History::empty(200);
    for (id, n) in uses {
        for _ in 0..*n {
            h.record(id, NOW);
        }
    }
    h
}

struct Lists {
    apps: Vec<DesktopEntry>,
    commands: Vec<String>,
    windows: Vec<Toplevel>,
    lines: Vec<String>,
    entries: Vec<SourceEntry>,
    entry_sources: Vec<SourceConfig>,
    history: History,
}

impl Lists {
    fn new() -> Lists {
        Lists {
            apps: apps(),
            commands: vec![],
            windows: vec![],
            lines: vec![],
            entries: vec![],
            entry_sources: vec![],
            history: History::empty(200),
        }
    }

    fn query<'a>(&'a self, mode: Mode, text: &'a str) -> Query<'a> {
        Query {
            text,
            mode,
            apps: &self.apps,
            commands: &self.commands,
            windows: &self.windows,
            lines: &self.lines,
            entries: &self.entries,
            entry_sources: &self.entry_sources,
            history: &self.history,
            now: NOW,
        }
    }

    fn rank(&self, mode: Mode, text: &str) -> Vec<Hit> {
        Ranker::new().rank(&self.query(mode, text))
    }

    fn sources(&self, mode: Mode, text: &str) -> Vec<Source> {
        self.rank(mode, text).into_iter().map(|h| h.source).collect()
    }
}

#[test]
fn modes_cycle_and_labels() {
    assert_eq!(Mode::All.next(), Mode::Apps);
    assert_eq!(Mode::Commands.next(), Mode::All, "wraps");
    assert_eq!(Mode::All.previous(), Mode::Commands, "wraps backwards");
    assert_eq!(Mode::Windows.previous(), Mode::Apps);
    assert_eq!((Mode::Script.next(), Mode::Script.previous()), (Mode::Script, Mode::Script));
    let labels: Vec<&str> = Mode::CYCLE.iter().map(|m| m.label()).collect();
    assert_eq!(labels, ["All", "Apps", "Windows", "Commands"]);
    assert_eq!(Mode::Script.label(), "Script");
}

#[test]
fn each_mode_lists_only_its_sources() {
    let mut l = Lists::new();
    l.windows = vec![window("id:1", "firefox", "Mail")];
    l.commands = vec!["firejail".into()];
    l.lines = vec!["fire drill".into()];
    l.entry_sources = vec![outils()];
    l.entries = vec![SourceEntry::new(0, "Alarme · fire drill")];
    let kinds = |m: Mode| l.rank(m, "fire").iter().map(|h| kind(h.source)).collect::<Vec<_>>();
    assert_eq!(kinds(Mode::All), [0, 1, 2], "All: windows, apps, then source entries");
    assert_eq!(kinds(Mode::Apps), [1]);
    assert_eq!(kinds(Mode::Windows), [0]);
    assert_eq!(kinds(Mode::Commands), [3]);
    assert_eq!(kinds(Mode::Script), [4]);
}

fn outils() -> SourceConfig {
    SourceConfig {
        name: "Outils".into(),
        list: vec!["tools-menu".into(), "--list".into()],
        run: vec!["tools-menu".into(), "{line}".into()],
    }
}

fn with_entries(lines: &[&str]) -> Lists {
    let mut l = Lists::new();
    l.entry_sources = vec![outils()];
    l.entries = lines.iter().map(|x| SourceEntry::new(0, x)).collect();
    l
}

#[test]
fn entries_show_on_a_typed_query_in_all_only() {
    let l = with_entries(&["Captures · Zone", "Verrouiller"]);
    assert!(l.sources(Mode::All, "").iter().all(|s| !matches!(s, Source::Entry(_))), "empty query: today's list");
    assert_eq!(l.sources(Mode::All, "verrouil"), [Source::Entry(1)]);
    for mode in [Mode::Apps, Mode::Windows, Mode::Commands, Mode::Script] {
        assert!(l.sources(mode, "verrouil").is_empty(), "{mode:?}");
    }
}

#[test]
fn an_entry_hit_ranks_before_every_hit_that_needs_the_path() {
    let mut l = with_entries(&["Captures · Zone", "Captures · Fenêtre", "Vieilles captures"]);
    l.history = history(&[("source:Outils:Captures · Zone", 50)]);
    assert_eq!(
        l.sources(Mode::All, "captures"),
        [Source::Entry(2), Source::Entry(0), Source::Entry(1)],
        "the entry named so first, then the group — frecency first within it"
    );
}

/// Spec § 3: a hit that needs the path ranks below an App hit, even a weaker match.
#[test]
fn a_hit_that_needs_the_path_ranks_below_an_app_hit() {
    // The app matches by one of its keywords only (no name bonus), letters scattered, and the entry
    // is used often: only the tier keeps the app first.
    let mut l = with_entries(&["Captures · Zone"]);
    l.apps = vec![app("shots.desktop", "Shots", None, &["clip and paste tures"], "shots")];
    l.history = history(&[("source:Outils:Captures · Zone", 50)]);
    assert_eq!(l.sources(Mode::All, "captures"), [Source::App(0), Source::Entry(0)]);
}

/// Review Focus 4.
#[test]
fn a_query_spanning_the_path_and_the_entry_finds_it() {
    let l = with_entries(&["Captures · Zone", "Affichage · Zoom"]);
    assert_eq!(l.sources(Mode::All, "captures zone"), [Source::Entry(0)]);
}

#[test]
fn ties_go_window_app_entry_command() {
    assert!(kind(Source::Window(0)) < kind(Source::App(0)));
    assert!(kind(Source::App(0)) < kind(Source::Entry(0)));
    assert!(kind(Source::Entry(0)) < kind(Source::Command(0)));
    let mut l = with_entries(&["Zone"]);
    l.apps = vec![app("x.desktop", "Xyz", Some("Zone"), &[], "xyz")];
    l.windows = vec![window("id:1", "unlinked", "Zone")];
    assert_eq!(l.sources(Mode::All, "zon"), [Source::Window(0), Source::App(0), Source::Entry(0)]);
}

#[test]
fn entry_frecency_is_kept_per_source_and_line() {
    let mut l = with_entries(&["Zone A", "Zone B"]);
    l.history = history(&[("source:Outils:Zone B", 3), ("source:Autre:Zone A", 9)]);
    assert_eq!(l.sources(Mode::All, "zone"), [Source::Entry(1), Source::Entry(0)], "Zone A's uses were another source's");
}

#[test]
fn an_app_matches_by_name_generic_name_keywords_or_binary() {
    let l = Lists::new();
    assert_eq!(l.sources(Mode::Apps, "files"), [Source::App(1)], "name");
    assert_eq!(l.sources(Mode::Apps, "browser"), [Source::App(0)], "generic name");
    assert_eq!(l.sources(Mode::Apps, "explorer"), [Source::App(1)], "one of its keywords");
    assert_eq!(l.sources(Mode::Apps, "nautilus"), [Source::App(1)], "binary of Exec");
    assert_eq!(l.sources(Mode::Apps, "codium"), [Source::App(3)], "binary from a quoted path with a space");
    assert!(l.sources(Mode::Apps, "zzzz").is_empty());
}

#[test]
fn a_name_match_beats_a_generic_name_match_despite_frecency() {
    let mut l = Lists::new();
    l.apps = vec![app("term.desktop", "Term", None, &[], "term"), app("console.desktop", "Console", Some("Term"), &[], "kgx")];
    l.history = history(&[("app:console.desktop", 50)]);
    assert_eq!(l.sources(Mode::Apps, "term"), [Source::App(0), Source::App(1)], "NAME_BONUS outweighs frecency");
}

/// Same rule as above, but with a partial query: "term" is not typed whole against
/// either app's name, so the "name typed whole" tie-break (which would otherwise also
/// pick Term first) cannot be what makes the test pass — only NAME_BONUS can.
#[test]
fn a_name_match_beats_a_generic_name_match_when_neither_is_typed_whole() {
    let mut l = Lists::new();
    l.apps = vec![app("xterm.desktop", "xterm", None, &[], "xterm"), app("zeta.desktop", "Zeta", Some("xterm"), &[], "zeta")];
    l.history = history(&[("app:zeta.desktop", 50)]);
    assert_eq!(l.sources(Mode::Apps, "term"), [Source::App(0), Source::App(1)], "NAME_BONUS outweighs frecency");
}

#[test]
fn frecency_breaks_ties_within_a_bucket() {
    let mut l = Lists::new();
    l.apps = vec![
        app("firefox.desktop", "Firefox", None, &[], "firefox"),
        app("firefox-dev.desktop", "Firefox Developer Edition", None, &[], "firefox-dev"),
    ];
    assert_eq!(l.sources(Mode::Apps, "fire"), [Source::App(0), Source::App(1)], "no history: by name");
    l.history = history(&[("app:firefox-dev.desktop", 3)]);
    let hits = l.rank(Mode::Apps, "fire");
    assert_eq!(hits.iter().map(|h| h.source).collect::<Vec<_>>(), [Source::App(1), Source::App(0)], "used more: first");
    assert_eq!((hits[0].frecency, hits[1].frecency), (300, 0));
}

#[test]
fn a_better_match_beats_frecency_across_buckets() {
    let mut l = Lists::new();
    l.apps = vec![app("cool-decoder.desktop", "Cool Decoder", None, &[], "cool-decoder"), app("code.desktop", "Code", None, &[], "code")];
    l.history = history(&[("app:cool-decoder.desktop", 1000)]);
    let hits = l.rank(Mode::Apps, "code");
    assert_eq!(hits[0].source, Source::App(1), "exact name first: {hits:?}");
    assert!(hits[0].score / SCORE_BUCKET > hits[1].score / SCORE_BUCKET, "{hits:?}");
}

#[test]
fn a_name_typed_whole_beats_frecency_within_its_bucket() {
    let mut l = Lists::new();
    l.commands = vec!["lsblk".into(), "ls".into()];
    l.history = history(&[("cmd:lsblk", 5)]);
    let hits = l.rank(Mode::Commands, "ls");
    assert_eq!(hits[0].score / SCORE_BUCKET, hits[1].score / SCORE_BUCKET, "same bucket: {hits:?}");
    assert_eq!(hits.iter().map(|h| h.source).collect::<Vec<_>>(), [Source::Command(1), Source::Command(0)], "ls, typed whole, first");
    assert_eq!(l.sources(Mode::Commands, "lsb"), [Source::Command(0)], "a partial name: nothing typed whole");
}

/// Commands mode with whitespace in the query is text the model runs through the shell
/// (model.rs's `primary`/`secondary`), never a command to pick: ranking it would show a
/// row Enter then ignores. `gitlog` would otherwise fuzzy-match both atoms of "git log".
#[test]
fn commands_mode_does_not_rank_a_query_with_whitespace() {
    let mut l = Lists::new();
    l.commands = vec!["gitlog".into()];
    assert_eq!(l.sources(Mode::Commands, "git log"), [], "whitespace: nothing ranked, not even a fuzzy hit");
    assert_eq!(l.sources(Mode::Commands, "gitlog"), [Source::Command(0)], "no whitespace: ranked as usual");
    assert_eq!(
        l.sources(Mode::Commands, "  "),
        [Source::Command(0)],
        "blank (whitespace-only, nothing typed): still the empty-query listing, not suppressed"
    );
}

#[test]
fn per_list_facts_are_computed_once_until_the_lists_change() {
    let mut l = Lists::new();
    l.windows = vec![window("id:1", "firefox", "Mail")];
    l.commands = vec!["awk".into(), "btop".into()];
    l.history = history(&[("app:org.mozilla.firefox.desktop", 1), ("cmd:btop", 1)]);
    let mut r = Ranker::new();
    let mut last = Vec::new();
    for (mode, text) in [(Mode::All, ""), (Mode::All, "f"), (Mode::All, "fi"), (Mode::Commands, "t"), (Mode::Commands, "")] {
        last = r.rank(&l.query(mode, text));
        assert_eq!(last, l.rank(mode, text), "a reused Ranker ranks like a new one ({mode:?} {text:?})");
    }
    assert_eq!(last[0], Hit { source: Source::Command(1), score: 0, frecency: 100 });
    assert_eq!(r.preparations, 1, "frecency and window apps once per open, not per keystroke");
    l.commands.push("cat".into());
    r.rank(&l.query(Mode::Commands, ""));
    assert_eq!(r.preparations, 2, "another list length: computed again, never a stale index");
    r.forget();
    r.rank(&l.query(Mode::Commands, ""));
    assert_eq!(r.preparations, 3, "forget: computed again");
}

#[test]
fn script_lines_are_plain_text_not_fzf_operators() {
    let mut l = Lists::new();
    l.lines = vec!["!important".into(), "^start".into(), "it's".into(), "cost$".into(), "plain".into()];
    assert_eq!(l.sources(Mode::Script, "!imp"), [Source::Line(0)], "! is a character, not a negation");
    assert_eq!(l.sources(Mode::Script, "^st"), [Source::Line(1)]);
    assert_eq!(l.sources(Mode::Script, "'s"), [Source::Line(2)]);
    assert_eq!(l.sources(Mode::Script, "t$"), [Source::Line(3)]);
    l.commands = vec!["important".into(), "plain".into()];
    assert_eq!(l.sources(Mode::Commands, "!imp"), [Source::Command(1)], "elsewhere fzf syntax still holds");
}

#[test]
fn a_window_ranks_above_the_app_it_belongs_to() {
    let mut l = Lists::new();
    l.windows = vec![window("address:0x1", "org.gnome.Nautilus", "Documents")];
    l.history = history(&[("app:org.gnome.Nautilus.desktop", 2)]);
    let hits = l.rank(Mode::All, "files");
    assert_eq!(hits.iter().map(|h| h.source).collect::<Vec<_>>(), [Source::Window(0), Source::App(1)], "via the app's name");
    assert_eq!(hits[0].score, hits[1].score, "the window takes its app's score");
    assert_eq!(hits[0].frecency, 200, "and its frecency");
    assert_eq!(l.sources(Mode::Windows, "files"), [Source::Window(0)], "Windows mode too");
    assert_eq!(l.sources(Mode::All, "docu"), [Source::Window(0)], "by its own title");
}

#[test]
fn same_score_and_frecency_sort_by_name_case_insensitively_then_index() {
    let mut l = Lists::new();
    l.commands = vec!["B-tool".into(), "a-tool".into()];
    assert_eq!(l.sources(Mode::Commands, "tool"), [Source::Command(1), Source::Command(0)], "a before B, whatever the case");
    l.lines = vec!["same".into(), "other".into(), "same".into()];
    assert_eq!(l.sources(Mode::Script, "same"), [Source::Line(0), Source::Line(2)], "duplicates keep their order");
}

#[test]
fn case_and_accents_fold_smartly() {
    let l = Lists::new();
    assert_eq!(l.sources(Mode::Apps, "fire"), [Source::App(0)], "lower case matches any case");
    assert!(l.sources(Mode::Apps, "FIRE").is_empty(), "an upper case letter makes it case sensitive (smart case)");
    assert_eq!(l.sources(Mode::Apps, "cafe"), [Source::App(4)], "café found by cafe");
}

#[test]
fn empty_query_orders_per_mode() {
    let mut l = Lists::new();
    l.windows = vec![window("id:2", "Alacritty", "zsh"), window("id:1", "firefox", "Mail")];
    l.commands = vec!["awk".into(), "btop".into(), "cat".into()];
    l.lines = vec!["z".into(), "a".into()];
    l.history = history(&[("app:Alacritty.desktop", 1), ("cmd:cat", 1)]);
    assert_eq!(
        l.sources(Mode::All, ""),
        [Source::App(2), Source::App(4), Source::App(1), Source::App(0), Source::App(3), Source::Window(0), Source::Window(1)],
        "apps by frecency then name (Alacritty, Café Noir, Files, Firefox, VSCodium), then windows as given"
    );
    assert_eq!(l.sources(Mode::Windows, ""), [Source::Window(0), Source::Window(1)]);
    assert_eq!(l.sources(Mode::Commands, ""), [Source::Command(2), Source::Command(0), Source::Command(1)]);
    assert_eq!(l.sources(Mode::Script, ""), [Source::Line(0), Source::Line(1)], "input order");
    assert_eq!(l.sources(Mode::All, "   "), l.sources(Mode::All, ""), "blanks are an empty query");
}

#[test]
fn no_catalog_yet_still_lists_windows() {
    let mut l = Lists::new();
    l.apps = vec![];
    l.windows = vec![window("id:1", "firefox", "Mail")];
    assert_eq!(l.sources(Mode::All, ""), [Source::Window(0)]);
    assert_eq!(l.sources(Mode::All, "mail"), [Source::Window(0)]);
}

#[test]
fn results_are_capped() {
    let mut l = Lists::new();
    l.commands = (0..MAX_RESULTS + 100).map(|i| format!("cmd-{i:04}")).collect();
    assert_eq!(l.rank(Mode::Commands, "cmd").len(), MAX_RESULTS);
    assert_eq!(l.rank(Mode::Commands, "").len(), MAX_RESULTS);
}

#[test]
fn app_for_window_by_class() {
    let mut a = apps();
    a.push(DesktopEntry { startup_wm_class: Some("VSCodium-Window".into()), ..app("vsc.desktop", "VSC", None, &[], "vsc") });
    assert_eq!(app_for_window(&a, "firefox"), Some(0), "last dot-segment of the id");
    assert_eq!(app_for_window(&a, "org.gnome.nautilus"), Some(1), "whole id, any case");
    assert_eq!(app_for_window(&a, "foot"), Some(2), "id without .desktop");
    assert_eq!(app_for_window(&a, "codium"), Some(3), "binary of Exec");
    assert_eq!(app_for_window(&a, "vscodium-window"), Some(5), "StartupWMClass");
    assert_eq!(app_for_window(&a, "unknown"), None);
    assert_eq!(app_for_window(&a, ""), None, "an empty class matches nothing");
}

#[test]
fn command_path_finds_the_first_dir_that_has_it() {
    let dir = crate::launcher::testing::TempDir::new("command-path");
    std::fs::write(dir.join("btop"), b"").unwrap();
    let empty = crate::launcher::testing::TempDir::new("command-path-empty");
    let dirs = [empty.to_path_buf(), dir.to_path_buf()];
    assert_eq!(command_path(&dirs, "btop").as_deref(), Some(dir.join("btop").as_path()), "skips the empty dir, finds it in the next");
    assert_eq!(command_path(&dirs, "missing"), None, "not in any dir");
    assert_eq!(command_path(&[], "btop"), None, "no dirs at all");
}

#[test]
fn exec_program_is_the_first_word_s_basename() {
    assert_eq!(exec_program("firefox %u").as_deref(), Some("firefox"));
    assert_eq!(exec_program("  /usr/bin/foot -e").as_deref(), Some("foot"));
    assert_eq!(exec_program("env FOO=1 bar").as_deref(), Some("env"));
    assert_eq!(exec_program("\"/opt/My App/bin/my-app\" --x").as_deref(), Some("my-app"));
    assert_eq!(exec_program("\"/opt/a\\\"b/c\\\"d\"").as_deref(), Some("c\"d"));
    assert_eq!(exec_program(""), None);
    assert_eq!(exec_program("/usr/bin/"), None);
}

/// `exec_program` follows `exec_argv`'s quoting (single quotes, a backslash outside
/// quotes, field codes), not a hand-rolled subset of it: a single-quoted first argument
/// (valid Exec, just not the common case) must not leave a stray leading quote in the
/// basename.
#[test]
fn exec_program_follows_exec_argv_s_quoting() {
    assert_eq!(exec_program("'my app' --x").as_deref(), Some("my app"), "single-quoted, like exec_argv");
    assert_eq!(exec_program("prog\\ it\\'s"), Some("prog it's".into()), "backslash escapes outside quotes too");
}

// --- Performance round (delivery 3): precomputed orders, select_nth_unstable_by, a
// frecency map and a window → app index, plus two minor correctness fixes. ---

#[test]
fn fold_cow_matches_fold_key_even_without_an_ascii_uppercase_letter() {
    // "École": a capital letter that is not in the ASCII range at all — the fast
    // "no ASCII uppercase" check must not mistake that for "nothing to fold".
    assert_eq!(fold_cow("École"), fold_key("École"), "a capital letter can sit on a non-ASCII byte too");
}

#[test]
fn a_dmenu_line_longer_than_nucleos_matrix_limit_does_not_panic() {
    let mut l = Lists::new();
    let long = "a".repeat(5_000);
    l.lines = vec![long, "other".into()];
    assert_eq!(l.sources(Mode::Script, "aaaa"), [Source::Line(0)], "scores the long line without panicking");
}

#[test]
fn name_typed_whole_folds_accents_like_nucleo() {
    let mut l = Lists::new();
    l.commands = vec!["cafeteria".into(), "café".into()];
    assert_eq!(
        l.sources(Mode::Commands, "cafe"),
        [Source::Command(1), Source::Command(0)],
        "café, typed whole with its accent folded, beats the longer name"
    );
}

#[test]
fn app_for_window_class_match_is_unicode_case_insensitive() {
    let mut a = apps();
    a.push(DesktopEntry { startup_wm_class: Some("Éditeur".into()), ..app("ed.desktop", "Editeur", None, &[], "ed") });
    assert_eq!(app_for_window(&a, "éditeur"), Some(5), "É and é fold the same, not just ASCII letters");
}

#[test]
fn forget_windows_refreshes_links_without_recomputing_the_catalog() {
    let mut l = Lists::new();
    l.windows = vec![window("id:1", "firefox", "Mail")];
    l.commands = vec!["awk".into(), "btop".into()];
    l.history = history(&[("app:org.mozilla.firefox.desktop", 1), ("cmd:btop", 1)]);
    let mut r = Ranker::new();
    r.rank(&l.query(Mode::All, ""));
    assert_eq!((r.preparations, r.window_preparations), (1, 0), "built fresh, windows included");

    // The window's class now names a different app ("Files"), itself with no history:
    // only `forget_windows` tells the Ranker its window → app link is stale.
    l.windows = vec![window("id:1", "org.gnome.Nautilus", "Docs")];
    r.forget_windows();
    let hits = r.rank(&l.query(Mode::All, ""));
    assert_eq!((r.preparations, r.window_preparations), (1, 1), "catalog kept, only the window link refreshed");
    let window_hit = hits.iter().find(|h| matches!(h.source, Source::Window(_))).copied().unwrap();
    assert_eq!(window_hit, Hit { source: Source::Window(0), score: 0, frecency: 0 }, "no longer linked to Firefox's frecency");

    r.rank(&l.query(Mode::All, ""));
    assert_eq!((r.preparations, r.window_preparations), (1, 1), "a second rank reuses what was just refreshed");
}

#[test]
fn rank_does_not_keep_a_catalog_sized_allocation_after_truncation() {
    // Windows: `everything()` still cannot bound this one while building it (every window
    // gets a `Hit` unconditionally — there is no frecency-ordered index to slice the way
    // apps/commands/lines now have, Script lines included since the fix below). `rank()`'s
    // own `shrink_to_fit()` is what keeps a session with many windows open from carrying a
    // 20,000-entry allocation after every keystroke.
    let mut l = Lists::new();
    l.windows = (0..20_000).map(|i| window(&format!("id:{i}"), "x", &format!("win-{i:05}"))).collect();
    let hits = Ranker::new().rank(&l.query(Mode::Windows, ""));
    assert_eq!(hits.len(), MAX_RESULTS);
    assert!(hits.capacity() <= MAX_RESULTS, "capacity {} must not carry the 20,000-window scan", hits.capacity());
}

/// `everything()` appends windows after the apps slice, and the windows loop is never
/// itself capped (there are rarely many open at once): with a catalog larger than
/// `MAX_RESULTS`, apps alone could fill the budget and `rank()`'s final truncation would
/// then cut every window off the tail. Apps must leave windows their room.
#[test]
fn an_empty_all_query_reserves_room_for_every_window() {
    let mut l = Lists::new();
    l.apps = (0..MAX_RESULTS + 50).map(|i| app(&format!("a{i}.desktop"), &format!("App {i:04}"), None, &[], "x")).collect();
    l.windows = (0..5).map(|i| window(&format!("id:{i}"), "x", &format!("win-{i}"))).collect();
    let hits = l.rank(Mode::All, "");
    assert_eq!(hits.len(), MAX_RESULTS, "still capped");
    let windows_kept = hits.iter().filter(|h| matches!(h.source, Source::Window(_))).count();
    assert_eq!(windows_kept, 5, "every window must survive, not get truncated away by the apps ahead of it");
}

/// Review Focus 4: dmenu lines must not rebuild `everything()`'s old behaviour of
/// collecting every line before the final truncate — `.take(remaining)` bounds the
/// `Vec` from the first push, so this is observable on `everything()` alone, before
/// `rank()`'s `shrink_to_fit()` ever runs (which the test above still needs, for windows).
#[test]
fn everything_bounds_script_lines_without_building_the_whole_list() {
    let mut l = Lists::new();
    l.lines = (0..20_000).map(|i| format!("line-{i:05}")).collect();
    let q = l.query(Mode::Script, "");
    let pr = Prepared::new(&q);
    let hits = Ranker::new().everything(&q, &pr);
    assert_eq!(hits.len(), MAX_RESULTS);
    assert!(hits.capacity() <= MAX_RESULTS, "capacity {} must not have built all 20,000 lines", hits.capacity());
}

/// splitmix64: deterministic, no new dependency, good enough spread for a test fixture.
struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// The optimized path (precomputed `name_rank`, `select_nth_unstable_by` then a partial
/// sort) must produce exactly the same order as an obviously-correct full sort over the
/// same hits, even on a large list with heavy ties in score, frecency and name, mixed
/// case and accents (Review Focus 3: a `fold_cow` bug that mistakes "no ASCII uppercase"
/// for "nothing to fold" must be caught here too — on a tied list is exactly where it
/// would bite — not only by the small dedicated `fold_cow_matches_fold_key_…` test).
#[test]
fn optimized_ranking_matches_a_reference_full_sort_on_a_large_tied_list() {
    let mut rng = Lcg(0x5EED_C0FF_EEEE_1234);
    let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "École", "ÉCOLE", "Ångström", "ZEBRA"];
    let commands: Vec<String> = (0..4_000u64).map(|i| format!("{}-{}", words[rng.below(words.len() as u64) as usize], i % 12)).collect();
    let hits: Vec<Hit> = (0..commands.len())
        .map(|i| Hit { source: Source::Command(i), score: rng.below(12) as u32 * SCORE_BUCKET, frecency: rng.below(40) as u32 })
        .collect();
    let typed = commands[17].clone();
    let history = History::empty(0);
    let q = Query {
        text: &typed,
        mode: Mode::Commands,
        apps: &[],
        commands: &commands,
        windows: &[],
        lines: &[],
        entries: &[],
        entry_sources: &[],
        history: &history,
        now: 0,
    };
    let pr = Prepared::new(&q);

    let actual = finish(hits.clone(), &q, &pr);

    // Reference: reads names and the typed-whole check directly, never through the
    // precomputed `name_rank`/`exact` arrays this test exists to validate.
    let typed_trimmed = typed.trim();
    let mut expected = hits;
    expected.sort_by(|a, b| {
        let (Source::Command(ia), Source::Command(ib)) = (a.source, b.source) else { unreachable!() };
        (b.score / SCORE_BUCKET)
            .cmp(&(a.score / SCORE_BUCKET))
            .then_with(|| folded_eq(&commands[ib], typed_trimmed).cmp(&folded_eq(&commands[ia], typed_trimmed)))
            .then(b.frecency.cmp(&a.frecency))
            .then_with(|| cmp_ci(&commands[ia], &commands[ib]))
            .then(ia.cmp(&ib))
    });
    expected.truncate(MAX_RESULTS);

    assert_eq!(actual, expected, "a large, mixed-case/accented, heavily tied command list");

    // Mode::All through the real `rank()` pipeline, mixing windows and apps (not
    // synthetic Hits built by hand this time): every score, exact flag and frecency
    // comes from the production path; only the name tie-break is recomputed
    // independently of `name_rank`/`fold_cow`, against the real `name()`/`is_exact()`.
    let apps: Vec<DesktopEntry> = (0..120u64)
        .map(|i| {
            let w = words[rng.below(words.len() as u64) as usize];
            app(&format!("app{i}.desktop"), &format!("{w}-{}", i % 12), None, &[], &format!("prog{i}"))
        })
        .collect();
    let windows: Vec<Toplevel> = (0..40u64)
        .map(|i| {
            let w = words[rng.below(words.len() as u64) as usize];
            window(&format!("id:{i}"), "unlinked", &format!("{w}-{}", i % 12))
        })
        .collect();
    let empty_history = History::empty(0);
    let q2 = Query {
        text: "a",
        mode: Mode::All,
        apps: &apps,
        commands: &[],
        windows: &windows,
        lines: &[],
        entries: &[],
        entry_sources: &[],
        history: &empty_history,
        now: NOW,
    };
    let mut ranker = Ranker::new();
    let actual2 = ranker.rank(&q2);
    let pr2 = Prepared::new(&q2);
    let mut expected2 = actual2.clone();
    expected2.sort_by(|a, b| {
        (b.score / SCORE_BUCKET)
            .cmp(&(a.score / SCORE_BUCKET))
            .then_with(|| is_exact(&q2, &pr2, b.source, "a").cmp(&is_exact(&q2, &pr2, a.source, "a")))
            .then(b.frecency.cmp(&a.frecency))
            .then(kind(a.source).cmp(&kind(b.source)))
            .then_with(|| cmp_ci(name(&q2, a.source), name(&q2, b.source)))
            .then(index(a.source).cmp(&index(b.source)))
    });
    assert_eq!(actual2, expected2, "All mode, mixing windows and apps, through rank()");

    // Empty query: `everything()`'s precomputed `apps_empty_order` (also built from
    // `name_ranks`/`fold_cow`) must match a reference order too — apps by frecency then
    // name first (Mae, 2026-10-05), then windows as given (`everything()` never sorts them).
    let q3 = Query {
        text: "",
        mode: Mode::All,
        apps: &apps,
        commands: &[],
        windows: &windows,
        lines: &[],
        entries: &[],
        entry_sources: &[],
        history: &empty_history,
        now: NOW,
    };
    let actual3 = ranker.rank(&q3);
    let (app_frecency, _) = empty_history.frecency_maps(NOW);
    let fre = |i: usize| app_frecency.get(apps[i].id.as_str()).copied().unwrap_or(0);
    let mut expected_apps: Vec<usize> = (0..apps.len()).collect();
    expected_apps.sort_by(|&a, &b| fre(b).cmp(&fre(a)).then_with(|| cmp_ci(&apps[a].name, &apps[b].name)).then(a.cmp(&b)));
    let expected3: Vec<Source> = expected_apps.into_iter().map(Source::App).chain((0..windows.len()).map(Source::Window)).collect();
    assert_eq!(
        actual3.iter().map(|h| h.source).collect::<Vec<_>>(),
        expected3,
        "empty query: apps by frecency then name, then windows as given"
    );
}
