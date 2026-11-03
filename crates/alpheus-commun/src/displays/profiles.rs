//! Display profiles: ~/.config/alpheus/displays.toml. Screens are
//! identified there by their identity (stable from one plug-in to
//! another, unlike the connector), with the connector kept as an optional
//! hint for identical screens. Absent file: no profile, no message;
//! invalid file: no profile + a warning (like config.toml).

use super::geometry::{Align, Placed, Placement, Relation, Screen, deduce, logical_size, resolve};
use super::{Active, Mode, Output, Plan, Scale, short_from_description, short_names};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileScreen {
    /// Identity (or an older compositor description): see `Output::is`.
    pub description: String,
    /// Connector hint, only to tell identical screens apart.
    pub connector: Option<String>,
    pub enabled: bool,
    /// Required for an active screen.
    pub mode: Option<Mode>,
    pub scale: Scale,
    /// Rotation 0-7 (`wl_output_transform`).
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connector: Option<String>,
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
                    Ok(ProfileScreen {
                        description: e.description,
                        connector: e.connector,
                        enabled: e.enabled,
                        mode,
                        scale,
                        transform: e.rotation,
                        placement,
                    })
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
                        connector: s.connector.clone(),
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
                connector: Some(o.name.clone()),
                enabled: true,
                mode: Some(a.mode),
                scale: a.scale,
                transform: a.transform,
                placement,
            },
            None => ProfileScreen {
                description: o.description.clone(),
                connector: Some(o.name.clone()),
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

/// How a profile's screens map onto the plugged-in outputs.
struct Assignment {
    /// One entry per profile screen: the output index, `None` when absent
    /// or when the screen is `tied`.
    found: Vec<Option<usize>>,
    /// The screen fits several outputs and its connector hint cannot choose
    /// (identical screens): it is satisfied, but never guessed.
    tied: Vec<bool>,
}

impl Assignment {
    fn has_tie(&self) -> bool {
        self.tied.contains(&true)
    }

    /// Every screen is plugged in (a tied one counts, its outputs being free).
    fn complete(&self) -> bool {
        self.found.iter().zip(&self.tied).all(|(f, t)| f.is_some() || *t)
    }
}

/// Resolves each screen to an output by `Output::is`; when several outputs
/// satisfy it (identical screens), the connector hint picks the one it
/// names, otherwise the screen is tied (never a guess), and the others keep
/// resolving. An output already taken is not offered again. Screens are
/// taken in file order, so a tie can persist where a later hint would have
/// settled it: conservative, never a swap.
fn assign(p: &Profile, outs: &[Output]) -> Assignment {
    let mut taken = vec![false; outs.len()];
    let mut found = Vec::with_capacity(p.screens.len());
    let mut tied = vec![false; p.screens.len()];
    for (k, s) in p.screens.iter().enumerate() {
        let fits: Vec<usize> = (0..outs.len()).filter(|&i| !taken[i] && outs[i].is(&s.description)).collect();
        let pick = match fits.as_slice() {
            [] => None,
            [one] => Some(*one),
            _ => {
                let hinted: Vec<usize> = fits.iter().copied().filter(|&i| s.connector.as_deref() == Some(outs[i].name.as_str())).collect();
                match hinted.as_slice() {
                    [one] => Some(*one),
                    _ => {
                        tied[k] = true;
                        None
                    }
                }
            }
        };
        if let Some(i) = pick {
            taken[i] = true;
        }
        found.push(pick);
    }
    // A tied screen whose outputs the later screens took is not satisfiable.
    for (k, s) in p.screens.iter().enumerate() {
        if tied[k] && !(0..outs.len()).any(|i| !taken[i] && outs[i].is(&s.description)) {
            tied[k] = false;
        }
    }
    Assignment { found, tied }
}

/// Result of looking for the profile that fits the plugged-in screens.
#[derive(Debug, PartialEq, Eq)]
pub enum ProfileMatch<'p> {
    Matches(&'p Profile),
    /// The first otherwise-matching profile has two screens that look the
    /// same and no hint to tell them apart: nothing is applied.
    Ambiguous,
    NoMatch,
}

/// The first profile (file order) whose screens map one to one onto `outs`.
pub fn match_profile<'p>(profiles: &'p [Profile], outs: &[Output]) -> ProfileMatch<'p> {
    for p in profiles {
        if p.screens.len() != outs.len() {
            continue;
        }
        let a = assign(p, outs);
        if a.complete() {
            return if a.has_tie() { ProfileMatch::Ambiguous } else { ProfileMatch::Matches(p) };
        }
    }
    ProfileMatch::NoMatch
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
    let assignment = assign(p, outs);
    if assignment.has_tie() {
        return Err("two screens look the same".into());
    }
    let found = assignment.found;
    let connected = |d: &str| outs.iter().any(|o| o.is(d));
    // A reference names an output by a profile string: the identity, an
    // older compositor description, even for an output outside the profile.
    // Several outputs can share it (twins): only the single profile screen
    // holding that string, through its hint, can say which.
    let reference_to = |r: &str| -> Result<String, String> {
        let fits: Vec<usize> = (0..outs.len()).filter(|&i| outs[i].is(r)).collect();
        match fits.as_slice() {
            [one] => Ok(format!("#{one}")),
            _ => {
                let named: Vec<usize> = (0..p.screens.len()).filter(|&k| p.screens[k].description == r).collect();
                match named.as_slice() {
                    [k] => found[*k].map(|i| format!("#{i}")).ok_or_else(|| "two screens look the same".to_string()),
                    _ => Err("two screens look the same".to_string()),
                }
            }
        }
    };
    let absent: Vec<String> =
        p.screens.iter().zip(&found).filter(|(_, f)| f.is_none()).map(|(s, _)| short_from_description(&s.description)).collect();
    // The profile screen standing for output `i`; its string is the
    // identifier placements refer to.
    let in_profile = |i: usize| found.iter().position(|f| *f == Some(i)).map(|k| &p.screens[k]);
    let mut has_origin = (0..outs.len()).filter_map(in_profile).any(|s| s.enabled && s.placement == Placement::Origin);
    let mut screens = Vec::new();
    // Target setting of each enabled output: mode, scale, rotation.
    let mut settings: Vec<Option<(Mode, Scale, u8)>> = Vec::new();
    for (i, (o, label)) in outs.iter().zip(&labels).enumerate() {
        // Distinct per output, so twins never share an identifier.
        let id = format!("#{i}");
        let off = Screen { id, label: label.clone(), enabled: false, size: (0, 0), placement: Placement::Origin };
        let (screen, setting) = match in_profile(i) {
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
                    } else if let Placement::Relative { relation, align, .. } = placement {
                        placement = Placement::Relative { relation, reference: reference_to(reference)?, align };
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
                connector: None,
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

    // ── Identity and matching ────────────────────────────────────────────

    fn screen(desc: &str, connector: Option<&str>, x: i32) -> ProfileScreen {
        ProfileScreen {
            description: desc.into(),
            connector: connector.map(Into::into),
            enabled: true,
            mode: Some(mode(1920, 1080, 60)),
            scale: Scale::ONE,
            transform: 0,
            placement: Placement::Absolute { x, y: 0 },
        }
    }

    fn profile(name: &str, screens: Vec<ProfileScreen>) -> Profile {
        Profile { name: name.into(), screens }
    }

    const OLD_DESCRIPTION: &str = "Acme Corporation ACM ALPHA-10";

    #[test]
    fn an_old_profile_matches_by_compositor_description() {
        let mut o = on("DP-1", "Acme Corporation ACM ALPHA-10 V1234", 1920, 1080, 0, 0);
        o.compositor_description = OLD_DESCRIPTION.into();
        let profiles = vec![profile("Old", vec![screen(OLD_DESCRIPTION, None, 0)])];
        assert!(matches!(match_profile(&profiles, std::slice::from_ref(&o)), ProfileMatch::Matches(p) if p.name == "Old"));
        // plan_profile resolves the same way: nothing absent.
        assert!(plan_profile(&profiles[0], &[o]).unwrap().absent.is_empty());
    }

    #[test]
    fn a_profile_matches_only_the_exact_set() {
        let profiles = vec![profile("Two", vec![screen(ALPHA, None, 0), screen(BRAVO, None, 1920)])];
        let a = on("DP-1", ALPHA, 1920, 1080, 0, 0);
        let b = on("DP-2", BRAVO, 1920, 1080, 1920, 0);
        let c = on("DP-3", "Charlie Modèle 3 SERIE0003", 1920, 1080, 3840, 0);
        assert_eq!(match_profile(&profiles, std::slice::from_ref(&a)), ProfileMatch::NoMatch);
        assert_eq!(match_profile(&profiles, &[a.clone(), b.clone(), c]), ProfileMatch::NoMatch);
        assert!(matches!(match_profile(&profiles, &[b, a]), ProfileMatch::Matches(_)), "another order");
    }

    #[test]
    fn first_matching_profile_wins() {
        let profiles = vec![
            profile("Other", vec![screen(BRAVO, None, 0)]),
            profile("First", vec![screen(ALPHA, None, 0)]),
            profile("Second", vec![screen(ALPHA, None, 0)]),
        ];
        let outs = [on("DP-1", ALPHA, 1920, 1080, 0, 0)];
        assert!(matches!(match_profile(&profiles, &outs), ProfileMatch::Matches(p) if p.name == "First"));
    }

    #[test]
    fn identical_screens_need_the_connector_hint() {
        let twin = "Acme Panel";
        let outs = [on("DP-1", twin, 1920, 1080, 0, 0), on("DP-2", twin, 1920, 1080, 1920, 0)];
        // Hints deliberately in the reverse of the outputs' order.
        let hinted = profile("Hinted", vec![screen(twin, Some("DP-2"), 0), screen(twin, Some("DP-1"), 1920)]);
        let plain = profile("Plain", vec![screen(twin, None, 0), screen(twin, None, 1920)]);
        assert!(matches!(match_profile(std::slice::from_ref(&hinted), &outs), ProfileMatch::Matches(_)));
        // Ambiguous stops the search: the later, matching profile is not tried.
        let later = profile("Later", vec![screen(twin, Some("DP-1"), 0), screen(twin, Some("DP-2"), 1920)]);
        assert_eq!(match_profile(&[plain.clone(), later], &outs), ProfileMatch::Ambiguous);
        let stale = profile("Stale", vec![screen(twin, Some("HDMI-9"), 0), screen(twin, Some("HDMI-8"), 1920)]);
        assert_eq!(match_profile(&[stale], &outs), ProfileMatch::Ambiguous, "hints naming nothing cannot decide");
        // plan_profile places each screen by its hint: DP-2 gets x = 0, DP-1 gets x = 1920.
        let plan = plan_profile(&hinted, &outs).unwrap().plan;
        let at = |name: &str| plan.steps.iter().find(|s| s.output == name).and_then(|s| s.active.as_ref()).map(|a| a.x);
        assert_eq!((at("DP-2"), at("DP-1")), (Some(0), Some(1920)));
        assert!(plan_profile(&plain, &outs).is_err(), "no guess without hints");
    }

    #[test]
    fn connector_is_optional_in_the_file() {
        let old = "[[profile]]\nname = \"X\"\n[[profile.display]]\ndescription = \"Alpha 1\"\nmode = \"1920x1080@60\"\n";
        assert_eq!(parse(old).unwrap()[0].screens[0].connector, None);
        let new = format!("{old}connector = \"DP-3\"\n");
        let parsed = parse(&new).unwrap();
        assert_eq!(parsed[0].screens[0].connector.as_deref(), Some("DP-3"));
        let path = dir("connector").below("displays.toml");
        save(&path, &parsed).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("connector = \"DP-3\""), "{text}");
        assert_eq!(load(&path).0, parsed);
        save(&path, &parse(old).unwrap()).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("connector"), "absent hint not written");
    }

    #[test]
    fn from_outputs_stores_identity_and_connector() {
        let mut o = on("DP-4", "Acme Panel S9", 1920, 1080, 0, 0);
        o.compositor_description = "Acme Panel".into();
        let p = from_outputs("Home", &[o, off("DP-5", "Acme Other S8")]);
        assert_eq!(p.screens[0].description, "Acme Panel S9", "identity, not the compositor description");
        assert_eq!(p.screens[0].connector.as_deref(), Some("DP-4"));
        assert_eq!(p.screens[1].connector.as_deref(), Some("DP-5"), "also for a disabled screen");
    }

    #[test]
    fn a_tie_only_ends_the_search_for_a_profile_that_otherwise_fits() {
        let twin = "Acme Panel";
        let outs =
            [on("DP-1", twin, 1920, 1080, 0, 0), on("DP-2", twin, 1920, 1080, 1920, 0), on("HDMI-1", "Acme TV", 1920, 1080, 3840, 0)];
        let laptop = profile("A", vec![screen(twin, None, 0), screen(twin, None, 1920), screen(INTERNAL, None, 3840)]);
        let tv = profile("B", vec![screen(twin, Some("DP-1"), 0), screen(twin, Some("DP-2"), 1920), screen("Acme TV", None, 3840)]);
        assert!(matches!(match_profile(&[laptop, tv], &outs), ProfileMatch::Matches(p) if p.name == "B"));
    }

    #[test]
    fn an_empty_key_matches_nothing() {
        let o = off("DP-1", "");
        assert!(!o.is(""));
    }

    #[test]
    fn a_reference_to_a_twin_needs_a_single_hinted_screen() {
        let twin = "Acme Panel";
        let outs =
            [on("DP-1", twin, 1920, 1080, 0, 0), on("DP-2", twin, 1920, 1080, 1920, 0), on("HDMI-1", "Acme TV", 1920, 1080, 3840, 0)];
        let mut tv = screen("Acme TV", None, 0);
        tv.placement = Placement::Relative { relation: Relation::RightOf, reference: twin.into(), align: Align::Start };
        // Both twins are in the profile: the reference cannot say which.
        let both = profile("Both", vec![screen(twin, Some("DP-1"), 0), screen(twin, Some("DP-2"), 1920), tv.clone()]);
        assert_eq!(plan_profile(&both, &outs).unwrap_err(), "two screens look the same");
        // Absolute twins still get distinct ids and their own places.
        let abs = profile("Abs", vec![screen(twin, Some("DP-1"), 0), screen(twin, Some("DP-2"), 1920), screen("Acme TV", None, 3840)]);
        assert!(plan_profile(&abs, &outs).is_ok());
        // One twin in the profile, with its hint: the reference resolves to it.
        let mut far = outs[0].clone();
        far.active.as_mut().unwrap().x = 5000;
        let outs2 = [far, outs[1].clone()];
        let mut other = screen("Acme Other", None, 0);
        other.placement = Placement::Relative { relation: Relation::RightOf, reference: twin.into(), align: Align::Start };
        let one = profile("One", vec![screen(twin, Some("DP-2"), 0), other]);
        let mut outs3 = outs2.to_vec();
        outs3.push(on("HDMI-2", "Acme Other", 1920, 1080, 0, 0));
        let plan = plan_profile(&one, &outs3).unwrap().plan;
        let at = |n: &str| plan.steps.iter().find(|s| s.output == n).and_then(|s| s.active.as_ref()).map(|a| a.x);
        assert_eq!(at("HDMI-2"), Some(1920), "right of DP-2 (the hinted twin)");
    }

    #[test]
    fn an_old_profile_places_by_old_descriptions() {
        let mut a = on("DP-1", "Acme Panel S1", 1920, 1080, 5, 5);
        a.compositor_description = "Acme Panel".into();
        let mut b = on("DP-2", "Acme Other S2", 1920, 1080, 9, 9);
        b.compositor_description = "Acme Other".into();
        let mut second = screen("Acme Other", None, 0);
        second.placement = Placement::Relative { relation: Relation::RightOf, reference: "Acme Panel".into(), align: Align::Start };
        let mut first = screen("Acme Panel", None, 0);
        first.placement = Placement::Origin;
        let p = profile("Old", vec![first, second]);
        let plan = plan_profile(&p, &[a, b]).unwrap().plan;
        let at = |n: &str| plan.steps.iter().find(|s| s.output == n).and_then(|s| s.active.as_ref()).map(|a| (a.x, a.y));
        assert_eq!((at("DP-1"), at("DP-2")), (Some((0, 0)), Some((1920, 0))));
    }

    #[test]
    fn a_reference_may_name_an_output_outside_the_profile_by_its_old_description() {
        let mut a = on("DP-1", "Acme Panel S1", 1920, 1080, 0, 0);
        a.compositor_description = "Acme Panel".into();
        let mut out = on("DP-2", "Acme Out S2", 1920, 1080, 0, 0);
        out.compositor_description = "Acme Out".into();
        let mut inside = screen("Acme Panel S1", None, 0);
        inside.placement = Placement::Relative { relation: Relation::RightOf, reference: "Acme Out".into(), align: Align::Start };
        let p = profile("P", vec![inside]);
        let plan = plan_profile(&p, &[a, out]).unwrap().plan;
        let at = |n: &str| plan.steps.iter().find(|s| s.output == n).and_then(|s| s.active.as_ref()).map(|a| a.x);
        assert_eq!(at("DP-1"), Some(1920), "right of the untouched out-of-profile screen at x = 0");
    }
}
