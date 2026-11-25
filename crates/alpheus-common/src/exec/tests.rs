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
        assert!(t.elapsed() < Duration::from_secs(10), "grandchild PID never written");
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

/// The parts reach the program's standard input in order, then its end
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
    assert!(matches!(exec().spawn_with_input("sh", &["-c", &touch], &[&half, &half]), Err(ExecError::Io(_))), "counted over every part");
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

const LIMIT: Duration = Duration::from_secs(5);

/// A standard input from a file has no size bound: a megabyte arrives whole, and the call returns
/// the program's own exit code, so a caller knows the program took its input.
#[test]
fn run_with_stdin_file_feeds_a_big_file_and_reports_the_exit() {
    let dir = std::env::temp_dir().join(format!("alpheus-stdin-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (input, got) = (dir.join("in"), dir.join("got"));
    let payload: Vec<u8> = (0..(1 << 20)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&input, &payload).unwrap();
    let script = format!("cat > '{}'", got.display());
    assert_eq!(exec().run_with_stdin_file("sh", &["-c", &script], &input, LIMIT), Ok(Some(0)));
    assert_eq!(std::fs::read(&got).unwrap(), payload, "the whole file, read before the call returned");
    assert_eq!(
        exec().run_with_stdin_file("sh", &["-c", "cat > /dev/null; exit 3"], &input, LIMIT),
        Ok(Some(3)),
        "a failing exit is reported"
    );
    assert_eq!(exec().run_with_stdin_file("sh", &["-c", "kill -9 $$"], &input, LIMIT), Ok(None), "a signal has no code");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What the program leaves running in the background (`wl-copy` forks and its foreground ends) does
/// not hold the call, and one that never ends is killed at the limit.
#[test]
fn run_with_stdin_file_returns_at_the_foreground_exit_and_kills_at_the_limit() {
    let dir = std::env::temp_dir().join(format!("alpheus-stdin-limit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in");
    std::fs::write(&input, "x").unwrap();
    let late = dir.join("late");
    let script = format!("cat > /dev/null; (sleep 1; touch '{}') & exit 0", late.display());
    let t = Instant::now();
    assert_eq!(exec().run_with_stdin_file("sh", &["-c", &script], &input, LIMIT), Ok(Some(0)));
    assert!(t.elapsed() < Duration::from_millis(900), "the background child is not waited for: {:?}", t.elapsed());
    let hang = dir.join("hung");
    let script = format!("sleep 2; touch '{}'", hang.display());
    let t = Instant::now();
    assert_eq!(exec().run_with_stdin_file("sh", &["-c", &script], &input, Duration::from_millis(200)), Err(ExecError::Timeout));
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    std::thread::sleep(Duration::from_millis(2300));
    assert!(!hang.exists(), "killed at the limit, it never got to finish");
    assert!(late.exists(), "the background child of the first run was left alone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_with_stdin_file_names_a_missing_program_or_file() {
    let dir = std::env::temp_dir().join(format!("alpheus-stdin-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let there = dir.join("there");
    std::fs::write(&there, "x").unwrap();
    assert_eq!(exec().run_with_stdin_file("alpheus-nonexistent-program", &[], &there, LIMIT), Err(ExecError::NotFound));
    let marker = dir.join("started");
    let touch = format!("touch '{}'", marker.display());
    assert!(matches!(exec().run_with_stdin_file("sh", &["-c", &touch], &dir.join("absent"), LIMIT), Err(ExecError::Io(_))));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "a missing file starts nothing");
    let _ = std::fs::remove_dir_all(&dir);
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
    // `run_with_stdin_file` discards the output: its command writes the mask to a file of its own.
    let (input, from_file) = (f.with_extension("in"), f.with_extension("file"));
    std::fs::write(&input, "x").unwrap();
    let file_script = format!("grep SigBlk /proc/self/status > {}", from_file.display());
    let stdin_file = input.clone();
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
        assert_eq!(e.run_with_stdin_file("sh", &["-c", &file_script], &stdin_file, Duration::from_secs(5)), Ok(Some(0)));
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
    let with_stdin_file = std::fs::read_to_string(&from_file).unwrap_or_default();
    let _ = std::fs::remove_file(&from_file);
    let _ = std::fs::remove_file(&input);
    for out in [run, stream, spawned, with_stdin_file] {
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

/// A command that leaves a child holding its standard output behind ends the
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

/// A command that leaves a chatty child behind (it writes on and on) ends the
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

/// A lock poisoned by a panic elsewhere never makes `capture` panic (only the
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

/// The cut is found incrementally (each byte scanned once across chunks), and
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

/// The variables reach the program, added to the inherited environment.
#[test]
fn spawn_with_env_hands_the_variables_to_the_program() {
    let dir = std::env::temp_dir().join(format!("alpheus-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (got, done) = (dir.join("got").display().to_string(), dir.join("done"));
    let script = format!("printf '%s|%s' \"$ALPHEUS_TEST_A\" \"$ALPHEUS_TEST_B\" > '{got}' && mv '{got}' '{}'", done.display());
    exec().spawn_with_env("sh", &["-c", &script], &[("ALPHEUS_TEST_A", "one two"), ("ALPHEUS_TEST_B", "é")], &[]).unwrap();
    let t = Instant::now();
    while !done.exists() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "one two|é");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_with_env_inherits_the_environment() {
    let dir = std::env::temp_dir().join(format!("alpheus-env-inh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (got, done) = (dir.join("got").display().to_string(), dir.join("done"));
    let script = format!("printf %s \"$PATH\" > '{got}' && mv '{got}' '{}'", done.display());
    exec().spawn_with_env("sh", &["-c", &script], &[("ALPHEUS_TEST_A", "x")], &[]).unwrap();
    let t = Instant::now();
    while !done.exists() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!std::fs::read_to_string(&done).unwrap().is_empty(), "PATH inherited");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A name in `unset` never reaches the program, `env` still wins.
#[test]
fn spawn_with_env_removes_the_unset_names() {
    let dir = std::env::temp_dir().join(format!("alpheus-env-unset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (got, done) = (dir.join("got").display().to_string(), dir.join("done"));
    let script = format!("printf '%s|%s' \"${{HOME-gone}}\" \"${{ALPHEUS_TEST_A-gone}}\" > '{got}' && mv '{got}' '{}'", done.display());
    exec().spawn_with_env("sh", &["-c", &script], &[("ALPHEUS_TEST_A", "x")], &["HOME", "ALPHEUS_TEST_A"]).unwrap();
    let t = Instant::now();
    while !done.exists() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "gone|x");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_with_env_refuses_a_bad_variable_before_starting() {
    let dir = std::env::temp_dir().join(format!("alpheus-env-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("started");
    let touch = format!("touch '{}'", marker.display());
    for env in [("", "x"), ("A=B", "x"), ("A\0", "x"), ("A", "x\0y")] {
        assert!(matches!(exec().spawn_with_env("sh", &["-c", &touch], &[env], &[]), Err(ExecError::Io(_))), "{env:?}");
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "nothing started");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_with_env_of_a_missing_program_is_not_found() {
    assert_eq!(exec().spawn_with_env("alpheus-nonexistent-program", &[], &[("A", "x")], &[]), Err(ExecError::NotFound));
}

#[test]
fn env_ok_refuses_what_command_envs_cannot_carry() {
    assert!(env_ok(&[("ALPHEUS_EVENT", "dnd"), ("ALPHEUS_X", "")]));
    assert!(!env_ok(&[("A\0B", "v")]), "NUL in a name");
    assert!(!env_ok(&[("A", "v\0w")]), "NUL in a value");
    assert!(!env_ok(&[("", "v")]), "an empty name");
    assert!(!env_ok(&[("A=B", "v")]), "a name holding =");
    assert!(!env_ok(&[("A", "v"), ("B=", "v")]), "one bad pair among good ones");
}

/// A command that writes a lot, late, on its standard output and error still
/// completes (they are `/dev/null`, never a pipe that closes under it) and its code is returned.
#[test]
fn run_silent_waits_and_a_late_writer_never_meets_a_closed_pipe() {
    let f = std::env::temp_dir().join(format!("alpheus-silent-{}-late", std::process::id()));
    let _ = std::fs::remove_file(&f);
    let script =
        format!("sleep 0.3; head -c 1 /dev/zero || exit 9; head -c 1 /dev/zero >&2 || exit 9; echo done > {}; exit 7", f.display());
    let t = Instant::now();
    assert_eq!(exec().run_silent("sh", &["-c", &script]), Ok(Some(7)));
    assert!(t.elapsed() >= Duration::from_millis(250), "it waited for the command: {:?}", t.elapsed());
    assert_eq!(std::fs::read_to_string(&f).unwrap().trim(), "done", "the command ran to its end");
    let _ = std::fs::remove_file(&f);
}

#[test]
fn run_silent_reports_a_signal_a_missing_program_and_none_after_closing() {
    assert_eq!(exec().run_silent("sh", &["-c", "kill -9 $$"]), Ok(None));
    assert_eq!(exec().run_silent("alpheus-nonexistent-program", &[]), Err(ExecError::NotFound));
    let e = exec();
    e.stop_running();
    assert!(matches!(e.run_silent("sh", &["-c", "true"]), Err(ExecError::Io(_))));
}

/// `stop_running` kills the group of a command in progress (and its children), and the waiter returns.
#[test]
fn stop_running_kills_a_silent_command_s_group() {
    let f = std::env::temp_dir().join(format!("alpheus-grandchild-{}-silent-stop", std::process::id()));
    let _ = std::fs::remove_file(&f);
    let script = format!("sleep 30 & echo $! > {}; wait", f.display());
    let e = Arc::new(exec());
    let waiter = {
        let e = e.clone();
        std::thread::spawn(move || e.run_silent("sh", &["-c", &script]))
    };
    let pid = pid_written(&f);
    let _ = std::fs::remove_file(&f);
    e.stop_running();
    assert_eq!(waiter.join().unwrap(), Ok(None), "ended by the kill");
    wait_for_death(pid);
}

/// A private scratch folder of one test, emptied first.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("alpheus-watched-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `fd` turns readable within `ms` milliseconds.
fn turns_readable(fd: &std::os::fd::OwnedFd, ms: i32) -> bool {
    use std::os::fd::AsRawFd;
    readable(fd.as_raw_fd(), ms) == Ok(true)
}

/// A watched program's end is told by its descriptor, which stays quiet while it runs, and its code
/// is read once it has ended.
#[test]
fn a_watched_program_signals_its_end_on_its_descriptor_and_gives_its_code() {
    let mut child = exec().spawn_watched("sh", &["-c", "sleep 0.3; exit 3"], None).unwrap();
    let fd = child.exit_fd().expect("a pidfd");
    assert!(!turns_readable(&fd, 50), "quiet while it runs");
    assert_eq!(child.try_exit(), None, "still running");
    assert!(turns_readable(&fd, 3000), "readable once it ended");
    assert_eq!(child.try_exit(), Some(Some(3)));
    assert_eq!(child.try_exit(), Some(Some(3)), "the same answer once reaped");
    let mut killed = exec().spawn_watched("sh", &["-c", "kill -9 $$"], None).unwrap();
    assert_eq!(killed.wait_exit(Duration::from_secs(3)), Some(None), "a signal has no code");
}

/// SIGINT reaches the watched program (its trap runs and it ends as it chooses) and never another
/// program: a second watched one keeps running until it is dropped, which kills it.
#[test]
fn interrupt_reaches_the_watched_program_only() {
    let dir = scratch("interrupt");
    let (marker, other_pid) = (dir.join("interrupted"), dir.join("other.pid"));
    let trap = format!("trap 'echo int > {}; exit 0' INT; while :; do sleep 0.05; done", marker.display());
    let mut recorder = exec().spawn_watched("sh", &["-c", &trap], None).unwrap();
    let mut other = exec().spawn_watched("sh", &["-c", &format!("echo $$ > {}; exec sleep 30", other_pid.display())], None).unwrap();
    let pid = pid_written(&other_pid);
    // The shell installs its trap as it starts: give it that moment.
    std::thread::sleep(Duration::from_millis(200));
    assert!(recorder.interrupt());
    assert_eq!(recorder.wait_exit(Duration::from_secs(3)), Some(Some(0)), "its trap ended it with its own code");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "int");
    assert!(!recorder.interrupt(), "an ended program is not signalled");
    assert_eq!(other.try_exit(), None, "the other program never got it");
    assert!(alive(pid));
    drop(other);
    wait_for_death(pid);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The interrupt goes to the program's own process group: what a wrapper script runs in the foreground gets
/// it too (a non-interactive shell's background jobs ignore SIGINT, as POSIX wants).
#[test]
fn interrupt_reaches_what_the_watched_program_started() {
    let dir = scratch("interrupt-group");
    let inner = dir.join("inner.pid");
    let script = format!("sh -c 'echo $$ > {}; exec sleep 30'; echo after", inner.display());
    let mut wrapper = exec().spawn_watched("sh", &["-c", &script], None).unwrap();
    let pid = pid_written(&inner);
    assert!(wrapper.interrupt());
    wait_for_death(pid);
    assert!(wrapper.wait_exit(Duration::from_secs(3)).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `wait_exit` gives up at its limit, and the program is killed with its group when it is dropped.
#[test]
fn wait_exit_stops_at_its_limit_and_a_drop_kills() {
    let dir = scratch("limit");
    let pid_file = dir.join("pid");
    let mut child = exec().spawn_watched("sh", &["-c", &format!("echo $$ > {}; exec sleep 30", pid_file.display())], None).unwrap();
    let pid = pid_written(&pid_file);
    let t = Instant::now();
    assert_eq!(child.wait_exit(Duration::from_millis(200)), None);
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    drop(child);
    wait_for_death(pid);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The standard output goes to the file given (private, written from its start), else nowhere; the
/// standard input is empty.
#[test]
fn a_watched_program_writes_its_output_to_the_file_given() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("stdout");
    let out = dir.join("out.txt");
    std::fs::write(&out, "old content that is longer").unwrap();
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut child = exec().spawn_watched("sh", &["-c", "cat; printf 'some text'; echo err >&2"], Some(&out)).unwrap();
    assert_eq!(child.wait_exit(Duration::from_secs(3)), Some(Some(0)));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "some text", "only its standard output, from the start");
    assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o600);
    let mut none = exec().spawn_watched("sh", &["-c", "printf x"], None).unwrap();
    assert_eq!(none.wait_exit(Duration::from_secs(3)), Some(Some(0)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_watched_names_a_missing_program_and_starts_nothing_after_closing() {
    assert_eq!(exec().spawn_watched("alpheus-nonexistent-program", &[], None).err(), Some(ExecError::NotFound));
    let e = exec();
    e.stop_running();
    assert!(matches!(e.spawn_watched("sh", &["-c", "true"], None), Err(ExecError::Io(_))));
}

/// A watched program starts with no signal blocked (the daemon blocks SIGINT in every thread: without
/// this, the interrupt would never reach it).
#[test]
fn a_watched_program_starts_with_no_blocked_signal() {
    let dir = scratch("sigblk");
    let out = dir.join("mask");
    let mask_file = out.clone();
    let exit = std::thread::spawn(move || {
        // SAFETY: as in `children_start_with_no_blocked_signal`: this test thread's mask only.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        // Waited for on this thread: the program is interrupted when the thread that started it ends.
        let mut child = exec().spawn_watched("grep", &["SigBlk", "/proc/self/status"], Some(&mask_file)).unwrap();
        child.wait_exit(Duration::from_secs(3))
    })
    .join()
    .unwrap();
    assert_eq!(exit, Some(Some(0)));
    assert_eq!(std::fs::read_to_string(dir.join("mask")).unwrap().trim(), "SigBlk:\t0000000000000000");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The daemon gone (the thread that started the program ended without stopping it), the program gets
/// SIGINT: a recorder finishes its file instead of running on with nobody to stop it.
#[test]
fn a_watched_program_is_interrupted_when_its_thread_ends() {
    let dir = scratch("orphan");
    let (pid_file, marker) = (dir.join("pid"), dir.join("interrupted"));
    let script =
        format!("trap 'echo int > {}; exit 0' INT; echo $$ > {}; while :; do sleep 0.05; done", marker.display(), pid_file.display());
    let child = std::thread::spawn(move || {
        let child = exec().spawn_watched("sh", &["-c", &script], None).unwrap();
        // The shell's trap is in place once it wrote its pid.
        std::thread::sleep(Duration::from_millis(300));
        // Not dropped (a drop kills): forgotten, as a daemon killed outright forgets it.
        std::mem::forget(child);
    });
    child.join().unwrap();
    let pid = pid_written(&pid_file);
    wait_for_death(pid);
    assert_eq!(std::fs::read_to_string(&marker).unwrap_or_default().trim(), "int", "it ended through its trap");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The interrupt is sent only to a program not reaped yet: once reaped its pid may be another
/// process's, and no test can make the kernel reuse it on cue, so the order is read in the source.
#[test]
fn an_interrupt_is_never_sent_to_a_reaped_pid() {
    let source = include_str!("../exec.rs");
    let start = source.find("    fn interrupt(&mut self) -> bool {").expect("the watched interrupt");
    let body = &source[start..start + source[start..].find("\n    }\n").expect("its end")];
    let checked = body.find("if self.try_exit().is_some() {").expect("the end is checked first");
    let signalled = body.find("signal_group_and_pid(self.child.id(), libc::SIGINT)").expect("then the group is signalled");
    assert!(checked < signalled, "{body}");
    assert!(body[checked..signalled].contains("return false;"), "an ended program gets nothing");
}

/// A released program runs on: the handle goes without the kill a drop sends (the daemon's shutdown
/// lets an interrupted recorder finish its file rather than kill it).
#[test]
fn a_released_watched_program_is_not_killed() {
    let dir = scratch("release");
    let pid_file = dir.join("pid");
    let child = exec().spawn_watched("sh", &["-c", &format!("echo $$ > {}; exec sleep 30", pid_file.display())], None).unwrap();
    let pid = pid_written(&pid_file);
    child.release();
    std::thread::sleep(Duration::from_millis(100));
    assert!(alive(pid), "the release did not kill it");
    // SAFETY: cleanup of the program this test started, by its pid (> 1), then reaped.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A signal for a watched program goes to its process group and, when the program left it, to the
/// program itself: a recorder that moved out of its group can still be
/// stopped. A background job of a shell stands in for it here: its pid leads no group.
#[test]
fn a_program_outside_its_group_is_signalled_by_its_pid() {
    let dir = scratch("outside-group");
    let pid_file = dir.join("pid");
    let script = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
    let mut parent = exec().spawn_watched("sh", &["-c", &script], None).unwrap();
    let pid = pid_written(&pid_file);
    // SAFETY: getpgid(2) on the process this test started; no pointer.
    assert_ne!(unsafe { libc::getpgid(pid) }, pid, "it leads no group: the group signal cannot reach it");
    assert!(signal_group_and_pid(u32::try_from(pid).unwrap(), libc::SIGTERM), "sent to the pid");
    wait_for_death(pid);
    assert!(parent.wait_exit(Duration::from_secs(3)).is_some(), "its shell ends once it is gone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The watched program's interrupt and its drop's kill both fall back to its pid: a program that left
/// its group is still stopped, and a drop never waits for a program its kill missed.
#[test]
fn a_watched_program_s_signals_fall_back_to_its_pid() {
    let source = include_str!("../exec.rs");
    let start = source.find("impl Watched for WatchedChild {").expect("the watched child");
    let rest = &source[start..];
    let body = |name: &str| {
        let at = rest.find(name).expect(name);
        &rest[at..at + rest[at..].find("\n    }\n").expect("its end")]
    };
    assert!(body("    fn interrupt(&mut self) -> bool {").contains("signal_group_and_pid(self.child.id(), libc::SIGINT)"));
    assert!(body("    fn drop(&mut self) {").contains("signal_group_and_pid(self.child.id(), libc::SIGKILL)"));
}

/// A program that moved to another group of the session while a helper stays in its original one:
/// the original group still exists, so a group kill succeeds and misses the program itself. A group
/// leader cannot `setsid`, but it can `setpgid` into a group that exists: this test binary starts
/// itself (`left_group_program_entry`) to do it, with no outside tool. Its helpers die with it
/// (PDEATHSIG), so nothing is left to kill by pid. Returns the handle and the program's pid.
fn left_group_child(dir: &std::path::Path) -> (Box<dyn Watched>, i32) {
    let file = dir.join("pid");
    let exe = std::env::current_exe().unwrap();
    let exe = exe.to_str().unwrap();
    let var = format!("{LEFT_GROUP_FILE}={}", file.display());
    let child = exec().spawn_watched("env", &[&var, exe, "--exact", "exec::tests::left_group_program_entry"], None).unwrap();
    let pid = pid_written(&file);
    // SAFETY: getpgid(2) on the process this test started; no pointer.
    assert_ne!(unsafe { libc::getpgid(pid) }, pid, "it left its own group, which the helper keeps alive");
    (child, pid)
}

const LEFT_GROUP_FILE: &str = "ALPHEUS_TEST_LEFT_GROUP_FILE";

/// Not a test of its own: the program `left_group_child` runs (this binary, told by the variable).
/// It starts a helper in its group and a sleeper in a group of its own, joins that group, writes its
/// pid and waits to be killed (ten seconds at most, should nobody).
#[test]
#[allow(clippy::zombie_processes)] // killed with this program (PDEATHSIG), which is killed by its handle
fn left_group_program_entry() {
    let Some(file) = std::env::var_os(LEFT_GROUP_FILE).map(std::path::PathBuf::from) else { return };
    let sleeper = |own_group: bool| {
        let mut command = Command::new("sleep");
        command.arg("30");
        if own_group {
            command.process_group(0);
        }
        // SAFETY: the closure only makes an async-signal-safe call (prctl).
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        command.spawn().unwrap()
    };
    let _helper = sleeper(false);
    let other = sleeper(true);
    // SAFETY: setpgid(2) into the group of the child just started, which exists once spawn returned.
    assert_eq!(unsafe { libc::setpgid(0, libc::pid_t::try_from(other.id()).unwrap()) }, 0);
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, std::process::id().to_string()).unwrap();
    std::fs::rename(&tmp, &file).unwrap();
    std::thread::sleep(Duration::from_secs(10));
}

/// A drop never waits for a program its group kill missed: the program left its group, a helper
/// keeps the group alive, and the daemon's loop thread would block forever in the wait.
#[test]
fn a_drop_signals_a_program_that_left_a_group_a_helper_keeps() {
    let dir = scratch("left-group-drop");
    let (child, _) = left_group_child(&dir);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(child);
        let _ = tx.send(());
    });
    let dropped = rx.recv_timeout(Duration::from_secs(3)).is_ok();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(dropped, "the drop is still waiting for the program");
}

/// A stop reaches such a program too, not only the helper left in its old group.
#[test]
fn an_interrupt_reaches_a_program_that_left_a_group_a_helper_keeps() {
    let dir = scratch("left-group-int");
    let (mut child, _) = left_group_child(&dir);
    assert!(child.interrupt());
    let ended = child.wait_exit(Duration::from_secs(3)).is_some();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ended, "the program never saw the interrupt");
}

/// The output file is never reached through a link: a link put where the output goes is refused,
/// nothing starts, and the file it points to is left as it was.
#[test]
fn a_watched_program_s_output_never_follows_a_link() {
    let dir = scratch("nofollow");
    let (target, link) = (dir.join("target"), dir.join("out.txt"));
    std::fs::write(&target, "kept").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let started = exec().spawn_watched("sh", &["-c", "printf replaced"], Some(&link));
    assert!(matches!(started, Err(ExecError::Io(_))), "refused");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
    let _ = std::fs::remove_dir_all(&dir);
}
