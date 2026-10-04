//! niri, analyse pure (aucune entrée-sortie ici, voir sources::niri) : le
//! flux d'événements de `$NIRI_SOCKET` après la requête `"EventStream"`, une
//! ligne JSON par événement, au format de niri-ipc 26.04. Le flux commence
//! par l'état complet (WorkspacesChanged, WindowsChanged) : rien d'autre à
//! lire. Les variantes inconnues sont ignorées : niri en ajoute au fil des
//! versions, et ses types ne sont pas stables d'une version à l'autre (d'où
//! une analyse maison plutôt que le crate niri-ipc).

use super::{Bureau, EtatCompositeur, Fenetre};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
struct BureauN {
    id: u64,
    idx: u8,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    is_urgent: bool,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    is_focused: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct FenetreN {
    id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<u64>,
    #[serde(default)]
    is_focused: bool,
    #[serde(default)]
    is_urgent: bool,
}

#[derive(Debug, Default)]
pub struct Modele {
    bureaux: BTreeMap<u64, BureauN>,
    fenetres: BTreeMap<u64, FenetreN>,
}

fn champ_u64(corps: &Value, cle: &str) -> Result<u64, String> {
    corps.get(cle).and_then(Value::as_u64).ok_or_else(|| format!("champ « {cle} » absent"))
}

fn champ_bool(corps: &Value, cle: &str) -> Result<bool, String> {
    corps.get(cle).and_then(Value::as_bool).ok_or_else(|| format!("champ « {cle} » absent"))
}

fn lire<T: serde::de::DeserializeOwned>(nom: &str, v: Option<&Value>) -> Result<T, String> {
    serde_json::from_value(v.cloned().unwrap_or(Value::Null)).map_err(|e| format!("{nom} illisible : {e}"))
}

impl Modele {
    pub fn appliquer_ligne(&mut self, ligne: &str) -> Result<bool, String> {
        let v: Value = serde_json::from_str(ligne).map_err(|e| format!("ligne niri illisible : {e}"))?;
        let Some((nom, corps)) = v.as_object().and_then(|o| o.iter().next()) else { return Ok(false) };
        match nom.as_str() {
            "Ok" => Ok(false),
            "Err" => Err(format!("niri refuse : {corps}")),
            "WorkspacesChanged" => {
                let l: Vec<BureauN> = lire(nom, corps.get("workspaces"))?;
                self.bureaux = l.into_iter().map(|b| (b.id, b)).collect();
                Ok(true)
            }
            "WorkspaceUrgencyChanged" => {
                let (id, urgent) = (champ_u64(corps, "id")?, champ_bool(corps, "urgent")?);
                if let Some(b) = self.bureaux.get_mut(&id) {
                    b.is_urgent = urgent;
                }
                Ok(true)
            }
            "WorkspaceActivated" => {
                let (id, focalise) = (champ_u64(corps, "id")?, champ_bool(corps, "focused")?);
                let sortie = self.bureaux.get(&id).and_then(|b| b.output.clone());
                for b in self.bureaux.values_mut() {
                    if b.output == sortie {
                        b.is_active = b.id == id;
                    }
                    if focalise {
                        b.is_focused = b.id == id;
                    }
                }
                Ok(true)
            }
            "WindowsChanged" => {
                let l: Vec<FenetreN> = lire(nom, corps.get("windows"))?;
                self.fenetres = l.into_iter().map(|f| (f.id, f)).collect();
                Ok(true)
            }
            "WindowOpenedOrChanged" => {
                let f: FenetreN = lire(nom, corps.get("window"))?;
                if f.is_focused {
                    for g in self.fenetres.values_mut() {
                        g.is_focused = false;
                    }
                }
                self.fenetres.insert(f.id, f);
                Ok(true)
            }
            "WindowClosed" => {
                self.fenetres.remove(&champ_u64(corps, "id")?);
                Ok(true)
            }
            "WindowFocusChanged" => {
                let id = corps.get("id").and_then(Value::as_u64); // null : plus de focus
                for f in self.fenetres.values_mut() {
                    f.is_focused = Some(f.id) == id;
                }
                Ok(true)
            }
            "WindowUrgencyChanged" => {
                let (id, urgent) = (champ_u64(corps, "id")?, champ_bool(corps, "urgent")?);
                if let Some(f) = self.fenetres.get_mut(&id) {
                    f.is_urgent = urgent;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub fn etat(&self) -> EtatCompositeur {
        // Un bureau sans sortie (écran débranché, niri le garde) n'est pas affiché.
        let mut bs: Vec<&BureauN> = self.bureaux.values().filter(|b| b.output.is_some()).collect();
        // Sorties triées sans tenir compte de la casse (« eDP-1 » avant « HDMI-A-1 »).
        bs.sort_by_key(|b| (b.output.as_deref().map(str::to_lowercase), b.idx));
        let bureaux = bs
            .into_iter()
            .map(|b| {
                let siennes = || self.fenetres.values().filter(move |f| f.workspace_id == Some(b.id));
                Bureau {
                    nom: b.name.clone().unwrap_or_else(|| b.idx.to_string()),
                    reference: b.idx.to_string(),
                    sortie: b.output.clone().unwrap_or_default(),
                    fenetres: siennes().count().min(u16::MAX as usize) as u16,
                    urgent: b.is_urgent || siennes().any(|f| f.is_urgent),
                    visible: b.is_active,
                    focalise: b.is_focused,
                }
            })
            .collect();
        let fenetre = self
            .fenetres
            .values()
            .find(|f| f.is_focused)
            .map(|f| Fenetre { classe: f.app_id.clone().unwrap_or_default(), titre: f.title.clone().unwrap_or_default() });
        EtatCompositeur { bureaux, fenetre }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLUX: &str = include_str!("../../tests/fixtures/niri-evenements.jsonl");

    fn apres(n: usize) -> EtatCompositeur {
        let mut m = Modele::default();
        for l in FLUX.lines().take(n) {
            m.appliquer_ligne(l).unwrap();
        }
        m.etat()
    }

    /// (nom, sortie, fenêtres, urgent, visible, focalisé) de chaque bureau.
    fn resume(e: &EtatCompositeur) -> Vec<(String, String, u16, bool, bool, bool)> {
        e.bureaux.iter().map(|b| (b.nom.clone(), b.sortie.clone(), b.fenetres, b.urgent, b.visible, b.focalise)).collect()
    }

    fn b(nom: &str, sortie: &str, f: u16, u: bool, v: bool, fo: bool) -> (String, String, u16, bool, bool, bool) {
        (nom.into(), sortie.into(), f, u, v, fo)
    }

    #[test]
    fn etat_initial_du_flux() {
        let e = apres(6);
        assert_eq!(
            resume(&e),
            [
                b("1", "eDP-1", 1, false, true, true),
                b("2", "eDP-1", 0, false, false, false),
                b("web", "HDMI-A-1", 2, false, true, false),
                b("2", "HDMI-A-1", 0, false, false, false),
            ]
        );
        assert_eq!(e.bureaux.iter().map(|b| b.reference.as_str()).collect::<Vec<_>>(), ["1", "2", "1", "2"]);
        assert_eq!(e.fenetre, Some(Fenetre { classe: "Alacritty".into(), titre: "~/projets — zsh".into() }));
    }

    #[test]
    fn urgence_focus_ouverture_fermeture() {
        assert!(apres(7).bureaux[2].urgent, "fenêtre 12 urgente → bureau web urgent");
        let e = apres(8);
        assert!(e.bureaux[2].focalise && e.bureaux[0].visible && !e.bureaux[0].focalise);
        assert_eq!(apres(9).fenetre.unwrap().titre, "Page web — Mozilla Firefox");
        assert!(!apres(10).bureaux[2].urgent);
        let e = apres(11);
        assert_eq!(e.fenetre.unwrap().classe, "com.mitchellh.ghostty");
        assert_eq!(e.bureaux[3].fenetres, 1);
        let e = apres(12);
        assert!(e.bureaux[3].visible && e.bureaux[3].focalise);
        assert!(!e.bureaux[2].visible, "un seul bureau actif par sortie");
        assert!(e.bureaux[0].visible, "l'autre sortie garde le sien");
        assert_eq!(apres(13).bureaux[2].fenetres, 1);
    }

    #[test]
    fn lignes_sans_effet_ou_refusees() {
        let mut m = Modele::default();
        assert_eq!(m.appliquer_ligne(r#"{"Ok":"Handled"}"#), Ok(false));
        assert_eq!(m.appliquer_ligne(r#"{"NouvelleVariante":{"x":1}}"#), Ok(false));
        assert_eq!(m.appliquer_ligne(FLUX.lines().nth(13).unwrap()), Ok(false), "WindowLayoutsChanged ignoré");
        assert!(m.appliquer_ligne(r#"{"Err":"pas de flux"}"#).unwrap_err().contains("niri refuse"));
        assert!(m.appliquer_ligne("pas du json").unwrap_err().contains("illisible"));
    }
}
