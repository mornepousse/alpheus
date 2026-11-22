//! Tests of the hooks' model (`mod.rs`): which changes fire which event, the variables each gets, the
//! quiet window and the token bucket. No clock, no file, no daemon: each test passes the instant it
//! wants.

use super::*;
use crate::config::HooksConfig;
use crate::privacy::Indicator;
use crate::system::BtDevice;

/// The `[hooks]` table with the given keys set, each to a command named after it.
fn config_with(keys: &[&str]) -> HooksConfig {
    let text = format!("[hooks]\n{}", keys.iter().map(|k| format!("{k} = [\"hook-{k}\"]\n")).collect::<String>());
    crate::config::BarConfig::from_text(&text).unwrap().hooks
}

/// The model of a bar that has a hook on each of `keys`, started at `t0`.
fn hooks_with(keys: &[&str], t0: Instant) -> Hooks {
    Hooks::new(&config_with(keys), t0).unwrap()
}

/// What the network source says when online over `link`.
fn online_message(link: Link, vpns: usize, limited: bool) -> Msg {
    Msg::Network(Some(NetState::Online { link, vpns, limited }))
}

/// What the Bluetooth source says when the adapter is on and these devices are connected.
fn bluetooth_message(names: &[&str]) -> Msg {
    Msg::Bluetooth(Some(BtState::On { devices: names.iter().map(|n| BtDevice { name: n.to_string(), battery: None }).collect() }))
}

/// What the power source says for a laptop battery at this charge.
fn battery_message(percent: u8, charge: Charge) -> Msg {
    Msg::Battery(Some(PowerState { laptop: Some(Battery { percent, charge }), peripherals: vec![] }))
}

/// What the power-profile source says when `p` is active.
fn profile_message(p: Profile) -> Msg {
    Msg::Profile(Some(ProfileState { active: Some(p), offered: vec![] }))
}

/// Indicators with exactly these captures on.
fn captures_on(kinds: &[CaptureKind]) -> Capture {
    let ind = |k| Indicator { names: vec![], unnamed: kinds.contains(&k) };
    Capture { mic: ind(CaptureKind::Microphone), camera: ind(CaptureKind::Camera), screen: ind(CaptureKind::Screen) }
}

#[test]
fn no_hook_set_gives_no_model() {
    assert!(Hooks::new(&HooksConfig::default(), Instant::now()).is_none());
}

#[test]
fn the_events_are_listed_in_the_order_of_the_config_keys() {
    let keys: Vec<&str> = HookId::ALL.iter().map(|i| i.key()).collect();
    let want: Vec<&str> = HooksConfig::default().commands().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, want);
}

#[test]
fn the_first_value_of_each_event_is_a_seed_and_never_fires() {
    let t0 = Instant::now();
    let mut h = hooks_with(&["network", "bluetooth", "battery_charge", "battery_level", "power_profile", "theme_mode"], t0);
    assert!(h.on_msg(&online_message(Link::Wifi, 0, false)).is_empty());
    assert!(h.on_msg(&online_message(Link::Wifi, 0, false)).is_empty());
    assert_eq!(h.on_msg(&online_message(Link::Ethernet, 0, false)), [Event::Network(Some(Link::Ethernet))]);
    assert!(h.on_msg(&bluetooth_message(&["A"])).is_empty());
    assert!(h.on_msg(&bluetooth_message(&["A"])).is_empty());
    assert_eq!(h.on_msg(&bluetooth_message(&[])), [Event::Bluetooth(BtChange::Disconnected("A".into()))]);
    assert!(h.on_msg(&battery_message(50, Charge::Discharging)).is_empty());
    assert!(h.on_msg(&battery_message(50, Charge::Discharging)).is_empty());
    assert_eq!(h.on_msg(&battery_message(50, Charge::Charging)), [Event::BatteryCharge(Battery { percent: 50, charge: Charge::Charging })]);
    assert!(h.on_msg(&profile_message(Profile::Balanced)).is_empty());
    assert!(h.on_msg(&profile_message(Profile::Balanced)).is_empty());
    assert_eq!(h.on_msg(&profile_message(Profile::Performance)), [Event::PowerProfile(Profile::Performance)]);
    assert_eq!(h.on_mode(Mode::Dark, true), None);
    assert_eq!(h.on_mode(Mode::Dark, true), None);
    assert_eq!(h.on_mode(Mode::Light, true), Some(Event::ThemeMode(Mode::Light)));
}

