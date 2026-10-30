use super::testing::*;
use super::*;
use crate::backends::BackendError;

fn later(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}

#[test]
fn startup_requests_home() {
    let mut a = app();
    assert_eq!(a.tab, Tab::Home);
    assert_eq!(a.take_requests(), vec![Request::Refresh(Tab::Home.domains().to_vec())]);
    assert_eq!(a.data.volume, Loadable::Loading);
}

#[test]
fn updates() {
    let mut a = app();
    a.on_update(Update::Volume(Ok(Volume { percent: 40, muted: true })), Instant::now());
    a.on_update(Update::Brightness(Err(BackendError::Missing { tool: "brightnessctl".into() })), Instant::now());
    assert_eq!(a.data.volume, Loadable::Ready(Volume { percent: 40, muted: true }));
    assert_eq!(a.data.brightness, Loadable::Failed("brightnessctl not found".into()));
}

#[test]
fn display_read_ignored_during_a_setting() {
    let mut a = ready_app();
    a.on_update(Update::Displays(Ok(Vec::new())), Instant::now());
    assert_eq!(a.data.displays, Loadable::Ready(Vec::new()));
    a.send_op(Op::ApplyDisplays { plan: crate::displays::Plan::default(), undo: None });
    a.on_update(Update::Displays(Ok(crate::displays::testing::machine())), Instant::now());
    assert_eq!(a.data.displays, Loadable::Ready(Vec::new()), "stale read ignored");
    a.on_update(Update::Displays(Err(BackendError::Missing { tool: "wm-set".into() })), Instant::now());
    assert_eq!(a.data.displays, Loadable::Ready(Vec::new()));
}

#[test]
fn optimistic_slider_and_request() {
    let mut a = ready_app();
    a.nudge_slider(SliderId::Volume, STEP);
    assert_eq!(a.slider_value(SliderId::Volume), Some(67));
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 67))]);
    a.nudge_slider(SliderId::Node(64), -STEP);
    assert_eq!(a.slider_value(SliderId::Node(64)), Some(70));
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::Id(64), 70))]);
}

#[test]
fn sliders_are_bounded() {
    let mut a = ready_app();
    a.set_slider(SliderId::Volume, 150);
    a.set_slider(SliderId::Brightness, -20);
    assert_eq!(
        a.take_requests(),
        vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100)),
            Request::Do(Op::SetBrightness(crate::backends::brightness::MIN_PERCENT)),
        ]
    );
}

#[test]
fn volume_above_100_is_never_lowered_by_plus() {
    let mut a = ready_app();
    a.data.volume = Loadable::Ready(Volume { percent: 120, muted: false });
    a.nudge_slider(SliderId::Volume, STEP);
    assert!(a.take_requests().is_empty(), "\"+\" does not bring 120 % down to 100 %");
    assert_eq!(a.slider_value(SliderId::Volume), Some(120));
    a.nudge_slider(SliderId::Volume, -STEP);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 100))], "\"-\" clamps to 100");
    assert_eq!(a.slider_value(SliderId::Volume), Some(100));
    // Same rule for a device in the Sound tab.
    if let Loadable::Ready(st) = &mut a.data.audio {
        st.sinks[0].volume.percent = 130;
    }
    a.nudge_slider(SliderId::Node(64), STEP);
    assert!(a.take_requests().is_empty());
    a.nudge_slider(SliderId::Node(64), -STEP);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::Id(64), 100))]);
}

#[test]
fn unknown_or_unchanged_slider_sends_nothing() {
    let mut a = app();
    a.take_requests();
    a.nudge_slider(SliderId::Volume, STEP);
    assert!(a.take_requests().is_empty());
    let mut a = ready_app();
    a.set_slider(SliderId::Volume, 62);
    assert!(a.take_requests().is_empty());
}

