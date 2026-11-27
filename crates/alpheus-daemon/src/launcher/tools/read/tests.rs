use super::*;
use crate::launcher::sources::{LIST_DEADLINE, MAX_SOURCE_LINES};
use crate::launcher::testing::TempDir;
use crate::launcher::tools::nav::Counts;
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::exec::{Captured, SystemExecutor};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `spawn_job` shares one static slot: the tests that use it run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn real() -> SystemExecutor {
    SystemExecutor::new(Duration::from_secs(5))
}

/// An `sh` script in `dir`, run as `sh <script>` (a file just written can't be exec'd while another
/// test's fork holds its write descriptor).
fn script(dir: &Path, name: &str, body: &str) -> Vec<String> {
    let p: PathBuf = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    vec!["sh".into(), p.display().to_string()]
}

fn state_job(entry: usize, position: usize, argv: Vec<String>) -> StateJob {
    StateJob { entry, position, argv }
}

fn status(argv: &[String]) -> Result<CaptureEnd, ExecError> {
    sources::capture_status(argv, &real())
}

fn v(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| x.to_string()).collect()
}

#[test]
fn state_exit_zero_is_on_other_exit_off() {
    let d = TempDir::new("tools-state");
    assert_eq!(state_of(status(&script(&d, "on", "exit 0"))), (Some(true), None));
    assert_eq!(state_of(status(&script(&d, "off", "exit 3"))), (Some(false), None), "a plain no is no note");
    let sig = status(&script(&d, "sig", "kill -9 $$"));
    assert_eq!(state_of(sig), (None, Some(Why::Signal)));
}

#[test]
fn a_hung_state_is_killed_at_1_s_and_unknown() {
    let d = TempDir::new("tools-hang");
    let t = Instant::now();
    let r = state_of(status(&script(&d, "hang", "sleep 30")));
    let took = t.elapsed();
    assert!(took >= LIST_DEADLINE && took < Duration::from_millis(1800), "{took:?}");
    assert_eq!(r, (None, Some(Why::TimedOut)), "a timeout is not an off");
}

#[test]
fn a_missing_state_program_is_unknown() {
    let r = status(&v(&["/nonexistent/alpheus-fake-state"]));
    assert_eq!(state_of(r), (None, Some(Why::NotFound)));
    let fake = FakeExecutor::new();
    fake.capture_err("x", ExecError::Io("secret path /home/x".into()));
    let (state, why) = state_of(sources::capture_status(&v(&["x"]), &fake));
    assert_eq!((state, why), (None, Some(Why::CouldNotStart)));
    assert!(!why.unwrap().to_string().contains("secret"));
}

#[test]
fn a_chatty_state_hitting_the_cap_is_unknown() {
    let d = TempDir::new("tools-chatty");
    let r = state_of(status(&script(&d, "chat", "while :; do echo spam; done")));
    assert_eq!(r, (None, Some(Why::Capped)));
}

#[test]
fn the_whys_read_as_the_plan_says() {
    assert_eq!(Why::NotFound.to_string(), "program not found");
    assert_eq!(Why::ExitCode(4).to_string(), "exited with code 4");
    assert_eq!(Why::Signal.to_string(), "killed by a signal");
    assert_eq!(Why::TimedOut.to_string(), "timed out after 1 s");
    assert_eq!(Why::Capped.to_string(), "output cap reached (2000 lines or 256 KiB)");
}

fn lines_of(argv: &[String]) -> (Vec<String>, bool, Option<Why>) {
    choices_of(sources::capture_lines(argv, &real()))
}

#[test]
fn choices_are_cleaned_lines_in_order() {
    let d = TempDir::new("tools-choices");
    let argv = script(&d, "c", "printf 'beta\\n\\n  \\nalpha\\r\\nbeta\\nga\\033mma\\n'");
    let (lines, failed, why) = lines_of(&argv);
    assert_eq!((lines, failed, why), (v(&["beta", "alpha", "gamma"]), false, None), "blank and repeated lines out, controls cleaned");
}

#[test]
fn choices_capped_keep_the_lines_read() {
    let d = TempDir::new("tools-flood");
    let (lines, failed, why) = lines_of(&script(&d, "f", "i=0; while :; do i=$((i+1)); echo \"line $i\"; done"));
    assert_eq!((lines.len(), failed, why), (MAX_SOURCE_LINES, false, Some(Why::Capped)));
    assert_eq!(lines[0], "line 1");
}

