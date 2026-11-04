use super::*;
use crate::compositor::example;
use crate::config::{BarConfig, ModuleId};
use crate::state::Msg;
use crate::system::*;
use alpheus_commun::exec::fake::FakeExecutor;

#[test]
fn the_media_module_answers_the_middle_button_and_the_wheel() {
    use crate::media::Control;
    let media = Target::Module(ModuleId::Media);
    assert_eq!(media_control(&media, Button::Middle), Some(Control::PlayPause));
    assert_eq!(media_control(&media, Button::WheelUp), Some(Control::Next), "spec § 1: up is next");
    assert_eq!(media_control(&media, Button::WheelDown), Some(Control::Previous));
    assert_eq!(media_control(&media, Button::Left), None, "the popup's, not a control");
    assert_eq!(media_control(&media, Button::Right), None);
    assert_eq!(media_control(&Target::Module(ModuleId::Volume), Button::Middle), None, "another module: its own action");
}

#[test]
fn only_a_middle_click_on_the_bell_toggles_do_not_disturb() {
    let bell = Target::Module(ModuleId::Notifications);
    assert!(toggles_dnd(&bell, Button::Middle));
    for b in [Button::Left, Button::Right, Button::WheelUp, Button::WheelDown] {
        assert!(!toggles_dnd(&bell, b), "{b:?}");
    }
    assert!(!toggles_dnd(&Target::Module(ModuleId::Media), Button::Middle), "the media's middle button is its own");
    let bar = bar();
    for b in [Button::Left, Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
        assert_eq!(command(&bar, "DP-1", &bell, b), None, "{b:?}: the daemon acts itself, never a process");
    }
}

#[test]
fn no_media_click_is_a_command() {
    let bar = bar();
    for b in [Button::Left, Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
        assert_eq!(command(&bar, "DP-1", &Target::Module(ModuleId::Media), b), None, "{b:?}: a D-Bus call, never a process");
    }
}

#[test]
fn wheel_scrolls_vertical_and_horizontal() {
    let scroll = |delta, orientation| Gesture::Scroll { delta, orientation };
    assert_eq!(tray_gesture(Button::Left), Gesture::Left);
    assert_eq!(tray_gesture(Button::Right), Gesture::Right);
    assert_eq!(tray_gesture(Button::Middle), Gesture::Middle);
    assert_eq!(tray_gesture(Button::WheelUp), scroll(120, Orientation::Vertical), "wheel up: positive");
    assert_eq!(tray_gesture(Button::WheelDown), scroll(-120, Orientation::Vertical));
    assert_eq!(tray_horizontal(Button::WheelDown), Some(scroll(-120, Orientation::Horizontal)), "right: negative, Plasma's convention");
    assert_eq!(tray_horizontal(Button::WheelUp), Some(scroll(120, Orientation::Horizontal)), "left: positive");
    assert_eq!(tray_horizontal(Button::Left), None);
    // Through the wheel accumulator, as wayland.rs feeds it: Wayland's
    // positive value120 is down (vertical) or right (horizontal).
    let mut w = Wheel::default();
    assert_eq!(w.add(15.0, 0, 120).map(tray_gesture), Some(scroll(-120, Orientation::Vertical)));
    assert_eq!(w.add(-15.0, 0, -120).and_then(tray_horizontal), Some(scroll(120, Orientation::Horizontal)));
}

