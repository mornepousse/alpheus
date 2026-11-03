//! The output controller: one configuration in flight, the hotplug
//! debounce and the automatic profiles, the DisplayLink 120 Hz rule and
//! the 15 s trial. Pure: no Wayland and no clock (every call takes `now`);
//! the glue turns protocol events into `Event`s and runs the `Action`s.

use super::model::{Head, HeadMode, KEEP, NO_SCREEN, Target, is_noop, over_cap, targets, to_output};
use alpheus_commun::displays::Step;
use alpheus_commun::displays::profiles::{Profile, ProfileMatch, match_profile, plan_profile};
use alpheus_commun::displays::wire::{TrialEnd, TrialState};
use std::time::{Duration, Instant};

/// Quiet time after the last `done` that changed the screens.
pub const DEBOUNCE: Duration = Duration::from_millis(500);
/// How long a configuration waits for the compositor's answer.
pub const WATCHDOG: Duration = Duration::from_secs(5);
/// How long a trial waits for Keep before reverting.
pub const TRIAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The manager's `done`: every head as it now is.
    Done(Vec<Head>),
    /// The answers to the configuration sent with that generation.
    Succeeded(u64),
    Failed(u64),
    Cancelled(u64),
    /// That configuration could not be built (the compositor would raise a
    /// protocol error): never sent, answered as a refusal of its own.
    Invalid(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyTo {
    Try,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Build and apply one configuration: every head, enabled or not. Its
    /// answer comes back with the same generation.
    Send(u64, Vec<Target>),
    Log(&'static str),
    /// `displays: profile "<name>" does not resolve`.
    LogProfile(String),
    Reply(ReplyTo, Result<(), &'static str>),
}

const LOG_REFUSED: &str = "displays: configuration refused by the compositor";
const LOG_NO_ANSWER: &str = "displays: no answer from the compositor";
const LOG_NO_PROFILE: &str = "displays: no profile for these screens";
const LOG_TOO_MANY: &str = "displays: profile not re-applied (too many changes)";
const LOG_TWINS: &str = "displays: two screens look the same";
const LOG_LOWERED: &str = "displays: lowered a DisplayLink screen to 120 Hz";
const BUSY: &str = "another change is in progress";
const REFUSED: &str = "the compositor refused this configuration";
const LOG_INVALID: &str = "displays: configuration not sent (invalid)";
const INVALID: &str = "configuration not sent (invalid)";
/// `cancelled` answers re-sent before giving up.
const MAX_RESENDS: u8 = 3;
/// Automatic applies for one set of screens within `AUTO_WINDOW`, at most (a dock reconnecting
/// in a loop must not modeset forever).
const MAX_AUTO_APPLIES: usize = 3;
const AUTO_WINDOW: Duration = Duration::from_secs(60);
/// Applies and skip lines remembered, at most (each needs a debounce: a handful a minute).
const MAX_AUTO_HISTORY: usize = 32;

/// Why a configuration was sent: what its answer leads to.
enum Purpose {
    /// A profile applied on hotplug.
    Auto,
    /// The DisplayLink 120 Hz rule.
    Cap,
    /// A trial asked by the TUI; `before` is what to restore. `asked`: a keep or a revert
    /// received while it flies, run as soon as its trial opens (a revert wins).
    Try { before: Vec<Kept>, asked: Option<TrialEnd> },
    /// The end of a trial, restoring the state from before.
    Revert,
}

/// A target remembered with its screen, to be re-sent or restored on
/// whichever connector that screen is on by then.
#[derive(Debug, Clone)]
struct Kept {
    identity: String,
    /// The connector it was on: breaks ties between twins.
    name: String,
    /// The mode its index named (width, height, mHz); `None` for `KEEP`.
    mode: Option<(i32, i32, i32)>,
    state: Option<(usize, i32, i32, f64, u8)>,
}

/// How a configuration in flight ended.
#[derive(Clone, Copy, PartialEq)]
enum Answer {
    Applied,
    Refused,
    /// Never sent: it could not be built.
    Invalid,
}

/// The one configuration waiting for the compositor's answer.
struct Flight {
    /// The generation its `Send` and its answers carry.
    generation: u64,
    purpose: Purpose,
    targets: Vec<Kept>,
    watchdog: Instant,
    resends: u8,
    /// A `done` arrived since it was sent (a `cancelled` after it is about those heads).
    done_since: bool,
    /// Sent while the heads were stale: the next `done` reports the earlier change, not this one.
    behind: bool,
    /// A `done` that may report this configuration arrived (not the one it was behind).
    own_done: bool,
    /// `cancelled`: send it again on the next `done`.
    resend: bool,
}

struct Trial {
    before: Vec<Kept>,
    applied: Vec<Kept>,
    deadline: Instant,
    /// The compositor said `succeeded` (not the watchdog).
    confirmed: bool,
    /// `applied` is what a `done` after the answer reported (the compositor
    /// may round a scale or a rate): only later `done`s are judged.
    adopted: bool,
}

pub struct Controller {
    auto: bool,
    heads: Vec<Head>,
    /// Identity set (sorted) of the last evaluation; `None` before the first.
    evaluated: Option<Vec<String>>,
    /// A `done` changed the identity set since the last evaluation (a screen
    /// that left and came back inside the debounce still counts).
    changed: bool,
    debounce: Option<Instant>,
    /// Heads the cap already lowered for this identity set (connectors).
    lowered: Vec<String>,
    flight: Option<Flight>,
    /// The last generation sent.
    generation: u64,
    /// After a `succeeded`, until its `done` (at most the watchdog): the
    /// heads predate that change, so no try is taken.
    stale: Option<Instant>,
    trial: Option<Trial>,
    /// How the last trial ended, until `trial_state` reports it.
    ended: Option<TrialEnd>,
    /// A try was sent since the controller started.
    trial_seen: bool,
    /// The automatic applies sent in the last `AUTO_WINDOW` (identity set, when).
    auto_applies: Vec<(Vec<String>, Instant)>,
    /// The skip lines logged in the last `AUTO_WINDOW` (identity set, when).
    auto_skips: Vec<(Vec<String>, Instant)>,
}

impl Controller {
    pub fn new(auto: bool) -> Self {
        Controller {
            auto,
            heads: Vec::new(),
            evaluated: None,
            changed: false,
            debounce: None,
            lowered: Vec::new(),
            flight: None,
            generation: 0,
            stale: None,
            trial: None,
            ended: None,
            trial_seen: false,
            auto_applies: Vec::new(),
            auto_skips: Vec::new(),
        }
    }

    /// The generation of the configuration waiting for its answer, if any.
    pub fn in_flight(&self) -> Option<u64> {
        self.flight.as_ref().map(|f| f.generation)
    }

    /// Whether a try was ever sent by this controller (since the daemon started).
    pub fn trial_seen(&self) -> bool {
        self.trial_seen
    }

    /// The daemon stops: the state from before a trial, open or flying, is sent back (sent even
    /// when the heads still show it: the try may land yet). Its answer is not waited for.
    pub fn shutdown(&mut self, now: Instant) -> Vec<Action> {
        let before = match (self.trial.take(), self.flight.take_if(|f| matches!(f.purpose, Purpose::Try { .. }))) {
            (Some(t), _) => t.before,
            (None, Some(Flight { purpose: Purpose::Try { before, .. }, .. })) => before,
            _ => return Vec::new(),
        };
        let targets = self.placed(&before);
        self.send_as(targets, Purpose::Revert, true, 0, now + WATCHDOG, now)
    }

    /// The heads of the last `done`.
    pub fn heads(&self) -> &[Head] {
        &self.heads
    }

    pub fn on_event(&mut self, e: Event, now: Instant) -> Vec<Action> {
        // An answer to a configuration no longer in flight (given up by the
        // watchdog, or re-sent since) is not this one's.
        if let Event::Succeeded(g) | Event::Failed(g) | Event::Cancelled(g) | Event::Invalid(g) = e
            && self.flight.as_ref().is_none_or(|f| f.generation != g)
        {
            return Vec::new();
        }
        match e {
            Event::Done(heads) => {
                let ids = self.identities();
                self.heads = heads;
                if self.identities() != ids {
                    self.changed = true;
                }
                self.stale = None;
                if let Some(f) = &mut self.flight {
                    if f.resend {
                        return self.resend(now);
                    }
                    f.done_since = true;
                    if f.behind {
                        f.behind = false;
                    } else {
                        f.own_done = true;
                    }
                    return Vec::new();
                }
                let mut out = self.check_trial(true, now);
                out.extend(self.settle(true, true, now));
                out
            }
            // Applied: the heads are stale until its `done`.
            Event::Succeeded(_) => self.answer(Answer::Applied, false, now),
            Event::Failed(_) => self.answer(Answer::Refused, true, now),
            Event::Invalid(_) => self.answer(Answer::Invalid, true, now),
            Event::Cancelled(_) => {
                let Some(f) = &mut self.flight else { return Vec::new() };
                if f.resends >= MAX_RESENDS {
                    return self.answer(Answer::Refused, true, now);
                }
                if f.done_since {
                    return self.resend(now);
                }
                f.resend = true;
                Vec::new()
            }
        }
    }

    /// Starts a trial of `steps`. `Err` answers at once; with `Ok`, the
    /// answer comes as a `Reply(Try, ..)` action, now or later.
    pub fn try_(&mut self, steps: &[Step], now: Instant) -> Result<Vec<Action>, &'static str> {
        if self.busy() || self.stale.is_some() {
            return Err(BUSY);
        }
        let ok = vec![Action::Reply(ReplyTo::Try, Ok(()))];
        // Nothing asked, or nothing would change: nothing to try.
        if steps.is_empty() {
            return Ok(ok);
        }
        let t = targets(&self.heads, steps).map_err(|e| if e == NO_SCREEN { NO_SCREEN } else { REFUSED })?;
        if is_noop(&self.heads, &t) {
            return Ok(ok);
        }
        self.ended = None;
        let before = self.remember(self.current());
        self.trial_seen = true;
        Ok(self.send(t, Purpose::Try { before, asked: None }, 0, now + WATCHDOG, now))
    }

    pub fn keep(&mut self, now: Instant) -> Vec<Action> {
        if let Some(asked) = self.asked() {
            asked.get_or_insert(TrialEnd::Kept);
            return Vec::new();
        }
        if self.trial.take().is_none() {
            return Vec::new();
        }
        self.end(TrialEnd::Kept);
        self.settle(false, true, now)
    }

    pub fn revert(&mut self, now: Instant) -> Vec<Action> {
        if let Some(asked) = self.asked() {
            *asked = Some(TrialEnd::Reverted);
            return Vec::new();
        }
        let Some(t) = self.trial.take() else { return Vec::new() };
        self.end(TrialEnd::Reverted);
        self.restore(&t, now)
    }

    /// What a try in flight will do once its trial opens.
    fn asked(&mut self) -> Option<&mut Option<TrialEnd>> {
        match &mut self.flight {
            Some(Flight { purpose: Purpose::Try { asked, .. }, .. }) => Some(asked),
            _ => None,
        }
    }

    /// Runs whatever deadline passed. `profiles` is read only when the
    /// debounce ends with the automatic profiles on.
    pub fn tick(&mut self, profiles: &dyn Fn() -> Vec<Profile>, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(f) = self.flight.take_if(|f| now >= f.watchdog) {
            out.push(Action::Log(LOG_NO_ANSWER));
            // Possibly applied: a trial still opens, so it can be reverted.
            if let Purpose::Try { before, asked } = f.purpose {
                out.push(Action::Reply(ReplyTo::Try, Ok(())));
                out.extend(self.open_trial(before, f.targets, false, f.own_done, now));
                out.extend(self.run_asked(asked, now));
            }
            out.extend(self.settle(false, true, now));
        }
        self.stale.take_if(|s| now >= *s);
        if let Some(t) = self.trial.take_if(|t| now >= t.deadline) {
            self.end(TrialEnd::Reverted);
            out.extend(self.restore(&t, now));
        }
        if self.debounce.take_if(|d| now >= *d).is_some() {
            out.extend(self.evaluate(profiles, now));
        }
        out
    }

    /// The earliest deadline armed: watchdog, the wait for a `done`, trial
    /// or debounce.
    pub fn next_deadline(&self) -> Option<Instant> {
        [self.flight.as_ref().map(|f| f.watchdog), self.stale, self.trial.as_ref().map(|t| t.deadline), self.debounce]
            .into_iter()
            .flatten()
            .min()
    }

    /// The trial for the TUI; an end is reported once, after the compositor showed it: while a
    /// configuration flies or the heads predate its answer, Busy (the end kept).
    pub fn trial_state(&mut self, now: Instant) -> TrialState {
        if let Some(t) = &self.trial {
            let left = t.deadline.saturating_duration_since(now).as_millis().div_ceil(1000);
            return TrialState::Open { seconds_left: left as u32 };
        }
        if self.flight.is_some() || self.stale.is_some() {
            return TrialState::Busy;
        }
        self.ended.take().map_or(TrialState::None, TrialState::Ended)
    }

    fn busy(&self) -> bool {
        self.flight.is_some() || self.trial.is_some()
    }

    fn identities(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.heads.iter().map(Head::identity).collect();
        ids.sort();
        ids
    }

    /// Every head as it is now.
    fn current(&self) -> Vec<Target> {
        self.heads.iter().map(now_target).collect()
    }

    /// `targets` (named after the current heads) with their screens.
    fn remember(&self, targets: Vec<Target>) -> Vec<Kept> {
        targets
            .into_iter()
            .filter_map(|t| {
                let h = self.heads.iter().find(|h| h.name == t.name)?;
                let mode = t.state.and_then(|s| h.modes.get(s.0)).map(|m| (m.width, m.height, m.mhz));
                Some(Kept { identity: h.identity(), name: t.name, mode, state: t.state })
            })
            .collect()
    }

    /// `kept` on the heads there are now: each screen where it now is, its
    /// mode found again by size and rate; a screen gone is dropped and a
    /// head nothing names (or whose mode is gone) stays as it is.
    fn placed(&self, kept: &[Kept]) -> Vec<Target> {
        let on = assign(&self.heads, kept);
        self.heads.iter().zip(on).map(|(h, k)| k.and_then(|k| place(h, k)).unwrap_or_else(|| now_target(h))).collect()
    }

    /// A screen of `kept` is on no head any more.
    fn lost(&self, kept: &[Kept]) -> bool {
        assign(&self.heads, kept).iter().flatten().count() < kept.len()
    }

    /// Whether the screens named in `kept` are in that state.
    fn shows(&self, kept: &[Kept]) -> bool {
        let on = assign(&self.heads, kept);
        on.iter().flatten().count() == kept.len()
            && self
                .heads
                .iter()
                .zip(on)
                .all(|(h, k)| k.is_none_or(|k| place(h, k).is_some_and(|t| is_noop(std::slice::from_ref(h), std::slice::from_ref(&t)))))
    }

    fn end(&mut self, how: TrialEnd) {
        self.ended = Some(how);
        // A user's change: the cap may lower a restored mode once more.
        self.lowered.clear();
    }

    /// Sends `targets`, or succeeds at once when they change nothing.
    fn send(&mut self, targets: Vec<Target>, purpose: Purpose, resends: u8, watchdog: Instant, now: Instant) -> Vec<Action> {
        self.send_as(targets, purpose, false, resends, watchdog, now)
    }

    /// `send`; with `force`, sent even when the heads already show it.
    fn send_as(
        &mut self,
        targets: Vec<Target>,
        purpose: Purpose,
        force: bool,
        resends: u8,
        watchdog: Instant,
        now: Instant,
    ) -> Vec<Action> {
        // The last screen: never a configuration with none enabled.
        if targets.iter().all(|t| t.state.is_none()) {
            return Vec::new();
        }
        self.debounce = None;
        let noop = !force && is_noop(&self.heads, &targets);
        let sent = if noop {
            Vec::new()
        } else {
            self.generation += 1;
            vec![Action::Send(self.generation, targets.clone())]
        };
        let targets = self.remember(targets);
        let behind = self.stale.is_some();
        self.flight = Some(Flight {
            generation: self.generation,
            purpose,
            targets,
            watchdog,
            resends,
            done_since: false,
            behind,
            own_done: false,
            resend: false,
        });
        if noop {
            return self.answer(Answer::Applied, true, now);
        }
        sent
    }

    /// After a `cancelled`, the same configuration again on the heads there
    /// are now (each screen followed to its connector).
    fn resend(&mut self, now: Instant) -> Vec<Action> {
        let Some(f) = self.flight.take() else { return Vec::new() };
        // A profile or a cap chosen for a set of screens that changed since: dropped, the new
        // set is evaluated after its debounce.
        if self.changed && matches!(f.purpose, Purpose::Auto | Purpose::Cap) {
            return self.settle(false, true, now);
        }
        let targets = self.placed(&f.targets);
        if targets.iter().all(|t| t.state.is_none()) {
            // Every screen it left on is gone: answered as refused.
            self.flight = Some(f);
            return self.answer(Answer::Refused, true, now);
        }
        // The first send's watchdog stands: a client's own wait does not restart.
        self.send(targets, f.purpose, f.resends + 1, f.watchdog, now)
    }

    /// The compositor's answer (`succeeded` or a refusal), or ours for a
    /// configuration never sent. `fresh`: the heads already show the outcome
    /// (else the cap waits for the `done`).
    fn answer(&mut self, answer: Answer, fresh: bool, now: Instant) -> Vec<Action> {
        let Some(f) = self.flight.take() else { return Vec::new() };
        let mut out = Vec::new();
        let ok = answer == Answer::Applied;
        let (log, reply) = if answer == Answer::Invalid { (LOG_INVALID, INVALID) } else { (LOG_REFUSED, REFUSED) };
        if !ok {
            out.push(Action::Log(log));
        }
        if ok && !fresh && !f.own_done {
            self.stale = Some(now + WATCHDOG);
        }
        match f.purpose {
            Purpose::Try { before, asked } if ok => {
                out.push(Action::Reply(ReplyTo::Try, Ok(())));
                out.extend(self.open_trial(before, f.targets, true, f.own_done, now));
                out.extend(self.run_asked(asked, now));
            }
            Purpose::Try { .. } => out.push(Action::Reply(ReplyTo::Try, Err(reply))),
            // The screens did not come back: the TUI must not say they did.
            Purpose::Revert if !ok => self.ended = Some(TrialEnd::Refused),
            _ => {}
        }
        out.extend(self.settle(false, fresh || f.own_done, now));
        out
    }

    fn open_trial(&mut self, before: Vec<Kept>, applied: Vec<Kept>, confirmed: bool, done_since: bool, now: Instant) -> Vec<Action> {
        self.trial = Some(Trial { before, applied, deadline: now + TRIAL, confirmed, adopted: false });
        if !done_since {
            return Vec::new();
        }
        // A screen may have gone while it waited. A `done` that no longer
        // shows the state from before carried our result: adopted now, so
        // the next `done` is judged. One still showing it may predate our
        // change: the next `done` is adopted instead.
        let out = self.check_trial(false, now);
        if self.trial.as_ref().is_some_and(|t| !self.shows(&t.before)) {
            self.adopt();
        }
        out
    }

    /// A keep or a revert asked while the try flew, on the trial it opened (unless a lost screen
    /// already ended it).
    fn run_asked(&mut self, asked: Option<TrialEnd>, now: Instant) -> Vec<Action> {
        let Some(how) = asked else { return Vec::new() };
        let Some(t) = self.trial.take() else { return Vec::new() };
        self.end(how);
        if how == TrialEnd::Reverted { self.restore(&t, now) } else { Vec::new() }
    }

    /// The trial's own screens, as the heads now report them, become what
    /// was applied (a screen plugged meanwhile is not part of it).
    fn adopt(&mut self) {
        let Some(t) = &self.trial else { return };
        let on = assign(&self.heads, &t.applied);
        let ours = self.heads.iter().zip(on).filter(|(_, k)| k.is_some()).map(|(h, _)| now_target(h)).collect();
        let applied = self.remember(ours);
        let t = self.trial.as_mut().expect("checked above");
        t.applied = applied;
        t.adopted = true;
    }

    /// A trial against the heads: a lost screen reverts the rest. With
    /// `judge`, the first `done` after the answer is adopted as what was
    /// applied (unconfirmed, unless it still shows the state from before:
    /// the change may land later); a later state other than that one ends
    /// it as changed elsewhere.
    fn check_trial(&mut self, judge: bool, now: Instant) -> Vec<Action> {
        let Some(t) = &self.trial else { return Vec::new() };
        if self.lost(&t.applied) {
            let t = self.trial.take().expect("checked above");
            self.end(TrialEnd::ScreenLost);
            return self.restore(&t, now);
        }
        if !judge {
            return Vec::new();
        }
        if !t.adopted {
            if t.confirmed || !self.shows(&t.before) {
                self.adopt();
            }
        } else if !self.shows(&t.applied) {
            self.trial = None;
            self.end(TrialEnd::ChangedElsewhere);
        }
        Vec::new()
    }

    /// The trial's state from before on the heads still there; the others stay as they are.
    /// Sent even when the heads already show it while they may predate the trial's change (not
    /// adopted yet): that change can still land.
    fn restore(&mut self, t: &Trial, now: Instant) -> Vec<Action> {
        let targets = self.placed(&t.before);
        self.send_as(targets, Purpose::Revert, !t.adopted, 0, now + WATCHDOG, now)
    }

    /// Once nothing is in flight and no trial is open: a changed identity
    /// set (re)arms the debounce (only a `done` pushes it back), an
    /// unchanged one runs the cap (when `cap`: the heads are current).
    fn settle(&mut self, from_done: bool, cap: bool, now: Instant) -> Vec<Action> {
        if self.busy() {
            // Run again when it ends: the identity set is compared then.
            self.debounce = None;
            return Vec::new();
        }
        if self.changed || self.evaluated.as_ref() != Some(&self.identities()) {
            if from_done || self.debounce.is_none() {
                self.debounce = Some(now + DEBOUNCE);
            }
            return Vec::new();
        }
        self.debounce = None;
        if cap { self.cap(now) } else { Vec::new() }
    }

    /// The debounce ended: the matching profile (when `auto`), then the cap.
    fn evaluate(&mut self, profiles: &dyn Fn() -> Vec<Profile>, now: Instant) -> Vec<Action> {
        self.evaluated = Some(self.identities());
        self.changed = false;
        self.lowered.clear();
        let mut out = Vec::new();
        if self.auto {
            let profiles = profiles();
            let outs: Vec<_> = self.heads.iter().map(to_output).collect();
            match match_profile(&profiles, &outs) {
                ProfileMatch::Matches(p) => {
                    match plan_profile(p, &outs).ok().and_then(|pp| targets(&self.heads, &pp.plan.steps).ok()) {
                        // Already shown: nothing sent, nothing counted.
                        Some(t) if is_noop(&self.heads, &t) => return self.send(t, Purpose::Auto, 0, now + WATCHDOG, now),
                        Some(_) if self.too_many_applies(now) => {
                            if self.skip_line_due(now) {
                                out.push(Action::Log(LOG_TOO_MANY));
                            }
                        }
                        // A refusal is remembered: `evaluated` holds this set.
                        Some(t) => {
                            self.note_auto(true, now);
                            return self.send(t, Purpose::Auto, 0, now + WATCHDOG, now);
                        }
                        None => out.push(Action::LogProfile(p.name.clone())),
                    }
                }
                ProfileMatch::Ambiguous => out.push(Action::Log(LOG_TWINS)),
                // Silent without a profile at all (no displays.toml, or an empty one).
                ProfileMatch::NoMatch if !profiles.is_empty() => out.push(Action::Log(LOG_NO_PROFILE)),
                ProfileMatch::NoMatch => {}
            }
        }
        out.extend(self.cap(now));
        out
    }

    /// This set of screens already had `MAX_AUTO_APPLIES` automatic applies in the last
    /// `AUTO_WINDOW` (older ones forgotten).
    fn too_many_applies(&mut self, now: Instant) -> bool {
        let fresh = |e: &(Vec<String>, Instant)| now.saturating_duration_since(e.1) < AUTO_WINDOW;
        self.auto_applies.retain(fresh);
        self.auto_skips.retain(fresh);
        let ids = self.evaluated.as_ref();
        self.auto_applies.iter().filter(|(s, _)| Some(s) == ids).count() >= MAX_AUTO_APPLIES
    }

    /// The skip line, once per window for this set.
    fn skip_line_due(&mut self, now: Instant) -> bool {
        let ids = self.evaluated.as_ref();
        if self.auto_skips.iter().any(|(s, _)| Some(s) == ids) {
            return false;
        }
        self.note_auto(false, now);
        true
    }

    /// Remembers an apply (or a skip line) for the evaluated set, the oldest dropped past the cap.
    fn note_auto(&mut self, apply: bool, now: Instant) {
        let list = if apply { &mut self.auto_applies } else { &mut self.auto_skips };
        if list.len() >= MAX_AUTO_HISTORY {
            list.remove(0);
        }
        list.push((self.evaluated.clone().unwrap_or_default(), now));
    }

    /// DisplayLink heads above 120 Hz lowered, once per head per identity set.
    fn cap(&mut self, now: Instant) -> Vec<Action> {
        let mut targets = self.current();
        let mut out = Vec::new();
        for (h, t) in self.heads.iter().zip(&mut targets) {
            if self.lowered.contains(&h.name) {
                continue;
            }
            if let (Some(i), Some(state)) = (over_cap(h), &mut t.state) {
                state.0 = i;
                self.lowered.push(h.name.clone());
                out.push(Action::Log(LOG_LOWERED));
            }
        }
        if !out.is_empty() {
            out.extend(self.send(targets, Purpose::Cap, 0, now + WATCHDOG, now));
        }
        out
    }
}

/// The head as it is now, by the model's own rule (`KEEP` for an unknown
/// mode, sanitised scale and transform).
fn now_target(h: &Head) -> Target {
    targets(std::slice::from_ref(h), &[]).ok().and_then(|mut t| t.pop()).unwrap_or_else(|| Target { name: h.name.clone(), state: None })
}

/// Which kept screen each head is, by identity; among twins, the one that
/// was on the same connector first (as profiles match them).
fn assign<'k>(heads: &[Head], kept: &'k [Kept]) -> Vec<Option<&'k Kept>> {
    let ids: Vec<String> = heads.iter().map(Head::identity).collect();
    let mut on = vec![None; heads.len()];
    let mut used = vec![false; kept.len()];
    for same_connector in [true, false] {
        for (i, h) in heads.iter().enumerate() {
            if on[i].is_some() {
                continue;
            }
            let pick = (0..kept.len()).find(|&j| !used[j] && kept[j].identity == ids[i] && (!same_connector || kept[j].name == h.name));
            if let Some(j) = pick {
                used[j] = true;
                on[i] = Some(&kept[j]);
            }
        }
    }
    on
}

/// `k` on head `h`: its mode index found again by size and rate (the
/// current one first); `None` when `h` no longer has that mode.
fn place(h: &Head, k: &Kept) -> Option<Target> {
    let state = match (k.state, k.mode) {
        (None, _) => None,
        (Some(s), None) => Some((KEEP, s.1, s.2, s.3, s.4)),
        (Some(s), Some(mode)) => {
            let same = |m: &HeadMode| (m.width, m.height, m.mhz) == mode;
            let i = h.current.filter(|&c| h.modes.get(c).is_some_and(same)).or_else(|| h.modes.iter().position(same))?;
            Some((i, s.1, s.2, s.3, s.4))
        }
    };
    Some(Target { name: h.name.clone(), state })
}

#[cfg(test)]
mod tests;
