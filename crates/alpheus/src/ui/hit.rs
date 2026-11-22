//! Clickable regions recorded at render time, looked up by the mouse.

use crate::app::PanelField;
use crate::app::action::Tab;
use alpheus_commun::theme::settings::{BarStyle, ModeSetting, ModuleColors};
use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Tab(Tab),
    /// Row of a focusable element (index into App::items()).
    Item(usize),
    /// A slider's bar (index into App::items()).
    Slider {
        item: usize,
    },
    /// Energy profile option (index into Profiles::available).
    ProfileOption(usize),
    ConfirmYes,
    ConfirmNo,
    PaletteEntry(usize),
    /// Chip in the display-profile bar (count = number of profiles:
    /// "+ Save current").
    DisplayProfile(usize),
    /// A display's on/off switch (index into App::items()).
    OutputToggle(usize),
    /// Row of a field in the settings panel.
    PanelField(PanelField),
    /// ◀ (-1) or ▶ (+1) arrow of a panel field.
    PanelStep {
        field: PanelField,
        dir: i32,
    },
    PanelCancel,
    PanelApply,
    /// A row of the Appearance theme list (index into `theme_file::entries()`).
    AppearanceTheme(usize),
    /// A chip of the Appearance mode row.
    AppearanceMode(ModeSetting),
    /// A chip of the Appearance module-colours row.
    AppearanceColors(ModuleColors),
    /// A chip of the Appearance style row.
    AppearanceStyle(BarStyle),
    /// The bug report's `[ Copy ]`.
    ReportCopy,
    /// Inside of an overlay: absorbs the click.
    Overlay,
}

#[derive(Debug, Clone, Default)]
pub struct HitMap {
    regions: Vec<(Rect, Target)>,
}

impl HitMap {
    pub fn push(&mut self, r: Rect, t: Target) {
        if r.width > 0 && r.height > 0 {
            self.regions.push((r, t));
        }
    }

    pub fn at(&self, x: u16, y: u16) -> Option<(Rect, Target)> {
        self.regions.iter().rev().find(|(r, _)| x >= r.x && x < r.right() && y >= r.y && y < r.bottom()).copied()
    }

    pub fn find(&self, t: Target) -> Option<Rect> {
        self.regions.iter().rev().find(|(_, u)| *u == t).map(|(r, _)| *r)
    }
}

pub fn slider_pick(bar: Rect, x: u16) -> i32 {
    if bar.width <= 1 {
        return 0;
    }
    let span = i32::from(bar.width - 1);
    let off = i32::from(x.saturating_sub(bar.x)).min(span);
    (off * 100 + span / 2) / span
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_recent_region_wins() {
        let mut h = HitMap::default();
        h.push(Rect::new(0, 2, 80, 1), Target::Item(0));
        h.push(Rect::new(23, 2, 24, 1), Target::Slider { item: 0 });
        h.push(Rect::new(0, 0, 0, 1), Target::Overlay); // empty: ignored
        assert_eq!(h.at(30, 2).map(|x| x.1), Some(Target::Slider { item: 0 }));
        assert_eq!(h.at(5, 2).map(|x| x.1), Some(Target::Item(0)));
        assert_eq!(h.at(5, 3), None);
        assert_eq!(h.at(80, 2), None, "right edge excluded");
    }

    #[test]
    fn value_of_a_click_on_a_bar() {
        let bar = Rect::new(10, 0, 21, 1);
        assert_eq!(slider_pick(bar, 10), 0);
        assert_eq!(slider_pick(bar, 30), 100);
        assert_eq!(slider_pick(bar, 20), 50);
        assert_eq!(slider_pick(bar, 2), 0, "dragged left of the bar");
        assert_eq!(slider_pick(bar, 70), 100, "dragged right of the bar");
        assert_eq!(slider_pick(Rect::new(0, 0, 1, 1), 0), 0);
    }
}
