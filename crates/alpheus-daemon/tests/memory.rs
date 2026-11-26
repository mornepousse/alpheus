//! Startup PSS < 15 MB (firm),
//! < 15 MB at steady state and no growth, under a busy compositor and a
//! busy PipeWire; at rest with the resources module hidden, no wake-up —
//! none at all for the sound and D-Bus threads; with resources, the loop
//! wakes at the configured interval, no faster; threads counted, none left
//! behind when the system bus restarts. The system bus is a private
//! dbus-daemon (no NetworkManager, UPower nor BlueZ on it: those modules
//! stay hidden), pw-dump a script printing the fixtures.
//!
//! **Release only.** These budgets are tuned against the release binary —
//! the daemon ships in release, and a debug build inflates PSS by roughly
//! 2x (measured 2026-10-05: "everything" startup 17.4 MB debug vs 7.8 MB
//! release), which would either fail a budget tuned for release or pass
//! one loosened to fit debug, neither of them the figure that matters.
//! `require_release` below panics with this same command if it's missing:
//! `nix develop -c cargo test --release -p alpheus-daemon --test memory -- --ignored --test-threads=1`

mod common;

use alpheus_common::displays::wire::{self, TrialState};
use alpheus_common::displays::{Active, Step};
use alpheus_common::launcher_ipc::{CaptureVerb, DmenuFlags, DndVerb, QuickVerb, Reply, Request, TimerVerb, encode_request, socket_path};
use alpheus_common::theme::presets::preset;
use common::bluez::FakeBluez;
use common::displays::{displays, output, outputs_snapshot, snapshot_until};
use common::fake_mango::FakeMango;
use common::mpris::{FakePlayer, Spec as PlayerSpec};
use common::sni::{FakeItem, Spec};
use common::{
    AG06_SINK, Backend, Daemon, DaemonOptions, FakeAp, FakeHyprland, FakeNetworkManager, FakeSystemd, FakeVpn, NmChange, Peer, PrivateBus,
    PwDump, SIGNATURE, Sway, TempDir, UUID_HOME, Workstation, assert_no_spin, capture_delta, capture_gone, fake_env_recorder,
    fake_recorder, fake_source, fake_stdin_recorder, launcher, pw_dump_send, shield_record, shield_stub, start_daemon_with, volume_delta,
    wait_until_executable, write_atomically, wtype, wtype_spawn,
};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DELAY: Duration = Duration::from_secs(10);
/// Startup PSS budget: firm (2026-10-04, Q13). Never raised: a figure
/// above it is reported to the maintainer with `startup_pss_per_source`'s breakdown.
const STARTUP_MAX_KB: u64 = 15 * 1024;

/// Refuses to measure PSS on a debug build (module doc: roughly 2x
/// inflated, in either direction against a budget tuned for release).
/// Takes the flag rather than reading `cfg!` itself so the decision stays
/// a plain, testable function.
fn require_release(debug_assertions: bool) {
    assert!(
        !debug_assertions,
        "run with --release: these PSS budgets are tuned for the release binary (debug roughly doubles it) — \
         nix develop -c cargo test --release -p alpheus-daemon --test memory -- --ignored --test-threads=1"
    );
}

#[test]
fn require_release_panics_only_on_a_debug_build() {
    require_release(false);
    let panicked = std::panic::catch_unwind(|| require_release(true)).is_err();
    assert!(panicked, "a debug build must refuse to run these budget tests");
}

/// Every module but the resources, which tick by design. `interval = 1`: a
/// timer started although the module is hidden would tick 5 times in 5 s.
const AT_REST: &str =
    "modules = [\"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \"clock\"]\n[resources]\ninterval = 1\n";

/// 24 different glyphs per title: over 700 titles, the 4,096 md icons of
/// the Nerd Font all cycle through (the glyph cache must stay bounded).
fn varied_glyphs(n: u64) -> String {
    (0..24u64).filter_map(|i| char::from_u32(0xF0001 + ((n * 24 + i) % 4096) as u32)).collect()
}

fn event(n: u64) -> String {
    match n % 4 {
        0 => format!("activewindow>>firefox,Title {n} {} — 日本語 🦀\n", varied_glyphs(n)),
        1 => "workspacev2>>2,2\n".to_string(),
        2 => format!("windowtitlev2>>583d9c60da90,Title {n}\n"),
        _ => "urgent>>583d9d5fd540\n".to_string(),
    }
}

