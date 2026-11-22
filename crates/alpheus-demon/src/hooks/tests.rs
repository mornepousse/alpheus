use super::*;
use crate::config::HooksConfig;
use crate::privacy::Indicator;
use crate::system::BtDevice;

fn cfg(keys: &[&str]) -> HooksConfig {
    let text = format!("[hooks]\n{}", keys.iter().map(|k| format!("{k} = [\"hook-{k}\"]\n")).collect::<String>());
    crate::config::BarConfig::from_text(&text).unwrap().hooks
}

fn hooks(keys: &[&str], t0: Instant) -> Hooks {
    Hooks::new(&cfg(keys), t0).unwrap()
}

fn online(link: Link, vpns: usize, limited: bool) -> Msg {
    Msg::Network(Some(NetState::Online { link, vpns, limited }))
}

fn bt(names: &[&str]) -> Msg {
    Msg::Bluetooth(Some(BtState::On { devices: names.iter().map(|n| BtDevice { name: n.to_string(), battery: None }).collect() }))
}

fn power(percent: u8, charge: Charge) -> Msg {
    Msg::Battery(Some(PowerState { laptop: Some(Battery { percent, charge }), peripherals: vec![] }))
}

fn profile(p: Profile) -> Msg {
    Msg::Profile(Some(ProfileState { active: Some(p), offered: vec![] }))
}

fn on(kinds: &[CaptureKind]) -> Capture {
    let ind = |k| Indicator { names: vec![], unnamed: kinds.contains(&k) };
    Capture { mic: ind(CaptureKind::Microphone), camera: ind(CaptureKind::Camera), screen: ind(CaptureKind::Screen) }
}

#[test]
fn an_empty_table_is_none() {
    assert!(Hooks::new(&HooksConfig::default(), Instant::now()).is_none());
}

#[test]
fn ids_match_the_config_keys() {
    let keys: Vec<&str> = HookId::ALL.iter().map(|i| i.key()).collect();
    let want: Vec<&str> = HooksConfig::default().commands().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, want);
}

#[test]
fn the_first_value_is_a_seed() {
    let t0 = Instant::now();
    let mut h = hooks(&["network", "bluetooth", "battery_charge", "battery_level", "power_profile", "theme_mode"], t0);
    assert!(h.on_msg(&online(Link::Wifi, 0, false)).is_empty());
    assert!(h.on_msg(&online(Link::Wifi, 0, false)).is_empty());
    assert_eq!(h.on_msg(&online(Link::Ethernet, 0, false)), [Event::Network(Some(Link::Ethernet))]);
    assert!(h.on_msg(&bt(&["A"])).is_empty());
    assert!(h.on_msg(&bt(&["A"])).is_empty());
    assert_eq!(h.on_msg(&bt(&[])), [Event::Bluetooth(BtChange::Disconnected("A".into()))]);
    assert!(h.on_msg(&power(50, Charge::Discharging)).is_empty());
    assert!(h.on_msg(&power(50, Charge::Discharging)).is_empty());
    assert_eq!(h.on_msg(&power(50, Charge::Charging)), [Event::BatteryCharge(Battery { percent: 50, charge: Charge::Charging })]);
    assert!(h.on_msg(&profile(Profile::Balanced)).is_empty());
    assert!(h.on_msg(&profile(Profile::Balanced)).is_empty());
    assert_eq!(h.on_msg(&profile(Profile::Performance)), [Event::PowerProfile(Profile::Performance)]);
    assert_eq!(h.on_mode(Mode::Dark, true), None);
    assert_eq!(h.on_mode(Mode::Dark, true), None);
    assert_eq!(h.on_mode(Mode::Light, true), Some(Event::ThemeMode(Mode::Light)));
}

