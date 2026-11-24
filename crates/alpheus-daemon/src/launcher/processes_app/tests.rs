//! Tests of the kill list's wiring that need no `App`: what a key becomes once the glue has
//! signalled (a recording fake, never `kill(2)`), what the box says, and when the timer is wanted.

use super::*;
use crate::keys::Key;
use crate::launcher::config::LauncherConfig;
use crate::launcher::matching::Mode;
use crate::launcher::model::Outcome;
use crate::launcher::processes::ProcRow;
use crate::launcher::testing::TempDir;
use std::cell::RefCell;

/// A `Signaller` that records each signal and answers as told.
struct Fake {
    start: Option<u64>,
    result: Result<(), SignalError>,
    sent: RefCell<Vec<(i32, Sig)>>,
}

impl Fake {
    fn answering(result: Result<(), SignalError>) -> Fake {
        Fake { start: Some(2000), result, sent: RefCell::new(Vec::new()) }
    }
}

impl Signaller for Fake {
    fn signal(&self, pid: i32, sig: Sig) -> Result<(), SignalError> {
        self.sent.borrow_mut().push((pid, sig));
        self.result
    }

    fn start_time(&self, _pid: i32) -> Option<u64> {
        self.start
    }
}

fn list() -> Launcher {
    let row = |pid: i32, name: &str, rss_kb| ProcRow {
        pid,
        start: u64::from(pid.unsigned_abs()) * 10,
        name: name.into(),
        cmdline: format!("/usr/bin/{name}"),
        rss_kb,
        cpu_pct: None,
    };
    Launcher::for_processes(LauncherConfig::default(), vec![row(200, "firefox", 900), row(201, "editor", 500)])
}

/// Performs what the key returned, as the surface does for `Outcome::Signal`.
fn perform(l: &mut Launcher, signaller: &Fake, outcome: Outcome) {
    match outcome {
        Outcome::Signal { pid, start, sig } => {
            apply_signal(l, signaller, &Excluded { pids: Vec::new(), comms: Vec::new() }, pid, start, sig);
        }
        other => panic!("expected a signal, got {other:?}"),
    }
}

#[test]
fn enter_sends_term_and_shows_the_result() {
    let mut l = list();
    let fake = Fake::answering(Ok(()));
    let outcome = l.key(Key::Enter);
    perform(&mut l, &fake, outcome);
    assert_eq!(fake.sent.borrow().as_slice(), [(200, Sig::Term)]);
    assert_eq!(l.message.as_deref(), Some("asked firefox to quit"));
    assert_eq!(l.procs.len(), 2, "the row stays, marked");
}

#[test]
fn ctrl_enter_asks_then_a_second_enter_kills() {
    let mut l = list();
    let fake = Fake::answering(Ok(()));
    let t = Instant::now();
    l.set_now(t);
    assert_eq!(l.key(Key::CtrlEnter), Outcome::Asked);
    assert!(fake.sent.borrow().is_empty(), "asking sends nothing");
    // Inside 0.4 s nothing happens; from then on, the second Enter kills.
    l.set_now(t + Duration::from_millis(100));
    assert_eq!(l.key(Key::Enter), Outcome::Nothing);
    assert!(fake.sent.borrow().is_empty());
    l.set_now(t + Duration::from_millis(450));
    let outcome = l.key(Key::Enter);
    perform(&mut l, &fake, outcome);
    assert_eq!(fake.sent.borrow().as_slice(), [(200, Sig::Kill)]);
    assert_eq!(l.message.as_deref(), Some("killed firefox"));
}

#[test]
fn eperm_and_esrch_show_a_word_in_the_box() {
    let mut l = list();
    let denied = Fake::answering(Err(SignalError::Denied));
    let t = Instant::now();
    l.set_now(t);
    let outcome = l.key(Key::Enter);
    perform(&mut l, &denied, outcome);
    assert_eq!(l.message.as_deref(), Some("firefox: not allowed"));
    assert_eq!(l.procs.len(), 2, "still listed: it is still running");
    let gone = Fake { start: None, ..Fake::answering(Ok(())) };
    l.set_now(t + Duration::from_millis(600));
    let outcome = l.key(Key::Enter);
    perform(&mut l, &gone, outcome);
    assert_eq!(l.message.as_deref(), Some("firefox: already gone"));
    assert!(gone.sent.borrow().is_empty());
    assert_eq!(l.procs.len(), 1);
}

