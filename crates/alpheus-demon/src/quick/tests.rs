use super::*;
use crate::grid::Tint;
use crate::quick_wifi::{OFF_NOTE, STRENGTH, WIFI_OFF, WIFI_ON};
use crate::segments::BLUETOOTH_CONNECTED;
use crate::segments::VOLUME_MEDIUM;
use crate::system::{AccessPoint, BtDevice, Headset, HeadsetMode, Profile, SavedWifi, Security, WifiDevice, WifiState};

fn vol(percent: u8, muted: bool) -> Option<Volume> {
    Some(Volume { percent, muted })
}

fn sink(id: u32, label: &str, default: bool) -> Sink {
    Sink { id, label: label.into(), default }
}

/// Speaker (99) and the default AG06 (122).
fn two_sinks() -> Vec<Sink> {
    vec![sink(99, "Speaker", false), sink(122, "AG06/AG03", true)]
}

fn backlight(percent: u8) -> Option<Backlight> {
    Some(Backlight { device: "intel_backlight".into(), percent })
}

/// What the rest of the daemon says, owned, so a test changes one part.
struct World {
    sound: SoundState,
    dnd: Option<bool>,
    profile: ProfileState,
    config: QuickConfig,
    bluetooth: Option<BtState>,
}

impl World {
    /// Everything present: the sink at 64 %, the microphone live, do-not-disturb off, the balanced
    /// profile of three, the default config (lock, reboot, power off; no toggle).
    fn full() -> World {
        World {
            sound: SoundState { sink: vol(64, false), source: vol(100, false), ..SoundState::default() },
            dnd: Some(false),
            profile: ProfileState { active: Some(Profile::Balanced), offered: Profile::ALL.to_vec() },
            config: QuickConfig::default(),
            bluetooth: None,
        }
    }

    fn live(&self) -> Live<'_> {
        Live {
            sound: Some(&self.sound),
            dnd: self.dnd,
            profile: Some(&self.profile),
            config: &self.config,
            bluetooth: self.bluetooth.as_ref(),
        }
    }
}

/// The panel as it opens on `w`: two sinks, the backlight at 75 %, toggles unknown, room for 40 rows.
fn quick(w: &World, focus: Focus) -> Quick {
    Quick::new(focus, &w.live(), two_sinks(), backlight(75), [None, None], 40)
}

fn texts(b: &Boxed) -> Vec<String> {
    b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
}

#[test]
fn every_row_shows_in_the_spec_order_with_its_separators() {
    let w = World::full();
    let l = w.live();
    let q = quick(&w, Focus::Top);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
    assert_eq!(
        lines(&l, &q),
        [
            Line::Row(Row::Volume),
            Line::Row(Row::Brightness),
            Line::Separator,
            Line::Row(Row::Mic),
            Line::Row(Row::Dnd),
            Line::Row(Row::Profile),
            Line::Separator,
            Line::Row(Row::Session)
        ]
    );
    let b = rows(&l, &q);
    assert_eq!(b.cells(), (WIDTH + 2, 8));
    assert_eq!(b.separators, [2, 6]);
    let t = texts(&b);
    assert_eq!(t[0], format!(" {VOLUME_MEDIUM} {}{}  64%  ›", "━".repeat(14), "─".repeat(8)), "64 % of 22 cells: 14; › with two sinks");
    assert_eq!(t[1], format!(" {BRIGHTNESS} {}{}  75%", "━".repeat(16), "─".repeat(6)));
    assert_eq!(t[3], format!(" {MIC_ON} Microphone{}live", " ".repeat(16)));
    assert_eq!(t[4], format!(" {BELL} Do not disturb{}○", " ".repeat(15)));
    assert_eq!(t[5], format!(" {PROFILE} Power profile{}balanced ›", " ".repeat(7)));
    assert_eq!(t[7], format!("   {LOCK}    {REBOOT}    {POWER}"), "lock, reboot, power off: logout is empty by default");
    // Selected: the band spans the whole row, its track included; the bar cells keep their own
    // foreground on it (a band that skipped the track looked broken).
    let selected = b.rows[0].cells();
    assert!(selected.iter().all(|c| c.style.bg == Tint::Selection), "the band spans the row: {}", b.rows[0].styles());
    assert_eq!((&b.rows[0].styles()[..3], &b.rows[0].styles()[25..]), ("SSS", "S".repeat(9).as_str()));
    let track: String = selected[3..25]
        .iter()
        .map(|c| match c.style.fg {
            Tint::Accent => 'a',
            Tint::FgDim => 'm',
            _ => '?',
        })
        .collect();
    assert_eq!(track, format!("{}{}", "a".repeat(14), "m".repeat(8)), "the bar in its colours, on the band");
    assert_eq!(b.rows[1].styles(), format!(".a.{}{}{}", "a".repeat(16), "m".repeat(6), ".".repeat(9)));
    assert_eq!(&b.rows[4].styles()[32..33], "m", "○ in fg_dim");
}

#[test]
fn marks_warn_and_dim() {
    let mut w = World::full();
    w.sound.source = vol(100, true);
    w.sound.sink = vol(120, false);
    w.dnd = Some(true);
    w.config.keep_awake.toggle = vec!["fake-keep-awake".into()];
    w.config.night_light.toggle = vec!["fake-night".into()];
    let q = Quick::new(Focus::Top, &w.live(), two_sinks(), backlight(75), [Some(true), None], 40);
    let b = rows(&w.live(), &q);
    let t = texts(&b);
    // Volume, brightness, separator, mic, DND, profile, keep awake, night light, separator, session.
    assert_eq!(t[3], format!(" {MIC_OFF} Microphone{}muted", " ".repeat(15)));
    assert_eq!(&b.rows[3].styles()[28..33], "wwwww", "muted in warn");
    assert_eq!(t[4], format!(" {BELL} Do not disturb{}●", " ".repeat(15)));
    assert_eq!(&b.rows[4].styles()[32..33], "a", "● in the accent");
    assert_eq!(t[6], format!(" {KEEP_AWAKE} Keep awake{}●", " ".repeat(19)));
    assert_eq!(t[7], format!(" {NIGHT_LIGHT} Night light{}·", " ".repeat(18)), "unknown state");
    // The volume row is selected: its track on the band, in its own foreground.
    let track = |b: &Boxed| b.rows[0].cells()[3..25].iter().map(|c| c.style.fg).collect::<Vec<_>>();
    assert!(track(&b).iter().all(|f| *f == Tint::Warn), "above 100 %: the whole bar in warn: {:?}", track(&b));
    let mut muted = World::full();
    muted.sound.sink = vol(64, true);
    let b = rows(&muted.live(), &quick(&muted, Focus::Top));
    assert!(track(&b).iter().all(|f| *f == Tint::FgDim), "muted: the bar dimmed: {:?}", track(&b));
}

