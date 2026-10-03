//! Onglet Écrans : profils, bascule d'une sortie, et l'essai « Garder ces
//! réglages ? » avec retour automatique. Tout réglage passe par un essai :
//! plan envoyé au fil avec son plan inverse, compte à rebours de 15 s une
//! fois appliqué.

use super::worker::Op;
use super::{App, Overlay};
use crate::backends::BResult;
use crate::displays::geometry::{deduce, logical_size, resolve, Align, Placement, Relation, Screen};
use crate::displays::profiles::{placed, plan_profile, Profile};
use crate::displays::{
    active_count, changed_elsewhere, rotation_label, short_from_description, short_names, Active, Mode, Output, Plan,
    Scale, Step,
};
use std::time::{Duration, Instant};

/// Délai avant le retour automatique aux réglages précédents.
pub const KEEP_DELAY: Duration = Duration::from_secs(15);

/// État de l'onglet hors données lues.
#[derive(Debug, Default)]
pub struct Ecrans {
    pub profiles: Vec<Profile>,
    /// Puce choisie dans la barre des profils ; `profiles.len()` désigne
    /// « + Enregistrer l'actuel ».
    pub profile_sel: usize,
    pub trial: Option<Trial>,
    /// Retour envoyé et sans réponse : à refaire si alpheus se ferme.
    pub undoing: Option<Plan>,
    /// Profil demandé par la palette : appliqué à la prochaine lecture.
    pub queued_profile: Option<String>,
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
    Ok(Plan::between(outs, &targets(outs, pos, i, (mode, Scale::ONE, 0))))
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
        let busy = self.ecrans.trial.is_some() || self.ecrans.undoing.is_some();
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
    /// profil demandé par la palette.
    pub(crate) fn displays_read(&mut self, now: Instant) {
        let outs = self.outputs().to_vec();
        if self.data.displays.ready().is_none() {
            return;
        }
        if let Some(t) = &self.ecrans.trial {
            if t.deadline.is_some() && changed_elsewhere(&t.before, &t.plan, &outs) {
                self.ecrans.trial = None;
                self.close_keep();
                self.status.last = Some("Écrans modifiés ailleurs".into());
                self.toast("Écrans modifiés ailleurs".into(), false, now);
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
        if self.displays_busy(now) {
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

    /// Entrée sur la barre des profils : appliquer le profil choisi.
    pub(crate) fn profile_bar_activate(&mut self, now: Instant) {
        if let Some(name) = self.ecrans.profiles.get(self.ecrans.profile_sel).map(|p| p.name.clone()) {
            self.apply_display_profile(&name, now);
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

    #[test]
    fn libelles_de_placement() {
        assert_eq!(placement_labels(&poste()), vec!["position 0, 1080", "origine", "à droite de Alpha"]);
        let outs = vec![on("DP-1", ALPHA, 1920, 1080, 0, 0), on("DP-2", BRAVO, 800, 600, 560, 1080), off("DP-3", "Charlie 3")];
        assert_eq!(placement_labels(&outs), vec!["origine", "en dessous de Alpha, centré", ""]);
    }
}