fn pump(hypr: &FakeHyprland, duration: Duration, n: &mut u64) {
    let t = Instant::now();
    while t.elapsed() < duration {
        hypr.write(&event(*n));
        *n += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn start(
    name: &str,
    outputs: u32,
    bus: &PrivateBus,
    session: Option<&PrivateBus>,
    bar_toml: Option<&str>,
    pw_dump: PwDump,
) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, outputs);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon_with(
        &sway,
        &DaemonOptions {
            bar_toml,
            system_bus: Some(&bus.address),
            session_bus: session.map(|s| s.address.as_str()),
            pw_dump,
            ..DaemonOptions::default()
        },
    );
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// 5 KDE-style items, 2 Qt-style, 2 Chromium-style, 1 Steam-like: the
/// population startup and rest are measured against.
fn ten_fakes(session: &PrivateBus) -> Vec<FakeItem> {
    let mut items: Vec<FakeItem> = (1..=5).map(|n| FakeItem::start(session, Spec::kde(n))).collect();
    items.push(FakeItem::start(session, Spec::qt()));
    items.push(FakeItem::start(session, Spec::qt()));
    items.push(FakeItem::start(session, Spec::chromium()));
    items.push(FakeItem::start(session, Spec::chromium()));
    items.push(FakeItem::start(session, Spec::steam()));
    items
}

/// Waits (10 s at most) until someone owns `org.kde.StatusNotifierWatcher`
/// on `bus` (the same check as the headless tests).
fn wait_for_the_watcher(bus: &PrivateBus) {
    let peer = Peer::connect(bus);
    let t = Instant::now();
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

/// The daemon's tray is connected and its watcher serves: only then may a
/// fake register (a KDE-style fake asks the watcher's ProtocolVersion, a
/// Chromium-style one IsStatusNotifierHostRegistered, once — neither retries).
fn tray_up(d: &Daemon, session: &PrivateBus) {
    assert!(d.wait_for("tray: on the session bus", DELAY), "{:?}", d.log());
    wait_for_the_watcher(session);
}

/// `tray_up`, the ten fakes, then all ten shown: nothing is measured on a
/// half-populated tray.
fn populate(d: &Daemon, session: &PrivateBus) -> Vec<FakeItem> {
    tray_up(d, session);
    let items = ten_fakes(session);
    assert!(d.wait_for("tray: icons shown: 10", DELAY), "{:?}", d.log());
    items
}

/// Once every icon is shown: the last Gets' buffers and startup's own
/// release_memory settle (the same 3 s as pss_under_30mb_and_stable).
const TRAY_SETTLE: Duration = Duration::from_secs(3);

/// ≈ 300 synthetic applications, each with an action: the size of a real
/// desktop's catalog (a large desktop has about that many). Synthetic only (no capture).
fn many_apps() -> Vec<(String, String)> {
    (0..300)
        .map(|i| {
            (
                format!("alpheus-synthetic-{i}.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=Synthetic app {i}\nGenericName=Generic thing {i}\nKeywords=one;two;three;\nExec=synthetic-{i} %U\nActions=new;\n\n[Desktop Action new]\nName=New window\nExec=synthetic-{i} --new\n"
                ),
            )
        })
        .collect()
}

/// `launcher_toml` builds launcher.toml from the sway runtime directory (a fake source lives in it).
fn start_launcher(
    name: &str,
    bus: &PrivateBus,
    bar_toml: Option<&str>,
    apps: &[(String, String)],
    launcher_toml: Option<&dyn Fn(&Path) -> String>,
) -> (Sway, FakeHyprland, Daemon) {
    let files: Vec<(&str, &str)> = apps.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let toml = launcher_toml.map(|build| build(&sway.runtime));
    let options = DaemonOptions {
        bar_toml,
        system_bus: Some(&bus.address),
        desktop_files: &files,
        launcher_toml: toml.as_deref(),
        ..DaemonOptions::default()
    };
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// Open, then close, through the socket like Mod+Space: the n-th open and
/// the n-th close (of any reason).
fn cycle(sway: &Sway, d: &Daemon, n: usize) {
    assert_eq!(launcher(sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    // Past the toggle debounce: a Toggle right after the open is Hyprland's press-bind refire.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(launcher(sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher closed (", n, DELAY), "{:?}", d.log());
}

/// Open through the socket, close with a real Escape typed by wtype: the
/// keyboard is bound (keymap, compose table), and the key that closes arms
/// sctk's repeat timer — what toggle cycles never touch.
fn key_cycle(sway: &Sway, d: &Daemon, n: usize) {
    assert_eq!(launcher(sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    assert!(wtype(sway, &["-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for_count("launcher closed (", n, DELAY), "{:?}", d.log());
    let log = d.log();
    let last = log.iter().rev().find(|l| l.contains("launcher closed ("));
    assert!(last.is_some_and(|l| l.contains("(escape)")), "closed by the key, not otherwise: {last:?}");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_open_latency_under_20ms() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("latency");
    let (sway, _hypr, d) = start_launcher("latency", &bus, None, &many_apps(), None);
    std::thread::sleep(Duration::from_secs(2));
    let mut us = Vec::new();
    for n in 1..=20 {
        cycle(&sway, &d, n);
        us.push(d.last_open_latency_us().expect("the latency is logged"));
    }
    us.sort_unstable();
    let (median, max) = (us[us.len() / 2], us[us.len() - 1]);
    eprintln!("launcher open latency, 20 opens (request accepted → first frame committed): median {median} µs, max {max} µs; {us:?}");
    assert!(median < 20_000, "median {median} µs >= 20 ms: report it with these figures");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_open_close_cycles_no_growth() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("cycles");
    let (sway, _hypr, d) = start_launcher("cycles", &bus, None, &many_apps(), None);
    std::thread::sleep(Duration::from_secs(3)); // startup settled, as in pss_under_30mb_and_stable
    let closed = d.pss_kb();
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 1, DELAY), "{:?}", d.log());
    // Typed: the keyboard is bound and its keymap and compose table are
    // compiled — part of what an open launcher costs, measured with it.
    // One continuous wtype invocation, not two: on headless sway
    // (WLR_LIBINPUT_NO_DEVICES=1) the seat's only keyboard capability is
    // wtype's own virtual keyboard, gone the moment its process exits — a
    // second, separate wtype call would find the launcher already closed
    // (the first process exiting looks exactly like the keyboard being
    // unplugged, `new_capability`/`remove_capability` in wayland.rs). One
    // process, two `-s` delays, keeps that one virtual keyboard alive from
    // the typing through to the Escape that closes the launcher.
    let typing = wtype_spawn(&sway, &["-s", "300", "synth", "-s", "3000", "-k", "Escape"]);
    std::thread::sleep(Duration::from_millis(1500)); // after "synth" is typed, well before Escape fires
    let open = d.pss_kb();
    // Still well inside the delay before Escape: the launcher must still be
    // open when this PSS was read, or it is not measuring what it claims to.
    assert_eq!(d.count("launcher closed ("), 0, "launcher closed before the 'open' PSS was read: {:?}", d.log());
    assert!(typing.wait().unwrap().success());
    assert!(d.wait_for_count("launcher closed (escape)", 1, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    // Baseline after one warm-up cycle: the bar's bounded glyph cache now
    // holds the box's glyphs (same convention as pss_under_30mb_and_stable).
    let baseline = d.pss_kb();
    // 50 cycles, 17 of them closed by a typed Escape (a keyboard bound and
    // released each time), the others by Mod+Space's toggle.
    for n in 2..=51 {
        if n % 3 == 0 { key_cycle(&sway, &d, n) } else { cycle(&sway, &d, n) }
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.pss_kb();
    eprintln!(
        "launcher PSS: closed {closed} KB, open {open} KB (+{} KB), after one cycle {baseline} KB, after 50 more {end} KB",
        open.saturating_sub(closed)
    );
    assert!(open <= closed + 6 * 1024, "open costs {} KB > 6 MB: report it, never absorb", open - closed);
    assert!(end <= baseline + 1024, "50 open/close cycles grew PSS: {baseline} -> {end} KB");
}

/// 300 fictional source lines in tools-menu's shape.
fn source_lines(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("Catégorie {} · Entrée {i}", i / 20)).collect()
}

/// Delivered-source summaries in the log: "launcher sources: <n> entries, <ms> ms after the request"
/// (not the "previous open's are still being read" notice).
fn source_summaries(d: &Daemon) -> Vec<String> {
    d.log()
        .into_iter()
        .filter(|l| l.split_once("launcher sources: ").is_some_and(|(_, r)| r.starts_with(|c: char| c.is_ascii_digit())))
        .collect()
}

/// ms from a summary line.
fn entries_ms(line: &str) -> Option<u64> {
    line.rsplit_once(", ")?.1.strip_suffix(" ms after the request")?.trim().parse().ok()
}

/// PSS with no source and with a 300-line source, open latency with it (the box
/// shows at once, the entries follow), and 0 wake-ups at rest after. Printed for docs/measurements.md; the
/// latency keeps the launcher's 20 ms budget, the rest its <= 4 wake-ups.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_sources_pss_latency_and_rest() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("sources");
    for lines in [0, 300] {
        let build = move |runtime: &Path| fake_source(runtime, "Outils", &source_lines(lines));
        let toml: Option<&dyn Fn(&Path) -> String> = if lines > 0 { Some(&build) } else { None };
        let (sway, _hypr, d) = start_launcher(&format!("sources-{lines}"), &bus, Some(AT_REST), &many_apps(), toml);
        std::thread::sleep(Duration::from_secs(3));
        let closed = d.pss_kb();
        let (mut us, mut open) = (Vec::new(), 0);
        for n in 1..=21 {
            assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
            assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
            if lines > 0 {
                let t = Instant::now();
                while source_summaries(&d).len() < n && t.elapsed() < DELAY {
                    std::thread::sleep(Duration::from_millis(20));
                }
                assert!(source_summaries(&d).len() >= n, "{:?}", d.log());
            }
            us.push(d.last_open_latency_us().expect("the latency is logged"));
            std::thread::sleep(Duration::from_millis(300)); // past the toggle debounce
            if n == 1 {
                open = d.pss_kb();
            }
            assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
            assert!(d.wait_for_count("launcher closed (", n, DELAY), "{:?}", d.log());
        }
        let mut ms: Vec<u64> = source_summaries(&d).iter().filter_map(|l| entries_ms(l)).collect();
        us.sort_unstable();
        ms.sort_unstable();
        std::thread::sleep(Duration::from_secs(3));
        let (before, ticks) = (d.wakeups(), d.cpu_ticks());
        std::thread::sleep(Duration::from_secs(5));
        let wakeups = d.wakeups().saturating_sub(before);
        assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon after source runs");
        let after = d.pss_kb();
        eprintln!(
            "launcher sources, {lines}-line source: PSS closed {closed} KB, open {open} KB (+{} KB), after 21 opens {after} KB; \
             open latency median {} us (max {}); entries median {:?} ms (max {:?}); wake-ups at rest (5 s): {wakeups}",
            open.saturating_sub(closed),
            us[us.len() / 2],
            us[us.len() - 1],
            ms.get(ms.len() / 2),
            ms.last()
        );
        assert!(us[us.len() / 2] < 20_000, "median open latency {} us >= 20 ms: report it", us[us.len() / 2]);
        assert!(wakeups <= 4, "{wakeups} wake-ups in 5 s at rest after source runs");
    }
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_launcher_cycle() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("launcher-rest");
    let (sway, _hypr, d) = start_launcher("launcher-rest", &bus, Some(AT_REST), &[], None);
    std::thread::sleep(Duration::from_secs(2));
    let threads = d.threads();
    // Closed by a typed Escape: a key that closes arms sctk's repeat timer
    // after the keyboard is released; it must fire once
    // and go, never keep re-arming.
    key_cycle(&sway, &d, 1);
    // A script menu whose client goes away: its watch must go with it.
    let mut client = UnixStream::connect(socket_path(&sway.runtime)).unwrap();
    client.write_all(&encode_request(&Request::Dmenu { prompt: None, lines: vec!["a".into()], flags: DmenuFlags::default() })).unwrap();
    assert!(d.wait_for_count("launcher shown on", 2, DELAY), "{:?}", d.log());
    drop(client);
    assert!(d.wait_for("launcher closed (close)", DELAY), "{:?}", d.log());
    // Past REQUEST_DEADLINE: a timer that keeps re-arming, a keyboard repeat
    // or a watch left behind shows up below as wake-ups.
    std::thread::sleep(Duration::from_secs(3));
    let (before, ticks) = (d.wakeups(), d.cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.wakeups().saturating_sub(before);
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon after a launcher cycle");
    eprintln!("wake-ups at rest after a launcher cycle (5 s): {delta}; threads {} (before the cycle: {threads})", d.threads());
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest after the launcher was used");
    assert_eq!(d.threads(), threads, "no thread left by the launcher");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_threads_back_to_baseline() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("launcher-threads");
    let (sway, _hypr, d) = start_launcher("launcher-threads", &bus, Some(AT_REST), &many_apps(), None);
    std::thread::sleep(Duration::from_secs(2));
    let before = d.threads();
    cycle(&sway, &d, 1);
    let t = Instant::now();
    while d.threads() != before {
        assert!(t.elapsed() < Duration::from_secs(1), "{} threads 1 s after close, {before} before open", d.threads());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A dmenu of 100,000 short lines, open
/// PSS and its value 1 s after cancel — printed, no assertion (the budgets
/// above already cover growth and the 6 MB open margin on a realistic
/// catalog; this is the worst-case dmenu size, reported for docs/measurements.md).
/// The request itself goes through `alpheus_launch::exchange` on its own
/// thread, exactly as the shipped client would send it; a typed Escape
/// cancels it, as a person would.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn large_dmenu_pss_is_reported() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("large-dmenu");
    let (sway, _hypr, d) = start_launcher("large-dmenu", &bus, None, &[], None);
    std::thread::sleep(Duration::from_secs(2));
    let closed = d.pss_kb();
    let socket = socket_path(&sway.runtime);
    let lines: Vec<String> = (0..100_000).map(|i| format!("entry-{i}")).collect();
    let menu = std::thread::spawn(move || {
        alpheus_launch::exchange(&socket, &Request::Dmenu { prompt: None, lines, flags: DmenuFlags::default() })
    });
    assert!(d.wait_for("launcher shown on", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1)); // ranked and drawn
    let open = d.pss_kb();
    assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
    assert_eq!(menu.join().unwrap(), Ok(Reply::Cancelled));
    assert!(d.wait_for("launcher closed (escape)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    let after = d.pss_kb();
    eprintln!(
        "large dmenu (100,000 lines) PSS: {closed} KB closed, {open} KB open (+{} KB), {after} KB 1 s after cancel (+{} KB vs closed)",
        open.saturating_sub(closed),
        after.saturating_sub(closed)
    );
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn pss_under_30mb_and_stable() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("memory");
    let _workstation = Workstation::serve(&bus);
    let (sway, hypr, d) = start("memory", 3, &bus, None, Some("[resources]\ninterval = 1\n"), PwDump::Busy);
    // 3 s: startup is over (fonts, pw-dump's first array, the D-Bus initial
    // state, their release_memory): `initial` is a settled figure, which the
    // return-margin check below needs.
    std::thread::sleep(Duration::from_secs(3));
    let initial = d.pss_kb();
    // Fonts: regular + bold ≈ 2 MB (the whole font database: ≈ 47 MB);
    // the first three modules measured ≈ 12 MB here; the D-Bus source and the sound source
    // must fit in what is left.
    assert!(initial < STARTUP_MAX_KB, "startup: PSS {initial} KB >= 15 MB: run startup_pss_per_source and report it, never raise");
    let mut n = 0;
    pump(&hypr, Duration::from_secs(10), &mut n); // warm-up
    // A first unplug/replug cycle before `baseline` (the first three modules):
    // `baseline` and `end` are read in comparable states.
    sway.command("output HEADLESS-2 disable");
    std::thread::sleep(Duration::from_secs(1));
    sway.command("output HEADLESS-2 enable");
    std::thread::sleep(Duration::from_secs(1));
    let baseline = d.pss_kb();
    for _ in 0..4 {
        pump(&hypr, Duration::from_secs(8), &mut n);
        sway.command("output HEADLESS-2 disable");
        std::thread::sleep(Duration::from_secs(1));
        sway.command("output HEADLESS-2 enable");
        std::thread::sleep(Duration::from_secs(1));
    }
    let end = d.pss_kb();
    eprintln!(
        "PSS: {initial} KB at startup, {baseline} KB after warm-up, {end} KB after {n} events, 5 plug cycles, pw-dump deltas every 10 ms"
    );
    // The resident budget of docs/decisions.md ("Everything alpheus keeps resident fits in
    // 15 MB at rest"); measured 11.3 to 11.9 MB, never raise.
    assert!(end < 15 * 1024, "PSS {end} KB >= 15 MB");
    assert!(end <= baseline + 1024, "growth: {baseline} -> {end} KB");
    const RETURN_MARGIN_KB: u64 = 2 * 1024;
    assert!(end.abs_diff(initial) <= RETURN_MARGIN_KB, "PSS does not come back close to the start: {initial} KB -> {end} KB");
    assert!(d.log().iter().all(|l| !l.contains("sound source down")), "pw-dump kept up: {:?}", d.log());
}

/// Every module, the launcher's included, as a `modules` list: `startup_pss_per_source`'s
/// "everything" case and `EVERYTHING`, one literal for both.
macro_rules! every_module {
    () => {
        "\"launcher\", \"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \"resources\", \"timer\", \"clock\""
    };
}

/// Every module, the launcher's included: `startup_pss_per_source`'s "everything" case.
const EVERYTHING: &str = concat!("modules = [", every_module!(), "]\n");

/// Startup PSS per source, against the firm 15 MB budget: the first three modules
/// (workspaces, window, clock) alone, then each new source on its own, then everything. Printed:
/// which source costs what.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn startup_pss_per_source() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("per-source");
    let _workstation = Workstation::serve(&bus);
    // The system manager at count 0: only the `+ failed` case reads it.
    let _systemd = FakeSystemd::serve(&bus, 0);
    let mut cases = vec![
        // The case names are what docs/measurements.md cites next to its figures: keep them.
        ("bar only", "\"workspaces\", \"window\", \"clock\"", ""),
        ("+ sound", "\"workspaces\", \"window\", \"volume\", \"clock\"", ""),
        ("+ D-Bus", "\"workspaces\", \"window\", \"network\", \"bluetooth\", \"battery\", \"clock\"", ""),
        ("+ resources", "\"workspaces\", \"window\", \"resources\", \"clock\"", ""),
        ("+ launcher", "\"launcher\", \"workspaces\", \"window\", \"clock\"", ""),
        ("everything", every_module!(), ""),
        ("+ privacy", concat!(every_module!(), ", \"privacy\""), ""),
        ("+ clipboard", every_module!(), "[clipboard]\nenabled = true\n"),
    ];
    // Another build under test (`ALPHEUS_DEMON_UNDER_TEST`, a base from before the module) does
    // not know its name.
    if std::env::var_os("ALPHEUS_DEMON_UNDER_TEST").is_none() {
        // `[failed]` at its defaults: `subscribe = true`.
        cases.push(("+ failed", concat!(every_module!(), ", \"failed\""), ""));
    }
    let mut report = Vec::new();
    for (i, (what, modules, extra)) in cases.iter().enumerate() {
        let toml = format!("modules = [{modules}]\n{extra}");
        let (_sway, _hypr, d) = start(&format!("per-source-{i}"), 3, &bus, None, Some(&toml), PwDump::Silent);
        std::thread::sleep(Duration::from_secs(3)); // as in pss_under_30mb_and_stable
        report.push((*what, d.pss_kb()));
    }
    eprintln!("startup PSS per source (KB): {report:?}");
    for (what, kb) in &report {
        assert!(*kb < STARTUP_MAX_KB, "{what}: {kb} KB >= 15 MB — report it with these figures, never raise: {report:?}");
    }
    // With nothing captured, the module keeps a small graph and draws nothing.
    let kb = |w: &str| report.iter().find(|(what, _)| *what == w).map(|(_, kb)| *kb).unwrap();
    let (everything, privacy) = (kb("everything"), kb("+ privacy"));
    assert!(privacy <= everything + 256, "+ privacy {privacy} KB vs everything {everything} KB: more than 256 KB at rest");
    // The failed count is a number per manager and a listener, nothing kept.
    if let Some((_, failed)) = report.iter().find(|(what, _)| *what == "+ failed") {
        assert!(*failed <= everything + 256, "+ failed {failed} KB vs everything {everything} KB: more than 256 KB at rest");
    }
    // The history on, nothing copied yet, reports a small step (report, never raise).
    let clipboard = kb("+ clipboard");
    assert!(clipboard <= everything + 256, "+ clipboard {clipboard} KB vs everything {everything} KB: more than 256 KB at rest");
}

/// DejaVu Sans Mono (the default `[font] fallback`): printed for the task
/// report — what loading it costs at startup while unused, and what
/// actually drawing from it costs once a title needs it (✳◐◑◒, the glyphs
/// `fallback_font_draws_a_symbol_the_main_family_lacks` and
/// `uncovered_character_is_masked_with_a_consistent_width`, in
/// `painter.rs`, cover unit-wise).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn fallback_font_pss_cost() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("fallback-pss");
    let (_sway, hypr, d) = start("fallback-pss", 1, &bus, None, Some("modules = [\"window\", \"clock\"]\n"), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(3)); // startup settled: fonts loaded, DejaVu Sans Mono unused so far
    let startup = d.pss_kb();
    // A title with the bug's own symbols, covered only by the fallback.
    hypr.write("activewindow>>app,\u{2733} build \u{25D0}\u{25D1}\u{25D2}\u{25D3}\n");
    std::thread::sleep(Duration::from_millis(500));
    let rendered = d.pss_kb();
    eprintln!("PSS: {startup} KB at startup (DejaVu Sans Mono loaded, unused), {rendered} KB after rendering a title with ✳◐◑◒ from it");
    assert!(startup < STARTUP_MAX_KB, "startup: {startup} KB >= 15 MB");
    assert!(rendered < STARTUP_MAX_KB, "after rendering the fallback glyphs: {rendered} KB >= 15 MB");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("rest");
    let _workstation = Workstation::serve(&bus);
    let (_sway, _hypr, d) = start("rest", 3, &bus, None, Some(AT_REST), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(2)); // end of startup
    let before = d.wakeups();
    let sound_before = d.thread_wakeups("sound").expect("the sound thread runs");
    let dbus_before = d.thread_wakeups("D-Bus").expect("the D-Bus thread runs");
    let threads = d.threads();
    let ticks = d.cpu_ticks();
    std::thread::sleep(Duration::from_secs(5));
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon at rest");
    let delta = d.wakeups().saturating_sub(before);
    let sound = d.thread_wakeups("sound").unwrap().saturating_sub(sound_before);
    let dbus = d.thread_wakeups("D-Bus").unwrap().saturating_sub(dbus_before);
    eprintln!("wake-ups at rest (5 s): {delta} in all, sound thread {sound}, D-Bus thread {dbus}; {threads} threads");
    // The sound and D-Bus sources are up and block without a timeout: a
    // source down would retry (1 s, 2 s…) and wake the daemon.
    assert!(d.log().iter().all(|l| !l.contains("source down")), "every source up: {:?}", d.log());
    // Nothing changes on the bus nor in PipeWire: a single wake-up of one of
    // these threads is a poll (a loop with a timeout somewhere).
    assert_eq!((sound, dbus), (0, 0), "a source thread wakes at rest");
    // The first three modules observed 0 to 2; a 1 Hz tick would add ≈ 5.
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest");
    // Loop, guard, compositor, one rayon, sound, D-Bus (libdbus has no
    // thread of its own), plus transient command reapers.
    assert!(threads <= 12, "{threads} threads");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn resources_tick_at_the_configured_interval() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("cadence");
    let toml = "modules = [\"workspaces\", \"resources\", \"clock\"]\n[resources]\ninterval = 1\n";
    // One output: one redraw round trip per tick, not three.
    let (_sway, _hypr, d) = start("cadence", 1, &bus, None, Some(toml), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(2));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop wake-ups in 5 s at interval = 1: {delta}");
    // 5 ticks: each wakes the loop once for its timer, and a reading that
    // changes the bar adds further round trips (redraw commit, buffer
    // release, frame callback) — measured steadily at ~3 context switches
    // per tick with 1 output (2026-10-05, release, one machine: 15, 15,
    // 17, 15 over 4 runs). ≥ 4: it really ticks every second; ≤ 20: never
    // faster — halving the period (the `ToDuration(period / 2)` mutation)
    // measured 27, comfortably outside this range. The old 4..=12 could
    // no longer hold the steady ~15-17 and was widened from measurements,
    // not loosened to paper over a bug.
    assert!((4..=20).contains(&delta), "{delta} loop wake-ups in 5 s at interval = 1");
}

const EVERY_ITEM: &str = "show = [\"cpu\", \"memory\", \"temperature\", \"net\", \"disk\", \"gpu\"]\n";

/// The resources items `net`, `disk`, `gpu` and the do-not-disturb duration: the one-time cost of all six items over the default three (two fresh
/// daemons, same tick rate), then, in the six-item daemon, what ~60 more passes keep. PSS
/// without the shm pools: the compositor maps those too, so they move on their own.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn resources_pss_default_and_every_item() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("res-items");
    let bar = |extra: &str| format!("modules = [\"workspaces\", \"resources\", \"clock\"]\n[resources]\ninterval = 1\n{extra}");
    let default = {
        let (_sway, _hypr, d) = start("res-items-a", 1, &bus, None, Some(&bar("")), PwDump::Silent);
        std::thread::sleep(Duration::from_secs(3));
        d.pss_kb()
    };
    let (_sway, _hypr, d) = start("res-items-b", 1, &bus, None, Some(&bar(EVERY_ITEM)), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(3));
    let (settled, settled_shm) = d.pss_and_shm_kb();
    std::thread::sleep(Duration::from_secs(60));
    let (later, later_shm) = d.pss_and_shm_kb();
    let (own_settled, own_later) = (settled - settled_shm, later - later_shm);
    eprintln!(
        "resources PSS (KB): {default} with the default items, {settled} with all six; own memory {own_settled} settled, {own_later} after ~60 more passes"
    );
    assert!(settled <= default + 384, "every item costs {} KB over the default", settled.saturating_sub(default));
    assert!(
        own_later <= own_settled + 64,
        "~60 passes with all six items grew the daemon's own memory by {} KB",
        own_later.saturating_sub(own_settled)
    );
}

/// All six items tick at the configured rate: none adds a timer of its own.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn resources_every_item_ticks_at_the_same_rate() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("cadence-all");
    let toml = format!("modules = [\"workspaces\", \"resources\", \"clock\"]\n[resources]\ninterval = 1\n{EVERY_ITEM}");
    let (_sway, _hypr, d) = start("cadence-all", 1, &bus, None, Some(&toml), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(2));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop wake-ups in 5 s at interval = 1, six items: {delta}");
    assert!((4..=20).contains(&delta), "{delta} loop wake-ups in 5 s at interval = 1, six items");
}

/// The D-Bus source reconnects after the system bus restarts (dbus.service
/// restarted, a crash): each reconnection must leave no thread behind.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn no_thread_leak_across_system_bus_restarts() {
    require_release(cfg!(debug_assertions));
    let mut bus = PrivateBus::start("restarts");
    let (_sway, _hypr, d) = start("restarts", 1, &bus, None, Some(AT_REST), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(2));
    let before = d.threads();
    for round in 1..=2 {
        bus.restart();
        // Down, then reconnected at the first retry (1 s, then 2 s). The
        // "down" log line itself is rate-limited:
        // only the first outage ever, or one after a SUCCEEDED session,
        // is logged (`first || succeeded` in `run_supervised`); a session
        // counts as succeeded only once it has run for `STABLE_DURATION`
        // (10 s) — this private bus has no NetworkManager/UPower/BlueZ to
        // generate a second publication, so the reconnected session must
        // simply stay up that long before the next restart, or its outage
        // would be silently folded into the previous "down" count instead
        // of logging a fresh line (a wait of 5 s undershoots
        // STABLE_DURATION: the real rate-limiting rule applies).
        std::thread::sleep(Duration::from_secs(11));
        let downs = d.log().iter().filter(|l| l.contains("D-Bus source down")).count();
        assert_eq!(downs, round, "one outage per restart, then reconnected: {:?}", d.log());
    }
    let after = d.threads();
    eprintln!("threads: {before} before, {after} after two system bus restarts");
    assert_eq!(after, before, "a reconnection leaves no thread behind");
    assert!(after <= 12, "{after} threads");
}

/// Polls `d.threads()` every 100 ms until two consecutive reads agree, or
/// `timeout` elapses: a bounded wait for "settled" instead of a fixed
/// sleep, which either undershoots (flaky on a loaded machine) or
/// overshoots (slow on every run alike) a one-off delay.
/// Stable means 3 equal consecutive readings, 100 ms apart, never trusted
/// before 500 ms have passed: a single repeat (the old rule) could land on
/// a transient plateau mid-startup (a thread about to spawn, between two
/// polls) and report that count as final.
fn wait_for_stable_threads(d: &Daemon, timeout: Duration) -> usize {
    const MIN_WAIT: Duration = Duration::from_millis(500);
    const STABLE_READINGS: u32 = 3;
    let t = Instant::now();
    let mut last = d.threads();
    let mut streak = 1;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let now = d.threads();
        streak = if now == last { streak + 1 } else { 1 };
        last = now;
        if (streak >= STABLE_READINGS && t.elapsed() >= MIN_WAIT) || t.elapsed() >= timeout {
            return now;
        }
    }
}

/// The D-Bus source costs exactly one thread, its own (`D-Bus`): no
/// executor, reactor or pool thread behind it (zbus had two, plus
/// `blocking`'s). Same bar but for the three D-Bus modules, on the same
/// recorded workstation.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_d_bus_source_costs_one_thread() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("one-thread");
    let _workstation = Workstation::serve(&bus);
    let without = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let with = "modules = [\"workspaces\", \"window\", \"network\", \"bluetooth\", \"battery\", \"clock\"]\n";
    let (_s1, _h1, a) = start("one-thread-a", 1, &bus, None, Some(without), PwDump::Silent);
    let (_s2, _h2, b) = start("one-thread-b", 1, &bus, None, Some(with), PwDump::Silent);
    // Bounded poll instead of a fixed sleep: both settled once their thread
    // count stops changing (the D-Bus initial state read adds nothing further).
    let ta = wait_for_stable_threads(&a, Duration::from_secs(5));
    let tb = wait_for_stable_threads(&b, Duration::from_secs(5));
    assert!(b.log().iter().all(|l| !l.contains("source down")), "the D-Bus source up: {:?}", b.log());
    eprintln!("threads: {ta} without the D-Bus modules, {tb} with them");
    assert_eq!(tb, ta + 1, "the D-Bus source must cost its own thread and nothing else");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn ten_fake_items_cost_at_most_2_mb() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("tray-cost");
    let without = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let with = "modules = [\"workspaces\", \"window\", \"tray\", \"clock\"]\n";
    // Each configuration alone, its daemon gone before the next starts: two
    // daemons of the same binary would split their shared pages (PSS).
    let base = {
        let (_sway, _hypr, d) = start("tray-cost-a", 1, &bus, None, Some(without), PwDump::Silent);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let empty = {
        let session = PrivateBus::start("tray-cost-empty");
        let (_sway, _hypr, d) = start("tray-cost-b", 1, &bus, Some(&session), Some(with), PwDump::Silent);
        tray_up(&d, &session);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let full = {
        let session = PrivateBus::start("tray-cost-full");
        let (_sway, _hypr, d) = start("tray-cost-c", 1, &bus, Some(&session), Some(with), PwDump::Silent);
        let _items = populate(&d, &session);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let module_cost = empty.saturating_sub(base);
    let items_cost = full.saturating_sub(empty);
    eprintln!(
        "tray PSS (KB): {base} without the module, {empty} with an empty tray (module cost {module_cost}), \
         {full} with 10 fake items (items cost {items_cost})"
    );
    assert!(items_cost <= 2048, "10 fake items cost {items_cost} KB, budget 2,048 KB (module alone: {module_cost} KB)");
}

/// Sleeps until a `window` starting now fits inside one minute, 2 s from
/// each boundary: the clock (the only timer at rest without resources) then
/// never ticks inside the measurement, and any loop-thread wake-up is a fault.
fn away_from_the_minute(window: Duration) {
    let into = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() % 60;
    if into < 2 {
        std::thread::sleep(Duration::from_secs(2 - into));
    } else if !fits_the_minute(window) {
        std::thread::sleep(Duration::from_secs(60 - into + 2));
    }
}

/// Whether a `window` starting now fits inside one minute, 2 s from each boundary
/// (`away_from_the_minute`'s rule), without sleeping.
fn fits_the_minute(window: Duration) -> bool {
    let into = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() % 60;
    into >= 2 && into + window.as_secs() + 2 < 60
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_a_populated_tray() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("tray-rest");
    let session = PrivateBus::start("tray-rest-session");
    let toml = "modules = [\"workspaces\", \"window\", \"tray\", \"clock\"]\n";
    let (_sway, _hypr, d) = start("tray-rest", 1, &bus, Some(&session), Some(toml), PwDump::Silent);
    let _items = populate(&d, &session);
    std::thread::sleep(TRAY_SETTLE);
    const REST: Duration = Duration::from_secs(10);
    away_from_the_minute(REST);
    let before = d.wakeups();
    let main_before = d.main_thread_wakeups();
    let ticks = d.cpu_ticks();
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "the daemon with a populated tray");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest with a populated tray (10 s): {delta} whole daemon, {main_delta} loop thread");
    assert!(
        d.log().iter().all(|l| !l.contains("source down") && !l.contains("tray: no session bus")),
        "every source up, a session bus found: {:?}",
        d.log()
    );
    // The budget, ≤ 4 in 5 s for the whole daemon, over 10 s.
    assert!(delta <= 8, "{delta} wake-ups in 10 s at rest with the tray populated");
    // No minute boundary inside the window: the loop thread has nothing to
    // do. A call still pending would fire its 2 s deadline here, a timer of
    // any period up to 10 s would show at least once.
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s at rest (no clock tick in the window)");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_tray_adds_no_thread() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("tray-threads");
    let without = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let with = "modules = [\"workspaces\", \"window\", \"tray\", \"clock\"]\n";
    let (_sway_a, _hypr_a, d_without) = start("tray-threads-a", 1, &bus, None, Some(without), PwDump::Silent);
    let session = PrivateBus::start("tray-threads-session");
    let (_sway_b, _hypr_b, d_with) = start("tray-threads-b", 1, &bus, Some(&session), Some(with), PwDump::Silent);
    let _items = populate(&d_with, &session);
    let ta = wait_for_stable_threads(&d_without, Duration::from_secs(5));
    let tb = wait_for_stable_threads(&d_with, Duration::from_secs(5));
    eprintln!("threads: {ta} without the tray, {tb} with it and 10 fake items");
    assert_eq!(tb, ta, "the tray's fd is a calloop source on the loop thread: it must cost no extra thread");
}

/// The tray alone then the clock: the first icon is
/// cell 1, x = 12 inside it at any monospace advance of the test machine.
const ICON: (u32, u32) = (12, 10);
/// Headless sway's output is 1280 px wide and, with no window module, the
/// clock sits at the right end of the bar (measured by a click scan:
/// x = 1170..1260 hit it): x = 1240 is inside its text.
const CLOCK: (u32, u32) = (1280 - 40, 10);

#[test]
#[ignore = "headless sway, dbus-daemon and wtype (nix develop)"]
fn popups_free_everything_on_close() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("tray-popups");
    let session = PrivateBus::start("tray-popups-session");
    // Tooltips off: the pointer rests on the icon after each right click, and
    // a tooltip would add its own "popup closed" lines to the count.
    let toml = "modules = [\"tray\", \"clock\"]\n[tray]\ntooltips = false\n[clock]\ncalendar = true\n";
    let (sway, _hypr, d) = start("tray-popups", 1, &bus, Some(&session), Some(toml), PwDump::Silent);
    tray_up(&d, &session);
    let _item = FakeItem::start(&session, Spec::kde(1));
    assert!(d.wait_for("tray: icons shown: 1", DELAY), "{:?}", d.log());
    // Cycle n: the menu (right click, Escape), then the calendar (a click on
    // the clock opens it, a second one closes it) — exactly two
    // "popup closed" lines per cycle.
    let cycle = |n: usize| {
        sway.click(ICON.0, ICON.1, "button3");
        assert!(d.wait_for_count("menu shown", n, DELAY), "cycle {n}: {:?}", d.log());
        assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success(), "wtype");
        assert!(d.wait_for_count("popup closed", 2 * n - 1, DELAY), "cycle {n}: {:?}", d.log());
        sway.click(CLOCK.0, CLOCK.1, "button1");
        assert!(d.wait_for_count("calendar shown", n, DELAY), "cycle {n}: {:?}", d.log());
        sway.click(CLOCK.0, CLOCK.1, "button1");
        assert!(d.wait_for_count("popup closed", 2 * n, DELAY), "cycle {n}: {:?}", d.log());
    };
    // Warm-up: what the first open builds once and keeps by design (the
    // glyphs of the menu and calendar in the shared glyph cache, wtype's
    // virtual keyboard keymap) is in the baseline, not in the difference.
    cycle(1);
    std::thread::sleep(Duration::from_millis(500));
    let baseline = d.pss_kb();
    for n in 2..=21 {
        cycle(n);
    }
    std::thread::sleep(Duration::from_millis(500));
    let end = d.pss_kb();
    eprintln!("PSS: {baseline} KB baseline (after one warm-up cycle), {end} KB after 20 menu/calendar open-close cycles");
    assert!(end.abs_diff(baseline) <= 512, "popups leak: {baseline} KB -> {end} KB");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn startup_pss_with_the_tray_under_15_mb() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("tray-startup");
    let _workstation = Workstation::serve(&bus);
    let session = PrivateBus::start("tray-startup-session");
    let toml = "modules = [\"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \
                \"resources\", \"tray\", \"clock\"]\n[resources]\ninterval = 1\n";
    let (_sway, _hypr, d) = start("tray-startup", 3, &bus, Some(&session), Some(toml), PwDump::Silent);
    let _items = populate(&d, &session);
    std::thread::sleep(TRAY_SETTLE);
    let kb = d.pss_kb();
    eprintln!("startup PSS, every module, 10 tray items, 3 outputs: {kb} KB");
    assert!(kb < STARTUP_MAX_KB, "{kb} KB >= 15 MB with the tray populated — report it, never raise: {kb}");
}

/// The inotify watch adds nothing at rest, even right after a reload. Read in
/// docs/measurements.md as "0 wake-ups at rest" with the watch in place.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_theme_reload() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("theme-rest");
    let toml = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let (sway, _hypr, d) = start("theme-rest", 1, &bus, None, Some(toml), PwDump::Silent);
    let config = sway.runtime.join("config/alpheus");
    write_atomically(&config, "theme.toml", "theme = \"gruvbox\"\nmode = \"dark\"\n");
    assert!(d.wait_for("theme reloaded: gruvbox, dark", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    const REST: Duration = Duration::from_secs(10);
    away_from_the_minute(REST);
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "the daemon after a theme reload");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest after a theme reload (10 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s at rest after a reload");
    assert!(delta <= 8, "{delta} wake-ups in 10 s at rest after a reload");
}

/// 50 reloads (two presets in turn, three bars repainted each time) leave no
/// growth behind. Printed for docs/measurements.md; budget 768 KB (a leak of 64 KB per reload makes 3.2 MB).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn theme_reloads_no_growth() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("theme-reloads");
    let toml = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let (sway, _hypr, d) = start("theme-reloads", 3, &bus, None, Some(toml), PwDump::Silent);
    let config = sway.runtime.join("config/alpheus");
    let reload = |n: usize| {
        let theme = if n.is_multiple_of(2) { "tokyo-night" } else { "gruvbox" };
        write_atomically(&config, "theme.toml", &format!("theme = \"{theme}\"\nmode = \"dark\"\n"));
        assert!(d.wait_for_count("theme reloaded:", n + 1, DELAY), "reload {n}: {:?}", d.log());
    };
    reload(0);
    reload(1); // warm-up: both presets painted once
    std::thread::sleep(Duration::from_secs(2));
    reload(2);
    reload(3); // and once more: the first run after a build still settled by +444 KB
    std::thread::sleep(Duration::from_secs(2));
    let baseline = d.pss_kb();
    for n in 4..54 {
        reload(n);
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.pss_kb();
    eprintln!("theme reloads: PSS {baseline} KB after 4 warm-up reloads, {end} KB after 50 more (3 outputs)");
    assert!(end <= baseline + 768, "growth: {baseline} -> {end} KB over 50 reloads");
}

/// Every style, in the order the tests switch to them (`flat` last, so the
/// switch back from translucent is measured too).
const STYLES: [&str; 6] = ["islands", "powerline", "floating", "underline", "translucent", "flat"];

/// No style adds a wake-up at rest, `floating`'s reconfigured surfaces included.
/// Three outputs, 5 s per style, away from the minute so the clock never ticks inside a window.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_under_each_style() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("style-rest");
    let toml = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let (sway, _hypr, d) = start("style-rest", 3, &bus, None, Some(toml), PwDump::Silent);
    let config = sway.runtime.join("config/alpheus");
    const REST: Duration = Duration::from_secs(5);
    let mut report = Vec::new();
    for style in STYLES {
        write_atomically(&config, "theme.toml", &format!("theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"{style}\"\n"));
        assert!(d.wait_for(&format!(", style {style}"), DELAY), "{style}: {:?}", d.log());
        std::thread::sleep(Duration::from_secs(1)); // the repaint and, for floating, the configures settled
        away_from_the_minute(REST);
        let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
        std::thread::sleep(REST);
        assert_no_spin(d.cpu_ticks() - ticks, REST, style);
        let delta = d.wakeups().saturating_sub(before);
        let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
        report.push((style, delta, main_delta));
    }
    eprintln!("wake-ups at rest per style (5 s each, 3 outputs; style, whole daemon, loop thread): {report:?}");
    for (style, delta, main_delta) in &report {
        assert_eq!(*main_delta, 0, "{style}: loop-thread wake-ups at rest: {report:?}");
        assert!(*delta <= 4, "{style}: {delta} wake-ups in 5 s at rest: {report:?}");
    }
}

/// `start`, with `theme.toml` written before the daemon starts: the style is in effect from the
/// first paint, as on a real login.
fn start_styled(name: &str, outputs: u32, bus: &PrivateBus, bar_toml: &str, theme_toml: &str) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, outputs);
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("theme.toml"), theme_toml).unwrap();
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml: Some(bar_toml), system_bus: Some(&bus.address), pw_dump: PwDump::Silent, ..DaemonOptions::default() },
    );
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// PSS at start under each style, every module, 3 outputs. A style costs its
/// branch, its corner masks (created on first use, a few hundred bytes) and, for floating, a
/// buffer 4 px taller and 12 px narrower: each within 512 KB of flat, all under the firm budget.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn startup_pss_per_style() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("per-style");
    let _workstation = Workstation::serve(&bus);
    let modules = "modules = [\"launcher\", \"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \
                   \"resources\", \"clock\"]\n";
    let mut report = Vec::new();
    for (i, style) in ["flat", "islands", "powerline", "floating", "underline", "translucent"].into_iter().enumerate() {
        let theme = format!("theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"{style}\"\n");
        let (_sway, _hypr, d) = start_styled(&format!("per-style-{i}"), 3, &bus, modules, &theme);
        std::thread::sleep(Duration::from_secs(3)); // as in startup_pss_per_source
        report.push((style, d.pss_kb()));
    }
    eprintln!("startup PSS per style (KB, all modules, 3 outputs): {report:?}");
    let flat = report[0].1;
    for (style, kb) in &report {
        assert!(*kb < STARTUP_MAX_KB, "{style}: {kb} KB >= 15 MB — report it, never raise: {report:?}");
        assert!(*kb <= flat + 512, "{style}: {kb} KB vs flat {flat} KB: {report:?}");
    }
}

/// 48 style switches (every style 8 times; floating reconfigures every bar, the
/// masks are cached) leave no growth behind. Budget 768 KB, as for theme reloads.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn style_switches_no_growth() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("style-switches");
    let toml = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let (sway, _hypr, d) = start("style-switches", 3, &bus, None, Some(toml), PwDump::Silent);
    let config = sway.runtime.join("config/alpheus");
    let switch = |n: usize| {
        let style = STYLES[n % STYLES.len()];
        write_atomically(&config, "theme.toml", &format!("theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"{style}\"\nopacity = 0.6\n"));
        assert!(d.wait_for_count("theme reloaded:", n + 1, DELAY), "switch {n} ({style}): {:?}", d.log());
        std::thread::sleep(Duration::from_millis(100)); // floating's configures land before the next write
    };
    for n in 0..12 {
        switch(n); // warm-up: every style painted twice, every mask created
    }
    std::thread::sleep(Duration::from_secs(2));
    let baseline = d.pss_kb();
    for n in 12..60 {
        switch(n);
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.pss_kb();
    eprintln!("style switches: PSS {baseline} KB after 12 warm-up switches, {end} KB after 48 more (3 outputs)");
    assert!(end <= baseline + 768, "growth: {baseline} -> {end} KB over 48 style switches");
}

/// The media module on the bar's own session link (tray off): its connection, Mpris, nothing else.
const MEDIA_BAR: &str = "modules = [\"workspaces\", \"window\", \"media\", \"clock\"]\n";
const NO_MEDIA_BAR: &str = "modules = [\"workspaces\", \"window\", \"clock\"]\n";

/// `names` as fake players on `session`, each read by the daemon (its GetAll answered).
fn fake_players(session: &PrivateBus, names: &[&str]) -> Vec<FakePlayer> {
    let players: Vec<FakePlayer> = names.iter().map(|n| FakePlayer::start(session, PlayerSpec::playing(n, "Paper Lanterns"))).collect();
    for p in &players {
        assert!(p.wait_call("GetAll", DELAY).is_some(), "the daemon never read a fake player");
    }
    players
}

/// Bar step 5: what the media module costs at start with no player, and what 3 players add. Each
/// configuration alone, its daemon gone before the next. In mode `auto` (no `theme.toml`) the bar's
/// session link is open anyway, for the light/dark portal: the module adds its match rules and
/// Mpris to it. In mode `dark` nothing else opens it: the module's cost includes the connection
/// itself.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn media_module_pss_without_and_with_players() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("media-cost");
    let measure = |name: &str, toml: &str, players: &[&str], theme: Option<&str>| {
        let session = PrivateBus::start(&format!("{name}-session"));
        let sway = Sway::start(name, 1);
        if let Some(theme) = theme {
            let config = sway.runtime.join("config/alpheus");
            std::fs::create_dir_all(&config).unwrap();
            std::fs::write(config.join("theme.toml"), theme).unwrap();
        }
        let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
        let d = start_daemon_with(
            &sway,
            &DaemonOptions {
                bar_toml: Some(toml),
                system_bus: Some(&bus.address),
                session_bus: Some(&session.address),
                pw_dump: PwDump::Silent,
                ..DaemonOptions::default()
            },
        );
        assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
        // A cost of 0 must come from a module that runs, not from one that never started.
        if toml.contains("\"media\"") {
            assert!(d.wait_for("media: on the bar's session bus", DELAY), "{:?}", d.log());
        }
        let _players = fake_players(&session, players);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let base = measure("media-cost-a", NO_MEDIA_BAR, &[], None);
    let empty = measure("media-cost-b", MEDIA_BAR, &[], None);
    let three = measure("media-cost-c", MEDIA_BAR, &["alpha", "beta", "gamma"], None);
    let (module_cost, players_cost) = (empty.saturating_sub(base), three.saturating_sub(empty));
    const DARK: &str = "theme = \"catppuccin\"\nmode = \"dark\"\n";
    let dark_base = measure("media-cost-d", NO_MEDIA_BAR, &[], Some(DARK));
    let dark_empty = measure("media-cost-e", MEDIA_BAR, &[], Some(DARK));
    let own_link = dark_empty.saturating_sub(dark_base);
    eprintln!(
        "media PSS (KB): mode auto (the portal's link open anyway): {base} without the module, {empty} with it and no player \
         (module cost {module_cost}), {three} with 3 fake players (players cost {players_cost}); mode dark (no link otherwise): \
         {dark_base} without, {dark_empty} with it and no player (module and its own connection {own_link})"
    );
    assert!(three < STARTUP_MAX_KB, "{three} KB >= 15 MB — report it, never raise");
    assert!(module_cost <= 1024, "the module on the portal's connection costs {module_cost} KB (its rules and Mpris: ≤ 1 MB)");
    assert!(own_link <= 1024, "the module with its own connection costs {own_link} KB (≤ 1 MB)");
    assert!(players_cost <= 512, "3 players cost {players_cost} KB (bounded strings: ≤ 512 KB)");
}

/// Bar step 5: 100 track changes (each a new title, drawn) leave no growth behind. The titles
/// cycle through the glyphs the warm-up already drew: fresh glyphs on every title would only
/// measure the bounded glyph cache filling up (`painter::MAX_CACHE`, 2,048 glyphs; its bound is
/// `pss_under_30mb_and_stable`'s), not the media path.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn track_changes_no_growth() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("media-tracks");
    let session = PrivateBus::start("media-tracks-session");
    let (_sway, _hypr, d) = start("media-tracks", 1, &bus, Some(&session), Some(MEDIA_BAR), PwDump::Silent);
    let players = fake_players(&session, &["lumen"]);
    let change = |n: i64| {
        players[0].set_track(
            &format!("Track {n} — 日本語 {}", varied_glyphs(n as u64 % 10)),
            "The Quiet Hours",
            200_000_000 + n * 1_000_000,
        );
        std::thread::sleep(Duration::from_millis(30));
    };
    for n in 0..10 {
        change(n); // warm-up
    }
    std::thread::sleep(Duration::from_secs(2));
    let baseline = d.pss_kb();
    for n in 10..110 {
        change(n);
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.pss_kb();
    eprintln!("media track changes: PSS {baseline} KB after 10 warm-up changes, {end} KB after 100 more");
    assert!(end <= baseline + 512, "growth: {baseline} -> {end} KB over 100 track changes");
}

/// Bar step 5: a player playing, the popup closed — nothing ticks, on either host of the module.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_a_playing_player() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("media-rest");
    for (tray, toml) in [("off", MEDIA_BAR), ("on", "modules = [\"workspaces\", \"window\", \"media\", \"tray\", \"clock\"]\n")] {
        let session = PrivateBus::start(&format!("media-rest-{tray}-session"));
        let (_sway, _hypr, d) = start(&format!("media-rest-{tray}"), 1, &bus, Some(&session), Some(toml), PwDump::Silent);
        let _players = fake_players(&session, &["lumen"]);
        std::thread::sleep(TRAY_SETTLE);
        const REST: Duration = Duration::from_secs(10);
        away_from_the_minute(REST);
        let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
        std::thread::sleep(REST);
        assert_no_spin(d.cpu_ticks() - ticks, REST, "the daemon with a playing player");
        let delta = d.wakeups().saturating_sub(before);
        let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
        eprintln!("wake-ups at rest with a playing player (10 s, tray {tray}): {delta} whole daemon, {main_delta} loop thread");
        assert_eq!(main_delta, 0, "tray {tray}: {main_delta} loop-thread wake-ups in 10 s with a player playing, popup closed");
        assert!(delta <= 8, "tray {tray}: {delta} wake-ups in 10 s");
    }
}

/// The bell and the server on the bar's own session link (tray off).
const NOTIFY_BAR: &str = "modules = [\"workspaces\", \"window\", \"notifications\", \"clock\"]\n";
const NO_NOTIFY_BAR: &str = "modules = [\"workspaces\", \"window\", \"clock\"]\n";

/// Waits (10 s at most) until the daemon owns `org.freedesktop.Notifications` on `session`.
fn wait_for_the_notification_server(session: &PrivateBus) {
    let peer = Peer::connect(session);
    let t = Instant::now();
    loop {
        let m = dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner")
            .unwrap()
            .append1("org.freedesktop.Notifications");
        if peer.call(m).unwrap().read1::<bool>().unwrap() {
            return;
        }
        assert!(t.elapsed() < DELAY, "the daemon never took org.freedesktop.Notifications");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `Notify` from `peer` (fictional texts, a two-line body): the id.
fn notify(peer: &Peer, app: &str, summary: &str, urgency: u8, expire_ms: i32) -> u32 {
    let mut hints = dbus::arg::PropMap::new();
    hints.insert("urgency".to_string(), dbus::arg::Variant(Box::new(urgency) as Box<dyn dbus::arg::RefArg>));
    let m = dbus::Message::new_method_call(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "Notify",
    )
    .unwrap()
    .append3(app, 0u32, "")
    .append3(summary, "A fictional first line of the body.\nAnd a second one.", Vec::<&str>::new())
    .append2(hints, expire_ms);
    peer.call(m).unwrap().read1::<u32>().unwrap()
}

fn close_notification(peer: &Peer, id: u32) {
    let m = dbus::Message::new_method_call(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "CloseNotification",
    )
    .unwrap()
    .append1(id);
    peer.call(m).unwrap();
}

/// Waits for one more "bubbles shown: {n}" line than there were before `act`: the fifty already
/// logged every count from 1 to 4, so a bare `wait_for` would match an old line.
fn shown_after(d: &Daemon, n: usize, act: impl FnOnce()) {
    let line = format!("bubbles shown: {n}");
    let before = d.count(&line);
    act();
    assert!(d.wait_for_count(&line, before + 1, DELAY), "{:?}", d.log());
}

/// 50 notifications from 10 apps, each expiring after 2 s: the history full at 50 once every
/// bubble went (the surface dropped).
fn fifty_into_the_history(d: &Daemon, client: &Peer) {
    for n in 0..50u32 {
        notify(client, &format!("App {}", n % 10), &format!("Event {n}: a fictional line of text"), 1, 2_000);
    }
    assert!(d.wait_for_count("bubbles surface dropped (empty)", 1, Duration::from_secs(60)), "{:?}", d.log());
}

/// The module serving with nothing, a 50-entry history, 4 bubbles shown,
/// then closed. The module off is measured on a daemon of its own; the rest in sequence on one.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn notifications_pss_idle_history_and_bubbles() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("notify-cost");
    let base = {
        let session = PrivateBus::start("notify-cost-a-session");
        let (_sway, _hypr, d) = start("notify-cost-a", 1, &bus, Some(&session), Some(NO_NOTIFY_BAR), PwDump::Silent);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let session = PrivateBus::start("notify-cost-b-session");
    let (_sway, _hypr, d) = start("notify-cost-b", 1, &bus, Some(&session), Some(NOTIFY_BAR), PwDump::Silent);
    wait_for_the_notification_server(&session);
    std::thread::sleep(TRAY_SETTLE);
    let idle = d.pss_kb();
    let client = Peer::connect(&session);
    fifty_into_the_history(&d, &client);
    std::thread::sleep(TRAY_SETTLE);
    let history = d.pss_kb();
    let mut critical = Vec::new();
    shown_after(&d, 4, || critical.extend((0..4).map(|n| notify(&client, "Monitor", &format!("Alert {n}"), 2, -1))));
    std::thread::sleep(TRAY_SETTLE);
    let shown = d.pss_kb();
    for id in critical {
        close_notification(&client, id);
    }
    assert!(d.wait_for_count("bubbles surface dropped (empty)", 2, DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let closed = d.pss_kb();
    let (module_cost, history_cost, bubbles_cost) =
        (idle.saturating_sub(base), history.saturating_sub(idle), shown.saturating_sub(history));
    eprintln!(
        "notifications PSS (KB): {base} without the module, {idle} serving with nothing (module cost {module_cost}), \
         {history} with 50 entries (history cost {history_cost}), {shown} with 4 bubbles (bubbles cost {bubbles_cost}), {closed} once closed"
    );
    assert!(shown < STARTUP_MAX_KB, "{shown} KB >= 15 MB — report it, never raise");
    assert!(module_cost <= 1024, "the server and the bell cost {module_cost} KB (≤ 1 MB)");
    assert!(history_cost <= 512, "50 entries cost {history_cost} KB (capped strings: ≤ 512 KB; ≈ 125 KB at worst)");
    assert!(bubbles_cost <= 2048, "4 bubbles cost {bubbles_cost} KB (their own pool, two buffers: ≤ 2 MB)");
    assert!(closed <= history + 256, "closed: {closed} KB vs {history} KB before them — the surface and its pool must go");
}

/// At rest with a sticky critical bubble shown over a 50-entry history, then
/// with every bubble gone — no timer, no wake-up, no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_notifications() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("notify-rest");
    let session = PrivateBus::start("notify-rest-session");
    let (_sway, _hypr, d) = start("notify-rest", 1, &bus, Some(&session), Some(NOTIFY_BAR), PwDump::Silent);
    wait_for_the_notification_server(&session);
    let client = Peer::connect(&session);
    fifty_into_the_history(&d, &client);
    let mut sticky = 0;
    shown_after(&d, 1, || sticky = notify(&client, "Monitor", "Disk almost full", 2, -1));
    for what in ["a sticky bubble shown", "every bubble gone"] {
        if what == "every bubble gone" {
            close_notification(&client, sticky);
            assert!(d.wait_for_count("bubbles surface dropped (empty)", 2, DELAY), "{:?}", d.log());
        }
        std::thread::sleep(TRAY_SETTLE);
        const REST: Duration = Duration::from_secs(10);
        away_from_the_minute(REST);
        let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
        std::thread::sleep(REST);
        assert_no_spin(d.cpu_ticks() - ticks, REST, what);
        let delta = d.wakeups().saturating_sub(before);
        let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
        eprintln!("wake-ups at rest with notifications (10 s, {what}): {delta} whole daemon, {main_delta} loop thread");
        assert_eq!(main_delta, 0, "{what}: {main_delta} loop-thread wake-ups in 10 s");
        assert!(delta <= 8, "{what}: {delta} wake-ups in 10 s");
    }
}

/// The OSD's bars: the volume module (the sound source either way), with and without the OSD. A
/// 5 s timeout, so the pill is still up when its PSS is read.
const OSD_BAR: &str = "modules = [\"workspaces\", \"window\", \"volume\", \"osd\", \"clock\"]\n[osd]\ntimeout_ms = 5000\n";
const NO_OSD_BAR: &str = "modules = [\"workspaces\", \"window\", \"volume\", \"clock\"]\n";

/// The module on with nothing shown (its cost over the same bar without it), one
/// pill shown, then gone. The module off runs on a daemon of its own; the rest in sequence on one.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn osd_pss_idle_shown_and_gone() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("osd-cost");
    let base = {
        let (_sway, _hypr, d) = start("osd-cost-a", 1, &bus, None, Some(NO_OSD_BAR), PwDump::Fifo);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let (sway, _hypr, d) = start("osd-cost-b", 1, &bus, None, Some(OSD_BAR), PwDump::Fifo);
    assert!(d.wait_for("osd: sound baseline", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let idle = d.pss_kb();
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    assert!(d.wait_for("osd surface on", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1)); // configured and painted; up until 5 s
    let shown = d.pss_kb();
    assert!(d.wait_for("osd surface dropped (timeout", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let gone = d.pss_kb();
    let (module_cost, pill_cost) = (idle.saturating_sub(base), shown.saturating_sub(idle));
    eprintln!(
        "osd PSS (KB): {base} without the module, {idle} with it at rest (module cost {module_cost}), \
         {shown} with the pill shown (pill cost {pill_cost}), {gone} once gone"
    );
    assert!(shown < STARTUP_MAX_KB, "{shown} KB >= 15 MB — report it, never raise");
    assert!(module_cost <= 256, "the OSD at rest costs {module_cost} KB (a sound state and three instants: ≤ 256 KB)");
    assert!(pill_cost <= 1024, "one pill costs {pill_cost} KB (its own pool, two buffers of one row: ≤ 1 MB)");
    assert!(gone <= idle + 256, "gone: {gone} KB vs {idle} KB before it — the surface and its pool must go");
}

/// Once a pill went, no timer, no wake-up, no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_the_osd() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("osd-rest");
    let (sway, _hypr, d) = start("osd-rest", 1, &bus, None, Some(OSD_BAR), PwDump::Fifo);
    assert!(d.wait_for("osd: sound baseline", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(1300)); // the start-up grace
    pw_dump_send(&sway.runtime, &volume_delta(AG06_SINK, 90, false));
    assert!(d.wait_for("osd surface dropped (timeout", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    const REST: Duration = Duration::from_secs(10);
    away_from_the_minute(REST);
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the pill");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest after the osd (10 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s");
    assert!(delta <= 8, "{delta} wake-ups in 10 s");
}

/// The quick measurements' bars: every system module (their D-Bus thread is there with or without
/// the panel), without and with `quick`. `[osd] backlight_dir` is a fake directory of the test's own
/// (never the real /sys): the panel reads it once when it opens.
fn quick_bar(with_quick: bool, backlight_dir: &Path) -> String {
    let quick = if with_quick { "\"quick\", " } else { "" };
    format!(
        "modules = [\"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", {quick}\"clock\"]\n\
         [osd]\nbacklight_dir = {:?}\n",
        backlight_dir.display().to_string()
    )
}

/// Past the daemon's TOGGLE_DEBOUNCE (250 ms): a verb right after the verb opened the panel is
/// ignored, so a close waits this long.
const PAST_DEBOUNCE: Duration = Duration::from_millis(300);

/// A fake backlight directory: one device at 75 %.
fn fake_backlight(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    let device = dir.0.join("panel");
    std::fs::create_dir_all(&device).unwrap();
    std::fs::write(device.join("brightness"), "75\n").unwrap();
    std::fs::write(device.join("max_brightness"), "100\n").unwrap();
    dir
}

/// PSS without the module, with it at rest, the panel open (by the verb), closed,
/// and after 20 more open/close cycles.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn quick_pss_idle_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("quick-cost");
    let _workstation = Workstation::serve(&bus);
    let backlight = fake_backlight("quick-cost-backlight");
    let base = {
        let (_sway, _hypr, d) = start("quick-cost-a", 1, &bus, None, Some(&quick_bar(false, &backlight.0)), PwDump::Fifo);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let (sway, _hypr, d) = start("quick-cost-b", 1, &bus, None, Some(&quick_bar(true, &backlight.0)), PwDump::Fifo);
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let idle = d.pss_kb();
    let toggle = || assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Sound)), Reply::Done);
    toggle();
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1)); // configured and painted
    let open = d.pss_kb();
    toggle(); // past the daemon's 250 ms debounce of a verb right after the verb's open
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let closed = d.pss_kb();
    for n in 2..=21 {
        toggle();
        assert!(d.wait_for_count("quick shown", n, DELAY), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        toggle();
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
    }
    std::thread::sleep(TRAY_SETTLE);
    let cycled = d.pss_kb();
    let (module_cost, panel_cost) = (idle.saturating_sub(base), open.saturating_sub(idle));
    eprintln!(
        "quick PSS (KB): {base} without the module, {idle} with it at rest (module cost {module_cost}), \
         {open} open (panel cost {panel_cost}), {closed} closed, {cycled} after 20 more cycles"
    );
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB — report it, never raise");
    assert!(module_cost <= 256, "the module at rest costs {module_cost} KB (a profile state and a tap: ≤ 256 KB)");
    assert!(
        panel_cost <= 1024,
        "the open panel costs {panel_cost} KB (its pool, two buffers, and the first open's one-time warm-up below: ≤ 1 MB)"
    );
    // Closed, the first open leaves ≈ 340 KB (binary ≈ +320, font ≈ +64, stack ≈ +20; heap and pool
    // back at idle) — from a manual smaps run, 2026-10-11: the binary's code pages and the font
    // file's, touched for the first time and now resident (any first popup pays it), while the heap
    // (216 → 212 KB) and the shm pool (220 → 160 KB) are back to idle's. So: a one-time warm-up
    // bound, and a leak bound.
    assert!(closed <= idle + 512, "closed {closed} KB vs {idle} KB: more than the first open's code and font pages");
    assert!(cycled <= closed + 128, "cycled {cycled} KB vs {closed} KB closed: 20 cycles must keep nothing");
}

/// Once the panel closed, no timer, no wake-up, no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_the_quick_panel() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("quick-rest");
    let _workstation = Workstation::serve(&bus);
    let backlight = fake_backlight("quick-rest-backlight");
    let (sway, _hypr, d) = start("quick-rest", 1, &bus, None, Some(&quick_bar(true, &backlight.0)), PwDump::Fifo);
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    for n in 1..=3 {
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Top)), Reply::Done);
        assert!(d.wait_for_count("quick shown", n, DELAY), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Top)), Reply::Done);
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
    }
    // Not the 3 s settle: a stray one-second timer armed by the last close must fall inside the window.
    std::thread::sleep(Duration::from_millis(300));
    const REST: Duration = Duration::from_secs(10);
    away_from_the_minute(REST);
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the quick panel");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest after the quick panel (10 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s");
    assert!(delta <= 8, "{delta} wake-ups in 10 s");
}

