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
    /// has fixed it (set by `build_app`, task 11).
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
            // Controller re-check (task 5): the file loaded was invalid —
            // never overwrite what we couldn't read.
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
mod tests {
    use super::*;
    use crate::app::action::{Command, Tab};
    use crate::app::testing::ready_app;
    use crate::app::worker::{Domain, Request, Update};
    use crate::app::{Item, Loadable};
    use crate::backends::BackendError;
    use crate::displays::profiles::from_outputs;
    use crate::displays::testing::*;

    fn later(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    /// Displays tab open, data ready, request box emptied.
    fn displays_tab() -> App {
        let mut a = ready_app();
        a.go_tab(Tab::Displays, Instant::now());
        a.take_requests();
        a
    }

    /// The sent trial: (plan, reverse plan).
    fn sent_trial(a: &mut App) -> (Plan, Plan) {
        match a.take_requests().as_slice() {
            [Request::Do(Op::ApplyDisplays { plan, undo: Some(u) })] => (plan.clone(), u.clone()),
            r => panic!("expected a trial: {r:?}"),
        }
    }

    fn sent_undo(a: &mut App) -> Plan {
        let r = a.take_requests();
        r.iter()
            .find_map(|q| match q {
                Request::Do(Op::ApplyDisplays { plan, undo: None }) => Some(plan.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a rollback: {r:?}"))
    }

    /// "Bureau" trial sent and applied at `t0`: countdown started.
    fn applied(t0: Instant) -> (App, Plan, Plan) {
        let mut a = displays_tab();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: plan.clone(), undo: Some(undo.clone()) }, Ok(())), t0);
        a.take_requests();
        (a, plan, undo)
    }

    /// The outputs as `plan` leaves them (matching read).
    fn applied_outputs(plan: &Plan) -> Vec<Output> {
        let mut outs = machine();
        for s in &plan.steps {
            let o = outs.iter_mut().find(|o| o.name == s.output).unwrap();
            o.active = s.active.clone();
        }
        outs
    }

    fn no_do(a: &mut App) -> bool {
        !a.take_requests().iter().any(|r| matches!(r, Request::Do(_)))
    }

    #[test]
    fn locked_until_a_fresh_read() {
        let t0 = Instant::now();
        let (mut a, plan, undo) = applied(t0);
        // Read taken during the trial: it shows the tried settings.
        a.on_update(Update::Displays(Ok(applied_outputs(&plan))), t0);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: undo.clone(), undo: None }, Ok(())), t0);
        assert_eq!(a.displays.undoing, None);
        // The data is still that of the rejected trial: nothing should go out.
        a.toggle_output("HDMI-A-1", t0);
        a.apply_display_profile("Laptop seul", t0);
        assert!(no_do(&mut a), "no setting on a stale read");
        assert_eq!(a.displays.trial, None);
        // Only the fresh read is missing: the message says so.
        assert!(a.toasts.iter().any(|t| t.text == "Displays: waiting for a fresh read"));
        assert!(!a.toasts.iter().any(|t| t.text == "Displays: a change is already in progress"));
        // Fresh read: the restored state becomes the new base, and the new rollback.
        a.on_update(Update::Displays(Ok(machine())), t0);
        a.toggle_output("HDMI-A-1", t0);
        let (p, u) = sent_trial(&mut a);
        assert_eq!(u, Plan::undo(&machine(), &p));
        assert_eq!(a.displays.trial.as_ref().unwrap().before, machine());

        // Keep before any read: same lock.
        let (mut a, _, _) = applied(t0);
        a.keep_answer(true, t0);
        assert_eq!(a.displays.trial, None);
        a.toggle_output("HDMI-A-1", t0);
        assert!(no_do(&mut a), "kept but not read yet");
        assert_eq!(a.toasts.last().map(|t| t.text.as_str()), Some("Displays: waiting for a fresh read"));
        a.on_update(Update::Displays(Ok(applied_outputs(&plan))), t0);
        a.toggle_output("HDMI-A-1", t0);
        let (_, u) = sent_trial(&mut a);
        assert_eq!(u, Plan::undo(&applied_outputs(&plan), &Plan { steps: vec![Step { output: "HDMI-A-1".into(), active: None }] }));
    }

    #[test]
    fn cancel_before_the_response_then_success() {
        let t0 = Instant::now();
        let mut a = displays_tab();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo, "Cancel while applying: rollback right away");
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo.clone()) }, Ok(())), t0);
        assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None), "the trial does not come back");
        assert_eq!(a.exit_undo(), Some(undo.clone()), "the in-flight rollback is still pending");
        a.on_tick(later(t0, 20_000));
        assert!(no_do(&mut a), "no second rollback at the deadline");
        a.on_update(Update::Done(Op::ApplyDisplays { plan: undo, undo: None }, Ok(())), t0);
        assert_eq!(a.exit_undo(), None);
        a.on_update(Update::Displays(Ok(machine())), t0);
        a.take_requests();
        a.toggle_output("HDMI-A-1", t0);
        sent_trial(&mut a);
    }

    #[test]
    fn cancel_before_the_response_then_failure() {
        let t0 = Instant::now();
        let mut a = displays_tab();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo);
        let e = BackendError::Timeout { tool: "wm-set".into() };
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo.clone()) }, Err(e)), t0);
        assert_eq!(sent_undo(&mut a), undo, "second rollback sent");
        assert_eq!(a.displays.trial, None);
        for _ in 0..2 {
            a.on_update(Update::Done(Op::ApplyDisplays { plan: undo.clone(), undo: None }, Ok(())), t0);
        }
        assert_eq!(a.displays.undoing, None);
        assert_eq!(a.exit_undo(), None);
        a.on_update(Update::Displays(Ok(machine())), t0);
        a.take_requests();
        a.toggle_output("HDMI-A-1", t0);
        sent_trial(&mut a);
    }

    #[test]
    fn tab_items() {
        let mut a = ready_app();
        a.go_tab(Tab::Displays, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])]);
        assert_eq!(
            a.items(),
            vec![Item::DisplayProfiles, Item::Output("eDP-1".into()), Item::Output("DVI-I-1".into()), Item::Output("HDMI-A-1".into()),]
        );
    }

    /// Under Sway, as under niri, the tab calls nothing (no read, no trial, no profile), so
    /// `wm-set` is never run (`wm-set outputs` refuses there).
    #[test]
    fn under_sway_or_niri_the_tab_calls_nothing() {
        for name in ["Sway", "niri"] {
            let mut a = ready_app();
            a.displays.unavailable = Some(name);
            a.go_tab(Tab::Displays, Instant::now());
            assert!(a.take_requests().is_empty(), "{name}");
            assert!(a.items().is_empty(), "{name}");
            a.on_tick(Instant::now() + Duration::from_secs(30));
            assert!(a.take_requests().is_empty(), "{name}: not even on tick");
            assert!(!a.palette_matches().iter().any(|(l, _)| l.starts_with("Display profile")), "{name}");
            a.run_command(Command::ApplyDisplayProfile("Bureau".into()), Instant::now());
            assert_eq!(a.displays.queued_profile, None, "{name}");
            a.take_requests();
            a.toggle_output("HDMI-A-1", Instant::now());
            assert!(a.take_requests().is_empty(), "{name}: not even toggling an output");
            assert_eq!(a.displays.trial, None, "{name}");
        }
    }

    #[test]
    fn profile_applied_with_countdown() {
        let mut a = displays_tab();
        let t0 = Instant::now();
        a.displays.profile_sel = 1; // Bureau
        a.profile_bar_activate(t0);
        let (plan, undo) = sent_trial(&mut a);
        assert_eq!(plan.steps.iter().map(|s| s.output.as_str()).collect::<Vec<_>>(), vec!["DVI-I-1", "HDMI-A-1", "eDP-1"]);
        assert_eq!(undo, Plan::undo(&machine(), &plan));
        assert_eq!(a.overlay, Overlay::Keep(Keep { keep_focused: false }), "focus on Cancel");
        let t = a.displays.trial.clone().unwrap();
        assert_eq!(t.title, "Profile \"Bureau\"");
        assert_eq!(t.deadline, None, "no countdown before the response");
        assert_eq!(t.summary[0], "Alpha: 1920×1080@60, scale 1.0, 0°, position 0, 0");
        assert_eq!(t.summary[2], "Laptop: off");
        assert_eq!(a.exit_undo(), Some(undo.clone()), "closing while applying: rollback");
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo) }, Ok(())), t0);
        let t = a.displays.trial.clone().unwrap();
        assert_eq!((t.deadline, t.remaining), (Some(t0 + KEEP_DELAY), 15));
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])], "re-read after applying");
    }

    #[test]
    fn automatic_rollback_after_fifteen_seconds() {
        let t0 = Instant::now();
        let (mut a, _, undo) = applied(t0);
        a.on_tick(later(t0, 5_200));
        assert_eq!(a.displays.trial.as_ref().unwrap().remaining, 10);
        assert!(a.take_requests().iter().all(|r| matches!(r, Request::Refresh(_))), "nothing is rolled back before 15 s");
        a.on_tick(later(t0, 15_000));
        assert_eq!(sent_undo(&mut a), undo);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.displays.trial, None);
        assert_eq!(a.exit_undo(), Some(undo.clone()), "unanswered rollback: redone on close");
        a.on_update(Update::Done(Op::ApplyDisplays { plan: undo, undo: None }, Ok(())), later(t0, 15_100));
        assert_eq!(a.exit_undo(), None);
        assert!(a.toasts.iter().any(|t| t.text == "Displays: previous settings restored"));
    }

    #[test]
    fn cancel_and_keep() {
        let t0 = Instant::now();
        let (mut a, _, undo) = applied(t0);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo, "Cancel: rollback right away");
        let (mut a, _, _) = applied(t0);
        a.keep_answer(true, t0);
        assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None));
        assert!(a.take_requests().is_empty(), "Keep reapplies nothing");
        assert_eq!(a.exit_undo(), None, "kept: nothing to undo on close");
        a.on_tick(later(t0, 20_000));
        assert!(a.take_requests().iter().all(|r| matches!(r, Request::Refresh(_))));
    }

    #[test]
    fn keep_waits_for_the_apply() {
        let mut a = displays_tab();
        a.apply_display_profile("Bureau", Instant::now());
        a.take_requests();
        a.keep_answer(true, Instant::now());
        assert!(a.displays.trial.is_some(), "not applied yet: Keep has no effect");
    }

    #[test]
    fn failure_mid_plan_rolls_back_right_away() {
        let mut a = displays_tab();
        let t0 = Instant::now();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        let e = BackendError::Failed { tool: "wm-set".into(), code: Some(1), stderr: "Hyprland refused \"eval …\": boom\n".into() };
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo.clone()) }, Err(e)), t0);
        assert_eq!(sent_undo(&mut a), undo);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(
            a.status.error.as_deref(),
            Some("Displays: wm-set failed (code 1): Hyprland refused \"eval …\": boom — rolling back to the previous settings")
        );
    }

    #[test]
    fn rollback_impossible_is_reported() {
        let mut a = displays_tab();
        let e = BackendError::Timeout { tool: "wm-set".into() };
        a.displays.undoing = Some(Plan::default());
        a.on_update(Update::Done(Op::ApplyDisplays { plan: Plan::default(), undo: None }, Err(e)), Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Displays: rollback impossible: wm-set is not responding"));
        assert_eq!(a.displays.undoing, None);
    }

    #[test]
    fn incomplete_rollback_is_reported() {
        let mut a = displays_tab();
        let e = BackendError::Incomplete {
            failures: vec![("DVI-I-1".into(), BackendError::Timeout { tool: "wm-set".into() })],
            kept: Vec::new(),
        };
        a.displays.undoing = Some(Plan::default());
        a.on_update(Update::Done(Op::ApplyDisplays { plan: Plan::default(), undo: None }, Err(e)), Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Displays: incomplete rollback: DVI-I-1 (wm-set is not responding)"));
        assert_eq!(a.displays.undoing, None);
    }

    #[test]
    fn external_change_closes_without_reapplying_anything() {
        let t0 = Instant::now();
        let (mut a, plan, _) = applied(t0);
        // Read matching the trial: the dialog stays.
        a.on_update(Update::Displays(Ok(applied_outputs(&plan))), t0);
        assert!(a.displays.trial.is_some());
        // Dock plugged in next to it (an output the trial doesn't touch):
        // the dialog closes, no rollback sent.
        let mut read = applied_outputs(&plan);
        read.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
        a.on_update(Update::Displays(Ok(read)), t0);
        assert_eq!(a.displays.trial, None);
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.toasts.iter().any(|t| t.text == "Displays changed elsewhere"));
        assert!(no_do(&mut a), "nothing is reapplied");
        assert_eq!(a.exit_undo(), None);

        // An untouched output unplugged (the trial only turns Bravo off): same thing.
        let mut a = displays_tab();
        a.toggle_output("HDMI-A-1", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: plan.clone(), undo: Some(undo) }, Ok(())), t0);
        a.take_requests();
        let mut read = applied_outputs(&plan);
        read.retain(|o| o.name != "DVI-I-1");
        a.on_update(Update::Displays(Ok(read)), t0);
        assert_eq!(a.displays.trial, None);
        assert_eq!(a.status.last.as_deref(), Some("Displays changed elsewhere"));
        assert!(no_do(&mut a), "nothing is reapplied");
    }

    const SCREEN_LOST: &str = "Screen lost during the trial: rolling back to the previous state";

    #[test]
    fn touched_screen_lost_rolls_back_on_the_outputs_still_present() {
        // Decision D1: Bureau tried (laptop panel off, Alpha and Bravo on),
        // then Alpha (DVI-I-1) disappears.
        let t0 = Instant::now();
        let (mut a, plan, undo) = applied(t0);
        let mut read = applied_outputs(&plan);
        read.retain(|o| o.name != "DVI-I-1");
        a.on_update(Update::Displays(Ok(read)), t0);
        let rollback = sent_undo(&mut a);
        let expected: Vec<Step> = undo.steps.iter().filter(|s| s.output != "DVI-I-1").cloned().collect();
        assert_eq!(rollback.steps, expected, "reverse plan restricted to the outputs still present");
        assert_eq!(rollback.steps[0], Step { output: "eDP-1".into(), active: machine()[0].active.clone() }, "panel turned back on");
        assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None));
        assert_eq!(a.exit_undo(), Some(rollback), "in-flight rollback: redone on close");
        assert_eq!(a.status.error.as_deref(), Some(SCREEN_LOST));
        assert!(a.toasts.iter().any(|t| t.text == SCREEN_LOST));
    }

    #[test]
    fn no_active_screen_left_rolls_back() {
        // Every output present, none active: full rollback.
        let t0 = Instant::now();
        let (mut a, plan, undo) = applied(t0);
        let mut read = applied_outputs(&plan);
        for o in &mut read {
            o.active = None;
        }
        a.on_update(Update::Displays(Ok(read)), t0);
        assert_eq!(sent_undo(&mut a), undo);
        assert_eq!(a.displays.trial, None);
        assert_eq!(a.status.error.as_deref(), Some(SCREEN_LOST));

        // A trial that only turns Bravo off, then the other two turn off
        // elsewhere: nothing on any more, Bravo is turned back on.
        let mut a = displays_tab();
        a.toggle_output("HDMI-A-1", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: plan.clone(), undo: Some(undo.clone()) }, Ok(())), t0);
        a.take_requests();
        let mut read = applied_outputs(&plan);
        read[0].active = None;
        read[1].active = None;
        a.on_update(Update::Displays(Ok(read)), t0);
        assert_eq!(sent_undo(&mut a), undo);
        assert_eq!(undo.steps, vec![Step { output: "HDMI-A-1".into(), active: machine()[2].active.clone() }]);
    }

    const NOTHING_TO_RESTORE: &str = "Screen lost during the trial: nothing to restore";

    #[test]
    fn touched_screen_renamed_within_the_same_read() {
        // Alpha reappears right away under another connector (DVI-I-1 →
        // DVI-I-2): the touched output has disappeared, roll back on what's left.
        let t0 = Instant::now();
        let (mut a, plan, undo) = applied(t0);
        let mut read = applied_outputs(&plan);
        read[1].name = "DVI-I-2".into();
        a.on_update(Update::Displays(Ok(read)), t0);
        let rollback = sent_undo(&mut a);
        let expected: Vec<Step> = undo.steps.iter().filter(|s| s.output != "DVI-I-1").cloned().collect();
        assert_eq!(rollback.steps, expected);
        assert!(!rollback.touches("DVI-I-2"), "the new connector is not touched");
        assert_eq!(a.status.error.as_deref(), Some(SCREEN_LOST));
    }

    #[test]
    fn lost_screen_never_an_unneeded_shutdown_or_the_last_one() {
        let t0 = Instant::now();
        // Bravo off, the trial turns it on; then everything turns off
        // elsewhere: the rollback would only contain "turn Bravo off",
        // already off.
        let mut a = displays_tab();
        let mut outs = machine();
        outs[2].active = None;
        a.data.displays = Loadable::Ready(outs);
        a.toggle_output("HDMI-A-1", t0);
        let (plan, undo) = sent_trial(&mut a);
        assert!(undo.steps.iter().all(|s| s.active.is_none()));
        a.on_update(Update::Done(Op::ApplyDisplays { plan: plan.clone(), undo: Some(undo) }, Ok(())), t0);
        a.take_requests();
        let mut read = applied_outputs(&plan);
        for o in &mut read {
            o.active = None;
        }
        a.on_update(Update::Displays(Ok(read)), t0);
        assert!(no_do(&mut a), "no unneeded shutdown");
        assert_eq!((a.displays.trial.clone(), a.displays.undoing.clone()), (None, None));
        assert_eq!(a.status.error.as_deref(), Some(NOTHING_TO_RESTORE));

        // Panel off and Alpha on; the trial swaps them, then Alpha
        // disappears: turning the panel off would leave everything dark,
        // nothing is sent.
        let mut a = displays_tab();
        let mut outs = machine();
        outs.truncate(2);
        let previous = outs[0].active.take();
        a.data.displays = Loadable::Ready(outs);
        let plan = Plan { steps: vec![Step { output: "eDP-1".into(), active: previous }, Step { output: "DVI-I-1".into(), active: None }] };
        a.start_trial("swap".into(), plan.clone(), Vec::new(), t0);
        let (plan, undo) = sent_trial(&mut a);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: plan.clone(), undo: Some(undo) }, Ok(())), t0);
        a.take_requests();
        let mut read = applied_outputs(&plan);
        read.truncate(1); // only the panel remains, on
        a.on_update(Update::Displays(Ok(read)), t0);
        assert!(no_do(&mut a), "the last output still on is never turned off");
        assert_eq!(a.status.error.as_deref(), Some(NOTHING_TO_RESTORE));
    }

    #[test]
    fn touched_screen_lost_with_nothing_to_restore() {
        // The trial only turns Alpha off, which disappears: the other
        // outputs weren't touched, nothing to send, the dialog closes.
        let t0 = Instant::now();
        let mut a = displays_tab();
        a.toggle_output("DVI-I-1", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: plan.clone(), undo: Some(undo) }, Ok(())), t0);
        a.take_requests();
        let mut read = applied_outputs(&plan);
        read.retain(|o| o.name != "DVI-I-1");
        a.on_update(Update::Displays(Ok(read)), t0);
        assert!(no_do(&mut a), "no empty plan sent");
        assert_eq!((a.displays.trial.clone(), a.displays.undoing.clone()), (None, None));
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.status.error.as_deref(), Some(NOTHING_TO_RESTORE));
    }

    #[test]
    fn no_other_change_during_the_trial() {
        let t0 = Instant::now();
        let (mut a, _, _) = applied(t0);
        a.toggle_output("eDP-1", t0);
        a.apply_display_profile("Laptop seul", t0);
        a.open_panel("eDP-1", t0);
        assert!(!a.take_requests().iter().any(|r| matches!(r, Request::Do(_))));
        assert_eq!(a.overlay, Overlay::Keep(Keep::default()));
        assert!(a.toasts.iter().any(|t| t.text == "Displays: a change is already in progress"));
    }

    #[test]
    fn displaylink_warning() {
        let mut a = displays_tab();
        a.toggle_output("DVI-I-1", Instant::now()); // Alpha, on the DisplayLink dock
        assert!(a.displays.trial.as_ref().unwrap().displaylink);
        let mut a = displays_tab();
        a.toggle_output("HDMI-A-1", Instant::now());
        assert!(!a.displays.trial.as_ref().unwrap().displaylink);
    }

    #[test]
    fn last_active_screen_protected() {
        let mut a = displays_tab();
        a.data.displays = Loadable::Ready(vec![machine()[0].clone(), off("DVI-I-1", ALPHA), off("HDMI-A-1", BRAVO)]);
        a.toggle_output("eDP-1", Instant::now());
        assert!(a.take_requests().is_empty());
        assert!(a.toasts.iter().any(|t| t.text == "Can't disable Laptop: last active screen"));
        // From a profile: Bureau far from the dock would turn off the
        // panel, the only one plugged in.
        a.data.displays = Loadable::Ready(vec![machine()[0].clone()]);
        a.apply_display_profile("Bureau", Instant::now());
        assert!(a.take_requests().is_empty());
        assert_eq!(a.status.error.as_deref(), Some("Profile \"Bureau\" refused: this profile would turn off the last active screen"));
    }

    #[test]
    fn toggling_an_output() {
        let mut a = displays_tab();
        a.toggle_output("eDP-1", Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert_eq!(plan.steps, vec![Step { output: "eDP-1".into(), active: None }], "the others don't move");
        // An off output turns on to the right of the rightmost one (Bravo ends at 4053).
        let mut a = displays_tab();
        let mut outs = machine();
        outs.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
        a.data.displays = Loadable::Ready(outs);
        a.toggle_output("DP-3", Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert_eq!(
            plan.steps,
            vec![Step {
                output: "DP-3".into(),
                active: Some(Active { mode: mode(1920, 1080, 60), x: 4053, y: 0, scale: Scale::ONE, transform: 0 }),
            }]
        );
    }

    #[test]
    fn palette_profile_applied_on_the_next_read() {
        let mut a = ready_app();
        a.run_command(Command::ApplyDisplayProfile("Bureau".into()), Instant::now());
        assert_eq!(a.tab, Tab::Displays);
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])], "read first, nothing applied yet");
        a.on_update(Update::Displays(Ok(machine())), Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert!(plan.touches("DVI-I-1"));
        assert_eq!(a.displays.queued_profile, None);
    }

    #[test]
    fn queued_profile_dropped_if_the_read_fails() {
        let mut a = ready_app();
        a.run_command(Command::ApplyDisplayProfile("Bureau".into()), Instant::now());
        a.take_requests();
        a.on_update(Update::Displays(Err(BackendError::Missing { tool: "wm-set".into() })), Instant::now());
        assert_eq!(a.displays.queued_profile, None);
        // A successful read much later applies nothing.
        a.on_update(Update::Displays(Ok(machine())), Instant::now());
        assert!(no_do(&mut a), "profile dropped along with the failed read");
        assert_eq!(a.displays.trial, None);
    }

    #[test]
    fn missing_profile_screen_is_reported() {
        let mut a = displays_tab();
        a.data.displays = Loadable::Ready(vec![machine()[0].clone(), machine()[1].clone()]);
        a.apply_display_profile("Bureau", Instant::now());
        assert_eq!(a.displays.trial.as_ref().unwrap().notes, vec!["Bravo missing, ignored"]);
    }

    #[test]
    fn profile_already_in_place() {
        let mut a = displays_tab();
        a.displays.profiles = vec![from_outputs("Ici", &machine())];
        a.apply_display_profile("Ici", Instant::now());
        assert!(a.take_requests().is_empty());
        assert_eq!(a.displays.trial, None);
        assert!(a.toasts.iter().any(|t| t.text == "Profile \"Ici\": nothing to change"));
    }

    #[test]
    fn profile_bar_bounded() {
        let mut a = ready_app();
        a.go_tab(Tab::Displays, Instant::now());
        a.adjust(1, Instant::now());
        a.adjust(1, Instant::now());
        a.adjust(1, Instant::now());
        assert_eq!(a.displays.profile_sel, 2, "two profiles, then \"+ Save current\"");
        a.adjust(-1, Instant::now());
        assert_eq!(a.displays.profile_sel, 1);
        assert!(a.take_requests().iter().all(|r| matches!(r, Request::Refresh(_))), "choosing applies nothing");
    }

    // ── Panel ────────────────────────────────────────────────────────

    fn panel(a: &App) -> Panel {
        match &a.overlay {
            Overlay::Panel(p) => p.clone(),
            o => panic!("expected a panel: {o:?}"),
        }
    }

    #[test]
    fn panel_opens_on_the_current_state() {
        let mut a = displays_tab();
        a.open_panel("HDMI-A-1", Instant::now());
        let p = panel(&a);
        assert_eq!((p.mode, p.scale, p.transform), (mode(1920, 1080, 240), Scale::ONE, 0));
        assert_eq!((p.place, p.reference.as_deref(), p.align), (PlaceChoice::Rel(Relation::RightOf), Some(ALPHA), Align::Start));
        assert_eq!(a.panel_preview(&p), Err("no change".into()));
        a.open_panel("eDP-1", Instant::now());
        let p = panel(&a);
        assert_eq!((p.place, p.free), (PlaceChoice::Free, Some((-213, 1080))), "position not reproducible: free");
        assert_eq!(
            panel_fields(&p),
            vec![PanelField::Mode, PanelField::Scale, PanelField::Rotation, PanelField::Place, PanelField::Cancel, PanelField::Apply]
        );
    }

    #[test]
    fn scaling_the_origin_neighbors_follow() {
        let mut a = displays_tab();
        a.open_panel("DVI-I-1", Instant::now());
        a.panel_move(1); // Scale
        a.panel_step(1); // 1.25: Alpha moves to 1536×864
        let plan = a.panel_preview(&panel(&a)).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].output, "DVI-I-1");
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y, a.scale)), Some((213, 0, Scale(1250))));
        assert_eq!(plan.steps[1].output, "HDMI-A-1");
        assert_eq!(plan.steps[1].active.as_ref().map(|a| (a.x, a.y)), Some((1749, 0)), "Bravo follows Alpha");
        a.panel_apply(Instant::now());
        assert_eq!(a.displays.trial.as_ref().unwrap().title, "Adjusting Alpha");
        assert_eq!(sent_trial(&mut a).0, plan);
    }

    #[test]
    fn cycle_avoided_the_others_keep_their_place() {
        // Alpha to the right of Bravo, while Bravo is to the right of Alpha.
        let mut a = displays_tab();
        a.open_panel("DVI-I-1", Instant::now());
        let mut p = panel(&a);
        p.place = PlaceChoice::Rel(Relation::RightOf);
        p.reference = Some(BRAVO.into());
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(plan.steps.len(), 1, "only Alpha moves: {plan:?}");
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y)), Some((4053, 0)));
    }

    #[test]
    fn placement_refused_with_a_reason() {
        let mut a = displays_tab();
        a.open_panel("HDMI-A-1", Instant::now());
        let mut p = panel(&a);
        p.place = PlaceChoice::Rel(Relation::Below);
        a.overlay = Overlay::Panel(p.clone());
        assert_eq!(a.panel_preview(&p), Err("Laptop and Bravo overlap".into()));
        a.panel_apply(Instant::now());
        assert!(a.take_requests().is_empty(), "Apply greyed out: nothing is sent");
        assert!(matches!(a.overlay, Overlay::Panel(_)));
    }

    #[test]
    fn choosing_the_placement_and_the_reference() {
        let mut a = displays_tab();
        a.open_panel("eDP-1", Instant::now());
        a.panel_move(3); // Placement
        assert_eq!(panel(&a).field, PanelField::Place);
        a.panel_step(1); // free → origin
        assert_eq!(panel(&a).place, PlaceChoice::Origin);
        a.panel_step(1); // → right of
        let p = panel(&a);
        assert_eq!((p.place, p.reference.as_deref()), (PlaceChoice::Rel(Relation::RightOf), Some(ALPHA)), "reference offered");
        assert!(panel_fields(&p).contains(&PanelField::Reference));
        a.panel_move(1);
        a.panel_step(1);
        assert_eq!(panel(&a).reference.as_deref(), Some(BRAVO));
        a.panel_move(1);
        a.panel_step(-1);
        assert_eq!(panel(&a).align, Align::End, "cyclic");
        a.panel_step(1);
        a.panel_step(-1);
        a.panel_move(5);
        assert_eq!(panel(&a).field, PanelField::Apply, "bounded at the last field");
    }

    #[test]
    fn rotation_and_scale() {
        let mut a = displays_tab();
        a.open_panel("eDP-1", Instant::now());
        a.panel_move(2);
        a.panel_step(-1);
        assert_eq!(panel(&a).transform, 3, "0° ← : 270°");
        a.panel_move(-1);
        for _ in 0..20 {
            a.panel_step(1);
        }
        assert_eq!(panel(&a).scale, Scale::MAX);
    }

    #[test]
    fn mode_search() {
        let mut a = displays_tab();
        a.data.displays =
            Loadable::Ready(crate::backends::displays::parse_outputs(include_str!("../../tests/fixtures/wm-set-outputs.json")).unwrap());
        a.open_panel("DVI-I-1", Instant::now());
        assert_eq!(a.panel_modes(&panel(&a)).len(), 26);
        a.panel_query(|q| q.push_str("1280"));
        let p = panel(&a);
        assert_eq!(p.mode, mode(1280, 1024, 75), "first match");
        assert!(a.panel_modes(&p).iter().all(|m| m.to_string().contains("1280")));
        assert_eq!(a.panel_modes(&p).len(), 5, "\"1280\" does not find \"1920x1080\"");
        a.panel_step(1);
        assert_eq!(panel(&a).mode, mode(1280, 1024, 70));
    }

    #[test]
    fn panel_of_an_unplugged_output() {
        let mut a = displays_tab();
        a.open_panel("HDMI-A-1", Instant::now());
        let p = panel(&a);
        let without: Vec<Output> = machine().into_iter().filter(|o| o.name != "HDMI-A-1").collect();
        a.data.displays = Loadable::Ready(without.clone());
        assert_eq!(a.panel_preview(&p), Err("output unplugged".into()));
        a.on_update(Update::Displays(Ok(without)), Instant::now());
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.toasts.iter().any(|t| t.text == "Displays: HDMI-A-1 unplugged, setting abandoned"));
    }

    #[test]
    fn off_output_adjusted_then_turned_on() {
        let mut a = displays_tab();
        let mut outs = machine();
        outs.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
        a.data.displays = Loadable::Ready(outs);
        a.open_panel("DP-3", Instant::now());
        let p = panel(&a);
        assert_eq!((p.place, p.reference.as_deref()), (PlaceChoice::Rel(Relation::RightOf), Some(BRAVO)));
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y)), Some((4053, 0)));
    }

    /// The machine, laptop panel off: the layout starts at (213, 0).
    fn without_laptop() -> Vec<Output> {
        let mut outs = machine();
        outs[0] = off("eDP-1", INTERNAL);
        outs
    }

    #[test]
    fn space_on_an_off_screen_the_others_do_not_move() {
        let mut a = displays_tab();
        a.data.displays = Loadable::Ready(without_laptop());
        a.toggle_output("eDP-1", Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert_eq!(
            plan.steps,
            vec![Step {
                output: "eDP-1".into(),
                active: Some(Active { mode: mode(1920, 1080, 60), x: 4053, y: 0, scale: Scale::ONE, transform: 0 }),
            }],
            "Alpha and Bravo stay at 213 and 2133"
        );
    }

    #[test]
    fn layout_off_origin_panel_with_no_change() {
        let mut a = displays_tab();
        a.data.displays = Loadable::Ready(without_laptop());
        for name in ["HDMI-A-1", "DVI-I-1"] {
            a.open_panel(name, Instant::now());
            assert_eq!(a.panel_preview(&panel(&a)), Err("no change".into()), "{name}");
        }
        // A real adjustment only moves the adjusted screen.
        a.open_panel("HDMI-A-1", Instant::now());
        let mut p = panel(&a);
        p.scale = Scale(1250);
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(plan.steps.len(), 1, "only Bravo changes: {plan:?}");
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y)), Some((2133, 0)));
        // Alpha adjusted, Bravo to its right: Bravo follows, Alpha stays at 213, 0.
        a.open_panel("DVI-I-1", Instant::now());
        let mut p = panel(&a);
        p.scale = Scale(1250);
        let plan = a.panel_preview(&p).unwrap();
        let at = |o: &str| plan.steps.iter().find(|s| s.output == o).and_then(|s| s.active.as_ref().map(|a| (a.x, a.y)));
        assert_eq!((at("DVI-I-1"), at("HDMI-A-1")), (Some((213, 0)), Some((1749, 0))));
        // Panel alone, at 0, 1080.
        a.data.displays = Loadable::Ready(vec![machine()[0].clone()]);
        a.open_panel("eDP-1", Instant::now());
        assert_eq!(a.panel_preview(&panel(&a)), Err("no change".into()));
    }

    #[test]
    fn never_a_negative_coordinate_after_anchoring() {
        let at = |plan: &Plan, o: &str| plan.steps.iter().find(|s| s.output == o).and_then(|s| s.active.as_ref().map(|a| (a.x, a.y)));
        // Laptop off, Bravo to the left of Alpha (213, 0): everything slides right.
        let mut a = displays_tab();
        a.data.displays = Loadable::Ready(without_laptop());
        a.open_panel("HDMI-A-1", Instant::now());
        let mut p = panel(&a);
        p.place = PlaceChoice::Rel(Relation::LeftOf);
        p.reference = Some(ALPHA.into());
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!((at(&plan, "HDMI-A-1"), at(&plan, "DVI-I-1")), (Some((0, 0)), Some((1920, 0))), "{plan:?}");
        // Full machine, laptop above Alpha: everything moves down, x unchanged.
        let mut a = displays_tab();
        a.open_panel("eDP-1", Instant::now());
        let mut p = panel(&a);
        p.place = PlaceChoice::Rel(Relation::Above);
        p.reference = Some(ALPHA.into());
        p.align = Align::Start;
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(
            (at(&plan, "eDP-1"), at(&plan, "DVI-I-1"), at(&plan, "HDMI-A-1")),
            (Some((213, 0)), Some((213, 1600)), Some((2133, 1600))),
            "{plan:?}"
        );
        // Everything stays positive: only the adjusted screen moves.
        p.place = PlaceChoice::Rel(Relation::Below);
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(plan.steps.len(), 1, "{plan:?}");
        assert_eq!(at(&plan, "eDP-1"), Some((213, 1080)));
    }

    // ── Save current ─────────────────────────────────────────────────

    fn tmp(name: &str) -> alpheus_commun::temp_path::TempPath {
        alpheus_commun::temp_path::TempPath::new(&format!("app-displays-{name}")).below("displays.toml")
    }

    #[test]
    fn saving_a_new_profile() {
        let mut a = displays_tab();
        let path = tmp("new");
        a.displays.profiles_path = Some(path.clone());
        a.displays.profile_sel = 2; // "+ Save current"
        a.profile_bar_activate(Instant::now());
        assert_eq!(a.overlay, Overlay::SaveProfile(SaveDialog::default()));
        a.overlay = Overlay::SaveProfile(SaveDialog { name: " Maison ".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.displays.profiles.len(), 3);
        assert_eq!(a.displays.profiles[2], from_outputs("Maison", &machine()));
        assert_eq!(profiles::load(&path), (a.displays.profiles.clone(), None), "written to disk");
        assert!(a.toasts.iter().any(|t| t.text == "Profile \"Maison\" saved"));
    }

    #[test]
    fn replace_after_confirmation() {
        let mut a = displays_tab();
        let path = tmp("replace");
        a.displays.profiles_path = Some(path.clone());
        let before = a.displays.profiles.clone();
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.overlay, Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), replace: true, yes_focused: false }));
        a.save_enter(Instant::now()); // focus on Cancel
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.displays.profiles, before, "nothing replaced");
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), replace: true, yes_focused: true });
        a.save_enter(Instant::now());
        assert_eq!(a.displays.profiles.len(), 2);
        assert_eq!(a.displays.profiles[1], from_outputs("Bureau", &machine()));
    }

    #[test]
    fn saving_impossible_is_reported() {
        let mut a = displays_tab();
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "X".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Profile \"X\" not saved: no configuration directory"));
        assert_eq!(a.displays.profiles.len(), 2);
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "   ".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert!(matches!(a.overlay, Overlay::SaveProfile(_)), "empty name: nothing happens");
    }

    #[test]
    fn saving_refused_if_the_file_is_invalid() {
        // Task 5 (controller re-check): the file loaded was invalid
        // (warning kept) → never overwrite what we couldn't read.
        let mut a = displays_tab();
        let path = tmp("invalid");
        a.displays.profiles_path = Some(path.clone());
        a.displays.profiles_invalid = true;
        let before = a.displays.profiles.clone();
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Maison".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("invalid displays.toml: fix it before saving"));
        assert_eq!(a.displays.profiles, before, "nothing replaced");
        assert!(!path.exists(), "nothing written");
    }

    #[test]
    fn placement_labels_text() {
        assert_eq!(placement_labels(&machine()), vec!["position 0, 1080", "origin", "right of Alpha"]);
        let outs = vec![on("DP-1", ALPHA, 1920, 1080, 0, 0), on("DP-2", BRAVO, 800, 600, 560, 1080), off("DP-3", "Charlie 3")];
        assert_eq!(placement_labels(&outs), vec!["origin", "below Alpha, centered", ""]);
    }
}