#[test]
fn optimistic_toggle() {
    let mut a = ready_app();
    a.toggle(ToggleId::Wifi, Instant::now());
    assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false));
    a.toggle(ToggleId::KeepAwake, Instant::now());
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetWifi(false)), Request::Do(Op::ToggleKeepAwake { now_on: true })]);
}

fn net(wifi: bool) -> Update {
    Update::Network(Ok(NetStatus { wifi_enabled: wifi, active: Vec::new() }))
}

#[test]
fn stale_read_ignored_during_a_toggle() {
    let mut a = ready_app();
    a.toggle(ToggleId::Wifi, Instant::now());
    a.take_requests();
    // Read launched before the toggle: it carries the old state.
    a.on_update(net(true), Instant::now());
    assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false), "the display stays optimistic");
    a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
    // After the response, the next read is trusted.
    a.on_update(net(true), Instant::now());
    assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
}

#[test]
fn other_domains_keep_being_read_during_a_toggle() {
    let mut a = ready_app();
    a.toggle(ToggleId::Wifi, Instant::now());
    a.on_update(Update::Dnd(Ok(true)), Instant::now());
    assert_eq!(a.data.dnd, Loadable::Ready(true));
}

#[test]
fn two_toggles_wait_for_both_responses() {
    let mut a = ready_app();
    a.toggle(ToggleId::Wifi, Instant::now()); // → off
    a.toggle(ToggleId::Wifi, Instant::now()); // → on
    a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
    a.on_update(net(false), Instant::now()); // read between the two actions
    assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
    a.on_update(Update::Done(Op::SetWifi(true), Ok(())), Instant::now());
    a.on_update(net(true), Instant::now());
    assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(true));
    a.on_update(net(false), Instant::now());
    assert_eq!(a.toggle_loadable(ToggleId::Wifi), Loadable::Ready(false), "nothing pending any more");
}

#[test]
fn coalesced_slider_burst_does_not_lock_the_domain() {
    let mut a = ready_app();
    a.nudge_slider(SliderId::Brightness, STEP); // 25
    a.nudge_slider(SliderId::Brightness, STEP); // 30
    a.nudge_slider(SliderId::Brightness, STEP); // 35
    a.on_update(Update::Brightness(Ok(20)), Instant::now());
    assert_eq!(a.data.brightness, Loadable::Ready(35), "stale read ignored");
    // The thread coalesced the burst: two settings dropped, one done.
    a.on_update(Update::Superseded(Op::SetBrightness(25)), Instant::now());
    a.on_update(Update::Superseded(Op::SetBrightness(30)), Instant::now());
    a.on_update(Update::Done(Op::SetBrightness(35), Ok(())), Instant::now());
    a.on_update(Update::Brightness(Ok(40)), Instant::now());
    assert_eq!(a.data.brightness, Loadable::Ready(40), "the domain did not stay locked");
}

#[test]
fn default_volume_also_locks_the_audio_list() {
    let mut a = ready_app();
    a.nudge_slider(SliderId::Volume, STEP);
    a.on_update(Update::Audio(Ok(AudioStatus { sinks: Vec::new(), sources: Vec::new() })), Instant::now());
    assert_eq!(a.data.audio.ready().unwrap().sinks.len(), 2);
    a.on_update(Update::Done(Op::SetVolume(NodeRef::DefaultSink, 67), Ok(())), Instant::now());
    a.on_update(Update::Audio(Ok(AudioStatus { sinks: Vec::new(), sources: Vec::new() })), Instant::now());
    assert!(a.data.audio.ready().unwrap().sinks.is_empty());
}

