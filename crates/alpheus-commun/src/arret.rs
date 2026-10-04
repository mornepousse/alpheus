//! Arrêt propre sur signal, partagé par la TUI et le démon.
//! - TUI : un gestionnaire lève `FIN_DEMANDEE`, la boucle le voit au tour
//!   suivant (`installer_signaux`).
//! - Démon : les signaux de fin sont BLOQUÉS dans tous les fils et attendus
//!   par le fil de garde (`bloquer_signaux`, `attendre_signal`) : aucun
//!   réveil périodique pendant que le démon tourne.
//!
//! Dans les deux cas, `surveiller` force la sortie si la boucle ne finit
//! pas d'elle-même dans le délai de grâce.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// Levé par SIGTERM ou SIGHUP (TUI) : la boucle sort normalement.
pub static FIN_DEMANDEE: AtomicBool = AtomicBool::new(false);
/// Numéro du signal reçu (code de sortie 128 + n si la sortie est forcée).
pub static SIGNAL_RECU: AtomicI32 = AtomicI32::new(0);

/// Signaux de fin du démon : SIGTERM (pkill, fin de session), SIGHUP, SIGINT.
pub const SIGNAUX_DE_FIN: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

/// Bloque les signaux de fin dans le fil appelant et dans tous les fils
/// créés ENSUITE (ils héritent du masque) : à appeler avant tout fil.
pub fn bloquer_signaux() -> io::Result<libc::sigset_t> {
    // SAFETY : sigset_t est une structure C simple, initialisée par
    // sigemptyset avant usage ; pthread_sigmask ne garde aucun pointeur.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in SIGNAUX_DE_FIN {
            libc::sigaddset(&mut set, s);
        }
        let r = libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        if r != 0 {
            return Err(io::Error::from_raw_os_error(r));
        }
        Ok(set)
    }
}

/// Attend, sans tourner, l'un des signaux bloqués ; rend son numéro.
pub fn attendre_signal(set: &libc::sigset_t) -> libc::c_int {
    let mut sig: libc::c_int = 0;
    loop {
        // SAFETY : `set` et `sig` vivent pendant l'appel.
        if unsafe { libc::sigwait(set, &mut sig) } == 0 {
            return sig;
        }
    }
}

/// Gestionnaire de signal : deux stores atomiques, async-signal-safe.
extern "C" fn sur_signal(sig: libc::c_int) {
    SIGNAL_RECU.store(sig, Ordering::SeqCst);
    FIN_DEMANDEE.store(true, Ordering::SeqCst);
}

/// Installe le gestionnaire pour SIGTERM et SIGHUP. SA_RESTART : un appel
/// système interrompu (lecture du terminal, attente d'un outil) reprend au
/// lieu d'échouer ; la boucle voit le drapeau au tour suivant (≤ 100 ms).
pub fn installer_signaux() -> io::Result<()> {
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        // SAFETY : sigaction est une structure C simple, valide remplie de zéros.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = sur_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        sa.sa_flags = libc::SA_RESTART;
        // SAFETY : `sa` est valide pendant l'appel ; le gestionnaire ne fait
        // qu'un store atomique (async-signal-safe).
        let r = unsafe {
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut())
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Fil de garde : attend le drapeau de fin, laisse `grace` à la boucle pour
/// sortir d'elle-même (et lever `finie`), sinon appelle `forcer`. Renvoie
/// `true` si la sortie a été forcée. Ne dépend ni du terminal ni de
/// crossterm : testable avec des drapeaux locaux et des délais courts.
pub fn surveiller(fin: &AtomicBool, finie: &AtomicBool, grace: Duration, pas: Duration, forcer: impl FnOnce()) -> bool {
    while !fin.load(Ordering::SeqCst) {
        std::thread::sleep(pas);
    }
    let t = Instant::now();
    while t.elapsed() < grace {
        if finie.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(pas);
    }
    if finie.load(Ordering::SeqCst) {
        return false;
    }
    forcer();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const GRACE: Duration = Duration::from_millis(150);
    const PAS: Duration = Duration::from_millis(5);

    #[test]
    fn garde_force_la_sortie_si_la_boucle_est_coincee() {
        let (fin, finie) = (AtomicBool::new(true), AtomicBool::new(false));
        let mut force = false;
        let t = Instant::now();
        assert!(surveiller(&fin, &finie, GRACE, PAS, || force = true));
        assert!(force, "boucle coincée : sortie forcée");
        assert!(t.elapsed() >= GRACE, "après le délai de grâce seulement ({:?})", t.elapsed());
    }

    #[test]
    fn garde_laisse_sortir_la_boucle_a_temps() {
        let fin = Arc::new(AtomicBool::new(false));
        let finie = Arc::new(AtomicBool::new(false));
        let (f2, d2) = (fin.clone(), finie.clone());
        let h = std::thread::spawn(move || {
            let mut force = false;
            let r = surveiller(&f2, &d2, GRACE, PAS, || force = true);
            (r, force)
        });
        // Plus long que la grâce : sans signal, la garde ne compte rien.
        std::thread::sleep(GRACE + Duration::from_millis(100));
        assert!(!h.is_finished(), "sans signal, la garde attend");
        fin.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        finie.store(true, Ordering::SeqCst); // la boucle sort dans le délai
        assert_eq!(h.join().unwrap(), (false, false), "aucune sortie forcée");
    }

    /// Disposition actuelle du signal `sig`.
    fn gestionnaire(sig: libc::c_int) -> libc::sighandler_t {
        // SAFETY : lecture seule de la disposition (nouvelle action nulle).
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(sig, std::ptr::null(), &mut old), 0);
            old.sa_sigaction
        }
    }

    #[test]
    fn sigterm_et_sighup_levent_le_drapeau() {
        installer_signaux().unwrap();
        let attendu = sur_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for sig in [libc::SIGTERM, libc::SIGHUP] {
            // Vérifié AVANT raise : sans gestionnaire, le signal tuerait les tests.
            assert_eq!(gestionnaire(sig), attendu, "signal {sig} sans gestionnaire");
            FIN_DEMANDEE.store(false, Ordering::SeqCst);
            // SAFETY : raise(3) vers nous-mêmes, gestionnaire installé ci-dessus.
            assert_eq!(unsafe { libc::raise(sig) }, 0);
            assert!(FIN_DEMANDEE.load(Ordering::SeqCst), "signal {sig} : drapeau non levé");
        }
        FIN_DEMANDEE.store(false, Ordering::SeqCst);
        // SAFETY : rétablit la disposition par défaut pour le reste des tests.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
        }
    }

    #[test]
    fn signal_bloque_attendu_par_le_fil_de_garde() {
        let h = std::thread::spawn(|| {
            let set = bloquer_signaux().unwrap();
            // SAFETY : raise(3) vise CE fil ; le signal y est bloqué, il
            // reste en attente jusqu'à sigwait (jamais livré ailleurs).
            assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
            attendre_signal(&set)
        });
        assert_eq!(h.join().unwrap(), libc::SIGTERM);
    }
}