#[test]
fn network_and_vpn_fire_on_a_change_of_kind_or_vpn_state() {
    let t0 = Instant::now();
    let mut h = hooks_with(&["network", "vpn"], t0);
    assert!(h.on_msg(&Msg::Network(Some(NetState::Offline))).is_empty());
    assert_eq!(h.on_msg(&online_message(Link::Wifi, 0, false)), [Event::Network(Some(Link::Wifi))]);
    assert!(h.on_msg(&online_message(Link::Wifi, 0, true)).is_empty(), "limited alone is no edge");
    assert_eq!(h.on_msg(&online_message(Link::Wifi, 1, true)), [Event::Vpn(true)]);
    assert_eq!(h.on_msg(&online_message(Link::Ethernet, 0, false)), [Event::Network(Some(Link::Ethernet)), Event::Vpn(false)]);

    let mut only_vpn = hooks_with(&["vpn"], t0);
    assert!(only_vpn.on_msg(&online_message(Link::Wifi, 0, false)).is_empty());
    assert!(only_vpn.on_msg(&online_message(Link::Ethernet, 0, false)).is_empty(), "the network is not tracked");
    assert_eq!(only_vpn.on_msg(&online_message(Link::Ethernet, 2, false)), [Event::Vpn(true)]);
}

#[test]
fn bluetooth_fires_once_per_device_and_for_the_adapter() {
    let mut h = hooks_with(&["bluetooth"], Instant::now());
    assert!(h.on_msg(&Msg::Bluetooth(Some(BtState::Off))).is_empty());
    assert_eq!(
        h.on_msg(&bluetooth_message(&["A", "B"])),
        [
            Event::Bluetooth(BtChange::On),
            Event::Bluetooth(BtChange::Connected("A".into())),
            Event::Bluetooth(BtChange::Connected("B".into()))
        ]
    );
    assert_eq!(h.on_msg(&bluetooth_message(&["A"])), [Event::Bluetooth(BtChange::Disconnected("B".into()))]);
    assert_eq!(h.on_msg(&bluetooth_message(&["A", "A"])), [Event::Bluetooth(BtChange::Connected("A".into()))], "alike names count twice");
    assert_eq!(h.on_msg(&bluetooth_message(&["A"])), [Event::Bluetooth(BtChange::Disconnected("A".into()))]);
    assert_eq!(
        h.on_msg(&Msg::Bluetooth(Some(BtState::Off))),
        [Event::Bluetooth(BtChange::Disconnected("A".into())), Event::Bluetooth(BtChange::Off)]
    );
}

#[test]
fn a_battery_change_fires_the_charge_before_the_level() {
    let mut h = hooks_with(&["battery_charge", "battery_level"], Instant::now());
    assert!(h.on_msg(&battery_message(50, Charge::Discharging)).is_empty());
    let b = |percent, charge| Battery { percent, charge };
    assert_eq!(h.on_msg(&battery_message(49, Charge::Discharging)), [Event::BatteryLevel(b(49, Charge::Discharging))]);
    assert_eq!(h.on_msg(&battery_message(49, Charge::Charging)), [Event::BatteryCharge(b(49, Charge::Charging))]);
    assert_eq!(
        h.on_msg(&battery_message(50, Charge::Full)),
        [Event::BatteryCharge(b(50, Charge::Full)), Event::BatteryLevel(b(50, Charge::Full))]
    );
    assert!(h.on_msg(&battery_message(50, Charge::Unknown)).is_empty(), "unknown keeps the seed");
    assert!(h.on_msg(&battery_message(50, Charge::Full)).is_empty(), "back to the seed: no edge");
}

