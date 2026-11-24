//! The quick panel's acting Bluetooth connection, on the event loop: its whole life.
//!
//! This is the wiring half of the Bluetooth list; the model is `bt.rs`. The acting connection
//! (`sources/dbus/bt_act.rs`) is opened when the detail opens or a call goes out with an adapter
//! seen, as one level-triggered fd source while it lives, and closed (its agent unregistered first)
//! once the panel no longer wants it: the detail left with no call in flight, or the panel gone.
//! When BlueZ goes or its owner changes, the connection is let go without a word (`bt_drop`):
//! nothing is ever sent to an old unique name, the panel's action ends silently, and the next need
//! opens a new one on the owner then there.
//!
//! Never:
//! - reach BlueZ except through that connection: it is named only here, in `quick/app.rs` (for its
//!   field) and in `bt_act.rs` (guardrail `only_bt_app_and_bt_act_name_btact`), and what it can call
//!   is a pinned list (guardrail `the_bluetooth_connection_acts_only_through_act_method`);
//! - keep the connection beyond the panel's need, or unregister the agent from a BlueZ that went
//!   (guardrail `the_bluetooth_connection_goes_with_the_panel`: it reads `bt_close`, `bt_drop`,
//!   `bt_let_go`, `bt_readable` and `bt_run` here, and the panel's open, flush, tick and close paths
//!   in `quick/app.rs`; `bt_let_go` removes the source without closing);
//! - run a command or start a thread (guardrail `the_tray_spawns_no_thread`, through its
//!   `NO_THREAD_NOR_COMMAND` list), or arm a timer (the table of timer and thread tokens in
//!   `nothing_resident_while_the_launcher_is_closed`, where this file's one entry is the
//!   connection's fd source): the panel's one timer covers the deadlines;
//! - put a device in a log line (guardrail `the_bluetooth_logs_name_no_device`): a line names an
//!   action and a D-Bus error name, because BlueZ's object paths embed the device's address, and
//!   the only thing formatted for a log with `Debug` is an `io::ErrorKind`;
//! - panic (guardrail `the_quick_panel_has_no_panicking_path`).
//!
//! See: docs/features/quick-settings.md and docs/decisions.md ("The Bluetooth acting connection opens
//! with one blocking connect and asks for BlueZ's owner itself", "Bluetooth deadlines live in the
//! panel, and a late answer is forgotten").

use crate::log::write as log;
use crate::popup::PopupBody;
use crate::quick::app::bt_scope_for;
use crate::quick::bt::{Bt, BtOrder};
use crate::quick::{self, Quick, QuickAction};
use crate::sources::dbus::bt_act::{ActCall, ActError, AgentReply, AgentRequest, BtAct, BtEvent};
use crate::wayland::App;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode as Trigger, PostAction};
use std::time::Instant;

/// The error a call is answered with when no acting connection could be opened: the action shows
/// `failed`.
pub(crate) const NO_SERVER: &str = "org.freedesktop.DBus.Error.NoServer";

/// Whether the acting connection is wanted: a panel, and its detail open, an action or a
/// code running, or a call still in flight (kept for its reply). Never without a panel: closing it
/// sent its leaving orders first.
pub(crate) fn connection_wanted(q: Option<&Quick>, in_flight: usize) -> bool {
    q.is_some_and(|q| q.bt.wants_connection() || in_flight > 0)
}

/// Whether the panel's view holds an adapter: BlueZ is there to be reached.
pub(crate) fn adapter_seen(q: &Quick) -> bool {
    q.bt.view.as_ref().is_some_and(|v| v.adapter.is_some())
}

