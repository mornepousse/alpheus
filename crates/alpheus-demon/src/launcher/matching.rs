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
        // feedback round). A blank/whitespace-only query ("", "  ") is still the
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
mod tests;
