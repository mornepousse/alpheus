//! The Tools menu's reader (tools spec § 6): the `state` of every entry that has one, and a
//! `choices_cmd`, read on one short-lived thread (`alpheus-tools`) through `sources::capture_*`
//! (1 s, 2,000 lines, 256 KiB, the group killed and reaped). One job at a time across opens; states
//! arrive one entry per delivery; a waiting choices job (`preempt`) stops a states job after its
//! current entry and gets the unread ones back. Notes name an entry by its position in
//! launcher.toml, never its name, command or output.

use super::nav::{Counts, Job, StateJob};
use crate::launcher::sources;
use crate::log::write as log;
use alpheus_common::exec::{CaptureEnd, ExecError, Executor};
use std::fmt;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The thread's name.
pub const THREAD: &str = "alpheus-tools";

/// Why a read gave less than a clean answer: fixed words for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    NotFound,
    /// A `choices_cmd` exited with this code (a `state`'s exit is its answer).
    ExitCode(i32),
    Signal,
    TimedOut,
    Capped,
    CouldNotStart,
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::NotFound => f.write_str("program not found"),
            Why::ExitCode(c) => write!(f, "exited with code {c}"),
            Why::Signal => f.write_str("killed by a signal"),
            Why::TimedOut => write!(f, "timed out after {} s", sources::LIST_DEADLINE.as_secs()),
            Why::Capped => {
                write!(f, "output cap reached ({} lines or {} KiB)", sources::MAX_SOURCE_LINES, sources::MAX_SOURCE_BYTES / 1024)
            }
            Why::CouldNotStart => f.write_str("could not start"),
        }
    }
}

fn why_of_error(e: &ExecError) -> Why {
    match e {
        ExecError::NotFound => Why::NotFound,
        ExecError::Timeout => Why::TimedOut,
        // The executor's message may quote a path: only that it could not start is kept.
        ExecError::Io(_) => Why::CouldNotStart,
    }
}

/// A `state` run's answer: exit 0 is on, any other exit off; a kill, a timeout or a cap leaves it
/// unknown (no switch), and so does a program that cannot run.
pub fn state_of(r: Result<CaptureEnd, ExecError>) -> (Option<bool>, Option<Why>) {
    match r {
        Ok(CaptureEnd::Exited(Some(0))) => (Some(true), None),
        Ok(CaptureEnd::Exited(Some(_))) => (Some(false), None),
        Ok(CaptureEnd::Exited(None)) => (None, Some(Why::Signal)),
        Ok(CaptureEnd::TimedOut) => (None, Some(Why::TimedOut)),
        Ok(CaptureEnd::Capped) => (None, Some(Why::Capped)),
        Err(e) => (None, Some(why_of_error(&e))),
    }
}

/// A `choices_cmd` run's answer: the lines kept (whatever the end), whether the read failed with
/// none to show, and why it was not clean.
pub fn choices_of(r: Result<(Vec<String>, CaptureEnd), ExecError>) -> (Vec<String>, bool, Option<Why>) {
    let (lines, why) = match r {
        Ok((lines, CaptureEnd::Exited(Some(0)))) => (lines, None),
        Ok((lines, CaptureEnd::Exited(Some(c)))) => (lines, Some(Why::ExitCode(c))),
        Ok((lines, CaptureEnd::Exited(None))) => (lines, Some(Why::Signal)),
        Ok((lines, CaptureEnd::TimedOut)) => (lines, Some(Why::TimedOut)),
        Ok((lines, CaptureEnd::Capped)) => (lines, Some(Why::Capped)),
        Err(e) => (Vec::new(), Some(why_of_error(&e))),
    };
    let failed = lines.is_empty() && why.is_some();
    (lines, failed, why)
}

/// What the reader hands back; the glue adds the open's generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// One entry's state (None: unknown).
    State { entry: usize, state: Option<bool> },
    /// The states job is over (it ended, or `preempt` stopped it): the entries it did not read, and
    /// the counts so far (to resume with).
    /// Always the job's last event.
    StatesDone { unread: Vec<StateJob>, counts: Counts },
    /// The choices job's only event.
    Choices { entry: usize, token: u64, lines: Vec<String>, failed: bool },
}

/// Runs `job` here: a `State` per entry through `deliver` as each one ends (`preempt` checked
/// between entries), and returns the job's last event, for the caller to deliver once it has freed
/// its slot.
pub fn run_job(
    job: Job,
    exec: &dyn Executor,
    preempt: &AtomicBool,
    deliver: &mut dyn FnMut(JobEvent),
    note: &mut dyn FnMut(&str),
) -> JobEvent {
    match job {
        Job::States(jobs, mut counts) => {
            let mut rest = jobs.into_iter();
            let mut unread = Vec::new();
            let mut first = true;
            while let Some(j) = rest.next() {
                if !first && preempt.load(Ordering::Acquire) {
                    unread.push(j);
                    unread.extend(rest);
                    break;
                }
                first = false;
                let (state, why) = state_of(sources::capture_status(&j.argv, exec));
                if let Some(why) = why {
                    let i = j.position;
                    note(&format!("tools: entry {i}: state {why}"));
                }
                match state {
                    Some(true) => counts.on += 1,
                    Some(false) => counts.off += 1,
                    None => counts.unknown += 1,
                }
                deliver(JobEvent::State { entry: j.entry, state });
            }
            if unread.is_empty() {
                let Counts { on, off, unknown } = counts;
                note(&format!("tools: states read ({on} on, {off} off, {unknown} unknown)"));
            }
            JobEvent::StatesDone { unread, counts }
        }
        Job::Choices { entry, position, argv, token } => {
            let (lines, failed, why) = choices_of(sources::capture_lines(&argv, exec));
            if let Some(why) = why {
                let (i, k) = (position, lines.len());
                note(&format!("tools: entry {i}: choices {why}, {k} kept"));
            }
            JobEvent::Choices { entry, token, lines, failed }
        }
    }
}

/// Set while the reader thread runs: one job at a time across opens.
static TOOLS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Clears TOOLS_RUNNING however the job ends.
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        TOOLS_RUNNING.store(false, Ordering::Release);
    }
}

/// Starts `job` on its own thread; `deliver` gets each event, the last one after the slot is free
/// (a delivery that starts the next job never finds it taken). Err(WouldBlock) while a job runs,
/// and Err if the thread cannot start (nothing is then running). A panic is caught: one
/// `tools read failed` note, and a last event (a failed choices read, an ended states read).
pub fn spawn_job(
    job: Job,
    exec: Arc<dyn Executor>,
    preempt: Arc<AtomicBool>,
    mut deliver: impl FnMut(JobEvent) + Send + 'static,
) -> io::Result<()> {
    if TOOLS_RUNNING.swap(true, Ordering::AcqRel) {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "a tools read is still running"));
    }
    let running = Running;
    std::thread::Builder::new().name(THREAD.into()).spawn(move || {
        let on_panic = match &job {
            Job::States(..) => JobEvent::StatesDone { unread: Vec::new(), counts: Counts::default() },
            Job::Choices { entry, token, .. } => JobEvent::Choices { entry: *entry, token: *token, lines: Vec::new(), failed: true },
        };
        let last =
            catch_unwind(AssertUnwindSafe(|| run_job(job, exec.as_ref(), &preempt, &mut deliver, &mut |m| log(m)))).unwrap_or_else(|_| {
                // Fixed words: a payload can quote what was read.
                log("tools read failed");
                on_panic
            });
        // Freed before the last delivery, as the scan's: a reopen racing it must find the slot free.
        drop(running);
        deliver(last);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
