//! alpheus's state and logic, with no terminal or thread: every input (key,
//! mouse, worker response, tick) changes the state; requests to the worker
//! and the external tool to open are left in boxes that the real loop
//! (run.rs) drains. Everything is tested by calling methods.

pub mod action;
pub mod appearance;
pub mod displays;
mod input;
pub mod keymap;
mod mouse;
pub mod run;
pub mod worker;

use crate::backends::{AudioNode, AudioStatus, BResult, BtStatus, NetStatus, NodeRef, PowerAction, Profiles, Volume, brightness};
use crate::config::Config;
use crate::displays::Output;
use crate::fuzzy;
use crate::theme::Theme;
use action::{Command, External, Tab, palette_entries, profile_label};
pub use appearance::AppearanceTab;
pub use displays::{DisplaysTab, KEEP_DELAY, Keep, Panel, PanelField, PlaceChoice, SaveDialog, Trial};
use keymap::Keymap;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use worker::{Domain, Op, Request, Update, domains};

/// Slider step (-/+ keys, scroll wheel).
pub const STEP: i32 = 5;
/// Lifetime of an ephemeral message.
pub const TOAST_TTL: Duration = Duration::from_secs(3);
/// A failed domain is re-read at most once per this period.
pub const FAILED_RETRY: Duration = Duration::from_secs(15);

