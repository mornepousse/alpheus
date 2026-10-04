//! Une interface (trait) par domaine ; implémentation v1 = outils existants
//! appelés via l'exécuteur injectable. Un backend D-Bus pourra remplacer une
//! implémentation sans toucher à `ui`.

use crate::exec::{ExecError, Executor};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

pub mod bluetooth;
pub mod brightness;
pub mod displays;
pub mod energy;
pub mod network;
pub mod notifications;
pub mod power;
pub mod sound;

pub use bluetooth::{Bluetooth, BluetoothCtl, BtDevice, BtStatus};
pub use brightness::{Brightness, BrightnessCtl};
pub use displays::{Displays, WmSetDisplays};
pub use energy::{Energy, PowerProfiles, Profiles};
pub use network::{Connection, NetStatus, Network, NmCli};
pub use notifications::{Notifications, ShellCtl};
pub use power::{Power, PowerAction, SessionPower};
pub use sound::{AudioNode, AudioStatus, NodeRef, Sound, Volume, WpctlSound};

/// Échec d'un backend, affiché tel quel dans l'onglet concerné.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    Missing {
        tool: String,
    },
    Failed {
        tool: String,
        code: Option<i32>,
        stderr: String,
    },
    Unparsable {
        tool: String,
        detail: String,
    },
    Timeout {
        tool: String,
    },
    /// Retour d'écrans appliqué au mieux : sorties dont l'étape a échoué,
    /// avec leur erreur, et sorties laissées allumées faute d'avoir pu
    /// rallumer les autres (leur désactivation n'a pas été tentée).
    Incomplete {
        failures: Vec<(String, BackendError)>,
        kept: Vec<String>,
    },
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Missing { tool } => write!(f, "{tool} introuvable"),
            BackendError::Timeout { tool } => write!(f, "{tool} ne répond pas"),
            BackendError::Unparsable { tool, detail } => write!(f, "sortie de {tool} illisible : {detail}"),
            BackendError::Incomplete { failures, kept } => {
                let list: Vec<String> = failures
                    .iter()
                    .map(|(out, e)| format!("{out} ({e})"))
                    .chain(kept.iter().map(|out| format!("{out} (gardé allumé : retour incomplet)")))
                    .collect();
                write!(f, "retour incomplet : {}", list.join(", "))
            }
            BackendError::Failed { tool, code, stderr } => {
                let first = stderr.lines().next().unwrap_or("").trim();
                match code {
                    Some(c) => write!(f, "{tool} a échoué (code {c})")?,
                    None => write!(f, "{tool} a échoué")?,
                }
                if !first.is_empty() {
                    write!(f, " : {first}")?;
                }
                Ok(())
            }
        }
    }
}

pub type BResult<T> = Result<T, BackendError>;

pub(crate) fn unparsable(tool: &str, detail: impl Into<String>) -> BackendError {
    BackendError::Unparsable { tool: tool.to_string(), detail: detail.into() }
}

fn exec_error(tool: &str, e: ExecError) -> BackendError {
    match e {
        ExecError::NotFound => BackendError::Missing { tool: tool.to_string() },
        ExecError::Timeout => BackendError::Timeout { tool: tool.to_string() },
        ExecError::Io(msg) => BackendError::Failed { tool: tool.to_string(), code: None, stderr: msg },
    }
}

/// Lance une commande ; tout ce qui n'est pas un succès devient BackendError.
pub(crate) fn run_ok(exec: &dyn Executor, prog: &str, args: &[&str]) -> BResult<String> {
    match exec.run(prog, args) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(BackendError::Failed { tool: prog.to_string(), code: out.code, stderr: out.stderr }),
        Err(e) => Err(exec_error(prog, e)),
    }
}

/// Lance sans attendre (écran de verrouillage).
pub(crate) fn spawn_ok(exec: &dyn Executor, prog: &str, args: &[&str]) -> BResult<()> {
    exec.spawn(prog, args).map_err(|e| exec_error(prog, e))
}

