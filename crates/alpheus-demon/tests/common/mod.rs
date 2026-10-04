//! Tools for the daemon's integration tests: a headless sway (a disposable
//! Wayland compositor with virtual outputs), a fake Hyprland, the daemon
//! launched with its log. Nothing touches Mae's session: a temporary run
//! directory, a cleared environment, neither WAYLAND_DISPLAY nor the
//! session's signature.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

pub struct Sway {
    pub runtime: PathBuf,
    pub wayland: String,
    ipc: PathBuf,
    child: Child,
}

impl Sway {
    pub fn start(name: &str, outputs: u32) -> Sway {
        let runtime = env::temp_dir().join(format!("alpheus-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime);
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let conf = runtime.join("sway.conf");
        fs::write(&conf, "").unwrap();
        let mut command = Command::new("sway");
        command
            .arg("-c")
            .arg(&conf)
            .env_clear()
            .env("PATH", env::var_os("PATH").unwrap_or_default())
            .env("HOME", &runtime)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_HEADLESS_OUTPUTS", outputs.to_string())
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("WLR_RENDERER", "pixman")
            // Session D-Bus bus unreachable, on purpose: without this, the
            // nixpkgs wrapper script relays through `dbus-run-session
            // sway-unwrapped …` as soon as it finds neither a variable nor
            // `$XDG_RUNTIME_DIR/bus` (our temporary directory is empty).
            // `dbus-run-session` then FORKS its own children (`dbus-daemon`,
            // the compositor): a `prctl(PDEATHSIG)` set on the process we
            // launch (see below) does not cover them, since that flag isn't
            // carried across a fork, only across an exec. With this
            // variable set (even unreachable), the wrapper execs
            // `sway-unwrapped` DIRECTLY (a plain exec, same PID we track):
            // sway runs just fine with no real bus in a headless session
            // (verified: same tests, all green). Also checked with a
            // dedicated probe, outside this repository: without this
            // variable, killing the process that launched sway leaves
            // `dbus-daemon` and the compositor orphaned despite
            // PDEATHSIG; with it, everything dies.
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/alpheus-no-bus")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own process group (pgid = its own pid): in case, despite
            // the variable above, some version of sway still went through a
            // relay that forks (defense in depth, not only for this one
            // case). `Drop` below kills the whole group, not just this PID.
            .process_group(0);
        // SAFETY: `pre_exec` itself (the closure runs in the child, between
        // the fork and the exec) — it makes only one syscall (prctl),
        // async-signal-safe, nothing else. If THIS PROCESS (the test) is
        // itself killed with no warning (SIGKILL: `Drop` does not run,
        // nothing kills sway), the kernel then sends SIGKILL to sway when we
        // die, instead of letting it survive, reparented to PID 1.
        unsafe {
            command.pre_exec(|| {
                // SAFETY: prctl(2) only writes a flag of the calling
                // process (the child, after the fork); no shared memory
                // touched.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let child = command.spawn().expect("sway not found: run these tests inside `nix develop`");
        // Stored in `Sway` right away: its Drop kills and waits for sway
        // even if the wait below panics (no zombie sway left behind).
        let mut sway = Sway { runtime: runtime.clone(), wayland: String::new(), ipc: PathBuf::new(), child };
        let start = Instant::now();
        loop {
            let names: Vec<String> =
                fs::read_dir(&runtime).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            let wayland = names.iter().find(|n| n.starts_with("wayland-") && !n.ends_with(".lock")).cloned();
            let ipc = names.iter().find(|n| n.starts_with("sway-ipc.")).map(|n| runtime.join(n));
            if let (Some(wayland), Some(ipc)) = (wayland, ipc) {
                sway.wayland = wayland;
                sway.ipc = ipc;
                return sway;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "headless sway does not start");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Command to THIS test's headless sway (its own IPC socket).
    pub fn command(&self, cmd: &str) {
        let ok = Command::new("swaymsg").arg("-s").arg(&self.ipc).arg(cmd).stdout(Stdio::null()).status().unwrap().success();
        assert!(ok, "swaymsg {cmd}");
    }

    /// Kills this sway's process group right away (see `Drop`): simulates a
    /// compositor crashing while the daemon is running, without waiting for
    /// the test to end.
    pub fn kill(&mut self) {
        let pgid = self.child.id() as i32;
        // SAFETY: as in `Drop`, kill(2) towards the group this sway
        // received at launch (`process_group(0)`).
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

impl Drop for Sway {
    fn drop(&mut self) {
        // The whole group (see `process_group(0)` above), not just this
        // PID: `dbus-run-session`, `dbus-daemon` and the compositor it
        // started share this pgid, never the test session's. SAFETY:
        // kill(2) towards a group WE created at this launch
        // (process_group(0)), never -1 nor 0.
        let pgid = self.child.id() as i32;
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.runtime);
    }
}

const MONITORS: &str = include_str!("../fixtures/hypr-monitors.json");
const WORKSPACES: &str = include_str!("../fixtures/hypr-workspaces.json");
const CLIENTS: &str = include_str!("../fixtures/hypr-clients.json");
const ACTIVE: &str = include_str!("../fixtures/hypr-activewindow.json");

/// Fake Hyprland in `<runtime>/hypr/<signature>`: fixture state, event
/// stream fed by the test.
pub struct FakeHyprland {
    stream: Arc<Mutex<Option<UnixStream>>>,
}

impl FakeHyprland {
    pub fn start(runtime: &Path, signature: &str) -> FakeHyprland {
        let directory = runtime.join("hypr").join(signature);
        fs::create_dir_all(&directory).unwrap();
        let l1 = UnixListener::bind(directory.join(".socket.sock")).unwrap();
        thread::spawn(move || {
            for c in l1.incoming() {
                let Ok(mut c) = c else { return };
                let mut buf = [0u8; 256];
                let n = c.read(&mut buf).unwrap_or(0);
                let rep = match &buf[..n] {
                    b"j/monitors" => MONITORS,
                    b"j/workspaces" => WORKSPACES,
                    b"j/clients" => CLIENTS,
                    b"j/activewindow" => ACTIVE,
                    _ => "unknown request",
                };
                let _ = c.write_all(rep.as_bytes());
            }
        });
        let stream = Arc::new(Mutex::new(None));
        let f = stream.clone();
        let l2 = UnixListener::bind(directory.join(".socket2.sock")).unwrap();
        thread::spawn(move || {
            for c in l2.incoming().flatten() {
                *f.lock().unwrap() = Some(c); // a reconnection replaces the old one
            }
        });
        FakeHyprland { stream }
    }

    /// Writes to the stream if connected; returns false otherwise.
    pub fn write(&self, lines: &str) -> bool {
        match self.stream.lock().unwrap().as_mut() {
            Some(c) => c.write_all(lines.as_bytes()).is_ok(),
            None => false,
        }
    }
}

pub struct Daemon {
    pub child: Child,
    log: Arc<Mutex<Vec<String>>>,
}

pub const SIGNATURE: &str = "fake";

pub fn start_daemon(sway: &Sway) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_alpheus-demon"))
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("HOME", &sway.runtime)
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("XDG_CONFIG_HOME", sway.runtime.join("config"))
        .env("WAYLAND_DISPLAY", &sway.wayland)
        .env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE)
        .env("ALPHEUS_POLICES", env::var_os("ALPHEUS_POLICES").unwrap_or_default())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let j = log.clone();
    let err = child.stderr.take().unwrap();
    thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            j.lock().unwrap().push(l);
        }
    });
    Daemon { child, log }
}

impl Daemon {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Waits for a log line containing `pattern`.
    pub fn wait_for(&self, pattern: &str, timeout: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if self.log.lock().unwrap().iter().any(|l| l.contains(pattern)) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// PSS in KB (/proc/<pid>/smaps_rollup).
    pub fn pss_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid())).unwrap();
        t.lines().find_map(|l| l.strip_prefix("Pss:")).and_then(|v| v.split_whitespace().next()).unwrap().parse().unwrap()
    }

    /// Number of the daemon's threads.
    pub fn threads(&self) -> usize {
        fs::read_dir(format!("/proc/{}/task", self.pid())).map(|d| d.count()).unwrap_or(0)
    }

    /// Cumulative context switches of all the daemon's threads.
    pub fn wakeups(&self) -> u64 {
        let mut n = 0;
        for t in fs::read_dir(format!("/proc/{}/task", self.pid())).unwrap().flatten() {
            let s = fs::read_to_string(t.path().join("status")).unwrap_or_default();
            for l in s.lines() {
                if l.starts_with("voluntary_ctxt_switches:") || l.starts_with("nonvoluntary_ctxt_switches:") {
                    n += l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                }
            }
        }
        n
    }

    pub fn signal(&self, sig: i32) {
        // SAFETY: kill(2) towards our own child, still ours (not reaped).
        unsafe { libc::kill(self.pid() as i32, sig) };
    }

    /// Exit code within the timeout, otherwise None.
    pub fn exit(&mut self, timeout: Duration) -> Option<i32> {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code();
            }
            thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A private D-Bus daemon for the tests: its own socket in a temporary
/// directory, the session configuration (anyone may own a name). Never the
/// user's session bus nor the system bus.
pub struct PrivateBus {
    child: Child,
    /// `unix:path=…`, without the server's guid: the same address across
    /// `restart`s (a new daemon has a new guid, which zbus would refuse).
    pub address: String,
    directory: PathBuf,
}

impl PrivateBus {
    pub fn start(name: &str) -> PrivateBus {
        let directory = env::temp_dir().join(format!("alpheus-bus-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let address = format!("unix:path={}", directory.join("bus").display());
        let child = Self::spawn(&address);
        PrivateBus { child, address, directory }
    }

    fn spawn(address: &str) -> Child {
        let mut command = Command::new("dbus-daemon");
        command
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .arg(format!("--address={address}"))
            .env_clear()
            .env("PATH", env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: as for sway (see `Sway::start`): a single prctl(2) between
        // fork and exec, so the bus dies with the test even on a SIGKILL.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let mut child = command.spawn().expect("dbus-daemon not found: run these tests inside `nix develop`");
        // The printed address means it listens.
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert!(line.starts_with(address), "dbus-daemon address: {line:?}");
        child
    }

    /// Kills it and starts a new one at the same address: a system bus
    /// restarting (dbus.service restarted, or a crash).
    pub fn restart(&mut self) {
        self.kill();
        let _ = fs::remove_file(self.directory.join("bus")); // left behind by a SIGKILL
        self.child = Self::spawn(&self.address);
    }

    /// Kills it right away (a system bus going away).
    pub fn kill(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            // SAFETY: kill(2) towards the group this dbus-daemon received at
            // launch (process_group(0)), not reaped yet: still ours.
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        }
        let _ = self.child.wait();
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        self.kill();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