#[test]
fn a_source_down_is_not_a_state() {
    let mut h = hooks_with(&["network", "bluetooth", "battery_charge", "power_profile"], Instant::now());
    h.on_msg(&online_message(Link::Wifi, 0, false));
    assert!(h.on_msg(&Msg::Network(None)).is_empty());
    assert!(h.on_msg(&Msg::Network(Some(NetState::Absent))).is_empty());
    assert!(h.on_msg(&online_message(Link::Wifi, 0, false)).is_empty());
    assert_eq!(h.on_msg(&Msg::Network(Some(NetState::Offline))), [Event::Network(None)]);

    h.on_msg(&bluetooth_message(&["A"]));
    assert!(h.on_msg(&Msg::Bluetooth(None)).is_empty());
    assert!(h.on_msg(&Msg::Bluetooth(Some(BtState::Absent))).is_empty());
    assert!(h.on_msg(&bluetooth_message(&["A"])).is_empty());

    h.on_msg(&battery_message(50, Charge::Discharging));
    assert!(h.on_msg(&Msg::Battery(None)).is_empty());
    assert!(h.on_msg(&Msg::Battery(Some(PowerState { laptop: None, peripherals: vec![] }))).is_empty());
    assert!(h.on_msg(&battery_message(50, Charge::Discharging)).is_empty());

    h.on_msg(&profile_message(Profile::Balanced));
    assert!(h.on_msg(&Msg::Profile(None)).is_empty());
    assert!(h.on_msg(&Msg::Profile(Some(ProfileState { active: None, offered: vec![] }))).is_empty());
    assert!(h.on_msg(&profile_message(Profile::Balanced)).is_empty());
}

#[test]
fn a_capture_indicator_fires_when_it_turns_on() {
    let mut h = hooks_with(&["privacy"], Instant::now());
    assert!(h.on_capture(&captures_on(&[])).is_empty());
    assert_eq!(h.on_capture(&captures_on(&[CaptureKind::Camera])), [Event::Privacy(CaptureKind::Camera, true)]);
    assert_eq!(
        h.on_capture(&captures_on(&[CaptureKind::Camera, CaptureKind::Microphone, CaptureKind::Screen])),
        [Event::Privacy(CaptureKind::Microphone, true), Event::Privacy(CaptureKind::Screen, true)]
    );
}

/// The sound source is up (it sends a state).
fn sound_up_message() -> Msg {
    Msg::Sound(Some(crate::system::SoundState::default()))
}

/// The compositor source is up (it sends a state).
fn compositor_up_message() -> Msg {
    Msg::Compositor(Some(crate::compositor::CompositorState::default()))
}

#[test]
fn the_sound_source_going_down_fires_nothing() {
    use CaptureKind::*;
    let mut h = hooks_with(&["privacy"], Instant::now());
    assert_eq!(h.on_capture(&captures_on(&[Microphone, Camera, Screen])).len(), 3);
    h.on_msg(&Msg::Sound(None));
    assert!(h.on_capture(&captures_on(&[])).is_empty(), "down is not a state: no off");
    assert!(h.on_msg(&sound_up_message()).is_empty());
    assert!(h.on_capture(&captures_on(&[Microphone, Camera, Screen])).is_empty(), "back with the same state: nothing");
    h.on_msg(&Msg::Sound(None));
    assert!(h.on_capture(&captures_on(&[])).is_empty());
    h.on_msg(&sound_up_message());
    assert_eq!(
        h.on_capture(&captures_on(&[Camera, Screen])),
        [Event::Privacy(Microphone, false)],
        "back with a different state: the difference fires"
    );
}

