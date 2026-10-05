//! The launcher drawn as rows of cells (spec § 2, mockups A and C): a thin
//! frame, the modes row, the input line, the results, the help line; a
//! preview panel on the right when it has something to show. Every row is
//! a `Grid` exactly `columns` wide, written only through `Grid::write`
//! (controls become spaces, wide characters are never cut): the painter
//! copies it, tests snapshot it.

use crate::compositor::Toplevel;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::icons;
use crate::launcher::matching::{Mode, Source, app_for_window, command_path, exec_program};
use crate::launcher::model::Launcher;

pub const BOX_COLUMNS: u16 = 60;
pub const PREVIEW_COLUMNS: u16 = 36;
pub const MIN_COLUMNS: u16 = 24;
/// Glyph for $PATH commands (md-console, as in icons.rs).
pub const COMMAND_ICON: char = '\u{f018d}';
/// Longest generic name or kind on the right of a result.
const KIND_MAX: u16 = 20;
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

/// Rows: top frame, modes row (not in Script), input line, `list_rows` results, help line, bottom frame.
/// columns = BOX_COLUMNS (+ PREVIEW_COLUMNS + 1 when `preview` and the output has room), never more than
/// output_columns - 2 nor less than MIN_COLUMNS (or the output width if smaller); list_rows =
/// min(config_rows, what the output height leaves), at least 1. `preview` reserves the panel (task 12
/// passes `!script`, once per open); `render` always fills a reserved panel with the selected
/// result (Mae, 2026-10-05 — there is no more toggle to gate it on).
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

fn frame(b: &BoxLayout, left: char, right: char, cross: char) -> Grid {
    let mut g = Grid::empty(b.columns);
    let line = "─".repeat(usize::from(b.columns));
    g.write(0, b.columns, &line, Style::MUTED);
    g.write(0, b.columns, &left.to_string(), Style::MUTED);
    if b.preview_columns > 0 {
        g.write(b.pane_end(), b.columns, &cross.to_string(), Style::MUTED);
    }
    g.write(b.columns.saturating_sub(1), b.columns, &right.to_string(), Style::MUTED);
    g
}

/// An interior row: side frames and the divider, the rest blank.
fn interior(b: &BoxLayout) -> Grid {
    let mut g = Grid::empty(b.columns);
    g.write(0, b.columns, "│", Style::MUTED);
    if b.preview_columns > 0 {
        g.write(b.pane_end(), b.columns, "│", Style::MUTED);
    }
    g.write(b.columns.saturating_sub(1), b.columns, "│", Style::MUTED);
    g
}

/// The end of `s` that fits in `max` cells, "…" first when cut: the
/// input line keeps showing what is being typed.
fn tail(s: &str, max: u16) -> String {
    if text_width(s) <= max {
        return s.to_string();
    }
    let reversed: String = s.chars().rev().collect();
    let cut = truncate(&reversed, max);
    // Exactly the one ellipsis `truncate` just appended (Review Focus 6): `s` may
    // legitimately end with its own "…", right where the cut falls — `trim_end_matches`
    // would eat that one too.
    let stripped = cut.strip_suffix('…').unwrap_or(&cut);
    let kept: String = stripped.chars().rev().collect();
    format!("…{kept}")
}

fn modes_row(l: &Launcher, b: &BoxLayout, g: &mut Grid) {
    let end = b.pane_end();
    for (m, start, _) in mode_spans() {
        let style = if m == l.mode { Style::INVERTED } else { Style::MUTED };
        g.write(start, end, &format!(" {} ", m.label()), style);
    }
}

fn input_row(l: &Launcher, b: &BoxLayout, g: &mut Grid) {
    let end = b.pane_end();
    let prompt = match (&l.prompt, l.is_script()) {
        (Some(p), true) => truncate(p, end.saturating_sub(1) / 2),
        _ => PROMPT.to_string(),
    };
    let mut x = g.write(2, end, &prompt, Style::BOLD_ACCENT);
    // fuzzel's prompts end with their own space ("Target: "): never a second one.
    if !prompt.ends_with(char::is_whitespace) {
        x = g.write(x, end, " ", Style::NORMAL);
    }
    // One cell kept for the cursor, one for the margin.
    let room = end.saturating_sub(x).saturating_sub(2);
    x = g.write(x, end, &tail(&l.query, room), Style::NORMAL);
    g.write(x, end, " ", Style::INVERTED);
}

/// A window's title, or its class when the title is empty — or, Review Focus 3, made only of
/// zero-width characters (a lone bidi mark, a combining mark with nothing to combine with):
/// `text_width` sees that as nothing to show, where `str::is_empty`/`trim` would not.
fn window_title(w: &Toplevel) -> &str {
    if text_width(&w.title) == 0 { &w.class } else { &w.title }
}

