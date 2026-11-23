//! The launcher drawn as rows of cells (spec § 2, mockups A and C): the
//! modes row, the input line, the results, the help line; a preview panel
//! on the right when it has something to show; the frame is the painter's
//! (a pixel line, `list::Boxed`). Every row is a `Grid` written only
//! through `Grid::write` (controls become spaces, wide characters are never
//! cut): the painter copies it, tests snapshot it.
//!
//! The layout counts a ring of one blank cell all around (`BoxLayout::columns`
//! and `rows` include it, as the box-drawing frame once did): every number
//! below is in the ring's coordinates, and `render` drops the ring once
//! built (`list::boxed`).

use crate::compositor::Toplevel;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::icons;
use crate::launcher::emoji;
use crate::launcher::matching::{Mode, Source, app_for_window, command_path, exec_program};
use crate::launcher::model::{Launcher, Origin};
use crate::launcher::prefix::{self, Prefix, search_url};
use crate::launcher::tools::catalog::Choices;
use crate::launcher::tools::nav::{RowId, Trail};
use crate::list::{self, Boxed, Row, tail, wrap};
use crate::quick;
use alpheus_common::launcher_ipc::wipe;
use std::borrow::Cow;

pub const BOX_COLUMNS: u16 = 60;
pub const PREVIEW_COLUMNS: u16 = 36;
pub const MIN_COLUMNS: u16 = 24;
/// Glyph for $PATH commands (md-console, as in icons.rs).
pub const COMMAND_ICON: char = '\u{f018d}';
/// Glyph for the calculator row (md-calculator).
pub const CALC_ICON: char = '\u{f00ec}';
/// Glyph for the web search row (md-magnify).
pub const SEARCH_ICON: char = '\u{f0349}';
/// Glyph for a pinned history row (md-pin).
pub const PIN_ICON: char = '\u{f0403}';
/// The history mode's empty input line (a bare `"` in All lists everything instead).
const CLIPBOARD_PLACEHOLDER: &str = "type to filter the clipboard history";
const PROMPT: &str = "❯";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxLayout {
    pub columns: u16,
    pub rows: u16,
    /// Row of the first result.
    pub list_top: u16,
    pub list_rows: u16,
    /// 0 when no preview panel is reserved (a script menu, or an output too narrow).
    pub preview_columns: u16,
}

impl BoxLayout {
    /// First column after the results' pane: the divider when the preview
    /// shows, the right frame otherwise.
    fn pane_end(&self) -> u16 {
        if self.preview_columns > 0 { self.columns.saturating_sub(self.preview_columns + 2) } else { self.columns.saturating_sub(1) }
    }
}

/// Rows: top frame, modes row (`script`: not in a script menu, the calculator or the switcher), input line, `list_rows` results, help line, bottom frame.
/// columns = BOX_COLUMNS (+ PREVIEW_COLUMNS + 1 when `preview` and the output has room), never more than
/// output_columns - 2 nor less than MIN_COLUMNS (or the output width if smaller); list_rows =
/// min(config_rows, what the output height leaves), at least 1. `preview` reserves the panel (the host
/// passes `!script`, once per open); `render` always fills a reserved panel with the selected
/// result (2026-10-05 — there is no more toggle to gate it on).
pub fn layout(output_columns: u16, output_rows: u16, config_rows: u16, script: bool, preview: bool) -> BoxLayout {
    let room = output_columns.saturating_sub(2);
    let wide = BOX_COLUMNS + PREVIEW_COLUMNS + 1;
    let preview_columns = if preview && room >= wide { PREVIEW_COLUMNS } else { 0 };
    let wanted = if preview_columns > 0 { wide } else { BOX_COLUMNS };
    let columns = wanted.min(room).max(MIN_COLUMNS.min(output_columns));
    let chrome = if script { 4 } else { 5 };
    let list_rows = config_rows.min(output_rows.saturating_sub(chrome)).max(1);
    BoxLayout { columns, rows: chrome + list_rows, list_top: chrome - 2, list_rows, preview_columns }
}

/// The modes row's labels and their columns [start, end).
fn mode_spans() -> Vec<(Mode, u16, u16)> {
    let mut x = 1;
    Mode::CYCLE
        .iter()
        .map(|m| {
            let w = text_width(m.label()) + 2;
            let span = (*m, x, x + w);
            x += w;
            span
        })
        .collect()
}

