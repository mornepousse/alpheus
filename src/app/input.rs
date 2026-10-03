//! Clavier : surcouches d'abord (confirmation, palette, aide), puis saisie de
//! recherche, puis table de touches configurable.

use super::action::{Action, Tab};
use super::{App, Item, Overlay, Palette, Search};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::Instant;

enum PaletteKey {
    Close,
    Run,
    Up,
    Down,
    Pop,
    Push(char),
    Ignore,
}

fn plain(e: &KeyEvent) -> bool {
    !e.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

impl App {
    pub fn on_key(&mut self, e: KeyEvent, now: Instant) {
        if e.kind == KeyEventKind::Release {
            return;
        }
        match self.overlay {
            Overlay::Confirm(_) => return self.key_confirm(e),
            Overlay::Palette(_) => return self.key_palette(e, now),
            Overlay::Help => return self.key_help(e),
            Overlay::None => {}
        }
        if self.search.editing {
            return self.key_search(e);
        }
        if let Some(a) = self.keymap.lookup(&e) {
            self.do_action(a, now);
        }
    }

    fn key_confirm(&mut self, e: KeyEvent) {
        let action = if e.code == KeyCode::Esc { Some(Action::Back) } else { self.keymap.lookup(&e) };
        match action {
            Some(Action::Back | Action::Quit) => self.close_confirm(false),
            Some(Action::Activate) => {
                let yes = matches!(self.overlay, Overlay::Confirm(c) if c.yes_focused);
                self.close_confirm(yes);
            }
            Some(Action::Left | Action::Right | Action::NextTab | Action::PrevTab) => {
                if let Overlay::Confirm(c) = &mut self.overlay {
                    c.yes_focused = !c.yes_focused;
                }
            }
            _ => {}
        }
    }

    fn key_help(&mut self, e: KeyEvent) {
        let a = self.keymap.lookup(&e);
        if e.code == KeyCode::Esc || matches!(a, Some(Action::Back | Action::Help | Action::Quit)) {
            self.overlay = Overlay::None;
        }
    }

    fn key_palette(&mut self, e: KeyEvent, now: Instant) {
        let k = match e.code {
            KeyCode::Esc => PaletteKey::Close,
            KeyCode::Enter => PaletteKey::Run,
            KeyCode::Up => PaletteKey::Up,
            KeyCode::Down | KeyCode::Tab => PaletteKey::Down,
            KeyCode::Backspace => PaletteKey::Pop,
            KeyCode::Char(ch) if plain(&e) => PaletteKey::Push(ch),
            _ if self.keymap.lookup(&e) == Some(Action::Palette) => PaletteKey::Close,
            _ => PaletteKey::Ignore,
        };
        let count = self.palette_matches().len();
        match k {
            PaletteKey::Close => self.overlay = Overlay::None,
            PaletteKey::Run => self.palette_run(None, now),
            PaletteKey::Ignore => {}
            k => {
                if let Overlay::Palette(p) = &mut self.overlay {
                    match k {
                        PaletteKey::Up => p.selected = p.selected.saturating_sub(1),
                        PaletteKey::Down => p.selected = (p.selected + 1).min(count.saturating_sub(1)),
                        PaletteKey::Pop => {
                            p.query.pop();
                            p.selected = 0;
                        }
                        PaletteKey::Push(ch) => {
                            p.query.push(ch);
                            p.selected = 0;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn key_search(&mut self, e: KeyEvent) {
        match e.code {
            KeyCode::Esc => self.search = Search::default(),
            KeyCode::Enter => self.search.editing = false,
            KeyCode::Backspace => {
                self.search.text.pop();
            }
            KeyCode::Char(ch) if plain(&e) => self.search.text.push(ch),
            _ => return,
        }
        self.focus = 0;
        self.clamp_focus();
    }

    fn do_action(&mut self, a: Action, now: Instant) {
        match a {
            Action::Quit => self.should_quit = true,
            Action::Back => {
                if !self.search.text.is_empty() {
                    self.search = Search::default();
                    self.clamp_focus();
                } else if self.tab != Tab::Accueil {
                    self.go_tab(Tab::Accueil, now);
                }
            }
            Action::Help => self.overlay = Overlay::Help,
            Action::Palette => self.overlay = Overlay::Palette(Palette::default()),
            Action::Search => {
                if self.tab.searchable() {
                    self.search.editing = true;
                }
            }
            Action::NextTab => self.go_tab(self.tab.next(), now),
            Action::PrevTab => self.go_tab(self.tab.prev(), now),
            Action::GoTab(t) => self.go_tab(t, now),
            Action::Up => self.move_focus(-1),
            Action::Down => self.move_focus(1),
            Action::PageUp => self.move_focus(-5),
            Action::PageDown => self.move_focus(5),
            Action::Home => self.focus = 0,
            Action::End => self.focus = self.items().len().saturating_sub(1),
            Action::Left | Action::Decrease => self.adjust(-1, now),
            Action::Right | Action::Increase => self.adjust(1, now),
            Action::Toggle => {
                if let Some(Item::Toggle(id)) = self.focused() {
                    self.toggle(id, now);
                }
            }
            Action::Activate => self.activate(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::super::worker::{Op, Request};
    use super::super::{Confirm, SliderId, ToggleId};
    use super::*;
    use crate::backends::{NodeRef, PowerAction};
    use ratatui::crossterm::event::KeyEventState;

    fn press(a: &mut App, code: KeyCode, mods: KeyModifiers) {
        a.on_key(KeyEvent::new(code, mods), Instant::now());
    }
    fn k(a: &mut App, code: KeyCode) {
        press(a, code, KeyModifiers::NONE);
    }
    fn c(a: &mut App, ch: char) {
        k(a, KeyCode::Char(ch));
    }
    fn typed(a: &mut App, s: &str) {
        s.chars().for_each(|ch| c(a, ch));
    }

    #[test]
    fn quitter() {
        let mut a = ready_app();
        c(&mut a, 'q');
        assert!(a.should_quit);
    }

    #[test]
    fn onglets() {
        let mut a = ready_app();
        c(&mut a, '2');
        assert_eq!(a.tab, Tab::Son);
        k(&mut a, KeyCode::Tab);
        assert_eq!(a.tab, Tab::Reseau);
        press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
        press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
        press(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(a.tab, Tab::Alimentation);
        k(&mut a, KeyCode::Esc);
        assert_eq!(a.tab, Tab::Accueil);
        k(&mut a, KeyCode::Esc);
        assert_eq!(a.tab, Tab::Accueil);
        assert!(!a.should_quit, "Échap ne quitte pas");
    }

    #[test]
    fn deplacements() {
        let mut a = ready_app();
        k(&mut a, KeyCode::Down);
        c(&mut a, 'h'); // Dvorak : bas
        assert_eq!(a.focus, 2);
        k(&mut a, KeyCode::End);
        assert_eq!(a.focus, 6);
        k(&mut a, KeyCode::Down);
        assert_eq!(a.focus, 6, "borné");
        k(&mut a, KeyCode::PageUp);
        assert_eq!(a.focus, 1);
        k(&mut a, KeyCode::Home);
        assert_eq!(a.focus, 0);
    }

    #[test]
    fn curseur_au_clavier() {
        let mut a = ready_app();
        press(&mut a, KeyCode::Char('+'), KeyModifiers::SHIFT);
        k(&mut a, KeyCode::Right);
        c(&mut a, '-');
        assert_eq!(a.take_requests(), vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 67)),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 72)),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 67)),
        ]);
    }

    #[test]
    fn espace_bascule_seulement_les_interrupteurs() {
        let mut a = ready_app();
        c(&mut a, ' '); // sur le curseur Volume : rien
        assert!(a.take_requests().is_empty());
        a.focus = 2;
        c(&mut a, ' ');
        assert_eq!(a.toggle_loadable(ToggleId::Wifi).ready(), Some(&false));
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false))]);
    }

    #[test]
    fn confirmation_au_clavier() {
        let mut a = ready_app();
        c(&mut a, '5');
        a.focus = 4; // Éteindre
        k(&mut a, KeyCode::Enter);
        assert_eq!(a.overlay, Overlay::Confirm(Confirm { action: PowerAction::Poweroff, yes_focused: false }));
        c(&mut a, 'q');
        assert!(!a.should_quit, "q annule la confirmation, ne quitte pas");
        assert_eq!(a.overlay, Overlay::None);
        k(&mut a, KeyCode::Enter);
        k(&mut a, KeyCode::Enter); // focus sur Annuler
        assert_eq!(a.overlay, Overlay::None);
        a.take_requests();
        k(&mut a, KeyCode::Enter);
        k(&mut a, KeyCode::Right);
        k(&mut a, KeyCode::Enter);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Poweroff))]);
        k(&mut a, KeyCode::Enter);
        k(&mut a, KeyCode::Esc);
        assert_eq!(a.overlay, Overlay::None);
    }

    #[test]
    fn palette_au_clavier() {
        let mut a = ready_app();
        press(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(a.overlay, Overlay::Palette(Palette::default()));
        typed(&mut a, "qx");
        assert!(!a.should_quit);
        k(&mut a, KeyCode::Backspace);
        k(&mut a, KeyCode::Backspace);
        typed(&mut a, "wifi");
        k(&mut a, KeyCode::Enter);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false))]);
        press(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
        k(&mut a, KeyCode::Down);
        k(&mut a, KeyCode::Down);
        k(&mut a, KeyCode::Up);
        assert_eq!(a.overlay, Overlay::Palette(Palette { query: String::new(), selected: 1 }));
        press(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(a.overlay, Overlay::None, "Ctrl+P referme");
    }

    #[test]
    fn aide() {
        let mut a = ready_app();
        press(&mut a, KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(a.overlay, Overlay::Help);
        k(&mut a, KeyCode::Esc);
        assert_eq!(a.overlay, Overlay::None);
        k(&mut a, KeyCode::F(1));
        k(&mut a, KeyCode::F(1));
        assert_eq!(a.overlay, Overlay::None);
    }

    #[test]
    fn recherche() {
        let mut a = ready_app();
        c(&mut a, '/');
        assert!(!a.search.editing, "pas de liste sur l'Accueil");
        c(&mut a, '2');
        c(&mut a, '/');
        assert!(a.search.editing);
        typed(&mut a, "casq");
        assert_eq!(a.items().len(), 2);
        k(&mut a, KeyCode::Enter);
        assert!(!a.search.editing);
        assert_eq!(a.search.text, "casq");
        k(&mut a, KeyCode::Down);
        assert_eq!(a.focus, 1);
        k(&mut a, KeyCode::Esc);
        assert_eq!(a.search, Search::default());
        assert_eq!(a.tab, Tab::Son, "le premier Échap efface le filtre");
    }

    #[test]
    fn entree_sur_une_sortie() {
        let mut a = ready_app();
        c(&mut a, '2');
        a.take_requests();
        k(&mut a, KeyCode::Enter);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDefault(64))]);
        assert_eq!(a.focused(), Some(Item::Slider(SliderId::Node(64))));
    }

    #[test]
    fn relachement_ignore() {
        let mut a = ready_app();
        let mut e = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        e.kind = KeyEventKind::Release;
        e.state = KeyEventState::NONE;
        a.on_key(e, Instant::now());
        assert!(!a.should_quit);
    }
}
