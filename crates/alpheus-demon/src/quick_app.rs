//! The quick settings panel's wiring (quick spec § 1, § 5): opened from the volume, network and
//! Bluetooth modules or `alpheus-launch --quick`, filled from the bar's state, the sound thread's
//! sink list (`pipewire::SoundTap`), the backlight and the configured `state`s, and acting through
//! the daemon's executor: every command spawned, never waited for. One `DeadlineTimer`
//! (`quick_timer`) is armed only for a pending re-read, the reboot question or a Wi-Fi attempt.
//! Nothing of it stays once the panel closes (`quick_closed`), but for one thing (Mae's ruling P1):
//! an attempt on a new network outlives the panel (`WifiFollow`), the Wi-Fi still read and the
//! timer armed for it alone until it ends, so a failed new profile is still deleted.

use crate::backlight::{self, Reread};
use crate::config::ModuleId;
use crate::launcher::host::toggle_is_debounced;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, PopupBody, PopupClose};
use crate::quick::{self, Backlight, Focus, Live, Quick, QuickAction, Toggle, WifiNews};
use crate::quick_state;
use crate::quick_wifi::{Attempt, NM_GONE, Outcome, Wifi, starts_attempt};
use crate::sources::dbus::wifi::WifiTap;
use crate::state::{Bar, Msg};
use crate::system::{Sink, SoundState, WifiScope, WifiState};
use crate::wayland::App;
use alpheus_commun::exec::{ExecError, Executor};
use alpheus_commun::launcher_ipc::{QuickVerb, Reply, Request};
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::path::Path;
use std::time::Instant;

/// What `SoundTap::open` read when the panel opens (controller reconciliation P4): its sink list,
/// returned for the panel, and the rest given to the bar, so a microphone mute the base scope never
/// published (no `osd` module) shows at once. An empty model (pw-dump starting, or down since the
/// stream ended) leaves the bar as it is: no level-less row, never a dead PipeWire's volumes. True
/// when the bar's own drawing changed.
pub(crate) fn opened_sound(bar: &mut Bar, mut s: SoundState) -> (Vec<Sink>, bool) {
    let sinks = s.sinks.take().unwrap_or_default();
    if s.sink.is_none() && s.source.is_none() {
        return (sinks, false);
    }
    let redraw = bar.apply(Msg::Sound(Some(s)));
    (sinks, redraw)
}

/// The sink list a bar message carries for the open panel, taken out of it (the bar never keeps
/// one): a sound message's own; none at all when pw-dump went (`Msg::Sound(None)`), so the outputs
/// detail closes and no `set-default` goes to a node id that is gone or recycled.
pub(crate) fn taken_sinks(m: &mut Msg) -> Option<Vec<Sink>> {
    match m {
        Msg::Sound(Some(s)) => s.sinks.take(),
        Msg::Sound(None) => Some(Vec::new()),
        _ => None,
    }
}

/// The Wi-Fi state a bar message carries for the open panel (quick Wi-Fi spec § 5), taken out of it
/// with the open it was read for: the bar never keeps one. None: not a Wi-Fi message.
pub(crate) fn taken_wifi(m: &mut Msg) -> Option<(Option<u64>, Option<WifiState>)> {
    match m {
        Msg::Wifi { open, state } => Some((*open, state.take())),
        _ => None,
    }
}

/// A Wi-Fi state read for `open` belongs to the tap's current open (`current`, from
/// `WifiTap::snapshot`); one read for an earlier open (a close's last word, a re-read under the
/// scope before) does not (final review B-M1). Untagged (the system bus down), or with no tap: it
/// does.
pub(crate) fn wifi_current(open: Option<u64>, current: Option<u64>) -> bool {
    match (open, current) {
        (Some(open), Some(current)) => open == current,
        _ => true,
    }
}

/// The open panel's part of a Wi-Fi message: `Quick::follow_wifi` on a state of this open, nothing
/// for an earlier open's (`wifi_current`).
pub(crate) fn panel_wifi(
    q: &mut Quick,
    live: &Live,
    open: Option<u64>,
    state: Option<WifiState>,
    current: Option<u64>,
    now: Instant,
) -> Option<WifiNews> {
    wifi_current(open, current).then(|| q.follow_wifi(live, state, now))
}

/// Whether a verb arriving at `now` is the key press that opened the panel again (Hyprland
/// re-fires a press bind; `launcher::host::TOGGLE_DEBOUNCE`, as Mod+Space): ignored rather than
/// closing it. Only a verb-opened panel (`opened`) is guarded.
pub(crate) fn verb_debounced(opened: Option<Instant>, now: Instant) -> bool {
    opened.is_some_and(|at| toggle_is_debounced(now.saturating_duration_since(at)))
}

/// An attempt on a not-saved network that outlives the panel (Mae's ruling P1): followed until it
/// ends, so a failed new profile is still deleted after the panel closed. It holds the attempt and
/// the last Wi-Fi state it saw, nothing else (never a password: the field closed when it was sent);
/// None at rest. The hand-over (`quick_flush`, then `quick_closed`) keeps the D-Bus thread's reader
/// as it is (scope Detail, never Closed in between): a reset would read a profile it skipped before
/// as a new one.
#[derive(Debug)]
pub struct WifiFollow {
    attempt: Attempt,
    /// Read again at the timeout: is the new profile still saved?
    state: Option<WifiState>,
}

impl WifiFollow {
    /// The panel closing on `wifi`: its attempt moved out with the state it saw, when it may leave
    /// a new profile behind and could delete it (the saved list known when it began); None
    /// otherwise (a saved network's attempt goes with the panel; final review B-M2).
    pub fn leaving(wifi: &mut Wifi) -> Option<WifiFollow> {
        let attempt = wifi.attempt.take_if(|a| a.creates && a.known.is_some())?;
        Some(WifiFollow { attempt, state: wifi.state.take() })
    }

