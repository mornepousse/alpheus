//! Tools for the daemon's integration tests: a headless sway (a disposable
//! Wayland compositor with virtual outputs), a fake Hyprland, the daemon
//! launched with its log. Nothing touches the user's session: a temporary run
//! directory, a cleared environment, neither WAYLAND_DISPLAY nor the
//! session's signature.
//!
//! **Assumes NixOS.** `coreutils_dir()` (below) needs `cat` — and with it
//! `sleep`/`mv`, the fake programs' other tools — to resolve, through
//! `$PATH` and symlinks, into one shared coreutils derivation directory
//! that also carries nothing session-specific (no `wm-set`, no
//! `systemd-run`): true of any Nix-managed machine (`cat`, `sleep`, `mv`
//! all come from the same `coreutils` package, one `/nix/store/…/bin`), not
//! guaranteed on a distribution that splits these tools across separate
//! packages or directories (e.g. a minimal busybox `/bin`). This harness
//! has only ever run on a NixOS workstation; a non-NixOS contributor
//! hitting `coreutils_dir`'s assertions should read them as "this harness
//! doesn't know your distribution's layout yet", not as a daemon bug.
#![allow(dead_code)]

use alpheus_common::launcher_ipc::{Reply, Request, socket_path};
use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_pointer, wl_registry, wl_seat};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;
use std::cell::OnceCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

pub struct Sway {
    pub runtime: PathBuf,
    pub wayland: String,
    ipc: PathBuf,
    child: Child,
    /// Created by the first `click`, kept for this sway's lifetime.
    pointer: OnceCell<VirtualPointer>,
}

/// A wlr virtual pointer on THIS test's headless sway. Without an input
/// device (WLR_LIBINPUT_NO_DEVICES) the seat has no pointer capability, so no
/// client ever gets a wl_pointer and `seat - cursor press` reaches nobody
/// (checked with wayland-info: `capabilities:` empty). This device gives the
/// seat its pointer; the cursor itself is moved and pressed by `swaymsg`.
/// Connected through sway's socket path, never WAYLAND_DISPLAY.
struct VirtualPointer {
    born: Instant,
    _connection: Connection,
    _queue: EventQueue<NoEvents>,
    _pointer: ZwlrVirtualPointerV1,
}

/// The virtual pointer's protocol objects send no event this harness reads.
struct NoEvents;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for NoEvents {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(NoEvents: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(NoEvents: ignore ZwlrVirtualPointerV1);
delegate_noop!(NoEvents: ignore wl_seat::WlSeat);
delegate_noop!(NoEvents: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(NoEvents: ignore ZwpVirtualKeyboardV1);

/// The keymap of `VirtualKeyboard`: two named layouts, one key. Sway never applies its `input`
/// configuration to a virtual keyboard (the client's keymap is kept), so the layouts it reports
/// and switches between are these.
const TWO_LAYOUTS: &str = r#"xkb_keymap {
xkb_keycodes "alpheus" { minimum = 8; maximum = 255; <AC01> = 38; };
xkb_types "alpheus" { include "complete" };
xkb_compat "alpheus" { include "complete" };
xkb_symbols "alpheus" { name[Group1] = "English (US)"; name[Group2] = "French"; key <AC01> { [ a ], [ q ] }; };
};
"#;

/// A virtual keyboard on THIS test's headless sway with two layouts, "English (US)" then
/// "French" (`TWO_LAYOUTS`): the seat has no other keyboard with a named layout (wtype's keymap
/// names none), so `input type:keyboard xkb_switch_layout next` switches this one, and Sway sends
/// its `input` event. Gone with the value. Connected through sway's socket path, never
/// WAYLAND_DISPLAY.
pub struct VirtualKeyboard {
    _connection: Connection,
    _queue: EventQueue<NoEvents>,
    _keyboard: ZwpVirtualKeyboardV1,
}

impl VirtualPointer {
    fn create(sway: &Sway) -> VirtualPointer {
        let stream = UnixStream::connect(sway.runtime.join(&sway.wayland)).unwrap();
        let connection = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<NoEvents>(&connection).unwrap();
        let qh = queue.handle();
        let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).expect("zwlr_virtual_pointer_manager_v1");
        let pointer = manager.create_virtual_pointer(None, &qh, ());
        queue.roundtrip(&mut NoEvents).unwrap();
        VirtualPointer { _connection: connection, _queue: queue, _pointer: pointer, born: Instant::now() }
    }

    /// The pointer moved to (x, y) of the output at the layout's origin (`OUTPUT_SIZE`): a real
    /// motion event, which `seat - cursor set` is not while a button is held (sway sends none
    /// during the implicit grab).
    fn motion(&self, x: u32, y: u32) {
        let time = self.born.elapsed().as_millis() as u32;
        self._pointer.motion_absolute(time, x, y, OUTPUT_SIZE.0, OUTPUT_SIZE.1);
        self._pointer.frame();
        let _ = self._connection.flush();
    }

    /// Button `code` (evdev, 0x110 is the left one) down or up.
    fn button(&self, code: u32, down: bool) {
        let time = self.born.elapsed().as_millis() as u32;
        let state = if down { wl_pointer::ButtonState::Pressed } else { wl_pointer::ButtonState::Released };
        self._pointer.button(time, code, state);
        self._pointer.frame();
        let _ = self._connection.flush();
    }
}

/// The headless output's size in px: what `VirtualPointer::motion` maps its coordinates onto.
pub const OUTPUT_SIZE: (u32, u32) = (1280, 720);

impl Sway {
    pub fn start(name: &str, outputs: u32) -> Sway {
        Sway::start_with_config(name, outputs, "")
    }

