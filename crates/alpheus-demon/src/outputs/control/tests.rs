use super::*;
use crate::outputs::model::{HeadMode, KEEP};
use alpheus_commun::displays::profiles::parse;
use alpheus_commun::displays::{Active, Mode, Scale};
use std::cell::Cell;
use std::time::Duration;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A head reporting `make model serial`, enabled at `x` on its first mode
/// (1920x1080, 60 Hz preferred, then 50 Hz).
fn head(name: &str, make: &str, serial: &str, x: i32) -> Head {
    Head {
        name: name.into(),
        description: format!("{make} Panel ({name})"),
        make: make.into(),
        model: "P24".into(),
        serial: serial.into(),
        enabled: true,
        modes: vec![
            HeadMode { width: 1920, height: 1080, mhz: 60_000, preferred: true },
            HeadMode { width: 1920, height: 1080, mhz: 50_000, preferred: false },
        ],
        current: Some(0),
        x,
        y: 0,
        scale: 1.0,
        transform: 0,
        displaylink: false,
    }
}

fn alpha(x: i32) -> Head {
    head("DP-1", "Alpha", "A1", x)
}

fn bravo(x: i32) -> Head {
    head("DP-2", "Bravo", "B2", x)
}

fn charlie(x: i32) -> Head {
    head("DP-3", "Charlie", "C3", x)
}

/// A DisplayLink head running 144 Hz, with a 120 Hz mode of the same size.
fn dock(x: i32) -> Head {
    let mut h = head("DVI-I-1", "Dock", "D4", x);
    h.displaylink = true;
    h.modes = vec![
        HeadMode { width: 2560, height: 1440, mhz: 144_000, preferred: true },
        HeadMode { width: 2560, height: 1440, mhz: 120_000, preferred: false },
        HeadMode { width: 2560, height: 1440, mhz: 60_000, preferred: false },
    ];
    h
}

/// Alpha at the origin, Bravo at x = 1920.
const SIDE_BY_SIDE: &str = r#"
[[profile]]
name = "Desk"
[[profile.display]]
description = "Alpha P24 A1"
mode = "1920x1080@60"
placement = { x = 0, y = 0 }
[[profile.display]]
description = "Bravo P24 B2"
mode = "1920x1080@60"
placement = { x = 1920, y = 0 }
"#;

/// A profiles source that counts its reads.
struct Source {
    text: &'static str,
    reads: Cell<u32>,
}

impl Source {
    fn new(text: &'static str) -> Self {
        Source { text, reads: Cell::new(0) }
    }

    fn get(&self) -> Vec<Profile> {
        self.reads.set(self.reads.get() + 1);
        parse(self.text).unwrap()
    }
}

fn sends(actions: &[Action]) -> Vec<&Vec<Target>> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Send(_, t) => Some(t),
            _ => None,
        })
        .collect()
}

/// The generation in flight (0, never sent, when none is).
fn gen_of(c: &Controller) -> u64 {
    c.flight.as_ref().map_or(0, |f| f.generation)
}

/// The compositor's answers to the configuration in flight.
fn ok(c: &Controller) -> Event {
    Event::Succeeded(gen_of(c))
}

fn failed(c: &Controller) -> Event {
    Event::Failed(gen_of(c))
}

fn cancelled(c: &Controller) -> Event {
    Event::Cancelled(gen_of(c))
}

/// The generation of the one configuration sent in `actions`.
fn sent_gen(actions: &[Action]) -> u64 {
    let gens: Vec<u64> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Send(g, _) => Some(*g),
            _ => None,
        })
        .collect();
    assert_eq!(gens.len(), 1, "{actions:?}");
    gens[0]
}

fn logs(actions: &[Action]) -> Vec<&'static str> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Log(l) => Some(*l),
            _ => None,
        })
        .collect()
}

fn replies(actions: &[Action]) -> Vec<Result<(), &'static str>> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Reply(ReplyTo::Try, r) => Some(*r),
            _ => None,
        })
        .collect()
}

fn target<'t>(targets: &'t [Target], name: &str) -> &'t Target {
    targets.iter().find(|t| t.name == name).unwrap()
}

fn move_to(out: &str, x: i32) -> Step {
    let mode = Mode { width: 1920, height: 1080, refresh: 60 };
    Step { output: out.into(), active: Some(Active { mode, x, y: 0, scale: Scale::ONE, transform: 0 }) }
}

/// A controller that already evaluated `heads` (no profile), at rest.
fn settled(heads: Vec<Head>, t0: Instant) -> Controller {
    let none = Source::new("");
    let mut c = Controller::new(true);
    c.on_event(Event::Done(heads), t0);
    c.tick(&|| none.get(), t0 + DEBOUNCE);
    assert_eq!(none.reads.get(), 1);
    assert_eq!(c.next_deadline(), None);
    c
}

/// A trial moving Alpha to x = 100 on [Alpha, Bravo], answered at
/// t0 + 10 ms; the new layout reported just after.
fn trial_open(t0: Instant) -> Controller {
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    let a = c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    assert_eq!(sends(&a).len(), 1);
    let a = c.on_event(ok(&c), t0 + ms(10));
    assert_eq!(replies(&a), vec![Ok(())]);
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(15)).is_empty());
    c
}