/// (icon, name, kind) of a result.
fn describe(l: &Launcher, s: Source) -> (Option<char>, String, String) {
    let apps = l.catalog.as_ref().map(|c| &c.apps[..]).unwrap_or(&[]);
    let commands = l.catalog.as_ref().map(|c| &c.commands[..]).unwrap_or(&[]);
    match s {
        Source::App(i) => {
            let e = &apps[i];
            (Some(icons::glyph(&e.id)), e.name.clone(), e.generic_name.clone().unwrap_or_else(|| "application".into()))
        }
        Source::Window(i) => {
            let w = &l.windows[i];
            (Some(icons::glyph(&w.class)), window_title(w).to_string(), format!("window · {}", w.workspace))
        }
        Source::Command(i) => (Some(COMMAND_ICON), commands[i].clone(), "command".into()),
        Source::Line(i) => (None, l.lines[i].clone(), String::new()),
    }
}

fn result_row(l: &Launcher, b: &BoxLayout, g: &mut Grid, index: usize) {
    let (start, end) = (1, b.pane_end());
    let selected = index == l.selected;
    let (icon, name, kind) = describe(l, l.results[index].source);
    let pick = |s: Style| if selected { Style::INVERTED } else { s };
    if selected {
        g.write(start, end, &" ".repeat(usize::from(end.saturating_sub(start))), Style::INVERTED);
    }
    let mut x = start + 1;
    if let Some(c) = icon {
        x = g.write(x, end, &c.to_string(), pick(Style::ACCENT));
        x = g.write(x, end, " ", pick(Style::NORMAL));
    }
    // Review Focus 5: at the minimum width, KIND_MAX alone can still reach past the icon
    // (whose space `x` already claimed) into the frame — clamped so the kind column never
    // starts to the left of where the icon ends.
    let max_kind = KIND_MAX.min(end.saturating_sub(x).saturating_sub(2));
    let kind = truncate(&kind, max_kind);
    let kind_width = text_width(&kind);
    let name_end = if kind_width > 0 { end.saturating_sub(kind_width + 2) } else { end.saturating_sub(1) };
    g.write(x, name_end, &truncate(&name, name_end.saturating_sub(x)), pick(Style::NORMAL));
    if kind_width > 0 {
        g.write(end.saturating_sub(kind_width + 1), end, &kind, pick(Style::MUTED));
    }
}

fn help_text(l: &Launcher) -> &'static str {
    match l.mode {
        Mode::Script => "enter choose · esc cancel",
        Mode::Commands => "enter run in terminal · ctrl+enter run in background · tab mode · esc close",
        _ => "enter open · ctrl+enter action · tab mode · esc close",
    }
}

/// Greedy word wrap to `width` cells, words too long cut with "…". A word that `split_whitespace`
/// handed us but that displays as nothing (a lone bidi mark between two spaces, Review Focus 4)
/// is skipped outright: kept, it would still occupy a word slot and force a separating space
/// around something invisible — a double space, or a "blank" line made only of such marks.
fn wrap(text: &str, width: u16) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word = truncate(word, width);
        if text_width(&word) == 0 {
            continue;
        }
        if !line.is_empty() && text_width(&line) + 1 + text_width(&word) > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// `wrap`, but kept to at most `budget` lines: the last kept line ends with "…" when more
/// would have followed (Review Focus 1). Used for the parts of the preview that can grow
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
/// (Mae, 2026-10-05): an app shows its name, generic name, description and binary even
/// without actions; a window its full title, its app, its workspace and what Enter does; a
/// command its name and path; a script line nothing (script mode reserves no panel, so
/// `render` never calls this for it). `budget` is how many rows the panel actually has: the
/// one part that can grow without bound (a comment, a window title) is capped so the fixed
/// part after it — the actions, the "enter" hint — is never silently pushed off (Review Focus 1).
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
                // The real model (`Launcher::primary`/`secondary`, Mae 2026-10-05): Enter holds
                // it open in the configured terminal; Ctrl+Enter runs it detached, no terminal.
                out.push(("enter  run in terminal".to_string(), Style::NORMAL));
                out.push(("ctrl+enter  run in background".to_string(), Style::NORMAL));
            }
        }
        Source::Line(i) => out.extend(wrap(&l.lines[i], width).into_iter().map(|t| (t, Style::NORMAL))),
    }
    out
}