/// Whether these orders open a connection: a call among them, with an adapter seen. A reply or a
/// forget never does: it belongs to the connection it came from; nor does a leaving call
/// (`StopDiscovery`, `CancelPairing`): it undoes what a connection did, and with none there is
/// nothing to undo.
pub(crate) fn opens_for(orders: &[BtOrder], adapter_seen: bool) -> bool {
    let opening = |o: &BtOrder| match o {
        BtOrder::Call(ActCall::StopDiscovery { .. } | ActCall::CancelPairing { .. }) | BtOrder::Reply(..) | BtOrder::Forget(_) => false,
        BtOrder::Call(_) => true,
    };
    adapter_seen && orders.iter().any(opening)
}

/// Whether an answer says the connection's owner is stale: `Gone` on a connection still open means
/// BlueZ left or was replaced without the connection seeing it close (a hand-over with no gap, a
/// `NoReply` recheck finding another owner). The connection is then let go, so the next need opens
/// one on the owner there then.
pub(crate) fn stale_owner(result: &Result<(), ActError>) -> bool {
    matches!(result, Err(ActError::Gone))
}

/// The reply to an agent request nobody can answer: with no panel, rejected at once (BlueZ's
/// `Cancel` and `Release` were already acknowledged by the connection, so they get none); None: the
/// panel answers it.
pub(crate) fn unanswered(panel_shown: bool, request: &AgentRequest) -> Option<AgentReply> {
    if panel_shown {
        return None;
    }
    match request {
        AgentRequest::Cancel | AgentRequest::Release => None,
        _ => Some(AgentReply::Rejected),
    }
}

/// The panel's own record (`Quick::note_failure`) that no acting connection could be opened this
/// open: it is logged once, and the panel's syncs stop retrying (an action still tries, and shows
/// `failed`).
const REFUSED: &str = "bluetooth connection";

/// The panel's record that the connection already closed under us once this open.
const LOST: &str = "bluetooth connection lost";

/// The connection closed under us with BlueZ still there (a bus restart, a refusal): the syncs
/// reopen it once per panel open; a second loss stops them (`REFUSED`), so a bus that keeps closing
/// it is not asked again on every bar message. A user's action, or the next open, still tries.
pub(crate) fn note_lost(q: &mut Quick) {
    if !q.note_failure(LOST) {
        q.note_failure(REFUSED);
    }
}

/// Whether the panel's sync opens the connection: wanted, an adapter seen, and not already refused
/// this open (a sync runs on every bar message: a refusing bus is not asked again each time).
pub(crate) fn opens_on_sync(q: Option<&Quick>, in_flight: usize) -> bool {
    connection_wanted(q, in_flight) && q.is_some_and(|q| adapter_seen(q) && !q.failed.iter().any(|p| p == REFUSED))
}

/// What a sync does to the acting connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncStep {
    Keep,
    Open,
    Close,
    /// BlueZ gone by the bar's own reading (its row hidden): let go without a word (`bt_drop`).
    Drop,
}

/// The sync's step: with a connection (`connected`), dropped once the row hides (BlueZ gone, the
/// domain out: whichever of the bar's state and the panel's view says it first), closed once no
/// longer wanted; without one, opened by `opens_on_sync` while the row shows. The `match` below
/// reads the first arm that fits, top to bottom.
pub(crate) fn sync_step(q: Option<&Quick>, row_shown: bool, connected: bool, in_flight: usize) -> SyncStep {
    match connected {
        true if !row_shown => SyncStep::Drop,
        true if !connection_wanted(q, in_flight) => SyncStep::Close,
        false if row_shown && opens_on_sync(q, in_flight) => SyncStep::Open,
        _ => SyncStep::Keep,
    }
}

/// The open quick panel, if any.
fn panel(app: &App) -> Option<&Quick> {
    match app.popup_body() {
        Some(PopupBody::Quick(q)) => Some(q),
        _ => None,
    }
}