#[test]
fn a_burst_of_dones_evaluates_once_after_the_debounce() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    // A dock enumerating its screens over a few hundred milliseconds.
    assert!(c.on_event(Event::Done(vec![alpha(0)]), t0).is_empty());
    assert!(c.on_event(Event::Done(vec![alpha(0), charlie(1920)]), t0 + ms(300)).is_empty());
    assert!(c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(700)).is_empty());
    assert_eq!(c.next_deadline(), Some(t0 + ms(1200)));
    // Each Done re-armed the debounce: nothing before it expires.
    assert!(c.tick(&profiles, t0 + ms(1100)).is_empty());
    assert_eq!(src.reads.get(), 0);
    // Settled: the profile is read once; it already matches, so nothing is sent.
    let a = c.tick(&profiles, t0 + ms(1200));
    assert_eq!(src.reads.get(), 1);
    assert!(sends(&a).is_empty());
    assert!(c.tick(&profiles, t0 + ms(5000)).is_empty());
    assert_eq!(src.reads.get(), 1);
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn our_own_apply_does_not_reevaluate() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    // Bravo plugged left of Alpha: the profile wants it on the right.
    c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0);
    let a = c.tick(&profiles, t0 + DEBOUNCE);
    let sent = sends(&a);
    assert_eq!(sent.len(), 1);
    assert_eq!(target(sent[0], "DP-2").state, Some((0, 1920, 0, 1.0, 0)));
    // The compositor answers, then reports the new layout (either order).
    assert!(c.on_event(ok(&c), t0 + ms(600)).is_empty());
    assert!(c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(610)).is_empty());
    assert!(c.tick(&profiles, t0 + ms(5000)).is_empty());
    assert_eq!(src.reads.get(), 1);
    assert_eq!(c.next_deadline(), None);
    // Done before the answer: still one evaluation, one send.
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0);
    assert_eq!(sends(&c.tick(&profiles, t0 + DEBOUNCE)).len(), 1);
    assert!(c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(600)).is_empty());
    assert!(c.on_event(ok(&c), t0 + ms(610)).is_empty());
    assert!(c.tick(&profiles, t0 + ms(5000)).is_empty());
    assert_eq!(src.reads.get(), 1);
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn no_profile_logs_once() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), charlie(1920)]), t0);
    let a = c.tick(&profiles, t0 + DEBOUNCE);
    assert_eq!(logs(&a), vec!["displays: no profile for these screens"]);
    assert!(sends(&a).is_empty());
    // Same screens again (a state change elsewhere): no second evaluation.
    assert!(c.on_event(Event::Done(vec![alpha(0), charlie(3000)]), t0 + ms(800)).is_empty());
    assert!(c.tick(&profiles, t0 + ms(5000)).is_empty());
    assert_eq!(src.reads.get(), 1);
}

#[test]
fn ambiguous_screens_apply_nothing() {
    let t0 = Instant::now();
    const TWINS: &str = r#"
[[profile]]
name = "Twins"
[[profile.display]]
description = "Same P24"
mode = "1920x1080@60"
placement = { x = 0, y = 0 }
[[profile.display]]
description = "Same P24"
mode = "1920x1080@60"
placement = { x = 1920, y = 0 }
"#;
    let src = Source::new(TWINS);
    let mut c = Controller::new(true);
    // Identical screens with no serial, swapped from the profile's layout.
    c.on_event(Event::Done(vec![head("DP-1", "Same", "", 1920), head("DP-2", "Same", "", 0)]), t0);
    let a = c.tick(&|| src.get(), t0 + DEBOUNCE);
    assert_eq!(logs(&a), vec!["displays: two screens look the same"]);
    assert!(sends(&a).is_empty());
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn a_profile_that_does_not_resolve_names_itself() {
    let t0 = Instant::now();
    const NO_SUCH_MODE: &str = r#"
[[profile]]
name = "Desk"
[[profile.display]]
description = "Alpha P24 A1"
mode = "640x480@60"
placement = { x = 0, y = 0 }
"#;
    let src = Source::new(NO_SUCH_MODE);
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0)]), t0);
    let a = c.tick(&|| src.get(), t0 + DEBOUNCE);
    assert_eq!(a, vec![Action::LogProfile("Desk".into())]);
}

#[test]
fn a_failed_profile_is_not_retried_for_the_same_screens() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0);
    assert_eq!(sends(&c.tick(&profiles, t0 + DEBOUNCE)).len(), 1);
    let a = c.on_event(failed(&c), t0 + ms(600));
    assert_eq!(logs(&a), vec!["displays: configuration refused by the compositor"]);
    // The same screens, even moved elsewhere: never retried.
    assert!(c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0 + ms(700)).is_empty());
    assert!(c.on_event(Event::Done(vec![alpha(1000), bravo(0)]), t0 + ms(800)).is_empty());
    assert!(c.tick(&profiles, t0 + ms(5000)).is_empty());
    assert_eq!(src.reads.get(), 1);
    assert_eq!(c.next_deadline(), None);
    // Once the set changes and comes back, it is tried again.
    c.on_event(Event::Done(vec![alpha(1920)]), t0 + ms(6000));
    c.tick(&profiles, t0 + ms(6500));
    c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0 + ms(7000));
    assert_eq!(sends(&c.tick(&profiles, t0 + ms(7500))).len(), 1);
}

#[test]
fn a_noop_is_never_sent() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0);
    // The profile asks for what already runs.
    assert!(sends(&c.tick(&|| src.get(), t0 + DEBOUNCE)).is_empty());
    // A try asking for what already runs succeeds at once, with no trial.
    let a = c.try_(&[move_to("DP-1", 0)], t0 + ms(600)).unwrap();
    assert_eq!(a, vec![Action::Reply(ReplyTo::Try, Ok(()))]);
    assert_eq!(c.trial_state(t0 + ms(600)), TrialState::None);
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn an_empty_try_answers_ok_and_opens_no_trial() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    let a = c.try_(&[], t0).unwrap();
    assert_eq!(a, vec![Action::Reply(ReplyTo::Try, Ok(()))]);
    assert_eq!(c.trial_state(t0), TrialState::None);
    assert_eq!(c.next_deadline(), None);
    // Nothing is held: a real try goes through.
    assert_eq!(sends(&c.try_(&[move_to("DP-1", 10)], t0).unwrap()).len(), 1);
    // Even before the first `done`.
    assert_eq!(Controller::new(true).try_(&[], t0), Ok(vec![Action::Reply(ReplyTo::Try, Ok(()))]));
}