/// A blank row (the ring's, or one to write into).
fn blank(b: &BoxLayout) -> Grid {
    Grid::empty(b.columns)
}

fn modes_row(l: &Launcher, b: &BoxLayout, g: &mut Grid) {
    let end = b.pane_end();
    for (m, start, _) in mode_spans() {
        let style = if m == l.mode { Style::ACTIVE } else { Style::MUTED };
        g.write(start, end, &format!(" {} ", m.label()), style);
    }
}

/// What `--password` draws for each character typed.
const MASK: char = '•';

fn input_row(l: &Launcher, b: &BoxLayout, g: &mut Grid) {
    let end = b.pane_end();
    let prompt = match (&l.prompt, l.is_script()) {
        (Some(p), true) => truncate(p, end.saturating_sub(1) / 2),
        // The Tools menu's prompt is its breadcrumb, cut from the left when too long.
        _ if l.mode == Mode::Tools => l.tools().map_or_else(|| PROMPT.to_string(), |t| tail(&t.breadcrumb(), end.saturating_sub(1) / 2)),
        _ => PROMPT.to_string(),
    };
    let mut x = g.write(2, end, &prompt, Style::BOLD_ACCENT);
    // fuzzel's prompts end with their own space ("Target: "): never a second one.
    if !prompt.ends_with(char::is_whitespace) {
        x = g.write(x, end, " ", Style::NORMAL);
    }
    // One cell kept for the cursor, one for the margin.
    let room = end.saturating_sub(x).saturating_sub(2);
    // `--password`: one dot per character, never the text itself.
    let shown = if l.password { tail(&MASK.to_string().repeat(l.query.chars().count()), room) } else { tail(&l.query, room) };
    x = g.write(x, end, &shown, Style::NORMAL);
    x = g.write(x, end, " ", Style::ACTIVE);
    if l.mode == Mode::Clipboard && l.query.is_empty() {
        g.write(x + 1, end, CLIPBOARD_PLACEHOLDER, Style::MUTED);
    }
}

/// A window's title, or its class when the title is empty — or made only of
/// zero-width characters (a lone bidi mark, a combining mark with nothing to combine with):
/// `text_width` sees that as nothing to show, where `str::is_empty`/`trim` would not.
fn window_title(w: &Toplevel) -> &str {
    if text_width(&w.title) == 0 { &w.class } else { &w.title }
}

/// (icon, name, kind) of a result.
fn describe(l: &Launcher, s: Source) -> (Option<char>, Cow<'_, str>, String) {
    let apps = l.catalog.as_ref().map(|c| &c.apps[..]).unwrap_or(&[]);
    let commands = l.catalog.as_ref().map(|c| &c.commands[..]).unwrap_or(&[]);
    match s {
        Source::App(i) => {
            let e = &apps[i];
            (Some(icons::glyph(&e.id)), e.name.clone().into(), e.generic_name.clone().unwrap_or_else(|| "application".into()))
        }
        Source::Window(i) => {
            let w = &l.windows[i];
            (Some(icons::glyph(&w.class)), window_title(w).into(), format!("window · {}", w.workspace))
        }
        Source::Command(i) => (Some(COMMAND_ICON), commands[i].as_str().into(), "command".into()),
        Source::Line(i) => (None, l.lines[i].as_str().into(), String::new()),
        Source::Calc => (Some(CALC_ICON), calc_text(l).into(), "calculator".into()),
        Source::Search => (Some(SEARCH_ICON), format!("Search the web: {}", l.prefixed().map_or("", |(_, t)| t)).into(), "web".into()),
        Source::Emoji(offset) => {
            let (emoji, name) = emoji::at(offset).unwrap_or_default();
            (None, format!("{emoji}  {name}").into(), "emoji".into())
        }
        Source::Shell => (Some(COMMAND_ICON), format!("Run in a shell: {}", l.prefixed().map_or("", |(_, t)| t)).into(), "shell".into()),
        Source::Clip(i) => match l.clips.get(i) {
            // Borrowed here; the list's `truncate` and the grid cells then make per-frame copies of
            // the visible rows' first lines that are freed unwiped — out of scope ("where practical",
            // clipboard spec § wiping): the model's own rows (`ClipRow`) are the ones wiped.
            Some(c) => (Some(if c.pinned { PIN_ICON } else { ' ' }), Cow::Borrowed(c.first.as_str()), more_lines(c.more_lines)),
            None => (None, Cow::Borrowed(""), String::new()),
        },
        Source::ClipClear => (None, "Clear history".into(), "pins kept".into()),
        // Reached only if the row vanished: `result_row` draws the real row.
        Source::Tool(i) => (None, l.tool_rows().get(i).map_or("", |r| r.text.as_str()).into(), String::new()),
        // Reached only if the entry vanished: `result_row` draws the real row.
        Source::Entry(i) => (None, l.entries.as_ref().and_then(|e| e.items.get(i)).map_or("", |e| e.entry.as_str()).into(), String::new()),
    }
}

