//! Internal screen brightness: brightnessctl (backlight class).
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

const TOOL: &str = "brightnessctl";

/// Floor: 0% turns off the backlight, which amounts to turning off the
/// screen, and alpheus never does that.
pub const MIN_PERCENT: u8 = 1;

pub trait Brightness: Send + Sync {
    fn get(&self) -> BResult<u8>;
    fn set(&self, percent: u8) -> BResult<()>;
}

pub struct BrightnessCtl {
    exec: Arc<dyn Executor>,
}

impl BrightnessCtl {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        BrightnessCtl { exec }
    }
}

impl Brightness for BrightnessCtl {
    fn get(&self) -> BResult<u8> {
        parse_machine(&run_ok(&*self.exec, TOOL, &["-m", "--class=backlight", "info"])?)
    }

    fn set(&self, percent: u8) -> BResult<()> {
        let p = percent.clamp(MIN_PERCENT, 100);
        run_ok(&*self.exec, TOOL, &["--class=backlight", "set", &format!("{p}%")]).map(|_| ())
    }
}

/// `intel_backlight,backlight,3879,20%,19393`: current and maximum.
pub fn parse_machine(out: &str) -> BResult<u8> {
    let line = out.lines().next().ok_or_else(|| unparsable(TOOL, "empty output"))?;
    let f: Vec<&str> = line.split(',').collect();
    if f.len() < 5 {
        return Err(unparsable(TOOL, format!("{} fields instead of 5", f.len())));
    }
    let cur: u64 = f[2].trim().parse().map_err(|_| unparsable(TOOL, format!("value `{}`", f[2])))?;
    let max: u64 = f[4].trim().parse().map_err(|_| unparsable(TOOL, format!("maximum `{}`", f[4])))?;
    if max == 0 {
        return Err(unparsable(TOOL, "zero maximum"));
    }
    Ok(((cur * 100 + max / 2) / max).min(100) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::fake::FakeExecutor;

    #[test]
    fn parses_the_real_output() {
        assert_eq!(parse_machine("intel_backlight,backlight,3879,20%,19393\n").unwrap(), 20);
        assert_eq!(parse_machine("x,backlight,19393,100%,19393\n").unwrap(), 100);
    }

    #[test]
    fn unreadable_output() {
        assert!(parse_machine("").is_err());
        assert!(parse_machine("a,b").is_err());
        assert!(parse_machine("x,backlight,5,0%,0").is_err()); // zero maximum
    }

    #[test]
    fn exact_commands() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("brightnessctl -m --class=backlight info", "intel_backlight,backlight,3879,20%,19393\n");
        f.ok("brightnessctl --class=backlight set 35%", "");
        let b = BrightnessCtl::new(f.clone());
        assert_eq!(b.get().unwrap(), 20);
        b.set(35).unwrap();
    }

    #[test]
    fn set_is_clamped_to_1_percent() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("brightnessctl --class=backlight set 1%", "");
        f.ok("brightnessctl --class=backlight set 100%", "");
        let b = BrightnessCtl::new(f.clone());
        b.set(0).unwrap();
        b.set(250).unwrap();
        assert_eq!(f.calls(), vec!["brightnessctl --class=backlight set 1%", "brightnessctl --class=backlight set 100%"]);
    }
}
