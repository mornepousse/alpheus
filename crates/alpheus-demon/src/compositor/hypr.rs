//! Hyprland, pure parsing (no I/O here, see sources::hypr, which names the
//! two sockets): lines from the event stream ("name>>data") and JSON
//! responses of the four read requests. The stream never gives back the
//! initial state nor past urgency: state comes from the JSON responses,
//! urgency from `urgent>>` events alone.

use super::{CompositorState, Toplevel, Window, Workspace};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Workspaces, windows or screens changed: reread the state.
    Structure,
    /// `activewindow>>class,title` (None: no more active window).
    ActiveWindow(Option<Window>),
    /// `activewindowv2>>address`.
    Focus(Option<String>),
    /// `windowtitlev2>>address,title`.
    Title {
        address: String,
        title: String,
    },
    Urgent(String),
    /// `closewindow>>address` (also structure).
    Closed(String),
    Ignore,
}

/// Normalized address: no "0x", lowercase (the stream writes
/// `583d9c60da90`, the JSON `0x583d9c60da90`).
pub fn address(s: &str) -> String {
    s.trim().trim_start_matches("0x").to_ascii_lowercase()
}

/// Events after which the state is reread (workspaces, windows, screens).
/// `configreloaded`: Hyprland reloaded its config (a reload command, or a
/// script that regenerates it) — workspaces, monitors or window rules may
/// have changed without any other structure event being emitted.
const STRUCTURE: [&str; 21] = [
    "workspace",
    "workspacev2",
    "focusedmon",
    "focusedmonv2",
    "createworkspace",
    "createworkspacev2",
    "destroyworkspace",
    "destroyworkspacev2",
    "moveworkspace",
    "moveworkspacev2",
    "renameworkspace",
    "openwindow",
    "movewindow",
    "movewindowv2",
    "monitoradded",
    "monitoraddedv2",
    "monitorremoved",
    "monitorremovedv2",
    "activespecial",
    "activespecialv2",
    "configreloaded",
];

pub fn parse_line(line: &str) -> Event {
    let Some((name, data)) = line.split_once(">>") else { return Event::Ignore };
    match name {
        "activewindow" => {
            // The class has no comma; the title may have some.
            let (class, title) = data.split_once(',').unwrap_or((data, ""));
            if class.is_empty() && title.is_empty() {
                Event::ActiveWindow(None)
            } else {
                Event::ActiveWindow(Some(Window { class: class.into(), title: title.into() }))
            }
        }
        "activewindowv2" => {
            let a = address(data);
            Event::Focus((!a.is_empty()).then_some(a))
        }
        "windowtitlev2" => match data.split_once(',') {
            Some((a, t)) => Event::Title { address: address(a), title: t.into() },
            None => Event::Ignore,
        },
        "urgent" => Event::Urgent(address(data)),
        "closewindow" => Event::Closed(address(data)),
        n if STRUCTURE.contains(&n) => Event::Structure,
        _ => Event::Ignore,
    }
}

#[derive(Deserialize)]
struct WorkspaceRef {
    id: i64,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct Monitor {
    #[serde(default)]
    focused: bool,
    #[serde(rename = "activeWorkspace")]
    active: WorkspaceRef,
}

#[derive(Deserialize)]
struct RawWorkspace {
    id: i64,
    name: String,
    #[serde(default)]
    monitor: String,
    #[serde(default)]
    windows: u16,
}

fn yes() -> bool {
    true
}

/// Never focused (or a Hyprland too old to say): after every other window.
fn never_focused() -> i64 {
    i64::MAX
}

#[derive(Deserialize)]
struct Client {
    address: String,
    workspace: WorkspaceRef,
    #[serde(default = "yes")]
    mapped: bool,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    /// 0: the focused window, 1: the one before…
    #[serde(rename = "focusHistoryID", default = "never_focused")]
    focus_history: i64,
}

/// A mapped window outside special workspaces, for the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientInfo {
    /// Normalized (see `address`).
    address: String,
    class: String,
    title: String,
    workspace: String,
}

#[derive(Deserialize, Default)]
struct RawActiveWindow {
    #[serde(default)]
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
}

