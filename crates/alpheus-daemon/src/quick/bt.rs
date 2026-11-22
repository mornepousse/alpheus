//! The quick panel's Bluetooth (quick Bluetooth spec §§ 1–2), pure: the row's glyph and value, the
//! detail with the paired devices then the ones nearby (at most ROWS_MAX), its selection kept by
//! object path, its keys and clicks, and the BlueZ calls they ask for (`BtOrder`), which the wiring
//! hands to the acting connection (`sources/dbus/bt_act.rs`). The view comes from the D-Bus thread
//! (`sources/dbus/bt.rs`) only while the panel is open. Device names, aliases, addresses and object
//! paths never reach a log from here: a log line names the action's word and the D-Bus error.
//!
//! One action at a time (spec § 2): a power toggle, a connect, a disconnect, a pairing (pair, trust,
//! connect), a forget; each call with its own CALL_DEADLINE, suspended while a pairing code shows.
//! The agent's requests (spec § 3) are answered for the device being paired only.

use crate::actions::Button;
use crate::grid::{Grid, Style, text_width, truncate};
use crate::keys::Key;
pub use crate::quick::FAILED_SHOWN;
use crate::quick::wifi::VALUE_CELLS;
use crate::quick::{FAILED_TAG, Line, Question, QuickAction, RUNNING_TAG, moved, nav, note_row, run, text_row};
use crate::segments::{BLUETOOTH, BLUETOOTH_CONNECTED};
use crate::session::{self, Activation};
use crate::sources::dbus::bt_act::{ActCall, ActError, AgentId, AgentReply, AgentRequest};
use crate::system::{BtAdapter, BtEntry, BtKind, BtScope, BtState, BtView};
use crate::text;
use std::time::{Duration, Instant};

/// The most devices the detail lists, paired and nearby together (quick Bluetooth spec § 2).
pub const ROWS_MAX: usize = 32;
/// Material Design (Nerd Font) bluetooth-off; on and connected are the bar module's own
/// (`segments::BLUETOOTH`, `segments::BLUETOOTH_CONNECTED`): the row draws as the module.
pub const BT_OFF: char = '\u{f00b2}';
/// A device's glyph by its kind: headphones, keyboard, mouse, speaker, cellphone, and the
/// bluetooth-connect one for anything else.
pub const HEADSET: char = '\u{f02cb}';
pub const KEYBOARD: char = '\u{f030c}';
pub const MOUSE: char = '\u{f037d}';
pub const SPEAKER: char = '\u{f04c3}';
pub const PHONE: char = '\u{f03f2}';
pub const GENERIC: char = BLUETOOTH_CONNECTED;
/// The detail's note with the adapter off, and the nearby devices' heading.
pub const OFF_NOTE: &str = "Bluetooth is off";
pub const NEARBY: &str = "Nearby";
/// A paired, unconnected device's tag.
const PAIRED_TAG: &str = "paired";
/// The forget question's tag (spec § 2).
const FORGET_TAG: &str = "Forget? ⏎";
/// How long each call of an action may wait for its answer (spec § 2).
pub const CALL_DEADLINE: Duration = Duration::from_secs(30);
/// BlueZ's answer to a `Connect` on a device already connected: a success.
const ALREADY_CONNECTED: &str = "org.bluez.Error.AlreadyConnected";
/// The object went (a device removed meanwhile): the action ends silently.
const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";

/// What the panel asks of BlueZ: a call on the acting connection, or the agent's answer to one of
/// BlueZ's requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtOrder {
    Call(ActCall),
    Reply(AgentId, AgentReply),
    /// Stop waiting for this call's answer (the acting connection's `forget`): nothing goes on the bus.
    Forget(ActCall),
}

/// Where an action is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Power,
    Connect,
    Disconnect,
    Pair,
    /// `Trusted` set after a pairing.
    Trust,
    /// The `Connect` after a pairing's trust.
    ConnectAfterPair,
    Forget,
}

/// The one action running: its device (None: the adapter's power), its step, the call waiting for
/// its answer, and until when (None: suspended while a code shows, or not armed yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub device: Option<String>,
    pub step: Step,
    pub call: ActCall,
    pub deadline: Option<Instant>,
}

