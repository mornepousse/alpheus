//! Hyprland, analyse pure (aucune entrée-sortie ici, voir sources::hypr) :
//! lignes du flux d'événements (`.socket2.sock`, « nom>>données ») et
//! réponses JSON des quatre requêtes de lecture. Le flux ne redonne jamais
//! l'état initial ni l'urgence passée : l'état vient des réponses JSON,
//! l'urgence des seuls événements `urgent>>`.

use super::{Bureau, EtatCompositeur, Fenetre};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evenement {
    /// Bureaux, fenêtres ou écrans ont changé : relire l'état.
    Structure,
    /// `activewindow>>classe,titre` (None : plus de fenêtre active).
    FenetreActive(Option<Fenetre>),
    /// `activewindowv2>>adresse`.
    Focus(Option<String>),
    /// `windowtitlev2>>adresse,titre`.
    Titre { adresse: String, titre: String },
    Urgent(String),
    /// `closewindow>>adresse` (structure aussi).
    Fermee(String),
    Ignore,
}

/// Adresse normalisée : sans « 0x », en minuscules (le flux écrit
/// `583d9c60da90`, le JSON `0x583d9c60da90`).
pub fn adresse(s: &str) -> String {
    s.trim().trim_start_matches("0x").to_ascii_lowercase()
}

/// Événements après lesquels l'état se relit (bureaux, fenêtres, écrans).
const STRUCTURE: [&str; 20] = [
    "workspace", "workspacev2", "focusedmon", "focusedmonv2", "createworkspace", "createworkspacev2",
    "destroyworkspace", "destroyworkspacev2", "moveworkspace", "moveworkspacev2", "renameworkspace",
    "openwindow", "movewindow", "movewindowv2", "monitoradded", "monitoraddedv2", "monitorremoved",
    "monitorremovedv2", "activespecial", "activespecialv2",
];

pub fn analyser_ligne(ligne: &str) -> Evenement {
    let Some((nom, donnees)) = ligne.split_once(">>") else { return Evenement::Ignore };
    match nom {
        "activewindow" => {
            // La classe n'a pas de virgule ; le titre peut en avoir.
            let (classe, titre) = donnees.split_once(',').unwrap_or((donnees, ""));
            if classe.is_empty() && titre.is_empty() {
                Evenement::FenetreActive(None)
            } else {
                Evenement::FenetreActive(Some(Fenetre { classe: classe.into(), titre: titre.into() }))
            }
        }
        "activewindowv2" => {
            let a = adresse(donnees);
            Evenement::Focus((!a.is_empty()).then_some(a))
        }
        "windowtitlev2" => match donnees.split_once(',') {
            Some((a, t)) => Evenement::Titre { adresse: adresse(a), titre: t.into() },
            None => Evenement::Ignore,
        },
        "urgent" => Evenement::Urgent(adresse(donnees)),
        "closewindow" => Evenement::Fermee(adresse(donnees)),
        n if STRUCTURE.contains(&n) => Evenement::Structure,
        _ => Evenement::Ignore,
    }
}

#[derive(Deserialize)]
struct RefBureau {
    id: i64,
}

#[derive(Deserialize)]
struct Moniteur {
    #[serde(default)]
    focused: bool,
    #[serde(rename = "activeWorkspace")]
    actif: RefBureau,
}

#[derive(Deserialize)]
struct BureauH {
    id: i64,
    name: String,
    #[serde(default)]
    monitor: String,
    #[serde(default)]
    windows: u16,
}

#[derive(Deserialize)]
struct Client {
    address: String,
    workspace: RefBureau,
}

#[derive(Deserialize, Default)]
struct Active {
    #[serde(default)]
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
}

/// Réponses brutes aux quatre requêtes de lecture.
pub struct Reponses<'a> {
    pub moniteurs: &'a str,
    pub bureaux: &'a str,
    pub clients: &'a str,
    pub active: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Instantane {
    /// Bureaux triés, spéciaux exclus, sans urgence (le JSON n'en dit rien).
    bureaux: Vec<Bureau>,
    /// Id Hyprland de chaque bureau, dans le même ordre.
    ids: Vec<i64>,
    /// Adresse de fenêtre → id de son bureau.
    fenetres: BTreeMap<String, i64>,
    active: Option<(String, Fenetre)>,
}

