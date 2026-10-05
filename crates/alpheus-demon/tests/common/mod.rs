//! Tools for the daemon's integration tests: a headless sway (a disposable
//! Wayland compositor with virtual outputs), a fake Hyprland, the daemon
//! launched with its log. Nothing touches Mae's session: a temporary run
//! directory, a cleared environment, neither WAYLAND_DISPLAY nor the
//! session's signature.
#![allow(dead_code)]

use alpheus_commun::launcher_ipc::{Reply, Request, socket_path};
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
        Self::start_with(runtime, signature, &[])
    }

    /// The fixtures with their outputs renamed after headless sway's
    /// (eDP-1 → HEADLESS-1, HDMI-A-1 → HEADLESS-2, DVI-I-1 → HEADLESS-3):
    /// the fixture's focused workspace is on HDMI-A-1, so the focused output
    /// exists on the test's compositor — HEADLESS-2.
    pub fn start_on_headless(runtime: &Path, signature: &str) -> FakeHyprland {
        Self::start_with(runtime, signature, &[("eDP-1", "HEADLESS-1"), ("HDMI-A-1", "HEADLESS-2"), ("DVI-I-1", "HEADLESS-3")])
    }

    fn start_with(runtime: &Path, signature: &str, renames: &[(&str, &str)]) -> FakeHyprland {
        let rename = |text: &str| renames.iter().fold(text.to_string(), |t, (from, to)| t.replace(from, to));
        let [monitors, workspaces, clients, active] = [MONITORS, WORKSPACES, CLIENTS, ACTIVE].map(rename);
        let directory = runtime.join("hypr").join(signature);
        fs::create_dir_all(&directory).unwrap();
        let l1 = UnixListener::bind(directory.join(".socket.sock")).unwrap();
        thread::spawn(move || {
            for c in l1.incoming() {
                let Ok(mut c) = c else { return };
                let mut buf = [0u8; 256];
                let n = c.read(&mut buf).unwrap_or(0);
                let rep = match &buf[..n] {
                    b"j/monitors" => monitors.as_str(),
                    b"j/workspaces" => workspaces.as_str(),
                    b"j/clients" => clients.as_str(),
                    b"j/activewindow" => active.as_str(),
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

/// The fake `pw-dump` put first on the daemon's PATH (never the real
/// PipeWire): it writes its PID to `<runtime>/pw-dump.pid`, prints the
/// recorded initial dump inflated to the real one's size, then stays silent
/// (`Silent`) or sends the recorded deltas every 10 ms (`Busy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PwDump {
    Silent,
    Busy,
}

pub struct DaemonOptions<'a> {
    /// Written to `$XDG_CONFIG_HOME/alpheus/bar.toml` when given.
    pub bar_toml: Option<&'a str>,
    /// `DBUS_SYSTEM_BUS_ADDRESS`: a `PrivateBus`'s address; None: a path
    /// where no bus listens (the D-Bus modules show "–").
    pub system_bus: Option<&'a str>,
    pub pw_dump: PwDump,
    /// Written to $XDG_CONFIG_HOME/alpheus/launcher.toml when given.
    pub launcher_toml: Option<&'a str>,
    /// Extra .desktop files written to $XDG_DATA_HOME/applications: (file name, content).
    pub desktop_files: &'a [(&'a str, &'a str)],
}

impl Default for DaemonOptions<'_> {
    fn default() -> Self {
        DaemonOptions { bar_toml: None, system_bus: None, pw_dump: PwDump::Silent, launcher_toml: None, desktop_files: &[] }
    }
}

/// The recorded initial dump, inflated to the real one's size (≈ 600 KB on
/// the workstation, survey) by repeating its MIDI port under fresh ids: the
/// startup PSS then includes parsing a full-size first array (budget: 15 MB).
fn full_size_initial_dump(fixtures: &Path, to: &Path) {
    let initial = fs::read_to_string(fixtures.join("pw-dump-initial.json")).unwrap();
    let port = initial.lines().find(|l| l.contains("{\"id\":51,")).expect("the fixture's port").trim().trim_end_matches(',');
    let mut dump = initial.trim_end().strip_suffix(']').expect("a JSON array").trim_end().to_string();
    let mut id = 10_000;
    while dump.len() < 610_000 {
        dump.push_str(",\n  ");
        dump.push_str(&port.replacen("{\"id\":51,", &format!("{{\"id\":{id},"), 1));
        id += 1;
    }
    dump.push_str("\n]\n");
    fs::write(to, dump).unwrap();
}

fn fake_pw_dump(sway: &Sway, mode: PwDump) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let bin = sway.runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let initial = sway.runtime.join("pw-dump-initial-full.json");
    full_size_initial_dump(&fixtures, &initial);
    let then = match mode {
        PwDump::Silent => "exec sleep 100000".to_string(),
        PwDump::Busy => format!("while :; do cat '{}'; sleep 0.01; done", fixtures.join("pw-dump-deltas.json").display()),
    };
    let script = format!(
        "#!/bin/sh\n# Fake pw-dump of the daemon's tests: recorded fixtures, never PipeWire.\necho $$ > '{}'\ncat '{}'\n{then}\n",
        sway.runtime.join("pw-dump.pid").display(),
        initial.display()
    );
    let path = bin.join("pw-dump");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

pub fn start_daemon(sway: &Sway) -> Daemon {
    start_daemon_with(sway, &DaemonOptions::default())
}

/// The directory of the real `cat`, symlinks resolved: on NixOS a
/// coreutils-only store path. The fake programs need cat, sleep, mv; the
/// session's profile (wm-set, systemd-run, real applications) stays out of
/// the daemon's PATH, so no launcher activation can ever reach it.
fn coreutils_dir() -> PathBuf {
    let path = env::var_os("PATH").unwrap_or_default();
    let cat = env::split_paths(&path).map(|d| d.join("cat")).find(|p| p.is_file()).expect("cat on PATH");
    let dir = fs::canonicalize(&cat).unwrap().parent().unwrap().to_path_buf();
    // The whole point of this directory is to carry nothing the launcher
    // could activate on the session: assert it, don't just assume it.
    assert!(!dir.join("systemd-run").exists(), "coreutils dir {} also has systemd-run", dir.display());
    assert!(!dir.join("wm-set").exists(), "coreutils dir {} also has wm-set", dir.display());
    dir
}

pub fn start_daemon_with(sway: &Sway, o: &DaemonOptions) -> Daemon {
    let config = sway.runtime.join("config");
    fs::create_dir_all(config.join("alpheus")).unwrap();
    if let Some(text) = o.bar_toml {
        fs::write(config.join("alpheus/bar.toml"), text).unwrap();
    }
    if let Some(text) = o.launcher_toml {
        fs::write(config.join("alpheus/launcher.toml"), text).unwrap();
    }
    // The launcher's sources, all temporary: never the machine's
    // applications nor its history.
    let data_home = sway.runtime.join("data");
    fs::create_dir_all(data_home.join("applications")).unwrap();
    for (name, text) in o.desktop_files {
        fs::write(data_home.join("applications").join(name), text).unwrap();
    }
    let data_dirs = sway.runtime.join("data-system");
    fs::create_dir_all(&data_dirs).unwrap();
    let bin = fake_pw_dump(sway, o.pw_dump);
    let path = env::join_paths([bin, coreutils_dir()]).unwrap();
    let no_bus = format!("unix:path={}", sway.runtime.join("no-system-bus").display());
    let mut child = Command::new(env!("CARGO_BIN_EXE_alpheus-demon"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", &sway.runtime)
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_DATA_DIRS", &data_dirs)
        .env("XDG_STATE_HOME", sway.runtime.join("state"))
        .env("WAYLAND_DISPLAY", &sway.wayland)
        .env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE)
        // Never the machine's system bus: a private one, or none at all.
        .env("DBUS_SYSTEM_BUS_ADDRESS", o.system_bus.map(String::from).unwrap_or(no_bus))
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

/// One request on the daemon's launcher socket (this test's runtime dir).
pub fn launcher(sway: &Sway, request: &Request) -> Reply {
    alpheus_launch::exchange(&socket_path(&sway.runtime), request).unwrap_or_else(|e| panic!("launcher {request:?}: {e:?}"))
}

/// wtype against the headless compositor only (WAYLAND_DISPLAY = sway.wayland).
pub fn wtype(sway: &Sway, args: &[&str]) -> std::process::ExitStatus {
    wtype_spawn(sway, args).wait().expect("wtype not found: run these tests inside `nix develop`")
}

/// Like `wtype`, but does not wait: on headless sway (`WLR_LIBINPUT_NO_DEVICES=1`),
/// the seat's only keyboard capability is wtype's own virtual keyboard, gone the
/// moment its process exits — so a test that must do something (read PSS, for
/// instance) between typing and a later key, while keeping the launcher's keyboard
/// bound throughout, has to use one continuous wtype invocation (several `-s`
/// delays) and this spawn, never two separate `wtype` calls.
///
/// Returns a guard, not the raw `Child`: a test that panics (an assertion
/// between the spawn and the explicit `wait()`) must still not leave a
/// `wtype` process running against the headless compositor.
pub fn wtype_spawn(sway: &Sway, args: &[&str]) -> WtypeChild {
    let child = Command::new("wtype")
        .args(args)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("WAYLAND_DISPLAY", &sway.wayland)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("wtype not found: run these tests inside `nix develop`");
    WtypeChild(Some(child))
}

/// A spawned `wtype` process: killed and waited on drop if never explicitly
/// `wait()`-ed, so a panic anywhere in a test using it never leaks the
/// process into the headless compositor.
pub struct WtypeChild(Option<Child>);

impl WtypeChild {
    pub fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.take().unwrap().wait()
    }
}

impl Drop for WtypeChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A program that writes its argv to <runtime>/launched-<name> and exits.
pub fn fake_program(sway: &Sway, name: &str) -> PathBuf {
    let bin = sway.runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = sway.runtime.join(format!("launched-{name}"));
    let script = format!(
        "#!/bin/sh\n# Fake application of the launcher tests: records its arguments, nothing else.\nprintf '%s\\n' \"$@\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n",
        marker.display()
    );
    let path = bin.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Is `pid` alive? A zombie (waiting to be reaped) counts as dead.
pub fn process_alive(pid: i32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(st) => st.rsplit_once(')').and_then(|(_, r)| r.trim_start().chars().next()) != Some('Z'),
    }
}

/// Waits for a file holding a PID (the fake pw-dump writes its own).
pub fn wait_for_pid_file(path: &Path, timeout: Duration) -> i32 {
    let t = Instant::now();
    loop {
        if let Some(p) = fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok()) {
            return p;
        }
        assert!(t.elapsed() < timeout, "{} never written", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

/// Voluntary and involuntary context switches in a /proc status file.
fn switches(status: &str) -> u64 {
    status
        .lines()
        .filter(|l| l.starts_with("voluntary_ctxt_switches:") || l.starts_with("nonvoluntary_ctxt_switches:"))
        .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        .sum()
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

    /// Log lines containing `pattern`.
    pub fn count(&self, pattern: &str) -> usize {
        self.log.lock().unwrap().iter().filter(|l| l.contains(pattern)).count()
    }

    /// Waits for at least `n` log lines containing `pattern`.
    pub fn wait_for_count(&self, pattern: &str, n: usize, timeout: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if self.count(pattern) >= n {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// µs from the last "launcher shown … in N µs" line.
    pub fn last_open_latency_us(&self) -> Option<u64> {
        let log = self.log.lock().unwrap();
        let line = log.iter().rev().find(|l| l.contains("launcher shown on"))?;
        line.rsplit_once(" in ")?.1.strip_suffix(" µs")?.trim().parse().ok()
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
        fs::read_dir(format!("/proc/{}/task", self.pid()))
            .unwrap()
            .flatten()
            .map(|t| switches(&fs::read_to_string(t.path().join("status")).unwrap_or_default()))
            .sum()
    }

    /// Context switches of the event loop's thread only (the main thread).
    pub fn main_thread_wakeups(&self) -> u64 {
        switches(&fs::read_to_string(format!("/proc/{0}/task/{0}/status", self.pid())).unwrap_or_default())
    }

    /// Context switches of the daemon's thread named `name` (its comm:
    /// "sound", "D-Bus"…); None if no thread has that name.
    pub fn thread_wakeups(&self, name: &str) -> Option<u64> {
        let mut found = None;
        for t in fs::read_dir(format!("/proc/{}/task", self.pid())).unwrap().flatten() {
            if fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim_end() == name) {
                *found.get_or_insert(0) += switches(&fs::read_to_string(t.path().join("status")).unwrap_or_default());
            }
        }
        found
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
