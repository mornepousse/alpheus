//! Matching and ranking (spec § 1): `nucleo-matcher` (fzf scoring, smart
//! case, Unicode folding) then frecency from the history. Pure: the lists
//! come in through `Query`, the hits go out as indices into them. The
//! `Ranker` owns the matcher's scratch memory: built when the launcher
//! opens, dropped when it closes (nothing kept at rest).

use crate::compositor::Toplevel;
use crate::launcher::config::SourceConfig;
use crate::launcher::desktop::{DesktopEntry, ExecContext, exec_argv};
use crate::launcher::history::{History, source_id};
use crate::launcher::sources::SourceEntry;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    /// A command-source entry (sources spec § 3): an index into `Query::entries`.
    Entry(usize),
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
    /// Command-source entries: matched in All only, on a typed query.
    pub entries: &'a [SourceEntry],
    /// Their sources (`SourceEntry::source` indexes it): the names key the frecency.
    pub entry_sources: &'a [SourceConfig],
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
    /// How many times the whole `Prepared` (catalog + windows) was computed from scratch
    /// (tests only).
    #[cfg(test)]
    preparations: usize,
    /// How many times only the window → app links were refreshed, catalog kept (tests only).
    #[cfg(test)]
    window_preparations: usize,
}

/// Per-list facts, indexed like the lists of the `Query` they came from. Windows are tracked
/// separately from the catalog (apps, commands, lines): a title change, or a window opening
/// or closing, is common while the launcher is open and must not re-walk the history or
/// re-sort thousands of commands (`Ranker::forget_windows`).
struct Prepared {
    /// Lengths of apps, commands, lines and entries, and `now`: a mismatch recomputes the
    /// catalog (a stale fact never indexes out of range).
    catalog_key: (usize, usize, usize, usize, u64),
    /// Last length the window facts were built for; a mismatch refreshes them even without
    /// an explicit `forget_windows` (the same "never a stale index" guarantee as the catalog).
    windows_len: usize,
    /// Set by `forget_windows`: refreshed at the next `rank` even though the length matches
    /// (a window's title changed, not its count).
    windows_stale: bool,
    app_frecency: Vec<u32>,
    /// Binary of each app's Exec (`exec_program`).
    programs: Vec<Option<String>>,
    command_frecency: Vec<u32>,
    /// Position of each app/command/line in case-insensitive name order (ties by index):
    /// precomputed once so the per-keystroke comparator compares integers, never names.
    app_name_rank: Vec<u32>,
    command_name_rank: Vec<u32>,
    line_name_rank: Vec<u32>,
    entry_frecency: Vec<u32>,
    entry_name_rank: Vec<u32>,
    /// Apps, then commands, in empty-query order (frecency desc, name, index): built once per
    /// catalog so an empty query is a slice of this, not a fresh sort.
    apps_empty_order: Vec<u32>,
    commands_empty_order: Vec<u32>,
    /// Lower-folded id/last-segment/StartupWMClass/program → first app with that key, built
    /// once from the apps (depends on apps alone, kept across a window-only refresh): the
    /// window → app link is then one lookup per window, not a scan of every app.
    class_index: HashMap<String, usize>,
    /// `app_for_window` of each window, via `class_index`.
    window_apps: Vec<Option<usize>>,
    window_name_rank: Vec<u32>,
}

impl Prepared {
    fn new(q: &Query) -> Prepared {
        let (app_map, command_map) = q.history.frecency_maps(q.now);
        let app_frecency: Vec<u32> = q.apps.iter().map(|e| app_map.get(e.id.as_str()).copied().unwrap_or(0)).collect();
        let programs: Vec<Option<String>> = q.apps.iter().map(|e| exec_program(&e.exec)).collect();
        let command_frecency: Vec<u32> = q.commands.iter().map(|c| command_map.get(c.as_str()).copied().unwrap_or(0)).collect();
        let app_name_rank = name_ranks(q.apps.len(), |i| q.apps[i].name.as_str());
        let command_name_rank = name_ranks(q.commands.len(), |i| q.commands[i].as_str());
        let line_name_rank = name_ranks(q.lines.len(), |i| q.lines[i].as_str());
        let entry_frecency = entry_frecency(q);
        let entry_name_rank = name_ranks(q.entries.len(), |i| q.entries[i].entry.as_str());
        let apps_empty_order = order_by_frecency(q.apps.len(), &app_frecency, &app_name_rank);
        let commands_empty_order = order_by_frecency(q.commands.len(), &command_frecency, &command_name_rank);
        let class_index = build_class_index(q.apps, &programs);
        let window_apps: Vec<Option<usize>> = q.windows.iter().map(|w| lookup_class(&class_index, &w.class)).collect();
        let window_name_rank = name_ranks(q.windows.len(), |i| q.windows[i].title.as_str());
        Prepared {
            catalog_key: (q.apps.len(), q.commands.len(), q.lines.len(), q.entries.len(), q.now),
            windows_len: q.windows.len(),
            windows_stale: false,
            app_frecency,
            programs,
            command_frecency,
            app_name_rank,
            command_name_rank,
            line_name_rank,
            entry_frecency,
            entry_name_rank,
            apps_empty_order,
            commands_empty_order,
            class_index,
            window_apps,
            window_name_rank,
        }
    }

