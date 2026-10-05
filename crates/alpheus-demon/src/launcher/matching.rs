//! Matching and ranking (spec § 1): `nucleo-matcher` (fzf scoring, smart
//! case, Unicode folding) then frecency from the history. Pure: the lists
//! come in through `Query`, the hits go out as indices into them. The
//! `Ranker` owns the matcher's scratch memory: built when the launcher
//! opens, dropped when it closes (nothing kept at rest).

use crate::compositor::Toplevel;
use crate::launcher::desktop::DesktopEntry;
use crate::launcher::history::{History, app_key, command_key};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    All,
    Apps,
    Windows,
    Commands,
    Script,
}

impl Mode {
    /// The modes row, left to right (Script has no row).
    pub const CYCLE: [Mode; 4] = [Mode::All, Mode::Apps, Mode::Windows, Mode::Commands];

    pub fn label(self) -> &'static str {
        match self {
            Mode::All => "All",
            Mode::Apps => "Apps",
            Mode::Windows => "Windows",
            Mode::Commands => "Commands",
            Mode::Script => "Script",
        }
    }

    /// Next mode along CYCLE, wrapping; Script stays Script.
    pub fn next(self) -> Mode {
        self.step(1)
    }

    pub fn previous(self) -> Mode {
        self.step(Mode::CYCLE.len() - 1)
    }

    fn step(self, by: usize) -> Mode {
        match Mode::CYCLE.iter().position(|m| *m == self) {
            Some(i) => Mode::CYCLE[(i + by) % Mode::CYCLE.len()],
            None => self,
        }
    }
}

/// Index into the catalog's apps/commands, the window list, or the dmenu lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    App(usize),
    Window(usize),
    Command(usize),
    Line(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub source: Source,
    pub score: u32,
    pub frecency: u32,
}

/// Scores within one bucket (one matched character's worth, nucleo's
/// SCORE_MATCH) count as equal: frecency decides between them.
pub const SCORE_BUCKET: u32 = 16;
/// An app whose own name matches outranks one matched by a generic name,
/// keywords or a binary (two buckets).
pub const NAME_BONUS: u32 = 2 * SCORE_BUCKET;
/// Most hits returned (the event loop never sorts more for the view).
pub const MAX_RESULTS: usize = 500;

pub struct Query<'a> {
    /// Prefix already stripped.
    pub text: &'a str,
    pub mode: Mode,
    pub apps: &'a [DesktopEntry],
    pub commands: &'a [String],
    pub windows: &'a [Toplevel],
    pub lines: &'a [String],
    pub history: &'a History,
    pub now: u64,
}

/// Owns the nucleo Matcher and scratch buffers: created at open, dropped at close.
pub struct Ranker {
    matcher: Matcher,
    buf: Vec<char>,
    /// What does not depend on the query, computed at the first `rank` and
    /// again after `forget`: a keystroke never walks the history for 20,000
    /// commands nor matches every window against every app.
    prepared: Option<Prepared>,
    /// How many times `Prepared` was computed (tests only).
    #[cfg(test)]
    preparations: usize,
}

/// Per-list facts, indexed like the lists of the `Query` they came from.
struct Prepared {
    /// Lengths of apps, commands, windows, and `now`: a mismatch recomputes
    /// (a stale fact never indexes out of range).
    key: (usize, usize, usize, u64),
    app_frecency: Vec<u32>,
    /// Binary of each app's Exec (`exec_program`).
    programs: Vec<Option<String>>,
    command_frecency: Vec<u32>,
    /// `app_for_window` of each window.
    window_apps: Vec<Option<usize>>,
}

impl Prepared {
    fn new(q: &Query) -> Prepared {
        Prepared {
            key: (q.apps.len(), q.commands.len(), q.windows.len(), q.now),
            app_frecency: q.apps.iter().map(|e| q.history.frecency(&app_key(&e.id), q.now)).collect(),
            programs: q.apps.iter().map(|e| exec_program(&e.exec)).collect(),
            command_frecency: q.commands.iter().map(|c| q.history.frecency(&command_key(c), q.now)).collect(),
            window_apps: q.windows.iter().map(|w| app_for_window(q.apps, &w.class)).collect(),
        }
    }

    /// A window's frecency is its app's.
    fn window_frecency(&self, i: usize) -> u32 {
        self.window_apps[i].map_or(0, |a| self.app_frecency[a])
    }
}

impl Default for Ranker {
    fn default() -> Ranker {
        Ranker::new()
    }
}

/// Window, App, Command, Line: the order between kinds at equal score and frecency.
fn kind(s: Source) -> u8 {
    match s {
        Source::Window(_) => 0,
        Source::App(_) => 1,
        Source::Command(_) => 2,
        Source::Line(_) => 3,
    }
}

fn index(s: Source) -> usize {
    match s {
        Source::App(i) | Source::Window(i) | Source::Command(i) | Source::Line(i) => i,
    }
}

/// Case-insensitive order without allocating.
fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