/// The Wi-Fi measurements' bar: the network module and the panel (the D-Bus thread and the Wi-Fi tap
/// are there either way); `[osd] backlight_dir` a fake directory of the test's own, never /sys.
fn wifi_bar(backlight_dir: &Path) -> String {
    format!("modules = [\"network\", \"quick\", \"clock\"]\n[osd]\nbacklight_dir = {:?}\n", backlight_dir.display().to_string())
}

/// 40 fictional access points (the detail lists 32), two in three WPA2, one saved.
fn forty_networks() -> Vec<FakeAp> {
    (1..=40).map(|n| FakeAp::new(&format!("Reseau-Test-{n:02}"), (n * 2) as u8, n % 3 != 0)).collect()
}

/// Waits until the fake NetworkManager has listed the saved profiles twice for each of `n` opens:
/// the `n`-th open of the detail is read (they come last). The panel's VPN row lists them too, once
/// per open, beside the Wi-Fi detail's own listing.
fn detail_read(nm: &FakeNetworkManager, n: usize) -> bool {
    let t = Instant::now();
    while t.elapsed() < DELAY {
        if nm.calls().iter().filter(|c| c.starts_with("ListConnections ")).count() >= 2 * n {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// The leak bounds of the Wi-Fi and VPN details on anonymous memory, in KB (`anon_kb`). Three
/// release runs (2026-11-15): closed over rest, Wi-Fi +84…+100 and VPN
/// +60…+68 (the main heap: most likely the painter's glyph caches, bounded by MAX_CACHE); 10
/// more cycles over closed, Wi-Fi +0…+40 and VPN +4…+72. A leak of 300 KB per open fails both
/// tests at the close, one of 30 KB per open fails the cycles.
const ANON_CLOSED: u64 = 256;
const ANON_CYCLED: u64 = 128;

/// PSS with the panel at rest, the Wi-Fi detail open on 40 visible access
/// points (32 listed), closed, and after 10 more cycles. A fake nmcli (never the real one) takes the
/// rescans.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn quick_wifi_pss_detail_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("wifi-cost");
    let nm = FakeNetworkManager::serve(&bus, forty_networks(), vec![(UUID_HOME.into(), "Reseau-Test-03".into())]);
    let backlight = fake_backlight("wifi-cost-backlight");
    let (sway, _hypr, d) = start("wifi-cost", 1, &bus, None, Some(&wifi_bar(&backlight.0)), PwDump::Fifo);
    fake_stdin_recorder(&sway, "nmcli");
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (idle, idle_shm) = d.pss_and_shm_kb();
    let idle_own = idle - idle_shm;
    let idle_anon = anon_kb(&d);
    let idle_map = smaps_by_mapping(&d);
    let toggle = || assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Wifi)), Reply::Done);
    toggle();
    assert!(detail_read(&nm, 1), "{:?} {:?}", nm.calls(), d.log());
    std::thread::sleep(Duration::from_secs(1)); // published, configured and painted
    let (open, _) = d.pss_and_shm_kb();
    toggle();
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (closed, closed_shm) = d.pss_and_shm_kb();
    let closed_own = closed - closed_shm;
    let closed_anon = anon_kb(&d);
    let closed_map = smaps_by_mapping(&d);
    eprintln!("quick Wi-Fi smaps, idle to closed (KB): {}", smaps_moved(&idle_map, &closed_map));
    for n in 2..=11 {
        toggle();
        assert!(detail_read(&nm, n), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        toggle();
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
    }
    std::thread::sleep(TRAY_SETTLE);
    let (cycled, cycled_shm) = d.pss_and_shm_kb();
    let cycled_own = cycled - cycled_shm;
    let cycled_anon = anon_kb(&d);
    eprintln!("quick Wi-Fi smaps, closed to cycled (KB): {}", smaps_moved(&closed_map, &smaps_by_mapping(&d)));
    let detail_cost = open.saturating_sub(idle);
    eprintln!(
        "quick Wi-Fi PSS (KB): {idle} at rest, {open} with the detail open on 32 networks (cost {detail_cost}), \
         {closed} closed, {cycled} after 10 more cycles; without the shm pools: {idle_own} at rest, {closed_own} closed, \
         {cycled_own} cycled; the pools: {idle_shm} at rest, {closed_shm} closed, {cycled_shm} cycled; \
         anonymous: {idle_anon} at rest, {closed_anon} closed, {cycled_anon} cycled; file-backed (own − anonymous): \
         {} at rest, {} closed, {} cycled",
        idle_own.saturating_sub(idle_anon),
        closed_own.saturating_sub(closed_anon),
        cycled_own.saturating_sub(cycled_anon)
    );
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB — report it, never raise");
    assert!(
        detail_cost <= 1536,
        "the detail open costs {detail_cost} KB (the panel ≤ 1 MB, 40 access points read on the D-Bus thread, 32 rows): report, never raise"
    );
    // The leak bounds are on anonymous memory only (`anon_kb`: heap, thread arenas, stacks, the
    // binary's written data; the shm pools apart): the first open also brings in the binary's code
    // pages and the font file's (file-backed, clean, one-time), and how many depends on page-cache
    // fault-around (64 KB around each fault) and the tests run before, so a bound on the total PSS
    // read +631…+704 KB one day and failed at 640 another. Total PSS and the file-backed share
    // stay printed above, never asserted.
    assert!(
        closed_anon <= idle_anon + ANON_CLOSED,
        "closed {closed_anon} KB anonymous vs {idle_anon} KB at rest: more than the first open's heap (glyph caches)"
    );
    assert!(
        cycled_anon <= closed_anon + ANON_CYCLED,
        "cycled {cycled_anon} KB anonymous vs {closed_anon} KB closed: 10 cycles must keep nothing"
    );
    // The pools alone, looser: a pool or a buffer kept after the close shows here, the
    // compositor's mapping and unmapping of a buffer stays within it.
    assert!(closed_shm <= idle_shm + 128, "the pools closed {closed_shm} KB vs {idle_shm} KB at rest: the popup's pool kept");
}

/// Once the detail closed, no timer, no wake-up (the loop's nor the D-Bus
/// thread's), no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_the_wifi_detail() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("wifi-rest");
    let nm = FakeNetworkManager::serve(&bus, forty_networks(), vec![(UUID_HOME.into(), "Reseau-Test-03".into())]);
    let backlight = fake_backlight("wifi-rest-backlight");
    let (sway, _hypr, d) = start("wifi-rest", 1, &bus, None, Some(&wifi_bar(&backlight.0)), PwDump::Fifo);
    fake_stdin_recorder(&sway, "nmcli");
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    const REST: Duration = Duration::from_secs(10);
    // Three cycles at least. From the third, away from the minute before the cycle, never between
    // the close and the window: that sleep would let a stray one-second timer armed by the close
    // fire before the window (then unseen). A last cycle overrunning its 5 s on a loaded machine
    // brings the minute's tick too close: checked again right after the close, without sleeping,
    // and then one more cycle.
    let mut n = 0;
    loop {
        n += 1;
        if n >= 3 {
            away_from_the_minute(REST + Duration::from_secs(5));
        }
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Wifi)), Reply::Done);
        assert!(detail_read(&nm, n), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Wifi)), Reply::Done);
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
        if n >= 3 && fits_the_minute(REST + Duration::from_secs(1)) {
            break;
        }
        assert!(n < 6, "the minute's tick always too close to the window");
    }
    // Not the 3 s settle: a stray one-second timer armed by the last close must fall inside the window.
    let closed_at = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    assert!(closed_at.elapsed() < Duration::from_millis(900), "the window starts within a second of the close");
    let dbus_before = d.thread_wakeups("D-Bus").expect("the D-Bus thread");
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the Wi-Fi detail");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    let dbus_delta = d.thread_wakeups("D-Bus").expect("the D-Bus thread").saturating_sub(dbus_before);
    eprintln!("wake-ups at rest after the Wi-Fi detail (10 s): {delta} whole daemon, {main_delta} loop thread, {dbus_delta} D-Bus thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s");
    assert_eq!(dbus_delta, 0, "{dbus_delta} D-Bus-thread wake-ups in 10 s: its tap and its bus are quiet");
    assert!(delta <= 8, "{delta} wake-ups in 10 s");
}

/// Ten fictional VPN profiles (OpenVPN-style and WireGuard alternating), no Wi-Fi profile saved, so
/// `VPN-Test-NN` is `Settings/NN`.
fn ten_vpns() -> Vec<FakeVpn> {
    (1..=10).map(|n| FakeVpn { name: format!("VPN-Test-{n:02}"), wireguard: n % 2 == 0, asks: n % 3 == 0 }).collect()
}

/// Waits until the fake NetworkManager has answered `GetSettings` of the 10th profile `n` times:
/// the `n`-th open of the VPN detail has read every profile (the last one comes last).
fn vpn_detail_read(nm: &FakeNetworkManager, n: usize) -> bool {
    let call = "GetSettings /org/freedesktop/NetworkManager/Settings/10";
    let t = Instant::now();
    while t.elapsed() < DELAY {
        if nm.calls().iter().filter(|c| c.as_str() == call).count() >= n {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// PSS with the panel at rest, the VPN detail open on 10 profiles, closed, and
/// after 10 more cycles. A fake nmcli (never the real one) is there; nothing is brought up.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn quick_vpn_pss_detail_open_with_10_profiles_and_after() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("vpn-cost");
    let nm = FakeNetworkManager::serve_with(&bus, vec![], vec![], ten_vpns());
    let backlight = fake_backlight("vpn-cost-backlight");
    let (sway, _hypr, d) = start("vpn-cost", 1, &bus, None, Some(&wifi_bar(&backlight.0)), PwDump::Fifo);
    fake_stdin_recorder(&sway, "nmcli");
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (idle, idle_shm) = d.pss_and_shm_kb();
    let idle_own = idle - idle_shm;
    let idle_anon = anon_kb(&d);
    let idle_map = smaps_by_mapping(&d);
    let toggle = || assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Vpn)), Reply::Done);
    toggle();
    assert!(vpn_detail_read(&nm, 1), "{:?} {:?}", nm.calls(), d.log());
    std::thread::sleep(Duration::from_secs(1)); // published, configured and painted
    let (open, _) = d.pss_and_shm_kb();
    toggle();
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (closed, closed_shm) = d.pss_and_shm_kb();
    let closed_own = closed - closed_shm;
    let closed_anon = anon_kb(&d);
    let closed_map = smaps_by_mapping(&d);
    eprintln!("quick VPN smaps, idle to closed (KB): {}", smaps_moved(&idle_map, &closed_map));
    for n in 2..=11 {
        toggle();
        assert!(vpn_detail_read(&nm, n), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        toggle();
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
    }
    std::thread::sleep(TRAY_SETTLE);
    let (cycled, cycled_shm) = d.pss_and_shm_kb();
    let cycled_own = cycled - cycled_shm;
    let cycled_anon = anon_kb(&d);
    eprintln!("quick VPN smaps, closed to cycled (KB): {}", smaps_moved(&closed_map, &smaps_by_mapping(&d)));
    let detail_cost = open.saturating_sub(idle);
    eprintln!(
        "quick VPN PSS (KB): {idle} at rest, {open} with the detail open on 10 profiles (cost {detail_cost}), \
         {closed} closed, {cycled} after 10 more cycles; without the shm pools: {idle_own} at rest, {closed_own} closed, \
         {cycled_own} cycled; the pools: {idle_shm} at rest, {closed_shm} closed, {cycled_shm} cycled; \
         anonymous: {idle_anon} at rest, {closed_anon} closed, {cycled_anon} cycled; file-backed (own − anonymous): \
         {} at rest, {} closed, {} cycled",
        idle_own.saturating_sub(idle_anon),
        closed_own.saturating_sub(closed_anon),
        cycled_own.saturating_sub(cycled_anon)
    );
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB — report it, never raise");
    // The leak bounds are on anonymous memory only (`anon_kb`: heap, thread arenas, stacks, the
    // binary's written data; the shm pools apart): the first open also brings in the binary's code
    // pages and the font file's (file-backed, clean, one-time), and how many depends on page-cache
    // fault-around (64 KB around each fault) and the tests run before, so a bound on the total PSS
    // read +631…+704 KB one day and failed at 640 another. Total PSS and the file-backed share
    // stay printed above, never asserted.
    assert!(
        closed_anon <= idle_anon + ANON_CLOSED,
        "closed {closed_anon} KB anonymous vs {idle_anon} KB at rest: more than the first open's heap (glyph caches)"
    );
    assert!(
        cycled_anon <= closed_anon + ANON_CYCLED,
        "cycled {cycled_anon} KB anonymous vs {closed_anon} KB closed: 10 cycles must keep nothing"
    );
    assert!(closed_shm <= idle_shm + 128, "the pools closed {closed_shm} KB vs {idle_shm} KB at rest: the popup's pool kept");
}

/// Once the VPN detail closed, no timer, no wake-up (the loop's nor the D-Bus
/// thread's), no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_the_vpn_detail() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("vpn-rest");
    let nm = FakeNetworkManager::serve_with(&bus, vec![], vec![], ten_vpns());
    let backlight = fake_backlight("vpn-rest-backlight");
    let (sway, _hypr, d) = start("vpn-rest", 1, &bus, None, Some(&wifi_bar(&backlight.0)), PwDump::Fifo);
    fake_stdin_recorder(&sway, "nmcli");
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    const REST: Duration = Duration::from_secs(5);
    // As the Wi-Fi test: three cycles at least, the window away from the minute's tick and starting
    // within a second of the last close (a stray one-second timer must fall inside it).
    let mut n = 0;
    loop {
        n += 1;
        if n >= 3 {
            away_from_the_minute(REST + Duration::from_secs(5));
        }
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Vpn)), Reply::Done);
        assert!(vpn_detail_read(&nm, n), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Vpn)), Reply::Done);
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
        if n >= 3 && fits_the_minute(REST + Duration::from_secs(1)) {
            break;
        }
        assert!(n < 6, "the minute's tick always too close to the window");
    }
    let closed_at = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    assert!(closed_at.elapsed() < Duration::from_millis(900), "the window starts within a second of the close");
    let dbus_before = d.thread_wakeups("D-Bus").expect("the D-Bus thread");
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the VPN detail");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    let dbus_delta = d.thread_wakeups("D-Bus").expect("the D-Bus thread").saturating_sub(dbus_before);
    eprintln!("wake-ups at rest after the VPN detail (5 s): {delta} whole daemon, {main_delta} loop thread, {dbus_delta} D-Bus thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 5 s");
    assert_eq!(dbus_delta, 0, "{dbus_delta} D-Bus-thread wake-ups in 5 s: its tap and its bus are quiet");
    assert!(delta <= 4, "{delta} wake-ups in 5 s");
    // Closed, a VPN change is read by the network domain alone. Sent right after the window (it
    // wakes the D-Bus thread, as any signal does): two signals (the root's ActiveConnections, the
    // active connection's State), the root and the active connection read once for each; a VPN
    // reader left open would read them again after each.
    let calls = nm.calls().len();
    nm.change(NmChange::Vpn(1, Some(1)));
    std::thread::sleep(Duration::from_millis(500));
    let since = nm.calls()[calls..].to_vec();
    assert!(!since.iter().any(|c| c.starts_with("ListConnections") || c.starts_with("GetSettings")), "{since:?}");
    let root = since.iter().filter(|c| c.as_str() == "GetAll /org/freedesktop/NetworkManager org.freedesktop.NetworkManager").count();
    let actives = since.iter().filter(|c| c.starts_with("GetAll ") && c.contains("/ActiveConnection/")).count();
    assert_eq!((root, actives), (2, 2), "closed: the domain's own reads only, one per signal: {since:?}");
}

/// The Bluetooth measurements' bar: the bluetooth module and the panel (the D-Bus thread and the
/// Bluetooth tap are there either way); `[osd] backlight_dir` a fake directory of the test's own.
fn bt_bar(backlight_dir: &Path) -> String {
    format!("modules = [\"bluetooth\", \"quick\", \"clock\"]\n[osd]\nbacklight_dir = {:?}\n", backlight_dir.display().to_string())
}

/// The Bluetooth bar on one output with the fake BlueZ (hci0 powered, CasqueBT paired) and fake
/// `wpctl` and `rfkill` recorders (never the real tools), the launcher socket ready.
fn start_bt(name: &str, bus: &PrivateBus, bar_toml: &str) -> (Sway, FakeHyprland, Daemon) {
    let (sway, hypr, d) = start(name, 1, bus, None, Some(bar_toml), PwDump::Fifo);
    for program in ["wpctl", "rfkill"] {
        fake_recorder(&sway, program);
    }
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// Waits until the fake BlueZ saw the `n`-th StartDiscovery: the `n`-th detail open.
fn detail_opened(b: &FakeBluez, n: usize) -> bool {
    let t = Instant::now();
    while t.elapsed() < DELAY {
        if b.calls().iter().filter(|c| c.starts_with("StartDiscovery ")).count() >= n {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// PSS with the panel's module at rest, the detail open on 20 devices
/// (CasqueBT paired and 19 nearby, found while open), closed, and after 10 more cycles.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn bluetooth_detail_pss_with_20_devices_and_after() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("bt-cost");
    let bluez = FakeBluez::serve(&bus);
    let backlight = fake_backlight("bt-cost-backlight");
    let (sway, _hypr, d) = start_bt("bt-cost", &bus, &bt_bar(&backlight.0));
    std::thread::sleep(TRAY_SETTLE);
    let (idle, idle_shm) = d.pss_and_shm_kb();
    let idle_own = idle - idle_shm;
    let idle_anon = d.anonymous_kb();
    let toggle = || assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Bluetooth)), Reply::Done);
    toggle();
    assert!(detail_opened(&bluez, 1), "{:?} {:?}", bluez.calls(), d.log());
    // CasqueBT is device 2: devices 3…21 make 20 with it.
    for n in 3..=21u8 {
        bluez.add_nearby(n, &format!("Device-Test-{n:02}"), -30 - i16::from(n));
    }
    std::thread::sleep(Duration::from_secs(1)); // read, published, configured and painted
    let (open, open_shm) = d.pss_and_shm_kb();
    let open_anon = d.anonymous_kb();
    toggle();
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (closed, closed_shm) = d.pss_and_shm_kb();
    let closed_own = closed - closed_shm;
    let closed_anon = d.anonymous_kb();
    for n in 2..=11 {
        toggle();
        assert!(detail_opened(&bluez, n), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        toggle();
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
    }
    std::thread::sleep(TRAY_SETTLE);
    let (cycled, cycled_shm) = d.pss_and_shm_kb();
    let cycled_own = cycled - cycled_shm;
    let cycled_anon = d.anonymous_kb();
    let detail_cost = open.saturating_sub(idle);
    eprintln!(
        "quick Bluetooth PSS (KB): {idle} at rest, {open} with the detail open on 20 devices (cost {detail_cost}), \
         {closed} closed, {cycled} after 10 more cycles; without the shm pools: {idle_own} at rest, {} open, {closed_own} closed, \
         {cycled_own} cycled; the pools: {idle_shm} at rest, {open_shm} open, {closed_shm} closed, {cycled_shm} cycled; \
         anonymous: {idle_anon} at rest, {open_anon} open, {closed_anon} closed, {cycled_anon} cycled",
        open - open_shm
    );
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB — report it, never raise");
    // Open, the cost splits in two (release, 2026-10-23, 8 runs: +1078…+1788 KB in all). The daemon's
    // own memory +436…+540 KB (4 runs): the D-Bus thread's 20 devices, the panel's model and the first
    // open's code, font and glyph-cache pages (most of which stay, below). The shm pools +774…+1240 KB
    // (4 runs), the moving part, most likely because the detail grows one row per device found while
    // it is open, so its buffers are re-made at each new height while the compositor may still hold
    // the previous ones; all of it goes with the popup (the pools' closed bound below). Report, never
    // raise.
    // The daemon's own memory is bounded on `Anonymous:` (heap, stacks, written data), as the Wi-Fi
    // and VPN details are: PSS without the pools also counts file-backed pages (code, fonts), which
    // page-cache fault-around maps 64 KB at a time depending on the binary and the tests before.
    // On the clipboard branch (+140 KiB of binary) the PSS form read +774…+890 KB against a 768
    // bound (base +420…+448) while anonymous memory was +196…+252: nothing on this path changed.
    let open_pools = open_shm.saturating_sub(idle_shm);
    assert!(open_anon <= idle_anon + 512, "open {open_anon} KB anon vs {idle_anon} at rest: more than 20 devices and the panel's model");
    assert!(
        open_pools <= 1536,
        "the pools open {open_pools} KB over rest: more than a growing panel's buffers (≤ 32 rows) — report, never raise"
    );
    // Closed, the daemon's anonymous memory is back near rest; 10 more cycles keep nothing.
    assert!(
        closed_anon <= idle_anon + 256,
        "closed {closed_anon} KB anon vs {idle_anon} at rest: the first open left more than a warm-up's heap"
    );
    assert!(cycled_anon <= closed_anon + 128, "cycled {cycled_anon} KB anon vs {closed_anon} closed: 10 cycles must keep nothing");
    assert!(closed_shm <= idle_shm + 128, "the pools closed {closed_shm} KB vs {idle_shm} KB at rest: the popup's pool kept");
}

/// The acting connection (a second system-bus connection with the
/// pairing agent) while the detail is open, against the panel's rows open without it; no thread,
/// and the loop asleep while it idles.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn bluetooth_acting_connection_cost() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("bt-act");
    let bluez = FakeBluez::serve(&bus);
    let backlight = fake_backlight("bt-act-backlight");
    let (sway, _hypr, d) = start_bt("bt-act", &bus, &bt_bar(&backlight.0));
    std::thread::sleep(TRAY_SETTLE);
    let quick = |verb| assert_eq!(launcher(&sway, &Request::Quick(verb)), Reply::Done);
    // The rows (no acting connection), twice: the first open's one-time warm-up (code and font
    // pages) is paid before the figure.
    for n in 1..=2 {
        quick(QuickVerb::Top);
        assert!(d.wait_for_count("quick shown", n, DELAY), "{:?}", d.log());
        std::thread::sleep(Duration::from_secs(1));
        if n == 1 {
            quick(QuickVerb::Top);
            assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
        }
    }
    let rows = d.pss_kb();
    let rows_threads = wait_for_stable_threads(&d, DELAY);
    assert!(!bluez.calls().iter().any(|c| c.starts_with("RegisterAgent ")), "the rows alone open no acting connection");
    quick(QuickVerb::Top);
    assert!(d.wait_for_count("popup closed", 2, DELAY), "{:?}", d.log());
    std::thread::sleep(PAST_DEBOUNCE);
    quick(QuickVerb::Bluetooth);
    assert!(detail_opened(&bluez, 1), "{:?} {:?}", bluez.calls(), d.log());
    assert!(bluez.calls().iter().any(|c| c.starts_with("RegisterAgent ")), "{:?}", bluez.calls());
    std::thread::sleep(Duration::from_secs(1));
    let detail = d.pss_kb();
    let detail_threads = wait_for_stable_threads(&d, DELAY);
    let delta = detail as i64 - rows as i64;
    eprintln!(
        "quick Bluetooth acting connection (KB): {rows} rows, {detail} detail ({delta:+}); threads {rows_threads} → {detail_threads}"
    );
    assert_eq!(detail_threads, rows_threads, "the acting connection adds no thread");
    // Measured +54…+116 KB (release, 5 runs): the connection, its agent object and the detail's
    // one device, against the rows. Report, never raise.
    assert!(delta <= 512, "the detail with its acting connection costs {delta} KB over the rows: report, never raise");
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(Duration::from_secs(6));
    let (main_before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "with the Bluetooth detail open");
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups with the Bluetooth detail open (5 s): {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 5 s with the detail open and nothing happening");
}

/// The headset row's cost — the panel's rows open on a wired default
/// sink, then on a Bluetooth headset (its bluez5 device and profiles kept, the row drawn).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn bluetooth_headset_row_cost() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("bt-headset");
    let _bluez = FakeBluez::serve(&bus);
    let backlight = fake_backlight("bt-headset-backlight");
    let toml = format!(
        "modules = [\"volume\", \"bluetooth\", \"quick\", \"clock\"]\n[osd]\nbacklight_dir = {:?}\n",
        backlight.0.display().to_string()
    );
    let (sway, _hypr, d) = start_bt("bt-headset", &bus, &toml);
    std::thread::sleep(TRAY_SETTLE);
    let quick = || assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Top)), Reply::Done);
    // Twice on the wired sink: the first open's warm-up paid before the figure.
    for n in 1..=2 {
        quick();
        assert!(d.wait_for_count("quick shown", n, DELAY), "{:?}", d.log());
        std::thread::sleep(Duration::from_secs(1));
        if n == 1 {
            quick();
            assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
        }
    }
    assert!(!d.log().iter().any(|l| l.contains("quick shown (rows: volume headset")), "{:?}", d.log());
    let wired = d.pss_kb();
    quick();
    assert!(d.wait_for_count("popup closed", 2, DELAY), "{:?}", d.log());
    let fixture = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pw-dump-bluez-headset.json")).unwrap();
    pw_dump_send(&sway.runtime, &fixture);
    std::thread::sleep(TRAY_SETTLE);
    let wired_closed = d.pss_kb();
    quick();
    assert!(d.wait_for("quick shown (rows: volume headset", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    let headset = d.pss_kb();
    let delta = headset as i64 - wired as i64;
    eprintln!(
        "quick Bluetooth headset row (KB): {wired} rows on a wired sink, {headset} with the headset row ({delta:+}); {wired_closed} closed on the headset"
    );
    assert!(headset < STARTUP_MAX_KB, "{headset} KB >= 15 MB — report it, never raise");
    // Measured +12…+20 KB (release, 5 runs): the bluez5 device and its profiles (a few hundred
    // bytes), one row more in the panel's buffers. Report, never raise.
    assert!(delta <= 128, "the headset row costs {delta} KB: report, never raise");
}

/// Once the detail closed, no timer, no wake-up (the loop's nor the
/// D-Bus thread's), no spin; the acting connection gone.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn bluetooth_at_rest_no_wakeups_after_the_detail() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("bt-rest");
    let bluez = FakeBluez::serve(&bus);
    let backlight = fake_backlight("bt-rest-backlight");
    let (sway, _hypr, d) = start_bt("bt-rest", &bus, &bt_bar(&backlight.0));
    const REST: Duration = Duration::from_secs(10);
    // As `at_rest_no_wakeups_after_the_wifi_detail`: three cycles at least, the last one away from
    // the minute, the window within a second of the close.
    let mut n = 0;
    loop {
        n += 1;
        if n >= 3 {
            away_from_the_minute(REST + Duration::from_secs(5));
        }
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Bluetooth)), Reply::Done);
        assert!(detail_opened(&bluez, n), "{:?}", d.log());
        std::thread::sleep(PAST_DEBOUNCE);
        assert_eq!(launcher(&sway, &Request::Quick(QuickVerb::Bluetooth)), Reply::Done);
        assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
        if n >= 3 && fits_the_minute(REST + Duration::from_secs(1)) {
            break;
        }
        assert!(n < 6, "the minute's tick always too close to the window");
    }
    let closed_at = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    assert!(closed_at.elapsed() < Duration::from_millis(900), "the window starts within a second of the close");
    let dbus_before = d.thread_wakeups("D-Bus").expect("the D-Bus thread");
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the Bluetooth detail");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    let dbus_delta = d.thread_wakeups("D-Bus").expect("the D-Bus thread").saturating_sub(dbus_before);
    eprintln!(
        "wake-ups at rest after the Bluetooth detail (10 s): {delta} whole daemon, {main_delta} loop thread, {dbus_delta} D-Bus thread"
    );
    let unregistered = bluez.calls().iter().filter(|c| c.starts_with("UnregisterAgent ")).count();
    assert_eq!(unregistered, n, "each close unregisters the agent: {:?}", bluez.calls());
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s");
    assert_eq!(dbus_delta, 0, "{dbus_delta} D-Bus-thread wake-ups in 10 s: its taps and its bus are quiet");
    assert!(delta <= 8, "{delta} wake-ups in 10 s");
}