#[test]
fn network_and_vpn_edges() {
    let t0 = Instant::now();
    let mut h = hooks(&["network", "vpn"], t0);
    assert!(h.on_msg(&Msg::Network(Some(NetState::Offline))).is_empty());
    assert_eq!(h.on_msg(&online(Link::Wifi, 0, false)), [Event::Network(Some(Link::Wifi))]);
    assert!(h.on_msg(&online(Link::Wifi, 0, true)).is_empty(), "limited alone is no edge");
    assert_eq!(h.on_msg(&online(Link::Wifi, 1, true)), [Event::Vpn(true)]);
    assert_eq!(h.on_msg(&online(Link::Ethernet, 0, false)), [Event::Network(Some(Link::Ethernet)), Event::Vpn(false)]);

    let mut only_vpn = hooks(&["vpn"], t0);
    assert!(only_vpn.on_msg(&online(Link::Wifi, 0, false)).is_empty());
    assert!(only_vpn.on_msg(&online(Link::Ethernet, 0, false)).is_empty(), "the network is not tracked");
    assert_eq!(only_vpn.on_msg(&online(Link::Ethernet, 2, false)), [Event::Vpn(true)]);
}

#[test]
fn bluetooth_edges_per_device() {
    let mut h = hooks(&["bluetooth"], Instant::now());
    assert!(h.on_msg(&Msg::Bluetooth(Some(BtState::Off))).is_empty());
    assert_eq!(
        h.on_msg(&bt(&["A", "B"])),
        [
            Event::Bluetooth(BtChange::On),
            Event::Bluetooth(BtChange::Connected("A".into())),
            Event::Bluetooth(BtChange::Connected("B".into()))
        ]
    );
    assert_eq!(h.on_msg(&bt(&["A"])), [Event::Bluetooth(BtChange::Disconnected("B".into()))]);
    assert_eq!(h.on_msg(&bt(&["A", "A"])), [Event::Bluetooth(BtChange::Connected("A".into()))], "alike names count twice");
    assert_eq!(h.on_msg(&bt(&["A"])), [Event::Bluetooth(BtChange::Disconnected("A".into()))]);
    assert_eq!(
        h.on_msg(&Msg::Bluetooth(Some(BtState::Off))),
        [Event::Bluetooth(BtChange::Disconnected("A".into())), Event::Bluetooth(BtChange::Off)]
    );
}

#[test]
fn battery_charge_then_level() {
    let mut h = hooks(&["battery_charge", "battery_level"], Instant::now());
    assert!(h.on_msg(&power(50, Charge::Discharging)).is_empty());
    let b = |percent, charge| Battery { percent, charge };
    assert_eq!(h.on_msg(&power(49, Charge::Discharging)), [Event::BatteryLevel(b(49, Charge::Discharging))]);
    assert_eq!(h.on_msg(&power(49, Charge::Charging)), [Event::BatteryCharge(b(49, Charge::Charging))]);
    assert_eq!(h.on_msg(&power(50, Charge::Full)), [Event::BatteryCharge(b(50, Charge::Full)), Event::BatteryLevel(b(50, Charge::Full))]);
    assert!(h.on_msg(&power(50, Charge::Unknown)).is_empty(), "unknown keeps the seed");
    assert!(h.on_msg(&power(50, Charge::Full)).is_empty(), "back to the seed: no edge");
}

#[test]
fn a_source_down_is_not_a_state() {
    let mut h = hooks(&["network", "bluetooth", "battery_charge", "power_profile"], Instant::now());
    h.on_msg(&online(Link::Wifi, 0, false));
    assert!(h.on_msg(&Msg::Network(None)).is_empty());
    assert!(h.on_msg(&Msg::Network(Some(NetState::Absent))).is_empty());
    assert!(h.on_msg(&online(Link::Wifi, 0, false)).is_empty());
    assert_eq!(h.on_msg(&Msg::Network(Some(NetState::Offline))), [Event::Network(None)]);

    h.on_msg(&bt(&["A"]));
    assert!(h.on_msg(&Msg::Bluetooth(None)).is_empty());
    assert!(h.on_msg(&Msg::Bluetooth(Some(BtState::Absent))).is_empty());
    assert!(h.on_msg(&bt(&["A"])).is_empty());

    h.on_msg(&power(50, Charge::Discharging));
    assert!(h.on_msg(&Msg::Battery(None)).is_empty());
    assert!(h.on_msg(&Msg::Battery(Some(PowerState { laptop: None, peripherals: vec![] }))).is_empty());
    assert!(h.on_msg(&power(50, Charge::Discharging)).is_empty());

    h.on_msg(&profile(Profile::Balanced));
    assert!(h.on_msg(&Msg::Profile(None)).is_empty());
    assert!(h.on_msg(&Msg::Profile(Some(ProfileState { active: None, offered: vec![] }))).is_empty());
    assert!(h.on_msg(&profile(Profile::Balanced)).is_empty());
}