    /// What it needs read: the detail's lists, to see the attempt's profile appear and go.
    pub fn scope(&self) -> WifiScope {
        WifiScope::Detail
    }

    /// The attempt's timeout (moved to ACTIVATION_BOUND for a slow activation): the panel's one
    /// timer stays armed for it, and for nothing after.
    pub fn deadline(&self) -> Instant {
        self.attempt.until
    }

    /// The D-Bus thread's state, no panel open: anything but `Waiting` ends the follower (the
    /// cleanup to run, why it failed for the log).
    pub fn set_state(&mut self, state: Option<WifiState>) -> Outcome {
        self.state = state;
        self.attempt.observe(self.state.as_ref())
    }

    /// The timer, at `now`: `Failed` once the attempt timed out (its new profile deleted if still
    /// saved in the last state seen).
    pub fn due(&mut self, now: Instant) -> Outcome {
        self.attempt.expired(self.state.as_ref(), now)
    }

    /// A panel opened meanwhile takes the attempt back (no second attempt; its scope stays the
    /// detail's), and the state seen when it has none yet.
    pub fn resume(self, wifi: &mut Wifi) {
        if wifi.state.is_none() {
            wifi.state = self.state;
        }
        wifi.attempt = Some(self.attempt);
    }
}

/// The Wi-Fi the D-Bus thread follows: the wider of the open panel's (`panel`) and a follower's.
pub(crate) fn wifi_scope(panel: WifiScope, follow: Option<&WifiFollow>) -> WifiScope {
    panel.max(follow.map_or(WifiScope::Closed, WifiFollow::scope))
}

/// The panel is gone: nothing of the Wi-Fi read or followed any more and the D-Bus thread's copy
/// dropped, unless a follower still needs it (then the tap keeps its scope, and no word is sent).
pub(crate) fn wifi_closed(tap: &WifiTap, follow: Option<&WifiFollow>) {
    tap.set(wifi_scope(WifiScope::Closed, follow));
}

/// The follower after a state or the clock (Mae's ruling P1): `Waiting` keeps it; any end drops it,
/// a failure (or NetworkManager gone) logged once, its new profile deleted through `exec` while
/// still saved. The log lines, never an SSID.
pub(crate) fn followed(exec: &dyn Executor, follow: &mut Option<WifiFollow>, outcome: Outcome) -> Vec<String> {
    let mut lines = Vec::new();
    match outcome {
        Outcome::Waiting => return lines,
        Outcome::Connected => {}
        Outcome::Gone => lines.push(format!("quick: wifi connection failed ({NM_GONE})")),
        Outcome::Failed { why, cleanup } => {
            lines.push(format!("quick: wifi connection failed ({why})"));
            if let Some(QuickAction::Run { argv, what }) = cleanup {
                lines.extend(spawn_logged(exec, None, &argv, &what));
            }
        }
    }
    *follow = None;
    lines
}

/// The panel `q` just opened takes back the followed attempt `f` (Mae's ruling P1): no second
/// attempt. Opened on the Wi-Fi (the network module, `--quick wifi`), the detail opens at once from
/// the state the follower carried, since no word of the D-Bus thread is coming (its scope did not
/// change). The action to take: the detail's rescan, else a repaint.
pub(crate) fn resumed(q: &mut Quick, f: WifiFollow) -> QuickAction {
    f.resume(&mut q.wifi);
    q.resumed_wifi_focus().unwrap_or(QuickAction::Redraw)
}

/// Spawns `argv` for the open panel `q` (None: none open), never waiting (spec § 5): the log line,
/// "quick: <what>", or a program that cannot start, once per open (spec § 6; its row stays). A
/// connection whose nmcli never started (NotFound) ends its attempt at once: nothing to follow.
pub(crate) fn spawn_logged(exec: &dyn Executor, q: Option<&mut Quick>, argv: &[String], what: &str) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match exec.spawn(program, &args) {
        Ok(()) => Some(format!("quick: {what}")),
        Err(e) => {
            let first = match q {
                Some(q) => {
                    if e == ExecError::NotFound && starts_attempt(what) {
                        q.wifi.never_started();
                    }
                    q.note_failure(program)
                }
                None => true,
            };
            first.then(|| format!("quick: {program} could not start ({e:?})"))
        }
    }
}

/// Spawns `argv` with the open password field's bytes and a newline on its standard input
/// (quick Wi-Fi spec § 3), never as an argument; the field closes right after, its buffer wiped,
/// whatever happened. The log line: "quick: <what>", never the password, never the SSID. nmcli
/// not found ends the attempt at once; any other error may come after it started (it closed its
/// input early): the attempt stays, never retried, and ends by NetworkManager's word or its timeout.
pub(crate) fn quick_spawn_secret(exec: &dyn Executor, q: &mut Quick, argv: &[String], what: &str) -> Option<String> {
    let started = match (argv.split_first(), q.wifi.password()) {
        (Some((program, args)), Some(secret)) => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            Some((program.clone(), exec.spawn_with_input(program, &args, &[secret.bytes(), b"\n"])))
        }
        _ => None,
    };
    q.wifi.password_sent();
    match started {
        Some((_, Ok(()))) => Some(format!("quick: {what}")),
        Some((program, Err(e))) => {
            if e == ExecError::NotFound {
                q.wifi.never_started();
            }
            q.note_failure(&program).then(|| format!("quick: {program} could not start or take its input ({e:?})"))
        }
        None => None,
    }
}

impl App {
    pub(crate) fn quick_enabled(&self) -> bool {
        self.bar.config.modules.contains(&ModuleId::Quick)
    }

