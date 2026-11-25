//! Injectable command executor: the ONLY point through which alpheus
//! launches a system tool. Tests inject `fake::FakeExecutor`, never the real one.
//!
//! Never:
//! - launch a tool anywhere else: this is the one point, so a test can replace it and no code path
//!   can reach a real program by accident (CONTRIBUTING.md's list of session tools);
//! - block in the daemon: `run` waits, and the daemon's wrapper refuses it outright
//!   (`the_only_system_executor_is_wrapped_in_non_blocking`, in the daemon's guardrails);
//! - write more than one page to a program's standard input (`MAX_INPUT`), so a write never waits
//!   for the program to read (a payload that needs more goes through a file: `run_with_stdin_file`).
//! - signal a program it did not start, or one already reaped: a watched program (`spawn_watched`) is
//!   interrupted through its own process group (by its pid if it left that group) only while it is
//!   still our unreaped child (`an_interrupt_is_never_sent_to_a_reaped_pid`).
//!
//! See: docs/ARCHITECTURE.md ("The foundation and the client"), docs/features/shield.md and
//! docs/features/quick-settings.md (what runs through it), docs/decisions.md ("An external capture tool
//! runs as a watched child of the daemon").

use std::collections::HashSet;
use std::io::{Read, Write};
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

/// What `capture` reads at most, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureLimits {
    /// Counted from the spawn: past it, the command's group is killed.
    pub deadline: Duration,
    /// Bytes of standard output kept at most; one more and reading stops.
    pub max_bytes: usize,
    /// Lines kept at most; anything after the last of them and reading stops.
    pub max_lines: usize,
}

/// How a capture ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEnd {
    /// The command ended by itself: its exit code, None when a signal ended it.
    Exited(Option<i32>),
    /// The deadline passed first.
    TimedOut,
    /// A cap was passed first.
    Capped,
}

/// A command's standard output read by `capture`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// As read, cut at the caps: a capped or timed out read may end in the middle of a line.
    pub stdout: Vec<u8>,
    pub end: CaptureEnd,
}

/// The most bytes `spawn_with_input` writes, all parts together: one page (4,096 bytes), which every
/// pipe holds whatever its size, so the write never waits for the program to read.
pub const MAX_INPUT: usize = 4096;

pub trait Executor: Send + Sync {
    /// Launches `prog args…`, waits for it to finish (bounded delay), captures the outputs.
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError>;
    /// Launches `prog args…` without waiting (e.g. a lock screen, or whatever a
    /// click's `[actions]`/`[workspaces]` command runs), outputs discarded.
    /// Fully detached (double fork): the launched program is never our
    /// child, so it is never reachable through `kill_running` and outlives
    /// alpheus restarting or shutting down (e.g. a lock screen must keep
    /// running). No thread lives for as long as the program does, however
    /// long that is.
    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError>;

    /// `spawn`, with `input`'s parts written in order to the program's standard input, which is then
    /// closed: a secret handed to a program this way (the Wi-Fi password, `nmcli --ask`) is never on
    /// its command line, never through a shell. The parts are written straight from the caller's
    /// memory, never joined into a copy. More than MAX_INPUT bytes in all: refused before anything
    /// starts. An `Io` error may also come once the program started (it exited, or closed its
    /// input, before reading it all): the program was launched, so the caller never runs it again
    /// for that error.
    fn spawn_with_input(&self, prog: &str, args: &[&str], input: &[&[u8]]) -> Result<(), ExecError>;

    /// Runs `prog args…` with the standard input read from the file `file`, its output discarded, and
    /// waits at most `limit` for the program's own exit (a program that forks and leaves a child
    /// behind, as `wl-copy` does to hold the clipboard, is done when its foreground ends). For a
    /// payload `spawn_with_input`'s page cannot carry: a screenshot sent to the clipboard program. The
    /// file is read through a descriptor opened here, so no pipe can fill. Returns the exit code
    /// (`None`: ended by a signal); `Timeout` kills the program (only it, never what it left behind);
    /// a file that cannot be opened is an `Io` error and starts nothing. Blocks: only on a worker
    /// thread, never the event loop. Not supported by default.
    ///
    /// The program is not registered in `Running`, so `kill_running` at shutdown does not reach it;
    /// what bounds it is `limit`, at which only the direct child is killed (not its process group).
    fn run_with_stdin_file(
        &self,
        _prog: &str,
        _args: &[&str],
        _file: &std::path::Path,
        _limit: Duration,
    ) -> Result<Option<i32>, ExecError> {
        Err(ExecError::Io("standard input from a file is not supported".into()))
    }

