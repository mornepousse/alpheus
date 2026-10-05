//! Guardrails from the framing document for the daemon, checked
//! mechanically: it only ever runs the workspace command (`wm-set
//! workspace …` by default), only ever reads the compositor (four fixed
//! Hyprland requests, the niri stream), only opens the compositor's
//! sockets in `src/sources/`, never spawns a process directly, and the
//! forbidden words appear nowhere in its sources.

use alpheus_commun::exec::fake::FakeExecutor;
use alpheus_demon::actions::{Button, command, run};
use alpheus_demon::compositor::{CompositorState, Toplevel, Window, Workspace};
use alpheus_demon::config::BarConfig;
use alpheus_demon::launcher::config::LauncherConfig;
use alpheus_demon::launcher::desktop::{DesktopAction, DesktopEntry};
use alpheus_demon::launcher::history::History;
use alpheus_demon::launcher::input::Key;
use alpheus_demon::launcher::matching::Mode;
use alpheus_demon::launcher::model::{Launcher, Outcome};
use alpheus_demon::launcher::run::{RunContext, SYSTEMD_RUN, Spawner, perform};
use alpheus_demon::launcher::scan::Catalog;
use alpheus_demon::sources::dbus::zbus_bus::{LOGIN1_RULE, MATCH_RULES, Method};
use alpheus_demon::sources::{hypr::Request, niri::REQUEST};
use alpheus_demon::state::{Bar, Msg};
use alpheus_demon::system::*;
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

/// The socket accepts exactly toggle, close and dmenu (spec § 1): this match
/// has no wildcard, so a fourth request does not compile until this
/// guardrail (and the review that goes with it) is updated.
#[test]
fn the_socket_accepts_exactly_three_requests() {
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

/// D-Bus only in src/sources/ (like the compositor's sockets), through one
/// file, which can only call read methods; no word of a method that changes
/// anything in the D-Bus sources.
#[test]
fn d_bus_is_confined_to_sources_and_read_only() {
    for (p, t) in source_files() {
        let name = relative_path(&p);
        if t.contains("zbus::") {
            assert_eq!(name, "sources/dbus/zbus_bus.rs", "zbus used in {name}");
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
    let file = source_files().into_iter().find(|(p, _)| relative_path(p) == "sources/dbus/zbus_bus.rs").expect("zbus_bus.rs").1;
    assert_eq!(production_code(&file).matches("call_method(").count(), 1, "every call goes through `call`, which takes a `Method`");
    for (_, rule) in MATCH_RULES {
        assert!(rule.starts_with("type='signal',"), "{rule}");
    }
    assert!(
        MATCH_RULES.iter().all(|(_, r)| !r.contains("AccessPoint")),
        "the network module is icons only (Q1): no AccessPoint signal is ever read"
    );
    assert!(LOGIN1_RULE.starts_with("type='signal',"), "{LOGIN1_RULE}");
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
/// command; `sh -c` only from the Commands mode; the focus template, never
/// wrapped. And each shape is reached.
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
    // its own (never run: the executor is fake).
    let bin = std::env::temp_dir().join(format!("alpheus-guardrails-{}-bin", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    for p in ["firefox", "foot", "nautilus", "htop", "nvim", "sh"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(bin.join(p), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path_dirs = [bin.clone()];
    let apps = ["firefox", "firefox --new-window", "foot -e btop", "nautilus --new-window", "htop", "nvim"];
    let terminal_commands = ["foot -e htop", "foot -e nvim"];
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
                    } else if terminal_commands.contains(&unwrapped) {
                        assert_eq!(mode, Mode::Commands, "a $PATH command outside the Commands mode: {c}");
                        "terminal command"
                    } else if let Some(text) = unwrapped.strip_prefix("sh -c ").or_else(|| unwrapped.strip_prefix("foot -e sh -c ")) {
                        assert_eq!(mode, Mode::Commands, "a shell outside the Commands mode: {c}");
                        assert_eq!(text, "ls -la", "only typed text goes through the shell: {c}");
                        if unwrapped.starts_with("sh") { "shell" } else { "terminal shell" }
                    } else {
                        panic!("command outside the allowlist: {c}")
                    };
                    seen.push((shape, c.starts_with(&scope)));
                }
            }
        }
    }
    for expected in
        [("program", true), ("program", false), ("terminal command", true), ("shell", true), ("terminal shell", false), ("focus", false)]
    {
        assert!(seen.contains(&expected), "{expected:?} never reached: {seen:?}");
    }
    assert!(!seen.contains(&("focus", true)), "focus never in a scope");
    let _ = std::fs::remove_dir_all(&bin);
}
