//! The daemon on a real headless Wayland compositor: one bar per output,
//! hot plugging, clean shutdown, only one daemon, and the launcher end to
//! end. Requires headless sway and wtype (`nix develop`):
//! `cargo test -p alpheus-demon --test headless_session -- --ignored --test-threads=1`.

mod common;

use alpheus_commun::launcher_ipc::{Reply, Request, encode_request, socket_path};
use alpheus_commun::theme::Rgb;
use alpheus_commun::theme::presets::preset;
use alpheus_launch::{EXIT_CANCELLED, EXIT_ERROR, EXIT_OK, EXIT_UNREACHABLE, run};
use common::mpris::{FakePlayer, Spec as PlayerSpec, text};
use common::portal::FakePortal;
use common::sni::{Entry, FakeItem, FakeMenu, Spec, Value};
use common::{
    Daemon, DaemonOptions, FakeHyprland, Peer, Ppm, PrivateBus, SIGNATURE, Sway, Workstation, fake_program, launcher, process_alive,
    start_daemon, start_daemon_with, wait_for_pid_file, write_atomically, wtype, wtype_spawn,
};
use dbus::arg::{PropMap, RefArg, Variant};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_secs(10);

/// Waits (2 s at most) for the fake pw-dump `pid` to end.
fn dies_within_2_s(pid: i32) {
    let t = std::time::Instant::now();
    while process_alive(pid) {
        assert!(t.elapsed() < Duration::from_secs(2), "pw-dump {pid} survived the daemon");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn one_bar_per_output_and_hot_plugging() {
    let sway = Sway::start("outputs", 3);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    for s in ["HEADLESS-1", "HEADLESS-2", "HEADLESS-3"] {
        assert!(d.wait_for(&format!("bar placed on {s}"), DELAY), "{s}: {:?}", d.log());
    }
    sway.command("output HEADLESS-2 disable"); // screen unplugged (dock yanked out)
    assert!(d.wait_for("bar removed from HEADLESS-2", DELAY), "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(500)), None, "the daemon survives unplugging");
    sway.command("output HEADLESS-2 enable");
    let placed = || d.log().iter().filter(|l| l.contains("bar placed on HEADLESS-2")).count();
    let t = std::time::Instant::now();
    while placed() < 2 && t.elapsed() < DELAY {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(placed(), 2, "the bar comes back with the screen: {:?}", d.log());
    sway.command("create_output");
    assert!(d.wait_for("bar placed on HEADLESS-4", DELAY), "screen added: {:?}", d.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn clean_shutdown_on_sigterm_and_lock_released() {
    let sway = Sway::start("shutdown", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    d.signal(libc::SIGTERM);
    assert_eq!(d.exit(Duration::from_millis(1500)), Some(0), "clean exit, code 0: {:?}", d.log());
    assert!(d.wait_for("shutdown", Duration::from_millis(500)));
    let d2 = start_daemon(&sway);
    assert!(d2.wait_for("bar placed on HEADLESS-1", DELAY), "the lock was released: {:?}", d2.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn compositor_crash_is_an_end_of_session_exit_code_0() {
    // Decided and verified on this machine (recorded 2026-10-04): killing
    // sway with a SIGKILL closes the daemon's Wayland connection with EPIPE
    // ("Broken pipe (os error 32)"), exactly what a clean disconnect would
    // also produce (end of session, compositor restart — see spec § 3).
    // Nothing, on the client side, tells the two apart. The daemon never
    // reconnects on its own in either case (it's the session that
    // relaunches it, through exec-once then shell-ctl): treating them the
    // same way (code 0, end_of_session) keeps an external supervisor from
    // wrongly reading an ordinary end of session as a sign of failure.
    let mut sway = Sway::start("compositor-crash", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    sway.kill();
    let code = d.exit(DELAY);
    assert_eq!(code, Some(0), "connection cut (EPIPE/ECONNRESET): end of session, code 0 ({:?})", d.log());
    assert!(d.wait_for("end of session", Duration::from_millis(500)), "{:?}", d.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn only_one_daemon_at_a_time() {
    let sway = Sway::start("unique", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d1 = start_daemon(&sway);
    assert!(d1.wait_for("bar placed on HEADLESS-1", DELAY));
    let mut d2 = start_daemon(&sway);
    assert_eq!(d2.exit(DELAY), Some(1));
    assert!(d2.wait_for("already running", Duration::from_secs(1)), "{:?}", d2.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn pw_dump_dies_with_the_daemon() {
    let sway = Sway::start("pw-dump", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    let pid = wait_for_pid_file(&sway.runtime.join("pw-dump.pid"), DELAY);
    assert!(process_alive(pid), "the fake pw-dump runs: {:?}", d.log());
    // SIGKILL: no Drop runs in the daemon; only PR_SET_PDEATHSIG is left.
    d.signal(libc::SIGKILL);
    let _ = d.child.wait();
    dies_within_2_s(pid);
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn pw_dump_ends_with_a_clean_shutdown() {
    let sway = Sway::start("pw-dump-term", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let pid = wait_for_pid_file(&sway.runtime.join("pw-dump.pid"), DELAY);
    // SIGTERM: the clean path, within the daemon's grace period (1 s, see
    // clean_shutdown_on_sigterm…); the sound thread ends with the process,
    // and PDEATHSIG takes pw-dump along.
    d.signal(libc::SIGTERM);
    assert_eq!(d.exit(Duration::from_millis(1500)), Some(0), "clean exit, code 0: {:?}", d.log());
    dies_within_2_s(pid);
}

/// The daemon reads the system bus where `DBUS_SYSTEM_BUS_ADDRESS` points
/// (libdbus reads it): pointing at a path where nothing listens (the
/// harness's default), the D-Bus source is down — had it fallen back to the
/// machine's /run/dbus/system_bus_socket, it would be up; pointing at a
/// private bus serving the recorded workstation, it is up.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_system_bus_address_is_honoured() {
    let sway = Sway::start("bus-nowhere", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon(&sway);
    assert!(d.wait_for("D-Bus source down", DELAY), "no bus where the address points: {:?}", d.log());
    let bus = PrivateBus::start("bus-address");
    let _workstation = Workstation::serve(&bus);
    let sway2 = Sway::start("bus-private", 1);
    let _hypr2 = FakeHyprland::start(&sway2.runtime, SIGNATURE);
    let d2 = start_daemon_with(&sway2, &DaemonOptions { system_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d2.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d2.log());
    // A bounded poll (I3 carry-over, tray plan 1 review), not a flat sleep:
    // fails as soon as "source down" shows up, rather than only finding out
    // after waiting out a fixed delay.
    let t = std::time::Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        assert!(d2.log().iter().all(|l| !l.contains("source down")), "the private bus reached: {:?}", d2.log());
        std::thread::sleep(Duration::from_millis(50));
    }
}

const SHOWN: &str = "launcher shown on";

fn ready(d: &Daemon) {
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
}

fn desktop(name: &str, exec: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n")
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let t = Instant::now();
    while !path.exists() {
        assert!(t.elapsed() < timeout, "{} never written", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

/// alpheus-launch's whole program (alpheus_launch::run) in a thread, as a
/// script runs it: (exit code, stdout, stderr).
fn script(sway: &Sway, args: &[&str], stdin: &str) -> thread::JoinHandle<(i32, String, String)> {
    let socket = socket_path(&sway.runtime);
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let input = stdin.as_bytes().to_vec();
    thread::spawn(move || {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, &socket, &mut input.as_slice(), &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
    })
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn toggle_opens_and_closes_the_launcher() {
    let sway = Sway::start("launcher-toggle", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    ready(&d);
    let mode = std::fs::metadata(socket_path(&sway.runtime)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "owner-only socket");
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    // Past TOGGLE_DEBOUNCE: a real Toggle closes it.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for("launcher closed (toggle)", DELAY), "{:?}", d.log());
    // close is idempotent: answered, nothing to close.
    assert_eq!(launcher(&sway, &Request::Close), Reply::Done);
    assert_eq!(launcher(&sway, &Request::Close), Reply::Done);
    // Mod+Space twice quickly (the Hyprland press-bind refire, Mae
    // 2026-10-06): the box opens, and the second Toggle — arriving within
    // TOGGLE_DEBOUNCE — is still answered Done (alpheus-launch exits 0) but
    // must NOT close the box back down. This replaces the previous
    // expectation here (opened then closed, "never two boxes"): that was
    // exactly the bug a real Hyprland press bind triggers on release.
    // Both sent back to back, without waiting for "shown": the second is
    // then within the window whatever the machine load (the unit test of
    // `toggle_is_debounced` is the precise check of the 250 ms edge).
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("launcher closed (toggle)"), 1, "the quick second Toggle must not close it: {:?}", d.log());
    // Past the window, a Toggle closes it for good.
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher closed (toggle)", 2, DELAY), "{:?}", d.log());
    assert_eq!(d.count("launcher closed ("), 2, "the two Close requests closed nothing: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar keeps running");
}

#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn typing_launches_the_fake_program() {
    let sway = Sway::start("launcher-type", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    fake_program(&sway, "fake-notes");
    let entry = desktop("Fakenotes", "fake-notes --new-window %U");
    let files = [("alpheus-fake-notes.desktop", entry.as_str())];
    let d = start_daemon_with(&sway, &DaemonOptions { desktop_files: &files, ..DaemonOptions::default() });
    ready(&d);
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    // -s 300: wtype's virtual keyboard appears first; the open launcher
    // binds it (capability appearing while open) before the keys arrive.
    assert!(wtype(&sway, &["-s", "300", "fakeno", "-k", "Return"]).success());
    let marker = sway.runtime.join("launched-fake-notes");
    wait_for_file(&marker, DELAY);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "--new-window\n", "argv, %U removed, no shell");
    assert!(d.wait_for("launcher closed (activated)", DELAY), "{:?}", d.log());
    let history = std::fs::read_to_string(sway.runtime.join("state/alpheus/launcher-history")).unwrap();
    assert!(history.lines().any(|l| l.ends_with("\tapp:alpheus-fake-notes.desktop")), "{history:?}");
    assert!(!history.contains("fakeno"), "never typed text in the history: {history:?}");
}

#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn dmenu_round_trip_and_cancel() {
    let sway = Sway::start("launcher-dmenu", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon(&sway);
    ready(&d);
    // A choice: the line on stdout, exit 0.
    let menu = script(&sway, &["--dmenu", "--prompt", "Pick"], "alpha\nbeta\ngamma\n");
    assert!(d.wait_for_count(SHOWN, 1, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "gam", "-k", "Return"]).success());
    let (code, out, _) = menu.join().unwrap();
    assert_eq!((code, out.as_str()), (EXIT_OK, "gamma\n"));
    assert!(d.wait_for("launcher closed (activated)", DELAY), "{:?}", d.log());
    // A second script menu while one is open: refused, exit 2. Then Escape: nothing printed, exit 1.
    let menu = script(&sway, &["--dmenu"], "one\ntwo\n");
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
    let (code, out, err) = script(&sway, &["--dmenu"], "other\n").join().unwrap();
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("another menu is already open"), "{err}");
    assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
    let (code, out, _) = menu.join().unwrap();
    assert_eq!((code, out.as_str()), (EXIT_CANCELLED, ""));
    assert!(d.wait_for("launcher closed (escape)", DELAY), "{:?}", d.log());
    // Each close by a key (Return, then Escape) released the keyboard, though the key was still
    // down: its focus, the launcher's box, died with it (fix round 1 of media task 7).
    assert!(d.wait_for_count("keyboard released", 2, DELAY), "the keyboard stayed bound at rest: {:?}", d.log());
    // Free text with no lines (tools-menu's "Cible (IP / hôte)"): Enter returns what was typed.
    let menu = script(&sway, &["--dmenu", "--prompt", "Target"], "");
    assert!(d.wait_for_count(SHOWN, 3, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "10.0.0.1", "-k", "Return"]).success());
    let (code, out, _) = menu.join().unwrap();
    assert_eq!((code, out.as_str()), (EXIT_OK, "10.0.0.1\n"));
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn dmenu_client_killed_closes_the_launcher() {
    let sway = Sway::start("launcher-killed", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon(&sway);
    ready(&d);
    let mut client = UnixStream::connect(socket_path(&sway.runtime)).unwrap();
    client.write_all(&encode_request(&Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into()] })).unwrap();
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    drop(client); // the script killed (Ctrl-C, a timeout) while its menu is open
    assert!(d.wait_for("the dmenu client hung up", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher closed (close)", DELAY), "{:?}", d.log());
    // Nothing left behind: it opens again.
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn output_unplugged_while_the_launcher_is_open() {
    let sway = Sway::start("launcher-unplug", 3);
    let _hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    ready(&d);
    assert!(d.wait_for("bar placed on HEADLESS-3", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(500)); // the compositor state (focused output) is in
    let menu = script(&sway, &["--dmenu"], "a\nb\n");
    assert!(d.wait_for("launcher shown on HEADLESS-2", DELAY), "on the focused output: {:?}", d.log());
    sway.command("output HEADLESS-2 disable"); // the dock unplugged
    assert!(d.wait_for("launcher closed (output gone)", DELAY), "{:?}", d.log());
    let (code, out, _) = menu.join().unwrap();
    assert_eq!((code, out.as_str()), (EXIT_CANCELLED, ""), "the script sees a cancel");
    assert!(d.wait_for("bar removed from HEADLESS-2", DELAY), "the bar's own logic runs as before: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(300)), None, "the daemon survives");
    sway.command("output HEADLESS-2 enable");
    assert!(d.wait_for_count("bar placed on HEADLESS-2", 2, DELAY), "{:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "the launcher opens again: {:?}", d.log());
}

/// No sway needed: a runtime dir with no daemon in it. Exit 3, not 2: only
/// "unreachable" makes shell-ctl and tools-menu fall back to fuzzel (task 15).
#[test]
fn alpheus_launch_exits_3_when_the_daemon_is_absent() {
    let dir = std::env::temp_dir().join(format!("alpheus-no-daemon-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut empty: &[u8] = &[];
    let code = run(&[OsString::from("--toggle")], &socket_path(&dir), &mut empty, &mut out, &mut err);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, EXIT_UNREACHABLE);
    assert!(String::from_utf8_lossy(&err).contains("not running"), "{}", String::from_utf8_lossy(&err));
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn sigterm_cancels_a_waiting_script_and_removes_the_socket() {
    let sway = Sway::start("launcher-sigterm", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon(&sway);
    ready(&d);
    let menu = script(&sway, &["--dmenu"], "a\nb\n");
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    // A request still being read at shutdown: its connection source holds a
    // LoopHandle clone — the kind of cycle that would keep the listener, and
    // its socket file, alive past the end of `run`.
    let mut slow = UnixStream::connect(socket_path(&sway.runtime)).unwrap();
    slow.write_all(b"alpheus-launcher/1 dm").unwrap();
    thread::sleep(Duration::from_millis(200));
    d.signal(libc::SIGTERM);
    assert_eq!(d.exit(Duration::from_millis(1500)), Some(0), "a clean stop, within the grace period: {:?}", d.log());
    let (code, out, _) = menu.join().unwrap();
    assert_eq!((code, out.as_str()), (EXIT_CANCELLED, ""), "the waiting script sees a cancel, not a cut connection");
    assert!(!socket_path(&sway.runtime).exists(), "the socket file goes with the daemon");
    drop(slow);
}

/// Waits (10 s at most) until someone owns `org.kde.StatusNotifierWatcher` on
/// `bus`: the daemon's tray is up and serving.
fn wait_for_the_watcher(bus: &PrivateBus) {
    let peer = Peer::connect(bus);
    let t = std::time::Instant::now();
    loop {
        let m = dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner")
            .unwrap()
            .append1("org.kde.StatusNotifierWatcher");
        if peer.call(m).unwrap().read1::<bool>().unwrap() {
            return;
        }
        assert!(t.elapsed() < DELAY, "the daemon never took the watcher name");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The tray alone then the clock: the first icon is cell 1 (cell 0 is the
/// margin), 8.4 px per cell at 14 px (any monospace font of the test machine
/// keeps x = 12 inside it).
const TRAY_ONLY: &str = "modules = [\"tray\", \"clock\"]\n";
const FIRST_ICON: (u32, u32) = (12, 10);

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_fake_app_shows_in_the_tray_and_a_left_click_activates_it() {
    let bus = PrivateBus::start("tray-click");
    let sway = Sway::start("tray-click", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(TRAY_ONLY), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    wait_for_the_watcher(&bus);
    let nimbus = FakeItem::start(&bus, Spec::kde(1));
    assert!(d.wait_for("tray: icons shown: 1", DELAY), "{:?}", d.log());
    sway.click(FIRST_ICON.0, FIRST_ICON.1, "button1");
    let activate = nimbus.wait_call("Activate", DELAY);
    assert_eq!(activate.as_deref(), Some("org.kde.StatusNotifierItem.Activate(12, 10)"), "at the press position: {:?}", d.log());
    // The horizontal wheel over the icon: one notch right, one Scroll(-120, horizontal) — Plasma's
    // actual convention (decision 4, fixed in `tray_horizontal`: qtbase negates the horizontal
    // value120 before Plasma re-exposes it, so right is -120, left is +120; vertical is unaffected).
    sway.scroll(FIRST_ICON.0, FIRST_ICON.1, "right", 1);
    let scroll = nimbus.wait_call("Scroll", DELAY);
    assert_eq!(scroll.as_deref(), Some("org.kde.StatusNotifierItem.Scroll(-120, horizontal)"), "one notch right: {:?}", d.log());
    std::thread::sleep(Duration::from_millis(300));
    let scrolls = nimbus.calls().iter().filter(|c| c.contains(".Scroll(")).count();
    assert_eq!(scrolls, 1, "one notch, one Scroll: {:?}", nimbus.calls());
    // A burst of signals: dirty, one frame callback, one re-read (decision 10).
    let reads = || nimbus.calls().iter().filter(|c| c.ends_with("Get(org.kde.StatusNotifierItem, IconName)")).count();
    assert_eq!(reads(), 1, "the first Get only: {:?}", nimbus.calls());
    nimbus.set("IconName", Value::Str("nimbus-busy".into()));
    nimbus.burst("NewIcon", 20);
    let t = std::time::Instant::now();
    while reads() < 2 && t.elapsed() < DELAY {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(500));
    // A signal landing after the frame callback may cost one more read under load; never one per
    // signal (an uncoalesced daemon would read 21 times).
    assert!((2..=3).contains(&reads()), "20 NewIcon, one re-read per frame at most: {:?}", nimbus.calls());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn the_bar_works_without_a_session_bus() {
    let sway = Sway::start("tray-no-bus", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    // No session_bus: the harness points DBUS_SESSION_BUS_ADDRESS where nothing listens.
    let mut d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(TRAY_ONLY), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("tray: no session bus", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(3));
    let tray_lines: Vec<String> = d.log().into_iter().filter(|l| l.contains("tray:")).collect();
    assert_eq!(tray_lines.len(), 1, "one line, never retried: {tray_lines:?}");
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_closed_session_bus_empties_the_tray_once() {
    let mut bus = PrivateBus::start("tray-closed");
    let sway = Sway::start("tray-closed", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(TRAY_ONLY), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    wait_for_the_watcher(&bus);
    let _nimbus = FakeItem::start(&bus, Spec::kde(1));
    assert!(d.wait_for("tray: icons shown: 1", DELAY), "{:?}", d.log());
    bus.kill();
    assert!(d.wait_for("tray: the session bus closed", DELAY), "{:?}", d.log());
    assert!(d.wait_for("tray: icons shown: 0", DELAY), "the tray emptied: {:?}", d.log());
    // The dead socket stays readable: its source must be gone, or the loop spins.
    std::thread::sleep(Duration::from_millis(500));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_millis(4500));
    let wakeups = d.main_thread_wakeups() - before;
    assert!(wakeups <= 4, "{wakeups} wake-ups of the loop in 4.5 s after the bus closed");
    let log = d.log();
    assert_eq!(log.iter().filter(|l| l.contains("session bus closed")).count(), 1, "{log:?}");
    assert_eq!(
        log.iter().filter(|l| l.contains("tray: on the session bus") || l.contains("tray: no session bus")).count(),
        1,
        "never reconnected: {log:?}"
    );
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the daemon alive: {log:?}");
}

/// A bar with the tray first (its first icon at cell 1, x ≈ 12 px) and the clock last (right end),
/// on a private session bus; one KDE-like fake item.
fn tray_bar(name: &str, extra: &str) -> (PrivateBus, Sway, FakeHyprland, Daemon, FakeItem) {
    let bus = PrivateBus::start(name);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = format!("modules = [\"tray\", \"clock\"]\n{extra}");
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(&toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let item = FakeItem::start(&bus, Spec::kde(1));
    // Registered, its eight Gets answered, the bar redrawn: well under a second on the private bus.
    std::thread::sleep(Duration::from_secs(1));
    (bus, sway, hypr, d, item)
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_tooltip_appears_after_half_a_second_and_goes_on_leave() {
    let (_bus, sway, _hypr, d, _item) = tray_bar("tooltip", "");
    // The virtual pointer first, off the bar (its creation's 300 ms wait must not count as hovering).
    sway.hover(640, 400);
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    sway.hover(12, 10);
    assert!(d.wait_for("tooltip shown", DELAY), "{:?}", d.log());
    assert!(t.elapsed() >= Duration::from_millis(450), "not before ½ s: {:?}", t.elapsed());
    assert_eq!(d.count("tooltip shown"), 1);
    sway.hover(640, 400); // off the bar: its surface's leave
    assert!(d.wait_for("popup closed (hover ended)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("tooltip shown"), 1, "nothing shown again off the icons: {:?}", d.log());
}

/// The hover timer exists only while the pointer rests on an icon: resting on the clock, the
/// loop sleeps (the window avoids the clock's minute tick).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn no_timer_while_the_pointer_is_elsewhere() {
    let (_bus, sway, _hypr, d, _item) = tray_bar("tooltip-rest", "");
    sway.hover(640, 400); // the virtual pointer exists, off the bar
    while alpheus_demon::clock::LocalTime::now().0.second > 50 {
        std::thread::sleep(Duration::from_millis(200));
    }
    sway.hover(1260, 10); // the clock, at the right end
    // The motion handled; a timer armed by it would fire at 0.5 s, inside the window.
    std::thread::sleep(Duration::from_millis(200));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    assert_eq!(delta, 0, "the loop woke {delta} times with the pointer on the clock: {:?}", d.log());
    assert!(d.log().iter().all(|l| !l.contains("tooltip")), "{:?}", d.log());
}

/// Task 15 fix round, from the task 16 review: opening the launcher used to close an open
/// tooltip (`popup_close(Launcher)`) without also cancelling `Hover` — `Hover.over` kept naming
/// the same tray item, and since `Hover::moved` compares only the item (never a surface), resting
/// on that SAME icon afterwards was wrongly seen as "still on it" and never re-armed the timer.
/// Fixed: `launcher_open` now cancels the hover too.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn opening_the_launcher_lets_the_same_tooltip_show_again_after() {
    let (_bus, sway, _hypr, d, _item) = tray_bar("tooltip-launcher", "");
    sway.hover(640, 400);
    std::thread::sleep(Duration::from_millis(200));
    sway.hover(12, 10); // the tray's one icon
    assert!(d.wait_for("tooltip shown", DELAY), "{:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for("launcher shown", DELAY), "{:?}", d.log());
    // Past the toggle debounce: a Toggle right after the open is Hyprland's press-bind refire.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for("launcher closed (toggle)", DELAY), "{:?}", d.log());
    // Still resting near the same icon (a different pixel, same cell): before the fix, this was
    // seen as "still on the same icon" and never re-armed.
    sway.hover(14, 10);
    assert!(d.wait_for_count("tooltip shown", 2, DELAY), "{:?}", d.log());
}

/// Task 15 fix round, from the task 16 review: an output going away used to close its tooltip
/// (`popup_output_gone`) without also cancelling `Hover` — the SAME staleness as above, reached
/// from a bar removal instead of the launcher opening. Fixed: `popup_output_gone` now cancels the
/// hover too.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn an_unplugged_output_lets_the_same_tooltip_show_again_once_restored() {
    let (_bus, sway, _hypr, d, _item) = tray_bar("tooltip-unplug", "");
    sway.hover(640, 400);
    std::thread::sleep(Duration::from_millis(200));
    sway.hover(12, 10); // the tray's one icon
    assert!(d.wait_for("tooltip shown", DELAY), "{:?}", d.log());
    sway.command("output HEADLESS-1 disable");
    assert!(d.wait_for("popup closed (output gone)", DELAY), "{:?}", d.log());
    sway.command("output HEADLESS-1 enable");
    assert!(d.wait_for_count("bar placed on HEADLESS-1", 2, DELAY), "{:?}", d.log());
    sway.hover(640, 400); // off the bar first: the restored bar must see a real arrival
    std::thread::sleep(Duration::from_millis(100));
    sway.hover(12, 10); // the same icon again, same ItemKey, a brand new bar surface
    assert!(d.wait_for_count("tooltip shown", 2, DELAY), "{:?}", d.log());
}

/// Fix round 2026-10-06 (main fix): `tray_output` used to cancel the hover unconditionally on
/// every `out.redraw`, which fires for ANY property changing on ANY item — a messenger's "3
/// unread" becoming "4 unread" closed (or never armed) the tooltip even though nothing about the
/// hovered icon's OWN position changed. `ToolTip` here never affects which letter/glyph is drawn
/// (`icons::icons` falls back to the item's Title, never its tooltip text), so `bar.apply` reports
/// no change and the hover is left alone entirely: the tooltip stays shown with its original text
/// (the simpler of the two options the brief allowed — see task-16-fix-report.md).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_hovered_items_tooltip_text_changing_does_not_close_it() {
    let (_bus, sway, _hypr, d, item) = tray_bar("tooltip-text-changes", "");
    sway.hover(640, 400);
    std::thread::sleep(Duration::from_millis(200));
    sway.hover(12, 10); // the tray's one icon
    assert!(d.wait_for("tooltip shown", DELAY), "{:?}", d.log());
    item.set("ToolTip", Value::ToolTip { title: "Nimbus — 4 new messages".into(), pixmap_bytes: 0 });
    item.emit("NewToolTip");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("popup closed"), 0, "the tooltip must not close just because its text changed: {:?}", d.log());
    assert_eq!(d.count("tooltip shown"), 1, "still the same tooltip, not reopened: {:?}", d.log());
}

/// Fix round 2026-10-06 (main fix): an icon animating faster than the ½ s delay (`NewIcon` signals
/// a few tens of milliseconds apart) used to cancel the pending hover on every single one of them
/// — since the pointer never moves again on its own, the timer was gone for good and the tooltip
/// never showed. `IconName` changing never moves the hovered icon's zone either (one item, one
/// cell, same position throughout), so `hover_target_moved` stays false and the timer survives.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn an_animating_icon_still_gets_its_tooltip_after_half_a_second() {
    let (_bus, sway, _hypr, d, item) = tray_bar("tooltip-animated", "");
    sway.hover(640, 400);
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    sway.hover(12, 10); // the tray's one icon
    let mut frame = 0;
    while t.elapsed() < Duration::from_millis(450) {
        frame += 1;
        item.set("IconName", Value::Str(format!("nimbus-frame-{frame}")));
        item.emit("NewIcon");
        std::thread::sleep(Duration::from_millis(90));
    }
    assert!(d.wait_for("tooltip shown", DELAY), "{:?}", d.log());
    assert!(t.elapsed() >= Duration::from_millis(450), "not before ½ s: {:?}", t.elapsed());
}

/// Item 4 of the 2026-10-06 fix round: `[tray] tooltips = false` arms no timer at all, even
/// resting right on an icon — no wake-up of the loop, no "tooltip" log line ever.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn tooltips_false_gives_no_timer_and_no_tooltip() {
    let (_bus, sway, _hypr, d, _item) = tray_bar("tooltip-off", "[tray]\ntooltips = false\n");
    sway.hover(640, 400); // the virtual pointer exists, off the bar
    // Not across a minute tick: the clock's own wake-up would count (as in the test above).
    while alpheus_demon::clock::LocalTime::now().0.second > 55 {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(200));
    sway.hover(12, 10); // the tray's one icon: armed with tooltips on, never with them off
    std::thread::sleep(Duration::from_millis(200));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(2));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    assert_eq!(delta, 0, "tooltips = false: the loop woke {delta} times with the pointer on the icon: {:?}", d.log());
    assert!(d.log().iter().all(|l| !l.contains("tooltip")), "{:?}", d.log());
}

/// Item 4 of the 2026-10-06 fix round: a press on the bar, even away from the hovered icon, closes
/// a shown tooltip (already true via the `Press` arm's `hover.cancel()`; exercised headlessly here).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_press_on_the_bar_closes_a_shown_tooltip() {
    let (_bus, sway, _hypr, d, _item) = tray_bar("tooltip-press", "");
    sway.hover(640, 400);
    std::thread::sleep(Duration::from_millis(200));
    sway.hover(12, 10); // the tray's one icon
    assert!(d.wait_for("tooltip shown", DELAY), "{:?}", d.log());
    sway.click(1260, 10, "button1"); // elsewhere on the bar (the clock): still cancels the hover
    assert!(d.wait_for("popup closed (hover ended)", DELAY), "{:?}", d.log());
}

/// Item 4 of the 2026-10-06 fix round: the hovered item's application quitting (its
/// StatusNotifierItem disappearing from the bus) closes its shown tooltip as `ItemGone`, not
/// `HoverEnded` (already true via `tooltip_item_gone`; exercised headlessly here).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_hovered_item_vanishing_closes_its_tooltip() {
    let (_bus, sway, _hypr, d, item) = tray_bar("tooltip-vanish", "");
    sway.hover(640, 400);
    std::thread::sleep(Duration::from_millis(200));
    sway.hover(12, 10); // the tray's one icon
    assert!(d.wait_for("tooltip shown", DELAY), "{:?}", d.log());
    item.kill(); // the application quits: its StatusNotifierItem disappears
    assert!(d.wait_for("popup closed (item gone)", DELAY), "{:?}", d.log());
}

// ---- Menus wired end to end (task 17) ----

/// The id of the entry labelled `label` (mnemonic underscores aside) in a fake menu.
fn entry_id(menu: &FakeMenu, label: &str) -> i32 {
    fn find(entries: &[Entry], label: &str) -> Option<i32> {
        entries.iter().find_map(|e| {
            let named = e.props.iter().any(|(k, v)| k == "label" && matches!(v, Value::Str(s) if s.replace('_', "") == label));
            if named { Some(e.id) } else { find(&e.children, label) }
        })
    }
    find(&menu.root, label).unwrap_or_else(|| panic!("no {label:?} in {menu:?}"))
}

/// A KDE-like fake whose menu is FakeMenu::sample(): "Open" (selected at open), "Mute", "Low",
/// "High", a separator, "Disabled", a hidden entry, "More" (two children, fetched on demand).
fn menu_bar(name: &str) -> (PrivateBus, Sway, FakeHyprland, Daemon, FakeItem) {
    let bus = PrivateBus::start(name);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"tray\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let mut spec = Spec::kde(1);
    spec.menu = Some(FakeMenu::sample());
    let item = FakeItem::start(&bus, spec);
    std::thread::sleep(Duration::from_secs(1)); // registered, read, drawn
    (bus, sway, hypr, d, item)
}

/// Keys typed after wtype's keyboard appeared (fact check 5: the bar's surface takes it).
fn keys(sway: &Sway, names: &[&str]) {
    let mut args = vec!["-s", "300"];
    for n in names {
        args.extend(["-k", n]);
    }
    assert!(wtype(sway, &args).success());
}

#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn a_fake_apps_menu_opens_runs_an_item_and_closes() {
    let (_bus, sway, _hypr, d, item) = menu_bar("menu-run");
    sway.click(12, 10, "button3"); // right click on the icon
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    keys(&sway, &["Down", "Down", "Return"]); // Open → Mute → Low
    let low = format!("Event({},", entry_id(&FakeMenu::sample(), "Low"));
    assert!(wait_recorded(&item, &low), "{:?}", item.calls());
    assert!(item.calls().iter().any(|c| c.contains(&low) && c.contains("clicked")), "{:?}", item.calls());
    assert!(d.wait_for("popup closed (activated)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("tray: menu session closed", DELAY), "the session ends with the popup: {:?}", d.log());
}

/// Waits until one of `item`'s recorded calls contains `pattern`.
fn wait_recorded(item: &FakeItem, pattern: &str) -> bool {
    let t = Instant::now();
    while t.elapsed() < DELAY {
        if item.calls().iter().any(|c| c.contains(pattern)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn a_submenu_opens_with_right_and_back_with_left() {
    let (_bus, sway, _hypr, d, item) = menu_bar("menu-submenu");
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    let sample = FakeMenu::sample();
    let more = entry_id(&sample, "More");
    keys(&sway, &["End", "Right"]); // on More, entered
    assert!(wait_recorded(&item, &format!("AboutToShow({more}")), "the submenu fetched on demand: {:?}", item.calls());
    assert!(wait_recorded(&item, &format!("GetLayout({more},")), "{:?}", item.calls());
    std::thread::sleep(Duration::from_millis(300)); // grafted and entered
    // Back at the root, on More; Up skips Disabled and the separator: High.
    keys(&sway, &["Left", "Up", "Return"]);
    let high = entry_id(&sample, "High");
    assert!(wait_recorded(&item, &format!("Event({high},")), "← went back one level: {:?}", item.calls());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn steam_left_click_opens_the_menu() {
    let bus = PrivateBus::start("menu-steam");
    let sway = Sway::start("menu-steam", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"tray\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let item = FakeItem::start(&bus, Spec::steam());
    std::thread::sleep(Duration::from_secs(1));
    sway.click(12, 10, "button1"); // Activate refused (UnknownMethod): the menu instead
    assert!(d.wait_for("menu shown", DELAY), "{:?} / {:?}", d.log(), item.calls());
    assert!(item.calls().iter().any(|c| c.contains("Activate")), "Activate tried first: {:?}", item.calls());
}

/// Mae (2026-10-06): a click on the icon whose menu is shown, with the button that opened it,
/// only closes it. Steam's menu opens from a failed left-click Activate; the SAME left click again
/// must only close it — no new Activate, no flicker.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_second_left_click_on_steams_icon_only_closes_its_menu() {
    let bus = PrivateBus::start("menu-steam-toggle");
    let sway = Sway::start("menu-steam-toggle", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"tray\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let item = FakeItem::start(&bus, Spec::steam());
    std::thread::sleep(Duration::from_secs(1));
    sway.click(12, 10, "button1"); // Activate refused: the menu instead
    assert!(d.wait_for("menu shown", DELAY), "{:?} / {:?}", d.log(), item.calls());
    std::thread::sleep(Duration::from_millis(300)); // the popup configured and mapped
    let activates_before = item.calls().iter().filter(|c| c.contains("Activate")).count();
    sway.click(12, 10, "button1"); // the SAME left click: only closes it
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(item.calls().iter().filter(|c| c.contains("Activate")).count(), activates_before, "no new Activate: {:?}", item.calls());
    assert_eq!(d.count("menu shown"), 1, "closed, not reopened: {:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn an_outside_click_closes_the_menu() {
    let (_bus, sway, _hypr, d, item) = menu_bar("menu-outside");
    // Escape first, spec § 2's other way out.
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("tray: menu session closed", DELAY), "{:?}", d.log());
    sway.click(12, 10, "button3");
    assert!(d.wait_for_count("menu shown", 2, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300)); // the popup configured and mapped
    sway.click(640, 400, "button1");
    assert!(d.wait_for("popup closed (dismissed)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("tray: menu session closed", 2, DELAY), "{:?}", d.log());
    assert!(item.calls().iter().all(|c| !c.contains("Event")), "nothing clicked: {:?}", item.calls());
}

/// An item whose properties answer but whose menu answers late or never: a `Peer` (not a FakeItem,
/// whose menus always answer at once), registered with the daemon's watcher. `menu_after`: None, the
/// menu never answers; Some(d), `AboutToShow` and then `GetLayout` are each answered after `d` (one
/// entry, "Late").
fn hushed_item(bus: &PrivateBus, menu_after: Option<Duration>) {
    let peer = Peer::connect(bus);
    std::thread::spawn(move || {
        let register = dbus::Message::new_method_call(
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "RegisterStatusNotifierItem",
        )
        .unwrap()
        .append1("/StatusNotifierItem");
        let _ = peer.call(register);
        while let Some(call) = peer.next_call() {
            if call.interface().as_deref() == Some("com.canonical.dbusmenu") {
                let Some(after) = menu_after else { continue }; // never answered
                std::thread::sleep(after);
                let reply = if call.member().as_deref() == Some("AboutToShow") {
                    call.method_return().append1(false)
                } else {
                    // GetLayout: revision 1, the root (its children-display) with one entry.
                    type Node = (i32, PropMap, Vec<Variant<Box<dyn RefArg>>>);
                    let prop = |k: &str, v: &str| PropMap::from([(k.to_string(), Variant(Box::new(v.to_string()) as Box<dyn RefArg>))]);
                    let late: Node = (1, prop("label", "Late"), Vec::new());
                    let root: Node = (0, prop("children-display", "submenu"), vec![Variant(Box::new(late) as Box<dyn RefArg>)]);
                    call.method_return().append2(1u32, root)
                };
                let _ = peer.channel.send(reply);
                peer.channel.flush();
                continue;
            }
            if call.interface().as_deref() != Some("org.freedesktop.DBus.Properties") {
                continue;
            }
            let property: String = call.read2::<&str, &str>().map(|(_, p)| p.to_string()).unwrap_or_default();
            let reply = match property.as_str() {
                "Id" => call.method_return().append1(dbus::arg::Variant("hush")),
                "Menu" => call.method_return().append1(dbus::arg::Variant(dbus::Path::from("/Menu"))),
                _ => dbus::Message::error(
                    &call,
                    &"org.freedesktop.DBus.Error.UnknownProperty".into(),
                    &std::ffi::CString::new("no").unwrap(),
                ),
            };
            let _ = peer.channel.send(reply);
            peer.channel.flush();
        }
    });
}

/// An item with a `Menu` and no `ItemIsMenu` (so a left click first tries Activate): a `Peer`
/// whose `Activate` is refused only after `delay`, but whose dbusmenu answers at once (one entry,
/// "Item") so a menu that does open shows right away — review fix round requirement 7: the delay
/// is wide enough to click elsewhere on the bar before the error reply, and so the late
/// `open_menu`, lands.
fn slow_activate_item(bus: &PrivateBus, delay: Duration) {
    let peer = Peer::connect(bus);
    std::thread::spawn(move || {
        let register = dbus::Message::new_method_call(
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "RegisterStatusNotifierItem",
        )
        .unwrap()
        .append1("/StatusNotifierItem");
        let _ = peer.call(register);
        while let Some(call) = peer.next_call() {
            if call.interface().as_deref() == Some("org.kde.StatusNotifierItem") && call.member().as_deref() == Some("Activate") {
                std::thread::sleep(delay);
                let reply =
                    dbus::Message::error(&call, &"org.freedesktop.DBus.Error.UnknownMethod".into(), &std::ffi::CString::new("no").unwrap());
                let _ = peer.channel.send(reply);
                peer.channel.flush();
                continue;
            }
            if call.interface().as_deref() == Some("com.canonical.dbusmenu") {
                let reply = if call.member().as_deref() == Some("AboutToShow") {
                    call.method_return().append1(false)
                } else {
                    // GetLayout: revision 1, the root (its children-display) with one entry, "Item".
                    type Node = (i32, PropMap, Vec<Variant<Box<dyn RefArg>>>);
                    let prop = |k: &str, v: &str| PropMap::from([(k.to_string(), Variant(Box::new(v.to_string()) as Box<dyn RefArg>))]);
                    let entry: Node = (1, prop("label", "Item"), Vec::new());
                    let root: Node = (0, prop("children-display", "submenu"), vec![Variant(Box::new(entry) as Box<dyn RefArg>)]);
                    call.method_return().append2(1u32, root)
                };
                let _ = peer.channel.send(reply);
                peer.channel.flush();
                continue;
            }
            if call.interface().as_deref() != Some("org.freedesktop.DBus.Properties") {
                continue;
            }
            let property: String = call.read2::<&str, &str>().map(|(_, p)| p.to_string()).unwrap_or_default();
            let reply = match property.as_str() {
                "Id" => call.method_return().append1(dbus::arg::Variant("slow")),
                "Menu" => call.method_return().append1(dbus::arg::Variant(dbus::Path::from("/Menu"))),
                _ => dbus::Message::error(
                    &call,
                    &"org.freedesktop.DBus.Error.UnknownProperty".into(),
                    &std::ffi::CString::new("no").unwrap(),
                ),
            };
            let _ = peer.channel.send(reply);
            peer.channel.flush();
        }
    });
}

/// Fix round 2026-10-06, requirement 7: a press on the bar that hits no icon drops `last_icon` —
/// a failed Activate answered late, for the icon pressed earlier, must open nothing once the user
/// has since clicked elsewhere.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_late_failed_activate_after_clicking_elsewhere_opens_nothing() {
    let bus = PrivateBus::start("menu-elsewhere");
    let sway = Sway::start("menu-elsewhere", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"tray\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    slow_activate_item(&bus, Duration::from_millis(1500));
    std::thread::sleep(Duration::from_secs(1));
    sway.click(12, 10, "button1"); // left click: Activate sent, answered late
    std::thread::sleep(Duration::from_millis(200));
    sway.click(100, 10, "button1"); // elsewhere on the bar, before the error reply lands
    std::thread::sleep(Duration::from_millis(1800)); // the delayed error reply has now landed
    assert_eq!(d.count("menu shown"), 0, "clicked elsewhere before the reply landed: {:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_silent_menu_never_opens_and_leaves_nothing_pending() {
    let bus = PrivateBus::start("menu-silent");
    let sway = Sway::start("menu-silent", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"tray\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    hushed_item(&bus, None);
    std::thread::sleep(Duration::from_secs(1));
    sway.click(12, 10, "button3");
    assert!(d.wait_for("tray: menu not shown (timeout)", Duration::from_secs(4)), "{:?}", d.log());
    assert_eq!(d.count("menu shown"), 0, "{:?}", d.log());
    // Nothing pending: the deadline timer is gone, the loop sleeps (away from a clock tick).
    while alpheus_demon::clock::LocalTime::now().0.second > 50 {
        std::thread::sleep(Duration::from_millis(200));
    }
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(d.main_thread_wakeups().saturating_sub(before), 0, "{:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_menu_closes_when_its_app_leaves() {
    let (_bus, sway, _hypr, d, item) = menu_bar("menu-app-leaves");
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    item.kill();
    assert!(d.wait_for("popup closed (item gone)", DELAY), "{:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_menu_closes_when_its_output_goes() {
    let (_bus, sway, _hypr, mut d, _item) = menu_bar("menu-output");
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    sway.command("output HEADLESS-1 disable");
    assert!(d.wait_for("popup closed (output gone)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bar removed from HEADLESS-1", DELAY), "{:?}", d.log());
    let popup = d.log().iter().position(|l| l.contains("popup closed (output gone)"));
    let bar = d.log().iter().position(|l| l.contains("bar removed from HEADLESS-1"));
    assert!(popup < bar, "the popup goes before its parent: {:?}", d.log());
    sway.command("output HEADLESS-1 enable");
    assert!(d.wait_for_count("bar placed on HEADLESS-1", 2, DELAY), "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(500)), None, "no protocol error: the daemon lives: {:?}", d.log());
}

#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_launcher_and_a_menu_are_never_open_together() {
    let (_bus, sway, _hypr, d, _item) = menu_bar("menu-launcher");
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    // The launcher opens (its socket, as Mod+Space would): the menu goes first.
    common::launcher(&sway, &alpheus_commun::launcher_ipc::Request::Toggle);
    assert!(d.wait_for("popup closed (launcher)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher shown", DELAY), "{:?}", d.log());
    // A right click on the icon now: the launcher closes (click outside) before the menu opens.
    sway.click(12, 10, "button3");
    assert!(d.wait_for("launcher closed (click outside)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("menu shown", 2, DELAY), "{:?}", d.log());
    let closed = d.log().iter().position(|l| l.contains("launcher closed (click outside)"));
    let shown = d.log().iter().rposition(|l| l.contains("menu shown"));
    assert!(closed < shown, "never both: {:?}", d.log());
}

/// Mae (2026-10-05): a second right click on the icon closes its menu; it does not reopen it.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_second_right_click_closes_the_menu() {
    let (_bus, sway, _hypr, d, _item) = menu_bar("menu-toggle");
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300)); // the popup configured and mapped
    sway.click(12, 10, "button3");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("tray: menu session closed", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("menu shown"), 1, "closed, not reopened: {:?}", d.log());
    sway.click(12, 10, "button3");
    assert!(d.wait_for_count("menu shown", 2, DELAY), "a third right click opens it again: {:?}", d.log());
}

/// Task 17 fix-round review: a press that itself closes the menu leaves no "closed recently"
/// record (only a compositor `done` arriving ahead of the press does) — a quick right click right
/// after closing reopens the menu instead of being swallowed.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_quick_right_click_after_closing_reopens_the_menu() {
    let (_bus, sway, _hypr, d, _item) = menu_bar("menu-reopen");
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300)); // the popup configured and mapped
    sway.click(12, 10, "button3");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    sway.click(12, 10, "button3"); // within the 300 ms window
    assert!(d.wait_for_count("menu shown", 2, DELAY), "the quick right click reopens it: {:?}", d.log());
}

/// Mae's "at most 2 s" from the click: every call answered within its own 2 s, but the layout lands
/// 3 s after the request — the menu is not shown.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_menu_answering_after_2_s_is_not_shown() {
    let bus = PrivateBus::start("menu-late");
    let sway = Sway::start("menu-late", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"tray\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    hushed_item(&bus, Some(Duration::from_millis(1500)));
    std::thread::sleep(Duration::from_secs(1));
    sway.click(12, 10, "button3");
    assert!(d.wait_for("tray: menu not shown (timeout)", Duration::from_secs(5)), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("menu shown"), 0, "{:?}", d.log());
    assert_eq!(d.count("popup closed"), 0, "no popup ever opened: {:?}", d.log());
}

/// Review requirement 2: the session bus dying while a menu is shown must close that popup (never
/// leave the bar stuck `Exclusive`, holding Mae's keyboard for good).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_dead_session_bus_closes_an_open_menu() {
    let (mut bus, sway, _hypr, d, _item) = menu_bar("menu-bus-dies");
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    bus.kill();
    assert!(d.wait_for("popup closed (item gone)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("tray: the session bus closed", DELAY), "{:?}", d.log());
}

/// Review requirement 7: the item's Menu path changing under an open menu (a duplicate
/// registration re-reading it, router.rs) closes it just like the item leaving. A duplicate
/// registration only re-reads Menu for a unique-name item (`Spec::qt()`, like
/// `a_duplicate_registration_closes_the_open_menu_if_its_path_changed` at the router level): a
/// well-known-name re-registration (`Spec::kde`, `menu_bar`'s item) is already known by its service
/// name and `register()` is then a no-op (host.rs), so this needs its own Qt-like item.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_menu_closes_when_its_path_changes_under_it() {
    let bus = PrivateBus::start("menu-path-changed");
    let sway = Sway::start("menu-path-changed", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"tray\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let item = FakeItem::start(&bus, Spec::qt());
    std::thread::sleep(Duration::from_secs(1));
    sway.click(12, 10, "button3");
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    item.set("Menu", Value::Path("/OtherMenu".into()));
    item.register();
    // Fix round 2026-10-06, requirement 5: the item is still here — only its Menu moved — so this
    // must never be logged as "timeout", and the session itself still ends (and says so), even
    // though the router already dropped its own session object as part of noticing the move.
    assert!(d.wait_for("popup closed (menu moved)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("tray: menu session closed", DELAY), "{:?}", d.log());
}

/// A bar with only the clock (cells 1 to ~16 of HEADLESS-1, from x ≈ 8 px), and `extra` bar.toml lines.
fn clock_bar(name: &str, extra: &str) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = format!("modules = [\"clock\"]\n{extra}");
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(&toml), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300)); // configured and drawn
    (sway, hypr, d)
}

/// "YYYY-MM" of every "calendar shown/shows" line, in order.
fn calendar_months(d: &Daemon) -> Vec<String> {
    d.log().iter().filter_map(|l| l.split_once("calendar show").map(|(_, r)| r.rsplit(' ').next().unwrap_or("").to_string())).collect()
}

/// The month after (or before) "YYYY-MM".
fn month_step(m: &str, by: i32) -> String {
    let (y, mo) = m.split_once('-').unwrap();
    let n = y.parse::<i32>().unwrap() * 12 + mo.parse::<i32>().unwrap() - 1 + by;
    format!("{}-{:02}", n.div_euclid(12), n.rem_euclid(12) + 1)
}

#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn the_calendar_opens_on_the_clock_and_closes_on_escape() {
    let (sway, _hypr, d) = clock_bar("calendar-escape", "");
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "released at Escape's key-up: {:?}", d.log());
    assert_eq!(d.count("calendar shown"), 1, "{:?}", d.log());
}

#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn the_wheel_and_arrows_change_month() {
    let (sway, _hypr, d) = clock_bar("calendar-months", "");
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300)); // the popup configured and mapped
    sway.scroll(60, 120, "down", 1); // inside the calendar, below the clock: next month
    assert!(d.wait_for_count("calendar shows", 1, DELAY), "{:?}", d.log());
    keys(&sway, &["Right"]);
    assert!(d.wait_for_count("calendar shows", 2, DELAY), "{:?}", d.log());
    keys(&sway, &["Left", "Left"]);
    assert!(d.wait_for_count("calendar shows", 4, DELAY), "{:?}", d.log());
    sway.scroll(60, 120, "up", 1); // previous month
    assert!(d.wait_for_count("calendar shows", 5, DELAY), "{:?}", d.log());
    let m = calendar_months(&d);
    let today = m[0].clone();
    let expected: Vec<String> = [0, 1, 2, 1, 0, -1].iter().map(|by| month_step(&today, *by)).collect();
    assert_eq!(m, expected, "{:?}", d.log());
    assert!(d.log().iter().all(|l| !l.contains("popup closed")), "still open: {:?}", d.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn a_second_click_closes_it() {
    let (sway, _hypr, d) = clock_bar("calendar-twice", "");
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    sway.click(40, 10, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("calendar shown"), 1, "closed, not reopened: {:?}", d.log());
    sway.click(40, 10, "button1");
    assert!(d.wait_for_count("calendar shown", 2, DELAY), "a third click opens it again: {:?}", d.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn calendar_false_opens_nothing() {
    let (sway, _hypr, d) = clock_bar("calendar-off", "[clock]\ncalendar = false\n");
    sway.click(40, 10, "button1");
    std::thread::sleep(Duration::from_secs(1));
    // The default mode is auto: with no session bus, its one line names the temporary directory
    // ("calendar-off"), which is not the calendar.
    let log = d.log();
    assert!(log.iter().filter(|l| !l.contains("appearance:")).all(|l| !l.contains("calendar") && !l.contains("popup")), "{log:?}");
}

/// Requirement 3 of the review: a click elsewhere on the bar closes the calendar (an outside
/// click), and the loop stays at rest afterwards — nothing the calendar armed is left.
#[test]
#[ignore = "headless sway (nix develop)"]
fn a_click_elsewhere_closes_it_and_leaves_no_timer() {
    let (sway, _hypr, d) = clock_bar("calendar-outside", "");
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    sway.click(640, 10, "button1"); // the bar, far from the clock
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    sway.hover(640, 400);
    while alpheus_demon::clock::LocalTime::now().0.second > 50 {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(300));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    assert_eq!(delta, 0, "the loop woke {delta} times at rest after the calendar: {:?}", d.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn an_output_going_away_closes_the_calendar() {
    let sway = Sway::start("calendar-gone", 2);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let mut d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some("modules = [\"clock\"]\n"), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-2", DELAY), "{:?}", d.log());
    sway.command("output HEADLESS-1 pos 0 0"); // pinned: the click below lands on HEADLESS-1
    sway.command("output HEADLESS-2 pos 3000 0");
    std::thread::sleep(Duration::from_millis(300));
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    sway.command("output HEADLESS-1 disable");
    assert!(d.wait_for("popup closed (output gone)", DELAY), "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(300)), None, "{:?}", d.log());
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn opening_the_launcher_closes_the_calendar() {
    let (sway, _hypr, d) = clock_bar("calendar-launcher", "");
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for("popup closed (launcher)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher shown", DELAY), "{:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_tray_menu_replaces_the_calendar() {
    let (_bus, sway, _hypr, d, _item) = menu_bar("calendar-menu");
    sway.click(1260, 10, "button1"); // the clock, at the right end
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    sway.click(12, 10, "button3"); // the tray icon's menu
    assert!(d.wait_for("menu shown", DELAY), "{:?}", d.log());
    assert!(d.log().iter().any(|l| l.contains("popup closed (outside click)")), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
}

/// The bar alone with its clock: its left edge is plain background (themes task 5).
const CLOCK_BAR: &str = "modules = [\"clock\"]\n";
/// A pixel of the bar's plain background on HEADLESS-1, far from the clock.
const BAR_BACKGROUND: (u32, u32) = (2, 2);
/// The clock of a bar with only that module sits at the left edge (as `clock_bar`'s click at x = 40).
const CLOCK_AT: (u32, u32) = (40, 10);

fn rgb(c: Rgb) -> [u8; 3] {
    [c.0, c.1, c.2]
}

fn bar_bg(theme: &str) -> [u8; 3] {
    rgb(preset(theme).unwrap().dark.bar_bg)
}

fn popup_bg(theme: &str) -> [u8; 3] {
    rgb(preset(theme).unwrap().dark.popup_bg)
}

/// A column of pixels, top to bottom.
fn column(s: &Ppm, x: u32) -> Vec<[u8; 3]> {
    (0..s.height).filter_map(|y| s.at(x, y)).collect()
}

/// A bar on catppuccin dark, painted (its background checked), its config directory returned.
fn theme_bar(name: &str) -> (Sway, FakeHyprland, Daemon, PathBuf) {
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), ..DaemonOptions::default() });
    assert!(d.wait_for("theme: catppuccin, dark", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let (x, y) = BAR_BACKGROUND;
    let mocha = bar_bg("catppuccin");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(mocha)), "never on Mantle: {:?}", d.log());
    (sway, hypr, d, config)
}

#[test]
fn a_ppm_is_read_pixel_by_pixel() {
    let ppm = Ppm::parse(b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06").unwrap();
    assert_eq!((ppm.width, ppm.height), (2, 1));
    assert_eq!((ppm.at(0, 0), ppm.at(1, 0), ppm.at(2, 0)), (Some([1, 2, 3]), Some([4, 5, 6]), None));
    assert!(Ppm::parse(b"P5\n2 1\n255\n\x01\x02").is_none(), "only binary RGB");
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn writing_theme_toml_repaints_the_bar_in_the_new_bar_bg() {
    let (sway, _hypr, mut d, config) = theme_bar("theme-reload");
    let (x, y) = BAR_BACKGROUND;
    // A rename into place (IN_MOVED_TO).
    write_atomically(&config, "theme.toml", "theme = \"tokyo-night\"\nmode = \"dark\"\n");
    assert!(d.wait_for("theme reloaded: tokyo-night, dark", DELAY), "{:?}", d.log());
    let night = bar_bg("tokyo-night");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(night)), "{:?}", d.log());
    // A colour file the current theme does not read: same roles, no line, no repaint.
    let legacy = "bg = \"#102030\"\nfg = \"#e0e0e0\"\naccent = \"#ffaa00\"\nmuted = \"#808080\"\nsurface = \"#203040\"\nerror = \"#ff5555\"\nok = \"#55ff55\"\n";
    write_atomically(&config, "colors.toml", legacy);
    // A plain write (IN_CLOSE_WRITE).
    std::fs::write(config.join("theme.toml"), "theme = \"gruvbox\"\nmode = \"dark\"\n").unwrap();
    assert!(d.wait_for("theme reloaded: gruvbox, dark", DELAY), "{:?}", d.log());
    let gruvbox = bar_bg("gruvbox");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(gruvbox)), "{:?}", d.log());
    // theme.toml removed (IN_DELETE): no theme key and a colors.toml, so the wallpaper (spec § 2 as amended).
    std::fs::remove_file(config.join("theme.toml")).unwrap();
    assert!(d.wait_for("theme reloaded: wallpaper, dark", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some([0x10, 0x20, 0x30])), "{:?}", d.log());
    assert_eq!(d.count("theme reloaded:"), 3, "colors.toml under a preset changed nothing: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// Shapes task 3, end to end: a style set at startup reaches the painter, and the opaque region the
/// bar now sends is accepted (a bad one would end the client). Under underline the bar's last row is
/// the border line; above it, the margin stays on bar_bg.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn a_style_set_at_startup_is_painted() {
    let sway = Sway::start("shapes-underline", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"underline\"\n").unwrap();
    let mut d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let mocha = preset("catppuccin").unwrap().dark;
    let (line, bg) = (rgb(mocha.border_over(mocha.bar_bg)), rgb(mocha.bar_bg));
    let (x, y) = BAR_BACKGROUND;
    let bottom = 22 - 1; // bar.toml's default height, scale 1
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, bottom) == Some(line) && s.at(x, y) == Some(bg)), "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn a_module_colours_change_alone_repaints_the_bar() {
    let (_sway, _hypr, mut d, config) = theme_bar("theme-module-colors");
    // Same theme and mode, so the roles are equal: only the setting differs (themes spec § 9).
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nmodule_colors = \"full\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, module colours full", DELAY), "{:?}", d.log());
    // The same text again: nothing changed, nothing repainted.
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nmodule_colors = \"full\"\n");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("theme reloaded:"), 1, "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn a_burst_of_writes_repaints_once() {
    let (sway, _hypr, d, config) = theme_bar("theme-burst");
    for _ in 0..20 {
        write_atomically(&config, "theme.toml", "theme = \"nord\"\nmode = \"dark\"\n");
    }
    assert!(d.wait_for("theme reloaded: nord, dark", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(500));
    // The same text once more, well after the burst: the roles do not change, nothing is repainted.
    write_atomically(&config, "theme.toml", "theme = \"nord\"\nmode = \"dark\"\n");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("theme reloaded:"), 1, "one repaint for 21 writes: {:?}", d.log());
    let (x, y) = BAR_BACKGROUND;
    let nord = bar_bg("nord");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(nord)), "{:?}", d.log());
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn an_open_launcher_box_is_repainted_too() {
    let (sway, _hypr, d, config) = theme_bar("theme-launcher");
    ready(&d);
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    // The box is centred: the output's middle column crosses it.
    let (old, new) = (popup_bg("catppuccin"), popup_bg("gruvbox"));
    assert_ne!(old, new);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| column(s, s.width / 2).contains(&old)), "the box on Base: {:?}", d.log());
    write_atomically(&config, "theme.toml", "theme = \"gruvbox\"\nmode = \"dark\"\n");
    assert!(d.wait_for("theme reloaded: gruvbox, dark", DELAY), "{:?}", d.log());
    let repainted = |s: &Ppm| {
        let c = column(s, s.width / 2);
        c.contains(&new) && !c.contains(&old)
    };
    assert!(sway.wait_for_screen("HEADLESS-1", repainted), "the open box repainted on gruvbox's popup_bg: {:?}", d.log());
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn an_open_calendar_is_repainted_too() {
    let (sway, _hypr, d, config) = theme_bar("theme-calendar");
    sway.click(CLOCK_AT.0, CLOCK_AT.1, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    let (old, new) = (popup_bg("catppuccin"), popup_bg("gruvbox"));
    assert_ne!(old, new);
    let x = CLOCK_AT.0;
    assert!(sway.wait_for_screen("HEADLESS-1", |s| column(s, x).contains(&old)), "the calendar on Base: {:?}", d.log());
    write_atomically(&config, "theme.toml", "theme = \"gruvbox\"\nmode = \"dark\"\n");
    assert!(d.wait_for("theme reloaded: gruvbox, dark", DELAY), "{:?}", d.log());
    let repainted = |s: &Ppm| {
        let c = column(s, x);
        c.contains(&new) && !c.contains(&old)
    };
    assert!(sway.wait_for_screen("HEADLESS-1", repainted), "the open calendar repainted: {:?}", d.log());
}

/// Review Focus 1 and controller reconciliation 9: matugen caught mid-write. A truncated
/// colors.toml keeps the colours on screen (one line, no flash of the default theme); the full
/// file then repaints once, in its own colours.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn a_truncated_colour_file_keeps_the_colours_until_the_full_one() {
    let (sway, _hypr, d, config) = theme_bar("theme-truncated");
    let (x, y) = BAR_BACKGROUND;
    // Legacy 7-key files: in dark mode, bar_bg is the file's bg.
    let first = "bg = \"#102030\"\nfg = \"#e0e0e0\"\naccent = \"#ffaa00\"\nmuted = \"#808080\"\nsurface = \"#203040\"\nerror = \"#ff5555\"\nok = \"#55ff55\"\n";
    let second = first.replace("#102030", "#304050");
    write_atomically(&config, "colors.toml", first);
    write_atomically(&config, "theme.toml", "theme = \"wallpaper\"\nmode = \"dark\"\n");
    assert!(d.wait_for("theme reloaded: wallpaper, dark", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some([0x10, 0x20, 0x30])), "{:?}", d.log());
    let reloads = d.count("theme reloaded:");
    // A plain write cut mid-way (`fg = ` and nothing after): what a reader can catch of matugen's in-place write.
    std::fs::write(config.join("colors.toml"), &second[..20]).unwrap();
    assert!(d.wait_for("theme: keeping the current colours until the colour file is usable", DELAY), "{:?}", d.log());
    let mocha = bar_bg("catppuccin");
    assert_ne!(mocha, [0x10, 0x20, 0x30], "the flash check bites only if the two differ");
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(500) {
        let s = sway.screenshot("HEADLESS-1").expect("grim reads the output");
        assert_eq!(s.at(x, y), Some([0x10, 0x20, 0x30]), "kept, never the default theme's {mocha:?}: {:?}", d.log());
    }
    std::fs::write(config.join("colors.toml"), &second).unwrap();
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some([0x30, 0x40, 0x50])), "{:?}", d.log());
    assert_eq!(d.count("theme reloaded:"), reloads + 1, "one repaint for the truncated-then-full pair: {:?}", d.log());
    assert_eq!(d.count("invalid colors.toml"), 1, "the bad file logged once: {:?}", d.log());
    assert_eq!(d.count("keeping the current colours"), 1, "{:?}", d.log());
}

/// The bar's middle row (22 px high by default).
const MIDDLE: u32 = 11;

/// Polls `ok` for 10 s at most.
fn wait_until(ok: impl Fn() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < DELAY {
        if ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Shapes task 7: a bar on catppuccin dark holding only the clock, at the left edge. Also returns
/// what the compositor draws where the bar's right end will be, read BEFORE the bar exists: what a
/// transparent pixel there must show (grim drops alpha, and there is no swaybg in these tests).
fn shape_bar(name: &str) -> (Sway, FakeHyprland, Daemon, PathBuf, [u8; 3]) {
    let sway = Sway::start(name, 1);
    let t = Instant::now();
    let behind = loop {
        if let Some(p) = sway.screenshot("HEADLESS-1").and_then(|s| s.at(s.width - 20, MIDDLE)) {
            break p;
        }
        assert!(t.elapsed() < DELAY, "grim never read the empty output");
        thread::sleep(Duration::from_millis(50));
    };
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let mocha = bar_bg("catppuccin");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(s.width - 20, MIDDLE) == Some(mocha)), "flat, end to end: {:?}", d.log());
    assert_ne!(behind, mocha, "the checks bite only if the compositor's background differs from bar_bg");
    (sway, hypr, d, config, behind)
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn islands_gaps_show_what_is_behind_the_bar_and_translucent_blends_it() {
    let (sway, _hypr, d, config, behind) = shape_bar("shape-islands");
    let mocha = bar_bg("catppuccin");
    let pill = rgb(preset("catppuccin").unwrap().dark.pill_bg);
    assert_ne!(behind, pill, "the block check bites only if the background differs from pill_bg");
    let end = |s: &Ppm| s.at(s.width - 20, MIDDLE);
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"islands\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style islands", DELAY), "{:?}", d.log());
    // The empty right end is a gap: transparent. The clock's block is drawn on pill_bg: a bar that
    // drew nothing at all would show the background at the end too.
    let islands = |s: &Ppm| end(s) == Some(behind) && (0..s.width / 2).any(|x| s.at(x, MIDDLE) == Some(pill));
    assert!(sway.wait_for_screen("HEADLESS-1", islands), "{:?}", d.log());
    // Translucent at 0.5: bar_bg premultiplied by α = 128/255, over what is behind.
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"translucent\"\nopacity = 0.5\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style translucent, opacity 0.5", DELAY), "{:?}", d.log());
    let blend = |c: usize| {
        let a = 128.0 / 255.0;
        f32::from(mocha[c]) * a + f32::from(behind[c]) * (1.0 - a)
    };
    assert!((0..3).any(|c| (blend(c) - f32::from(mocha[c])).abs() > 2.0), "the blend check bites only if it differs from opaque");
    let blended = |s: &Ppm| end(s).is_some_and(|p| (0..3).all(|c| (f32::from(p[c]) - blend(c)).abs() <= 2.0));
    assert!(sway.wait_for_screen("HEADLESS-1", blended), "{:?}", d.log());
    // Back to flat: opaque again, end to end.
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"flat\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style flat", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| end(s) == Some(mocha)), "{:?}", d.log());
    assert_eq!(d.count("theme reloaded:"), 3, "one reload per write: {:?}", d.log());
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn floating_moves_the_bar_down_by_its_margin_and_back() {
    let (sway, _hypr, mut d, config, behind) = shape_bar("shape-floating");
    let mocha = bar_bg("catppuccin");
    // The middle column crosses the bar's empty middle (the clock sits at the left edge).
    let first_row = |s: &Ppm| (0..s.height).find(|y| s.at(s.width / 2, *y) == Some(mocha));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| first_row(s) == Some(0)), "flat: from the top edge: {:?}", d.log());
    assert!(wait_until(|| sway.workspace_y() == Some(22)), "flat reserves the bar's height: {:?}", sway.workspace_y());
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"floating\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style floating", DELAY), "{:?}", d.log());
    // 6 px lower, the margin above showing what is behind, the top-left corner rounded off. A bar
    // that never repainted after its new configure would still show its old flat buffer at (6, 6).
    let floating = |s: &Ppm| first_row(s) == Some(6) && (0..6).all(|y| s.at(s.width / 2, y) == Some(behind)) && s.at(6, 6) == Some(behind);
    assert!(sway.wait_for_screen("HEADLESS-1", floating), "{:?}", d.log());
    // Reconciliation 5: the exclusive zone is the bar height and the compositor adds the 6 px
    // top margin, so tiled windows start at 6 + 22, just under the floating bar.
    assert!(wait_until(|| sway.workspace_y() == Some(28)), "floating: windows start at 6 + 22: {:?}", sway.workspace_y());
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"flat\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style flat", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| first_row(s) == Some(0) && s.at(6, 6) == Some(mocha)), "{:?}", d.log());
    assert!(wait_until(|| sway.workspace_y() == Some(22)), "back to 22: {:?}", sway.workspace_y());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// Review Focus 1: the calendar hangs from the bar's surface; a switch to floating moves and resizes
/// that surface, so the calendar is closed first, and opens again on the moved bar.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn a_switch_to_floating_closes_a_popup_hanging_from_the_bar() {
    let (sway, _hypr, mut d, config, _behind) = shape_bar("shape-popup");
    let (x, y) = CLOCK_AT;
    sway.click(x, y, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"floating\"\n");
    assert!(d.wait_for("popup closed (restyled)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style floating", DELAY), "{:?}", d.log());
    // The reload's line names the change, then the restyle closes the popup, before the bar moves.
    let log = d.log();
    let reload = log.iter().position(|l| l.contains("theme reloaded:"));
    let popup = log.iter().position(|l| l.contains("popup closed (restyled)"));
    assert!(matches!((reload, popup), (Some(r), Some(p)) if r < p), "the reload line, then the close: {log:?}");
    // The moved bar, painted at its new place (its first row 6 px down): the clock is 6 px further
    // right and down.
    let mocha = bar_bg("catppuccin");
    let first_row = |s: &Ppm| (0..s.height).find(|y| s.at(s.width / 2, *y) == Some(mocha));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| first_row(s) == Some(6)), "{:?}", d.log());
    sway.click(x + 6, y + 6, "button1");
    assert!(d.wait_for_count("calendar shown", 2, DELAY), "the calendar opens again on the moved bar: {:?}", d.log());
    // A style switch that keeps the placement leaves an open popup alone.
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"floating\"\nmodule_colors = \"full\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, module colours full", DELAY), "{:?}", d.log());
    assert_eq!(d.count("popup closed"), 1, "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// Review Focus 5: an invalid style is one warning and `flat`, with one repaint; reading the same
/// file again repeats neither the warning nor the reload.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn an_invalid_style_warns_once_and_falls_back_to_flat() {
    let (sway, _hypr, mut d, config, behind) = shape_bar("shape-invalid");
    let mocha = bar_bg("catppuccin");
    let end = |s: &Ppm| s.at(s.width - 20, MIDDLE);
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"islands\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style islands", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| end(s) == Some(behind)), "{:?}", d.log());
    let bad = "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"round\"\n";
    write_atomically(&config, "theme.toml", bad);
    assert!(d.wait_for("style must be one of", DELAY), "{:?}", d.log());
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style flat", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| end(s) == Some(mocha)), "flat again: {:?}", d.log());
    // The same file again, then with a comment: two more reads, nothing new to say.
    write_atomically(&config, "theme.toml", bad);
    write_atomically(&config, "theme.toml", &format!("# still round\n{bad}"));
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("style must be one of"), 1, "the warning is not repeated: {:?}", d.log());
    assert_eq!(d.count("theme reloaded:"), 2, "islands, then flat once: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// The mode file the TUI reads (themes task 6), or None.
fn mode_file(sway: &Sway) -> Option<String> {
    std::fs::read_to_string(sway.runtime.join("alpheus/mode")).ok()
}

/// Polls the mode file until it holds `text`, 10 s at most.
fn wait_for_mode_file(sway: &Sway, text: &str) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if mode_file(sway).as_deref() == Some(text) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn mode_auto_follows_the_portal_without_the_tray() {
    let bus = PrivateBus::start("appearance-auto");
    let portal = FakePortal::start(&bus, 1);
    let sway = Sway::start("appearance-auto", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    // No theme.toml: mode "auto" is the default.
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("appearance: the portal says dark", DELAY), "{:?}", d.log());
    portal.set(2);
    assert!(d.wait_for("appearance: the portal says light", DELAY), "{:?}", d.log());
    assert_eq!(d.count("appearance:"), 2, "one line per change: {:?}", d.log());
    assert_eq!(d.count("tray:"), 0, "the tray module is off: {:?}", d.log());
    assert_eq!(portal.calls().len(), 1, "one ReadOne: {:?}", portal.calls());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_fixed_mode_without_the_tray_never_opens_the_session_bus() {
    let bus = PrivateBus::start("appearance-fixed");
    let portal = FakePortal::start(&bus, 2);
    let sway = Sway::start("appearance-fixed", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "mode = \"dark\"\n").unwrap();
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(portal.calls(), Vec::<String>::new(), "never asked");
    assert_eq!(d.count("appearance:"), 0, "{:?}", d.log());
}

/// The bar turns light (pixel-checked) and the mode file follows. The switch from mode = "dark" to
/// "auto" in theme.toml starts the portal at once: `follow_portal_if_auto` runs after the new
/// settings are stored (controller reconciliation 5).
#[test]
#[ignore = "headless sway, grim and dbus-daemon (nix develop)"]
fn switching_to_auto_turns_the_bar_light_and_the_mode_file_follows() {
    let bus = PrivateBus::start("appearance-light");
    let portal = FakePortal::start(&bus, 2);
    let sway = Sway::start("appearance-light", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let (x, y) = BAR_BACKGROUND;
    let (dark, light) = (bar_bg("catppuccin"), rgb(preset("catppuccin").unwrap().light.unwrap().bar_bg));
    assert_ne!(dark, light, "the pixel check bites only if the two differ");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(dark)), "{:?}", d.log());
    assert!(wait_for_mode_file(&sway, "dark\n"), "{:?}", mode_file(&sway));
    assert_eq!(portal.calls().len(), 0, "a fixed mode never asks");
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"auto\"\n");
    assert!(d.wait_for("appearance: the portal says light", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(light)), "the bar on Latte's bar_bg: {:?}", d.log());
    assert!(wait_for_mode_file(&sway, "light\n"), "{:?}", mode_file(&sway));
    let perms = std::fs::metadata(sway.runtime.join("alpheus")).unwrap().permissions().mode() & 0o777;
    assert_eq!(perms, 0o700, "owner-only");
}

/// A legacy wallpaper colour file whose `bg` (the bar's background) is `bg`.
fn wallpaper(bg: &str) -> String {
    format!(
        "bg = \"{bg}\"\nfg = \"#e0e0e0\"\naccent = \"#ffaa00\"\nmuted = \"#808080\"\nsurface = \"#203040\"\nerror = \"#ff5555\"\nok = \"#55ff55\"\n"
    )
}

/// An empty theme.toml left on purpose means the defaults: at startup with no line about it, and on
/// a reload the colour files are still read, so matugen's next wallpaper repaints the bar.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn an_empty_theme_toml_repaints_on_a_new_wallpaper() {
    let sway = Sway::start("theme-blank-wallpaper", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "").unwrap();
    std::fs::write(config.join("colors.toml"), wallpaper("#102030")).unwrap();
    let mut d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let (x, y) = BAR_BACKGROUND;
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some([0x10, 0x20, 0x30])), "{:?}", d.log());
    write_atomically(&config, "colors.toml", &wallpaper("#405060"));
    assert!(d.wait_for("theme reloaded: wallpaper, dark", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some([0x40, 0x50, 0x60])), "{:?}", d.log());
    assert_eq!(d.count("theme.toml is empty"), 0, "an empty file is the defaults, no fault: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// An editor truncating theme.toml mid-save: the islands bar stays islands (never flat and back),
/// one line says so, and a new wallpaper written meanwhile still repaints it.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn a_truncated_theme_toml_keeps_the_style_and_still_reads_a_new_wallpaper() {
    let (sway, _hypr, mut d, config, behind) = shape_bar("theme-truncated");
    let end = |s: &Ppm| s.at(s.width - 20, MIDDLE);
    std::fs::write(config.join("colors.toml"), wallpaper("#102030")).unwrap();
    write_atomically(&config, "theme.toml", "theme = \"wallpaper\"\nmode = \"dark\"\nstyle = \"islands\"\n");
    assert!(d.wait_for("theme reloaded: wallpaper, dark, style islands", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| end(s) == Some(behind)), "{:?}", d.log());
    std::fs::write(config.join("theme.toml"), "").unwrap();
    assert!(d.wait_for("theme.toml is empty", DELAY), "{:?}", d.log());
    write_atomically(&config, "colors.toml", &wallpaper("#405060"));
    assert!(d.wait_for_count("theme reloaded:", 2, DELAY), "{:?}", d.log());
    assert!(d.log().iter().any(|l| l.ends_with("theme reloaded: wallpaper, dark")), "the colours alone: {:?}", d.log());
    thread::sleep(Duration::from_millis(300));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| end(s) == Some(behind)), "still islands: {:?}", d.log());
    assert_eq!(d.count("theme.toml is empty"), 1, "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// An empty theme.toml is mode auto: the portal's switch to light repaints the bar and rewrites the
/// mode file the TUI reads.
#[test]
#[ignore = "headless sway, grim and dbus-daemon (nix develop)"]
fn an_empty_theme_toml_follows_the_portal_and_the_mode_file_too() {
    let bus = PrivateBus::start("appearance-blank");
    let portal = FakePortal::start(&bus, 1);
    let sway = Sway::start("appearance-blank", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "").unwrap();
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("appearance: the portal says dark", DELAY), "{:?}", d.log());
    let (x, y) = BAR_BACKGROUND;
    let (dark, light) = (bar_bg("catppuccin"), rgb(preset("catppuccin").unwrap().light.unwrap().bar_bg));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(dark)), "{:?}", d.log());
    assert!(wait_for_mode_file(&sway, "dark\n"), "{:?}", mode_file(&sway));
    portal.set(2);
    assert!(d.wait_for("appearance: the portal says light", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(light)), "{:?}", d.log());
    assert!(wait_for_mode_file(&sway, "light\n"), "{:?}", mode_file(&sway));
    assert_eq!(d.count("theme.toml is empty"), 0, "{:?}", d.log());
}

/// A portal answering late never blocks the bar. A dark first frame (and a dark mode file, written
/// at startup), then light when the answer comes after 1 s.
#[test]
#[ignore = "headless sway, grim and dbus-daemon (nix develop)"]
fn a_late_portal_paints_dark_first_then_light() {
    let bus = PrivateBus::start("appearance-late");
    let _portal = FakePortal::start_late(&bus, 2, Duration::from_secs(1));
    let sway = Sway::start("appearance-late", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    // No theme.toml: catppuccin, mode "auto".
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(CLOCK_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let (x, y) = BAR_BACKGROUND;
    let (dark, light) = (bar_bg("catppuccin"), rgb(preset("catppuccin").unwrap().light.unwrap().bar_bg));
    assert!(wait_for_mode_file(&sway, "dark\n"), "written at startup, before the answer: {:?}", mode_file(&sway));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(dark)), "{:?}", d.log());
    assert_eq!(d.count("appearance: the portal says light"), 0, "the dark frame came before the answer: {:?}", d.log());
    assert!(d.wait_for("appearance: the portal says light", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| s.at(x, y) == Some(light)), "{:?}", d.log());
    assert!(wait_for_mode_file(&sway, "light\n"), "{:?}", mode_file(&sway));
    assert_eq!(d.count("appearance: no portal"), 0, "1 s is within the 2 s deadline: {:?}", d.log());
}

/// How many connections `pid` holds on `bus`, as the bus driver sees them (each unique name's process).
fn connections_of(bus: &PrivateBus, pid: u32) -> usize {
    let peer = Peer::connect(bus);
    let call = |member: &str| {
        dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", member).unwrap()
    };
    let names: Vec<String> = peer.call(call("ListNames")).unwrap().read1().unwrap();
    names
        .iter()
        .filter(|n| n.starts_with(':'))
        .filter(|n| {
            // `String::as_str`, not `RefArg::as_str` (in scope here), which gives an Option.
            let m = call("GetConnectionUnixProcessID").append1(String::as_str(n));
            peer.call(m).ok().and_then(|r| r.read1::<u32>().ok()) == Some(pid)
        })
        .count()
}

/// The media module at the left of the bar (`modules`), on a private session bus, with one fake
/// player playing "Harbor Lights" (fictional) started before the daemon; `theme` written to
/// theme.toml when given.
fn media_daemon(name: &str, modules: &str, theme: Option<&str>) -> (PrivateBus, Sway, FakeHyprland, Daemon, FakePlayer) {
    let (bus, sway, hypr, d, player, ()) = media_daemon_with(name, modules, theme, |_| ());
    (bus, sway, hypr, d, player)
}

/// `media_daemon`, with `before` run on the bus once the player is up and before the daemon starts
/// (a peer the daemon must find there, like the portal); its result is handed back last.
fn media_daemon_with<T>(
    name: &str,
    modules: &str,
    theme: Option<&str>,
    before: impl FnOnce(&PrivateBus) -> T,
) -> (PrivateBus, Sway, FakeHyprland, Daemon, FakePlayer, T) {
    let bus = PrivateBus::start(name);
    let player = FakePlayer::start(&bus, PlayerSpec::playing("tidewave", "Harbor Lights"));
    let extra = before(&bus);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    if let Some(text) = theme {
        let config = sway.runtime.join("config/alpheus");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("theme.toml"), text).unwrap();
    }
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(modules), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    (bus, sway, hypr, d, player, extra)
}

/// The pixels of the media module at the bar's left edge ("▶ Harbor Lights", cells 1 to 16 at
/// ≈ 8.4 px), the clock's cells left out.
fn media_cells(s: &Ppm) -> Vec<[u8; 3]> {
    (2..20).flat_map(|y| (8..130).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect()
}

const MEDIA_LINK_BAR: &str = "modules = [\"media\", \"clock\"]\n";
const MEDIA_TRAY_BAR: &str = "modules = [\"media\", \"tray\", \"clock\"]\n";
const GET_ALL: &str = "org.freedesktop.DBus.Properties.GetAll(org.mpris.MediaPlayer2.Player)";
const IDENTITY: &str = "org.freedesktop.DBus.Properties.Get(org.mpris.MediaPlayer2, Identity)";

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn with_the_tray_the_media_module_adds_no_connection() {
    let (bus, _sway, _hypr, d, player) = media_daemon("media-on-tray", MEDIA_TRAY_BAR, None);
    assert!(d.wait_for("media: on the tray's session bus", DELAY), "{:?}", d.log());
    assert!(d.wait_for("media: shown", DELAY), "{:?}", d.log());
    assert_eq!(connections_of(&bus, d.pid()), 1, "the tray's connection carries it: {:?}", d.log());
    assert_eq!(player.calls(), [GET_ALL, IDENTITY]);
    assert_eq!(d.count("media: on the bar's session bus"), 0, "{:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn without_the_tray_in_mode_auto_one_connection_serves_the_portal_and_the_players() {
    // The portal is up before the daemon: its ReadOne is answered on the media module's link.
    let (bus, _sway, _hypr, d, player, portal) = media_daemon_with("media-auto", MEDIA_LINK_BAR, None, |bus| FakePortal::start(bus, 2));
    assert!(d.wait_for("media: on the bar's session bus", DELAY), "{:?}", d.log());
    assert!(d.wait_for("media: shown", DELAY), "{:?}", d.log());
    assert!(d.wait_for("appearance: the portal says light", DELAY), "the portal was followed on the same link: {:?}", d.log());
    assert_eq!(connections_of(&bus, d.pid()), 1, "never two: {:?}", d.log());
    assert_eq!(player.calls(), [GET_ALL, IDENTITY]);
    assert_eq!(portal.calls().len(), 1, "{:?}", portal.calls());
    assert_eq!(d.count("tray:"), 0, "the tray module is off: {:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn without_the_tray_in_a_fixed_mode_the_connection_is_for_the_players_alone() {
    let (bus, _sway, _hypr, d, _player) = media_daemon("media-dark", MEDIA_LINK_BAR, Some("mode = \"dark\"\n"));
    let portal = FakePortal::start(&bus, 2);
    assert!(d.wait_for("media: shown", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(connections_of(&bus, d.pid()), 1, "{:?}", d.log());
    assert_eq!(portal.calls(), Vec::<String>::new(), "a fixed mode never asks");
    assert_eq!(d.count("appearance:"), 0, "{:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn switching_to_auto_joins_the_media_connection() {
    let (bus, sway, _hypr, d, _player) = media_daemon("media-join", MEDIA_LINK_BAR, Some("mode = \"dark\"\n"));
    let portal = FakePortal::start(&bus, 2);
    assert!(d.wait_for("media: shown", DELAY), "{:?}", d.log());
    write_atomically(&sway.runtime.join("config/alpheus"), "theme.toml", "mode = \"auto\"\n");
    assert!(d.wait_for("appearance: the portal says light", DELAY), "{:?}", d.log());
    assert_eq!(connections_of(&bus, d.pid()), 1, "joined, never a second connection: {:?}", d.log());
    assert_eq!(portal.calls().len(), 1);
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn without_the_media_module_nothing_touches_the_players() {
    let (bus, _sway, _hypr, d, player) = media_daemon("media-absent", "modules = [\"clock\"]\n", Some("mode = \"dark\"\n"));
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(connections_of(&bus, d.pid()), 0, "no connection, so no rule and no call: {:?}", d.log());
    assert_eq!(player.calls(), Vec::<String>::new());
    assert_eq!(d.count("media:"), 0, "{:?}", d.log());
}

/// Review Focus: the private session bus dying — the media module goes away, nothing spins.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_dead_session_bus_takes_the_media_module_away_and_nothing_spins() {
    let (mut bus, sway, _hypr, mut d, player) = media_daemon("media-bus-dies", MEDIA_LINK_BAR, None);
    assert!(d.wait_for("media: shown", DELAY), "{:?}", d.log());
    // Playing: the module's play glyph takes the purple (catppuccin dark, mode auto without a
    // portal); paused, the whole module is muted.
    let purple = rgb(preset("catppuccin").unwrap().dark.hues.purple);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| media_cells(s).contains(&purple)), "never drawn playing: {:?}", d.log());
    // Playing to paused: the module stays, so no second "shown" line.
    player.set("PlaybackStatus", text("Paused"));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| !media_cells(s).contains(&purple)), "never repainted paused: {:?}", d.log());
    bus.kill();
    assert!(d.wait_for("media: the session bus closed", DELAY), "{:?}", d.log());
    assert!(d.wait_for("media: hidden", DELAY), "{:?}", d.log());
    // The dead socket stays readable: its source must be gone, or the loop spins.
    std::thread::sleep(Duration::from_millis(500));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_millis(4500));
    let wakeups = d.main_thread_wakeups() - before;
    assert!(wakeups <= 4, "{wakeups} wake-ups of the loop in 4.5 s after the bus closed");
    assert_eq!(d.count("media: the session bus closed"), 1, "{:?}", d.log());
    assert_eq!((d.count("media: shown"), d.count("media: hidden")), (1, 1), "one line each way: {:?}", d.log());
    assert_eq!(d.count("media: on the bar's session bus"), 1, "never reconnected: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// Inside "▶ Harbor Lights" at the bar's left edge (cells 1 to 16, ≈ 8.4 px each): the title's
/// third cell.
const MEDIA_AT: (u32, u32) = (30, 10);

/// A middle click and one wheel notch each way on the media module reach the fake player: one
/// call per gesture, in order, and no popup.
fn clicks_reach_the_player(name: &str, modules: &str) {
    let (_bus, sway, _hypr, d, player) = media_daemon(name, modules, Some("mode = \"dark\"\n"));
    assert!(d.wait_for("media: shown", DELAY), "{:?}", d.log());
    // Drawn: the play glyph in the purple (catppuccin dark), as in the bus-dies test.
    let purple = rgb(preset("catppuccin").unwrap().dark.hues.purple);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| media_cells(s).contains(&purple)), "never drawn: {:?}", d.log());
    let (x, y) = MEDIA_AT;
    sway.click(x, y, "button2");
    assert_eq!(player.wait_call("PlayPause", DELAY).as_deref(), Some("org.mpris.MediaPlayer2.Player.PlayPause()"), "{:?}", d.log());
    sway.scroll(x, y, "up", 1);
    assert_eq!(player.wait_call("Next", DELAY).as_deref(), Some("org.mpris.MediaPlayer2.Player.Next()"), "{:?}", d.log());
    sway.scroll(x, y, "down", 1);
    assert_eq!(player.wait_call("Previous", DELAY).as_deref(), Some("org.mpris.MediaPlayer2.Player.Previous()"), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300));
    let controls: Vec<String> = player.calls().into_iter().filter(|c| c.starts_with("org.mpris.MediaPlayer2.Player.")).collect();
    assert_eq!(
        controls,
        ["org.mpris.MediaPlayer2.Player.PlayPause()", "org.mpris.MediaPlayer2.Player.Next()", "org.mpris.MediaPlayer2.Player.Previous()"],
        "one call per gesture"
    );
    assert_eq!(d.count("popup"), 0, "a middle click or the wheel opens nothing: {:?}", d.log());
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_middle_click_and_the_wheel_reach_the_player_on_the_bars_link() {
    clicks_reach_the_player("media-clicks-link", MEDIA_LINK_BAR);
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_middle_click_and_the_wheel_reach_the_player_through_the_tray() {
    clicks_reach_the_player("media-clicks-tray", MEDIA_TRAY_BAR);
}

/// The media bar on a private session bus on `outputs` outputs: the fake player "lumen" playing
/// "Paper Lanterns" by Marlowe Vane (fictional), plus one playing "Second Song" per name in `extra`; read and drawn.
fn media_bar(name: &str, outputs: u32, extra: &[&str]) -> (PrivateBus, Sway, FakeHyprland, Daemon, Vec<FakePlayer>) {
    let bus = PrivateBus::start(name);
    let sway = Sway::start(name, outputs);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml: Some(MEDIA_LINK_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() },
    );
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    let mut players = vec![FakePlayer::start(&bus, PlayerSpec::playing("lumen", "Paper Lanterns"))];
    players.extend(extra.iter().map(|n| FakePlayer::start(&bus, PlayerSpec::playing(n, "Second Song"))));
    for p in &players {
        assert!(p.wait_call("GetAll", DELAY).is_some(), "the daemon read every fake player: {:?}", d.log());
    }
    std::thread::sleep(Duration::from_millis(500)); // read, drawn
    (bus, sway, hypr, d, players)
}

/// Layout coordinates of the popup's cell (column, row) on a bar whose media zone starts at cell 1 at
/// x = `bar_x`: the popup hangs from that cell (≈ 8 px), under the 22 px bar, its frame ≈ 4 px at the
/// sides and ≈ 7 px at the top, ≈ 8.4 px cells, 22 px rows. The middle of the cell: a few px of
/// error in these estimates still lands in it (a button takes 5 cells, a row 22 px).
fn popup_cell(bar_x: u32, column: u32, row: u32) -> (u32, u32) {
    (bar_x + 8 + 4 + (column * 84 + 42) / 10, 22 + 7 + row * 22 + 11)
}

/// Waits until the minute's turn (the clock's redraw, one per bar) is neither about to come nor
/// just past: seconds 2 to 50.
fn away_from_the_minute() {
    while !(2..=50).contains(&alpheus_demon::clock::LocalTime::now().0.second) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The loop thread's wake-ups over 5 s, away from the minute's turn (the clock is the bar's only
/// other timer).
fn wakeups_over_5_s(d: &Daemon) -> u64 {
    away_from_the_minute();
    std::thread::sleep(Duration::from_millis(300));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    d.main_thread_wakeups().saturating_sub(before)
}

/// The loop's wake-ups while `player` broadcasts five Seeked over 1.5 s: none once the popup closed,
/// since the bar's Seeked match rule goes with it (the bus then delivers it nothing).
fn wakeups_over_seeked(d: &Daemon, player: &FakePlayer) -> u64 {
    away_from_the_minute();
    let before = d.main_thread_wakeups();
    for us in 1..=5 {
        player.seeked(us * 1_000_000);
        std::thread::sleep(Duration::from_millis(300));
    }
    d.main_thread_wakeups().saturating_sub(before)
}

#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_media_popup_opens_on_a_left_click_and_escape_closes_it() {
    let (_bus, sway, _hypr, d, _players) = media_bar("media-open", 1, &[]);
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    // Controller reconciliation 10: the fake track's title and artist, as the popup draws them.
    assert!(d.wait_for("media popup shown: Paper Lanterns — Marlowe Vane", DELAY), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    // On sway the release waits for Escape's key-up on the bar's surface (fix wave A).
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "the keyboard stayed bound at rest: {:?}", d.log());
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for_count("media popup shown", 2, DELAY), "opens again: {:?}", d.log());
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "a second click on the module closes it: {:?}", d.log());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("media popup shown"), 2, "closed, not reopened by the same click: {:?}", d.log());
}

#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_popup_buttons_reach_the_player() {
    let (_bus, sway, _hypr, d, players) = media_bar("media-buttons", 1, &[]);
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown", DELAY), "{:?}", d.log());
    // Rows: title, artist, time, buttons (row 3); ⏭ at content column 20, boxed column 21.
    let (x, y) = popup_cell(0, 21, 3);
    sway.click(x, y, "button1");
    assert!(players[0].wait_call("Next", DELAY).is_some(), "a click on ⏭: {:?} / {:?}", players[0].calls(), d.log());
    assert_eq!(d.count("popup closed"), 0, "a click inside the popup keeps it open: {:?}", d.log());
    keys(&sway, &["Left", "Return"]); // from ⏭ (the click selected it) to ⏯
    assert!(players[0].wait_call("PlayPause", DELAY).is_some(), "Enter on ⏯: {:?}", players[0].calls());
    assert!(
        players[0]
            .calls()
            .iter()
            .all(|c| !c.contains("Seek") && !c.contains("SetPosition") && !c.contains("Volume") && !c.contains("Raise")),
        "never seeks, sets volume or raises: {:?}",
        players[0].calls()
    );
}

/// The bar whose popup Escape closed goes with its output while Escape is still down (sway): the
/// release deferred on its surface goes then, not at a key-up or leave that can no longer come.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn a_bar_removed_while_escape_is_down_releases_the_keyboard() {
    let (_bus, sway, _hypr, d, _players) = media_bar("media-unplug-escape", 2, &[]);
    sway.command("output HEADLESS-1 pos 0 0");
    sway.command("output HEADLESS-2 pos 3000 0");
    std::thread::sleep(Duration::from_millis(500));
    sway.click(3000 + MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown", DELAY), "{:?}", d.log());
    // Escape pressed and held 4 s: one wtype, its virtual keyboard alive throughout.
    let held = wtype_spawn(&sway, &["-s", "300", "-P", "Escape", "-s", "4000", "-p", "Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(d.count("keyboard released"), 0, "Escape is down: the release waits: {:?}", d.log());
    sway.command("output HEADLESS-2 disable");
    assert!(d.wait_for("bar removed from HEADLESS-2", DELAY), "{:?}", d.log());
    assert!(d.wait_for("keyboard released", Duration::from_secs(2)), "released with the bar, Escape still down: {:?}", d.log());
    held.wait().unwrap();
}

/// The 1 s tick: never with the popup closed (0 wake-ups at rest with a player playing), every
/// second while it is open and the player plays, gone again once the player pauses (Review Focus).
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_tick_runs_only_while_the_popup_is_open_and_the_player_plays() {
    let (_bus, sway, _hypr, d, players) = media_bar("media-tick", 1, &[]);
    sway.hover(640, 400);
    let closed = wakeups_over_5_s(&d);
    assert_eq!(closed, 0, "the loop woke {closed} times at rest, popup closed, player playing: {:?}", d.log());
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown", DELAY), "{:?}", d.log());
    let open = wakeups_over_5_s(&d);
    assert!((4..=12).contains(&open), "about one tick a second while open and playing: {open}");
    players[0].set_status("Paused");
    std::thread::sleep(Duration::from_secs(1));
    let paused = wakeups_over_5_s(&d);
    assert_eq!(paused, 0, "paused while open: the tick is disarmed ({paused} wake-ups)");
    players[0].set_status("Playing");
    std::thread::sleep(Duration::from_secs(1));
    assert!(wakeups_over_5_s(&d) >= 4, "playing again: the tick is back");
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    sway.hover(640, 400);
    // SCTK arms its key repeat timer at the press of Escape and, at the release, only forgets the
    // key: the timer fires once at the repeat delay (600 ms on sway), finds no key and drops itself.
    std::thread::sleep(Duration::from_secs(1));
    let after = wakeups_over_5_s(&d);
    assert_eq!(after, 0, "closed: nothing armed is left ({after} wake-ups)");
    let seeked = wakeups_over_seeked(&d, &players[0]);
    assert_eq!(seeked, 0, "closed: the Seeked rule went with the popup ({seeked} wake-ups)");
}

/// The active player vanishing moves the popup to the next one; the last one vanishing closes it.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_popup_moves_to_the_next_player_then_closes_when_none_is_left() {
    let (_bus, sway, _hypr, d, mut players) = media_bar("media-gone", 1, &["beta"]);
    players[1].set_status("Paused"); // lumen plays: it is the active one
    std::thread::sleep(Duration::from_millis(500));
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown", DELAY), "{:?}", d.log());
    players.remove(0).kill(); // lumen
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("popup closed"), 0, "beta is left: the popup shows it: {:?}", d.log());
    players.remove(0).kill(); // beta
    assert!(d.wait_for("popup closed (player gone)", DELAY), "{:?}", d.log());
}

/// A player chosen in the popup takes the bar too: the pin changes the active player with no
/// D-Bus signal, and the bar's media cells repaint (fix wave A: the view is no longer rebuilt at
/// every popup refresh, the pin hands it over itself).
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn a_player_chosen_in_the_popup_takes_the_bar() {
    let (_bus, sway, _hypr, d, players) = media_bar("media-pin", 1, &["beta"]);
    players[1].set_status("Paused"); // lumen plays: it is the active one
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown: Paper Lanterns", DELAY), "lumen active: {:?}", d.log());
    // media_bar's theme: catppuccin dark. Playing, the module takes the purple; paused, muted.
    let purple = rgb(preset("catppuccin").unwrap().dark.hues.purple);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| media_cells(s).contains(&purple)), "lumen playing: {:?}", d.log());
    // Rows: title, artist, time, buttons, separator, lumen, beta (row 6).
    let (x, y) = popup_cell(0, 3, 6);
    sway.click(x, y, "button1");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| !media_cells(s).contains(&purple)), "the bar never showed beta paused: {:?}", d.log());
    assert_eq!(d.count("popup closed"), 0, "the pin keeps the popup open: {:?}", d.log());
}

/// Review Focus: the popup open on one output while the bars restyle. Every bar shares one
/// placement, so a switch to floating moves the popup's own bar too: it closes once, by its own
/// parent's identity, with its timer and Seeked rule gone. A style change that keeps the placement
/// leaves it open and ticking.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn a_restyle_closes_the_media_popup_on_the_second_output_once() {
    let (_bus, sway, _hypr, d, players) = media_bar("media-restyle", 2, &[]);
    sway.command("output HEADLESS-1 pos 0 0");
    sway.command("output HEADLESS-2 pos 3000 0");
    std::thread::sleep(Duration::from_millis(500));
    sway.click(3000 + MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown", DELAY), "{:?}", d.log());
    let config = sway.runtime.join("config/alpheus");
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"floating\"\n");
    assert!(d.wait_for("popup closed (restyled)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("popup closed"), 1, "closed once, though both bars moved: {:?}", d.log());
    sway.hover(640, 400);
    assert_eq!(wakeups_over_5_s(&d), 0, "the timer went with the popup");
    assert_eq!(wakeups_over_seeked(&d, &players[0]), 0, "and the Seeked rule");
    // Floating: the bar 6 px down and in.
    sway.click(3000 + MEDIA_AT.0 + 6, MEDIA_AT.1 + 6, "button1");
    assert!(d.wait_for_count("media popup shown", 2, DELAY), "{:?}", d.log());
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"floating\"\nmodule_colors = \"full\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, module colours full", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("popup closed"), 1, "the same placement: the popup stays: {:?}", d.log());
    assert!(wakeups_over_5_s(&d) >= 4, "and keeps ticking");
}
