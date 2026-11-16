//! The daemon on a real headless Wayland compositor: one bar per output,
//! hot plugging, clean shutdown, only one daemon, and the launcher end to
//! end. Requires headless sway and wtype (`nix develop`):
//! `cargo test -p alpheus-demon --test headless_session -- --ignored --test-threads=1`.

mod common;

use alpheus_commun::displays::wire::{self, Snapshot, Status, TrialEnd, TrialState};
use alpheus_commun::displays::{Active, Mode, Scale, Step};
use alpheus_commun::launcher_ipc::{DmenuFlags, OsdVerb, Reply, Request, encode_request, socket_path};
use alpheus_commun::theme::Rgb;
use alpheus_commun::theme::presets::preset;
use alpheus_demon::quick::{BAR, BAR_START};
use alpheus_launch::{EXIT_CANCELLED, EXIT_ERROR, EXIT_OK, EXIT_UNREACHABLE, run};
use common::bluez::{FakeBluez, device_path};
use common::displays::{displays, output, outputs_snapshot, snapshot_until};
use common::fake_mango::FakeMango;
use common::mpris::{FakePlayer, Spec as PlayerSpec, text};
use common::notify::{FakeClient, FakeMako, Heard, Hint, NAME as NOTIFICATIONS, Note};
use common::portal::FakePortal;
use common::sni::{Entry, FakeItem, FakeMenu, Spec, Value};
use common::sway_client::{TestWindow, con_id};
use common::{
    AG06_SINK, AG06_SOURCE, Backend, Daemon, DaemonOptions, FAKE_RUN_MARKER, FakeAp, FakeHyprland, FakeNetworkManager, GroupChild,
    NmChange, Peer, Ppm, PrivateBus, PwDump, SIGNATURE, SPEAKER_SINK, Sway, TempDir, UUID_HOME, UUID_NEW, Workstation, assert_no_spin,
    capture_delta, capture_gone, default_sink_delta, fake_program, fake_pw_dump_in, fake_recorder, fake_source, fake_stdin_recorder,
    harness_bar_toml, launcher, process_alive, pw_dump_send, recorded, recorded_stdin, start_daemon, start_daemon_with, volume_delta,
    wait_for_pid_file, write_atomically, wtype, wtype_spawn,
};
use dbus::arg::{PropMap, RefArg, Variant};
use std::cell::Cell;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

/// A base build (`ALPHEUS_TEST_BASE_CONFIG=1`) gets no `[displays]`, which it would reject; the
/// privacy roots stay on empty directories either way.
#[test]
fn the_harness_leaves_displays_out_for_a_base_build() {
    let runtime = Path::new("/run/test");
    for base in [false, true] {
        let text = harness_bar_toml("modules = [\"clock\"]\n", runtime, base);
        assert!(text.contains("dev_dir = \"/run/test/privacy-dev\"") && text.contains("proc_dir = \"/run/test/privacy-proc\""), "{text}");
        assert_eq!(text.contains("[displays]"), !base, "{text}");
        assert!(alpheus_demon::config::BarConfig::from_text(&text).is_ok(), "{text}");
    }
    let own = harness_bar_toml("[displays]\ndrm_dir = \"/x\"\n", runtime, false);
    assert_eq!(own.matches("drm_dir").count(), 1, "the test's own table is kept: {own}");
}

/// The harness fails a test whose daemon rejected bar.toml, instead of running it on defaults.
#[test]
#[ignore = "headless sway (nix develop)"]
fn a_rejected_bar_toml_fails_the_harness() {
    let sway = Sway::start("rejected", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some("[nonsense]\n"), ..DaemonOptions::default() })
    }));
    assert!(started.is_err(), "the harness took a daemon running on defaults");
    let d = start_daemon(&sway);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
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
    // A bounded poll, not a flat sleep:
    // fails as soon as "source down" shows up, rather than only finding out
    // after waiting out a fixed delay.
    let t = std::time::Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        assert!(d2.log().iter().all(|l| !l.contains("source down")), "the private bus reached: {:?}", d2.log());
        std::thread::sleep(Duration::from_millis(50));
    }
}

const SHOWN: &str = "launcher shown on";
/// What a scan logs as it starts: its absence proves no scan ran only next to a control that logs it.
const SCAN_STARTED: &str = "launcher scan: started";

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

/// Small features, `--calc`: the calculator opens with no scan, Enter copies the result through
/// `copy` (a stub script, never `wl-copy`), and a second `--calc` past the debounce closes it.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn calc_opens_the_calculator_and_enter_copies() {
    use std::os::unix::fs::PermissionsExt;
    let sway = Sway::start("launcher-calc", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let copied = sway.runtime.join("copied");
    let stub = sway.runtime.join("fake-copy");
    std::fs::write(&stub, format!("#!/bin/sh\ncat > '{0}.tmp' && mv '{0}.tmp' '{0}'\n", copied.display())).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let toml = format!("copy = [\"{}\"]\n", stub.display());
    let mut d = start_daemon_with(&sway, &DaemonOptions { launcher_toml: Some(&toml), ..DaemonOptions::default() });
    ready(&d);
    assert_eq!(script(&sway, &["--calc"], "").join().unwrap(), (0, String::new(), String::new()));
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    assert_eq!(d.count(SCAN_STARTED), 0, "the calculator scans nothing: {:?}", d.log());
    // One wtype process: its virtual keyboard is the seat's only one (see `wtype_spawn`).
    assert!(wtype(&sway, &["-s", "300", "6*7", "-k", "Return"]).success());
    wait_for_file(&copied, DELAY);
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "42", "the result alone on the copy program's stdin");
    assert!(d.wait_for("launcher closed (activated)", DELAY), "{:?}", d.log());
    // Past TOGGLE_DEBOUNCE, a second `--calc` closes an open calculator.
    assert_eq!(script(&sway, &["--calc"], "").join().unwrap().0, 0);
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
    thread::sleep(PAST_DEBOUNCE);
    assert_eq!(script(&sway, &["--calc"], "").join().unwrap().0, 0);
    assert!(d.wait_for("launcher closed (toggle)", DELAY), "{:?}", d.log());
    assert_eq!(d.count(SCAN_STARTED), 0, "neither calculator open scanned: {:?}", d.log());
    // The control: an apps open logs that line, so the zeros above can go red.
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count(SHOWN, 3, DELAY), "{:?}", d.log());
    assert!(d.wait_for(SCAN_STARTED, DELAY), "an apps open scans: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar keeps running");
}

/// Launcher 2, `?`: Enter opens the percent-encoded search URL through `open` (a stub script,
/// never `xdg-open`), and the text typed is logged nowhere.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn a_question_mark_opens_the_search_url() {
    use std::os::unix::fs::PermissionsExt;
    let sway = Sway::start("launcher-search", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let opened = sway.runtime.join("opened");
    let stub = sway.runtime.join("fake-open");
    std::fs::write(&stub, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n", opened.display())).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let toml = format!("open = [\"{}\"]\n", stub.display());
    let d = start_daemon_with(&sway, &DaemonOptions { launcher_toml: Some(&toml), ..DaemonOptions::default() });
    ready(&d);
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "? rust ratatui", "-k", "Return"]).success());
    wait_for_file(&opened, DELAY);
    assert_eq!(std::fs::read_to_string(&opened).unwrap(), "https://duckduckgo.com/?q=rust%20ratatui\n");
    assert!(d.wait_for("launcher closed (activated)", DELAY), "{:?}", d.log());
    assert!(d.log().iter().all(|l| !l.contains("ratatui")), "the typed text is never logged: {:?}", d.log());
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
    // down: its focus, the launcher's box, died with it.
    assert!(d.wait_for_count("keyboard released", 2, DELAY), "the keyboard stayed bound at rest: {:?}", d.log());
    // Free text with no lines (tools-menu's "Cible (IP / hôte)"): Enter returns what was typed.
    let menu = script(&sway, &["--dmenu", "--prompt", "Target"], "");
    assert!(d.wait_for_count(SHOWN, 3, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "10.0.0.1", "-k", "Return"]).success());
    let (code, out, _) = menu.join().unwrap();
    assert_eq!((code, out.as_str()), (EXIT_OK, "10.0.0.1\n"));
}

/// The output's pixels below the bar (whose clock may tick between two shots): the launcher box.
fn below_the_bar(sway: &Sway) -> Vec<[u8; 3]> {
    let s = sway.screenshot("HEADLESS-1").expect("grim reads the output");
    (100..s.height).flat_map(|y| (0..s.width).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect()
}

/// A dmenu menu that types `text` (then waits before Enter): what the box showed once the text
/// was drawn, and what the client got.
fn typed_menu(sway: &Sway, d: &Daemon, shown: usize, args: &[&str], text: &str) -> (Vec<[u8; 3]>, (i32, String, String)) {
    let menu = script(sway, args, "");
    assert!(d.wait_for_count(SHOWN, shown, DELAY), "{:?}", d.log());
    // The box drawn (its first frame committed, then given time to reach the screen), nothing
    // typed yet: the frame to tell typing from.
    thread::sleep(Duration::from_millis(200));
    let empty = below_the_bar(sway);
    let keys = wtype_spawn(sway, &["-s", "300", text, "-s", "3000", "-k", "Return"]);
    let t = Instant::now();
    let mut last = empty.clone();
    let drawn = loop {
        assert!(t.elapsed() < DELAY, "the typed text was never drawn");
        thread::sleep(Duration::from_millis(150));
        let now = below_the_bar(sway);
        if now != empty && now == last {
            break now;
        }
        last = now;
    };
    assert!(keys.wait().unwrap().success());
    (drawn, menu.join().unwrap())
}

/// `--dmenu --password` (networkmanager_dmenu's Wi-Fi key): the typed characters come back as
/// is; the box showed dots — two passwords of one length draw the same pixels, where the same
/// text without `--password` draws others —; the daemon's log never holds the text, and the
/// launcher's history never sees it.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn dmenu_password_shows_dots_and_keeps_nothing() {
    let sway = Sway::start("launcher-password", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon(&sway);
    ready(&d);
    let (first, got) = typed_menu(&sway, &d, 1, &["--dmenu", "--password", "-p", "Key"], "hunter2");
    assert_eq!(got, (EXIT_OK, "hunter2\n".into(), String::new()));
    let (second, got) = typed_menu(&sway, &d, 2, &["--dmenu", "--password", "-p", "Key"], "abcdefg");
    assert_eq!(got, (EXIT_OK, "abcdefg\n".into(), String::new()));
    assert!(first == second, "seven dots both times, whatever was typed");
    let (plain, got) = typed_menu(&sway, &d, 3, &["--dmenu", "-p", "Key"], "hunter2");
    assert_eq!(got.1, "hunter2\n");
    assert!(plain != first, "without --password the text itself is drawn");
    assert!(d.wait_for_count("launcher closed (activated)", 3, DELAY), "{:?}", d.log());
    let log = d.log();
    assert!(!log.iter().any(|l| l.contains("abcdefg") || l.contains("hunter2")), "typed text in the daemon's log: {log:?}");
    let history = std::fs::read_to_string(sway.runtime.join("state/alpheus/launcher-history")).unwrap_or_default();
    assert!(!history.contains("hunter2") && !history.contains("abcdefg"), "{history:?}");
}

/// `--dmenu --index` (audio-switcher, bzmenu): the third line prints 2; typed text that matches
/// no line prints -1, as fuzzel does, and exits 0.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn dmenu_index_prints_the_position() {
    let sway = Sway::start("launcher-index", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon(&sway);
    ready(&d);
    let menu = script(&sway, &["--dmenu", "--index"], "alpha\nbeta\ngamma\ndelta\n");
    assert!(d.wait_for_count(SHOWN, 1, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "-k", "Down", "-k", "Down", "-k", "Return"]).success());
    assert_eq!(menu.join().unwrap(), (EXIT_OK, "2\n".into(), String::new()));
    let menu = script(&sway, &["--dmenu", "--index"], "alpha\nbeta\n");
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "zzz", "-k", "Return"]).success());
    assert_eq!(menu.join().unwrap(), (EXIT_OK, "-1\n".into(), String::new()));
}

/// `--dmenu --visible 2` (tools-menu's home menu under alpheus): the two categories on an empty
/// query, any line by typing; with `--index`, its position among all the lines.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn dmenu_visible_lists_the_first_lines_and_searches_them_all() {
    let sway = Sway::start("launcher-visible", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon(&sway);
    ready(&d);
    let input = "Captures\nAffichage\nCaptures · Zone\nAffichage · Luminosité · 50%\n";
    let menu = script(&sway, &["--dmenu", "--visible", "2"], input);
    assert!(d.wait_for_count(SHOWN, 1, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "-k", "Down", "-k", "Down", "-k", "Down", "-k", "Return"]).success());
    assert_eq!(menu.join().unwrap(), (EXIT_OK, "Affichage\n".into(), String::new()), "two lines listed: Down stops on the second");
    let menu = script(&sway, &["--dmenu", "--visible", "2"], input);
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "lumi", "-k", "Return"]).success());
    assert_eq!(menu.join().unwrap(), (EXIT_OK, "Affichage · Luminosité · 50%\n".into(), String::new()));
    let menu = script(&sway, &["--dmenu", "--index", "--visible", "2"], input);
    assert!(d.wait_for_count(SHOWN, 3, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "zone", "-k", "Return"]).success());
    assert_eq!(menu.join().unwrap(), (EXIT_OK, "2\n".into(), String::new()));
    // A query matching more than N lines lists them all: "a" matches the four, and 0 to 3 Downs
    // reach four different lines (cut to two, the third and fourth would repeat the second). The
    // order is nucleo's, not pinned here.
    let mut chosen = std::collections::BTreeSet::new();
    for downs in 0..4 {
        let menu = script(&sway, &["--dmenu", "--index", "--visible", "2"], input);
        assert!(d.wait_for_count(SHOWN, 4 + downs, DELAY), "{:?}", d.log());
        let mut keys = vec!["-s", "300", "a"];
        keys.extend(std::iter::repeat_n(["-k", "Down"], downs).flatten());
        keys.extend(["-k", "Return"]);
        assert!(wtype(&sway, &keys).success());
        let (code, out, err) = menu.join().unwrap();
        assert_eq!((code, err), (EXIT_OK, String::new()));
        chosen.insert(out);
    }
    assert_eq!(chosen.into_iter().collect::<Vec<_>>(), ["0\n", "1\n", "2\n", "3\n"], "every line reachable");
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn dmenu_client_killed_closes_the_launcher() {
    let sway = Sway::start("launcher-killed", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon(&sway);
    ready(&d);
    let mut client = UnixStream::connect(socket_path(&sway.runtime)).unwrap();
    client
        .write_all(&encode_request(&Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into()], flags: DmenuFlags::default() }))
        .unwrap();
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
/// "unreachable" makes shell-ctl and tools-menu fall back to fuzzel.
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
    // actual convention (fixed in `tray_horizontal`: qtbase negates the horizontal
    // value120 before Plasma re-exposes it, so right is -120, left is +120; vertical is unaffected).
    sway.scroll(FIRST_ICON.0, FIRST_ICON.1, "right", 1);
    let scroll = nimbus.wait_call("Scroll", DELAY);
    assert_eq!(scroll.as_deref(), Some("org.kde.StatusNotifierItem.Scroll(-120, horizontal)"), "one notch right: {:?}", d.log());
    std::thread::sleep(Duration::from_millis(300));
    let scrolls = nimbus.calls().iter().filter(|c| c.contains(".Scroll(")).count();
    assert_eq!(scrolls, 1, "one notch, one Scroll: {:?}", nimbus.calls());
    // A burst of signals: dirty, one frame callback, one re-read.
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
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_millis(4500));
    let wakeups = d.main_thread_wakeups() - before;
    assert!(wakeups <= 4, "{wakeups} wake-ups of the loop in 4.5 s after the bus closed");
    assert_no_spin(d.main_thread_cpu_ticks() - ticks, Duration::from_millis(4500), "after the bus closed");
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
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    assert_eq!(delta, 0, "the loop woke {delta} times with the pointer on the clock: {:?}", d.log());
    assert_no_spin(d.main_thread_cpu_ticks() - ticks, Duration::from_secs(5), "the pointer on the clock");
    assert!(d.log().iter().all(|l| !l.contains("tooltip")), "{:?}", d.log());
}

/// Opening the launcher used to close an open
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

/// An output going away used to close its tooltip
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

/// `tray_output` used to cancel the hover unconditionally on
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

/// An icon animating faster than the ½ s delay (`NewIcon` signals
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

/// `[tray] tooltips = false` arms no timer at all, even
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
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_secs(2));
    assert_no_spin(d.main_thread_cpu_ticks() - ticks, Duration::from_secs(2), "tooltips = false");
    let delta = d.main_thread_wakeups().saturating_sub(before);
    assert_eq!(delta, 0, "tooltips = false: the loop woke {delta} times with the pointer on the icon: {:?}", d.log());
    assert!(d.log().iter().all(|l| !l.contains("tooltip")), "{:?}", d.log());
}

/// A press on the bar, even away from the hovered icon, closes
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

/// The hovered item's application quitting (its
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

// ---- Menus wired end to end ----

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

/// Keys typed after wtype's keyboard appeared (the bar's surface takes it).
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
/// "Item") so a menu that does open shows right away — the delay
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

/// A press on the bar that hits no icon drops `menu.last_icon` —
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
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(d.main_thread_wakeups().saturating_sub(before), 0, "{:?}", d.log());
    assert_no_spin(d.main_thread_cpu_ticks() - ticks, Duration::from_secs(3), "nothing pending");
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

/// A press that itself closes the menu leaves no "closed recently"
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

/// The session bus dying while a menu is shown must close that popup (never
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

/// The item's Menu path changing under an open menu (a duplicate
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
    // The item is still here — only its Menu moved — so this
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

/// A click elsewhere on the bar closes the calendar (an outside
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
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    assert_eq!(delta, 0, "the loop woke {delta} times at rest after the calendar: {:?}", d.log());
    assert_no_spin(d.main_thread_cpu_ticks() - ticks, Duration::from_secs(5), "after the calendar");
}

/// The bar still runs a second later, and never panicked: a debug build's panic takes a while to
/// print its backtrace before the abort, so the log is checked too.
fn survives(d: &mut Daemon, after: &str) {
    assert_eq!(d.exit(Duration::from_millis(1000)), None, "the bar died after {after}: {:?}", d.log());
    assert!(!d.log().iter().any(|l| l.contains("panicked")), "the bar panicked after {after}: {:?}", d.log());
}

/// Mae's crash (2026-10-06, 2026-10-09: "RefCell already borrowed" in calloop, then an abort):
/// a key that repeats (any letter, Right…) arms SCTK's repeat timer; once its delay has passed,
/// the timer is gone but its token stays in the keyboard's data. The keyboard released after that,
/// with no key arming a new timer (an outside click, a Toggle), had its last reference dropped
/// inside calloop's dispatch when the compositor's `delete_id` was read, and that data's Drop
/// re-entered calloop. On headless sway, wtype's virtual keyboard is the seat's only one: each
/// wtype below stays alive (its trailing `-s`) past the close, so the close is what releases it.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn a_keyboard_released_after_its_repeat_delay_keeps_the_bar_alive() {
    let (sway, _hypr, mut d) = clock_bar("keyboard-drop", "");
    ready(&d);
    // The calendar, then Right (a repeating key), then past sway's 600 ms repeat delay an outside
    // click: "popup closed (outside click)", as in Mae's journal.
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    let held = wtype_spawn(&sway, &["-s", "300", "-k", "Right", "-s", "2500"]);
    assert!(d.wait_for("calendar shows", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(1000));
    sway.click(640, 10, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("keyboard released", DELAY), "{:?}", d.log());
    survives(&mut d, "the popup");
    let _ = held.wait();
    // The launcher, a letter typed, then past the delay a Toggle closes it.
    for round in 1..=3 {
        assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
        assert!(d.wait_for_count(SHOWN, round, DELAY), "{:?}", d.log());
        let held = wtype_spawn(&sway, &["-s", "300", "x", "-s", "2000"]);
        thread::sleep(Duration::from_millis(1300));
        assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
        assert!(d.wait_for_count("launcher closed (toggle)", round, DELAY), "{:?}", d.log());
        assert!(d.wait_for_count("keyboard released", round + 1, DELAY), "{:?}", d.log());
        survives(&mut d, &format!("the launcher, round {round}"));
        let _ = held.wait();
    }
}

/// The closes of Mae's daily use, many times in a row: Enter (a script menu answered) and Escape
/// on the launcher, then an outside click on the calendar after Right's repeat delay. The bar must
/// outlive all of them. (A close by a repeating key arms a fresh repeat timer that keeps the
/// keyboard's data alive past the `delete_id`, so those 60 closes alone never crashed; the last
/// close is the one that did.)
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn thirty_launcher_closes_by_key_then_an_outside_click_keep_the_bar_alive() {
    let (sway, _hypr, mut d) = clock_bar("keyboard-cycles", "");
    ready(&d);
    for round in 1..=30 {
        let shown = 2 * round - 1;
        let menu = script(&sway, &["--dmenu"], "one\ntwo\n");
        assert!(d.wait_for_count(SHOWN, shown, DELAY), "{:?}", d.log());
        assert!(wtype(&sway, &["-s", "300", "-k", "Return"]).success());
        let (code, out, _) = menu.join().unwrap();
        assert_eq!((code, out.as_str()), (EXIT_OK, "one\n"), "round {round}");
        assert!(d.wait_for_count("launcher closed (activated)", round, DELAY), "{:?}", d.log());
        assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
        assert!(d.wait_for_count(SHOWN, shown + 1, DELAY), "{:?}", d.log());
        assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
        assert!(d.wait_for_count("launcher closed (escape)", round, DELAY), "{:?}", d.log());
    }
    sway.click(40, 10, "button1");
    assert!(d.wait_for("calendar shown", DELAY), "{:?}", d.log());
    let held = wtype_spawn(&sway, &["-s", "300", "-k", "Right", "-s", "2500"]);
    assert!(d.wait_for("calendar shows", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(1000));
    sway.click(640, 10, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    survives(&mut d, "sixty launcher closes and an outside click");
    let _ = held.wait();
    // The retired list never grows: a keyboard released by Enter or Escape may still be held by
    // that key's repeat timer (600 ms) at the next release, never longer; the outside click, over
    // a second after the last close, finds none left.
    let releases: Vec<String> = d.log().into_iter().filter(|l| l.contains("keyboard released")).collect();
    assert!(releases.len() >= 61, "{releases:?}");
    let ok = |l: &String| l.ends_with("keyboard released (0 retired)") || l.ends_with("keyboard released (1 retired)");
    assert!(releases.iter().all(ok), "the retired keyboards pile up: {releases:?}");
    assert!(releases.last().is_some_and(|l| l.ends_with("(0 retired)")), "{releases:?}");
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

/// The bar alone with its clock: its left edge is plain background.
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

/// A wake-up is the loop blocking then running again: a voluntary switch. Involuntary ones are
/// preemptions under CPU load, not wake-ups: counting them made the budgets flaky.
#[test]
fn wakeups_count_voluntary_switches_only() {
    let status = "Name:\talpheus-demon\nvoluntary_ctxt_switches:\t12\nnonvoluntary_ctxt_switches:\t7\n";
    assert_eq!(common::switches(status), 12);
    assert_eq!(common::switches(""), 0);
}

/// A busy spin never blocks, so it makes no voluntary switch: the at-rest checks also read the CPU
/// time (utime + stime, fields 14 and 15 of a /proc stat line, after the command in parentheses,
/// which may hold spaces and parentheses itself).
#[test]
fn cpu_ticks_are_utime_plus_stime() {
    let stat = "4242 (alpheus (demon) x) S 1 4242 4242 0 -1 4194560 900 0 0 0 31 12 0 0 20 0 6 0 100 0";
    assert_eq!(common::cpu_ticks(stat), 43);
    assert_eq!(common::cpu_ticks(""), 0);
    assert_eq!(common::rest_ticks(Duration::from_secs(5)), common::REST_TICKS);
    assert_eq!(common::rest_ticks(Duration::from_secs(10)), 2 * common::REST_TICKS);
    assert_eq!(common::rest_ticks(Duration::from_secs(2)), common::REST_TICKS, "never below the 5 s allowance");
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

/// End to end: a style set at startup reaches the painter, and the opaque region the
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

/// A colour file caught mid-write (matugen). A truncated
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

/// A bar on catppuccin dark holding only the clock, at the left edge. Also returns
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
    // The exclusive zone is the bar height and the compositor adds the 6 px
    // top margin, so tiled windows start at 6 + 22, just under the floating bar.
    assert!(wait_until(|| sway.workspace_y() == Some(28)), "floating: windows start at 6 + 22: {:?}", sway.workspace_y());
    write_atomically(&config, "theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"flat\"\n");
    assert!(d.wait_for("theme reloaded: catppuccin, dark, style flat", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| first_row(s) == Some(0) && s.at(6, 6) == Some(mocha)), "{:?}", d.log());
    assert!(wait_until(|| sway.workspace_y() == Some(22)), "back to 22: {:?}", sway.workspace_y());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// The calendar hangs from the bar's surface; a switch to floating moves and resizes
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

/// An invalid style is one warning and `flat`, with one repaint; reading the same
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

/// The mode file the TUI reads, or None.
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
/// settings are stored.
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
    let (sway, _hypr, mut d, config, behind) = shape_bar("shape-truncated");
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

/// The private session bus dying — the media module goes away, nothing spins.
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
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_millis(4500));
    let wakeups = d.main_thread_wakeups() - before;
    assert!(wakeups <= 4, "{wakeups} wake-ups of the loop in 4.5 s after the bus closed");
    assert_no_spin(d.main_thread_cpu_ticks() - ticks, Duration::from_millis(4500), "after the bus closed");
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
    assert!(players[0].wait_call("GetAll", DELAY).is_some(), "the daemon read the fake player: {:?}", d.log());
    // Read and drawn: a player plays, so the module shows in the purple (catppuccin dark).
    let purple = rgb(preset("catppuccin").unwrap().dark.hues.purple);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| media_cells(s).contains(&purple)), "never drawn: {:?}", d.log());
    // Each extra player, the latest to play, takes the bar: on screen before the next one (or the
    // test) starts. Its GetAll asked is not its reply read: a test taking the bar as its reference
    // before that could see the previous player, and miss the change it waits for.
    for n in extra {
        let before = sway.screenshot("HEADLESS-1").map(|s| media_cells(&s)).unwrap();
        let player = FakePlayer::start(&bus, PlayerSpec::playing(n, "Second Song"));
        assert!(player.wait_call("GetAll", DELAY).is_some(), "the daemon read {n}: {:?}", d.log());
        assert!(sway.wait_for_screen("HEADLESS-1", |s| media_cells(s) != before), "{n} never took the bar: {:?}", d.log());
        players.push(player);
    }
    (bus, sway, hypr, d, players)
}