#[test]
fn a_try_the_model_refuses_answers_at_once() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    let off = Step { output: "DP-1".into(), active: None };
    assert_eq!(c.try_(&[off], t0), Err("refused: no screen would stay on"));
    assert_eq!(c.try_(&[move_to("HDMI-A-9", 0)], t0), Err("the compositor refused this configuration"));
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn cancelled_is_retried_three_times() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    let first = c.try_(&[move_to("DP-1", 10)], t0).unwrap();
    let wanted = sends(&first)[0].clone();
    for i in 1..=3 {
        let at = t0 + ms(100 * i);
        assert!(c.on_event(cancelled(&c), at).is_empty());
        // Re-sent after the next Done, unchanged.
        let a = c.on_event(Event::Done(vec![alpha(0)]), at + ms(10));
        assert_eq!(sends(&a), vec![&wanted], "retry {i}");
    }
    let a = c.on_event(cancelled(&c), t0 + ms(400));
    assert_eq!(logs(&a), vec!["displays: configuration refused by the compositor"]);
    assert_eq!(replies(&a), vec![Err("the compositor refused this configuration")]);
    assert!(sends(&c.on_event(Event::Done(vec![alpha(0)]), t0 + ms(410))).is_empty());
    assert_eq!(c.trial_state(t0 + ms(410)), TrialState::None);
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn a_resend_keeps_the_first_sends_watchdog() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    c.try_(&[move_to("DP-1", 10)], t0).unwrap();
    assert!(c.on_event(cancelled(&c), t0 + ms(3000)).is_empty());
    let a = c.on_event(Event::Done(vec![alpha(0)]), t0 + ms(3100));
    assert_eq!(sends(&a).len(), 1, "re-sent");
    assert_eq!(c.next_deadline(), Some(t0 + WATCHDOG), "the first send's deadline, not the re-send's");
    assert!(c.tick(&|| Vec::new(), t0 + WATCHDOG - ms(1)).is_empty());
    let a = c.tick(&|| Vec::new(), t0 + WATCHDOG);
    assert_eq!(logs(&a), vec!["displays: no answer from the compositor"]);
    assert_eq!(replies(&a), vec![Ok(())], "possibly applied: the try is answered");
}

#[test]
fn a_cancel_after_its_done_resends_at_once() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    c.try_(&[move_to("DP-1", 10)], t0).unwrap();
    // The Done that outdated the configuration came first.
    assert!(c.on_event(Event::Done(vec![alpha(0)]), t0 + ms(10)).is_empty());
    assert_eq!(sends(&c.on_event(cancelled(&c), t0 + ms(20))).len(), 1);
}

#[test]
fn a_head_whose_mode_is_unknown_is_kept_as_is() {
    let t0 = Instant::now();
    let mut unknown = bravo(1920);
    unknown.current = None;
    let mut c = settled(vec![alpha(0), unknown.clone()], t0);
    let a = c.try_(&[move_to("DP-1", 10)], t0).unwrap();
    assert_eq!(target(sends(&a)[0], "DP-2").state, Some((KEEP, 1920, 0, 1.0, 0)));
    // Re-sent after a cancel, and restored by the revert, still kept.
    c.on_event(cancelled(&c), t0 + ms(10));
    let a = c.on_event(Event::Done(vec![alpha(0), unknown.clone()]), t0 + ms(20));
    assert_eq!(target(sends(&a)[0], "DP-2").state, Some((KEEP, 1920, 0, 1.0, 0)));
    c.on_event(ok(&c), t0 + ms(30));
    c.on_event(Event::Done(vec![alpha(10), unknown]), t0 + ms(40));
    let a = c.revert(t0 + ms(50));
    assert_eq!(target(sends(&a)[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    assert_eq!(target(sends(&a)[0], "DP-2").state, Some((KEEP, 1920, 0, 1.0, 0)));
}

#[test]
fn one_configuration_in_flight() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0);
    assert_eq!(sends(&c.tick(&profiles, t0 + DEBOUNCE)).len(), 1);
    assert_eq!(c.next_deadline(), Some(t0 + DEBOUNCE + WATCHDOG));
    // While it waits: no try, and a Done that changes the screens waits too.
    assert_eq!(c.try_(&[move_to("DP-1", 5)], t0 + ms(600)), Err("another change is in progress"));
    assert!(c.on_event(Event::Done(vec![alpha(1920)]), t0 + ms(700)).is_empty());
    assert!(c.tick(&profiles, t0 + ms(1300)).is_empty());
    assert_eq!(src.reads.get(), 1);
    // After the answer, the deferred evaluation runs once, debounced.
    assert!(c.on_event(failed(&c), t0 + ms(1400)).iter().all(|a| !matches!(a, Action::Send(..))));
    assert_eq!(c.next_deadline(), Some(t0 + ms(1900)));
    let a = c.tick(&profiles, t0 + ms(1900));
    assert_eq!(src.reads.get(), 2);
    assert_eq!(logs(&a), vec!["displays: no profile for these screens"]);
}

#[test]
fn the_watchdog_ends_an_unanswered_configuration() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0);
    c.tick(&|| src.get(), t0 + DEBOUNCE);
    assert!(c.tick(&|| src.get(), t0 + DEBOUNCE + WATCHDOG - ms(1)).is_empty());
    let a = c.tick(&|| src.get(), t0 + DEBOUNCE + WATCHDOG);
    assert_eq!(logs(&a), vec!["displays: no answer from the compositor"]);
    assert_eq!(c.next_deadline(), None);
    assert!(c.try_(&[move_to("DP-1", 5)], t0 + ms(6000)).is_ok());
}

#[test]
fn the_cap_lowers_a_displaylink_head_without_a_profile() {
    let t0 = Instant::now();
    let none = Source::new("");
    let profiles = || none.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0);
    // After the debounce, like the profiles.
    assert!(c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0 + ms(100)).is_empty());
    let a = c.tick(&profiles, t0 + ms(600));
    assert!(logs(&a).contains(&"displays: lowered a DisplayLink screen to 120 Hz"));
    let sent = sends(&a);
    assert_eq!(sent.len(), 1);
    assert_eq!(target(sent[0], "DVI-I-1").state, Some((1, 1920, 0, 1.0, 0)));
    assert_eq!(target(sent[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    c.on_event(ok(&c), t0 + ms(700));
    let mut lowered = dock(1920);
    lowered.current = Some(1);
    assert!(c.on_event(Event::Done(vec![alpha(0), lowered]), t0 + ms(710)).is_empty());
    // Put back over the cap on the same screens: lowered once only.
    assert!(c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0 + ms(800)).is_empty());
    assert_eq!(c.next_deadline(), None);
    // With the automatic profiles off, the cap still holds.
    let mut c = Controller::new(false);
    c.on_event(Event::Done(vec![dock(0)]), t0);
    assert_eq!(sends(&c.tick(&profiles, t0 + DEBOUNCE)).len(), 1);
}

#[test]
fn the_cap_waits_for_the_trial() {
    let t0 = Instant::now();
    let none = Source::new("");
    let profiles = || none.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0);
    // A try before the debounce expired: the trial keeps the dock as it is.
    let a = c.try_(&[move_to("DP-1", 10)], t0 + ms(100)).unwrap();
    assert_eq!(target(sends(&a)[0], "DVI-I-1").state, Some((0, 1920, 0, 1.0, 0)));
    c.on_event(ok(&c), t0 + ms(200));
    assert!(c.on_event(Event::Done(vec![alpha(10), dock(1920)]), t0 + ms(210)).is_empty());
    assert!(c.tick(&profiles, t0 + ms(1000)).is_empty());
    assert_eq!(c.next_deadline(), Some(t0 + ms(200) + TRIAL));
    // Kept: the cap runs then, after its debounce.
    assert!(c.keep(t0 + ms(2000)).is_empty());
    let a = c.tick(&profiles, t0 + ms(2500));
    assert_eq!(target(sends(&a)[0], "DVI-I-1").state, Some((1, 1920, 0, 1.0, 0)));
}

