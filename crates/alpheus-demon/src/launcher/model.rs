//! The launcher's state and every key rule, pure (spec § 2): the query,
//! the mode (and the prefixes that jump to one), the selection and the
//! visible window of results, what Enter and Ctrl+Enter activate.
//! Wayland (task 12) feeds it keys and clicks and applies the `Outcome`;
//! nothing here touches the session.

use crate::compositor::Toplevel;
use crate::keys::Key;
use crate::launcher::config::LauncherConfig;
use crate::launcher::history::History;
use crate::launcher::matching::{Hit, Mode, Query, Ranker, Source};
use crate::launcher::scan::Catalog;
use crate::list;
use std::path::PathBuf;

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
    /// Script mode: the chosen line, or the typed text when nothing matched.
    Choose(String),
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
    /// Changed only through `set_windows`: a direct assignment skips `ranker.forget_windows()`
    /// and the identity-preserving reselection `set_windows` does (`Launcher::identity`).
    pub windows: Vec<Toplevel>,
    /// Script mode only: set once by `for_script`, never mutated afterward (no `set_lines` —
    /// the dmenu line list is fixed for the life of one open).
    pub lines: Vec<String>,
    /// $PATH directories, in order, for the preview's "full path" of a command (task 12 sets
    /// them from the same `ScanEnv` the scan used); empty until then, or if never set — the
    /// preview falls back to naming $PATH alone.
    pub path_dirs: Vec<PathBuf>,
    pub history: History,
    pub now: u64,
    pub results: Vec<Hit>,
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
            windows: Vec::new(),
            lines: Vec::new(),
            path_dirs: Vec::new(),
            history: History::empty(0),
            now: 0,
            results: Vec::new(),
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
        let mut l = Launcher::new(config, Mode::Script);
        l.prompt = prompt;
        l.lines = lines;
        l.refilter();
        l
    }

    pub fn is_script(&self) -> bool {
        self.mode == Mode::Script
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
            Key::Left | Key::Right => Outcome::Nothing,
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
        if self.is_script() || m == self.mode {
            return Outcome::Nothing;
        }
        self.mode = m;
        self.by_prefix = false;
        self.query_changed()
    }

    fn query_changed(&mut self) -> Outcome {
        self.message = None;
        self.refilter();
        Outcome::Redraw
    }

    fn rank(&mut self) {
        let (apps, commands) = match &self.catalog {
            Some(c) => (&c.apps[..], &c.commands[..]),
            None => (&[][..], &[][..]),
        };
        let q = Query {
            text: &self.query,
            mode: self.mode,
            apps,
            commands,
            windows: &self.windows,
            lines: &self.lines,
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
                Some(Hit { source: Source::Line(i), .. }) => Outcome::Activate(Activation::Choose(self.lines[i].clone())),
                _ if !self.query.is_empty() => Outcome::Activate(Activation::Choose(self.query.clone())),
                _ => Outcome::Nothing,
            },
            Mode::Commands if hit.is_none() || self.query.contains(char::is_whitespace) => self.shell(true),
            _ => match hit.map(|h| h.source) {
                Some(Source::App(index)) => Outcome::Activate(Activation::App { index, action: None }),
                Some(Source::Window(i)) => Outcome::Activate(Activation::Window(i)),
                Some(Source::Command(index)) => Outcome::Activate(Activation::Command { index, terminal: true }),
                Some(Source::Line(i)) => Outcome::Activate(Activation::Choose(self.lines[i].clone())),
                None => Outcome::Nothing,
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::desktop::{DesktopAction, DesktopEntry};
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

    /// The bug Mae hit (2026-10-05): the catalog arrives after the first window has been
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

    /// A click is navigation too, like the keys `move_by` covers (Mae, 2026-10-05's rule):
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

    /// Typing never counts as moved, even when it reshuffles the results (Mae, 2026-10-05):
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
    /// boundary bump as a deliberate choice and stop landing on top (Mae, 2026-10-05).
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

    /// The preview panel always shows the selected result now (view.rs, Mae 2026-10-05): the
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
        // (Mae, 2026-10-05, "until the user moves, the selection stays on top").
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
        assert_eq!(s.key(Key::Enter), activate(Activation::Choose("10.0.0.1".into())), "free text (tools-menu's prompts)");
        let mut s = Launcher::for_script(LauncherConfig::default(), None, vec!["alpha".into(), "beta".into()]);
        assert_eq!(sources(&s), [Source::Line(0), Source::Line(1)]);
        typed(&mut s, "bet");
        assert_eq!(s.key(Key::Enter), activate(Activation::Choose("beta".into())), "the matching line, as shown");
        typed(&mut s, "zzz");
        assert_eq!(s.key(Key::Enter), activate(Activation::Choose("betzzz".into())), "nothing matches: the typed text");
        assert_eq!(s.key(Key::CtrlEnter), activate(Activation::Choose("betzzz".into())), "Ctrl+Enter as Enter");
        assert_eq!(s.key(Key::Escape), Outcome::Close);
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
    /// branch, blank text: Review the brief's Enter rule) — the one way to reach `Nothing`
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
}
