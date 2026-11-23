//! The control center's state and logic: the `App` that every input changes.
//!
//! A key, a mouse event, a worker answer or a clock tick each arrives as a method call on `App`
//! and changes its state. What has to reach the outside is mostly left in boxes that the real loop
//! (`run.rs`) empties: requests for the worker (`take_requests`), an external tool to open
//! (`take_external`) and a system unit's action (`take_system_unit`, in `diagnostics.rs`). Two
//! things the `App` does itself, on the loop thread: it writes theme.toml (`appearance_apply`, then
//! reloads the theme with `Theme::load`) and displays.toml (`displays.rs`, `profiles::save`). So
//! behaviour is tested by calling methods, with no terminal and no worker thread (the fake
//! backends are for `worker.rs`'s tests).
//!
//! This file holds what every tab shares: the data types (`Data`, `Item`, `Overlay`), the focus,
//! the sliders and toggles, the commands, the palette and the rule that decides what is read again
//! and when (`on_tick`). A tab's own state is in the file of its name (`sound.rs`, `displays.rs`,
//! ...); keys are in `input.rs`, the mouse in `mouse.rs`.
//!
//! Never:
//! - touch the terminal or draw: `ui/` draws from this state;
//! - start a thread or run a tool: a change is queued with `send_op` and the worker runs it
//!   (`worker.rs`);
//! - read the monotonic clock (`Instant::now`): every method that needs the time takes `now` from
//!   its caller, so a test can pick any time it likes. The one exception is the journal's wall-clock
//!   stamp (`journal.rs`'s `wall_secs`, `SystemTime::now`).
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3, "Refresh"), docs/features/control-center.md.

pub mod action;
pub mod appearance;
mod bluetooth;
pub mod diagnostics;
pub mod displays;
mod home;
mod input;
pub mod journal;
pub mod keymap;
mod mouse;
mod network;
mod power;
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
/// A failed domain is re-read at most once per this period (spec § 3: "a failing domain is
/// retried at most every 15 s"). Without it a failed domain would be re-read once per tab period
/// (1 to 5 s, `TabSpec::refresh`) instead of every 15 s.
pub const FAILED_RETRY: Duration = Duration::from_secs(15);

/// A value the worker may not have read yet: `Loading` before the first answer, `Ready` once read,
/// `Failed` with the message the tab shows when the read failed.
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

/// What the worker has read, one field per domain that the Home, Sound, Network, Bluetooth and
/// Displays tabs show.
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

/// A slider: the default output's volume, the backlight, or one node of the Sound tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SliderId {
    Volume,
    Brightness,
    Node(u32),
}

/// A switch with two states. Wi-Fi is on Home and on the Network tab, Bluetooth on Home and the
/// Bluetooth tab; Do not disturb and Keep awake are on Home only (no tab of their own).
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

/// The command palette's state: the text typed and the entry under the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

/// The power confirmation box (`Overlay::Confirm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirm {
    pub action: PowerAction,
    /// Focus starts on "Cancel": Enter by reflex powers nothing off.
    pub yes_focused: bool,
}

/// What is drawn over the tab and takes the keys first; at most one at a time.
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

/// The text typed after `/`, and whether the search box still has the keys (`editing`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Search {
    pub text: String,
    pub editing: bool,
}

/// A corner message that disappears at `until` (see `TOAST_TTL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

/// What the status shows: the last message (the classic layout's status line, the foot of the
/// rail), the last action's error and the startup warning (the status line, the rail's header).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub last: Option<String>,
    /// Last action error; cleared on the next success.
    pub error: Option<String>,
    /// Startup warning (config, theme, keys): persistent.
    pub warning: Option<String>,
}

/// The name a toggle goes by, in its row, its toast and the palette.
pub fn toggle_label(id: ToggleId) -> &'static str {
    match id {
        ToggleId::Wifi => "Wi-Fi",
        ToggleId::Bluetooth => "Bluetooth",
        ToggleId::Dnd => "Do not disturb",
        ToggleId::KeepAwake => "Keep awake",
    }
}

/// The word that starts an action's failure message (`Volume: ...`).
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

/// Everything the control center remembers. The real loop (`run.rs`) owns one.
pub struct App {
    /// The tab shown.
    pub tab: Tab,
    pub data: Data,
    /// Index of the focused item in `items()`.
    pub focus: usize,
    pub overlay: Overlay,
    pub search: Search,
    pub status: Status,
    pub toasts: Vec<Toast>,
    pub keymap: Keymap,
    pub config: Config,
    pub theme: Theme,
    /// Set by the Quit action; `run.rs` leaves its loop when it sees it.
    pub should_quit: bool,
    pub displays: DisplaysTab,
    pub appearance: AppearanceTab,
    pub diag: DiagnosticsTab,
    /// Slider currently being dragged with the mouse, and its bar.
    pub(crate) dragging: Option<(SliderId, Rect)>,
    /// When the shown tab was last asked for a whole refresh (`on_tick` compares it with the
    /// tab's period).
    last_refresh: Instant,
    /// Requests for the worker, emptied by `take_requests`.
    requests: Vec<Request>,
    /// An external tool to open, emptied by `take_external`.
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

