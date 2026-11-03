//! Displays: shared types (mode, scale, an output's setting) and
//! application plans. No call happens here: the daemon reads the outputs
//! and applies the changes; geometry and profiles compute them.

pub mod geometry;
pub mod profiles;
pub mod wire;

use std::fmt;

/// Display mode, refresh rate rounded to an integer as in `wm-set outputs`:
/// "1920x1080@60".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
}

impl Mode {
    /// "1920x1080@60"; `None` if unreadable or of zero size (a disabled
    /// output can report "0x0@0"). A refresh of 0 is one the compositor does
    /// not know (the protocol's 0 mHz: headless and virtual outputs).
    pub fn parse(s: &str) -> Option<Mode> {
        let (size, hz) = s.trim().split_once('@')?;
        let (w, h) = size.split_once('x')?;
        let m = Mode { width: w.parse().ok()?, height: h.parse().ok()?, refresh: hz.parse().ok()? };
        (m.width > 0 && m.height > 0).then_some(m)
    }

    /// Displayed label: "1920×1080@60".
    pub fn label(&self) -> String {
        format!("{}×{}@{}", self.width, self.height, self.refresh)
    }
}

/// Shape expected by `wm-set monitor`: "1920x1080@60".
impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.refresh)
    }
}

/// Scale in thousandths (1.25 → 1250): exactly comparable, no floating point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scale(pub u32);

impl Scale {
    pub const ONE: Scale = Scale(1000);
    /// Bounds and step of the settings panel: from 0.5 to 3 by 0.25.
    pub const MIN: Scale = Scale(500);
    pub const MAX: Scale = Scale(3000);
    pub const STEP: u32 = 250;

    /// From Hyprland's value (1.0, 1.25, 1.333333…).
    pub fn from_f64(v: f64) -> Option<Scale> {
        if !v.is_finite() || v <= 0.0 || v > 10.0 {
            return None;
        }
        let m = (v * 1000.0).round() as u32;
        (m > 0).then_some(Scale(m))
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 1000.0
    }

    /// `wm-set` argument: "1", "1.25", "1.2".
    pub fn arg(self) -> String {
        let (whole, frac) = (self.0 / 1000, self.0 % 1000);
        if frac == 0 { whole.to_string() } else { format!("{whole}.{frac:03}").trim_end_matches('0').to_string() }
    }

    /// Displayed label: "1.0", "1.25".
    pub fn label(self) -> String {
        if self.0 % 1000 == 0 { format!("{}.0", self.0 / 1000) } else { self.arg() }
    }

    /// Next step (`dir` > 0) or previous one, clamped to [0.5; 3]. A scale
    /// off the grid (1.2) first snaps to the grid (1.25 or 1.0).
    pub fn step(self, dir: i32) -> Scale {
        let s = self.0;
        let next = if dir > 0 {
            (s / Self::STEP + 1) * Self::STEP
        } else if s % Self::STEP == 0 {
            s.saturating_sub(Self::STEP)
        } else {
            s / Self::STEP * Self::STEP
        };
        Scale(next.clamp(Self::MIN.0, Self::MAX.0))
    }
}

/// Hyprland rotation (0-7, `wl_output_transform`): "0°", "90°"…; 4 to 7 are
/// the flipped variants.
pub fn rotation_label(t: u8) -> String {
    let deg = u32::from(t % 4) * 90;
    if t >= 4 { format!("flipped {deg}°") } else { format!("{deg}°") }
}

/// Setting of an enabled output (logical coordinates, as Hyprland reports them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub mode: Mode,
    pub x: i32,
    pub y: i32,
    pub scale: Scale,
    pub transform: u8,
}

/// An output known to the compositor (`wm-set outputs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Connector: "DVI-I-1" (can change from one plug-in to another).
    pub name: String,
    /// Stable identity: `make model serial` (empty parts left out), else the
    /// compositor's description. Profiles match screens on it.
    pub description: String,
    /// The head's own `description`, kept so profiles saved with it still
    /// match (empty when unknown).
    pub compositor_description: String,
    /// Real modes, in Hyprland's order.
    pub modes: Vec<Mode>,
    pub displaylink: bool,
    /// `None`: disabled output.
    pub active: Option<Active>,
}

