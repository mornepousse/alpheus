//! The quick panel's Bluetooth: the row, the list of devices behind it and the pairing codes.
//!
//! This is the pure half of the Bluetooth list; the wiring is in `bt_app.rs`. It holds the row's
//! glyph and value, the devices the detail lists (the paired ones, then the ones nearby, at most
//! `ROWS_MAX`, the selection kept by object path), the keys and clicks of the detail, and the BlueZ
//! calls they ask for (`BtOrder`), which the wiring hands to the acting connection
//! (`sources/dbus/bt_act.rs`). The view comes from the D-Bus thread (`sources/dbus/bt.rs`) only
//! while the panel is open. One action runs at a time (a power toggle, a connect, a disconnect, a
//! pairing, a forget), each call with its own `CALL_DEADLINE`, which waits while a pairing code
//! shows. BlueZ's agent requests are answered for the device being paired only. The row's place on
//! the panel and the window over the lines belong to `mod.rs`, which calls into this file for the
//! Bluetooth row and the detail's lines.
//!
//! Never:
//! - write or create a file, or make a raw system call (guardrail
//!   `the_quick_model_and_its_state_reader_only_read`);
//! - run a command or start a thread (guardrail `the_tray_spawns_no_thread`, through its
//!   `NO_THREAD_NOR_COMMAND` list), or arm a timer (the table of timer and thread tokens in
//!   `nothing_resident_while_the_launcher_is_closed`): a command is handed back as a `QuickAction`
//!   (the one is `rfkill unblock bluetooth`), a BlueZ call as a `BtOrder`, and the wiring arms the
//!   panel's one timer from `Quick::deadline`, which includes `Bt::deadline`;
//! - call BlueZ itself: a `BtOrder` is carried out by `bt_app.rs` on the acting connection, whose
//!   calls are the pinned `ActMethod` list (guardrail
//!   `the_bluetooth_connection_acts_only_through_act_method`);
//! - put a device's name, label, address or object path in a log line (guardrail
//!   `the_bluetooth_logs_name_no_device`, which reads this file's log lines): a line holds an
//!   action's word and the D-Bus error's name;
//! - panic (guardrail `the_quick_panel_has_no_panicking_path`);
//! - read a clock: the caller passes `now` (no guardrail checks it).
//!
//! See: docs/specs/2026-10-20-alpheus-quick-settings-bluetooth-design.md,
//! docs/features/quick-settings.md, and docs/decisions.md ("Space on a blocked Bluetooth radio
//! unblocks it, then powers it on", "Nearby Bluetooth devices are inserted once, by signal strength",
//! "A paired Bluetooth device is trusted, and a connect that is already done counts as done",
//! "Forgetting a Bluetooth device asks first, as the power menu does", "Bluetooth deadlines live in
//! the panel, and a late answer is forgotten", "A pairing is answered by alpheus's own agent, for
//! that device only", "A pairing code has no timeout, and leaving the detail cancels the pairing").

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