    /// Opens directly on `tab`: only its domains are read (the refresh rule: only the shown tab is
    /// read).
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

    /// Adds a startup warning (config, theme, keys), kept for the whole run and shown with the
    /// last action's error.
    pub fn warn(&mut self, msg: String) {
        self.status.warning = Some(match self.status.warning.take() {
            Some(prev) => format!("{prev} ; {msg}"), // several faulty files
            None => msg,
        });
    }

    /// The command that opens `e`, from `[commands]` in config.toml (or its default).
    pub fn external_argv(&self, e: External) -> Vec<String> {
        let c = &self.config.commands;
        match e {
            External::Mixer => c.mixer.clone(),
            External::Network => c.network.clone(),
            External::Bluetooth => c.bluetooth.clone(),
        }
    }

    /// The external tool has returned: an error becomes a message, and the tab is read again
    /// because the tool may have changed anything.
    pub fn external_done(&mut self, e: External, r: Result<(), String>, now: Instant) {
        if let Err(msg) = r {
            let msg = format!("{}: {msg}", e.label());
            self.status.error = Some(msg.clone());
            self.toast(msg, true, now);
        }
        self.refresh_tab(now);
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

    /// Whether an action on `d` is still waiting for its answer (see the `pending` field).
    fn is_pending(&self, d: Domain) -> bool {
        self.pending.contains_key(&d)
    }

    /// Applies one answer from the worker: stores a read, or finishes an action.
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
        if let Some(kept) = keep {
            if let Some(i) = self.items().iter().position(|i| *i == kept) {
                self.focus = i;
            }
        }
        self.clamp_focus();
    }

    /// An action got its answer: a message, and a new read of the tab, except after a slider that
    /// succeeded (a failed slider is re-read too, which cancels the optimistic display).
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

    /// The toast an action's success shows; none for the ones that have nothing to say.
    fn success_message(&self, op: &Op) -> Option<String> {
        let onoff = |id: ToggleId, b: bool| format!("{}: {}", toggle_label(id), if b { "on" } else { "off" });
        Some(match op {
            Op::SetVolume(..) | Op::SetBrightness(_) | Op::TryDisplays(_) | Op::KeepDisplays | Op::RevertDisplays | Op::FailedRefresh => {
                return None;
            }
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
            Op::UnitUser(UnitAction::Restart, unit) => format!("Restart queued: {unit}"),
            Op::UnitUser(UnitAction::ResetFailed, unit) => format!("Cleared: {unit}"),
            Op::Copy { .. } => "Copied to the clipboard".to_string(),
        })
    }