#[test]
fn the_compositor_going_down_keeps_only_the_screen() {
    use CaptureKind::*;
    let mut h = hooks_with(&["privacy"], Instant::now());
    h.on_capture(&captures_on(&[Microphone, Screen]));
    h.on_msg(&Msg::Compositor(None));
    assert!(h.on_capture(&captures_on(&[Microphone])).is_empty(), "the screen part is kept");
    assert_eq!(h.on_capture(&captures_on(&[])), [Event::Privacy(Microphone, false)], "the rest still compares");
    h.on_msg(&compositor_up_message());
    assert_eq!(h.on_capture(&captures_on(&[])), [Event::Privacy(Screen, false)], "compared again once back");
    h.on_msg(&Msg::Compositor(None));
    assert!(h.on_capture(&captures_on(&[Screen])).is_empty());
    h.on_msg(&compositor_up_message());
    assert_eq!(h.on_capture(&captures_on(&[Screen])), [Event::Privacy(Screen, true)]);
}

#[test]
fn a_bluetooth_address_is_never_a_device_name() {
    for addr in ["AA:BB:CC:DD:EE:FF", "aa-bb-cc-dd-ee-ff", "00:1A:7d:da:71:13"] {
        let env = Event::Bluetooth(BtChange::Connected(addr.into())).env();
        assert!(env.iter().all(|(k, _)| *k != "ALPHEUS_DEVICE"), "{addr}");
        let env = Event::Bluetooth(BtChange::Disconnected(addr.into())).env();
        assert!(env.iter().all(|(k, _)| *k != "ALPHEUS_DEVICE"), "{addr}");
    }
    for name in ["AA:BB:CC:DD:EE", "AA:BB:CC:DD:EE:GG", "Test Headset", "AA:BB:CC:DD:EE:FF:00", "AA:BB-CC:DD:EE:FF"] {
        let env = Event::Bluetooth(BtChange::Connected(name.into())).env();
        assert!(env.iter().any(|(k, v)| *k == "ALPHEUS_DEVICE" && v == name), "{name}");
    }
}

#[test]
fn theme_mode_seeds_on_the_portals_first_word() {
    let mut h = hooks_with(&["theme_mode"], Instant::now());
    assert_eq!(h.on_mode(Mode::Dark, false), None, "no portal answer yet: nothing seeded");
    assert_eq!(h.on_mode(Mode::Light, false), None, "still none, even if the fallback moves");
    assert_eq!(h.on_mode(Mode::Dark, true), None, "the first word seeds");
    assert_eq!(h.on_mode(Mode::Light, true), Some(Event::ThemeMode(Mode::Light)));
    let mut fixed = hooks_with(&["theme_mode"], Instant::now());
    assert_eq!(fixed.on_mode(Mode::Dark, true), None);
    assert_eq!(fixed.on_mode(Mode::Light, true), Some(Event::ThemeMode(Mode::Light)));
}

#[test]
fn a_hook_never_inherits_a_variable_its_event_does_not_set() {
    let t0 = Instant::now();
    let mut h = hooks_with(&["dnd", "bluetooth"], t0);
    let Verdict::Run(f) = h.fire(Event::Dnd(true), t0 + QUIET) else { panic!() };
    assert!(f.unset.contains(&"ALPHEUS_DEVICE") && f.unset.contains(&"ALPHEUS_MODE"));
    assert!(!f.unset.contains(&"ALPHEUS_DND") && !f.unset.contains(&"ALPHEUS_EVENT"));
    let Verdict::Run(f) = h.fire(Event::Bluetooth(BtChange::Connected("AA:BB:CC:DD:EE:FF".into())), t0 + QUIET) else { panic!() };
    assert!(f.unset.contains(&"ALPHEUS_DEVICE"), "set nowhere: removed, an address being no name");
    // Every variable an event can set is in VARS: set or removed, never inherited.
    let b = Battery { percent: 1, charge: Charge::Full };
    for e in [
        Event::Started,
        Event::ThemeMode(Mode::Dark),
        Event::Dnd(true),
        Event::BatteryCharge(b),
        Event::PowerProfile(Profile::Balanced),
        Event::Network(None),
        Event::Vpn(true),
        Event::Bluetooth(BtChange::Connected("X".into())),
        Event::Privacy(CaptureKind::Camera, true),
        Event::Displays("p".into()),
        Event::Shield(true),
        Event::FailedUnits { total: 1, system: Some(1), user: None },
    ] {
        let env = e.env();
        let mut all: Vec<&str> = env.iter().map(|(k, _)| *k).chain(unset_for(&env)).collect();
        all.sort();
        let mut want = VARS.to_vec();
        want.sort();
        assert_eq!(all, want, "{e:?}");
    }
}

