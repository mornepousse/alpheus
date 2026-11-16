use super::*;
use crate::grid::Style;
use crate::notes::keys::{Motion, NoteKey};
use crate::notes::model::{Notes, Refusal};

fn text_of(g: &Grid) -> String {
    g.cells().iter().filter(|c| c.ch != crate::grid::CONT).map(|c| c.ch).collect()
}

fn notes(text: &str) -> (Notes, NotesLayout) {
    let b = layout(232, 49);
    let mut n = Notes::new(text.to_string(), None);
    n.set_area(wrap_width(&b), usize::from(b.text_rows));
    (n, b)
}

#[test]
fn layout_fits_the_output() {
    let b = layout(232, 49);
    assert_eq!((b.columns, b.rows, b.text_rows, b.text_top, b.text_columns), (60, 20, 14, 3, 56));
    let b = layout(30, 8);
    assert_eq!((b.columns, b.text_rows, b.rows), (28, 3, 9));
    let b = layout(1, 1);
    assert_eq!((b.columns, b.text_rows), (1, 3), "a tiny output never underflows");
    assert_eq!(wrap_width(&layout(232, 49)), 55);
    assert_eq!(wrap_width(&b), 1);
}

#[test]
fn render_shows_title_status_text_caret_and_help() {
    let (mut n, b) = notes("buy milk\nand tea");
    n.pad.set_caret(4, false);
    let r = render(&n, &b);
    let title = text_of(&r.rows[0]);
    assert!(title.contains("Notes") && title.contains("saved"), "{title:?}");
    assert_eq!(r.separators, vec![1, 16]);
    assert_eq!(r.rows.len(), usize::from(b.rows) - 2);
    assert!(text_of(&r.rows[2]).contains("buy milk"));
    assert!(text_of(&r.rows[3]).contains("and tea"));
    // The caret: the `m` of milk, at text column 4; the Boxed dropped the ring (one column).
    let caret = &r.rows[2].cells()[usize::from(TEXT_LEFT - 1 + 4)];
    assert_eq!((caret.ch, caret.style), ('m', Style::ACTIVE));
    assert_eq!(r.rows[2].cells()[usize::from(TEXT_LEFT - 1 + 3)].style, Style::NORMAL);
    assert_eq!(text_of(&r.rows[17]).trim(), "esc close · ctrl+c copy · ctrl+v paste");

    n.dirty = true;
    assert!(text_of(&render(&n, &b).rows[0]).contains("edited"));
    let mut ro = Notes::new("x".into(), Some(Refusal::NotOurs));
    let r = render(&ro, &b);
    assert!(text_of(&r.rows[0]).contains("read only"));
    assert_eq!(text_of(&r.rows[17]).trim(), "read only: notes.txt is not ours", "the refusal shows at open");
    ro.key(NoteKey::Move { to: Motion::Left, extend: false });
    assert_eq!(text_of(&render(&ro, &b).rows[17]).trim(), "esc close · ctrl+c copy");
}

#[test]
fn the_caret_at_a_rows_end_or_on_a_newline_is_a_blank_active_cell() {
    let (mut n, b) = notes("ab\ncd");
    n.pad.set_caret(2, false);
    let r = render(&n, &b);
    let c = &r.rows[2].cells()[usize::from(TEXT_LEFT - 1 + 2)];
    assert_eq!((c.ch, c.style), (' ', Style::ACTIVE));
    n.pad.set_caret(5, false);
    let r = render(&n, &b);
    let c = &r.rows[3].cells()[usize::from(TEXT_LEFT - 1 + 2)];
    assert_eq!((c.ch, c.style), (' ', Style::ACTIVE));
}

#[test]
fn an_empty_pad_shows_the_hint_after_the_caret() {
    let (n, b) = notes("");
    let r = render(&n, &b);
    let row = &r.rows[2];
    assert_eq!(row.cells()[usize::from(TEXT_LEFT - 1)].style, Style::ACTIVE);
    assert!(text_of(row).contains("Type here. Esc closes; the text is kept."));
    let at = text_of(row).find('T').unwrap();
    assert_eq!(row.cells()[at].style, Style::MUTED);
    let (n, _) = notes("x");
    assert!(!text_of(&render(&n, &b).rows[2]).contains("Type here"));
    // I1: an empty read-only pad (a file not ours) never invites typing.
    let mut ro = Notes::new(String::new(), Some(Refusal::NotOurs));
    ro.set_area(wrap_width(&b), usize::from(b.text_rows));
    assert!(!text_of(&render(&ro, &b).rows[2]).contains("Type here"));
}

/// M6 (decision 4): `not saved` in the error style, `read only` in warn, the others muted.
#[test]
fn the_status_word_styles() {
    let (mut n, b) = notes("x");
    let style_of = |n: &Notes, word: &str| {
        let row = &render(n, &b).rows[0];
        let at = text_of(row).find(word).unwrap();
        row.cells()[at].style
    };
    assert_eq!(style_of(&n, "saved"), Style::MUTED);
    n.dirty = true;
    assert_eq!(style_of(&n, "edited"), Style::MUTED);
    n.save_failed = true;
    assert_eq!(style_of(&n, "not saved"), Style::ERROR);
    n.read_only = Some(Refusal::NotUtf8);
    assert_eq!(style_of(&n, "read only"), Style::WARN);
}

#[test]
fn a_selection_is_drawn_selected() {
    let (mut n, b) = notes("buy milk");
    n.pad.set_caret(4, false);
    n.key(NoteKey::Move { to: Motion::End, extend: true });
    let r = render(&n, &b);
    let cells = r.rows[2].cells();
    for i in 0..3 {
        assert_eq!(cells[usize::from(TEXT_LEFT - 1) + i].style, Style::NORMAL, "{i}");
    }
    for i in 4..7 {
        assert_eq!(cells[usize::from(TEXT_LEFT - 1) + i].style, Style::SELECTED, "{i}");
    }
}

#[test]
fn a_message_replaces_the_help_row_in_warn() {
    let (mut n, b) = notes("x");
    n.show_message("the pad is full (64 KiB)".into());
    let r = render(&n, &b);
    let row = &r.rows[17];
    assert!(text_of(row).contains("the pad is full (64 KiB)"));
    assert!(!text_of(row).contains("esc close"));
    let at = text_of(row).find('t').unwrap();
    assert_eq!(row.cells()[at].style, Style::WARN);
}

#[test]
fn a_scrolled_pad_draws_from_its_top_row() {
    let text = (0..40).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
    let (n, b) = notes(&text);
    let r = render(&n, &b);
    assert!(text_of(&r.rows[2]).contains("l26"), "{:?}", text_of(&r.rows[2]));
    assert!(text_of(&r.rows[15]).contains("l39"));
}

#[test]
fn hit_maps_text_cells_only() {
    let b = layout(232, 49);
    assert_eq!(hit(&b, 2, 3), Some((0, 0)));
    assert_eq!(hit(&b, 7, 5), Some((2, 5)));
    assert_eq!(hit(&b, 57, 16), Some((13, 55)));
    assert_eq!(hit(&b, 2, 1), None, "title");
    assert_eq!(hit(&b, 2, 2), None, "separator");
    assert_eq!(hit(&b, 2, 17), None, "separator");
    assert_eq!(hit(&b, 2, 18), None, "help");
    assert_eq!(hit(&b, 2, 0), None, "ring");
    assert_eq!(hit(&b, 0, 5), None, "frame");
    assert_eq!(hit(&b, 1, 5), None, "margin");
    assert_eq!(hit(&b, 58, 5), None, "right margin");
}
