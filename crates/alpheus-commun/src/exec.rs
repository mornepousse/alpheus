//! Injectable command executor: the ONLY point through which alpheus
//! launches a system tool. Tests inject `fake::FakeExecutor`, never the real one.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod fake;

/// Result of a command run to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Exit code; `None` if the command was killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// Success (code 0) with this standard output.
    pub fn ok(stdout: &str) -> Self {
        Output { code: Some(0), stdout: stdout.to_string(), stderr: String::new() }
    }

    /// Failure with this code and this error message.
    pub fn fail(code: i32, stderr: &str) -> Self {
        Output { code: Some(code), stdout: String::new(), stderr: stderr.to_string() }
    }

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Failure that happened before getting any output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// Program missing from PATH.
    NotFound,
    /// Timeout exceeded: the command was killed.
    Timeout,
    /// Any other I/O error.
    Io(String),
}

pub trait Executor: Send + Sync {
    /// Launches `prog args…`, waits for it to finish (bounded delay), captures the outputs.
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError>;
    /// Launches `prog args…` without waiting (e.g. hyprlock), outputs discarded.
    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError>;
}

/// PIDs of the `run` commands in progress, and a closing lock: once set to
/// `true` by `kill_running`, it forbids any new launch — before and after
/// the call alike, never a window where a tool could start without our
/// knowing, and therefore without our being able to kill it.
#[derive(Default)]
struct Running {
    pids: HashSet<u32>,
    closed: bool,
}

/// Real executor: `std::process`, a maximum delay per command.
pub struct SystemExecutor {
    timeout: Duration,
    running: Arc<Mutex<Running>>,
}

impl SystemExecutor {
    pub fn new(timeout: Duration) -> Self {
        SystemExecutor { timeout, running: Arc::new(Mutex::new(Running::default())) }
    }

    /// Kills the commands still in progress and closes the door: alpheus
    /// leaves nothing behind, and nothing can start after this.
    pub fn kill_running(&self) {
        let mut guard = self.running.lock().unwrap();
        guard.closed = true;
        // Killed with the lock held: `run` only removes a child from the
        // list after reaping it under this same lock, so every PID targeted
        // here has not yet been reaped — it cannot have been reused. The
        // whole group is killed: grandchildren (`sh -c '… &'`) included.
        for pid in guard.pids.drain() {
            kill_group(pid);
        }
    }
}

/// kill(2) target for process group `pgid`: `-pgid`, only if `pgid` could be
/// the PID of one of our children. Never `-1` (every process of the user)
/// nor `0` (our own group). Pure function: tested without ever sending a signal.
fn group_target(pgid: u32) -> Option<libc::pid_t> {
    let p = libc::pid_t::try_from(pgid).ok()?;
    (p > 1).then_some(-p)
}

/// Kills the whole process group `pgid` (a child launched with
/// `.process_group(0)` and its descendants). Only called for a child not
/// yet reaped, whose PID is therefore still its own.
fn kill_group(pgid: u32) {
    let Some(target) = group_target(pgid) else { return };
    assert!(target < -1, "dangerous group target: {target}");
    // SAFETY: kill(2) targets only group `pgid`, created by us; no pointer involved.
    unsafe { libc::kill(target, libc::SIGKILL) };
}

/// Has the child `pid` finished? Queries without reaping it (WNOWAIT): as
/// long as it is not reaped, its PID cannot be reused.
fn exited(pid: u32) -> Result<bool, ExecError> {
    let id = libc::id_t::from(pid);
    // SAFETY: siginfo_t is a plain C struct, valid when zero-filled.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid struct that lives for the duration of the call.
    let r = unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
    if r == -1 {
        return Err(ExecError::Io(std::io::Error::last_os_error().to_string()));
    }
    // SAFETY: with WNOHANG, si_pid is 0 as long as nothing has changed.
    Ok(unsafe { info.si_pid() } != 0)
}

fn io_err(e: std::io::Error) -> ExecError {
    if e.kind() == std::io::ErrorKind::NotFound { ExecError::NotFound } else { ExecError::Io(e.to_string()) }
}

/// Drains a pipe to the end on a separate thread: output bigger than the
/// pipe's buffer cannot block waiting for the process.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

impl Executor for SystemExecutor {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError> {
        // alpheus is shutting down (`kill_running` already called): no launch.
        if self.running.lock().unwrap().closed {
            return Err(ExecError::Io("alpheus is shutting down".into()));
        }
        let mut child = Command::new(prog)
            .args(args)
            // Untranslated output: parsers expect English.
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Own process group (PGID = child's PID): on timeout or on
            // shutdown, we kill the group, grandchildren included.
            .process_group(0)
            .spawn()
            .map_err(io_err)?;
        let pid = child.id();
        {
            let mut guard = self.running.lock().unwrap();
            if guard.closed {
                // Shutdown happened between the check and the spawn: the
                // child was just born without our being able to announce it
                // to `kill_running`, which will therefore never kill it —
                // we kill it ourselves right away.
                drop(guard);
                kill_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExecError::Io("alpheus is shutting down".into()));
            }
            guard.pids.insert(pid);
        }
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let start = Instant::now();
        let ended = loop {
            match exited(pid) {
                Ok(true) => break Ok(()),
                Ok(false) if start.elapsed() >= self.timeout => {
                    // Child not yet reaped: its PID (= PGID) is ours.
                    kill_group(pid);
                    break Err(ExecError::Timeout);
                }
                Ok(false) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => break Err(e),
            }
        };
        // Removed from the list BEFORE being reaped (see `kill_running`).
        self.running.lock().unwrap().pids.remove(&pid);
        if let Err(e) = ended {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        let status = child.wait().map_err(|e| ExecError::Io(e.to_string()))?;
        Ok(Output { code: status.code(), stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
    }

    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Separate process group: closing alpheus's window (SIGHUP to
            // the terminal's group) does not kill the lock screen.
            .process_group(0)
            .spawn()
            .map_err(io_err)?;
        // Reaps the child at its end (no zombie) without blocking the caller.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only `sh` and `sleep` are launched here: no session tool.
    fn exec() -> SystemExecutor {
        SystemExecutor::new(Duration::from_secs(5))
    }

    #[test]
    fn captures_standard_output() {
        let out = exec().run("sh", &["-c", "printf hello"]).unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, "hello");
    }

    #[test]
    fn reports_code_and_error() {
        let out = exec().run("sh", &["-c", "echo oops >&2; exit 3"]).unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stderr, "oops\n");
        assert!(!out.success());
    }

