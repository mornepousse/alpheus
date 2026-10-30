//! Displays tab: profiles, toggling an output, the settings panel, saving
//! the current state, and the "Keep these settings?" trial with automatic
//! rollback. Every setting goes through a trial: the plan is sent to the
//! worker thread with its reverse plan, a 15 s countdown once applied.

use super::worker::Op;
use super::{App, Overlay};
use crate::backends::{BResult, BackendError};
use crate::displays::geometry::{Align, GeoError, Placement, Relation, Screen, deduce, logical_size, resolve};
use crate::displays::profiles::{self, Profile, from_outputs, placed, plan_profile};
use crate::displays::{
    Active, Mode, Output, Plan, Scale, Step, active_count, changed_elsewhere, rotation_label, screen_lost, short_from_description,
    short_names,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Delay before automatically rolling back to the previous settings.
pub const KEEP_DELAY: Duration = Duration::from_secs(15);

/// Tab state outside the data read.
#[derive(Debug, Default)]
pub struct DisplaysTab {
    pub profiles: Vec<Profile>,
    /// displays.toml; `None`: saving is impossible (tests).
    pub profiles_path: Option<PathBuf>,
    /// The file loaded at startup was invalid (warning kept): saving would
    /// overwrite it without having been able to read it, refused until Mae
    /// has fixed it (set by `build_app`).
    pub profiles_invalid: bool,
    /// Chip selected in the profile bar; `profiles.len()` designates
    /// "+ Save current".
    pub profile_sel: usize,
    pub trial: Option<Trial>,
    /// Rollback sent and still unanswered: to redo if alpheus closes.
    pub undoing: Option<Plan>,
    /// Profile requested from the palette: applied on the next read.
    pub queued_profile: Option<String>,
    /// A plan has answered and no fresh read has arrived yet: the known
    /// outputs may be those of a rejected trial, so nothing is planned on
    /// top of them.
    pub awaiting_read: bool,
    /// The compositor's name when the tab calls nothing there (Sway, niri: `wm-set outputs`
    /// refuses); None: through `wm-set`.
    pub unavailable: Option<&'static str>,
}

/// A trial in progress: applied (or being applied), not kept yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trial {
    pub title: String,
    pub plan: Plan,
    pub undo: Plan,
    /// Outputs read just before (to detect an external change).
    pub before: Vec<Output>,
    pub summary: Vec<String>,
    /// "Bravo missing, ignored".
    pub notes: Vec<String>,
    /// The plan touches a DisplayLink output.
    pub displaylink: bool,
    /// End of the countdown; `None` until the plan has answered.
    pub deadline: Option<Instant>,
    /// Remaining seconds shown.
    pub remaining: u64,
}

/// "Keep these settings?" dialog; focus starts on Cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Keep {
    pub keep_focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelField {
    Mode,
    Scale,
    Rotation,
    Place,
    Reference,
    Align,
    Cancel,
    Apply,
}

/// Placement chosen in the panel. `Free`: current position, not
/// reproducible through a relation (offered only in that case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceChoice {
    Free,
    Origin,
    Rel(Relation),
}

/// Settings panel for an output: draft sent by "Apply".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    /// Connector of the output being adjusted.
    pub output: String,
    pub field: PanelField,
    pub mode: Mode,
    pub scale: Scale,
    pub transform: u8,
    pub place: PlaceChoice,
    /// Description of the reference screen.
    pub reference: Option<String>,
    pub align: Align,
    /// Current position (offset from the inferred origin).
    pub free: Option<(i32, i32)>,
    /// Mode search in progress ("/").
    pub query: Option<String>,
}

/// "Save current": name entry, then confirmation if it already exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SaveDialog {
    pub name: String,
    pub replace: bool,
    pub yes_focused: bool,
}

pub fn place_label(p: PlaceChoice) -> &'static str {
    match p {
        PlaceChoice::Free => "current position",
        PlaceChoice::Origin => "origin",
        PlaceChoice::Rel(r) => r.label(),
    }
}