#[test]
fn a_failing_choices_cmd_with_no_line_is_failed() {
    let d = TempDir::new("tools-fail");
    assert_eq!(lines_of(&script(&d, "f", "exit 2")), (Vec::new(), true, Some(Why::ExitCode(2))));
    // With lines before the failure they are kept, and it is not "failed".
    assert_eq!(lines_of(&script(&d, "g", "echo one\nexit 2")), (v(&["one"]), false, Some(Why::ExitCode(2))));
    // A clean empty answer is an empty level, not a failure.
    assert_eq!(lines_of(&script(&d, "h", "exit 0")), (Vec::new(), false, None));
    assert_eq!(choices_of(Err(ExecError::NotFound)), (Vec::new(), true, Some(Why::NotFound)));
}

#[test]
fn states_arrive_one_by_one() {
    let d = TempDir::new("tools-one");
    let jobs = vec![
        state_job(0, 1, script(&d, "a", "exit 0")),
        state_job(1, 2, script(&d, "b", "sleep 0.5; exit 1")),
        state_job(2, 3, script(&d, "c", "exit 0")),
    ];
    let t = Instant::now();
    let mut seen = Vec::new();
    let last =
        run_job(Job::States(jobs, Counts::default()), &real(), &AtomicBool::new(false), &mut |e| seen.push((t.elapsed(), e)), &mut |_| {});
    assert_eq!(seen.len(), 3);
    assert!(seen[0].0 < Duration::from_millis(400), "the first state does not wait for the second: {:?}", seen[0].0);
    let states: Vec<_> = seen.into_iter().map(|(_, e)| e).collect();
    assert_eq!(
        states,
        [
            JobEvent::State { entry: 0, state: Some(true) },
            JobEvent::State { entry: 1, state: Some(false) },
            JobEvent::State { entry: 2, state: Some(true) },
        ]
    );
    assert_eq!(last, JobEvent::StatesDone { unread: Vec::new(), counts: Counts { on: 2, off: 1, unknown: 0 } });
}

#[test]
fn preempt_stops_after_the_current_entry_and_returns_the_rest() {
    let d = TempDir::new("tools-preempt");
    let jobs: Vec<StateJob> = (0..4).map(|i| state_job(i, i + 1, script(&d, &format!("s{i}"), "exit 0"))).collect();
    let preempt = AtomicBool::new(false);
    let mut seen = Vec::new();
    let last = run_job(
        Job::States(jobs.clone(), Counts::default()),
        &real(),
        &preempt,
        &mut |e| {
            // A choices job starts waiting while the second entry's state was just read.
            if matches!(e, JobEvent::State { entry: 1, .. }) {
                preempt.store(true, Ordering::Release);
            }
            seen.push(e);
        },
        &mut |_| {},
    );
    assert_eq!(seen.len(), 2, "the current entry finished, no other started");
    assert_eq!(last, JobEvent::StatesDone { unread: jobs[2..].to_vec(), counts: Counts { on: 2, off: 0, unknown: 0 } });
    // Set before a job starts, it still reads its first entry.
    let last = run_job(Job::States(jobs.clone(), Counts::default()), &real(), &preempt, &mut |_| {}, &mut |_| {});
    assert_eq!(last, JobEvent::StatesDone { unread: jobs[1..].to_vec(), counts: Counts { on: 1, off: 0, unknown: 0 } });
}

/// A preempted slice's counts travel with the unread entries: the slice that finishes the menu logs
/// the whole menu's totals, once.
#[test]
fn a_resumed_slice_logs_the_whole_menus_totals() {
    let d = TempDir::new("tools-resume");
    let jobs = vec![
        state_job(0, 1, script(&d, "a", "exit 0")),
        state_job(1, 2, script(&d, "b", "exit 1")),
        state_job(2, 3, script(&d, "c", "exit 0")),
        state_job(3, 4, vec!["/nonexistent/x".into()]),
    ];
    let preempt = AtomicBool::new(true);
    let mut notes = Vec::new();
    let JobEvent::StatesDone { unread, counts } =
        run_job(Job::States(jobs, Counts::default()), &real(), &preempt, &mut |_| {}, &mut |n| notes.push(n.to_string()))
    else {
        panic!()
    };
    assert_eq!((unread.len(), counts), (3, Counts { on: 1, off: 0, unknown: 0 }));
    assert!(notes.is_empty(), "a preempted slice writes no summary: {notes:?}");
    // Resumed (the middle slice is preempted again after one more entry).
    let JobEvent::StatesDone { unread, counts } =
        run_job(Job::States(unread, counts), &real(), &preempt, &mut |_| {}, &mut |n| notes.push(n.to_string()))
    else {
        panic!()
    };
    assert_eq!((unread.len(), counts), (2, Counts { on: 1, off: 1, unknown: 0 }));
    preempt.store(false, Ordering::Release);
    let last = run_job(Job::States(unread, counts), &real(), &preempt, &mut |_| {}, &mut |n| notes.push(n.to_string()));
    assert_eq!(last, JobEvent::StatesDone { unread: Vec::new(), counts: Counts { on: 2, off: 1, unknown: 1 } });
    assert_eq!(notes, ["tools: entry 4: state program not found", "tools: states read (2 on, 1 off, 1 unknown)"]);
}