/// Raw responses to the four read requests.
pub struct Responses<'a> {
    pub monitors: &'a str,
    pub workspaces: &'a str,
    pub clients: &'a str,
    pub active: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// Sorted workspaces, specials excluded, no urgency (the JSON says nothing about it).
    workspaces: Vec<Workspace>,
    /// Hyprland id of each workspace, in the same order.
    ids: Vec<i64>,
    /// Window address → id of its workspace.
    windows: BTreeMap<String, i64>,
    active: Option<(String, Window)>,
    /// Mapped windows outside special workspaces, most recently focused first.
    clients: Vec<ClientInfo>,
}

impl Snapshot {
    pub fn parse(r: &Responses) -> Result<Snapshot, String> {
        fn read<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T, String> {
            serde_json::from_str(text).map_err(|e| format!("unreadable {what} response: {e}"))
        }
        let monitors: Vec<Monitor> = read("j/monitors", r.monitors)?;
        let mut workspaces: Vec<RawWorkspace> = read("j/workspaces", r.workspaces)?;
        let clients: Vec<Client> = read("j/clients", r.clients)?;
        let active: RawActiveWindow = read("j/activewindow", r.active)?;
        // Special workspaces (scratchpads): never in the bar.
        workspaces.retain(|w| !w.name.starts_with("special:"));
        // Numbered first, in order; named ones (negative id) after.
        workspaces.sort_by_key(|w| (w.id <= 0, w.id.unsigned_abs()));
        let mut out = Snapshot::default();
        for w in workspaces {
            let m = monitors.iter().find(|m| m.active.id == w.id);
            let reference = if w.id > 0 { w.id.to_string() } else { format!("name:{}", w.name) };
            out.ids.push(w.id);
            out.workspaces.push(Workspace {
                reference,
                name: w.name,
                output: w.monitor,
                windows: w.windows,
                urgent: false,
                visible: m.is_some(),
                focused: m.is_some_and(|m| m.focused),
            });
        }
        out.windows = clients.iter().map(|c| (address(&c.address), c.workspace.id)).collect();
        let mut listed: Vec<&Client> = clients.iter().filter(|c| c.mapped && !c.workspace.name.starts_with("special:")).collect();
        // Hyprland writes -1 for a window never focused: after every focused one.
        listed.sort_by_key(|c| if c.focus_history < 0 { i64::MAX } else { c.focus_history });
        out.clients = listed
            .into_iter()
            .map(|c| ClientInfo {
                address: address(&c.address),
                class: c.class.clone(),
                title: c.title.clone(),
                workspace: c.workspace.name.clone(),
            })
            .collect();
        if !active.address.is_empty() || !active.class.is_empty() {
            out.active = Some((address(&active.address), Window { class: active.class, title: active.title }));
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Nothing,
    Changed,
    Reread,
}

#[derive(Debug, Default)]
pub struct Model {
    snapshot: Snapshot,
    urgent: BTreeSet<String>,
    active: Option<String>,
    window: Option<Window>,
}

impl Model {
    /// New state read: replaces the old one. Urgency only ever comes from
    /// the stream, we keep it for windows that still exist.
    pub fn set_snapshot(&mut self, s: Snapshot) {
        self.urgent.retain(|a| s.windows.contains_key(a));
        match &s.active {
            Some((a, w)) => {
                self.active = Some(a.clone());
                self.window = Some(w.clone());
            }
            None => {
                self.active = None;
                self.window = None;
            }
        }
        self.snapshot = s;
    }

    pub fn apply(&mut self, e: &Event) -> Effect {
        match e {
            Event::Structure => Effect::Reread,
            Event::Closed(a) => {
                self.urgent.remove(a);
                Effect::Reread
            }
            Event::ActiveWindow(w) => {
                if &self.window == w {
                    Effect::Nothing
                } else {
                    self.window = w.clone();
                    Effect::Changed
                }
            }
            Event::Focus(a) => {
                self.active = a.clone();
                // Most recently focused first, like focusHistoryID at the next
                // reread: the launcher lists the previous window first.
                if let Some(i) = a.as_ref().and_then(|a| self.snapshot.clients.iter().position(|c| &c.address == a)) {
                    let c = self.snapshot.clients.remove(i);
                    self.snapshot.clients.insert(0, c);
                }
                match a {
                    Some(a) if self.urgent.remove(a) => Effect::Changed,
                    _ => Effect::Nothing, // the title arrives through `activewindow`
                }
            }
            Event::Title { address, title } => {
                // Any window's title is kept for the launcher's list, silently:
                // it is published with the next state, never an extra wake-up.
                if let Some(c) = self.snapshot.clients.iter_mut().find(|c| &c.address == address) {
                    c.title.clone_from(title);
                }
                match (&self.active, &mut self.window) {
                    (Some(a), Some(w)) if a == address && &w.title != title => {
                        w.title = title.clone();
                        Effect::Changed
                    }
                    _ => Effect::Nothing,
                }
            }
            Event::Urgent(a) => {
                if self.active.as_ref() != Some(a) && self.urgent.insert(a.clone()) {
                    Effect::Changed
                } else {
                    Effect::Nothing
                }
            }
            Event::Ignore => Effect::Nothing,
        }
    }

    pub fn state(&self) -> CompositorState {
        let urgent: BTreeSet<i64> = self.urgent.iter().filter_map(|a| self.snapshot.windows.get(a)).copied().collect();
        let workspaces = self
            .snapshot
            .workspaces
            .iter()
            .zip(&self.snapshot.ids)
            .map(|(w, id)| Workspace { urgent: urgent.contains(id), ..w.clone() })
            .collect();
        let mut toplevels: Vec<Toplevel> = self
            .snapshot
            .clients
            .iter()
            .map(|c| Toplevel {
                selector: format!("address:0x{}", c.address),
                class: c.class.clone(),
                title: c.title.clone(),
                workspace: c.workspace.clone(),
                focused: self.active.as_deref() == Some(c.address.as_str()),
            })
            .collect();
        if let Some(i) = toplevels.iter().position(|t| t.focused) {
            let focused = toplevels.remove(i);
            toplevels.push(focused);
        }
        CompositorState { workspaces, window: self.window.clone(), toplevels }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = include_str!("../../tests/fixtures/hypr-monitors.json");
    const WORKSPACES: &str = include_str!("../../tests/fixtures/hypr-workspaces.json");
    const CLIENTS: &str = include_str!("../../tests/fixtures/hypr-clients.json");
    const ACTIVE: &str = include_str!("../../tests/fixtures/hypr-activewindow.json");
    const EVENTS: &str = include_str!("../../tests/fixtures/hypr-evenements.txt");

    fn model_from_fixture() -> Model {
        let s = Snapshot::parse(&Responses { monitors: MONITORS, workspaces: WORKSPACES, clients: CLIENTS, active: ACTIVE }).unwrap();
        let mut m = Model::default();
        m.set_snapshot(s);
        m
    }

    fn workspace<'a>(e: &'a CompositorState, name: &str) -> &'a Workspace {
        e.workspaces.iter().find(|w| w.name == name).unwrap_or_else(|| panic!("workspace {name} missing: {e:?}"))
    }

    #[test]
    fn snapshot_from_fixture() {
        let e = model_from_fixture().state();
        assert_eq!(e.workspaces.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["1", "2", "3"]);
        let w1 = workspace(&e, "1");
        assert_eq!((w1.output.as_str(), w1.windows, w1.visible, w1.focused), ("eDP-1", 0, true, false));
        let w2 = workspace(&e, "2");
        assert_eq!((w2.output.as_str(), w2.windows, w2.visible, w2.focused), ("HDMI-A-1", 1, true, true));
        let w3 = workspace(&e, "3");
        assert_eq!((w3.output.as_str(), w3.windows, w3.visible, w3.focused, w3.reference.as_str()), ("DVI-I-1", 2, true, false, "3"));
        assert!(e.workspaces.iter().all(|w| !w.urgent));
        assert_eq!(e.window, Some(Window { class: "firefox".into(), title: "Page web — Mozilla Firefox".into() }));
    }

    #[test]
    fn stream_lines() {
        let f = |c: &str, t: &str| Event::ActiveWindow(Some(Window { class: c.into(), title: t.into() }));
        assert_eq!(parse_line("activewindow>>firefox,Title, with, commas"), f("firefox", "Title, with, commas"));
        assert_eq!(parse_line("activewindow>>,"), Event::ActiveWindow(None));
        assert_eq!(parse_line("activewindowv2>>583D9C60DA90"), Event::Focus(Some("583d9c60da90".into())));
        assert_eq!(parse_line("activewindowv2>>"), Event::Focus(None));
        assert_eq!(parse_line("windowtitlev2>>583d9c60da90,a,b"), Event::Title { address: "583d9c60da90".into(), title: "a,b".into() });
        assert_eq!(parse_line("urgent>>0x583d9d5fd540"), Event::Urgent("583d9d5fd540".into()));
        assert_eq!(parse_line("closewindow>>583d9c8a1b20"), Event::Closed("583d9c8a1b20".into()));
        for s in [
            "openwindow>>1,2,a,b",
            "workspacev2>>3,3",
            "focusedmonv2>>DVI-I-1,3",
            "monitorremovedv2>>2,DVI-I-1,x",
            "renameworkspace>>3,web",
            "configreloaded>>", // Hyprland config reload: state to reread
        ] {
            assert_eq!(parse_line(s), Event::Structure, "{s}");
        }
        for s in ["windowtitle>>583d9c60da90", "bell>>x", "no separator", ""] {
            assert_eq!(parse_line(s), Event::Ignore, "{s}");
        }
    }

    #[test]
    fn fixture_stream_applied() {
        let mut m = model_from_fixture();
        let mut effects = Vec::new();
        for l in EVENTS.lines() {
            effects.push(m.apply(&parse_line(l)));
            if l.starts_with("activewindowv2>>583d9c60da90") {
                assert_eq!(m.state().window.unwrap().title, "Autre page — Mozilla Firefox");
            }
            if l.starts_with("urgent>>") {
                assert!(workspace(&m.state(), "3").urgent, "urgent window on workspace 3");
            }
        }
        assert!(effects.contains(&Effect::Reread) && effects.contains(&Effect::Changed));
        let e = m.state();
        assert_eq!(e.window.as_ref().unwrap().class, "Alacritty");
        assert!(workspace(&e, "3").urgent, "another window taking focus does not clear urgency");
    }

    #[test]
    fn urgency_cleared_when_the_window_gets_focus() {
        let mut m = model_from_fixture();
        assert_eq!(m.apply(&Event::Urgent("583d9d5fd540".into())), Effect::Changed);
        assert!(workspace(&m.state(), "3").urgent);
        assert_eq!(m.apply(&Event::Focus(Some("583d9d5fd540".into()))), Effect::Changed);
        assert!(!workspace(&m.state(), "3").urgent);
        assert_eq!(m.apply(&Event::Urgent("583d9d5fd540".into())), Effect::Nothing, "active window: not urgent");
    }

    #[test]
    fn special_workspaces_hidden_named_workspaces_reachable() {
        let workspaces = r#"[{"id": -98, "name": "special:magic", "monitor": "eDP-1", "windows": 1},
                          {"id": -1337, "name": "web", "monitor": "eDP-1", "windows": 2},
                          {"id": 1, "name": "1", "monitor": "eDP-1", "windows": 0}]"#;
        let monitors = r#"[{"name": "eDP-1", "focused": true, "activeWorkspace": {"id": 1, "name": "1"}}]"#;
        let s = Snapshot::parse(&Responses { monitors, workspaces, clients: "[]", active: "{}" }).unwrap();
        let mut m = Model::default();
        m.set_snapshot(s);
        let e = m.state();
        let r: Vec<(&str, &str)> = e.workspaces.iter().map(|w| (w.name.as_str(), w.reference.as_str())).collect();
        assert_eq!(r, [("1", "1"), ("web", "name:web")]);
        assert_eq!(e.window, None);
    }