impl App {
    /// Opens the acting connection and its fd source; on any failure one log line, and false.
    fn bt_open(&mut self) -> bool {
        let opened = BtAct::system().and_then(|mut act| act.watch_fd().map(|fd| (act, fd)));
        let (act, fd) = match opened {
            Ok(o) => o,
            Err(e) => {
                self.bt_refused(&format!("{:?}", e.kind()));
                return false;
            }
        };
        // Level-triggered: `read` drains libdbus to its last message, so it never wakes the loop at
        // rest. `inserted` is None without an event loop handle, and Some of the insertion's result
        // otherwise.
        let inserted = self.loop_handle.as_ref().map(|h| {
            h.insert_source(Generic::new(fd, Interest::READ, Trigger::Level), |_, _, app: &mut App| {
                app.bt_readable();
                Ok(PostAction::Continue)
            })
        });
        match inserted {
            Some(Ok(token)) => {
                self.quick.bt_act = Some(act);
                self.quick.bt_source = Some(token);
                true
            }
            _ => {
                self.bt_refused("event loop refused the source");
                false
            }
        }
    }

    /// No acting connection: one line per open (`REFUSED`), or with no panel.
    fn bt_refused(&mut self, why: &str) {
        let first = match self.popup_body_mut() {
            Some(PopupBody::Quick(q)) => q.note_failure(REFUSED),
            _ => true,
        };
        if first {
            log(&format!("quick: no bluetooth connection ({why})"));
        }
    }

    /// The panel's orders on the acting connection, opened for a call when BlueZ has an adapter;
    /// written, then what libdbus read meanwhile delivered. With no connection to be had, each call
    /// is answered NO_SERVER (its action shows `failed`); the other orders are dropped.
    pub(crate) fn bt_run(&mut self, orders: Vec<BtOrder>) {
        if orders.is_empty() {
            return;
        }
        if self.quick.bt_act.is_none() && opens_for(&orders, panel(self).is_some_and(adapter_seen)) && !self.bt_open() {
            let now = Instant::now();
            for o in orders {
                if let BtOrder::Call(call) = o {
                    self.bt_answer(&call, Err(ActError::Remote(NO_SERVER.into())), now);
                }
            }
            return;
        }
        let Some(act) = self.quick.bt_act.as_mut() else { return };
        for o in orders {
            match o {
                BtOrder::Call(call) => act.call(call),
                BtOrder::Reply(id, reply) => act.reply(id, reply),
                BtOrder::Forget(call) => act.forget(&call),
            }
        }
        act.flush();
        self.bt_readable();
    }

    /// `call`'s answer to the open panel: its line logged, its action taken.
    fn bt_answer(&mut self, call: &ActCall, result: Result<(), ActError>, now: Instant) {
        let action = self.bt_result(call, result, now);
        self.quick_action(action);
    }

    /// `call`'s answer given to the open panel, its line logged; the action is the caller's to take.
    fn bt_result(&mut self, call: &ActCall, result: Result<(), ActError>, now: Instant) -> QuickAction {
        let mut line = None;
        let action = self.quick_with(|q, _| {
            let (a, l) = q.bt_answered(call, result, now);
            line = l;
            a
        });
        if let Some(line) = line {
            log(&line);
        }
        action
    }

    /// The connection went under us: counted (`note_lost`, the syncs' bounded reopening).
    fn bt_note_lost(&mut self) {
        if let Some(PopupBody::Quick(q)) = self.popup_body_mut() {
            note_lost(q);
        }
    }

