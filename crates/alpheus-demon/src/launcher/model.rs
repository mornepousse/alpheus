//! The launcher's state and every key rule, pure (spec § 2): the query,
//! the mode (and the prefixes that jump to one), the selection and the
//! visible window of results, what Enter and Ctrl+Enter activate.
//! Wayland feeds it keys and clicks and applies the `Outcome`;
//! nothing here touches the session.

use crate::compositor::Toplevel;
use crate::keys::Key;
use crate::launcher::calc::{Calc, evaluate, looks_like_math};
use crate::launcher::config::LauncherConfig;
use crate::launcher::history::{History, source_key};
use crate::launcher::matching::{Hit, Mode, Query, Ranker, Source};
use crate::launcher::prefix::{self, Prefix, search_url};
use crate::launcher::scan::Catalog;
use crate::launcher::sources::Entries;
use crate::list;
use alpheus_commun::launcher_ipc::{DmenuFlags, wipe};
use std::path::PathBuf;

/// Bytes reserved for a password's query at open (`for_dmenu`).
const PASSWORD_ROOM: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// action: index into DesktopEntry.actions (None: the main Exec).
    App {
        index: usize,
        action: Option<usize>,
    },
    Window(usize),
    /// A $PATH command, by itself (never through `sh -c`); terminal: held open in the
    /// configured terminal (Enter, `terminal_hold`); false: detached, no terminal (Ctrl+Enter).
    Command {
        index: usize,
        terminal: bool,
    },
    /// Typed text run through `sh -c` (Commands mode only); terminal: held open in the
    /// configured terminal (Enter, `terminal_hold`); false: detached, no terminal (Ctrl+Enter).
    Shell {
        text: String,
        terminal: bool,
    },
    /// Script mode: the chosen line and its position in the client's order (`--index`), or the
    /// typed text when nothing matched (line None).
    Choose {
        text: String,
        line: Option<usize>,
    },
    /// A command-source entry (sources spec § 3): its source's `run` with the whole line.
    Entry(usize),
    /// The calculator's text (small features 1) for the clipboard: handed to `Config::copy` on its
    /// stdin, never on an argv.
    Copy {
        text: String,
    },
    /// The `?` row: `Config::open` with this URL appended, in a scope like an app; never recorded.
    Open {
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    Redraw,
    Close,
    Activate(Activation),
}

/// A `Source`'s stable identity (`Launcher::identity`), private: how the selection survives a
/// refilter without pinning it to an index that a reshuffle can hand to a different item.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Identity {
    Window(String),
    App(String),
    Command(String),
    Line(usize),
    /// Its history key (`source_key`): the source's name and the whole line.
    Entry(String),
    Calc,
    /// The prefix rows have one position each: nothing to carry across a refilter.
    Search,
    Emoji(usize),
    Shell,
}

