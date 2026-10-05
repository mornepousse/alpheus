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
}

impl Default for DaemonOptions<'_> {
    fn default() -> Self {
        DaemonOptions { bar_toml: None, system_bus: None, pw_dump: PwDump::Silent }
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

pub fn start_daemon_with(sway: &Sway, o: &DaemonOptions) -> Daemon {
    let config = sway.runtime.join("config");
    if let Some(text) = o.bar_toml {
        fs::create_dir_all(config.join("alpheus")).unwrap();
        fs::write(config.join("alpheus/bar.toml"), text).unwrap();
    }
    let bin = fake_pw_dump(sway, o.pw_dump);
    let path = env::join_paths(std::iter::once(bin).chain(env::split_paths(&env::var_os("PATH").unwrap_or_default()))).unwrap();
    let no_bus = format!("unix:path={}", sway.runtime.join("no-system-bus").display());
    let mut child = Command::new(env!("CARGO_BIN_EXE_alpheus-demon"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", &sway.runtime)
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("XDG_CONFIG_HOME", &config)
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
    /// `restart`s (a new daemon has a new guid, which a client may refuse).
    pub address: String,
    directory: PathBuf,
}

impl PrivateBus {
    pub fn start(name: &str) -> PrivateBus {
        let directory = env::temp_dir().join(format!("alpheus-bus-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let address = format!("unix:path={}", directory.join("bus").display());
        let child = Self::spawn(&directory, &address);
        PrivateBus { child, address, directory }
    }

    /// A minimal configuration of our own, never the system's session.conf:
    /// no `<standard_session_servicedirs/>` nor any `<servicedir>`, so nothing
    /// can ever be activated on this bus; every client may own names and
    /// send to anyone (the test peers stand in for system services).
    fn config(directory: &Path, address: &str) -> PathBuf {
        let path = directory.join("bus.conf");
        let text = format!(
            "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
             \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
             <busconfig><type>session</type><listen>{address}</listen>\
             <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
             <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>\n"
        );
        fs::write(&path, text).unwrap();
        path
    }

    fn spawn(directory: &Path, address: &str) -> Child {
        match Self::spawn_program("dbus-daemon", directory, address) {
            Ok(child) => child,
            Err((pid, line)) => panic!("dbus-daemon (pid {pid}, killed and reaped): {line:?}"),
        }
    }

    /// Starts `program` as a dbus-daemon would be, and waits for its
    /// readiness line (the printed address). Ok: it is ready. Err: the
    /// child's pid and whatever it printed instead — the child is already
    /// killed and reaped by then (I3 carry-over, tray plan 1 review): a bare
    /// `.unwrap()`/`assert!` after spawning used to panic with the child
    /// still running, leaked (nothing yet holds a `PrivateBus` whose `Drop`
    /// would otherwise catch it). `program` lets a test exercise this
    /// failure path without a real dbus-daemon (which exits on its own on
    /// every failure we could otherwise produce, leaving nothing to prove
    /// the fix against).
    fn spawn_program(program: &str, directory: &Path, address: &str) -> Result<Child, (i32, String)> {
        let mut command = Command::new(program);
        command
            .args(["--nofork", "--nopidfile", "--print-address=1"])
            .arg(format!("--config-file={}", Self::config(directory, address).display()))
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
        let pid = child.id() as i32;
        // The printed address means it listens.
        let mut line = String::new();
        let _ = BufReader::new(child.stdout.take().unwrap()).read_line(&mut line);
        if line.starts_with(address) {
            return Ok(child);
        }
        // Never leaked: killed and waited on before the caller panics.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
        let _ = child.wait();
        Err((pid, line))
    }

    /// Test-only entry point for `spawn_program`'s failure path (I3
    /// carry-over, tray plan 1 review): see its doc comment.
    pub fn spawn_never_ready_for_test(program: &str, directory: &Path, address: &str) -> Result<Child, (i32, String)> {
        Self::spawn_program(program, directory, address)
    }

    /// Kills it and starts a new one at the same address: a system bus
    /// restarting (dbus.service restarted, or a crash).
    pub fn restart(&mut self) {
        self.kill();
        let _ = fs::remove_file(self.directory.join("bus")); // left behind by a SIGKILL
        self.child = Self::spawn(&self.directory, &self.address);
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

/// Another client of a `PrivateBus` (stands in for NetworkManager, logind,
/// a silent service…), on dbus-rs like the daemon. Test code only: it may
/// own names, emit signals and call anything — the daemon may not.
pub struct Peer {
    pub channel: dbus::channel::Channel,
    /// Its unique name (":1.N").
    pub name: String,
}

impl Peer {
    pub fn connect(bus: &PrivateBus) -> Peer {
        let mut channel = dbus::channel::Channel::open_private(&bus.address).unwrap();
        channel.register().unwrap();
        let name = channel.unique_name().unwrap().to_string();
        Peer { channel, name }
    }

    /// Calls and waits for the reply (5 s at most).
    pub fn call(&self, msg: dbus::Message) -> Result<dbus::Message, dbus::Error> {
        self.channel.send_with_reply_and_block(msg, Duration::from_secs(5))
    }

    /// Takes `name` on the bus (RequestName, flags 0), as a service would.
    pub fn own(&self, name: &str) {
        let m = dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName")
            .unwrap()
            .append2(name, 0u32);
        assert_eq!(self.call(m).unwrap().read1::<u32>().unwrap(), 1, "primary owner of {name}");
    }

    /// Broadcasts a signal whose only argument is `arg`.
    pub fn emit<A: dbus::arg::Append>(&self, path: &str, interface: &str, member: &str, arg: A) {
        self.channel.send(dbus::Message::new_signal(path, interface, member).unwrap().append1(arg)).unwrap();
        self.channel.flush();
    }

    /// The next method call addressed to this peer; None once the bus is gone.
    pub fn next_call(&self) -> Option<dbus::Message> {
        loop {
            match self.channel.blocking_pop_message(Duration::from_secs(3600)) {
                Ok(Some(m)) if m.msg_type() == dbus::message::MessageType::MethodCall => return Some(m),
                Ok(_) if self.channel.is_connected() => continue,
                _ => return None,
            }
        }
    }
}

#[path = "../support/busctl_wire.rs"]
pub mod busctl_wire;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// The workstation recorded on 2026-10-04 (tests/fixtures/dbus-*.json,
/// anonymized), served on a private bus by one `Peer` owning
/// NetworkManager's, UPower's and BlueZ's names: what `FakeBus::workstation`
/// is to the unit tests, over the wire. An object the recording doesn't
/// have gets `UnknownObject`, as `FakeBus` answers NotFound.
pub struct Workstation;

impl Workstation {
    pub fn serve(bus: &PrivateBus) -> Workstation {
        let peer = Peer::connect(bus);
        for name in ["org.freedesktop.NetworkManager", "org.freedesktop.UPower", "org.bluez"] {
            peer.own(name);
        }
        let read = |file: &str| fs::read_to_string(format!("{FIXTURES}/{file}")).unwrap();
        let mut replies: Vec<((String, &str), String)> = vec![
            (("/org/freedesktop/NetworkManager".into(), "GetAll"), read("dbus-nm-root.json")),
            (("/org/freedesktop/UPower".into(), "EnumerateDevices"), read("dbus-upower-devices.json")),
            (("/".into(), "GetManagedObjects"), read("dbus-bluez-objects.json")),
        ];
        for n in [10, 9, 3, 2] {
            replies.push((
                (format!("/org/freedesktop/NetworkManager/ActiveConnection/{n}"), "GetAll"),
                read(&format!("dbus-nm-active-{n}.json")),
            ));
        }
        for (device, file) in
            [("DisplayDevice", "display"), ("battery_BAT0", "bat0"), ("battery_hidpp_battery_1", "mouse"), ("line_power_AC", "ac")]
        {
            replies.push(((format!("/org/freedesktop/UPower/devices/{device}"), "GetAll"), read(&format!("dbus-upower-{file}.json"))));
        }
        thread::spawn(move || {
            while let Some(call) = peer.next_call() {
                let path = call.path().map(|p| p.to_string()).unwrap_or_default();
                let member = call.member().map(|m| m.to_string()).unwrap_or_default();
                let reply = match replies.iter().find(|((p, m), _)| *p == path && *m == member) {
                    Some((_, json)) => {
                        let mut r = call.method_return();
                        busctl_wire::push_reply(&mut r, json);
                        r
                    }
                    None => call.error(&"org.freedesktop.DBus.Error.UnknownObject".into(), c"not in the recording"),
                };
                let _ = peer.channel.send(reply);
                peer.channel.flush();
            }
        });
        Workstation
    }
}