/// The pixels of the media popup's title row (row 0, its 30 content cells), hung from MEDIA_AT.
fn popup_title(s: &Ppm) -> Vec<[u8; 3]> {
    let ((x0, y), (x1, _)) = (popup_cell(0, 1, 0), popup_cell(0, 30, 0));
    (y - 8..y + 8).flat_map(|y| (x0 - 4..x1 + 4).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect()
}

/// Waits until the media popup is on screen: its title row drawn in the accent (catppuccin dark).
/// "media popup shown" is logged when the popup is asked for, before the compositor's configure
/// and the first buffer that maps it; a click in that gap lands on no surface, and sway ends the
/// grab ("popup closed (dismissed)"). A click meant for the popup waits for this.
fn wait_for_the_popup_drawn(sway: &Sway, d: &Daemon) {
    let accent = rgb(preset("catppuccin").unwrap().dark.accent);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| popup_title(s).contains(&accent)), "the popup drawn: {:?}", d.log());
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

/// The loop thread's wake-ups and CPU ticks over 5 s, away from the minute's turn (the clock is the
/// bar's only other timer).
fn activity_over_5_s(d: &Daemon) -> (u64, u64) {
    away_from_the_minute();
    std::thread::sleep(Duration::from_millis(300));
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    (d.main_thread_wakeups().saturating_sub(before), d.main_thread_cpu_ticks().saturating_sub(ticks))
}

/// The loop thread's wake-ups over 5 s while something ticks.
fn wakeups_over_5_s(d: &Daemon) -> u64 {
    activity_over_5_s(d).0
}

/// The loop thread's wake-ups over 5 s at rest, its CPU checked too: a busy spin makes no wake-up.
fn rest_over_5_s(d: &Daemon) -> u64 {
    let (wakeups, ticks) = activity_over_5_s(d);
    assert_no_spin(ticks, Duration::from_secs(5), "the loop thread");
    wakeups
}

/// The loop's wake-ups while `player` broadcasts five Seeked over 1.5 s: none once the popup closed,
/// since the bar's Seeked match rule goes with it (the bus then delivers it nothing).
fn wakeups_over_seeked(d: &Daemon, player: &FakePlayer) -> u64 {
    away_from_the_minute();
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    for us in 1..=5 {
        player.seeked(us * 1_000_000);
        std::thread::sleep(Duration::from_millis(300));
    }
    assert_no_spin(d.main_thread_cpu_ticks().saturating_sub(ticks), Duration::from_millis(1500), "the loop thread");
    d.main_thread_wakeups().saturating_sub(before)
}

#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_media_popup_opens_on_a_left_click_and_escape_closes_it() {
    let (_bus, sway, _hypr, d, _players) = media_bar("media-open", 1, &[]);
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    // The fake track's title and artist, as the popup draws them.
    assert!(d.wait_for("media popup shown: Paper Lanterns — Marlowe Vane", DELAY), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    // On sway the release waits for Escape's key-up on the bar's surface.
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
    wait_for_the_popup_drawn(&sway, &d);
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
/// second while it is open and the player plays, gone again once the player pauses.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_tick_runs_only_while_the_popup_is_open_and_the_player_plays() {
    let (_bus, sway, _hypr, d, players) = media_bar("media-tick", 1, &[]);
    sway.hover(640, 400);
    let closed = rest_over_5_s(&d);
    assert_eq!(closed, 0, "the loop woke {closed} times at rest, popup closed, player playing: {:?}", d.log());
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown", DELAY), "{:?}", d.log());
    let open = wakeups_over_5_s(&d);
    assert!((4..=12).contains(&open), "about one tick a second while open and playing: {open}");
    players[0].set_status("Paused");
    std::thread::sleep(Duration::from_secs(1));
    let paused = rest_over_5_s(&d);
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
    let after = rest_over_5_s(&d);
    assert_eq!(after, 0, "closed: nothing armed is left ({after} wake-ups)");
    let seeked = wakeups_over_seeked(&d, &players[0]);
    assert_eq!(seeked, 0, "closed: the Seeked rule went with the popup ({seeked} wake-ups)");
}

/// The active player vanishing moves the popup to the next one; the last one vanishing closes it.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_popup_moves_to_the_next_player_then_closes_when_none_is_left() {
    let (_bus, sway, _hypr, d, mut players) = media_bar("media-gone", 1, &["beta"]);
    // beta, the latest to play, is active; once it pauses, lumen is, and the bar shows its title.
    let shown = sway.screenshot("HEADLESS-1").map(|s| media_cells(&s)).unwrap();
    players[1].set_status("Paused");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| media_cells(s) != shown), "the bar never moved to lumen: {:?}", d.log());
    sway.click(MEDIA_AT.0, MEDIA_AT.1, "button1");
    assert!(d.wait_for("media popup shown: Paper Lanterns", DELAY), "lumen active: {:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    let accent = rgb(preset("catppuccin").unwrap().dark.accent);
    let before = sway.screenshot("HEADLESS-1").map(|s| popup_title(&s)).unwrap();
    players.remove(0).kill(); // lumen
    // The title row repainted with beta's track ("Second Song"), still in the accent: still open.
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| {
            let t = popup_title(s);
            t.contains(&accent) && t != before
        }),
        "beta's title never drawn: {:?}",
        d.log()
    );
    assert_eq!(d.count("popup closed"), 0, "beta is left: the popup shows it: {:?}", d.log());
    players.remove(0).kill(); // beta
    assert!(d.wait_for("popup closed (player gone)", DELAY), "{:?}", d.log());
}

/// A player chosen in the popup takes the bar too: the pin changes the active player with no
/// D-Bus signal, and the bar's media cells repaint (the view is no longer rebuilt at
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
    wait_for_the_popup_drawn(&sway, &d);
    // Rows: title, artist, time, buttons, separator, lumen, beta (row 6).
    let (x, y) = popup_cell(0, 3, 6);
    sway.click(x, y, "button1");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| !media_cells(s).contains(&purple)), "the bar never showed beta paused: {:?}", d.log());
    assert_eq!(d.count("popup closed"), 0, "the pin keeps the popup open: {:?}", d.log());
}

/// The popup open on one output while the bars restyle. Every bar shares one
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
    assert_eq!(rest_over_5_s(&d), 0, "the timer went with the popup");
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

// ---- The bell ----

const BELL_BAR: &str = "modules = [\"notifications\", \"clock\"]\n";
/// Inside the bell on a bar whose first module is `notifications` (cell 1, ≈ 8.4 px a cell): the
/// status block starts at the left edge when there is no workspace or window module, as the tray's
/// icon at (12, 10) does in the tray tests.
const BELL_AT: (u32, u32) = (12, 10);

/// The bell alone on the bar (or `modules`), on a private session bus; `before` runs on the bus
/// before the daemon starts (a mako already holding the name).
fn bell_daemon<T>(name: &str, modules: &str, before: impl FnOnce(&PrivateBus) -> T) -> (PrivateBus, Sway, FakeHyprland, Daemon, T) {
    let bus = PrivateBus::start(name);
    let extra = before(&bus);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(modules), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    (bus, sway, hypr, d, extra)
}

/// The pid behind a unique name on `bus`.
fn pid_of(bus: &PrivateBus, unique: &str) -> Option<u32> {
    let peer = Peer::connect(bus);
    let m = dbus::Message::new_method_call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetConnectionUnixProcessID",
    )
    .unwrap()
    .append1(unique);
    peer.call(m).ok().and_then(|r| r.read1::<u32>().ok())
}

