//! The quick panel's VPN (quick VPN spec), pure: the row's value, the profiles listed (active
//! first, then by name, at most PROFILES_MAX), the exact `nmcli` commands to run and the connection
//! attempt they start, followed to its end (`Attempt`). The state comes from the D-Bus thread
//! (`sources/dbus/vpn.rs`) only while the panel is open. The detail's lines, keys, clicks and rows
//! are here too; the row on the panel and the window over the lines are `quick.rs`'s.

use crate::actions::Button;
use crate::config::QuickConfig;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::keys::Key;
pub use crate::quick::FAILED_SHOWN;
use crate::quick::{FAILED_TAG, Line, QuickAction, RUNNING_TAG, moved, nav, run, run_and_close, text_row};
use crate::quick_wifi::NM_GONE;
pub use crate::quick_wifi::{ACTIVATION_BOUND, CONNECT_TIMEOUT};
use crate::session;
use crate::system::{VpnKind, VpnScope, VpnState};
use std::time::Instant;

/// The most profiles the detail lists.
pub const PROFILES_MAX: usize = 16;
/// The row's value: at most this many cells of the active profile's name.
pub const VALUE_CELLS: u16 = 16;
/// md-shield-lock: a WireGuard profile's detail prefix.
pub const WIREGUARD: char = '\u{f099d}';
/// NMActiveConnectionState: activating, activated, deactivating.
pub const ACTIVATING: u32 = 1;
pub const ACTIVATED: u32 = 2;
pub const DEACTIVATING: u32 = 3;

/// Why an attempt failed, for the log (never a name or a uuid).
const ACTIVATION_FAILED: &str = "the activation failed";
const NEEDS_SECRET: &str = "the connection may need a secret";
const NO_ANSWER: &str = "no answer in 20 s";
const STILL_ACTIVATING: &str = "still activating after 60 s";
const PROFILE_GONE: &str = "the profile went away";

/// A profile as the detail lists it: `state` is its active connection's, None when it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub uuid: String,
    pub label: String,
    pub kind: VpnKind,
    pub state: Option<u32>,
}

/// A connection under way (`nmcli connection up`), followed by the panel until it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub uuid: String,
    /// Enter, when it began.
    pub started: Instant,
    /// No word by then: failed (or, still activating, ACTIVATION_BOUND from `started`).
    pub until: Instant,
    /// An activating state was seen: leaving it for anything but activated is a failure.
    pub activating: bool,
    /// The profile asks for a secret at connection time: a failure says so.
    pub asks: bool,
}

