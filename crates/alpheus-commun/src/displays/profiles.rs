//! Profils d'écran : ~/.config/alpheus/ecrans.toml. Les écrans y sont
//! identifiés par leur description (stable d'un branchement à l'autre, le
//! connecteur ne l'est pas). Fichier absent : aucun profil, sans message ;
//! fichier invalide : aucun profil + avertissement (comme config.toml).

use super::geometry::{deduce, logical_size, resolve, Align, Placed, Placement, Relation, Screen};
use super::{short_from_description, short_names, Active, Mode, Output, Plan, Scale};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileScreen {
    pub description: String,
    pub enabled: bool,
    /// Obligatoire pour un écran actif.
    pub mode: Option<Mode>,
    pub scale: Scale,
    /// Rotation Hyprland 0-7 (comme `wm-set monitor`).
    pub transform: u8,
    pub placement: Placement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub screens: Vec<ProfileScreen>,
}

// ── Format du fichier ────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileToml {
    #[serde(default)]
    profil: Vec<ProfilToml>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfilToml {
    nom: String,
    #[serde(default)]
    ecran: Vec<EcranToml>,
}

fn vrai() -> bool {
    true
}
fn un() -> f64 {
    1.0
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EcranToml {
    description: String,
    #[serde(default = "vrai")]
    actif: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(default = "un")]
    echelle: f64,
    #[serde(default)]
    rotation: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    place: Option<PlaceToml>,
}

