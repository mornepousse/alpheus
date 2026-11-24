//! The kill list's wiring on the daemon's state: opening the list, its second sample, the signal.
//!
//! The rows, the exclusions and the start-time check are pure and live in `processes.rs`; the keys
//! and the question are the launcher model's. This file is the part that touches the rest of the
//! daemon. When the list opens it reads `/proc` once and shows the rows sorted by memory; 300 ms
//! later one more read fills in the CPU and sorts again; then nothing more is read. One
//! `DeadlineTimer` follows the next of two moments (the second sample, the end of a force-kill
//! question) and exists only while the list is open and one of them is pending. A signal goes
//! through the `Signaller` seam, whose real form is `kill(2)` plus a start-time read.
//!
//! Never:
//! - keep a timer or a sample while the list is closed: `processes_timer_sync` clears both
//!   whenever no kill list is open (`closing_the_list_drops_its_timer`, and the memory suite's
//!   `at_rest_no_wakeups_after_a_kill_list`);
//! - write a process name, a pid or a command line into a log line, or into a notification: the
//!   outcome of a signal is said in the box only, and the one log line is a fixed word;
//! - read `/proc` while the list is closed, or more than twice for one open;
//! - signal anything but the pid and start time the list was shown with (`processes::send`).
//!
//! See: docs/features/tools-menu.md ("Kill a process"), docs/decisions.md ("Only the user's own
//! processes are offered, and a reused pid is never signalled").

use crate::deadline::DeadlineTimer;
use crate::launcher::model::Launcher;
use crate::launcher::processes::{self, Excluded, MAX_PROCESSES, ProcSample, Sig, SignalError, Signaller};
use crate::log::write as log;
use crate::wayland::App;
use alpheus_common::compositor::Compositor;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long after the list opens the second sample is read (it fills in the CPU).
pub const SAMPLE_AFTER: Duration = Duration::from_millis(300);

/// The kill list's `Signaller`: `kill(2)`, and the start time from `stat` under `root`.
pub struct RealSignaller {
    root: PathBuf,
}

impl RealSignaller {
    pub fn new(root: PathBuf) -> RealSignaller {
        RealSignaller { root }
    }
}

/// What `kill(2)` failing with `errno` means for the list.
pub fn signal_error_of(errno: i32) -> SignalError {
    match errno {
        libc::ESRCH => SignalError::Gone,
        libc::EPERM => SignalError::Denied,
        _ => SignalError::Other,
    }
}

impl Signaller for RealSignaller {
    fn signal(&self, pid: i32, sig: Sig) -> Result<(), SignalError> {
        let number = match sig {
            Sig::Term => libc::SIGTERM,
            Sig::Kill => libc::SIGKILL,
        };
        // SAFETY: kill(2) takes two integers and no pointer; `processes::send` has already refused
        // a pid of 1 or below, so this never names init or a process group.
        let r = unsafe { libc::kill(pid, number) };
        if r == 0 { Ok(()) } else { Err(signal_error_of(io::Error::last_os_error().raw_os_error().unwrap_or(0))) }
    }

    fn start_time(&self, pid: i32) -> Option<u64> {
        processes::start_time_in(&self.root, pid)
    }
}

/// The process name of a compositor, as `stat` shows it.
fn compositor_comm(c: &Compositor) -> &'static str {
    match c {
        Compositor::Sway { .. } => "sway",
        Compositor::Hyprland { .. } => "Hyprland",
        Compositor::Mango { .. } => "mango",
        Compositor::Niri { .. } => "niri",
    }
}

/// What the list never offers: alpheus itself (`own`), the session leader (`leader`), the
/// compositor by its socket's peer (`compositor`) and by its name (`compositor_name`, for when the
/// socket did not tell), their `ancestors` (the script that started the compositor, the shell that
/// started alpheus), and the user's service manager, whose name is `systemd`.
pub fn excluded(own: i32, leader: i32, compositor: Option<i32>, compositor_name: Option<&str>, ancestors: &[i32]) -> Excluded {
    let mut pids = vec![own, leader];
    pids.extend(compositor);
    pids.extend_from_slice(ancestors);
    let mut comms = vec!["systemd".to_string()];
    comms.extend(compositor_name.map(str::to_string));
    Excluded { pids, comms }
}

/// `excluded`, with the ancestors of `own` and of the compositor read from `root` (`/proc`).
pub fn excluded_in(root: &Path, own: i32, leader: i32, compositor: Option<i32>, compositor_name: Option<&str>) -> Excluded {
    let mut parents = processes::ancestors(root, own);
    if let Some(pid) = compositor {
        parents.extend(processes::ancestors(root, pid));
    }
    excluded(own, leader, compositor, compositor_name, &parents)
}