/// A pairing's code (spec § 3): shown to be typed on the device, or asked to be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    Shown { device: String, text: String },
    Question { device: String, passkey: u32, id: AgentId },
}

/// The panel's Bluetooth: dropped whole with the panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bt {
    /// What the D-Bus thread last read; None: nothing yet, or BlueZ gone.
    pub view: Option<BtView>,
    /// The detail shows in place of the rows.
    pub open: bool,
    /// The selected device by object path; None: the header.
    pub selected: Option<String>,
    /// Discovery was asked for during this open (and the adapter has stayed powered since).
    pub discovering: bool,
    /// `rfkill unblock bluetooth` ran: power the adapter on once BlueZ reports it unblocked.
    pub power_after_unblock: bool,
    /// The one action running.
    pub action: Option<Action>,
    /// The device whose action failed, until when `failed` shows.
    pub failed: Option<(String, Instant)>,
    /// The running pairing's code.
    pub code: Option<Code>,
}

/// The adapter's power set to `on`, as a call.
fn powered(a: &BtAdapter, on: bool) -> ActCall {
    ActCall::Powered { adapter: a.path.clone(), on }
}

fn kind_glyph(kind: BtKind) -> char {
    match kind {
        BtKind::Headset => HEADSET,
        BtKind::Keyboard => KEYBOARD,
        BtKind::Mouse => MOUSE,
        BtKind::Speaker => SPEAKER,
        BtKind::Phone => PHONE,
        BtKind::Other => GENERIC,
    }
}

/// Orders as an action: Redraw when there are none.
fn orders(v: Vec<BtOrder>) -> QuickAction {
    if v.is_empty() { QuickAction::Redraw } else { QuickAction::Bluetooth(v) }
}

impl Bt {
    /// The row shows: BlueZ has an adapter (`Absent`, or the bar's state not read: hidden).
    pub fn shows(bt: Option<&BtState>) -> bool {
        matches!(bt, Some(BtState::Off | BtState::On { .. }))
    }

    fn adapter(&self) -> Option<&BtAdapter> {
        self.view.as_ref()?.adapter.as_ref()
    }

    fn powered(&self) -> bool {
        self.adapter().is_some_and(|a| a.powered)
    }

    pub fn glyph(&self, bt: &BtState) -> char {
        match bt {
            BtState::On { devices } if !devices.is_empty() => BLUETOOTH_CONNECTED,
            BtState::On { .. } => BLUETOOTH,
            _ => BT_OFF,
        }
    }

    /// The row's value (quick Bluetooth spec § 1): the first connected device's name (the bar's
    /// order), else `on`, `off`, or `blocked` when rfkill holds the adapter off.
    pub fn value(&self, bt: &BtState) -> String {
        match bt {
            BtState::On { devices } => match devices.first() {
                Some(d) => {
                    let name = text::clean(&d.name);
                    if name.trim().is_empty() { "?".into() } else { truncate(&name, VALUE_CELLS) }
                }
                None => "on".into(),
            },
            _ if self.adapter().is_some_and(|a| a.blocked) => "blocked".into(),
            _ => "off".into(),
        }
    }

    /// The detail's devices (quick Bluetooth spec § 2): none unless the adapter is powered; the
    /// paired ones, connected first, then by name (any case), then the ones nearby in the reader's
    /// order; at most ROWS_MAX in all.
    pub fn devices(&self) -> Vec<&BtEntry> {
        let Some(v) = self.view.as_ref().filter(|_| self.powered()) else { return Vec::new() };
        let mut paired: Vec<&BtEntry> = v.paired.iter().collect();
        paired.sort_by_cached_key(|e| (!e.connected, e.label.to_lowercase(), e.path.clone()));
        paired.into_iter().chain(&v.nearby).take(ROWS_MAX).collect()
    }

    /// How many of `devices()` are paired: they come first.
    fn paired_listed(&self) -> usize {
        self.view.as_ref().filter(|_| self.powered()).map_or(0, |v| v.paired.len().min(ROWS_MAX))
    }

