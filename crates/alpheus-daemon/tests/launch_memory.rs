//! The memory of `alpheus-launch` (a part that is not resident: it runs on every key press and in
//! every script menu, then exits), against the isolated harness's daemon. The owner's rule
//! (docs/decisions.md "Every part is measured, resident or not"): every change measures every part
//! it touches, the resident daemon and the parts it starts on demand alike.
//!
//! Two invocations, the real binary each time, its PSS (/proc/<pid>/smaps_rollup) and its peak
//! resident size (`VmHWM` of /proc/<pid>/status) sampled every 10 ms from this test's own child
//! while it lives. `--toggle` answers within milliseconds, too fast to sample, so the daemon (this
//! test's own child) is stopped with SIGSTOP while it waits for the reply, then continued: the
//! client has connected, sent its request and waits, as it does for any verb. A `--dmenu` of 300
//! lines lives until the menu is closed and is sampled while it waits. Not `ru_maxrss` from wait4:
//! the kernel carries into it the resident size of the forked copy of this test from before the
//! exec.
//! The binary runs with its environment cleared: this test's sway runtime directory (the daemon's
//! socket in it) and an empty PATH.
//!
//! **Release only**, like `memory.rs`: `require_release` panics with the command if it's missing.
//! The binary is this tree's release `alpheus-launch`, built by the test itself with the cargo that
//! runs it (a stale one would measure an older build), or the one `ALPHEUS_LAUNCH_UNDER_TEST`
//! names, to measure another build on the same harness:
//! `nix develop -c cargo test --release -p alpheus-daemon --test launch_memory -- --ignored --test-threads=1`
//!
//! The bounds are first bounds, generous on purpose (docs/measurements.md "Parts that are not
//! resident" gives the three runs they come from): about twice the highest figure measured, rounded
//! up to the megabyte, so they catch a doubling, never a boot's drift.

mod common;

use common::{Daemon, DaemonOptions, FakeHyprland, SIGNATURE, Sway, start_daemon_with};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_secs(10);
/// First bound of the highest PSS sampled while an invocation waits (`--toggle` or `--dmenu`): the
/// highest of the first three runs was 648 KB (2026-11-27), twice that rounded up (module doc).
const PSS_MAX_KB: u64 = 2 * 1024;
/// First bound of the peak resident size (`VmHWM`) of either invocation: 2,296 KB in the first
/// three runs, twice that rounded up.
const PEAK_RSS_MAX_KB: u64 = 5 * 1024;
/// How long the daemon stays stopped while `--toggle` waits (its reply deadline is 2 s).
const STOPPED: Duration = Duration::from_millis(300);

/// Refuses to measure on a debug build (memory.rs: a debug build inflates PSS). Takes the flag
/// rather than reading `cfg!` itself so the decision stays a plain, testable function.
fn require_release(debug_assertions: bool) {
    assert!(
        !debug_assertions,
        "run with --release: these bounds are tuned for the release binary — \
         nix develop -c cargo test --release -p alpheus-daemon --test launch_memory -- --ignored --test-threads=1"
    );
}

#[test]
fn require_release_panics_only_on_a_debug_build() {
    require_release(false);
    let panicked = std::panic::catch_unwind(|| require_release(true)).is_err();
    assert!(panicked, "a debug build must refuse to run these bound tests");
}

/// The binary under test: `ALPHEUS_LAUNCH_UNDER_TEST`, or this tree's release `alpheus-launch`,
/// built now in this build's target directory (cargo builds a dev-dependency's library, never its
/// binary). A daemon from another build (`ALPHEUS_DEMON_UNDER_TEST`) needs the launch of that build
/// named too: this tree's would mix two builds in one measurement.
fn launch_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("ALPHEUS_LAUNCH_UNDER_TEST") {
        return PathBuf::from(path);
    }
    assert!(
        std::env::var_os("ALPHEUS_DEMON_UNDER_TEST").is_none(),
        "ALPHEUS_DEMON_UNDER_TEST is set: name that build's alpheus-launch with ALPHEUS_LAUNCH_UNDER_TEST too"
    );
    let ok = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--release", "-p", "alpheus-launch", "--bin", "alpheus-launch"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("cargo")
        .success();
    assert!(ok, "cargo build --release -p alpheus-launch failed");
    PathBuf::from(env!("CARGO_BIN_EXE_alpheus-demon")).with_file_name("alpheus-launch")
}

/// One `alpheus-launch` run against this test's daemon, in its own process group: on drop, unless
/// it was reaped, that group is killed and the child reaped. Only this child, never a pattern.
struct Launch {
    pid: i32,
    reaped: bool,
}

impl Launch {
    fn spawn(binary: &PathBuf, sway: &Sway, args: &[&str], stdin: Option<&str>) -> Launch {
        #[allow(clippy::zombie_processes)] // reaped by pid: `wait` or the drop (waitpid)
        let mut child = Command::new(binary)
            .args(args)
            .env_clear()
            .env("PATH", "")
            .env("HOME", &sway.runtime)
            .env("XDG_RUNTIME_DIR", &sway.runtime)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("the alpheus-launch binary");
        if let Some(text) = stdin {
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(text.as_bytes()).unwrap();
        } // the pipe closes here: the input ends
        Launch { pid: child.id() as i32, reaped: false }
    }