    pub(crate) fn quick_shown(&self) -> bool {
        matches!(self.popup.as_ref().map(|p| &p.body), Some(PopupBody::Quick(_)))
    }

    /// The backlight as the panel shows it: the OSD's device and directory (decision 13), read once.
    fn quick_backlight(&self) -> Option<Backlight> {
        let o = &self.bar.config.osd;
        backlight::named_level(&o.backlight_dir, &o.backlight).map(|(device, percent)| Backlight { device, percent })
    }

    /// A configured toggle's state, read now; None without its toggle.
    fn quick_toggle_state(&self, t: Toggle) -> Option<bool> {
        let c = t.config(&self.bar.config.quick);
        if c.toggle.is_empty() {
            return None;
        }
        quick_state::read(&c.state, Path::new("/proc"), &|k| std::env::var_os(k))
    }

    /// The verb's anchor: the last cell of the bar on the focused output (the positioner slides the
    /// panel left into the output), else of the first configured bar.
    fn quick_anchor(&self) -> Option<PopupAnchor> {
        let (_, focused) = self.bubbles_target();
        let s = focused
            .as_deref()
            .and_then(|n| self.surfaces.iter().find(|s| s.name == n && s.configured))
            .or_else(|| self.surfaces.iter().find(|s| s.configured))?;
        let columns = s.last.as_ref().map_or(1, |g| g.width()).max(1);
        Some(PopupAnchor { output: s.name.clone(), start: columns - 1, end: columns })
    }