/// `{ relation, reference, alignement }` ou `{ x, y }` (coordonnées absolues).
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PlaceToml {
    Rel(RelToml),
    Abs(AbsToml),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelToml {
    relation: Relation,
    reference: String,
    #[serde(default)]
    alignement: Align,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsToml {
    x: i32,
    y: i32,
}

fn from_toml(f: FileToml) -> Result<Vec<Profile>, String> {
    f.profil
        .into_iter()
        .map(|p| {
            let screens = p
                .ecran
                .into_iter()
                .map(|e| {
                    let ctx = |m: String| format!("profil « {} », {} : {m}", p.nom, short_from_description(&e.description));
                    let mode = match &e.mode {
                        None => None,
                        Some(s) => Some(Mode::parse(s).ok_or_else(|| ctx(format!("mode « {s} » illisible")))?),
                    };
                    if e.actif && mode.is_none() {
                        return Err(ctx("mode manquant".into()));
                    }
                    let scale = Scale::from_f64(e.echelle).ok_or_else(|| ctx(format!("échelle {} invalide", e.echelle)))?;
                    if e.rotation > 7 {
                        return Err(ctx(format!("rotation {} invalide (0 à 7)", e.rotation)));
                    }
                    let placement = match e.place {
                        None => Placement::Origin,
                        Some(PlaceToml::Rel(r)) => Placement::Relative { relation: r.relation, reference: r.reference, align: r.alignement },
                        Some(PlaceToml::Abs(a)) => Placement::Absolute { x: a.x, y: a.y },
                    };
                    Ok(ProfileScreen { description: e.description, enabled: e.actif, mode, scale, transform: e.rotation, placement })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Profile { name: p.nom, screens })
        })
        .collect()
}

fn to_toml(profiles: &[Profile]) -> FileToml {
    FileToml {
        profil: profiles
            .iter()
            .map(|p| ProfilToml {
                nom: p.name.clone(),
                ecran: p
                    .screens
                    .iter()
                    .map(|s| EcranToml {
                        description: s.description.clone(),
                        actif: s.enabled,
                        mode: s.mode.map(|m| m.to_string()),
                        echelle: s.scale.as_f64(),
                        rotation: s.transform,
                        place: match &s.placement {
                            Placement::Origin => None,
                            Placement::Relative { relation, reference, align } => Some(PlaceToml::Rel(RelToml {
                                relation: *relation,
                                reference: reference.clone(),
                                alignement: *align,
                            })),
                            Placement::Absolute { x, y } => Some(PlaceToml::Abs(AbsToml { x: *x, y: *y })),
                        },
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Analyse le contenu d'ecrans.toml ; l'erreur est la raison, en français.
pub fn parse(text: &str) -> Result<Vec<Profile>, String> {
    toml::from_str::<FileToml>(text).map_err(|e| e.message().to_string()).and_then(from_toml)
}

/// Absent → aucun profil, sans message ; illisible ou invalide → aucun
/// profil + message pour la ligne d'état.
pub fn load(path: &Path) -> (Vec<Profile>, Option<String>) {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(e) => (Vec::new(), Some(format!("ecrans.toml illisible : {e}"))),
        Ok(s) => match parse(&s) {
            Ok(p) => (p, None),
            Err(e) => (Vec::new(), Some(format!("ecrans.toml invalide : {e}"))),
        },
    }
}

/// Écrit tous les profils (dossier créé au besoin ; fichier remplacé d'un
/// coup, jamais à moitié écrit).
pub fn save(path: &Path, profiles: &[Profile]) -> Result<(), String> {
    let body = toml::to_string(&to_toml(profiles)).map_err(|e| e.to_string())?;
    let text = format!("# Profils d'écran d'alpheus (onglet Écrans), modifiables à la main.\n\n{body}");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Écrans posés tels que Hyprland les rapporte (taille logique), pour la déduction.
pub fn placed(outs: &[Output]) -> Vec<Placed> {
    outs.iter()
        .map(|o| match &o.active {
            Some(a) => Placed {
                id: o.description.clone(),
                enabled: true,
                x: a.x,
                y: a.y,
                size: logical_size(a.mode, a.scale, a.transform),
            },
            None => Placed { id: o.description.clone(), enabled: false, x: 0, y: 0, size: (0, 0) },
        })
        .collect()
}

/// « Enregistrer l'actuel » : un profil qui reproduit l'état courant.
pub fn from_outputs(name: &str, outs: &[Output]) -> Profile {
    let placements = deduce(&placed(outs), None, None);
    let screens = outs
        .iter()
        .zip(placements)
        .map(|(o, placement)| match &o.active {
            Some(a) => ProfileScreen {
                description: o.description.clone(),
                enabled: true,
                mode: Some(a.mode),
                scale: a.scale,
                transform: a.transform,
                placement,
            },
            None => ProfileScreen {
                description: o.description.clone(),
                enabled: false,
                mode: None,
                scale: Scale::ONE,
                transform: 0,
                placement: Placement::Origin,
            },
        })
        .collect();
    Profile { name: name.to_string(), screens }
}

/// Plan d'un profil, et les écrans du profil qui ne sont pas branchés.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePlan {
    pub plan: Plan,
    /// Noms courts des écrans du profil absents (« Bravo »), ignorés.
    pub absent: Vec<String>,
}

/// Calcule l'application d'un profil sur les sorties branchées. Un écran du
/// profil non branché est ignoré (et signalé) ; un écran branché absent du
/// profil reste tel quel. Un placement dont la référence n'est pas branchée
/// devient l'origine si aucune autre n'existe, sinon le profil est refusé.
/// Refusé aussi s'il ne laisse aucun écran actif, ou si la géométrie refuse.
pub fn plan_profile(p: &Profile, outs: &[Output]) -> Result<ProfilePlan, String> {
    let labels = short_names(outs);
    let connected = |d: &str| outs.iter().any(|o| o.description == d);
    let absent: Vec<String> =
        p.screens.iter().filter(|s| !connected(&s.description)).map(|s| short_from_description(&s.description)).collect();
    let in_profile = |o: &Output| p.screens.iter().find(|s| s.description == o.description);
    let mut has_origin = outs
        .iter()
        .filter_map(in_profile)
        .any(|s| s.enabled && s.placement == Placement::Origin);
    let mut screens = Vec::new();
    // Réglage visé de chaque sortie activée : mode, échelle, rotation.
    let mut settings: Vec<Option<(Mode, Scale, u8)>> = Vec::new();
    for (o, label) in outs.iter().zip(&labels) {
        let off = Screen { id: o.description.clone(), label: label.clone(), enabled: false, size: (0, 0), placement: Placement::Origin };
        let (screen, setting) = match in_profile(o) {
            Some(s) if s.enabled => {
                let mode = s.mode.ok_or_else(|| format!("{label} : mode manquant"))?;
                let mut placement = s.placement.clone();
                if let Placement::Relative { reference, .. } = &placement {
                    if !connected(reference) {
                        if has_origin {
                            return Err(format!("{label} est placé par rapport à {}, absent", short_from_description(reference)));
                        }
                        placement = Placement::Origin;
                        has_origin = true;
                    }
                }
                let size = logical_size(mode, s.scale, s.transform);
                (Screen { enabled: true, size, placement, ..off }, Some((mode, s.scale, s.transform)))
            }
            Some(_) => (off, None),
            // Branché mais absent du profil : reste où il est.
            None => match &o.active {
                Some(a) => {
                    let size = logical_size(a.mode, a.scale, a.transform);
                    let placement = Placement::Absolute { x: a.x, y: a.y };
                    (Screen { enabled: true, size, placement, ..off }, Some((a.mode, a.scale, a.transform)))
                }
                None => (off, None),
            },
        };
        screens.push(screen);
        settings.push(setting);
    }
    if !screens.iter().any(|s| s.enabled) {
        return Err("ce profil désactiverait le dernier écran actif".into());
    }
    let pos = resolve(&screens).map_err(|e| e.to_string())?;
    let target: Vec<(String, Option<Active>)> = outs
        .iter()
        .zip(pos.into_iter().zip(settings))
        .map(|(o, (pos, setting))| {
            let active = match (pos, setting) {
                (Some((x, y)), Some((mode, scale, transform))) => Some(Active { mode, x, y, scale, transform }),
                _ => None,
            };
            (o.name.clone(), active)
        })
        .collect();
    Ok(ProfilePlan { plan: Plan::between(outs, &target), absent })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::testing::*;
    use crate::displays::Step;
    use std::path::PathBuf;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-ecrans-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    const BUREAU: &str = r#"
[[profil]]
nom = "Bureau"
[[profil.ecran]]
description = "Alpha Modèle 1 SERIE0001"
mode = "1920x1080@60"
[[profil.ecran]]
description = "Bravo Modèle 2 SERIE0002"
actif = true
mode = "1920x1080@60"
echelle = 1.0
rotation = 0
place = { relation = "right_of", reference = "Alpha Modèle 1 SERIE0001", alignement = "start" }
[[profil.ecran]]
description = "Interne Dalle 0x0001"
actif = false

[[profil]]
nom = "Portable seul"
[[profil.ecran]]
description = "Interne Dalle 0x0001"
mode = "2560x1600@60"
echelle = 1.25
place = { x = 0, y = 0 }
"#;

    fn write(name: &str, content: &str) -> PathBuf {
        let d = dir(name);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("ecrans.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    fn bureau() -> Profile {
        parse(BUREAU).unwrap().remove(0)
    }

    #[test]
    fn lecture() {
        let (p, w) = load(&write("lecture", BUREAU));
        assert_eq!(w, None);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].name, "Bureau");
        assert_eq!(p[0].screens[0], ProfileScreen {
            description: ALPHA.into(),
            enabled: true,
            mode: Some(mode(1920, 1080, 60)),
            scale: Scale::ONE,
            transform: 0,
            placement: Placement::Origin,
        });
        assert_eq!(p[0].screens[1].placement, Placement::Relative { relation: Relation::RightOf, reference: ALPHA.into(), align: Align::Start });
        assert!(!p[0].screens[2].enabled);
        assert_eq!(p[1].screens[0].scale, Scale(1250));
        assert_eq!(p[1].screens[0].placement, Placement::Absolute { x: 0, y: 0 }, "forme absolue acceptée");
    }

    #[test]
    fn absent_sans_message() {
        assert_eq!(load(Path::new("/nonexistent/alpheus/ecrans.toml")), (Vec::new(), None));
    }

    #[test]
    fn invalide_avec_message() {
        for (nom, contenu, attendu) in [
            ("syntaxe", "[[profil]\nnom = 3", "ecrans.toml invalide"),
            ("champ", "[[profil]]\nnom = \"X\"\ncouleur = 1\n", "couleur"),
            ("mode", "[[profil]]\nnom = \"X\"\n[[profil.ecran]]\ndescription = \"Alpha 1\"\nmode = \"1080p\"\n", "mode « 1080p » illisible"),
            ("sans-mode", "[[profil]]\nnom = \"X\"\n[[profil.ecran]]\ndescription = \"Alpha 1\"\n", "Alpha : mode manquant"),
            ("rotation", "[[profil]]\nnom = \"X\"\n[[profil.ecran]]\ndescription = \"Alpha 1\"\nmode = \"1920x1080@60\"\nrotation = 90\n", "rotation 90 invalide"),
            ("relation", "[[profil]]\nnom = \"X\"\n[[profil.ecran]]\ndescription = \"A\"\nmode = \"1920x1080@60\"\nplace = { relation = \"devant\", reference = \"B\" }\n", "ecrans.toml invalide"),
        ] {
            let (p, w) = load(&write(nom, contenu));
            assert!(p.is_empty(), "{nom}");
            let w = w.unwrap_or_default();
            assert!(w.starts_with("ecrans.toml invalide") && w.contains(attendu), "{nom} : {w}");
        }
    }

    #[test]
    fn ecriture_puis_relecture() {
        let path = dir("ecriture").join("sous-dossier").join("ecrans.toml");
        let (profils, _) = load(&write("source", BUREAU));
        save(&path, &profils).unwrap();
        let (relu, w) = load(&path);
        assert_eq!(w, None);
        assert_eq!(relu, profils);
        let texte = std::fs::read_to_string(&path).unwrap();
        assert!(texte.starts_with("# Profils d'écran d'alpheus"), "{texte}");
        assert!(texte.contains("nom = \"Bureau\"") && texte.contains("relation = \"right_of\""), "{texte}");
        assert!(!path.with_extension("toml.tmp").exists(), "pas de fichier temporaire laissé");
    }

    #[test]
    fn enregistrer_l_actuel_deduit_les_placements() {
        let mut outs = poste();
        outs.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
        let p = from_outputs("Maison", &outs);
        assert_eq!(p.name, "Maison");
        assert_eq!(p.screens.len(), 4);
        assert_eq!(p.screens[0].placement, Placement::Absolute { x: -213, y: 1080 }, "non reproductible : absolu");
        assert_eq!(p.screens[1].placement, Placement::Origin);
        assert_eq!(p.screens[2].placement, Placement::Relative { relation: Relation::RightOf, reference: ALPHA.into(), align: Align::Start });
        assert_eq!((p.screens[3].enabled, p.screens[3].mode), (false, None));
        // Réappliqué tel quel : rien à faire.
        assert_eq!(plan_profile(&p, &outs).unwrap().plan, Plan::default());
    }

    #[test]
    fn profil_applique() {
        let outs = poste();
        let pp = plan_profile(&bureau(), &outs).unwrap();
        assert!(pp.absent.is_empty());
        // Alpha à l'origine (0, 0), Bravo à sa droite, la dalle éteinte en dernier.
        assert_eq!(pp.plan.steps, vec![
            Step { output: "DVI-I-1".into(), active: Some(Active { mode: mode(1920, 1080, 60), x: 0, y: 0, scale: Scale::ONE, transform: 0 }) },
            Step { output: "HDMI-A-1".into(), active: Some(Active { mode: mode(1920, 1080, 60), x: 1920, y: 0, scale: Scale::ONE, transform: 0 }) },
            Step { output: "eDP-1".into(), active: None },
        ]);
    }

    #[test]
    fn profil_incomplet_ecran_absent_ignore() {
        // Bravo débranché ; une sortie branchée hors profil reste telle quelle.
        let outs = vec![poste()[0].clone(), poste()[1].clone(), on("DP-3", "Charlie Modèle 3 SERIE0003", 1280, 1024, 5000, 0)];
        let pp = plan_profile(&bureau(), &outs).unwrap();
        assert_eq!(pp.absent, vec!["Bravo"]);
        assert!(!pp.plan.touches("DP-3"), "hors profil : inchangée");
        assert!(pp.plan.touches("DVI-I-1") && pp.plan.touches("eDP-1"));
    }

    #[test]
    fn reference_absente_devient_l_origine_si_possible() {
        // Seul Bravo est branché (avec la dalle) : sa référence Alpha manque,
        // aucune autre origine active → Bravo devient l'origine.
        let outs = vec![poste()[0].clone(), poste()[2].clone()];
        let pp = plan_profile(&bureau(), &outs).unwrap();
        assert_eq!(pp.absent, vec!["Alpha"]);
        assert_eq!(pp.plan.steps[0], Step { output: "HDMI-A-1".into(), active: Some(Active { mode: mode(1920, 1080, 60), x: 0, y: 0, scale: Scale::ONE, transform: 0 }) });
        // Une autre origine existe déjà : refus avec la raison.
        let mut p = bureau();
        p.screens[1].placement = Placement::Relative { relation: Relation::Below, reference: "Charlie Modèle 3 SERIE0003".into(), align: Align::Start };
        assert_eq!(plan_profile(&p, &poste()).unwrap_err(), "Bravo est placé par rapport à Charlie, absent");
    }

    #[test]
    fn profil_qui_eteindrait_tout_refuse() {
        // Bureau loin du dock : Alpha et Bravo absents, la dalle éteinte.
        let outs = vec![poste()[0].clone()];
        assert_eq!(plan_profile(&bureau(), &outs).unwrap_err(), "ce profil désactiverait le dernier écran actif");
    }

    #[test]
    fn refus_de_la_geometrie() {
        let mut p = bureau();
        p.screens[1].placement = Placement::Absolute { x: 100, y: 0 };
        assert_eq!(plan_profile(&p, &poste()).unwrap_err(), "Alpha et Bravo se chevauchent");
    }

    #[test]
    fn frequence_arrondie_rien_a_faire() {
        // Hyprland rapporte 59,934 Hz, lu « @60 » : le profil à 60 Hz est déjà en place.
        let outs = vec![on("DVI-I-1", ALPHA, 1920, 1080, 0, 0), on("HDMI-A-1", BRAVO, 1920, 1080, 1920, 0), off("eDP-1", INTERNE)];
        assert_eq!(plan_profile(&bureau(), &outs).unwrap().plan, Plan::default());
    }
}