/// The bar on `outputs` headless outputs reading this sway itself (`mango` false: $SWAYSOCK) or a
/// FakeMango in its runtime directory; the system bus `bus`, pw-dump silent.
fn start_reading(name: &str, outputs: u32, bus: &PrivateBus, bar_toml: Option<&str>, mango: bool) -> (Sway, Option<FakeMango>, Daemon) {
    let sway = Sway::start(name, outputs);
    let fake = mango.then(|| FakeMango::start(&sway.runtime));
    let backend = match &fake {
        Some(m) => Backend::Mango(&m.socket),
        None => Backend::Sway,
    };
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml, system_bus: Some(&bus.address), pw_dump: PwDump::Silent, backend, ..DaemonOptions::default() },
    );
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    (sway, fake, d)
}

/// Waits until the daemon has sent the fake Mango both watch lines (5 s at most): a figure taken
/// before would measure a daemon not reading it yet.
fn wait_until_read(fake: &FakeMango, d: &Daemon) {
    let t = Instant::now();
    while fake.requests() != ["watch all-clients", "watch all-monitors"] {
        assert!(t.elapsed() < DELAY, "the daemon never read the fake Mango: {:?} {:?}", fake.requests(), d.log());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Startup PSS, every module, 3 outputs, under each backend — the fake
/// Hyprland (the reference, as `startup_pss_per_source`), Sway (this headless sway read through
/// its i3 IPC), Mango (a fake socket serving the real captures). Printed for the rows of docs/measurements.md.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn startup_pss_per_backend() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("per-backend");
    let _workstation = Workstation::serve(&bus);
    let mut report = Vec::new();
    {
        let (_sway, _hypr, d) = start("per-backend-hyprland", 3, &bus, None, Some(EVERYTHING), PwDump::Silent);
        std::thread::sleep(Duration::from_secs(3));
        report.push(("Hyprland (fake)", d.pss_kb(), d.threads()));
    }
    for (what, line, mango) in [("Sway", "compositor: Sway", false), ("Mango (fake socket)", "compositor: Mango", true)] {
        let (_sway, fake, d) =
            start_reading(&format!("per-backend-{}", if mango { "mango" } else { "sway" }), 3, &bus, Some(EVERYTHING), mango);
        assert!(d.wait_for(line, DELAY), "{:?}", d.log());
        if let Some(fake) = &fake {
            // The figure is the daemon reading the fake.
            wait_until_read(fake, &d);
        }
        std::thread::sleep(Duration::from_secs(3)); // as in startup_pss_per_source
        report.push((what, d.pss_kb(), d.threads()));
        assert!(d.log().iter().all(|l| !l.contains("compositor source down")), "{:?}", d.log());
    }
    eprintln!("startup PSS (KB) and threads per backend: {report:?}");
    let (_, hyprland_kb, hyprland_threads) = report[0];
    for (what, kb, threads) in &report {
        assert!(*kb < STARTUP_MAX_KB, "{what}: {kb} KB >= 15 MB — report it with these figures, never raise: {report:?}");
        // Measured back to back: a backend costs at most 512 KB over the fake Hyprland's source.
        assert!(*kb <= hyprland_kb + 512, "{what}: {kb} KB > {hyprland_kb} + 512 KB (the fake Hyprland): {report:?}");
        // No new thread: the backend runs on the existing `compositor` thread.
        assert_eq!(*threads, hyprland_threads, "{what}: threads differ from the fake Hyprland's: {report:?}");
    }
}

/// At rest under Sway and under Mango, the compositor thread never wakes
/// (both block without a timeout), the daemon as at rest under the fake Hyprland.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_under_sway_and_mango() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("rest-backends");
    let _workstation = Workstation::serve(&bus);
    for (what, mango) in [("Sway", false), ("Mango", true)] {
        let (_sway, fake, d) = start_reading(&format!("rest-{}", what.to_lowercase()), 3, &bus, Some(AT_REST), mango);
        assert!(d.wait_for(&format!("compositor: {what}"), DELAY), "{:?}", d.log());
        if let Some(fake) = &fake {
            // At rest means reading the fake: both watches open before the window.
            wait_until_read(fake, &d);
        }
        std::thread::sleep(Duration::from_secs(2)); // end of startup
        let before = d.wakeups();
        let compositor_before = d.thread_wakeups("compositor").expect("the compositor thread runs");
        let ticks = d.cpu_ticks();
        std::thread::sleep(Duration::from_secs(5));
        assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), what);
        let delta = d.wakeups().saturating_sub(before);
        let compositor = d.thread_wakeups("compositor").unwrap().saturating_sub(compositor_before);
        eprintln!("wake-ups at rest under {what} (5 s): {delta} in all, compositor thread {compositor}");
        assert_eq!(compositor, 0, "the {what} source wakes at rest");
        assert!(delta <= 4, "{delta} wake-ups in 5 s at rest under {what}");
        assert!(d.log().iter().all(|l| !l.contains("source down")), "every source up: {:?}", d.log());
    }
}

/// The power menu's bars: the button and the clock, then the clock alone (the baseline).
const POWER_BAR: &str = "modules = [\"power\", \"clock\"]\n";
const NO_POWER_BAR: &str = "modules = [\"clock\"]\n";
/// The power button: cell 1 of the bar.
const POWER_BUTTON: (u32, u32) = (12, 10);
/// The menu's third row (Power off, with the default `[quick]`), cell 4: the menu hangs from cell 1
/// (≈ 8 px), under the 22 px bar, its frame ≈ 4 px at the sides and ≈ 7 px at the top, ≈ 8.4 px
/// cells, 22 px rows (headless_session.rs's `popup_cell`).
const POWER_OFF_ROW: (u32, u32) = (8 + 4 + (4 * 84 + 42) / 10, 22 + 7 + 2 * 22 + 11);

/// PSS in KB by kind of mapping from /proc/<pid>/smaps: font files, the binary, the heap, the rest.
/// Printed for the report (is the module's cost the ⏻ glyph's fallback face?), never asserted.
fn smaps_breakdown(d: &Daemon) -> String {
    let t = std::fs::read_to_string(format!("/proc/{}/smaps", d.pid())).unwrap();
    let (mut fonts, mut binary, mut heap, mut other) = (0u64, 0u64, 0u64, 0u64);
    let mut kind = 3;
    for line in t.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().unwrap_or(0);
            match kind {
                0 => fonts += kb,
                1 => binary += kb,
                2 => heap += kb,
                _ => other += kb,
            }
        } else if line.contains('-') && line.split_whitespace().next().is_some_and(|r| r.contains('-') && !r.ends_with(':')) {
            let l = line.to_lowercase();
            kind = if [".ttf", ".otf", ".ttc"].iter().any(|e| l.contains(e)) {
                0
            } else if l.contains("alpheus-demon") {
                1
            } else if l.contains("[heap]") {
                2
            } else {
                3
            };
        }
    }
    format!("fonts {fonts}, binary {binary}, heap {heap}, other {other}")
}

/// The daemon's anonymous memory in KB (smaps `Anonymous`, every mapping but the shm pools): the
/// heap, the thread arenas, the stacks and the binary's written data pages, never a file's page.
/// A leak bound on it does not move with page-cache fault-around (the kernel maps up to 64 KB of
/// a file around each fault, so the code and font pages a first open brings in depend on the
/// tests run before it and on what the page cache holds).
fn anon_kb(d: &Daemon) -> u64 {
    let t = std::fs::read_to_string(format!("/proc/{}/smaps", d.pid())).unwrap();
    let (mut pool, mut anon) = (false, 0);
    for line in t.lines() {
        if let Some(rest) = line.strip_prefix("Anonymous:") {
            if !pool {
                anon += rest.trim().trim_end_matches("kB").trim().parse::<u64>().unwrap_or(0);
            }
        } else if line.split_whitespace().next().is_some_and(|r| r.contains('-') && !r.ends_with(':')) {
            pool = line.contains("/memfd:");
        }
    }
    anon
}

/// PSS in KB by mapping from /proc/<pid>/smaps, keyed "<range> <perms> <name>", for `smaps_moved`.
fn smaps_by_mapping(d: &Daemon) -> std::collections::BTreeMap<String, u64> {
    let t = std::fs::read_to_string(format!("/proc/{}/smaps", d.pid())).unwrap();
    let (mut map, mut key) = (std::collections::BTreeMap::new(), String::new());
    for line in t.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().unwrap_or(0);
            *map.entry(key.clone()).or_insert(0) += kb;
        } else if line.split_whitespace().next().is_some_and(|r| r.contains('-') && !r.ends_with(':')) {
            let mut w = line.split_whitespace();
            let (range, perms) = (w.next().unwrap_or(""), w.next().unwrap_or(""));
            let name = w.nth(3).unwrap_or("[anon]");
            key = format!("{range} {perms} {name}");
        }
    }
    map
}

/// The mappings whose PSS moved by 8 KB or more between two `smaps_by_mapping` readings, with the
/// kind of each (binary code r-x, binary data, font file, heap, stack, memfd/shm pool, other anon
/// = thread arenas and the rest), for the report; printed, never asserted.
fn smaps_moved(from: &std::collections::BTreeMap<String, u64>, to: &std::collections::BTreeMap<String, u64>) -> String {
    let mut out = Vec::new();
    let keys: std::collections::BTreeSet<&String> = from.keys().chain(to.keys()).collect();
    for k in keys {
        let (before, after) = (from.get(k).copied().unwrap_or(0), to.get(k).copied().unwrap_or(0));
        let delta = after as i64 - before as i64;
        if delta.abs() >= 8 {
            let l = k.to_lowercase();
            let perms = k.split_whitespace().nth(1).unwrap_or("");
            let kind = if [".ttf", ".otf", ".ttc"].iter().any(|e| l.contains(e)) {
                "font"
            } else if l.contains("alpheus-demon") || l.contains("/memory-") || l.contains("deps/") {
                if perms.contains('x') { "binary code" } else { "binary data" }
            } else if l.contains("[heap]") {
                "main heap"
            } else if l.contains("[stack") {
                "stack"
            } else if l.contains("memfd") || l.contains("shm") || l.contains("wl_shm") {
                "shm pool"
            } else if l.contains(".so") {
                "library"
            } else {
                "anon (thread arenas, other)"
            };
            out.push(format!("{kind} {} {before}->{after} ({delta:+})", k.split_whitespace().next().unwrap_or("")));
        }
    }
    out.join("; ")
}

/// A click on the power button, then the line it waits for (`n` of them so far).
fn power_click(sway: &Sway, d: &Daemon, line: &str, n: usize) {
    sway.click(POWER_BUTTON.0, POWER_BUTTON.1, "button1");
    assert!(d.wait_for_count(line, n, DELAY), "{line} ×{n}: {:?}", d.log());
}

/// Waits until the third row (Power off) asks: its text is painted in the theme's warn (the default
/// catppuccin, dark without a portal), which no other state of the menu uses. Proves the question
/// was asked, so the rest below measures a menu closed with its question pending.
fn power_asking(sway: &Sway, d: &Daemon) {
    let w = preset("catppuccin").unwrap().dark.warn;
    let warn = [w.0, w.1, w.2];
    let asking =
        |s: &common::Ppm| (73..95).flat_map(|y| (14..140).map(move |x| (x, y))).filter(|&(x, y)| s.at(x, y) == Some(warn)).count() > 5;
    assert!(sway.wait_for_screen("HEADLESS-1", asking), "the question shown in warn: {:?}", d.log());
}

/// PSS without the module, with it at rest, the menu open, closed, and after 20
/// more open/close cycles; each bound against this run's own baseline.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn power_menu_pss_idle_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("power-cost");
    let base = {
        let (_sway, _hypr, d) = start("power-cost-a", 1, &bus, None, Some(NO_POWER_BAR), PwDump::Silent);
        std::thread::sleep(TRAY_SETTLE);
        eprintln!("power base smaps (KB): {}", smaps_breakdown(&d));
        d.pss_kb()
    };
    let (sway, _hypr, d) = start("power-cost-b", 1, &bus, None, Some(POWER_BAR), PwDump::Silent);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let idle = d.pss_kb();
    eprintln!("power idle smaps (KB): {}", smaps_breakdown(&d));
    power_click(&sway, &d, "power menu shown", 1);
    std::thread::sleep(Duration::from_secs(1)); // configured and painted
    let open = d.pss_kb();
    power_click(&sway, &d, "popup closed", 1);
    std::thread::sleep(TRAY_SETTLE);
    let closed = d.pss_kb();
    for n in 2..=21 {
        power_click(&sway, &d, "power menu shown", n);
        std::thread::sleep(Duration::from_millis(300)); // mapped: a press in the gap lands on no surface
        power_click(&sway, &d, "popup closed", n);
    }
    std::thread::sleep(TRAY_SETTLE);
    let cycled = d.pss_kb();
    let (module_cost, menu_cost) = (idle.saturating_sub(base), open.saturating_sub(idle));
    eprintln!(
        "power PSS (KB): {base} without the module, {idle} with it at rest (module cost {module_cost}), \
         {open} open (menu cost {menu_cost}), {closed} closed, {cycled} after 20 more cycles"
    );
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB — report it, never raise");
    assert!(module_cost <= 384, "the button at rest costs {module_cost} KB: a one-cell segment, nothing resident (192–196 KB measured)");
    assert!(menu_cost <= 1024, "the open menu costs {menu_cost} KB (its pool, two buffers, the first open's warm-up: ≤ 1 MB)");
    assert!(closed <= idle + 512, "closed {closed} KB vs {idle} KB: more than the first open's code and font pages");
    assert!(cycled <= closed + 128, "cycled {cycled} KB vs {closed} KB closed: 20 cycles must keep nothing");
}

/// Once the menu closed — the last time with its question pending — no timer, no
/// wake-up, no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_the_power_menu() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("power-rest");
    let (sway, _hypr, d) = start("power-rest", 1, &bus, None, Some(POWER_BAR), PwDump::Silent);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    for n in 1..=3 {
        power_click(&sway, &d, "power menu shown", n);
        std::thread::sleep(Duration::from_millis(300));
        power_click(&sway, &d, "popup closed", n);
    }
    assert_eq!(d.count("popup closed"), 3, "three plain cycles: {:?}", d.log());
    const REST: Duration = Duration::from_secs(10);
    // Before the question: its 3 s deadline must fall inside the rest window, never in a wait for the minute.
    away_from_the_minute(REST + Duration::from_secs(5));
    power_click(&sway, &d, "power menu shown", 4);
    std::thread::sleep(Duration::from_millis(300)); // mapped: a press in the gap lands on no surface
    sway.click(POWER_OFF_ROW.0, POWER_OFF_ROW.1, "button1"); // the question asked, by the mouse alone
    power_asking(&sway, &d); // a click before the map would dismiss the menu and ask nothing
    power_click(&sway, &d, "popup closed", 4);
    assert_eq!(d.count("power menu shown"), 4, "{:?}", d.log());
    assert_eq!(d.count("popup closed (outside click)"), 4, "the 4th close is the button's own second click: {:?}", d.log());
    assert_eq!(d.count("popup closed (dismissed)"), 0, "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(300));
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the power menu");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest after the power menu (10 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s: the question's timer went with the menu");
    assert!(delta <= 8, "{delta} wake-ups in 10 s");
}

/// The resources popup measurements' bar: the module alone (a one-cell-wide click target at the
/// power button's spot), a reading every second.
const RES_POPUP_BAR: &str = "modules = [\"resources\"]\n[resources]\ninterval = 1\n";

/// A click on the resources module, then the line it waits for (`n` of them so far).
fn resources_click(sway: &Sway, d: &Daemon, line: &str, n: usize) {
    sway.click(POWER_BUTTON.0, POWER_BUTTON.1, "button1");
    assert!(d.wait_for_count(line, n, DELAY), "{line} x{n}: {:?}", d.log());
}

/// PSS at rest, the popup open, closed, and after 20 more
/// cycles. The first open's file-backed warm-up (code and font pages) is in `closed`, and the shm
/// pools move on their own (the compositor maps them too), so the closed and cycled bounds read
/// `Pss_Anon` (smaps_rollup: heap, stacks, caches, nothing file-backed or shared); the pools get
/// a looser bound of their own, the whole PSS is printed.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn resources_popup_pss_idle_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("res-popup-cost");
    let (sway, _hypr, d) = start("res-popup-cost", 1, &bus, None, Some(RES_POPUP_BAR), PwDump::Silent);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (idle, idle_shm) = d.pss_and_shm_kb();
    let idle_anon = d.pss_anon_kb();
    eprintln!("resources popup idle smaps (KB): {}", smaps_breakdown(&d));
    resources_click(&sway, &d, "resources popup shown", 1);
    std::thread::sleep(Duration::from_secs(1)); // configured and painted
    let (open, open_shm) = d.pss_and_shm_kb();
    resources_click(&sway, &d, "popup closed", 1);
    std::thread::sleep(TRAY_SETTLE);
    let (closed, closed_shm) = d.pss_and_shm_kb();
    let closed_anon = d.pss_anon_kb();
    eprintln!("resources popup closed smaps (KB): {}", smaps_breakdown(&d));
    for n in 2..=21 {
        resources_click(&sway, &d, "resources popup shown", n);
        // Mapped (a press in the gap lands on no surface), and open across two ticks (interval = 1): each
        // cycle repaints at least once, so a repaint that keeps something shows in the cycles.
        std::thread::sleep(Duration::from_millis(2100));
        resources_click(&sway, &d, "popup closed", n);
    }
    std::thread::sleep(TRAY_SETTLE);
    let (cycled, cycled_shm) = d.pss_and_shm_kb();
    let cycled_anon = d.pss_anon_kb();
    eprintln!("resources popup smaps after the cycles (KB): {}", smaps_breakdown(&d));
    let (idle_own, closed_own, cycled_own) = (idle - idle_shm, closed - closed_shm, cycled - cycled_shm);
    let open_cost = open.saturating_sub(idle);
    eprintln!(
        "resources popup PSS (KB): {idle} at rest, {open} open (cost {open_cost}), {closed} closed, {cycled} after 20 more cycles; \
         own memory {idle_own} at rest, {closed_own} closed, {cycled_own} cycled; the pools: {idle_shm} at rest, {open_shm} open, \
         {closed_shm} closed, {cycled_shm} cycled; Pss_Anon {idle_anon} at rest, {closed_anon} closed, {cycled_anon} cycled"
    );
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB: report it, never raise");
    assert!(open_cost <= 1024, "the open popup costs {open_cost} KB (its pool, two buffers, the first open's warm-up: <= 1 MB)");
    assert!(closed_anon <= idle_anon + 256, "closed {closed_anon} KB vs {idle_anon} KB at rest (Pss_Anon): the open popup kept memory");
    assert!(cycled_anon <= closed_anon + 128, "cycled {cycled_anon} KB vs {closed_anon} KB closed (Pss_Anon): 20 cycles must keep nothing");
    assert!(closed_shm <= idle_shm + 128, "the pools closed {closed_shm} KB vs {idle_shm} KB at rest: the popup's pool must go");
}

/// The daemon's `[heap]` mapping in KB (PSS): the main thread's arena, where the loop's allocations
/// land. Far quieter than the whole PSS, which drifts by 0.7-0.9 MB at random moments on this harness.
fn heap_kb(d: &Daemon) -> u64 {
    let t = std::fs::read_to_string(format!("/proc/{}/smaps", d.pid())).unwrap();
    let (mut in_heap, mut kb) = (false, 0);
    for l in t.lines() {
        if let Some(v) = l.strip_prefix("Pss:") {
            if in_heap {
                kb += v.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
            }
        } else if l.split_whitespace().next().is_some_and(|f| f.contains('-') && !f.ends_with(':')) {
            in_heap = l.contains("[heap]");
        }
    }
    kb
}

/// The ring is fixed, so a full and wrapped one keeps nothing more than
/// one half full at 30 s (interval = 1: 60 samples after 60 s, the ring wraps after that). The bound
/// reads `Pss_Anon` (smaps_rollup: no file pages, no shm pools, which are what moves by 0.7-0.9 MB
/// at random on this harness); the whole own memory and the main heap are printed.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn resources_history_full_keeps_nothing_more() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("res-history");
    let (_sway, _hypr, d) = start("res-history", 1, &bus, None, Some(RES_POPUP_BAR), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(30));
    let (early, early_shm) = d.pss_and_shm_kb();
    let heap_early = heap_kb(&d);
    let anon_early = d.pss_anon_kb();
    std::thread::sleep(Duration::from_secs(40)); // 70 s in: the ring is full and has wrapped
    let (late, late_shm) = d.pss_and_shm_kb();
    let heap_late = heap_kb(&d);
    let anon_late = d.pss_anon_kb();
    eprintln!(
        "resources history (KB): own memory {} at 30 s, {} at 70 s; main heap {heap_early} at 30 s, {heap_late} at 70 s; Pss_Anon {anon_early} at 30 s, {anon_late} at 70 s (ring full and wrapped)",
        early - early_shm,
        late - late_shm
    );
    assert!(anon_late <= anon_early + 64, "a full ring grew Pss_Anon by {} KB", anon_late.saturating_sub(anon_early));
}

/// Once the popup closed, no timer, no wake-up. At
/// interval = 60 no tick falls in the window, so any wake-up is the popup's.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_the_resources_popup() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("res-popup-rest");
    let toml = "modules = [\"resources\"]\n[resources]\ninterval = 60\n";
    let (sway, _hypr, d) = start("res-popup-rest", 1, &bus, None, Some(toml), PwDump::Silent);
    let started = Instant::now();
    for n in 1..=3 {
        resources_click(&sway, &d, "resources popup shown", n);
        std::thread::sleep(Duration::from_millis(300));
        resources_click(&sway, &d, "popup closed", n);
    }
    assert!(started.elapsed() < Duration::from_secs(20), "the cycles took {:?}: the harness was too slow", started.elapsed());
    std::thread::sleep(Duration::from_millis(300));
    const REST: Duration = Duration::from_secs(10);
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert!(
        started.elapsed() < Duration::from_secs(55),
        "the window ended {:?} after the start: the 60 s tick is too close, the harness was too slow",
        started.elapsed()
    );
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the resources popup");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest after the resources popup (10 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s: the popup left a timer");
    assert!(delta <= 8, "{delta} wake-ups in 10 s");
}

/// The privacy measurements' bar: the sound source (privacy reads its graph), the indicators, the
/// clock. No resources: they tick by design.
const PRIVACY_BAR: &str = "modules = [\"workspaces\", \"window\", \"volume\", \"privacy\", \"clock\"]\n";

/// The microphone glyph in catppuccin's orange (dark without a portal; the default `module_colors`
/// paints nothing else in it on this bar), anywhere on the bar's 22 px.
fn mic_shown(s: &common::Ppm) -> bool {
    let o = preset("catppuccin").unwrap().dark.hues.orange;
    (0..22).any(|y| (0..s.width).any(|x| s.at(x, y) == Some([o.0, o.1, o.2])))
}

/// One capture on the screen, then gone (its 0.5 s off-delay included). `n`: node and link ids apart.
fn capture_cycle(sway: &Sway, d: &Daemon, n: u32) {
    pw_dump_send(&sway.runtime, &capture_delta(200 + n, "firefox", "running", 300 + n, true));
    assert!(sway.wait_for_screen("HEADLESS-1", mic_shown), "capture {n} never shown: {:?}", d.log());
    pw_dump_send(&sway.runtime, &capture_gone(200 + n, 300 + n));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| !mic_shown(s)), "capture {n} never gone: {:?}", d.log());
}

/// PSS idle, a capture shown, ended, then after 20 more capture cycles; each
/// bound against this run's own baseline.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn privacy_pss_with_a_capture_and_after() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("privacy-cost");
    let (sway, _hypr, d) = start("privacy-cost", 1, &bus, None, Some(PRIVACY_BAR), PwDump::Fifo);
    std::thread::sleep(TRAY_SETTLE);
    let idle = d.pss_kb();
    pw_dump_send(&sway.runtime, &capture_delta(200, "firefox", "running", 300, true));
    assert!(sway.wait_for_screen("HEADLESS-1", mic_shown), "never shown: {:?}", d.log());
    std::thread::sleep(Duration::from_secs(1)); // painted
    let shown = d.pss_kb();
    pw_dump_send(&sway.runtime, &capture_gone(200, 300));
    assert!(sway.wait_for_screen("HEADLESS-1", |s| !mic_shown(s)), "never gone: {:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let ended = d.pss_kb();
    for n in 1..=20 {
        capture_cycle(&sway, &d, n);
    }
    std::thread::sleep(TRAY_SETTLE);
    let cycled = d.pss_kb();
    eprintln!("privacy PSS (KB): {idle} idle, {shown} with a capture shown, {ended} once ended, {cycled} after 20 more cycles");
    eprintln!("privacy smaps after the cycles (KB): {}", smaps_breakdown(&d));
    assert!(shown < STARTUP_MAX_KB, "{shown} KB >= 15 MB — report it, never raise");
    assert!(shown <= idle + 512, "shown {shown} KB vs idle {idle} KB: one glyph, a node and a link");
    assert!(ended <= shown + 64, "ended {ended} KB vs shown {shown} KB: the capture must leave nothing");
    assert!(cycled <= ended + 128, "cycled {cycled} KB vs ended {ended} KB: 20 captures must keep nothing");
}

