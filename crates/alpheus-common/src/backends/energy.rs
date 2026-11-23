//! Energy: profile (powerprofilesctl) and "Keep awake" (tools-menu).

use super::{BResult, run_ok, unparsable};
use crate::exec::Executor;
use std::path::PathBuf;
use std::sync::Arc;

const TOOL: &str = "powerprofilesctl";

/// Flag dropped by tools-menu in $XDG_RUNTIME_DIR while "Keep awake" is
/// active. Filename kept as-is: it is the contract with the external
/// tools-menu script, not translated here.
pub const KEEP_AWAKE_FLAG: &str = "garder-eveille";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profiles {
    pub active: String,
    /// In powerprofilesctl's order (performance, balanced, power-saver).
    pub available: Vec<String>,
}

pub trait Energy: Send + Sync {
    fn profiles(&self) -> BResult<Profiles>;
    fn set_profile(&self, name: &str) -> BResult<()>;
    fn keep_awake(&self) -> BResult<bool>;
    /// Toggles via tools-menu: the logic (hypridle, gaming mode) is not
    /// duplicated here.
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
        // Argument kept as-is: the exact subcommand name expected by the
        // external tools-menu script, not translated here.
        run_ok(&*self.exec, "tools-menu", &["Garder éveillé"]).map(|_| ())
    }
}

/// Profile lines: `* name:` (active) or `  name:`; details are indented by 4.
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
            continue; // detail indented by 4 (CpuDriver…)
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
        None => Err(unparsable(TOOL, "no active profile")),
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
    fn parses_the_real_list() {
        let p = parse_list(LIST).unwrap();
        assert_eq!(p.active, "balanced");
        assert_eq!(p.available, vec!["performance", "balanced", "power-saver"]);
    }

    #[test]
    fn list_without_an_active_profile() {
        assert!(parse_list("").is_err());
        assert!(parse_list("  performance:\n").is_err());
    }

    #[test]
    fn detail_without_a_value_ignored() {
        // An empty detail ("PlatformDriver:\t") ends with ":" once trimmed.
        let p = parse_list("* balanced:\n    PlatformDriver:\t\n  power-saver:\n").unwrap();
        assert_eq!(p.available, vec!["balanced", "power-saver"]);
    }

    #[test]
    fn exact_commands() {
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
    fn keep_awake_reads_the_flag() {
        let dir = temp_dir("awake");
        let e = PowerProfiles::new(Arc::new(FakeExecutor::new()), dir.clone());
        assert_eq!(e.keep_awake(), Ok(false));
        std::fs::write(dir.join(KEEP_AWAKE_FLAG), "").unwrap();
        assert_eq!(e.keep_awake(), Ok(true));
        let _ = std::fs::remove_dir_all(dir);
    }
}