    /// The connection's fd is readable (or a flush read for it): every event of a batch handled in
    /// order, the actions they produce taken only after the batch (an action may run orders, which
    /// read again; nested, newer events would be handled before older ones), until a read brings
    /// nothing more; then the tap and the connection follow the panel. Once the connection goes
    /// mid-batch, the rest of the batch was its own: dropped.
    pub(crate) fn bt_readable(&mut self) {
        let now = Instant::now();
        // `map` reads from the connection if there is one, `filter` keeps a batch that is not empty:
        // the loop ends when the connection is gone or a read brings nothing.
        while let Some(events) = self.quick.bt_act.as_mut().map(BtAct::read).filter(|e| !e.is_empty()) {
            let mut later = Vec::new();
            // A lost connection answers its calls `Gone`, then `Closed`: that one is let go there.
            let closing = events.iter().any(|e| matches!(e, BtEvent::Closed));
            for event in events {
                if self.quick.bt_act.is_none() {
                    break;
                }
                match event {
                    BtEvent::Answer { call, result } => {
                        let stale = !closing && stale_owner(&result);
                        later.push(self.bt_result(&call, result, now));
                        if stale {
                            self.bt_note_lost();
                            later.push(self.bt_let_go());
                        }
                    }
                    BtEvent::Agent { id, request } => match unanswered(self.quick_shown(), &request) {
                        Some(reply) => {
                            if let Some(act) = self.quick.bt_act.as_mut() {
                                act.reply(id, reply);
                                act.flush();
                            }
                        }
                        None => later.push(self.quick_with(|q, _| q.bt_agent(id, request, now))),
                    },
                    BtEvent::Closed => {
                        log("quick: bluetooth connection lost");
                        self.bt_note_lost();
                        later.push(self.bt_let_go());
                    }
                }
            }
            for action in later {
                self.quick_action(action);
            }
        }
        self.quick_bt_sync();
    }

    /// The D-Bus thread reads what the open panel shows of Bluetooth (`bt_scope_for`); the acting
    /// connection opens while it is wanted and BlueZ has an adapter, and closes once it is not.
    pub(crate) fn quick_bt_sync(&mut self) {
        let in_flight = self.quick.bt_act.as_ref().map_or(0, BtAct::in_flight);
        let (scope, step) = {
            let q = panel(self);
            let row_shown = Bt::shows(quick::live(&self.bar, &self.dnd_timed.model).bluetooth);
            (bt_scope_for(q, row_shown), sync_step(q, row_shown, self.quick.bt_act.is_some(), in_flight))
        };
        if let Some(tap) = &self.quick.bt_tap {
            tap.set(scope);
        }
        match step {
            SyncStep::Keep => {}
            SyncStep::Open => {
                self.bt_open();
            }
            SyncStep::Close => self.bt_close(),
            SyncStep::Drop => self.bt_drop(),
        }
    }

    /// The connection closed in order: its source removed, then the held agent calls rejected, the
    /// agent unregistered, what is queued written (`BtAct::close`).
    pub(crate) fn bt_close(&mut self) {
        if let (Some(token), Some(handle)) = (self.quick.bt_source.take(), self.loop_handle.as_ref()) {
            handle.remove(token);
        }
        if let Some(act) = self.quick.bt_act.take() {
            act.close();
        }
    }

    /// The connection let go without a word: BlueZ gone or replaced, or the
    /// connection closed under us. Nothing is sent, so no `UnregisterAgent` reaches an old owner;
    /// the panel's action, whose answer can no longer come, ends silently (`Gone`).
    pub(crate) fn bt_drop(&mut self) {
        let action = self.bt_let_go();
        self.quick_action(action);
    }

