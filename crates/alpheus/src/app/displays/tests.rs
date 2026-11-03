use super::*;
use crate::app::action::{Command, Tab};
use crate::app::testing::{ready, ready_app};
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

/// The daemon's snapshot of `outputs` with its trial as `trial`, read at `now`.
fn read(a: &mut App, outputs: Vec<Output>, trial: TrialState, now: Instant) {
    a.on_update(Update::Displays(Ok(Snapshot { trial, ..ready(outputs) })), now);
}

/// The actions sent to the worker (reads left out), emptying the box.
fn sent(a: &mut App) -> Vec<Op> {
    a.take_requests()
        .into_iter()
        .filter_map(|r| match r {
            Request::Do(op) => Some(op),
            Request::Refresh(_) => None,
        })
        .collect()
}

/// The plan sent to the daemon to try.
fn sent_trial(a: &mut App) -> Plan {
    match sent(a).as_slice() {
        [Op::TryDisplays(plan)] => plan.clone(),
        r => panic!("expected a try: {r:?}"),
    }
}

/// "Bureau" tried and answered at `t0`: countdown started.
fn applied(t0: Instant) -> (App, Plan) {
    let mut a = displays_tab();
    a.apply_display_profile("Bureau", t0);
    let plan = sent_trial(&mut a);
    a.on_update(Update::Done(Op::TryDisplays(plan.clone()), Ok(())), t0);
    a.take_requests();
    (a, plan)
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
    sent(a).is_empty()
}

fn toasted(a: &App, text: &str) -> usize {
    a.toasts.iter().filter(|t| t.text == text).count()
}

#[test]
fn locked_until_a_fresh_read() {
    let t0 = Instant::now();
    let (mut a, plan) = applied(t0);
    // Read taken during the trial: it shows the tried settings.
    read(&mut a, applied_outputs(&plan), TrialState::Open { seconds_left: 15 }, t0);
    a.keep_answer(false, t0);
    assert_eq!(sent(&mut a), vec![Op::RevertDisplays]);
    // The data is still that of the rejected trial: nothing should go out.
    a.toggle_output("HDMI-A-1", t0);
    a.apply_display_profile("Laptop seul", t0);
    assert!(no_do(&mut a), "no setting on a stale read");
    assert_eq!(a.displays.trial, None);
    // Only the fresh read is missing: the message says so.
    assert!(toasted(&a, "Displays: waiting for a fresh read") > 0);
    assert_eq!(toasted(&a, "Displays: a change is already in progress"), 0);
    a.on_update(Update::Done(Op::RevertDisplays, Ok(())), t0);
    a.toggle_output("HDMI-A-1", t0);
    assert!(no_do(&mut a), "answered, but not read since");
    // Fresh read: the restored state becomes the new base.
    read(&mut a, machine(), TrialState::Ended(TrialEnd::Reverted), t0);
    a.toggle_output("HDMI-A-1", t0);
    assert_eq!(sent_trial(&mut a).steps, vec![Step { output: "HDMI-A-1".into(), active: None }]);

    // Keep before any read: same lock.
    let (mut a, _) = applied(t0);
    a.keep_answer(true, t0);
    assert_eq!(a.displays.trial, None);
    assert_eq!(sent(&mut a), vec![Op::KeepDisplays]);
    a.toggle_output("HDMI-A-1", t0);
    assert!(no_do(&mut a), "kept but not read yet");
    assert_eq!(a.toasts.last().map(|t| t.text.as_str()), Some("Displays: waiting for a fresh read"));
    a.on_update(Update::Done(Op::KeepDisplays, Ok(())), t0);
    read(&mut a, applied_outputs(&plan), TrialState::Ended(TrialEnd::Kept), t0);
    a.toggle_output("HDMI-A-1", t0);
    sent_trial(&mut a);
}

