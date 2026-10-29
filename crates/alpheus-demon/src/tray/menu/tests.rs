use super::*;
use crate::tray::codec::MenuProps;

fn node(id: i32, label: &str) -> MenuNode {
    MenuNode { id, props: MenuProps { label: Some(label.into()), ..MenuProps::default() }, children: Vec::new() }
}

fn with(mut n: MenuNode, f: impl FnOnce(&mut MenuProps)) -> MenuNode {
    f(&mut n.props);
    n
}

fn root(children: Vec<MenuNode>) -> Layout {
    Layout { revision: 1, root: MenuNode { id: 0, props: MenuProps::default(), children } }
}

/// FakeMenu::sample's tree (task 3), built in memory: "Open", "Mute" (checkmark on), "Low"/"High"
/// (radios, High on), a separator, "Disabled", a hidden entry, "More" (a submenu not fetched yet).
fn sample() -> Layout {
    root(vec![
        node(1, "Open"),
        with(node(2, "Mute"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Checkmark), Some(1))),
        with(node(3, "Low"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Radio), Some(0))),
        with(node(4, "High"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Radio), Some(1))),
        with(node(5, ""), |p| p.separator = Some(true)),
        with(node(6, "Disabled"), |p| p.enabled = Some(false)),
        with(node(7, "Hidden"), |p| p.visible = Some(false)),
        with(node(8, "More"), |p| p.submenu = Some(true)),
    ])
}

/// "More" fetched: its two children grafted (MenuSession, task 6).
fn fetched() -> Layout {
    let mut l = sample();
    let mut more = l.node(8).unwrap().clone();
    more.children = vec![node(9, "Settings"), node(10, "About")];
    assert!(l.graft(more));
    l
}

/// As the wiring opens it (task 17): relayout selects the first selectable entry.
fn open(l: &Layout) -> Menu {
    let mut m = Menu::new();
    m.relayout(l);
    m
}

/// Sized and rendered like the wiring does, on an output of `columns` × `rows` cells.
fn draw(m: &mut Menu, l: &Layout, columns: u16, rows: u16) -> Boxed {
    let (c, r) = m.size(l, columns, rows);
    m.set_rows(usize::from(r - 2));
    let out = m.render(l, c, r);
    assert_eq!(out.rows.len(), usize::from(r) - 2, "the size's ring (one row each side) is the painter's frame");
    for g in &out.rows {
        assert_eq!(g.width(), c - 2, "{}", snapshot(&out));
    }
    out
}

/// Each row's text, then each row's style codes, then the rows drawn as lines.
fn snapshot(b: &Boxed) -> String {
    let texts: Vec<String> = b.rows.iter().map(Grid::text).collect();
    let styles: Vec<String> = b.rows.iter().map(Grid::styles).collect();
    format!("{}\n\n{}\n\nseparators: {:?}", texts.join("\n"), styles.join("\n"), b.separators)
}

fn selected_id(m: &Menu, l: &Layout) -> Option<i32> {
    let e = m.entries(l);
    m.selected_index(&e).map(|i| e[i].id)
}

#[test]
fn menu_checks_radios_separators_disabled_hidden() {
    let l = sample();
    let mut m = open(&l);
    let rows = draw(&mut m, &l, 200, 40);
    insta::assert_snapshot!("menu_sample", snapshot(&rows));
    assert_eq!(m.entries(&l).len(), 7, "the hidden entry is not listed");
}

#[test]
fn submenu_level_with_its_header() {
    let l = fetched();
    let mut m = open(&l);
    m.key(Key::End, &l);
    assert_eq!(m.key(Key::Enter, &l), MenuAction::Submenu(8));
    assert!(m.enter(8, &l));
    let rows = draw(&mut m, &l, 200, 40);
    insta::assert_snapshot!("menu_submenu", snapshot(&rows));
}

#[test]
fn long_labels_truncated_and_width_bounded() {
    let l = root(vec![node(1, &"A very long label ".repeat(10)), with(node(2, "Short"), |p| p.submenu = Some(true))]);
    let mut m = open(&l);
    let rows = draw(&mut m, &l, 200, 40);
    assert_eq!(rows.cells().0 + 2, MENU_MAX_COLUMNS, "the size counts the ring the painter replaces");
    insta::assert_snapshot!("menu_long_labels", snapshot(&rows));
    let rows = draw(&mut m, &l, 30, 40);
    assert_eq!(rows.cells().0 + 2, 30, "never wider than the output");
}

