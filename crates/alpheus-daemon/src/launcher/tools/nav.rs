//! The Tools menu's levels, rows and confirmation question: which rows the
//! current level lists, what Enter does on each, how the question is asked and expires. Pure: no
//! thread, no command, no log; the model (`Launcher`) owns the query and the selection and ranks
//! the rows with its own `Ranker`, the host runs the jobs and arms the deadline.

use super::catalog::{Catalog, Choices, EntryKind, Item};
use super::config::ToolsConfig;
use crate::launcher::matching::ToolHitRow;
use crate::launcher::sources::PATH_JOIN;
use crate::session::{self, Activation as Ask};
use std::time::Instant;

/// One `state` to read: the catalog entry, its 1-based position in launcher.toml (what a log names
/// it by) and its argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateJob {
    pub entry: usize,
    pub position: usize,
    pub argv: Vec<String>,
}

/// How many states a menu's reads found so far, carried across a preempted job's slices so the
/// last one logs the whole menu's totals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub on: usize,
    pub off: usize,
    pub unknown: usize,
}

/// What the reader thread is asked: the states of every entry that has one, or a
/// `choices_cmd`. A choices job carries the token of the open it answers: a delivery with another
/// token is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// The entries to read, and the counts of the slices already read (none for a first slice).
    States(Vec<StateJob>, Counts),
    Choices {
        entry: usize,
        position: usize,
        argv: Vec<String>,
        token: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Root,
    Category(usize),
    /// The choices of the catalog entry.
    Choices {
        entry: usize,
    },
    /// The text the entry asks for: one row, the typed query is its answer.
    Prompt {
        entry: usize,
    },
}

/// A row's identity, stable while the rows around it change: the question and the selection
/// follow it, never an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowId {
    Category(usize),
    Entry(usize),
    /// A choice of the entry: an index into its fixed choices or into the lines read.
    Choice(usize, usize),
    /// The prompt row of the entry.
    Prompt(usize),
}

/// What a row shows at its right edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trail {
    None,
    /// ` ›`: a category, or an entry with choices.
    Chevron,
    /// The quick panel's switch, for a state that is known.
    Switch(bool),
    /// A flat search's row: its category, dimmed after the entry (empty for a root entry).
    Category(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    pub id: RowId,
    pub icon: Option<char>,
    pub text: String,
    pub trail: Trail,
}

/// What a row runs. `Builtin` indexes `catalog::BUILTINS`; `User` names the catalog entry (its
/// `EntryKind::User` indexes the `[[tools.entry]]` list), the chosen line and the typed text, whole.
/// The text is never written to a log or to the history.
#[derive(Clone, PartialEq, Eq)]
pub enum ToolAction {
    Builtin(usize),
    User { entry: usize, choice: Option<String>, text: Option<String> },
}

// By hand: the typed text never reaches a `{:?}`, a panic message or an assertion's output.
impl std::fmt::Debug for ToolAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolAction::Builtin(i) => write!(f, "Builtin({i})"),
            ToolAction::User { entry, choice, text } => {
                write!(f, "User {{ entry: {entry}, choice: {choice:?}, text: {} }}", if text.is_some() { "Some(..)" } else { "None" })
            }
        }
    }
}

/// What `ToolsState::enter` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A level was opened; a `choices_cmd` entry carries the read to start.
    Drill(Option<Job>),
    /// The question shows (or shows again).
    Ask,
    /// Too soon after the question: nothing.
    Wait,
    Run(ToolAction),
}

pub struct ToolsState {
    pub catalog: Catalog,
    level: Level,
    /// The levels above the current one, each with the selection to restore.
    stack: Vec<(Level, usize)>,
    /// Per catalog entry: its state, when known.
    states: Vec<Option<bool>>,
    /// The lines a `choices_cmd` read delivered, for the entry whose level is open.
    dynamic: Option<(usize, Vec<String>)>,
    /// A `choices_cmd` is being read for the open level.
    pub reading: bool,
    /// The choice the open prompt level follows, when its entry has choices; set by each
    /// `drill_prompt`, read only while a prompt is open.
    asked: Option<String>,
    /// The question pending and the instant it expires.
    question: Option<(RowId, Instant)>,
    /// The token of the choices read the open level waits for.
    token: u64,
    /// Bumped by everything that changes the rows a level lists (level, lines, states): the
    /// model rebuilds its copy of them only when it moved.
    rev: u64,
    /// The flat search's rows and their candidates, built once: every entry, then every fixed
    /// choice. Never a dynamic choice.
    flat: Vec<ToolRow>,
    flat_hits: Vec<ToolHitRow>,
}