/// Refresh period of the visible tab: Home every second, Sound/Network/
/// Bluetooth/Displays every 2 s, Power never (nothing to read).
pub fn refresh_period(tab: Tab) -> Option<Duration> {
    match tab {
        Tab::Home => Some(Duration::from_secs(1)),
        Tab::Sound | Tab::Network | Tab::Bluetooth | Tab::Displays => Some(Duration::from_secs(2)),
        Tab::Power | Tab::Appearance => None,
    }
}

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
        Op::SetWifi(_) => "Wi-Fi".into(),
        Op::SetBluetooth(_) => "Bluetooth".into(),
        Op::SetDnd(_) => "Do not disturb".into(),
        Op::ToggleKeepAwake { .. } => "Keep awake".into(),
        Op::SetProfile(_) => "Energy profile".into(),
        Op::Power(a) => a.label().into(),
        Op::ApplyDisplays { .. } => "Displays".into(),
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
        Update::Done(..) | Update::Superseded(_) => return None,
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
    /// Slider currently being dragged with the mouse, and its bar.
    pub(crate) dragging: Option<(SliderId, Rect)>,
    last_refresh: Instant,
    requests: Vec<Request>,
    external: Option<External>,
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
            dragging: None,
            last_refresh: now,
            requests: Vec::new(),
            external: None,
            pending: HashMap::new(),
            last_read: HashMap::new(),
        };
        app.refresh_tab(now);
        app
    }

    /// Under Sway or niri (`name`) the Displays tab calls nothing: a read of the displays already
    /// queued (opening on that tab) is dropped too.
    pub fn set_displays_unavailable(&mut self, name: Option<&'static str>) {
        self.displays.unavailable = name;
        if name.is_none() {
            return;
        }
        for r in &mut self.requests {
            if let Request::Refresh(ds) = r {
                ds.retain(|d| *d != Domain::Displays);
            }
        }
        self.requests.retain(|r| !matches!(r, Request::Refresh(ds) if ds.is_empty()));
        self.last_read.remove(&Domain::Displays);
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
                return; // stale read: an action on this domain is in progress
            }
        }
        match u {
            Update::Volume(r) => self.data.volume = r.into(),
            Update::Brightness(r) => self.data.brightness = r.into(),
            Update::Network(r) => self.data.network = r.into(),
            Update::Bluetooth(r) => self.data.bluetooth = r.into(),
            Update::Dnd(r) => self.data.dnd = r.into(),
            Update::KeepAwake(r) => self.data.keep_awake = r.into(),
            Update::Profiles(r) => self.data.profiles = r.into(),
            Update::Audio(r) => self.data.audio = r.into(),
            Update::Displays(r) => {
                self.data.displays = r.into();
                self.displays_read(now);
            }
            Update::Done(op @ Op::ApplyDisplays { .. }, r) => {
                self.op_answered(&op);
                self.displays_done(op, r, now);
            }
            Update::Done(op, r) => {
                self.op_answered(&op);
                self.op_done(op, r, now);
            }
            Update::Superseded(op) => self.op_answered(&op),
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
                let msg = format!("{}: {e}", op_label(&op));
                self.status.error = Some(msg.clone());
                self.toast(msg, true, now);
                self.refresh_tab(now); // cancels the optimistic display
            }
        }
    }

    fn success_message(&self, op: &Op) -> Option<String> {
        let onoff = |b: bool| if b { "on" } else { "off" };
        Some(match op {
            Op::SetVolume(..) | Op::SetBrightness(_) => return None,
            Op::SetDefault(id) => {
                let a = self.data.audio.ready()?;
                match a.sinks.iter().find(|n| n.id == *id) {
                    Some(n) => format!("Output: {}", n.name),
                    None => format!("Input: {}", a.sources.iter().find(|n| n.id == *id)?.name),
                }
            }
            Op::SetWifi(b) => format!("Wi-Fi: {}", onoff(*b)),
            Op::SetBluetooth(b) => format!("Bluetooth: {}", onoff(*b)),
            Op::SetDnd(b) => format!("Do not disturb: {}", onoff(*b)),
            Op::ToggleKeepAwake { now_on } => format!("Keep awake: {}", onoff(*now_on)),
            Op::SetProfile(p) => format!("Profile: {}", profile_label(p)),
            Op::Power(a) => a.label().to_string(),
            Op::ApplyDisplays { .. } => return None,
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
        let Some(period) = refresh_period(self.tab) else { return };
        let due = now.duration_since(self.last_refresh) >= period;
        // A failed domain is not probed on the normal tick: a broken tool
        // (shell-ctl reports every one of its failures) must not be
        // relaunched every second. It's re-read at most every 15 s, and on
        // entering the tab or after an action.
        let ds: Vec<Domain> =
            domains(self.tab)
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
    fn push_refresh(&mut self, mut ds: Vec<Domain>, now: Instant) {
        if self.displays.unavailable.is_some() {
            ds.retain(|d| *d != Domain::Displays); // under Sway or niri, the tab calls nothing
        }
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
        }
    }

    fn refresh_tab(&mut self, now: Instant) {
        self.push_refresh(domains(self.tab).to_vec(), now);
        self.last_refresh = now;
    }

    // ── Navigation ───────────────────────────────────────────────────

    pub fn go_tab(&mut self, t: Tab, now: Instant) {
        if t != self.tab {
            self.tab = t;
            self.focus = 0;
            self.search = Search::default();
            if t == Tab::Appearance {
                self.appearance_open();
            }
        }
        self.refresh_tab(now);
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
            Tab::Sound => {
                let (sinks, sources) = self.visible_nodes();
                let mut v: Vec<Item> = sinks.iter().chain(sources.iter()).map(|n| Item::Slider(SliderId::Node(n.id))).collect();
                v.push(Item::Button(Command::Open(External::Mixer)));
                v
            }
            Tab::Network => vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))],
            Tab::Bluetooth => {
                vec![Item::Toggle(ToggleId::Bluetooth), Item::Button(Command::Open(External::Bluetooth))]
            }
            Tab::Power => PowerAction::ALL.iter().map(|a| Item::Button(Command::Power(*a))).collect(),
            Tab::Appearance => Vec::new(),
            Tab::Displays if self.displays.unavailable.is_some() => Vec::new(),
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

    /// Outputs then inputs, filtered by the search.
    pub fn visible_nodes(&self) -> (Vec<&AudioNode>, Vec<&AudioNode>) {
        match self.data.audio.ready() {
            None => (Vec::new(), Vec::new()),
            Some(a) => (
                a.sinks.iter().filter(|n| self.matches_search(&n.name)).collect(),
                a.sources.iter().filter(|n| self.matches_search(&n.name)).collect(),
            ),
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
                if self.displays.unavailable.is_none() {
                    self.displays.queued_profile = Some(name);
                }
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

    fn set_default(&mut self, id: u32) {
        if let Loadable::Ready(a) = &mut self.data.audio {
            for list in [&mut a.sinks, &mut a.sources] {
                if list.iter().any(|n| n.id == id) {
                    for n in list.iter_mut() {
                        n.default = n.id == id;
                    }
                }
            }
        }
        self.send_op(Op::SetDefault(id));
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
        if let Overlay::Confirm(c) = self.overlay {
            self.overlay = Overlay::None;
            if yes {
                self.send_op(Op::Power(c.action));
            }
        }
    }

    pub fn palette_matches(&self) -> Vec<(String, Command)> {
        let query = match &self.overlay {
            Overlay::Palette(p) => p.query.as_str(),
            _ => "",
        };
        let profiles = self.data.profiles.ready().map(|p| p.available.clone()).unwrap_or_default();
        let display_profiles: Vec<String> =
            if self.displays.unavailable.is_some() { Vec::new() } else { self.displays.profiles.iter().map(|p| p.name.clone()).collect() };
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
mod tests {
    use super::testing::*;
    use super::*;
    use crate::backends::BackendError;

    fn later(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn startup_requests_home() {
        let mut a = app();
        assert_eq!(a.tab, Tab::Home);
        assert_eq!(a.take_requests(), vec![Request::Refresh(domains(Tab::Home).to_vec())]);
        assert_eq!(a.data.volume, Loadable::Loading);
    }

    #[test]
    fn updates() {
        let mut a = app();
        a.on_update(Update::Volume(Ok(Volume { percent: 40, muted: true })), Instant::now());
        a.on_update(Update::Brightness(Err(BackendError::Missing { tool: "brightnessctl".into() })), Instant::now());
        assert_eq!(a.data.volume, Loadable::Ready(Volume { percent: 40, muted: true }));
        assert_eq!(a.data.brightness, Loadable::Failed("brightnessctl not found".into()));
    }

    #[test]
    fn display_read_ignored_during_a_setting() {
        let mut a = ready_app();
        a.on_update(Update::Displays(Ok(Vec::new())), Instant::now());
        assert_eq!(a.data.displays, Loadable::Ready(Vec::new()));
        a.send_op(Op::ApplyDisplays { plan: crate::displays::Plan::default(), undo: None });
        a.on_update(Update::Displays(Ok(crate::displays::testing::machine())), Instant::now());
        assert_eq!(a.data.displays, Loadable::Ready(Vec::new()), "stale read ignored");
        a.on_update(Update::Displays(Err(BackendError::Missing { tool: "wm-set".into() })), Instant::now());
        assert_eq!(a.data.displays, Loadable::Ready(Vec::new()));
    }

    #[test]
    fn optimistic_slider_and_request() {
        let mut a = ready_app();
        a.nudge_slider(SliderId::Volume, STEP);
        assert_eq!(a.slider_value(SliderId::Volume), Some(67));
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 67))]);
        a.nudge_slider(SliderId::Node(64), -STEP);
        assert_eq!(a.slider_value(SliderId::Node(64)), Some(70));
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::Id(64), 70))]);
    }

    #[test]
    fn sliders_are_bounded() {
        let mut a = ready_app();
        a.set_slider(SliderId::Volume, 150);
        a.set_slider(SliderId::Brightness, -20);
        assert_eq!(
            a.take_requests(),
            vec![
                Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100)),
                Request::Do(Op::SetBrightness(crate::backends::brightness::MIN_PERCENT)),
            ]
        );
    }

    #[test]
    fn volume_above_100_is_never_lowered_by_plus() {
        let mut a = ready_app();
        a.data.volume = Loadable::Ready(Volume { percent: 120, muted: false });
        a.nudge_slider(SliderId::Volume, STEP);
        assert!(a.take_requests().is_empty(), "\"+\" does not bring 120 % down to 100 %");
        assert_eq!(a.slider_value(SliderId::Volume), Some(120));
        a.nudge_slider(SliderId::Volume, -STEP);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100))], "\"-\" clamps to 100");
        assert_eq!(a.slider_value(SliderId::Volume), Some(100));
        // Same rule for a device in the Sound tab.
        if let Loadable::Ready(st) = &mut a.data.audio {
            st.sinks[0].volume.percent = 130;
        }
        a.nudge_slider(SliderId::Node(64), STEP);
        assert!(a.take_requests().is_empty());
        a.nudge_slider(SliderId::Node(64), -STEP);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::Id(64), 100))]);
    }

    #[test]
    fn unknown_or_unchanged_slider_sends_nothing() {
        let mut a = app();
        a.take_requests();
        a.nudge_slider(SliderId::Volume, STEP);
        assert!(a.take_requests().is_empty());
        let mut a = ready_app();
        a.set_slider(SliderId::Volume, 62);
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn optimistic_toggle() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false));
        a.toggle(ToggleId::KeepAwake, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false)), Request::Do(Op::ToggleKeepAwake { now_on: true })]);
    }

    fn net(wifi: bool) -> Update {
        Update::Network(Ok(NetStatus { wifi_enabled: wifi, active: Vec::new() }))
    }

    #[test]
    fn stale_read_ignored_during_a_toggle() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now());
        a.take_requests();
        // Read launched before the toggle: it carries the old state.
        a.on_update(net(true), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false), "the display stays optimistic");
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        // After the response, the next read is trusted.
        a.on_update(net(true), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
    }

    #[test]
    fn other_domains_keep_being_read_during_a_toggle() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now());
        a.on_update(Update::Dnd(Ok(true)), Instant::now());
        assert_eq!(a.data.dnd, Loadable::Ready(true));
    }

    #[test]
    fn two_toggles_wait_for_both_responses() {
        let mut a = ready_app();
        a.toggle(ToggleId::Wifi, Instant::now()); // → off
        a.toggle(ToggleId::Wifi, Instant::now()); // → on
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        a.on_update(net(false), Instant::now()); // read between the two actions
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
        a.on_update(Update::Done(Op::SetWifi(true), Ok(())), Instant::now());
        a.on_update(net(true), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
        a.on_update(net(false), Instant::now());
        assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false), "nothing pending any more");
    }

    #[test]
    fn coalesced_slider_burst_does_not_lock_the_domain() {
        let mut a = ready_app();
        a.nudge_slider(SliderId::Brightness, STEP); // 25
        a.nudge_slider(SliderId::Brightness, STEP); // 30
        a.nudge_slider(SliderId::Brightness, STEP); // 35
        a.on_update(Update::Brightness(Ok(20)), Instant::now());
        assert_eq!(a.data.brightness, Loadable::Ready(35), "stale read ignored");
        // The thread coalesced the burst: two settings dropped, one done.
        a.on_update(Update::Superseded(Op::SetBrightness(25)), Instant::now());
        a.on_update(Update::Superseded(Op::SetBrightness(30)), Instant::now());
        a.on_update(Update::Done(Op::SetBrightness(35), Ok(())), Instant::now());
        a.on_update(Update::Brightness(Ok(40)), Instant::now());
        assert_eq!(a.data.brightness, Loadable::Ready(40), "the domain did not stay locked");
    }

    #[test]
    fn default_volume_also_locks_the_audio_list() {
        let mut a = ready_app();
        a.nudge_slider(SliderId::Volume, STEP);
        a.on_update(Update::Audio(Ok(AudioStatus { sinks: Vec::new(), sources: Vec::new() })), Instant::now());
        assert_eq!(a.data.audio.ready().unwrap().sinks.len(), 2);
        a.on_update(Update::Done(Op::SetVolume(NodeRef::DefaultSink, 67), Ok(())), Instant::now());
        a.on_update(Update::Audio(Ok(AudioStatus { sinks: Vec::new(), sources: Vec::new() })), Instant::now());
        assert!(a.data.audio.ready().unwrap().sinks.is_empty());
    }

    #[test]
    fn keep_awake_message_survives_a_stale_value() {
        let mut a = ready_app(); // off
        a.toggle(ToggleId::KeepAwake, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::ToggleKeepAwake { now_on: true })]);
        a.on_update(Update::KeepAwake(Ok(false)), Instant::now()); // read before the toggle
        a.on_update(Update::Done(Op::ToggleKeepAwake { now_on: true }, Ok(())), Instant::now());
        assert_eq!(a.toasts[0].text, "Keep awake: on");
        // Even if the displayed state was stale, the message follows the intended state.
        a.data.keep_awake = Loadable::Ready(false);
        a.on_update(Update::Done(Op::ToggleKeepAwake { now_on: true }, Ok(())), Instant::now());
        assert_eq!(a.toasts[1].text, "Keep awake: on");
    }

    #[test]
    fn toggling_from_an_unknown_state() {
        let mut a = app();
        a.take_requests();
        a.toggle(ToggleId::Dnd, Instant::now());
        assert!(a.take_requests().is_empty());
        assert_eq!(a.toasts[0].text, "Do not disturb: unknown state");
        assert!(a.toasts[0].error);
    }

    #[test]
    fn changing_tab() {
        let mut a = ready_app();
        a.focus = 3;
        a.search.text = "x".into();
        a.go_tab(Tab::Sound, Instant::now());
        assert_eq!((a.tab, a.focus), (Tab::Sound, 0));
        assert_eq!(a.search, Search::default());
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Audio])]);
        a.go_tab(Tab::Power, Instant::now());
        assert!(a.take_requests().is_empty()); // nothing to read
    }

    #[test]
    fn items_per_tab() {
        let mut a = ready_app();
        assert_eq!(a.items().len(), 7);
        a.go_tab(Tab::Sound, Instant::now());
        assert_eq!(
            a.items(),
            vec![
                Item::Slider(SliderId::Node(64)),
                Item::Slider(SliderId::Node(116)),
                Item::Slider(SliderId::Node(113)),
                Item::Button(Command::Open(External::Mixer)),
            ]
        );
        a.go_tab(Tab::Power, Instant::now());
        assert_eq!(a.items().len(), 5);
        a.go_tab(Tab::Network, Instant::now());
        assert_eq!(a.items(), vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))]);
    }

    #[test]
    fn search_filter() {
        let mut a = ready_app();
        a.go_tab(Tab::Sound, Instant::now());
        a.search.text = "casq".into();
        let (sinks, sources) = a.visible_nodes();
        assert_eq!(sinks.iter().map(|n| n.id).collect::<Vec<_>>(), vec![116]);
        assert!(sources.is_empty());
        assert_eq!(a.items().len(), 2);
        a.focus = 3;
        a.clamp_focus();
        assert_eq!(a.focus, 1);
    }

    #[test]
    fn activating_an_output_makes_it_the_default() {
        let mut a = ready_app();
        a.go_tab(Tab::Sound, Instant::now());
        a.take_requests();
        a.activate(Instant::now());
        let st = a.data.audio.ready().unwrap();
        assert!(st.sinks[0].default && !st.sinks[1].default);
        assert!(st.sources[0].default, "inputs do not move");
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDefault(64))]);
    }

    #[test]
    fn profile_is_bounded() {
        let mut a = ready_app();
        a.focus = 6;
        a.adjust(1, Instant::now());
        assert_eq!(a.data.profiles.ready().unwrap().active, "power-saver");
        a.adjust(1, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetProfile("power-saver".into()))]);
    }

    #[test]
    fn power_with_or_without_confirmation() {
        let mut a = ready_app();
        a.run_command(Command::Power(PowerAction::Poweroff), Instant::now());
        assert_eq!(a.overlay, Overlay::Confirm(Confirm { action: PowerAction::Poweroff, yes_focused: false }));
        assert!(a.take_requests().is_empty());
        a.close_confirm(false);
        assert_eq!(a.overlay, Overlay::None);
        assert!(a.take_requests().is_empty());
        a.run_command(Command::Power(PowerAction::Poweroff), Instant::now());
        a.close_confirm(true);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Poweroff))]);
        a.run_command(Command::Power(PowerAction::Lock), Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Lock))]);
    }

    #[test]
    fn external_tool() {
        let mut a = ready_app();
        a.run_command(Command::Open(External::Mixer), Instant::now());
        assert_eq!(a.take_external(), Some(External::Mixer));
        assert_eq!(a.take_external(), None);
        assert_eq!(a.external_argv(External::Mixer), vec!["wiremix"]);
        a.external_done(External::Mixer, Err("wiremix not found".into()), Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Per-app mixer: wiremix not found"));
        assert!(a.toasts[0].error);
    }

    #[test]
    fn successful_action() {
        let mut a = ready_app();
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        assert_eq!(a.toasts[0].text, "Wi-Fi: off");
        assert_eq!(a.status.last.as_deref(), Some("Wi-Fi: off"));
        assert_eq!(a.take_requests(), vec![Request::Refresh(domains(Tab::Home).to_vec())]);
        a.on_update(Update::Done(Op::SetDefault(116), Ok(())), Instant::now());
        assert_eq!(a.toasts[1].text, "Output: CasqueBT");
        a.on_update(Update::Done(Op::SetDefault(113), Ok(())), Instant::now());
        assert_eq!(a.toasts[2].text, "Input: Stereo Microphone");
    }

    #[test]
    fn successful_slider_has_no_message_or_reread() {
        let mut a = ready_app();
        a.on_update(Update::Done(Op::SetVolume(NodeRef::DefaultSink, 50), Ok(())), Instant::now());
        assert!(a.toasts.is_empty());
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn failed_action() {
        let mut a = ready_app();
        let e = BackendError::Failed { tool: "shell-ctl".into(), code: Some(1), stderr: "boom".into() };
        a.on_update(Update::Done(Op::SetDnd(true), Err(e)), Instant::now());
        assert_eq!(a.status.error.as_deref(), Some("Do not disturb: shell-ctl failed (code 1): boom"));
        assert!(a.toasts[0].error);
        assert_eq!(a.take_requests().len(), 1, "re-read to cancel the optimistic display");
    }

    #[test]
    fn tick_refreshes_home_every_second() {
        let mut a = ready_app();
        a.on_tick(later(1100));
        assert_eq!(a.take_requests(), vec![Request::Refresh(domains(Tab::Home).to_vec())]);
        a.on_tick(later(1500));
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn tick_does_not_reread_a_failed_domain() {
        let mut a = ready_app();
        a.data.dnd = Loadable::Failed("shell-ctl failed (code 1)".into());
        a.on_tick(later(1100));
        let r = a.take_requests();
        assert_eq!(r.len(), 1);
        let Request::Refresh(ds) = &r[0] else { panic!("{r:?}") };
        assert!(!ds.contains(&Domain::Dnd));
        assert_eq!(ds.len(), 6);
    }

    #[test]
    fn tick_refreshes_sound_network_bluetooth_every_two_seconds() {
        for (tab, d) in [
            (Tab::Sound, Domain::Audio),
            (Tab::Network, Domain::Network),
            (Tab::Bluetooth, Domain::Bluetooth),
            (Tab::Displays, Domain::Displays),
        ] {
            let mut a = ready_app();
            let t0 = Instant::now();
            a.go_tab(tab, t0);
            a.take_requests();
            a.on_tick(t0 + Duration::from_millis(1100));
            assert!(a.take_requests().is_empty(), "{tab:?}: not read at 1 s");
            a.on_tick(t0 + Duration::from_millis(2100));
            assert_eq!(a.take_requests(), vec![Request::Refresh(vec![d])], "{tab:?}: read at 2 s");
            a.on_tick(t0 + Duration::from_millis(3000));
            assert!(a.take_requests().is_empty(), "{tab:?}: the period restarted from the read");
        }
    }

    fn refreshed(r: &[Request]) -> Vec<Domain> {
        r.iter()
            .flat_map(|q| match q {
                Request::Refresh(ds) => ds.clone(),
                Request::Do(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn failed_domain_reread_every_fifteen_seconds() {
        let mut a = ready_app();
        let t0 = Instant::now();
        a.go_tab(Tab::Home, t0); // full read at t0
        a.take_requests();
        a.data.dnd = Loadable::Failed("shell-ctl failed (code 1)".into());
        let at = |ms| t0 + Duration::from_millis(ms);
        a.on_tick(at(1100));
        assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "not read at 1 s");
        a.on_tick(at(14_900));
        assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "not read before 15 s");
        a.on_tick(at(15_100));
        let r = refreshed(&a.take_requests());
        assert_eq!(r, vec![Domain::Dnd], "read at 15 s, without waiting for the normal tick");
        a.on_tick(at(16_200));
        assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "the 15 s period restarts");
        a.on_tick(at(30_200));
        assert!(refreshed(&a.take_requests()).contains(&Domain::Dnd));
    }

    #[test]
    fn failed_tab_reread_only_at_fifteen_seconds() {
        let mut a = ready_app();
        let t0 = Instant::now();
        a.go_tab(Tab::Sound, t0);
        a.take_requests();
        a.data.audio = Loadable::Failed("wpctl not found".into());
        a.on_tick(t0 + Duration::from_millis(2100));
        assert!(a.take_requests().is_empty(), "not read at the 2 s tick");
        a.on_tick(t0 + Duration::from_millis(15_100));
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Audio])]);
    }

    #[test]
    fn tick_reads_nothing_on_power() {
        let mut a = ready_app();
        let t0 = Instant::now();
        a.go_tab(Tab::Power, t0);
        a.take_requests();
        a.on_tick(t0 + Duration::from_secs(60));
        assert!(a.take_requests().is_empty());
    }

    #[test]
    fn messages_expire_and_are_capped_at_three() {
        let mut a = ready_app();
        for _ in 0..5 {
            a.on_update(Update::Done(Op::SetWifi(true), Ok(())), Instant::now());
        }
        assert_eq!(a.toasts.len(), 3);
        a.on_tick(Instant::now() + TOAST_TTL + Duration::from_millis(100));
        assert!(a.toasts.is_empty());
    }

    #[test]
    fn palette_without_accents() {
        let mut a = ready_app();
        a.overlay = Overlay::Palette(Palette { query: "poweroff".into(), selected: 0 });
        assert_eq!(a.palette_matches()[0].0, "Power off");
        a.palette_run(None, Instant::now());
        assert!(matches!(a.overlay, Overlay::Confirm(_)));
    }

    #[test]
    fn startup_warning() {
        let mut a = app();
        a.warn("invalid config.toml: x".into());
        assert_eq!(a.status.warning.as_deref(), Some("invalid config.toml: x"));
        assert_eq!(a.status.error, None);
        a.warn("invalid colors.toml: y".into());
        assert_eq!(a.status.warning.as_deref(), Some("invalid config.toml: x ; invalid colors.toml: y"), "neither is lost");
    }

    #[test]
    fn the_warning_survives_actions() {
        let mut a = ready_app();
        a.warn("invalid config.toml: x".into());
        let e = BackendError::Failed { tool: "shell-ctl".into(), code: Some(1), stderr: "boom".into() };
        a.on_update(Update::Done(Op::SetDnd(true), Err(e)), Instant::now());
        assert!(a.status.error.is_some());
        a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
        assert_eq!(a.status.error, None, "the last action error is cleared on the next success");
        assert_eq!(a.status.warning.as_deref(), Some("invalid config.toml: x"), "the warning remains");
    }
}