/// One variable of an event's environment, as `Event::env` lists it.
fn variable(a: &'static str, b: &str) -> (&'static str, String) {
    (a, b.to_string())
}

#[test]
fn the_environment_of_each_event() {
    assert_eq!(Event::Started.env(), [variable("ALPHEUS_EVENT", "started")]);
    assert_eq!(Event::ThemeMode(Mode::Light).env(), [variable("ALPHEUS_EVENT", "theme_mode"), variable("ALPHEUS_MODE", "light")]);
    assert_eq!(Event::Dnd(true).env(), [variable("ALPHEUS_EVENT", "dnd"), variable("ALPHEUS_DND", "on")]);
    let b = Battery { percent: 42, charge: Charge::PendingCharge };
    assert_eq!(
        Event::BatteryCharge(b).env(),
        [variable("ALPHEUS_EVENT", "battery_charge"), variable("ALPHEUS_CHARGE", "pending"), variable("ALPHEUS_PERCENT", "42")]
    );
    let b = Battery { percent: 7, charge: Charge::Unknown };
    assert_eq!(
        Event::BatteryLevel(b).env(),
        [variable("ALPHEUS_EVENT", "battery_level"), variable("ALPHEUS_CHARGE", "unknown"), variable("ALPHEUS_PERCENT", "7")]
    );
    assert_eq!(
        Event::PowerProfile(Profile::PowerSaver).env(),
        [variable("ALPHEUS_EVENT", "power_profile"), variable("ALPHEUS_POWER_PROFILE", "power-saver")]
    );
    assert_eq!(
        Event::Network(None).env(),
        [variable("ALPHEUS_EVENT", "network"), variable("ALPHEUS_NETWORK", "offline"), variable("ALPHEUS_LINK", "none")]
    );
    assert_eq!(
        Event::Network(Some(Link::Wifi)).env(),
        [variable("ALPHEUS_EVENT", "network"), variable("ALPHEUS_NETWORK", "online"), variable("ALPHEUS_LINK", "wifi")]
    );
    assert_eq!(Event::Vpn(false).env(), [variable("ALPHEUS_EVENT", "vpn"), variable("ALPHEUS_VPN", "off")]);
    assert_eq!(Event::Bluetooth(BtChange::Off).env(), [variable("ALPHEUS_EVENT", "bluetooth"), variable("ALPHEUS_BLUETOOTH", "off")]);
    assert_eq!(
        Event::Bluetooth(BtChange::Connected("Test Headset".into())).env(),
        [variable("ALPHEUS_EVENT", "bluetooth"), variable("ALPHEUS_BLUETOOTH", "connected"), variable("ALPHEUS_DEVICE", "Test Headset")]
    );
    assert_eq!(
        Event::Privacy(CaptureKind::Screen, false).env(),
        [variable("ALPHEUS_EVENT", "privacy"), variable("ALPHEUS_CAPTURE", "screen"), variable("ALPHEUS_STATE", "off")]
    );
    assert_eq!(Event::Displays("desk".into()).env(), [variable("ALPHEUS_EVENT", "displays"), variable("ALPHEUS_DISPLAY_PROFILE", "desk")]);
}