/// Visible panel fields, in order.
pub fn panel_fields(p: &Panel) -> Vec<PanelField> {
    let mut v = vec![PanelField::Mode, PanelField::Scale, PanelField::Rotation, PanelField::Place];
    if matches!(p.place, PlaceChoice::Rel(_)) {
        v.extend([PanelField::Reference, PanelField::Align]);
    }
    v.extend([PanelField::Cancel, PanelField::Apply]);
    v
}

fn label_of(outs: &[Output], labels: &[String], desc: &str) -> String {
    outs.iter().position(|o| o.description == desc).map_or_else(|| short_from_description(desc), |i| labels[i].clone())
}

/// Each output's placement, in plain words, for the list: "origin", "right
/// of Alpha", "below Alpha, centered", "position 0, 1080".
pub fn placement_labels(outs: &[Output]) -> Vec<String> {
    let labels = short_names(outs);
    deduce(&placed(outs), None, None)
        .into_iter()
        .zip(outs)
        .map(|(p, o)| match (&o.active, p) {
            (None, _) => String::new(),
            (Some(_), Placement::Origin) => "origin".into(),
            (Some(_), Placement::Relative { relation, reference, align }) => {
                let mut s = format!("{} {}", relation.label(), label_of(outs, &labels, &reference));
                if align != Align::Start {
                    s.push_str(&format!(", {}", align.label(relation)));
                }
                s
            }
            (Some(a), Placement::Absolute { .. }) => format!("position {}, {}", a.x, a.y),
        })
        .collect()
}

/// Rightmost screen (largest right edge) among the enabled ones.
fn rightmost(outs: &[Output], except: usize) -> Option<usize> {
    outs.iter()
        .enumerate()
        .filter(|(j, _)| *j != except)
        .filter_map(|(j, o)| o.active.as_ref().map(|a| (j, a.x + logical_size(a.mode, a.scale, a.transform).0)))
        .max_by_key(|&(_, right)| right)
        .map(|(j, _)| j)
}

/// Screens for the geometry engine: each at its current (absolute) place.
fn screens_as_is(outs: &[Output], labels: &[String]) -> Vec<Screen> {
    outs.iter()
        .zip(labels)
        .map(|(o, l)| match &o.active {
            Some(a) => Screen {
                id: o.description.clone(),
                label: l.clone(),
                enabled: true,
                size: logical_size(a.mode, a.scale, a.transform),
                placement: Placement::Absolute { x: a.x, y: a.y },
            },
            None => Screen { id: o.description.clone(), label: l.clone(), enabled: false, size: (0, 0), placement: Placement::Origin },
        })
        .collect()
}

/// Targeted settings: output `i` receives `own`; the other enabled ones
/// keep their mode, scale and rotation, at their new position.
fn targets(outs: &[Output], pos: Vec<Option<(i32, i32)>>, i: usize, own: (Mode, Scale, u8)) -> Vec<(String, Option<Active>)> {
    outs.iter()
        .enumerate()
        .zip(pos)
        .map(|((j, o), pos)| {
            let active = match pos {
                Some((x, y)) if j == i => Some(Active { mode: own.0, x, y, scale: own.1, transform: own.2 }),
                Some((x, y)) => o.active.clone().map(|a| Active { x, y, ..a }),
                None => None,
            };
            (o.name.clone(), active)
        })
        .collect()
}

/// Enable output `i`: its first mode (Hyprland's preferred one), scale 1,
/// to the right of the rightmost screen, aligned at the top; the others
/// don't move.
fn enable_plan(outs: &[Output], i: usize) -> Result<Plan, String> {
    let mode = outs[i].modes.first().copied().ok_or("no known mode")?;
    let labels = short_names(outs);
    let mut screens = screens_as_is(outs, &labels);
    screens[i].enabled = true;
    screens[i].size = logical_size(mode, Scale::ONE, 0);
    screens[i].placement = match rightmost(outs, i) {
        Some(j) => Placement::Relative { relation: Relation::RightOf, reference: outs[j].description.clone(), align: Align::Start },
        None => Placement::Origin,
    };
    let pos = resolve(&screens).map_err(|e| e.to_string())?;
    let pos = kept_in_place(outs, &screens, pos, i);
    Ok(Plan::between(outs, &targets(outs, pos, i, (mode, Scale::ONE, 0))))
}

