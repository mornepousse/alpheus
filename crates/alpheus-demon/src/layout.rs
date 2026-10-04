//! From state to a screen's grid. Each module produces a segment; segments
//! are laid out left to right in bar.toml order, separated by "│". The
//! window module is elastic: it takes the free space (without it, the free
//! space precedes the last module). Narrow screen: modules disappear by
//! priority (resources, media, then title); workspaces and the clock
//! always stay.

use crate::compositor::{CompositorState, Workspace};
use crate::config::ModuleId;
use crate::grid::{Grid, Style, Target, text_width, truncate};
use crate::state::{Bar, Feed};
use crate::{clock, icons};

/// Order of disappearance when space runs short.
const REMOVAL: [ModuleId; 3] = [ModuleId::Resources, ModuleId::Media, ModuleId::Window];
/// Minimum title width before it disappears.
const MIN_TITLE: u16 = 8;
/// Beyond this, a workspace's windows are summarized by "+" (Q4).
const MAX_DOTS: u16 = 4;
/// A longer workspace name is truncated.
const MAX_NAME: u16 = 12;
const SEPARATOR: &str = " │ ";
/// Margin of one cell at each end.
const MARGIN: u16 = 1;

struct Piece {
    text: String,
    style: Style,
    target: Option<Target>,
}

struct Segment {
    module: ModuleId,
    pieces: Vec<Piece>,
    elastic: bool,
}

impl Segment {
    fn width(&self) -> u16 {
        self.pieces.iter().map(|p| text_width(&p.text)).fold(0, u16::saturating_add)
    }

    fn min_width(&self) -> u16 {
        if self.elastic { MIN_TITLE } else { self.width() }
    }
}

fn piece(text: impl Into<String>, style: Style) -> Piece {
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
    Some(Segment { module: ModuleId::Workspaces, pieces, elastic: false })
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
    Some(Segment { module: ModuleId::Window, pieces: vec![piece(text, Style::NORMAL)], elastic: true })
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
    Segment { module: ModuleId::Clock, pieces, elastic: false }
}

/// A module's segment, or None if it has nothing to show.
fn segment(bar: &Bar, m: ModuleId, output: &str) -> Option<Segment> {
    match m {
        ModuleId::Workspaces => workspaces(&bar.compositor, output),
        ModuleId::Window => window(&bar.compositor),
        ModuleId::Clock => Some(bar_clock(bar)),
        // Deliveries 3 to 5: not shown yet.
        ModuleId::Media
        | ModuleId::Volume
        | ModuleId::Network
        | ModuleId::Bluetooth
        | ModuleId::Battery
        | ModuleId::Resources
        | ModuleId::Tray
        | ModuleId::Notifications => None,
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
        lay_out(&segs, columns)
    }
}

