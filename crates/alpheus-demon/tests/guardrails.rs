//! Guardrails from the framing document for the daemon, checked
//! mechanically: it only ever runs the workspace command (`wm-set
//! workspace …` by default), only ever reads the compositor (four fixed
//! Hyprland requests, the niri stream), only opens the compositor's
//! sockets in `src/sources/`, never spawns a process directly, and the
//! forbidden words appear nowhere in its sources.

use alpheus_commun::exec::fake::FakeExecutor;
use alpheus_commun::exec::{CaptureLimits, Captured, ExecError, Executor, Output, Stream};
use alpheus_demon::actions::{Button, command, run};
use alpheus_demon::appearance::SETTING_RULE;
use alpheus_demon::compositor::{CompositorState, Toplevel, Window, Workspace};
use alpheus_demon::config::{BarConfig, ModuleId, QuickConfig, ToggleConfig};
use alpheus_demon::keys::Key;
use alpheus_demon::launcher::config::{LauncherConfig, SourceConfig};
use alpheus_demon::launcher::desktop::{DesktopAction, DesktopEntry};
use alpheus_demon::launcher::history::History;
use alpheus_demon::launcher::matching::Mode;
use alpheus_demon::launcher::model::{Activation, Launcher, Outcome};
use alpheus_demon::launcher::run::{RunContext, SYSTEMD_RUN, Spawner, perform};
use alpheus_demon::launcher::scan::Catalog;
use alpheus_demon::launcher::sources::{Entries, SourceEntry};
use alpheus_demon::notify::NotifyView;
use alpheus_demon::power::{Power, PowerAction};
use alpheus_demon::privacy::{Capture, Indicator};
use alpheus_demon::quick::{self, Backlight, Focus, Live, Quick, QuickAction};
use alpheus_demon::session::{self, Session};
use alpheus_demon::session_bus::bus::{Call, DBUS_NAME_FLAG_DO_NOT_QUEUE, Method as TrayMethod, NOTIFICATIONS_NAME_FLAGS, message};
use alpheus_demon::session_bus::{
    APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, NOTIFICATIONS_NAME, PORTAL_NAME, PORTAL_PATH, SETTINGS_INTERFACE, WATCHER_NAME,
};
use alpheus_demon::sources::dbus::bt_act::{AGENT_PATH, ActMethod, CAPABILITY};
use alpheus_demon::sources::dbus::dbusrs_bus::{LOGIN1_RULE, MATCH_RULES, Method, WIFI_RULES};
use alpheus_demon::sources::{hypr::Request, mango, niri::REQUEST, sway};
use alpheus_demon::state::{Bar, Msg};
use alpheus_demon::system::*;
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

/// Small features 1, notification rules: the rules never name an app or a text in a log, and
/// notify.rs, the pure model, logs nothing.
#[test]
fn the_notification_rules_log_nothing() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/notify.rs")).unwrap();
    let prod = production_code(&text);
    for log in ["log(", "log::write", "crate::log"] {
        assert!(!prod.contains(log), "notify.rs logs nothing ({log})");
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
/// and sent only by `outputs_app.rs` (the controller's one-in-flight, last-screen and cap rules);
/// never output power management, never dpms (displays spec § 2, as amended).
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
            assert_eq!(name, "outputs_app.rs", "an output configuration sent outside the displays glue");
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

/// Final review A-I2: the daemon stopping sends a trial's state from before while the app (its
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

/// Mae, 2026-10-05: the launcher's click-outside scrims must keep working
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
}