/// Spec § 1: tray off, the media module, the portal and the notifications server share ONE
/// connection, and it is the daemon's own that holds the name.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_bell_serves_on_the_bars_link_with_media_and_the_portal_on_one_connection() {
    let modules = "modules = [\"media\", \"notifications\", \"clock\"]\n";
    let (bus, _sway, _hypr, d, _player, portal) = media_daemon_with("bell-link", modules, None, |bus| FakePortal::start(bus, 2));
    assert!(d.wait_for("notifications: on the bar's session bus", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    assert!(d.wait_for("media: shown", DELAY), "{:?}", d.log());
    assert!(d.wait_for("appearance: the portal says light", DELAY), "{:?}", d.log());
    assert_eq!(connections_of(&bus, d.pid()), 1, "never two: {:?}", d.log());
    let client = FakeClient::connect(&bus);
    assert!(client.notify(&Note::new("Fictional", "Download finished")).is_ok());
    let owner = client.owner(NOTIFICATIONS).expect("the name is held");
    assert_eq!(pid_of(&bus, &owner), Some(d.pid()), "held by the daemon's one connection");
    assert_eq!(portal.calls().len(), 1);
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn with_the_tray_the_bell_rides_its_connection() {
    let (bus, _sway, _hypr, d, ()) = bell_daemon("bell-tray", "modules = [\"tray\", \"notifications\", \"clock\"]\n", |_| ());
    assert!(d.wait_for("notifications: on the tray's session bus", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    assert_eq!(connections_of(&bus, d.pid()), 1, "{:?}", d.log());
    let client = FakeClient::connect(&bus);
    assert!(client.notify(&Note::new("Fictional", "on the tray's connection")).is_ok());
    assert_eq!(client.owner(NOTIFICATIONS), client.owner("org.kde.StatusNotifierWatcher"), "one connection holds both names");
}

/// Spec § 6: without `notifications` in `modules`, the name is never requested, and the verbs say so.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn without_the_module_the_name_is_never_requested() {
    let (bus, sway, _hypr, d, ()) = bell_daemon("bell-none", "modules = [\"tray\", \"clock\"]\n", |_| ());
    assert!(d.wait_for("tray: on the session bus", DELAY), "{:?}", d.log());
    ready(&d);
    std::thread::sleep(Duration::from_millis(500));
    let client = FakeClient::connect(&bus);
    assert_eq!(client.owner(NOTIFICATIONS), None, "never requested");
    assert_eq!(client.notify(&Note::new("Fictional", "anyone?")).unwrap_err(), "org.freedesktop.DBus.Error.ServiceUnknown");
    let (code, _, err) = script(&sway, &["--dnd", "toggle"], "").join().unwrap();
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("notifications module"), "{err}");
    assert!(!sway.runtime.join("state/alpheus/dnd").exists(), "nothing written");
    assert_eq!((d.count("notifications:"), d.count("bell:")), (0, 0), "{:?}", d.log());
}

/// Spec § 5: `alpheus-launch --dnd …` reaches the bell (its line), writes the owner-only empty file,
/// removes it, is idempotent, and the state comes back at the next start; DND still answers Notify.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn dnd_from_alpheus_launch_is_saved_shown_and_restored() {
    let (bus, sway, _hypr, mut d, ()) = bell_daemon("bell-dnd", BELL_BAR, |_| ());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    ready(&d);
    let file = sway.runtime.join("state/alpheus/dnd");
    assert!(!file.exists());
    assert_eq!(script(&sway, &["--dnd", "toggle"], "").join().unwrap(), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    let meta = std::fs::metadata(&file).unwrap();
    assert_eq!((meta.len(), meta.permissions().mode() & 0o777), (0, 0o600), "an empty, owner-only file");
    let client = FakeClient::connect(&bus);
    assert!(client.notify(&Note::new("Fictional", "quiet please")).is_ok(), "do not disturb still answers");
    assert_eq!(script(&sway, &["--dnd", "toggle"], "").join().unwrap().0, EXIT_OK);
    // The first view logged "bell: dnd off" at startup: this is the second.
    assert!(d.wait_for_count("bell: dnd off", 2, DELAY), "{:?}", d.log());
    assert!(!file.exists(), "off: no file");
    for _ in 0..2 {
        assert_eq!(script(&sway, &["--dnd", "on"], "").join().unwrap().0, EXIT_OK);
    }
    assert!(d.wait_for_count("bell: dnd on", 2, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("bell: dnd on"), 2, "on twice is on once: {:?}", d.log());
    assert_eq!(script(&sway, &["--clear-notifications"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for("notifications: cleared", DELAY), "{:?}", d.log());
    d.signal(libc::SIGTERM);
    assert_eq!(d.exit(DELAY), Some(0), "{:?}", d.log());
    let options = DaemonOptions { bar_toml: Some(BELL_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() };
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bell: dnd on", DELAY), "restored at the next start: {:?}", d.log());
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// The bell bar on a private bus, started after `text` was written to the do-not-disturb file.
fn bell_daemon_with_dnd_file(name: &str, text: &str) -> (PrivateBus, Sway, FakeHyprland, Daemon) {
    let bus = PrivateBus::start(name);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    write_dnd_file(&sway, text);
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(BELL_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    (bus, sway, hypr, d)
}

/// Small features 1, decisions 6, 8, 11: `--dnd on --for 30m` writes `until <unix>`, survives a
/// restart, and a file whose end is about to pass turns do-not-disturb off by the timer, removing
/// the file.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_timed_dnd_is_saved_restored_and_ends() {
    let (bus, sway, _hypr, mut d, ()) = bell_daemon("bell-dnd-timed", BELL_BAR, |_| ());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    ready(&d);
    let file = sway.runtime.join("state/alpheus/dnd");
    let before = unix_now();
    assert_eq!(script(&sway, &["--dnd", "on", "--for", "30m"], "").join().unwrap(), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    assert!(d.wait_for("notifications: do not disturb on (ends in 30 min)", DELAY), "{:?}", d.log());
    let text = std::fs::read_to_string(&file).unwrap();
    let end: u64 = text.strip_prefix("until ").and_then(|t| t.strip_suffix('\n')).and_then(|t| t.parse().ok()).expect(&text);
    assert!(end.abs_diff(before + 1800) <= 5, "{text:?} against {}", before + 1800);
    d.signal(libc::SIGTERM);
    assert_eq!(d.exit(DELAY), Some(0), "{:?}", d.log());
    let options = DaemonOptions { bar_toml: Some(BELL_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() };
    let mut d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bell: dnd on", DELAY), "restored at the next start: {:?}", d.log());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "kept as it was");
    d.signal(libc::SIGTERM);
    assert_eq!(d.exit(DELAY), Some(0), "{:?}", d.log());
    // An end two seconds away: on at start, then ended by the timer, the file removed.
    write_dnd_file(&sway, &format!("until {}\n", unix_now() + 2));
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    assert!(d.wait_for("notifications: do not disturb off (timer ended)", Duration::from_secs(4)), "{:?}", d.log());
    assert!(d.wait_for("bell: dnd off", DELAY), "{:?}", d.log());
    assert!(!file.exists(), "the end removes the file: {:?}", d.log());
}

/// Decision 6: at start, an end already passed and a `sharing` file read as off, and the file goes.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_past_or_sharing_dnd_file_is_cleaned_at_start() {
    for (name, text) in [("past", format!("until {}\n", unix_now() - 10)), ("sharing", "sharing\n".to_string())] {
        let (_bus, sway, _hypr, d) = bell_daemon_with_dnd_file(&format!("bell-dnd-clean-{name}"), &text);
        assert!(d.wait_for("bell: dnd off", DELAY), "{name}: {:?}", d.log());
        let file = sway.runtime.join("state/alpheus/dnd");
        let end = Instant::now() + DELAY;
        while file.exists() && Instant::now() < end {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!file.exists(), "{name}: the stale file is removed: {:?}", d.log());
        assert_eq!(d.count("bell: dnd on"), 0, "{name}: {:?}", d.log());
    }
}

/// Decision 7: `--dnd off` and `toggle` end a timed do-not-disturb.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_timed_dnd_is_cancelled_by_off_and_toggle() {
    let (_bus, sway, _hypr, d, ()) = bell_daemon("bell-dnd-cancel", BELL_BAR, |_| ());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    ready(&d);
    let file = sway.runtime.join("state/alpheus/dnd");
    assert_eq!(script(&sway, &["--dnd", "on", "--for", "30m"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    assert!(file.exists());
    assert_eq!(script(&sway, &["--dnd", "off"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for_count("bell: dnd off", 2, DELAY), "{:?}", d.log());
    assert!(!file.exists(), "off: no file");
    assert_eq!(script(&sway, &["--dnd", "on", "--for", "30m"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for_count("bell: dnd on", 2, DELAY), "{:?}", d.log());
    assert_eq!(script(&sway, &["--dnd", "toggle"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for_count("bell: dnd off", 3, DELAY), "{:?}", d.log());
    assert!(!file.exists(), "toggled off: no file");
}

/// Spec, Success criteria: with mako running nothing changes and the bell is dim; once mako stops,
/// alpheus takes the name at once.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn mako_holding_the_name_dims_the_bell_until_it_stops() {
    let (bus, _sway, _hypr, d, mako) = bell_daemon("bell-mako", BELL_BAR, FakeMako::start);
    assert!(d.wait_for("bell: not serving", DELAY), "{:?}", d.log());
    let client = FakeClient::connect(&bus);
    assert_eq!(client.notify(&Note::new("Fictional", "to mako")), Ok(1000), "nothing changes while mako runs");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("bell: serving"), 0, "{:?}", d.log());
    mako.kill(); // mako exits (pkill mako): its connection closes
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    assert!(client.notify(&Note::new("Fictional", "to alpheus")).is_ok_and(|id| id < 1000));
}

/// Spec § 7: a dead session bus dims the bell once; its source goes, nothing spins.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn a_dead_session_bus_dims_the_bell_and_nothing_spins() {
    let (mut bus, _sway, _hypr, mut d, ()) = bell_daemon("bell-bus-dies", BELL_BAR, |_| ());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    let before = d.count("bell: not serving");
    bus.kill();
    assert!(d.wait_for("notifications: the session bus closed", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("bell: not serving", before + 1, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(500));
    let (wakeups, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    std::thread::sleep(Duration::from_millis(4500));
    let wakeups = d.main_thread_wakeups() - wakeups;
    assert!(wakeups <= 4, "{wakeups} wake-ups of the loop in 4.5 s after the bus closed");
    assert_no_spin(d.main_thread_cpu_ticks() - ticks, Duration::from_millis(4500), "after the bus closed");
    assert_eq!(d.count("notifications: the session bus closed"), 1, "{:?}", d.log());
    assert_eq!(d.count("notifications: on the bar's session bus"), 1, "never reconnected: {:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(100)), None, "the bar carries on");
}

/// Spec § 4 and § 8: the bell's middle click toggles do-not-disturb inside the daemon, through the
/// same path as the verb (the file follows), and runs no command.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_bells_middle_click_toggles_do_not_disturb() {
    let (_bus, sway, _hypr, d, ()) = bell_daemon("bell-middle", BELL_BAR, |_| ());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    ready(&d);
    let file = sway.runtime.join("state/alpheus/dnd");
    sway.click(BELL_AT.0, BELL_AT.1, "button2");
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    assert!(file.exists(), "persisted through App::set_dnd");
    sway.click(BELL_AT.0, BELL_AT.1, "button2");
    // The first view logged "bell: dnd off" at startup: this is the second.
    assert!(d.wait_for_count("bell: dnd off", 2, DELAY), "{:?}", d.log());
    assert!(!file.exists(), "off: no file");
}

// ---- The bubbles ----

/// Waits (10 s at most) until `org.freedesktop.Notifications` has an owner on `bus`: the daemon serves.
fn wait_for_the_server(bus: &PrivateBus) {
    let peer = Peer::connect(bus);
    let t = Instant::now();
    loop {
        let m = dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner")
            .unwrap()
            .append1("org.freedesktop.Notifications");
        if peer.call(m).unwrap().read1::<bool>().unwrap() {
            return;
        }
        assert!(t.elapsed() < DELAY, "the daemon never took org.freedesktop.Notifications");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Do-not-disturb on before the daemon starts: the empty file `$XDG_STATE_HOME/alpheus/dnd` (the
/// harness points `XDG_STATE_HOME` at `<runtime>/state`).
fn dnd_on_before_start(sway: &Sway) {
    write_dnd_file(sway, "");
}

/// The same with `text` in the file (`until <unix>\n`, `sharing\n`).
fn write_dnd_file(sway: &Sway, text: &str) {
    let dir = sway.runtime.join("state/alpheus");
    std::fs::create_dir_all(&dir).unwrap();
    for d in [sway.runtime.join("state"), dir.clone()] {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(dir.join("dnd"), text).unwrap();
}

/// The bell bar on catppuccin dark, serving notifications, a client connected. `on_headless`: the
/// fake Hyprland's outputs renamed after sway's, its focused one HEADLESS-2; otherwise its focused
/// output does not exist here, and the compositor chooses. `before` runs once sway is up.
fn notify_daemon(
    name: &str,
    outputs: u32,
    on_headless: bool,
    before: impl FnOnce(&Sway),
) -> (PrivateBus, Sway, FakeHyprland, Daemon, FakeClient) {
    let bus = PrivateBus::start(name);
    let sway = Sway::start(name, outputs);
    let hypr =
        if on_headless { FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE) } else { FakeHyprland::start(&sway.runtime, SIGNATURE) };
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    before(&sway);
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(BELL_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    wait_for_the_server(&bus);
    let client = FakeClient::connect(&bus);
    thread::sleep(Duration::from_millis(300)); // drawn
    (bus, sway, hypr, d, client)
}

/// The first bubble on `s`: its first background row (down the output's centre column), and `inset`
/// px lower (inside its top inset, where no glyph is) its leftmost and rightmost background pixels.
/// None: no bubble background on that column.
fn bubble_box(s: &Ppm, inset: u32) -> Option<(u32, u32, u32)> {
    let bg = popup_bg("catppuccin");
    let cx = s.width / 2;
    let top = (0..s.height).find(|&y| s.at(cx, y) == Some(bg))?;
    let y = top + inset;
    let (mut left, mut right) = (cx, cx);
    while left > 0 && s.at(left - 1, y) == Some(bg) {
        left -= 1;
    }
    while s.at(right + 1, y) == Some(bg) {
        right += 1;
    }
    Some((top, left, right))
}

/// Bubbles crossed by column `x` (inside their left inset, where no glyph is): runs of background
/// at least 10 px tall.
fn bubbles_on(s: &Ppm, x: u32) -> usize {
    let bg = popup_bg("catppuccin");
    let (mut runs, mut run) = (0, 0);
    for y in 0..s.height {
        if s.at(x, y) == Some(bg) {
            run += 1;
        } else {
            runs += usize::from(run >= 10);
            run = 0;
        }
    }
    runs + usize::from(run >= 10)
}

/// The first bubble's frame on `s`: the pixel just left of its background, halfway down its header
/// (clear of the rounded corners and of the text).
fn bubble_frame(s: &Ppm) -> Option<[u8; 3]> {
    // From the top inset's leftmost background (no text there), on along the header row through the
    // pad cell (no text either) to the stroke.
    let (top, mut x, _) = bubble_box(s, 3)?;
    let (y, bg) = (top + 12, popup_bg("catppuccin"));
    while x > 0 && s.at(x - 1, y) == Some(bg) {
        x -= 1;
    }
    s.at(x.checked_sub(1)?, y)
}

/// A layout point in the first bubble on a single output placed at (0, 0): the middle of its header.
fn bubble_at(sway: &Sway, output: &str) -> (u32, u32) {
    let found = Cell::new(None);
    assert!(
        sway.wait_for_screen(output, |s| {
            found.set(bubble_box(s, 3));
            found.get().is_some()
        }),
        "no bubble on {output}"
    );
    let (top, left, right) = found.get().unwrap();
    ((left + right) / 2, top + 12)
}

/// Spec § 3: a bubble appears top centre, under the bar, on the focused output only.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn a_bubble_appears_top_centre_on_the_focused_output() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-place", 2, true, |_| ());
    sway.command("output HEADLESS-1 pos 0 0");
    sway.command("output HEADLESS-2 pos 3000 0");
    for o in ["HEADLESS-1", "HEADLESS-2"] {
        let s = sway.screenshot(o).expect("grim");
        assert_eq!(bubble_box(&s, 3), None, "{o}: the checks bite only if nothing there is popup_bg yet");
    }
    client.notify(&Note::new("Mailer", "Weekly report ready")).unwrap();
    assert!(d.wait_for("bubbles surface on HEADLESS-2", DELAY), "the fixture's focused output: {:?}", d.log());
    assert!(d.wait_for("bubbles shown: 1", DELAY), "{:?}", d.log());
    assert!(
        sway.wait_for_screen("HEADLESS-2", |s| bubble_box(s, 3).is_some_and(|(top, l, r)| top >= 22 && (l + r).abs_diff(s.width) <= 4)),
        "under the 22 px bar, centred: {:?}",
        d.log()
    );
    let s = sway.screenshot("HEADLESS-1").expect("grim");
    assert_eq!(bubble_box(&s, 3), None, "nothing on the other output");
}

/// Spec § 3, clicks: a left click runs the `default` action, then the bubble closes (dismissed)
/// unless the notification is resident.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn a_left_click_runs_the_default_action_then_closes_unless_resident() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-left", 1, false, |_| ());
    let open = |n: Note| n.action("default", "Open").action("later", "Later").timeout(0);
    let id = client.notify(&open(Note::new("Mailer", "Weekly report ready"))).unwrap();
    assert!(d.wait_for("bubbles shown: 1", DELAY), "{:?}", d.log());
    let (x, y) = bubble_at(&sway, "HEADLESS-1");
    let frame = bubble_frame(&sway.screenshot("HEADLESS-1").expect("grim"));
    let urgent = rgb(preset("catppuccin").unwrap().dark.urgent);
    assert!(frame.is_some_and(|f| f != urgent && f != popup_bg("catppuccin")), "a normal bubble keeps the border: {frame:?}");
    sway.click(x, y, "button1");
    assert!(client.wait_heard(&Heard::Invoked(id, "default".into())), "{:?}", d.log());
    assert!(client.wait_heard(&Heard::Closed(id, 2)), "then dismissed");
    assert!(d.wait_for("bubbles surface dropped (empty)", DELAY), "{:?}", d.log());
    let resident = client.notify(&open(Note::new("Chat", "Message from Lina")).hint("resident", Hint::Bool(true))).unwrap();
    assert!(d.wait_for_count("bubbles shown: 1", 2, DELAY), "{:?}", d.log());
    let (x, y) = bubble_at(&sway, "HEADLESS-1");
    sway.click(x, y, "button1");
    assert!(client.wait_heard(&Heard::Invoked(resident, "default".into())), "{:?}", d.log());
    assert!(!client.heard_within(Duration::from_secs(1)).iter().any(|h| matches!(h, Heard::Closed(..))), "resident: the bubble stays");
    assert_eq!(d.count("bubbles surface dropped"), 1, "{:?}", d.log());
}

/// Spec § 3: a right click dismisses (reason 2) and runs nothing.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn a_right_click_dismisses_with_reason_2_and_runs_nothing() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-right", 1, false, |_| ());
    let id = client.notify(&Note::new("Mailer", "Weekly report ready").action("default", "Open").timeout(0)).unwrap();
    assert!(d.wait_for("bubbles shown: 1", DELAY), "{:?}", d.log());
    let (x, y) = bubble_at(&sway, "HEADLESS-1");
    sway.click(x, y, "button3");
    assert!(client.wait_heard(&Heard::Closed(id, 2)), "{:?}", d.log());
    assert!(
        !client.heard_within(Duration::from_millis(500)).iter().any(|h| matches!(h, Heard::Invoked(..))),
        "a right click never runs an action"
    );
    assert!(d.wait_for("bubbles surface dropped (empty)", DELAY), "{:?}", d.log());
}

/// Spec § 3, expiry: `NotificationClosed(1)` at its time, never before, and the surface goes with
/// the last bubble.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn an_expired_bubble_sends_closed_1_and_its_surface_goes() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-expiry", 1, false, |_| ());
    let t = Instant::now();
    let id = client.notify(&Note::new("Mailer", "Weekly report ready").timeout(2_000)).unwrap();
    assert!(d.wait_for("bubbles shown: 1", DELAY), "{:?}", d.log());
    assert!(client.wait_heard(&Heard::Closed(id, 1)), "expired: {:?}", d.log());
    assert!(t.elapsed() >= Duration::from_millis(1_900), "not before its 2 s: {:?}", t.elapsed());
    assert!(d.wait_for("bubbles: 1 expired", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bubbles surface dropped (empty)", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| bubble_box(s, 3).is_none()), "gone from the screen");
}

/// Spec § 3: under do-not-disturb, only critical notifications get a bubble.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn do_not_disturb_shows_only_critical_bubbles() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-dnd", 1, false, dnd_on_before_start);
    client.notify(&Note::new("Mailer", "Weekly report ready")).unwrap();
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("bubbles surface on"), 0, "a normal one goes to the history only: {:?}", d.log());
    let critical = client.notify(&Note::new("Monitor", "Disk almost full").critical()).unwrap();
    assert!(d.wait_for("bubbles shown: 1", DELAY), "a critical one still shows: {:?}", d.log());
    let (x, y) = bubble_at(&sway, "HEADLESS-1");
    let urgent = rgb(preset("catppuccin").unwrap().dark.urgent);
    let frame = bubble_frame(&sway.screenshot("HEADLESS-1").expect("grim"));
    assert_eq!(frame, Some(urgent), "spec § 3: a critical bubble is framed in urgent");
    sway.click(x, y, "button3");
    assert!(client.wait_heard(&Heard::Closed(critical, 2)), "{:?}", d.log());
}

/// Spec § 3 and § 7: 0 loop wake-ups at rest once the bubbles expired, and while a sticky critical
/// bubble shows (no finite expiry: no timer).
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn no_wakeup_at_rest_after_the_bubbles_expire_or_under_a_sticky_one() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-rest", 1, false, |_| ());
    sway.hover(20, 400);
    let id = client.notify(&Note::new("Mailer", "Weekly report ready").timeout(2_000)).unwrap();
    assert!(client.wait_heard(&Heard::Closed(id, 1)), "{:?}", d.log());
    let after = rest_over_5_s(&d);
    assert_eq!(after, 0, "expired: nothing armed ({after} wake-ups): {:?}", d.log());
    client.notify(&Note::new("Monitor", "Disk almost full").critical()).unwrap();
    assert!(d.wait_for_count("bubbles shown: 1", 2, DELAY), "{:?}", d.log());
    let sticky = rest_over_5_s(&d);
    assert_eq!(sticky, 0, "a sticky bubble arms no timer ({sticky} wake-ups)");
}

/// The focused output changes while a bubble shows. The bubble stays where it is until
/// the stack changes; then the stack moves to the focused output.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn the_bubbles_stay_put_when_the_focus_moves_until_the_stack_changes() {
    let (_bus, sway, hypr, d, client) = notify_daemon("bubble-focus", 2, true, |_| ());
    sway.command("output HEADLESS-1 pos 0 0");
    sway.command("output HEADLESS-2 pos 3000 0");
    client.notify(&Note::new("Monitor", "Disk almost full").critical()).unwrap();
    assert!(d.wait_for("bubbles surface on HEADLESS-2", DELAY), "{:?}", d.log());
    hypr.focus("HEADLESS-1");
    assert!(hypr.write("focusedmon>>HEADLESS-1,1\n"));
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("bubbles surface on"), 1, "the focus alone moves nothing: {:?}", d.log());
    // A sync that leaves the stack as it is (do-not-disturb keeps critical bubbles) moves nothing
    // either.
    ready(&d);
    assert_eq!(script(&sway, &["--dnd", "on"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("bubbles surface on"), 1, "the same stack stays put: {:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-2", |s| bubble_box(s, 3).is_some()), "still there");
    client.notify(&Note::new("Monitor", "Backup finished").critical()).unwrap();
    assert!(d.wait_for("bubbles surface on HEADLESS-1", DELAY), "the stack changed: to the focused output: {:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| bubble_box(s, 3).is_some()));
    assert!(sway.wait_for_screen("HEADLESS-2", |s| bubble_box(s, 3).is_none()), "and only there");
}

/// The output holding the bubbles is unplugged. They come back on another output (the
/// compositor's choice: the fixture's focused output is the one gone) and still answer.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn an_unplugged_output_holding_the_bubbles_brings_them_back_elsewhere() {
    let (_bus, sway, _hypr, mut d, client) = notify_daemon("bubble-unplug", 2, true, |_| ());
    sway.command("output HEADLESS-1 pos 0 0");
    sway.command("output HEADLESS-2 pos 3000 0");
    let id = client.notify(&Note::new("Monitor", "Disk almost full").critical()).unwrap();
    assert!(d.wait_for("bubbles surface on HEADLESS-2", DELAY), "{:?}", d.log());
    sway.command("output HEADLESS-2 disable");
    assert!(d.wait_for("bubbles surface on the compositor's choice of output", DELAY), "{:?}", d.log());
    assert_eq!(d.exit(Duration::from_millis(300)), None, "the daemon survives: {:?}", d.log());
    let (x, y) = bubble_at(&sway, "HEADLESS-1");
    sway.click(x, y, "button3");
    assert!(client.wait_heard(&Heard::Closed(id, 2)), "the bubble still answers: {:?}", d.log());
}

/// 4 bubbles shown and more waiting (4 here: 8 critical ones, the most that stay live).
/// Dismissing the top one shows the next waiting one, and the hit map follows the new
/// stack.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn four_shown_four_waiting_a_dismissal_shows_the_next() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-queue", 1, false, |_| ());
    let ids: Vec<u32> = (0..8)
        .map(|n| {
            let summary = format!("Alert {n}");
            client.notify(&Note::new("Monitor", &summary).critical()).unwrap()
        })
        .collect();
    let four = |s: &Ppm| bubble_box(s, 3).is_some_and(|(_, left, _)| bubbles_on(s, left + 3) == 4);
    assert!(sway.wait_for_screen("HEADLESS-1", four), "4 shown: {:?}", d.log());
    let (x, y) = bubble_at(&sway, "HEADLESS-1");
    sway.click(x, y, "button3");
    assert!(client.wait_heard(&Heard::Closed(ids[7], 2)), "the newest is on top: {:?}", d.log());
    thread::sleep(Duration::from_millis(500));
    assert!(sway.wait_for_screen("HEADLESS-1", four), "still 4: the next waiting one shows");
    sway.click(x, y, "button3");
    assert!(client.wait_heard(&Heard::Closed(ids[6], 2)), "the stack and its hit map moved up: {:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", four));
    assert!(
        !client.heard_within(Duration::from_millis(500)).iter().any(|h| matches!(h, Heard::Closed(..))),
        "the waiting ones were never closed"
    );
}

/// The scale changes between 1 and 2 under a bubble. It is drawn again at the new
/// scale, twice as wide in pixels, and back.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn a_bubble_follows_the_scale_from_1_to_2_and_back() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-scale", 1, false, |_| ());
    client.notify(&Note::new("Monitor", "Disk almost full").critical()).unwrap();
    let one = Cell::new(0u32);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| bubble_box(s, 3).map(|(_, l, r)| one.set(r - l)).is_some()), "{:?}", d.log());
    let w1 = one.get();
    sway.command("output HEADLESS-1 scale 2");
    assert!(d.wait_for("bubbles scale 2", DELAY), "{:?}", d.log());
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| bubble_box(s, 6).is_some_and(|(_, l, r)| (r - l).abs_diff(2 * w1) <= 8)),
        "twice as wide at scale 2 (scale 1: {w1} px): {:?}",
        d.log()
    );
    sway.command("output HEADLESS-1 scale 1");
    assert!(d.wait_for("bubbles scale 1", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| bubble_box(s, 3).is_some_and(|(_, l, r)| (r - l).abs_diff(w1) <= 4)), "and back");
}

/// Constraints, Lightness: the timer follows every live bubble with a finite
/// timeout, hidden by do-not-disturb or not. A bubble hidden by DND, then replaced with a 2 s
/// timeout, closes on time with reason 1, and never shows up later past its time.
#[test]
#[ignore = "headless sway, dbus-daemon and grim (nix develop)"]
fn under_dnd_a_hidden_replaced_bubble_still_expires_on_time() {
    let (_bus, sway, _hypr, d, client) = notify_daemon("bubble-dnd-expiry", 1, false, |_| ());
    ready(&d);
    let id = client.notify(&Note::new("Mailer", "Weekly report ready").timeout(20_000)).unwrap();
    assert!(d.wait_for("bubbles shown: 1", DELAY), "{:?}", d.log());
    assert_eq!(script(&sway, &["--dnd", "on"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for("bubbles surface dropped (empty)", DELAY), "hidden by do-not-disturb: {:?}", d.log());
    let t = Instant::now();
    let replaced = client.notify(&Note::new("Mailer", "Weekly report sent").replaces(id).timeout(2_000)).unwrap();
    assert_eq!(replaced, id, "replaced in place, still live");
    assert!(client.wait_heard(&Heard::Closed(id, 1)), "the hidden one expires: {:?}", d.log());
    let at = t.elapsed();
    assert!(at >= Duration::from_millis(1_900) && at < Duration::from_secs(5), "on its new time, not its old 20 s: {at:?}");
    assert_eq!(script(&sway, &["--dnd", "off"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for("bell: dnd off", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("bubbles surface on"), 1, "never shown again once past its time: {:?}", d.log());
}

/// Do-not-disturb reaches the bell through the tray's connection too.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn with_the_tray_dnd_from_alpheus_launch_updates_the_bell() {
    let (_bus, sway, _hypr, d, ()) = bell_daemon("bell-dnd-tray", "modules = [\"tray\", \"notifications\", \"clock\"]\n", |_| ());
    assert!(d.wait_for("notifications: on the tray's session bus", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    ready(&d);
    assert_eq!(script(&sway, &["--dnd", "on"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    assert!(sway.runtime.join("state/alpheus/dnd").exists());
}

/// With no session bus there is no server, but the bell is drawn
/// muted ("not serving"), so its middle click still toggles do-not-disturb; every change is saved,
/// logged and shown (the crossed bell).
#[test]
#[ignore = "headless sway (nix develop)"]
fn without_a_session_bus_the_bell_is_muted_and_still_toggles_dnd() {
    let sway = Sway::start("bell-no-bus", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(BELL_BAR), ..DaemonOptions::default() });
    assert!(d.wait_for("notifications: no session bus", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bell: not serving", DELAY), "the bell drawn muted: {:?}", d.log());
    ready(&d);
    let file = sway.runtime.join("state/alpheus/dnd");
    sway.click(BELL_AT.0, BELL_AT.1, "button2");
    assert!(d.wait_for("notifications: do not disturb on", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bell: dnd on", DELAY), "the crossed bell: {:?}", d.log());
    assert!(file.exists());
    assert_eq!(script(&sway, &["--dnd", "off"], "").join().unwrap().0, EXIT_OK);
    assert!(d.wait_for("notifications: do not disturb off", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("bell: dnd off", 2, DELAY), "{:?}", d.log());
    assert!(!file.exists());
    assert_eq!(d.count("bell: serving"), 0, "never serving: {:?}", d.log());
}

// ---- The panel ----

/// The bell at the bar's left edge, the media module after it, the clock.
const PANEL_BAR: &str = "modules = [\"notifications\", \"media\", \"clock\"]\n";

/// The panel's bar on a private session bus with one fake player playing "Paper Lanterns"
/// (fictional), serving notifications, a client connected; the bell and the playing module drawn.
fn panel_daemon(name: &str) -> (PrivateBus, Sway, FakeHyprland, Daemon, FakeClient, FakePlayer) {
    let bus = PrivateBus::start(name);
    let player = FakePlayer::start(&bus, PlayerSpec::playing("lumen", "Paper Lanterns"));
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    let d =
        start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(PANEL_BAR), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    wait_for_the_server(&bus);
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    assert!(player.wait_call("GetAll", DELAY).is_some(), "the daemon read the fake player: {:?}", d.log());
    // Read and drawn: the playing module's glyph in the purple (catppuccin dark), after the bell.
    let purple = rgb(preset("catppuccin").unwrap().dark.hues.purple);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| media_cells(s).contains(&purple)), "never drawn: {:?}", d.log());
    let client = FakeClient::connect(&bus);
    thread::sleep(Duration::from_millis(300));
    (bus, sway, hypr, d, client, player)
}

/// The bar's cells right of the bell (where its count shows), above any popup.
fn after_the_bell(s: &Ppm) -> Vec<[u8; 3]> {
    (2..20).flat_map(|y| (17..60).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect()
}

/// Spec § 4: a bell click opens the panel with the media card and the history. Delete removes an
/// entry; Clear all empties the history and closes the live bubbles; the DND row toggles the file.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_bell_opens_the_panel_with_the_card_and_the_history() {
    let (_bus, sway, _hypr, d, client, _player) = panel_daemon("panel-open");
    let old = client.notify(&Note::new("Mailer", "Weekly report ready").timeout(2_000)).unwrap();
    let live = client.notify(&Note::new("Monitor", "Disk almost full").critical()).unwrap();
    assert!(client.wait_heard(&Heard::Closed(old, 1)), "the first one expired: history only: {:?}", d.log());
    sway.click(BELL_AT.0, BELL_AT.1, "button1");
    assert!(d.wait_for("panel shown (entries: 2, card: yes)", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    // The newest (the live one) is selected at open: Down to the older one, then Delete.
    keys(&sway, &["Down", "Delete"]);
    assert!(d.wait_for("panel: entry removed", DELAY), "{:?}", d.log());
    assert!(d.wait_for("panel: entries 1", DELAY), "{:?}", d.log());
    keys(&sway, &["End", "Return"]);
    assert!(d.wait_for("panel: cleared", DELAY), "{:?}", d.log());
    assert!(client.wait_heard(&Heard::Closed(live, 2)), "Clear all closes the live bubble, dismissed: {:?}", d.log());
    assert!(d.wait_for("bubbles surface dropped (empty)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("panel: entries 0", DELAY), "{:?}", d.log());
    let dnd = sway.runtime.join("state/alpheus/dnd");
    assert!(!dnd.exists());
    keys(&sway, &["Up", "Return"]); // from Clear all to Do not disturb
    assert!(wait_until(|| dnd.exists()), "DND on, persisted: {:?}", d.log());
    assert!(d.wait_for("panel: do not disturb on", DELAY), "{:?}", d.log());
    keys(&sway, &["Return"]);
    assert!(wait_until(|| !dnd.exists()), "DND off: the file goes: {:?}", d.log());
    assert!(d.wait_for("panel: do not disturb off", DELAY), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
}

/// The panel is open when a notification arrives. Its rows show it at once, and it
/// counts as read: the bell shows no count while the panel shows it.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn a_notification_arriving_while_the_panel_is_open_shows_in_it_read() {
    let (_bus, sway, _hypr, d, client, _player) = panel_daemon("panel-arrival");
    // `before`: the bar with no notification yet; `unread`: showing the count; `read`: once the
    // panel marked it read.
    let before = sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap();
    client.notify(&Note::new("Mailer", "Weekly report ready").timeout(2_000)).unwrap();
    assert!(sway.wait_for_screen("HEADLESS-1", |s| after_the_bell(s) != before), "the count shows: {:?}", d.log());
    thread::sleep(Duration::from_millis(300));
    let unread = sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap();
    sway.click(BELL_AT.0, BELL_AT.1, "button1");
    assert!(d.wait_for("panel shown (entries: 1, card: yes)", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| after_the_bell(s) != unread), "opening marks it read: the count goes");
    wait_for_the_popup_drawn(&sway, &d);
    let read = sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap();
    client.notify(&Note::new("Calendar", "Standup in 5 minutes").timeout(2_000)).unwrap();
    assert!(d.wait_for("panel: entries 2", DELAY), "the rows follow at once: {:?}", d.log());
    thread::sleep(Duration::from_millis(500));
    assert_eq!(sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap(), read, "shown in the open panel: read, no count");
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
}

/// Mae's keyboard crash (see `a_keyboard_released_after_its_repeat_delay_keeps_the_bar_alive`),
/// through the panel, whose Down then a click outside is its likeliest trigger: (a) the panel
/// closed by an outside click past Down's repeat delay, wtype still alive; (b) reopened, Down,
/// wtype gone (`keyboard_lost`), then the outside click.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_panel_closed_after_a_repeat_delay_keeps_the_bar_alive() {
    let (_bus, sway, _hypr, mut d, _client, _player) = panel_daemon("panel-keyboard");
    sway.click(BELL_AT.0, BELL_AT.1, "button1");
    assert!(d.wait_for("panel shown", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    let held = wtype_spawn(&sway, &["-s", "300", "-k", "Down", "-s", "2500"]);
    thread::sleep(Duration::from_secs(1));
    sway.click(640, 400, "button1");
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    assert!(d.wait_for("keyboard released", DELAY), "{:?}", d.log());
    survives(&mut d, "the panel closed with wtype alive");
    let _ = held.wait();
    sway.click(BELL_AT.0, BELL_AT.1, "button1");
    assert!(d.wait_for_count("panel shown", 2, DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    assert!(wtype(&sway, &["-s", "300", "-k", "Down"]).success());
    thread::sleep(Duration::from_millis(1500));
    sway.click(640, 400, "button1");
    assert!(d.wait_for_count("popup closed", 2, DELAY), "{:?}", d.log());
    survives(&mut d, "the panel closed after wtype left");
}

/// Spec § 4: the media card's 1 s tick runs while the panel shows it and the player plays, and
/// stops with the panel, its Seeked rule too.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn the_panel_ticks_only_while_its_card_plays() {
    let (_bus, sway, _hypr, d, _client, player) = panel_daemon("panel-tick");
    sway.hover(640, 400);
    assert_eq!(rest_over_5_s(&d), 0, "closed: nothing ticks: {:?}", d.log());
    sway.click(BELL_AT.0, BELL_AT.1, "button1");
    assert!(d.wait_for("panel shown (entries: 0, card: yes)", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    sway.hover(640, 400);
    let open = wakeups_over_5_s(&d);
    assert!((4..=12).contains(&open), "about one tick a second with the card playing: {open}");
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    // SCTK's key repeat timer, armed at Escape's press, fires once at the repeat delay (600 ms on
    // sway) and drops itself, as in the media popup's tick test.
    thread::sleep(Duration::from_secs(1));
    let after = rest_over_5_s(&d);
    assert_eq!(after, 0, "closed: the tick went with it ({after} wake-ups)");
    let seeked = wakeups_over_seeked(&d, &player);
    assert_eq!(seeked, 0, "closed: the Seeked rule went with the panel ({seeked} wake-ups)");
}

/// No player, so no card and no tick to refresh the panel: opening it marks read at once, and an
/// arrival while it is open shows in it (the host's output refreshes it) and stays read.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn without_a_card_opening_and_arrivals_mark_read_at_once() {
    no_card_marks_read("panel-no-card", BELL_BAR);
}

/// The same with the tray, as Mae runs it: the server then lives on the tray's connection, and the
/// tray's output (not the link's) is what refreshes the open panel.
#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn with_the_tray_arrivals_refresh_the_open_panel() {
    no_card_marks_read("panel-no-card-tray", "modules = [\"notifications\", \"tray\", \"clock\"]\n");
}

/// Waits until the panel is on screen with its newest entry selected: the selection band in its
/// first row (catppuccin dark, the default with no theme file).
fn wait_for_the_panel_drawn(sway: &Sway, d: &Daemon) {
    let band = rgb(preset("catppuccin").unwrap().dark.selection);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| popup_title(s).contains(&band)), "the panel drawn: {:?}", d.log());
}

fn no_card_marks_read(name: &str, modules: &str) {
    let (bus, sway, _hypr, d, ()) = bell_daemon(name, modules, |_| ());
    wait_for_the_server(&bus);
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    // The bell drawn: something other than the bar's background left of its count.
    let drawn = |s: &Ppm| {
        let bg = s.at(BAR_BACKGROUND.0, BAR_BACKGROUND.1);
        (2..20).flat_map(|y| (4..17).map(move |x| (x, y))).any(|(x, y)| s.at(x, y) != bg)
    };
    assert!(sway.wait_for_screen("HEADLESS-1", drawn), "the bell never drawn: {:?}", d.log());
    let client = FakeClient::connect(&bus);
    let before = sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap();
    // Critical: sticky, so no expiry refreshes anything behind the test's back.
    client.notify(&Note::new("Monitor", "Disk almost full").critical()).unwrap();
    assert!(sway.wait_for_screen("HEADLESS-1", |s| after_the_bell(s) != before), "the count shows: {:?}", d.log());
    thread::sleep(Duration::from_millis(300));
    let unread = sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap();
    sway.click(BELL_AT.0, BELL_AT.1, "button1");
    assert!(d.wait_for("panel shown (entries: 1, card: no)", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| after_the_bell(s) != unread), "opening marks it read: the count goes");
    wait_for_the_panel_drawn(&sway, &d);
    thread::sleep(Duration::from_millis(300));
    let read = sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap();
    client.notify(&Note::new("Calendar", "Standup in 5 minutes").critical()).unwrap();
    assert!(d.wait_for("panel: entries 2", DELAY), "the host's output refreshes the open panel: {:?}", d.log());
    thread::sleep(Duration::from_millis(500));
    assert_eq!(sway.screenshot("HEADLESS-1").map(|s| after_the_bell(&s)).unwrap(), read, "shown in the open panel: read, no count");
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
}

/// The `Fifo` fake prints the initial dump, then each array a test sends, in order, and
/// once killed nothing is left reading the pipe (it `exec`s its `cat`: the daemon's PDEATHSIG
/// reaches it).
#[test]
fn the_fifo_fake_pw_dump_prints_the_initial_dump_then_each_send() {
    // Declared first, so dropped last: removed after the child's group is gone, even on a failure.
    let tmp = TempDir::new("alpheus-it-pw-fifo");
    let dir = tmp.0.clone();
    let bin = fake_pw_dump_in(&dir, PwDump::Fifo);
    // Reaped with its whole group even when an assert below fails: a `cat` orphaned by a failure
    // would never exit, and would hold the caller's pipe (`cargo test | tail` hanging).
    let mut child = GroupChild::spawn(Command::new(bin.join("pw-dump")).stdout(Stdio::piped()));
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = vec![0u8; 1 << 16];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let mut read_until = |needle: &str| -> usize {
        let t = Instant::now();
        loop {
            if let Some(at) = String::from_utf8_lossy(&seen).find(needle) {
                return at;
            }
            assert!(t.elapsed() < DELAY, "the fake never printed {needle}");
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(100)) {
                seen.extend(b);
            }
        }
    };
    read_until(r#""id":123,"#);
    let (first, second) = (volume_delta(AG06_SINK, 90, false), default_sink_delta(SPEAKER_SINK));
    pw_dump_send(&dir, &first);
    pw_dump_send(&dir, &second);
    let (a, b) = (read_until(&first), read_until(&second));
    assert!(a < b, "in the order sent");
    // The leader only, as the daemon's PDEATHSIG does: what this test proves is that nothing else
    // of the fake (no un-exec'd `cat`) is left reading the pipe. The guard's group kill comes after,
    // as cleanup (an empty group by then when the fake is right).
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(child.reaped(), "reaped by the test: the guard never signals its group again");
    use std::os::unix::fs::OpenOptionsExt;
    let left = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(dir.join("pw-dump.fifo"));
    assert_eq!(left.err().and_then(|e| e.raw_os_error()), Some(libc::ENXIO), "nothing reads the pipe once the fake is gone");
}

/// The harness's guards: a `GroupChild` the test reaped is never signalled
/// again (its pid may be reused); one never waited for is killed with its group and reaped on drop.
/// A `TempDir` is removed on drop.
#[test]
fn a_reaped_group_child_is_left_alone_and_a_temp_dir_goes_with_its_guard() {
    let tmp = TempDir::new("alpheus-it-guards");
    let dir = tmp.0.clone();
    let mut done = GroupChild::spawn(&mut Command::new("true"));
    assert!(!done.reaped());
    done.wait().unwrap();
    assert!(done.reaped(), "wait reaps");
    drop(done);
    let mut quick = GroupChild::spawn(&mut Command::new("true"));
    while quick.try_wait().unwrap().is_none() {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(quick.reaped(), "try_wait reaps once it exited");
    let mut left = GroupChild::spawn(Command::new("sleep").arg("30"));
    assert!(left.try_wait().unwrap().is_none() && !left.reaped());
    let pid = left.id() as i32;
    let t = Instant::now();
    drop(left);
    assert!(t.elapsed() < Duration::from_secs(5), "killed, not waited for: {:?}", t.elapsed());
    // SAFETY: signal 0 only checks that the pid exists; ours until reaped, reaped by the drop.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "killed and reaped by the guard");
    drop(tmp);
    assert!(!dir.exists(), "the temp dir went with its guard");
}

/// A send above PIPE_BUF would not be atomic: refused loudly, before the pipe is even opened.
#[test]
#[should_panic(expected = "PIPE_BUF")]
fn a_pw_dump_send_above_pipe_buf_fails_clearly() {
    pw_dump_send(Path::new("/nonexistent-alpheus-it"), &"x".repeat(4096));
}

// ---- The OSD (osd plan). The fake pw-dump is `Fifo`; the backlight a fake tree
// under the test's runtime directory, never /sys. ----

/// The OSD's tests' bar: the volume (its sound source), the OSD, the clock. `extra`: more `[osd]`
/// lines.
fn osd_bar(sway: &Sway, extra: &str) -> String {
    format!(
        "modules = [\"workspaces\", \"volume\", \"osd\", \"clock\"]\n[osd]\nbacklight_dir = \"{}\"\n{extra}",
        sway.runtime.join("backlight").display()
    )
}

/// After the baseline, past the start-up grace (OSD spec § 2: 1 s).
const GRACE_OVER: Duration = Duration::from_millis(1300);

/// A daemon on `outputs` headless outputs (the fixture's focus on HEADLESS-2 when there are two),
/// catppuccin dark, the `Fifo` fake pw-dump. Returned once the OSD took its sound baseline and the
/// grace is over. `before`: what the test puts in place first (a fake backlight).
fn osd_daemon(name: &str, outputs: u32, extra: &str, before: impl FnOnce(&Sway)) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, outputs);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    before(&sway);
    let toml = osd_bar(&sway, extra);
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(&toml), pw_dump: PwDump::Fifo, ..DaemonOptions::default() });
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    assert!(d.wait_for("osd: sound baseline", DELAY), "{:?}", d.log());
    thread::sleep(GRACE_OVER);
    (sway, hypr, d)
}

/// The pill on `s` in the colour `bg`: its gap to the bottom edge (px below its last background
/// row, down the output's centre column) and its leftmost and rightmost background pixels 3 px
/// above that row (inside its bottom inset, where no glyph is). None: no pill on that column.
fn osd_box_in(s: &Ppm, bg: [u8; 3]) -> Option<(u32, u32, u32)> {
    let cx = s.width / 2;
    let bottom = (0..s.height).rev().find(|&y| s.at(cx, y) == Some(bg))?;
    let y = bottom.checked_sub(3)?;
    let (mut left, mut right) = (cx, cx);
    while left > 0 && s.at(left - 1, y) == Some(bg) {
        left -= 1;
    }
    while s.at(right + 1, y) == Some(bg) {
        right += 1;
    }
    Some((s.height - 1 - bottom, left, right))
}

fn osd_box(s: &Ppm) -> Option<(u32, u32, u32)> {
    osd_box_in(s, popup_bg("catppuccin"))
}

/// OSD spec § 1, § 3: a volume change shows the pill bottom centre, `margin` px above the edge, on
/// the focused output only; it goes `timeout_ms` after the change.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_a_volume_change_shows_the_pill_bottom_centre_then_it_goes() {
    let (sway, _hypr, d) = osd_daemon("osd-place", 2, "", |_| ());
    sway.command("output HEADLESS-1 pos 0 0");
    sway.command("output HEADLESS-2 pos 3000 0");
    for o in ["HEADLESS-1", "HEADLESS-2"] {
        assert_eq!(osd_box(&sway.screenshot(o).expect("grim")), None, "{o}: the checks bite only if no pill is there yet");
    }
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    assert!(d.wait_for("osd surface on HEADLESS-2: volume 90%", DELAY), "the fixture's focused output: {:?}", d.log());
    let shown = Instant::now();
    assert!(
        sway.wait_for_screen("HEADLESS-2", |s| osd_box(s)
            .is_some_and(|(gap, l, r)| (48..=52).contains(&gap) && (l + r).abs_diff(s.width) <= 4)),
        "48 px above the bottom edge (plus the frame's stroke), centred: {:?}",
        d.log()
    );
    assert_eq!(osd_box(&sway.screenshot("HEADLESS-1").expect("grim")), None, "nothing on the other output");
    assert!(d.wait_for("osd surface dropped (timeout, 1 shown)", DELAY), "{:?}", d.log());
    let after = shown.elapsed();
    assert!(after >= Duration::from_millis(1000) && after <= Duration::from_millis(2500), "gone {after:?} after it showed (timeout 1.3 s)");
    assert!(sway.wait_for_screen("HEADLESS-2", |s| osd_box(s).is_none()), "gone from the screen too");
}

/// OSD spec § 2: the state pw-dump starts with is the baseline; nothing shows.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_the_start_up_state_shows_nothing() {
    let (sway, _hypr, d) = osd_daemon("osd-start", 1, "", |_| ());
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("osd surface on"), 0, "the first dump is the baseline: {:?}", d.log());
    assert_eq!(osd_box(&sway.screenshot("HEADLESS-1").expect("grim")), None);
}

/// OSD spec § 3: a held key (30 changes at 25 Hz) keeps ONE pill up, its content replaced, then it
/// goes once, `timeout_ms` after the last change.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_a_held_key_keeps_one_pill_up_until_the_last_change() {
    let (sway, _hypr, d) = osd_daemon("osd-held", 1, "", |_| ());
    for n in 0..30u32 {
        pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 40 + n, false));
        thread::sleep(Duration::from_millis(40));
    }
    let last = Instant::now();
    assert!(d.wait_for("osd surface dropped (timeout, 30 shown)", DELAY), "{:?}", d.log());
    let gone = last.elapsed();
    assert!(gone >= Duration::from_millis(1000), "it stayed {gone:?} after the last change (timeout 1.3 s)");
    assert_eq!(d.count("osd surface on"), 1, "one surface for the whole hold: {:?}", d.log());
}

/// OSD spec § 3: the input region is empty. A click on the pill never reaches it, and leaves it
/// alone.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_the_pill_never_takes_a_click() {
    let (sway, _hypr, d) = osd_daemon("osd-click", 1, "timeout_ms = 5000\n", |_| ());
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    let found = Cell::new(None);
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| {
            found.set(osd_box(s).map(|b| (b, s.height)));
            found.get().is_some()
        }),
        "{:?}",
        d.log()
    );
    let ((gap, left, right), height) = found.get().unwrap();
    let (x, y) = ((left + right) / 2, height - 1 - gap - 10);
    sway.hover(x, y);
    sway.click(x, y, "button1");
    sway.click(x, y, "button3");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("osd: a pointer event reached the pill"), 0, "its input region is empty: {:?}", d.log());
    assert_eq!(d.count("osd surface dropped"), 0, "and the clicks left it alone");
}

/// OSD spec § 1, § 2: the microphone's mute shows its pill, its level never does. A switch of the default sink shows its
/// name and level, the fixture's speaker at 94 %.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_the_microphone_and_a_default_sink_switch_show_their_pills() {
    // The pill's timeout is not what this test measures: a long one keeps it on screen while it looks.
    let (sway, _hypr, d) = osd_daemon("osd-mic", 1, "timeout_ms = 5000\n", |_| ());
    // Mae (2026-10-09): a call's gain control moving the microphone's level shows nothing.
    for level in [80, 65, 90] {
        pw_dump_send(&sway.runtime, &volume_delta(AG06_SOURCE, level, false));
    }
    thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("osd surface on"), 0, "a microphone level alone: {:?}", d.log());
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SOURCE, 100, true));
    assert!(d.wait_for("mic 100% muted", DELAY), "{:?}", d.log());
    assert!(d.wait_for("osd surface dropped (timeout, 1 shown)", DELAY), "{:?}", d.log());
    pw_dump_send(&sway.runtime, &default_sink_delta(SPEAKER_SINK));
    assert!(d.wait_for("default sink changed, 94%", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| osd_box(s).is_some()), "{:?}", d.log());
}

/// OSD spec § 3: a change while the pill shows repaints its level on the same surface. The lit
/// part of the bar is counted in the accent colour, below the screen's middle: the bar's own volume
/// gauge, at the top, is never counted.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_a_change_while_shown_repaints_the_level() {
    let (sway, _hypr, d) = osd_daemon("osd-level", 1, "timeout_ms = 5000\n", |_| ());
    let accent = rgb(preset("catppuccin").unwrap().dark.accent);
    let lit = |s: &Ppm| -> usize { (s.height / 2..s.height).map(|y| (0..s.width).filter(|&x| s.at(x, y) == Some(accent)).count()).sum() };
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 10, false));
    let low = Cell::new(0);
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| {
            low.set(lit(s));
            osd_box(s).is_some() && low.get() > 0
        }),
        "{:?}",
        d.log()
    );
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 100, false));
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| lit(s) > 3 * low.get()),
        "10 % lights 2 of 25 cells, 100 % all of them: {} px lit at 10 %: {:?}",
        low.get(),
        d.log()
    );
    assert_eq!(d.count("osd surface on"), 1, "the same surface");
}

