//! Guardrails from the framing document for the daemon, checked
//! mechanically: it only ever runs the workspace command (`wm-set
//! workspace …` by default), only ever reads the compositor (four fixed
//! Hyprland requests, the niri stream), only opens the compositor's
//! sockets in `src/sources/`, never spawns a process directly, and the
//! forbidden words appear nowhere in its sources.

use alpheus_commun::exec::fake::FakeExecutor;
use alpheus_demon::actions::{Button, command, run};
use alpheus_demon::appearance::SETTING_RULE;
use alpheus_demon::compositor::{CompositorState, Toplevel, Window, Workspace};
use alpheus_demon::config::BarConfig;
use alpheus_demon::keys::Key;
use alpheus_demon::launcher::config::LauncherConfig;
use alpheus_demon::launcher::desktop::{DesktopAction, DesktopEntry};
use alpheus_demon::launcher::history::History;
use alpheus_demon::launcher::matching::Mode;
use alpheus_demon::launcher::model::{Launcher, Outcome};
use alpheus_demon::launcher::run::{RunContext, SYSTEMD_RUN, Spawner, perform};
use alpheus_demon::launcher::scan::Catalog;
use alpheus_demon::sources::dbus::dbusrs_bus::{LOGIN1_RULE, MATCH_RULES, Method};
use alpheus_demon::sources::{hypr::Request, niri::REQUEST};
use alpheus_demon::state::{Bar, Msg};
use alpheus_demon::system::*;
use alpheus_demon::tray::bus::{Call, DBUS_NAME_FLAG_DO_NOT_QUEUE, Method as TrayMethod, message};
use alpheus_demon::tray::{APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, PORTAL_NAME, PORTAL_PATH, SETTINGS_INTERFACE, WATCHER_NAME};
use std::path::{Path, PathBuf};

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
    })));
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 50, muted: false }) })));
    b.apply(Msg::Network(Some(NetState::Online { link: Link::Wifi, vpns: 0, limited: false })));
    b.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(72) }] })));
    b.apply(Msg::Battery(Some(PowerState { laptop: Some(Battery { percent: 60, charge: Charge::Discharging }), peripherals: vec![] })));
    b.apply(Msg::Resources(Sample { cpu: Some(12), memory: Some(61), temperature: Some(60) }));
    b
}

/// Every zone of a bar where every module is shown, every button: only the
/// commands of the allowlist (Global Constraints), and each of them reached.
#[test]
fn every_click_on_a_full_bar_stays_on_the_allowlist() {
    let b = full_bar();
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
    let fixed = [
        "alpheus-toggle sound",
        "alpheus-toggle network",
        "alpheus-toggle bluetooth",
        "alpheus-toggle home",
        "foot -e btop",
        "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle",
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+",
        "wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%-",
    ];
    let calls = fake.calls();
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
}

fn source_files() -> Vec<(PathBuf, String)> {
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
/// (read by the launcher) is erased before the search instead.
const FORBIDDEN_WORDS: [&str; 6] = ["dpms", "hyprctl", "niri msg", "swaymsg", "keyword", "\"dispatch"];

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
}