/// One Grid per row, each `columns` wide. Selected row INVERTED; generic name/kind MUTED right-aligned
/// ("Web browser", "window · 2", "command"); empty results → "no match" (or "scanning…" while the
/// catalog is None and nothing matched); message on the help line in Style::ERROR.
pub fn render(l: &Launcher, b: &BoxLayout) -> Vec<Grid> {
    let mut rows = vec![frame(b, '╭', '╮', '┬')];
    let end = b.pane_end();
    if !l.is_script() {
        let mut g = interior(b);
        modes_row(l, b, &mut g);
        rows.push(g);
    }
    let mut g = interior(b);
    input_row(l, b, &mut g);
    rows.push(g);
    for r in 0..usize::from(b.list_rows) {
        let mut g = interior(b);
        let index = l.top + r;
        if index < l.results.len() {
            result_row(l, b, &mut g, index);
        } else if r == 0 && l.results.is_empty() {
            let text = if l.catalog.is_none() && !l.is_script() { "scanning…" } else { "no match" };
            g.write(2, end, text, Style::MUTED);
        }
        rows.push(g);
    }
    let mut g = interior(b);
    match &l.message {
        Some(m) => g.write(2, end.saturating_sub(1), &truncate(m, end.saturating_sub(3)), Style::ERROR),
        None => g.write(2, end.saturating_sub(1), &truncate(help_text(l), end.saturating_sub(3)), Style::MUTED),
    };
    rows.push(g);
    rows.push(frame(b, '╰', '╯', '┴'));
    // The panel is reserved for the whole open (task 12, Mae 2026-10-05: the box never
    // changes width with the selection) and, once reserved, always shows the selected
    // result (Mae, 2026-10-05, after seeing the real rendering: never blank while one
    // exists — `preview_lines` picks the content, `capped_wrap` keeps it inside `budget`).
    if b.preview_columns > 0 {
        let (start, stop) = (end + 2, b.columns.saturating_sub(2));
        let interior_rows = rows.len() - 2;
        let lines = preview_lines(l, stop.saturating_sub(start), interior_rows);
        // A safety net, not the common path (`capped_wrap` already fits the one part that can
        // grow without bound to `interior_rows`): a fixed tail longer than the panel itself —
        // many `.desktop` Actions at the smallest `rows` configuration — can still overflow it;
        // marks the cut on the last visible line rather than dropping it silently.
        let overflow = lines.len() > interior_rows;
        let last = rows.len() - 1;
        for (i, (row, (mut text, style))) in rows[1..last].iter_mut().zip(lines).enumerate() {
            if overflow && i + 1 == interior_rows && !text.ends_with('…') {
                text.push('…');
            }
            row.write(start, stop, &truncate(&text, stop.saturating_sub(start)), style);
        }
    }
    rows
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
    if !l.is_script() && row == 1 {
        if let Some((m, _, _)) = mode_spans().into_iter().find(|(_, s, e)| (*s..*e).contains(&column) && *e <= b.pane_end()) {
            return Some(Click::Mode(m));
        }
    }
    Some(Click::Inside)
}