impl Instantane {
    pub fn analyser(r: &Reponses) -> Result<Instantane, String> {
        fn lire<T: serde::de::DeserializeOwned>(quoi: &str, texte: &str) -> Result<T, String> {
            serde_json::from_str(texte).map_err(|e| format!("réponse {quoi} illisible : {e}"))
        }
        let moniteurs: Vec<Moniteur> = lire("j/monitors", r.moniteurs)?;
        let mut bureaux: Vec<BureauH> = lire("j/workspaces", r.bureaux)?;
        let clients: Vec<Client> = lire("j/clients", r.clients)?;
        let active: Active = lire("j/activewindow", r.active)?;
        // Bureaux spéciaux (scratchpads) : jamais dans la barre.
        bureaux.retain(|b| !b.name.starts_with("special:"));
        // Numérotés d'abord, dans l'ordre ; les nommés (id négatif) ensuite.
        bureaux.sort_by_key(|b| (b.id <= 0, b.id.unsigned_abs()));
        let mut out = Instantane::default();
        for b in bureaux {
            let m = moniteurs.iter().find(|m| m.actif.id == b.id);
            let reference = if b.id > 0 { b.id.to_string() } else { format!("name:{}", b.name) };
            out.ids.push(b.id);
            out.bureaux.push(Bureau {
                reference,
                nom: b.name,
                sortie: b.monitor,
                fenetres: b.windows,
                urgent: false,
                visible: m.is_some(),
                focalise: m.is_some_and(|m| m.focused),
            });
        }
        out.fenetres = clients.into_iter().map(|c| (adresse(&c.address), c.workspace.id)).collect();
        if !active.address.is_empty() || !active.class.is_empty() {
            out.active = Some((adresse(&active.address), Fenetre { classe: active.class, titre: active.title }));
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effet {
    Rien,
    Changement,
    Relire,
}

#[derive(Debug, Default)]
pub struct Modele {
    inst: Instantane,
    urgents: BTreeSet<String>,
    active: Option<String>,
    fenetre: Option<Fenetre>,
}

impl Modele {
    /// Nouvel état lu : remplace l'ancien. L'urgence ne vient que du flux,
    /// on la garde pour les fenêtres qui existent encore.
    pub fn instantane(&mut self, i: Instantane) {
        self.urgents.retain(|a| i.fenetres.contains_key(a));
        match &i.active {
            Some((a, f)) => {
                self.active = Some(a.clone());
                self.fenetre = Some(f.clone());
            }
            None => {
                self.active = None;
                self.fenetre = None;
            }
        }
        self.inst = i;
    }

    pub fn appliquer(&mut self, e: &Evenement) -> Effet {
        match e {
            Evenement::Structure => Effet::Relire,
            Evenement::Fermee(a) => {
                self.urgents.remove(a);
                Effet::Relire
            }
            Evenement::FenetreActive(f) => {
                if &self.fenetre == f {
                    Effet::Rien
                } else {
                    self.fenetre = f.clone();
                    Effet::Changement
                }
            }
            Evenement::Focus(a) => {
                self.active = a.clone();
                match a {
                    Some(a) if self.urgents.remove(a) => Effet::Changement,
                    _ => Effet::Rien, // le titre arrive par `activewindow`
                }
            }
            Evenement::Titre { adresse, titre } => match (&self.active, &mut self.fenetre) {
                (Some(a), Some(f)) if a == adresse && &f.titre != titre => {
                    f.titre = titre.clone();
                    Effet::Changement
                }
                _ => Effet::Rien,
            },
            Evenement::Urgent(a) => {
                if self.active.as_ref() != Some(a) && self.urgents.insert(a.clone()) {
                    Effet::Changement
                } else {
                    Effet::Rien
                }
            }
            Evenement::Ignore => Effet::Rien,
        }
    }

    pub fn etat(&self) -> EtatCompositeur {
        let urgents: BTreeSet<i64> = self.urgents.iter().filter_map(|a| self.inst.fenetres.get(a)).copied().collect();
        let bureaux = self
            .inst
            .bureaux
            .iter()
            .zip(&self.inst.ids)
            .map(|(b, id)| Bureau { urgent: urgents.contains(id), ..b.clone() })
            .collect();
        EtatCompositeur { bureaux, fenetre: self.fenetre.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITEURS: &str = include_str!("../../tests/fixtures/hypr-monitors.json");
    const BUREAUX: &str = include_str!("../../tests/fixtures/hypr-workspaces.json");
    const CLIENTS: &str = include_str!("../../tests/fixtures/hypr-clients.json");
    const ACTIVE: &str = include_str!("../../tests/fixtures/hypr-activewindow.json");
    const EVENEMENTS: &str = include_str!("../../tests/fixtures/hypr-evenements.txt");

    fn modele_du_poste() -> Modele {
        let i = Instantane::analyser(&Reponses { moniteurs: MONITEURS, bureaux: BUREAUX, clients: CLIENTS, active: ACTIVE }).unwrap();
        let mut m = Modele::default();
        m.instantane(i);
        m
    }

    fn bureau<'a>(e: &'a EtatCompositeur, nom: &str) -> &'a Bureau {
        e.bureaux.iter().find(|b| b.nom == nom).unwrap_or_else(|| panic!("bureau {nom} absent : {e:?}"))
    }

    #[test]
    fn instantane_du_poste() {
        let e = modele_du_poste().etat();
        assert_eq!(e.bureaux.iter().map(|b| b.nom.as_str()).collect::<Vec<_>>(), ["1", "2", "3"]);
        let b1 = bureau(&e, "1");
        assert_eq!((b1.sortie.as_str(), b1.fenetres, b1.visible, b1.focalise), ("eDP-1", 0, true, false));
        let b2 = bureau(&e, "2");
        assert_eq!((b2.sortie.as_str(), b2.fenetres, b2.visible, b2.focalise), ("HDMI-A-1", 1, true, true));
        let b3 = bureau(&e, "3");
        assert_eq!((b3.sortie.as_str(), b3.fenetres, b3.visible, b3.focalise, b3.reference.as_str()), ("DVI-I-1", 2, true, false, "3"));
        assert!(e.bureaux.iter().all(|b| !b.urgent));
        assert_eq!(e.fenetre, Some(Fenetre { classe: "firefox".into(), titre: "Page web — Mozilla Firefox".into() }));
    }

    #[test]
    fn lignes_du_flux() {
        let f = |c: &str, t: &str| Evenement::FenetreActive(Some(Fenetre { classe: c.into(), titre: t.into() }));
        assert_eq!(analyser_ligne("activewindow>>firefox,Titre, avec, virgules"), f("firefox", "Titre, avec, virgules"));
        assert_eq!(analyser_ligne("activewindow>>,"), Evenement::FenetreActive(None));
        assert_eq!(analyser_ligne("activewindowv2>>583D9C60DA90"), Evenement::Focus(Some("583d9c60da90".into())));
        assert_eq!(analyser_ligne("activewindowv2>>"), Evenement::Focus(None));
        assert_eq!(analyser_ligne("windowtitlev2>>583d9c60da90,a,b"), Evenement::Titre { adresse: "583d9c60da90".into(), titre: "a,b".into() });
        assert_eq!(analyser_ligne("urgent>>0x583d9d5fd540"), Evenement::Urgent("583d9d5fd540".into()));
        assert_eq!(analyser_ligne("closewindow>>583d9c8a1b20"), Evenement::Fermee("583d9c8a1b20".into()));
        for s in ["openwindow>>1,2,a,b", "workspacev2>>3,3", "focusedmonv2>>DVI-I-1,3", "monitorremovedv2>>2,DVI-I-1,x", "renameworkspace>>3,web"] {
            assert_eq!(analyser_ligne(s), Evenement::Structure, "{s}");
        }
        for s in ["windowtitle>>583d9c60da90", "bell>>x", "sans séparateur", ""] {
            assert_eq!(analyser_ligne(s), Evenement::Ignore, "{s}");
        }
    }

    #[test]
    fn fixture_du_flux_appliquee() {
        let mut m = modele_du_poste();
        let mut effets = Vec::new();
        for l in EVENEMENTS.lines() {
            effets.push(m.appliquer(&analyser_ligne(l)));
            if l.starts_with("activewindowv2>>583d9c60da90") {
                assert_eq!(m.etat().fenetre.unwrap().titre, "Autre page — Mozilla Firefox");
            }
            if l.starts_with("urgent>>") {
                assert!(bureau(&m.etat(), "3").urgent, "fenêtre urgente sur le bureau 3");
            }
        }
        assert!(effets.contains(&Effet::Relire) && effets.contains(&Effet::Changement));
        let e = m.etat();
        assert_eq!(e.fenetre.as_ref().unwrap().classe, "Alacritty");
        assert!(bureau(&e, "3").urgent, "le focus d'une AUTRE fenêtre n'efface pas l'urgence");
    }

    #[test]
    fn urgence_effacee_quand_la_fenetre_prend_le_focus() {
        let mut m = modele_du_poste();
        assert_eq!(m.appliquer(&Evenement::Urgent("583d9d5fd540".into())), Effet::Changement);
        assert!(bureau(&m.etat(), "3").urgent);
        assert_eq!(m.appliquer(&Evenement::Focus(Some("583d9d5fd540".into()))), Effet::Changement);
        assert!(!bureau(&m.etat(), "3").urgent);
        assert_eq!(m.appliquer(&Evenement::Urgent("583d9d5fd540".into())), Effet::Rien, "fenêtre active : pas urgente");
    }

    #[test]
    fn bureaux_speciaux_caches_bureaux_nommes_atteignables() {
        let bureaux = r#"[{"id": -98, "name": "special:magic", "monitor": "eDP-1", "windows": 1},
                          {"id": -1337, "name": "web", "monitor": "eDP-1", "windows": 2},
                          {"id": 1, "name": "1", "monitor": "eDP-1", "windows": 0}]"#;
        let moniteurs = r#"[{"name": "eDP-1", "focused": true, "activeWorkspace": {"id": 1, "name": "1"}}]"#;
        let i = Instantane::analyser(&Reponses { moniteurs, bureaux, clients: "[]", active: "{}" }).unwrap();
        let mut m = Modele::default();
        m.instantane(i);
        let e = m.etat();
        let r: Vec<(&str, &str)> = e.bureaux.iter().map(|b| (b.nom.as_str(), b.reference.as_str())).collect();
        assert_eq!(r, [("1", "1"), ("web", "name:web")]);
        assert_eq!(e.fenetre, None);
    }

    #[test]
    fn json_illisible_erreur_nommee() {
        let r = Instantane::analyser(&Reponses { moniteurs: "pas du json", bureaux: "[]", clients: "[]", active: "{}" });
        assert!(r.unwrap_err().contains("j/monitors"));
    }
}