/// Where an attempt stands after a state or the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Waiting,
    /// Activated: done.
    Connected,
    /// NetworkManager or the profile went: why, for the log; no `failed` mark.
    Gone(&'static str),
    /// Why, for the log; the profile shows `failed`.
    Failed(&'static str),
}

/// The state of `uuid`'s active connection, if it has one. Several (a down then an up: the old one
/// may still be listed, deactivated, beside the new): the most advanced, activated before
/// activating before deactivating before deactivated, so a stale entry never fails a fresh attempt.
fn active_state(state: &VpnState, uuid: &str) -> Option<u32> {
    let rank = |s: u32| match s {
        ACTIVATED => 0,
        ACTIVATING => 1,
        DEACTIVATING => 2,
        4 => 3,
        _ => 4,
    };
    state.active.iter().filter(|a| a.uuid == uuid).map(|a| a.state).min_by_key(|s| rank(*s))
}

/// 1, 2 or 3: an active connection of any kind (it sorts first in the detail).
fn is_active(state: Option<u32>) -> bool {
    matches!(state, Some(ACTIVATING..=DEACTIVATING))
}

impl Attempt {
    fn failure(&self) -> Outcome {
        Outcome::Failed(if self.asks { NEEDS_SECRET } else { ACTIVATION_FAILED })
    }

    /// NetworkManager's word: no state, or no profile, ends it; activated is done; activating is
    /// noted; deactivating or deactivated (4), or, once it was activating, no active connection at
    /// all, is a failure.
    pub fn observe(&mut self, state: Option<&VpnState>) -> Outcome {
        let Some(state) = state else { return Outcome::Gone(NM_GONE) };
        if !state.profiles.iter().any(|p| p.uuid == self.uuid) {
            return Outcome::Gone(PROFILE_GONE);
        }
        match active_state(state, &self.uuid) {
            Some(ACTIVATED) => Outcome::Connected,
            Some(ACTIVATING) => {
                self.activating = true;
                Outcome::Waiting
            }
            Some(DEACTIVATING | 4) => self.failure(),
            None if self.activating => self.failure(),
            _ => Outcome::Waiting,
        }
    }

    /// The clock: before `until`, nothing; no word by then, failed; still activating, a slow
    /// connection: the deadline moves to ACTIVATION_BOUND from its start, and there it ends.
    pub fn expired(&mut self, state: Option<&VpnState>, now: Instant) -> Outcome {
        if self.until > now {
            return Outcome::Waiting;
        }
        if state.and_then(|s| active_state(s, &self.uuid)) == Some(ACTIVATING) {
            let bound = self.started + ACTIVATION_BOUND;
            if now < bound {
                self.until = bound;
                return Outcome::Waiting;
            }
            return Outcome::Failed(STILL_ACTIVATING);
        }
        Outcome::Failed(NO_ANSWER)
    }
}

/// What the cursor is on in the detail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pick {
    #[default]
    None,
    Profile(String),
    More,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Vpn {
    /// The D-Bus thread's last reading; None: not read, or NetworkManager gone.
    pub state: Option<VpnState>,
    /// The detail is open.
    pub open: bool,
    pub pick: Pick,
    pub focus_pending: bool,
    pub attempt: Option<Attempt>,
    /// The profile whose attempt failed, shown `failed` until that instant.
    pub failed: Option<(String, Instant)>,
}

impl Vpn {
    /// The row shows with at least one profile.
    pub fn shows(&self) -> bool {
        self.state.as_ref().is_some_and(|s| !s.profiles.is_empty())
    }

    /// The row's value: `failed` while an attempt's failure is shown (FAILED_SHOWN: one started by
    /// Space on the row shows there), else the first activated profile's name (16 cells), `…` while
    /// one is under way (activating, or deactivating: ruling 6), else `off`.
    pub fn value(&self) -> String {
        if self.failed.is_some() {
            return FAILED_TAG.into();
        }
        let listed = self.listed();
        if let Some(p) = listed.iter().find(|p| p.state == Some(ACTIVATED)) {
            return truncate(&p.label, VALUE_CELLS);
        }
        if listed.iter().any(|p| matches!(p.state, Some(ACTIVATING | DEACTIVATING))) { "…".into() } else { "off".into() }
    }

    /// Every profile in the detail's order: active ones first, then by name (case ignored), then
    /// uuid.
    fn sorted(&self) -> Vec<Listed> {
        let Some(state) = &self.state else { return Vec::new() };
        let mut l: Vec<Listed> = state
            .profiles
            .iter()
            .map(|p| Listed { uuid: p.uuid.clone(), label: p.name.clone(), kind: p.kind, state: active_state(state, &p.uuid) })
            .collect();
        l.sort_by_cached_key(|p| (!is_active(p.state), p.label.to_lowercase(), p.uuid.clone()));
        l
    }

    /// The profiles the detail lists: `sorted`, cut to PROFILES_MAX.
    pub fn listed(&self) -> Vec<Listed> {
        let mut l = self.sorted();
        l.truncate(PROFILES_MAX);
        l
    }

    /// Space on the row (rulings 2 and 3): the first activating or activated profile goes down,
    /// else the last used one (the highest `last_used` over every profile read, even past the
    /// 16 listed; ties and none: the first in the detail's order) comes up. Nothing while an
    /// attempt runs.
    pub fn toggle(&mut self, now: Instant) -> QuickAction {
        if self.attempt.is_some() {
            return QuickAction::Nothing;
        }
        let all = self.sorted();
        let target = all.iter().find(|p| matches!(p.state, Some(ACTIVATING | ACTIVATED))).or_else(|| {
            let used = |uuid: &str| self.state.as_ref().and_then(|s| s.profiles.iter().find(|p| p.uuid == uuid)).map_or(0, |p| p.last_used);
            // The first maximum: `max_by_key` keeps the last one.
            all.iter()
                .fold(None, |best: Option<&Listed>, p| if best.is_none_or(|b| used(&p.uuid) > used(&b.uuid)) { Some(p) } else { best })
        });
        match target {
            Some(p) => self.act(&p.uuid.clone(), p.state, now),
            None => QuickAction::Nothing,
        }
    }

    /// Enter on a listed profile: down when activating or activated, nothing while deactivating or
    /// during an attempt (one at a time), else the attempt begins and `connection up` runs.
    pub fn enter(&mut self, uuid: &str, now: Instant) -> QuickAction {
        let Some(p) = self.listed().into_iter().find(|p| p.uuid == uuid) else { return QuickAction::Nothing };
        self.act(uuid, p.state, now)
    }

    /// `connection down` or `up` on `uuid` in `state`; one attempt at a time.
    fn act(&mut self, uuid: &str, state: Option<u32>, now: Instant) -> QuickAction {
        if self.attempt.is_some() {
            return QuickAction::Nothing;
        }
        match state {
            Some(ACTIVATING | ACTIVATED) => run(&["nmcli", "connection", "down", "uuid", uuid], "vpn down"),
            Some(DEACTIVATING) => QuickAction::Nothing,
            _ => {
                let asks = self.state.as_ref().and_then(|s| s.profiles.iter().find(|q| q.uuid == uuid)).is_some_and(|q| q.asks);
                self.attempt =
                    Some(Attempt { uuid: uuid.to_string(), started: now, until: now + CONNECT_TIMEOUT, activating: false, asks });
                self.failed = None;
                run(&["nmcli", "connection", "up", "uuid", uuid], "vpn up")
            }
        }
    }

    /// Enter, →, a click on the row: the detail, on the first profile listed.
    pub fn open_detail(&mut self) -> QuickAction {
        self.open = true;
        self.pick = self.listed().into_iter().next().map_or(Pick::None, |p| Pick::Profile(p.uuid));
        QuickAction::Redraw
    }

    /// Back to the rows.
    pub fn close_detail(&mut self) {
        self.open = false;
        self.pick = Pick::None;
    }

    /// What the D-Bus thread is to read for the open panel: the detail's lists while it is open, or
    /// while an attempt runs.
    pub fn scope(&self) -> VpnScope {
        if self.open || self.attempt.is_some() { VpnScope::Detail } else { VpnScope::Row }
    }

    /// The D-Bus thread's state: kept, and the attempt followed. Returns why when the attempt
    /// failed or went (for the log).
    pub fn set_state(&mut self, state: Option<VpnState>, now: Instant) -> Option<&'static str> {
        let before: Vec<String> = if self.open { self.listed().into_iter().map(|p| p.uuid).collect() } else { Vec::new() };
        self.state = state;
        self.pick_neighbour(&before);
        let outcome = self.attempt.as_mut()?.observe(self.state.as_ref());
        self.end(outcome, now)
    }

    /// The picked profile went from the list (`before`, in its order): the pick moves to the one
    /// below it that is still listed, else the one above; none left, nothing picked (`fit` decides).
    fn pick_neighbour(&mut self, before: &[String]) {
        let Pick::Profile(picked) = &self.pick else { return };
        let now: Vec<String> = self.listed().into_iter().map(|p| p.uuid).collect();
        if now.contains(picked) {
            return;
        }
        let Some(at) = before.iter().position(|u| u == picked) else { return };
        let neighbour = before[at + 1..].iter().chain(before[..at].iter().rev()).find(|u| now.contains(u));
        if let Some(u) = neighbour {
            self.pick = Pick::Profile(u.clone());
        }
    }

    /// At `now`: the attempt's deadline and the `failed` mark's end. Returns why the attempt failed
    /// and whether anything changed to redraw.
    pub fn due(&mut self, now: Instant) -> (Option<&'static str>, bool) {
        let why = self.attempt.as_mut().map(|a| a.expired(self.state.as_ref(), now)).and_then(|o| self.end(o, now));
        let redraw = session::expire(&mut self.failed, now);
        (why, redraw || why.is_some())
    }

    /// An attempt's outcome applied: `Failed` marks its profile `failed` for FAILED_SHOWN, `Gone`
    /// marks nothing (and clears an old mark); both end it.
    fn end(&mut self, outcome: Outcome, now: Instant) -> Option<&'static str> {
        match outcome {
            Outcome::Waiting => None,
            Outcome::Connected => {
                self.attempt = None;
                None
            }
            Outcome::Gone(why) => {
                self.attempt = None;
                self.failed = None;
                Some(why)
            }
            Outcome::Failed(why) => {
                if let Some(a) = self.attempt.take() {
                    self.failed = Some((a.uuid, now + FAILED_SHOWN));
                }
                Some(why)
            }
        }
    }

    /// The attempt's deadline or the `failed` mark's end, whichever first.
    pub fn deadline(&self) -> Option<Instant> {
        [self.attempt.as_ref().map(|a| a.until), self.failed.as_ref().map(|(_, at)| *at)].into_iter().flatten().min()
    }

    /// The attempt's nmcli was never started (not found): it ends at once, no `failed` mark (the
    /// log says why).
    pub fn never_started(&mut self) {
        self.attempt = None;
    }
}

