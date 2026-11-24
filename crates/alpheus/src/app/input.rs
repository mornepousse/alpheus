//! The keyboard: which part of the `App` a key press goes to, and what the keys mean there.
//!
//! A key goes to the first of these that is open: an overlay (a confirmation, the palette, help,
//! the display trial's Keep box, an output's settings panel, the save-profile box), then the
//! search box, then the Diag tab's own views, then the key table (`keymap.rs`), whose `Action`
//! `do_action` carries out. The mouse equivalent is in `mouse.rs`.
//!
//! Never:
//! - run a tool or read anything: a key only changes the `App` (and queues requests, through the
//!   methods it calls);
//! - read the clock: `on_key` takes `now` from its caller.
//!
//! See: docs/features/control-center.md, docs/specs/2026-10-02-alpheus-v1-design.md (§ 2, "Keyboard").

use super::action::{Action, Tab};
use super::{App, Confirm, Item, Overlay, Palette, PanelField, Search};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::Instant;

/// What one key press means inside the command palette; see `key_palette`.
enum PaletteKey {
    Close,
    Run,
    Up,
    Down,
    Pop,
    Push(char),
    Ignore,
}

/// A key with no Ctrl or Alt held: a character typed, not a shortcut.
fn plain(e: &KeyEvent) -> bool {
    !e.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

impl App {
    /// Handles one key. A release is ignored (only presses and repeats act).
    pub fn on_key(&mut self, e: KeyEvent, now: Instant) {
        if e.kind == KeyEventKind::Release {
            return;
        }
        // `return self.key_x(e)` ends this function with what `key_x` returns (nothing): an open
        // overlay takes the key and nothing below sees it.
        match self.overlay {
            Overlay::Confirm(_) | Overlay::UnitConfirm { .. } => return self.key_confirm(e),
            Overlay::Palette(_) => return self.key_palette(e, now),
            Overlay::Help => return self.key_help(e),
            Overlay::Keep(_) => return self.key_keep(e),
            Overlay::Panel(_) => return self.key_panel(e, now),
            Overlay::SaveProfile(_) => return self.key_save(e, now),
            Overlay::None => {}
        }
        if self.search.editing {
            return self.key_search(e);
        }
        if self.tab == Tab::Diagnostics {
            self.diag_touch();
        }
        if self.journal_key(e) || self.report_views_key(e, now) {
            return;
        }
        if let Some(a) = self.keymap.lookup(&e) {
            self.do_action(a, now);
        }
    }

    #[cfg(test)]
    pub(crate) fn on_key_action(&mut self, a: Action) {
        self.do_action(a, Instant::now());
    }

    /// A power or unit confirmation: Esc, Back and Quit cancel; Enter answers with the focused
    /// button. A confirmation starts on "Cancel" (`Confirm::yes_focused`), so Enter by reflex
    /// powers nothing off.
    fn key_confirm(&mut self, e: KeyEvent) {
        let action = if e.code == KeyCode::Esc { Some(Action::Back) } else { self.keymap.lookup(&e) };
        match action {
            Some(Action::Back | Action::Quit) => self.close_confirm(false),
            Some(Action::Activate) => {
                let yes = matches!(
                    self.overlay,
                    Overlay::Confirm(Confirm { yes_focused: true, .. }) | Overlay::UnitConfirm { yes_focused: true, .. }
                );
                self.close_confirm(yes);
            }
            // Arrow keys: "Cancel" is on the left, "Confirm" on the right;
            // Tab and Shift+Tab toggle between them.
            Some(a @ (Action::Left | Action::Right | Action::NextTab | Action::PrevTab)) => {
                if let Overlay::Confirm(Confirm { yes_focused, .. }) | Overlay::UnitConfirm { yes_focused, .. } = &mut self.overlay {
                    *yes_focused = match a {
                        Action::Left => false,
                        Action::Right => true,
                        _ => !*yes_focused,
                    };
                }
            }
            _ => {}
        }
    }

    /// "Keep these settings?": ← Cancel, → Keep, Esc cancels; "q" closes
    /// alpheus, and closing asks the daemon to revert the trial.
    fn key_keep(&mut self, e: KeyEvent) {
        let action = if e.code == KeyCode::Esc { Some(Action::Back) } else { self.keymap.lookup(&e) };
        match action {
            Some(Action::Quit) => self.should_quit = true,
            Some(Action::Back) => self.keep_answer(false),
            Some(Action::Activate) => {
                let keep = matches!(self.overlay, Overlay::Keep(k) if k.keep_focused);
                self.keep_answer(keep);
            }
            Some(a @ (Action::Left | Action::Right | Action::NextTab | Action::PrevTab)) => {
                if let Overlay::Keep(k) = &mut self.overlay {
                    k.keep_focused = match a {
                        Action::Left => false,
                        Action::Right => true,
                        _ => !k.keep_focused,
                    };
                }
            }
            _ => {}
        }
    }

    /// Settings panel: ↑/↓ field, ←/→ value, "/" searches a mode, Enter on
    /// Apply sends, Esc closes.
    fn key_panel(&mut self, e: KeyEvent, now: Instant) {
        if matches!(&self.overlay, Overlay::Panel(p) if p.query.is_some()) {
            match e.code {
                KeyCode::Esc | KeyCode::Enter => {
                    if let Overlay::Panel(p) = &mut self.overlay {
                        p.query = None; // the chosen mode remains
                    }
                }
                KeyCode::Backspace => self.panel_query(|q| {
                    q.pop();
                }),
                KeyCode::Left => self.panel_step(-1),
                KeyCode::Right => self.panel_step(1),
                KeyCode::Char(ch) if plain(&e) => self.panel_query(|q| q.push(ch)),
                _ => {}
            }
            return;
        }
        let action = if e.code == KeyCode::Esc { Some(Action::Back) } else { self.keymap.lookup(&e) };
        match action {
            Some(Action::Back | Action::Quit) => self.overlay = Overlay::None,
            Some(Action::Up | Action::PrevTab) => self.panel_move(-1),
            Some(Action::Down | Action::NextTab) => self.panel_move(1),
            Some(Action::Left | Action::Decrease) => self.panel_step(-1),
            Some(Action::Right | Action::Increase) => self.panel_step(1),
            Some(Action::Search) => {
                if let Overlay::Panel(p) = &mut self.overlay
                    && p.field == PanelField::Mode
                {
                    p.query = Some(String::new());
                }
            }
            Some(Action::Activate) => self.panel_enter(now),
            _ => {}
        }
    }

    /// The save-profile box: the name is typed, then, if that name already exists, a replace
    /// confirmation (`SaveDialog::replace`) with the same buttons as the other boxes.
    fn key_save(&mut self, e: KeyEvent, now: Instant) {
        let replace = matches!(&self.overlay, Overlay::SaveProfile(d) if d.replace);
        if replace {
            let action = if e.code == KeyCode::Esc { Some(Action::Back) } else { self.keymap.lookup(&e) };
            match action {
                Some(Action::Back | Action::Quit) => self.overlay = Overlay::None,
                Some(Action::Activate) => self.save_enter(now),
                Some(a @ (Action::Left | Action::Right | Action::NextTab | Action::PrevTab)) => {
                    if let Overlay::SaveProfile(d) = &mut self.overlay {
                        d.yes_focused = match a {
                            Action::Left => false,
                            Action::Right => true,
                            _ => !d.yes_focused,
                        };
                    }
                }
                _ => {}
            }
            return;
        }
        match e.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => self.save_enter(now),
            KeyCode::Backspace => {
                if let Overlay::SaveProfile(d) = &mut self.overlay {
                    d.name.pop();
                }
            }
            KeyCode::Char(ch) if plain(&e) => {
                if let Overlay::SaveProfile(d) = &mut self.overlay {
                    d.name.push(ch);
                }
            }
            _ => {}
        }
    }

    /// The help box: Esc, Back, Help or Quit close it; every other key is ignored.
    fn key_help(&mut self, e: KeyEvent) {
        let a = self.keymap.lookup(&e);
        if e.code == KeyCode::Esc || matches!(a, Some(Action::Back | Action::Help | Action::Quit)) {
            self.overlay = Overlay::None;
        }
    }

    /// The command palette: text typed filters the entries and puts the cursor on the first; Up,
    /// Down and Tab move it; Enter runs the entry; Esc or the palette key closes. A key with Ctrl or
    /// Alt is no text.
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

    /// The search box after `/`: text filters the tab's list; Enter keeps the filter and gives the
    /// keys back; Esc clears it. Any other key is ignored and does not move the focus.
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

    /// Carries out a key-table action. The Theme tab and the Diag tab, when shown, get the first
    /// try (`appearance_action`, `diagnostics_action`): an action they handle stops here.
    fn do_action(&mut self, a: Action, now: Instant) {
        if self.tab == Tab::Appearance && self.appearance_action(a, now) {
            return;
        }
        if self.diagnostics_action(a, now) {
            return;
        }
        match a {
            Action::Quit => self.should_quit = true,
            // Back undoes one thing at a time: a search first, then the tab (back to Home).
            Action::Back => {
                if !self.search.text.is_empty() {
                    self.search = Search::default();
                    self.clamp_focus();
                } else if self.tab != Tab::Home {
                    self.go_tab(Tab::Home, now);
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
            Action::Toggle => match self.focused() {
                Some(Item::Toggle(id)) => self.toggle(id, now),
                Some(Item::Output(name)) => self.toggle_output(&name, now),
                _ => {}
            },
            Action::Activate => self.activate(now),
            // Only the Diag tab acts on these two, and `diagnostics_action` took them above; on any
            // other tab they do nothing.
            Action::UnitRestart | Action::UnitReset => {}
            Action::Refresh => self.refresh_tab(now),
        }
    }
}

#[cfg(test)]
mod tests;