/// A history row's dim tail: how many lines its first line leaves out.
fn more_lines(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "+1 line".into(),
        n => format!("+{n} lines"),
    }
}

/// The calculator row's text: `= 4 · 0x4 · 0b100`, or `error: division by zero`.
fn calc_text(l: &Launcher) -> String {
    match &l.calc {
        Some(Ok(a)) => a.shown(),
        Some(Err(e)) => format!("error: {e}"),
        None => String::new(),
    }
}

fn result_row(l: &Launcher, b: &BoxLayout, g: &mut Grid, index: usize) {
    let selected = index == l.selected;
    if let Source::Entry(i) = l.results[index].source
        && let Some(e) = l.entries.as_ref().and_then(|e| e.items.get(i))
    {
        entry_row(g, 1, b.pane_end(), &e.entry, e.path.as_deref(), selected);
        return;
    }
    if let Source::Tool(i) = l.results[index].source
        && l.tool_rows().get(i).is_some()
    {
        tool_row(l, b, g, i, selected);
        return;
    }
    let (icon, name, kind) = describe(l, l.results[index].source);
    if matches!(l.results[index].source, Source::ClipClear) && l.clear_question() {
        let row = Row { icon: None, icon_style: Style::ACCENT, text: "Clear? ⏎", right: "", style: Style::WARN, selected };
        list::row(g, 1, b.pane_end(), &row);
        return;
    }
    let failed = matches!(l.results[index].source, Source::Calc) && matches!(l.calc, Some(Err(_)));
    let row = Row {
        icon,
        icon_style: Style::ACCENT,
        text: &name,
        right: &kind,
        style: if failed { Style::ERROR } else { Style::NORMAL },
        selected,
    };
    list::row(g, 1, b.pane_end(), &row);
}

/// A Tools row (tools spec § 2): `<icon> <text>` and its trail at the right edge: ` ›` dim, the quick
/// panel's switch, or, in a flat search, the category dimmed after the entry (`entry_row`, no glyph).
/// A pending question replaces the text with `<name>? ⏎` in warn.
fn tool_row(l: &Launcher, b: &BoxLayout, g: &mut Grid, i: usize, selected: bool) {
    let (Some(t), Some(row)) = (l.tools(), l.tool_rows().get(i)) else { return };
    let end = b.pane_end();
    if t.asking(row.id) {
        let text = t.question_text(row);
        list::row(g, 1, end, &Row { icon: row.icon, icon_style: Style::ACCENT, text: &text, right: "", style: Style::WARN, selected });
        return;
    }
    match &row.trail {
        Trail::Category(category) => entry_row(g, 1, end, &row.text, (!category.is_empty()).then_some(category.as_str()), selected),
        Trail::None | Trail::Chevron | Trail::Switch(_) => {
            let right = if row.trail == Trail::Chevron { "›" } else { "" };
            // A switch is drawn over the row's right edge: the name is cut before it, ellipsis
            // included, with one blank cell between.
            let (switch, shown) = match row.trail {
                Trail::Switch(on) => {
                    let (text, style) = quick::switch(Some(on));
                    (
                        Some((text, style)),
                        truncate(&row.text, end.saturating_sub(text_width(text) + 3 + 2 * u16::from(row.icon.is_some()) + 1)),
                    )
                }
                _ => (None, row.text.clone()),
            };
            list::row(g, 1, end, &Row { icon: row.icon, icon_style: Style::ACCENT, text: &shown, right, style: Style::NORMAL, selected });
            if let Some((text, style)) = switch {
                g.write(end.saturating_sub(text_width(text) + 1), end, text, style);
            }
        }
    }
}