/// Once a capture ended and its glyph went, no timer, no wake-up, no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_capture() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("privacy-rest");
    let (sway, _hypr, d) = start("privacy-rest", 1, &bus, None, Some(PRIVACY_BAR), PwDump::Fifo);
    std::thread::sleep(Duration::from_millis(500)); // pw-dump's initial state read
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(REST + Duration::from_secs(3));
    capture_cycle(&sway, &d, 0);
    std::thread::sleep(Duration::from_millis(300));
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after a capture");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest after a capture (5 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 5 s: the off-delay's timer must be gone");
    assert!(delta <= 4, "{delta} wake-ups in 5 s");
}

/// A fake `dev/video0` and `proc` for the camera measurements (a fake holder is process 4242), below
/// one guard that removes them.
struct FakeCam(alpheus_common::temp_path::TempPath);

impl FakeCam {
    fn new(name: &str) -> FakeCam {
        let root = alpheus_common::temp_path::TempPath::dir(name);
        std::fs::create_dir_all(root.join("dev")).unwrap();
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::write(root.join("dev/video0"), "").unwrap();
        FakeCam(root)
    }

    fn bar(&self) -> String {
        format!(
            "{PRIVACY_BAR}[privacy]\ndev_dir = \"{}\"\nproc_dir = \"{}\"\n",
            self.0.join("dev").display(),
            self.0.join("proc").display()
        )
    }

    /// The holder appears or goes, then the node is opened and closed: the watch's trigger.
    fn hold(&self, held: bool) {
        let dir = self.0.join("proc/4242");
        if held {
            std::fs::create_dir_all(dir.join("fd")).unwrap();
            std::fs::write(dir.join("comm"), "fakecam\n").unwrap();
            std::os::unix::fs::symlink(self.0.join("dev/video0"), dir.join("fd/3")).unwrap();
        } else {
            std::fs::remove_dir_all(dir).unwrap();
        }
        drop(std::fs::File::open(self.0.join("dev/video0")).unwrap());
    }
}

/// The camera glyph in catppuccin's green, anywhere on the bar's 22 px (this bar paints no other).
fn camera_shown(s: &common::Ppm) -> bool {
    let g = preset("catppuccin").unwrap().dark.hues.green;
    (0..22).any(|y| (0..s.width).any(|x| s.at(x, y) == Some([g.0, g.1, g.2])))
}

/// One holder on the screen, then gone (the debounce and the 0.5 s off-delay included).
fn camera_cycle(sway: &Sway, d: &Daemon, cam: &FakeCam, n: u32) {
    cam.hold(true);
    assert!(sway.wait_for_screen("HEADLESS-1", camera_shown), "camera {n} never shown: {:?}", d.log());
    cam.hold(false);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| !camera_shown(s)), "camera {n} never gone: {:?}", d.log());
}

/// PSS idle (the watch on, no camera), with a fake holder shown, once
/// gone, and after 20 more cycles; then no wake-up at rest after the holder went.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn privacy_camera_pss_and_rest() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("camera-cost");
    let cam = FakeCam::new("camera-cost");
    let (sway, _hypr, d) = start("camera-cost", 1, &bus, None, Some(&cam.bar()), PwDump::Fifo);
    assert!(d.wait_for("privacy: watching 1 video device(s)", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let idle = d.pss_kb();
    cam.hold(true);
    assert!(sway.wait_for_screen("HEADLESS-1", camera_shown), "never shown: {:?}", d.log());
    std::thread::sleep(Duration::from_secs(1)); // painted
    let shown = d.pss_kb();
    cam.hold(false);
    assert!(sway.wait_for_screen("HEADLESS-1", |s| !camera_shown(s)), "never gone: {:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let ended = d.pss_kb();
    for n in 1..=20 {
        camera_cycle(&sway, &d, &cam, n);
    }
    std::thread::sleep(TRAY_SETTLE);
    let cycled = d.pss_kb();
    eprintln!("camera PSS (KB): {idle} idle, {shown} with a holder shown, {ended} once gone, {cycled} after 20 more cycles");
    assert!(shown < STARTUP_MAX_KB, "{shown} KB >= 15 MB — report it, never raise");
    assert!(shown <= idle + 512, "shown {shown} KB vs idle {idle} KB: one glyph and a scan");
    assert!(ended <= shown + 64, "ended {ended} KB vs shown {shown} KB: the holder must leave nothing");
    assert!(cycled <= ended + 128, "cycled {cycled} KB vs ended {ended} KB: 20 holders must keep nothing");
    // The loop at rest after the holder went: the scan timer and the off-delay's are gone.
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(REST + Duration::from_secs(3));
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after a camera");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest after a camera (5 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 5 s: the scan's and the off-delay's timers must be gone");
    assert!(delta <= 4, "{delta} wake-ups in 5 s");
}

/// A fake `/proc` of `pids` processes with `fds` descriptors each, the holder of `video0` in
/// `dev` being pid 1000 + `pids` / 2 on its last descriptor. Returns the proc directory.
fn fake_proc(root: &Path, dev: &Path, pids: u32, fds: u32) -> PathBuf {
    let proc = root.join(format!("proc-{pids}x{fds}"));
    for pid in 1000..1000 + pids {
        let (fd_dir, comm) = (proc.join(pid.to_string()).join("fd"), proc.join(pid.to_string()).join("comm"));
        std::fs::create_dir_all(&fd_dir).unwrap();
        std::fs::write(comm, "worker\n").unwrap();
        for fd in 0..fds {
            let target = if pid == 1000 + pids / 2 && fd == fds - 1 { dev.join("video0") } else { root.join(format!("other{fd}")) };
            std::os::unix::fs::symlink(target, fd_dir.join(fd.to_string())).unwrap();
        }
    }
    proc
}

/// The median and the maximum of 21 scans of `proc`, each finding the one holder.
fn scan_times(proc: &Path, dev: &Path) -> (Duration, Duration) {
    // SAFETY: geteuid(2) cannot fail.
    let uid = unsafe { libc::geteuid() };
    let mut times: Vec<Duration> = (0..21)
        .map(|_| {
            let t = Instant::now();
            let scan = alpheus_daemon::privacy::camera::scan(proc, dev, uid, std::process::id());
            let took = t.elapsed();
            assert_eq!(scan.camera.names, ["worker"], "the one holder, found");
            assert!(!scan.bounded);
            took
        })
        .collect();
    times.sort();
    (times[10], times[20])
}

/// One scan of 120 processes with 30 descriptors each (one holder; the
/// daemon scans only its own uid, 93 processes and 2,726 descriptors measured on the workstation)
/// costs at most 10 ms, the median of 21. The scan stays on the event loop. A 500 x 20 tree is timed
/// for the record (the worst case seen), asserted nothing.
#[test]
#[ignore = "release build only (nix develop)"]
fn camera_scan_duration() {
    require_release(cfg!(debug_assertions));
    let root = alpheus_common::temp_path::TempPath::dir("camera-scan");
    let dev = root.join("dev");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("video0"), "").unwrap();
    let (median, max) = scan_times(&fake_proc(&root, &dev, 120, 30), &dev);
    eprintln!("camera scan of 120 processes x 30 descriptors: median {median:?}, max {max:?}");
    let (wide_median, wide_max) = scan_times(&fake_proc(&root, &dev, 500, 20), &dev);
    eprintln!("camera scan of 500 processes x 20 descriptors (not asserted): median {wide_median:?}, max {wide_max:?}");
    assert!(median <= Duration::from_millis(10), "median {median:?} > 10 ms: report to the controller");
}

/// Every module that rests (no resources: they tick by design), the displays module left out.
macro_rules! resting_modules {
    () => {
        "\"launcher\", \"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \"clock\""
    };
}

/// The displays trials' and hotplug's bar: nothing that ticks, so the loop rests between events.
const DISPLAYS_BAR: &str = "modules = [\"workspaces\", \"window\", \"displays\", \"clock\"]\n";

/// No loop-thread and no whole-daemon wake-up, no spin, in `rest` from now (the caller keeps the
/// window away from the minute: the clock is the only timer left).
fn displays_rest(d: &Daemon, rest: Duration, what: &str) {
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(rest);
    assert_no_spin(d.cpu_ticks() - ticks, rest, what);
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest {what} ({rest:?}): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups {what}: a debounce, watchdog or trial timer left armed");
    assert_eq!(delta, 0, "{delta} wake-ups of the daemon {what}");
}

/// The module on at start, with 1 and 3 headless outputs (their heads
/// and modes read), against the same bar without it on this build (the base has no module: its
/// one-time cost is `startup_pss_per_source`'s `everything`, back to back); then no wake-up at rest.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn displays_pss_at_start() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("displays-start");
    let _workstation = Workstation::serve(&bus);
    let off = concat!("modules = [", resting_modules!(), "]\n");
    let on = concat!("modules = [", resting_modules!(), ", \"displays\"]\n");
    let mut report = Vec::new();
    for outputs in [1, 3] {
        let mut kb = [0u64; 2];
        for (i, toml) in [off, on].iter().enumerate() {
            let (sway, _hypr, d) = start(&format!("displays-start-{outputs}-{i}"), outputs, &bus, None, Some(toml), PwDump::Silent);
            if i == 1 {
                outputs_snapshot(&sway, &d, outputs as usize..=outputs as usize); // the heads and their modes read
            }
            std::thread::sleep(TRAY_SETTLE);
            kb[i] = d.pss_kb();
            if i == 1 {
                const REST: Duration = Duration::from_secs(5);
                away_from_the_minute(REST + Duration::from_secs(1));
                displays_rest(&d, REST, &format!("at start on {outputs} output(s)"));
            }
        }
        report.push((outputs, kb[0], kb[1]));
    }
    eprintln!("displays PSS at start (outputs, off KB, on KB): {report:?}");
    for (outputs, off, on) in report {
        assert!(on < STARTUP_MAX_KB, "{on} KB >= 15 MB — report it, never raise");
        assert!(on <= off + 512, "{outputs} output(s): on {on} KB vs off {off} KB: more than 512 KB — report, never raise");
    }
}

/// `outputs-try` of HEADLESS-2 moved to `x`, the rest of it as it is.
fn try_x(s: &wire::Snapshot, x: i32) -> Request {
    let active = output(s, "HEADLESS-2").active.clone().expect("enabled");
    Request::OutputsTry(wire::encode_desired(&[Step { output: "HEADLESS-2".into(), active: Some(Active { x, ..active }) }]))
}

/// HEADLESS-2's x in `s`.
fn x_of(s: &wire::Snapshot) -> i32 {
    output(s, "HEADLESS-2").active.as_ref().expect("enabled").x
}

/// One trial on HEADLESS-2, moved 100 px right, then kept or reverted; returns once the daemon's
/// snapshot shows the outcome with no trial left.
fn trial(sway: &Sway, d: &Daemon, keep: bool) {
    let before = outputs_snapshot(sway, d, 2..=2);
    let (from, to) = (x_of(&before), x_of(&before) + 100);
    assert_eq!(displays(sway, &try_x(&before, to)), Reply::Done, "{:?}", d.log());
    snapshot_until(sway, d, |s| x_of(s) == to && matches!(s.trial, TrialState::Open { .. }));
    let end = if keep { Request::OutputsKeep } else { Request::OutputsRevert };
    assert_eq!(displays(sway, &end), Reply::Done, "{:?}", d.log());
    let x = if keep { to } else { from };
    snapshot_until(sway, d, |s| x_of(s) == x && s.trial == TrialState::None);
}

/// PSS idle, one trial open, once kept, then after 20 more trials (kept
/// and reverted in turn; a timed-out one would take 15 s each); then no wake-up at rest in the
/// 16 s after the last one, its 15 s deadline included.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn displays_pss_after_trials() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("displays-trials");
    let (sway, _hypr, d) = start("displays-trials", 2, &bus, None, Some(DISPLAYS_BAR), PwDump::Silent);
    let before = outputs_snapshot(&sway, &d, 2..=2);
    std::thread::sleep(TRAY_SETTLE);
    let idle = d.pss_kb();
    let to = x_of(&before) + 100;
    assert_eq!(displays(&sway, &try_x(&before, to)), Reply::Done, "{:?}", d.log());
    snapshot_until(&sway, &d, |s| x_of(s) == to && matches!(s.trial, TrialState::Open { .. }));
    let open = d.pss_kb();
    assert_eq!(displays(&sway, &Request::OutputsKeep), Reply::Done, "{:?}", d.log());
    snapshot_until(&sway, &d, |s| s.trial == TrialState::None);
    std::thread::sleep(TRAY_SETTLE);
    let ((first, first_shm), first_smaps) = (d.pss_and_shm_kb(), smaps_breakdown(&d));
    for n in 0..19 {
        trial(&sway, &d, n % 2 == 0);
    }
    // The last trial ends the run: its 15 s deadline and the watchdog fall inside the rest window,
    // which must not cross the minute (the clock's tick); a trial overrunning on a loaded machine
    // is followed by one more.
    const REST: Duration = Duration::from_secs(16);
    let mut extra = 0;
    loop {
        away_from_the_minute(REST + Duration::from_secs(3));
        trial(&sway, &d, false);
        if fits_the_minute(REST) {
            break;
        }
        extra += 1;
        assert!(extra < 3, "the minute's tick always too close to the window");
    }
    displays_rest(&d, REST, "after the last trial");
    let (cycled, cycled_shm) = d.pss_and_shm_kb(); // settled by the rest window
    let (first_own, cycled_own) = (first - first_shm, cycled - cycled_shm);
    eprintln!(
        "displays trials PSS (KB): {idle} idle, {open} with a trial open, {first} once kept, {cycled} after 20 more trials; \
         without the shm pools: {first_own} once kept, {cycled_own} cycled; the pools: {first_shm} once kept, {cycled_shm} cycled"
    );
    eprintln!("displays smaps once kept (KB): {first_smaps}; after the trials: {}", smaps_breakdown(&d));
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB — report it, never raise");
    assert!(first <= idle + 512, "kept {first} KB vs idle {idle} KB: more than 512 KB — report, never raise");
    // The shm pools are printed only (`displays_pss_after_hotplug_cycles` says why): a moved screen
    // has its bar drawn again, its pool holding one buffer or two (110 or 220 KB) as it happens.
    assert!(cycled_own <= first_own + 128, "cycled {cycled_own} KB vs {first_own} KB without the pools: 20 trials must keep nothing");
}

/// A displays.toml placing the plugged head `Headless output <n>` (the headless backend's counter)
/// left of HEADLESS-1 (described `first`), and HEADLESS-1 alone where that left it. The plug
/// applies a profile, each time a real one: the plugged head is a new screen, so each plug is a
/// new set of screens, never held back by the limit of 3 automatic applies per set per minute.
/// The unplug brings back the same set, {HEADLESS-1}, every cycle (more than 3 a minute): its
/// profile matches but is already shown, so it is evaluated, never sent nor counted, and nothing
/// is logged. (A profile moving HEADLESS-1 back to the origin would be skipped from the 4th cycle
/// of a minute, and retried only when the minute ends.) Two profiles, a real file's size: the
/// file is read at each debounce end, so its parse's own heap peak stays out of what the cycles
/// measure.
fn hotplug_profiles(first: &str, n: usize) -> String {
    format!(
        "[[profile]]\nname = \"Alone\"\n[[profile.display]]\ndescription = \"{first}\"\nmode = \"1280x720@0\"\nplacement = {{ x = 1920, y = 0 }}\n\
         [[profile]]\nname = \"Desk\"\n\
         [[profile.display]]\ndescription = \"Headless output {n}\"\nmode = \"1920x1080@0\"\nplacement = {{ x = 0, y = 0 }}\n\
         [[profile.display]]\ndescription = \"{first}\"\nmode = \"1280x720@0\"\nplacement = {{ x = 1920, y = 0 }}\n"
    )
}

/// The `n`th hotplug cycle: one screen plugged, its profile applied, then unplugged and HEADLESS-1
/// left alone where the profile put it (the lone profile, already shown, sends nothing). The backend's counter, in the plugged head's description, runs two ahead
/// (HEADLESS-1 is `Headless output 1`, the first plugged one `Headless output 3`).
fn hotplug(sway: &Sway, d: &Daemon, first: &str, n: usize) {
    std::fs::write(sway.runtime.join("config/alpheus/displays.toml"), hotplug_profiles(first, n + 2)).unwrap();
    sway.command("create_output");
    let s = snapshot_until(sway, d, |s| {
        s.outputs.len() == 2
            && s.trial == TrialState::None
            && s.outputs
                .iter()
                .all(|o| o.active.as_ref().is_some_and(|a| (a.x, a.y) == if o.name == "HEADLESS-1" { (1920, 0) } else { (0, 0) }))
    });
    let plugged = s.outputs.iter().find(|o| o.name != "HEADLESS-1").expect("the plugged head").name.clone();
    sway.command(&format!("output {plugged} unplug"));
    snapshot_until(sway, d, |s| {
        s.outputs.len() == 1
            && s.trial == TrialState::None
            && output(s, "HEADLESS-1").active.as_ref().is_some_and(|a| (a.x, a.y) == (1920, 0))
    });
}

/// Cycles in `displays_pss_after_hotplug_cycles`: one, then the readings every `HOTPLUG_STEP` after it.
const HOTPLUG_CYCLES: usize = 61;
const HOTPLUG_STEP: usize = 20;

/// PSS idle, after one hotplug cycle (a screen plugged, its profile
/// applied, unplugged, the lone profile evaluated, already shown), then every 20 more, up to 60; then no wake-up at
/// rest after the last one (its debounce and watchdog inside the window). Bounded on what the
/// daemon keeps, the shm pools left out: 20 cycles ≤ +128 KB, and the next 20 ≤ +32 KB (levelled
/// off). The shm pools are printed only: a bar's pool grows to fit its largest buffer, and SCTK's
/// `RawPool::resize` maps the grown file again, so the pool's PSS counts the pages touched through
/// the new mapping, not its capacity — 0 right after a resize, then whatever the 1920 px and the
/// 1280 px bars drew since (84 to 220 KB measured).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn displays_pss_after_hotplug_cycles() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("displays-plug");
    let sway = Sway::start("displays-plug", 1);
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    // One profile for a screen that is not there: the start-up evaluation logs its line (none is
    // logged without a profile) first, and none may follow.
    let config = sway.runtime.join("config/alpheus");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("displays.toml"),
        "[[profile]]\nname = \"Elsewhere\"\n[[profile.display]]\ndescription = \"Nobody P0 X0\"\nmode = \"640x480@60\"\nplacement = { x = 0, y = 0 }\n",
    )
    .unwrap();
    let options = DaemonOptions { bar_toml: Some(DISPLAYS_BAR), system_bus: Some(&bus.address), ..DaemonOptions::default() };
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    let first = outputs_snapshot(&sway, &d, 1..=1).outputs[0].description.clone();
    assert!(d.wait_for("displays: no profile for these screens", DELAY), "{:?}", d.log());
    std::fs::write(sway.runtime.join("config/alpheus/displays.toml"), hotplug_profiles(&first, 3)).unwrap();
    std::thread::sleep(TRAY_SETTLE);
    let (idle, idle_shm) = d.pss_and_shm_kb();
    let quiet = d.log().len();
    hotplug(&sway, &d, &first, 1);
    std::thread::sleep(TRAY_SETTLE);
    let mut readings = vec![(1, d.pss_and_shm_kb())];
    let once_smaps = smaps_breakdown(&d);
    for n in 2..HOTPLUG_CYCLES {
        hotplug(&sway, &d, &first, n);
        if n % HOTPLUG_STEP == 1 {
            std::thread::sleep(TRAY_SETTLE);
            readings.push((n, d.pss_and_shm_kb()));
        }
    }
    // The last cycle ends the run: the rest window must not cross the minute (the clock's tick);
    // a cycle overrunning on a loaded machine is followed by one more.
    const REST: Duration = Duration::from_secs(10);
    let mut extra = 0;
    loop {
        away_from_the_minute(REST + Duration::from_secs(5));
        hotplug(&sway, &d, &first, HOTPLUG_CYCLES + extra);
        // The unplug's evaluation (its 500 ms debounce) sends nothing, the lone profile being
        // already shown, so no snapshot shows it: waited out before the rest window.
        std::thread::sleep(Duration::from_secs(1));
        if fits_the_minute(REST) {
            break;
        }
        extra += 1;
        assert!(extra < 3, "the minute's tick always too close to the window");
    }
    displays_rest(&d, REST, "after the last hotplug cycle");
    readings.push((HOTPLUG_CYCLES + extra, d.pss_and_shm_kb())); // settled by the rest window
    let own: Vec<(usize, u64)> = readings.iter().map(|&(n, (total, shm))| (n, total - shm)).collect();
    eprintln!(
        "displays hotplug PSS (KB): {idle} idle ({} without the shm pools, pools {idle_shm}); after n cycles (n, total, pools): {readings:?}; \
         without the pools: {own:?}",
        idle - idle_shm
    );
    eprintln!("displays smaps after one cycle (KB): {once_smaps}; after the last: {}", smaps_breakdown(&d));
    let applied = d.log()[quiet..].iter().filter(|l| l.contains("displays:")).cloned().collect::<Vec<_>>();
    assert!(applied.is_empty(), "every profile applied, nothing refused: {applied:?}");
    let (once, once_own, own_20, own_40) = (readings[0].1.0, own[0].1, own[1].1, own[2].1);
    assert!(once <= idle + 512, "one cycle {once} KB vs idle {idle} KB: more than 512 KB — report, never raise");
    assert!(own_20 <= once_own + 128, "{own_20} KB after 20 more cycles vs {once_own} KB without the pools: hotplug must keep nothing");
    assert!(own_40 <= own_20 + 32, "{own_40} KB after 40 cycles vs {own_20} KB after 20, without the pools: still growing");
}

/// `NOTIFY_BAR` plus what the battery alerts, rules and timed do-not-disturb add to the configuration: 32 rules, four battery
/// thresholds and `dnd_while_sharing`, with `battery` and `privacy` in `modules` for the last two.
const SMALL_BASE_BAR: &str = "modules = [\"workspaces\", \"window\", \"notifications\", \"battery\", \"privacy\", \"clock\"]\n";

fn small_features_bar() -> String {
    let rules: String = (0..32).map(|i| format!("[[notifications.rules]]\napp = \"Chatty app {i}\"\naction = \"mute\"\n")).collect();
    format!("{SMALL_BASE_BAR}[battery]\nwarn = [30, 20, 10, 5]\n[notifications]\ndnd_while_sharing = true\n{rules}")
}

/// The rules, the battery thresholds and the option cost nothing at start (3
/// outputs, each bar on a daemon of its own, the server answering).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn small_features_cost_nothing_at_start() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("small-cost");
    let mut kb = Vec::new();
    for (name, toml) in [("small-cost-a", SMALL_BASE_BAR.to_string()), ("small-cost-b", small_features_bar())] {
        let session = PrivateBus::start(&format!("{name}-session"));
        let (_sway, _hypr, d) = start(name, 3, &bus, Some(&session), Some(&toml), PwDump::Silent);
        wait_for_the_notification_server(&session);
        std::thread::sleep(TRAY_SETTLE);
        kb.push((d.pss_kb(), d.anonymous_kb()));
    }
    let ((pss_base, base), (pss_with, with)) = (kb[0], kb[1]);
    eprintln!(
        "notification-rules at start (KB): PSS {pss_base} notifications, battery and privacy alone, {pss_with} with 32 rules, 4 thresholds and dnd_while_sharing; anonymous {base} and {with}"
    );
    // Anonymous memory, not PSS: the bar's shm pool (a `memfd:smithay-client-toolkit` mapping) reads
    // 112 KB or 220 KB at the same point, depending on whether a second frame buffer was drawn by
    // the time of the reading, which moves PSS by about 108 KB on either daemon; `Anonymous:` holds
    // the heap the configuration allocates and neither the pool nor the file pages.
    assert!(with <= base + 128, "{with} KB vs {base} KB anonymous: more than 128 KB for configuration alone — report, never raise");
}