pub struct Launcher {
    pub config: LauncherConfig,
    pub mode: Mode,
    pub query: String,
    pub selected: usize,
    /// First visible result row.
    pub top: usize,
    /// Shown on the help line in the error style until the next key.
    pub message: Option<String>,
    pub prompt: Option<String>,
    /// Changed only through `set_catalog`: assigning it directly would leave the ranker's
    /// per-catalog facts (and the selection) stale — same reason as `windows` below.
    pub catalog: Option<Catalog>,
    /// Command-source entries (sources spec § 3): None until the scan delivers them (never with no
    /// source configured). Changed only through `set_entries`, for the reason `catalog` is.
    pub entries: Option<Entries>,
    /// Changed only through `set_windows`: a direct assignment skips `ranker.forget_windows()`
    /// and the identity-preserving reselection `set_windows` does (`Launcher::identity`).
    pub windows: Vec<Toplevel>,
    /// Script mode only: set once by `for_script`, never mutated afterward (no `set_lines` —
    /// the dmenu line list is fixed for the life of one open).
    pub lines: Vec<String>,
    /// Script mode, `--index`: Enter answers the line's position (`Reply::Index`), not its text.
    pub index: bool,
    /// Script mode, `--password`: no list, the query drawn as dots, wiped on drop (`Drop`).
    pub password: bool,
    /// Script mode, `--visible N`: an empty query lists the first N lines only (sources spec § 4).
    pub visible: Option<usize>,
    /// $PATH directories, in order, for the preview's "full path" of a command (the host sets
    /// them from the same `ScanEnv` the scan used); empty until then, or if never set — the
    /// preview falls back to naming $PATH alone.
    pub path_dirs: Vec<PathBuf>,
    pub history: History,
    pub now: u64,
    pub results: Vec<Hit>,
    /// The calculator's answer for the query (small features 1): None outside `Mode::All` and
    /// `Mode::Calc`, for a query that is no expression, or in All for one that does not look like
    /// math. Set by `rank`, with the row it makes.
    pub calc: Option<Calc>,
    ranker: Ranker,
    /// The current mode was entered by typing its prefix: Backspace on the
    /// empty query goes back to All.
    by_prefix: bool,
    list_rows: usize,
    /// Whether the user has navigated the selection since the last query or mode change: an
    /// arrow key, PageUp/PageDown, Home/End or the wheel, and only on a non-empty list
    /// (`move_by`). Set back to false by every `refilter()` — a typed character, Backspace,
    /// Ctrl+U, Ctrl+W, Tab/BackTab, a prefix jump (Mae, 2026-10-05): the selection is on top
    /// unless the user navigated since the last query or mode change. Until then, a catalog
    /// or window update puts the selection back on top instead of carrying it along by
    /// identity: the catalog arrives after the first window has been auto-selected, and a
    /// hasty Enter must not land on that window once apps outrank it. Once `moved` is set,
    /// `identity` keeps tracking the user's choice as before, until the next refilter.
    moved: bool,
}

impl Launcher {
    fn new(config: LauncherConfig, mode: Mode) -> Launcher {
        let list_rows = usize::from(config.rows).max(1);
        Launcher {
            config,
            mode,
            query: String::new(),
            selected: 0,
            top: 0,
            message: None,
            prompt: None,
            catalog: None,
            entries: None,
            windows: Vec::new(),
            lines: Vec::new(),
            index: false,
            password: false,
            visible: None,
            path_dirs: Vec::new(),
            history: History::empty(0),
            now: 0,
            results: Vec::new(),
            calc: None,
            ranker: Ranker::new(),
            by_prefix: false,
            list_rows,
            moved: false,
        }
    }

    /// The app launcher: windows at once, apps and commands once the scan
    /// delivers its catalog (`set_catalog`).
    pub fn for_apps(config: LauncherConfig, windows: Vec<Toplevel>, history: History, now: u64) -> Launcher {
        let mut l = Launcher::new(config, Mode::All);
        l.windows = windows;
        l.history = history;
        l.now = now;
        l.refilter();
        l
    }

    /// The dmenu menu: the client's lines, its prompt, no modes, no history.
    pub fn for_script(config: LauncherConfig, prompt: Option<String>, lines: Vec<String>) -> Launcher {
        Launcher::for_dmenu(config, prompt, lines, DmenuFlags::default())
    }

    /// `for_script` with the client's flags. `password`: the lines are dropped (no list), and
    /// the query gets room up front, so typing a usual password never reallocates (each
    /// reallocation would leave a copy behind in freed memory that the drop cannot wipe).
    pub fn for_dmenu(config: LauncherConfig, prompt: Option<String>, lines: Vec<String>, flags: DmenuFlags) -> Launcher {
        let mut l = Launcher::new(config, Mode::Script);
        l.prompt = prompt;
        l.index = flags.index;
        l.password = flags.password;
        if flags.password {
            l.query = String::with_capacity(PASSWORD_ROOM);
        } else {
            l.lines = lines;
        }
        l.visible = if flags.password { None } else { flags.visible };
        l.refilter();
        l
    }

