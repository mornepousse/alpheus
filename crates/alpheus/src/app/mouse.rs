//! Mouse: click, drag, wheel, on the regions recorded at render time.

use super::action::Command;
use super::{App, Item, Overlay, PanelField, STEP};
use crate::ui::hit::{HitMap, Target, slider_pick};
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::time::Instant;

impl App {
    pub fn on_mouse(&mut self, e: MouseEvent, hits: &HitMap, now: Instant) {
        let hit = hits.at(e.column, e.row);
        match e.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(hit, e.column, now),
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((id, bar)) = self.dragging {
                    self.set_slider(id, slider_pick(bar, e.column));
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = None,
            MouseEventKind::ScrollUp => self.wheel(hit, 1),
            MouseEventKind::ScrollDown => self.wheel(hit, -1),
            _ => {}
        }
    }

    fn click(&mut self, hit: Option<(Rect, Target)>, x: u16, now: Instant) {
        if self.overlay != Overlay::None {
            match hit.map(|(_, t)| t) {
                Some(Target::ConfirmYes) => self.dialog_answer(true, now),
                Some(Target::ConfirmNo) => self.dialog_answer(false, now),
                Some(Target::PaletteEntry(i)) => self.palette_run(Some(i), now),
                Some(Target::PanelField(f)) => self.panel_focus(f),
                Some(Target::PanelStep { field, dir }) => {
                    self.panel_focus(field);
                    self.panel_step(dir);
                }
                Some(Target::PanelCancel) => self.overlay = Overlay::None,
                Some(Target::PanelApply) => self.panel_apply(now),
                Some(Target::Overlay) => {}
                // Outside the overlay: closes help, the palette, the panel
                // or an entry; a confirmation and "Keep these settings?"
                // require an explicit choice.
                _ => {
                    if !matches!(self.overlay, Overlay::Confirm(_) | Overlay::Keep(_)) {
                        self.overlay = Overlay::None;
                    }
                }
            }
            return;
        }
        let Some((rect, target)) = hit else { return };
        match target {
            Target::Tab(t) => self.go_tab(t, now),
            Target::Item(i) => {
                self.focus = i;
                self.activate(now);
            }
            Target::Slider { item } => {
                self.focus = item;
                if let Some(Item::Slider(id)) = self.focused() {
                    self.dragging = Some((id, rect));
                    self.set_slider(id, slider_pick(rect, x));
                }
            }
            Target::ProfileOption(j) => {
                if let Some(i) = self.items().iter().position(|it| *it == Item::Profile) {
                    self.focus = i;
                }
                let choice = self.data.profiles.ready().and_then(|p| p.available.get(j).filter(|name| **name != p.active)).cloned();
                if let Some(name) = choice {
                    self.run_command(Command::SetProfile(name), now);
                }
            }
            Target::DisplayProfile(j) => {
                if let Some(i) = self.items().iter().position(|it| *it == Item::DisplayProfiles) {
                    self.focus = i;
                }
                self.displays.profile_sel = j;
                self.profile_bar_activate(now);
            }
            Target::OutputToggle(i) => {
                self.focus = i;
                if let Some(Item::Output(name)) = self.focused() {
                    self.toggle_output(&name, now);
                }
            }
            _ => {}
        }
    }

    /// Yes/no of a dialog, depending on the open overlay.
    fn dialog_answer(&mut self, yes: bool, now: Instant) {
        match self.overlay {
            Overlay::Confirm(_) => self.close_confirm(yes),
            Overlay::Keep(_) => self.keep_answer(yes, now),
            Overlay::SaveProfile(_) => self.save_answer(yes, now),
            _ => {}
        }
    }

    fn panel_focus(&mut self, f: PanelField) {
        if let Overlay::Panel(p) = &mut self.overlay {
            p.field = f;
        }
    }