#[test]
fn a_reused_pid_says_so_and_sends_nothing() {
    let mut l = list();
    let reused = Fake { start: Some(999_999), ..Fake::answering(Ok(())) };
    let outcome = l.key(Key::Enter);
    perform(&mut l, &reused, outcome);
    assert!(reused.sent.borrow().is_empty());
    assert_eq!(l.message.as_deref(), Some("firefox: pid reused, nothing sent"));
}

#[test]
fn errno_maps_to_the_two_words_and_the_rest() {
    assert_eq!(signal_error_of(libc::ESRCH), SignalError::Gone);
    assert_eq!(signal_error_of(libc::EPERM), SignalError::Denied);
    assert_eq!(signal_error_of(libc::EINVAL), SignalError::Other);
    assert_eq!(signal_error_of(0), SignalError::Other);
}

#[test]
fn closing_the_list_drops_its_timer() {
    let now = Instant::now();
    let due = Some(now + SAMPLE_AFTER);
    // Open: the second sample is what the loop waits for.
    let l = list();
    assert_eq!(wanted(Some(&l), due), due);
    // Taken: nothing is left to wait for.
    assert_eq!(wanted(Some(&l), None), None);
    // Closed, or another box open: no timer, whatever the state still held.
    assert_eq!(wanted(None, due), None);
    let other = Launcher::for_calc(LauncherConfig::default());
    assert_ne!(other.mode, Mode::Processes);
    assert_eq!(wanted(Some(&other), due), None);
}

#[test]
fn the_timer_follows_the_earlier_of_the_sample_and_the_question() {
    let mut l = list();
    let t = Instant::now();
    l.set_now(t);
    l.key(Key::CtrlEnter);
    let sample = Some(t + SAMPLE_AFTER);
    assert_eq!(wanted(Some(&l), sample), sample, "the sample comes first");
    assert_eq!(wanted(Some(&l), None), Some(t + Duration::from_secs(3)), "then only the question");
    assert_eq!(wanted(Some(&l), Some(t + Duration::from_secs(10))), Some(t + Duration::from_secs(3)));
}

#[test]
fn alpheus_the_leader_the_compositor_and_the_manager_are_excluded() {
    let e = excluded(10, 20, Some(30), Some("sway"), &[]);
    assert_eq!(e.pids, [10, 20, 30]);
    assert_eq!(e.comms, ["systemd", "sway"]);
    let e = excluded(10, 20, None, None, &[]);
    assert_eq!(e.pids, [10, 20]);
    assert_eq!(e.comms, ["systemd"]);
}

/// A fake `/proc` with just the `stat` files the ancestor walk reads: pid, parent.
fn stat_tree(name: &str, pairs: &[(i32, i32)]) -> TempDir {
    let root = TempDir::new(name);
    for (pid, ppid) in pairs {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stat"), format!("{pid} (x) S {ppid} {pid} {pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5 0 0\n")).unwrap();
    }
    root
}

#[test]
fn every_ancestor_of_the_compositor_and_of_alpheus_is_excluded() {
    // 10 `bash mango-launcher.sh` <- 20 the compositor <- 30 alpheus; 5 a terminal <- 6 a shell <- 30 is not (one parent).
    let root = stat_tree("kill-excluded-ancestors", &[(10, 1), (20, 10), (30, 20), (50, 1), (60, 50), (70, 60)]);
    // The compositor (20) and the daemon (70, in another branch here): both branches' ancestors.
    let e = excluded_in(&root, 70, 70, Some(20), Some("mango"));
    for pid in [70, 60, 50, 20, 10] {
        assert!(e.pids.contains(&pid), "{pid} must never be offered: {:?}", e.pids);
    }
    assert!(!e.pids.contains(&30), "a sibling is not an ancestor");
    assert!(!e.pids.contains(&1), "init is dropped by `rows` anyway");
}

#[test]
fn the_real_signaller_reads_start_times_and_finds_a_missing_process_gone() {
    // A process of this test, found in the real /proc: its start time is a real, stable number.
    let signaller = RealSignaller::new(PathBuf::from("/proc"));
    let own = std::process::id() as i32;
    let first = signaller.start_time(own).expect("this process has a start time");
    assert_eq!(signaller.start_time(own), Some(first));
    // A pid that cannot exist is Gone: nothing is signalled, `send` never reaches `kill`.
    assert_eq!(signaller.start_time(i32::MAX), None);
    let nobody = Excluded { pids: Vec::new(), comms: Vec::new() };
    assert_eq!(processes::send(&signaller, i32::MAX, 1, Sig::Kill, &nobody), Err(SignalError::Gone));
    // A start time that is not ours: Reused, for this very process, and nothing is sent to it.
    assert_eq!(processes::send(&signaller, own, first + 1, Sig::Kill, &nobody), Err(SignalError::Reused));
}
