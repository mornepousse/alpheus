//! Display profiles: ~/.config/alpheus/displays.toml. Screens are
//! identified there by their description (stable from one plug-in to
//! another, unlike the connector). Absent file: no profile, no message;
//! invalid file: no profile + a warning (like config.toml).

use super::geometry::{Align, Placed, Placement, Relation, Screen, deduce, logical_size, resolve};
use super::{Active, Mode, Output, Plan, Scale, short_from_description, short_names};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileScreen {
    pub description: String,
    pub enabled: bool,
    /// Required for an active screen.
    pub mode: Option<Mode>,
    pub scale: Scale,
    /// Hyprland rotation 0-7 (as in `wm-set monitor`).
    pub transform: u8,
    pub placement: Placement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub screens: Vec<ProfileScreen>,
}

// ── File format ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileToml {
    #[serde(default)]
    profile: Vec<ProfileToml>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileToml {
    name: String,
    #[serde(default)]
    display: Vec<DisplayToml>,
}

fn default_true() -> bool {
    true
}
fn default_scale() -> f64 {
    1.0
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayToml {
    description: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(default = "default_scale")]
    scale: f64,
    #[serde(default)]
    rotation: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    placement: Option<PlacementToml>,
}

/// `{ relation, reference, align }` or `{ x, y }` (absolute coordinates).
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PlacementToml {
    Relative(RelativeToml),
    Absolute(AbsoluteToml),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelativeToml {
    relation: Relation,
    reference: String,
    #[serde(default)]
    align: Align,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsoluteToml {
    x: i32,
    y: i32,
}

fn from_toml(f: FileToml) -> Result<Vec<Profile>, String> {
    f.profile
        .into_iter()
        .map(|p| {
            let screens = p
                .display
                .into_iter()
                .map(|e| {
                    let ctx = |m: String| format!("profile `{}`, {}: {m}", p.name, short_from_description(&e.description));
                    let mode = match &e.mode {
                        None => None,
                        Some(s) => Some(Mode::parse(s).ok_or_else(|| ctx(format!("unreadable mode `{s}`")))?),
                    };
                    if e.enabled && mode.is_none() {
                        return Err(ctx("missing mode".into()));
                    }
                    let scale = Scale::from_f64(e.scale).ok_or_else(|| ctx(format!("invalid scale {}", e.scale)))?;
                    if e.rotation > 7 {
                        return Err(ctx(format!("invalid rotation {} (0 to 7)", e.rotation)));
                    }
                    let placement = match e.placement {
                        None => Placement::Origin,
                        Some(PlacementToml::Relative(r)) => {
                            Placement::Relative { relation: r.relation, reference: r.reference, align: r.align }
                        }
                        Some(PlacementToml::Absolute(a)) => Placement::Absolute { x: a.x, y: a.y },
                    };
                    Ok(ProfileScreen { description: e.description, enabled: e.enabled, mode, scale, transform: e.rotation, placement })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Profile { name: p.name, screens })
        })
        .collect()
}

fn to_toml(profiles: &[Profile]) -> FileToml {
    FileToml {
        profile: profiles
            .iter()
            .map(|p| ProfileToml {
                name: p.name.clone(),
                display: p
                    .screens
                    .iter()
                    .map(|s| DisplayToml {
                        description: s.description.clone(),
                        enabled: s.enabled,
                        mode: s.mode.map(|m| m.to_string()),
                        scale: s.scale.as_f64(),
                        rotation: s.transform,
                        placement: match &s.placement {
                            Placement::Origin => None,
                            Placement::Relative { relation, reference, align } => Some(PlacementToml::Relative(RelativeToml {
                                relation: *relation,
                                reference: reference.clone(),
                                align: *align,
                            })),
                            Placement::Absolute { x, y } => Some(PlacementToml::Absolute(AbsoluteToml { x: *x, y: *y })),
                        },
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Parses the content of displays.toml; the error is the reason.
pub fn parse(text: &str) -> Result<Vec<Profile>, String> {
    toml::from_str::<FileToml>(text).map_err(|e| e.message().to_string()).and_then(from_toml)
}

/// Absent → no profile, no message; unreadable or invalid → no profile +
/// a message for the status line.
pub fn load(path: &Path) -> (Vec<Profile>, Option<String>) {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(e) => (Vec::new(), Some(format!("displays.toml unreadable: {e}"))),
        Ok(s) => match parse(&s) {
            Ok(p) => (p, None),
            Err(e) => (Vec::new(), Some(format!("invalid displays.toml: {e}"))),
        },
    }
}

/// Writes every profile (directory created if needed; file replaced in one
/// go, never half-written).
pub fn save(path: &Path, profiles: &[Profile]) -> Result<(), String> {
    let body = toml::to_string(&to_toml(profiles)).map_err(|e| e.to_string())?;
    let text = format!("# alpheus's display profiles (Displays tab), editable by hand.\n\n{body}");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Screens as laid out by Hyprland's report (logical size), for inference.
pub fn placed(outs: &[Output]) -> Vec<Placed> {
    outs.iter()
        .map(|o| match &o.active {
            Some(a) => {
                Placed { id: o.description.clone(), enabled: true, x: a.x, y: a.y, size: logical_size(a.mode, a.scale, a.transform) }
            }
            None => Placed { id: o.description.clone(), enabled: false, x: 0, y: 0, size: (0, 0) },
        })
        .collect()
}

/// "Save the current layout": a profile that reproduces the current state.
pub fn from_outputs(name: &str, outs: &[Output]) -> Profile {
    let placements = deduce(&placed(outs), None, None);
    let screens = outs
        .iter()
        .zip(placements)
        .map(|(o, placement)| match &o.active {
            Some(a) => ProfileScreen {
                description: o.description.clone(),
                enabled: true,
                mode: Some(a.mode),
                scale: a.scale,
                transform: a.transform,
                placement,
            },
            None => ProfileScreen {
                description: o.description.clone(),
                enabled: false,
                mode: None,
                scale: Scale::ONE,
                transform: 0,
                placement: Placement::Origin,
            },
        })
        .collect();
    Profile { name: name.to_string(), screens }
}

/// A profile's plan, and the profile's screens that are not plugged in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePlan {
    pub plan: Plan,
    /// Short names of the profile's missing screens ("Bravo"), ignored.
    pub absent: Vec<String>,
}

/// Computes the application of a profile to the plugged-in outputs. A
/// profile screen that is not plugged in is ignored (and reported); a
/// plugged-in output missing from the profile stays as it is. A placement
/// whose reference is not plugged in becomes the origin if no other one
/// exists, otherwise the profile is refused. Also refused if it would
/// leave no screen active, or if the geometry refuses.
pub fn plan_profile(p: &Profile, outs: &[Output]) -> Result<ProfilePlan, String> {
    let labels = short_names(outs);
    let connected = |d: &str| outs.iter().any(|o| o.description == d);
    let absent: Vec<String> =
        p.screens.iter().filter(|s| !connected(&s.description)).map(|s| short_from_description(&s.description)).collect();
    let in_profile = |o: &Output| p.screens.iter().find(|s| s.description == o.description);
    let mut has_origin = outs.iter().filter_map(in_profile).any(|s| s.enabled && s.placement == Placement::Origin);
    let mut screens = Vec::new();
    // Target setting of each enabled output: mode, scale, rotation.
    let mut settings: Vec<Option<(Mode, Scale, u8)>> = Vec::new();
    for (o, label) in outs.iter().zip(&labels) {
        let off = Screen { id: o.description.clone(), label: label.clone(), enabled: false, size: (0, 0), placement: Placement::Origin };
        let (screen, setting) = match in_profile(o) {
            Some(s) if s.enabled => {
                let mode = s.mode.ok_or_else(|| format!("{label}: missing mode"))?;
                let mut placement = s.placement.clone();
                if let Placement::Relative { reference, .. } = &placement {
                    if !connected(reference) {
                        if has_origin {
                            return Err(format!("{label} is placed relative to {}, which is missing", short_from_description(reference)));
                        }
                        placement = Placement::Origin;
                        has_origin = true;
                    }
                }
                let size = logical_size(mode, s.scale, s.transform);
                (Screen { enabled: true, size, placement, ..off }, Some((mode, s.scale, s.transform)))
            }
            Some(_) => (off, None),
            // Plugged in but missing from the profile: stays where it is.
            None => match &o.active {
                Some(a) => {
                    let size = logical_size(a.mode, a.scale, a.transform);
                    let placement = Placement::Absolute { x: a.x, y: a.y };
                    (Screen { enabled: true, size, placement, ..off }, Some((a.mode, a.scale, a.transform)))
                }
                None => (off, None),
            },
        };
        screens.push(screen);
        settings.push(setting);
    }
    if !screens.iter().any(|s| s.enabled) {
        return Err("this profile would turn off the last active screen".into());
    }
    let pos = resolve(&screens).map_err(|e| e.to_string())?;
    let target: Vec<(String, Option<Active>)> = outs
        .iter()
        .zip(pos.into_iter().zip(settings))
        .map(|(o, (pos, setting))| {
            let active = match (pos, setting) {
                (Some((x, y)), Some((mode, scale, transform))) => Some(Active { mode, x, y, scale, transform }),
                _ => None,
            };
            (o.name.clone(), active)
        })
        .collect();
    Ok(ProfilePlan { plan: Plan::between(outs, &target), absent })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::Step;
    use crate::displays::testing::*;
    use crate::temp_path::TempPath;

    fn dir(name: &str) -> TempPath {
        TempPath::new(&format!("displays-{name}"))
    }

    const OFFICE: &str = r#"
[[profile]]
name = "Bureau"
[[profile.display]]
description = "Alpha Modèle 1 SERIE0001"
mode = "1920x1080@60"
[[profile.display]]
description = "Bravo Modèle 2 SERIE0002"
enabled = true
mode = "1920x1080@60"
scale = 1.0
rotation = 0
placement = { relation = "right_of", reference = "Alpha Modèle 1 SERIE0001", align = "start" }
[[profile.display]]
description = "Interne Dalle 0x0001"
enabled = false

[[profile]]
name = "Portable seul"
[[profile.display]]
description = "Interne Dalle 0x0001"
mode = "2560x1600@60"
scale = 1.25
placement = { x = 0, y = 0 }
"#;

    fn write(name: &str, content: &str) -> TempPath {
        let d = dir(name);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.below("displays.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    fn office() -> Profile {
        parse(OFFICE).unwrap().remove(0)
    }

    #[test]
    fn reads() {
        let (p, w) = load(&write("reads", OFFICE));
        assert_eq!(w, None);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].name, "Bureau");
        assert_eq!(
            p[0].screens[0],
            ProfileScreen {
                description: ALPHA.into(),
                enabled: true,
                mode: Some(mode(1920, 1080, 60)),
                scale: Scale::ONE,
                transform: 0,
                placement: Placement::Origin,
            }
        );
        assert_eq!(
            p[0].screens[1].placement,
            Placement::Relative { relation: Relation::RightOf, reference: ALPHA.into(), align: Align::Start }
        );
        assert!(!p[0].screens[2].enabled);
        assert_eq!(p[1].screens[0].scale, Scale(1250));
        assert_eq!(p[1].screens[0].placement, Placement::Absolute { x: 0, y: 0 }, "absolute form accepted");
    }

    #[test]
    fn absent_with_no_message() {
        assert_eq!(load(Path::new("/nonexistent/alpheus/displays.toml")), (Vec::new(), None));
    }

    #[test]
    fn invalid_with_message() {
        for (name, content, expected) in [
            ("syntax", "[[profile]\nname = 3", "invalid displays.toml"),
            ("field", "[[profile]]\nname = \"X\"\ncolor = 1\n", "color"),
            (
                "mode",
                "[[profile]]\nname = \"X\"\n[[profile.display]]\ndescription = \"Alpha 1\"\nmode = \"1080p\"\n",
                "unreadable mode `1080p`",
            ),
            ("missing-mode", "[[profile]]\nname = \"X\"\n[[profile.display]]\ndescription = \"Alpha 1\"\n", "Alpha: missing mode"),
            (
                "rotation",
                "[[profile]]\nname = \"X\"\n[[profile.display]]\ndescription = \"Alpha 1\"\nmode = \"1920x1080@60\"\nrotation = 90\n",
                "invalid rotation 90",
            ),
            (
                "relation",
                "[[profile]]\nname = \"X\"\n[[profile.display]]\ndescription = \"A\"\nmode = \"1920x1080@60\"\nplacement = { relation = \"in-front-of\", reference = \"B\" }\n",
                "invalid displays.toml",
            ),
        ] {
            let (p, w) = load(&write(name, content));
            assert!(p.is_empty(), "{name}");
            let w = w.unwrap_or_default();
            assert!(w.starts_with("invalid displays.toml") && w.contains(expected), "{name}: {w}");
        }
    }

    #[test]
    fn write_then_reread() {
        let path = dir("write").below("subdir").below("displays.toml");
        let (profiles, _) = load(&write("source", OFFICE));
        save(&path, &profiles).unwrap();
        let (reread, w) = load(&path);
        assert_eq!(w, None);
        assert_eq!(reread, profiles);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# alpheus's display profiles"), "{text}");
        assert!(text.contains("name = \"Bureau\"") && text.contains("relation = \"right_of\""), "{text}");
        assert!(!path.with_extension("toml.tmp").exists(), "no temporary file left behind");
    }

    #[test]
    fn save_current_deduces_the_placements() {
        let mut outs = machine();
        outs.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
        let p = from_outputs("Home", &outs);
        assert_eq!(p.name, "Home");
        assert_eq!(p.screens.len(), 4);
        assert_eq!(p.screens[0].placement, Placement::Absolute { x: -213, y: 1080 }, "not reproducible: absolute");
        assert_eq!(p.screens[1].placement, Placement::Origin);
        assert_eq!(
            p.screens[2].placement,
            Placement::Relative { relation: Relation::RightOf, reference: ALPHA.into(), align: Align::Start }
        );
        assert_eq!((p.screens[3].enabled, p.screens[3].mode), (false, None));
        // Applied again as-is: nothing to do.
        assert_eq!(plan_profile(&p, &outs).unwrap().plan, Plan::default());
    }

    #[test]
    fn profile_applied() {
        let outs = machine();
        let pp = plan_profile(&office(), &outs).unwrap();
        assert!(pp.absent.is_empty());
        // Alpha at the origin (0, 0), Bravo to its right, the panel off last.
        assert_eq!(
            pp.plan.steps,
            vec![
                Step {
                    output: "DVI-I-1".into(),
                    active: Some(Active { mode: mode(1920, 1080, 60), x: 0, y: 0, scale: Scale::ONE, transform: 0 })
                },
                Step {
                    output: "HDMI-A-1".into(),
                    active: Some(Active { mode: mode(1920, 1080, 60), x: 1920, y: 0, scale: Scale::ONE, transform: 0 })
                },
                Step { output: "eDP-1".into(), active: None },
            ]
        );
    }

    #[test]
    fn incomplete_profile_missing_screen_ignored() {
        // Bravo unplugged; a plugged-in output outside the profile stays as it is.
        let outs = vec![machine()[0].clone(), machine()[1].clone(), on("DP-3", "Charlie Modèle 3 SERIE0003", 1280, 1024, 5000, 0)];
        let pp = plan_profile(&office(), &outs).unwrap();
        assert_eq!(pp.absent, vec!["Bravo"]);
        assert!(!pp.plan.touches("DP-3"), "outside the profile: unchanged");
        assert!(pp.plan.touches("DVI-I-1") && pp.plan.touches("eDP-1"));
    }

    #[test]
    fn missing_reference_becomes_the_origin_if_possible() {
        // Only Bravo is plugged in (with the panel): its reference Alpha is
        // missing, no other active origin → Bravo becomes the origin.
        let outs = vec![machine()[0].clone(), machine()[2].clone()];
        let pp = plan_profile(&office(), &outs).unwrap();
        assert_eq!(pp.absent, vec!["Alpha"]);
        assert_eq!(
            pp.plan.steps[0],
            Step {
                output: "HDMI-A-1".into(),
                active: Some(Active { mode: mode(1920, 1080, 60), x: 0, y: 0, scale: Scale::ONE, transform: 0 })
            }
        );
        // Another origin already exists: refused with the reason.
        let mut p = office();
        p.screens[1].placement =
            Placement::Relative { relation: Relation::Below, reference: "Charlie Modèle 3 SERIE0003".into(), align: Align::Start };
        assert_eq!(plan_profile(&p, &machine()).unwrap_err(), "Bravo is placed relative to Charlie, which is missing");
    }

    #[test]
    fn profile_that_would_turn_everything_off_is_refused() {
        // Office far from the dock: Alpha and Bravo absent, the panel off.
        let outs = vec![machine()[0].clone()];
        assert_eq!(plan_profile(&office(), &outs).unwrap_err(), "this profile would turn off the last active screen");
    }

    #[test]
    fn geometry_refusal() {
        let mut p = office();
        p.screens[1].placement = Placement::Absolute { x: 100, y: 0 };
        assert_eq!(plan_profile(&p, &machine()).unwrap_err(), "Alpha and Bravo overlap");
    }

    #[test]
    fn rounded_refresh_rate_nothing_to_do() {
        // Hyprland reports 59.934 Hz, read as "@60": the 60 Hz profile is already in place.
        let outs = vec![on("DVI-I-1", ALPHA, 1920, 1080, 0, 0), on("HDMI-A-1", BRAVO, 1920, 1080, 1920, 0), off("eDP-1", INTERNAL)];
        assert_eq!(plan_profile(&office(), &outs).unwrap().plan, Plan::default());
    }
}
