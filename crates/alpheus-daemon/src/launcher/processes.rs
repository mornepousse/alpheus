//! The kill list's pure half: a snapshot of the user's processes read from `/proc`, the rows built
//! from one or two snapshots, and the signal that checks a pid's start time before it is sent.
//!
//! `snapshot` reads a `/proc`-shaped folder (the real one, or a temporary one in a test) and keeps
//! only the processes of one user, at most `MAX_PROCESSES` of them. `rows` turns a snapshot into the
//! list the box shows: the processes that may be offered, sorted by memory, or by CPU once a second
//! snapshot gave a figure. `send` is the only way a signal leaves this file, and it goes through a
//! `Signaller` so that a test records the signal instead of sending it. The other half,
//! `processes_app.rs`, holds the box's state, the second sample and the real `Signaller`.
//!
//! Never:
//! - offer PID 1, alpheus itself, the session leader, the compositor or the user manager: `rows`
//!   drops them (`pid_1_alpheus_and_the_session_leader_are_never_offered`);
//! - signal a pid whose start time is not the one that was listed: a pid reused between the
//!   listing and the key press would hit the wrong program (`a_reused_pid_is_never_signalled`);
//! - pass `kill(2)` a pid of 1 or below, which would reach init or a whole group
//!   (`a_pid_that_could_reach_a_group_or_init_is_refused`);
//! - read more than `MAX_PROCESSES` processes, or more than `CMDLINE_MAX` bytes of a command line;
//! - write a name, a pid or a command line into a log line: nothing here logs.
//!
//! See: docs/features/tools-menu.md ("Kill a process"), docs/decisions.md ("Only the user's own
//! processes are offered, and a reused pid is never signalled").

use std::fs;
use std::io::Read;
use std::path::Path;

/// Most processes one snapshot keeps.
pub const MAX_PROCESSES: usize = 4096;
/// Most bytes of a command line kept, and shown.
pub const CMDLINE_MAX: usize = 512;

/// One process as `/proc` described it at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcSample {
    pub pid: i32,
    /// Field 22 of `stat`: when the process started, in clock ticks after boot. With the pid it
    /// names one process for ever; the pid alone can be reused.
    pub start: u64,
    pub name: String,
    pub cmdline: String,
    pub rss_kb: u64,
    /// User plus system time used so far, in clock ticks.
    pub ticks: u64,
    /// The machine's uptime when this sample was read, in hundredths of a second.
    pub uptime_cs: u64,
}

/// One line of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub pid: i32,
    pub start: u64,
    pub name: String,
    pub cmdline: String,
    pub rss_kb: u64,
    /// Share of one core since the previous snapshot: None until there is one.
    pub cpu_pct: Option<f32>,
}

/// What `rows` never offers, besides PID 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excluded {
    pub pids: Vec<i32>,
    /// Process names (the `comm` of `stat`).
    pub comms: Vec<String>,
}

/// The kernel's page size in kilobytes (`statm` counts pages).
pub fn page_kb() -> u64 {
    // SAFETY: sysconf(3) only reads a system constant and takes no pointer.
    let bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(bytes).unwrap_or(4096) / 1024
}

/// The kernel's clock ticks per second (`stat` times are in ticks).
pub fn clock_ticks_per_second() -> u64 {
    // SAFETY: sysconf(3) only reads a system constant and takes no pointer.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100)
}

/// The processes of user `uid` under `root` (`/proc`), at most `max` of them, in directory order.
/// A process that goes away between the reads, or whose files do not parse, is skipped.
pub fn snapshot(root: &Path, uid: u32, max: usize) -> Vec<ProcSample> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let uptime_cs = read_uptime_cs(root).unwrap_or(0);
    let page = page_kb();
    let mut samples = Vec::new();
    for entry in entries.flatten() {
        if samples.len() >= max {
            break;
        }
        // Only the numeric folders are processes (`/proc/sys`, `/proc/net`... are not).
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        if let Some(sample) = read_process(&entry.path(), pid, uid, page, uptime_cs) {
            samples.push(sample);
        }
    }
    samples
}