#[test]
fn cancel_while_applying() {
    let t0 = Instant::now();
    let mut a = displays_tab();
    a.apply_display_profile("Bureau", t0);
    let plan = sent_trial(&mut a);
    a.keep_answer(false, t0);
    assert_eq!(sent(&mut a), vec![Op::RevertDisplays], "Cancel while applying: the revert follows the try");
    assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None));
    a.on_update(Update::Done(Op::TryDisplays(plan), Ok(())), t0);
    assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None), "the trial does not come back");
    a.on_tick(later(t0, 20_000));
    assert!(no_do(&mut a), "no second revert at the deadline");
    a.on_update(Update::Done(Op::RevertDisplays, Ok(())), t0);
    assert_eq!(toasted(&a, "Displays: previous settings restored"), 0, "answered at once, before the compositor");
    read(&mut a, machine(), TrialState::Ended(TrialEnd::Reverted), t0);
    assert_eq!(toasted(&a, "Displays: previous settings restored"), 1, "told by the daemon's report");
    read(&mut a, machine(), TrialState::Ended(TrialEnd::Reverted), t0);
    assert_eq!(toasted(&a, "Displays: previous settings restored"), 1, "the same end: told once");
    a.toggle_output("HDMI-A-1", t0);
    sent_trial(&mut a);
}

/// The daemon's refusals and the daemon gone: the box closes on its words, nothing is reverted
/// (nothing was applied).
#[test]
fn a_refused_try_closes_the_box_with_the_daemons_words() {
    for e in [
        "another change is in progress",
        "refused: no screen would stay on",
        "the compositor refused this configuration",
        "no answer from the compositor",
        "displays are off in bar.toml",
        "the daemon is too old for displays",
        "the alpheus daemon is not running",
    ] {
        let t0 = Instant::now();
        let mut a = displays_tab();
        a.apply_display_profile("Bureau", t0);
        let plan = sent_trial(&mut a);
        a.on_update(Update::Done(Op::TryDisplays(plan), Err(BackendError::Daemon(e.into()))), t0);
        assert!(no_do(&mut a), "{e}: nothing reverted");
        assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None), "{e}");
        let msg = format!("Displays: {e}");
        assert_eq!(a.status.error.as_deref(), Some(msg.as_str()));
        assert_eq!(toasted(&a, &msg), 1);
    }
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

/// The daemon's status: off and no manager show its words; ready with no output yet (before the
/// compositor's first `done`) is an empty list, not an error.
#[test]
fn the_daemons_status_shows_its_words() {
    let t0 = Instant::now();
    for (status, expected) in [
        (Status::Off, Loadable::Failed("displays are off in bar.toml".into())),
        (Status::NoManager, Loadable::Failed("the compositor has no output management".into())),
        (Status::Ready, Loadable::Ready(Vec::new())),
    ] {
        let mut a = displays_tab();
        a.displays.queued_profile = Some("Bureau".into());
        a.on_update(Update::Displays(Ok(Snapshot { status, outputs: Vec::new(), trial: TrialState::None })), t0);
        assert_eq!(a.data.displays, expected);
        assert!(no_do(&mut a), "nothing tried on {status:?}");
    }
    let mut a = displays_tab();
    a.on_update(Update::Displays(Err(BackendError::Daemon("the alpheus daemon is not running".into()))), t0);
    assert_eq!(a.data.displays, Loadable::Failed("the alpheus daemon is not running".into()));
}

#[test]
fn profile_applied_with_countdown() {
    let mut a = displays_tab();
    let t0 = Instant::now();
    a.displays.profile_sel = 1; // Bureau
    a.profile_bar_activate(t0);
    let plan = sent_trial(&mut a);
    assert_eq!(plan.steps.iter().map(|s| s.output.as_str()).collect::<Vec<_>>(), vec!["DVI-I-1", "HDMI-A-1", "eDP-1"]);
    assert_eq!(a.overlay, Overlay::Keep(Keep { keep_focused: false }), "focus on Cancel");
    let t = a.displays.trial.clone().unwrap();
    assert_eq!(t.title, "Profile \"Bureau\"");
    assert_eq!(t.deadline, None, "no countdown before the answer");
    assert_eq!(t.summary[0], "Alpha: 1920×1080@60, scale 1.0, 0°, position 0, 0");
    assert_eq!(t.summary[2], "Laptop: off");
    assert!(a.trial_open(), "closing while applying: reverted");
    a.on_update(Update::Done(Op::TryDisplays(plan), Ok(())), t0);
    let t = a.displays.trial.clone().unwrap();
    assert_eq!((t.deadline, t.remaining), (Some(t0 + KEEP_DELAY), 15));
    assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])], "re-read after applying");
}

