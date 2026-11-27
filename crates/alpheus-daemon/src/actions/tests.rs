use super::*;
use crate::compositor::example;
use crate::config::{BarConfig, ModuleId};
use crate::state::Msg;
use crate::system::*;
use alpheus_common::exec::ExecError;
use alpheus_common::exec::fake::FakeExecutor;

#[test]
fn only_the_timer_chips_middle_button_is_the_timers() {
    let timer = Target::Module(ModuleId::Timer);
    assert!(timer_middle(&timer, Button::Middle));
    for b in [Button::Left, Button::Right, Button::WheelUp, Button::WheelDown] {
        assert!(!timer_middle(&timer, b), "{b:?}");
    }
    assert!(!timer_middle(&Target::Module(ModuleId::Media), Button::Middle));
}

#[test]
fn the_media_module_answers_the_middle_button_and_the_wheel() {
    use crate::media::Control;
    let media = Target::Module(ModuleId::Media);
    assert_eq!(media_control(&media, Button::Middle), Some(Control::PlayPause));
    assert_eq!(media_control(&media, Button::WheelUp), Some(Control::Next), "up is next");
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
    let g = b.grid("HDMI-A-1", 100); // launcher button in cell 1, "4" in cells 11 to 19 (x = 100 is cell 12)
    // 8 px advance at scale 1, 16 px at scale 2: same cell.
    assert_eq!(click(&b, "HDMI-A-1", &g, 100.0, 1, 8.0, Button::Left), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
    assert_eq!(click(&b, "HDMI-A-1", &g, 100.0, 2, 16.0, Button::Left), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
    assert_eq!(click(&b, "HDMI-A-1", &g, 4.0, 1, 8.0, Button::Left), None, "margin");
    assert_eq!(click(&b, "HDMI-A-1", &g, 12.0, 1, 8.0, Button::Left), None, "launcher button: no command");
    assert_eq!(click(&b, "HDMI-A-1", &g, 400.0, 1, 8.0, Button::Left), None, "title");
    assert_eq!(click(&b, "HDMI-A-1", &g, 100.0, 1, 8.0, Button::WheelDown), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
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
    b.apply(Msg::Resources(Sample { cpu: Some(12), memory: Some(61), temperature: Some(60), ..Sample::default() }));
    b
}

/// A bar without the quick panel: `quick` out of `modules`.
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

/// The middle click toggles the default source's mute, the quick panel's
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
fn indicators_open_their_tab() {
    let b = without_quick(full());
    for (m, tab) in [(ModuleId::Network, "network"), (ModuleId::Bluetooth, "bluetooth"), (ModuleId::Battery, "home")] {
        let t = Target::Module(m);
        assert_eq!(command(&b, "eDP-1", &t, Button::Left), argv(&["alpheus-toggle", tab]), "{m:?}");
        for other in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
            assert_eq!(command(&b, "eDP-1", &t, other), None, "{m:?} {other:?}");
        }
    }
}

/// The left click opens the popup (no command), the right click runs
/// `[actions] resources`; the other buttons do nothing.
#[test]
fn resources_left_opens_the_popup_right_runs_the_monitor() {
    let mut b = without_quick(full());
    b.config.actions.resources = vec!["foot".into(), "htop".into()];
    let r = Target::Module(ModuleId::Resources);
    assert_eq!(command(&b, "eDP-1", &r, Button::Left), None);
    assert_eq!(command(&b, "eDP-1", &r, Button::Right), argv(&["foot", "htop"]));
    for other in [Button::Middle, Button::WheelUp, Button::WheelDown] {
        assert_eq!(command(&b, "eDP-1", &r, other), None, "{other:?}");
    }
    assert_eq!(ModulePopup::of(&r, Button::Left, false), Some(ModulePopup::Resources));
    assert_eq!(ModulePopup::of(&r, Button::Right, false), None);
}

/// A left click on the failed count opens the Diag tab through `open_tab`;
/// no other click does anything.
#[test]
fn a_left_click_opens_the_diag_tab() {
    let b = full();
    let t = Target::Module(ModuleId::Failed);
    assert_eq!(command(&b, "eDP-1", &t, Button::Left), argv(&["alpheus-toggle", "diagnostics"]));
    for other in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
        assert_eq!(command(&b, "eDP-1", &t, other), None, "{other:?}");
    }
    assert_eq!(ModulePopup::of(&t, Button::Left, true), None, "no popup");
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
    assert_eq!(command(&b, "eDP-1", &Target::Module(ModuleId::Resources), Button::Right), argv(&["foot", "htop"]));
}

/// With the panel, the left click is its popup's (no command), the right click
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

/// A failed start logs a fixed line: never the command, whose program or arguments may name the
/// user, nor the error's own text.
#[test]
fn a_failed_start_logs_a_kind_word_and_no_command() {
    let argv = vec!["/home/someone/bin/open".to_string(), "--file".into(), "/home/someone/notes".into()];
    let fake = FakeExecutor::new();
    let mut lines = Vec::new();
    run_logged(&fake, &argv, &mut |l| lines.push(l.to_string()));
    assert_eq!(lines, ["action: could not start (not found)"]);
    let fake = FakeExecutor::new();
    fake.respond("/home/someone/bin/open --file /home/someone/notes", Err(ExecError::Io("/home/someone/notes: busy".into())));
    let mut lines = Vec::new();
    run_logged(&fake, &argv, &mut |l| lines.push(l.to_string()));
    assert_eq!(lines, ["action: could not start (io)"]);
    let ok = FakeExecutor::new();
    ok.ok("/home/someone/bin/open --file /home/someone/notes", "");
    let mut lines = Vec::new();
    run_logged(&ok, &argv, &mut |l| lines.push(l.to_string()));
    assert!(lines.is_empty(), "a start that works logs nothing: {lines:?}");
    run_logged(&fake, &[], &mut |l| lines.push(l.to_string()));
    assert!(lines.is_empty(), "an empty command is nothing to run");
}

const BUTTONS: [Button; 5] = [Button::Left, Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown];

/// `full()` with `bindings` (bar.toml text) read the way the file is.
fn bound_bar(bindings: &str) -> Bar {
    let mut b = full();
    b.config = BarConfig::from_text(bindings).unwrap();
    b
}

fn run_argv(v: &[&str]) -> Option<Bound> {
    Some(Bound::Run(v.iter().map(|s| s.to_string()).collect()))
}

#[test]
fn a_command_binding_runs_as_written() {
    let b = bound_bar("[bindings.volume]\nright = [\"my-mixer\", \"{output}\", \"{tab}\"]");
    assert_eq!(bound(&b, "DP-1", &Target::Module(ModuleId::Volume), Button::Right), run_argv(&["my-mixer", "DP-1", "sound"]));
    let b = bound_bar("[bindings.window]\nmiddle = [\"x; $(y)\", \"{output}\"]");
    assert_eq!(
        bound(&b, "DP-1", &Target::Passive(ModuleId::Window), Button::Middle),
        run_argv(&["x; $(y)", "DP-1"]),
        "a passive zone runs its binding; the argument stays one, never a shell"
    );
}

#[test]
fn each_verb_resolves() {
    let b = bound_bar(
        "[bindings.volume]\nleft = \"none\"\nright = \"mute\"\nmiddle = \"open_tab\"\nwheel_up = \"quick\"\nwheel_down = \"launcher\"\n\
         [bindings.clock]\nleft = \"popup\"\nmiddle = \"dnd\"",
    );
    let v = Target::Module(ModuleId::Volume);
    assert_eq!(bound(&b, "DP-1", &v, Button::Left), Some(Bound::Nothing));
    assert_eq!(bound(&b, "DP-1", &v, Button::Right), run_argv(&["wpctl", "set-mute", SINK, "toggle"]));
    assert_eq!(bound(&b, "DP-1", &v, Button::Middle), run_argv(&["alpheus-toggle", "sound"]));
    assert_eq!(bound(&b, "DP-1", &v, Button::WheelUp), Some(Bound::Popup(ModulePopup::Quick)));
    assert_eq!(bound(&b, "DP-1", &v, Button::WheelDown), Some(Bound::Launcher));
    let c = Target::Module(ModuleId::Clock);
    assert_eq!(bound(&b, "DP-1", &c, Button::Left), Some(Bound::Popup(ModulePopup::Calendar)));
    assert_eq!(bound(&b, "DP-1", &c, Button::Middle), Some(Bound::Dnd));
    assert_eq!(bound(&b, "DP-1", &Target::Passive(ModuleId::Clock), Button::Left), Some(Bound::Popup(ModulePopup::Calendar)));
}

#[test]
fn an_unbound_button_keeps_today() {
    let b = bound_bar("[bindings.volume]\nright = [\"my-mixer\"]");
    let v = Target::Module(ModuleId::Volume);
    for bt in [Button::Left, Button::Middle, Button::WheelUp, Button::WheelDown] {
        assert_eq!(bound(&b, "DP-1", &v, bt), None, "{bt:?}");
    }
    assert_eq!(bound(&b, "DP-1", &Target::Module(ModuleId::Clock), Button::Right), None, "another module");
    assert_eq!(bound(&b, "DP-1", &workspace("1", "DP-1"), Button::Right), None, "a workspace takes no binding");
}

/// `full()`'s feeds and a bell, under `config`.
fn drawn(config: BarConfig) -> Bar {
    let mut b = full();
    b.config = config;
    b.apply(Msg::Notifications(crate::notify::NotifyView { unread: 2, dnd: false, serving: true }));
    b
}

/// A bar where every module draws, with and without the quick panel, and the same with empty
/// `[bindings.<module>]` tables: the same zones (an empty table adds no `Passive` one), and every
/// zone and button gives the same command and popup, and no binding.
#[test]
fn empty_bindings_change_no_click_byte_for_byte() {
    for quick in [true, false] {
        let (mut a, mut b) =
            (drawn(BarConfig::default()), drawn(BarConfig::from_text("[bindings.volume]\n[bindings.clock]\n[bindings.window]").unwrap()));
        assert_eq!(b.config.bindings.len(), 3, "three empty tables, the window's among them (it has no zone of its own)");
        if !quick {
            (a, b) = (without_quick(a), without_quick(b));
        }
        for output in ["HDMI-A-1", "DVI-I-1"] {
            let (ga, gb) = (a.grid(output, 240), b.grid(output, 240));
            assert_eq!(ga.zones(), gb.zones(), "{output}: the same zones");
            assert!(ga.zones().iter().any(|z| z.target == Target::Module(ModuleId::Clock)), "the clock is drawn");
            for z in ga.zones() {
                for bt in BUTTONS {
                    assert_eq!(command(&a, output, &z.target, bt), command(&b, output, &z.target, bt), "{z:?} {bt:?}");
                    assert_eq!(
                        ModulePopup::of(&z.target, bt, quick),
                        ModulePopup::of(&z.target, bt, b.config.modules.contains(&ModuleId::Quick)),
                        "{z:?} {bt:?}"
                    );
                    assert_eq!(bound(&b, output, &z.target, bt), None, "{z:?} {bt:?}");
                    assert_eq!(bound(&a, output, &z.target, bt), None, "{z:?} {bt:?}");
                }
            }
        }
    }
}

/// The clock and the window are the modules that get a `Passive` zone; every module is checked, so
/// a `Passive` zone read as its module's own (the volume's mute, the clock's calendar) shows.
#[test]
fn a_passive_zone_takes_no_click_of_its_own() {
    for b in [full(), without_quick(full())] {
        for m in ModuleId::ALL {
            let t = Target::Passive(m);
            for bt in BUTTONS {
                assert_eq!(command(&b, "DP-1", &t, bt), None, "{m:?} {bt:?}");
                assert_eq!(ModulePopup::of(&t, bt, true), None, "{m:?} {bt:?}");
                assert_eq!(ModulePopup::of(&t, bt, false), None, "{m:?} {bt:?}");
                assert!(!toggles_launcher(&t, bt) && !toggles_dnd(&t, bt) && !timer_middle(&t, bt), "{m:?} {bt:?}");
                assert_eq!(media_control(&t, bt), None, "{m:?} {bt:?}");
                assert_eq!(bound(&b, "DP-1", &t, bt), None, "no binding: nothing");
            }
        }
    }
}