/// The first number of `uptime`, in hundredths of a second.
fn read_uptime_cs(root: &Path) -> Option<u64> {
    let text = fs::read_to_string(root.join("uptime")).ok()?;
    let seconds: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some((seconds * 100.0).round() as u64)
}

/// One process folder as a sample; None when it is not `uid`'s or a file is missing or odd.
fn read_process(dir: &Path, pid: i32, uid: u32, page_kb: u64, uptime_cs: u64) -> Option<ProcSample> {
    // The owner first: most of what `/proc` lists belongs to someone else and costs one read.
    let status = fs::read_to_string(dir.join("status")).ok()?;
    if owner(&status)? != uid {
        return None;
    }
    let stat = parse_stat(&fs::read(dir.join("stat")).ok()?)?;
    let statm = fs::read_to_string(dir.join("statm")).ok()?;
    // `statm`: size, then resident pages.
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(ProcSample {
        pid,
        start: stat.start,
        name: stat.name,
        cmdline: read_cmdline(&dir.join("cmdline")),
        rss_kb: resident.saturating_mul(page_kb),
        ticks: stat.ticks,
        uptime_cs,
    })
}

/// The real user id of a `status` file: the first number of its `Uid:` line.
fn owner(status: &str) -> Option<u32> {
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// What the list needs of a `stat` line.
struct Stat {
    name: String,
    ppid: i32,
    ticks: u64,
    start: u64,
}

/// A `stat` line: `pid (comm) state field4 field5 ...`. The name may hold spaces and
/// parentheses, so it ends at the last `)`; the fields count from the one after it.
fn parse_stat(bytes: &[u8]) -> Option<Stat> {
    let text = String::from_utf8_lossy(bytes);
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    // `after[0]` is the state (field 3), so field `n` is `after[n - 3]`.
    let after: Vec<&str> = text[close + 1..].split_whitespace().collect();
    let field = |n: usize| -> Option<u64> { after.get(n - 3)?.parse().ok() };
    // A name is cleaned like a command line: `prctl(PR_SET_NAME)` can put an escape or a newline in it.
    let name = text[open + 1..close].chars().filter(|c| !c.is_control()).collect();
    let ppid = i32::try_from(field(4)?).ok()?;
    Some(Stat { name, ppid, ticks: field(14)?.saturating_add(field(15)?), start: field(22)? })
}

/// A command line: its arguments joined by spaces, at most `CMDLINE_MAX` bytes read, with the
/// control characters (an escape sequence in an argument) taken out. Empty when unreadable.
fn read_cmdline(path: &Path) -> String {
    let Ok(file) = fs::File::open(path) else { return String::new() };
    let mut raw = Vec::with_capacity(CMDLINE_MAX);
    if file.take(CMDLINE_MAX as u64).read_to_end(&mut raw).is_err() {
        return String::new();
    }
    // Arguments are NUL-separated, and the last one ends with a NUL too.
    let spaced: Vec<u8> = raw.iter().map(|b| if *b == 0 { b' ' } else { *b }).collect();
    let text = String::from_utf8_lossy(&spaced);
    text.trim_end().chars().filter(|c| !c.is_control()).collect()
}

/// The list: the processes of `now` that may be offered, in the order the box shows them.
///
/// Without `prev` the order is memory, largest first. With it, each row gets the CPU it used
/// since `prev` (a process absent from `prev`, or with another start time under the same pid, counts as
/// idle) and the order is CPU, then memory.
pub fn rows(prev: Option<&[ProcSample]>, now: &[ProcSample], excluded: &Excluded) -> Vec<ProcRow> {
    let ticks_per_second = clock_ticks_per_second() as f32;
    let mut out: Vec<ProcRow> = now
        .iter()
        .filter(|s| s.pid > 1 && !excluded.pids.contains(&s.pid) && !excluded.comms.contains(&s.name))
        .map(|s| ProcRow {
            pid: s.pid,
            start: s.start,
            name: s.name.clone(),
            cmdline: s.cmdline.clone(),
            rss_kb: s.rss_kb,
            cpu_pct: prev.map(|p| cpu_percent(p, s, ticks_per_second)),
        })
        .collect();
    out.sort_by(|a, b| {
        let by_cpu = b.cpu_pct.unwrap_or(0.0).total_cmp(&a.cpu_pct.unwrap_or(0.0));
        by_cpu.then(b.rss_kb.cmp(&a.rss_kb)).then(a.pid.cmp(&b.pid))
    });
    out
}

/// The share of one core `s` used since `prev` had it (0 when it is new or no time passed).
fn cpu_percent(prev: &[ProcSample], s: &ProcSample, ticks_per_second: f32) -> f32 {
    let Some(before) = prev.iter().find(|p| p.pid == s.pid && p.start == s.start) else { return 0.0 };
    let elapsed_cs = s.uptime_cs.saturating_sub(before.uptime_cs);
    if elapsed_cs == 0 {
        return 0.0;
    }
    let used = s.ticks.saturating_sub(before.ticks) as f32 / ticks_per_second;
    used / (elapsed_cs as f32 / 100.0) * 100.0
}

/// What a signal asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sig {
    /// SIGTERM: ask to quit.
    Term,
    /// SIGKILL: no asking.
    Kill,
}