/// The countdown runs on the tab's clock between reads; the daemon's seconds left bring it
/// forward, never back.
#[test]
fn the_countdown_follows_the_daemon() {
    let t0 = Instant::now();
    let (mut a, plan) = applied(t0);
    a.on_tick(later(t0, 5_200));
    assert_eq!(a.displays.trial.as_ref().unwrap().remaining, 10);
    read(&mut a, applied_outputs(&plan), TrialState::Open { seconds_left: 8 }, later(t0, 5_200));
    assert_eq!(a.displays.trial.as_ref().unwrap().remaining, 8, "the daemon is ahead: brought forward");
    a.on_tick(later(t0, 6_200));
    assert_eq!(a.displays.trial.as_ref().unwrap().remaining, 7, "then the tab's own clock");
    read(&mut a, applied_outputs(&plan), TrialState::Open { seconds_left: 12 }, later(t0, 6_200));
    assert_eq!(a.displays.trial.as_ref().unwrap().remaining, 7, "never pushed back");
    assert!(no_do(&mut a));
}

#[test]
fn automatic_rollback_after_fifteen_seconds() {
    let t0 = Instant::now();
    let (mut a, plan) = applied(t0);
    a.on_tick(later(t0, 5_200));
    assert_eq!(a.displays.trial.as_ref().unwrap().remaining, 10);
    assert!(no_do(&mut a), "nothing is rolled back before 15 s");
    a.on_tick(later(t0, 15_000));
    assert_eq!(sent(&mut a), vec![Op::RevertDisplays], "the daemon reverts on its own; asked as well");
    assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None));
    assert_eq!(toasted(&a, "Displays: time's up, rolling back to the previous settings"), 1);
    a.on_update(Update::Done(Op::RevertDisplays, Ok(())), later(t0, 15_100));
    read(&mut a, applied_outputs(&plan), TrialState::Ended(TrialEnd::Reverted), later(t0, 15_200));
    assert_eq!(toasted(&a, "Displays: previous settings restored"), 1);
}

#[test]
fn cancel_and_keep() {
    let t0 = Instant::now();
    let (mut a, _) = applied(t0);
    a.keep_answer(false, t0);
    assert_eq!(sent(&mut a), vec![Op::RevertDisplays], "Cancel: reverted right away");
    assert!(!a.trial_open());
    let (mut a, _) = applied(t0);
    a.keep_answer(true, t0);
    assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None));
    assert_eq!(sent(&mut a), vec![Op::KeepDisplays]);
    assert!(!a.trial_open(), "kept: nothing to revert on close");
    a.on_tick(later(t0, 20_000));
    assert!(no_do(&mut a));
}

#[test]
fn keep_waits_for_the_apply() {
    let mut a = displays_tab();
    a.apply_display_profile("Bureau", Instant::now());
    a.take_requests();
    a.keep_answer(true, Instant::now());
    assert!(a.displays.trial.is_some(), "not applied yet: Keep has no effect");
}

