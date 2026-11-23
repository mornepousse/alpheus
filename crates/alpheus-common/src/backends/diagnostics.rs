//! Diagnostics: systemd's own tools (`systemctl`, `journalctl`, `coredumpctl`, `systemd-analyze`)
//! read through `Executor::capture` (a deadline, a byte cap and a line cap, the process group
//! killed after), plus the bug report's clipboard program. Only the Diag tab's worker calls these.

use super::{BResult, BackendError, exec_error, unparsable};
use crate::config::Check;
use crate::diagnostics::analyze::{self, Blame};
use crate::diagnostics::crashes::{self, Crash};
use crate::diagnostics::env;
use crate::diagnostics::journal::{self, Entry};
use crate::diagnostics::names::valid_unit;
use crate::diagnostics::query::{JournalQuery, Scope, journal_args};
use crate::diagnostics::report::{self, HostFacts, ReportCrash, ReportError, ReportInputs, ReportLoop, ReportUnit};
use crate::diagnostics::show::{self, Restarts};
use crate::diagnostics::units::{self, FailedUnit};
use crate::exec::{CaptureEnd, CaptureLimits, ExecError, Executor, MAX_INPUT};
use crate::launcher_ipc::{Reply, Request, exchange_waiting};
use crate::text::clean_line;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 3 s, 256 KiB, 2,000 lines: the overview's reads.
const SMALL: CaptureLimits = CaptureLimits { deadline: Duration::from_secs(3), max_bytes: 256 << 10, max_lines: 2000 };
/// 3 s, 512 KiB, 20,000 lines: `show` of every service.
const SHOW: CaptureLimits = CaptureLimits { deadline: Duration::from_secs(3), max_bytes: 512 << 10, max_lines: 20_000 };
/// 5 s, 256 KiB, 2,000 lines: the boot report.
const BOOT: CaptureLimits = CaptureLimits { deadline: Duration::from_secs(5), max_bytes: 256 << 10, max_lines: 2000 };
/// The journal's first read: 5 s, 4 MiB (lines come from the query).
const JOURNAL_DEADLINE: Duration = Duration::from_secs(5);
const JOURNAL_BYTES: usize = 4 << 20;
/// A follow read: 2 s, 1 MiB, 1,000 entries.
const FOLLOW: CaptureLimits = CaptureLimits { deadline: Duration::from_secs(2), max_bytes: 1 << 20, max_lines: 1001 };
const FOLLOW_ENTRIES: usize = 1000;
/// Most service units a picker lists per manager.
const MAX_SERVICES: usize = 512;
/// Most characters of a check's detail.
const CHECK_DETAIL: usize = 120;

/// What a unit action does (the only two the tab offers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitAction {
    Restart,
    ResetFailed,
}

/// What a journal read brought.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRead {
    pub entries: Vec<Entry>,
    /// More was there than the read was allowed to take.
    pub capped: bool,
    /// The deadline passed first: what came before it is shown.
    pub timed_out: bool,
}

/// A declared check's outcome: exit 0 is fine, else the first line of what it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub ok: bool,
    pub detail: String,
}

pub trait Diagnostics: Send + Sync {
    fn failed(&self, scope: Scope) -> BResult<Vec<FailedUnit>>;
    /// Services restarted at least `threshold` times, highest first.
    fn restarts(&self, scope: Scope, threshold: u32) -> BResult<Vec<Restarts>>;
    /// The loaded service units' names, sorted, at most 512 (the journal picker).
    fn service_units(&self, scope: Scope) -> BResult<Vec<String>>;
    fn crashes(&self) -> BResult<Vec<Crash>>;
    /// The first line of `systemd-analyze time`.
    fn boot_time(&self) -> BResult<String>;
    fn blame(&self, max: usize) -> BResult<Vec<Blame>>;
    fn critical_chain(&self, max_lines: usize) -> BResult<Vec<String>>;
    /// The user environment's `names`, nothing else kept.
    fn session_env(&self, names: &[&str]) -> BResult<Vec<(String, String)>>;
    fn systemd_version(&self) -> BResult<String>;
    fn journal(&self, query: &JournalQuery) -> BResult<JournalRead>;
    /// A user unit's restart (queued, not waited for) or reset-failed.
    fn user_action(&self, action: UnitAction, unit: &str) -> BResult<()>;
    /// Puts `text` on the standard input of the copy program `argv`.
    fn copy(&self, argv: &[String], text: &str) -> BResult<()>;
    fn check(&self, check: &Check) -> CheckResult;
    /// Whether this user can open the system journal's files (a member of `systemd-journal`,
    /// `adm` or `wheel`); false means `journalctl` shows only that user's own messages.
    fn journal_readable(&self) -> bool;
    /// The bug report, built and redacted: what shows is what is copied.
    fn report(&self, compositor: &str, restart_loop: u32) -> String;
}

