//! The quick settings panel's wiring (quick spec § 1, § 5): opened from the volume, network and
//! Bluetooth modules or `alpheus-launch --quick`, filled from the bar's state, the sound thread's
//! sink list (`pipewire::SoundTap`), the backlight and the configured `state`s, and acting through
//! the daemon's executor: every command spawned, never waited for. One `DeadlineTimer`
//! (`QuickApp::timer`) is armed only for a pending re-read, the reboot question or a Wi-Fi attempt.
//! Nothing of it stays once the panel closes (`quick_closed`), but for one thing (Mae's ruling P1):
//! an attempt on a new network outlives the panel (`WifiFollow`), the Wi-Fi still read and the
//! timer armed for it alone until it ends, so a failed new profile is still deleted.

use crate::backlight::{self, Reread};
use crate::config::ModuleId;
use crate::deadline::DeadlineTimer;
use crate::grid::Target;
use crate::keys::Key;
use crate::launcher::host::toggle_is_debounced;
use crate::log::write as log;
use crate::notify::app::unix_now;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer};
use crate::quick::dnd::DndChoice;
use crate::quick::state;
use crate::quick::wifi::{Attempt, NM_GONE, Outcome, Wifi, starts_attempt};
use crate::quick::{self, Backlight, Focus, Live, Quick, QuickAction, Toggle, WifiNews};
use crate::sources::dbus::wifi::WifiTap;
use crate::state::{Bar, Msg};
use crate::system::{BtScope, BtView, Headset, Sink, SoundState, VpnScope, VpnState, WifiScope, WifiState};
use crate::wayland::App;
use alpheus_common::exec::{ExecError, Executor};
use alpheus_common::launcher_ipc::{DndEnd, DndVerb, QuickVerb, Reply, Request};
use alpheus_common::local_time::next_local;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// The quick panel's state beyond the model, one field of `App`: what the glue shares with the
/// source threads, the connection it acts on and the attempt it still follows.
pub(crate) struct QuickApp {
    /// The quick panel's timer: armed only while the panel is open and a re-read
    /// (150 ms, 300 ms), the reboot question (3 s) or a Wi-Fi deadline is pending, or a Bluetooth
    /// call's 30 s deadline (suspended while a code shows), its 4 s `failed` or the 3 s forget
    /// question (quick delivery 3); closed, only for an attempt still followed (`QuickApp::wifi_follow`, Mae's
    /// ruling P1).
    pub(crate) timer: DeadlineTimer,
    /// Shared with the sound thread (`pipewire::SoundTap`): the panel reads the sinks at open and
    /// asks for them while open. None without a sound source.
    pub(crate) sound_tap: Option<Arc<crate::pipewire::SoundTap>>,
    /// Shared with the D-Bus thread (`sources::dbus::wifi::WifiTap`): what the open quick panel
    /// shows of the Wi-Fi. None without the quick module or the network domain.
    pub(crate) wifi_tap: Option<Arc<crate::sources::dbus::wifi::WifiTap>>,
    /// Shared with the D-Bus thread (`sources::dbus::bt::BtTap`): what the open quick panel shows
    /// of Bluetooth. None without the quick module or the Bluetooth domain.
    pub(crate) bt_tap: Option<Arc<crate::sources::dbus::bt::BtTap>>,
    /// Shared with the D-Bus thread (`sources::dbus::vpn::VpnTap`): what the open quick panel shows
    /// of the VPNs. None without the quick module or the network domain.
    pub(crate) vpn_tap: Option<Arc<crate::sources::dbus::vpn::VpnTap>>,
    /// The quick panel's acting Bluetooth connection (bt_app.rs): only while the Bluetooth detail
    /// is open or a call is in flight. None at rest.
    pub(crate) bt_act: Option<crate::sources::dbus::bt_act::BtAct>,
    /// Its fd's source on the loop, while it lives.
    pub(crate) bt_source: Option<smithay_client_toolkit::reexports::calloop::RegistrationToken>,
    /// An attempt on a new network still followed once the panel closed (Mae's ruling P1): the
    /// Wi-Fi stays read for it until it ends. None at rest.
    pub(crate) wifi_follow: Option<WifiFollow>,
    /// When `alpheus-launch --quick` last opened the panel; None once it closed, or when a click
    /// opened it. A verb inside TOGGLE_DEBOUNCE of it is the same key press again.
    pub(crate) verb_opened: Option<Instant>,
}