    /// Enter, →, a click on the row: the detail, the first device selected (the header without
    /// one); discovery starts when the adapter is powered.
    pub fn open_detail(&mut self) -> QuickAction {
        self.open = true;
        self.selected = self.devices().first().map(|d| d.path.clone());
        match self.adapter().filter(|a| a.powered).map(|a| a.path.clone()) {
            Some(adapter) => {
                self.discovering = true;
                QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::StartDiscovery { adapter })])
            }
            None => QuickAction::Redraw,
        }
    }

    /// Back to the rows (`leaving`'s orders).
    pub fn close_detail(&mut self) -> Vec<BtOrder> {
        self.open = false;
        self.selected = None;
        self.leaving()
    }

    /// Leaving the detail or closing the panel, in order: a pending code question rejected, a
    /// pairing cancelled and its call forgotten (the code could no longer show), discovery stopped
    /// if this open started it. A connect, disconnect, trust or forget outlives the detail.
    pub fn leaving(&mut self) -> Vec<BtOrder> {
        let mut v = Vec::new();
        if let Some(Code::Question { id, .. }) = self.code.take() {
            v.push(BtOrder::Reply(id, AgentReply::Rejected));
        }
        if let Some(a) = self.action.take_if(|a| a.step == Step::Pair) {
            if let Some(device) = a.device {
                v.push(BtOrder::Call(ActCall::CancelPairing { device }));
            }
            v.push(BtOrder::Forget(a.call));
        }
        if std::mem::take(&mut self.discovering)
            && let Some(a) = self.adapter()
        {
            v.push(BtOrder::Call(ActCall::StopDiscovery { adapter: a.path.clone() }));
        }
        v
    }

    /// What the D-Bus thread is to read: the detail's lists while it is open (or while an action on
    /// a device runs: a row reading would look like that device gone), the adapter while the row
    /// shows, nothing otherwise.
    pub fn scope(&self, row_shown: bool) -> BtScope {
        if self.open || self.action.as_ref().is_some_and(|a| a.device.is_some()) {
            BtScope::Detail
        } else if row_shown {
            BtScope::Row
        } else {
            BtScope::Closed
        }
    }

    /// The acting connection is wanted: the detail is open, an action runs or a code shows.
    pub fn wants_connection(&self) -> bool {
        self.open || self.action.is_some() || self.code.is_some()
    }

    /// The action and its code end: a pending code question rejected; its call forgotten when its
    /// answer is still awaited (`forget`); `failed` on its device from `failed_at`, if given.
    fn finish(&mut self, forget: bool, failed_at: Option<Instant>) -> Vec<BtOrder> {
        let mut v = self.end_code();
        if let Some(a) = self.action.take() {
            if let (Some(at), Some(d)) = (failed_at, a.device) {
                self.failed = Some((d, at + FAILED_SHOWN));
            }
            if forget {
                v.push(BtOrder::Forget(a.call));
            }
        }
        v
    }

    /// The code goes; a pending code question is rejected (the order to send).
    fn end_code(&mut self) -> Vec<BtOrder> {
        match self.code.take() {
            Some(Code::Question { id, .. }) => vec![BtOrder::Reply(id, AgentReply::Rejected)],
            _ => Vec::new(),
        }
    }

    /// Starts an action on `call`, armed at `now` (None: armed by `arm`); the order sending it.
    fn start(&mut self, device: Option<String>, step: Step, call: ActCall, now: Option<Instant>) -> BtOrder {
        self.action = Some(Action { device, step, call: call.clone(), deadline: now.map(|n| n + CALL_DEADLINE) });
        BtOrder::Call(call)
    }

    /// A just-started action with no deadline and no code gets its CALL_DEADLINE from `now` (the
    /// power toggle keeps no clock).
    pub(crate) fn arm(&mut self, now: Instant) {
        if self.code.is_none()
            && let Some(a) = self.action.as_mut().filter(|a| a.deadline.is_none())
        {
            a.deadline = Some(now + CALL_DEADLINE);
        }
    }

    /// Enter (or a left press) on the selected device, no action running (spec § 2): a connected one
    /// disconnects, a paired one connects, a nearby one pairs.
    pub fn enter(&mut self, now: Instant) -> QuickAction {
        if self.action.is_some() {
            return QuickAction::Nothing;
        }
        let devices = self.devices();
        let Some(d) = self.selected.as_ref().and_then(|p| devices.iter().find(|d| &d.path == p)) else {
            return QuickAction::Nothing;
        };
        let device = d.path.clone();
        let (step, call) = if d.connected {
            (Step::Disconnect, ActCall::Disconnect { device: device.clone() })
        } else if d.paired {
            (Step::Connect, ActCall::Connect { device: device.clone() })
        } else {
            (Step::Pair, ActCall::Pair { device: device.clone() })
        };
        QuickAction::Bluetooth(vec![self.start(Some(device), step, call, Some(now))])
    }

    /// The selected device's forget question (Delete, a right press): only on a paired device, with
    /// no action running.
    fn ask_forget(&mut self, confirm: &mut Option<(Question, Instant)>, now: Instant) -> QuickAction {
        let Some(p) = self.selected.clone() else { return QuickAction::Nothing };
        let paired = self.view.as_ref().is_some_and(|v| v.paired.iter().any(|e| e.path == p));
        if self.action.is_some() || !paired {
            return QuickAction::Nothing;
        }
        *confirm = Some((Question::Forget(p), now + session::CONFIRM));
        QuickAction::Redraw
    }

    /// Enter, or a left press, on the selected device: the forget question's second activation when
    /// it asks about this device (from CONFIRM_AFTER to CONFIRM), else `enter`.
    fn activate(&mut self, confirm: &mut Option<(Question, Instant)>, now: Instant) -> QuickAction {
        let asked = matches!((confirm.as_ref(), &self.selected), (Some((Question::Forget(q), until)), Some(p)) if q == p && now < *until);
        if !asked {
            *confirm = None;
            return self.enter(now);
        }
        let (Some(device), Some(adapter)) = (self.selected.clone(), self.adapter().map(|a| a.path.clone())) else {
            return QuickAction::Nothing;
        };
        match session::ask(confirm, Question::Forget(device.clone()), now) {
            Activation::Wait => QuickAction::Nothing,
            Activation::Ask => QuickAction::Redraw,
            Activation::Run if self.action.is_some() => QuickAction::Redraw,
            Activation::Run => {
                let call = ActCall::RemoveDevice { adapter, device: device.clone() };
                QuickAction::Bluetooth(vec![self.start(Some(device), Step::Forget, call, Some(now))])
            }
        }
    }

    /// The answer to `call` (the acting connection's `BtEvent::Answer`): the action's next step,
    /// its end, or its failure; the log line to write, if any (never a device in it).
    pub fn answered(&mut self, call: &ActCall, result: Result<(), ActError>, now: Instant) -> (QuickAction, Option<String>) {
        let Some(a) = self.action.as_ref().filter(|a| &a.call == call) else {
            // Not the action's: discovery failing is logged once, never retried.
            return match (call, result) {
                (ActCall::StartDiscovery { .. }, Err(ActError::Remote(why))) => {
                    (QuickAction::Nothing, Some(format!("quick: bluetooth {} failed ({why})", call.word())))
                }
                _ => (QuickAction::Nothing, None),
            };
        };
        let step = a.step;
        let success = match &result {
            Ok(()) => true,
            Err(ActError::Remote(why)) => why == ALREADY_CONNECTED && matches!(step, Step::Connect | Step::ConnectAfterPair),
            Err(ActError::Gone) => false,
        };
        if !success {
            return match result {
                Err(ActError::Remote(why)) if why != UNKNOWN_OBJECT => {
                    let v = self.finish(false, Some(now));
                    (orders(v), Some(format!("quick: bluetooth {} failed ({why})", call.word())))
                }
                _ => (orders(self.finish(false, None)), None),
            };
        }
        let device = a.device.clone();
        let mut v = Vec::new();
        let next = match (step, device) {
            (Step::Pair, Some(d)) => {
                v = self.end_code();
                Some((Step::Trust, ActCall::Trusted { device: d }))
            }
            (Step::Trust, Some(d)) if !self.connected(&d) => Some((Step::ConnectAfterPair, ActCall::Connect { device: d })),
            _ => None,
        };
        match next {
            Some((step, call)) => {
                let device = self.action.as_ref().and_then(|a| a.device.clone());
                v.push(self.start(device, step, call, Some(now)));
                (QuickAction::Bluetooth(v), None)
            }
            None => {
                self.action = None;
                (QuickAction::Redraw, None)
            }
        }
    }

    /// The view shows `path` connected.
    fn connected(&self, path: &str) -> bool {
        self.view.as_ref().is_some_and(|v| v.paired.iter().chain(&v.nearby).any(|e| e.path == path && e.connected))
    }

    /// The device being paired: the action's, while its step is `Pair`.
    fn pairing(&self) -> Option<&str> {
        self.action.as_ref().filter(|a| a.step == Step::Pair).and_then(|a| a.device.as_deref())
    }

    /// `code` replaces the one showing: a pending question is rejected first. The deadline waits.
    fn show(&mut self, code: Code, v: &mut Vec<BtOrder>) {
        if let Some(Code::Question { id, .. }) = self.code.take() {
            v.push(BtOrder::Reply(id, AgentReply::Rejected));
        }
        self.code = Some(code);
        if let Some(a) = self.action.as_mut() {
            a.deadline = None;
        }
    }

    /// One of BlueZ's agent requests (spec § 3), for the device being paired only: a code shown, a
    /// code question, a code to type in (refused: `failed`), BlueZ cancelling. Anything else, or for
    /// any other device, is rejected.
    pub fn agent(&mut self, id: AgentId, request: AgentRequest, now: Instant) -> QuickAction {
        use AgentRequest::*;
        let reject = || QuickAction::Bluetooth(vec![BtOrder::Reply(id, AgentReply::Rejected)]);
        let mut v = Vec::new();
        match request {
            Cancel | Release => {
                // BlueZ's own (acting.rs answered it): the code goes, the deadline runs again.
                let shown = self.code.take().is_some();
                if let Some(a) = self.action.as_mut().filter(|a| shown || a.deadline.is_none()) {
                    a.deadline = Some(now + CALL_DEADLINE);
                }
                QuickAction::Redraw
            }
            DisplayPasskey { device, passkey } if self.pairing() == Some(device.as_str()) => {
                self.show(Code::Shown { device, text: Bt::format_code(passkey) }, &mut v);
                v.push(BtOrder::Reply(id, AgentReply::Ok));
                QuickAction::Bluetooth(v)
            }
            DisplayPinCode { device, pincode } if self.pairing() == Some(device.as_str()) => {
                // Cleaned like a label, whatever handed it (the connection already cleans it).
                self.show(Code::Shown { device, text: text::clean(&pincode) }, &mut v);
                v.push(BtOrder::Reply(id, AgentReply::Ok));
                QuickAction::Bluetooth(v)
            }
            RequestConfirmation { device, passkey } if self.pairing() == Some(device.as_str()) => {
                self.show(Code::Question { device, passkey, id }, &mut v);
                orders(v)
            }
            RequestPinCode { device } | RequestPasskey { device } if self.pairing() == Some(device.as_str()) => {
                v.push(BtOrder::Reply(id, AgentReply::Rejected));
                v.extend(self.finish(true, Some(now)));
                QuickAction::Bluetooth(v)
            }
            _ => reject(),
        }
    }

    /// The six digits of a passkey, zero-padded, grouped 3+3: `482 913`.
    pub fn format_code(n: u32) -> String {
        format!("{:03} {:03}", n / 1000, n % 1000)
    }

    /// At `now`: the action's deadline passed (its pairing cancelled, its call forgotten, `failed`,
    /// a `(timeout)` log line), `failed` gone; and whether to repaint.
    pub fn due(&mut self, now: Instant) -> (Vec<BtOrder>, Option<String>, bool) {
        let redraw = session::expire(&mut self.failed, now);
        let Some(a) = self.action.as_ref().filter(|a| a.deadline.is_some_and(|d| d <= now)) else {
            return (Vec::new(), None, redraw);
        };
        let log = format!("quick: bluetooth {} failed (timeout)", a.call.word());
        let mut v = Vec::new();
        if let (Step::Pair, Some(device)) = (a.step, a.device.clone()) {
            v.push(BtOrder::Call(ActCall::CancelPairing { device }));
        }
        v.extend(self.finish(true, Some(now)));
        (v, Some(log), true)
    }

    /// The earliest of the action's deadline and `failed`'s end; None: nothing to wait for.
    pub fn deadline(&self) -> Option<Instant> {
        [self.action.as_ref().and_then(|a| a.deadline), self.failed.as_ref().map(|(_, until)| *until)].into_iter().flatten().min()
    }

    /// The D-Bus thread's view, while the panel is open. None (BlueZ lost) closes the detail, its
    /// orders dropped (the acting connection goes with BlueZ). An action whose device left both
    /// lists, or whose adapter is off, ends silently. An adapter just unblocked after `rfkill` is
    /// powered on (once no action runs); an open detail on a powered adapter discovers, once per
    /// open (power back after an outage starts it again); the first view of an open selects the
    /// first device.
    pub fn set_view(&mut self, view: Option<BtView>, now: Instant) -> QuickAction {
        let first = self.view.is_none();
        self.view = view;
        if self.view.is_none() {
            if self.open {
                let _dropped = self.close_detail();
            }
            self.discovering = false;
            return QuickAction::Redraw;
        }
        let mut v = Vec::new();
        let gone =
            self.action.as_ref().and_then(|a| a.device.as_deref()).is_some_and(|d| {
                !self.powered() || self.view.as_ref().is_none_or(|w| !w.paired.iter().chain(&w.nearby).any(|e| e.path == d))
            });
        if gone {
            v.extend(self.finish(true, None));
        }
        match self.adapter().cloned() {
            Some(a) => {
                if !a.blocked && self.action.is_none() && std::mem::take(&mut self.power_after_unblock) {
                    v.push(self.start(None, Step::Power, powered(&a, true), Some(now)));
                }
                if !a.powered {
                    // BlueZ ended it: power back starts it again.
                    self.discovering = false;
                } else if self.open && !self.discovering {
                    self.discovering = true;
                    v.push(BtOrder::Call(ActCall::StartDiscovery { adapter: a.path.clone() }));
                }
            }
            None => self.discovering = false,
        }
        if self.open && first && self.selected.is_none() {
            self.selected = self.devices().first().map(|d| d.path.clone());
        }
        orders(v)
    }

    /// Space on the row (quick Bluetooth spec § 1): the adapter's power the other way; with rfkill
    /// holding it off, `rfkill unblock bluetooth` first. Nothing without an adapter read.
    pub fn power(&mut self, bt: &BtState) -> QuickAction {
        if *bt == BtState::Absent {
            return QuickAction::Nothing;
        }
        self.toggle()
    }

    /// The view's adapter the other way (the row's and the header's Space), an action until its
    /// answer (`arm` gives it its deadline); nothing while another action runs.
    fn toggle(&mut self) -> QuickAction {
        let Some(a) = self.adapter().cloned() else { return QuickAction::Nothing };
        if self.action.is_some() {
            return QuickAction::Nothing;
        }
        if a.blocked {
            self.power_after_unblock = true;
            return run(&["rfkill", "unblock", "bluetooth"], "bluetooth unblock");
        }
        QuickAction::Bluetooth(vec![self.start(None, Step::Power, powered(&a, !a.powered), None)])
    }

    /// Before each use: a selected device no longer listed gives the selection to the header (it
    /// never jumps onto another device).
    pub fn fit(&mut self) {
        if let Some(p) = &self.selected
            && !self.devices().iter().any(|d| &d.path == p)
        {
            self.selected = None;
        }
    }

    /// The detail's lines: its header, a separator, then (once a view is read) `Bluetooth is off`,
    /// or the paired devices, the `Nearby` heading and the nearby ones.
    pub fn lines(&self) -> Vec<Line> {
        let mut v = vec![Line::BtHeader, Line::Separator];
        if self.view.is_none() {
            return v;
        }
        if !self.powered() {
            v.push(Line::BtNote(OFF_NOTE));
            return v;
        }
        let devices = self.devices();
        let paired = self.paired_listed();
        let shown = match &self.code {
            Some(Code::Shown { device, .. }) => devices.iter().position(|d| &d.path == device),
            _ => None,
        };
        let device = |v: &mut Vec<Line>, i: usize| {
            v.push(Line::BtDevice(i));
            if shown == Some(i) {
                v.push(Line::BtCode);
            }
        };
        (0..paired).for_each(|i| device(&mut v, i));
        v.push(Line::BtNearby);
        (paired..devices.len()).for_each(|i| device(&mut v, i));
        v
    }

    /// Whether `line` is selected; `devices` is `devices()`, computed once per pass.
    pub fn is_selected(&self, line: Line, devices: &[&BtEntry]) -> bool {
        match line {
            Line::BtHeader => self.selected.is_none(),
            Line::BtDevice(i) => devices.get(i).is_some_and(|d| self.selected.as_deref() == Some(d.path.as_str())),
            _ => false,
        }
    }

    /// A code question first takes the key: Enter replies OK, any other key rejects it and ends the
    /// pairing silently. Otherwise: Escape or ← back to the rows; ↑ ↓ Home End over the header and
    /// the devices, never wrapping; Space on the header powers the adapter; Enter acts on the device
    /// (or confirms its forget question); Delete asks to forget it. Any key but Enter drops the
    /// question.
    pub fn key(&mut self, k: Key, confirm: &mut Option<(Question, Instant)>, now: Instant) -> QuickAction {
        if let Some(Code::Question { id, .. }) = self.code {
            if k == Key::Enter {
                self.code = None;
                if let Some(a) = self.action.as_mut() {
                    a.deadline = Some(now + CALL_DEADLINE);
                }
                return QuickAction::Bluetooth(vec![BtOrder::Reply(id, AgentReply::Ok)]);
            }
            let mut v = vec![BtOrder::Reply(id, AgentReply::Rejected)];
            self.code = None;
            v.extend(self.finish(true, None));
            return QuickAction::Bluetooth(v);
        }
        if k != Key::Enter {
            *confirm = None;
        }
        let devices: Vec<String> = self.devices().iter().map(|d| d.path.clone()).collect();
        let at = self.selected.as_ref().and_then(|p| devices.iter().position(|d| d == p)).map_or(0, |i| i + 1);
        match k {
            Key::Escape | Key::Left => return orders(self.close_detail()),
            Key::Char(' ') if self.selected.is_none() => {
                let a = self.toggle();
                self.arm(now);
                return a;
            }
            Key::Enter => return self.activate(confirm, now),
            Key::Delete => return self.ask_forget(confirm, now),
            _ => {}
        }
        // The header first: one more entry than the devices.
        let Some(to) = nav(k, Some(at), devices.len() + 1) else { return QuickAction::Nothing };
        moved(&mut self.selected, to.checked_sub(1).and_then(|i| devices.get(i)).cloned())
    }

    /// A press on a detail line: a left one on the header goes back; on a device it selects it then
    /// is Enter (the forget question's second activation there); a right one selects it and asks to
    /// forget it. While a code question shows, only the header answers. The question goes with a
    /// press anywhere but its own device.
    pub fn press(&mut self, line: Line, b: Button, confirm: &mut Option<(Question, Instant)>, now: Instant) -> QuickAction {
        let device = match line {
            Line::BtDevice(i) => self.devices().get(i).map(|d| d.path.clone()),
            _ => None,
        };
        if !matches!((confirm.as_ref(), &device), (Some((Question::Forget(q), _)), Some(p)) if q == p) {
            *confirm = None;
        }
        if matches!(self.code, Some(Code::Question { .. })) && line != Line::BtHeader {
            return QuickAction::Nothing;
        }
        match (line, b, device) {
            (Line::BtHeader, Button::Left, _) => orders(self.close_detail()),
            (Line::BtDevice(_), Button::Left, Some(p)) => {
                let on_it = moved(&mut self.selected, Some(p));
                match self.activate(confirm, now) {
                    QuickAction::Nothing => on_it,
                    a => a,
                }
            }
            (Line::BtDevice(_), Button::Right, Some(p)) => {
                let on_it = moved(&mut self.selected, Some(p));
                if confirm.is_some() {
                    // Already asked about this device: the question stays as it is.
                    return on_it;
                }
                match self.ask_forget(confirm, now) {
                    QuickAction::Nothing => on_it,
                    a => a,
                }
            }
            _ => QuickAction::Nothing,
        }
    }

    /// One detail line into `g` (`WIDTH + 2` wide, a pad cell each side); `devices` is `devices()`,
    /// computed once per pass.
    pub fn draw(&self, g: &mut Grid, line: Line, devices: &[&BtEntry], confirm: Option<&(Question, Instant)>, selected: bool) {
        let end = g.width();
        match line {
            Line::BtHeader => {
                let value = match &self.view {
                    None => "",
                    Some(_) if self.powered() => "on",
                    Some(_) => "off",
                };
                text_row(g, None, "‹ Bluetooth", value, Style::MUTED, selected)
            }
            Line::BtCode => {
                if let Some(Code::Shown { text, .. }) = &self.code {
                    g.write(5, end.saturating_sub(1), &format!("Type {text} then ⏎"), Style::ACCENT);
                }
            }
            Line::BtNearby => {
                g.write(1, end.saturating_sub(1), NEARBY, Style::MUTED);
            }
            Line::BtNote(note) => note_row(g, note),
            Line::BtDevice(i) => {
                if let Some(d) = devices.get(i) {
                    let (tag, style) = self.tag(d, confirm);
                    device_row(g, d, &tag, style, selected);
                }
            }
            _ => {}
        }
    }
}

