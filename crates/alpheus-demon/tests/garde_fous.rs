//! Garde-fous du cadrage pour le démon, vérifiés mécaniquement : il ne lance
//! que la commande des bureaux (`wm-set workspace …` par défaut), ne lit le
//! compositeur qu'en lecture (quatre requêtes Hyprland fixes, le flux niri),
//! n'ouvre les sockets du compositeur que dans `src/sources/`, ne lance
//! aucun processus en direct, et les mots interdits n'apparaissent nulle
//! part dans ses sources.

use alpheus_commun::exec::fake::FakeExecutor;
use alpheus_demon::actions::{commande, executer, Bouton};
use alpheus_demon::compositeur::{Bureau, EtatCompositeur, Fenetre};
use alpheus_demon::config::BarreConfig;
use alpheus_demon::etat::{Barre, Msg};
use alpheus_demon::sources::{hypr::Requete, niri::DEMANDE};
use std::path::{Path, PathBuf};

fn bureau(nom: &str, sortie: &str, fenetres: u16, visible: bool) -> Bureau {
    Bureau { nom: nom.into(), reference: nom.into(), sortie: sortie.into(), fenetres, urgent: false, visible, focalise: visible }
}

#[test]
fn chaque_clic_ne_lance_que_la_commande_des_bureaux() {
    let mut b = Barre::new(BarreConfig::default());
    b.appliquer(Msg::Compositeur(Some(EtatCompositeur {
        bureaux: vec![bureau("1", "eDP-1", 1, true), bureau("2", "eDP-1", 0, false), bureau("3", "HDMI-A-1", 2, true)],
        fenetre: Some(Fenetre { classe: "firefox".into(), titre: "t".into() }),
    })));
    let fake = FakeExecutor::new();
    for sortie in ["eDP-1", "HDMI-A-1"] {
        let g = b.grille(sortie, 200);
        for z in g.zones() {
            for bt in [Bouton::Gauche, Bouton::Droit, Bouton::Milieu, Bouton::MoletteHaut, Bouton::MoletteBas] {
                if let Some(argv) = commande(&b, sortie, &z.cible, bt) {
                    executer(&fake, &argv);
                }
            }
        }
    }
    let calls = fake.calls();
    assert!(calls.len() >= 4, "{calls:?}");
    for c in &calls {
        assert!(c.starts_with("wm-set workspace "), "commande hors liste : {c}");
        assert_eq!(c.split(' ').count(), 4, "{c}");
        assert!(!c.to_lowercase().contains("dpms"), "{c}");
    }
}

#[test]
fn requetes_au_compositeur_en_lecture_seule() {
    let textes: Vec<&str> = Requete::TOUTES.iter().map(|r| r.texte()).collect();
    assert_eq!(textes, ["j/monitors", "j/workspaces", "j/clients", "j/activewindow"]);
    assert_eq!(DEMANDE, "\"EventStream\"");
}

fn sources() -> Vec<(PathBuf, String)> {
    fn parcourir(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                parcourir(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut f = Vec::new();
    parcourir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut f);
    assert!(f.len() > 10);
    f.into_iter()
        .map(|p| {
            let t = std::fs::read_to_string(&p).unwrap();
            (p, t)
        })
        .collect()
}

/// Chemin de `p` relatif à `src/` (p. ex. `sources/hypr.rs`, ou
/// `sources/hypr/mod.rs` si `sources::hypr` devient un jour un dossier) :
/// contrairement aux deux derniers composants du chemin, cette forme reste
/// correcte quelle que soit la profondeur de nidification — un simple
/// `components().rev().take(2)` perdrait le préfixe `sources/` dès qu'un
/// module gagnerait un sous-dossier.
fn chemin_relatif(p: &Path) -> String {
    let racine = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    p.strip_prefix(&racine).expect("chemin hors de src/").to_string_lossy().into_owned()
}

#[test]
fn le_demon_ne_parle_au_compositeur_qu_en_lecture() {
    for (p, t) in sources() {
        let bas = t.to_lowercase();
        for mot in ["dpms", "hyprctl", "niri msg", "swaymsg", "keyword", "\"dispatch"] {
            assert!(!bas.contains(mot), "« {mot} » dans {}", p.display());
        }
        let nom = chemin_relatif(&p);
        if t.contains(".socket.sock") {
            assert_eq!(nom, "sources/hypr.rs", "socket de requêtes Hyprland ouvert ailleurs");
        }
        if t.contains("UnixStream::connect") {
            assert!(nom == "sources/hypr.rs" || nom == "sources/niri.rs", "connexion au compositeur dans {nom}");
        }
        assert!(!t.contains("Command::new"), "processus lancé en direct dans {nom} (passer par l'exécuteur)");
    }
}

/// Garde-fou de revue : seuls les fichiers de `src/sources/` ont le droit
/// d'ouvrir ou de se connecter aux sockets du compositeur — jamais ailleurs
/// dans le démon (peintre, wayland, étal, grille…), qui ne reçoivent que
/// l'état déjà normalisé (`EtatCompositeur`), jamais une connexion brute.
/// Généralise les deux vérifications ci-dessus (qui ne couvraient pas
/// `.socket2.sock`) en une règle unique, par dossier plutôt que par fichier
/// nommé, pour rester valable si `sources::hypr` ou `sources::niri` se
/// scindent un jour en plusieurs fichiers.
#[test]
fn sockets_du_compositeur_ouverts_seulement_dans_sources() {
    for (p, t) in sources() {
        let nom = chemin_relatif(&p);
        for motif in [".socket.sock", ".socket2.sock", "UnixStream::connect"] {
            assert!(!t.contains(motif) || nom.starts_with("sources/"), "« {motif} » hors de src/sources/ : {nom}");
        }
    }
}