/// Snapshot text: each row's text, then each row's style codes (Grid::text, Grid::styles).
pub fn snapshot(rows: &[Grid]) -> String {
    let texts: Vec<String> = rows.iter().map(Grid::text).collect();
    let styles: Vec<String> = rows.iter().map(Grid::styles).collect();
    format!("{}\n\n{}", texts.join("\n"), styles.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::Toplevel;
    use crate::launcher::config::LauncherConfig;
    use crate::launcher::desktop::{DesktopAction, DesktopEntry};
    use crate::launcher::history::History;
    use crate::launcher::input::Key;
    use crate::launcher::scan::Catalog;
    use std::path::PathBuf;

    const NOW: u64 = 1_800_000_000;

    fn app(id: &str, name: &str, generic: Option<&str>, exec: &str) -> DesktopEntry {
        DesktopEntry {
            id: id.into(),
            path: PathBuf::from(format!("/usr/share/applications/{id}")),
            name: name.into(),
            generic_name: generic.map(String::from),
            comment: None,
            keywords: vec![],
            icon: None,
            exec: exec.into(),
            terminal: false,
            startup_wm_class: None,
            actions: vec![],
        }
    }

    fn catalog() -> Catalog {
        let mut firefox = app("org.mozilla.firefox.desktop", "Firefox", Some("Web Browser"), "firefox %u");
        firefox.comment = Some("Browse the World Wide Web with a fast, private and free browser".into());
        firefox.actions = vec![
            DesktopAction { key: "new-window".into(), name: "New Window".into(), exec: "firefox --new-window %u".into() },
            DesktopAction {
                key: "new-private-window".into(),
                name: "New Private Window".into(),
                exec: "firefox --private-window %u".into(),
            },
        ];
        Catalog {
            apps: vec![
                firefox,
                app("org.gnome.Nautilus.desktop", "Files", Some("File Manager"), "nautilus --new-window"),
                app("Alacritty.desktop", "Alacritty", Some("Terminal"), "foot"),
                app("btop.desktop", "btop++", None, "btop"),
            ],
            commands: vec!["btop".into(), "htop".into(), "nvim".into()],
        }
    }

    fn windows() -> Vec<Toplevel> {
        let w = |selector: &str, class: &str, title: &str, workspace: &str| Toplevel {
            selector: selector.into(),
            class: class.into(),
            title: title.into(),
            workspace: workspace.into(),
            focused: false,
        };
        vec![w("id:1", "Alacritty", "~/projects — zsh", "3"), w("id:2", "firefox", "Web page — Mozilla Firefox", "2")]
    }

    fn launcher() -> Launcher {
        let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
        l.set_catalog(catalog());
        l
    }

    fn keys(l: &mut Launcher, ks: &[Key]) {
        for k in ks {
            l.key(*k);
        }
    }

    fn typed(l: &mut Launcher, text: &str) {
        for c in text.chars() {
            l.key(Key::Char(c));
        }
    }

    /// Lays out for an output of `columns` × 40 cells, like task 12 does:
    /// the preview panel reserved for the app launcher, whatever the selection.
    fn draw(l: &mut Launcher, columns: u16) -> (BoxLayout, Vec<Grid>) {
        let b = layout(columns, 40, l.config.rows, l.is_script(), !l.is_script());
        l.set_list_rows(usize::from(b.list_rows));
        let rows = render(l, &b);
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.width(), b.columns, "row {i} of {}", snapshot(&rows));
        }
        assert_eq!(rows.len(), usize::from(b.rows));
        (b, rows)
    }

    #[test]
    fn all_mode_lists_apps_then_windows() {
        let mut l = launcher();
        let (b, rows) = draw(&mut l, 100);
        // The window `for_apps` started on stays selected by identity once the catalog
        // arrives (Mae, 2026-10-05: apps now list before it), so the panel still fills with
        // its preview even though it is no longer the first row.
        assert_eq!((b.columns, b.preview_columns), (97, PREVIEW_COLUMNS), "the panel reserved, filled: the selected window's preview");
        insta::assert_snapshot!("all", snapshot(&rows));
    }

    #[test]
    fn apps_mode_with_the_preview() {
        let mut l = launcher();
        keys(&mut l, &[Key::Tab, Key::Down]); // Apps: Alacritty, btop++, Files, Firefox
        keys(&mut l, &[Key::Down, Key::Down]);
        let (b, rows) = draw(&mut l, 120);
        assert_eq!((b.columns, b.preview_columns), (97, PREVIEW_COLUMNS), "Firefox has actions: its preview fills the panel");
        insta::assert_snapshot!("apps_preview", snapshot(&rows));
    }

    #[test]
    fn windows_mode() {
        let mut l = launcher();
        keys(&mut l, &[Key::Tab, Key::Tab]);
        let (_, rows) = draw(&mut l, 100);
        insta::assert_snapshot!("windows", snapshot(&rows));
    }

    #[test]
    fn commands_mode_entered_by_its_prefix() {
        let mut l = launcher();
        typed(&mut l, ">top");
        let (_, rows) = draw(&mut l, 100);
        insta::assert_snapshot!("commands", snapshot(&rows));
    }

    #[test]
    fn script_mode_with_and_without_prompt() {
        let lines = vec!["Scan rapide".to_string(), "Scan complet".to_string(), "Ports".to_string()];
        let mut l = Launcher::for_script(LauncherConfig::default(), Some("nmap: ".into()), lines.clone());
        typed(&mut l, "scan");
        let (b, rows) = draw(&mut l, 100);
        assert_eq!(b.list_top, 2, "no modes row");
        insta::assert_snapshot!("script_prompt", snapshot(&rows));
        let mut l = Launcher::for_script(LauncherConfig::default(), None, lines);
        let (_, rows) = draw(&mut l, 100);
        insta::assert_snapshot!("script", snapshot(&rows));
    }

    /// Helper shared with other tests: the preview panel's text, one entry per interior row.
    fn panel(rows: &[Grid], b: &BoxLayout) -> Vec<String> {
        let start = usize::from(b.columns - b.preview_columns - 1);
        rows[1..rows.len() - 1].iter().map(|r| r.text().chars().skip(start).take(usize::from(b.preview_columns)).collect()).collect()
    }

    /// Mae, 2026-10-05, after seeing the real rendering: the box never changes width as the
    /// selection moves, and the panel — once reserved — always shows the selected result; it
    /// is never blank while one exists. `Left`/`Right` (the model's own fix round removed the
    /// toggle they used to drive, along with `Ctrl+Space`) are still recognised keys, now
    /// simply ignored — pressing them changes nothing here either.
    #[test]
    fn the_panel_always_shows_the_selected_result() {
        let blank = " ".repeat(usize::from(PREVIEW_COLUMNS));
        let mut l = launcher();
        let (b, rows) = draw(&mut l, 120);
        assert!(panel(&rows, &b).iter().any(|t| t.contains("~/projects")), "a window selected: its title\n{}", snapshot(&rows));
        assert!(!panel(&rows, &b).iter().all(|t| *t == blank), "never blank while a result is selected");
        keys(&mut l, &[Key::Tab, Key::End]); // Apps: Firefox, with actions
        let (b2, rows) = draw(&mut l, 120);
        assert_eq!(b2, b, "the same box");
        assert!(panel(&rows, &b).iter().any(|t| t.contains("New Private Window")), "{}", snapshot(&rows));
        keys(&mut l, &[Key::Up]); // Files, no actions: still shows its own info
        let (b3, rows) = draw(&mut l, 120);
        assert_eq!(b3, b, "never narrower");
        assert!(panel(&rows, &b).iter().any(|t| t.contains("File Manager")), "no actions: still filled\n{}", snapshot(&rows));
        l.key(Key::Left); // ignored now (it used to hide the preview)
        let (b4, rows) = draw(&mut l, 120);
        assert_eq!(b4, b, "still the same box");
        assert!(panel(&rows, &b).iter().any(|t| t.contains("File Manager")), "Left no longer blanks it\n{}", snapshot(&rows));
    }

    /// Review Focus: via the ranker's window → app link (`matching::app_for_window`) when the
    /// window's class names a known app, or the class itself otherwise.
    #[test]
    fn window_preview_names_its_app_via_the_class_link_or_the_class_itself() {
        // Scoped to the preview panel itself (`panel`, not the whole box): the result list on
        // the left also shows app names, which would make a substring check on the whole
        // snapshot pass even if the preview picked the wrong text entirely.
        let mut l = launcher();
        let (b, rows) = draw(&mut l, 120); // the Alacritty window: its class is also an app's id
        let text = panel(&rows, &b);
        assert!(text.iter().any(|t| t.contains("~/projects — zsh")), "the full title\n{}", snapshot(&rows));
        assert!(text.iter().any(|t| t.contains("Alacritty")), "linked to the Alacritty app by class\n{}", snapshot(&rows));
        l.key(Key::Down); // the firefox window: class "firefox" links to the Firefox app
        let (b, rows) = draw(&mut l, 120);
        let text = panel(&rows, &b);
        assert!(text.iter().any(|t| t.contains("Firefox")), "its app's name, not the raw class \"firefox\"\n{}", snapshot(&rows));
        assert!(!text.iter().any(|t| t.trim() == "firefox"), "not the raw lower-case class\n{}", snapshot(&rows));
        let mut w = windows();
        w.push(Toplevel {
            selector: "id:9".into(),
            class: "unknown-app".into(),
            title: "Mystery".into(),
            workspace: "9".into(),
            focused: false,
        });
        let mut l2 = Launcher::for_apps(LauncherConfig::default(), w, History::empty(200), NOW);
        l2.set_catalog(catalog());
        keys(&mut l2, &[Key::Down, Key::Down]); // the third window: "Mystery", linked to no app
        let (b, rows) = draw(&mut l2, 120);
        let text = panel(&rows, &b);
        assert!(text.iter().any(|t| t.contains("unknown-app")), "no linked app: the class itself\n{}", snapshot(&rows));
    }

    /// Mae, 2026-10-05: "its description and binary" even without a `.desktop` Action.
    #[test]
    fn apps_mode_preview_without_actions_still_shows_description_and_binary() {
        let mut c = catalog();
        c.apps[1].comment = Some("Browse and manage the files on this computer".into()); // Files: no actions
        let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
        l.set_catalog(c);
        keys(&mut l, &[Key::Tab, Key::Down, Key::Down]); // Apps: Alacritty, btop++, Files
        assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::App(1)), "Files");
        let (_, rows) = draw(&mut l, 120);
        insta::assert_snapshot!("apps_preview_no_actions", snapshot(&rows));
    }

    /// Review Focus 1: the comment is capped, not the actions — a long comment loses its own
    /// tail (ending with "…") rather than pushing "exec:" and the actions off the panel.
    #[test]
    fn a_long_comment_is_capped_so_the_actions_stay_visible() {
        let mut c = catalog();
        c.apps[0].comment = Some("word ".repeat(60)); // Firefox: many more wrapped lines than the panel has rows
        let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
        l.set_catalog(c);
        keys(&mut l, &[Key::Tab, Key::End]); // Apps: Firefox (last, alphabetically)
        assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::App(0)));
        let (b, rows) = draw(&mut l, 120);
        let text = panel(&rows, &b);
        assert!(text.iter().any(|t| t.contains("New Private Window")), "the action stays visible\n{}", snapshot(&rows));
        assert!(text.iter().any(|t| t.contains("exec: firefox")), "exec stays visible too\n{}", snapshot(&rows));
        assert!(text.iter().any(|t| t.trim_end().ends_with('…')), "the comment's cut is marked\n{}", snapshot(&rows));
    }

    /// Review Focus 1, the render-level safety net: `capped_wrap` only bounds the one part of
    /// the preview that can grow without limit (a comment, a title); a *fixed* tail longer
    /// than the panel itself — many `.desktop` Actions at the smallest `rows` configuration —
    /// is not something it caps. `render`'s own check must still mark that cut, not drop it
    /// silently.
    #[test]
    fn an_overflowing_fixed_tail_is_marked_not_dropped_silently() {
        let mut c = catalog();
        c.apps[0].actions =
            (0..10).map(|i| DesktopAction { key: format!("a{i}"), name: format!("Action {i}"), exec: "x".into() }).collect();
        // interior_rows at its minimum (4): far fewer than the 10 actions alone.
        let config = LauncherConfig { rows: 1, ..LauncherConfig::default() };
        let mut l = Launcher::for_apps(config, windows(), History::empty(200), NOW);
        l.set_catalog(c);
        keys(&mut l, &[Key::Tab, Key::End]); // Apps: Firefox (last, alphabetically), now 10 actions
        assert_eq!(l.selected_hit().map(|h| h.source), Some(Source::App(0)));
        let (b, rows) = draw(&mut l, 120);
        let text = panel(&rows, &b);
        assert!(text.iter().any(|t| t.trim_end().ends_with('…')), "the overflow is marked\n{}", snapshot(&rows));
    }

    /// Review Focus 5: at the minimum width, a long kind must not overwrite the icon.
    #[test]
    fn the_kind_column_never_overwrites_the_icon_at_minimum_width() {
        let mut w = windows();
        w[0].workspace = "many-many-many-characters".into(); // "window · many-many-…" would reach past the icon
        let mut l = Launcher::for_apps(LauncherConfig::default(), w, History::empty(200), NOW);
        l.set_catalog(catalog());
        let (b, rows) = draw(&mut l, MIN_COLUMNS);
        let row = &rows[usize::from(b.list_top)];
        let icon = icons::glyph(&l.windows[0].class);
        assert_eq!(row.text().chars().nth(2), Some(icon), "the icon must survive the kind column: {:?}", row.text());
    }

    /// Review Focus 3: a title made only of zero-width characters (bidi marks here) is as
    /// empty as `""` would be — it must fall back to the class, like `describe` already does
    /// for a truly empty title.
    #[test]
    fn a_title_made_only_of_zero_width_characters_falls_back_to_the_class() {
        let mut w = windows();
        w[0].title = "\u{200e}\u{200f}\u{2066}\u{2069}".into();
        let mut l = Launcher::for_apps(LauncherConfig::default(), w, History::empty(200), NOW);
        l.set_catalog(catalog());
        let (b, rows) = draw(&mut l, 100);
        let row = &rows[usize::from(b.list_top)];
        assert!(row.text().contains("Alacritty"), "falls back to the class: {:?}", row.text());
    }

    /// Review Focus 6: `s`'s own trailing ellipsis, right at the cut point, must survive —
    /// only the one `tail` itself adds should ever be stripped.
    #[test]
    fn tail_strips_exactly_one_ellipsis_not_the_users_own() {
        let s = format!("{}…AAA", "X".repeat(10));
        assert_eq!(tail(&s, 5), "……AAA");
    }

    /// Review Focus 4: a "word" that displays as nothing must not force a space around it,
    /// nor stand as a line on its own.
    #[test]
    fn wrap_skips_zero_width_words() {
        assert_eq!(wrap("foo \u{200e} bar", 10), vec!["foo bar".to_string()]);
        assert_eq!(wrap("\u{200e} \u{200f}", 10), Vec::<String>::new(), "only invisible words: no blank line");
    }

    #[test]
    fn commands_mode_preview_shows_the_full_path_when_known() {
        let dir = crate::launcher::testing::TempDir::new("view-command-path");
        std::fs::write(dir.join("btop"), b"").unwrap();
        let mut l = launcher();
        l.path_dirs = vec![dir.to_path_buf()];
        typed(&mut l, ">top");
        let (_, rows) = draw(&mut l, 100);
        let text = snapshot(&rows);
        let expected = dir.join("btop").display().to_string();
        assert!(!text.contains("command on $PATH"), "a real path was resolved\n{text}");
        // The panel is only ~34 cells wide: a long $TMPDIR path is truncated, but its
        // prefix (well under that width) must still appear verbatim.
        assert!(text.contains(&expected[..20.min(expected.len())]), "{text}\nexpected a prefix of {expected:?}");
    }

    #[test]
    fn narrow_output_drops_the_preview_and_truncates() {
        let mut l = launcher();
        keys(&mut l, &[Key::Tab, Key::End]); // a result selected: the panel would fill if there were room
        let (b, rows) = draw(&mut l, 40);
        assert_eq!((b.columns, b.preview_columns), (38, 0));
        insta::assert_snapshot!("narrow", snapshot(&rows));
    }

    #[test]
    fn no_match_scanning_and_a_message() {
        let mut l = launcher();
        typed(&mut l, "zzzz");
        let (_, rows) = draw(&mut l, 100);
        insta::assert_snapshot!("no_match", snapshot(&rows));
        let mut l = Launcher::for_apps(LauncherConfig::default(), windows(), History::empty(200), NOW);
        typed(&mut l, "zzzz");
        let (_, rows) = draw(&mut l, 100);
        assert!(rows[3].text().contains("scanning…"), "{}", snapshot(&rows));
        let mut l = launcher();
        l.show_message("firefox: program not found".into());
        let (_, rows) = draw(&mut l, 100);
        insta::assert_snapshot!("message", snapshot(&rows));
    }

    #[test]
    fn hostile_names_never_misalign_the_box() {
        let hostile = [
            "x".repeat(300),
            "tab\there\nnewline\u{1b}[31m escape".to_string(),
            "\u{202e}reversed\u{200e} bidi \u{2066}isolate\u{2069}".to_string(),
            "日本語のアプリケーション名前がとても長い場合".to_string(),
            "🦀🦀🦀 crab 🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀".to_string(),
            "\u{2028}line\u{2029}para".to_string(),
        ];
        let mut c = catalog();
        for (i, h) in hostile.iter().enumerate() {
            let mut e = app(&format!("h{i}.desktop"), h, Some(h), h);
            e.comment = Some(h.clone());
            e.actions = vec![DesktopAction { key: "a".into(), name: h.clone(), exec: "x".into() }];
            c.apps.push(e);
            c.commands.push(h.clone());
        }
        let mut w = windows();
        for h in &hostile {
            w.push(Toplevel { selector: "id:9".into(), class: h.clone(), title: h.clone(), workspace: h.clone(), focused: false });
        }
        let lines = hostile.to_vec();
        for columns in [24u16, 40, 61, 99, 200] {
            for mode_keys in [&[][..], &[Key::Tab][..], &[Key::Tab, Key::Tab][..], &[Key::BackTab][..]] {
                let mut l = Launcher::for_apps(LauncherConfig::default(), w.clone(), History::empty(200), NOW);
                l.set_catalog(c.clone());
                keys(&mut l, mode_keys);
                for _ in 0..l.results.len() {
                    check(&mut l, columns);
                    l.key(Key::Down);
                }
            }
            let mut s = Launcher::for_script(LauncherConfig::default(), Some(hostile[1].clone()), lines.clone());
            typed(&mut s, &hostile[0]);
            check(&mut s, columns);
            s.key(Key::CtrlU);
            check(&mut s, columns);
        }

        fn check(l: &mut Launcher, columns: u16) {
            let (b, rows) = draw(l, columns);
            for r in &rows {
                assert_eq!(r.width(), b.columns);
                assert!(r.cells().iter().all(|c| !c.ch.is_control() || c.ch == crate::grid::CONT), "{:?}", r.text());
                assert!(!r.text().contains(['\u{2028}', '\u{2029}']), "{:?}", r.text());
                let t = r.text();
                assert!(t.starts_with(['│', '╭', '╰']) && t.ends_with(['│', '╮', '╯']), "frame broken at {columns}: {t:?}");
            }
            // Review Focus 2: every field this fixture makes hostile (window class, title and
            // workspace; command names; action names; app name, generic name and comment; the
            // dmenu prompt and its lines) is covered right here, not only by the six now-redundant
            // single-field tests this replaces.
            assert_no_raw_hostile_characters(&rows);
        }
    }

    #[test]
    fn layout_on_tiny_and_huge_outputs() {
        assert_eq!(layout(200, 60, 10, false, false), BoxLayout { columns: 60, rows: 15, list_top: 3, list_rows: 10, preview_columns: 0 });
        assert_eq!(layout(200, 60, 10, true, false), BoxLayout { columns: 60, rows: 14, list_top: 2, list_rows: 10, preview_columns: 0 });
        assert_eq!(layout(200, 60, 10, false, true).columns, 97, "60 + 36 + 1");
        assert_eq!(layout(99, 60, 10, false, true).preview_columns, PREVIEW_COLUMNS, "97 + 2 just fits");
        assert_eq!(layout(98, 60, 10, false, true), BoxLayout { columns: 60, rows: 15, list_top: 3, list_rows: 10, preview_columns: 0 });
        assert_eq!(layout(40, 60, 10, false, false).columns, 38, "output - 2");
        assert_eq!(layout(25, 60, 10, false, false).columns, 24, "never under MIN_COLUMNS");
        assert_eq!(layout(10, 60, 10, false, false).columns, 10, "unless the output is smaller");
        assert_eq!(layout(0, 0, 10, false, false), BoxLayout { columns: 0, rows: 6, list_top: 3, list_rows: 1, preview_columns: 0 });
        assert_eq!(layout(200, 12, 10, false, false).list_rows, 7, "the height leaves 7 rows");
        assert_eq!(layout(200, 3, 30, false, false).list_rows, 1, "at least one");
        assert_eq!(layout(u16::MAX, u16::MAX, 30, false, true).columns, 97);
        let mut l = launcher();
        for (c, r) in [(0u16, 0u16), (1, 1), (10, 3), (u16::MAX, u16::MAX)] {
            let b = layout(c, r, 10, false, true);
            l.set_list_rows(usize::from(b.list_rows));
            assert!(render(&l, &b).iter().all(|g| g.width() == b.columns), "{c}×{r}");
        }
    }

    #[test]
    fn hit_finds_results_modes_and_the_outside() {
        let mut l = launcher();
        let (b, _) = draw(&mut l, 100);
        assert_eq!(hit(&l, &b, 5, b.list_top), Some(Click::Result(0)));
        assert_eq!(hit(&l, &b, 58, b.list_top + 5), Some(Click::Result(5)), "last cell of the pane");
        assert_eq!(hit(&l, &b, 5, b.list_top + 6), Some(Click::Inside), "below the last result");
        assert_eq!(hit(&l, &b, 0, b.list_top), Some(Click::Inside), "the frame");
        assert_eq!(hit(&l, &b, 1, 1), Some(Click::Mode(Mode::All)));
        assert_eq!(hit(&l, &b, 6, 1), Some(Click::Mode(Mode::Apps)));
        assert_eq!(hit(&l, &b, 12, 1), Some(Click::Mode(Mode::Windows)));
        assert_eq!(hit(&l, &b, 25, 1), Some(Click::Mode(Mode::Commands)));
        assert_eq!(hit(&l, &b, 40, 1), Some(Click::Inside), "after the labels");
        assert_eq!(hit(&l, &b, 10, 2), Some(Click::Inside), "the input line");
        assert_eq!(hit(&l, &b, b.columns, 3), None, "right of the box");
        assert_eq!(hit(&l, &b, 3, b.rows), None, "below the box");
        l.key(Key::PageDown);
        l.set_list_rows(2);
        assert_eq!(hit(&l, &b, 5, b.list_top), Some(Click::Result(l.top)), "absolute index once scrolled");
        let mut s = Launcher::for_script(LauncherConfig::default(), None, vec!["a".into()]);
        let (b, _) = draw(&mut s, 100);
        assert_eq!(hit(&s, &b, 3, 1), Some(Click::Inside), "script: row 1 is the input line, no modes");
        assert_eq!(hit(&s, &b, 3, 2), Some(Click::Result(0)));
    }

    /// Carried requirement (earlier review rounds): every string the view shows must have its
    /// control characters (`\n`, `\r`, `\t`, other C0/C1) and bidi override marks neutralised
    /// before a row is shaped — never shown literally, never breaking the row's width. The
    /// mechanism lives in `Grid::write` (controls become spaces; zero-width marks, including
    /// bidi overrides, are dropped). Review Focus 2: `hostile_names_never_misalign_the_box`'s
    /// `check` now asserts this too, for every field its fixture makes hostile at once (window
    /// class/title/workspace, command names, action names, app name/generic name/comment, the
    /// dmenu prompt and its lines) — so the six single-field tests this helper used to serve
    /// are redundant and were folded into that one combinatorial test.
    fn assert_no_raw_hostile_characters(rows: &[Grid]) {
        const HOSTILE: [char; 6] = ['\n', '\r', '\t', '\u{202e}', '\u{200e}', '\u{2066}'];
        for r in rows {
            let t = r.text();
            assert!(!t.contains(HOSTILE), "a control character or bidi override leaked into a row: {t:?}");
        }
    }
}