    /// The calculator alone (`--calc`): no windows, no history, no catalog, no modes.
    pub fn for_calc(config: LauncherConfig) -> Launcher {
        let mut l = Launcher::new(config, Mode::Calc);
        l.refilter();
        l
    }

    pub fn is_script(&self) -> bool {
        self.mode == Mode::Script
    }

    /// A menu with no modes row and no preview: a script's, or the calculator's.
    pub fn is_bare(&self) -> bool {
        matches!(self.mode, Mode::Script | Mode::Calc)
    }

    /// Refilters; keeps the selection on the same item if it is still present (by identity,
    /// not by its old index — see `identity`), else 0.
    pub fn set_catalog(&mut self, catalog: Catalog) {
        let kept = self.selected_hit().and_then(|h| self.identity(h.source));
        self.catalog = Some(catalog);
        self.ranker.forget();
        self.refilter_keeping(kept);
    }

    pub fn set_windows(&mut self, windows: Vec<Toplevel>) {
        let kept = self.selected_hit().and_then(|h| self.identity(h.source));
        self.windows = windows;
        self.ranker.forget_windows();
        self.refilter_keeping(kept);
    }

    /// The sources' entries, the open's second delivery (sources spec § 6): refilters, keeping the
    /// selection by identity like `set_catalog` (back on top unless the user moved).
    pub fn set_entries(&mut self, entries: Entries) {
        let kept = self.selected_hit().and_then(|h| self.identity(h.source));
        self.entries = Some(entries);
        self.ranker.forget();
        self.refilter_keeping(kept);
    }

    /// What makes a result the "same" one across a refilter, independent of its numeric index:
    /// a window by its `Toplevel::selector` (a window's index shifts whenever another window
    /// opens or closes before it in the list), an app by its `DesktopEntry::id`, a command by
    /// its name; a dmenu line has no identity beyond its position (the list never changes while
    /// the launcher is open), so it keeps the old rule. Read from `self.windows`/`self.catalog`
    /// as they are *now*: called both before and after `set_windows`/`set_catalog` replace them.
    fn identity(&self, source: Source) -> Option<Identity> {
        Some(match source {
            Source::Window(i) => Identity::Window(self.windows.get(i)?.selector.clone()),
            Source::App(i) => Identity::App(self.catalog.as_ref()?.apps.get(i)?.id.clone()),
            Source::Command(i) => Identity::Command(self.catalog.as_ref()?.commands.get(i)?.clone()),
            Source::Line(i) => Identity::Line(i),
            Source::Calc => Identity::Calc,
            Source::Search => Identity::Search,
            Source::Emoji(i) => Identity::Emoji(i),
            Source::Shell => Identity::Shell,
            Source::Entry(i) => {
                let e = self.entries.as_ref()?;
                let item = e.items.get(i)?;
                Identity::Entry(source_key(&e.sources.get(item.source)?.name, &item.line))
            }
        })
    }

    /// Rows the view shows (PageUp/PageDown step, `top` clamping).
    pub fn set_list_rows(&mut self, rows: usize) {
        self.list_rows = rows.max(1);
        self.keep_visible();
    }

    pub fn key(&mut self, k: Key) -> Outcome {
        let had_message = self.message.take().is_some();
        let o = self.apply(k);
        if had_message && o == Outcome::Nothing { Outcome::Redraw } else { o }
    }