/// The most devices the detail lists, paired and nearby together (spec § 2).
pub const ROWS_MAX: usize = 32;
/// The row's glyph for an adapter that is off or blocked: `md-bluetooth-off` (Nerd Font's Material
/// Design set; the bar's module has no glyph for off, it hides). The on and connected glyphs are the
/// module's own (`segments::BLUETOOTH`, `segments::BLUETOOTH_CONNECTED`), so the row draws as the
/// module does (spec § 1).
pub const BT_OFF: char = '\u{f00b2}';
/// A device's glyph by its kind: headphones, keyboard, mouse, speaker, cellphone, and the
/// bluetooth-connect glyph for any other kind.
pub const HEADSET: char = '\u{f02cb}';
pub const KEYBOARD: char = '\u{f030c}';
pub const MOUSE: char = '\u{f037d}';
pub const SPEAKER: char = '\u{f04c3}';
pub const PHONE: char = '\u{f03f2}';
pub const GENERIC: char = BLUETOOTH_CONNECTED;
/// The note the detail shows with the adapter off, and the heading above the nearby devices.
pub const OFF_NOTE: &str = "Bluetooth is off";
pub const NEARBY: &str = "Nearby";
/// A paired, unconnected device's tag.
const PAIRED_TAG: &str = "paired";
/// The tag a device shows while its forget question is asked (spec § 2).
const FORGET_TAG: &str = "Forget? ⏎";
/// How long each call of an action may wait for its answer (spec § 2): an action with several calls
/// (a pairing is a pair, a trust, a connect) gives each one its own.
pub const CALL_DEADLINE: Duration = Duration::from_secs(30);
/// BlueZ's answer to a `Connect` on a device already connected. It counts as a success (spec § 2:
/// a Bluetooth LE device is often connected by the pairing itself).
const ALREADY_CONNECTED: &str = "org.bluez.Error.AlreadyConnected";
/// BlueZ's answer for an object that went (a device removed meanwhile): the action ends silently
/// (spec § 6).
const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";

/// What the panel asks of the acting connection: a call to BlueZ, the agent's answer to one of
/// BlueZ's requests, or to stop waiting for a call's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtOrder {
    Call(ActCall),
    Reply(AgentId, AgentReply),
    /// Stop waiting for this call's answer (the acting connection's `forget`): nothing goes on the
    /// bus, and the answer, if it comes late, is dropped.
    Forget(ActCall),
}

/// Where an action is: the call it is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Power,
    Connect,
    Disconnect,
    Pair,
    /// The `Trusted` property set after a pairing.
    Trust,
    /// The connect that follows a pairing's trust.
    ConnectAfterPair,
    Forget,
}

/// The one action running: its device (None: the adapter's power), its step, the call waiting for
/// its answer, and until when. A deadline of None means either suspended (a code shows) or not
/// armed yet (`arm` gives it one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub device: Option<String>,
    pub step: Step,
    pub call: ActCall,
    pub deadline: Option<Instant>,
}

/// A pairing's code (spec § 3): shown to be typed on the device, or asked to be compared with the
/// one the device shows (`id` names the BlueZ request the answer goes to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    Shown { device: String, text: String },
    Question { device: String, passkey: u32, id: AgentId },
}

/// The panel's Bluetooth state: dropped whole with the panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bt {
    /// What the D-Bus thread last read; None: nothing yet, or BlueZ gone.
    pub view: Option<BtView>,
    /// The detail shows in place of the rows.
    pub open: bool,
    /// The selected device by object path (never by index, so a row inserted above does not move the
    /// selection); None: the header.
    pub selected: Option<String>,
    /// Discovery was asked for during this open, and the adapter has stayed powered since (a
    /// powered-off adapter ends it on BlueZ's side).
    pub discovering: bool,
    /// `rfkill unblock bluetooth` ran: the adapter is powered on once BlueZ reports it unblocked.
    pub power_after_unblock: bool,
    /// The one action running.
    pub action: Option<Action>,
    /// The device whose action failed, and until when its tag reads `failed`.
    pub failed: Option<(String, Instant)>,
    /// The code of the running pairing.
    pub code: Option<Code>,
}

/// The call that sets the adapter's power to `on`.
fn powered(a: &BtAdapter, on: bool) -> ActCall {
    ActCall::Powered { adapter: a.path.clone(), on }
}

/// The glyph a device of this kind shows in the detail.
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

/// The orders as an action for the wiring: a redraw when there are none.
fn orders(v: Vec<BtOrder>) -> QuickAction {
    if v.is_empty() { QuickAction::Redraw } else { QuickAction::Bluetooth(v) }
}

impl Bt {
    /// Whether the row shows: BlueZ has an adapter. `Absent`, or a state the bar has not read, hides
    /// it (spec § 1).
    pub fn shows(bt: Option<&BtState>) -> bool {
        matches!(bt, Some(BtState::Off | BtState::On { .. }))
    }

