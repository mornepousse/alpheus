//! niri, pure parsing (no I/O here, see sources::niri): the event stream
//! from `$NIRI_SOCKET` after the `"EventStream"` request, one JSON line per
//! event, in the niri-ipc 26.04 format. The stream starts with the full
//! state (WorkspacesChanged, WindowsChanged): nothing else to read.
//! Unknown variants are ignored: niri adds more over its versions, and its
//! types are not stable from one version to another (hence a hand-rolled
//! parser rather than the niri-ipc crate).

use super::{CompositorState, Toplevel, Window, Workspace};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceN {
    id: u64,
    idx: u8,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    is_urgent: bool,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    is_focused: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct WindowN {
    id: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<u64>,
    #[serde(default)]
    is_focused: bool,
    #[serde(default)]
    is_urgent: bool,
}

#[derive(Debug, Default)]
pub struct Model {
    workspaces: BTreeMap<u64, WorkspaceN>,
    windows: BTreeMap<u64, WindowN>,
    /// Ids of the windows that had focus, most recent first (the launcher
    /// lists the previous window first); only ids still in `windows`.
    recent: Vec<u64>,
}

fn field_u64(body: &Value, key: &str) -> Result<u64, String> {
    body.get(key).and_then(Value::as_u64).ok_or_else(|| format!("field \"{key}\" missing"))
}

fn field_bool(body: &Value, key: &str) -> Result<bool, String> {
    body.get(key).and_then(Value::as_bool).ok_or_else(|| format!("field \"{key}\" missing"))
}

fn read<T: serde::de::DeserializeOwned>(name: &str, v: Option<&Value>) -> Result<T, String> {
    serde_json::from_value(v.cloned().unwrap_or(Value::Null)).map_err(|e| format!("unreadable {name}: {e}"))
}

impl Model {
    pub fn apply_line(&mut self, line: &str) -> Result<bool, String> {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("unreadable niri line: {e}"))?;
        let Some((name, body)) = v.as_object().and_then(|o| o.iter().next()) else { return Ok(false) };
        match name.as_str() {
            "Ok" => Ok(false),
            "Err" => Err(format!("niri refused: {body}")),
            "WorkspacesChanged" => {
                let l: Vec<WorkspaceN> = read(name, body.get("workspaces"))?;
                self.workspaces = l.into_iter().map(|w| (w.id, w)).collect();
                Ok(true)
            }
            "WorkspaceUrgencyChanged" => {
                let (id, urgent) = (field_u64(body, "id")?, field_bool(body, "urgent")?);
                if let Some(w) = self.workspaces.get_mut(&id) {
                    w.is_urgent = urgent;
                }
                Ok(true)
            }
            "WorkspaceActivated" => {
                let (id, focused) = (field_u64(body, "id")?, field_bool(body, "focused")?);
                let output = self.workspaces.get(&id).and_then(|w| w.output.clone());
                for w in self.workspaces.values_mut() {
                    if w.output == output {
                        w.is_active = w.id == id;
                    }
                    if focused {
                        w.is_focused = w.id == id;
                    }
                }
                Ok(true)
            }
            "WindowsChanged" => {
                let l: Vec<WindowN> = read(name, body.get("windows"))?;
                self.windows = l.into_iter().map(|w| (w.id, w)).collect();
                self.recent.retain(|id| self.windows.contains_key(id));
                if let Some(id) = self.windows.values().find(|w| w.is_focused).map(|w| w.id) {
                    self.focus(id);
                }
                Ok(true)
            }
            "WindowOpenedOrChanged" => {
                let w: WindowN = read(name, body.get("window"))?;
                if w.is_focused {
                    for g in self.windows.values_mut() {
                        g.is_focused = false;
                    }
                }
                let (id, focused) = (w.id, w.is_focused);
                self.windows.insert(id, w);
                if focused {
                    self.focus(id);
                }
                Ok(true)
            }
            "WindowClosed" => {
                let id = field_u64(body, "id")?;
                self.windows.remove(&id);
                self.recent.retain(|r| *r != id);
                Ok(true)
            }
            "WindowFocusChanged" => {
                let id = body.get("id").and_then(Value::as_u64); // null: no more focus
                for w in self.windows.values_mut() {
                    w.is_focused = Some(w.id) == id;
                }
                if let Some(id) = id {
                    self.focus(id);
                }
                Ok(true)
            }
            "WindowUrgencyChanged" => {
                let (id, urgent) = (field_u64(body, "id")?, field_bool(body, "urgent")?);
                if let Some(w) = self.windows.get_mut(&id) {
                    w.is_urgent = urgent;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// `id` now has focus: first of `recent` (an unknown id is not kept).
    fn focus(&mut self, id: u64) {
        self.recent.retain(|r| *r != id);
        if self.windows.contains_key(&id) {
            self.recent.insert(0, id);
        }
    }

    pub fn state(&self) -> CompositorState {
        // A workspace with no output (screen unplugged, niri keeps it) is not shown.
        let mut ws: Vec<&WorkspaceN> = self.workspaces.values().filter(|w| w.output.is_some()).collect();
        // Outputs sorted case-insensitively ("eDP-1" before "HDMI-A-1").
        ws.sort_by_key(|w| (w.output.as_deref().map(str::to_lowercase), w.idx));
        let workspaces = ws
            .into_iter()
            .map(|w| {
                let its_own = || self.windows.values().filter(move |win| win.workspace_id == Some(w.id));
                Workspace {
                    name: w.name.clone().unwrap_or_else(|| w.idx.to_string()),
                    reference: w.idx.to_string(),
                    output: w.output.clone().unwrap_or_default(),
                    windows: its_own().count().min(u16::MAX as usize) as u16,
                    urgent: w.is_urgent || its_own().any(|win| win.is_urgent),
                    visible: w.is_active,
                    focused: w.is_focused,
                }
            })
            .collect();
        let window = self
            .windows
            .values()
            .find(|w| w.is_focused)
            .map(|w| Window { class: w.app_id.clone().unwrap_or_default(), title: w.title.clone().unwrap_or_default() });
        // The launcher's window list: most recently focused first (never
        // focused ones after, by id), then the focused one moved last.
        let workspace_name = |id: Option<u64>| {
            id.and_then(|id| self.workspaces.get(&id)).map(|w| w.name.clone().unwrap_or_else(|| w.idx.to_string())).unwrap_or_default()
        };
        let rank = |id: u64| self.recent.iter().position(|r| *r == id).unwrap_or(usize::MAX);
        let mut listed: Vec<&WindowN> = self.windows.values().collect();
        listed.sort_by_key(|w| rank(w.id)); // stable: by id within "never focused"
        let mut toplevels: Vec<Toplevel> = listed
            .into_iter()
            .map(|w| Toplevel {
                selector: format!("id:{}", w.id),
                class: w.app_id.clone().unwrap_or_default(),
                title: w.title.clone().unwrap_or_default(),
                workspace: workspace_name(w.workspace_id),
                focused: w.is_focused,
            })
            .collect();
        if let Some(i) = toplevels.iter().position(|t| t.focused) {
            let focused = toplevels.remove(i);
            toplevels.push(focused);
        }
        CompositorState { workspaces, window, toplevels }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = include_str!("../../tests/fixtures/niri-evenements.jsonl");

    fn after(n: usize) -> CompositorState {
        let mut m = Model::default();
        for l in STREAM.lines().take(n) {
            m.apply_line(l).unwrap();
        }
        m.state()
    }

    /// (name, output, windows, urgent, visible, focused) of each workspace.
    fn summary(e: &CompositorState) -> Vec<(String, String, u16, bool, bool, bool)> {
        e.workspaces.iter().map(|w| (w.name.clone(), w.output.clone(), w.windows, w.urgent, w.visible, w.focused)).collect()
    }

    fn w(name: &str, output: &str, n: u16, u: bool, v: bool, fo: bool) -> (String, String, u16, bool, bool, bool) {
        (name.into(), output.into(), n, u, v, fo)
    }

    #[test]
    fn initial_state_from_the_stream() {
        let e = after(6);
        assert_eq!(
            summary(&e),
            [
                w("1", "eDP-1", 1, false, true, true),
                w("2", "eDP-1", 0, false, false, false),
                w("web", "HDMI-A-1", 2, false, true, false),
                w("2", "HDMI-A-1", 0, false, false, false),
            ]
        );
        assert_eq!(e.workspaces.iter().map(|w| w.reference.as_str()).collect::<Vec<_>>(), ["1", "2", "1", "2"]);
        assert_eq!(e.window, Some(Window { class: "Alacritty".into(), title: "~/projets — zsh".into() }));
    }

    #[test]
    fn urgency_focus_opening_closing() {
        assert!(after(7).workspaces[2].urgent, "window 12 urgent → workspace web urgent");
        let e = after(8);
        assert!(e.workspaces[2].focused && e.workspaces[0].visible && !e.workspaces[0].focused);
        assert_eq!(after(9).window.unwrap().title, "Page web — Mozilla Firefox");
        assert!(!after(10).workspaces[2].urgent);
        let e = after(11);
        assert_eq!(e.window.unwrap().class, "com.mitchellh.ghostty");
        assert_eq!(e.workspaces[3].windows, 1);
        let e = after(12);
        assert!(e.workspaces[3].visible && e.workspaces[3].focused);
        assert!(!e.workspaces[2].visible, "only one active workspace per output");
        assert!(e.workspaces[0].visible, "the other output keeps its own");
        assert_eq!(after(13).workspaces[2].windows, 1);
    }

    /// (selector, class, title, workspace, focused) of each toplevel.
    fn toplevels(e: &CompositorState) -> Vec<(String, String, String, String, bool)> {
        e.toplevels.iter().map(|t| (t.selector.clone(), t.class.clone(), t.title.clone(), t.workspace.clone(), t.focused)).collect()
    }

    fn t(selector: &str, class: &str, title: &str, workspace: &str, focused: bool) -> (String, String, String, String, bool) {
        (selector.into(), class.into(), title.into(), workspace.into(), focused)
    }

    #[test]
    fn toplevels_most_recent_first_focused_last() {
        assert_eq!(
            toplevels(&after(3)),
            [
                t("id:12", "firefox", "Page web — Mozilla Firefox", "web", false),
                t("id:13", "firefox", "Autre page — Mozilla Firefox", "web", false),
                t("id:11", "Alacritty", "~/projets — zsh", "1", true),
            ],
            "workspace by name, else its index"
        );
        let order = |n: usize| after(n).toplevels.iter().map(|t| t.selector.clone()).collect::<Vec<_>>();
        assert_eq!(order(9), ["id:11", "id:13", "id:12"], "focus moved to 12");
        assert_eq!(order(11), ["id:12", "id:11", "id:13", "id:14"], "a new focused window: 12, focused before it, first");
        assert_eq!(order(13), ["id:12", "id:11", "id:14"], "13 closed");
        assert_eq!(after(11).toplevels[3].workspace, "2", "unnamed workspace: its idx");
    }

    #[test]
    fn two_focus_changes_list_the_previous_window_first() {
        let mut m = Model::default();
        let window =
            |id: u64, focused: bool| format!(r#"{{"id":{id},"title":"w{id}","app_id":"foot","workspace_id":1,"is_focused":{focused}}}"#);
        m.apply_line(&format!(r#"{{"WindowsChanged":{{"windows":[{},{},{}]}}}}"#, window(1, true), window(2, false), window(3, false)))
            .unwrap();
        let order = |m: &Model| m.state().toplevels.iter().map(|t| t.selector.clone()).collect::<Vec<_>>();
        assert_eq!(order(&m), ["id:2", "id:3", "id:1"], "never focused: by id; the focused one last");
        m.apply_line(r#"{"WindowFocusChanged":{"id":3}}"#).unwrap();
        m.apply_line(r#"{"WindowFocusChanged":{"id":2}}"#).unwrap();
        assert_eq!(order(&m), ["id:3", "id:1", "id:2"], "3 had focus just before 2: first (alt-tab), not by id");
        m.apply_line(&format!(r#"{{"WindowOpenedOrChanged":{{"window":{}}}}}"#, window(1, true))).unwrap();
        assert_eq!(order(&m), ["id:2", "id:3", "id:1"], "focus through WindowOpenedOrChanged too");
        m.apply_line(r#"{"WindowClosed":{"id":2}}"#).unwrap();
        m.apply_line(r#"{"WindowFocusChanged":{"id":99}}"#).unwrap();
        assert_eq!(order(&m), ["id:1", "id:3"], "closed and unknown ids are not kept");
    }

    #[test]
    fn a_window_on_an_unknown_workspace_is_listed_without_one() {
        let mut m = Model::default();
        m.apply_line(r#"{"WindowsChanged":{"windows":[{"id":5,"title":"x","app_id":"foot","workspace_id":99,"is_focused":false}]}}"#)
            .unwrap();
        assert_eq!(toplevels(&m.state()), [t("id:5", "foot", "x", "", false)]);
    }

    #[test]
    fn lines_without_effect_or_refused() {
        let mut m = Model::default();
        assert_eq!(m.apply_line(r#"{"Ok":"Handled"}"#), Ok(false));
        assert_eq!(m.apply_line(r#"{"NewVariant":{"x":1}}"#), Ok(false));
        assert_eq!(m.apply_line(STREAM.lines().nth(13).unwrap()), Ok(false), "WindowLayoutsChanged ignored");
        assert!(m.apply_line(r#"{"Err":"no stream"}"#).unwrap_err().contains("niri refused"));
        assert!(m.apply_line("not json").unwrap_err().contains("unreadable"));
    }
}