    /// (selector, class, title, workspace, focused) of each toplevel.
    fn toplevels(e: &CompositorState) -> Vec<(&str, &str, &str, &str, bool)> {
        e.toplevels.iter().map(|t| (t.selector.as_str(), t.class.as_str(), t.title.as_str(), t.workspace.as_str(), t.focused)).collect()
    }

    #[test]
    fn toplevels_from_clients_focused_last() {
        let e = model_from_fixture().state();
        assert_eq!(
            toplevels(&e),
            [
                ("address:0x583d9d5fd540", "firefox", "Page web — Mozilla Firefox", "3", false),
                ("address:0x583d9c701c60", "Alacritty", "~/projets — zsh", "3", false),
                ("address:0x583d9c60da90", "firefox", "Page web — Mozilla Firefox", "2", true),
            ],
            "focusHistoryID order (1, 2), the focused one (0) last"
        );
    }

    #[test]
    fn focus_moves_the_window_last_without_a_reread() {
        let mut m = model_from_fixture();
        assert_eq!(m.apply(&Event::Focus(Some("583d9c701c60".into()))), Effect::Nothing);
        let e = m.state();
        let order: Vec<&str> = e.toplevels.iter().map(|t| t.selector.as_str()).collect();
        assert_eq!(order, ["address:0x583d9c60da90", "address:0x583d9d5fd540", "address:0x583d9c701c60"], "previously focused first");
        assert!(e.toplevels[2].focused && !e.toplevels[0].focused);
        // A second focus change: the window focused just before comes first
        // (alt-tab), not the snapshot's old order.
        assert_eq!(m.apply(&Event::Focus(Some("583d9d5fd540".into()))), Effect::Nothing);
        let e = m.state();
        let order: Vec<&str> = e.toplevels.iter().map(|t| t.selector.as_str()).collect();
        assert_eq!(order, ["address:0x583d9c701c60", "address:0x583d9c60da90", "address:0x583d9d5fd540"], "most recent first");
    }

