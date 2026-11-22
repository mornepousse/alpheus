//! The launcher's state and every key rule, pure (spec § 2): the query,
//! the mode (and the prefixes that jump to one), the selection and the
//! visible window of results, what Enter and Ctrl+Enter activate.
//! Wayland feeds it keys and clicks and applies the `Outcome`;
//! nothing here touches the session.

use crate::clipboard::history::{ClipRow, MAX_PINS};
use crate::compositor::Toplevel;
use crate::keys::Key;
use crate::launcher::calc::{Calc, evaluate, looks_like_math};
use crate::launcher::config::LauncherConfig;
use crate::launcher::emoji;
use crate::launcher::history::{History, source_key};
use crate::launcher::matching::{Hit, Mode, Query, Ranker, Source, ToolHitRow};
use crate::launcher::prefix::{self, Prefix, search_url};
use crate::launcher::scan::Catalog;
use crate::launcher::sources::Entries;
use crate::launcher::tools::catalog::Catalog as ToolsCatalog;
use crate::launcher::tools::nav::{Job, Level, RowId, Step, ToolAction, ToolRow, ToolsState};
use crate::list;
use crate::session;
use alpheus_commun::launcher_ipc::{DmenuFlags, wipe};
use std::path::PathBuf;
use std::time::Instant;

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
    /// A clipboard history row's Enter: the history's entry (by id, never its text) goes back to
    /// the selection. Never performed by `run::perform`: the app handles it first.
    Clip {
        id: u64,
    },
    /// A Tools menu row (tools spec § 5): what the glue runs after the box closed.
    Tool(ToolAction),
}

/// A change to the clipboard history the launcher asks for: ids only, never text. The model has
/// already applied it to its own rows; the app applies it to the real history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipEdit {
    /// Toggle the entry's pin.
    Pin(u64),
    Delete(u64),
    /// Forget every unpinned entry.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    Redraw,
    Close,
    Activate(Activation),
    /// The history changed (pin, delete, clear): the box stays open.
    Edit(ClipEdit),
    /// The Tools mode was entered by the `/` prefix: the host reads the built-ins' states and the
    /// reader reads the entries' (the job, when any has a `state`). A drill or a confirm from a
    /// Mod+Space hit reads none (the rows show no switch there; spec § 1 allows it).
    EnterTools(Option<Job>),
    /// A `choices_cmd` level was opened: the reader reads its lines.
    ReadChoices(Job),
    /// Backspace at the Tools root of a Mod+Space open: the model is back in All.
    BackToAll,
    /// A confirmation question was asked: the host syncs its timer.
    Asked,
}