/// A command-source entry (sources spec § 3): the entry, then its path dimmed two cells after it,
/// cut to fit; no glyph, nothing right-aligned. Selected: SELECTED across the span, the path
/// included, as `list::row` draws its secondary text.
fn entry_row(g: &mut Grid, start: u16, end: u16, entry: &str, path: Option<&str>, selected: bool) {
    let pick = |s: Style| if selected { Style::SELECTED } else { s };
    if selected {
        g.write(start, end, &" ".repeat(usize::from(end.saturating_sub(start))), Style::SELECTED);
    }
    let stop = end.saturating_sub(1);
    let x = start + 1;
    let x = g.write(x, stop, &truncate(entry, stop.saturating_sub(x)), pick(Style::NORMAL));
    if let Some(path) = path
        && stop.saturating_sub(x) > 2
    {
        let x = g.write(x, stop, "  ", pick(Style::NORMAL));
        g.write(x, stop, &truncate(path, stop.saturating_sub(x)), pick(Style::MUTED));
    }
}

fn clips_help(l: &Launcher) -> &'static str {
    if l.clips.is_empty() { "esc close" } else { "enter copy · ctrl+enter pin · del forget · esc close" }
}

fn help_text(l: &Launcher) -> &'static str {
    match l.prefixed() {
        Some((Prefix::Search, _)) => return "enter search · esc close",
        Some((Prefix::Emoji, _)) if l.config.copy.is_empty() => return "esc close",
        Some((Prefix::Emoji, _)) => return "enter copy · ctrl+enter copy name · esc close",
        Some((Prefix::Shell, _)) => return "enter run · ctrl+enter run in terminal · esc close",
        Some((Prefix::Clipboard, _)) => return clips_help(l),
        None => {}
    }
    if l.is_switcher() {
        return "enter focus · tab next · esc close";
    }
    match l.mode {
        // ← does nothing at the root; Backspace goes back to All only in a Mod+Space open.
        Mode::Tools if l.tools().is_some_and(|t| t.at_root()) && l.origin == Origin::Apps => "⏎ open/run · ⌫ all · esc close",
        Mode::Tools if l.tools().is_some_and(|t| t.at_root()) => "⏎ open/run · esc close",
        Mode::Tools => "⏎ open/run · ← esc back",
        Mode::Script => "enter choose · esc cancel",
        Mode::Clipboard => clips_help(l),
        Mode::Calc if l.config.copy.is_empty() => "esc close",
        Mode::Calc => "enter copy · ctrl+enter copy expression · esc close",
        Mode::Commands => "enter terminal · ctrl+enter bg · tab mode · esc close",
        // No `.desktop` action to run on a window: Ctrl+Enter is just Enter again here
        // (`secondary`'s catch-all), so it is left out, like Script's line above.
        Mode::Windows => "enter focus · tab mode · esc close",
        Mode::All | Mode::Apps => "enter open · ctrl+enter action · tab mode · esc close",
    }
}

/// `wrap`, but kept to at most `budget` lines: the last kept line ends with "…" when more
/// would have followed. Used for the parts of the preview that can grow
/// without bound (a comment, a window title) so what comes after — the actions, the "enter"
/// hint — is never pushed off the panel and silently dropped.
fn capped_wrap(text: &str, width: u16, budget: usize) -> Vec<String> {
    let mut lines = wrap(text, width);
    if lines.len() > budget {
        lines.truncate(budget);
        if let Some(last) = lines.last_mut() {
            if !last.ends_with('…') {
                last.push('…');
            }
        }
    }
    lines
}