#[test]
fn no_answer_counts_as_possibly_applied_and_the_trial_still_reverts() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    let a = c.tick(&|| Vec::new(), t0 + WATCHDOG);
    assert_eq!(replies(&a), vec![Ok(())]);
    assert_eq!(c.trial_state(t0 + WATCHDOG), TrialState::Open { seconds_left: 15 });
    // The change lands late: ours, not a change made elsewhere.
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(6000)).is_empty());
    assert_eq!(c.trial_state(t0 + ms(6000)), TrialState::Open { seconds_left: 14 });
    let a = c.tick(&|| Vec::new(), t0 + WATCHDOG + TRIAL);
    let sent = sends(&a);
    assert_eq!(sent.len(), 1);
    assert_eq!(target(sent[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    assert_eq!(c.trial_state(t0 + ms(20_001)), TrialState::Busy);
    // The revert is answered and lands: nothing is held any more.
    c.on_event(ok(&c), t0 + ms(20_100));
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(20_110));
    assert_eq!(c.trial_state(t0 + ms(20_110)), TrialState::Ended(TrialEnd::Reverted));
    assert_eq!(c.next_deadline(), None);
    assert!(c.try_(&[move_to("DP-1", 5)], t0 + ms(20_200)).is_ok());
}

#[test]
fn a_late_change_that_never_lands_is_not_a_change_elsewhere() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    c.tick(&|| Vec::new(), t0 + WATCHDOG);
    // Unconfirmed: a Done showing the state from before is still ours.
    assert!(c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(6000)).is_empty());
    assert!(matches!(c.trial_state(t0 + ms(6000)), TrialState::Open { .. }));
    // It may still land after that: ours too.
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(7000)).is_empty());
    assert!(matches!(c.trial_state(t0 + ms(7000)), TrialState::Open { .. }));
}

#[test]
fn the_trial_reverts_at_the_deadline_without_the_tui() {
    let t0 = Instant::now();
    let mut c = trial_open(t0);
    let deadline = t0 + ms(10) + TRIAL;
    assert_eq!(c.next_deadline(), Some(deadline));
    assert_eq!(c.trial_state(t0 + ms(10)), TrialState::Open { seconds_left: 15 });
    assert!(c.tick(&|| Vec::new(), deadline - ms(1)).is_empty());
    let a = c.tick(&|| Vec::new(), deadline);
    let sent = sends(&a);
    assert_eq!(sent.len(), 1);
    assert_eq!(target(sent[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    assert_eq!(target(sent[0], "DP-2").state, Some((0, 1920, 0, 1.0, 0)));
    assert_eq!(c.trial_state(deadline), TrialState::Busy);
    c.on_event(ok(&c), deadline + ms(10));
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), deadline + ms(20));
    assert_eq!(c.trial_state(deadline + ms(20)), TrialState::Ended(TrialEnd::Reverted));
    assert_eq!(c.trial_state(deadline + ms(20)), TrialState::None);
}

#[test]
fn revert_restores_the_state_from_before() {
    let t0 = Instant::now();
    let mut c = trial_open(t0);
    let a = c.revert(t0 + ms(1000));
    assert_eq!(target(sends(&a)[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    // Without a trial, keep and revert do nothing.
    c.on_event(ok(&c), t0 + ms(1100));
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(1110));
    assert!(c.revert(t0 + ms(1200)).is_empty());
    assert!(c.keep(t0 + ms(1200)).is_empty());
    assert_eq!(c.trial_state(t0 + ms(1200)), TrialState::Ended(TrialEnd::Reverted));
    assert_eq!(c.trial_state(t0 + ms(1200)), TrialState::None);
}

#[test]
fn keep_ends_the_trial() {
    let t0 = Instant::now();
    let mut c = trial_open(t0);
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(20)).is_empty());
    assert!(c.keep(t0 + ms(1000)).is_empty());
    assert_eq!(c.next_deadline(), None);
    assert_eq!(c.trial_state(t0 + ms(1000)), TrialState::Ended(TrialEnd::Kept));
    assert_eq!(c.trial_state(t0 + ms(1000)), TrialState::None);
    assert!(c.tick(&|| Vec::new(), t0 + ms(60_000)).is_empty());
}

#[test]
fn a_second_try_during_a_trial_is_refused() {
    let t0 = Instant::now();
    let mut c = trial_open(t0);
    assert_eq!(c.try_(&[move_to("DP-1", 5)], t0 + ms(100)), Err("another change is in progress"));
    assert_eq!(c.try_(&[], t0 + ms(100)), Err("another change is in progress"));
}

#[test]
fn a_refused_try_opens_no_trial() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    c.try_(&[move_to("DP-1", 10)], t0).unwrap();
    let a = c.on_event(failed(&c), t0 + ms(10));
    assert_eq!(replies(&a), vec![Err("the compositor refused this configuration")]);
    assert_eq!(c.trial_state(t0 + ms(10)), TrialState::None);
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn a_lost_screen_reverts_the_rest() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100), move_to("DP-2", 2020)], t0).unwrap();
    c.on_event(ok(&c), t0 + ms(10));
    // Bravo unplugged; a new screen appeared meanwhile and stays as it is.
    let a = c.on_event(Event::Done(vec![alpha(100), charlie(5000)]), t0 + ms(1000));
    let sent = sends(&a);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].len(), 2);
    assert_eq!(target(sent[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    assert_eq!(target(sent[0], "DP-3").state, Some((0, 5000, 0, 1.0, 0)));
    assert_eq!(c.trial_state(t0 + ms(1000)), TrialState::Busy, "the end waits for the restore");
    // The changed screens wait for the revert's answer: only its watchdog is armed.
    assert_eq!(c.next_deadline(), Some(t0 + ms(1000) + WATCHDOG));
}

#[test]
fn a_done_before_the_answer_is_not_a_change_elsewhere() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    // A `done` from before our change lands first, then the answer.
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(5));
    c.on_event(ok(&c), t0 + ms(10));
    assert!(matches!(c.trial_state(t0 + ms(10)), TrialState::Open { .. }));
    // Our change, reported next: still ours.
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(20)).is_empty());
    assert!(matches!(c.trial_state(t0 + ms(20)), TrialState::Open { .. }));
}