impl Output {
    /// Whether a profile's stored string names this output: its identity or
    /// the compositor's description.
    pub fn is(&self, key: &str) -> bool {
        !key.is_empty() && (key == self.description || key == self.compositor_description)
    }
}

/// Short name drawn from a description: its first word (the brand).
pub fn short_from_description(desc: &str) -> String {
    desc.split_whitespace().next().unwrap_or("?").to_string()
}

fn short_name(o: &Output) -> String {
    if o.name.starts_with("eDP") {
        "Laptop".to_string()
    } else if o.description.trim().is_empty() {
        o.name.clone()
    } else {
        short_from_description(&o.description)
    }
}

/// Displayed short names, in the outputs' order: "Laptop" for an internal
/// panel (eDP-*), otherwise the brand. Two outputs with the same brand are
/// told apart by their connector ("Bravo HDMI-A-1").
pub fn short_names(outs: &[Output]) -> Vec<String> {
    let base: Vec<String> = outs.iter().map(short_name).collect();
    base.iter()
        .zip(outs)
        .map(|(b, o)| if base.iter().filter(|x| *x == b).count() > 1 { format!("{b} {}", o.name) } else { b.clone() })
        .collect()
}

/// Number of enabled outputs.
pub fn active_count(outs: &[Output]) -> usize {
    outs.iter().filter(|o| o.active.is_some()).count()
}

/// A `wm-set` call: set (`Some`) or disable (`None`) an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub output: String,
    pub active: Option<Active>,
}

impl Step {
    /// `wm-set` arguments: `monitor <output> <mode> <XxY> <scale>
    /// <rotation>` or `monitor-disable <output>`.
    pub fn args(&self) -> Vec<String> {
        match &self.active {
            Some(a) => vec![
                "monitor".into(),
                self.output.clone(),
                a.mode.to_string(),
                format!("{}x{}", a.x, a.y),
                a.scale.arg(),
                a.transform.to_string(),
            ],
            None => vec!["monitor-disable".into(), self.output.clone()],
        }
    }
}

/// Sequence of `wm-set` calls, run in order: stopped at the first failure
/// for a trial, best-effort for a rollback.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
}

/// Activations first, deactivations after: never a moment without a screen.
fn ordered(mut steps: Vec<Step>) -> Plan {
    steps.sort_by_key(|s| s.active.is_none()); // stable sort: order is kept
    Plan { steps }
}

/// Rank of an output in a rollback: internal panel, ordinary, DisplayLink.
fn undo_rank(o: &Output) -> u8 {
    if o.name.starts_with("eDP") {
        0
    } else if o.displaylink {
        2
    } else {
        1
    }
}

impl Plan {
    /// Steps to go from `current` to the `target` settings (by output
    /// name): only the outputs that change; an unknown output is ignored.
    pub fn between(current: &[Output], target: &[(String, Option<Active>)]) -> Plan {
        let steps = target
            .iter()
            .filter(|(name, t)| current.iter().any(|o| o.name == *name && o.active != *t))
            .map(|(name, t)| Step { output: name.clone(), active: t.clone() })
            .collect();
        ordered(steps)
    }

    /// Plan that gives back to the outputs touched by `plan` their setting from `before`.
    /// Order (decision D2): activations first, deactivations after; within
    /// each, the internal panel (eDP-*), then the ordinary outputs, then
    /// the DisplayLink ones — the safest output is turned back on first.
    pub fn undo(before: &[Output], plan: &Plan) -> Plan {
        let mut outs: Vec<&Output> = plan.steps.iter().filter_map(|s| before.iter().find(|o| o.name == s.output)).collect();
        outs.sort_by_key(|o| (o.active.is_none(), undo_rank(o))); // stable sort
        Plan { steps: outs.into_iter().map(|o| Step { output: o.name.clone(), active: o.active.clone() }).collect() }
    }

    pub fn touches(&self, output: &str) -> bool {
        self.steps.iter().any(|s| s.output == output)
    }

    /// This plan's steps whose output is still present in `now`, in the same order.
    pub fn restricted_to(&self, now: &[Output]) -> Plan {
        let steps = self.steps.iter().filter(|s| now.iter().any(|o| o.name == s.output)).cloned().collect();
        Plan { steps }
    }
}