/// The preview's lines for the selected result, each with its style — always something
/// (2026-10-05): an app shows its name, generic name, description and binary even
/// without actions; a window its full title, its app, its workspace and what Enter does; a
/// command its name and path; a script line nothing (script mode reserves no panel, so
/// `render` never calls this for it). `budget` is how many rows the panel actually has: the
/// one part that can grow without bound (a comment, a window title) is capped so the fixed
/// part after it — the actions, the "enter" hint — is never silently pushed off.
fn preview_lines(l: &Launcher, width: u16, budget: usize) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    let Some(hit) = l.selected_hit() else { return out };
    match hit.source {
        Source::App(i) => {
            let Some(e) = l.catalog.as_ref().and_then(|c| c.apps.get(i)) else { return out };
            out.push((e.name.clone(), Style::BOLD_ACCENT));
            if let Some(g) = &e.generic_name {
                out.push((g.clone(), Style::NORMAL));
            }
            let mut tail = Vec::new();
            tail.push((String::new(), Style::NORMAL));
            if let Some(p) = exec_program(&e.exec) {
                tail.push((format!("exec: {p}"), Style::MUTED));
            }
            for a in &e.actions {
                tail.push((format!("· {}", a.name), Style::NORMAL));
            }
            if let Some(c) = &e.comment {
                out.push((String::new(), Style::NORMAL));
                let comment_budget = budget.saturating_sub(out.len() + tail.len());
                out.extend(capped_wrap(c, width, comment_budget).into_iter().map(|t| (t, Style::NORMAL)));
            }
            out.extend(tail);
        }
        Source::Window(i) => {
            let w = &l.windows[i];
            let app_name = l
                .catalog
                .as_ref()
                .and_then(|c| app_for_window(&c.apps, &w.class))
                .and_then(|a| l.catalog.as_ref().map(|c| c.apps[a].name.clone()));
            let tail = [
                (app_name.unwrap_or_else(|| w.class.clone()), Style::MUTED),
                (format!("window · workspace {}", w.workspace), Style::MUTED),
                (String::new(), Style::NORMAL),
                ("enter  focus".to_string(), Style::NORMAL),
            ];
            let title_budget = budget.saturating_sub(tail.len());
            out.extend(capped_wrap(window_title(w), width, title_budget).into_iter().map(|t| (t, Style::BOLD_ACCENT)));
            out.extend(tail);
        }
        Source::Command(i) => {
            if let Some(c) = l.catalog.as_ref().and_then(|c| c.commands.get(i)) {
                out.push((c.clone(), Style::BOLD_ACCENT));
                let path = command_path(&l.path_dirs, c).map(|p| p.display().to_string()).unwrap_or_else(|| "command on $PATH".into());
                out.push((path, Style::MUTED));
                // The real model (`Launcher::primary`/`secondary`, 2026-10-05): Enter holds
                // it open in the configured terminal; Ctrl+Enter runs it detached, no terminal.
                out.push(("enter  run in terminal".to_string(), Style::NORMAL));
                out.push(("ctrl+enter  run in background".to_string(), Style::NORMAL));
            }
        }
        Source::Entry(i) => {
            if let Some(e) = l.entries.as_ref().and_then(|e| e.items.get(i)) {
                out.push((e.entry.clone(), Style::BOLD_ACCENT));
                if let Some(p) = &e.path {
                    out.push((p.clone(), Style::MUTED));
                }
                out.push((String::new(), Style::NORMAL));
                out.push(("enter  run".to_string(), Style::NORMAL));
            }
        }
        Source::Search => {
            let text = l.prefixed().map_or("", |(_, t)| t);
            let tail = [(String::new(), Style::NORMAL), ("enter  search".to_string(), Style::NORMAL)];
            let room = budget.saturating_sub(tail.len());
            let shown = capped_wrap(text, width, room.div_ceil(2));
            let url_budget = room.saturating_sub(shown.len());
            out.extend(shown.into_iter().map(|t| (t, Style::BOLD_ACCENT)));
            out.extend(capped_wrap(&search_url(&l.config.search, text), width, url_budget).into_iter().map(|t| (t, Style::MUTED)));
            out.extend(tail);
        }
        Source::Emoji(offset) => {
            if let Some((emoji, name)) = emoji::at(offset) {
                out.extend(capped_wrap(name, width, budget.saturating_sub(4)).into_iter().map(|t| (t, Style::BOLD_ACCENT)));
                out.push((emoji.to_string(), Style::NORMAL));
                if !l.config.copy.is_empty() {
                    out.push((String::new(), Style::NORMAL));
                    out.push(("enter  copy".to_string(), Style::NORMAL));
                    out.push(("ctrl+enter  copy name".to_string(), Style::NORMAL));
                }
            }
        }
        Source::Clip(i) => {
            if let Some(c) = l.clips.get(i) {
                let tail = [
                    (String::new(), Style::NORMAL),
                    ("enter  copy".to_string(), Style::NORMAL),
                    ("ctrl+enter  pin".to_string(), Style::NORMAL),
                    ("delete  forget".to_string(), Style::NORMAL),
                ];
                let room = budget.saturating_sub(tail.len());
                out.extend(capped_wrap(&c.preview, width, room).into_iter().map(|t| (t, Style::BOLD_ACCENT)));
                out.extend(tail);
            }
        }
        Source::ClipClear => {
            out.push(("Forget every copy".to_string(), Style::BOLD_ACCENT));
            out.push(("pinned ones stay".to_string(), Style::MUTED));
            out.push((String::new(), Style::NORMAL));
            out.push(("enter  clear".to_string(), Style::NORMAL));
        }
        Source::Shell => {
            let text = l.prefixed().map_or("", |(_, t)| t);
            let tail = [
                (String::new(), Style::NORMAL),
                ("enter  run in background".to_string(), Style::NORMAL),
                ("ctrl+enter  run in terminal".to_string(), Style::NORMAL),
            ];
            let room = budget.saturating_sub(tail.len());
            out.extend(capped_wrap(text, width, room).into_iter().map(|t| (t, Style::BOLD_ACCENT)));
            out.extend(tail);
        }
        Source::Calc => {
            out.push((l.query.trim().to_string(), Style::BOLD_ACCENT));
            match &l.calc {
                Some(Ok(a)) => {
                    out.push((a.shown(), Style::NORMAL));
                    if !l.config.copy.is_empty() {
                        out.push((String::new(), Style::NORMAL));
                        out.push(("enter  copy".to_string(), Style::NORMAL));
                        out.push(("ctrl+enter  copy expression".to_string(), Style::NORMAL));
                    }
                }
                Some(Err(e)) => out.push((format!("error: {e}"), Style::ERROR)),
                None => {}
            }
        }
        Source::Tool(i) => {
            if let Some(row) = l.tool_rows().get(i) {
                out.extend(capped_wrap(&row.text, width, budget.saturating_sub(4)).into_iter().map(|t| (t, Style::BOLD_ACCENT)));
                if let Trail::Category(c) = &row.trail
                    && !c.is_empty()
                {
                    out.push((c.clone(), Style::MUTED));
                }
                out.push((String::new(), Style::NORMAL));
                let opens = match row.id {
                    RowId::Category(_) => true,
                    RowId::Entry(e) => {
                        l.tools().and_then(|t| t.catalog.entries.get(e)).is_some_and(|e| !matches!(e.choices, Choices::None))
                    }
                    RowId::Choice(..) => false,
                };
                out.push((if opens { "enter  open" } else { "enter  run" }.to_string(), Style::NORMAL));
            }
        }
        // Unreachable in practice: a `Source::Line` hit only exists in Script mode, where
        // `layout()` never reserves a preview panel (`preview` is always `!script`, see
        // `layout`'s doc comment) — this function is never even called with one. Kept empty,
        // not removed, for `Source`'s match exhaustiveness.
        Source::Line(_) => {}
    }
    out
}

