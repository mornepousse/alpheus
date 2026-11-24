//! Tests of the kill list's pure half (`processes.rs`): a synthetic `/proc` tree in a temporary
//! folder, the rows built from two samples, the search, and the signal that checks a pid's start
//! time first. Nothing here reads the real `/proc` or signals anything: the signal goes to a fake
//! that records what it was asked.

use super::*;
use crate::launcher::matching::{Ranker, Source};
use crate::launcher::testing::TempDir;
use std::cell::RefCell;
use std::fs;

const ME: u32 = 1000;

/// What one fake process looks like in the tree.
struct Fake<'a> {
    pid: i32,
    comm: &'a str,
    uid: u32,
    ticks: u64,
    start: u64,
    pages: u64,
    cmdline: &'a [u8],
}

impl Fake<'_> {
    fn new(pid: i32, comm: &str) -> Fake<'_> {
        Fake { pid, comm, uid: ME, ticks: 0, start: 500, pages: 100, cmdline: b"" }
    }
}

/// Writes `fake` under `root` as `/proc/<pid>/{stat,status,statm,cmdline}`.
fn write(root: &Path, f: &Fake) {
    let dir = root.join(f.pid.to_string());
    fs::create_dir_all(&dir).unwrap();
    // Fields 4 to 22 after the comm: parent, group, session, tty, tty group, flags, four fault
    // counts, utime, stime, two child times, priority, nice, threads, timer, start time.
    let stat = format!("{} ({}) S 1 {} {} 0 -1 0 0 0 0 0 {} 0 0 0 20 0 1 0 {} 0 0\n", f.pid, f.comm, f.pid, f.pid, f.ticks, f.start);
    fs::write(dir.join("stat"), stat).unwrap();
    fs::write(dir.join("status"), format!("Name:\t{}\nUid:\t{}\t{}\t{}\t{}\n", f.comm, f.uid, f.uid, f.uid, f.uid)).unwrap();
    fs::write(dir.join("statm"), format!("1000 {} 10 1 0 5 0\n", f.pages)).unwrap();
    fs::write(dir.join("cmdline"), f.cmdline).unwrap();
}

fn write_uptime(root: &Path, centiseconds: u64) {
    fs::write(root.join("uptime"), format!("{}.{:02} 99.00\n", centiseconds / 100, centiseconds % 100)).unwrap();
}

fn tree(name: &str) -> TempDir {
    let root = TempDir::new(name);
    write_uptime(&root, 10_000);
    root
}

fn none() -> Excluded {
    Excluded { pids: Vec::new(), comms: Vec::new() }
}