    #[test]
    fn missing_program() {
        assert_eq!(exec().run("alpheus-nonexistent-program", &[]), Err(ExecError::NotFound));
    }

    #[test]
    fn timeout_kills_the_command() {
        let e = SystemExecutor::new(Duration::from_millis(100));
        let t = Instant::now();
        assert_eq!(e.run("sleep", &["5"]), Err(ExecError::Timeout));
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    }

    /// Is the grandchild `pid` still alive? A zombie (waiting to be reaped
    /// by its new parent) counts as dead.
    fn alive(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => false,
            // "pid (comm) state …": the state follows the last closing parenthesis.
            Ok(st) => st.rsplit_once(')').and_then(|(_, r)| r.trim_start().chars().next()) != Some('Z'),
        }
    }

    fn pid_written(f: &std::path::Path) -> i32 {
        let t = Instant::now();
        loop {
            if let Some(p) = std::fs::read_to_string(f).ok().and_then(|s| s.trim().parse().ok()) {
                return p;
            }
            assert!(t.elapsed() < Duration::from_secs(2), "grandchild PID never written");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_death(pid: i32) {
        let t = Instant::now();
        while alive(pid) {
            if t.elapsed() > Duration::from_secs(2) {
                // SAFETY: cleanup of the `sleep` launched by this test (pid > 1).
                unsafe { libc::kill(pid, libc::SIGKILL) };
                panic!("grandchild {pid} survived");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn timeout_also_kills_grandchildren() {
        let f = std::env::temp_dir().join(format!("alpheus-grandchild-{}-timeout", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("sleep 30 & echo $! > {}; wait", f.display());
        let e = SystemExecutor::new(Duration::from_millis(100));
        let t = Instant::now();
        assert_eq!(e.run("sh", &["-c", &script]), Err(ExecError::Timeout));
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        assert!(pid > 1);
        wait_for_death(pid);
    }

    #[test]
    fn kill_running_also_kills_grandchildren() {
        let f = std::env::temp_dir().join(format!("alpheus-grandchild-{}-shutdown", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("sleep 30 & echo $! > {}; wait", f.display());
        let e = Arc::new(SystemExecutor::new(Duration::from_secs(30)));
        let e2 = e.clone();
        let h = std::thread::spawn(move || e2.run("sh", &["-c", &script]));
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        e.kill_running();
        assert!(pid > 1);
        // Before the join: a surviving grandchild would keep the output
        // pipe open and would block `run` until it ends.
        wait_for_death(pid);
        let _ = h.join().unwrap();
    }

    #[test]
    fn never_a_dangerous_group_pid() {
        // -0 would target our own group, -1 every one of our processes: refused.
        // (Pure function: no signal is sent here.)
        assert_eq!(group_target(0), None);
        assert_eq!(group_target(1), None);
        assert_eq!(group_target(u32::MAX), None, "does not fit in a positive pid_t");
        assert_eq!(group_target(1 << 31), None);
        assert_eq!(group_target(4242), Some(-4242));
    }

    #[test]
    fn output_not_localised() {
        // nmcli translates some output: we force a neutral locale.
        let out = exec().run("sh", &["-c", "printf %s \"$LC_ALL\""]).unwrap();
        assert_eq!(out.stdout, "C.UTF-8");
    }

    #[test]
    fn kill_running_kills_running_commands() {
        let e = Arc::new(SystemExecutor::new(Duration::from_secs(30)));
        let e2 = e.clone();
        let h = std::thread::spawn(move || e2.run("sleep", &["30"]));
        let t = Instant::now();
        while e.running.lock().unwrap().pids.is_empty() {
            assert!(t.elapsed() < Duration::from_secs(2), "the command did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        e.kill_running();
        let out = h.join().unwrap().unwrap();
        assert_eq!(out.code, None, "killed by a signal");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn no_launch_after_closing() {
        let e = exec();
        e.kill_running();
        assert_eq!(e.run("sh", &["-c", "true"]), Err(ExecError::Io("alpheus is shutting down".into())));
    }

    #[test]
    fn spawn_does_not_block() {
        let t = Instant::now();
        exec().spawn("sleep", &["1"]).unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn spawn_missing_program() {
        assert_eq!(exec().spawn("alpheus-nonexistent-program", &[]), Err(ExecError::NotFound));
    }
}
