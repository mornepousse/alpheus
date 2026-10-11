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
//! battery, and last the launcher button); workspaces, the clock and the
//! notifications bell always stay.
//!
//! Groups (shapes spec § 2): consecutive modules of one kind (`group_kind`) make one block, never
//! across the free space. `flat` and `underline` keep the "│" separators; the block styles put
//! one blank pad cell inside each end of a block, one blank cell between blocks (powerline's
//! arrow cell) and two blank cells between the modules of one block. The grid records the
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
/// after resources (Mae, tray plan 2 question 1), and the launcher button
/// last (Mae, 2026-10-05: `Mod+Space` still opens the launcher). Workspaces,
/// the clock and the bell always stay.
const REMOVAL: [ModuleId; 9] = [
    ModuleId::Resources,
    ModuleId::Tray,
    ModuleId::Media,
    ModuleId::Window,
    ModuleId::Bluetooth,
    ModuleId::Network,
    ModuleId::Volume,
    ModuleId::Battery,
    ModuleId::Launcher,
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

/// The block a module belongs to (shapes spec § 2). Media is a block of its own (media spec § 1);
/// the bell joins the status block.
pub fn group_kind(m: ModuleId) -> GroupKind {
    match m {
        ModuleId::Launcher => GroupKind::Launcher,
        ModuleId::Workspaces => GroupKind::Workspaces,
        ModuleId::Window => GroupKind::Title,
        ModuleId::Media => GroupKind::Media,
        ModuleId::Volume
        | ModuleId::Network
        | ModuleId::Bluetooth
        | ModuleId::Battery
        | ModuleId::Notifications
        | ModuleId::Osd
        | ModuleId::Quick => GroupKind::Status,
        ModuleId::Resources => GroupKind::Resources,
        ModuleId::Tray => GroupKind::Tray,
        ModuleId::Clock => GroupKind::Clock,
    }
}

/// A module's fixed hue (themes spec § 9; the CPU's orange for resources), None for the modules
/// without one.
pub const fn module_hue(m: ModuleId) -> Option<Hue> {
    match m {
        ModuleId::Window | ModuleId::Bluetooth | ModuleId::Media => Some(Hue::Purple),
        ModuleId::Volume => Some(Hue::Blue),
        ModuleId::Network => Some(Hue::Cyan),
        ModuleId::Battery => Some(Hue::Green),
        ModuleId::Resources => Some(Hue::Orange),
        ModuleId::Launcher
        | ModuleId::Workspaces
        | ModuleId::Tray
        | ModuleId::Notifications
        | ModuleId::Osd
        | ModuleId::Quick
        | ModuleId::Clock => None,
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
fn workspaces(state: &Feed<CompositorState>, output: &str) -> Option<Segment> {
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
fn window(state: &Feed<CompositorState>, mc: ModuleColors) -> Option<Segment> {
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
fn bar_clock(bar: &Bar) -> Segment {
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
    match m {
        ModuleId::Launcher => Some(segments::launcher()),
        ModuleId::Workspaces => workspaces(&bar.compositor, output),
        ModuleId::Window => window(&bar.compositor, bar.module_colors),
        ModuleId::Clock => Some(bar_clock(bar)),
        ModuleId::Volume => segments::volume(&bar.sound, bar.module_colors),
        ModuleId::Network => segments::network(&bar.network, bar.module_colors),
        ModuleId::Bluetooth => segments::bluetooth(&bar.bluetooth, bar.module_colors),
        ModuleId::Battery => segments::battery(&bar.battery, &bar.config.battery, bar.module_colors),
        ModuleId::Resources => segments::resources(&bar.resources, &bar.config.resources, bar.module_colors),
        ModuleId::Tray => segments::tray(&bar.tray, &bar.config.tray),
        ModuleId::Media => segments::media(bar.media.as_ref(), bar.module_colors),
        ModuleId::Notifications => segments::notifications(bar.notifications.as_ref()),
        // The OSD draws its own surface (osd_surface.rs), never in the bar.
        ModuleId::Osd => None,
        // The quick panel is a popup (quick.rs), never a segment.
        ModuleId::Quick => None,
    }
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
    i == 0 || space == Some(i) || group_kind(segs[i - 1].module) != group_kind(segs[i].module)
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
            open = Some((group_kind(s.module), x, if s.withhold_hue { None } else { module_hue(s.module) }));
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
mod tests {
    use super::*;
    use crate::clock::LocalTime;
    use crate::compositor::{Window, example};
    use crate::config::{BarConfig, BatteryConfig, BatteryVisibility, ModuleId};
    use crate::media::MediaView;
    use crate::notify::NotifyView;
    use crate::state::Msg;
    use crate::system::{Battery, Charge, Link, NetState, Peripheral, PeripheralKind, PowerState, SoundState, Volume};
    use alpheus_commun::theme::settings::{BarStyle, ModuleColors};

    const SUN: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };

    fn bar() -> Bar {
        let mut b = Bar::new(BarConfig::default());
        b.apply(Msg::Compositor(Some(example())));
        b.apply(Msg::Clock(SUN));
        b
    }

    fn snapshot(g: &Grid) -> String {
        format!("{}\n{}", g.text(), g.styles())
    }

    #[test]
    fn wide_bar_on_the_hdmi_screen() {
        let g = bar().grid("HDMI-A-1", 100);
        let (t, s) = (g.text(), g.styles());
        assert_eq!(g.width(), 100);
        assert!(t.starts_with(" \u{f0349} │  2·  4!····+  5  │ \u{f0239} Web page — Mozilla Firefox "), "launcher button first: {t}");
        assert!(t.ends_with(" │ Sun 4 Oct 14:05 "), "{t}");
        assert!(s.starts_with("..mmmAAAAuuuuuuuuummmmmm"), "{s}");
        assert!(s.ends_with("mmmmmmmmmmmmmGGGGG."), "{s}");
        insta::assert_snapshot!("hdmi_100", snapshot(&g));
    }

    #[test]
    fn title_truncated_then_removed_when_space_is_short() {
        let g = bar().grid("HDMI-A-1", 60);
        assert!(g.text().contains("Web page — Moz…"), "{}", g.text());
        insta::assert_snapshot!("hdmi_60", snapshot(&g));
        let g = bar().grid("HDMI-A-1", 40);
        let t = g.text();
        assert!(!t.contains("Web") && t.contains("14:05") && t.contains(" 4!····+ "), "{t}");
        assert_eq!(g.width(), 40);
        insta::assert_snapshot!("hdmi_40", snapshot(&g));
    }

    #[test]
    fn extreme_sizes_without_panic() {
        for w in [0u16, 1, 5, 12, 39] {
            assert_eq!(bar().grid("HDMI-A-1", w).width(), w);
        }
        // At 40 columns the title has disappeared (see
        // title_truncated_then_removed_when_space_is_short), but the
        // launcher button, workspaces and the clock are all still shown
        // whole (1 + 16 + 15 cells, 2 separators, 2 margins). At 39 the
        // button goes too (last of REMOVAL); workspaces and the clock,
        // never removed, stay whole.
        let t = bar().grid("HDMI-A-1", 40).text();
        assert!(t.starts_with(" \u{f0349} │ ") && t.contains(" 4!····+ ") && t.ends_with(" 14:05 "), "{t}");
        let t = bar().grid("HDMI-A-1", 39).text();
        assert!(!t.contains('\u{f0349}') && t.contains(" 4!····+ ") && t.ends_with(" 14:05 "), "{t}");
    }

    #[test]
    fn workspaces_of_each_screen() {
        let b = bar();
        let g = b.grid("eDP-1", 60);
        assert!(g.text().starts_with(" \u{f0349} │  1  │ "), "{}", g.text());
        assert!(g.styles().starts_with("..mmmsss"), "visible without focus: surface");
        let g = b.grid("DVI-I-1", 60);
        assert!(g.text().starts_with(" \u{f0349} │  3··  web·  │ "), "{}", g.text());
        let t = b.grid("HEADLESS-1", 120).text();
        assert!(t.contains(" 1 ") && t.contains(" web· "), "unknown screen: every workspace ({t})");
    }

    #[test]
    fn the_clock_has_a_zone_only_with_the_calendar() {
        let clock = Target::Module(ModuleId::Clock);
        let g = bar().grid("HDMI-A-1", 100);
        let t = g.text();
        let start = t.chars().count() as u16 - " Sun 4 Oct 14:05 ".chars().count() as u16 + 1;
        assert_eq!(g.target_at(start), Some(&clock), "the date's first cell: {t}");
        assert_eq!(g.target_at(98), Some(&clock), "the time's last cell");
        assert_eq!(g.target_at(start - 1), None, "the space before the date");
        assert_eq!(g.target_at(99), None, "the margin");
        let mut b = bar();
        b.config.clock.calendar = false;
        let g = b.grid("HDMI-A-1", 100);
        assert!(g.zones().iter().all(|z| z.target != clock), "no calendar: the clock is no zone");
    }

    #[test]
    fn workspace_zones() {
        let g = bar().grid("HDMI-A-1", 100);
        let two = Target::Workspace { reference: "2".into(), output: "HDMI-A-1".into() };
        let four = Target::Workspace { reference: "4".into(), output: "HDMI-A-1".into() };
        let five = Target::Workspace { reference: "5".into(), output: "HDMI-A-1".into() };
        let launcher = Target::Module(ModuleId::Launcher);
        // Edges of each zone (launcher button at 1, "2·" at 5-8, "4!····+"
        // at 9-17, "5" at 18-20): a bound off by one cell would let clicks
        // through or cut them off.
        assert_eq!(g.target_at(0), None, "margin");
        assert_eq!(g.target_at(1), Some(&launcher), "the launcher button");
        assert_eq!(g.target_at(2), None, "separator after the button");
        assert_eq!(g.target_at(4), None, "separator after the button");
        assert_eq!(g.target_at(5), Some(&two), "first edge of workspace 2");
        assert_eq!(g.target_at(8), Some(&two), "last edge of workspace 2");
        assert_eq!(g.target_at(9), Some(&four), "first edge of workspace 4");
        assert_eq!(g.target_at(10), Some(&four));
        assert_eq!(g.target_at(17), Some(&four), "last edge of workspace 4");
        assert_eq!(g.target_at(20), Some(&five), "last edge of workspace 5");
        assert_eq!(g.target_at(21), None, "on the separator, no zone");
        assert_eq!(g.target_at(34), None, "title");
        let g = bar().grid("DVI-I-1", 60);
        let web = Target::Workspace { reference: "name:web".into(), output: "DVI-I-1".into() };
        assert_eq!(g.target_at(11), Some(&web));
        // Unknown screen: each workspace keeps ITS OWN output in the command.
        let g = bar().grid("HEADLESS-1", 120);
        assert_eq!(g.target_at(6), Some(&Target::Workspace { reference: "1".into(), output: "eDP-1".into() }));
    }

    #[test]
    fn order_from_bar_toml() {
        let mut b = bar();
        b.config.modules = vec![ModuleId::Clock, ModuleId::Workspaces];
        let t = b.grid("HDMI-A-1", 60).text();
        assert!(t.starts_with(" Sun 4 Oct 14:05"), "{t}");
        assert!(t.ends_with(" │  2·  4!····+  5  "), "free space before the last module: {t}");
        b.config.clock.date = String::new();
        assert!(b.grid("HDMI-A-1", 60).text().starts_with(" 14:05"));
    }

    #[test]
    fn battery_config_reaches_the_segment() {
        // `bar.config.battery` (bar.toml's `[battery]`) must be the one
        // `grid()` actually hands to `segments::battery`, not a default
        // built on the fly.
        let mut b = bar();
        let mouse = Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: Some(80), low: false };
        b.apply(Msg::Battery(Some(PowerState { laptop: None, peripherals: vec![mouse] })));
        assert!(b.grid("HDMI-A-1", 100).text().contains('%'), "default [battery] peripherals = \"always\": shown");
        b.config.battery = BatteryConfig { peripherals: BatteryVisibility::Never, ..BatteryConfig::default() };
        assert!(!b.grid("HDMI-A-1", 100).text().contains('%'), "[battery] peripherals = \"never\": hidden");
    }

    #[test]
    fn waiting_and_down() {
        let b = Bar::new(BarConfig::default());
        assert!(b.grid("eDP-1", 40).text().starts_with(" \u{f0349} │  …  │ …"), "nothing received: \"…\"");
        let mut b = bar();
        b.apply(Msg::Compositor(None));
        let t = b.grid("eDP-1", 40).text();
        assert!(t.starts_with(" \u{f0349} │  –  │ –"), "source down: \"–\" ({t})");
        assert!(t.contains("14:05"), "the rest of the bar carries on");
    }

    #[test]
    fn launcher_button_on_every_output_and_dropped_last() {
        let b = bar();
        for output in ["eDP-1", "HDMI-A-1", "DVI-I-1", "HEADLESS-1"] {
            for w in [60u16, 100] {
                let g = b.grid(output, w);
                assert_eq!(g.cells()[1].ch, crate::segments::LAUNCHER, "{output} at {w}: {}", g.text());
                assert_eq!(g.target_at(1), Some(&Target::Module(ModuleId::Launcher)), "{output} at {w}");
            }
        }
        let g = b.grid("HDMI-A-1", 40);
        assert_eq!(g.target_at(1), Some(&Target::Module(ModuleId::Launcher)), "40 columns: title gone, the button kept");
        let g = b.grid("HDMI-A-1", 39);
        assert!(!g.text().contains(crate::segments::LAUNCHER), "39 columns: the button dropped, after every other module: {}", g.text());
        assert!((0..39).all(|x| g.target_at(x) != Some(&Target::Module(ModuleId::Launcher))), "and its click zone");
        let mut b = bar();
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Clock];
        assert!(!b.grid("HDMI-A-1", 60).text().contains(crate::segments::LAUNCHER), "removed from bar.toml: gone");
    }

    #[test]
    fn hostile_title() {
        let mut e = example();
        e.window = Some(Window { class: "Alacritty".into(), title: format!("a\tb\n日本語 — 🦀 \u{1b}[31m{}", "x".repeat(300)) });
        let mut b = bar();
        b.apply(Msg::Compositor(Some(e)));
        for w in [30u16, 47, 48, 49, 200] {
            let g = b.grid("HDMI-A-1", w);
            assert_eq!(g.width(), w);
            let t = g.text();
            assert!(!t.contains(['\t', '\n', '\u{1b}']), "{t:?}");
        }
    }

    /// The window module's icon is purple under icons and full; the title only under full; a bar
    /// that has not heard from the theme yet (`Bar::new`) is delivery 1's look.
    #[test]
    fn the_window_title_takes_the_purple_by_module_colours() {
        let styles = |mc| {
            let mut b = bar();
            b.module_colors = mc;
            b.grid("HDMI-A-1", 100).styles()
        };
        let none = styles(ModuleColors::None);
        assert_eq!(bar().module_colors, ModuleColors::None);
        let (icons, full) = (styles(ModuleColors::Icons), styles(ModuleColors::Full));
        assert_eq!(none.matches('p').count(), 0);
        assert_eq!(icons.matches('p').count(), 1, "the glyph alone: {icons}");
        assert!(full.matches('p').count() > 10, "the glyph, the space and the title: {full}");
        assert_eq!(none.replace('p', "."), none);
        assert_eq!(icons.replace('p', "."), none, "only the icon cell differs");
        assert_eq!(full.replace('p', "."), none, "only the tints differ, never a character");
    }

    #[test]
    fn modules_not_yet_delivered_are_invisible() {
        let mut b = bar();
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Volume, ModuleId::Tray, ModuleId::Clock];
        let t = b.grid("HDMI-A-1", 60).text();
        assert_eq!(t.matches('│').count(), 1, "no separator for a missing module: {t}");
    }

    #[test]
    fn no_empty_segment_without_window_or_known_workspace() {
        // With no active window: the window module must leave no empty
        // segment (so no "│ <empty> │").
        let mut e = example();
        e.window = None;
        let mut b = bar();
        b.apply(Msg::Compositor(Some(e)));
        let t = b.grid("HDMI-A-1", 60).text();
        assert_eq!(t.matches('│').count(), 2, "launcher, workspaces and clock only: {t}");

        // With no workspace at all known to the compositor: same thing for
        // the workspaces module.
        let e2 = CompositorState {
            workspaces: Vec::new(),
            window: Some(Window { class: "firefox".into(), title: "Page".into() }),
            toplevels: Vec::new(),
        };
        let mut b2 = bar();
        b2.apply(Msg::Compositor(Some(e2)));
        let t2 = b2.grid("HDMI-A-1", 60).text();
        assert_eq!(t2.matches('│').count(), 2, "launcher, window and clock only: {t2}");

        // While waiting or down, "…" / "–" stay shown for both modules
        // (covered by `waiting_and_down`), never removed.
    }

    /// Launcher, workspaces, window, volume, network, battery, clock — the
    /// module set from Mae's bug report (mae-feedback.md item 3): with no
    /// active window, the only elastic segment vanishes, and the free
    /// space it used to carry must keep tracking its slot in the order.
    fn bar_with_right_modules() -> Bar {
        let mut b = bar();
        b.config.modules = vec![
            ModuleId::Launcher,
            ModuleId::Workspaces,
            ModuleId::Window,
            ModuleId::Volume,
            ModuleId::Network,
            ModuleId::Battery,
            ModuleId::Clock,
        ];
        b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 55, muted: false }), ..SoundState::default() })));
        b.apply(Msg::Network(Some(NetState::Online { link: Link::Wifi, vpns: 0, limited: false })));
        b.apply(Msg::Battery(Some(PowerState {
            laptop: Some(Battery { percent: 80, charge: Charge::Discharging }),
            peripherals: Vec::new(),
        })));
        b
    }

    /// The column of `target`'s first cell, and the text from there to the
    /// end of the grid (by cell, not by byte: a column does not line up
    /// with a byte offset into `Grid::text()` once a wide character has
    /// been written before it).
    fn tail_from(g: &Grid, target: &Target) -> (u16, String) {
        let start = (0..g.width()).find(|&x| g.target_at(x) == Some(target)).expect("segment not found");
        let tail = g.cells()[start as usize..].iter().filter(|c| c.ch != crate::grid::CONT).map(|c| c.ch).collect();
        (start, tail)
    }

    #[test]
    fn with_no_active_window_the_right_hand_modules_stay_right() {
        let b = bar_with_right_modules(); // bar() already sets an active window (example())
        let volume = Target::Module(ModuleId::Volume);

        let with_window = b.grid("HDMI-A-1", 100);
        let (start_with, tail_with) = tail_from(&with_window, &volume);

        let mut b2 = bar_with_right_modules();
        let mut e = example();
        e.window = None;
        b2.apply(Msg::Compositor(Some(e)));
        let without_window = b2.grid("HDMI-A-1", 100);
        let (start_without, tail_without) = tail_from(&without_window, &volume);

        assert_eq!(without_window.width(), with_window.width());
        assert_eq!(start_without, start_with, "volume starts at the same column either way: {}", without_window.text());
        assert_eq!(tail_without, tail_with, "volume…clock, right-aligned exactly as with a window");
        assert_eq!(
            without_window.text().matches('│').count(),
            with_window.text().matches('│').count() - 1,
            "one separator fewer: no empty window segment ({})",
            without_window.text()
        );
        insta::assert_snapshot!("no_active_window_right_modules", snapshot(&without_window));
    }

    #[test]
    fn narrow_screen_keeps_the_gap_at_the_window_slot_as_modules_are_removed() {
        // Narrow screens drop modules by priority (REMOVAL: here, network
        // is the first of this module set to go) on top of the window
        // module already having no segment. The free space must keep
        // tracking the window's configured slot through that shrinking,
        // not fall back to "before the last segment" as soon as anything
        // else disappears.
        let mut b = bar_with_right_modules();
        let mut e = example();
        e.window = None;
        b.apply(Msg::Compositor(Some(e)));

        let narrow = (20..100)
            .rev()
            .find(|&w| {
                let t = b.grid("HDMI-A-1", w).text();
                !t.contains(crate::segments::WIFI) && t.contains('%')
            })
            .expect("no width drops exactly the network module while keeping volume and battery");
        let got = b.grid("HDMI-A-1", narrow);
        assert_eq!(
            got.text().matches('│').count(),
            4,
            "launcher, workspaces, volume, battery, clock (network dropped, window absent): {}",
            got.text()
        );

        // Same surviving modules, built directly without the window module
        // in the configuration at all: the old "before the last segment"
        // fallback applies there. The fix's gap must land somewhere else
        // (right after workspaces), not degrade back to that fallback the
        // moment a second module is also missing.
        let mut reference = b.clone();
        reference.config.modules = vec![ModuleId::Launcher, ModuleId::Workspaces, ModuleId::Volume, ModuleId::Battery, ModuleId::Clock];
        let old_rule = reference.grid("HDMI-A-1", narrow);

        let volume = Target::Module(ModuleId::Volume);
        let (vol_at_got, _) = tail_from(&got, &volume);
        let (vol_at_old_rule, _) = tail_from(&old_rule, &volume);
        assert!(
            vol_at_got > vol_at_old_rule,
            "volume still pushed right by the gap at the window's slot, not stuck against workspaces like the old-rule fallback: got {} vs old-rule {} ({})",
            vol_at_got,
            vol_at_old_rule,
            got.text()
        );
    }

    // ---- groups and per-style spacing (shapes spec § 2) ----

    const BLOCK_STYLES: [BarStyle; 4] = [BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Translucent];

    fn styled(mut b: Bar, style: BarStyle) -> Bar {
        b.style = style;
        b
    }

    fn group(kind: GroupKind, start: u16, end: u16, hue: Option<Hue>) -> Group {
        Group { kind, start, end, hue }
    }

    /// `flat` records no block: its grid is exactly today's (the snapshots above), groups included.
    #[test]
    fn flat_records_no_group() {
        for w in [100u16, 60, 40] {
            let g = bar().grid("HDMI-A-1", w);
            assert_eq!(g.groups(), &[], "{w} columns");
        }
        assert_eq!(bar_with_right_modules().grid("HDMI-A-1", 100).groups(), &[]);
    }

    /// `underline` keeps flat's cells and zones to the cell; it only records the blocks, each from
    /// its first module's first cell to its last module's last.
    #[test]
    fn underline_keeps_the_flat_cells_and_records_the_blocks() {
        for b in [bar(), bar_with_right_modules()] {
            for w in [100u16, 60, 40, 20] {
                let (flat, under) = (b.grid("HDMI-A-1", w), styled(b.clone(), BarStyle::Underline).grid("HDMI-A-1", w));
                assert_eq!((under.text(), under.styles(), under.zones()), (flat.text(), flat.styles(), flat.zones()), "{w}");
            }
        }
        let g = styled(bar_with_right_modules(), BarStyle::Underline).grid("HDMI-A-1", 100);
        assert_eq!(
            g.groups(),
            &[
                group(GroupKind::Launcher, 1, 2, None),
                group(GroupKind::Workspaces, 5, 21, None),
                group(GroupKind::Title, 24, 52, Some(Hue::Purple)),
                group(GroupKind::Status, 56, 81, Some(Hue::Blue)),
                group(GroupKind::Clock, 84, 99, None),
            ],
            "{}",
            g.text()
        );
    }

    /// The block styles: no "│", a pad cell inside each end of a block, one blank cell between
    /// blocks, the title's block around its text only (the free space stays outside it).
    #[test]
    fn block_styles_drop_the_separators_and_pad_each_block() {
        let g = styled(bar(), BarStyle::Islands).grid("HDMI-A-1", 100);
        let t = g.text();
        assert!(!t.contains('│'), "{t}");
        assert!(t.starts_with("  \u{f0349}    2·  4!····+  5    \u{f0239} Web page — Mozilla Firefox "), "{t}");
        assert!(t.ends_with(" Sun 4 Oct 14:05  "), "the clock's pad, then the margin: {t}");
        assert_eq!(
            g.groups(),
            &[
                group(GroupKind::Launcher, 1, 4, None),
                group(GroupKind::Workspaces, 5, 23, None),
                group(GroupKind::Title, 24, 54, Some(Hue::Purple)),
                group(GroupKind::Clock, 82, 99, None),
            ]
        );
        assert_eq!(g.split(), 81, "the title's free space and closing pad end where the right-hand side starts");
        assert_eq!(g.target_at(2), Some(&Target::Module(ModuleId::Launcher)), "the zones follow the cells");
        assert_eq!(g.target_at(6), Some(&Target::Workspace { reference: "2".into(), output: "HDMI-A-1".into() }));
        insta::assert_snapshot!("blocks_hdmi_100", snapshot(&g));
        for style in BLOCK_STYLES {
            let other = styled(bar(), style).grid("HDMI-A-1", 100);
            assert_eq!(other, g, "{style:?}: every block style lays out alike, only the painter differs");
        }
    }

    /// Consecutive modules of one kind share one block, two blank cells apart; its hue is its
    /// first module's.
    #[test]
    fn a_block_holds_consecutive_modules_of_one_kind() {
        let g = styled(bar_with_right_modules(), BarStyle::Islands).grid("HDMI-A-1", 100);
        let status: Vec<&Group> = g.groups().iter().filter(|x| x.kind == GroupKind::Status).collect();
        assert_eq!(status, [&group(GroupKind::Status, 56, 81, Some(Hue::Blue))], "volume, network and battery: one block");
        let t: String = g.cells()[56..81].iter().filter(|c| c.ch != crate::grid::CONT).map(|c| c.ch).collect();
        assert_eq!(t, " \u{f0580} ▁▃▅▆▇▇  55%  \u{f05a9}  \u{f0081} 80% ");
        insta::assert_snapshot!("blocks_right_modules_100", snapshot(&g));
        // Without volume, the block starts with the network: cyan.
        let mut b = styled(bar_with_right_modules(), BarStyle::Islands);
        b.config.modules.retain(|m| *m != ModuleId::Volume);
        let g = b.grid("HDMI-A-1", 100);
        assert_eq!(g.groups().iter().find(|x| x.kind == GroupKind::Status).map(|x| x.hue), Some(Some(Hue::Cyan)));
    }

    /// No block spans the free space: the window module's slot between two status modules splits
    /// them into two blocks, and the right-hand side starts after it.
    #[test]
    fn the_free_space_splits_a_block() {
        let mut b = styled(bar_with_right_modules(), BarStyle::Islands);
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Volume, ModuleId::Window, ModuleId::Network, ModuleId::Clock];
        let mut e = example();
        e.window = None;
        b.apply(Msg::Compositor(Some(e)));
        let g = b.grid("HDMI-A-1", 100);
        let kinds: Vec<GroupKind> = g.groups().iter().map(|x| x.kind).collect();
        assert_eq!(kinds, [GroupKind::Workspaces, GroupKind::Status, GroupKind::Status, GroupKind::Clock], "{}", g.text());
        let (left, right) = (g.groups()[1], g.groups()[2]);
        assert!(left.end < g.split() && right.start >= g.split(), "{left:?} {right:?} split {}", g.split());
        assert_eq!((left.hue, right.hue), (Some(Hue::Blue), Some(Hue::Cyan)));
    }

    /// Every width, every style: the bar is exactly as wide as asked, the workspaces and the clock
    /// stay whole (the block styles' spacing is counted before a module is removed: the clock's pad
    /// and the margin end the bar), and the modules leave in REMOVAL's order.
    #[test]
    fn narrow_outputs_drop_modules_in_order_under_every_style() {
        // This bar's removable modules, in REMOVAL's order.
        let removable = [ModuleId::Window, ModuleId::Network, ModuleId::Volume, ModuleId::Battery, ModuleId::Launcher];
        assert!(removable.windows(2).all(|p| REMOVAL.iter().position(|m| *m == p[0]) < REMOVAL.iter().position(|m| *m == p[1])));
        for style in BarStyle::ALL {
            let b = styled(bar_with_right_modules(), style);
            let mut removed_at = Vec::new();
            for w in (45u16..=140).rev() {
                let g = b.grid("HDMI-A-1", w);
                let t = g.text();
                assert_eq!(g.width(), w, "{style:?}");
                let tail = if spacing(style) == Spacing::Blocks { " 14:05  " } else { " 14:05 " };
                assert!(t.ends_with(tail) && t.contains(" 4!····+ "), "{style:?} at {w}: {t}");
                let shown = |m: ModuleId| match m {
                    ModuleId::Window => t.contains("Web"),
                    m => g.zones().iter().any(|z| z.target == Target::Module(m)),
                };
                let gone = removable.iter().filter(|m| !shown(**m)).count();
                assert!(removable[gone..].iter().all(|m| shown(*m)), "{style:?} at {w}: out of REMOVAL's order: {t}");
                // Review focus 3: a block always holds something; no lone border around blank cells.
                for x in g.groups() {
                    let cells = &g.cells()[usize::from(x.start)..usize::from(x.end)];
                    assert!(cells.iter().any(|c| c.ch != ' '), "{style:?} at {w}: an empty block {x:?}: {t}");
                }
                while removed_at.len() < gone {
                    removed_at.push(w);
                }
            }
            assert_eq!(removed_at.len(), 4, "{style:?}: all but the launcher leave by 45 columns ({removed_at:?})");
        }
    }

    /// Left blocks end at or before the split and right blocks start at or after it, a long
    /// (truncated) title included: powerline reads the side of a block from it.
    #[test]
    fn blocks_sit_on_one_side_of_the_split_even_with_a_truncated_title() {
        for style in BLOCK_STYLES {
            for title in ["Web page", &"very long title ".repeat(12)] {
                let mut b = styled(bar_with_right_modules(), style);
                let mut e = example();
                e.window.as_mut().unwrap().title = title.to_string();
                b.apply(Msg::Compositor(Some(e)));
                for w in [100u16, 80, 70] {
                    let g = b.grid("HDMI-A-1", w);
                    for x in g.groups() {
                        let left = matches!(x.kind, GroupKind::Launcher | GroupKind::Workspaces | GroupKind::Title);
                        assert!(
                            if left { x.end <= g.split() } else { x.start >= g.split() },
                            "{style:?} {w}: {x:?} split {} in {}",
                            g.split(),
                            g.text()
                        );
                    }
                }
            }
        }
    }

    // ---- the media module (media spec § 1) ----

    /// The media module between the title and the volume (its place in `ModuleId::ALL`).
    fn bar_with_media(playing: bool) -> Bar {
        let mut b = bar();
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Media, ModuleId::Volume, ModuleId::Clock];
        b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
        b.apply(Msg::Media(Some(MediaView { playing, title: "Bohemian Rhapsody".into() })));
        b
    }

    #[test]
    fn media_sits_between_the_title_and_the_volume() {
        let g = bar_with_media(true).grid("HDMI-A-1", 100);
        let t = g.text();
        assert!(t.contains(" │ \u{f040a} Bohemian Rhapsody │ \u{f057e} ▁▃▅▆▇▇  84% │ Sun 4 Oct 14:05 "), "{t}");
        let media = Target::Module(ModuleId::Media);
        let (start, _) = tail_from(&g, &media);
        assert_eq!(g.target_at(start + 18), Some(&media), "its last cell: 19 cells, one zone");
        assert_eq!((g.target_at(start - 1), g.target_at(start + 19)), (None, None), "the separators around it");
        insta::assert_snapshot!("media_flat_100", snapshot(&g));
        let mut gone = bar_with_media(true);
        gone.apply(Msg::Media(None));
        let mut absent = bar_with_media(true);
        absent.config.modules.retain(|m| *m != ModuleId::Media);
        assert_eq!(gone.grid("HDMI-A-1", 100), absent.grid("HDMI-A-1", 100), "no player: exactly the bar without the module");
    }

    #[test]
    fn media_is_a_block_of_its_own_in_purple() {
        for style in [BarStyle::Underline, BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Translucent] {
            let g = styled(bar_with_media(true), style).grid("HDMI-A-1", 100);
            let kinds: Vec<GroupKind> = g.groups().iter().map(|x| x.kind).collect();
            assert_eq!(
                kinds,
                [GroupKind::Workspaces, GroupKind::Title, GroupKind::Media, GroupKind::Status, GroupKind::Clock],
                "{style:?}"
            );
            let (media, status) = (g.groups()[2], g.groups()[3]);
            assert_eq!((media.hue, status.hue), (Some(Hue::Purple), Some(Hue::Blue)), "{style:?}: the volume opens a block of its own");
            let cells: String = g.cells()[usize::from(media.start)..usize::from(media.end)]
                .iter()
                .filter(|c| c.ch != crate::grid::CONT)
                .map(|c| c.ch)
                .collect();
            let want = if style == BarStyle::Underline { "\u{f040a} Bohemian Rhapsody" } else { " \u{f040a} Bohemian Rhapsody " };
            assert_eq!(cells, want, "{style:?}: the block wraps the mark and the title, its pads included");
        }
        let g = styled(bar_with_media(true), BarStyle::Islands).grid("HDMI-A-1", 100);
        insta::assert_snapshot!("media_islands_100", snapshot(&g));
    }

    #[test]
    fn a_paused_media_block_takes_no_hue_even_with_pills() {
        for style in BLOCK_STYLES {
            for (playing, want) in [(true, Some(Hue::Purple)), (false, None)] {
                let mut b = styled(bar_with_media(playing), style);
                b.module_colors = ModuleColors::Pills;
                let g = b.grid("HDMI-A-1", 100);
                let media = g.groups().iter().find(|x| x.kind == GroupKind::Media).expect("a media block");
                assert_eq!(media.hue, want, "{style:?}, playing {playing}: a paused block stays neutral");
            }
        }
    }

    #[test]
    fn media_leaves_after_the_tray_and_before_the_title() {
        let at = |m: ModuleId| REMOVAL.iter().position(|r| *r == m);
        assert!(at(ModuleId::Tray) < at(ModuleId::Media) && at(ModuleId::Media) < at(ModuleId::Window));
        let b = bar_with_media(true);
        let shown = |w: u16| {
            let t = b.grid("HDMI-A-1", w).text();
            (t.contains(crate::segments::PLAY), t.contains("Web"))
        };
        let gone = (20u16..=100).rev().find(|&w| !shown(w).0).expect("narrow enough, the media leaves");
        assert_eq!(shown(gone), (false, true), "the title is still there at {gone} columns");
        for w in 20u16..=100 {
            let (media, title) = shown(w);
            assert!(!media || title, "at {w}: the media outlived the title");
        }
    }

    // ---- the bell (notifications spec § 4) ----

    const THREE: NotifyView = NotifyView { unread: 3, dnd: false, serving: true };

    /// The bell between the volume and the clock (its place in `ModuleId::ALL`).
    fn bar_with_bell(view: NotifyView) -> Bar {
        let mut b = bar();
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Volume, ModuleId::Notifications, ModuleId::Clock];
        b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
        b.apply(Msg::Notifications(view));
        b
    }

    #[test]
    fn the_bell_sits_between_the_volume_and_the_clock() {
        let g = bar_with_bell(THREE).grid("HDMI-A-1", 100);
        let t = g.text();
        assert!(t.contains(" │ \u{f057e} ▁▃▅▆▇▇  84% │ \u{f009a} 3 │ Sun 4 Oct 14:05 "), "{t}");
        let bell = Target::Module(ModuleId::Notifications);
        let (start, _) = tail_from(&g, &bell);
        assert_eq!(g.target_at(start + 2), Some(&bell), "its last cell: 3 cells, one zone");
        assert_eq!((g.target_at(start - 1), g.target_at(start + 3)), (None, None), "the separators around it");
        insta::assert_snapshot!("bell_flat_100", snapshot(&g));
        let mut unheard = bar_with_bell(THREE);
        unheard.notifications = None;
        let mut absent = bar_with_bell(THREE);
        absent.config.modules.retain(|m| *m != ModuleId::Notifications);
        assert_eq!(unheard.grid("HDMI-A-1", 100), absent.grid("HDMI-A-1", 100), "nothing heard: exactly the bar without the module");
    }

    #[test]
    fn the_bell_joins_the_status_block_and_takes_no_hue() {
        for style in [BarStyle::Underline, BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Translucent] {
            let g = styled(bar_with_bell(THREE), style).grid("HDMI-A-1", 100);
            let kinds: Vec<GroupKind> = g.groups().iter().map(|x| x.kind).collect();
            assert_eq!(kinds, [GroupKind::Workspaces, GroupKind::Title, GroupKind::Status, GroupKind::Clock], "{style:?}");
            let status = g.groups()[2];
            assert_eq!(status.hue, Some(Hue::Blue), "{style:?}: the volume opens the block");
            let cells: String = g.cells()[usize::from(status.start)..usize::from(status.end)]
                .iter()
                .filter(|c| c.ch != crate::grid::CONT)
                .map(|c| c.ch)
                .collect();
            assert!(
                cells.contains('\u{f057e}') && (cells.ends_with("\u{f009a} 3") || cells.ends_with("\u{f009a} 3 ")),
                "{style:?}: {cells:?}"
            );
        }
        for mc in [ModuleColors::None, ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills] {
            let mut b = styled(bar_with_bell(THREE), BarStyle::Islands);
            b.config.modules = vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Notifications, ModuleId::Volume, ModuleId::Clock];
            b.module_colors = mc;
            let g = b.grid("HDMI-A-1", 100);
            let status = g.groups().iter().find(|x| x.kind == GroupKind::Status).expect("a status block");
            assert_eq!(status.hue, None, "{mc:?}: the bell opens the block, which stays neutral");
        }
        insta::assert_snapshot!("bell_islands_100", snapshot(&styled(bar_with_bell(THREE), BarStyle::Islands).grid("HDMI-A-1", 100)));
    }

    #[test]
    fn the_bell_is_never_dropped_on_a_narrow_output() {
        assert!(!REMOVAL.contains(&ModuleId::Notifications));
        let b = bar_with_bell(THREE);
        let volume_gone =
            (30u16..=100).rev().find(|&w| !b.grid("HDMI-A-1", w).text().contains('\u{f057e}')).expect("narrow enough, the volume leaves");
        assert!(volume_gone >= 50, "the volume leaves at {volume_gone} columns");
        // Down to the narrowest bar that still holds the whole clock: workspaces, bell and clock.
        let floor = (0u16..=100).find(|&w| b.grid("HDMI-A-1", w).text().contains("Sun 4 Oct 14:05")).expect("the clock fits somewhere");
        assert!(floor < 50, "the check goes below 50 columns: {floor}");
        let t = b.grid("HDMI-A-1", floor).text();
        assert!(!t.contains('\u{f057e}'), "at {floor} only the workspaces, the bell and the clock: {t}");
        for w in floor..=100 {
            let t = b.grid("HDMI-A-1", w).text();
            assert!(t.contains("\u{f009a} 3"), "{w}: the bell stays: {t}");
        }
    }

    /// Below the minimum width the clipped text may vanish: no block is left holding only blanks.
    #[test]
    fn no_block_holds_only_blanks_at_any_width() {
        for style in BLOCK_STYLES {
            let b = styled(bar_with_right_modules(), style);
            for w in 0u16..45 {
                let g = b.grid("HDMI-A-1", w);
                for x in g.groups() {
                    let cells = &g.cells()[usize::from(x.start)..usize::from(x.end)];
                    assert!(cells.iter().any(|c| c.ch != ' '), "{style:?} at {w}: {x:?} in {:?}", g.text());
                }
            }
        }
    }

    /// The OSD is a module without a segment (OSD spec § 4): the bar is exactly the same without it.
    #[test]
    fn the_osd_module_draws_nothing_in_the_bar() {
        let mut with = bar();
        assert!(with.config.modules.contains(&ModuleId::Osd), "on by default");
        with.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
        let mut without = with.clone();
        without.config.modules.retain(|m| *m != ModuleId::Osd);
        // Flat, and a block style: no empty block either.
        for style in [BarStyle::Flat, BarStyle::Islands] {
            let (with, without) = (styled(with.clone(), style), styled(without.clone(), style));
            for width in [100, 60, 40] {
                assert_eq!(
                    with.grid("HDMI-A-1", width),
                    without.grid("HDMI-A-1", width),
                    "{style:?}, {width} cells: no segment, no separator, no gap"
                );
            }
        }
    }

    /// The quick panel is a popup of its own (quick spec § 1): its module draws nothing in the bar.
    #[test]
    fn the_quick_module_draws_nothing_in_the_bar() {
        let mut with = bar();
        assert!(with.config.modules.contains(&ModuleId::Quick), "on by default");
        with.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
        let mut without = with.clone();
        without.config.modules.retain(|m| *m != ModuleId::Quick);
        for style in [BarStyle::Flat, BarStyle::Islands] {
            let (with, without) = (styled(with.clone(), style), styled(without.clone(), style));
            for width in [100, 60, 40] {
                assert_eq!(with.grid("HDMI-A-1", width), without.grid("HDMI-A-1", width), "{style:?}, {width} cells");
            }
        }
        assert_eq!(module_hue(ModuleId::Quick), None);
    }
}