    /// The view's adapter, if a view was read and BlueZ has one.
    fn adapter(&self) -> Option<&BtAdapter> {
        // `?` after an Option that is None ends the function with None.
        self.view.as_ref()?.adapter.as_ref()
    }

    /// Whether the view's adapter is powered on (false without a view or an adapter).
    fn powered(&self) -> bool {
        match self.adapter() {
            Some(a) => a.powered,
            None => false,
        }
    }

    /// The row's glyph: the connected one when a device is connected, the module's own on glyph for
    /// an adapter that is on, else the off glyph.
    pub fn glyph(&self, bt: &BtState) -> char {
        match bt {
            BtState::On { devices } if !devices.is_empty() => BLUETOOTH_CONNECTED,
            BtState::On { .. } => BLUETOOTH,
            _ => BT_OFF,
        }
    }

    /// The row's value (spec § 1): the first connected device's name (the bar's order, cleaned, cut
    /// to `VALUE_CELLS`, `?` when nothing is left), else `on`, `off`, or `blocked` when rfkill holds
    /// the adapter off.
    pub fn value(&self, bt: &BtState) -> String {
        match bt {
            BtState::On { devices } => match devices.first() {
                Some(d) => {
                    let name = text::clean(&d.name);
                    if name.trim().is_empty() { "?".into() } else { truncate(&name, VALUE_CELLS) }
                }
                None => "on".into(),
            },
            _ => {
                let blocked = match self.adapter() {
                    Some(a) => a.blocked,
                    None => false,
                };
                if blocked { "blocked".into() } else { "off".into() }
            }
        }
    }

    /// The detail's devices (spec § 2): none unless the adapter is powered; the paired ones,
    /// connected first, then by name (any case), then the ones nearby in the reader's order; at most
    /// `ROWS_MAX` in all.
    pub fn devices(&self) -> Vec<&BtEntry> {
        if !self.powered() {
            return Vec::new();
        }
        let Some(v) = self.view.as_ref() else { return Vec::new() };
        let mut paired: Vec<&BtEntry> = v.paired.iter().collect();
        // The key is computed once per device: `false` sorts before `true`, so `!connected` puts
        // the connected ones first; the path breaks a tie between two equal names.
        paired.sort_by_cached_key(|e| (!e.connected, e.label.to_lowercase(), e.path.clone()));
        paired.into_iter().chain(&v.nearby).take(ROWS_MAX).collect()
    }

    /// How many of `devices()` are paired: they come first.
    fn paired_listed(&self) -> usize {
        // Its only caller, `lines`, has already returned for an adapter that is not powered, so this
        // check is never the one that decides; it keeps the count in step with `devices`.
        if !self.powered() {
            return 0;
        }
        match &self.view {
            Some(v) => v.paired.len().min(ROWS_MAX),
            None => 0,
        }
    }

