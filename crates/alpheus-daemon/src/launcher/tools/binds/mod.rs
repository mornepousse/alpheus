//! The keybinding cheatsheet's parser: the compositor's key bindings read from its config files,
//! grouped and described. Pure: the files come through the `Files` seam, which the glue
//! (`launcher/tools/app.rs`) fills with the disk when the Keybindings level opens.
//!
//! `roots` names the first files (`binds_files`, else the dialect's own config); `load` walks them
//! breadth-first in file order, each file's includes queued after it, and the dialect's parser
//! (`mango.rs`, `sway.rs`) turns each line into a `Bind`. Descriptions and combos come from
//! `describe.rs`.
//!
//! Never:
//! - read without a bound: MAX_FILES files, MAX_BYTES in all, MAX_BINDS binds, MAX_GROUPS groups,
//!   a line cut at MAX_LINE, MAX_VARS variables of MAX_VAR bytes; past one, the first are kept and `Binds::cut` counts the rest;
//! - read a file twice: an include cycle stops at the file seen;
//! - write, run or log anything: the glue logs `Binds`' counts, never a path, combo or command.
//!
//! See: docs/superpowers/specs/2026-11-27-session-binds-design.md ("Cheatsheet").

pub mod describe;
mod mango;
mod sway;

use super::nav::{RowId, ToolRow};
use super::panel::{Label, PanelRow};
use crate::launcher::matching::{Hit, Source};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The compositor whose config is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Mango,
    Sway,
}

/// The first files of a load: `Each` reads every one (`binds_files`), `FirstOf` only the first
/// readable one (a dialect's default places).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Roots {
    Each(Vec<PathBuf>),
    FirstOf(Vec<PathBuf>),
}

/// One binding: its combo as printed (`Super+Shift+A`), its description, its command as written
/// (variables expanded), and indexes into `Binds::groups` and `Binds::files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    pub combo: String,
    pub description: String,
    pub command: String,
    pub group: usize,
    pub file: usize,
}

/// What the bounds left out, each counted past its bound: `files` include lines naming a file
/// past MAX_FILES, `bytes` reads the MAX_BYTES budget cut short or skipped, `binds` binds past
/// MAX_BINDS, `groups` headers past MAX_GROUPS (their binds stay in the group before), `lines`
/// lines cut at MAX_LINE, `vars` variable definitions past MAX_VARS or longer than MAX_VAR.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cut {
    pub files: usize,
    pub bytes: usize,
    pub binds: usize,
    pub groups: usize,
    pub lines: usize,
    pub vars: usize,
}

/// The result of a load. `files` holds the last path component of each file read, in reading
/// order; `found` says a root was readable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Binds {
    pub groups: Vec<String>,
    pub binds: Vec<Bind>,
    pub files: Vec<String>,
    pub not_understood: usize,
    pub found: bool,
    pub cut: Cut,
}

/// The seam to the disk. `read` gives at most `max` bytes of the file's text, None when it cannot
/// be read; `list` gives a folder's entries, sorted.
pub trait Files {
    fn read(&self, path: &Path, max: usize) -> Option<String>;
    fn list(&self, dir: &Path) -> Vec<PathBuf>;
}

pub const MAX_FILES: usize = 8;
pub const MAX_BYTES: usize = 512 * 1024;
pub const MAX_BINDS: usize = 1000;
pub const MAX_GROUPS: usize = 32;
pub const MAX_LINE: usize = 1024;
/// Variables a load keeps (`env=`), and the bytes of a variable's name or value: `expand` scans
/// them for every `$`, so a config of thousands of definitions must not make it quadratic.
pub const MAX_VARS: usize = 64;
pub const MAX_VAR: usize = 128;

