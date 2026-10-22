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
use crate::list;
use crate::quick::{Line, Question, QuickAction, run, text_row};
use crate::quick_wifi::VALUE_CELLS;
use crate::session::{self, Activation};
use crate::sources::dbus::bt_act::{ActCall, ActError, AgentId, AgentReply, AgentRequest};
use crate::system::{BtAdapter, BtEntry, BtKind, BtScope, BtState, BtView};
use crate::text;
use std::time::{Duration, Instant};

/// The most devices the detail lists, paired and nearby together (quick Bluetooth spec § 2).
pub const ROWS_MAX: usize = 32;
/// Material Design (Nerd Font): bluetooth, bluetooth-connect, bluetooth-off.
pub const BT_ON: char = '\u{f00af}';
pub const BT_CONNECTED: char = '\u{f00b1}';
pub const BT_OFF: char = '\u{f00b2}';
/// A device's glyph by its kind: headphones, keyboard, mouse, speaker, cellphone, and the
/// bluetooth-connect one for anything else.
pub const HEADSET: char = '\u{f02cb}';
pub const KEYBOARD: char = '\u{f030c}';
pub const MOUSE: char = '\u{f037d}';
pub const SPEAKER: char = '\u{f04c3}';
pub const PHONE: char = '\u{f03f2}';
pub const GENERIC: char = '\u{f00b1}';
/// The detail's note with the adapter off, and the nearby devices' heading.
pub const OFF_NOTE: &str = "Bluetooth is off";
pub const NEARBY: &str = "Nearby";
/// A paired, unconnected device's tag.
const PAIRED_TAG: &str = "paired";
/// The tags of an action running, a failure, and the forget question (spec § 2).
const RUNNING_TAG: &str = "…";
const FAILED_TAG: &str = "failed";
const FORGET_TAG: &str = "Forget? ⏎";
/// How long each call of an action may wait for its answer (spec § 2).
pub const CALL_DEADLINE: Duration = Duration::from_secs(30);
/// How long `failed` shows after a failure.
pub const FAILED_SHOWN: Duration = Duration::from_secs(4);
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
            BtState::On { devices } if !devices.is_empty() => BT_CONNECTED,
            BtState::On { .. } => BT_ON,
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
        let mut redraw = false;
        if self.failed.as_ref().is_some_and(|(_, until)| *until <= now) {
            self.failed = None;
            redraw = true;
        }
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
        let last = devices.len();
        let to = match k {
            Key::Escape | Key::Left => return orders(self.close_detail()),
            Key::Char(' ') if self.selected.is_none() => {
                let a = self.toggle();
                self.arm(now);
                return a;
            }
            Key::Enter => return self.activate(confirm, now),
            Key::Delete => return self.ask_forget(confirm, now),
            Key::Up | Key::CtrlP => at.saturating_sub(1),
            Key::Down | Key::CtrlN => (at + 1).min(last),
            Key::Home => 0,
            Key::End => last,
            _ => return QuickAction::Nothing,
        };
        self.select(to.checked_sub(1).and_then(|i| devices.get(i)).cloned())
    }

    /// The selection on `to`: Redraw when it moved.
    fn select(&mut self, to: Option<String>) -> QuickAction {
        if to == self.selected {
            return QuickAction::Nothing;
        }
        self.selected = to;
        QuickAction::Redraw
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
                let moved = self.select(Some(p));
                match self.activate(confirm, now) {
                    QuickAction::Nothing => moved,
                    a => a,
                }
            }
            (Line::BtDevice(_), Button::Right, Some(p)) => {
                let moved = self.select(Some(p));
                if confirm.is_some() {
                    // Already asked about this device: the question stays as it is.
                    return moved;
                }
                match self.ask_forget(confirm, now) {
                    QuickAction::Nothing => moved,
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
            Line::BtNote(note) => {
                g.write(3, end.saturating_sub(1), note, Style::MUTED);
            }
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
    let tag_width = text_width(tag);
    let room = if tag_width > 0 { tag_width + 2 } else { 1 };
    let label = if d.label.trim().is_empty() { "?" } else { d.label.as_str() };
    let label = truncate(label, end.saturating_sub(5).saturating_sub(room));
    let text = format!("{} {label}", kind_glyph(d.kind));
    let mark = if d.connected { '●' } else { ' ' };
    list::row(
        g,
        0,
        end,
        &list::Row { icon: Some(mark), icon_style: Style::ACCENT, text: &text, right: "", style: Style::NORMAL, selected },
    );
    if tag_width > 0 {
        g.write(end.saturating_sub(tag_width + 1), end, tag, if selected { Style::SELECTED } else { tag_style });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quick::{WIDTH, argv};
    use crate::session::{self, CONFIRM, CONFIRM_AFTER};
    use crate::system::BtDevice;

    const ADAPTER: &str = "/org/bluez/hci0";

    fn path(n: u8) -> String {
        format!("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_{n:02X}")
    }

    fn entry(n: u8, label: &str, kind: BtKind, paired: bool, connected: bool) -> BtEntry {
        BtEntry { path: path(n), label: label.into(), kind, paired, trusted: paired, connected }
    }

    fn view(powered: bool, paired: Vec<BtEntry>, nearby: Vec<BtEntry>) -> BtView {
        BtView { adapter: Some(BtAdapter { path: ADAPTER.into(), powered, blocked: false }), paired, nearby }
    }

    fn blocked() -> BtView {
        BtView { adapter: Some(BtAdapter { path: ADAPTER.into(), powered: false, blocked: true }), ..BtView::default() }
    }

    fn paired() -> Vec<BtEntry> {
        vec![
            entry(1, "CasqueBT", BtKind::Headset, true, true),
            entry(2, "ClavierBT", BtKind::Keyboard, true, false),
            entry(5, "baffle-BT", BtKind::Speaker, true, false),
        ]
    }

    /// Paired, in path order: CasqueBT (01, connected), ClavierBT (02), baffle-BT (05); nearby in
    /// reader order: Speaker-X (04), Keyboard-Test (03).
    fn workstation() -> BtView {
        view(
            true,
            paired(),
            vec![entry(4, "Speaker-X", BtKind::Speaker, false, false), entry(3, "Keyboard-Test", BtKind::Keyboard, false, false)],
        )
    }

    fn opened() -> Bt {
        let mut b = Bt { view: Some(workstation()), ..Bt::default() };
        b.open_detail();
        b
    }

    fn call(c: ActCall) -> QuickAction {
        QuickAction::Bluetooth(vec![BtOrder::Call(c)])
    }

    fn texts(b: &Bt, confirm: Option<&(Question, Instant)>) -> Vec<String> {
        let d = b.devices();
        b.lines()
            .into_iter()
            .map(|l| {
                let mut g = Grid::empty(WIDTH + 2);
                b.draw(&mut g, l, &d, confirm, b.is_selected(l, &d));
                g.text().trim_end().to_string()
            })
            .collect()
    }

    fn labels(b: &Bt) -> Vec<&str> {
        b.devices().iter().map(|d| d.label.as_str()).collect()
    }

    fn key(b: &mut Bt, k: Key) -> QuickAction {
        b.key(k, &mut None, Instant::now())
    }

    #[test]
    fn row_value_in_every_state() {
        assert!(!Bt::shows(None), "the bar's state not read yet");
        assert!(!Bt::shows(Some(&BtState::Absent)));
        assert!(Bt::shows(Some(&BtState::Off)));
        assert!(Bt::shows(Some(&BtState::On { devices: vec![] })));
        let b = Bt::default();
        let both = |s: &BtState| (b.value(s), b.glyph(s));
        assert_eq!(both(&BtState::Off), ("off".into(), BT_OFF));
        assert_eq!(both(&BtState::On { devices: vec![] }), ("on".into(), BT_ON));
        let two = BtState::On {
            devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(80) }, BtDevice { name: "ClavierBT".into(), battery: None }],
        };
        assert_eq!(both(&two), ("CasqueBT".into(), BT_CONNECTED), "the first, as the bar orders them");
        let long = BtState::On { devices: vec![BtDevice { name: "CasqueBT-".repeat(5), battery: None }] };
        let v = b.value(&long);
        assert_eq!(text_width(&v), 16);
        assert!(v.ends_with('…'), "{v}");
        let hostile = BtState::On { devices: vec![BtDevice { name: "\u{202e}\n".into(), battery: None }] };
        assert_eq!(b.value(&hostile), "?", "nothing left once cleaned");
        let b = Bt { view: Some(blocked()), ..Bt::default() };
        assert_eq!((b.value(&BtState::Off), b.glyph(&BtState::Off)), ("blocked".into(), BT_OFF), "rfkill holds it off");
    }

    #[test]
    fn space_powers_or_unblocks() {
        let mut on = Bt { view: Some(view(true, vec![], vec![])), ..Bt::default() };
        assert_eq!(on.power(&BtState::On { devices: vec![] }), call(ActCall::Powered { adapter: ADAPTER.into(), on: false }));
        let mut b = Bt { view: Some(blocked()), ..Bt::default() };
        assert_eq!(
            b.power(&BtState::Off),
            QuickAction::Run { argv: argv(&["rfkill", "unblock", "bluetooth"]), what: "bluetooth unblock".into() }
        );
        assert!(b.power_after_unblock);
        let t = Instant::now();
        assert_eq!(
            b.set_view(Some(view(false, vec![], vec![])), t),
            call(ActCall::Powered { adapter: ADAPTER.into(), on: true }),
            "unblocked: powered on"
        );
        assert!(!b.power_after_unblock, "once");
        assert_eq!(b.set_view(Some(view(false, vec![], vec![])), t), QuickAction::Redraw, "never twice");
        assert_eq!(Bt::default().power(&BtState::Off), QuickAction::Nothing, "no adapter read");
        assert_eq!(on.power(&BtState::Absent), QuickAction::Nothing);
    }

    #[test]
    fn detail_order_paired_then_nearby() {
        let b = opened();
        assert_eq!(
            b.lines(),
            [
                Line::BtHeader,
                Line::Separator,
                Line::BtDevice(0),
                Line::BtDevice(1),
                Line::BtDevice(2),
                Line::BtNearby,
                Line::BtDevice(3),
                Line::BtDevice(4)
            ]
        );
        assert_eq!(labels(&b), ["CasqueBT", "baffle-BT", "ClavierBT", "Speaker-X", "Keyboard-Test"], "by name in any case");
        assert_eq!(
            texts(&b, None),
            [
                format!(" ‹ Bluetooth{}on", " ".repeat(19)),
                String::new(),
                format!(" ● {HEADSET} CasqueBT"),
                format!("   {SPEAKER} baffle-BT{}paired", " ".repeat(13)),
                format!("   {KEYBOARD} ClavierBT{}paired", " ".repeat(13)),
                " Nearby".into(),
                format!("   {SPEAKER} Speaker-X"),
                format!("   {KEYBOARD} Keyboard-Test"),
            ]
        );
    }

    /// Review Focus 4: a hostile alias fits its row; an empty one reads `?`.
    #[test]
    fn an_empty_or_long_label_fits_its_row() {
        let one = |e: BtEntry| {
            let mut b = Bt { view: Some(view(true, vec![e], vec![])), ..Bt::default() };
            b.open_detail();
            texts(&b, None)[2].clone()
        };
        let long = "A".repeat(300);
        assert_eq!(one(entry(1, &long, BtKind::Keyboard, true, false)), format!("   {KEYBOARD} {}… paired", "A".repeat(20)));
        assert_eq!(one(entry(1, &long, BtKind::Headset, true, true)), format!(" ● {HEADSET} {}…", "A".repeat(27)));
        assert_eq!(one(entry(1, "", BtKind::Headset, true, true)), format!(" ● {HEADSET} ?"));
    }

    #[test]
    fn cap_32_rows() {
        let many = |n: std::ops::Range<u8>, paired: bool| -> Vec<BtEntry> {
            n.map(|n| entry(n, &format!("Device-{n}"), BtKind::Other, paired, false)).collect()
        };
        let mut b = Bt { view: Some(view(true, many(10..50, true), vec![])), ..Bt::default() };
        b.open_detail();
        assert_eq!(b.devices().len(), ROWS_MAX);
        assert_eq!(b.lines().len(), 2 + 32 + 1, "the header, the separator, 32 devices and the heading");
        let mut b = Bt { view: Some(view(true, many(10..30, true), many(30..50, false))), ..Bt::default() };
        b.open_detail();
        let d = b.devices();
        assert_eq!(d.len(), ROWS_MAX);
        assert_eq!(d.iter().filter(|e| e.paired).count(), 20, "the paired ones first");
        assert_eq!(d.iter().filter(|e| !e.paired).count(), 12, "then nearby, up to the cap");
        assert_eq!(b.lines()[22], Line::BtNearby);
    }

    #[test]
    fn glyph_per_kind() {
        for (kind, glyph) in [
            (BtKind::Headset, HEADSET),
            (BtKind::Keyboard, KEYBOARD),
            (BtKind::Mouse, MOUSE),
            (BtKind::Speaker, SPEAKER),
            (BtKind::Phone, PHONE),
            (BtKind::Other, GENERIC),
        ] {
            let mut b = Bt { view: Some(view(true, vec![entry(1, "X", kind, true, true)], vec![])), ..Bt::default() };
            b.open_detail();
            assert_eq!(texts(&b, None)[2], format!(" ● {glyph} X"), "{kind:?}");
        }
    }

    #[test]
    fn discovery_starts_once_per_open_and_stops_on_leave() {
        let t = Instant::now();
        let mut b = Bt { view: Some(workstation()), ..Bt::default() };
        assert_eq!(b.open_detail(), call(ActCall::StartDiscovery { adapter: ADAPTER.into() }));
        assert_eq!(b.set_view(Some(workstation()), t), QuickAction::Redraw, "once per open");
        assert_eq!(b.scope(true), BtScope::Detail);
        assert!(b.wants_connection());
        assert_eq!(b.close_detail(), vec![BtOrder::Call(ActCall::StopDiscovery { adapter: ADAPTER.into() })]);
        assert_eq!(b.close_detail(), vec![], "stopped once");
        assert_eq!((b.scope(true), b.scope(false)), (BtScope::Row, BtScope::Closed));
        assert!(!b.wants_connection());
        let mut b = Bt { view: Some(view(false, paired(), vec![])), ..Bt::default() };
        assert_eq!(b.open_detail(), QuickAction::Redraw, "off: nothing to discover");
        assert_eq!(b.set_view(Some(workstation()), t), call(ActCall::StartDiscovery { adapter: ADAPTER.into() }), "powered on");
        assert_eq!(b.set_view(Some(workstation()), t), QuickAction::Redraw, "started once");
        assert_eq!(b.set_view(Some(view(false, paired(), vec![])), t), QuickAction::Redraw);
        assert!(!b.discovering, "BlueZ ended it with the power");
        assert_eq!(b.set_view(Some(workstation()), t), call(ActCall::StartDiscovery { adapter: ADAPTER.into() }), "power back");
        let mut closed = Bt { view: Some(view(false, vec![], vec![])), ..Bt::default() };
        assert_eq!(closed.set_view(Some(workstation()), t), QuickAction::Redraw, "the detail closed: never discovering");
    }

    #[test]
    fn selection_kept_by_path_when_a_row_inserts_above() {
        let t = Instant::now();
        let mut b = opened();
        assert_eq!(b.selected, Some(path(1)), "the first device at open");
        assert_eq!(key(&mut b, Key::Down), QuickAction::Redraw);
        assert_eq!(key(&mut b, Key::Down), QuickAction::Redraw);
        assert_eq!(b.selected, Some(path(2)));
        let mut more = workstation();
        more.paired.push(entry(6, "aa-Test", BtKind::Keyboard, true, true));
        b.set_view(Some(more), t);
        b.fit();
        assert_eq!(b.selected, Some(path(2)), "by path");
        assert!(b.is_selected(Line::BtDevice(3), &b.devices()));
        let mut gone = workstation();
        gone.paired.retain(|e| e.path != path(2));
        b.set_view(Some(gone), t);
        b.fit();
        assert_eq!(b.selected, None, "gone: the header, never another device");
        assert!(b.is_selected(Line::BtHeader, &b.devices()));
        assert_eq!(key(&mut b, Key::Up), QuickAction::Nothing, "never wrapping");
        assert_eq!(key(&mut b, Key::End), QuickAction::Redraw);
        assert_eq!(b.selected, Some(path(3)), "the last device");
        assert_eq!(key(&mut b, Key::Down), QuickAction::Nothing);
        assert_eq!(key(&mut b, Key::Home), QuickAction::Redraw);
        assert_eq!(b.selected, None);
        assert_eq!(
            b.press(Line::BtDevice(1), Button::Left, &mut None, t),
            call(ActCall::Connect { device: path(5) }),
            "a click selects, then is Enter"
        );
        assert_eq!(b.selected, Some(path(5)));
        assert_eq!(b.press(Line::BtDevice(1), Button::Right, &mut None, t), QuickAction::Nothing);
        assert_eq!(key(&mut b, Key::Escape), call(ActCall::StopDiscovery { adapter: ADAPTER.into() }), "Escape leaves");
        assert!(!b.open);
        let mut b = opened();
        assert_eq!(
            b.press(Line::BtHeader, Button::Left, &mut None, t + Duration::from_millis(1)),
            call(ActCall::StopDiscovery { adapter: ADAPTER.into() })
        );
        assert!(!b.open, "the header goes back");
    }

    #[test]
    fn off_reads_bluetooth_is_off() {
        let mut b = Bt { view: Some(view(false, paired(), vec![])), ..Bt::default() };
        b.open_detail();
        assert_eq!(b.lines(), [Line::BtHeader, Line::Separator, Line::BtNote(OFF_NOTE)]);
        assert!(b.devices().is_empty());
        let t = texts(&b, None);
        assert_eq!(t[0], format!(" ‹ Bluetooth{}off", " ".repeat(18)));
        assert_eq!(t[2], "   Bluetooth is off");
        assert_eq!(key(&mut b, Key::Char(' ')), call(ActCall::Powered { adapter: ADAPTER.into(), on: true }), "Space on the header");
        let mut none = Bt::default();
        none.open_detail();
        assert_eq!(none.lines(), [Line::BtHeader, Line::Separator], "nothing read yet");
        assert_eq!(none.selected, None);
        none.set_view(Some(workstation()), Instant::now());
        assert_eq!(none.selected, Some(path(1)), "the first view of an open selects the first device");
    }

    const SEC: Duration = Duration::from_secs(1);

    fn remote(name: &str) -> Result<(), ActError> {
        Err(ActError::Remote(name.into()))
    }

    fn bt(orders: Vec<BtOrder>) -> QuickAction {
        QuickAction::Bluetooth(orders)
    }

    /// `opened()` with device `n` selected and Enter pressed at `t`.
    fn acting(n: u8, t: Instant) -> (Bt, QuickAction) {
        let mut b = opened();
        b.selected = Some(path(n));
        let a = b.enter(t);
        (b, a)
    }

    fn step(b: &Bt) -> Option<Step> {
        b.action.as_ref().map(|a| a.step)
    }

    #[test]
    fn enter_builds_the_call_per_device_kind() {
        let t = Instant::now();
        for (n, c, s) in [
            (1, ActCall::Disconnect { device: path(1) }, Step::Disconnect),
            (2, ActCall::Connect { device: path(2) }, Step::Connect),
            (3, ActCall::Pair { device: path(3) }, Step::Pair),
        ] {
            let (b, a) = acting(n, t);
            assert_eq!(a, call(c.clone()), "device {n}");
            assert_eq!(b.action, Some(Action { device: Some(path(n)), step: s, call: c, deadline: Some(t + CALL_DEADLINE) }));
        }
        let mut b = opened();
        b.selected = None;
        assert_eq!(b.enter(t), QuickAction::Nothing, "the header");
        assert_eq!(b.action, None);
    }

    #[test]
    fn pair_then_trust_then_connect() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        assert_eq!(b.answered(&ActCall::Pair { device: path(3) }, Ok(()), t), (call(ActCall::Trusted { device: path(3) }), None));
        assert_eq!(step(&b), Some(Step::Trust));
        assert_eq!(b.answered(&ActCall::Trusted { device: path(3) }, Ok(()), t), (call(ActCall::Connect { device: path(3) }), None));
        assert_eq!(step(&b), Some(Step::ConnectAfterPair));
        assert_eq!(b.answered(&ActCall::Connect { device: path(3) }, Ok(()), t), (QuickAction::Redraw, None));
        assert_eq!(b.action, None);
    }

    /// A pairing that succeeds while its code question is still pending rejects that question
    /// first, as any other end of the action does (`finish`; Task 7 review minor).
    #[test]
    fn a_pair_success_rejects_a_pending_code_question() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        let id = AgentId(5);
        b.agent(id, AgentRequest::RequestConfirmation { device: path(3), passkey: 7 }, t);
        assert_eq!(
            b.answered(&ActCall::Pair { device: path(3) }, Ok(()), t),
            (bt(vec![BtOrder::Reply(id, AgentReply::Rejected), BtOrder::Call(ActCall::Trusted { device: path(3) })]), None)
        );
        assert_eq!((b.code.clone(), step(&b)), (None, Some(Step::Trust)));
        let (mut b, _) = acting(3, t);
        b.agent(AgentId(6), AgentRequest::DisplayPasskey { device: path(3), passkey: 7 }, t);
        assert_eq!(
            b.answered(&ActCall::Pair { device: path(3) }, Ok(()), t),
            (call(ActCall::Trusted { device: path(3) }), None),
            "a code shown needs no answer"
        );
        assert_eq!(b.code, None);
    }

    /// A PIN shown is cleaned like a label (control and invisible characters), whatever handed it.
    #[test]
    fn a_pin_shown_is_cleaned_like_a_label() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        b.agent(AgentId(1), AgentRequest::DisplayPinCode { device: path(3), pincode: "\u{202e}12\u{7}34\u{200b}".into() }, t);
        assert_eq!(b.code, Some(Code::Shown { device: path(3), text: "1234".into() }));
    }

    #[test]
    fn pair_on_le_already_connected_skips_connect() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        b.answered(&ActCall::Pair { device: path(3) }, Ok(()), t);
        let mut v = workstation();
        v.nearby.retain(|e| e.path != path(3));
        v.paired.push(entry(3, "Keyboard-Test", BtKind::Keyboard, true, true));
        b.set_view(Some(v), t);
        assert_eq!(b.answered(&ActCall::Trusted { device: path(3) }, Ok(()), t), (QuickAction::Redraw, None));
        assert_eq!(b.action, None);
    }

    #[test]
    fn already_connected_is_success() {
        let t = Instant::now();
        let (mut b, _) = acting(2, t);
        assert_eq!(
            b.answered(&ActCall::Connect { device: path(2) }, remote("org.bluez.Error.AlreadyConnected"), t),
            (QuickAction::Redraw, None)
        );
        assert_eq!((b.action.clone(), b.failed.clone()), (None, None));
    }

    #[test]
    fn one_action_at_a_time() {
        let t = Instant::now();
        let (mut b, a) = acting(2, t);
        assert_eq!(a, call(ActCall::Connect { device: path(2) }));
        b.selected = Some(path(3));
        assert_eq!(b.enter(t), QuickAction::Nothing, "another device during …");
        assert_eq!(b.press(Line::BtDevice(4), Button::Left, &mut None, t), QuickAction::Nothing, "a click on it neither");
        b.selected = Some(path(2));
        let mut confirm = None;
        assert_eq!(b.key(Key::Delete, &mut confirm, t), QuickAction::Nothing);
        assert_eq!(confirm, None, "no forget question during an action");
        b.selected = None;
        assert_eq!(b.key(Key::Char(' '), &mut None, t), QuickAction::Nothing, "nor a power toggle");
        assert_eq!(b.power(&BtState::On { devices: vec![] }), QuickAction::Nothing);
        assert_eq!(step(&b), Some(Step::Connect));
    }

    #[test]
    fn failure_shows_failed_4s_and_logs_the_error_name() {
        let t = Instant::now();
        let (mut b, _) = acting(2, t);
        assert_eq!(texts(&b, None)[4], format!("   {KEYBOARD} ClavierBT{}…", " ".repeat(18)));
        assert_eq!(
            b.answered(&ActCall::Connect { device: path(2) }, remote("org.bluez.Error.Failed"), t + SEC),
            (QuickAction::Redraw, Some("quick: bluetooth connect failed (org.bluez.Error.Failed)".into()))
        );
        assert_eq!(b.failed, Some((path(2), t + SEC + FAILED_SHOWN)));
        assert_eq!(b.action, None);
        assert_eq!(texts(&b, None)[4], format!("   {KEYBOARD} ClavierBT{}failed", " ".repeat(13)));
        assert_eq!(b.deadline(), Some(t + 5 * SEC));
        assert_eq!(b.due(t + 4 * SEC), (vec![], None, false));
        assert_eq!(b.due(t + 5 * SEC), (vec![], None, true));
        assert_eq!((b.failed.clone(), b.deadline()), (None, None));
        assert_eq!(texts(&b, None)[4], format!("   {KEYBOARD} ClavierBT{}paired", " ".repeat(13)));
    }

    #[test]
    fn deadline_30s_per_call_and_suspended_while_a_code_shows() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        assert_eq!(b.deadline(), Some(t + 30 * SEC));
        b.agent(AgentId(1), AgentRequest::DisplayPasskey { device: path(3), passkey: 482_913 }, t + 5 * SEC);
        assert_eq!(b.deadline(), None, "suspended while the code shows");
        assert_eq!(b.due(t + 60 * SEC), (vec![], None, false));
        assert_eq!(b.agent(AgentId(2), AgentRequest::Cancel, t + 61 * SEC), QuickAction::Redraw);
        assert_eq!(b.deadline(), Some(t + 91 * SEC));
        b.answered(&ActCall::Pair { device: path(3) }, Ok(()), t + 62 * SEC);
        assert_eq!(step(&b), Some(Step::Trust));
        assert_eq!(b.deadline(), Some(t + 92 * SEC), "the next call's own 30 s");
        assert_eq!(
            b.due(t + 92 * SEC),
            (vec![BtOrder::Forget(ActCall::Trusted { device: path(3) })], Some("quick: bluetooth trust failed (timeout)".into()), true)
        );
        assert_eq!(b.failed, Some((path(3), t + 92 * SEC + FAILED_SHOWN)));
        assert_eq!(b.action, None);
        let (mut b, _) = acting(3, t);
        assert_eq!(
            b.due(t + 30 * SEC),
            (
                vec![BtOrder::Call(ActCall::CancelPairing { device: path(3) }), BtOrder::Forget(ActCall::Pair { device: path(3) })],
                Some("quick: bluetooth pair failed (timeout)".into()),
                true
            )
        );
    }

    #[test]
    fn silent_ends() {
        let t = Instant::now();
        for gone in [Err(ActError::Gone), remote("org.freedesktop.DBus.Error.UnknownObject")] {
            let (mut b, _) = acting(2, t);
            assert_eq!(b.answered(&ActCall::Connect { device: path(2) }, gone, t), (QuickAction::Redraw, None));
            assert_eq!((b.action.clone(), b.failed.clone()), (None, None));
        }
        let (mut b, _) = acting(2, t);
        let mut v = workstation();
        v.paired.retain(|e| e.path != path(2));
        assert_eq!(b.set_view(Some(v), t), bt(vec![BtOrder::Forget(ActCall::Connect { device: path(2) })]), "the device went");
        assert_eq!((b.action.clone(), b.failed.clone()), (None, None));
        // Review Focus 5: the adapter powered off elsewhere mid-pairing.
        let (mut b, _) = acting(3, t);
        b.agent(AgentId(1), AgentRequest::DisplayPasskey { device: path(3), passkey: 482_913 }, t);
        assert_eq!(b.set_view(Some(view(false, paired(), vec![])), t), bt(vec![BtOrder::Forget(ActCall::Pair { device: path(3) })]));
        assert_eq!((b.action.clone(), b.code.clone(), b.failed.clone()), (None, None, None));
        assert_eq!(b.lines()[2], Line::BtNote(OFF_NOTE));
        assert!(!b.discovering, "not restarted while off");
        assert_eq!(b.set_view(Some(view(false, paired(), vec![])), t), QuickAction::Redraw);
        assert_eq!(b.set_view(Some(workstation()), t), call(ActCall::StartDiscovery { adapter: ADAPTER.into() }), "power back");
        // A power action has no device: the adapter going off is its own answer.
        let mut b = Bt { view: Some(workstation()), ..Bt::default() };
        b.power(&BtState::On { devices: vec![] });
        b.arm(t);
        assert_eq!(b.set_view(Some(view(false, paired(), vec![])), t), QuickAction::Redraw);
        assert_eq!(step(&b), Some(Step::Power));
    }

    #[test]
    fn a_power_toggle_is_an_action_until_its_answer() {
        let t = Instant::now();
        let mut b = Bt { view: Some(workstation()), ..Bt::default() };
        let off = ActCall::Powered { adapter: ADAPTER.into(), on: false };
        assert_eq!(b.power(&BtState::On { devices: vec![] }), call(off.clone()));
        assert_eq!(b.action, Some(Action { device: None, step: Step::Power, call: off.clone(), deadline: None }));
        assert!(b.wants_connection(), "the row's toggle keeps the connection until its answer");
        assert_eq!(b.scope(true), BtScope::Row, "no device: the row's reading");
        b.arm(t);
        assert_eq!(b.deadline(), Some(t + CALL_DEADLINE));
        b.arm(t + SEC);
        assert_eq!(b.deadline(), Some(t + CALL_DEADLINE), "armed once");
        assert_eq!(
            b.answered(&off, remote("org.bluez.Error.Busy"), t),
            (QuickAction::Redraw, Some("quick: bluetooth power failed (org.bluez.Error.Busy)".into()))
        );
        assert_eq!((b.action.clone(), b.failed.clone()), (None, None), "nothing to tag");
        assert!(!b.wants_connection());
        // Unblocked by rfkill: powered on as an action, with its deadline.
        let mut b = Bt { view: Some(blocked()), ..Bt::default() };
        b.power(&BtState::Off);
        assert_eq!(b.action, None, "rfkill is a command, not a call");
        let running = Action { device: None, step: Step::Power, call: off.clone(), deadline: Some(t) };
        b.action = Some(running.clone());
        assert_eq!(b.set_view(Some(view(false, vec![], vec![])), t), QuickAction::Redraw, "waits while an action runs");
        assert_eq!((b.action.clone(), b.power_after_unblock), (Some(running), true));
        b.action = None;
        b.set_view(Some(view(false, vec![], vec![])), t);
        assert_eq!(
            b.action,
            Some(Action {
                device: None,
                step: Step::Power,
                call: ActCall::Powered { adapter: ADAPTER.into(), on: true },
                deadline: Some(t + CALL_DEADLINE)
            })
        );
    }

    #[test]
    fn forget_timing_and_cancels() {
        let t = Instant::now();
        let mut b = opened();
        let mut confirm = None;
        b.selected = Some(path(2));
        assert_eq!(b.key(Key::Delete, &mut confirm, t), QuickAction::Redraw);
        assert_eq!(confirm, Some((Question::Forget(path(2)), t + CONFIRM)));
        assert_eq!(texts(&b, confirm.as_ref())[4], format!("   {KEYBOARD} ClavierBT{}Forget? ⏎", " ".repeat(10)));
        assert_eq!(b.key(Key::Enter, &mut confirm, t + Duration::from_millis(200)), QuickAction::Nothing, "a double Enter never confirms");
        assert!(confirm.is_some(), "the question stays");
        assert_eq!(
            b.key(Key::Enter, &mut confirm, t + CONFIRM_AFTER),
            call(ActCall::RemoveDevice { adapter: ADAPTER.into(), device: path(2) })
        );
        assert_eq!(confirm, None);
        assert_eq!(step(&b), Some(Step::Forget));
        let mut b = opened();
        b.selected = Some(path(5));
        b.key(Key::Delete, &mut confirm, t);
        assert!(session::expire(&mut confirm, t + CONFIRM));
        assert_eq!(b.key(Key::Enter, &mut confirm, t + CONFIRM), call(ActCall::Connect { device: path(5) }), "expired: Enter is Enter");
        let mut b = opened();
        b.selected = Some(path(2));
        b.key(Key::Delete, &mut confirm, t);
        assert_eq!(b.key(Key::Down, &mut confirm, t), QuickAction::Redraw);
        assert_eq!(confirm, None, "any other key cancels");
        b.selected = Some(path(4));
        assert_eq!(b.key(Key::Delete, &mut confirm, t), QuickAction::Nothing, "a nearby device cannot be forgotten");
        assert_eq!(confirm, None);
        b.selected = Some(path(1));
        assert_eq!(b.press(Line::BtDevice(2), Button::Right, &mut confirm, t), QuickAction::Redraw);
        assert_eq!(confirm, Some((Question::Forget(path(2)), t + CONFIRM)));
        assert_eq!(b.selected, Some(path(2)));
        assert_eq!(
            b.press(Line::BtDevice(2), Button::Left, &mut confirm, t + CONFIRM_AFTER),
            call(ActCall::RemoveDevice { adapter: ADAPTER.into(), device: path(2) }),
            "a left press there is the second activation"
        );
    }

    #[test]
    fn each_agent_request_state_and_reply() {
        use AgentRequest::*;
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        let id = AgentId(1);
        assert_eq!(b.agent(id, DisplayPasskey { device: path(3), passkey: 482_913 }, t), bt(vec![BtOrder::Reply(id, AgentReply::Ok)]));
        assert_eq!(b.code, Some(Code::Shown { device: path(3), text: "482 913".into() }));
        assert_eq!(b.lines()[b.lines().len() - 2..], [Line::BtDevice(4), Line::BtCode]);
        assert_eq!(texts(&b, None).last().map(String::as_str), Some("     Type 482 913 then ⏎"));
        assert!(b.wants_connection());
        assert_eq!(b.agent(AgentId(2), Cancel, t), QuickAction::Redraw);
        assert_eq!(b.code, None);
        b.agent(AgentId(3), DisplayPinCode { device: path(3), pincode: "0000".into() }, t);
        assert_eq!(texts(&b, None).last().map(String::as_str), Some("     Type 0000 then ⏎"));
        assert_eq!(b.agent(AgentId(4), Release, t), QuickAction::Redraw);
        assert_eq!(b.code, None);
        let id = AgentId(5);
        assert_eq!(b.agent(id, RequestConfirmation { device: path(3), passkey: 7 }, t), QuickAction::Redraw);
        assert_eq!(b.code, Some(Code::Question { device: path(3), passkey: 7, id }));
        assert_eq!(b.deadline(), None);
        assert_eq!(texts(&b, None)[7], format!("   {KEYBOARD} Keyboard-Te… Code 000 007? ⏎"));
        assert_eq!(b.press(Line::BtDevice(0), Button::Left, &mut None, t), QuickAction::Nothing, "presses wait for the answer");
        let now = t + 10 * SEC;
        assert_eq!(b.key(Key::Enter, &mut None, now), bt(vec![BtOrder::Reply(id, AgentReply::Ok)]));
        assert_eq!(b.code, None);
        assert_eq!(b.deadline(), Some(now + 30 * SEC));
        let id2 = AgentId(6);
        b.agent(id2, RequestConfirmation { device: path(3), passkey: 8 }, now);
        assert_eq!(
            b.agent(AgentId(7), RequestConfirmation { device: path(3), passkey: 9 }, now),
            bt(vec![BtOrder::Reply(id2, AgentReply::Rejected)]),
            "an older question rejected first"
        );
        let id2 = AgentId(7);
        assert_eq!(
            b.key(Key::Escape, &mut None, now),
            bt(vec![BtOrder::Reply(id2, AgentReply::Rejected), BtOrder::Forget(ActCall::Pair { device: path(3) })])
        );
        assert_eq!((b.action.clone(), b.code.clone(), b.failed.clone()), (None, None, None));
        assert!(b.open, "the key does nothing else");
        for r in [RequestAuthorization { device: path(3) }, AuthorizeService { device: path(3) }] {
            let (mut b, _) = acting(3, t);
            assert_eq!(b.agent(AgentId(8), r, t), bt(vec![BtOrder::Reply(AgentId(8), AgentReply::Rejected)]));
            assert_eq!((step(&b), b.code.clone()), (Some(Step::Pair), None));
        }
        for r in [RequestPinCode { device: path(3) }, RequestPasskey { device: path(3) }] {
            let (mut b, _) = acting(3, now);
            assert_eq!(
                b.agent(AgentId(9), r, now),
                bt(vec![BtOrder::Reply(AgentId(9), AgentReply::Rejected), BtOrder::Forget(ActCall::Pair { device: path(3) })])
            );
            assert_eq!((b.action.clone(), b.failed.clone()), (None, Some((path(3), now + 4 * SEC))));
        }
        assert_eq!(Bt::format_code(0), "000 000");
        assert_eq!(Bt::format_code(999_999), "999 999");
    }

    /// A DisplayPasskey repeats as keys are typed on the keyboard: the same line, updated.
    #[test]
    fn display_passkey_repeats_on_one_line() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        for (i, id) in (1..=3).map(AgentId).enumerate() {
            assert_eq!(
                b.agent(id, AgentRequest::DisplayPasskey { device: path(3), passkey: 482_913 }, t),
                bt(vec![BtOrder::Reply(id, AgentReply::Ok)]),
                "{i}"
            );
        }
        assert_eq!(b.lines().iter().filter(|l| **l == Line::BtCode).count(), 1);
    }

    #[test]
    fn another_device_is_rejected() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        let reject = bt(vec![BtOrder::Reply(AgentId(1), AgentReply::Rejected)]);
        assert_eq!(b.agent(AgentId(1), AgentRequest::DisplayPasskey { device: path(4), passkey: 1 }, t), reject);
        assert_eq!(b.agent(AgentId(1), AgentRequest::RequestConfirmation { device: path(4), passkey: 1 }, t), reject);
        assert_eq!(b.code, None);
        let mut b = opened();
        assert_eq!(b.agent(AgentId(1), AgentRequest::RequestConfirmation { device: path(3), passkey: 1 }, t), reject, "nothing pairing");
        assert_eq!(b.code, None);
        let (mut b, _) = acting(2, t);
        assert_eq!(
            b.agent(AgentId(1), AgentRequest::DisplayPasskey { device: path(2), passkey: 1 }, t),
            reject,
            "a connect is not a pairing"
        );
        assert_eq!(b.code, None);
    }

    /// Review Focus 2: closing during `Code 482 913? ⏎`: Rejected, CancelPairing, then discovery stops.
    #[test]
    fn leaving_rejects_then_cancels_pairing_then_stops_discovery() {
        let t = Instant::now();
        let (mut b, _) = acting(3, t);
        b.agent(AgentId(9), AgentRequest::RequestConfirmation { device: path(3), passkey: 482_913 }, t);
        assert_eq!(
            b.leaving(),
            vec![
                BtOrder::Reply(AgentId(9), AgentReply::Rejected),
                BtOrder::Call(ActCall::CancelPairing { device: path(3) }),
                BtOrder::Forget(ActCall::Pair { device: path(3) }),
                BtOrder::Call(ActCall::StopDiscovery { adapter: ADAPTER.into() }),
            ]
        );
        assert_eq!((b.action.clone(), b.code.clone(), b.discovering), (None, None, false));
        assert_eq!(b.leaving(), vec![]);
        let (mut b, _) = acting(2, t);
        assert_eq!(b.close_detail(), vec![BtOrder::Call(ActCall::StopDiscovery { adapter: ADAPTER.into() })]);
        assert_eq!(step(&b), Some(Step::Connect), "a connect outlives the detail");
        assert!(!b.open);
        assert!(b.wants_connection());
        assert_eq!(b.scope(true), BtScope::Detail, "its device still read");
        assert_eq!(b.answered(&ActCall::Connect { device: path(2) }, Ok(()), t), (QuickAction::Redraw, None));
        assert!(!b.wants_connection());
        assert_eq!(b.scope(true), BtScope::Row);
    }

    #[test]
    fn no_log_names_a_device() {
        let t = Instant::now();
        let d = path(1);
        let calls = [
            ActCall::Powered { adapter: ADAPTER.into(), on: true },
            ActCall::Trusted { device: d.clone() },
            ActCall::StartDiscovery { adapter: ADAPTER.into() },
            ActCall::StopDiscovery { adapter: ADAPTER.into() },
            ActCall::RemoveDevice { adapter: ADAPTER.into(), device: d.clone() },
            ActCall::Connect { device: d.clone() },
            ActCall::Disconnect { device: d.clone() },
            ActCall::Pair { device: d.clone() },
            ActCall::CancelPairing { device: d.clone() },
        ];
        let mut logs = Vec::new();
        for c in &calls {
            for e in ["org.bluez.Error.Failed", "org.bluez.Error.AuthenticationFailed"] {
                for step in [Step::Power, Step::Connect, Step::Pair, Step::Forget] {
                    let mut b = opened();
                    b.action = Some(Action { device: Some(d.clone()), step, call: c.clone(), deadline: Some(t) });
                    logs.extend(b.answered(c, remote(e), t).1);
                    let mut b = opened();
                    b.action = Some(Action { device: Some(d.clone()), step, call: c.clone(), deadline: Some(t) });
                    logs.extend(b.due(t).1);
                }
            }
        }
        assert!(logs.len() >= 2 * calls.len(), "{}", logs.len());
        let words = |s: &str| s.chars().all(|c| c.is_ascii_lowercase() || c == ' ');
        for l in &logs {
            let rest = l.strip_prefix("quick: bluetooth ").unwrap_or_else(|| panic!("{l}"));
            let (word, why) = rest.split_once(" failed (").unwrap_or_else(|| panic!("{l}"));
            let why = why.strip_suffix(')').unwrap_or_else(|| panic!("{l}"));
            assert!(words(word) && !word.is_empty(), "{l}");
            assert!(why.chars().all(|c| c.is_ascii_alphabetic() || c == '.'), "{l}");
            for never in ["AA_BB", "/org", "CasqueBT", "Keyboard"] {
                assert!(!l.contains(never), "{l}");
            }
        }
    }

    /// A left click on the Bluetooth row (in the panel) opens the detail: see quick::tests; here a
    /// left click on a device is Enter.
    #[test]
    fn a_left_press_on_a_device_is_enter() {
        let t = Instant::now();
        let mut b = opened();
        assert_eq!(b.press(Line::BtDevice(2), Button::Left, &mut None, t), call(ActCall::Connect { device: path(2) }));
        assert_eq!(b.selected, Some(path(2)));
    }

    #[test]
    fn the_header_draws_no_value_before_a_view() {
        let mut b = Bt::default();
        b.open_detail();
        assert_eq!(texts(&b, None)[0], " ‹ Bluetooth");
    }
}