    fn apply(&mut self, k: Key) -> Outcome {
        let page = i32::try_from(self.list_rows).unwrap_or(i32::MAX);
        match k {
            Key::Char(c) => {
                self.query.push(c);
                self.take_prefix();
                self.query_changed()
            }
            Key::Backspace => {
                if self.query.pop().is_some() {
                    self.query_changed()
                } else if self.by_prefix {
                    self.by_prefix = false;
                    self.mode = Mode::All;
                    self.query_changed()
                } else {
                    Outcome::Nothing
                }
            }
            Key::CtrlU => {
                if self.query.is_empty() {
                    return Outcome::Nothing;
                }
                self.query.clear();
                self.query_changed()
            }
            Key::CtrlW => {
                if self.query.is_empty() {
                    return Outcome::Nothing;
                }
                let kept = self.query.trim_end().trim_end_matches(|c: char| !c.is_whitespace()).len();
                self.query.truncate(kept);
                self.query_changed()
            }
            Key::Up | Key::CtrlP => self.move_by(-1),
            Key::Down | Key::CtrlN => self.move_by(1),
            Key::PageUp => self.move_by(-page),
            Key::PageDown => self.move_by(page),
            Key::Home => self.move_by(i32::MIN),
            Key::End => self.move_by(i32::MAX),
            Key::Tab => self.switch_mode(self.mode.next()),
            Key::BackTab => self.switch_mode(self.mode.previous()),
            // No binding: the preview panel they used to toggle always shows now (view.rs,
            // 2026-10-05) — ignored rather than removed from `Key`, so a future rebind needs
            // no change to `keys.rs`.
            Key::Left | Key::Right | Key::Delete => Outcome::Nothing,
            Key::Escape => Outcome::Close,
            Key::Enter => self.primary(),
            Key::CtrlEnter => self.secondary(),
        }
    }

    /// In All mode (not already entered by a prefix), a query equal to a
    /// configured prefix switches to that mode and is consumed.
    fn take_prefix(&mut self) {
        if self.mode != Mode::All || self.by_prefix {
            return;
        }
        let p = &self.config.prefixes;
        let mode = if self.query == p.commands {
            Mode::Commands
        } else if self.query == p.windows {
            Mode::Windows
        } else {
            return;
        };
        self.mode = mode;
        self.query.clear();
        self.by_prefix = true;
    }

    fn switch_mode(&mut self, m: Mode) -> Outcome {
        if self.is_bare() || m == self.mode {
            return Outcome::Nothing;
        }
        self.mode = m;
        self.by_prefix = false;
        self.query_changed()
    }

    /// The query's prefix (`?`, `:`, `$`) and its text: only in All and never under `--password`
    /// (the secret is not parsed). A configured mode prefix is taken before, by `take_prefix`.
    pub fn prefixed(&self) -> Option<(Prefix, &str)> {
        if self.mode != Mode::All || self.password {
            return None;
        }
        prefix::split(&self.query, self.config.search_enabled())
    }

    fn query_changed(&mut self) -> Outcome {
        self.message = None;
        self.refilter();
        Outcome::Redraw
    }

    fn rank(&mut self) {
        // `--password`: no list to rank, and a nucleo pattern built from the query would copy
        // the secret into heap buffers the drop cannot wipe.
        if self.password {
            self.results.clear();
            self.calc = None;
            return;
        }
        // A prefix lists only its own rows: no catalog ranking, no calculator.
        if let Some((p, text)) = self.prefixed() {
            let row = (p == Prefix::Search && !text.is_empty()).then_some(Hit { source: Source::Search, score: u32::MAX, frecency: 0 });
            self.results = row.into_iter().collect();
            self.calc = None;
            return;
        }
        let (apps, commands) = match &self.catalog {
            Some(c) => (&c.apps[..], &c.commands[..]),
            None => (&[][..], &[][..]),
        };
        let (entries, entry_sources) = match &self.entries {
            Some(e) => (&e.items[..], &e.sources[..]),
            None => (&[][..], &[][..]),
        };
        let q = Query {
            text: &self.query,
            mode: self.mode,
            apps,
            commands,
            windows: &self.windows,
            lines: &self.lines,
            entries,
            entry_sources,
            history: &self.history,
            now: self.now,
        };
        self.results = self.ranker.rank(&q);
        // `--visible N`: an empty query lists the first N lines (`everything` keeps input order).
        // Empty as the ranker sees it: a blank query ("  ") holds no atom and lists every line.
        if let Some(n) = self.visible
            && self.query.trim().is_empty()
        {
            self.results.truncate(n);
        }
        // The calculator row: first in All (for a query that looks like math), alone in Calc.
        self.calc = match self.mode {
            Mode::All => evaluate(&self.query).filter(|_| looks_like_math(&self.query)),
            Mode::Calc => evaluate(&self.query),
            _ => None,
        };
        if self.calc.is_some() {
            self.results.insert(0, Hit { source: Source::Calc, score: u32::MAX, frecency: 0 });
        }
    }