/// Is `j` placed, step by step, against screen `i`?
fn follows(screens: &[Screen], mut j: usize, i: usize) -> bool {
    for _ in 0..screens.len() {
        if j == i {
            return true;
        }
        let Placement::Relative { reference, .. } = &screens[j].placement else { return false };
        let Some(r) = screens.iter().position(|s| s.id == *reference) else { return false };
        j = r;
    }
    false
}

/// `resolve` re-anchors the whole set at (0, 0); on a live screen, we
/// translate back so nothing moves without reason: an enabled output that
/// wasn't adjusted, and that doesn't follow the adjusted output `i`, keeps
/// its current position; if there is none, the adjusted output keeps its
/// own. Then never a negative coordinate.
fn kept_in_place(outs: &[Output], screens: &[Screen], mut pos: Vec<Option<(i32, i32)>>, i: usize) -> Vec<Option<(i32, i32)>> {
    let now_at = |j: usize| outs[j].active.as_ref().map(|a| (a.x, a.y)).zip(pos[j]);
    let anchor = (0..outs.len()).filter(|&j| j != i && !follows(screens, j, i)).find_map(now_at).or_else(|| now_at(i));
    if let Some(((cx, cy), (nx, ny))) = anchor {
        for p in pos.iter_mut().flatten() {
            *p = (p.0 + cx - nx, p.1 + cy - ny);
        }
    }
    // Never a negative coordinate: only on the axis that overflows does the
    // whole set slide so the minimum is 0 (a screen placed left of or above
    // everything else legitimately pushes the others).
    let min_x = pos.iter().flatten().map(|p| p.0).min().unwrap_or(0).min(0);
    let min_y = pos.iter().flatten().map(|p| p.1).min().unwrap_or(0).min(0);
    for p in pos.iter_mut().flatten() {
        *p = (p.0 - min_x, p.1 - min_y);
    }
    pos
}

/// Line of a trial's summary.
fn describe_step(s: &Step, outs: &[Output], labels: &[String]) -> String {
    let label = outs.iter().position(|o| o.name == s.output).map_or_else(|| s.output.clone(), |i| labels[i].clone());
    match &s.active {
        None => format!("{label}: off"),
        Some(a) => {
            format!("{label}: {}, scale {}, {}, position {}, {}", a.mode.label(), a.scale.label(), rotation_label(a.transform), a.x, a.y)
        }
    }
}

/// Rollback after a lost screen: the reverse plan restricted to the outputs
/// still present, without any shutdown if the result would leave nothing on.
fn lost_undo(undo: &Plan, now: &[Output]) -> Plan {
    let mut plan = undo.restricted_to(now);
    let lit_after = now
        .iter()
        .filter(|o| match plan.steps.iter().find(|s| s.output == o.name) {
            Some(s) => s.active.is_some(),
            None => o.active.is_some(),
        })
        .count();
    if lit_after == 0 {
        plan.steps.retain(|s| s.active.is_some());
    }
    plan
}

fn wrap(idx: usize, dir: i32, len: usize) -> usize {
    (idx as i32 + dir).rem_euclid(len.max(1) as i32) as usize
}

impl App {
    /// Outputs read so far (empty until something is read).
    pub fn outputs(&self) -> &[Output] {
        self.data.displays.ready().map_or(&[], Vec::as_slice)
    }

    /// Plan to undo if alpheus closes right now (trial or rollback in progress).
    pub fn exit_undo(&self) -> Option<Plan> {
        self.displays.trial.as_ref().map(|t| t.undo.clone()).or_else(|| self.displays.undoing.clone())
    }

    /// Is a display change already in progress? (Warns with a message.)
    fn displays_busy(&mut self, now: Instant) -> bool {
        let msg = if self.displays.trial.is_some() || self.displays.undoing.is_some() {
            "Displays: a change is already in progress"
        } else if self.displays.awaiting_read {
            "Displays: waiting for a fresh read"
        } else {
            return false;
        };
        self.toast(msg.into(), true, now);
        true
    }

