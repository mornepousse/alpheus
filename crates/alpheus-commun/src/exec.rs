//! Injectable command executor: the ONLY point through which alpheus
//! launches a system tool. Tests inject `fake::FakeExecutor`, never the real one.

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

/// What `capture` reads at most, and for how long (launcher sources spec § 2).
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
    /// Launches `prog args…` without waiting (e.g. hyprlock, or whatever a
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
/// closure runs, and both forked copies inherit them identically. The
/// grandchild then calls `setsid`: its own session (spec of the launcher,
/// § 1, "fully detached, own session"), so no terminal or session leader of
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
            if unsafe { libc::setsid() } == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
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
    // child it left behind writing on and on cannot keep the read going (final review A1).
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

    /// Quick Wi-Fi spec § 3: the parts reach the program's standard input in order, then its end
    /// (`cat` ends only on it): never an argument.
    #[test]
    fn spawn_with_input_writes_the_parts_then_closes_stdin() {
        let dir = std::env::temp_dir().join(format!("alpheus-input-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (got, done) = (dir.join("got"), dir.join("done"));
        let script = format!("cat > '{}' && mv '{}' '{}'", got.display(), got.display(), done.display());
        exec().spawn_with_input("sh", &["-c", &script], &[b"fake-pass", b"\n"]).unwrap();
        let t = Instant::now();
        while !done.exists() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read(&done).unwrap(), b"fake-pass\n", "both parts, then the end of file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// More than MAX_INPUT bytes: refused before anything starts; MAX_INPUT itself goes.
    #[test]
    fn spawn_with_input_refuses_more_than_max_input_before_starting() {
        let dir = std::env::temp_dir().join(format!("alpheus-input-max-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("started");
        let touch = format!("touch '{}'", marker.display());
        let big = vec![b'x'; MAX_INPUT + 1];
        assert!(matches!(exec().spawn_with_input("sh", &["-c", &touch], &[&big]), Err(ExecError::Io(_))));
        let half = vec![b'x'; MAX_INPUT / 2 + 1];
        assert!(
            matches!(exec().spawn_with_input("sh", &["-c", &touch], &[&half, &half]), Err(ExecError::Io(_))),
            "counted over every part"
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "nothing started");
        let at_max = vec![b'x'; MAX_INPUT];
        assert_eq!(exec().spawn_with_input("sh", &["-c", "cat > /dev/null"], &[&at_max]), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_with_input_of_a_missing_program_is_not_found() {
        assert_eq!(exec().spawn_with_input("alpheus-nonexistent-program", &[], &[b"x"]), Err(ExecError::NotFound));
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

    /// Session id of process `pid`: field 6 of /proc/<pid>/stat, counted
    /// after the command name's closing parenthesis (a name may hold spaces).
    fn session_of(pid: i32) -> i32 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("process gone");
        let after = &stat[stat.rfind(')').unwrap() + 2..];
        after.split(' ').nth(3).unwrap().parse().unwrap()
    }

    /// The program `spawn` starts leads its own session (`setsid` in the
    /// grandchild): neither our session nor our terminal's, so closing
    /// whatever started alpheus never sends it SIGHUP.
    #[test]
    fn spawned_child_has_its_own_session() {
        let f = std::env::temp_dir().join(format!("alpheus-spawn-{}-session", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("echo $$ > {}; sleep 1", f.display());
        exec().spawn("sh", &["-c", &script]).unwrap();
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        let ours = session_of(std::process::id() as i32);
        assert_eq!(session_of(pid), pid, "the program leads its own session");
        assert_ne!(session_of(pid), ours, "not in our session");
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

    fn limits(deadline_ms: u64) -> CaptureLimits {
        CaptureLimits { deadline: Duration::from_millis(deadline_ms), max_bytes: 1024, max_lines: 10 }
    }

    #[test]
    fn capture_reads_the_output_and_the_exit_code_never_stderr() {
        let c = exec().capture("sh", &["-c", "printf 'a\\nb\\n'; echo noise >&2; exit 3"], limits(2000)).unwrap();
        assert_eq!(c, Captured { stdout: b"a\nb\n".to_vec(), end: CaptureEnd::Exited(Some(3)) });
    }

    /// Unlike `run` and `stream`: what a source prints is shown, not parsed — its locale is the user's.
    #[test]
    fn capture_keeps_the_callers_locale() {
        let c = exec().capture("sh", &["-c", "printf %s \"${LC_ALL-unset}\""], limits(2000)).unwrap();
        let expected = std::env::var("LC_ALL").unwrap_or_else(|_| "unset".into());
        assert_eq!(String::from_utf8(c.stdout).unwrap(), expected);
    }

    #[test]
    fn capture_keeps_what_came_before_the_deadline_and_kills_the_group() {
        let f = std::env::temp_dir().join(format!("alpheus-grandchild-{}-capture-deadline", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("echo early; sleep 30 & echo $! > {}; wait", f.display());
        let t = Instant::now();
        let c = exec().capture("sh", &["-c", &script], limits(300)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert_eq!(c, Captured { stdout: b"early\n".to_vec(), end: CaptureEnd::TimedOut });
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        assert!(pid > 1);
        wait_for_death(pid);
    }

    /// Review Focus 2: a command that leaves a child holding its standard output behind ends the
    /// read when IT ends, not at the deadline; the child goes with the group.
    #[test]
    fn capture_ends_with_the_command_even_if_a_child_keeps_the_pipe() {
        let f = std::env::temp_dir().join(format!("alpheus-grandchild-{}-capture-exit", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("echo done; sleep 30 & echo $! > {}; exit 0", f.display());
        let t = Instant::now();
        let c = exec().capture("sh", &["-c", &script], limits(5000)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1), "stopped at the deadline, not at the exit: {:?}", t.elapsed());
        assert_eq!(c, Captured { stdout: b"done\n".to_vec(), end: CaptureEnd::Exited(Some(0)) });
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        wait_for_death(pid);
    }

    /// Final review A1: a command that leaves a chatty child behind (it writes on and on) ends the
    /// read when IT ends: its exit is checked while data flows too, then the group is killed. What
    /// the child wrote before that exit is indistinguishable from the command's own lines.
    #[test]
    fn capture_ends_with_the_command_even_if_a_child_keeps_writing() {
        let f = std::env::temp_dir().join(format!("alpheus-grandchild-{}-capture-chatty", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("(while :; do echo orphan; sleep 0.002; done) & echo $! > {}; sleep 0.1; echo own; exit 0", f.display());
        let big = CaptureLimits { deadline: Duration::from_millis(3000), max_bytes: 1 << 20, max_lines: 100_000 };
        let t = Instant::now();
        let c = exec().capture("sh", &["-c", &script], big).unwrap();
        let elapsed = t.elapsed();
        let pid = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        wait_for_death(pid);
        assert!(elapsed < Duration::from_secs(1), "read to the deadline, not to the exit: {elapsed:?}");
        assert_eq!(c.end, CaptureEnd::Exited(Some(0)));
        let out = String::from_utf8(c.stdout).unwrap();
        assert!(out.lines().all(|l| l == "orphan" || l == "own") && out.contains("own\n"), "{out:?}");
    }

    /// Once the command is seen gone, its group is killed before the drain: a child it left behind
    /// writing on and on could otherwise keep the drain reading.
    #[test]
    fn a_command_seen_gone_takes_its_group_with_it() {
        use std::os::unix::process::CommandExt;
        let f = std::env::temp_dir().join(format!("alpheus-grandchild-{}-ended", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("sleep 30 & echo $! > {}; exit 0", f.display());
        let mut child = Command::new("sh").args(["-c", &script]).process_group(0).spawn().unwrap();
        let pid = child.id();
        let t = Instant::now();
        while !exited(pid).unwrap() {
            assert!(t.elapsed() < Duration::from_secs(5), "sh never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        let orphan = pid_written(&f);
        let _ = std::fs::remove_file(&f);
        assert!(alive(orphan), "left behind, still running");
        assert!(ended(pid));
        wait_for_death(orphan);
        child.wait().unwrap();
    }

    /// Final review A5: a lock poisoned by a panic elsewhere never makes `capture` panic (only the
    /// launcher's scan thread calls it, and a panic after the spawn would leave a zombie behind).
    #[test]
    fn capture_survives_a_poisoned_lock() {
        let e = exec();
        let running = e.running.clone();
        let poisoner = std::thread::spawn(move || {
            let _guard = running.lock().unwrap();
            panic!("poisoning the lock on purpose");
        });
        assert!(poisoner.join().is_err());
        assert!(e.running.is_poisoned());
        let c = e.capture("sh", &["-c", "echo still"], limits(2000)).unwrap();
        assert_eq!(c, Captured { stdout: b"still\n".to_vec(), end: CaptureEnd::Exited(Some(0)) });
    }

    #[test]
    fn capture_stops_at_the_caps() {
        let lines = exec().capture("sh", &["-c", "i=0; while :; do i=$((i+1)); echo \"l$i\"; done"], limits(5000)).unwrap();
        assert_eq!(lines.end, CaptureEnd::Capped);
        assert_eq!(String::from_utf8(lines.stdout).unwrap(), (1..=10).map(|i| format!("l{i}\n")).collect::<String>());
        let bytes = exec().capture("sh", &["-c", "while :; do printf xxxxxxxx; done"], limits(5000)).unwrap();
        assert_eq!((bytes.stdout.len(), bytes.end), (1024, CaptureEnd::Capped));
    }

    #[test]
    fn capture_of_a_missing_program_is_not_found_and_none_after_closing() {
        assert_eq!(exec().capture("alpheus-nonexistent-program", &[], limits(1000)), Err(ExecError::NotFound));
        let e = exec();
        e.kill_running();
        assert!(matches!(e.capture("sh", &["-c", "true"], limits(1000)), Err(ExecError::Io(_))));
    }

    #[test]
    fn cut_at_follows_both_caps() {
        let l = |max_lines, max_bytes| CaptureLimits { deadline: Duration::ZERO, max_bytes, max_lines };
        assert_eq!(cut_at(b"a\nb\nc", &l(2, 100)), Some(4), "something after the second line: cut after it");
        assert_eq!(cut_at(b"a\nb\n", &l(2, 100)), None, "exactly two lines: not past the cap yet");
        assert_eq!(cut_at(b"abcdef", &l(10, 4)), Some(4), "past the byte cap");
        assert_eq!(cut_at(b"abcd", &l(10, 4)), None);
        assert_eq!(cut_at(b"a\nb\nc", &l(0, 100)), Some(0));
    }

    /// Final review A2: the cut is found incrementally (each byte scanned once across chunks), and
    /// it lands where a scan of the whole buffer would, a line or a byte per write alike.
    #[test]
    fn the_cut_found_chunk_by_chunk_is_the_whole_buffer_s() {
        let l = |max_lines, max_bytes| CaptureLimits { deadline: Duration::ZERO, max_bytes, max_lines };
        let text: Vec<u8> = (1..=40).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
        for limits in [l(10, 1 << 20), l(1, 1 << 20), l(0, 100), l(1000, 50), l(5, 30)] {
            let per_line: Vec<&[u8]> = text.split_inclusive(|b| *b == b'\n').collect();
            let writes = [1, 3, 8].map(|step| text.chunks(step).collect::<Vec<_>>());
            for (n, chunks) in writes.iter().chain([&per_line]).enumerate() {
                let mut cutter = Cutter::default();
                let mut bytes = Vec::new();
                let mut got = None;
                for chunk in chunks {
                    bytes.extend_from_slice(chunk);
                    if let Some(at) = cutter.cut(&bytes, &limits) {
                        got = Some(at);
                        break;
                    }
                }
                let expected = (1..=text.len()).find_map(|n| cut_at(&text[..n], &limits));
                assert_eq!(got, expected, "{} lines, {} bytes, writes #{n}", limits.max_lines, limits.max_bytes);
            }
        }
    }
}