/// The text a hit sorts by after score, frecency and kind.
fn name<'a>(q: &Query<'a>, s: Source) -> &'a str {
    match s {
        Source::App(i) => &q.apps[i].name,
        Source::Window(i) => &q.windows[i].title,
        Source::Command(i) => &q.commands[i],
        Source::Line(i) => &q.lines[i],
    }
}

impl Ranker {
    pub fn new() -> Ranker {
        Ranker {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
            prepared: None,
            #[cfg(test)]
            preparations: 0,
        }
    }

    /// The lists changed (a catalog arrived, the window list moved): the
    /// per-list facts are computed again at the next `rank`.
    pub fn forget(&mut self) {
        self.prepared = None;
    }

    fn score(&mut self, p: &Pattern, text: &str) -> Option<u32> {
        p.score(Utf32Str::new(text, &mut self.buf), &mut self.matcher)
    }

    /// max(s(name) + NAME_BONUS, s(generic name), s(each of the keywords), s(binary)).
    fn app_score(&mut self, p: &Pattern, e: &DesktopEntry, program: Option<&str>) -> Option<u32> {
        let mut best = self.score(p, &e.name).map(|s| s + NAME_BONUS);
        let others = e.generic_name.iter().chain(e.keywords.iter()).map(String::as_str).chain(program);
        for text in others {
            best = best.max(self.score(p, text));
        }
        best
    }

    pub fn rank(&mut self, q: &Query) -> Vec<Hit> {
        let key = (q.apps.len(), q.commands.len(), q.windows.len(), q.now);
        let prepared = match self.prepared.take() {
            Some(p) if p.key == key => p,
            _ => {
                #[cfg(test)]
                {
                    self.preparations += 1;
                }
                Prepared::new(q)
            }
        };
        let pattern = if q.mode == Mode::Script {
            // dmenu lines are plain text: "!", "^", "'" and "$" are
            // characters to find, not fzf operators.
            Pattern::new(q.text, CaseMatching::Smart, Normalization::Smart, AtomKind::Fuzzy)
        } else {
            Pattern::parse(q.text, CaseMatching::Smart, Normalization::Smart)
        };
        let mut hits = if pattern.atoms.is_empty() { self.everything(q, &prepared) } else { self.matching(q, &prepared, &pattern) };
        self.prepared = Some(prepared);
        hits.truncate(MAX_RESULTS);
        hits
    }

    /// Empty query: windows in the compositor's order, then apps (or
    /// commands) by frecency then name, or the dmenu lines as given.
    fn everything(&mut self, q: &Query, pr: &Prepared) -> Vec<Hit> {
        let mut hits = Vec::new();
        if matches!(q.mode, Mode::All | Mode::Windows) {
            for i in 0..q.windows.len() {
                hits.push(Hit { source: Source::Window(i), score: 0, frecency: pr.window_frecency(i) });
            }
        }
        let start = hits.len();
        match q.mode {
            Mode::All | Mode::Apps => {
                hits.extend((0..q.apps.len()).map(|i| Hit { source: Source::App(i), score: 0, frecency: pr.app_frecency[i] }))
            }
            Mode::Commands => {
                hits.extend((0..q.commands.len()).map(|i| Hit { source: Source::Command(i), score: 0, frecency: pr.command_frecency[i] }))
            }
            Mode::Script => hits.extend((0..q.lines.len()).map(|i| Hit { source: Source::Line(i), score: 0, frecency: 0 })),
            Mode::Windows => {}
        }
        if q.mode != Mode::Script {
            hits[start..].sort_by(|a, b| {
                b.frecency
                    .cmp(&a.frecency)
                    .then_with(|| cmp_ci(name(q, a.source), name(q, b.source)))
                    .then(index(a.source).cmp(&index(b.source)))
            });
        }
        hits
    }