#[test]
fn notes_name_the_position_only() {
    let d = TempDir::new("tools-notes");
    let jobs = vec![
        state_job(0, 4, script(&d, "secret-cmd-a", "echo secret-line; exit 0")),
        state_job(1, 7, vec!["/nonexistent/secret-cmd".into(), "secret-name".into()]),
        state_job(2, 9, script(&d, "secret-cmd-b", "sleep 30")),
    ];
    let mut notes = Vec::new();
    run_job(Job::States(jobs, Counts::default()), &real(), &AtomicBool::new(false), &mut |_| {}, &mut |n| notes.push(n.to_string()));
    let choices = Job::Choices { entry: 3, position: 12, argv: script(&d, "secret-cmd-c", "echo secret-line\nexit 5"), token: 1 };
    run_job(choices, &real(), &AtomicBool::new(false), &mut |_| {}, &mut |n| notes.push(n.to_string()));
    assert_eq!(
        notes,
        [
            "tools: entry 7: state program not found",
            "tools: entry 9: state timed out after 1 s",
            "tools: states read (1 on, 0 off, 2 unknown)",
            "tools: entry 12: choices exited with code 5, 1 kept",
        ]
    );
    assert!(notes.iter().all(|n| !n.contains("secret")), "{notes:?}");
}

/// Collects a spawned job's events and hands the receiver back.
fn spawn(job: Job, exec: Arc<dyn Executor>) -> io::Result<mpsc::Receiver<JobEvent>> {
    let (tx, rx) = mpsc::channel();
    spawn_job(job, exec, Arc::new(AtomicBool::new(false)), move |e| {
        let _ = tx.send(e);
    })?;
    Ok(rx)
}

fn choices_job(argv: Vec<String>) -> Job {
    Job::Choices { entry: 0, position: 1, argv, token: 5 }
}

#[test]
fn one_job_at_a_time() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let d = TempDir::new("tools-one-at-a-time");
    let slow = Arc::new(real());
    let first = spawn(choices_job(script(&d, "slow", "sleep 0.5\necho a")), slow.clone()).unwrap();
    let second = spawn(choices_job(script(&d, "other", "echo b")), slow.clone());
    assert_eq!(second.err().map(|e| e.kind()), Some(io::ErrorKind::WouldBlock), "refused while the first runs");
    let e = first.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(e, JobEvent::Choices { entry: 0, token: 5, lines: v(&["a"]), failed: false });
    // The slot is free again once the last event arrives.
    let third = spawn(choices_job(script(&d, "again", "echo c")), slow).unwrap();
    assert!(matches!(third.recv_timeout(Duration::from_secs(3)).unwrap(), JobEvent::Choices { .. }));
}

/// An executor whose every capture panics, with a message that must never be logged.
struct Panics;

impl Executor for Panics {
    fn run(&self, _: &str, _: &[&str]) -> Result<alpheus_common::exec::Output, ExecError> {
        Err(ExecError::NotFound)
    }
    fn spawn(&self, _: &str, _: &[&str]) -> Result<(), ExecError> {
        Err(ExecError::NotFound)
    }
    fn spawn_with_input(&self, _: &str, _: &[&str], _: &[&[u8]]) -> Result<(), ExecError> {
        Err(ExecError::NotFound)
    }
    fn spawn_with_env(&self, _: &str, _: &[&str], _: &[(&str, &str)], _: &[&str]) -> Result<(), ExecError> {
        Err(ExecError::NotFound)
    }
    fn stream(&self, _: &str, _: &[&str]) -> Result<alpheus_common::exec::Stream, ExecError> {
        Err(ExecError::NotFound)
    }
    fn capture(&self, _: &str, _: &[&str], _: alpheus_common::exec::CaptureLimits) -> Result<Captured, ExecError> {
        panic!("secret-payload")
    }
    fn run_silent(&self, _: &str, _: &[&str]) -> Result<Option<i32>, ExecError> {
        Err(ExecError::NotFound)
    }
}

