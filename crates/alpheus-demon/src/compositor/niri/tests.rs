use super::*;

#[test]
fn casts_follow_the_event_stream() {
    let mut m = Model::default();
    let casting = |m: &Model| m.state().casting;
    assert!(!casting(&m));
    let l = r#"{"CastsChanged":{"casts":[{"stream_id":5,"is_active":true,"kind":"PipeWire","target":{"Nothing":{}}}]}}"#;
    assert_eq!(m.apply_line(l), Ok(true));
    assert!(casting(&m));
    assert_eq!(m.state().cast_hold, Duration::ZERO);
    assert_eq!(m.apply_line(r#"{"CastStartedOrChanged":{"cast":{"stream_id":5,"is_active":false}}}"#), Ok(true));
    assert!(!casting(&m));
    assert_eq!(m.apply_line(r#"{"CastStartedOrChanged":{"cast":{"stream_id":6}}}"#), Ok(false), "no is_active: not active, no flip");
    assert_eq!(m.apply_line(r#"{"CastStartedOrChanged":{"cast":{"stream_id":6,"is_active":true}}}"#), Ok(true));
    assert!(casting(&m));
    assert_eq!(m.apply_line(r#"{"CastStopped":{"stream_id":6}}"#), Ok(true));
    assert!(!casting(&m));
    assert_eq!(m.apply_line(r#"{"CastStopped":{"stream_id":5}}"#), Ok(false), "no flip");
    assert!(m.casts.is_empty());
    // Malformed bodies change nothing.
    assert_eq!(m.apply_line(r#"{"CastStartedOrChanged":{"cast":7}}"#), Ok(false));
    assert_eq!(m.apply_line(r#"{"CastStartedOrChanged":{}}"#), Ok(false));
    assert_eq!(m.apply_line(r#"{"CastsChanged":{"casts":"x"}}"#), Ok(false));
    assert_eq!(m.apply_line(r#"{"CastStopped":{}}"#), Ok(false));
    assert!(m.casts.is_empty() && !casting(&m));
}

#[test]
fn the_cast_map_is_bounded() {
    let mut m = Model::default();
    for id in 0..100u64 {
        m.apply_line(&format!(r#"{{"CastStartedOrChanged":{{"cast":{{"stream_id":{id},"is_active":false}}}}}}"#)).unwrap();
    }
    assert_eq!(m.casts.len(), MAX_CASTS);
    let many: Vec<String> = (0..100).map(|id| format!(r#"{{"stream_id":{id},"is_active":false}}"#)).collect();
    m.apply_line(&format!(r#"{{"CastsChanged":{{"casts":[{}]}}}}"#, many.join(","))).unwrap();
    assert_eq!(m.casts.len(), MAX_CASTS);
    assert_eq!(m.apply_line(r#"{"CastStartedOrChanged":{"cast":{"stream_id":3,"is_active":true}}}"#), Ok(true), "a known id still updates");
    assert!(m.state().casting);
}

const STREAM: &str = include_str!("../../../tests/fixtures/niri-evenements.jsonl");

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
    assert_eq!(
        order(&m),
        ["id:3", "id:1"],
        "closed and unknown ids are not kept in `recent`; nothing flagged focused (99 does not \
             exist), so 1 (still the most recent) stays last"
    );
}

#[test]
fn focus_null_keeps_the_most_recent_window_last() {
    // A layer-shell surface (the launcher) grabbing exclusive focus
    // makes niri emit `WindowFocusChanged{id: null}`: the previously
    // focused window must stay last, not jump back to the front.
    let mut m = Model::default();
    let window =
        |id: u64, focused: bool| format!(r#"{{"id":{id},"title":"w{id}","app_id":"foot","workspace_id":1,"is_focused":{focused}}}"#);
    m.apply_line(&format!(r#"{{"WindowsChanged":{{"windows":[{},{}]}}}}"#, window(1, false), window(2, false))).unwrap();
    let order = |m: &Model| m.state().toplevels.iter().map(|t| t.selector.clone()).collect::<Vec<_>>();
    m.apply_line(r#"{"WindowFocusChanged":{"id":1}}"#).unwrap();
    m.apply_line(r#"{"WindowFocusChanged":{"id":2}}"#).unwrap();
    m.apply_line(r#"{"WindowFocusChanged":{"id":null}}"#).unwrap();
    assert_eq!(order(&m), ["id:1", "id:2"], "order still ends with 2");
    assert!(!m.state().toplevels.iter().any(|t| t.focused), "nothing flagged focused once focus is null");
}

#[test]
fn a_window_on_an_unknown_workspace_is_listed_without_one() {
    let mut m = Model::default();
    m.apply_line(r#"{"WindowsChanged":{"windows":[{"id":5,"title":"x","app_id":"foot","workspace_id":99,"is_focused":false}]}}"#).unwrap();
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