/// The daemon ended the trial (its 2 s refresh): the box closes with how, and nothing is sent.
#[test]
fn an_ended_trial_closes_the_keep_box() {
    for (end, text, error) in [
        (TrialEnd::ScreenLost, "Screen lost during the trial: rolling back to the previous state", true),
        (TrialEnd::ChangedElsewhere, "Displays changed elsewhere", false),
        (TrialEnd::Refused, "Displays: the compositor refused this configuration", true),
        (TrialEnd::NoAnswer, "Displays: no answer from the compositor", true),
        (TrialEnd::Reverted, "Displays: previous settings restored", false),
        (TrialEnd::Kept, "Displays: settings kept", false),
    ] {
        let t0 = Instant::now();
        let (mut a, plan) = applied(t0);
        read(&mut a, applied_outputs(&plan), TrialState::Ended(end), t0);
        assert_eq!((a.displays.trial.clone(), a.overlay.clone()), (None, Overlay::None), "{end:?}");
        assert!(!a.trial_open());
        assert_eq!(toasted(&a, text), 1, "{end:?}");
        assert_eq!(a.toasts.last().map(|t| t.error), Some(error), "{end:?}");
        assert!(no_do(&mut a), "{end:?}: nothing sent");
        a.on_tick(later(t0, 20_000));
        assert!(no_do(&mut a), "{end:?}: no revert at the deadline");
    }

    // The last end read is the one told: a cancelled trial whose revert the compositor refused.
    let t0 = Instant::now();
    let (mut a, plan) = applied(t0);
    a.keep_answer(false, t0);
    a.on_update(Update::Done(Op::RevertDisplays, Ok(())), t0);
    read(&mut a, applied_outputs(&plan), TrialState::Ended(TrialEnd::Reverted), t0);
    read(&mut a, applied_outputs(&plan), TrialState::Ended(TrialEnd::Refused), t0);
    assert_eq!(toasted(&a, "Displays: previous settings restored"), 1);
    assert_eq!(toasted(&a, "Displays: the compositor refused this configuration"), 1);
    // Kept: told at once, the daemon's report of it not again.
    let (mut a, plan) = applied(t0);
    a.keep_answer(true, t0);
    a.on_update(Update::Done(Op::KeepDisplays, Ok(())), t0);
    read(&mut a, applied_outputs(&plan), TrialState::Ended(TrialEnd::Kept), t0);
    assert_eq!(toasted(&a, "Displays: settings kept"), 1);
    // A trial this tab never tried (another alpheus): told nothing.
    let mut a = displays_tab();
    read(&mut a, machine(), TrialState::Ended(TrialEnd::ScreenLost), t0);
    assert!(a.toasts.is_empty() && a.status.error.is_none(), "{:?}", a.toasts);
}

#[test]
fn revert_impossible_is_reported() {
    let t0 = Instant::now();
    let (mut a, _) = applied(t0);
    a.keep_answer(false, t0);
    a.take_requests();
    a.on_update(Update::Done(Op::RevertDisplays, Err(BackendError::Daemon("the alpheus daemon is not running".into()))), t0);
    assert_eq!(a.status.error.as_deref(), Some("Displays: rollback impossible: the alpheus daemon is not running"));
    let (mut a, _) = applied(t0);
    a.keep_answer(true, t0);
    a.take_requests();
    a.on_update(Update::Done(Op::KeepDisplays, Err(BackendError::Daemon("the alpheus daemon is not running".into()))), t0);
    assert_eq!(a.status.error.as_deref(), Some("Displays: the alpheus daemon is not running"));
}

#[test]
fn no_other_change_during_the_trial() {
    let t0 = Instant::now();
    let (mut a, _) = applied(t0);
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
    let plan = sent_trial(&mut a);
    assert_eq!(plan.steps, vec![Step { output: "eDP-1".into(), active: None }], "the others don't move");
    // An off output turns on to the right of the rightmost one (Bravo ends at 4053).
    let mut a = displays_tab();
    let mut outs = machine();
    outs.push(off("DP-3", "Charlie Modèle 3 SERIE0003"));
    a.data.displays = Loadable::Ready(outs);
    a.toggle_output("DP-3", Instant::now());
    let plan = sent_trial(&mut a);
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
    read(&mut a, machine(), TrialState::None, Instant::now());
    let plan = sent_trial(&mut a);
    assert!(plan.touches("DVI-I-1"));
    assert_eq!(a.displays.queued_profile, None);
}

#[test]
fn queued_profile_dropped_if_the_read_fails() {
    let mut a = ready_app();
    a.run_command(Command::ApplyDisplayProfile("Bureau".into()), Instant::now());
    a.take_requests();
    a.on_update(Update::Displays(Err(BackendError::Daemon("the alpheus daemon is not running".into()))), Instant::now());
    assert_eq!(a.displays.queued_profile, None);
    // A successful read much later applies nothing.
    read(&mut a, machine(), TrialState::None, Instant::now());
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
    assert_eq!(sent_trial(&mut a), plan);
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
    a.data.displays = Loadable::Ready(recorded_outputs());
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
    read(&mut a, without, TrialState::None, Instant::now());
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
    let plan = sent_trial(&mut a);
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
    // The file loaded was invalid (warning kept) → never overwrite what we couldn't read.
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

/// The machine's outputs as the daemon's snapshot gives them (anonymised: descriptions replaced).
pub(crate) fn recorded_outputs() -> Vec<Output> {
    crate::displays::wire::decode_snapshot(include_str!("../../../tests/fixtures/outputs-snapshot.txt")).unwrap().outputs
}