/// OSD spec § 3: a theme change while the pill shows repaints it in place.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_a_theme_change_repaints_the_shown_pill() {
    let (sway, _hypr, d) = osd_daemon("osd-theme", 1, "timeout_ms = 5000\n", |_| ());
    let config = sway.runtime.join("config/alpheus");
    let (old, new) = (popup_bg("catppuccin"), popup_bg("gruvbox"));
    assert_ne!(old, new);
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| osd_box_in(s, old).is_some()), "{:?}", d.log());
    write_atomically(&config, "theme.toml", "theme = \"gruvbox\"\nmode = \"dark\"\n");
    assert!(d.wait_for("theme reloaded: gruvbox, dark", DELAY), "{:?}", d.log());
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| osd_box_in(s, new).is_some() && osd_box_in(s, old).is_none()),
        "repainted: {:?}",
        d.log()
    );
    assert_eq!(d.count("osd surface on"), 1, "in place, never recreated");
}

/// OSD spec, success criteria: at rest nothing is mapped and the loop never wakes, before any pill
/// and once one went (the CPU checked too: a spin makes no wake-up).
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_no_wakeup_at_rest_before_or_after_a_pill() {
    let (sway, _hypr, d) = osd_daemon("osd-rest", 1, "", |_| ());
    sway.hover(20, 400);
    let before = rest_over_5_s(&d);
    assert_eq!(before, 0, "nothing shown yet ({before} wake-ups): {:?}", d.log());
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    assert!(d.wait_for("osd surface dropped (timeout", DELAY), "{:?}", d.log());
    let after = rest_over_5_s(&d);
    assert_eq!(after, 0, "the pill gone: no timer left ({after} wake-ups): {:?}", d.log());
}

/// Without `osd` in `modules`, the OSD is never fed: no baseline, no pill, though the
/// volume module's sound reaches the bar (its gauge repainted: the messages did get through).
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_off_never_reads_the_sound() {
    let sway = Sway::start("osd-off", 1);
    let _hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    // The volume alone: nothing else on the bar may change its pixels (no clock).
    let toml = "modules = [\"volume\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), pw_dump: PwDump::Fifo, ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let band =
        |s: &Ppm| -> Vec<Option<[u8; 3]>> { (0..20).flat_map(|y| (0..s.width).map(move |x| (x, y))).map(|(x, y)| s.at(x, y)).collect() };
    // The initial dump drawn (the gauge's ink on the bar), and past what would be the OSD's
    // start-up grace.
    let bg = bar_bg("catppuccin");
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| band(s).iter().any(|p| p.is_some_and(|p| p != bg))),
        "the initial gauge is drawn: {:?}",
        d.log()
    );
    thread::sleep(GRACE_OVER);
    let before = sway.screenshot("HEADLESS-1").expect("grim");
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| band(s) != band(&before)), "the bar's gauge moved: {:?}", d.log());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("osd: sound baseline"), 0, "the OSD heard nothing: {:?}", d.log());
    assert_eq!(d.count("osd surface"), 0, "{:?}", d.log());
    assert_eq!(osd_box(&sway.screenshot("HEADLESS-1").expect("grim")), None);
}

/// A fake backlight device under the test's runtime directory (`osd_bar`'s `backlight_dir`), as
/// sysfs shows it: a number and a newline in each file.
fn fake_backlight(sway: &Sway, name: &str, brightness: u32, max: u32) -> PathBuf {
    let dir = sway.runtime.join("backlight").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("brightness"), format!("{brightness}\n")).unwrap();
    std::fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
    dir
}

fn osd_brightness(sway: &Sway) -> i32 {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let args = [OsString::from("--osd"), OsString::from(OsdVerb::Brightness.word())];
    run(&args, &socket_path(&sway.runtime), &mut std::io::empty(), &mut out, &mut err)
}

/// OSD spec § 2: `alpheus-launch --osd brightness` reads the fake backlight once and shows its
/// level; the files are never written.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_the_brightness_verb_reads_the_fake_backlight_and_shows_the_pill() {
    // The pill's timeout is not what this test measures: a long one keeps it on screen while it looks.
    let (sway, _hypr, d) = osd_daemon("osd-bright", 1, "timeout_ms = 5000\n", |sway| {
        fake_backlight(sway, "intel_backlight", 9696, 19393);
    });
    let dir = sway.runtime.join("backlight/intel_backlight");
    let before = std::fs::metadata(dir.join("brightness")).unwrap().modified().unwrap();
    assert_eq!(osd_brightness(&sway), EXIT_OK);
    assert!(d.wait_for("brightness 50%", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| osd_box(s).is_some()), "{:?}", d.log());
    assert_eq!(std::fs::read_to_string(dir.join("brightness")).unwrap(), "9696\n", "read, never written");
    assert_eq!(std::fs::metadata(dir.join("brightness")).unwrap().modified().unwrap(), before);
}

/// OSD spec § 3: a volume change while the brightness pill shows replaces its content on the same
/// surface and pushes its deadline back: it goes `timeout_ms` after the volume change, not after
/// the brightness. The volume lights more of the bar (90 % against 50 %), counted in the accent
/// below the screen's middle (the bar's own gauge, at the top, is never counted).
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_a_volume_change_replaces_the_brightness_pill_and_pushes_its_deadline_back() {
    let (sway, _hypr, d) = osd_daemon("osd-replace", 1, "", |sway| {
        fake_backlight(sway, "intel_backlight", 9696, 19393);
    });
    let accent = rgb(preset("catppuccin").unwrap().dark.accent);
    let lit = |s: &Ppm| -> usize { (s.height / 2..s.height).map(|y| (0..s.width).filter(|&x| s.at(x, y) == Some(accent)).count()).sum() };
    let asked = Instant::now();
    assert_eq!(osd_brightness(&sway), EXIT_OK);
    assert!(d.wait_for(": brightness 50%", DELAY), "{:?}", d.log());
    let brightness = Cell::new(0);
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| {
            brightness.set(lit(s));
            osd_box(s).is_some() && brightness.get() > 0
        }),
        "{:?}",
        d.log()
    );
    // Well inside the brightness pill's 1.3 s, late enough that its own deadline comes first.
    thread::sleep(Duration::from_millis(700).saturating_sub(asked.elapsed()));
    let changed = Instant::now();
    assert!(changed - asked < Duration::from_millis(1100), "the brightness pill may be gone already ({:?})", changed - asked);
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| lit(s) * 2 > brightness.get() * 3),
        "90 % lights more than 50 %: {} px lit at 50 %: {:?}",
        brightness.get(),
        d.log()
    );
    assert!(d.wait_for("osd surface dropped (timeout, 2 shown)", DELAY), "{:?}", d.log());
    let gone = changed.elapsed();
    assert!(gone >= Duration::from_millis(1000), "it went {gone:?} after the volume change (timeout 1.3 s): not pushed back");
    assert_eq!(d.count("osd surface on"), 1, "one surface, its content replaced: {:?}", d.log());
}

/// OSD spec § 4: with no backlight device, or without the module, the verb answers Done (exit 0)
/// and nothing shows.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn osd_without_a_backlight_or_the_module_the_verb_exits_0_and_shows_nothing() {
    let (sway, _hypr, d) = osd_daemon("osd-nobright", 1, "", |_| ());
    assert_eq!(osd_brightness(&sway), EXIT_OK, "no backlight device");
    assert!(d.wait_for("osd: no backlight device, nothing shown", DELAY), "{:?}", d.log());
    let off = Sway::start("osd-bright-off", 1);
    let _hypr_off = FakeHyprland::start_on_headless(&off.runtime, SIGNATURE);
    fake_backlight(&off, "intel_backlight", 9696, 19393);
    let toml = format!("modules = [\"workspaces\", \"clock\"]\n[osd]\nbacklight_dir = \"{}\"\n", off.runtime.join("backlight").display());
    let d_off = start_daemon_with(&off, &DaemonOptions { bar_toml: Some(&toml), ..DaemonOptions::default() });
    assert!(d_off.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d_off.log());
    assert_eq!(osd_brightness(&off), EXIT_OK, "the module off: exit 0 all the same");
    assert!(d_off.wait_for("the osd module is not in modules", DELAY), "{:?}", d_off.log());
    thread::sleep(Duration::from_millis(300));
    assert_eq!((d.count("osd surface on"), d_off.count("osd surface on")), (0, 0), "nothing shown");
}

const QUICK_BAR: &str = "modules = [\"volume\", \"quick\", \"clock\"]\n";
/// A press on the volume module: the first one, from cell 1.
const VOLUME_AT: (u32, u32) = (16, 10);

/// The quick panel's bar (catppuccin dark): the volume and the panel, a private system bus with no
/// power-profiles-daemon (the D-Bus source settles instead of retrying), the `Fifo` fake pw-dump,
/// fake `wpctl`, `brightnessctl` and `alpheus-toggle` recording their calls, a fake backlight.
fn quick_daemon(name: &str, extra: &str) -> (PrivateBus, Sway, FakeHyprland, Daemon) {
    quick_daemon_on(name, QUICK_BAR, extra)
}

/// `quick_daemon` with its own `modules` line.
fn quick_daemon_on(name: &str, modules: &str, extra: &str) -> (PrivateBus, Sway, FakeHyprland, Daemon) {
    let bus = PrivateBus::start(name);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    for program in ["wpctl", "brightnessctl", "alpheus-toggle"] {
        fake_recorder(&sway, program);
    }
    fake_backlight(&sway, "intel_backlight", 9696, 19393);
    let toml = format!("{modules}[osd]\nbacklight_dir = \"{}\"\n{extra}", sway.runtime.join("backlight").display());
    let options =
        DaemonOptions { bar_toml: Some(&toml), system_bus: Some(&bus.address), pw_dump: PwDump::Fifo, ..DaemonOptions::default() };
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(500)); // pw-dump's initial state read, the bar drawn
    (bus, sway, hypr, d)
}

/// `Mixer…`, three rows under the volume: Enter runs `[quick] mixer` and closes the panel; the log
/// names the row, never the command.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_the_mixer_row_runs_its_command_and_closes_the_panel() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-mixer", "[quick]\nmixer = [\"fake-mixer\", \"--flag\"]\n");
    fake_recorder(&sway, "fake-mixer");
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("quick shown (rows: volume brightness mic mixer session; sinks: 3)", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    keys(&sway, &["Down", "Down", "Down", "Return"]);
    assert!(wait_for_call(&sway, "fake-mixer", "fake-mixer --flag"), "{:?} {:?}", recorded(&sway, "fake-mixer"), d.log());
    assert!(d.wait_for("quick: mixer", DELAY), "{:?}", d.log());
    assert!(d.wait_for("popup closed (activated)", DELAY), "{:?}", d.log());
    assert!(!d.log().iter().any(|l| l.contains("fake-mixer")), "the command never reaches the log: {:?}", d.log());
}

/// Waits until `program` recorded `call`.
fn wait_for_call(sway: &Sway, program: &str, call: &str) -> bool {
    wait_until(|| recorded(sway, program).iter().any(|c| c == call))
}

/// The pixels of the panel's volume bar (row 0, its 22 cells), the panel hanging from cell 1.
fn quick_volume_bar(s: &Ppm) -> Vec<[u8; 3]> {
    let ((x0, y), (x1, _)) = (popup_cell(0, u32::from(BAR_START), 0), popup_cell(0, u32::from(BAR_START + BAR - 1), 0));
    (y - 8..y + 8).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect()
}

/// Quick spec § 1, § 3, § 7: a left click on the volume opens the panel at the volume row; → sets
/// the volume through wpctl (84 % + 5, fix S1: an absolute level), then ↓ → the brightness through
/// brightnessctl (50 % + 5), on the configured device.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_a_click_on_the_volume_opens_the_panel_and_the_arrows_reach_wpctl_and_brightnessctl() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-open", "");
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("quick shown (rows: volume brightness mic mixer session; sinks: 3)", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    keys(&sway, &["Right"]);
    assert!(
        wait_for_call(&sway, "wpctl", "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 89%"),
        "{:?} {:?}",
        recorded(&sway, "wpctl"),
        d.log()
    );
    // Fix S1: two → 40 ms apart, inside one window (the fake wpctl moves nothing: from 84 % again
    // once the last window ended): the first goes at once, the second waits, and the timer sends it,
    // 94 %. (wtype's first key needs the lead-in `keys` gives it too.)
    thread::sleep(Duration::from_millis(400));
    assert!(wtype(&sway, &["-s", "300", "-k", "Right", "-s", "40", "-k", "Right"]).success());
    assert!(
        wait_for_call(&sway, "wpctl", "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 94%"),
        "{:?} {:?}",
        recorded(&sway, "wpctl"),
        d.log()
    );
    keys(&sway, &["Down", "Right"]);
    let call = "brightnessctl --class=backlight --device=intel_backlight set 55%";
    assert!(wait_for_call(&sway, "brightnessctl", call), "{:?} {:?}", recorded(&sway, "brightnessctl"), d.log());
    assert!(d.wait_for("quick: brightness 55%", DELAY), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
}

/// Spec § 7: the outputs detail lists two sinks (the HDMI output unplugged first); Enter on the one
/// above the default records `wpctl set-default <its id>`.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_the_outputs_detail_lists_two_sinks_and_enter_sets_the_default() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-outputs", "");
    pw_dump_send(&sway.runtime, r#"[{"id":64,"info":null}]"#);
    thread::sleep(Duration::from_millis(500)); // applied by the sound thread: no publication to wait for
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("quick shown (rows: volume brightness mic mixer session; sinks: 2)", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    keys(&sway, &["Return", "Up", "Return"]);
    assert!(
        wait_for_call(&sway, "wpctl", "wpctl set-default 99"),
        "the speaker, above the AG06: {:?} {:?}",
        recorded(&sway, "wpctl"),
        d.log()
    );
    assert!(d.wait_for("quick: default sink 99", DELAY), "{:?}", d.log());
}

/// Spec § 1, § 7: a pw-dump volume change made elsewhere repaints the open panel.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_a_volume_change_elsewhere_repaints_the_open_panel() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-repaint", "");
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    let before = quick_volume_bar(&sway.screenshot("HEADLESS-1").expect("grim"));
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 30, false));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| quick_volume_bar(s) != before), "84 % to 30 %: the bar repainted: {:?}", d.log());
}

/// The panel's lines below the bar (the first six, its whole width), hanging from cell 1.
fn quick_panel(s: &Ppm) -> Vec<[u8; 3]> {
    let ((x0, y0), (x1, y1)) = (popup_cell(0, 0, 0), popup_cell(0, u32::from(alpheus_demon::quick::WIDTH) + 1, 5));
    (y0 - 11..y1 + 11).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect()
}

/// Without the `osd` module the sound thread never publishes a
/// microphone mute alone; the panel reads the state at open, so a mute made while it was closed
/// shows at the next open, never the stale row.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_a_microphone_muted_while_closed_shows_at_the_next_open() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-mic", "");
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("quick shown (rows: volume brightness mic mixer session", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    let before = quick_panel(&sway.screenshot("HEADLESS-1").expect("grim"));
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SOURCE, 100, true));
    thread::sleep(Duration::from_millis(500)); // applied by the sound thread: nothing published (no osd)
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for_count("quick shown", 2, DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d); // compared once drawn: never the bare screen under it
    assert!(sway.wait_for_screen("HEADLESS-1", |s| quick_panel(s) != before), "the microphone muted at open: {:?}", d.log());
}

/// Spec § 1: with the panel, the right click opens the TUI tab the left click used to.
#[test]
#[ignore = "headless sway, dbus-daemon (nix develop)"]
fn quick_the_right_click_still_opens_the_tui_tab() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-right", "");
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button3");
    assert!(wait_for_call(&sway, "alpheus-toggle", "alpheus-toggle sound"), "{:?} {:?}", recorded(&sway, "alpheus-toggle"), d.log());
    assert_eq!(d.count("quick shown"), 0, "the right click opens no panel");
}

/// Spec § 1, § 7: Escape, a second click on the module and a click elsewhere on the bar close it;
/// 0 loop wake-ups while it is open and idle, and once it closed.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_escape_and_clicks_close_it_and_nothing_stays_awake() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-close", "");
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    assert_eq!(rest_over_5_s(&d), 0, "open and idle: nothing armed, nothing wakes");
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "{:?}", d.log());
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for_count("quick shown", 2, DELAY), "{:?}", d.log());
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "a second click on the module closes it: {:?}", d.log());
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("quick shown"), 2, "closed, not reopened by the same click: {:?}", d.log());
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for_count("quick shown", 3, DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    sway.click(640, 10, "button1"); // the bar, far from the volume
    assert!(d.wait_for_count("popup closed (outside click)", 2, DELAY), "{:?}", d.log());
    assert_eq!(rest_over_5_s(&d), 0, "closed: nothing left awake");
}

/// The loop thread's wake-ups in the second after `batch` reaches the fake pw-dump.
fn wakeups_over_a_batch(sway: &Sway, d: &Daemon, batch: &str) -> u64 {
    away_from_the_minute();
    thread::sleep(Duration::from_millis(300));
    let before = d.main_thread_wakeups();
    pw_dump_send(&sway.runtime, batch);
    thread::sleep(Duration::from_secs(1));
    d.main_thread_wakeups().saturating_sub(before)
}