/// Tous les backends, partagés entre la boucle et le fil de travail.
#[derive(Clone)]
pub struct Backends {
    pub sound: Arc<dyn Sound>,
    pub brightness: Arc<dyn Brightness>,
    pub network: Arc<dyn Network>,
    pub bluetooth: Arc<dyn Bluetooth>,
    pub energy: Arc<dyn Energy>,
    pub notifications: Arc<dyn Notifications>,
    pub power: Arc<dyn Power>,
    pub displays: Arc<dyn Displays>,
}

impl Backends {
    /// Backends v1 (outils en ligne de commande). `runtime_dir` = $XDG_RUNTIME_DIR.
    pub fn system(exec: Arc<dyn Executor>, runtime_dir: PathBuf) -> Self {
        Backends {
            sound: Arc::new(WpctlSound::new(exec.clone())),
            brightness: Arc::new(BrightnessCtl::new(exec.clone())),
            network: Arc::new(NmCli::new(exec.clone())),
            bluetooth: Arc::new(BluetoothCtl::new(exec.clone())),
            energy: Arc::new(PowerProfiles::new(exec.clone(), runtime_dir)),
            notifications: Arc::new(ShellCtl::new(exec.clone())),
            power: Arc::new(SessionPower::new(exec.clone())),
            displays: Arc::new(WmSetDisplays::new(exec)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Output;
    use crate::exec::fake::FakeExecutor;

    #[test]
    fn messages_en_francais() {
        let m = |e: BackendError| e.to_string();
        assert_eq!(m(BackendError::Missing { tool: "wpctl".into() }), "wpctl introuvable");
        assert_eq!(m(BackendError::Timeout { tool: "bluetoothctl".into() }), "bluetoothctl ne répond pas");
        assert_eq!(
            m(BackendError::Failed { tool: "nmcli".into(), code: Some(8), stderr: "Error: boum\nsuite".into() }),
            "nmcli a échoué (code 8) : Error: boum"
        );
        assert_eq!(m(BackendError::Failed { tool: "nmcli".into(), code: Some(1), stderr: "".into() }), "nmcli a échoué (code 1)");
        assert_eq!(m(BackendError::Failed { tool: "nmcli".into(), code: None, stderr: "".into() }), "nmcli a échoué");
        assert_eq!(m(unparsable("wpctl", "section absente")), "sortie de wpctl illisible : section absente");
    }

    #[test]
    fn run_ok_convertit_les_echecs() {
        let f = FakeExecutor::new();
        f.ok("a ok", "sortie");
        f.respond("a ko", Ok(Output::fail(2, "mal")));
        f.respond("a lent", Err(ExecError::Timeout));
        f.respond("a io", Err(ExecError::Io("tube cassé".into())));
        assert_eq!(run_ok(&f, "a", &["ok"]), Ok("sortie".into()));
        assert_eq!(run_ok(&f, "a", &["ko"]), Err(BackendError::Failed { tool: "a".into(), code: Some(2), stderr: "mal".into() }));
        assert_eq!(run_ok(&f, "a", &["lent"]), Err(BackendError::Timeout { tool: "a".into() }));
        assert_eq!(run_ok(&f, "a", &["io"]), Err(BackendError::Failed { tool: "a".into(), code: None, stderr: "tube cassé".into() }));
        assert_eq!(run_ok(&f, "absent", &[]), Err(BackendError::Missing { tool: "absent".into() }));
        assert_eq!(spawn_ok(&f, "absent", &[]), Err(BackendError::Missing { tool: "absent".into() }));
    }

    #[test]
    fn system_branche_chaque_domaine_sur_son_outil() {
        let f = std::sync::Arc::new(FakeExecutor::new());
        let b = Backends::system(f.clone(), std::env::temp_dir());
        let _ = b.sound.volume();
        let _ = b.brightness.get();
        let _ = b.network.status();
        let _ = b.bluetooth.status();
        let _ = b.energy.profiles();
        let _ = b.notifications.dnd();
        let _ = b.power.perform(PowerAction::Suspend);
        let _ = b.displays.outputs();
        let progs: Vec<String> = f.calls().iter().map(|c| c.split(' ').next().unwrap().to_string()).collect();
        assert_eq!(progs, vec!["wpctl", "brightnessctl", "nmcli", "bluetoothctl", "powerprofilesctl", "shell-ctl", "systemctl", "wm-set"]);
    }
}