    /// `spawn` (detached, double fork, own session, stdio nulled, never registered in `Running`), with
    /// each `(name, value)` added to the inherited environment and each name of `unset` removed from
    /// it first (so `env` wins for a name in both). Refused before anything starts
    /// (`Io("invalid environment variable")`) when a name is empty or holds `=` or NUL, or a value
    /// holds NUL.
    fn spawn_with_env(&self, prog: &str, args: &[&str], env: &[(&str, &str)], unset: &[&str]) -> Result<(), ExecError>;
    /// Launches `prog args…` for as long as its output is read: standard
    /// output piped, standard error discarded, its own process group, and
    /// killed (SIGTERM) if the THREAD that launched it ends (prctl
    /// PR_SET_PDEATHSIG) — even when nothing runs any `Drop` (SIGKILL).
    /// PDEATHSIG covers the command itself, not its children: normal
    /// cleanup (`Drop`, `kill_running`) kills the whole group instead.
    /// Dropping the returned stream kills the group and reaps the child.
    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError>;
    /// Launches `prog args…` and reads its standard output until it ends, `limits.deadline` passes
    /// (counted from the spawn) or a cap is passed. Standard input and error `/dev/null`, the
    /// caller's environment as is (no `LC_ALL`: what it prints is shown, not parsed), its own process
    /// group, killed if the launching thread dies (PDEATHSIG). Whatever the end, the group is then
    /// killed (SIGKILL) and the command reaped: nothing it started outlives the read. Blocks for up to
    /// the deadline: never on the event loop (only the launcher's scan thread calls it, a guardrail).
    fn capture(&self, prog: &str, args: &[&str], limits: CaptureLimits) -> Result<Captured, ExecError>;
    /// Launches `prog args…` and waits for it, with no bound, its standard input, output and error
    /// all `/dev/null` (so nothing it writes can ever fail, and a privileged helper that outlives
    /// whoever waits is never cut by a closed pipe), its own process group, killed if the launching
    /// thread dies (PDEATHSIG). Returns its exit code (`None`: ended by a signal). Registered with
    /// `stop_running`, which kills the group of a command it can still reach. Blocks: never on the
    /// event loop.
    fn run_silent(&self, prog: &str, args: &[&str]) -> Result<Option<i32>, ExecError>;
    /// Kills the commands of `run_silent`, `capture` and `stream` still in progress and refuses new
    /// ones (the daemon at shutdown). A privileged command that cannot be signalled is left to end.
    /// The fakes do nothing.
    fn stop_running(&self) {}

    /// Launches `prog args…` as our own child, for as long as it chooses to run (an annotation
    /// editor, a screen recorder), and returns it without waiting: its end is told by a descriptor
    /// the caller's event loop watches (`Watched::exit_fd`), so no thread and no timer wait for it.
    /// Standard input `/dev/null`; standard output to the file `stdout` (created owner-only when
    /// missing, emptied first) or `/dev/null`; standard error `/dev/null`; the caller's environment
    /// as is (a program with a window needs the user's locale); its own process group; SIGINT when
    /// the launching thread dies (PDEATHSIG), so a recorder finishes its file rather than run on
    /// with nobody to stop it. Not registered in `Running`: `stop_running` does not reach it, its
    /// owner ends it (a recorder deserves SIGINT and time to finish its file, not SIGKILL). Refused
    /// once `stop_running` was called. Not supported by default.
    fn spawn_watched(&self, _prog: &str, _args: &[&str], _stdout: Option<&std::path::Path>) -> Result<Box<dyn Watched>, ExecError> {
        Err(ExecError::Io("watched programs are not supported".into()))
    }
}

