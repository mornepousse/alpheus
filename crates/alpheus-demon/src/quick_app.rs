//! The quick settings panel's wiring (quick spec § 1, § 5): opened from the volume, network and
//! Bluetooth modules or `alpheus-launch --quick`, filled from the bar's state, the sound thread's
//! sink list (`pipewire::SoundTap`), the backlight and the configured `state`s, and acting through
//! the daemon's executor: every command spawned, never waited for. One `DeadlineTimer`
//! (`quick_timer`) is armed only for a pending re-read or the reboot question. Nothing of it stays
//! once the panel closes (`quick_closed`).

use crate::backlight::{self, Reread};
use crate::config::ModuleId;
use crate::launcher::host::toggle_is_debounced;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, PopupBody, PopupClose};
use crate::quick::{self, Backlight, Focus, Live, Quick, QuickAction, Toggle};
use crate::quick_state;
use crate::quick_wifi::{Attempt, Outcome, Wifi};
use crate::sources::dbus::wifi::WifiTap;
use crate::state::{Bar, Msg};
use crate::system::{Sink, SoundState, WifiScope, WifiState};
use crate::wayland::App;
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

/// Whether a verb arriving at `now` is the key press that opened the panel again (Hyprland
/// re-fires a press bind; `launcher::host::TOGGLE_DEBOUNCE`, as Mod+Space): ignored rather than
/// closing it. Only a verb-opened panel (`opened`) is guarded.
pub(crate) fn verb_debounced(opened: Option<Instant>, now: Instant) -> bool {
    opened.is_some_and(|at| toggle_is_debounced(now.saturating_duration_since(at)))
}

/// An attempt on a not-saved network that outlives the panel (Mae's ruling P1): followed until it
/// ends, so a failed new profile is still deleted after the panel closed. It holds the attempt and
/// the last Wi-Fi state it saw, nothing else (never a password: the field closed when it was sent);
/// None at rest.
#[derive(Debug)]
pub struct WifiFollow {
    attempt: Attempt,
    /// Read again at the timeout: is the new profile still saved?
    state: Option<WifiState>,
}

impl WifiFollow {
    /// The panel closing on `wifi`: its attempt moved out with the state it saw, when it may leave
    /// a new profile behind; None otherwise (a saved network's attempt goes with the panel).
    pub fn leaving(wifi: &mut Wifi) -> Option<WifiFollow> {
        let attempt = wifi.attempt.take_if(|a| a.creates)?;
        Some(WifiFollow { attempt, state: wifi.state.take() })
    }

    /// What it needs read: the detail's lists, to see the attempt's profile appear and go.
    pub fn scope(&self) -> WifiScope {
        WifiScope::Detail
    }

    /// The attempt's timeout: the panel's one timer stays armed for it, and for nothing after.
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
    pub fn due(&self, now: Instant) -> Outcome {
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
        } else {
            self.quick_closed();
        }
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
                self.quick_spawn_secret(&argv, &what);
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

    /// Spawns `argv` through the daemon's executor, never waiting (spec § 5): "quick: <what>"; a
    /// program that cannot start is logged once per open (spec § 6), and its row stays.
    fn quick_spawn(&mut self, argv: &[String], what: &str) {
        let Some((program, args)) = argv.split_first() else { return };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match self.exec.spawn(program, &args) {
            Ok(()) => log(&format!("quick: {what}")),
            Err(e) => {
                let first = match self.popup.as_mut().map(|p| &mut p.body) {
                    Some(PopupBody::Quick(q)) => q.note_failure(program),
                    _ => true,
                };
                if first {
                    log(&format!("quick: {program} could not start ({e:?})"));
                }
            }
        }
    }

    /// Spawns `argv` with the open password field's bytes and a newline on its standard input
    /// (quick Wi-Fi spec § 3), never as an argument; the field closes right after, its buffer wiped,
    /// whatever happened. Logged as "quick: <what>": never the password, never the SSID. An error
    /// may come after nmcli started (it closed its input early): the attempt stays, never retried,
    /// and ends by NetworkManager's word or its timeout.
    fn quick_spawn_secret(&mut self, argv: &[String], what: &str) {
        let App { popup, exec, .. } = self;
        let Some(PopupBody::Quick(q)) = popup.as_mut().map(|p| &mut p.body) else { return };
        let started = match (argv.split_first(), q.wifi.password()) {
            (Some((program, args)), Some(secret)) => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                Some((program.clone(), exec.spawn_with_input(program, &args, &[secret.bytes(), b"\n"])))
            }
            _ => None,
        };
        q.wifi.password_sent();
        match started {
            Some((_, Ok(()))) => log(&format!("quick: {what}")),
            Some((program, Err(e))) if q.note_failure(&program) => {
                log(&format!("quick: {program} could not start or take its input ({e:?})"));
            }
            Some((_, Err(_))) | None => {}
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
    }

    /// The timer follows the panel's earliest pending re-read or question; none when nothing is
    /// pending or the panel is closed.
    pub(crate) fn quick_timer_sync(&mut self) {
        let at = match self.popup.as_ref().map(|p| &p.body) {
            Some(PopupBody::Quick(q)) => q.deadline(),
            _ => None,
        };
        if let Some(handle) = self.loop_handle.clone() {
            self.quick_timer.sync(&handle, at, App::quick_tick);
        }
    }

    /// The timer fired (calloop callback; its source is dropped): the slider levels that waited
    /// for their window sent, the due re-reads, then a refresh.
    pub(crate) fn quick_tick(app: &mut App, now: Instant) {
        app.quick_timer.fired();
        let (due, device) = match app.popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => (q.due(now), q.backlight.as_ref().map(|b| b.device.clone())),
            _ => return,
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
    /// for their window are sent now, never lost.
    pub(crate) fn quick_flush(&mut self) {
        let waiting = match self.popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => q.flush(),
            _ => return,
        };
        for a in waiting {
            if let QuickAction::Run { argv, what } = a {
                self.quick_spawn(&argv, &what);
            }
        }
    }

    /// The panel went, whatever closed it (`popup_close`), or never opened: the sound thread back
    /// to its own scope, the timer gone.
    pub(crate) fn quick_closed(&mut self) {
        self.quick_verb_opened = None;
        if let Some(tap) = &self.sound_tap {
            tap.close();
        }
        if let Some(tap) = &self.wifi_tap {
            wifi_closed(tap, self.wifi_follow.as_ref());
        }
        if let Some(handle) = self.loop_handle.clone() {
            self.quick_timer.sync(&handle, None, App::quick_tick);
        }
    }

    /// A bar message before anyone else reads it: a sound message's sink list goes to the open
    /// panel, or is dropped (the bar never keeps one). True when the open panel shows what it
    /// carries (a sound or profile message): the caller refreshes it after the bar took it.
    pub(crate) fn quick_take(&mut self, m: &mut Msg) -> bool {
        if let Some(sinks) = taken_sinks(m)
            && let Some(PopupBody::Quick(q)) = self.popup.as_mut().map(|p| &mut p.body)
        {
            q.set_sinks(sinks);
        }
        self.quick_shown() && matches!(m, Msg::Sound(_) | Msg::Profile(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BarConfig;
    use crate::quick::{Row, shown};
    use crate::system::Volume;

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
}