    /// Toggles the panel: open, it closes (a second click, or the verb again). `anchor`: the clicked
    /// module's cells, the keyboard taken with a grab; None (the verb): under the right end of the
    /// bar on the focused output, the keyboard taken without one (decision 1).
    pub(crate) fn quick_toggle(&mut self, qh: &QueueHandle<App>, anchor: Option<PopupAnchor>, focus: Focus) {
        if self.quick_shown() {
            self.popup_close(if anchor.is_some() { PopupClose::OutsideClick } else { PopupClose::Verb });
            return;
        }
        if !self.quick_enabled() {
            return;
        }
        let (anchor, input) = match anchor {
            Some(a) => (a, Input::Grab),
            None => match self.quick_anchor() {
                Some(a) => (a, Input::Keys),
                None => {
                    log("quick: no bar to open under");
                    return;
                }
            },
        };
        let Some(i) = self.surfaces.iter().position(|s| s.name == anchor.output) else { return };
        let room = self.popup_room(i).1.saturating_sub(1);
        // From here the sound thread publishes the list (Scope::Panel): every way out below, and
        // every close, goes back through `quick_closed`.
        let opened = self.sound_tap.as_ref().map(|t| t.open());
        let sinks = match opened {
            Some(s) => {
                let (sinks, redraw) = opened_sound(&mut self.bar, s);
                if redraw {
                    self.dirty = true;
                }
                sinks
            }
            None => Vec::new(),
        };
        let backlight = self.quick_backlight();
        let toggles = [self.quick_toggle_state(Toggle::KeepAwake), self.quick_toggle_state(Toggle::NightLight)];
        let built = {
            let live = quick::live(&self.bar, self.dnd);
            let q = Quick::new(focus, &live, sinks, backlight, toggles, room);
            (!q.is_empty(&live)).then(|| {
                let rows = quick::rows(&live, &q);
                let what = quick::describe(&live, &q);
                (q, rows, what)
            })
        };
        let Some((q, rows, what)) = built else {
            self.quick_closed();
            log("quick: nothing to show");
            return;
        };
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Quick(Box::new(q)), columns, height, input) {
            self.quick_verb_opened = (input == Input::Keys).then(Instant::now);
            self.popup_paint(rows);
            log(&format!("quick shown ({what})"));
            self.quick_resume();
            self.quick_wifi_sync();
        } else {
            self.quick_closed();
        }
    }

    /// The panel just opened takes back an attempt still followed (`resumed`): its "…" tag,
    /// its timer, the detail when asked for (its rescan), the D-Bus thread's reading kept.
    fn quick_resume(&mut self) {
        let action = match self.popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => match self.wifi_follow.take() {
                Some(f) => resumed(q, f),
                None => return,
            },
            _ => return,
        };
        self.quick_action(action);
    }

    /// `alpheus-launch --quick [sound]` (spec § 1), answered before the launcher's guard
    /// (launch.rs): toggles the panel on the focused output, the keyboard without a grab
    /// (decision 1); a verb within TOGGLE_DEBOUNCE of the verb's own open is ignored. Done, also
    /// without the module (nothing opens).
    pub(crate) fn quick_verb(&mut self, request: &Request) -> Reply {
        let Request::Quick(verb) = request else { return Reply::Done };
        if !self.quick_enabled() {
            log("quick: asked, but the quick module is not in modules");
            return Reply::Done;
        }
        if self.quick_shown() && verb_debounced(self.quick_verb_opened, Instant::now()) {
            log("quick: toggle debounced");
            return Reply::Done;
        }
        let focus = match verb {
            QuickVerb::Top => Focus::Top,
            QuickVerb::Sound => Focus::Sound,
            QuickVerb::Wifi => Focus::Wifi,
        };
        let qh = self.qh.clone();
        self.quick_toggle(&qh, None, focus);
        Reply::Done
    }

    /// `f` on the open panel and what it reads; Nothing when the panel is not open.
    pub(crate) fn quick_with(&mut self, f: impl FnOnce(&mut Quick, &Live) -> QuickAction) -> QuickAction {
        let App { popup, bar, dnd, .. } = self;
        let live = quick::live(bar, *dnd);
        match popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => f(q, &live),
            _ => QuickAction::Nothing,
        }
    }

    pub(crate) fn quick_action(&mut self, action: QuickAction) {
        match action {
            QuickAction::Nothing => {}
            QuickAction::Redraw => self.quick_refresh(),
            QuickAction::Close => self.popup_close(PopupClose::Escape),
            QuickAction::Run { argv, what } => {
                self.quick_spawn(&argv, &what);
                self.quick_refresh();
            }
            QuickAction::RunAndClose { argv, what } => {
                self.quick_spawn(&argv, &what);
                self.popup_close(PopupClose::Activated);
            }
            QuickAction::RunSecret { argv, what } => {
                self.quick_send_secret(&argv, &what);
                self.quick_refresh();
            }
            // The one path (notify_app.rs): persisted, given to the server; it repaints this panel.
            QuickAction::ToggleDnd => {
                let on = !self.dnd;
                self.set_dnd(on);
                log(&format!("quick: do not disturb {}", if on { "on" } else { "off" }));
            }
        }
    }

    /// Spawns `argv` through the daemon's executor, never waiting (spec § 5): `spawn_logged`.
    fn quick_spawn(&mut self, argv: &[String], what: &str) {
        let App { popup, exec, .. } = self;
        let q = match popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => Some(q.as_mut()),
            _ => None,
        };
        if let Some(line) = spawn_logged(exec.as_ref(), q, argv, what) {
            log(&line);
        }
    }

    /// The open password field handed to nmcli (`quick_spawn_secret`), then the panel repainted.
    fn quick_send_secret(&mut self, argv: &[String], what: &str) {
        let App { popup, exec, .. } = self;
        let Some(PopupBody::Quick(q)) = popup.as_mut().map(|p| &mut p.body) else { return };
        if let Some(line) = quick_spawn_secret(exec.as_ref(), q, argv, what) {
            log(&line);
        }
    }

    /// After anything that may change what the panel shows (a key, a press, a sound or profile
    /// message, a do-not-disturb change, a re-read): fitted, rebuilt and painted (`popup_draw`
    /// commits nothing when the rows did not change), the timer re-synced. Nothing left to show:
    /// the panel closes (Review Focus 2).
    pub(crate) fn quick_refresh(&mut self) {
        if !self.quick_shown() {
            return;
        }
        let rows = {
            let App { popup, bar, dnd, .. } = &mut *self;
            let live = quick::live(bar, *dnd);
            match popup.as_mut().map(|p| &mut p.body) {
                Some(PopupBody::Quick(q)) => {
                    q.fit(&live);
                    (!q.is_empty(&live)).then(|| quick::rows(&live, q))
                }
                _ => None,
            }
        };
        match rows {
            Some(r) => self.popup_paint(r),
            None => self.popup_close(PopupClose::ItemGone),
        }
        self.quick_timer_sync();
        self.quick_wifi_sync();
    }

    /// The timer follows the panel's earliest pending re-read, question or Wi-Fi deadline; with the
    /// panel closed, only a followed attempt's (Mae's ruling P1); none when nothing is pending.
    pub(crate) fn quick_timer_sync(&mut self) {
        let at = match self.popup.as_ref().map(|p| &p.body) {
            Some(PopupBody::Quick(q)) => q.deadline(),
            _ => self.wifi_follow.as_ref().map(WifiFollow::deadline),
        };
        if let Some(handle) = self.loop_handle.clone() {
            self.quick_timer.sync(&handle, at, App::quick_tick);
        }
    }

    /// The D-Bus thread follows what the open panel shows of the Wi-Fi (quick Wi-Fi spec § 5): its row,
    /// or its detail (and while a connection is under way); with no panel, only a followed attempt's
    /// detail (Mae's ruling P1); else nothing. A change wakes the thread once; the same scope again,
    /// nothing.
    pub(crate) fn quick_wifi_sync(&self) {
        let Some(tap) = &self.wifi_tap else { return };
        let panel = match self.popup.as_ref().map(|p| &p.body) {
            Some(PopupBody::Quick(q)) => q.wifi_scope(),
            _ => WifiScope::Closed,
        };
        tap.set(wifi_scope(panel, self.wifi_follow.as_ref()));
    }

    /// The follower's `outcome` (Mae's ruling P1, `followed`), logged; the tap and the timer then
    /// follow it: re-armed on its deadline while it waits (it may move to ACTIVATION_BOUND), both
    /// let go once it ended.
    fn quick_follow(&mut self, outcome: Outcome) {
        let App { exec, wifi_follow, .. } = self;
        for line in followed(exec.as_ref(), wifi_follow, outcome) {
            log(&line);
        }
        self.quick_wifi_sync();
        self.quick_timer_sync();
    }

    /// The timer fired (calloop callback; its source is dropped): the slider levels that waited
    /// for their window sent, the due re-reads, then a refresh.
    pub(crate) fn quick_tick(app: &mut App, now: Instant) {
        app.quick_timer.fired();
        let (due, device) = match app.popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => (q.due(now), q.backlight.as_ref().map(|b| b.device.clone())),
            _ => {
                // No panel: a followed attempt's timeout (Mae's ruling P1).
                if let Some(outcome) = app.wifi_follow.as_mut().map(|f| f.due(now)) {
                    app.quick_follow(outcome);
                }
                return;
            }
        };
        if let Some(why) = due.wifi_failed {
            log(&format!("quick: wifi connection failed ({why})"));
        }
        for a in &due.run {
            if let QuickAction::Run { argv, what } = a {
                app.quick_spawn(argv, what);
            }
        }
        // The device shown, re-read: one failed read keeps the row (final review A-m2).
        let dir = &app.bar.config.osd.backlight_dir;
        let level = due.backlight.then(|| device.as_deref().map_or(Reread::Gone, |d| backlight::reread(dir, d)));
        let states = Toggle::ALL.map(|t| due.toggles[t.index()].then(|| app.quick_toggle_state(t)));
        if let Some(PopupBody::Quick(q)) = app.popup.as_mut().map(|p| &mut p.body) {
            if let Some(level) = level {
                q.set_backlight(level);
            }
            for t in Toggle::ALL {
                if let Some(on) = states[t.index()] {
                    q.set_toggle(t, on);
                }
            }
        }
        app.quick_refresh();
    }

    /// The panel is closing (`popup_close`, before its body goes): the slider levels still waiting
    /// for their window are sent now, never lost; an attempt on a new network handed to the follower
    /// (Mae's ruling P1), the D-Bus thread's reading left as it is (never Closed in between).
    pub(crate) fn quick_flush(&mut self) {
        let (waiting, follow) = match self.popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => (q.flush(), WifiFollow::leaving(&mut q.wifi)),
            _ => return,
        };
        if follow.is_some() {
            self.wifi_follow = follow;
        }
        for a in waiting {
            if let QuickAction::Run { argv, what } = a {
                self.quick_spawn(&argv, &what);
            }
        }
    }

    /// The panel went, whatever closed it (`popup_close`), or never opened: the sound thread back
    /// to its own scope, the timer gone, unless an attempt is still followed (then the Wi-Fi stays
    /// read and the timer stays on that attempt's deadline alone).
    pub(crate) fn quick_closed(&mut self) {
        self.quick_verb_opened = None;
        if let Some(tap) = &self.sound_tap {
            tap.close();
        }
        if let Some(tap) = &self.wifi_tap {
            wifi_closed(tap, self.wifi_follow.as_ref());
        }
        if let Some(handle) = self.loop_handle.clone() {
            let at = self.wifi_follow.as_ref().map(WifiFollow::deadline);
            self.quick_timer.sync(&handle, at, App::quick_tick);
        }
    }

    /// A bar message before anyone else reads it: a sound message's sink list and a Wi-Fi state go to
    /// the open panel, or are dropped (the bar never keeps either); a Wi-Fi state may open the detail
    /// (with its rescan), end an attempt or ask for a cleanup, done here. With no panel, a Wi-Fi state
    /// goes to a followed attempt (Mae's ruling P1). True when the open panel shows what it carries:
    /// the caller refreshes it after the bar took it.
    pub(crate) fn quick_take(&mut self, m: &mut Msg) -> bool {
        if let Some(sinks) = taken_sinks(m)
            && let Some(PopupBody::Quick(q)) = self.popup.as_mut().map(|p| &mut p.body)
        {
            q.set_sinks(sinks);
        }
        if let Some((open, state)) = taken_wifi(m) {
            let now = Instant::now();
            let current = self.wifi_tap.as_ref().map(|t| t.snapshot().1);
            let App { popup, bar, dnd, wifi_follow, .. } = &mut *self;
            let news = match popup.as_mut().map(|p| &mut p.body) {
                Some(PopupBody::Quick(q)) => panel_wifi(q, &quick::live(bar, *dnd), open, state, current, now),
                _ => {
                    // The follower's open is the one the tap still has (it never went Closed).
                    if wifi_current(open, current)
                        && let Some(outcome) = wifi_follow.as_mut().map(|f| f.set_state(state))
                    {
                        self.quick_follow(outcome);
                    }
                    None
                }
            };
            if let Some(news) = news {
                if let Some(why) = news.failed {
                    log(&format!("quick: wifi connection failed ({why})"));
                }
                self.quick_action(news.action);
            }
        }
        self.quick_shown() && matches!(m, Msg::Sound(_) | Msg::Profile(_) | Msg::Wifi { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BarConfig, QuickConfig};
    use crate::keys::Key;
    use crate::quick::{Row, shown};
    use crate::quick_wifi::Pick;
    use crate::system::Volume;
    use alpheus_commun::exec::fake::FakeExecutor;

    /// Mae's ruling P1: closing the panel lets the D-Bus thread go back to reading nothing, unless
    /// an attempt on a new network is still followed: then the detail's reading stays, so a failed
    /// new profile is still seen (and deleted) after the panel went.
    #[test]
    fn closing_the_panel_keeps_the_detail_read_while_an_attempt_is_followed() {
        let follow = WifiFollow { attempt: Attempt::new(b"Cafe-Libre", true, None, Instant::now()), state: None };
        let tap = WifiTap::new().unwrap();
        tap.set(WifiScope::Detail);
        wifi_closed(&tap, Some(&follow));
        assert_eq!(tap.scope(), WifiScope::Detail, "a follower: still read");
        wifi_closed(&tap, None);
        assert_eq!(tap.scope(), WifiScope::Closed, "nothing followed: nothing read");
        assert_eq!(wifi_scope(WifiScope::Row, Some(&follow)), WifiScope::Detail, "the wider of the two");
        assert_eq!(wifi_scope(WifiScope::Detail, None), WifiScope::Detail);
        assert_eq!(wifi_scope(WifiScope::Row, None), WifiScope::Row);
    }

    /// Quick Wi-Fi spec § 5: a Wi-Fi message's state goes to the open panel; the message left for the
    /// bar carries nothing.
    #[test]
    fn a_wifi_message_goes_to_the_panel_and_leaves_nothing_for_the_bar() {
        let state = WifiState { enabled: true, device: None };
        let mut m = Msg::Wifi { open: Some(2), state: Some(state.clone()) };
        assert_eq!(taken_wifi(&mut m), Some((Some(2), Some(state))));
        assert_eq!(m, Msg::Wifi { open: Some(2), state: None }, "emptied");
        assert_eq!(
            taken_wifi(&mut Msg::Wifi { open: Some(2), state: None }),
            Some((Some(2), None)),
            "NetworkManager gone: the panel hears it too"
        );
        assert_eq!(taken_wifi(&mut Msg::Profile(None)), None, "only a Wi-Fi message");
    }

    /// Mae's ruling P1: a follower waits while the attempt does; once it ends it is dropped (the tap
    /// then goes Closed, the timer disarms), a failure logged once and its new profile deleted
    /// through the executor; a success or NetworkManager gone deletes nothing.
    #[test]
    fn a_follower_ends_with_its_attempt_and_deletes_only_a_failed_new_profile() {
        let delete = "nmcli connection delete uuid 22222222-3333-4444-8555-666666666666".to_string();
        let cleanup = || {
            Some(QuickAction::Run {
                argv: quick::argv(&delete.split(' ').collect::<Vec<_>>()),
                what: "wifi delete the failed profile".into(),
            })
        };
        let follower = || Some(WifiFollow { attempt: Attempt::new(b"Cafe-Libre", true, None, Instant::now()), state: None });
        let fake = FakeExecutor::new();
        fake.ok(&delete, "");
        let mut f = follower();
        assert!(followed(&fake, &mut f, Outcome::Waiting).is_empty());
        assert!(f.is_some(), "waiting: still followed");
        let lines = followed(&fake, &mut f, Outcome::Failed { why: "the activation failed", cleanup: cleanup() });
        assert_eq!(lines, ["quick: wifi connection failed (the activation failed)", "quick: wifi delete the failed profile"]);
        assert!(f.is_none(), "ended: dropped");
        assert_eq!(fake.calls(), std::slice::from_ref(&delete), "the delete through the executor");
        let fake = FakeExecutor::new();
        let mut f = follower();
        assert!(followed(&fake, &mut f, Outcome::Connected).is_empty());
        assert!(f.is_none() && fake.calls().is_empty(), "connected: dropped, nothing deleted");
        let mut f = follower();
        assert_eq!(followed(&fake, &mut f, Outcome::Gone), ["quick: wifi connection failed (NetworkManager went away)"]);
        assert!(f.is_none() && fake.calls().is_empty());
        let mut f = follower();
        assert_eq!(
            followed(&fake, &mut f, Outcome::Failed { why: "no answer in 20 s", cleanup: None }),
            ["quick: wifi connection failed (no answer in 20 s)"]
        );
        assert!(f.is_none() && fake.calls().is_empty(), "no profile made: nothing deleted");
    }

    /// Fix round 1 (I1): a panel reopened on the Wi-Fi (the network module, `--quick wifi`) while an
    /// attempt is followed takes it back and opens the detail at once from the state it carried
    /// (no word of the D-Bus thread is coming: its scope never changed), with its one rescan.
    #[test]
    fn a_panel_reopened_on_the_wifi_takes_the_attempt_back_and_opens_the_detail() {
        let t = Instant::now();
        let bar = Bar::new(BarConfig::default());
        let live = quick::live(&bar, false);
        let mut first = panel_on("Cafe-Libre");
        let QuickAction::Run { .. } = first.wifi.key(&QuickConfig::default(), Key::Enter, t) else { panic!("an open network") };
        let follow = WifiFollow::leaving(&mut first.wifi).expect("a new network: followed");
        let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
        let action = resumed(&mut q, follow);
        assert_eq!(action, quick::run(&["nmcli", "device", "wifi", "rescan"], "wifi rescan"), "the detail opened");
        assert!(q.wifi.open && q.selected == Row::Wifi && q.wifi.attempt.is_some(), "taken back, the detail open");
        assert!(!q.wifi.focus_pending, "spent");
        let mut top = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
        let mut again = panel_on("Cafe-Libre");
        again.wifi.key(&QuickConfig::default(), Key::Enter, t);
        let follow = WifiFollow::leaving(&mut again.wifi).expect("followed");
        assert_eq!(resumed(&mut top, follow), QuickAction::Redraw, "opened on the top: no detail");
        assert!(!top.wifi.open && top.wifi.attempt.is_some());
    }

    /// Final review B-M1, both orders: a state read for an earlier open (a close's last word, a
    /// re-read under the scope before) never reaches the panel, so it neither spends the focus nor
    /// blanks the row; the first state of this open with a device opens the detail. The bus going
    /// down (untagged) still reaches it.
    #[test]
    fn only_a_state_read_for_this_open_reaches_the_panel() {
        let t = Instant::now();
        let bar = Bar::new(BarConfig::default());
        let live = quick::live(&bar, false);
        let rescan = quick::run(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
        let current = Some(7);
        let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
        assert_eq!(panel_wifi(&mut q, &live, Some(6), None, current, t), None, "the close's None, queued: dropped");
        assert_eq!(panel_wifi(&mut q, &live, Some(6), Some(wifi_around()), current, t), None, "the open before's: dropped");
        assert!(q.wifi.focus_pending && q.wifi.state.is_none(), "nothing taken");
        let news = panel_wifi(&mut q, &live, Some(7), Some(wifi_around()), current, t).expect("this open's");
        assert_eq!(news.action, rescan, "the detail opened");
        assert!(q.wifi.open && q.selected == Row::Wifi);
        let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
        panel_wifi(&mut q, &live, Some(7), Some(wifi_around()), current, t);
        assert_eq!(panel_wifi(&mut q, &live, Some(6), None, current, t), None, "delivered after the reopen: dropped");
        assert!(q.wifi.open && q.wifi.shows(), "never blanked by it");
        assert!(panel_wifi(&mut q, &live, None, None, current, t).is_some(), "the bus down: the panel hears it");
        assert!(!q.wifi.shows());
        let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
        assert!(panel_wifi(&mut q, &live, Some(3), Some(wifi_around()), None, t).is_some(), "no tap: nothing to tell apart");
    }

    /// Final review B-M2: an attempt whose saved list was unknown when it began can never clean up,
    /// so it is not followed after the panel (no rules, reads or timer for nothing).
    #[test]
    fn an_attempt_that_cannot_clean_up_is_not_followed() {
        let t = Instant::now();
        let mut q = panel_on("Cafe-Libre");
        if let Some(d) = q.wifi.state.as_mut().and_then(|s| s.device.as_mut()) {
            d.saved = None;
        }
        let QuickAction::Run { .. } = q.wifi.key(&QuickConfig::default(), Key::Enter, t) else { panic!("an open network") };
        assert!(q.wifi.attempt.as_ref().is_some_and(|a| a.creates && a.known.is_none()), "{:?}", q.wifi.attempt);
        assert!(WifiFollow::leaving(&mut q.wifi).is_none(), "nothing it could delete: not followed");
        let mut q = panel_on("Cafe-Libre");
        q.wifi.key(&QuickConfig::default(), Key::Enter, t);
        assert!(WifiFollow::leaving(&mut q.wifi).is_some(), "the saved list known: followed");
    }

    fn sink(id: u32, label: &str, default: bool) -> Sink {
        Sink { id, label: label.into(), default }
    }

    fn vol(percent: u8, muted: bool) -> Option<Volume> {
        Some(Volume { percent, muted })
    }

    /// Controller reconciliation P4: without the `osd` module the sound thread publishes the sink's
    /// volume only, so the bar never heard of the microphone; the state read at open (the panel's
    /// scope) shows it, muted, at once — never a stale or missing Mic row.
    #[test]
    fn the_state_read_at_open_shows_a_microphone_the_bar_never_heard_of() {
        let mut config = BarConfig::default();
        config.modules.retain(|m| *m != ModuleId::Osd);
        let mut bar = Bar::new(config);
        bar.apply(Msg::Sound(Some(SoundState { sink: vol(84, false), ..SoundState::default() })));
        let opened = SoundState {
            sink: vol(84, false),
            source: vol(60, true),
            sinks: Some(vec![sink(99, "Speaker", false), sink(122, "AG06", true)]),
            ..SoundState::default()
        };
        let (sinks, redraw) = opened_sound(&mut bar, opened);
        assert_eq!(sinks.len(), 2, "the list goes to the panel");
        assert!(!redraw, "the bar draws the sink alone: unchanged");
        let live = quick::live(&bar, false);
        assert_eq!(live.sound.and_then(|s| s.source), vol(60, true), "the microphone, muted");
        assert_eq!(live.sound.and_then(|s| s.sinks.as_ref()), None, "the bar never keeps a list");
        let q = Quick::new(Focus::Sound, &live, sinks, None, [None, None], 20);
        assert!(shown(&live, &q).contains(&Row::Mic), "{:?}", shown(&live, &q));
    }

    /// Final review A-I1: pw-dump going while the outputs detail is open empties the panel's list,
    /// so the detail closes and Enter never sends a `set-default` to a dead node id.
    #[test]
    fn a_pw_dump_outage_closes_the_outputs_detail() {
        let mut bar = Bar::new(BarConfig::default());
        bar.apply(Msg::Sound(Some(SoundState { sink: vol(64, false), ..SoundState::default() })));
        let sinks = vec![sink(99, "Speaker", false), sink(122, "AG06", true)];
        let mut q = Quick::new(Focus::Sound, &quick::live(&bar, false), sinks, None, [None, None], 20);
        let now = Instant::now();
        assert_eq!(q.key(&quick::live(&bar, false), crate::keys::Key::Enter, false, now), QuickAction::Redraw);
        assert!(q.detail, "the outputs detail is open");
        let mut down = Msg::Sound(None);
        if let Some(s) = taken_sinks(&mut down) {
            q.set_sinks(s);
        }
        bar.apply(down);
        let live = quick::live(&bar, false);
        q.fit(&live); // quick_refresh, after the message
        assert!(!q.detail, "the detail closed");
        let enter = q.key(&live, crate::keys::Key::Enter, false, now);
        assert!(
            !matches!(&enter, QuickAction::Run { argv, .. } if argv.iter().any(|a| a == "set-default")),
            "no set-default after the outage: {enter:?}"
        );
        let mut other = Msg::Profile(None);
        assert_eq!(taken_sinks(&mut other), None, "only a sound message carries a list");
    }

    /// Final review B-I1: a verb within TOGGLE_DEBOUNCE of a verb's open is ignored; past it, or
    /// with the panel opened by a click, it closes the panel.
    #[test]
    fn a_verb_right_after_the_verb_opened_the_panel_is_debounced() {
        use crate::launcher::host::TOGGLE_DEBOUNCE;
        use std::time::Duration;
        let t = Instant::now();
        assert!(verb_debounced(Some(t), t + Duration::from_millis(100)), "the same press, re-fired");
        assert!(!verb_debounced(Some(t), t + TOGGLE_DEBOUNCE), "past the window: a real toggle");
        assert!(!verb_debounced(Some(t), t + Duration::from_millis(300)));
        assert!(!verb_debounced(None, t), "opened by a click: never debounced");
    }

    /// After a pw-dump outage the tap reads an empty model (`sinks: Some([])`, no sink, no source):
    /// the bar keeps what it has (Down), and the panel gets no sound row.
    #[test]
    fn an_empty_model_at_open_leaves_the_bar_alone() {
        let mut bar = Bar::new(BarConfig::default());
        bar.apply(Msg::Sound(None));
        let (sinks, redraw) = opened_sound(&mut bar, SoundState { sinks: Some(vec![]), ..SoundState::default() });
        assert!(sinks.is_empty() && !redraw);
        let live = quick::live(&bar, false);
        assert!(live.sound.is_none(), "still down");
        let q = Quick::new(Focus::Sound, &live, sinks, None, [None, None], 20);
        let rows = shown(&live, &q);
        assert!(!rows.contains(&Row::Volume) && !rows.contains(&Row::Mic), "{rows:?}");
    }

    /// Bureau-Test (WPA2) and Cafe-Libre, Labo-Test (open) around, the device disconnected, no
    /// profile saved (fictional networks).
    fn wifi_around() -> WifiState {
        use crate::system::{AccessPoint, Security, WifiDevice};
        let ap = |s: &str, security| AccessPoint { ssid: s.as_bytes().to_vec(), strength: 60, security };
        WifiState {
            enabled: true,
            device: Some(WifiDevice {
                state: 30,
                active: None,
                active_uuid: None,
                access_points: Some(vec![
                    ap("Bureau-Test", Security::Psk),
                    ap("Cafe-Libre", Security::Open),
                    ap("Labo-Test", Security::Open),
                ]),
                saved: Some(vec![]),
            }),
        }
    }

    /// A panel with the Wi-Fi detail open on `wifi_around`, `ssid` picked.
    fn panel_on(ssid: &str) -> Quick {
        let bar = Bar::new(BarConfig::default());
        let mut q = Quick::new(Focus::Top, &quick::live(&bar, false), vec![], None, [None, None], 20);
        q.wifi = Wifi { state: Some(wifi_around()), ..Wifi::default() };
        q.wifi.open_detail();
        q.wifi.pick = Pick::Network(ssid.as_bytes().to_vec());
        q
    }

    const ASK: &str = "nmcli --ask device wifi connect Bureau-Test";

    /// `q` with the field open on Bureau-Test, `fake-pass` typed, Enter pressed: the RunSecret.
    fn password_typed(q: &mut Quick, t: Instant) -> (Vec<String>, String) {
        let c = QuickConfig::default();
        q.wifi.key(&c, Key::Enter, t);
        for ch in "fake-pass".chars() {
            q.wifi.key(&c, Key::Char(ch), t);
        }
        match q.wifi.key(&c, Key::Enter, t) {
            QuickAction::RunSecret { argv, what } => (argv, what),
            other => panic!("not sent: {other:?}"),
        }
    }

    /// Fix round 1 (minor 4): the password goes to nmcli's standard input with a newline, never in
    /// its arguments; the field closes. An `Io` error may come after nmcli started: the attempt stays.
    #[test]
    fn the_password_is_handed_on_stdin_and_the_field_closes() {
        let t = Instant::now();
        let fake = FakeExecutor::new();
        fake.ok(ASK, "");
        let mut q = panel_on("Bureau-Test");
        let (argv, what) = password_typed(&mut q, t);
        assert_eq!(quick_spawn_secret(&fake, &mut q, &argv, &what), Some("quick: wifi connect (password)".into()));
        assert_eq!(fake.inputs(), [(ASK.to_string(), b"fake-pass\n".to_vec())]);
        assert_eq!(fake.calls(), [ASK]);
        assert!(q.wifi.field.is_none() && q.wifi.password().is_none(), "the field closed, wiped");
        assert!(q.wifi.attempt.is_some(), "followed");
        let fake = FakeExecutor::new();
        fake.respond(ASK, Err(ExecError::Io("Broken pipe".into())));
        let mut q = panel_on("Bureau-Test");
        let (argv, what) = password_typed(&mut q, t);
        let line = quick_spawn_secret(&fake, &mut q, &argv, &what).unwrap_or_default();
        assert!(!line.contains("fake-pass"), "{line}");
        assert!(q.wifi.field.is_none() && q.wifi.attempt.is_some(), "maybe started: the attempt stays, never retried");
    }

    /// Fix round 1 (I1): nmcli not found never started anything: the attempt ends at once (nothing
    /// to follow, no false timeout), and Enter on another network acts.
    #[test]
    fn an_nmcli_never_started_ends_the_attempt_at_once() {
        let (t, c) = (Instant::now(), QuickConfig::default());
        let fake = FakeExecutor::new();
        let mut q = panel_on("Bureau-Test");
        let (argv, what) = password_typed(&mut q, t);
        assert!(quick_spawn_secret(&fake, &mut q, &argv, &what).is_some_and(|l| l.contains("could not start")));
        assert!(q.wifi.attempt.is_none() && q.wifi.deadline().is_none(), "no attempt, no timer");
        assert!(WifiFollow::leaving(&mut q.wifi).is_none(), "nothing to follow");
        q.wifi.pick = Pick::Network(b"Cafe-Libre".to_vec());
        let open = q.wifi.key(&c, Key::Enter, t);
        assert!(matches!(&open, QuickAction::Run { what, .. } if what == "wifi connect (open)"), "{open:?}");
        let QuickAction::Run { argv, what } = open else { return };
        assert_eq!(spawn_logged(&fake, Some(&mut q), &argv, &what), None, "logged once per open");
        assert!(q.wifi.attempt.is_none(), "an open network's nmcli not found: ended too");
        q.wifi.pick = Pick::Network(b"Labo-Test".to_vec());
        assert!(matches!(q.wifi.key(&c, Key::Enter, t), QuickAction::Run { .. }), "and the next one acts");
        let fake = FakeExecutor::new();
        fake.respond("nmcli device wifi connect Labo-Test", Err(ExecError::Io("busy".into())));
        let mut q = panel_on("Labo-Test");
        let QuickAction::Run { argv, what } = q.wifi.key(&c, Key::Enter, t) else { panic!("an open network") };
        spawn_logged(&fake, Some(&mut q), &argv, &what);
        assert!(q.wifi.attempt.is_some(), "Io: maybe started, kept");
    }
}
