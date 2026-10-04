//! Click and wheel → command. Going to a workspace goes through the
//! bar.toml command (`[workspaces] command`, default `wm-set workspace …`),
//! never by writing to the compositor directly. The wheel computes the
//! neighboring workspace itself and sends an absolute reference.

use crate::compositor::Workspace;
use crate::grid::{Grid, Target};
use crate::painter::column_under;
use crate::state::{Bar, Feed};
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

/// Replaces `{workspace}` and `{output}` in a single pass over `template`
/// (the bar.toml text, never the already-substituted result): a workspace
/// reference that would literally contain `{output}` (unlikely, but
/// possible) is therefore never itself run back through the mill — unlike
/// two successive calls to `str::replace` on the already-substituted
/// string, where the second pass would run back over what the first one
/// inserted.
fn substitute(template: &str, workspace: &str, output: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let from_the_brace = &rest[i..];
        if let Some(after) = from_the_brace.strip_prefix("{workspace}") {
            out.push_str(workspace);
            rest = after;
        } else if let Some(after) = from_the_brace.strip_prefix("{output}") {
            out.push_str(output);
            rest = after;
        } else {
            // A brace that opens neither placeholder: copied as-is, move
            // forward by one character.
            out.push('{');
            rest = &from_the_brace[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Command for a click on `target` in the bar of `output`.
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
            Some(template.iter().map(|a| substitute(a, &workspace, &workspace_output)).collect())
        }
    }
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
    /// carries over to the next gesture. To be called when Wayland says THIS
    /// wheel gesture is over — `wl_pointer::axis_source` at `AxisScroll::stop`
    /// (`axis_stop`, a touchpad lifted) and `wl_pointer::leave` (pointer left
    /// the surface) — otherwise a remainder close to the threshold would
    /// wrongly combine with the next gesture. Wired up in task 12
    /// (Wayland); this task only provides the method and its test.
    pub fn stop(&mut self) {
        self.accumulated = 0.0;
        self.accumulated120 = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::example;
    use crate::config::BarConfig;
    use crate::state::Msg;
    use alpheus_commun::exec::fake::FakeExecutor;

    #[test]
    fn click_chain_coordinate_cell_zone_command() {
        let b = bar();
        let g = b.grid("HDMI-A-1", 100); // "4" in cells 5 to 13
        // 8 px advance at scale 1, 16 px at scale 2: same cell.
        assert_eq!(click(&b, "HDMI-A-1", &g, 52.0, 1, 8.0, Button::Left), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
        assert_eq!(click(&b, "HDMI-A-1", &g, 52.0, 2, 16.0, Button::Left), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
        assert_eq!(click(&b, "HDMI-A-1", &g, 4.0, 1, 8.0, Button::Left), None, "margin");
        assert_eq!(click(&b, "HDMI-A-1", &g, 400.0, 1, 8.0, Button::Left), None, "title");
        assert_eq!(click(&b, "HDMI-A-1", &g, 52.0, 1, 8.0, Button::WheelDown), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
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
}
