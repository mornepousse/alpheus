//! Une interface (trait) par domaine ; implémentation v1 = outils existants
//! appelés via l'exécuteur injectable. Un backend D-Bus pourra remplacer une
//! implémentation sans toucher à `ui`.

use crate::exec::{ExecError, Executor};
use std::fmt;

pub mod sound;

pub use sound::{AudioNode, AudioStatus, NodeRef, Sound, Volume, WpctlSound};

/// Échec d'un backend, affiché tel quel dans l'onglet concerné.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    Missing { tool: String },
    Failed { tool: String, code: Option<i32>, stderr: String },
    Unparsable { tool: String, detail: String },
    Timeout { tool: String },
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Missing { tool } => write!(f, "{tool} introuvable"),
            BackendError::Timeout { tool } => write!(f, "{tool} ne répond pas"),
            BackendError::Unparsable { tool, detail } => write!(f, "sortie de {tool} illisible : {detail}"),
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
}
