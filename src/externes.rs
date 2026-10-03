//! Outils externes (wiremix, nmtui, bluetui) ouverts DANS la fenêtre
//! d'alpheus : alpheus rend le terminal, l'outil tourne au premier plan,
//! puis alpheus reprend son écran. Séquence testée avec de faux écran et
//! lanceur ; aucun vrai terminal en test.

use std::io;
use std::process::Command;

pub trait Screen {
    /// Quitte l'écran alterné, le mode brut et la capture souris.
    fn suspend(&mut self) -> io::Result<()>;
    /// Les rétablit.
    fn resume(&mut self) -> io::Result<()>;
}

pub trait Launcher {
    /// Lance `argv` au premier plan et attend sa fin.
    fn run(&mut self, argv: &[String]) -> Result<(), String>;
}

pub fn open(screen: &mut dyn Screen, launcher: &mut dyn Launcher, argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Err("commande vide dans config.toml".into());
    }
    screen.suspend().map_err(|e| format!("impossible de rendre le terminal : {e}"))?;
    let ran = launcher.run(argv);
    match screen.resume() {
        Err(e) => Err(format!("écran d'alpheus non restauré : {e}")),
        Ok(()) => ran,
    }
}

extern "C" fn ignore_signal(_: libc::c_int) {}

/// Lanceur réel.
pub struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn run(&mut self, argv: &[String]) -> Result<(), String> {
        let Some((prog, args)) = argv.split_first() else {
            return Err("commande vide".into());
        };
        // Hors mode brut, Ctrl+C dans l'outil envoie SIGINT à tout le groupe
        // au premier plan, alpheus compris. Un gestionnaire VIDE protège
        // alpheus ; l'outil, lui, retrouve le comportement par défaut à son
        // exec (un gestionnaire ne s'hérite pas, contrairement à SIG_IGN).
        let handler = ignore_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY : signal(2) avec un gestionnaire vide (async-signal-safe).
        let (old_int, old_quit) =
            unsafe { (libc::signal(libc::SIGINT, handler), libc::signal(libc::SIGQUIT, handler)) };
        let status = Command::new(prog).args(args).status();
        // SAFETY : rétablit les gestionnaires précédents.
        unsafe {
            libc::signal(libc::SIGINT, old_int);
            libc::signal(libc::SIGQUIT, old_quit);
        }
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("{prog} s'est terminé en erreur ({s})")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!("{prog} introuvable")),
            Err(e) => Err(format!("{prog} : {e}")),
        }
    }
}

/// Écran réel (crossterm).
pub struct CrosstermScreen;

impl Screen for CrosstermScreen {
    fn suspend(&mut self) -> io::Result<()> {
        use ratatui::crossterm::{cursor, event, execute, terminal};
        execute!(io::stdout(), event::DisableMouseCapture, terminal::LeaveAlternateScreen, cursor::Show)?;
        terminal::disable_raw_mode()
    }

    fn resume(&mut self) -> io::Result<()> {
        use ratatui::crossterm::{cursor, event, execute, terminal};
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, event::EnableMouseCapture, cursor::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<String>>>;

    struct FakeScreen {
        log: Log,
        fail_suspend: bool,
    }
    impl Screen for FakeScreen {
        fn suspend(&mut self) -> io::Result<()> {
            self.log.borrow_mut().push("quitter l'écran".into());
            if self.fail_suspend { Err(io::Error::other("tty")) } else { Ok(()) }
        }
        fn resume(&mut self) -> io::Result<()> {
            self.log.borrow_mut().push("restaurer l'écran".into());
            Ok(())
        }
    }

    struct FakeLauncher {
        log: Log,
        result: Result<(), String>,
    }
    impl Launcher for FakeLauncher {
        fn run(&mut self, argv: &[String]) -> Result<(), String> {
            self.log.borrow_mut().push(format!("lancer {}", argv.join(" ")));
            self.result.clone()
        }
    }

    fn fakes(fail_suspend: bool, result: Result<(), String>) -> (Log, FakeScreen, FakeLauncher) {
        let log: Log = Rc::default();
        (log.clone(), FakeScreen { log: log.clone(), fail_suspend }, FakeLauncher { log, result })
    }

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn sequence_quitter_lancer_restaurer() {
        let (log, mut s, mut l) = fakes(false, Ok(()));
        assert_eq!(open(&mut s, &mut l, &argv("wiremix")), Ok(()));
        assert_eq!(*log.borrow(), vec!["quitter l'écran", "lancer wiremix", "restaurer l'écran"]);
    }

    #[test]
    fn l_ecran_revient_meme_si_l_outil_echoue() {
        let (log, mut s, mut l) = fakes(false, Err("wiremix introuvable".into()));
        assert_eq!(open(&mut s, &mut l, &argv("wiremix")), Err("wiremix introuvable".into()));
        assert_eq!(log.borrow().last().unwrap(), "restaurer l'écran");
    }

    #[test]
    fn rien_n_est_lance_si_le_terminal_ne_se_libere_pas() {
        let (log, mut s, mut l) = fakes(true, Ok(()));
        assert!(open(&mut s, &mut l, &argv("nmtui")).unwrap_err().contains("impossible de rendre le terminal"));
        assert_eq!(*log.borrow(), vec!["quitter l'écran"]);
    }

    #[test]
    fn commande_vide() {
        let (log, mut s, mut l) = fakes(false, Ok(()));
        assert!(open(&mut s, &mut l, &[]).is_err());
        assert!(log.borrow().is_empty());
    }

    // Lanceur réel : seulement `sh` (aucun outil de session).
    #[test]
    fn lanceur_reel() {
        let mut l = ProcessLauncher;
        assert_eq!(l.run(&argv("sh -c true")), Ok(()));
        assert!(l.run(&["sh".into(), "-c".into(), "exit 4".into()]).unwrap_err().contains("4"));
        assert_eq!(l.run(&argv("alpheus-programme-inexistant")), Err("alpheus-programme-inexistant introuvable".into()));
    }

    #[test]
    fn alpheus_survit_au_ctrl_c_de_l_outil() {
        // Ctrl+C dans l'outil envoie SIGINT à tout le groupe au premier plan,
        // alpheus compris. On le simule : l'outil tue son parent par SIGINT.
        // Sans protection, c'est tout le binaire de test qui meurt.
        let mut l = ProcessLauncher;
        let r = l.run(&["sh".into(), "-c".into(), "kill -INT $PPID; sleep 0.2".into()]);
        assert_eq!(r, Ok(()));
    }
}
