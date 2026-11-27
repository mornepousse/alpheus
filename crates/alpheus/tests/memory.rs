//! The control center's memory (`alpheus`, a part that is not resident): its PSS at start, after
//! one visit of every tab and after 20 more cycles through them, then a clean quit. The owner's
//! rule (docs/decisions.md "Every part is measured, resident or not"): every change measures every
//! part it touches, the resident daemon and the parts it starts on demand alike.
//!
//! The real binary runs in a pseudo-terminal this test opens (no terminal emulator), in its own
//! session, its environment cleared: a temporary HOME and XDG directories (no config, no daemon
//! socket), no compositor variable, no D-Bus bus, and a PATH holding only this test's fake tools.
//! Each fake prints an anonymised fixture of `alpheus-common/tests/fixtures` for the reads the tabs
//! make and fails anything else, so no real session tool ever runs and nothing is changed. The one
//! thing of the machine the binary still touches is the Diag tab's journal check, which only tries
//! to open `/var/log/journal/*/system.journal` and reads nothing from it.
//!
//! **Release only**, like the daemon's memory suite: the control center ships in release, and a
//! debug build inflates PSS. `require_release` panics with the command if it's missing:
//! `nix develop -c cargo test --release -p alpheus --test memory -- --ignored --test-threads=1`
//!
//! The bounds are first bounds, generous on purpose (docs/measurements.md "Parts that are not
//! resident" gives the three runs they come from): about twice the highest PSS measured, rounded up
//! to the megabyte, so they catch a doubling, never a boot's drift.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// First bound of the PSS at start, after the tabs and after the cycles: the highest of the first
/// six runs was 3.6 MB (2026-11-27), twice that rounded up (module doc).
const PSS_MAX_KB: u64 = 8 * 1024;
/// 20 cycles through the tabs grow the anonymous memory by at most this over the first full cycle
/// (the daemon's launcher cycles get the same megabyte).
const CYCLES_GROWTH_MAX_KB: u64 = 1024;
/// First bound of the peak resident size (`VmHWM` of /proc/<pid>/status, read before quitting):
/// 5.3 MB in the first three runs, twice that rounded up. Not `ru_maxrss` from wait4: the kernel
/// carries into it the resident size of the forked copy of this test from before the exec.
const PEAK_RSS_MAX_KB: u64 = 11 * 1024;

const COLUMNS: u16 = 120;
const ROWS: u16 = 36;
/// One visit: the tab's refresh asks the fakes (a fork and an exec each) and the answer is drawn.
const VISIT: Duration = Duration::from_millis(400);
/// After the first draw: the Home tab's first refresh answered and drawn.
const SETTLE: Duration = Duration::from_millis(1500);
const CYCLES: usize = 20;

/// Refuses to measure PSS on a debug build (module doc). Takes the flag rather than reading `cfg!`
/// itself so the decision stays a plain, testable function.
fn require_release(debug_assertions: bool) {
    assert!(
        !debug_assertions,
        "run with --release: these PSS bounds are tuned for the release binary — \
         nix develop -c cargo test --release -p alpheus --test memory -- --ignored --test-threads=1"
    );
}

#[test]
fn require_release_panics_only_on_a_debug_build() {
    require_release(false);
    let panicked = std::panic::catch_unwind(|| require_release(true)).is_err();
    assert!(panicked, "a debug build must refuse to run these bound tests");
}

/// A test's own temporary directory, removed on drop (even while a failed assert unwinds).
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("alpheus-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../alpheus-common/tests/fixtures");