    /// Shows `text` in the corner; at most three at once, the oldest goes first.
    fn toast(&mut self, text: String, error: bool, now: Instant) {
        self.toasts.push(Toast { text, error, until: now + TOAST_TTL });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    // ── Time ─────────────────────────────────────────────────────────

    /// The clock tick (the real loop calls it each turn): drops expired toasts, runs the Displays
    /// trial's countdown (`displays_tick`) and the journal's follow read (`journal_tick`), then,
    /// unless the Diag tab holds its refresh back (`diagnostics_refresh_held`, the journal view open),
    /// asks the worker to read the shown tab again once its period has passed (`TabSpec::refresh`).
    /// A tab with no period (Power, Theme) is never read on a tick.
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
        // relaunched at every tab period (1 to 5 s). It's re-read at most every 15 s, and on
        // entering the tab or after an action.
        let mut ds: Vec<Domain> = Vec::new();
        for d in self.tab.domains() {
            let read_now = if self.domain_failed(*d) {
                match self.last_read.get(d) {
                    Some(t) => now.duration_since(*t) >= FAILED_RETRY,
                    None => true,
                }
            } else {
                due
            };
            if read_now {
                ds.push(*d);
            }
        }
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

    /// Whether the last read of `d` failed. The health read (`Domain::Health`) never counts as
    /// failed.
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

    /// Asks for all of the shown tab's domains now (entering the tab, after an action).
    fn refresh_tab(&mut self, now: Instant) {
        self.push_refresh(self.tab.domains().to_vec(), now);
        self.last_refresh = now;
    }

    // ── Navigation ───────────────────────────────────────────────────

    /// Shows tab `t` (the focus goes back to the top and the search is cleared) and reads it.
    pub fn go_tab(&mut self, t: Tab, now: Instant) {
        if t != self.tab {
            if self.tab == Tab::Displays {
                // The user moved on: a profile still waiting is not applied behind their back.
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

    /// The focusable items of the shown tab, in drawing order: each tab's own file builds its list.
    pub fn items(&self) -> Vec<Item> {
        match self.tab {
            Tab::Home => home::items(),
            Tab::Sound => sound::items(self),
            Tab::Network => network::items(),
            Tab::Bluetooth => bluetooth::items(),
            Tab::Power => power::items(),
            Tab::Appearance => appearance::items(),
            Tab::Diagnostics => self.diag_items(),
            Tab::Displays => displays::items(self),
        }
    }

    /// The item under the focus, if the tab has any.
    pub fn focused(&self) -> Option<Item> {
        self.items().get(self.focus).cloned()
    }

    /// Moves the focus by `delta` items and stops at the first or the last.
    pub(crate) fn move_focus(&mut self, delta: i32) {
        let n = self.items().len() as i32;
        self.focus = if n == 0 { 0 } else { (self.focus as i32 + delta).clamp(0, n - 1) as usize };
    }

    /// Brings the focus back inside the list when the list has become shorter.
    pub(crate) fn clamp_focus(&mut self) {
        let n = self.items().len();
        if self.focus >= n {
            self.focus = n.saturating_sub(1);
        }
    }

    /// Whether `label` passes the search (everything does while the search is empty).
    pub fn matches_search(&self, label: &str) -> bool {
        self.search.text.is_empty() || fuzzy::score(&self.search.text, label).is_some()
    }

    // ── Commands ─────────────────────────────────────────────────────

    /// Does what a command says: a button, a palette entry or a key reaches it.
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

    /// Flips a toggle. The new state is shown at once and the action is queued; reads of that
    /// domain are then ignored until the action is answered (see `pending`).
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

    /// Sets a slider to `value` percent: shown at once, queued for the worker. Nothing is sent when
    /// the state is unknown or already equal, nor when the current value is above 100 % and `value`
    /// asks for more (it would be clamped to 100 and lower the sound). Brightness never goes below `brightness::MIN_PERCENT`
    /// (0 % would turn the screen off, which alpheus never does).
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

    /// Moves a slider by `delta` percent.
    pub fn nudge_slider(&mut self, id: SliderId, delta: i32) {
        if let Some(cur) = self.slider_value(id) {
            self.set_slider(id, i32::from(cur) + delta);
        }
    }

    /// Moves the energy profile one step along the machine's list and stops at its ends.
    fn cycle_profile(&mut self, dir: i32, now: Instant) {
        let Some(p) = self.data.profiles.ready() else { return };
        let Some(cur) = p.available.iter().position(|n| *n == p.active) else { return };
        let next = (cur as i32 + dir).clamp(0, p.available.len() as i32 - 1) as usize;
        if next != cur {
            let name = p.available[next].clone();
            self.run_command(Command::SetProfile(name), now);
        }
    }

    /// Closes a confirmation box; `yes` runs what it asked about.
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

    /// The palette entries that match what is typed, best first (all of them when nothing is typed).
    pub fn palette_matches(&self) -> Vec<(String, Command)> {
        let query = match &self.overlay {
            Overlay::Palette(p) => p.query.as_str(),
            _ => "",
        };
        let profiles = match self.data.profiles.ready() {
            Some(p) => p.available.clone(),
            None => Vec::new(),
        };
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

    /// The audio node `id`, an output or an input, once the audio data has loaded.
    fn node(&self, id: u32) -> Option<&AudioNode> {
        let audio = self.data.audio.ready()?;
        audio.sinks.iter().chain(&audio.sources).find(|n| n.id == id)
    }

    /// A slider's value as a `Loadable`, so the drawing can say loading or failed.
    pub fn slider_loadable(&self, id: SliderId) -> Loadable<u8> {
        match id {
            SliderId::Volume => self.data.volume.map(|v| v.percent),
            SliderId::Brightness => self.data.brightness.clone(),
            SliderId::Node(n) => match &self.data.audio {
                // The data is read but holds no such node (it left between two reads): loading.
                Loadable::Ready(_) => match self.node(n) {
                    Some(x) => Loadable::Ready(x.volume.percent),
                    None => Loadable::Loading,
                },
                other => other.map(|_| 0),
            },
        }
    }

    /// A slider's value, when it is known.
    pub fn slider_value(&self, id: SliderId) -> Option<u8> {
        self.slider_loadable(id).ready().copied()
    }

    /// Whether the slider's node is muted (brightness never is).
    pub fn slider_muted(&self, id: SliderId) -> bool {
        match id {
            SliderId::Volume => self.data.volume.ready().is_some_and(|v| v.muted),
            SliderId::Brightness => false,
            SliderId::Node(n) => self.node(n).is_some_and(|x| x.volume.muted),
        }
    }

    /// The slider's row label; a node's carries a `●` when it is the default.
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

    /// A toggle's state as a `Loadable`.
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
