//! The resources popup's pure half: the rows it draws, the graphs' series, and its keys.
//!
//! This is the popup that opens from the bar's resources module. It is built from
//! `system::Resources`, which holds the latest sample the bar read and a ring of the last 60
//! (`History`), written on every tick of the bar's resources timer; the code that reads `/proc` and
//! `/sys` to make those samples is `sources/resources.rs`. This file only reads them: a
//! `ResourcesPopup` draws one block per item of `[resources] show`, and one per `disk_mounts` path
//! for `disk` (`blocks`: its icon, name and current figure, then a graph two rows tall), a header with the span, and a `Monitor…` row, and answers
//! keys and the pointer with a `ResourcesAction`. `series` turns an item's history into the
//! graphs' points in permille, and `render` returns a `Boxed` that also carries the graphs, which
//! the painter draws in pixels. The other half, `app.rs`, opens the popup, repaints it on each
//! tick while it is open, and runs the `Monitor…` command.
//!
//! Never:
//! - read a file or a clock, or start a thread or a command (a guardrail checks the thread and the
//!   command, none checks the file or the clock): everything is handed in as arguments;
//! - panic (a guardrail checks).
//!
//! See: docs/features/resources.md, and docs/decisions.md ("The resources history is kept always, as whole samples",
//! "The resources popup spans 60 times the interval", "Every resources item keeps its block in the popup",
//! "Each resources graph has a set scale; the network's follows its peak, with a floor",
//! "The bar shows the fullest disk, the popup one block per path").

use crate::actions::Button;
use crate::config::{ResourceItem, ResourcesConfig};
use crate::grid::{Grid, Hue, Style, Tint, char_width, text_width};
use crate::keys::Key;
use crate::list::{self, Boxed, Graph, Row, Series};
use crate::popup::PopupPointer;
use crate::segments::{CPU, DOWNLOAD, GPU, HARDDISK, HOT, MEMORY, THERMOMETER, UPLOAD, icon_style, rate};
use crate::system::{DISKS, HISTORY, History, Resources, Sample};
use alpheus_common::theme::settings::ModuleColors;
use std::path::Path;

/// A row's cells, its pad cell at each end included: 40 cells; the column positions
/// inside them are this file's own.
pub const WIDTH: u16 = 40;
/// Rows each graph takes, under its figure's row.
pub const GRAPH_ROWS: u16 = 2;
/// The network graph's lowest top, in bytes per second (128 KiB/s): an idle link does not fill the
/// graph with noise. See docs/decisions.md, "Each resources graph has a set scale; the network's
/// follows its peak, with a floor".
pub const NET_FLOOR: u64 = 128 * 1024;

/// What a key or the pointer asks of the wiring (`app.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcesAction {
    /// The popup did not change.
    Nothing,
    /// The popup changed: draw it again.
    Redraw,
    /// Run `[actions] resources`, then close.
    Monitor,
    /// Escape: close the popup.
    Close,
}

/// The open popup's own state: whether `Monitor…` is selected, and whether it is there at all
/// (`[actions] resources` empty: no row, no separator, nothing to select or run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcesPopup {
    pub selected: bool,
    pub monitor: bool,
}

/// A popup with the `Monitor…` row, which is what the default `[actions] resources` gives.
impl Default for ResourcesPopup {
    fn default() -> Self {
        Self::new(true)
    }
}

/// Rows one item's block takes: its figure's row and its graph.
const BLOCK: usize = 1 + GRAPH_ROWS as usize;

/// A value as permille of `top` (parts in a thousand), rounded down, at most 1000; 0 when `top` is
/// 0. The product is worked out in `u128` so that a rate near `u64::MAX` cannot overflow.
fn permille(v: u64, top: u64) -> u16 {
    if top == 0 {
        return 0;
    }
    (u128::from(v) * 1000 / u128::from(top)).min(1000) as u16
}

