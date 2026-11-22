//! Mechanically checked guardrails from the scoping doc: only the planned
//! commands are launched; nothing talks to the compositor directly, and
//! nothing touches turning displays off.

use alpheus::app::worker::{Domain, Op, fetch, perform};
use alpheus::backends::{Backends, NodeRef, PowerAction};
use alpheus::displays::{Plan, Step};
use alpheus::exec::fake::FakeExecutor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ALLOWED: [&str; 13] = [
    "wpctl",
    "brightnessctl",
    "nmcli",
    "bluetoothctl",
    "powerprofilesctl",
    "shell-ctl",
    "tools-menu",
    "hyprlock",
    "wm-set",
    "systemctl",
    "journalctl",
    "coredumpctl",
    "systemd-analyze",
];

/// The bug report's clipboard program, read from the config's default rather than spelled here: the
/// daemon's guardrail keeps its name out of every test file, so that no test can start it by hand.
fn copy_program() -> String {
    alpheus::config::DiagnosticsConfig::default().copy.remove(0)
}

/// The only `systemctl` lines the TUI's executor may run (spec § 8): the three power actions, the
/// diagnostics' reads, and the two user-unit actions (`--` before the unit, `--no-block` for the
/// restart). System-unit actions never reach the executor: they run in the TUI's own window.
fn systemctl_line_is_allowed(line: &str) -> bool {
    if ["systemctl suspend", "systemctl reboot", "systemctl poweroff"].contains(&line) {
        return true;
    }
    let rest = line.strip_prefix("systemctl ").unwrap_or("");
    let user = rest.strip_prefix("--user ");
    let (scoped, body) = (user.is_some(), user.unwrap_or(rest));
    let unit_action = |prefix: &str| body.strip_prefix(prefix).is_some_and(|u| !u.is_empty() && !u.contains(' '));
    match body {
        "list-units --failed --output=json --no-pager"
        | "list-units --type=service --all --output=json --no-pager"
        | "show --property=Id,NRestarts,ActiveState,SubState --no-pager *.service" => true,
        "show-environment" => scoped,
        "--version" => !scoped,
        _ => scoped && (unit_action("--no-block restart -- ") || unit_action("reset-failed -- ")),
    }
}

/// The diagnostics backend's reads and actions, all of them, on the fake executor.
fn exercise_diagnostics(b: &Backends) {
    use alpheus::backends::UnitAction;
    use alpheus::diagnostics::query::{JournalQuery, Preset, Scope};
    for scope in [Scope::System, Scope::User] {
        let _ = b.diagnostics.failed(scope);
        let _ = b.diagnostics.restarts(scope, 3);
        let _ = b.diagnostics.service_units(scope);
    }
    let _ = b.diagnostics.crashes();
    let _ = b.diagnostics.boot_time();
    let _ = b.diagnostics.blame(15);
    let _ = b.diagnostics.critical_chain(60);
    let _ = b.diagnostics.session_env(&["WAYLAND_DISPLAY"]);
    let _ = b.diagnostics.systemd_version();
    for preset in Preset::ALL {
        let mut q = JournalQuery::new(preset, 500);
        if preset == Preset::ServiceLogs {
            q.unit = Some((Scope::User, "-.mount".into()));
        }
        let _ = b.diagnostics.journal(&q);
        q.after_cursor = Some("s=1;i=1".into());
        let _ = b.diagnostics.journal(&q);
    }
    let _ = b.diagnostics.user_action(UnitAction::Restart, "-.mount");
    let _ = b.diagnostics.user_action(UnitAction::ResetFailed, "example-sync.service");
    let _ = b.diagnostics.copy(&[copy_program()], "report");
}

#[test]
fn every_launched_command_is_on_the_allowlist() {
    let fake = Arc::new(FakeExecutor::new());
    // The displays ask the daemon's socket in the runtime directory: one that does not exist here,
    // so they reach no daemon (never the session's), and run no command.
    let runtime = std::env::temp_dir().join(format!("alpheus-guardrails-{}-no-runtime", std::process::id()));
    let b = Backends::system(fake.clone(), runtime);
    for d in Domain::ALL {
        let _ = fetch(&b, d);
    }
    let ops = [
        Op::SetVolume(NodeRef::DefaultSink, 50),
        Op::SetVolume(NodeRef::Id(64), 50),
        Op::SetDefault(64),
        Op::SetBrightness(50),
        Op::SetWifi(true),
        Op::SetWifi(false),
        Op::SetBluetooth(true),
        Op::SetBluetooth(false),
        Op::SetDnd(true),
        Op::SetDnd(false),
        Op::ToggleKeepAwake { now_on: true },
        Op::SetProfile("balanced".into()),
        Op::TryDisplays(Plan { steps: vec![Step { output: "eDP-1".into(), active: None }] }),
        Op::KeepDisplays,
        Op::RevertDisplays,
    ];
    for op in ops.into_iter().chain(PowerAction::ALL.map(Op::Power)) {
        let _ = perform(&b, &op);
    }
    exercise_diagnostics(&b);
    let calls = fake.calls();
    // 24 calls today; the floor drops by the three `wm-set` calls the displays no longer make
    // (`outputs`, `monitor`, `monitor-disable`): 20 → 17; the diagnostics add 12 reads, 16
    // journal reads, two actions and the copy.
    assert!(calls.len() >= 17 + 30, "{calls:?}");
    for c in &calls {
        let prog = c.split(' ').next().unwrap();
        assert!(ALLOWED.contains(&prog) || prog == copy_program(), "command off the list: {c}");
        assert!(!c.to_lowercase().contains("dpms"), "turning a display off is forbidden: {c}");
        if prog == "systemctl" {
            assert!(systemctl_line_is_allowed(c), "systemctl line off the list: {c}");
        }
        match prog {
            "journalctl" => assert!(c.starts_with("journalctl -o json --output-fields="), "{c}"),
            "coredumpctl" => assert_eq!(c, "coredumpctl list --json=short --no-pager --reverse --since=-7d -n 20"),
            "systemd-analyze" => {
                assert!(
                    ["systemd-analyze time --no-pager", "systemd-analyze blame --no-pager", "systemd-analyze critical-chain --no-pager"]
                        .contains(&c.as_str()),
                    "{c}"
                )
            }
            p if p == copy_program() => assert_eq!(c, p, "the report goes on its standard input, never an argument"),
            _ => {}
        }
        if prog == "wm-set" {
            // Leaving the session, nothing else: the displays go through the daemon.
            assert_eq!(c, "wm-set exit", "wm-set subcommand off the list");
        }
    }
}

