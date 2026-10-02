//! Ne pas déranger : shell-ctl (nixos-config), qui route vers mako ou DMS
//! selon la pile de bureau. alpheus ne parle jamais directement au démon.

use super::{run_ok, unparsable, BResult};
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
            other => Err(unparsable(TOOL, format!("état « {other} »"))),
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
    fn lit_l_etat() {
        let f = Arc::new(FakeExecutor::new());
        let n = ShellCtl::new(f.clone());
        f.ok("shell-ctl dnd-status", "on\n");
        assert_eq!(n.dnd(), Ok(true));
        f.ok("shell-ctl dnd-status", "off\n");
        assert_eq!(n.dnd(), Ok(false));
        f.ok("shell-ctl dnd-status", "peut-être\n");
        assert!(matches!(n.dnd(), Err(BackendError::Unparsable { .. })));
    }

    #[test]
    fn bascule() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("shell-ctl dnd-on", "");
        f.ok("shell-ctl dnd-off", "");
        let n = ShellCtl::new(f.clone());
        n.set_dnd(true).unwrap();
        n.set_dnd(false).unwrap();
        assert_eq!(f.calls(), vec!["shell-ctl dnd-on", "shell-ctl dnd-off"]);
    }
}
