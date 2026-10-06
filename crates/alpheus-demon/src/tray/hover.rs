//! The tooltip's delay (spec § 2: "hover ½ s: tooltip with ToolTip title,
//! else Title, else Id — the timer runs only while hovering"), pure: which
//! icon the pointer rests on and since when. The wiring arms one
//! `DeadlineTimer` on `Arm`, removes it on `Cancel`; with the pointer
//! anywhere else, nothing is armed.

use crate::grid::{Grid, Style, Target, text_width, truncate};
use crate::popup::Anchor;
use crate::tray::ItemKey;
use std::time::{Duration, Instant};

pub const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverChange {
    Nothing,
    /// (Re)arm the timer at this instant.
    Arm(Instant),
    /// Remove the timer, and a tooltip shown.
    Cancel,
}

/// Which icon the pointer rests on, since when; pure.
#[derive(Debug, Default)]
pub struct Hover {
    over: Option<(ItemKey, Anchor)>,
    /// When the pointer came on `over`; None once its tooltip was asked for.
    since: Option<Instant>,
}

impl Hover {
    pub fn new() -> Hover {
        Hover::default()
    }

    /// The pointer moved over `over` (None: off any tray icon) at `now`: a new icon (re)arms, the same
    /// icon changes nothing, none cancels.
    pub fn moved(&mut self, over: Option<(ItemKey, Anchor)>, now: Instant) -> HoverChange {
        match over {
            None => self.cancel(),
            Some((key, _)) if self.over.as_ref().is_some_and(|(k, _)| *k == key) => HoverChange::Nothing,
            Some(o) => {
                self.over = Some(o);
                self.since = Some(now);
                HoverChange::Arm(now + TOOLTIP_DELAY)
            }
        }
    }

    /// A press, a leave, the tray redrawn under the pointer: cancel.
    pub fn cancel(&mut self) -> HoverChange {
        self.since = None;
        if self.over.take().is_some() { HoverChange::Cancel } else { HoverChange::Nothing }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.since.map(|s| s + TOOLTIP_DELAY)
    }

    /// The delay elapsed: the icon to show a tooltip for.
    pub fn fire(&mut self, now: Instant) -> Option<(ItemKey, Anchor)> {
        let due = self.deadline().is_some_and(|d| now >= d);
        if !due {
            return None;
        }
        // Asked once: resting longer on the same icon shows nothing more.
        self.since = None;
        self.over.clone()
    }
}

/// The tray icon under `column` of `output`'s bar, as a tooltip target; always None when
/// `[tray] tooltips = false` (no hover timer at all).
pub fn over(tooltips: bool, grid: &Grid, output: &str, column: u16) -> Option<(ItemKey, Anchor)> {
    if !tooltips {
        return None;
    }
    let zone = grid.zone_at(column)?;
    match &zone.target {
        Target::TrayItem(key) => Some((key.clone(), Anchor { output: output.to_string(), start: zone.start, end: zone.end })),
        _ => None,
    }
}

/// The tooltip's one row: the text on SURFACE with one blank cell each side, at most `max_columns`
/// wide (the output's columns − 2), truncated with "…".
pub fn tooltip_row(text: &str, max_columns: u16) -> Grid {
    let text = truncate(text, max_columns.saturating_sub(2));
    let columns = (text_width(&text) + 2).min(max_columns.max(2));
    let mut g = Grid::empty(columns);
    g.write(0, columns, &" ".repeat(usize::from(columns)), Style::SURFACE);
    g.write(1, columns.saturating_sub(1), &text, Style::SURFACE);
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> ItemKey {
        ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() }
    }

    fn at(n: u32) -> Option<(ItemKey, Anchor)> {
        Some((key(n), Anchor { output: "HDMI-A-1".into(), start: n as u16, end: n as u16 + 1 }))
    }

    #[test]
    fn hover_arms_once_per_icon_and_cancels_off_icon() {
        let t0 = Instant::now();
        let mut h = Hover::new();
        assert_eq!(h.moved(None, t0), HoverChange::Nothing, "off any icon: nothing armed");
        assert_eq!(h.moved(at(1), t0), HoverChange::Arm(t0 + TOOLTIP_DELAY));
        let later = t0 + Duration::from_millis(200);
        assert_eq!(h.moved(at(1), later), HoverChange::Nothing, "moving on the same icon does not re-arm");
        assert_eq!(h.deadline(), Some(t0 + TOOLTIP_DELAY), "the first arrival counts");
        assert_eq!(h.moved(at(2), later), HoverChange::Arm(later + TOOLTIP_DELAY), "another icon re-arms");
        assert_eq!(h.moved(None, later), HoverChange::Cancel, "off the icons: cancelled");
        assert_eq!(h.deadline(), None);
        assert_eq!(h.moved(None, later), HoverChange::Nothing, "already cancelled");
    }

    #[test]
    fn a_press_cancels() {
        let t0 = Instant::now();
        let mut h = Hover::new();
        h.moved(at(1), t0);
        assert_eq!(h.cancel(), HoverChange::Cancel);
        assert_eq!(h.fire(t0 + Duration::from_secs(1)), None, "nothing to show after a press");
        assert_eq!(h.cancel(), HoverChange::Nothing);
        assert_eq!(h.moved(at(1), t0), HoverChange::Arm(t0 + TOOLTIP_DELAY), "the same icon arms again after a cancel");
    }

    #[test]
    fn fire_only_after_the_delay() {
        let t0 = Instant::now();
        let mut h = Hover::new();
        h.moved(at(3), t0);
        assert_eq!(h.fire(t0 + Duration::from_millis(499)), None, "too early");
        assert_eq!(h.fire(t0 + TOOLTIP_DELAY), at(3));
        assert_eq!(h.deadline(), None, "fired once: no timer left");
        assert_eq!(h.fire(t0 + Duration::from_secs(5)), None, "not twice");
        assert_eq!(h.moved(at(3), t0 + Duration::from_secs(5)), HoverChange::Nothing, "still on it: nothing re-armed");
        assert_eq!(h.moved(None, t0 + Duration::from_secs(5)), HoverChange::Cancel, "leaving it closes the tooltip");
    }

    #[test]
    fn tooltips_off_never_arm() {
        let mut g = Grid::empty(10);
        g.write(4, 5, "x", Style::NORMAL);
        g.zone(4, 5, Target::TrayItem(key(7)));
        assert_eq!(over(true, &g, "HDMI-A-1", 4), at(7).map(|(k, _)| (k, Anchor { output: "HDMI-A-1".into(), start: 4, end: 5 })));
        assert_eq!(over(true, &g, "HDMI-A-1", 3), None, "beside the icon");
        assert_eq!(over(false, &g, "HDMI-A-1", 4), None, "tooltips = false");
        let mut h = Hover::new();
        assert_eq!(h.moved(over(false, &g, "HDMI-A-1", 4), Instant::now()), HoverChange::Nothing, "never armed");
    }

    #[test]
    fn tooltip_row() {
        let rows =
            [super::tooltip_row("Nimbus — 3 new messages", 80), super::tooltip_row(&"Kettle ".repeat(20), 30), super::tooltip_row("", 80)];
        let text: Vec<String> = rows.iter().map(|g| format!("{:?}\n{}", g.text(), g.styles())).collect();
        insta::assert_snapshot!("tooltip_row", text.join("\n"));
        assert!(rows[1].width() <= 30, "never wider than the output allows");
    }
}