pub struct SystemdTools {
    exec: Arc<dyn Executor>,
    /// Where the journal's per-machine directories are.
    journal_dirs: Vec<PathBuf>,
    /// What the report says of this machine; read when a report is made unless fixed here.
    facts: Option<HostFacts>,
}

impl SystemdTools {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        SystemdTools { exec, journal_dirs: vec![PathBuf::from("/var/log/journal"), PathBuf::from("/run/log/journal")], facts: None }
    }

    /// Tools that read nothing of the machine they run on: the journal under `journal_dirs`, the
    /// names and ids of `facts` (tests).
    #[cfg(any(test, feature = "testing"))]
    pub fn for_tests(exec: Arc<dyn Executor>, journal_dirs: Vec<PathBuf>, facts: HostFacts) -> Self {
        SystemdTools { exec, journal_dirs, facts: Some(facts) }
    }

    /// The same, with the machine's facts given (tests).
    pub fn with_facts(mut self, facts: HostFacts) -> Self {
        self.facts = Some(facts);
        self
    }

    /// The same, looking for the journal in `dirs` (tests).
    pub fn with_journal_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.journal_dirs = dirs;
        self
    }

    /// The standard output of a command that ended by itself with code 0 (and, when
    /// `none_is_exit_1`, also with code 1 and nothing printed: `coredumpctl` with no crash). A
    /// deadline or a cap passed gives an error: half a JSON document is no answer.
    fn read(&self, prog: &str, args: &[&str], limits: CaptureLimits, none_is_exit_1: bool) -> BResult<Vec<u8>> {
        let got = self.exec.capture(prog, args, limits).map_err(|e| exec_error(prog, e))?;
        match got.end {
            CaptureEnd::Exited(Some(0)) => Ok(got.stdout),
            CaptureEnd::Exited(Some(1)) if none_is_exit_1 && got.stdout.is_empty() => Ok(Vec::new()),
            CaptureEnd::Exited(code) => Err(BackendError::Failed { tool: prog.to_string(), code, stderr: String::new() }),
            CaptureEnd::TimedOut => Err(BackendError::Timeout { tool: prog.to_string() }),
            CaptureEnd::Capped => Err(unparsable(prog, "output too large")),
        }
    }

    fn systemctl(&self, scope: Scope, args: &[&str], limits: CaptureLimits) -> BResult<Vec<u8>> {
        let mut all: Vec<&str> = Vec::new();
        if scope == Scope::User {
            all.push("--user");
        }
        all.extend_from_slice(args);
        self.read("systemctl", &all, limits, false)
    }
}

impl Diagnostics for SystemdTools {
    fn failed(&self, scope: Scope) -> BResult<Vec<FailedUnit>> {
        let out = self.systemctl(scope, &["list-units", "--failed", "--output=json", "--no-pager"], SMALL)?;
        units::parse_failed(&out).map_err(|e| unparsable("systemctl", e))
    }

    fn restarts(&self, scope: Scope, threshold: u32) -> BResult<Vec<Restarts>> {
        let out = self.systemctl(scope, &["show", "--property=Id,NRestarts,ActiveState,SubState", "--no-pager", "*.service"], SHOW)?;
        Ok(show::parse_restarts(&out, threshold))
    }

    fn service_units(&self, scope: Scope) -> BResult<Vec<String>> {
        let out = self.systemctl(scope, &["list-units", "--type=service", "--all", "--output=json", "--no-pager"], SHOW)?;
        let mut names: Vec<String> = units::parse_units(&out, usize::MAX)
            .map_err(|e| unparsable("systemctl", e))?
            .into_iter()
            .filter(|u| u.load == "loaded")
            .map(|u| u.unit)
            .collect();
        names.sort();
        names.truncate(MAX_SERVICES);
        Ok(names)
    }

    fn crashes(&self) -> BResult<Vec<Crash>> {
        let args = ["list", "--json=short", "--no-pager", "--reverse", "--since=-7d", "-n", "20"];
        let out = self.read("coredumpctl", &args, SMALL, true)?;
        if out.is_empty() {
            return Ok(Vec::new());
        }
        crashes::parse_crashes(&out).map_err(|e| unparsable("coredumpctl", e))
    }

    fn boot_time(&self) -> BResult<String> {
        let out = self.read("systemd-analyze", &["time", "--no-pager"], SMALL, false)?;
        let text = String::from_utf8_lossy(&out);
        match text.lines().map(|l| clean_line(l, 200)).find(|l| !l.trim().is_empty()) {
            Some(line) => Ok(line),
            None => Err(unparsable("systemd-analyze", "no output")),
        }
    }