#[test]
fn a_row_whose_tool_hardware_or_command_is_missing_is_not_shown() {
    let w = World::full();
    let l = w.live();
    let rows_of = |l: &Live, b: Option<Backlight>| shown(l, &Quick::new(Focus::Top, l, two_sinks(), b, [None, None], 40));
    assert!(!rows_of(&l, None).contains(&Row::Brightness), "no backlight");
    let no_daemon = ProfileState::default();
    assert!(!rows_of(&Live { profile: Some(&no_daemon), ..l }, backlight(75)).contains(&Row::Profile), "no power-profiles-daemon");
    assert!(!rows_of(&Live { profile: None, ..l }, backlight(75)).contains(&Row::Profile), "the system bus down");
    let no_mic = SoundState { source: None, ..w.sound.clone() };
    assert_eq!(
        rows_of(&Live { sound: Some(&no_mic), ..l }, backlight(75))[..3],
        [Row::Volume, Row::Brightness, Row::Dnd],
        "no default source"
    );
    assert_eq!(rows_of(&Live { sound: None, ..l }, backlight(75))[0], Row::Brightness, "pw-dump down: no volume, no microphone");
    assert!(!rows_of(&Live { dnd: None, ..l }, backlight(75)).contains(&Row::Dnd), "no notifications module");
    let mut c = QuickConfig { lock: vec![], reboot: vec![], poweroff: vec![], ..QuickConfig::default() };
    assert!(!rows_of(&Live { config: &c, ..l }, backlight(75)).contains(&Row::Session), "every session command empty");
    c.logout = vec!["fake-logout".into()];
    let one = Live { config: &c, ..l };
    let b = rows(&one, &Quick::new(Focus::Top, &one, two_sinks(), None, [None, None], 40));
    assert_eq!(texts(&b).last().cloned(), Some(format!("   {LOGOUT}")), "one button: log out");
    let k =
        QuickConfig { keep_awake: ToggleConfig { toggle: vec!["fake-keep-awake".into()], state: String::new() }, ..QuickConfig::default() };
    assert_eq!(
        rows_of(&Live { config: &k, ..l }, None)[4..],
        [Row::KeepAwake, Row::Session],
        "a configured toggle shows; the other stays hidden"
    );
}

/// Nothing at all to show.
#[test]
fn with_nothing_to_show_the_panel_is_empty() {
    let c = QuickConfig { lock: vec![], reboot: vec![], poweroff: vec![], ..QuickConfig::default() };
    let l = Live { sound: None, dnd: None, profile: None, config: &c, bluetooth: None };
    let q = Quick::new(Focus::Sound, &l, Vec::new(), None, [None, None], 40);
    assert!(q.is_empty(&l), "nothing to open: the wiring logs it and opens nothing");
    assert!(lines(&l, &q).is_empty());
    assert_eq!(rows(&l, &q).cells(), (0, 0));
    let w = World::full();
    assert!(!quick(&w, Focus::Top).is_empty(&w.live()));
}

#[test]
fn the_section_asked_for_is_selected_at_open() {
    assert_eq!(Focus::of(&Target::Module(ModuleId::Volume)), Focus::Sound);
    assert_eq!(Focus::of(&Target::Module(ModuleId::Network)), Focus::Wifi, "quick Wi-Fi spec § 1");
    assert_eq!(quick(&World::full(), Focus::Wifi).selected, Row::Volume, "no Wi-Fi state read yet, so no Wi-Fi row: the first row");
    assert_eq!(Focus::of(&Target::Module(ModuleId::Bluetooth)), Focus::Bluetooth, "quick Bluetooth spec § 1");
    let w = World::full();
    assert_eq!(quick(&w, Focus::Sound).selected, Row::Volume);
    assert_eq!(quick(&w, Focus::Top).selected, Row::Volume, "the volume is the first row");
    let l = Live { sound: None, ..w.live() };
    assert_eq!(
        Quick::new(Focus::Sound, &l, two_sinks(), backlight(75), [None, None], 40).selected,
        Row::Brightness,
        "no volume row: the first"
    );
}

/// Quick Bluetooth spec § 1: a click on the Bluetooth module, or `--quick bluetooth`, opens its
/// detail; Enter on the row too, Escape goes back to the row.
#[test]
fn focus_bluetooth_opens_the_detail() {
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![BtDevice { name: "CasqueBT".into(), battery: Some(80) }] });
    let q = quick(&w, Focus::Bluetooth);
    assert!(q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth);
    assert_eq!(lines(&w.live(), &q)[..2], [Line::BtHeader, Line::Separator]);
    assert_eq!(q.bt_scope(), BtScope::Detail);
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Bluetooth, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
    assert_eq!(texts(&rows(&l, &q))[3], format!(" {BLUETOOTH_CONNECTED} Bluetooth{}CasqueBT ›", " ".repeat(11)));
    q.selected = Row::Bluetooth;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "no view read yet: no discovery");
    assert!(q.bt.open);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth, "back on its row");
    w.bluetooth = Some(BtState::Absent);
    let q = quick(&w, Focus::Bluetooth);
    assert!(!q.bt.open, "no Bluetooth row: nothing opens");
    assert_eq!(q.selected, Row::Volume, "the first row");
}

/// The D-Bus thread's view while the detail is open: discovery, BlueZ lost, the row gone.
#[test]
fn the_bluetooth_detail_follows_its_view() {
    use crate::quick_bt::BtOrder;
    use crate::sources::dbus::bt_act::ActCall;
    use crate::system::{BtAdapter, BtEntry, BtKind, BtView};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let adapter = "/org/bluez/hci0";
    let view = || BtView {
        adapter: Some(BtAdapter { path: adapter.into(), powered: true, blocked: false }),
        paired: vec![BtEntry {
            path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01".into(),
            label: "CasqueBT".into(),
            kind: BtKind::Headset,
            paired: true,
            trusted: true,
            connected: false,
        }],
        nearby: vec![],
    };
    let t = Instant::now();
    let mut q = quick(&w, Focus::Bluetooth);
    assert_eq!(
        q.follow_bt(&w.live(), Some(view()), t),
        QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::StartDiscovery { adapter: adapter.into() })])
    );
    assert!(q.bt.is_selected(Line::BtDevice(0), &q.bt.devices()), "the first device");
    assert_eq!(q.follow_bt(&w.live(), None, t), QuickAction::Redraw, "BlueZ lost: its orders dropped");
    assert!(!q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth, "back on the row");
    let mut q = quick(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(view()), t);
    w.bluetooth = Some(BtState::Absent);
    q.fit(&w.live());
    assert!(!q.bt.open, "the row gone: the detail closes");
    assert_eq!(q.selected, Row::Mic, "the next row down");
}

/// A Bluetooth view with ClavierBT paired, and Keyboard-Test nearby.
fn clavier_view() -> BtView {
    use crate::system::{BtAdapter, BtEntry, BtKind};
    let entry = |n: u8, label: &str, paired: bool| BtEntry {
        path: format!("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_{n:02X}"),
        label: label.into(),
        kind: BtKind::Keyboard,
        paired,
        trusted: paired,
        connected: false,
    };
    BtView {
        adapter: Some(BtAdapter { path: "/org/bluez/hci0".into(), powered: true, blocked: false }),
        paired: vec![entry(2, "ClavierBT", true)],
        nearby: vec![entry(3, "Keyboard-Test", false)],
    }
}

/// A double Enter (or a held one) on a device makes one call.
#[test]
fn a_double_enter_on_a_device_calls_once() {
    use crate::quick_bt::BtOrder;
    use crate::sources::dbus::bt_act::ActCall;
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = quick(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let connect = ActCall::Connect { device: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into() };
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Bluetooth(vec![BtOrder::Call(connect)]));
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing, "auto-repeat");
    assert_eq!(q.key(&l, Key::Enter, false, t + Duration::from_millis(50)), QuickAction::Nothing, "during …");
}