    fn close_keep(&mut self) {
        if matches!(self.overlay, Overlay::Keep(_)) {
            self.overlay = Overlay::None;
        }
    }

    // ── Responses and time ───────────────────────────────────────────

    /// After reading the outputs: a lost screen or an external change
    /// during a trial, a profile requested from the palette, the panel of
    /// an unplugged output.
    pub(crate) fn displays_read(&mut self, now: Instant) {
        let outs = self.outputs().to_vec();
        if self.data.displays.ready().is_none() {
            // Failed read: a queued profile won't be applied much later.
            self.displays.queued_profile = None;
            return;
        }
        // Read that comes after any plan response (reads in flight during a
        // plan are discarded): a reliable base going forward.
        self.displays.awaiting_read = false;
        if let Some(t) = &self.displays.trial {
            if t.deadline.is_some() && screen_lost(&t.plan, &outs) {
                // Decision D1: a screen from the trial has disappeared, or
                // nothing is on any more: roll back on the outputs still
                // present.
                let undo = lost_undo(&t.undo, &outs);
                self.displays.trial = None;
                self.close_keep();
                let msg = if undo.steps.is_empty() {
                    "Screen lost during the trial: nothing to restore"
                } else {
                    "Screen lost during the trial: rolling back to the previous state"
                };
                self.status.error = Some(msg.to_string());
                self.toast(msg.to_string(), true, now);
                if !undo.steps.is_empty() {
                    self.start_undo(undo);
                }
            } else if t.deadline.is_some() && changed_elsewhere(&t.before, &t.plan, &outs) {
                self.displays.trial = None;
                self.close_keep();
                self.status.last = Some("Displays changed elsewhere".into());
                self.toast("Displays changed elsewhere".into(), false, now);
            }
        }
        if let Overlay::Panel(p) = &self.overlay {
            if !outs.iter().any(|o| o.name == p.output) {
                let msg = format!("Displays: {} unplugged, setting abandoned", p.output);
                self.overlay = Overlay::None;
                self.toast(msg, true, now);
            }
        }
        self.displays.profile_sel = self.displays.profile_sel.min(self.displays.profiles.len());
        if let Some(name) = self.displays.queued_profile.take() {
            self.apply_display_profile(&name, now);
        }
    }

    /// Worker thread's response to a display plan.
    pub(crate) fn displays_done(&mut self, op: Op, r: BResult<()>, now: Instant) {
        let Op::ApplyDisplays { plan, undo } = op else { return };
        self.displays.awaiting_read = true;
        match (undo, r) {
            (Some(_), Ok(())) => {
                if let Some(t) = &mut self.displays.trial {
                    if t.plan == plan && t.deadline.is_none() {
                        t.deadline = Some(now + KEEP_DELAY);
                        t.remaining = KEEP_DELAY.as_secs();
                    }
                }
                self.refresh_tab(now);
            }
            (Some(u), Err(e)) => {
                // Never a half-applied state: roll back immediately.
                if self.displays.trial.as_ref().is_some_and(|t| t.plan == plan) {
                    self.displays.trial = None;
                    self.close_keep();
                }
                let msg = format!("Displays: {e} — rolling back to the previous settings");
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
                self.start_undo(u);
            }
            (None, Ok(())) => {
                self.displays.undoing = None;
                self.status.last = Some("Displays: previous settings restored".into());
                self.toast("Displays: previous settings restored".into(), false, now);
                self.refresh_tab(now);
            }
            (None, Err(e)) => {
                self.displays.undoing = None;
                let msg = match e {
                    BackendError::Incomplete { .. } => format!("Displays: {e}"),
                    e => format!("Displays: rollback impossible: {e}"),
                };
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
                self.refresh_tab(now);
            }
        }
    }

    /// Countdown: remaining seconds, rollback at the deadline.
    pub(crate) fn displays_tick(&mut self, now: Instant) {
        let Some(t) = &mut self.displays.trial else { return };
        let Some(deadline) = t.deadline else { return };
        if now < deadline {
            t.remaining = (deadline - now).as_millis().div_ceil(1000) as u64;
            return;
        }
        if let Some(t) = self.displays.trial.take() {
            self.close_keep();
            self.toast("Displays: time's up, rolling back to the previous settings".into(), false, now);
            self.start_undo(t.undo);
        }
    }