    /// Only the window → app links and their name order, reusing `class_index` (apps did not
    /// change): a title edit or a window opening/closing never re-walks the history.
    fn refresh_windows(&mut self, q: &Query) {
        self.window_apps = q.windows.iter().map(|w| lookup_class(&self.class_index, &w.class)).collect();
        self.window_name_rank = name_ranks(q.windows.len(), |i| q.windows[i].title.as_str());
        self.windows_len = q.windows.len();
        self.windows_stale = false;
    }

    /// A window's frecency is its app's.
    fn window_frecency(&self, i: usize) -> u32 {
        self.window_apps[i].map_or(0, |a| self.app_frecency[a])
    }
}

/// Each entry's frecency, from the history's "source:<name>:<line>" ids: the map is built only when
/// there are entries, and a key formatted only when the history holds any source id at all.
fn entry_frecency(q: &Query) -> Vec<u32> {
    if q.entries.is_empty() {
        return Vec::new();
    }
    let map = q.history.source_frecency(q.now);
    if map.is_empty() {
        return vec![0; q.entries.len()];
    }
    q.entries
        .iter()
        .map(|e| q.entry_sources.get(e.source).and_then(|s| map.get(source_id(&s.name, &e.line).as_str()).copied()).unwrap_or(0))
        .collect()
}

/// Position of each of the `len` items (named by `name`) in case-insensitive name order,
/// ties broken by index: computed once per `Prepared` so sorting compares two `u32`s instead
/// of calling `cmp_ci` on every comparison. The fold key is also computed only once per item
/// (`sort_unstable_by` on a `(key, index)` pair, not `cmp_ci` recomputed at every comparison a
/// plain `sort_by` would make) and borrows rather than allocates whenever a name already has
/// no uppercase letter (most commands): for a 20,000-command catalog the difference is an
/// order of magnitude (measured: ≈ 35 ms down to ≈ 1.6 ms, release; see the task's perf report).
fn name_ranks<'a>(len: usize, name: impl Fn(usize) -> &'a str) -> Vec<u32> {
    let mut keyed: Vec<(Cow<'a, str>, u32)> = (0..len).map(|i| (fold_cow(name(i)), i as u32)).collect();
    keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut rank = vec![0u32; len];
    for (r, (_, i)) in keyed.into_iter().enumerate() {
        rank[i as usize] = r as u32;
    }
    rank
}

/// `fold_key`, but borrowing instead of allocating when `s` has no uppercase letter to fold
/// (a plain command name, most of the time): the allocation that a 20,000-item sort can't
/// amortize away, skipped for the common case instead.
fn fold_cow(s: &str) -> Cow<'_, str> {
    // Every byte ASCII (so no accent can be hiding in a multi-byte sequence) and already
    // lower: nothing folding would change.
    if s.bytes().all(|b| b.is_ascii() && !b.is_ascii_uppercase()) { Cow::Borrowed(s) } else { Cow::Owned(fold_key(s)) }
}

/// Indices `0..len`, ordered as an empty query lists them: frecency descending, then name,
/// then index. Built once per catalog (`Prepared`): an empty-query keystroke then costs
/// O(results shown), not a fresh O(n log n) sort.
fn order_by_frecency(len: usize, frecency: &[u32], name_rank: &[u32]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..len as u32).collect();
    // Unstable: fine, since (frecency, name_rank, index) is already a strict total order (the
    // index on its own tells any two elements apart) — one less pass than a stable sort needs.
    order.sort_unstable_by(|&a, &b| {
        frecency[b as usize].cmp(&frecency[a as usize]).then(name_rank[a as usize].cmp(&name_rank[b as usize])).then(a.cmp(&b))
    });
    order
}

/// An id without ".desktop", then its last dot-segment ("org.gnome.Nautilus.desktop" →
/// "org.gnome.Nautilus" → "Nautilus"): shared by `build_class_index` and `app_for_window`,
/// which must agree on exactly the same two candidate keys.
fn id_and_last(id: &str) -> (&str, &str) {
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    let last = id.rsplit('.').next().unwrap_or(id);
    (id, last)
}