#[test]
fn the_daemon_only_reads_from_the_compositor() {
    for (p, t) in source_files() {
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
    }
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
    for (p, t) in source_files() {
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
/// stacked beneath them instead of above (review item M2, final review
/// slice 3). No fake layer-shell exists to exercise the real stacking end to
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

/// Review item M6, final review slice 3: every seat/keyboard entry point
/// that can reach launcher code goes through `launcher_guarded` — a panic
/// there must close the launcher, never take the bar down with it.
///
/// Task 15 (popups) moved the keyboard out of `OpenLauncher` into `App`,
/// shared with a grabbing popup: `new_capability`/`remove_capability`/
/// `remove_seat` (wayland.rs) now call the shared `keyboard_acquire`/
/// `keyboard_lost` (popup.rs), which only touch Wayland/seat state, not the
/// launcher's model — nothing there to guard anymore. The guard moved with
/// the actual launcher calls: `popup.rs`'s `key_event` (every keypress,
/// including `update_modifiers`'s own `self.mods` write, touches no
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

/// Task 15 fix round, minor (e): `press_key`/`repeat_key` (the `KeyboardHandler` trait methods)
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

/// Task 15 fix round, CRITICAL: `popup_close`/`popup_output_gone` (popup.rs) must reset their
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

/// Task 15 fix round, from the task 16 review: `hover_fire` (wayland.rs) used to re-locate its bar
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

/// `launcher_fail` must take the launcher out of `self` FIRST, then release the keyboard, then
/// early-return on "nothing was open". `keyboard_release` is a no-op while `keyboard_wanted()`
/// holds, which it does as long as `self.launcher` is `Some`: releasing before the take (the order
/// task 15 locked in, final review R2) left the keyboard, its keymap and its repeat source resident
/// after a panic closed the launcher. And the release must precede the early return, for a panic
/// inside `launcher_close` that already took the launcher. No `wl_keyboard` can be built without a
/// compositor, so the order is checked in the source.
#[test]
fn launcher_fail_takes_the_launcher_then_releases_the_keyboard_then_returns_early() {
    let files = source_files();
    let host = &files.iter().find(|(p, _)| relative_path(p) == "launcher/host.rs").expect("launcher/host.rs").1;
    let prod = production_code(host);
    let body = function_body(&prod, "fn launcher_fail(");
    let take_at = body.find("self.launcher.take()").expect("launcher_fail must take the launcher");
    let release_at = body.find("self.keyboard_release()").expect("launcher_fail must call keyboard_release");
    let early_return_at = body.find("else { return }").expect("launcher_fail must early-return when nothing was open");
    assert!(take_at < release_at, "the launcher must be taken BEFORE keyboard_release (still wanted otherwise): {body}");
    assert!(release_at < early_return_at, "keyboard_release must run before the early return: {body}");
}

/// Final review R2: three small orderings no fake Wayland surface can exercise. `menu_repaint`
/// finds its bar by the popup's parent SURFACE (never an output name); `launcher_open` drops the
/// remembered tray press (`last_icon`), so a late failed Activate cannot open a menu over the
/// launcher; and the bar's keyboard interactivity is only reset where `popup_open` set it
/// (`exclusive_during_grab`, false on Hyprland), at both places.
#[test]
fn menu_repaint_launcher_open_and_the_interactivity_reset_keep_their_guards() {
    let files = source_files();
    let src = |name: &str| production_code(&files.iter().find(|(p, _)| relative_path(p) == name).unwrap_or_else(|| panic!("{name}")).1);
    let (wayland, popup, surface) = (src("wayland.rs"), src("popup.rs"), src("launcher/surface.rs"));
    let repaint = function_body(&wayland, "fn menu_repaint(");
    assert!(!repaint.contains(".name =="), "menu_repaint must not find its bar by name: {repaint}");
    assert!(repaint.contains("position_by"), "menu_repaint must find its bar by the popup's parent surface: {repaint}");
    let open = function_body(&surface, "pub fn launcher_open(");
    assert!(open.contains("self.last_icon = None"), "launcher_open must drop the remembered tray press: {open}");
    let close = function_body(&popup, "pub fn popup_close(");
    assert!(close.contains("self.exclusive_during_grab"), "popup_close must only reset what popup_open set: {close}");
    let opening = function_body(&popup, "pub fn popup_open(");
    assert!(
        opening.contains(
            "if grab && self.exclusive_during_grab {\n                    layer.set_keyboard_interactivity(KeyboardInteractivity::None)"
        ),
        "popup_open's error path must only reset what it set: {opening}"
    );
}

/// Task 15 fix round, from the task 16 review: an output removal (`popup_output_gone`) and the
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

/// Fix round 2026-10-06, item 3: `tooltip_item_gone` (wayland.rs) relies on a Tooltip popup
/// always being paired with `tooltip_item` — set together in `hover_fire`, so it must be cleared
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

/// Not a behavioural test of the socket itself: this match has no wildcard,
/// so it is an exhaustive-match tripwire — a fourth `Request` variant stops
/// THIS FILE compiling until the match below (and the review that goes with
/// it) is updated, before it could ever reach the socket unexamined (review
/// item M6, final review slice 1: renamed from
/// `the_socket_accepts_exactly_three_requests`, which claimed to test the
/// socket's own behaviour when it only guards this match's exhaustiveness).
#[test]
fn adding_a_request_variant_must_update_this_exhaustive_match() {
    use alpheus_commun::launcher_ipc::Request;
    fn verb(r: &Request) -> &'static str {
        match r {
            Request::Toggle => "toggle",
            Request::Close => "close",
            Request::Dmenu { .. } => "dmenu",
        }
    }
    let all = [Request::Toggle, Request::Close, Request::Dmenu { prompt: None, lines: vec![] }];
    assert_eq!(all.iter().map(verb).collect::<Vec<_>>(), ["toggle", "close", "dmenu"]);
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
/// strip every real item in between along with it (A-m3, review).
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
        // The bar's clock and resources timers; the launcher's request
        // deadline, armed per connection and removed once it is read; the
        // listener's resume, armed only after an accept() error.
        ("launch.rs", "Timer::", 4),
        // DeadlineTimer::default() and Timer::from_deadline: the tray's call
        // timeout, armed only while a call is pending (task 1; wired into
        // the loop at task 11).
        ("deadline.rs", "Timer::", 2),
        // The bar's signal guard.
        ("launch.rs", "thread::Builder", 1),
        // The launcher's listening socket (its fd: the one thing at rest),
        // one watch per connection being read, and the tray's session-bus fd
        // (task 11; only inserted when "tray" is in modules and a session
        // bus answers — an unused module costs nothing at runtime, but the
        // call itself is still in the source).
        ("launch.rs", "Generic::new", 3),
        // The tray's DeadlineTimer (task 11), armed only while calls are pending; the tooltip's
        // (task 16), armed only while the pointer rests on a tray icon; the bar's session link's
        // (media task 5, `link_timer`), armed only while one of its calls is pending — the portal's
        // ReadOne, an MPRIS listing or read (2 s at most each) — never at rest.
        ("wayland.rs", "Timer::", 3),
        // A dmenu client's hang-up watch: only while its menu is open.
        ("launcher/host.rs", "Generic::new", 1),
        // The theme's inotify fd (themes task 5): readable only when theme.toml or a colour file
        // changes; `drain` reads it to EAGAIN, so it never wakes the loop at rest.
        ("watch.rs", "Generic::new", 1),
        // The bar's own session link (themes task 6, media task 5): only when the tray module is off
        // and mode = "auto" or the media module needs the session bus, ONE connection for both;
        // readable only when the portal or a player speaks. Its deadline timer, in wayland.rs, is
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
        if name.starts_with("sources/dbus/") {
            let prod = production_code(&t);
            for word in [
                "\"Set\"",
                "\"Connect\"",
                "\"Disconnect\"",
                "\"ActivateConnection\"",
                "\"DeactivateConnection\"",
                "\"Enable\"",
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
    assert_eq!(names, ["GetNameOwner", "AddMatch", "GetAll", "GetManagedObjects", "EnumerateDevices"]);
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
        "the network module is icons only (Q1): no AccessPoint signal is ever read"
    );
    assert!(LOGIN1_RULE.starts_with("type='signal',"), "{LOGIN1_RULE}");
}

/// The files that may name the `dbus` crate: the system bus's, and the
/// tray's four (its connection, codec, watcher and router).
const DBUS_FILES: [&str; 5] = ["sources/dbus/dbusrs_bus.rs", "tray/bus.rs", "tray/codec.rs", "tray/watcher.rs", "tray/router.rs"];

/// The tray's D-Bus (spec § 3, controller decisions 1 and 3): dbus-crossroads
/// in the watcher only; one place builds a call, from the typed allowlist;
/// no libdbus session lookup (it could autolaunch `dbus-launch`); no
/// blocking call nor `RefArg` decoding; the only name ever requested is the
/// watcher's, with DO_NOT_QUEUE, never replacing its owner.
#[test]
fn tray_d_bus_is_confined_and_allowlisted() {
    let mut built = Vec::new();
    let mut tray_files = 0;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if uses_crate(&t, "dbus_crossroads") {
            assert_eq!(name, "tray/watcher.rs", "dbus-crossroads used in {name}");
        }
        if !name.starts_with("tray/") {
            continue;
        }
        tray_files += 1;
        for never in ["BusType::Session", "get_private"] {
            assert!(!t.contains(never), "{never} in {name}: libdbus's own session lookup may autolaunch a bus");
        }
        let prod = production_code(&t);
        for never in
            ["send_with_reply_and_block", "new_session(", "blocking::", ".process(", "get_refarg", "PropMap", "RefArg", "REPLACE_EXISTING"]
        {
            assert!(!prod.contains(never), "{never} in {name}");
        }
        // "method_call(" alone catches `Message::new_method_call(` too (it is a substring of it): a
        // bare crossroads-style `ctx.method_call(` or any future way of building a call is caught the
        // same way, without double-counting the one already-allowed spelling. Consequence: no
        // private method under tray/ may be named `method_call` either — its definition
        // (`fn method_call(`) and every call site (`self.method_call(`) are substring matches too,
        // failing this guardrail even though the method has nothing to do with building a
        // `dbus::Message` (task 5's router dispatch method is named `inbound_call`, not
        // `method_call`, for exactly this reason — see its task 5 report).
        for pattern in ["method_call(", "call_with_args("] {
            built.extend(std::iter::repeat_n(name.clone(), prod.matches(pattern).count()));
        }
    }
    assert!(tray_files >= 2, "the tray's sources were not found");
    assert_eq!(built, ["tray/bus.rs"], "every tray call is built by bus::message, from a Call");
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
}

/// The portal (themes task 6): exactly two members, `ReadOne` (a call, one setting) and
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
/// Files named before they exist (later tasks) are checked once they do.
const NO_THREAD_NOR_COMMAND: [&str; 13] = [
    "tray/",
    "media.rs",
    "popup.rs",
    "calendar.rs",
    "deadline.rs",
    "list.rs",
    "keys.rs",
    "watch.rs",
    "appearance.rs",
    "link.rs",
    "background.rs",
    "painter/",
    "placement.rs",
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
/// someone else's path (`sources::dbus::…`, `dbusrs_bus::…`) (fix round,
/// review item 5: `::dbus::`, `use dbus as`, `extern crate dbus` were
/// previously missed).
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
/// same way; the focus template, never wrapped. And each shape is reached.
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
    // Every program the fixture may start, as executable stubs on a $PATH of
    // its own (never run: the executor is fake). `_bin_guard` removes the
    // directory when this function returns, even if an assertion below
    // panics first.
    let bin = std::env::temp_dir().join(format!("alpheus-guardrails-{}-bin", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    let _bin_guard = RemoveOnDrop(bin.clone());
    for p in ["firefox", "foot", "nautilus", "htop", "nvim", "sh"] {
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
                l.click_mode(mode);
                l
            };
            let mut outcomes = Vec::new();
            let n = fresh().results.len();
            for i in 0..n {
                for secondary in [false, true] {
                    outcomes.push((fresh().click(i, secondary), mode));
                }
            }
            let mut typed = fresh();
            for c in "ls -la".chars() {
                typed.key(Key::Char(c));
            }
            outcomes.push((typed.key(Key::Enter), mode));
            outcomes.push((typed.key(Key::CtrlEnter), mode));
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
                outcomes.push((via_prefix.key(Key::Enter), Mode::Commands));
            }
            for (o, mode) in outcomes {
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
                    } else if let Some(text) =
                        unwrapped.strip_prefix("sh -c -- ").or_else(|| unwrapped.strip_prefix("foot --hold -e sh -c -- "))
                    {
                        assert_eq!(mode, Mode::Commands, "a shell outside the Commands mode: {c}");
                        assert_eq!(text, "ls -la", "only typed text goes through the shell: {c}");
                        if unwrapped.starts_with("sh") { "shell" } else { "terminal hold shell" }
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
        ("focus", false),
    ] {
        assert!(seen.contains(&expected), "{expected:?} never reached: {seen:?}");
    }
    assert!(!seen.contains(&("focus", true)), "focus never in a scope");
}

/// Themes task 4: the daemon paints with the 16 roles only. Task 1's temporary 7-colour
/// `Palette` (deleted by task 7) is never named in its sources.
#[test]
fn the_daemon_paints_with_roles_never_the_legacy_palette() {
    let files = source_files();
    for (p, t) in &files {
        assert!(!t.contains("Palette"), "the legacy Palette in {}", relative_path(p));
    }
    assert!(files.len() > 20, "the daemon's sources were not found");
}

/// Themes task 5: the config directory is only watched, never written by the daemon; inotify lives
/// in watch.rs alone.
#[test]
fn the_theme_watch_only_reads_the_config_directory() {
    let mut found = false;
    for (p, t) in source_files() {
        let name = relative_path(&p);
        let prod = production_code(&t);
        if name == "watch.rs" {
            found = true;
            for never in
                ["fs::write", "create_dir", "OpenOptions", "remove_file", "remove_dir", "fs::rename", "set_permissions", "File::create"]
            {
                assert!(!prod.contains(never), "{never} in watch.rs: the config directory is read-only to the daemon");
            }
        } else {
            assert!(!prod.contains("inotify_"), "inotify outside watch.rs: {name}");
        }
    }
    assert!(found, "watch.rs not found");
}

/// The theme is resolved once more after everything it depends on is up (final review): after the
/// watch is armed (a file written between the first read and the watch would be missed for the
/// whole session), and after `start_tray` and `follow_portal_if_auto` (earlier, the portal start
/// sees no bus and gives up for good).
#[test]
fn the_startup_theme_is_resolved_again_after_the_watch_and_the_portal() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/launch.rs")).unwrap();
    let prod = production_code(&text);
    let at = |needle: &str| prod.find(needle).unwrap_or_else(|| panic!("{needle} not found in launch.rs"));
    let last = at("state.theme_changed();");
    assert_eq!(prod.matches("state.theme_changed();").count(), 1);
    for before in ["crate::watch::start(", "start_tray(&mut state", "state.follow_portal_if_auto();", "state.publish_mode();"] {
        assert!(at(before) < last, "{before} must come before the startup re-resolve");
    }
}