#[test]
fn a_hostile_device_name_is_cleaned_and_cut() {
    let env = Event::Bluetooth(BtChange::Connected("\u{202e}Test\u{7}\u{0}Headset".into())).env();
    assert_eq!(env.last().unwrap(), &variable("ALPHEUS_DEVICE", "TestHeadset"));
    let long = "é".repeat(300);
    let v = clean_value(&long);
    assert!(v.len() <= VALUE_MAX && !v.is_empty() && v.chars().all(|c| c == 'é'), "{}", v.len());
    let env = Event::Displays(format!("a\0{long}")).env();
    assert!(env.iter().all(|(_, v)| !v.contains('\0') && v.len() <= VALUE_MAX));
}

#[test]
fn edges_inside_the_quiet_window_only_move_the_seed() {
    let t0 = Instant::now();
    let mut h = hooks_with(&["dnd"], t0);
    assert_eq!(h.fire(Event::Dnd(true), t0 + Duration::from_secs(1)), Verdict::Dropped);
    assert!(matches!(h.fire(Event::Dnd(true), t0 + QUIET), Verdict::Run(_)));
}

#[test]
fn the_quiet_window_is_marked_once_when_it_ends() {
    let t0 = Instant::now();
    let mut h = hooks_with(&["dnd"], t0);
    assert!(!h.quiet_over(t0 + Duration::from_secs(1)));
    assert!(h.quiet_over(t0 + QUIET));
    assert!(!h.quiet_over(t0 + QUIET + Duration::from_secs(5)), "once");
}

#[test]
fn started_ignores_the_quiet_window() {
    let t0 = Instant::now();
    let mut h = hooks_with(&["started"], t0);
    assert_eq!(
        h.fire(Event::Started, t0),
        Verdict::Run(Fire {
            id: HookId::Started,
            argv: vec!["hook-started".into()],
            env: vec![variable("ALPHEUS_EVENT", "started")],
            unset: unset_for(&[variable("ALPHEUS_EVENT", "started")])
        })
    );
}

#[test]
fn an_event_with_no_hook_set_is_dropped() {
    let t0 = Instant::now();
    let mut h = hooks_with(&["dnd"], t0);
    assert!(!h.wants(HookId::Vpn));
    assert_eq!(h.fire(Event::Vpn(true), t0 + QUIET), Verdict::Dropped);
    assert_eq!(h.set(), 1);
}

#[test]
fn the_bucket_gives_five_then_one_every_three_seconds() {
    let t0 = Instant::now();
    let late = t0 + QUIET;
    let mut h = hooks_with(&["dnd", "vpn"], t0);
    let dnd = || Event::Dnd(true);
    for _ in 0..BURST {
        assert!(matches!(h.fire(dnd(), late), Verdict::Run(_)));
    }
    assert_eq!(h.fire(dnd(), late), Verdict::Throttled(HookId::Dnd));
    assert_eq!(h.fire(dnd(), late), Verdict::Dropped);
    assert!(matches!(h.fire(Event::Vpn(true), late), Verdict::Run(_)), "buckets are per event");
    // One refill: a run ends the episode, so the empty bucket says it again, once.
    assert!(matches!(h.fire(dnd(), late + REFILL), Verdict::Run(_)));
    assert_eq!(h.fire(dnd(), late + REFILL), Verdict::Throttled(HookId::Dnd));
    assert_eq!(h.fire(dnd(), late + REFILL), Verdict::Dropped);
    // Never more than BURST saved up.
    let much_later = late + 20 * REFILL;
    for _ in 0..BURST {
        assert!(matches!(h.fire(dnd(), much_later), Verdict::Run(_)));
    }
    assert_eq!(h.fire(dnd(), much_later), Verdict::Throttled(HookId::Dnd));
}

