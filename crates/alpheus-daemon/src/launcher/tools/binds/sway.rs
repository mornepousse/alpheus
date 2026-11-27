//! Sway's config lines: `bindsym` and `bindcode` (`[--flags] combo action…`, alone or as a
//! `bindsym { combo action }` block) become binds; `set $name value` defines a variable the lines
//! after it expand; `include path` names files to read next (relative to the including file, a `*`
//! or `?` in the last component listing that folder); `mode "name" { … }` is the group `name`;
//! outside a mode, `# ── Name ──` opens a group. Every other block (`bar`, `input`, `output`…) is
//! skipped whole, its own `bindsym` lines too (a bar's are mouse buttons on the bar). A line ending
//! in `\` goes on with the next.

use super::describe::{self, rule_name};
use super::mango::split_comment;
use super::{Dialect, Files, MAX_FILES, MAX_LINE, Parse, normal};
use std::path::{Path, PathBuf};

/// Blocks open at once that are told apart; deeper ones count as skipped blocks, so a config of
/// thousands of `{` costs a counter, not a list.
const MAX_DEPTH: usize = 8;

/// An open `{ … }` block.
enum Block {
    /// `mode "name" {`: the group the binds were in before it.
    Mode(String),
    /// `bindsym {` or `bindcode {`: each line inside is `combo action`.
    Binds { code: bool },
    /// Any other block: skipped whole.
    Other,
}

/// The block nesting of one file: the blocks told apart, and how many deeper ones are open.
#[derive(Default)]
struct Blocks {
    open: Vec<Block>,
    deeper: usize,
}

impl Blocks {
    fn push(&mut self, block: Block) {
        if self.open.len() < MAX_DEPTH && self.deeper == 0 {
            self.open.push(block);
        } else {
            self.deeper += 1;
        }
    }

    /// Closes the innermost block; a stray `}` closes nothing.
    fn pop(&mut self) -> Option<Block> {
        if self.deeper > 0 {
            self.deeper -= 1;
            return Some(Block::Other);
        }
        self.open.pop()
    }

    /// Whether a block opened now would only be counted (past MAX_DEPTH).
    fn full(&self) -> bool {
        self.open.len() >= MAX_DEPTH || self.deeper > 0
    }

    fn skipping(&self) -> bool {
        self.deeper > 0 || self.open.iter().any(|b| matches!(b, Block::Other))
    }

    fn in_mode(&self) -> bool {
        self.open.iter().any(|b| matches!(b, Block::Mode(_)))
    }
}

/// Parses one file's `text` (its folder is `here`) into `st`; gives the paths it includes, in
/// order, a glob's matches sorted (through `files`, which lists the folder). `taken` is the paths
/// the walk holds already: once as many new paths are given as it has slots left, no glob is
/// listed any more.
pub(crate) fn parse(text: &str, here: &Path, st: &mut Parse, files: &dyn Files, taken: &[PathBuf]) -> Vec<PathBuf> {
    let mut includes = Vec::new();
    let mut blocks = Blocks::default();
    let mut joined = String::new();
    // Whether a piece of the line being joined went past MAX_LINE: the rest of it is dropped.
    let mut dropped = false;
    for raw in text.lines() {
        // A trailing `\` joins the next line, up to MAX_LINE bytes.
        if let Some(head) = raw.strip_suffix('\\') {
            let head = if joined.is_empty() { head } else { head.trim_start() };
            join_piece(&mut joined, head, &mut dropped);
            continue;
        }
        let whole;
        let raw = if joined.is_empty() && !dropped {
            raw
        } else {
            join_piece(&mut joined, raw.trim_start(), &mut dropped);
            if std::mem::take(&mut dropped) {
                st.out.cut.lines += 1;
            }
            whole = std::mem::take(&mut joined);
            whole.as_str()
        };
        let line = st.cut_line(raw).trim();
        if line.is_empty() {
            st.above = None;
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim_start_matches('#').trim();
            match rule_name(comment) {
                Some(Some(name)) if !blocks.in_mode() && !blocks.skipping() => {
                    st.open_group(name.to_string());
                    st.above = None;
                }
                Some(_) => st.above = None,
                None => st.above = Some(comment.to_string()).filter(|c| !c.is_empty()),
            }
            continue;
        }
        let above = st.above.take();
        if line.starts_with('}') {
            if let Some(Block::Mode(before)) = blocks.pop() {
                st.open_group(before);
            }
            continue;
        }
        if let Some(head) = line.strip_suffix('{') {
            let (key, rest) = word(head.trim());
            let block = match key {
                // Past the depth cap the block is only counted: it never changes the group.
                _ if blocks.skipping() || blocks.full() => Block::Other,
                "mode" => Block::Mode(st.enter_group(mode_name(rest))),
                "bindsym" | "bindcode" if words_are_flags(rest) => Block::Binds { code: key == "bindcode" },
                _ => Block::Other,
            };
            blocks.push(block);
            continue;
        }
        if blocks.skipping() {
            continue;
        }
        if let Some(Block::Binds { code }) = blocks.open.last() {
            let code = *code;
            let line = st.expand(line);
            bind(&line, code, above, st);
            continue;
        }
        let (key, rest) = word(line);
        match key {
            "set" => {
                let (name, value) = word(rest);
                if let Some(name) = name.strip_prefix('$') {
                    let value = st.expand(value.trim());
                    st.define(name, &value);
                }
            }
            "bindsym" | "bindcode" => {
                let rest = st.expand(rest);
                bind(&rest, key == "bindcode", above, st);
            }
            "include" => {
                let path = st.expand(rest.trim());
                include(&path, here, files, st, &mut includes, taken);
            }
            _ => {}
        }
    }
    includes
}

