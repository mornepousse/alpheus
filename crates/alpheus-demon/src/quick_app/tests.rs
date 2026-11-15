use super::*;
use crate::config::{BarConfig, QuickConfig};
use crate::keys::Key;
use crate::quick::{Row, shown};
use crate::quick_wifi::Pick;
use crate::system::Volume;
use alpheus_commun::exec::fake::FakeExecutor;

/// Mae's ruling P1: closing the panel lets the D-Bus thread go back to reading nothing, unless
/// an attempt on a new network is still followed: then the detail's reading stays, so a failed
/// new profile is still seen (and deleted) after the panel went.
#[test]
fn closing_the_panel_keeps_the_detail_read_while_an_attempt_is_followed() {
    let follow = WifiFollow { attempt: Attempt::new(b"Cafe-Libre", true, None, Instant::now()), state: None };
    let tap = WifiTap::new().unwrap();
    tap.set(WifiScope::Detail);
    wifi_closed(&tap, Some(&follow));
    assert_eq!(tap.scope(), WifiScope::Detail, "a follower: still read");
    wifi_closed(&tap, None);
    assert_eq!(tap.scope(), WifiScope::Closed, "nothing followed: nothing read");
    assert_eq!(wifi_scope(WifiScope::Row, Some(&follow)), WifiScope::Detail, "the wider of the two");
    assert_eq!(wifi_scope(WifiScope::Detail, None), WifiScope::Detail);
    assert_eq!(wifi_scope(WifiScope::Row, None), WifiScope::Row);
}

/// Quick Wi-Fi spec § 5: a Wi-Fi message's state goes to the open panel; the message left for the
/// bar carries nothing.
#[test]
fn a_wifi_message_goes_to_the_panel_and_leaves_nothing_for_the_bar() {
    let state = WifiState { enabled: true, device: None, stale: false };
    let mut m = Msg::Wifi { open: Some(2), state: Some(state.clone()) };
    assert_eq!(taken_wifi(&mut m), Some((Some(2), Some(state))));
    assert_eq!(m, Msg::Wifi { open: Some(2), state: None }, "emptied");
    assert_eq!(
        taken_wifi(&mut Msg::Wifi { open: Some(2), state: None }),
        Some((Some(2), None)),
        "NetworkManager gone: the panel hears it too"
    );
    assert_eq!(taken_wifi(&mut Msg::Profile(None)), None, "only a Wi-Fi message");
}

/// Mae's ruling P1: a follower waits while the attempt does; once it ends it is dropped (the tap
/// then goes Closed, the timer disarms), a failure logged once and its new profile deleted
/// through the executor; a success or NetworkManager gone deletes nothing.
#[test]
fn a_follower_ends_with_its_attempt_and_deletes_only_a_failed_new_profile() {
    let delete = "nmcli connection delete uuid 22222222-3333-4444-8555-666666666666".to_string();
    let cleanup = || {
        Some(QuickAction::Run { argv: quick::argv(&delete.split(' ').collect::<Vec<_>>()), what: "wifi delete the failed profile".into() })
    };
    let follower = || Some(WifiFollow { attempt: Attempt::new(b"Cafe-Libre", true, None, Instant::now()), state: None });
    let fake = FakeExecutor::new();
    fake.ok(&delete, "");
    let mut f = follower();
    assert!(followed(&fake, &mut f, Outcome::Waiting).is_empty());
    assert!(f.is_some(), "waiting: still followed");
    let lines = followed(&fake, &mut f, Outcome::Failed { why: "the activation failed", cleanup: cleanup() });
    assert_eq!(lines, ["quick: wifi connection failed (the activation failed)", "quick: wifi delete the failed profile"]);
    assert!(f.is_none(), "ended: dropped");
    assert_eq!(fake.calls(), std::slice::from_ref(&delete), "the delete through the executor");
    let fake = FakeExecutor::new();
    let mut f = follower();
    assert!(followed(&fake, &mut f, Outcome::Connected).is_empty());
    assert!(f.is_none() && fake.calls().is_empty(), "connected: dropped, nothing deleted");
    let mut f = follower();
    assert_eq!(followed(&fake, &mut f, Outcome::Gone), ["quick: wifi connection failed (NetworkManager went away)"]);
    assert!(f.is_none() && fake.calls().is_empty());
    let mut f = follower();
    assert_eq!(
        followed(&fake, &mut f, Outcome::Failed { why: "no answer in 20 s", cleanup: None }),
        ["quick: wifi connection failed (no answer in 20 s)"]
    );
    assert!(f.is_none() && fake.calls().is_empty(), "no profile made: nothing deleted");
}