#[test]
fn a_throttled_event_is_said_once_per_episode() {
    let t0 = Instant::now();
    let late = t0 + QUIET;
    let mut h = hooks_with(&["dnd"], t0);
    for _ in 0..BURST {
        h.fire(Event::Dnd(true), late);
    }
    assert_eq!(h.fire(Event::Dnd(true), late), Verdict::Throttled(HookId::Dnd));
    assert_eq!(h.fire(Event::Dnd(true), late), Verdict::Dropped);
    assert!(matches!(h.fire(Event::Dnd(true), late + REFILL), Verdict::Run(_)));
    assert_eq!(h.fire(Event::Dnd(true), late + REFILL), Verdict::Throttled(HookId::Dnd), "a new episode");
}

/// Diagnostics spec § 6: `failed_units` fires when the total of the two counts changes; the first
/// total is a seed, a source that is down (None) is not a state.
#[test]
fn failed_units_fires_when_the_total_changes() {
    let ev = |total, system, user| Event::FailedUnits { total, system, user };
    let mut h = hooks_with(&["failed_units"], Instant::now());
    assert!(h.on_msg(&Msg::Units(Some(0))).is_empty(), "the first total seeds");
    assert!(h.on_msg(&Msg::Units(Some(0))).is_empty());
    assert_eq!(h.on_msg(&Msg::Units(Some(2))), [ev(2, Some(2), None)]);
    assert!(h.on_msg(&Msg::UserUnits(Some(1))).is_empty(), "a manager's first answer is a seed, not an edge, whenever it comes");
    assert!(h.on_msg(&Msg::UserUnits(Some(1))).is_empty(), "the same total");
    assert_eq!(h.on_msg(&Msg::UserUnits(Some(2))), [ev(4, Some(2), Some(2))]);
    assert_eq!(h.on_msg(&Msg::UserUnits(Some(1))), [ev(3, Some(2), Some(1))]);
    assert!(h.on_msg(&Msg::Units(None)).is_empty(), "the system bus going down is no state");
    assert!(h.on_msg(&Msg::UserUnits(None)).is_empty(), "nor the user manager's");
    assert_eq!(h.on_msg(&Msg::UserUnits(Some(0))), [ev(2, Some(2), Some(0))], "the last known system count stays");
    assert_eq!(h.on_msg(&Msg::Units(Some(0))), [ev(0, Some(0), Some(0))]);
    let mut none = hooks_with(&["dnd"], Instant::now());
    assert!(none.on_msg(&Msg::Units(Some(1))).is_empty() && none.on_msg(&Msg::Units(Some(2))).is_empty(), "not wanted: nothing");
}

#[test]
fn failed_units_gives_each_managers_count_once_known() {
    assert_eq!(
        Event::FailedUnits { total: 3, system: Some(2), user: Some(1) }.env(),
        [
            variable("ALPHEUS_EVENT", "failed_units"),
            variable("ALPHEUS_FAILED", "3"),
            variable("ALPHEUS_FAILED_SYSTEM", "2"),
            variable("ALPHEUS_FAILED_USER", "1")
        ]
    );
    let env = Event::FailedUnits { total: 2, system: Some(2), user: None }.env();
    assert_eq!(env, [variable("ALPHEUS_EVENT", "failed_units"), variable("ALPHEUS_FAILED", "2"), variable("ALPHEUS_FAILED_SYSTEM", "2")]);
    assert!(unset_for(&env).contains(&"ALPHEUS_FAILED_USER"), "unknown: removed, never inherited");
}

