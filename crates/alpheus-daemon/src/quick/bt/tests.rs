//! Tests of the quick panel's Bluetooth model (`bt.rs`): the row, the detail's devices, the keys and
//! clicks, the actions and their deadlines, the pairing codes and the forget question. No clock and
//! no D-Bus: each test takes one `Instant` as its start, adds the delays it wants and answers the
//! calls itself.

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
fn the_row_reads_its_value_in_every_state() {
    assert!(!Bt::shows(None), "the bar's state not read yet");
    assert!(!Bt::shows(Some(&BtState::Absent)));
    assert!(Bt::shows(Some(&BtState::Off)));
    assert!(Bt::shows(Some(&BtState::On { devices: vec![] })));
    let b = Bt::default();
    let both = |s: &BtState| (b.value(s), b.glyph(s));
    assert_eq!(both(&BtState::Off), ("off".into(), BT_OFF));
    assert_eq!(both(&BtState::On { devices: vec![] }), ("on".into(), BLUETOOTH));
    let two = BtState::On {
        devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(80) }, BtDevice { name: "ClavierBT".into(), battery: None }],
    };
    assert_eq!(both(&two), ("CasqueBT".into(), BLUETOOTH_CONNECTED), "the first, as the bar orders them");
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
fn space_powers_the_adapter_or_unblocks_a_blocked_radio_first() {
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
fn the_detail_lists_the_paired_devices_then_the_nearby_ones() {
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

/// A hostile alias fits its row; an empty one reads `?`.
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
fn the_detail_lists_at_most_32_rows() {
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
fn each_kind_of_device_has_its_own_glyph() {
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
fn the_selection_is_kept_by_path_when_a_row_is_inserted_above() {
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
fn an_adapter_that_is_off_shows_the_note_bluetooth_is_off() {
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
fn enter_asks_for_the_call_that_fits_the_devices_state() {
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
fn a_pairing_is_a_pair_then_a_trust_then_a_connect() {
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
/// first, as any other end of the action does (`finish`).
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
fn a_pairing_that_already_connected_the_device_skips_the_connect() {
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
fn an_already_connected_answer_to_a_connect_is_a_success() {
    let t = Instant::now();
    let (mut b, _) = acting(2, t);
    assert_eq!(
        b.answered(&ActCall::Connect { device: path(2) }, remote("org.bluez.Error.AlreadyConnected"), t),
        (QuickAction::Redraw, None)
    );
    assert_eq!((b.action.clone(), b.failed.clone()), (None, None));
}

#[test]
fn only_one_action_runs_at_a_time() {
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
fn a_failure_shows_failed_for_4_seconds_and_logs_the_error_name() {
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
fn each_call_has_a_30_second_deadline_suspended_while_a_code_shows() {
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
fn a_gone_connection_object_device_or_adapter_ends_the_action_silently() {
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
    // The adapter powered off elsewhere mid-pairing.
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
fn the_forget_question_waits_expires_and_is_cancelled_by_other_input() {
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
fn each_agent_request_gets_its_state_and_its_reply() {
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
fn a_repeated_passkey_shows_on_one_line() {
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
fn an_agent_request_for_another_device_is_rejected() {
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
    assert_eq!(b.agent(AgentId(1), AgentRequest::DisplayPasskey { device: path(2), passkey: 1 }, t), reject, "a connect is not a pairing");
    assert_eq!(b.code, None);
}

/// Closing during `Code 482 913? ⏎`: Rejected, CancelPairing, then discovery stops.
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
fn no_log_line_names_a_device() {
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

/// A forget question the panel's timer has not cleared yet, but whose 3 s are over, no longer
/// counts: Enter acts on the device as it always does.
#[test]
fn an_expired_forget_question_still_held_leaves_enter_to_act_on_the_device() {
    let t = Instant::now();
    let mut b = opened();
    let mut confirm = None;
    b.selected = Some(path(5));
    b.key(Key::Delete, &mut confirm, t);
    assert!(confirm.is_some());
    assert_eq!(b.key(Key::Enter, &mut confirm, t + CONFIRM + SEC), call(ActCall::Connect { device: path(5) }));
    assert_eq!(confirm, None);
}

#[test]
fn a_forget_question_about_another_device_is_not_confirmed_by_enter() {
    let t = Instant::now();
    let mut b = opened();
    b.selected = Some(path(5));
    let mut confirm = Some((Question::Forget(path(2)), t + CONFIRM));
    assert_eq!(b.key(Key::Enter, &mut confirm, t + CONFIRM_AFTER), call(ActCall::Connect { device: path(5) }));
    assert_eq!(confirm, None);
}

#[test]
fn an_answer_to_another_call_leaves_the_running_action_alone() {
    let t = Instant::now();
    let (mut b, _) = acting(2, t);
    let discovery = ActCall::StartDiscovery { adapter: ADAPTER.into() };
    assert_eq!(
        b.answered(&discovery, remote("org.bluez.Error.Failed"), t),
        (QuickAction::Nothing, Some("quick: bluetooth discovery failed (org.bluez.Error.Failed)".into()))
    );
    assert_eq!(step(&b), Some(Step::Connect), "the connect is still waiting for its own answer");
}

#[test]
fn the_next_deadline_is_the_earliest_of_the_action_and_the_failed_tag() {
    let t = Instant::now();
    let (mut b, _) = acting(2, t);
    b.failed = Some((path(3), t + 4 * SEC));
    assert_eq!(b.deadline(), Some(t + 4 * SEC), "the failed tag ends first");
    b.failed = Some((path(3), t + 60 * SEC));
    assert_eq!(b.deadline(), Some(t + 30 * SEC), "the action's deadline comes first");
}

#[test]
fn an_action_on_a_listed_device_ends_silently_when_the_adapter_goes_off() {
    let t = Instant::now();
    let (mut b, _) = acting(2, t);
    assert_eq!(b.set_view(Some(view(false, paired(), vec![])), t), bt(vec![BtOrder::Forget(ActCall::Connect { device: path(2) })]));
    assert_eq!((b.action.clone(), b.failed.clone()), (None, None));
}

#[test]
fn a_view_that_lists_the_pairing_device_only_as_nearby_keeps_the_action() {
    let t = Instant::now();
    let (mut b, _) = acting(3, t);
    assert_eq!(b.set_view(Some(workstation()), t), QuickAction::Redraw);
    assert_eq!(step(&b), Some(Step::Pair));
}

#[test]
fn a_right_press_on_another_device_moves_the_forget_question_to_it() {
    let t = Instant::now();
    let mut b = opened();
    let mut confirm = None;
    b.selected = Some(path(2));
    b.key(Key::Delete, &mut confirm, t);
    assert_eq!(b.press(Line::BtDevice(1), Button::Right, &mut confirm, t), QuickAction::Redraw);
    assert_eq!(confirm, Some((Question::Forget(path(5)), t + CONFIRM)), "asked about the device pressed, not the first one");
    assert_eq!(b.selected, Some(path(5)));
}

#[test]
fn arming_leaves_the_deadline_off_while_a_pairing_code_shows() {
    let t = Instant::now();
    let (mut b, _) = acting(3, t);
    b.agent(AgentId(1), AgentRequest::DisplayPasskey { device: path(3), passkey: 482_913 }, t);
    b.arm(t + SEC);
    assert_eq!(b.deadline(), None);
}
