//! Click and wheel → command. Going to a workspace goes through the
//! bar.toml command (`[workspaces] command`, default `wm-set workspace …`),
//! never by writing to the compositor directly. The wheel computes the
//! neighboring workspace itself and sends an absolute reference.

use crate::compositor::Workspace;
use crate::config::ModuleId;
use crate::grid::{Grid, Target};
use crate::painter::column_under;
use crate::state::{Bar, Feed};
use crate::tray::{Gesture, Orientation};
use alpheus_commun::exec::Executor;

use crate::log::write as log;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    WheelUp,
    WheelDown,
}

/// Wayland button (evdev BTN_* codes) → bar button.
pub fn button(code: u32) -> Option<Button> {
    match code {
        0x110 => Some(Button::Left),
        0x111 => Some(Button::Right),
        0x112 => Some(Button::Middle),
        _ => None,
    }
}

/// Next (or previous) workspace from this bar's visible workspace, without wrapping.
fn neighbor<'a>(bar: &'a Bar, output: &str, next: bool) -> Option<&'a Workspace> {
    let Feed::Ready(e) = &bar.compositor else { return None };
    let l = e.workspaces_of(output);
    let i = l.iter().position(|w| w.visible)?;
    let j = if next { i + 1 } else { i.checked_sub(1)? };
    l.get(j).copied()
}

/// Replaces each placeholder of `values` (e.g. `{workspace}`, `{tab}`) in a
/// single pass over `template` (the bar.toml text, never the
/// already-substituted result): a value that would literally contain a
/// placeholder (unlikely, but possible) is never run back through the mill.
pub(crate) fn substitute(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    'next: while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let from_the_brace = &rest[i..];
        for (placeholder, value) in values {
            if let Some(after) = from_the_brace.strip_prefix(placeholder) {
                out.push_str(value);
                rest = after;
                continue 'next;
            }
        }
        // A brace that opens no placeholder: copied as-is.
        out.push('{');
        rest = &from_the_brace[1..];
    }
    out.push_str(rest);
    out
}

/// wpctl target for the volume: the default output, like the TUI.
pub const SINK: &str = "@DEFAULT_AUDIO_SINK@";
/// One wheel notch.
pub const VOLUME_UP: &str = "5%+";
pub const VOLUME_DOWN: &str = "5%-";

fn wpctl(args: &[&str]) -> Vec<String> {
    std::iter::once("wpctl").chain(args.iter().copied()).map(String::from).collect()
}

/// TUI tab opened by a left click on a module (alpheus `--tab`).
fn tab(m: ModuleId) -> Option<&'static str> {
    match m {
        ModuleId::Volume => Some("sound"),
        ModuleId::Network => Some("network"),
        ModuleId::Bluetooth => Some("bluetooth"),
        ModuleId::Battery => Some("home"),
        _ => None,
    }
}

fn module_command(bar: &Bar, m: ModuleId, b: Button) -> Option<Vec<String>> {
    match (m, b) {
        // Opens the launcher inside the daemon (`toggles_launcher`): no command.
        (ModuleId::Launcher, _) => None,
        // -l 1.0: never beyond 100 %, like the TUI.
        (ModuleId::Volume, Button::Right) => Some(wpctl(&["set-mute", SINK, "toggle"])),
        (ModuleId::Volume, Button::WheelUp) => Some(wpctl(&["set-volume", "-l", "1.0", SINK, VOLUME_UP])),
        (ModuleId::Volume, Button::WheelDown) => Some(wpctl(&["set-volume", "-l", "1.0", SINK, VOLUME_DOWN])),
        (ModuleId::Resources, Button::Left) => Some(bar.config.actions.resources.clone()),
        (m, Button::Left) => {
            let t = tab(m)?;
            Some(bar.config.actions.open_tab.iter().map(|a| substitute(a, &[("{tab}", t)])).collect())
        }
        _ => None,
    }
}

/// Command for a click on `target` in the bar of `output`. Going to a
/// workspace goes through the bar.toml command (`[workspaces] command`); the
/// system modules open a TUI tab (`[actions] open_tab`), btop (`[actions]
/// resources`), or change the volume through wpctl. Never by writing to the
/// compositor directly.
pub fn command(bar: &Bar, output: &str, target: &Target, b: Button) -> Option<Vec<String>> {
    match target {
        Target::Workspace { reference, output: o } => {
            let (workspace, workspace_output) = match b {
                Button::Left => (reference.clone(), o.clone()),
                Button::WheelDown | Button::WheelUp => {
                    let v = neighbor(bar, output, b == Button::WheelDown)?;
                    (v.reference.clone(), v.output.clone())
                }
                Button::Right | Button::Middle => return None,
            };
            let template = &bar.config.workspaces.command;
            Some(template.iter().map(|a| substitute(a, &[("{workspace}", &workspace), ("{output}", &workspace_output)])).collect())
        }
        Target::Module(m) => module_command(bar, *m, b),
        // The tray never runs a command: its gestures are D-Bus calls (tray plan 2, task 10).
        Target::TrayItem(_) => None,
    }
}

