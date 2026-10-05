//! Guardrails from the framing document for the daemon, checked
//! mechanically: it only ever runs the workspace command (`wm-set
//! workspace …` by default), only ever reads the compositor (four fixed
//! Hyprland requests, the niri stream), only opens the compositor's
//! sockets in `src/sources/`, never spawns a process directly, and the
//! forbidden words appear nowhere in its sources.

use alpheus_commun::exec::fake::FakeExecutor;
use alpheus_demon::actions::{Button, command, run};
use alpheus_demon::compositor::{CompositorState, Window, Workspace};
use alpheus_demon::config::BarConfig;
use alpheus_demon::sources::dbus::dbusrs_bus::{LOGIN1_RULE, MATCH_RULES, Method};
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

#[test]
fn the_daemon_only_reads_from_the_compositor() {
    for (p, t) in source_files() {
        let lower = t.to_lowercase();
        for word in ["dpms", "hyprctl", "niri msg", "swaymsg", "keyword", "\"dispatch"] {
            assert!(!lower.contains(word), "\"{word}\" in {}", p.display());
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
        if uses_crate(&t, "dbus") {
            assert_eq!(name, "sources/dbus/dbusrs_bus.rs", "the dbus crate used in {name}");
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