/// The files a load starts from: `binds_files` (`~/` expanded from HOME) when any, else the
/// dialect's config under `$XDG_CONFIG_HOME` (absolute only), else under `~/.config`; for Sway,
/// `/etc/sway/config` after it, read only when the user's is not.
pub fn roots(dialect: Dialect, var: impl Fn(&str) -> Option<OsString>, binds_files: &[String]) -> Roots {
    let home = var("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    if !binds_files.is_empty() {
        let expand = |p: &String| match (p.strip_prefix("~/"), &home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(p),
        };
        return Roots::Each(binds_files.iter().map(expand).collect());
    }
    let config = var("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| home.map(|h| h.join(".config")));
    let file = match dialect {
        Dialect::Mango => "mango/config.conf",
        Dialect::Sway => "sway/config",
    };
    let mut first: Vec<PathBuf> = config.map(|c| c.join(file)).into_iter().collect();
    if dialect == Dialect::Sway {
        // Sway itself falls back to the system's config when the user has none.
        first.push(PathBuf::from("/etc/sway/config"));
    }
    Roots::FirstOf(first)
}

/// Reads the bindings from `roots` through `files`, within the bounds.
pub fn load(dialect: Dialect, roots: &Roots, files: &dyn Files) -> Binds {
    let mut walk = Walk { files, st: Parse::new(dialect), taken: Vec::new(), queue: VecDeque::new(), bytes: 0 };
    match roots {
        Roots::Each(paths) => {
            for p in paths {
                walk.take(normal(p));
            }
            // The roots come first in the queue; includes queue behind them.
            for _ in 0..walk.queue.len() {
                if let Some(p) = walk.queue.pop_front()
                    && walk.visit(&p)
                {
                    walk.st.out.found = true;
                }
            }
        }
        Roots::FirstOf(paths) => {
            for p in paths {
                let p = normal(p);
                walk.taken.push(p.clone());
                if walk.visit(&p) {
                    walk.st.out.found = true;
                    break;
                }
                walk.taken.pop();
            }
        }
    }
    while let Some(p) = walk.queue.pop_front() {
        walk.visit(&p);
    }
    walk.st.out
}

/// The walk over the files: the paths taken so far (read or queued, MAX_FILES at most), the queue,
/// the bytes read.
struct Walk<'a> {
    files: &'a dyn Files,
    st: Parse,
    taken: Vec<PathBuf>,
    queue: VecDeque<PathBuf>,
    bytes: usize,
}

impl Walk<'_> {
    /// Queues `path` unless it was taken already; past MAX_FILES, counts it instead.
    fn take(&mut self, path: PathBuf) {
        if self.taken.contains(&path) {
            return;
        }
        if self.taken.len() >= MAX_FILES {
            self.st.out.cut.files += 1;
            return;
        }
        self.taken.push(path.clone());
        self.queue.push_back(path);
    }

    /// Reads and parses one file, queueing its includes; false when it could not be read.
    fn visit(&mut self, path: &Path) -> bool {
        let left = MAX_BYTES.saturating_sub(self.bytes);
        if left == 0 {
            self.st.out.cut.bytes += 1;
            return false;
        }
        let Some(mut text) = self.files.read(path, left + 1) else {
            return false;
        };
        if text.len() > left {
            // Cut at the budget, then back to the end of the last whole line.
            let mut end = left;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(text.get(..end).and_then(|t| t.rfind('\n')).map_or(0, |n| n + 1));
            self.st.out.cut.bytes += 1;
        }
        self.bytes += text.len();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.st.start_file(name);
        let here = path.parent().unwrap_or(Path::new("/"));
        let includes = match self.st.dialect {
            Dialect::Mango => mango::parse(&text, here, &mut self.st),
            Dialect::Sway => sway::parse(&text, here, &mut self.st, self.files, &self.taken),
        };
        for p in includes {
            self.take(normal(&p));
        }
        true
    }
}

/// The parse state the dialects fill, carried across the files of one load.
pub(crate) struct Parse {
    pub(crate) dialect: Dialect,
    pub(crate) out: Binds,
    /// Variables defined so far (`env=NAME,value`), first definition first.
    pub(crate) vars: Vec<(String, String)>,
    /// The current file's index in `out.files`.
    file: usize,
    /// The current group: its name, and its index once a bind made it exist.
    group_name: String,
    group: Option<usize>,
    /// The comment line right above the current line, when there is one.
    pub(crate) above: Option<String>,
}

