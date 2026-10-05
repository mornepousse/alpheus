//! From state to a screen's grid. Each module produces a segment; segments
//! are laid out left to right in bar.toml order, separated by "│". The
//! window module is elastic: it takes the free space. With no active
//! window, the window module has no segment at all (no empty "│ │"), but
//! the free space still goes where it sits in the configured order — just
//! before the segment that follows it — so the right-hand modules stay
//! right-aligned instead of collapsing left; only with the window module
//! absent from the configuration entirely does the free space fall back to
//! preceding the last module. Narrow screen: modules disappear by priority
//! (resources, media, title, then Bluetooth, network, volume, battery, and
//! last the launcher button); workspaces and the clock always stay.

use crate::compositor::{CompositorState, Workspace};
use crate::config::ModuleId;
use crate::grid::{Grid, Style, Target, text_width, truncate};
use crate::state::{Bar, Feed};
use crate::{clock, icons, segments};

/// Order of disappearance when space runs short: the spec's (resources,
/// media, title), then the system indicators (Q9), and the launcher button
/// last (Mae, 2026-10-05: `Mod+Space` still opens the launcher). Workspaces
/// and the clock always stay.
const REMOVAL: [ModuleId; 8] = [
    ModuleId::Resources,
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
        Style::ERROR
    } else if w.focused {
        Style::INVERTED
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
    Some(Segment { module: ModuleId::Workspaces, pieces, elastic: false, target: None })
}

/// `None` with no active window: no empty segment (no "│ <empty> │"),
/// rather than an empty text piece.
fn window(state: &Feed<CompositorState>) -> Option<Segment> {
    let text = match state {
        Feed::Waiting => "…".to_string(),
        Feed::Down => "–".to_string(),
        Feed::Ready(CompositorState { window: Some(w), .. }) => format!("{} {}", icons::glyph(&w.class), w.title),
        Feed::Ready(_) => return None,
    };
    Some(Segment { module: ModuleId::Window, pieces: vec![piece(text, Style::NORMAL)], elastic: true, target: None })
}

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
    Segment { module: ModuleId::Clock, pieces, elastic: false, target: None }
}

/// A module's segment, or None if it has nothing to show.
fn segment(bar: &Bar, m: ModuleId, output: &str) -> Option<Segment> {
    match m {
        ModuleId::Launcher => Some(segments::launcher()),
        ModuleId::Workspaces => workspaces(&bar.compositor, output),
        ModuleId::Window => window(&bar.compositor),
        ModuleId::Clock => Some(bar_clock(bar)),
        ModuleId::Volume => segments::volume(&bar.sound),
        ModuleId::Network => segments::network(&bar.network),
        ModuleId::Bluetooth => segments::bluetooth(&bar.bluetooth),
        ModuleId::Battery => segments::battery(&bar.battery, &bar.config.battery),
        ModuleId::Resources => segments::resources(&bar.resources, &bar.config.resources),
        // Deliveries 4 and 5: not shown yet.
        ModuleId::Media | ModuleId::Tray | ModuleId::Notifications => None,
    }
}

fn separators(n: usize) -> u16 {
    text_width(SEPARATOR).saturating_mul(n.saturating_sub(1) as u16)
}

/// Minimum width: margins, separators, segments (title at MIN_TITLE).
fn needed(segs: &[Segment]) -> u16 {
    segs.iter().map(Segment::min_width).fold(2 * MARGIN + separators(segs.len()), u16::saturating_add)
}

impl Bar {
    pub fn grid(&self, output: &str, columns: u16) -> Grid {
        let mut segs: Vec<Segment> = self.config.modules.iter().filter_map(|m| segment(self, *m, output)).collect();
        for removed in REMOVAL {
            if needed(&segs) <= columns {
                break;
            }
            segs.retain(|s| s.module != removed);
        }
        let gap_at = self.window_gap(&segs);
        lay_out(&segs, columns, gap_at)
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

pub(crate) fn lay_out(segs: &[Segment], columns: u16, gap_at: Option<usize>) -> Grid {
    let mut g = Grid::empty(columns);
    let end = columns.saturating_sub(MARGIN);
    let fixed = segs.iter().filter(|s| !s.elastic).map(Segment::width).fold(2 * MARGIN + separators(segs.len()), u16::saturating_add);
    let free = columns.saturating_sub(fixed);
    let has_elastic = segs.iter().any(|s| s.elastic);
    // Without an elastic segment, the free space goes where `gap_at` says
    // (the absent window module's configured position), or, lacking that,
    // before the last segment.
    let space_before = if has_elastic || segs.len() < 2 {
        None
    } else {
        match gap_at {
            Some(i) if i < segs.len() => Some(i),
            _ => Some(segs.len() - 1),
        }
    };
    let mut x = MARGIN;
    for (i, s) in segs.iter().enumerate() {
        if space_before == Some(i) {
            x = x.saturating_add(free);
        }
        if i > 0 {
            x = g.write(x, end, SEPARATOR, Style::MUTED);
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
        if s.elastic {
            x = limit;
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LocalTime;
    use crate::compositor::{Window, example};
    use crate::config::{BarConfig, BatteryConfig, BatteryVisibility, ModuleId};
    use crate::state::Msg;
    use crate::system::{Battery, Charge, Link, NetState, Peripheral, PeripheralKind, PowerState, SoundState, Volume};

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
        assert!(s.starts_with("..mmmAAAAeeeeeeeeemmmmmm"), "{s}");
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
        b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 55, muted: false }) })));
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
}