/// A held arrow while a code question shows repeats into nothing: the question that just
/// arrived is never rejected by a key pressed before it.
#[test]
fn a_held_key_never_answers_a_code_question() {
    use crate::quick_bt::{BtOrder, Code};
    use crate::sources::dbus::bt_act::{ActCall, AgentId, AgentReply, AgentRequest};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = quick(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let pairing = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03".to_string();
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::Pair { device: pairing.clone() })]));
    let id = AgentId(4);
    q.bt_agent(id, AgentRequest::RequestConfirmation { device: pairing.clone(), passkey: 482_913 }, t);
    for k in [Key::Down, Key::Up, Key::Left, Key::Right] {
        assert_eq!(q.key(&l, k, true, t), QuickAction::Nothing, "{k:?} held");
    }
    assert!(matches!(q.bt.code, Some(Code::Question { .. })), "still asked");
    assert_eq!(
        q.key(&l, Key::Down, false, t),
        QuickAction::Bluetooth(vec![BtOrder::Reply(id, AgentReply::Rejected), BtOrder::Forget(ActCall::Pair { device: pairing })]),
        "a fresh press still answers no"
    );
}

/// Spec § 3: a code question cancels a pending `Forget?`; leaving drops it too.
#[test]
fn the_question_cancels_forget() {
    use crate::quick_bt::BtOrder;
    use crate::sources::dbus::bt_act::{ActCall, AgentId, AgentReply, AgentRequest};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = quick(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let pairing = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_03".to_string();
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::Pair { device: pairing.clone() })]));
    q.confirm = Some((Question::Forget("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into()), t + CONFIRM));
    assert_eq!(q.bt_agent(AgentId(1), AgentRequest::RequestConfirmation { device: pairing.clone(), passkey: 1 }, t), QuickAction::Redraw);
    assert_eq!(q.confirm, None);
    q.confirm = Some((Question::Forget("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into()), t + CONFIRM));
    assert_eq!(
        q.bt_leaving(),
        vec![
            BtOrder::Reply(AgentId(1), AgentReply::Rejected),
            BtOrder::Call(ActCall::CancelPairing { device: pairing.clone() }),
            BtOrder::Forget(ActCall::Pair { device: pairing }),
            BtOrder::Call(ActCall::StopDiscovery { adapter: "/org/bluez/hci0".into() }),
        ]
    );
    assert_eq!(q.confirm, None, "leaving drops a pending Forget");
    q.confirm = Some((Question::Session(Session::Reboot), t + CONFIRM));
    q.bt_leaving();
    assert!(q.confirm.is_some(), "not a session question");
}

/// The Bluetooth timers join the panel's: the earliest deadline, and what `due` finds.
#[test]
fn bluetooth_deadlines_and_logs_go_through_due() {
    use crate::quick_bt::{BtOrder, CALL_DEADLINE};
    use crate::sources::dbus::bt_act::ActCall;
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = quick(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    q.key(&w.live(), Key::Enter, false, t);
    assert_eq!(q.deadline(), Some(t + CALL_DEADLINE));
    let connect = ActCall::Connect { device: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".into() };
    assert_eq!(
        q.due(t + CALL_DEADLINE),
        Due {
            redraw: true,
            bt_orders: vec![BtOrder::Forget(connect)],
            bt_logs: vec!["quick: bluetooth connect failed (timeout)".into()],
            ..Due::default()
        }
    );
    let (a, log) = q.bt_answered(
        &ActCall::StartDiscovery { adapter: "/org/bluez/hci0".into() },
        Err(crate::sources::dbus::bt_act::ActError::Remote("org.bluez.Error.InProgress".into())),
        t,
    );
    assert_eq!((a, log.as_deref()), (QuickAction::Nothing, Some("quick: bluetooth discovery failed (org.bluez.Error.InProgress)")));
}

/// Spec § 2: a left press on the device asked about is the forget question's second activation;
/// a press on any other line cancels it.
#[test]
fn a_press_on_the_asking_device_confirms_forget() {
    use crate::quick_bt::BtOrder;
    use crate::sources::dbus::bt_act::ActCall;
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = quick(&w, Focus::Bluetooth);
    q.follow_bt(&w.live(), Some(clavier_view()), t);
    let l = w.live();
    let clavier = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02".to_string();
    let row = |q: &Quick, line: Line| lines(&l, q).iter().position(|x| *x == line).unwrap() as u16;
    assert_eq!(q.key(&l, Key::Delete, false, t), QuickAction::Redraw);
    assert_eq!(q.press(&l, (3, row(&q, Line::BtNearby)), Button::Left, t), QuickAction::Redraw, "another line: cancelled");
    assert_eq!(q.confirm, None);
    q.key(&l, Key::Delete, false, t);
    assert_eq!(q.confirm, Some((Question::Forget(clavier.clone()), t + CONFIRM)));
    assert_eq!(
        q.press(&l, (3, row(&q, Line::BtDevice(0))), Button::Left, t + CONFIRM_AFTER),
        QuickAction::Bluetooth(vec![BtOrder::Call(ActCall::RemoveDevice { adapter: "/org/bluez/hci0".into(), device: clavier })])
    );
}

/// A left click on the Bluetooth row opens its detail.
#[test]
fn a_left_click_on_the_bluetooth_row_opens_the_detail() {
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    let at = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Bluetooth)).unwrap();
    assert_eq!(q.press(&l, (3, at as u16), Button::Left, Instant::now()), QuickAction::Redraw, "no view read yet");
    assert!(q.bt.open);
    assert_eq!(q.selected, Row::Bluetooth);
}

/// Spec § 2: Space on the Bluetooth row is a power action with its 30 s deadline.
#[test]
fn the_rows_power_toggle_is_armed() {
    use crate::quick_bt::{CALL_DEADLINE, Step};
    let mut w = World::full();
    w.bluetooth = Some(BtState::On { devices: vec![] });
    let t = Instant::now();
    let mut q = quick(&w, Focus::Top);
    q.bt.view = Some(clavier_view());
    q.selected = Row::Bluetooth;
    q.key(&w.live(), Key::Char(' '), false, t);
    assert_eq!(q.bt.action.as_ref().map(|a| (a.step, a.deadline)), Some((Step::Power, Some(t + CALL_DEADLINE))));
}

#[test]
fn the_selection_stays_on_its_row_when_rows_come_and_go() {
    let mut w = World::full();
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Mic;
    w.sound.source = None;
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Dnd, "the microphone unplugged: the next row down");
    w.sound.source = vol(100, false);
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Dnd, "it stays where it went: by identity, never by line");
    q.selected = Row::Session;
    w.config.lock = vec![];
    w.config.reboot = vec![];
    w.config.poweroff = vec![];
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Profile, "the last row gone: the one above");
    let mut w = World::full();
    w.sound.source = None;
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Dnd;
    w.sound.source = vol(100, false);
    q.fit(&w.live());
    assert_eq!(q.selected, Row::Dnd, "a row appearing above never moves it");
}

#[test]
fn arrows_home_and_end_move_over_the_rows_never_wrapping() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Nothing, "the first row: clamped");
    assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Brightness);
    assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Mic, "over the separator");
    assert_eq!(q.key(&l, Key::End, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Session);
    assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Nothing, "never wrapping");
    assert_eq!(q.key(&l, Key::CtrlP, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Profile);
    assert_eq!(q.key(&l, Key::Home, false, t), QuickAction::Redraw);
    assert_eq!(q.selected, Row::Volume);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Close);
}

/// A held Enter opens nothing more; the arrows repeat.
#[test]
fn a_held_space_or_enter_acts_once() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing, "a repeated Enter: nothing");
    assert!(!q.detail);
    assert_eq!(q.key(&l, Key::Down, true, t), QuickAction::Redraw, "the arrows repeat");
    assert_eq!(q.key(&l, Key::Up, true, t), QuickAction::Redraw);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "pressed: the outputs open");
    assert!(q.detail);
}