/// Lower-folded id / last dot-segment / StartupWMClass / program → first app with that key, in
/// app order (first match wins, as `app_for_window`'s `position` would find). Built once per
/// catalog (apps alone), reused for every window and across a window-only refresh.
fn build_class_index(apps: &[DesktopEntry], programs: &[Option<String>]) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    let mut keys: Vec<&str> = Vec::with_capacity(4);
    for (i, e) in apps.iter().enumerate() {
        let (id, last) = id_and_last(&e.id);
        keys.clear();
        keys.extend(e.startup_wm_class.as_deref());
        keys.push(id);
        keys.push(last);
        keys.extend(programs[i].as_deref());
        // id and its last dot-segment are often the same string (no dots in the id, the
        // common case): fold and insert each distinct key once, not up to four times.
        keys.sort_unstable();
        keys.dedup();
        for key in &keys {
            index.entry(fold_key(key)).or_insert(i);
        }
    }
    index
}

/// Case folded the way `cmp_ci` compares (full Unicode, not just ASCII), as an owned `String`
/// fit for a sort key or a `HashMap`, not just a comparator. The common case (an ASCII
/// command or id) takes the much cheaper byte-wise lowercasing; `char::to_lowercase` maps
/// every ASCII letter to itself, so the two agree whenever `s.is_ascii()`.
fn fold_key(s: &str) -> String {
    if s.is_ascii() { s.to_ascii_lowercase() } else { s.chars().flat_map(char::to_lowercase).collect() }
}

fn lookup_class(index: &HashMap<String, usize>, class: &str) -> Option<usize> {
    if class.is_empty() {
        return None;
    }
    index.get(&fold_key(class)).copied()
}

impl Default for Ranker {
    fn default() -> Ranker {
        Ranker::new()
    }
}

/// Window, App, Entry, Command, Line: the order between kinds at equal score and frecency
/// (sources spec § 3: Window, App, Source, Command).
fn kind(s: Source) -> u8 {
    match s {
        Source::Window(_) => 0,
        Source::App(_) => 1,
        Source::Entry(_) => 2,
        Source::Command(_) => 3,
        Source::Line(_) => 4,
    }
}