    // ── Trial ────────────────────────────────────────────────────────

    /// Sends `plan` with its reverse and opens "Keep these settings?".
    pub(crate) fn start_trial(&mut self, title: String, plan: Plan, notes: Vec<String>, now: Instant) {
        if plan.steps.is_empty() {
            self.toast(format!("{title}: nothing to change"), false, now);
            return;
        }
        let Some(before) = self.data.displays.ready().cloned() else {
            self.toast("Displays: unknown state".into(), true, now);
            return;
        };
        let labels = short_names(&before);
        let summary = plan.steps.iter().map(|s| describe_step(s, &before, &labels)).collect();
        let displaylink = plan.steps.iter().any(|s| before.iter().any(|o| o.name == s.output && o.displaylink));
        let undo = Plan::undo(&before, &plan);
        self.displays.trial = Some(Trial {
            title,
            plan: plan.clone(),
            undo: undo.clone(),
            before,
            summary,
            notes,
            displaylink,
            deadline: None,
            remaining: KEEP_DELAY.as_secs(),
        });
        self.overlay = Overlay::Keep(Keep::default());
        self.send_op(Op::ApplyDisplays { plan, undo: Some(undo) });
    }

    fn start_undo(&mut self, plan: Plan) {
        self.displays.undoing = Some(plan.clone());
        self.send_op(Op::ApplyDisplays { plan, undo: None });
    }

    /// Keep (only once applied) or Cancel (roll back).
    pub(crate) fn keep_answer(&mut self, keep: bool, now: Instant) {
        if keep {
            if self.displays.trial.as_ref().is_some_and(|t| t.deadline.is_some()) {
                self.displays.trial = None;
                self.close_keep();
                self.status.last = Some("Displays: settings kept".into());
                self.toast("Displays: settings kept".into(), false, now);
            }
        } else if let Some(t) = self.displays.trial.take() {
            self.close_keep();
            self.start_undo(t.undo);
        }
    }

    // ── Profiles and list ────────────────────────────────────────────

    pub(crate) fn apply_display_profile(&mut self, name: &str, now: Instant) {
        if self.displays.unavailable.is_some() || self.displays_busy(now) {
            return;
        }
        let Some(outs) = self.data.displays.ready() else {
            self.toast("Displays: unknown state".into(), true, now);
            return;
        };
        let Some(p) = self.displays.profiles.iter().find(|p| p.name == name) else {
            self.toast(format!("Display profile \"{name}\" not found"), true, now);
            return;
        };
        match plan_profile(p, outs) {
            Err(e) => {
                let msg = format!("Profile \"{name}\" refused: {e}");
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
            }
            Ok(pp) => {
                let notes = pp.absent.iter().map(|a| format!("{a} missing, ignored")).collect();
                self.start_trial(format!("Profile \"{name}\""), pp.plan, notes, now);
            }
        }
    }

    /// Space on an output: enable it, or disable it (never the last one).
    pub(crate) fn toggle_output(&mut self, name: &str, now: Instant) {
        if self.displays.unavailable.is_some() || self.displays_busy(now) {
            return;
        }
        let outs = self.outputs().to_vec();
        let Some(i) = outs.iter().position(|o| o.name == name) else { return };
        let label = short_names(&outs)[i].clone();
        if outs[i].active.is_some() {
            if active_count(&outs) <= 1 {
                let msg = format!("Can't disable {label}: last active screen");
                self.toast(msg, true, now);
                return;
            }
            let plan = Plan { steps: vec![Step { output: name.to_string(), active: None }] };
            self.start_trial(format!("{label} disabled"), plan, Vec::new(), now);
        } else {
            match enable_plan(&outs, i) {
                Ok(plan) => self.start_trial(format!("{label} enabled"), plan, Vec::new(), now),
                Err(e) => self.toast(format!("{label}: {e}"), true, now),
            }
        }
    }

