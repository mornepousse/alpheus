//! Rendu ratatui. `render` dessine tout et renvoie la carte des zones
//! cliquables : chaque composant connaît sa zone pour le test de clic.

pub mod draw;
pub mod hit;
mod tabs;

pub use hit::{HitMap, Target};

use crate::app::action::{Action, Tab};
use crate::app::App;
use draw::put;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

/// En dessous, seul un message est affiché (jamais de panique).
pub const MIN_W: u16 = 30;
pub const MIN_H: u16 = 8;

pub fn render(f: &mut Frame, app: &App) -> HitMap {
    let area = f.area();
    let buf = f.buffer_mut();
    let mut hits = HitMap::default();
    let th = &app.theme;
    draw::clear(buf, area, Style::default().fg(th.fg).bg(th.bg));
    if area.width < MIN_W || area.height < MIN_H {
        put(buf, area, area.x, area.y, "alpheus : fenêtre trop petite", Style::default().fg(th.error));
        return hits;
    }
    let bar = |y| Rect::new(area.x, y, area.width, 1);
    let body = Rect::new(area.x + 1, area.y + 2, area.width - 2, area.height - 4);
    tab_bar(buf, bar(area.y), app, &mut hits);
    tabs::render(buf, body, app, &mut hits);
    status_line(buf, bar(area.bottom() - 2), app);
    shortcut_bar(buf, bar(area.bottom() - 1), app);
    hits
}

fn tab_bar(buf: &mut Buffer, r: Rect, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let mut x = r.x + 1;
    x += put(buf, r, x, r.y, "alpheus", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)) + 2;
    for (i, t) in Tab::ALL.iter().enumerate() {
        let label = format!(" {} {} ", i + 1, t.title());
        let style = if *t == app.tab {
            Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.fg)
        };
        let w = put(buf, r, x, r.y, &label, style);
        hits.push(Rect::new(x, r.y, w, 1), Target::Tab(*t));
        x = x.saturating_add(w + 1);
    }
}

fn status_line(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let style = Style::default().fg(th.fg).bg(th.surface);
    draw::clear(buf, r, style);
    let last = app.status.last.as_deref().unwrap_or("prêt");
    let mut x = r.x + 1;
    x += put(buf, r, x, r.y, &format!("{} · {last}", app.tab.title()), style);
    if let Some(err) = &app.status.error {
        put(buf, r, x, r.y, &format!(" · ✗ {err}"), Style::default().fg(th.error).bg(th.surface));
    }
}

