//! Injectable command executor: the ONLY point through which alpheus
//! launches a system tool. Tests inject `fake::FakeExecutor`, never the real one.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};
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

/// Standard output of a long-running command (e.g. `pw-dump -m`), read as a
/// stream. Dropping it kills the command's process group and reaps it.
pub type Stream = Box<dyn Read + Send>;

pub trait Executor: Send + Sync {
    /// Launches `prog args…`, waits for it to finish (bounded delay), captures the outputs.
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError>;
    /// Launches `prog args…` without waiting (e.g. hyprlock, or whatever a
    /// click's `[actions]`/`[workspaces]` command runs), outputs discarded.
    /// Fully detached (double fork): the launched program is never our
    /// child, so it is never reachable through `kill_running` and outlives
    /// alpheus restarting or shutting down (e.g. a lock screen must keep
    /// running). No thread lives for as long as the program does, however
    /// long that is.
    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError>;
    /// Launches `prog args…` for as long as its output is read: standard
    /// output piped, standard error discarded, its own process group, and
    /// killed (SIGTERM) if the THREAD that launched it ends (prctl
    /// PR_SET_PDEATHSIG) — even when nothing runs any `Drop` (SIGKILL).
    /// PDEATHSIG covers the command itself, not its children: normal
    /// cleanup (`Drop`, `kill_running`) kills the whole group instead.
    /// Dropping the returned stream kills the group and reaps the child.
    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError>;
}

/// PIDs of the commands in progress — `run`s, and `stream`s not yet dropped —
/// and a closing lock: once set to `true` by `kill_running`, it forbids any
/// new launch — before and after the call alike, never a window where a
/// tool could start without our knowing, and therefore without our being
/// able to kill it.
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
        // Killed with the lock held: a PID only leaves the list under this
        // same lock — `run` reaps its child and removes it in one go, a
        // stream's drop removes it before killing and reaping it — so every
        // PID targeted here has not yet been reaped: it cannot have been
        // reused. The whole group is killed: grandchildren (`sh -c '… &'`)
        // included.
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

/// Unblocks every signal in a child about to exec. The daemon and the TUI
/// block SIGTERM, SIGHUP and SIGINT in all their threads (they wait for
/// them in one, shutdown.rs); a child inherits that mask across fork and
/// exec, and std leaves it as is: without this, a PDEATHSIG SIGTERM would
/// stay pending forever, and an application launched from the bar would
/// ignore SIGTERM. Async-signal-safe (sigemptyset, sigprocmask).
fn unblock_signals() -> std::io::Result<()> {
    // SAFETY: a zero-filled sigset_t is valid storage, initialized by
    // sigemptyset; sigprocmask only changes the calling (child) process.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        if libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Forks a second time so the process that ultimately execs `prog` is never
/// our direct child: the first fork's child (the one `Command::spawn`
/// returns, reaped synchronously right after by `spawn`, above) exits
/// immediately here, while the second fork's child — about to be reparented
/// to init or the nearest subreaper — is the one that returns `Ok(())` and
/// lets `do_exec` proceed. Runs between fork and exec: only async-signal-safe
/// calls (sigprocmask via `unblock_signals`, fork, `_exit`); stdio, process
/// group and the rest of `Command`'s setup already happened before this
/// closure runs, and both forked copies inherit them identically.
fn detach() -> std::io::Result<()> {
    unblock_signals()?;
    // SAFETY: fork(2) and _exit(2) are async-signal-safe; nothing allocates
    // and no `Drop` runs between the fork and the `_exit`/return below.
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error()),
        0 => Ok(()),                    // grandchild: proceed to exec `prog`.
        _ => unsafe { libc::_exit(0) }, // intermediate: nothing left to do.
    }
}