/// Fix I1: closing the panel takes the sound thread back to its own scope. Open, a sink unplugged
/// that is not the default changes the list, published and drawn (a wake-up); closed, the same
/// change is no news to the bar (no `osd`: `Scope::Bar`) and wakes nothing.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_closed_the_sound_thread_wakes_nothing_for_the_sink_list() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-scope", "");
    sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
    assert!(d.wait_for("quick shown (rows: volume brightness mic mixer session; sinks: 3)", DELAY), "{:?}", d.log());
    wait_for_the_popup_drawn(&sway, &d);
    assert!(wakeups_over_a_batch(&sway, &d, r#"[{"id":64,"info":null}]"#) > 0, "open: the list published: {:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_secs(1)); // the close settles (one late wake-up, pw-dump's or not)
    assert_eq!(wakeups_over_a_batch(&sway, &d, r#"[{"id":99,"info":null}]"#), 0, "closed: the base scope, nothing published");
}

fn quick_verb(sway: &Sway, args: &[&str]) -> i32 {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    run(&args, &socket_path(&sway.runtime), &mut std::io::empty(), &mut out, &mut err)
}

/// The verb's panel hangs from the bar's right end: its first row (the volume, its bar lit in the
/// accent) somewhere in the right half, just under the bar.
fn quick_drawn_on_the_right(s: &Ppm) -> bool {
    let accent = rgb(preset("catppuccin").unwrap().dark.accent);
    (29..51).any(|y| (s.width / 2..s.width).any(|x| s.at(x, y) == Some(accent)))
}

/// Quick spec § 1: `alpheus-launch --quick sound` opens the panel under the bar's
/// right end at the volume row; its keys work without a grab; Escape closes it and gives the
/// keyboard back; the verb again closes an open one; nothing stays awake. The bar's keyboard
/// interactivity is read from sway's seat focus: 0 while the bar holds the keyboard (Exclusive),
/// back on the workspace once it is None again (`Input::Keys`' reset on close).
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_the_verb_toggles_the_panel_and_its_keys_work_without_a_grab() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-verb", "");
    assert!(wait_until(|| sway.seat_focus().is_some_and(|f| f != 0)), "closed: the bar holds no keyboard");
    assert_eq!(quick_verb(&sway, &["--quick", "sound"]), EXIT_OK);
    assert!(d.wait_for("quick shown (rows: volume", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", quick_drawn_on_the_right), "{:?}", d.log());
    assert!(wait_until(|| sway.seat_focus() == Some(0)), "open: the bar is Exclusive, no grab needed");
    // One continuous wtype (headless sway's only keyboard is its own, gone when it exits): → at
    // 0.4 s, Escape at 1.6 s, the keyboard kept alive for the verb's second round.
    let typist = wtype_spawn(&sway, &["-s", "400", "-k", "Right", "-s", "1200", "-k", "Escape", "-s", "12000"]);
    assert!(
        wait_for_call(&sway, "wpctl", "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 89%"),
        "{:?} {:?}",
        recorded(&sway, "wpctl"),
        d.log()
    );
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "{:?}", d.log());
    assert!(wait_until(|| sway.seat_focus().is_some_and(|f| f != 0)), "Escape: the bar is back to None");
    assert_eq!(quick_verb(&sway, &["--quick"]), EXIT_OK);
    assert!(d.wait_for_count("quick shown", 2, DELAY), "{:?}", d.log());
    assert!(wait_until(|| sway.seat_focus() == Some(0)), "reopened: Exclusive again");
    thread::sleep(PAST_DEBOUNCE);
    assert_eq!(quick_verb(&sway, &["--quick"]), EXIT_OK);
    assert!(d.wait_for("popup closed (verb)", DELAY), "the verb again closes it: {:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 2, DELAY), "{:?}", d.log());
    assert!(wait_until(|| sway.seat_focus().is_some_and(|f| f != 0)), "the verb again: the bar is back to None");
    drop(typist);
    thread::sleep(Duration::from_secs(1)); // the virtual keyboard's capability going settles
    assert_eq!(rest_over_5_s(&d), 0, "nothing left awake");
}

/// Twice the daemon's TOGGLE_DEBOUNCE (250 ms, crate-private): a verb that really toggles.
const PAST_DEBOUNCE: Duration = Duration::from_millis(500);

/// Hyprland re-fires a press bind, so two verbs back to back are one key press:
/// the second is debounced and the panel stays open; a third, past the window, closes it.
#[test]
#[ignore = "headless sway, dbus-daemon (nix develop)"]
fn quick_a_verb_right_after_the_verb_opened_it_is_debounced() {
    let (_bus, sway, _hypr, d) = quick_daemon("quick-debounce", "");
    assert_eq!(quick_verb(&sway, &["--quick"]), EXIT_OK);
    assert_eq!(quick_verb(&sway, &["--quick"]), EXIT_OK);
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    assert!(d.wait_for("quick: toggle debounced", DELAY), "{:?}", d.log());
    assert_eq!(d.count("popup closed"), 0, "still open: {:?}", d.log());
    thread::sleep(PAST_DEBOUNCE);
    assert_eq!(quick_verb(&sway, &["--quick"]), EXIT_OK);
    assert!(d.wait_for("popup closed (verb)", DELAY), "past the window, the verb closes it: {:?}", d.log());
    assert_eq!(d.count("quick shown"), 1, "{:?}", d.log());
}

/// Spec § 1: without `quick` in `modules`, the verb exits 0 and opens nothing.
#[test]
#[ignore = "headless sway, dbus-daemon (nix develop)"]
fn quick_without_the_module_the_verb_exits_0_and_opens_nothing() {
    let sway = Sway::start("quick-off", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"volume\", \"clock\"]\n";
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(toml), pw_dump: PwDump::Fifo, ..DaemonOptions::default() });
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    assert_eq!(quick_verb(&sway, &["--quick"]), EXIT_OK);
    assert!(d.wait_for("quick: asked, but the quick module is not in modules", DELAY), "{:?}", d.log());
    assert_eq!(d.count("quick shown"), 0);
}

/// The network module and the panel; `More…` a fake; the backlight directory one with no device
/// (never the real /sys: ledger, delivery 1, P3).
fn wifi_bar(sway: &Sway) -> String {
    format!(
        "modules = [\"network\", \"quick\", \"clock\"]\n[quick]\nwifi_more = [\"fake-more\"]\n[osd]\nbacklight_dir = \"{}\"\n",
        sway.runtime.join("no-backlight").display()
    )
}
/// A press on the network module: the first one, from cell 1.
const NETWORK_AT: (u32, u32) = (16, 10);

/// The quick panel's Wi-Fi (catppuccin dark): the network module and the panel; a private system bus
/// with the fake NetworkManager (fictional: Maison-Test 80 % saved, Bureau-Test 60 % WPA2, Cafe-Libre
/// 40 % open; nothing connected); a fake `nmcli` recording its argv and its standard input; the
/// `Fifo` fake pw-dump.
fn wifi_daemon(name: &str) -> (PrivateBus, FakeNetworkManager, Sway, FakeHyprland, Daemon) {
    let bus = PrivateBus::start(name);
    let aps = vec![FakeAp::new("Maison-Test", 80, true), FakeAp::new("Bureau-Test", 60, true), FakeAp::new("Cafe-Libre", 40, false)];
    let nm = FakeNetworkManager::serve(&bus, aps, vec![(UUID_HOME.into(), "Maison-Test".into())]);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    fake_stdin_recorder(&sway, "nmcli");
    fake_recorder(&sway, "fake-more");
    let toml = wifi_bar(&sway);
    let options =
        DaemonOptions { bar_toml: Some(&toml), system_bus: Some(&bus.address), pw_dump: PwDump::Fifo, ..DaemonOptions::default() };
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(500)); // the network module drawn
    (bus, nm, sway, hypr, d)
}

/// Waits until the fake NetworkManager answered the detail's reads (the saved profiles come last),
/// then lets the state reach the panel and be painted.
fn wait_for_the_detail(nm: &FakeNetworkManager) -> bool {
    let read = wait_until(|| nm.calls().iter().any(|c| c.starts_with("GetSettings ")));
    thread::sleep(Duration::from_millis(300));
    read
}

/// Waits until the fake NetworkManager was asked for the saved profile `n` (from 1).
fn wait_for_the_profile(nm: &FakeNetworkManager, n: usize) -> bool {
    let call = format!("GetSettings /org/freedesktop/NetworkManager/Settings/{n}");
    let read = wait_until(|| nm.calls().contains(&call));
    thread::sleep(Duration::from_millis(300));
    read
}

/// No SSID of the fixture in the daemon's log.
fn no_ssid_logged(d: &Daemon) -> bool {
    !d.log().iter().any(|l| ["Maison-Test", "Bureau-Test", "Cafe-Libre"].iter().any(|s| l.contains(s)))
}

/// After the Wi-Fi is no longer followed: an access point's change reaches neither the D-Bus thread
/// nor the fake (no call), its rules gone.
fn the_wifi_rules_are_gone(nm: &FakeNetworkManager, d: &Daemon) {
    thread::sleep(Duration::from_secs(1)); // the close reaches the D-Bus thread: its rules removed
    let calls = nm.calls().len();
    let thread_before = d.thread_wakeups("D-Bus").expect("the D-Bus thread");
    nm.change(NmChange::Strength(2, 70));
    thread::sleep(Duration::from_secs(1));
    assert_eq!(nm.calls().len(), calls, "no call: {:?}", &nm.calls()[calls..]);
    assert_eq!(d.thread_wakeups("D-Bus").expect("the D-Bus thread").saturating_sub(thread_before), 0, "the rule gone: asleep");
}

/// Quick Wi-Fi spec § 1, § 2, § 7: a click on the network module opens the panel on the Wi-Fi detail
/// (one rescan); Enter on the saved network, first in the list, records `connection up uuid …`; no
/// SSID reaches the log.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_wifi_a_click_on_the_network_opens_the_detail_and_enter_connects_a_saved_network() {
    let (_bus, nm, sway, _hypr, d) = wifi_daemon("quick-wifi-saved");
    sway.click(NETWORK_AT.0, NETWORK_AT.1, "button1");
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    assert!(wait_for_call(&sway, "nmcli", "nmcli device wifi rescan"), "{:?} {:?}", recorded(&sway, "nmcli"), d.log());
    assert!(wait_for_the_detail(&nm), "{:?}", nm.calls());
    keys(&sway, &["Return"]);
    let up = format!("nmcli connection up uuid {UUID_HOME}");
    assert!(wait_for_call(&sway, "nmcli", &up), "{:?} {:?}", recorded(&sway, "nmcli"), d.log());
    assert!(d.wait_for("quick: wifi connect (saved)", DELAY), "{:?}", d.log());
    assert!(no_ssid_logged(&d), "never an SSID in the log: {:?}", d.log());
}

/// Any pixel of the panel's first lines in catppuccin dark's warn: the `failed` tag (P8).
fn warn_in_the_panel(s: &Ppm) -> bool {
    let warn = rgb(preset("catppuccin").unwrap().dark.warn);
    quick_panel(s).contains(&warn)
}

/// Quick Wi-Fi spec § 3, § 7: a new secured network asks for its password; typed and Enter, nmcli
/// gets `--ask device wifi connect Bureau-Test` and the password on its standard input only. The
/// fake NetworkManager then activates, makes a profile, fails: `failed` painted (warn, the
/// selection moved off it first: P8), one log line, and the profile deleted. Closed, nothing stays
/// awake.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_wifi_a_new_secured_network_takes_its_password_on_stdin_only_then_fails_and_is_cleaned_up() {
    let (_bus, nm, sway, _hypr, d) = wifi_daemon("quick-wifi-password");
    sway.click(NETWORK_AT.0, NETWORK_AT.1, "button1");
    assert!(wait_for_the_detail(&nm), "{:?} {:?}", nm.calls(), d.log());
    keys(&sway, &["Down", "Return"]);
    assert!(wtype(&sway, &["-s", "300", "fake-pass", "-k", "Return"]).success());
    let ask = "nmcli --ask device wifi connect Bureau-Test";
    assert!(wait_for_call(&sway, "nmcli", ask), "{:?} {:?}", recorded(&sway, "nmcli"), d.log());
    assert!(
        wait_until(|| recorded_stdin(&sway, "nmcli") == b"fake-pass\n"),
        "the password on stdin, a newline after: {:?}",
        String::from_utf8_lossy(&recorded_stdin(&sway, "nmcli"))
    );
    assert!(recorded(&sway, "nmcli").iter().all(|c| !c.contains("fake-pass")), "never an argument");
    assert!(d.wait_for("quick: wifi connect (password)", DELAY), "{:?}", d.log());
    assert!(!d.log().iter().any(|l| l.contains("fake-pass")) && no_ssid_logged(&d), "never in the log: {:?}", d.log());
    // The selection off Bureau-Test, so its tag is drawn in its own colour, not the selection's.
    keys(&sway, &["Up"]);
    thread::sleep(Duration::from_millis(300));
    assert!(!warn_in_the_panel(&sway.screenshot("HEADLESS-1").expect("grim")), "…, connecting: nothing in warn yet");
    nm.change(NmChange::DeviceState(60));
    thread::sleep(Duration::from_millis(300));
    nm.change(NmChange::AddSaved(UUID_NEW.into(), "Bureau-Test".into()));
    assert!(wait_for_the_profile(&nm, 2), "{:?}", nm.calls());
    nm.change(NmChange::DeviceState(120));
    assert!(d.wait_for("quick: wifi connection failed (the activation failed)", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", warn_in_the_panel), "`failed` painted in warn: {:?}", d.log());
    let delete = format!("nmcli connection delete uuid {UUID_NEW}");
    assert!(wait_for_call(&sway, "nmcli", &delete), "{:?} {:?}", recorded(&sway, "nmcli"), d.log());
    keys(&sway, &["Escape", "Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_secs(1)); // the close settles (wtype's keyboard going, the D-Bus thread's last word)
    assert_eq!(rest_over_5_s(&d), 0, "closed: the failed mark's timer went with the panel");
}

/// Quick Wi-Fi spec § 5: open, an access point's change is followed; closed with no attempt under
/// way (straight from the detail, a second click on the network module), its rules are gone: the
/// same change reaches neither the D-Bus thread nor the fake (no call), and nothing wakes.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_wifi_closed_nothing_follows_the_access_points_and_nothing_wakes() {
    let (_bus, nm, sway, _hypr, d) = wifi_daemon("quick-wifi-rest");
    sway.click(NETWORK_AT.0, NETWORK_AT.1, "button1");
    assert!(wait_for_the_detail(&nm), "{:?} {:?}", nm.calls(), d.log());
    let reads = |nm: &FakeNetworkManager| nm.calls().iter().filter(|c| c.contains("AccessPoint/2 ")).count();
    let before = reads(&nm);
    nm.change(NmChange::Strength(2, 20));
    assert!(wait_until(|| reads(&nm) > before), "open: followed: {:?}", nm.calls());
    sway.click(NETWORK_AT.0, NETWORK_AT.1, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    the_wifi_rules_are_gone(&nm, &d);
    assert_eq!(rest_over_5_s(&d), 0);
}

/// Quick Wi-Fi spec § 1: `alpheus-launch --quick wifi` opens the panel on the Wi-Fi detail (its
/// rescan); the verb again closes it.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_wifi_the_verb_opens_the_detail() {
    let (_bus, nm, sway, _hypr, d) = wifi_daemon("quick-wifi-verb");
    assert_eq!(quick_verb(&sway, &["--quick", "wifi"]), EXIT_OK);
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    assert!(wait_for_call(&sway, "nmcli", "nmcli device wifi rescan"), "{:?} {:?}", recorded(&sway, "nmcli"), d.log());
    assert!(wait_for_the_detail(&nm), "{:?}", nm.calls());
    thread::sleep(PAST_DEBOUNCE);
    assert_eq!(quick_verb(&sway, &["--quick", "wifi"]), EXIT_OK);
    assert!(d.wait_for("popup closed (verb)", DELAY), "{:?}", d.log());
}

/// Opens the detail from the network module and presses Enter on Cafe-Libre (open, not saved:
/// third). When Enter was pressed.
fn a_new_network_attempt(nm: &FakeNetworkManager, sway: &Sway, d: &Daemon) -> Instant {
    sway.click(NETWORK_AT.0, NETWORK_AT.1, "button1");
    assert!(wait_for_the_detail(nm), "{:?} {:?}", nm.calls(), d.log());
    keys(sway, &["Down", "Down", "Return"]);
    assert!(wait_for_call(sway, "nmcli", "nmcli device wifi connect Cafe-Libre"), "{:?} {:?}", recorded(sway, "nmcli"), d.log());
    let entered = Instant::now();
    assert!(d.wait_for("quick: wifi connect (open)", DELAY), "{:?}", d.log());
    entered
}

/// Closes the panel with Escape (out of the detail, then the rows).
fn close_the_panel(sway: &Sway, d: &Daemon) {
    keys(sway, &["Escape", "Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
}

/// Mae's ruling P1: the panel closed during an attempt on a new network does not abandon it. The
/// fake NetworkManager makes its profile (seen while the panel was open), then says nothing more:
/// at the 20 s timeout (the panel's one timer, still armed for that attempt alone) the failure is
/// logged once and the profile deleted. Then nothing is read, nothing wakes.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_wifi_closed_during_a_failing_new_network_attempt_still_deletes_its_profile() {
    let (_bus, nm, sway, _hypr, d) = wifi_daemon("quick-wifi-follow-fail");
    a_new_network_attempt(&nm, &sway, &d);
    nm.change(NmChange::AddSaved(UUID_NEW.into(), "Cafe-Libre".into()));
    assert!(wait_for_the_profile(&nm, 2), "{:?}", nm.calls());
    close_the_panel(&sway, &d);
    assert_eq!(d.count("quick: wifi connection failed"), 0, "{:?}", d.log());
    assert!(d.wait_for("quick: wifi connection failed (no answer in 20 s)", Duration::from_secs(25)), "{:?}", d.log());
    let delete = format!("nmcli connection delete uuid {UUID_NEW}");
    assert!(wait_for_call(&sway, "nmcli", &delete), "{:?} {:?}", recorded(&sway, "nmcli"), d.log());
    assert!(d.wait_for("quick: wifi delete the failed profile", DELAY), "{:?}", d.log());
    assert_eq!(d.count("quick: wifi connection failed"), 1, "logged once: {:?}", d.log());
    assert!(no_ssid_logged(&d), "{:?}", d.log());
    the_wifi_rules_are_gone(&nm, &d);
    assert_eq!(rest_over_5_s(&d), 0, "ended: the timer and the follower gone");
}

/// Mae's ruling: the panel reopened from the network module while an attempt
/// is followed takes it back and opens the detail at once (its rescan), with no second attempt and
/// no word to the D-Bus thread (the tap stays on the detail: nothing removed, nothing re-read).
/// The attempt then times out in the reopened panel: one failure line, one delete.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_wifi_a_panel_reopened_during_a_followed_attempt_takes_it_back_on_the_detail() {
    let (_bus, nm, sway, _hypr, d) = wifi_daemon("quick-wifi-resume");
    a_new_network_attempt(&nm, &sway, &d);
    nm.change(NmChange::AddSaved(UUID_NEW.into(), "Cafe-Libre".into()));
    assert!(wait_for_the_profile(&nm, 2), "{:?}", nm.calls());
    let (calls, woken) = (nm.calls().len(), d.thread_wakeups("D-Bus").expect("the D-Bus thread"));
    close_the_panel(&sway, &d);
    thread::sleep(Duration::from_millis(500));
    sway.click(NETWORK_AT.0, NETWORK_AT.1, "button1");
    assert!(d.wait_for_count("quick shown", 2, DELAY), "{:?}", d.log());
    let rescans = || recorded(&sway, "nmcli").iter().filter(|c| *c == "nmcli device wifi rescan").count();
    assert!(wait_until(|| rescans() == 2), "the detail opened again: {:?} {:?}", recorded(&sway, "nmcli"), d.log());
    thread::sleep(Duration::from_millis(500));
    assert_eq!(nm.calls().len(), calls, "nothing read again: {:?}", &nm.calls()[calls..]);
    assert_eq!(d.thread_wakeups("D-Bus").expect("the D-Bus thread").saturating_sub(woken), 0, "the tap never changed scope");
    let connects = || recorded(&sway, "nmcli").iter().filter(|c| c.starts_with("nmcli device wifi connect")).count();
    assert_eq!(connects(), 1, "no second attempt: {:?}", recorded(&sway, "nmcli"));
    assert!(d.wait_for("quick: wifi connection failed (no answer in 20 s)", Duration::from_secs(25)), "{:?}", d.log());
    let delete = format!("nmcli connection delete uuid {UUID_NEW}");
    assert!(wait_for_call(&sway, "nmcli", &delete), "{:?} {:?}", recorded(&sway, "nmcli"), d.log());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(d.count("quick: wifi connection failed"), 1, "{:?}", d.log());
    assert_eq!(recorded(&sway, "nmcli").iter().filter(|c| **c == delete).count(), 1, "one delete");
    assert_eq!(connects(), 1);
    assert!(no_ssid_logged(&d), "{:?}", d.log());
}

/// Mae's ruling P1: the panel closed during an attempt on a new network that succeeds: the follower
/// hears NetworkManager's word after the close (the profile made, then activated) and ends there,
/// so nothing is deleted and no failure is logged, even past the 20 s timeout; then nothing is
/// read, nothing wakes.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_wifi_closed_during_a_successful_new_network_attempt_deletes_nothing() {
    let (_bus, nm, sway, _hypr, d) = wifi_daemon("quick-wifi-follow-ok");
    let entered = a_new_network_attempt(&nm, &sway, &d);
    close_the_panel(&sway, &d);
    nm.change(NmChange::DeviceState(60));
    thread::sleep(Duration::from_millis(300));
    nm.change(NmChange::AddSaved(UUID_NEW.into(), "Cafe-Libre".into()));
    assert!(wait_for_the_profile(&nm, 2), "closed: the new profile still read: {:?}", nm.calls());
    nm.change(NmChange::Connect(Some((3, UUID_NEW.into()))));
    the_wifi_rules_are_gone(&nm, &d);
    let past = (entered + Duration::from_secs(21)).saturating_duration_since(Instant::now());
    thread::sleep(past);
    assert_eq!(d.count("quick: wifi connection failed"), 0, "connected: never a failure: {:?}", d.log());
    assert!(recorded(&sway, "nmcli").iter().all(|c| !c.contains("delete")), "nothing deleted: {:?}", recorded(&sway, "nmcli"));
    assert_eq!(rest_over_5_s(&d), 0, "ended: the timer and the follower gone");
}

const BT_MODULES: &str = "\"bluetooth\", \"quick\", \"clock\"";
/// A press on the bluetooth module: the first one of `BT_MODULES`, from cell 1.
const BLUETOOTH_AT: (u32, u32) = (16, 10);
/// The fake BlueZ's paired headset, `CasqueBT` (support/bluez.rs: device 2).
const CASQUE: u8 = 2;

/// The quick panel's Bluetooth (catppuccin dark): `modules` and the panel; a private system bus with
/// the fake BlueZ (hci0 powered, CasqueBT paired, not connected); fake `wpctl` and `rfkill`
/// recording their calls; the `Fifo` fake pw-dump; a backlight directory with no device (no
/// brightness row, never the real /sys).
fn bt_daemon(name: &str, modules: &str) -> (PrivateBus, FakeBluez, Sway, FakeHyprland, Daemon) {
    let bus = PrivateBus::start(name);
    let bluez = FakeBluez::serve(&bus);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    for program in ["wpctl", "rfkill"] {
        fake_recorder(&sway, program);
    }
    let toml = format!("modules = [{modules}]\n[osd]\nbacklight_dir = \"{}\"\n", sway.runtime.join("no-backlight").display());
    let options =
        DaemonOptions { bar_toml: Some(&toml), system_bus: Some(&bus.address), pw_dump: PwDump::Fifo, ..DaemonOptions::default() };
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(500)); // BlueZ's tree read, the bluetooth module drawn
    (bus, bluez, sway, hypr, d)
}

/// Waits until the fake BlueZ received a call starting with `prefix`.
fn bt_calls(b: &FakeBluez, prefix: &str) -> bool {
    wait_until(|| b.calls().iter().any(|c| c.starts_with(prefix)))
}

/// The detail open: the agent registered and discovery started, then 300 ms to paint.
fn wait_for_the_bt_detail(b: &FakeBluez) -> bool {
    let ok = bt_calls(b, "RegisterAgent ") && bt_calls(b, "StartDiscovery /org/bluez/hci0");
    thread::sleep(Duration::from_millis(300));
    ok
}

/// A click on the bluetooth module: the panel shown on the Bluetooth detail, drawn (its background
/// down more than one row).
fn open_the_bt_detail(sway: &Sway, d: &Daemon, b: &FakeBluez) {
    sway.click(BLUETOOTH_AT.0, BLUETOOTH_AT.1, "button1");
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    assert!(wait_for_the_bt_detail(b), "{:?} {:?}", b.calls(), d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| panel_height(s) > 22), "the panel drawn: {:?}", d.log());
}

/// No device name, address or object path of the fixtures in the daemon's log (constraints: BlueZ
/// paths embed the address).
fn no_device_logged(d: &Daemon) -> bool {
    !d.log()
        .iter()
        .any(|l| ["CasqueBT", "Keyboard-Test", "Speaker-X", "AA:BB", "AA_BB", "/org/bluez/hci0/dev"].iter().any(|s| l.contains(s)))
}

/// The panel's height: `popup_bg` pixels down its third cell's column.
fn panel_height(s: &Ppm) -> usize {
    let bg = popup_bg("catppuccin");
    column(s, popup_cell(0, 2, 0).0).iter().filter(|p| **p == bg).count()
}

/// Quick Bluetooth spec § 2, § 5: a click on the bluetooth module opens the panel on its detail; a
/// second connection to the system bus registers the pairing agent (KeyboardDisplay, never as the
/// default agent) and starts discovery on hci0.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn bluetooth_a_module_click_opens_the_detail() {
    let (bus, bluez, sway, _hypr, d) = bt_daemon("bt-open", BT_MODULES);
    assert_eq!(connections_of(&bus, d.pid()), 1, "closed: the read connection only");
    open_the_bt_detail(&sway, &d, &bluez);
    let calls = bluez.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("RegisterAgent ") && c.contains("/org/alpheus/bluetooth/agent KeyboardDisplay")),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("RequestDefaultAgent")), "{calls:?}");
    assert_eq!(connections_of(&bus, d.pid()), 2, "open: read and act");
    assert!(no_device_logged(&d), "{:?}", d.log());
}

/// Quick Bluetooth spec § 3: Enter on the paired headset (selected first) calls `Connect` on it;
/// once BlueZ says it is connected, the bar's module shows it (more ink); nothing names it in the
/// log.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn bluetooth_enter_connects_a_paired_device() {
    let (_bus, bluez, sway, _hypr, d) = bt_daemon("bt-connect", BT_MODULES);
    open_the_bt_detail(&sway, &d, &bluez);
    let before = ink(&sway.screenshot("HEADLESS-1").expect("grim"), 0, 200);
    keys(&sway, &["Return"]);
    let connect = format!("Connect {}", device_path(CASQUE));
    assert!(bt_calls(&bluez, &connect), "{:?} {:?}", bluez.calls(), d.log());
    bluez.set_connected(CASQUE, true);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| ink(s, 0, 200) > before + 40), "the module shows the device: {before} {:?}", d.log());
    assert!(no_device_logged(&d), "{:?}", d.log());
}

/// Quick Bluetooth spec § 3, § 5: Enter on a nearby keyboard calls `Pair` (held by the fake); BlueZ
/// shows a passkey through the agent, which answers it, and the panel grows one line: `Type 482
/// 913 then ⏎`.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn bluetooth_pairing_a_keyboard_shows_its_code() {
    let (_bus, bluez, sway, _hypr, d) = bt_daemon("bt-pair", BT_MODULES);
    open_the_bt_detail(&sway, &d, &bluez);
    bluez.hold("Pair");
    bluez.add_nearby(3, "Keyboard-Test", -45);
    thread::sleep(Duration::from_millis(500));
    keys(&sway, &["Down", "Return"]); // CasqueBT, the `Nearby` heading (not selectable), Keyboard-Test
    let pair = format!("Pair {}", device_path(3));
    assert!(bt_calls(&bluez, &pair), "{:?} {:?}", bluez.calls(), d.log());
    thread::sleep(Duration::from_millis(300));
    let h = panel_height(&sway.screenshot("HEADLESS-1").expect("grim"));
    bluez.ask_passkey(3, 482_913);
    assert!(
        wait_until(|| bluez.replies().iter().any(|r| r.starts_with("DisplayPasskey") && !r.contains("Rejected"))),
        "{:?} {:?}",
        bluez.replies(),
        d.log()
    );
    assert!(sway.wait_for_screen("HEADLESS-1", |s| panel_height(s) >= h + 11), "one more line, the code: {h} {:?}", d.log());
    assert!(no_device_logged(&d), "{:?}", d.log());
}

