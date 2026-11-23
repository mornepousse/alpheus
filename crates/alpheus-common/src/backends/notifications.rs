//! Do not disturb: an external script (for example shell-ctl), which routes to the notification daemon in use
//! depending on the desktop stack. alpheus never talks directly to the daemon.
//!
//! Never:
//! - call the service behind the tool: the tool is the interface, and a missing one leaves the row
//!   or the tab empty rather than failing;
//! - run anything in a test: the fake executor answers (AGENTS.md).
//!
//! See: docs/features/control-center.md, docs/install.md (the tools), docs/compositors.md (what
//! `wm-set` and `shell-ctl` stand for).

use super::{BResult, run_ok, unparsable};
use crate::exec::Executor;
use std::sync::Arc;

const TOOL: &str = "shell-ctl";

pub trait Notifications: Send + Sync {
    fn dnd(&self) -> BResult<bool>;
    fn set_dnd(&self, on: bool) -> BResult<()>;
}

pub struct ShellCtl {
    exec: Arc<dyn Executor>,
}

impl ShellCtl {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        ShellCtl { exec }
    }
}

impl Notifications for ShellCtl {
    fn dnd(&self) -> BResult<bool> {
        match run_ok(&*self.exec, TOOL, &["dnd-status"])?.trim() {
            "on" => Ok(true),
            "off" => Ok(false),
            other => Err(unparsable(TOOL, format!("state `{other}`"))),
        }
    }

    fn set_dnd(&self, on: bool) -> BResult<()> {
        run_ok(&*self.exec, TOOL, &[if on { "dnd-on" } else { "dnd-off" }]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::BackendError;
    use crate::exec::fake::FakeExecutor;

    #[test]
    fn reads_the_state() {
        let f = Arc::new(FakeExecutor::new());
        let n = ShellCtl::new(f.clone());
        f.ok("shell-ctl dnd-status", "on\n");
        assert_eq!(n.dnd(), Ok(true));
        f.ok("shell-ctl dnd-status", "off\n");
        assert_eq!(n.dnd(), Ok(false));
        f.ok("shell-ctl dnd-status", "maybe\n");
        assert!(matches!(n.dnd(), Err(BackendError::Unparsable { .. })));
    }

    #[test]
    fn toggles() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("shell-ctl dnd-on", "");
        f.ok("shell-ctl dnd-off", "");
        let n = ShellCtl::new(f.clone());
        n.set_dnd(true).unwrap();
        n.set_dnd(false).unwrap();
        assert_eq!(f.calls(), vec!["shell-ctl dnd-on", "shell-ctl dnd-off"]);
    }
}