#[test]
fn the_outputs_detail_lists_every_sink_and_sets_the_default() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert!(q.detail);
    assert_eq!(q.sink, Some(122), "the default chosen at open");
    let b = rows(&l, &q);
    assert_eq!(texts(&b), [" ‹ Outputs".to_string(), String::new(), "   Speaker".to_string(), " ● AG06/AG03".to_string()]);
    assert_eq!(b.separators, [1]);
    assert_eq!(&b.rows[3].styles()[..3], "SSS", "the chosen sink on the band");
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Redraw);
    assert_eq!(q.sink, Some(99));
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Nothing, "clamped");
    assert_eq!(
        q.key(&l, Key::Enter, false, t),
        QuickAction::Run { argv: argv(&["wpctl", "set-default", "99"]), what: "default sink 99".into() }
    );
    assert!(q.detail, "the detail stays, the ● follows pw-dump");
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.detail);
    assert_eq!(q.selected, Row::Volume, "back on the volume row");
    q.key(&l, Key::Enter, false, t);
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw, "← leaves it too");
    assert!(!q.detail);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Close, "from the rows, Escape closes");
}

#[test]
fn a_single_sink_draws_no_arrow_and_opens_no_detail() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = Quick::new(Focus::Sound, &l, vec![sink(122, "AG06/AG03", true)], backlight(75), [None, None], 40);
    assert!(texts(&rows(&l, &q))[0].ends_with("64%"), "no ›");
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Nothing);
    assert!(!q.detail);
    let mut q = quick(&w, Focus::Sound);
    q.key(&l, Key::Enter, false, t);
    q.set_sinks(vec![sink(122, "AG06/AG03", true)]);
    q.fit(&l);
    assert!(!q.detail, "a sink unplugged under the detail: back to the rows");
}

#[test]
fn the_detail_keeps_its_sink_by_id() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = quick(&w, Focus::Sound);
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Up, false, t);
    assert_eq!(q.sink, Some(99));
    q.set_sinks(vec![sink(64, "HDMI", false), sink(99, "Speaker", false), sink(122, "AG06/AG03", true)]);
    q.fit(&l);
    assert_eq!(q.sink, Some(99), "a sink arriving above it moves its line, never the choice");
    assert_eq!(&rows(&l, &q).rows[3].styles()[..3], "SSS", "Speaker is now on line 3");
    q.set_sinks(vec![sink(64, "HDMI", false), sink(122, "AG06/AG03", true)]);
    q.fit(&l);
    assert_eq!(q.sink, Some(122), "its sink gone: the default");
}

#[test]
fn the_window_follows_the_selection_when_the_lines_do_not_fit() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 3);
    assert_eq!(rows(&l, &q).rows.len(), 3, "three rows of room");
    q.key(&l, Key::End, false, t);
    assert_eq!(q.top, 5, "8 lines, 3 shown: the last three");
    assert_eq!(texts(&rows(&l, &q)).last().cloned(), Some(format!("   {LOCK}    {REBOOT}    {POWER}")), "the session row in view");
    q.key(&l, Key::Home, false, t);
    assert_eq!(q.top, 0);
}

#[test]
fn re_reads_replace_the_backlight_and_the_toggles() {
    let w = World::full();
    let mut q = quick(&w, Focus::Top);
    q.set_backlight(Reread::Level(40));
    assert_eq!(q.backlight, backlight(40));
    q.set_backlight(Reread::Unreadable);
    assert_eq!(q.backlight, backlight(40), "one failed read: the row and its last level stay");
    q.set_backlight(Reread::Gone);
    assert!(!shown(&w.live(), &q).contains(&Row::Brightness), "the device went: its row with it");
    q.set_toggle(Toggle::NightLight, Some(true));
    assert_eq!(q.toggles, [None, Some(true)]);
}

const DND_LINE: u16 = 4;
const SESSION_LINE: u16 = 7;

/// The `i`-th instant a slider may send again: each a full COALESCE after the one before.
fn later(t: Instant, i: u32) -> Instant {
    t + COALESCE * i
}

fn set_volume(n: u8) -> QuickAction {
    run(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", &format!("{n}%")], &format!("volume {n}%"))
}

fn set_brightness(n: u8) -> QuickAction {
    run(&["brightnessctl", "--class=backlight", "--device=intel_backlight", "set", &format!("{n}%")], &format!("brightness {n}%"))
}

/// Fix S1: every volume command an absolute level the model computed from what it shows.
#[test]
fn volume_steps_mutes_and_takes_a_click_on_its_bar() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Sound);
    let t = Instant::now();
    let mute = run(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"], "mute");
    assert_eq!(q.key(&l, Key::Right, false, later(t, 0)), set_volume(69), "64 % + 5");
    assert!(texts(&rows(&l, &q))[0].contains(" 69%"), "the expected level shown at once");
    assert_eq!(q.key(&l, Key::Left, false, later(t, 1)), set_volume(64), "from the level shown, not pw-dump's");
    assert_eq!(q.key(&l, Key::Right, true, later(t, 2)), set_volume(69), "← → repeat");
    assert_eq!(q.key(&l, Key::Char(' '), false, later(t, 2)), mute);
    assert_eq!(q.press(&l, (BAR_START, 0), Button::Left, later(t, 3)), set_volume(4), "the bar's first cell");
    assert_eq!(q.press(&l, (BAR_START + BAR - 1, 0), Button::Left, later(t, 4)), set_volume(100), "its last cell");
    assert_eq!(q.press(&l, (BAR_START + 10, 0), Button::Left, later(t, 5)), set_volume(50), "(10 + 1) × 100 / 22");
    assert_eq!(q.press(&l, (1, 0), Button::Right, later(t, 5)), mute, "a right click on the volume: mute");
    assert_eq!(q.scroll(&l, Some((5, 0)), -1, later(t, 6)), set_volume(55), "the wheel up: louder");
    assert_eq!(q.scroll(&l, Some((5, 0)), 2, later(t, 7)), set_volume(45), "two notches, one command");
    assert_eq!(q.press(&l, (MORE_AT, 0), Button::Left, later(t, 8)), QuickAction::Redraw, "› opens the outputs");
    assert!(q.detail);
    let ten = World { config: QuickConfig { step: 10, ..QuickConfig::default() }, ..World::full() };
    let mut q = quick(&ten, Focus::Sound);
    assert_eq!(q.key(&ten.live(), Key::Right, false, t), set_volume(74), "[quick] step");
    let mut loud = World::full();
    loud.sound.sink = vol(97, false);
    let mut q = quick(&loud, Focus::Sound);
    assert_eq!(q.key(&loud.live(), Key::Right, false, t), set_volume(100), "never beyond 100 %");
}

