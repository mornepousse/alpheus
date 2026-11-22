//! Running one Shield job (shield spec §§ 6 and 7) on a short-lived thread named `alpheus-shield`,
//! so alpheus sees the exit code without ever blocking the loop. The worker mails its outcome back
//! (`Mail::Shield`). A listing (unprivileged, its output needed) is read through `Executor::capture`
//! and parsed here, bounded, so the loop never handles raw output. An action (on, off, allow: often
//! a privileged helper such as pkexec, whose output is never wanted) goes through
//! `Executor::run_silent`: waited for with no bound of its own (the model's 60 s deadline judges),
//! its standard output and error `/dev/null`, so a helper we cannot kill never meets a closed pipe
//! half way through its work. Nothing a command prints is ever kept or logged.

use crate::shield::usb::parse_blocked;
use crate::shield::{Job, RunOutcome, ShieldMail};
use alpheus_common::exec::{CaptureEnd, CaptureLimits, ExecError, Executor};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// A device list: short and small.
pub const LIST_LIMITS: CaptureLimits = CaptureLimits { deadline: Duration::from_secs(2), max_bytes: 64 * 1024, max_lines: 1024 };

/// Runs `argv` as `job` and reports how it ended. Synchronous.
pub fn run_job(exec: &dyn Executor, job: Job, generation: u64, argv: &[String]) -> ShieldMail {
    let done = |outcome, listed| ShieldMail::Done { job, generation, outcome, listed };
    let Some((prog, args)) = argv.split_first() else { return done(RunOutcome::CouldNotStart, None) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
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
                let listed = (job == Job::List && code == Some(0)).then(|| parse_blocked(&c.stdout));
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
pub fn spawn_job(
    exec: Arc<dyn Executor>,
    job: Job,
    generation: u64,
    argv: Vec<String>,
    deliver: impl FnOnce(ShieldMail) + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("alpheus-shield".into()).spawn(move || {
        let mail = catch_unwind(AssertUnwindSafe(|| run_job(exec.as_ref(), job, generation, &argv))).unwrap_or_else(|_| {
            crate::log::write("shield: worker failed");
            ShieldMail::worker_failed(job, generation)
        });
        deliver(mail);
    })
}

#[cfg(test)]
mod tests;