    pub(crate) fn profile_bar_move(&mut self, dir: i32) {
        let n = self.displays.profiles.len() as i32;
        self.displays.profile_sel = (self.displays.profile_sel as i32 + dir).clamp(0, n) as usize;
    }

    /// Enter on the profile bar: apply, or "+ Save current".
    pub(crate) fn profile_bar_activate(&mut self, now: Instant) {
        match self.displays.profiles.get(self.displays.profile_sel).map(|p| p.name.clone()) {
            Some(name) => self.apply_display_profile(&name, now),
            None => self.open_save(now),
        }
    }

    // ── Settings panel ───────────────────────────────────────────────

    pub(crate) fn open_panel(&mut self, name: &str, now: Instant) {
        if self.displays_busy(now) {
            return;
        }
        let outs = self.outputs().to_vec();
        let Some(i) = outs.iter().position(|o| o.name == name) else { return };
        let o = &outs[i];
        let (mode, scale, transform) = match (&o.active, o.modes.first()) {
            (Some(a), _) => (a.mode, a.scale, a.transform),
            (None, Some(m)) => (*m, Scale::ONE, 0),
            (None, None) => {
                let msg = format!("{}: no known mode", short_names(&outs)[i]);
                self.toast(msg, true, now);
                return;
            }
        };
        let mut p = Panel {
            output: name.to_string(),
            field: PanelField::Mode,
            mode,
            scale,
            transform,
            place: PlaceChoice::Origin,
            reference: None,
            align: Align::Start,
            free: None,
            query: None,
        };
        if o.active.is_some() {
            match &deduce(&placed(&outs), None, None)[i] {
                Placement::Origin => {}
                Placement::Relative { relation, reference, align } => {
                    p.place = PlaceChoice::Rel(*relation);
                    p.reference = Some(reference.clone());
                    p.align = *align;
                }
                Placement::Absolute { x, y } => {
                    p.place = PlaceChoice::Free;
                    p.free = Some((*x, *y));
                }
            }
        } else if let Some(j) = rightmost(&outs, i) {
            // Off: offered to the right of the rightmost screen.
            p.place = PlaceChoice::Rel(Relation::RightOf);
            p.reference = Some(outs[j].description.clone());
        }
        self.overlay = Overlay::Panel(p);
    }

    /// Offered modes: the output's own, filtered by the search.
    pub fn panel_modes(&self, p: &Panel) -> Vec<Mode> {
        let Some(o) = self.outputs().iter().find(|o| o.name == p.output) else { return Vec::new() };
        let mut modes = o.modes.clone();
        if !modes.contains(&p.mode) {
            modes.insert(0, p.mode);
        }
        // Substring match, not fuzzy search: "1280" must not find "1920x1080".
        let q = p.query.as_deref().unwrap_or("").trim().replace('×', "x");
        modes.into_iter().filter(|m| m.to_string().contains(&q)).collect()
    }

    /// Possible references: the other enabled screens (descriptions).
    pub fn panel_references(&self, p: &Panel) -> Vec<String> {
        self.outputs().iter().filter(|o| o.name != p.output && o.active.is_some()).map(|o| o.description.clone()).collect()
    }

