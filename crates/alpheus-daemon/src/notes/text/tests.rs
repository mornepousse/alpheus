use super::*;

fn pad(s: &str) -> Pad {
    Pad::new(s.to_string())
}

#[test]
fn clean_normalises_newlines_tabs_and_controls() {
    assert_eq!(clean("a\r\nb\rc\td\u{7}e\u{1b}"), "a\nb\nc    de");
}

#[test]
fn insert_replaces_the_selection_and_moves_the_caret() {
    let mut p = pad("buy milk");
    p.set_caret(4, false);
    p.set_caret(8, true);
    assert_eq!(p.selected(), Some("milk"));
    assert_eq!(p.insert("tea"), Fit::Whole);
    assert_eq!(p.text(), "buy tea");
    assert_eq!(p.caret(), 7);
    assert_eq!(p.selection(), None);
}

#[test]
fn insert_past_the_cap_keeps_a_prefix_on_a_boundary() {
    let mut p = Pad::new("a".repeat(CAP - 1));
    assert_eq!(p.insert("éé"), Fit::Cut);
    assert_eq!(p.text().len(), CAP - 1, "the 2-byte é does not fit in 1");
    assert_eq!(p.insert("x"), Fit::Whole);
    assert_eq!(p.text().len(), CAP);
    assert_eq!(p.insert("x"), Fit::Cut);
    assert_eq!(p.text().len(), CAP, "nothing added");
}

#[test]
fn insert_over_a_selection_counts_the_room_it_frees() {
    let mut p = Pad::new("a".repeat(CAP));
    p.select_all();
    assert_eq!(p.insert("b"), Fit::Whole);
    assert_eq!(p.text(), "b");
}

#[test]
fn backspace_and_delete_take_a_scalar_and_its_marks() {
    let mut p = pad("e\u{301}x");
    assert!(p.backspace());
    assert_eq!(p.text(), "e\u{301}");
    assert!(p.backspace());
    assert_eq!(p.text(), "");
    assert!(!p.backspace());
    let mut p = pad("e\u{301}x");
    p.set_caret(0, false);
    assert!(p.delete());
    assert_eq!(p.text(), "x");
    let mut p = pad("abc");
    p.set_caret(1, false);
    p.set_caret(2, true);
    assert!(p.backspace());
    assert_eq!(p.text(), "ac", "the selection only");
    let mut p = pad("abc");
    p.set_caret(1, false);
    p.set_caret(2, true);
    assert!(p.delete());
    assert_eq!(p.text(), "ac", "the selection only");
}

#[test]
fn word_moves_and_deletes() {
    let mut p = pad("foo bar_baz, qux");
    assert_eq!(p.word_right_of(0), 3);
    assert_eq!(p.word_right_of(3), 11);
    assert_eq!(p.word_left_of(16), 13);
    assert_eq!(p.word_left_of(13), 4);
    p.set_caret(11, false);
    assert!(p.delete_word_back());
    assert_eq!(p.text(), "foo , qux");
    assert!(p.delete_word_forward());
    assert_eq!(p.text(), "foo ");
}

#[test]
fn line_start_and_end_are_logical() {
    let p = pad("ab\ncd\n");
    assert_eq!(p.line_start_of(4), 3);
    assert_eq!(p.line_end_of(3), 5);
    assert_eq!(p.line_end_of(6), 6);
}

#[test]
fn moves_never_split_a_scalar_or_a_mark() {
    let text = "a\u{301}漢🇫🇷\nz\u{200d}y";
    let mut p = pad(text);
    let ok = |at: usize| {
        assert!(text.is_char_boundary(at), "{at}");
        assert!(!text.get(at..).and_then(|s| s.chars().next()).is_some_and(|c| char_width(c) == 0), "{at} is before a mark");
    };
    for at in 0..=text.len() {
        if !text.is_char_boundary(at) {
            continue;
        }
        let r = p.right_of(at);
        let l = p.left_of(at);
        ok(r);
        ok(l);
        assert!(r >= at && l <= at);
        p.set_caret(at, false);
        ok(p.caret());
    }
    p.set_caret(2, false);
    assert_eq!(p.caret(), 0, "inside a scalar: back to its start (and the mark glued to a)");
    p.set_caret(1, false);
    assert_eq!(p.caret(), 0, "before a mark: back before its base");
}

#[test]
fn take_selection_removes_and_returns() {
    let mut p = pad("foo bar");
    p.set_caret(4, false);
    p.set_caret(7, true);
    assert_eq!(p.take_selection().as_deref(), Some("bar"));
    assert_eq!(p.text(), "foo ");
    assert_eq!(p.take_selection(), None);
}

#[test]
fn an_insert_before_a_mark_keeps_the_caret_after_the_insert() {
    let mut p = pad("\u{301}x");
    p.set_caret(0, false);
    assert_eq!(p.caret(), 0);
    p.insert("a");
    assert_eq!(p.text(), "a\u{301}x");
    assert!(p.caret() >= 1, "the caret is not before the inserted text: {}", p.caret());
    assert_eq!(p.caret(), "a\u{301}".len(), "and moves on past the mark that now follows it");
}
