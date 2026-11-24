//! Mechanically checked guardrails (docs/SCOPE.md): only the listed
//! commands are launched; nothing talks to the compositor directly, and
//! nothing touches turning displays off.

use alpheus::app::worker::{Domain, Op, fetch, perform};
use alpheus::backends::{Backends, NodeRef, PowerAction};
use alpheus::displays::{Plan, Step};
use alpheus::exec::fake::FakeExecutor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ALLOWED: [&str; 12] = [
    "wpctl",
    "brightnessctl",
    "nmcli",
    "bluetoothctl",
    "powerprofilesctl",
    "shell-ctl",
    "tools-menu",
    "loginctl",
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

/// The only `systemctl` lines the TUI's executor may run: the three power actions, the
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
    let _ = b.diagnostics.report("Mango", 3);
}

#[test]
fn every_launched_command_is_on_the_allowlist() {
    let fake = Arc::new(FakeExecutor::new());
    // The displays ask the daemon's socket in the runtime directory: one that does not exist here,
    // so they reach no daemon (never the session's), and run no command.
    let runtime = std::env::temp_dir().join(format!("alpheus-guardrails-{}-no-runtime", std::process::id()));
    let b = Backends::system_isolated(fake.clone(), runtime);
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
            "journalctl" => {
                assert!(c.starts_with("journalctl -o json --output-fields="), "{c}");
                // The bug report and the view never ask for a command line, an environment or an id.
                assert!(
                    !c.contains("_CMDLINE") && !c.contains("_BOOT_ID") && !c.contains("_MACHINE_ID") && !c.contains("_HOSTNAME"),
                    "{c}"
                );
            }
            "loginctl" => assert_eq!(c, "loginctl lock-session"),
            "coredumpctl" => assert_eq!(c, "coredumpctl list --json=short --no-pager --reverse --since=-7d -n 20"),
            "systemd-analyze" => {
                assert!(
                    ["systemd-analyze time --no-pager", "systemd-analyze blame --no-pager", "systemd-analyze critical-chain --no-pager"]
                        .contains(&c.as_str()),
                    "{c}"
                );
            }
            p if p == copy_program() => assert_eq!(c, p, "the report goes on its standard input, never an argument"),
            _ => {}
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
/// stops, enables, masks or kills a unit, nor one that changes the environment. The
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
    for krate in ["alpheus", "alpheus-common"] {
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
/// entry point (`run.rs`, two reads) hands them the process's own; every other file under `src/`
/// and `tests/` (the worker, the checks, every test) gets an injected one, so `cargo test` never
/// reaches the session it runs in.
#[test]
fn nothing_but_the_real_entry_point_reads_the_process_environment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    rs_files(&root.join("tests"), &mut files);
    assert!(files.len() >= 20, "{files:?}");
    let mut run_rs = 0;
    for f in files.into_iter().filter(|f| f.file_name().is_some_and(|n| n != "guardrails.rs")) {
        let t = std::fs::read_to_string(&f).unwrap();
        let n: usize = ["env::var_os", "env::var(", "env::vars"].iter().map(|call| t.matches(call).count()).sum();
        if f == root.join("src/app/run.rs") {
            run_rs = n;
        } else {
            assert_eq!(n, 0, "an environment read in {}: the environment must be injected", f.display());
        }
    }
    assert_eq!(run_rs, 2, "run.rs reads the environment exactly twice (the mode file's runtime dir and the worker's)");
}

/// `Backends::system` hands the diagnostics the machine's own journal directories, hostname,
/// user and machine id (`/var/log/journal`, `/etc/machine-id`, `$USER`): a test that built it
/// would read the real ones. Only the real worker (`run.rs`'s `run`) builds it; every test uses
/// `Backends::system_isolated`, whose diagnostics look in a directory that does not exist and
/// know no names.
#[test]
fn only_the_real_worker_builds_the_system_backends() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    rs_files(&root.join("tests"), &mut files);
    let mut found = Vec::new();
    for f in files.into_iter().filter(|f| f.file_name().is_some_and(|n| n != "guardrails.rs")) {
        let t = std::fs::read_to_string(&f).unwrap();
        // The text of a call, not of a doc comment that names it.
        for (n, line) in t.lines().enumerate() {
            if line.contains("Backends::system(") && !line.trim_start().starts_with("//") {
                found.push(format!("{}:{}", f.strip_prefix(root).unwrap().display(), n + 1));
            }
        }
    }
    assert_eq!(found.len(), 1, "Backends::system( outside the real worker: {found:?}");
    assert!(found[0].starts_with("src/app/run.rs:"), "{found:?}");
    // And the isolated one never reads the machine.
    let b = Backends::system_isolated(
        Arc::new(FakeExecutor::new()),
        std::env::temp_dir().join(format!("alpheus-guardrails-{}-isolated", std::process::id())),
    );
    assert!(!b.diagnostics.journal_readable());
    let text = b.diagnostics.report("Mango", 3);
    assert!(!text.contains("kernel "), "no kernel release was read: {text}");
}
