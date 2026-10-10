//! Spec budget with every module of delivery 3: startup PSS < 15 MB (firm),
//! < 30 MB at steady state and no growth, under a busy compositor and a
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
//! `nix develop -c cargo test --release -p alpheus-demon --test memory -- --ignored --test-threads=1`

mod common;

use alpheus_commun::launcher_ipc::{DmenuFlags, Reply, Request, encode_request, socket_path};
use common::mpris::{FakePlayer, Spec as PlayerSpec};
use common::sni::{FakeItem, Spec};
use common::{
    AG06_SINK, Daemon, DaemonOptions, FakeHyprland, Peer, PrivateBus, PwDump, SIGNATURE, Sway, Workstation, assert_no_spin, fake_source,
    launcher, pw_dump_send, start_daemon_with, volume_delta, write_atomically, wtype, wtype_spawn,
};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DELAY: Duration = Duration::from_secs(10);
/// Startup PSS budget: firm (Mae, 2026-10-04, Q13). Never raised: a figure
/// above it is reported to her with `startup_pss_per_source`'s breakdown.
const STARTUP_MAX_KB: u64 = 15 * 1024;

/// Refuses to measure PSS on a debug build (module doc: roughly 2x
/// inflated, in either direction against a budget tuned for release).
/// Takes the flag rather than reading `cfg!` itself so the decision stays
/// a plain, testable function.
fn require_release(debug_assertions: bool) {
    assert!(
        !debug_assertions,
        "run with --release: these PSS budgets are tuned for the release binary (debug roughly doubles it) — \
         nix develop -c cargo test --release -p alpheus-demon --test memory -- --ignored --test-threads=1"
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
/// population spec § 4 asks startup and rest to be measured against.
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
/// on `bus` (the same check as task 11's headless tests).
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
/// desktop's catalog (Mae's has about that many). Synthetic only (no capture).
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
/// sctk's repeat timer (task 12, note 2) — what toggle cycles never touch.
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
    assert!(median < 20_000, "median {median} µs >= 20 ms: report to Mae with these figures");
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
    assert!(open <= closed + 6 * 1024, "open costs {} KB > 6 MB: report to Mae, never absorb", open - closed);
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

/// Sources spec § 7: PSS with no source and with a 300-line source, open latency with it (the box
/// shows at once, the entries follow), and 0 wake-ups at rest after. Printed for the README; the
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
        assert!(us[us.len() / 2] < 20_000, "median open latency {} us >= 20 ms: report to Mae", us[us.len() / 2]);
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
    // after the keyboard is released (task 12, note 2); it must fire once
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

/// Controller carry (task 1 review): a dmenu of 100,000 short lines, open
/// PSS and its value 1 s after cancel — printed, no assertion (the budgets
/// above already cover growth and the 6 MB open margin on a realistic
/// catalog; this is the worst-case dmenu size, reported for the README).
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
    // plan 1 measured ≈ 12 MB here; the D-Bus source and the sound source
    // must fit in what is left (task 16, budget decisions).
    assert!(initial < STARTUP_MAX_KB, "startup: PSS {initial} KB >= 15 MB: run startup_pss_per_source and report to Mae, never raise");
    let mut n = 0;
    pump(&hypr, Duration::from_secs(10), &mut n); // warm-up
    // A first unplug/replug cycle before `baseline` (see plan 1, task 13):
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
    assert!(end < 30 * 1024, "PSS {end} KB >= 30 MB");
    assert!(end <= baseline + 1024, "growth: {baseline} -> {end} KB");
    const RETURN_MARGIN_KB: u64 = 2 * 1024;
    assert!(end.abs_diff(initial) <= RETURN_MARGIN_KB, "PSS does not come back close to the start: {initial} KB -> {end} KB");
    assert!(d.log().iter().all(|l| !l.contains("sound source down")), "pw-dump kept up: {:?}", d.log());
}

/// Startup PSS per source, against the firm 15 MB budget: plan 1's modules
/// alone, then each new source on its own, then everything. Printed for
/// the task report (step 6): which source costs what.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn startup_pss_per_source() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("per-source");
    let _workstation = Workstation::serve(&bus);
    let cases = [
        ("plan 1", "\"workspaces\", \"window\", \"clock\""),
        ("+ sound", "\"workspaces\", \"window\", \"volume\", \"clock\""),
        ("+ D-Bus", "\"workspaces\", \"window\", \"network\", \"bluetooth\", \"battery\", \"clock\""),
        ("+ resources", "\"workspaces\", \"window\", \"resources\", \"clock\""),
        ("+ launcher", "\"launcher\", \"workspaces\", \"window\", \"clock\""),
        (
            "everything",
            "\"launcher\", \"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \"resources\", \"clock\"",
        ),
    ];
    let mut report = Vec::new();
    for (i, (what, modules)) in cases.iter().enumerate() {
        let toml = format!("modules = [{modules}]\n");
        let (_sway, _hypr, d) = start(&format!("per-source-{i}"), 3, &bus, None, Some(&toml), PwDump::Silent);
        std::thread::sleep(Duration::from_secs(3)); // as in pss_under_30mb_and_stable
        report.push((*what, d.pss_kb()));
    }
    eprintln!("startup PSS per source (KB): {report:?}");
    for (what, kb) in &report {
        assert!(*kb < STARTUP_MAX_KB, "{what}: {kb} KB >= 15 MB — report to Mae with these figures, never raise: {report:?}");
    }
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
    // Plan 1 observed 0 to 2; a 1 Hz tick would add ≈ 5.
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
    // per tick with 1 output (2026-10-05, release, this machine: 15, 15,
    // 17, 15 over 4 runs). ≥ 4: it really ticks every second; ≤ 20: never
    // faster — halving the period (the `ToDuration(period / 2)` mutation)
    // measured 27, comfortably outside this range. The old 4..=12 could
    // no longer hold the steady ~15-17 and was widened from measurements,
    // not loosened to paper over a bug.
    assert!((4..=20).contains(&delta), "{delta} loop wake-ups in 5 s at interval = 1");
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
        // "down" log line itself is rate-limited (dbus-fixes-report, M2):
        // only the first outage ever, or one after a SUCCEEDED session,
        // is logged (`first || succeeded` in `run_supervised`); a session
        // counts as succeeded only once it has run for `STABLE_DURATION`
        // (10 s) — this private bus has no NetworkManager/UPower/BlueZ to
        // generate a second publication, so the reconnected session must
        // simply stay up that long before the next restart, or its outage
        // would be silently folded into the previous "down" count instead
        // of logging a fresh line (the brief's "5 s is plenty" undershoots
        // STABLE_DURATION; adapted here to the real rate-limiting rule).
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
    } else if into + window.as_secs() + 2 >= 60 {
        std::thread::sleep(Duration::from_secs(60 - into + 2));
    }
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