    /// `start` with `config` as sway's configuration (a `mode` block, for one: Sway defines modes
    /// only while it reads its configuration).
    pub fn start_with_config(name: &str, outputs: u32, config: &str) -> Sway {
        let runtime = env::temp_dir().join(format!("alpheus-it-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime);
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let conf = runtime.join("sway.conf");
        fs::write(&conf, config).unwrap();
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
        let mut sway = Sway { runtime: runtime.clone(), wayland: String::new(), ipc: PathBuf::new(), child, pointer: OnceCell::new() };
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

    /// Whether THIS sway advertises the global `interface` (`ext_data_control_manager_v1`...): an
    /// older compositor has fewer of them, and a test that names one asks first.
    pub fn offers(&self, interface: &str) -> bool {
        let stream = UnixStream::connect(self.runtime.join(&self.wayland)).unwrap();
        let connection = Connection::from_socket(stream).unwrap();
        let (globals, _queue) = registry_queue_init::<NoEvents>(&connection).unwrap();
        globals.contents().with_list(|list| list.iter().any(|g| g.interface == interface))
    }

    /// A new `VirtualKeyboard` on this sway, once sway has its keymap (a round trip).
    pub fn keyboard(&self) -> VirtualKeyboard {
        let stream = UnixStream::connect(self.runtime.join(&self.wayland)).unwrap();
        let connection = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<NoEvents>(&connection).unwrap();
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).expect("wl_seat");
        let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).expect("zwp_virtual_keyboard_manager_v1");
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        // The keymap travels as a file descriptor, NUL-terminated; the file lives in this sway's
        // own runtime directory.
        static KEYMAPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = KEYMAPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = self.runtime.join(format!("keymap-{n}"));
        fs::write(&path, [TWO_LAYOUTS.as_bytes(), b"\0"].concat()).unwrap();
        let file = fs::File::open(&path).unwrap();
        // 1: WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1.
        keyboard.keymap(1, file.as_fd(), TWO_LAYOUTS.len() as u32 + 1);
        queue.roundtrip(&mut NoEvents).unwrap();
        VirtualKeyboard { _connection: connection, _queue: queue, _keyboard: keyboard }
    }

    /// Command to THIS test's headless sway (its own IPC socket).
    pub fn command(&self, cmd: &str) {
        let ok = Command::new("swaymsg").arg("-s").arg(&self.ipc).arg(cmd).stdout(Stdio::null()).status().unwrap().success();
        assert!(ok, "swaymsg {cmd}");
    }

    /// The first click or scroll creates the virtual pointer and leaves the
    /// clients 300 ms to bind their wl_pointer.
    fn ensure_pointer(&self) {
        if self.pointer.get().is_none() {
            let _ = self.pointer.set(VirtualPointer::create(self));
            thread::sleep(Duration::from_millis(300));
        }
    }

    /// `seat - cursor set <x> <y>` (layout coordinates), then press and release
    /// `button` ("button1" left, "button3" right, "button2" middle).
    pub fn click(&self, x: u32, y: u32, button: &str) {
        self.ensure_pointer();
        self.command(&format!("seat - cursor set {x} {y}"));
        self.command(&format!("seat - cursor press {button}"));
        self.command(&format!("seat - cursor release {button}"));
    }

    /// A drag with the left button, through the virtual pointer: down at `from`, up at `to`, the
    /// motion in between a real event. `hold` ends the drag with the button still down.
    pub fn drag(&self, from: (u32, u32), to: (u32, u32), hold: bool) {
        self.ensure_pointer();
        let Some(pointer) = self.pointer.get() else { return };
        pointer.motion(from.0, from.1);
        thread::sleep(Duration::from_millis(60));
        pointer.button(0x110, true);
        thread::sleep(Duration::from_millis(60));
        pointer.motion((from.0 + to.0) / 2, (from.1 + to.1) / 2);
        thread::sleep(Duration::from_millis(60));
        pointer.motion(to.0, to.1);
        thread::sleep(Duration::from_millis(60));
        if !hold {
            pointer.button(0x110, false);
        }
    }

    /// Lets go of the left button a `drag(.., hold = true)` kept down.
    pub fn release_drag(&self) {
        if let Some(pointer) = self.pointer.get() {
            pointer.button(0x110, false);
        }
    }

    /// `seat - cursor set <x> <y>` only: the pointer rests there (motion, no button).
    pub fn hover(&self, x: u32, y: u32) {
        self.ensure_pointer();
        self.command(&format!("seat - cursor set {x} {y}"));
    }

    /// `seat - cursor set <x> <y>`, then `notches` wheel notches towards
    /// `direction` ("up", "down", "left", "right"). sway's `cursor press
    /// button4…7` scrolls 1/120 of a notch per press, and a release scrolls
    /// again (Step 1): a notch is 120 presses, sent as one command list.
    pub fn scroll(&self, x: u32, y: u32, direction: &str, notches: u32) {
        let button = match direction {
            "up" => "button4",
            "down" => "button5",
            "left" => "button6",
            "right" => "button7",
            other => panic!("no wheel direction {other:?}"),
        };
        self.ensure_pointer();
        self.command(&format!("seat - cursor set {x} {y}"));
        let press = format!("seat - cursor press {button}");
        self.command(&vec![press.as_str(); 120 * notches as usize].join("; "));
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
        // started share this pgid, never the test session's.
        let pgid = self.child.id() as i32;
        // SAFETY: kill(2) towards a group WE created at this launch
        // (process_group(0)), never -1 nor 0.
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
    /// What `j/monitors` answers: `focus` rewrites it.
    monitors: Arc<Mutex<String>>,
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
        let monitors = Arc::new(Mutex::new(monitors));
        let served = monitors.clone();
        let directory = runtime.join("hypr").join(signature);
        fs::create_dir_all(&directory).unwrap();
        let l1 = UnixListener::bind(directory.join(".socket.sock")).unwrap();
        thread::spawn(move || {
            for c in l1.incoming() {
                let Ok(mut c) = c else { return };
                let mut buf = [0u8; 256];
                let n = c.read(&mut buf).unwrap_or(0);
                let current = served.lock().map(|m| m.clone()).unwrap_or_default();
                let rep = match &buf[..n] {
                    b"j/monitors" => current.as_str(),
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
        FakeHyprland { stream, monitors }
    }

    /// Makes `output` the focused monitor in `j/monitors` (the others unfocused). Each fixture monitor
    /// sits on a line of its own. The test then writes a `focusedmon` event, so the daemon reads it
    /// again.
    pub fn focus(&self, output: &str) {
        let mut m = self.monitors.lock().unwrap();
        let name = format!("\"name\": \"{output}\"");
        *m = m
            .lines()
            .map(|l| {
                if l.contains(&name) {
                    l.replace("\"focused\": false", "\"focused\": true")
                } else {
                    l.replace("\"focused\": true", "\"focused\": false")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
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
    /// After the initial dump, exactly what a test writes to `<runtime>/pw-dump.fifo`
    /// (`pw_dump_send`), and nothing else: no wake-up of its own (the OSD's tests).
    Fifo,
    /// An empty graph (`[]`), then silence: no sink, no source, nothing the sound source ever publishes
    /// again (Shield mode's tests: no volume row, so the Shield row is the panel's first).
    Empty,
}

/// Which compositor the daemon reads, through its environment: the tests' fake Hyprland unless a
/// test says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Backend<'a> {
    /// `HYPRLAND_INSTANCE_SIGNATURE` = `SIGNATURE`: the `FakeHyprland` every older test starts.
    #[default]
    FakeHyprland,
    /// `SWAYSOCK` = THIS headless sway's own IPC socket: the sway the bar draws on is the
    /// compositor it reads.
    Sway,
    /// `MANGO_INSTANCE_SIGNATURE` = a `FakeMango`'s socket, in this test's runtime directory.
    Mango(&'a Path),
}

pub struct DaemonOptions<'a> {
    /// Written to `$XDG_CONFIG_HOME/alpheus/bar.toml` when given.
    pub bar_toml: Option<&'a str>,
    /// `DBUS_SYSTEM_BUS_ADDRESS`: a `PrivateBus`'s address; None: a path
    /// where no bus listens (the D-Bus modules show "–").
    pub system_bus: Option<&'a str>,
    pub pw_dump: PwDump,
    /// `DBUS_SESSION_BUS_ADDRESS`: a `PrivateBus`'s address; None: a path where no bus listens (no tray).
    pub session_bus: Option<&'a str>,
    /// Written to $XDG_CONFIG_HOME/alpheus/launcher.toml when given.
    pub launcher_toml: Option<&'a str>,
    /// Extra .desktop files written to $XDG_DATA_HOME/applications: (file name, content).
    pub desktop_files: &'a [(&'a str, &'a str)],
    /// The compositor the daemon reads.
    pub backend: Backend<'a>,
}

impl Default for DaemonOptions<'_> {
    fn default() -> Self {
        DaemonOptions {
            bar_toml: None,
            system_bus: None,
            pw_dump: PwDump::Silent,
            session_bus: None,
            launcher_toml: None,
            desktop_files: &[],
            backend: Backend::FakeHyprland,
        }
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

/// A child run in its own process group (never the test's), its stderr discarded: on drop, even
/// while a failed assert unwinds, the whole group is killed and the child reaped. A fake that a
/// failure leaves behind (the `Fifo` fake's `cat`) would never exit, and would hold the caller's
/// pipe open (`cargo test | tail` hanging). Once the test reaped the child itself (`wait`,
/// `try_wait`), its pid, and so its group id, may already be someone else's: never signalled then.
pub struct GroupChild {
    child: Child,
    reaped: bool,
}

impl GroupChild {
    pub fn spawn(command: &mut Command) -> GroupChild {
        GroupChild { child: command.process_group(0).stderr(Stdio::null()).spawn().unwrap(), reaped: false }
    }

    /// `Child::wait`, remembering that the child is reaped.
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    /// `Child::try_wait`, remembering when the child is reaped.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }

    pub fn reaped(&self) -> bool {
        self.reaped
    }
}

impl std::ops::Deref for GroupChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl std::ops::DerefMut for GroupChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: plain kill(2) on our child's own group (process_group(0) at spawn), not reaped
        // yet: still ours, never -1 nor 0.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

/// A test's own temporary directory, removed on drop (even while a failed assert unwinds).
pub struct TempDir(pub PathBuf);

impl TempDir {
    /// `$TMPDIR/<name>-<pid>`, emptied first.
    pub fn new(name: &str) -> TempDir {
        let dir = env::temp_dir().join(format!("{name}-{}", std::process::id()));
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

/// The `Fifo` fake's pipe, in the runtime directory.
pub const PW_DUMP_FIFO: &str = "pw-dump.fifo";

/// The fixture's AG06 sink and source (`pw-dump-initial.json`): (id, media.class, node.name).
pub const AG06_SINK: (u32, &str, &str) = (122, "Audio/Sink", "alsa_output.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo");
pub const AG06_SOURCE: (u32, &str, &str) = (123, "Audio/Source", "alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo");
/// The fixture's laptop speaker (id 99, 94 %).
pub const SPEAKER_SINK: &str = "alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink";

/// A pw-dump delta re-dumping `node` (its fixture description kept) with both channels at
/// `percent`, as wpctl shows it (the linear volume is its cube).
pub fn volume_delta(node: (u32, &str, &str), percent: u32, mute: bool) -> String {
    let (id, class, name) = node;
    let linear = (f64::from(percent) / 100.0).powi(3);
    format!(
        r#"[{{"id":{id},"type":"PipeWire:Interface:Node","info":{{"props":{{"media.class":"{class}","node.name":"{name}","node.description":"AG06/AG03 Analog Stereo"}},"params":{{"Props":[{{"channelVolumes":[{linear},{linear}],"mute":{mute}}}]}}}}}}]"#
    )
}

/// A delta of the `default` metadata (id 40) naming `node` the default sink.
pub fn default_sink_delta(node: &str) -> String {
    format!(
        r#"[{{"id":40,"type":"PipeWire:Interface:Metadata","props":{{"metadata.name":"default"}},"metadata":[{{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{{"name":"{node}"}}}}]}}]"#
    )
}

/// A pw-dump delta in which app `app` records the fixture's AG06 source: its `Stream/Input/Audio`
/// node `consumer_id` in `state`, and the link `link_id` from the source to it (`active` or
/// `paused`). Only the props the privacy rules read: well under PIPE_BUF.
pub fn capture_delta(consumer_id: u32, app: &str, state: &str, link_id: u32, active: bool) -> String {
    let source = AG06_SOURCE.0;
    let link_state = if active { "active" } else { "paused" };
    format!(
        r#"[{{"id":{consumer_id},"type":"PipeWire:Interface:Node","info":{{"state":"{state}","props":{{"media.class":"Stream/Input/Audio","node.name":"{app}","application.name":"{app}"}}}}}},{{"id":{link_id},"type":"PipeWire:Interface:Link","info":{{"output-node-id":{source},"input-node-id":{consumer_id},"state":"{link_state}"}}}}]"#
    )
}

/// A pw-dump delta removing `capture_delta`'s link and node (pw-dump's `"info": null`).
pub fn capture_gone(consumer_id: u32, link_id: u32) -> String {
    format!(r#"[{{"id":{link_id},"info":null}},{{"id":{consumer_id},"info":null}}]"#)
}

fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
    let r = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    let e = std::io::Error::last_os_error();
    assert!(r == 0 || e.kind() == std::io::ErrorKind::AlreadyExists, "mkfifo {}: {e}", path.display());
}

/// The fake `pw-dump` in `runtime/bin` (returns that directory). `Fifo`: after the initial dump the
/// script opens the pipe read-write (never an EOF, never a block) and `exec`s `cat` on it. It is ONE
/// process, so the daemon's PDEATHSIG takes it, and nothing is left reading the pipe.
pub fn fake_pw_dump_in(runtime: &Path, mode: PwDump) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let bin = runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let initial = runtime.join("pw-dump-initial-full.json");
    if mode == PwDump::Empty {
        fs::write(&initial, "[]\n").unwrap();
    } else {
        full_size_initial_dump(&fixtures, &initial);
    }
    let then = match mode {
        PwDump::Silent | PwDump::Empty => "exec sleep 100000".to_string(),
        PwDump::Busy => format!("while :; do cat '{}'; sleep 0.01; done", fixtures.join("pw-dump-deltas.json").display()),
        PwDump::Fifo => {
            let fifo = runtime.join(PW_DUMP_FIFO);
            make_fifo(&fifo);
            format!("exec 3<> '{}'\nexec cat <&3", fifo.display())
        }
    };
    let script = format!(
        "#!/bin/sh\n# Fake pw-dump of the daemon's tests: recorded fixtures, never PipeWire.\necho $$ > '{}'\ncat '{}'\n{then}\n",
        runtime.join("pw-dump.pid").display(),
        initial.display()
    );
    let path = bin.join("pw-dump");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn fake_pw_dump(sway: &Sway, mode: PwDump) -> PathBuf {
    fake_pw_dump_in(&sway.runtime, mode)
}

/// One delta (one JSON array, or several back to back) to the `Fifo` fake. The pipe is opened
/// without blocking, retried for 10 s while the fake has not opened it yet (ENXIO), then written
/// whole in one write. A pipe write is atomic only up to PIPE_BUF (4096 bytes on Linux), newline
/// included: a longer one could be cut, so it is refused before anything is opened.
pub fn pw_dump_send(runtime: &Path, json: &str) {
    use std::os::unix::fs::OpenOptionsExt;
    assert!(
        json.len() < libc::PIPE_BUF,
        "pw_dump_send: {} bytes and a newline exceed PIPE_BUF ({}), the write would not be atomic: send less at once",
        json.len(),
        libc::PIPE_BUF
    );
    let path = runtime.join(PW_DUMP_FIFO);
    let t = Instant::now();
    let mut f = loop {
        match fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&path) {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) && t.elapsed() < Duration::from_secs(10) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{}: {e}", path.display()),
        }
    };
    f.write_all(format!("{json}\n").as_bytes()).unwrap();
}

pub fn start_daemon(sway: &Sway) -> Daemon {
    start_daemon_with(sway, &DaemonOptions::default())
}

/// The daemon under test: this build's, or the binary named by
/// `ALPHEUS_DEMON_UNDER_TEST` — to measure another build (the commit before
/// a change) on exactly the same harness.
pub fn daemon_binary() -> PathBuf {
    env::var_os("ALPHEUS_DEMON_UNDER_TEST").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_alpheus-demon")))
}

/// `ALPHEUS_TEST_BASE_CONFIG=1`, set with `ALPHEUS_DEMON_UNDER_TEST` when that binary is a base
/// build from before `[displays]`: the harness then leaves out the tables such a build does not
/// know (bar.toml has `deny_unknown_fields`: one unknown table and the whole file is rejected,
/// every test's settings with it, and `[privacy]` back on the machine's /dev and /proc). Today
/// only `[displays]`, which a daemon without the module never reads.
pub fn base_config() -> bool {
    env::var_os("ALPHEUS_TEST_BASE_CONFIG").is_some_and(|v| v == "1")
}

/// Whether the file at `binary` holds `key` as bytes: a daemon whose config knows a key carries its
/// name (serde's field match, the check's message); a build from before it does not.
pub fn binary_knows(binary: &Path, key: &str) -> bool {
    fs::read(binary).is_ok_and(|bytes| bytes.windows(key.len()).any(|w| w == key.as_bytes()))
}

/// The quick panel's hardware folders, each injected whenever the daemon under test knows its key
/// (read once per test process, and only for a base build: this build always knows them).
const QUICK_DIRS: [(&str, &str); 2] = [("kbd_backlight_dir", "quick-leds"), ("rfkill_dir", "quick-rfkill")];

fn under_test_knows(key: &str) -> bool {
    static KNOWN: std::sync::OnceLock<[bool; QUICK_DIRS.len()]> = std::sync::OnceLock::new();
    let known = KNOWN.get_or_init(|| QUICK_DIRS.map(|(k, _)| binary_knows(&daemon_binary(), k)));
    QUICK_DIRS.iter().zip(known).any(|((k, _), known)| *k == key && *known)
}

/// What the harness writes as bar.toml: the test's `text`, with the camera watch's `[privacy]`
/// roots, the DisplayLink detection's `[displays] drm_dir` and the quick panel's
/// `[quick] kbd_backlight_dir` and `rfkill_dir` pointed at empty directories under `runtime` (never
/// the machine's /dev, /proc or /sys) unless the test sets its own; `base` (`base_config`) leaves
/// `[displays]` and the timer's `proc_dir` out, and the quick folders too unless the binary under
/// test knows their keys (`binary_knows`): a base build that reads LEDs or radios never reads the
/// machine's.
pub fn harness_bar_toml(text: &str, runtime: &Path, base: bool) -> String {
    let (dev, proc, drm) = (runtime.join("privacy-dev"), runtime.join("privacy-proc"), runtime.join("displays-drm"));
    let mut text = text.to_string();
    if !text.contains("dev_dir") {
        text = under_table(&text, "privacy", &format!("dev_dir = \"{}\"\nproc_dir = \"{}\"\n", dev.display(), proc.display()));
    }
    // The shutdown's lock lookup reads this folder, never the machine's /proc; a test fakes a locker
    // by writing `<pid>/comm` and `<pid>/status` in it. A base build predates `[timer] proc_dir`.
    if !base && !text.contains("timer-proc") {
        let timer = runtime.join("timer-proc");
        text = under_table(&text, "timer", &format!("proc_dir = \"{}\"\n", timer.display()));
    }
    if !base && !text.contains("drm_dir") {
        text = under_table(&text, "displays", &format!("drm_dir = \"{}\"\n", drm.display()));
    }
    // The keyboard backlight row reads LEDs there, never the machine's /sys/class/leds; the airplane
    // mode row reads the radios there, never the machine's /sys/class/rfkill (its `rfkill` command
    // acts by type whatever this says: the private PATH, without `rfkill`, is what keeps the
    // machine's radios safe; this keeps the row's state the test's own).
    for (key, folder) in QUICK_DIRS {
        if (!base || under_test_knows(key)) && !text.contains(key) {
            text = under_table(&text, "quick", &format!("{key} = \"{}\"\n", runtime.join(folder).display()));
        }
    }
    text
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
    // Nor the session's own commands the power menu runs: a fake one missing
    // fails to start, never reaches the real one.
    for real in ["systemctl", "loginctl"] {
        assert!(!dir.join(real).exists(), "coreutils dir {} also has {real}", dir.display());
    }
    // Nor Shield mode's privileged commands: the tests' `fake-shield` stands in for
    // the configured argv, and a `pkexec` or `usbguard` reaching the daemon would act on the machine.
    for real in ["pkexec", "usbguard"] {
        assert!(!dir.join(real).exists(), "coreutils dir {} also has {real}", dir.display());
    }
    // Nor the quick panel's hardware tools: `brightnessctl` would set the machine's keyboard
    // backlight and `rfkill` cut its radios (by type, whatever directory the test points at); only
    // a recorder on the private PATH may stand in for them.
    for real in ["brightnessctl", "rfkill"] {
        assert!(!dir.join(real).exists(), "coreutils dir {} also has {real}", dir.display());
    }
    dir
}

/// `keys` (whole lines) added to `[table]` of `text`: right under its header when it has one (TOML
/// forbids a second), else in a new table at the end.
fn under_table(text: &str, table: &str, keys: &str) -> String {
    let header = format!("[{table}]");
    let Some(at) = text.find(&header) else { return format!("{text}\n{header}\n{keys}") };
    let (head, rest) = match text[at..].find('\n') {
        Some(n) => text.split_at(at + n + 1),
        None => (text, ""),
    };
    let nl = if head.ends_with('\n') { "" } else { "\n" };
    format!("{head}{nl}{keys}{rest}")
}

pub fn start_daemon_with(sway: &Sway, o: &DaemonOptions) -> Daemon {
    let config = sway.runtime.join("config");
    fs::create_dir_all(config.join("alpheus")).unwrap();
    for empty in ["privacy-dev", "privacy-proc", "displays-drm", "timer-proc", "quick-leds", "quick-rfkill"] {
        fs::create_dir_all(sway.runtime.join(empty)).unwrap();
    }
    fs::write(config.join("alpheus/bar.toml"), harness_bar_toml(o.bar_toml.unwrap_or(""), &sway.runtime, base_config())).unwrap();
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
    let no_session_bus = format!("unix:path={}", sway.runtime.join("no-session-bus").display());
    let mut command = Command::new(daemon_binary());
    command
        .env_clear()
        .env("PATH", path)
        .env("HOME", &sway.runtime)
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_DATA_DIRS", &data_dirs)
        .env("XDG_STATE_HOME", sway.runtime.join("state"))
        .env("WAYLAND_DISPLAY", &sway.wayland)
        // Never the machine's system bus: a private one, or none at all.
        .env("DBUS_SYSTEM_BUS_ADDRESS", o.system_bus.map(String::from).unwrap_or(no_bus))
        // Never the user's session bus either: a private one, or none (XDG_RUNTIME_DIR is sway's, without a `bus`).
        .env("DBUS_SESSION_BUS_ADDRESS", o.session_bus.map(String::from).unwrap_or(no_session_bus))
        .env("ALPHEUS_POLICES", env::var_os("ALPHEUS_POLICES").unwrap_or_default());
    // One compositor variable, never the session's own: the daemon reads this test's compositor only.
    match o.backend {
        Backend::FakeHyprland => command.env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE),
        Backend::Sway => command.env("SWAYSOCK", &sway.ipc),
        Backend::Mango(socket) => command.env("MANGO_INSTANCE_SIGNATURE", socket),
    };
    let mut child = command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let j = log.clone();
    let err = child.stderr.take().unwrap();
    thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            j.lock().unwrap().push(l);
        }
    });
    let mut d = Daemon { child, log };
    d.assert_config_taken();
    d
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

/// `wl-copy` serving `text` as the selection of THIS test's headless sway, and nowhere else: the
/// one place a test starts `wl-copy` (a guardrail checks it). A bare `wayland-N` name would also
/// open the machine's own compositor under an inherited `XDG_RUNTIME_DIR`, so the environment is
/// cleared, the runtime directory and display are this sway's, and the socket must exist before
/// the spawn. `mime` (`--type`): the one type offered, text by default. `--foreground` keeps the serving process the child itself (never a forked orphan),
/// `--primary` is never passed, and the guard kills and reaps it on drop. Fictional text only.
pub fn wl_copy(sway: &Sway, text: &str, mime: Option<&str>, sensitive: bool) -> WlCopyChild {
    assert!(sway.runtime.join(&sway.wayland).exists(), "this test's compositor socket is not there: wl-copy would reach another one");
    let mut command = Command::new("wl-copy");
    command.arg("--foreground");
    if sensitive {
        command.arg("--sensitive");
    }
    if let Some(mime) = mime {
        command.arg("--type").arg(mime);
    }
    let child = command
        .arg("--")
        .arg(text)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("WAYLAND_DISPLAY", &sway.wayland)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("wl-copy not found: run these tests inside `nix develop`");
    WlCopyChild(Some(child))
}

/// A test that cannot run in this environment says so, where a reader looks: on stderr and, in
/// GitHub Actions, in the run's summary. It is a visible skip, never a silent pass.
pub fn skip(test: &str, reason: &str) {
    let line = format!("SKIPPED {test}: {reason}");
    eprintln!("{line}");
    if let Some(summary) = env::var_os("GITHUB_STEP_SUMMARY")
        && let Ok(mut file) = fs::OpenOptions::new().append(true).open(summary)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// The text of THIS sway's clipboard (`wl-paste --no-newline --type text/plain`), None when there
/// is no selection or it is no text (a non-zero exit). Same guard as `wl_copy`: the socket must
/// exist, the environment is cleared, the runtime directory and display are this sway's, stdin is
/// null, and a read that is not over in 2 s is killed (None). Never the primary selection.
pub fn wl_paste(sway: &Sway) -> Option<String> {
    assert!(sway.runtime.join(&sway.wayland).exists(), "this test's compositor socket is not there: wl-paste would reach another one");
    let mut child = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain"])
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("XDG_RUNTIME_DIR", &sway.runtime)
        .env("WAYLAND_DISPLAY", &sway.wayland)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("wl-paste not found: run these tests inside `nix develop`");
    let mut out = child.stdout.take().expect("piped");
    let reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = Read::read_to_string(&mut out, &mut text);
        text
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(status) => break Some(status),
            None if start.elapsed() > Duration::from_secs(2) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    };
    let text = reader.join().unwrap_or_default();
    status.filter(|s| s.success()).map(|_| text)
}

/// The `wl-copy` serving a selection: killed and waited on drop, so a failed assertion never
/// leaves it serving.
pub struct WlCopyChild(Option<Child>);

impl WlCopyChild {
    /// The child has exited with a failure: it never served. `wl-copy` older than 2.3.0 does so at
    /// once for `--sensitive` (unrecognised option), which is how a test learns the option is missing.
    pub fn failed(&mut self) -> bool {
        self.0.as_mut().and_then(|c| c.try_wait().ok().flatten()).is_some_and(|status| !status.success())
    }
}

impl Drop for WlCopyChild {
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

/// A program that appends one line, `<name> <its arguments>`, to `<runtime>/calls-<name>` and
/// exits: what the quick panel ran, in order (never the real tool).
pub fn fake_recorder(sway: &Sway, name: &str) -> PathBuf {
    let bin = sway.runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = sway.runtime.join(format!("calls-{name}"));
    let script = format!(
        "#!/bin/sh\n# Fake {name} of the quick settings tests: records its arguments, nothing else.\nprintf '%s\\n' \"{name} $*\" >> '{}'\n",
        calls.display()
    );
    let path = bin.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A program that appends its arguments to `<runtime>/calls-<name>`, ONE PER LINE, then a `--`
/// line: what `fake_recorder`'s single line cannot prove (an argument with spaces or quotes stays
/// whole). Read back with `recorded_argv`.
pub fn fake_argv_recorder(sway: &Sway, name: &str) -> PathBuf {
    let bin = sway.runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = sway.runtime.join(format!("calls-{name}"));
    let script = format!(
        "#!/bin/sh\n# Fake {name} of the tools menu tests: records each argument on its own line, then `--`.\n{{\n  for a in \"$@\"; do printf '%s\\n' \"$a\"; done\n  printf '%s\\n' '--'\n}} >> '{}'\n",
        calls.display()
    );
    let path = bin.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The calls of a `fake_argv_recorder`: one `Vec` of arguments per call, in order.
pub fn recorded_argv(sway: &Sway, name: &str) -> Vec<Vec<String>> {
    let text = fs::read_to_string(sway.runtime.join(format!("calls-{name}"))).unwrap_or_default();
    let mut calls = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        if line == "--" {
            calls.push(std::mem::take(&mut current));
        } else {
            current.push(line.to_string());
        }
    }
    calls
}

/// Like `fake_recorder`, and the `ALPHEUS_*` variables it was started with, sorted, each followed by
/// a space: `<name> <arguments>|ALPHEUS_DND=off ALPHEUS_EVENT=dnd `, read back with `recorded`. Built
/// in `sh` from `env` and `sort` only (no `grep` on the harness's PATH); the daemon's own
/// `ALPHEUS_POLICES` is skipped, and a value keeps its spaces.
pub fn fake_env_recorder(sway: &Sway, name: &str) -> PathBuf {
    let bin = sway.runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = sway.runtime.join(format!("calls-{name}"));
    let script = format!(
        "#!/bin/sh\n# Fake {name} of the hooks tests: records its arguments and ALPHEUS_* variables, nothing else.\n\
         env | sort | {{\n  line=\"{name} $*|\"\n  while IFS= read -r v; do\n    case \"$v\" in\n      ALPHEUS_POLICES=*) ;;\n      ALPHEUS_*) line=\"$line$v \" ;;\n    esac\n  done\n  printf '%s\\n' \"$line\" >> '{}'\n}}\n",
        calls.display()
    );
    let path = bin.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// What `fake_recorder`'s `name` recorded, in order.
pub fn recorded(sway: &Sway, name: &str) -> Vec<String> {
    fs::read_to_string(sway.runtime.join(format!("calls-{name}"))).unwrap_or_default().lines().map(String::from).collect()
}

/// Like `fake_recorder`, and what it reads on its standard input appended to `<runtime>/stdin-<name>`:
/// the quick panel's Wi-Fi password (fake passwords only), never on its command line.
pub fn fake_stdin_recorder(sway: &Sway, name: &str) -> PathBuf {
    let bin = sway.runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let calls = sway.runtime.join(format!("calls-{name}"));
    let stdin = sway.runtime.join(format!("stdin-{name}"));
    let script = format!(
        "#!/bin/sh\n# Fake {name} of the quick settings tests: records its arguments and its standard input, nothing else.\nprintf '%s\\n' \"{name} $*\" >> '{}'\ncat >> '{}'\n",
        calls.display(),
        stdin.display()
    );
    let path = bin.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// What `fake_stdin_recorder`'s `name` read on its standard input, every call's in order.
pub fn recorded_stdin(sway: &Sway, name: &str) -> Vec<u8> {
    fs::read(sway.runtime.join(format!("stdin-{name}"))).unwrap_or_default()
}

/// Where the fake source's `run` records its arguments, in the runtime directory.
pub const FAKE_RUN_MARKER: &str = "launched-fake-run";

/// A fake command source of the launcher tests (never tools-menu): `<runtime>/bin/fake-list-<name>`
/// prints `lines`; `<runtime>/bin/fake-run` writes its arguments, one per line, to
/// `<runtime>/launched-fake-run`. Returns the `[[sources]]` table naming them (run =
/// [fake-run, "--from-alpheus", "{line}"]), to put in launcher.toml. Both are executed directly
/// by the daemon (no `sh` in front), as a real menu script is.
pub fn fake_source(runtime: &Path, name: &str, lines: &[String]) -> String {
    let bin = runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let data = runtime.join(format!("source-lines-{name}"));
    fs::write(&data, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let list = bin.join(format!("fake-list-{name}"));
    let run = bin.join("fake-run");
    let marker = runtime.join(FAKE_RUN_MARKER);
    for (path, body) in [
        (&list, format!("exec cat '{}'", data.display())),
        (&run, format!("printf '%s\\n' \"$@\" > '{0}.tmp' && mv '{0}.tmp' '{0}'", marker.display())),
    ] {
        fs::write(path, format!("#!/bin/sh\n# Fake command source of the launcher tests.\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    wait_until_executable(&list);
    format!(
        "[[sources]]\nname = {name:?}\nlist = [{:?}]\nrun = [{:?}, \"--from-alpheus\", \"{{line}}\"]\n",
        list.display().to_string(),
        run.display().to_string()
    )
}

/// The blocked devices `fake-shield list` prints (USBGuard's `list-devices` format, fictional ones).
pub const SHIELD_DEVICES: [&str; 2] = [
    "7: block id abcd:0001 serial \"\" name \"Test Keyboard\" hash \"h1\" parent-hash \"p1\" via-port \"1-2\" with-interface 03:01:01 with-connect-type \"hotplug\"",
    "12: block id 1234:5678 serial \"\" name \"Test Stick\" hash \"h2\" parent-hash \"p2\" via-port \"1-3\" with-interface 08:06:50 with-connect-type \"hotplug\"",
];

/// The `fake-shield` of Shield mode's tests (never a real `pkexec`, `usbguard` or `systemctl`):
/// `<runtime>/bin/fake-shield`, paths baked in (the daemon's environment is cleared). With no
/// argument it does nothing and exits 0 (the executable probe has no side effect). `on` creates
/// `flag`, `slow` exits 0 at once and creates it a second later (the grace's case: `timeout` moves it to its own process group, which the runner's group kill at the exit then leaves, given the 0.3 s the stub waits for that), `off` removes it, `fail` exits 126, `list`
/// appends `list` to `record` and prints `devices` blocked devices (`SHIELD_DEVICES`, then synthetic ones), `allow <id>` appends
/// `allow <id>`. Every invocation with an argument is appended to `record` as well, as
/// `run <arguments>` for `on`, `off` and `fail`.
pub fn shield_stub(sway: &Sway, flag: &Path, record: &Path, devices: usize, list_secs: u32) -> PathBuf {
    let bin = sway.runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(flag.parent().unwrap()).unwrap();
    // The two fixtures, then synthetic ones (ids from 100) up to `devices`: the memory tests list ten.
    let synthetic = (SHIELD_DEVICES.len()..devices).map(|n| {
        format!(
            "{}: block id 1234:{:04x} serial \"\" name \"Test Device {n}\" hash \"h{n}\" parent-hash \"p{n}\" via-port \"1-{n}\" with-interface 08:06:50 with-connect-type \"hotplug\"",
            100 + n,
            n
        )
    });
    let lines: String =
        SHIELD_DEVICES.iter().map(|l| l.to_string()).chain(synthetic).take(devices).map(|l| format!("printf '%s\\n' '{l}'\n")).collect();
    let script = format!(
        "#!/bin/sh\n# Fake privileged command of Shield mode's tests: touches or removes the flag, records, nothing else.\n\
         [ $# -eq 0 ] && exit 0\n\
         case \"$1\" in\n\
         on) printf '%s\\n' 'run on' >> '{record}'; touch '{flag}' ;;\n\
         off) printf '%s\\n' 'run off' >> '{record}'; rm -f '{flag}' ;;\n\
         slow) printf '%s\\n' 'run slow' >> '{record}'; timeout 10 /bin/sh -c \"sleep 1; touch '{flag}'\" </dev/null >/dev/null 2>&1 & sleep 0.3 ;;\n\
         fail) printf '%s\\n' 'run fail' >> '{record}'; exit 126 ;;\n\
         list) printf '%s\\n' 'list' >> '{record}'; sleep {list_secs}\n{lines};;\n\
         allow) printf '%s\\n' \"allow $2\" >> '{record}' ;;\n\
         esac\n",
        record = record.display(),
        flag = flag.display(),
        list_secs = list_secs,
    );
    let path = bin.join("fake-shield");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    wait_until_executable(&path);
    path
}

/// What `fake-shield` recorded, in order.
pub fn shield_record(record: &Path) -> Vec<String> {
    fs::read_to_string(record).unwrap_or_default().lines().map(String::from).collect()
}

/// Runs a script just written (one with no side effect) until it no longer fails with "Text file
/// busy": a fork elsewhere in this process may still hold its write descriptor for a moment
/// (rust-lang/rust#114554). The daemon then executes it directly without racing that.
pub fn wait_until_executable(script: &Path) {
    let t = Instant::now();
    loop {
        match Command::new(script).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && t.elapsed() < Duration::from_secs(5) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{} cannot run: {e}", script.display()),
            Ok(status) => {
                assert!(status.success(), "{} failed: {status}", script.display());
                return;
            }
        }
    }
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

/// Voluntary context switches in a /proc status file: the thread blocked, then ran again (a
/// wake-up). Involuntary ones are preemptions under CPU load, noise for a wake-up budget.
pub fn switches(status: &str) -> u64 {
    status
        .lines()
        .filter(|l| l.starts_with("voluntary_ctxt_switches:"))
        .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        .sum()
}

/// CPU clock ticks (utime + stime) in a /proc stat line: what a busy spin burns while it makes no
/// voluntary switch (it never blocks), so no wake-up count ever sees it.
pub fn cpu_ticks(stat: &str) -> u64 {
    let Some((_, rest)) = stat.rsplit_once(')') else { return 0 };
    // After the command: the state is field 3, utime and stime fields 14 and 15.
    rest.split_whitespace().skip(11).take(2).filter_map(|v| v.parse::<u64>().ok()).sum()
}

/// CPU ticks a loop at rest may use over 5 s (the clock's tick is 10 ms on Linux): a spin uses
/// hundreds.
pub const REST_TICKS: u64 = 2;

/// The CPU ticks allowed at rest over `window`: `REST_TICKS` per 5 s, never less than that.
pub fn rest_ticks(window: Duration) -> u64 {
    (REST_TICKS * window.as_secs().div_ceil(5)).max(REST_TICKS)
}

/// Asserts that `ticks` of CPU over an at-rest `window` are no spin.
pub fn assert_no_spin(ticks: u64, window: Duration, what: &str) {
    let allowed = rest_ticks(window);
    assert!(ticks <= allowed, "{what}: {ticks} CPU ticks in {window:?} at rest (≤ {allowed}): the loop spins");
}

impl Daemon {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Fails the test when the daemon rejected the bar.toml the harness wrote: it would run on
    /// the defaults, every test setting ignored and the privacy roots back on the machine's.
    /// bar.toml's warnings come just before the `theme:` line (or the daemon ended first).
    fn assert_config_taken(&mut self) {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) && !self.wait_for("theme: ", Duration::from_millis(20)) {
            if self.child.try_wait().ok().flatten().is_some() {
                thread::sleep(Duration::from_millis(50)); // the last lines, read by the log thread
                break;
            }
        }
        let rejected = self.log().into_iter().find(|l| l.contains("bar.toml invalid") || l.contains("bar.toml unreadable"));
        assert!(rejected.is_none(), "the daemon rejected the harness's bar.toml: {rejected:?} (a base build: ALPHEUS_TEST_BASE_CONFIG=1)");
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

    /// The box's physical size from the last "launcher shown on … (<w>×<h> px) in …" line.
    pub fn last_launcher_box_px(&self) -> Option<(u64, u64)> {
        let log = self.log.lock().unwrap();
        let line = log.iter().rev().find(|l| l.contains("launcher shown on"))?;
        let (w, h) = line.rsplit_once(" (")?.1.split_once(" px)")?.0.split_once('×')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    }

    /// PSS in KB (/proc/<pid>/smaps_rollup).
    pub fn pss_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid())).unwrap();
        t.lines().find_map(|l| l.strip_prefix("Pss:")).and_then(|v| v.split_whitespace().next()).unwrap().parse().unwrap()
    }

    /// Anonymous memory in KB (/proc/<pid>/smaps_rollup `Anonymous:`): the heap and stacks the
    /// daemon itself keeps. Unlike PSS it counts no file page (code, fonts) and no shm pool (the
    /// compositor's mapping moves those), so a leak shows and a first open's warm-up does not.
    pub fn anon_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid())).unwrap();
        t.lines().find_map(|l| l.strip_prefix("Anonymous:")).and_then(|v| v.split_whitespace().next()).unwrap().parse().unwrap()
    }

    /// Anonymous memory in KB (/proc/<pid>/smaps_rollup `Anonymous:`): heap, stacks, anonymous maps.
    /// Neither the shm pools (shmem) nor the file-backed pages are in it, so it does not move with
    /// what other processes share or with the page cache's fault-around: the figure leak bounds use.
    pub fn anonymous_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid())).unwrap();
        t.lines().find_map(|l| l.strip_prefix("Anonymous:")).and_then(|v| v.split_whitespace().next()).unwrap().parse().unwrap()
    }

    /// File-backed PSS in KB (`Pss_File:`): code, read-only data and font pages, shared proportionally.
    pub fn file_pss_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid())).unwrap();
        t.lines().find_map(|l| l.strip_prefix("Pss_File:")).and_then(|v| v.split_whitespace().next()).unwrap().parse().unwrap()
    }

    /// PSS in KB, the whole daemon's then its shm pools' (memfd mappings), from one pass over
    /// /proc/<pid>/smaps so the two cannot skew. The compositor maps the pools too, so their PSS
    /// moves by a buffer as it maps and unmaps one: a bound on what the daemon itself keeps leaves
    /// them out (whole − pools), and the pools get a looser one of their own.
    pub fn pss_and_shm_kb(&self) -> (u64, u64) {
        let t = fs::read_to_string(format!("/proc/{}/smaps", self.pid())).unwrap();
        let (mut shm, mut total, mut pools) = (false, 0, 0);
        for l in t.lines() {
            if let Some(v) = l.strip_prefix("Pss:") {
                let kb = v.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
                total += kb;
                if shm {
                    pools += kb;
                }
            } else if l.split_whitespace().next().is_some_and(|f| f.contains('-') && !f.ends_with(':')) {
                shm = l.contains("/memfd:");
            }
        }
        (total, pools)
    }

    /// The daemon's anonymous memory in KB (`Pss_Anon` of smaps_rollup): heap, stacks, caches. It
    /// leaves out file pages (code, fonts: the first open's warm-up) and the shm pools (`Pss_Shmem`,
    /// which the compositor maps too and which move on their own), so a leak shows alone.
    pub fn pss_anon_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid())).unwrap();
        t.lines()
            .find_map(|l| l.strip_prefix("Pss_Anon:"))
            .and_then(|v| v.split_whitespace().next().and_then(|n| n.parse().ok()))
            .expect("Pss_Anon in smaps_rollup")
    }

