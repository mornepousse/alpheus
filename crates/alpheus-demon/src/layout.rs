//! From state to a screen's grid. Each module produces a segment; segments
//! are laid out left to right in bar.toml order, separated by "│". The
//! window module is elastic: it takes the free space. With no active
//! window, the window module has no segment at all (no empty "│ │"), but
//! the free space still goes where it sits in the configured order — just
//! before the segment that follows it — so the right-hand modules stay
//! right-aligned instead of collapsing left; only with the window module
//! absent from the configuration entirely does the free space fall back to
//! preceding the last module. Narrow screen: modules disappear by priority
//! (resources, the tray, media, title, then Bluetooth, network, volume,
//! battery, the launcher button, and last the power button); workspaces, the
//! clock and the notifications bell always stay.
//!
//! Groups (shapes spec § 2): consecutive modules of one kind (`ModuleSpec::group`) make one
//! block, never across the free space. `flat` and `underline` keep the "│" separators; the block
//! styles put one blank pad cell inside each end of a block, one blank cell between blocks
//! (powerline's arrow cell) and two blank cells between the modules of one block. The grid records the
//! blocks (`Grid::groups`) for every style but `flat`, whose grid is exactly today's.

use crate::compositor::{CompositorState, Workspace};
use crate::config::ModuleId;
use crate::grid::{Grid, Hue, Style, Target, text_width, truncate};
pub use crate::grid::{Group, GroupKind};
use crate::state::{Bar, Feed};
use crate::{clock, icons, segments};
use alpheus_commun::theme::settings::{BarStyle, ModuleColors};

/// Order of disappearance when space runs short: the spec's (resources,
/// media, title), then the system indicators (Q9); the tray goes right
/// after resources (Mae's ruling for the tray), and the launcher button
/// after the system indicators (Mae, 2026-10-05: `Mod+Space` still opens the
/// launcher), then the privacy indicators (privacy spec § 1), then the power button, the last of
/// all (power spec § 1).
/// Workspaces, the clock and the bell always stay.
const REMOVAL: [ModuleId; 11] = [
    ModuleId::Resources,
    ModuleId::Tray,
    ModuleId::Media,
    ModuleId::Window,
    ModuleId::Bluetooth,
    ModuleId::Network,
    ModuleId::Volume,
    ModuleId::Battery,
    ModuleId::Launcher,
    ModuleId::Privacy,
    ModuleId::Power,
];
/// Minimum title width before it disappears.
const MIN_TITLE: u16 = 8;
/// Beyond this, a workspace's windows are summarized by "+" (Q4).
const MAX_DOTS: u16 = 4;
/// A longer workspace name is truncated.
const MAX_NAME: u16 = 12;
const SEPARATOR: &str = " │ ";
/// Margin of one cell at each end.
const MARGIN: u16 = 1;
/// Block styles: blank cells inside each end of a block.
const PAD: u16 = 1;
/// Block styles: blank cells between two blocks (powerline draws its arrow there).
const GAP: u16 = 1;
/// Block styles: blank cells between two modules of one block, in place of " │ ".
const INNER: u16 = 2;

/// How a style spaces the modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spacing {
    /// " │ " between modules: `flat`, `underline`.
    Separators,
    /// Pads, gaps, blank cells: `islands`, `powerline`, `floating`, `translucent`.
    Blocks,
}

fn spacing(style: BarStyle) -> Spacing {
    match style {
        BarStyle::Flat | BarStyle::Underline => Spacing::Separators,
        BarStyle::Islands | BarStyle::Powerline | BarStyle::Floating | BarStyle::Translucent => Spacing::Blocks,
    }
}

pub(crate) struct Piece {
    pub(crate) text: String,
    pub(crate) style: Style,
    pub(crate) target: Option<Target>,
}

pub(crate) struct Segment {
    pub(crate) module: ModuleId,
    pub(crate) pieces: Vec<Piece>,
    pub(crate) elastic: bool,
    /// Click zone covering the whole segment (system modules); `None`: only
    /// the pieces' own zones (workspaces), or none (title, clock).
    pub(crate) target: Option<Target>,
    /// The block styles give this segment's block no hue (the neutral fill): a state wins over the
    /// hue (a paused media).
    pub(crate) withhold_hue: bool,
}

impl Segment {
    pub(crate) fn width(&self) -> u16 {
        self.pieces.iter().map(|p| text_width(&p.text)).fold(0, u16::saturating_add)
    }

    fn min_width(&self) -> u16 {
        if self.elastic { MIN_TITLE } else { self.width() }
    }
}

pub(crate) fn piece(text: impl Into<String>, style: Style) -> Piece {
    Piece { text: text.into(), style, target: None }
}

fn label(w: &Workspace) -> String {
    let n = w.windows;
    let urgent = if w.urgent { "!" } else { "" };
    let dots = "·".repeat(n.min(MAX_DOTS) as usize);
    let plus = if n > MAX_DOTS { "+" } else { "" };
    format!(" {}{urgent}{dots}{plus} ", truncate(&w.name, MAX_NAME))
}

