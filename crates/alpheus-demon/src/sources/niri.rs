//! niri en LECTURE seule : une connexion à `$NIRI_SOCKET`, la requête
//! `"EventStream"`, puis le flux d'événements jusqu'à la fermeture. Aucune
//! autre requête. Lecture bloquante sans délai : aucun réveil à vide.

use crate::compositeur::niri::Modele;
use crate::compositeur::EtatCompositeur;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// La seule requête envoyée à niri (vérifiée par tests/garde_fous.rs).
pub const DEMANDE: &str = "\"EventStream\"";

pub fn session(socket: &Path, publier: &mut dyn FnMut(EtatCompositeur) -> bool) -> io::Result<()> {
    let mut s = UnixStream::connect(socket)?;
    s.write_all(DEMANDE.as_bytes())?;
    s.write_all(b"\n")?;
    let mut lecteur = BufReader::new(s);
    let mut modele = Modele::default();
    let mut ligne = Vec::new();
    loop {
        ligne.clear();
        if lecteur.read_until(b'\n', &mut ligne)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "flux d'événements niri fermé"));
        }
        let texte = String::from_utf8_lossy(&ligne);
        match modele.appliquer_ligne(texte.trim_end()) {
            Ok(true) => {
                if !publier(modele.etat()) {
                    return Ok(());
                }
            }
            Ok(false) => {}
            Err(m) => return Err(io::Error::other(m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;

    const FLUX: &str = include_str!("../../tests/fixtures/niri-evenements.jsonl");

    #[test]
    fn session_lit_le_flux_jusqu_a_la_fermeture() {
        let dossier = std::env::temp_dir().join(format!("alpheus-niri-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        let chemin = dossier.join("niri.sock");
        let l = UnixListener::bind(&chemin).unwrap();
        let serveur = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut lecteur = BufReader::new(c.try_clone().unwrap());
            let mut demande = String::new();
            lecteur.read_line(&mut demande).unwrap();
            let mut c = c;
            c.write_all(FLUX.as_bytes()).unwrap(); // puis fermeture
            demande
        });
        let (tx, rx) = mpsc::channel();
        let r = session(&chemin, &mut |e| tx.send(e).is_ok());
        assert_eq!(serveur.join().unwrap().trim_end(), DEMANDE);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let dernier = rx.try_iter().last().expect("au moins un état publié");
        assert_eq!(dernier.fenetre.unwrap().classe, "com.mitchellh.ghostty");
        assert!(rx.recv_timeout(Duration::from_millis(10)).is_err());
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn demande_est_la_chaine_json_eventstream() {
        // niri attend une requête JSON : la chaîne "EventStream" entre guillemets.
        assert_eq!(serde_json::from_str::<String>(DEMANDE).unwrap(), "EventStream");
    }

    #[test]
    fn niri_absent_erreur() {
        assert!(session(Path::new("/nonexistent/niri.sock"), &mut |_| true).is_err());
    }
}
