//! The daemon on a real headless Wayland compositor: one bar per output,
//! hot plugging, clean shutdown, only one daemon, and the launcher end to
//! end. Requires headless sway and wtype (`nix develop`):
//! `cargo test -p alpheus-demon --test headless_session -- --ignored --test-threads=1`.

mod common;

use alpheus_commun::launcher_ipc::{Reply, Request, encode_request, socket_path};
use alpheus_launch::{EXIT_CANCELLED, EXIT_ERROR, EXIT_OK, EXIT_UNREACHABLE, run};
use common::{
    Daemon, DaemonOptions, FakeHyprland, PrivateBus, SIGNATURE, Sway, Workstation, fake_program, launcher, process_alive, start_daemon,
    start_daemon_with, wait_for_pid_file, wtype,
};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
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
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for("launcher closed (toggle)", DELAY), "{:?}", d.log());
    // close is idempotent: answered, nothing to close.
    assert_eq!(launcher(&sway, &Request::Close), Reply::Done);
    assert_eq!(launcher(&sway, &Request::Close), Reply::Done);
    // Mod+Space twice quickly: opened then closed, never two boxes.
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher closed (toggle)", 2, DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(300));
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