/// Quick Bluetooth spec § 3: Delete asks `Forget? ⏎`; Enter past `CONFIRM_AFTER` removes the
/// device from the adapter.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn bluetooth_forget_removes_a_device() {
    let (_bus, bluez, sway, _hypr, d) = bt_daemon("bt-forget", BT_MODULES);
    open_the_bt_detail(&sway, &d, &bluez);
    keys(&sway, &["Delete"]);
    thread::sleep(Duration::from_millis(500)); // past CONFIRM_AFTER (0.4 s)
    keys(&sway, &["Return"]);
    let target = device_path(CASQUE);
    assert!(
        wait_until(|| bluez.calls().iter().any(|c| c.starts_with("RemoveDevice ") && c.contains(&target))),
        "{:?} {:?}",
        bluez.calls(),
        d.log()
    );
    assert!(no_device_logged(&d), "{:?}", d.log());
}

/// Quick Bluetooth spec § 4: the default sink on a Bluetooth headset adds the headset row under
/// the volume; Enter on it switches the headset to its other mode (`headset-head-unit`, index 3).
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn bluetooth_headset_row_switches_profile() {
    let (_bus, _bluez, sway, _hypr, d) = bt_daemon("bt-headset", "\"volume\", \"bluetooth\", \"quick\", \"clock\"");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pw-dump-bluez-headset.json");
    pw_dump_send(&sway.runtime, &std::fs::read_to_string(fixture).unwrap());
    // The sound thread publishes nothing to wait for: the panel is opened until it shows the
    // headset row (a slow pw-dump read under load), closed again in between.
    let t = Instant::now();
    for opens in 1.. {
        sway.click(VOLUME_AT.0, VOLUME_AT.1, "button1");
        assert!(d.wait_for_count("quick shown", opens, DELAY), "{:?}", d.log());
        wait_for_the_popup_drawn(&sway, &d);
        let last = d.log().into_iter().rfind(|l| l.contains("quick shown")).unwrap_or_default();
        if last.contains("quick shown (rows: volume headset") {
            break;
        }
        assert!(t.elapsed() < DELAY, "the headset row never showed: {:?}", d.log());
        keys(&sway, &["Escape"]);
        assert!(d.wait_for_count("popup closed (escape)", opens, DELAY), "{:?}", d.log());
        assert!(d.wait_for_count("keyboard released", opens, DELAY), "{:?}", d.log());
    }
    keys(&sway, &["Down", "Return"]);
    assert!(wait_for_call(&sway, "wpctl", "wpctl set-profile 61 3"), "{:?} {:?}", recorded(&sway, "wpctl"), d.log());
}

/// Constraints (acting connection): closing the panel unregisters the agent and lets the acting
/// connection go; a device found afterwards reaches no reader that calls BlueZ, and nothing wakes.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn bluetooth_at_rest_no_wakeups_after_the_panel_closes() {
    let (bus, bluez, sway, _hypr, d) = bt_daemon("bt-rest", BT_MODULES);
    open_the_bt_detail(&sway, &d, &bluez);
    keys(&sway, &["Escape", "Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(bt_calls(&bluez, "UnregisterAgent "), "{:?} {:?}", bluez.calls(), d.log());
    assert!(wait_until(|| connections_of(&bus, d.pid()) == 1), "the acting connection gone: {:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_secs(1)); // the close settles
    let calls = bluez.calls().len();
    bluez.add_nearby(4, "Speaker-X", -60);
    thread::sleep(Duration::from_secs(1));
    assert_eq!(bluez.calls().len(), calls, "closed: nothing read: {:?}", &bluez.calls()[calls..]);
    assert_eq!(rest_over_5_s(&d), 0, "closed: nothing left awake");
}

/// Quick Bluetooth spec § 6: BlueZ leaving while a `Pair` is under way ends it silently: no
/// `failed` drawn, no failure logged, and the acting connection goes.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn bluetooth_bluez_gone_mid_pair_shows_no_failure() {
    let (bus, bluez, sway, _hypr, d) = bt_daemon("bt-gone", BT_MODULES);
    open_the_bt_detail(&sway, &d, &bluez);
    bluez.hold("Pair");
    bluez.add_nearby(3, "Keyboard-Test", -45);
    thread::sleep(Duration::from_millis(500));
    keys(&sway, &["Down", "Return"]);
    assert!(bt_calls(&bluez, &format!("Pair {}", device_path(3))), "{:?} {:?}", bluez.calls(), d.log());
    drop(bluez); // org.bluez loses its owner, the held Pair unanswered
    assert!(wait_until(|| connections_of(&bus, d.pid()) == 1), "the acting connection gone: {:?}", d.log());
    thread::sleep(Duration::from_secs(1));
    assert!(!d.log().iter().any(|l| l.contains("quick: bluetooth") && l.contains("failed")), "{:?}", d.log());
    assert!(!warn_in_the_panel(&sway.screenshot("HEADLESS-1").expect("grim")), "no `failed` drawn: {:?}", d.log());
    assert!(no_device_logged(&d), "{:?}", d.log());
}

/// The fake source's lines (fictional, in tools-menu's shape).
fn source_lines() -> Vec<String> {
    ["Apparence · Schéma couleurs · Vibrant", "Captures · Screenshot zone", "Verrouiller"].map(String::from).to_vec()
}

#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn a_source_entry_is_found_by_typing_and_runs_its_whole_line() {
    let sway = Sway::start("launcher-source", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = fake_source(&sway.runtime, "Outils", &source_lines());
    let d = start_daemon_with(&sway, &DaemonOptions { launcher_toml: Some(&toml), ..DaemonOptions::default() });
    ready(&d);
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher sources: 3 entries", DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "vibrant", "-k", "Return"]).success());
    let marker = sway.runtime.join(FAKE_RUN_MARKER);
    wait_for_file(&marker, DELAY);
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "--from-alpheus\nApparence · Schéma couleurs · Vibrant\n",
        "the whole line, one argument"
    );
    assert!(d.wait_for("launcher closed (activated)", DELAY), "{:?}", d.log());
    let history = std::fs::read_to_string(sway.runtime.join("state/alpheus/launcher-history")).unwrap();
    assert!(history.lines().any(|l| l.ends_with("\tsource:Outils:Apparence · Schéma couleurs · Vibrant")), "{history:?}");
    assert!(!d.log().iter().any(|l| l.contains("Vibrant")), "a line reached the log: {:?}", d.log());
}

/// The launcher box once it stops changing (rows below the bar, whose clock may tick): grim
/// every 100 ms until two shots in a row agree.
fn settled_box(sway: &Sway) -> Vec<[u8; 3]> {
    const BELOW_THE_BAR: u32 = 48;
    let shot = || {
        let s = sway.screenshot("HEADLESS-1").expect("grim reads the output");
        (BELOW_THE_BAR..s.height).flat_map(|y| (0..s.width).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect::<Vec<_>>()
    };
    let t = Instant::now();
    let mut last = shot();
    loop {
        thread::sleep(Duration::from_millis(100));
        let next = shot();
        if next == last {
            return next;
        }
        assert!(t.elapsed() < DELAY, "the launcher box never settled");
        last = next;
    }
}

/// Spec § 3: an empty query lists today's apps and windows, never a source entry. Watched on
/// screen, never by pressing Return (it would act on the fixture's windows): the box with the
/// entries delivered is the box of an open with no source at all.
#[test]
#[ignore = "headless sway, wtype and grim (nix develop)"]
fn the_empty_query_shows_no_source_entry() {
    let sway = Sway::start("launcher-source-empty", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = fake_source(&sway.runtime, "Outils", &source_lines());
    let d = start_daemon_with(&sway, &DaemonOptions { launcher_toml: Some(&toml), ..DaemonOptions::default() });
    ready(&d);
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher sources: 3 entries", DELAY), "{:?}", d.log());
    let with_entries = settled_box(&sway);
    assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for("launcher closed (escape)", DELAY), "{:?}", d.log());
    // launcher.toml is read at each open: the next one has no source, hence no entries.
    std::fs::write(sway.runtime.join("config/alpheus/launcher.toml"), "").unwrap();
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
    let without = settled_box(&sway);
    assert!(with_entries == without, "the empty query listed something more with the source's entries");
    assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for_count("launcher closed (escape)", 2, DELAY), "{:?}", d.log());
    assert_eq!(d.count("launcher sources:"), 1, "the second open read no source");
}

#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn a_hung_source_is_killed_and_nothing_stays_awake() {
    let sway = Sway::start("launcher-source-rest", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    // Written first: this script hangs, so `wait_until_executable` cannot run it to rule out a
    // "Text file busy" (a fork elsewhere in this process briefly holding its write descriptor).
    // `fake_source` below writes and runs its own until it executes, long after any such fork has
    // exec'd and let go of this one's.
    let pid_file = sway.runtime.join("slow-source-child.pid");
    let slow = sway.runtime.join("bin/fake-list-slow");
    std::fs::create_dir_all(sway.runtime.join("bin")).unwrap();
    let body = format!(
        "#!/bin/sh\n# A fake source that hangs (never tools-menu).\nsleep 30 &\necho $! > '{}'\necho 'Lent · Une'\nwait\n",
        pid_file.display()
    );
    std::fs::write(&slow, body).unwrap();
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut toml = fake_source(&sway.runtime, "Outils", &source_lines());
    let run = sway.runtime.join("bin/fake-run");
    toml.push_str(&format!(
        "[[sources]]\nname = \"Lent\"\nlist = [{:?}]\nrun = [{:?}, \"{{line}}\"]\n",
        slow.display().to_string(),
        run.display().to_string()
    ));
    // A quiet bar (the clock only), as every rest test: the default modules tick (resources, 2 s).
    let options = DaemonOptions { bar_toml: Some(CLOCK_BAR), launcher_toml: Some(&toml), ..DaemonOptions::default() };
    let d = start_daemon_with(&sway, &options);
    ready(&d);
    let threads = d.threads();
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher source \"Lent\": timed out after 1 s, 1 lines kept", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher sources: 4 entries", DELAY), "the hung source's line kept: {:?}", d.log());
    let child = wait_for_pid_file(&pid_file, DELAY);
    let t = Instant::now();
    while process_alive(child) {
        assert!(t.elapsed() < Duration::from_secs(2), "the hung source's child {child} survived");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for("launcher closed (escape)", DELAY), "{:?}", d.log());
    let t = Instant::now();
    while d.threads() != threads {
        assert!(t.elapsed() < Duration::from_secs(2), "{} threads, {threads} before the open", d.threads());
        thread::sleep(Duration::from_millis(20));
    }
    // The scrim gone, the pointer enters what lies beneath: one event, with or without sources
    // (an open with none shows it too). Let it land, as the other rest tests do.
    sway.hover(640, 400);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(rest_over_5_s(&d), 0, "the loop woke at rest after an open that ran sources");
}

/// Mango and Sway spec § 6: the Sway fixtures come from THIS isolated headless sway, never Mae's
/// session. Two outputs; on workspace 1, two windows (a then b, a focused last); on the named
/// workspace "music", a floating c (urgent) and d moved to the scratchpad. With
/// ALPHEUS_CAPTURE_SWAY=<directory>, writes the three replies there (once, to make them); always checks
/// that the live replies give the state the committed fixtures give, ids aside.
#[test]
#[ignore = "headless sway (nix develop)"]
fn sway_fixtures_match_a_live_headless_sway() {
    let sway = Sway::start("sway-fixtures", 2);
    let _a = TestWindow::open(&sway, "fixture-a", "Fixture A");
    let _b = TestWindow::open(&sway, "fixture-b", "Fixture B");
    sway.command("workspace music");
    let _c = TestWindow::open(&sway, "fixture-c", "Fixture C");
    sway.command("floating enable");
    let _d = TestWindow::open(&sway, "fixture-d", "Fixture D");
    sway.command("move scratchpad");
    sway.command("workspace 1");
    sway.command("[app_id=fixture-a] focus");
    sway.command("[app_id=fixture-c] urgent enable");
    let workspaces = sway.ipc_raw(1, b"");
    let tree = sway.ipc_raw(4, b"");
    let refused = sway.ipc_raw(2, b"[\"no-such-event\"]");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    if let Some(dir) = std::env::var_os("ALPHEUS_CAPTURE_SWAY") {
        let dir = Path::new(&dir);
        std::fs::write(dir.join("sway-workspaces.json"), &workspaces).unwrap();
        std::fs::write(dir.join("sway-tree.json"), &tree).unwrap();
        std::fs::write(dir.join("sway-subscribe-refused.json"), &refused).unwrap();
    }
    let read = |name: &str| std::fs::read(fixtures.join(name)).unwrap_or_else(|e| panic!("{name}: {e} (capture first)"));
    let live = alpheus_demon::compositor::sway::state(&workspaces, &tree).unwrap();
    let fixture = alpheus_demon::compositor::sway::state(&read("sway-workspaces.json"), &read("sway-tree.json")).unwrap();
    let shape = |e: &alpheus_demon::compositor::CompositorState| {
        let tops: Vec<_> = e.toplevels.iter().map(|t| (t.class.clone(), t.title.clone(), t.workspace.clone(), t.focused)).collect();
        (e.workspaces.clone(), e.window.clone(), tops)
    };
    assert_eq!(shape(&live), shape(&fixture), "the committed fixtures still describe what this sway sends");
    assert!(con_id(&tree, "fixture-a").is_some());
    let refusal: serde_json::Value = serde_json::from_slice(&refused).unwrap();
    assert_eq!(refusal.get("success").and_then(|s| s.as_bool()), Some(false), "{refusal}");
}

/// The bar of the backend tests: workspaces at the left edge, then the title, the clock at the right.
const BACKEND_BAR: &str = "modules = [\"workspaces\", \"window\", \"clock\"]\n";

/// Pixels of the bar's text band — rows 4 to 17, columns [x0, x1) — that differ from the bar's own
/// background (its pixel at (x1, 1), above any text).
fn ink(shot: &Ppm, x0: u32, x1: u32) -> usize {
    let Some(background) = shot.at(x1, 1) else { return 0 };
    (4..18).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| shot.at(x, y).is_some_and(|p| p != background)).count()
}

/// Mango and Sway spec § 6: on the isolated headless sway, read as the compositor ($SWAYSOCK of
/// THIS sway), the bar shows the focused window's title, a click on workspace 1 runs `wm-set
/// workspace 1 HEADLESS-1` (a fake wm-set), and the title goes with the window.
#[test]
#[ignore = "headless sway (nix develop)"]
fn under_sway_the_bar_shows_the_workspaces_and_the_title() {
    let sway = Sway::start("sway-bar", 1);
    let window = TestWindow::open(&sway, "alpheus-test", "Fixture window");
    fake_recorder(&sway, "wm-set");
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(BACKEND_BAR), backend: Backend::Sway, ..DaemonOptions::default() });
    assert!(d.wait_for("compositor: Sway ($SWAYSOCK)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| ink(s, 48, 400) > 30), "the window's title is drawn");
    sway.click(12, 10, "button1"); // column 1: workspace 1's cell
    assert!(
        wait_until(|| recorded(&sway, "wm-set").iter().any(|l| l == "wm-set workspace 1 HEADLESS-1")),
        "{:?} {:?}",
        recorded(&sway, "wm-set"),
        d.log()
    );
    sway.command("[app_id=alpheus-test] kill");
    assert!(wait_until(|| window.is_closed()), "sway closed the test window");
    assert!(sway.wait_for_screen("HEADLESS-1", |s| ink(s, 48, 400) == 0), "the title went with the window");
    assert!(d.log().iter().all(|l| !l.contains("compositor source down")), "{:?}", d.log());
}

/// Spec § 6: under Sway the launcher lists the window; Enter on it runs `wm-set focus-window
/// id:<its con_id>` (a fake wm-set).
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn under_sway_the_launcher_lists_and_focuses_a_window() {
    let sway = Sway::start("sway-launcher", 1);
    let _window = TestWindow::open(&sway, "alpheus-test", "Fixture window");
    let id = con_id(&sway.ipc_raw(4, b""), "alpheus-test").expect("the window in this sway's tree");
    fake_recorder(&sway, "wm-set");
    let d = start_daemon_with(&sway, &DaemonOptions { backend: Backend::Sway, ..DaemonOptions::default() });
    ready(&d);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "Fixture", "-k", "Return"]).success());
    let expected = format!("wm-set focus-window id:{id}");
    assert!(wait_until(|| recorded(&sway, "wm-set").contains(&expected)), "{:?} {:?}", recorded(&sway, "wm-set"), d.log());
}

/// Launcher 2, `--windows`: the switcher opens on the previous window (Enter focuses it), a second
/// `--windows` while it is open steps to the next one, and none of it scans. Three windows opened
/// in order a, b, c: Sway's order is b, a, c (the fake `wm-set` focuses nothing, so it stays). A
/// `--windows` right after the open is the press-bind refire and is ignored (final review); past
/// the debounce two quick ones step twice.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn windows_focuses_the_previous_window_then_steps() {
    let sway = Sway::start("launcher-windows", 1);
    let _a = TestWindow::open(&sway, "alpheus-a", "Fixture a");
    let _b = TestWindow::open(&sway, "alpheus-b", "Fixture b");
    let _c = TestWindow::open(&sway, "alpheus-c", "Fixture c");
    let tree = sway.ipc_raw(4, b"");
    let id = |app: &str| con_id(&tree, app).unwrap_or_else(|| panic!("{app} in this sway's tree"));
    let (b, c) = (id("alpheus-b"), id("alpheus-c"));
    fake_recorder(&sway, "wm-set");
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(BACKEND_BAR), backend: Backend::Sway, ..DaemonOptions::default() });
    ready(&d);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    // The compositor thread has read the tree once the focused window's title is drawn.
    assert!(sway.wait_for_screen("HEADLESS-1", |s| ink(s, 48, 400) > 30), "the title is drawn: {:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Windows), Reply::Done);
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "-k", "Return"]).success());
    let first = format!("wm-set focus-window id:{b}");
    assert!(wait_until(|| recorded(&sway, "wm-set").contains(&first)), "{:?} {:?}", recorded(&sway, "wm-set"), d.log());
    assert!(d.wait_for("launcher closed (activated)", DELAY), "{:?}", d.log());
    // Again: a `--windows` at once (ignored: b stays), then past the debounce two in a row step
    // from b to a to c. Without the debounce: b, a, c, b.
    assert_eq!(launcher(&sway, &Request::Windows), Reply::Done);
    assert!(d.wait_for_count(SHOWN, 2, DELAY), "{:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Windows), Reply::Done);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(launcher(&sway, &Request::Windows), Reply::Done);
    assert_eq!(launcher(&sway, &Request::Windows), Reply::Done);
    assert!(wtype(&sway, &["-s", "300", "-k", "Return"]).success());
    let second = format!("wm-set focus-window id:{c}");
    assert!(wait_until(|| recorded(&sway, "wm-set").contains(&second)), "{:?} {:?}", recorded(&sway, "wm-set"), d.log());
    assert!(d.wait_for_count("launcher closed (activated)", 2, DELAY), "{:?}", d.log());
    assert_eq!(d.count(SCAN_STARTED), 0, "the switcher scans nothing: {:?}", d.log());
    // The control: an apps open logs that line, so the zero above can go red.
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count(SHOWN, 3, DELAY), "{:?}", d.log());
    assert!(d.wait_for(SCAN_STARTED, DELAY), "an apps open scans: {:?}", d.log());
}

/// Final review: with both of the box's buffers still held by the compositor, a draw waits for a
/// release instead of allocating, and is done then. Three `--windows` sent back to back go round
/// the three windows to where two paced ones led: the screen must show that last selection, not a
/// frame left behind by the burst.
#[test]
#[ignore = "headless sway (nix develop)"]
fn back_to_back_steps_end_on_the_last_selection_drawn() {
    let sway = Sway::start("launcher-coalesce", 1);
    let _windows: Vec<_> =
        ["a", "b", "c"].iter().map(|n| TestWindow::open(&sway, &format!("alpheus-{n}"), &format!("Fixture {n}"))).collect();
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(BACKEND_BAR), backend: Backend::Sway, ..DaemonOptions::default() });
    ready(&d);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| ink(s, 48, 400) > 30), "the title is drawn: {:?}", d.log());
    // Below the bar: the box alone changes with the selection (the bar's clock may tick).
    let below_bar = || {
        let s = sway.screenshot("HEADLESS-1").expect("a screenshot");
        (60..s.height).flat_map(|y| (0..s.width).map(move |x| (x, y))).map(|(x, y)| s.at(x, y).unwrap_or_default()).collect::<Vec<_>>()
    };
    let step = || assert_eq!(launcher(&sway, &Request::Windows), Reply::Done);
    step();
    assert!(d.wait_for(SHOWN, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300)); // past the open's debounce
    let first = below_bar();
    step();
    std::thread::sleep(Duration::from_millis(300));
    step();
    std::thread::sleep(Duration::from_millis(300));
    let paced = below_bar();
    assert_ne!(paced, first, "two steps moved the selection");
    // Round the three windows at once, back to the same selection.
    step();
    step();
    step();
    std::thread::sleep(Duration::from_millis(500));
    assert!(below_bar() == paced, "after a burst of three steps the box shows the selection two paced steps showed");
}

/// Spec § 6: under Mango (a fake socket serving the real captures, outputs renamed after headless
/// sway's), the two watch lines, the active output's title, a click on tag 1 runs `wm-set
/// workspace 1 HEADLESS-1`.
#[test]
#[ignore = "headless sway (nix develop)"]
fn under_mango_the_bar_reads_the_fake_mangos_tags() {
    let sway = Sway::start("mango-bar", 1);
    let mango = FakeMango::start(&sway.runtime);
    fake_recorder(&sway, "wm-set");
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml: Some(BACKEND_BAR), backend: Backend::Mango(&mango.socket), ..DaemonOptions::default() },
    );
    assert!(d.wait_for("compositor: Mango ($MANGO_INSTANCE_SIGNATURE)", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(wait_until(|| mango.requests() == ["watch all-clients", "watch all-monitors"]), "{:?}", mango.requests());
    assert!(sway.wait_for_screen("HEADLESS-1", |s| ink(s, 48, 400) > 30), "Terminal 2, the active output's client");
    sway.click(12, 10, "button1");
    assert!(
        wait_until(|| recorded(&sway, "wm-set").iter().any(|l| l == "wm-set workspace 1 HEADLESS-1")),
        "{:?} {:?}",
        recorded(&sway, "wm-set"),
        d.log()
    );
    assert!(d.log().iter().all(|l| !l.contains("compositor source down")), "{:?}", d.log());
}

/// The power button: the bar's only module, so cell 1 (≈ 8 to 16 px).
const POWER_AT: (u32, u32) = (12, 10);

/// The power menu's bar (catppuccin dark): the button alone, fake `loginctl` and `systemctl`
/// recording their calls first on the daemon's PATH (the rest of it is coreutils only, checked to
/// hold neither: never the real ones), the default `[quick]` (lock, reboot, power off).
fn power_daemon(name: &str) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    for program in ["loginctl", "systemctl"] {
        fake_recorder(&sway, program);
    }
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some("modules = [\"power\"]\n"), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(300)); // configured and drawn
    (sway, hypr, d)
}

/// Waits until the power menu is on screen: its popup background (catppuccin dark) fills a good
/// part of the area under the button. "power menu shown" is logged when the popup is asked for,
/// before the configure and the first buffer; a key or click in that gap is lost.
fn power_drawn(sway: &Sway, d: &Daemon) {
    let bg = rgb(preset("catppuccin").unwrap().dark.popup_bg);
    let filled = |s: &Ppm| (29..60).flat_map(|y| (14..60).map(move |x| (x, y))).filter(|&(x, y)| s.at(x, y) == Some(bg)).count() > 300;
    assert!(sway.wait_for_screen("HEADLESS-1", filled), "the power menu drawn: {:?}", d.log());
}

/// Waits until the third row (Power off) asks: its text is painted in the theme's warn, which no
/// other state of the menu uses. Proves the question was asked before a key that must cancel it.
fn power_asking(sway: &Sway, d: &Daemon) {
    let warn = rgb(preset("catppuccin").unwrap().dark.warn);
    let asking = |s: &Ppm| (73..95).flat_map(|y| (14..140).map(move |x| (x, y))).filter(|&(x, y)| s.at(x, y) == Some(warn)).count() > 5;
    assert!(sway.wait_for_screen("HEADLESS-1", asking), "the question shown in warn: {:?}", d.log());
}

/// The power menu opened by a click on ⏻, and drawn (a key or click before the map is lost).
fn power_open(sway: &Sway, d: &Daemon, n: usize) {
    sway.click(POWER_AT.0, POWER_AT.1, "button1");
    assert!(d.wait_for_count("power menu shown (lock reboot poweroff)", n, DELAY), "{:?}", d.log());
    power_drawn(sway, d);
}

/// Power spec § 2, § 5: a click on ⏻ opens the menu (logout is empty by default); ↓ selects Lock
/// and Enter runs it through the fake loginctl; the menu closes.
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn power_a_click_opens_the_menu_and_down_enter_locks() {
    let (sway, _hypr, d) = power_daemon("power-lock");
    power_open(&sway, &d, 1);
    keys(&sway, &["Down", "Return"]);
    assert!(wait_for_call(&sway, "loginctl", "loginctl lock-session"), "{:?} {:?}", recorded(&sway, "loginctl"), d.log());
    assert!(d.wait_for("power: lock", DELAY), "{:?}", d.log());
    assert!(d.wait_for("popup closed (activated)", DELAY), "{:?}", d.log());
    assert!(recorded(&sway, "systemctl").is_empty(), "nothing else ran");
}

/// Power spec § 2: hovering selects and a left press activates: a click on the first row locks.
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn power_a_click_on_a_row_runs_it() {
    let (sway, _hypr, d) = power_daemon("power-press");
    power_open(&sway, &d, 1);
    let (x, y) = popup_cell(0, 4, 0);
    sway.click(x, y, "button1");
    assert!(wait_for_call(&sway, "loginctl", "loginctl lock-session"), "{:?} {:?}", recorded(&sway, "loginctl"), d.log());
    assert!(d.wait_for("popup closed (activated)", DELAY), "{:?}", d.log());
}

/// Power spec § 2, § 5: ↑ selects Power off (the last entry); Enter asks; a second Enter 0.8 s later
/// runs it (a margin of 0.4 s to the 0.4 s rule, which a loaded machine eats) through the fake systemctl.
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn power_off_asks_then_a_second_enter_after_half_a_second_runs_it() {
    let (sway, _hypr, d) = power_daemon("power-off");
    power_open(&sway, &d, 1);
    assert!(wtype(&sway, &["-s", "300", "-k", "Up", "-s", "100", "-k", "Return", "-s", "800", "-k", "Return"]).success());
    assert!(wait_for_call(&sway, "systemctl", "systemctl poweroff"), "{:?} {:?}", recorded(&sway, "systemctl"), d.log());
    assert!(d.wait_for("power: power off", DELAY), "{:?}", d.log());
    assert!(d.wait_for("popup closed (activated)", DELAY), "{:?}", d.log());
}

