//! The resources popup (resources popup spec §§ 2, 4, 6), pure: one block per item of `[resources]
//! show` (its icon, name and figure, then a graph two rows tall), the graphs' series in permille,
//! the `Monitor…` row, its selection, keys and pointer. The popup (`PopupBody::Resources`), the
//! repaint on each tick and the command are the wiring's (`resources_app.rs`): nothing here reads
//! a file, starts a thread or runs a command.

use crate::actions::Button;
use crate::config::{ResourceItem, ResourcesConfig};
use crate::grid::{Grid, Hue, Style, Tint, text_width};
use crate::keys::Key;
use crate::list::{self, Boxed, Graph, Row, Series};
use crate::popup::PopupPointer;
use crate::segments::{CPU, DOWNLOAD, GPU, HARDDISK, HOT, MEMORY, THERMOMETER, UPLOAD, icon_style, rate};
use crate::system::{HISTORY, History, Resources};
use alpheus_commun::theme::settings::ModuleColors;

/// A row's cells, its pad cell at each end included.
pub const WIDTH: u16 = 40;
/// Rows each graph takes, under its figure's row.
pub const GRAPH_ROWS: u16 = 2;
/// The network graph's lowest top, bytes per second: an idle link does not fill it with noise.
pub const NET_FLOOR: u64 = 128 * 1024;

/// What a key or the pointer asks of the wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcesAction {
    Nothing,
    Redraw,
    /// Run `[actions] resources`, then close.
    Monitor,
    /// Escape.
    Close,
}

/// The open popup's own state: whether `Monitor…` is selected, and whether it is there at all
/// (`[actions] resources` empty: no row, no separator, nothing to select or run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcesPopup {
    pub selected: bool,
    pub monitor: bool,
}

impl Default for ResourcesPopup {
    fn default() -> Self {
        Self::new(true)
    }
}

/// Rows one item's block takes: its figure's row and its graph.
const BLOCK: usize = 1 + GRAPH_ROWS as usize;

/// A value as permille of `top`, rounded down, at most 1000.
fn permille(v: u64, top: u64) -> u16 {
    if top == 0 {
        return 0;
    }
    (u128::from(v) * 1000 / u128::from(top)).min(1000) as u16
}

/// One item's points in permille, oldest first, always `HISTORY` long: a ring not yet full leaves
/// leading gaps, so the newest point stays at the right edge (a fixed time axis). Net gives two
/// series, download then upload; the others one. A sample with no reading is a gap.
pub fn series(item: ResourceItem, h: &History) -> Vec<Vec<Option<u16>>> {
    let top = net_top(h).unwrap_or(NET_FLOOR);
    let count = if item == ResourceItem::Net { 2 } else { 1 };
    let mut out = vec![vec![None; HISTORY - h.len()]; count];
    for s in h.iter() {
        let points: [Option<u16>; 2] = match item {
            ResourceItem::Cpu => [s.cpu.map(|p| permille(u64::from(p), 100)), None],
            ResourceItem::Disk => [s.disk.map(|p| permille(u64::from(p), 100)), None],
            ResourceItem::Memory => {
                let point = match (s.memory, s.memory_total) {
                    (Some(m), Some(t)) if t > 0 => Some(permille(u64::from(m), u64::from(t))),
                    _ => None,
                };
                [point, None]
            }
            ResourceItem::Temperature => [s.temperature.map(degrees), None],
            ResourceItem::Gpu => [s.gpu.map(degrees), None],
            ResourceItem::Net => match s.net {
                Some(n) => [Some(permille(n.down, top)), Some(permille(n.up, top))],
                None => [None, None],
            },
        };
        for (column, p) in out.iter_mut().zip(points) {
            column.push(p);
        }
    }
    out
}

/// °C on a 0 to 100 scale, clamped.
fn degrees(t: i16) -> u16 {
    permille(t.clamp(0, 100) as u64, 100)
}

/// The network graph's top: the highest download or upload in the ring, never below `NET_FLOOR`;
/// None while no sample has a net reading.
pub fn net_top(h: &History) -> Option<u64> {
    h.iter().filter_map(|s| s.net).map(|n| n.down.max(n.up)).max().map(|top| top.max(NET_FLOOR))
}

/// The item's icon, hue and name.
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