    /// The daemon's shm pools' mapped size in KB (smaps `Size:` of its memfd mappings): unlike
    /// their PSS, which moves as the compositor maps them and only counts the pages painted, the
    /// size a pool was made or grown to, so a SlotPool that doubles shows exactly.
    pub fn shm_size_kb(&self) -> u64 {
        let t = fs::read_to_string(format!("/proc/{}/smaps", self.pid())).unwrap();
        let (mut shm, mut size) = (false, 0);
        for l in t.lines() {
            if let Some(v) = l.strip_prefix("Size:") {
                if shm {
                    size += v.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
                }
            } else if l.split_whitespace().next().is_some_and(|f| f.contains('-') && !f.ends_with(':')) {
                shm = l.contains("/memfd:");
            }
        }
        size
    }

    /// Number of the daemon's threads.
    pub fn threads(&self) -> usize {
        fs::read_dir(format!("/proc/{}/task", self.pid())).map(|d| d.count()).unwrap_or(0)
    }

    /// Cumulative voluntary context switches (wake-ups) of all the daemon's threads.
    pub fn wakeups(&self) -> u64 {
        fs::read_dir(format!("/proc/{}/task", self.pid()))
            .unwrap()
            .flatten()
            .map(|t| switches(&fs::read_to_string(t.path().join("status")).unwrap_or_default()))
            .sum()
    }