impl ToolsState {
    pub fn new(catalog: Catalog) -> ToolsState {
        let states = vec![None; catalog.entries.len()];
        let mut state = ToolsState {
            catalog,
            level: Level::Root,
            stack: Vec::new(),
            states,
            dynamic: None,
            reading: false,
            asked: None,
            question: None,
            token: 0,
            rev: 0,
            flat: Vec::new(),
            flat_hits: Vec::new(),
        };
        state.build_flat();
        state
    }

    fn build_flat(&mut self) {
        let mut flat = Vec::new();
        let mut hits = Vec::new();
        for f in self.catalog.flat() {
            let Some(e) = self.catalog.entries.get(f.entry) else { continue };
            let (id, text, key) = match (f.choice, &e.choices) {
                (Some(k), Choices::Fixed(list)) => {
                    let Some(choice) = list.get(k) else { continue };
                    (RowId::Choice(f.entry, k), format!("{} · {choice}", e.name), self.catalog.choice_key(f.entry, choice))
                }
                _ => (RowId::Entry(f.entry), e.name.clone(), e.key.clone()),
            };
            let category = e.category.and_then(|c| self.catalog.categories.get(c)).map(|c| c.name.as_str());
            let path_text = category.map(|c| format!("{c} {text}")).unwrap_or_default();
            flat.push(ToolRow { id, icon: e.icon, text: text.clone(), trail: Trail::Category(category.unwrap_or_default().to_string()) });
            hits.push(ToolHitRow { name: text, path_text, key });
        }
        self.flat = flat;
        self.flat_hits = hits;
    }

