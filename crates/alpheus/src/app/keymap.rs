//! The key table: which key press is which `Action`.
//!
//! `Chord` is one key with its modifiers, written and read as text ("ctrl+p", "shift+tab", "f1").
//! `Keymap` holds, for each `Action`, the chords bound to it: the standard conventions (Ctrl+P,
//! F1, Esc, the arrows) plus `d`, `h`, `t`, `n` for left, down, up and right (Dvorak navigation,
//! off by default; `[keys] dvorak_nav = true` turns it on). `[keys]` in config.toml overrides any
//! of them by action name. `input.rs` asks `lookup` what a key press means; the help box and the shortcut
//! hints (the classic bar and the rail's keycaps, both through `shortcut_pairs`) ask for the labels.
//!
//! Never:
//! - read a file or a config itself: it is handed a `KeysConfig`;
//! - leave one chord on two actions: an override takes its keys from every other action.
//!
//! See: docs/features/control-center.md ("The default keymap").

use super::action::Action;
use crate::config::KeysConfig;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key with its modifiers, normalized: a character never carries Shift (see `from_event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    /// Normalizes an event: for a character, Shift is already baked into the
    /// character ("?", "+", "D") and Shift+Tab arrives as BackTab, so SHIFT
    /// is removed — otherwise "?" would never match.
    pub fn from_event(e: &KeyEvent) -> Chord {
        let mut mods = e.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        if matches!(e.code, KeyCode::Char(_) | KeyCode::BackTab) {
            mods.remove(KeyModifiers::SHIFT);
        }
        let code = match e.code {
            // Ctrl+P may arrive as uppercase "P" depending on the terminal.
            KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => KeyCode::Char(c.to_ascii_lowercase()),
            c => c,
        };
        Chord { code, mods }
    }

    /// "ctrl+p", "f1", "?", "shift+tab", "space", "pageup", "-", "+"…
    pub fn parse(s: &str) -> Result<Chord, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty key".into());
        }
        // A lone "+", or "ctrl++": the last "+" is the key.
        let (mods_part, key) = if s == "+" {
            ("", "+")
        } else if let Some(p) = s.strip_suffix("++") {
            (p, "+")
        } else {
            match s.rsplit_once('+') {
                Some(pair) => pair,
                None => ("", s),
            }
        };
        let mut mods = KeyModifiers::NONE;
        for m in mods_part.split('+').filter(|m| !m.is_empty()) {
            match m.to_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                other => return Err(format!("unknown modifier: {other}")),
            }
        }
        let lower = key.to_lowercase();
        let mut code = match lower.as_str() {
            "esc" => KeyCode::Esc,
            "enter" => KeyCode::Enter,
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            f if f.len() > 1 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                KeyCode::F(f[1..].parse().map_err(|_| format!("unknown key: {key}"))?)
            }
            _ => {
                let mut cs = key.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return Err(format!("unknown key: {key}")),
                }
            }
        };
        // Same normalization as from_event.
        if code == KeyCode::Tab && mods.contains(KeyModifiers::SHIFT) {
            code = KeyCode::BackTab;
        }
        if let KeyCode::Char(c) = code {
            if mods.contains(KeyModifiers::SHIFT) {
                code = KeyCode::Char(c.to_ascii_uppercase());
            }
            if mods.contains(KeyModifiers::CONTROL) {
                code = KeyCode::Char(c.to_ascii_lowercase());
            }
        }
        if matches!(code, KeyCode::Char(_) | KeyCode::BackTab) {
            mods.remove(KeyModifiers::SHIFT);
        }
        Ok(Chord { code, mods })
    }

    /// Displayed label: "Ctrl+P", "F1", "Esc", "Shift+Tab", "q"…
    pub fn label(&self) -> String {
        let key = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) if self.mods.contains(KeyModifiers::CONTROL) => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "Shift+Tab".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::PageUp => "Page↑".into(),
            KeyCode::PageDown => "Page↓".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            _ => "?".into(),
        };
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("Ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("Alt+");
        }
        s + &key
    }
}

/// The keys an action has when config.toml says nothing, as text `Chord::parse` reads. The
/// Dvorak keys are added only when `dvorak` is set. A tab key is its digit (`tab::digit`), none
/// for a tab past the ninth.
fn default_keys(a: Action, dvorak: bool) -> Vec<&'static str> {
    let mut v = match a {
        Action::Quit => vec!["q"],
        Action::Back => vec!["esc"],
        Action::Help => vec!["f1", "?"],
        Action::Palette => vec!["ctrl+p"],
        Action::Search => vec!["/", "ctrl+f"],
        Action::NextTab => vec!["tab"],
        Action::PrevTab => vec!["shift+tab"],
        Action::GoTab(t) => super::tab::digit(t.index()).into_iter().collect(),
        Action::Left => vec!["left"],
        Action::Right => vec!["right"],
        Action::Up => vec!["up"],
        Action::Down => vec!["down"],
        Action::PageUp => vec!["pageup"],
        Action::PageDown => vec!["pagedown"],
        Action::Home => vec!["home"],
        Action::End => vec!["end"],
        Action::Toggle => vec!["space"],
        Action::Activate => vec!["enter"],
        Action::Decrease => vec!["-"],
        Action::Increase => vec!["+"],
        Action::UnitRestart => vec!["r"],
        Action::UnitReset => vec!["c"],
        Action::Refresh => vec!["f5"],
    };
    if dvorak {
        match a {
            Action::Left => v.push("d"),
            Action::Down => v.push("h"),
            Action::Up => v.push("t"),
            Action::Right => v.push("n"),
            _ => {}
        }
    }
    v
}

/// The chords bound to each action.
#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    /// In Action::ALL order (used by the help screen).
    bindings: Vec<(Action, Vec<Chord>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::defaults(KeysConfig::default().dvorak_nav)
    }
}

impl Keymap {
    /// The default table. The keys are written in this file, so `expect` cannot fail while the
    /// tests that build `Keymap::default()` pass.
    pub fn defaults(dvorak_nav: bool) -> Keymap {
        let bindings = Action::ALL
            .into_iter()
            .map(|a| {
                let chords = default_keys(a, dvorak_nav).into_iter().map(|k| Chord::parse(k).expect("valid default key")).collect();
                (a, chords)
            })
            .collect();
        Keymap { bindings }
    }

    /// Defaults + overrides; an overridden key is removed from other actions.
    pub fn from_config(cfg: &KeysConfig) -> Result<Keymap, String> {
        let mut km = Keymap::defaults(cfg.dvorak_nav);
        for (name, keys) in &cfg.bindings {
            let action = Action::from_name(name).ok_or_else(|| format!("unknown action: {name}"))?;
            let chords = keys.iter().map(|k| Chord::parse(k)).collect::<Result<Vec<_>, _>>()?;
            km.set(action, chords);
        }
        Ok(km)
    }

    /// Binds `action` to exactly `chords` and takes those chords away from every other action.
    fn set(&mut self, action: Action, chords: Vec<Chord>) {
        for (a, cs) in &mut self.bindings {
            if *a == action {
                *cs = chords.clone();
            } else {
                cs.retain(|c| !chords.contains(c));
            }
        }
    }

    /// The action a key press is bound to, if any.
    pub fn lookup(&self, e: &KeyEvent) -> Option<Action> {
        let c = Chord::from_event(e);
        for (action, chords) in &self.bindings {
            if chords.contains(&c) {
                return Some(*action);
            }
        }
        None
    }

    /// The chords bound to `a` (none when the user unbound it).
    pub fn keys(&self, a: Action) -> &[Chord] {
        for (action, chords) in &self.bindings {
            if *action == a {
                return chords.as_slice();
            }
        }
        &[]
    }

    /// First key of the action (shortcut bar), empty if none.
    pub fn label(&self, a: Action) -> String {
        match self.keys(a).first() {
            Some(chord) => chord.label(),
            None => String::new(),
        }
    }

    /// Every action with its chords, in `Action::ALL` order.
    pub fn all(&self) -> &[(Action, Vec<Chord>)] {
        &self.bindings
    }
}

#[cfg(test)]
mod tests {
    use super::super::action::Tab;
    use super::*;
    use std::collections::BTreeMap;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
    fn ch(c: char) -> KeyEvent {
        ev(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn the_default_keys_reach_their_actions() {
        let k = Keymap::default();
        assert_eq!(k.lookup(&ch('q')), Some(Action::Quit));
        assert_eq!(k.lookup(&ev(KeyCode::F(1), KeyModifiers::NONE)), Some(Action::Help));
        assert_eq!(k.lookup(&ev(KeyCode::Char('p'), KeyModifiers::CONTROL)), Some(Action::Palette));
        assert_eq!(k.lookup(&ch('3')), Some(Action::GoTab(Tab::Network)));
        assert_eq!(k.lookup(&ch('6')), Some(Action::GoTab(Tab::Displays)));
        assert_eq!(k.lookup(&ch('7')), Some(Action::GoTab(Tab::Appearance)));
        assert_eq!(k.lookup(&ch(' ')), Some(Action::Toggle));
        assert_eq!(k.lookup(&ev(KeyCode::Enter, KeyModifiers::NONE)), Some(Action::Activate));
        assert_eq!(k.lookup(&ch('-')), Some(Action::Decrease));
        assert_eq!(k.lookup(&ch('/')), Some(Action::Search));
        assert_eq!(k.lookup(&ch('r')), Some(Action::UnitRestart));
        assert_eq!(k.lookup(&ch('c')), Some(Action::UnitReset));
        assert_eq!(k.lookup(&ev(KeyCode::F(5), KeyModifiers::NONE)), Some(Action::Refresh));
        assert_eq!(k.lookup(&ev(KeyCode::Char('f'), KeyModifiers::CONTROL)), Some(Action::Search));
    }

    #[test]
    fn a_shifted_key_is_told_from_its_plain_one() {
        // The terminal sends SHIFT along with "?", "+" and Shift+Tab.
        let k = Keymap::default();
        assert_eq!(k.lookup(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT)), Some(Action::Help));
        assert_eq!(k.lookup(&ev(KeyCode::Char('+'), KeyModifiers::SHIFT)), Some(Action::Increase));
        assert_eq!(k.lookup(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)), Some(Action::PrevTab));
    }

    #[test]
    fn dvorak_is_off_by_default_and_can_be_enabled() {
        assert_eq!(Keymap::default().lookup(&ch('d')), None);
        assert_eq!(Keymap::default().lookup(&ch('h')), None);
        assert_eq!(Keymap::from_config(&KeysConfig::default()).unwrap().lookup(&ch('t')), None);
        assert_eq!(Keymap::defaults(true).lookup(&ch('d')), Some(Action::Left));
        assert_eq!(Keymap::defaults(true).lookup(&ch('h')), Some(Action::Down));
        assert_eq!(Keymap::defaults(true).lookup(&ch('t')), Some(Action::Up));
        assert_eq!(Keymap::defaults(true).lookup(&ch('n')), Some(Action::Right));
        assert_eq!(Keymap::defaults(false).lookup(&ch('d')), None);
    }

    #[test]
    fn a_binding_is_parsed_from_its_text() {
        assert_eq!(Chord::parse("ctrl+p"), Ok(Chord { code: KeyCode::Char('p'), mods: KeyModifiers::CONTROL }));
        assert_eq!(Chord::parse("Ctrl+P"), Ok(Chord { code: KeyCode::Char('p'), mods: KeyModifiers::CONTROL }));
        assert_eq!(Chord::parse("shift+tab"), Ok(Chord { code: KeyCode::BackTab, mods: KeyModifiers::NONE }));
        assert_eq!(Chord::parse("+"), Ok(Chord { code: KeyCode::Char('+'), mods: KeyModifiers::NONE }));
        assert_eq!(Chord::parse("F12"), Ok(Chord { code: KeyCode::F(12), mods: KeyModifiers::NONE }));
        assert!(Chord::parse("ctrl+").is_err());
        assert!(Chord::parse("hyper+x").is_err());
        assert!(Chord::parse("foo").is_err());
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn every_binding_has_a_label() {
        let k = Keymap::default();
        assert_eq!(k.label(Action::Help), "F1");
        assert_eq!(k.label(Action::Palette), "Ctrl+P");
        assert_eq!(k.label(Action::Back), "Esc");
        assert_eq!(k.label(Action::Quit), "q");
        assert_eq!(k.label(Action::Toggle), "Space");
        assert_eq!(k.label(Action::PrevTab), "Shift+Tab");
    }

    #[test]
    fn override_and_key_theft() {
        let mut b = BTreeMap::new();
        b.insert("palette".to_string(), vec!["q".to_string()]);
        b.insert("quit".to_string(), vec!["x".to_string()]);
        let k = Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap();
        assert_eq!(k.lookup(&ch('q')), Some(Action::Palette));
        assert_eq!(k.lookup(&ch('x')), Some(Action::Quit));
        assert_eq!(k.lookup(&ev(KeyCode::Char('p'), KeyModifiers::CONTROL)), None);
    }

    #[test]
    fn override_steals_a_key_from_an_action_not_itself_overridden() {
        // Only "palette" is overridden; "quit" keeps its default elsewhere,
        // but must lose "q" to "palette".
        let mut b = BTreeMap::new();
        b.insert("palette".to_string(), vec!["q".to_string()]);
        let k = Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap();
        assert_eq!(k.lookup(&ch('q')), Some(Action::Palette));
        assert!(!k.keys(Action::Quit).contains(&Chord::parse("q").unwrap()));
    }

    #[test]
    fn an_invalid_override_is_refused_with_a_reason() {
        let mut b = BTreeMap::new();
        b.insert("fly".to_string(), vec!["v".to_string()]);
        assert!(Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap_err().contains("fly"));
        let mut b = BTreeMap::new();
        b.insert("quit".to_string(), vec!["hyper+q".to_string()]);
        assert!(Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).is_err());
    }
}
