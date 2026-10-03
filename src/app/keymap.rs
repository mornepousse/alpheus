//! Table de touches : conventions standard (Ctrl+P, F1, Échap…), D/H/T/N en
//! plus (désactivables), toutes surchargeables dans config.toml.

use super::action::Action;
use crate::config::KeysConfig;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Une touche avec ses modificateurs, normalisée.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    /// Normalise un événement : pour un caractère, Maj est déjà dans le
    /// caractère (« ? », « + », « D ») et Maj+Tab arrive en BackTab, donc on
    /// retire SHIFT, sinon « ? » ne correspondrait jamais.
    pub fn from_event(e: &KeyEvent) -> Chord {
        let mut mods = e.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        if matches!(e.code, KeyCode::Char(_) | KeyCode::BackTab) {
            mods.remove(KeyModifiers::SHIFT);
        }
        let code = match e.code {
            // Ctrl+P peut arriver en « P » majuscule selon le terminal.
            KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => KeyCode::Char(c.to_ascii_lowercase()),
            c => c,
        };
        Chord { code, mods }
    }

    /// « ctrl+p », « f1 », « ? », « shift+tab », « space », « pageup », « - », « + »…
    pub fn parse(s: &str) -> Result<Chord, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("touche vide".into());
        }
        // « + » seul ou « ctrl++ » : le dernier « + » est la touche.
        let (mods_part, key) = if s == "+" {
            ("", "+")
        } else if let Some(p) = s.strip_suffix("++") {
            (p, "+")
        } else {
            s.rsplit_once('+').unwrap_or(("", s))
        };
        let mut mods = KeyModifiers::NONE;
        for m in mods_part.split('+').filter(|m| !m.is_empty()) {
            match m.to_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" => mods |= KeyModifiers::ALT,
                "shift" | "maj" => mods |= KeyModifiers::SHIFT,
                other => return Err(format!("modificateur inconnu : {other}")),
            }
        }
        let lower = key.to_lowercase();
        let mut code = match lower.as_str() {
            "esc" | "echap" | "échap" => KeyCode::Esc,
            "enter" | "entree" | "entrée" => KeyCode::Enter,
            "space" | "espace" => KeyCode::Char(' '),
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
                KeyCode::F(f[1..].parse().map_err(|_| format!("touche inconnue : {key}"))?)
            }
            _ => {
                let mut cs = key.chars();
                match (cs.next(), cs.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return Err(format!("touche inconnue : {key}")),
                }
            }
        };
        // Même normalisation que from_event.
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

    /// Libellé affiché : « Ctrl+P », « F1 », « Échap », « Maj+Tab », « q »…
    pub fn label(&self) -> String {
        let key = match self.code {
            KeyCode::Char(' ') => "Espace".to_string(),
            KeyCode::Char(c) if self.mods.contains(KeyModifiers::CONTROL) => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Esc => "Échap".into(),
            KeyCode::Enter => "Entrée".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "Maj+Tab".into(),
            KeyCode::Backspace => "Retour arr.".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::PageUp => "Page↑".into(),
            KeyCode::PageDown => "Page↓".into(),
            KeyCode::Home => "Début".into(),
            KeyCode::End => "Fin".into(),
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

fn default_keys(a: Action, dvorak: bool) -> Vec<&'static str> {
    let mut v = match a {
        Action::Quit => vec!["q"],
        Action::Back => vec!["esc"],
        Action::Help => vec!["f1", "?"],
        Action::Palette => vec!["ctrl+p"],
        Action::Search => vec!["/", "ctrl+f"],
        Action::NextTab => vec!["tab"],
        Action::PrevTab => vec!["shift+tab"],
        Action::GoTab(t) => vec![["1", "2", "3", "4", "5", "6"][t.index()]],
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

#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    /// Dans l'ordre d'Action::ALL (sert à l'aide).
    bindings: Vec<(Action, Vec<Chord>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::defaults(true)
    }
}

impl Keymap {
    pub fn defaults(dvorak_nav: bool) -> Keymap {
        let bindings = Action::ALL
            .into_iter()
            .map(|a| {
                let chords = default_keys(a, dvorak_nav)
                    .into_iter()
                    .map(|k| Chord::parse(k).expect("touche par défaut valide"))
                    .collect();
                (a, chords)
            })
            .collect();
        Keymap { bindings }
    }

    /// Défauts + surcharges ; une touche surchargée est retirée des autres actions.
    pub fn from_config(cfg: &KeysConfig) -> Result<Keymap, String> {
        let mut km = Keymap::defaults(cfg.dvorak_nav);
        for (name, keys) in &cfg.bindings {
            let action = Action::from_name(name).ok_or_else(|| format!("action inconnue : {name}"))?;
            let chords = keys.iter().map(|k| Chord::parse(k)).collect::<Result<Vec<_>, _>>()?;
            km.set(action, chords);
        }
        Ok(km)
    }

    fn set(&mut self, action: Action, chords: Vec<Chord>) {
        for (a, cs) in &mut self.bindings {
            if *a == action {
                *cs = chords.clone();
            } else {
                cs.retain(|c| !chords.contains(c));
            }
        }
    }

    pub fn lookup(&self, e: &KeyEvent) -> Option<Action> {
        let c = Chord::from_event(e);
        self.bindings.iter().find(|(_, cs)| cs.contains(&c)).map(|(a, _)| *a)
    }

    pub fn keys(&self, a: Action) -> &[Chord] {
        self.bindings.iter().find(|(b, _)| *b == a).map(|(_, cs)| cs.as_slice()).unwrap_or(&[])
    }

    /// Première touche de l'action (barre de raccourcis), vide si aucune.
    pub fn label(&self, a: Action) -> String {
        self.keys(a).first().map(Chord::label).unwrap_or_default()
    }

    pub fn all(&self) -> &[(Action, Vec<Chord>)] {
        &self.bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::action::Tab;
    use std::collections::BTreeMap;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
    fn ch(c: char) -> KeyEvent {
        ev(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn touches_par_defaut() {
        let k = Keymap::default();
        assert_eq!(k.lookup(&ch('q')), Some(Action::Quit));
        assert_eq!(k.lookup(&ev(KeyCode::F(1), KeyModifiers::NONE)), Some(Action::Help));
        assert_eq!(k.lookup(&ev(KeyCode::Char('p'), KeyModifiers::CONTROL)), Some(Action::Palette));
        assert_eq!(k.lookup(&ch('3')), Some(Action::GoTab(Tab::Reseau)));
        assert_eq!(k.lookup(&ch('6')), Some(Action::GoTab(Tab::Ecrans)));
        assert_eq!(k.lookup(&ch(' ')), Some(Action::Toggle));
        assert_eq!(k.lookup(&ev(KeyCode::Enter, KeyModifiers::NONE)), Some(Action::Activate));
        assert_eq!(k.lookup(&ch('-')), Some(Action::Decrease));
        assert_eq!(k.lookup(&ch('/')), Some(Action::Search));
        assert_eq!(k.lookup(&ev(KeyCode::Char('f'), KeyModifiers::CONTROL)), Some(Action::Search));
    }

    #[test]
    fn touches_avec_maj() {
        // Le terminal envoie SHIFT avec « ? », « + » et Maj+Tab.
        let k = Keymap::default();
        assert_eq!(k.lookup(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT)), Some(Action::Help));
        assert_eq!(k.lookup(&ev(KeyCode::Char('+'), KeyModifiers::SHIFT)), Some(Action::Increase));
        assert_eq!(k.lookup(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)), Some(Action::PrevTab));
    }

    #[test]
    fn dvorak_desactivable() {
        assert_eq!(Keymap::defaults(true).lookup(&ch('d')), Some(Action::Left));
        assert_eq!(Keymap::defaults(true).lookup(&ch('h')), Some(Action::Down));
        assert_eq!(Keymap::defaults(true).lookup(&ch('t')), Some(Action::Up));
        assert_eq!(Keymap::defaults(true).lookup(&ch('n')), Some(Action::Right));
        assert_eq!(Keymap::defaults(false).lookup(&ch('d')), None);
    }

    #[test]
    fn analyse_des_touches() {
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
    fn libelles() {
        let k = Keymap::default();
        assert_eq!(k.label(Action::Help), "F1");
        assert_eq!(k.label(Action::Palette), "Ctrl+P");
        assert_eq!(k.label(Action::Back), "Échap");
        assert_eq!(k.label(Action::Quit), "q");
        assert_eq!(k.label(Action::Toggle), "Espace");
        assert_eq!(k.label(Action::PrevTab), "Maj+Tab");
    }

    #[test]
    fn surcharge_et_vol_de_touche() {
        let mut b = BTreeMap::new();
        b.insert("palette".to_string(), vec!["q".to_string()]);
        b.insert("quit".to_string(), vec!["x".to_string()]);
        let k = Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap();
        assert_eq!(k.lookup(&ch('q')), Some(Action::Palette));
        assert_eq!(k.lookup(&ch('x')), Some(Action::Quit));
        assert_eq!(k.lookup(&ev(KeyCode::Char('p'), KeyModifiers::CONTROL)), None);
    }

    #[test]
    fn surcharge_vole_une_touche_a_une_action_non_surchargee() {
        // Seul « palette » est surchargé ; « quit » garde son défaut ailleurs,
        // mais doit perdre « q » au profit de « palette ».
        let mut b = BTreeMap::new();
        b.insert("palette".to_string(), vec!["q".to_string()]);
        let k = Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap();
        assert_eq!(k.lookup(&ch('q')), Some(Action::Palette));
        assert!(!k.keys(Action::Quit).contains(&Chord::parse("q").unwrap()));
    }

    #[test]
    fn surcharge_invalide() {
        let mut b = BTreeMap::new();
        b.insert("voler".to_string(), vec!["v".to_string()]);
        assert!(Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap_err().contains("voler"));
        let mut b = BTreeMap::new();
        b.insert("quit".to_string(), vec!["hyper+q".to_string()]);
        assert!(Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).is_err());
    }
}
