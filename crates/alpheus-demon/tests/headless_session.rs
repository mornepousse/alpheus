//! The daemon on a real headless Wayland compositor: one bar per output,
//! hot plugging, clean shutdown, only one daemon, and the launcher end to
//! end. Requires headless sway and wtype (`nix develop`):
//! `cargo test -p alpheus-demon --test headless_session -- --ignored --test-threads=1`.

mod common;

use alpheus_commun::launcher_ipc::{Reply, Request, encode_request, socket_path};
use alpheus_commun::theme::Rgb;
use alpheus_commun::theme::presets::preset;
use alpheus_launch::{EXIT_CANCELLED, EXIT_ERROR, EXIT_OK, EXIT_UNREACHABLE, run};
use common::sni::{Entry, FakeItem, FakeMenu, Spec, Value};
use common::{
    Daemon, DaemonOptions, FakeHyprland, Peer, Ppm, PrivateBus, SIGNATURE, Sway, Workstation, fake_program, launcher, process_alive,
    start_daemon, start_daemon_with, wait_for_pid_file, write_atomically, wtype,
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
    assert!(d.log().iter().all(|l| !l.contains("calendar") && !l.contains("popup")), "{:?}", d.log());
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