/// A program started by `spawn_watched`. It is our child until it is reaped here, so its pid, which
/// is also its process group's, cannot be reused while this lives: a signal sent through it never
/// reaches a stranger. Dropping it while the program runs kills the program's group (SIGKILL) and
/// reaps it, unless it was released (`release`).
pub trait Watched: Send {
    /// A descriptor that turns readable once the program has ended (a duplicate of its pidfd), for an
    /// event loop to watch; None where there is none (the fakes).
    fn exit_fd(&self) -> Option<std::os::fd::OwnedFd>;
    /// Its end once it has ended: `Some(code)`, the code None when a signal ended it; None while it
    /// runs. The first call that sees the end reaps it; the later ones give the same answer.
    fn try_exit(&mut self) -> Option<Option<i32>>;
    /// Sends SIGINT to the program's own process group (the program and what it started, never
    /// another group). False when it has already ended: nothing is sent.
    fn interrupt(&mut self) -> bool;
    /// Waits at most `limit` for the program's end (on its descriptor: no busy loop), then answers
    /// as `try_exit`. Blocks: only where blocking is the point (the daemon's shutdown).
    fn wait_exit(&mut self, limit: Duration) -> Option<Option<i32>>;
    /// Lets the program run on with nobody watching: the handle goes without the kill a drop sends.
    /// For the daemon's shutdown, once a recorder it interrupted has had its time and may still be
    /// finishing its file: a kill would leave the file unplayable. The program is not reaped here; it
    /// stays our child until it ends or we exit.
    fn release(self: Box<Self>);
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
        let mut guard = self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// Sends `signal` to the process group `pid` leads, or to `pid` alone when that group is gone
/// (ESRCH: the program moved to another group): a stop still reaches it. Only for our own child not
/// yet reaped, whose pid therefore cannot be another process's. True when the signal was sent.
fn signal_group_else_pid(pid: u32, signal: libc::c_int) -> bool {
    let Some(target) = group_target(pid) else { return false };
    // SAFETY: kill(2) towards the group our child leads, created at its launch; no pointer.
    if unsafe { libc::kill(target, signal) } == 0 {
        return true;
    }
    if std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return false;
    }
    // SAFETY: kill(2) towards our unreaped child itself (`-target` is its pid, above 1); no pointer.
    unsafe { libc::kill(-target, signal) == 0 }
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

/// Blocks until the child `pid` has ended, without reaping it (WNOWAIT).
fn wait_exited(pid: u32) {
    let id = libc::id_t::from(pid);
    loop {
        // SAFETY: siginfo_t is a plain C struct, valid when zero-filled.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid struct that lives for the duration of the call.
        let r = unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if r == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
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
/// closure runs, and both forked copies inherit them identically. The
/// grandchild then calls `setsid`: its own session (fully detached: docs/decisions.md
/// "A launched program is never a child of the bar"), so no terminal or session leader of
/// ours can ever send it SIGHUP. It is not a group leader (the intermediate
/// is), so `setsid` cannot fail with EPERM; async-signal-safe.
fn detach() -> std::io::Result<()> {
    unblock_signals()?;
    // SAFETY: fork(2), setsid(2) and _exit(2) are async-signal-safe; nothing
    // allocates and no `Drop` runs between the fork and the `_exit`/return below.
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error()),
        0 => {
            // Grandchild: its own session, then proceed to exec `prog`.
            // SAFETY: setsid(2) takes no argument; the grandchild is not a group leader.
            if unsafe { libc::setsid() } == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
        // intermediate: nothing left to do.
        // SAFETY: _exit(2) ends the process at once, running no destructor or handler.
        _ => unsafe { libc::_exit(0) },
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
/// thread behind for as long as the program runs. The grandchild's
/// protection is now its own session (`setsid` in `detach`), not merely a
/// process group as before the double fork; the intermediate below still
/// gets its own process group, though it only lives long enough to fork
/// again and exit. Signal mask unblocked, stdio nulled, not killed by
/// `kill_running` (never registered in `Running`: a spawned program
/// outlives the daemon, e.g. the lock screen must survive alpheus
/// restarting).
fn spawn_detached(prog: &str, args: &[&str]) -> Result<u32, ExecError> {
    let mut intermediate = detached_command(prog, args, Stdio::null()).spawn().map_err(io_err)?;
    let pid = intermediate.id();
    let _ = intermediate.wait();
    Ok(pid)
}

/// An environment `Command::envs` can carry: names non-empty without `=` or NUL, values without NUL.
pub(crate) fn env_ok(env: &[(&str, &str)]) -> bool {
    env.iter().all(|(k, v)| !k.is_empty() && !k.contains(['=', '\0']) && !v.contains('\0'))
}

/// `spawn_detached` with `env` added to the inherited environment.
fn spawn_detached_with_env(prog: &str, args: &[&str], env: &[(&str, &str)], unset: &[&str]) -> Result<u32, ExecError> {
    if !env_ok(env) || unset.iter().any(|k| k.is_empty() || k.contains(['=', '\0'])) {
        return Err(ExecError::Io("invalid environment variable".into()));
    }
    let mut command = detached_command(prog, args, Stdio::null());
    for k in unset {
        command.env_remove(k);
    }
    let mut intermediate = command.envs(env.iter().copied()).spawn().map_err(io_err)?;
    let pid = intermediate.id();
    let _ = intermediate.wait();
    Ok(pid)
}

/// `spawn_detached`'s command: standard input as given, output and errors nulled, the intermediate's
/// own process group, then `detach` in the child (double fork, own session).
fn detached_command(prog: &str, args: &[&str], stdin: Stdio) -> Command {
    let mut command = Command::new(prog);
    command
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // The intermediate's own process group (it only lives long enough
        // to fork again and exit; the grandchild's protection is its own
        // session instead, via setsid in `detach`).
        .process_group(0);
    // SAFETY: `detach` runs in the child between fork and exec and only
    // makes async-signal-safe calls (sigprocmask, fork, setsid, _exit).
    unsafe { command.pre_exec(detach) };
    command
}

/// `spawn_detached` with a standard input: a pipe, its read end inherited by the grandchild across
/// the double fork; the parts written once the program is started (the pipe holds them all,
/// MAX_INPUT being one page), then the write end dropped: the program's end of file.
fn spawn_detached_with_input(prog: &str, args: &[&str], input: &[&[u8]]) -> Result<u32, ExecError> {
    let total: usize = input.iter().map(|p| p.len()).sum();
    if total > MAX_INPUT {
        return Err(ExecError::Io(format!("standard input of {total} bytes refused (at most {MAX_INPUT})")));
    }
    let mut intermediate = detached_command(prog, args, Stdio::piped()).spawn().map_err(io_err)?;
    let pid = intermediate.id();
    let written = match intermediate.stdin.take() {
        Some(mut stdin) => input.iter().try_for_each(|part| stdin.write_all(part)),
        None => Ok(()),
    };
    let _ = intermediate.wait();
    written.map_err(|e| ExecError::Io(e.to_string()))?;
    Ok(pid)
}

/// How `read_until` stopped.
enum Stop {
    Exited,
    TimedOut,
    Capped,
}

/// How often a quiet read checks whether the command already ended (a child it left behind may
/// keep the pipe open long after): only while a capture runs, never at rest.
const EXIT_CHECK: Duration = Duration::from_millis(25);

/// Where the read is cut once a cap is passed: just after the `max_lines`-th line when anything
/// follows it, or at `max_bytes`. Incremental: the buffer only grows between calls, and each byte is
/// scanned for newlines once (a line per write stays linear).
#[derive(Default)]
struct Cutter {
    /// Bytes already scanned, and the newlines among them.
    scanned: usize,
    lines: usize,
    /// Just after the `max_lines`-th newline, once seen.
    line_end: Option<usize>,
}

impl Cutter {
    /// The cut for `bytes` (the previous call's bytes plus what was read since), None while within
    /// both caps.
    fn cut(&mut self, bytes: &[u8], limits: &CaptureLimits) -> Option<usize> {
        if limits.max_lines == 0 {
            self.line_end = Some(0);
        }
        if self.line_end.is_none() {
            for (i, b) in bytes[self.scanned..].iter().enumerate() {
                if *b == b'\n' {
                    self.lines += 1;
                    if self.lines == limits.max_lines {
                        self.line_end = Some(self.scanned + i + 1);
                        break;
                    }
                }
            }
            self.scanned = bytes.len();
        }
        match self.line_end {
            Some(end) if bytes.len() > end => Some(end.min(limits.max_bytes)),
            _ if bytes.len() > limits.max_bytes => Some(limits.max_bytes),
            _ => None,
        }
    }
}

/// The cut for a whole buffer at once: the reference the incremental cut is tested against.
#[cfg(test)]
fn cut_at(bytes: &[u8], limits: &CaptureLimits) -> Option<usize> {
    Cutter::default().cut(bytes, limits)
}

/// Waits up to `ms` for `fd` to be readable: true when a read would not block (data, end of file
/// or an error to read), false when quiet or interrupted.
fn readable(fd: std::os::fd::RawFd, ms: i32) -> Result<bool, ()> {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd, alive for the duration of the call.
    match unsafe { libc::poll(&mut p, 1, ms) } {
        r if r > 0 => Ok(true),
        0 => Ok(false),
        _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => Ok(false),
        _ => Err(()),
    }
}

/// Reads `out` until the command `pid` ends, the deadline (from `start`) passes or a cap is passed.
fn read_until(out: &mut ChildStdout, pid: u32, start: Instant, limits: &CaptureLimits) -> (Vec<u8>, Stop) {
    use std::os::fd::AsRawFd;
    let fd = out.as_raw_fd();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut cutter = Cutter::default();
    let mut eof = false;
    // The command is gone and its group killed: no wait any more, but what was written before may
    // still sit in the pipe — read until end of file or until nothing is ready. A write racing the
    // kill (between the last poll(0) and the SIGKILL's delivery) is lost; no test can force that
    // window, and those bytes came after the command's own end anyway.
    let mut draining = false;
    // When the command's exit was last checked: at least every EXIT_CHECK, quiet pipe or not, so a
    // child it left behind writing on and on cannot keep the read going.
    let mut checked = Instant::now();
    loop {
        let Some(left) = limits.deadline.checked_sub(start.elapsed()).filter(|d| !d.is_zero()) else {
            return (bytes, Stop::TimedOut);
        };
        if eof {
            if draining {
                return (bytes, Stop::Exited);
            }
            // The pipe closed: the command itself must still end within the deadline.
            match exited(pid) {
                Ok(false) => std::thread::sleep(left.min(EXIT_CHECK)),
                _ => return (bytes, Stop::Exited),
            }
            continue;
        }
        if !draining && checked.elapsed() >= EXIT_CHECK {
            checked = Instant::now();
            draining = ended(pid);
        }
        let ms = if draining { 0 } else { i32::try_from(left.min(EXIT_CHECK).as_millis()).unwrap_or(i32::MAX).max(1) };
        match readable(fd, ms) {
            Err(()) => eof = true,
            Ok(false) if draining => return (bytes, Stop::Exited),
            // Quiet: the command may be gone while something it started keeps the pipe open.
            Ok(false) => {
                checked = Instant::now();
                draining = ended(pid);
            }
            Ok(true) => match out.read(&mut chunk) {
                Ok(0) => eof = true,
                Ok(n) => {
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(at) = cutter.cut(&bytes, limits) {
                        bytes.truncate(at);
                        return (bytes, Stop::Capped);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => eof = true,
            },
        }
    }
}

/// Has the command `pid` ended? If so its group is killed at once: whatever it left behind stops
/// writing, and the drain that follows meets the end of the pipe. The leader is not reaped yet
/// (`exited` waits with WNOWAIT): its PID, the group's, is still ours.
fn ended(pid: u32) -> bool {
    match exited(pid) {
        Ok(true) => {
            kill_group(pid);
            true
        }
        Ok(false) => false,
        // waitid failing on our own unreaped child: stop waiting for it, read what is there.
        Err(_) => true,
    }
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

    fn spawn_with_input(&self, prog: &str, args: &[&str], input: &[&[u8]]) -> Result<(), ExecError> {
        spawn_detached_with_input(prog, args, input).map(|_intermediate_pid| ())
    }

    fn run_with_stdin_file(&self, prog: &str, args: &[&str], file: &std::path::Path, limit: Duration) -> Result<Option<i32>, ExecError> {
        if self.running.lock().unwrap_or_else(|p| p.into_inner()).closed {
            return Err(ExecError::Io("alpheus is shutting down".into()));
        }
        let input = std::fs::File::open(file).map_err(|e| ExecError::Io(e.to_string()))?;
        let mut command = Command::new(prog);
        command
            .args(args)
            // As `run`: untranslated, whatever the user's locale.
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::from(input))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        // The daemon blocks SIGTERM, SIGHUP and SIGINT in every thread: without this the program, and
        // what it leaves behind (`wl-copy` holding the clipboard), would ignore them for life.
        // SAFETY: `unblock_signals` only makes async-signal-safe calls.
        unsafe { command.pre_exec(unblock_signals) };
        let mut child = command.spawn().map_err(io_err)?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status.code()),
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ExecError::Io(e.to_string()));
                }
            }
            if start.elapsed() >= limit {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExecError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn spawn_with_env(&self, prog: &str, args: &[&str], env: &[(&str, &str)], unset: &[&str]) -> Result<(), ExecError> {
        spawn_detached_with_env(prog, args, env, unset).map(|_intermediate_pid| ())
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
    fn capture(&self, prog: &str, args: &[&str], limits: CaptureLimits) -> Result<Captured, ExecError> {
        if self.running.lock().unwrap_or_else(|p| p.into_inner()).closed {
            return Err(ExecError::Io("alpheus is shutting down".into()));
        }
        let parent = std::process::id();
        let start = Instant::now();
        let mut command = Command::new(prog);
        command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
        // SAFETY: the closure runs in the child between fork and exec and only makes
        // async-signal-safe calls (sigprocmask, prctl, getppid, _exit), as in `stream`.
        unsafe {
            command.pre_exec(move || {
                unblock_signals()?;
                // The reading thread gone (the daemon killed mid-read): the command goes too.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() as u32 != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(io_err)?;
        let pid = child.id();
        let Some(mut stdout) = child.stdout.take() else {
            kill_group(pid);
            let _ = child.wait();
            return Err(ExecError::Io("no standard output".into()));
        };
        {
            let mut guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
            if guard.closed {
                // Shutdown between the check and the spawn (see `run`).
                drop(guard);
                kill_group(pid);
                let _ = child.wait();
                return Err(ExecError::Io("alpheus is shutting down".into()));
            }
            guard.pids.insert(pid);
        }
        let (bytes, stop) = read_until(&mut stdout, pid, start, &limits);
        {
            // Never panic here: a poisoned lock still holds a valid set. Still listed = neither
            // reaped nor killed by `kill_running`: its PID (= PGID) is still ours.
            let mut guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
            if guard.pids.remove(&pid) {
                kill_group(pid);
            }
        }
        drop(stdout);
        let status = child.wait().map_err(|e| ExecError::Io(e.to_string()))?;
        let end = match stop {
            Stop::Exited => CaptureEnd::Exited(status.code()),
            Stop::TimedOut => CaptureEnd::TimedOut,
            Stop::Capped => CaptureEnd::Capped,
        };
        Ok(Captured { stdout: bytes, end })
    }

    fn run_silent(&self, prog: &str, args: &[&str]) -> Result<Option<i32>, ExecError> {
        if self.running.lock().unwrap_or_else(|p| p.into_inner()).closed {
            return Err(ExecError::Io("alpheus is shutting down".into()));
        }
        let parent = std::process::id();
        let mut command = Command::new(prog);
        command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
        // SAFETY: as in `capture`: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                unblock_signals()?;
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() as u32 != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(io_err)?;
        let pid = child.id();
        {
            let mut guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
            if guard.closed {
                drop(guard);
                kill_group(pid);
                let _ = child.wait();
                return Err(ExecError::Io("alpheus is shutting down".into()));
            }
            guard.pids.insert(pid);
        }
        // Without reaping (WNOWAIT): the PID stays ours until it leaves the list, then it is reaped.
        wait_exited(pid);
        self.running.lock().unwrap_or_else(|p| p.into_inner()).pids.remove(&pid);
        let status = child.wait().map_err(|e| ExecError::Io(e.to_string()))?;
        Ok(status.code())
    }

    fn stop_running(&self) {
        self.kill_running();
    }

    fn spawn_watched(&self, prog: &str, args: &[&str], stdout: Option<&std::path::Path>) -> Result<Box<dyn Watched>, ExecError> {
        if self.running.lock().unwrap_or_else(|p| p.into_inner()).closed {
            return Err(ExecError::Io("alpheus is shutting down".into()));
        }
        let out = match stdout {
            Some(path) => {
                use std::os::unix::fs::OpenOptionsExt;
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(path)
                    .map_err(|e| ExecError::Io(e.to_string()))?;
                Stdio::from(file)
            }
            None => Stdio::null(),
        };
        let parent = std::process::id();
        let mut command = Command::new(prog);
        command.args(args).stdin(Stdio::null()).stdout(out).stderr(Stdio::null()).process_group(0);
        // SAFETY: as in `capture`: only async-signal-safe calls between fork and exec (sigprocmask,
        // prctl, getppid, _exit).
        unsafe {
            command.pre_exec(move || {
                unblock_signals()?;
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGINT) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() as u32 != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(io_err)?;
        let pid = child.id();
        // Without a pidfd nobody could hear the end without a thread or a timer: the program is
        // given up at once rather than left unwatched.
        let Some(pidfd) = pidfd_open(pid) else {
            kill_group(pid);
            let _ = child.wait();
            return Err(ExecError::Io("no pidfd for the program".into()));
        };
        Ok(Box::new(WatchedChild { child, pidfd, ended: None, released: false }))
    }
}

/// A pidfd for our child `pid` (Linux 5.3 or later), None when the kernel cannot give one.
fn pidfd_open(pid: u32) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let pid = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: pidfd_open(2) takes a pid and flags, no pointer; a result of 0 or more is a new
    // descriptor that nobody else owns, so `OwnedFd` may take it.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let fd = libc::c_int::try_from(fd).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: `fd` was just returned by the kernel to us alone (see above).
    Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// `spawn_watched`'s program: the child, its pidfd, and its end once reaped.
struct WatchedChild {
    child: Child,
    pidfd: std::os::fd::OwnedFd,
    ended: Option<Option<i32>>,
    /// Let run on (`release`): the drop leaves it alone.
    released: bool,
}

impl Watched for WatchedChild {
    fn exit_fd(&self) -> Option<std::os::fd::OwnedFd> {
        self.pidfd.try_clone().ok()
    }

    fn try_exit(&mut self) -> Option<Option<i32>> {
        if self.ended.is_none() {
            // An error of waitpid on our own child cannot leave it running: it is counted as gone.
            match self.child.try_wait() {
                Ok(Some(status)) => self.ended = Some(status.code()),
                Ok(None) => {}
                Err(_) => self.ended = Some(None),
            }
        }
        self.ended
    }

    fn interrupt(&mut self) -> bool {
        if self.try_exit().is_some() {
            return false;
        }
        // Not reaped (try_exit just saw it running): its pid, its group's, is still ours.
        signal_group_else_pid(self.child.id(), libc::SIGINT)
    }

    fn wait_exit(&mut self, limit: Duration) -> Option<Option<i32>> {
        use std::os::fd::AsRawFd;
        let start = Instant::now();
        while self.try_exit().is_none() {
            let Some(left) = limit.checked_sub(start.elapsed()).filter(|d| !d.is_zero()) else { break };
            let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
            // Readable means ended; quiet means the limit passed; an error is read as an end below.
            if readable(self.pidfd.as_raw_fd(), ms).is_err() {
                break;
            }
        }
        self.try_exit()
    }

    fn release(mut self: Box<Self>) {
        self.released = true;
    }
}

impl Drop for WatchedChild {
    fn drop(&mut self) {
        if !self.released && self.try_exit().is_none() {
            // Not reaped: the group is still the program's own, and its pid too if it left the group,
            // so the wait below never waits for a program the kill missed.
            signal_group_else_pid(self.child.id(), libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests;
