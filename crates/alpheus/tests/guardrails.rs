//! Mechanically checked guardrails from the scoping doc: only the planned
//! commands are launched; nothing talks to the compositor directly, and
//! nothing touches turning displays off.

use alpheus::app::worker::{Domain, Op, fetch, perform};
use alpheus::backends::{Backends, NodeRef, PowerAction};
use alpheus::displays::{Active, Mode, Plan, Scale, Step};
use alpheus::exec::fake::FakeExecutor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ALLOWED: [&str; 10] =
    ["wpctl", "brightnessctl", "nmcli", "bluetoothctl", "powerprofilesctl", "shell-ctl", "tools-menu", "hyprlock", "wm-set", "systemctl"];

#[test]
fn every_launched_command_is_on_the_allowlist() {
    let fake = Arc::new(FakeExecutor::new());
    let b = Backends::system(fake.clone(), std::env::temp_dir());
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
        // A plan stops at the first failure (here, every call fails): one
        // step per plan, so every command line gets checked.
        Op::ApplyDisplays {
            plan: Plan {
                steps: vec![Step {
                    output: "HDMI-A-1".into(),
                    active: Some(Active {
                        mode: Mode { width: 1920, height: 1080, refresh: 60 },
                        x: 0,
                        y: 0,
                        scale: Scale::ONE,
                        transform: 0,
                    }),
                }],
            },
            undo: None,
        },
        Op::ApplyDisplays { plan: Plan { steps: vec![Step { output: "eDP-1".into(), active: None }] }, undo: None },
    ];
    for op in ops.into_iter().chain(PowerAction::ALL.map(Op::Power)) {
        let _ = perform(&b, &op);
    }
    let calls = fake.calls();
    assert!(calls.len() >= 20, "{calls:?}");
    for c in &calls {
        let prog = c.split(' ').next().unwrap();
        assert!(ALLOWED.contains(&prog), "command off the list: {c}");
        assert!(!c.to_lowercase().contains("dpms"), "turning a display off is forbidden: {c}");
        if prog == "systemctl" {
            assert!(["systemctl suspend", "systemctl reboot", "systemctl poweroff"].contains(&c.as_str()), "{c}");
        }
        if prog == "wm-set" {
            // Leaving the session, reading and adjusting the displays: nothing else.
            let ok =
                c == "wm-set exit" || c == "wm-set outputs" || c.starts_with("wm-set monitor ") || c.starts_with("wm-set monitor-disable ");
            assert!(ok, "wm-set subcommand off the list: {c}");
        }
    }
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
        for word in ["dpms", "hyprctl", "niri msg", "swaymsg"] {
            assert!(!t.contains(word), "\"{word}\" in {}", f.display());
        }
    }
}