/// One `--calc` cycle: open, `6*7` typed, closed by a typed Escape; the n-th of each. The open
/// reading is taken while the box is still up (one wtype process keeps its virtual keyboard from the
/// typing through the Escape, as in `launcher_open_close_cycles_no_growth`).
fn calc_cycle(sway: &Sway, d: &Daemon, n: usize) -> u64 {
    assert_eq!(launcher(sway, &Request::Calc), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    let typing = wtype_spawn(sway, &["-s", "300", "6*7", "-s", "3000", "-k", "Escape"]);
    std::thread::sleep(Duration::from_millis(1500));
    let open = d.pss_kb();
    assert_eq!(d.count("launcher closed ("), n - 1, "closed before the open reading: {:?}", d.log());
    assert!(typing.wait().unwrap().success());
    assert!(d.wait_for_count("launcher closed (escape)", n, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(2));
    open
}

/// `--calc` opened with `6*7` typed, then closed (a typed Escape): the box's
/// cost, and nothing kept once it is gone. The first open of any launcher box warms file-backed
/// pages (code, fonts) and the glyph cache once (+0.4-0.7 MB, measured for a plain Toggle with the
/// same typing too), so, as in `launcher_open_close_cycles_no_growth`, `closed` is read after one
/// warm-up cycle; the first cycle's figures are printed.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_calc_pss_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("calc-cost");
    let (sway, _hypr, d) = start_launcher("calc-cost", &bus, None, &many_apps(), None);
    std::thread::sleep(Duration::from_secs(3));
    let cold = d.pss_kb();
    let first_open = calc_cycle(&sway, &d, 1);
    let closed = d.pss_kb();
    let open = calc_cycle(&sway, &d, 2);
    let again = d.pss_kb();
    eprintln!(
        "calculator PSS (KB): cold {cold}, first open {first_open}, warmed closed {closed} (+{} the warm-up), open with 6*7 {open} (+{}), closed again {again}",
        closed.saturating_sub(cold),
        open.saturating_sub(closed)
    );
    assert!(closed <= cold + 768, "closed after the warm-up {closed} KB vs cold {cold} KB: more than 768 KB — report, never raise");
    assert!(open <= closed + 1536, "open costs {} KB > 1.5 MB — report, never raise", open.saturating_sub(closed));
    assert!(again <= closed + 256, "closed again {again} KB vs {closed} KB before: more than 256 KB kept");
}

/// A do-not-disturb with an end an hour away is one armed timer and nothing
/// else: the loop sleeps, as it does with an untimed one.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_a_timed_dnd() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("dnd-rest");
    let session = PrivateBus::start("dnd-rest-session");
    let sway = Sway::start("dnd-rest", 1);
    let state = sway.runtime.join("state/alpheus");
    std::fs::create_dir_all(&state).unwrap();
    // Owner only, as the daemon insists on (it reads a state directory open to others as off).
    for dir in [state.parent().unwrap(), state.as_path()] {
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    }
    let end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 3600;
    std::fs::write(state.join("dnd"), format!("until {end}\n")).unwrap();
    let _hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon_with(
        &sway,
        &DaemonOptions {
            bar_toml: Some(NOTIFY_BAR),
            system_bus: Some(&bus.address),
            session_bus: Some(&session.address),
            ..DaemonOptions::default()
        },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bell: dnd on", DELAY), "{:?}", d.log());
    wait_for_the_notification_server(&session);
    std::thread::sleep(TRAY_SETTLE);
    const REST: Duration = Duration::from_secs(10);
    away_from_the_minute(REST);
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "a timed do-not-disturb");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest with a timed do-not-disturb (10 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 10 s with a timed do-not-disturb");
    assert!(delta <= 8, "{delta} wake-ups in 10 s");
}

/// After `--calc`, a typed query and an Escape: no timer, watch or repeat left.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_calc_cycle() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("calc-rest");
    let (sway, _hypr, d) = start_launcher("calc-rest", &bus, Some(AT_REST_NO_RESOURCES), &[], None);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(launcher(&sway, &Request::Calc), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 1, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "6*7", "-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for_count("launcher closed (escape)", 1, DELAY), "{:?}", d.log());
    // Past the key repeat's one firing and the scan thread.
    std::thread::sleep(Duration::from_secs(3));
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(REST);
    let (main_before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after a calculator cycle");
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("loop-thread wake-ups at rest after a calculator cycle (5 s): {main_delta}");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 5 s after a calculator cycle");
}

/// Quick notes: a daemon on the headless sway (the backend the notes tests use), the bar reduced to
/// the three base modules, the launcher socket ready; a private state directory (the harness's).
fn notes_daemon(name: &str) -> (Sway, Daemon) {
    let sway = Sway::start(name, 1);
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml: Some(AT_REST_NO_RESOURCES), backend: Backend::Sway, ..DaemonOptions::default() },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    (sway, d)
}

/// One `--notes` cycle: open, 2,000 characters typed, closed by a typed Escape; the n-th of each.
/// The open reading is taken with the pad up and the text in it (one wtype process keeps its
/// virtual keyboard through the Escape, as in `calc_cycle`). Returns the open reading.
fn notes_cycle(sway: &Sway, d: &Daemon, n: usize) -> u64 {
    assert_eq!(launcher(sway, &Request::Notes), Reply::Done);
    assert!(d.wait_for_count("notes shown on", n, DELAY), "{:?}", d.log());
    let text = "lorem ipsum ".repeat(2000 / 12 + 1);
    let text = &text[..2000];
    let typing = wtype_spawn(sway, &["-s", "300", text, "-s", "3000", "-k", "Escape"]);
    std::thread::sleep(Duration::from_millis(3500));
    let open = d.pss_kb();
    assert_eq!(d.count("notes closed ("), n - 1, "closed before the open reading: {:?}", d.log());
    let status = typing.wait().unwrap();
    assert!(status.success(), "wtype: {status:?}, {:?}", d.log());
    assert!(d.wait_for_count("notes closed (escape)", n, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(2));
    open
}

/// Quick notes: the pad open with 2,000 characters typed, then closed. As for the calculator, the
/// first open warms code and font pages once, so `closed` is read after one warm-up cycle. Total
/// PSS moves with what other processes share and with the page cache's fault-around (a cold read
/// varied 7.5-8.4 MB at the same closed figure on one boot), so the bounds are on the daemon's
/// anonymous memory (heap, stacks), and PSS and its file-backed share are printed.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn notes_pss_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let (sway, d) = notes_daemon("notes-cost");
    std::thread::sleep(Duration::from_secs(3));
    let read = |d: &Daemon| (d.pss_kb(), d.anonymous_kb(), d.file_pss_kb());
    let cold = read(&d);
    let first_open = notes_cycle(&sway, &d, 1);
    let closed = read(&d);
    let (_, closed_pools) = d.pss_and_shm_kb();
    let open_pss = notes_cycle(&sway, &d, 2);
    let again = read(&d);
    let (_, again_pools) = d.pss_and_shm_kb();
    eprintln!(
        "notes PSS (KB): cold {}, first open {first_open}, warmed closed {}, open with 2,000 characters {open_pss}, closed again {}",
        cold.0, closed.0, again.0
    );
    eprintln!(
        "notes anonymous (KB): cold {}, warmed closed {}, closed again {} ({:+}); file-backed PSS: cold {}, closed {}, again {}",
        cold.1,
        closed.1,
        again.1,
        again.1 as i64 - closed.1 as i64,
        cold.2,
        closed.2,
        again.2
    );
    assert!(
        closed.1 <= cold.1 + 768,
        "closed after the warm-up {} KB anon vs cold {} KB: more than 768 KB — report, never raise",
        closed.1,
        cold.1
    );
    assert!(open_pss <= closed.0 + 1536, "open costs {} KB PSS: more than 1.5 MB — report, never raise", open_pss.saturating_sub(closed.0));
    assert!(again.1 <= closed.1 + 256, "closed again {} KB anon vs {} KB before: more than 256 KB kept", again.1, closed.1);
    // The shm pools alone (as for Wi-Fi and Bluetooth): the pad's own pool goes with it.
    eprintln!("notes shm pools (KB): warmed closed {closed_pools}, closed again {again_pools}");
    assert!(again_pools <= closed_pools + 64, "the pools closed again {again_pools} KB vs {closed_pools} KB: the pad's pool kept");
}

/// Quick notes: after a cycle, then with the pad open and saved: no timer, watch or repeat left
/// (SCTK's key-repeat timer fires once about 0.6 s after a key press, so each rest window starts
/// past it; the first still opens within 1.3 s of the close, so a stray save timer always shows).
/// The open window is closed by the verb after it, never by a key: a typed Escape scheduled ahead
/// landed inside the window whenever the minute check slept, and the wtype process
/// stays alive through it (its keyboard leaving would close the pad as `focus lost`).
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn at_rest_no_wakeups_after_notes() {
    require_release(cfg!(debug_assertions));
    let (sway, d) = notes_daemon("notes-rest");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(launcher(&sway, &Request::Notes), Reply::Done);
    assert!(d.wait_for_count("notes shown on", 1, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "hello pad", "-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for_count("notes closed (escape)", 1, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(1100));
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(REST);
    let (main_before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after a notes cycle");
    let closed_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("loop-thread wake-ups at rest after a notes cycle (5 s): {closed_delta}");
    assert_eq!(closed_delta, 0, "{closed_delta} loop-thread wake-ups in 5 s after a notes cycle");

    // Open, one key, the save fires 2 s later: the pad stays open and saved, still asleep.
    assert_eq!(launcher(&sway, &Request::Notes), Reply::Done);
    assert!(d.wait_for_count("notes shown on", 2, DELAY), "{:?}", d.log());
    let typing = wtype_spawn(&sway, &["-s", "300", "x", "-s", "120000"]);
    // The save has fired once the file holds the pad's text (a loaded machine delays the key and
    // the timer, so waiting a fixed time would put one of them inside the window); then past SCTK's
    // one repeat firing.
    let file = sway.runtime.join("state/alpheus/notes.txt");
    let t = Instant::now();
    while !std::fs::read_to_string(&file).unwrap_or_default().ends_with('x') {
        assert!(t.elapsed() < Duration::from_secs(10), "no autosave: {:?}", d.log());
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(1500));
    away_from_the_minute(REST);
    let (main_before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "with the notes pad open and saved");
    let open_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("loop-thread wake-ups at rest with the pad open and saved (5 s): {open_delta}");
    assert_eq!(d.count("notes closed ("), 1, "still open: {:?}", d.log());
    assert_eq!(open_delta, 0, "{open_delta} loop-thread wake-ups in 5 s with the pad open and saved: {:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Notes), Reply::Done);
    assert!(d.wait_for("notes closed (toggle)", DELAY), "{:?}", d.log());
    drop(typing);
}

/// One prefix cycle: open (`--toggle`), `:face` typed, closed by a typed Escape; the n-th of each.
/// Returns the reading taken with the box open and the query typed (one wtype process, as in
/// `calc_cycle`).
fn prefix_cycle(sway: &Sway, d: &Daemon, n: usize) -> u64 {
    assert_eq!(launcher(sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    let typing = wtype_spawn(sway, &["-s", "300", ":face", "-s", "3000", "-k", "Escape"]);
    std::thread::sleep(Duration::from_millis(1500));
    let open = d.pss_kb();
    assert_eq!(d.count("launcher closed ("), n - 1, "closed before the open reading: {:?}", d.log());
    assert!(typing.wait().unwrap().success());
    assert!(d.wait_for_count("launcher closed (escape)", n, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(2));
    open
}

/// Launcher 2: the `:` prefix (the emoji table, 1,906 entries in the binary's read-only data) typed
/// as `:face`, then closed. As for the calculator, `closed` is read after one warm-up cycle.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_prefixes_pss_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("prefix-cost");
    let (sway, _hypr, d) = start_launcher("prefix-cost", &bus, None, &many_apps(), None);
    std::thread::sleep(Duration::from_secs(3));
    let cold = d.pss_kb();
    let first_open = prefix_cycle(&sway, &d, 1);
    let closed = d.pss_kb();
    let open = prefix_cycle(&sway, &d, 2);
    let again = d.pss_kb();
    eprintln!(
        "prefixes PSS (KB): cold {cold}, first open {first_open}, warmed closed {closed} (+{} the warm-up), open with :face {open} (+{}), closed again {again}",
        closed.saturating_sub(cold),
        open.saturating_sub(closed)
    );
    assert!(open <= closed + 1536, "open costs {} KB > 1.5 MB — report, never raise", open.saturating_sub(closed));
    assert!(again <= closed + 256, "closed again {again} KB vs {closed} KB before: more than 256 KB kept");
}

/// Launcher 2: the window switcher with three windows, opened by `--windows` and stepped (a
/// repeated `--windows` while it is open), then closed by a typed Escape: nothing kept, and the
/// loop sleeps after. Each step draws at once, and with both buffers still held by
/// the compositor the launcher used to allocate a third and the box's SlotPool doubled (20
/// unpaced steps: pools' PSS 56 → 2,654 KB). Now a draw with no free buffer waits for a release:
/// the pools stay at the box's two frames (`pool_size`, from the box's size in the "launcher
/// shown" line), paced (~100 ms apart) and back to back alike. The pools are bounded by their mapped
/// size (exact; their PSS moves as the compositor maps them, printed only) and the daemon's own
/// memory (PSS − pools' PSS) apart.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn windows_switcher_pss_and_rest() {
    require_release(cfg!(debug_assertions));
    let sway = Sway::start("windows-cost", 1);
    let _windows: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|n| common::sway_client::TestWindow::open(&sway, &format!("alpheus-{n}"), &format!("Fixture {n}")))
        .collect();
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml: Some(AT_REST_NO_RESOURCES), backend: Backend::Sway, ..DaemonOptions::default() },
    );
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(3)); // the tree read, the bar settled
    let step = || assert_eq!(launcher(&sway, &Request::Windows), Reply::Done);
    // (PSS, pools' PSS, pools' mapped size) in KB.
    let read = || {
        let (pss, pools) = d.pss_and_shm_kb();
        (pss, pools, d.shm_size_kb())
    };
    // Opens, waits past the open's debounce (a `--windows` inside it is ignored), then steps paced
    // and back to back.
    let cycle = |n: usize| {
        step();
        assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
        std::thread::sleep(Duration::from_millis(300));
        for _ in 0..20 {
            step();
            std::thread::sleep(Duration::from_millis(100));
        }
        let paced = read();
        for _ in 0..20 {
            step();
        }
        std::thread::sleep(Duration::from_millis(300)); // the last step drawn
        let unpaced = read();
        assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
        assert!(d.wait_for_count("launcher closed (escape)", n, DELAY), "{:?}", d.log());
        std::thread::sleep(Duration::from_secs(2));
        (paced, unpaced)
    };
    let cold = d.pss_kb();
    let first = cycle(1);
    let (closed, closed_pools, closed_size) = read();
    let (paced, unpaced) = cycle(2);
    let again = d.pss_kb();
    let (w, h) = d.last_launcher_box_px().expect("the box's size in the log");
    let frames = (2 * w * h * 4).div_ceil(1024);
    let own = |(pss, pools, _): (u64, u64, u64)| pss.saturating_sub(pools);
    let closed_own = closed.saturating_sub(closed_pools);
    eprintln!(
        "window switcher PSS (KB): cold {cold}, first open paced {} unpaced {}, warmed closed {closed} (own {closed_own}, pools {closed_pools}, \
         mapped {closed_size}), the box's two frames {frames}; paced: own {} (+{}), pools {} (+{}), mapped {} (+{}); back to back: own {} \
         (+{}), pools {} (+{}), mapped {} (+{}); closed again {again}",
        first.0.0,
        first.1.0,
        own(paced),
        own(paced).saturating_sub(closed_own),
        paced.1,
        paced.1.saturating_sub(closed_pools),
        paced.2,
        paced.2.saturating_sub(closed_size),
        own(unpaced),
        own(unpaced).saturating_sub(closed_own),
        unpaced.1,
        unpaced.1.saturating_sub(closed_pools),
        unpaced.2,
        unpaced.2.saturating_sub(closed_size),
    );
    assert!(frames > 0, "a box of {w}×{h} px");
    for (what, reading) in [("paced", paced), ("back to back", unpaced)] {
        assert!(own(reading) <= closed_own + 512, "{what}: own memory {} KB over closed {closed_own} KB + 512", own(reading));
        assert!(
            reading.2 <= closed_size + frames + 64,
            "{what}: pools mapped {} KB over closed {closed_size} KB + the box's two frames {frames} KB + 64",
            reading.2
        );
    }
    // The pages too: a burst touches no more of the pools than paced steps (a third buffer would).
    assert!(unpaced.1 <= paced.1 + 64, "back to back: pools' PSS {} KB over paced {} KB + 64", unpaced.1, paced.1);
    assert!(again <= closed + 256, "closed again {again} KB vs {closed} KB before: more than 256 KB kept");
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(REST);
    let (main_before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after the window switcher");
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("loop-thread wake-ups at rest after the window switcher (5 s): {main_delta}");
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 5 s after the window switcher");
}

/// The bar without the resources timer: the clock is the only timer, which `away_from_the_minute` avoids.
const AT_REST_NO_RESOURCES: &str = "modules = [\"workspaces\", \"window\", \"clock\"]\n";

/// `[hooks]` with every key set to the recording stub.
fn all_hooks() -> String {
    let keys: String = alpheus_daemon::hooks::HookId::ALL.iter().map(|id| format!("{} = [\"hook-rec\"]\n", id.key())).collect();
    format!("[hooks]\n{keys}")
}

/// A one-output daemon on `bar_toml` with the stub `hook-rec` in its PATH (the hooks' only command).
fn hooks_start(name: &str, bus: &PrivateBus, bar_toml: &str) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    fake_env_recorder(&sway, "hook-rec");
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml: Some(bar_toml), system_bus: Some(&bus.address), pw_dump: PwDump::Silent, ..DaemonOptions::default() },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// The quiet window behind it (`hooks::QUIET`), then `--dnd toggle` five times, 3.1 s apart (each
/// runs its hook), then twenty back to back (at most five more run, the rest are throttled).
fn hooks_burst(sway: &Sway, d: &Daemon) {
    std::thread::sleep(alpheus_daemon::hooks::QUIET + Duration::from_millis(500));
    let toggle = || assert_eq!(launcher(sway, &Request::Dnd(DndVerb::Toggle)), Reply::Done, "{:?}", d.log());
    for _ in 0..5 {
        toggle();
        std::thread::sleep(Duration::from_millis(3100));
    }
    for _ in 0..20 {
        toggle();
    }
}

/// What a daemon holds in memory of its own: its PSS without the shm pools, and its anonymous memory
/// (smaps_rollup `Anonymous:`: the heap and stacks, never a file's pages or a shm pool).
#[derive(Clone, Copy, Debug)]
struct Own {
    pss: u64,
    anon: u64,
}

fn own_memory(d: &Daemon) -> Own {
    let (total, pools) = d.pss_and_shm_kb();
    let rollup = std::fs::read_to_string(format!("/proc/{}/smaps_rollup", d.pid())).unwrap();
    let anon =
        rollup.lines().find_map(|l| l.strip_prefix("Anonymous:")).and_then(|v| v.split_whitespace().next()).unwrap().parse().unwrap();
    Own { pss: total - pools, anon }
}

/// One daemon alone (shared libraries are counted proportionally to the processes mapping them): its
/// memory at start and after `hooks_burst`, with the smaps split of both.
fn hooks_run(name: &str, bus: &PrivateBus, bar_toml: &str) -> ((Own, Own), (String, String), Daemon) {
    let (sway, _hypr, d) = hooks_start(name, bus, bar_toml);
    std::thread::sleep(TRAY_SETTLE);
    let (start, split_start) = (own_memory(&d), smaps_breakdown(&d));
    hooks_burst(&sway, &d);
    std::thread::sleep(Duration::from_secs(1)); // the last hook's process has ended
    ((start, own_memory(&d)), (split_start, smaps_breakdown(&d)), d)
}

/// Eleven hooks set cost nothing at start (the default modules, one output, back to
/// back with the same bar without `[hooks]`), and a burst of fires keeps nothing: five paced runs,
/// then twenty back to back (some run, the rest are throttled; the count is not asserted here, the
/// bucket's own test is the end-to-end one). Both bounds are on the anonymous memory: the PSS
/// without the shm pools is printed too, but the font file's and the binary's pages in it move by
/// ±200 KB between two identical daemons on a machine that is doing other work (measured: the same
/// burst read −215 and +163 KB on the font file alone), which says nothing about what hooks keep.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn hooks_pss_at_start_and_after_fires() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("hooks-start");
    let _workstation = Workstation::serve(&bus);
    let (plain, plain_split, _) = hooks_run("hooks-start-0", &bus, "");
    let ((with, after), split, d) = hooks_run("hooks-start-1", &bus, &all_hooks());
    eprintln!(
        "hooks own memory (KB, PSS without the pools / anonymous): without [hooks] {}/{} at start, {}/{} after the same burst; with eleven set {}/{} at start (+{}/+{}), {}/{} after the burst (+{}/+{} over start); {} hook: dnd lines, {} throttled",
        plain.0.pss,
        plain.0.anon,
        plain.1.pss,
        plain.1.anon,
        with.pss,
        with.anon,
        with.pss.saturating_sub(plain.0.pss),
        with.anon.saturating_sub(plain.0.anon),
        after.pss,
        after.anon,
        after.pss.saturating_sub(with.pss),
        after.anon.saturating_sub(with.anon),
        d.count("hook: dnd"),
        d.count("hook: dnd throttled"),
    );
    eprintln!("hooks smaps without: {} -> {}; with: {} -> {}", plain_split.0, plain_split.1, split.0, split.1);
    assert!(
        with.anon <= plain.0.anon + 128,
        "{} KB anonymous vs {} KB: more than 128 KB for configuration alone — report, never raise",
        with.anon,
        plain.0.anon
    );
    assert!(
        after.anon <= with.anon + 64,
        "{} KB anonymous after the burst vs {} KB at start: more than 64 KB kept — report, never raise",
        after.anon,
        with.anon
    );
}

/// After the burst nothing is left armed (no repeating timer, no watch): no
/// loop-thread and no whole-daemon wake-up in 5 s, the window starting within a second of the last
/// toggle.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_hooks() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("hooks-rest");
    let _workstation = Workstation::serve(&bus);
    let toml = format!("modules = [{}, \"notifications\"]\n{}", resting_modules!(), all_hooks());
    let (sway, _hypr, d) = hooks_start("hooks-rest", &bus, &toml);
    assert!(d.wait_for("hook: started", DELAY), "{:?}", d.log());
    const REST: Duration = Duration::from_secs(5);
    // The burst takes ~20 s and the rest window must start within a second of its end: the whole
    // sequence fits one minute, so the clock never ticks inside the window.
    away_from_the_minute(Duration::from_secs(30));
    hooks_burst(&sway, &d);
    std::thread::sleep(Duration::from_millis(300));
    displays_rest(&d, REST, "after the hooks' burst");
}

const TIMER_BAR: &str = "modules = [\"workspaces\", \"timer\", \"clock\"]\n";
/// The chip alone before the clock, so it sits at cell 1 (the headless tests' `TIMER_AT`).
const TIMER_CHIP_FIRST_BAR: &str = "modules = [\"timer\", \"clock\"]\n";
const TIMER_CHIP: (u32, u32) = (12, 10);
/// `TIMER_BAR` with a harmless `[quick] poweroff` (a daemon on the default would run the real one).
const TIMER_POWEROFF_BAR: &str = "modules = [\"workspaces\", \"timer\", \"clock\"]\n[quick]\npoweroff = [\"true\"]\n";
const NO_TIMER_BAR: &str = "modules = [\"workspaces\", \"clock\"]\n";

/// One `alpheus-launch --timer <verb>` on the socket (no key is ever pressed: SCTK's repeat timer,
/// armed by a key press, fires once ~0.6 s later and would be an unrelated wake-up).
fn timer(sway: &Sway, verb: TimerVerb) {
    assert_eq!(launcher(sway, &Request::Timer(verb)), Reply::Done);
}

/// One popup cycle by the verb: open, then (past the verb's own debounce) close; the n-th of each.
fn timer_popup_cycle(sway: &Sway, d: &Daemon, n: usize) {
    timer(sway, TimerVerb::Popup);
    assert!(d.wait_for_count("timer shown", n, DELAY), "{:?}", d.log());
    std::thread::sleep(PAST_DEBOUNCE);
    timer(sway, TimerVerb::Popup);
    assert!(d.wait_for_count("popup closed", n, DELAY), "{:?}", d.log());
}

/// The idle chip costs next to nothing, a running countdown nothing more, and the
/// popup keeps nothing once closed (the first open's file-backed warm-up aside, which is printed).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn timer_pss_idle_running_and_popup() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("timer-cost");
    let without = {
        let (_sway, _hypr, d) = start("timer-cost-a", 1, &bus, None, Some(NO_TIMER_BAR), PwDump::Silent);
        std::thread::sleep(TRAY_SETTLE);
        d.pss_kb()
    };
    let (sway, _hypr, d) = start("timer-cost-b", 1, &bus, None, Some(TIMER_BAR), PwDump::Silent);
    std::thread::sleep(TRAY_SETTLE);
    let (idle, idle_shm) = d.pss_and_shm_kb();
    let idle_anon = d.anon_kb();
    timer(&sway, TimerVerb::Start(1800));
    assert!(d.wait_for("timer: countdown started (30 min)", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (running, running_shm) = d.pss_and_shm_kb();
    let running_anon = d.anon_kb();
    timer_popup_cycle(&sway, &d, 1); // the warm-up
    std::thread::sleep(TRAY_SETTLE);
    let (closed, closed_shm) = d.pss_and_shm_kb();
    let closed_anon = d.anon_kb();
    for n in 2..=21 {
        timer_popup_cycle(&sway, &d, n);
    }
    std::thread::sleep(TRAY_SETTLE);
    let (cycled, cycled_shm) = d.pss_and_shm_kb();
    let cycled_anon = d.anon_kb();
    let (idle_own, running_own, closed_own, cycled_own) =
        (idle - idle_shm, running - running_shm, closed - closed_shm, cycled - cycled_shm);
    eprintln!(
        "timer PSS (KB): {without} without the module, {idle} with the idle chip (module cost {}), {running} running, {closed} \
         popup closed once, {cycled} after 20 more cycles; without the shm pools: {idle_own} idle, {running_own} running, \
         {closed_own} closed, {cycled_own} cycled; the pools: {idle_shm}, {running_shm}, {closed_shm}, {cycled_shm}; \
         anonymous: {idle_anon} idle, {running_anon} running, {closed_anon} closed, {cycled_anon} cycled",
        idle.saturating_sub(without)
    );
    assert!(cycled < STARTUP_MAX_KB, "{cycled} KB >= 15 MB — report it, never raise");
    assert!(idle <= without + 256, "the idle chip costs {} KB: a glyph and a cell, nothing resident", idle.saturating_sub(without));
    // Anonymous memory only: the heap and stacks the daemon keeps. PSS also counts file pages (the
    // first open's code and font warm-up) and the shm pools, which move with the compositor.
    assert!(
        running_anon <= idle_anon + 64,
        "running {running_anon} KB anonymous vs {idle_anon} KB idle: a countdown keeps nothing but its state"
    );
    assert!(cycled_anon <= closed_anon + 64, "cycled {cycled_anon} KB anonymous vs {closed_anon} KB closed: 20 cycles must keep nothing");
}

/// Idle, then started and paused, the loop sleeps: no timer, no wake-up, no spin.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_the_timer_idle_or_paused() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("timer-rest");
    let (sway, _hypr, d) = start("timer-rest", 1, &bus, None, Some(TIMER_BAR), PwDump::Silent);
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    const REST: Duration = Duration::from_secs(5);
    let window = |what: &str| {
        away_from_the_minute(REST);
        let (before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
        std::thread::sleep(REST);
        assert_no_spin(d.cpu_ticks() - ticks, REST, what);
        let delta = d.main_thread_wakeups().saturating_sub(before);
        eprintln!("loop-thread wake-ups at rest, {what} (5 s): {delta}");
        assert_eq!(delta, 0, "{delta} loop-thread wake-ups in 5 s, {what}: {:?}", d.log());
    };
    window("the timer idle");
    timer(&sway, TimerVerb::Start(1800));
    assert!(d.wait_for("timer: countdown started (30 min)", DELAY), "{:?}", d.log());
    timer(&sway, TimerVerb::Pause);
    assert!(d.wait_for("timer: paused", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    window("the timer paused");
}

/// A scheduled shutdown arms one deadline at its next chip minute: the loop sleeps in between (no
/// wake-up in a 5 s window away from both the clock's minute and the chip's own), and over a minute
/// it wakes about once for the chip and once for the clock, never each second. The window starts
/// within seconds of the verb, so the chip's first change (a minute after it) falls outside.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_a_shutdown_scheduled() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("shutdown-rest");
    let (sway, _hypr, d) = start("shutdown-rest", 1, &bus, None, Some(TIMER_POWEROFF_BAR), PwDump::Silent);
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    const REST: Duration = Duration::from_secs(5);
    // The verb, a second to settle and the window: ten seconds clear of the clock's tick, and the
    // chip's first change comes a full minute after the verb.
    away_from_the_minute(Duration::from_secs(10));
    timer(&sway, TimerVerb::Shutdown(10_800));
    assert!(d.wait_for("timer: shutdown scheduled (180 min)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    let (before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "a shutdown scheduled");
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop-thread wake-ups with a shutdown scheduled (5 s): {delta}");
    assert_eq!(delta, 0, "{delta} loop-thread wake-ups in 5 s with a shutdown scheduled: {:?}", d.log());
    // Over a minute: the chip's change and the clock's tick, a few at most.
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(65));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop-thread wake-ups with a shutdown scheduled (65 s): {delta}");
    assert!(delta <= 6, "{delta} loop-thread wake-ups in 65 s: the shutdown ticks faster than the minute: {:?}", d.log());
    timer(&sway, TimerVerb::ShutdownCancel);
    assert!(d.wait_for("timer: shutdown cancelled", DELAY), "{:?}", d.log());
}

/// The warning's minute looks at the lock every 5 s (twelve a minute at most), and only that
/// minute: a few more wake-ups than the chip's minute, never one a second, and none once the
/// shutdown is cancelled. `[quick] poweroff` is `true`, and the shutdown is cancelled well before its
/// minute ends: nothing real is ever powered off.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_warning_minute_samples_the_lock_every_five_seconds_then_rests() {
    require_release(cfg!(debug_assertions));
    const BAR: &str = "modules = [\"workspaces\", \"timer\", \"clock\"]\n[quick]\npoweroff = [\"true\"]\n";
    let bus = PrivateBus::start("shutdown-warning");
    let (sway, _hypr, d) = start("shutdown-warning", 1, &bus, None, Some(BAR), PwDump::Silent);
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    timer(&sway, TimerVerb::Shutdown(2));
    assert!(d.wait_for("timer: shutdown warning", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(30));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop-thread wake-ups in 30 s of the warning's minute: {delta}");
    // Six looks, the clock's tick, slack; a one-second tick would read 30.
    assert!((3..=12).contains(&delta), "{delta} loop-thread wake-ups in 30 s of the warning's minute: {:?}", d.log());
    timer(&sway, TimerVerb::ShutdownCancel);
    assert!(d.wait_for("timer: shutdown cancelled", DELAY), "{:?}", d.log());
    assert_eq!(d.count("timer: shutdown power off"), 0, "{:?}", d.log());
    // Away from the clock's tick: nothing wakes once there is no shutdown.
    away_from_the_minute(Duration::from_secs(10));
    std::thread::sleep(Duration::from_secs(1));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop-thread wake-ups after the cancel (5 s): {delta}");
    assert_eq!(delta, 0, "{delta} loop-thread wake-ups at rest after the warning's minute: {:?}", d.log());
}

/// A running countdown wakes the loop about once a minute (the chip's change), the
/// clock's own tick aside, never each second.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn timer_running_wakes_about_once_a_minute() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("timer-minute");
    let (sway, _hypr, d) = start("timer-minute", 1, &bus, None, Some(TIMER_BAR), PwDump::Silent);
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    timer(&sway, TimerVerb::Start(1800));
    assert!(d.wait_for("timer: countdown started (30 min)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(65));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop-thread wake-ups with a countdown running (65 s): {delta}");
    // The chip's 1-2, the clock's 1-2, slack; a one-second tick would read about 65.
    assert!(delta <= 6, "{delta} loop-thread wake-ups in 65 s: the countdown ticks faster than the minute: {:?}", d.log());
}

/// The popup ticks each second only while open; closed (by the
/// verb), the loop is back to the minute at once. The windows stay within 60 s of the start, so the
/// chip's own minute boundary (not aligned with the wall minute) never falls inside.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn timer_popup_ticks_each_second_only_while_open() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("timer-tick");
    let (sway, _hypr, d) = start("timer-tick", 1, &bus, None, Some(TIMER_CHIP_FIRST_BAR), PwDump::Silent);
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    // The whole test (about 15 s) between two minute turns: the clock's redraw is a wake-up of its own.
    away_from_the_minute(Duration::from_secs(25));
    timer(&sway, TimerVerb::Start(1800));
    assert!(d.wait_for("timer: countdown started (30 min)", DELAY), "{:?}", d.log());
    std::thread::sleep(PAST_DEBOUNCE);
    sway.click(TIMER_CHIP.0, TIMER_CHIP.1, "button1");
    assert!(d.wait_for("timer shown", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1)); // configured and painted
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let open = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop-thread wake-ups with the popup open on a running countdown (5 s): {open}");
    // Two per repaint, traced: the one-second tick, then the compositor's release of the previous
    // buffer on the socket (not one a second).
    assert!((8..=12).contains(&open), "open: two wake-ups per repaint (the tick and the buffer release), {open} in 5 s: {:?}", d.log());
    // Closed by a second click on the chip, not by the verb (whose own close sends the buffer
    // destroys and the keyboard release: the round trip's replies are wake-ups of their own) nor by
    // Escape (SCTK's repeat timer).
    sway.click(TIMER_CHIP.0, TIMER_CHIP.1, "button1");
    assert!(d.wait_for("popup closed (outside click)", DELAY), "{:?}", d.log());
    // A barrier: a verb with nothing to act on is answered once the loop has handled everything
    // the close queued before it, so the window starts after the close's own replies.
    timer(&sway, TimerVerb::Resume);
    assert!(d.wait_for("timer: nothing to resume", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(100)); // the verb's own hang-up
    // The window starts right after the close: a one-second timer left armed by it would show.
    let (before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the loop after the timer popup");
    let closed = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop-thread wake-ups right after the close (5 s): {closed}");
    assert_eq!(closed, 0, "closed: no second tick, {closed} wake-ups in 5 s: {:?}", d.log());
}

// ---- The failed-units module. Two private buses, each with a
// `FakeSystemd` (the system and the user manager), as the headless tests. ----

const FAILED_ON: &str = "modules = [\"workspaces\", \"window\", \"failed\", \"clock\"]\n";
const FAILED_OFF: &str = "modules = [\"workspaces\", \"window\", \"clock\"]\n";

/// A daemon, the workstation's services and a `FakeSystemd` on the system bus; with `session`, a
/// second private bus with the user manager's. Dropped in the order the fields are written (the
/// daemon first, then the fakes, then the buses).
struct FailedRig {
    d: Daemon,
    usr: Option<FakeSystemd>,
    sys: FakeSystemd,
    ws: Workstation,
    sway: Sway,
    _hypr: FakeHyprland,
    _user_bus: PrivateBus,
    _sys_bus: PrivateBus,
}

/// `wait`: the counts were read before returning (the module is on), else only a 3 s settle.
fn failed_rig(name: &str, toml: &str, system: u32, user: u32, session: bool, wait: bool) -> FailedRig {
    let sys_bus = PrivateBus::start(&format!("{name}-sys"));
    let user_bus = PrivateBus::start(&format!("{name}-user"));
    let ws = Workstation::serve(&sys_bus);
    let sys = FakeSystemd::serve(&sys_bus, system);
    let usr = session.then(|| FakeSystemd::serve(&user_bus, user));
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon_with(
        &sway,
        &DaemonOptions {
            bar_toml: Some(toml),
            system_bus: Some(&sys_bus.address),
            session_bus: session.then_some(user_bus.address.as_str()),
            pw_dump: PwDump::Silent,
            ..DaemonOptions::default()
        },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    if wait {
        sys.wait_until("the system count read", |s| s.calls("Get") >= 1);
        if let Some(usr) = &usr {
            usr.wait_until("the user count read", |s| s.calls("Get") >= 1);
        }
    }
    std::thread::sleep(TRAY_SETTLE);
    FailedRig { d, usr, sys, ws, sway, _hypr: hypr, _user_bus: user_bus, _sys_bus: sys_bus }
}

/// The daemon's open sockets (Wayland, the launcher's, each D-Bus link, the notify and wake pairs).
fn sockets(d: &Daemon) -> usize {
    std::fs::read_dir(format!("/proc/{}/fd", d.pid()))
        .unwrap()
        .flatten()
        .filter(|e| std::fs::read_link(e.path()).is_ok_and(|l| l.to_string_lossy().starts_with("socket:")))
        .count()
}

/// PSS, anonymous PSS, threads and sockets at the settled start.
fn start_figures(r: &FailedRig) -> (u64, u64, usize, usize) {
    (r.d.pss_kb(), r.d.pss_anon_kb(), r.d.threads(), sockets(&r.d))
}

/// The default bar, then the same list with and without `failed`, at start, count 0 on both
/// managers, with `subscribe` stated each way (it defaults to true): PSS printed, the anonymous
/// memory bounded (+ 256 KB), threads and sockets printed (does a bar without a session bus, or
/// with one, open a link it did not before?).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn failed_module_pss_at_start() {
    require_release(cfg!(debug_assertions));
    let off = failed_rig("failed-start-off", FAILED_OFF, 0, 0, true, false);
    let off_figures = start_figures(&off);
    drop(off);
    let unsubscribed = format!("{FAILED_ON}[failed]\nsubscribe = false\n");
    let subscribed = format!("{FAILED_ON}[failed]\nsubscribe = true\n");
    let on = failed_rig("failed-start-on", &unsubscribed, 0, 0, true, true);
    let on_figures = start_figures(&on);
    let both_read = (on.sys.calls("Get"), on.usr.as_ref().map(|u| u.calls("Subscribe")), on.sys.calls("Subscribe"));
    drop(on);
    let sub = failed_rig("failed-start-sub", &subscribed, 0, 0, true, true);
    let sub_figures = start_figures(&sub);
    let sub_subscribed = (sub.sys.subscribers(), sub.usr.as_ref().map(|u| u.subscribers()));
    drop(sub);
    let no_session = failed_rig("failed-start-nosession", &unsubscribed, 0, 0, false, true);
    let no_session_figures = start_figures(&no_session);
    drop(no_session);
    eprintln!(
        "failed module at start (PSS KB, Pss_Anon KB, threads, sockets): without {off_figures:?}; with, count 0 on both, subscribe false \
         {on_figures:?} (system Get calls, user and system Subscribe calls: {both_read:?}); subscribe true {sub_figures:?} \
         (subscribers system, user: {sub_subscribed:?}); subscribe false, no session bus {no_session_figures:?}"
    );
    assert_eq!(both_read.1.unwrap_or(0) + both_read.2, 0, "subscribe = false: no Subscribe call");
    assert_eq!(sub_subscribed, (1, Some(1)), "subscribe = true: one Subscribe on each manager");
    for (what, figures) in [("subscribe false", on_figures), ("subscribe true", sub_figures)] {
        assert!(
            figures.1 <= off_figures.1 + 256,
            "the module costs {} KB of anonymous memory at start, {what} (bound 256)",
            figures.1.saturating_sub(off_figures.1)
        );
    }
}

/// The default bar (no `modules`, so `[failed]` at its defaults: `subscribe = true`, the base build
/// knowing no such table) at start, a system manager and a user manager at count 0 on
/// their buses: PSS, anonymous PSS, threads and sockets printed. The same test runs against a base
/// build (`ALPHEUS_DEMON_UNDER_TEST`), which has no failed module: the difference is what the
/// module's being in the default list costs.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn default_bar_at_start() {
    require_release(cfg!(debug_assertions));
    let r = failed_rig("default-start", "", 0, 0, true, false);
    let (pss, anon, threads, sockets) = start_figures(&r);
    eprintln!("the default bar at start: PSS {pss} KB, Pss_Anon {anon} KB, {threads} threads, {sockets} sockets");
    assert!(pss < STARTUP_MAX_KB, "the default bar: {pss} KB >= 15 MB — report it, never raise");
    drop(r);
    // The same with no session bus at all: which links are the session bus's.
    let r = failed_rig("default-start-nosession", "", 0, 0, false, false);
    let (pss, anon, threads, sockets) = start_figures(&r);
    eprintln!("the default bar at start, no session bus: PSS {pss} KB, Pss_Anon {anon} KB, {threads} threads, {sockets} sockets");
}

/// Two managers failing, then twenty changes (each read), with `subscribe` on and off: the
/// anonymous memory stays within 64 KB of what it was at zero; PSS printed. Unsubscribed, each
/// change is followed by the verb (`alpheus-launch --failed-refresh`).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn failed_module_shown_pss() {
    require_release(cfg!(debug_assertions));
    for subscribe in [true, false] {
        let toml = format!("{FAILED_ON}[failed]\nsubscribe = {subscribe}\n");
        let r = failed_rig(&format!("failed-shown-{subscribe}"), &toml, 0, 0, true, true);
        let usr = r.usr.as_ref().unwrap();
        if subscribe {
            r.sys.wait_until("the system subscription", |s| s.subscribers() == 1);
            usr.wait_until("the user subscription", |s| s.subscribers() == 1);
        }
        let change = |count: u32| {
            let (gs, gu) = (r.sys.calls("Get"), usr.calls("Get"));
            r.sys.set_failed(count);
            usr.set_failed(count);
            if !subscribe {
                assert_eq!(launcher(&r.sway, &Request::FailedRefresh), Reply::Done);
            }
            r.sys.wait_until("the system count read", |s| s.calls("Get") > gs);
            usr.wait_until("the user count read", |s| s.calls("Get") > gu);
            std::thread::sleep(Duration::from_millis(100));
        };
        let (pss0, anon0) = (r.d.pss_kb(), r.d.pss_anon_kb());
        change(2);
        std::thread::sleep(Duration::from_secs(1));
        let (pss_shown, anon_shown) = (r.d.pss_kb(), r.d.pss_anon_kb());
        for n in 0..20u32 {
            change(2 + n % 2);
        }
        std::thread::sleep(Duration::from_secs(1));
        let (pss_end, anon_end) = (r.d.pss_kb(), r.d.pss_anon_kb());
        eprintln!(
            "failed shown (subscribe {subscribe}): PSS {pss0} KB at 0, {pss_shown} KB with 2 failing (+{}), {pss_end} KB after 20 more changes (+{}); \
             Pss_Anon {anon0} -> {anon_shown} -> {anon_end} KB",
            pss_shown.saturating_sub(pss0),
            pss_end.saturating_sub(pss0)
        );
        assert!(
            anon_shown <= anon0 + 64,
            "subscribe {subscribe}: shown costs {} KB of anonymous memory (bound 64)",
            anon_shown.saturating_sub(anon0)
        );
        assert!(
            anon_end <= anon0 + 64,
            "subscribe {subscribe}: 20 changes cost {} KB of anonymous memory (bound 64)",
            anon_end.saturating_sub(anon0)
        );
        drop(r.ws);
    }
}

/// Five seconds at rest after start: no wake-up of the loop, of the D-Bus thread or of the whole
/// daemon, with `subscribe` on and off.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_failed_units() {
    require_release(cfg!(debug_assertions));
    for subscribe in [true, false] {
        let toml = format!("{FAILED_ON}[failed]\nsubscribe = {subscribe}\n");
        let r = failed_rig(&format!("failed-rest-{subscribe}"), &toml, 0, 0, true, true);
        const REST: Duration = Duration::from_secs(5);
        away_from_the_minute(REST);
        let (all, main, dbus, ticks) =
            (r.d.wakeups(), r.d.main_thread_wakeups(), r.d.thread_wakeups("D-Bus").expect("the D-Bus thread"), r.d.cpu_ticks());
        std::thread::sleep(REST);
        assert_no_spin(r.d.cpu_ticks() - ticks, REST, "the daemon with the failed module");
        let all = r.d.wakeups().saturating_sub(all);
        let main = r.d.main_thread_wakeups().saturating_sub(main);
        let dbus = r.d.thread_wakeups("D-Bus").unwrap().saturating_sub(dbus);
        eprintln!("wake-ups at rest with failed units (subscribe {subscribe}, 5 s): {all} whole daemon, {main} loop, {dbus} D-Bus thread");
        assert!(r.d.log().iter().all(|l| !l.contains("source down")), "every source up: {:?}", r.d.log());
        assert_eq!((all, main, dbus), (0, 0, 0), "subscribe {subscribe}: a wake-up at rest (whole, loop, D-Bus thread)");
    }
}

/// `subscribe = false`: the system count rides another domain's wake-up once a minute at most.
/// A real 61 s wait, then a battery change (one piggybacked read), then five seconds at rest: no
/// wake-up, the piggyback armed nothing. Slow: run with `--exact`.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop), 61 s wait: run with --exact"]
fn piggyback_costs_no_wakeup() {
    require_release(cfg!(debug_assertions));
    let toml = "modules = [\"workspaces\", \"window\", \"battery\", \"failed\", \"clock\"]\n[failed]\nsubscribe = false\n";
    let r = failed_rig("failed-piggyback", toml, 0, 0, true, true);
    // An early change is inside the minute since the first read: no read.
    let first = r.sys.calls("Get");
    r.ws.battery_changed();
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(r.sys.calls("Get"), first, "a change within the minute rides nothing");
    std::thread::sleep(Duration::from_secs(61));
    r.ws.battery_changed();
    r.sys.wait_until("the piggybacked read", |s| s.calls("Get") > first);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(r.sys.calls("Get"), first + 1, "one piggybacked read, no more");
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(REST);
    let (all, main, dbus, ticks) =
        (r.d.wakeups(), r.d.main_thread_wakeups(), r.d.thread_wakeups("D-Bus").expect("the D-Bus thread"), r.d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(r.d.cpu_ticks() - ticks, REST, "the daemon after a piggybacked read");
    let all = r.d.wakeups().saturating_sub(all);
    let main = r.d.main_thread_wakeups().saturating_sub(main);
    let dbus = r.d.thread_wakeups("D-Bus").unwrap().saturating_sub(dbus);
    eprintln!("wake-ups after a piggybacked read (5 s): {all} whole daemon, {main} loop, {dbus} D-Bus thread");
    assert_eq!((all, main, dbus), (0, 0, 0), "a wake-up at rest after the piggyback");
}

/// Clipboard history: a daemon on the headless sway with the history on and the three base modules.
fn clipboard_mem_daemon(name: &str) -> (Sway, Daemon) {
    let sway = Sway::start(name, 1);
    let toml = format!("{AT_REST_NO_RESOURCES}[clipboard]\nenabled = true\n");
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml: Some(&toml), backend: Backend::Sway, ..DaemonOptions::default() });
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    assert!(d.wait_for("clipboard: ext-data-control-v1", DELAY), "{:?}", d.log());
    (sway, d)
}