/// How the launcher was opened: what scan deliveries and window lists it takes (tools spec § 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Mod+Space: the apps, the windows, the sources' entries, the catalog; the Tools mode is a jump.
    Apps,
    /// `alpheus-launch --tools`: the Tools mode alone.
    Tools,
    /// A script menu, the calculator, the history, the window switcher.
    Other,
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
    /// A history row by its entry's id: the row's place changes when rows go.
    Clip(u64),
    /// A tool row by its identity in the catalog.
    Tool(RowId),
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
    /// The clipboard history's rows, a snapshot at open (clipboard spec): empty unless the history
    /// is on. Changed through `set_clips` and the keys only; wiped when the box drops.
    pub clips: Vec<ClipRow>,
    /// The `"` prefix is claimed in All (the history is on and a protocol bound).
    pub clipboard: bool,
    /// How this open began (`for_apps`, `for_tools`, the others).
    pub origin: Origin,
    /// The Tools menu's state: the catalog, the level, the states read. None until `set_tools` or
    /// `for_tools`; dropped with the launcher.
    tools: Option<ToolsState>,
    /// The Tools mode's rows of the open level (the flat search's are the state's own).
    tool_level: Vec<ToolRow>,
    tool_level_hits: Vec<ToolHitRow>,
    /// The level and `ToolsState::rev` the level's rows were built at (None: not built).
    tool_level_key: Option<(Level, u64)>,
    /// How many times the level's rows were built (tests: a keystroke builds none).
    #[cfg(test)]
    tool_level_builds: u32,
    /// Whether `Source::Tool` hits index the flat search's rows (All, and a typed root query).
    tool_flat_shown: bool,
    /// When the Clear row asked `Clear? ⏎`: it never expires (no timer), only another key or click cancels it.
    clear_asked: Option<Instant>,
    /// The instant of the key or click being handled: the host sets it before each one and the
    /// event uses it once (reset after it, so a stale instant never answers a later event).
    at: Option<Instant>,
    ranker: Ranker,
    /// The current mode was entered by typing its prefix: Backspace on the
    /// empty query goes back to All.
    by_prefix: bool,
    list_rows: usize,
    /// Whether the user has navigated the selection since the last query or mode change: an
    /// arrow key, PageUp/PageDown, Home/End or the wheel, and only on a non-empty list
    /// (`move_by`), or a switcher's Tab/Shift+Tab (`step`). Set back to false by every
    /// `refilter()` — a typed character, Backspace,
    /// Ctrl+U, Ctrl+W, Tab/BackTab (outside the switcher, where they step), a prefix jump (Mae, 2026-10-05): the selection is on top
    /// unless the user navigated since the last query or mode change. Until then, a catalog
    /// or window update puts the selection back on top instead of carrying it along by
    /// identity: the catalog arrives after the first window has been auto-selected, and a
    /// hasty Enter must not land on that window once apps outrank it. Once `moved` is set,
    /// `identity` keeps tracking the user's choice as before, until the next refilter.
    moved: bool,
    /// The window switcher (launcher 2): `for_windows` only. Tab steps instead of switching mode.
    switcher: bool,
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
            clips: Vec::new(),
            clipboard: false,
            origin: Origin::Other,
            tools: None,
            tool_level: Vec::new(),
            tool_level_hits: Vec::new(),
            tool_level_key: None,
            #[cfg(test)]
            tool_level_builds: 0,
            tool_flat_shown: true,
            clear_asked: None,
            at: None,
            ranker: Ranker::new(),
            by_prefix: false,
            list_rows,
            moved: false,
            switcher: false,
        }
    }

    /// The app launcher: windows at once, apps and commands once the scan
    /// delivers its catalog (`set_catalog`).
    pub fn for_apps(config: LauncherConfig, windows: Vec<Toplevel>, history: History, now: u64) -> Launcher {
        let mut l = Launcher::new(config, Mode::All);
        l.origin = Origin::Apps;
        l.windows = windows;
        l.history = history;
        l.now = now;
        l.refilter();
        l
    }

    /// The window switcher: windows only, in the backends' order, no history and no catalog; the
    /// selection starts on the previous window (the first one).
    pub fn for_windows(config: LauncherConfig, windows: Vec<Toplevel>) -> Launcher {
        let mut l = Launcher::new(config, Mode::Windows);
        l.switcher = true;
        l.windows = windows;
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

    /// The history alone (`alpheus-launch --clipboard`): its rows, no modes row, no catalog.
    pub fn for_clipboard(config: LauncherConfig, rows: Vec<ClipRow>) -> Launcher {
        let mut l = Launcher::new(config, Mode::Clipboard);
        l.clips = rows;
        l.refilter();
        l
    }

    /// The Tools menu alone (`alpheus-launch --tools`): the root of its catalog, no modes row, no
    /// apps, no windows.
    pub fn for_tools(config: LauncherConfig, catalog: ToolsCatalog, history: History, now: u64) -> Launcher {
        let mut l = Launcher::new(config, Mode::Tools);
        l.origin = Origin::Tools;
        l.history = history;
        l.now = now;
        l.tools = Some(ToolsState::new(catalog));
        l.refilter();
        l
    }

    /// The Tools catalog of a Mod+Space open (its typed search ranks the entries, the `/` prefix
    /// jumps in): refilters, keeping the selection by identity like `set_entries`. A catalog with
    /// no entry installs nothing (the `/` stays text).
    pub fn set_tools(&mut self, catalog: ToolsCatalog) {
        if catalog.entries.is_empty() {
            return;
        }
        let kept = self.selected_hit().and_then(|h| self.identity(h.source));
        self.tools = Some(ToolsState::new(catalog));
        self.tool_level_key = None;
        self.ranker.forget();
        self.refilter_keeping(kept);
    }

    pub fn tools(&self) -> Option<&ToolsState> {
        self.tools.as_ref()
    }

    /// The rows the `Source::Tool` hits index.
    pub fn tool_rows(&self) -> &[ToolRow] {
        match &self.tools {
            Some(t) if self.tool_flat_shown => t.flat().0,
            Some(_) => &self.tool_level,
            None => &[],
        }
    }

    /// A state the reader read: the row's switch.
    pub fn adopt_state(&mut self, entry: usize, on: Option<bool>) {
        let Some(t) = self.tools.as_mut() else { return };
        t.adopt_state(entry, on);
        self.reshape();
    }

    /// A `choices_cmd` delivery: false when stale. A failed read leaves the level empty and says so.
    pub fn adopt_choices(&mut self, entry: usize, token: u64, lines: Vec<String>, failed: bool) -> bool {
        let Some(t) = self.tools.as_mut() else { return false };
        if !t.adopt_choices(entry, token, lines, failed) {
            return false;
        }
        if failed {
            self.message = Some("could not read the choices".into());
        }
        self.ranker.forget();
        self.refilter();
        true
    }

    /// The question's instant, while one is pending (the host's one timer).
    pub fn tools_deadline(&self) -> Option<Instant> {
        self.tools.as_ref().and_then(ToolsState::next_deadline)
    }

    /// True when the question expired at `now`: its row is redrawn.
    pub fn tools_expire(&mut self, now: Instant) -> bool {
        self.tools.as_mut().is_some_and(|t| t.expire(now))
    }

    /// The history's rows for All's `"` prefix (the open's snapshot): refilters, keeping the
    /// selection by identity like `set_entries`. The old rows are wiped as they drop.
    pub fn set_clips(&mut self, rows: Vec<ClipRow>) {
        let kept = self.selected_hit().and_then(|h| self.identity(h.source));
        self.clips = rows;
        self.refilter_keeping(kept);
    }

    /// The instant of the next key or click (the Clear question's clock).
    pub fn set_now(&mut self, at: Instant) {
        self.at = Some(at);
    }

    /// Whether the Clear row asks `Clear? ⏎` (the view draws it).
    pub fn clear_question(&self) -> bool {
        self.clear_asked.is_some()
    }

    pub fn is_switcher(&self) -> bool {
        self.switcher && self.mode == Mode::Windows
    }

    pub fn is_script(&self) -> bool {
        self.mode == Mode::Script
    }

    /// Whether the box draws the modes row: not a script menu, the calculator or the switcher.
    /// The Tools menu of a `--tools` open has none; one entered from Mod+Space keeps the row it
    /// was laid out with (the layout is decided once per open).
    pub fn has_modes(&self) -> bool {
        !self.is_bare() && !self.is_switcher() && self.origin != Origin::Tools
    }

    /// A menu with no modes row and no preview: a script's, the calculator's or the history's.
    pub fn is_bare(&self) -> bool {
        matches!(self.mode, Mode::Script | Mode::Calc | Mode::Clipboard)
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
            Source::Clip(i) => Identity::Clip(self.clips.get(i)?.id),
            Source::ClipClear => return None,
            Source::Tool(i) => Identity::Tool(self.tool_rows().get(i)?.id),
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
        self.key_repeat(k, false)
    }

    /// `key`, told whether it is an auto-repeat. On a history row a held Enter (Clear), Ctrl+Enter
    /// (pin) or Delete does nothing: like the power menu, a held key repeats only to move.
    pub fn key_repeat(&mut self, k: Key, repeat: bool) -> Outcome {
        // A held Enter in the Tools menu does nothing (tools spec § 2): it neither drills, runs
        // nor asks, so a key held past the box's closing never acts in what it opened.
        if repeat && k == Key::Enter && self.mode == Mode::Tools {
            self.at = None;
            return Outcome::Nothing;
        }
        if repeat && matches!(k, Key::Enter | Key::CtrlEnter | Key::Delete) {
            let hit = self.selected_hit().map(|h| h.source);
            let guarded = match k {
                Key::Enter => matches!(hit, Some(Source::ClipClear)),
                _ => matches!(hit, Some(Source::Clip(_) | Source::ClipClear)),
            };
            if guarded {
                self.at = None;
                return Outcome::Nothing;
            }
        }
        let had_message = self.message.take().is_some();
        // Any key but Enter cancels the Clear question; its row is redrawn.
        let cancelled = k != Key::Enter && self.clear_asked.take().is_some();
        // The same for a Tools question.
        let cancelled_tool = k != Key::Enter && self.tools.as_mut().is_some_and(ToolsState::cancel);
        let o = self.apply(k);
        self.at = None;
        if (had_message || cancelled || cancelled_tool) && o == Outcome::Nothing { Outcome::Redraw } else { o }
    }

    fn apply(&mut self, k: Key) -> Outcome {
        if self.mode == Mode::Tools
            && let Some(o) = self.apply_tools(k)
        {
            return o;
        }
        let page = i32::try_from(self.list_rows).unwrap_or(i32::MAX);
        match k {
            Key::Char(c) => {
                self.query.push(c);
                let entered = self.take_prefix();
                let o = self.query_changed();
                entered.map_or(o, Outcome::EnterTools)
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
            Key::Tab if self.is_switcher() => self.step(1),
            Key::BackTab if self.is_switcher() => self.step(-1),
            Key::Tab => self.switch_mode(self.mode.next()),
            Key::BackTab => self.switch_mode(self.mode.previous()),
            // Forgets the selected history row (nothing on any other row).
            Key::Delete => self.delete(),
            Key::Left | Key::Right => Outcome::Nothing,
            Key::Escape => Outcome::Close,
            Key::Enter => self.primary(),
            Key::CtrlEnter => self.secondary(),
        }
    }

    /// In All mode (not already entered by a prefix), a query equal to a
    /// configured prefix switches to that mode and is consumed.
    /// The `/` prefix jumps into the Tools menu at its root (only with a catalog to show), and
    /// returns the read of its states.
    fn take_prefix(&mut self) -> Option<Option<Job>> {
        if self.mode != Mode::All || self.by_prefix {
            return None;
        }
        let p = &self.config.prefixes;
        let mode = if self.query == p.commands {
            Mode::Commands
        } else if self.query == p.windows {
            Mode::Windows
        } else if !p.tools.is_empty() && self.query == p.tools && self.tools.is_some() {
            Mode::Tools
        } else {
            return None;
        };
        self.mode = mode;
        self.query.clear();
        self.by_prefix = true;
        if mode != Mode::Tools {
            return None;
        }
        let t = self.tools.as_mut()?;
        t.reset();
        self.ranker.forget();
        Some(t.state_job(&self.config.tools))
    }

    /// The query set as is, with no prefix jump (a built-in that types its own query: the emoji
    /// picker's `:` must stay the emoji prefix whatever the modes' prefixes are).
    pub fn set_query(&mut self, text: &str) {
        self.query = text.into();
        self.query_changed();
    }

    /// The keys that mean something else in the Tools mode (spec § 2); the rest (editing, moving)
    /// is the common handling.
    fn apply_tools(&mut self, k: Key) -> Option<Outcome> {
        Some(match k {
            Key::Backspace if self.query.is_empty() => {
                if self.tools_up() {
                    Outcome::Redraw
                } else if self.origin == Origin::Apps {
                    self.back_to_all()
                } else {
                    Outcome::Nothing
                }
            }
            Key::Left if self.query.is_empty() => {
                if self.tools_up() {
                    Outcome::Redraw
                } else {
                    Outcome::Nothing
                }
            }
            Key::Left => Outcome::Nothing,
            Key::Right => self.tools_drill(),
            Key::Escape => {
                if self.tools_up() {
                    Outcome::Redraw
                } else {
                    Outcome::Close
                }
            }
            Key::Enter => self.tools_enter(),
            Key::Tab | Key::BackTab | Key::CtrlEnter | Key::Delete => Outcome::Nothing,
            _ => return None,
        })
    }

    /// Up one level (the query dropped, the parent's selection back): false at the root.
    fn tools_up(&mut self) -> bool {
        let Some(selected) = self.tools.as_mut().and_then(ToolsState::up) else { return false };
        self.query.clear();
        self.message = None;
        // The parent's list may have as many rows as the one left: its facts are not that list's.
        self.ranker.forget();
        self.refilter();
        self.selected = selected.min(self.results.len().saturating_sub(1));
        self.keep_visible();
        true
    }

    fn back_to_all(&mut self) -> Outcome {
        self.mode = Mode::All;
        self.by_prefix = false;
        self.query.clear();
        self.ranker.forget();
        self.refilter();
        Outcome::BackToAll
    }

    /// The selected Tools row's identity.
    fn selected_tool(&self) -> Option<RowId> {
        match self.selected_hit()?.source {
            Source::Tool(i) => self.tool_rows().get(i).map(|r| r.id),
            _ => None,
        }
    }

    /// → on a category or an entry with choices opens it like Enter; elsewhere nothing.
    fn tools_drill(&mut self) -> Outcome {
        let drills = match (self.selected_tool(), &self.tools) {
            (Some(RowId::Category(_)), _) => true,
            (Some(RowId::Entry(e)), Some(t)) => {
                t.catalog.entries.get(e).is_some_and(|e| !matches!(e.choices, crate::launcher::tools::catalog::Choices::None))
            }
            _ => false,
        };
        if drills { self.tools_enter() } else { Outcome::Nothing }
    }

    fn tools_enter(&mut self) -> Outcome {
        match self.selected_tool() {
            Some(row) => self.tools_activate(row),
            None => Outcome::Nothing,
        }
    }

    /// Enter on `row` of the open level (or of the flat search): opens, asks, or hands the action out.
    fn tools_activate(&mut self, row: RowId) -> Outcome {
        let now = self.at.unwrap_or_else(Instant::now);
        // The flat search's index means nothing in the level it drills into: restore the top.
        let query_empty = self.query.is_empty();
        let selected = self.selected;
        let Some(step) = self.tools.as_mut().map(|t| {
            // A typed query's index means nothing in the level: `up` restores the row's real place.
            let selected = if query_empty { selected } else { t.here(row) };
            t.enter(&self.config.tools, row, selected, now)
        }) else {
            return Outcome::Nothing;
        };
        match step {
            Step::Drill(job) => {
                self.level_changed();
                job.map_or(Outcome::Redraw, Outcome::ReadChoices)
            }
            Step::Ask => Outcome::Asked,
            Step::Wait => Outcome::Nothing,
            Step::Run(action) => Outcome::Activate(Activation::Tool(action)),
        }
    }

    /// The open level changed (a drill, a jump): a fresh query and rows, the first selected.
    fn level_changed(&mut self) {
        self.query.clear();
        self.message = None;
        self.ranker.forget();
        self.refilter();
    }

    /// A Mod+Space hit on a tool (tools spec § 1): runs it as the Tools box would; one that
    /// drills or must ask switches the box to the Tools mode, at its level.
    fn tool_from_all(&mut self, i: usize) -> Outcome {
        use crate::launcher::tools::catalog::Choices;
        let Some(row) = self.tool_rows().get(i).map(|r| r.id) else { return Outcome::Nothing };
        let Some(t) = self.tools.as_mut() else { return Outcome::Nothing };
        let (entry, choice) = match row {
            RowId::Entry(e) => (e, None),
            RowId::Choice(e, k) => (e, Some(k)),
            RowId::Category(_) => return Outcome::Nothing,
        };
        let Some(e) = t.catalog.entries.get(entry) else { return Outcome::Nothing };
        let (confirm, with_choices) = (e.confirm, !matches!(e.choices, Choices::None));
        if choice.is_none() && with_choices {
            let step = t.goto_choices(&self.config.tools, entry);
            self.enter_tools_mode();
            return match step {
                Step::Drill(Some(job)) => Outcome::ReadChoices(job),
                _ => Outcome::Redraw,
            };
        }
        if !confirm {
            // Nothing to ask: the action itself, as `ToolsState::enter` would hand it out.
            let now = self.at.unwrap_or_else(Instant::now);
            return match t.enter(&self.config.tools, row, 0, now) {
                Step::Run(action) => Outcome::Activate(Activation::Tool(action)),
                _ => Outcome::Nothing,
            };
        }
        // A confirm entry or choice: the box goes to its level with its question asked, so the
        // question, its expiry and its timer are the Tools mode's only (spec decision 20).
        match choice {
            Some(_) => {
                t.goto_choices(&self.config.tools, entry);
            }
            None => t.goto_entry(entry),
        }
        self.enter_tools_mode();
        let at =
            self.results.iter().position(|h| matches!(h.source, Source::Tool(i) if self.tool_rows().get(i).is_some_and(|r| r.id == row)));
        if let Some(at) = at {
            self.selected = at;
            self.keep_visible();
        }
        self.tools_activate(row)
    }

    /// The box becomes the Tools mode at the level the state is already on.
    fn enter_tools_mode(&mut self) {
        self.mode = Mode::Tools;
        self.by_prefix = false;
        self.level_changed();
    }

    fn switch_mode(&mut self, m: Mode) -> Outcome {
        if self.is_bare() || self.switcher || m == self.mode {
            return Outcome::Nothing;
        }
        if self.mode == Mode::Tools {
            // The Tools list's prepared facts are not another mode's, whatever its row count.
            self.ranker.forget();
            if let Some(t) = self.tools.as_mut() {
                t.cancel_read();
            }
        }
        self.mode = m;
        self.by_prefix = false;
        self.query_changed()
    }

    /// The query's prefix (`?`, `:`, `$`) and its text: only in All (`--password` is a dmenu, so
    /// Script: the secret is never parsed). A configured mode prefix is taken before, by `take_prefix`.
    pub fn prefixed(&self) -> Option<(Prefix, &str)> {
        split_query(self.mode, &self.query, &self.config, self.clipboard)
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
        // The history alone: the query filters its rows.
        if self.mode == Mode::Clipboard {
            self.results = self.ranker.rank_clips(&self.query, &self.clips);
            self.calc = None;
            return;
        }
        if self.mode == Mode::Tools {
            self.rank_tools();
            return;
        }
        // A prefix lists only its own rows: no catalog ranking, no calculator.
        // Field by field, not `prefixed()`: the ranker is borrowed mutably below.
        if let Some((p, text)) = split_query(self.mode, &self.query, &self.config, self.clipboard) {
            self.results = match p {
                Prefix::Clipboard => self.ranker.rank_clips(text, &self.clips),
                Prefix::Search if !text.is_empty() => vec![Hit { source: Source::Search, score: u32::MAX, frecency: 0 }],
                Prefix::Emoji => self.ranker.rank_emoji(text, usize::from(self.config.rows)),
                Prefix::Shell if !text.is_empty() => vec![Hit { source: Source::Shell, score: u32::MAX, frecency: 0 }],
                _ => Vec::new(),
            };
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
        // Mod+Space's typed search ranks the flat rows (tools spec § 1).
        self.tool_flat_shown = true;
        let tools = match &self.tools {
            Some(t) => t.flat().1,
            None => &[],
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
            tools,
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

    /// The Tools mode: the open level's rows (or, at the root, the flat search of a typed query),
    /// in the level's order, ranked by what is typed.
    fn rank_tools(&mut self) {
        self.calc = None;
        let Some(t) = &self.tools else {
            self.results.clear();
            return;
        };
        let flat = t.at_root() && !self.query.trim().is_empty();
        if flat != self.tool_flat_shown {
            // The prepared facts are per list: another list, another facts.
            self.ranker.forget();
        }
        self.tool_flat_shown = flat;
        let key = (t.level(), t.rev());
        if !flat && self.tool_level_key != Some(key) {
            self.tool_level_key = Some(key);
            #[cfg(test)]
            {
                self.tool_level_builds += 1;
            }
            let rows = t.rows();
            self.tool_level_hits = rows.iter().map(|r| t.hit_row(r)).collect();
            self.tool_level = rows;
        }
        let tools = if flat { t.flat().1 } else { &self.tool_level_hits[..] };
        let q = Query {
            text: &self.query,
            mode: Mode::Tools,
            apps: &[],
            commands: &[],
            windows: &[],
            lines: &[],
            entries: &[],
            entry_sources: &[],
            tools,
            history: &self.history,
            now: self.now,
        };
        self.results = self.ranker.rank(&q);
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

    /// The switcher's Tab: moves the selection by `by`, wrapping around the results (unlike
    /// `move_by`, which clamps).
    pub fn step(&mut self, by: i32) -> Outcome {
        let n = i64::try_from(self.results.len()).unwrap_or(i64::MAX);
        if n == 0 {
            return Outcome::Nothing;
        }
        let to = (i64::try_from(self.selected).unwrap_or(0) + i64::from(by)).rem_euclid(n);
        let to = usize::try_from(to).unwrap_or(0);
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
    fn primary(&mut self) -> Outcome {
        let hit = self.selected_hit();
        match self.mode {
            Mode::Tools => self.tools_enter(),
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
                Some(Source::Tool(i)) => self.tool_from_all(i),
                Some(Source::Calc) => self.copy(false),
                Some(Source::Search) => self.search(),
                Some(Source::Emoji(offset)) => self.emoji(offset, false),
                Some(Source::Shell) => self.dollar(false),
                Some(Source::Clip(i)) => self.clips.get(i).map_or(Outcome::Nothing, |c| Outcome::Activate(Activation::Clip { id: c.id })),
                Some(Source::ClipClear) => self.clear(),
                None => Outcome::Nothing,
            },
        }
    }

    /// The `$` row: the text after the prefix, through `sh -c`, detached or held in the terminal.
    fn dollar(&self, terminal: bool) -> Outcome {
        match self.prefixed() {
            Some((Prefix::Shell, text)) if !text.is_empty() => Outcome::Activate(Activation::Shell { text: text.to_string(), terminal }),
            _ => Outcome::Nothing,
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

    /// An emoji row's copy: the emoji, or with `name` its name. No `copy` command copies nothing.
    fn emoji(&self, offset: usize, name: bool) -> Outcome {
        match emoji::at(offset) {
            Some((emoji, label)) if !self.config.copy.is_empty() => {
                Outcome::Activate(Activation::Copy { text: (if name { label } else { emoji }).to_string() })
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

    /// The Clear row's Enter: the first asks `Clear? ⏎`, a second from 0.4 s on (never too late)
    /// clears (the rows the history keeps, pins, stay).
    fn clear(&mut self) -> Outcome {
        let now = self.at.unwrap_or_else(Instant::now);
        match self.clear_asked {
            None => {
                self.clear_asked = Some(now);
                Outcome::Redraw
            }
            Some(asked) if now.saturating_duration_since(asked) >= session::CONFIRM_AFTER => {
                self.clear_asked = None;
                self.clips.retain(|c| c.pinned);
                self.refilter();
                Outcome::Edit(ClipEdit::Clear)
            }
            Some(_) => Outcome::Nothing,
        }
    }

    /// Ctrl+Enter or a right click on row `i`: its pin flips here and in the history. The box
    /// stays open; an eleventh pin is refused.
    fn pin(&mut self, i: usize) -> Outcome {
        let pinned = self.clips.iter().filter(|c| c.pinned).count();
        let Some(row) = self.clips.get_mut(i) else { return Outcome::Nothing };
        if !row.pinned && pinned >= MAX_PINS {
            self.message = Some(format!("at most {MAX_PINS} pins"));
            return Outcome::Redraw;
        }
        row.pinned = !row.pinned;
        let id = row.id;
        // The history's order: a new pin heads the pinned group (most recently pinned first), an
        // unpinned row goes right behind it. The selection follows the row.
        let row = self.clips.remove(i);
        let to = if row.pinned { 0 } else { self.clips.iter().filter(|c| c.pinned).count() };
        self.clips.insert(to, row);
        self.rank();
        self.selected = self.results.iter().position(|h| h.source == Source::Clip(to)).unwrap_or(0);
        self.keep_visible();
        Outcome::Edit(ClipEdit::Pin(id))
    }

    /// Delete on a history row: the row goes here and in the history; the box stays open.
    fn delete(&mut self) -> Outcome {
        let Some(Source::Clip(i)) = self.selected_hit().map(|h| h.source) else { return Outcome::Nothing };
        if i >= self.clips.len() {
            return Outcome::Nothing;
        }
        let id = self.clips.remove(i).id;
        self.reshape();
        Outcome::Edit(ClipEdit::Delete(id))
    }

    /// Re-ranks after the rows changed under the selection, which stays at its place (clamped).
    fn reshape(&mut self) {
        self.rank();
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
        self.keep_visible();
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
    fn secondary(&mut self) -> Outcome {
        let hit = self.selected_hit().map(|h| h.source);
        match (self.mode, hit) {
            // Ctrl+Enter and a right click do nothing in the Tools mode.
            (Mode::Tools, _) => Outcome::Nothing,
            (Mode::Commands, Some(Source::Command(index))) if !self.query.contains(char::is_whitespace) => {
                Outcome::Activate(Activation::Command { index, terminal: false })
            }
            (Mode::Commands, _) => self.shell(false),
            (_, Some(Source::Shell)) => self.dollar(true),
            (_, Some(Source::Calc)) => self.copy(true),
            (_, Some(Source::Emoji(offset))) => self.emoji(offset, true),
            (_, Some(Source::Clip(i))) => self.pin(i),
            // Ctrl+Enter or a right click never confirms the question.
            (_, Some(Source::ClipClear)) => Outcome::Nothing,
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
        let on_clear = matches!(self.results[index].source, Source::ClipClear);
        // Another row's click cancels the Clear question; the same row's is its answer (`ask`).
        let mut cancelled = !on_clear && self.clear_asked.take().is_some();
        // A right click on another row cancels a Tools question (a left click re-asks or runs:
        // `ToolsState::enter` replaces it); its own row's right click leaves it, like Ctrl+Enter
        // never confirming.
        if secondary && self.mode == Mode::Tools {
            let row = self.results[index].source;
            let own = matches!(row, Source::Tool(i) if self.tool_rows().get(i).is_some_and(|r| self.tools.as_ref().is_some_and(|t| t.asking(r.id))));
            cancelled |= !own && self.tools.as_mut().is_some_and(ToolsState::cancel);
        }
        self.moved = true;
        self.selected = index;
        self.keep_visible();
        let o = if secondary { self.secondary() } else { self.primary() };
        self.at = None;
        if (had_message || cancelled) && o == Outcome::Nothing { Outcome::Redraw } else { o }
    }

    pub fn click_mode(&mut self, m: Mode) -> Outcome {
        if m == Mode::Script {
            return Outcome::Nothing;
        }
        self.switch_mode(m)
    }

    /// Wheel: moves the selection by `rows`, clamped.
    pub fn scroll(&mut self, rows: i32) -> Outcome {
        // Any other input cancels the Clear question.
        let cancelled = self.clear_asked.take().is_some() | self.tools.as_mut().is_some_and(ToolsState::cancel);
        let o = self.move_by(rows);
        if cancelled && o == Outcome::Nothing { Outcome::Redraw } else { o }
    }

    pub fn selected_hit(&self) -> Option<Hit> {
        self.results.get(self.selected).copied()
    }

    pub fn show_message(&mut self, m: String) {
        self.message = Some(m);
    }
}

/// `Launcher::prefixed`'s rule on loose fields.
fn split_query<'a>(mode: Mode, query: &'a str, config: &LauncherConfig, clipboard: bool) -> Option<(Prefix, &'a str)> {
    if mode != Mode::All {
        return None;
    }
    prefix::split(query, config.search_enabled(), clipboard)
}

/// The history rows' text is overwritten before it is freed (clipboard spec decision 18).
fn wipe_clips(clips: &mut [ClipRow]) {
    for c in clips {
        wipe(&mut c.preview);
        wipe(&mut c.first);
    }
}

/// `--password`: the typed text is overwritten before it is freed (where practical: the copies
/// SCTK and xkbcommon made of each key are out of reach).
impl Drop for Launcher {
    fn drop(&mut self) {
        if self.password {
            wipe(&mut self.query);
        }
        wipe_clips(&mut self.clips);
    }
}

#[cfg(test)]
mod tests;