#[test]
fn capture_edges() {
    let mut h = hooks(&["privacy"], Instant::now());
    assert!(h.on_capture(&on(&[])).is_empty());
    assert_eq!(h.on_capture(&on(&[CaptureKind::Camera])), [Event::Privacy(CaptureKind::Camera, true)]);
    assert_eq!(
        h.on_capture(&on(&[CaptureKind::Camera, CaptureKind::Microphone, CaptureKind::Screen])),
        [Event::Privacy(CaptureKind::Microphone, true), Event::Privacy(CaptureKind::Screen, true)]
    );
}

fn sound_up() -> Msg {
    Msg::Sound(Some(crate::system::SoundState::default()))
}

fn compositor_up() -> Msg {
    Msg::Compositor(Some(crate::compositor::CompositorState::default()))
}

#[test]
fn the_sound_source_going_down_fires_nothing() {
    use CaptureKind::*;
    let mut h = hooks(&["privacy"], Instant::now());
    assert_eq!(h.on_capture(&on(&[Microphone, Camera, Screen])).len(), 3);
    h.on_msg(&Msg::Sound(None));
    assert!(h.on_capture(&on(&[])).is_empty(), "down is not a state: no off");
    assert!(h.on_msg(&sound_up()).is_empty());
    assert!(h.on_capture(&on(&[Microphone, Camera, Screen])).is_empty(), "back with the same state: nothing");
    h.on_msg(&Msg::Sound(None));
    assert!(h.on_capture(&on(&[])).is_empty());
    h.on_msg(&sound_up());
    assert_eq!(
        h.on_capture(&on(&[Camera, Screen])),
        [Event::Privacy(Microphone, false)],
        "back with a different state: the difference fires"
    );
}

#[test]
fn the_compositor_going_down_keeps_only_the_screen() {
    use CaptureKind::*;
    let mut h = hooks(&["privacy"], Instant::now());
    h.on_capture(&on(&[Microphone, Screen]));
    h.on_msg(&Msg::Compositor(None));
    assert!(h.on_capture(&on(&[Microphone])).is_empty(), "the screen part is kept");
    assert_eq!(h.on_capture(&on(&[])), [Event::Privacy(Microphone, false)], "the rest still compares");
    h.on_msg(&compositor_up());
    assert_eq!(h.on_capture(&on(&[])), [Event::Privacy(Screen, false)], "compared again once back");
    h.on_msg(&Msg::Compositor(None));
    assert!(h.on_capture(&on(&[Screen])).is_empty());
    h.on_msg(&compositor_up());
    assert_eq!(h.on_capture(&on(&[Screen])), [Event::Privacy(Screen, true)]);
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
    let mut h = hooks(&["theme_mode"], Instant::now());
    assert_eq!(h.on_mode(Mode::Dark, false), None, "no portal answer yet: nothing seeded");
    assert_eq!(h.on_mode(Mode::Light, false), None, "still none, even if the fallback moves");
    assert_eq!(h.on_mode(Mode::Dark, true), None, "the first word seeds");
    assert_eq!(h.on_mode(Mode::Light, true), Some(Event::ThemeMode(Mode::Light)));
    let mut fixed = hooks(&["theme_mode"], Instant::now());
    assert_eq!(fixed.on_mode(Mode::Dark, true), None);
    assert_eq!(fixed.on_mode(Mode::Light, true), Some(Event::ThemeMode(Mode::Light)));
}

#[test]
fn a_hook_never_inherits_a_variable_its_event_does_not_set() {
    let t0 = Instant::now();
    let mut h = hooks(&["dnd", "bluetooth"], t0);
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
    ] {
        let env = e.env();
        let mut all: Vec<&str> = env.iter().map(|(k, _)| *k).chain(unset_for(&env)).collect();
        all.sort();
        let mut want = VARS.to_vec();
        want.sort();
        assert_eq!(all, want, "{e:?}");
    }
}