#[test]
fn a_panicking_job_is_caught_and_frees_the_slot() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let rx = spawn(choices_job(v(&["x"])), Arc::new(Panics)).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), JobEvent::Choices { entry: 0, token: 5, lines: Vec::new(), failed: true });
    let rx = spawn(Job::States(vec![state_job(0, 1, v(&["x"]))], Counts::default()), Arc::new(Panics)).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), JobEvent::StatesDone { unread: Vec::new(), counts: Counts::default() });
}

/// The slot is free by the time the last event is delivered: a delivery that starts the next job
/// (the glue's waiting one) never finds it taken. Checked from inside `deliver`, on both job kinds.
#[test]
fn the_slot_is_free_when_the_last_event_is_delivered() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let d = TempDir::new("tools-free-slot");
    for first in
        [choices_job(script(&d, "c1", "echo a")), Job::States(vec![state_job(0, 1, script(&d, "s1", "exit 0"))], Counts::default())]
    {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let exec = Arc::new(real());
        let (next_exec, next) = (exec.clone(), choices_job(script(&d, "c2", "echo b")));
        let mut next = Some(next);
        spawn_job(first, exec, Arc::new(AtomicBool::new(false)), move |e| {
            if matches!(e, JobEvent::Choices { .. } | JobEvent::StatesDone { .. })
                && let Some(job) = next.take()
            {
                let done_tx = done_tx.clone();
                let started = spawn_job(job, next_exec.clone(), Arc::new(AtomicBool::new(false)), move |e| {
                    let _ = done_tx.send(e);
                });
                let _ = tx.send(started.map_err(|e| e.kind()));
            }
        })
        .unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), Ok(()), "the next job starts from inside the last delivery");
        assert!(matches!(done_rx.recv_timeout(Duration::from_secs(3)).unwrap(), JobEvent::Choices { .. }));
    }
}

/// The wallpaper folder is read here like a `choices_cmd`'s output: its image names, sorted, as a
/// choices read's lines; a folder that cannot be read is a failed read with one fixed note.
#[test]
fn a_folder_job_lists_names_like_a_choices_cmd() {
    let d = TempDir::new("tools-folder");
    for f in ["b.png", "secret notes.txt", "a.JPG"] {
        std::fs::write(d.join(f), b"").unwrap();
    }
    let mut notes = Vec::new();
    let job = Job::Folder { entry: 3, dir: d.to_path_buf(), token: 9 };
    let last = run_job(job, &FakeExecutor::new(), &AtomicBool::new(false), &mut |_| {}, &mut |n| notes.push(n.to_string()));
    assert_eq!(last, JobEvent::Choices { entry: 3, token: 9, lines: v(&["a.JPG", "b.png"]), failed: false });
    assert!(notes.is_empty(), "{notes:?}");
    let missing = Job::Folder { entry: 3, dir: d.join("secret-gone"), token: 10 };
    let last = run_job(missing, &FakeExecutor::new(), &AtomicBool::new(false), &mut |_| {}, &mut |n| notes.push(n.to_string()));
    assert_eq!(last, JobEvent::Choices { entry: 3, token: 10, lines: Vec::new(), failed: true });
    assert_eq!(notes, ["tools: folder read failed"]);
}

/// A folder of more images than the level holds is cut at `MAX_FILES` names, and one fixed note
/// says so (the count kept, never the folder or a name), as a cut `choices_cmd` does.
#[test]
fn a_folder_cut_at_the_cap_says_so_in_a_fixed_note() {
    let d = TempDir::new("tools-folder-cut");
    for i in 0..=crate::wallpaper::MAX_FILES {
        std::fs::write(d.join(format!("secret-{i:04}.png")), b"").unwrap();
    }
    let mut notes = Vec::new();
    let job = Job::Folder { entry: 1, dir: d.to_path_buf(), token: 2 };
    let last = run_job(job, &FakeExecutor::new(), &AtomicBool::new(false), &mut |_| {}, &mut |n| notes.push(n.to_string()));
    let JobEvent::Choices { lines, failed, .. } = last else { panic!("a choices read: {last:?}") };
    assert_eq!((lines.len(), failed), (crate::wallpaper::MAX_FILES, false));
    assert_eq!(notes, ["tools: folder cut, 2000 kept"]);
}