/// Launches `prog args…` fully detached (double fork), outputs discarded,
/// never waiting for it. Returns the INTERMEDIATE's PID — reaped
/// synchronously before returning, kept only so tests can confirm it is
/// never left as a zombie; callers otherwise have no use for it. The
/// grandchild that actually execs `prog` is reparented to init (or the
/// nearest subreaper) the instant the intermediate exits (see `detach`), so
/// no zombie is ever left for alpheus to clean up, and nothing of ours keeps
/// running once `prog` has started — a click never leaves a resident reaper
/// thread behind for as long as the program runs. Same guarantees as the
/// pre-double-fork implementation: own process group, signal mask unblocked,
/// stdio nulled, not killed by `kill_running` (never registered in
/// `Running`: a spawned program outlives the daemon, e.g. the lock screen
/// must survive alpheus restarting).
fn spawn_detached(prog: &str, args: &[&str]) -> Result<u32, ExecError> {
    let mut command = Command::new(prog);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Separate process group: closing alpheus's window (SIGHUP to
        // the terminal's group) does not kill the lock screen.
        .process_group(0);
    // SAFETY: `detach` runs in the child between fork and exec and only
    // makes async-signal-safe calls (sigprocmask, fork, _exit).
    unsafe { command.pre_exec(detach) };
    let mut intermediate = command.spawn().map_err(io_err)?;
    let pid = intermediate.id();
    let _ = intermediate.wait();
    Ok(pid)
}

/// A command started by `stream`: reading it reads its standard output;
/// dropping it kills its process group (if `kill_running` hasn't already)
/// and reaps it, so a stream never leaves anything behind.
struct ChildStream {
    child: Child,
    stdout: ChildStdout,
    running: Arc<Mutex<Running>>,
}

impl Read for ChildStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Drop for ChildStream {
    fn drop(&mut self) {
        let pid = self.child.id();
        {
            // Never panic in a drop: a poisoned lock still holds a valid set.
            let mut guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
            // Still listed = not killed by `kill_running`, not reaped: its
            // PID (= PGID) is still ours.
            if guard.pids.remove(&pid) {
                kill_group(pid);
            }
        }
        let _ = self.child.wait();
    }
}