/// OSD spec § 1 and the bubbles' (notifications spec § 3): the pill and the bubbles live on the
/// Overlay layer, the only one Hyprland draws above a fullscreen window (the launcher box's rule,
/// above). A headless sway cannot tell the layers apart (no fullscreen window there), so this
/// checks the one call that would regress it: each file creates exactly one layer surface, in its
/// create function, on `Layer::Overlay`.
#[test]
fn the_osd_and_the_bubbles_are_created_on_the_overlay_layer() {
    let files = source_files();
    for (name, create) in [("osd_surface.rs", "fn osd_create("), ("bubbles_surface.rs", "fn bubbles_create(")] {
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
/// direct `launcher_close` call. No fake Wayland seat or keyboard exists to
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
/// briefly shared by two bars — or the wrong bar (or none) gets reset, left stuck holding Mae's
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

/// Mae's crash (2026-10-06, 2026-10-09): a released `wl_keyboard`'s last reference, dropped inside
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
    let panel_app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "panel_app.rs").expect("panel_app.rs").1);
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
    let (link, bridge) = (src("link.rs"), src("tray_bridge.rs"));
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
    let (menu_app, popup, surface) = (src("menu_app.rs"), src("popup.rs"), src("launcher/surface.rs"));
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

/// Launcher sources (sources spec § 6): `Executor::capture` blocks for up to its deadline, so only
/// the source runner calls it, on the scan thread (`NonBlocking` delegates it); and the runner never
/// builds a shell command from a line — the line is one argv element.
#[test]
fn only_the_source_runner_captures_and_never_through_a_shell() {
    let mut runner = None;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = production_code(&t);
        if prod.contains(".capture(") {
            assert!(matches!(name.as_str(), "launcher/sources.rs" | "nonblocking.rs"), ".capture( in {name}: it blocks");
        }
        if name == "launcher/sources.rs" {
            runner = Some(prod);
        }
    }
    let runner = runner.expect("launcher/sources.rs");
    assert!(runner.contains(".capture("), "the runner reads through the executor");
    for never in ["shell_argv", "\"sh\"", "\"-c\"", "spawn("] {
        assert!(!runner.contains(never), "{never} in launcher/sources.rs");
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
    use alpheus_commun::launcher_ipc::{DmenuFlags, DndEnd, DndVerb, OsdVerb, QuickVerb, Request};
    fn verb(r: &Request) -> &'static str {
        match r {
            Request::Toggle => "toggle",
            Request::Close => "close",
            Request::Calc => "calc",
            Request::Windows => "windows",
            Request::Dmenu { .. } => "dmenu",
            Request::Dnd(_) => "dnd",
            Request::DndOn(_) => "dnd-on",
            Request::ClearNotifications => "clear-notifications",
            Request::Osd(_) => "osd",
            Request::Quick(_) => "quick",
            Request::Outputs => "outputs",
            Request::OutputsTry(_) => "outputs-try",
            Request::OutputsKeep => "outputs-keep",
            Request::OutputsRevert => "outputs-revert",
        }
    }
    let all = [
        Request::Toggle,
        Request::Close,
        Request::Calc,
        Request::Windows,
        Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() },
        Request::Dnd(DndVerb::Toggle),
        Request::DndOn(DndEnd::For(60)),
        Request::ClearNotifications,
        Request::Osd(OsdVerb::Brightness),
        Request::Quick(QuickVerb::Top),
        Request::Outputs,
        Request::OutputsTry(String::new()),
        Request::OutputsKeep,
        Request::OutputsRevert,
    ];
    assert_eq!(
        all.iter().map(verb).collect::<Vec<_>>(),
        [
            "toggle",
            "close",
            "calc",
            "windows",
            "dmenu",
            "dnd",
            "dnd-on",
            "clear-notifications",
            "osd",
            "quick",
            "outputs",
            "outputs-try",
            "outputs-keep",
            "outputs-revert"
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
    assert_eq!(alpheus_demon::sources::sound::COMMAND, ["pw-dump", "-m", "-N", "-R"]);
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
        ("camera_app.rs", "Timer::", 1),
        // The quick panel's timer, built in `QuickApp::new`: armed only while it is open and a
        // re-read (150 ms, 300 ms), the reboot question (3 s) or a Wi-Fi deadline is pending, or a
        // Bluetooth call's 30 s deadline (suspended while a code shows), its 4 s `failed` or the 3 s
        // forget question (quick delivery 3) is.
        ("quick_app.rs", "Timer::", 1),
        // The notification bubbles' expiry, built in `BubbleSurface::new`: armed only while a live
        // bubble (shown, or hidden by do-not-disturb) has a finite timeout; the OSD's hide timer,
        // built in `OsdSurface::new`: armed only while the pill shows.
        ("bubbles_surface.rs", "Timer::", 1),
        ("osd_surface.rs", "Timer::", 1),
        // The bar's session link's timer, built in `LinkState::new`: armed only while one of its
        // calls is pending — the portal's ReadOne, an MPRIS listing or read (2 s at most each).
        ("link.rs", "Timer::", 1),
        // The displays' one DeadlineTimer (displays spec § 1): the hotplug debounce (500 ms after a
        // change of screens), the watchdog (5 s, while a configuration waits for its answer) or the
        // trial (15 s), armed only while one of them is pending.
        ("outputs_app.rs", "Timer::", 1),
        // The timed do-not-disturb's end, built in `DndTimed::new`: armed only while a timed
        // do-not-disturb is on.
        ("notify_app.rs", "Timer::", 1),
        // The Displays tab's try client's hang-up watch: only while its `outputs-try` waits for
        // the compositor's answer (the watchdog: 5 s at most), removed with the answer.
        ("outputs_app.rs", "Generic::new", 1),
        // A dmenu client's hang-up watch: only while its menu is open.
        ("launcher/host.rs", "Generic::new", 1),
        // The theme's inotify fd: readable only when theme.toml or a colour file
        // changes; `drain` reads it to EAGAIN, so it never wakes the loop at rest.
        ("watch.rs", "Generic::new", 1),
        // The camera watch's inotify fd (privacy delivery 2): readable only on an open, close,
        // creation or removal of a video device; `drain` reads it to EAGAIN.
        ("camera_app.rs", "Generic::new", 1),
        // The quick panel's acting Bluetooth connection (quick delivery 3): inserted only while the
        // Bluetooth detail is open or a call is in flight, removed with it; readable only when BlueZ
        // answers or calls the agent.
        ("bt_app.rs", "Generic::new", 1),
        // The bar's own session link: only when the tray module is off
        // and mode = "auto" or the media module needs the session bus, ONE connection for both;
        // readable only when the portal or a player speaks. Its deadline timer, above, is
        // armed only while one of its calls is pending.
        ("link.rs", "Generic::new", 1),
        // The scan: started at open, ends once its catalog is delivered.
        ("launcher/scan.rs", "thread::Builder", 1),
        // The keyboard, shared by the launcher and the grabbing popups: bound
        // while one of them is open, released once neither is.
        ("popup.rs", "get_keyboard", 1),
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
            "AddMatch",
            "RemoveMatch",
            "GetAll",
            "GetManagedObjects",
            "EnumerateDevices",
            "GetDevices",
            "ListConnections",
            "GetSettings"
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

/// The files of the tray, the popups and the calendar: they run on the
/// bar's loop, never on a thread of their own, and never run a command
/// (no executor, no `actions::run`; naming `actions::Button`, as the
/// popups do, is fine): the applications act, the bar only asks them.
/// Files named before they exist are checked once they do.
const NO_THREAD_NOR_COMMAND: [&str; 52] = [
    "session_bus/",
    "media.rs",
    "notify.rs",
    "osd.rs",
    "osd_surface.rs",
    "backlight.rs",
    "bubbles.rs",
    "bubbles_surface.rs",
    "panel.rs",
    "media_popup.rs",
    "popup.rs",
    "calendar.rs",
    "deadline.rs",
    "list.rs",
    "keys.rs",
    "watch.rs",
    "appearance.rs",
    "state_file.rs",
    "notify_app.rs",
    "link.rs",
    "background.rs",
    "painter/",
    "placement.rs",
    "text.rs",
    "quick.rs",
    "quick_state.rs",
    "quick_wifi.rs",
    "quick_secret.rs",
    "quick_bt.rs",
    "quick_dnd.rs",
    "compositor/mango.rs",
    "sources/mango.rs",
    "compositor/sway.rs",
    "sources/sway.rs",
    "session.rs",
    "power.rs",
    "privacy.rs",
    "camera_dev.rs",
    "camera_app.rs",
    "sources/dbus/bt.rs",
    "sources/dbus/bt_act.rs",
    "bt_app.rs",
    "outputs/",
    "outputs_app.rs",
    "launcher/calc.rs",
    "launcher/calc/units.rs",
    "dnd.rs",
    "battery_alert.rs",
    "hooks.rs",
    "launcher/prefix.rs",
    "launcher/emoji.rs",
    "launcher/emoji/",
    "resources_popup.rs",
];

#[test]
fn the_tray_spawns_no_thread() {
    let mut checked = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !NO_THREAD_NOR_COMMAND.iter().any(|f| if f.ends_with('/') { name.starts_with(f) } else { name == *f }) {
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
/// through `alpheus_demon::log::write` (`writeln!`, a failed write simply
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
    // Ctrl+Enter on a command: detached, bare — never through `sh`, and (Mae, 2026-10-05) never
    // reachable outside the Commands mode, since a command is only ever a result there.
    let detached_commands = ["htop", "nvim"];
    // Enter on a command: held open in the terminal (`terminal_hold`, Mae 2026-10-05).
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
    let password = alpheus_commun::launcher_ipc::DmenuFlags { password: true, ..Default::default() };
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
                assert_eq!(l.selected_hit().map(|h| h.source), Some(alpheus_demon::launcher::matching::Source::Calc));
                let Outcome::Activate(a) = l.key(key) else { panic!("{key:?} on the calculator row activates nothing") };
                let fake = FakeExecutor::new();
                fake.ok("wl-copy", "");
                let ctx = RunContext { exec: &fake, spawner, history_path: None, path_dirs: &copy_dirs };
                assert_eq!(perform(&mut l, a, &ctx), Ok(alpheus_demon::launcher::run::Done::Close));
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
            assert!(matches!(l.selected_hit().map(|h| h.source), Some(alpheus_demon::launcher::matching::Source::Emoji(_))));
            let Outcome::Activate(a) = l.key(key) else { panic!("{key:?} on an emoji row activates nothing") };
            let fake = FakeExecutor::new();
            fake.ok("wl-copy", "");
            let ctx = RunContext { exec: &fake, spawner, history_path: None, path_dirs: &copy_dirs };
            assert_eq!(perform(&mut l, a, &ctx), Ok(alpheus_demon::launcher::run::Done::Close));
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
                    let is_calc = source == alpheus_demon::launcher::matching::Source::Calc;
                    let o = l.click(i, secondary);
                    let copies = matches!(&o, Outcome::Activate(a) if format!("{a:?}").starts_with("Copy"));
                    assert!(
                        !copies || is_calc || matches!(source, alpheus_demon::launcher::matching::Source::Emoji(_)),
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
/// camera_dev.rs (the cameras) alone.
#[test]
fn the_theme_and_camera_watches_only_read() {
    let mut found = false;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = production_code(&t);
        // camera_dev.rs (privacy delivery 2) holds the second inotify instance, on /dev's video nodes.
        if name == "watch.rs" || name == "camera_dev.rs" || name == "inotify.rs" {
            found |= name == "watch.rs";
            for never in
                ["fs::write", "create_dir", "OpenOptions", "remove_file", "remove_dir", "fs::rename", "set_permissions", "File::create"]
            {
                assert!(!prod.contains(never), "{never} in {name}: the config directory and the video devices are read-only to the daemon");
            }
        } else {
            for raw in ["inotify_", "Inotify::new", "inotify::"] {
                assert!(!prod.contains(raw), "inotify ({raw}) outside inotify.rs, watch.rs and camera_dev.rs: {name}");
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
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/notify_app.rs")).unwrap();
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
        let prod = production_code(&t);
        assert!(!prod.contains("state.dnd ="), "state.dnd = in {name}");
        // notify.rs's `Center` has a `dnd` of its own (the server's copy, told by `set_dnd`).
        if name == "notify_app.rs" || name == "notify.rs" {
            continue;
        }
        assert!(!prod.contains("self.dnd ="), "self.dnd = in {name}");
    }
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/notify_app.rs")).unwrap();
    let prod = production_code(&text);
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
    assert!(allowed, "self.dnd = outside dnd_apply and dnd_load in notify_app.rs");
}

/// No panic in the notification server's production code: its model,
/// consumer, bubbles, panel and state files, and the signal builders in bus.rs, end their
/// production part (before the first `#[cfg(`) with no `.expect(`, `.unwrap()` nor `panic!(`.
#[test]
fn the_notification_server_has_no_panicking_path() {
    let panics = |text: &str| [".expect(", ".unwrap()", "panic!("].iter().any(|p| text.contains(p));
    let files = source_files();
    let text =
        |name: &str| files.iter().find(|(p, _)| relative_path(p) == name).map(|(_, t)| t.clone()).unwrap_or_else(|| panic!("{name}"));
    for name in [
        "notify.rs",
        "session_bus/notifications.rs",
        "notify_app.rs",
        "dnd.rs",
        "bubbles.rs",
        "bubbles_surface.rs",
        "panel.rs",
        "state_file.rs",
        "session_bus/codec.rs",
    ] {
        let t = text(name);
        let production = t.split("#[cfg(").next().unwrap_or_default();
        assert!(!panics(production), "a panicking call in {name}'s production code");
    }
    let bus = text("session_bus/bus.rs");
    let start = bus.find("pub fn notify_reply").expect("the notification builders in bus.rs");
    let end = bus.find("pub fn session_address").expect("the builders end before session_address");
    assert!(!panics(&bus[start..end]), "a panicking call in bus.rs's notification builders");
}

/// OSD spec § 2 and its guardrails: the OSD reads sysfs and shows, nothing else. Its three files
/// write no file, create nothing, change no permission, make no raw system call (no `libc::`, no
/// `unsafe`), and, through `NO_THREAD_NOR_COMMAND`, run no command. Production code only.
#[test]
fn the_osd_only_reads() {
    let mut checked = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !["osd.rs", "osd_surface.rs", "backlight.rs"].contains(&name.as_str()) {
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
    assert_eq!(checked, 3, "osd.rs, osd_surface.rs and backlight.rs");
}

/// No panic in the OSD's production code (spec § 5): with its tests left out, each file has no
/// `.expect(`, `.unwrap()` nor `panic!(`.
#[test]
fn the_osd_has_no_panicking_path() {
    let files = source_files();
    for name in ["osd.rs", "osd_surface.rs", "backlight.rs"] {
        let t = &files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1;
        let production = production_code(t);
        for p in [".expect(", ".unwrap()", "panic!("] {
            assert!(!production.contains(p), "{p} in {name}'s production code");
        }
    }
}

/// Resources popup: no panic in the model's production code.
#[test]
fn the_resources_popup_has_no_panicking_path() {
    let files = source_files();
    let t = &files.iter().find(|(p, _)| relative_path(p) == "resources_popup.rs").expect("resources_popup.rs").1;
    let production = production_code(t);
    for p in [".expect(", ".unwrap()", "panic!("] {
        assert!(!production.contains(p), "{p} in resources_popup.rs's production code");
    }
}

/// Quick spec § 5, § 6: the panel's model and its state reader read and decide, nothing else: no
/// file written, nothing created, no raw system call; and, through `NO_THREAD_NOR_COMMAND`, no
/// command run. Production code only.
#[test]
fn the_quick_model_and_its_state_reader_only_read() {
    let mut checked = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if !["quick.rs", "quick_state.rs", "quick_wifi.rs", "quick_bt.rs", "quick_dnd.rs"].contains(&name.as_str()) {
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
            assert!(!prod.contains(never), "{never} in {name}: the quick panel's model only reads");
        }
    }
    assert_eq!(checked, 5, "quick.rs, quick_state.rs, quick_wifi.rs, quick_bt.rs and quick_dnd.rs");
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
        mixer: Some(vec!["fake-mixer".into()]),
        ..QuickConfig::default()
    };
    let sound = SoundState {
        sink: Some(Volume { percent: 64, muted: false }),
        source: Some(Volume { percent: 100, muted: false }),
        ..SoundState::default()
    };
    let profile = ProfileState { active: Some(Profile::Balanced), offered: Profile::ALL.to_vec() };
    let live = Live { sound: Some(&sound), dnd: Some(false), dnd_end: None, profile: Some(&profile), config: &config, bluetooth: None };
    let sinks = vec![Sink { id: 99, label: "Speaker".into(), default: false }, Sink { id: 122, label: "AG06/AG03".into(), default: true }];
    let backlight = Backlight { device: "intel_backlight".into(), percent: 50 };
    let fresh = || {
        let mut q = Quick::new(Focus::Top, &live, sinks.clone(), Some(backlight.clone()), [Some(false), None], 40);
        q.set_headset(Some(Headset { device: 61, label: "CasqueBT".into(), active: HeadsetMode::HiFi, hifi: 1, mic: 3 }));
        q
    };
    let t = Instant::now();
    // Mae: a second activation counts only from CONFIRM_AFTER on; past COALESCE too.
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
        q.wifi.pick = alpheus_demon::quick_wifi::Pick::Network(b"Bureau-Test".to_vec());
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
        record(q.due(t + alpheus_demon::quick_wifi::CONNECT_TIMEOUT).run.into_iter().next().unwrap_or(QuickAction::Nothing));
    }
    // Quick Bluetooth spec § 2: Space on the row with rfkill holding the adapter off unblocks it.
    let bt_off = Live { bluetooth: Some(&BtState::Off), ..live };
    let mut q = Quick::new(Focus::Top, &bt_off, sinks.clone(), Some(backlight.clone()), [Some(false), None], 40);
    q.bt.view =
        Some(BtView { adapter: Some(BtAdapter { path: "/org/bluez/hci0".into(), powered: false, blocked: true }), ..BtView::default() });
    q.selected = quick::Row::Bluetooth;
    record(q.key(&bt_off, Key::Char(' '), false, t));
    // Fix S1's paced paths: a second slider key inside the window waits; the
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
    // Fix S1: absolute levels only, 0 to 100 (`min` up); never a relative step.
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
        "fake-mixer",
    ];
    for c in &calls {
        let ok = match c.split(' ').collect::<Vec<_>>().as_slice() {
            ["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", n] => percent(n, 0),
            ["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@" | "@DEFAULT_AUDIO_SOURCE@", "toggle"] => true,
            ["wpctl", "set-default", id] => ["99", "122"].contains(id),
            ["wpctl", "set-profile", id, idx] => *id == "61" && ["1", "3"].contains(idx),
            ["brightnessctl", "--class=backlight", "--device=intel_backlight", "set", n] => percent(n, 1), // Mae: never below 1 %
            ["powerprofilesctl", "set", p] => ["power-saver", "balanced", "performance"].contains(p),
            ["nmcli", "radio", "wifi", "on" | "off"] => true,
            ["nmcli", "device", "wifi", "rescan"] => true,
            ["nmcli", "connection", "up" | "down", "uuid", u] => [UUID_HOME, UUID_SAVED].contains(u),
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
        "fake-more",
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
        "quick.rs",
        "quick_state.rs",
        "quick_app.rs",
        "quick_wifi.rs",
        "quick_secret.rs",
        "quick_bt.rs",
        "quick_dnd.rs",
        "session.rs",
        "bt_app.rs",
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
    let (quick_app, bt_app) = (src("quick_app.rs"), src("bt_app.rs"));
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

/// Only bt_app.rs drives the acting connection; quick_app.rs only declares its field.
#[test]
fn only_bt_app_and_bt_act_name_btact() {
    let mut naming = Vec::new();
    for (p, t) in source_files() {
        let (name, prod) = (relative_path(&p), production_code(&t));
        if prod.contains("BtAct") {
            naming.push(name.clone());
        }
        if prod.contains(".bt_act") {
            assert_eq!(name, "bt_app.rs", "the acting connection used outside bt_app.rs");
        }
    }
    naming.sort();
    assert_eq!(naming, ["bt_app.rs", "quick_app.rs", "sources/dbus/bt_act.rs"]);
}

/// Quick Bluetooth spec § 2: a log line never names a device (BlueZ paths embed the address), and
/// the glue formats nothing with Debug but an `io::ErrorKind` (an `ActCall`, an `AgentRequest` or a
/// `BtEvent` carries an object path).
#[test]
fn the_bluetooth_logs_name_no_device() {
    let files = source_files();
    let mut lines = 0;
    for name in ["quick_bt.rs", "bt_app.rs", "sources/dbus/bt_act.rs", "sources/dbus/bt.rs"] {
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
    let bt_app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "bt_app.rs").expect("bt_app.rs").1);
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
    let quick_app = src("quick_app.rs");
    let body = function_body(&quick_app, "fn quick_spawn_secret(");
    let (input, sent) = (body.find("spawn_with_input("), body.find("password_sent()"));
    assert!(matches!((input, sent), (Some(i), Some(s)) if i < s), "handed on stdin, then the field closed: {body}");
    assert!(body.contains("b\"\\n\""), "followed by a newline: {body}");
    assert!(!body.contains(".spawn("), "never an argument: {body}");
    for name in ["quick.rs", "quick_wifi.rs", "quick_secret.rs", "quick_app.rs", "popup.rs"] {
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
    let secret = src("quick_secret.rs");
    assert_eq!(secret.matches("unsafe").count(), 1, "one unsafe block: the volatile write");
    assert!(secret.contains("write_volatile") && secret.contains("compiler_fence"));
    assert!(!secret.contains("derive(Debug") && !secret.contains("derive(Clone"), "its own Debug (no byte) and Clone");
}

/// Quick Wi-Fi spec § 6: no panic in the Wi-Fi reader (the D-Bus thread's): bounded, cleaned, never
/// unwrapped.
#[test]
fn the_wifi_reader_has_no_panicking_path() {
    let files = source_files();
    let t = &files.iter().find(|(p, _)| relative_path(p) == "sources/dbus/wifi.rs").expect("sources/dbus/wifi.rs").1;
    let production = production_code(t);
    for p in [".expect(", ".unwrap()", "panic!("] {
        assert!(!production.contains(p), "{p} in sources/dbus/wifi.rs's production code");
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
    let (popup, quick_app, notify_app) = (src("popup.rs"), src("quick_app.rs"), src("notify_app.rs"));
    let close = function_body(&popup, "pub fn popup_close(");
    // Fix S1: a slider level still waiting for its window is sent before the panel's body goes:
    // the row's `flush` runs before the take, its `closed` once the body is dropped.
    let (flush, take) = (close.find("(p.ops().flush)(self)"), close.find("self.popup.take()"));
    assert!(matches!((flush, take), (Some(f), Some(t)) if f < t), "popup_close flushes the sliders first: {close}");
    let (dropped, closed) = (close.find("drop(open)"), close.find("(p.ops().closed)(self)"));
    assert!(matches!((dropped, closed), (Some(d), Some(c)) if d < c), "popup_close must let the quick panel go: {close}");
    let row = popup_ops_row(&quick_app);
    assert!(row.contains("flush: App::quick_flush,") && row.contains("closed: App::quick_closed,"), "the quick panel's row: {row}");
    // The media popup's and the panel's card's Seeked rule and 1 s tick go when either closes.
    for name in ["media_app.rs", "panel_app.rs"] {
        let text = src(name);
        let row = popup_ops_row(&text);
        assert!(row.contains("closed: App::media_closed,"), "{name}: the tick must go with the popup: {row}");
    }
    let media_closed = function_body(&src("media_app.rs"), "pub(crate) fn media_closed(").to_string();
    assert!(media_closed.contains("popup_closed(out)") && media_closed.contains("self.media_timer_sync()"), "{media_closed}");
    let tick = function_body(&quick_app, "pub(crate) fn quick_tick(");
    assert!(tick.contains("due.run") && tick.contains("quick_spawn("), "the tick sends the levels that waited: {tick}");
    let closed = function_body(&quick_app, "pub(crate) fn quick_closed(");
    // Mae's ruling P1: the timer goes with the panel, unless an attempt on a new network is still
    // followed (then it stays on that attempt's deadline only).
    assert!(
        closed.contains(".close()")
            && closed.contains("quick.timer.sync_opt(")
            && closed.contains("self.quick.wifi_follow.as_ref().map(WifiFollow::deadline)"),
        "{closed}"
    );
    // Quick Wi-Fi spec § 5 and Mae's ruling P1: the D-Bus thread stops reading the Wi-Fi, unless an
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
    // Mae's ruling P1: with no panel, the state and the clock go to the follower, which ends with
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
    for name in ["power.rs", "power_app.rs"] {
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
    for name in ["launcher/calc.rs", "launcher/calc/units.rs", "hooks.rs", "hooks_app.rs"] {
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
    let (popup, power_app) = (src("popup.rs"), src("power_app.rs"));
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
    let mut texts = vec![src("privacy.rs"), src("privacy_app.rs")];
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
    let app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "privacy_app.rs").expect("privacy_app.rs").1);
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
    let (app, popup) = (src("privacy_app.rs"), src("popup.rs"));
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
    let app = src("resources_app.rs");
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
    ["camera_dev.rs", "camera_app.rs"]
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
    assert_eq!(dev.0, "camera_dev.rs");
    for never in ["File::open", "OpenOptions", "fs::read("] {
        assert!(!dev.1.contains(never), "{never} in camera_dev.rs's production code");
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
    let quick_bt = production_code(&files.iter().find(|(p, _)| relative_path(p) == "quick_bt.rs").expect("quick_bt.rs").1);
    for copy in ["\\u{f00af}", "\\u{f00b1}", "\u{f00af}", "\u{f00b1}"] {
        assert!(!quick_bt.contains(copy), "a copy of a module glyph in quick_bt.rs: {copy}");
    }
    assert!(quick_bt.contains("segments::{BLUETOOTH, BLUETOOTH_CONNECTED}"), "quick_bt.rs takes the module's glyphs");
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
    let feed = function_body(&src("notify_app.rs"), "pub(crate) fn battery_feed(").to_string();
    for text in [src("battery_alert.rs"), feed] {
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
    let app = production_code(&files.iter().find(|(p, _)| relative_path(p) == "notify_app.rs").expect("notify_app.rs").1);
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
    // nonblocking.rs declares and delegates it (twice); hooks_app.rs calls it once.
    assert_eq!(seen.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["hooks_app.rs", "nonblocking.rs"]);
    assert_eq!(seen[0].1, 1, "hooks_app.rs starts a hook in one place");
}

/// Hooks final review I1: the hooks see a sound or compositor message before the privacy indicators
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
    let t = &files.iter().find(|(p, _)| relative_path(p) == "hooks_app.rs").expect("hooks_app.rs").1;
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
        ("notify_app.rs", "fn dnd_apply(", "hooks_dnd("),
        ("appearance.rs", "fn publish_mode(", "hooks_mode("),
        ("privacy_app.rs", "fn privacy_changed(", "hooks_capture("),
        ("startup.rs", "pub fn run(", "hooks_started("),
    ] {
        let t = text(file);
        assert!(function_body(&t, signature).contains(call), "{signature} in {file} must call {call}");
    }
    let app = production_code(&text("hooks_app.rs"));
    let mut checked = 0;
    for (at, _) in app.match_indices("pub(crate) fn hooks_") {
        let body = function_body(&app[at..], "pub(crate) fn hooks_");
        let first = body.trim_start_matches('{').trim_start();
        assert!(first.starts_with("let Some("), "a hooks_ entry point must start with `let Some(`: {first:.60}");
        checked += 1;
    }
    assert_eq!(checked, 6);
}