fn index(s: Source) -> usize {
    match s {
        Source::App(i) | Source::Window(i) | Source::Command(i) | Source::Line(i) | Source::Entry(i) => i,
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
        Source::Entry(i) => &q.entries[i].entry,
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
            #[cfg(test)]
            window_preparations: 0,
        }
    }

    /// The catalog changed (a scan arrived): every per-list fact, windows included, is
    /// computed again at the next `rank`.
    pub fn forget(&mut self) {
        self.prepared = None;
    }

    /// Only the windows changed (one opened or closed, a title changed): the window → app
    /// links and their name order are refreshed at the next `rank`, the app and command
    /// frecency (already computed from up to 24,000 catalog entries) are kept.
    pub fn forget_windows(&mut self) {
        if let Some(p) = &mut self.prepared {
            p.windows_stale = true;
        }
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

    /// How many times the whole `Prepared` was computed (tests outside this module).
    #[cfg(test)]
    pub fn preparations(&self) -> usize {
        self.preparations
    }

    pub fn rank(&mut self, q: &Query) -> Vec<Hit> {
        let catalog_key = (q.apps.len(), q.commands.len(), q.lines.len(), q.entries.len(), q.now);
        let mut prepared = match self.prepared.take() {
            Some(p) if p.catalog_key == catalog_key => p,
            _ => {
                #[cfg(test)]
                {
                    self.preparations += 1;
                }
                Prepared::new(q)
            }
        };
        if prepared.windows_stale || prepared.windows_len != q.windows.len() {
            #[cfg(test)]
            {
                self.window_preparations += 1;
            }
            prepared.refresh_windows(q);
        }
        // Commands mode with non-blank text that contains whitespace ("git log") is text the
        // model runs through the shell (model.rs's `primary`/`secondary`), never a command to
        // pick: nothing here would be meaningful to select, so nothing is ranked (Mae's
        // feedback round, final review). A blank/whitespace-only query ("", "  ") is still the
        // empty-query case below: there is no typed text to run either way (`shell`'s own
        // `trim().is_empty()` check), so it keeps listing every command.
        let mut hits = if q.mode == Mode::Commands && !q.text.trim().is_empty() && q.text.contains(char::is_whitespace) {
            Vec::new()
        } else {
            let pattern = if q.mode == Mode::Script {
                // dmenu lines are plain text: "!", "^", "'" and "$" are
                // characters to find, not fzf operators.
                Pattern::new(q.text, CaseMatching::Smart, Normalization::Smart, AtomKind::Fuzzy)
            } else {
                Pattern::parse(q.text, CaseMatching::Smart, Normalization::Smart)
            };
            if pattern.atoms.is_empty() { self.everything(q, &prepared) } else { self.matching(q, &prepared, &pattern) }
        };
        self.prepared = Some(prepared);
        hits.truncate(MAX_RESULTS);
        // Many windows open can make `hits` grow past MAX_RESULTS before the truncate above
        // (`everything()`'s windows loop lists every one, uncapped): nothing resident should
        // keep that backing allocation past this call.
        hits.shrink_to_fit();
        hits
    }

    /// Empty query: apps (or commands) by frecency then name first — sliced from `Prepared`'s
    /// precomputed order (O(results shown), not a fresh sort) — then open windows in the
    /// compositor's order (Mae, 2026-10-05: an empty query is about what you launch most, not
    /// about what happens to be open right now), or the dmenu lines as given.
    fn everything(&mut self, q: &Query, pr: &Prepared) -> Vec<Hit> {
        let mut hits = Vec::new();
        if matches!(q.mode, Mode::All | Mode::Apps) {
            // The final `rank` truncates to MAX_RESULTS anyway: never build more of the
            // precomputed order than could survive that truncation. In All, windows are
            // appended next and are never themselves capped (there are rarely many open at
            // once): a catalog larger than MAX_RESULTS must leave them room, or they would be
            // the ones cut off that final truncation, coming after apps in the list.
            let reserved_for_windows = if q.mode == Mode::All { q.windows.len() } else { 0 };
            let remaining = MAX_RESULTS.saturating_sub(hits.len()).saturating_sub(reserved_for_windows);
            hits.extend(pr.apps_empty_order.iter().take(remaining).map(|&i| {
                let i = i as usize;
                Hit { source: Source::App(i), score: 0, frecency: pr.app_frecency[i] }
            }));
        }
        if matches!(q.mode, Mode::All | Mode::Windows) {
            for i in 0..q.windows.len() {
                hits.push(Hit { source: Source::Window(i), score: 0, frecency: pr.window_frecency(i) });
            }
        }
        match q.mode {
            Mode::Commands => {
                let remaining = MAX_RESULTS.saturating_sub(hits.len());
                hits.extend(pr.commands_empty_order.iter().take(remaining).map(|&i| {
                    let i = i as usize;
                    Hit { source: Source::Command(i), score: 0, frecency: pr.command_frecency[i] }
                }));
            }
            Mode::Script => {
                let remaining = MAX_RESULTS.saturating_sub(hits.len());
                hits.extend((0..q.lines.len()).take(remaining).map(|i| Hit { source: Source::Line(i), score: 0, frecency: 0 }));
            }
            Mode::All | Mode::Apps | Mode::Windows => {}
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
        // Command-source entries (sources spec § 3), in All only: a prefix leaves All, so none shows
        // behind one. A hit on the entry ranks as is; a hit that needs the path — scored on the whole
        // line, so "captures zone" finds "Captures · Zone" — ranks below every other hit.
        let mut path_hits = Vec::new();
        if q.mode == Mode::All {
            for (i, e) in q.entries.iter().enumerate() {
                let frecency = pr.entry_frecency[i];
                if let Some(score) = self.score(p, &e.entry) {
                    hits.push(Hit { source: Source::Entry(i), score, frecency });
                } else if e.path.is_some()
                    && let Some(score) = self.score(p, &e.line)
                {
                    path_hits.push(Hit { source: Source::Entry(i), score, frecency });
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
        let mut ranked = finish(hits, q, pr);
        if !path_hits.is_empty() {
            ranked.extend(finish(path_hits, q, pr));
        }
        ranked
    }
}

/// Sorts (score bucket desc, a name typed in full first, frecency desc, kind, name, index) and
/// keeps only the top `MAX_RESULTS`: `select_nth_unstable_by` partitions the top results, then
/// only those are sorted — the comparator never calls `cmp_ci` or the typed-whole check itself
/// (both precomputed once per hit, not once per comparison), and the result is rebuilt into a
/// right-sized `Vec` (never keeps a catalog-sized allocation around).
fn finish(hits: Vec<Hit>, q: &Query, pr: &Prepared) -> Vec<Hit> {
    // A name typed whole ("ls") comes before the longer names it also matches ("lsblk",
    // however often used) within its bucket; a window whose app is named so too (it takes
    // its app's score).
    let typed = q.text.trim();
    let exact: Vec<bool> = hits.iter().map(|h| is_exact(q, pr, h.source, typed)).collect();
    let name_rank = |s: Source| -> u32 {
        match s {
            Source::App(i) => pr.app_name_rank[i],
            Source::Window(i) => pr.window_name_rank[i],
            Source::Command(i) => pr.command_name_rank[i],
            Source::Line(i) => pr.line_name_rank[i],
            Source::Entry(i) => pr.entry_name_rank[i],
        }
    };
    let cmp = |&ia: &usize, &ib: &usize| -> Ordering {
        let (a, b) = (hits[ia], hits[ib]);
        (b.score / SCORE_BUCKET)
            .cmp(&(a.score / SCORE_BUCKET))
            .then_with(|| exact[ib].cmp(&exact[ia]))
            .then(b.frecency.cmp(&a.frecency))
            .then(kind(a.source).cmp(&kind(b.source)))
            .then_with(|| name_rank(a.source).cmp(&name_rank(b.source)))
            .then(index(a.source).cmp(&index(b.source)))
    };
    let mut order: Vec<usize> = (0..hits.len()).collect();
    if order.len() > MAX_RESULTS {
        order.select_nth_unstable_by(MAX_RESULTS, cmp);
        order.truncate(MAX_RESULTS);
    }
    order.sort_by(cmp);
    order.into_iter().map(|i| hits[i]).collect()
}

/// Whether `s`'s own name is `typed`, case- and accent-folded the way nucleo's Smart
/// normalization treats a needle without accents ("cafe" typed whole matches "Café"); a
/// window takes its app's name too (the app's score and frecency, above, are shared the
/// same way).
fn is_exact(q: &Query, pr: &Prepared, s: Source, typed: &str) -> bool {
    match s {
        Source::Window(i) => folded_eq(&q.windows[i].title, typed) || pr.window_apps[i].is_some_and(|a| folded_eq(&q.apps[a].name, typed)),
        _ => folded_eq(name(q, s), typed),
    }
}

fn folded_eq(a: &str, b: &str) -> bool {
    a.chars().map(fold).eq(b.chars().map(fold))
}

/// Case- and accent-fold a character the way nucleo's `Normalization::Smart` +
/// `CaseMatching::Smart` do once both apply (`chars::normalize` then `chars::to_lower_case`),
/// so `é` and `e` compare equal here exactly as they do inside the matcher.
fn fold(c: char) -> char {
    nucleo_matcher::chars::to_lower_case(nucleo_matcher::chars::normalize(c))
}

/// First app whose StartupWMClass, id without ".desktop", last dot-segment of
/// that id, or Exec program basename equals `class` case-insensitively (full Unicode case
/// folding, not just ASCII: a class in Cyrillic or accented Latin still matches).
pub fn app_for_window(apps: &[DesktopEntry], class: &str) -> Option<usize> {
    if class.is_empty() {
        return None;
    }
    let eq = |a: &str| cmp_ci(a, class) == Ordering::Equal;
    apps.iter().position(|e| {
        let (id, last) = id_and_last(&e.id);
        e.startup_wm_class.as_deref().is_some_and(eq) || eq(id) || eq(last) || exec_program(&e.exec).is_some_and(|p| eq(&p))
    })
}

/// Full path of a $PATH command, the first of `path_dirs` where it is an actual file ("btop"
/// → the first ".../btop" found); None when it is not there (removed since the scan, or
/// `path_dirs` not known yet — the preview then falls back to naming $PATH alone). Resolved
/// at preview time, not kept: the catalog's `commands` are names only (spec § 1).
pub fn command_path(path_dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    path_dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Basename of the first argv word of an Exec value ("env FOO=1 bar" →
/// "env"), through `exec_argv`'s own quoting (single quotes, a backslash
/// outside quotes, field codes) rather than a hand-rolled subset of it: a
/// name and icon are never relevant to just naming the program, so an
/// empty/absent placeholder context stands in for the real one.
pub fn exec_program(exec: &str) -> Option<String> {
    let ctx = ExecContext { name: "", icon: None, path: Path::new("") };
    let word = exec_argv(exec, &ctx).ok()?.into_iter().next()?;
    let base = word.rsplit('/').next().unwrap_or("");
    (!base.is_empty()).then_some(base.to_string())
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
        let commands: Vec<String> =
            (0..4_000u64).map(|i| format!("{}-{}", words[rng.below(words.len() as u64) as usize], i % 12)).collect();
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
}