/// One item's points in permille, oldest first, always `HISTORY` long: a ring not yet full leaves
/// leading gaps, so the newest point stays at the right edge (a fixed time axis). Net gives two
/// series, download then upload; the others one. A sample with no reading is a gap: the pass is
/// kept, so the time axis never shifts.
pub fn series(item: ResourceItem, index: usize, h: &History) -> Vec<Vec<Option<u16>>> {
    let top = net_top(h).unwrap_or(NET_FLOOR);
    // The GPU graph plots usage as soon as one sample of the ring has it, else the degrees.
    let gpu_usage = item == ResourceItem::Gpu && h.iter().any(|s| s.gpu_busy.is_some());
    let count = if item == ResourceItem::Net { 2 } else { 1 };
    let mut out = vec![vec![None; HISTORY - h.len()]; count];
    for s in h.iter() {
        // Two slots for every item, the second used by the network (its upload) alone. Each
        // scale is described in docs/decisions.md, "Each resources graph has a set scale; the
        // network's follows its peak, with a floor".
        let points: [Option<u16>; 2] = match item {
            ResourceItem::Cpu => [s.cpu.map(|p| permille(u64::from(p), 100)), None],
            ResourceItem::Disk => [disk_of(s, index).map(|p| permille(u64::from(p), 100)), None],
            ResourceItem::Memory => {
                // No total, or a total of 0, gives no point rather than a division by zero.
                let point = match (s.memory, s.memory_total) {
                    (Some(m), Some(t)) if t > 0 => Some(permille(u64::from(m), u64::from(t))),
                    _ => None,
                };
                [point, None]
            }
            ResourceItem::Temperature => [s.temperature.map(degrees), None],
            ResourceItem::Gpu if gpu_usage => [s.gpu_busy.map(|b| permille(u64::from(b.percent), 100)), None],
            ResourceItem::Gpu => [s.gpu.map(degrees), None],
            ResourceItem::Net => match s.net {
                Some(n) => [Some(permille(n.down, top)), Some(permille(n.up, top))],
                None => [None, None],
            },
        };
        // The first slot goes to the first series, the second to the second (a one-series item
        // has no second column, so its `None` is dropped by `zip`).
        for (column, p) in out.iter_mut().zip(points) {
            column.push(p);
        }
    }
    out
}

/// The `index`th disk's figure; None past `DISKS`.
fn disk_of(s: &Sample, index: usize) -> Option<u8> {
    s.disks.get(index).copied().flatten()
}

/// The popup's blocks in order: one per item of `show`, except `disk`, which gives one per
/// `disk_mounts` path (its index into `Sample::disks`); every other item, and `disk` with no list,
/// has index 0. `render`, its graphs and `monitor_row` all count these, so they never drift.
pub fn blocks(c: &ResourcesConfig) -> Vec<(ResourceItem, usize)> {
    let mut out = Vec::with_capacity(c.show.len() + DISKS);
    for &item in &c.show {
        match item {
            ResourceItem::Disk if !c.disk_mounts.is_empty() => out.extend((0..c.disk_mounts.len().min(DISKS)).map(|i| (item, i))),
            _ => out.push((item, 0)),
        }
    }
    out
}

