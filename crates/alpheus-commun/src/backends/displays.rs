//! Displays: read via `wm-set outputs` (normalised JSON), written via
//! `wm-set monitor` / `monitor-disable`. Never the compositor directly.

use super::{BResult, BackendError, run_ok, unparsable};
use crate::displays::{Active, Mode, Output, Plan, Scale, Step};
use crate::exec::Executor;
use serde::Deserialize;
use std::sync::Arc;

const TOOL: &str = "wm-set";

pub trait Displays: Send + Sync {
    /// Outputs known to the compositor, enabled or not.
    fn outputs(&self) -> BResult<Vec<Output>>;
    /// Runs the plan in order; stops at the first failure (a trial: never
    /// half-applied, the failure triggers the rollback at once).
    fn apply(&self, plan: &Plan) -> BResult<()>;
    /// Rollback: every activation, in order, even if a previous one failed
    /// (turn back on everything that can be); then the deactivations, only
    /// if every activation succeeded. The `Incomplete` error summarises the
    /// outputs that failed and those left on.
    fn apply_best_effort(&self, plan: &Plan) -> BResult<()>;
}

pub struct WmSetDisplays {
    exec: Arc<dyn Executor>,
}

impl WmSetDisplays {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        WmSetDisplays { exec }
    }
}

impl Displays for WmSetDisplays {
    fn outputs(&self) -> BResult<Vec<Output>> {
        parse_outputs(&run_ok(&*self.exec, TOOL, &["outputs"])?)
    }

    fn apply(&self, plan: &Plan) -> BResult<()> {
        plan.steps.iter().try_for_each(|s| self.run_step(s))
    }

    fn apply_best_effort(&self, plan: &Plan) -> BResult<()> {
        let (enables, disables): (Vec<&Step>, Vec<&Step>) = plan.steps.iter().partition(|s| s.active.is_some());
        let mut failures: Vec<(String, BackendError)> =
            enables.iter().filter_map(|s| self.run_step(s).err().map(|e| (s.output.clone(), e))).collect();
        let mut kept = Vec::new();
        if failures.is_empty() {
            failures.extend(disables.iter().filter_map(|s| self.run_step(s).err().map(|e| (s.output.clone(), e))));
        } else {
            // A power-on failed: nothing is turned off, whatever is on
            // stays on (Mae's decision: never go all black).
            kept = disables.iter().map(|s| s.output.clone()).collect();
        }
        if failures.is_empty() { Ok(()) } else { Err(BackendError::Incomplete { failures, kept }) }
    }
}

impl WmSetDisplays {
    fn run_step(&self, step: &Step) -> BResult<()> {
        let args = step.args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run_ok(&*self.exec, TOOL, &args).map(drop)
    }
}

/// An entry from `wm-set outputs` as written by the script.
#[derive(Deserialize)]
struct RawOutput {
    name: String,
    description: String,
    enabled: bool,
    mode: String,
    modes: Vec<String>,
    x: i32,
    y: i32,
    scale: f64,
    transform: u8,
    displaylink: bool,
}

