use super::*;
use crate::notes::keys::{Motion, NoteKey};
use crate::notes::lines::{lines, locate};

fn mv(to: Motion) -> NoteKey {
    NoteKey::Move { to, extend: false }
}

fn type_str(n: &mut Notes, s: &str) {
    for c in s.chars() {
        n.key(NoteKey::Char(c));
    }
}

fn row_col(n: &Notes) -> (usize, u16) {
    let ls = lines(n.pad.text(), n.width);
    locate(n.pad.text(), &ls, n.pad.caret())
}

fn numbered(count: usize) -> String {
    (0..count).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n")
}

#[test]
fn typing_marks_dirty_and_enter_breaks_the_line() {
    let mut n = Notes::new(String::new(), None);
    assert_eq!(n.status(), "saved");
    type_str(&mut n, "buy");
    assert_eq!(n.key(NoteKey::Enter), NotesOutcome::Redraw);
    type_str(&mut n, "milk");
    assert_eq!(n.pad.text(), "buy\nmilk");
    assert!(n.dirty);
    assert_eq!(n.status(), "edited");
    n.key(NoteKey::Tab);
    assert_eq!(n.pad.text(), "buy\nmilk    ");
}

#[test]
fn escape_closes_and_never_edits() {
    let mut n = Notes::new("x".into(), None);
    assert_eq!(n.key(NoteKey::Escape), NotesOutcome::Close);
    assert_eq!(n.pad.text(), "x");
    assert!(!n.dirty);
}

#[test]
fn up_and_down_keep_a_goal_column() {
    let mut n = Notes::new("abcdef\nab\nabcdef".into(), None);
    n.set_area(20, 5);
    n.pad.set_caret(5, false);
    assert_eq!(n.key(mv(Motion::Down)), NotesOutcome::Redraw);
    assert_eq!(row_col(&n), (1, 2));
    n.key(mv(Motion::Down));
    assert_eq!(row_col(&n), (2, 5));
    n.key(mv(Motion::Up));
    n.key(mv(Motion::Left));
    assert_eq!(row_col(&n), (1, 1), "any other key resets the goal");
    n.key(mv(Motion::Down));
    assert_eq!(row_col(&n), (2, 1));
}

#[test]
fn page_down_moves_one_screen_and_scrolls() {
    let mut n = Notes::new(numbered(40), None);
    n.set_area(20, 14);
    n.pad.set_caret(0, false);
    n.key(mv(Motion::DocStart));
    n.key(mv(Motion::PageDown));
    let (row, _) = row_col(&n);
    assert_eq!(row, 13);
    assert!(n.top() <= row && row < n.top() + 14);
    n.key(mv(Motion::DocEnd));
    let (row, _) = row_col(&n);
    assert_eq!(row, 39);
    assert!(n.top() <= row && row < n.top() + 14, "the last row is visible");
    n.key(mv(Motion::PageUp));
    assert_eq!(row_col(&n).0, 26);
}

#[test]
fn copy_and_cut_need_a_selection_and_respect_the_input_cap() {
    let mut n = Notes::new("token 1234".into(), None);
    n.pad.set_caret(0, false);
    assert_eq!(n.key(NoteKey::Copy), NotesOutcome::Nothing);
    assert_eq!(n.key(NoteKey::Cut), NotesOutcome::Nothing);
    n.key(NoteKey::SelectAll);
    assert_eq!(n.key(NoteKey::Copy), NotesOutcome::Copy { text: "token 1234".into(), cut: false });
    assert_eq!(n.key(NoteKey::Cut), NotesOutcome::Copy { text: "token 1234".into(), cut: true });
    assert_eq!(n.pad.text(), "token 1234", "nothing is cut before the copy started");
    assert_eq!(n.after_copy(true, Ok(())), NotesOutcome::Redraw);
    assert_eq!(n.pad.text(), "");
    assert!(n.dirty);

    let mut n = Notes::new("token".into(), None);
    n.key(NoteKey::SelectAll);
    assert_eq!(n.key(NoteKey::Cut), NotesOutcome::Copy { text: "token".into(), cut: true });
    assert_eq!(n.after_copy(true, Err("wl-copy: program not found".into())), NotesOutcome::Redraw);
    assert_eq!(n.pad.text(), "token");
    assert_eq!(n.message(), Some("wl-copy: program not found"));

    let mut big = Notes::new("a".repeat(MAX_INPUT + 1), None);
    big.key(NoteKey::SelectAll);
    assert_eq!(big.key(NoteKey::Copy), NotesOutcome::Redraw);
    assert_eq!(big.message(), Some("selection too large to copy (4 KiB at most)"));
    let mut ok = Notes::new("a".repeat(MAX_INPUT), None);
    ok.key(NoteKey::SelectAll);
    assert!(matches!(ok.key(NoteKey::Copy), NotesOutcome::Copy { .. }), "exactly the cap is fine");
}