impl Parse {
    fn new(dialect: Dialect) -> Parse {
        Parse { dialect, out: Binds::default(), vars: Vec::new(), file: 0, group_name: String::new(), group: None, above: None }
    }

    fn default_group(&self) -> &'static str {
        match self.dialect {
            Dialect::Mango => "General",
            Dialect::Sway => "Default",
        }
    }

    /// A new file starts in the default group, with no comment above.
    fn start_file(&mut self, name: String) {
        self.file = self.out.files.len();
        self.out.files.push(clean(&name));
        self.open_group(self.default_group().to_string());
        self.above = None;
    }

    /// Makes `name` the group of the next binds; it exists once a bind lands in it.
    pub(crate) fn open_group(&mut self, name: String) {
        self.group_name = clean(&name);
        self.group = None;
    }

    /// Makes `name` the group of the next binds, giving the name of the group it replaces.
    pub(crate) fn enter_group(&mut self, name: String) -> String {
        let before = std::mem::take(&mut self.group_name);
        self.open_group(name);
        before
    }

    /// A line cut to MAX_LINE bytes (counted when cut).
    pub(crate) fn cut_line<'t>(&mut self, line: &'t str) -> &'t str {
        if line.len() <= MAX_LINE {
            return line;
        }
        self.out.cut.lines += 1;
        let mut end = MAX_LINE;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.get(..end).unwrap_or_default()
    }

    /// Adds one bind to the current group (made to exist if it was only named).
    pub(crate) fn push(&mut self, combo: String, description: String, command: String) {
        if self.out.binds.len() >= MAX_BINDS {
            self.out.cut.binds += 1;
            return;
        }
        let group = match self.group {
            Some(g) => g,
            None => {
                let g = match self.out.groups.iter().position(|n| *n == self.group_name) {
                    Some(g) => Some(g),
                    None if self.out.groups.len() < MAX_GROUPS => {
                        self.out.groups.push(self.group_name.clone());
                        Some(self.out.groups.len() - 1)
                    }
                    None => {
                        self.out.cut.groups += 1;
                        // Past the bound the binds stay in the group before, or the last one.
                        self.out.groups.len().checked_sub(1)
                    }
                };
                let g = g.unwrap_or(0);
                self.group = Some(g);
                g
            }
        };
        self.out.binds.push(Bind {
            combo: clean(&combo),
            description: clean(&description),
            command: clean(&command),
            group,
            file: self.file,
        });
    }

    /// Defines `name` as `value` (a later definition replaces an earlier one); a name or value past
    /// MAX_VAR bytes, or a new name past MAX_VARS, is ignored and counted.
    pub(crate) fn define(&mut self, name: &str, value: &str) {
        if name.len() > MAX_VAR || value.len() > MAX_VAR {
            self.out.cut.vars += 1;
            return;
        }
        let full = self.vars.len() >= MAX_VARS;
        match self.vars.iter_mut().find(|(n, _)| n == name) {
            Some(var) => value.clone_into(&mut var.1),
            None if full => self.out.cut.vars += 1,
            None => self.vars.push((name.to_string(), value.to_string())),
        }
    }

    /// The longest defined variable name `text` starts with.
    fn longest_var_at<'t>(&self, text: &'t str) -> Option<&'t str> {
        let n = self.vars.iter().map(|(n, _)| n).filter(|n| !n.is_empty() && text.starts_with(n.as_str())).map(String::len).max()?;
        text.get(..n)
    }

    /// `text` with each `$NAME` or `${NAME}` defined in `vars` replaced; others kept. The result
    /// is cut at MAX_LINE.
    pub(crate) fn expand(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find('$') {
            out.push_str(rest.get(..at).unwrap_or_default());
            let after = rest.get(at + 1..).unwrap_or_default();
            let (name, len) = match after.strip_prefix('{') {
                Some(inner) => match inner.find('}') {
                    Some(close) => (inner.get(..close).unwrap_or_default(), close + 2),
                    None => ("", 0),
                },
                // Sway tries the defined names longest first (`$ws-1`, and `$modx` is `$mod` then
                // `x`); Mango's names are a run of letters, digits and `_`.
                None if self.dialect == Dialect::Sway => match self.longest_var_at(after) {
                    Some(n) => (n, n.len()),
                    None => {
                        let len = word_len(after);
                        (after.get(..len).unwrap_or_default(), len)
                    }
                },
                None => {
                    let len = word_len(after);
                    (after.get(..len).unwrap_or_default(), len)
                }
            };
            match self.vars.iter().find(|(n, _)| !name.is_empty() && n == name) {
                Some((_, value)) => out.push_str(value),
                None => {
                    out.push('$');
                    out.push_str(after.get(..len).unwrap_or_default());
                }
            }
            rest = after.get(len..).unwrap_or_default();
            if out.len() > MAX_LINE {
                break;
            }
        }
        out.push_str(rest);
        if out.len() > MAX_LINE {
            let mut end = MAX_LINE;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
        }
        out
    }
}

