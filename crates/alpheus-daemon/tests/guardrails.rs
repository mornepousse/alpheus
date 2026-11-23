//! Guardrails from the framing document for the daemon, checked
//! mechanically: it only ever runs the workspace command (`wm-set
//! workspace …` by default), only ever reads the compositor (four fixed
//! Hyprland requests, the niri stream), only opens the compositor's
//! sockets in `src/sources/`, never spawns a process directly, and the
//! forbidden words appear nowhere in its sources.

use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::exec::{CaptureLimits, Captured, ExecError, Executor, Output, Stream};
use alpheus_daemon::actions::{Button, command, run};
use alpheus_daemon::appearance::SETTING_RULE;
use alpheus_daemon::compositor::{CompositorState, Toplevel, Window, Workspace};
use alpheus_daemon::config::{BarConfig, ModuleId, QuickConfig, ToggleConfig};
use alpheus_daemon::keys::Key;
use alpheus_daemon::launcher::config::{LauncherConfig, SourceConfig};
use alpheus_daemon::launcher::desktop::{DesktopAction, DesktopEntry};
use alpheus_daemon::launcher::history::History;
use alpheus_daemon::launcher::matching::Mode;
use alpheus_daemon::launcher::model::{Activation, Launcher, Outcome};
use alpheus_daemon::launcher::run::{RunContext, SYSTEMD_RUN, Spawner, perform};
use alpheus_daemon::launcher::scan::Catalog;
use alpheus_daemon::launcher::sources::{Entries, SourceEntry};
use alpheus_daemon::notify::NotifyView;
use alpheus_daemon::power::{Power, PowerAction};
use alpheus_daemon::privacy::{Capture, Indicator};
use alpheus_daemon::quick::{self, Backlight, Focus, Live, Quick, QuickAction};
use alpheus_daemon::session::{self, Session};
use alpheus_daemon::session_bus::bus::{Call, DBUS_NAME_FLAG_DO_NOT_QUEUE, Method as TrayMethod, NOTIFICATIONS_NAME_FLAGS, message};
use alpheus_daemon::session_bus::{
    APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, NOTIFICATIONS_NAME, PORTAL_NAME, PORTAL_PATH, SETTINGS_INTERFACE, SYSTEMD_MANAGER,
    SYSTEMD_NAME, SYSTEMD_PATH, WATCHER_NAME,
};
use alpheus_daemon::sources::dbus::bt_act::{AGENT_PATH, ActMethod, CAPABILITY};
use alpheus_daemon::sources::dbus::dbusrs_bus::{LOGIN1_RULE, MATCH_RULES, Method, WIFI_RULES};
use alpheus_daemon::sources::{hypr::Request, mango, niri::REQUEST, sway};
use alpheus_daemon::state::{Bar, Msg};
use alpheus_daemon::system::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn workspace(name: &str, output: &str, windows: u16, visible: bool) -> Workspace {
    Workspace { name: name.into(), reference: name.into(), output: output.into(), windows, urgent: false, visible, focused: visible }
}

#[test]
fn every_click_only_runs_the_workspace_command() {
    let mut b = Bar::new(BarConfig::default());
    b.apply(Msg::Compositor(Some(CompositorState {
        workspaces: vec![workspace("1", "eDP-1", 1, true), workspace("2", "eDP-1", 0, false), workspace("3", "HDMI-A-1", 2, true)],
        window: Some(Window { class: "firefox".into(), title: "t".into() }),
        toplevels: vec![],
        ..CompositorState::default()
    })));
    let fake = FakeExecutor::new();
    for output in ["eDP-1", "HDMI-A-1"] {
        let g = b.grid(output, 200);
        for z in g.zones() {
            for bt in [Button::Left, Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
                if let Some(argv) = command(&b, output, &z.target, bt) {
                    run(&fake, &argv);
                }
            }
        }
    }
    let calls = fake.calls();
    assert!(calls.len() >= 4, "{calls:?}");
    for c in &calls {
        assert!(c.starts_with("wm-set workspace "), "command outside the list: {c}");
        assert_eq!(c.split(' ').count(), 4, "{c}");
        assert!(!c.to_lowercase().contains("dpms"), "{c}");
    }
}

fn full_bar() -> Bar {
    let mut b = Bar::new(BarConfig::default());
    b.apply(Msg::Compositor(Some(CompositorState {
        workspaces: vec![workspace("1", "eDP-1", 1, true), workspace("2", "eDP-1", 0, false), workspace("3", "HDMI-A-1", 2, true)],
        window: Some(Window { class: "firefox".into(), title: "t".into() }),
        toplevels: vec![],
        ..CompositorState::default()
    })));
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 50, muted: false }), ..SoundState::default() })));
    b.apply(Msg::Network(Some(NetState::Online { link: Link::Wifi, vpns: 0, limited: false })));
    b.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(72) }] })));
    b.apply(Msg::Battery(Some(PowerState { laptop: Some(Battery { percent: 60, charge: Charge::Discharging }), peripherals: vec![] })));
    b.apply(Msg::Resources(Sample { cpu: Some(12), memory: Some(61), temperature: Some(60), ..Sample::default() }));
    b.apply(Msg::Notifications(NotifyView { unread: 2, dnd: false, serving: true }));
    // A shown microphone, so the privacy module has a zone (privacy spec § 6).
    let mic = Capture { mic: Indicator { names: vec!["firefox".into()], unnamed: false }, ..Capture::default() };
    b.privacy_at(&Msg::Sound(Some(SoundState { capture: Some(mic), ..SoundState::default() })), Instant::now());
    b
}

/// Every zone of a bar where every module is shown, every button, with the quick panel and without
/// it: only the commands of the allowlist, and each of them reached (the mute on the bar without the
/// panel, whose right click is the TUI tab's).
#[test]
fn every_click_on_a_full_bar_stays_on_the_allowlist() {
    let mut without_quick = full_bar();
    without_quick.config.modules.retain(|m| *m != ModuleId::Quick);
    let mut calls = Vec::new();
    for b in [full_bar(), without_quick] {
        let fake = FakeExecutor::new();
        for output in ["eDP-1", "HDMI-A-1"] {
            let g = b.grid(output, 240);
            for z in g.zones() {
                for bt in [Button::Left, Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
                    if let Some(argv) = command(&b, output, &z.target, bt) {
                        run(&fake, &argv);
                    }
                }
            }
        }
        calls.extend(fake.calls());
    }
    let fixed = [
        "alpheus-toggle sound",
        "alpheus-toggle network",
        "alpheus-toggle bluetooth",
        "alpheus-toggle home",
        "foot -e btop",
        "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle",
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+",
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%-",
        "wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle",
    ];
    for c in &calls {
        assert!(c.starts_with("wm-set workspace ") || fixed.contains(&c.as_str()), "command outside the allowlist: {c}");
        assert!(!c.to_lowercase().contains("dpms"), "{c}");
    }
    for expected in fixed {
        assert!(calls.iter().any(|c| c == expected), "{expected} never reached: {calls:?}");
    }
}

#[test]
fn compositor_requests_are_read_only() {
    let texts: Vec<&str> = Request::ALL.iter().map(|r| r.text()).collect();
    assert_eq!(texts, ["j/monitors", "j/workspaces", "j/clients", "j/activewindow"]);
    assert_eq!(REQUEST, "\"EventStream\"");
    assert_eq!(mango::REQUESTS, ["watch all-monitors", "watch all-clients"]);
    assert_eq!(sway::Message::ALL.map(|m| m.code()), [1, 2, 4], "GET_WORKSPACES, SUBSCRIBE, GET_TREE: never RUN_COMMAND (0)");
    assert_eq!(sway::SUBSCRIPTION, "[\"workspace\",\"window\"]");
    assert_eq!(sway::Message::ALL.map(|m| m.payload().len()), [0, sway::SUBSCRIPTION.len(), 0]);
}

/// Every other way to write on a socket, refused in the Mango and Sway sources: their one
/// `write_all(` is the only write.
const OTHER_WRITES: [&str; 8] =
    [".write(", "libc::write", "libc::send", "send_to(", "write_vectored(", "write!(", "writeln!(", "write_fmt("];

/// Mango and Sway spec § 5: the Mango source writes one thing, its request line (`request_line`,
/// built from `REQUESTS` only), and every request is a watch: no dispatch, no other verb. No
/// other way to write on a socket (`OTHER_WRITES`) is used.
#[test]
fn mango_writes_only_its_request_lines() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sources/mango.rs")).unwrap();
    let prod = production_code(&text);
    assert_eq!(prod.matches("write_all(").count(), 1, "one write in the Mango source");
    assert!(prod.contains("socket.write_all(request_line(watch).as_bytes())"), "and it writes a request line");
    assert!(mango::REQUESTS.iter().all(|r| r.starts_with("watch ")), "{:?}", mango::REQUESTS);
    assert!(!prod.to_lowercase().contains("dispatch"), "no dispatch verb in the Mango source");
    for never in OTHER_WRITES {
        assert!(!prod.contains(never), "{never} in the Mango source");
    }
}

/// Mango and Sway spec § 3: the Sway source writes one thing, a `Message`'s frame; adding a
/// message type must update this exhaustive match (and so this review). No other way to write on
/// a socket (`OTHER_WRITES`) is used.
#[test]
fn sway_writes_only_its_three_messages() {
    fn name(m: sway::Message) -> &'static str {
        match m {
            sway::Message::GetWorkspaces => "GET_WORKSPACES",
            sway::Message::Subscribe => "SUBSCRIBE",
            sway::Message::GetTree => "GET_TREE",
        }
    }
    assert_eq!(sway::Message::ALL.map(name), ["GET_WORKSPACES", "SUBSCRIBE", "GET_TREE"]);
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sources/sway.rs")).unwrap();
    let prod = production_code(&text);
    assert_eq!(prod.matches("write_all(").count(), 1, "one write in the Sway source");
    assert!(prod.contains("s.write_all(&frame(m))"), "and it writes a Message's frame");
    for never in OTHER_WRITES {
        assert!(!prod.contains(never), "{never} in the Sway source");
    }
}

/// The four files of the two backends parse what a compositor sends: no path in them may panic
/// (Mango and Sway spec § 5). Production code only.
#[test]
fn the_mango_and_sway_backends_have_no_panicking_path() {
    for file in ["compositor/mango.rs", "compositor/sway.rs", "sources/mango.rs", "sources/sway.rs"] {
        let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file)).unwrap();
        let prod = production_code(&text);
        for never in [".unwrap()", ".expect(", "panic!(", "unreachable!(", "[0]", "[1]", "[i]", "[..n]"] {
            assert!(!prod.contains(never), "{never} in {file}");
        }
    }
}

/// The media model parses what a player publishes over D-Bus (titles, positions, lengths): no
/// path in `media/mod.rs`, `media/popup.rs` or `media/app.rs` may panic. Production code only.
#[test]
fn the_media_files_have_no_panicking_path() {
    for file in ["media/mod.rs", "media/popup.rs", "media/app.rs"] {
        let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file)).unwrap();
        let prod = production_code(&text);
        for never in [".unwrap()", ".expect(", "panic!(", "unreachable!(", "[0]", "[1]", "[..n]"] {
            assert!(!prod.contains(never), "{never} in {file}");
        }
        // `[i]` indexes unless it is `vec![i]`, the one-element vector popup.rs builds.
        assert!(!prod.replace("vec![i]", "").contains("[i]"), "[i] in {file}");
    }
}

/// Small features 1, notification rules: the rules never name an app or a text in a log, and
/// notify/mod.rs, the pure model, logs nothing.
#[test]
fn the_notification_rules_log_nothing() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/notify/mod.rs")).unwrap();
    let prod = production_code(&text);
    for log in ["log(", "log::write", "crate::log"] {
        assert!(!prod.contains(log), "notify/mod.rs logs nothing ({log})");
    }
}

/// Every `.rs` file under `src/`, `tests.rs` included: for the scans of raw
/// text, which must also see test code (no real call, no forbidden word).
fn all_source_files() -> Vec<(PathBuf, String)> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut f = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut f);
    assert!(f.len() > 10);
    f.into_iter()
        .map(|p| {
            let t = std::fs::read_to_string(&p).unwrap();
            (p, t)
        })
        .collect()
}

/// The production files: `all_source_files()` without the `tests.rs` files
/// (the moved-out `#[cfg(test)] mod tests;` bodies).
fn source_files() -> Vec<(PathBuf, String)> {
    all_source_files().into_iter().filter(|(p, _)| p.file_name().is_some_and(|n| n != "tests.rs")).collect()
}

/// Path of `p` relative to `src/` (e.g. `sources/hypr.rs`, or
/// `sources/hypr/mod.rs` if `sources::hypr` ever becomes a directory):
/// unlike the last two path components, this form stays correct no matter
/// how deeply nested — a plain `components().rev().take(2)` would lose the
/// `sources/` prefix as soon as a module gained a subdirectory.
fn relative_path(p: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    p.strip_prefix(&root).expect("path outside src/").to_string_lossy().into_owned()
}

/// A `UnixStream::connect` (or opening a compositor socket) is only allowed
/// in `src/sources/` — and anywhere in that directory, not only in the two
/// files named `hypr.rs` and `niri.rs`: the rest of the daemon (painter,
/// wayland, state, grid…) only ever receives the already normalized state
/// (`CompositorState`), never a raw connection. Stays valid if
/// `sources::hypr` or `sources::niri` ever split into several files, or if
/// a shared module (`sources/common.rs`) appears.
fn compositor_connection_allowed(relative_path: &str) -> bool {
    relative_path.starts_with("sources/")
}

#[test]
fn connection_allowed_anywhere_in_sources_not_just_hypr_and_niri() {
    assert!(compositor_connection_allowed("sources/hypr.rs"));
    assert!(compositor_connection_allowed("sources/niri.rs"));
    assert!(compositor_connection_allowed("sources/mod.rs"), "not only the two named files");
    assert!(compositor_connection_allowed("sources/future/submodule.rs"), "even nested deeper");
    assert!(!compositor_connection_allowed("wayland.rs"));
    assert!(!compositor_connection_allowed("painter.rs"));
}

/// Words that must appear nowhere in the daemon's sources (compared
/// lowercased): ways to write to the compositor. `keyword` stays a bare word
/// (Hyprland's request socket takes `keyword general:gaps_in 0` as one
/// string, not only `hyprctl`'s argv), and the `.desktop` key `Keywords`
/// (read by the launcher) is erased before the search instead. `mmsg`: Mango's
/// command-line client (its watch and dispatch verbs), never run, never named.
const FORBIDDEN_WORDS: [&str; 7] = ["dpms", "hyprctl", "niri msg", "swaymsg", "keyword", "\"dispatch", "mmsg"];

/// First forbidden word found in `text`, compared lowercased, `keywords` erased.
fn forbidden_word(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase().replace("keywords", "");
    FORBIDDEN_WORDS.into_iter().find(|w| lower.contains(w))
}

#[test]
fn the_keywords_key_passes_but_the_keyword_command_does_not() {
    assert_eq!(forbidden_word(r#"localized(&main, env.locale, "Keywords")"#), None);
    assert_eq!(forbidden_word("pub keywords: Vec<String>, // localized list"), None);
    assert_eq!(forbidden_word("keyword general:gaps_in 0"), Some("keyword"), "the socket request, unquoted");
    assert_eq!(forbidden_word(r#"format!("keyword {k} {v}")"#), Some("keyword"));
    assert_eq!(forbidden_word(r#"exec.spawn("wm", &["Keyword", "general:gaps_in", "0"])"#), Some("keyword"), "case does not hide it");
    assert_eq!(forbidden_word("keywords; keyword x"), Some("keyword"), "a Keywords nearby does not hide it");
    assert_eq!(forbidden_word("set DPMS off"), Some("dpms"));
    assert_eq!(forbidden_word("mmsg dispatch view,2"), Some("mmsg"));
}

/// The daemon only reads from the compositor, plus two writes: the workspace command, and output
/// configurations (`zwlr_output_configuration_v1`), built and applied in `outputs/wlr.rs` only
/// and sent only by `outputs/app.rs` (the controller's one-in-flight, last-screen and cap rules);
/// never output power management, never dpms (displays spec § 2, as amended). A third write is the
/// clipboard's own selection, set in `clipboard/control.rs` only (clipboard spec decision 14): the
/// daemon's re-copy of a history entry, never a primary selection.
#[test]
fn the_daemon_only_reads_from_the_compositor() {
    for (p, t) in all_source_files() {
        if let Some(word) = forbidden_word(&t) {
            panic!("\"{word}\" in {}", p.display());
        }
        let name = relative_path(&p);
        if t.contains(".socket.sock") {
            assert_eq!(name, "sources/hypr.rs", "Hyprland request socket opened elsewhere");
        }
        if t.contains("UnixStream::connect") {
            assert!(compositor_connection_allowed(&name), "connection to the compositor in {name}");
        }
        assert!(!t.contains("Command::new"), "process spawned directly in {name} (go through the executor)");
        // The one compositor write besides the workspace command (displays spec § 2): an output
        // configuration, built and applied in the protocol file only.
        if let Some(w) = OUTPUT_WRITES.iter().find(|w| t.contains(**w)) {
            assert_eq!(name, OUTPUTS_PROTOCOL_FILE, "\"{w}\": an output configuration outside the protocol file");
        }
        // Only the glue sends one, so none bypasses the controller.
        if t.contains("wlr.send(") {
            assert_eq!(name, "outputs/app.rs", "an output configuration sent outside the displays glue");
        }
    }
}

/// The displays timer is synced as soon as the loop exists: a `done` handled before (a
/// roundtrip at startup) would leave its debounce unarmed until the next hotplug.
#[test]
fn the_displays_timer_is_synced_once_the_loop_exists() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/startup.rs")).unwrap();
    let prod = production_code(&text);
    let set = "state.loop_handle = Some(handle.clone());";
    let at = prod.find(set).unwrap_or_else(|| panic!("{set} not found in startup.rs"));
    assert!(prod[at + set.len()..].trim_start().starts_with("state.displays_sync();"), "displays_sync right after the loop handle");
}

/// The daemon stopping sends a trial's state from before while the app (its
/// output manager) still exists, before the connection's last flush carries it.
#[test]
fn the_displays_are_restored_before_the_app_is_dropped() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/startup.rs")).unwrap();
    let prod = production_code(&text);
    let body = &prod[prod.find("fn shut_down(").expect("shut_down in startup.rs")..];
    let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("{needle} not found in shut_down"));
    let restore = at("state.displays_shutdown()");
    assert!(restore < at("drop(core.state);"), "restored before the app is dropped");
    assert!(restore < at("core.conn.flush()"), "restored before the last flush");
}

/// The only file that speaks `zwlr_output_manager_v1` (displays spec § 1).
const OUTPUTS_PROTOCOL_FILE: &str = "outputs/wlr.rs";

/// What builds or applies an output configuration. `.apply()` with nothing inside: `Bar::apply`
/// takes a message.
const OUTPUT_WRITES: [&str; 4] = ["zwlr_output_configuration", "ZwlrOutputConfiguration", "create_configuration", ".apply()"];

/// Displays spec § 1: one file speaks the output protocol; the glue and the rest of the daemon
/// only see the pure model (`outputs::model`, `outputs::control`).
#[test]
fn outputs_protocol_only_in_wlr_rs() {
    let mut seen = false;
    for (p, t) in all_source_files() {
        let name = relative_path(&p);
        for token in ["zwlr_output_", "ZwlrOutput"] {
            if t.contains(token) {
                assert_eq!(name, OUTPUTS_PROTOCOL_FILE, "\"{token}\" outside the protocol file");
                seen = true;
            }
        }
    }
    assert!(seen, "the protocol file names the protocol");
}

/// Displays spec § 2 and the DisplayLink freeze: no output power management anywhere in alpheus,
/// never a screen powered off (`dpms` is already a forbidden word in the daemon).
#[test]
fn output_power_management_never_appears() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = 0;
    let mut stack: Vec<PathBuf> =
        std::fs::read_dir(crates).unwrap().flatten().map(|e| e.path().join("src")).filter(|p| p.is_dir()).collect();
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files += 1;
                let t = std::fs::read_to_string(&p).unwrap().to_lowercase().replace('_', "");
                for word in ["outputpower", "dpms"] {
                    assert!(!t.contains(word), "\"{word}\" in {}", p.display());
                }
            }
        }
    }
    assert!(files > 100, "the workspace's sources were not found ({files})");
}

/// Diagnostics spec § 8: the daemon reads systemd's failed-units count and subscribes to its signals,
/// and asks it for nothing else. No method that starts, stops, restarts, resets, kills, enables,
/// masks, reloads or changes anything of systemd's may be named in `sources/dbus/` (the acting
/// Bluetooth connection excepted: it names none of them either), the count's one `Get` never
/// becomes a `GetAll` (the Manager's carries its `Environment`), and no unit name is logged.
#[test]
fn no_acting_systemd1_method_in_the_daemon() {
    let mut seen = 0;
    for (path, text) in source_files() {
        let name = relative_path(&path);
        if !name.starts_with("sources/dbus/") {
            continue;
        }
        seen += 1;
        let prod = production_code(&text);
        for word in [
            "StartUnit",
            "StopUnit",
            "RestartUnit",
            "TryRestartUnit",
            "ReloadUnit",
            "ReloadOrRestartUnit",
            "ResetFailed",
            "KillUnit",
            "CleanUnit",
            "FreezeUnit",
            "StartTransientUnit",
            "SetUnitProperties",
            "EnableUnitFiles",
            "DisableUnitFiles",
            "MaskUnitFiles",
            "LinkUnitFiles",
            "SetEnvironment",
            "UnsetEnvironment",
            "Reexecute",
            "Unsubscribe",
            "systemd1.Manager.Reload",
            "\"Reload\"",
            "\"Halt\"",
            "\"KExec\"",
            "\"Exit\"",
        ] {
            assert!(!prod.contains(word), "{word} in {name}: the daemon only counts failed units");
        }
        // `Subscribe` is built in `dbusrs_bus.rs` and driven from `units.rs`, nowhere else (the
        // test-only fake answers it).
        let allowed = ["dbusrs_bus.rs", "units.rs", "mod.rs", "fake.rs"].map(|f| format!("sources/dbus/{f}"));
        if !allowed.contains(&name) {
            assert!(!prod.contains("subscribe_units("), "{name} subscribes to systemd");
        }
    }
    assert!(seen >= 10, "sources/dbus/ was not found: {seen} files");
    let units = source_files().into_iter().find(|(p, _)| relative_path(p) == "sources/dbus/units.rs").expect("units.rs").1;
    let prod = production_code(&units);
    assert!(!prod.contains("get_all(") && !prod.contains("\"Environment\""), "units.rs reads one property, never GetAll");
    assert_eq!(prod.matches("get_u32(").count(), 1, "one read");
    // The reader logs a fixed line: no formatting of a name, an owner or a path.
    assert_eq!(prod.matches("log(").count(), 1, "{prod}");
    assert!(prod.contains("log(\"D-Bus Units: Subscribe refused\")"), "a fixed line");
}

/// Review guardrail: only files in `src/sources/` are allowed to open or
/// connect to the compositor's sockets — never elsewhere in the daemon
/// (painter, wayland, state, grid…), which only ever receive the already
/// normalized state (`CompositorState`), never a raw connection.
/// Generalizes the two checks above (which didn't cover `.socket2.sock`)
/// into a single rule, by directory rather than by named file, to stay
/// valid if `sources::hypr` or `sources::niri` ever split into several
/// files.
#[test]
fn compositor_sockets_opened_only_in_sources() {
    for (p, t) in all_source_files() {
        let name = relative_path(&p);
        for pattern in [".socket.sock", ".socket2.sock", "UnixStream::connect"] {
            assert!(!t.contains(pattern) || compositor_connection_allowed(&name), "\"{pattern}\" outside src/sources/: {name}");
        }
    }
}

/// 2026-10-05: the launcher's click-outside scrims must keep working
/// even over a fullscreen game (Hyprland draws its Top layer BENEATH a
/// fullscreen window — a scrim there would never see the click). Both the
/// scrims and the box now share the Overlay layer (`surface::scrim_layer`);
/// within one layer, wlr-layer-shell stacks surfaces by creation order, so
/// the box must be created AFTER its scrims, not before, or it ends up
/// stacked beneath them instead of above. No fake layer-shell exists to exercise the real stacking end to
/// end, so this checks the one thing that would actually regress it: the two
/// calls swapped back.
#[test]
fn scrims_are_created_before_the_box_so_it_stacks_above_them() {
    let (_, text) = source_files().into_iter().find(|(p, _)| relative_path(p) == "launcher/surface.rs").expect("surface.rs");
    let prod = production_code(&text);
    let body = function_body(&prod, "pub fn launcher_open(");
    let scrims_at = body.find("self.launcher_scrims(qh)").expect("the scrims are created in launcher_open");
    let box_at = body.find("Layer::Overlay, Some(NAMESPACE)").expect("the box's layer surface is created in launcher_open");
    assert!(scrims_at < box_at, "the scrims must be created before the box, so it stacks above them");
    // Quick notes: the pad's box follows the same rule, its scrims under their own namespace.
    let (_, text) = source_files().into_iter().find(|(p, _)| relative_path(p) == "notes/surface.rs").expect("notes/surface.rs");
    let prod = production_code(&text);
    let body = function_body(&prod, "pub(crate) fn notes_open(");
    let scrims_at = body.find("self.notes_scrims(qh)").expect("the scrims are created in notes_open");
    let box_at = body.find("Layer::Overlay, Some(NAMESPACE)").expect("the box's layer surface is created in notes_open");
    assert!(scrims_at < box_at, "the pad's scrims must be created before its box, so it stacks above them");
    assert!(prod.contains("const SCRIM_NAMESPACE: &str = \"alpheus-notes-scrim\";"), "the pad's scrims are alpheus-notes-scrim");
}

/// Quick notes: a daemon stopping saves the pad (an edit not yet autosaved, or after a failed
/// autosave) through its close, guarded, before the surfaces go.
#[test]
fn shutting_down_closes_and_saves_the_pad() {
    let (_, text) = source_files().into_iter().find(|(p, _)| relative_path(p) == "startup.rs").expect("startup.rs");
    let prod = production_code(&text);
    let body = function_body(&prod, "fn shut_down(");
    let close = body.find("notes_guarded(\"shutdown\", |app| app.notes_close(").expect("shut_down closes the pad, guarded");
    let drop_at = body.find("drop(core.state)").expect("shut_down drops the app");
    assert!(close < drop_at, "the pad is saved before the app goes");
}

/// OSD spec § 1 and the bubbles' (notifications spec § 3): the pill and the bubbles live on the
/// Overlay layer, the only one Hyprland draws above a fullscreen window (the launcher box's rule,
/// above). A headless sway cannot tell the layers apart (no fullscreen window there), so this
/// checks the one call that would regress it: each file creates exactly one layer surface, in its
/// create function, on `Layer::Overlay`.
#[test]
fn the_osd_and_the_bubbles_are_created_on_the_overlay_layer() {
    let files = source_files();
    for (name, create) in [("osd/surface.rs", "fn osd_create("), ("notify/bubbles_surface.rs", "fn bubbles_create(")] {
        let text = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let prod = production_code(text);
        assert_eq!(prod.matches("create_layer_surface(").count(), 1, "one layer surface in {name}");
        let body = function_body(&prod, create);
        let call =
            &body[body.find("create_layer_surface(").unwrap_or_else(|| panic!("the layer surface is created in {name}'s {create}"))..];
        let args = &call[..call.find(';').unwrap_or(call.len())];
        assert!(args.contains("Layer::Overlay"), "{name}: {args}");
    }
}

/// Every seat/keyboard entry point
/// that can reach launcher code goes through `launcher_guarded` — a panic
/// there must close the launcher, never take the bar down with it.
///
/// The keyboard moved out of `OpenLauncher` into `App`,
/// shared with a grabbing popup: `new_capability`/`remove_capability`/
/// `remove_seat` (wayland.rs) now call the shared `keyboard_acquire`/
/// `keyboard_lost` (popup.rs), which only touch Wayland/seat state, not the
/// launcher's model — nothing there to guard anymore. The guard moved with
/// the actual launcher calls: `popup.rs`'s `key_event` (every keypress,
/// including `update_modifiers`'s own `self.keyboard.mods` write, touches no
/// launcher state and needs none either) dispatches to the launcher only
/// for its `KeyTo::Launcher` branch, guarded there; `leave` still guards its
/// direct `launcher_close` call. The quick notepad's pad gets the same treatment through
/// `notes_guarded`: its `KeyTo::Notes` arm in `key_event`, and its own close in `leave`. No fake Wayland seat or keyboard exists to
/// call these trait methods directly in a unit test, so this checks the one
/// thing that would actually regress it: the guard call disappearing from
/// the body that still reaches launcher code.
#[test]
fn seat_and_keyboard_handlers_guard_their_launcher_calls() {
    let files = source_files();
    let wayland = &files.iter().find(|(p, _)| relative_path(p) == "wayland.rs").expect("wayland.rs").1;
    let wayland_prod = production_code(wayland);
    for (func, call) in
        [("fn new_capability(", "keyboard_acquire"), ("fn remove_capability(", "keyboard_lost"), ("fn remove_seat(", "keyboard_lost")]
    {
        let body = function_body(&wayland_prod, func);
        assert!(body.contains(call), "{func} must still call {call}: {body}");
    }
    let popup = &files.iter().find(|(p, _)| relative_path(p) == "popup.rs").expect("popup.rs").1;
    let popup_prod = production_code(popup);
    let key_event = function_body(&popup_prod, "fn key_event(");
    assert!(key_event.contains("launcher_guarded("), "key_event must guard its launcher call: {key_event}");
    let leave = function_body(&popup_prod, "fn leave(");
    assert!(leave.contains("launcher_guarded("), "leave must guard its launcher call: {leave}");
    // Quick notes: the pad's calls go through its own guard, beside the launcher's.
    let notes_arm = key_event.find("KeyTo::Notes =>").map(|at| &key_event[at..]).expect("key_event has a KeyTo::Notes arm");
    let notes_arm = &notes_arm[..notes_arm.find("KeyTo::Popup").unwrap_or(notes_arm.len())];
    assert!(notes_arm.contains("notes_guarded("), "key_event's KeyTo::Notes arm must guard the pad: {notes_arm}");
    assert!(leave.contains("notes_guarded("), "leave must guard its pad call: {leave}");
}