impl Executor for SystemExecutor {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError> {
        // alpheus is shutting down (`kill_running` already called): no launch.
        if self.running.lock().unwrap().closed {
            return Err(ExecError::Io("alpheus is shutting down".into()));
        }
        let mut command = Command::new(prog);
        command
            .args(args)
            // Untranslated output: parsers expect English.
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Own process group (PGID = child's PID): on timeout or on
            // shutdown, we kill the group, grandchildren included.
            .process_group(0);
        // SAFETY: `unblock_signals` only makes async-signal-safe calls.
        unsafe { command.pre_exec(unblock_signals) };
        let mut child = command.spawn().map_err(io_err)?;
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
        spawn_detached(prog, args).map(|_intermediate_pid| ())
    }

    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError> {
        if self.running.lock().unwrap().closed {
            return Err(ExecError::Io("alpheus is shutting down".into()));
        }
        let parent = std::process::id();
        let mut command = Command::new(prog);
        command.args(args).env("LC_ALL", "C.UTF-8").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
        // SAFETY: the closure runs in the child between fork and exec and
        // only makes async-signal-safe calls (sigprocmask, prctl, getppid,
        // _exit).
        unsafe {
            command.pre_exec(move || {
                unblock_signals()?;
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The parent already died between fork and prctl: nobody
                // will ever read us. PDEATHSIG follows the forking THREAD,
                // not the process, and getppid() can't tell whether that
                // thread is still alive — a harmless gap here: the only
                // stream's thread (`sound`) lives as long as the daemon,
                // so the thread dies only when the process does.
                if libc::getppid() as u32 != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(io_err)?;
        let pid = child.id();
        let Some(stdout) = child.stdout.take() else {
            kill_group(pid);
            let _ = child.wait();
            return Err(ExecError::Io("no standard output".into()));
        };
        {
            let mut guard = self.running.lock().unwrap();
            if guard.closed {
                // Shutdown between the check and the spawn (see `run`).
                drop(guard);
                kill_group(pid);
                let _ = child.wait();
                return Err(ExecError::Io("alpheus is shutting down".into()));
            }
            guard.pids.insert(pid);
        }
        Ok(Box::new(ChildStream { child, stdout, running: self.running.clone() }))
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

    /// Threads of this whole test binary (other tests may have their own,
    /// transient ones): a regression that brings back a reaper thread per
    /// `spawn` shows up as a jump of (about) N, held for as long as the N
    /// children run.
    fn thread_count() -> usize {
        std::fs::read_dir("/proc/self/task").map(|d| d.count()).unwrap_or(0)
    }

    /// The old implementation (`std::thread::spawn` + `child.wait()`) kept
    /// one OS thread alive per `spawn` for as long as the launched program
    /// ran. With the double fork, `spawn` only reaps the intermediate (which
    /// exits immediately) and returns: no thread survives into the
    /// children's 2 s lifetime. A generous tolerance absorbs unrelated
    /// threads from tests running concurrently in this binary; the old bug
    /// would add N persistent threads, far past this margin.
    #[test]
    fn spawn_leaves_no_thread_for_the_lifetime_of_the_children() {
        let e = exec();
        let before = thread_count();
        const N: usize = 50;
        for _ in 0..N {
            e.spawn("sleep", &["2"]).unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
        let after = thread_count();
        // Generous tolerance absorbs unrelated threads from tests running
        // concurrently in this binary; the old bug would add N (here 50)
        // persistent threads, far past this margin.
        assert!(after <= before + 10, "before={before} after={after}: spawn left threads alive while children run");
    }

    /// The grandchild that actually execs `prog` is never our child (it is
    /// reparented to init, or the nearest subreaper, the instant the
    /// intermediate exits): it still runs (writes its marker file) and its
    /// parent is not us.
    #[test]
    fn spawned_child_runs_detached_from_us() {
        // Reaping an orphan is the job of whatever adopts it (init or a
        // subreaper such as `systemd --user`); some environments have none
        // that reaps (the Nix build sandbox), so a zombie there says nothing
        // about us. What spawn guarantees is that the program runs and is NOT
        // our child: we never accumulate zombies ourselves.
        let f = std::env::temp_dir().join(format!("alpheus-spawn-{}-detach", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("echo $$ > {}; sleep 1", f.display());
        exec().spawn("sh", &["-c", &script]).unwrap();
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        assert!(pid > 1);
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("the program is still running");
        let ppid: u32 = status.lines().find_map(|l| l.strip_prefix("PPid:")).and_then(|v| v.trim().parse().ok()).unwrap();
        assert_ne!(ppid, std::process::id(), "the spawned program is our own child: not detached");
    }

    /// `spawn`'s own child (the intermediate — see `detach`) must be reaped
    /// synchronously before `spawn` returns: forgetting that `wait()` would
    /// leave it a zombie under its own (known) PID forever. Goes through
    /// `spawn_detached` directly (not the `Executor` trait) to get that PID:
    /// scanning all of `/proc` for "any zombie child of this process" would
    /// also catch other tests' transient, legitimate zombies (the brief
    /// window between a child exiting and ITS parent's `wait()`), since
    /// every test in this binary shares one PID.
    #[test]
    fn spawn_leaves_no_zombie_intermediate() {
        let pid = spawn_detached("true", &[]).unwrap();
        assert!(std::fs::metadata(format!("/proc/{pid}")).is_err(), "intermediate {pid} still present after wait() (zombie or otherwise)");
    }

    #[test]
    fn stream_reads_the_output_until_the_end() {
        let mut s = exec().stream("sh", &["-c", "printf 'a\\nb\\n'"]).unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        assert_eq!(text, "a\nb\n");
    }

    #[test]
    fn stream_missing_program() {
        assert_eq!(exec().stream("alpheus-nonexistent-program", &[]).err(), Some(ExecError::NotFound));
    }

    /// The grandchild outlives the command (300 s against 3 s): only killing
    /// the group ends it. Without the kill, `drop` would also wait for the
    /// command to end on its own (3 s) — hence the timing assertion.
    #[test]
    fn dropping_the_stream_kills_the_command_and_its_children() {
        let f = std::env::temp_dir().join(format!("alpheus-stream-{}-drop", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("sleep 300 & echo $! > {}; exec sleep 3", f.display());
        let s = exec().stream("sh", &["-c", &script]).unwrap();
        let grandchild = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        assert!(grandchild > 1);
        let t = Instant::now();
        drop(s);
        let took = t.elapsed();
        wait_for_death(grandchild); // kills it itself (cleanup) before failing
        assert!(took < Duration::from_secs(1), "drop waited {took:?} for the command instead of killing it");
    }

    #[test]
    fn kill_running_also_kills_streams() {
        let f = std::env::temp_dir().join(format!("alpheus-stream-{}-shutdown", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("echo $$ > {}; exec sleep 30", f.display());
        let e = exec();
        let s = e.stream("sh", &["-c", &script]).unwrap();
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        e.kill_running();
        wait_for_death(pid);
        drop(s); // already killed: the drop only reaps it
    }

    #[test]
    fn no_stream_after_closing() {
        let e = exec();
        e.kill_running();
        assert_eq!(e.stream("sh", &["-c", "true"]).err(), Some(ExecError::Io("alpheus is shutting down".into())));
    }

    /// If the daemon dies without running any `Drop` (SIGKILL, abort), the
    /// kernel must still kill the stream's command: PR_SET_PDEATHSIG, which
    /// fires when the THREAD that launched it ends. Simulated by a thread
    /// that launches it, then ends without dropping the stream (forgotten).
    #[test]
    fn the_command_dies_with_the_thread_that_launched_it() {
        let f = std::env::temp_dir().join(format!("alpheus-stream-{}-pdeathsig", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("echo $$ > {}; exec sleep 30", f.display());
        let e = Arc::new(exec());
        let e2 = e.clone();
        let f2 = f.clone();
        let pid = std::thread::spawn(move || {
            let s = e2.stream("sh", &["-c", &script]).unwrap();
            // The thread must outlive the shell's `echo`: PDEATHSIG would
            // otherwise kill it before it writes its PID.
            let pid = pid_written(&f2);
            std::mem::forget(s);
            pid
        })
        .join()
        .unwrap();
        let _ = std::fs::remove_file(&f);
        assert!(pid > 1);
        wait_for_death(pid);
    }

    /// The daemon and the TUI block SIGTERM, SIGHUP and SIGINT in every
    /// thread (they wait for them in one, see shutdown.rs); a child inherits
    /// that mask across fork and exec. Every command must start with none
    /// blocked, or it ignores SIGTERM (and a PDEATHSIG SIGTERM never comes).
    #[test]
    fn children_start_with_no_blocked_signal() {
        let f = std::env::temp_dir().join(format!("alpheus-sigblk-{}", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("grep SigBlk /proc/self/status > {}", f.display());
        let (run, stream) = std::thread::spawn(move || {
            // SAFETY: a zero-filled sigset_t initialized by sigemptyset and
            // sigaddset; pthread_sigmask only changes THIS test thread's mask.
            unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGTERM);
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            }
            let e = exec();
            let run = e.run("grep", &["SigBlk", "/proc/self/status"]).unwrap().stdout;
            let mut stream = String::new();
            e.stream("grep", &["SigBlk", "/proc/self/status"]).unwrap().read_to_string(&mut stream).unwrap();
            e.spawn("sh", &["-c", &script]).unwrap();
            (run, stream)
        })
        .join()
        .unwrap();
        let spawned = (0..200)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(10));
                std::fs::read_to_string(&f).ok().filter(|t| !t.is_empty())
            })
            .expect("the spawned command ran");
        let _ = std::fs::remove_file(&f);
        for out in [run, stream, spawned] {
            assert_eq!(out.trim(), "SigBlk:\t0000000000000000", "a mask inherited from the parent");
        }
    }
}
