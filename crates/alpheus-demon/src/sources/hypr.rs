//! Hyprland en LECTURE seule. Deux sockets dans
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/` :
//! - `.socket2.sock` : le flux d'événements, lu en continu ;
//! - `.socket.sock` : quatre requêtes de lecture (`Requete`), au démarrage
//!   et après un événement de structure. Le flux ne redonne pas l'état
//!   initial, d'où ces lectures (Q1 du plan). La seule fonction qui écrit
//!   sur ce socket prend une `Requete`, dont les quatre textes sont fixes et
//!   vérifiés par tests/garde_fous.rs : aucune action, aucun réglage.

use crate::compositeur::hypr::{analyser_ligne, Effet, Instantane, Modele, Reponses};
use crate::compositeur::EtatCompositeur;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requete {
    Moniteurs,
    Bureaux,
    Clients,
    FenetreActive,
}

impl Requete {
    pub const TOUTES: [Requete; 4] = [Requete::Moniteurs, Requete::Bureaux, Requete::Clients, Requete::FenetreActive];

    pub fn texte(self) -> &'static str {
        match self {
            Requete::Moniteurs => "j/monitors",
            Requete::Bureaux => "j/workspaces",
            Requete::Clients => "j/clients",
            Requete::FenetreActive => "j/activewindow",
        }
    }
}

/// Calme attendu après un événement de structure avant de relire l'état :
/// une rafale (dix fenêtres fermées d'un coup) ne coûte qu'une relecture.
pub const CALME: Duration = Duration::from_millis(40);
/// Relecture forcée si le flux ne se calme jamais (titre d'un terminal mis
/// à jour en continu).
pub const ATTENTE_MAX: Duration = Duration::from_millis(250);
const DELAI_REQUETE: Duration = Duration::from_secs(2);

/// Délai de lecture du flux : aucun tant que rien n'attend (le fil dort
/// sans jamais se réveiller), `CALME` quand une relecture est due.
pub fn delai_lecture(a_relire: bool) -> Option<Duration> {
    a_relire.then_some(CALME)
}

fn demander(dossier: &Path, r: Requete) -> io::Result<String> {
    let mut s = UnixStream::connect(dossier.join(".socket.sock"))?;
    s.set_read_timeout(Some(DELAI_REQUETE))?;
    s.set_write_timeout(Some(DELAI_REQUETE))?;
    s.write_all(r.texte().as_bytes())?;
    // Octets bruts puis conversion tolérante : un titre de fenêtre mal
    // encodé ne doit pas rendre tout l'état illisible.
    let mut rep = Vec::new();
    s.read_to_end(&mut rep)?;
    Ok(String::from_utf8_lossy(&rep).into_owned())
}

fn instantane(dossier: &Path) -> io::Result<Instantane> {
    let m = demander(dossier, Requete::Moniteurs)?;
    let b = demander(dossier, Requete::Bureaux)?;
    let c = demander(dossier, Requete::Clients)?;
    let a = demander(dossier, Requete::FenetreActive)?;
    Instantane::analyser(&Reponses { moniteurs: &m, bureaux: &b, clients: &c, active: &a }).map_err(io::Error::other)
}

pub fn session(dossier: &Path, publier: &mut dyn FnMut(EtatCompositeur) -> bool) -> io::Result<()> {
    // Le flux d'abord : un événement survenu pendant la lecture de l'état
    // attend dans le socket au lieu d'être perdu.
    let flux = UnixStream::connect(dossier.join(".socket2.sock"))?;
    let mut modele = Modele::default();
    modele.instantane(instantane(dossier)?);
    if !publier(modele.etat()) {
        return Ok(());
    }
    let mut lecteur = BufReader::new(flux);
    let mut ligne = Vec::new();
    let mut due: Option<Instant> = None; // relecture due depuis
    loop {
        lecteur.get_ref().set_read_timeout(delai_lecture(due.is_some()))?;
        let relire = match lecteur.read_until(b'\n', &mut ligne) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "flux d'événements Hyprland fermé")),
            Ok(_) if ligne.ends_with(b"\n") => {
                let texte = String::from_utf8_lossy(&ligne);
                let effet = modele.appliquer(&analyser_ligne(texte.trim_end()));
                ligne.clear();
                match effet {
                    Effet::Rien => {}
                    Effet::Changement => {
                        if !publier(modele.etat()) {
                            return Ok(());
                        }
                    }
                    Effet::Relire => {
                        due.get_or_insert_with(Instant::now);
                    }
                }
                due.is_some_and(|t| t.elapsed() >= ATTENTE_MAX)
            }
            Ok(_) => false, // ligne incomplète : la fin du flux suit
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => due.is_some(),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => false,
            Err(e) => return Err(e),
        };
        if relire {
            // Une ligne à moitié lue reste dans `ligne` (read_until garde les
            // octets reçus) : rien n'est perdu.
            modele.instantane(instantane(dossier)?);
            due = None;
            if !publier(modele.etat()) {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod faux {
    //! Faux Hyprland : les deux sockets dans un dossier temporaire, l'état
    //! servi depuis les fixtures, les requêtes reçues journalisées.
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    pub const MONITEURS: &str = include_str!("../../tests/fixtures/hypr-monitors.json");
    pub const BUREAUX: &str = include_str!("../../tests/fixtures/hypr-workspaces.json");
    pub const CLIENTS: &str = include_str!("../../tests/fixtures/hypr-clients.json");
    pub const ACTIVE: &str = include_str!("../../tests/fixtures/hypr-activewindow.json");

    pub struct FauxHyprland {
        pub dossier: PathBuf,
        requetes: Arc<Mutex<Vec<String>>>,
        flux: Arc<Mutex<Option<UnixStream>>>,
    }

    impl FauxHyprland {
        pub fn demarrer(nom: &str) -> FauxHyprland {
            FauxHyprland::demarrer_avec(nom, CLIENTS.as_bytes(), false)
        }

        /// `clients` : réponse servie à `j/clients` (octets bruts) ;
        /// `bagoteur` : le flux accepte chaque connexion puis la ferme aussitôt.
        pub fn demarrer_avec(nom: &str, clients: &[u8], bagoteur: bool) -> FauxHyprland {
            let clients = clients.to_vec();
            let dossier = std::env::temp_dir().join(format!("alpheus-hypr-{}-{nom}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dossier);
            std::fs::create_dir_all(&dossier).unwrap();
            let requetes = Arc::new(Mutex::new(Vec::new()));
            let flux = Arc::new(Mutex::new(None));
            let l1 = UnixListener::bind(dossier.join(".socket.sock")).unwrap();
            let r = requetes.clone();
            std::thread::spawn(move || {
                for c in l1.incoming() {
                    let Ok(mut c) = c else { return };
                    let mut buf = [0u8; 256];
                    let n = c.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let rep: &[u8] = match req.as_str() {
                        "j/monitors" => MONITEURS.as_bytes(),
                        "j/workspaces" => BUREAUX.as_bytes(),
                        "j/clients" => &clients,
                        "j/activewindow" => ACTIVE.as_bytes(),
                        _ => b"unknown request",
                    };
                    r.lock().unwrap().push(req);
                    let _ = c.write_all(rep);
                }
            });
            let l2 = UnixListener::bind(dossier.join(".socket2.sock")).unwrap();
            let f = flux.clone();
            std::thread::spawn(move || {
                if bagoteur {
                    // Chaque connexion fermée dès l'acceptation.
                    for c in l2.incoming() {
                        drop(c);
                    }
                } else if let Ok((c, _)) = l2.accept() {
                    *f.lock().unwrap() = Some(c);
                }
            });
            FauxHyprland { dossier, requetes, flux }
        }

        pub fn requetes(&self) -> Vec<String> {
            self.requetes.lock().unwrap().clone()
        }

        /// Écrit sur le flux (attend la connexion, 5 s au plus).
        pub fn ecrire(&self, lignes: &str) {
            let t = Instant::now();
            loop {
                if let Some(c) = self.flux.lock().unwrap().as_mut() {
                    c.write_all(lignes.as_bytes()).unwrap();
                    return;
                }
                assert!(t.elapsed() < Duration::from_secs(5), "flux jamais connecté");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        pub fn fermer_flux(&self) {
            if let Some(c) = self.flux.lock().unwrap().take() {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    impl Drop for FauxHyprland {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dossier);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::faux::FauxHyprland;
    use super::*;
    use std::sync::mpsc;

    const DELAI: Duration = Duration::from_secs(5);

    #[test]
    fn quatre_requetes_fixes_en_lecture_seule() {
        // Garde-fou : seules ces quatre lectures JSON partent sur .socket.sock.
        let textes: Vec<&str> = Requete::TOUTES.iter().map(|r| r.texte()).collect();
        assert_eq!(textes, ["j/monitors", "j/workspaces", "j/clients", "j/activewindow"]);
    }

    #[test]
    fn lecture_sans_delai_quand_rien_n_attend() {
        assert_eq!(delai_lecture(false), None, "aucun réveil à vide");
        assert_eq!(delai_lecture(true), Some(CALME));
    }

    #[test]
    fn session_publie_l_etat_puis_suit_le_flux() {
        let f = FauxHyprland::demarrer("suit");
        let (tx, rx) = mpsc::channel();
        let d = f.dossier.clone();
        let h = std::thread::spawn(move || session(&d, &mut |e| tx.send(e).is_ok()));
        let e0 = rx.recv_timeout(DELAI).unwrap();
        assert_eq!(e0.bureaux.len(), 3);
        assert_eq!(f.requetes().len(), 4);
        f.ecrire("activewindow>>Alacritty,~/projets — zsh\n");
        assert_eq!(rx.recv_timeout(DELAI).unwrap().fenetre.unwrap().classe, "Alacritty");
        f.ecrire("openwindow>>583d9c8a1b20,2,Alacritty,~\n");
        let _ = rx.recv_timeout(DELAI).unwrap(); // état relu
        assert_eq!(f.requetes().len(), 8, "une relecture complète après un événement de structure");
        for r in f.requetes() {
            assert!(Requete::TOUTES.iter().any(|q| q.texte() == r), "requête hors liste : {r}");
        }
        f.fermer_flux();
        let fin = h.join().unwrap();
        assert_eq!(fin.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn relecture_meme_sous_un_deluge_de_titres() {
        let f = FauxHyprland::demarrer("deluge");
        let (tx, rx) = mpsc::channel();
        let d = f.dossier.clone();
        std::thread::spawn(move || session(&d, &mut |e| tx.send(e).is_ok()));
        rx.recv_timeout(DELAI).unwrap();
        f.ecrire("openwindow>>583d9c8a1b20,2,Alacritty,~\n");
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(600) {
            f.ecrire("windowtitlev2>>583d9c8a1b20,compilation en cours\n");
            std::thread::sleep(Duration::from_millis(10)); // jamais 40 ms de calme
        }
        assert!(f.requetes().len() >= 8, "la relecture ne doit pas attendre un calme qui ne vient pas");
    }

    #[test]
    fn titre_utf8_invalide_ne_casse_pas_l_etat() {
        let clients = faux::CLIENTS.replacen("\"title\": \"~/projets", "\"title\": \"\u{1}~/projets", 1).into_bytes();
        // Un octet invalide en UTF-8 à la place du marqueur.
        let clients: Vec<u8> = clients.into_iter().map(|o| if o == 1 { 0xff } else { o }).collect();
        assert!(std::str::from_utf8(&clients).is_err());
        let f = FauxHyprland::demarrer_avec("utf8", &clients, false);
        let (tx, rx) = mpsc::channel();
        let d = f.dossier.clone();
        std::thread::spawn(move || session(&d, &mut |e| tx.send(e).is_ok()));
        assert_eq!(rx.recv_timeout(DELAI).unwrap().bureaux.len(), 3);
    }

    #[test]
    fn hyprland_absent_erreur() {
        let r = session(Path::new("/nonexistent/hypr/x"), &mut |_| true);
        assert!(r.is_err());
    }
}