/// `press_key`/`repeat_key` (the `KeyboardHandler` trait methods)
/// and the repeat closure bound by `keyboard_acquire` must all route through `key_event` — the
/// one place that guards the launcher branch — never call the launcher directly themselves; and
/// `keyboard_acquire`/`keyboard_lost`, which only touch Wayland/seat state, must call no launcher
/// method at all. No fake Wayland keyboard exists to call these in a unit test, so this checks the
/// one thing that would actually regress it: a direct call appearing where `key_event` should be,
/// or a launcher call appearing in a function that must have none.
#[test]
fn keyboard_entry_points_go_through_key_event_and_bind_no_launcher_call() {
    let files = source_files();
    let popup = &files.iter().find(|(p, _)| relative_path(p) == "popup.rs").expect("popup.rs").1;
    let popup_prod = production_code(popup);
    for func in ["fn press_key(", "fn repeat_key("] {
        let body = function_body(&popup_prod, func);
        assert!(body.contains("key_event("), "{func} must go through key_event: {body}");
    }
    let keyboard_acquire = function_body(&popup_prod, "fn keyboard_acquire(");
    assert!(keyboard_acquire.contains("key_event("), "the repeat closure must go through key_event: {keyboard_acquire}");
    // `self.launcher_` (a method) AND `self.launcher.` (the field itself, e.g. `self.launcher.take()`).
    for launcher in ["self.launcher_", "self.launcher."] {
        assert!(!keyboard_acquire.contains(launcher), "keyboard_acquire must not touch the launcher ({launcher}): {keyboard_acquire}");
    }
    let keyboard_lost = function_body(&popup_prod, "fn keyboard_lost(");
    for launcher in ["self.launcher_", "self.launcher."] {
        assert!(!keyboard_lost.contains(launcher), "keyboard_lost must not touch the launcher ({launcher}): {keyboard_lost}");
    }
}

/// `popup_close`/`popup_output_gone` (popup.rs) must reset their
/// parent bar's `Exclusive` keyboard interactivity by its stored SURFACE IDENTITY (`OpenPopup`'s
/// own `parent` field), never by a bar's `name` — changed after the fact by `update_output`, or
/// briefly shared by two bars — or the wrong bar (or none) gets reset, left stuck holding the user's
/// keyboard. No fake Wayland surface exists to build two same-named bars in a unit test (the
/// identity-vs-name LOGIC itself is unit-tested generically, `position_by_goes_by_the_projected_
/// identity_never_a_shared_name` in `wayland.rs`), so this checks the one thing that would
/// actually regress production code: a `.name ==` comparison reappearing in the parent lookup.
#[test]
fn popup_close_and_output_gone_find_their_parent_by_identity_never_by_name() {
    let files = source_files();
    let popup = &files.iter().find(|(p, _)| relative_path(p) == "popup.rs").expect("popup.rs").1;
    let prod = production_code(popup);
    let popup_close = function_body(&prod, "pub fn popup_close(");
    assert!(!popup_close.contains(".name =="), "popup_close must not look up its parent by name: {popup_close}");
    assert!(popup_close.contains("position_by"), "popup_close must look up its parent by identity: {popup_close}");
    let popup_output_gone = function_body(&prod, "pub(crate) fn popup_output_gone(");
    assert!(!popup_output_gone.contains(".name =="), "popup_output_gone must not look up its parent by name: {popup_output_gone}");
    assert!(
        popup_output_gone.contains("&p.parent == parent"),
        "popup_output_gone must compare the stored parent identity: {popup_output_gone}"
    );
}

/// `hover_fire` (wayland.rs) used to re-locate its bar
/// by `s.name == anchor.output` — the same stale-name problem as above, reached from the tooltip's
/// own deferred timer (up to ½ s after `hover_moved` armed it) instead of an output going away.
/// Fixed by carrying the surface's identity from `hover_moved`, which knows the exact surface,
/// instead of trusting a name that could have changed meanwhile. No fake Wayland surface exists to
/// rename a bar in a unit test, so this checks the one thing that would actually regress it: a
/// `.name ==` lookup reappearing in `hover_fire`.
#[test]
fn hover_fire_finds_its_bar_by_surface_identity_never_by_name() {
    let files = source_files();
    let wayland = &files.iter().find(|(p, _)| relative_path(p) == "wayland.rs").expect("wayland.rs").1;
    let prod = production_code(wayland);
    let hover_fire = function_body(&prod, "pub(crate) fn hover_fire(");
    assert!(!hover_fire.contains(".name =="), "hover_fire must not look up its bar by name: {hover_fire}");
    assert!(hover_fire.contains("hover_surface"), "hover_fire must use the surface identity carried from hover_moved: {hover_fire}");
}

/// `launcher_fail` must take the launcher out of `self` FIRST, drop its box, then release the
/// keyboard, then early-return on "nothing was open". `keyboard_release` is a no-op while
/// `keyboard_wanted()` holds, which it does as long as `self.launcher` is `Some`: releasing before
/// the take left the keyboard, its keymap and its
/// repeat source resident after a panic closed the launcher. While the box lives, a key down on it
/// defers the release to a key-up its dead surface never sends: released after
/// the box is dropped (the guarded close consumes it). And the release must precede the early
/// return, for a panic inside `launcher_close` that already took the launcher. No `wl_keyboard`
/// can be built without a compositor, so the order is checked in the source.
#[test]
fn launcher_fail_takes_the_launcher_then_releases_the_keyboard_then_returns_early() {
    let files = source_files();
    let host = &files.iter().find(|(p, _)| relative_path(p) == "launcher/host.rs").expect("launcher/host.rs").1;
    let prod = production_code(host);
    let body = function_body(&prod, "fn launcher_fail(");
    let take_at = body.find("self.launcher.take()").expect("launcher_fail must take the launcher");
    let close_at = body.find("guarded(\"close\"").expect("launcher_fail must drop the box in a guarded close");
    let release_at = body.find("self.keyboard_release()").expect("launcher_fail must call keyboard_release");
    let early_return_at = body.find("return;").expect("launcher_fail must early-return when nothing was open");
    assert!(take_at < release_at, "the launcher must be taken BEFORE keyboard_release (still wanted otherwise): {body}");
    assert!(close_at < release_at, "the box must be dropped BEFORE keyboard_release (a key down on it defers): {body}");
    assert!(release_at < early_return_at, "keyboard_release must run before the early return: {body}");
    assert_eq!(body.matches("self.keyboard_release()").count(), 1, "one release, on both paths: {body}");
}

/// The crash of 2026-10-06 and 2026-10-09: a released `wl_keyboard`'s last reference, dropped inside
/// calloop's dispatch, made SCTK's repeat data re-enter the loop. Every release goes through
/// `keyboard_retire`, which holds the keyboard in `keyboard.retired`, and the loop drains that
/// list right after each `dispatch` returns. The ordering needs a compositor's `delete_id`, so the
/// wiring is checked in the source (`retired.rs` tests the hazard and the drain on a real loop).
#[test]
fn a_released_keyboard_is_retired_then_dropped_after_the_dispatch() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (popup, startup) = (src("popup.rs"), src("startup.rs"));
    for f in ["pub(crate) fn keyboard_release(", "pub(crate) fn keyboard_lost("] {
        let body = function_body(&popup, f);
        assert!(body.contains("self.keyboard_retire(k)"), "{f} must retire the keyboard, never drop it: {body}");
    }
    let retire = function_body(&popup, "fn keyboard_retire(");
    assert!(retire.contains("self.keyboard.retired.hold(k)"), "keyboard_retire must hold it: {retire}");
    let run = function_body(&startup, "fn main_loop(");
    let dispatch = run.find("event_loop.dispatch(").expect("the loop dispatches");
    let drain = run.find("state.keyboard.retired.drain()").expect("the loop must drain the retired keyboards");
    assert!(dispatch < drain, "drained after the dispatch returns, never inside it: {run}");
}

/// Opening the panel marks everything read (spec § 4), but only once it shows:
/// a `popup_open` that refuses (no shared buffer, no positioner) must leave the count on the bell.
/// Those refusals need a failing compositor or allocator, so the order is checked in the source.
#[test]
fn the_panel_marks_read_only_after_it_opened() {
    let files = source_files();
    let panel_app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "notify/panel_app.rs").expect("notify/panel_app.rs").1);
    let body = function_body(&panel_app, "pub(crate) fn panel_toggle(");
    let open = body.find("self.popup_open(").expect("panel_toggle opens a popup");
    let read = body.find("mark_read()").expect("panel_toggle marks read");
    let shown = body.find("\"panel shown (entries").expect("panel_toggle logs the shown panel");
    assert_eq!(body.matches("mark_read()").count(), 1, "one mark, on the shown path: {body}");
    assert!(open < read, "marked read only once `popup_open` answered true: {body}");
    // Inside the shown branch: after the open, before its log line (never after the `if`).
    assert!(read < shown, "marked read inside the shown branch, before `panel shown`: {body}");
}

/// The settle rounds (link.rs, session_bus/router.rs) stop after `SETTLE_ROUNDS`, and
/// what the last round's flush drained waits in the bus's `ready`, which no fd event announces (the
/// socket was read already). After every callback, `link_sync` and `tray_sync` route it from one
/// idle read. libdbus's flush reads only when its write has to wait, so no private bus builds that
/// state on demand: the wiring is checked in the source.
#[test]
fn answers_left_by_the_settle_rounds_are_routed_from_an_idle() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (link, bridge) = (src("link.rs"), src("tray/bridge.rs"));
    for (body, ready, read) in [
        (function_body(&link, "pub(crate) fn link_sync("), "SessionLink::has_ready", "app.link_readable()"),
        (function_body(&bridge, "fn tray_sync("), "Tray::has_ready", "app.tray_readable()"),
    ] {
        let (at, idle) = (body.find(ready), body.find("insert_idle"));
        assert!(at.is_some() && idle.is_some(), "{ready} must queue an idle: {body}");
        assert!(body[idle.unwrap_or(0)..].contains(read), "the idle must read: {read}: {body}");
    }
    let mpris_do = function_body(&bridge, "pub(crate) fn mpris_do(");
    assert!(mpris_do.contains("self.link_sync()") && mpris_do.contains("self.tray_sync()"), "mpris_do ends in the syncs: {mpris_do}");
}

/// Three small orderings no fake Wayland surface can exercise. `menu_repaint`
/// finds its bar by the popup's parent SURFACE (never an output name); `launcher_open` drops the
/// remembered tray press (`menu.last_icon`), so a late failed Activate cannot open a menu over the
/// launcher; and the bar's keyboard interactivity is only reset where `popup_open` set it
/// (`exclusive_during_grab`, false on Hyprland), at both places.
#[test]
fn menu_repaint_launcher_open_and_the_interactivity_reset_keep_their_guards() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (menu_app, popup, surface) = (src("tray/menu_app.rs"), src("popup.rs"), src("launcher/surface.rs"));
    let repaint = function_body(&menu_app, "fn menu_repaint(");
    assert!(!repaint.contains(".name =="), "menu_repaint must not find its bar by name: {repaint}");
    assert!(repaint.contains("position_by"), "menu_repaint must find its bar by the popup's parent surface: {repaint}");
    let open = function_body(&surface, "pub fn launcher_open(");
    assert!(open.contains("self.menu.last_icon = None"), "launcher_open must drop the remembered tray press: {open}");
    let close = function_body(&popup, "pub fn popup_close(");
    assert!(close.contains("self.exclusive_during_grab"), "popup_close must only reset what popup_open set: {close}");
    let opening = function_body(&popup, "pub fn popup_open(");
    assert!(
        opening.contains(
            "if grab && (self.exclusive_during_grab || keyed) {\n                    layer.set_keyboard_interactivity(KeyboardInteractivity::None)"
        ),
        "popup_open's error path must only reset what it set (a keyed popup set it on every compositor): {opening}"
    );
}

/// An output removal (`popup_output_gone`) and the
/// launcher opening (`launcher_open`) both close any open popup, but neither used to cancel
/// `Hover` too — left alone, `Hover.over` keeps naming the same icon, and `Hover::moved` (keyed
/// only on the item, never a surface) then wrongly sees "still on it" and never re-arms the
/// tooltip timer again, even long after the bar came back or the launcher closed. No fake Wayland
/// surface or output exists to exercise this directly in a unit test (the headless tests below
/// exercise the real, observable symptom end to end), so this checks the one thing that would
/// actually regress the fix: the `hover.cancel()` call disappearing from either function.
#[test]
fn an_output_removal_and_a_launcher_open_also_cancel_a_stale_hover() {
    let files = source_files();
    let popup = &files.iter().find(|(p, _)| relative_path(p) == "popup.rs").expect("popup.rs").1;
    let popup_prod = production_code(popup);
    let popup_output_gone = function_body(&popup_prod, "pub(crate) fn popup_output_gone(");
    assert!(popup_output_gone.contains("hover.cancel()"), "popup_output_gone must also cancel a stale hover: {popup_output_gone}");
    let surface = &files.iter().find(|(p, _)| relative_path(p) == "launcher/surface.rs").expect("launcher/surface.rs").1;
    let surface_prod = production_code(surface);
    let launcher_open = function_body(&surface_prod, "pub fn launcher_open(");
    assert!(launcher_open.contains("hover.cancel()"), "launcher_open must also cancel a stale hover: {launcher_open}");
}

/// `tooltip_item_gone` (wayland.rs) relies on a Tooltip popup
/// always being paired with `menu.tooltip_item` — set together in `hover_fire`, so it must be cleared
/// together too, in `popup_close`, whatever the closing reason (hover ended, a press, a replaced
/// popup…), or a later redraw's check would read a stale key left behind by an earlier tooltip.
/// No fake Wayland popup exists to exercise every closing reason in a unit test, so this checks
/// the one thing that would actually regress the fix: the reset disappearing from `popup_close`.
#[test]
fn popup_close_resets_tooltip_item_for_a_closed_tooltip() {
    let files = source_files();
    let popup = &files.iter().find(|(p, _)| relative_path(p) == "popup.rs").expect("popup.rs").1;
    let prod = production_code(popup);
    let popup_close = function_body(&prod, "pub fn popup_close(");
    assert!(popup_close.contains("tooltip_item = None"), "popup_close must reset tooltip_item on a closed tooltip: {popup_close}");
}

/// The launcher socket (spec § 3): bound in exactly one place, its file
/// name only ever built by `launcher_ipc::socket_path` (never spelled out,
/// so it cannot drift between the daemon and alpheus-launch).
#[test]
fn the_launcher_socket_is_bound_only_in_socket_rs() {
    let mut binds = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = production_code(&t);
        if prod.contains("UnixListener::bind") {
            assert_eq!(name, "launcher/socket.rs", "a listening socket bound in {name}");
            binds += prod.matches("UnixListener::bind").count();
        }
        assert!(!t.contains("alpheus-launcher.sock"), "socket name spelled out in {name}: use launcher_ipc::socket_path");
    }
    assert_eq!(binds, 1, "the launcher socket is bound once");
}

/// Launcher sources (sources spec § 6) and Shield (shield spec § 7): `Executor::capture` blocks for
/// up to its deadline, so only the source runner (on the scan thread) and the Shield runner (on its
/// own worker thread) call it (`NonBlocking` delegates it); and neither builds a shell command from
/// a line — the line is one argv element.
#[test]
fn only_the_runners_capture_and_never_through_a_shell() {
    let mut runner = None;
    let mut shield = None;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = production_code(&t);
        if prod.contains(".capture(") {
            assert!(matches!(name.as_str(), "launcher/sources.rs" | "shield/run.rs" | "nonblocking.rs"), ".capture( in {name}: it blocks");
        }
        // `run_silent` waits with no bound of its own: the Shield worker's thread alone (and the delegate).
        if prod.contains(".run_silent(") {
            assert!(matches!(name.as_str(), "shield/run.rs" | "nonblocking.rs"), ".run_silent( in {name}: it blocks");
        }
        if name == "launcher/sources.rs" {
            runner = Some(prod);
        } else if name == "shield/run.rs" {
            shield = Some(prod);
        }
    }
    let runner = runner.expect("launcher/sources.rs");
    assert!(runner.contains(".capture("), "the runner reads through the executor");
    for never in ["shell_argv", "\"sh\"", "\"-c\"", "spawn("] {
        assert!(!runner.contains(never), "{never} in launcher/sources.rs");
    }
    // The Shield runner (shield spec § 7): a configured argv through `capture`, on its own worker
    // thread (`thread::Builder` is its one way to start it), never a shell and never a detached spawn.
    let shield = shield.expect("shield/run.rs");
    assert!(shield.contains(".capture("), "the Shield runner reads through the executor");
    for never in ["spawn_with_", ".run(", "exec.spawn(", "executor.spawn(", "\"sh\"", "\"-c\"", "shell_argv", "Command::new"] {
        assert!(!shield.contains(never), "{never} in shield/run.rs");
    }
}

/// Not a behavioural test of the socket itself: this match has no wildcard,
/// so it is an exhaustive-match tripwire — a new `Request` variant stops
/// THIS FILE compiling until the match below (and the review that goes with
/// it) is updated, before it could ever reach the socket unexamined (review
/// Renamed from
/// `the_socket_accepts_exactly_three_requests`, which claimed to test the
/// socket's own behaviour when it only guards this match's exhaustiveness).
/// The notifications verbs are answered in `watch_connection` before
/// the launcher's guard, and never reach `decide` in practice.
#[test]
fn adding_a_request_variant_must_update_this_exhaustive_match() {
    use alpheus_common::launcher_ipc::{DmenuFlags, DndEnd, DndVerb, OsdVerb, QuickVerb, Request, ShieldVerb, TimerVerb};
    fn verb(r: &Request) -> &'static str {
        match r {
            Request::Toggle => "toggle",
            Request::Close => "close",
            Request::Calc => "calc",
            Request::Windows => "windows",
            Request::Notes => "notes",
            Request::Clipboard => "clipboard",
            Request::ClipboardClear => "clipboard-clear",
            Request::Tools => "tools",
            Request::Power => "power",
            Request::Dmenu { .. } => "dmenu",
            Request::Dnd(_) => "dnd",
            Request::DndOn(_) => "dnd-on",
            Request::ClearNotifications => "clear-notifications",
            Request::FailedRefresh => "failed-refresh",
            Request::Osd(_) => "osd",
            Request::Quick(_) => "quick",
            Request::Timer(_) => "timer",
            Request::Outputs => "outputs",
            Request::OutputsTry(_) => "outputs-try",
            Request::OutputsKeep => "outputs-keep",
            Request::OutputsRevert => "outputs-revert",
            Request::Shield(_) => "shield",
        }
    }
    let all = [
        Request::Toggle,
        Request::Close,
        Request::Calc,
        Request::Windows,
        Request::Notes,
        Request::Clipboard,
        Request::ClipboardClear,
        Request::Tools,
        Request::Power,
        Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() },
        Request::Dnd(DndVerb::Toggle),
        Request::DndOn(DndEnd::For(60)),
        Request::ClearNotifications,
        Request::FailedRefresh,
        Request::Osd(OsdVerb::Brightness),
        Request::Quick(QuickVerb::Top),
        Request::Timer(TimerVerb::Popup),
        Request::Outputs,
        Request::OutputsTry(String::new()),
        Request::OutputsKeep,
        Request::OutputsRevert,
        Request::Shield(ShieldVerb::Status),
    ];
    assert_eq!(
        all.iter().map(verb).collect::<Vec<_>>(),
        [
            "toggle",
            "close",
            "calc",
            "windows",
            "notes",
            "clipboard",
            "clipboard-clear",
            "tools",
            "power",
            "dmenu",
            "dnd",
            "dnd-on",
            "clear-notifications",
            "failed-refresh",
            "osd",
            "quick",
            "timer",
            "outputs",
            "outputs-try",
            "outputs-keep",
            "outputs-revert",
            "shield"
        ]
    );
}

/// Production code of a file: everything EXCEPT the item that follows each
/// `#[cfg(test)]` (a function, a `mod tests { … }` module, a fixture module
/// like `sources/hypr.rs::fake`…, or a brace-less one like
/// `sources/dbus/mod.rs`'s `pub mod fake;`, pointing at a separate file),
/// whatever its name or place in the file — not just "the last
/// `#[cfg(test)]`, necessarily a trailing test module": `compositor/mod.rs`
/// and `sources/hypr.rs` each have TWO, and nothing guarantees some future
/// file won't have one in the middle. Finds the end of the item by counting
/// braces (correct even nested), not by cutting at the first `#[cfg(test)]`
/// found (that would also, wrongly, have erased everything after it in the
/// file, test or not).
fn production_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..i]);
        let after_marker = &rest["#[cfg(test)]".len() + i..];
        let open = after_marker.find('{');
        let semi = after_marker.find(';');
        // A brace-less item (`pub mod fake;`, a `use`, a unit struct…) ends
        // at its `;`: stop there rather than let `find('{')` run past it
        // into some later, unrelated item's brace — which would wrongly
        // also strip every real item in between as if it were the test one.
        let ends_at_semicolon = match (open, semi) {
            (Some(o), Some(s)) => s < o,
            (Some(_), None) => false,
            (None, _) => true,
        };
        if ends_at_semicolon {
            rest = match semi {
                Some(s) => &after_marker[s + 1..],
                // No `;` and no `{` anywhere after the marker (end of
                // file): nothing left to strip.
                None => "",
            };
            continue;
        }
        let open = open.expect("ends_at_semicolon is false only when `open` is Some");
        let mut depth = 0u32;
        let mut relative_end = None;
        for (j, c) in after_marker[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        relative_end = Some(open + j + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        // Brace never closed: should not happen in Rust that compiles, but
        // an empty string rather than a panic on a malformed file remains
        // the safest choice for a guardrail.
        rest = relative_end.map(|f| &after_marker[f..]).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// `text` without its comments (`//`, `///`, `//!` to the end of the line, and `/* … */` blocks),
/// for the guardrails that read code: a comment naming a call must not satisfy them, nor a
/// comment holding `fn ` or a brace move the function a line belongs to. Newlines stay, so line
/// numbers keep their meaning. String and char literals are skipped over, so a `//` inside a URL
/// string is not a comment; raw strings are not recognised (none in the files read this way).
fn without_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match (c, next) {
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                let mut depth = 0;
                while i < chars.len() {
                    match (chars[i], chars.get(i + 1).copied()) {
                        ('/', Some('*')) => {
                            depth += 1;
                            i += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        (n, _) => {
                            if n == '\n' {
                                out.push('\n');
                            }
                            i += 1;
                        }
                    }
                }
            }
            ('"', _) => {
                out.push(c);
                i += 1;
                while i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                    if chars[i - 1] == '\\' && i < chars.len() {
                        out.push(chars[i]);
                        i += 1;
                    } else if chars[i - 1] == '"' {
                        break;
                    }
                }
            }
            // A char literal (`'"'`, `'\\n'`, `'x'`) is copied whole; a lifetime has no closing quote.
            ('\'', _) => {
                let len = match (next, chars.get(i + 2).copied()) {
                    (Some('\\'), _) => chars[i + 2..].iter().position(|&q| q == '\'').map(|p| p + 3),
                    (Some(_), Some('\'')) => Some(3),
                    _ => None,
                };
                let len = len.unwrap_or(1);
                out.extend(&chars[i..i + len]);
                i += len;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[test]
fn without_comments_drops_comments_and_keeps_code_and_lines() {
    let text = "a(); // self.x(\n/// fn doc(\nb(\"//not\"); /* c { */ d('\"'); e('\\'');\n/* one\ntwo */f<'a>();\n";
    assert_eq!(without_comments(text), "a(); \n\nb(\"//not\");  d('\"'); e('\\'');\n\nf<'a>();\n");
}

/// The brace-matched body of the first function in `text` whose signature
/// starts with `signature` (e.g. `"fn new_capability("`): same brace-counting
/// approach as `production_code`, reused here to pull out one function's own
/// text instead of stripping every test item.
fn function_body<'a>(text: &'a str, signature: &str) -> &'a str {
    let at = text.find(signature).unwrap_or_else(|| panic!("{signature} not found"));
    let open = text[at..].find('{').map(|o| at + o).expect("a function has a body");
    let mut depth = 0u32;
    for (j, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..open + j + 1];
                }
            }
            _ => {}
        }
    }
    panic!("{signature}: body never closes");
}

/// A `*_app.rs`'s row of `popup::POPUPS`: its `OPS` const, up to its closing `};`.
fn popup_ops_row(text: &str) -> &str {
    let at = text.find("pub(crate) const OPS: PopupOps = PopupOps {").expect("a popup row");
    let end = text[at..].find("\n};").expect("a row ends");
    &text[at..at + end]
}

#[test]
fn function_body_extracts_the_brace_matched_body() {
    let text = "fn a() { 1 }\nfn b() {\n    if true { 2 } else { 3 }\n}\n";
    assert_eq!(function_body(text, "fn a("), "{ 1 }");
    assert_eq!(function_body(text, "fn b("), "{\n    if true { 2 } else { 3 }\n}");
}

#[test]
fn production_code_strips_each_item_following_cfg_test() {
    let text = "fn x() {}\n#[cfg(test)]\nfn helper() { eprintln!(\"x\"); }\nfn y() {}\n#[cfg(test)]\nmod tests {\n    fn z() { eprintln!(\"ok\"); }\n}\n";
    assert_eq!(production_code(text), "fn x() {}\n\nfn y() {}\n\n", "two markers, not just the first or the last");
}

/// A brace-less item (`sources/dbus/mod.rs`'s `#[cfg(test)] pub mod fake;`,
/// pointing at a separate file) ends at its `;`: `find('{')` must not run
/// past it into some later, unrelated item's brace, which would wrongly
/// strip every real item in between along with it.
#[test]
fn production_code_ends_a_brace_less_item_at_its_semicolon() {
    let text = "#[cfg(test)]\npub mod fake;\nfn real_code() {}\nfn also_real() {}\n";
    assert_eq!(
        production_code(text),
        "\nfn real_code() {}\nfn also_real() {}\n",
        "only the `mod fake;` stripped, not the real items after it"
    );
}

/// `Executor::run` blocks (up to 2 s): it must never be reachable from the
/// daemon, whose event loop would freeze every bar. The only system
/// executor is built inside `NonBlocking(…)`, which refuses `run`.
#[test]
fn the_only_system_executor_is_wrapped_in_non_blocking() {
    let mut found = 0;
    for (p, t) in source_files() {
        let prod = production_code(&t);
        for (i, _) in prod.match_indices("SystemExecutor::new(") {
            found += 1;
            assert!(prod[..i].ends_with("NonBlocking("), "SystemExecutor not wrapped in NonBlocking in {}", relative_path(&p));
        }
    }
    assert_eq!(found, 1, "exactly one system executor in the daemon");
}

/// The daemon keeps exactly one command running, `pw-dump -m -N -R`, started
/// only by the sound source (the executor wrapper merely delegates).
#[test]
fn the_only_stream_is_pw_dump_monitor() {
    assert_eq!(alpheus_daemon::sources::sound::COMMAND, ["pw-dump", "-m", "-N", "-R"]);
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if production_code(&t).contains(".stream(") {
            assert!(["sources/sound.rs", "nonblocking.rs"].contains(&name.as_str()), "a stream started in {name}");
        }
    }
}