fn workspace_style(w: &Workspace) -> Style {
    if w.urgent && !w.focused {
        Style::URGENT
    } else if w.focused {
        Style::ACTIVE
    } else if w.visible {
        Style::SURFACE
    } else if w.windows == 0 {
        Style::MUTED
    } else {
        Style::NORMAL
    }
}

/// `None` if the compositor knows no workspace at all: no empty segment (no
/// "│ │"), rather than a segment with no piece.
pub(crate) fn workspaces(state: &Feed<CompositorState>, output: &str) -> Option<Segment> {
    let pieces = match state {
        Feed::Waiting => vec![piece(" … ", Style::MUTED)],
        Feed::Down => vec![piece(" – ", Style::MUTED)],
        Feed::Ready(e) => {
            let m: Vec<Piece> = e
                .workspaces_of(output)
                .into_iter()
                .map(|w| Piece {
                    text: label(w),
                    style: workspace_style(w),
                    target: Some(Target::Workspace { reference: w.reference.clone(), output: w.output.clone() }),
                })
                .collect();
            if m.is_empty() {
                return None;
            }
            m
        }
    };
    Some(Segment { module: ModuleId::Workspaces, pieces, elastic: false, target: None, withhold_hue: false })
}

/// `None` with no active window: no empty segment (no "│ <empty> │"),
/// rather than an empty text piece.
pub(crate) fn window(state: &Feed<CompositorState>, mc: ModuleColors) -> Option<Segment> {
    let pieces = match state {
        Feed::Waiting => vec![piece("…", Style::NORMAL)],
        Feed::Down => vec![piece("–", Style::NORMAL)],
        Feed::Ready(CompositorState { window: Some(w), .. }) => vec![
            piece(icons::glyph(&w.class).to_string(), segments::icon_style(mc, Hue::Purple, Style::NORMAL)),
            piece(format!(" {}", w.title), segments::text_style(mc, Hue::Purple, Style::NORMAL)),
        ],
        Feed::Ready(_) => return None,
    };
    Some(Segment { module: ModuleId::Window, pieces, elastic: true, target: None, withhold_hue: false })
}

/// The clock is a click zone only when a left click opens the calendar (`[clock] calendar`).
pub(crate) fn bar_clock(bar: &Bar) -> Segment {
    let pieces = match &bar.clock {
        None => vec![piece("…", Style::MUTED)],
        Some(h) => {
            let c = &bar.config.clock;
            let mut v = Vec::new();
            if !c.date.is_empty() {
                v.push(piece(format!("{} ", clock::format(&c.date, h)), Style::MUTED));
            }
            v.push(piece(clock::format(&c.format, h), Style::BOLD_ACCENT));
            v
        }
    };
    let target = bar.config.clock.calendar.then_some(Target::Module(ModuleId::Clock));
    Segment { module: ModuleId::Clock, pieces, elastic: false, target, withhold_hue: false }
}

/// A module's segment, or None if it has nothing to show.
fn segment(bar: &Bar, m: ModuleId, output: &str) -> Option<Segment> {
    m.spec().segment.and_then(|f| f(bar, output))
}

/// The segment the free space goes before when none is elastic: `gap_at` (the absent window
/// module's slot), or else the last one. None with an elastic segment (it takes the free space) or
/// fewer than two segments.
fn space_before(segs: &[Segment], gap_at: Option<usize>) -> Option<usize> {
    if segs.len() < 2 || segs.iter().any(|s| s.elastic) {
        return None;
    }
    match gap_at {
        Some(i) if i < segs.len() => Some(i),
        _ => Some(segs.len() - 1),
    }
}

/// True when segment `i` starts a block: the first one, another kind than the one before it, or
/// right after the free space.
fn opens_group(segs: &[Segment], i: usize, space: Option<usize>) -> bool {
    i == 0 || space == Some(i) || segs[i - 1].module.spec().group != segs[i].module.spec().group
}

/// Cells between segment `i - 1` and segment `i` (`i > 0`), the free space aside.
fn joint(segs: &[Segment], i: usize, space: Option<usize>, spacing: Spacing) -> u16 {
    match spacing {
        Spacing::Separators => text_width(SEPARATOR),
        Spacing::Blocks if opens_group(segs, i, space) => PAD + GAP + PAD,
        Spacing::Blocks => INNER,
    }
}

/// Every cell that is not a segment's: margins, joints and, for the block styles, the pads at
/// both ends.
fn spacing_width(segs: &[Segment], space: Option<usize>, spacing: Spacing) -> u16 {
    let ends = if spacing == Spacing::Blocks && !segs.is_empty() { 2 * PAD } else { 0 };
    (1..segs.len()).map(|i| joint(segs, i, space, spacing)).fold(2 * MARGIN + ends, u16::saturating_add)
}