/// `[quick] vpn_more`, resolved at startup; unresolved (a test's default) or empty: no `More…`.
pub fn more(config: &QuickConfig) -> &[String] {
    config.vpn_more.as_deref().unwrap_or(&[])
}

/// `More…`: `vpn_more` as given, then the panel closes; nothing when it is empty.
fn run_more(config: &QuickConfig) -> QuickAction {
    run_and_close(more(config), "vpn more")
}

impl Vpn {
    /// The picks in order: the profiles listed, then `More…` when shown.
    fn picks(&self, config: &QuickConfig) -> Vec<Pick> {
        let mut v: Vec<Pick> = self.listed().into_iter().map(|p| Pick::Profile(p.uuid)).collect();
        if !more(config).is_empty() {
            v.push(Pick::More);
        }
        v
    }

    /// Before each use: no profile left (or NetworkManager gone), the detail closes; the selection
    /// back on a pick that exists, the first one when it has no neighbour to move to.
    pub fn fit(&mut self, config: &QuickConfig) {
        if !self.shows() {
            self.close_detail();
            return;
        }
        if !self.open {
            return;
        }
        let picks = self.picks(config);
        if !picks.contains(&self.pick) {
            self.pick = picks.into_iter().next().unwrap_or_default();
        }
    }

    /// The detail's lines: its header, a separator, the profiles, and `More…` after a separator when
    /// configured.
    pub fn lines(&self, config: &QuickConfig) -> Vec<Line> {
        let mut v = vec![Line::VpnHeader, Line::Separator];
        v.extend((0..self.listed().len()).map(Line::VpnProfile));
        if !more(config).is_empty() {
            v.push(Line::Separator);
            v.push(Line::VpnMore);
        }
        v
    }