    /// The draft's plan, or the reason it's refused (Apply greyed out).
    pub fn panel_preview(&self, p: &Panel) -> Result<Plan, String> {
        let outs = self.outputs();
        let i = outs.iter().position(|o| o.name == p.output).ok_or("output unplugged")?;
        let labels = short_names(outs);
        let current = placed(outs);
        let own = match p.place {
            PlaceChoice::Origin => Placement::Origin,
            PlaceChoice::Free => {
                let (x, y) = p.free.ok_or("unknown position")?;
                Placement::Absolute { x, y }
            }
            PlaceChoice::Rel(relation) => {
                Placement::Relative { relation, reference: p.reference.clone().ok_or("choose a reference screen")?, align: p.align }
            }
        };
        let size = logical_size(p.mode, p.scale, p.transform);
        let screens = |placements: Vec<Placement>| -> Vec<Screen> {
            outs.iter()
                .enumerate()
                .zip(placements)
                .map(|((j, o), pl)| Screen {
                    id: o.description.clone(),
                    label: labels[j].clone(),
                    enabled: j == i || o.active.is_some(),
                    size: if j == i { size } else { current[j].size },
                    placement: if j == i { own.clone() } else { pl },
                })
                .collect()
        };
        let solve = |sc: Vec<Screen>| resolve(&sc).map(|pos| (sc, pos));
        let (sc, pos) = if p.place == PlaceChoice::Origin {
            let mut placements = deduce(&current, Some(i), None);
            if outs[i].active.is_none() {
                // Off: the old origin keeps its place (0, 0).
                for (j, pl) in placements.iter_mut().enumerate() {
                    if j != i && *pl == Placement::Origin {
                        *pl = Placement::Absolute { x: 0, y: 0 };
                    }
                }
            }
            solve(screens(placements))
        } else {
            match solve(screens(deduce(&current, None, None))) {
                // The others depended on this screen: they keep their place.
                Err(GeoError::Cycle(_)) => solve(screens(deduce(&current, None, Some(i)))),
                r => r,
            }
        }
        .map_err(|e| e.to_string())?;
        let pos = kept_in_place(outs, &sc, pos, i);
        let plan = Plan::between(outs, &targets(outs, pos, i, (p.mode, p.scale, p.transform)));
        if plan.steps.is_empty() { Err("no change".into()) } else { Ok(plan) }
    }

    fn panel_mut(&mut self) -> Option<&mut Panel> {
        match &mut self.overlay {
            Overlay::Panel(p) => Some(p),
            _ => None,
        }
    }

    /// Previous field (`dir` < 0) or next.
    pub(crate) fn panel_move(&mut self, dir: i32) {
        if let Some(p) = self.panel_mut() {
            let fields = panel_fields(p);
            let k = fields.iter().position(|f| *f == p.field).unwrap_or(0) as i32;
            p.field = fields[(k + dir).clamp(0, fields.len() as i32 - 1) as usize];
        }
    }

    /// Changes the focused field's value (←/→, -/+).
    pub(crate) fn panel_step(&mut self, dir: i32) {
        let Overlay::Panel(p) = &self.overlay else { return };
        let mut p = p.clone();
        match p.field {
            PanelField::Mode => {
                let modes = self.panel_modes(&p);
                let k = match modes.iter().position(|m| *m == p.mode) {
                    Some(k) => (k as i32 + dir).clamp(0, modes.len() as i32 - 1) as usize,
                    None => 0,
                };
                if let Some(m) = modes.get(k) {
                    p.mode = *m;
                }
            }
            PanelField::Scale => p.scale = p.scale.step(dir),
            PanelField::Rotation => p.transform = wrap(usize::from(p.transform % 4), dir, 4) as u8,
            PanelField::Place => {
                let mut choices: Vec<PlaceChoice> = p.free.map(|_| PlaceChoice::Free).into_iter().collect();
                choices.push(PlaceChoice::Origin);
                choices.extend(Relation::ALL.map(PlaceChoice::Rel));
                let k = choices.iter().position(|c| *c == p.place).unwrap_or(0);
                p.place = choices[wrap(k, dir, choices.len())];
                if matches!(p.place, PlaceChoice::Rel(_)) && p.reference.is_none() {
                    p.reference = self.panel_references(&p).first().cloned();
                }
            }
            PanelField::Reference => {
                let refs = self.panel_references(&p);
                if !refs.is_empty() {
                    let k = p.reference.as_ref().and_then(|r| refs.iter().position(|x| x == r)).unwrap_or(0);
                    p.reference = Some(refs[wrap(k, dir, refs.len())].clone());
                }
            }
            PanelField::Align => {
                let k = Align::ALL.iter().position(|a| *a == p.align).unwrap_or(0);
                p.align = Align::ALL[wrap(k, dir, 3)];
            }
            PanelField::Cancel | PanelField::Apply => {
                p.field = if dir < 0 { PanelField::Cancel } else { PanelField::Apply };
            }
        }
        self.overlay = Overlay::Panel(p);
    }

