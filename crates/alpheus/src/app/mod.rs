//! alpheus's state and logic, with no terminal or thread: every input (key,
//! mouse, worker response, tick) changes the state; requests to the worker
//! and the external tool to open are left in boxes that the real loop
//! (run.rs) drains. Everything is tested by calling methods.

pub mod action;
pub mod appearance;
pub mod diagnostics;
pub mod displays;
mod input;
pub mod journal;
pub mod keymap;
mod mouse;
pub mod report_views;
pub mod run;
mod sound;
pub mod tab;
pub mod worker;

use crate::backends::{
    AudioNode, AudioStatus, BResult, BackendError, BtStatus, NetStatus, NodeRef, PowerAction, Profiles, UnitAction, Volume, brightness,
};
use crate::config::Config;
use crate::diagnostics::query::Scope;
use crate::displays::Output;
use crate::fuzzy;
use crate::theme::Theme;
use action::{Command, External, Tab, palette_entries, profile_label};
pub use appearance::AppearanceTab;
pub use diagnostics::{DiagItem, DiagnosticsTab};
pub use displays::{DisplaysTab, KEEP_DELAY, Keep, Panel, PanelField, PlaceChoice, SaveDialog, Trial};
use keymap::Keymap;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use worker::{Domain, Op, Request, Update};

/// Slider step (-/+ keys, scroll wheel).
pub const STEP: i32 = 5;
/// Lifetime of an ephemeral message.
pub const TOAST_TTL: Duration = Duration::from_secs(3);
/// A failed domain is re-read at most once per this period.
pub const FAILED_RETRY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Loadable<T> {
    #[default]
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Loadable::Ready(v) => Some(v),
            _ => None,
        }
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Loadable::Failed(_))
    }

    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Loadable<U> {
        match self {
            Loadable::Loading => Loadable::Loading,
            Loadable::Ready(v) => Loadable::Ready(f(v)),
            Loadable::Failed(e) => Loadable::Failed(e.clone()),
        }
    }
}

