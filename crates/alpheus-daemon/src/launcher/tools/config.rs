//! `[tools]`, `[[tools.category]]` and `[[tools.entry]]` of launcher.toml: the
//! structure, its validation and its bounds. Pure: no thread, no command, no log (the bound
//! notes go through a callback the caller logs).

use serde::Deserialize;
use std::collections::HashSet;

/// At most this many `[[tools.category]]`; the first ones are kept.
pub const MAX_CATEGORIES: usize = 16;
/// At most this many `[[tools.entry]]`; the first ones are kept.
pub const MAX_ENTRIES: usize = 128;
/// Fixed `choices` per entry; the first ones are kept.
pub const MAX_CHOICES: usize = 32;
/// Entries whose `state` is read; past it an entry shows with no state.
pub const MAX_STATES: usize = 16;
/// A category's or an entry's name: 1 to TOOL_NAME_MAX characters (not bytes).
pub const TOOL_NAME_MAX: usize = 48;
/// A fixed choice: 1 to CHOICE_MAX characters.
pub const CHOICE_MAX: usize = 64;
/// `order` is 0 to ORDER_MAX.
pub const ORDER_MAX: u16 = 1000;
/// The `order` of a category or an entry that sets none.
pub const DEFAULT_ORDER: u16 = 50;
/// The one element of `run` replaced by the chosen line, whole: never part of an argument.
pub const CHOICE_PLACEHOLDER: &str = "{choice}";
/// The built-in categories; an entry may name one, a `[[tools.category]]` may restyle one.
pub const BUILTIN_CATEGORIES: [&str; 5] = ["Quick settings", "Control center", "Utilities", "System", "Session"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    /// `false`: no built-in entry.
    pub builtins: bool,
    /// Built-in ids never shown.
    pub hide: Vec<String>,
    /// Built-in ids also listed at the root, first, in this order.
    pub root: Vec<String>,
    /// The "Reload the bar" built-in's argv; empty hides it.
    pub reload: Vec<String>,
    #[serde(rename = "category")]
    pub categories: Vec<CategoryConfig>,
    #[serde(rename = "entry")]
    pub entries: Vec<EntryConfig>,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        ToolsConfig { builtins: true, hide: Vec::new(), root: Vec::new(), reload: Vec::new(), categories: Vec::new(), entries: Vec::new() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CategoryConfig {
    pub name: String,
    /// One character; absent: a generic folder glyph.
    pub icon: Option<String>,
    pub order: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EntryConfig {
    pub name: String,
    /// A `[[tools.category]]` or a built-in category; absent: the root.
    pub category: Option<String>,
    pub icon: Option<String>,
    pub order: Option<u16>,
    /// Argv, no shell; with choices, one element is exactly `{choice}`.
    pub run: Vec<String>,
    /// `terminal` in front of `run`.
    pub terminal: bool,
    /// `terminal_hold` in front of `run`.
    pub hold: bool,
    pub confirm: bool,
    /// Argv run when the Tools mode is entered: exit 0 is on, any other exit off.
    pub state: Vec<String>,
    pub choices: Vec<String>,
    /// Argv whose output lines are the choices, run at drill-in.
    pub choices_cmd: Vec<String>,
    /// 1-based position in the file (what logs name an entry by); set after parsing.
    #[serde(skip)]
    pub position: usize,
}

impl EntryConfig {
    /// Whether the entry has choices, fixed or dynamic.
    pub fn has_choices(&self) -> bool {
        !self.choices.is_empty() || !self.choices_cmd.is_empty()
    }
}

/// Numbers the entries by their place in the file (1-based).
pub fn number(t: &mut ToolsConfig) {
    for (i, e) in t.entries.iter_mut().enumerate() {
        e.position = i + 1;
    }
}

fn bad_text(s: &str) -> bool {
    s.contains(|c: char| c.is_control() || c == '\u{FFFD}')
}

/// A name or a choice: 1 to `max` characters, not blank, no control character or U+FFFD.
fn check_text(at: &str, what: &str, s: &str, max: usize) -> Result<(), String> {
    if !(1..=max).contains(&s.chars().count()) {
        return Err(format!("{at}: {what} must be 1 to {max} characters"));
    }
    if s.trim().is_empty() {
        return Err(format!("{at}: {what} must not be blank"));
    }
    if bad_text(s) {
        return Err(format!("{at}: {what} must not hold a control character or U+FFFD"));
    }
    Ok(())
}

/// A name: `check_text`, and no `/` (it separates the parts of a history key).
fn check_name(at: &str, s: &str) -> Result<(), String> {
    check_text(at, "name", s, TOOL_NAME_MAX)?;
    if s.contains('/') {
        return Err(format!("{at}: name must not hold \"/\""));
    }
    Ok(())
}

fn check_icon(at: &str, icon: &Option<String>) -> Result<(), String> {
    match icon {
        Some(i) if i.chars().count() != 1 => Err(format!("{at}: icon must be one character")),
        _ => Ok(()),
    }
}

fn check_order(at: &str, order: Option<u16>) -> Result<(), String> {
    match order {
        Some(o) if o > ORDER_MAX => Err(format!("{at}: order {o} past {ORDER_MAX}")),
        _ => Ok(()),
    }
}

/// An argv with a non-blank program.
fn check_argv(at: &str, key: &str, argv: &[String]) -> Result<(), String> {
    match argv.first() {
        Some(p) if !p.trim().is_empty() => Ok(()),
        _ => Err(format!("{at}: {key} empty (expected: [\"program\", \"argument\", …])")),
    }
}

/// The rules of a valid `[tools]` table. A message names the key and `tools.entry <i>` / `tools.category <i>`
/// (1-based), never the text of a name, command or choice.
pub fn validate(t: &ToolsConfig) -> Result<(), String> {
    if !t.reload.is_empty() {
        check_argv("tools.reload", "reload", &t.reload)
            .map_err(|_| "tools.reload: empty program (expected: [\"program\", \"argument\", …], or [] to hide)".to_string())?;
    }
    // Called before `bounded`: what a bound would cut is checked all the same (the sets keep it linear).
    let mut category_names = HashSet::new();
    for (i, c) in t.categories.iter().enumerate() {
        let at = format!("tools.category {}", i + 1);
        check_name(&at, &c.name)?;
        if !category_names.insert(c.name.as_str()) {
            return Err(format!("{at}: name used twice"));
        }
        check_icon(&at, &c.icon)?;
        check_order(&at, c.order)?;
    }
    let mut entry_names = HashSet::new();
    for (i, e) in t.entries.iter().enumerate() {
        // The place in the file (set by `number`); a hand-built config has none: its index.
        let at = format!("tools.entry {}", if e.position > 0 { e.position } else { i + 1 });
        check_name(&at, &e.name)?;
        if let Some(cat) = &e.category
            && !category_names.contains(cat.as_str())
            && !BUILTIN_CATEGORIES.contains(&cat.as_str())
        {
            return Err(format!("{at}: category names no category"));
        }
        if !entry_names.insert((e.category.as_deref(), e.name.as_str())) {
            return Err(format!("{at}: name used twice in its category"));
        }
        check_icon(&at, &e.icon)?;
        check_order(&at, e.order)?;
        if e.terminal && e.hold {
            return Err(format!("{at}: terminal and hold are both set"));
        }
        if !e.choices.is_empty() && !e.choices_cmd.is_empty() {
            return Err(format!("{at}: choices and choices_cmd are both set"));
        }
        if !e.state.is_empty() && e.has_choices() {
            return Err(format!("{at}: state with choices (an entry with choices has no single state)"));
        }
        check_argv(&at, "run", &e.run)?;
        if !e.state.is_empty() {
            check_argv(&at, "state", &e.state)?;
        }
        if !e.choices_cmd.is_empty() {
            check_argv(&at, "choices_cmd", &e.choices_cmd)?;
        }
        for (k, choice) in e.choices.iter().enumerate() {
            check_text(&at, &format!("choice {}", k + 1), choice, CHOICE_MAX)?;
        }
        let whole = e.run.iter().filter(|a| a.as_str() == CHOICE_PLACEHOLDER).count();
        let partial = e.run.iter().any(|a| a.contains(CHOICE_PLACEHOLDER) && a.as_str() != CHOICE_PLACEHOLDER);
        if e.has_choices() {
            if e.run.first().is_some_and(|p| p.as_str() == CHOICE_PLACEHOLDER) {
                return Err(format!("{at}: run's program must not be \"{CHOICE_PLACEHOLDER}\""));
            }
            if whole != 1 || partial {
                return Err(format!("{at}: run must hold \"{CHOICE_PLACEHOLDER}\" exactly once, as a whole argument"));
            }
        } else if whole != 0 || partial {
            return Err(format!("{at}: run holds \"{CHOICE_PLACEHOLDER}\" but the entry has no choices"));
        }
    }
    Ok(())
}

/// Cuts what is past a bound, with one `note` per bound (counts only): categories
/// (an entry of a category cut is dropped, not moved to the root, and counted in the same
/// note), entries, the fixed choices past `MAX_CHOICES` (one note for all the entries), and the
/// `state` past the 16th entry that has one. Run after `validate`: what is cut was checked.
pub fn bounded(mut t: ToolsConfig, note: &mut dyn FnMut(&str)) -> ToolsConfig {
    // What launcher.toml held, before an entry of a cut category is dropped.
    let file_entries = t.entries.len();
    if t.categories.len() > MAX_CATEGORIES {
        let cut: HashSet<String> = t.categories.iter().skip(MAX_CATEGORIES).map(|c| c.name.clone()).collect();
        let total = t.categories.len();
        t.categories.truncate(MAX_CATEGORIES);
        let kept: HashSet<&str> = t.categories.iter().map(|c| c.name.as_str()).collect();
        let before = t.entries.len();
        t.entries.retain(|e| {
            e.category.as_ref().is_none_or(|c| !cut.contains(c) || kept.contains(c.as_str()) || BUILTIN_CATEGORIES.contains(&c.as_str()))
        });
        let dropped = before - t.entries.len();
        let mut line = format!("tools: {total} categories in launcher.toml, the first {MAX_CATEGORIES} kept");
        if dropped > 0 {
            line.push_str(&format!(", {dropped} entries of the others dropped"));
        }
        note(&line);
    }
    if t.entries.len() > MAX_ENTRIES {
        note(&format!("tools: {file_entries} entries in launcher.toml, the first {MAX_ENTRIES} kept"));
        t.entries.truncate(MAX_ENTRIES);
    }
    let long = t.entries.iter().filter(|e| e.choices.len() > MAX_CHOICES).count();
    if long > 0 {
        note(&format!("tools: {long} entries past {MAX_CHOICES} choices, the first {MAX_CHOICES} kept"));
        for e in &mut t.entries {
            e.choices.truncate(MAX_CHOICES);
        }
    }
    let states = t.entries.iter().filter(|e| !e.state.is_empty()).count();
    if states > MAX_STATES {
        note(&format!("tools: {states} entries with a state, the first {MAX_STATES} read"));
        let mut seen = 0;
        for e in t.entries.iter_mut().filter(|e| !e.state.is_empty()) {
            seen += 1;
            if seen > MAX_STATES {
                e.state.clear();
            }
        }
    }
    t
}

#[cfg(test)]
mod tests;