/// Minimum width: the spacing, segments (title at MIN_TITLE).
fn needed(segs: &[Segment], space: Option<usize>, spacing: Spacing) -> u16 {
    segs.iter().map(Segment::min_width).fold(spacing_width(segs, space, spacing), u16::saturating_add)
}

impl Bar {
    pub fn grid(&self, output: &str, columns: u16) -> Grid {
        let mut segs: Vec<Segment> = self.config.modules.iter().filter_map(|m| segment(self, *m, output)).collect();
        let spacing = spacing(self.style);
        for removed in REMOVAL {
            if needed(&segs, space_before(&segs, self.window_gap(&segs)), spacing) <= columns {
                break;
            }
            segs.retain(|s| s.module != removed);
        }
        let gap_at = self.window_gap(&segs);
        lay_out_styled(&segs, columns, gap_at, self.style)
    }

    /// When the window module is configured but has no segment (no active
    /// window: `window()` returned `None`, so nothing in `segs` is elastic),
    /// the index in `segs` where its free space belongs: just before the
    /// segment that follows it in the configured module order, counting
    /// only modules that actually survived into `segs` (narrow screens may
    /// have dropped others along the way). `None` when the window module is
    /// absent from the configuration entirely (the old "before the last
    /// segment" rule applies then) or when it does have a segment (its own
    /// elasticity handles the free space).
    fn window_gap(&self, segs: &[Segment]) -> Option<usize> {
        if segs.iter().any(|s| s.module == ModuleId::Window) {
            return None;
        }
        let window_pos = self.config.modules.iter().position(|m| *m == ModuleId::Window)?;
        let before = &self.config.modules[..window_pos];
        Some(segs.iter().filter(|s| before.contains(&s.module)).count())
    }
}

#[cfg(test)]
pub(crate) fn lay_out(segs: &[Segment], columns: u16, gap_at: Option<usize>) -> Grid {
    lay_out_styled(segs, columns, gap_at, BarStyle::Flat)
}

/// Records a block unless clipping left nothing but blanks in it: a lone pad would be a lone border.
fn record_block(g: &mut Grid, block: Group) {
    let end = usize::from(block.end.min(g.width()));
    if g.cells().get(usize::from(block.start)..end).is_some_and(|c| c.iter().any(|c| c.ch != ' ')) {
        g.group(block);
    }
}

pub(crate) fn lay_out_styled(segs: &[Segment], columns: u16, gap_at: Option<usize>, style: BarStyle) -> Grid {
    let spacing = spacing(style);
    let mut g = Grid::empty(columns);
    let end = columns.saturating_sub(MARGIN);
    let space = space_before(segs, gap_at);
    let fixed = segs.iter().filter(|s| !s.elastic).map(Segment::width).fold(spacing_width(segs, space, spacing), u16::saturating_add);
    let free = columns.saturating_sub(fixed);
    let pad = if spacing == Spacing::Blocks { PAD } else { 0 };
    // `flat` paints no block: its grid records none, exactly today's.
    let record = style != BarStyle::Flat;
    // The block being laid out: its kind, first cell and hue; and where the last segment's text ended.
    let mut open: Option<(GroupKind, u16, Option<Hue>)> = None;
    let mut text_end = MARGIN;
    let mut x = MARGIN;
    for (i, s) in segs.iter().enumerate() {
        let opens = opens_group(segs, i, space);
        if opens && i > 0 && spacing == Spacing::Blocks {
            x = x.saturating_add(PAD); // the closing pad of the block before
        }
        if space == Some(i) {
            x = x.saturating_add(free);
            g.set_split(x);
        }
        if i > 0 {
            x = match spacing {
                Spacing::Separators => g.write(x, end, SEPARATOR, Style::MUTED),
                Spacing::Blocks if opens => x.saturating_add(GAP),
                Spacing::Blocks => x.saturating_add(INNER),
            };
        }
        if opens {
            if let Some((kind, start, hue)) = open.take()
                && record
            {
                record_block(&mut g, Group { kind, start, end: text_end.saturating_add(pad), hue });
            }
            open = Some((s.module.spec().group, x, if s.withhold_hue { None } else { s.module.spec().hue }));
            x = x.saturating_add(pad);
        }
        let limit = if s.elastic { x.saturating_add(free).min(end) } else { end };
        let segment_start = x;
        for p in &s.pieces {
            let text = if s.elastic { truncate(&p.text, limit.saturating_sub(x)) } else { p.text.clone() };
            let start = x;
            x = g.write(x, limit, &text, p.style);
            if let Some(t) = &p.target {
                g.zone(start, x, t.clone());
            }
        }
        if let Some(t) = &s.target {
            g.zone(segment_start, x, t.clone());
        }
        text_end = x;
        if s.elastic {
            x = limit;
            // Past the closing pad, as in the non-elastic branch: the title's block ends before the split.
            g.set_split(x.saturating_add(pad));
        }
    }
    if let Some((kind, start, hue)) = open
        && record
    {
        record_block(&mut g, Group { kind, start, end: text_end.saturating_add(pad), hue });
    }
    g
}

#[cfg(test)]
mod tests;
