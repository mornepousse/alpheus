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
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// Casts kept; a new id past it is ignored.
const MAX_CASTS: usize = 64;

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
    /// `stream_id → is_active` of the casts niri reports; casting is any active one.
    casts: HashMap<u64, bool>,
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
    // a malformed event and an unknown one both change nothing, each said where it is.
    #[allow(clippy::match_same_arms)]
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
            "CastsChanged" => {
                let Some(list) = body.get("casts").and_then(Value::as_array) else { return Ok(false) };
                let before = self.casting();
                self.casts.clear();
                for c in list {
                    let Some(id) = c.get("stream_id").and_then(Value::as_u64) else { continue };
                    if self.casts.len() < MAX_CASTS {
                        self.casts.insert(id, c.get("is_active").and_then(Value::as_bool).unwrap_or(false));
                    }
                }
                Ok(self.casting() != before)
            }
            "CastStartedOrChanged" => {
                let Some(cast) = body.get("cast") else { return Ok(false) };
                let Some(id) = cast.get("stream_id").and_then(Value::as_u64) else { return Ok(false) };
                let active = cast.get("is_active").and_then(Value::as_bool).unwrap_or(false);
                let before = self.casting();
                if let Some(a) = self.casts.get_mut(&id) {
                    *a = active;
                } else if self.casts.len() < MAX_CASTS {
                    self.casts.insert(id, active);
                }
                Ok(self.casting() != before)
            }
            "CastStopped" => {
                let Some(id) = body.get("stream_id").and_then(Value::as_u64) else { return Ok(false) };
                let before = self.casting();
                self.casts.remove(&id);
                Ok(self.casting() != before)
            }
            _ => Ok(false),
        }
    }

    fn casting(&self) -> bool {
        self.casts.values().any(|a| *a)
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
        } else if !self.recent.is_empty() {
            // niri reports `WindowFocusChanged{id: null}` when a
            // layer-shell surface (the launcher) grabs exclusive focus:
            // `recent[0]` is still the most recently used window (and
            // `toplevels[0]` after the rank sort above), so keep it last,
            // as if it were still "focused".
            let most_recent = toplevels.remove(0);
            toplevels.push(most_recent);
        }
        CompositorState { workspaces, window, toplevels, casting: self.casting(), cast_hold: Duration::ZERO, ..Default::default() }
    }
}

#[cfg(test)]
mod tests;