fn names(rows: &[ProcRow]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

#[test]
fn only_the_users_processes_are_listed() {
    let root = tree("proc-uid");
    write(&root, &Fake::new(200, "mine"));
    write(&root, &Fake { uid: 0, ..Fake::new(201, "theirs") });
    write(&root, &Fake { uid: 1001, ..Fake::new(202, "other-user") });
    let samples = snapshot(&root, ME, MAX_PROCESSES);
    assert_eq!(rows(None, &samples, &none()).iter().map(|r| r.pid).collect::<Vec<_>>(), [200]);
}

#[test]
fn a_comm_with_spaces_and_parentheses_still_parses() {
    let root = tree("proc-comm");
    write(&root, &Fake { ticks: 7, start: 42, ..Fake::new(300, "a b) (c") });
    let samples = snapshot(&root, ME, MAX_PROCESSES);
    assert_eq!(samples.len(), 1);
    assert_eq!((samples[0].name.as_str(), samples[0].ticks, samples[0].start), ("a b) (c", 7, 42));
}

#[test]
fn a_process_that_vanishes_mid_read_is_skipped() {
    let root = tree("proc-vanish");
    write(&root, &Fake::new(400, "stays"));
    // A directory with no files at all, and one with a stat that is not one.
    fs::create_dir_all(root.join("401")).unwrap();
    fs::create_dir_all(root.join("402")).unwrap();
    fs::write(root.join("402/stat"), "garbage").unwrap();
    fs::write(root.join("402/status"), "Uid:\t1000\t1000\t1000\t1000\n").unwrap();
    // Not a process at all.
    fs::create_dir_all(root.join("sys")).unwrap();
    let samples = snapshot(&root, ME, MAX_PROCESSES);
    assert_eq!(samples.iter().map(|s| s.pid).collect::<Vec<_>>(), [400]);
}

#[test]
fn at_most_max_processes_are_read() {
    let root = tree("proc-max");
    for pid in 500..510 {
        write(&root, &Fake::new(pid, "many"));
    }
    assert_eq!(snapshot(&root, ME, 4).len(), 4);
    assert_eq!(MAX_PROCESSES, 4096, "the bound the plan states");
}

#[test]
fn the_memory_is_the_resident_pages_in_kilobytes() {
    let root = tree("proc-rss");
    write(&root, &Fake { pages: 250, ..Fake::new(600, "big") });
    let samples = snapshot(&root, ME, MAX_PROCESSES);
    assert_eq!(samples[0].rss_kb, 250 * page_kb());
}

#[test]
fn the_command_line_is_joined_capped_and_cleaned() {
    let root = tree("proc-cmdline");
    write(&root, &Fake { cmdline: b"/bin/prog\0--flag\0va\x1b[31lue\0", ..Fake::new(700, "prog") });
    let long = vec![b'x'; 2000];
    write(&root, &Fake { cmdline: &long, ..Fake::new(701, "long") });
    let samples = snapshot(&root, ME, MAX_PROCESSES);
    let by = |pid| samples.iter().find(|s| s.pid == pid).unwrap();
    assert_eq!(by(700).cmdline, "/bin/prog --flag va[31lue", "NULs become spaces, the escape byte is dropped");
    assert_eq!(by(701).cmdline.len(), CMDLINE_MAX);
}

#[test]
fn pid_1_alpheus_and_the_session_leader_are_never_offered() {
    let root = tree("proc-excluded");
    for (pid, comm) in [(1, "init"), (200, "alpheus-demon"), (300, "leader"), (400, "editor")] {
        write(&root, &Fake::new(pid, comm));
    }
    let samples = snapshot(&root, ME, MAX_PROCESSES);
    let excluded = Excluded { pids: vec![200, 300], comms: Vec::new() };
    assert_eq!(names(&rows(None, &samples, &excluded)), ["editor"]);
    // PID 1 stays out even when nobody listed it.
    assert!(!rows(None, &samples, &none()).iter().any(|r| r.pid == 1));
}

#[test]
fn the_compositor_and_the_user_manager_are_never_offered_by_name() {
    let root = tree("proc-comms");
    for (pid, comm) in [(200, "sway"), (201, "systemd"), (202, "editor")] {
        write(&root, &Fake::new(pid, comm));
    }
    let samples = snapshot(&root, ME, MAX_PROCESSES);
    let excluded = Excluded { pids: Vec::new(), comms: vec!["sway".into(), "systemd".into()] };
    assert_eq!(names(&rows(None, &samples, &excluded)), ["editor"]);
}

#[test]
fn rows_sort_by_rss_then_by_cpu_once_sampled() {
    let root = tree("proc-sort");
    write(&root, &Fake { pages: 300, ..Fake::new(200, "big") });
    write(&root, &Fake { pages: 200, ..Fake::new(201, "busy") });
    write(&root, &Fake { pages: 100, ..Fake::new(202, "small") });
    let first = snapshot(&root, ME, MAX_PROCESSES);
    let unsampled = rows(None, &first, &none());
    assert_eq!(names(&unsampled), ["big", "busy", "small"]);
    assert!(unsampled.iter().all(|r| r.cpu_pct.is_none()), "no CPU figure before a second sample");

    // One second later: `busy` used half a core, `small` a tenth.
    let tick = clock_ticks_per_second();
    write_uptime(&root, 10_100);
    write(&root, &Fake { pages: 300, ..Fake::new(200, "big") });
    write(&root, &Fake { pages: 200, ticks: tick / 2, ..Fake::new(201, "busy") });
    write(&root, &Fake { pages: 100, ticks: tick / 10, ..Fake::new(202, "small") });
    let second = snapshot(&root, ME, MAX_PROCESSES);
    let sampled = rows(Some(&first), &second, &none());
    assert_eq!(names(&sampled), ["busy", "small", "big"]);
    let busy = sampled[0].cpu_pct.unwrap();
    assert!((busy - 50.0).abs() < 1.0, "half a core in one second, got {busy}");
    assert_eq!(sampled[2].cpu_pct, Some(0.0), "an idle process shows 0, not nothing");
}

#[test]
fn a_reused_pid_does_not_inherit_the_cpu_of_the_process_that_had_it() {
    let root = tree("proc-reuse-cpu");
    write(&root, &Fake { ticks: 10, start: 500, ..Fake::new(200, "old") });
    let first = snapshot(&root, ME, MAX_PROCESSES);
    write_uptime(&root, 10_100);
    // The same pid, another start time, more time used than the old one had: a new process, idle for the list.
    write(&root, &Fake { ticks: 5_000, start: 900, ..Fake::new(200, "new") });
    let second = snapshot(&root, ME, MAX_PROCESSES);
    let sampled = rows(Some(&first), &second, &none());
    assert_eq!(sampled[0].cpu_pct, Some(0.0), "a new start time is a new process");
}

/// A `Signaller` that records every signal and answers with a chosen start time and result.
struct Recording {
    start: Option<u64>,
    result: Result<(), SignalError>,
    sent: RefCell<Vec<(i32, Sig)>>,
}

impl Recording {
    fn new(start: Option<u64>) -> Recording {
        Recording { start, result: Ok(()), sent: RefCell::new(Vec::new()) }
    }
}

impl Signaller for Recording {
    fn signal(&self, pid: i32, sig: Sig) -> Result<(), SignalError> {
        self.sent.borrow_mut().push((pid, sig));
        self.result
    }

    fn start_time(&self, _pid: i32) -> Option<u64> {
        self.start
    }
}

#[test]
fn a_matching_start_time_sends_the_signal() {
    let fake = Recording::new(Some(500));
    assert_eq!(send(&fake, 200, 500, Sig::Term), Ok(()));
    assert_eq!(fake.sent.borrow().as_slice(), [(200, Sig::Term)]);
}

#[test]
fn a_reused_pid_is_never_signalled() {
    let fake = Recording::new(Some(900));
    assert_eq!(send(&fake, 200, 500, Sig::Kill), Err(SignalError::Reused));
    assert!(fake.sent.borrow().is_empty(), "nothing was sent to the new process");
}

#[test]
fn a_pid_that_is_gone_is_never_signalled() {
    let fake = Recording::new(None);
    assert_eq!(send(&fake, 200, 500, Sig::Term), Err(SignalError::Gone));
    assert!(fake.sent.borrow().is_empty());
}

#[test]
fn a_pid_that_could_reach_a_group_or_init_is_refused() {
    // kill(0), kill(-1) and kill(-pid) signal groups, never one program: refused before anything.
    let fake = Recording::new(Some(500));
    for pid in [i32::MIN, -200, -1, 0, 1] {
        assert_eq!(send(&fake, pid, 500, Sig::Kill), Err(SignalError::Other), "pid {pid}");
    }
    assert!(fake.sent.borrow().is_empty());
}

#[test]
fn the_signals_failure_is_passed_on() {
    let mut fake = Recording::new(Some(500));
    fake.result = Err(SignalError::Denied);
    assert_eq!(send(&fake, 200, 500, Sig::Term), Err(SignalError::Denied));
}

#[test]
fn search_matches_the_name_and_the_full_command_line() {
    let row =
        |pid, name: &str, cmdline: &str| ProcRow { pid, start: 1, name: name.into(), cmdline: cmdline.into(), rss_kb: 10, cpu_pct: None };
    let list = [
        row(1, "firefox", "/nix/store/x-firefox/bin/firefox --new-window"),
        row(2, "sleep", "sleep 600"),
        row(3, "helper", "node server.js --port 8080"),
    ];
    let mut ranker = Ranker::new();
    let found = |ranker: &mut Ranker, text: &str| -> Vec<Source> { ranker.rank_procs(text, &list).iter().map(|h| h.source).collect() };
    assert_eq!(found(&mut ranker, ""), [Source::Proc(0), Source::Proc(1), Source::Proc(2)], "blank: every row, in order");
    assert_eq!(found(&mut ranker, "fire"), [Source::Proc(0)], "by name");
    assert_eq!(found(&mut ranker, "8080"), [Source::Proc(2)], "by the command line only");
    assert!(found(&mut ranker, "zzzz").is_empty());
}