impl<T> From<BResult<T>> for Loadable<T> {
    fn from(r: BResult<T>) -> Self {
        match r {
            Ok(v) => Loadable::Ready(v),
            Err(e) => Loadable::Failed(e.to_string()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Data {
    pub volume: Loadable<Volume>,
    pub brightness: Loadable<u8>,
    pub network: Loadable<NetStatus>,
    pub bluetooth: Loadable<BtStatus>,
    pub dnd: Loadable<bool>,
    pub keep_awake: Loadable<bool>,
    pub profiles: Loadable<Profiles>,
    pub audio: Loadable<AudioStatus>,
    pub displays: Loadable<Vec<Output>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SliderId {
    Volume,
    Brightness,
    Node(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToggleId {
    Wifi,
    Bluetooth,
    Dnd,
    KeepAwake,
}

/// Focusable element of the current tab (order = display order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Slider(SliderId),
    Toggle(ToggleId),
    Profile,
    Button(Command),
    /// Display profile bar (←/→ chooses, Enter applies).
    DisplayProfiles,
    /// A display output, by connector.
    Output(String),
    /// A row of the Diag tab's overview.
    Diag(DiagItem),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirm {
    pub action: PowerAction,
    /// Focus starts on "Cancel": Enter by reflex powers nothing off.
    pub yes_focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Palette(Palette),
    Confirm(Confirm),
    /// "Keep these settings?" (display-trial countdown).
    Keep(Keep),
    /// Settings panel for an output.
    Panel(Panel),
    /// "Save current": display profile name.
    SaveProfile(SaveDialog),
    /// "Restart this unit?": a failed or looping unit's action. Focus starts on "Cancel".
    UnitConfirm {
        scope: Scope,
        unit: String,
        action: UnitAction,
        yes_focused: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Search {
    pub text: String,
    pub editing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub last: Option<String>,
    /// Last action error; cleared on the next success.
    pub error: Option<String>,
    /// Startup warning (config, theme, keys): persistent.
    pub warning: Option<String>,
}

pub fn toggle_label(id: ToggleId) -> &'static str {
    match id {
        ToggleId::Wifi => "Wi-Fi",
        ToggleId::Bluetooth => "Bluetooth",
        ToggleId::Dnd => "Do not disturb",
        ToggleId::KeepAwake => "Keep awake",
    }
}

fn op_label(op: &Op) -> String {
    match op {
        Op::SetVolume(..) => "Volume".into(),
        Op::SetDefault(_) => "Default device".into(),
        Op::SetBrightness(_) => "Brightness".into(),
        Op::SetWifi(_) => toggle_label(ToggleId::Wifi).into(),
        Op::SetBluetooth(_) => toggle_label(ToggleId::Bluetooth).into(),
        Op::SetDnd(_) => toggle_label(ToggleId::Dnd).into(),
        Op::ToggleKeepAwake { .. } => toggle_label(ToggleId::KeepAwake).into(),
        Op::SetProfile(_) => "Energy profile".into(),
        Op::Power(a) => a.label().into(),
        Op::TryDisplays(_) | Op::KeepDisplays | Op::RevertDisplays => "Displays".into(),
        Op::UnitUser(UnitAction::Restart, _) => "Restart".into(),
        Op::UnitUser(UnitAction::ResetFailed, _) => "Reset failed".into(),
        Op::FailedRefresh => "Failed units".into(),
        Op::Copy { .. } => "Copy".into(),
    }
}

/// Domain re-read by an update (none for an action's response).
fn update_domain(u: &Update) -> Option<Domain> {
    Some(match u {
        Update::Volume(_) => Domain::Volume,
        Update::Brightness(_) => Domain::Brightness,
        Update::Network(_) => Domain::Network,
        Update::Bluetooth(_) => Domain::Bluetooth,
        Update::Dnd(_) => Domain::Dnd,
        Update::KeepAwake(_) => Domain::KeepAwake,
        Update::Profiles(_) => Domain::Profiles,
        Update::Audio(_) => Domain::Audio,
        Update::Displays(_) => Domain::Displays,
        Update::Failed(..) => Domain::Failed,
        Update::Health(_) => Domain::Health,
        Update::Done(..) | Update::Superseded(_) | Update::Diag(..) | Update::Poke(_) => return None,
    })
}

pub struct App {
    pub tab: Tab,
    pub data: Data,
    pub focus: usize,
    pub overlay: Overlay,
    pub search: Search,
    pub status: Status,
    pub toasts: Vec<Toast>,
    pub keymap: Keymap,
    pub config: Config,
    pub theme: Theme,
    pub should_quit: bool,
    pub displays: DisplaysTab,
    pub appearance: AppearanceTab,
    pub diag: DiagnosticsTab,
    /// Slider currently being dragged with the mouse, and its bar.
    pub(crate) dragging: Option<(SliderId, Rect)>,
    last_refresh: Instant,
    requests: Vec<Request>,
    external: Option<External>,
    /// A system unit's action waiting to run in the TUI's own window (see `take_system_unit`).
    system_unit: Option<(UnitAction, String)>,
    /// Actions sent and still unanswered, by domain touched. As long as a
    /// domain has one, its reads are ignored: if launched before the
    /// action, they would bring back the old state over the optimistic
    /// display (a flicker). The thread answers each `Request::Do` with
    /// exactly one `Done` or `Superseded` (a setting coalesced into a
    /// burst), so the count always returns to zero.
    pending: HashMap<Domain, u32>,
    /// Last read request, by domain (slow re-read of failures).
    last_read: HashMap<Domain, Instant>,
}

impl App {
    pub fn new(config: Config, theme: Theme, keymap: Keymap, now: Instant) -> App {
        App::with_tab(config, theme, keymap, Tab::Home, now)
    }

    /// Opens directly on `tab`: only its domains are read.
    pub fn with_tab(config: Config, theme: Theme, keymap: Keymap, tab: Tab, now: Instant) -> App {
        let mut app = App {
            tab,
            data: Data::default(),
            focus: 0,
            overlay: Overlay::None,
            search: Search::default(),
            status: Status::default(),
            toasts: Vec::new(),
            keymap,
            config,
            theme,
            should_quit: false,
            displays: DisplaysTab::default(),
            appearance: AppearanceTab::default(),
            diag: DiagnosticsTab::default(),
            dragging: None,
            last_refresh: now,
            requests: Vec::new(),
            external: None,
            system_unit: None,
            pending: HashMap::new(),
            last_read: HashMap::new(),
        };
        app.refresh_tab(now);
        if tab == Tab::Diagnostics {
            app.diagnostics_reads();
        }
        app
    }

    /// Requests to forward to the worker (empties the box).
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// External tool to open (empties the box).
    pub fn take_external(&mut self) -> Option<External> {
        self.external.take()
    }

    /// Startup warning (config, theme, keys): status line.
    pub fn warn(&mut self, msg: String) {
        self.status.warning = Some(match self.status.warning.take() {
            Some(prev) => format!("{prev} ; {msg}"), // several faulty files
            None => msg,
        });
    }

    pub fn external_argv(&self, e: External) -> Vec<String> {
        let c = &self.config.commands;
        match e {
            External::Mixer => c.mixer.clone(),
            External::Network => c.network.clone(),
            External::Bluetooth => c.bluetooth.clone(),
        }
    }

    pub fn external_done(&mut self, e: External, r: Result<(), String>, now: Instant) {
        if let Err(msg) = r {
            let msg = format!("{}: {msg}", e.label());
            self.status.error = Some(msg.clone());
            self.toast(msg, true, now);
        }
        self.refresh_tab(now); // the tool may have changed anything
    }

    // ── Worker responses ─────────────────────────────────────────────

    /// Sends an action to the thread and records the domains it touches.
    fn send_op(&mut self, op: Op) {
        for d in op.domains() {
            *self.pending.entry(*d).or_default() += 1;
        }
        self.requests.push(Request::Do(op));
    }

    /// The action got its response (done or dropped by coalescing).
    fn op_answered(&mut self, op: &Op) {
        for d in op.domains() {
            if let Some(n) = self.pending.get_mut(d) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    self.pending.remove(d);
                }
            }
        }
    }

    fn is_pending(&self, d: Domain) -> bool {
        self.pending.contains_key(&d)
    }

    pub fn on_update(&mut self, u: Update, now: Instant) {
        if let Some(d) = update_domain(&u) {
            if self.is_pending(d) {
                // Stale read: an action on this domain is in progress. A displays trial's end
                // is reported once, so it is still told; only the outputs are dropped.
                if let Update::Displays(Ok(s)) = u {
                    self.trial_read(s.trial, now);
                }
                return;
            }
        }
        // On the Diag overview a read can add or drop rows above the cursor: a selected unit stays
        // selected (`r` and `c` act on it), never whatever slid into its place. The other rows keep
        // their place: the first read must not carry the cursor off the top.
        let keep = match self.focused() {
            Some(item @ Item::Diag(DiagItem::Failed(..) | DiagItem::Loop(..))) => Some(item),
            _ => None,
        };
        match u {
            Update::Volume(r) => self.data.volume = r.into(),
            Update::Brightness(r) => self.data.brightness = r.into(),
            Update::Network(r) => self.data.network = r.into(),
            Update::Bluetooth(r) => self.data.bluetooth = r.into(),
            Update::Dnd(r) => self.data.dnd = r.into(),
            Update::KeepAwake(r) => self.data.keep_awake = r.into(),
            Update::Profiles(r) => self.data.profiles = r.into(),
            Update::Audio(r) => self.data.audio = r.into(),
            Update::Displays(r) => self.displays_read(r, now),
            Update::Failed(Scope::System, r) => self.diag.failed_system.set(r),
            Update::Failed(Scope::User, r) => self.diag.failed_user.set(r),
            Update::Health(h) => self.diag.health = Some(h),
            Update::Diag(read, result) => self.diag_update(read, result, now),
            Update::Poke(answer) => self.diag.daemon = Some(answer),
            Update::Done(op @ (Op::TryDisplays(_) | Op::KeepDisplays | Op::RevertDisplays), r) => {
                self.op_answered(&op);
                self.displays_done(op, r, now);
            }
            Update::Done(op, r) => {
                self.op_answered(&op);
                self.op_done(op, r, now);
            }
            Update::Superseded(op) => self.op_answered(&op),
        }
        if let Some(i) = keep.and_then(|k| self.items().iter().position(|i| *i == k)) {
            self.focus = i;
        }
        self.clamp_focus();
    }

    fn op_done(&mut self, op: Op, r: BResult<()>, now: Instant) {
        match r {
            Ok(()) => {
                self.status.error = None;
                if let Some(msg) = self.success_message(&op) {
                    self.status.last = Some(msg.clone());
                    self.toast(msg, false, now);
                }
                // Sliders: the displayed value is already correct; reading
                // again mid-drag would make the slider jump.
                if !matches!(op, Op::SetVolume(..) | Op::SetBrightness(_)) {
                    self.refresh_tab(now);
                }
            }
            Err(e) => {
                // A queued restart that outlived the 2 s limit may well have worked.
                let msg = match (&op, &e) {
                    (Op::UnitUser(..), BackendError::Timeout { .. }) => {
                        "systemctl is not responding; the list will show the result".to_string()
                    }
                    _ => format!("{}: {e}", op_label(&op)),
                };
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
                self.refresh_tab(now); // cancels the optimistic display
            }
        }
        if matches!(op, Op::UnitUser(..)) {
            self.requests.push(Request::Do(Op::FailedRefresh)); // the bar's count follows at once
        }
    }

    fn success_message(&self, op: &Op) -> Option<String> {
        let onoff = |id: ToggleId, b: bool| format!("{}: {}", toggle_label(id), if b { "on" } else { "off" });
        Some(match op {
            Op::SetVolume(..) | Op::SetBrightness(_) => return None,
            Op::SetDefault(id) => {
                let a = self.data.audio.ready()?;
                match a.sinks.iter().find(|n| n.id == *id) {
                    Some(n) => format!("Output: {}", n.name),
                    None => format!("Input: {}", a.sources.iter().find(|n| n.id == *id)?.name),
                }
            }
            Op::SetWifi(b) => onoff(ToggleId::Wifi, *b),
            Op::SetBluetooth(b) => onoff(ToggleId::Bluetooth, *b),
            Op::SetDnd(b) => onoff(ToggleId::Dnd, *b),
            Op::ToggleKeepAwake { now_on } => onoff(ToggleId::KeepAwake, *now_on),
            Op::SetProfile(p) => format!("Profile: {}", profile_label(p)),
            Op::Power(a) => a.label().to_string(),
            Op::TryDisplays(_) | Op::KeepDisplays | Op::RevertDisplays => return None,
            Op::UnitUser(UnitAction::Restart, unit) => format!("Restart queued: {unit}"),
            Op::UnitUser(UnitAction::ResetFailed, unit) => format!("Cleared: {unit}"),
            Op::FailedRefresh => return None,
            Op::Copy { .. } => "Copied to the clipboard".to_string(),
        })
    }

    fn toast(&mut self, text: String, error: bool, now: Instant) {
        self.toasts.push(Toast { text, error, until: now + TOAST_TTL });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    // ── Time ─────────────────────────────────────────────────────────

    pub fn on_tick(&mut self, now: Instant) {
        self.toasts.retain(|t| t.until > now);
        self.displays_tick(now);
        self.journal_tick(now);
        if self.diagnostics_refresh_held() {
            return;
        }
        let Some(period) = self.tab.refresh() else { return };
        let due = now.duration_since(self.last_refresh) >= period;
        // A failed domain is not probed on the normal tick: a broken tool
        // (shell-ctl reports every one of its failures) must not be
        // relaunched every second. It's re-read at most every 15 s, and on
        // entering the tab or after an action.
        let ds: Vec<Domain> =
            self.tab
                .domains()
                .iter()
                .copied()
                .filter(|d| {
                    if self.domain_failed(*d) { self.last_read.get(d).is_none_or(|t| now.duration_since(*t) >= FAILED_RETRY) } else { due }
                })
                .collect();
        if due {
            self.last_refresh = now;
        }
        self.push_refresh(ds, now);
    }

    /// Requests a re-read of `ds` and records the time, domain by domain.
    fn push_refresh(&mut self, ds: Vec<Domain>, now: Instant) {
        if ds.is_empty() {
            return;
        }
        for d in &ds {
            self.last_read.insert(*d, now);
        }
        self.requests.push(Request::Refresh(ds));
    }

    fn domain_failed(&self, d: Domain) -> bool {
        match d {
            Domain::Volume => self.data.volume.is_failed(),
            Domain::Brightness => self.data.brightness.is_failed(),
            Domain::Dnd => self.data.dnd.is_failed(),
            Domain::KeepAwake => self.data.keep_awake.is_failed(),
            Domain::Profiles => self.data.profiles.is_failed(),
            Domain::Network => self.data.network.is_failed(),
            Domain::Bluetooth => self.data.bluetooth.is_failed(),
            Domain::Audio => self.data.audio.is_failed(),
            Domain::Displays => self.data.displays.is_failed(),
            Domain::Failed => self.domain_diag_failing(d),
            Domain::Health => false,
        }
    }

    fn refresh_tab(&mut self, now: Instant) {
        self.push_refresh(self.tab.domains().to_vec(), now);
        self.last_refresh = now;
    }

    // ── Navigation ───────────────────────────────────────────────────

    pub fn go_tab(&mut self, t: Tab, now: Instant) {
        if t != self.tab {
            if self.tab == Tab::Displays {
                // Mae moved on: a profile still waiting is not applied behind her back.
                self.displays.queued_profile = None;
            }
            if self.tab == Tab::Diagnostics {
                self.diag.view = diagnostics::View::Overview; // a view does not wait behind another tab
            }
            self.tab = t;
            self.focus = 0;
            self.search = Search::default();
            if t == Tab::Appearance {
                self.appearance_open();
            }
            if t == Tab::Diagnostics {
                self.diag.touched = false; // the first click on a returning visit only selects
                self.diagnostics_reads();
            }
        }
        // The journal view reads for itself: a tab key pressed on it queues nothing behind it.
        if !self.diagnostics_refresh_held() {
            self.refresh_tab(now);
        }
    }

    pub fn items(&self) -> Vec<Item> {
        match self.tab {
            Tab::Home => vec![
                Item::Slider(SliderId::Volume),
                Item::Slider(SliderId::Brightness),
                Item::Toggle(ToggleId::Wifi),
                Item::Toggle(ToggleId::Bluetooth),
                Item::Toggle(ToggleId::Dnd),
                Item::Toggle(ToggleId::KeepAwake),
                Item::Profile,
            ],
            Tab::Sound => sound::items(self),
            Tab::Network => vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))],
            Tab::Bluetooth => {
                vec![Item::Toggle(ToggleId::Bluetooth), Item::Button(Command::Open(External::Bluetooth))]
            }
            Tab::Power => PowerAction::ALL.iter().map(|a| Item::Button(Command::Power(*a))).collect(),
            Tab::Appearance => Vec::new(),
            Tab::Diagnostics => self.diag_items(),
            Tab::Displays => {
                std::iter::once(Item::DisplayProfiles).chain(self.outputs().iter().map(|o| Item::Output(o.name.clone()))).collect()
            }
        }
    }

    pub fn focused(&self) -> Option<Item> {
        self.items().get(self.focus).cloned()
    }

    pub(crate) fn move_focus(&mut self, delta: i32) {
        let n = self.items().len() as i32;
        self.focus = if n == 0 { 0 } else { (self.focus as i32 + delta).clamp(0, n - 1) as usize };
    }

    pub(crate) fn clamp_focus(&mut self) {
        let n = self.items().len();
        if self.focus >= n {
            self.focus = n.saturating_sub(1);
        }
    }

    pub fn matches_search(&self, label: &str) -> bool {
        self.search.text.is_empty() || fuzzy::score(&self.search.text, label).is_some()
    }

    // ── Commands ─────────────────────────────────────────────────────

    pub fn run_command(&mut self, c: Command, now: Instant) {
        match c {
            Command::ToggleWifi => self.toggle(ToggleId::Wifi, now),
            Command::ToggleBluetooth => self.toggle(ToggleId::Bluetooth, now),
            Command::ToggleDnd => self.toggle(ToggleId::Dnd, now),
            Command::ToggleKeepAwake => self.toggle(ToggleId::KeepAwake, now),
            Command::SetProfile(name) => {
                if let Loadable::Ready(p) = &mut self.data.profiles {
                    p.active = name.clone();
                }
                self.send_op(Op::SetProfile(name));
            }
            Command::Power(a) if a.needs_confirm() => {
                self.overlay = Overlay::Confirm(Confirm { action: a, yes_focused: false });
            }
            Command::Power(a) => self.send_op(Op::Power(a)),
            Command::Open(e) => self.external = Some(e),
            Command::GoTab(t) => self.go_tab(t, now),
            Command::ApplyDisplayProfile(name) => {
                // Applied on the next read: never on a stale state.
                self.go_tab(Tab::Displays, now);
                self.displays.queued_profile = Some(name);
                self.displays.queued_until = Some(now + displays::QUEUED_PROFILE_WAIT);
            }
        }
    }

    pub(crate) fn toggle(&mut self, id: ToggleId, now: Instant) {
        let Some(on) = self.toggle_loadable(id).ready().copied() else {
            self.toast(format!("{}: unknown state", toggle_label(id)), true, now);
            return;
        };
        let op = match id {
            ToggleId::Wifi => {
                if let Loadable::Ready(n) = &mut self.data.network {
                    n.wifi_enabled = !on;
                }
                Op::SetWifi(!on)
            }
            ToggleId::Bluetooth => {
                if let Loadable::Ready(b) = &mut self.data.bluetooth {
                    b.powered = !on;
                }
                Op::SetBluetooth(!on)
            }
            ToggleId::Dnd => {
                self.data.dnd = Loadable::Ready(!on);
                Op::SetDnd(!on)
            }
            ToggleId::KeepAwake => {
                self.data.keep_awake = Loadable::Ready(!on);
                Op::ToggleKeepAwake { now_on: !on }
            }
        };
        self.send_op(op);
    }

    /// Enter (or click) on the focused item.
    pub(crate) fn activate(&mut self, now: Instant) {
        match self.focused() {
            Some(Item::Toggle(id)) => self.toggle(id, now),
            Some(Item::Button(c)) => self.run_command(c, now),
            Some(Item::Slider(SliderId::Node(id))) => self.set_default(id),
            Some(Item::DisplayProfiles) => self.profile_bar_activate(now),
            Some(Item::Output(name)) => self.open_panel(&name, now),
            Some(Item::Diag(item)) => self.diag_activate(item),
            _ => {}
        }
    }

    /// -/+ or left/right arrows on the focused item.
    pub(crate) fn adjust(&mut self, dir: i32, now: Instant) {
        match self.focused() {
            Some(Item::Slider(id)) => self.nudge_slider(id, dir * STEP),
            Some(Item::Profile) => self.cycle_profile(dir, now),
            Some(Item::DisplayProfiles) => self.profile_bar_move(dir),
            _ => {}
        }
    }

    pub fn set_slider(&mut self, id: SliderId, value: i32) {
        let min = if id == SliderId::Brightness { i32::from(brightness::MIN_PERCENT) } else { 0 };
        let v = value.clamp(min, 100) as u8;
        match self.slider_value(id) {
            None => return, // unknown state: nothing sent blindly
            Some(cur) if cur == v => return,
            // Volume pushed above 100 % (wpctl allows it): a requested
            // increase would be clamped to 100 and would lower the sound.
            // Nothing is sent; a decrease, though, goes down normally (to
            // 100 at most).
            Some(cur) if cur > 100 && value > i32::from(cur) => return,
            Some(_) => {}
        }
        let op = match id {
            SliderId::Volume => {
                if let Loadable::Ready(vol) = &mut self.data.volume {
                    vol.percent = v;
                }
                Op::SetVolume(NodeRef::DefaultSink, v)
            }
            SliderId::Brightness => {
                self.data.brightness = Loadable::Ready(v);
                Op::SetBrightness(v)
            }
            SliderId::Node(n) => {
                if let Loadable::Ready(a) = &mut self.data.audio {
                    for node in a.sinks.iter_mut().chain(a.sources.iter_mut()) {
                        if node.id == n {
                            node.volume.percent = v;
                        }
                    }
                }
                Op::SetVolume(NodeRef::Id(n), v)
            }
        };
        self.send_op(op);
    }

    pub fn nudge_slider(&mut self, id: SliderId, delta: i32) {
        if let Some(cur) = self.slider_value(id) {
            self.set_slider(id, i32::from(cur) + delta);
        }
    }

    fn cycle_profile(&mut self, dir: i32, now: Instant) {
        let Some(p) = self.data.profiles.ready() else { return };
        let Some(cur) = p.available.iter().position(|n| *n == p.active) else { return };
        let next = (cur as i32 + dir).clamp(0, p.available.len() as i32 - 1) as usize;
        if next != cur {
            let name = p.available[next].clone();
            self.run_command(Command::SetProfile(name), now);
        }
    }

    pub(crate) fn close_confirm(&mut self, yes: bool) {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Confirm(c) => {
                if yes {
                    self.send_op(Op::Power(c.action));
                }
            }
            Overlay::UnitConfirm { scope, unit, action, .. } => {
                if yes {
                    self.run_unit(scope, unit, action);
                }
            }
            other => self.overlay = other,
        }
    }

    pub fn palette_matches(&self) -> Vec<(String, Command)> {
        let query = match &self.overlay {
            Overlay::Palette(p) => p.query.as_str(),
            _ => "",
        };
        let profiles = self.data.profiles.ready().map(|p| p.available.clone()).unwrap_or_default();
        let display_profiles: Vec<String> = self.displays.profiles.iter().map(|p| p.name.clone()).collect();
        let entries = palette_entries(&profiles, &display_profiles);
        let labels: Vec<&str> = entries.iter().map(|(l, _)| l.as_str()).collect();
        fuzzy::rank(query, &labels).into_iter().map(|i| entries[i].clone()).collect()
    }

    /// Runs the palette's `index` entry (or the selection), then closes it.
    pub(crate) fn palette_run(&mut self, index: Option<usize>, now: Instant) {
        let sel = match &self.overlay {
            Overlay::Palette(p) => index.unwrap_or(p.selected),
            _ => return,
        };
        let matches = self.palette_matches();
        self.overlay = Overlay::None;
        if let Some((_, c)) = matches.into_iter().nth(sel) {
            self.run_command(c, now);
        }
    }

    // ── Views for rendering ──────────────────────────────────────────

    fn node(&self, id: u32) -> Option<&AudioNode> {
        self.data.audio.ready()?.sinks.iter().chain(&self.data.audio.ready()?.sources).find(|n| n.id == id)
    }

    pub fn slider_loadable(&self, id: SliderId) -> Loadable<u8> {
        match id {
            SliderId::Volume => self.data.volume.map(|v| v.percent),
            SliderId::Brightness => self.data.brightness.clone(),
            SliderId::Node(n) => match &self.data.audio {
                Loadable::Ready(_) => self.node(n).map_or(Loadable::Loading, |x| Loadable::Ready(x.volume.percent)),
                other => other.map(|_| 0),
            },
        }
    }

    pub fn slider_value(&self, id: SliderId) -> Option<u8> {
        self.slider_loadable(id).ready().copied()
    }

    pub fn slider_muted(&self, id: SliderId) -> bool {
        match id {
            SliderId::Volume => self.data.volume.ready().is_some_and(|v| v.muted),
            SliderId::Brightness => false,
            SliderId::Node(n) => self.node(n).is_some_and(|x| x.volume.muted),
        }
    }

    pub fn slider_label(&self, id: SliderId) -> String {
        match id {
            SliderId::Volume => "Volume".into(),
            SliderId::Brightness => "Brightness".into(),
            SliderId::Node(n) => match self.node(n) {
                Some(x) => format!("{} {}", if x.default { "●" } else { " " }, x.name),
                None => "?".into(),
            },
        }
    }

    pub fn toggle_loadable(&self, id: ToggleId) -> Loadable<bool> {
        match id {
            ToggleId::Wifi => self.data.network.map(|n| n.wifi_enabled),
            ToggleId::Bluetooth => self.data.bluetooth.map(|b| b.powered),
            ToggleId::Dnd => self.data.dnd.clone(),
            ToggleId::KeepAwake => self.data.keep_awake.clone(),
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::backends::Displays;
    use crate::displays::Step;
    use crate::displays::wire::{Snapshot, Status, TrialState};
    use std::sync::Mutex;

    /// The daemon's displays, answered from memory: what the tab asks is recorded.
    pub struct FakeDisplays {
        pub snapshot: Mutex<Result<Snapshot, String>>,
        /// The answer to a try, a keep or a revert.
        pub answer: Mutex<Result<(), String>>,
        calls: Mutex<Vec<String>>,
    }

    impl Default for FakeDisplays {
        fn default() -> Self {
            let snapshot = Snapshot { status: Status::Ready, outputs: crate::displays::testing::machine(), trial: TrialState::None };
            FakeDisplays { snapshot: Mutex::new(Ok(snapshot)), answer: Mutex::new(Ok(())), calls: Mutex::default() }
        }
    }

    impl FakeDisplays {
        /// "snapshot", "try <output>,<output>", "keep", "revert".
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn call(&self, c: String) -> Result<(), String> {
            self.calls.lock().unwrap().push(c);
            self.answer.lock().unwrap().clone()
        }
    }

    impl Displays for FakeDisplays {
        fn snapshot(&self) -> Result<Snapshot, String> {
            self.calls.lock().unwrap().push("snapshot".into());
            self.snapshot.lock().unwrap().clone()
        }

        fn try_(&self, steps: &[Step]) -> Result<(), String> {
            self.call(format!("try {}", steps.iter().map(|s| s.output.as_str()).collect::<Vec<_>>().join(",")))
        }

        fn keep(&self) -> Result<(), String> {
            self.call("keep".into())
        }

        fn revert(&self) -> Result<(), String> {
            self.call("revert".into())
        }
    }

    /// The daemon's snapshot of `outputs`, no trial.
    pub fn ready(outputs: Vec<Output>) -> Snapshot {
        Snapshot { status: Status::Ready, outputs, trial: TrialState::None }
    }

    use crate::backends::{BtDevice, Connection};

    pub fn app() -> App {
        App::new(Config::default(), Theme::default(), Keymap::default(), Instant::now())
    }

    pub fn node(id: u32, name: &str, percent: u8, default: bool) -> AudioNode {
        AudioNode { id, name: name.into(), volume: Volume { percent, muted: false }, default }
    }

    /// Ready data, taken from the machine's recordings.
    pub fn ready_app() -> App {
        let mut a = app();
        a.take_requests();
        a.data = Data {
            volume: Loadable::Ready(Volume { percent: 62, muted: false }),
            brightness: Loadable::Ready(20),
            network: Loadable::Ready(NetStatus {
                wifi_enabled: true,
                active: vec![
                    Connection { name: "MON_WIFI_1".into(), kind: "Wi-Fi".into(), device: "wlp0s1f01".into() },
                    Connection { name: "Wired connection 1".into(), kind: "Ethernet".into(), device: "enp0s0u1u2i10".into() },
                ],
            }),
            bluetooth: Loadable::Ready(BtStatus {
                powered: true,
                connected: vec![BtDevice { mac: "AA:BB:CC:DD:EE:01".into(), name: "CasqueBT".into() }],
            }),
            dnd: Loadable::Ready(false),
            keep_awake: Loadable::Ready(false),
            profiles: Loadable::Ready(Profiles {
                active: "balanced".into(),
                available: vec!["performance".into(), "balanced".into(), "power-saver".into()],
            }),
            audio: Loadable::Ready(AudioStatus {
                sinks: vec![node(64, "Dock D6000 Dell", 75, false), node(116, "CasqueBT", 62, true)],
                sources: vec![node(113, "Stereo Microphone", 100, true)],
            }),
            displays: Loadable::Ready(crate::displays::testing::machine()),
        };
        a.displays.profiles = crate::displays::profiles::parse(PROFILES).unwrap();
        a
    }

    /// The Diag tab filled with anonymised data: all good, or with problems (a stale environment,
    /// failed units of both managers, a loop, a crash). Its reads are taken.
    pub fn diag_app(problems: bool) -> App {
        use crate::app::diagnostics::{CompositorHealth, HealthRead, WaylandHealth};
        use crate::app::worker::{DiagRead, DiagResult};
        use crate::backends::PokeAnswer;
        use crate::diagnostics::crashes::Crash;
        use crate::diagnostics::query::Scope;
        use crate::diagnostics::show::Restarts;
        use crate::diagnostics::units::FailedUnit;
        let t = Instant::now();
        let mut a = ready_app();
        a.go_tab(Tab::Diagnostics, t);
        let sock = "/run/user/1000/mango-1.sock";
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        let theirs = if problems { "wayland-0" } else { "wayland-1" };
        a.on_update(
            Update::Health(HealthRead {
                compositor: Some(CompositorHealth { name: "Mango", var: "MANGO_INSTANCE_SIGNATURE", answers: true }),
                wayland: WaylandHealth::Reachable("wayland-1".into()),
                systemd_env: Ok(vec![pair("WAYLAND_DISPLAY", theirs), pair("MANGO_INSTANCE_SIGNATURE", sock)]),
                own_env: vec![pair("WAYLAND_DISPLAY", "wayland-1"), pair("MANGO_INSTANCE_SIGNATURE", sock)],
            }),
            t,
        );
        a.on_update(Update::Poke(PokeAnswer::Done), t);
        let unit = |n: &str, d: &str| FailedUnit {
            unit: n.into(),
            load: "loaded".into(),
            active: "failed".into(),
            sub: "failed".into(),
            description: d.into(),
        };
        let (system, user) = if problems {
            (
                vec![unit("mnt-data.mount", "/mnt/data"), unit("example-sync.service", "Example sync job")],
                vec![unit("example-app.service", "Example app")],
            )
        } else {
            (vec![], vec![])
        };
        a.on_update(Update::Failed(Scope::System, Ok(system)), t);
        a.on_update(Update::Failed(Scope::User, Ok(user)), t);
        let loops = if problems {
            vec![Restarts { unit: "example-sync.service".into(), restarts: 7, active: "activating".into(), sub: "auto-restart".into() }]
        } else {
            vec![]
        };
        let crashes = if problems {
            vec![Crash { time_us: 1_795_183_320_000_000, pid: 4242, signal: 11, core_present: true, exe_name: "example-app".into() }]
        } else {
            vec![]
        };
        let boot = "Startup finished in 18.1s (firmware) + 3.3s (loader) + 0.9s (kernel) + 10.8s (initrd) + 9.8s (userspace) = 42.9s";
        for (read, result) in [
            (DiagRead::Restarts(3), DiagResult::Restarts { system: Ok(vec![]), user: Ok(loops) }),
            (DiagRead::Crashes, DiagResult::Crashes(Ok(crashes))),
            (DiagRead::BootTime, DiagResult::BootTime(Ok(boot.into()))),
        ] {
            a.on_update(Update::Diag(read, result), t);
        }
        a.take_requests();
        a
    }

    /// Test display profiles (anonymized descriptions from the machine).
    pub const PROFILES: &str = r#"
[[profile]]
name = "Laptop seul"
[[profile.display]]
description = "Interne Dalle 0x0001"
mode = "2560x1600@60"
[[profile.display]]
description = "Alpha Modèle 1 SERIE0001"
enabled = false
[[profile.display]]
description = "Bravo Modèle 2 SERIE0002"
enabled = false

[[profile]]
name = "Bureau"
[[profile.display]]
description = "Alpha Modèle 1 SERIE0001"
mode = "1920x1080@60"
[[profile.display]]
description = "Bravo Modèle 2 SERIE0002"
mode = "1920x1080@60"
placement = { relation = "right_of", reference = "Alpha Modèle 1 SERIE0001" }
[[profile.display]]
description = "Interne Dalle 0x0001"
enabled = false
"#;
}

#[cfg(test)]
mod tests;