    /// PSS and peak resident size in KB (`Pss:` of smaps_rollup, `VmHWM:` of status, this exec's
    /// own), or None once it has exited.
    fn memory_kb(&self) -> Option<(u64, u64)> {
        let field = |file: &str, name: &str| -> Option<u64> {
            let t = std::fs::read_to_string(format!("/proc/{}/{file}", self.pid)).ok()?;
            t.lines().find_map(|l| l.strip_prefix(name)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        };
        Some((field("smaps_rollup", "Pss:")?, field("status", "VmHWM:")?))
    }

    /// Samples every 10 ms while `go_on` holds: the highest PSS and the highest peak seen. Panics if
    /// the child exits meanwhile.
    fn sample_while(&mut self, mut go_on: impl FnMut() -> bool) -> (u64, u64) {
        let (mut pss, mut peak) = (0, 0);
        let t = Instant::now();
        while go_on() {
            assert!(t.elapsed() < DELAY, "sampled for too long");
            assert!(self.try_wait().is_none(), "alpheus-launch exited while it should wait");
            if let Some((p, h)) = self.memory_kb() {
                (pss, peak) = (pss.max(p), peak.max(h));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(pss > 0 && peak > 0, "the child was sampled");
        (pss, peak)
    }

    /// Reaps the child if it has exited: its exit code.
    fn try_wait(&mut self) -> Option<i32> {
        let mut status = 0;
        // SAFETY: waitpid(2) on our own child's pid, WNOHANG: it fills the local only.
        let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if r != self.pid {
            return None;
        }
        self.reaped = true;
        assert!(libc::WIFEXITED(status), "alpheus-launch did not exit on its own: wait status {status}");
        Some(libc::WEXITSTATUS(status))
    }

    /// Waits for the exit (`DELAY` at most): its exit code.
    fn wait(&mut self) -> i32 {
        let t = Instant::now();
        loop {
            if let Some(code) = self.try_wait() {
                return code;
            }
            assert!(t.elapsed() < DELAY, "alpheus-launch did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// SIGSTOP or SIGCONT to this test's own daemon (never another process).
fn signal_daemon(d: &Daemon, signal: i32) {
    // SAFETY: kill(2) on the pid of the daemon this test spawned and still holds (not reaped).
    let r = unsafe { libc::kill(d.pid() as i32, signal) };
    assert_eq!(r, 0, "kill: {}", std::io::Error::last_os_error());
}

impl Drop for Launch {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: kill(2) on our child's own group (process_group(0) at spawn), not reaped yet so
        // still ours, never -1 nor 0; then waitpid(2) reaps it.
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
            libc::waitpid(self.pid, std::ptr::null_mut(), 0);
        }
    }
}

fn start(name: &str) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon_with(&sway, &DaemonOptions::default());
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// `--toggle`, sampled while the stopped daemon keeps it waiting, opens the launcher once the daemon
/// is continued and exits 0; `--close` closes it; a `--dmenu` of 300 lines, sampled while it waits on
/// the open menu, exits 1 (cancelled) once `--close` closes it.
#[test]
#[ignore = "release only, headless sway (nix develop)"]
fn alpheus_launch_stays_light() {
    require_release(cfg!(debug_assertions));
    let binary = launch_binary();
    let (sway, _hypr, d) = start("launch-memory");
    std::thread::sleep(Duration::from_secs(1));

    signal_daemon(&d, libc::SIGSTOP);
    let mut toggle = Launch::spawn(&binary, &sway, &["--toggle"], None);
    let t = Instant::now();
    let sampled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| toggle.sample_while(|| t.elapsed() < STOPPED)));
    signal_daemon(&d, libc::SIGCONT); // before any panic goes on: the daemon is never left stopped
    let (toggle_pss, toggle_peak) = sampled.unwrap_or_else(|e| std::panic::resume_unwind(e));
    assert_eq!(toggle.wait(), 0, "--toggle exits 0");
    assert!(d.wait_for_count("launcher shown on", 1, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300)); // past the toggle debounce
    assert_eq!(Launch::spawn(&binary, &sway, &["--close"], None).wait(), 0, "--close exits 0");
    assert!(d.wait_for_count("launcher closed (", 1, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300));

    let lines: String = (0..300).map(|i| format!("Category {} · Entry {i}\n", i / 20)).collect();
    let mut menu = Launch::spawn(&binary, &sway, &["--dmenu", "--prompt", "Pick"], Some(&lines));
    // From the spawn until the menu has been shown for 500 ms.
    let mut shown_at: Option<Instant> = None;
    let (dmenu_pss, dmenu_peak) = menu.sample_while(|| {
        if shown_at.is_none() && d.count("launcher shown on") == 2 {
            shown_at = Some(Instant::now());
        }
        shown_at.is_none_or(|at| at.elapsed() < Duration::from_millis(500))
    });
    assert_eq!(Launch::spawn(&binary, &sway, &["--close"], None).wait(), 0, "--close exits 0");
    assert_eq!(menu.wait(), 1, "a closed menu is a cancelled one (fuzzel's contract)");

    println!(
        "alpheus-launch: --toggle PSS {toggle_pss} KB, peak RSS {toggle_peak} KB; \
         --dmenu (300 lines) PSS {dmenu_pss} KB, peak RSS {dmenu_peak} KB"
    );
    for (what, pss, peak) in [("--toggle", toggle_pss, toggle_peak), ("--dmenu", dmenu_pss, dmenu_peak)] {
        assert!(pss <= PSS_MAX_KB, "{what} PSS {pss} KB > {PSS_MAX_KB} KB");
        assert!(peak <= PEAK_RSS_MAX_KB, "{what} peak RSS {peak} KB > {PEAK_RSS_MAX_KB} KB");
    }
}