fn s(a: &'static str, b: &str) -> (&'static str, String) {
    (a, b.to_string())
}

#[test]
fn the_environment_of_each_event() {
    assert_eq!(Event::Started.env(), [s("ALPHEUS_EVENT", "started")]);
    assert_eq!(Event::ThemeMode(Mode::Light).env(), [s("ALPHEUS_EVENT", "theme_mode"), s("ALPHEUS_MODE", "light")]);
    assert_eq!(Event::Dnd(true).env(), [s("ALPHEUS_EVENT", "dnd"), s("ALPHEUS_DND", "on")]);
    let b = Battery { percent: 42, charge: Charge::PendingCharge };
    assert_eq!(
        Event::BatteryCharge(b).env(),
        [s("ALPHEUS_EVENT", "battery_charge"), s("ALPHEUS_CHARGE", "pending"), s("ALPHEUS_PERCENT", "42")]
    );
    let b = Battery { percent: 7, charge: Charge::Unknown };
    assert_eq!(
        Event::BatteryLevel(b).env(),
        [s("ALPHEUS_EVENT", "battery_level"), s("ALPHEUS_CHARGE", "unknown"), s("ALPHEUS_PERCENT", "7")]
    );
    assert_eq!(
        Event::PowerProfile(Profile::PowerSaver).env(),
        [s("ALPHEUS_EVENT", "power_profile"), s("ALPHEUS_POWER_PROFILE", "power-saver")]
    );
    assert_eq!(Event::Network(None).env(), [s("ALPHEUS_EVENT", "network"), s("ALPHEUS_NETWORK", "offline"), s("ALPHEUS_LINK", "none")]);
    assert_eq!(
        Event::Network(Some(Link::Wifi)).env(),
        [s("ALPHEUS_EVENT", "network"), s("ALPHEUS_NETWORK", "online"), s("ALPHEUS_LINK", "wifi")]
    );
    assert_eq!(Event::Vpn(false).env(), [s("ALPHEUS_EVENT", "vpn"), s("ALPHEUS_VPN", "off")]);
    assert_eq!(Event::Bluetooth(BtChange::Off).env(), [s("ALPHEUS_EVENT", "bluetooth"), s("ALPHEUS_BLUETOOTH", "off")]);
    assert_eq!(
        Event::Bluetooth(BtChange::Connected("Test Headset".into())).env(),
        [s("ALPHEUS_EVENT", "bluetooth"), s("ALPHEUS_BLUETOOTH", "connected"), s("ALPHEUS_DEVICE", "Test Headset")]
    );
    assert_eq!(
        Event::Privacy(CaptureKind::Screen, false).env(),
        [s("ALPHEUS_EVENT", "privacy"), s("ALPHEUS_CAPTURE", "screen"), s("ALPHEUS_STATE", "off")]
    );
    assert_eq!(Event::Displays("desk".into()).env(), [s("ALPHEUS_EVENT", "displays"), s("ALPHEUS_DISPLAY_PROFILE", "desk")]);
}

#[test]
fn a_hostile_device_name_is_cleaned_and_cut() {
    let env = Event::Bluetooth(BtChange::Connected("\u{202e}Test\u{7}\u{0}Headset".into())).env();
    assert_eq!(env.last().unwrap(), &s("ALPHEUS_DEVICE", "TestHeadset"));
    let long = "é".repeat(300);
    let v = clean_value(&long);
    assert!(v.len() <= VALUE_MAX && !v.is_empty() && v.chars().all(|c| c == 'é'), "{}", v.len());
    let env = Event::Displays(format!("a\0{long}")).env();
    assert!(env.iter().all(|(_, v)| !v.contains('\0') && v.len() <= VALUE_MAX));
}

#[test]
fn edges_inside_the_quiet_window_only_move_the_seed() {
    let t0 = Instant::now();
    let mut h = hooks(&["dnd"], t0);
    assert_eq!(h.fire(Event::Dnd(true), t0 + Duration::from_secs(1)), Verdict::Dropped);
    assert!(matches!(h.fire(Event::Dnd(true), t0 + QUIET), Verdict::Run(_)));
}

