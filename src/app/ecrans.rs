//! Onglet Écrans : profils, bascule d'une sortie, panneau de réglage,
//! enregistrement de l'état courant, et l'essai « Garder ces réglages ? »
//! avec retour automatique. Tout réglage passe par un essai : plan envoyé
//! au fil avec son plan inverse, compte à rebours de 15 s une fois appliqué.

use super::worker::Op;
use super::{App, Overlay};
use crate::backends::BResult;
use crate::displays::geometry::{deduce, logical_size, resolve, Align, GeoError, Placement, Relation, Screen};
use crate::displays::profiles::{self, from_outputs, placed, plan_profile, Profile};
use crate::displays::{
    active_count, changed_elsewhere, rotation_label, short_from_description, short_names, Active, Mode, Output, Plan,
    Scale, Step,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Délai avant le retour automatique aux réglages précédents.
pub const KEEP_DELAY: Duration = Duration::from_secs(15);

/// État de l'onglet hors données lues.
#[derive(Debug, Default)]
pub struct Ecrans {
    pub profiles: Vec<Profile>,
    /// ecrans.toml ; `None` : enregistrement impossible (tests).
    pub profiles_path: Option<PathBuf>,
    /// Le fichier chargé au démarrage était invalide (avertissement
    /// conservé) : enregistrer l'écraserait sans l'avoir su lire, refusé
    /// tant que Mae ne l'a pas corrigé (posé par `build_app`, tâche 11).
    pub profiles_invalid: bool,
    /// Puce choisie dans la barre des profils ; `profiles.len()` désigne
    /// « + Enregistrer l'actuel ».
    pub profile_sel: usize,
    pub trial: Option<Trial>,
    /// Retour envoyé et sans réponse : à refaire si alpheus se ferme.
    pub undoing: Option<Plan>,
    /// Profil demandé par la palette : appliqué à la prochaine lecture.
    pub queued_profile: Option<String>,
    /// Un plan a répondu et aucune lecture fraîche n'est encore arrivée :
    /// les sorties connues peuvent être celles d'un essai rejeté, on ne
    /// planifie rien dessus.
    pub awaiting_read: bool,
    /// Session niri : l'onglet n'appelle rien.
    pub niri: bool,
}

/// Un essai en cours : appliqué (ou en train de l'être), pas encore gardé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trial {
    pub title: String,
    pub plan: Plan,
    pub undo: Plan,
    /// Sorties lues juste avant (détection d'un changement extérieur).
    pub before: Vec<Output>,
    pub summary: Vec<String>,
    /// « Bravo absent, ignoré ».
    pub notes: Vec<String>,
    /// Le plan touche une sortie DisplayLink.
    pub displaylink: bool,
    /// Fin du compte à rebours ; `None` tant que le plan n'a pas répondu.
    pub deadline: Option<Instant>,
    /// Secondes restantes affichées.
    pub remaining: u64,
}

/// Boîte « Garder ces réglages ? » ; le focus démarre sur Annuler.
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

/// Placement choisi dans le panneau. `Free` : position actuelle, non
/// reproductible par une relation (proposée seulement dans ce cas).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceChoice {
    Free,
    Origin,
    Rel(Relation),
}

/// Panneau de réglage d'une sortie : brouillon envoyé par « Appliquer ».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    /// Connecteur de la sortie réglée.
    pub output: String,
    pub field: PanelField,
    pub mode: Mode,
    pub scale: Scale,
    pub transform: u8,
    pub place: PlaceChoice,
    /// Description de l'écran de référence.
    pub reference: Option<String>,
    pub align: Align,
    /// Position actuelle (décalage par rapport à l'origine déduite).
    pub free: Option<(i32, i32)>,
    /// Recherche de mode en cours (« / »).
    pub query: Option<String>,
}

/// « Enregistrer l'actuel » : saisie du nom, puis confirmation s'il existe.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SaveDialog {
    pub name: String,
    pub replace: bool,
    pub yes_focused: bool,
}

pub fn place_label(p: PlaceChoice) -> &'static str {
    match p {
        PlaceChoice::Free => "position actuelle",
        PlaceChoice::Origin => "origine",
        PlaceChoice::Rel(r) => r.label(),
    }
}

/// Champs visibles du panneau, dans l'ordre.
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

