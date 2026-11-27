//! The Super+X command panel's model: its tabs, the rows of the open tab and the actions a
//! tool's detail card lists. Pure: no thread, no command, no log.
//!
//! See: docs/superpowers/specs/2026-11-26-superx-panel-design.md.

use super::catalog::{BUILTINS, BuiltinAction, Catalog, Choices, Entry, EntryKind, Item};

/// How many tools the All tab's Recent section holds at most.
pub const RECENT_MAX: usize = 4;

/// A tab of the strip: All, then each category that holds a tool, in category order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    All,
    /// An index into `Catalog::categories`.
    Category(usize),
}

/// A section label of the All tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Pinned,
    Recent,
    /// An index into `Catalog::categories`: its hue dot and its name in capitals.
    Category(usize),
    /// The Keybindings level's All tab: an index into `Binds::groups`, its name in capitals.
    Group(usize),
}

/// A row of the open tab: a section label, or a tool (an index into `Catalog::entries`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRow {
    Section(Label),
    Tool(usize),
}

/// The panel's tabs, the open one and its rows. Built at open and at each tab change, from the
/// catalog and the Recent entries read once at open: nothing else is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    pub tabs: Vec<Tab>,
    /// An index into `tabs`.
    pub active: usize,
    pub rows: Vec<PanelRow>,
}

/// The entries Pinned lists: the root's, in its order (`[tools] root`, then the user entries with
/// no category).
fn pinned(catalog: &Catalog) -> Vec<usize> {
    catalog.root_rows().into_iter().filter_map(|r| if let Item::Entry(e) = r { Some(e) } else { None }).collect()
}

impl Panel {
    /// The panel on tab `active` (past the last: the last), `recent` its Recent section.
    pub fn build(catalog: &Catalog, recent: &[usize], active: usize) -> Panel {
        let rows = catalog.root_rows();
        let mut tabs = vec![Tab::All];
        // `root_rows` lists only the categories that hold an entry, in category order.
        tabs.extend(rows.iter().filter_map(|r| if let Item::Category(k) = *r { Some(Tab::Category(k)) } else { None }));
        let active = active.min(tabs.len().saturating_sub(1));
        let mut out = Vec::new();
        match tabs.get(active).copied().unwrap_or(Tab::All) {
            Tab::All => {
                let mut section = |label: Label, tools: Vec<usize>| {
                    if !tools.is_empty() {
                        out.push(PanelRow::Section(label));
                        out.extend(tools.into_iter().map(PanelRow::Tool));
                    }
                };
                section(Label::Pinned, pinned(catalog));
                section(Label::Recent, recent.to_vec());
                for tab in &tabs {
                    if let Tab::Category(k) = *tab {
                        section(Label::Category(k), catalog.category_rows(k));
                    }
                }
            }
            Tab::Category(k) => out.extend(catalog.category_rows(k).into_iter().map(PanelRow::Tool)),
        }
        Panel { tabs, active, rows: out }
    }

    /// The selectable rows' entries, in row order: a tool shown twice is there twice.
    pub fn tools(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows.iter().filter_map(|r| if let PanelRow::Tool(e) = *r { Some(e) } else { None })
    }
}

/// A line of the list: a section label, or the first of a tool's two (`tool` indexes the
/// selectable rows, `Panel::tools` at home).
#[derive(Debug, Clone, Copy)]
pub struct Line {
    pub at: usize,
    pub row: PanelRow,
    pub tool: usize,
}

impl Line {
    /// Whether this is a tool drawn whole in a pane of `pane_rows` lines from line `start`: a tool
    /// cut by the pane's edge is not drawn (nor its capsule, nor does a click or a letter reach it).
    pub fn drawn(&self, start: usize, pane_rows: usize) -> bool {
        matches!(self.row, PanelRow::Tool(_)) && self.at >= start && self.at + 2 <= start + pane_rows
    }
}