impl Bt {
    /// A device's tag, the first that applies (spec §§ 2–3): its forget question, its code
    /// question, its action running, its failure, paired.
    fn tag(&self, d: &BtEntry, confirm: Option<&(Question, Instant)>) -> (String, Style) {
        let is = |p: &str| p == d.path;
        if matches!(confirm, Some((Question::Forget(p), _)) if is(p)) {
            return (FORGET_TAG.into(), Style::WARN);
        }
        if let Some(Code::Question { device, passkey, .. }) = &self.code
            && is(device)
        {
            return (format!("Code {}? ⏎", Bt::format_code(*passkey)), Style::ACCENT);
        }
        if self.action.as_ref().and_then(|a| a.device.as_deref()).is_some_and(is) {
            return (RUNNING_TAG.into(), Style::MUTED);
        }
        if self.failed.as_ref().is_some_and(|(p, _)| is(p)) {
            return (FAILED_TAG.into(), Style::WARN);
        }
        if d.paired && !d.connected { (PAIRED_TAG.into(), Style::MUTED) } else { (String::new(), Style::MUTED) }
    }
}

/// "● 󰋋 CasqueBT" / "  󰌌 ClavierBT      paired" (quick Bluetooth spec § 2): ● on a connected one in
/// the accent, the kind's glyph, the label cut to fit (`?` when empty), the tag right-aligned one
/// cell from the end. Selected: the band across the whole row.
fn device_row(g: &mut Grid, d: &BtEntry, tag: &str, tag_style: Style, selected: bool) {
    let end = g.width();
    let room = match text_width(tag) {
        0 => 1,
        w => w + 2,
    };
    let label = if d.label.trim().is_empty() { "?" } else { d.label.as_str() };
    let label = truncate(label, end.saturating_sub(5).saturating_sub(room));
    let text = format!("{} {label}", kind_glyph(d.kind));
    let mark = if d.connected { '●' } else { ' ' };
    text_row(g, Some(mark), &text, tag, tag_style, selected);
}

#[cfg(test)]
mod tests;