    fn matching(&mut self, q: &Query, pr: &Prepared, p: &Pattern) -> Vec<Hit> {
        let mut hits = Vec::new();
        let wants_apps = matches!(q.mode, Mode::All | Mode::Apps);
        if matches!(q.mode, Mode::All | Mode::Windows) {
            for (i, w) in q.windows.iter().enumerate() {
                let mut score = self.score(p, &w.title).max(self.score(p, &w.class));
                if let Some(a) = pr.window_apps[i] {
                    // The app's own score (its name included): when the app
                    // matches, the window ties with it and wins on kind (spec § 1).
                    score = score.max(self.app_score(p, &q.apps[a], pr.programs[a].as_deref()));
                }
                if let Some(score) = score {
                    hits.push(Hit { source: Source::Window(i), score, frecency: pr.window_frecency(i) });
                }
            }
        }
        if wants_apps {
            for (i, e) in q.apps.iter().enumerate() {
                if let Some(score) = self.app_score(p, e, pr.programs[i].as_deref()) {
                    hits.push(Hit { source: Source::App(i), score, frecency: pr.app_frecency[i] });
                }
            }
        }
        if q.mode == Mode::Commands {
            for (i, c) in q.commands.iter().enumerate() {
                if let Some(score) = self.score(p, c) {
                    hits.push(Hit { source: Source::Command(i), score, frecency: pr.command_frecency[i] });
                }
            }
        }
        if q.mode == Mode::Script {
            for (i, l) in q.lines.iter().enumerate() {
                if let Some(score) = self.score(p, l) {
                    hits.push(Hit { source: Source::Line(i), score, frecency: 0 });
                }
            }
        }
        // A name typed whole ("ls") comes before the longer names it also
        // matches ("lsblk", however often used) within its bucket; a window
        // whose app is named so too (it takes its app's score).
        let typed = q.text.trim();
        let is_typed = |text: &str| cmp_ci(text, typed) == Ordering::Equal;
        let exact = |s: Source| match s {
            Source::Window(i) => is_typed(&q.windows[i].title) || pr.window_apps[i].is_some_and(|a| is_typed(&q.apps[a].name)),
            _ => is_typed(name(q, s)),
        };
        hits.sort_by(|a, b| {
            (b.score / SCORE_BUCKET)
                .cmp(&(a.score / SCORE_BUCKET))
                .then_with(|| exact(b.source).cmp(&exact(a.source)))
                .then(b.frecency.cmp(&a.frecency))
                .then(kind(a.source).cmp(&kind(b.source)))
                .then_with(|| cmp_ci(name(q, a.source), name(q, b.source)))
                .then(index(a.source).cmp(&index(b.source)))
        });
        hits
    }
}

/// First app whose StartupWMClass, id without ".desktop", last dot-segment of
/// that id, or Exec program basename equals `class` case-insensitively.
pub fn app_for_window(apps: &[DesktopEntry], class: &str) -> Option<usize> {
    if class.is_empty() {
        return None;
    }
    apps.iter().position(|e| {
        let id = e.id.strip_suffix(".desktop").unwrap_or(&e.id);
        let last = id.rsplit('.').next().unwrap_or(id);
        e.startup_wm_class.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(class))
            || id.eq_ignore_ascii_case(class)
            || last.eq_ignore_ascii_case(class)
            || exec_program(&e.exec).is_some_and(|p| p.eq_ignore_ascii_case(class))
    })
}

/// Basename of the first argv word of an Exec value ("env FOO=1 bar" →
/// "env"; quotes honoured).
pub fn exec_program(exec: &str) -> Option<String> {
    let s = exec.trim_start();
    let word: String = if let Some(rest) = s.strip_prefix('"') {
        let mut w = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => w.extend(chars.next()),
                c => w.push(c),
            }
        }
        w
    } else {
        s.split(char::is_whitespace).next().unwrap_or("").to_string()
    };
    let base = word.rsplit('/').next().unwrap_or("");
    (!base.is_empty()).then(|| base.to_string())
}

#[cfg(test)]
mod tests {
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
        history: History,
    }

    impl Lists {
        fn new() -> Lists {
            Lists { apps: apps(), commands: vec![], windows: vec![], lines: vec![], history: History::empty(200) }
        }

        fn query<'a>(&'a self, mode: Mode, text: &'a str) -> Query<'a> {
            Query {
                text,
                mode,
                apps: &self.apps,
                commands: &self.commands,
                windows: &self.windows,
                lines: &self.lines,
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
        let kinds = |m: Mode| l.rank(m, "fire").iter().map(|h| kind(h.source)).collect::<Vec<_>>();
        assert_eq!(kinds(Mode::All), [0, 1], "All: windows and apps");
        assert_eq!(kinds(Mode::Apps), [1]);
        assert_eq!(kinds(Mode::Windows), [0]);
        assert_eq!(kinds(Mode::Commands), [2]);
        assert_eq!(kinds(Mode::Script), [3]);
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
        l.apps =
            vec![app("cool-decoder.desktop", "Cool Decoder", None, &[], "cool-decoder"), app("code.desktop", "Code", None, &[], "code")];
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
            [Source::Window(0), Source::Window(1), Source::App(2), Source::App(4), Source::App(1), Source::App(0), Source::App(3)],
            "windows as given, then apps by frecency then name (Café Noir, Files, Firefox, VSCodium)"
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
    fn exec_program_is_the_first_word_s_basename() {
        assert_eq!(exec_program("firefox %u").as_deref(), Some("firefox"));
        assert_eq!(exec_program("  /usr/bin/foot -e").as_deref(), Some("foot"));
        assert_eq!(exec_program("env FOO=1 bar").as_deref(), Some("env"));
        assert_eq!(exec_program("\"/opt/My App/bin/my-app\" --x").as_deref(), Some("my-app"));
        assert_eq!(exec_program("\"/opt/a\\\"b/c\\\"d\"").as_deref(), Some("c\"d"));
        assert_eq!(exec_program(""), None);
        assert_eq!(exec_program("/usr/bin/"), None);
    }
}