/// Placement de chaque sortie, en clair, pour la liste : « origine », « à
/// droite de Alpha », « en dessous de Alpha, centré », « position 0, 1080 ».
pub fn placement_labels(outs: &[Output]) -> Vec<String> {
    let labels = short_names(outs);
    deduce(&placed(outs), None, None)
        .into_iter()
        .zip(outs)
        .map(|(p, o)| match (&o.active, p) {
            (None, _) => String::new(),
            (Some(_), Placement::Origin) => "origine".into(),
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

/// Écran le plus à droite (bord droit maximal) parmi les activés.
fn rightmost(outs: &[Output], except: usize) -> Option<usize> {
    outs.iter()
        .enumerate()
        .filter(|(j, _)| *j != except)
        .filter_map(|(j, o)| o.active.as_ref().map(|a| (j, a.x + logical_size(a.mode, a.scale, a.transform).0)))
        .max_by_key(|&(_, right)| right)
        .map(|(j, _)| j)
}

/// Écrans pour la géométrie : chacun à sa place actuelle (absolue).
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

/// Réglages visés : la sortie `i` reçoit `own` ; les autres activées gardent
/// leur mode, échelle et rotation, à leur nouvelle position.
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

/// Activer la sortie `i` : son premier mode (le préféré de Hyprland),
/// échelle 1, à droite de l'écran le plus à droite, aligné en haut ; les
/// autres ne bougent pas.
fn enable_plan(outs: &[Output], i: usize) -> Result<Plan, String> {
    let mode = outs[i].modes.first().copied().ok_or("aucun mode connu")?;
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

/// `j` est-il placé, de proche en proche, contre l'écran `i` ?
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

/// `resolve` recale l'ensemble en (0, 0) ; sur un écran en direct, on
/// retranslate pour que rien ne bouge sans raison : un écran activé non
/// réglé, qui ne suit pas la sortie réglée `i`, garde sa position actuelle ;
/// s'il n'y en a aucun, c'est la sortie réglée qui garde la sienne.
fn kept_in_place(outs: &[Output], screens: &[Screen], mut pos: Vec<Option<(i32, i32)>>, i: usize) -> Vec<Option<(i32, i32)>> {
    let now_at = |j: usize| outs[j].active.as_ref().map(|a| (a.x, a.y)).zip(pos[j]);
    let anchor = (0..outs.len())
        .filter(|&j| j != i && !follows(screens, j, i))
        .find_map(now_at)
        .or_else(|| now_at(i));
    if let Some(((cx, cy), (nx, ny))) = anchor {
        for p in pos.iter_mut().flatten() {
            *p = (p.0 + cx - nx, p.1 + cy - ny);
        }
    }
    pos
}

/// Ligne du résumé d'un essai.
fn describe_step(s: &Step, outs: &[Output], labels: &[String]) -> String {
    let label = outs.iter().position(|o| o.name == s.output).map_or_else(|| s.output.clone(), |i| labels[i].clone());
    match &s.active {
        None => format!("{label} : désactivé"),
        Some(a) => format!(
            "{label} : {}, échelle {}, {}, position {}, {}",
            a.mode.label(),
            a.scale.label(),
            rotation_label(a.transform),
            a.x,
            a.y
        ),
    }
}

fn wrap(idx: usize, dir: i32, len: usize) -> usize {
    (idx as i32 + dir).rem_euclid(len.max(1) as i32) as usize
}

impl App {
    /// Sorties lues (vide tant que rien n'est lu).
    pub fn outputs(&self) -> &[Output] {
        self.data.displays.ready().map_or(&[], Vec::as_slice)
    }

    /// Plan à défaire si alpheus se ferme maintenant (essai ou retour en cours).
    pub fn exit_undo(&self) -> Option<Plan> {
        self.ecrans.trial.as_ref().map(|t| t.undo.clone()).or_else(|| self.ecrans.undoing.clone())
    }

    /// Un changement d'écran est-il en cours ? (Prévient par un message.)
    fn displays_busy(&mut self, now: Instant) -> bool {
        let busy = self.ecrans.trial.is_some() || self.ecrans.undoing.is_some() || self.ecrans.awaiting_read;
        if busy {
            self.toast("Écrans : un changement est déjà en cours".into(), true, now);
        }
        busy
    }

    fn close_keep(&mut self) {
        if matches!(self.overlay, Overlay::Keep(_)) {
            self.overlay = Overlay::None;
        }
    }

    // ── Réponses et temps ────────────────────────────────────────────

    /// Après une lecture des sorties : changement extérieur pendant un essai,
    /// profil demandé par la palette, panneau d'une sortie débranchée.
    pub(crate) fn displays_read(&mut self, now: Instant) {
        let outs = self.outputs().to_vec();
        if self.data.displays.ready().is_none() {
            // Lecture échouée : un profil en file ne s'appliquera pas bien plus tard.
            self.ecrans.queued_profile = None;
            return;
        }
        // Lecture postérieure à toute réponse de plan (les lectures en vol
        // pendant un plan sont écartées) : base fiable pour la suite.
        self.ecrans.awaiting_read = false;
        if let Some(t) = &self.ecrans.trial {
            if t.deadline.is_some() && changed_elsewhere(&t.before, &t.plan, &outs) {
                self.ecrans.trial = None;
                self.close_keep();
                self.status.last = Some("Écrans modifiés ailleurs".into());
                self.toast("Écrans modifiés ailleurs".into(), false, now);
            }
        }
        if let Overlay::Panel(p) = &self.overlay {
            if !outs.iter().any(|o| o.name == p.output) {
                let msg = format!("Écrans : {} débranché, réglage abandonné", p.output);
                self.overlay = Overlay::None;
                self.toast(msg, true, now);
            }
        }
        self.ecrans.profile_sel = self.ecrans.profile_sel.min(self.ecrans.profiles.len());
        if let Some(name) = self.ecrans.queued_profile.take() {
            self.apply_display_profile(&name, now);
        }
    }

    /// Réponse du fil à un plan d'écrans.
    pub(crate) fn displays_done(&mut self, op: Op, r: BResult<()>, now: Instant) {
        let Op::ApplyDisplays { plan, undo } = op else { return };
        self.ecrans.awaiting_read = true;
        match (undo, r) {
            (Some(_), Ok(())) => {
                if let Some(t) = &mut self.ecrans.trial {
                    if t.plan == plan && t.deadline.is_none() {
                        t.deadline = Some(now + KEEP_DELAY);
                        t.remaining = KEEP_DELAY.as_secs();
                    }
                }
                self.refresh_tab(now);
            }
            (Some(u), Err(e)) => {
                // Jamais d'état à moitié appliqué : retour aussitôt.
                if self.ecrans.trial.as_ref().is_some_and(|t| t.plan == plan) {
                    self.ecrans.trial = None;
                    self.close_keep();
                }
                let msg = format!("Écrans : {e} — retour aux réglages précédents");
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
                self.start_undo(u);
            }
            (None, Ok(())) => {
                self.ecrans.undoing = None;
                self.status.last = Some("Écrans : réglages précédents rétablis".into());
                self.toast("Écrans : réglages précédents rétablis".into(), false, now);
                self.refresh_tab(now);
            }
            (None, Err(e)) => {
                self.ecrans.undoing = None;
                let msg = format!("Écrans : retour impossible : {e}");
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
                self.refresh_tab(now);
            }
        }
    }

    /// Compte à rebours : secondes restantes, retour à l'échéance.
    pub(crate) fn displays_tick(&mut self, now: Instant) {
        let Some(t) = &mut self.ecrans.trial else { return };
        let Some(deadline) = t.deadline else { return };
        if now < deadline {
            t.remaining = (deadline - now).as_millis().div_ceil(1000) as u64;
            return;
        }
        if let Some(t) = self.ecrans.trial.take() {
            self.close_keep();
            self.toast("Écrans : délai écoulé, retour aux réglages précédents".into(), false, now);
            self.start_undo(t.undo);
        }
    }

    // ── Essai ────────────────────────────────────────────────────────

    /// Envoie `plan` avec son inverse et ouvre « Garder ces réglages ? ».
    pub(crate) fn start_trial(&mut self, title: String, plan: Plan, notes: Vec<String>, now: Instant) {
        if plan.steps.is_empty() {
            self.toast(format!("{title} : rien à changer"), false, now);
            return;
        }
        let Some(before) = self.data.displays.ready().cloned() else {
            self.toast("Écrans : état inconnu".into(), true, now);
            return;
        };
        let labels = short_names(&before);
        let summary = plan.steps.iter().map(|s| describe_step(s, &before, &labels)).collect();
        let displaylink = plan.steps.iter().any(|s| before.iter().any(|o| o.name == s.output && o.displaylink));
        let undo = Plan::undo(&before, &plan);
        self.ecrans.trial = Some(Trial {
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
        self.ecrans.undoing = Some(plan.clone());
        self.send_op(Op::ApplyDisplays { plan, undo: None });
    }

    /// Garder (seulement une fois appliqué) ou Annuler (retour).
    pub(crate) fn keep_answer(&mut self, keep: bool, now: Instant) {
        if keep {
            if self.ecrans.trial.as_ref().is_some_and(|t| t.deadline.is_some()) {
                self.ecrans.trial = None;
                self.close_keep();
                self.status.last = Some("Écrans : réglages gardés".into());
                self.toast("Écrans : réglages gardés".into(), false, now);
            }
        } else if let Some(t) = self.ecrans.trial.take() {
            self.close_keep();
            self.start_undo(t.undo);
        }
    }

    // ── Profils et liste ─────────────────────────────────────────────

    pub(crate) fn apply_display_profile(&mut self, name: &str, now: Instant) {
        if self.ecrans.niri || self.displays_busy(now) {
            return;
        }
        let Some(outs) = self.data.displays.ready() else {
            self.toast("Écrans : état inconnu".into(), true, now);
            return;
        };
        let Some(p) = self.ecrans.profiles.iter().find(|p| p.name == name) else {
            self.toast(format!("Profil d'écran « {name} » introuvable"), true, now);
            return;
        };
        match plan_profile(p, outs) {
            Err(e) => {
                let msg = format!("Profil « {name} » refusé : {e}");
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
            }
            Ok(pp) => {
                let notes = pp.absent.iter().map(|a| format!("{a} absent, ignoré")).collect();
                self.start_trial(format!("Profil « {name} »"), pp.plan, notes, now);
            }
        }
    }

    /// Espace sur une sortie : l'activer, ou la désactiver (jamais la dernière).
    pub(crate) fn toggle_output(&mut self, name: &str, now: Instant) {
        if self.ecrans.niri || self.displays_busy(now) {
            return;
        }
        let outs = self.outputs().to_vec();
        let Some(i) = outs.iter().position(|o| o.name == name) else { return };
        let label = short_names(&outs)[i].clone();
        if outs[i].active.is_some() {
            if active_count(&outs) <= 1 {
                let msg = format!("Impossible de désactiver {label} : dernier écran actif");
                self.toast(msg, true, now);
                return;
            }
            let plan = Plan { steps: vec![Step { output: name.to_string(), active: None }] };
            self.start_trial(format!("{label} désactivé"), plan, Vec::new(), now);
        } else {
            match enable_plan(&outs, i) {
                Ok(plan) => self.start_trial(format!("{label} activé"), plan, Vec::new(), now),
                Err(e) => self.toast(format!("{label} : {e}"), true, now),
            }
        }
    }

    pub(crate) fn profile_bar_move(&mut self, dir: i32) {
        let n = self.ecrans.profiles.len() as i32;
        self.ecrans.profile_sel = (self.ecrans.profile_sel as i32 + dir).clamp(0, n) as usize;
    }

    /// Entrée sur la barre des profils : appliquer, ou « + Enregistrer l'actuel ».
    pub(crate) fn profile_bar_activate(&mut self, now: Instant) {
        match self.ecrans.profiles.get(self.ecrans.profile_sel).map(|p| p.name.clone()) {
            Some(name) => self.apply_display_profile(&name, now),
            None => self.open_save(now),
        }
    }

    // ── Panneau de réglage ───────────────────────────────────────────

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
                let msg = format!("{} : aucun mode connu", short_names(&outs)[i]);
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
            // Éteinte : proposée à droite de l'écran le plus à droite.
            p.place = PlaceChoice::Rel(Relation::RightOf);
            p.reference = Some(outs[j].description.clone());
        }
        self.overlay = Overlay::Panel(p);
    }

    /// Modes proposés : ceux de la sortie, filtrés par la recherche.
    pub fn panel_modes(&self, p: &Panel) -> Vec<Mode> {
        let Some(o) = self.outputs().iter().find(|o| o.name == p.output) else { return Vec::new() };
        let mut modes = o.modes.clone();
        if !modes.contains(&p.mode) {
            modes.insert(0, p.mode);
        }
        // Sous-chaîne, pas recherche floue : « 1280 » ne doit pas trouver « 1920x1080 ».
        let q = p.query.as_deref().unwrap_or("").trim().replace('×', "x");
        modes.into_iter().filter(|m| m.to_string().contains(&q)).collect()
    }

    /// Références possibles : les autres écrans activés (descriptions).
    pub fn panel_references(&self, p: &Panel) -> Vec<String> {
        self.outputs()
            .iter()
            .filter(|o| o.name != p.output && o.active.is_some())
            .map(|o| o.description.clone())
            .collect()
    }

    /// Plan du brouillon, ou la raison du refus (Appliquer grisé).
    pub fn panel_preview(&self, p: &Panel) -> Result<Plan, String> {
        let outs = self.outputs();
        let i = outs.iter().position(|o| o.name == p.output).ok_or("sortie débranchée")?;
        let labels = short_names(outs);
        let current = placed(outs);
        let own = match p.place {
            PlaceChoice::Origin => Placement::Origin,
            PlaceChoice::Free => {
                let (x, y) = p.free.ok_or("position inconnue")?;
                Placement::Absolute { x, y }
            }
            PlaceChoice::Rel(relation) => Placement::Relative {
                relation,
                reference: p.reference.clone().ok_or("choisir un écran de référence")?,
                align: p.align,
            },
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
                // Éteinte : l'ancienne origine garde sa place (0, 0).
                for (j, pl) in placements.iter_mut().enumerate() {
                    if j != i && *pl == Placement::Origin {
                        *pl = Placement::Absolute { x: 0, y: 0 };
                    }
                }
            }
            solve(screens(placements))
        } else {
            match solve(screens(deduce(&current, None, None))) {
                // Les autres dépendaient de cet écran : ils gardent leur place.
                Err(GeoError::Cycle(_)) => solve(screens(deduce(&current, None, Some(i)))),
                r => r,
            }
        }
        .map_err(|e| e.to_string())?;
        let pos = kept_in_place(outs, &sc, pos, i);
        let plan = Plan::between(outs, &targets(outs, pos, i, (p.mode, p.scale, p.transform)));
        if plan.steps.is_empty() { Err("aucun changement".into()) } else { Ok(plan) }
    }

    fn panel_mut(&mut self) -> Option<&mut Panel> {
        match &mut self.overlay {
            Overlay::Panel(p) => Some(p),
            _ => None,
        }
    }

    /// Champ précédent (`dir` < 0) ou suivant.
    pub(crate) fn panel_move(&mut self, dir: i32) {
        if let Some(p) = self.panel_mut() {
            let fields = panel_fields(p);
            let k = fields.iter().position(|f| *f == p.field).unwrap_or(0) as i32;
            p.field = fields[(k + dir).clamp(0, fields.len() as i32 - 1) as usize];
        }
    }

    /// Change la valeur du champ focalisé (←/→, -/+).
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

    /// Recherche de mode : le texte change, le mode suit la première correspondance.
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

    /// Entrée dans le panneau : Annuler ferme, Appliquer envoie, un champ
    /// mène à Appliquer.
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
                self.start_trial(format!("Réglage de {label}"), plan, Vec::new(), now);
            }
            Err(e) => self.toast(format!("Écrans : {e}"), true, now),
        }
    }

    // ── Enregistrer l'actuel ─────────────────────────────────────────

    pub(crate) fn open_save(&mut self, now: Instant) {
        if self.data.displays.ready().is_none() {
            self.toast("Écrans : état inconnu".into(), true, now);
            return;
        }
        self.overlay = Overlay::SaveProfile(SaveDialog::default());
    }

    /// Entrée dans la saisie du nom : demande confirmation si le profil
    /// existe, sinon enregistre.
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
        if self.ecrans.profiles.iter().any(|p| p.name == name) {
            self.overlay = Overlay::SaveProfile(SaveDialog { name, replace: true, yes_focused: false });
        } else {
            self.save_current(&name, now);
        }
    }

    /// Réponse à « Remplacer le profil ? » : oui enregistre, non ferme.
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
        if self.ecrans.profiles_invalid {
            // Relecture contrôleur (tâche 5) : le fichier chargé était
            // invalide — jamais écraser ce qu'on n'a pas su lire.
            let msg = "ecrans.toml invalide : corrige-le avant d'enregistrer".to_string();
            self.status.error = Some(msg.clone());
            self.toast(msg, true, now);
            return;
        }
        let Some(outs) = self.data.displays.ready() else {
            self.toast("Écrans : état inconnu".into(), true, now);
            return;
        };
        let p = from_outputs(name, outs);
        let mut list = self.ecrans.profiles.clone();
        match list.iter().position(|x| x.name == name) {
            Some(k) => list[k] = p,
            None => list.push(p),
        }
        let r = match &self.ecrans.profiles_path {
            Some(path) => profiles::save(path, &list),
            None => Err("aucun dossier de configuration".into()),
        };
        match r {
            Ok(()) => {
                self.ecrans.profiles = list;
                let msg = format!("Profil « {name} » enregistré");
                self.status.last = Some(msg.clone());
                self.toast(msg, false, now);
            }
            Err(e) => {
                let msg = format!("Profil « {name} » non enregistré : {e}");
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

    /// Onglet Écrans ouvert, données prêtes, boîte aux requêtes vidée.
    fn ecrans() -> App {
        let mut a = ready_app();
        a.go_tab(Tab::Ecrans, Instant::now());
        a.take_requests();
        a
    }

    /// L'essai envoyé : (plan, plan inverse).
    fn sent_trial(a: &mut App) -> (Plan, Plan) {
        match a.take_requests().as_slice() {
            [Request::Do(Op::ApplyDisplays { plan, undo: Some(u) })] => (plan.clone(), u.clone()),
            r => panic!("essai attendu : {r:?}"),
        }
    }

    fn sent_undo(a: &mut App) -> Plan {
        let r = a.take_requests();
        r.iter()
            .find_map(|q| match q {
                Request::Do(Op::ApplyDisplays { plan, undo: None }) => Some(plan.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("retour attendu : {r:?}"))
    }

    /// Essai « Bureau » envoyé et appliqué à `t0` : compte à rebours lancé.
    fn applied(t0: Instant) -> (App, Plan, Plan) {
        let mut a = ecrans();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: plan.clone(), undo: Some(undo.clone()) }, Ok(())), t0);
        a.take_requests();
        (a, plan, undo)
    }

    /// Les sorties telles que `plan` les laisse (relecture conforme).
    fn relu(plan: &Plan) -> Vec<Output> {
        let mut outs = poste();
        for s in &plan.steps {
            let o = outs.iter_mut().find(|o| o.name == s.output).unwrap();
            o.active = s.active.clone();
        }
        outs
    }

    fn sans_do(a: &mut App) -> bool {
        !a.take_requests().iter().any(|r| matches!(r, Request::Do(_)))
    }

    #[test]
    fn verrou_jusqu_a_une_relecture_fraiche() {
        let t0 = Instant::now();
        let (mut a, plan, undo) = applied(t0);
        // Lecture prise pendant l'essai : elle montre les réglages essayés.
        a.on_update(Update::Displays(Ok(relu(&plan))), t0);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo);
        a.on_update(Update::Done(Op::ApplyDisplays { plan: undo.clone(), undo: None }, Ok(())), t0);
        assert_eq!(a.ecrans.undoing, None);
        // Les données sont encore celles de l'essai rejeté : rien ne doit partir.
        a.toggle_output("HDMI-A-1", t0);
        a.apply_display_profile("Laptop seul", t0);
        assert!(sans_do(&mut a), "pas de réglage sur une lecture périmée");
        assert_eq!(a.ecrans.trial, None);
        assert!(a.toasts.iter().any(|t| t.text == "Écrans : un changement est déjà en cours"));
        // Relecture fraîche : l'état rétabli sert de base, et de retour.
        a.on_update(Update::Displays(Ok(poste())), t0);
        a.toggle_output("HDMI-A-1", t0);
        let (p, u) = sent_trial(&mut a);
        assert_eq!(u, Plan::undo(&poste(), &p));
        assert_eq!(a.ecrans.trial.as_ref().unwrap().before, poste());

        // Garder avant toute relecture : même verrou.
        let (mut a, _, _) = applied(t0);
        a.keep_answer(true, t0);
        assert_eq!(a.ecrans.trial, None);
        a.toggle_output("HDMI-A-1", t0);
        assert!(sans_do(&mut a), "gardé mais pas encore relu");
        a.on_update(Update::Displays(Ok(relu(&plan))), t0);
        a.toggle_output("HDMI-A-1", t0);
        let (_, u) = sent_trial(&mut a);
        assert_eq!(u, Plan::undo(&relu(&plan), &Plan { steps: vec![Step { output: "HDMI-A-1".into(), active: None }] }));
    }

    #[test]
    fn annuler_avant_la_reponse_puis_succes() {
        let t0 = Instant::now();
        let mut a = ecrans();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo, "Annuler pendant l'application : retour aussitôt");
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo.clone()) }, Ok(())), t0);
        assert_eq!((a.ecrans.trial.clone(), a.overlay.clone()), (None, Overlay::None), "l'essai ne renaît pas");
        assert_eq!(a.exit_undo(), Some(undo.clone()), "le retour en vol reste à refaire");
        a.on_tick(later(t0, 20_000));
        assert!(sans_do(&mut a), "pas de second retour à l'échéance");
        a.on_update(Update::Done(Op::ApplyDisplays { plan: undo, undo: None }, Ok(())), t0);
        assert_eq!(a.exit_undo(), None);
        a.on_update(Update::Displays(Ok(poste())), t0);
        a.take_requests();
        a.toggle_output("HDMI-A-1", t0);
        sent_trial(&mut a);
    }

    #[test]
    fn annuler_avant_la_reponse_puis_echec() {
        let t0 = Instant::now();
        let mut a = ecrans();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo);
        let e = BackendError::Timeout { tool: "wm-set".into() };
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo.clone()) }, Err(e)), t0);
        assert_eq!(sent_undo(&mut a), undo, "second retour envoyé");
        assert_eq!(a.ecrans.trial, None);
        for _ in 0..2 {
            a.on_update(Update::Done(Op::ApplyDisplays { plan: undo.clone(), undo: None }, Ok(())), t0);
        }
        assert_eq!(a.ecrans.undoing, None);
        assert_eq!(a.exit_undo(), None);
        a.on_update(Update::Displays(Ok(poste())), t0);
        a.take_requests();
        a.toggle_output("HDMI-A-1", t0);
        sent_trial(&mut a);
    }

    #[test]
    fn elements_de_l_onglet() {
        let mut a = ready_app();
        a.go_tab(Tab::Ecrans, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])]);
        assert_eq!(a.items(), vec![
            Item::DisplayProfiles,
            Item::Output("eDP-1".into()),
            Item::Output("DVI-I-1".into()),
            Item::Output("HDMI-A-1".into()),
        ]);
    }

    #[test]
    fn sous_niri_l_onglet_n_appelle_rien() {
        let mut a = ready_app();
        a.ecrans.niri = true;
        a.go_tab(Tab::Ecrans, Instant::now());
        assert!(a.take_requests().is_empty());
        assert!(a.items().is_empty());
        a.on_tick(Instant::now() + Duration::from_secs(30));
        assert!(a.take_requests().is_empty(), "ni au tic");
        assert!(!a.palette_matches().iter().any(|(l, _)| l.starts_with("Profil d'écran")));
        a.run_command(Command::ApplyDisplayProfile("Bureau".into()), Instant::now());
        assert_eq!(a.ecrans.queued_profile, None);
        a.take_requests();
        a.toggle_output("HDMI-A-1", Instant::now());
        assert!(a.take_requests().is_empty(), "ni la bascule d'une sortie");
        assert_eq!(a.ecrans.trial, None);
    }

    #[test]
    fn profil_applique_avec_compte_a_rebours() {
        let mut a = ecrans();
        let t0 = Instant::now();
        a.ecrans.profile_sel = 1; // Bureau
        a.profile_bar_activate(t0);
        let (plan, undo) = sent_trial(&mut a);
        assert_eq!(plan.steps.iter().map(|s| s.output.as_str()).collect::<Vec<_>>(), vec!["DVI-I-1", "HDMI-A-1", "eDP-1"]);
        assert_eq!(undo, Plan::undo(&poste(), &plan));
        assert_eq!(a.overlay, Overlay::Keep(Keep { keep_focused: false }), "focus sur Annuler");
        let t = a.ecrans.trial.clone().unwrap();
        assert_eq!(t.title, "Profil « Bureau »");
        assert_eq!(t.deadline, None, "pas de compte à rebours avant la réponse");
        assert_eq!(t.summary[0], "Alpha : 1920×1080@60, échelle 1.0, 0°, position 0, 0");
        assert_eq!(t.summary[2], "Portable : désactivé");
        assert_eq!(a.exit_undo(), Some(undo.clone()), "fermeture pendant l'application : retour");
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo) }, Ok(())), t0);
        let t = a.ecrans.trial.clone().unwrap();
        assert_eq!((t.deadline, t.remaining), (Some(t0 + KEEP_DELAY), 15));
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])], "relecture après application");
    }

    #[test]
    fn retour_automatique_a_quinze_secondes() {
        let t0 = Instant::now();
        let (mut a, _, undo) = applied(t0);
        a.on_tick(later(t0, 5_200));
        assert_eq!(a.ecrans.trial.as_ref().unwrap().remaining, 10);
        assert!(a.take_requests().iter().all(|r| matches!(r, Request::Refresh(_))), "rien n'est défait avant 15 s");
        a.on_tick(later(t0, 15_000));
        assert_eq!(sent_undo(&mut a), undo);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.ecrans.trial, None);
        assert_eq!(a.exit_undo(), Some(undo.clone()), "retour sans réponse : refait à la fermeture");
        a.on_update(Update::Done(Op::ApplyDisplays { plan: undo, undo: None }, Ok(())), later(t0, 15_100));
        assert_eq!(a.exit_undo(), None);
        assert!(a.toasts.iter().any(|t| t.text == "Écrans : réglages précédents rétablis"));
    }

    #[test]
    fn annuler_et_garder() {
        let t0 = Instant::now();
        let (mut a, _, undo) = applied(t0);
        a.keep_answer(false, t0);
        assert_eq!(sent_undo(&mut a), undo, "Annuler : retour aussitôt");
        let (mut a, _, _) = applied(t0);
        a.keep_answer(true, t0);
        assert_eq!((a.ecrans.trial.clone(), a.overlay.clone()), (None, Overlay::None));
        assert!(a.take_requests().is_empty(), "Garder ne réapplique rien");
        assert_eq!(a.exit_undo(), None, "gardé : rien à défaire à la fermeture");
        a.on_tick(later(t0, 20_000));
        assert!(a.take_requests().iter().all(|r| matches!(r, Request::Refresh(_))));
    }

    #[test]
    fn garder_attend_l_application() {
        let mut a = ecrans();
        a.apply_display_profile("Bureau", Instant::now());
        a.take_requests();
        a.keep_answer(true, Instant::now());
        assert!(a.ecrans.trial.is_some(), "pas encore appliqué : Garder sans effet");
    }

    #[test]
    fn echec_en_cours_de_plan_retour_aussitot() {
        let mut a = ecrans();
        let t0 = Instant::now();
        a.apply_display_profile("Bureau", t0);
        let (plan, undo) = sent_trial(&mut a);
        let e = BackendError::Failed { tool: "wm-set".into(), code: Some(1), stderr: "Hyprland a refusé « eval … » : boum\n".into() };
        a.on_update(Update::Done(Op::ApplyDisplays { plan, undo: Some(undo.clone()) }, Err(e)), t0);
        assert_eq!(sent_undo(&mut a), undo);
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(
            a.status.error.as_deref(),
            Some("Écrans : wm-set a échoué (code 1) : Hyprland a refusé « eval … » : boum — retour aux réglages précédents")
        );
    }

    #[test]
    fn retour_impossible_signale() {
        let mut a = ecrans();
        let e = BackendError::Timeout { tool: "wm-set".into() };
        a.ecrans.undoing = Some(Plan::default());
        a.on_update(Update::Done(Op::ApplyDisplays { plan: Plan::default(), undo: None }, Err(e)), Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Écrans : retour impossible : wm-set ne répond pas"));
        assert_eq!(a.ecrans.undoing, None);
    }

    #[test]
    fn changement_exterieur_ferme_sans_rien_reappliquer() {
        let t0 = Instant::now();
        let (mut a, plan, _) = applied(t0);
        // Lecture conforme à l'essai : la boîte reste.
        let mut attendu = poste();
        for s in &plan.steps {
            let o = attendu.iter_mut().find(|o| o.name == s.output).unwrap();
            o.active = s.active.clone();
        }
        a.on_update(Update::Displays(Ok(attendu.clone())), t0);
        assert!(a.ecrans.trial.is_some());
        // Dock débranché : la boîte se ferme, aucun retour envoyé.
        attendu.retain(|o| o.name != "DVI-I-1");
        a.on_update(Update::Displays(Ok(attendu)), t0);
        assert_eq!(a.ecrans.trial, None);
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.toasts.iter().any(|t| t.text == "Écrans modifiés ailleurs"));
        assert!(!a.take_requests().iter().any(|r| matches!(r, Request::Do(_))), "rien n'est réappliqué");
    }

    #[test]
    fn aucun_autre_changement_pendant_l_essai() {
        let t0 = Instant::now();
        let (mut a, _, _) = applied(t0);
        a.toggle_output("eDP-1", t0);
        a.apply_display_profile("Laptop seul", t0);
        a.open_panel("eDP-1", t0);
        assert!(!a.take_requests().iter().any(|r| matches!(r, Request::Do(_))));
        assert_eq!(a.overlay, Overlay::Keep(Keep::default()));
        assert!(a.toasts.iter().any(|t| t.text == "Écrans : un changement est déjà en cours"));
    }

    #[test]
    fn avertissement_displaylink() {
        let mut a = ecrans();
        a.toggle_output("DVI-I-1", Instant::now()); // Alpha, sur le dock DisplayLink
        assert!(a.ecrans.trial.as_ref().unwrap().displaylink);
        let mut a = ecrans();
        a.toggle_output("HDMI-A-1", Instant::now());
        assert!(!a.ecrans.trial.as_ref().unwrap().displaylink);
    }

    #[test]
    fn dernier_ecran_actif_protege() {
        let mut a = ecrans();
        a.data.displays = Loadable::Ready(vec![poste()[0].clone(), off("DVI-I-1", ALPHA), off("HDMI-A-1", BRAVO)]);
        a.toggle_output("eDP-1", Instant::now());
        assert!(a.take_requests().is_empty());
        assert!(a.toasts.iter().any(|t| t.text == "Impossible de désactiver Portable : dernier écran actif"));
        // Depuis un profil : Bureau loin du dock éteindrait la dalle, seule branchée.
        a.data.displays = Loadable::Ready(vec![poste()[0].clone()]);
        a.apply_display_profile("Bureau", Instant::now());
        assert!(a.take_requests().is_empty());
        assert_eq!(a.status.error.as_deref(), Some("Profil « Bureau » refusé : ce profil désactiverait le dernier écran actif"));
    }

    #[test]
    fn bascule_d_une_sortie() {
        let mut a = ecrans();
        a.toggle_output("eDP-1", Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert_eq!(plan.steps, vec![Step { output: "eDP-1".into(), active: None }], "les autres ne bougent pas");
        // Une sortie éteinte s'allume à droite de la plus à droite (Bravo finit à 4053).
        let mut a = ecrans();
        let mut outs = poste();
        outs.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
        a.data.displays = Loadable::Ready(outs);
        a.toggle_output("DP-3", Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert_eq!(plan.steps, vec![Step {
            output: "DP-3".into(),
            active: Some(Active { mode: mode(1920, 1080, 60), x: 4053, y: 0, scale: Scale::ONE, transform: 0 }),
        }]);
    }

    #[test]
    fn profil_de_la_palette_applique_a_la_prochaine_lecture() {
        let mut a = ready_app();
        a.run_command(Command::ApplyDisplayProfile("Bureau".into()), Instant::now());
        assert_eq!(a.tab, Tab::Ecrans);
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])], "lecture d'abord, rien d'appliqué");
        a.on_update(Update::Displays(Ok(poste())), Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert!(plan.touches("DVI-I-1"));
        assert_eq!(a.ecrans.queued_profile, None);
    }

    #[test]
    fn profil_en_file_abandonne_si_la_lecture_echoue() {
        let mut a = ready_app();
        a.run_command(Command::ApplyDisplayProfile("Bureau".into()), Instant::now());
        a.take_requests();
        a.on_update(Update::Displays(Err(BackendError::Missing { tool: "wm-set".into() })), Instant::now());
        assert_eq!(a.ecrans.queued_profile, None);
        // Une lecture réussie bien plus tard n'applique rien.
        a.on_update(Update::Displays(Ok(poste())), Instant::now());
        assert!(sans_do(&mut a), "profil abandonné avec la lecture échouée");
        assert_eq!(a.ecrans.trial, None);
    }

    #[test]
    fn ecran_du_profil_absent_signale() {
        let mut a = ecrans();
        a.data.displays = Loadable::Ready(vec![poste()[0].clone(), poste()[1].clone()]);
        a.apply_display_profile("Bureau", Instant::now());
        assert_eq!(a.ecrans.trial.as_ref().unwrap().notes, vec!["Bravo absent, ignoré"]);
    }

    #[test]
    fn profil_deja_en_place() {
        let mut a = ecrans();
        a.ecrans.profiles = vec![from_outputs("Ici", &poste())];
        a.apply_display_profile("Ici", Instant::now());
        assert!(a.take_requests().is_empty());
        assert_eq!(a.ecrans.trial, None);
        assert!(a.toasts.iter().any(|t| t.text == "Profil « Ici » : rien à changer"));
    }

    #[test]
    fn barre_des_profils_bornee() {
        let mut a = ready_app();
        a.go_tab(Tab::Ecrans, Instant::now());
        a.adjust(1, Instant::now());
        a.adjust(1, Instant::now());
        a.adjust(1, Instant::now());
        assert_eq!(a.ecrans.profile_sel, 2, "deux profils, puis « + Enregistrer l'actuel »");
        a.adjust(-1, Instant::now());
        assert_eq!(a.ecrans.profile_sel, 1);
        assert!(a.take_requests().iter().all(|r| matches!(r, Request::Refresh(_))), "choisir n'applique rien");
    }

    // ── Panneau ──────────────────────────────────────────────────────

    fn panel(a: &App) -> Panel {
        match &a.overlay {
            Overlay::Panel(p) => p.clone(),
            o => panic!("panneau attendu : {o:?}"),
        }
    }

    #[test]
    fn panneau_ouvert_sur_l_etat_courant() {
        let mut a = ecrans();
        a.open_panel("HDMI-A-1", Instant::now());
        let p = panel(&a);
        assert_eq!((p.mode, p.scale, p.transform), (mode(1920, 1080, 240), Scale::ONE, 0));
        assert_eq!((p.place, p.reference.as_deref(), p.align), (PlaceChoice::Rel(Relation::RightOf), Some(ALPHA), Align::Start));
        assert_eq!(a.panel_preview(&p), Err("aucun changement".into()));
        a.open_panel("eDP-1", Instant::now());
        let p = panel(&a);
        assert_eq!((p.place, p.free), (PlaceChoice::Free, Some((-213, 1080))), "position non reproductible : libre");
        assert_eq!(panel_fields(&p), vec![PanelField::Mode, PanelField::Scale, PanelField::Rotation, PanelField::Place, PanelField::Cancel, PanelField::Apply]);
    }

    #[test]
    fn echelle_de_l_origine_les_voisins_suivent() {
        let mut a = ecrans();
        a.open_panel("DVI-I-1", Instant::now());
        a.panel_move(1); // Échelle
        a.panel_step(1); // 1,25 : Alpha passe à 1536×864
        let plan = a.panel_preview(&panel(&a)).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].output, "DVI-I-1");
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y, a.scale)), Some((213, 0, Scale(1250))));
        assert_eq!(plan.steps[1].output, "HDMI-A-1");
        assert_eq!(plan.steps[1].active.as_ref().map(|a| (a.x, a.y)), Some((1749, 0)), "Bravo suit Alpha");
        a.panel_apply(Instant::now());
        assert_eq!(a.ecrans.trial.as_ref().unwrap().title, "Réglage de Alpha");
        assert_eq!(sent_trial(&mut a).0, plan);
    }

    #[test]
    fn cycle_evite_les_autres_gardent_leur_place() {
        // Alpha à droite de Bravo, alors que Bravo est à droite d'Alpha.
        let mut a = ecrans();
        a.open_panel("DVI-I-1", Instant::now());
        let mut p = panel(&a);
        p.place = PlaceChoice::Rel(Relation::RightOf);
        p.reference = Some(BRAVO.into());
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(plan.steps.len(), 1, "seul Alpha bouge : {plan:?}");
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y)), Some((4053, 0)));
    }

    #[test]
    fn placement_refuse_avec_raison() {
        let mut a = ecrans();
        a.open_panel("HDMI-A-1", Instant::now());
        let mut p = panel(&a);
        p.place = PlaceChoice::Rel(Relation::Below);
        a.overlay = Overlay::Panel(p.clone());
        assert_eq!(a.panel_preview(&p), Err("Portable et Bravo se chevauchent".into()));
        a.panel_apply(Instant::now());
        assert!(a.take_requests().is_empty(), "Appliquer grisé : rien n'est envoyé");
        assert!(matches!(a.overlay, Overlay::Panel(_)));
    }

    #[test]
    fn choix_du_placement_et_de_la_reference() {
        let mut a = ecrans();
        a.open_panel("eDP-1", Instant::now());
        a.panel_move(3); // Placement
        assert_eq!(panel(&a).field, PanelField::Place);
        a.panel_step(1); // libre → origine
        assert_eq!(panel(&a).place, PlaceChoice::Origin);
        a.panel_step(1); // → à droite de
        let p = panel(&a);
        assert_eq!((p.place, p.reference.as_deref()), (PlaceChoice::Rel(Relation::RightOf), Some(ALPHA)), "référence proposée");
        assert!(panel_fields(&p).contains(&PanelField::Reference));
        a.panel_move(1);
        a.panel_step(1);
        assert_eq!(panel(&a).reference.as_deref(), Some(BRAVO));
        a.panel_move(1);
        a.panel_step(-1);
        assert_eq!(panel(&a).align, Align::End, "cyclique");
        a.panel_step(1);
        a.panel_step(-1);
        a.panel_move(5);
        assert_eq!(panel(&a).field, PanelField::Apply, "borné au dernier champ");
    }

    #[test]
    fn rotation_et_echelle() {
        let mut a = ecrans();
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
    fn recherche_de_mode() {
        let mut a = ecrans();
        a.data.displays = Loadable::Ready(crate::backends::displays::parse_outputs(include_str!("../../tests/fixtures/wm-set-outputs.json")).unwrap());
        a.open_panel("DVI-I-1", Instant::now());
        assert_eq!(a.panel_modes(&panel(&a)).len(), 26);
        a.panel_query(|q| q.push_str("1280"));
        let p = panel(&a);
        assert_eq!(p.mode, mode(1280, 1024, 75), "première correspondance");
        assert!(a.panel_modes(&p).iter().all(|m| m.to_string().contains("1280")));
        assert_eq!(a.panel_modes(&p).len(), 5, "« 1280 » ne trouve pas « 1920x1080 »");
        a.panel_step(1);
        assert_eq!(panel(&a).mode, mode(1280, 1024, 70));
    }

    #[test]
    fn panneau_d_une_sortie_debranchee() {
        let mut a = ecrans();
        a.open_panel("HDMI-A-1", Instant::now());
        let p = panel(&a);
        let sans: Vec<Output> = poste().into_iter().filter(|o| o.name != "HDMI-A-1").collect();
        a.data.displays = Loadable::Ready(sans.clone());
        assert_eq!(a.panel_preview(&p), Err("sortie débranchée".into()));
        a.on_update(Update::Displays(Ok(sans)), Instant::now());
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.toasts.iter().any(|t| t.text == "Écrans : HDMI-A-1 débranché, réglage abandonné"));
    }

    #[test]
    fn sortie_eteinte_reglee_puis_allumee() {
        let mut a = ecrans();
        let mut outs = poste();
        outs.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
        a.data.displays = Loadable::Ready(outs);
        a.open_panel("DP-3", Instant::now());
        let p = panel(&a);
        assert_eq!((p.place, p.reference.as_deref()), (PlaceChoice::Rel(Relation::RightOf), Some(BRAVO)));
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y)), Some((4053, 0)));
    }

    /// Le poste, dalle interne éteinte : l'agencement commence en (213, 0).
    fn sans_portable() -> Vec<Output> {
        let mut outs = poste();
        outs[0] = off("eDP-1", INTERNE);
        outs
    }

    #[test]
    fn espace_sur_un_ecran_eteint_les_autres_ne_bougent_pas() {
        let mut a = ecrans();
        a.data.displays = Loadable::Ready(sans_portable());
        a.toggle_output("eDP-1", Instant::now());
        let (plan, _) = sent_trial(&mut a);
        assert_eq!(plan.steps, vec![Step {
            output: "eDP-1".into(),
            active: Some(Active { mode: mode(1920, 1080, 60), x: 4053, y: 0, scale: Scale::ONE, transform: 0 }),
        }], "Alpha et Bravo restent en 213 et 2133");
    }

    #[test]
    fn agencement_hors_origine_panneau_sans_changement() {
        let mut a = ecrans();
        a.data.displays = Loadable::Ready(sans_portable());
        for name in ["HDMI-A-1", "DVI-I-1"] {
            a.open_panel(name, Instant::now());
            assert_eq!(a.panel_preview(&panel(&a)), Err("aucun changement".into()), "{name}");
        }
        // Un vrai réglage ne déplace que l'écran réglé.
        a.open_panel("HDMI-A-1", Instant::now());
        let mut p = panel(&a);
        p.scale = Scale(1250);
        let plan = a.panel_preview(&p).unwrap();
        assert_eq!(plan.steps.len(), 1, "seul Bravo change : {plan:?}");
        assert_eq!(plan.steps[0].active.as_ref().map(|a| (a.x, a.y)), Some((2133, 0)));
        // Alpha réglée, Bravo à sa droite : Bravo suit, Alpha reste en 213, 0.
        a.open_panel("DVI-I-1", Instant::now());
        let mut p = panel(&a);
        p.scale = Scale(1250);
        let plan = a.panel_preview(&p).unwrap();
        let at = |o: &str| plan.steps.iter().find(|s| s.output == o).and_then(|s| s.active.as_ref().map(|a| (a.x, a.y)));
        assert_eq!((at("DVI-I-1"), at("HDMI-A-1")), (Some((213, 0)), Some((1749, 0))));
        // Dalle seule, en 0, 1080.
        a.data.displays = Loadable::Ready(vec![poste()[0].clone()]);
        a.open_panel("eDP-1", Instant::now());
        assert_eq!(a.panel_preview(&panel(&a)), Err("aucun changement".into()));
    }

    // ── Enregistrer l'actuel ─────────────────────────────────────────

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-app-ecrans-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("ecrans.toml")
    }

    #[test]
    fn enregistrer_un_nouveau_profil() {
        let mut a = ecrans();
        let path = tmp("nouveau");
        a.ecrans.profiles_path = Some(path.clone());
        a.ecrans.profile_sel = 2; // « + Enregistrer l'actuel »
        a.profile_bar_activate(Instant::now());
        assert_eq!(a.overlay, Overlay::SaveProfile(SaveDialog::default()));
        a.overlay = Overlay::SaveProfile(SaveDialog { name: " Maison ".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.ecrans.profiles.len(), 3);
        assert_eq!(a.ecrans.profiles[2], from_outputs("Maison", &poste()));
        assert_eq!(profiles::load(&path), (a.ecrans.profiles.clone(), None), "écrit sur le disque");
        assert!(a.toasts.iter().any(|t| t.text == "Profil « Maison » enregistré"));
    }

    #[test]
    fn remplacer_apres_confirmation() {
        let mut a = ecrans();
        a.ecrans.profiles_path = Some(tmp("remplacer"));
        let avant = a.ecrans.profiles.clone();
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.overlay, Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), replace: true, yes_focused: false }));
        a.save_enter(Instant::now()); // focus sur Annuler
        assert_eq!(a.overlay, Overlay::None);
        assert_eq!(a.ecrans.profiles, avant, "rien de remplacé");
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), replace: true, yes_focused: true });
        a.save_enter(Instant::now());
        assert_eq!(a.ecrans.profiles.len(), 2);
        assert_eq!(a.ecrans.profiles[1], from_outputs("Bureau", &poste()));
    }

    #[test]
    fn enregistrement_impossible_signale() {
        let mut a = ecrans();
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "X".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Profil « X » non enregistré : aucun dossier de configuration"));
        assert_eq!(a.ecrans.profiles.len(), 2);
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "   ".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert!(matches!(a.overlay, Overlay::SaveProfile(_)), "nom vide : rien");
    }

    #[test]
    fn enregistrement_refuse_si_fichier_invalide() {
        // Tâche 5 (relecture contrôleur) : le fichier chargé était invalide
        // (avertissement conservé) → jamais écraser ce qu'on n'a pas su lire.
        let mut a = ecrans();
        let path = tmp("invalide");
        a.ecrans.profiles_path = Some(path.clone());
        a.ecrans.profiles_invalid = true;
        let avant = a.ecrans.profiles.clone();
        a.overlay = Overlay::SaveProfile(SaveDialog { name: "Maison".into(), ..SaveDialog::default() });
        a.save_enter(Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("ecrans.toml invalide : corrige-le avant d'enregistrer"));
        assert_eq!(a.ecrans.profiles, avant, "rien de remplacé");
        assert!(!path.exists(), "rien écrit");
    }

    #[test]
    fn libelles_de_placement() {
        assert_eq!(placement_labels(&poste()), vec!["position 0, 1080", "origine", "à droite de Alpha"]);
        let outs = vec![on("DP-1", ALPHA, 1920, 1080, 0, 0), on("DP-2", BRAVO, 800, 600, 560, 1080), off("DP-3", "Charlie 3")];
        assert_eq!(placement_labels(&outs), vec!["origine", "en dessous de Alpha, centré", ""]);
    }
}
