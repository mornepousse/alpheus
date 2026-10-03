//! Souris : clic, glissement, molette, sur les zones relevées au rendu.

use super::action::Command;
use super::{App, Item, Overlay, STEP};
use crate::ui::hit::{slider_pick, HitMap, Target};
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
                Some(Target::ConfirmYes) => self.close_confirm(true),
                Some(Target::ConfirmNo) => self.close_confirm(false),
                Some(Target::PaletteEntry(i)) => self.palette_run(Some(i), now),
                Some(Target::Overlay) => {}
                // Hors de la surcouche : ferme l'aide ou la palette ; une
                // confirmation exige un choix explicite.
                _ => {
                    if !matches!(self.overlay, Overlay::Confirm(_)) {
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
                let choice = self
                    .data
                    .profiles
                    .ready()
                    .and_then(|p| p.available.get(j).filter(|name| **name != p.active))
                    .cloned();
                if let Some(name) = choice {
                    self.run_command(Command::SetProfile(name), now);
                }
            }
            _ => {}
        }
    }

    fn wheel(&mut self, hit: Option<(Rect, Target)>, dir: i32) {
        if matches!(self.overlay, Overlay::Palette(_)) {
            let count = self.palette_matches().len();
            if let Overlay::Palette(p) = &mut self.overlay {
                p.selected = if dir > 0 {
                    p.selected.saturating_sub(1)
                } else {
                    (p.selected + 1).min(count.saturating_sub(1))
                };
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
            _ => self.move_focus(-dir), // molette vers le bas = focus vers le bas
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
    /// Rend l'écran puis envoie l'événement sur les zones du rendu.
    fn send(a: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        let (_, hits) = draw(a, 80, 24);
        a.on_mouse(ev(kind, x, y), &hits, Instant::now());
    }
    fn click(a: &mut App, x: u16, y: u16) {
        send(a, MouseEventKind::Down(MouseButton::Left), x, y);
    }
    fn center(a: &App, t: Target) -> (u16, u16) {
        let r = draw(a, 80, 24).1.find(t).unwrap_or_else(|| panic!("{t:?} absent"));
        (r.x + r.width / 2, r.y)
    }

    #[test]
    fn clic_sur_un_onglet() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::Tab(Tab::Bluetooth));
        click(&mut a, x, y);
        assert_eq!(a.tab, Tab::Bluetooth);
    }

    #[test]
    fn clic_sur_un_interrupteur() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::Item(4)); // Ne pas déranger
        click(&mut a, x, y);
        assert_eq!(a.focus, 4);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDnd(true))]);
    }

    #[test]
    fn clic_et_glissement_sur_un_curseur() {
        let mut a = ready_app();
        let bar = draw(&a, 80, 24).1.find(Target::Slider { item: 0 }).unwrap();
        click(&mut a, bar.x, bar.y);
        assert_eq!(a.slider_value(SliderId::Volume), Some(0));
        send(&mut a, MouseEventKind::Drag(MouseButton::Left), bar.right() + 10, 7);
        assert_eq!(a.slider_value(SliderId::Volume), Some(100), "suit la souris hors de la barre");
        send(&mut a, MouseEventKind::Up(MouseButton::Left), 0, 0);
        send(&mut a, MouseEventKind::Drag(MouseButton::Left), bar.x, bar.y);
        assert_eq!(a.slider_value(SliderId::Volume), Some(100), "glissement terminé");
        assert_eq!(a.take_requests(), vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 0)),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100)),
        ]);
    }

    #[test]
    fn la_luminosite_reste_au_dessus_de_zero_a_la_souris() {
        let mut a = ready_app();
        let bar = draw(&a, 80, 24).1.find(Target::Slider { item: 1 }).unwrap();
        click(&mut a, bar.x, bar.y);
        assert_eq!(a.slider_value(SliderId::Brightness), Some(1));
    }

    #[test]
    fn molette() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::Slider { item: 1 });
        send(&mut a, MouseEventKind::ScrollUp, x, y);
        assert_eq!(a.slider_value(SliderId::Brightness), Some(25));
        assert_eq!(a.focus, 1);
        send(&mut a, MouseEventKind::ScrollDown, 5, 10); // hors curseur : défilement
        assert_eq!(a.focus, 2);
    }

    #[test]
    fn clic_sur_une_sortie_et_sur_un_profil() {
        let mut a = ready_app();
        let (x, y) = center(&a, Target::ProfileOption(0));
        click(&mut a, x, y);
        assert_eq!(a.data.profiles.ready().unwrap().active, "performance");
        assert_eq!(a.focus, 6);
        a.go_tab(Tab::Son, Instant::now());
        a.take_requests();
        let r = draw(&a, 80, 24).1.find(Target::Item(0)).unwrap();
        click(&mut a, r.x + 3, r.y); // sur le nom, pas sur la barre
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDefault(64))]);
    }

    #[test]
    fn confirmation_a_la_souris() {
        let mut a = ready_app();
        a.overlay = Overlay::Confirm(Confirm { action: PowerAction::Reboot, yes_focused: false });
        click(&mut a, 0, 23); // dehors : la confirmation reste
        assert!(matches!(a.overlay, Overlay::Confirm(_)));
        let (x, y) = center(&a, Target::Tab(Tab::Son));
        click(&mut a, x, y); // un onglet sous la surcouche ne répond pas
        assert_eq!(a.tab, Tab::Accueil);
        let (x, y) = center(&a, Target::ConfirmYes);
        click(&mut a, x, y);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Reboot))]);
    }

    #[test]
    fn palette_a_la_souris() {
        let mut a = ready_app();
        a.overlay = Overlay::Palette(Palette { query: "wifi".into(), selected: 0 });
        let (x, y) = center(&a, Target::PaletteEntry(0));
        click(&mut a, x, y);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false))]);
        a.overlay = Overlay::Palette(Palette::default());
        send(&mut a, MouseEventKind::ScrollDown, 40, 12);
        assert_eq!(a.overlay, Overlay::Palette(Palette { query: String::new(), selected: 1 }));
        click(&mut a, 0, 23); // dehors : ferme
        assert_eq!(a.overlay, Overlay::None);
    }
}