    /// Mode search: the text changes, the mode follows the first match.
    pub(crate) fn panel_query(&mut self, edit: impl FnOnce(&mut String)) {
        let Overlay::Panel(p) = &self.overlay else { return };
        let mut p = p.clone();
        let mut q = p.query.take().unwrap_or_default();
        edit(&mut q);
        p.query = Some(q);
        let modes = self.panel_modes(&p);
        if !modes.contains(&p.mode) {
            if let Some(m) = modes.first() {
                p.mode = *m;
            }
        }
        self.overlay = Overlay::Panel(p);
    }

    /// Enter in the panel: Cancel closes, Apply sends, any other field
    /// moves to Apply.
    pub(crate) fn panel_enter(&mut self, now: Instant) {
        let Overlay::Panel(p) = &self.overlay else { return };
        match p.field {
            PanelField::Cancel => self.overlay = Overlay::None,
            PanelField::Apply => self.panel_apply(now),
            _ => {
                if let Some(p) = self.panel_mut() {
                    p.field = PanelField::Apply;
                }
            }
        }
    }

    pub(crate) fn panel_apply(&mut self, now: Instant) {
        let Overlay::Panel(p) = &self.overlay else { return };
        let p = p.clone();
        match self.panel_preview(&p) {
            Ok(plan) => {
                self.overlay = Overlay::None;
                let outs = self.outputs();
                let labels = short_names(outs);
                let label = outs.iter().position(|o| o.name == p.output).map_or(p.output.clone(), |i| labels[i].clone());
                self.start_trial(format!("Adjusting {label}"), plan, Vec::new(), now);
            }
            Err(e) => self.toast(format!("Displays: {e}"), true, now),
        }
    }

    // ── Save current ─────────────────────────────────────────────────

    pub(crate) fn open_save(&mut self, now: Instant) {
        if self.data.displays.ready().is_none() {
            self.toast("Displays: unknown state".into(), true, now);
            return;
        }
        self.overlay = Overlay::SaveProfile(SaveDialog::default());
    }

    /// Enter in the name entry: asks for confirmation if the profile
    /// exists, otherwise saves.
    pub(crate) fn save_enter(&mut self, now: Instant) {
        let Overlay::SaveProfile(d) = &self.overlay else { return };
        let name = d.name.trim().to_string();
        if d.replace {
            let yes = d.yes_focused;
            return self.save_answer(yes, now);
        }
        if name.is_empty() {
            return;
        }
        if self.displays.profiles.iter().any(|p| p.name == name) {
            self.overlay = Overlay::SaveProfile(SaveDialog { name, replace: true, yes_focused: false });
        } else {
            self.save_current(&name, now);
        }
    }

    /// Answer to "Replace the profile?": yes saves, no closes.
    pub(crate) fn save_answer(&mut self, yes: bool, now: Instant) {
        let Overlay::SaveProfile(d) = &self.overlay else { return };
        let name = d.name.trim().to_string();
        if yes {
            self.save_current(&name, now);
        } else {
            self.overlay = Overlay::None;
        }
    }

    fn save_current(&mut self, name: &str, now: Instant) {
        self.overlay = Overlay::None;
        if self.displays.profiles_invalid {
            // The file loaded was invalid: never overwrite what we couldn't read.
            let msg = "invalid displays.toml: fix it before saving".to_string();
            self.status.error = Some(msg.clone());
            self.toast(msg, true, now);
            return;
        }
        let Some(outs) = self.data.displays.ready() else {
            self.toast("Displays: unknown state".into(), true, now);
            return;
        };
        let p = from_outputs(name, outs);
        let mut list = self.displays.profiles.clone();
        match list.iter().position(|x| x.name == name) {
            Some(k) => list[k] = p,
            None => list.push(p),
        }
        let r = match &self.displays.profiles_path {
            Some(path) => profiles::save(path, &list),
            None => Err("no configuration directory".into()),
        };
        match r {
            Ok(()) => {
                self.displays.profiles = list;
                let msg = format!("Profile \"{name}\" saved");
                self.status.last = Some(msg.clone());
                self.toast(msg, false, now);
            }
            Err(e) => {
                let msg = format!("Profile \"{name}\" not saved: {e}");
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
            }
        }
    }
}

#[cfg(test)]
mod tests;