    fn wheel(&mut self, hit: Option<(Rect, Target)>, dir: i32) {
        if matches!(self.overlay, Overlay::Palette(_)) {
            let count = self.palette_matches().len();
            if let Overlay::Palette(p) = &mut self.overlay {
                p.selected = if dir > 0 { p.selected.saturating_sub(1) } else { (p.selected + 1).min(count.saturating_sub(1)) };
            }
            return;
        }
        if self.overlay != Overlay::None {
            return;
        }
        match hit.map(|(_, t)| t) {
            Some(Target::Slider { item }) => {
                self.focus = item;
                if let Some(Item::Slider(id)) = self.focused() {
                    self.nudge_slider(id, dir * STEP);
                }
            }
            _ => self.move_focus(-dir), // wheel down = focus moves down
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::super::worker::{Op, Request};
    use super::super::{Confirm, Palette, SliderId};
    use super::*;
    use crate::app::action::Tab;
    use crate::backends::{NodeRef, PowerAction};
    use crate::ui::testing::draw;
    use ratatui::crossterm::event::KeyModifiers;

    fn ev(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
    }
    /// Renders the screen then sends the event onto the rendered regions.
    fn send(a: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        let (_, hits) = draw(a, 80, 24);
        a.on_mouse(ev(kind, x, y), &hits, Instant::now());
    }
    fn click(a: &mut App, x: u16, y: u16) {
        send(a, MouseEventKind::Down(MouseButton::Left), x, y);
    }
    fn center(a: &App, t: Target) -> (u16, u16) {
        let r = draw(a, 80, 24).1.find(t).unwrap_or_else(|| panic!("{t:?} missing"));
        (r.x + r.width / 2, r.y)
    }

    #[test]
    fn click_on_a_tab() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::Tab(Tab::Bluetooth));
        click(&mut a, x, y);
        assert_eq!(a.tab, Tab::Bluetooth);
    }

