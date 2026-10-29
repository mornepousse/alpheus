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
    let e =
        BackendError::Incomplete { failures: vec![("DVI-I-1".into(), BackendError::Timeout { tool: "wm-set".into() })], kept: Vec::new() };
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
        Loadable::Ready(crate::backends::displays::parse_outputs(include_str!("../../../tests/fixtures/wm-set-outputs.json")).unwrap());
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