    fn blame(&self, max: usize) -> BResult<Vec<Blame>> {
        let out = self.read("systemd-analyze", &["blame", "--no-pager"], BOOT, false)?;
        Ok(analyze::parse_blame(&String::from_utf8_lossy(&out), max))
    }

    fn critical_chain(&self, max_lines: usize) -> BResult<Vec<String>> {
        let out = self.read("systemd-analyze", &["critical-chain", "--no-pager"], BOOT, false)?;
        Ok(String::from_utf8_lossy(&out).lines().map(|l| clean_line(l, 200)).filter(|l| !l.trim().is_empty()).take(max_lines).collect())
    }

    fn session_env(&self, names: &[&str]) -> BResult<Vec<(String, String)>> {
        let out = self.systemctl(Scope::User, &["show-environment"], SMALL)?;
        Ok(env::session_vars(&String::from_utf8_lossy(&out), names))
    }

    fn systemd_version(&self) -> BResult<String> {
        let out = self.read("systemctl", &["--version"], SMALL, false)?;
        match String::from_utf8_lossy(&out).lines().map(|l| clean_line(l, 100)).find(|l| !l.is_empty()) {
            Some(line) => Ok(line),
            None => Err(unparsable("systemctl", "no output")),
        }
    }

    fn journal(&self, query: &JournalQuery) -> BResult<JournalRead> {
        let args = journal_args(query).map_err(|e| unparsable("journalctl", e))?;
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (limits, max) = if query.after_cursor.is_some() {
            (FOLLOW, FOLLOW_ENTRIES)
        } else {
            let lines = query.lines.clamp(1, 5000) as usize;
            (CaptureLimits { deadline: JOURNAL_DEADLINE, max_bytes: JOURNAL_BYTES, max_lines: lines + 1 }, lines)
        };
        let got = self.exec.capture("journalctl", &refs, limits).map_err(|e| exec_error("journalctl", e))?;
        // A grep with no match, or a follow with nothing new, ends with code 1 and nothing printed.
        if let CaptureEnd::Exited(code) = got.end
            && code != Some(0)
            && !(code == Some(1) && got.stdout.is_empty())
        {
            return Err(BackendError::Failed { tool: "journalctl".into(), code, stderr: String::new() });
        }
        let (entries, more) = journal::parse_entries(&got.stdout, max);
        Ok(JournalRead { entries, capped: more || got.end == CaptureEnd::Capped, timed_out: got.end == CaptureEnd::TimedOut })
    }

    fn user_action(&self, action: UnitAction, unit: &str) -> BResult<()> {
        if !valid_unit(unit) {
            return Err(unparsable("systemctl", "not a unit name"));
        }
        let args: &[&str] = match action {
            UnitAction::Restart => &["--user", "--no-block", "restart", "--", unit],
            UnitAction::ResetFailed => &["--user", "reset-failed", "--", unit],
        };
        super::run_ok(&*self.exec, "systemctl", args).map(|_| ())
    }

