//! Tests of the Shield runner (`run.rs`): the exit code a command reports, a listing parsed on the
//! worker, and the worker delivering exactly once. A fake executor stands in for every program.

use super::*;
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::exec::{Captured, Output};
use std::sync::mpsc;

fn argv(s: &[&str]) -> Vec<String> {
    s.iter().map(|a| a.to_string()).collect()
}

fn exited(code: i32, out: &str) -> Captured {
    Captured { stdout: out.as_bytes().to_vec(), end: CaptureEnd::Exited(Some(code)) }
}

fn outcome(m: &ShieldMail) -> RunOutcome {
    let ShieldMail::Done { outcome, .. } = m;
    *outcome
}

#[test]
fn a_switch_reports_the_exit_code_and_keeps_no_output() {
    let fake = FakeExecutor::new();
    fake.capture_ok("fake-shield on", exited(126, "output that is never kept\n"));
    let m = run_job(&fake, Job::Switch(true), 4, &argv(&["fake-shield", "on"]));
    assert_eq!(m, ShieldMail::Done { job: Job::Switch(true), generation: 4, outcome: RunOutcome::Exited(Some(126)), listed: None });
    assert_eq!(fake.calls(), ["fake-shield on"], "run once");
    assert!(fake.capture_limits().is_empty(), "an action is never a capture (final fix wave I1): its output is dropped, not piped");
    // A signal.
    fake.capture_ok("fake-shield off", Captured { stdout: vec![], end: CaptureEnd::Exited(None) });
    assert_eq!(outcome(&run_job(&fake, Job::Switch(false), 5, &argv(&["fake-shield", "off"]))), RunOutcome::Exited(None));
    fake.capture_ok("fake-allow", exited(0, ""));
    assert_eq!(outcome(&run_job(&fake, Job::Allow(7), 1, &argv(&["fake-allow"]))), RunOutcome::Exited(Some(0)));
    assert!(fake.capture_limits().is_empty());
}

#[test]
fn a_listing_is_parsed_on_the_worker() {
    let fake = FakeExecutor::new();
    let line = "3: block id 1234:5678 serial \"S\" name \"Test Stick\" hash \"h\"\n";
    fake.capture_ok("fake-list", exited(0, line));
    let m = run_job(&fake, Job::List, 0, &argv(&["fake-list"]));
    let ShieldMail::Done { listed: Some(p), outcome, .. } = m else { panic!("a listing") };
    assert_eq!(outcome, RunOutcome::Exited(Some(0)));
    assert_eq!((p.devices.len(), p.devices[0].id, p.devices[0].name.as_str()), (1, 3, "Test Stick"));
    assert_eq!(fake.capture_limits(), [LIST_LIMITS]);
    // Output an action never parses.
    fake.capture_ok("fake-allow", exited(0, line));
    let ShieldMail::Done { listed, .. } = run_job(&fake, Job::Allow(3), 0, &argv(&["fake-allow"]));
    assert_eq!(listed, None);
}

#[test]
fn a_failed_listing_is_reported() {
    let fake = FakeExecutor::new();
    let line = "3: block id 1234:5678 name \"Test Stick\"\n";
    fake.capture_ok("fake-timeout", Captured { stdout: line.into(), end: CaptureEnd::TimedOut });
    fake.capture_ok("fake-capped", Captured { stdout: line.into(), end: CaptureEnd::Capped });
    fake.capture_ok("fake-exit1", exited(1, line));
    for (p, want) in
        [("fake-timeout", RunOutcome::TimedOut), ("fake-capped", RunOutcome::Capped), ("fake-exit1", RunOutcome::Exited(Some(1)))]
    {
        let ShieldMail::Done { outcome, listed, .. } = run_job(&fake, Job::List, 0, &argv(&[p]));
        assert_eq!((outcome, listed), (want, None), "{p}: nothing is parsed from a failed read");
    }
}

#[test]
fn the_worker_delivers_once_and_ends() {
    let fake = Arc::new(FakeExecutor::new());
    fake.capture_ok("fake-shield on", exited(0, ""));
    let (tx, rx) = mpsc::channel();
    let handle = spawn_job(fake.clone(), Job::Switch(true), 9, argv(&["fake-shield", "on"]), move |m| tx.send(m).unwrap()).unwrap();
    assert_eq!(handle.thread().name(), Some("alpheus-shield"));
    handle.join().expect("the worker ends");
    let m = rx.try_recv().expect("one mail");
    assert_eq!(outcome(&m), RunOutcome::Exited(Some(0)));
    assert!(rx.try_recv().is_err(), "exactly one");
}

/// An executor whose capture panics: the loop still hears of the job.
struct Panicking;

impl Executor for Panicking {
    fn run(&self, _: &str, _: &[&str]) -> Result<Output, ExecError> {
        unreachable!()
    }
    fn spawn(&self, _: &str, _: &[&str]) -> Result<(), ExecError> {
        unreachable!()
    }
    fn spawn_with_input(&self, _: &str, _: &[&str], _: &[&[u8]]) -> Result<(), ExecError> {
        unreachable!()
    }
    fn spawn_with_env(&self, _: &str, _: &[&str], _: &[(&str, &str)], _: &[&str]) -> Result<(), ExecError> {
        unreachable!()
    }
    fn stream(&self, _: &str, _: &[&str]) -> Result<alpheus_common::exec::Stream, ExecError> {
        unreachable!()
    }
    fn capture(&self, _: &str, _: &[&str], _: CaptureLimits) -> Result<Captured, ExecError> {
        panic!("a bug in the executor")
    }
    fn run_silent(&self, _: &str, _: &[&str]) -> Result<Option<i32>, ExecError> {
        panic!("a bug in the executor")
    }
}

#[test]
fn a_panicking_worker_still_reports() {
    let (tx, rx) = mpsc::channel();
    let h = spawn_job(Arc::new(Panicking), Job::Switch(false), 2, argv(&["x"]), move |m| tx.send(m).unwrap()).unwrap();
    h.join().expect("the panic was caught");
    assert_eq!(rx.try_recv().unwrap(), ShieldMail::worker_failed(Job::Switch(false), 2));
}
