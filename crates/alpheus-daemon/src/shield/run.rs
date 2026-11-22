//! Runs one Shield job: a command or a read of the blocked devices, on a short-lived thread that
//! mails its outcome back.
//!
//! The thread is named `alpheus-shield` and lives only while the job runs, so alpheus sees the exit
//! code without ever blocking the event loop. The worker mails the outcome back (`Mail::Shield`).
//! A listing (unprivileged, its output wanted) is read through `Executor::capture` and parsed here,
//! bounded, so the loop never handles raw output. An action (on, off, allow: often a privileged
//! helper such as pkexec, whose output is never wanted) goes through `Executor::run_silent`: waited
//! for with no bound of its own (the model's 60 s deadline judges, and kills nothing), its standard
//! output and error `/dev/null`, so a helper that cannot be killed never meets a closed pipe half
//! way through its work.
//!
//! Never:
//! - keep or log anything a command prints;
//! - build a shell command: the configured argv is run as it is (a guardrail,
//!   `only_the_runners_capture_and_never_through_a_shell`, reads this file's code);
//! - read an action's output through `capture`: only the listing is, and a guardrail
//!   (`a_shield_action_is_never_a_capture_and_shutdown_stops_the_commands`) checks the order of the
//!   two calls in `run_job`;
//! - panic: a worker that panics still reports, as `WorkerFailed`.
//!
//! See: docs/specs/2026-11-21-alpheus-shield-design.md (§§ 6 and 7), docs/features/shield.md, and
//! docs/decisions.md ("Shield commands and reads run on a short-lived thread", "A running Shield
//! command is never killed at its deadline").

use crate::shield::usb::parse_blocked;
use crate::shield::{Job, RunOutcome, ShieldMail};
use alpheus_common::exec::{CaptureEnd, CaptureLimits, ExecError, Executor};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// A device list: short and small (shield spec § 6: 2 s, 64 KiB, 1,024 lines).
pub const LIST_LIMITS: CaptureLimits = CaptureLimits { deadline: Duration::from_secs(2), max_bytes: 64 * 1024, max_lines: 1024 };

/// Runs `argv` as `job` and reports how it ended. Synchronous.
pub fn run_job(exec: &dyn Executor, job: Job, generation: u64, argv: &[String]) -> ShieldMail {
    // `done` builds the report; the closure captures `job` and `generation`, which every end shares.
    let done = |outcome, listed| ShieldMail::Done { job, generation, outcome, listed };
    // The program is the first element, the arguments the rest; an empty argv cannot start.
    let Some((prog, args)) = argv.split_first() else { return done(RunOutcome::CouldNotStart, None) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // An action comes first and returns here, so it never reaches the listing's `capture` below.
    if job != Job::List {
        return match exec.run_silent(prog, &args) {
            Ok(code) => done(RunOutcome::Exited(code), None),
            Err(ExecError::NotFound) => done(RunOutcome::NotFound, None),
            Err(_) => done(RunOutcome::CouldNotStart, None),
        };
    }
    match exec.capture(prog, &args, LIST_LIMITS) {
        Ok(c) => match c.end {
            CaptureEnd::Exited(code) => {
                let listed = if job == Job::List && code == Some(0) { Some(parse_blocked(&c.stdout)) } else { None };
                done(RunOutcome::Exited(code), listed)
            }
            CaptureEnd::TimedOut => done(RunOutcome::TimedOut, None),
            CaptureEnd::Capped => done(RunOutcome::Capped, None),
        },
        Err(ExecError::NotFound) => done(RunOutcome::NotFound, None),
        Err(_) => done(RunOutcome::CouldNotStart, None),
    }
}

/// `run_job` on its own thread; `deliver` gets the mail once, whatever happens (a panic reports
/// `WorkerFailed`). The daemon drops the handle; tests join it.
///
/// The `io::Result` is the thread builder's: an error means the thread could not start, and
/// `deliver` was then never called.
pub fn spawn_job(
    exec: Arc<dyn Executor>,
    job: Job,
    generation: u64,
    argv: Vec<String>,
    deliver: impl FnOnce(ShieldMail) + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("alpheus-shield".into()).spawn(move || {
        // `catch_unwind` needs a closure that is unwind safe; `AssertUnwindSafe` says this one is (it
        // only reads `exec` and `argv`).
        let mail = match catch_unwind(AssertUnwindSafe(|| run_job(exec.as_ref(), job, generation, &argv))) {
            Ok(mail) => mail,
            Err(_) => {
                crate::log::write("shield: worker failed");
                ShieldMail::worker_failed(job, generation)
            }
        };
        deliver(mail);
    })
}

#[cfg(test)]
mod tests;
