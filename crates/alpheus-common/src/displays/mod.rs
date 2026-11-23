//! Displays: shared types (mode, scale, an output's setting) and
//! application plans. No call happens here: the daemon reads the outputs
//! and applies the changes; geometry and profiles compute them.
//!
//! Never:
//! - call anything: the daemon reads the screens and applies a configuration, and these only
//!   compute (`the_daemon_only_reads_from_the_compositor` holds the one place that writes);
//! - identify a screen by its connector: it is matched by its identity, which survives a replug
//!   (docs/decisions.md, "A DisplayLink screen is told from the DRM directory, once per screen
//!   name").
//!
//! See: docs/features/displays.md, docs/features/control-center.md (the Displays tab).

pub mod geometry;
pub mod profiles;
pub mod wire;

use std::fmt;

/// Display mode, refresh rate rounded to the hertz: "1920x1080@60".
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

    /// Displayed label: "1920×1080@60"; "1280×720" for a refresh the
    /// compositor does not know.
    pub fn label(&self) -> String {
        if self.refresh == 0 {
            format!("{}×{}", self.width, self.height)
        } else {
            format!("{}×{}@{}", self.width, self.height, self.refresh)
        }
    }
}

/// The wire's and the profiles' shape: "1920x1080@60".
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

    /// Displayed label: "1.0", "1.25", "1.2".
    pub fn label(self) -> String {
        let (whole, frac) = (self.0 / 1000, self.0 % 1000);
        if frac == 0 { format!("{whole}.0") } else { format!("{whole}.{frac:03}").trim_end_matches('0').to_string() }
    }

    /// Next step (`dir` > 0) or previous one, clamped to [0.5; 3]. A scale
    /// off the grid (1.2) first snaps to the grid (1.25 or 1.0).
    pub fn step(self, dir: i32) -> Scale {
        let s = self.0;
        let next = if dir > 0 {
            (s / Self::STEP + 1) * Self::STEP
        } else if s.is_multiple_of(Self::STEP) {
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

/// An output known to the compositor (the daemon's snapshot).
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

/// One output's change: set (`Some`) or disable (`None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub output: String,
    pub active: Option<Active>,
}

/// A change of the outputs, described step by step (previews, the Keep
/// box's list of what changes); the daemon applies it as one configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
}

/// Activations first, deactivations after, as the Keep box lists them.
fn ordered(mut steps: Vec<Step>) -> Plan {
    steps.sort_by_key(|s| s.active.is_none()); // stable sort: order is kept
    Plan { steps }
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

    pub fn touches(&self, output: &str) -> bool {
        self.steps.iter().any(|s| s.output == output)
    }
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
        assert_eq!(Mode::parse("1920x1080@59.93"), None, "already rounded to the hertz");
        assert_eq!(mode(1920, 1080, 60).to_string(), "1920x1080@60");
        assert_eq!(mode(1920, 1080, 60).label(), "1920×1080@60");
        assert_eq!(mode(1280, 720, 0).to_string(), "1280x720@0", "the wire keeps the unknown refresh");
        assert_eq!(mode(1280, 720, 0).label(), "1280×720", "shown without a rate");
    }

    #[test]
    fn scales() {
        assert_eq!(Scale::from_f64(1.0), Some(Scale::ONE));
        assert_eq!(Scale::from_f64(1.333333), Some(Scale(1333)));
        assert_eq!(Scale::from_f64(0.0), None);
        assert_eq!(Scale::from_f64(f64::NAN), None);
        assert_eq!(Scale(1250).label(), "1.25");
        assert_eq!(Scale(1200).label(), "1.2");
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
