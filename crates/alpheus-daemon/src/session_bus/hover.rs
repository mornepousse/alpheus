//! The tooltip's delay (hover ½ s on a tray icon: tooltip with ToolTip title, else Title, else
//! Id; on a module with a tooltip: its text from `tooltips.rs`; the timer runs only while
//! hovering), pure: which icon or module the pointer rests on and since when. The wiring arms one
//! `DeadlineTimer` on `Arm`, removes it on `Cancel`; with the pointer anywhere else, nothing is
//! armed.
//!
//! Never:
//! - keep the timer while the pointer is elsewhere: it is armed on `Arm` and removed on `Cancel`,
//!   so a bar nobody hovers costs nothing (`nothing_resident_while_the_launcher_is_closed`), nor
//!   arm it over a module with no tooltip or with `tooltips = false`
//!   (`a_module_with_no_tooltip_arms_nothing`, `tooltips_false_arms_no_timer_anywhere`);
//! - find its bar by an output's name (`hover_fire_finds_its_bar_by_surface_identity_never_by_name`),
//!   or leave a tooltip's owner behind after its popup closed
//!   (`popup_close_resets_tooltip_owner_for_a_closed_tooltip`);
//! - close a module's tooltip because the tray changed (`a_module_tooltip_is_never_gone_with_a_tray_item`).
//!
//! See: docs/features/tray.md ("Tooltips"), docs/features/bar.md.

use crate::config::ModuleId;
use crate::grid::{Grid, Style, Target, text_width, truncate};
use crate::popup::Anchor;
use crate::session_bus::ItemKey;
use crate::text::clean;
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

/// What a tooltip belongs to: a tray icon, or a module of the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverKey {
    Tray(ItemKey),
    Module(ModuleId),
}

/// A `HoverKey` borrowed from the grid's zone: comparing it allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverRef<'a> {
    Tray(&'a ItemKey),
    Module(ModuleId),
}

impl HoverKey {
    pub fn as_ref(&self) -> HoverRef<'_> {
        match self {
            HoverKey::Tray(k) => HoverRef::Tray(k),
            HoverKey::Module(m) => HoverRef::Module(*m),
        }
    }
}

impl HoverRef<'_> {
    pub fn to_key(self) -> HoverKey {
        match self {
            HoverRef::Tray(k) => HoverKey::Tray(k.clone()),
            HoverRef::Module(m) => HoverKey::Module(m),
        }
    }
}

/// Which icon or module the pointer rests on, since when; pure.
#[derive(Debug, Default)]
pub struct Hover {
    over: Option<(HoverKey, Anchor)>,
    /// When the pointer came on `over`; None once its tooltip was asked for.
    since: Option<Instant>,
}

impl Hover {
    pub fn new() -> Hover {
        Hover::default()
    }