/// Fix S1: a held → sends at most one command per COALESCE, each the latest level; a burst
/// inside one window is two commands, the second the level the burst ended on.
#[test]
fn a_held_slider_key_sends_the_latest_level_once_per_window() {
    let mut w = World::full();
    w.sound.sink = vol(30, false);
    let l = w.live();
    let mut q = quick(&w, Focus::Sound);
    let t = Instant::now();
    let burst: Vec<QuickAction> = (0..10).map(|i| q.key(&l, Key::Right, i > 0, t + Duration::from_millis(15 * i))).collect();
    assert_eq!(burst[0], set_volume(35), "the first goes at once");
    assert!(burst[1..].iter().all(|a| *a == QuickAction::Redraw), "the rest wait: {burst:?}");
    assert!(texts(&rows(&l, &q))[0].contains(" 80%"), "the level shown follows every key");
    assert_eq!(q.deadline(), Some(t + COALESCE), "the window's end");
    assert_eq!(q.due(t + COALESCE), Due { run: vec![set_volume(80)], ..Due::default() }, "the latest level, once");
    assert_eq!(q.deadline(), Some(t + COALESCE * 2), "a new window after it");
    assert_eq!(q.due(t + COALESCE * 2), Due::default(), "quiet, pw-dump still at 30 %: the level sent stays");
    assert!(texts(&rows(&l, &q))[0].contains(" 80%"));
    assert_eq!(q.deadline(), Some(t + COALESCE * 2 + SETTLE), "for SETTLE at most");
    assert_eq!(q.due(t + COALESCE * 2 + SETTLE), Due { redraw: true, ..Due::default() }, "then pw-dump's level");
    assert_eq!(q.deadline(), None, "nothing pending: no timer");
    assert!(texts(&rows(&l, &q))[0].contains(" 30%"), "pw-dump's own level again");
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Brightness;
    let burst: Vec<QuickAction> = (0..10).map(|i| q.key(&l, Key::Right, i > 0, t + Duration::from_millis(15 * i))).collect();
    assert_eq!(burst[0], set_brightness(80), "75 % + 5");
    assert!(burst[1..].iter().all(|a| *a == QuickAction::Redraw), "{burst:?}");
    assert_eq!(q.due(t + COALESCE), Due { run: vec![set_brightness(100)], ..Due::default() });
    assert_eq!(q.due(t + COALESCE * 2), Due { backlight: true, ..Due::default() }, "quiet: the re-read");
    assert_eq!(q.key(&l, Key::Left, false, t + COALESCE * 3), set_brightness(95), "a new window: at once");
    assert_eq!(q.key(&l, Key::Left, true, t + COALESCE * 3), QuickAction::Redraw);
    assert_eq!(q.flush(), vec![set_brightness(90)], "closing sends what waits");
    assert_eq!(q.flush(), vec![], "once");
}

/// wpctl and pw-dump (or brightnessctl) slower than a window. The level sent stays
/// until the live source reports it, or SETTLE passes: a stale reading never makes the next
/// press step backwards.
#[test]
fn a_stale_live_level_never_makes_the_next_press_step_backwards() {
    let mut w = World::full();
    w.sound.sink = vol(30, false);
    let mut q = quick(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&w.live(), Key::Right, false, t), set_volume(35));
    assert_eq!(q.key(&w.live(), Key::Right, true, t + Duration::from_millis(10)), QuickAction::Redraw);
    assert_eq!(q.due(later(t, 1)).run, vec![set_volume(40)]);
    w.sound.sink = vol(35, false); // pw-dump one command behind
    q.fit(&w.live());
    assert_eq!(q.due(later(t, 2)), Due::default(), "quiet, pw-dump still behind");
    q.fit(&w.live());
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 2)), set_volume(45), "from 40 % shown, never 35 %");
    q.due(later(t, 3));
    w.sound.sink = vol(45, false);
    q.fit(&w.live());
    assert_eq!(q.volume_level, None, "pw-dump caught up: its level from now on");
    assert_eq!(q.deadline(), None, "and nothing left to wait for");
    q.selected = Row::Brightness;
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 4)), set_brightness(80));
    assert_eq!(q.due(later(t, 5)), Due { backlight: true, ..Due::default() });
    q.set_backlight(Reread::Level(75)); // read before brightnessctl wrote
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 5)), set_brightness(85), "from 80 % shown, never 75 %");
}

/// The default sink switched while a level is held or waiting: that level was the
/// old sink's; it is dropped, never sent to the new one.
#[test]
fn a_default_sink_switch_drops_the_held_volume() {
    let mut w = World::full();
    w.sound.sink_name = Some(crate::system::DeviceName { node: "alsa_output.ag06".into(), label: "AG06/AG03".into() });
    let mut q = quick(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.key(&w.live(), Key::Right, false, t), set_volume(69));
    assert_eq!(q.key(&w.live(), Key::Right, true, t + Duration::from_millis(10)), QuickAction::Redraw, "74 % waits");
    w.sound.sink_name = Some(crate::system::DeviceName { node: "alsa_output.speaker".into(), label: "Speaker".into() });
    w.sound.sink = vol(40, false);
    q.fit(&w.live());
    assert_eq!(q.due(later(t, 1)).run, vec![], "never sent to the new sink");
    assert_eq!(q.flush(), vec![]);
    assert!(texts(&rows(&w.live(), &q))[0].contains(" 40%"), "the new sink's level");
    assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 2)), set_volume(45), "stepped from it");
}

#[test]
fn brightness_shows_the_expected_level_until_its_re_read() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Brightness;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Right, false, t), set_brightness(80));
    assert_eq!(q.backlight, backlight(80), "the expected level at once");
    assert_eq!(q.deadline(), Some(t + BRIGHTNESS_REREAD));
    assert_eq!(q.due(t + Duration::from_millis(149)), Due::default(), "not yet");
    assert_eq!(q.due(t + BRIGHTNESS_REREAD), Due { backlight: true, ..Due::default() });
    q.set_backlight(Reread::Level(80));
    assert_eq!(q.deadline(), None, "the re-read agrees: nothing pending, no timer");
    q.key(&l, Key::Left, false, later(t, 1));
    q.due(later(t, 2));
    q.set_backlight(Reread::Level(78));
    assert_eq!(q.backlight, backlight(75), "a re-read that disagrees is kept back for SETTLE");
    assert_eq!(q.due(later(t, 2) + SETTLE), Due { backlight: true, ..Due::default() }, "one more re-read");
    q.set_backlight(Reread::Level(78));
    assert_eq!(q.backlight, backlight(78), "then the re-read wins");
    assert_eq!(q.deadline(), None);
    q.backlight = backlight(98);
    q.key(&l, Key::Right, false, later(t, 2));
    assert_eq!(q.backlight, backlight(100), "never above 100");
    assert_eq!(q.press(&l, (BAR_START + 10, 1), Button::Left, later(t, 3)), set_brightness(50), "a click on its bar");
    assert_eq!(q.scroll(&l, Some((5, 1)), -2, later(t, 4)), set_brightness(60));
}

/// Mae's ruling M2: the panel never takes the backlight below 1 % (a black screen), by a key,
/// the wheel or a click.
#[test]
fn brightness_never_goes_below_one_percent() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Brightness;
    let t = Instant::now();
    q.backlight = backlight(3);
    assert_eq!(q.key(&l, Key::Left, false, later(t, 0)), set_brightness(1), "3 % − 5: 1 %");
    assert_eq!(q.backlight, backlight(1));
    assert_eq!(q.key(&l, Key::Left, false, later(t, 1)), set_brightness(1), "at 1 %: stays");
    q.backlight = backlight(40);
    assert_eq!(q.scroll(&l, Some((5, 1)), 10, later(t, 2)), set_brightness(1), "ten notches down");
    assert_eq!(q.press(&l, (BAR_START, 1), Button::Left, later(t, 3)), set_brightness(4), "the bar's first cell");
    q.backlight = backlight(0);
    assert_eq!(q.key(&l, Key::Right, false, later(t, 4)), set_brightness(5), "from a dark backlight, up");
}

/// Fix m4: the wheel counts ten notches at most.
#[test]
fn the_wheel_takes_ten_notches_at_most() {
    let mut w = World::full();
    w.sound.sink = vol(30, false);
    let l = w.live();
    let mut q = quick(&w, Focus::Sound);
    let t = Instant::now();
    assert_eq!(q.scroll(&l, Some((5, 0)), -15, t), set_volume(80), "15 notches of 5: 50, not 75");
    q.selected = Row::Brightness;
    q.backlight = backlight(80);
    assert_eq!(q.scroll(&l, Some((5, 1)), 15, t), set_brightness(30), "15 notches down: 50");
}

