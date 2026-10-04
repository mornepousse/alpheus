//! Garde-fous du cadrage, vérifiés mécaniquement : seules les commandes
//! prévues sont lancées ; rien ne parle au compositeur en direct ni ne
//! touche à l'extinction des écrans.

use alpheus::app::worker::{fetch, perform, Domain, Op};
use alpheus::backends::{Backends, NodeRef, PowerAction};
use alpheus::displays::{Active, Mode, Plan, Scale, Step};
use alpheus::exec::fake::FakeExecutor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const AUTORISES: [&str; 10] = [
    "wpctl", "brightnessctl", "nmcli", "bluetoothctl", "powerprofilesctl",
    "shell-ctl", "tools-menu", "hyprlock", "wm-set", "systemctl",
];

#[test]
fn chaque_commande_lancee_est_dans_la_liste_blanche() {
    let fake = Arc::new(FakeExecutor::new());
    let b = Backends::system(fake.clone(), std::env::temp_dir());
    for d in Domain::ALL {
        let _ = fetch(&b, d);
    }
    let ops = [
        Op::SetVolume(NodeRef::DefaultSink, 50), Op::SetVolume(NodeRef::Id(64), 50), Op::SetDefault(64),
        Op::SetBrightness(50), Op::SetWifi(true), Op::SetWifi(false), Op::SetBluetooth(true),
        Op::SetBluetooth(false), Op::SetDnd(true), Op::SetDnd(false), Op::ToggleKeepAwake { now_on: true },
        Op::SetProfile("balanced".into()),
        // Un plan s'arrête au premier échec (ici, chaque appel échoue) : une
        // étape par plan, pour que chaque ligne de commande soit vérifiée.
        Op::ApplyDisplays {
            plan: Plan { steps: vec![Step {
                output: "HDMI-A-1".into(),
                active: Some(Active { mode: Mode { width: 1920, height: 1080, refresh: 60 }, x: 0, y: 0, scale: Scale::ONE, transform: 0 }),
            }] },
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
        assert!(AUTORISES.contains(&prog), "commande hors liste : {c}");
        assert!(!c.to_lowercase().contains("dpms"), "extinction d'écran interdite : {c}");
        if prog == "systemctl" {
            assert!(["systemctl suspend", "systemctl reboot", "systemctl poweroff"].contains(&c.as_str()), "{c}");
        }
        if prog == "wm-set" {
            // Quitter la session, lire et régler les écrans : rien d'autre.
            let ok = c == "wm-set exit"
                || c == "wm-set outputs"
                || c.starts_with("wm-set monitor ")
                || c.starts_with("wm-set monitor-disable ");
            assert!(ok, "sous-commande wm-set hors liste : {c}");
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
fn le_source_ne_parle_jamais_au_compositeur_ni_aux_ecrans() {
    let mut files = Vec::new();
    rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(files.len() > 10);
    for f in files {
        let t = std::fs::read_to_string(&f).unwrap().to_lowercase();
        for mot in ["dpms", "hyprctl", "niri msg", "swaymsg"] {
            assert!(!t.contains(mot), "« {mot} » dans {}", f.display());
        }
    }
}
