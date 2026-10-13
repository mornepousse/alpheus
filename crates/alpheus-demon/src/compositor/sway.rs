//! Sway (1.12), pure parsing (no I/O here, see sources::sway): the replies to GET_WORKSPACES and
//! GET_TREE, both read again after each burst of events. Workspaces in GET_WORKSPACES' order; the
//! window counts, the focused window and the launcher's list from the tree, walked without
//! recursion (bounded depth) along each container's `focus` array, most recent first. The
//! scratchpad (`__i3` output, `__i3_scratch` workspace) is skipped.

use super::{CompositorState, MAX_NESTING, Toplevel, Window, Workspace, too_deep};
use crate::text::clean;
use serde_json::Value;
use std::collections::HashMap;

/// Spec § 3 bounds: what is past them is dropped, never an error.
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_WINDOWS: usize = 512;
pub const MAX_DEPTH: usize = 64;
const SCRATCHPAD: &str = "__i3_scratch";
const SCRATCH_OUTPUT: &str = "__i3";

/// Lossy: a badly encoded title must not make the whole state unreadable. A reply nested past
/// MAX_NESTING is unreadable, refused before serde_json recurses into it. The error names
/// serde's class and place only, never its text (which may quote a title: no titles in the log).
fn parse(what: &str, bytes: &[u8]) -> Result<Value, String> {
    if too_deep(bytes) {
        return Err(format!("unreadable sway {what}: nested past {MAX_NESTING} levels"));
    }
    serde_json::from_str(&String::from_utf8_lossy(bytes))
        .map_err(|e| format!("unreadable sway {what}: {:?} at line {} column {}", e.classify(), e.line(), e.column()))
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// app_id (Wayland), else the X11 class (Xwayland), cleaned.
fn class_of(node: &Value) -> String {
    let id = text(node, "app_id").or_else(|| node.get("window_properties").and_then(|p| text(p, "class")));
    clean(id.unwrap_or(""))
}

/// `nodes` then `floating_nodes`, ordered by the node's `focus` array (ids, most recent first);
/// children it does not name follow, in tree order (a stable sort).
fn by_focus(node: &Value) -> Vec<&Value> {
    let mut children: Vec<&Value> =
        ["nodes", "floating_nodes"].iter().filter_map(|k| node.get(*k).and_then(Value::as_array)).flatten().collect();
    let focus: Vec<u64> =
        node.get("focus").and_then(Value::as_array).map(|f| f.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    children.sort_by_key(|c| c.get("id").and_then(Value::as_u64).and_then(|id| focus.iter().position(|f| *f == id)).unwrap_or(usize::MAX));
    children
}

/// A window of the tree: a node with a `pid`.
struct Found {
    id: u64,
    class: String,
    title: String,
    /// The enclosing workspace's raw name (the counts' key).
    workspace: String,
    focused: bool,
}

/// The tree's windows, most recently focused first: never the scratchpad's, at most MAX_WINDOWS,
/// nothing below MAX_DEPTH. An explicit stack, never recursion (the compositor thread's stack is
/// 256 KiB).
fn windows(tree: &Value) -> Vec<Found> {
    let mut out = Vec::new();
    let mut stack: Vec<(&Value, usize, String)> = vec![(tree, 0, String::new())];
    while let Some((node, depth, workspace)) = stack.pop() {
        if out.len() >= MAX_WINDOWS {
            break;
        }
        let kind = text(node, "type").unwrap_or("");
        let name = text(node, "name").unwrap_or("");
        if (kind == "output" && name == SCRATCH_OUTPUT) || (kind == "workspace" && name == SCRATCHPAD) {
            continue;
        }
        if node.get("pid").is_some_and(|p| !p.is_null()) {
            out.push(Found {
                id: node.get("id").and_then(Value::as_u64).unwrap_or(0),
                class: class_of(node),
                title: clean(name),
                workspace,
                focused: flag(node, "focused"),
            });
            continue;
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let workspace = if kind == "workspace" { name.to_string() } else { workspace };
        // Reversed onto the stack: the most recently focused child comes off first.
        for child in by_focus(node).into_iter().rev() {
            stack.push((child, depth + 1, workspace.clone()));
        }
    }
    out
}

pub fn state(workspaces: &[u8], tree: &[u8]) -> Result<CompositorState, String> {
    let list = parse("workspaces", workspaces)?;
    let list = list.as_array().ok_or_else(|| "the sway workspaces reply is not a list".to_string())?;
    let found = windows(&parse("tree", tree)?);
    let mut counts: HashMap<&str, u16> = HashMap::new();
    for w in &found {
        let n = counts.entry(w.workspace.as_str()).or_default();
        *n = n.saturating_add(1);
    }
    let workspaces = list
        .iter()
        .filter(|w| text(w, "name") != Some(SCRATCHPAD))
        .take(MAX_WORKSPACES)
        .map(|w| {
            let name = text(w, "name").unwrap_or("");
            let num = w.get("num").and_then(Value::as_i64).unwrap_or(-1);
            Workspace {
                name: clean(name),
                // `wm-set workspace` takes both under Sway; the raw name, so that it finds it.
                reference: if num >= 0 { num.to_string() } else { format!("name:{name}") },
                output: clean(text(w, "output").unwrap_or("")),
                windows: counts.get(name).copied().unwrap_or(0),
                urgent: flag(w, "urgent"),
                visible: flag(w, "visible"),
                focused: flag(w, "focused"),
            }
        })
        .collect();
    let window = found.iter().find(|w| w.focused).map(|w| Window { class: w.class.clone(), title: w.title.clone() });
    // Most recent first, the first moved to the end: Hyprland's order (previous first, the
    // focused window, or the most recent while nothing is focused, last).
    let toplevels = found
        .iter()
        .skip(1)
        .chain(found.first())
        .map(|w| Toplevel {
            selector: format!("id:{}", w.id),
            class: w.class.clone(),
            title: w.title.clone(),
            workspace: clean(&w.workspace),
            focused: w.focused,
        })
        .collect();
    Ok(CompositorState { workspaces, window, toplevels })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACES: &[u8] = include_bytes!("../../tests/fixtures/sway-workspaces.json");
    const TREE: &[u8] = include_bytes!("../../tests/fixtures/sway-tree.json");

    /// (name, reference, output, windows, urgent, visible, focused) of each workspace.
    fn summary(e: &CompositorState) -> Vec<(String, String, String, u16, bool, bool, bool)> {
        e.workspaces
            .iter()
            .map(|w| (w.name.clone(), w.reference.clone(), w.output.clone(), w.windows, w.urgent, w.visible, w.focused))
            .collect()
    }

    fn w(name: &str, reference: &str, output: &str, n: u16, u: bool, v: bool, fo: bool) -> (String, String, String, u16, bool, bool, bool) {
        (name.into(), reference.into(), output.into(), n, u, v, fo)
    }

    /// (class, title, workspace, focused) of each toplevel, the selector aside (ids vary per run).
    fn toplevels(e: &CompositorState) -> Vec<(String, String, String, bool)> {
        e.toplevels.iter().map(|t| (t.class.clone(), t.title.clone(), t.workspace.clone(), t.focused)).collect()
    }

    fn t(class: &str, title: &str, workspace: &str, focused: bool) -> (String, String, String, bool) {
        (class.into(), title.into(), workspace.into(), focused)
    }

    fn tree_with(edit: impl Fn(&mut Value)) -> Vec<u8> {
        let mut v: Value = serde_json::from_slice(TREE).unwrap();
        edit(&mut v);
        serde_json::to_vec(&v).unwrap()
    }

    /// The node whose app_id is `app_id`, for editing (test only: recursion is fine here).
    fn node<'a>(v: &'a mut Value, app_id: &str) -> Option<&'a mut Value> {
        if v.get("app_id").and_then(Value::as_str) == Some(app_id) {
            return Some(v);
        }
        v.as_object_mut()?
            .iter_mut()
            .filter(|(k, _)| *k == "nodes" || *k == "floating_nodes")
            .filter_map(|(_, children)| children.as_array_mut())
            .flatten()
            .find_map(|c| node(c, app_id))
    }

    /// Spec § 3: GET_WORKSPACES' order; num, or name:<name>; counts from the tree (floating
    /// included); the scratchpad never listed.
    #[test]
    fn workspaces_and_window_counts_from_the_captured_session() {
        let e = state(WORKSPACES, TREE).unwrap();
        assert_eq!(
            summary(&e),
            [
                w("1", "1", "HEADLESS-2", 2, false, true, true),
                w("music", "name:music", "HEADLESS-2", 1, true, false, false),
                w("2", "2", "HEADLESS-1", 0, false, true, false),
            ]
        );
        assert_eq!(e.focused_output(), Some("HEADLESS-2"), "the headless sway focuses its last output");
    }

    #[test]
    fn the_focused_window_wayland_and_xwayland() {
        let e = state(WORKSPACES, TREE).unwrap();
        assert_eq!(e.window, Some(Window { class: "fixture-a".into(), title: "Fixture A".into() }));
        let x = tree_with(|v| {
            let n = node(v, "fixture-a").unwrap();
            n["app_id"] = Value::Null;
            n["shell"] = "xwayland".into();
            n["window_properties"] = serde_json::json!({"class": "XFixture", "instance": "xfixture", "title": "Fixture A"});
        });
        assert_eq!(state(WORKSPACES, &x).unwrap().window, Some(Window { class: "XFixture".into(), title: "Fixture A".into() }));
        let none = tree_with(|v| node(v, "fixture-a").unwrap()["focused"] = false.into());
        assert_eq!(state(WORKSPACES, &none).unwrap().window, None, "no focused node with a pid: no window");
    }

    /// Spec § 3: the `focus` arrays from the root, most recent first, the first moved
    /// to the end (Hyprland's order: previous first, focused last); the scratchpad's d nowhere.
    #[test]
    fn toplevels_follow_the_focus_arrays_focused_last() {
        let e = state(WORKSPACES, TREE).unwrap();
        assert_eq!(
            toplevels(&e),
            [t("fixture-b", "Fixture B", "1", false), t("fixture-c", "Fixture C", "music", false), t("fixture-a", "Fixture A", "1", true)]
        );
        for top in &e.toplevels {
            let id = top.selector.strip_prefix("id:").and_then(|n| n.parse::<u64>().ok());
            assert!(id.is_some_and(|n| n > 0), "{}", top.selector);
        }
        let tree = br#"{"id":1,"type":"root","name":"root","focus":[2],"nodes":[{"id":2,"type":"output","name":"HEADLESS-1","focus":[3],"nodes":[{"id":3,"type":"workspace","name":"1","focus":[12,10,11],"nodes":[{"id":10,"type":"con","name":"ten","pid":1,"app_id":"a","focused":false,"nodes":[]},{"id":11,"type":"con","name":"eleven","pid":1,"app_id":"b","focused":false,"nodes":[]}],"floating_nodes":[{"id":12,"type":"floating_con","name":"twelve","pid":1,"app_id":"c","focused":true,"nodes":[]}]}]}]}"#;
        let ws = br#"[{"num":1,"name":"1","output":"HEADLESS-1","visible":true,"focused":true,"urgent":false}]"#;
        let e = state(ws, tree).unwrap();
        assert_eq!(
            e.toplevels.iter().map(|t| t.selector.as_str()).collect::<Vec<_>>(),
            ["id:10", "id:11", "id:12"],
            "focus [12, 10, 11]: 10 previous first, 12 focused last"
        );
        assert_eq!(e.workspaces[0].windows, 3, "the floating window counted");
    }

    /// Review Focus 3: the launcher (a layer surface) holds the keyboard: no node focused; the
    /// most recently focused window, first in its `focus` array, stays last.
    #[test]
    fn with_no_focused_window_the_most_recent_stays_last() {
        let none = tree_with(|v| node(v, "fixture-a").unwrap()["focused"] = false.into());
        let e = state(WORKSPACES, &none).unwrap();
        assert_eq!(e.toplevels.last().map(|t| t.title.as_str()), Some("Fixture A"));
        assert!(!e.toplevels.iter().any(|t| t.focused));
    }

    #[test]
    fn the_scratchpad_is_never_listed() {
        let e = state(WORKSPACES, TREE).unwrap();
        assert!(!e.toplevels.iter().any(|t| t.class == "fixture-d"), "d is in the scratchpad");
        let ws = br#"[{"num":-1,"name":"__i3_scratch","output":"__i3","visible":false,"focused":false,"urgent":false},{"num":1,"name":"1","output":"HEADLESS-1","visible":true,"focused":true,"urgent":false}]"#;
        assert_eq!(state(ws, TREE).unwrap().workspaces.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), ["1"]);
    }

    /// Review Focus 5: a named workspace is `name:<the raw name>`, one argv element; a numbered
    /// one (`2: mail`) its number; the shown name cleaned.
    #[test]
    fn named_workspaces_are_referenced_by_name() {
        let ws = format!(
            r#"[{{"num":-1,"name":"web; $(x) \"q\"","output":"HEADLESS-1","visible":true,"focused":true,"urgent":false}},{{"num":2,"name":"2: mail","output":"HEADLESS-1","visible":false,"focused":false,"urgent":false}},{{"num":0,"name":"0","output":"HEADLESS-1"}},{{"name":"bidi{rlo}evil","output":"HEADLESS-1"}}]"#,
            rlo = '\u{202e}'
        );
        let e = state(ws.as_bytes(), br#"{"id":1,"type":"root","nodes":[]}"#).unwrap();
        assert_eq!(
            e.workspaces.iter().map(|w| (w.name.as_str(), w.reference.as_str())).collect::<Vec<_>>(),
            [("web; $(x) \"q\"", "name:web; $(x) \"q\""), ("2: mail", "2"), ("0", "0"), ("bidievil", "name:bidi\u{202e}evil")],
            "num ≥ 0: the number; num absent or negative: name:<raw>; shown names cleaned"
        );
        assert!(e.workspaces.iter().all(|w| !w.visible || w.name.starts_with("web")), "absent flags: false");
    }

    /// Spec § 3 bounds: 64 workspaces, 512 windows; labels cleaned and capped.
    #[test]
    fn bounds_and_labels() {
        let ws: Vec<String> = (1..=100).map(|n| format!(r#"{{"num":{n},"name":"{n}","output":"HEADLESS-1"}}"#)).collect();
        let windows: Vec<String> =
            (1..=600).map(|n| format!(r#"{{"id":{n},"type":"con","name":"w{n}","pid":1,"app_id":"a","nodes":[]}}"#)).collect();
        let tree = format!(
            r#"{{"id":100000,"type":"root","nodes":[{{"id":100001,"type":"output","name":"HEADLESS-1","nodes":[{{"id":100002,"type":"workspace","name":"1","nodes":[{}]}}]}}]}}"#,
            windows.join(",")
        );
        let e = state(format!("[{}]", ws.join(",")).as_bytes(), tree.as_bytes()).unwrap();
        assert_eq!(e.workspaces.len(), MAX_WORKSPACES);
        assert_eq!(e.toplevels.len(), MAX_WINDOWS);
        assert_eq!(e.workspaces[0].windows, MAX_WINDOWS as u16, "counted among the windows kept");
        let long = tree_with(|v| node(v, "fixture-a").unwrap()["name"] = format!("{}\u{7}{}", "x".repeat(100), "y".repeat(900)).into());
        let title = state(WORKSPACES, &long).unwrap().window.unwrap().title;
        assert_eq!(title.chars().count(), crate::text::MAX_TEXT);
        assert!(!title.contains('\u{7}'));
    }

    /// root → output → workspace → (depth − 3) split containers → one window at `depth`.
    fn chain(depth: usize) -> String {
        let mut s = String::from(r#"{"id":99999,"type":"con","name":"deep","pid":1,"app_id":"deep","focused":true,"nodes":[]}"#);
        for i in 0..depth.saturating_sub(3) {
            s = format!(r#"{{"id":{i},"type":"con","name":null,"nodes":[{s}]}}"#);
        }
        format!(
            r#"{{"id":100000,"type":"root","name":"root","nodes":[{{"id":100001,"type":"output","name":"HEADLESS-1","nodes":[{{"id":100002,"type":"workspace","name":"1","nodes":[{s}]}}]}}]}}"#
        )
    }

    /// Review Focus 2 (spec § 3): parsed on a thread with the compositor thread's 256 KiB stack:
    /// 43 containers deep (88 levels of JSON) still finds its window; 70 deep is past
    /// MAX_NESTING, refused before serde_json recurses (in a debug build its recursion to its own
    /// 128-level limit overflows 256 KiB); never a stack overflow.
    #[test]
    fn a_deep_tree_neither_overflows_nor_loses_its_window() {
        let ws = br#"[{"num":1,"name":"1","output":"HEADLESS-1","visible":true,"focused":true}]"#;
        let h = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let found = state(ws, chain(43).as_bytes()).map(|e| e.window.map(|w| w.title));
                let refused = state(ws, chain(70).as_bytes()).err();
                let pathological = state(ws, chain(1000).as_bytes()).err();
                (found, refused, pathological)
            })
            .unwrap();
        let (found, refused, pathological) = h.join().expect("no stack overflow on a 256 KiB stack");
        assert_eq!(found, Ok(Some("deep".to_string())));
        assert_eq!(refused.as_deref(), Some("unreadable sway tree: nested past 90 levels"));
        assert_eq!(pathological.as_deref(), Some("unreadable sway tree: nested past 90 levels"));
    }

    /// Uses at least `kib` KiB of stack, then runs `f`: the frames above `state` on the
    /// compositor thread (the source's session and its supervisor), modelled with margin.
    fn with_reserve<R>(kib: usize, f: &dyn Fn() -> R) -> R {
        let buf = [0u8; 1024];
        std::hint::black_box(&buf);
        if kib == 0 { f() } else { with_reserve(kib - 1, f) }
    }

    /// Review Focus 2 at the bound itself: replies nested exactly MAX_NESTING levels, in the
    /// shapes that cost serde_json the most stack per level (objects of several keys, and sway's
    /// own object/`nodes` alternation), parsed and dropped on a 256 KiB thread after a 24 KiB
    /// reserve, in a debug build too. A limit too high for this stack aborts here.
    #[test]
    fn a_reply_at_max_nesting_parses_on_the_compositor_stack() {
        let mut objects = String::from("0");
        for _ in 0..MAX_NESTING {
            objects = format!(r#"{{"p0":0,"p1":"x","p2":null,"z":{objects}}}"#);
        }
        let sway = chain((MAX_NESTING - 2) / 2);
        let ws = br#"[{"num":1,"name":"1","output":"HEADLESS-1","visible":true,"focused":true}]"#;
        let h = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                with_reserve(24, &|| {
                    let objects = state(ws, objects.as_bytes()).map(|e| e.toplevels.len());
                    let sway = state(ws, sway.as_bytes()).map(|e| e.window.map(|w| w.title));
                    (objects, sway)
                })
            })
            .unwrap();
        let (objects, sway) = h.join().expect("no stack overflow at MAX_NESTING on a 256 KiB stack");
        assert_eq!(objects, Ok(0), "read, not refused");
        assert_eq!(sway, Ok(Some("deep".to_string())), "read, not refused");
    }

    /// root → output → workspace → splits → one window at `depth` (the root at 0), as a Value: a
    /// window past MAX_DEPTH only exists past serde_json's nesting limit, so the walk's own bound
    /// is reached this way. Built and taken apart without recursion.
    fn deep_value(depth: usize) -> Value {
        let mut v = serde_json::json!({"id": 99999, "type": "con", "name": "deep", "pid": 1, "app_id": "deep", "nodes": []});
        for i in 0..depth.saturating_sub(3) {
            v = serde_json::json!({"id": i, "type": "con", "name": null, "nodes": [v]});
        }
        serde_json::json!({"id": 100000, "type": "root", "name": "root", "nodes": [
            {"id": 100001, "type": "output", "name": "HEADLESS-1", "nodes": [
                {"id": 100002, "type": "workspace", "name": "1", "nodes": [v]}]}]})
    }

    /// Drops a single-child chain from the top, one level at a time (a recursive drop of a deep
    /// chain could exhaust a small stack).
    fn take_apart(mut v: Value) {
        while let Some(child) = v.get_mut("nodes").and_then(Value::as_array_mut).and_then(Vec::pop) {
            v = child;
        }
    }

    /// Spec § 3: nodes deeper than MAX_DEPTH are ignored, never a crash.
    #[test]
    fn the_walk_stops_at_max_depth() {
        let found = |depth: usize| {
            let v = deep_value(depth);
            let titles: Vec<String> = windows(&v).into_iter().map(|w| w.title).collect();
            take_apart(v);
            titles
        };
        assert_eq!(found(MAX_DEPTH), ["deep"], "a window at MAX_DEPTH is found");
        assert!(found(MAX_DEPTH + 1).is_empty(), "one level deeper: ignored");
        assert!(found(200).is_empty());
    }

    #[test]
    fn unreadable_replies_are_errors() {
        assert!(state(b"not json", TREE).unwrap_err().contains("unreadable sway workspaces"));
        assert_eq!(
            state(WORKSPACES, b"{").unwrap_err(),
            "unreadable sway tree: Eof at line 1 column 1",
            "serde's class and place, never its text"
        );
        assert!(state(b"{\"success\":true}", TREE).unwrap_err().contains("not a list"));
        let latin1 = [b"[{\"num\":1,\"name\":\"caf".as_slice(), &[0xe9], b"\",\"output\":\"HEADLESS-1\"}]"].concat();
        assert_eq!(state(&latin1, TREE).unwrap().workspaces[0].name, "caf\u{fffd}", "lossy, never refused");
    }
}