impl QuickApp {
    /// Nothing shared yet, nothing pending. Written out (not derived) so the guardrail that lists
    /// every timer still counts this one.
    pub(crate) fn new() -> QuickApp {
        QuickApp {
            timer: DeadlineTimer::new(),
            sound_tap: None,
            wifi_tap: None,
            bt_tap: None,
            vpn_tap: None,
            bt_act: None,
            bt_source: None,
            wifi_follow: None,
            verb_opened: None,
        }
    }
}

/// What `SoundTap::open` read when the panel opens: its sink list,
/// returned for the panel, and the rest given to the bar, so a microphone mute the base scope never
/// published (no `osd` module) shows at once. An empty model (pw-dump starting, or down since the
/// stream ended) leaves the bar as it is: no level-less row, never a dead PipeWire's volumes. True
/// when the bar's own drawing changed.
pub(crate) fn opened_sound(bar: &mut Bar, mut s: SoundState) -> (Vec<Sink>, Option<Headset>, bool) {
    let sinks = s.sinks.take().unwrap_or_default();
    let headset = s.headset.take();
    if s.sink.is_none() && s.source.is_none() {
        return (sinks, headset, false);
    }
    let redraw = bar.apply(Msg::Sound(Some(s)));
    (sinks, headset, redraw)
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

/// The headset a bar message carries for the open panel (quick Bluetooth spec § 4), taken out of it
/// like the sink list (the bar never keeps one), and read before `taken_sinks`: a message carries
/// the panel's state only when it has a sink list (`Scope::Panel`); a base-scope one, whose headset
/// is None for "not carried", gives None and leaves the panel's row alone. pw-dump gone
/// (`Msg::Sound(None)`) gives `Some(None)`, so the row goes; None: nothing to set.
pub(crate) fn taken_headset(m: &mut Msg) -> Option<Option<Headset>> {
    match m {
        Msg::Sound(Some(s)) => {
            let h = s.headset.take();
            s.sinks.is_some().then_some(h)
        }
        Msg::Sound(None) => Some(None),
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

/// The VPN state a bar message carries for the open panel (quick VPN spec § 5), taken out of it
/// with the open it was read for: the bar never keeps one. None: not a VPN message.
pub(crate) fn taken_vpn(m: &mut Msg) -> Option<(Option<u64>, Option<VpnState>)> {
    match m {
        Msg::Vpn { open, state } => Some((*open, state.take())),
        _ => None,
    }
}

/// A panel state read for `open` belongs to the tap's current open (`current`, from
/// `Tap::snapshot`); one read for an earlier open (a close's last word, a re-read under the
/// scope before) does not. Untagged (the system bus down), or with no tap: it
/// does.
pub(crate) fn open_current(open: Option<u64>, current: Option<u64>) -> bool {
    match (open, current) {
        (Some(open), Some(current)) => open == current,
        _ => true,
    }
}

/// The open panel's part of a Wi-Fi message: `Quick::follow_wifi` on a state of this open, nothing
/// for an earlier open's (`open_current`).
pub(crate) fn panel_wifi(
    q: &mut Quick,
    live: &Live,
    open: Option<u64>,
    state: Option<WifiState>,
    current: Option<u64>,
    now: Instant,
) -> Option<WifiNews> {
    open_current(open, current).then(|| q.follow_wifi(live, state, now))
}

/// The open panel's part of a VPN message: `Quick::follow_vpn` on a state of this open
/// (`open_current`), nothing for an earlier open's.
pub(crate) fn panel_vpn(
    q: &mut Quick,
    live: &Live,
    open: Option<u64>,
    state: Option<VpnState>,
    current: Option<u64>,
    now: Instant,
) -> Option<WifiNews> {
    open_current(open, current).then(|| q.follow_vpn(live, state, now))
}

/// The Bluetooth view a bar message carries for the open panel (quick Bluetooth spec § 5), taken out
/// of it with the open it was read for: the bar never keeps one. None: not a Bluetooth view message.
pub(crate) fn taken_bt(m: &mut Msg) -> Option<(Option<u64>, Option<BtView>)> {
    match m {
        Msg::Bt { open, state } => Some((*open, state.take())),
        _ => None,
    }
}

/// The open panel's part of a Bluetooth view message: `Quick::follow_bt` on a view of this open
/// (`open_current`, the same tags), nothing for an earlier open's.
pub(crate) fn panel_bt(
    q: &mut Quick,
    live: &Live,
    open: Option<u64>,
    view: Option<BtView>,
    current: Option<u64>,
    now: Instant,
) -> Option<QuickAction> {
    open_current(open, current).then(|| q.follow_bt(live, view, now))
}

/// A view of this open says BlueZ is no longer there to be reached: none (the bus
/// down, the Bluetooth domain out), or one with no adapter (BlueZ gone: the D-Bus thread reads
/// `BtState::Absent` between two owners). The acting connection is then let go.
pub(crate) fn bluez_left(open: Option<u64>, view: Option<&BtView>, current: Option<u64>) -> bool {
    open_current(open, current) && view.is_none_or(|v| v.adapter.is_none())
}

/// What the D-Bus thread reads of Bluetooth: nothing without a panel, else what its row and
/// detail show (`row_shown`: `Bt::shows` on the bar's state).
pub(crate) fn bt_scope_for(panel: Option<&Quick>, row_shown: bool) -> BtScope {
    panel.map_or(BtScope::Closed, |q| q.bt.scope(row_shown))
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
    /// otherwise (a saved network's attempt goes with the panel).
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
                    if e == ExecError::NotFound && what == "vpn up" {
                        q.vpn.never_started();
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

/// The quick panel's row of `popup::POPUPS`.
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Quick,
    toggle: App::quick_toggle_at,
    key: App::quick_key,
    pointer: App::quick_pointer,
    // The slider levels still waiting for their window are sent before the body goes.
    flush: App::quick_flush,
    // Whatever the reason: the sound thread back to its own scope, the panel's timer gone.
    closed: App::quick_closed,
};

impl App {
    pub(crate) fn quick_enabled(&self) -> bool {
        self.bar.config.modules.contains(&ModuleId::Quick)
    }

    pub(crate) fn quick_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Quick(_)))
    }

    /// The backlight as the panel shows it: the OSD's device and directory, read once.
    fn quick_backlight(&self) -> Option<Backlight> {
        let o = &self.bar.config.osd;
        backlight::named_level(&o.backlight_dir, &o.backlight).map(|(device, percent)| Backlight { device, percent })
    }

    /// A configured toggle's state, read now; None without its toggle.
    pub(crate) fn quick_toggle_state(&self, t: Toggle) -> Option<bool> {
        let c = t.config(&self.bar.config.quick);
        if c.toggle.is_empty() {
            return None;
        }
        state::read(&c.state, Path::new("/proc"), &|k| std::env::var_os(k))
    }

    /// The bar a verb opens under: the focused output's, else the first configured one.
    pub(crate) fn verb_bar(&self) -> Option<&crate::wayland::Surface> {
        let (_, focused) = self.bubbles_target();
        focused
            .as_deref()
            .and_then(|n| self.surfaces.iter().find(|s| s.name == n && s.configured))
            .or_else(|| self.surfaces.iter().find(|s| s.configured))
    }

    /// The verb's anchor: the last cell of the bar on the focused output (the positioner slides the
    /// panel left into the output), else of the first configured bar.
    pub(crate) fn quick_anchor(&self) -> Option<PopupAnchor> {
        let s = self.verb_bar()?;
        let columns = s.last.as_ref().map_or(1, |g| g.width()).max(1);
        Some(PopupAnchor { output: s.name.clone(), start: columns - 1, end: columns })
    }

    /// A press on the volume, network or Bluetooth module: the panel under it, on that module's
    /// section.
    fn quick_toggle_at(&mut self, qh: &QueueHandle<App>, anchor: PopupAnchor, target: &Target) {
        self.quick_toggle(qh, Some(anchor), Focus::of(target));
    }

    /// ↑ ↓ ← →, Space, Enter, Escape, Home, End (quick spec § 3); the repeat tells a held key.
    fn quick_key(&mut self, k: Key) {
        let (repeat, now) = (self.keyboard.repeat, Instant::now());
        let action = self.quick_with(|q, live| q.key(live, k, repeat, now));
        self.quick_action(action);
    }

    /// A press acts on its row; the wheel steps a slider or moves the window (quick spec § 3).
    fn quick_pointer(&mut self, cell: Option<(u16, u16)>, kind: PopupPointer) {
        let now = Instant::now();
        let action = match (kind, cell) {
            (PopupPointer::Press(b), Some(c)) => self.quick_with(|q, live| q.press(live, c, b, now)),
            (PopupPointer::Scroll(steps), _) => self.quick_with(|q, live| q.scroll(live, cell, steps, now)),
            _ => QuickAction::Nothing,
        };
        self.quick_action(action);
    }

    /// Toggles the panel: open, it closes (a second click, or the verb again). `anchor`: the clicked
    /// module's cells, the keyboard taken with a grab; None (the verb): under the right end of the
    /// bar on the focused output, the keyboard taken without one.
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
        let opened = self.quick.sound_tap.as_ref().map(|t| t.open());
        let (sinks, headset) = match opened {
            Some(s) => {
                let (sinks, headset, redraw) = opened_sound(&mut self.bar, s);
                if redraw {
                    self.dirty = true;
                }
                (sinks, headset)
            }
            None => (Vec::new(), None),
        };
        self.shield_before_panel();
        let backlight = self.quick_backlight();
        let toggles = [self.quick_toggle_state(Toggle::KeepAwake), self.quick_toggle_state(Toggle::NightLight)];
        let built = {
            let live = quick::live(&self.bar, &self.dnd_timed.model);
            let mut q = Quick::new(focus, &live, sinks, backlight, toggles, room);
            q.set_headset(headset);
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
            self.quick.verb_opened = (input == Input::Keys).then(Instant::now);
            self.popup_paint(rows);
            log(&format!("quick shown ({what})"));
            self.quick_resume();
            self.quick_wifi_sync();
            self.quick_bt_sync();
            self.quick_vpn_sync();
            // A `failed` shown at open expires after its 4 s (the panel's timer follows the view).
            self.quick_timer_sync();
        } else {
            self.quick_closed();
        }
    }

    /// The panel just opened takes back an attempt still followed (`resumed`): its "…" tag,
    /// its timer, the detail when asked for (its rescan), the D-Bus thread's reading kept.
    fn quick_resume(&mut self) {
        let action = match self.popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Quick(q)) => match self.quick.wifi_follow.take() {
                Some(f) => resumed(q, f),
                None => return,
            },
            _ => return,
        };
        self.quick_action(action);
    }

    /// `alpheus-launch --quick [sound]` (spec § 1), answered before the launcher's guard
    /// (`launcher/socket_glue.rs`, `watch_connection`): toggles the panel on the focused output, the keyboard without a grab;
    /// a verb within TOGGLE_DEBOUNCE of the verb's own open is ignored. Done, also
    /// without the module (nothing opens).
    pub(crate) fn quick_verb(&mut self, request: &Request) -> Reply {
        let Request::Quick(verb) = request else { return Reply::Done };
        if !self.quick_enabled() {
            log("quick: asked, but the quick module is not in modules");
            return Reply::Done;
        }
        if self.quick_shown() && verb_debounced(self.quick.verb_opened, Instant::now()) {
            log("quick: toggle debounced");
            return Reply::Done;
        }
        let focus = match verb {
            QuickVerb::Top => Focus::Top,
            QuickVerb::Sound => Focus::Sound,
            QuickVerb::Wifi => Focus::Wifi,
            QuickVerb::Bluetooth => Focus::Bluetooth,
            QuickVerb::Vpn => Focus::Vpn,
        };
        let qh = self.qh.clone();
        self.quick_toggle(&qh, None, focus);
        Reply::Done
    }

    /// `f` on the open panel and what it reads; Nothing when the panel is not open.
    pub(crate) fn quick_with(&mut self, f: impl FnOnce(&mut Quick, &Live) -> QuickAction) -> QuickAction {
        let App { popup, bar, dnd_timed, .. } = self;
        let live = quick::live(bar, &dnd_timed.model);
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
            // The acting connection (bt_app.rs), opened for a call when BlueZ has an adapter.
            QuickAction::Bluetooth(orders) => {
                self.bt_run(orders);
                self.quick_refresh();
            }
            // The one path (notify_app.rs): persisted, given to the server; it repaints this panel.
            QuickAction::ToggleDnd => {
                let on = !self.dnd;
                self.set_dnd(on);
                log(&format!("quick: do not disturb {}", if on { "on" } else { "off" }));
            }
            // Shield mode (shield_app.rs): a switch or an allow. A switch changes the view, and its
            // publish repaints the panel (the row says `…`); an allow or a refusal changes only the
            // panel's own state, repainted here: one paint either way.
            QuickAction::Shield(ask) => {
                let before = self.bar.shield;
                self.shield_ask(ask);
                if self.bar.shield == before {
                    self.quick_refresh();
                }
            }
            // The detail's choice, as the CLI would send it (notify_app.rs).
            QuickAction::Dnd(c) => {
                let until = self.bar.config.quick.dnd_until_hm();
                match dnd_request(c, until, unix_now() as i64) {
                    Some(r) => match self.notifications_verb(&r) {
                        Reply::Error(m) => log(&format!("quick: do not disturb refused ({m})")),
                        _ => log(&format!("quick: do not disturb {}", dnd_word(c, until))),
                    },
                    None => log("quick: do not disturb refused (no such local time)"),
                }
                self.quick_refresh();
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
    /// the panel closes.
    pub(crate) fn quick_refresh(&mut self) {
        if !self.quick_shown() {
            return;
        }
        let rows = {
            let App { popup, bar, dnd_timed, .. } = &mut *self;
            let live = quick::live(bar, &dnd_timed.model);
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
        self.quick_bt_sync();
        self.quick_vpn_sync();
        self.shield_panel_sync();
    }

    /// The timer follows the panel's earliest pending re-read, question or Wi-Fi deadline; with the
    /// panel closed, only a followed attempt's (Mae's ruling P1); none when nothing is pending.
    pub(crate) fn quick_timer_sync(&mut self) {
        let at = match self.popup_body() {
            Some(PopupBody::Quick(q)) => q.deadline(),
            _ => self.quick.wifi_follow.as_ref().map(WifiFollow::deadline),
        };
        self.quick.timer.sync_opt(self.loop_handle.as_ref(), at, App::quick_tick);
    }

    /// The D-Bus thread follows what the open panel shows of the Wi-Fi (quick Wi-Fi spec § 5): its row,
    /// or its detail (and while a connection is under way); with no panel, only a followed attempt's
    /// detail (Mae's ruling P1); else nothing. A change wakes the thread once; the same scope again,
    /// nothing.
    pub(crate) fn quick_wifi_sync(&self) {
        let Some(tap) = &self.quick.wifi_tap else { return };
        let panel = match self.popup_body() {
            Some(PopupBody::Quick(q)) => q.wifi_scope(),
            _ => WifiScope::Closed,
        };
        tap.set(wifi_scope(panel, self.quick.wifi_follow.as_ref()));
    }

    /// The D-Bus thread follows what the open panel shows of the VPNs (quick VPN spec § 5): the row's
    /// state, or the detail's lists (and while a connection is under way); closed, nothing. A change
    /// wakes the thread once; the same scope again, nothing.
    pub(crate) fn quick_vpn_sync(&self) {
        let Some(tap) = &self.quick.vpn_tap else { return };
        tap.set(match self.popup_body() {
            Some(PopupBody::Quick(q)) => q.vpn_scope(),
            _ => VpnScope::Closed,
        });
    }

    /// The follower's `outcome` (Mae's ruling P1, `followed`), logged; the tap and the timer then
    /// follow it: re-armed on its deadline while it waits (it may move to ACTIVATION_BOUND), both
    /// let go once it ended.
    fn quick_follow(&mut self, outcome: Outcome) {
        let App { exec, quick, .. } = self;
        for line in followed(exec.as_ref(), &mut quick.wifi_follow, outcome) {
            log(&line);
        }
        self.quick_wifi_sync();
        self.quick_timer_sync();
    }

    /// The timer fired (calloop callback; its source is dropped): the slider levels that waited
    /// for their window sent, the due re-reads, then a refresh.
    pub(crate) fn quick_tick(app: &mut App, now: Instant) {
        app.quick.timer.fired();
        let (mut due, device) = match app.popup_body_mut() {
            Some(PopupBody::Quick(q)) => (q.due(now), q.backlight.as_ref().map(|b| b.device.clone())),
            _ => {
                // No panel: a followed attempt's timeout (Mae's ruling P1).
                if let Some(outcome) = app.quick.wifi_follow.as_mut().map(|f| f.due(now)) {
                    app.quick_follow(outcome);
                }
                return;
            }
        };
        if let Some(why) = due.wifi_failed {
            log(&format!("quick: wifi connection failed ({why})"));
        }
        if let Some(why) = due.vpn_failed {
            log(&format!("quick: vpn connection failed ({why})"));
        }
        // A Bluetooth call's deadline: its pairing cancelled, its answer forgotten, `failed`.
        for line in &due.bt_logs {
            log(line);
        }
        app.bt_run(std::mem::take(&mut due.bt_orders));
        for a in &due.run {
            if let QuickAction::Run { argv, what } = a {
                app.quick_spawn(argv, what);
            }
        }
        // The device shown, re-read: one failed read keeps the row.
        let dir = &app.bar.config.osd.backlight_dir;
        let level = due.backlight.then(|| device.as_deref().map_or(Reread::Gone, |d| backlight::reread(dir, d)));
        let states = Toggle::ALL.map(|t| due.toggles[t.index()].then(|| app.quick_toggle_state(t)));
        if due.shield_failed {
            app.shield_failed_due(now);
        }
        if let Some(PopupBody::Quick(q)) = app.popup_body_mut() {
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
        let (leaving, waiting, follow) = match self.popup_body_mut() {
            Some(PopupBody::Quick(q)) => (q.bt_leaving(), q.flush(), WifiFollow::leaving(&mut q.wifi)),
            _ => return,
        };
        if follow.is_some() {
            self.quick.wifi_follow = follow;
        }
        // A code question rejected, the pairing cancelled, discovery stopped while
        // the connection still exists (`quick_closed` then unregisters the agent and closes it).
        self.bt_run(leaving);
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
        self.quick.verb_opened = None;
        if let Some(tap) = &self.quick.sound_tap {
            tap.close();
        }
        if let Some(tap) = &self.quick.wifi_tap {
            wifi_closed(tap, self.quick.wifi_follow.as_ref());
        }
        // Nothing of Bluetooth outlives the panel: the D-Bus thread reads nothing, the agent goes.
        if let Some(tap) = &self.quick.bt_tap {
            tap.set(BtScope::Closed);
        }
        self.bt_close();
        // Nothing of the blocked devices outlives the panel.
        self.shield_panel_closed();
        // Nothing of the VPNs outlives the panel either (no follower: its attempt goes with it).
        if let Some(tap) = &self.quick.vpn_tap {
            tap.set(VpnScope::Closed);
        }
        let at = self.quick.wifi_follow.as_ref().map(WifiFollow::deadline);
        self.quick.timer.sync_opt(self.loop_handle.as_ref(), at, App::quick_tick);
    }

    /// A bar message before anyone else reads it: a sound message's sink list and a Wi-Fi state go to
    /// the open panel, or are dropped (the bar never keeps either); a Wi-Fi state may open the detail
    /// (with its rescan), end an attempt or ask for a cleanup, done here. With no panel, a Wi-Fi state
    /// goes to a followed attempt (Mae's ruling P1). True when the open panel shows what it carries:
    /// the caller refreshes it after the bar took it.
    pub(crate) fn quick_take(&mut self, m: &mut Msg) -> bool {
        // Both taken always, so the bar never keeps either; the headset first: it
        // reads `sinks` as the panel-scope marker.
        let (headset, sinks) = (taken_headset(m), taken_sinks(m));
        if let Some(PopupBody::Quick(q)) = self.popup_body_mut() {
            if let Some(sinks) = sinks {
                q.set_sinks(sinks);
            }
            if let Some(h) = headset {
                q.set_headset(h);
            }
        }
        if let Some((open, state)) = taken_wifi(m) {
            let now = Instant::now();
            let current = self.quick.wifi_tap.as_ref().map(|t| t.snapshot().1);
            let App { popup, bar, dnd_timed, quick, .. } = &mut *self;
            let news = match popup.as_mut().map(|p| &mut p.body) {
                Some(PopupBody::Quick(q)) => panel_wifi(q, &quick::live(bar, &dnd_timed.model), open, state, current, now),
                _ => {
                    // The follower's open is the one the tap still has (it never went Closed).
                    if open_current(open, current)
                        && let Some(outcome) = quick.wifi_follow.as_mut().map(|f| f.set_state(state))
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
        if let Some((open, view)) = taken_bt(m) {
            let now = Instant::now();
            let current = self.quick.bt_tap.as_ref().map(|t| t.snapshot().1);
            // BlueZ gone or replaced: the connection let go first, so nothing the panel orders next
            // reaches the old owner; the next need opens one on the new owner.
            if bluez_left(open, view.as_ref(), current) {
                self.bt_drop();
            }
            let App { popup, bar, dnd_timed, .. } = &mut *self;
            let action = match popup.as_mut().map(|p| &mut p.body) {
                Some(PopupBody::Quick(q)) => panel_bt(q, &quick::live(bar, &dnd_timed.model), open, view, current, now),
                _ => None,
            };
            if let Some(action) = action {
                self.quick_action(action);
            }
        }
        // The VPN state goes to the open panel only, dropped with none (never kept by the bar).
        if let Some((open, state)) = taken_vpn(m) {
            let now = Instant::now();
            let current = self.quick.vpn_tap.as_ref().map(|t| t.snapshot().1);
            let App { popup, bar, dnd_timed, .. } = &mut *self;
            let news = match popup.as_mut().map(|p| &mut p.body) {
                Some(PopupBody::Quick(q)) => panel_vpn(q, &quick::live(bar, &dnd_timed.model), open, state, current, now),
                _ => None,
            };
            if let Some(news) = news {
                if let Some(why) = news.failed {
                    log(&format!("quick: vpn connection failed ({why})"));
                }
                self.quick_action(news.action);
            }
        }
        self.quick_bt_sync();
        self.quick_vpn_sync();
        self.quick_shown()
            && matches!(m, Msg::Sound(_) | Msg::Profile(_) | Msg::Wifi { .. } | Msg::Bluetooth(_) | Msg::Bt { .. } | Msg::Vpn { .. })
    }
}

/// The request the CLI sends for `c` (`alpheus-launch --dnd …`, `launcher_ipc::Request`). None: the
/// `Until` time cannot be placed.
pub(crate) fn dnd_request(c: DndChoice, until: (u32, u32), now: i64) -> Option<Request> {
    Some(match c {
        DndChoice::Off => Request::Dnd(DndVerb::Off),
        DndChoice::For(s) => Request::DndOn(DndEnd::For(s)),
        DndChoice::Until => {
            let at = next_local(until.0, until.1, now)?;
            Request::DndOn(DndEnd::Until(u64::try_from(at).ok()?))
        }
        DndChoice::Untimed => Request::Dnd(DndVerb::On),
    })
}

/// The log's word for `c`: `off`, `on`, `for N min`, `until HH:MM`.
fn dnd_word(c: DndChoice, until: (u32, u32)) -> String {
    match c {
        DndChoice::Off => "off".into(),
        DndChoice::For(s) => format!("for {} min", s / 60),
        DndChoice::Until => format!("until {:02}:{:02}", until.0, until.1),
        DndChoice::Untimed => "on".into(),
    }
}

#[cfg(test)]
mod tests;