fn fixture(name: &str) -> String {
    fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

/// `text` as one single-quoted shell word.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// A fake `name` in `bin`: it appends `name` to `<bin>/calls` (which tabs were really visited),
/// then for each (shell pattern over its arguments, output) prints the output with the shell's own
/// `printf` (no other program, so PATH needs nothing else), and fails any other call: a write such
/// as `wpctl set-volume` never succeeds, and never reaches the real tool.
fn fake(bin: &Path, name: &str, answers: &[(&str, String)]) {
    let calls = quoted(&bin.join("calls").display().to_string());
    let mut script = format!(
        "#!/bin/sh\n# Fake {name} of the control center's memory test: prints a fixture, changes nothing.\n\
         printf '%s\\n' {name} >> {calls}\ncase \"$*\" in\n"
    );
    for (pattern, out) in answers {
        script.push_str(&format!("  {pattern}) printf '%s' {} ;;\n", quoted(out)));
    }
    script.push_str("  *) exit 1 ;;\nesac\n");
    let path = bin.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The reads every tab makes, answered from the fixtures.
fn fake_tools(bin: &Path) {
    fs::create_dir_all(bin).unwrap();
    fake(bin, "wpctl", &[("'status'", fixture("wpctl-status.txt")), ("'get-volume @DEFAULT_AUDIO_SINK@'", "Volume: 0.45\n".into())]);
    fake(bin, "nmcli", &[("'-t radio wifi'", "enabled\n".into()), ("*'connection show --active'", fixture("nmcli-active.txt"))]);
    fake(
        bin,
        "bluetoothctl",
        &[("'show'", fixture("bluetoothctl-show.txt")), ("'devices Connected'", fixture("bluetoothctl-devices.txt"))],
    );
    fake(bin, "brightnessctl", &[("'-m --class=backlight info'", "intel_backlight,backlight,600,60%,1000\n".into())]);
    fake(bin, "powerprofilesctl", &[("'list'", fixture("powerprofilesctl-list.txt"))]);
    fake(bin, "shell-ctl", &[("'dnd-status'", "off\n".into())]);
    fake(
        bin,
        "systemctl",
        &[
            ("*'list-units --failed'*", fixture("diag-failed-system.json")),
            ("*'list-units --type=service'*", fixture("diag-failed-system.json")),
            ("*'show --property='*", fixture("diag-show-restarts.txt")),
            ("*'show-environment'", fixture("diag-show-environment.txt")),
            ("'--version'", "systemd 257 (257.6)\n".into()),
        ],
    );
    fake(
        bin,
        "systemd-analyze",
        &[
            ("'time --no-pager'", "Startup finished in 2.1s (kernel) + 5.3s (userspace) = 7.4s\n".into()),
            ("'blame --no-pager'", fixture("diag-blame.txt")),
        ],
    );
    fake(bin, "coredumpctl", &[("'list '*", fixture("diag-coredumps.json"))]);
    fake(bin, "journalctl", &[("*", fixture("diag-journal.jsonl"))]);
}

/// The control center in its pseudo-terminal: on drop, unless it was reaped, its whole session's
/// group is killed (it leads its own, `setsid` below) and reaped. Only this child, never a pattern.
struct Tui {
    pid: i32,
    master: File,
    output: Arc<Mutex<Vec<u8>>>,
    reaped: bool,
}

impl Tui {
    fn start(home: &Path) -> Tui {
        let bin = home.join("bin");
        fake_tools(&bin);
        let runtime = home.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let size = libc::winsize { ws_row: ROWS, ws_col: COLUMNS, ws_xpixel: 0, ws_ypixel: 0 };
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty(3) writes two new descriptors into the two ints; the name buffer is null
        // (not asked for), the termios null (defaults), the winsize a valid struct read only.
        let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &size) };
        assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: both descriptors were just opened by openpty and are owned here alone.
        let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let no_bus = format!("unix:path={}", runtime.join("no-bus").display());
        let mut command = Command::new(env!("CARGO_BIN_EXE_alpheus"));
        command
            .env_clear()
            .env("PATH", &bin)
            .env("HOME", home)
            .env("TERM", "xterm-256color")
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("DBUS_SESSION_BUS_ADDRESS", &no_bus)
            .env("DBUS_SYSTEM_BUS_ADDRESS", &no_bus)
            .current_dir(home)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: the closure runs in the child between fork and exec and makes only
        // async-signal-safe syscalls: setsid(2) (its own session and group, so the drop kills
        // only it), ioctl(2) TIOCSCTTY (the pty on its stdin becomes its controlling terminal, as
        // a terminal emulator does), prctl(2) PDEATHSIG (it dies if this test is killed).
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        #[allow(clippy::zombie_processes)] // reaped by pid: `wait` or the drop (waitpid)
        let child = command.spawn().expect("the alpheus binary");
        let pid = child.id() as i32;
        drop(command); // the slave's copies close here: only the child holds the pty open
        let output = Arc::new(Mutex::new(Vec::new()));
        // The pty must be drained, or the child blocks once its buffer is full. Ends on EIO when the
        // child has closed its side.
        let (mut reader, sink) = (master.try_clone().unwrap(), output.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 16384];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Tui { pid, master, output, reaped: false }
    }

    fn output_contains(&self, needle: &[u8]) -> bool {
        self.output.lock().unwrap().windows(needle.len()).any(|w| w == needle)
    }

    fn keys(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    /// PSS and anonymous memory in KB (`Pss:` and `Anonymous:` of /proc/<pid>/smaps_rollup of this
    /// child, one read); panics if it is gone.
    fn memory_kb(&self) -> Memory {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid)).expect("the control center is still running");
        let field = |name: &str| {
            t.lines()
                .find_map(|l| l.strip_prefix(name))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
                .unwrap_or_else(|| panic!("{name} in smaps_rollup"))
        };
        Memory { pss: field("Pss:"), anon: field("Anonymous:") }
    }

    /// The peak resident size in KB so far (`VmHWM:` of /proc/<pid>/status, this exec's own).
    fn peak_rss_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/status", self.pid)).expect("the control center is still running");
        t.lines().find_map(|l| l.strip_prefix("VmHWM:")).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).expect("VmHWM")
    }

    /// Waits for the exit (5 s at most): its wait status.
    fn wait(&mut self) -> i32 {
        let t = Instant::now();
        loop {
            let mut status = 0;
            // SAFETY: waitpid(2) on our own child's pid, WNOHANG: it fills the local only.
            let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if r == self.pid {
                self.reaped = true;
                return status;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "the control center did not quit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: kill(2) on the group our child leads (setsid at spawn), not reaped yet so still
        // ours, never -1 nor 0; then waitpid(2) reaps it.
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
            libc::waitpid(self.pid, std::ptr::null_mut(), 0);
        }
    }
}