    /// CPU ticks (utime + stime) of the whole daemon, every thread.
    pub fn cpu_ticks(&self) -> u64 {
        cpu_ticks(&fs::read_to_string(format!("/proc/{}/stat", self.pid())).unwrap_or_default())
    }

    /// CPU ticks (utime + stime) of the event loop's thread only (the main thread).
    pub fn main_thread_cpu_ticks(&self) -> u64 {
        cpu_ticks(&fs::read_to_string(format!("/proc/{0}/task/{0}/stat", self.pid())).unwrap_or_default())
    }

    /// Voluntary context switches (wake-ups) of the event loop's thread only (the main thread).
    pub fn main_thread_wakeups(&self) -> u64 {
        switches(&fs::read_to_string(format!("/proc/{0}/task/{0}/status", self.pid())).unwrap_or_default())
    }

    /// Voluntary context switches of the daemon's thread named `name` (its comm:
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
    /// killed and reaped by then: a bare
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
        // SAFETY: kill(2) towards the group this child received at launch
        // (`process_group(0)` above), not reaped yet: still ours.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
        let _ = child.wait();
        Err((pid, line))
    }

    /// Test-only entry point for `spawn_program`'s failure path: see its doc
    /// comment.
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
        Peer::connect_at(&bus.address)
    }

    /// The same, from the address alone (a fake service restarting inside its own thread).
    pub fn connect_at(address: &str) -> Peer {
        let mut channel = dbus::channel::Channel::open_private(address).unwrap();
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

    /// Sends a signal to `destination` alone (a unicast: the bus delivers it whatever the receiver's
    /// match rules), whose only argument is `arg`.
    pub fn emit_to<A: dbus::arg::Append>(&self, destination: &str, path: &str, interface: &str, member: &str, arg: A) {
        let mut m = dbus::Message::new_signal(path, interface, member).unwrap().append1(arg);
        m.set_destination(Some(destination.into()));
        self.channel.send(m).unwrap();
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

#[path = "../support/sni.rs"]
pub mod sni;

#[path = "../support/portal.rs"]
pub mod portal;

#[path = "../support/mpris.rs"]
pub mod mpris;

#[path = "../support/notify.rs"]
pub mod notify;

#[path = "../support/sway_client.rs"]
pub mod sway_client;

#[path = "../support/fake_mango.rs"]
pub mod fake_mango;

#[path = "../support/bluez.rs"]
pub mod bluez;

#[path = "../support/displays.rs"]
pub mod displays;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// The workstation recorded on 2026-10-04 (tests/fixtures/dbus-*.json,
/// anonymized), served on a private bus by one `Peer` owning
/// NetworkManager's, UPower's and BlueZ's names: what `FakeBus::workstation`
/// is to the unit tests, over the wire. An object the recording doesn't
/// have gets `UnknownObject`, as `FakeBus` answers NotFound.
pub struct Workstation {
    peer: Arc<Peer>,
}

impl Workstation {
    pub fn serve(bus: &PrivateBus) -> Workstation {
        Self::serve_recorded(bus, None)
    }

    /// As `serve`, with the display device's `Percentage` (rounded by the reader) and `State`
    /// (UpDeviceState: 1 charging, 2 discharging) rewritten in its recorded `GetAll`.
    pub fn serve_with_display(bus: &PrivateBus, percent: f64, state: u32) -> Workstation {
        Self::serve_recorded(bus, Some((percent, state)))
    }

    fn serve_recorded(bus: &PrivateBus, display: Option<(f64, u32)>) -> Workstation {
        let peer = Arc::new(Peer::connect(bus));
        let served = peer.clone();
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
            let mut json = read(&format!("dbus-upower-{file}.json"));
            if let (Some((percent, state)), "display") = (display, file) {
                json = rewrite_display(&json, percent, state);
            }
            replies.push(((format!("/org/freedesktop/UPower/devices/{device}"), "GetAll"), json));
        }
        thread::spawn(move || {
            // A short poll, never `next_call`'s hour: a signal sent from another thread
            // (`battery_changed`) would wait on the connection's lock behind it.
            while served.channel.is_connected() {
                let Ok(Some(call)) = served.channel.blocking_pop_message(Duration::from_millis(20)) else { continue };
                if call.msg_type() != dbus::message::MessageType::MethodCall {
                    continue;
                }
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
                let _ = served.channel.send(reply);
                served.channel.flush();
            }
        });
        Workstation { peer }
    }

    /// UPower announces a change on its display device (an empty `PropertiesChanged`): the
    /// daemon reads its battery again, as it would for a percentage that moved.
    pub fn battery_changed(&self) {
        let mut m = dbus::Message::new_signal(
            "/org/freedesktop/UPower/devices/DisplayDevice",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .unwrap();
        busctl_wire::push_reply(&mut m, r#"{"type":"s","data":["org.freedesktop.UPower.Device"]}"#);
        busctl_wire::push_reply(&mut m, r#"{"type":"a{sv}","data":[{"Percentage":{"type":"d","data":70.0}}]}"#);
        busctl_wire::push_reply(&mut m, r#"{"type":"as","data":[[]]}"#);
        let _ = self.peer.channel.send(m);
        self.peer.channel.flush();
    }
}

/// The recorded display device with its `Percentage` and `State` replaced.
fn rewrite_display(json: &str, percent: f64, state: u32) -> String {
    let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
    let props = v
        .pointer_mut("/data/0")
        .filter(|p| p["Percentage"]["data"].is_number() && p["State"]["data"].is_number())
        .expect("dbus-upower-display.json no longer has data[0].Percentage/State numbers: update rewrite_display");
    props["Percentage"]["data"] = serde_json::json!(percent);
    props["State"]["data"] = serde_json::json!(state);
    v.to_string()
}

/// power-profiles-daemon's well-known name.
pub const POWER_PROFILES_NAME: &str = "net.hadess.PowerProfiles";
const POWER_PROFILES_PATH: &str = "/net/hadess/PowerProfiles";

/// A power-profiles-daemon on a private bus (test code only): owns its name, answers `GetAll` on its
/// one object with the active profile and the three offered, and emits `PropertiesChanged` when the
/// test sets another. Dropped: its connection closes, the name loses its owner.
pub struct FakePowerProfiles {
    tx: Option<mpsc::Sender<String>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakePowerProfiles {
    pub fn serve(bus: &PrivateBus, active: &str) -> FakePowerProfiles {
        let peer = Peer::connect(bus);
        peer.own(POWER_PROFILES_NAME);
        let (tx, rx) = mpsc::channel::<String>();
        let mut active = active.to_string();
        let thread = thread::spawn(move || {
            loop {
                match rx.try_recv() {
                    Ok(p) => {
                        active = p;
                        let mut m =
                            dbus::Message::new_signal(POWER_PROFILES_PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged").unwrap();
                        busctl_wire::push_reply(&mut m, &format!(r#"{{"type":"s","data":["{POWER_PROFILES_NAME}"]}}"#));
                        busctl_wire::push_reply(
                            &mut m,
                            &format!(r#"{{"type":"a{{sv}}","data":[{{"ActiveProfile":{{"type":"s","data":"{active}"}}}}]}}"#),
                        );
                        busctl_wire::push_reply(&mut m, r#"{"type":"as","data":[[]]}"#);
                        let _ = peer.channel.send(m);
                        peer.channel.flush();
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match peer.channel.blocking_pop_message(Duration::from_millis(20)) {
                    Ok(Some(call)) if call.msg_type() == dbus::message::MessageType::MethodCall => {
                        let mut r = call.method_return();
                        busctl_wire::push_reply(&mut r, &power_profiles_reply(&active));
                        let _ = peer.channel.send(r);
                        peer.channel.flush();
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        FakePowerProfiles { tx: Some(tx), thread: Some(thread) }
    }

    /// Sets the active profile and emits `PropertiesChanged` for it.
    pub fn set_active(&self, profile: &str) {
        if let Some(tx) = &self.tx {
            tx.send(profile.to_string()).unwrap();
        }
    }
}

impl Drop for FakePowerProfiles {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// `GetAll`'s reply as busctl prints it: the active profile and the three offered (fictional drivers).
fn power_profiles_reply(active: &str) -> String {
    let profile = |p: &str| format!(r#"{{"Profile":{{"type":"s","data":"{p}"}},"Driver":{{"type":"s","data":"placeholder"}}}}"#);
    format!(
        r#"{{"type":"a{{sv}}","data":[{{"ActiveProfile":{{"type":"s","data":"{active}"}},"Profiles":{{"type":"aa{{sv}}","data":[{},{},{}]}}}}]}}"#,
        profile("power-saver"),
        profile("balanced"),
        profile("performance")
    )
}

/// systemd's well-known name and its Manager.
pub const SYSTEMD_NAME: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";

#[derive(Default)]
struct SystemdShared {
    n_failed: u32,
    /// `Get` answers with an error reply (an odd reply, a service that is failing).
    get_error: bool,
    /// The unique names that called `Subscribe` (systemd drops them with their connection).
    subscribers: Vec<String>,
    /// Every call received, "Member" or "Member arg".
    calls: Vec<String>,
}

enum SystemdCmd {
    Failed(u32),
    Restart,
}

/// A systemd on a private bus (test code only): owns its name, answers `Get` of `NFailedUnits`,
/// `GetAll` (its `Environment` holds `SECRET=x`: a daemon that asked would show in `calls`),
/// `Subscribe` and `Unsubscribe`; and, like the real one, emits the Manager's `PropertiesChanged`
/// **only while a client has subscribed**. `restart` drops its connection and takes the name again
/// (a new owner with no subscriber). Dropped: its connection closes.
pub struct FakeSystemd {
    tx: Option<mpsc::Sender<SystemdCmd>>,
    shared: Arc<Mutex<SystemdShared>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeSystemd {
    pub fn serve(bus: &PrivateBus, n_failed: u32) -> FakeSystemd {
        Self::start(bus, n_failed, false)
    }

    /// One whose `Subscribe` is refused (an error reply).
    pub fn serve_refusing(bus: &PrivateBus, n_failed: u32) -> FakeSystemd {
        Self::start(bus, n_failed, true)
    }

    fn start(bus: &PrivateBus, n_failed: u32, refuse: bool) -> FakeSystemd {
        let address = bus.address.clone();
        let mut peer = Peer::connect_at(&address);
        peer.own(SYSTEMD_NAME);
        let shared = Arc::new(Mutex::new(SystemdShared { n_failed, ..Default::default() }));
        let state = shared.clone();
        let (tx, rx) = mpsc::channel::<SystemdCmd>();
        let thread = thread::spawn(move || {
            loop {
                match rx.try_recv() {
                    Ok(SystemdCmd::Failed(n)) => {
                        let announce = {
                            let mut s = state.lock().unwrap();
                            s.n_failed = n;
                            !s.subscribers.is_empty()
                        };
                        if announce {
                            let mut m =
                                dbus::Message::new_signal(SYSTEMD_PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged").unwrap();
                            busctl_wire::push_reply(&mut m, &format!(r#"{{"type":"s","data":["{SYSTEMD_MANAGER}"]}}"#));
                            busctl_wire::push_reply(
                                &mut m,
                                &format!(r#"{{"type":"a{{sv}}","data":[{{"NFailedUnits":{{"type":"u","data":{n}}}}}]}}"#),
                            );
                            busctl_wire::push_reply(&mut m, r#"{"type":"as","data":[[]]}"#);
                            let _ = peer.channel.send(m);
                            peer.channel.flush();
                        }
                    }
                    Ok(SystemdCmd::Restart) => {
                        // The old connection goes: its name is free, its subscribers forgotten.
                        drop(peer);
                        state.lock().unwrap().subscribers.clear();
                        peer = Peer::connect_at(&address);
                        let own = |peer: &Peer| {
                            let m = dbus::Message::new_method_call(
                                "org.freedesktop.DBus",
                                "/org/freedesktop/DBus",
                                "org.freedesktop.DBus",
                                "RequestName",
                            )
                            .unwrap()
                            .append2(SYSTEMD_NAME, 0u32);
                            peer.call(m).ok().and_then(|r| r.read1::<u32>().ok()) == Some(1)
                        };
                        let until = Instant::now() + Duration::from_secs(3);
                        while !own(&peer) && Instant::now() < until {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match peer.channel.blocking_pop_message(Duration::from_millis(20)) {
                    Ok(Some(call)) if call.msg_type() == dbus::message::MessageType::MethodCall => {
                        let member = call.member().map(|m| m.to_string()).unwrap_or_default();
                        let sender = call.sender().map(|s| s.to_string()).unwrap_or_default();
                        let mut s = state.lock().unwrap();
                        let reply = match member.as_str() {
                            "Get" => {
                                let (interface, property): (String, String) = call.read2().unwrap_or_default();
                                s.calls.push(format!("Get {property}"));
                                if s.get_error {
                                    call.error(&"org.freedesktop.DBus.Error.Failed".into(), c"failing")
                                } else if interface == SYSTEMD_MANAGER && property == "NFailedUnits" {
                                    let mut r = call.method_return();
                                    busctl_wire::push_reply(
                                        &mut r,
                                        &format!(r#"{{"type":"v","data":[{{"type":"u","data":{}}}]}}"#, s.n_failed),
                                    );
                                    r
                                } else {
                                    call.error(&"org.freedesktop.DBus.Error.UnknownProperty".into(), c"not the count")
                                }
                            }
                            "GetAll" => {
                                s.calls.push("GetAll".into());
                                let mut r = call.method_return();
                                busctl_wire::push_reply(
                                    &mut r,
                                    &format!(
                                        r#"{{"type":"a{{sv}}","data":[{{"NFailedUnits":{{"type":"u","data":{}}},"Environment":{{"type":"as","data":[["SECRET=x"]]}}}}]}}"#,
                                        s.n_failed
                                    ),
                                );
                                r
                            }
                            "Subscribe" => {
                                s.calls.push("Subscribe".into());
                                if refuse {
                                    call.error(&"org.freedesktop.DBus.Error.AccessDenied".into(), c"refused")
                                } else {
                                    if !s.subscribers.contains(&sender) {
                                        s.subscribers.push(sender);
                                    }
                                    call.method_return()
                                }
                            }
                            "Unsubscribe" => {
                                s.calls.push("Unsubscribe".into());
                                s.subscribers.retain(|n| *n != sender);
                                call.method_return()
                            }
                            other => {
                                s.calls.push(other.to_string());
                                call.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), c"not served")
                            }
                        };
                        drop(s);
                        let _ = peer.channel.send(reply);
                        peer.channel.flush();
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        FakeSystemd { tx: Some(tx), shared, thread: Some(thread) }
    }

    /// Sets the count (returns once it is set: a read that follows sees it); emits
    /// `PropertiesChanged` only while someone has subscribed.
    pub fn set_failed(&self, n: u32) {
        if let Some(tx) = &self.tx {
            tx.send(SystemdCmd::Failed(n)).unwrap();
        }
        self.wait_until("the count to be set", |s| s.shared.lock().unwrap().n_failed == n);
    }

    /// From now on `Get` is answered with an error reply (or not, again).
    pub fn set_get_error(&self, on: bool) {
        self.shared.lock().unwrap().get_error = on;
    }

    /// Drops the connection and takes the name again: a new owner, with no subscriber.
    pub fn restart(&self) {
        if let Some(tx) = &self.tx {
            tx.send(SystemdCmd::Restart).unwrap();
        }
    }

    /// The clients subscribed now.
    pub fn subscribers(&self) -> usize {
        self.shared.lock().unwrap().subscribers.len()
    }

    /// The calls received whose text starts with `prefix` ("Get", "GetAll", "Subscribe").
    pub fn calls(&self, prefix: &str) -> usize {
        self.shared.lock().unwrap().calls.iter().filter(|c| c.starts_with(prefix)).count()
    }

    /// Waits until `f` holds (5 s at most).
    pub fn wait_until(&self, what: &str, f: impl Fn(&FakeSystemd) -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !f(self) {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for FakeSystemd {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A monitor on a private bus (`BecomeMonitor`): it records the match rules every client adds
/// (the `AddMatch` calls to the bus driver), so a test can say a rule was never asked for.
pub struct MatchMonitor {
    rules: Arc<Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MatchMonitor {
    /// Start it before the daemon under test connects.
    pub fn start(bus: &PrivateBus) -> MatchMonitor {
        let peer = Peer::connect(bus);
        let m = dbus::Message::new_method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus.Monitoring",
            "BecomeMonitor",
        )
        .unwrap()
        .append2(Vec::<String>::new(), 0u32);
        peer.call(m).expect("BecomeMonitor");
        let rules = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (seen, flag) = (rules.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                match peer.channel.blocking_pop_message(Duration::from_millis(100)) {
                    Ok(Some(msg)) => {
                        if msg.member().is_some_and(|m| &*m == "AddMatch")
                            && let Some(rule) = msg.get1::<String>()
                        {
                            seen.lock().unwrap().push(rule);
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
        MatchMonitor { rules, stop, thread: Some(thread) }
    }

    /// The rules added so far.
    pub fn rules(&self) -> Vec<String> {
        self.rules.lock().unwrap().clone()
    }
}

impl Drop for MatchMonitor {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// `session` with the failed-units count's defaults and nobody to tell (the other private-bus
/// tests do not read it).
pub fn session_plain(
    bus: &mut dyn alpheus_daemon::sources::dbus::Bus,
    domains: &[alpheus_daemon::sources::dbus::Domain],
    log_state: &mut alpheus_daemon::sources::dbus::DomainLog,
    wifi: &alpheus_daemon::sources::dbus::wifi::WifiTap,
    bt: &alpheus_daemon::sources::dbus::bt::BtTap,
    vpn: &alpheus_daemon::sources::dbus::vpn::VpnTap,
    publish: &mut dyn FnMut(alpheus_daemon::sources::dbus::SystemState) -> bool,
) -> std::io::Result<()> {
    alpheus_daemon::sources::dbus::session(
        bus,
        domains,
        log_state,
        wifi,
        bt,
        vpn,
        &alpheus_daemon::sources::dbus::UnitsTap::default(),
        alpheus_daemon::sources::dbus::units::UnitsOptions::default(),
        publish,
        &mut || true,
    )
}

/// NetworkManager's well-known name.
pub const NM_NAME: &str = "org.freedesktop.NetworkManager";
/// Fictional profiles' uuids: a saved network, and the one a failed attempt makes.
pub const UUID_HOME: &str = "11111111-2222-4333-8444-555555555555";
pub const UUID_NEW: &str = "22222222-3333-4444-8555-666666666666";
pub const NM_WIFI_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/2";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_ROOT: &str = "org.freedesktop.NetworkManager";
const NM_WIRED_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/1";
const NM_ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/1";
const NM_SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
const NM_ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const NM_DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const NM_WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const NM_ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";

/// The access point `n`'s path (from 1, in the order `FakeNetworkManager::serve` was given).
pub fn nm_ap_path(n: usize) -> String {
    format!("/org/freedesktop/NetworkManager/AccessPoint/{n}")
}

fn nm_settings_path(n: usize) -> String {
    format!("/org/freedesktop/NetworkManager/Settings/{n}")
}

/// The uuid of the fake VPN `n` (from 1).
pub fn vpn_uuid(n: usize) -> String {
    format!("aaaaaaaa-0000-4000-8000-{n:012}")
}

fn nm_active_vpn_path(n: usize) -> String {
    format!("/org/freedesktop/NetworkManager/ActiveConnection/{}", 100 + n)
}

/// A fictional VPN profile: a plugin's (`wireguard` false) or a WireGuard tunnel; `asks`: a secret
/// is asked at connection time (a flag in its settings, never a secret).
#[derive(Debug, Clone)]
pub struct FakeVpn {
    pub name: String,
    pub wireguard: bool,
    pub asks: bool,
}

/// A fictional access point (`psk`: WPA2 personal; else open).
#[derive(Debug, Clone)]
pub struct FakeAp {
    pub ssid: String,
    pub strength: u8,
    pub psk: bool,
}

impl FakeAp {
    pub fn new(ssid: &str, strength: u8, psk: bool) -> FakeAp {
        FakeAp { ssid: ssid.into(), strength, psk }
    }
}

/// What a test changes on the fake NetworkManager; each emits the signal NetworkManager would.
pub enum NmChange {
    /// WirelessEnabled, and the root's PropertiesChanged.
    Radio(bool),
    /// The Wi-Fi device's State (NMDeviceState), and the root's PropertiesChanged on
    /// ActiveConnections, as an activation starting or ending sends.
    DeviceState(u32),
    /// The access point `n` (from 1) at `strength`, and its PropertiesChanged.
    Strength(usize, u8),
    /// One more access point, and the device's AccessPointAdded.
    AddAp(FakeAp),
    /// One more saved Wi-Fi profile (uuid, SSID), and Settings' NewConnection.
    AddSaved(String, String),
    /// On access point `n` under the profile `uuid` (State 100), or on none (State 30); the root's
    /// PropertiesChanged.
    Connect(Option<(usize, String)>),
    /// The VPN `n` (from 1)'s active connection set at a State, or removed (None); then the root's
    /// PropertiesChanged on ActiveConnections and, when set, that connection's on State.
    Vpn(usize, Option<u32>),
}

struct NmState {
    radio: bool,
    device: u32,
    active: Option<(usize, String)>,
    aps: Vec<FakeAp>,
    saved: Vec<(String, String)>,
    vpns: Vec<FakeVpn>,
    /// The VPNs with an active connection: (n, State).
    vpn_active: Vec<(usize, u32)>,
}

/// A NetworkManager on a private bus (test code only): owns its name, answers the bar's reads (the
/// root, GetDevices with a wired device first, the Wi-Fi device, its access points, the active
/// connection, ListConnections, GetSettings without any secret), records every call, and emits
/// what a test changes. Dropped: its connection closes.
pub struct FakeNetworkManager {
    tx: Option<mpsc::Sender<NmChange>>,
    thread: Option<thread::JoinHandle<()>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FakeNetworkManager {
    pub fn serve(bus: &PrivateBus, aps: Vec<FakeAp>, saved: Vec<(String, String)>) -> FakeNetworkManager {
        Self::serve_with(bus, aps, saved, vec![])
    }

    /// As `serve`, with VPN profiles saved after the Wi-Fi ones (`Settings/<saved.len() + n>`, uuid
    /// `vpn_uuid(n)`, `timestamp` n).
    pub fn serve_with(bus: &PrivateBus, aps: Vec<FakeAp>, saved: Vec<(String, String)>, vpns: Vec<FakeVpn>) -> FakeNetworkManager {
        let peer = Peer::connect(bus);
        peer.own(NM_NAME);
        let (tx, rx) = mpsc::channel::<NmChange>();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let record = calls.clone();
        let mut st = NmState { radio: true, device: 30, active: None, aps, saved, vpns, vpn_active: Vec::new() };
        let thread = thread::spawn(move || {
            loop {
                match rx.try_recv() {
                    Ok(c) => {
                        for m in st.change(c) {
                            let _ = peer.channel.send(m);
                        }
                        peer.channel.flush();
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match peer.channel.blocking_pop_message(Duration::from_millis(20)) {
                    Ok(Some(call)) if call.msg_type() == dbus::message::MessageType::MethodCall => {
                        let reply = st.answer(&call, &record);
                        let _ = peer.channel.send(reply);
                        peer.channel.flush();
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        FakeNetworkManager { tx: Some(tx), thread: Some(thread), calls }
    }

    pub fn change(&self, c: NmChange) {
        if let Some(tx) = &self.tx {
            tx.send(c).unwrap();
        }
    }

    /// Every call received: "GetAll <path> <interface>", or "<member> <path>".
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Drop for FakeNetworkManager {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// An `ao` value inside a property: `{"type":"ao","data":[…]}`.
fn paths_value(paths: &[String]) -> String {
    let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
    format!(r#"{{"type":"ao","data":[{}]}}"#, quoted.join(","))
}

/// An `ao` reply (one argument): `{"type":"ao","data":[[…]]}`.
fn paths_json(paths: &[String]) -> String {
    let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
    format!(r#"{{"type":"ao","data":[[{}]]}}"#, quoted.join(","))
}

fn ssid_json(ssid: &str) -> String {
    let bytes: Vec<String> = ssid.bytes().map(|b| b.to_string()).collect();
    format!(r#"{{"type":"ay","data":[{}]}}"#, bytes.join(","))
}

fn nm_signal(path: &str, interface: &str, member: &str, object: &str) -> dbus::Message {
    let mut m = dbus::Message::new_signal(path, interface, member).unwrap();
    busctl_wire::push_reply(&mut m, &format!(r#"{{"type":"o","data":["{object}"]}}"#));
    m
}

fn nm_changed(path: &str, interface: &str, changed: &str) -> dbus::Message {
    let mut m = dbus::Message::new_signal(path, "org.freedesktop.DBus.Properties", "PropertiesChanged").unwrap();
    busctl_wire::push_reply(&mut m, &format!(r#"{{"type":"s","data":["{interface}"]}}"#));
    busctl_wire::push_reply(&mut m, &format!(r#"{{"type":"a{{sv}}","data":[{{{changed}}}]}}"#));
    busctl_wire::push_reply(&mut m, r#"{"type":"as","data":[[]]}"#);
    m
}

impl NmState {
    fn active_connections(&self) -> String {
        let mut paths: Vec<String> = self.active.iter().map(|_| NM_ACTIVE.to_string()).collect();
        paths.extend(self.vpn_active.iter().map(|(n, _)| nm_active_vpn_path(*n)));
        let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
        let list = format!("[{}]", quoted.join(","));
        format!(r#""ActiveConnections":{{"type":"ao","data":{list}}}"#)
    }

    fn root(&self) -> String {
        let primary = if self.active.is_some() { NM_ACTIVE } else { "/" };
        format!(
            r#"{{"type":"a{{sv}}","data":[{{"WirelessEnabled":{{"type":"b","data":{}}},{},"PrimaryConnection":{{"type":"o","data":"{primary}"}},"Connectivity":{{"type":"u","data":4}}}}]}}"#,
            self.radio,
            self.active_connections()
        )
    }

    fn ap(&self, n: usize) -> Option<String> {
        let a = self.aps.get(n.checked_sub(1)?)?;
        let (flags, rsn) = if a.psk { (1, 0x188) } else { (0, 0) };
        Some(format!(
            r#"{{"type":"a{{sv}}","data":[{{"Ssid":{},"Strength":{{"type":"y","data":{}}},"Flags":{{"type":"u","data":{flags}}},"WpaFlags":{{"type":"u","data":0}},"RsnFlags":{{"type":"u","data":{rsn}}},"HwAddress":{{"type":"s","data":"00:00:5E:00:53:{n:02X}"}}}}]}}"#,
            ssid_json(&a.ssid),
            a.strength
        ))
    }

    fn vpn_settings(&self, n: usize) -> Option<String> {
        let v = self.vpns.get(n.checked_sub(1)?)?;
        let (kind, secret) = match (v.wireguard, v.asks) {
            (true, true) => ("wireguard", r#","wireguard":{"private-key-flags":{"type":"u","data":1}}"#),
            (true, false) => ("wireguard", ""),
            (false, true) => (
                "vpn",
                r#","vpn":{"service-type":{"type":"s","data":"org.freedesktop.NetworkManager.openvpn"},"data":{"type":"a{ss}","data":{"password-flags":"1"}}}"#,
            ),
            (false, false) => ("vpn", ""),
        };
        Some(format!(
            r#"{{"type":"a{{sa{{sv}}}}","data":[{{"connection":{{"id":{{"type":"s","data":"{}"}},"uuid":{{"type":"s","data":"{}"}},"type":{{"type":"s","data":"{kind}"}},"timestamp":{{"type":"t","data":{n}}}}}{secret}}}]}}"#,
            v.name,
            vpn_uuid(n)
        ))
    }

    fn vpn_active_props(&self, n: usize) -> Option<String> {
        let v = self.vpns.get(n.checked_sub(1)?)?;
        let (_, state) = self.vpn_active.iter().find(|(i, _)| *i == n)?;
        let kind = if v.wireguard { "wireguard" } else { "vpn" };
        Some(format!(
            r#"{{"type":"a{{sv}}","data":[{{"Uuid":{{"type":"s","data":"{}"}},"Type":{{"type":"s","data":"{kind}"}},"State":{{"type":"u","data":{state}}},"Vpn":{{"type":"b","data":{}}}}}]}}"#,
            vpn_uuid(n),
            !v.wireguard
        ))
    }

    fn settings(&self, n: usize) -> Option<String> {
        if n > self.saved.len() {
            return self.vpn_settings(n - self.saved.len());
        }
        let (uuid, ssid) = self.saved.get(n.checked_sub(1)?)?;
        Some(format!(
            r#"{{"type":"a{{sa{{sv}}}}","data":[{{"connection":{{"id":{{"type":"s","data":"{ssid}"}},"uuid":{{"type":"s","data":"{uuid}"}},"type":{{"type":"s","data":"802-11-wireless"}}}},"802-11-wireless":{{"ssid":{},"mode":{{"type":"s","data":"infrastructure"}}}},"802-11-wireless-security":{{"key-mgmt":{{"type":"s","data":"wpa-psk"}}}}}}]}}"#,
            ssid_json(ssid)
        ))
    }

    fn answer(&self, call: &dbus::Message, calls: &Mutex<Vec<String>>) -> dbus::Message {
        let path = call.path().map(|p| p.to_string()).unwrap_or_default();
        let member = call.member().map(|m| m.to_string()).unwrap_or_default();
        let interface = call.read1::<&str>().ok().map(String::from).unwrap_or_default();
        calls.lock().unwrap().push(if member == "GetAll" { format!("GetAll {path} {interface}") } else { format!("{member} {path}") });
        let index = |prefix: &str| path.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok());
        let active_ap = self.active.as_ref().map_or("/".to_string(), |(n, _)| nm_ap_path(*n));
        let json = match (member.as_str(), path.as_str(), interface.as_str()) {
            ("GetAll", NM_PATH, NM_ROOT) => Some(self.root()),
            ("GetAll", NM_ACTIVE, _) => self.active.as_ref().map(|(_, uuid)| {
                format!(
                    r#"{{"type":"a{{sv}}","data":[{{"Uuid":{{"type":"s","data":"{uuid}"}},"Type":{{"type":"s","data":"802-11-wireless"}},"State":{{"type":"u","data":2}},"Vpn":{{"type":"b","data":false}}}}]}}"#
                )
            }),
            ("GetAll", _, "org.freedesktop.NetworkManager.Connection.Active") => {
                index("/org/freedesktop/NetworkManager/ActiveConnection/").and_then(|n| self.vpn_active_props(n.checked_sub(100)?))
            }
            ("GetDevices", NM_PATH, _) => Some(paths_json(&[NM_WIRED_DEVICE.into(), NM_WIFI_DEVICE.into()])),
            ("GetAll", NM_WIRED_DEVICE, NM_DEVICE) => {
                Some(r#"{"type":"a{sv}","data":[{"DeviceType":{"type":"u","data":1},"State":{"type":"u","data":30}}]}"#.into())
            }
            ("GetAll", NM_WIFI_DEVICE, NM_DEVICE) => {
                let active = if self.active.is_some() { NM_ACTIVE } else { "/" };
                Some(format!(
                    r#"{{"type":"a{{sv}}","data":[{{"DeviceType":{{"type":"u","data":2}},"State":{{"type":"u","data":{}}},"ActiveConnection":{{"type":"o","data":"{active}"}}}}]}}"#,
                    self.device
                ))
            }
            ("GetAll", NM_WIFI_DEVICE, NM_WIRELESS) => {
                let listed = paths_value(&(1..=self.aps.len()).map(nm_ap_path).collect::<Vec<_>>());
                Some(format!(r#"{{"type":"a{{sv}}","data":[{{"ActiveAccessPoint":{{"type":"o","data":"{active_ap}"}},"AccessPoints":{listed}}}]}}"#))
            }
            ("GetAll", _, NM_ACCESS_POINT) => index("/org/freedesktop/NetworkManager/AccessPoint/").and_then(|n| self.ap(n)),
            ("ListConnections", NM_SETTINGS, _) => {
                Some(paths_json(&(1..=self.saved.len() + self.vpns.len()).map(nm_settings_path).collect::<Vec<_>>()))
            }
            ("GetSettings", _, _) => index("/org/freedesktop/NetworkManager/Settings/").and_then(|n| self.settings(n)),
            _ => None,
        };
        match json {
            Some(j) => {
                let mut r = call.method_return();
                busctl_wire::push_reply(&mut r, &j);
                r
            }
            None => call.error(&"org.freedesktop.DBus.Error.UnknownObject".into(), c"not in the fake NetworkManager"),
        }
    }

    fn change(&mut self, c: NmChange) -> Vec<dbus::Message> {
        match c {
            NmChange::Radio(on) => {
                self.radio = on;
                vec![nm_changed(NM_PATH, NM_ROOT, &format!(r#""WirelessEnabled":{{"type":"b","data":{on}}}"#))]
            }
            NmChange::DeviceState(s) => {
                self.device = s;
                vec![nm_changed(NM_PATH, NM_ROOT, &self.active_connections())]
            }
            NmChange::Strength(n, s) => {
                if let Some(a) = n.checked_sub(1).and_then(|i| self.aps.get_mut(i)) {
                    a.strength = s;
                }
                vec![nm_changed(&nm_ap_path(n), NM_ACCESS_POINT, &format!(r#""Strength":{{"type":"y","data":{s}}}"#))]
            }
            NmChange::AddAp(a) => {
                self.aps.push(a);
                vec![nm_signal(NM_WIFI_DEVICE, NM_WIRELESS, "AccessPointAdded", &nm_ap_path(self.aps.len()))]
            }
            NmChange::AddSaved(uuid, ssid) => {
                self.saved.push((uuid, ssid));
                vec![nm_signal(
                    NM_SETTINGS,
                    "org.freedesktop.NetworkManager.Settings",
                    "NewConnection",
                    &nm_settings_path(self.saved.len()),
                )]
            }
            NmChange::Connect(to) => {
                self.device = if to.is_some() { 100 } else { 30 };
                self.active = to;
                vec![nm_changed(NM_PATH, NM_ROOT, &self.active_connections())]
            }
            NmChange::Vpn(n, state) => {
                self.vpn_active.retain(|(i, _)| *i != n);
                if let Some(s) = state {
                    self.vpn_active.push((n, s));
                }
                let mut out = vec![nm_changed(NM_PATH, NM_ROOT, &self.active_connections())];
                if let Some(s) = state {
                    out.push(nm_changed(&nm_active_vpn_path(n), NM_ACTIVE_IFACE, &format!(r#""State":{{"type":"u","data":{s}}}"#)));
                }
                out
            }
        }
    }
}

/// A binary PPM (P6, maxval 255), as grim writes it.
pub struct Ppm {
    pub width: u32,
    pub height: u32,
    data: Vec<u8>,
}

impl Ppm {
    pub fn parse(bytes: &[u8]) -> Option<Ppm> {
        // "P6", width, height, maxval, each followed by whitespace; the raster after one byte.
        let mut fields = Vec::new();
        let mut i = 0;
        while fields.len() < 4 {
            while bytes.get(i)?.is_ascii_whitespace() {
                i += 1;
            }
            let start = i;
            while !bytes.get(i)?.is_ascii_whitespace() {
                i += 1;
            }
            fields.push(std::str::from_utf8(&bytes[start..i]).ok()?.to_string());
        }
        if fields[0] != "P6" || fields[3] != "255" {
            return None;
        }
        let (width, height) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
        Some(Ppm { width, height, data: bytes.get(i + 1..)?.to_vec() })
    }

    /// (r, g, b) at (x, y); None outside the image.
    pub fn at(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let o = ((y * self.width + x) * 3) as usize;
        Some([*self.data.get(o)?, *self.data.get(o + 1)?, *self.data.get(o + 2)?])
    }
}

impl Sway {
    /// `output` of THIS test's headless sway, read with grim (wlr-screencopy). None if grim fails.
    pub fn screenshot(&self, output: &str) -> Option<Ppm> {
        let out = Command::new("grim")
            .env_clear()
            .env("PATH", env::var_os("PATH").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("WAYLAND_DISPLAY", &self.wayland)
            .args(["-t", "ppm", "-o", output, "-"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Ppm::parse(&out.stdout)
    }

    /// y of the first workspace's rect, from THIS test's sway (its own IPC socket): where tiled
    /// windows start (sway's default gaps are 0), below the space reserved at the top by exclusive
    /// zones and their margins. None if swaymsg fails or prints no rect.
    pub fn workspace_y(&self) -> Option<u32> {
        let out = Command::new("swaymsg").arg("-s").arg(&self.ipc).args(["-t", "get_workspaces", "-r"]).output().ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let rect = &text[text.find("\"rect\"")?..];
        let after = rect[rect.find("\"y\"")? + 3..].trim_start().strip_prefix(':')?.trim_start();
        let end = after.find(|c: char| !c.is_ascii_digit())?;
        after[..end].parse().ok()
    }

    /// The seat's `focus` from sway's `get_seats`: 0 while a layer surface holds the keyboard (a
    /// bar that is `Exclusive`), a container's id otherwise. None if swaymsg fails.
    pub fn seat_focus(&self) -> Option<u64> {
        let out = Command::new("swaymsg").arg("-s").arg(&self.ipc).args(["-t", "get_seats", "-r"]).output().ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let after = text[text.find("\"focus\"")? + 7..].trim_start().strip_prefix(':')?.trim_start();
        let end = after.find(|c: char| !c.is_ascii_digit())?;
        after[..end].parse().ok()
    }

    /// Screenshots `output` until `ok` holds, 10 s at most: the daemon paints asynchronously.
    pub fn wait_for_screen(&self, output: &str, ok: impl Fn(&Ppm) -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if self.screenshot(output).is_some_and(|s| ok(&s)) {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

/// Writes `dir/name` the way editors and matugen-like tools often do: a temporary file, then a
/// rename into place (one IN_MOVED_TO for `name`; the temporary's IN_CLOSE_WRITE is filtered out).
pub fn write_atomically(dir: &Path, name: &str, text: &str) {
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, text).unwrap();
    fs::rename(&tmp, dir.join(name)).unwrap();
}