#[test]
fn keep_awake_message_survives_a_stale_value() {
    let mut a = ready_app(); // off
    a.toggle(ToggleId::KeepAwake, Instant::now());
    assert_eq!(a.take_requests(), vec![Request::Do(Op::ToggleKeepAwake { now_on: true })]);
    a.on_update(Update::KeepAwake(Ok(false)), Instant::now()); // read before the toggle
    a.on_update(Update::Done(Op::ToggleKeepAwake { now_on: true }, Ok(())), Instant::now());
    assert_eq!(a.toasts[0].text, "Keep awake: on");
    // Even if the displayed state was stale, the message follows the intended state.
    a.data.keep_awake = Loadable::Ready(false);
    a.on_update(Update::Done(Op::ToggleKeepAwake { now_on: true }, Ok(())), Instant::now());
    assert_eq!(a.toasts[1].text, "Keep awake: on");
}

#[test]
fn toggling_from_an_unknown_state() {
    let mut a = app();
    a.take_requests();
    a.toggle(ToggleId::Dnd, Instant::now());
    assert!(a.take_requests().is_empty());
    assert_eq!(a.toasts[0].text, "Do not disturb: unknown state");
    assert!(a.toasts[0].error);
}

#[test]
fn changing_tab() {
    let mut a = ready_app();
    a.focus = 3;
    a.search.text = "x".into();
    a.go_tab(Tab::Sound, Instant::now());
    assert_eq!((a.tab, a.focus), (Tab::Sound, 0));
    assert_eq!(a.search, Search::default());
    assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Audio])]);
    a.go_tab(Tab::Power, Instant::now());
    assert!(a.take_requests().is_empty()); // nothing to read
}

#[test]
fn items_per_tab() {
    let mut a = ready_app();
    assert_eq!(a.items().len(), 7);
    a.go_tab(Tab::Sound, Instant::now());
    assert_eq!(
        a.items(),
        vec![
            Item::Slider(SliderId::Node(64)),
            Item::Slider(SliderId::Node(116)),
            Item::Slider(SliderId::Node(113)),
            Item::Button(Command::Open(External::Mixer)),
        ]
    );
    a.go_tab(Tab::Power, Instant::now());
    assert_eq!(a.items().len(), 5);
    a.go_tab(Tab::Network, Instant::now());
    assert_eq!(a.items(), vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))]);
}

#[test]
fn search_filter() {
    let mut a = ready_app();
    a.go_tab(Tab::Sound, Instant::now());
    a.search.text = "casq".into();
    let (sinks, sources) = a.visible_nodes();
    assert_eq!(sinks.iter().map(|n| n.id).collect::<Vec<_>>(), vec![116]);
    assert!(sources.is_empty());
    assert_eq!(a.items().len(), 2);
    a.focus = 3;
    a.clamp_focus();
    assert_eq!(a.focus, 1);
}

#[test]
fn activating_an_output_makes_it_the_default() {
    let mut a = ready_app();
    a.go_tab(Tab::Sound, Instant::now());
    a.take_requests();
    a.activate(Instant::now());
    let st = a.data.audio.ready().unwrap();
    assert!(st.sinks[0].default && !st.sinks[1].default);
    assert!(st.sources[0].default, "inputs do not move");
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDefault(64))]);
}

#[test]
fn profile_is_bounded() {
    let mut a = ready_app();
    a.focus = 6;
    a.adjust(1, Instant::now());
    assert_eq!(a.data.profiles.ready().unwrap().active, "power-saver");
    a.adjust(1, Instant::now());
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetProfile("power-saver".into()))]);
}

#[test]
fn power_with_or_without_confirmation() {
    let mut a = ready_app();
    a.run_command(Command::Power(PowerAction::Poweroff), Instant::now());
    assert_eq!(a.overlay, Overlay::Confirm(Confirm { action: PowerAction::Poweroff, yes_focused: false }));
    assert!(a.take_requests().is_empty());
    a.close_confirm(false);
    assert_eq!(a.overlay, Overlay::None);
    assert!(a.take_requests().is_empty());
    a.run_command(Command::Power(PowerAction::Poweroff), Instant::now());
    a.close_confirm(true);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Poweroff))]);
    a.run_command(Command::Power(PowerAction::Lock), Instant::now());
    assert_eq!(a.take_requests(), vec![Request::Do(Op::Power(PowerAction::Lock))]);
}