/// A panel reopened on the Wi-Fi (the network module, `--quick wifi`) while an
/// attempt is followed takes it back and opens the detail at once from the state it carried
/// (no word of the D-Bus thread is coming: its scope never changed), with its one rescan.
#[test]
fn a_panel_reopened_on_the_wifi_takes_the_attempt_back_and_opens_the_detail() {
    let t = Instant::now();
    let bar = Bar::new(BarConfig::default());
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    let mut first = panel_on("Cafe-Libre");
    let QuickAction::Run { .. } = first.wifi.key(&QuickConfig::default(), Key::Enter, t) else { panic!("an open network") };
    let follow = WifiFollow::leaving(&mut first.wifi).expect("a new network: followed");
    let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
    let action = resumed(&mut q, follow);
    assert_eq!(action, quick::run(&["nmcli", "device", "wifi", "rescan"], "wifi rescan"), "the detail opened");
    assert!(q.wifi.open && q.selected == Row::Wifi && q.wifi.attempt.is_some(), "taken back, the detail open");
    assert!(!q.wifi.focus_pending, "spent");
    let mut top = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
    let mut again = panel_on("Cafe-Libre");
    again.wifi.key(&QuickConfig::default(), Key::Enter, t);
    let follow = WifiFollow::leaving(&mut again.wifi).expect("followed");
    assert_eq!(resumed(&mut top, follow), QuickAction::Redraw, "opened on the top: no detail");
    assert!(!top.wifi.open && top.wifi.attempt.is_some());
}