/// Fix m3: `BAR` is the width `osd::level` draws, from `BAR_START` (the click mapping counts on it).
#[test]
fn the_drawn_bar_spans_bar_start_and_its_bar_cells() {
    let mut g = Grid::empty(WIDTH + 2);
    slider(&mut g, BRIGHTNESS, Volume { percent: 100, muted: false }, false, false);
    let styles: Vec<char> = g.styles().chars().collect();
    let (first, end) = (usize::from(BAR_START), usize::from(BAR_START + BAR));
    assert!(styles[first..end].iter().all(|c| *c == 'a'), "{}", g.styles());
    assert_ne!(styles[first - 1], 'a', "the cell before the bar: {}", g.styles());
    assert_ne!(styles[end], 'a', "the cell after it: {}", g.styles());
}

#[test]
fn the_toggles_run_their_command_and_re_read_their_state() {
    let mut w = World::full();
    w.config.keep_awake.toggle = vec!["fake-keep-awake".into()];
    w.config.night_light.toggle = vec!["fake-night".into(), "--toggle".into()];
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    let t = Instant::now();
    let mic = run(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"], "microphone mute");
    q.selected = Row::Mic;
    assert_eq!(q.key(&l, Key::Char(' '), false, t), mic);
    assert_eq!(q.key(&l, Key::Enter, false, t), mic);
    q.selected = Row::Dnd;
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::ToggleDnd, "set_dnd's one path, in the wiring");
    assert_eq!(q.key(&l, Key::Char(' '), true, t), QuickAction::Nothing, "a held Space acts once");
    q.selected = Row::KeepAwake;
    assert_eq!(q.key(&l, Key::Enter, false, t), run(&["fake-keep-awake"], "keep awake"), "run as given");
    q.selected = Row::NightLight;
    assert_eq!(q.key(&l, Key::Char(' '), false, t + Duration::from_millis(100)), run(&["fake-night", "--toggle"], "night light"));
    assert_eq!(q.deadline(), Some(t + TOGGLE_REREAD), "keep awake's re-read comes first");
    assert_eq!(q.due(t + TOGGLE_REREAD), Due { toggles: [true, false], ..Due::default() });
    assert_eq!(q.due(t + TOGGLE_REREAD + Duration::from_millis(100)), Due { toggles: [false, true], ..Due::default() });
    q.selected = Row::Volume;
    assert_eq!(q.press(&l, (10, 4), Button::Left, t), QuickAction::ToggleDnd, "a left click on a row is Enter");
    assert_eq!(q.selected, Row::Dnd);
}

#[test]
fn enter_cycles_the_power_profile_and_space_does_not() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Profile;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), run(&["powerprofilesctl", "set", "performance"], "profile performance"));
    assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Nothing, "space does not cycle the profile");
    let top = ProfileState { active: Some(Profile::Performance), offered: Profile::ALL.to_vec() };
    assert_eq!(
        q.key(&Live { profile: Some(&top), ..l }, Key::Enter, false, t),
        run(&["powerprofilesctl", "set", "power-saver"], "profile power-saver"),
        "wrapping"
    );
    let two = ProfileState { active: Some(Profile::Balanced), offered: vec![Profile::PowerSaver, Profile::Balanced] };
    assert_eq!(
        q.key(&Live { profile: Some(&two), ..l }, Key::Enter, false, t),
        run(&["powerprofilesctl", "set", "power-saver"], "profile power-saver"),
        "only what the daemon offers"
    );
}

#[test]
fn reboot_and_power_off_ask_first_and_the_question_expires() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Session;
    let t = Instant::now();
    let lock = QuickAction::RunAndClose { argv: argv(&["loginctl", "lock-session"]), what: "lock".into() };
    assert_eq!(q.key(&l, Key::Enter, false, t), lock, "lock: at once");
    q.button = Session::Reboot;
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "the first activation asks");
    assert_eq!(q.confirm, Some((Question::Session(Session::Reboot), t + CONFIRM)));
    let b = rows(&l, &q);
    assert_eq!(texts(&b)[7], format!(" {REBOOT} Reboot? ⏎"));
    assert_eq!(b.rows[7].styles()[1..12], "w".repeat(11), "in warn");
    assert_eq!(q.deadline(), Some(t + CONFIRM), "the timer waits for it");
    let early = t + Duration::from_millis(200);
    assert_eq!(q.key(&l, Key::Enter, false, early), QuickAction::Nothing, "Mae (M1): a double activation never confirms");
    assert_eq!(q.press(&l, (20, SESSION_LINE), Button::Left, early), QuickAction::Nothing, "nor a double click");
    assert_eq!(q.confirm, Some((Question::Session(Session::Reboot), t + CONFIRM)), "the question stays, its 3 s unchanged");
    assert_eq!(
        q.key(&l, Key::Enter, false, t + Duration::from_millis(500)),
        QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
        "0.5 s later: it runs"
    );
    q.button = Session::Reboot;
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert_eq!(
        q.key(&l, Key::Enter, false, t + Duration::from_secs(2)),
        QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
        "the second within 3 s runs it"
    );
    q.button = Session::Poweroff;
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert_eq!(q.due(t + CONFIRM), Due { redraw: true, ..Due::default() }, "3 s later the question goes");
    assert_eq!(q.confirm, None);
    assert_eq!(q.key(&l, Key::Enter, false, t + CONFIRM), QuickAction::Redraw, "the next Enter asks again");
    assert_eq!(texts(&rows(&l, &q))[7], format!(" {POWER} Power off? ⏎"));
}

#[test]
fn moving_away_cancels_the_confirmation_and_coming_back_asks_again() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    q.selected = Row::Session;
    q.button = Session::Reboot;
    let t = Instant::now();
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw, "← moves to lock");
    assert_eq!((q.confirm.clone(), q.button), (None, Session::Lock), "and cancels");
    q.key(&l, Key::Right, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "back on reboot: it asks again, never runs");
    assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Redraw);
    assert_eq!(q.confirm, None, "↑ cancels too");
    q.selected = Row::Session;
    q.key(&l, Key::Enter, false, t);
    assert_eq!(q.press(&l, (10, DND_LINE), Button::Left, t), QuickAction::ToggleDnd, "a press on another row");
    assert_eq!(q.confirm, None, "cancels it");
    assert_eq!(q.press(&l, (SESSION_AT + SESSION_GAP, SESSION_LINE), Button::Left, t), QuickAction::Redraw, "a press on reboot asks");
    assert_eq!(
        q.press(&l, (20, SESSION_LINE), Button::Left, t + CONFIRM_AFTER),
        QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
        "anywhere on the question: the second activation"
    );
}

#[test]
fn the_wheel_elsewhere_moves_the_window_only_when_the_lines_do_not_fit() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = quick(&w, Focus::Top);
    assert_eq!(q.scroll(&l, Some((10, 4)), 1, t), QuickAction::Nothing, "everything fits");
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 3);
    assert_eq!(q.scroll(&l, Some((10, 2)), 1, t), QuickAction::Redraw, "on the separator, three rows of room");
    assert_eq!(q.top, 1);
    assert_eq!(q.scroll(&l, Some((10, 2)), 10, t), QuickAction::Redraw);
    assert_eq!(q.top, 5, "clamped: the last three lines");
    assert_eq!(q.scroll(&l, Some((10, 2)), -1, t), QuickAction::Redraw);
    assert_eq!(q.top, 4);
    assert_eq!(q.scroll(&l, None, 0, t), QuickAction::Nothing);
}