#[test]
fn external_tool() {
    let mut a = ready_app();
    a.run_command(Command::Open(External::Mixer), Instant::now());
    assert_eq!(a.take_external(), Some(External::Mixer));
    assert_eq!(a.take_external(), None);
    assert_eq!(a.external_argv(External::Mixer), vec!["wiremix"]);
    a.external_done(External::Mixer, Err("wiremix not found".into()), Instant::now());
    assert_eq!(a.status.error.as_deref(), Some("Per-app mixer: wiremix not found"));
    assert!(a.toasts[0].error);
}

#[test]
fn successful_action() {
    let mut a = ready_app();
    a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
    assert_eq!(a.toasts[0].text, "Wi-Fi: off");
    assert_eq!(a.status.last.as_deref(), Some("Wi-Fi: off"));
    assert_eq!(a.take_requests(), vec![Request::Refresh(Tab::Home.domains().to_vec())]);
    a.on_update(Update::Done(Op::SetDefault(116), Ok(())), Instant::now());
    assert_eq!(a.toasts[1].text, "Output: CasqueBT");
    a.on_update(Update::Done(Op::SetDefault(113), Ok(())), Instant::now());
    assert_eq!(a.toasts[2].text, "Input: Stereo Microphone");
}

#[test]
fn successful_slider_has_no_message_or_reread() {
    let mut a = ready_app();
    a.on_update(Update::Done(Op::SetVolume(NodeRef::DefaultSink, 50), Ok(())), Instant::now());
    assert!(a.toasts.is_empty());
    assert!(a.take_requests().is_empty());
}

#[test]
fn failed_action() {
    let mut a = ready_app();
    let e = BackendError::Failed { tool: "shell-ctl".into(), code: Some(1), stderr: "boom".into() };
    a.on_update(Update::Done(Op::SetDnd(true), Err(e)), Instant::now());
    assert_eq!(a.status.error.as_deref(), Some("Do not disturb: shell-ctl failed (code 1): boom"));
    assert!(a.toasts[0].error);
    assert_eq!(a.take_requests().len(), 1, "re-read to cancel the optimistic display");
}

#[test]
fn tick_refreshes_home_every_second() {
    let mut a = ready_app();
    a.on_tick(later(1100));
    assert_eq!(a.take_requests(), vec![Request::Refresh(Tab::Home.domains().to_vec())]);
    a.on_tick(later(1500));
    assert!(a.take_requests().is_empty());
}

#[test]
fn tick_does_not_reread_a_failed_domain() {
    let mut a = ready_app();
    a.data.dnd = Loadable::Failed("shell-ctl failed (code 1)".into());
    a.on_tick(later(1100));
    let r = a.take_requests();
    assert_eq!(r.len(), 1);
    let Request::Refresh(ds) = &r[0] else { panic!("{r:?}") };
    assert!(!ds.contains(&Domain::Dnd));
    assert_eq!(ds.len(), 6);
}

#[test]
fn tick_refreshes_sound_network_bluetooth_every_two_seconds() {
    for (tab, d) in [
        (Tab::Sound, Domain::Audio),
        (Tab::Network, Domain::Network),
        (Tab::Bluetooth, Domain::Bluetooth),
        (Tab::Displays, Domain::Displays),
    ] {
        let mut a = ready_app();
        let t0 = Instant::now();
        a.go_tab(tab, t0);
        a.take_requests();
        a.on_tick(t0 + Duration::from_millis(1100));
        assert!(a.take_requests().is_empty(), "{tab:?}: not read at 1 s");
        a.on_tick(t0 + Duration::from_millis(2100));
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![d])], "{tab:?}: read at 2 s");
        a.on_tick(t0 + Duration::from_millis(3000));
        assert!(a.take_requests().is_empty(), "{tab:?}: the period restarted from the read");
    }
}