    #[test]
    fn unmapped_and_special_clients_are_not_listed() {
        let clients = r#"[
            {"address": "0x1e", "mapped": true, "workspace": {"id": 1, "name": "1"}, "class": "foot", "title": "e", "focusHistoryID": -1},
            {"address": "0x1a", "mapped": true, "workspace": {"id": 1, "name": "1"}, "class": "foot", "title": "a", "focusHistoryID": 1},
            {"address": "0x1b", "mapped": false, "workspace": {"id": 1, "name": "1"}, "class": "foot", "title": "b", "focusHistoryID": 2},
            {"address": "0x1c", "mapped": true, "workspace": {"id": -98, "name": "special:magic"}, "class": "foot", "title": "c", "focusHistoryID": 3},
            {"address": "0x1d", "workspace": {"id": -1337, "name": "web"}, "class": "firefox", "title": "d"}
        ]"#;
        let monitors = r#"[{"name": "eDP-1", "focused": true, "activeWorkspace": {"id": 1, "name": "1"}}]"#;
        let workspaces = r#"[{"id": 1, "name": "1", "monitor": "eDP-1", "windows": 2}]"#;
        let s = Snapshot::parse(&Responses { monitors, workspaces, clients, active: "{}" }).unwrap();
        let mut m = Model::default();
        m.set_snapshot(s);
        let e = m.state();
        assert_eq!(
            toplevels(&e),
            [
                ("address:0x1a", "foot", "a", "1", false),
                ("address:0x1e", "foot", "e", "1", false),
                ("address:0x1d", "firefox", "d", "web", false)
            ],
            "unmapped and special:magic left out; never focused (-1) or no mapped/focusHistoryID: listed, last"
        );
    }

    #[test]
    fn a_background_title_change_is_kept_silently() {
        let mut m = model_from_fixture();
        let e = Event::Title { address: "583d9c701c60".into(), title: "vim notes.md".into() };
        assert_eq!(m.apply(&e), Effect::Nothing, "not the active window: no extra publication");
        let s = m.state();
        let t = s.toplevels.iter().find(|t| t.selector == "address:0x583d9c701c60").unwrap();
        assert_eq!(t.title, "vim notes.md", "published with the next state");
        assert_eq!(s.window.unwrap().title, "Page web — Mozilla Firefox", "the bar's title unchanged");
        let e = Event::Title { address: "583d9c60da90".into(), title: "Other".into() };
        assert_eq!(m.apply(&e), Effect::Changed, "the active window's title: as before");
        assert_eq!(m.state().toplevels[2].title, "Other");
    }

    #[test]
    fn unreadable_json_named_error() {
        let r = Snapshot::parse(&Responses { monitors: "not json", workspaces: "[]", clients: "[]", active: "{}" });
        assert!(r.unwrap_err().contains("j/monitors"));
    }
}