    fn copy(&self, argv: &[String], text: &str) -> BResult<()> {
        let Some((prog, args)) = argv.split_first() else {
            return Err(BackendError::Failed { tool: "copy".into(), code: None, stderr: "no copy program configured".into() });
        };
        if text.len() > MAX_INPUT {
            return Err(BackendError::Failed { tool: prog.clone(), code: None, stderr: "the text is too large to copy".into() });
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.exec.spawn_with_input(prog, &args, &[text.as_bytes()]).map_err(|e| exec_error(prog, e))
    }

    fn journal_readable(&self) -> bool {
        system_journal_opens(&self.journal_dirs)
    }

    fn report(&self, compositor: &str, restart_loop: u32) -> String {
        let facts = self.facts.clone().unwrap_or_else(HostFacts::gather);
        let scope_name = |s: Scope| if s == Scope::User { "user" } else { "system" };
        let tag = |s: Scope, r: BResult<Vec<FailedUnit>>| r.map(|v| v.into_iter().map(|u| (s, u)).collect::<Vec<_>>());
        let failed = both(tag(Scope::System, self.failed(Scope::System)), tag(Scope::User, self.failed(Scope::User)));
        let loops_of = |s: Scope| self.restarts(s, restart_loop).map(|v| v.into_iter().map(|r| (s, r)).collect::<Vec<_>>());
        let loops = both(loops_of(Scope::System), loops_of(Scope::User));
        let crashes = self.crashes().ok();
        let errors = self.journal(&JournalQuery::report_errors(200)).ok();
        let systemd = self.systemd_version().ok().map(|l| l.strip_prefix("systemd ").unwrap_or(&l).to_string()).unwrap_or_default();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let inputs = ReportInputs {
            when: report::utc_stamp(now),
            alpheus: env!("CARGO_PKG_VERSION").to_string(),
            systemd,
            kernel: facts.kernel.clone(),
            compositor: clean_line(compositor, 20),
            os: facts.os.clone(),
            failed: failed.map(|v| {
                v.into_iter().map(|(s, u)| ReportUnit { unit: u.unit, scope: scope_name(s), description: u.description }).collect()
            }),
            loops: loops
                .map(|v| v.into_iter().map(|(s, r)| ReportLoop { unit: r.unit, scope: scope_name(s), restarts: r.restarts }).collect()),
            crashes: crashes.map(|v| {
                v.into_iter()
                    .map(|c| ReportCrash {
                        time: report::utc_month_day(c.time_us / 1_000_000),
                        exe: c.exe_name,
                        signal: crashes::signal_name(c.signal).to_string(),
                    })
                    .collect()
            }),
            errors: errors.map(|r| {
                r.entries
                    .into_iter()
                    .map(|e| ReportError { time: report::utc_clock(e.time_us / 1_000_000), ident: e.ident, message: e.message })
                    .collect()
            }),
        };
        report::build(&inputs, &facts.redactions())
    }

    fn check(&self, check: &Check) -> CheckResult {
        let Some((prog, args)) = check.run.split_first() else {
            return CheckResult { ok: false, detail: "no command".into() };
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match self.exec.run(prog, &args) {
            Ok(out) if out.success() => CheckResult { ok: true, detail: String::new() },
            Ok(out) => {
                let first = |s: &str| s.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| clean_line(l, CHECK_DETAIL));
                CheckResult { ok: false, detail: first(&out.stdout).or_else(|| first(&out.stderr)).unwrap_or_default() }
            }
            Err(ExecError::NotFound) => CheckResult { ok: false, detail: clean_line(&format!("{prog} not found"), CHECK_DETAIL) },
            Err(ExecError::Timeout) => CheckResult { ok: false, detail: clean_line(&format!("{prog} is not responding"), CHECK_DETAIL) },
            Err(ExecError::Io(e)) => CheckResult { ok: false, detail: clean_line(&e, CHECK_DETAIL) },
        }
    }
}

/// True when `system.journal` opens in one of the machine directories under `dirs`. Listing the
/// top directory proves nothing (it is world-readable); the file is what needs the group.
fn system_journal_opens(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        let Ok(machines) = std::fs::read_dir(dir) else { return false };
        machines.flatten().take(8).any(|m| std::fs::File::open(m.path().join("system.journal")).is_ok())
    })
}

/// Both managers' lists, or None: half a list would read as a clean bill in the report.
fn both<T>(a: BResult<Vec<T>>, b: BResult<Vec<T>>) -> Option<Vec<T>> {
    match (a, b) {
        (Ok(mut a), Ok(b)) => {
            a.extend(b);
            Some(a)
        }
        _ => None,
    }
}

/// What the daemon answered to a `failed-refresh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PokeAnswer {
    Done,
    /// An older daemon that does not know the request.
    Older,
    Unreachable(String),
}

/// Asks the alpheus daemon to re-read its failed-units count (after an action).
pub trait DaemonPoke: Send + Sync {
    fn failed_refresh(&self) -> PokeAnswer;
}

/// No daemon wired: always unreachable (the tests that must never touch a socket).
pub struct NoPoke;

impl DaemonPoke for NoPoke {
    fn failed_refresh(&self) -> PokeAnswer {
        PokeAnswer::Unreachable("not wired".into())
    }
}

/// `failed-refresh` on the daemon's owner-only launcher socket.
pub struct SocketPoke {
    socket: PathBuf,
}

impl SocketPoke {
    pub fn new(socket: PathBuf) -> Self {
        SocketPoke { socket }
    }
}