/// The tray alone then the clock (task 11's convention): the first icon is
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
    assert!(kb < STARTUP_MAX_KB, "{kb} KB >= 15 MB with the tray populated — report to Mae, never raise: {kb}");
}

/// Themes task 5: the inotify watch adds nothing at rest, even right after a reload. Read in the
/// README as "0 wake-ups at rest" with the watch in place.
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

/// Themes delivery 1: 50 reloads (two presets in turn, three bars repainted each time) leave no
/// growth behind. Printed for the README; budget 768 KB (a leak of 64 KB per reload makes 3.2 MB).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn theme_reloads_no_growth() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("theme-reloads");
    let toml = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let (sway, _hypr, d) = start("theme-reloads", 3, &bus, None, Some(toml), PwDump::Silent);
    let config = sway.runtime.join("config/alpheus");
    let reload = |n: usize| {
        let theme = if n % 2 == 0 { "tokyo-night" } else { "gruvbox" };
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

/// Shapes delivery: every style, in the order the tests switch to them (`flat` last, so the
/// switch back from translucent is measured too).
const STYLES: [&str; 6] = ["islands", "powerline", "floating", "underline", "translucent", "flat"];

/// Shapes task 7: no style adds a wake-up at rest, `floating`'s reconfigured surfaces included.
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

/// Shapes delivery: PSS at start under each style, every module, 3 outputs. A style costs its
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
        assert!(*kb < STARTUP_MAX_KB, "{style}: {kb} KB >= 15 MB — report to Mae, never raise: {report:?}");
        assert!(*kb <= flat + 512, "{style}: {kb} KB vs flat {flat} KB: {report:?}");
    }
}

/// Shapes delivery: 48 style switches (every style 8 times; floating reconfigures every bar, the
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
/// itself (fix wave A).
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
    assert!(three < STARTUP_MAX_KB, "{three} KB >= 15 MB — report to Mae, never raise");
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

/// Notifications delivery: the module serving with nothing, a 50-entry history, 4 bubbles shown,
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
    assert!(shown < STARTUP_MAX_KB, "{shown} KB >= 15 MB — report to Mae, never raise");
    assert!(module_cost <= 1024, "the server and the bell cost {module_cost} KB (≤ 1 MB)");
    assert!(history_cost <= 512, "50 entries cost {history_cost} KB (capped strings: ≤ 512 KB; spec § 7 computes ≈ 125 KB at worst)");
    assert!(bubbles_cost <= 2048, "4 bubbles cost {bubbles_cost} KB (their own pool, two buffers: ≤ 2 MB)");
    assert!(closed <= history + 256, "closed: {closed} KB vs {history} KB before them — the surface and its pool must go");
}

/// Notifications delivery: at rest with a sticky critical bubble shown over a 50-entry history, then
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

/// The OSD delivery: the module on with nothing shown (its cost over the same bar without it), one
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
    assert!(shown < STARTUP_MAX_KB, "{shown} KB >= 15 MB — report to Mae, never raise");
    assert!(module_cost <= 256, "the OSD at rest costs {module_cost} KB (a sound state and three instants: ≤ 256 KB)");
    assert!(pill_cost <= 1024, "one pill costs {pill_cost} KB (its own pool, two buffers of one row: ≤ 1 MB)");
    assert!(gone <= idle + 256, "gone: {gone} KB vs {idle} KB before it — the surface and its pool must go");
}

/// The OSD delivery: once a pill went, no timer, no wake-up, no spin.
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
