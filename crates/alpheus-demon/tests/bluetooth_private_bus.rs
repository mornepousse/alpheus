//! The D-Bus thread's Bluetooth panel reads against a PRIVATE dbus-daemon started by the test, its
//! socket in a temporary directory: never the session bus, never the system bus.
//! `nix develop -c cargo test -p alpheus-demon --test bluetooth_private_bus -- --ignored`

mod common;

use alpheus_demon::sources::dbus::bt::BtTap;
use alpheus_demon::sources::dbus::bt_act::{AGENT_PATH, ActCall, ActError, AgentId, AgentReply, AgentRequest, BtAct, BtEvent};
use alpheus_demon::sources::dbus::dbusrs_bus::DbusRsBus;
use alpheus_demon::sources::dbus::vpn::VpnTap;
use alpheus_demon::sources::dbus::wifi::WifiTap;
use alpheus_demon::sources::dbus::{Domain, DomainLog};
use alpheus_demon::system::{BtEntry, BtScope, BtState, BtView};
use common::bluez::FakeBluez;
use common::{Peer, PrivateBus};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

type Seen = (Option<BtState>, Option<BtView>);

fn follow(b: &PrivateBus, tap: &Arc<BtTap>) -> mpsc::Receiver<Seen> {
    let mut bus = DbusRsBus::at(&b.address, &[Domain::Bluetooth]).unwrap().waking(tap.clone());
    let (tx, rx) = mpsc::channel();
    let t = tap.clone();
    std::thread::spawn(move || {
        let _ = common::session_plain(
            &mut bus,
            &[Domain::Bluetooth],
            &mut DomainLog::default(),
            &WifiTap::default(),
            &t,
            &VpnTap::default(),
            &mut |s| tx.send((s.bluetooth, s.bt)).is_ok(),
        );
    });
    rx
}

fn next(rx: &mpsc::Receiver<Seen>) -> Seen {
    rx.recv_timeout(Duration::from_secs(5)).expect("a publication")
}

fn names(v: &Option<BtView>) -> (Vec<String>, Vec<String>) {
    let l = |e: &[BtEntry]| e.iter().map(|e| e.label.clone()).collect();
    v.as_ref().map(|v| (l(&v.paired), l(&v.nearby))).unwrap_or_default()
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn bt_detail_follows_devices_only_while_open() {
    let b = PrivateBus::start("bt-detail");
    let bz = FakeBluez::serve(&b);
    let tap = Arc::new(BtTap::new().unwrap());
    let rx = follow(&b, &tap);
    let on = Some(BtState::On { devices: vec![] });
    assert_eq!(next(&rx), (on.clone(), None), "closed: no view");
    tap.set(BtScope::Row);
    assert_eq!(names(&next(&rx).1), (vec![], vec![]), "the row: the adapter only");
    bz.add_nearby(5, "Keyboard-Test", -50);
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "the row follows no device");
    assert!(!bz.calls().iter().any(|c| c.starts_with("GetAll") && c.contains("dev_")), "{:?}", bz.calls());
    tap.set(BtScope::Detail);
    let (bar, view) = next(&rx);
    assert_eq!((bar, names(&view)), (on.clone(), (vec!["CasqueBT".into()], vec!["Keyboard-Test".into()])));
    bz.add_nearby(6, "Speaker-X", -40);
    let (bar, view) = next(&rx);
    assert_eq!(names(&view).1, ["Speaker-X", "Keyboard-Test"], "inserted by its RSSI, the first never moved");
    assert_eq!(bar, on, "the bar's state unchanged");
}

#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn the_bt_tap_wakes_the_thread() {
    let b = PrivateBus::start("bt-wake");
    let _bz = FakeBluez::serve(&b);
    let tap = Arc::new(BtTap::new().unwrap());
    let rx = follow(&b, &tap);
    next(&rx);
    let t = Instant::now();
    assert!(tap.set(BtScope::Detail));
    let (_, view) = rx.recv_timeout(Duration::from_secs(1)).expect("woken by the tap");
    assert!(view.is_some(), "the detail read");
    assert!(t.elapsed() < Duration::from_secs(1));
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "asleep again");
}