#[test]
fn a_revert_to_a_capped_mode_is_lowered_again() {
    let t0 = Instant::now();
    let none = Source::new("");
    let mut lowered = dock(1920);
    lowered.current = Some(1);
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0);
    assert_eq!(sends(&c.tick(&|| none.get(), t0 + DEBOUNCE)).len(), 1);
    c.on_event(ok(&c), t0 + ms(600));
    c.on_event(Event::Done(vec![alpha(0), lowered]), t0 + ms(610));
    // The compositor puts it back: not fought on the same screens.
    assert!(c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0 + ms(700)).is_empty());
    // A trial, reverted: the state from before (over the cap) comes back,
    // then the cap lowers it once more.
    c.try_(&[move_to("DP-1", 10)], t0 + ms(800)).unwrap();
    c.on_event(ok(&c), t0 + ms(810));
    c.on_event(Event::Done(vec![alpha(10), dock(1920)]), t0 + ms(820));
    assert_eq!(sends(&c.revert(t0 + ms(900))).len(), 1);
    c.on_event(ok(&c), t0 + ms(910));
    let a = c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0 + ms(920));
    assert_eq!(target(sends(&a)[0], "DVI-I-1").state, Some((1, 1920, 0, 1.0, 0)));
}

#[test]
fn a_revert_never_leaves_no_screen_on() {
    let t0 = Instant::now();
    let mut off = bravo(1920);
    off.enabled = false;
    off.current = None;
    let mut c = settled(vec![alpha(0), off], t0);
    // Bravo on, Alpha off; then Alpha, the only screen on before, is lost.
    c.try_(&[Step { output: "DP-1".into(), active: None }, move_to("DP-2", 0)], t0).unwrap();
    c.on_event(ok(&c), t0 + ms(10));
    let mut alpha_off = alpha(0);
    alpha_off.enabled = false;
    c.on_event(Event::Done(vec![alpha_off, bravo(0)]), t0 + ms(20));
    let a = c.on_event(Event::Done(vec![bravo(0)]), t0 + ms(1000));
    assert!(sends(&a).is_empty());
    assert_eq!(c.trial_state(t0 + ms(1000)), TrialState::Ended(TrialEnd::ScreenLost));
    // Only the debounce of the changed screens is armed.
    assert_eq!(c.next_deadline(), Some(t0 + ms(1000) + DEBOUNCE));
}

#[test]
fn a_change_elsewhere_ends_the_trial_without_reverting() {
    let t0 = Instant::now();
    let mut c = trial_open(t0);
    // Our own change, reported: the trial goes on.
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(20)).is_empty());
    assert!(matches!(c.trial_state(t0 + ms(20)), TrialState::Open { .. }));
    // Someone else moves Bravo.
    let a = c.on_event(Event::Done(vec![alpha(100), bravo(3000)]), t0 + ms(2000));
    assert!(sends(&a).is_empty());
    assert_eq!(c.trial_state(t0 + ms(2000)), TrialState::Ended(TrialEnd::ChangedElsewhere));
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn auto_waits_for_the_trial_then_runs_once() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0)]), t0);
    c.tick(&profiles, t0 + DEBOUNCE);
    assert_eq!(src.reads.get(), 1);
    c.try_(&[move_to("DP-1", 100)], t0 + ms(1000)).unwrap();
    c.on_event(ok(&c), t0 + ms(1010));
    // Bravo plugged in, then out and in again, during the trial: nothing yet.
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(0)]), t0 + ms(2000)).is_empty());
    assert!(c.on_event(Event::Done(vec![alpha(100)]), t0 + ms(2100)).is_empty());
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(0)]), t0 + ms(2200)).is_empty());
    assert!(c.tick(&profiles, t0 + ms(3000)).is_empty());
    assert_eq!(src.reads.get(), 1);
    assert_eq!(c.next_deadline(), Some(t0 + ms(1010) + TRIAL));
    // Kept: one evaluation, after the debounce.
    assert!(c.keep(t0 + ms(4000)).is_empty());
    let a = c.tick(&profiles, t0 + ms(4500));
    assert_eq!(src.reads.get(), 2);
    assert_eq!(sends(&a).len(), 1);
    c.on_event(ok(&c), t0 + ms(4600));
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(4610));
    assert!(c.tick(&profiles, t0 + ms(9000)).is_empty());
    assert_eq!(src.reads.get(), 2);
}

#[test]
fn auto_off_never_evaluates() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(false);
    c.on_event(Event::Done(vec![alpha(1920), bravo(0)]), t0);
    let a = c.tick(&profiles, t0 + DEBOUNCE);
    assert!(a.is_empty());
    c.on_event(Event::Done(vec![alpha(1920)]), t0 + ms(1000));
    assert!(c.tick(&profiles, t0 + ms(2000)).is_empty());
    assert_eq!(src.reads.get(), 0);
    assert_eq!(c.next_deadline(), None);
}