fn lay_out(segs: &[Segment], columns: u16) -> Grid {
    let mut g = Grid::empty(columns);
    let end = columns.saturating_sub(MARGIN);
    let fixed = segs.iter().filter(|s| !s.elastic).map(Segment::width).fold(2 * MARGIN + separators(segs.len()), u16::saturating_add);
    let free = columns.saturating_sub(fixed);
    // Without an elastic segment, the free space precedes the last segment.
    let space_before = if segs.iter().any(|s| s.elastic) || segs.len() < 2 { None } else { Some(segs.len() - 1) };
    let mut x = MARGIN;
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            if space_before == Some(i) {
                x = x.saturating_add(free);
            }
            x = g.write(x, end, SEPARATOR, Style::MUTED);
        }
        let limit = if s.elastic { x.saturating_add(free).min(end) } else { end };
        for p in &s.pieces {
            let text = if s.elastic { truncate(&p.text, limit.saturating_sub(x)) } else { p.text.clone() };
            let start = x;
            x = g.write(x, limit, &text, p.style);
            if let Some(t) = &p.target {
                g.zone(start, x, t.clone());
            }
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
    use crate::config::BarConfig;
    use crate::state::Msg;

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
        assert!(t.starts_with("  2·  4!····+  5  │ \u{f0239} Web page — Mozilla Firefox "), "{t}");
        assert!(t.ends_with(" │ Sun 4 Oct 14:05 "), "{t}");
        assert!(s.starts_with(".AAAAeeeeeeeeemmmmmm"), "{s}");
        assert!(s.ends_with("mmmmmmmmmmmmmGGGGG."), "{s}");
        insta::assert_snapshot!("hdmi_100", snapshot(&g));
    }

    #[test]
    fn title_truncated_then_removed_when_space_is_short() {
        let g = bar().grid("HDMI-A-1", 60);
        assert!(g.text().contains("Mozilla…"), "{}", g.text());
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
        // At 39 columns the title has disappeared (see
        // title_truncated_then_removed_when_space_is_short), but workspaces
        // and the clock, never removed, are both still shown.
        let t = bar().grid("HDMI-A-1", 39).text();
        assert!(t.contains(" 4!····+ ") && t.contains("14:05"), "{t}");
    }

    #[test]
    fn workspaces_of_each_screen() {
        let b = bar();
        let g = b.grid("eDP-1", 60);
        assert!(g.text().starts_with("  1  │ "), "{}", g.text());
        assert!(g.styles().starts_with(".sss"), "visible without focus: surface");
        let g = b.grid("DVI-I-1", 60);
        assert!(g.text().starts_with("  3··  web·  │ "), "{}", g.text());
        let t = b.grid("HEADLESS-1", 120).text();
        assert!(t.contains(" 1 ") && t.contains(" web· "), "unknown screen: every workspace ({t})");
    }

    #[test]
    fn workspace_zones() {
        let g = bar().grid("HDMI-A-1", 100);
        let two = Target::Workspace { reference: "2".into(), output: "HDMI-A-1".into() };
        let four = Target::Workspace { reference: "4".into(), output: "HDMI-A-1".into() };
        let five = Target::Workspace { reference: "5".into(), output: "HDMI-A-1".into() };
        // Edges of each zone ("2·" at 1-4, "4!····+" at 5-13, "5" at 14-16):
        // a bound off by one cell would let clicks through or cut them off.
        assert_eq!(g.target_at(1), Some(&two), "first edge of workspace 2");
        assert_eq!(g.target_at(4), Some(&two), "last edge of workspace 2");
        assert_eq!(g.target_at(5), Some(&four), "first edge of workspace 4");
        assert_eq!(g.target_at(6), Some(&four));
        assert_eq!(g.target_at(13), Some(&four), "last edge of workspace 4");
        assert_eq!(g.target_at(16), Some(&five), "last edge of workspace 5");
        assert_eq!(g.target_at(17), None, "on the separator, no zone");
        assert_eq!(g.target_at(0), None, "margin");
        assert_eq!(g.target_at(30), None, "title");
        let g = bar().grid("DVI-I-1", 60);
        let web = Target::Workspace { reference: "name:web".into(), output: "DVI-I-1".into() };
        assert_eq!(g.target_at(7), Some(&web));
        // Unknown screen: each workspace keeps ITS OWN output in the command.
        let g = bar().grid("HEADLESS-1", 120);
        assert_eq!(g.target_at(2), Some(&Target::Workspace { reference: "1".into(), output: "eDP-1".into() }));
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
    fn waiting_and_down() {
        let b = Bar::new(BarConfig::default());
        assert!(b.grid("eDP-1", 40).text().starts_with("  …  │ …"), "nothing received: \"…\"");
        let mut b = bar();
        b.apply(Msg::Compositor(None));
        let t = b.grid("eDP-1", 40).text();
        assert!(t.starts_with("  –  │ –"), "source down: \"–\" ({t})");
        assert!(t.contains("14:05"), "the rest of the bar carries on");
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
        assert_eq!(t.matches('│').count(), 1, "workspaces and clock only: {t}");

        // With no workspace at all known to the compositor: same thing for
        // the workspaces module.
        let e2 = CompositorState { workspaces: Vec::new(), window: Some(Window { class: "firefox".into(), title: "Page".into() }) };
        let mut b2 = bar();
        b2.apply(Msg::Compositor(Some(e2)));
        let t2 = b2.grid("HDMI-A-1", 60).text();
        assert_eq!(t2.matches('│').count(), 1, "window and clock only: {t2}");

        // While waiting or down, "…" / "–" stay shown for both modules
        // (covered by `waiting_and_down`), never removed.
    }
}