#[test]
fn a_tall_menu_scrolls() {
    let l = root((1..=30).map(|i| node(i, &format!("Entry {i}"))).collect());
    let mut m = open(&l);
    draw(&mut m, &l, 200, 8);
    for _ in 0..9 {
        m.key(Key::Down, &l);
    }
    assert_eq!(selected_id(&m, &l), Some(10));
    let rows = draw(&mut m, &l, 200, 8);
    insta::assert_snapshot!("menu_scrolled", snapshot(&rows));
    assert_eq!(m.hover(5, &l), MenuAction::Nothing, "the last visible row is the selection");
    assert_eq!(m.press(0, &l), MenuAction::Click(5), "row 0 is entry 5 once scrolled");
}

/// The ▴/▾ marks keep a blank cell before them, and sit on the nearest entry row when the
/// first or last visible row is a separator.
#[test]
fn scroll_marks_keep_a_gap_and_skip_separators() {
    // An entry, a separator, an entry, …: the window's edges meet separators.
    let children: Vec<MenuNode> = (1..=19)
        .map(|i| if i % 2 == 0 { with(node(i, ""), |p| p.separator = Some(true)) } else { node(i, &format!("Entry {i}")) })
        .collect();
    let l = root(children);
    let mut at_a_separator = 0;
    // 12 columns: the longest label is truncated, its last cell would touch the mark.
    for columns in [12, 200] {
        let mut m = open(&l);
        for _ in 0..12 {
            let rows = draw(&mut m, &l, columns, 8); // 6 rows shown
            let texts: Vec<String> = rows.rows.iter().map(Grid::text).collect();
            let has = |mark: char| texts.iter().position(|t| t.ends_with(mark));
            let numbers: Vec<u32> = texts
                .iter()
                .filter_map(|t| t.trim().strip_prefix("Entry "))
                .filter_map(|n| n.trim_end_matches(['▴', '▾', ' ']).parse().ok())
                .collect();
            assert_eq!(has('▴').is_some(), numbers.first() != Some(&1), "▴ iff entries lie above: {texts:?}");
            assert_eq!(has('▾').is_some(), numbers.last() != Some(&19), "▾ iff entries lie below: {texts:?}");
            for i in has('▴').into_iter().chain(has('▾')) {
                let t: Vec<char> = texts[i].chars().collect();
                assert_eq!(t[t.len() - 2], ' ', "a blank cell before the mark: {texts:?}");
                assert!(!rows.separators.contains(&i), "never on a separator line: {texts:?}");
            }
            at_a_separator += usize::from(rows.separators.contains(&0) || rows.separators.contains(&(rows.rows.len() - 1)));
            m.key(Key::Down, &l);
        }
    }
    assert!(at_a_separator > 0, "the scenario reached a separator on an edge row");
}

#[test]
fn hostile_labels_never_misalign() {
    let hostile = [
        "x".repeat(300),
        "日本語のメニュー項目がとても長い場合".to_string(),
        "\u{202e}reversed\u{200e} bidi \u{2066}isolate\u{2069}".to_string(),
        "🦀🦀🦀 crab 🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀".to_string(),
        "e\u{301}cole\u{0300}".to_string(),
    ];
    let l = root(
        hostile
            .iter()
            .enumerate()
            .map(|(i, h)| with(node(i as i32 + 1, h), |p| p.submenu = Some(i % 2 == 0)))
            .chain(std::iter::once(with(node(99, "t"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Radio), Some(1)))))
            .collect(),
    );
    for columns in [3u16, 12, 20, 33, 48, 200] {
        let mut m = open(&l);
        for _ in 0..l.root.children.len() {
            let rows = draw(&mut m, &l, columns, 40);
            for r in &rows.rows {
                let t = r.text();
                assert_eq!(r.width(), rows.cells().0, "misaligned at {columns}: {t:?}");
                assert!(!t.contains(['\u{202e}', '\u{200e}', '\u{2066}']), "{t:?}");
            }
            m.key(Key::Down, &l);
        }
    }
}