    /// The pointer moved over `found` (the hovered zone's key, borrowed, and its columns; None:
    /// off any zone with a tooltip) on `output`, at `now`: a new icon or module (re)arms — cloning
    /// the key and allocating `output` into an `Anchor` only then — the same one changes nothing
    /// (compared on the borrowed key, no allocation at all), none cancels.
    pub fn moved(&mut self, found: Option<(HoverRef<'_>, u16, u16)>, output: &str, now: Instant) -> HoverChange {
        match found {
            None => self.cancel(),
            Some((key, _, _)) if self.over.as_ref().is_some_and(|(k, _)| k.as_ref() == key) => HoverChange::Nothing,
            Some((key, start, end)) => {
                self.over = Some((key.to_key(), Anchor { output: output.to_string(), start, end }));
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

    /// The delay elapsed: the icon or module to show a tooltip for.
    pub fn fire(&mut self, now: Instant) -> Option<(HoverKey, Anchor)> {
        let due = self.deadline().is_some_and(|d| now >= d);
        if !due {
            return None;
        }
        // Asked once: resting longer on the same zone shows nothing more.
        self.since = None;
        self.over.clone()
    }

    /// The icon or module currently tracked (armed, or already shown as a tooltip) and its last
    /// known anchor; None at rest. Lets the wiring (`hover_target_moved`, wayland.rs) tell an
    /// unrelated zone changing from the tracked one actually moving or disappearing under a
    /// redrawn grid: only the latter should ever cancel.
    pub fn target(&self) -> Option<(HoverRef<'_>, &Anchor)> {
        self.over.as_ref().map(|(k, a)| (k.as_ref(), a))
    }
}

/// The zone under `column` of the bar that can show a tooltip, borrowed from `grid` (no
/// allocation): a tray icon, or a module (its click zone or its passive one) whose spec has a
/// tooltip; its key and the zone's own columns. Always None when `tooltips = false` (no hover
/// timer at all). The `Anchor` (which owns the output name) is built only once `Hover::moved`
/// actually arms: a plain motion event over the SAME zone must allocate nothing.
pub fn over(tooltips: bool, grid: &Grid, column: u16) -> Option<(HoverRef<'_>, u16, u16)> {
    if !tooltips {
        return None;
    }
    let zone = grid.zone_at(column)?;
    let key = match &zone.target {
        Target::TrayItem(key) => HoverRef::Tray(key),
        Target::Module(m) | Target::Passive(m) if m.spec().tooltip.is_some() => HoverRef::Module(*m),
        _ => return None,
    };
    Some((key, zone.start, zone.end))
}

/// Whether the zone of `key` left `anchor`'s columns in `grid`, or left the grid: a tray icon by
/// its item, a module by its click or passive zone.
pub fn zone_moved(grid: &Grid, key: HoverRef<'_>, anchor: &Anchor) -> bool {
    let tracked = |t: &Target| match (t, key) {
        (Target::TrayItem(k), HoverRef::Tray(key)) => k == key,
        (Target::Module(m) | Target::Passive(m), HoverRef::Module(key)) => *m == key,
        _ => false,
    };
    match grid.zones().iter().find(|z| tracked(&z.target)) {
        Some(z) => (z.start, z.end) != (anchor.start, anchor.end),
        None => true, // dropped from the grid: gone
    }
}

/// Whether the shown tooltip's owner is gone: a tray icon that left the tray (`item_exists` false),
/// or no owner recorded at all (the tooltip closes rather than lingering). A module never leaves
/// with a tray item: a tray change must not close the clock's tooltip.
pub fn owner_gone(owner: Option<&HoverKey>, item_exists: impl Fn(&ItemKey) -> bool) -> bool {
    match owner {
        Some(HoverKey::Tray(key)) => !item_exists(key),
        Some(HoverKey::Module(_)) => false,
        None => true,
    }
}

/// The most cells (pad cells included) a tooltip may take under a bar `bar_columns` wide: the bar's
/// own width less the columns the pixel frame adds around the cells (`extra_width` physical px, at
/// `advance` px a column, rounded up) — on a tall bar the frame's side room is larger than the two
/// cells the tooltip used to reserve, and a very long text would then overrun the output.
pub fn tooltip_columns(bar_columns: u16, extra_width: u32, advance: f32) -> u16 {
    let frame = if advance > 0.0 { (extra_width as f32 / advance).ceil() as u32 } else { 2 };
    bar_columns.saturating_sub(frame.min(u32::from(u16::MAX)) as u16)
}

/// The text a tooltip keeps: made drawable (`text::clean`), and none at all when nothing is left
/// to read: a title of only invisible characters shows no empty box.
pub fn shown_text(text: Option<String>) -> Option<String> {
    text.map(|t| clean(&t)).filter(|t| !t.trim().is_empty())
}

/// The tooltip's one row: the text made drawable (`text::clean`: controls and bidirectional
/// formatting gone, at most `MAX_TEXT` characters — a window title or a device name comes whole
/// and uncapped from `tooltips.rs`), truncated with "…" to `max_columns` − 2 (the room
/// `tooltip_columns` leaves less the pad cell `list::padded` adds on each side). The frame around
/// it is the painter's, as for every boxed surface.
pub fn tooltip_row(text: &str, max_columns: u16) -> Grid {
    let text = truncate(&clean(text), max_columns.saturating_sub(2));
    let columns = text_width(&text);
    let mut g = Grid::empty(columns);
    g.write(0, columns, &text, Style::NORMAL);
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> ItemKey {
        ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() }
    }

    fn at(n: u32) -> Option<(HoverKey, Anchor)> {
        Some((HoverKey::Tray(key(n)), Anchor { output: "HDMI-A-1".into(), start: n as u16, end: n as u16 + 1 }))
    }

    /// `Hover::moved` now borrows its key (no allocation on a
    /// plain motion event); these two helpers own the `ItemKey` for the call's duration, the way
    /// the real wiring's `Grid` does.
    fn moved_to(h: &mut Hover, n: u32, now: Instant) -> HoverChange {
        let k = key(n);
        h.moved(Some((HoverRef::Tray(&k), n as u16, n as u16 + 1)), "HDMI-A-1", now)
    }

    fn moved_off(h: &mut Hover, now: Instant) -> HoverChange {
        h.moved(None, "HDMI-A-1", now)
    }

    #[test]
    fn a_tall_bars_frame_costs_more_columns_than_two() {
        assert_eq!(tooltip_columns(100, 16, 8.0), 98, "a normal bar: the old two columns");
        assert_eq!(tooltip_columns(100, 17, 8.0), 97, "rounded up");
        assert_eq!(tooltip_columns(100, 60, 8.0), 92, "a tall bar: 8 columns of frame, never 2");
        assert_eq!(tooltip_columns(5, 60, 8.0), 0, "never negative");
        assert_eq!(tooltip_columns(100, 16, 0.0), 98, "no advance: the old default, no division by zero");
        // The whole popup, text truncated to what is left, fits the bar in pixels.
        let (bar, extra, advance) = (100_u16, 60_u32, 8.0_f32);
        let row = super::tooltip_row(&"x".repeat(500), tooltip_columns(bar, extra, advance));
        let cells = row.width() + 2; // `list::padded`
        assert!(f32::from(cells) * advance + extra as f32 <= f32::from(bar) * advance, "{cells} cells + frame within {bar} columns");
    }

    #[test]
    fn hover_arms_once_per_icon_and_cancels_off_icon() {
        let t0 = Instant::now();
        let mut h = Hover::new();
        assert_eq!(moved_off(&mut h, t0), HoverChange::Nothing, "off any icon: nothing armed");
        assert_eq!(moved_to(&mut h, 1, t0), HoverChange::Arm(t0 + TOOLTIP_DELAY));
        let later = t0 + Duration::from_millis(200);
        assert_eq!(moved_to(&mut h, 1, later), HoverChange::Nothing, "moving on the same icon does not re-arm");
        assert_eq!(h.deadline(), Some(t0 + TOOLTIP_DELAY), "the first arrival counts");
        assert_eq!(moved_to(&mut h, 2, later), HoverChange::Arm(later + TOOLTIP_DELAY), "another icon re-arms");
        assert_eq!(moved_off(&mut h, later), HoverChange::Cancel, "off the icons: cancelled");
        assert_eq!(h.deadline(), None);
        assert_eq!(moved_off(&mut h, later), HoverChange::Nothing, "already cancelled");
    }

    #[test]
    fn a_press_cancels() {
        let t0 = Instant::now();
        let mut h = Hover::new();
        moved_to(&mut h, 1, t0);
        assert_eq!(h.cancel(), HoverChange::Cancel);
        assert_eq!(h.fire(t0 + Duration::from_secs(1)), None, "nothing to show after a press");
        assert_eq!(h.cancel(), HoverChange::Nothing);
        assert_eq!(moved_to(&mut h, 1, t0), HoverChange::Arm(t0 + TOOLTIP_DELAY), "the same icon arms again after a cancel");
    }

    #[test]
    fn fire_only_after_the_delay() {
        let t0 = Instant::now();
        let mut h = Hover::new();
        moved_to(&mut h, 3, t0);
        assert_eq!(h.fire(t0 + Duration::from_millis(499)), None, "too early");
        assert_eq!(h.fire(t0 + TOOLTIP_DELAY), at(3));
        assert_eq!(h.deadline(), None, "fired once: no timer left");
        assert_eq!(h.fire(t0 + Duration::from_secs(5)), None, "not twice");
        assert_eq!(moved_to(&mut h, 3, t0 + Duration::from_secs(5)), HoverChange::Nothing, "still on it: nothing re-armed");
        assert_eq!(moved_off(&mut h, t0 + Duration::from_secs(5)), HoverChange::Cancel, "leaving it closes the tooltip");
    }

    #[test]
    fn tooltips_off_never_arm() {
        let mut g = Grid::empty(10);
        g.write(4, 5, "x", Style::NORMAL);
        g.zone(4, 5, Target::TrayItem(key(7)));
        let k7 = key(7);
        assert_eq!(over(true, &g, 4), Some((HoverRef::Tray(&k7), 4, 5)));
        assert_eq!(over(true, &g, 3), None, "beside the icon");
        assert_eq!(over(false, &g, 4), None, "tooltips = false");
        let mut h = Hover::new();
        assert_eq!(h.moved(over(false, &g, 4), "HDMI-A-1", Instant::now()), HoverChange::Nothing, "never armed");
    }

    #[test]
    fn target_reports_the_tracked_item_and_its_anchor() {
        let t0 = Instant::now();
        let mut h = Hover::new();
        assert_eq!(h.target(), None, "nothing tracked at rest");
        moved_to(&mut h, 1, t0);
        assert_eq!(h.target(), Some((HoverRef::Tray(&key(1)), &Anchor { output: "HDMI-A-1".into(), start: 1, end: 2 })));
        h.cancel();
        assert_eq!(h.target(), None, "cancelled: nothing tracked any more");
    }

    fn module_grid() -> Grid {
        let mut g = Grid::empty(40);
        g.write(0, 5, "12:00", Style::NORMAL);
        g.zone(0, 5, Target::Module(ModuleId::Clock));
        g.write(6, 7, "x", Style::NORMAL);
        g.zone(6, 7, Target::TrayItem(key(7)));
        g.write(10, 15, "title", Style::NORMAL);
        g.zone(10, 15, Target::Passive(ModuleId::Window));
        g.write(20, 21, "L", Style::NORMAL);
        g.zone(20, 21, Target::Module(ModuleId::Launcher));
        g
    }

    #[test]
    fn module_hover_arms_once_and_cancels_off_the_zone() {
        let t0 = Instant::now();
        let g = module_grid();
        let mut h = Hover::new();
        assert_eq!(over(true, &g, 2), Some((HoverRef::Module(ModuleId::Clock), 0, 5)));
        assert_eq!(h.moved(over(true, &g, 2), "HDMI-A-1", t0), HoverChange::Arm(t0 + TOOLTIP_DELAY));
        let later = t0 + Duration::from_millis(200);
        assert_eq!(h.moved(over(true, &g, 4), "HDMI-A-1", later), HoverChange::Nothing, "the same module: not re-armed");
        assert_eq!(h.target(), Some((HoverRef::Module(ModuleId::Clock), &Anchor { output: "HDMI-A-1".into(), start: 0, end: 5 })));
        assert_eq!(
            h.fire(t0 + TOOLTIP_DELAY),
            Some((HoverKey::Module(ModuleId::Clock), Anchor { output: "HDMI-A-1".into(), start: 0, end: 5 }))
        );
        assert_eq!(h.fire(t0 + Duration::from_secs(5)), None, "once");
        assert_eq!(h.moved(over(true, &g, 8), "HDMI-A-1", later), HoverChange::Cancel, "off any zone");
        assert_eq!(h.target(), None);
    }

    #[test]
    fn moving_from_a_module_to_a_tray_icon_rearms() {
        let t0 = Instant::now();
        let g = module_grid();
        let mut h = Hover::new();
        h.moved(over(true, &g, 1), "HDMI-A-1", t0);
        let later = t0 + Duration::from_millis(300);
        let k7 = key(7);
        assert_eq!(over(true, &g, 6), Some((HoverRef::Tray(&k7), 6, 7)));
        assert_eq!(h.moved(over(true, &g, 6), "HDMI-A-1", later), HoverChange::Arm(later + TOOLTIP_DELAY));
        assert_eq!(h.moved(over(true, &g, 1), "HDMI-A-1", later), HoverChange::Arm(later + TOOLTIP_DELAY), "and back");
        assert_eq!(h.target().map(|(k, _)| k), Some(HoverRef::Module(ModuleId::Clock)));
    }

    #[test]
    fn a_module_with_no_tooltip_arms_nothing() {
        assert!(ModuleId::Launcher.spec().tooltip.is_none(), "the premise: the launcher has no tooltip");
        let g = module_grid();
        assert_eq!(over(true, &g, 20), None);
        let mut h = Hover::new();
        assert_eq!(h.moved(over(true, &g, 20), "HDMI-A-1", Instant::now()), HoverChange::Nothing, "never armed");
    }

    #[test]
    fn a_passive_zone_with_a_tooltip_arms() {
        let g = module_grid();
        assert_eq!(over(true, &g, 12), Some((HoverRef::Module(ModuleId::Window), 10, 15)));
    }

    #[test]
    fn tooltips_false_arms_no_timer_anywhere() {
        let g = module_grid();
        for column in 0..g.width() {
            assert_eq!(over(false, &g, column), None, "column {column}: tooltips = false");
        }
        let mut h = Hover::new();
        for column in [2, 6, 12] {
            assert_eq!(h.moved(over(false, &g, column), "HDMI-A-1", Instant::now()), HoverChange::Nothing);
        }
    }

    #[test]
    fn a_zone_moves_by_its_own_columns_only() {
        let g = module_grid();
        let anchor = |start, end| Anchor { output: "HDMI-A-1".into(), start, end };
        assert!(!zone_moved(&g, HoverRef::Module(ModuleId::Clock), &anchor(0, 5)), "same cells");
        assert!(zone_moved(&g, HoverRef::Module(ModuleId::Clock), &anchor(0, 6)), "wider");
        assert!(!zone_moved(&g, HoverRef::Module(ModuleId::Window), &anchor(10, 15)), "a passive zone");
        assert!(zone_moved(&g, HoverRef::Module(ModuleId::Volume), &anchor(0, 5)), "not in the grid: gone");
        let k7 = key(7);
        assert!(!zone_moved(&g, HoverRef::Tray(&k7), &anchor(6, 7)), "a tray icon by its item");
        assert!(zone_moved(&g, HoverRef::Tray(&key(8)), &anchor(6, 7)), "another item: gone");
    }

    #[test]
    fn a_module_tooltip_is_never_gone_with_a_tray_item() {
        assert!(!owner_gone(Some(&HoverKey::Module(ModuleId::Clock)), |_| false), "a tray change never closes a module's tooltip");
        assert!(owner_gone(Some(&HoverKey::Tray(key(1))), |_| false), "its item left the tray");
        assert!(!owner_gone(Some(&HoverKey::Tray(key(1))), |k| *k == key(1)), "its item still there");
        assert!(owner_gone(None, |_| true), "no owner recorded: the tooltip closes rather than lingering");
    }

    /// A window title or a Bluetooth name is drawn whole by its tooltip, and the pure text
    /// functions do not cap it: the row cleans it (controls, bidirectional overrides) and cuts it.
    #[test]
    fn a_hostile_title_is_drawn_clean_and_capped() {
        let hostile = format!("\u{1b}[31mEvil\n\u{202e}eltit\u{7}\u{200b}{}", "x".repeat(10_000));
        let row = super::tooltip_row(&hostile, u16::MAX);
        let text = row.text();
        assert!(!text.chars().any(|c| c.is_control() || crate::text::invisible(c)), "drawn clean: {:?}", &text[..40]);
        assert!(text.starts_with("[31mEvil eltit"), "the line kept, its controls gone: {:?}", &text[..40]);
        assert!(text.chars().count() <= crate::text::MAX_TEXT, "capped whatever the room: {}", text.chars().count());
        assert!(super::tooltip_row(&hostile, 30).width() <= 28, "and cut to the room");
    }

    /// A title of only invisible or blank characters leaves nothing to read: no tooltip at all,
    /// never an empty framed box.
    #[test]
    fn nothing_left_to_read_shows_no_tooltip() {
        assert_eq!(shown_text(Some("\u{200b}".into())), None);
        assert_eq!(shown_text(Some(" \n\u{200e}".into())), None);
        assert_eq!(shown_text(None), None);
        assert_eq!(shown_text(Some("Evil\u{7}\ntitle".into())).as_deref(), Some("Evil title"));
    }

    #[test]
    fn tooltip_row() {
        let rows =
            [super::tooltip_row("Nimbus — 3 new messages", 80), super::tooltip_row(&"Kettle ".repeat(20), 30), super::tooltip_row("", 80)];
        let text: Vec<String> = rows.iter().map(|g| format!("{:?}\n{}", g.text(), g.styles())).collect();
        insta::assert_snapshot!("tooltip_row", text.join("\n"));
        assert!(crate::list::padded(&rows[1..2]).cells().0 <= 30, "never wider than the output allows, pad cells included");
    }
}