impl DaemonPoke for SocketPoke {
    fn failed_refresh(&self) -> PokeAnswer {
        let request = Request::FailedRefresh;
        match exchange_waiting(&self.socket, &request, request.reply_timeout()) {
            Ok(Reply::Done) => PokeAnswer::Done,
            // A daemon from before the verb refuses it as an unknown request.
            Ok(Reply::Error(e)) if e == "malformed request: unknown request \"failed-refresh\"" => PokeAnswer::Older,
            Ok(Reply::Error(e)) => PokeAnswer::Unreachable(clean_line(&e, 120)),
            Ok(_) => PokeAnswer::Unreachable("unexpected answer from the alpheus daemon".into()),
            Err(e) => PokeAnswer::Unreachable(match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => "the alpheus daemon is not running".into(),
                _ => clean_line(&e.to_string(), 120),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::query::Preset;
    use crate::exec::Captured;
    use crate::exec::fake::FakeExecutor;

    const FAILED: &str = include_str!("../../tests/fixtures/diag-failed-system.json");
    const SHOW: &str = include_str!("../../tests/fixtures/diag-show-restarts.txt");
    const JOURNAL: &str = include_str!("../../tests/fixtures/diag-journal.jsonl");

    fn done(stdout: &str) -> Captured {
        Captured { stdout: stdout.as_bytes().to_vec(), end: CaptureEnd::Exited(Some(0)) }
    }

    fn tools() -> (Arc<FakeExecutor>, SystemdTools) {
        let f = Arc::new(FakeExecutor::new());
        (f.clone(), SystemdTools::new(f))
    }

    #[test]
    fn failed_reads_both_managers() {
        let (f, t) = tools();
        f.capture_ok("systemctl list-units --failed --output=json --no-pager", done(FAILED));
        f.capture_ok("systemctl --user list-units --failed --output=json --no-pager", done("[]"));
        assert_eq!(t.failed(Scope::System).unwrap().len(), 3);
        assert_eq!(t.failed(Scope::User), Ok(vec![]));
        assert!(f.limits().iter().all(|(_, l)| *l == SMALL), "3 s, 256 KiB, 2,000 lines");
        f.capture_ok("systemctl list-units --failed --output=json --no-pager", done("nope"));
        assert!(matches!(t.failed(Scope::System), Err(BackendError::Unparsable { .. })));
    }

    #[test]
    fn restarts_and_the_picker() {
        let (f, t) = tools();
        f.capture_ok("systemctl show --property=Id,NRestarts,ActiveState,SubState --no-pager *.service", done(SHOW));
        assert_eq!(t.restarts(Scope::System, 3).unwrap()[0].unit, "example-sync.service");
        assert_eq!(f.limits()[0].1, SHOW_LIMITS_FOR_TEST);
        f.capture_ok(
            "systemctl --user list-units --type=service --all --output=json --no-pager",
            done(
                r#"[{"unit":"z.service","load":"loaded"},{"unit":"a.service","load":"loaded"},{"unit":"gone.service","load":"not-found"}]"#,
            ),
        );
        assert_eq!(t.service_units(Scope::User).unwrap(), ["a.service", "z.service"], "loaded only, sorted");
        let many: Vec<String> = (0..600).map(|i| format!(r#"{{"unit":"u{i:04}.service","load":"loaded"}}"#)).collect();
        f.capture_ok("systemctl list-units --type=service --all --output=json --no-pager", done(&format!("[{}]", many.join(","))));
        assert_eq!(t.service_units(Scope::System).unwrap().len(), 512);
    }

    const SHOW_LIMITS_FOR_TEST: CaptureLimits = CaptureLimits { deadline: Duration::from_secs(3), max_bytes: 512 << 10, max_lines: 20_000 };

    #[test]
    fn no_coredump_is_an_empty_list() {
        let (f, t) = tools();
        let line = "coredumpctl list --json=short --no-pager --reverse --since=-7d -n 20";
        f.capture_ok(line, Captured { stdout: vec![], end: CaptureEnd::Exited(Some(1)) });
        assert_eq!(t.crashes(), Ok(vec![]));
        f.capture_ok(line, Captured { stdout: b"boom".to_vec(), end: CaptureEnd::Exited(Some(1)) });
        assert!(matches!(t.crashes(), Err(BackendError::Failed { code: Some(1), .. })), "exit 1 with output is a failure");
        f.capture_ok(line, done(include_str!("../../tests/fixtures/diag-coredumps.json")));
        assert_eq!(t.crashes().unwrap().len(), 3);
    }

    #[test]
    fn a_missing_tool_is_missing_and_a_cut_read_is_an_error() {
        let (f, t) = tools();
        assert_eq!(t.crashes(), Err(BackendError::Missing { tool: "coredumpctl".into() }));
        assert_eq!(t.boot_time(), Err(BackendError::Missing { tool: "systemd-analyze".into() }));
        f.capture_ok(
            "systemctl list-units --failed --output=json --no-pager",
            Captured { stdout: b"[{".to_vec(), end: CaptureEnd::TimedOut },
        );
        assert_eq!(t.failed(Scope::System), Err(BackendError::Timeout { tool: "systemctl".into() }));
        f.capture_ok(
            "systemctl --user list-units --failed --output=json --no-pager",
            Captured { stdout: b"[{".to_vec(), end: CaptureEnd::Capped },
        );
        assert!(matches!(t.failed(Scope::User), Err(BackendError::Unparsable { .. })));
    }

    #[test]
    fn boot_report_reads() {
        let (f, t) = tools();
        f.capture_ok(
            "systemd-analyze time --no-pager",
            done("\nStartup finished in 3.2s (firmware) + 8.1s (userspace) = 21.3s\ngraphical.target reached after 7.9s\n"),
        );
        assert_eq!(t.boot_time().unwrap(), "Startup finished in 3.2s (firmware) + 8.1s (userspace) = 21.3s");
        f.capture_ok("systemd-analyze blame --no-pager", done(include_str!("../../tests/fixtures/diag-blame.txt")));
        assert_eq!(t.blame(2).unwrap().len(), 2);
        f.capture_ok("systemd-analyze critical-chain --no-pager", done("The time when unit became active is printed after the \"@\" character.\n\ngraphical.target @7.9s\n└─multi-user.target @7.9s\n"));
        assert_eq!(t.critical_chain(2).unwrap().len(), 2, "empty lines skipped, bounded");
        assert!(
            f.limits()
                .iter()
                .filter(|(l, _)| l.contains("blame") || l.contains("critical"))
                .all(|(_, l)| l.deadline == Duration::from_secs(5))
        );
    }

    #[test]
    fn session_env_keeps_only_the_names_asked() {
        let (f, t) = tools();
        f.capture_ok("systemctl --user show-environment", done(include_str!("../../tests/fixtures/diag-show-environment.txt")));
        let vars = t.session_env(&["WAYLAND_DISPLAY"]).unwrap();
        assert_eq!(vars, [("WAYLAND_DISPLAY".to_string(), "wayland-1".to_string())]);
        f.capture_ok("systemctl --version", done("systemd 260 (260.5)\n+PAM +AUDIT\n"));
        assert_eq!(t.systemd_version().unwrap(), "systemd 260 (260.5)");
    }

    #[test]
    fn the_journal_reads_with_its_bounds() {
        let (f, t) = tools();
        let q = JournalQuery::new(Preset::Live, 500);
        let line = format!("journalctl {}", journal_args(&q).unwrap().join(" "));
        f.capture_ok(&line, done(JOURNAL));
        let read = t.journal(&q).unwrap();
        assert_eq!(read.entries.len(), 3);
        assert!(!read.capped && !read.timed_out);
        let (_, l) = &f.limits()[0];
        assert_eq!((l.deadline, l.max_bytes, l.max_lines), (Duration::from_secs(5), 4 << 20, 501));

        f.capture_ok(&line, Captured { stdout: JOURNAL.as_bytes().to_vec(), end: CaptureEnd::TimedOut });
        assert!(t.journal(&q).unwrap().timed_out, "what came before the deadline is shown");
        f.capture_ok(&line, Captured { stdout: JOURNAL.as_bytes().to_vec(), end: CaptureEnd::Capped });
        assert!(t.journal(&q).unwrap().capped);
        f.capture_ok(&line, Captured { stdout: vec![], end: CaptureEnd::Exited(Some(1)) });
        assert_eq!(t.journal(&q).unwrap().entries, vec![], "no match is an empty view");
        f.capture_ok(&line, Captured { stdout: vec![], end: CaptureEnd::Exited(Some(2)) });
        assert!(matches!(t.journal(&q), Err(BackendError::Failed { code: Some(2), .. })));

        let mut bad = JournalQuery::new(Preset::ServiceLogs, 10);
        bad.unit = Some((Scope::System, "--now".into()));
        let before = f.calls().len();
        assert!(t.journal(&bad).is_err());
        assert_eq!(f.calls().len(), before, "an invalid query runs nothing");
    }

    #[test]
    fn follow_reads_have_a_two_second_deadline() {
        let (f, t) = tools();
        let mut q = JournalQuery::new(Preset::Live, 500);
        q.after_cursor = Some("s=1;i=1".into());
        f.capture_ok(&format!("journalctl {}", journal_args(&q).unwrap().join(" ")), done(JOURNAL));
        assert_eq!(t.journal(&q).unwrap().entries.len(), 3);
        let (_, l) = &f.limits()[0];
        assert_eq!((l.deadline, l.max_bytes, l.max_lines), (Duration::from_secs(2), 1 << 20, 1001));
    }

    #[test]
    fn user_action_lines() {
        let (f, t) = tools();
        f.ok("systemctl --user --no-block restart -- -.mount", "");
        f.ok("systemctl --user reset-failed -- example-sync.service", "");
        assert_eq!(t.user_action(UnitAction::Restart, "-.mount"), Ok(()));
        assert_eq!(t.user_action(UnitAction::ResetFailed, "example-sync.service"), Ok(()));
        assert_eq!(f.calls(), ["systemctl --user --no-block restart -- -.mount", "systemctl --user reset-failed -- example-sync.service"]);
        assert!(t.user_action(UnitAction::Restart, "--now").is_err());
        assert_eq!(f.calls().len(), 2, "an invalid name runs nothing");
    }

    #[test]
    fn a_user_action_timeout_is_a_timeout() {
        let (f, t) = tools();
        f.respond("systemctl --user --no-block restart -- example-app.service", Err(ExecError::Timeout));
        assert_eq!(t.user_action(UnitAction::Restart, "example-app.service"), Err(BackendError::Timeout { tool: "systemctl".into() }));
    }

    #[test]
    fn copy_sends_the_text_on_stdin() {
        let (f, t) = tools();
        f.ok("wl-copy --trim-newline", "");
        t.copy(&["wl-copy".into(), "--trim-newline".into()], "report text").unwrap();
        assert_eq!(f.inputs(), [("wl-copy --trim-newline".to_string(), b"report text".to_vec())]);
        assert!(f.calls().iter().all(|c| !c.contains("report text")), "never on the command line");
        assert!(t.copy(&[], "x").is_err());
        f.ok("wl-copy", "");
        assert_eq!(t.copy(&["wl-copy".into()], &"x".repeat(MAX_INPUT)), Ok(()), "a page exactly is fine");
        assert!(t.copy(&["wl-copy".into()], &"x".repeat(MAX_INPUT + 1)).is_err(), "over a page is refused before anything starts");
        assert_eq!(f.inputs().len(), 2, "the refused one started nothing");
        assert_eq!(t.copy(&["missing".into()], "x"), Err(BackendError::Missing { tool: "missing".into() }));
    }

    #[test]
    fn a_check_reports_its_first_line() {
        let (f, t) = tools();
        let check = |run: &[&str]| Check { name: "c".into(), run: run.iter().map(|s| s.to_string()).collect(), fix: None };
        f.ok("good", "");
        assert_eq!(t.check(&check(&["good"])), CheckResult { ok: true, detail: String::new() });
        f.respond(
            "bad one",
            Ok(crate::exec::Output { code: Some(1), stdout: "\n  2 hypridle running\u{1b}[2J\nsecond".into(), stderr: "err".into() }),
        );
        assert_eq!(t.check(&check(&["bad", "one"])), CheckResult { ok: false, detail: "2 hypridle running".into() });
        f.respond("quiet", Ok(crate::exec::Output::fail(1, "only on stderr\nmore")));
        assert_eq!(t.check(&check(&["quiet"])).detail, "only on stderr");
        assert_eq!(t.check(&check(&["nothing"])), CheckResult { ok: false, detail: "nothing not found".into() });
        f.respond("slow", Err(ExecError::Timeout));
        assert_eq!(t.check(&check(&["slow"])).detail, "slow is not responding");
        f.respond("long", Ok(crate::exec::Output::fail(1, &"x".repeat(300))));
        assert_eq!(t.check(&check(&["long"])).detail.chars().count(), 120);
        assert!(!t.check(&check(&[])).ok);
    }

    #[test]
    fn the_report_is_built_from_the_reads_and_redacted() {
        use crate::diagnostics::report::HostFacts;
        let (f, t) = tools();
        let facts = HostFacts {
            kernel: "6.18.2".into(),
            os: "NixOS 26.05 (Yarara)".into(),
            users: vec!["alice".into()],
            homes: vec!["/home/alice".into()],
            host: Some("host-01".into()),
            machine_id: None,
        };
        let t = t.with_facts(facts);
        f.capture_ok("systemctl list-units --failed --output=json --no-pager", done(FAILED));
        f.capture_ok("systemctl --user list-units --failed --output=json --no-pager", done("[]"));
        f.capture_ok("systemctl show --property=Id,NRestarts,ActiveState,SubState --no-pager *.service", done(SHOW));
        f.capture_ok("systemctl --user show --property=Id,NRestarts,ActiveState,SubState --no-pager *.service", done(""));
        f.capture_ok(
            "coredumpctl list --json=short --no-pager --reverse --since=-7d -n 20",
            done(include_str!("../../tests/fixtures/diag-coredumps.json")),
        );
        f.capture_ok("systemctl --version", done("systemd 260 (260.5)\n+PAM\n"));
        let errors = r#"{"__CURSOR":"s=1;i=1","__REALTIME_TIMESTAMP":"1700010000000000","SYSLOG_IDENTIFIER":"example-app","MESSAGE":"cannot open /home/alice/data from host-01 at 192.168.1.20"}"#;
        f.capture_ok(
            "journalctl -o json --output-fields=MESSAGE,SYSLOG_IDENTIFIER,_COMM --no-pager -n 200 -b -p err",
            done(&format!("{errors}\n")),
        );
        let text = t.report("Mango", 3);
        assert!(text.starts_with("alpheus diagnostics report, 20") && text.lines().next().unwrap().ends_with(" UTC"), "{text}");
        assert!(text.contains(" · systemd 260 (260.5) · kernel 6.18.2 · Mango · NixOS 26.05 (Yarara)"), "{text}");
        assert!(text.contains("Failed units:") && text.contains("Restarting in a loop: example-sync.service"), "{text}");
        assert!(text.contains("cannot open ~/data from <host> at <ip>"), "redacted before it is shown: {text}");
        assert!(!text.contains("alice") && !text.contains("192.168"), "{text}");
        assert!(text.len() <= MAX_INPUT);
        // Not one command line, environment or id was asked for.
        for (line, _) in f.limits() {
            assert!(!line.contains("_CMDLINE") && !line.contains("_BOOT_ID") && !line.contains("show-environment"), "{line}");
        }
    }

    #[test]
    fn a_failed_read_is_unknown_in_the_report_not_none() {
        let (_f, t) = tools();
        let t = t.with_facts(HostFacts::default());
        let text = t.report("Mango", 3);
        assert!(text.contains("Failed units: unknown") && text.contains("Errors this boot: unknown"), "{text}");
    }

    #[test]
    fn the_system_journal_is_readable_only_when_a_journal_file_opens() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::temp_path::TempPath::dir("journal-readable");
        let machine = dir.join("0123456789abcdef0123456789abcdef");
        std::fs::create_dir_all(&machine).unwrap();
        let tools = |d: &std::path::Path| SystemdTools::new(Arc::new(FakeExecutor::new())).with_journal_dirs(vec![d.to_path_buf()]);
        assert!(!tools(&dir).journal_readable(), "a directory with no journal file");
        let file = machine.join("system.journal");
        std::fs::write(&file, b"x").unwrap();
        assert!(tools(&dir).journal_readable());
        // Not in systemd-journal: the directory lists, the file does not open.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = !tools(&dir).journal_readable();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        // (root opens anything: the assertion only holds for an ordinary user)
        // SAFETY: geteuid(2) takes no argument and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        assert!(denied || root, "a file the user cannot open is not readable");
        assert!(!tools(&dir.join("missing")).journal_readable(), "no such directory");
    }

    /// A daemon of one connection in the test's directory: reads one request, answers `reply`.
    fn serve(dir: &std::path::Path, reply: Reply) -> (SocketPoke, std::thread::JoinHandle<Request>) {
        use crate::launcher_ipc::{RequestDecoder, encode_reply};
        use std::io::{Read, Write};
        let socket = dir.join("alpheus-launcher.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let h = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut decoder = RequestDecoder::new();
            let mut chunk = [0u8; 4096];
            let request = loop {
                let n = conn.read(&mut chunk).unwrap();
                assert!(n > 0, "the client hung up before a whole request");
                if let Some(r) = decoder.feed(&chunk[..n]).unwrap() {
                    break r;
                }
            };
            conn.write_all(&encode_reply(&reply)).unwrap();
            request
        });
        (SocketPoke::new(socket), h)
    }

    #[test]
    fn the_poke_sends_failed_refresh_and_reads_the_answer() {
        let dir = crate::temp_path::TempPath::dir("poke-done");
        let (p, h) = serve(&dir, Reply::Done);
        assert_eq!(p.failed_refresh(), PokeAnswer::Done);
        assert_eq!(h.join().unwrap(), Request::FailedRefresh);
        let (p, h) = serve(&dir, Reply::Error("malformed request: unknown request \"failed-refresh\"".into()));
        assert_eq!(p.failed_refresh(), PokeAnswer::Older, "a daemon from before the verb");
        h.join().unwrap();
        let (p, h) = serve(&dir, Reply::Error("request deadline passed".into()));
        assert_eq!(p.failed_refresh(), PokeAnswer::Unreachable("request deadline passed".into()));
        h.join().unwrap();
        let (p, h) = serve(&dir, Reply::Cancelled);
        assert!(matches!(p.failed_refresh(), PokeAnswer::Unreachable(_)));
        h.join().unwrap();
    }

    #[test]
    fn no_socket_or_no_listener_is_a_daemon_not_running() {
        let dir = crate::temp_path::TempPath::dir("poke-absent");
        let p = SocketPoke::new(dir.join("alpheus-launcher.sock"));
        let not_running = PokeAnswer::Unreachable("the alpheus daemon is not running".into());
        assert_eq!(p.failed_refresh(), not_running);
        drop(std::os::unix::net::UnixListener::bind(dir.join("alpheus-launcher.sock")).unwrap());
        assert_eq!(p.failed_refresh(), not_running, "a socket left behind");
    }

    #[test]
    fn no_poke_is_unreachable() {
        assert_eq!(NoPoke.failed_refresh(), PokeAnswer::Unreachable("not wired".into()));
    }
}