#[test]
fn click_chain_coordinate_cell_zone_command() {
    let b = bar();
    let g = b.grid("HDMI-A-1", 100); // launcher button in cell 1, "4" in cells 9 to 17
    // 8 px advance at scale 1, 16 px at scale 2: same cell.
    assert_eq!(click(&b, "HDMI-A-1", &g, 84.0, 1, 8.0, Button::Left), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
    assert_eq!(click(&b, "HDMI-A-1", &g, 84.0, 2, 16.0, Button::Left), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
    assert_eq!(click(&b, "HDMI-A-1", &g, 4.0, 1, 8.0, Button::Left), None, "margin");
    assert_eq!(click(&b, "HDMI-A-1", &g, 12.0, 1, 8.0, Button::Left), None, "launcher button: no command");
    assert_eq!(click(&b, "HDMI-A-1", &g, 400.0, 1, 8.0, Button::Left), None, "title");
    assert_eq!(click(&b, "HDMI-A-1", &g, 84.0, 1, 8.0, Button::WheelDown), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
}

fn bar() -> Bar {
    let mut b = Bar::new(BarConfig::default());
    b.apply(Msg::Compositor(Some(example())));
    b
}

fn workspace(reference: &str, output: &str) -> Target {
    Target::Workspace { reference: reference.into(), output: output.into() }
}

fn argv(v: &[&str]) -> Option<Vec<String>> {
    Some(v.iter().map(|s| s.to_string()).collect())
}

#[test]
fn left_click_goes_to_the_workspace() {
    let b = bar();
    assert_eq!(command(&b, "HDMI-A-1", &workspace("4", "HDMI-A-1"), Button::Left), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
    assert_eq!(
        command(&b, "DVI-I-1", &workspace("name:web", "DVI-I-1"), Button::Left),
        argv(&["wm-set", "workspace", "name:web", "DVI-I-1"])
    );
    assert_eq!(command(&b, "HDMI-A-1", &workspace("4", "HDMI-A-1"), Button::Right), None);
    assert_eq!(command(&b, "HDMI-A-1", &workspace("4", "HDMI-A-1"), Button::Middle), None);
}

#[test]
fn workspace_name_with_special_characters_stays_one_argument() {
    // {workspace} and {output} are substituted in each argv element,
    // never through `sh -c` nor by concatenating into a shell string: a
    // workspace name with spaces, ";" or "$(…)" stays a single literal
    // argument (neither cut nor interpreted).
    let b = bar();
    let c = workspace("my workspace; $(rm -rf ~)", "HDMI-A-1");
    assert_eq!(command(&b, "HDMI-A-1", &c, Button::Left), argv(&["wm-set", "workspace", "my workspace; $(rm -rf ~)", "HDMI-A-1"]));
}

#[test]
fn reference_literally_containing_a_placeholder_not_resubstituted() {
    // A workspace reference that literally contains "{output}"
    // (unlikely, but possible) must not be rewritten by the {output}
    // substitution: {workspace} and {output} are replaced in a SINGLE
    // pass over the TEMPLATE, never in two successive passes over the
    // result (the second would run back over what the first inserted).
    let b = bar();
    let c = workspace("name:{output}", "HDMI-A-1");
    assert_eq!(command(&b, "HDMI-A-1", &c, Button::Left), argv(&["wm-set", "workspace", "name:{output}", "HDMI-A-1"]));
}

#[test]
fn command_configurable_in_bar_toml() {
    let mut b = bar();
    b.config.workspaces.command = vec!["my-tool".into(), "--go".into(), "{workspace}".into()];
    assert_eq!(command(&b, "HDMI-A-1", &workspace("4", "HDMI-A-1"), Button::Left), argv(&["my-tool", "--go", "4"]));
}

#[test]
fn wheel_goes_to_the_neighboring_workspace_without_wrapping() {
    let b = bar();
    // HDMI-A-1: 2 (visible), 4, 5.
    let c = workspace("2", "HDMI-A-1");
    assert_eq!(command(&b, "HDMI-A-1", &c, Button::WheelDown), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
    assert_eq!(command(&b, "HDMI-A-1", &c, Button::WheelUp), None, "first workspace: we don't wrap");
    // DVI-I-1: 3 (visible), web.
    assert_eq!(
        command(&b, "DVI-I-1", &workspace("3", "DVI-I-1"), Button::WheelDown),
        argv(&["wm-set", "workspace", "name:web", "DVI-I-1"])
    );
    let waiting = Bar::new(BarConfig::default());
    assert_eq!(command(&waiting, "HDMI-A-1", &c, Button::WheelDown), None);
}

#[test]
fn run_through_the_injected_executor() {
    let fake = FakeExecutor::new();
    run(&fake, &["wm-set".into(), "workspace".into(), "4".into(), "HDMI-A-1".into()]);
    run(&fake, &[]);
    assert_eq!(fake.calls(), ["wm-set workspace 4 HDMI-A-1"]);
}

#[test]
fn buttons_and_wheel() {
    assert_eq!(button(0x110), Some(Button::Left));
    assert_eq!(button(0x113), None);
    let mut m = Wheel::default();
    // Wheel (recent compositors): value120, discrete at 0, absolute ignored.
    assert_eq!(m.add(15.0, 0, 120), Some(Button::WheelDown));
    assert_eq!(m.add(-15.0, 0, -120), Some(Button::WheelUp));
    assert_eq!(m.add(7.5, 0, 60), None, "fine wheel: half notch");
    assert_eq!(m.add(7.5, 0, 60), Some(Button::WheelDown), "two half-notches = one workspace");
    // Older compositors: discrete alone.
    assert_eq!(m.add(15.0, 1, 0), Some(Button::WheelDown));
    // Touchpad: neither notch nor value120, 30 px accumulated.
    assert_eq!(m.add(12.0, 0, 0), None);
    assert_eq!(m.add(12.0, 0, 0), None);
    assert_eq!(m.add(12.0, 0, 0), Some(Button::WheelDown));
    assert_eq!(m.add(-31.0, 0, 0), Some(Button::WheelUp));
}

#[test]
fn stop_resets_the_continuous_remainder_and_the_notch_accumulator() {
    let mut m = Wheel::default();
    assert_eq!(m.add(25.0, 0, 0), None, "stays under the 30 px threshold");
    m.stop();
    assert_eq!(m.add(5.0, 0, 0), None, "reset: 25 + 5 would have triggered it without the stop");
    // Same for the value120 accumulator (fine notch, two half-notches).
    assert_eq!(m.add(0.0, 0, 60), None, "half-notch");
    m.stop();
    assert_eq!(m.add(0.0, 0, 60), None, "reset: 60 + 60 would have triggered it without the stop");
}

fn full() -> Bar {
    let mut b = bar();
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
    b.apply(Msg::Network(Some(NetState::Offline)));
    b.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![] })));
    b.apply(Msg::Battery(Some(PowerState { laptop: Some(Battery { percent: 93, charge: Charge::Charging }), peripherals: vec![] })));
    b.apply(Msg::Resources(Sample { cpu: Some(12), memory: Some(61), temperature: Some(60) }));
    b
}

/// The bar as it was before the quick panel: `quick` out of `modules`.
fn without_quick(mut b: Bar) -> Bar {
    b.config.modules.retain(|m| *m != ModuleId::Quick);
    b
}

#[test]
fn volume_clicks_tab_mute_and_wheel() {
    let b = without_quick(full());
    let v = Target::Module(ModuleId::Volume);
    assert_eq!(command(&b, "HDMI-A-1", &v, Button::Left), argv(&["alpheus-toggle", "sound"]));
    assert_eq!(command(&b, "HDMI-A-1", &v, Button::Right), argv(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]));
    assert_eq!(command(&b, "HDMI-A-1", &v, Button::WheelUp), argv(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"]));
    assert_eq!(command(&b, "HDMI-A-1", &v, Button::WheelDown), argv(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%-"]));
    assert_eq!(command(&b, "HDMI-A-1", &v, Button::Middle), None);
}

/// Privacy spec § 6: the middle click toggles the default source's mute, the quick panel's
/// command; the left click is the popup's (no command), the other buttons nothing.
#[test]
fn a_middle_click_on_privacy_toggles_the_source_mute() {
    for b in [full(), without_quick(full())] {
        assert_eq!(module_command(&b, ModuleId::Privacy, Button::Middle), argv(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"]));
        for bt in [Button::Left, Button::Right, Button::WheelUp, Button::WheelDown] {
            assert_eq!(module_command(&b, ModuleId::Privacy, bt), None, "{bt:?}");
        }
    }
}

#[test]
fn indicators_open_their_tab_resources_open_the_monitor() {
    let b = without_quick(full());
    for (m, tab) in [(ModuleId::Network, "network"), (ModuleId::Bluetooth, "bluetooth"), (ModuleId::Battery, "home")] {
        let t = Target::Module(m);
        assert_eq!(command(&b, "eDP-1", &t, Button::Left), argv(&["alpheus-toggle", tab]), "{m:?}");
        for other in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
            assert_eq!(command(&b, "eDP-1", &t, other), None, "{m:?} {other:?}");
        }
    }
    let r = Target::Module(ModuleId::Resources);
    assert_eq!(command(&b, "eDP-1", &r, Button::Left), argv(&["foot", "-e", "btop"]));
    assert_eq!(command(&b, "eDP-1", &r, Button::Right), None);
}

#[test]
fn click_commands_configurable_in_bar_toml() {
    let mut b = without_quick(full());
    b.config.actions.open_tab = vec!["my-panel".into(), "--tab={tab}".into(), "{output}".into()];
    b.config.actions.resources = vec!["foot".into(), "htop".into()];
    assert_eq!(
        command(&b, "eDP-1", &Target::Module(ModuleId::Volume), Button::Left),
        argv(&["my-panel", "--tab=sound", "{output}"]),
        "only {{tab}} is a placeholder here"
    );
    assert_eq!(command(&b, "eDP-1", &Target::Module(ModuleId::Resources), Button::Left), argv(&["foot", "htop"]));
}

/// Quick spec § 1: with the panel, the left click is its popup's (no command), the right click
/// takes over the TUI tab on the three modules; the wheel and the battery are unchanged.
#[test]
fn with_the_quick_panel_the_right_click_opens_the_tab() {
    let b = full();
    for (m, tab) in [(ModuleId::Volume, "sound"), (ModuleId::Network, "network"), (ModuleId::Bluetooth, "bluetooth")] {
        let t = Target::Module(m);
        assert_eq!(command(&b, "eDP-1", &t, Button::Left), None, "{m:?}: the panel's");
        assert_eq!(command(&b, "eDP-1", &t, Button::Right), argv(&["alpheus-toggle", tab]), "{m:?}");
    }
    let v = Target::Module(ModuleId::Volume);
    assert_eq!(command(&b, "eDP-1", &v, Button::WheelUp), argv(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"]));
    assert_eq!(command(&b, "eDP-1", &Target::Module(ModuleId::Battery), Button::Left), argv(&["alpheus-toggle", "home"]));
}

#[test]
fn only_a_left_click_on_the_launcher_button_toggles_the_launcher() {
    let b = full();
    let l = Target::Module(ModuleId::Launcher);
    assert!(toggles_launcher(&l, Button::Left));
    for other in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
        assert!(!toggles_launcher(&l, other), "{other:?}");
        assert_eq!(command(&b, "eDP-1", &l, other), None, "{other:?}");
    }
    assert_eq!(command(&b, "eDP-1", &l, Button::Left), None, "the daemon opens it itself: no command");
    for t in [Target::Module(ModuleId::Volume), Target::Module(ModuleId::Clock), workspace("1", "eDP-1")] {
        assert!(!toggles_launcher(&t, Button::Left), "{t:?}");
    }
}

#[test]
fn right_click_on_the_drawn_volume_mutes() {
    let b = without_quick(full());
    let g = b.grid("HDMI-A-1", 160);
    let col = g.cells().iter().position(|c| c.ch == crate::segments::VOLUME_HIGH).unwrap();
    let x = col as f64 * 8.0 + 4.0;
    assert_eq!(click(&b, "HDMI-A-1", &g, x, 1, 8.0, Button::Right), argv(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]));
    assert_eq!(
        click(&b, "HDMI-A-1", &g, x, 1, 8.0, Button::WheelUp),
        argv(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"])
    );
}

#[test]
fn a_tray_icon_runs_no_command() {
    use crate::session_bus::ItemKey;
    let t = Target::TrayItem(ItemKey { owner: ":1.7".into(), path: "/StatusNotifierItem".into() });
    for b in [Button::Left, Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
        assert_eq!(command(&full(), "eDP-1", &t, b), None, "{b:?}");
    }
}