/// Both orders: a state read for an earlier open (a close's last word, a
/// re-read under the scope before) never reaches the panel, so it neither spends the focus nor
/// blanks the row; the first state of this open with a device opens the detail. The bus going
/// down (untagged) still reaches it.
#[test]
fn only_a_state_read_for_this_open_reaches_the_panel() {
    let t = Instant::now();
    let bar = Bar::new(BarConfig::default());
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    let rescan = quick::run(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
    let current = Some(7);
    let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
    assert_eq!(panel_wifi(&mut q, &live, Some(6), None, current, t), None, "the close's None, queued: dropped");
    assert_eq!(panel_wifi(&mut q, &live, Some(6), Some(wifi_around()), current, t), None, "the open before's: dropped");
    assert!(q.wifi.focus_pending && q.wifi.state.is_none(), "nothing taken");
    let news = panel_wifi(&mut q, &live, Some(7), Some(wifi_around()), current, t).expect("this open's");
    assert_eq!(news.action, rescan, "the detail opened");
    assert!(q.wifi.open && q.selected == Row::Wifi);
    let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
    panel_wifi(&mut q, &live, Some(7), Some(wifi_around()), current, t);
    assert_eq!(panel_wifi(&mut q, &live, Some(6), None, current, t), None, "delivered after the reopen: dropped");
    assert!(q.wifi.open && q.wifi.shows(), "never blanked by it");
    assert!(panel_wifi(&mut q, &live, None, None, current, t).is_some(), "the bus down: the panel hears it");
    assert!(!q.wifi.shows());
    let mut q = Quick::new(Focus::Wifi, &live, vec![], None, [None, None], 20);
    assert!(panel_wifi(&mut q, &live, Some(3), Some(wifi_around()), None, t).is_some(), "no tap: nothing to tell apart");
}

/// An attempt whose saved list was unknown when it began can never clean up,
/// so it is not followed after the panel (no rules, reads or timer for nothing).
#[test]
fn an_attempt_that_cannot_clean_up_is_not_followed() {
    let t = Instant::now();
    let mut q = panel_on("Cafe-Libre");
    if let Some(d) = q.wifi.state.as_mut().and_then(|s| s.device.as_mut()) {
        d.saved = None;
    }
    let QuickAction::Run { .. } = q.wifi.key(&QuickConfig::default(), Key::Enter, t) else { panic!("an open network") };
    assert!(q.wifi.attempt.as_ref().is_some_and(|a| a.creates && a.known.is_none()), "{:?}", q.wifi.attempt);
    assert!(WifiFollow::leaving(&mut q.wifi).is_none(), "nothing it could delete: not followed");
    let mut q = panel_on("Cafe-Libre");
    q.wifi.key(&QuickConfig::default(), Key::Enter, t);
    assert!(WifiFollow::leaving(&mut q.wifi).is_some(), "the saved list known: followed");
}

fn sink(id: u32, label: &str, default: bool) -> Sink {
    Sink { id, label: label.into(), default }
}

fn vol(percent: u8, muted: bool) -> Option<Volume> {
    Some(Volume { percent, muted })
}

/// Without the `osd` module the sound thread publishes the sink's
/// volume only, so the bar never heard of the microphone; the state read at open (the panel's
/// scope) shows it, muted, at once — never a stale or missing Mic row.
#[test]
fn the_state_read_at_open_shows_a_microphone_the_bar_never_heard_of() {
    let mut config = BarConfig::default();
    config.modules.retain(|m| *m != ModuleId::Osd);
    let mut bar = Bar::new(config);
    bar.apply(Msg::Sound(Some(SoundState { sink: vol(84, false), ..SoundState::default() })));
    let opened = SoundState {
        sink: vol(84, false),
        source: vol(60, true),
        sinks: Some(vec![sink(99, "Speaker", false), sink(122, "AG06", true)]),
        ..SoundState::default()
    };
    let (sinks, _, redraw) = opened_sound(&mut bar, opened);
    assert_eq!(sinks.len(), 2, "the list goes to the panel");
    assert!(!redraw, "the bar draws the sink alone: unchanged");
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    assert_eq!(live.sound.and_then(|s| s.source), vol(60, true), "the microphone, muted");
    assert_eq!(live.sound.and_then(|s| s.sinks.as_ref()), None, "the bar never keeps a list");
    let q = Quick::new(Focus::Sound, &live, sinks, None, [None, None], 20);
    assert!(shown(&live, &q).contains(&Row::Mic), "{:?}", shown(&live, &q));
}

/// pw-dump going while the outputs detail is open empties the panel's list,
/// so the detail closes and Enter never sends a `set-default` to a dead node id.
#[test]
fn a_pw_dump_outage_closes_the_outputs_detail() {
    let mut bar = Bar::new(BarConfig::default());
    bar.apply(Msg::Sound(Some(SoundState { sink: vol(64, false), ..SoundState::default() })));
    let sinks = vec![sink(99, "Speaker", false), sink(122, "AG06", true)];
    let mut q = Quick::new(Focus::Sound, &quick::live(&bar, &crate::dnd::Dnd::default()), sinks, None, [None, None], 20);
    let now = Instant::now();
    assert_eq!(q.key(&quick::live(&bar, &crate::dnd::Dnd::default()), crate::keys::Key::Enter, false, now), QuickAction::Redraw);
    assert!(q.detail, "the outputs detail is open");
    let mut down = Msg::Sound(None);
    if let Some(s) = taken_sinks(&mut down) {
        q.set_sinks(s);
    }
    bar.apply(down);
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    q.fit(&live); // quick_refresh, after the message
    assert!(!q.detail, "the detail closed");
    let enter = q.key(&live, crate::keys::Key::Enter, false, now);
    assert!(
        !matches!(&enter, QuickAction::Run { argv, .. } if argv.iter().any(|a| a == "set-default")),
        "no set-default after the outage: {enter:?}"
    );
    let mut other = Msg::Profile(None);
    assert_eq!(taken_sinks(&mut other), None, "only a sound message carries a list");
}

/// A verb within TOGGLE_DEBOUNCE of a verb's open is ignored; past it, or
/// with the panel opened by a click, it closes the panel.
#[test]
fn a_verb_right_after_the_verb_opened_the_panel_is_debounced() {
    use crate::launcher::host::TOGGLE_DEBOUNCE;
    use std::time::Duration;
    let t = Instant::now();
    assert!(verb_debounced(Some(t), t + Duration::from_millis(100)), "the same press, re-fired");
    assert!(!verb_debounced(Some(t), t + TOGGLE_DEBOUNCE), "past the window: a real toggle");
    assert!(!verb_debounced(Some(t), t + Duration::from_millis(300)));
    assert!(!verb_debounced(None, t), "opened by a click: never debounced");
}

/// After a pw-dump outage the tap reads an empty model (`sinks: Some([])`, no sink, no source):
/// the bar keeps what it has (Down), and the panel gets no sound row.
#[test]
fn an_empty_model_at_open_leaves_the_bar_alone() {
    let mut bar = Bar::new(BarConfig::default());
    bar.apply(Msg::Sound(None));
    let (sinks, _, redraw) = opened_sound(&mut bar, SoundState { sinks: Some(vec![]), ..SoundState::default() });
    assert!(sinks.is_empty() && !redraw);
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    assert!(live.sound.is_none(), "still down");
    let q = Quick::new(Focus::Sound, &live, sinks, None, [None, None], 20);
    let rows = shown(&live, &q);
    assert!(!rows.contains(&Row::Volume) && !rows.contains(&Row::Mic), "{rows:?}");
}

/// Bureau-Test (WPA2) and Cafe-Libre, Labo-Test (open) around, the device disconnected, no
/// profile saved (fictional networks).
fn wifi_around() -> WifiState {
    use crate::system::{AccessPoint, Security, WifiDevice};
    let ap = |s: &str, security| AccessPoint { ssid: s.as_bytes().to_vec(), strength: 60, security };
    WifiState {
        enabled: true,
        device: Some(WifiDevice {
            state: 30,
            active: None,
            active_uuid: None,
            access_points: Some(vec![ap("Bureau-Test", Security::Psk), ap("Cafe-Libre", Security::Open), ap("Labo-Test", Security::Open)]),
            saved: Some(vec![]),
        }),
        stale: false,
    }
}

/// A panel with the Wi-Fi detail open on `wifi_around`, `ssid` picked.
fn panel_on(ssid: &str) -> Quick {
    let bar = Bar::new(BarConfig::default());
    let mut q = Quick::new(Focus::Top, &quick::live(&bar, &crate::dnd::Dnd::default()), vec![], None, [None, None], 20);
    q.wifi = Wifi { state: Some(wifi_around()), ..Wifi::default() };
    q.wifi.open_detail();
    q.wifi.pick = Pick::Network(ssid.as_bytes().to_vec());
    q
}

const ASK: &str = "nmcli --ask device wifi connect Bureau-Test";

/// `q` with the field open on Bureau-Test, `fake-pass` typed, Enter pressed: the RunSecret.
fn password_typed(q: &mut Quick, t: Instant) -> (Vec<String>, String) {
    let c = QuickConfig::default();
    q.wifi.key(&c, Key::Enter, t);
    for ch in "fake-pass".chars() {
        q.wifi.key(&c, Key::Char(ch), t);
    }
    match q.wifi.key(&c, Key::Enter, t) {
        QuickAction::RunSecret { argv, what } => (argv, what),
        other => panic!("not sent: {other:?}"),
    }
}

/// The password goes to nmcli's standard input with a newline, never in
/// its arguments; the field closes. An `Io` error may come after nmcli started: the attempt stays.
#[test]
fn the_password_is_handed_on_stdin_and_the_field_closes() {
    let t = Instant::now();
    let fake = FakeExecutor::new();
    fake.ok(ASK, "");
    let mut q = panel_on("Bureau-Test");
    let (argv, what) = password_typed(&mut q, t);
    assert_eq!(quick_spawn_secret(&fake, &mut q, &argv, &what), Some("quick: wifi connect (password)".into()));
    assert_eq!(fake.inputs(), [(ASK.to_string(), b"fake-pass\n".to_vec())]);
    assert_eq!(fake.calls(), [ASK]);
    assert!(q.wifi.field.is_none() && q.wifi.password().is_none(), "the field closed, wiped");
    assert!(q.wifi.attempt.is_some(), "followed");
    let fake = FakeExecutor::new();
    fake.respond(ASK, Err(ExecError::Io("Broken pipe".into())));
    let mut q = panel_on("Bureau-Test");
    let (argv, what) = password_typed(&mut q, t);
    let line = quick_spawn_secret(&fake, &mut q, &argv, &what).unwrap_or_default();
    assert!(!line.contains("fake-pass"), "{line}");
    assert!(q.wifi.field.is_none() && q.wifi.attempt.is_some(), "maybe started: the attempt stays, never retried");
}

/// nmcli not found never started anything: the attempt ends at once (nothing
/// to follow, no false timeout), and Enter on another network acts.
#[test]
fn an_nmcli_never_started_ends_the_attempt_at_once() {
    let (t, c) = (Instant::now(), QuickConfig::default());
    let fake = FakeExecutor::new();
    let mut q = panel_on("Bureau-Test");
    let (argv, what) = password_typed(&mut q, t);
    assert!(quick_spawn_secret(&fake, &mut q, &argv, &what).is_some_and(|l| l.contains("could not start")));
    assert!(q.wifi.attempt.is_none() && q.wifi.deadline().is_none(), "no attempt, no timer");
    assert!(WifiFollow::leaving(&mut q.wifi).is_none(), "nothing to follow");
    q.wifi.pick = Pick::Network(b"Cafe-Libre".to_vec());
    let open = q.wifi.key(&c, Key::Enter, t);
    assert!(matches!(&open, QuickAction::Run { what, .. } if what == "wifi connect (open)"), "{open:?}");
    let QuickAction::Run { argv, what } = open else { return };
    assert_eq!(spawn_logged(&fake, Some(&mut q), &argv, &what), None, "logged once per open");
    assert!(q.wifi.attempt.is_none(), "an open network's nmcli not found: ended too");
    q.wifi.pick = Pick::Network(b"Labo-Test".to_vec());
    assert!(matches!(q.wifi.key(&c, Key::Enter, t), QuickAction::Run { .. }), "and the next one acts");
    let fake = FakeExecutor::new();
    fake.respond("nmcli device wifi connect Labo-Test", Err(ExecError::Io("busy".into())));
    let mut q = panel_on("Labo-Test");
    let QuickAction::Run { argv, what } = q.wifi.key(&c, Key::Enter, t) else { panic!("an open network") };
    spawn_logged(&fake, Some(&mut q), &argv, &what);
    assert!(q.wifi.attempt.is_some(), "Io: maybe started, kept");
}

fn casque() -> Option<crate::system::Headset> {
    Some(crate::system::Headset { device: 61, label: "CasqueBT".into(), active: crate::system::HeadsetMode::HiFi, hifi: 1, mic: 3 })
}

/// Quick Bluetooth spec § 4: the headset goes to the open panel, never the bar.
#[test]
fn the_headset_goes_to_the_panel_never_the_bar() {
    let mut bar = Bar::new(BarConfig::default());
    let (_, headset, _) = opened_sound(&mut bar, SoundState { sink: vol(40, false), headset: casque(), ..SoundState::default() });
    assert_eq!(headset, casque());
    assert_eq!(quick::live(&bar, &crate::dnd::Dnd::default()).sound.and_then(|s| s.headset.clone()), None, "the bar keeps none");
    let mut m = Msg::Sound(Some(SoundState { headset: casque(), sinks: Some(vec![]), ..SoundState::default() }));
    assert_eq!(taken_headset(&mut m), Some(casque()));
    assert!(matches!(&m, Msg::Sound(Some(s)) if s.headset.is_none()), "taken out of the message");
    let mut base = Msg::Sound(Some(SoundState { headset: casque(), ..SoundState::default() }));
    assert_eq!(taken_headset(&mut base), None, "a base-scope message says nothing of the headset");
    assert!(matches!(&base, Msg::Sound(Some(s)) if s.headset.is_none()), "and still never keeps one");
    let l = quick::live(&bar, &crate::dnd::Dnd::default());
    let mut q = Quick::new(Focus::Sound, &l, Vec::new(), None, [None, None], 20);
    q.set_headset(casque());
    if let Some(h) = taken_headset(&mut base) {
        q.set_headset(h);
    }
    assert_eq!(q.headset, casque(), "a stale base-scope message never wipes the panel's row");
    assert_eq!(taken_headset(&mut Msg::Sound(None)), Some(None), "pw-dump gone: the row goes");
    assert_eq!(taken_headset(&mut Msg::Profile(None)), None, "only a sound message carries one");
}

use crate::bt_app::tests::{HCI0, bar_bt_on, bt_view};
use crate::system::BtScope;

/// Quick Bluetooth spec § 5: a Bluetooth view goes to the open panel; the message left for the
/// bar carries nothing.
#[test]
fn a_bt_message_goes_to_the_panel_and_leaves_nothing_for_the_bar() {
    let mut m = Msg::Bt { open: Some(2), state: Some(bt_view()) };
    assert_eq!(taken_bt(&mut m), Some((Some(2), Some(bt_view()))));
    assert_eq!(m, Msg::Bt { open: Some(2), state: None }, "emptied");
    assert_eq!(taken_bt(&mut Msg::Bt { open: None, state: None }), Some((None, None)), "the bus down: heard too");
    assert_eq!(taken_bt(&mut Msg::Wifi { open: Some(2), state: None }), None, "only a Bluetooth message");
}

#[test]
fn only_a_view_read_for_this_open_reaches_the_panel() {
    let (t, bar) = (Instant::now(), bar_bt_on());
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    let mut q = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
    assert_eq!(panel_bt(&mut q, &live, Some(6), Some(bt_view()), Some(7), t), None, "the open before's: dropped");
    assert!(q.bt.view.is_none());
    assert!(panel_bt(&mut q, &live, Some(7), Some(bt_view()), Some(7), t).is_some());
    assert_eq!(q.bt.view, Some(bt_view()));
    assert_eq!(panel_bt(&mut q, &live, Some(6), None, Some(7), t), None, "a close's None, queued: dropped");
    assert!(q.bt.view.is_some(), "never blanked by it");
    assert!(panel_bt(&mut q, &live, None, None, Some(7), t).is_some(), "the bus down (untagged): heard");
    assert!(q.bt.view.is_none());
    assert!(panel_bt(&mut q, &live, Some(3), Some(bt_view()), None, t).is_some(), "no tap: nothing to tell apart");
}

#[test]
fn bt_scope_follows_the_panel() {
    let bar = bar_bt_on();
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    assert_eq!(bt_scope_for(None, true), BtScope::Closed);
    let mut q = Quick::new(Focus::Top, &live, vec![], None, [None, None], 20);
    assert_eq!(bt_scope_for(Some(&q), false), BtScope::Closed, "no row: nothing read");
    assert_eq!(bt_scope_for(Some(&q), true), BtScope::Row);
    q.bt.open_detail();
    assert_eq!(bt_scope_for(Some(&q), true), BtScope::Detail);
}

/// BlueZ restarting with the detail open.
#[test]
fn bluez_restarting_closes_the_detail_and_lets_the_connection_go() {
    use crate::bt_app::{adapter_seen, connection_wanted, opens_for};
    let (t, bar) = (Instant::now(), bar_bt_on());
    let live = quick::live(&bar, &crate::dnd::Dnd::default());
    let mut q = Quick::new(Focus::Bluetooth, &live, vec![], None, [None, None], 20);
    panel_bt(&mut q, &live, Some(3), Some(bt_view()), Some(3), t).expect("this open's");
    assert!(q.bt.open && adapter_seen(&q) && connection_wanted(Some(&q), 0));
    panel_bt(&mut q, &live, Some(3), None, Some(3), t).expect("BlueZ gone: heard");
    assert!(!q.bt.open, "the detail closed");
    assert!(!adapter_seen(&q) && !connection_wanted(Some(&q), 0));
    assert!(!opens_for(&q.bt_leaving(), adapter_seen(&q)), "nothing reopened toward the old owner");
    let late = crate::quick_bt::BtOrder::Call(crate::sources::dbus::bt_act::ActCall::StopDiscovery { adapter: HCI0.into() });
    assert!(!opens_for(&[late], adapter_seen(&q)), "nor by a call still made before the new owner shows");
    panel_bt(&mut q, &live, Some(3), Some(bt_view()), Some(3), t).expect("the new owner");
    assert!(adapter_seen(&q) && quick::shown(&live, &q).contains(&Row::Bluetooth), "the row back");
}

/// As the D-Bus thread reports it: BlueZ gone reads as a view with no adapter
/// (`BtState::Absent`), the bus down as None; either lets the acting connection go before the
/// panel's orders, a view of an earlier open never does.
#[test]
fn a_view_without_an_adapter_of_this_open_lets_the_connection_go() {
    let no_adapter = BtView { adapter: None, ..bt_view() };
    assert!(bluez_left(Some(3), None, Some(3)), "BlueZ's view gone");
    assert!(bluez_left(Some(3), Some(&no_adapter), Some(3)), "BlueZ gone: an empty view");
    assert!(bluez_left(None, None, Some(3)), "the bus down");
    assert!(!bluez_left(Some(3), Some(&bt_view()), Some(3)), "an adapter: kept");
    assert!(!bluez_left(Some(2), None, Some(3)), "an earlier open's close: kept");
}

#[test]
fn dnd_request_is_the_cli_request() {
    use crate::quick_dnd::DndChoice;
    use alpheus_commun::launcher_ipc::{DndEnd, DndVerb};
    let now = 1_800_000_000;
    let at = (8, 0);
    assert_eq!(dnd_request(DndChoice::Off, at, now), Some(Request::Dnd(DndVerb::Off)));
    assert_eq!(dnd_request(DndChoice::For(1800), at, now), Some(Request::DndOn(DndEnd::For(1800))));
    assert_eq!(dnd_request(DndChoice::Untimed, at, now), Some(Request::Dnd(DndVerb::On)));
    let Some(Request::DndOn(DndEnd::Until(t))) = dnd_request(DndChoice::Until, at, now) else { panic!("until") };
    let t = t as i64;
    assert!(now < t && t <= now + 90_000, "{} s ahead", t - now);
    let local = crate::clock::LocalTime::from_epoch(t);
    assert_eq!((u32::from(local.hour), u32::from(local.minute)), at, "the end falls on the configured wall-clock time");
}