/// Power spec § 2, § 5 (Mae's ruling): two Enters 0.1 s apart never confirm. Nothing runs, the menu
/// stays open with its question; a third Enter, past 0.4 s and inside 3 s, runs it.
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn power_two_enters_a_tenth_of_a_second_apart_run_nothing() {
    let (sway, _hypr, d) = power_daemon("power-double");
    power_open(&sway, &d, 1);
    assert!(wtype(&sway, &["-s", "300", "-k", "Up", "-s", "100", "-k", "Return", "-s", "100", "-k", "Return"]).success());
    thread::sleep(Duration::from_secs(1));
    assert!(recorded(&sway, "systemctl").is_empty(), "a double Enter never confirms: {:?}", d.log());
    assert_eq!(d.count("popup closed"), 0, "still open, still asking: {:?}", d.log());
    keys(&sway, &["Return"]); // ≈ 1.4 s after the question: it counts
    assert!(wait_for_call(&sway, "systemctl", "systemctl poweroff"), "{:?} {:?}", recorded(&sway, "systemctl"), d.log());
}

/// Power spec § 2, § 5: Escape closes the menu while the question is pending; nothing runs and
/// nothing wakes (the question's 3 s timer went with the menu); a second click on ⏻ closes it too.
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn power_escape_closes_it_with_its_question_and_nothing_stays_awake() {
    let (sway, _hypr, d) = power_daemon("power-escape");
    power_open(&sway, &d, 1);
    away_from_the_minute(); // the 4 s window below must hold no minute turn (seconds 2 to 50 now)
    keys(&sway, &["Up", "Return"]);
    power_asking(&sway, &d);
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(d.wait_for_count("keyboard released", 1, DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_secs(1)); // wtype's virtual keyboard going settles (one late wake-up)
    // The question was asked < 1 s ago: its 3 s deadline falls inside this window if its timer stayed.
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    thread::sleep(Duration::from_secs(4));
    assert_no_spin(d.main_thread_cpu_ticks().saturating_sub(ticks), Duration::from_secs(4), "the loop thread");
    assert_eq!(d.main_thread_wakeups().saturating_sub(before), 0, "the question's timer went with the menu: {:?}", d.log());
    assert!(recorded(&sway, "systemctl").is_empty(), "Escape runs nothing");
    power_open(&sway, &d, 2);
    sway.click(POWER_AT.0, POWER_AT.1, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "a second click on ⏻ closes it: {:?}", d.log());
    thread::sleep(Duration::from_millis(500));
    assert_eq!(d.count("power menu shown"), 2, "closed, not reopened by the same click: {:?}", d.log());
}

// ---- The privacy indicators (privacy plan). The fake pw-dump is `Fifo`, the batches
// synthetic (`capture_delta`); wpctl a fake recorder; the compositor the fake Hyprland. ----

/// The bar's height in px (default font): the glyphs are looked for above it.
const BAR_HEIGHT: u32 = 22;

/// A daemon with the default modules (privacy among them, privacy spec § 1) on one output,
/// catppuccin dark with `module_colors = "none"`: no other module paints a hue, so a pixel in
/// orange, green or red on the bar is a privacy glyph (they keep their hue whatever
/// `module_colors` says). The temperature sensor named matches none: never the machine's own, hot
/// under a build and then drawn in the error colour, catppuccin's red. The fake wpctl records the
/// middle click. `modules`: a `modules = […]` line, or "" for the default ones. Returned once the
/// bar is placed and pw-dump's initial state is read, with no glyph drawn yet.
fn privacy_daemon(name: &str, modules: &str) -> (Sway, FakeHyprland, Daemon) {
    privacy_daemon_with(name, modules, "", &[privacy_orange(), privacy_red()])
}

/// `privacy_daemon` with `extra` appended to bar.toml (a `[privacy]` section pointing at fake
/// roots) and `quiet` the hues that must be absent from the bar once it is up.
fn privacy_daemon_with(name: &str, modules: &str, extra: &str, quiet: &[[u8; 3]]) -> (Sway, FakeHyprland, Daemon) {
    privacy_daemon_before(name, modules, extra, quiet, |_| ())
}

/// `privacy_daemon_with`, `before` running once sway is up and before the daemon starts.
fn privacy_daemon_before(
    name: &str,
    modules: &str,
    extra: &str,
    quiet: &[[u8; 3]],
    before: impl FnOnce(&Sway),
) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, 1);
    before(&sway);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\nmodule_colors = \"none\"\n").unwrap();
    fake_recorder(&sway, "wpctl");
    let toml = format!("{modules}[resources]\nsensor = \"alpheus-it-no-sensor\"\n{extra}");
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(&toml), pw_dump: PwDump::Fifo, ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(500)); // pw-dump's initial state read, the bar drawn
    let shot = sway.screenshot("HEADLESS-1").expect("grim");
    for &hue in quiet {
        assert_eq!(on_the_bar(&shot, hue), None, "no glyph at rest, nothing else in {hue:?}: {:?}", d.log());
    }
    (sway, hypr, d)
}

/// The rest tests' bar: the default modules but the resources, which tick by design.
const PRIVACY_REST_BAR: &str = "modules = [\"workspaces\", \"window\", \"privacy\", \"volume\", \"clock\"]\n";

fn privacy_orange() -> [u8; 3] {
    rgb(preset("catppuccin").unwrap().dark.hues.orange)
}

fn privacy_green() -> [u8; 3] {
    rgb(preset("catppuccin").unwrap().dark.hues.green)
}

fn privacy_red() -> [u8; 3] {
    rgb(preset("catppuccin").unwrap().dark.hues.red)
}

/// Whether `p` is `colour` drawn over the bar's background at half coverage or more: on the line
/// from the background to `colour`, at least half way, within a few levels of it. The screen glyph
/// is thin outlines: anti-aliased, no pixel of it is fully covered at scale 1.
fn inked_in(p: [u8; 3], colour: [u8; 3]) -> bool {
    let bg = bar_bg("catppuccin").map(f64::from);
    let (p, c) = (p.map(f64::from), colour.map(f64::from));
    let (d, v): (Vec<f64>, Vec<f64>) = (0..3).map(|i| (c[i] - bg[i], p[i] - bg[i])).unzip();
    let t = (0..3).map(|i| d[i] * v[i]).sum::<f64>() / (0..3).map(|i| d[i] * d[i]).sum::<f64>();
    let off = (0..3).map(|i| (v[i] - t * d[i]).powi(2)).sum::<f64>().sqrt();
    t >= 0.5 && off <= 16.0
}

/// The leftmost and rightmost x of the pixels inked in `colour` on the bar. None: none there.
fn on_the_bar(s: &Ppm, colour: [u8; 3]) -> Option<(u32, u32)> {
    let xs: Vec<u32> = (0..BAR_HEIGHT)
        .flat_map(|y| (0..s.width).map(move |x| (x, y)))
        .filter(|&(x, y)| s.at(x, y).is_some_and(|p| inked_in(p, colour)))
        .map(|(x, _)| x)
        .collect();
    Some((*xs.iter().min()?, *xs.iter().max()?))
}

/// Waits until a glyph in `colour` is on the bar; returns the middle of its x span.
fn wait_for_the_glyph(sway: &Sway, d: &Daemon, colour: [u8; 3]) -> u32 {
    let at = Cell::new(None);
    let shown = sway.wait_for_screen("HEADLESS-1", |s| {
        at.set(on_the_bar(s, colour));
        at.get().is_some()
    });
    assert!(shown, "the glyph {colour:?} never drawn: {:?}", d.log());
    let (x0, x1) = at.get().unwrap();
    (x0 + x1) / 2
}

/// Screenshots until no glyph in `colour` is on the bar (10 s at most); how long that took from
/// `since`, or None if it never went.
fn glyph_gone_after(sway: &Sway, colour: [u8; 3], since: Instant) -> Option<Duration> {
    while since.elapsed() < DELAY {
        if sway.screenshot("HEADLESS-1").is_some_and(|s| on_the_bar(&s, colour).is_none()) {
            return Some(since.elapsed());
        }
        thread::sleep(Duration::from_millis(20));
    }
    None
}

/// The microphone lit by `firefox` (node 200, link 300); returns the glyph's x.
fn firefox_records(sway: &Sway, d: &Daemon) -> u32 {
    pw_dump_send(&sway.runtime, &capture_delta(200, "firefox", "running", 300, true));
    wait_for_the_glyph(sway, d, privacy_orange())
}

/// Waits until the privacy popup is on screen: its heading's glyph (orange, below the bar), then,
/// one 22 px row down, a name 7 cells wide starting 4 cells to the right of the heading's glyph
/// (`    firefox`, privacy spec § 5), before the popup's right frame. "privacy popup shown" is logged before the
/// configure and the first buffer: a key or click in that gap is lost.
fn wait_for_the_privacy_popup(sway: &Sway, d: &Daemon) {
    wait_for_the_privacy_popup_in(sway, d, privacy_orange(), 45..=70);
}

/// `wait_for_the_privacy_popup` for a heading in `orange`'s place: the camera's green. Its holder's
/// name, `fakecam`, is 7 cells as `firefox` is.
fn wait_for_the_privacy_popup_in(sway: &Sway, d: &Daemon, heading_hue: [u8; 3], name_width: std::ops::RangeInclusive<u32>) {
    let (orange, bg) = (heading_hue, popup_bg("catppuccin"));
    let drawn = |s: &Ppm| {
        let heading: Vec<(u32, u32)> = (BAR_HEIGHT + 2..s.height.min(120))
            .flat_map(|y| (0..s.width).map(move |x| (x, y)))
            .filter(|&(x, y)| s.at(x, y) == Some(orange))
            .collect();
        let (Some(left), Some(top), Some(bottom)) =
            (heading.iter().map(|p| p.0).min(), heading.iter().map(|p| p.1).min(), heading.iter().map(|p| p.1).max())
        else {
            return false;
        };
        // The name's row: 22 px under the heading's, on the popup's background.
        let middle = (top + bottom) / 2 + 22;
        let ink: Vec<u32> = (middle - 8..middle + 8)
            .flat_map(|y| (left..left + 100).map(move |x| (x, y)))
            .filter(|&(x, y)| s.at(x, y).is_some_and(|c| c != bg))
            .map(|(x, _)| x)
            .collect();
        let (Some(from), Some(to)) = (ink.iter().min(), ink.iter().max()) else {
            return false;
        };
        // 4 cells in (≈ 34 px; the scan stops short of the narrowest popup's right frame, the camera's), 7 cells wide (≈ 59 px), a few px of glyph bearing either way.
        (24..=44).contains(&(from - left)) && name_width.contains(&(to - from))
    };
    assert!(sway.wait_for_screen("HEADLESS-1", drawn), "the popup with its holder drawn: {:?}", d.log());
}

/// Privacy spec § 2, § 4: a recording stream lights the microphone in orange; its removal hides
/// it after the 0.5 s off-delay, through the timer (never a later message).
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_a_recording_stream_shows_the_microphone_then_it_goes() {
    let (sway, _hypr, d) = privacy_daemon("privacy-mic", "");
    firefox_records(&sway, &d);
    let sent = Instant::now();
    pw_dump_send(&sway.runtime, &capture_gone(200, 300));
    let gone = glyph_gone_after(&sway, privacy_orange(), sent);
    assert!(gone.is_some_and(|t| t >= Duration::from_millis(400)), "kept through the off-delay: {gone:?} {:?}", d.log());
    assert!(gone.is_some_and(|t| t <= Duration::from_secs(2)), "gone after the off-delay: {gone:?} {:?}", d.log());
}

/// Privacy spec § 2: a meter (`stream.monitor`) linked and running never lights it.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_a_meter_never_lights_it() {
    let (sway, _hypr, d) = privacy_daemon("privacy-meter", "");
    let meter = capture_delta(201, "pavucontrol", "running", 301, true)
        .replace(r#""application.name":"pavucontrol""#, r#""application.name":"pavucontrol","stream.monitor":"true""#);
    assert!(meter.contains("stream.monitor"), "{meter}");
    pw_dump_send(&sway.runtime, &meter);
    thread::sleep(Duration::from_secs(1));
    let shot = sway.screenshot("HEADLESS-1").expect("grim");
    assert_eq!(on_the_bar(&shot, privacy_orange()), None, "a meter is no recording: {:?}", d.log());
    // A recorder sent after it lights the glyph: the meter's batch, through the same pipe first, was read.
    firefox_records(&sway, &d);
}

/// Privacy spec § 5, § 6: a left click opens the popup listing the app; Escape closes it; a middle
/// click toggles the default source's mute through wpctl.
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn privacy_left_click_lists_the_app_and_middle_click_mutes() {
    let (sway, _hypr, d) = privacy_daemon("privacy-click", "");
    let x = firefox_records(&sway, &d);
    sway.click(x, MIDDLE, "button1");
    assert!(d.wait_for("privacy popup shown", DELAY), "{:?}", d.log());
    wait_for_the_privacy_popup(&sway, &d);
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
    assert!(recorded(&sway, "wpctl").is_empty(), "neither the click nor Escape ran wpctl");
    sway.click(x, MIDDLE, "button2");
    assert!(wait_for_call(&sway, "wpctl", "wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle"), "{:?} {:?}", recorded(&sway, "wpctl"), d.log());
    assert_eq!(d.count("privacy popup shown"), 1, "the middle click opens nothing: {:?}", d.log());
}

/// Privacy spec § 4, § 5: the open popup stays through the off-delay, a capture
/// back within it keeps it open, and once the capture ends for good it closes with
/// "nothing captured" no sooner than the delay; then nothing wakes (the timer went with it).
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn privacy_the_popup_closes_when_the_capture_ends() {
    let (sway, _hypr, d) = privacy_daemon("privacy-close", PRIVACY_REST_BAR);
    let x = firefox_records(&sway, &d);
    sway.click(x, MIDDLE, "button1");
    assert!(d.wait_for("privacy popup shown", DELAY), "{:?}", d.log());
    wait_for_the_privacy_popup(&sway, &d);
    // Gone, then back at once: well inside the off-delay.
    pw_dump_send(&sway.runtime, &capture_gone(200, 300));
    pw_dump_send(&sway.runtime, &capture_delta(200, "firefox", "running", 300, true));
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("popup closed"), 0, "back within the delay: still open: {:?}", d.log());
    let sent = Instant::now();
    pw_dump_send(&sway.runtime, &capture_gone(200, 300));
    assert!(d.wait_for("popup closed (nothing captured)", DELAY), "{:?}", d.log());
    let closed = sent.elapsed();
    assert!(closed >= Duration::from_millis(400), "open through the off-delay, closed after {closed:?}: {:?}", d.log());
    assert_eq!(on_the_bar(&sway.screenshot("HEADLESS-1").expect("grim"), privacy_orange()), None, "the glyph went too");
    let rest = rest_over_5_s(&d);
    assert_eq!(rest, 0, "the popup closed, nothing pending: no timer ({rest} wake-ups): {:?}", d.log());
}

/// Privacy spec § 3: Hyprland's screencast events light the screen glyph in red; the release
/// counts after its 1 s hold, then the 0.5 s off-delay.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_hyprland_screencast_lights_the_screen() {
    let (sway, hypr, d) = privacy_daemon("privacy-cast", "");
    assert!(wait_until(|| hypr.write("screencast>>1,monitor\n")), "the daemon never connected to the fake Hyprland: {:?}", d.log());
    wait_for_the_glyph(&sway, &d, privacy_red());
    let sent = Instant::now();
    assert!(hypr.write("screencast>>0,monitor\n"));
    let gone = glyph_gone_after(&sway, privacy_red(), sent);
    assert!(gone.is_some_and(|t| t >= Duration::from_millis(1300)), "held, then delayed: {gone:?} {:?}", d.log());
    assert!(gone.is_some_and(|t| t <= Duration::from_secs(3)), "gone within 3 s: {gone:?} {:?}", d.log());
}

const SHARING_BAR: &str = "modules = [\"privacy\", \"notifications\", \"clock\"]\n";
const SHARING_ON: &str = "[notifications]\ndnd_while_sharing = true\n";

fn sharing_daemon(name: &str, extra: &str) -> (Sway, FakeHyprland, Daemon) {
    let (sway, hypr, d) = privacy_daemon_with(name, SHARING_BAR, extra, &[privacy_orange(), privacy_red()]);
    assert!(wait_until(|| hypr.write("screencast>>0,monitor\n")), "the daemon never connected to the fake Hyprland: {:?}", d.log());
    (sway, hypr, d)
}

/// Small features 1, decision 7: a screencast turns do-not-disturb on, its end turns it off.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn dnd_while_sharing_follows_a_hyprland_screencast() {
    let (sway, hypr, d) = sharing_daemon("dnd-sharing", SHARING_ON);
    let file = sway.runtime.join("state/alpheus/dnd");
    assert!(hypr.write("screencast>>1,monitor\n"));
    assert!(d.wait_for("notifications: do not disturb on (screen shared)", DELAY), "{:?}", d.log());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "sharing\n");
    assert!(hypr.write("screencast>>0,monitor\n"));
    assert!(d.wait_for("do not disturb off (screen sharing ended)", DELAY), "{:?}", d.log());
    assert!(!file.exists(), "the file is gone");
}

/// Decision 7 (controller's ruling): a timed do-not-disturb that ends while a screen is shared
/// hands over to sharing, so it stays on, and the cast's end turns it off.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn a_timed_dnd_ending_during_a_share_hands_over_to_sharing() {
    let (sway, hypr, d) = {
        let name = "dnd-timed-share";
        let (sway, hypr, d) = privacy_daemon_before(name, SHARING_BAR, SHARING_ON, &[privacy_orange(), privacy_red()], |sway| {
            write_dnd_file(sway, &format!("until {}\n", unix_now() + 20))
        });
        (sway, hypr, d)
    };
    let file = sway.runtime.join("state/alpheus/dnd");
    assert!(wait_until(|| hypr.write("screencast>>0,monitor\n")), "the daemon never connected to the fake Hyprland: {:?}", d.log());
    assert!(std::fs::read_to_string(&file).unwrap().starts_with("until "), "timed at start: {:?}", d.log());
    assert!(hypr.write("screencast>>1,monitor\n"));
    wait_for_the_glyph(&sway, &d, privacy_red());
    assert!(std::fs::read_to_string(&file).unwrap().starts_with("until "), "still timed while the share runs: {:?}", d.log());
    assert!(d.wait_for("notifications: do not disturb on (screen shared)", Duration::from_secs(25)), "{:?}", d.log());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "sharing\n");
    assert_eq!(d.count("do not disturb off"), 0, "handed over, never off: {:?}", d.log());
    assert!(hypr.write("screencast>>0,monitor\n"));
    assert!(d.wait_for("do not disturb off (screen sharing ended)", DELAY), "{:?}", d.log());
    assert!(!file.exists(), "the cast's end turned it off: {:?}", d.log());
}

/// Decision 7: the user's word wins over the capture, either way.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn the_users_choice_wins_over_sharing() {
    let (sway, hypr, d) = sharing_daemon("dnd-user-wins", SHARING_ON);
    let file = sway.runtime.join("state/alpheus/dnd");
    // Off by hand during a cast: the end leaves it off, and says nothing.
    assert!(hypr.write("screencast>>1,monitor\n"));
    assert!(d.wait_for("do not disturb on (screen shared)", DELAY), "{:?}", d.log());
    assert_eq!(script(&sway, &["--dnd", "off"], "").join().unwrap().0, EXIT_OK);
    // Another change of what the bar shows while the cast goes on: an edge, not a level, so the
    // cast does not turn it back on.
    firefox_records(&sway, &d);
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("(screen shared)"), 1, "an edge, never a level: {:?}", d.log());
    assert!(!file.exists(), "off by hand, off still: {:?}", d.log());
    assert!(hypr.write("screencast>>0,monitor\n"));
    thread::sleep(Duration::from_secs(3));
    assert_eq!(d.count("screen sharing ended"), 0, "{:?}", d.log());
    assert!(!file.exists(), "still off: {:?}", d.log());
    pw_dump_send(&sway.runtime, &capture_gone(200, 300));
    assert!(glyph_gone_after(&sway, privacy_orange(), Instant::now()).is_some(), "{:?}", d.log());
    // On by hand during a cast (the cast turned it on, then the user said on): the end keeps it.
    assert!(hypr.write("screencast>>1,monitor\n"));
    assert!(d.wait_for_count("do not disturb on (screen shared)", 2, DELAY), "{:?}", d.log());
    assert_eq!(script(&sway, &["--dnd", "on"], "").join().unwrap().0, EXIT_OK);
    assert!(hypr.write("screencast>>0,monitor\n"));
    thread::sleep(Duration::from_secs(3));
    assert_eq!(d.count("screen sharing ended"), 0, "{:?}", d.log());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "", "still on, untimed: {:?}", d.log());
}

/// The option needs `notifications` too (`dnd_load` runs only with it): without the module, a
/// screencast leaves do-not-disturb alone, and nothing is written.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn sharing_without_the_notifications_module_leaves_dnd_alone() {
    let (sway, hypr, d) =
        privacy_daemon_with("dnd-no-module", "modules = [\"privacy\", \"clock\"]\n", SHARING_ON, &[privacy_orange(), privacy_red()]);
    assert!(wait_until(|| hypr.write("screencast>>0,monitor\n")), "the daemon never connected to the fake Hyprland: {:?}", d.log());
    assert!(hypr.write("screencast>>1,monitor\n"));
    wait_for_the_glyph(&sway, &d, privacy_red());
    thread::sleep(Duration::from_secs(2));
    assert_eq!(d.count("screen shared"), 0, "{:?}", d.log());
    assert!(!sway.runtime.join("state/alpheus/dnd").exists());
}

#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn without_the_option_a_screencast_leaves_dnd_alone() {
    let (sway, hypr, d) = sharing_daemon("dnd-no-option", "");
    assert!(hypr.write("screencast>>1,monitor\n"));
    thread::sleep(Duration::from_secs(3));
    assert_eq!(d.count("screen shared"), 0, "{:?}", d.log());
    assert!(!sway.runtime.join("state/alpheus/dnd").exists());
}

/// Privacy spec § 8: once a capture ended and its glyph went, the loop sleeps (no timer left).
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_at_rest_no_wakeups_after_a_capture() {
    let (sway, _hypr, d) = privacy_daemon("privacy-rest", PRIVACY_REST_BAR);
    firefox_records(&sway, &d);
    pw_dump_send(&sway.runtime, &capture_gone(200, 300));
    assert!(glyph_gone_after(&sway, privacy_orange(), Instant::now()).is_some(), "{:?}", d.log());
    let rest = rest_over_5_s(&d);
    assert_eq!(rest, 0, "nothing pending: no timer ({rest} wake-ups): {:?}", d.log());
}

// ---- The camera opened outside PipeWire (privacy delivery 2). Fake `/dev` and `/proc` trees in a
// temporary directory, handed to the daemon through `[privacy] dev_dir` and `proc_dir`. ----

/// A fake `dev/video0` (a plain file) and a fake `proc`, both below one guard.
struct FakeCam {
    root: alpheus_commun::temp_path::TempPath,
}

impl FakeCam {
    fn new(name: &str) -> FakeCam {
        let root = alpheus_commun::temp_path::TempPath::dir(name);
        std::fs::create_dir_all(root.join("dev")).unwrap();
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::write(root.join("dev/video0"), "").unwrap();
        FakeCam { root }
    }

    /// The `[privacy]` section that points the daemon at the fake trees.
    fn toml(&self) -> String {
        format!("[privacy]\ndev_dir = \"{}\"\nproc_dir = \"{}\"\n", self.root.join("dev").display(), self.root.join("proc").display())
    }

    /// Process 4242, `fakecam`, holds `video0` on its descriptor 3. No event: see `trigger`.
    fn hold(&self) {
        let fd = self.root.join("proc/4242/fd");
        std::fs::create_dir_all(&fd).unwrap();
        std::fs::write(self.root.join("proc/4242/comm"), "fakecam\n").unwrap();
        std::os::unix::fs::symlink(self.root.join("dev/video0"), fd.join("3")).unwrap();
    }

    fn release(&self) {
        std::fs::remove_dir_all(self.root.join("proc/4242")).unwrap();
    }

    /// An open and a close of the node: what the daemon's watch sees of any holder change.
    fn trigger(&self) {
        drop(std::fs::File::open(self.root.join("dev/video0")).unwrap());
    }
}

/// The camera tests' daemon: only the modules the glyph needs (no other green on the bar), the fake
/// trees, and, once up, no orange nor red; no green either unless `held`.
fn privacy_camera_daemon(name: &str, cam: &FakeCam, held: bool) -> (Sway, FakeHyprland, Daemon) {
    let quiet: &[[u8; 3]] = if held { &[privacy_orange(), privacy_red()] } else { &[privacy_orange(), privacy_red(), privacy_green()] };
    let out = privacy_daemon_with(name, PRIVACY_REST_BAR, &cam.toml(), quiet);
    assert!(out.2.wait_for("privacy: watching 1 video device(s)", DELAY), "{:?}", out.2.log());
    out
}

/// Privacy delivery 2 spec § 1-2: a holder plus one open and close lights the camera in green
/// within a second; once it is gone and a trigger follows, the glyph goes within 1.5 s (the 300 ms
/// debounce, the 0.5 s off-delay).
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_a_camera_opened_directly_shows_then_goes() {
    let cam = FakeCam::new("privacy-direct");
    let (sway, _hypr, d) = privacy_camera_daemon("privacy-direct", &cam, false);
    cam.hold();
    let sent = Instant::now();
    cam.trigger();
    wait_for_the_glyph(&sway, &d, privacy_green());
    assert!(sent.elapsed() <= Duration::from_secs(1), "shown within 1 s: {:?} {:?}", sent.elapsed(), d.log());
    cam.release();
    let sent = Instant::now();
    cam.trigger();
    let gone = glyph_gone_after(&sway, privacy_green(), sent);
    assert!(gone.is_some_and(|t| t <= Duration::from_millis(1500)), "gone within 1.5 s: {gone:?} {:?}", d.log());
}

/// A camera held before the daemon starts lights at once, with no event.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_a_camera_held_before_start_shows() {
    let cam = FakeCam::new("privacy-held");
    cam.hold();
    let (sway, _hypr, d) = privacy_camera_daemon("privacy-held", &cam, true);
    wait_for_the_glyph(&sway, &d, privacy_green());
}

/// An open and close with nobody holding the node lights nothing, whatever the scan
/// timing (after the 300 ms debounce, and past it).
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_a_camera_blip_lights_nothing() {
    let cam = FakeCam::new("privacy-blip");
    let (sway, _hypr, d) = privacy_camera_daemon("privacy-blip", &cam, false);
    cam.trigger();
    for ms in [400, 400] {
        thread::sleep(Duration::from_millis(ms));
        let shot = sway.screenshot("HEADLESS-1").expect("grim");
        assert_eq!(on_the_bar(&shot, privacy_green()), None, "a blip lights nothing: {:?}", d.log());
    }
    // The watch was alive all along: a real holder, then a trigger, lights it.
    cam.hold();
    cam.trigger();
    wait_for_the_glyph(&sway, &d, privacy_green());
}