/// The length of the run of letters, digits and `_` `text` starts with.
fn word_len(text: &str) -> usize {
    text.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(text.len())
}

/// Text from a config file made safe to draw: control characters, escape sequences and
/// bidirectional overrides dropped (the file is the user's, but a pasted line can hold them).
fn clean(text: &str) -> String {
    alpheus_common::text::clean_line(text, MAX_LINE)
}

/// What a load is worth saying, in fixed words and counts (never a path, combo, command or group
/// name): one `note` per line, which the glue logs.
pub fn notes(b: &Binds, note: &mut dyn FnMut(&str)) {
    if !b.found {
        note("tools: binds no source");
        return;
    }
    let (n, groups) = (b.binds.len(), b.groups.len());
    note(&format!("tools: binds shown ({n} binds, {groups} groups)"));
    let skipped = b.not_understood;
    if skipped > 0 {
        note(&format!("tools: binds {skipped} lines not understood"));
    }
    let Cut { files, bytes, binds, groups, lines, vars } = b.cut;
    if b.cut != Cut::default() {
        note(&format!("tools: binds cut (files {files}, bytes {bytes}, binds {binds}, groups {groups}, lines {lines}, vars {vars})"));
    }
}

/// The level's list on tab `tab` (0: All, k: group k-1): All shows each group that holds a bind
/// as a section, its binds under it; a group's tab shows its binds alone. `PanelRow::Tool` holds
/// an index into `Binds::binds`.
pub fn rows(binds: &Binds, tab: usize) -> Vec<PanelRow> {
    let of = |g: usize| binds.binds.iter().enumerate().filter(move |(_, b)| b.group == g).map(|(i, _)| PanelRow::Tool(i));
    match tab.checked_sub(1) {
        None => {
            let mut out = Vec::with_capacity(binds.binds.len() + binds.groups.len());
            for g in 0..binds.groups.len() {
                let at = out.len();
                out.extend(of(g));
                if out.len() > at {
                    out.insert(at, PanelRow::Section(Label::Group(g)));
                }
            }
            out
        }
        Some(g) => of(g).collect(),
    }
}

/// A search's `hits` (over the level's `rows`) grouped by group: the groups in the order of their
/// best hit, the hits of one group in rank order. Stable; a hit on no bind keeps its place last.
pub fn group_hits(hits: &[Hit], rows: &[ToolRow], binds: &Binds) -> Vec<Hit> {
    let group = |h: &Hit| match h.source {
        Source::Tool(i) => match rows.get(i).map(|r| r.id) {
            Some(RowId::Bind(b)) => binds.binds.get(b).map(|b| b.group),
            _ => None,
        },
        _ => None,
    };
    let mut order: Vec<Option<usize>> = Vec::new();
    for h in hits {
        let g = group(h);
        if !order.contains(&g) {
            order.push(g);
        }
    }
    let mut out = Vec::with_capacity(hits.len());
    for g in order {
        out.extend(hits.iter().filter(|h| group(h) == g).copied());
    }
    out
}

/// `path` with its `.` components dropped and each `..` taking off the component before, so the
/// same file always has the same path (the cycle check compares them).
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
