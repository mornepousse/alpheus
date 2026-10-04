//! ~/.config/alpheus/barre.toml, facultatif. Absent : défauts, sans
//! message. Invalide : défauts et avertissement (journal du démon). Jamais
//! d'arrêt. Chaque réglage est lu une fois au démarrage et ne coûte rien
//! s'il n'est pas utilisé.
//!
//! ```toml
//! modules = ["bureaux", "fenetre", "heure"]   # ordre et présence, de gauche à droite
//! hauteur = 22                                # px logiques
//! [police]
//! famille = "JetBrainsMono Nerd Font Mono"    # absente : une police à chasse fixe du système
//! taille = 14.0
//! repli = []                                  # ex. ["Noto Color Emoji"] : chargées en plus, à la demande
//! [heure]
//! format = "%H:%M"
//! date = "%a %e %b"                           # "" : pas de date
//! [bureaux]
//! commande = ["wm-set", "workspace", "{bureau}", "{sortie}"]
//! ```

use crate::heure;
use serde::Deserialize;
use std::path::Path;

/// Les modules de la barre (spec, § 2), de gauche à droite par défaut. Tous
/// sont acceptés dès maintenant : un barre.toml écrit aujourd'hui reste
/// valable quand les livraisons 3 à 5 arriveront. Ceux qui ne sont pas
/// encore livrés ne s'affichent pas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
    Bureaux,
    Fenetre,
    Lecture,
    Volume,
    Reseau,
    Bluetooth,
    Batterie,
    Ressources,
    Plateau,
    Notifications,
    Heure,
}

impl ModuleId {
    pub const TOUS: [ModuleId; 11] = [
        ModuleId::Bureaux,
        ModuleId::Fenetre,
        ModuleId::Lecture,
        ModuleId::Volume,
        ModuleId::Reseau,
        ModuleId::Bluetooth,
        ModuleId::Batterie,
        ModuleId::Ressources,
        ModuleId::Plateau,
        ModuleId::Notifications,
        ModuleId::Heure,
    ];