#[test]
fn a_program_that_fails_is_noted_once_per_open() {
    let w = World::full();
    let mut q = quick(&w, Focus::Top);
    assert!(q.note_failure("wpctl"));
    assert!(!q.note_failure("wpctl"), "logged once");
    assert!(q.note_failure("brightnessctl"));
}

const HOME: &str = "11111111-2222-4333-8444-555555555555";

fn home() -> AccessPoint {
    AccessPoint { ssid: b"Maison-Test".to_vec(), strength: 80, security: Security::Psk }
}

/// Maison-Test (80 %, saved), Bureau-Test (60 %, WPA2); `connected` to the first; `listed`: the
/// detail's lists read.
fn wifi_state(enabled: bool, connected: bool, listed: bool) -> WifiState {
    let aps = vec![home(), AccessPoint { ssid: b"Bureau-Test".to_vec(), strength: 60, security: Security::Psk }];
    WifiState {
        enabled,
        device: Some(WifiDevice {
            state: if connected { 100 } else { 30 },
            active: connected.then(home),
            active_uuid: connected.then(|| HOME.to_string()),
            access_points: listed.then_some(aps),
            saved: listed.then(|| vec![SavedWifi { uuid: HOME.into(), ssid: b"Maison-Test".to_vec() }]),
        }),
        stale: false,
    }
}

fn run_of(parts: &[&str], what: &str) -> QuickAction {
    QuickAction::Run { argv: argv(parts), what: what.into() }
}

#[test]
fn the_wifi_row_in_every_state() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    assert!(!shown(&l, &q).contains(&Row::Wifi), "nothing read yet: hidden");
    q.set_wifi(Some(WifiState { enabled: true, device: None, stale: false }), t);
    assert!(!shown(&l, &q).contains(&Row::Wifi), "no Wi-Fi device: hidden");
    q.set_wifi(Some(wifi_state(true, true, false)), t);
    assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Wifi, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
    assert_eq!(lines(&l, &q)[2..4], [Line::Separator, Line::Row(Row::Wifi)], "first under the sliders' separator");
    assert_eq!(texts(&rows(&l, &q))[3], format!(" {} Wi-Fi{}Maison-Test ›", STRENGTH[3], " ".repeat(12)), "80 %: four steps");
    q.set_wifi(Some(wifi_state(true, false, false)), t);
    assert_eq!(texts(&rows(&l, &q))[3], format!(" {WIFI_ON} Wi-Fi{}on ›", " ".repeat(21)));
    q.set_wifi(Some(wifi_state(false, false, false)), t);
    assert_eq!(texts(&rows(&l, &q))[3], format!(" {WIFI_OFF} Wi-Fi{}off ›", " ".repeat(20)));
    assert!(describe(&l, &q).starts_with("rows: volume brightness wifi mic"), "{}", describe(&l, &q));
    q.set_wifi(None, t);
    assert!(!shown(&l, &q).contains(&Row::Wifi), "NetworkManager gone: hidden");
}

#[test]
fn space_toggles_the_radio_and_enter_right_or_a_click_open_the_detail() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let rescan = run_of(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
    let mut q = quick(&w, Focus::Top);
    q.set_wifi(Some(wifi_state(true, true, true)), t);
    q.selected = Row::Wifi;
    assert_eq!(q.key(&l, Key::Char(' '), false, t), run_of(&["nmcli", "radio", "wifi", "off"], "wifi off"));
    assert_eq!(q.key(&l, Key::Enter, false, t), rescan, "one rescan as it opens");
    assert!(q.wifi.open && lines(&l, &q)[0] == Line::WifiHeader);
    assert_eq!(q.wifi_scope(), WifiScope::Detail);
    assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
    assert!(!q.wifi.open && q.selected == Row::Wifi, "Escape: back on the Wi-Fi row, the panel open");
    assert_eq!(q.wifi_scope(), WifiScope::Row);
    assert_eq!(q.key(&l, Key::Right, false, t), rescan, "→ opens it too");
    assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw);
    assert!(!q.wifi.open, "← back");
    q.selected = Row::Volume;
    assert_eq!(q.press(&l, (5, 3), Button::Left, t), rescan, "a click on the Wi-Fi row");
    assert_eq!(q.press(&l, (5, 0), Button::Left, t), QuickAction::Redraw, "a click on the header goes back");
    assert!(!q.wifi.open && q.selected == Row::Wifi);
    q.set_wifi(Some(wifi_state(false, false, false)), t);
    assert_eq!(q.key(&l, Key::Char(' '), false, t), run_of(&["nmcli", "radio", "wifi", "on"], "wifi on"));
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "off: the detail opens, no rescan");
    assert_eq!(texts(&rows(&l, &q))[2].trim(), OFF_NOTE);
}

/// Quick Wi-Fi spec § 1: from the network module (or `--quick wifi`), the first Wi-Fi
/// state with a device selects the row and opens the detail, once.
#[test]
fn the_network_module_opens_the_detail_once_the_state_arrives() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let rescan = run_of(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
    let mut q = quick(&w, Focus::Wifi);
    assert_eq!(q.selected, Row::Volume, "nothing read yet: the first row");
    assert_eq!(q.set_wifi(Some(wifi_state(true, true, false)), t), WifiNews { action: rescan, failed: None });
    assert!(q.wifi.open && q.selected == Row::Wifi);
    q.key(&l, Key::Escape, false, t);
    assert_eq!(q.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "once only");
    assert!(!q.wifi.open);
    let mut none = quick(&w, Focus::Wifi);
    assert_eq!(none.set_wifi(Some(WifiState { enabled: true, device: None, stale: false }), t).action, QuickAction::Redraw);
    assert!(!none.wifi.open && none.selected == Row::Volume, "no device: nothing to open");
    none.set_wifi(Some(wifi_state(true, true, false)), t);
    assert!(none.wifi.open && none.selected == Row::Wifi, "spent only on a state with a device");
    let mut top = quick(&w, Focus::Top);
    assert_eq!(top.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "the top: no detail");
}

/// The focus asked at open waits for the user only until they act. Their first key,
/// click or wheel notch drops it: a device appearing later moves nothing and opens no detail.
#[test]
fn the_users_first_key_or_click_drops_a_pending_wifi_focus() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let no_device = || Some(WifiState { enabled: true, device: None, stale: false });
    let mut keyed = quick(&w, Focus::Wifi);
    keyed.set_wifi(no_device(), t);
    keyed.key(&l, Key::Down, false, t);
    let chosen = keyed.selected;
    assert_eq!(keyed.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "a key: no rescan");
    assert!(!keyed.wifi.open && keyed.selected == chosen, "a key: the selection stays the user's");
    let mut clicked = quick(&w, Focus::Wifi);
    clicked.set_wifi(no_device(), t);
    clicked.press(&l, (5, 1), Button::Left, t);
    let chosen = clicked.selected;
    assert_eq!(clicked.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "a click: no rescan");
    assert!(!clicked.wifi.open && clicked.selected == chosen, "a click: the selection stays the user's");
    let mut wheeled = quick(&w, Focus::Wifi);
    wheeled.set_wifi(no_device(), t);
    wheeled.scroll(&l, Some((5, 1)), 1, t);
    wheeled.set_wifi(Some(wifi_state(true, true, false)), t);
    assert!(!wheeled.wifi.open, "a wheel notch: no detail");
    let mut untouched = quick(&w, Focus::Wifi);
    untouched.set_wifi(no_device(), t);
    untouched.key(&l, Key::Enter, true, t);
    untouched.set_wifi(Some(wifi_state(true, true, false)), t);
    assert!(untouched.wifi.open, "an auto-repeat ignored by the panel is no act of the user's");
}