/// Nothing of the launcher is resident at rest (spec § 3, Global
/// Constraints): every place in the daemon that can start a thread, arm a
/// timer, watch a file descriptor or bind a keyboard is in this table with
/// the reason it does not run at rest. A new one fails until it is added —
/// and justified — here. Production code only (comments included: never
/// write these tokens in a comment).
#[test]
fn nothing_resident_while_the_launcher_is_closed() {
    const TOKENS: [&str; 5] = ["Timer::", "thread::Builder", "thread::spawn", "Generic::new", "get_keyboard"];
    const ALLOWED: &[(&str, &str, usize)] = &[
        // The bar's clock and resources timers.
        ("startup.rs", "Timer::", 2),
        // The launcher's request deadline, armed per connection and removed once it is read; the
        // listener's resume, armed only after an accept() error.
        ("launcher/socket_glue.rs", "Timer::", 2),
        // DeadlineTimer::default() and Timer::from_deadline: the tray's call
        // timeout, armed only while a call is pending.
        ("deadline.rs", "Timer::", 2),
        // The bar's signal guard.
        ("startup.rs", "thread::Builder", 1),
        // The launcher's listening socket (its fd: the one thing at rest) and
        // one watch per connection being read.
        ("launcher/socket_glue.rs", "Generic::new", 2),
        // The tray's session-bus fd (only inserted when "tray" is in modules and a
        // session bus answers — an unused module costs nothing at runtime, but the call itself is
        // still in the source).
        ("startup.rs", "Generic::new", 1),
        // In wayland.rs: the tray's DeadlineTimer, armed only while calls are pending; the tooltip's,
        // armed only while the pointer rests on a tray icon; the media popup's tick, armed only while
        // that popup is open and its player plays; the power menu's (power spec § 4), armed only
        // while it is open and its reboot or power-off question (3 s) is pending; the privacy
        // indicators' off-delay (privacy spec § 4), armed only while an indicator is pending off.
        // The other features' timers are built in their own files, listed below.
        ("wayland.rs", "Timer::", 5),
        // The camera scan's debounce (privacy delivery 2), built in `Camera::new`: armed only for
        // 300 ms after a device open, close, creation or removal.
        ("privacy/camera_app.rs", "Timer::", 1),
        // The quick panel's timer, built in `QuickApp::new`: armed only while it is open and a
        // re-read (150 ms, 300 ms), the reboot question (3 s) or a Wi-Fi deadline is pending, or a
        // Bluetooth call's 30 s deadline (suspended while a code shows), its 4 s `failed` or the 3 s
        // forget question (quick delivery 3) is.
        ("quick/app.rs", "Timer::", 1),
        // The focus timer's one DeadlineTimer, built in `TimerApp::new`: armed only while a timer
        // runs (at its end, and at each minute of the chip).
        ("timer/app.rs", "Timer::", 1),
        // The notification bubbles' expiry, built in `BubbleSurface::new`: armed only while a live
        // bubble (shown, or hidden by do-not-disturb) has a finite timeout; the OSD's hide timer,
        // built in `OsdSurface::new`: armed only while the pill shows.
        ("notify/bubbles_surface.rs", "Timer::", 1),
        ("osd/surface.rs", "Timer::", 1),
        // The Tools menu's confirmation timer, built in `ToolsApp::new`: armed only while a
        // confirm question is pending, disarmed by `launcher_close` and `launcher_fail`.
        ("launcher/tools/app.rs", "Timer::", 1),
        // The bar's session link's timer, built in `LinkState::new`: armed only while one of its
        // calls is pending — the portal's ReadOne, an MPRIS listing or read (2 s at most each).
        ("link.rs", "Timer::", 1),
        // The displays' one DeadlineTimer (displays spec § 1): the hotplug debounce (500 ms after a
        // change of screens), the watchdog (5 s, while a configuration waits for its answer) or the
        // trial (15 s), armed only while one of them is pending.
        ("outputs/app.rs", "Timer::", 1),
        // The timed do-not-disturb's end, built in `DndTimed::new`: armed only while a timed
        // do-not-disturb is on.
        ("notify/app.rs", "Timer::", 1),
        // The clipboard history's read deadline, built in `ClipboardApp::bind`: armed only while a
        // read is pending, disarmed by every way a read ends.
        ("clipboard/app.rs", "Timer::", 1),
        // The pad's save, armed only while it is open and edited (quick notes), built in
        // `NotesApp::new`.
        ("notes/app.rs", "Timer::", 1),
        // The Displays tab's try client's hang-up watch: only while its `outputs-try` waits for
        // the compositor's answer (the watchdog: 5 s at most), removed with the answer.
        ("outputs/app.rs", "Generic::new", 1),
        // A dmenu client's hang-up watch: only while its menu is open.
        ("launcher/host.rs", "Generic::new", 1),
        // The theme's inotify fd: readable only when theme.toml or a colour file
        // changes; `drain` reads it to EAGAIN, so it never wakes the loop at rest.
        ("watch.rs", "Generic::new", 1),
        // The camera watch's inotify fd (privacy delivery 2): readable only on an open, close,
        // creation or removal of a video device; `drain` reads it to EAGAIN.
        ("privacy/camera_app.rs", "Generic::new", 1),
        // The quick panel's acting Bluetooth connection (quick delivery 3): inserted only while the
        // Bluetooth detail is open or a call is in flight, removed with it; readable only when BlueZ
        // answers or calls the agent.
        ("quick/bt_app.rs", "Generic::new", 1),
        // The bar's own session link: only when the tray module is off
        // and mode = "auto" or the media module needs the session bus, ONE connection for both;
        // readable only when the portal or a player speaks. Its deadline timer, above, is
        // armed only while one of its calls is pending.
        ("link.rs", "Generic::new", 1),
        // The scan: started at open, ends once its catalog is delivered.
        ("launcher/scan.rs", "thread::Builder", 1),
        // The Tools menu's reader: one short-lived thread per read of states or of a
        // `choices_cmd`, started while the menu is open, ending with its last delivery.
        ("launcher/tools/read.rs", "thread::Builder", 1),
        // The keyboard, shared by the launcher and the grabbing popups: bound
        // while one of them is open, released once neither is.
        ("popup.rs", "get_keyboard", 1),
        // The Shield command or read (shield spec § 7): one worker thread, alive only while that
        // command or read runs (one action slot and one read slot, each held until the worker's
        // report), and its one DeadlineTimer, built in `ShieldApp::new`: armed only while a
        // switch waits for its flag (the 5 s grace or the 60 s bound).
        ("shield/run.rs", "thread::Builder", 1),
        ("shield/app.rs", "Timer::", 1),
        // The bar's sources (compositor, sound; D-Bus).
        ("sources/dbus/mod.rs", "thread::Builder", 1),
        ("sources/mod.rs", "thread::Builder", 1),
    ];
    let mut found: Vec<(String, &str, usize)> = Vec::new();
    for (p, t) in source_files() {
        let prod = production_code(&t);
        for token in TOKENS {
            let n = prod.matches(token).count();
            if n > 0 {
                found.push((relative_path(&p), token, n));
            }
        }
    }
    found.sort();
    let mut expected: Vec<(String, &str, usize)> = ALLOWED.iter().map(|(f, t, n)| (f.to_string(), *t, *n)).collect();
    expected.sort();
    assert_eq!(found, expected, "a thread, timer, fd watch or keyboard outside the reviewed list");
}

/// D-Bus only in src/sources/ (like the compositor's sockets), through one
/// file, which can only call read methods; no word of a method that changes
/// anything in the D-Bus sources.
#[test]
fn d_bus_is_confined_to_sources_and_read_only() {
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if uses_crate(&t, "dbus") {
            assert!(DBUS_FILES.contains(&name.as_str()), "the dbus crate used in {name}");
        }
        // bt_act.rs, Bluetooth's acting connection, has its own guardrail, below.
        if name.starts_with("sources/dbus/") && name != "sources/dbus/bt_act.rs" {
            let prod = production_code(&t);
            for word in [
                "\"Set\"",
                "\"Connect\"",
                "\"Disconnect\"",
                "\"ActivateConnection\"",
                "\"DeactivateConnection\"",
                "\"Enable\"",
                "\"GetSecrets\"",
                "\"Sleep\"",
                "\"Pair\"",
                "\"RemoveDevice\"",
                "\"StartDiscovery\"",
                "\"Suspend\"",
                "\"Hibernate\"",
                "\"PowerOff\"",
                "\"Reboot\"",
            ] {
                assert!(!prod.contains(word), "{word} in {name}");
            }
        }
    }
    let names: Vec<&str> = Method::ALL.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        [
            "GetNameOwner",
            "StartServiceByName",
            "AddMatch",
            "RemoveMatch",
            "GetAll",
            "GetManagedObjects",
            "EnumerateDevices",
            "GetDevices",
            "ListConnections",
            "GetSettings",
            "Get",
            "Subscribe"
        ]
    );
    let file = source_files().into_iter().find(|(p, _)| relative_path(p) == "sources/dbus/dbusrs_bus.rs").expect("dbusrs_bus.rs").1;
    let prod = production_code(&file);
    assert_eq!(prod.matches("send_with_reply_and_block(").count(), 1, "every call goes through `call`, which takes a `Method`");
    assert_eq!(prod.matches("Message::new_method_call(").count(), 1, "every call is built by `message`, from a `Method`");
    assert_eq!(prod.matches(".send(").count(), 1, "the only other message sent: the refusal of a call to the bar");
    assert_eq!(prod.matches(".error(").count(), 1, "that refusal is an error reply");
    for never in [
        "new_signal(",
        "method_return(",
        "request_name",
        "RequestName",
        ".add_match(",
        "Crossroads",
        "Message::signal(",
        "Message::method_call(",
        "call_with_args(",
        "return_with_args(",
    ] {
        assert!(!prod.contains(never), "{never} in dbusrs_bus.rs: the bar emits, owns and serves nothing");
    }
    for (_, rule) in MATCH_RULES {
        assert!(rule.starts_with("type='signal',"), "{rule}");
    }
    assert!(
        MATCH_RULES.iter().all(|(_, r)| !r.contains("AccessPoint")),
        "the network module is icons only (Q1): the access points' rules are the Wi-Fi detail's (WIFI_RULES), added and removed with it"
    );
    for rule in WIFI_RULES {
        assert!(
            rule.starts_with("type='signal',sender='org.freedesktop.NetworkManager',"),
            "{rule}: the detail follows NetworkManager only"
        );
    }
    assert!(
        MATCH_RULES.iter().filter(|(_, r)| r.contains("PowerProfiles")).all(|(_, r)| r.contains("'net.hadess.PowerProfiles'")),
        "power profiles: its own signals only, never a write (quick spec § 5)"
    );
    assert!(LOGIN1_RULE.starts_with("type='signal',"), "{LOGIN1_RULE}");
}

/// The files that may name the `dbus` crate: the system bus's, Bluetooth's
/// acting connection (`bt_act.rs`), and the tray's four (its connection,
/// codec, watcher and router).
const DBUS_FILES: [&str; 6] = [
    "sources/dbus/dbusrs_bus.rs",
    "sources/dbus/bt_act.rs",
    "session_bus/bus.rs",
    "session_bus/codec.rs",
    "session_bus/watcher.rs",
    "session_bus/router.rs",
];

/// Bluetooth's acting connection (quick settings delivery 3, spec § 5): the
/// only D-Bus file that changes anything, and only through the pinned
/// `ActMethod` list; one builder, one sender, auto-start off; never a name
/// requested, never the default agent, never a blocking call.
#[test]
fn the_bluetooth_connection_acts_only_through_act_method() {
    let names: Vec<&str> = ActMethod::ALL.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        [
            "GetNameOwner",
            "Set",
            "StartDiscovery",
            "StopDiscovery",
            "RemoveDevice",
            "Connect",
            "Disconnect",
            "Pair",
            "CancelPairing",
            "RegisterAgent",
            "UnregisterAgent"
        ]
    );
    let file = source_files().into_iter().find(|(p, _)| relative_path(p) == "sources/dbus/bt_act.rs").expect("bt_act.rs").1;
    let prod = production_code(&file);
    assert_eq!(prod.matches("Message::new_method_call(").count(), 1, "every call is built by `build`, from an ActMethod");
    assert_eq!(prod.matches(".send(").count(), 1, "every message leaves through `post`");
    assert_eq!(prod.matches("set_auto_start(false)").count(), 1, "in `build`: no call starts BlueZ");
    assert_eq!(prod.matches("method_return(").count(), 1, "in `answer`: the only reply it builds");
    assert_eq!(prod.matches(".error(").count(), 1, "in `answer`: the only error it builds");
    for never in [
        "send_with_reply_and_block",
        "RequestName",
        "request_name",
        "RequestDefaultAgent",
        "Crossroads",
        "blocking_pop_message",
        "RefArg",
        "new_signal(",
        "Message::method_call(",
        "call_with_args(",
        "Message::signal(",
        "return_with_args(",
        ".add_match(",
        "\"Suspend\"",
        "\"Hibernate\"",
        "\"PowerOff\"",
        "\"Reboot\"",
        "\"Sleep\"",
        "\"ActivateConnection\"",
        "\"DeactivateConnection\"",
        "\"GetSecrets\"",
        "\"Enable\"",
        // A message is addressed by `build` alone (`Message::new_method_call`'s
        // arguments), never re-aimed afterwards; no call may start BlueZ; no error but `answer`'s.
        "set_auto_start(true)",
        "set_destination(",
        "set_path(",
        "set_interface(",
        "set_member(",
        "Message::new_error(",
    ] {
        assert!(!prod.contains(never), "{never} in bt_act.rs");
    }
}

/// Bluetooth spec § 3, § 5: the acting connection serves one object, `org.bluez.Agent1` at
/// AGENT_PATH, to BlueZ's unique name only; every reply is built in `answer`.
#[test]
fn the_bluetooth_connection_serves_only_the_agent() {
    assert_eq!((AGENT_PATH, CAPABILITY), ("/org/alpheus/bluetooth/agent", "KeyboardDisplay"));
    let file = source_files().into_iter().find(|(p, _)| relative_path(p) == "sources/dbus/bt_act.rs").expect("bt_act.rs").1;
    let prod = production_code(&file);
    assert_eq!(prod.matches("method_return(").count(), 1);
    assert_eq!(prod.matches(".error(").count(), 1);
    let answer = function_body(&prod, "fn answer(");
    assert!(answer.contains("method_return(") && answer.contains(".error("), "replies only in `answer`: {answer}");
    assert_eq!(prod.matches("\"org.bluez.Agent1\"").count(), 1, "one served interface, named once (AGENT_INTERFACE)");
    let screen = function_body(&prod, "fn screen(");
    for must in ["ACCESS_DENIED", "AGENT_PATH", "AGENT_INTERFACE", "UNKNOWN_METHOD"] {
        assert!(screen.contains(must), "{must} missing from screen: {screen}");
    }
    assert!(screen.find("ACCESS_DENIED") < screen.find("UNKNOWN_METHOD"), "the sender is checked first");
}

/// The tray's D-Bus (spec § 3): dbus-crossroads
/// in the watcher only; one place builds a call, from the typed allowlist;
/// no libdbus session lookup (it could autolaunch `dbus-launch`); no
/// blocking call nor `RefArg` decoding; the only names ever requested are the
/// watcher's (DO_NOT_QUEUE) and the notification server's (flags 0: queued), never replacing an
/// owner nor letting one replace us.
#[test]
fn tray_d_bus_is_confined_and_allowlisted() {
    let mut built = Vec::new();
    let mut tray_files = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if uses_crate(&t, "dbus_crossroads") {
            assert_eq!(name, "session_bus/watcher.rs", "dbus-crossroads used in {name}");
        }
        if !name.starts_with("session_bus/") {
            continue;
        }
        tray_files += 1;
        for never in ["BusType::Session", "get_private"] {
            assert!(!t.contains(never), "{never} in {name}: libdbus's own session lookup may autolaunch a bus");
        }
        let prod = production_code(&t);
        for never in [
            "send_with_reply_and_block",
            "new_session(",
            "blocking::",
            ".process(",
            "get_refarg",
            "PropMap",
            "RefArg",
            "REPLACE_EXISTING",
            "ALLOW_REPLACEMENT",
        ] {
            assert!(!prod.contains(never), "{never} in {name}");
        }
        // "method_call(" alone catches `Message::new_method_call(` too (it is a substring of it): a
        // bare crossroads-style `ctx.method_call(` or any future way of building a call is caught the
        // same way, without double-counting the one already-allowed spelling. Consequence: no
        // private method under session_bus/ may be named `method_call` either — its definition
        // (`fn method_call(`) and every call site (`self.method_call(`) are substring matches too,
        // failing this guardrail even though the method has nothing to do with building a
        // `dbus::Message` (the router's dispatch method is named `inbound_call`, not
        // `method_call`, for exactly this reason).
        for pattern in ["method_call(", "call_with_args("] {
            built.extend(std::iter::repeat_n(name.clone(), prod.matches(pattern).count()));
        }
    }
    assert!(tray_files >= 2, "the tray's sources were not found");
    assert_eq!(built, ["session_bus/bus.rs"], "every tray call is built by bus::message, from a Call");
    let names: Vec<&str> = TrayMethod::ALL.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        [
            "Get",
            "Activate",
            "SecondaryActivate",
            "ContextMenu",
            "Scroll",
            "AboutToShow",
            "GetLayout",
            "Event",
            "RequestName",
            "GetNameOwner",
            "GetConnectionUnixProcessID",
            "ListNames",
            "AddMatch",
            "RemoveMatch",
            "RegisterStatusNotifierHost",
            "ReadOne",
            "GetAll",
            "PlayPause",
            "Next",
            "Previous",
            "Subscribe",
        ]
    );
    let (name, flags) = message(&Call::RequestName).read2::<String, u32>().unwrap();
    assert_eq!((name.as_str(), flags), (WATCHER_NAME, DBUS_NAME_FLAG_DO_NOT_QUEUE), "the one name, never replacing");
    let (name, flags) = message(&Call::RequestNotifications).read2::<String, u32>().unwrap();
    assert_eq!(
        (name.as_str(), flags, NOTIFICATIONS_NAME_FLAGS),
        (NOTIFICATIONS_NAME, 0, 0),
        "the second name: queued behind its owner, never replacing it, never replaceable"
    );
}

/// The portal: exactly two members, `ReadOne` (a call, one setting) and
/// `SettingChanged` (a signal, through one narrowed rule). Read-only, never starting it, built only by
/// `bus::message`; `appearance.rs` never names the dbus crate.
#[test]
fn the_portal_is_read_through_exactly_two_members() {
    let portal_methods: Vec<&str> = TrayMethod::ALL.iter().filter(|m| m.interface() == SETTINGS_INTERFACE).map(|m| m.name()).collect();
    assert_eq!(portal_methods, ["ReadOne"], "the only call on the portal's Settings interface");
    let m = message(&Call::ReadOne);
    assert_eq!(
        (m.destination().as_deref(), m.path().as_deref(), m.member().as_deref()),
        (Some(PORTAL_NAME), Some(PORTAL_PATH), Some("ReadOne"))
    );
    assert_eq!(m.read2::<&str, &str>().unwrap(), (APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY), "one setting, never ReadAll");
    assert!(!m.get_auto_start(), "never starts the portal");
    assert!(SETTING_RULE.starts_with("type='signal',"), "{SETTING_RULE}");
    assert!(SETTING_RULE.contains("member='SettingChanged'") && SETTING_RULE.contains("arg1='color-scheme'"), "{SETTING_RULE}");
    let file = source_files().into_iter().find(|(p, _)| relative_path(p) == "appearance.rs").expect("appearance.rs").1;
    let prod = production_code(&file);
    assert!(!uses_crate(&prod, "dbus"), "appearance.rs goes through the tray's typed calls and codec");
    assert!(!prod.contains("method_call("), "every call is built by bus::message");
    assert_eq!(
        prod.matches("Call::").count(),
        prod.matches("Call::ReadOne").count() + prod.matches("Call::AddMatch").count(),
        "only ReadOne and the rule's AddMatch"
    );
    for never in ["\"Read\"", "\"ReadAll\"", "Call::RequestName", "RemoveMatch"] {
        assert!(!prod.contains(never), "{never} in appearance.rs");
    }
}

/// Diagnostics spec § 8: the user manager is asked for one property and told `Subscribe`, nothing
/// else of systemd's. `Subscribe` is the only call on the Manager interface the session bus's
/// allowlist holds; the count is a `Get` of one property, never `GetAll` (its `Environment` may
/// hold secrets); `failed/app.rs` names no dbus type, builds no call, and logs fixed words (no
/// owner, no unit name).
#[test]
fn the_user_manager_is_asked_for_one_property_and_subscribed_to() {
    let on_manager: Vec<&str> = TrayMethod::ALL.iter().filter(|m| m.interface() == SYSTEMD_MANAGER).map(|m| m.name()).collect();
    assert_eq!(on_manager, ["Subscribe"], "the only systemd call on the session bus");
    let get = message(&Call::Get {
        destination: ":1.9".into(),
        path: SYSTEMD_PATH.into(),
        property: alpheus_daemon::session_bus::bus::Property::NFailedUnits,
    });
    assert_eq!(get.read2::<&str, &str>().unwrap(), (SYSTEMD_MANAGER, "NFailedUnits"), "one property");
    assert!(!get.get_auto_start(), "never starts the manager");
    let sub = message(&Call::SubscribeUnits { dest: ":1.9".into() });
    assert_eq!(sub.destination().as_deref(), Some(":1.9"), "to its unique name, never the well-known one");
    assert!(!sub.get_auto_start());
    assert_eq!(SYSTEMD_NAME, "org.freedesktop.systemd1");
    let file = source_files().into_iter().find(|(p, _)| relative_path(p) == "failed/app.rs").expect("failed/app.rs").1;
    let prod = production_code(&file);
    assert!(!uses_crate(&prod, "dbus"), "failed/app.rs goes through the tray's typed calls and codec");
    assert!(!prod.contains("method_call(") && !prod.contains("get_all") && !prod.contains("GetAll"), "one Get, built by bus::message");
    assert!(!prod.contains("Environment"), "never the Manager's environment");
    assert_eq!(
        prod.matches("Call::").count(),
        prod.matches("Call::AddMatch").count()
            + prod.matches("Call::GetNameOwner").count()
            + prod.matches("Call::Get {").count()
            + prod.matches("Call::SubscribeUnits").count(),
        "only the two rules, the owner, the count and the subscription"
    );
    for word in ["StartUnit", "StopUnit", "RestartUnit", "ResetFailed", "KillUnit", "Reload", "Unsubscribe", "SetEnvironment"] {
        assert!(!prod.contains(word), "{word} in failed/app.rs: the daemon only counts failed units");
    }
    // Every log line is a fixed word: nothing formatted, so no owner, no unit, no path.
    assert!(!prod.contains("log(&format!") && !prod.contains("say)(&format!"), "fixed lines only");
}

/// The files of the tray, the popups and the calendar: they run on the
/// bar's loop, never on a thread of their own, and never run a command
/// (no executor, no `actions::run`; naming `actions::Button`, as the
/// popups do, is fine): the applications act, the bar only asks them.
/// Every entry must name something that exists (a file, or a folder ending in `/`): a misspelt
/// entry would otherwise skip its file without a word (`unmatched_entries`).
const NO_THREAD_NOR_COMMAND: &[&str] = &[
    "session_bus/",
    "tray/",
    "media/mod.rs",
    "notify/mod.rs",
    "osd/mod.rs",
    "osd/surface.rs",
    "backlight.rs",
    "notify/bubbles.rs",
    "notify/bubbles_surface.rs",
    "notify/panel.rs",
    "media/popup.rs",
    "popup.rs",
    "calendar/mod.rs",
    "deadline.rs",
    "list.rs",
    "keys.rs",
    "watch.rs",
    "appearance.rs",
    "state_file.rs",
    "notify/app.rs",
    "link.rs",
    "failed/app.rs",
    "background.rs",
    "painter/",
    "placement.rs",
    "text.rs",
    "quick/mod.rs",
    "quick/draw.rs",
    "quick/state.rs",
    "quick/wifi.rs",
    "quick/secret.rs",
    "quick/bt.rs",
    "quick/vpn.rs",
    "quick/dnd.rs",
    "quick/shield.rs",
    "shield/mod.rs",
    "shield/usb.rs",
    "compositor/mango.rs",
    "sources/mango.rs",
    "compositor/sway.rs",
    "sources/sway.rs",
    "session.rs",
    "power/mod.rs",
    "privacy/mod.rs",
    "privacy/camera.rs",
    "privacy/camera_app.rs",
    "sources/dbus/bt.rs",
    "sources/dbus/bt_act.rs",
    "quick/bt_app.rs",
    "outputs/",
    "launcher/calc.rs",
    "launcher/calc/units.rs",
    "notify/dnd.rs",
    "notify/battery_alert.rs",
    "hooks/mod.rs",
    "launcher/prefix.rs",
    "launcher/emoji.rs",
    "launcher/emoji/",
    "resources/mod.rs",
    "timer/mod.rs",
    "timer/popup.rs",
    "notes/text.rs",
    "notes/lines.rs",
    "notes/keys.rs",
    "notes/model.rs",
    "notes/view.rs",
    "notes/store.rs",
    "notes/surface.rs",
    "notes/paste.rs",
    "clipboard/",
    "launcher/tools/config.rs",
    "launcher/tools/catalog.rs",
    "launcher/tools/nav.rs",
];

/// Whether the list entry `entry` names the file `name`: a folder entry (ending in `/`) names every
/// file under it, any other entry names one file exactly.
fn entry_names(entry: &str, name: &str) -> bool {
    if entry.ends_with('/') { name.starts_with(entry) } else { name == entry }
}

/// The entries of `list` that do not name what they should among the source files `names`: a file
/// entry must match exactly one of them, a folder entry (ending in `/`) at least one. A list that
/// is checked by matching its entries against the files would otherwise skip a misspelt entry in
/// silence, and the file it was meant for would stop being checked.
fn unmatched_entries<'a>(list: &[&'a str], names: &[String]) -> Vec<&'a str> {
    list.iter()
        .copied()
        .filter(|entry| {
            let hits = names.iter().filter(|name| entry_names(entry, name)).count();
            if entry.ends_with('/') { hits == 0 } else { hits != 1 }
        })
        .collect()
}

#[test]
fn unmatched_entries_names_a_misspelt_entry() {
    let names: Vec<String> = ["timer/mod.rs", "timer/app.rs", "media.rs"].map(String::from).to_vec();
    assert!(unmatched_entries(&["timer/", "media.rs", "timer/mod.rs"], &names).is_empty());
    assert_eq!(unmatched_entries(&["media.rs", "midea.rs"], &names), ["midea.rs"], "a misspelt file");
    assert_eq!(unmatched_entries(&["timer/mod.rs", "timer/ap.rs"], &names), ["timer/ap.rs"], "a misspelt file in a folder");
    assert_eq!(unmatched_entries(&["timers/"], &names), ["timers/"], "a folder with no file");
    let twice: Vec<String> = ["media.rs", "media.rs"].map(String::from).to_vec();
    assert_eq!(unmatched_entries(&["media.rs"], &twice), ["media.rs"], "a file entry matching two files");
}

#[test]
fn the_tray_spawns_no_thread() {
    let names: Vec<String> = source_files().iter().map(|(p, _)| relative_path(p)).collect();
    let unmatched = unmatched_entries(NO_THREAD_NOR_COMMAND, &names);
    assert!(unmatched.is_empty(), "NO_THREAD_NOR_COMMAND names no existing file or folder: {unmatched:?}");
    let mut checked = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !NO_THREAD_NOR_COMMAND.iter().any(|f| entry_names(f, &name)) {
            continue;
        }
        checked += 1;
        let prod = production_code(&t);
        // "spawn(": no process, task or thread started by any means (calloop's executor, a Child).
        for never in ["thread::", "Executor", "actions::run", ".stream(", "Command::new", "spawn("] {
            assert!(!prod.contains(never), "{never} in {name}");
        }
    }
    assert!(checked >= 3, "the tray's files were not found");
}

/// Whether `text` names the crate `krate`: as a path root (`dbus::…`,
/// including the explicit absolute form `::dbus::…`), an `extern crate`
/// declaration, or a `use … as` rename — never as a module further down
/// someone else's path (`sources::dbus::…`, `dbusrs_bus::…`); `::dbus::`, `use dbus as`
/// and `extern crate dbus` count too.
fn uses_crate(text: &str, krate: &str) -> bool {
    let ident = |c: char| c == '_' || c.is_alphanumeric();
    // A path root: nothing identifier-like precedes it — except the
    // explicit absolute-path `::`, which is a root too (`::dbus::…`), not a
    // continuation of someone else's path (`sources::dbus::…`).
    let root = |before: &str| match before.strip_suffix("::") {
        Some(rest) => !rest.ends_with(ident),
        None => !before.ends_with(ident),
    };
    let needle = format!("{krate}::");
    if text.match_indices(&needle).any(|(i, _)| root(&text[..i])) {
        return true;
    }
    // `extern crate dbus;` / `extern crate dbus as d;`: a word boundary
    // after the name, so `dbus_other` doesn't match.
    let extern_needle = format!("extern crate {krate}");
    if text.match_indices(&extern_needle).any(|(i, _)| !text[i + extern_needle.len()..].starts_with(ident)) {
        return true;
    }
    // `use dbus as d;`: the crate pulled in under another name.
    text.contains(&format!("use {krate} as "))
}

#[test]
fn uses_crate_tells_the_crate_from_a_module_path() {
    assert!(uses_crate("use dbus::Message;", "dbus"));
    assert!(uses_crate("fn f(e: dbus::Error) {}", "dbus"));
    assert!(!uses_crate("use crate::sources::dbus::props;", "dbus"));
    assert!(!uses_crate("dbusrs_bus::DbusRsBus::system", "dbus"));
    assert!(!uses_crate("libdbus::x", "dbus"));
    assert!(uses_crate("use ::dbus::Message;", "dbus"), "the explicit absolute path is still the crate root");
    assert!(!uses_crate("use crate::sources::dbus::props as p;", "dbus"), "a module path, even renamed, is not the crate");
    assert!(uses_crate("use dbus as d;", "dbus"), "a rename still pulls in the crate");
    assert!(uses_crate("extern crate dbus;", "dbus"));
    assert!(!uses_crate("extern crate dbus_other;", "dbus"), "a different crate sharing the prefix");
}