    /// See `rev`.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn at_root(&self) -> bool {
        self.level == Level::Root
    }

    /// The flat search's rows (every entry, then every fixed choice) and their candidates.
    pub fn flat(&self) -> (&[ToolRow], &[ToolHitRow]) {
        (&self.flat, &self.flat_hits)
    }

    fn entry_row(&self, e: usize) -> Option<ToolRow> {
        let entry = self.catalog.entries.get(e)?;
        let trail = match (&entry.choices, self.states.get(e).copied().flatten()) {
            (Choices::None, Some(on)) => Trail::Switch(on),
            (Choices::None, None) => Trail::None,
            _ => Trail::Chevron,
        };
        Some(ToolRow { id: RowId::Entry(e), icon: entry.icon, text: entry.name.clone(), trail })
    }

    /// The choices of `entry` as the open level lists them: the fixed ones, or the lines read.
    pub fn choice_lines(&self, entry: usize) -> &[String] {
        match self.catalog.entries.get(entry).map(|e| &e.choices) {
            Some(Choices::Fixed(list)) => list,
            Some(Choices::Command) => match &self.dynamic {
                Some((e, lines)) if *e == entry => lines,
                _ => &[],
            },
            _ => &[],
        }
    }

    /// The rows of the current level, in its order.
    pub fn rows(&self) -> Vec<ToolRow> {
        match self.level {
            Level::Root => self
                .catalog
                .root_rows()
                .into_iter()
                .filter_map(|item| match item {
                    Item::Entry(e) => self.entry_row(e),
                    Item::Category(c) => self.category_row(c),
                })
                .collect(),
            Level::Category(c) => self.catalog.category_rows(c).into_iter().filter_map(|e| self.entry_row(e)).collect(),
            Level::Choices { entry } => self
                .choice_lines(entry)
                .iter()
                .enumerate()
                .map(|(k, line)| ToolRow { id: RowId::Choice(entry, k), icon: None, text: line.clone(), trail: Trail::None })
                .collect(),
            Level::Prompt { entry } => self
                .catalog
                .entries
                .get(entry)
                .and_then(|e| {
                    e.ask.as_ref().map(|label| ToolRow { id: RowId::Prompt(entry), icon: e.icon, text: label.clone(), trail: Trail::None })
                })
                .into_iter()
                .collect(),
        }
    }

    fn category_row(&self, c: usize) -> Option<ToolRow> {
        let category = self.catalog.categories.get(c)?;
        Some(ToolRow { id: RowId::Category(c), icon: Some(category.icon), text: category.name.clone(), trail: Trail::Chevron })
    }

    /// `row` as a search candidate: its name, its category's words and its history key.
    pub fn hit_row(&self, row: &ToolRow) -> ToolHitRow {
        let key = match row.id {
            RowId::Entry(e) => self.catalog.entries.get(e).map(|e| e.key.clone()),
            RowId::Choice(e, k) => match self.catalog.entries.get(e).map(|e| &e.choices) {
                // A dynamic choice is never recorded: nothing to rank it by.
                Some(Choices::Fixed(list)) => list.get(k).map(|c| self.catalog.choice_key(e, c)),
                _ => None,
            },
            RowId::Category(_) | RowId::Prompt(_) => None,
        };
        ToolHitRow { name: row.text.clone(), path_text: String::new(), key: key.unwrap_or_default() }
    }

    /// The question a pending `row` shows: `<name>? ⏎` (the icon is the row's own), a choice of a
    /// level `<entry> · <choice>? ⏎` (a flat row's text already names its entry).
    pub fn question_text(&self, row: &ToolRow) -> String {
        match (row.id, &row.trail) {
            (RowId::Choice(e, _), Trail::None) => {
                format!("{} · {}? ⏎", self.catalog.entries.get(e).map_or("", |e| e.name.as_str()), row.text)
            }
            _ => format!("{}? ⏎", row.text),
        }
    }

    /// `Tools`, `Tools › <category>`, `Tools › <category> › <entry>`.
    pub fn breadcrumb(&self) -> String {
        let mut parts = vec!["Tools".to_string()];
        let entry = match self.level {
            Level::Root => None,
            Level::Category(c) => {
                parts.extend(self.catalog.categories.get(c).map(|c| c.name.clone()));
                None
            }
            Level::Choices { entry } | Level::Prompt { entry } => Some(entry),
        };
        if let Some(e) = entry.and_then(|e| self.catalog.entries.get(e)) {
            parts.extend(e.category.and_then(|c| self.catalog.categories.get(c)).map(|c| c.name.clone()));
            parts.push(e.name.clone());
            // A prompt after a choice names it: `Tools › Net › Scan › Quick`.
            parts.extend(self.asked.clone().filter(|_| matches!(self.level, Level::Prompt { .. })));
        }
        parts.join(PATH_JOIN)
    }

    /// The reads of every entry's `state`, in file order, or none.
    pub fn state_job(&self, config: &ToolsConfig) -> Option<Job> {
        let jobs: Vec<StateJob> = self
            .catalog
            .entries
            .iter()
            .enumerate()
            .filter_map(|(entry, e)| match e.kind {
                EntryKind::User(i) => config.entries.get(i).filter(|c| !c.state.is_empty()).map(|c| StateJob {
                    entry,
                    position: c.position,
                    argv: c.state.clone(),
                }),
                EntryKind::Builtin(_) => None,
            })
            .collect();
        (!jobs.is_empty()).then_some(Job::States(jobs, Counts::default()))
    }

    /// The state of `entry`, read: a switch on its row (None: unknown, no switch).
    pub fn adopt_state(&mut self, entry: usize, on: Option<bool>) {
        if let Some(slot) = self.states.get_mut(entry) {
            *slot = on;
            self.rev += 1;
        }
    }

    pub fn state_of(&self, entry: usize) -> Option<bool> {
        self.states.get(entry).copied().flatten()
    }

    /// Back to the root with nothing open or pending (the `/` prefix jumps in at the root).
    pub fn reset(&mut self) {
        self.stack.clear();
        self.level = Level::Root;
        self.question = None;
        self.dynamic = None;
        self.reading = false;
        self.token += 1;
        self.rev += 1;
    }

    /// Drops the choices read the open level waits for: its late delivery is stale.
    pub fn cancel_read(&mut self) {
        self.reading = false;
        self.token += 1;
    }

    /// Opens the level of `row` (`selected`: the row's index, restored by `up`).
    fn open(&mut self, level: Level, selected: usize) {
        self.stack.push((self.level, selected));
        self.level = level;
        self.question = None;
        self.rev += 1;
    }

    /// Goes up one level: the selection to restore, or None at the root. A choices level that is
    /// left drops its lines and its token (a late delivery is stale).
    pub fn up(&mut self) -> Option<usize> {
        let (level, selected) = self.stack.pop()?;
        if matches!(self.level, Level::Choices { .. }) {
            self.dynamic = None;
            self.reading = false;
            self.token += 1;
        }
        self.level = level;
        self.question = None;
        self.rev += 1;
        Some(selected)
    }

    /// Opens the choices of `entry`, a `choices_cmd` one with the read to start.
    fn drill_choices(&mut self, config: &ToolsConfig, entry: usize, selected: usize) -> Step {
        self.open(Level::Choices { entry }, selected);
        self.dynamic = None;
        self.reading = false;
        self.token += 1;
        self.rev += 1;
        let Some(e) = self.catalog.entries.get(entry) else { return Step::Drill(None) };
        let job = match (&e.choices, e.kind) {
            (Choices::Command, EntryKind::User(i)) => config.entries.get(i).map(|c| {
                self.reading = true;
                Job::Choices { entry, position: c.position, argv: c.choices_cmd.clone(), token: self.token }
            }),
            _ => None,
        };
        Step::Drill(job)
    }

    /// Enter on `row` at `now` (`selected`: its index, for `up`). A confirm entry or choice asks
    /// first (`session::ask`: the second activation counts from 0.4 s to 3 s).
    pub fn enter(&mut self, config: &ToolsConfig, row: RowId, selected: usize, now: Instant) -> Step {
        match row {
            RowId::Category(c) => {
                self.open(Level::Category(c), selected);
                Step::Drill(None)
            }
            RowId::Entry(e) => {
                let Some(entry) = self.catalog.entries.get(e) else { return Step::Drill(None) };
                if !matches!(entry.choices, Choices::None) {
                    let selected = self.through_category(e, selected);
                    return self.drill_choices(config, e, selected);
                }
                if entry.ask.is_some() {
                    let selected = self.through_category(e, selected);
                    return self.drill_prompt(e, None, selected);
                }
                let action = match entry.kind {
                    EntryKind::Builtin(b) => ToolAction::Builtin(b),
                    EntryKind::User(_) => ToolAction::User { entry: e, choice: None, text: None },
                };
                self.answer(row, entry.confirm, action, now)
            }
            RowId::Choice(e, k) => {
                let Some(entry) = self.catalog.entries.get(e) else { return Step::Drill(None) };
                let Some(choice) = self.choice_lines(e).get(k).cloned() else { return Step::Drill(None) };
                if entry.ask.is_some() {
                    // From the root's flat search the prompt goes down through the entry's
                    // choices, so Esc lands on them.
                    let selected = if matches!(self.level, Level::Root) {
                        let at = self.through_category(e, selected);
                        self.drill_choices(config, e, at);
                        k
                    } else {
                        selected
                    };
                    return self.drill_prompt(e, Some(choice), selected);
                }
                self.answer(row, entry.confirm, ToolAction::User { entry: e, choice: Some(choice), text: None }, now)
            }
            // Its answer is the typed text: `submit`.
            RowId::Prompt(_) => Step::Wait,
        }
    }

    /// From the root's flat search an entry of a category goes down through that category, as
    /// the breadcrumb shows: ← and Esc land on it. Returns the selection to restore in the next
    /// level (`selected` when already below the root).
    fn through_category(&mut self, e: usize, selected: usize) -> usize {
        if let (Level::Root, Some(c)) = (self.level, self.catalog.entries.get(e).and_then(|e| e.category)) {
            let from = self.position(Level::Root, RowId::Category(c));
            self.open(Level::Category(c), from);
            return self.position(Level::Category(c), RowId::Entry(e));
        }
        selected
    }

    /// Opens the prompt of `entry`, after `choice` when it has choices.
    fn drill_prompt(&mut self, entry: usize, choice: Option<String>, selected: usize) -> Step {
        self.open(Level::Prompt { entry }, selected);
        self.asked = choice;
        Step::Drill(None)
    }

    /// Whether the open level is a prompt: the model's query is then the answer, not a filter.
    pub fn prompting(&self) -> bool {
        matches!(self.level, Level::Prompt { .. })
    }

    /// Enter in the prompt with `text`: the action, with the text whole. A blank text runs
    /// nothing. None when no prompt is open.
    pub fn submit(&mut self, text: String) -> Option<Step> {
        let Level::Prompt { entry } = self.level else { return None };
        if text.trim().is_empty() {
            return Some(Step::Wait);
        }
        Some(Step::Run(ToolAction::User { entry, choice: self.asked.clone(), text: Some(text) }))
    }

    fn answer(&mut self, row: RowId, confirm: bool, action: ToolAction, now: Instant) -> Step {
        if !confirm {
            self.question = None;
            return Step::Run(action);
        }
        match session::ask(&mut self.question, row, now) {
            Ask::Run => Step::Run(action),
            Ask::Ask => Step::Ask,
            Ask::Wait => Step::Wait,
        }
    }

    /// Whether the question pending is `row`'s.
    pub fn asking(&self, row: RowId) -> bool {
        self.question.as_ref().is_some_and(|(r, _)| *r == row)
    }

    /// Cancels the question (any other key, another row): true when one was pending.
    pub fn cancel(&mut self) -> bool {
        self.question.take().is_some()
    }

    /// The question's instant, when one is pending: the host arms its one timer for it.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.question.as_ref().map(|(_, until)| *until)
    }

    /// True when the question expired at `now` (its row is redrawn).
    pub fn expire(&mut self, now: Instant) -> bool {
        session::expire(&mut self.question, now)
    }

    /// A `choices_cmd` delivery for the open level: false when stale (another token, a level
    /// left). `failed` leaves the level empty (the caller says so).
    pub fn adopt_choices(&mut self, entry: usize, token: u64, lines: Vec<String>, failed: bool) -> bool {
        let waiting = matches!(self.level, Level::Choices { entry: e } if e == entry) && self.reading && token == self.token;
        if !waiting {
            return false;
        }
        self.reading = false;
        self.dynamic = (!failed).then_some((entry, lines));
        self.rev += 1;
        true
    }

    /// The index of `row` among the rows the open level lists (0 when absent).
    pub fn here(&mut self, row: RowId) -> usize {
        let level = self.level;
        self.position(level, row)
    }

    /// The index of `id` among the rows `level` lists (0 when absent).
    fn position(&mut self, level: Level, id: RowId) -> usize {
        let saved = std::mem::replace(&mut self.level, level);
        let at = self.rows().iter().position(|r| r.id == id).unwrap_or(0);
        self.level = saved;
        at
    }

    /// Jumps straight to `entry`'s level (a Mod+Space hit that must ask or drill): its category
    /// with the root above, or the root. The stack holds the row each level came through, so up
    /// lands on it. The caller selects the row.
    pub fn goto_entry(&mut self, entry: usize) {
        self.stack.clear();
        self.level = Level::Root;
        self.question = None;
        self.cancel_read();
        self.rev += 1;
        if let Some(c) = self.catalog.entries.get(entry).and_then(|e| e.category) {
            let at = self.position(Level::Root, RowId::Category(c));
            self.stack.push((Level::Root, at));
            self.level = Level::Category(c);
        }
    }

    /// Jumps to the choices of `entry`: its category and the root above. Returns the read to
    /// start for a `choices_cmd` entry.
    pub fn goto_choices(&mut self, config: &ToolsConfig, entry: usize) -> Step {
        self.goto_entry(entry);
        let at = self.position(self.level, RowId::Entry(entry));
        self.drill_choices(config, entry, at)
    }
}

#[cfg(test)]
mod tests;