    pub fn is_selected(&self, line: Line, listed: &[Listed]) -> bool {
        match (line, &self.pick) {
            (Line::VpnProfile(i), Pick::Profile(u)) => listed.get(i).is_some_and(|p| p.uuid == *u),
            (Line::VpnMore, Pick::More) => true,
            _ => false,
        }
    }

    /// Keys in the detail: ↑ ↓ Home End over the profiles and `More…` (never wrapping), Enter acts,
    /// Escape or ← go back to the rows.
    pub fn key(&mut self, config: &QuickConfig, k: Key, now: Instant) -> QuickAction {
        let picks = self.picks(config);
        let at = picks.iter().position(|p| *p == self.pick);
        match k {
            Key::Escape | Key::Left => {
                self.close_detail();
                return QuickAction::Redraw;
            }
            Key::Enter => {
                return match self.pick.clone() {
                    Pick::More => run_more(config),
                    Pick::Profile(uuid) => self.enter(&uuid, now),
                    Pick::None => QuickAction::Nothing,
                };
            }
            _ => {}
        }
        let Some(to) = nav(k, at, picks.len()) else { return QuickAction::Nothing };
        picks.into_iter().nth(to).map_or(QuickAction::Nothing, |p| moved(&mut self.pick, p))
    }

    /// A press on a detail line (the left button only): a profile is chosen and entered; `More…`
    /// runs; the header goes back.
    pub fn press(&mut self, config: &QuickConfig, line: Line, b: Button, now: Instant) -> QuickAction {
        if b != Button::Left {
            return QuickAction::Nothing;
        }
        match line {
            Line::VpnHeader => {
                self.close_detail();
                QuickAction::Redraw
            }
            Line::VpnProfile(i) => {
                let Some(p) = self.listed().into_iter().nth(i) else { return QuickAction::Nothing };
                let before = self.pick.clone();
                self.pick = Pick::Profile(p.uuid.clone());
                let a = self.enter(&p.uuid, now);
                if a == QuickAction::Nothing && self.pick != before { QuickAction::Redraw } else { a }
            }
            Line::VpnMore => {
                self.pick = Pick::More;
                run_more(config)
            }
            _ => QuickAction::Nothing,
        }
    }

