//! Un seul démon à la fois : `flock` exclusif sur
//! `$XDG_RUNTIME_DIR/alpheus-demon.lock`, tenu tant que le démon vit et
//! relâché par le noyau à sa fin, même brutale (aucun fichier orphelin qui
//! bloquerait le suivant).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug)]
pub struct Verrou {
    _fichier: File,
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
        let mut pid = String::new();
        let _ = f.read_to_string(&mut pid);
        return Err(format!("alpheus-demon tourne déjà (pid {}) ; verrou {}", pid.trim(), chemin.display()));
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
}