#[test]
fn next_deadline_is_none_at_rest() {
    let t0 = Instant::now();
    let c = Controller::new(true);
    assert_eq!(c.next_deadline(), None);
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    assert_eq!(c.trial_state(t0), TrialState::None);
    // A finished trial leaves nothing armed.
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    c.on_event(ok(&c), t0 + ms(10));
    c.revert(t0 + ms(100));
    c.on_event(ok(&c), t0 + ms(200));
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(210));
    assert_eq!(c.next_deadline(), None);
    assert_eq!(c.heads().len(), 2);
}

/// A dock screen at `x` whose modes are these sizes at 60 Hz, the first current.
fn screen(name: &str, serial: &str, x: i32, sizes: &[(i32, i32)]) -> Head {
    let mut h = head(name, "Dock", serial, x);
    h.displaylink = true;
    h.modes = sizes.iter().map(|&(width, height)| HeadMode { width, height, mhz: 60_000, preferred: false }).collect();
    h
}

const FHD: (i32, i32) = (1920, 1080);
const QHD: (i32, i32) = (2560, 1440);

#[test]
fn a_late_answer_is_not_taken_for_a_later_configuration() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    let first = sent_gen(&c.try_(&[move_to("DP-1", 100)], t0).unwrap());
    // No answer: the trial opens, possibly applied, and is reverted.
    assert_eq!(replies(&c.tick(&|| Vec::new(), t0 + WATCHDOG)), vec![Ok(())]);
    c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(5500));
    let second = sent_gen(&c.revert(t0 + ms(6000)));
    assert_ne!(first, second);
    // The first configuration's late answers are not the revert's.
    assert!(c.on_event(Event::Succeeded(first), t0 + ms(6100)).is_empty());
    assert!(c.on_event(Event::Failed(first), t0 + ms(6110)).is_empty());
    assert!(c.on_event(Event::Cancelled(first), t0 + ms(6120)).is_empty());
    assert_eq!(c.try_(&[move_to("DP-1", 5)], t0 + ms(6200)), Err("another change is in progress"));
    assert_eq!(c.next_deadline(), Some(t0 + ms(6000) + WATCHDOG));
    // The revert's own answer ends it.
    c.on_event(Event::Succeeded(second), t0 + ms(6300));
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(6310));
    assert!(c.try_(&[move_to("DP-1", 5)], t0 + ms(6400)).is_ok());
}

#[test]
fn the_first_done_after_the_answer_is_what_was_applied() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    let mode = Mode { width: 1920, height: 1080, refresh: 60 };
    let scaled = Active { mode, x: 0, y: 0, scale: Scale::from_f64(1.3).unwrap(), transform: 0 };
    let a = c.try_(&[Step { output: "DP-1".into(), active: Some(scaled) }], t0).unwrap();
    assert_eq!(target(sends(&a)[0], "DP-1").state, Some((0, 0, 0, 1.3, 0)));
    c.on_event(ok(&c), t0 + ms(10));
    // The compositor rounds the scale to one it accepts: still our change.
    let mut rounded = alpha(0);
    rounded.scale = 1.333;
    assert!(c.on_event(Event::Done(vec![rounded.clone(), bravo(1920)]), t0 + ms(20)).is_empty());
    assert!(c.on_event(Event::Done(vec![rounded, bravo(1920)]), t0 + ms(30)).is_empty());
    assert!(matches!(c.trial_state(t0 + ms(30)), TrialState::Open { .. }));
    // The safety net holds: it reverts at the deadline.
    let a = c.tick(&|| Vec::new(), t0 + ms(10) + TRIAL);
    assert_eq!(target(sends(&a)[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    assert_eq!(c.trial_state(t0 + ms(10) + TRIAL), TrialState::Busy, "until the restore lands");
}

#[test]
fn a_resend_follows_each_screen_to_its_new_connector() {
    let t0 = Instant::now();
    let one = screen("DVI-I-1", "A1", 0, &[FHD, QHD]);
    let two = screen("DVI-I-2", "B2", 1920, &[QHD, FHD]);
    let mut c = settled(vec![one, two], t0);
    c.try_(&[move_to("DVI-I-1", 100)], t0).unwrap();
    c.on_event(cancelled(&c), t0 + ms(10));
    // The dock re-enumerates: the screens swap connectors, and A lists its
    // modes in another order.
    let mut a1 = screen("DVI-I-2", "A1", 0, &[QHD, FHD]);
    a1.current = Some(1);
    let b2 = screen("DVI-I-1", "B2", 1920, &[QHD, FHD]);
    let a = c.on_event(Event::Done(vec![b2, a1]), t0 + ms(20));
    let sent = sends(&a);
    assert_eq!(sent.len(), 1);
    assert_eq!(target(sent[0], "DVI-I-2").state, Some((1, 100, 0, 1.0, 0)));
    assert_eq!(target(sent[0], "DVI-I-1").state, Some((0, 1920, 0, 1.0, 0)));
}

#[test]
fn a_resend_drops_a_screen_that_left() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100), move_to("DP-2", 2020)], t0).unwrap();
    c.on_event(cancelled(&c), t0 + ms(10));
    let a = c.on_event(Event::Done(vec![alpha(0)]), t0 + ms(20));
    let sent = sends(&a);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0], &vec![Target { name: "DP-1".into(), state: Some((0, 100, 0, 1.0, 0)) }]);
}

#[test]
fn a_revert_follows_each_screen_to_its_new_connector() {
    let t0 = Instant::now();
    let one = screen("DVI-I-1", "A1", 0, &[FHD, QHD]);
    let two = screen("DVI-I-2", "B2", 1920, &[QHD, FHD]);
    let mut c = settled(vec![one.clone(), two], t0);
    c.try_(&[move_to("DVI-I-1", 100)], t0).unwrap();
    c.on_event(ok(&c), t0 + ms(10));
    let mut moved = one;
    moved.x = 100;
    c.on_event(Event::Done(vec![moved, screen("DVI-I-2", "B2", 1920, &[QHD, FHD])]), t0 + ms(20));
    // Swapped connectors, same layout: the trial goes on.
    let mut a1 = screen("DVI-I-2", "A1", 100, &[QHD, FHD]);
    a1.current = Some(1);
    let b2 = screen("DVI-I-1", "B2", 1920, &[QHD, FHD]);
    assert!(c.on_event(Event::Done(vec![b2, a1]), t0 + ms(1000)).is_empty());
    assert!(matches!(c.trial_state(t0 + ms(1000)), TrialState::Open { .. }));
    let a = c.revert(t0 + ms(2000));
    let sent = sends(&a);
    assert_eq!(target(sent[0], "DVI-I-2").state, Some((1, 0, 0, 1.0, 0)));
    assert_eq!(target(sent[0], "DVI-I-1").state, Some((0, 1920, 0, 1.0, 0)));
}