    /// New results, selection back on the first one. A typed character, Backspace, Ctrl+U,
    /// Ctrl+W, Tab/BackTab and a prefix jump all go through here: each one resets `moved`
    /// (Mae, 2026-10-05) — the selection is on top unless the user navigated since the last
    /// query or mode change.
    fn refilter(&mut self) {
        self.rank();
        self.selected = 0;
        self.top = 0;
        self.moved = false;
    }

    /// Re-ranks. Before the user has moved the selection (`self.moved`), a catalog or window
    /// update always lands back on top (index 0): the default selection is a placeholder, not
    /// a choice, and must not survive apps outranking the window it happened to start on
    /// (Mae, 2026-10-05). Once the user has moved, puts the selection back on `kept` (captured
    /// by the caller *before* replacing the list it identifies into — see
    /// `set_catalog`/`set_windows`) if it is still among the results, else 0.
    fn refilter_keeping(&mut self, kept: Option<Identity>) {
        self.rank();
        self.selected = if self.moved {
            kept.and_then(|id| self.results.iter().position(|h| self.identity(h.source).as_ref() == Some(&id))).unwrap_or(0)
        } else {
            0
        };
        self.keep_visible();
    }

    fn keep_visible(&mut self) {
        self.top = list::keep_visible(self.selected, self.top, self.list_rows, self.results.len());
    }

    /// Arrow keys, PageUp/PageDown, Home/End and the wheel (`scroll`) all land here: the only
    /// keys that set `moved` (Mae, 2026-10-05), and only when the selection actually changes —
    /// an empty list, or a clamped move at an edge (already the top or bottom result), leaves
    /// `moved` untouched. Every result can be selected: `list::step` is a plain clamp here (no
    /// wrap).
    fn move_by(&mut self, delta: i32) -> Outcome {
        if self.results.is_empty() {
            return Outcome::Nothing;
        }
        let to = list::step(self.selected, delta, self.results.len(), |_| true);
        if to == self.selected {
            return Outcome::Nothing;
        }
        self.moved = true;
        self.selected = to;
        self.keep_visible();
        Outcome::Redraw
    }

    fn app_has_actions(&self, index: usize) -> bool {
        self.catalog.as_ref().and_then(|c| c.apps.get(index)).is_some_and(|e| !e.actions.is_empty())
    }

    /// The query, when it is something to run through the shell.
    fn shell(&self, terminal: bool) -> Outcome {
        if self.query.trim().is_empty() {
            Outcome::Nothing
        } else {
            Outcome::Activate(Activation::Shell { text: self.query.clone(), terminal })
        }
    }

    /// Enter: opens an app, focuses a window, picks a dmenu line — and, for a command or typed
    /// shell text, runs it held open in the configured terminal (`terminal_hold`, Mae
    /// 2026-10-05: the command's output stays on screen after it exits).
    fn primary(&self) -> Outcome {
        let hit = self.selected_hit();
        match self.mode {
            Mode::Script => match hit {
                Some(Hit { source: Source::Line(i), .. }) => self.choose_line(i),
                _ if !self.query.is_empty() => Outcome::Activate(Activation::Choose { text: self.query.clone(), line: None }),
                _ => Outcome::Nothing,
            },
            Mode::Commands if hit.is_none() || self.query.contains(char::is_whitespace) => self.shell(true),
            _ => match hit.map(|h| h.source) {
                Some(Source::App(index)) => Outcome::Activate(Activation::App { index, action: None }),
                Some(Source::Window(i)) => Outcome::Activate(Activation::Window(i)),
                Some(Source::Command(index)) => Outcome::Activate(Activation::Command { index, terminal: true }),
                Some(Source::Line(i)) => self.choose_line(i),
                Some(Source::Entry(i)) => Outcome::Activate(Activation::Entry(i)),
                Some(Source::Calc) => self.copy(false),
                Some(Source::Search) => self.search(),
                // Rows of the `:` and `$` prefixes arrive with their own tasks.
                Some(Source::Emoji(_) | Source::Shell) | None => Outcome::Nothing,
            },
        }
    }