/// The harness points every daemon's camera watch at empty directories: a default test daemon
/// watches no device (never the machine's /dev), and the camera glyph stays dark.
#[test]
#[ignore = "headless sway and grim (nix develop)"]
fn privacy_a_default_test_daemon_watches_no_real_device() {
    let (sway, _hypr, d) = privacy_daemon_with("privacy-guard", "", "", &[privacy_orange(), privacy_red(), privacy_green()]);
    assert!(d.wait_for("privacy: watching 0 video device(s)", DELAY), "{:?}", d.log());
    drop(sway);
}

/// Privacy delivery 2 spec § 5: the popup names a direct holder under the camera heading.
#[test]
#[ignore = "headless sway, grim and wtype (nix develop)"]
fn privacy_the_popup_names_a_direct_camera() {
    let cam = FakeCam::new("privacy-popup");
    cam.hold();
    let (sway, _hypr, d) = privacy_camera_daemon("privacy-popup", &cam, true);
    let x = wait_for_the_glyph(&sway, &d, privacy_green());
    sway.click(x, MIDDLE, "button1");
    assert!(d.wait_for("privacy popup shown", DELAY), "{:?}", d.log());
    // `fakecam` measures 57 px; `(unnamed)`, 9 cells, 63.
    wait_for_the_privacy_popup_in(&sway, &d, privacy_green(), 52..=60);
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed (escape)", DELAY), "{:?}", d.log());
}

/// A daemon on `sway`, reading it as its compositor, displays on (the default modules).
fn displays_daemon(sway: &Sway) -> Daemon {
    displays_daemon_with(sway, None)
}

fn displays_daemon_with(sway: &Sway, bar_toml: Option<&str>) -> Daemon {
    let d = start_daemon_with(sway, &DaemonOptions { bar_toml, backend: Backend::Sway, ..DaemonOptions::default() });
    ready(&d);
    d
}

/// Displays protocol spec: the heads come from `zwlr_output_manager_v1`, answered from memory.
#[test]
#[ignore = "headless sway (nix develop)"]
fn outputs_are_read_from_the_protocol() {
    let sway = Sway::start("displays-read", 2);
    let d = displays_daemon(&sway);
    let s = outputs_snapshot(&sway, &d, 2..=2);
    assert_eq!(s.status, Status::Ready);
    assert_eq!(s.trial, TrialState::None);
    let mut names: Vec<&str> = s.outputs.iter().map(|o| o.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"]);
    assert!(s.outputs.iter().all(|o| o.active.is_some()), "both on: {s:?}");
}

/// `outputs-try` of `name` as it is, at scale `scale` (thousandths).
fn try_scale(s: &Snapshot, name: &str, scale: u32) -> Request {
    let active = output(s, name).active.clone().expect("enabled");
    Request::OutputsTry(wire::encode_desired(&[Step { output: name.into(), active: Some(Active { scale: Scale(scale), ..active }) }]))
}

/// Displays protocol spec, the trial: a try answers done once the compositor applied it, the
/// next snapshot shows it with the trial open; keep ends it as kept, and the scale stays.
#[test]
#[ignore = "headless sway (nix develop)"]
fn a_try_applies_and_keep_keeps() {
    let sway = Sway::start("displays-keep", 1);
    let d = displays_daemon(&sway);
    let before = outputs_snapshot(&sway, &d, 1..=1);
    assert_eq!(output(&before, "HEADLESS-1").active.as_ref().map(|a| a.scale), Some(Scale(1000)));
    assert_eq!(displays(&sway, &try_scale(&before, "HEADLESS-1", 2000)), Reply::Done, "{:?}", d.log());
    let s = snapshot_until(&sway, &d, |s| output(s, "HEADLESS-1").active.as_ref().is_some_and(|a| a.scale == Scale(2000)));
    assert!(matches!(s.trial, TrialState::Open { seconds_left: 1..=15 }), "{s:?}");
    assert_eq!(displays(&sway, &Request::OutputsKeep), Reply::Done);
    let s = outputs_snapshot(&sway, &d, 1..=1);
    assert_eq!(s.trial, TrialState::Ended(TrialEnd::Kept), "reported once");
    assert_eq!(outputs_snapshot(&sway, &d, 1..=1).trial, TrialState::None);
    thread::sleep(Duration::from_secs(1));
    assert_eq!(output(&outputs_snapshot(&sway, &d, 1..=1), "HEADLESS-1").active.as_ref().map(|a| a.scale), Some(Scale(2000)));
    assert!(d.log().iter().all(|l| !l.contains("displays: configuration refused") && !l.contains("displays: no answer")), "{:?}", d.log());
}

/// Displays protocol spec, the trial: with no keep, the daemon brings the state from before
/// back at the 15 s deadline, without the TUI.
#[test]
#[ignore = "headless sway, 16 s (nix develop)"]
fn a_try_reverts_at_the_deadline() {
    let sway = Sway::start("displays-revert", 1);
    let d = displays_daemon(&sway);
    let before = outputs_snapshot(&sway, &d, 1..=1);
    assert_eq!(displays(&sway, &try_scale(&before, "HEADLESS-1", 2000)), Reply::Done, "{:?}", d.log());
    snapshot_until(&sway, &d, |s| output(s, "HEADLESS-1").active.as_ref().is_some_and(|a| a.scale == Scale(2000)));
    thread::sleep(Duration::from_secs(16));
    let s = outputs_snapshot(&sway, &d, 1..=1);
    assert_eq!(s.trial, TrialState::Ended(TrialEnd::Reverted), "{s:?}");
    assert_eq!(output(&s, "HEADLESS-1").active, output(&before, "HEADLESS-1").active, "the state from before");
}

/// Displays protocol spec, the TUI's errors: a mode the screen does not have is refused,
/// `the compositor refused this configuration`, and nothing changes. The refusal comes from the
/// controller (the screen has no such mode, so nothing is sent); a real compositor `failed` is
/// covered by the controller's unit tests only: headless sway refuses nothing the controller
/// lets through.
#[test]
#[ignore = "headless sway (nix develop)"]
fn a_refused_configuration_answers_refused() {
    let sway = Sway::start("displays-refused", 1);
    let d = displays_daemon(&sway);
    let before = outputs_snapshot(&sway, &d, 1..=1);
    let active = output(&before, "HEADLESS-1").active.clone().expect("enabled");
    let mode = Mode { width: 1234, height: 567, refresh: 75 };
    let body = wire::encode_desired(&[Step { output: "HEADLESS-1".into(), active: Some(Active { mode, ..active }) }]);
    assert_eq!(displays(&sway, &Request::OutputsTry(body)), Reply::Error("the compositor refused this configuration".into()));
    let s = outputs_snapshot(&sway, &d, 1..=1);
    assert_eq!((s.trial, s.outputs), (TrialState::None, before.outputs), "no trial, nothing changed");
}

/// Displays protocol spec, hotplug: a plugged screen whose set matches a profile of
/// displays.toml gets its positions once the debounce ended, and only once.
#[test]
#[ignore = "headless sway (nix develop)"]
fn hotplug_applies_the_matching_profile_once() {
    let sway = Sway::start("displays-hotplug", 1);
    // One profile for a screen that is not there: the start-up evaluation logs its line (none
    // is logged without a profile), and it comes before `quiet` is taken.
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("displays.toml"),
        "[[profile]]\nname = \"Elsewhere\"\n[[profile.display]]\ndescription = \"Nobody P0 X0\"\nmode = \"640x480@60\"\nplacement = { x = 0, y = 0 }\n",
    )
    .unwrap();
    // No module that refreshes on its own (resources every 2 s): the loop rests between events.
    let d = displays_daemon_with(&sway, Some("modules = [\"workspaces\", \"displays\"]\n"));
    let first = outputs_snapshot(&sway, &d, 1..=1).outputs[0].description.clone();
    assert!(d.wait_for("displays: no profile for these screens", DELAY), "{:?}", d.log());
    // The plugged head's description carries a counter of the headless backend: one profile per
    // value it may take, the same placement in each (only the set that is there matches).
    let profiles: String = (2..=9)
        .map(|n| {
            format!(
                "[[profile]]\nname = \"Desk {n}\"\n\
                 [[profile.display]]\ndescription = \"Headless output {n}\"\nmode = \"1920x1080@0\"\nplacement = {{ x = 0, y = 0 }}\n\
                 [[profile.display]]\ndescription = \"{first}\"\nmode = \"1280x720@0\"\nplacement = {{ x = 1920, y = 0 }}\n"
            )
        })
        .collect();
    std::fs::write(sway.runtime.join("config/alpheus/displays.toml"), profiles).unwrap();
    let quiet = d.log().len();
    sway.command("create_output");
    let placed = |s: &Snapshot| {
        s.outputs.len() == 2
            && s.trial == TrialState::None
            && output(s, "HEADLESS-2").active.as_ref().is_some_and(|a| (a.x, a.y) == (0, 0))
            && output(s, "HEADLESS-1").active.as_ref().is_some_and(|a| (a.x, a.y) == (1920, 0))
    };
    let s = snapshot_until(&sway, &d, placed);
    // An apply logs nothing (the spec's fixed lines): a second evaluation would show as the
    // loop waking each debounce, with no request on the socket meanwhile. The snapshot request
    // and the apply's last events settle first: no wake-up for 300 ms.
    let quiet_since = |d: &Daemon| {
        let (start, mut last, mut since) = (Instant::now(), d.main_thread_wakeups(), Instant::now());
        while since.elapsed() < Duration::from_millis(300) {
            assert!(start.elapsed() < DELAY, "the loop never settled: {:?}", d.log());
            thread::sleep(Duration::from_millis(20));
            let now = d.main_thread_wakeups();
            if now != last {
                (last, since) = (now, Instant::now());
            }
        }
    };
    quiet_since(&d);
    let before = d.main_thread_wakeups();
    thread::sleep(Duration::from_secs(2));
    let wakeups = d.main_thread_wakeups() - before;
    assert_eq!(wakeups, 0, "{wakeups} wake-ups of the loop in 2 s once applied");
    assert_eq!(outputs_snapshot(&sway, &d, 2..=2), s, "unchanged 2 s later");
    let lines: Vec<String> = d.log()[quiet..].iter().filter(|l| l.contains("displays:")).cloned().collect();
    assert!(lines.is_empty(), "applied, nothing refused, no other evaluation: {lines:?}");
}

/// The timer chip: the first module of the bar, so cell 1 (≈ 8 to 16 px), as the power button alone.
const TIMER_AT: (u32, u32) = (12, 10);

/// The timer's bar (catppuccin dark) on a private session bus (the notifications tests need one):
/// `modules` as a TOML array, `[timer] sound` touching `<runtime>/rang` (coreutils' `touch`, a
/// private path), `extra` appended. Never the real state directory: `XDG_STATE_HOME` is the
/// runtime's.
fn timer_daemon(name: &str, modules: &str, extra: &str) -> (PrivateBus, Sway, FakeHyprland, Daemon) {
    let bus = PrivateBus::start(name);
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), "theme = \"catppuccin\"\nmode = \"dark\"\n").unwrap();
    let toml = timer_toml(&sway, modules, extra);
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(&toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(300)); // configured and drawn
    (bus, sway, hypr, d)
}

/// The timer bar's `bar.toml`: `modules` (a TOML array), the sound touching `<runtime>/rang`, `extra`.
fn timer_toml(sway: &Sway, modules: &str, extra: &str) -> String {
    format!("modules = {modules}\n[timer]\nsound = [\"touch\", \"{}\"]\n{extra}", sway.runtime.join("rang").display())
}

/// `alpheus-launch <args>` against the daemon: its exit code, stdout and stderr.
fn timer_verb(sway: &Sway, args: &[&str]) -> (i32, String, String) {
    script(sway, args, "").join().unwrap()
}

/// Waits until the timer popup is on screen: its background fills a good part of the area under
/// the chip ("timer shown" is logged when the popup is asked for, before its first buffer).
fn timer_drawn(sway: &Sway, d: &Daemon) {
    let bg = rgb(preset("catppuccin").unwrap().dark.popup_bg);
    let filled = |s: &Ppm| (29..60).flat_map(|y| (14..60).map(move |x| (x, y))).filter(|&(x, y)| s.at(x, y) == Some(bg)).count() > 300;
    assert!(sway.wait_for_screen("HEADLESS-1", filled), "the timer popup drawn: {:?}", d.log());
}

/// Timer spec § 3: a click on the chip opens the popup (idle: its Timer row selected); Enter starts
/// the countdown of the row's length (10 min), the popup stays open; Escape closes it; the state
/// file says it runs.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn timer_a_click_opens_the_popup_and_enter_starts_a_countdown() {
    let (_bus, sway, _hypr, d) = timer_daemon("timer-popup", "[\"timer\", \"clock\"]", "");
    sway.click(TIMER_AT.0, TIMER_AT.1, "button1");
    assert!(d.wait_for("timer shown (idle)", DELAY), "{:?}", d.log());
    timer_drawn(&sway, &d);
    keys(&sway, &["Return"]);
    assert!(d.wait_for("timer: countdown started (10 min)", DELAY), "{:?}", d.log());
    keys(&sway, &["Escape"]);
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    let file = std::fs::read_to_string(sway.runtime.join("state/alpheus/timer")).unwrap();
    assert!(file.starts_with("countdown running "), "{file:?}");
}

/// Plan decision 4, F6: with the popup open and a countdown running the loop wakes each second;
/// once the popup is closed (a second click on the chip: Escape's key-up and repeat make one
/// unrelated wake-up of their own) it does not wake at all, the countdown's next chip change being
/// a minute away; the window starts right after the close's own round trip (a barrier verb), so a
/// one-second timer left armed by it shows.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn timer_the_popups_second_tick_stops_at_close() {
    // The whole test fits between two minute turns (the clock's redraw is a wake-up of its own).
    while !(2..=35).contains(&alpheus_demon::clock::LocalTime::now().0.second) {
        thread::sleep(Duration::from_millis(200));
    }
    let (_bus, sway, _hypr, d) = timer_daemon("timer-tick", "[\"timer\", \"clock\"]", "");
    sway.click(TIMER_AT.0, TIMER_AT.1, "button1");
    assert!(d.wait_for("timer shown (idle)", DELAY), "{:?}", d.log());
    timer_drawn(&sway, &d);
    keys(&sway, &["Return"]);
    assert!(d.wait_for("timer: countdown started (10 min)", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_secs(1)); // Return's own key-up and repeat events settle
    let before = d.main_thread_wakeups();
    thread::sleep(Duration::from_secs(3));
    let open = d.main_thread_wakeups().saturating_sub(before);
    assert!(open >= 2, "open: the loop wakes each second, {open} in 3 s: {:?}", d.log());
    sway.click(TIMER_AT.0, TIMER_AT.1, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    // A barrier: a verb with nothing to act on is answered once the loop has handled everything the
    // close queued before it (the buffer's release), so the window starts after the close's replies.
    assert_eq!(timer_verb(&sway, &["--timer", "resume"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: nothing to resume", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(100)); // the verb's own hang-up
    let (before, ticks) = (d.main_thread_wakeups(), d.main_thread_cpu_ticks());
    thread::sleep(Duration::from_secs(3));
    assert_no_spin(d.main_thread_cpu_ticks().saturating_sub(ticks), Duration::from_secs(3), "the loop thread");
    let closed = d.main_thread_wakeups().saturating_sub(before);
    assert_eq!(closed, 0, "closed: no second tick, {closed} wake-ups in 3 s: {:?}", d.log());
}

const TIMER_END_MODULES: &str = "[\"timer\", \"notifications\", \"clock\"]";

/// Timer spec § 5, § 7: `--timer start 2s` ends with the notification (a bubble) and the sound
/// (the stub touches `<runtime>/rang`), and the state file is gone once it ended.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn timer_a_short_countdown_ends_with_a_bubble_and_the_sound() {
    let (_bus, sway, _hypr, d) = timer_daemon("timer-end", TIMER_END_MODULES, "");
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    ready(&d);
    assert_eq!(timer_verb(&sway, &["--timer", "start", "2s"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: countdown started (2 s)", DELAY), "{:?}", d.log());
    assert!(sway.runtime.join("state/alpheus/timer").exists(), "saved while it runs");
    assert!(d.wait_for("timer: countdown ended", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bubbles shown: 1", DELAY), "{:?}", d.log());
    wait_for_file(&sway.runtime.join("rang"), DELAY);
    assert!(wait_until(|| !sway.runtime.join("state/alpheus/timer").exists()), "the state file goes with the end: {:?}", d.log());
}

/// Plan decision 8: under do-not-disturb the end goes to the history only (no bubble) and makes no sound.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn timer_under_dnd_ends_without_bubble_or_sound() {
    let (_bus, sway, _hypr, d) = timer_daemon("timer-dnd", TIMER_END_MODULES, "");
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    ready(&d);
    assert_eq!(script(&sway, &["--dnd", "on"], "").join().unwrap(), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    // The bar's left part (the chip and the bell; the clock is further right): its pixels with the
    // chip idle and no count, and again once the end is dismissed, the history holding the notice.
    let left = |s: &Ppm| (2..20).flat_map(|y| (0..120).map(move |x| (x, y))).filter_map(|(x, y)| s.at(x, y)).collect::<Vec<_>>();
    let before = sway.screenshot("HEADLESS-1").map(|s| left(&s)).unwrap();
    assert_eq!(timer_verb(&sway, &["--timer", "start", "2s"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: countdown ended", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_secs(1));
    assert!(!d.log().iter().any(|l| l.contains("bubbles shown")), "no bubble: {:?}", d.log());
    assert!(!sway.runtime.join("rang").exists(), "no sound");
    // The positive side: the notice is in the history, so the bell counts it. Dismissing `done`
    // (any verb sees it) puts the chip back to its idle glyph, so only the count differs.
    assert_eq!(timer_verb(&sway, &["--timer", "stop"]), (EXIT_OK, String::new(), String::new()));
    assert!(
        sway.wait_for_screen("HEADLESS-1", |s| left(s) != before),
        "the bell counts the notice kept under do-not-disturb: {:?}",
        d.log()
    );
}

/// Timer spec § 6: a running countdown is on file, and a daemon started on it restores it.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn timer_survives_a_daemon_restart() {
    let (bus, sway, _hypr, mut d) = timer_daemon("timer-restart", TIMER_END_MODULES, "");
    ready(&d);
    assert_eq!(timer_verb(&sway, &["--timer", "start", "30m"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: countdown started (30 min)", DELAY), "{:?}", d.log());
    let file = std::fs::read_to_string(sway.runtime.join("state/alpheus/timer")).unwrap();
    assert!(file.starts_with("countdown running "), "{file:?}");
    d.signal(libc::SIGTERM);
    assert_eq!(d.exit(DELAY), Some(0), "{:?}", d.log());
    let toml = timer_toml(&sway, TIMER_END_MODULES, "");
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(&toml), session_bus: Some(&bus.address), ..DaemonOptions::default() });
    assert!(d.wait_for("timer: restored countdown", DELAY), "{:?}", d.log());
}

/// Without the module the verbs are an error (exit 2, named); with it, a verb with nothing to act
/// on is Done and says so in the log.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn timer_verbs_need_the_module() {
    let (_bus, sway, _hypr, d) = timer_daemon("timer-off", "[\"clock\"]", "");
    ready(&d);
    let (code, out, err) = timer_verb(&sway, &["--timer", "stop"]);
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""), "{err}");
    assert!(err.contains("the timer module is not in modules"), "{err}");
    drop(d);
    let (_bus, sway, _hypr, d) = timer_daemon("timer-nothing", "[\"timer\", \"clock\"]", "");
    ready(&d);
    // A `done` chip: the first verb dismisses it (Seen changed something, so no line), the next
    // has nothing to act on.
    assert_eq!(timer_verb(&sway, &["--timer", "start", "1s"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: countdown ended", DELAY), "{:?}", d.log());
    assert_eq!(timer_verb(&sway, &["--timer", "skip"]), (EXIT_OK, String::new(), String::new()));
    assert_eq!(d.count("timer: nothing to skip"), 0, "dismissing done is something: {:?}", d.log());
    assert_eq!(timer_verb(&sway, &["--timer", "skip"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: nothing to skip", DELAY), "{:?}", d.log());
}

/// Plan decision 17: `--timer popup` opens the popup under the chip (not the bar's right end: the
/// drawn background fills the area under the chip's cell), a second within 250 ms is the press
/// bind's refire and is ignored, a later one closes it.
#[test]
#[ignore = "headless sway, grim and dbus-daemon (nix develop)"]
fn timer_popup_verb_opens_and_closes_it() {
    let (_bus, sway, _hypr, d) = timer_daemon("timer-verb", "[\"timer\", \"clock\"]", "");
    ready(&d);
    assert_eq!(timer_verb(&sway, &["--timer", "popup"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer shown (idle)", DELAY), "{:?}", d.log());
    timer_drawn(&sway, &d);
    assert_eq!(timer_verb(&sway, &["--timer", "popup"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: popup debounced", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(timer_verb(&sway, &["--timer", "popup"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("popup closed (verb)", DELAY), "{:?}", d.log());
}

/// Ruling (final review M3): an end seen in the open popup is seen. The popup is open when a
/// countdown ends and is closed afterwards; the next verb then finds nothing to dismiss (without
/// the `Seen` at close, `skip` would have dismissed `done` and logged nothing).
#[test]
#[ignore = "headless sway, grim and dbus-daemon (nix develop)"]
fn timer_an_end_seen_in_the_open_popup_is_seen() {
    let (_bus, sway, _hypr, d) = timer_daemon("timer-seen", "[\"timer\", \"clock\"]", "");
    ready(&d);
    assert_eq!(timer_verb(&sway, &["--timer", "popup"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer shown (idle)", DELAY), "{:?}", d.log());
    assert_eq!(timer_verb(&sway, &["--timer", "start", "2s"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: countdown ended", DELAY), "{:?}", d.log());
    thread::sleep(Duration::from_millis(300)); // past the verb's own debounce
    assert_eq!(timer_verb(&sway, &["--timer", "popup"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("popup closed (verb)", DELAY), "{:?}", d.log());
    assert_eq!(timer_verb(&sway, &["--timer", "skip"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: nothing to skip", DELAY), "the end was seen at close: {:?}", d.log());
}

/// A popup opened while idle follows a change made elsewhere: after `--timer start 5m` its first
/// action is Pause, so Enter pauses and never restarts the countdown.
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn timer_the_open_popup_follows_a_change_made_outside() {
    let (_bus, sway, _hypr, d) = timer_daemon("timer-follow", "[\"timer\", \"clock\"]", "");
    sway.click(TIMER_AT.0, TIMER_AT.1, "button1");
    assert!(d.wait_for("timer shown (idle)", DELAY), "{:?}", d.log());
    timer_drawn(&sway, &d);
    assert_eq!(timer_verb(&sway, &["--timer", "start", "5m"]), (EXIT_OK, String::new(), String::new()));
    assert!(d.wait_for("timer: countdown started (5 min)", DELAY), "{:?}", d.log());
    keys(&sway, &["Return"]);
    assert!(d.wait_for("timer: paused", DELAY), "{:?}", d.log());
    assert_eq!(d.count("timer: countdown started"), 1, "Enter never restarted it: {:?}", d.log());
}

/// Each test gives its compositor a name, and the name picks its runtime directory
/// (`alpheus-it-<pid>-<name>`): two tests sharing one would remove each other's directory and
/// fight over the same sockets when run in parallel. Not ignored: it only reads this file.
#[test]
fn every_test_compositor_has_its_own_name() {
    // Everything above this test: its own list of starters is not a test's name.
    let source = include_str!("headless_session.rs").split("fn every_test_compositor_has_its_own_name").next().unwrap_or("");
    let starters = ["Sway::start(", "tray_bar(", "menu_bar(", "clock_bar(", "theme_bar(", "shape_bar(", "media_daemon(", "timer_daemon("];
    let mut names = Vec::new();
    for starter in starters {
        for (at, _) in source.match_indices(starter) {
            let rest = &source[at + starter.len()..];
            if let Some(name) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) {
                names.push(name);
            }
        }
    }
    assert!(names.len() > 50, "the scan finds the tests' names: {}", names.len());
    names.sort_unstable();
    let shared: Vec<_> = names.windows(2).filter(|w| w[0] == w[1]).map(|w| w[0]).collect();
    assert!(shared.is_empty(), "compositor names used by two tests: {shared:?}");
}

/// Small features 1, task 9: a laptop at 8 % at start, discharging, alerts once, for the lowest
/// threshold crossed (10 %), once the server holds the name (whichever of the reading and
/// the name comes first: an alert is never lost to the start-up order).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn battery_low_at_start_alerts_once_for_the_lowest_threshold() {
    let system = PrivateBus::start("battery-alert-system");
    let _workstation = Workstation::serve_with_display(&system, 8.0, 2);
    let bus = PrivateBus::start("battery-alert-session");
    let sway = Sway::start("battery-alert", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = "modules = [\"battery\", \"notifications\", \"clock\"]\n";
    let d = start_daemon_with(
        &sway,
        &DaemonOptions {
            bar_toml: Some(toml),
            system_bus: Some(&system.address),
            session_bus: Some(&bus.address),
            ..DaemonOptions::default()
        },
    );
    assert!(d.wait_for("bell: serving", DELAY), "{:?}", d.log());
    assert!(d.wait_for("battery: alert at 10 %", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(d.count("battery: alert"), 1, "{:?}", d.log());
}

/// Small features 2, task 5: the panel's do-not-disturb detail, Enter on its default choice, sets a
/// 30-minute do-not-disturb through the one path (the log lines and the state file say so).
#[test]
#[ignore = "headless sway, grim, dbus-daemon and wtype (nix develop)"]
fn quick_the_dnd_detail_sets_a_timed_do_not_disturb() {
    let (_bus, sway, _hypr, d) =
        quick_daemon_on("quick-dnd-timed", "modules = [\"volume\", \"quick\", \"notifications\", \"clock\"]\n", "");
    assert_eq!(quick_verb(&sway, &["--quick"]), EXIT_OK);
    assert!(d.wait_for("quick shown (rows: ", DELAY), "{:?}", d.log());
    let line = d.log().into_iter().find(|l| l.contains("quick shown (rows: ")).unwrap();
    let rows = line.split("(rows: ").nth(1).unwrap().split(';').next().unwrap();
    let downs = rows.split_whitespace().position(|r| r == "dnd").expect(&line);
    assert!(wait_until(|| sway.seat_focus() == Some(0)), "open: the bar is Exclusive");
    let mut names = vec!["Down"; downs];
    names.extend(["Return", "Return"]);
    keys(&sway, &names);
    assert!(d.wait_for("quick: do not disturb for 30 min", DELAY), "{:?}", d.log());
    assert!(d.wait_for("notifications: do not disturb on (ends in 30 min)", DELAY), "{:?}", d.log());
    let text = std::fs::read_to_string(sway.runtime.join("state/alpheus/dnd")).unwrap();
    assert!(text.starts_with("until "), "{text:?}");
}
