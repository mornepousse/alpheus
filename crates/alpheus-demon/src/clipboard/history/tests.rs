use super::*;

fn texts(h: &History) -> Vec<String> {
    h.rows().into_iter().map(|r| r.preview.clone()).collect()
}

fn id_of(h: &History, text: &str) -> u64 {
    h.rows().into_iter().find(|r| r.preview == text).map(|r| r.id).unwrap()
}

#[test]
fn push_adds_newest_first_and_merges_duplicates() {
    let mut h = History::default();
    assert!(matches!(h.push("alpha".into()), Pushed::Added(_)));
    h.push("beta".into());
    let alpha = id_of(&h, "alpha");
    assert_eq!(h.push("alpha".into()), Pushed::Moved(alpha));
    assert_eq!(texts(&h), ["alpha", "beta"]);
    assert_eq!(id_of(&h, "alpha"), alpha);
    assert_eq!(h.len(), 2);
}

#[test]
fn a_duplicate_keeps_its_pin() {
    let mut h = History::default();
    h.push("alpha".into());
    h.push("beta".into());
    let alpha = id_of(&h, "alpha");
    assert_eq!(h.toggle_pin(alpha), Ok(true));
    h.push("alpha".into());
    assert!(h.rows().iter().find(|r| r.id == alpha).unwrap().pinned);
}

#[test]
fn too_long_and_blank_texts_are_skipped() {
    let mut h = History::default();
    assert!(matches!(h.push("a".repeat(MAX_BYTES)), Pushed::Added(_)));
    assert_eq!(h.push("a".repeat(MAX_BYTES + 1)), Pushed::Skipped);
    assert_eq!(h.push("  \n\t".into()), Pushed::Skipped);
    assert_eq!(h.push(String::new()), Pushed::Skipped);
    assert_eq!(h.len(), 1);
}

#[test]
fn eviction_takes_the_oldest_unpinned() {
    let mut h = History::default();
    for i in 1..=MAX_ENTRIES {
        h.push(format!("t{i}"));
    }
    let first = id_of(&h, "t1");
    h.toggle_pin(first).unwrap();
    h.push(format!("t{}", MAX_ENTRIES + 1));
    assert_eq!(h.len(), MAX_ENTRIES);
    assert!(h.text_of(first).is_some(), "the pinned one stays");
    assert!(!texts(&h).contains(&"t2".to_string()), "the oldest unpinned went");
}

#[test]
fn pins_are_bounded_listed_first_and_kept_by_clear() {
    let mut h = History::default();
    for i in 0..12 {
        h.push(format!("t{i}"));
    }
    let ids: Vec<u64> = (0..12).map(|i| id_of(&h, &format!("t{i}"))).collect();
    for id in ids.iter().take(MAX_PINS) {
        assert_eq!(h.toggle_pin(*id), Ok(true));
    }
    assert_eq!(h.toggle_pin(ids[10]), Err(PinError::Full));
    assert_eq!(h.toggle_pin(9999), Err(PinError::Gone));
    let rows = h.rows();
    assert!(rows.iter().take(MAX_PINS).all(|r| r.pinned));
    assert_eq!(rows[0].id, ids[MAX_PINS - 1], "most recently pinned first");
    assert!(h.has_unpinned());
    assert_eq!(h.clear(), 2);
    assert_eq!(h.len(), MAX_PINS);
    assert!(!h.has_unpinned());
    assert!(h.delete(ids[0]), "delete removes a pinned entry");
    assert_eq!(h.len(), MAX_PINS - 1);
    assert_eq!(h.toggle_pin(ids[1]), Ok(false), "twice unpins");
}

#[test]
fn touch_moves_to_the_front() {
    let mut h = History::default();
    h.push("alpha".into());
    h.push("beta".into());
    assert!(h.touch(id_of(&h, "alpha")));
    assert_eq!(texts(&h), ["alpha", "beta"]);
    assert!(!h.touch(9999));
}

#[test]
fn rows_preview_is_one_clean_line() {
    let mut h = History::default();
    h.push("buy milk\n\ttoken 1234\n".into());
    let r = &h.rows()[0];
    assert_eq!(r.preview, "buy milk token 1234");
    assert_eq!(r.more_lines, 1);

    let mut h = History::default();
    h.push("x".repeat(3000));
    let r = &h.rows()[0];
    assert!(r.preview.len() <= PREVIEW_BYTES);
    assert_eq!(r.more_lines, 0);

    for pad in 0..4 {
        let mut h = History::default();
        h.push(format!("{}{}", "a".repeat(PREVIEW_BYTES - 2 + pad), "é".repeat(10)));
        let r = &h.rows()[0];
        assert!(r.preview.len() <= PREVIEW_BYTES);
        assert!(r.preview.is_char_boundary(r.preview.len()));
    }
}

#[test]
fn debug_never_prints_text() {
    let mut h = History::default();
    h.push("buy milk".into());
    let id = h.rows()[0].id;
    let row = h.rows()[0].clone();
    let entry = &h.entries[0];
    for s in [format!("{h:?}"), format!("{entry:?}"), format!("{row:?}")] {
        assert!(!s.contains("buy milk"), "{s}");
        assert!(!s.contains("len"), "{s}");
    }
    assert!(format!("{entry:?}").contains(&id.to_string()));
}

#[test]
fn rows_preview_drops_invisible_characters() {
    let mut h = History::default();
    h.push("pay\u{202e}me\u{2066}now\u{2069} \u{200b}ok".into());
    assert_eq!(h.rows()[0].preview, "paymenow ok");
}

#[test]
fn rows_show_the_first_line_and_count_the_lines_not_shown() {
    let mut h = History::default();
    h.push("\n  first\tline \nsecond\nthird\n\n".into());
    let r = &h.rows()[0];
    assert_eq!(r.first, "first line", "the first non-blank line, folded");
    assert_eq!(r.more_lines, 2, "second and third: trailing blank lines are not lines");
    assert_eq!(r.preview, "first line second third", "matching sees every line");

    let mut h = History::default();
    h.push("one line".into());
    let r = &h.rows()[0];
    assert_eq!((r.first.as_str(), r.more_lines), ("one line", 0));

    let mut h = History::default();
    h.push(format!("{}\nnext", "a".repeat(3000)));
    let r = &h.rows()[0];
    assert!(r.first.len() <= PREVIEW_BYTES);
    assert_eq!(r.more_lines, 1);
}

#[test]
fn a_row_wipes_its_first_line_too() {
    let src = include_str!("../history.rs");
    let drop_impl = src.split("impl Drop for ClipRow").nth(1).and_then(|s| s.split("\n}\n").next()).unwrap();
    assert!(drop_impl.contains("wipe(&mut self.preview)") && drop_impl.contains("wipe(&mut self.first)"), "{drop_impl}");
}

#[test]
fn an_unpinned_entry_heads_the_unpinned_ones_as_the_launcher_box_lists_it() {
    let mut h = History::default();
    for t in ["a", "b", "c", "d"] {
        h.push(t.into());
    }
    // Newest first: d c b a. Pin then unpin the oldest.
    let a = id_of(&h, "a");
    assert_eq!(h.toggle_pin(a), Ok(true));
    assert_eq!(texts(&h), ["a", "d", "c", "b"]);
    assert_eq!(h.toggle_pin(a), Ok(false));
    assert_eq!(texts(&h), ["a", "d", "c", "b"], "right behind the (now empty) pinned group");
    let c = id_of(&h, "c");
    h.toggle_pin(c).unwrap();
    h.toggle_pin(c).unwrap();
    assert_eq!(texts(&h), ["c", "a", "d", "b"]);
}