/// Was a screen lost during a trial? Yes if an output touched by the plan
/// has disappeared, or if no output is active any more (decision D1): a
/// rollback is then needed for whatever remains.
pub fn screen_lost(plan: &Plan, now: &[Output]) -> bool {
    active_count(now) == 0 || plan.steps.iter().any(|s| !now.iter().any(|o| o.name == s.output))
}

/// Did the screens change outside alpheus during a trial? Yes if an output
/// appeared or disappeared (dock), or if an output the plan does not touch
/// changed setting (monitor-toggle…). Touched outputs do not count:
/// Hyprland can set them slightly differently from what was asked.
pub fn changed_elsewhere(before: &[Output], plan: &Plan, now: &[Output]) -> bool {
    let names = |v: &[Output]| {
        let mut n: Vec<String> = v.iter().map(|o| o.name.clone()).collect();
        n.sort();
        n
    };
    if names(before) != names(now) {
        return true;
    }
    now.iter().filter(|o| !plan.touches(&o.name)).any(|o| before.iter().any(|b| b.name == o.name && b.active != o.active))
}

/// Test helpers, shared with the TUI's tests (`testing` feature, enabled
/// only in its dev-dependencies).
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::*;

    pub fn mode(w: u32, h: u32, r: u32) -> Mode {
        Mode { width: w, height: h, refresh: r }
    }

    /// Enabled output, scale 1, no rotation; its mode is also the only one in its list.
    pub fn on(name: &str, desc: &str, w: u32, h: u32, x: i32, y: i32) -> Output {
        let m = mode(w, h, 60);
        Output {
            name: name.into(),
            description: desc.into(),
            compositor_description: String::new(),
            modes: vec![m],
            displaylink: false,
            active: Some(Active { mode: m, x, y, scale: Scale::ONE, transform: 0 }),
        }
    }

    pub fn off(name: &str, desc: &str) -> Output {
        Output {
            name: name.into(),
            description: desc.into(),
            compositor_description: String::new(),
            modes: vec![mode(1920, 1080, 60)],
            displaylink: false,
            active: None,
        }
    }

    pub const INTERNAL: &str = "Interne Dalle 0x0001";
    pub const ALPHA: &str = "Alpha Modèle 1 SERIE0001";
    pub const BRAVO: &str = "Bravo Modèle 2 SERIE0002";

    /// Real layout of the machine (anonymised readout): Alpha (DisplayLink)
    /// at the top at 213, Bravo (240 Hz) to its right, the internal panel
    /// below, at x = 0.
    pub fn machine() -> Vec<Output> {
        let mut alpha = on("DVI-I-1", ALPHA, 1920, 1080, 213, 0);
        alpha.displaylink = true;
        let mut bravo = on("HDMI-A-1", BRAVO, 1920, 1080, 2133, 0);
        bravo.modes = vec![mode(1920, 1080, 60), mode(1920, 1080, 240)];
        if let Some(a) = &mut bravo.active {
            a.mode = mode(1920, 1080, 240);
        }
        vec![on("eDP-1", INTERNAL, 2560, 1600, 0, 1080), alpha, bravo]
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(Mode::parse("1920x1080@60"), Some(mode(1920, 1080, 60)));
        assert_eq!(Mode::parse(" 2560x1600@60\n"), Some(mode(2560, 1600, 60)));
        assert_eq!(Mode::parse("0x0@0"), None, "disabled output");
        assert_eq!(Mode::parse("1280x720@0"), Some(mode(1280, 720, 0)), "a refresh the compositor does not know");
        assert_eq!(Mode::parse("1920x1080"), None);
        assert_eq!(Mode::parse("1920x1080@59.93"), None, "already rounded by wm-set");
        assert_eq!(mode(1920, 1080, 60).to_string(), "1920x1080@60");
        assert_eq!(mode(1920, 1080, 60).label(), "1920×1080@60");
    }

    #[test]
    fn scales() {
        assert_eq!(Scale::from_f64(1.0), Some(Scale::ONE));
        assert_eq!(Scale::from_f64(1.333333), Some(Scale(1333)));
        assert_eq!(Scale::from_f64(0.0), None);
        assert_eq!(Scale::from_f64(f64::NAN), None);
        assert_eq!(Scale(1000).arg(), "1");
        assert_eq!(Scale(1250).arg(), "1.25");
        assert_eq!(Scale(1200).arg(), "1.2");
        assert_eq!(Scale(1000).label(), "1.0");
        assert_eq!(Scale(1500).label(), "1.5");
    }

    #[test]
    fn scale_steps_clamped_and_snapped_to_the_grid() {
        assert_eq!(Scale(1000).step(1), Scale(1250));
        assert_eq!(Scale(1000).step(-1), Scale(750));
        assert_eq!(Scale(1200).step(1), Scale(1250), "off the grid: to the next notch");
        assert_eq!(Scale(1200).step(-1), Scale(1000), "off the grid: to the previous notch");
        assert_eq!(Scale(500).step(-1), Scale(500), "floor 0.5");
        assert_eq!(Scale(3000).step(1), Scale(3000), "ceiling 3");
    }

    #[test]
    fn rotations() {
        assert_eq!(rotation_label(0), "0°");
        assert_eq!(rotation_label(3), "270°");
        assert_eq!(rotation_label(5), "flipped 90°");
    }

    #[test]
    fn short_names_test() {
        assert_eq!(short_names(&machine()), vec!["Laptop", "Alpha", "Bravo"]);
        let two = vec![
            on("DVI-I-1", "Bravo Modèle 2 SERIE0002", 1920, 1080, 0, 0),
            on("DVI-I-2", "Bravo Modèle 2 SERIE0003", 1920, 1080, 1920, 0),
        ];
        assert_eq!(short_names(&two), vec!["Bravo DVI-I-1", "Bravo DVI-I-2"], "same brand: connector appended");
        assert_eq!(short_names(&[on("HDMI-A-2", "  ", 1920, 1080, 0, 0)]), vec!["HDMI-A-2"], "without a description");
        assert_eq!(short_from_description(BRAVO), "Bravo");
    }

    #[test]
    fn wm_set_arguments() {
        let a = Active { mode: mode(1920, 1080, 60), x: 213, y: 0, scale: Scale(1250), transform: 1 };
        let s = Step { output: "DVI-I-1".into(), active: Some(a) };
        assert_eq!(s.args(), vec!["monitor", "DVI-I-1", "1920x1080@60", "213x0", "1.25", "1"]);
        let s = Step { output: "eDP-1".into(), active: None };
        assert_eq!(s.args(), vec!["monitor-disable", "eDP-1"]);
    }

    #[test]
    fn plan_between_two_states_activations_first() {
        let cur = machine();
        let moved = Active { x: 0, ..cur[1].active.clone().unwrap() };
        let target = vec![
            ("eDP-1".to_string(), None),
            ("DVI-I-1".to_string(), Some(moved.clone())),
            ("HDMI-A-1".to_string(), cur[2].active.clone()), // unchanged: absent from the plan
            ("DP-9".to_string(), None),                      // unknown: ignored
        ];
        let p = Plan::between(&cur, &target);
        assert_eq!(p.steps, vec![Step { output: "DVI-I-1".into(), active: Some(moved) }, Step { output: "eDP-1".into(), active: None },]);
        assert!(p.touches("eDP-1") && !p.touches("HDMI-A-1"));
    }

    #[test]
    fn reverse_plan() {
        let before = machine();
        let p = Plan { steps: vec![Step { output: "HDMI-A-1".into(), active: None }, Step { output: "eDP-1".into(), active: None }] };
        let u = Plan::undo(&before, &p);
        assert_eq!(
            u.steps,
            vec![
                Step { output: "eDP-1".into(), active: before[0].active.clone() },
                Step { output: "HDMI-A-1".into(), active: before[2].active.clone() },
            ],
            "internal panel first"
        );
        // An output off before the trial is turned off again, after the activations.
        let before = vec![off("DP-3", ALPHA), on("eDP-1", INTERNAL, 2560, 1600, 0, 0)];
        let p = Plan {
            steps: vec![Step { output: "DP-3".into(), active: before[1].active.clone() }, Step { output: "eDP-1".into(), active: None }],
        };
        let u = Plan::undo(&before, &p);
        assert_eq!(u.steps[0].output, "eDP-1");
        assert_eq!(u.steps[1], Step { output: "DP-3".into(), active: None });
    }

    #[test]
    fn reverse_plan_internal_panel_then_others_then_displaylink() {
        // Decision D2: the rollback first turns on the internal panel (the
        // safest), then the ordinary outputs, then the DisplayLink ones.
        let before = machine(); // eDP-1, DVI-I-1 (DisplayLink), HDMI-A-1
        let off_step = |n: &str| Step { output: n.into(), active: None };
        let p = Plan { steps: vec![off_step("DVI-I-1"), off_step("HDMI-A-1"), off_step("eDP-1")] };
        let u = Plan::undo(&before, &p);
        assert_eq!(u.steps.iter().map(|s| s.output.as_str()).collect::<Vec<_>>(), vec!["eDP-1", "HDMI-A-1", "DVI-I-1"]);
        // A deactivation stays after every activation, even that of the
        // internal panel: never a moment without a screen.
        let mut dl = on("DVI-I-1", ALPHA, 1920, 1080, 0, 0);
        dl.displaylink = true;
        let before = vec![dl, off("eDP-1", INTERNAL)];
        let p = Plan { steps: vec![Step { output: "eDP-1".into(), active: before[0].active.clone() }, off_step("DVI-I-1")] };
        let u = Plan::undo(&before, &p);
        assert_eq!(u.steps, vec![Step { output: "DVI-I-1".into(), active: before[0].active.clone() }, off_step("eDP-1"),]);
    }

    #[test]
    fn external_change() {
        let before = machine();
        let plan = Plan { steps: vec![Step { output: "eDP-1".into(), active: None }] };
        let mut now = before.clone();
        now[0].active = None; // expected effect of the plan
        assert!(!changed_elsewhere(&before, &plan, &now));
        now[0].active = Some(Active { scale: Scale(1250), ..before[0].active.clone().unwrap() });
        assert!(!changed_elsewhere(&before, &plan, &now), "touched output: set differently, not \"elsewhere\"");
        let mut now2 = now.clone();
        now2[1].active = None; // monitor-toggle turned Alpha off
        assert!(changed_elsewhere(&before, &plan, &now2));
        let mut now3 = now.clone();
        now3.pop(); // Bravo unplugged
        assert!(changed_elsewhere(&before, &plan, &now3));
        let mut now4 = now.clone();
        now4.push(off("DP-3", "Charlie Modèle 3 SERIE0003")); // dock plugged in
        assert!(changed_elsewhere(&before, &plan, &now4));
    }

    #[test]
    fn screen_lost_during_a_trial() {
        let before = machine();
        let plan = Plan {
            steps: vec![Step { output: "DVI-I-1".into(), active: None }, Step { output: "eDP-1".into(), active: before[0].active.clone() }],
        };
        assert!(!screen_lost(&plan, &before));
        let mut without_alpha = before.clone();
        without_alpha.remove(1);
        assert!(screen_lost(&plan, &without_alpha), "touched output disappeared");
        assert_eq!(plan.restricted_to(&without_alpha).steps, vec![plan.steps[1].clone()]);
        let mut without_bravo = before.clone();
        without_bravo.remove(2);
        assert!(!screen_lost(&plan, &without_bravo), "untouched output: not \"lost\"");
        let off: Vec<Output> = before.iter().map(|o| Output { active: None, ..o.clone() }).collect();
        assert!(screen_lost(&plan, &off), "no output active any more");
    }

    #[test]
    fn is_matches_identity_or_compositor_description() {
        let mut o = on("DP-1", "Acme Panel S1", 1920, 1080, 0, 0);
        assert!(o.is("Acme Panel S1") && !o.is("Acme Panel"));
        assert!(!o.is(""), "an empty compositor description matches nothing");
        o.compositor_description = "Acme Panel".into();
        assert!(o.is("Acme Panel S1") && o.is("Acme Panel") && !o.is("Other"));
    }

    #[test]
    fn active_screen_count() {
        let mut v = machine();
        assert_eq!(active_count(&v), 3);
        v[0].active = None;
        assert_eq!(active_count(&v), 2);
    }
}