/// `eprintln!`/`eprint!` panic if the other end of standard error is closed
/// (EPIPE): forbidden in the daemon's production code, which must always go
/// through `alpheus_daemon::log::write` (`writeln!`, a failed write simply
/// ignored). Tests are allowed to use them: they only live a fraction of a
/// second, no risk of EPIPE like in a resident daemon.
#[test]
fn eprint_forbidden_outside_tests() {
    for (p, t) in source_files() {
        let prod = production_code(&t);
        for word in ["eprintln!(", "eprint!("] {
            assert!(!prod.contains(word), "{word} (panics on EPIPE) in {}: use crate::log::write", p.display());
        }
    }
}

/// Removes `path` (recursively) when dropped, even if the holder's scope
/// ends through a panic: unlike a plain `remove_dir_all` call at the bottom
/// of a test function, which an earlier `assert!` failure would skip.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn launcher_app(id: &str, name: &str, exec: &str, terminal: bool, actions: &[(&str, &str)]) -> DesktopEntry {
    DesktopEntry {
        id: id.into(),
        path: PathBuf::from(format!("/usr/share/applications/{id}")),
        name: name.into(),
        generic_name: None,
        comment: None,
        keywords: vec![],
        icon: None,
        exec: exec.into(),
        terminal,
        startup_wm_class: None,
        actions: actions.iter().map(|(n, e)| DesktopAction { key: n.to_lowercase(), name: n.to_string(), exec: e.to_string() }).collect(),
    }
}

