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

/// Un `UnixStream::connect` (ou l'ouverture d'un socket du compositeur)
/// n'est permis que dans `src/sources/` — et n'importe où dans ce dossier,
/// pas seulement dans les deux fichiers nommés `hypr.rs` et `niri.rs` : le
/// reste du démon (peintre, wayland, étal, grille…) ne reçoit que l'état
/// déjà normalisé (`EtatCompositeur`), jamais une connexion brute. Reste
/// valable si `sources::hypr` ou `sources::niri` se scindent un jour en
/// plusieurs fichiers, ou si un module partagé (`sources/commun.rs`)
/// apparaît.
fn connexion_compositeur_autorisee(chemin_relatif: &str) -> bool {
    chemin_relatif.starts_with("sources/")
}

#[test]
fn connexion_autorisee_dans_tout_sources_pas_seulement_hypr_et_niri() {
    assert!(connexion_compositeur_autorisee("sources/hypr.rs"));
    assert!(connexion_compositeur_autorisee("sources/niri.rs"));
    assert!(connexion_compositeur_autorisee("sources/mod.rs"), "pas seulement les deux fichiers nommés");
    assert!(connexion_compositeur_autorisee("sources/futur/sous_module.rs"), "même nidifié plus profond");
    assert!(!connexion_compositeur_autorisee("wayland.rs"));
    assert!(!connexion_compositeur_autorisee("peintre.rs"));
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
            assert!(connexion_compositeur_autorisee(&nom), "connexion au compositeur dans {nom}");
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
            assert!(!t.contains(motif) || connexion_compositeur_autorisee(&nom), "« {motif} » hors de src/sources/ : {nom}");
        }
    }
}

/// Code de production d'un fichier : tout, SAUF l'élément qui suit chaque
/// `#[cfg(test)]` (une fonction, un module `mod tests { … }`, un module de
/// fixtures comme `sources/hypr.rs::faux`…), quel que soit son nom ou sa
/// place dans le fichier — pas seulement « le dernier `#[cfg(test)]`,
/// forcément un module de tests final » : `compositeur/mod.rs` et
/// `sources/hypr.rs` en ont chacun DEUX, et rien ne garantit qu'un futur
/// fichier n'en ait pas un au milieu. Repère la fin de l'élément en comptant
/// les accolades (correct même nidifié), pas en coupant au premier
/// `#[cfg(test)]` rencontré (ça aurait aussi, à tort, effacé tout ce qui
/// suit dans le fichier, test ou non).
fn code_de_production(texte: &str) -> String {
    let mut sortie = String::with_capacity(texte.len());
    let mut reste = texte;
    while let Some(i) = reste.find("#[cfg(test)]") {
        sortie.push_str(&reste[..i]);
        let apres_marqueur = &reste["#[cfg(test)]".len() + i..];
        let Some(ouvre) = apres_marqueur.find('{') else {
            // Pas d'accolade après ce marqueur (improbable ici, mais pas
            // notre travail de le refuser) : rien à retirer, on avance
            // juste après le marqueur pour ne pas boucler dessus.
            reste = apres_marqueur;
            continue;
        };
        let mut profondeur = 0u32;
        let mut fin_relative = None;
        for (j, c) in apres_marqueur[ouvre..].char_indices() {
            match c {
                '{' => profondeur += 1,
                '}' => {
                    profondeur -= 1;
                    if profondeur == 0 {
                        fin_relative = Some(ouvre + j + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        // Accolade jamais refermée : ne devrait pas arriver dans du Rust
        // qui compile, mais une chaîne vide plutôt qu'un panique sur un
        // fichier malformé reste le choix le plus sûr pour un garde-fou.
        reste = fin_relative.map(|f| &apres_marqueur[f..]).unwrap_or("");
    }
    sortie.push_str(reste);
    sortie
}

#[test]
fn code_de_production_retire_chaque_element_suivant_cfg_test() {
    let texte = "fn x() {}\n#[cfg(test)]\nfn aide() { eprintln!(\"x\"); }\nfn y() {}\n#[cfg(test)]\nmod tests {\n    fn z() { eprintln!(\"ok\"); }\n}\n";
    assert_eq!(code_de_production(texte), "fn x() {}\n\nfn y() {}\n\n", "deux marqueurs, pas seulement le premier ou le dernier");
}

/// `eprintln!`/`eprint!` paniquent si l'autre bout de l'erreur standard est
/// fermé (EPIPE) : interdits dans le code de production du démon, qui doit
/// toujours passer par `alpheus_demon::journal::ecrire` (`writeln!`, une
/// écriture ratée simplement ignorée). Les tests ont le droit d'en user :
/// ils ne vivent qu'une fraction de seconde, aucun risque d'EPIPE comme
/// dans un démon résident.
#[test]
fn eprint_interdit_hors_des_tests() {
    for (p, t) in sources() {
        let prod = code_de_production(&t);
        for mot in ["eprintln!(", "eprint!("] {
            assert!(!prod.contains(mot), "{mot} (panique sur EPIPE) dans {} : utiliser crate::journal::ecrire", p.display());
        }
    }
}