    #[test]
    fn click_on_a_toggle() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::Item(4)); // Do not disturb
        click(&mut a, x, y);
        assert_eq!(a.focus, 4);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDnd(true))]);
    }

    #[test]
    fn click_and_drag_on_a_slider() {
        let mut a = ready_app();
        let bar = draw(&a, 80, 24).1.find(Target::Slider { item: 0 }).unwrap();
        click(&mut a, bar.x, bar.y);
        assert_eq!(a.slider_value(SliderId::Volume), Some(0));
        send(&mut a, MouseEventKind::Drag(MouseButton::Left), bar.right() + 10, 7);
        assert_eq!(a.slider_value(SliderId::Volume), Some(100), "follows the mouse past the bar");
        send(&mut a, MouseEventKind::Up(MouseButton::Left), 0, 0);
        send(&mut a, MouseEventKind::Drag(MouseButton::Left), bar.x, bar.y);
        assert_eq!(a.slider_value(SliderId::Volume), Some(100), "drag finished");
        assert_eq!(
            a.take_requests(),
            vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 0)), Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100)),]
        );
    }

    #[test]
    fn brightness_stays_above_zero_with_the_mouse() {
        let mut a = ready_app();
        let bar = draw(&a, 80, 24).1.find(Target::Slider { item: 1 }).unwrap();
        click(&mut a, bar.x, bar.y);
        assert_eq!(a.slider_value(SliderId::Brightness), Some(1));
    }

    #[test]
    fn wheel() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::Slider { item: 1 });
        send(&mut a, MouseEventKind::ScrollUp, x, y);
        assert_eq!(a.slider_value(SliderId::Brightness), Some(25));
        assert_eq!(a.focus, 1);
        send(&mut a, MouseEventKind::ScrollDown, 5, 10); // off the slider: scrolls focus
        assert_eq!(a.focus, 2);
    }

    #[test]
    fn click_on_an_output_and_on_a_profile() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::ProfileOption(0));
        click(&mut a, x, y);
        assert_eq!(a.data.profiles.ready().unwrap().active, "performance");
        assert_eq!(a.focus, 6);
        a.go_tab(Tab::Sound, Instant::now());
        a.take_requests();
        let r = draw(&a, 80, 24).1.find(Target::Item(0)).unwrap();
        click(&mut a, r.x + 3, r.y); // on the name, not on the bar
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDefault(64))]);
    }

    #[test]
    fn confirmation_with_the_mouse() {
        let mut a = ready_app();
        a.overlay = Overlay::Confirm(Confirm { action: PowerAction::Reboot, yes_focused: false });
        click(&mut a, 0, 23); // outside: the confirmation stays
        assert!(matches!(a.overlay, Overlay::Confirm(_)));
        let (x, y) = center(&a, Target::Tab(Tab::Sound));
        click(&mut a, x, y); // a tab under the overlay doesn't respond
        assert_eq!(a.tab, Tab::Home);
        let (x, y) = center(&a, Target::ConfirmYes);
        click(&mut a, x, y);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Reboot))]);
    }

    // ── Displays tab ─────────────────────────────────────────────────

    use crate::app::worker::Update;
    use crate::app::{Item, Keep};

    fn displays_tab() -> App {
        let mut a = ready_app();
        a.go_tab(Tab::Displays, Instant::now());
        a.take_requests();
        a
    }

    #[test]
    fn click_on_a_display_profile() {
        let mut a = displays_tab();
        let (x, y) = center(&a, Target::DisplayProfile(1));
        click(&mut a, x, y);
        assert_eq!(a.displays.trial.as_ref().map(|t| t.title.as_str()), Some("Profile \"Bureau\""));
        assert!(matches!(a.overlay, Overlay::Keep(_)));
    }

    #[test]
    fn click_on_an_output_s_switch() {
        let mut a = displays_tab();
        let (x, y) = center(&a, Target::OutputToggle(3)); // Bravo
        click(&mut a, x, y);
        assert_eq!(a.focused(), Some(Item::Output("HDMI-A-1".into())));
        assert_eq!(a.displays.trial.as_ref().unwrap().title, "Bravo disabled");
    }

    #[test]
    fn keep_with_the_mouse() {
        let mut a = displays_tab();
        a.toggle_output("HDMI-A-1", Instant::now());
        let Some(Request::Do(op)) = a.take_requests().into_iter().next() else { panic!() };
        a.on_update(Update::Done(op, Ok(())), Instant::now());
        click(&mut a, 0, 23); // outside: the dialog stays
        assert_eq!(a.overlay, Overlay::Keep(Keep::default()));
        let (x, y) = center(&a, Target::ConfirmYes);
        click(&mut a, x, y);
        assert_eq!((a.overlay.clone(), a.displays.trial.clone()), (Overlay::None, None));
    }

    #[test]
    fn panel_with_the_mouse() {
        let mut a = displays_tab();
        let r = draw(&a, 80, 24).1.find(Target::Item(1)).unwrap();
        click(&mut a, r.x + 3, r.y); // panel row: opens the panel
        assert!(matches!(a.overlay, Overlay::Panel(_)));
        let (x, y) = center(&a, Target::PanelStep { field: PanelField::Scale, dir: 1 });
        click(&mut a, x, y);
        let Overlay::Panel(p) = &a.overlay else { panic!() };
        assert_eq!((p.field, p.scale), (PanelField::Scale, crate::displays::Scale(1250)));
        let (x, y) = center(&a, Target::PanelApply);
        click(&mut a, x, y);
        assert!(matches!(a.overlay, Overlay::Keep(_)));
        let mut a = displays_tab();
        a.open_panel("eDP-1", Instant::now());
        click(&mut a, 0, 23); // outside: the panel closes
        assert_eq!(a.overlay, Overlay::None);
    }

    #[test]
    fn palette_with_the_mouse() {
        let mut a = ready_app();
        a.overlay = Overlay::Palette(Palette { query: "wifi".into(), selected: 0 });
        let (x, y) = center(&a, Target::PaletteEntry(0));
        click(&mut a, x, y);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false))]);
        a.overlay = Overlay::Palette(Palette::default());
        send(&mut a, MouseEventKind::ScrollDown, 40, 12);
        assert_eq!(a.overlay, Overlay::Palette(Palette { query: String::new(), selected: 1 }));
        click(&mut a, 0, 23); // outside: closes
        assert_eq!(a.overlay, Overlay::None);
    }
}