#[test]
fn the_quiet_window_is_marked_once_when_it_ends() {
    let t0 = Instant::now();
    let mut h = hooks(&["dnd"], t0);
    assert!(!h.quiet_over(t0 + Duration::from_secs(1)));
    assert!(h.quiet_over(t0 + QUIET));
    assert!(!h.quiet_over(t0 + QUIET + Duration::from_secs(5)), "once");
}

#[test]
fn started_ignores_the_quiet_window() {
    let t0 = Instant::now();
    let mut h = hooks(&["started"], t0);
    assert_eq!(
        h.fire(Event::Started, t0),
        Verdict::Run(Fire {
            id: HookId::Started,
            argv: vec!["hook-started".into()],
            env: vec![s("ALPHEUS_EVENT", "started")],
            unset: unset_for(&[s("ALPHEUS_EVENT", "started")])
        })
    );
}

#[test]
fn an_unset_hook_is_dropped() {
    let t0 = Instant::now();
    let mut h = hooks(&["dnd"], t0);
    assert!(!h.wants(HookId::Vpn));
    assert_eq!(h.fire(Event::Vpn(true), t0 + QUIET), Verdict::Dropped);
    assert_eq!(h.set(), 1);
}

#[test]
fn the_bucket_gives_five_then_one_every_three_seconds() {
    let t0 = Instant::now();
    let late = t0 + QUIET;
    let mut h = hooks(&["dnd", "vpn"], t0);
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
fn throttled_is_said_once_per_episode() {
    let t0 = Instant::now();
    let late = t0 + QUIET;
    let mut h = hooks(&["dnd"], t0);
    for _ in 0..BURST {
        h.fire(Event::Dnd(true), late);
    }
    assert_eq!(h.fire(Event::Dnd(true), late), Verdict::Throttled(HookId::Dnd));
    assert_eq!(h.fire(Event::Dnd(true), late), Verdict::Dropped);
    assert!(matches!(h.fire(Event::Dnd(true), late + REFILL), Verdict::Run(_)));
    assert_eq!(h.fire(Event::Dnd(true), late + REFILL), Verdict::Throttled(HookId::Dnd), "a new episode");
}

#[test]
fn reads_follows_the_modules() {
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
}

#[test]
fn the_shield_event_sets_its_variable() {
    assert_eq!(Event::Shield(true).env(), [s("ALPHEUS_EVENT", "shield"), s("ALPHEUS_SHIELD", "on")]);
    assert_eq!(Event::Shield(false).env(), [s("ALPHEUS_EVENT", "shield"), s("ALPHEUS_SHIELD", "off")]);
    assert_eq!(Event::Shield(true).id(), HookId::Shield);
    assert_eq!(HookId::Shield.key(), "shield");
    // Fired like the others: past the quiet window, under the token bucket.
    let t0 = Instant::now();
    let mut h = hooks(&["shield"], t0);
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
fn a_family_without_its_key_gives_nothing() {
    let t0 = Instant::now();
    // Only `started` is set: every other family is unset.
    let mut h = hooks(&["started"], t0);
    for _ in 0..2 {
        assert!(h.on_msg(&bt(&["A"])).is_empty());
        assert!(h.on_msg(&bt(&[])).is_empty());
        assert!(h.on_msg(&power(50, Charge::Discharging)).is_empty());
        assert!(h.on_msg(&power(40, Charge::Charging)).is_empty());
        assert!(h.on_msg(&profile(Profile::Balanced)).is_empty());
        assert!(h.on_msg(&profile(Profile::Performance)).is_empty());
        assert!(h.on_mode(Mode::Dark, true).is_none());
        assert!(h.on_mode(Mode::Light, true).is_none());
        assert!(h.on_capture(&on(&[CaptureKind::Camera])).is_empty());
        assert!(h.on_capture(&on(&[])).is_empty());
    }
}

#[test]
fn the_index_is_the_position_in_all() {
    for (i, id) in HookId::ALL.iter().enumerate() {
        assert_eq!(id.index(), i);
    }
}