#[test]
fn the_systemctl_line_rule_itself_bites() {
    for good in [
        "systemctl --user --no-block restart -- -.mount",
        "systemctl --user reset-failed -- example-sync.service",
        "systemctl --user show-environment",
        "systemctl --version",
        "systemctl list-units --failed --output=json --no-pager",
        "systemctl --user list-units --failed --output=json --no-pager",
    ] {
        assert!(systemctl_line_is_allowed(good), "{good}");
    }
    for bad in [
        "systemctl --user restart example-app.service",
        "systemctl --user --no-block restart example-app.service",
        "systemctl restart -- example-app.service",
        "systemctl --user --no-block restart -- ",
        "systemctl --user --no-block restart -- a b",
        "systemctl --user stop -- example-app.service",
        "systemctl --user start -- example-app.service",
        "systemctl --user set-environment A=b",
        "systemctl --user import-environment WAYLAND_DISPLAY",
        "systemctl show-environment",
        "systemctl --user --version",
        "systemctl daemon-reload",
    ] {
        assert!(!systemctl_line_is_allowed(bad), "{bad}");
    }
}

/// Source guardrail: the code that builds `systemctl` arguments never spells a verb that starts,
/// stops, enables, masks or kills a unit, nor one that changes the environment (spec § 8). The
/// fix text the tab shows for a check is a string for the eyes, not an argument, and lives in
/// the user's config, never in these files.
#[test]
fn systemctl_never_starts_stops_or_changes_the_environment() {
    const VERBS: [&str; 17] = [
        "start",
        "stop",
        "enable",
        "disable",
        "mask",
        "unmask",
        "kill",
        "isolate",
        "reload",
        "try-restart",
        "reload-or-restart",
        "daemon-reload",
        "edit",
        "set-environment",
        "unset-environment",
        "import-environment",
        "link",
    ];
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut files = Vec::new();
    for krate in ["alpheus", "alpheus-commun"] {
        rs_files(&crates.join(krate).join("src"), &mut files);
    }
    let mut scanned = 0;
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        // Production code only: the tests name the verbs they prove refused.
        let production = text.split("#[cfg(test)]").next().unwrap();
        if !production.contains("\"systemctl\"") {
            continue;
        }
        scanned += 1;
        for verb in VERBS {
            assert!(!production.contains(&format!("\"{verb}\"")), "\"{verb}\" in {}", f.display());
        }
    }
    assert!(scanned >= 2, "the files that run systemctl were not found");
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_source_never_talks_to_the_compositor_or_the_displays_directly() {
    // Every crate in the workspace: crates/*/src (not their tests).
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut files = Vec::new();
    rs_files(&crates, &mut files);
    files.retain(|f| f.components().any(|c| c.as_os_str() == "src"));
    assert!(files.len() > 10);
    for f in files {
        let t = std::fs::read_to_string(&f).unwrap().to_lowercase();
        for word in ["dpms", "hyprctl", "niri msg", "swaymsg", "mmsg"] {
            assert!(!t.contains(word), "\"{word}\" in {}", f.display());
        }
    }
}

/// The health checks connect to the compositor's socket named by the environment: only the real
/// worker (`run.rs`) hands them the process's own; the worker, the checks and every test file get
/// an injected one, so `cargo test` never reaches the session it runs in.
#[test]
fn the_health_checks_never_read_the_process_environment() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("app/worker.rs")];
    rs_files(&src.join("app/diagnostics"), &mut files);
    files.push(src.join("app/diagnostics.rs"));
    let mut tests = Vec::new();
    rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"), &mut tests);
    files.extend(tests.into_iter().filter(|f| f.file_name().is_some_and(|n| n != "guardrails.rs")));
    assert!(files.len() >= 4, "{files:?}");
    for f in files {
        let t = std::fs::read_to_string(&f).unwrap();
        for call in ["env::var_os", "env::var(", "env::vars"] {
            assert!(!t.contains(call), "`{call}` in {}: the environment must be injected", f.display());
        }
    }
}
