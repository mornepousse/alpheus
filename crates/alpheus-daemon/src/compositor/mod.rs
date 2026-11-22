//! What the bar knows about the compositor, whichever it is: workspaces,
//! windows per workspace, urgency, active window. `hypr`, `niri`, `mango` and `sway`
//! translate their compositor into this normalized state; the rest of the
//! daemon only ever sees this one.

use std::time::Duration;

pub mod hypr;
pub mod mango;
pub mod niri;
pub mod sway;

/// Levels of JSON nesting (`{` and `[`) a Sway reply or a Mango line may reach, checked before
/// serde_json recurses into it. Measured on a 256 KiB thread (the compositor thread's stack) in a
/// debug build: serde_json overflows at 128 levels of sway's object/`nodes` shape and at about 110
/// levels of objects with several keys (before its own limit of 128); with 24 KiB of the stack
/// already used, the latter parse at 95 levels and overflow at 100. A release build refuses 1,000
/// levels at 128 even on 64 KiB. 90 levels parse in both shapes with that 24 KiB used
/// (`sway::tests::a_reply_at_max_nesting_parses_on_the_compositor_stack`). They hold about 44
/// Sway containers (a tree `sway::MAX_DEPTH` deep would need about 130 levels); a real tree nests
/// a handful, a Mango line 4.
pub const MAX_NESTING: usize = 90;

/// The text nests past MAX_NESTING, counted outside strings. One pass, no recursion.
pub(crate) fn too_deep(bytes: &[u8]) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &b in bytes {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_NESTING {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompositorState {
    pub workspaces: Vec<Workspace>,
    /// Window that has focus, if any.
    pub window: Option<Window>,
    /// Mapped windows; the focused one LAST (the first is the previously
    /// focused one, alt-tab style). Read by the launcher's window list.
    pub toplevels: Vec<Toplevel>,
    /// The compositor says a screen is being cast (privacy spec § 3). Never names the app.
    pub casting: bool,
    /// How long a release of `casting` still counts as casting: Hyprland's `0` comes about
    /// 500 ms after the last frame and a screenshot blips, niri's events are exact (zero).
    pub cast_hold: Duration,
}

/// One mapped window, as the launcher lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toplevel {
    /// What the `focus` template's {window} becomes: "address:0x583d9c60da90"
    /// (Hyprland), "id:12" (niri, Mango, Sway) — the selectors `wm-set focus-window` takes.
    pub selector: String,
    /// Hyprland class / niri, Mango and Sway app_id (Sway: the X11 class under Xwayland).
    pub class: String,
    pub title: String,
    /// Workspace name as the bar shows it.
    pub workspace: String,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// Shown in the bar.
    pub name: String,
    /// Passed to `wm-set workspace` to go there.
    pub reference: String,
    /// Name of the output (eDP-1, HDMI-A-1…).
    pub output: String,
    pub windows: u16,
    pub urgent: bool,
    /// Workspace shown on its output.
    pub visible: bool,
    /// Visible on the output that has focus.
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Class (Hyprland) or app_id (niri, Mango, Sway).
    pub class: String,
    pub title: String,
}

impl CompositorState {
    /// Workspaces of output `output`, in order. An output the compositor
    /// knows no workspace for (empty name, a screen just plugged in): every
    /// workspace, rather than an empty bar.
    pub fn workspaces_of(&self, output: &str) -> Vec<&Workspace> {
        let a: Vec<&Workspace> = self.workspaces.iter().filter(|w| w.output == output).collect();
        if a.is_empty() { self.workspaces.iter().collect() } else { a }
    }

    /// Output of the focused workspace, if any: where the launcher opens.
    pub fn focused_output(&self) -> Option<&str> {
        self.workspaces.iter().find(|w| w.focused).map(|w| w.output.as_str())
    }
}

/// Reference state for tests: three screens, an urgent workspace with six
/// windows, a named workspace.
#[cfg(test)]
pub(crate) fn example() -> CompositorState {
    let w = |name: &str, reference: &str, output: &str, windows: u16, urgent: bool, visible: bool, focused: bool| Workspace {
        name: name.into(),
        reference: reference.into(),
        output: output.into(),
        windows,
        urgent,
        visible,
        focused,
    };
    let t = |selector: &str, class: &str, title: &str, workspace: &str, focused: bool| Toplevel {
        selector: selector.into(),
        class: class.into(),
        title: title.into(),
        workspace: workspace.into(),
        focused,
    };
    CompositorState {
        workspaces: vec![
            w("1", "1", "eDP-1", 0, false, true, false),
            w("2", "2", "HDMI-A-1", 1, false, true, true),
            w("4", "4", "HDMI-A-1", 6, true, false, false),
            w("5", "5", "HDMI-A-1", 0, false, false, false),
            w("3", "3", "DVI-I-1", 2, false, true, false),
            w("web", "name:web", "DVI-I-1", 1, false, false, false),
        ],
        window: Some(Window { class: "firefox".into(), title: "Web page — Mozilla Firefox".into() }),
        toplevels: vec![
            t("address:0x5a00000000a1", "Alacritty", "~/projects — zsh", "3", false),
            t("address:0x5a00000000a2", "org.gnome.Nautilus", "Documents", "4", false),
            t("address:0x5a00000000a3", "firefox", "Web page — Mozilla Firefox", "2", true),
        ],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-check counts `{` and `[` outside strings only: brackets in a title, or after an
    /// escaped quote, are text.
    #[test]
    fn nesting_is_counted_outside_strings() {
        let brackets = "[{".repeat(200);
        assert!(!too_deep(format!(r#"{{"name":"{brackets}"}}"#).as_bytes()));
        assert!(!too_deep(format!(r#"{{"name":"q\"{brackets}\\"}}"#).as_bytes()), "an escaped quote stays inside");
        assert!(too_deep(format!(r#"{{"name":"\\",{brackets}}}"#).as_bytes()), "an escaped backslash ends the string");
        let levels = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(!too_deep(levels(MAX_NESTING).as_bytes()));
        assert!(too_deep(levels(MAX_NESTING + 1).as_bytes()));
        assert!(!too_deep(format!("{}{}", levels(MAX_NESTING), levels(MAX_NESTING)).as_bytes()), "depth, not a count");
    }

    #[test]
    fn workspaces_by_output_and_unknown_output() {
        let e = example();
        let names = |s: &str| e.workspaces_of(s).iter().map(|w| w.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names("HDMI-A-1"), ["2", "4", "5"]);
        assert_eq!(names("DVI-I-1"), ["3", "web"]);
        assert_eq!(names(""), ["1", "2", "4", "5", "3", "web"]);
        assert_eq!(names("HEADLESS-1"), ["1", "2", "4", "5", "3", "web"]);
    }

    #[test]
    fn focused_output_follows_the_focused_workspace() {
        let mut e = example();
        assert_eq!(e.focused_output(), Some("HDMI-A-1"));
        for w in &mut e.workspaces {
            w.focused = false;
        }
        assert_eq!(e.focused_output(), None, "no focused workspace: the compositor picks");
        e.workspaces[4].focused = true;
        assert_eq!(e.focused_output(), Some("DVI-I-1"));
    }
}