/// Why a signal did not go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalError {
    /// The process is gone (ESRCH).
    Gone,
    /// Not allowed (EPERM).
    Denied,
    /// The pid now belongs to another process: nothing was sent.
    Reused,
    /// Anything else, including a pid that could reach a group.
    Other,
}

/// How a signal is sent and a start time read: the real one in `processes_app.rs`, a recorder in tests.
pub trait Signaller {
    fn signal(&self, pid: i32, sig: Sig) -> Result<(), SignalError>;
    /// The process's start time (`stat` field 22) now; None when it is gone.
    fn start_time(&self, pid: i32) -> Option<u64>;
}

/// Sends `sig` to `pid` only if it still started at `start`, the time it was listed with, and it is
/// not one of `excluded` (the list never offers those, and a refusal here is the second line).
pub fn send(signaller: &dyn Signaller, pid: i32, start: u64, sig: Sig, excluded: &Excluded) -> Result<(), SignalError> {
    // kill(2) with 0, -1 or a negative pid reaches a group or everything; 1 is init.
    if pid <= 1 {
        return Err(SignalError::Other);
    }
    if excluded.pids.contains(&pid) {
        return Err(SignalError::Denied);
    }
    match signaller.start_time(pid) {
        None => Err(SignalError::Gone),
        Some(now) if now != start => Err(SignalError::Reused),
        Some(_) => signaller.signal(pid, sig),
    }
}

/// Most parents `ancestors` follows: far more than a session is deep.
const ANCESTORS_MAX: usize = 64;

/// The parents of `pid` under `root` (`/proc`), nearest first, up to init (not included): whoever
/// owns them. The kill list never offers them: ending the compositor's launcher script, or the
/// shell that started alpheus, may end the session. The walk ends on a loop, a parent whose `stat`
/// cannot be read (that parent is still listed), or `ANCESTORS_MAX` steps.
pub fn ancestors(root: &Path, pid: i32) -> Vec<i32> {
    let mut found = Vec::new();
    let mut at = pid;
    for _ in 0..ANCESTORS_MAX {
        let Some(stat) = fs::read(root.join(at.to_string()).join("stat")).ok().and_then(|b| parse_stat(&b)) else { break };
        if stat.ppid <= 1 || stat.ppid == pid || found.contains(&stat.ppid) {
            break;
        }
        found.push(stat.ppid);
        at = stat.ppid;
    }
    found
}

/// The start time of `pid` under `root` (`/proc`), from `stat`; None when it is gone or odd.
pub fn start_time_in(root: &Path, pid: i32) -> Option<u64> {
    Some(parse_stat(&fs::read(root.join(pid.to_string()).join("stat")).ok()?)?.start)
}

#[cfg(test)]
mod tests;