#[test]
fn a_bounce_within_the_debounce_still_evaluates() {
    let t0 = Instant::now();
    let src = Source::new(SIDE_BY_SIDE);
    let profiles = || src.get();
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0);
    assert!(sends(&c.tick(&profiles, t0 + DEBOUNCE)).is_empty());
    // Bravo drops and comes back at once, re-created at the origin.
    assert!(c.on_event(Event::Done(vec![alpha(0)]), t0 + ms(1000)).is_empty());
    assert!(c.on_event(Event::Done(vec![alpha(0), bravo(0)]), t0 + ms(1100)).is_empty());
    assert_eq!(c.next_deadline(), Some(t0 + ms(1100) + DEBOUNCE));
    let a = c.tick(&profiles, t0 + ms(1600));
    assert_eq!(src.reads.get(), 2);
    assert_eq!(target(sends(&a)[0], "DP-2").state, Some((0, 1920, 0, 1.0, 0)));
}

#[test]
fn a_refused_revert_ends_the_trial_as_refused() {
    let t0 = Instant::now();
    let mut c = trial_open(t0);
    assert_eq!(sends(&c.revert(t0 + ms(1000))).len(), 1);
    c.on_event(failed(&c), t0 + ms(1010));
    assert_eq!(c.trial_state(t0 + ms(1010)), TrialState::Ended(TrialEnd::Refused));
    assert_eq!(c.trial_state(t0 + ms(1010)), TrialState::None);
    // At the deadline too.
    let mut c = trial_open(t0);
    c.tick(&|| Vec::new(), t0 + ms(10) + TRIAL);
    c.on_event(failed(&c), t0 + ms(20) + TRIAL);
    assert_eq!(c.trial_state(t0 + ms(20) + TRIAL), TrialState::Ended(TrialEnd::Refused));
}

#[test]
fn a_try_before_the_done_of_an_answered_change_is_refused() {
    let t0 = Instant::now();
    let none = Source::new("");
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0);
    assert_eq!(sends(&c.tick(&|| none.get(), t0 + DEBOUNCE)).len(), 1);
    c.on_event(ok(&c), t0 + ms(600));
    // The heads predate the cap's change until its `done`.
    assert_eq!(c.try_(&[move_to("DP-1", 10)], t0 + ms(610)), Err("another change is in progress"));
    let mut lowered = dock(1920);
    lowered.current = Some(1);
    c.on_event(Event::Done(vec![alpha(0), lowered]), t0 + ms(620));
    assert!(c.try_(&[move_to("DP-1", 10)], t0 + ms(630)).is_ok());
    // A `done` that never comes holds it no longer than the watchdog.
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(0), dock(1920)]), t0);
    c.tick(&|| none.get(), t0 + DEBOUNCE);
    c.on_event(ok(&c), t0 + ms(600));
    assert_eq!(c.next_deadline(), Some(t0 + ms(600) + WATCHDOG));
    assert!(c.tick(&|| none.get(), t0 + ms(600) + WATCHDOG).is_empty());
    assert_eq!(c.next_deadline(), None);
    assert!(c.try_(&[move_to("DP-1", 10)], t0 + ms(5700)).is_ok());
}

#[test]
fn a_resend_with_no_screen_left_on_is_refused() {
    let t0 = Instant::now();
    let mut off = bravo(1920);
    off.enabled = false;
    off.current = None;
    let mut c = settled(vec![alpha(0), off], t0);
    c.try_(&[Step { output: "DP-1".into(), active: None }, move_to("DP-2", 0)], t0).unwrap();
    c.on_event(cancelled(&c), t0 + ms(10));
    // Bravo, the screen the try turned on, left before the re-send.
    let mut alpha_off = alpha(0);
    alpha_off.enabled = false;
    let a = c.on_event(Event::Done(vec![alpha_off]), t0 + ms(20));
    assert!(sends(&a).is_empty());
    assert_eq!(replies(&a), vec![Err("the compositor refused this configuration")]);
    assert_eq!(c.trial_state(t0 + ms(20)), TrialState::None);
}

#[test]
fn our_result_reported_before_the_answer_is_adopted() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    // Our result's `done` comes first, then the answer.
    assert!(c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(5)).is_empty());
    c.on_event(ok(&c), t0 + ms(10));
    // Another tool moves Bravo: not ours, never reverted.
    let a = c.on_event(Event::Done(vec![alpha(100), bravo(3000)]), t0 + ms(2000));
    assert!(sends(&a).is_empty());
    assert_eq!(c.trial_state(t0 + ms(2000)), TrialState::Ended(TrialEnd::ChangedElsewhere));
    assert!(sends(&c.tick(&|| Vec::new(), t0 + ms(10) + TRIAL)).is_empty());
}

#[test]
fn a_configuration_never_sent_answers_invalid_not_refused() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    c.try_(&[move_to("DP-1", 10)], t0).unwrap();
    let a = c.on_event(Event::Invalid(gen_of(&c)), t0 + ms(1));
    assert_eq!(logs(&a), vec!["displays: configuration not sent (invalid)"]);
    assert_eq!(replies(&a), vec![Err("configuration not sent (invalid)")]);
    assert_eq!(c.trial_state(t0 + ms(1)), TrialState::None);
    assert_eq!(c.next_deadline(), None);
    assert!(c.on_event(Event::Invalid(99), t0 + ms(2)).is_empty(), "not the one in flight");
}

#[test]
fn the_flight_is_known_until_answered_or_given_up() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0)], t0);
    assert_eq!(c.in_flight(), None);
    let g = sent_gen(&c.try_(&[move_to("DP-1", 10)], t0).unwrap());
    assert_eq!(c.in_flight(), Some(g));
    c.tick(&Vec::new, t0 + WATCHDOG);
    assert_eq!(c.in_flight(), None, "given up by the watchdog");
    let mut c = settled(vec![alpha(0)], t0);
    c.try_(&[move_to("DP-1", 10)], t0).unwrap();
    c.on_event(failed(&c), t0 + ms(1));
    assert_eq!(c.in_flight(), None, "answered");
}