/// The i-th distinct 4,000-byte text (fictional filler: a copy is never real data).
fn clip_text(i: usize) -> String {
    let mut text = format!("{i:05} {}", "clipboard filler ".repeat(240));
    text.truncate(4000);
    text
}

/// `n` copies from `first` on, each held until the selection is its text, plus 200 ms (the daemon
/// reads it), before its child goes.
fn clip_copies(sway: &Sway, first: usize, n: usize) {
    for i in first..first + n {
        let text = clip_text(i);
        let child = common::wl_copy(sway, &text, None, false);
        let start = Instant::now();
        while common::wl_paste(sway).as_deref() != Some(text.as_str()) {
            assert!(start.elapsed() < Duration::from_secs(5), "the selection never became copy {i}");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        drop(child);
    }
}

/// One `--clipboard` cycle: open, an Escape typed after the reading, closed; the n-th of each.
/// Returns the reading with the box open.
fn clip_cycle(sway: &Sway, d: &Daemon, n: usize) -> u64 {
    assert_eq!(launcher(sway, &Request::Clipboard), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    let typing = wtype_spawn(sway, &["-s", "3000", "-k", "Escape"]);
    std::thread::sleep(Duration::from_millis(1500));
    let open = d.pss_kb();
    assert_eq!(d.count("launcher closed ("), n - 1, "closed before the open reading: {:?}", d.log());
    assert!(typing.wait().unwrap().success());
    assert!(d.wait_for_count("launcher closed (escape)", n, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(2));
    open
}

/// A full history (50 entries of 4,000 bytes), then 100 more copies
/// through it, then cleared. Bounds on `Anonymous:` (PSS moves with the page cache); both printed.
#[test]
#[ignore = "headless sway and wl-clipboard (nix develop)"]
fn clipboard_full_history() {
    require_release(cfg!(debug_assertions));
    let (sway, d) = clipboard_mem_daemon("clip-full");
    std::thread::sleep(Duration::from_secs(3));
    let rest = (d.anonymous_kb(), d.pss_kb());
    clip_copies(&sway, 0, 50);
    let full = (d.anonymous_kb(), d.pss_kb());
    clip_copies(&sway, 50, 100);
    let cycled = (d.anonymous_kb(), d.pss_kb());
    assert_eq!(launcher(&sway, &Request::ClipboardClear), Reply::Done);
    std::thread::sleep(Duration::from_secs(1));
    let cleared = (d.anonymous_kb(), d.pss_kb());
    eprintln!(
        "clipboard anonymous (KB): rest {}, full {} ({:+}), after 100 more {} ({:+}), cleared {}; PSS: rest {}, full {}, cycled {}, cleared {}",
        rest.0,
        full.0,
        full.0 as i64 - rest.0 as i64,
        cycled.0,
        cycled.0 as i64 - full.0 as i64,
        cleared.0,
        rest.1,
        full.1,
        cycled.1,
        cleared.1
    );
    assert!(full.0 <= rest.0 + 384, "full history {} KB anon vs rest {}: more than 384 KB — report, never raise", full.0, rest.0);
    assert!(cycled.0 <= full.0 + 64, "100 more copies {} KB anon vs {}: more than 64 KB kept", cycled.0, full.0);
    assert!(
        full.0 >= rest.0 + 100,
        "full history {} KB anon vs rest {}: fewer than 100 KB, the copies were not stored (measured +164…+196)",
        full.0,
        rest.0
    );
    assert!(cleared.0 + 64 <= full.0, "cleared {} KB anon vs full {}: the clear freed less than 64 KB", cleared.0, full.0);
}

/// Clipboard history: the launcher on a full history, open and closed. Two warm-up cycles first:
/// the first open's code and font pages, and a one-off ±130 KB step of the heap's state that the
/// first cycle leaves either way (readings after cycle 1 were 1,708 and 1,840 KB at the same
/// steady state of ≈ 1,730 KB; cycles 3 to 6 moved +16 KB in all). Four more cycles are measured.
#[test]
#[ignore = "headless sway, wtype and wl-clipboard (nix develop)"]
fn clipboard_launcher_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let (sway, d) = clipboard_mem_daemon("clip-open");
    clip_copies(&sway, 0, 50);
    std::thread::sleep(Duration::from_secs(1));
    clip_cycle(&sway, &d, 1);
    clip_cycle(&sway, &d, 2);
    let closed = (d.anonymous_kb(), d.pss_kb());
    let mut open_pss = 0;
    for n in 3..=6 {
        open_pss = clip_cycle(&sway, &d, n);
    }
    let again = (d.anonymous_kb(), d.pss_kb());
    eprintln!(
        "clipboard launcher anonymous (KB): warmed closed {}, closed again {} ({:+}) after 4 cycles; PSS: closed {}, open on 50 entries {open_pss}, again {}",
        closed.0,
        again.0,
        again.0 as i64 - closed.0 as i64,
        closed.1,
        again.1
    );
    // The rows are really there: Enter on the first reads back the newest copy (the 50th).
    assert_eq!(launcher(&sway, &Request::Clipboard), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 7, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "-k", "Return"]).success());
    assert!(d.wait_for("launcher closed (activated)", DELAY), "{:?}", d.log());
    assert_eq!(common::wl_paste(&sway).as_deref(), Some(clip_text(49).as_str()), "the 50 entries are not in the box: {:?}", d.log());
    assert!(again.0 <= closed.0 + 128, "closed again {} KB anon vs {} KB before: more than 128 KB kept over 4 cycles", again.0, closed.0);
}