/// Left click on the launcher button: the daemon toggles its launcher
/// itself, nothing is run.
pub fn toggles_launcher(target: &Target, b: Button) -> bool {
    b == Button::Left && *target == Target::Module(ModuleId::Launcher)
}

/// Runs the command without waiting for it; a failure goes to the log.
pub fn run(exec: &dyn Executor, argv: &[String]) {
    let Some((prog, args)) = argv.split_first() else { return };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = exec.spawn(prog, &args) {
        log(&format!("{}: {e:?}", argv.join(" ")));
    }
}

/// Click at `x_logical` (logical px, like the Wayland pointer) on the bar of
/// `output`, whose `grid` is the last one drawn.
pub fn click(bar: &Bar, output: &str, grid: &Grid, x_logical: f64, scale: i32, advance: f32, b: Button) -> Option<Vec<String>> {
    let col = column_under(x_logical, scale, advance);
    command(bar, output, grid.target_at(col)?, b)
}

/// Notch size of `Gesture::Scroll` (controller decision 4: Qt's angle delta,
/// what Plasma sends).
pub const SCROLL_NOTCH: i32 = 120;

/// A bar button on a tray icon → its gesture: Left/Right/Middle; WheelUp →
/// Scroll(+120, Vertical), WheelDown → Scroll(-120, Vertical) (positive for
/// wheel up, decision 4).
pub fn tray_gesture(b: Button) -> Gesture {
    let vertical = |delta| Gesture::Scroll { delta, orientation: Orientation::Vertical };
    match b {
        Button::Left => Gesture::Left,
        Button::Right => Gesture::Right,
        Button::Middle => Gesture::Middle,
        Button::WheelUp => vertical(SCROLL_NOTCH),
        Button::WheelDown => vertical(-SCROLL_NOTCH),
    }
}

/// The horizontal wheel (a second `Wheel` fed with the horizontal axis, where
/// "down" is right): WheelDown (right) → +120, WheelUp (left) → -120,
/// Horizontal; a click is not a horizontal gesture.
pub fn tray_horizontal(b: Button) -> Option<Gesture> {
    let horizontal = |delta| Some(Gesture::Scroll { delta, orientation: Orientation::Horizontal });
    match b {
        Button::WheelDown => horizontal(SCROLL_NOTCH),
        Button::WheelUp => horizontal(-SCROLL_NOTCH),
        Button::Left | Button::Right | Button::Middle => None,
    }
}

/// Continuous scroll (touchpad) that counts as one wheel notch.
pub const CONTINUOUS_STEP: f64 = 30.0;

/// One wheel notch in `value120` (wl_pointer v8+, sent by Hyprland and niri;
/// `discrete` is no longer sent by recent compositors).
const NOTCH_120: i32 = 120;

#[derive(Debug, Default)]
pub struct Wheel {
    accumulated: f64,
    accumulated120: i32,
}

fn direction(positive: bool) -> Button {
    if positive { Button::WheelDown } else { Button::WheelUp }
}

impl Wheel {
    /// One wheel notch (value120 accumulated to ±120, or discrete from
    /// older compositors), or 30 px of continuous scroll, = one workspace.
    /// Wayland: positive = downward.
    pub fn add(&mut self, absolute: f64, discrete: i32, value120: i32) -> Option<Button> {
        if value120 != 0 {
            self.accumulated = 0.0;
            self.accumulated120 += value120;
            if self.accumulated120.abs() >= NOTCH_120 {
                let b = direction(self.accumulated120 > 0);
                self.accumulated120 = 0;
                return Some(b);
            }
            return None;
        }
        if discrete != 0 {
            self.accumulated = 0.0;
            return Some(direction(discrete > 0));
        }
        self.accumulated += absolute;
        if self.accumulated >= CONTINUOUS_STEP {
            self.accumulated = 0.0;
            Some(Button::WheelDown)
        } else if self.accumulated <= -CONTINUOUS_STEP {
            self.accumulated = 0.0;
            Some(Button::WheelUp)
        } else {
            None
        }
    }