/// A clipboard history row spawns nothing from the launcher: `perform` refuses it (the app copies
/// from its own history first), and no key or click on the rows reaches an executor.
#[test]
fn a_clipboard_row_spawns_nothing_from_the_launcher() {
    use alpheus_daemon::clipboard::history::ClipRow;
    let rows = || {
        (0..3).map(|i| ClipRow { id: i, preview: format!("row {i}"), first: format!("row {i}"), more_lines: 0, pinned: i == 0 }).collect()
    };
    let fake = FakeExecutor::new();
    let ctx = RunContext { exec: &fake, spawner: Spawner::Detached, history_path: None, path_dirs: &[] };
    for secondary in [false, true] {
        for i in 0..4 {
            let mut l = Launcher::for_clipboard(LauncherConfig::default(), rows());
            let o = l.click(i, secondary);
            if let Outcome::Activate(a) = o {
                assert!(matches!(a, Activation::Clip { .. }), "{a:?}");
                assert!(perform(&mut l, a, &ctx).is_err(), "perform refuses a history row");
            }
        }
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

/// Every activation the launcher can produce — every result of every mode,
/// Enter and Ctrl+Enter, typed shell text, under both spawners — only runs
/// the shapes of the allowlist (Global Constraints): an app's argv, alone
/// or in its systemd scope, after the terminal for Terminal=true; a $PATH
/// command, alone (Ctrl+Enter, detached) or held in the terminal (Enter,
/// `terminal_hold`); `sh -c` only from the Commands mode, alone or held the
/// same way; the focus template, never wrapped; a command-source entry, its
/// source's `run` with the whole line as one argv element (never `sh -c`),
/// from All only. And each shape is reached.
#[test]
fn every_launcher_activation_stays_on_the_allowlist() {
    let catalog = Catalog {
        apps: vec![
            launcher_app("org.mozilla.firefox.desktop", "Firefox", "firefox %u", false, &[("New Window", "firefox --new-window %u")]),
            launcher_app("btop.desktop", "btop++", "btop", true, &[]),
            launcher_app("files.desktop", "Files", "nautilus --new-window", false, &[]),
        ],
        commands: vec!["htop".into(), "nvim".into()],
    };
    let toplevel = |selector: &str| Toplevel {
        selector: selector.into(),
        class: "firefox".into(),
        title: "t".into(),
        workspace: "1".into(),
        focused: false,
    };
    let windows = vec![toplevel("address:0x583d9c60da90"), toplevel("id:12")];
    // A source line full of shell syntax: it must reach `run` as one argument, as is.
    let line = "Captures · Zone; echo $(id) > ~/x";
    let outils = SourceConfig {
        name: "Outils".into(),
        list: vec!["tools-menu".into(), "--list".into()],
        run: vec!["tools-menu".into(), "--run".into(), "{line}".into()],
    };
    let entries = Entries { sources: vec![outils], items: vec![SourceEntry::new(0, line)] };
    let entry_call = format!("tools-menu --run {line}");
    // Every program the fixture may start, as executable stubs on a $PATH of
    // its own (never run: the executor is fake). `_bin_guard` removes the
    // directory when this function returns, even if an assertion below
    // panics first.
    let bin = std::env::temp_dir().join(format!("alpheus-guardrails-{}-bin", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    let _bin_guard = RemoveOnDrop(bin.clone());
    for p in ["firefox", "foot", "nautilus", "htop", "nvim", "sh", "tools-menu", "xdg-open"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(bin.join(p), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_dirs = [bin.clone()];
    let apps = ["firefox", "firefox --new-window", "foot -e btop", "nautilus --new-window"];
    // Ctrl+Enter on a command: detached, bare — never through `sh`, and (2026-10-05) never
    // reachable outside the Commands mode, since a command is only ever a result there.
    let detached_commands = ["htop", "nvim"];
    // Enter on a command: held open in the terminal (`terminal_hold`, 2026-10-05).
    let terminal_hold_commands = ["foot --hold -e htop", "foot --hold -e nvim"];
    let focus = ["wm-set focus-window address:0x583d9c60da90", "wm-set focus-window id:12"];
    let scope = format!("{} ", SYSTEMD_RUN.join(" "));
    let mut seen = Vec::new();
    for spawner in [Spawner::SystemdScope, Spawner::Detached] {
        for mode in Mode::CYCLE {
            let fresh = || {
                let mut l = Launcher::for_apps(LauncherConfig::default(), windows.clone(), History::empty(200), 1_800_000_000);
                l.set_catalog(catalog.clone());
                l.set_entries(entries.clone());
                l.click_mode(mode);
                l
            };
            let mut outcomes = Vec::new();
            let n = fresh().results.len();
            for i in 0..n {
                for secondary in [false, true] {
                    outcomes.push((fresh().click(i, secondary), mode, false));
                }
            }
            let mut typed = fresh();
            for c in "ls -la".chars() {
                typed.key(Key::Char(c));
            }
            outcomes.push((typed.key(Key::Enter), mode, false));
            outcomes.push((typed.key(Key::CtrlEnter), mode, false));
            // A typed query reaches the source entries (in All only).
            let mut sourced = fresh();
            for c in "zone".chars() {
                sourced.key(Key::Char(c));
            }
            outcomes.push((sourced.key(Key::Enter), mode, false));
            outcomes.push((sourced.key(Key::CtrlEnter), mode, false));
            // The `?` row (launcher 2): `open` plus the percent-encoded URL, never the raw text.
            let mut searched = fresh();
            for c in "? rust ratatui".chars() {
                searched.key(Key::Char(c));
            }
            outcomes.push((searched.key(Key::Enter), mode, false));
            outcomes.push((searched.key(Key::CtrlEnter), mode, false));
            // `$ ls -la` typed in every mode: only All reads the prefix. Elsewhere it is plain text
            // (Commands: the whole line goes through `sh`, which the checks below pin to Commands).
            let mut dollared = fresh();
            for c in "$ ls -la".chars() {
                dollared.key(Key::Char(c));
            }
            let reads_it = mode == Mode::All;
            outcomes.push((dollared.key(Key::Enter), mode, reads_it));
            outcomes.push((dollared.key(Key::CtrlEnter), mode, reads_it));
            if mode == Mode::All {
                // The prefix route into Commands mode (Global Constraints:
                // `>` by default): starting in All and typing the prefix
                // switches mode exactly like click_mode, so typed shell
                // text after it reaches `sh` too — tagged Commands below,
                // the mode it actually ran in by the time Enter fired.
                let mut via_prefix = fresh();
                for c in ">ls -la".chars() {
                    via_prefix.key(Key::Char(c));
                }
                outcomes.push((via_prefix.key(Key::Enter), Mode::Commands, false));
            }
            for (o, mode, dollar) in outcomes {
                let Outcome::Activate(a) = o else { continue };
                let fake = FakeExecutor::new();
                let mut l = fresh();
                let _ = perform(&mut l, a, &RunContext { exec: &fake, spawner, history_path: None, path_dirs: &path_dirs });
                for c in fake.calls() {
                    assert!(!c.to_lowercase().contains("dpms"), "{c}");
                    let unwrapped = c.strip_prefix(&scope).unwrap_or(&c);
                    let shape = if focus.contains(&c.as_str()) {
                        "focus"
                    } else if unwrapped.starts_with("wm-set") {
                        panic!("the focus template wrapped or altered: {c}")
                    } else if apps.contains(&unwrapped) {
                        "program"
                    } else if detached_commands.contains(&unwrapped) {
                        assert_eq!(mode, Mode::Commands, "a detached $PATH command outside the Commands mode: {c}");
                        "detached command"
                    } else if terminal_hold_commands.contains(&unwrapped) {
                        assert_eq!(mode, Mode::Commands, "a $PATH command outside the Commands mode: {c}");
                        "terminal hold command"
                    } else if unwrapped == "xdg-open https://duckduckgo.com/?q=rust%20ratatui" {
                        assert_eq!(mode, Mode::All, "a web search outside the All mode: {c}");
                        "open"
                    } else if unwrapped == entry_call {
                        assert_eq!(mode, Mode::All, "a source entry outside the All mode: {c}");
                        "source entry"
                    } else if let Some(text) =
                        unwrapped.strip_prefix("sh -c -- ").or_else(|| unwrapped.strip_prefix("foot --hold -e sh -c -- "))
                    {
                        let held = !unwrapped.starts_with("sh");
                        if dollar {
                            // From All, only the text that followed the `$`.
                            assert_eq!(mode, Mode::All, "a `$` row outside the All mode: {c}");
                            assert_eq!(text, "ls -la", "only the text after the `$` goes through the shell: {c}");
                            if held { "prefix terminal hold shell" } else { "prefix shell" }
                        } else {
                            assert_eq!(mode, Mode::Commands, "a shell outside the Commands mode: {c}");
                            assert!(
                                matches!(text, "ls -la" | "$ ls -la" | "zone" | "? rust ratatui"),
                                "only typed text goes through the shell: {c}"
                            );
                            if held { "terminal hold shell" } else { "shell" }
                        }
                    } else {
                        panic!("command outside the allowlist: {c}")
                    };
                    seen.push((shape, c.starts_with(&scope)));
                }
            }
        }
    }
    for expected in [
        ("program", true),
        ("program", false),
        ("detached command", false),
        ("terminal hold command", true),
        ("shell", true),
        ("terminal hold shell", false),
        ("prefix shell", true),
        ("prefix terminal hold shell", false),
        ("focus", false),
        ("source entry", true),
        ("source entry", false),
        ("open", true),
        ("open", false),
    ] {
        assert!(seen.contains(&expected), "{expected:?} never reached: {seen:?}");
    }
    assert!(!seen.contains(&("focus", true)), "focus never in a scope");
    // The prefix is the All mode's alone: a dmenu/script box (a password prompt too), the
    // calculator and the window switcher never run it.
    let password = alpheus_common::launcher_ipc::DmenuFlags { password: true, ..Default::default() };
    for mut l in [
        Launcher::for_dmenu(LauncherConfig::default(), None, vec!["a".into(), "ls -la".into()], Default::default()),
        Launcher::for_dmenu(LauncherConfig::default(), None, vec!["ls -la".into()], password),
        Launcher::for_calc(LauncherConfig::default()),
        Launcher::for_windows(LauncherConfig::default(), windows.clone()),
    ] {
        for c in "$ ls -la".chars() {
            l.key(Key::Char(c));
        }
        for o in [l.key(Key::Enter), l.key(Key::CtrlEnter)] {
            assert!(!matches!(o, Outcome::Activate(Activation::Shell { .. })), "`$` outside All reached the shell");
        }
    }
    // No click on a non-prefix result in All ever reaches `sh`: the `$` row is the only way in.
    for query in ["", "ls -la", "zone", "firefox", "htop"] {
        let in_all = || {
            let mut l = Launcher::for_apps(LauncherConfig::default(), windows.clone(), History::empty(200), 1_800_000_000);
            l.set_catalog(catalog.clone());
            l.set_entries(entries.clone());
            for c in query.chars() {
                l.key(Key::Char(c));
            }
            l
        };
        for i in 0..in_all().results.len() {
            for secondary in [false, true] {
                let o = in_all().click(i, secondary);
                assert!(!matches!(o, Outcome::Activate(Activation::Shell { .. })), "{query:?}: click {i} reached the shell");
            }
        }
    }
    // The joined call lines above cannot tell one argument from several: the entry's argv, as
    // given to the executor, is the source's `run` with the line as exactly one element.
    for spawner in [Spawner::SystemdScope, Spawner::Detached] {
        let mut l = Launcher::for_apps(LauncherConfig::default(), vec![], History::empty(200), 1_800_000_000);
        l.set_entries(entries.clone());
        for c in "zone".chars() {
            l.key(Key::Char(c));
        }
        let Outcome::Activate(a) = l.key(Key::Enter) else { panic!("Enter on a source entry activates nothing") };
        let spy = ArgvSpy::default();
        let _ = perform(&mut l, a, &RunContext { exec: &spy, spawner, history_path: None, path_dirs: &path_dirs });
        let calls = spy.0.lock().unwrap().clone();
        let scoped: Vec<String> = SYSTEMD_RUN.iter().map(|s| s.to_string()).collect();
        let argv = calls.first().expect("the entry ran nothing");
        let argv = if spawner == Spawner::SystemdScope { argv.strip_prefix(&scoped[..]).expect("not in its scope") } else { &argv[..] };
        assert_eq!(argv, ["tools-menu", "--run", line], "{spawner:?}: the line is one argv element, as is");
        assert!(!argv.iter().any(|a| a == "sh" || a == "-c"), "never through a shell: {argv:?}");
    }
    // The calculator (small features 1): Enter and Ctrl+Enter on its row, in its own mode and in All,
    // reach exactly one call, the configured `copy` command, the text on its input and never in its
    // argv, never in a scope, never through a shell; and Copy comes from the calculator row alone.
    let copy_bin = bin.join("copy-stub");
    std::fs::create_dir_all(&copy_bin).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(copy_bin.join("wl-copy"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(copy_bin.join("wl-copy"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let copy_dirs = [copy_bin];
    let config = LauncherConfig::default();
    for spawner in [Spawner::SystemdScope, Spawner::Detached] {
        let fresh = |in_calc_mode: bool| {
            if in_calc_mode {
                Launcher::for_calc(config.clone())
            } else {
                let mut l = Launcher::for_apps(config.clone(), windows.clone(), History::empty(200), 1_800_000_000);
                l.set_catalog(catalog.clone());
                l.set_entries(entries.clone());
                l
            }
        };
        for in_calc_mode in [true, false] {
            for (key, input) in [(Key::Enter, "42"), (Key::CtrlEnter, "6*7 = 42")] {
                let mut l = fresh(in_calc_mode);
                for c in "6*7".chars() {
                    l.key(Key::Char(c));
                }
                assert_eq!(l.selected_hit().map(|h| h.source), Some(alpheus_daemon::launcher::matching::Source::Calc));
                let Outcome::Activate(a) = l.key(key) else { panic!("{key:?} on the calculator row activates nothing") };
                let fake = FakeExecutor::new();
                fake.ok("wl-copy", "");
                let ctx = RunContext { exec: &fake, spawner, history_path: None, path_dirs: &copy_dirs };
                assert_eq!(perform(&mut l, a, &ctx), Ok(alpheus_daemon::launcher::run::Done::Close));
                assert_eq!(fake.calls(), ["wl-copy"], "{spawner:?}: one call, the copy program alone: no scope, no shell, no text in argv");
                assert_eq!(fake.inputs(), [("wl-copy".to_string(), input.as_bytes().to_vec())], "{spawner:?} {key:?}");
                let spy = ArgvSpy::default();
                let mut l = fresh(in_calc_mode);
                for c in "6*7".chars() {
                    l.key(Key::Char(c));
                }
                let Outcome::Activate(a) = l.key(key) else { unreachable!() };
                let _ = perform(&mut l, a, &RunContext { exec: &spy, spawner, history_path: None, path_dirs: &copy_dirs });
                let calls = spy.0.lock().unwrap().clone();
                assert_eq!(calls, std::slice::from_ref(&config.copy), "the argv is `copy` unchanged: {calls:?}");
            }
        }
    }
    // The emoji (launcher 2): in All, `:grinning face`, Enter and Ctrl+Enter reach exactly one call,
    // the copy program alone, the emoji or its name on its input, never in argv nor in a scope.
    for spawner in [Spawner::SystemdScope, Spawner::Detached] {
        for (key, input) in [(Key::Enter, "\u{1f600}"), (Key::CtrlEnter, "grinning face")] {
            let fresh = || {
                let mut l = Launcher::for_apps(config.clone(), windows.clone(), History::empty(200), 1_800_000_000);
                l.set_catalog(catalog.clone());
                for c in ":grinning face".chars() {
                    l.key(Key::Char(c));
                }
                l
            };
            let mut l = fresh();
            assert!(matches!(l.selected_hit().map(|h| h.source), Some(alpheus_daemon::launcher::matching::Source::Emoji(_))));
            let Outcome::Activate(a) = l.key(key) else { panic!("{key:?} on an emoji row activates nothing") };
            let fake = FakeExecutor::new();
            fake.ok("wl-copy", "");
            let ctx = RunContext { exec: &fake, spawner, history_path: None, path_dirs: &copy_dirs };
            assert_eq!(perform(&mut l, a, &ctx), Ok(alpheus_daemon::launcher::run::Done::Close));
            assert_eq!(fake.calls(), ["wl-copy"], "{spawner:?}: one call, the copy program alone: no scope, no shell, no text in argv");
            assert_eq!(fake.inputs(), [("wl-copy".to_string(), input.as_bytes().to_vec())], "{spawner:?} {key:?}");
            let spy = ArgvSpy::default();
            let mut l = fresh();
            let Outcome::Activate(a) = l.key(key) else { unreachable!() };
            let _ = perform(&mut l, a, &RunContext { exec: &spy, spawner, history_path: None, path_dirs: &copy_dirs });
            let calls = spy.0.lock().unwrap().clone();
            assert_eq!(calls, std::slice::from_ref(&config.copy), "the argv is `copy` unchanged: {calls:?}");
        }
    }
    // Copy is reached only from a Calc or an emoji hit: every other result of every mode, clicked
    // either way, never gives one.
    // A math query is part of the sweep: the positive case, the calculator's own row copies.
    let mut calc_copies = 0;
    for mode in Mode::CYCLE {
        let mut l = Launcher::for_apps(config.clone(), windows.clone(), History::empty(200), 1_800_000_000);
        l.set_catalog(catalog.clone());
        l.set_entries(entries.clone());
        l.click_mode(mode);
        for query in ["", "fire", "zone", "htop", "ls -la", "6*7"] {
            for c in query.chars() {
                l.key(Key::Char(c));
            }
            for i in 0..l.results.len() {
                for secondary in [false, true] {
                    let source = l.results[i].source;
                    let is_calc = source == alpheus_daemon::launcher::matching::Source::Calc;
                    let o = l.click(i, secondary);
                    let copies = matches!(&o, Outcome::Activate(a) if format!("{a:?}").starts_with("Copy"));
                    assert!(
                        !copies || is_calc || matches!(source, alpheus_daemon::launcher::matching::Source::Emoji(_)),
                        "{mode:?} {query:?}: Copy from a result that is neither the calculator nor an emoji"
                    );
                    if is_calc {
                        assert!(copies, "{mode:?} {query:?}: the calculator's row, clicked, copies");
                        calc_copies += 1;
                    }
                }
            }
            l.key(Key::CtrlU);
        }
    }
    assert!(calc_copies > 0, "the sweep never met a calculator row: the positive case is vacuous");
}

/// Records each call's argv as given (program first), element by element; every call fails.
#[derive(Default)]
struct ArgvSpy(std::sync::Mutex<Vec<Vec<String>>>);

impl ArgvSpy {
    fn record(&self, prog: &str, args: &[&str]) -> ExecError {
        self.0.lock().unwrap().push(std::iter::once(prog).chain(args.iter().copied()).map(String::from).collect());
        ExecError::NotFound
    }
}

impl Executor for ArgvSpy {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError> {
        Err(self.record(prog, args))
    }
    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        Err(self.record(prog, args))
    }
    fn spawn_with_input(&self, prog: &str, args: &[&str], _input: &[&[u8]]) -> Result<(), ExecError> {
        Err(self.record(prog, args))
    }
    fn spawn_with_env(&self, prog: &str, args: &[&str], _env: &[(&str, &str)], _unset: &[&str]) -> Result<(), ExecError> {
        Err(self.record(prog, args))
    }
    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError> {
        Err(self.record(prog, args))
    }
    fn capture(&self, prog: &str, args: &[&str], _limits: CaptureLimits) -> Result<Captured, ExecError> {
        Err(self.record(prog, args))
    }
    fn run_silent(&self, prog: &str, args: &[&str]) -> Result<Option<i32>, ExecError> {
        Err(self.record(prog, args))
    }
}

/// The daemon paints with the 16 roles only. The old 7-colour
/// `Palette` is never named in its sources.
#[test]
fn the_daemon_paints_with_roles_never_the_legacy_palette() {
    let files = all_source_files();
    for (p, t) in &files {
        assert!(!t.contains("Palette"), "the legacy Palette in {}", relative_path(p));
    }
    assert!(files.len() > 20, "the daemon's sources were not found");
}

/// Privacy delivery 2: the config directory and the video devices are only watched,
/// never written by the daemon; inotify lives in inotify.rs (the wrapper), watch.rs (the theme) and
/// privacy/camera.rs (the cameras) alone.
#[test]
fn the_theme_and_camera_watches_only_read() {
    let mut found = false;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = production_code(&t);
        // privacy/camera.rs (privacy delivery 2) holds the second inotify instance, on /dev's video nodes.
        if name == "watch.rs" || name == "privacy/camera.rs" || name == "inotify.rs" {
            found |= name == "watch.rs";
            for never in
                ["fs::write", "create_dir", "OpenOptions", "remove_file", "remove_dir", "fs::rename", "set_permissions", "File::create"]
            {
                assert!(!prod.contains(never), "{never} in {name}: the config directory and the video devices are read-only to the daemon");
            }
        } else {
            for raw in ["inotify_", "Inotify::new", "inotify::"] {
                assert!(!prod.contains(raw), "inotify ({raw}) outside inotify.rs, watch.rs and privacy/camera.rs: {name}");
            }
        }
    }
    assert!(found, "watch.rs not found");
}

/// The theme is resolved once more after everything it depends on is up: after the
/// watch is armed (a file written between the first read and the watch would be missed for the
/// whole session), and after `start_tray` and `follow_portal_if_auto` (earlier, the portal start
/// sees no bus and gives up for good).
#[test]
fn the_startup_theme_is_resolved_again_after_the_watch_and_the_portal() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/startup.rs")).unwrap();
    let prod = production_code(&text);
    let at = |needle: &str| prod.find(needle).unwrap_or_else(|| panic!("{needle} not found in startup.rs"));
    let last = at("state.theme_changed();");
    assert_eq!(prod.matches("state.theme_changed();").count(), 1);
    for before in ["crate::watch::start(", "start_tray(state, handle)", "state.follow_portal_if_auto();", "state.publish_mode();"] {
        assert!(at(before) < last, "{before} must come before the startup re-resolve");
    }
}

/// Timed do-not-disturb (small features 1, decisions 7 and 8): the timer's callback calls `fired()`
/// before anything arms the timer again, and the clock's minute tick re-checks the wall-clock end
/// (a suspended laptop's monotonic timer is late).
#[test]
fn the_dnd_tick_fires_before_checking() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/notify/app.rs")).unwrap();
    let start = text.find("fn dnd_tick(").expect("dnd_tick");
    let body = &text[start..];
    let body = &body[..body.find("\n    }\n").expect("the end of dnd_tick")];
    let fired = body.find(".fired()").expect("dnd_tick calls fired()");
    let check = body.find("dnd_check_clock").expect("dnd_tick checks the clock");
    assert!(fired < check, "fired() must come before dnd_check_clock in dnd_tick");
}

#[test]
fn the_clock_rechecks_a_timed_dnd() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/startup.rs")).unwrap();
    let start = text.find("Msg::Clock(h)").expect("the clock closure");
    let end = start + text[start..].find("TimeoutAction::ToDuration").expect("the clock closure's end");
    assert!(text[start..end].contains("dnd_check_clock()"), "the clock tick must call dnd_check_clock()");
}

/// One path for every do-not-disturb change: `self.dnd =` only in `dnd_apply` and `dnd_load`,
/// `state.dnd =` nowhere (startup goes through `dnd_load`).
#[test]
fn every_dnd_change_goes_through_dnd_apply() {
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = without_comments(&production_code(&t));
        assert!(!prod.contains("state.dnd ="), "state.dnd = in {name}");
        // notify/mod.rs's `Center` has a `dnd` of its own (the server's copy, told by `set_dnd`).
        if name == "notify/app.rs" || name == "notify/mod.rs" {
            continue;
        }
        assert!(!prod.contains("self.dnd ="), "self.dnd = in {name}");
    }
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/notify/app.rs")).unwrap();
    let prod = without_comments(&production_code(&text));
    let mut owner = String::new();
    let mut allowed = true;
    for line in prod.lines() {
        if let Some(i) = line.find("fn ") {
            owner = line[i + 3..].split('(').next().unwrap_or_default().to_string();
        }
        if line.contains("self.dnd =") {
            allowed &= owner == "dnd_apply" || owner == "dnd_load";
        }
    }
    assert!(prod.contains("self.dnd ="), "dnd_apply sets it");
    assert!(allowed, "self.dnd = outside dnd_apply and dnd_load in notify/app.rs");
}

/// No panic in the notification server's production code: its model,
/// consumer, bubbles, panel (and its wiring) and state files, and the signal builders in bus.rs, hold no `.expect(`,
/// `.unwrap()` nor `panic!(` in their production code (`production_code`: everything but the items
/// that follow `#[cfg(test)]`).
#[test]
fn the_notification_server_has_no_panicking_path() {
    let panics = |text: &str| [".expect(", ".unwrap()", "panic!("].iter().any(|p| text.contains(p));
    let files = source_files();
    let text =
        |name: &str| files.iter().find(|(p, _)| relative_path(p) == name).map(|(_, t)| t.clone()).unwrap_or_else(|| panic!("{name}"));
    for name in [
        "notify/mod.rs",
        "session_bus/notifications.rs",
        "notify/app.rs",
        "notify/dnd.rs",
        "notify/bubbles.rs",
        "notify/bubbles_surface.rs",
        "notify/panel.rs",
        "notify/panel_app.rs",
        "state_file.rs",
        "session_bus/codec.rs",
    ] {
        let t = text(name);
        assert!(!panics(&production_code(&t)), "a panicking call in {name}'s production code");
    }
    let bus = text("session_bus/bus.rs");
    let start = bus.find("pub fn notify_reply").expect("the notification builders in bus.rs");
    let end = bus.find("pub fn session_address").expect("the builders end before session_address");
    assert!(!panics(&bus[start..end]), "a panicking call in bus.rs's notification builders");
}

/// The OSD's three files, each checked by the guardrails below; every entry must name a file
/// (`unmatched_entries`), so a rename cannot take one out of the checks in silence.
const OSD_FILES: &[&str] = &["osd/mod.rs", "osd/surface.rs", "backlight.rs"];

/// OSD spec § 2 and its guardrails: the OSD reads sysfs and shows, nothing else. Its three files
/// write no file, create nothing, change no permission, make no raw system call (no `libc::`, no
/// `unsafe`), and, through `NO_THREAD_NOR_COMMAND`, run no command. Production code only.
#[test]
fn the_osd_only_reads() {
    let names: Vec<String> = source_files().iter().map(|(p, _)| relative_path(p)).collect();
    let unmatched = unmatched_entries(OSD_FILES, &names);
    assert!(unmatched.is_empty(), "OSD_FILES names no existing file: {unmatched:?}");
    let mut checked = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !OSD_FILES.contains(&name.as_str()) {
            continue;
        }
        checked += 1;
        let prod = production_code(&t);
        for never in [
            "fs::write",
            "OpenOptions",
            "File::create",
            "File::options(",
            "set_permissions",
            "remove_file",
            "create_dir",
            "fs::rename",
            "fs::copy",
            "libc::",
            "unsafe",
        ] {
            assert!(!prod.contains(never), "{never} in {name}: the OSD only reads");
        }
    }
    assert_eq!(checked, 3, "osd/mod.rs, osd/surface.rs and backlight.rs");
}

/// No panic in the OSD's production code (spec § 5): with its tests left out, each file has no
/// `.expect(`, `.unwrap()` nor `panic!(`.
#[test]
fn the_osd_has_no_panicking_path() {
    let files = source_files();
    let names: Vec<String> = files.iter().map(|(p, _)| relative_path(p)).collect();
    let unmatched = unmatched_entries(OSD_FILES, &names);
    assert!(unmatched.is_empty(), "OSD_FILES names no existing file: {unmatched:?}");
    for &name in OSD_FILES {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Resources popup: no panic in the model's production code, nor in the wiring's (it runs on every
/// resources tick while the popup is open).
#[test]
fn the_resources_popup_has_no_panicking_path() {
    let files = source_files();
    for name in ["resources/mod.rs", "resources/app.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Quick spec § 5, § 6: the panel's model and its state reader read and decide, nothing else: no
/// file written, nothing created, no raw system call; and, through `NO_THREAD_NOR_COMMAND`, no
/// command run. Production code only.
#[test]
fn the_quick_model_and_its_state_reader_only_read() {
    const QUICK_MODEL_FILES: &[&str] = &[
        "quick/mod.rs",
        "quick/draw.rs",
        "quick/state.rs",
        "quick/wifi.rs",
        "quick/bt.rs",
        "quick/vpn.rs",
        "quick/dnd.rs",
        "quick/shield.rs",
    ];
    let names: Vec<String> = source_files().iter().map(|(p, _)| relative_path(p)).collect();
    let unmatched = unmatched_entries(QUICK_MODEL_FILES, &names);
    assert!(unmatched.is_empty(), "QUICK_MODEL_FILES names no existing file: {unmatched:?}");
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !QUICK_MODEL_FILES.contains(&name.as_str()) {
            continue;
        }
        let prod = production_code(&t);
        for never in [
            "fs::write",
            "OpenOptions",
            "File::create",
            "File::options(",
            "set_permissions",
            "remove_file",
            "create_dir",
            "fs::rename",
            "fs::copy",
            "libc::",
            "unsafe",
        ] {
            assert!(!prod.contains(never), "{never} in {name}: the quick panel's model only reads");
        }
    }
}

/// Quick spec § 5: every command the panel can build — every key on every row (twice: the
/// confirmation's second activation), the outputs detail, every press of every cell with every
/// button (twice), the wheel both ways — is one of the exact shapes; the configured commands run as
/// given. And each shape is reached.
#[test]
fn every_quick_command_stays_on_the_allowlist() {
    let config = QuickConfig {
        keep_awake: ToggleConfig { toggle: vec!["fake-keep-awake".into()], state: String::new() },
        night_light: ToggleConfig { toggle: vec!["fake-night".into(), "--toggle".into()], state: String::new() },
        logout: vec!["fake-logout".into()],
        wifi_more: Some(vec!["fake-more".into()]),
        vpn_more: Some(vec!["fake-vpn-more".into()]),
        mixer: Some(vec!["fake-mixer".into()]),
        ..QuickConfig::default()
    };
    let sound = SoundState {
        sink: Some(Volume { percent: 64, muted: false }),
        source: Some(Volume { percent: 100, muted: false }),
        ..SoundState::default()
    };
    let profile = ProfileState { active: Some(Profile::Balanced), offered: Profile::ALL.to_vec() };
    let live = Live {
        sound: Some(&sound),
        dnd: Some(false),
        dnd_end: None,
        profile: Some(&profile),
        config: &config,
        bluetooth: None,
        shield: None,
    };
    let sinks = vec![Sink { id: 99, label: "Speaker".into(), default: false }, Sink { id: 122, label: "AG06/AG03".into(), default: true }];
    let backlight = Backlight { device: "intel_backlight".into(), percent: 50 };
    let fresh = || {
        let mut q = Quick::new(Focus::Top, &live, sinks.clone(), Some(backlight.clone()), [Some(false), None], 40);
        q.set_headset(Some(Headset { device: 61, label: "CasqueBT".into(), active: HeadsetMode::HiFi, hifi: 1, mic: 3 }));
        q
    };
    let t = Instant::now();
    // A second activation counts only from CONFIRM_AFTER on; past COALESCE too.
    let later = t + quick::CONFIRM_AFTER.max(quick::COALESCE);
    let mut calls: Vec<String> = Vec::new();
    let mut record = |a: QuickAction| {
        if let QuickAction::Run { argv, .. } | QuickAction::RunAndClose { argv, .. } | QuickAction::RunSecret { argv, .. } = a {
            calls.push(argv.join(" "));
        }
    };
    let keys = [Key::Up, Key::Down, Key::Left, Key::Right, Key::Char(' '), Key::Enter, Key::Home, Key::End, Key::Escape];
    for row in quick::shown(&live, &fresh()) {
        for k in keys {
            let mut q = fresh();
            q.selected = row;
            record(q.key(&live, k, false, t));
            record(q.key(&live, k, false, later));
        }
    }
    let mut q = fresh();
    for k in [Key::Enter, Key::Up, Key::Enter, Key::Down, Key::Enter] {
        record(q.key(&live, k, false, t));
    }
    let lines = quick::lines(&live, &fresh()).len() as u16;
    for y in 0..lines {
        for x in 0..quick::WIDTH + 2 {
            for b in [Button::Left, Button::Right, Button::Middle] {
                let mut q = fresh();
                record(q.press(&live, (x, y), b, t));
                record(q.press(&live, (x, y), b, later));
            }
            for steps in [-1, 1, -3] {
                record(fresh().scroll(&live, Some((x, y)), steps, t));
            }
        }
    }
    // Quick Wi-Fi spec § 5: the Wi-Fi row and its detail, the radio on and off, a network of every
    // kind; every key on every pick (twice), every press of every cell with every button (twice).
    const UUID_HOME: &str = "11111111-2222-4333-8444-555555555555";
    const UUID_SAVED: &str = "33333333-4444-4555-8666-777777777777";
    let ap = |s: &str, strength: u8, security: Security| AccessPoint { ssid: s.as_bytes().to_vec(), strength, security };
    let wifi_state = |enabled: bool| WifiState {
        enabled,
        device: Some(WifiDevice {
            state: 100,
            active: Some(ap("Maison-Test", 80, Security::Psk)),
            active_uuid: Some(UUID_HOME.into()),
            access_points: Some(vec![
                ap("Maison-Test", 80, Security::Psk),
                ap("Bureau-Test", 60, Security::Psk),
                ap("Cafe-Libre", 40, Security::Open),
                ap("Labo-Test", 70, Security::Enterprise),
                ap("Vieux-Test", 50, Security::Other),
                ap("-x", 30, Security::Open),
                ap("Atelier-Test", 20, Security::Open),
            ]),
            saved: Some(vec![
                SavedWifi { uuid: UUID_HOME.into(), ssid: b"Maison-Test".to_vec() },
                SavedWifi { uuid: UUID_SAVED.into(), ssid: b"Atelier-Test".to_vec() },
            ]),
        }),
        stale: false,
    };
    for enabled in [true, false] {
        let fresh_wifi = || {
            let mut q = fresh();
            q.set_wifi(Some(wifi_state(enabled)), t);
            q.selected = quick::Row::Wifi;
            q
        };
        for k in keys {
            record(fresh_wifi().key(&live, k, false, t));
        }
        let mut opened = fresh_wifi();
        record(opened.key(&live, Key::Enter, false, t));
        let picks = opened.wifi.networks().len() + 1;
        for down in 0..=picks {
            for k in keys {
                let mut q = opened.clone();
                for _ in 0..down {
                    q.key(&live, Key::Down, false, t);
                }
                record(q.key(&live, k, false, t));
                record(q.key(&live, k, false, later));
            }
        }
        let n = quick::lines(&live, &opened).len() as u16;
        for y in 0..n {
            for x in 0..quick::WIDTH + 2 {
                for b in [Button::Left, Button::Right, Button::Middle] {
                    let mut q = opened.clone();
                    record(q.press(&live, (x, y), b, t));
                    record(q.press(&live, (x, y), b, later));
                }
            }
        }
        // The password field, then a failure with the profile nmcli made: `--ask`, then the cleanup.
        const UUID_NEW: &str = "22222222-3333-4444-8555-666666666666";
        let mut q = opened.clone();
        q.wifi.pick = quick::wifi::Pick::Network(b"Bureau-Test".to_vec());
        record(q.key(&live, Key::Enter, false, t));
        for ch in "fake-pass".chars() {
            record(q.key(&live, Key::Char(ch), false, t));
        }
        record(q.key(&live, Key::Enter, false, t));
        let mut failing = wifi_state(enabled);
        if let Some(d) = failing.device.as_mut() {
            d.saved.get_or_insert_with(Vec::new).push(SavedWifi { uuid: UUID_NEW.into(), ssid: b"Bureau-Test".to_vec() });
            d.state = 60;
        }
        record(q.set_wifi(Some(failing.clone()), t).action);
        if let Some(d) = failing.device.as_mut() {
            d.state = 30;
        }
        record(q.set_wifi(Some(failing), t).action);
        record(q.due(t + quick::wifi::CONNECT_TIMEOUT).run.into_iter().next().unwrap_or(QuickAction::Nothing));
    }
    // Quick VPN spec: the VPN row and its detail with one profile of each kind, active or not; every
    // key on every pick (twice), every press of every cell with every button (twice).
    const UUID_VPN_1: &str = "00000000-0000-4000-8000-000000000001";
    const UUID_VPN_2: &str = "00000000-0000-4000-8000-000000000002";
    let vpn_state = |active: bool| VpnState {
        profiles: vec![
            VpnProfile { uuid: UUID_VPN_1.into(), name: "VPN-Test-1".into(), kind: VpnKind::Plugin, last_used: 10, asks: false },
            VpnProfile { uuid: UUID_VPN_2.into(), name: "WG-Test".into(), kind: VpnKind::WireGuard, last_used: 20, asks: true },
        ],
        active: if active { vec![ActiveVpn { uuid: UUID_VPN_1.into(), state: 2 }] } else { Vec::new() },
    };
    for active in [false, true] {
        let fresh_vpn = || {
            let mut q = fresh();
            q.set_vpn(Some(vpn_state(active)), t);
            q.selected = quick::Row::Vpn;
            q
        };
        for k in keys {
            record(fresh_vpn().key(&live, k, false, t));
        }
        let mut opened = fresh_vpn();
        record(opened.key(&live, Key::Enter, false, t));
        for down in 0..=3 {
            for k in keys {
                let mut q = opened.clone();
                for _ in 0..down {
                    q.key(&live, Key::Down, false, t);
                }
                record(q.key(&live, k, false, t));
                record(q.key(&live, k, false, later));
            }
        }
        let n = quick::lines(&live, &opened).len() as u16;
        for y in 0..n {
            for x in 0..quick::WIDTH + 2 {
                for b in [Button::Left, Button::Right, Button::Middle] {
                    let mut q = opened.clone();
                    record(q.press(&live, (x, y), b, t));
                    record(q.press(&live, (x, y), b, later));
                }
            }
        }
    }
    // Quick Bluetooth spec § 2: Space on the row with rfkill holding the adapter off unblocks it.
    let bt_off = Live { bluetooth: Some(&BtState::Off), ..live };
    let mut q = Quick::new(Focus::Top, &bt_off, sinks.clone(), Some(backlight.clone()), [Some(false), None], 40);
    q.bt.view =
        Some(BtView { adapter: Some(BtAdapter { path: "/org/bluez/hci0".into(), powered: false, blocked: true }), ..BtView::default() });
    q.selected = quick::Row::Bluetooth;
    record(q.key(&bt_off, Key::Char(' '), false, t));
    // The paced paths: a second slider key inside the window waits; the
    // window's end (`due`) or the panel closing (`flush`) sends it. Each path is driven on its own.
    let line = |a: &QuickAction| match a {
        QuickAction::Run { argv, .. } | QuickAction::RunAndClose { argv, .. } => Some(argv.join(" ")),
        _ => None,
    };
    let soon = t + Duration::from_millis(1);
    let (mut by_due, mut by_flush) = (Vec::new(), Vec::new());
    for row in quick::shown(&live, &fresh()) {
        for k in keys {
            let mut q = fresh();
            q.selected = row;
            record(q.key(&live, k, false, t));
            record(q.key(&live, k, false, soon));
            by_flush.extend(q.clone().flush().iter().filter_map(line));
            by_due.extend(q.due(t + quick::COALESCE).run.iter().filter_map(line));
            by_flush.extend(q.flush().iter().filter_map(line));
        }
    }
    for paced in [
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 74%",
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 54%",
        "brightnessctl --class=backlight --device=intel_backlight set 60%",
        "brightnessctl --class=backlight --device=intel_backlight set 40%",
    ] {
        assert!(by_due.iter().any(|c| c == paced), "{paced} never sent by due(): {by_due:?}");
        assert!(by_flush.iter().any(|c| c == paced), "{paced} never sent by flush(): {by_flush:?}");
    }
    calls.extend(by_due);
    calls.extend(by_flush);
    // Absolute levels only, 0 to 100 (`min` up); never a relative step.
    let percent = |n: &str, min: u8| {
        n.strip_suffix('%')
            .filter(|d| !d.is_empty() && d.len() <= 3 && d.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|d| d.parse::<u8>().ok())
            .is_some_and(|p| (min..=100).contains(&p))
    };
    let configured = [
        "fake-keep-awake",
        "fake-night --toggle",
        "fake-logout",
        "loginctl lock-session",
        "systemctl reboot",
        "systemctl poweroff",
        "fake-more",
        "fake-vpn-more",
        "fake-mixer",
    ];
    for c in &calls {
        let ok = match c.split(' ').collect::<Vec<_>>().as_slice() {
            ["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", n] => percent(n, 0),
            ["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@" | "@DEFAULT_AUDIO_SOURCE@", "toggle"] => true,
            ["wpctl", "set-default", id] => ["99", "122"].contains(id),
            ["wpctl", "set-profile", id, idx] => *id == "61" && ["1", "3"].contains(idx),
            ["brightnessctl", "--class=backlight", "--device=intel_backlight", "set", n] => percent(n, 1), // never below 1 % (docs/decisions.md)
            ["powerprofilesctl", "set", p] => ["power-saver", "balanced", "performance"].contains(p),
            ["nmcli", "radio", "wifi", "on" | "off"] => true,
            ["nmcli", "device", "wifi", "rescan"] => true,
            ["nmcli", "connection", "up" | "down", "uuid", u] => [UUID_HOME, UUID_SAVED, UUID_VPN_1, UUID_VPN_2].contains(u),
            ["nmcli", "device", "wifi", "connect", ssid] => *ssid == "Cafe-Libre",
            ["nmcli", "--ask", "device", "wifi", "connect", ssid] => *ssid == "Bureau-Test",
            ["nmcli", "connection", "delete", "uuid", u] => *u == "22222222-3333-4444-8555-666666666666",
            ["rfkill", "unblock", "bluetooth"] => true,
            _ => configured.contains(&c.as_str()),
        };
        assert!(ok, "command outside the allowlist: {c}");
        assert!(!c.to_lowercase().contains("dpms") && !c.contains("suspend") && !c.contains("hibernate"), "{c}");
    }
    assert!(!calls.iter().any(|c| c.ends_with(" -x")), "a refused SSID never reaches nmcli: {calls:?}");
    assert!(!calls.iter().any(|c| c.contains("fake-pass")), "the password is never an argument: {calls:?}");
    for expected in [
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 69%",
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 59%",
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 50%",
        "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle",
        "wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle",
        "wpctl set-default 99",
        "wpctl set-profile 61 3",
        "brightnessctl --class=backlight --device=intel_backlight set 55%",
        "brightnessctl --class=backlight --device=intel_backlight set 45%",
        "brightnessctl --class=backlight --device=intel_backlight set 50%",
        "powerprofilesctl set performance",
        "fake-keep-awake",
        "fake-night --toggle",
        "fake-logout",
        "loginctl lock-session",
        "systemctl reboot",
        "systemctl poweroff",
        "nmcli radio wifi off",
        "nmcli radio wifi on",
        "nmcli device wifi rescan",
        "nmcli connection down uuid 11111111-2222-4333-8444-555555555555",
        "nmcli connection up uuid 33333333-4444-4555-8666-777777777777",
        "nmcli device wifi connect Cafe-Libre",
        "nmcli --ask device wifi connect Bureau-Test",
        "nmcli connection delete uuid 22222222-3333-4444-8555-666666666666",
        "nmcli connection up uuid 00000000-0000-4000-8000-000000000001",
        "nmcli connection down uuid 00000000-0000-4000-8000-000000000001",
        "fake-more",
        "fake-vpn-more",
        "fake-mixer",
        "rfkill unblock bluetooth",
    ] {
        assert!(calls.iter().any(|c| c == expected), "{expected} never reached: {calls:?}");
    }
}

/// Quick spec § 6: no panic in the panel's production code.
#[test]
fn the_quick_panel_has_no_panicking_path() {
    let files = source_files();
    for name in [
        "quick/mod.rs",
        "quick/draw.rs",
        "quick/state.rs",
        "quick/app.rs",
        "quick/wifi.rs",
        "quick/secret.rs",
        "quick/bt.rs",
        "quick/vpn.rs",
        "quick/dnd.rs",
        "quick/shield.rs",
        "session.rs",
        "quick/bt_app.rs",
    ] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Quick Bluetooth spec § 5: the acting connection goes with the panel and is
/// let go without a call when BlueZ goes.
#[test]
fn the_bluetooth_connection_goes_with_the_panel() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (quick_app, bt_app) = (src("quick/app.rs"), src("quick/bt_app.rs"));
    let closed = function_body(&quick_app, "pub(crate) fn quick_closed(");
    assert!(closed.contains("BtScope::Closed") && closed.contains("self.bt_close()"), "{closed}");
    let flush = function_body(&quick_app, "pub(crate) fn quick_flush(");
    let (leaving, run) = (flush.find("bt_leaving()"), flush.find("self.bt_run("));
    assert!(matches!((leaving, run), (Some(l), Some(r)) if l < r), "the leaving orders sent before the body goes: {flush}");
    for f in ["pub(crate) fn quick_refresh(", "pub(crate) fn quick_toggle(", "pub(crate) fn quick_take("] {
        assert!(function_body(&quick_app, f).contains("self.quick_bt_sync()"), "{f}: the tap and the connection follow the panel");
    }
    let take = function_body(&quick_app, "pub(crate) fn quick_take(");
    assert!(take.contains("taken_bt(") && take.contains("snapshot()") && take.contains("open_current("), "{take}");
    let (dropped, panel) = (take.find("self.bt_drop()"), take.find("panel_bt("));
    assert!(matches!((dropped, panel), (Some(d), Some(p)) if d < p), "BlueZ gone: dropped before the panel's orders: {take}");
    // The headset is read before the sink list, which marks a panel-scope message.
    let (headset, sinks) = (take.find("taken_headset("), take.find("taken_sinks("));
    assert!(matches!((headset, sinks), (Some(h), Some(s)) if h < s), "the headset before the sinks: {take}");
    let tick = function_body(&quick_app, "pub(crate) fn quick_tick(");
    assert!(tick.contains("bt_logs") && tick.contains("bt_orders") && tick.contains("bt_run("), "{tick}");
    let close = function_body(&bt_app, "pub(crate) fn bt_close(");
    assert!(close.contains(".remove(") && close.contains(".close()"), "{close}");
    let drop = function_body(&bt_app, "fn bt_let_go(");
    assert!(function_body(&bt_app, "pub(crate) fn bt_drop(").contains("self.bt_let_go()"), "bt_drop is bt_let_go's action taken");
    assert!(drop.contains(".remove(") && !drop.contains(".close()"), "no UnregisterAgent to a gone owner: {drop}");
    // Spec § 6: a call in flight ends without `failed` (its answer can no longer come).
    assert!(drop.contains("ActError::Gone"), "the panel's action ends silently: {drop}");
    // A connection lost under us is reopened by the syncs once per open, not forever.
    let readable = function_body(&bt_app, "pub(crate) fn bt_readable(");
    assert!(readable.contains("self.bt_note_lost()") && readable.contains("self.bt_let_go()"), "{readable}");
    // A `Gone` on a live connection (a stale owner) lets it go, counted as a loss.
    let stale = readable.find("stale_owner(").expect("bt_readable reads a stale owner");
    assert!(readable[stale..].find("self.bt_let_go()").is_some(), "{readable}");
    // No action taken inside a batch (it may run orders that read again, nested), and
    // nothing of a batch handled once its connection went.
    let batch = function_body(&readable[readable.find("for event in events").expect("the batch")..], "for event in events");
    for nested in ["self.quick_action(", "self.bt_answer(", "self.bt_drop()", "self.bt_run(", "self.bt_readable()"] {
        assert!(!batch.contains(nested), "{nested} inside the batch: {batch}");
    }
    assert!(
        batch.contains("if self.quick.bt_act.is_none() {\n                    break;"),
        "the rest of a gone connection's batch: {batch}"
    );
    let after = &readable[readable.find(batch).expect("the batch") + batch.len()..];
    assert!(after.contains("self.quick_action(action)"), "the batch's actions taken after it: {after}");
    let run = function_body(&bt_app, "pub(crate) fn bt_run(");
    assert!(run.contains("opens_for(") && run.contains(".flush()"), "{run}");
    // libdbus's flush also reads: what it read is delivered by the next `read`, the fd may stay quiet.
    let (flushed, read) = (run.rfind(".flush()"), run.rfind("self.bt_readable()"));
    assert!(matches!((flushed, read), (Some(f), Some(r)) if f < r), "read after the flush: {run}");
}

/// Only quick/bt_app.rs drives the acting connection; quick/app.rs only declares its field.
#[test]
fn only_bt_app_and_bt_act_name_btact() {
    let mut naming = Vec::new();
    for (p, t) in source_files() {
        let (name, prod) = (relative_path(&p), production_code(&t));
        if prod.contains("BtAct") {
            naming.push(name.clone());
        }
        if prod.contains(".bt_act") {
            assert_eq!(name, "quick/bt_app.rs", "the acting connection used outside quick/bt_app.rs");
        }
    }
    naming.sort();
    assert_eq!(naming, ["quick/app.rs", "quick/bt_app.rs", "sources/dbus/bt_act.rs"]);
}

/// Quick Bluetooth spec § 2: a log line never names a device (BlueZ paths embed the address), and
/// the glue formats nothing with Debug but an `io::ErrorKind` (an `ActCall`, an `AgentRequest` or a
/// `BtEvent` carries an object path).
#[test]
fn the_bluetooth_logs_name_no_device() {
    let files = source_files();
    let mut lines = 0;
    for name in ["quick/bt.rs", "quick/bt_app.rs", "sources/dbus/bt_act.rs", "sources/dbus/bt.rs"] {
        let prod = production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
        for line in prod.lines().filter(|l| l.contains("log(") || l.contains("\"quick: ")) {
            lines += 1;
            for word in ["path", "device", "label", "alias", "name", "address"] {
                let braced = format!("{{{word}"); // {path}, {path:?}
                let token = line.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|t| t == word);
                assert!(!line.contains(&braced) && !token, "{name}: a log line may name a device (`{word}`): {line}");
            }
        }
    }
    assert!(lines >= 3, "the log lines were not found: the check is hollow");
    let bt_app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "quick/bt_app.rs").expect("quick/bt_app.rs").1);
    for line in bt_app.lines().filter(|l| l.contains(":?") || l.contains(":#?")) {
        assert!(line.contains(".kind()"), "bt_app.rs: Debug of anything but an io::ErrorKind: {line}");
    }
}

/// Quick Wi-Fi spec § 3: the password goes to nmcli's standard input with a newline, never as an
/// argument, and its field closes (the buffer wiped) right after; nothing of the panel formats a key
/// or the secret with Debug; the buffer's only unsafe is its volatile wipe.
#[test]
fn the_wifi_password_goes_to_stdin_only_and_is_wiped_after() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let quick_app = src("quick/app.rs");
    let body = function_body(&quick_app, "fn quick_spawn_secret(");
    let (input, sent) = (body.find("spawn_with_input("), body.find("password_sent()"));
    assert!(matches!((input, sent), (Some(i), Some(s)) if i < s), "handed on stdin, then the field closed: {body}");
    assert!(body.contains("b\"\\n\""), "followed by a newline: {body}");
    assert!(!body.contains(".spawn("), "never an argument: {body}");
    for name in ["quick/mod.rs", "quick/wifi.rs", "quick/secret.rs", "quick/app.rs", "quick/draw.rs", "popup.rs"] {
        let prod = src(name);
        for leak in
            ["{k:?}", "{key:?}", "secret:?", "password:?", "{field:?}", "{k:#?}", "{key:#?}", "secret:#?", "password:#?", "{field:#?}"]
        {
            assert!(!prod.contains(leak), "{leak} in {name}");
        }
        // Positional forms: a `{:?}` (or `{:#?}`) whose arguments, on the rest of its line, name a key,
        // the secret, the password or the field.
        for form in ["{:?}", "{:#?}"] {
            for (at, _) in prod.match_indices(form) {
                let rest = prod[at..].lines().next().unwrap_or("");
                for word in ["key", "k)", "k,", "secret", "password", "field"] {
                    assert!(!rest.contains(word), "{word} formatted with Debug in {name}: {rest}");
                }
            }
        }
    }
    let secret = src("quick/secret.rs");
    assert_eq!(secret.matches("unsafe").count(), 1, "one unsafe block: the volatile write");
    assert!(secret.contains("write_volatile") && secret.contains("compiler_fence"));
    assert!(!secret.contains("derive(Debug") && !secret.contains("derive(Clone"), "its own Debug (no byte) and Clone");
}

/// Quick Wi-Fi spec § 6 and quick VPN spec: no panic in the panel readers (the D-Bus thread's):
/// bounded, cleaned, never unwrapped.
#[test]
fn the_panel_readers_have_no_panicking_path() {
    let files = source_files();
    for name in ["sources/dbus/wifi.rs", "sources/dbus/vpn.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Quick spec § 5: whatever closes the panel (Escape, an outside click, a restyle, the output gone,
/// a replacing popup), the sound thread goes back to its own scope and the panel's timer goes; a
/// do-not-disturb change from anywhere repaints its row. No fake Wayland popup can be closed every
/// way in a unit test: the wiring is checked in the source.
#[test]
fn the_quick_panel_lets_go_of_the_sound_and_its_timer_whatever_closes_it() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (popup, quick_app, notify_app) = (src("popup.rs"), src("quick/app.rs"), src("notify/app.rs"));
    let close = function_body(&popup, "pub fn popup_close(");
    // A slider level still waiting for its window is sent before the panel's body goes:
    // the row's `flush` runs before the take, its `closed` once the body is dropped.
    let (flush, take) = (close.find("(p.ops().flush)(self)"), close.find("self.popup.take()"));
    assert!(matches!((flush, take), (Some(f), Some(t)) if f < t), "popup_close flushes the sliders first: {close}");
    let (dropped, closed) = (close.find("drop(open)"), close.find("(p.ops().closed)(self)"));
    assert!(matches!((dropped, closed), (Some(d), Some(c)) if d < c), "popup_close must let the quick panel go: {close}");
    let row = popup_ops_row(&quick_app);
    assert!(row.contains("flush: App::quick_flush,") && row.contains("closed: App::quick_closed,"), "the quick panel's row: {row}");
    // The media popup's and the panel's card's Seeked rule and 1 s tick go when either closes.
    for name in ["media/app.rs", "notify/panel_app.rs"] {
        let text = src(name);
        let row = popup_ops_row(&text);
        assert!(row.contains("closed: App::media_closed,"), "{name}: the tick must go with the popup: {row}");
    }
    let media_closed = function_body(&src("media/app.rs"), "pub(crate) fn media_closed(").to_string();
    assert!(media_closed.contains("popup_closed(out)") && media_closed.contains("self.media_timer_sync()"), "{media_closed}");
    let tick = function_body(&quick_app, "pub(crate) fn quick_tick(");
    assert!(tick.contains("due.run") && tick.contains("quick_spawn("), "the tick sends the levels that waited: {tick}");
    let closed = function_body(&quick_app, "pub(crate) fn quick_closed(");
    // Decision "A Wi-Fi attempt on a new network outlives the quick panel" (docs/decisions.md): the timer goes with the panel, unless an attempt on a new network is still
    // followed (then it stays on that attempt's deadline only).
    assert!(
        closed.contains(".close()")
            && closed.contains("quick.timer.sync_opt(")
            && closed.contains("self.quick.wifi_follow.as_ref().map(WifiFollow::deadline)"),
        "{closed}"
    );
    // Quick Wi-Fi spec § 5 and the decision "A Wi-Fi attempt on a new network outlives the quick panel" (docs/decisions.md): the D-Bus thread stops reading the Wi-Fi, unless an
    // attempt on a new network is still followed.
    assert!(closed.contains("wifi_closed(") && closed.contains("self.quick.wifi_follow"), "the Wi-Fi tap follows the close: {closed}");
    let wifi_closed = function_body(&quick_app, "pub(crate) fn wifi_closed(");
    assert!(wifi_closed.contains("WifiScope::Closed"), "closed: no Wi-Fi read any more: {wifi_closed}");
    // Quick Wi-Fi spec § 5: the D-Bus thread follows what the open panel shows, and nothing once closed.
    for f in ["pub(crate) fn quick_refresh(", "pub(crate) fn quick_toggle("] {
        let body = function_body(&quick_app, f);
        assert!(body.contains("self.quick_wifi_sync()"), "{f}: the tap follows the panel: {body}");
    }
    let take = function_body(&quick_app, "pub(crate) fn quick_take(");
    assert!(take.contains("taken_wifi(") && take.contains("panel_wifi("), "the Wi-Fi state goes to the panel, never kept: {take}");
    // Only a state read for the tap's current open reaches the panel or the follower.
    let panel = function_body(&quick_app, "pub(crate) fn panel_wifi(");
    assert!(panel.contains("open_current(") && panel.contains("follow_wifi("), "{panel}");
    assert!(take.contains("open_current(") && take.contains("snapshot()"), "{take}");
    // Decision "A Wi-Fi attempt on a new network outlives the quick panel" (docs/decisions.md): with no panel, the state and the clock go to the follower, which ends with
    // its attempt (the tap and the timer let go then).
    assert!(take.contains("quick.wifi_follow") && take.contains(".set_state(") && take.contains("self.quick_follow("), "{take}");
    assert!(tick.contains("self.quick.wifi_follow") || tick.contains("app.quick.wifi_follow"), "the follower's timeout: {tick}");
    // A panel reopened while an attempt is followed takes it back: no second attempt.
    let toggle = function_body(&quick_app, "pub(crate) fn quick_toggle(");
    let resume = function_body(&quick_app, "fn quick_resume(");
    assert!(
        toggle.contains("self.quick_resume()") && resume.contains("resumed(") && resume.contains("self.quick_action("),
        "{toggle} {resume}"
    );
    let follow = function_body(&quick_app, "fn quick_follow(");
    assert!(follow.contains("self.quick_wifi_sync()") && follow.contains("self.quick_timer_sync()"), "{follow}");
    let dnd = function_body(&notify_app, "pub(crate) fn dnd_apply(");
    assert!(dnd.contains("self.quick_refresh()"), "the panel's DND row follows the bell, the verb and the panel: {dnd}");
}

/// Power spec § 3, § 5: whatever keys, hovers and presses reach the menu — every pair of keys at
/// once, then a third past CONFIRM_AFTER; every row pressed twice, at once and past it — it only ever
/// runs a `[quick]` session entry whose command is set, each one is reached, and reboot or power
/// off never run without a second activation CONFIRM_AFTER after the first.
#[test]
fn every_power_menu_activation_is_a_configured_session() {
    let keys = [Key::Up, Key::Down, Key::Tab, Key::BackTab, Key::Enter, Key::Char(' '), Key::Escape, Key::Home, Key::Left, Key::Char('x')];
    let t = Instant::now();
    let later = t + session::CONFIRM_AFTER;
    for mask in 0..16u8 {
        let set = |bit: u8, argv: &str| if mask & bit != 0 { vec![argv.to_string()] } else { Vec::new() };
        let config = QuickConfig {
            lock: set(1, "fake-lock"),
            logout: set(2, "fake-logout"),
            reboot: set(4, "fake-reboot"),
            poweroff: set(8, "fake-poweroff"),
            ..QuickConfig::default()
        };
        let Some(fresh) = Power::new(&config) else {
            assert_eq!(mask, 0, "a command set: a menu");
            continue;
        };
        let mut ran: Vec<Session> = Vec::new();
        // `before`: the question pending just before a late activation; reboot and power off run only
        // when it was theirs.
        let mut record = |a: PowerAction, late: bool, before: Option<(Session, Instant)>| {
            if let PowerAction::RunAndClose(s) = a {
                assert!(!s.command(&config).is_empty(), "{s:?} has no command (mask {mask:04b})");
                assert!(
                    !s.asks() || (late && matches!(before, Some((q, _)) if q == s)),
                    "{s:?} ran without a question of its own and a second activation 0.4 s later (mask {mask:04b})"
                );
                ran.push(s);
            }
        };
        for k1 in keys {
            for k2 in keys {
                let mut p = fresh.clone();
                record(p.key(k1, false, t), false, None);
                record(p.key(k2, false, t), false, None);
                for k3 in keys {
                    let mut q = p.clone();
                    let before = q.question;
                    record(q.key(k3, false, later), true, before);
                }
            }
        }
        for row in 0..6u16 {
            let mut p = fresh.clone();
            record(p.press(row, t), false, None);
            record(p.press(row, t), false, None);
            let before = p.question;
            record(p.press(row, later), true, before);
            let mut p = fresh.clone();
            record(p.hover(row), false, None);
            record(p.key(Key::Enter, false, t), false, None);
            let before = p.question;
            record(p.key(Key::Enter, false, later), true, before);
        }
        for s in session::configured(&config) {
            assert!(ran.contains(&s), "{s:?} never reached (mask {mask:04b})");
        }
        for s in &ran {
            let line = s.command(&config).join(" ");
            assert!(!line.contains("suspend") && !line.contains("hibernate") && !line.to_lowercase().contains("dpms"), "{line}");
        }
    }
}

/// Power spec § 5: no panic in the power menu's production code.
#[test]
fn the_power_menu_has_no_panicking_path() {
    let files = source_files();
    for name in ["power/mod.rs", "power/app.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// The focus timer's wiring: the deadline's callback calls `fired()` before the clock check; the
/// clock's tick re-checks the wall-clock end (a suspended laptop's deadline is late); the state is
/// loaded whenever `timer` is in `modules`, not only with `notifications`; the model changes in
/// `timer_do`, `timer_check_clock` and `timer_load` only.
#[test]
fn the_timer_is_wired_through_one_path() {
    let src = |f: &str| without_comments(&std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(f)).unwrap());
    let app = src("timer/app.rs");
    let tick = function_body(&app, "fn timer_tick(");
    let (fired, check) = (tick.find(".fired()"), tick.find("timer_check_clock"));
    assert!(fired.is_some() && check.is_some() && fired < check, "fired() before the check: {tick}");
    let startup = src("startup.rs");
    let at = startup.find("Msg::Clock(h)").expect("the clock closure");
    let end = at + startup[at..].find("TimeoutAction::ToDuration").expect("the clock closure's end");
    assert!(startup[at..end].contains("timer_check_clock()"), "the clock tick must re-check the timer");
    let modules = function_body(&startup, "fn start_modules(");
    let notifications = function_body(modules, "if want.notifications");
    assert!(modules.contains("state.timer_load("), "the timer is loaded at start");
    assert!(!notifications.contains("timer_load"), "not only with the notifications module: {notifications}");
    // Every file under src/: the model is assigned in timer/app.rs's three owners and nowhere else
    // (`self.timer.model =`, `app.timer.model =`, `state.timer.model =` alike).
    for (path, text) in source_files() {
        let name = relative_path(&path);
        let prod = without_comments(&production_code(&text));
        let mut owner = String::new();
        for line in prod.lines() {
            if let Some(i) = line.find("fn ") {
                owner = line[i + 3..].split('(').next().unwrap_or_default().to_string();
            }
            // `x.timer.model =`, a destructured `timer.model =` (not part of a longer name) and
            // `mem::replace(&mut …timer.model`.
            let assigns = line.match_indices("timer.model =").any(|(i, _)| {
                let before = line[..i].chars().next_back();
                !line[i..].starts_with("timer.model ==") && before.is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
            }) || (["replace(&mut", "swap(&mut"].iter().any(|f| line.contains(f)) && line.contains("timer.model"));
            if assigns {
                assert!(
                    name == "timer/app.rs" && ["timer_do", "timer_check_clock", "timer_load"].contains(&owner.as_str()),
                    "the model changed in {name}'s {owner}"
                );
            }
        }
    }
    // A due end is applied before a change (the pure `change_step`): a click after a resume must
    // not pause or extend a timer that has already ended; timer_do never applies a change itself.
    let body = function_body(&app, "pub(crate) fn timer_do(");
    assert!(body.contains("change_step("), "timer_do goes through change_step: {body}");
    assert!(!body.contains(".apply("), "timer_do applies no change but through change_step: {body}");
}

/// Timer spec § 8: no panic in the focus timer's production code (it parses a file it did not
/// write, and does arithmetic on wall-clock times).
#[test]
fn the_timer_has_no_panicking_path() {
    let files = source_files();
    for name in ["timer/mod.rs", "timer/app.rs", "timer/popup.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Small features 1: the calculator's evaluator runs on each keystroke of the launcher's
/// query and must never panic, whatever is typed. Production code only.
#[test]
fn the_calculator_has_no_panicking_path() {
    let files = source_files();
    for name in ["launcher/calc.rs", "launcher/calc/units.rs", "hooks/mod.rs", "hooks/app.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Launcher 2: the prefix parser runs on each keystroke and must never panic, and the query text
/// it sees is never logged. Production code only.
#[test]
fn the_launcher_prefixes_have_no_panicking_path_and_log_nothing() {
    let files = source_files();
    for name in ["launcher/prefix.rs", "launcher/emoji.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
        for p in ["log(", "log::write", "crate::log"] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Quick notes: the pad's pure files run on each key and must never panic, slice a string by range
/// (a boundary panic's message embeds the text) nor log (the note is private). Production code only.
const NOTES_PURE: [&str; 6] = ["notes/text.rs", "notes/lines.rs", "notes/keys.rs", "notes/model.rs", "notes/view.rs", "notes/store.rs"];

/// What in `line` indexes by `[...]` in a way that can panic: a `..` between a `[` and its matching
/// `]` (a range slice, `s[a..b]`, `s[a..=b]`), or a bare index (`x[i]`: a `[` right after an
/// identifier character, `)`, `]`, `?`, `}` or a closing quote; attributes follow `#`, macros `!`,
/// array types a space).
fn panicking_index(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut open: Vec<usize> = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => {
                let prev = i.checked_sub(1).and_then(|p| bytes.get(p)).copied().unwrap_or(b' ');
                if prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b')' | b']' | b'?' | b'}' | b'"') {
                    return Some(format!("bare index in `{}`", line.trim()));
                }
                open.push(i);
            }
            b']' => {
                if let Some(start) = open.pop() {
                    if line.get(start..i).is_some_and(|inner| inner.contains("..")) {
                        return Some(format!("range inside brackets in `{}`", line.trim()));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn the_index_ban_sees_ranges_and_bare_indexes() {
    for bad in ["let x = &s[a..b];", "s[a..=b]", "let t = v[i];", "f(x)[0]", "f()?[0]", "m[i][j]", "&s[..n]", "&s[n..]", "w[..]"] {
        assert!(panicking_index(bad).is_some(), "{bad}");
    }
    for good in
        ["let a: [u8; 4] = [0; 4];", "#[derive(Debug)]", "let v = vec![1, 2];", "s.get(a..b)", "for i in 0..n {}", "fn f(x: &[u8]) {}"]
    {
        assert!(panicking_index(good).is_none(), "{good}");
    }
}

#[test]
fn the_notes_pure_parts_have_no_panicking_path_and_log_nothing() {
    let files = source_files();
    for name in NOTES_PURE {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!(", "log(", "log::write", "crate::log"] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
        for line in production.lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(why) = panicking_index(code) {
                panic!("{why} in {name}'s production code");
            }
        }
        // The `String` methods that panic off a scalar boundary (M2). One exemption: `Pad::splice`'s
        // `replace_range`, after it checked both ends with `is_char_boundary`.
        for p in [".replace_range(", ".split_at(", ".truncate(", ".split_off("] {
            let allowed = if name == "notes/text.rs" && p == ".replace_range(" { checked_replace_range(&production) } else { 0 };
            assert_eq!(production.matches(p).count(), allowed, "{p} in {name}'s production code");
        }
    }
}

/// The Tools menu's pure parts (tools spec § 9): no panicking path (a name, a command or a choice
/// may sit in a message), no log (their notes go through a callback the glue logs), no slicing by
/// range. Production code only.
const TOOLS_PURE: &[&str] = &["launcher/tools/config.rs", "launcher/tools/catalog.rs", "launcher/tools/nav.rs"];

#[test]
fn the_tools_pure_parts_have_no_panicking_path_and_log_nothing() {
    let files = source_files();
    for name in TOOLS_PURE.iter().copied() {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!(", "log(", "log::write", "crate::log"] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
        for line in production.lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(why) = panicking_index(code) {
                panic!("{why} in {name}'s production code");
            }
        }
    }
}

/// The Tools menu reaches the parallel deliveries (clipboard, shield, diagnostics) through
/// `launcher/tools/join.rs` alone (tools spec § 8): no other production file of the menu names their items.
/// A plain word search for the shield would hit the emoji table and the VPN model, so the
/// searched words are the hook points' own names; `"diagnostics"` quoted is the tab's slug (the
/// table's `tab.diagnostics` id is another literal).
#[test]
fn the_parallel_branches_are_named_only_in_tools_join() {
    let mut seen = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !name.starts_with("launcher/tools/") || name == "launcher/tools/join.rs" {
            continue;
        }
        seen += 1;
        let production = production_code(&t);
        for word in ["clipboard_availability", "launcher_open_clipboard", "shield_open_detail", "ShieldConfig", "\"diagnostics\""] {
            assert!(!production.contains(word), "{word} in {name}: name the parallel branches in launcher/tools/join.rs only");
        }
    }
    assert!(seen >= 3, "the tools files were not found");
}

/// The shield and diagnostics hook points are filled in (they merged): the shield is available
/// exactly when `[shield]` is configured and the `quick` module is on (what `shield_open_detail`
/// needs), opens through it, and the tab's slug is the one the control center accepts.
#[test]
fn the_join_hooks_are_filled_in() {
    let files = source_files();
    let join = &files.iter().find(|(p, _)| relative_path(p) == "launcher/tools/join.rs").expect("launcher/tools/join.rs").1;
    let available = function_body(join, "pub fn shield_available");
    assert!(available.contains("shield.is_some()") && available.contains("quick_enabled()"), "shield_available: {available}");
    assert!(function_body(join, "pub fn open_shield").contains("shield_open_detail()"), "open_shield goes through the Shield feature");
    assert!(join.contains("pub const DIAGNOSTICS_TAB: Option<&str> = Some(\"diagnostics\");"), "DIAGNOSTICS_TAB");
    let tab = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../alpheus/src/app/tab.rs")).expect("the TUI's tab.rs");
    assert!(tab.contains("slug: \"diagnostics\""), "the control center has no tab with the slug alpheus-launch passes");
}

/// The clipboard history's pure files (and `control.rs`, which never logs either) run on each copy and must never panic (a boundary panic's
/// message embeds the text), slice a string by range nor log (a copy is private). Production code
/// only; Tasks 2 and 3 append theirs.
const CLIPBOARD_NO_PANIC_NO_LOG: &[&str] = &["clipboard/history.rs", "clipboard/offer.rs", "clipboard/control.rs"];

#[test]
fn clipboard_no_panic_no_log_files_have_no_panicking_path_and_log_nothing() {
    let files = source_files();
    for name in CLIPBOARD_NO_PANIC_NO_LOG.iter().copied() {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!(", "log(", "log::write", "crate::log"] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
        for line in production.lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(why) = panicking_index(code) {
                panic!("{why} in {name}'s production code");
            }
            for p in [".replace_range(", ".split_at(", ".truncate(", ".split_off("] {
                assert!(!code.contains(p) || code.contains("is_char_boundary"), "{p} in {name}'s production code");
            }
        }
    }
}

/// `clipboard/app.rs` logs, with fixed words only: no panicking path, and every `log(` call is a
/// plain literal or interpolates only the entry point's name or an error kind.
const CLIPBOARD_FIXED_LOG_PLACEHOLDERS: [&str; 2] = ["{what}", "{kind:?}"];

/// The first `log(` call in `code` that is not fixed words, if any.
fn clipboard_unfixed_log(code: &str) -> Option<String> {
    let mut rest = code;
    while let Some(i) = rest.find("log(") {
        let call = &rest[i..];
        // To the parenthesis that closes `log(`.
        let mut depth = 0usize;
        let mut end = call.len();
        for (j, c) in call.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let mut text = call[..end].to_string();
        for ok in CLIPBOARD_FIXED_LOG_PLACEHOLDERS {
            text = text.replace(ok, "");
        }
        // The argument is a string literal or `&format!("…")` of one: never a variable.
        let arg = call["log(".len()..end.saturating_sub(1)].trim();
        let arg = arg.strip_prefix("&format!(").and_then(|a| a.strip_suffix(')')).unwrap_or(arg).trim();
        let literal = arg.starts_with('"') && arg.ends_with('"') && arg.len() >= 2;
        if !literal || text.contains('{') || text.contains("+ ") {
            return Some(call[..end].to_string());
        }
        rest = &call[end..];
    }
    None
}

/// A print or debug macro in `code` (clipboard text must reach no stream but the log's words).
fn clipboard_print_macro(code: &str) -> Option<&'static str> {
    ["eprintln!", "println!", "eprint!", "print!", "dbg!"]
        .into_iter()
        .find(|m| code.match_indices(m).any(|(i, _)| !code[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')))
}

#[test]
fn clipboard_app_never_panics_and_logs_fixed_words() {
    let files = source_files();
    let name = "clipboard/app.rs";
    let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
    let production = production_code(t);
    for p in [".expect(", ".unwrap()", "panic!("] {
        assert!(!production.contains(p), "{p} in {name}'s production code");
    }
    for line in production.lines() {
        let code = line.split("//").next().unwrap_or("");
        if let Some(why) = panicking_index(code) {
            panic!("{why} in {name}'s production code");
        }
    }
    let code: String = production.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    assert_eq!(clipboard_unfixed_log(&code), None, "a log call in {name} names more than fixed words");
}

#[test]
fn nothing_under_clipboard_prints() {
    let mut seen = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !name.starts_with("clipboard/") {
            continue;
        }
        seen += 1;
        let production = production_code(&t);
        for line in production.lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(m) = clipboard_print_macro(code) {
                panic!("{m} in {name}'s production code");
            }
        }
    }
    assert!(seen >= 5, "the clipboard files were found: {seen}");
}

#[test]
fn the_clipboard_guardrail_helpers_bite() {
    assert_eq!(clipboard_unfixed_log("log(\"clipboard: a fixed word\");"), None);
    assert_eq!(clipboard_unfixed_log("log(&format!(\"clipboard error in {what}\"));"), None);
    assert_eq!(clipboard_unfixed_log("log(&format!(\"read failed ({kind:?})\"));"), None);
    assert!(clipboard_unfixed_log("log(&format!(\"copied {text}\"));").is_some());
    assert!(clipboard_unfixed_log("log(&text);").is_some(), "a variable is not fixed words");
    assert!(clipboard_unfixed_log("log(text);").is_some());
    assert!(clipboard_unfixed_log("log(\"a\"); log(&format!(\"{text:?}\"));").is_some());
    for m in ["eprintln!(\"x\")", "println!(\"x\")", "print!(\"x\")", "eprint!(\"x\")", "dbg!(x)"] {
        assert!(clipboard_print_macro(m).is_some(), "{m}");
    }
    assert_eq!(clipboard_print_macro("fn pprintln!() {}"), None, "a longer identifier is not the macro");
}

/// The hint types that mark a password-manager copy are named in `clipboard/offer.rs`'s `SENSITIVE`
/// and nowhere else: no code path can request such an offer's data.
#[test]
fn the_clipboard_sensitive_hint_type_is_never_received() {
    let hints = ["x-kde-passwordManagerHint", "application/x-nspasteboard-concealed-type", "ExcludeClipboardContentFromMonitorProcessing"];
    let mut seen = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let production = production_code(&t);
        if name == "clipboard/offer.rs" {
            let mut rest = production.clone();
            let start = rest.find("pub const SENSITIVE").expect("SENSITIVE");
            let end = start + rest[start..].find("];").expect("end of SENSITIVE") + 2;
            rest.replace_range(start..end, "");
            for h in hints {
                assert!(!rest.contains(h), "{h} outside SENSITIVE in offer.rs");
                assert!(production.contains(h), "{h} missing from SENSITIVE");
            }
            seen += 1;
        } else {
            for h in hints {
                assert!(!production.contains(h), "{h} in {name}");
            }
        }
    }
    assert_eq!(seen, 1);
}

/// 1 when `text.rs`'s only `replace_range` is the one in `fn splice`, after both ends are checked
/// with `is_char_boundary`; else 0 (so the count check fails).
fn checked_replace_range(production: &str) -> usize {
    let splice = function_body(production, "fn splice(");
    let call = splice.find(".replace_range(");
    let checks = splice.find("is_char_boundary(range.start)").zip(splice.find("is_char_boundary(range.end)"));
    match (call, checks) {
        (Some(call), Some((a, b))) if a < call && b < call => 1,
        _ => 0,
    }
}

/// Quick notes: the notes file is written only through `state_file` (atomic, private, never a
/// followed link), and only `store.rs` calls its writers.
#[test]
fn notes_files_are_written_only_through_state_file() {
    let mut seen = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !name.starts_with("notes/") {
            continue;
        }
        seen += 1;
        let production = production_code(&t);
        for never in ["File::create", "OpenOptions", "fs::write", "fs::rename"] {
            assert!(!production.contains(never), "{never} in {name}");
        }
        if name != "notes/store.rs" {
            for only_store in ["write_private", "remove_private"] {
                assert!(!production.contains(only_store), "{only_store} in {name}");
            }
        }
    }
    assert!(seen >= 7, "the notes files were found: {seen}");
}

/// Quick settings delivery 3: Bluetooth's D-Bus files run on the D-Bus
/// thread and the bar's loop; neither may panic (constraints: no panicking path).
#[test]
fn the_bluetooth_d_bus_files_have_no_panicking_path() {
    let files = source_files();
    for name in ["sources/dbus/bt.rs", "sources/dbus/bt_act.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Power spec § 3, § 4: whatever closes the menu (Escape, an entry run, an outside click, a restyle,
/// the output gone, a replacing popup), its timer goes; an entry runs its own `[quick]` command
/// through the executor's `spawn`, and nothing else. No fake Wayland popup can be closed every way
/// in a unit test: the wiring is checked in the source.
#[test]
fn the_power_menu_lets_go_of_its_timer_and_runs_only_its_entry() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (popup, power_app) = (src("popup.rs"), src("power/app.rs"));
    let close = function_body(&popup, "pub fn popup_close(");
    assert!(close.contains("(p.ops().closed)(self)"), "popup_close must let a module popup's hooks run: {close}");
    let row = popup_ops_row(&power_app);
    assert!(row.contains("closed: App::power_closed,"), "popup_close must let the power menu's timer go: {row}");
    let closed = function_body(&power_app, "pub(crate) fn power_closed(");
    assert!(closed.contains("power_timer.sync_opt(") && closed.contains("None"), "{closed}");
    let tick = function_body(&power_app, "pub(crate) fn power_tick(");
    let (fired, due) = (tick.find(".fired()"), tick.find(".due("));
    assert!(matches!((fired, due), (Some(f), Some(d)) if f < d), "the dropped source forgotten first: {tick}");
    let refresh = function_body(&power_app, "fn power_refresh(");
    assert!(refresh.contains("self.power_timer_sync()"), "the timer follows every repaint: {refresh}");
    let spawn = function_body(&power_app, "fn power_spawn(");
    assert!(spawn.contains("s.command(&self.bar.config.quick)") && spawn.contains("spawn_session("), "{spawn}");
    let session = function_body(&power_app, "pub(crate) fn spawn_session(");
    assert_eq!(session.matches(".spawn(").count(), 1, "{session}");
    assert!(!session.contains("spawn_with_input") && !session.contains("actions::run"), "{session}");
    assert_eq!(power_app.matches("spawn_session(").count(), 2, "defined once, called once: {power_app}");
    // A held Enter acts once (power spec § 2): the menu's key hook hands the keyboard's own repeat
    // flag to the model, never a constant.
    assert!(row.contains("key: App::power_key,"), "{row}");
    let key = function_body(&power_app, "fn power_key(");
    assert!(key.contains("self.keyboard.repeat") && key.contains("p.key(k, repeat, now)"), "the repeat reaches the model: {key}");
}

/// Timer spec § 3, § 8: whatever closes the timer popup, the one-second cadence goes (`timer_sync`
/// sees the popup gone) and an end the popup showed is seen. No fake Wayland popup can be closed
/// every way in a unit test: the wiring is checked in the source.
#[test]
fn the_timer_popup_closing_drops_its_tick_and_sees_the_end() {
    let files = source_files();
    let src = |name: &str| {
        without_comments(&production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1))
    };
    let app = src("timer/app.rs");
    let row = popup_ops_row(&app);
    assert!(row.contains("closed: App::timer_closed,"), "popup_close must run the timer's closed hook: {row}");
    let closed = function_body(&app, "fn timer_closed(");
    let (seen, sync) = (closed.find("seen_at_close("), closed.find("self.timer_sync()"));
    assert!(closed.contains("self.timer_do("), "an end the popup showed is seen: {closed}");
    assert!(matches!((seen, sync), (Some(a), Some(b)) if a < b), "seen first, then the deadline without the seconds: {closed}");
}

/// Quick spec § 3: only the arrows repeat in the panel, so SCTK's own repeat must be told from a
/// press: both repeat paths raise `keyboard.repeat` around `key_event`.
#[test]
fn the_keyboards_own_repeat_is_told_from_a_press() {
    let files = source_files();
    let popup = production_code(&files.iter().find(|(p, _)| relative_path(p) == "popup.rs").expect("popup.rs").1);
    for f in ["pub(crate) fn keyboard_acquire(", "fn repeat_key("] {
        let body = function_body(&popup, f);
        let (raised, lowered) = (body.find("keyboard.repeat = true"), body.find("keyboard.repeat = false"));
        let event = body.find("key_event(");
        assert!(matches!((raised, event, lowered), (Some(r), Some(e), Some(l)) if r < e && e < l), "{f}: {body}");
    }
}

/// Privacy spec § 8: no panic in the privacy module's production code, nor in the sound model's
/// privacy functions.
#[test]
fn the_privacy_module_has_no_panicking_path() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let pipewire = src("pipewire/graph.rs");
    let mut texts = vec![src("privacy/mod.rs"), src("privacy/app.rs")];
    for f in ["fn flag(", "fn graph_node(", "pub fn apply_privacy(", "fn track(", "fn bound(", "fn bound_line(", "fn held("] {
        texts.push(function_body(&pipewire, f).to_string());
    }
    for (i, text) in texts.iter().enumerate() {
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!text.contains(p), "{p} in privacy code #{i}");
        }
    }
}

/// Small features 1, decision 7: every change of what the privacy graph shows ends in
/// `privacy_changed`, which hands the screen's edge to do-not-disturb after the timer sync
/// (`privacy_tick` and `privacy_feed` both reach it).
#[test]
fn privacy_changed_follows_sharing_for_dnd() {
    let files = source_files();
    let app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "privacy/app.rs").expect("privacy/app.rs").1);
    let changed = function_body(&app, "pub(crate) fn privacy_changed(");
    let (sync, follow) = (changed.find("self.privacy_timer_sync()"), changed.find("self.dnd_sharing_follow()"));
    assert!(matches!((sync, follow), (Some(s), Some(f)) if s < f), "dnd_sharing_follow after the timer sync: {changed}");
}

/// Privacy spec § 4: the off-delay's timer is the bar's, not the popup's. It follows
/// `Privacy::deadline` after every change, popup open or not (so `popup_close` never lets it go),
/// and the tick forgets its dropped source before applying what is due.
#[test]
fn the_privacy_timer_is_bar_level_and_fired_before_due() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (app, popup) = (src("privacy/app.rs"), src("popup.rs"));
    let tick = function_body(&app, "pub(crate) fn privacy_tick(");
    let (fired, due) = (tick.find(".fired()"), tick.find(".due("));
    assert!(matches!((fired, due), (Some(f), Some(d)) if f < d), "the dropped source forgotten first: {tick}");
    let changed = function_body(&app, "pub(crate) fn privacy_changed(");
    assert!(changed.contains("self.privacy_timer_sync()"), "the timer follows every change: {changed}");
    assert!(changed.contains("self.dirty = true"), "the bar's redraw flag, drawn after the pass: {changed}");
    let sync = function_body(&app, "pub(crate) fn privacy_timer_sync(");
    assert!(sync.contains("self.bar.privacy.deadline()") && sync.contains("privacy_timer.sync_opt("), "{sync}");
    assert!(!function_body(&popup, "pub fn popup_close(").contains("privacy_timer"), "the popup never owns it");
    let row = popup_ops_row(&app);
    assert!(row.contains("flush: no_hook,") && row.contains("closed: no_hook,"), "closing the popup never touches it: {row}");
    // Each message: a change follows `privacy_changed`; anything else still re-syncs the timer, since
    // an indicator going off changes nothing shown yet, only the deadline (without it, the glyph
    // stays lit until the next message).
    let feed = function_body(&app, "pub(crate) fn privacy_feed(");
    let (at, changed, other, synced) = (
        feed.find("self.bar.privacy_at(m, Instant::now())"),
        feed.find("self.privacy_changed()"),
        feed.find("else"),
        feed.find("self.privacy_timer_sync()"),
    );
    assert!(
        matches!((at, changed, other, synced), (Some(a), Some(c), Some(e), Some(s)) if a < c && c < e && e < s),
        "privacy_at, privacy_changed, else privacy_timer_sync: {feed}"
    );
    // The mail callback feeds it before the bar keeps the message (as the OSD reads sound first).
    let startup = src("startup.rs");
    let (fed, kept) = (startup.find("self.privacy_feed(&m)"), startup.find("on_message(self, m,"));
    assert!(matches!((fed, kept), (Some(f), Some(k)) if f < k), "privacy_feed, then on_message");
}

/// Resources popup spec § 6: a tick applies the reading before it repaints, and repaints only
/// while the popup is the open one (`App` cannot be built without Wayland: a source guardrail, as
/// for the privacy feed). The timer in `startup.rs` goes through it.
#[test]
fn a_resources_tick_repaints_only_an_open_popup_after_applying_the_reading() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let app = src("resources/app.rs");
    let tick = function_body(&app, "pub(crate) fn resources_tick(");
    let (applied, shown, painted) = (tick.find("on_message("), tick.find("self.resources_shown()"), tick.find("self.resources_refresh()"));
    assert!(
        matches!((applied, shown, painted), (Some(a), Some(s), Some(p)) if a < s && s < p),
        "on_message, then the open check, then the repaint: {tick}"
    );
    let refresh = function_body(&app, "fn resources_refresh(");
    assert!(refresh.contains("popup_paint("), "{refresh}");
    let startup = src("startup.rs");
    assert!(startup.contains("state.resources_tick(reader.sample())"), "the timer goes through resources_tick");
    assert!(!startup.contains("Msg::Resources(reader.sample())"), "no bypass of the repaint");
}

fn camera_sources() -> Vec<(&'static str, String)> {
    let files = source_files();
    ["privacy/camera.rs", "privacy/camera_app.rs"]
        .into_iter()
        .map(|name| (name, production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1)))
        .collect()
}

/// Privacy delivery 2: no panic in the camera watch's production code.
#[test]
fn the_camera_watch_has_no_panicking_path() {
    for (name, production) in camera_sources() {
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Privacy delivery 2: nothing under /dev is ever opened or read; the one `read_to_string` is a
/// process's `comm`.
#[test]
fn the_camera_watch_opens_nothing_under_dev() {
    let sources = camera_sources();
    let dev = &sources[0];
    assert_eq!(dev.0, "privacy/camera.rs");
    for never in ["File::open", "OpenOptions", "fs::read("] {
        assert!(!dev.1.contains(never), "{never} in privacy/camera.rs's production code");
    }
    let mut reads = 0;
    for line in dev.1.lines().filter(|l| l.contains("read_to_string")) {
        reads += 1;
        assert!(line.contains("comm"), "read_to_string outside the comm path: {line}");
    }
    assert!(reads > 0, "the comm read was not found: the check is hollow");
}

/// Privacy delivery 2: the debounce's timer is forgotten first, and a scan that
/// changes nothing still syncs the off-delay's timer (a pending off arms it).
#[test]
fn the_camera_tick_fires_before_scanning() {
    let sources = camera_sources();
    let app = &sources[1].1;
    let tick = function_body(app, "pub(crate) fn camera_tick(");
    let (fired, scan) = (tick.find(".fired()"), tick.find("camera_scan"));
    assert!(matches!((fired, scan), (Some(f), Some(s)) if f < s), "the dropped source forgotten first: {tick}");
}

#[test]
fn the_camera_scan_syncs_the_off_delay() {
    let sources = camera_sources();
    let scan = function_body(&sources[1].1, "fn camera_scan(");
    assert!(scan.contains("privacy_timer_sync()"), "an unchanged scan must still sync the off-delay: {scan}");
}

/// Privacy delivery 2: a log line never names a process or a pid.
#[test]
fn the_camera_logs_name_no_process() {
    let mut logs = 0;
    for (name, production) in camera_sources() {
        for line in production.lines().filter(|l| l.contains("log(")) {
            logs += 1;
            assert!(!line.contains("process::id"), "{name}: a log line carries a pid: {line}");
            for word in ["comm", "name", "names", "pid"] {
                let braced = format!("{{{word}}}");
                let tokens = line.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|t| t == word);
                assert!(!line.contains(&braced) && !tokens, "{name}: a log line names `{word}`: {line}");
            }
        }
    }
    assert!(logs >= 3, "the log lines were not found: the check is hollow");
}

/// The panel's Bluetooth row draws as the bar's module, from the same glyphs
/// (`segments::BLUETOOTH`, `segments::BLUETOOTH_CONNECTED`), never a copy of them.
#[test]
fn the_bluetooth_row_reuses_the_modules_glyphs() {
    let files = source_files();
    let quick_bt = production_code(&files.iter().find(|(p, _)| relative_path(p) == "quick/bt.rs").expect("quick/bt.rs").1);
    for copy in ["\\u{f00af}", "\\u{f00b1}", "\u{f00af}", "\u{f00b1}"] {
        assert!(!quick_bt.contains(copy), "a copy of a module glyph in quick/bt.rs: {copy}");
    }
    assert!(quick_bt.contains("segments::{BLUETOOTH, BLUETOOTH_CONNECTED}"), "quick/bt.rs takes the module's glyphs");
}

/// Small features 1, task 9: the alerts' production code cannot panic, and `on_bar_message` feeds
/// them before the bar keeps the message.
#[test]
fn on_bar_message_feeds_the_battery_alerts() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let startup = src("startup.rs");
    let body = function_body(&startup, "pub(crate) fn on_bar_message(");
    let (fed, kept) = (body.find("self.battery_feed(&m)"), body.find("on_message("));
    assert!(matches!((fed, kept), (Some(f), Some(k)) if f < k), "battery_feed, then on_message: {body}");
    let feed = function_body(&src("notify/app.rs"), "pub(crate) fn battery_feed(").to_string();
    for text in [src("notify/battery_alert.rs"), feed] {
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!text.contains(p), "{p} in the battery alerts");
        }
    }
}

/// Small features 1, task 9: a server that just started serving re-offers the last battery
/// reading (an alert held back before the name was held must not be lost).
#[test]
fn a_new_server_gets_the_last_battery_reading() {
    let files = source_files();
    let app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "notify/app.rs").expect("notify/app.rs").1);
    let view = function_body(&app, "pub(crate) fn notify_view(");
    assert!(view.contains("view.serving") && view.contains("self.battery_deliver(None)"), "{view}");
}

/// Hooks spec § 3: `spawn_with_env(` starts a user's command with the event's variables; only the
/// hooks' glue calls it (once), beside the executor's own wrapper. Production code only.
#[test]
fn spawn_with_env_only_runs_hooks() {
    let mut seen = Vec::new();
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let n = production_code(&t).matches("spawn_with_env(").count();
        if n > 0 {
            seen.push((name, n));
        }
    }
    seen.sort();
    // nonblocking.rs declares and delegates it (twice); hooks/app.rs calls it once.
    assert_eq!(seen.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["hooks/app.rs", "nonblocking.rs"]);
    assert_eq!(seen[0].1, 1, "hooks/app.rs starts a hook in one place");
}

/// The hooks see a sound or compositor message before the privacy indicators
/// it clears reach `hooks_capture`, so a source going down fires nothing.
#[test]
fn hooks_read_a_message_before_the_privacy_feed() {
    let files = source_files();
    let t = production_code(&files.iter().find(|(p, _)| relative_path(p) == "startup.rs").expect("startup.rs").1);
    let body = function_body(&t, "pub(crate) fn on_bar_message(");
    let hooks = body.find("self.hooks_msg(&m)").expect("hooks_msg");
    let privacy = body.find("self.privacy_feed(&m)").expect("privacy_feed");
    assert!(hooks < privacy, "{body}");
}

/// Every `log(` call of `code`, from `log(` to its closing parenthesis (strings aware, so a `)` or
/// a `;` inside a format string ends nothing), however many lines it spans.
fn log_statements(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in code.match_indices("log(") {
        // Not `catalog(`, `dialog(`…: the call is a whole word.
        if code[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let (mut in_str, mut escaped, mut depth, mut end) = (false, false, 0usize, code.len());
        for (i, c) in code[at..].char_indices() {
            match (in_str, escaped, c) {
                (true, true, _) => escaped = false,
                (true, false, '\\') => escaped = true,
                (true, false, '"') => in_str = false,
                (false, _, '"') => in_str = true,
                (false, _, '(') => depth += 1,
                (false, _, ')') => {
                    depth -= 1;
                    if depth == 0 {
                        end = at + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(code[at..end].to_string());
    }
    out
}

#[test]
fn log_statements_span_lines_and_ignore_a_semicolon_in_a_string() {
    let code = "log(&format!(\n    \"hook: {key}; {}\",\n    argv\n));\nlet x = 1;\ncatalog(\"a\");\nlog(&line);";
    let found = log_statements(code);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].contains("argv") && found[0].ends_with("))"), "{found:?}");
    assert_eq!(found[1], "log(&line)");
}

/// Hooks spec § 5: a hook's log lines hold fixed words only, never a command, a variable or a name.
/// Whole statements, so a `format!` split over several lines is read as one.
#[test]
fn hook_logs_carry_fixed_words_only() {
    let files = source_files();
    let t = &files.iter().find(|(p, _)| relative_path(p) == "hooks/app.rs").expect("hooks/app.rs").1;
    let production = production_code(t);
    let statements = log_statements(&production);
    assert!(statements.len() >= 5, "the hook log statements were not found: {}", statements.len());
    for s in statements {
        assert!(
            s.contains("\"hook: {") || s.contains("\"hooks: {") || s.contains("log(&line)") || s == "log(\"hooks: quiet window over\")",
            "{s}"
        );
        for never in ["argv", "env", ".env", "DEVICE", "PROFILE", "name", "value"] {
            assert!(!s.contains(never), "{never} in a hook log statement: {s}");
        }
    }
}

/// Hooks spec § 3: `started` runs once the launcher's socket is watched, so a `started` hook can
/// call `alpheus-launch`.
#[test]
fn hooks_start_after_the_launcher_socket_watch() {
    let files = source_files();
    let t = &files.iter().find(|(p, _)| relative_path(p) == "startup.rs").expect("startup.rs").1;
    let body = function_body(t, "pub fn run(");
    let watch = body.find("watch_launcher_socket(").expect("the socket watch in run");
    let started = body.find("hooks_started(").expect("hooks_started in run");
    assert!(watch < started, "hooks_started() must come after the launcher socket watch");
}

/// Hooks spec § 2: each state change the hooks follow is handed to them where it happens, and each
/// public entry point is a no-op without hooks.
#[test]
fn the_hooks_are_fed_where_state_changes() {
    let files = source_files();
    let text = |name: &str| files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1.clone();
    for (file, signature, call) in [
        ("startup.rs", "fn on_bar_message(", "hooks_msg("),
        ("notify/app.rs", "fn dnd_apply(", "hooks_dnd("),
        ("appearance.rs", "fn publish_mode(", "hooks_mode("),
        ("privacy/app.rs", "fn privacy_changed(", "hooks_capture("),
        ("startup.rs", "pub fn run(", "hooks_started("),
        ("shield/app.rs", "fn shield_readable(", "hooks_shield("),
    ] {
        let t = text(file);
        assert!(function_body(&t, signature).contains(call), "{signature} in {file} must call {call}");
    }
    let app = production_code(&text("hooks/app.rs"));
    let mut checked = 0;
    for (at, _) in app.match_indices("pub(crate) fn hooks_") {
        let body = function_body(&app[at..], "pub(crate) fn hooks_");
        let first = body.trim_start_matches('{').trim_start();
        assert!(first.starts_with("let Some("), "a hooks_ entry point must start with `let Some(`: {first:.60}");
        checked += 1;
    }
    assert_eq!(checked, 7);
}

/// Quick notes: nothing of the pad outlives it. `notes_close` takes the pad out before it syncs the
/// timer (so it disarms) and releases the keyboard (wanted while the pad is `Some`); the save timer's
/// callback marks it fired before anything else.
#[test]
fn the_notes_timer_and_pipe_live_only_while_open() {
    let files = source_files();
    let app = &files.iter().find(|(p, _)| relative_path(p) == "notes/app.rs").expect("notes/app.rs").1;
    let prod = production_code(app);
    let close = function_body(&prod, "pub(crate) fn notes_close(");
    let take = close.find("self.notes.open.take()").expect("notes_close takes the pad out");
    for later in ["notes_timer_sync()", "keyboard_release()"] {
        let at = close.find(later).unwrap_or_else(|| panic!("notes_close calls {later}: {close}"));
        assert!(take < at, "notes_close must take the pad before {later}: {close}");
    }
    let fire = function_body(&prod, "fn notes_save_fire(");
    let first = fire.trim_start_matches('{').trim_start();
    assert!(first.starts_with("app.notes.timer.fired()"), "notes_save_fire marks the timer fired first: {fire}");
}

/// Each `log(` call in `text`, whole: from `log(` to the end of its statement (its matching
/// parenthesis, however many lines its arguments take).
fn log_calls(text: &str) -> Vec<&str> {
    let mut calls = Vec::new();
    for (at, _) in text.match_indices("log(") {
        let mut depth = 0u32;
        let mut end = text.len();
        for (j, c) in text[at + 3..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = at + 3 + j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        calls.push(&text[at..end]);
    }
    calls
}

#[test]
fn log_calls_take_the_whole_statement() {
    let text = "log(\"a\");\nlog(&format!(\n    \"b {}\",\n    pad.text().len()\n));\nother(x);";
    assert_eq!(log_calls(text), ["log(\"a\")", "log(&format!(\n    \"b {}\",\n    pad.text().len()\n))"]);
}

/// Quick notes: the pad's glue never logs the note: no `log(` line names its text, its selection
/// or its buffer.
#[test]
fn the_notes_glue_never_logs_the_text() {
    let files = source_files();
    for name in ["notes/surface.rs", "notes/app.rs", "notes/paste.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let prod = production_code(t);
        for call in log_calls(&prod) {
            // A close reason's log word (`CloseReason::text`) is the one `text` allowed.
            let call = call.replace("reason.text()", "reason");
            for word in ["text", "selected", "pad.text", "model.pad"] {
                assert!(!call.contains(word), "a log call in {name} names `{word}`: {call}");
            }
        }
    }
}

/// Quick notes: the pad runs one command, the copy program, with the selection on its standard
/// input; nothing else is started from `src/notes/`.
#[test]
fn the_notes_spawn_only_the_copy_command() {
    let mut seen = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !name.starts_with("notes/") {
            continue;
        }
        seen += 1;
        let prod = production_code(&t);
        for never in [".spawn(", ".capture(", ".stream(", ".run(", ".run_silent("] {
            assert!(!prod.contains(never), "{never} in {name}");
        }
        let n = prod.matches("spawn_with_input(").count();
        if name == "notes/app.rs" {
            assert_eq!(n, 1, "one spawn_with_input in notes/app.rs");
            let body = function_body(&prod, "fn copy_call(");
            assert!(body.contains("spawn_with_input("), "the one spawn is copy_call's: {body}");
        } else {
            assert_eq!(n, 0, "spawn_with_input in {name}");
        }
    }
    assert!(seen >= 8, "the notes files were found: {seen}");
}

/// Quick notes (P9): the pad's glue never panics on its own either: no unwrap, expect, panic or
/// panicking index (a boundary panic's message embeds the text, and the panic hook logs it).
#[test]
fn the_notes_glue_has_no_panicking_path() {
    let files = source_files();
    for name in ["notes/surface.rs", "notes/app.rs", "notes/paste.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
        for line in production.lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(why) = panicking_index(code) {
                panic!("{why} in {name}'s production code");
            }
        }
    }
}

/// Quick notes: a press on the bar while the pad is open closes
/// it, and the launcher button then opens the launcher — opening one closes the other. Only the
/// launcher's own close swallows its button (`click`'s `launcher_was_open`).
#[test]
fn a_bar_press_swallows_only_a_launcher_it_closed() {
    let (_, text) = source_files().into_iter().find(|(p, _)| relative_path(p) == "wayland.rs").expect("wayland.rs");
    let prod = production_code(&text);
    let body = function_body(&prod, "fn pointer_frame(");
    assert!(body.contains("self.click(qh, i, ev.position.0, b, launcher_was_open)"), "click gets the launcher's own state: {body}");
    assert!(!body.contains("launcher_was_open || notes_was_open"), "the pad's close never swallows the launcher button");
}

/// Quick notes, task 6: the paste pipe is a source of the loop only inside `notes/paste.rs`, and
/// goes with the pad; the pad never owns a selection nor starts a drag.
#[test]
fn the_paste_pipe_lives_only_while_the_pad_is_open() {
    let mut seen = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = production_code(&t);
        if name.starts_with("notes/") {
            seen += 1;
        }
        // The clipboard history reads its offers through the same SCTK pipe, in its one protocol file.
        if name != "notes/paste.rs" && name != "clipboard/control.rs" {
            assert!(
                !((prod.contains("insert_source(") || prod.contains(".receive(")) && prod.contains("ReadPipe")),
                "a ReadPipe source outside notes/paste.rs: {name}"
            );
        }
        for never in ["create_copy_paste_source", "set_selection", "create_drag_and_drop_source"] {
            let allowed = never == "set_selection" && name == "clipboard/control.rs";
            assert!(allowed || !prod.contains(never), "{never} in {name}: the pad never owns a selection");
        }
    }
    assert!(seen >= 9, "the notes files were found: {seen}");
    let files = source_files();
    let surface = &files.iter().find(|(p, _)| relative_path(p) == "notes/surface.rs").expect("notes/surface.rs").1;
    let prod = production_code(surface);
    let drop = function_body(&prod, "fn drop(&mut self)");
    assert!(drop.contains("stop_paste()"), "OpenNotes's Drop removes the paste source: {drop}");
    assert!(
        drop.contains("selection_offer()") && drop.contains("drag_offer()") && drop.matches(".destroy()").count() == 2,
        "OpenNotes's Drop destroys the selection and drag offers (SCTK's device drop does not): {drop}"
    );
    let stop = function_body(&prod, "fn stop_paste(");
    assert!(stop.contains("paste.take()") && stop.contains(".remove("), "stop_paste takes the token and removes the source: {stop}");
    let paste = &files.iter().find(|(p, _)| relative_path(p) == "notes/paste.rs").expect("notes/paste.rs").1;
    let paste = production_code(paste);
    assert_eq!(paste.matches("insert_source(").count(), 1, "one source, the pipe");
    let notes_paste = function_body(&paste, "fn notes_paste(");
    let (ro, rx) = (notes_paste.find("read_only"), notes_paste.find(".receive("));
    assert!(ro.is_some() && rx.is_some() && ro < rx, "a read-only pad returns before any pipe is opened: {notes_paste}");
}

/// Clipboard task 4: the daemon sets a selection in one place only, `own_selection` of
/// `clipboard/control.rs`, for a history entry's re-copy: never `None` (it would clear the user's
/// clipboard), never the primary selection.
#[test]
fn the_clipboard_selection_is_set_only_by_the_own_source() {
    let files = source_files();
    for (p, t) in &files {
        let name = relative_path(p);
        let prod = production_code(t);
        assert!(!prod.contains("set_primary_selection"), "the primary selection is never set: {name}");
        if name != "clipboard/control.rs" {
            assert!(!prod.contains("set_selection("), "set_selection outside clipboard/control.rs: {name}");
        }
    }
    let control = &files.iter().find(|(p, _)| relative_path(p) == "clipboard/control.rs").expect("control.rs").1;
    let prod = production_code(control);
    assert_eq!(prod.matches("set_selection(").count(), 2, "one call per protocol, in the macro-free own_selection: {prod}");
    assert!(!prod.contains("set_selection(None"), "the selection is never cleared");
    let body = function_body(&prod, "fn own_selection(");
    assert_eq!(body.matches("set_selection(").count(), 2, "both calls sit inside own_selection: {body}");
    assert_eq!(body.matches("own_offer_types()").count(), 2, "each source offers the one list, marker included: {body}");
    assert_eq!(body.matches("set_selection(Some(").count(), 2, "only ever a source: {body}");
}

/// An offer taken in `clipboard_selection_inner` sits in a `DestroyOnDrop` before anything can
/// panic: a panic between taking it and reading it still destroys it.
#[test]
fn the_clipboard_offer_is_wrapped_in_destroy_on_drop() {
    let files = source_files();
    let app = &files.iter().find(|(p, _)| relative_path(p) == "clipboard/app.rs").expect("clipboard/app.rs").1;
    let prod = production_code(app);
    let body = function_body(&prod, "fn clipboard_selection_inner(");
    let take = body.find("slot.take()").expect("the offer is taken out of its slot");
    let guard = body.find("DestroyOnDrop::new(offer)").expect("the offer is wrapped in DestroyOnDrop");
    assert!(take < guard, "wrapped right after it is taken: {body}");
    // Nothing that can panic or call out sits between the take and the guard.
    let between = &body[take..guard];
    assert!(!between.contains("self.") && !between.contains("unwrap") && !between.contains("expect"), "{between}");
}

/// This file and the harness, as `stray_clipboard_spawns` names them (relative to `crates/`).
const THIS_FILE: &str = "alpheus-daemon/tests/guardrails.rs";
const HARNESS: &str = "alpheus-daemon/tests/common/mod.rs";

/// The `wl-copy`/`wl-paste` spawns of the test files that are not the harness helper: a test that
/// starts one outside `tests/common`'s `wl_copy` could reach the machine's own compositor (and its
/// clipboard). Comments may name the programs.
fn stray_clipboard_spawns(files: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (name, text) in files {
        if name == THIS_FILE {
            continue;
        }
        let mut code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        if name == HARNESS {
            for helper in ["pub fn wl_copy(", "pub fn wl_paste("] {
                let body = function_body(&code, helper).to_string();
                code = code.replace(&body, "");
            }
        }
        for program in ["wl-copy", "wl-paste"] {
            if code.contains(program) {
                found.push(format!("{name} names {program}"));
            }
        }
    }
    found
}

#[test]
fn stray_clipboard_spawns_are_found() {
    let helper = "pub fn wl_copy(s: &Sway) -> C {\n    Command::new(\"wl-copy\")\n}\npub fn wl_paste(s: &Sway) -> C {\n    Command::new(\"wl-paste\")\n}\n";
    let files = |a: &str, b: &str| {
        vec![(HARNESS.to_string(), helper.to_string()), ("a.rs".to_string(), a.to_string()), ("b.rs".to_string(), b.to_string())]
    };
    assert!(stray_clipboard_spawns(&files("// wl-copy serves it\nfn t() {}\n", "fn t() {}\n")).is_empty());
    assert_eq!(
        stray_clipboard_spawns(&files("fn t() { Command::new(\"wl-copy\"); }\n", "fn t() { Command::new(\"wl-paste\"); }\n")),
        ["a.rs names wl-copy", "b.rs names wl-paste"]
    );
    let outside = vec![(HARNESS.to_string(), format!("{helper}fn other() {{ Command::new(\"wl-copy\"); }}\n"))];
    assert_eq!(stray_clipboard_spawns(&outside), [format!("{HARNESS} names wl-copy")]);
    // A second harness function naming wl-paste is still found next to the two helpers.
    let second = vec![(HARNESS.to_string(), format!("{helper}fn other() {{ Command::new(\"wl-paste\"); }}\n"))];
    assert_eq!(stray_clipboard_spawns(&second), [format!("{HARNESS} names wl-paste")]);
    // Another crate's harness or guardrails get no exemption.
    let elsewhere = vec![
        ("alpheus/tests/common/mod.rs".to_string(), helper.to_string()),
        ("alpheus/tests/guardrails.rs".to_string(), "fn t() { Command::new(\"wl-paste\"); }\n".to_string()),
    ];
    assert_eq!(
        stray_clipboard_spawns(&elsewhere),
        [
            "alpheus/tests/common/mod.rs names wl-copy",
            "alpheus/tests/common/mod.rs names wl-paste",
            "alpheus/tests/guardrails.rs names wl-paste"
        ]
    );
}

/// Quick notes (session safety): the one `wl-copy` a test starts is the harness helper, which checks
/// that this sway's socket exists, clears the environment, names this sway's runtime directory and
/// display, stays in the foreground and holds a guard that kills it; no other test file of any crate
/// names the program.
#[test]
fn no_test_spawns_wl_copy_outside_the_harness_helper() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/").to_path_buf();
    let mut files = Vec::new();
    let mut dirs: Vec<_> = std::fs::read_dir(&crates).unwrap().flatten().map(|e| e.path().join("tests")).filter(|p| p.is_dir()).collect();
    assert!(dirs.len() >= 3, "every crate's tests: {dirs:?}");
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                let name = path.strip_prefix(&crates).unwrap().to_string_lossy().into_owned();
                files.push((name, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    assert!(files.iter().any(|(n, _)| n == HARNESS), "the harness was found");
    assert!(files.iter().any(|(n, _)| n.starts_with("alpheus-launch/tests/")), "another crate's tests were walked");
    let stray = stray_clipboard_spawns(&files);
    assert!(stray.is_empty(), "{stray:?}");
    let common = &files.iter().find(|(n, _)| n == HARNESS).unwrap().1;
    let helper = function_body(common, "pub fn wl_copy(");
    let socket_check = helper.find("assert!(sway.runtime.join(&sway.wayland).exists()");
    let spawn = helper.find(".spawn()");
    assert!(
        socket_check.is_some() && spawn.is_some() && socket_check < spawn,
        "the wl-copy helper must assert this sway's socket exists before it spawns: {helper}"
    );
    for need in ["env_clear()", "\"XDG_RUNTIME_DIR\", &sway.runtime", "\"WAYLAND_DISPLAY\", &sway.wayland", "\"--foreground\""] {
        assert!(helper.contains(need), "the wl-copy helper lacks {need}: {helper}");
    }
    assert!(!helper.contains("--primary"), "never the primary selection");
    let paste = function_body(common, "pub fn wl_paste(");
    let socket_check = paste.find("assert!(sway.runtime.join(&sway.wayland).exists()");
    assert!(
        socket_check.is_some() && socket_check < paste.find(".spawn()"),
        "the wl-paste helper must assert this sway's socket exists before it spawns: {paste}"
    );
    for need in ["env_clear()", "\"XDG_RUNTIME_DIR\", &sway.runtime", "\"WAYLAND_DISPLAY\", &sway.wayland", "\"--no-newline\""] {
        assert!(paste.contains(need), "the wl-paste helper lacks {need}: {paste}");
    }
    assert!(!paste.contains("--primary"), "never the primary selection");
    let guard = function_body(common, "impl Drop for WlCopyChild");
    assert!(guard.contains(".kill()") && guard.contains(".wait()"), "the guard kills and reaps: {guard}");
}

/// Quick notes (M2): the daemon's panic hook (thread and place, never the message: it can quote
/// private text) is installed first in `startup::run`, before any thread is started.
#[test]
fn the_panic_hook_is_installed_before_anything_else() {
    let files = source_files();
    let startup = &files.iter().find(|(p, _)| relative_path(p) == "startup.rs").expect("startup.rs").1;
    let run = function_body(startup, "pub fn run() -> i32 {");
    let first_call = run.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//") && l != &"{");
    assert_eq!(first_call, Some("crate::log::install_panic_hook();"), "{run}");
    let log = &files.iter().find(|(p, _)| relative_path(p) == "log.rs").expect("log.rs").1;
    let hook = function_body(log, "pub fn install_panic_hook(");
    for leak in ["payload", "info.to_string", "{info}", "{info:", "message()"] {
        assert!(!hook.contains(leak), "the hook must never print the panic's message ({leak}): {hook}");
    }
}

/// The launcher's guard logs the place only (as the pad's): a panic message can quote the query, a
/// menu line or a password, which are never logged.
#[test]
fn the_launcher_guard_never_logs_a_panic_payload() {
    let files = source_files();
    let host = &files.iter().find(|(p, _)| relative_path(p) == "launcher/host.rs").expect("launcher/host.rs").1;
    let prod = production_code(host);
    let guard = function_body(&prod, "fn guarded_with<R>(");
    assert!(guard.contains("Err(_) =>"), "the payload is not even bound: {guard}");
    for leak in ["panic_text", "downcast", "Err(p)", "Err(payload)"] {
        assert!(!guard.contains(leak), "the launcher's guard must never read the panic's message ({leak}): {guard}");
    }
    assert!(!prod.contains("panic_text"), "launcher/host.rs formats no panic payload");
    // The scan threads read .desktop files and source lines: a payload there can quote them.
    let scan = &files.iter().find(|(p, _)| relative_path(p) == "launcher/scan.rs").expect("launcher/scan.rs").1;
    let scan = production_code(scan);
    for leak in ["panic_text", "downcast", "Err(p)", "Err(payload)", "|p|"] {
        assert!(!scan.contains(leak), "launcher/scan.rs must never read a panic's message ({leak})");
    }
}

/// Quick VPN spec § 2: a log line never names a profile (its name, label, uuid or object path): the
/// VPN's own files, and the VPN lines of the glue.
#[test]
fn the_vpn_logs_name_no_profile() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let mut statements = 0;
    for name in ["quick/vpn.rs", "sources/dbus/vpn.rs", "quick/app.rs"] {
        let prod = src(name);
        let own = name != "quick/app.rs";
        for statement in vpn_log_statements(&prod) {
            if !own && !statement.to_ascii_lowercase().contains("vpn") {
                continue;
            }
            statements += 1;
            for word in ["name", "label", "uuid", "path", "id"] {
                let braced = format!("{{{word}");
                let token = statement.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|t| t == word);
                assert!(!statement.contains(&braced) && !token, "{name}: a log line may name a profile (`{word}`): {statement}");
            }
        }
    }
    assert!(statements >= 3, "the log lines were not found: the check is hollow");
    // The scan itself: a log call split over several lines is one statement.
    let split = "fn f() {\n    log(&format!(\n        \"quick: vpn {name}\",\n    ));\n    other();\n}";
    let found = vpn_log_statements(split);
    assert!(!found.is_empty() && found.iter().all(|f| f.contains("{name}") && !f.contains("other")), "{found:?}");
}

/// Every `log(` call and every `"quick: ` string of `text`, each from its start to the next `;`
/// (a call may span several lines).
fn vpn_log_statements(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    for start in ["log(", "\"quick: "] {
        for (at, _) in text.match_indices(start) {
            let end = text[at..].find(';').map_or(text.len(), |e| at + e);
            found.push(&text[at..end]);
        }
    }
    found
}

/// Quick VPN spec § 5: the D-Bus thread reads the VPNs only while the panel is open and follows it,
/// and the VPN state goes to the panel alone.
#[test]
fn the_vpn_tap_follows_the_panel() {
    let files = source_files();
    let quick_app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "quick/app.rs").expect("quick/app.rs").1);
    let closed = function_body(&quick_app, "pub(crate) fn quick_closed(");
    assert!(closed.contains("VpnScope::Closed"), "{closed}");
    for f in ["pub(crate) fn quick_refresh(", "pub(crate) fn quick_toggle("] {
        assert!(function_body(&quick_app, f).contains("self.quick_vpn_sync()"), "{f}: the tap follows the panel");
    }
    let take = function_body(&quick_app, "pub(crate) fn quick_take(");
    assert!(take.contains("taken_vpn(") && take.contains("panel_vpn("), "{take}");
    assert!(take.contains("Msg::Vpn { .. }"), "a VPN message refreshes the panel it shows: {take}");
    let tick = function_body(&quick_app, "pub(crate) fn quick_tick(");
    assert!(tick.contains("vpn_failed"), "the attempt's timeout is logged: {tick}");
}