/// The next moment the kill list needs the loop: its second sample (`sample_due`) or the end of a
/// force-kill question. None unless `model` is an open kill list, so a closed list holds no timer.
pub fn wanted(model: Option<&Launcher>, sample_due: Option<Instant>) -> Option<Instant> {
    let model = model.filter(|m| m.mode == crate::launcher::matching::Mode::Processes)?;
    match (sample_due, model.processes_deadline()) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Sends `sig` to `pid` (if it still started at `start`) and says what happened in `model`'s box.
pub fn apply_signal(model: &mut Launcher, signaller: &dyn Signaller, excluded: &Excluded, pid: i32, start: u64, sig: Sig) {
    let result = processes::send(signaller, pid, start, sig, excluded);
    model.signal_result(pid, sig, result);
}

/// The glue's state on `App`: the timer, the first sample while the second is due, the seams.
pub struct ProcessesApp {
    pub(crate) timer: DeadlineTimer,
    /// The first read, kept only until the second one makes the CPU figures.
    first: Option<Vec<ProcSample>>,
    /// When the second read is due; None once taken (or with no list open).
    sample_due: Option<Instant>,
    signaller: Box<dyn Signaller>,
    /// What this open must not offer or signal, read once at the open.
    excluded: Excluded,
    /// `/proc`, or a test's tree.
    root: PathBuf,
}

impl ProcessesApp {
    pub fn new() -> ProcessesApp {
        let root = PathBuf::from("/proc");
        ProcessesApp {
            timer: DeadlineTimer::new(),
            first: None,
            sample_due: None,
            signaller: Box::new(RealSignaller::new(root.clone())),
            excluded: Excluded { pids: Vec::new(), comms: Vec::new() },
            root,
        }
    }
}

impl Default for ProcessesApp {
    fn default() -> Self {
        ProcessesApp::new()
    }
}

impl App {
    /// What this open of the list must not offer, worked out once per open (the compositor's pid
    /// asks its socket, and the second sample and every signal reuse the answer).
    fn processes_excluded(&self) -> Excluded {
        // SAFETY: getsid(2) with 0 asks for our own session and takes no pointer.
        let leader = unsafe { libc::getsid(0) };
        let compositor = alpheus_common::compositor::detect(|k| std::env::var_os(k));
        let pid = compositor.as_ref().and_then(|c| crate::sources::peer::peer_pid(&c.ipc_socket()));
        excluded_in(&self.processes.root, std::process::id() as i32, leader, pid, compositor.as_ref().map(compositor_comm))
    }

    /// `alpheus-launch --kill`: the user's processes, sorted by memory now and by CPU 300 ms later.
    pub(crate) fn launcher_open_processes(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        // The configuration's warnings are said by the opens that use more of it (the apps and the Tools menu).
        let config = crate::launcher::host::read_config(&alpheus_common::paths::config_dir(), &mut |_| {});
        // `proc_dir` is `/proc` but for a test's own folder: the signaller reads start times from the same place.
        self.processes.root = config.proc_dir.clone();
        self.processes.signaller = Box::new(RealSignaller::new(config.proc_dir.clone()));
        // SAFETY: getuid(2) takes nothing and cannot fail.
        let uid = unsafe { libc::getuid() };
        let samples = processes::snapshot(&self.processes.root, uid, MAX_PROCESSES);
        self.processes.excluded = self.processes_excluded();
        let rows = processes::rows(None, &samples, &self.processes.excluded);
        // A count, never a name: how many rows the box opens with.
        log(&format!("kill list: {count} listed", count = rows.len()));
        let model = Launcher::for_processes(config, rows);
        // Opened first: opening replaces any other box, whose close clears this state.
        self.launcher_open(qh, model, None, requested_at);
        if self.launcher.is_some() {
            self.processes.first = Some(samples);
            self.processes.sample_due = Some(Instant::now() + SAMPLE_AFTER);
        }
        self.processes_timer_sync();
    }

    /// The timer follows the next moment the list needs; none when no kill list is open, and the
    /// first sample is dropped with it.
    pub(crate) fn processes_timer_sync(&mut self) {
        let model = self.launcher.as_ref().map(|o| &o.model);
        let at = wanted(model, self.processes.sample_due);
        if at.is_none() && model.is_none_or(|m| m.mode != crate::launcher::matching::Mode::Processes) {
            self.processes.first = None;
            self.processes.sample_due = None;
        }
        self.processes.timer.sync_opt(self.loop_handle.as_ref(), at, App::processes_tick);
    }

    /// The timer fired (its source is dropped): the second sample is due, or a question ran out.
    pub(crate) fn processes_tick(app: &mut App, now: Instant) {
        app.processes.timer.fired();
        app.launcher_guarded("processes timer", move |app| {
            let mut changed = false;
            if app.processes.sample_due.is_some_and(|due| due <= now) {
                app.processes.sample_due = None;
                changed |= app.processes_second_sample();
            }
            if let Some(open) = app.launcher.as_mut() {
                changed |= open.model.processes_expire(now);
            }
            if changed {
                app.launcher_redraw();
            }
        });
        app.processes_timer_sync();
    }

    /// The second read: the CPU of each process since the first, the list sorted again with the
    /// highlighted process kept. True when the box changed.
    fn processes_second_sample(&mut self) -> bool {
        let Some(first) = self.processes.first.take() else { return false };
        // SAFETY: getuid(2) takes nothing and cannot fail.
        let uid = unsafe { libc::getuid() };
        let samples = processes::snapshot(&self.processes.root, uid, MAX_PROCESSES);
        let rows = processes::rows(Some(&first), &samples, &self.processes.excluded);
        let Some(open) = self.launcher.as_mut().filter(|o| o.model.mode == crate::launcher::matching::Mode::Processes) else {
            return false;
        };
        open.model.set_procs(rows);
        true
    }

    /// The list's Enter or confirmed Ctrl+Enter: the signal goes if the pid is still the listed
    /// process, and the box says the result. One fixed log word, never a name.
    pub(crate) fn processes_signal(&mut self, pid: i32, start: u64, sig: Sig) {
        let App { launcher, processes, .. } = self;
        let Some(open) = launcher.as_mut() else { return };
        apply_signal(&mut open.model, processes.signaller.as_ref(), &processes.excluded, pid, start, sig);
        log("kill list: signal");
        self.launcher_redraw();
    }
}

#[cfg(test)]
mod tests;
