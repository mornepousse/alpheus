//! One interface (trait) per domain; v1 implementation = existing tools
//! called through the injectable executor. A D-Bus backend will be able to
//! replace an implementation without touching `ui`.

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

/// Failure of a backend, displayed as-is in the relevant tab.
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
    /// Display rollback applied best-effort: outputs whose step failed,
    /// with their error, and outputs left on because the others could not
    /// be turned back on (their deactivation was never attempted).
    Incomplete {
        failures: Vec<(String, BackendError)>,
        kept: Vec<String>,
    },
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Missing { tool } => write!(f, "{tool} not found"),
            BackendError::Timeout { tool } => write!(f, "{tool} is not responding"),
            BackendError::Unparsable { tool, detail } => write!(f, "unreadable output from {tool}: {detail}"),
            BackendError::Incomplete { failures, kept } => {
                let list: Vec<String> = failures
                    .iter()
                    .map(|(out, e)| format!("{out} ({e})"))
                    .chain(kept.iter().map(|out| format!("{out} (left on: incomplete rollback)")))
                    .collect();
                write!(f, "incomplete rollback: {}", list.join(", "))
            }
            BackendError::Failed { tool, code, stderr } => {
                let first = stderr.lines().next().unwrap_or("").trim();
                match code {
                    Some(c) => write!(f, "{tool} failed (code {c})")?,
                    None => write!(f, "{tool} failed")?,
                }
                if !first.is_empty() {
                    write!(f, ": {first}")?;
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

/// Runs a command; anything short of a success becomes a BackendError.
pub(crate) fn run_ok(exec: &dyn Executor, prog: &str, args: &[&str]) -> BResult<String> {
    match exec.run(prog, args) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(BackendError::Failed { tool: prog.to_string(), code: out.code, stderr: out.stderr }),
        Err(e) => Err(exec_error(prog, e)),
    }
}

/// Launches without waiting (lock screen).
pub(crate) fn spawn_ok(exec: &dyn Executor, prog: &str, args: &[&str]) -> BResult<()> {
    exec.spawn(prog, args).map_err(|e| exec_error(prog, e))
}

/// All backends, shared between the loop and the worker thread.
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
    /// v1 backends (command-line tools). `runtime_dir` = $XDG_RUNTIME_DIR.
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
    fn messages_in_english() {
        let m = |e: BackendError| e.to_string();
        assert_eq!(m(BackendError::Missing { tool: "wpctl".into() }), "wpctl not found");
        assert_eq!(m(BackendError::Timeout { tool: "bluetoothctl".into() }), "bluetoothctl is not responding");
        assert_eq!(
            m(BackendError::Failed { tool: "nmcli".into(), code: Some(8), stderr: "Error: boom\nmore".into() }),
            "nmcli failed (code 8): Error: boom"
        );
        assert_eq!(m(BackendError::Failed { tool: "nmcli".into(), code: Some(1), stderr: "".into() }), "nmcli failed (code 1)");
        assert_eq!(m(BackendError::Failed { tool: "nmcli".into(), code: None, stderr: "".into() }), "nmcli failed");
        assert_eq!(m(unparsable("wpctl", "missing section")), "unreadable output from wpctl: missing section");
    }

    #[test]
    fn run_ok_converts_failures() {
        let f = FakeExecutor::new();
        f.ok("a ok", "output");
        f.respond("a ko", Ok(Output::fail(2, "bad")));
        f.respond("a slow", Err(ExecError::Timeout));
        f.respond("a io", Err(ExecError::Io("broken pipe".into())));
        assert_eq!(run_ok(&f, "a", &["ok"]), Ok("output".into()));
        assert_eq!(run_ok(&f, "a", &["ko"]), Err(BackendError::Failed { tool: "a".into(), code: Some(2), stderr: "bad".into() }));
        assert_eq!(run_ok(&f, "a", &["slow"]), Err(BackendError::Timeout { tool: "a".into() }));
        assert_eq!(run_ok(&f, "a", &["io"]), Err(BackendError::Failed { tool: "a".into(), code: None, stderr: "broken pipe".into() }));
        assert_eq!(run_ok(&f, "missing", &[]), Err(BackendError::Missing { tool: "missing".into() }));
        assert_eq!(spawn_ok(&f, "missing", &[]), Err(BackendError::Missing { tool: "missing".into() }));
    }

    #[test]
    fn system_wires_each_domain_to_its_tool() {
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