#[test]
fn the_window_follows_the_selection_in_the_wifi_detail() {
    let config = QuickConfig { wifi_more: Some(vec!["fake-more".into()]), ..QuickConfig::default() };
    let (w, t) = (World::full(), Instant::now());
    let mut l = w.live();
    l.config = &config;
    let mut s = wifi_state(true, false, true);
    let aps: Vec<AccessPoint> =
        (0..20).map(|n| AccessPoint { ssid: format!("Reseau-Test-{n:02}").into_bytes(), strength: 50, security: Security::Psk }).collect();
    s.device.as_mut().expect("a device").access_points = Some(aps);
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 6);
    q.set_wifi(Some(s), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::End, false, t);
    let b = rows(&l, &q);
    assert_eq!(b.rows.len(), 6, "the room under the bar");
    assert_eq!(texts(&b).last().map(|s| s.trim().to_string()), Some("More…".to_string()), "More… in view: {:?}", texts(&b));
}

/// A Wi-Fi state while open (`follow_wifi`) goes through `fit`; the window
/// follows the selection only when the state moved it (a reorder), never undoing a wheel scroll
/// otherwise; a state that closes the detail (the device gone) brings the window back to the top.
#[test]
fn a_wifi_state_moves_the_window_with_the_selection_and_a_closed_detail_goes_back_to_the_top() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    // Twenty networks at 50 %, Labo-Test (a name short enough to show whole) at `first`:
    // first in the list at 90, last at 10.
    let around = |first: u8| {
        let aps = (0..20)
            .map(|n| AccessPoint {
                ssid: if n == 0 { b"Labo-Test".to_vec() } else { format!("Reseau-Test-{n:02}").into_bytes() },
                strength: if n == 0 { first } else { 50 },
                security: Security::Psk,
            })
            .collect();
        let device = WifiDevice { state: 30, active: None, active_uuid: None, access_points: Some(aps), saved: Some(vec![]) };
        WifiState { enabled: true, device: Some(device), stale: false }
    };
    let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 6);
    q.follow_wifi(&l, Some(around(90)), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Home, false, t);
    assert_eq!(q.top, 0);
    let picked = |q: &Quick| texts(&rows(&l, q)).iter().any(|r| r.contains("Labo-Test"));
    assert!(picked(&q), "first: in view");
    q.follow_wifi(&l, Some(around(10)), t);
    assert!(q.top > 0 && picked(&q), "moved to the end by the state: the window follows it ({})", q.top);
    q.scroll(&l, None, -3, t);
    let scrolled = q.top;
    assert!(!picked(&q), "scrolled away from the selection");
    q.follow_wifi(&l, Some(around(10)), t);
    assert_eq!(q.top, scrolled, "a state that moves nothing keeps the wheel's window");
    q.follow_wifi(&l, Some(WifiState { enabled: true, device: None, stale: false }), t);
    assert!(!q.wifi.open, "the device gone: the detail closed");
    assert_eq!(q.top, 0, "back to the top");
}

/// The panel's one timer covers the connect timeout and the `failed` display (quick Wi-Fi § 5).
#[test]
fn the_wifi_attempt_rides_the_panel_s_one_timer() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    q.set_wifi(Some(wifi_state(true, true, true)), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Down, false, t);
    assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "Bureau-Test: the field");
    for c in "fake-pass".chars() {
        q.key(&l, Key::Char(c), false, t);
    }
    assert!(matches!(q.key(&l, Key::Enter, false, t), QuickAction::RunSecret { .. }));
    assert_eq!(q.deadline(), Some(t + crate::quick_wifi::CONNECT_TIMEOUT));
    let d = q.due(t + crate::quick_wifi::CONNECT_TIMEOUT);
    assert_eq!((d.wifi_failed, d.redraw), (Some("no answer in 20 s"), true));
    assert_eq!(q.deadline(), Some(t + crate::quick_wifi::CONNECT_TIMEOUT + crate::quick_wifi::FAILED_SHOWN));
}

/// A held Backspace keeps erasing in the password field; elsewhere it
/// does not repeat, nor does a held character.
#[test]
fn backspace_repeats_in_the_password_field_only() {
    let (w, t) = (World::full(), Instant::now());
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    q.set_wifi(Some(wifi_state(true, true, true)), t);
    q.selected = Row::Wifi;
    q.key(&l, Key::Enter, false, t);
    q.key(&l, Key::Down, false, t);
    q.key(&l, Key::Enter, false, t);
    for c in "abc".chars() {
        q.key(&l, Key::Char(c), false, t);
    }
    assert_eq!(q.key(&l, Key::Char('d'), true, t), QuickAction::Nothing, "a held character: once");
    assert_eq!(q.key(&l, Key::Backspace, true, t), QuickAction::Redraw, "a held Backspace erases");
    assert_eq!(q.wifi.password().map(|s| s.bytes()), Some(&b"ab"[..]));
    q.key(&l, Key::Escape, false, t);
    assert_eq!(q.key(&l, Key::Backspace, true, t), QuickAction::Nothing, "no field: no repeat");
}

fn casque(active: HeadsetMode) -> Option<Headset> {
    Some(Headset { device: 61, label: "CasqueBT".into(), active, hifi: 1, mic: 3 })
}

#[test]
fn the_headset_row_switches_to_the_other_kind() {
    let w = World::full();
    let l = w.live();
    let t = Instant::now();
    let mut q = quick(&w, Focus::Sound);
    q.set_headset(casque(HeadsetMode::HiFi));
    q.selected = Row::Headset;
    let to_mic = QuickAction::Run { argv: argv(&["wpctl", "set-profile", "61", "3"]), what: "headset mode mic".into() };
    for k in [Key::Enter, Key::Char(' '), Key::Left, Key::Right] {
        assert_eq!(q.clone().key(&l, k, false, t), to_mic, "{k:?}");
    }
    let y = lines(&l, &q).iter().position(|x| *x == Line::Row(Row::Headset)).expect("shown") as u16;
    assert_eq!(q.clone().press(&l, (10, y), Button::Left, t), to_mic, "a click is Enter");
    q.set_headset(casque(HeadsetMode::Mic));
    let to_hifi = QuickAction::Run { argv: argv(&["wpctl", "set-profile", "61", "1"]), what: "headset mode hi-fi".into() };
    assert_eq!(q.clone().key(&l, Key::Right, false, t), to_hifi);
    assert_eq!(q.key(&l, Key::Right, true, t), QuickAction::Nothing, "a held arrow switches once");
}

#[test]
fn the_headset_row_shows_under_the_volume_only_with_a_headset() {
    let w = World::full();
    let l = w.live();
    let mut q = quick(&w, Focus::Top);
    assert!(!shown(&l, &q).contains(&Row::Headset));
    q.set_headset(casque(HeadsetMode::HiFi));
    assert_eq!(shown(&l, &q)[..3], [Row::Volume, Row::Headset, Row::Brightness]);
    assert_eq!(lines(&l, &q)[..4], [Line::Row(Row::Volume), Line::Row(Row::Headset), Line::Row(Row::Brightness), Line::Separator]);
    let t = texts(&rows(&l, &q));
    assert!(t[1].contains("CasqueBT") && t[1].ends_with("‹ Hi-Fi ›"), "{:?}", t[1]);
    q.set_headset(casque(HeadsetMode::Mic));
    assert!(texts(&rows(&l, &q))[1].ends_with("‹ Mic ›"));
    assert!(describe(&l, &q).contains("rows: volume headset brightness"));
    q.selected = Row::Headset;
    q.set_headset(None);
    q.fit(&l);
    assert_eq!(q.selected, Row::Brightness, "its row gone: the next one below");
}