/// Clipboard task 3: every data-control token is named in `clipboard/control.rs` only.
#[test]
fn data_control_only_in_control_rs() {
    let mut seen = false;
    for (p, t) in all_source_files() {
        let name = relative_path(&p);
        for token in ["data_control", "DataControl", "zwlr_data_control", "ext_data_control"] {
            if t.contains(token) {
                assert_eq!(name, "clipboard/control.rs", "\"{token}\" outside the protocol file");
                seen = true;
            }
        }
    }
    assert!(seen, "the protocol file names the protocol");
}

/// Off (the default): no manager is bound. The `clipboard` field's value is computed under
/// `clipboard.enabled`.
#[test]
fn the_manager_is_bound_only_when_enabled() {
    let files = source_files();
    let wayland = &files.iter().find(|(p, _)| relative_path(p) == "wayland.rs").expect("wayland.rs").1;
    let prod = production_code(wayland);
    let at = prod.find("let clipboard =").expect("the clipboard binding in App::new");
    let stmt = &prod[at..at + prod[at..].find(';').expect("end of the statement")];
    let (gate, bind) = (stmt.find("clipboard.enabled"), stmt.find("ClipboardApp::bind("));
    assert!(gate.is_some() && bind.is_some() && gate < bind, "bound only under clipboard.enabled: {stmt}");
}