#[test]
fn arrows_skip_separators_and_disabled() {
    let l = sample();
    let mut m = open(&l);
    assert_eq!(selected_id(&m, &l), Some(1), "the first selectable entry at open");
    let mut seen = Vec::new();
    for _ in 0..6 {
        m.key(Key::Down, &l);
        seen.push(selected_id(&m, &l).unwrap());
    }
    assert_eq!(seen, [2, 3, 4, 8, 8, 8], "over the separator and Disabled, no wrap");
    assert_eq!(m.key(Key::Down, &l), MenuAction::Nothing, "at the end: nothing to redraw");
    assert_eq!(m.key(Key::Up, &l), MenuAction::Redraw);
    assert_eq!(selected_id(&m, &l), Some(4), "back over them");
    m.key(Key::Home, &l);
    assert_eq!(selected_id(&m, &l), Some(1));
    m.key(Key::CtrlN, &l);
    assert_eq!(selected_id(&m, &l), Some(2), "Ctrl+N like the launcher");
    m.key(Key::PageDown, &l);
    assert_eq!(selected_id(&m, &l), Some(8));
}

#[test]
fn enter_on_an_item_clicks() {
    let l = sample();
    let mut m = open(&l);
    m.key(Key::Down, &l);
    assert_eq!(m.key(Key::Enter, &l), MenuAction::Click(2));
    assert_eq!(m.key(Key::Right, &l), MenuAction::Nothing, "→ on an item that is no submenu");
}

#[test]
fn enter_or_right_on_a_submenu_asks_for_it() {
    let l = sample();
    let mut m = open(&l);
    m.key(Key::End, &l);
    assert_eq!(m.key(Key::Enter, &l), MenuAction::Submenu(8));
    assert_eq!(m.key(Key::Right, &l), MenuAction::Submenu(8));
    assert!(!m.enter(8, &l), "its children are not known yet: stays at the root");
    assert_eq!(m.entries(&l).len(), 7);
    let l = fetched();
    assert!(m.enter(8, &l), "grafted: entered");
    assert_eq!(m.entries(&l).iter().map(|e| e.id).collect::<Vec<_>>(), [9, 10]);
    assert_eq!(selected_id(&m, &l), Some(9));
}

/// A stale asynchronous `enter` reaching the menu after the user went back (or into a
/// different submenu) must be refused: `11` was an entry of "More"'s level, not of the root.
#[test]
fn enter_refuses_an_id_that_is_not_an_entry_of_the_current_level() {
    let mut l = sample();
    let mut more = l.node(8).unwrap().clone();
    more.children = vec![node(9, "Settings"), with(node(11, "Deeper"), |p| p.submenu = Some(true))];
    assert!(l.graft(more));
    let mut deeper = l.node(11).unwrap().clone();
    deeper.children = vec![node(12, "Deepest")];
    assert!(l.graft(deeper));

    let mut m = open(&l);
    m.key(Key::End, &l);
    assert!(m.enter(8, &l), "enters More");
    m.key(Key::Down, &l);
    assert!(m.enter(11, &l), "enters Deeper");
    assert_eq!(m.entries(&l).iter().map(|e| e.id).collect::<Vec<_>>(), [12]);

    assert_eq!(m.key(Key::Left, &l), MenuAction::Redraw, "back to More's level");
    assert_eq!(m.key(Key::Left, &l), MenuAction::Redraw, "back to the root");
    assert_eq!(m.entries(&l).len(), 7);

    assert!(!m.enter(11, &l), "11 is no longer an entry of the current (root) level");
    assert_eq!(m.entries(&l).len(), 7, "stays at the root");
}

#[test]
fn left_goes_back_one_level_escape_closes() {
    let l = fetched();
    let mut m = open(&l);
    m.key(Key::End, &l);
    m.enter(8, &l);
    assert_eq!(m.key(Key::Left, &l), MenuAction::Redraw);
    assert_eq!(m.entries(&l).len(), 7, "back at the root");
    assert_eq!(selected_id(&m, &l), Some(8), "on the submenu it came from");
    assert_eq!(m.key(Key::Left, &l), MenuAction::Nothing, "at the root, ← does nothing");
    assert_eq!(m.key(Key::Escape, &l), MenuAction::Close);
    assert_eq!(m.key(Key::Char('x'), &l), MenuAction::Nothing);
}

#[test]
fn relayout_keeps_selection_by_id() {
    let l = sample();
    let mut m = open(&l);
    m.key(Key::Down, &l);
    m.key(Key::Down, &l);
    assert_eq!(selected_id(&m, &l), Some(3));
    let mut changed = sample();
    changed.root.children.insert(0, node(20, "New first"));
    m.relayout(&changed);
    assert_eq!(selected_id(&m, &changed), Some(3), "still Low, one row lower");
    let mut gone = sample();
    gone.root.children.retain(|n| n.id != 3);
    m.relayout(&gone);
    assert_eq!(selected_id(&m, &gone), Some(1), "Low gone: the first selectable");
}