    /// Clears the continuous remainder and the notch accumulator: nothing
    /// carries over to the next gesture. Called from wayland.rs when
    /// Wayland says THIS wheel gesture is over — `wl_pointer::axis_source`
    /// at `AxisScroll::stop` (`axis_stop`, a touchpad lifted) and
    /// `wl_pointer::leave` (pointer left the surface) — otherwise a
    /// remainder close to the threshold would wrongly combine with the next
    /// gesture.
    pub fn stop(&mut self) {
        self.accumulated = 0.0;
        self.accumulated120 = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::example;
    use crate::config::{BarConfig, ModuleId};
    use crate::state::Msg;
    use crate::system::*;
    use alpheus_commun::exec::fake::FakeExecutor;

    #[test]
    fn wheel_scrolls_vertical_and_horizontal() {
        let scroll = |delta, orientation| Gesture::Scroll { delta, orientation };
        assert_eq!(tray_gesture(Button::Left), Gesture::Left);
        assert_eq!(tray_gesture(Button::Right), Gesture::Right);
        assert_eq!(tray_gesture(Button::Middle), Gesture::Middle);
        assert_eq!(tray_gesture(Button::WheelUp), scroll(120, Orientation::Vertical), "wheel up: positive");
        assert_eq!(tray_gesture(Button::WheelDown), scroll(-120, Orientation::Vertical));
        assert_eq!(tray_horizontal(Button::WheelDown), Some(scroll(120, Orientation::Horizontal)), "right: positive");
        assert_eq!(tray_horizontal(Button::WheelUp), Some(scroll(-120, Orientation::Horizontal)));
        assert_eq!(tray_horizontal(Button::Left), None);
        // Through the wheel accumulator, as wayland.rs feeds it: Wayland's
        // positive value120 is down (vertical) or right (horizontal).
        let mut w = Wheel::default();
        assert_eq!(w.add(15.0, 0, 120).map(tray_gesture), Some(scroll(-120, Orientation::Vertical)));
        assert_eq!(w.add(-15.0, 0, -120).and_then(tray_horizontal), Some(scroll(-120, Orientation::Horizontal)));
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
        b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }) })));
        b.apply(Msg::Network(Some(NetState::Offline)));
        b.apply(Msg::Bluetooth(Some(BtState::On { devices: vec![] })));
        b.apply(Msg::Battery(Some(PowerState { laptop: Some(Battery { percent: 93, charge: Charge::Charging }), peripherals: vec![] })));
        b.apply(Msg::Resources(Sample { cpu: Some(12), memory: Some(61), temperature: Some(60) }));
        b
    }

    #[test]
    fn volume_clicks_tab_mute_and_wheel() {
        let b = full();
        let v = Target::Module(ModuleId::Volume);
        assert_eq!(command(&b, "HDMI-A-1", &v, Button::Left), argv(&["alpheus-toggle", "sound"]));
        assert_eq!(command(&b, "HDMI-A-1", &v, Button::Right), argv(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]));
        assert_eq!(
            command(&b, "HDMI-A-1", &v, Button::WheelUp),
            argv(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"])
        );
        assert_eq!(
            command(&b, "HDMI-A-1", &v, Button::WheelDown),
            argv(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%-"])
        );
        assert_eq!(command(&b, "HDMI-A-1", &v, Button::Middle), None);
    }

    #[test]
    fn indicators_open_their_tab_resources_open_the_monitor() {
        let b = full();
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
        let mut b = full();
        b.config.actions.open_tab = vec!["my-panel".into(), "--tab={tab}".into(), "{output}".into()];
        b.config.actions.resources = vec!["foot".into(), "htop".into()];
        assert_eq!(
            command(&b, "eDP-1", &Target::Module(ModuleId::Volume), Button::Left),
            argv(&["my-panel", "--tab=sound", "{output}"]),
            "only {{tab}} is a placeholder here"
        );
        assert_eq!(command(&b, "eDP-1", &Target::Module(ModuleId::Resources), Button::Left), argv(&["foot", "htop"]));
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
        let b = full();
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
        use crate::tray::ItemKey;
        let t = Target::TrayItem(ItemKey { owner: ":1.7".into(), path: "/StatusNotifierItem".into() });
        for b in [Button::Left, Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
            assert_eq!(command(&full(), "eDP-1", &t, b), None, "{b:?}");
        }
    }
}
