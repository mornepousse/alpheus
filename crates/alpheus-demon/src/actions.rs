//! Click and wheel → command. Going to a workspace goes through the
//! bar.toml command (`[workspaces] command`, default `wm-set workspace …`),
//! never by writing to the compositor directly. The wheel computes the
//! neighboring workspace itself and sends an absolute reference.

use crate::compositor::Workspace;
use crate::config::ModuleId;
use crate::grid::{Grid, Target};
use crate::media::Control;
use crate::painter::column_under;
use crate::session_bus::{Gesture, Orientation};
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
    m.spec().tab
}

/// The TUI tab of `m`, through `[actions] open_tab`.
fn open_tab(bar: &Bar, m: ModuleId) -> Option<Vec<String>> {
    let t = tab(m)?;
    Some(bar.config.actions.open_tab.iter().map(|a| substitute(a, &[("{tab}", t)])).collect())
}

fn module_command(bar: &Bar, m: ModuleId, b: Button) -> Option<Vec<String>> {
    let quick = bar.config.modules.contains(&ModuleId::Quick);
    match (m, b) {
        // Opens the launcher inside the daemon (`toggles_launcher`): no command.
        (ModuleId::Launcher, _) => None,
        // With the quick panel (quick spec § 1): the left click opens it (`ModulePopup::Quick`), the
        // right click takes over the TUI tab the left click used to open.
        (ModuleId::Volume | ModuleId::Network | ModuleId::Bluetooth, Button::Left) if quick => None,
        (ModuleId::Volume | ModuleId::Network | ModuleId::Bluetooth, Button::Right) if quick => open_tab(bar, m),
        // -l 1.0: never beyond 100 %, like the TUI.
        (ModuleId::Volume, Button::Right) => Some(wpctl(&["set-mute", SINK, "toggle"])),
        (ModuleId::Volume, Button::WheelUp) => Some(wpctl(&["set-volume", "-l", "1.0", SINK, VOLUME_UP])),
        (ModuleId::Volume, Button::WheelDown) => Some(wpctl(&["set-volume", "-l", "1.0", SINK, VOLUME_DOWN])),
        (ModuleId::Resources, Button::Right) => Some(bar.config.actions.resources.clone()),
        // The default source's mute, the quick panel's microphone command (privacy spec § 6).
        (ModuleId::Privacy, Button::Middle) => Some(wpctl(&["set-mute", crate::quick::SOURCE, "toggle"])),
        (m, Button::Left) => open_tab(bar, m),
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
        // The tray never runs a command: its gestures are D-Bus calls.
        Target::TrayItem(_) => None,
    }
}

/// Left click on the launcher button: the daemon toggles its launcher
/// itself, nothing is run.
pub fn toggles_launcher(target: &Target, b: Button) -> bool {
    b == Button::Left && *target == Target::Module(ModuleId::Launcher)
}

/// The bell's middle button (notifications spec § 4): do-not-disturb flips inside the daemon,
/// nothing is run. The left button is the panel's (`ModulePopup`).
pub fn toggles_dnd(target: &Target, b: Button) -> bool {
    b == Button::Middle && *target == Target::Module(ModuleId::Notifications)
}

/// The media module's middle button and wheel (spec § 1): a D-Bus call to the active player
/// (`Mpris::control`), never a command; the left button is its popup's (`ModulePopup`).
pub fn media_control(target: &Target, b: Button) -> Option<Control> {
    match (target, b) {
        (Target::Module(ModuleId::Media), Button::Middle) => Some(Control::PlayPause),
        (Target::Module(ModuleId::Media), Button::WheelUp) => Some(Control::Next),
        (Target::Module(ModuleId::Media), Button::WheelDown) => Some(Control::Previous),
        _ => None,
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

/// Notch size of `Gesture::Scroll` (Qt's angle delta,
/// what Plasma sends).
pub const SCROLL_NOTCH: i32 = 120;

/// A bar button on a tray icon → its gesture: Left/Right/Middle; WheelUp →
/// Scroll(+120, Vertical), WheelDown → Scroll(-120, Vertical) (positive for
/// wheel up).
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
/// "down" is right): WheelDown (right) → -120, WheelUp (left) → +120,
/// Horizontal; a click is not a horizontal gesture.
///
/// Flipped from the naive mapping (right = positive) to match what Plasma
/// actually sends: qtbase negates Wayland's `value120` on both axes
/// (`qwaylandinputdevice.cpp`, `FrameData::angleDelta()` returns
/// `-delta120`), and Plasma passes `wheel.angleDelta.x` through unchanged for
/// its "horizontal" signal — so under Plasma, right is -120 and left is
/// +120. Vertical is unaffected (up stays +120, matching Qt's own vertical
/// convention). A libayatana-based tray item that assumes "right = positive"
/// will read our horizontal scrolls backwards under Plasma too — expected,
/// not a bug we can fix on our side.
pub fn tray_horizontal(b: Button) -> Option<Gesture> {
    let horizontal = |delta| Some(Gesture::Scroll { delta, orientation: Orientation::Horizontal });
    match b {
        Button::WheelDown => horizontal(-SCROLL_NOTCH),
        Button::WheelUp => horizontal(SCROLL_NOTCH),
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
mod tests;