/// The first word of `text` and what follows it (untrimmed).
fn word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    (text.get(..end).unwrap_or_default(), text.get(end..).unwrap_or_default())
}

/// Whether every word of `text` is a `--flag` (a block header's `bindsym --locked {`).
fn words_are_flags(text: &str) -> bool {
    text.split_whitespace().all(|w| w.starts_with("--"))
}

/// A mode's name from what follows `mode`: its flags (`--pango_markup`) skipped, quotes dropped.
fn mode_name(rest: &str) -> String {
    let name = rest.split_whitespace().skip_while(|w| w.starts_with("--")).collect::<Vec<_>>().join(" ");
    name.trim_matches('"').to_string()
}

/// One bind, `[--flags] combo action…` (variables expanded); one short of a combo or an action
/// is counted as not understood.
fn bind(text: &str, code: bool, above: Option<String>, st: &mut Parse) {
    let mut rest = text.trim_start();
    let combo = loop {
        let (w, after) = word(rest);
        rest = after.trim_start();
        if !w.starts_with("--") {
            break w;
        }
    };
    let (action, trailing) = split_comment(rest.trim());
    if combo.is_empty() || action.is_empty() {
        st.out.not_understood += 1;
        return;
    }
    let (mods, key) = match combo.rsplit_once('+') {
        Some((mods, key)) if !key.is_empty() => (mods, key),
        _ => ("", combo),
    };
    let key = if code { format!("code:{key}") } else { key.to_string() };
    let description = match (trailing, above) {
        (Some(t), _) => t.to_string(),
        (None, Some(a)) => a,
        (None, None) => {
            let (function, args) = word(action);
            describe::humanise(Dialect::Sway, function, args.trim())
        }
    };
    st.push(describe::combo(Dialect::Sway, mods, &key), description, action.to_string());
}

/// Queues the file(s) `path` names: its quotes taken off, relative to `here`, `~`, `$HOME` and
/// `${HOME}` left to the glue (as `~`), a `*` or `?` in the last component matched against that
/// folder's entries (hidden ones only by a pattern starting with `.`); a pattern before the last
/// component, or a variable left unexpanded, is not followed and counted. Once the walk's free
/// slots are filled by new paths (neither in `taken` nor given already), a glob is counted in
/// `cut.files` instead of listed.
fn include(path: &str, here: &Path, files: &dyn Files, st: &mut Parse, out: &mut Vec<PathBuf>, taken: &[PathBuf]) {
    let path = unquote(path);
    let home = match path.strip_prefix("${HOME}").or_else(|| path.strip_prefix("$HOME")) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    };
    let path = home.as_str();
    if path.is_empty() {
        return;
    }
    if path.contains('$') {
        st.out.not_understood += 1;
        return;
    }
    let path = if path == "~" || path.starts_with("~/") || path.starts_with('/') { PathBuf::from(path) } else { here.join(path) };
    let pattern = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("/"));
    if dir.to_string_lossy().contains(['*', '?']) {
        st.out.not_understood += 1;
        return;
    }
    let free = MAX_FILES.saturating_sub(taken.len());
    if !pattern.contains(['*', '?']) {
        push_new(out, taken, free, path);
        return;
    }
    if out.len() >= free {
        st.out.cut.files += 1;
        return;
    }
    for entry in files.list(dir) {
        let name = entry.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if (pattern.starts_with('.') || !name.starts_with('.')) && glob(&pattern, &name) {
            push_new(out, taken, free, entry);
        }
    }
}

/// Adds `path` (normalised) to `out` unless the walk would skip it as taken already, so `out`
/// counts new paths only. Past `free` the check is skipped (the walk counts or skips the rest), so
/// a file of thousands of includes costs no quadratic search.
fn push_new(out: &mut Vec<PathBuf>, taken: &[PathBuf], free: usize, path: PathBuf) {
    let path = normal(&path);
    if out.len() < free && (taken.contains(&path) || out.contains(&path)) {
        return;
    }
    out.push(path);
}

/// Adds `piece` to the line being joined, up to MAX_LINE bytes: the part past it, and every piece
/// after it once `dropped`, is left out.
fn join_piece(joined: &mut String, piece: &str, dropped: &mut bool) {
    if *dropped {
        return;
    }
    let room = MAX_LINE.saturating_sub(joined.len());
    if piece.len() <= room {
        joined.push_str(piece);
        return;
    }
    let mut end = room;
    while !piece.is_char_boundary(end) {
        end -= 1;
    }
    joined.push_str(piece.get(..end).unwrap_or_default());
    *dropped = true;
}

/// `text` without one pair of surrounding quotes (`"…"` or `'…'`).
fn unquote(text: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(q).and_then(|t| t.strip_suffix(q)) {
            return inner;
        }
    }
    text
}

/// Whether `name` matches `pattern`, where `*` is any run of characters and `?` one character.
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // The classic two-pointer match, back-tracking to the last `*` seen.
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < n.len() {
        match p.get(i) {
            Some('*') => {
                star = Some(i);
                mark = j;
                i += 1;
            }
            Some(&c) if c == '?' || Some(&c) == n.get(j) => {
                i += 1;
                j += 1;
            }
            _ => match star {
                Some(s) => {
                    i = s + 1;
                    mark += 1;
                    j = mark;
                }
                None => return false,
            },
        }
    }
    p.get(i..).is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}