    /// The `?` row: the URL for the typed text.
    fn search(&self) -> Outcome {
        match self.prefixed() {
            Some((Prefix::Search, text)) if !text.is_empty() => {
                Outcome::Activate(Activation::Open { url: search_url(&self.config.search, text) })
            }
            _ => Outcome::Nothing,
        }
    }

    /// The calculator row's copy: the result, or with `expression` the query and the result
    /// ("2+2 = 4"). An error answer, or no `copy` command, copies nothing.
    fn copy(&self, expression: bool) -> Outcome {
        match &self.calc {
            Some(Ok(answer)) if !self.config.copy.is_empty() => {
                let text = if expression { format!("{} = {}", self.query.trim(), answer.text) } else { answer.text.clone() };
                Outcome::Activate(Activation::Copy { text })
            }
            _ => Outcome::Nothing,
        }
    }

    /// Line `i` of the client's (its position on stdin, whatever the ranking did).
    fn choose_line(&self, i: usize) -> Outcome {
        match self.lines.get(i) {
            Some(text) => Outcome::Activate(Activation::Choose { text: text.clone(), line: Some(i) }),
            None => Outcome::Nothing,
        }
    }

    /// Ctrl+Enter: an app's first `.desktop` action (or its main Exec without one), a window as
    /// Enter — and, for a command or typed shell text, runs it detached with no terminal at all
    /// (Mae 2026-10-05).
    fn secondary(&self) -> Outcome {
        let hit = self.selected_hit().map(|h| h.source);
        match (self.mode, hit) {
            (Mode::Commands, Some(Source::Command(index))) if !self.query.contains(char::is_whitespace) => {
                Outcome::Activate(Activation::Command { index, terminal: false })
            }
            (Mode::Commands, _) => self.shell(false),
            (_, Some(Source::Calc)) => self.copy(true),
            (_, Some(Source::App(index))) => {
                let action = self.app_has_actions(index).then_some(0);
                Outcome::Activate(Activation::App { index, action })
            }
            _ => self.primary(),
        }
    }

    /// Click on result `index` (absolute index into `results`): select then activate. Counts
    /// as navigation like `move_by` (Mae, 2026-10-05): a later catalog/window update pins the
    /// clicked item by identity instead of landing back on top. Like `key`, turns a `Nothing`
    /// into a `Redraw` when it clears a message — otherwise the error line would linger after
    /// a click that already invalidated it.
    pub fn click(&mut self, index: usize, secondary: bool) -> Outcome {
        if index >= self.results.len() {
            return Outcome::Nothing;
        }
        let had_message = self.message.take().is_some();
        self.moved = true;
        self.selected = index;
        self.keep_visible();
        let o = if secondary { self.secondary() } else { self.primary() };
        if had_message && o == Outcome::Nothing { Outcome::Redraw } else { o }
    }

    pub fn click_mode(&mut self, m: Mode) -> Outcome {
        if m == Mode::Script {
            return Outcome::Nothing;
        }
        self.switch_mode(m)
    }

    /// Wheel: moves the selection by `rows`, clamped.
    pub fn scroll(&mut self, rows: i32) -> Outcome {
        self.move_by(rows)
    }

    pub fn selected_hit(&self) -> Option<Hit> {
        self.results.get(self.selected).copied()
    }

    pub fn show_message(&mut self, m: String) {
        self.message = Some(m);
    }
}

/// `--password`: the typed text is overwritten before it is freed (where practical: the copies
/// SCTK and xkbcommon made of each key are out of reach).
impl Drop for Launcher {
    fn drop(&mut self) {
        if self.password {
            wipe(&mut self.query);
        }
    }
}

#[cfg(test)]
mod tests;