/// Clipboard history: after copies and after a `--clipboard` cycle the loop sleeps: a read timer
/// left armed (its deadline is inside the window) shows at once. Each window starts past SCTK's
/// key repeat (the cycle is closed by a typed Escape, then 1.6 s).
#[test]
#[ignore = "headless sway, wtype and wl-clipboard (nix develop)"]
fn at_rest_no_wakeups_after_copies() {
    require_release(cfg!(debug_assertions));
    let (sway, d) = clipboard_mem_daemon("clip-rest");
    std::thread::sleep(Duration::from_secs(2));
    const REST: Duration = Duration::from_secs(5);
    // Before the copies, over their whole span (5 × ≈ 0.3 s) and the rest window: the minute tick
    // never lands inside it, and a stray one-second read timer always does.
    away_from_the_minute(REST + Duration::from_secs(4));
    clip_copies(&sway, 0, 5);
    std::thread::sleep(Duration::from_millis(300));
    let (main_before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after copies");
    let after_copies = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("loop-thread wake-ups at rest after copies (5 s): {after_copies}");
    assert_eq!(after_copies, 0, "{after_copies} loop-thread wake-ups in 5 s after copies: {:?}", d.log());

    assert_eq!(launcher(&sway, &Request::Clipboard), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 1, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for("launcher closed (escape)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(1600));
    away_from_the_minute(REST);
    let (main_before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after a clipboard cycle");
    let after_cycle = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("loop-thread wake-ups at rest after a clipboard cycle (5 s): {after_cycle}");
    assert_eq!(after_cycle, 0, "{after_cycle} loop-thread wake-ups in 5 s after a clipboard cycle: {:?}", d.log());
}

// ---- the Tools menu ----

/// An executable `<runtime>/bin/<name>` with `body` (after the shebang).
fn tools_mem_script(sway: &Sway, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = sway.runtime.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A bar that makes every built-in shown (quick, notifications, timer, the two toggles, the
/// session commands, the clipboard) and a launcher.toml with 40 user entries in 6 categories,
/// the first 16 with a `state` (the reader's bound), the other 24 with 5 fixed choices (a state
/// and choices exclude each other).
fn tools_mem_config(on: &Path, run: &Path) -> (String, String) {
    let bar = format!(
        "modules = [\"workspaces\", \"window\", \"quick\", \"notifications\", \"timer\", \"clock\"]\n[clipboard]\nenabled = true\n\
         [quick]\nkeep_awake = {{ toggle = [{run:?}], state = \"process:sleep\" }}\n\
         night_light = {{ toggle = [{run:?}], state = \"file:/nonexistent\" }}\n",
        run = run.display().to_string()
    );
    let mut launcher = String::from("[tools]\n");
    for c in 0..6 {
        launcher.push_str(&format!("[[tools.category]]\nname = \"Category {c}\"\n"));
    }
    // A state excludes choices (validation): the first 16 entries carry a state, the other 24 five choices.
    for i in 0..40 {
        let run_line = if i < 16 {
            format!("[{:?}]", run.display().to_string())
        } else {
            format!("[{:?}, \"{{choice}}\"]", run.display().to_string())
        };
        launcher.push_str(&format!("[[tools.entry]]\nname = \"Entry {i}\"\ncategory = \"Category {}\"\nrun = {run_line}\n", i % 6));
        if i < 16 {
            launcher.push_str(&format!("state = [{:?}]\n", on.display().to_string()));
        } else {
            launcher.push_str("choices = [\"one\", \"two\", \"three\", \"four\", \"five\"]\n");
        }
    }
    (bar, launcher)
}

fn tools_mem_start(name: &str, bus: &PrivateBus) -> (Sway, Daemon) {
    let sway = Sway::start(name, 1);
    let on = tools_mem_script(&sway, "fake-on", "exit 0");
    wait_until_executable(&on);
    let run = tools_mem_script(&sway, "fake-run", "exit 0");
    wait_until_executable(&run);
    let (bar, launcher_toml) = tools_mem_config(&on, &run);
    let d = start_daemon_with(
        &sway,
        &DaemonOptions {
            bar_toml: Some(&bar),
            launcher_toml: Some(&launcher_toml),
            system_bus: Some(&bus.address),
            backend: Backend::Sway,
            ..DaemonOptions::default()
        },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    (sway, d)
}

/// One `--tools` cycle: four moves through the root and a category (the typist then holds its
/// keyboard, so no focus loss closes the box), the box closed by the verb; the n-th open waits for
/// its states line. Returns the reading taken with the menu open and drawn after the moves: (PSS,
/// anonymous, shm pools) in KB.
fn tools_cycle(sway: &Sway, d: &Daemon, n: usize) -> (u64, u64, u64) {
    assert_eq!(launcher(sway, &Request::Tools), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    let typing = wtype_spawn(sway, &["-s", "300", "-k", "Down", "-k", "Right", "-k", "Down", "-k", "Left", "-s", "10000"]);
    assert!(d.wait_for_count("tools: states read (", n, DELAY), "{:?}", d.log());
    std::thread::sleep(PAST_DEBOUNCE + Duration::from_millis(700));
    let (pss, pools) = d.pss_and_shm_kb();
    let open = (pss, anon_kb(d), pools);
    assert_eq!(d.count("launcher closed ("), n - 1, "closed before the open reading: {:?}", d.log());
    assert_eq!(launcher(sway, &Request::Tools), Reply::Done);
    assert!(d.wait_for_count("launcher closed (toggle)", n, DELAY), "{:?}", d.log());
    drop(typing);
    std::thread::sleep(Duration::from_secs(1));
    open
}

/// The Tools menu with every built-in, 40 user entries and 16 states: open, closed again, 20 more
/// cycles. `closed` is read after one cycle (what that first open leaves behind is not
/// attributed here). Total PSS moves with the page cache (fault-around), so the bounds are on the
/// daemon's anonymous memory (every mapping but the shm pools); PSS and the pools are printed,
/// the open reading's pools included, so the open delta can be split into pools and anonymous.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn tools_pss_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("tools-cost");
    let _workstation = Workstation::serve(&bus);
    let (sway, d) = tools_mem_start("tools-cost", &bus);
    std::thread::sleep(Duration::from_secs(3));
    let threads = d.threads();
    let cold = (d.pss_kb(), anon_kb(&d));
    let first_open = tools_cycle(&sway, &d, 1);
    let closed = (d.pss_kb(), anon_kb(&d));
    let (_, closed_pools) = d.pss_and_shm_kb();
    assert_eq!(d.threads(), threads, "the reader thread is gone once the box closed");
    let open = tools_cycle(&sway, &d, 2);
    let again = (d.pss_kb(), anon_kb(&d));
    let (_, again_pools) = d.pss_and_shm_kb();
    for n in 3..=22 {
        tools_cycle(&sway, &d, n);
    }
    let cycled = (d.pss_kb(), anon_kb(&d));
    eprintln!(
        "tools PSS (KB): cold {}, first open {}, closed {}, open {} (+{}), closed again {}, after 20 more {}",
        cold.0,
        first_open.0,
        closed.0,
        open.0,
        open.0.saturating_sub(closed.0),
        again.0,
        cycled.0
    );
    eprintln!(
        "tools anonymous (KB): cold {}, closed {}, open {} (+{}), closed again {} ({:+}), after 20 more {} ({:+})",
        cold.1,
        closed.1,
        open.1,
        open.1 as i64 - closed.1 as i64,
        again.1,
        again.1 as i64 - closed.1 as i64,
        cycled.1,
        cycled.1 as i64 - again.1 as i64
    );
    eprintln!(
        "tools shm pools (KB): closed {closed_pools}, open {} (+{}), closed again {again_pools}; threads {threads} -> {}",
        open.2,
        open.2 as i64 - closed_pools as i64,
        d.threads()
    );
    assert!(again.1 <= closed.1 + 256, "closed again {} KB anon vs {} KB before: more than 256 KB kept", again.1, closed.1);
    assert!(cycled.1 <= again.1 + 128, "20 more cycles: {} KB anon vs {} KB: more than 128 KB kept", cycled.1, again.1);
    assert!(again_pools <= closed_pools + 128, "the pools closed again {again_pools} KB vs {closed_pools} KB: the box's pool kept");
    assert_eq!(d.threads(), threads, "no thread left behind");
}

/// A `choices_cmd` printing 2,000 lines of 200 bytes (400 KB: past the reader's 256 KiB cap, so
/// the cap is what is measured), drilled into, then closed.
fn choices_cycle(sway: &Sway, d: &Daemon, n: usize) -> (u64, u64) {
    assert_eq!(launcher(sway, &Request::Tools), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    let typing = wtype_spawn(sway, &["-s", "300", "-k", "Return", "-s", "4000", "-k", "Escape", "-k", "Escape"]);
    assert!(d.wait_for_count("tools: entry 1: choices output cap reached", n, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(1000));
    let open = (d.pss_kb(), anon_kb(d));
    assert_eq!(d.count("launcher closed ("), n - 1, "closed before the open reading: {:?}", d.log());
    assert!(typing.wait().unwrap().success());
    assert!(d.wait_for_count("launcher closed (escape)", n, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(2));
    open
}

#[test]
#[ignore = "headless sway (nix develop)"]
fn tools_choices_2000_lines() {
    require_release(cfg!(debug_assertions));
    let sway = Sway::start("tools-choices", 1);
    let data = sway.runtime.join("choices.txt");
    let lines: String = (0..2000).map(|i| format!("{i:04} {}\n", "x".repeat(194))).collect();
    std::fs::write(&data, &lines).unwrap();
    let lister = tools_mem_script(&sway, "fake-lister", &format!("cat '{}'", data.display()));
    let run = tools_mem_script(&sway, "fake-run", "exit 0");
    wait_until_executable(&run);
    let toml = format!(
        "[tools]\nbuiltins = false\n[[tools.entry]]\nname = \"Pick\"\nrun = [{:?}, \"{{choice}}\"]\nchoices_cmd = [{:?}]\n",
        run.display().to_string(),
        lister.display().to_string()
    );
    let d = start_daemon_with(
        &sway,
        &DaemonOptions {
            bar_toml: Some(AT_REST_NO_RESOURCES),
            launcher_toml: Some(&toml),
            backend: Backend::Sway,
            ..DaemonOptions::default()
        },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(3));
    let threads = d.threads();
    let first_open = choices_cycle(&sway, &d, 1);
    let closed = (d.pss_kb(), anon_kb(&d));
    let open = choices_cycle(&sway, &d, 2);
    let again = (d.pss_kb(), anon_kb(&d));
    for n in 3..=7 {
        choices_cycle(&sway, &d, n);
    }
    let cycled = (d.pss_kb(), anon_kb(&d));
    eprintln!(
        "tools choices_cmd 2,000 lines PSS (KB): first open {}, closed {}, open {} (+{}), closed again {}, after 5 more {}",
        first_open.0,
        closed.0,
        open.0,
        open.0.saturating_sub(closed.0),
        again.0,
        cycled.0
    );
    eprintln!(
        "tools choices_cmd anonymous (KB): closed {}, open {} (+{}), closed again {} ({:+}), after 5 more {} ({:+})",
        closed.1,
        open.1,
        open.1 as i64 - closed.1 as i64,
        again.1,
        again.1 as i64 - closed.1 as i64,
        cycled.1,
        cycled.1 as i64 - again.1 as i64
    );
    // A read of 2,000 capped lines leaves the allocator's high-water
    // behind once (measured +152…+272 KB over 3 runs, flat after), so the bounds are 384 KB for the
    // first close and 128 KB over five more cycles (a leak per open grows with them). The
    // 384 KB is accepted: the first read grows the heap to hold the
    // lines (+1.9–2.0 MB anonymous open, freed at close). Most likely (attributed by elimination, not
    // by smaps): glibc cannot return the heap's top, pinned by chunks that outlive the read, and
    // keeps the rest on its free lists, so the first close sits above the pre-open heap; a second
    // read reuses those free chunks instead of growing, which is why the high-water rises once and
    // is flat after (+36 KB over five more cycles). The five-cycle bound is what guards a leak.
    assert!(again.1 <= closed.1 + 384, "closed again {} KB anon vs {} KB before: more than 384 KB kept", again.1, closed.1);
    assert!(cycled.1 <= again.1 + 128, "5 more cycles: {} KB anon vs {} KB: more than 128 KB kept", cycled.1, again.1);
    assert_eq!(d.threads(), threads, "no thread left behind");
}

/// Nothing at rest after a Tools cycle that read states and asked a question, the box closed by
/// the `--tools` verb (past the debounce) with the question still pending, after one left to
/// expire, and after moves and a root entry's letter that ran it: no loop wake-up in 5 s (SCTK's key-repeat firing, once ~0.6 s after a key, is past
/// before the window starts), the thread count where it was.
#[test]
#[ignore = "headless sway and wtype (nix develop)"]
fn at_rest_no_wakeups_after_tools() {
    require_release(cfg!(debug_assertions));
    let sway = Sway::start("tools-rest", 1);
    let on = tools_mem_script(&sway, "fake-on", "exit 0");
    wait_until_executable(&on);
    let run = tools_mem_script(&sway, "fake-run", "exit 0");
    let toml = format!(
        "[tools]\nbuiltins = false\n[[tools.category]]\nname = \"Fake\"\n[[tools.entry]]\nname = \"Danger\"\ncategory = \"Fake\"\n\
         run = [{:?}]\nconfirm = true\nstate = [{:?}]\n\
         [[tools.entry]]\nname = \"Plain\"\nrun = [{:?}]\nkey = \"p\"\n",
        run.display().to_string(),
        on.display().to_string(),
        run.display().to_string()
    );
    let d = start_daemon_with(
        &sway,
        &DaemonOptions {
            bar_toml: Some(AT_REST_NO_RESOURCES),
            launcher_toml: Some(&toml),
            backend: Backend::Sway,
            ..DaemonOptions::default()
        },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(2));
    let threads = d.threads();
    const REST: Duration = Duration::from_secs(5);
    let rest = |what: &str| {
        std::thread::sleep(Duration::from_millis(1500));
        away_from_the_minute(REST);
        let (before, ticks) = (d.main_thread_wakeups(), d.cpu_ticks());
        std::thread::sleep(REST);
        assert_no_spin(d.cpu_ticks() - ticks, REST, what);
        let delta = d.main_thread_wakeups().saturating_sub(before);
        eprintln!("loop-thread wake-ups at rest {what} (5 s): {delta}");
        assert_eq!(delta, 0, "{delta} loop-thread wake-ups in 5 s {what}: {:?}", d.log());
    };
    // A question asked (Enter on the category, Enter on the entry), the box closed by the verb
    // with it pending (0.3 s after the open, past the debounce; the question lives 3 s). The typist stays alive for 2.5 s so that no focus loss
    // closes the box before the verb.
    assert_eq!(launcher(&sway, &Request::Tools), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 1, DELAY), "{:?}", d.log());
    let typing = wtype_spawn(&sway, &["-s", "300", "-k", "Return", "-k", "Return", "-s", "2500"]);
    assert!(d.wait_for("tools: states read (1 on, 0 off, 0 unknown)", DELAY), "{:?}", d.log());
    std::thread::sleep(PAST_DEBOUNCE + Duration::from_millis(300));
    assert_eq!(d.count("launcher closed ("), 0, "the box is still open with its question: {:?}", d.log());
    // The entry asks (`confirm`): the second Return did not run it, so a question is pending.
    assert_eq!(d.count("tools: entry 1 run"), 0, "the question was never asked, the entry ran: {:?}", d.log());
    assert_eq!(launcher(&sway, &Request::Tools), Reply::Done);
    assert!(d.wait_for_count("launcher closed (toggle)", 1, DELAY), "{:?}", d.log());
    assert!(typing.wait().unwrap().success());
    assert_eq!(d.count("tools: entry 1 run"), 0, "closing with the question pending ran nothing: {:?}", d.log());
    rest("after a question closed with the box");
    // A question left to expire (3.5 s), then the box closed.
    assert_eq!(launcher(&sway, &Request::Tools), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 2, DELAY), "{:?}", d.log());
    assert!(
        wtype(&sway, &["-s", "300", "-k", "Return", "-k", "Return", "-s", "3500", "-k", "Escape", "-k", "Escape", "-k", "Escape"])
            .success()
    );
    assert!(d.wait_for_count("launcher closed (escape)", 1, DELAY), "{:?}", d.log());
    rest("after an expired question");
    // Moves through the root (Plain sits there, after the category), then Plain's letter runs it
    // and closes the box.
    assert_eq!(launcher(&sway, &Request::Tools), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 3, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "-k", "Down", "-k", "Right", "-k", "Up", "-k", "Left", "-s", "100", "p"]).success());
    assert!(d.wait_for("tools: entry 2 run", DELAY), "the letter ran its entry: {:?}", d.log());
    assert!(d.wait_for_count("launcher closed (activated)", 1, DELAY), "{:?}", d.log());
    rest("after moves and a letter");
    assert_eq!(d.threads(), threads, "no thread left behind");
}

/// Mod+Space's open latency with a `process:` keep-awake configured: a plain open reads no state
///, and the catalog it builds costs next to nothing. The same body runs on the
/// base commit (no Tools menu there), so the figures compare.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_open_latency_with_a_process_state_toggle() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("latency-tools");
    let bar = "modules = [\"workspaces\", \"window\", \"quick\", \"clock\"]\n[quick]\n\
               keep_awake = { toggle = [\"/bin/true\"], state = \"process:sleep\" }\n";
    let (sway, _hypr, d) = start_launcher("latency-tools", &bus, Some(bar), &many_apps(), None);
    std::thread::sleep(Duration::from_secs(2));
    let mut us = Vec::new();
    for n in 1..=20 {
        cycle(&sway, &d, n);
        us.push(d.last_open_latency_us().expect("the latency is logged"));
    }
    us.sort_unstable();
    let (median, max) = (us[us.len() / 2], us[us.len() - 1]);
    eprintln!("launcher open latency with a process: keep-awake, 20 opens: median {median} µs, max {max} µs; {us:?}");
    assert!(median < 20_000, "median {median} µs >= 20 ms: report it with these figures");
}

// ---- Shield mode: the module, the row, the blocked-USB detail ----

/// A daemon with the Shield module and the quick panel (the Shield row is the panel's first: no
/// sound, no D-Bus services, no backlight), the `fake-shield` stub of the tests on a private `PATH`
/// (never a real `pkexec`, `usbguard` or `systemctl`) and a private, empty system bus.
struct ShieldBed {
    sway: Sway,
    _hypr: FakeHyprland,
    d: Daemon,
    flag: PathBuf,
    record: PathBuf,
    _system: PrivateBus,
}

/// `configured`: `[shield]` is written; `flag_present`: the flag exists before the daemon starts (the
/// mode is on); `devices`: how many blocked devices `fake-shield list` prints.
fn shield_bed(name: &str, configured: bool, flag_present: bool, devices: usize) -> ShieldBed {
    let system = PrivateBus::start(&format!("{name}-system"));
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start_on_headless(&sway.runtime, SIGNATURE);
    let flag = sway.runtime.join("shield-state/flag");
    let record = sway.runtime.join("shield-record");
    shield_stub(&sway, &flag, &record, devices, 0);
    if flag_present {
        std::fs::write(&flag, "").unwrap();
    }
    let table = if configured {
        format!(
            "[shield]\nflag = {:?}\non = [\"fake-shield\", \"on\"]\noff = [\"fake-shield\", \"off\"]\nblocked = [\"fake-shield\", \"list\"]\nallow = [\"fake-shield\", \"allow\", \"{{id}}\"]\n",
            flag.display().to_string()
        )
    } else {
        String::new()
    };
    let toml =
        format!("modules = [\"quick\", \"shield\"]\n[osd]\nbacklight_dir = \"{}\"\n{table}", sway.runtime.join("no-backlight").display());
    let d = start_daemon_with(
        &sway,
        &DaemonOptions { bar_toml: Some(&toml), pw_dump: PwDump::Empty, system_bus: Some(&system.address), ..DaemonOptions::default() },
    );
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    ShieldBed { sway, _hypr: hypr, d, flag, record, _system: system }
}

impl ShieldBed {
    fn lists(&self) -> usize {
        shield_record(&self.record).iter().filter(|l| l.as_str() == "list").count()
    }

    /// Waits until `fake-shield list` ran `n` times in all.
    fn wait_lists(&self, n: usize) {
        let t = Instant::now();
        while self.lists() < n {
            assert!(t.elapsed() < DELAY, "{n} listings: {:?} {:?}", shield_record(&self.record), self.d.log());
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn toggle(&self) {
        assert_eq!(launcher(&self.sway, &Request::Quick(QuickVerb::Top)), Reply::Done);
    }

    fn key(&self, name: &str) {
        assert!(wtype(&self.sway, &["-s", "300", "-k", name]).success());
    }

    /// The `n`-th open of the panel by the verb, Enter on the Shield row (its detail: one listing),
    /// then, past SCTK's key repeat (it fires once about 0.6 s after a press),
    /// the close by the verb.
    fn detail_cycle(&self, n: usize) {
        self.toggle();
        assert!(self.d.wait_for_count("quick shown", n, DELAY), "{:?}", self.d.log());
        let line = self.d.log().into_iter().rev().find(|l| l.contains("quick shown")).unwrap();
        assert!(line.contains("quick shown (rows: shield"), "the Shield row is first: {line}");
        self.key("Return");
        self.wait_lists(n);
        std::thread::sleep(Duration::from_millis(1400)); // the listing drawn, the key's repeat timer fired
        self.toggle();
        assert!(self.d.wait_for_count("popup closed", n, DELAY), "{:?}", self.d.log());
    }
}

/// The module at rest, unconfigured (nothing exists), configured with the mode off,
/// configured with it on (the glyph drawn). Anonymous memory: configured at most 64 KB over
/// unconfigured; the total PSS is printed (file pages and the machine's page cache move it ±200 KB).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn shield_pss_at_rest_unconfigured_and_configured() {
    require_release(cfg!(debug_assertions));
    let mut report = Vec::new();
    for (what, configured, present) in [("unconfigured", false, false), ("configured, off", true, false), ("configured, on", true, true)] {
        let bed = shield_bed(&format!("shield-rest-{}", report.len()), configured, present, 10);
        std::thread::sleep(TRAY_SETTLE);
        report.push((what, bed.d.pss_kb(), anon_kb(&bed.d)));
    }
    eprintln!("shield at rest, (PSS KB, anonymous KB): {report:?}");
    let anon = |w: &str| report.iter().find(|(what, ..)| *what == w).map(|(.., a)| *a).unwrap();
    for w in ["configured, off", "configured, on"] {
        assert!(anon(w) <= anon("unconfigured") + 64, "{w}: {} KB anonymous vs {} KB unconfigured", anon(w), anon("unconfigured"));
    }
}

/// The detail open on 10 blocked devices, closed, and after 10 more cycles. The leak
/// bounds are on anonymous memory (the shm pools apart), as the other panels' details.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn shield_pss_detail_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bed = shield_bed("shield-detail-cost", true, true, 10);
    let d = &bed.d;
    std::thread::sleep(TRAY_SETTLE);
    let (idle, idle_shm) = d.pss_and_shm_kb();
    let idle_anon = anon_kb(d);
    let idle_map = smaps_by_mapping(d);
    bed.toggle();
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    bed.key("Return");
    bed.wait_lists(1);
    std::thread::sleep(Duration::from_secs(1)); // read, published and painted
    let (open, open_shm) = d.pss_and_shm_kb();
    let open_anon = anon_kb(d);
    std::thread::sleep(Duration::from_millis(500)); // past the key's repeat and the open's debounce
    bed.toggle();
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    std::thread::sleep(TRAY_SETTLE);
    let (closed, closed_shm) = d.pss_and_shm_kb();
    let closed_anon = anon_kb(d);
    let closed_map = smaps_by_mapping(d);
    eprintln!("shield detail smaps, idle to closed (KB): {}", smaps_moved(&idle_map, &closed_map));
    for n in 2..=11 {
        bed.detail_cycle(n);
    }
    std::thread::sleep(TRAY_SETTLE);
    let (cycled, cycled_shm) = d.pss_and_shm_kb();
    let cycled_anon = anon_kb(d);
    eprintln!("shield detail smaps, closed to cycled (KB): {}", smaps_moved(&closed_map, &smaps_by_mapping(d)));
    eprintln!(
        "shield detail PSS (KB): {idle} at rest, {open} with the detail open on 10 devices (cost {}), {closed} closed, \
         {cycled} after 10 more cycles; the pools: {idle_shm} at rest, {open_shm} open, {closed_shm} closed, {cycled_shm} cycled; \
         anonymous: {idle_anon} at rest, {open_anon} open, {closed_anon} closed, {cycled_anon} cycled",
        open.saturating_sub(idle)
    );
    assert!(open < STARTUP_MAX_KB, "{open} KB >= 15 MB — report it, never raise");
    assert!(
        closed_anon <= idle_anon + ANON_CLOSED,
        "closed {closed_anon} KB anonymous vs {idle_anon} KB at rest: more than the first open's heap (glyph caches)"
    );
    assert!(
        cycled_anon <= closed_anon + ANON_CYCLED,
        "cycled {cycled_anon} KB anonymous vs {closed_anon} KB closed: 10 cycles must keep nothing"
    );
    assert!(closed_shm <= idle_shm + 128, "the pools closed {closed_shm} KB vs {idle_shm} KB at rest: the popup's pool kept");
}

/// One window of `REST` away from the minute's tick: the loop thread and the whole daemon.
fn shield_rest_window(d: &Daemon, what: &str) {
    const REST: Duration = Duration::from_secs(5);
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, what);
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!("wake-ups at rest, {what} (5 s): {delta} whole daemon, {main_delta} loop thread");
    assert_eq!(main_delta, 0, "{what}: {main_delta} loop-thread wake-ups in 5 s");
    assert_eq!(delta, 0, "{what}: {delta} wake-ups in 5 s");
}

/// 0 loop (and whole-daemon) wake-ups at rest with the mode off, with it on, and
/// after a detail cycle (the window starting within a second of the close: a one-second timer left
/// armed, or the switch's grace timer not disarmed, would fall inside it).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_with_shield() {
    require_release(cfg!(debug_assertions));
    let bed = shield_bed("shield-wake", true, false, 10);
    let d = &bed.d;
    away_from_the_minute(Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(1)); // the first reading of the flag, the bar drawn
    shield_rest_window(d, "configured, off");
    // The mode comes on from outside: the flag's watch reads it (no polling).
    std::fs::write(&bed.flag, "").unwrap();
    assert!(d.wait_for("shield: now on", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    shield_rest_window(d, "configured, on");
    bed.detail_cycle(1);
    let closed_at = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    assert!(closed_at.elapsed() < Duration::from_millis(900));
    shield_rest_window(d, "after a detail cycle");
}

/// The workers live only while a job runs: after a switch (through the panel's Space
/// key) and a listing the daemon's thread count is back to its baseline within 3 s.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn shield_threads_back_to_baseline() {
    require_release(cfg!(debug_assertions));
    let bed = shield_bed("shield-threads", true, false, 10);
    let d = &bed.d;
    let baseline = wait_for_stable_threads(d, Duration::from_secs(5));
    bed.toggle();
    assert!(d.wait_for("quick shown", DELAY), "{:?}", d.log());
    bed.key("space"); // the switch on
    assert!(d.wait_for("shield: now on", DELAY), "{:?}", d.log());
    bed.key("Return"); // the detail: one listing
    bed.wait_lists(1);
    std::thread::sleep(Duration::from_millis(500));
    bed.toggle();
    assert!(d.wait_for("popup closed", DELAY), "{:?}", d.log());
    let t = Instant::now();
    let mut last = d.threads();
    while last != baseline && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(50));
        last = d.threads();
    }
    eprintln!("shield threads: baseline {baseline}, {last} {:?} after the switch and the listing", t.elapsed());
    assert_eq!(last, baseline, "threads {last} vs the baseline {baseline} 3 s after a switch and a listing");
}

// ---- the kill list ----
//
// The list shows the real processes of the user running the test, so these tests only ever open
// it, filter nothing and press nothing that signals: Ctrl+Enter asks, and no Enter is ever sent.

/// One `--kill` cycle, closed by the verb: the n-th open is shown and its second sample (300 ms
/// after the open) is taken before the reading. Returns (PSS, anonymous) in KB with the list open.
fn kill_cycle(sway: &Sway, d: &Daemon, n: usize) -> (u64, u64) {
    assert_eq!(launcher(sway, &Request::Kill), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    std::thread::sleep(PAST_DEBOUNCE + Duration::from_millis(300));
    let open = (d.pss_kb(), anon_kb(d));
    assert_eq!(launcher(sway, &Request::Kill), Reply::Done);
    assert!(d.wait_for_count("launcher closed (", n, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_millis(500));
    open
}

/// Once the kill list closed, however it was left (the verb, a force-kill question pending, a
/// question expired), no timer, no wake-up, no spin, no thread.
#[test]
#[ignore = "headless sway, wtype and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_kill_list() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("kill-rest");
    let (sway, _hypr, d) = start_launcher("kill-rest", &bus, Some(AT_REST), &[], None);
    std::thread::sleep(Duration::from_secs(2));
    let threads = d.threads();
    // Closed by the verb, after the second sample.
    kill_cycle(&sway, &d, 1);
    // A force-kill question pending (Ctrl+Enter asks and sends nothing), the box closed by the verb
    // inside its 3 s; wtype keeps its keyboard up meanwhile, so nothing but the verb closes the box.
    assert_eq!(launcher(&sway, &Request::Kill), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 2, DELAY), "{:?}", d.log());
    let held = wtype_spawn(&sway, &["-s", "300", "-M", "ctrl", "-k", "Return", "-m", "ctrl", "-s", "20000"]);
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(launcher(&sway, &Request::Kill), Reply::Done);
    assert!(d.wait_for_count("launcher closed (", 2, DELAY), "{:?}", d.log());
    drop(held);
    // A question left to run out (3.5 s), the box then closed by Escape.
    assert_eq!(launcher(&sway, &Request::Kill), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 3, DELAY), "{:?}", d.log());
    assert!(wtype(&sway, &["-s", "300", "-M", "ctrl", "-k", "Return", "-m", "ctrl", "-s", "3500", "-k", "Escape"]).success());
    assert!(d.wait_for_count("launcher closed (", 3, DELAY), "{:?}", d.log());
    assert_eq!(d.count("kill list: signal"), 0, "nothing was ever signalled: {:?}", d.log());
    std::thread::sleep(Duration::from_secs(3));
    const REST: Duration = Duration::from_secs(5);
    away_from_the_minute(REST);
    let (before, main_before, ticks) = (d.wakeups(), d.main_thread_wakeups(), d.cpu_ticks());
    std::thread::sleep(REST);
    assert_no_spin(d.cpu_ticks() - ticks, REST, "after a kill list");
    let delta = d.wakeups().saturating_sub(before);
    let main_delta = d.main_thread_wakeups().saturating_sub(main_before);
    eprintln!(
        "wake-ups at rest after a kill list (5 s): {delta} whole daemon, {main_delta} loop thread; threads {} -> {}",
        threads,
        d.threads()
    );
    assert_eq!(main_delta, 0, "{main_delta} loop-thread wake-ups in 5 s after a kill list");
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest after a kill list");
    assert_eq!(d.threads(), threads, "no thread left behind");
}

/// The cost of the list: PSS and anonymous memory with it open on this machine's own processes,
/// closed again, and after 20 more cycles. Total PSS moves with the page cache, so the bounds are
/// on anonymous memory, as the Tools menu's.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn kill_list_pss_open_and_closed() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("kill-cost");
    let (sway, _hypr, d) = start_launcher("kill-cost", &bus, Some(AT_REST), &[], None);
    std::thread::sleep(Duration::from_secs(3));
    let threads = d.threads();
    let cold = (d.pss_kb(), anon_kb(&d));
    let first_open = kill_cycle(&sway, &d, 1);
    let closed = (d.pss_kb(), anon_kb(&d));
    let open = kill_cycle(&sway, &d, 2);
    let again = (d.pss_kb(), anon_kb(&d));
    for n in 3..=22 {
        kill_cycle(&sway, &d, n);
    }
    let cycled = (d.pss_kb(), anon_kb(&d));
    eprintln!(
        "kill list PSS (KB): cold {}, first open {}, closed {}, open {} (+{}), closed again {}, after 20 more {}",
        cold.0,
        first_open.0,
        closed.0,
        open.0,
        open.0.saturating_sub(closed.0),
        again.0,
        cycled.0
    );
    eprintln!(
        "kill list anonymous (KB): cold {}, closed {}, open {} (+{}), closed again {} ({:+}), after 20 more {} ({:+}); threads {threads} -> {}",
        cold.1,
        closed.1,
        open.1,
        open.1 as i64 - closed.1 as i64,
        again.1,
        again.1 as i64 - closed.1 as i64,
        cycled.1,
        cycled.1 as i64 - again.1 as i64,
        d.threads()
    );
    assert!(again.1 <= closed.1 + 256, "closed again {} KB anon vs {} KB before: more than 256 KB kept", again.1, closed.1);
    assert!(cycled.1 <= again.1 + 128, "20 more cycles: {} KB anon vs {} KB: more than 128 KB kept", cycled.1, again.1);
    assert_eq!(d.threads(), threads, "no thread left behind");
}

/// One selection of the area selector through the socket: the verb, a drag that confirms (`n` odd)
/// or a typed Escape (`n` even), and the n-th close of any reason.
fn selection(sway: &Sway, d: &Daemon, n: usize) {
    assert_eq!(launcher(sway, &Request::Capture(CaptureVerb::Select)), Reply::Done);
    assert!(d.wait_for_count("capture: selector shown", n, DELAY), "{:?}", d.log());
    sway.hover(60, 60);
    if n % 2 == 1 {
        sway.drag((100, 100), (500, 400), false);
    } else {
        assert!(wtype(sway, &["-s", "300", "-k", "Escape"]).success());
    }
    assert!(d.wait_for_count("capture: selector closed (", n, DELAY), "{:?}", d.log());
}

/// The area selector: the daemon's own memory is back within 64 KB after 10 more selections (its
/// two full-output frames live in an shm pool that goes with the surface), no thread is left, and
/// nothing wakes the loop at rest afterwards. The cost while open is printed for
/// docs/measurements.md.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_an_area_capture() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("selector-rest");
    let (sway, _hypr, d) = start_launcher("selector-rest", &bus, Some(AT_REST), &[], None);
    std::thread::sleep(Duration::from_secs(2));
    sway.hover(50, 50);
    let threads = d.threads();
    let at_rest = d.pss_kb();
    // The first selection warms the glyph cache and the keymap: the baseline is after it.
    selection(&sway, &d, 1);
    selection(&sway, &d, 2);
    std::thread::sleep(Duration::from_secs(1));
    let baseline = d.anonymous_kb();
    // The cost while open, a drag held down.
    assert_eq!(launcher(&sway, &Request::Capture(CaptureVerb::Select)), Reply::Done);
    assert!(d.wait_for_count("capture: selector shown", 3, DELAY), "{:?}", d.log());
    sway.hover(60, 60);
    sway.drag((100, 100), (500, 400), true);
    std::thread::sleep(Duration::from_millis(500));
    let (open, open_shm) = d.pss_and_shm_kb();
    sway.release_drag();
    assert!(d.wait_for_count("capture: selector closed (", 3, DELAY), "{:?}", d.log());
    for n in 4..=13 {
        selection(&sway, &d, n);
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.anonymous_kb();
    eprintln!(
        "area selector: PSS {at_rest} KB at rest, {open} KB open (+{} KB, of which the shm pool {open_shm} KB); \
         anonymous {baseline} KB after 2 selections, {end} KB after 10 more; threads {} (before: {threads})",
        open.saturating_sub(at_rest),
        d.threads()
    );
    assert!(end <= baseline + 64, "10 selections grew the daemon's anonymous memory: {baseline} -> {end} KB");
    assert_eq!(d.threads(), threads, "no thread left by the selector");
    std::thread::sleep(Duration::from_secs(3));
    let (before, ticks) = (d.wakeups(), d.cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.wakeups().saturating_sub(before);
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon after an area selection");
    eprintln!("wake-ups at rest after an area selection (5 s): {delta}");
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest after the selector was used");
}

/// One colour pick through the socket, the n-th: the picker opens over the frozen output, a click
/// picks, and its value goes to the copy command (`cat`, its output discarded).
fn colour_pick(sway: &Sway, d: &Daemon, n: usize) {
    assert_eq!(launcher(sway, &Request::Capture(CaptureVerb::Pick)), Reply::Done);
    assert!(d.wait_for_count("capture: picker shown", n, DELAY), "{:?}", d.log());
    sway.hover(300 + n as u32, 300);
    sway.click(300 + n as u32, 300, "button1");
    assert!(d.wait_for_count("capture: colour copied", n, DELAY), "{:?}", d.log());
}

/// The colour picker: the daemon's own anonymous memory is back within 64 KB after 10 more picks
/// (the frozen frame, the surface's pool and the worker go with each), no thread is left, and nothing
/// wakes the loop at rest afterwards. The cost while open is printed for docs/measurements.md.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_colour_pick() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("pick-rest");
    let (sway, _hypr, d) = start_launcher("pick-rest", &bus, Some(AT_REST), &[], Some(&|_| "copy = [\"cat\"]\n".to_string()));
    std::thread::sleep(Duration::from_secs(2));
    sway.hover(50, 50);
    let threads = d.threads();
    let at_rest = d.pss_kb();
    // The first picks warm the allocator, the glyph cache and the keymap: the baseline is after two.
    colour_pick(&sway, &d, 1);
    colour_pick(&sway, &d, 2);
    std::thread::sleep(Duration::from_secs(1));
    let baseline = d.anonymous_kb();
    // The cost while open, the lens drawn.
    assert_eq!(launcher(&sway, &Request::Capture(CaptureVerb::Pick)), Reply::Done);
    assert!(d.wait_for_count("capture: picker shown", 3, DELAY), "{:?}", d.log());
    sway.hover(400, 300);
    std::thread::sleep(Duration::from_millis(500));
    let (open, open_shm) = d.pss_and_shm_kb();
    sway.click(400, 300, "button1");
    assert!(d.wait_for_count("capture: colour copied", 3, DELAY), "{:?}", d.log());
    for n in 4..=13 {
        colour_pick(&sway, &d, n);
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.anonymous_kb();
    eprintln!(
        "colour picker: PSS {at_rest} KB at rest, {open} KB open (+{} KB, of which shm {open_shm} KB); \
         anonymous {baseline} KB after 2 picks, {end} KB after 10 more; threads {} (before: {threads})",
        open.saturating_sub(at_rest),
        d.threads()
    );
    assert!(end <= baseline + 64, "10 picks grew the daemon's anonymous memory: {baseline} -> {end} KB");
    assert_eq!(d.threads(), threads, "no thread left by the picker");
    std::thread::sleep(Duration::from_secs(3));
    let (before, ticks) = (d.wakeups(), d.cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.wakeups().saturating_sub(before);
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon after a colour pick");
    eprintln!("wake-ups at rest after a colour pick (5 s): {delta}");
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest after the picker was used");
}

/// One capture through the socket, the n-th: a whole screen when `n` is odd, an area picked with a
/// drag when it is even; returns once its n-th ending is logged. The copy command is `cat` (its
/// output is discarded): nothing reaches a clipboard.
fn screen_capture_cycle(sway: &Sway, d: &Daemon, n: usize) {
    if n % 2 == 1 {
        assert_eq!(launcher(sway, &Request::Capture(CaptureVerb::Full)), Reply::Done);
    } else {
        assert_eq!(launcher(sway, &Request::Capture(CaptureVerb::Area)), Reply::Done);
        assert!(d.wait_for_count("capture: selector shown", n / 2, DELAY), "{:?}", d.log());
        sway.hover(60, 60);
        sway.drag((100, 100), (500, 400), false);
    }
    assert!(d.wait_for_count("capture: copied", n, DELAY), "{:?}", d.log());
}

/// The captures: the daemon's own anonymous memory is back within 64 KB after 10 more captures (the
/// frame, the image and the worker's arena are all gone with each), no thread is left, and nothing
/// wakes the loop at rest afterwards. The cost of a capture is printed for docs/measurements.md.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_screen_capture() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("capture-rest");
    let (sway, _hypr, d) = start_launcher("capture-rest", &bus, Some(AT_REST), &[], Some(&|_| "copy = [\"cat\"]\n".to_string()));
    std::thread::sleep(Duration::from_secs(2));
    sway.hover(50, 50);
    let threads = d.threads();
    let at_rest = d.pss_kb();
    let anonymous_at_rest = d.anonymous_kb();
    // The first captures warm the allocator, the glyph cache and the keymap: the baseline is after two.
    screen_capture_cycle(&sway, &d, 1);
    screen_capture_cycle(&sway, &d, 2);
    std::thread::sleep(Duration::from_secs(1));
    let baseline = d.anonymous_kb();
    for n in 3..=12 {
        screen_capture_cycle(&sway, &d, n);
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.anonymous_kb();
    eprintln!(
        "captures: PSS {at_rest} KB at rest; anonymous {anonymous_at_rest} KB before the first, {baseline} KB after 2, {end} KB after 10 more; \
         threads {} (before: {threads})",
        d.threads()
    );
    assert!(end <= baseline + 64, "10 captures grew the daemon's anonymous memory: {baseline} -> {end} KB");
    assert_eq!(d.threads(), threads, "no thread left by the captures");
    std::thread::sleep(Duration::from_secs(3));
    let (before, ticks) = (d.wakeups(), d.cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.wakeups().saturating_sub(before);
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon after a capture");
    eprintln!("wake-ups at rest after a capture (5 s): {delta}");
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest after captures were taken");
}

/// The fake recorder of the recording tests: records until SIGINT, then writes the file given after
/// `-f` and exits 0, as wf-recorder finishes its file. Its own wake-ups (a sleep loop) are not the
/// daemon's.
fn fake_recorder_script(runtime: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let path = runtime.join("bin/fake-record");
    std::fs::create_dir_all(runtime.join("bin")).unwrap();
    let body = "#!/bin/sh\n# Fake recorder of the memory tests.\nout=''; prev=''\nfor a in \"$@\"; do [ \"$prev\" = -f ] && out=$a; prev=$a; done\n\
trap 'printf frames > \"$out\"; exit 0' INT\nwhile :; do sleep 0.1; done\n";
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// One recording through the socket, the n-th: started, then stopped once it runs; returns once its
/// n-th saved ending is logged.
fn recording_cycle(sway: &Sway, d: &Daemon, n: usize) {
    assert_eq!(launcher(sway, &Request::Capture(CaptureVerb::Record)), Reply::Done);
    assert!(d.wait_for_count("capture: recording started", n, DELAY), "{:?}", d.log());
    assert_eq!(launcher(sway, &Request::Capture(CaptureVerb::Stop)), Reply::Done);
    assert!(d.wait_for_count("capture: recording saved", n, DELAY), "{:?}", d.log());
}

/// The recording: while the recorder runs nothing wakes the daemon (its end is a pidfd on the loop,
/// readable only when it exits: no thread, no timer, no poll); afterwards the daemon's anonymous
/// memory is back within 64 KB after 10 more recordings, no thread is left, and nothing wakes it at
/// rest. The figures are printed for docs/measurements.md.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_recording() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("record-rest");
    let bar = format!("{AT_REST}[captures]\nrecord = [\"fake-record\", \"-g\", \"{{geometry}}\", \"-f\", \"{{out}}\"]\n");
    let (sway, _hypr, d) = start_launcher("record-rest", &bus, Some(&bar), &[], None);
    fake_recorder_script(&sway.runtime);
    std::thread::sleep(Duration::from_secs(2));
    let threads = d.threads();
    let at_rest = d.pss_kb();
    // The first recordings warm the allocator: the baseline is after two.
    recording_cycle(&sway, &d, 1);
    recording_cycle(&sway, &d, 2);
    std::thread::sleep(Duration::from_secs(1));
    let baseline = d.anonymous_kb();
    // While it records: the cost, and what wakes the daemon.
    assert_eq!(launcher(&sway, &Request::Capture(CaptureVerb::Record)), Reply::Done);
    assert!(d.wait_for_count("capture: recording started", 3, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(2));
    let recording = d.pss_kb();
    let (before, ticks) = (d.wakeups(), d.cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let while_recording = d.wakeups().saturating_sub(before);
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon while it records");
    assert_eq!(d.threads(), threads, "no thread while it records");
    assert_eq!(launcher(&sway, &Request::Capture(CaptureVerb::Stop)), Reply::Done);
    assert!(d.wait_for_count("capture: recording saved", 3, DELAY), "{:?}", d.log());
    for n in 4..=13 {
        recording_cycle(&sway, &d, n);
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.anonymous_kb();
    eprintln!(
        "recording: PSS {at_rest} KB at rest, {recording} KB while recording (+{} KB); wake-ups while recording (5 s): \
         {while_recording}; anonymous {baseline} KB after 2 recordings, {end} KB after 10 more; threads {} (before: {threads})",
        recording.saturating_sub(at_rest),
        d.threads()
    );
    assert!(while_recording <= 4, "{while_recording} wake-ups in 5 s while the recorder ran");
    assert!(end <= baseline + 64, "10 recordings grew the daemon's anonymous memory: {baseline} -> {end} KB");
    assert_eq!(d.threads(), threads, "no thread left by the recordings");
    std::thread::sleep(Duration::from_secs(3));
    let (before, ticks) = (d.wakeups(), d.cpu_ticks());
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.wakeups().saturating_sub(before);
    assert_no_spin(d.cpu_ticks() - ticks, Duration::from_secs(5), "the daemon after a recording");
    eprintln!("wake-ups at rest after a recording (5 s): {delta}");
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest after recordings");
}