/// `path` cut to `width` cells from the left, `…` first when cut, so the end of the path (the part
/// that tells two mounts apart) stays.
pub fn mount_label(path: &Path, width: u16) -> String {
    let text = path.to_string_lossy();
    if text_width(&text) <= width {
        return text.into_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut kept = 0u16;
    let mut start = text.len();
    for (i, ch) in text.char_indices().rev() {
        let w = char_width(ch);
        if kept.saturating_add(w) > width - 1 {
            break;
        }
        kept += w;
        start = i;
    }
    format!("…{}", &text[start..])
}

/// °C on a 0 to 100 scale, in permille: a reading below 0 or above 100 is clamped.
fn degrees(t: i16) -> u16 {
    permille(t.clamp(0, 100) as u64, 100)
}

/// The network graph's top: the highest download or upload in the ring, never below `NET_FLOOR`;
/// None while no sample has a net reading.
pub fn net_top(h: &History) -> Option<u64> {
    let mut highest: Option<u64> = None;
    for s in h.iter() {
        let Some(n) = s.net else { continue };
        let rate = n.down.max(n.up);
        highest = match highest {
            Some(so_far) if so_far >= rate => Some(so_far),
            _ => Some(rate),
        };
    }
    // `?` ends the function with None when no sample had a net reading.
    let top = highest?;
    Some(top.max(NET_FLOOR))
}

/// The item's icon, hue and name: the hues are the bar's own (cpu orange, memory yellow,
/// temperature red, net cyan, disk blue, gpu purple).
fn look(item: ResourceItem) -> (char, Hue, &'static str) {
    match item {
        ResourceItem::Cpu => (CPU, Hue::Orange, "CPU"),
        ResourceItem::Memory => (MEMORY, Hue::Yellow, "Memory"),
        ResourceItem::Temperature => (THERMOMETER, Hue::Red, "Temperature"),
        ResourceItem::Net => (DOWNLOAD, Hue::Cyan, "Network"),
        ResourceItem::Disk => (HARDDISK, Hue::Blue, "Disk"),
        ResourceItem::Gpu => (GPU, Hue::Purple, "GPU"),
    }
}

/// What an item shows when it has no figure: a dim dash.
fn no_figure() -> Vec<(String, Style)> {
    vec![("—".to_string(), Style::MUTED)]
}

/// A temperature figure, in the error colour from `HOT` (85 °C) as on the bar, else in the item's
/// hue.
fn degrees_figure(t: i16, hue: Hue) -> Vec<(String, Style)> {
    let style = if t >= HOT { Style::ERROR } else { Style::hue(hue) };
    vec![(format!("{t}°"), style)]
}

/// The item's current figure as styled pieces; `—` in the dim style when there is none.
fn figure(item: ResourceItem, index: usize, r: &Resources) -> Vec<(String, Style)> {
    // `let ... else` leaves the function when there is no sample yet.
    let Some(s) = r.last else { return no_figure() };
    let (_, hue, _) = look(item);
    match item {
        ResourceItem::Cpu => match s.cpu {
            Some(p) => vec![(format!("{p}%"), Style::hue(hue))],
            None => no_figure(),
        },
        ResourceItem::Disk => match disk_of(&s, index) {
            Some(p) => vec![(format!("{p}%"), Style::hue(hue))],
            None => no_figure(),
        },
        ResourceItem::Memory => match s.memory {
            // Tenths of GiB: `61` reads `6.1G`, and the total follows, dim, once it is known.
            Some(m) => {
                let mut pieces = vec![(format!("{}.{}G", m / 10, m % 10), Style::hue(hue))];
                if let Some(t) = s.memory_total {
                    pieces.push((format!(" of {}.{}G", t / 10, t % 10), Style::MUTED));
                }
                pieces
            }
            None => no_figure(),
        },
        ResourceItem::Temperature => match s.temperature {
            Some(t) => degrees_figure(t, hue),
            None => no_figure(),
        },
        // A dim `vram` share first, then the usage, then the degrees: each when read.
        ResourceItem::Gpu => {
            if s.gpu_busy.is_none() && s.gpu.is_none() {
                return no_figure();
            }
            let mut pieces = Vec::new();
            if let Some(v) = s.vram {
                pieces.push((format!("vram {v}% "), Style::MUTED));
            }
            if let Some(b) = s.gpu_busy {
                pieces.push((format!("{}%", b.percent), Style::hue(hue)));
            }
            if let Some(t) = s.gpu {
                if s.gpu_busy.is_some() {
                    pieces.push((" ".to_string(), Style::hue(hue)));
                }
                pieces.extend(degrees_figure(t, hue));
            }
            pieces
        }
        ResourceItem::Net => match s.net {
            // The dim `peak` is the graph's top, so the reader knows what its height means.
            Some(n) => {
                let mut pieces = Vec::new();
                if let Some(top) = net_top(&r.history) {
                    pieces.push((format!("peak {} ", rate(top)), Style::MUTED));
                }
                pieces.push((format!("{DOWNLOAD} {} {UPLOAD} {}", rate(n.down), rate(n.up)), Style::hue(hue)));
                pieces
            }
            None => no_figure(),
        },
    }
}

/// The graph's series: a filled body in the item's hue, and for the network its upload as a dim
/// line over it.
fn graph_series(item: ResourceItem, index: usize, h: &History) -> Vec<Series> {
    let (_, hue, _) = look(item);
    let mut lines = series(item, index, h).into_iter();
    let mut out = Vec::new();
    if let Some(points) = lines.next() {
        out.push(Series { points, ink: Tint::Hue(hue), fill: true });
    }
    if let Some(points) = lines.next() {
        out.push(Series { points, ink: Tint::FgDim, fill: false });
    }
    out
}

/// The `Monitor…` row's row in the `Boxed`'s coordinates: past the header and the blocks, and the
/// separator. The `min` stops at the largest `u16` rather than wrapping round.
pub fn monitor_row(c: &ResourcesConfig) -> u16 {
    (2 + BLOCK * blocks(c).len()).min(usize::from(u16::MAX)) as u16
}

impl ResourcesPopup {
    /// A popup with the `Monitor…` row when `monitor`, nothing selected.
    pub fn new(monitor: bool) -> Self {
        Self { selected: false, monitor }
    }

    /// The rows, `WIDTH` cells wide inside `list::boxed`'s ring: the header, a block per item of
    /// `show` (kept whether or not it has a figure, so the size never changes: see docs/decisions.md,
    /// "Every resources item keeps its block in the popup"), and, with the row, a separator and
    /// `Monitor…`. The graphs are set after `boxed`, in its coordinates (it drops the ring's first
    /// row and column).
    pub fn render(&self, r: &Resources, c: &ResourcesConfig) -> Boxed {
        let columns = WIDTH + 2;
        let mut rows = vec![Grid::empty(columns)];
        let mut header = Grid::empty(columns);
        header.write(2, WIDTH, "Resources", Style::BOLD);
        // 60 samples of `interval` seconds make `interval` minutes. See docs/decisions.md, "The
        // resources popup spans 60 times the interval".
        let span = format!("last {} min", c.interval);
        // Right-aligned: `saturating_sub` stops at 0 for a span wider than the row.
        header.write(WIDTH.saturating_sub(text_width(&span)), WIDTH, &span, Style::MUTED);
        rows.push(header);
        let blocks = blocks(c);
        for &(item, index) in &blocks {
            let (icon, hue, name) = look(item);
            let mut g = Grid::empty(columns);
            // The icon always shows its hue, whatever `module_colors` says, as in the privacy
            // popup (`ModuleColors::Icons` is passed for that).
            let x = g.write(2, WIDTH, &icon.to_string(), icon_style(ModuleColors::Icons, hue, Style::NORMAL));
            // The figure's pieces are right-aligned as one run: its width first, then each piece
            // is written where the last one ended.
            let pieces = figure(item, index, r);
            let mut width: u16 = 0;
            for (text, _) in &pieces {
                width += text_width(text);
            }
            // A listed path names its block, cut from the left to leave a cell before the figure.
            match c.disk_mounts.get(index).filter(|_| item == ResourceItem::Disk) {
                Some(path) => {
                    g.write(x + 1, WIDTH, &mount_label(path, WIDTH.saturating_sub(width + 1).saturating_sub(x + 1)), Style::NORMAL)
                }
                None => g.write(x + 1, WIDTH, name, Style::NORMAL),
            };
            let mut x = WIDTH.saturating_sub(width);
            for (text, style) in &pieces {
                x = g.write(x, WIDTH, text, *style);
            }
            rows.push(g);
            // The graph's own rows are blank here: the painter draws the graph over them.
            for _ in 0..GRAPH_ROWS {
                rows.push(Grid::empty(columns));
            }
        }
        rows.push(Grid::empty(columns));
        let mut separators = Vec::new();
        if self.monitor {
            // The separator sits on the blank row just pushed, above `Monitor…`.
            separators.push(rows.len() - 1);
            let mut monitor = Grid::empty(columns);
            let row =
                Row { icon: None, icon_style: Style::NORMAL, text: "Monitor…", right: "", style: Style::NORMAL, selected: self.selected };
            list::row(&mut monitor, 1, columns - 1, &row);
            rows.push(monitor);
            rows.push(Grid::empty(columns));
        }
        let mut boxed = list::boxed(&rows, &separators, None);
        for (i, &(item, index)) in blocks.iter().enumerate() {
            boxed.graphs.push(Graph {
                row: 2 + BLOCK * i,
                col: 1,
                width: WIDTH - 2,
                rows: GRAPH_ROWS,
                series: graph_series(item, index, &r.history),
            });
        }
        boxed
    }

    /// Selects `Monitor…`: Redraw when that changed something, Nothing when it was already
    /// selected or there is no such row.
    fn select(&mut self) -> ResourcesAction {
        if !self.monitor {
            return ResourcesAction::Nothing;
        }
        if self.selected {
            return ResourcesAction::Nothing;
        }
        self.selected = true;
        ResourcesAction::Redraw
    }

    /// ↓ ↑ Tab Shift+Tab Home End select `Monitor…`, Enter and Space run it once selected, Escape
    /// closes; every other key does nothing. Nothing is selected when the popup opens, as in the
    /// power menu, so an Enter at once does nothing.
    pub fn key(&mut self, k: Key) -> ResourcesAction {
        match k {
            Key::Escape => ResourcesAction::Close,
            Key::Up | Key::Down | Key::Tab | Key::BackTab | Key::Home | Key::End => self.select(),
            Key::Enter | Key::Char(' ') if self.selected => ResourcesAction::Monitor,
            _ => ResourcesAction::Nothing,
        }
    }

    /// The pointer on content row `row` (`None`: off every cell): hovering `Monitor…` selects it, a
    /// left press on it runs it; anything else does nothing.
    pub fn pointer(&mut self, row: Option<u16>, kind: PopupPointer, c: &ResourcesConfig) -> ResourcesAction {
        if !self.monitor || row != Some(monitor_row(c)) {
            return ResourcesAction::Nothing;
        }
        match kind {
            PopupPointer::Motion => self.select(),
            PopupPointer::Press(Button::Left) => ResourcesAction::Monitor,
            _ => ResourcesAction::Nothing,
        }
    }
}

pub mod app;

#[cfg(test)]
mod tests;