#[test]
fn a_read_only_pad_moves_and_copies_but_never_edits() {
    let mut n = Notes::new("x".into(), Some(Refusal::TooLarge));
    for k in [NoteKey::Char('y'), NoteKey::Backspace, NoteKey::Enter, NoteKey::Tab, NoteKey::Delete, NoteKey::WordBackspace, NoteKey::Paste]
    {
        assert_eq!(n.key(k), NotesOutcome::Redraw, "{k:?}");
        assert_eq!(n.pad.text(), "x", "{k:?}");
        assert!(!n.dirty, "{k:?}");
        assert_eq!(n.message(), Some("read only: notes.txt is too large"), "{k:?}");
    }
    assert_eq!(n.paste("z"), NotesOutcome::Redraw);
    assert_eq!(n.pad.text(), "x");
    assert_eq!(n.key(mv(Motion::Left)), NotesOutcome::Redraw);
    assert_eq!(n.message(), None, "any key clears the message");
    n.key(NoteKey::SelectAll);
    assert_eq!(n.key(NoteKey::Copy), NotesOutcome::Copy { text: "x".into(), cut: false });
    assert_eq!(n.key(NoteKey::Cut), NotesOutcome::Copy { text: "x".into(), cut: false });
    assert_eq!(n.after_copy(false, Ok(())), NotesOutcome::Nothing);
    assert_eq!(n.pad.text(), "x");
    assert_eq!(n.status(), "read only");
}

#[test]
fn paste_inserts_cleaned_text_and_says_when_full() {
    let mut n = Notes::new(String::new(), None);
    assert_eq!(n.paste("a\r\nb"), NotesOutcome::Redraw);
    assert_eq!(n.pad.text(), "a\nb");
    assert!(n.dirty);
    let mut full = Notes::new("a".repeat(crate::notes::text::CAP - 1), None);
    assert_eq!(full.paste("bcd"), NotesOutcome::Redraw);
    assert_eq!(full.pad.text().len(), crate::notes::text::CAP);
    assert_eq!(full.message(), Some("the pad is full (64 KiB)"));
    assert_eq!(full.key(NoteKey::Char('x')), NotesOutcome::Redraw, "a full pad still says so");
    assert_eq!(full.message(), Some("the pad is full (64 KiB)"));
    assert_eq!(full.key(NoteKey::Delete), NotesOutcome::Redraw, "the message goes with the next key");
    assert_eq!(full.message(), None);
}

#[test]
fn click_places_the_caret_and_scroll_never_moves_it() {
    let mut n = Notes::new(numbered(40), None);
    n.set_area(20, 14);
    n.key(mv(Motion::DocStart));
    assert_eq!(n.scroll(5), NotesOutcome::Redraw);
    assert_eq!(n.top(), 5);
    assert_eq!(n.pad.caret(), 0, "the wheel leaves the caret");
    assert_eq!(n.scroll(-100), NotesOutcome::Redraw);
    assert_eq!(n.top(), 0);
    assert_eq!(n.scroll(-1), NotesOutcome::Nothing);
    n.scroll(1000);
    assert_eq!(n.top(), 40 - 14, "never past the last screen");
    assert_eq!(n.click(2, 1), NotesOutcome::Redraw);
    assert_eq!(row_col(&n), (26 + 2, 1));
    assert_eq!(n.pad.text().get(n.pad.caret()..n.pad.caret() + 3), Some("28\n"), "column 1 of l28 is the 2");
    assert!(!n.dirty);
}

#[test]
fn the_status_word_order() {
    let mut n = Notes::new("x".into(), None);
    assert_eq!(n.status(), "saved");
    n.dirty = true;
    assert_eq!(n.status(), "edited");
    n.save_failed = true;
    assert_eq!(n.status(), "not saved");
    n.read_only = Some(Refusal::NotUtf8);
    assert_eq!(n.status(), "read only");
}

#[test]
fn refusal_words() {
    assert_eq!(Refusal::TooLarge.word(), "is too large");
    assert_eq!(Refusal::NotUtf8.word(), "is not UTF-8");
    assert_eq!(Refusal::NotOurs.word(), "is not ours");
    assert_eq!(Refusal::Unreadable.word(), "could not be read");
    assert_eq!(Refusal::NotClean.word(), "has tabs or control characters");
}

/// I1: a refused pad says why on the help row from its open, before any key.
#[test]
fn a_read_only_pad_says_why_at_open() {
    let n = Notes::new("x".into(), Some(Refusal::NotClean));
    assert_eq!(n.message(), Some("read only: notes.txt has tabs or control characters"));
    assert_eq!(Notes::new("x".into(), None).message(), None);
}

#[test]
fn a_new_pad_is_scrolled_to_its_caret() {
    let mut n = Notes::new(numbered(40), None);
    n.set_area(20, 14);
    assert_eq!(n.top(), 40 - 14);
}

#[test]
fn shift_moves_select_and_typing_replaces() {
    let mut n = Notes::new("hello world".into(), None);
    n.key(mv(Motion::DocStart));
    n.key(NoteKey::Move { to: Motion::WordRight, extend: true });
    assert_eq!(n.pad.selected(), Some("hello"));
    n.key(NoteKey::Char('J'));
    assert_eq!(n.pad.text(), "J world");
}

#[test]
fn a_big_delete_never_leaves_blank_rows() {
    let mut n = Notes::new(numbered(40), None);
    n.set_area(20, 14);
    assert_eq!(n.top(), 26);
    n.key(NoteKey::Move { to: Motion::DocStart, extend: true });
    n.key(NoteKey::Backspace);
    assert_eq!(n.pad.text(), "");
    assert_eq!(n.top(), 0);
    let mut n = Notes::new(numbered(40), None);
    n.set_area(20, 14);
    for _ in 0..10 {
        n.key(NoteKey::Move { to: Motion::Up, extend: true });
    }
    n.key(NoteKey::Backspace);
    let rows = lines(n.pad.text(), n.width).len();
    assert_eq!(rows, 30);
    assert_eq!(n.top(), rows - 14, "the last screen is full");
}