    /// Nom dans barre.toml.
    pub fn nom(self) -> &'static str {
        match self {
            ModuleId::Bureaux => "bureaux",
            ModuleId::Fenetre => "fenetre",
            ModuleId::Lecture => "lecture",
            ModuleId::Volume => "volume",
            ModuleId::Reseau => "reseau",
            ModuleId::Bluetooth => "bluetooth",
            ModuleId::Batterie => "batterie",
            ModuleId::Ressources => "ressources",
            ModuleId::Plateau => "plateau",
            ModuleId::Notifications => "notifications",
            ModuleId::Heure => "heure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoliceConfig {
    /// Famille à chasse fixe ; absente du système : une autre à chasse fixe.
    pub famille: String,
    /// Taille en px logiques.
    pub taille: f32,
    /// Familles de repli pour les caractères absents (emoji, CJK…). Vide par
    /// défaut : seules la régulière et la grasse de `famille` sont chargées
    /// (≈ 2 Mo) ; chaque famille ajoutée coûte sa mémoire (Q11).
    pub repli: Vec<String>,
}

impl Default for PoliceConfig {
    fn default() -> Self {
        PoliceConfig { famille: "JetBrainsMono Nerd Font Mono".into(), taille: 14.0, repli: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HeureConfig {
    /// L'heure, en gras d'accent.
    pub format: String,
    /// La date, estompée, avant l'heure ; vide = pas de date.
    pub date: String,
}

impl Default for HeureConfig {
    fn default() -> Self {
        HeureConfig { format: "%H:%M".into(), date: "%a %e %b".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BureauxConfig {
    /// Commande lancée pour aller à un bureau (clic, molette) ; `{bureau}`
    /// et `{sortie}` y sont remplacés. Défaut : `wm-set` (nixos-config de
    /// Mae), seule voie vers le compositeur autorisée par le cadrage.
    pub commande: Vec<String>,
}

impl Default for BureauxConfig {
    fn default() -> Self {
        BureauxConfig { commande: ["wm-set", "workspace", "{bureau}", "{sortie}"].map(String::from).to_vec() }
    }
}

pub const HAUTEUR_DEFAUT: u32 = 22;

#[derive(Debug, Clone, PartialEq)]
pub struct BarreConfig {
    pub modules: Vec<ModuleId>,
    /// Hauteur de la barre, en px logiques.
    pub hauteur: u32,
    pub police: PoliceConfig,
    pub heure: HeureConfig,
    pub bureaux: BureauxConfig,
}

impl Default for BarreConfig {
    fn default() -> Self {
        BarreConfig {
            modules: ModuleId::TOUS.to_vec(),
            hauteur: HAUTEUR_DEFAUT,
            police: PoliceConfig::default(),
            heure: HeureConfig::default(),
            bureaux: BureauxConfig::default(),
        }
    }
}

fn hauteur_defaut() -> u32 {
    HAUTEUR_DEFAUT
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Brut {
    #[serde(default)]
    modules: Option<Vec<ModuleId>>,
    #[serde(default = "hauteur_defaut")]
    hauteur: u32,
    #[serde(default)]
    police: PoliceConfig,
    #[serde(default)]
    heure: HeureConfig,
    #[serde(default)]
    bureaux: BureauxConfig,
}

impl BarreConfig {
    /// Absent → défauts sans message ; illisible ou invalide → défauts + message.
    pub fn load(path: &Path) -> (BarreConfig, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BarreConfig::default(), None),
            Err(e) => (BarreConfig::default(), Some(format!("barre.toml illisible : {e}"))),
            Ok(s) => match Self::depuis_texte(&s) {
                Ok(c) => (c, None),
                Err(m) => (BarreConfig::default(), Some(format!("barre.toml invalide : {m}"))),
            },
        }
    }

    pub fn depuis_texte(s: &str) -> Result<BarreConfig, String> {
        let b: Brut = toml::from_str(s).map_err(|e| e.message().to_string())?;
        let modules = b.modules.unwrap_or_else(|| ModuleId::TOUS.to_vec());
        for (i, m) in modules.iter().enumerate() {
            if modules[..i].contains(m) {
                return Err(format!("module « {} » en double", m.nom()));
            }
        }
        if !(12..=64).contains(&b.hauteur) {
            return Err(format!("hauteur {} hors de 12 à 64 px", b.hauteur));
        }
        if !(6.0..=48.0).contains(&b.police.taille) {
            return Err(format!("police.taille {} hors de 6 à 48 px", b.police.taille));
        }
        if b.police.famille.trim().is_empty() {
            return Err("police.famille vide".into());
        }
        if b.heure.format.trim().is_empty() {
            return Err("heure.format vide".into());
        }
        heure::valider(&b.heure.format).map_err(|m| format!("heure.format : {m}"))?;
        heure::valider(&b.heure.date).map_err(|m| format!("heure.date : {m}"))?;
        if b.bureaux.commande.first().is_none_or(|p| p.trim().is_empty()) {
            return Err("bureaux.commande vide (attendu : [\"programme\", \"argument\", …])".into());
        }
        Ok(BarreConfig { modules, hauteur: b.hauteur, police: b.police, heure: b.heure, bureaux: b.bureaux })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fichier(nom: &str, contenu: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-barre-{nom}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("barre.toml");
        std::fs::write(&p, contenu).unwrap();
        p
    }

    #[test]
    fn absent_defauts_sans_message() {
        let (c, w) = BarreConfig::load(Path::new("/nonexistent/barre.toml"));
        assert_eq!((c, w), (BarreConfig::default(), None));
        let d = BarreConfig::default();
        assert_eq!((d.modules.first(), d.modules.last()), (Some(&ModuleId::Bureaux), Some(&ModuleId::Heure)));
        assert_eq!((d.hauteur, d.police.taille, d.police.repli.len()), (22, 14.0, 0));
        assert_eq!(d.bureaux.commande, ["wm-set", "workspace", "{bureau}", "{sortie}"]);
    }

    #[test]
    fn reglages_lus() {
        let texte = "modules = [\"heure\", \"bureaux\"]\nhauteur = 28\n[police]\nfamille = \"Fira Code\"\nrepli = [\"Noto Color Emoji\"]\n[heure]\nformat = \"%H:%M:%S\"\n[bureaux]\ncommande = [\"mon-outil\", \"{bureau}\"]\n";
        let (c, w) = BarreConfig::load(&fichier("ok", texte));
        assert_eq!(w, None);
        assert_eq!(c.modules, vec![ModuleId::Heure, ModuleId::Bureaux]);
        assert_eq!((c.hauteur, c.police.famille.as_str(), c.police.taille), (28, "Fira Code", 14.0));
        assert_eq!((c.heure.format.as_str(), c.heure.date.as_str()), ("%H:%M:%S", "%a %e %b"));
        assert_eq!(c.police.repli, ["Noto Color Emoji"]);
        assert_eq!(c.bureaux.commande, ["mon-outil", "{bureau}"]);
    }

    #[test]
    fn module_inconnu_defauts_et_message() {
        let (c, w) = BarreConfig::load(&fichier("inconnu", "modules = [\"bureaux\", \"meteo\"]\n"));
        assert_eq!(c, BarreConfig::default());
        let w = w.unwrap();
        assert!(w.contains("barre.toml invalide") && w.contains("meteo"), "{w}");
    }

    #[test]
    fn refus_avec_raison() {
        let err = |t: &str| BarreConfig::depuis_texte(t).unwrap_err();
        assert!(err("modules = [\"heure\", \"heure\"]").contains("double"));
        assert!(err("[heure]\nformat = \"%Q\"").contains("heure.format"));
        assert!(err("[heure]\ndate = \"%\"").contains("heure.date"));
        assert!(err("[heure]\nformat = \"\"").contains("vide"));
        assert!(err("hauteur = 4").contains("hauteur"));
        assert!(err("[police]\ntaille = 200.0").contains("police.taille"));
        assert!(err("[police]\nfamille = \" \"").contains("police.famille"));
        assert!(err("[bureaux]\ncommande = []").contains("bureaux.commande"));
        assert!(BarreConfig::depuis_texte("taille = 3").is_err(), "clé inconnue refusée");
        assert_eq!(BarreConfig::depuis_texte("modules = []").unwrap().modules, vec![]);
    }
}