fn shortcut_bar(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let km = &app.keymap;
    let mut pairs: Vec<(String, &str)> = Vec::new();
    if app.search.editing {
        pairs.push(("Entrée".into(), "Valider"));
        pairs.push(("Échap".into(), "Effacer"));
    } else {
        pairs.push((km.label(Action::Help), "Aide"));
        pairs.push((km.label(Action::Palette), "Palette"));
        pairs.push((km.label(Action::Back), "Retour"));
        pairs.push((km.label(Action::Quit), "Quitter"));
        let adjust = format!("{}/{}", km.label(Action::Decrease), km.label(Action::Increase));
        match app.tab {
            Tab::Accueil => {
                pairs.push((km.label(Action::Toggle), "Basculer"));
                pairs.push((adjust, "Régler"));
            }
            Tab::Son => {
                pairs.push((km.label(Action::Activate), "Par défaut"));
                pairs.push((adjust, "Volume"));
                pairs.push((km.label(Action::Search), "Chercher"));
            }
            Tab::Reseau | Tab::Bluetooth => {
                pairs.push((km.label(Action::Toggle), "Basculer"));
                pairs.push((km.label(Action::Activate), "Ouvrir"));
                pairs.push((km.label(Action::Search), "Chercher"));
            }
            Tab::Alimentation => pairs.push((km.label(Action::Activate), "Activer")),
        }
    }
    let mut x = r.x + 1;
    for (key, what) in pairs {
        if key.is_empty() {
            continue; // action sans touche (désaffectée dans config.toml)
        }
        x = x.saturating_add(put(buf, r, x, r.y, &key, Style::default().fg(th.accent).add_modifier(Modifier::BOLD)));
        x = x.saturating_add(put(buf, r, x, r.y, &format!(" {what}  "), Style::default().fg(th.fg)));
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Dessine `app` sur un terminal w×h ; renvoie les lignes et les zones.
    pub fn draw(app: &App, w: u16, h: u16) -> (Vec<String>, HitMap) {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitMap::default();
        t.draw(|f| hits = render(f, app)).unwrap();
        let buf = t.backend().buffer();
        let rows = (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect();
        (rows, hits)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::draw;
    use super::*;
    use crate::app::testing::{app, node, ready_app};
    use crate::app::keymap::Keymap;
    use crate::app::{Loadable, SliderId};
    use crate::config::KeysConfig;
    use std::collections::BTreeMap;
    use std::time::Instant;

    #[test]
    fn cadre_de_l_accueil() {
        let (rows, _) = draw(&ready_app(), 80, 24);
        assert!(rows[0].contains("alpheus") && rows[0].contains(" 1 Accueil ") && rows[0].contains(" 5 Alimentation "));
        assert!(rows[2].starts_with(" ▶ Volume"), "{:?}", rows[2]);
        assert!(rows[2].contains("  62 %"));
        assert!(rows[4].contains("Luminosité") && rows[4].contains("  20 %"));
        assert!(rows[6].contains("Wi-Fi") && rows[6].contains("[■] activé"));
        assert!(rows[10].contains("Ne pas déranger") && rows[10].contains("[ ] désactivé"));
        assert!(rows[14].contains("Profil d'énergie") && rows[14].contains("Équilibré"));
        assert!(rows[22].contains("Accueil · prêt"));
        assert!(rows[23].contains("F1 Aide  Ctrl+P Palette  Échap Retour  q Quitter"), "{:?}", rows[23]);
        assert!(rows[23].contains("Espace Basculer"));
    }

    #[test]
    fn barre_de_curseur_proportionnelle() {
        let (rows, _) = draw(&ready_app(), 80, 24);
        // 62 % de 24 cases = 15 pleines.
        assert!(rows[2].contains(&format!("{}{}", "█".repeat(15), "░".repeat(9))), "{:?}", rows[2]);
    }

    #[test]
    fn zones_cliquables() {
        let (_, hits) = draw(&ready_app(), 80, 24);
        let son = hits.find(Target::Tab(Tab::Son)).unwrap();
        assert_eq!((son.y, son.width), (0, 7));
        assert_eq!(hits.find(Target::Slider { item: 0 }), Some(Rect::new(23, 2, 24, 1)));
        assert_eq!(hits.at(30, 2).map(|h| h.1), Some(Target::Slider { item: 0 }));
        assert_eq!(hits.at(5, 6).map(|h| h.1), Some(Target::Item(2)));
        assert!(hits.find(Target::ProfileOption(2)).is_some());
    }

    #[test]
    fn chargement_et_echec() {
        let (rows, _) = draw(&app(), 80, 24);
        assert!(rows[2].contains('…'));
        let mut a = ready_app();
        a.data.brightness = Loadable::Failed("brightnessctl introuvable".into());
        let (rows, hits) = draw(&a, 80, 24);
        assert!(rows[4].contains("✗ brightnessctl introuvable"));
        assert!(hits.find(Target::Slider { item: 1 }).is_none());
    }

    #[test]
    fn onglet_son() {
        let mut a = ready_app();
        a.go_tab(Tab::Son, Instant::now());
        let (rows, hits) = draw(&a, 80, 24);
        let all = rows.join("\n");
        for attendu in ["Sorties", "● CasqueBT", "Dock D6000 Dell", "Entrées", "● Stereo Microphone", "[ Mixage par appli ]"] {
            assert!(all.contains(attendu), "{attendu} absent :\n{all}");
        }
        assert!(hits.find(Target::Slider { item: 2 }).is_some());
        assert!(hits.find(Target::Item(3)).is_some(), "bouton cliquable");
        assert!(rows[23].contains("Entrée Par défaut"));
    }

    #[test]
    fn onglets_reseau_bluetooth_alimentation() {
        let mut a = ready_app();
        a.go_tab(Tab::Reseau, Instant::now());
        let all = draw(&a, 80, 24).0.join("\n");
        assert!(all.contains("Connexions actives") && all.contains("MON_WIFI_1") && all.contains("[ Choisir un réseau ]"));
        a.go_tab(Tab::Bluetooth, Instant::now());
        let all = draw(&a, 80, 24).0.join("\n");
        assert!(all.contains("Appareils connectés") && all.contains("CasqueBT") && all.contains("[ Appairer un appareil ]"));
        a.go_tab(Tab::Alimentation, Instant::now());
        let all = draw(&a, 80, 24).0.join("\n");
        assert!(all.contains("[ Verrouiller ]") && all.contains("[ Éteindre ]"));
    }

    #[test]
    fn ligne_de_recherche() {
        let mut a = ready_app();
        a.go_tab(Tab::Son, Instant::now());
        a.search.editing = true;
        a.search.text = "casq".into();
        let (rows, _) = draw(&a, 80, 24);
        assert!(rows[2].contains("Recherche : casq▏"));
        assert!(!rows.join("\n").contains("Dock D6000 Dell"));
        assert!(rows[23].contains("Entrée Valider") && rows[23].contains("Échap Effacer"));
    }

    #[test]
    fn erreur_dans_la_ligne_d_etat() {
        let mut a = ready_app();
        a.warn("config.toml invalide : x".into());
        assert!(draw(&a, 80, 24).0[22].contains("✗ config.toml invalide : x"));
    }

    #[test]
    fn touche_reconfiguree_affichee() {
        let mut b = BTreeMap::new();
        b.insert("palette".to_string(), vec!["ctrl+k".to_string()]);
        let mut a = ready_app();
        a.keymap = Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap();
        assert!(draw(&a, 80, 24).0[23].contains("Ctrl+K Palette"));
    }

    #[test]
    fn petites_tailles_sans_panique() {
        let a = ready_app();
        for (w, h) in [(1, 1), (10, 3), (29, 24), (80, 7), (30, 8), (31, 9), (200, 60)] {
            let (rows, _) = draw(&a, w, h);
            if w < MIN_W || h < MIN_H {
                assert!("alpheus : fenêtre trop petite".starts_with(rows[0].trim_end()), "{:?}", rows[0]);
            }
        }
        let (rows, _) = draw(&a, 10, 3);
        assert_eq!(rows[0], "alpheus : ");
    }

    #[test]
    fn defilement_garde_le_focus_visible() {
        let mut a = ready_app();
        if let Loadable::Ready(st) = &mut a.data.audio {
            st.sinks = (0..30).map(|i| node(i, &format!("Sortie {i}"), 50, i == 0)).collect();
        }
        a.go_tab(Tab::Son, Instant::now());
        a.focus = 29;
        let (rows, hits) = draw(&a, 80, 12);
        let all = rows.join("\n");
        assert!(all.contains("Sortie 29"), "{all}");
        assert!(!all.contains("Sortie 0 "), "le haut de la liste a défilé");
        assert!(hits.find(Target::Slider { item: 29 }).is_some());
        assert_eq!(a.items()[29], crate::app::Item::Slider(SliderId::Node(29)));
    }

    #[test]
    fn instantanes() {
        let mut a = ready_app();
        for t in Tab::ALL {
            a.go_tab(t, Instant::now());
            insta::assert_snapshot!(format!("onglet_{}", t.index() + 1), draw(&a, 80, 24).0.join("\n"));
        }
    }
}