/// The read deadline is armed by every function that starts or ends a read, and disarmed with it.
#[test]
fn every_clipboard_read_start_and_end_syncs_the_timer() {
    let files = source_files();
    let body = |file: &str, sig: &str| {
        let t = &files.iter().find(|(p, _)| relative_path(p) == file).unwrap_or_else(|| panic!("{file}")).1;
        function_body(&production_code(t), sig).to_string()
    };
    for (file, sig) in [
        ("clipboard/control.rs", "fn start_read("),
        ("clipboard/app.rs", "fn clipboard_selection_inner("),
        ("clipboard/app.rs", "fn clipboard_read_done("),
        ("clipboard/app.rs", "fn clipboard_deadline("),
    ] {
        assert!(body(file, sig).contains("clipboard_timer_sync()"), "{sig} syncs the read deadline");
    }
    let deadline = body("clipboard/app.rs", "fn clipboard_deadline(");
    let (fired, stop) = (deadline.find(".fired()"), deadline.find("clipboard_stop_read()"));
    assert!(fired.is_some() && fired < stop, "the deadline drops its own timer first: {deadline}");
    assert!(body("clipboard/control.rs", "fn start_read(").contains("clipboard_stop_read()"), "a new read drops the pending one");
}

/// The clipboard's log lines are the fixed words of the plan, never a text, a type or a count.
#[test]
fn clipboard_logs_only_fixed_words() {
    const ALLOWED: [&str; 9] = [
        "clipboard: ext-data-control-v1",
        "clipboard: wlr-data-control-unstable-v1",
        "clipboard: the compositor offers no data-control protocol; no history",
        "clipboard: the data device finished",
        "clipboard: a read timed out",
        "clipboard: a read failed ({kind:?})",
        "clipboard: history cleared (row)",
        "clipboard error in {what}",
        "clipboard: history cleared (verb)",
    ];
    let mut calls = 0;
    for (p, t) in source_files() {
        if !relative_path(&p).starts_with("clipboard/") {
            continue;
        }
        let prod = production_code(&t);
        for token in ["log(", "log::write("] {
            let mut rest = prod.as_str();
            while let Some(at) = rest.find(token) {
                let after = &rest[at + token.len()..];
                // Only a literal (or a `format!` of one) is logged, and nothing follows it.
                let trimmed = after.trim_start();
                let body = trimmed.strip_prefix("&format!(").unwrap_or(trimmed).trim_start();
                assert!(body.starts_with('"'), "a log call that is not a literal: {}", &after[..after.len().min(40)]);
                let inner = &body[1..];
                let close = inner.find('"').expect("the literal ends");
                let template = &inner[..close];
                assert!(ALLOWED.contains(&template), "a log line outside the fixed words: {template}");
                for word in ["text", "mime", "buffer", "bytes", "len"] {
                    assert!(!template.contains(word), "{word} in the log line {template}");
                }
                let next = inner[close + 1..].trim_start().chars().next();
                assert!(next == Some(')'), "an argument after the template of {template}");
                calls += 1;
                rest = &inner[close..];
            }
        }
    }
    assert!(calls >= 6, "the log calls were found: {calls}");
}