fn refreshed(r: &[Request]) -> Vec<Domain> {
    r.iter()
        .flat_map(|q| match q {
            Request::Refresh(ds) => ds.clone(),
            Request::Do(_) => Vec::new(),
        })
        .collect()
}

#[test]
fn failed_domain_reread_every_fifteen_seconds() {
    let mut a = ready_app();
    let t0 = Instant::now();
    a.go_tab(Tab::Home, t0); // full read at t0
    a.take_requests();
    a.data.dnd = Loadable::Failed("shell-ctl failed (code 1)".into());
    let at = |ms| t0 + Duration::from_millis(ms);
    a.on_tick(at(1100));
    assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "not read at 1 s");
    a.on_tick(at(14_900));
    assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "not read before 15 s");
    a.on_tick(at(15_100));
    let r = refreshed(&a.take_requests());
    assert_eq!(r, vec![Domain::Dnd], "read at 15 s, without waiting for the normal tick");
    a.on_tick(at(16_200));
    assert!(!refreshed(&a.take_requests()).contains(&Domain::Dnd), "the 15 s period restarts");
    a.on_tick(at(30_200));
    assert!(refreshed(&a.take_requests()).contains(&Domain::Dnd));
}

#[test]
fn failed_tab_reread_only_at_fifteen_seconds() {
    let mut a = ready_app();
    let t0 = Instant::now();
    a.go_tab(Tab::Sound, t0);
    a.take_requests();
    a.data.audio = Loadable::Failed("wpctl not found".into());
    a.on_tick(t0 + Duration::from_millis(2100));
    assert!(a.take_requests().is_empty(), "not read at the 2 s tick");
    a.on_tick(t0 + Duration::from_millis(15_100));
    assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Audio])]);
}

#[test]
fn tick_reads_nothing_on_power() {
    let mut a = ready_app();
    let t0 = Instant::now();
    a.go_tab(Tab::Power, t0);
    a.take_requests();
    a.on_tick(t0 + Duration::from_secs(60));
    assert!(a.take_requests().is_empty());
}

#[test]
fn messages_expire_and_are_capped_at_three() {
    let mut a = ready_app();
    for _ in 0..5 {
        a.on_update(Update::Done(Op::SetWifi(true), Ok(())), Instant::now());
    }
    assert_eq!(a.toasts.len(), 3);
    a.on_tick(Instant::now() + TOAST_TTL + Duration::from_millis(100));
    assert!(a.toasts.is_empty());
}

#[test]
fn palette_without_accents() {
    let mut a = ready_app();
    a.overlay = Overlay::Palette(Palette { query: "poweroff".into(), selected: 0 });
    assert_eq!(a.palette_matches()[0].0, "Power off");
    a.palette_run(None, Instant::now());
    assert!(matches!(a.overlay, Overlay::Confirm(_)));
}

#[test]
fn startup_warning() {
    let mut a = app();
    a.warn("invalid config.toml: x".into());
    assert_eq!(a.status.warning.as_deref(), Some("invalid config.toml: x"));
    assert_eq!(a.status.error, None);
    a.warn("invalid colors.toml: y".into());
    assert_eq!(a.status.warning.as_deref(), Some("invalid config.toml: x ; invalid colors.toml: y"), "neither is lost");
}

#[test]
fn the_warning_survives_actions() {
    let mut a = ready_app();
    a.warn("invalid config.toml: x".into());
    let e = BackendError::Failed { tool: "shell-ctl".into(), code: Some(1), stderr: "boom".into() };
    a.on_update(Update::Done(Op::SetDnd(true), Err(e)), Instant::now());
    assert!(a.status.error.is_some());
    a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
    assert_eq!(a.status.error, None, "the last action error is cleared on the next success");
    assert_eq!(a.status.warning.as_deref(), Some("invalid config.toml: x"), "the warning remains");
}