/// Fix round (stale snapshot): an end is reported only once the compositor showed it; until
/// then the trial reads Busy, and the end is not consumed.
#[test]
fn an_end_waits_for_the_compositor() {
    let t0 = Instant::now();
    let mut c = trial_open(t0);
    let a = c.revert(t0 + ms(1000));
    assert_eq!(sends(&a).len(), 1);
    assert_eq!(c.trial_state(t0 + ms(1000)), TrialState::Busy, "the restore flies");
    assert_eq!(c.trial_state(t0 + ms(1001)), TrialState::Busy, "Busy does not consume the end");
    c.on_event(ok(&c), t0 + ms(1100));
    assert_eq!(c.trial_state(t0 + ms(1100)), TrialState::Busy, "answered, its done not yet read");
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(1110));
    assert_eq!(c.trial_state(t0 + ms(1110)), TrialState::Ended(TrialEnd::Reverted));
    assert_eq!(c.trial_state(t0 + ms(1110)), TrialState::None);

    // The deadline's revert and a lost screen too.
    let mut c = trial_open(t0);
    c.tick(&|| Vec::new(), t0 + ms(10) + TRIAL);
    assert_eq!(c.trial_state(t0 + ms(10) + TRIAL), TrialState::Busy);
    c.on_event(ok(&c), t0 + ms(20) + TRIAL);
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(30) + TRIAL);
    assert_eq!(c.trial_state(t0 + ms(30) + TRIAL), TrialState::Ended(TrialEnd::Reverted));
    let mut c = trial_open(t0);
    c.on_event(Event::Done(vec![alpha(100)]), t0 + ms(1000));
    assert_eq!(c.trial_state(t0 + ms(1000)), TrialState::Busy);
    c.on_event(ok(&c), t0 + ms(1100));
    c.on_event(Event::Done(vec![alpha(0)]), t0 + ms(1110));
    assert_eq!(c.trial_state(t0 + ms(1110)), TrialState::Ended(TrialEnd::ScreenLost));

    // A profile applied on hotplug reads Busy too, with no end to report after it.
    let src = Source::new(SIDE_BY_SIDE);
    let mut c = Controller::new(true);
    c.on_event(Event::Done(vec![alpha(500), bravo(0)]), t0);
    assert_eq!(sends(&c.tick(&|| src.get(), t0 + DEBOUNCE)).len(), 1);
    assert_eq!(c.trial_state(t0 + DEBOUNCE), TrialState::Busy);
    c.on_event(ok(&c), t0 + ms(600));
    assert_eq!(c.trial_state(t0 + ms(600)), TrialState::Busy);
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(610));
    assert_eq!(c.trial_state(t0 + ms(610)), TrialState::None);
}

/// Fix round (close during a try's flight): a revert asked while the try flies reverts as soon
/// as its trial would open, even on heads that do not show the try yet.
#[test]
fn a_revert_asked_while_the_try_flies_reverts_once_it_lands() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    assert!(c.revert(t0 + ms(1)).is_empty(), "nothing to revert yet");
    assert_eq!(c.trial_state(t0 + ms(1)), TrialState::Busy);
    // Answered before its done: the heads still show the state from before.
    let a = c.on_event(ok(&c), t0 + ms(10));
    assert_eq!(replies(&a), vec![Ok(())]);
    let sent = sends(&a);
    assert_eq!(sent.len(), 1, "the state from before sent at once: {a:?}");
    assert_eq!(target(sent[0], "DP-1").state, Some((0, 0, 0, 1.0, 0)));
    assert_eq!(c.trial_state(t0 + ms(10)), TrialState::Busy);
    c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(15));
    c.on_event(ok(&c), t0 + ms(20));
    c.on_event(Event::Done(vec![alpha(0), bravo(1920)]), t0 + ms(25));
    assert_eq!(c.trial_state(t0 + ms(25)), TrialState::Ended(TrialEnd::Reverted));
    assert_eq!(c.trial_state(t0 + ms(25)), TrialState::None);
    assert_eq!(c.next_deadline(), None, "no trial left open");

    // Unanswered: the watchdog's possibly-applied trial is reverted at once.
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    c.revert(t0 + ms(1));
    let a = c.tick(&|| Vec::new(), t0 + WATCHDOG);
    assert_eq!(replies(&a), vec![Ok(())]);
    assert_eq!(sends(&a).len(), 1, "{a:?}");
    assert_eq!(c.next_deadline(), Some(t0 + WATCHDOG + WATCHDOG), "only the revert's watchdog");

    // A refused try opens nothing and the revert asked goes with it.
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    c.revert(t0 + ms(1));
    let a = c.on_event(failed(&c), t0 + ms(10));
    assert!(sends(&a).is_empty());
    assert_eq!(c.trial_state(t0 + ms(10)), TrialState::None);
    assert_eq!(c.next_deadline(), None);
}

/// A keep asked while the try flies keeps once it lands; a revert asked as well wins.
#[test]
fn a_keep_asked_while_the_try_flies_keeps_once_it_lands() {
    let t0 = Instant::now();
    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    assert!(c.keep(t0 + ms(1)).is_empty());
    let a = c.on_event(ok(&c), t0 + ms(10));
    assert_eq!(replies(&a), vec![Ok(())]);
    assert!(sends(&a).is_empty());
    c.on_event(Event::Done(vec![alpha(100), bravo(1920)]), t0 + ms(15));
    assert_eq!(c.trial_state(t0 + ms(15)), TrialState::Ended(TrialEnd::Kept));
    assert_eq!(c.next_deadline(), None, "no trial deadline");

    let mut c = settled(vec![alpha(0), bravo(1920)], t0);
    c.try_(&[move_to("DP-1", 100)], t0).unwrap();
    c.revert(t0 + ms(1));
    c.keep(t0 + ms(2));
    assert_eq!(sends(&c.on_event(ok(&c), t0 + ms(10))).len(), 1, "the revert wins");
}