/// The list's lines, then the first line shown: tool `top` (with its label right above it), moved
/// down only as far as tool `selected` needs to show whole. Shared by the view and the letters, so
/// a letter reaches exactly the tools drawn.
pub fn window(rows: &[PanelRow], top: usize, selected: Option<usize>, pane_rows: usize) -> (Vec<Line>, usize) {
    let mut lines = Vec::with_capacity(rows.len());
    let (mut at, mut tool) = (0, 0);
    for &row in rows {
        lines.push(Line { at, row, tool });
        match row {
            PanelRow::Section(_) => at += 1,
            PanelRow::Tool(_) => {
                at += 2;
                tool += 1;
            }
        }
    }
    let total = at;
    // The first line of tool `k`, its label's when the label is right above it.
    let begin = |k: usize| {
        let Some(i) = lines.iter().position(|x| matches!(x.row, PanelRow::Tool(_)) && x.tool == k) else { return 0 };
        let above = i.checked_sub(1).and_then(|p| lines.get(p)).filter(|x| matches!(x.row, PanelRow::Section(_)));
        above.or_else(|| lines.get(i)).map_or(0, |x| x.at)
    };
    let mut start = begin(top);
    if let Some(selected) = selected {
        let first = begin(selected);
        let end = lines.iter().find(|x| matches!(x.row, PanelRow::Tool(_)) && x.tool == selected).map_or(0, |x| x.at + 2);
        if end > start + pane_rows {
            start = end - pane_rows;
        }
        start = start.min(first);
    }
    (lines, start.min(total.saturating_sub(pane_rows)))
}

/// The entry a history key names: its own key, or one of its fixed choices' (`<key>/<choice>`).
fn entry_of(catalog: &Catalog, key: &str) -> Option<usize> {
    catalog.entries.iter().position(|e| e.key == key).or_else(|| {
        catalog.entries.iter().position(|e| {
            matches!(e.choices, Choices::Fixed(_)) && key.strip_prefix(e.key.as_str()).is_some_and(|rest| rest.starts_with('/'))
        })
    })
}

/// The Recent section: the first `n` distinct entries `keys` (the launcher's history keys, newest
/// first) name that still show and are not pinned. A key of no shown entry is skipped.
pub fn recent_entries(catalog: &Catalog, keys: &[String], n: usize) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for key in keys {
        if out.len() >= n {
            break;
        }
        if let Some(e) = entry_of(catalog, key)
            && !out.contains(&e)
            && !catalog.is_pinned(e)
        {
            out.push(e);
        }
    }
    out
}

/// A line of a tool's detail card, derived from what the tool is (never written per tool).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action<'a> {
    /// `Run ⏎`.
    Run,
    /// `Open ⏎`: it opens a panel or a box of its own.
    Open,
    /// `Choose →`: it has choices.
    Choose,
    /// `Type <label> ⏎`: it asks for a text, under this label.
    Type(&'a str),
    /// `Confirm ⏎ ⏎`: it asks first.
    Confirm,
    /// `Turn on/off ⏎`: a state it flips.
    Toggle,
    /// `Quick key <letter>`.
    QuickKey(char),
}

/// Whether a built-in's action opens something rather than doing it at once.
fn opens(action: BuiltinAction) -> bool {
    matches!(
        action,
        BuiltinAction::Quick(_)
            | BuiltinAction::Shield
            | BuiltinAction::Tab
            | BuiltinAction::Preset(_)
            | BuiltinAction::Notes
            | BuiltinAction::Timer
            | BuiltinAction::Clipboard
            | BuiltinAction::Emoji
            | BuiltinAction::Power
            | BuiltinAction::Kill
            | BuiltinAction::Binds
    )
}

/// The detail card's actions for `entry`: what Enter does first (choose, type, flip, confirm,
/// open or run), what follows it (a prompt after a choice, a question after any of them: a toggle
/// that asks lists the flip, then the question), then its letter.
pub fn actions(entry: &Entry) -> Vec<Action<'_>> {
    let choices = !matches!(entry.choices, Choices::None);
    let mut out = Vec::new();
    if choices {
        out.push(Action::Choose);
    }
    if let Some(label) = &entry.ask {
        out.push(Action::Type(label));
    }
    // A plain tool that asks: the question is what Enter does, no `Run` above it.
    if out.is_empty() && (entry.has_state || !entry.confirm) {
        let builtin = match entry.kind {
            EntryKind::Builtin(b) => BUILTINS.get(b).map(|b| b.action),
            EntryKind::User(_) => None,
        };
        out.push(if entry.has_state {
            Action::Toggle
        } else if builtin.is_some_and(opens) {
            Action::Open
        } else {
            Action::Run
        });
    }
    if entry.confirm {
        out.push(Action::Confirm);
    }
    out.extend(entry.letter.map(Action::QuickKey));
    out
}

#[cfg(test)]
mod tests;