#[test]
fn an_event_is_read_only_when_its_modules_are_in_modules() {
    use crate::config::ModuleId as M;
    assert!(HookId::Started.reads(&[], false) && HookId::ThemeMode.reads(&[], false));
    assert!(!HookId::Dnd.reads(&[M::Clock], false) && HookId::Dnd.reads(&[M::Notifications], false));
    assert!(!HookId::Privacy.reads(&[M::Clock], false) && HookId::Privacy.reads(&[M::Privacy], false));
    assert!(!HookId::Displays.reads(&[M::Clock], false) && HookId::Displays.reads(&[M::Displays], false));
    assert!(!HookId::BatteryLevel.reads(&[M::Clock], false) && HookId::BatteryLevel.reads(&[M::Battery], false));
    assert!(HookId::Vpn.reads(&[M::Network], false) && HookId::Vpn.reads(&[M::Quick], false));
    assert!(HookId::PowerProfile.reads(&[M::Quick], false) && !HookId::PowerProfile.reads(&[M::Network], false));
    assert!(HookId::Bluetooth.reads(&[M::Bluetooth], false) && HookId::Bluetooth.reads(&[M::Quick], false));
    assert_eq!(HookId::Network.needs(), "network or quick");
    assert!(!HookId::FailedUnits.reads(&[M::Clock], false) && HookId::FailedUnits.reads(&[M::Failed], false));
    assert_eq!(HookId::FailedUnits.needs(), "failed");
}

#[test]
fn the_shield_event_sets_its_variable_and_fires_like_the_others() {
    assert_eq!(Event::Shield(true).env(), [variable("ALPHEUS_EVENT", "shield"), variable("ALPHEUS_SHIELD", "on")]);
    assert_eq!(Event::Shield(false).env(), [variable("ALPHEUS_EVENT", "shield"), variable("ALPHEUS_SHIELD", "off")]);
    assert_eq!(Event::Shield(true).id(), HookId::Shield);
    assert_eq!(HookId::Shield.key(), "shield");
    // Fired like the others: past the quiet window, under the token bucket.
    let t0 = Instant::now();
    let mut h = hooks_with(&["shield"], t0);
    assert_eq!(h.fire(Event::Shield(true), t0), Verdict::Dropped, "inside the quiet window");
    let Verdict::Run(f) = h.fire(Event::Shield(true), t0 + QUIET) else { panic!("runs") };
    assert!(f.unset.contains(&"ALPHEUS_DND") && !f.unset.contains(&"ALPHEUS_SHIELD"));
    for _ in 1..BURST {
        assert!(matches!(h.fire(Event::Shield(false), t0 + QUIET), Verdict::Run(_)));
    }
    assert_eq!(h.fire(Event::Shield(false), t0 + QUIET), Verdict::Throttled(HookId::Shield));
}

#[test]
fn the_shield_hook_needs_shield_configured() {
    use crate::config::ModuleId as M;
    // No module feeds it: only a configured `[shield]` does, whatever `modules` hold.
    assert!(!HookId::Shield.reads(&[M::Quick, M::Shield, M::Notifications], false));
    assert!(HookId::Shield.reads(&[], true));
    assert_eq!(HookId::Shield.needs(), "[shield]");
}

#[test]
fn a_source_whose_key_is_not_set_fires_nothing() {
    let t0 = Instant::now();
    // Only `started` is set: every other family is unset.
    let mut h = hooks_with(&["started"], t0);
    for _ in 0..2 {
        assert!(h.on_msg(&bluetooth_message(&["A"])).is_empty());
        assert!(h.on_msg(&bluetooth_message(&[])).is_empty());
        assert!(h.on_msg(&battery_message(50, Charge::Discharging)).is_empty());
        assert!(h.on_msg(&battery_message(40, Charge::Charging)).is_empty());
        assert!(h.on_msg(&profile_message(Profile::Balanced)).is_empty());
        assert!(h.on_msg(&profile_message(Profile::Performance)).is_empty());
        assert!(h.on_mode(Mode::Dark, true).is_none());
        assert!(h.on_mode(Mode::Light, true).is_none());
        assert!(h.on_capture(&captures_on(&[CaptureKind::Camera])).is_empty());
        assert!(h.on_capture(&captures_on(&[])).is_empty());
    }
}

#[test]
fn an_events_index_is_its_position_in_all() {
    for (i, id) in HookId::ALL.iter().enumerate() {
        assert_eq!(id.index(), i);
    }
}