/// The read side: BlueZ restarting (a new unique name) with the detail open is read
/// again on the new owner, not device by device through NotFound on the old one.
#[test]
#[ignore = "dbus-daemon (nix develop)"]
fn bluez_restarting_reads_the_new_owner() {
    let b = PrivateBus::start("bt-restart");
    let bz = FakeBluez::serve(&b);
    let tap = Arc::new(BtTap::new().unwrap());
    let rx = follow(&b, &tap);
    next(&rx);
    tap.set(BtScope::Detail);
    let (_, view) = next(&rx);
    assert_eq!(names(&view).0, ["CasqueBT"]);
    let old = bz.unique_name();
    drop(bz);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let (bar, view) = rx.recv_timeout(left).expect("BlueZ gone seen");
        if bar == Some(BtState::Absent) && view == Some(BtView::default()) {
            break;
        }
    }
    let bz = FakeBluez::serve(&b);
    assert_ne!(bz.unique_name(), old);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let (_, view) = rx.recv_timeout(left).expect("the new owner read");
        if view.as_ref().is_some_and(|v| v.adapter.is_some()) {
            assert_eq!(names(&view).0, ["CasqueBT"]);
            break;
        }
    }
    assert!(bz.calls().iter().any(|c| c == "GetManagedObjects /"), "{:?}", bz.calls());
}

/// Reads `act` every 5 ms until `n` events came or `timeout` passed.
fn pump(act: &mut BtAct, n: usize, timeout: Duration) -> Vec<BtEvent> {
    let end = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < end {
        out.extend(act.read());
        std::thread::sleep(Duration::from_millis(5));
    }
    out
}

fn dev(n: u8) -> String {
    format!("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_{n:02X}")
}

fn ok(call: ActCall) -> BtEvent {
    BtEvent::Answer { call, result: Ok(()) }
}

