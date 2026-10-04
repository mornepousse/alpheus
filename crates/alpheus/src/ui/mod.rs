//! Rendu ratatui. `render` dessine tout et renvoie la carte des zones
//! cliquables : chaque composant connaît sa zone pour le test de clic.

pub mod draw;
mod ecrans;
pub mod hit;
mod overlays;
mod tabs;

pub use hit::{HitMap, Target};

use crate::app::App;
use crate::app::action::{Action, Tab};
use draw::put;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

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
    overlays::render(buf, area, app, &mut hits);
    hits
}

fn tab_bar(buf: &mut Buffer, r: Rect, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let mut x = r.x + 1;
    x += put(buf, r, x, r.y, "alpheus", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)) + 2;
    for (i, t) in Tab::ALL.iter().enumerate() {
        let label = format!(" {} {} ", i + 1, t.title());
        let style =
            if *t == app.tab { Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.fg) };
        let w = put(buf, r, x, r.y, &label, style);
        hits.push(Rect::new(x, r.y, w, 1), Target::Tab(*t));
        // Collés : chaque libellé porte déjà ses espaces, et les six
        // onglets tiennent ainsi en 80 colonnes.
        x = x.saturating_add(w);
    }
}

fn status_line(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let style = Style::default().fg(th.fg).bg(th.surface);
    draw::clear(buf, r, style);
    let last = app.status.last.as_deref().unwrap_or("prêt");
    let mut x = r.x + 1;
    x += put(buf, r, x, r.y, &format!("{} · {last}", app.tab.title()), style);
    // Dernière erreur d'action, puis avertissement de démarrage (persistant).
    for msg in [&app.status.error, &app.status.warning].into_iter().flatten() {
        x = x.saturating_add(put(buf, r, x, r.y, &format!(" · ✗ {msg}"), Style::default().fg(th.error).bg(th.surface)));
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
            Tab::Ecrans => match app.focused() {
                Some(crate::app::Item::DisplayProfiles) => {
                    pairs.push((format!("{}/{}", km.label(Action::Left), km.label(Action::Right)), "Choisir"));
                    pairs.push((km.label(Action::Activate), "Appliquer"));
                }
                _ => {
                    pairs.push((km.label(Action::Toggle), "Basculer"));
                    pairs.push((km.label(Action::Activate), "Régler"));
                }
            },
        }
    }
    let mut x = r.x + 1;
    for (key, what) in pairs {
        if key.is_empty() {
            continue; // action sans touche (désaffectée dans config.toml)
        }
        // Largeur de la paire « clé étiquette », sans le rembourrage final :
        // si elle ne tient pas entière avant le bord droit, on s'arrête plutôt
        // que de couper un libellé au milieu d'un mot.
        let w = key.chars().count() as u16 + 1 + what.chars().count() as u16;
        if x.saturating_add(w) > r.right() {
            break;
        }
        x = x.saturating_add(put(buf, r, x, r.y, &key, Style::default().fg(th.accent).add_modifier(Modifier::BOLD)));
        x = x.saturating_add(put(buf, r, x, r.y, &format!(" {what}  "), Style::default().fg(th.fg)));
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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
    use crate::app::keymap::Keymap;
    use crate::app::testing::{app, node, ready_app};
    use crate::app::worker::{Op, Update};
    use crate::app::{Confirm, Loadable, Overlay, Palette, SliderId};
    use crate::backends::PowerAction;
    use crate::config::KeysConfig;
    use std::collections::BTreeMap;
    use std::time::Instant;

    #[test]
    fn cadre_de_l_accueil() {
        let (rows, _) = draw(&ready_app(), 80, 24);
        assert!(rows[0].contains("alpheus") && rows[0].contains(" 1 Accueil ") && rows[0].contains(" 5 Alimentation "));
        assert!(rows[0].contains(" 6 Écrans "), "six onglets en 80 colonnes : {:?}", rows[0]);
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
        // Une action réussie n'efface pas l'avertissement de démarrage.
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        assert!(draw(&a, 80, 24).0[22].contains("✗ config.toml invalide : x"), "{:?}", draw(&a, 80, 24).0[22]);
        a.status.error = Some("Wi-Fi : boum".into());
        let line = &draw(&a, 100, 24).0[22];
        assert!(line.contains("✗ Wi-Fi : boum") && line.contains("✗ config.toml invalide : x"), "{line:?}");
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
    fn barre_de_raccourcis_sans_coupure() {
        let mut a = ready_app();
        a.go_tab(Tab::Reseau, Instant::now());
        let (rows, _) = draw(&a, 80, 24);
        let bar = &rows[23];
        // « Ouvrir » ne doit jamais apparaître tronqué en « Ouvri ».
        assert!(!bar.contains("Entrée Ouvri") || bar.contains("Entrée Ouvrir"), "{bar:?}");
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

    // ── Onglet Écrans ────────────────────────────────────────────────

    use crate::app::SaveDialog;

    fn ecrans() -> crate::app::App {
        let mut a = ready_app();
        a.go_tab(Tab::Ecrans, Instant::now());
        a.take_requests();
        a
    }

    #[test]
    fn onglet_ecrans() {
        let mut a = ecrans();
        a.focus = 2; // Alpha
        let (rows, hits) = draw(&a, 100, 28);
        let all = rows.join("\n");
        for attendu in [
            "Profils",
            "[ Laptop seul ]",
            "[ Bureau ]",
            "[ + Enregistrer l'actuel ]",
            "Carte",
            "Écrans",
            "1 Portable",
            "2 Alpha",
            "DVI-I-1 ⚠",
            "3 Bravo",
            "1920×1080@240",
            "échelle 1.0",
            "position 0, 1080",
            "origine",
            "à droite de Alpha",
        ] {
            assert!(all.contains(attendu), "{attendu} absent :\n{all}");
        }
        assert!(all.contains('●'), "la carte marque l'écran focalisé :\n{all}");
        assert!(hits.find(Target::DisplayProfile(2)).is_some());
        assert!(hits.find(Target::OutputToggle(3)).is_some());
        assert!(rows[27].contains("Espace Basculer") && rows[27].contains("Entrée Régler"), "{:?}", rows[27]);
        a.focus = 0;
        assert!(draw(&a, 100, 28).0[27].contains("Entrée Appliquer"));
    }

    #[test]
    fn dernier_ecran_grise() {
        let mut a = ecrans();
        a.data.displays = Loadable::Ready(vec![crate::displays::testing::poste()[0].clone()]);
        assert!(draw(&a, 100, 28).0.join("\n").contains("[■] seul actif"));
    }

    #[test]
    fn ecrans_sous_niri_en_echec_ou_en_attente() {
        let mut a = ecrans();
        a.ecrans.niri = true;
        assert!(draw(&a, 80, 24).0.join("\n").contains("Écrans : pas encore disponible sous niri"));
        let mut a = ecrans();
        a.data.displays = Loadable::Failed("wm-set a échoué (code 1) : sous-commande inconnue : outputs".into());
        let all = draw(&a, 100, 24).0.join("\n");
        assert!(all.contains("✗ wm-set a échoué (code 1) : sous-commande inconnue : outputs"), "{all}");
        assert!(all.contains("[ Bureau ]"), "les profils restent visibles");
        a.data.displays = Loadable::Loading;
        assert!(draw(&a, 80, 24).0.join("\n").contains('…'));
    }

    #[test]
    fn carte_a_l_echelle() {
        use super::ecrans::{MAP_ROWS, map_canvas};
        use crate::displays::testing::{ALPHA, BRAVO, on, poste};
        let text = |g: &Vec<Vec<Option<(char, bool)>>>| -> Vec<String> {
            g.iter().map(|r| r.iter().map(|c| c.map_or(' ', |(ch, _)| ch)).collect()).collect()
        };
        let g = map_canvas(&poste(), Some("HDMI-A-1"), 76);
        let lines = text(&g);
        assert!(lines.len() <= usize::from(MAP_ROWS));
        let all = lines.join("\n");
        assert!(all.contains("Alpha") && all.contains("Bravo") && all.contains("Portable"), "{all}");
        for n in ["1", "2", "3"] {
            assert!(all.contains(n), "numéro {n} :\n{all}");
        }
        assert!(g.iter().flatten().flatten().any(|(ch, foc)| *ch == '●' && *foc), "● sur Bravo");
        let g = map_canvas(&[on("DP-1", ALPHA, 1920, 1080, 0, 0), on("DP-2", BRAVO, 1920, 1080, 1920, 0)], None, 20);
        assert!(!g.is_empty() && g.iter().all(|r| r.len() <= 20), "bornée par la largeur");
        assert!(map_canvas(&[], None, 76).is_empty());
        assert!(map_canvas(&poste(), None, 2).is_empty(), "trop étroit : rien");
    }

    #[test]
    fn boite_garder_ces_reglages() {
        let mut a = ecrans();
        a.toggle_output("DVI-I-1", Instant::now());
        let all = draw(&a, 80, 24).0.join("\n");
        assert!(all.contains("Garder ces réglages ?") && all.contains("Alpha désactivé") && all.contains("Alpha : désactivé"), "{all}");
        assert!(all.contains("⚠ Sortie DisplayLink : risque de gel connu sur ce poste"), "{all}");
        assert!(all.contains("Application en cours…") && all.contains("[ Annuler ]") && all.contains("[ Garder ]"), "{all}");
        let r = a.take_requests();
        let Some(crate::app::worker::Request::Do(op)) = r.into_iter().next() else { panic!() };
        a.on_update(Update::Done(op, Ok(())), Instant::now());
        assert!(draw(&a, 80, 24).0.join("\n").contains("Retour automatique dans 15 s"));
        let (_, hits) = draw(&a, 80, 24);
        let (no, yes) = (hits.find(Target::ConfirmNo).unwrap(), hits.find(Target::ConfirmYes).unwrap());
        assert!(no.x < yes.x && no.y == yes.y, "Annuler à gauche");
    }

    #[test]
    fn boite_garder_en_petite_hauteur_garde_le_compte_a_rebours() {
        let mut a = ecrans();
        a.apply_display_profile("Bureau", Instant::now());
        let r = a.take_requests();
        let Some(crate::app::worker::Request::Do(op)) = r.into_iter().next() else { panic!() };
        a.on_update(Update::Done(op, Ok(())), Instant::now());
        for h in [MIN_H, MIN_H + 1, 10] {
            let all = draw(&a, 80, h).0.join("\n");
            assert!(all.contains("Retour automatique dans 15 s"), "h={h}\n{all}");
            assert!(all.contains("⚠ Sortie DisplayLink"), "h={h}\n{all}");
            assert!(all.contains("[ Annuler ]"), "h={h}\n{all}");
        }
    }

    #[test]
    fn panneau_de_reglage() {
        let mut a = ecrans();
        a.open_panel("HDMI-A-1", Instant::now());
        let (rows, hits) = draw(&a, 80, 24);
        let all = rows.join("\n");
        for attendu in [
            "Réglage — Bravo (HDMI-A-1)",
            "Mode",
            "1920×1080@240  2/2",
            "Échelle",
            "Rotation",
            "0°",
            "Placement",
            "à droite de",
            "Référence",
            "Alpha",
            "Alignement",
            "en haut",
            "✗ aucun changement",
            "[ Annuler ]",
            "[ Appliquer ]",
        ] {
            assert!(all.contains(attendu), "{attendu} absent :\n{all}");
        }
        assert!(hits.find(Target::PanelStep { field: crate::app::PanelField::Align, dir: -1 }).is_some());
        // Origine : plus de référence ni d'alignement.
        if let Overlay::Panel(p) = &mut a.overlay {
            p.place = crate::app::PlaceChoice::Origin;
        }
        let all = draw(&a, 80, 24).0.join("\n");
        assert!(!all.contains("Référence") && all.contains("origine"), "{all}");
    }

    #[test]
    fn saisie_du_nom_de_profil() {
        let mut a = ecrans();
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Maison".into(), ..SaveDialog::default() });
        let all = draw(&a, 80, 24).0.join("\n");
        assert!(all.contains("Enregistrer l'actuel") && all.contains("> Maison▏"), "{all}");
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), replace: true, yes_focused: false });
        let all = draw(&a, 80, 24).0.join("\n");
        assert!(all.contains("Remplacer le profil « Bureau » ?") && all.contains("[ Remplacer ]"), "{all}");
    }

    #[test]
    fn ecrans_sans_panique_en_petite_fenetre() {
        let mut a = ecrans();
        a.open_panel("eDP-1", Instant::now());
        let panneau = a.overlay.clone();
        a.overlay = Overlay::None;
        a.toggle_output("DVI-I-1", Instant::now());
        let garder = a.overlay.clone();
        for o in [Overlay::None, panneau, garder, Overlay::SaveProfile(SaveDialog::default())] {
            a.overlay = o;
            for (w, h) in [(30, 8), (31, 9), (40, 10), (50, 12), (200, 60)] {
                draw(&a, w, h);
            }
        }
    }

    #[test]
    fn instantanes_ecrans() {
        let mut a = ecrans();
        a.focus = 2;
        insta::assert_snapshot!("ecrans_liste", draw(&a, 100, 28).0.join("\n"));
        a.open_panel("HDMI-A-1", Instant::now());
        insta::assert_snapshot!("ecrans_panneau", draw(&a, 80, 24).0.join("\n"));
        a.overlay = Overlay::None;
        a.apply_display_profile("Bureau", Instant::now());
        insta::assert_snapshot!("ecrans_garder", draw(&a, 80, 24).0.join("\n"));
    }

    #[test]
    fn instantanes() {
        let mut a = ready_app();
        for t in Tab::ALL {
            a.go_tab(t, Instant::now());
            insta::assert_snapshot!(format!("onglet_{}", t.index() + 1), draw(&a, 80, 24).0.join("\n"));
        }
    }

    #[test]
    fn aide() {
        let mut a = ready_app();
        a.overlay = Overlay::Help;
        let (rows, hits) = draw(&a, 80, 30);
        let all = rows.join("\n");
        assert!(all.contains("Palette de commandes") && all.contains("Ctrl+P"));
        assert!(all.contains("Aide — Échap pour fermer"));
        assert_eq!(hits.at(40, 15).map(|h| h.1), Some(Target::Overlay));
    }

    #[test]
    fn aide_entiere_en_80x26() {
        let mut a = ready_app();
        a.overlay = Overlay::Help;
        let (rows, _) = draw(&a, 80, 26);
        let all = rows.join("\n");
        assert!(all.contains("Aide — Échap pour fermer"), "{all}");
        assert!(all.contains("Souris") && all.contains("clic, glisser, molette"), "dernière ligne coupée :\n{all}");
        assert!(all.contains("Onglets 1 à 6") && all.contains("1, 2, 3, 4, 5, 6"), "{all}");
        assert!(!all.contains("Onglet Son"), "les six onglets tiennent sur une ligne");
    }

    #[test]
    fn palette() {
        let mut a = ready_app();
        a.overlay = Overlay::Palette(Palette { query: "wifi".into(), selected: 0 });
        let (rows, hits) = draw(&a, 80, 24);
        let all = rows.join("\n");
        assert!(all.contains("> wifi▏") && all.contains("Wi-Fi"));
        assert!(hits.find(Target::PaletteEntry(0)).is_some());
        a.overlay = Overlay::Palette(Palette { query: "zzz".into(), selected: 0 });
        assert!(draw(&a, 80, 24).0.join("\n").contains("aucune commande"));
    }

    #[test]
    fn palette_defile_jusqu_a_la_selection() {
        let mut a = ready_app();
        let n = a.palette_matches().len();
        a.overlay = Overlay::Palette(Palette { query: String::new(), selected: n - 1 });
        let (rows, hits) = draw(&a, 80, 24);
        assert!(rows.join("\n").contains("Éteindre"), "dernière entrée visible");
        assert!(hits.find(Target::PaletteEntry(n - 1)).is_some());
        assert!(hits.find(Target::PaletteEntry(0)).is_none());
    }

    #[test]
    fn confirmation() {
        let mut a = ready_app();
        a.overlay = Overlay::Confirm(Confirm { action: PowerAction::Poweroff, yes_focused: false });
        let (rows, hits) = draw(&a, 80, 24);
        let all = rows.join("\n");
        assert!(all.contains("Éteindre l'ordinateur ?") && all.contains("[ Annuler ]") && all.contains("[ Confirmer ]"));
        let no = hits.find(Target::ConfirmNo).unwrap();
        let yes = hits.find(Target::ConfirmYes).unwrap();
        assert!(no.x < yes.x && no.y == yes.y);
    }

    #[test]
    fn messages_en_coin() {
        let mut a = ready_app();
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        let rows = draw(&a, 80, 24).0;
        assert!(rows[1].trim_end().ends_with("✓ Wi-Fi : désactivé"), "{:?}", rows[1]);
    }

    #[test]
    fn appliquer_grise_si_refuse() {
        let mut a = ready_app();
        a.go_tab(Tab::Ecrans, Instant::now());
        a.open_panel("HDMI-A-1", Instant::now());
        // Couleur du texte de « [ Appliquer ] » après rendu.
        let fg = |a: &crate::app::App| {
            let r = draw(a, 80, 24).1.find(Target::PanelApply).unwrap();
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            t.draw(|f| {
                render(f, a);
            })
            .unwrap();
            t.backend().buffer()[(r.x + 2, r.y)].fg
        };
        assert_eq!(fg(&a), a.theme.muted, "aucun changement : grisé");
        if let Overlay::Panel(p) = &mut a.overlay {
            p.scale = crate::displays::Scale(1250);
        }
        assert_ne!(fg(&a), a.theme.muted, "réglage valide : actif");
    }

    #[test]
    fn surcouches_sans_panique_en_petite_fenetre() {
        let mut a = ready_app();
        for o in [
            Overlay::Help,
            Overlay::Palette(Palette::default()),
            Overlay::Confirm(Confirm { action: PowerAction::Reboot, yes_focused: true }),
        ] {
            a.overlay = o;
            for (w, h) in [(30, 8), (31, 9), (40, 10)] {
                draw(&a, w, h);
            }
        }
    }
}