/// Parses the JSON from `wm-set outputs`. An unreadable mode in the list is
/// ignored; the unreadable current mode of an ENABLED output makes the
/// whole thing unreadable (we never guess the state of a powered-on screen).
pub fn parse_outputs(json: &str) -> BResult<Vec<Output>> {
    let raw: Vec<RawOutput> = serde_json::from_str(json).map_err(|e| unparsable(TOOL, e.to_string()))?;
    raw.into_iter()
        .map(|r| {
            let active = if r.enabled {
                let mode = Mode::parse(&r.mode).ok_or_else(|| unparsable(TOOL, format!("mode `{}` for {}", r.mode, r.name)))?;
                let scale = Scale::from_f64(r.scale).ok_or_else(|| unparsable(TOOL, format!("scale {} for {}", r.scale, r.name)))?;
                if r.transform > 7 {
                    return Err(unparsable(TOOL, format!("rotation {} for {}", r.transform, r.name)));
                }
                Some(Active { mode, x: r.x, y: r.y, scale, transform: r.transform })
            } else {
                None
            };
            Ok(Output {
                modes: r.modes.iter().filter_map(|m| Mode::parse(m)).collect(),
                name: r.name,
                description: r.description,
                displaylink: r.displaylink,
                active,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::testing::*;
    use crate::exec::Output as ExecOutput;
    use crate::exec::fake::FakeExecutor;

    /// `wm-set outputs` readout from the machine, anonymised (descriptions replaced).
    const OUTPUTS: &str = include_str!("../../tests/fixtures/wm-set-outputs.json");

    #[test]
    fn parses_the_machines_readout() {
        let o = parse_outputs(OUTPUTS).unwrap();
        assert_eq!(o.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["eDP-1", "DVI-I-1", "HDMI-A-1"]);
        assert_eq!(o[1].description, ALPHA);
        assert!(o[1].displaylink && !o[0].displaylink && !o[2].displaylink);
        assert_eq!(o[1].active, Some(Active { mode: mode(1920, 1080, 60), x: 213, y: 0, scale: Scale::ONE, transform: 0 }));
        assert_eq!(o[2].active.as_ref().unwrap().mode, mode(1920, 1080, 240));
        assert_eq!(o[0].active.as_ref().map(|a| (a.x, a.y)), Some((0, 1080)));
        assert_eq!(o[1].modes.len(), 26);
        assert_eq!(&o[2].modes[..3], &[mode(1920, 1080, 60), mode(1920, 1080, 240), mode(1920, 1080, 120)]);
        // Same layout as the test helper `machine()`.
        assert_eq!(o.iter().map(|x| x.active.clone()).collect::<Vec<_>>(), machine().into_iter().map(|x| x.active).collect::<Vec<_>>());
    }

    #[test]
    fn disabled_output_and_unreadable_modes() {
        let j = r#"[{"name":"DP-3","description":"Charlie Modèle 3 SERIE0003","enabled":false,"mode":"0x0@0",
            "modes":["3840x2160@60","nope"],"x":0,"y":0,"scale":1.00,"transform":0,"displaylink":false}]"#;
        let o = parse_outputs(j).unwrap();
        assert_eq!(o[0].active, None);
        assert_eq!(o[0].modes, vec![mode(3840, 2160, 60)]);
    }

    #[test]
    fn unreadable_output() {
        let active_without_mode = r#"[{"name":"DP-3","description":"C","enabled":true,"mode":"0x0@0","modes":[],
            "x":0,"y":0,"scale":1,"transform":0,"displaylink":false}]"#;
        for j in ["", "{}", "[{\"name\": 3}]", active_without_mode] {
            assert!(matches!(parse_outputs(j), Err(BackendError::Unparsable { .. })), "{j}");
        }
    }

    #[test]
    fn reads_via_wm_set_outputs() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set outputs", OUTPUTS);
        let d = WmSetDisplays::new(f.clone());
        assert_eq!(d.outputs().unwrap().len(), 3);
        assert_eq!(f.calls(), vec!["wm-set outputs"]);
    }

    #[test]
    fn wm_set_missing_or_outdated() {
        let d = WmSetDisplays::new(Arc::new(FakeExecutor::new()));
        assert_eq!(d.outputs(), Err(BackendError::Missing { tool: "wm-set".into() }));
        let f = Arc::new(FakeExecutor::new());
        f.respond("wm-set outputs", Ok(ExecOutput::fail(1, "unknown subcommand: outputs\n")));
        let e = WmSetDisplays::new(f).outputs().unwrap_err();
        assert_eq!(e.to_string(), "wm-set failed (code 1): unknown subcommand: outputs");
    }

    fn plan3() -> Plan {
        let a = |x| Some(Active { mode: mode(1920, 1080, 60), x, y: 0, scale: Scale(1250), transform: 0 });
        Plan {
            steps: vec![
                Step { output: "DVI-I-1".into(), active: a(0) },
                Step { output: "HDMI-A-1".into(), active: a(1536) },
                Step { output: "eDP-1".into(), active: None },
            ],
        }
    }

    #[test]
    fn exact_command_lines() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor DVI-I-1 1920x1080@60 0x0 1.25 0", "");
        f.ok("wm-set monitor HDMI-A-1 1920x1080@60 1536x0 1.25 0", "");
        f.ok("wm-set monitor-disable eDP-1", "");
        WmSetDisplays::new(f.clone()).apply(&plan3()).unwrap();
        assert_eq!(
            f.calls(),
            vec![
                "wm-set monitor DVI-I-1 1920x1080@60 0x0 1.25 0",
                "wm-set monitor HDMI-A-1 1920x1080@60 1536x0 1.25 0",
                "wm-set monitor-disable eDP-1",
            ]
        );
    }

    #[test]
    fn stops_at_first_failure() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor DVI-I-1 1920x1080@60 0x0 1.25 0", "");
        f.respond("wm-set monitor HDMI-A-1 1920x1080@60 1536x0 1.25 0", Ok(ExecOutput::fail(1, "Hyprland refused `eval …`: error\n")));
        f.ok("wm-set monitor-disable eDP-1", "");
        let e = WmSetDisplays::new(f.clone()).apply(&plan3()).unwrap_err();
        assert_eq!(e.to_string(), "wm-set failed (code 1): Hyprland refused `eval …`: error");
        assert_eq!(f.calls().len(), 2, "the deactivation is never launched: {:?}", f.calls());
    }

    #[test]
    fn rollback_turns_nothing_off_if_a_power_on_failed() {
        // Before: Alpha (DisplayLink) on, internal panel off; the trial
        // swapped them. Rollback: turn Alpha back on, then turn the panel
        // off. If Alpha does not turn back on, the panel stays on (never go
        // all black).
        let mut alpha = on("DVI-I-1", ALPHA, 1920, 1080, 0, 0);
        alpha.displaylink = true;
        let before = vec![alpha, off("eDP-1", INTERNAL)];
        let trial = Plan {
            steps: vec![Step { output: "eDP-1".into(), active: before[0].active.clone() }, Step { output: "DVI-I-1".into(), active: None }],
        };
        let rollback = Plan::undo(&before, &trial);
        let f = Arc::new(FakeExecutor::new());
        f.respond("wm-set monitor DVI-I-1 1920x1080@60 0x0 1 0", Ok(ExecOutput::fail(1, "dock freeze\n")));
        f.ok("wm-set monitor-disable eDP-1", "");
        let e = WmSetDisplays::new(f.clone()).apply_best_effort(&rollback).unwrap_err();
        assert_eq!(f.calls(), vec!["wm-set monitor DVI-I-1 1920x1080@60 0x0 1 0"], "monitor-disable never launched");
        assert_eq!(
            e.to_string(),
            "incomplete rollback: DVI-I-1 (wm-set failed (code 1): dock freeze), eDP-1 (left on: incomplete rollback)"
        );

        // Same plan given out of order: the deactivation still waits.
        let shuffled = Plan { steps: vec![rollback.steps[1].clone(), rollback.steps[0].clone()] };
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable eDP-1", "");
        assert!(WmSetDisplays::new(f.clone()).apply_best_effort(&shuffled).is_err());
        assert_eq!(f.calls(), vec!["wm-set monitor DVI-I-1 1920x1080@60 0x0 1 0"]);

        // Power-on succeeds: the deactivation follows as before.
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor DVI-I-1 1920x1080@60 0x0 1 0", "");
        f.ok("wm-set monitor-disable eDP-1", "");
        assert_eq!(WmSetDisplays::new(f.clone()).apply_best_effort(&rollback), Ok(()));
        assert_eq!(f.calls(), vec!["wm-set monitor DVI-I-1 1920x1080@60 0x0 1 0", "wm-set monitor-disable eDP-1"]);
    }

    #[test]
    fn best_effort_rollback_internal_panel_first() {
        // Decision D2: a rollback applies every step even if another fails,
        // the internal panel first, the DisplayLink last.
        let rollback = Plan::undo(&machine(), &plan3());
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor eDP-1 2560x1600@60 0x1080 1 0", "");
        f.ok("wm-set monitor HDMI-A-1 1920x1080@240 2133x0 1 0", "");
        f.respond("wm-set monitor DVI-I-1 1920x1080@60 213x0 1 0", Ok(ExecOutput::fail(1, "dock freeze\n")));
        let e = WmSetDisplays::new(f.clone()).apply_best_effort(&rollback).unwrap_err();
        assert_eq!(
            f.calls(),
            vec![
                "wm-set monitor eDP-1 2560x1600@60 0x1080 1 0",
                "wm-set monitor HDMI-A-1 1920x1080@240 2133x0 1 0",
                "wm-set monitor DVI-I-1 1920x1080@60 213x0 1 0",
            ]
        );
        assert_eq!(e.to_string(), "incomplete rollback: DVI-I-1 (wm-set failed (code 1): dock freeze)");

        // Earlier failing steps do not stop the following ones.
        let f = Arc::new(FakeExecutor::new());
        f.respond("wm-set monitor eDP-1 2560x1600@60 0x1080 1 0", Ok(ExecOutput::fail(1, "refused\n")));
        f.ok("wm-set monitor DVI-I-1 1920x1080@60 213x0 1 0", "");
        // HDMI-A-1 not programmed: "wm-set not found".
        let e = WmSetDisplays::new(f.clone()).apply_best_effort(&rollback).unwrap_err();
        assert_eq!(f.calls().len(), 3, "every step attempted: {:?}", f.calls());
        assert_eq!(e.to_string(), "incomplete rollback: eDP-1 (wm-set failed (code 1): refused), HDMI-A-1 (wm-set not found)");

        // Everything goes through: Ok.
        let f = Arc::new(FakeExecutor::new());
        for c in ["eDP-1 2560x1600@60 0x1080", "HDMI-A-1 1920x1080@240 2133x0", "DVI-I-1 1920x1080@60 213x0"] {
            f.ok(&format!("wm-set monitor {c} 1 0"), "");
        }
        assert_eq!(WmSetDisplays::new(f.clone()).apply_best_effort(&rollback), Ok(()));
        assert_eq!(f.calls().len(), 3);
    }
}
