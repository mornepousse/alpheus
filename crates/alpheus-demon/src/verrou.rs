//! Un seul démon à la fois : `flock` exclusif sur
//! `$XDG_RUNTIME_DIR/alpheus-demon.lock`, tenu tant que le démon vit et
//! relâché par le noyau à sa fin, même brutale (aucun fichier orphelin qui
//! bloquerait le suivant).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug)]
pub struct Verrou {
    _fichier: File,
}

/// Message pour un `flock` en échec : « tourne déjà » seulement si l'erreur
/// est `EWOULDBLOCK` (le verrou est tenu par un autre démon, `LOCK_NB` a
/// refusé d'attendre) ; toute autre erreur (droits, système de fichiers…)
/// affiche son vrai message — sinon elle serait prise, à tort, pour un
/// second démon alors que la cause est ailleurs.
fn message_flock(err: &io::Error, chemin: &Path, pid: &str) -> String {
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        format!("alpheus-demon tourne déjà (pid {}) ; verrou {}", pid.trim(), chemin.display())
    } else {
        format!("verrou {} : {err}", chemin.display())
    }
}

pub fn prendre(chemin: &Path) -> Result<Verrou, String> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(chemin)
        .map_err(|e| format!("verrou {} : {e}", chemin.display()))?;
    // SAFETY : flock(2) sur un descripteur valide que `f` possède.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        let mut pid = String::new();
        let _ = f.read_to_string(&mut pid);
        return Err(message_flock(&err, chemin, &pid));
    }
    f.set_len(0).and_then(|_| f.write_all(format!("{}\n", std::process::id()).as_bytes())).map_err(|e| format!("verrou {} : {e}", chemin.display()))?;
    Ok(Verrou { _fichier: f })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_seul_demon_a_la_fois() {
        let p = std::env::temp_dir().join(format!("alpheus-verrou-{}", std::process::id()));
        let v = prendre(&p).unwrap();
        let e = prendre(&p).unwrap_err();
        assert!(e.contains("déjà") && e.contains(&std::process::id().to_string()), "{e}");
        drop(v);
        let _v2 = prendre(&p).expect("verrou relâché à la fin du démon");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dossier_absent_message_clair() {
        assert!(prendre(Path::new("/nonexistent/alpheus-demon.lock")).unwrap_err().contains("/nonexistent"));
    }

    #[test]
    fn flock_rate_autre_que_deja_affiche_la_vraie_erreur() {
        // EWOULDBLOCK (verrou déjà tenu, LOCK_NB) : message « tourne déjà ».
        let chemin = Path::new("/run/alpheus-demon.lock");
        let m = message_flock(&io::Error::from_raw_os_error(libc::EWOULDBLOCK), chemin, "42");
        assert!(m.contains("déjà") && m.contains("42"), "{m}");
        // Toute autre erreur (ici simulée, sans vraiment appeler flock) :
        // le message réel, pas « tourne déjà » qui ferait croire à un
        // second démon alors que la cause est ailleurs (droits, FS…).
        let m = message_flock(&io::Error::from_raw_os_error(libc::EACCES), chemin, "");
        assert!(!m.contains("déjà"), "{m}");
        assert!(m.contains(&chemin.display().to_string()), "{m}");
    }
}
