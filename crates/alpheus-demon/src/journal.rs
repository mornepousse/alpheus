//! Le journal du démon : une seule fonction, utilisée partout au lieu
//! d'`eprintln!`. `eprintln!` panique si l'autre bout de l'erreur standard
//! est fermé (EPIPE — un `journalctl -f` interrompu, p. ex.) : un démon
//! résident ne doit jamais mourir pour si peu. `writeln!` rend une simple
//! erreur qu'on ignore (rien d'autre ne peut en faire, de toute façon).

use std::io::{self, Write};

/// Partie testable, séparée de `io::stderr()` : un test peut lui donner un
/// bout de tube déjà fermé en lecture (EPIPE garanti à l'écriture) sans
/// toucher à la vraie sortie d'erreur du processus de test, partagée par
/// tous les fils de tests qui tournent en même temps.
fn ecrire_dans(sortie: &mut impl Write, m: &str) {
    let _ = writeln!(sortie, "alpheus-demon : {m}");
}

pub fn ecrire(m: &str) {
    ecrire_dans(&mut io::stderr(), m);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::FromRawFd;

    #[test]
    fn ecrire_survit_a_un_epipe_reel() {
        // Un vrai tube, le bout lecture fermé tout de suite : la prochaine
        // écriture sur le bout écriture rend EPIPE, pour de vrai (pas une
        // simulation) — sans toucher au fd 2 réel du processus de test
        // (voir `ecrire_dans`).
        let mut fds = [0i32; 2];
        // SAFETY : pipe(2), tableau de deux `i32` valide, taille correcte.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2) a échoué");
        let (lecture, ecriture) = (fds[0], fds[1]);
        // SAFETY : `lecture` vient de nous rendre `pipe(2)` à l'instant,
        // jamais encore fermé ni possédé par personne d'autre.
        unsafe { libc::close(lecture) };
        // SAFETY : `ecriture` vient de nous rendre `pipe(2)` à l'instant ;
        // `File` en devient l'unique propriétaire (fermé par son `Drop`).
        let mut tube_casse = unsafe { File::from_raw_fd(ecriture) };
        ecrire_dans(&mut tube_casse, "ce message ne doit jamais faire paniquer le démon");
        // Rien à vérifier de plus : si `ecrire_dans` avait utilisé
        // `writeln!` directement (sans `let _ =`), la ligne au-dessus
        // aurait déjà paniqué.
    }
}
