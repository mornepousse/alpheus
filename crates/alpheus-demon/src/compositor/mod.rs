//! What the bar knows about the compositor, whichever it is: workspaces,
//! windows per workspace, urgency, active window. `hypr` and `niri`
//! translate their compositor into this normalized state; the rest of the
//! daemon only ever sees this one.

pub mod hypr;
pub mod niri;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompositorState {
    pub workspaces: Vec<Workspace>,
    /// Window that has focus, if any.
    pub window: Option<Window>,
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
    /// Class (Hyprland) or app_id (niri).
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_by_output_and_unknown_output() {
        let e = example();
        let names = |s: &str| e.workspaces_of(s).iter().map(|w| w.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names("HDMI-A-1"), ["2", "4", "5"]);
        assert_eq!(names("DVI-I-1"), ["3", "web"]);
        assert_eq!(names(""), ["1", "2", "4", "5", "3", "web"]);
        assert_eq!(names("HEADLESS-1"), ["1", "2", "4", "5", "3", "web"]);
    }
}