/// A `selection(None)` leaves a read in flight alone (an app that copies then quits sends it right
/// after): the read is dropped only once a real offer is taken, never before.
#[test]
fn a_cleared_selection_does_not_drop_a_read() {
    let files = source_files();
    let t = &files.iter().find(|(p, _)| relative_path(p) == "clipboard/app.rs").expect("app.rs").1;
    let prod = production_code(t);
    let body = function_body(&prod, "fn clipboard_selection_inner(");
    let (take, stop) = (body.find("let Some(offer) = slot.take() else { return }"), body.find("clipboard_stop_read()"));
    assert!(take.is_some() && take < stop, "returns on None before any read is stopped: {body}");
}

/// Shutdown wipes the whole history, pins included (`clear` keeps the pinned ones).
#[test]
fn the_clipboard_shutdown_wipes_pins_too() {
    let files = source_files();
    let t = &files.iter().find(|(p, _)| relative_path(p) == "clipboard/app.rs").expect("app.rs").1;
    let prod = production_code(t);
    let body = function_body(&prod, "fn clipboard_shutdown(");
    assert!(body.contains("History::default()") && !body.contains(".clear()"), "{body}");
}

/// Tools spec § 9: every command a Tools row starts is one the user configured
/// (`[[tools.entry]]`: its `run`, `terminal` or `terminal_hold` in front), the chosen line one
/// whole element — from the Tools box and from Mod+Space's search alike, in a scope or detached —
/// never through a shell alpheus builds; and each shape is reached.
#[test]
fn every_tool_run_is_a_configured_argv_with_the_choice_whole() {
    use alpheus_daemon::launcher::tools::catalog::{Catalog as ToolsCatalog, Context};
    let text = r#"
[[tools.category]]
name = "Nix"
[[tools.entry]]
name = "Update"
category = "Nix"
run = ["atool", "update"]
[[tools.entry]]
name = "Held"
run = ["atool", "held"]
hold = true
[[tools.entry]]
name = "Termed"
run = ["atool", "termed"]
terminal = true
[[tools.entry]]
name = "Gen"
category = "Nix"
run = ["atool", "gen", "{choice}"]
choices = ["a b", "$(id); rm -rf ~", "it's", "{choice}", "-rf"]
[tools]
builtins = false
"#;
    let config = LauncherConfig::from_text(text).unwrap();
    let bin = std::env::temp_dir().join(format!("alpheus-guardrails-{}-tools-bin", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    let _guard = RemoveOnDrop(bin.clone());
    for p in ["atool", "foot"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(bin.join(p), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_dirs = [bin.clone()];
    let configured = |argv: &[String]| -> Option<&'static str> {
        let a: Vec<&str> = argv.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["atool", "update"] => Some("plain"),
            ["foot", "--hold", "-e", "atool", "held"] => Some("hold"),
            ["foot", "-e", "atool", "termed"] => Some("terminal"),
            ["atool", "gen", c] if ["a b", "$(id); rm -rf ~", "it's", "{choice}", "-rf"].contains(c) => Some("choice"),
            _ => None,
        }
    };
    let mut seen = Vec::new();
    for spawner in [Spawner::SystemdScope, Spawner::Detached] {
        let catalog = || ToolsCatalog::build(&config.tools, &Context::default(), &mut |_| {});
        // The Tools box: every row of every level, by click.
        let mut outcomes = Vec::new();
        let open = || Launcher::for_tools(config.clone(), catalog(), History::empty(200), 1_800_000_000);
        let rows_at = |keys: &[Key]| {
            let mut l = open();
            for k in keys {
                l.key(*k);
            }
            l.results.len()
        };
        // Root: categories and root entries; into "Nix" (after the root entries: Held, Nix, Termed).
        let nix = (0..rows_at(&[])).find(|&i| {
            let mut l = open();
            l.click(i, false);
            l.tools().is_some_and(|t| t.breadcrumb() == "Tools › Nix")
        });
        for i in 0..rows_at(&[]) {
            outcomes.push(open().click(i, false));
        }
        let nix = nix.expect("the Nix category is a root row");
        let in_nix = || {
            let mut l = open();
            l.click(nix, false);
            l
        };
        for i in 0..in_nix().results.len() {
            let mut l = in_nix();
            let o = l.click(i, false);
            outcomes.push(o);
            // Drilled into Gen: its choices.
            for j in 0..l.results.len() {
                let mut c = in_nix();
                c.click(i, false);
                outcomes.push(c.click(j, false));
            }
        }
        // Mod+Space's typed search reaches the same entries and the fixed choices.
        for q in ["update", "held", "termed", "gen", "gen a b", "gen -rf", "gen it", "gen {choice}", "gen $(id)"] {
            let mut l = Launcher::for_apps(config.clone(), vec![], History::empty(200), 1_800_000_000);
            l.set_tools(catalog());
            for c in q.chars() {
                l.key(Key::Char(c));
            }
            outcomes.push(l.key(Key::Enter));
            outcomes.push(l.key(Key::CtrlEnter));
        }
        for o in outcomes {
            let Outcome::Activate(a) = o else { continue };
            let spy = ArgvSpy::default();
            let mut l = open();
            let _ = perform(&mut l, a, &RunContext { exec: &spy, spawner, history_path: None, path_dirs: &path_dirs });
            let calls = spy.0.lock().unwrap().clone();
            let argv = calls.first().expect("a tool ran nothing");
            let scoped: Vec<String> = SYSTEMD_RUN.iter().map(|s| s.to_string()).collect();
            let argv = if spawner == Spawner::SystemdScope { argv.strip_prefix(&scoped[..]).expect("not in its scope") } else { &argv[..] };
            let shape = configured(argv).unwrap_or_else(|| panic!("a tool started something nobody configured: {argv:?}"));
            assert!(!argv.iter().any(|a| a == "sh" || a == "-c"), "never through a shell alpheus builds: {argv:?}");
            seen.push((shape, spawner));
        }
    }
    for spawner in [Spawner::SystemdScope, Spawner::Detached] {
        for shape in ["plain", "hold", "terminal", "choice"] {
            assert!(seen.contains(&(shape, spawner)), "{shape} never reached under {spawner:?}: {seen:?}");
        }
    }
}

// ---- The Tools menu's glue. ----

/// The placeholders a Tools log line may interpolate: positions, counts, a built-in's static id and
/// a `Why`'s fixed words. Never a name, a command, a choice, a line or the query.
const TOOLS_LOG_PLACEHOLDERS: [&str; 8] = ["{i}", "{n}", "{k}", "{on}", "{off}", "{unknown}", "{id}", "{why}"];

/// The first `log(` or `note(` call in `code` that is not fixed words, if any. The relay
/// `|m| log(m)` (the notes of the pure code handed to the log) is the one variable allowed.
fn tools_unfixed_log(code: &str) -> Option<String> {
    let code = code.replace("|m| log(m)", "");
    for opener in ["log(", "note("] {
        let mut rest = code.as_str();
        while let Some(i) = rest.find(opener) {
            let before = rest[..i].chars().next_back();
            let call = &rest[i..];
            // A longer name (`catalog(`, `tools_note(`) is not the call.
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                rest = &call[opener.len()..];
                continue;
            }
            let mut depth = 0usize;
            let mut end = call.len();
            for (j, c) in call.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = j + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let mut text = call[..end].to_string();
            for ok in TOOLS_LOG_PLACEHOLDERS {
                text = text.replace(ok, "");
            }
            let arg = call[opener.len()..end.saturating_sub(1)].trim();
            let arg = arg.strip_prefix("&format!(").and_then(|a| a.strip_suffix(')')).unwrap_or(arg).trim();
            let literal = arg.starts_with('"') && arg.ends_with('"') && arg.len() >= 2;
            if !literal || text.contains('{') || text.contains("+ ") {
                return Some(call[..end].to_string());
            }
            rest = &call[end..];
        }
    }
    None
}

#[test]
fn the_tools_logs_are_fixed_words() {
    let files = source_files();
    let mut seen = 0;
    for name in ["launcher/tools/read.rs", "launcher/tools/app.rs", "launcher/tools/join.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let code: String = production_code(t).lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
        assert_eq!(tools_unfixed_log(&code), None, "a log call in {name} names more than fixed words");
        seen += code.matches("log(").count() + code.matches("note(").count();
    }
    let run = &files.iter().find(|(p, _)| relative_path(p) == "launcher/run.rs").expect("run.rs").1;
    let run_production = production_code(run);
    let perform_tool = function_body(&run_production, "fn perform_tool");
    assert_eq!(tools_unfixed_log(perform_tool), None, "a log call in perform_tool names more than fixed words");
    assert!(seen >= 8 && perform_tool.contains("log("), "the log calls were found: {seen}");
}

#[test]
fn the_tools_log_guardrail_helper_bites() {
    assert_eq!(tools_unfixed_log("log(\"tools: read failed\");"), None);
    assert_eq!(tools_unfixed_log("log(&format!(\"tools: built-in {id}\"));"), None);
    assert_eq!(tools_unfixed_log("note(&format!(\"tools: entry {i}: choices {why}, {k} kept\"));"), None);
    assert_eq!(tools_unfixed_log("let c = tools_catalog(&config); Catalog::build(a, b, &mut |m| log(m));"), None);
    assert!(tools_unfixed_log("log(&format!(\"tools: entry {name}\"));").is_some(), "a name");
    assert!(tools_unfixed_log("log(&format!(\"tools: entry {}\", e.name));").is_some(), "a bare placeholder");
    assert!(tools_unfixed_log("log(&text);").is_some(), "a variable");
    assert!(tools_unfixed_log("note(&line);").is_some(), "a variable note");
    assert!(tools_unfixed_log("log(\"a\"); log(&format!(\"{choice}\"));").is_some());
}

/// The question's timer goes with the box, however it goes: both the close and the panic path
/// re-sync it (it then reads no deadline).
#[test]
fn the_tools_timer_is_disarmed_by_close_and_by_a_panic() {
    let files = source_files();
    let text = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let surface = text("launcher/surface.rs");
    assert!(function_body(&surface, "pub fn launcher_close(").contains("tools_timer_sync()"), "launcher_close");
    assert!(function_body(&surface, "fn launcher_outcome(").contains("tools_timer_sync()"), "launcher_outcome");
    let host = text("launcher/host.rs");
    assert!(function_body(&host, "fn launcher_fail(").contains("tools_timer_sync()"), "launcher_fail");
}

/// A built-in spawns only through `run::launch` (its own scope, its program looked up first); the
/// glue never starts a process itself, through a shell or otherwise.
#[test]
fn the_tools_glue_spawns_only_through_launch() {
    let files = source_files();
    let t = &files.iter().find(|(p, _)| relative_path(p) == "launcher/tools/app.rs").expect("launcher/tools/app.rs").1;
    let code = production_code(t);
    for forbidden in [".spawn(", ".run(", ".capture(", "Command::new", "\"sh\"", "-c\""] {
        assert!(!code.contains(forbidden), "{forbidden} in launcher/tools/app.rs");
    }
    assert!(code.contains("run::launch("), "the one spawn");
    assert_eq!(code.matches("run::launch(").count(), 1);
}

/// Every built-in's argv is the configured command, `[actions] open_tab` with the tab, or the
/// terminal running `alpheus --tab <tab>`, nothing else, never through a shell.
#[test]
fn every_builtin_argv_stays_on_the_allowlist() {
    use alpheus_daemon::config::QuickConfig;
    use alpheus_daemon::launcher::tools::app::{Commands, builtin_argv};
    use alpheus_daemon::launcher::tools::catalog::{BUILTINS, BuiltinAction};
    use std::os::unix::fs::PermissionsExt;
    let bin = std::env::temp_dir().join(format!("alpheus-guardrails-{}-builtins", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    let _bin_guard = RemoveOnDrop(bin.clone());
    let programs = ["loginctl", "keepawake", "nightlight", "shell-ctl", "foot"];
    for p in programs {
        std::fs::write(bin.join(p), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let dirs = [bin.clone()];
    let mut quick = QuickConfig::default();
    quick.keep_awake.toggle = vec!["keepawake".into(), "--flip".into()];
    quick.night_light.toggle = vec!["nightlight".into()];
    let reload = vec!["shell-ctl".to_string(), "reload-bar".to_string()];
    let terminal = vec!["foot".to_string()];
    // `open_tab`'s program is missing: the terminal fallback is what runs.
    let open_tab = vec!["missing-opener".to_string(), "{tab}".to_string()];
    let c = Commands { quick: &quick, reload: &reload, open_tab: &open_tab, terminal: &terminal, path_dirs: &dirs };
    let mut spawned = 0;
    for b in &BUILTINS {
        let got = builtin_argv(b, &c).unwrap_or_else(|e| panic!("{}: {e}", b.id));
        let expected: Option<Vec<String>> = match b.action {
            BuiltinAction::Lock => Some(quick.lock.clone()),
            BuiltinAction::Toggle(t) => Some(t.config(&quick).toggle.clone()),
            BuiltinAction::Reload => Some(reload.clone()),
            BuiltinAction::Tab => {
                let slug = b.id.strip_prefix("tab.").expect("a tab id");
                Some(["foot", "alpheus", "--tab", slug].map(String::from).to_vec())
            }
            _ => None,
        };
        assert_eq!(got, expected, "{}", b.id);
        if let Some(argv) = got {
            assert!(!argv.iter().any(|a| a == "sh" || a == "-c"), "{}: {argv:?}", b.id);
            spawned += 1;
        }
    }
    assert!(spawned >= 5, "the spawning built-ins were reached: {spawned}");
}

/// Review I1, I2, M1, M5: what a Mod+Space open leaves alone, and what a closed menu stops.
#[test]
fn the_tools_reads_belong_to_the_tools_mode_and_stop_with_the_box() {
    let files = source_files();
    let text = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let host = text("launcher/host.rs");
    let apps = function_body(&host, "pub(crate) fn launcher_open_apps(");
    for word in ["tools_builtin_states", "tools_adopt_builtin_states", "tools_known_states"] {
        assert!(!apps.contains(word), "{word}: a plain Mod+Space open reads no built-in state (a /proc scan)");
    }
    assert!(apps.contains("tools_catalog(&config, false)"), "a Mod+Space open logs no catalog note");
    assert!(text("launcher/tools/join.rs").contains("clipboard_availability()"), "the clipboard row follows the protocol too");
    let tools = text("launcher/tools/app.rs");
    assert!(function_body(&tools, "pub(crate) fn launcher_open_tools(").contains("tools_catalog(&config, true)"));
    let surface = text("launcher/surface.rs");
    let outcome = function_body(&surface, "fn launcher_outcome_inner(");
    assert!(
        outcome.contains("Outcome::EnterTools(") && outcome.contains("tools_adopt_builtin_states()"),
        "the jump reads the built-ins' states"
    );
    assert!(function_body(&surface, "pub fn launcher_close(").contains("tools_reads_stop()"), "launcher_close");
    assert!(function_body(&host, "fn launcher_fail(").contains("tools_reads_stop()"), "launcher_fail");
    assert!(function_body(&tools, "pub(crate) fn run_builtin(").contains("is_refusal"), "a refused verb is logged");
    assert!(
        function_body(&tools, "fn tools_spawn(").contains("spawn_builtin(")
            && !function_body(&tools, "fn tools_spawn(").contains("Spawner::")
    );
    let tick = function_body(&tools, "pub(crate) fn tools_tick(");
    assert!(tick.contains("launcher_guarded(\"tools timer\""), "the timer's tick runs guarded");
}

/// The string literals of `call` (escapes skipped) and what is left of it once they are removed.
fn literals_and_rest(call: &str) -> (Vec<String>, String) {
    let (mut lits, mut rest, mut cur, mut inside, mut escaped) = (Vec::new(), String::new(), String::new(), false, false);
    for c in call.chars() {
        if inside {
            if escaped {
                cur.push(c);
                escaped = false;
            } else if c == '\\' {
                cur.push(c);
                escaped = true;
            } else if c == '"' {
                inside = false;
                lits.push(std::mem::take(&mut cur));
                rest.push('"');
            } else {
                cur.push(c);
            }
        } else {
            if c == '"' {
                inside = true;
            }
            rest.push(c);
        }
    }
    (lits, rest)
}

/// Shield spec § 11: a log line is one of the closed list's fixed words. No device name, id, `vid:pid`,
/// command, argument or path: what is left of each whole `log(` call once its literals are removed
/// names none of them, and every literal is one of the list's (a `<reason>` slot is filled by
/// `Reason::text` alone).
#[test]
fn the_shield_logs_name_nothing() {
    const CLOSED: [&str; 19] = [
        "shield: on",
        "shield: off",
        "shield: allow",
        "shield: now on",
        "shield: now off",
        "shield: switching {} failed ({})",
        "shield: allow failed ({})",
        "shield: listing failed ({})",
        "shield: more than 64 blocked devices; the rest not listed",
        "shield: the flag cannot be read",
        "shield: flag not watched ({})",
        "shield: flag not watched ({e})",
        "shield: flag not watched (shares the config directory's watch)",
        "shield: no notification (not serving)",
        "shield: worker failed",
        "shield: not started ({why})",
        "theme: mask not restored; watching again",
        "on",
        "off",
    ];
    const FORBIDDEN: [&str; 8] = ["name", "vid_pid", "id", "argv", "flag", "path", "config", "device"];
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    // Every log call of the Shield's own files, and the Shield's lines of the others.
    let mut calls: Vec<(String, String)> = Vec::new();
    for name in ["shield/app.rs", "shield/mod.rs", "shield/run.rs", "shield/usb.rs", "quick/shield.rs"] {
        calls.extend(log_calls(&src(name)).into_iter().map(|c| (name.to_string(), c.to_string())));
    }
    for name in
        ["quick/app.rs", "notify/app.rs", "launcher/socket_glue.rs", "launcher/host.rs", "launcher/surface.rs", "hooks/app.rs", "watch.rs"]
    {
        calls.extend(
            log_calls(&src(name)).into_iter().filter(|c| c.to_lowercase().contains("shield")).map(|c| (name.to_string(), c.to_string())),
        );
    }
    // The watch's one line built apart (`shield_unwatched_line`): its `format!` is read the same way.
    let watch = src("watch.rs");
    calls.push(("watch.rs".into(), function_body(&watch, "fn shield_unwatched_line(").to_string()));
    // The theme watch's own line for the flag's rearm.
    calls
        .extend(log_calls(&watch).into_iter().filter(|c| c.contains("mask not restored")).map(|c| ("watch.rs".to_string(), c.to_string())));
    calls.sort();
    calls.dedup();
    assert!(calls.len() >= 10, "the Shield's log calls were found: {}", calls.len());
    for (name, call) in &calls {
        let (lits, rest) = literals_and_rest(call);
        for l in &lits {
            assert!(CLOSED.contains(&l.as_str()), "a literal outside the closed list in {name}: {l:?} in {call}");
        }
        for word in rest.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            assert!(!FORBIDDEN.contains(&word), "a log call in {name} names `{word}`: {call}");
        }
    }
    // The `<reason>` slot of the failure lines is `Reason::text`.
    for call in calls.iter().map(|(_, c)| c).filter(|c| c.contains("failed ({})")) {
        assert!(call.contains("why.text()"), "{call}");
    }
}

/// Shield spec § 3 and decision 4: the flag is watched in the theme watch's one inotify instance; the
/// Shield's own files create none.
#[test]
fn the_shield_watch_is_the_theme_watchs_instance() {
    let files = source_files();
    for name in ["shield/app.rs", "shield/mod.rs", "shield/run.rs", "shield/usb.rs", "quick/shield.rs"] {
        let t = production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
        for never in ["Inotify::new", "inotify_", "inotify::"] {
            assert!(!t.contains(never), "{never} in {name}");
        }
    }
    let watch = production_code(&files.iter().find(|(p, _)| relative_path(p) == "watch.rs").expect("watch.rs").1);
    assert_eq!(watch.matches("Inotify::new()").count(), 1, "one inotify instance, the theme watch's, carries both targets");
}

/// Shield spec § 12 and the plan's constraints: the pure model, the parser, the runner and the panel
/// detail have no panicking path (a USB name is hostile text).
#[test]
fn the_shield_files_have_no_panicking_path() {
    let files = source_files();
    for name in ["shield/mod.rs", "shield/usb.rs", "shield/run.rs", "quick/shield.rs"] {
        let production = production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
        for p in [".expect(", ".unwrap()", "panic!(", "unreachable!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
        for line in production.lines() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(why) = panicking_index(code) {
                panic!("{why} in {name}'s production code");
            }
        }
    }
}

/// Shield spec § 7: an action (on, off, allow) never goes through a pipe it could
/// be cut by (`capture` drops its read end at a deadline: a privileged helper we cannot kill would die
/// of SIGPIPE half way through its work); only the listing does. And the daemon's shutdown stops what
/// it can still reach.
#[test]
fn a_shield_action_is_never_a_capture_and_shutdown_stops_the_commands() {
    let files = source_files();
    let t = production_code(&files.iter().find(|(p, _)| relative_path(p) == "shield/run.rs").expect("shield/run.rs").1);
    let body = function_body(&t, "pub fn run_job(");
    let list_at = body.find("exec.capture(").expect("the listing's capture");
    let silent_at = body.find("exec.run_silent(").expect("the actions' run_silent");
    assert!(silent_at < list_at, "the actions' branch comes first and returns before the listing's capture");
    assert_eq!(body.matches("exec.capture(").count(), 1);
    // A failed switch and a deadline are judged after one look at the flag.
    let app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "shield/app.rs").expect("shield/app.rs").1);
    let mail = function_body(&app, "fn shield_mail(");
    assert!(
        mail.find("self.shield_readable()").expect("a read in shield_mail") < mail.find("model.done(").expect("model.done"),
        "shield_mail reads the flag before it judges a failed switch"
    );
    let tick = function_body(&app, "fn shield_tick(");
    assert!(
        tick.find("shield_readable()").expect("a read in shield_tick") < tick.find("model.expired(").expect("expired"),
        "shield_tick reads the flag before it judges the deadline, watched or not"
    );
    assert!(!tick.contains("shield_watched"), "the deadline's read does not depend on the watch");
    let startup = &files.iter().find(|(p, _)| relative_path(p) == "startup.rs").expect("startup.rs").1;
    assert!(function_body(startup, "fn shut_down(").contains("exec.stop_running()"), "shut_down must stop the running commands");
}

/// Shield spec § 5: the panel's timer follows its view from the open, so a
/// `failed` shown at open (a switch that failed a moment before the panel was reopened) clears at its
/// 4 s: `quick_toggle` syncs it itself, whatever refresh happens to follow.
#[test]
fn the_quick_panels_timer_follows_the_view_from_the_open() {
    let files = source_files();
    let quick_app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "quick/app.rs").expect("quick/app.rs").1);
    for f in ["pub(crate) fn quick_toggle(", "pub(crate) fn quick_refresh("] {
        assert!(function_body(&quick_app, f).contains("self.quick_timer_sync()"), "{f} must sync the panel's timer");
    }
}
