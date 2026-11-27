//! Mango (0.18), pure parsing (no I/O here, see sources::mango): the snapshots of the two `watch`
//! connections, one JSON object per line, each a full state — `{"monitors":[…]}` and
//! `{"clients":[…]}`. Unknown fields are ignored and missing ones take their default (`appid` may
//! be null); `last_open_surface` (layer surfaces, the bar's own included) is never read. Mango
//! gives no focus history: the model keeps one (`mru`). The keyboard layout and bind mode
//! (`keyboardlayout`, `keymode`, per monitor) reach the state only with `Model::new(true)`.

use super::{CompositorState, MAX_NESTING, Toplevel, Window, Workspace, too_deep};
use crate::text::clean;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Bounds: what is past them is dropped, never an error.
pub const MAX_OUTPUTS: usize = 16;
pub const MAX_TAGS: u32 = 32;
pub const MAX_CLIENTS: usize = 512;

/// Which of the two connections a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watch {
    Monitors,
    Clients,
}

#[derive(Debug, Clone, Deserialize)]
struct Tag {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    is_urgent: bool,
    #[serde(default)]
    client_count: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ActiveClient {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    appid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Monitor {
    #[serde(default)]
    name: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    tag_num: Option<u32>,
    #[serde(default)]
    tags: Vec<Tag>,
    #[serde(default)]
    active_tags: Vec<i64>,
    #[serde(default)]
    active_client: Option<ActiveClient>,
    /// The bind mode ("default"); read only with `Model::keyboard`.
    #[serde(default)]
    keymode: Option<String>,
    /// The layout's name ("English (US)"); read only with `Model::keyboard`.
    #[serde(default)]
    keyboardlayout: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Client {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    appid: Option<String>,
    #[serde(default)]
    tags: Vec<u32>,
    #[serde(default)]
    is_focused: bool,
}

/// One line of either watch: its list, or Mango's refusal.
#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    monitors: Option<Vec<Monitor>>,
    #[serde(default)]
    clients: Option<Vec<Client>>,
}

#[derive(Debug, Default)]
pub struct Model {
    monitors: Option<Vec<Monitor>>,
    clients: Option<Vec<Client>>,
    /// The focused client of the last clients snapshot.
    focused: Option<u64>,
    /// Ids that had focus, oldest first, the most recent last; only ids of the last snapshot.
    mru: Vec<u64>,
    /// The keyboard is wanted: `state` carries the layout and the mode. Off, a layout switch
    /// changes no state: the compositor thread still reads and parses the snapshot, but it
    /// publishes nothing, so the main loop does not wake.
    keyboard: bool,
}

/// An optional label from Mango, cleaned and capped (`text::clean`); null or absent: empty.
fn label(s: &Option<String>) -> String {
    clean(s.as_deref().unwrap_or(""))
}

impl Model {
    /// A model that reads the keyboard layout and mode when `keyboard`; `Model::default()` does not.
    pub fn new(keyboard: bool) -> Model {
        Model { keyboard, ..Model::default() }
    }

    /// Takes one line of `watch`. Mango's refusal, a line that is not a JSON object, or one without
    /// the list its watch sends is an error: the source ends the session on it, and the
    /// model keeps what it had.
    pub fn apply(&mut self, watch: Watch, line: &str) -> Result<(), String> {
        // Refused before serde_json recurses into it: deep nesting would overflow the compositor
        // thread's stack in a debug build.
        if too_deep(line.as_bytes()) {
            return Err(format!("unreadable mango line: nested past {MAX_NESTING} levels"));
        }
        let l: Line = serde_json::from_str(line)
            .map_err(|e| format!("unreadable mango line: {:?} at line {} column {}", e.classify(), e.line(), e.column()))?;
        if let Some(e) = l.error {
            return Err(format!("mango refused: {}", clean(&e.to_string())));
        }
        match watch {
            Watch::Monitors => {
                let mut monitors = l.monitors.ok_or_else(|| "a mango monitors line without \"monitors\"".to_string())?;
                monitors.truncate(MAX_OUTPUTS);
                self.monitors = Some(monitors);
            }
            Watch::Clients => {
                let mut clients = l.clients.ok_or_else(|| "a mango clients line without \"clients\"".to_string())?;
                clients.truncate(MAX_CLIENTS);
                self.follow_focus(&clients);
                self.clients = Some(clients);
            }
        }
        Ok(())
    }

    /// A snapshot whose focused id changed moves that id to the end of `mru`; ids no longer
    /// present are dropped (so `mru` never outgrows the snapshot, at most MAX_CLIENTS).
    fn follow_focus(&mut self, clients: &[Client]) {
        let now = clients.iter().find(|c| c.is_focused).and_then(|c| c.id);
        if now != self.focused {
            if let Some(id) = now {
                self.mru.retain(|m| *m != id);
                self.mru.push(id);
            }
            self.focused = now;
        }
        let present: HashSet<u64> = clients.iter().filter_map(|c| c.id).collect();
        self.mru.retain(|id| present.contains(id));
    }

    /// Both first snapshots have arrived: nothing is published before (no half-read state).
    pub fn ready(&self) -> bool {
        self.monitors.is_some() && self.clients.is_some()
    }

    pub fn state(&self) -> CompositorState {
        let monitors = self.monitors.as_deref().unwrap_or_default();
        let mut workspaces = Vec::new();
        for m in monitors {
            let output = clean(&m.name);
            // Mango's overview: tag 0 active, or none.
            let overview = m.active_tags.is_empty() || m.active_tags.contains(&0);
            let last = m.tag_num.unwrap_or(MAX_TAGS).min(MAX_TAGS);
            let mut tags: Vec<&Tag> = m.tags.iter().filter(|t| (1..=last).contains(&t.index)).collect();
            tags.sort_by_key(|t| t.index);
            tags.dedup_by_key(|t| t.index);
            let lowest = tags.first().map(|t| t.index);
            let before = workspaces.len();
            for t in tags {
                let visible = t.is_active && !overview;
                // Waybar's hide-empty: Mango always exposes all its tags.
                if t.client_count == 0 && !visible && !t.is_urgent {
                    continue;
                }
                workspaces.push(Workspace {
                    name: t.index.to_string(),
                    reference: t.index.to_string(),
                    output: output.clone(),
                    windows: t.client_count.min(u32::from(u16::MAX)) as u16,
                    urgent: t.is_urgent,
                    visible,
                    focused: visible && m.active,
                });
            }
            // An output left with no tag (the overview on an empty screen) keeps its lowest one,
            // not visible and empty: otherwise its bar would fall back to every output's tags
            // (`workspaces_of`) and a click would act on another output.
            if let Some(index) = lowest.filter(|_| workspaces.len() == before) {
                workspaces.push(Workspace {
                    name: index.to_string(),
                    reference: index.to_string(),
                    output: output.clone(),
                    windows: 0,
                    urgent: false,
                    visible: false,
                    focused: false,
                });
            }
        }
        let window = monitors.iter().find(|m| m.active).and_then(|m| m.active_client.as_ref()).and_then(|c| {
            let (class, title) = (label(&c.appid), label(&c.title));
            (!class.is_empty() || !title.is_empty()).then_some(Window { class, title })
        });
        let clients = self.clients.as_deref().unwrap_or_default();
        let by_id: HashMap<u64, &Client> = clients.iter().filter_map(|c| Some((c.id?, c))).collect();
        let ranked: HashSet<u64> = self.mru.iter().copied().collect();
        let never = clients.iter().filter(|c| c.id.is_some_and(|id| !ranked.contains(&id)));
        // Hyprland's order: the previous window first, older ones next, never-focused windows
        // after them, the newest (focused, or most recent while nothing is) last.
        let recent: Vec<&Client> = self.mru.iter().filter_map(|id| by_id.get(id).copied()).collect();
        let (newest, older) = match recent.split_last() {
            Some((n, o)) => (Some(*n), o),
            None => (None, &[][..]),
        };
        let toplevels = older
            .iter()
            .rev()
            .copied()
            .chain(never)
            .chain(newest)
            .map(|c| Toplevel {
                selector: format!("id:{}", c.id.unwrap_or_default()),
                class: label(&c.appid),
                title: label(&c.title),
                workspace: c.tags.first().map(u32::to_string).unwrap_or_default(),
                focused: c.is_focused,
            })
            .collect();
        // Mango keeps a layout and a mode per monitor; the active one's is the keyboard's.
        let (layout, mode) = match monitors.iter().find(|m| m.active).or(monitors.first()).filter(|_| self.keyboard) {
            Some(m) => (named(&m.keyboardlayout), named(&m.keymode)),
            None => (None, None),
        };
        CompositorState { workspaces, window, toplevels, layout, mode, ..Default::default() }
    }
}

/// A name from Mango through `label`; absent, null or empty: None.
fn named(s: &Option<String>) -> Option<String> {
    Some(label(s)).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = include_str!("../../tests/fixtures/mango-all-monitors.json");
    const CLIENTS: &str = include_str!("../../tests/fixtures/mango-all-clients.json");
    const WATCH: &str = include_str!("../../tests/fixtures/mango-watch-monitors.jsonl");
    const OVERVIEW: &str = include_str!("../../tests/fixtures/mango-monitors-overview.json");
    const URGENT: &str = include_str!("../../tests/fixtures/mango-monitors-urgent.json");
    const MULTI: &str = include_str!("../../tests/fixtures/mango-monitors-multi.json");
    const NULL_MONITORS: &str = include_str!("../../tests/fixtures/mango-monitors-appid-null.json");
    const NULL_CLIENTS: &str = include_str!("../../tests/fixtures/mango-clients-appid-null.json");
    const SEVERAL_TAGS: &str = include_str!("../../tests/fixtures/mango-clients-several-tags.json");
    const ERROR: &str = include_str!("../../tests/fixtures/mango-error.json");
    const UNKNOWN: &str = include_str!("../../tests/fixtures/mango-unknown-fields.json");
    const LAYOUT_FR: &str = include_str!("../../tests/fixtures/mango-monitors-layout-fr.json");

    fn model(monitors: &str, clients: &str) -> Model {
        fed(Model::default(), monitors, clients)
    }

    fn fed(mut m: Model, monitors: &str, clients: &str) -> Model {
        m.apply(Watch::Monitors, monitors).unwrap();
        m.apply(Watch::Clients, clients).unwrap();
        m
    }

    /// (layout, mode) of the state of a keyboard model fed `monitors`.
    fn keyboard(monitors: &str) -> (Option<String>, Option<String>) {
        let e = fed(Model::new(true), monitors, CLIENTS).state();
        (e.layout, e.mode)
    }

    fn some(layout: &str, mode: &str) -> (Option<String>, Option<String>) {
        (Some(layout.into()), Some(mode.into()))
    }

    #[test]
    fn mango_the_monitors_carry_keymode_and_keyboardlayout() {
        assert_eq!(keyboard(MONITORS), some("English (US)", "default"));
    }

    /// The active monitor's, not the first's: Mango keeps one per monitor.
    #[test]
    fn mango_layout_comes_from_the_active_monitor() {
        assert_eq!(keyboard(LAYOUT_FR), some("French (AZERTY)", "resize"));
        let none_active = LAYOUT_FR.replace("\"name\": \"eDP-1\", \"active\": true", "\"name\": \"eDP-1\", \"active\": false");
        assert_ne!(none_active, LAYOUT_FR);
        assert_eq!(keyboard(&none_active), some("English (US)", "default"), "no active monitor: the first's");
    }

    /// Without the keyboard module nothing is published: a layout change alone changes no state.
    #[test]
    fn mango_without_keyboard_publishes_no_layout_nor_mode() {
        for monitors in [MONITORS, LAYOUT_FR] {
            let e = model(monitors, CLIENTS).state();
            assert_eq!((e.layout, e.mode), (None, None));
        }
    }

    #[test]
    fn mango_a_snapshot_without_the_fields_gives_none() {
        assert_eq!(keyboard(MULTI), (None, None));
        let mut m = Model::new(true);
        assert_eq!(m.apply(Watch::Monitors, UNKNOWN), Ok(()));
        m.apply(Watch::Clients, CLIENTS).unwrap();
        assert_eq!(m.state().layout, None, "keymode alone: no layout");
        assert_eq!(m.state().mode.as_deref(), Some("default"));
        let empty = MONITORS.replace("\"English (US)\"", "\"\"").replace("\"keymode\": \"default\"", "\"keymode\": null");
        assert_eq!(keyboard(&empty), (None, None), "an empty or null name: None");
    }

    #[test]
    fn mango_layout_is_cleaned_and_capped() {
        let name = format!("{}\\u0007{}", "x".repeat(10), "y".repeat(1990));
        let monitors = MONITORS.replace("\"keyboardlayout\": \"English (US)\"", &format!("\"keyboardlayout\": \"{name}\""));
        assert_ne!(monitors, MONITORS);
        let (layout, _) = keyboard(&monitors);
        let expected = clean(&format!("{}\u{7}{}", "x".repeat(10), "y".repeat(1990)));
        assert_eq!(expected.chars().count(), crate::text::MAX_TEXT);
        assert!(expected.starts_with("xxxxxxxxxxy"), "the control character removed: {expected}");
        assert_eq!(layout, Some(expected));
    }

    /// (name, output, windows, urgent, visible, focused) of each workspace.
    fn summary(e: &CompositorState) -> Vec<(String, String, u16, bool, bool, bool)> {
        e.workspaces.iter().map(|w| (w.name.clone(), w.output.clone(), w.windows, w.urgent, w.visible, w.focused)).collect()
    }

    fn w(name: &str, output: &str, n: u16, u: bool, v: bool, fo: bool) -> (String, String, u16, bool, bool, bool) {
        (name.into(), output.into(), n, u, v, fo)
    }

    /// (selector, class, title, workspace, focused) of each toplevel.
    fn toplevels(e: &CompositorState) -> Vec<(String, String, String, String, bool)> {
        e.toplevels.iter().map(|t| (t.selector.clone(), t.class.clone(), t.title.clone(), t.workspace.clone(), t.focused)).collect()
    }

    fn t(selector: &str, class: &str, title: &str, workspace: &str, focused: bool) -> (String, String, String, String, bool) {
        (selector.into(), class.into(), title.into(), workspace.into(), focused)
    }

    /// One workspace per (output, tag) in the monitors' order, an empty tag that is
    /// neither active nor urgent hidden, `reference` the index; on the real capture.
    #[test]
    fn tags_per_output_from_the_real_capture() {
        let e = model(MONITORS, CLIENTS).state();
        assert_eq!(summary(&e), [w("1", "HDMI-A-1", 2, false, true, true), w("3", "eDP-1", 2, false, true, false)]);
        assert_eq!(e.workspaces.iter().map(|w| w.reference.as_str()).collect::<Vec<_>>(), ["1", "3"]);
        assert_eq!(e.window, Some(Window { class: "Alacritty".into(), title: "Terminal 2".into() }));
        assert_eq!(e.focused_output(), Some("HDMI-A-1"));
        assert_eq!(model(WATCH.lines().next().unwrap(), CLIENTS).state(), e, "a watch line maps like the get reply");
    }

    #[test]
    fn several_active_tags_are_all_visible() {
        let e = model(MULTI, CLIENTS).state();
        assert_eq!(
            summary(&e),
            [w("1", "HDMI-A-1", 1, false, true, false), w("2", "HDMI-A-1", 0, false, true, false), w("3", "eDP-1", 1, false, true, true)],
            "both of HDMI-A-1's active tags visible (the empty one too); focused only on the active output"
        );
        assert_eq!(e.window.unwrap().title, "Editor");
    }

    #[test]
    fn urgent_tags_are_shown_and_empty_ones_hidden() {
        let e = model(URGENT, CLIENTS).state();
        assert_eq!(
            summary(&e),
            [w("1", "eDP-1", 1, false, true, true), w("2", "eDP-1", 1, true, false, false), w("3", "eDP-1", 0, true, false, false)],
            "tag 3 is empty but urgent: shown; tag 4 is empty, inactive, not urgent: hidden"
        );
    }

    /// `active_tags` [0] or empty is the overview: no tag visible, tag 0 never listed.
    #[test]
    fn the_overview_shows_no_tag_visible_and_never_tag_0() {
        let expected = [w("1", "HDMI-A-1", 2, false, false, false), w("2", "HDMI-A-1", 1, false, false, false)];
        let e = model(OVERVIEW, CLIENTS).state();
        assert_eq!(summary(&e), expected);
        assert_eq!(e.focused_output(), None);
        let empty = OVERVIEW.replace("\"active_tags\":[0]", "\"active_tags\":[]");
        assert_ne!(empty, OVERVIEW);
        assert_eq!(summary(&model(&empty, CLIENTS).state()), expected, "an empty active_tags is the overview too");
        let none = OVERVIEW.replace("\"active_tags\":[0],", "");
        assert_eq!(summary(&model(&none, CLIENTS).state()), expected, "absent: empty");
    }

    /// In the overview, an output with no window and
    /// nothing urgent keeps its lowest tag as a placeholder (not visible, empty), so its bar never
    /// falls back to the other output's tags (`workspaces_of`), where a click would act there.
    #[test]
    fn an_empty_output_in_the_overview_keeps_one_tag_of_its_own() {
        let first = OVERVIEW.trim().strip_prefix(r#"{"monitors":["#).and_then(|s| s.strip_suffix("]}")).unwrap();
        let tag = |i: u32| format!(r#"{{"index":{i},"is_active":{},"is_urgent":false,"client_count":0}}"#, i == 2);
        let tags = (0..=3).map(tag).collect::<Vec<_>>().join(",");
        let second = format!(r#"{{"name":"eDP-1","active":false,"tag_num":3,"tags":[{tags}],"active_tags":[0],"active_client":null}}"#);
        let e = model(&format!(r#"{{"monitors":[{first},{second}]}}"#), CLIENTS).state();
        assert_eq!(
            summary(&e),
            [
                w("1", "HDMI-A-1", 2, false, false, false),
                w("2", "HDMI-A-1", 1, false, false, false),
                w("1", "eDP-1", 0, false, false, false)
            ]
        );
        let names = |o: &str| e.workspaces_of(o).iter().map(|w| (w.name.clone(), w.output.clone())).collect::<Vec<_>>();
        assert_eq!(names("eDP-1"), [("1".to_string(), "eDP-1".to_string())], "its own tag only");
        assert_eq!(names("HDMI-A-1").len(), 2);
    }

    /// Several active tags on the focused output are all visible and all focused.
    #[test]
    fn several_active_tags_on_the_focused_output_are_all_focused() {
        let focused = MULTI
            .replace("\"name\":\"HDMI-A-1\",\"active\":false", "\"name\":\"HDMI-A-1\",\"active\":true")
            .replace("\"name\":\"eDP-1\",\"active\":true", "\"name\":\"eDP-1\",\"active\":false");
        assert_ne!(focused, MULTI);
        let e = model(&focused, CLIENTS).state();
        assert_eq!(
            summary(&e),
            [w("1", "HDMI-A-1", 1, false, true, true), w("2", "HDMI-A-1", 0, false, true, true), w("3", "eDP-1", 1, false, true, false)]
        );
        assert_eq!(e.focused_output(), Some("HDMI-A-1"));
        assert_eq!(e.window.unwrap().title, "Notes");
    }

    /// The window is the active client of the output with `active: true`, or none.
    #[test]
    fn the_title_is_the_focused_outputs_active_client() {
        let swapped = MONITORS
            .replace("\"name\": \"HDMI-A-1\", \"active\": true", "\"name\": \"HDMI-A-1\", \"active\": false")
            .replace("\"name\": \"eDP-1\", \"active\": false", "\"name\": \"eDP-1\", \"active\": true");
        assert_ne!(swapped, MONITORS);
        let e = model(&swapped, CLIENTS).state();
        assert_eq!(e.window, Some(Window { class: "Alacritty".into(), title: "Terminal 1".into() }));
        assert_eq!(e.focused_output(), Some("eDP-1"));
        let none = MONITORS.replace("\"name\": \"HDMI-A-1\", \"active\": true", "\"name\": \"HDMI-A-1\", \"active\": false");
        assert_eq!(model(&none, CLIENTS).state().window, None, "no active output: no window");
    }

    /// Clients `ids` (all on tag 1), the one `focused` flagged.
    fn clients(ids: &[u64], focused: Option<u64>) -> String {
        let one = |id: &u64| format!(r#"{{"id":{id},"title":"w{id}","appid":"foot","tags":[1],"is_focused":{}}}"#, Some(*id) == focused);
        format!(r#"{{"clients":[{}]}}"#, ids.iter().map(one).collect::<Vec<_>>().join(","))
    }

    /// Mango gives no focus history; the model keeps one and orders as
    /// Hyprland does: the previous window first, older ones next, never-focused windows after
    /// them in the snapshot's order, the focused one last. While nothing is focused (the launcher holds the keyboard), the most recent stays last.
    #[test]
    fn mru_order_never_focused_first_focused_last() {
        let mut m = Model::default();
        m.apply(Watch::Monitors, MONITORS).unwrap();
        let order = |m: &Model| m.state().toplevels.iter().map(|t| t.selector.clone()).collect::<Vec<_>>();
        m.apply(Watch::Clients, &clients(&[1, 2, 3], Some(2))).unwrap();
        assert_eq!(order(&m), ["id:1", "id:3", "id:2"]);
        m.apply(Watch::Clients, &clients(&[1, 2, 3], Some(3))).unwrap();
        assert_eq!(order(&m), ["id:2", "id:1", "id:3"], "2, the previous one, first (alt-tab)");
        m.apply(Watch::Clients, &clients(&[1, 2, 3], Some(1))).unwrap();
        assert_eq!(order(&m), ["id:3", "id:2", "id:1"]);
        m.apply(Watch::Clients, &clients(&[1, 2, 3], Some(1))).unwrap();
        assert_eq!(order(&m), ["id:3", "id:2", "id:1"], "the same focus again changes nothing");
        m.apply(Watch::Clients, &clients(&[1, 3], Some(1))).unwrap();
        assert_eq!(order(&m), ["id:3", "id:1"], "2 closed: dropped from the history");
        assert_eq!(m.mru, [3, 1], "the history itself no longer holds 2");
        m.apply(Watch::Clients, &clients(&[1, 3], None)).unwrap();
        assert_eq!(order(&m), ["id:3", "id:1"], "nothing focused: 1, the most recent, stays last");
        assert!(!m.state().toplevels.iter().any(|t| t.focused));
        m.apply(Watch::Clients, &clients(&[1, 3, 4], Some(3))).unwrap();
        assert_eq!(order(&m), ["id:1", "id:4", "id:3"], "1 previous first, 4 never focused next");
        assert_eq!(
            toplevels(&model(MONITORS, CLIENTS).state()),
            [
                t("id:28", "Alacritty", "Terminal 1", "3", false),
                t("id:24", "Alacritty", "Terminal 3", "1", false),
                t("id:2", "Alacritty", "Terminal 4", "3", false),
                t("id:26", "Alacritty", "Terminal 2", "1", true),
            ],
            "the real capture: 26 focused last, the others never focused, in the snapshot's order"
        );
    }

    #[test]
    fn appid_null_gives_an_empty_class() {
        let e = model(NULL_MONITORS, NULL_CLIENTS).state();
        assert_eq!(e.window, Some(Window { class: String::new(), title: "Untitled".into() }));
        assert_eq!(toplevels(&e), [t("id:7", "", "Untitled", "2", true)]);
    }

    #[test]
    fn a_client_on_several_tags_is_listed_under_its_first() {
        assert_eq!(
            toplevels(&model(MONITORS, SEVERAL_TAGS).state()),
            [
                t("id:9", "org.example.Notes", "Sticky note", "4", false),
                t("id:10", "org.example.Clock", "Everywhere", "", false),
                t("id:11", "org.example.Plain", "Plain", "1", false),
            ],
            "the first of `tags` as given; none: empty; a client without an id skipped"
        );
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let e = model(UNKNOWN, r#"{"clients":[{"id":3,"title":"Editor","appid":"org.example.Editor","tags":[1],"is_focused":true,"brand_new":{"x":[1]}}],"version":"0.19.0"}"#).state();
        assert_eq!(summary(&e), [w("1", "HDMI-A-1", 1, false, true, true)]);
        assert_eq!(e.window, Some(Window { class: "org.example.Editor".into(), title: "Editor".into() }));
        assert_eq!(toplevels(&e), [t("id:3", "org.example.Editor", "Editor", "1", true)]);
    }

    /// An `{"error":…}` reply or a line that is not JSON is an error (the source ends
    /// the session on it); the model keeps what it had.
    #[test]
    fn error_replies_and_unreadable_lines_are_errors() {
        let mut m = model(MONITORS, CLIENTS);
        let before = m.state();
        for watch in [Watch::Monitors, Watch::Clients] {
            let e = m.apply(watch, ERROR).unwrap_err();
            assert!(e.contains("mango refused") && e.contains("unknown command"), "{e}");
            assert!(m.apply(watch, "not json").unwrap_err().contains("unreadable mango line"));
            let secret = m
                .apply(watch, r#"{"clients":[{"title":"x","tags":"hunter2"}],"monitors":[{"name":"hunter2","tags":"hunter2"}]}"#)
                .unwrap_err();
            assert!(secret.contains("unreadable mango line") && !secret.contains("hunter2"), "no line content in the error: {secret}");
            assert!(m.apply(watch, "42").unwrap_err().contains("unreadable mango line"), "not an object");
        }
        assert!(m.apply(Watch::Monitors, CLIENTS).unwrap_err().contains("without \"monitors\""));
        assert!(m.apply(Watch::Clients, MONITORS).unwrap_err().contains("without \"clients\""));
        assert_eq!(m.state(), before, "nothing kept from a refused line");
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(10_000));
        assert!(m.apply(Watch::Monitors, &long).unwrap_err().len() < 400, "the refusal's text is capped");
    }

    /// A line nested past MAX_NESTING is unreadable, refused before serde_json recurses into it (a
    /// debug daemon would overflow the compositor thread's 256 KiB stack); one at the bound is read.
    #[test]
    fn a_line_nested_past_max_nesting_is_refused_before_parsing() {
        let nested = |n: usize| format!(r#"{{"clients":[],"x":{}0{}}}"#, "[".repeat(n - 1), "]".repeat(n - 1));
        let h = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut m = Model::default();
                let at = m.apply(Watch::Clients, &nested(MAX_NESTING));
                let past = m.apply(Watch::Clients, &nested(MAX_NESTING + 1));
                let far = m.apply(Watch::Monitors, &nested(1000).replace("clients", "monitors"));
                (at, past, far)
            })
            .unwrap();
        let (at, past, far) = h.join().expect("no stack overflow on a 256 KiB stack");
        assert_eq!(at, Ok(()), "read at MAX_NESTING");
        assert_eq!(past.unwrap_err(), "unreadable mango line: nested past 90 levels");
        assert_eq!(far.unwrap_err(), "unreadable mango line: nested past 90 levels");
    }

    #[test]
    fn ready_only_once_both_snapshots_arrived() {
        let mut m = Model::default();
        assert!(!m.ready());
        m.apply(Watch::Monitors, MONITORS).unwrap();
        assert!(!m.ready(), "monitors alone");
        m.apply(Watch::Clients, CLIENTS).unwrap();
        assert!(m.ready());
    }

    /// Bounds: 16 outputs, 32 tags per output, 512 clients; labels cleaned and capped.
    #[test]
    fn bounds_and_labels() {
        let tag = |i: u32| format!(r#"{{"index":{i},"is_active":{},"is_urgent":false,"client_count":1}}"#, i == 1);
        let tags = (1..=40).map(tag).collect::<Vec<_>>().join(",");
        let monitor = |n: u32| {
            format!(
                r#"{{"name":"OUT-{n}","active":{},"tag_num":40,"tags":[{tags}],"active_tags":[1],"active_client":{{"id":1,"title":"t","appid":"a"}}}}"#,
                n == 0
            )
        };
        let monitors = format!(r#"{{"monitors":[{}]}}"#, (0..20).map(monitor).collect::<Vec<_>>().join(","));
        let many: Vec<u64> = (1..=600).collect();
        let e = model(&monitors, &clients(&many, None)).state();
        assert_eq!(e.workspaces.len(), MAX_OUTPUTS * MAX_TAGS as usize, "16 outputs × 32 tags");
        assert_eq!(e.workspaces.iter().map(|w| w.output.as_str()).collect::<std::collections::BTreeSet<_>>().len(), 16);
        assert!(e.workspaces.iter().all(|w| w.reference.parse::<u32>().is_ok_and(|i| (1..=32).contains(&i))));
        assert_eq!(e.toplevels.len(), MAX_CLIENTS);
        let title = format!("{}\u{202e}{}", "x".repeat(100), "y".repeat(900));
        let line = format!(r#"{{"clients":[{{"id":1,"title":"{title}","appid":"a\u0007b","tags":[1],"is_focused":true}}]}}"#);
        let e = model(MONITORS, &line).state();
        assert_eq!(e.toplevels[0].title.chars().count(), crate::text::MAX_TEXT);
        assert!(!e.toplevels[0].title.contains('\u{202e}'));
        assert_eq!(e.toplevels[0].class, "ab", "a control character removed");
        let two_tags = MONITORS.replace("\"tag_num\": 10", "\"tag_num\": 2");
        assert_eq!(
            summary(&model(&two_tags, CLIENTS).state()),
            [w("1", "HDMI-A-1", 2, false, true, true), w("1", "eDP-1", 0, false, false, false)],
            "tag 3 is past eDP-1's tag_num: eDP-1 keeps only its placeholder"
        );
    }
}