/// The box's rows (ring dropped, the preview divider as a line). Selected row SELECTED; generic name/kind MUTED right-aligned
/// ("Web browser", "window · 2", "command"); empty results → "no match" (or "scanning…" while the
/// catalog is None and nothing matched); message on the help line in Style::ERROR.
// the empty-state texts: one row per mode, so a mode keeps its own line.
#[allow(clippy::match_same_arms)]
pub fn render(l: &Launcher, b: &BoxLayout) -> Boxed {
    let mut rows = vec![blank(b)];
    let end = b.pane_end();
    if l.has_modes() {
        let mut g = blank(b);
        modes_row(l, b, &mut g);
        rows.push(g);
    }
    let mut g = blank(b);
    input_row(l, b, &mut g);
    rows.push(g);
    for r in 0..usize::from(b.list_rows) {
        let mut g = blank(b);
        let index = l.top + r;
        if index < l.results.len() {
            result_row(l, b, &mut g, index);
        } else if r == 0 && l.results.is_empty() && !l.password {
            let text = match l.mode {
                _ if let Some((p, text)) = l.prefixed() => {
                    if text.is_empty() {
                        prefix::hint(p)
                    } else {
                        "no match"
                    }
                }
                Mode::Calc => "type an expression",
                Mode::Clipboard if l.query.trim().is_empty() => prefix::hint(Prefix::Clipboard),
                Mode::Clipboard => "no match",
                Mode::Tools if l.tools().is_some_and(|t| t.reading) => "reading…",
                Mode::Tools if l.query.trim().is_empty() => "nothing here",
                Mode::Tools => "no match",
                _ if l.is_switcher() => "no window open",
                _ if l.catalog.is_none() && !l.is_script() => "scanning…",
                _ => "no match",
            };
            g.write(2, end, text, Style::MUTED);
        }
        rows.push(g);
    }
    let mut g = blank(b);
    match &l.message {
        Some(m) => g.write(2, end.saturating_sub(1), &truncate(m, end.saturating_sub(3)), Style::ERROR),
        None => g.write(2, end.saturating_sub(1), &truncate(help_text(l), end.saturating_sub(3)), Style::MUTED),
    };
    rows.push(g);
    rows.push(blank(b));
    // The panel is reserved for the whole open (2026-10-05: the box never
    // changes width with the selection) and, once reserved, always shows the selected
    // result (2026-10-05, after seeing the real rendering: never blank while one
    // exists — `preview_lines` picks the content, `capped_wrap` keeps it inside `budget`).
    if b.preview_columns > 0 {
        let (start, stop) = (end + 2, b.columns.saturating_sub(2));
        let interior_rows = rows.len() - 2;
        let mut lines = preview_lines(l, stop.saturating_sub(start), interior_rows);
        // A safety net, not the common path (`capped_wrap` already fits the one part that can
        // grow without bound to `interior_rows`): a fixed tail longer than the panel itself —
        // many `.desktop` Actions at the smallest `rows` configuration — can still overflow it;
        // marks the cut on the last visible line rather than dropping it silently.
        let overflow = lines.len() > interior_rows;
        let last = rows.len() - 1;
        for (i, (row, (text, style))) in rows[1..last].iter_mut().zip(lines.iter_mut()).enumerate() {
            if overflow && i + 1 == interior_rows && !text.ends_with('…') {
                text.push('…');
            }
            let mut cut = truncate(text, stop.saturating_sub(start));
            row.write(start, stop, &cut, *style);
            wipe(&mut cut);
        }
        // The panel's lines may be a history row's text: overwritten before they are freed.
        for (text, _) in &mut lines {
            wipe(text);
        }
    }
    list::boxed(&rows, &[], (b.preview_columns > 0).then(|| b.pane_end()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    Result(usize),
    Mode(Mode),
    Inside,
}

/// Cell (column, row) inside the box → what it hits; None: outside the box.
pub fn hit(l: &Launcher, b: &BoxLayout, column: u16, row: u16) -> Option<Click> {
    if column >= b.columns || row >= b.rows {
        return None;
    }
    let in_pane = column >= 1 && column < b.pane_end();
    if in_pane && row >= b.list_top && row < b.list_top + b.list_rows {
        let index = l.top + usize::from(row - b.list_top);
        return Some(if index < l.results.len() { Click::Result(index) } else { Click::Inside });
    }
    if l.has_modes() && row == 1 {
        if let Some((m, _, _)) = mode_spans().into_iter().find(|(_, s, e)| (*s..*e).contains(&column) && *e <= b.pane_end()) {
            return Some(Click::Mode(m));
        }
    }
    Some(Click::Inside)
}

/// Snapshot text: each row's text, then each row's style codes (Grid::text, Grid::styles), then the
/// lines the painter draws (a divider column).
pub fn snapshot(b: &Boxed) -> String {
    let texts: Vec<String> = b.rows.iter().map(Grid::text).collect();
    let styles: Vec<String> = b.rows.iter().map(Grid::styles).collect();
    let divider = b.divider.map_or("none".to_string(), |c| c.to_string());
    format!("{}\n\n{}\n\ndivider: {divider}", texts.join("\n"), styles.join("\n"))
}

#[cfg(test)]
mod tests;
