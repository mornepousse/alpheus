//! Énergie : profil (powerprofilesctl) et « Garder éveillé » (tools-menu).

use super::{run_ok, unparsable, BResult};
use crate::exec::Executor;
use std::path::PathBuf;
use std::sync::Arc;

const TOOL: &str = "powerprofilesctl";

/// Drapeau posé par tools-menu dans $XDG_RUNTIME_DIR quand « Garder éveillé » est actif.
pub const KEEP_AWAKE_FLAG: &str = "garder-eveille";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profiles {
    pub active: String,
    /// Dans l'ordre de powerprofilesctl (performance, balanced, power-saver).
    pub available: Vec<String>,
}

pub trait Energy: Send + Sync {
    fn profiles(&self) -> BResult<Profiles>;
    fn set_profile(&self, name: &str) -> BResult<()>;
    fn keep_awake(&self) -> BResult<bool>;
    /// Bascule via tools-menu : la logique (hypridle, Mode gaming) n'est pas dupliquée ici.
    fn toggle_keep_awake(&self) -> BResult<()>;
}

pub struct PowerProfiles {
    exec: Arc<dyn Executor>,
    runtime_dir: PathBuf,
}

impl PowerProfiles {
    pub fn new(exec: Arc<dyn Executor>, runtime_dir: PathBuf) -> Self {
        PowerProfiles { exec, runtime_dir }
    }
}

impl Energy for PowerProfiles {
    fn profiles(&self) -> BResult<Profiles> {
        parse_list(&run_ok(&*self.exec, TOOL, &["list"])?)
    }

    fn set_profile(&self, name: &str) -> BResult<()> {
        run_ok(&*self.exec, TOOL, &["set", name]).map(|_| ())
    }

    fn keep_awake(&self) -> BResult<bool> {
        Ok(self.runtime_dir.join(KEEP_AWAKE_FLAG).exists())
    }

    fn toggle_keep_awake(&self) -> BResult<()> {
        run_ok(&*self.exec, "tools-menu", &["Garder éveillé"]).map(|_| ())
    }
}

/// Lignes de profil : `* nom:` (actif) ou `  nom:` ; les détails sont indentés de 4.
pub fn parse_list(out: &str) -> BResult<Profiles> {
    let mut active = None;
    let mut available = Vec::new();
    for line in out.lines() {
        let (star, rest) = if let Some(r) = line.strip_prefix("* ") {
            (true, r)
        } else if let Some(r) = line.strip_prefix("  ") {
            (false, r)
        } else {
            continue;
        };
        if rest.starts_with(char::is_whitespace) {
            continue; // détail indenté de 4 (CpuDriver…)
        }
        let Some(name) = rest.trim_end().strip_suffix(':') else { continue };
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        if star {
            active = Some(name.to_string());
        }
        available.push(name.to_string());
    }
    match active {
        Some(active) => Ok(Profiles { active, available }),
        None => Err(unparsable(TOOL, "aucun profil actif")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::fake::FakeExecutor;

    const LIST: &str = include_str!("../../tests/fixtures/powerprofilesctl-list.txt");

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn analyse_la_liste_reelle() {
        let p = parse_list(LIST).unwrap();
        assert_eq!(p.active, "balanced");
        assert_eq!(p.available, vec!["performance", "balanced", "power-saver"]);
    }

    #[test]
    fn liste_sans_profil_actif() {
        assert!(parse_list("").is_err());
        assert!(parse_list("  performance:\n").is_err());
    }

    #[test]
    fn detail_sans_valeur_ignore() {
        // Un détail vide (« PlatformDriver:\t ») se termine par « : » une fois rogné.
        let p = parse_list("* balanced:\n    PlatformDriver:\t\n  power-saver:\n").unwrap();
        assert_eq!(p.available, vec!["balanced", "power-saver"]);
    }

    #[test]
    fn commandes_exactes() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("powerprofilesctl list", LIST);
        f.ok("powerprofilesctl set power-saver", "");
        f.ok("tools-menu Garder éveillé", "");
        let dir = temp_dir("cmd");
        let e = PowerProfiles::new(f.clone(), dir.clone());
        assert_eq!(e.profiles().unwrap().active, "balanced");
        e.set_profile("power-saver").unwrap();
        e.toggle_keep_awake().unwrap();
        assert_eq!(f.calls().last().unwrap(), "tools-menu Garder éveillé");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn garder_eveille_lit_le_drapeau() {
        let dir = temp_dir("eveil");
        let e = PowerProfiles::new(Arc::new(FakeExecutor::new()), dir.clone());
        assert_eq!(e.keep_awake(), Ok(false));
        std::fs::write(dir.join(KEEP_AWAKE_FLAG), "").unwrap();
        assert_eq!(e.keep_awake(), Ok(true));
        let _ = std::fs::remove_dir_all(dir);
    }
}