#[test]
fn relayout_pops_a_vanished_submenu() {
    let l = fetched();
    let mut m = open(&l);
    m.key(Key::End, &l);
    m.enter(8, &l);
    let mut gone = sample();
    gone.root.children.retain(|n| n.id != 8);
    m.relayout(&gone);
    assert_eq!(m.entries(&gone).len(), 6, "back at the root, without More");
    assert_eq!(m.key(Key::Left, &gone), MenuAction::Nothing, "no level left to pop");
}

/// `relayout` and `enter` agree on when a level is alive (`level_alive`): a level whose
/// children all turned hidden is popped, exactly like one whose parent vanished entirely.
#[test]
fn relayout_pops_a_level_whose_children_all_turn_hidden() {
    let l = fetched();
    let mut m = open(&l);
    m.key(Key::End, &l);
    assert!(m.enter(8, &l));
    assert_eq!(m.entries(&l).len(), 2, "inside More");
    let mut hidden = fetched();
    let mut more = hidden.node(8).unwrap().clone();
    for c in &mut more.children {
        c.props.visible = Some(false);
    }
    assert!(hidden.graft(more));
    m.relayout(&hidden);
    assert_eq!(m.entries(&hidden).len(), 7, "back at the root: More has nothing left to show");
}

/// Same predicate, the other half: the level's own entry is still there with visible
/// children, but no longer selectable (disabled) — popped all the same.
#[test]
fn relayout_pops_a_level_whose_own_entry_turns_disabled() {
    let l = fetched();
    let mut m = open(&l);
    m.key(Key::End, &l);
    assert!(m.enter(8, &l));
    assert_eq!(m.entries(&l).len(), 2, "inside More");
    let mut disabled = fetched();
    let mut more = disabled.node(8).unwrap().clone();
    more.props.enabled = Some(false);
    assert!(disabled.graft(more));
    m.relayout(&disabled);
    assert_eq!(m.entries(&disabled).len(), 7, "back at the root: More is now disabled");
}

/// LayoutUpdated(parent 0): the wiring re-reads the root at depth 1, which announces More
/// again as a submenu but without its children; task 2's `graft` carries the fetched ones
/// over, so the open submenu is not popped and keeps its selection.
#[test]
fn relayout_keeps_a_submenu_across_a_root_reread() {
    let mut l = fetched();
    let mut m = open(&l);
    m.key(Key::End, &l);
    assert!(m.enter(8, &l));
    m.key(Key::Down, &l);
    assert_eq!(selected_id(&m, &l), Some(10));
    let reread = sample().root;
    assert!(reread.children.iter().any(|n| n.id == 8 && n.children.is_empty()), "depth 1: no grandchildren");
    assert!(l.graft(reread));
    m.relayout(&l);
    assert_eq!(m.entries(&l).iter().map(|e| e.id).collect::<Vec<_>>(), [9, 10], "still inside More");
    assert_eq!(selected_id(&m, &l), Some(10), "the selection kept by id");
}

#[test]
fn hover_selects_press_runs() {
    let l = fetched();
    let mut m = open(&l);
    draw(&mut m, &l, 200, 40);
    assert_eq!(m.hover(2, &l), MenuAction::Redraw, "row 2: Low");
    assert_eq!(selected_id(&m, &l), Some(3));
    assert_eq!(m.hover(2, &l), MenuAction::Nothing, "the same row: nothing to redraw");
    assert_eq!(m.hover(4, &l), MenuAction::Nothing, "the separator is never selected");
    assert_eq!(m.hover(5, &l), MenuAction::Nothing, "nor the disabled entry");
    assert_eq!(selected_id(&m, &l), Some(3));
    assert_eq!(m.press(4, &l), MenuAction::Nothing);
    assert_eq!(m.press(0, &l), MenuAction::Click(1), "at the root there is no header: row 0 is the first entry");
    assert_eq!(m.press(1, &l), MenuAction::Click(2));
    assert_eq!(m.press(6, &l), MenuAction::Submenu(8));
    assert_eq!(m.press(30, &l), MenuAction::Nothing, "below the entries");
    m.enter(8, &l);
    assert_eq!(m.press(0, &l), MenuAction::Redraw, "a press on the '◂ parent' header goes back one level, like Left");
    assert_eq!(m.entries(&l).len(), 7, "back at the root");
    assert!(m.enter(8, &l), "re-enter to keep exercising the submenu below");
    assert_eq!(m.press(2, &l), MenuAction::Click(10));
    assert_eq!(m.scroll(-1, &l), MenuAction::Redraw);
    assert_eq!(selected_id(&m, &l), Some(9), "the wheel moves the selection");
}