    /// `bt_drop` but for its action, the caller's to take (`bt_readable` takes it after its batch).
    fn bt_let_go(&mut self) -> QuickAction {
        if let (Some(token), Some(handle)) = (self.quick.bt_source.take(), self.loop_handle.as_ref()) {
            handle.remove(token);
        }
        if self.quick.bt_act.take().is_none() {
            return QuickAction::Nothing;
        }
        let call = panel(self).and_then(|q| q.bt.action.as_ref().map(|a| a.call.clone()));
        match call {
            Some(call) => self.bt_result(&call, Err(ActError::Gone), Instant::now()),
            None => QuickAction::Nothing,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::BarConfig;
    use crate::quick::{self, Focus};
    use crate::sources::dbus::bt_act::AgentId;
    use crate::state::{Bar, Msg};
    use crate::system::{BtAdapter, BtEntry, BtKind, BtState, BtView};
    use std::time::Instant;

    pub(crate) const HCI0: &str = "/org/bluez/hci0";
    pub(crate) const CASQUE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";

    pub(crate) fn bar_bt_on() -> Bar {
        let mut bar = Bar::new(BarConfig::default());
        bar.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![] })));
        bar
    }

    pub(crate) fn bt_view() -> BtView {
        let casque =
            BtEntry { path: CASQUE.into(), label: "CasqueBT".into(), kind: BtKind::Headset, paired: true, trusted: true, connected: false };
        BtView { adapter: Some(BtAdapter { path: HCI0.into(), powered: true, blocked: false }), paired: vec![casque], nearby: vec![] }
    }

    #[test]
    fn the_connection_is_wanted_while_the_detail_is_open_an_action_runs_or_a_call_is_in_flight() {
        let (t, bar) = (Instant::now(), bar_bt_on());
        let live = quick::live(&bar, &crate::notify::dnd::Dnd::default());
        let mut q = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
        assert!(!connection_wanted(None, 0) && !connection_wanted(None, 2), "no panel: never (leaving was sent first)");
        q.follow_bt(&live, Some(bt_view()), t);
        assert!(!connection_wanted(Some(&q), 0), "the rows only");
        q.bt.open_detail();
        assert!(connection_wanted(Some(&q), 0), "the detail open");
        q.bt.close_detail();
        assert!(connection_wanted(Some(&q), 1), "left with a call in flight: kept for its reply");
        assert!(!connection_wanted(Some(&q), 0), "left, nothing in flight");
        let off = ActCall::Powered { adapter: HCI0.into(), on: false };
        assert_eq!(q.bt.power(&BtState::On { devices: vec![] }), QuickAction::Bluetooth(vec![BtOrder::Call(off.clone())]));
        assert!(connection_wanted(Some(&q), 0), "a power toggle from the row: until its reply");
        q.bt_answered(&off, Ok(()), t);
        assert!(!connection_wanted(Some(&q), 0), "its reply came: closed");
    }

    #[test]
    fn only_a_call_with_an_adapter_seen_opens_the_connection() {
        let call = BtOrder::Call(ActCall::Connect { device: CASQUE.into() });
        assert!(opens_for(std::slice::from_ref(&call), true));
        assert!(!opens_for(std::slice::from_ref(&call), false), "BlueZ gone: nothing to reach");
        let reply = BtOrder::Reply(AgentId(7), AgentReply::Rejected);
        let forget = BtOrder::Forget(ActCall::Connect { device: CASQUE.into() });
        assert!(!opens_for(&[reply, forget], true), "a reply or a forget needs the connection it came from");
        assert!(!opens_for(&[], true));
        // A leaving order undoes what a connection did; with none, nothing to undo.
        let stop = BtOrder::Call(ActCall::StopDiscovery { adapter: HCI0.into() });
        let cancel = BtOrder::Call(ActCall::CancelPairing { device: CASQUE.into() });
        assert!(!opens_for(&[stop.clone(), cancel.clone()], true), "a leaving order never opens one");
        assert!(opens_for(&[stop, cancel, call], true), "another call among them does");
    }

    /// A `Gone` on a live connection means its owner is stale; any other answer keeps it.
    #[test]
    fn only_a_gone_answer_marks_the_owner_stale() {
        assert!(stale_owner(&Err(ActError::Gone)));
        assert!(!stale_owner(&Ok(())));
        assert!(!stale_owner(&Err(ActError::Remote(NO_SERVER.into()))), "a remote error: BlueZ is there");
    }

    #[test]
    fn an_agent_call_with_no_panel_is_rejected() {
        let ask = AgentRequest::RequestConfirmation { device: CASQUE.into(), passkey: 482_913 };
        assert_eq!(unanswered(false, &ask), Some(AgentReply::Rejected));
        assert_eq!(unanswered(true, &ask), None, "the panel answers");
        for done in [AgentRequest::Cancel, AgentRequest::Release] {
            assert_eq!(unanswered(false, &done), None, "already replied by BtAct");
        }
    }

    /// A refusing bus is asked once per open by the syncs, never on every bar message.
    #[test]
    fn a_refused_connection_is_not_reopened_by_the_syncs() {
        let (t, bar) = (Instant::now(), bar_bt_on());
        let live = quick::live(&bar, &crate::notify::dnd::Dnd::default());
        let mut q = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
        q.bt.open_detail();
        assert!(!opens_on_sync(Some(&q), 0), "no adapter seen yet");
        q.follow_bt(&live, Some(bt_view()), t);
        assert!(opens_on_sync(Some(&q), 0), "the detail open, BlueZ there");
        assert!(q.note_failure(REFUSED), "refused: noted once");
        assert!(!opens_on_sync(Some(&q), 0), "not asked again this open");
        assert!(!opens_on_sync(None, 1), "never without a panel");
    }

    /// The bar's row hiding (BlueZ gone) drops the connection without a word, even
    /// when the panel's view has not said so yet; otherwise it closes once no longer wanted.
    #[test]
    fn a_sync_drops_closes_opens_or_keeps_the_connection() {
        let (t, bar) = (Instant::now(), bar_bt_on());
        let live = quick::live(&bar, &crate::notify::dnd::Dnd::default());
        let mut q = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
        q.follow_bt(&live, Some(bt_view()), t);
        assert_eq!(sync_step(Some(&q), true, false, 0), SyncStep::Keep, "the rows only: none");
        q.bt.open_detail();
        assert_eq!(sync_step(Some(&q), true, false, 0), SyncStep::Open);
        assert_eq!(sync_step(Some(&q), true, true, 0), SyncStep::Keep);
        assert_eq!(sync_step(Some(&q), false, true, 0), SyncStep::Drop, "the row hid: BlueZ gone");
        assert_eq!(sync_step(Some(&q), false, false, 0), SyncStep::Keep, "never opened while the row is hidden");
        q.bt.close_detail();
        assert_eq!(sync_step(Some(&q), true, true, 1), SyncStep::Keep, "a call in flight");
        assert_eq!(sync_step(Some(&q), true, true, 0), SyncStep::Close);
        assert_eq!(sync_step(None, true, true, 0), SyncStep::Close, "the panel gone");
        assert_eq!(sync_step(None, false, true, 0), SyncStep::Drop);
    }

    /// One unexpected close is reopened by the syncs, a second within the same open is
    /// not.
    #[test]
    fn a_second_loss_stops_the_syncs_reopening() {
        let (t, bar) = (Instant::now(), bar_bt_on());
        let live = quick::live(&bar, &crate::notify::dnd::Dnd::default());
        let mut q = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
        q.follow_bt(&live, Some(bt_view()), t);
        q.bt.open_detail();
        note_lost(&mut q);
        assert_eq!(sync_step(Some(&q), true, false, 0), SyncStep::Open, "lost once: reopened");
        note_lost(&mut q);
        assert_eq!(sync_step(Some(&q), true, false, 0), SyncStep::Keep, "lost twice: left closed");
        let call = [BtOrder::Call(ActCall::Connect { device: CASQUE.into() })];
        assert!(opens_for(&call, adapter_seen(&q)), "an action still tries");
    }

    #[test]
    fn adapter_seen_needs_an_adapter_in_the_view() {
        let (t, bar) = (Instant::now(), bar_bt_on());
        let live = quick::live(&bar, &crate::notify::dnd::Dnd::default());
        let mut q = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
        assert!(!adapter_seen(&q), "nothing read yet");
        q.follow_bt(&live, Some(BtView { adapter: None, ..bt_view() }), t);
        assert!(!adapter_seen(&q), "BlueZ without an adapter");
        q.follow_bt(&live, Some(bt_view()), t);
        assert!(adapter_seen(&q));
    }
}