const HCI: &str = "/org/bluez/hci0";
const REGISTER: &str = "RegisterAgent /org/bluez /org/alpheus/bluetooth/agent KeyboardDisplay";

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn each_call_reaches_the_fake_with_its_arguments() {
    let b = PrivateBus::start("bt-act-calls");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    let calls = [
        ActCall::Powered { adapter: HCI.into(), on: false },
        ActCall::Trusted { device: dev(2) },
        ActCall::StartDiscovery { adapter: HCI.into() },
        ActCall::StopDiscovery { adapter: HCI.into() },
        ActCall::RemoveDevice { adapter: HCI.into(), device: dev(2) },
        ActCall::Connect { device: dev(1) },
        ActCall::Disconnect { device: dev(1) },
        ActCall::Pair { device: dev(2) },
        ActCall::CancelPairing { device: dev(2) },
    ];
    for c in calls.clone() {
        act.call(c);
    }
    act.flush();
    assert_eq!(pump(&mut act, 9, Duration::from_secs(5)), calls.map(ok));
    assert_eq!(
        bz.calls(),
        [
            REGISTER,
            "Set /org/bluez/hci0 org.bluez.Adapter1 Powered false",
            "Set /org/bluez/hci0/dev_AA_BB_CC_DD_EE_02 org.bluez.Device1 Trusted true",
            "StartDiscovery /org/bluez/hci0",
            "StopDiscovery /org/bluez/hci0",
            "RemoveDevice /org/bluez/hci0 /org/bluez/hci0/dev_AA_BB_CC_DD_EE_02",
            "Connect /org/bluez/hci0/dev_AA_BB_CC_DD_EE_01",
            "Disconnect /org/bluez/hci0/dev_AA_BB_CC_DD_EE_01",
            "Pair /org/bluez/hci0/dev_AA_BB_CC_DD_EE_02",
            "CancelPairing /org/bluez/hci0/dev_AA_BB_CC_DD_EE_02",
        ]
    );
    assert_eq!(act.in_flight(), 0, "the registration is not counted");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn calls_wait_for_the_owner_then_go_to_the_unique_name() {
    let b = PrivateBus::start("bt-act-owner");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    let connect = ActCall::Connect { device: dev(1) };
    act.call(connect.clone());
    assert_eq!(act.in_flight(), 1, "queued until the owner is known");
    assert_eq!(pump(&mut act, 1, Duration::from_secs(5)), [ok(connect)]);
    assert_eq!(bz.destinations(), [bz.unique_name(), bz.unique_name()], "never the well-known name (RegisterAgent, Connect)");
    assert_eq!(act.in_flight(), 0);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn no_bluez_answers_gone() {
    let b = PrivateBus::start("bt-act-absent");
    let mut act = BtAct::at(&b.address).unwrap();
    let connect = ActCall::Connect { device: dev(1) };
    act.call(connect.clone());
    act.flush();
    assert_eq!(pump(&mut act, 1, Duration::from_secs(5)), [BtEvent::Answer { call: connect, result: Err(ActError::Gone) }]);
    let pair = ActCall::Pair { device: dev(2) };
    act.call(pair.clone());
    assert_eq!(act.read(), [BtEvent::Answer { call: pair, result: Err(ActError::Gone) }], "no round-trip");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn bluez_error_name_is_kept() {
    let b = PrivateBus::start("bt-act-error");
    let bz = FakeBluez::serve(&b);
    bz.fail("Connect", "org.bluez.Error.AlreadyConnected");
    let mut act = BtAct::at(&b.address).unwrap();
    let connect = ActCall::Connect { device: dev(1) };
    act.call(connect.clone());
    act.flush();
    assert_eq!(
        pump(&mut act, 1, Duration::from_secs(5)),
        [BtEvent::Answer { call: connect, result: Err(ActError::Remote("org.bluez.Error.AlreadyConnected".into())) }]
    );
}

/// Spec § 6: BlueZ leaving with a call held makes the bus answer `NoReply`; asked
/// again, `org.bluez` has no owner (or another one): the call ends `Gone`, never `failed`, and no
/// later call goes to the old name nor to the new owner.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn bluez_leaving_with_a_call_held_answers_gone() {
    let b = PrivateBus::start("bt-act-noreply-gone");
    let bz = FakeBluez::serve(&b);
    bz.hold("Pair");
    let mut act = BtAct::at(&b.address).unwrap();
    let pair = ActCall::Pair { device: dev(2) };
    act.call(pair.clone());
    act.flush();
    assert_eq!(recorded(&bz, &mut act, "Pair "), []);
    drop(bz);
    assert_eq!(pump(&mut act, 1, Duration::from_secs(5)), [BtEvent::Answer { call: pair, result: Err(ActError::Gone) }]);
    assert_eq!(act.in_flight(), 0);
    let new = FakeBluez::serve(&b);
    let connect = ActCall::Connect { device: dev(1) };
    act.call(connect.clone());
    act.flush();
    assert_eq!(act.read(), [BtEvent::Answer { call: connect, result: Err(ActError::Gone) }], "no round-trip");
    std::thread::sleep(Duration::from_millis(100));
    assert!(!new.calls().iter().any(|c| c.starts_with("Connect ")), "the new owner is never called: {:?}", new.calls());
}

/// A `NoReply` while the same BlueZ still owns its name (a timeout) stays an error.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_no_reply_from_the_same_owner_is_kept() {
    let b = PrivateBus::start("bt-act-noreply-kept");
    let bz = FakeBluez::serve(&b);
    bz.fail("Pair", "org.freedesktop.DBus.Error.NoReply");
    let mut act = BtAct::at(&b.address).unwrap();
    let pair = ActCall::Pair { device: dev(2) };
    act.call(pair.clone());
    act.flush();
    assert_eq!(
        pump(&mut act, 1, Duration::from_secs(5)),
        [BtEvent::Answer { call: pair, result: Err(ActError::Remote("org.freedesktop.DBus.Error.NoReply".into())) }]
    );
    let connect = ActCall::Connect { device: dev(1) };
    act.call(connect.clone());
    act.flush();
    assert_eq!(pump(&mut act, 1, Duration::from_secs(5)), [ok(connect)], "the owner kept: calls still go");
}

/// Waits (2 s at most) until the fake recorded a call starting with `prefix`.
fn recorded(bz: &FakeBluez, act: &mut BtAct, prefix: &str) -> Vec<BtEvent> {
    let end = Instant::now() + Duration::from_secs(2);
    let mut out = Vec::new();
    while !bz.calls().iter().any(|c| c.starts_with(prefix)) {
        assert!(Instant::now() < end, "{prefix} never reached the fake: {:?}", bz.calls());
        out.extend(act.read());
        act.flush();
        std::thread::sleep(Duration::from_millis(5));
    }
    out
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_forgotten_call_answer_is_dropped() {
    let b = PrivateBus::start("bt-act-forget");
    let bz = FakeBluez::serve(&b);
    bz.hold("Pair");
    let mut act = BtAct::at(&b.address).unwrap();
    let pair = ActCall::Pair { device: dev(2) };
    act.call(pair.clone());
    act.flush();
    assert_eq!(recorded(&bz, &mut act, "Pair "), []);
    act.forget(&pair);
    assert_eq!(act.in_flight(), 0);
    bz.release();
    assert_eq!(pump(&mut act, 1, Duration::from_millis(300)), [], "the late answer is dropped");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_connection_closing_answers_pending_gone() {
    let mut b = PrivateBus::start("bt-act-closed");
    let bz = FakeBluez::serve(&b);
    bz.hold("Connect");
    let mut act = BtAct::at(&b.address).unwrap();
    let connect = ActCall::Connect { device: dev(1) };
    act.call(connect.clone());
    act.flush();
    assert_eq!(recorded(&bz, &mut act, "Connect "), []);
    b.kill();
    assert_eq!(
        pump(&mut act, 2, Duration::from_secs(5)),
        [BtEvent::Answer { call: connect.clone(), result: Err(ActError::Gone) }, BtEvent::Closed]
    );
    act.call(connect.clone());
    assert_eq!(act.read(), [BtEvent::Answer { call: connect, result: Err(ActError::Gone) }]);
}

/// Pumps `act` until the fake recorded `RegisterAgent` (2 s, else panics).
fn wait_registered(act: &mut BtAct, fake: &FakeBluez) {
    let end = Instant::now() + Duration::from_secs(2);
    while !fake.calls().iter().any(|c| c.starts_with("RegisterAgent ")) {
        assert!(Instant::now() < end, "never registered: {:?}", fake.calls());
        act.read();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Pumps `act` until an agent request arrives (2 s, else panics).
fn agent_event(act: &mut BtAct) -> (AgentId, AgentRequest) {
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(Instant::now() < end, "no agent request");
        for e in act.read() {
            if let BtEvent::Agent { id, request } = e {
                return (id, request);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits (2 s at most) until the fake has `n` replies from the agent.
fn replies(fake: &FakeBluez, act: &mut BtAct, n: usize) -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(2);
    while fake.replies().len() < n && Instant::now() < end {
        act.read();
        std::thread::sleep(Duration::from_millis(5));
    }
    fake.replies()
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_agent_is_registered_never_as_default() {
    let b = PrivateBus::start("bt-agent-reg");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    let connect = ActCall::Connect { device: dev(1) };
    act.call(connect.clone());
    assert_eq!(pump(&mut act, 1, Duration::from_secs(5)), [ok(connect)]);
    assert_eq!(bz.calls(), [REGISTER, "Connect /org/bluez/hci0/dev_AA_BB_CC_DD_EE_01"]);
    assert!(!bz.calls().iter().any(|c| c.starts_with("RequestDefaultAgent")));
    assert!(bz.agent_owner().is_some());
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn display_passkey_is_delivered_and_answered() {
    let b = PrivateBus::start("bt-agent-passkey");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    wait_registered(&mut act, &bz);
    act.call(ActCall::Pair { device: dev(2) });
    act.flush();
    bz.ask_passkey(2, 482_913);
    let (id, request) = agent_event(&mut act);
    assert_eq!(request, AgentRequest::DisplayPasskey { device: dev(2), passkey: 482_913 });
    act.reply(id, AgentReply::Ok);
    act.flush();
    assert_eq!(replies(&bz, &mut act, 1), ["DisplayPasskey ok"]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn request_confirmation_waits_for_the_reply() {
    let b = PrivateBus::start("bt-agent-confirm");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    wait_registered(&mut act, &bz);
    bz.ask_confirmation(2, 7);
    let (id, request) = agent_event(&mut act);
    assert_eq!(request, AgentRequest::RequestConfirmation { device: dev(2), passkey: 7 });
    std::thread::sleep(Duration::from_millis(300));
    act.read();
    assert!(bz.replies().is_empty(), "held until the panel answers: {:?}", bz.replies());
    act.reply(id, AgentReply::Ok);
    act.flush();
    assert_eq!(replies(&bz, &mut act, 1), ["RequestConfirmation ok"]);
    bz.ask_confirmation(2, 7);
    let (id2, _) = agent_event(&mut act);
    act.reply(id2, AgentReply::Rejected);
    act.flush();
    assert_eq!(replies(&bz, &mut act, 2), ["RequestConfirmation ok", "RequestConfirmation org.bluez.Error.Rejected"]);
    act.reply(id2, AgentReply::Ok);
    act.flush();
    assert_eq!(replies(&bz, &mut act, 3).len(), 2, "already answered: nothing sent");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn cancel_is_delivered() {
    let b = PrivateBus::start("bt-agent-cancel");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    wait_registered(&mut act, &bz);
    bz.cancel();
    assert_eq!(agent_event(&mut act).1, AgentRequest::Cancel);
    assert_eq!(replies(&bz, &mut act, 1), ["Cancel ok"], "acknowledged without a reply() call");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_stranger_calling_the_agent_is_refused() {
    let b = PrivateBus::start("bt-agent-stranger");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    wait_registered(&mut act, &bz);
    let owner = bz.agent_owner().unwrap();
    let stranger = Peer::connect(&b);
    let t = std::thread::spawn(move || {
        let m = dbus::Message::new_method_call(owner, AGENT_PATH, "org.bluez.Agent1", "DisplayPasskey").unwrap().append3(
            dbus::Path::from(dev(2)),
            1u32,
            0u16,
        );
        stranger.call(m).map(|_| ())
    });
    let events = pump(&mut act, usize::MAX, Duration::from_secs(2));
    assert!(!events.iter().any(|e| matches!(e, BtEvent::Agent { .. })), "{events:?}");
    let e = t.join().unwrap().unwrap_err();
    assert_eq!(e.name(), Some("org.freedesktop.DBus.Error.AccessDenied"));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn close_unregisters() {
    let b = PrivateBus::start("bt-agent-close");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    wait_registered(&mut act, &bz);
    bz.ask_confirmation(2, 7);
    agent_event(&mut act);
    act.close();
    let end = Instant::now() + Duration::from_secs(2);
    while (bz.replies().is_empty() || !bz.calls().last().is_some_and(|c| c.starts_with("UnregisterAgent"))) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(bz.replies(), ["RequestConfirmation org.bluez.Error.Rejected"]);
    assert_eq!(bz.calls().last().map(String::as_str), Some("UnregisterAgent /org/bluez /org/alpheus/bluetooth/agent"));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_fifth_held_call_rejects_the_oldest() {
    let b = PrivateBus::start("bt-agent-evict");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    wait_registered(&mut act, &bz);
    for _ in 0..4 {
        bz.ask_confirmation(2, 7);
        agent_event(&mut act);
    }
    assert!(bz.replies().is_empty(), "four held: {:?}", bz.replies());
    bz.ask_confirmation(2, 7);
    agent_event(&mut act);
    act.flush();
    assert_eq!(replies(&bz, &mut act, 1), ["RequestConfirmation org.bluez.Error.Rejected"]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_released_agent_is_not_unregistered() {
    let b = PrivateBus::start("bt-agent-release");
    let bz = FakeBluez::serve(&b);
    let mut act = BtAct::at(&b.address).unwrap();
    wait_registered(&mut act, &bz);
    bz.release_agent();
    assert_eq!(agent_event(&mut act).1, AgentRequest::Release);
    assert_eq!(replies(&bz, &mut act, 1), ["Release ok"]);
    act.close();
    let connect = ActCall::Connect { device: dev(1) };
    let mut probe = BtAct::at(&b.address).unwrap();
    probe.call(connect.clone());
    assert_eq!(pump(&mut probe, 1, Duration::from_secs(5)), [ok(connect)], "the bus has seen everything before");
    assert!(!bz.calls().iter().any(|c| c.starts_with("UnregisterAgent")), "{:?}", bz.calls());
}