/// `relayout` keeps both the selection and the scroll window consistent when a level shrinks
/// under a selection far down the list. This is not needed to keep `render` from panicking —
/// it reads entries with `.get(i)` (never indexes past the end), and `top()` independently
/// re-derives a safe scroll window from the *current* `selected_index` on every call,
/// regardless of what `relayout` leaves in `level.top` — but without this clamp a stale `top`
/// would still render a blank tail until the next key or hover nudged it back into range.
#[test]
fn relayout_clamps_selection_and_scroll_window_when_entries_shrink() {
    let l = root((1..=10).map(|i| node(i, &format!("Entry {i}"))).collect());
    let mut m = open(&l);
    for _ in 0..7 {
        m.key(Key::Down, &l);
    }
    assert_eq!(selected_id(&m, &l), Some(8));
    let shrunk = root(vec![node(1, "Entry 1")]);
    m.relayout(&shrunk);
    assert_eq!(selected_id(&m, &shrunk), Some(1), "clamped to the only entry left, never out of range");
    assert_eq!(m.key(Key::Down, &shrunk), MenuAction::Nothing, "nowhere further to go");
    assert_eq!(selected_id(&m, &shrunk), Some(1));
    // Rendering must not panic either: a stale `top` would index past the shrunk entries.
    let rows = draw(&mut m, &shrunk, 200, 40);
    assert_eq!(rows.rows.len(), 1, "one entry (the ring's two rows are the frame)");
}

#[test]
fn size_never_exceeds_a_tiny_output() {
    let l = sample();
    let m = open(&l);
    for &(max_columns, max_rows) in &[(0u16, 40u16), (1, 40), (2, 40), (40, 0), (40, 1), (40, 2), (0, 0), (1, 1)] {
        let (c, r) = m.size(&l, max_columns, max_rows);
        assert!(c <= max_columns, "columns {c} > bound {max_columns}");
        assert!(r <= max_rows, "rows {r} > bound {max_rows}");
    }
}

#[test]
fn render_is_safe_on_a_tiny_output() {
    let l = sample();
    let mut m = open(&l);
    for &(max_columns, max_rows) in &[(0u16, 0u16), (1, 1), (2, 2), (3, 3)] {
        let (c, r) = m.size(&l, max_columns, max_rows);
        m.set_rows(usize::from(r.saturating_sub(2)));
        let rows = m.render(&l, c, r); // must not panic
        for g in &rows.rows {
            assert_eq!(g.width(), c.saturating_sub(2));
        }
    }
}

#[test]
fn size_reserves_header_and_one_entry_row_in_a_submenu_when_the_output_allows_it() {
    let l = fetched();
    let mut m = open(&l);
    m.key(Key::End, &l);
    assert!(m.enter(8, &l));
    let (_, rows) = m.size(&l, 200, 40);
    assert!(rows >= 4, "frame + header + at least one entry + frame: got {rows}");
    let (_, rows) = m.size(&l, 200, 3);
    assert_eq!(rows, 3, "the output doesn't allow 4: never exceed the bound either");
}

#[test]
fn a_menu_with_nothing_selectable_selects_nothing() {
    let l = root(vec![with(node(1, ""), |p| p.separator = Some(true)), with(node(2, "Off"), |p| p.enabled = Some(false))]);
    let mut m = open(&l);
    assert_eq!(selected_id(&m, &l), None);
    for k in [Key::Down, Key::Up, Key::End, Key::Enter, Key::Right] {
        assert_eq!(m.key(k, &l), MenuAction::Nothing, "{k:?}");
    }
    let empty = root(Vec::new());
    let mut m = open(&empty);
    let rows = draw(&mut m, &empty, 200, 40);
    assert!(rows.rows[0].text().contains("empty"), "{}", snapshot(&rows));
    assert_eq!(m.key(Key::Enter, &empty), MenuAction::Nothing);
}