    /// Enter, →, a click on the row: the detail, the first device selected (the header without
    /// one); discovery starts when the adapter is powered (spec § 2).
    pub fn open_detail(&mut self) -> QuickAction {
        self.open = true;
        self.selected = self.devices().first().map(|d| d.path.clone());
        let adapter = match self.adapter() {
            Some(a) if a.powered => a.path.clone(),
            _ => return QuickAction::Redraw,
        };
        self.discovering = true;
        QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::StartDiscovery { adapter })])
    }

    /// Back to the rows: the orders `leaving` gives.
    pub fn close_detail(&mut self) -> Vec<BtOrder> {
        self.open = false;
        self.selected = None;
        self.leaving()
    }

    /// Leaving the detail or closing the panel, in order: a pending code question rejected, a
    /// pairing cancelled and its call forgotten (the code could no longer show: spec § 3),
    /// discovery stopped if this open started it. Any action but a pairing's Pair step outlives the
    /// detail.
    pub fn leaving(&mut self) -> Vec<BtOrder> {
        let mut v = Vec::new();
        if let Some(Code::Question { id, .. }) = self.code.take() {
            v.push(BtOrder::Reply(id, AgentReply::Rejected));
        }
        // `take_if` takes the action out only when the test is true, and leaves it otherwise.
        if let Some(a) = self.action.take_if(|a| a.step == Step::Pair) {
            if let Some(device) = a.device {
                v.push(BtOrder::Call(ActCall::CancelPairing { device }));
            }
            v.push(BtOrder::Forget(a.call));
        }
        // `mem::take` reads the flag and sets it to false in one step; the `if let` chained after
        // the `&&` is a let chain: it runs only when the flag was set.
        if std::mem::take(&mut self.discovering)
            && let Some(a) = self.adapter()
        {
            v.push(BtOrder::Call(ActCall::StopDiscovery { adapter: a.path.clone() }));
        }
        v
    }

    /// What the D-Bus thread is to read: the detail's lists while it is open (or while an action on
    /// a device runs: a row reading would look like that device gone), the adapter while the row
    /// shows, nothing otherwise. The panel's Bluetooth tap follows this at each sync.
    pub fn scope(&self, row_shown: bool) -> BtScope {
        if self.open || self.action.as_ref().is_some_and(|a| a.device.is_some()) {
            BtScope::Detail
        } else if row_shown {
            BtScope::Row
        } else {
            BtScope::Closed
        }
    }

    /// Whether the acting connection is wanted: the detail is open, an action runs or a code shows
    /// (spec § 5: it lives while the detail is open or a call is in flight).
    pub fn wants_connection(&self) -> bool {
        self.open || self.action.is_some() || self.code.is_some()
    }

    /// The action and its code end: a pending code question rejected; its call forgotten when its
    /// answer is still awaited (`forget` true); `failed` on its device until `FAILED_SHOWN` after
    /// `failed_at`, if given. The orders to send are returned.
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

    /// The code goes; a pending code question is rejected (the order to send is returned).
    fn end_code(&mut self) -> Vec<BtOrder> {
        match self.code.take() {
            Some(Code::Question { id, .. }) => vec![BtOrder::Reply(id, AgentReply::Rejected)],
            _ => Vec::new(),
        }
    }

    /// Starts an action on `call`, its deadline `CALL_DEADLINE` after `now` (None: no deadline yet,
    /// `arm` gives one); returns the order that sends the call.
    fn start(&mut self, device: Option<String>, step: Step, call: ActCall, now: Option<Instant>) -> BtOrder {
        self.action = Some(Action { device, step, call: call.clone(), deadline: now.map(|n| n + CALL_DEADLINE) });
        BtOrder::Call(call)
    }

    /// A just-started action with no deadline and no code gets its `CALL_DEADLINE` from `now`. The
    /// power toggle needs this: it starts an action without a clock (`toggle`).
    pub(crate) fn arm(&mut self, now: Instant) {
        if self.code.is_some() {
            return;
        }
        let Some(a) = self.action.as_mut() else { return };
        if a.deadline.is_none() {
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
        let Some(selected) = self.selected.as_ref() else { return QuickAction::Nothing };
        let Some(d) = devices.iter().find(|d| &d.path == selected) else { return QuickAction::Nothing };
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
    /// no action running (spec § 2: a nearby device cannot be forgotten). The question lasts
    /// `session::CONFIRM`.
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
    /// it asks about this device (from `CONFIRM_AFTER` to `CONFIRM`), else `enter`.
    fn activate(&mut self, confirm: &mut Option<(Question, Instant)>, now: Instant) -> QuickAction {
        // The question is open when it names the selected device and has not run out.
        let asked = match (confirm.as_ref(), &self.selected) {
            (Some((Question::Forget(q), until)), Some(p)) => q == p && now < *until,
            _ => false,
        };
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
        let running = match self.action.as_ref() {
            Some(a) if &a.call == call => Some(a),
            _ => None,
        };
        let Some(a) = running else {
            // Not the action's: discovery failing is logged once, never retried.
            return match (call, result) {
                (ActCall::StartDiscovery { .. }, Err(ActError::Remote(why))) => {
                    (QuickAction::Nothing, Some(format!("quick: bluetooth {} failed ({why})", call.word())))
                }
                _ => (QuickAction::Nothing, None),
            };
        };
        let step = a.step;
        // An answer is a success, or `AlreadyConnected` to a connect (see `ALREADY_CONNECTED`).
        let success = match &result {
            Ok(()) => true,
            Err(ActError::Remote(why)) => why == ALREADY_CONNECTED && matches!(step, Step::Connect | Step::ConnectAfterPair),
            Err(ActError::Gone) => false,
        };
        if !success {
            // A BlueZ error shows `failed` and logs its name. A vanished object (`UNKNOWN_OBJECT`) or
            // a connection that went (`Gone`) ends the action silently: no `failed`, no line.
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

    /// Whether the view shows the device at `path` connected.
    fn connected(&self, path: &str) -> bool {
        self.view.as_ref().is_some_and(|v| v.paired.iter().chain(&v.nearby).any(|e| e.path == path && e.connected))
    }

    /// The device being paired: the action's, while its step is `Pair`.
    fn pairing(&self) -> Option<&str> {
        self.action.as_ref().filter(|a| a.step == Step::Pair).and_then(|a| a.device.as_deref())
    }

    /// `code` replaces the one showing: a pending question is rejected first (its order is pushed
    /// on `v`). The action's deadline waits while a code shows (spec § 2: BlueZ's own pairing
    /// timeout ends it).
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
    /// any other device, is rejected. Every request but BlueZ's own Cancel and Release (already
    /// answered by the connection) gets its reply as a `BtOrder::Reply`.
    pub fn agent(&mut self, id: AgentId, request: AgentRequest, now: Instant) -> QuickAction {
        use AgentRequest::*;
        // A closure that captures `id` and builds the rejection; the last arm below uses it.
        let reject = || QuickAction::Bluetooth(vec![BtOrder::Reply(id, AgentReply::Rejected)]);
        let mut v = Vec::new();
        match request {
            Cancel | Release => {
                // BlueZ's own (`bt_act.rs` already answered it): the code goes, the deadline runs again.
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
        let Some(a) = self.action.as_ref() else { return (Vec::new(), None, redraw) };
        let passed = match a.deadline {
            Some(deadline) => deadline <= now,
            None => false,
        };
        if !passed {
            return (Vec::new(), None, redraw);
        }
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
        let action_deadline = match &self.action {
            Some(a) => a.deadline,
            None => None,
        };
        // `map` changes what is inside a Some and leaves a None alone: here, the (device, end) pair
        // becomes just its end.
        let failed_end = self.failed.as_ref().map(|(_, until)| *until);
        match (action_deadline, failed_end) {
            (Some(a), Some(f)) => Some(a.min(f)),
            (Some(a), None) => Some(a),
            (None, f) => f,
        }
    }

    /// Whether the running action is on a device that is no longer there: its adapter is off, or the
    /// device is in neither list of the view (spec § 6). An action on the adapter itself is never gone.
    fn action_device_gone(&self) -> bool {
        let Some(device) = self.action.as_ref().and_then(|a| a.device.as_deref()) else { return false };
        if !self.powered() {
            return true;
        }
        match &self.view {
            Some(v) => !v.paired.iter().chain(&v.nearby).any(|e| e.path == device),
            // `set_view`, the only caller, has already returned for a view of None.
            None => true,
        }
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
        if self.action_device_gone() {
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

    /// Space on the row (spec § 1): the adapter's power the other way; with rfkill holding it off,
    /// `rfkill unblock bluetooth` first. Nothing without an adapter read.
    pub fn power(&mut self, bt: &BtState) -> QuickAction {
        if *bt == BtState::Absent {
            return QuickAction::Nothing;
        }
        self.toggle()
    }

    /// The view's adapter powered the other way (the row's and the header's Space): an action until
    /// its answer (`arm` gives it its deadline); nothing while another action runs. When rfkill
    /// holds the adapter off, it hands back `rfkill unblock bluetooth` for the wiring to run instead
    /// and notes that the adapter is to be powered on once BlueZ reports it unblocked
    /// (`power_after_unblock`).
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
    /// or the paired devices, the `Nearby` heading and the nearby ones. A device whose pairing code
    /// is shown is followed by the code's line.
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
        for i in 0..paired {
            push_device(&mut v, i, shown);
        }
        v.push(Line::BtNearby);
        for i in paired..devices.len() {
            push_device(&mut v, i, shown);
        }
        v
    }

    /// Whether `line` is selected: the header when no device is, a device by its object path.
    /// `devices` is `devices()`, computed once per pass.
    pub fn is_selected(&self, line: Line, devices: &[&BtEntry]) -> bool {
        match line {
            Line::BtHeader => self.selected.is_none(),
            Line::BtDevice(i) => devices.get(i).is_some_and(|d| self.selected.as_deref() == Some(d.path.as_str())),
            _ => false,
        }
    }

    /// A code question first takes the key: Enter replies OK (the action's deadline runs again),
    /// any other key rejects it and ends the pairing silently. The code question is not a
    /// forget-style question: it has no 0.4 s wait and no 3 s expiry. Otherwise: Escape or ← back to
    /// the rows; ↑ ↓ Home End over the header and the devices, never wrapping; Space on the header
    /// powers the adapter; Enter acts on the device (or confirms its forget question); Delete asks
    /// to forget it. Any key but Enter drops the forget question.
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
        // The header is entry 0 and the devices follow, so a selected device at index `i` is entry
        // `i + 1`; no selection (or one no longer listed) is the header.
        let mut at = 0;
        if let Some(p) = &self.selected
            && let Some(i) = devices.iter().position(|d| d == p)
        {
            at = i + 1;
        }
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
        let target = if to == 0 { None } else { devices.get(to - 1).cloned() };
        moved(&mut self.selected, target)
    }

    /// A press on a detail line: a left one on the header goes back; on a device it selects it then
    /// is Enter (the forget question's second activation there); a right one selects it and asks to
    /// forget it. While a code question shows, only the header answers. The forget question goes
    /// with a press anywhere but its own device.
    pub fn press(&mut self, line: Line, b: Button, confirm: &mut Option<(Question, Instant)>, now: Instant) -> QuickAction {
        let device = match line {
            Line::BtDevice(i) => self.devices().get(i).map(|d| d.path.clone()),
            _ => None,
        };
        // The forget question stays only on a press on its own device.
        let on_question = match (confirm.as_ref(), &device) {
            (Some((Question::Forget(q), _)), Some(p)) => q == p,
            _ => false,
        };
        if !on_question {
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
    /// computed once per pass. `selected` is whether the line is the selected one (its band).
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

/// Pushes the line of the device at index `i` of `devices()`, and under it the code's line when
/// that device is the one whose code is shown (`shown`).
fn push_device(v: &mut Vec<Line>, i: usize, shown: Option<usize>) {
    v.push(Line::BtDevice(i));
    if shown == Some(i) {
        v.push(Line::BtCode);
    }
}

impl Bt {
    /// A device's tag, the first that applies (spec §§ 2–3): its forget question, its code
    /// question, its action running, its failure, paired.
    fn tag(&self, d: &BtEntry, confirm: Option<&(Question, Instant)>) -> (String, Style) {
        // Whether the object path `p` is this device's.
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

/// One device's row, as "● 󰋋 Headset" or "  󰌌 Keyboard      paired" (spec § 2): ● on a connected one
/// in the accent, the kind's glyph, the label cut to fit (`?` when empty), the tag right-aligned
/// one cell from the end. Selected: the band across the whole row.
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