    /// The tag after a profile's name: "…" during its attempt or while NetworkManager activates or
    /// deactivates it (muted), "failed" (warn) for FAILED_SHOWN after, else nothing.
    fn tag(&self, p: &Listed) -> (&'static str, Style) {
        if self.attempt.as_ref().is_some_and(|a| a.uuid == p.uuid) || matches!(p.state, Some(ACTIVATING | DEACTIVATING)) {
            (RUNNING_TAG, Style::MUTED)
        } else if self.failed.as_ref().is_some_and(|(u, _)| *u == p.uuid) {
            (FAILED_TAG, Style::WARN)
        } else {
            ("", Style::NORMAL)
        }
    }

    /// One detail line into `g` (`WIDTH + 2` wide, a pad cell each side).
    pub fn draw(&self, g: &mut Grid, line: Line, listed: &[Listed], selected: bool) {
        match line {
            Line::VpnHeader => text_row(g, None, "‹ VPN", "", Style::NORMAL, false),
            Line::VpnMore => text_row(g, Some(' '), "More…", "", Style::NORMAL, selected),
            Line::VpnProfile(i) => {
                if let Some(p) = listed.get(i) {
                    let (tag, style) = self.tag(p);
                    profile_row(g, p, tag, style, selected);
                }
            }
            _ => {}
        }
    }
}

/// "● ⛨ VPN-Test-1  …" (quick VPN spec): ● on an activated profile in the accent, the glyph of its
/// kind and its name cut to fit, then the tag. The tag column is the widest tag's, `failed`, ending
/// one cell before the row's end whatever the name; the name keeps one blank cell before it.
fn profile_row(g: &mut Grid, p: &Listed, tag: &str, tag_style: Style, selected: bool) {
    let end = g.width();
    let room = [RUNNING_TAG, FAILED_TAG].into_iter().map(text_width).max().unwrap_or(0);
    let glyph = if p.kind == VpnKind::WireGuard { WIREGUARD } else { crate::segments::VPN };
    // The mark and its space (3 cells with the pad), the gap, the tag column, the last cell.
    let cells = end.saturating_sub(3 + 1 + room + 1);
    let text = truncate(&format!("{glyph} {}", p.label), cells);
    let mark = if p.state == Some(ACTIVATED) { '●' } else { ' ' };
    text_row(g, Some(mark), &text, "", Style::NORMAL, selected);
    if !tag.is_empty() {
        g.write(end.saturating_sub(room + 1), end, tag, if selected { Style::SELECTED } else { tag_style });
    }
}

#[cfg(test)]
mod tests;