/// The item's current figure as styled pieces; `—` in the dim style when there is none.
fn figure(item: ResourceItem, r: &Resources) -> Vec<(String, Style)> {
    let none = || vec![("—".to_string(), Style::MUTED)];
    let Some(s) = r.last else { return none() };
    let (_, hue, _) = look(item);
    let hot = |t: i16| if t >= HOT { Style::ERROR } else { Style::hue(hue) };
    match item {
        ResourceItem::Cpu => s.cpu.map_or_else(none, |p| vec![(format!("{p}%"), Style::hue(hue))]),
        ResourceItem::Disk => s.disk.map_or_else(none, |p| vec![(format!("{p}%"), Style::hue(hue))]),
        ResourceItem::Memory => s.memory.map_or_else(none, |m| {
            let mut pieces = vec![(format!("{}.{}G", m / 10, m % 10), Style::hue(hue))];
            if let Some(t) = s.memory_total {
                pieces.push((format!(" of {}.{}G", t / 10, t % 10), Style::MUTED));
            }
            pieces
        }),
        ResourceItem::Temperature => s.temperature.map_or_else(none, |t| vec![(format!("{t}°"), hot(t))]),
        ResourceItem::Gpu => s.gpu.map_or_else(none, |t| vec![(format!("{t}°"), hot(t))]),
        ResourceItem::Net => s.net.map_or_else(none, |n| {
            let mut pieces = Vec::new();
            if let Some(top) = net_top(&r.history) {
                pieces.push((format!("peak {} ", rate(top)), Style::MUTED));
            }
            pieces.push((format!("{DOWNLOAD} {} {UPLOAD} {}", rate(n.down), rate(n.up)), Style::hue(hue)));
            pieces
        }),
    }
}

/// The graph's series: a filled hue body, the network's upload a dim line over it.
fn graph_series(item: ResourceItem, h: &History) -> Vec<Series> {
    let (_, hue, _) = look(item);
    let mut lines = series(item, h).into_iter();
    let mut out = Vec::new();
    if let Some(points) = lines.next() {
        out.push(Series { points, ink: Tint::Hue(hue), fill: true });
    }
    if let Some(points) = lines.next() {
        out.push(Series { points, ink: Tint::FgDim, fill: false });
    }
    out
}

/// The `Monitor…` row's row in the `Boxed`'s coordinates (the header, the blocks, the separator).
pub fn monitor_row(c: &ResourcesConfig) -> u16 {
    (2 + BLOCK * c.show.len()).min(usize::from(u16::MAX)) as u16
}

impl ResourcesPopup {
    /// A popup with the `Monitor…` row when `monitor`, nothing selected.
    pub fn new(monitor: bool) -> Self {
        Self { selected: false, monitor }
    }

    /// The rows, `WIDTH` cells wide inside `list::boxed`'s ring: the header, a block per item of
    /// `show` (kept whether or not it has a figure, so the size never changes), and, with the
    /// row, a separator and `Monitor…`; the graphs set after `boxed`, in its coordinates.
    pub fn render(&self, r: &Resources, c: &ResourcesConfig) -> Boxed {
        let columns = WIDTH + 2;
        let mut rows = vec![Grid::empty(columns)];
        let mut header = Grid::empty(columns);
        header.write(2, WIDTH, "Resources", Style::BOLD);
        let span = format!("last {} min", c.interval);
        header.write(WIDTH.saturating_sub(text_width(&span)), WIDTH, &span, Style::MUTED);
        rows.push(header);
        for &item in &c.show {
            let (icon, hue, name) = look(item);
            let mut g = Grid::empty(columns);
            let x = g.write(2, WIDTH, &icon.to_string(), icon_style(ModuleColors::Icons, hue, Style::NORMAL));
            g.write(x + 1, WIDTH, name, Style::NORMAL);
            let pieces = figure(item, r);
            let width: u16 = pieces.iter().map(|(t, _)| text_width(t)).sum();
            let mut x = WIDTH.saturating_sub(width);
            for (text, style) in &pieces {
                x = g.write(x, WIDTH, text, *style);
            }
            rows.push(g);
            rows.extend((0..GRAPH_ROWS).map(|_| Grid::empty(columns)));
        }
        rows.push(Grid::empty(columns));
        let mut separators = Vec::new();
        if self.monitor {
            separators.push(rows.len() - 1);
            let mut monitor = Grid::empty(columns);
            let row =
                Row { icon: None, icon_style: Style::NORMAL, text: "Monitor…", right: "", style: Style::NORMAL, selected: self.selected };
            list::row(&mut monitor, 1, columns - 1, &row);
            rows.push(monitor);
            rows.push(Grid::empty(columns));
        }
        let mut boxed = list::boxed(&rows, &separators, None);
        boxed.graphs = c
            .show
            .iter()
            .enumerate()
            .map(|(i, &item)| Graph {
                row: 2 + BLOCK * i,
                col: 1,
                width: WIDTH - 2,
                rows: GRAPH_ROWS,
                series: graph_series(item, &r.history),
            })
            .collect();
        boxed
    }

    fn select(&mut self) -> ResourcesAction {
        if !self.monitor || std::mem::replace(&mut self.selected, true) { ResourcesAction::Nothing } else { ResourcesAction::Redraw }
    }

    /// ↓ ↑ Tab Shift+Tab Home End select `Monitor…`, Enter and Space run it once selected, Escape
    /// closes; every other key does nothing.
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

#[cfg(test)]
mod tests;