/// One reading of the child's memory, in KB.
#[derive(Debug, Clone, Copy)]
struct Memory {
    pss: u64,
    anon: u64,
}

/// The figures of one run, printed and bounded together. The growth bound reads anonymous memory
/// (the heap the program keeps), not PSS, which moves by half a megabyte as other processes map
/// the same libraries (CONTRIBUTING.md: leak bounds read anonymous memory).
fn check(start: Memory, tabs: Memory, cycles: Memory, peak: u64) {
    println!(
        "alpheus PSS: start {} KB, every tab visited {} KB, after {CYCLES} more cycles {} KB; anonymous {} / {} / {} KB; peak RSS {peak} KB",
        start.pss, tabs.pss, cycles.pss, start.anon, tabs.anon, cycles.anon
    );
    assert!(start.pss <= PSS_MAX_KB, "PSS at start {} KB > {PSS_MAX_KB} KB", start.pss);
    assert!(tabs.pss <= PSS_MAX_KB, "PSS after every tab {} KB > {PSS_MAX_KB} KB", tabs.pss);
    assert!(cycles.pss <= PSS_MAX_KB, "PSS after the cycles {} KB > {PSS_MAX_KB} KB", cycles.pss);
    assert!(
        cycles.anon <= tabs.anon + CYCLES_GROWTH_MAX_KB,
        "{CYCLES} cycles through the tabs grew the anonymous memory: {} -> {} KB",
        tabs.anon,
        cycles.anon
    );
    assert!(peak <= PEAK_RSS_MAX_KB, "peak RSS {peak} KB > {PEAK_RSS_MAX_KB} KB");
}

/// Start, one visit of each tab (Tab moves to the next one, the last wraps to Home), 20 more cycles,
/// then `q`: the PSS read at start, after the visit and after the cycles, the peak before quitting.
#[test]
#[ignore = "release only, runs the real binary in a pty: cargo test --release -p alpheus --test memory -- --ignored"]
fn the_control_center_stays_light_through_every_tab() {
    require_release(cfg!(debug_assertions));
    let home = TempDir::new("memory-tui");
    let mut tui = Tui::start(&home.0);
    let t = Instant::now();
    // The alternate screen entered: ratatui is drawing.
    while !tui.output_contains(b"\x1b[?1049h") {
        assert!(t.elapsed() < Duration::from_secs(5), "the control center never drew");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(SETTLE);
    assert!(tui.output_contains(b"Volume"), "the Home tab is drawn");
    let start = tui.memory_kb();
    let tabs = 8;
    for _ in 0..tabs {
        tui.keys(b"\t");
        std::thread::sleep(VISIT);
    }
    let visited = tui.memory_kb();
    // The Diag tab, the last one, read its units: every tab was really visited.
    let calls = fs::read_to_string(home.0.join("bin/calls")).unwrap_or_default();
    for tool in ["wpctl", "nmcli", "bluetoothctl", "systemctl"] {
        assert!(calls.lines().any(|l| l == tool), "{tool} never asked: {calls}");
    }
    for _ in 0..CYCLES * tabs {
        tui.keys(b"\t");
        std::thread::sleep(Duration::from_millis(60));
    }
    std::thread::sleep(SETTLE);
    let cycled = tui.memory_kb();
    let peak = tui.peak_rss_kb();
    tui.keys(b"q");
    let status = tui.wait();
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, "q quits cleanly: wait status {status}");
    assert!(tui.output_contains(b"\x1b[?1049l"), "the terminal is restored (alternate screen left)");
    check(start, visited, cycled, peak);
}
