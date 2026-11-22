use super::*;

fn rows(text: &str, width: u16) -> Vec<&str> {
    lines(text, width).iter().map(|l| text.get(l.start..l.end).unwrap_or("?")).collect()
}

#[test]
fn wrap_breaks_after_the_last_space_that_fits() {
    assert_eq!(rows("aaa bbb ccc", 8), ["aaa bbb ", "ccc"]);
    assert_eq!(rows("aaa bbbb", 6), ["aaa ", "bbbb"], "the word moves down whole, the break is not at the width");
}

#[test]
fn wrap_cuts_a_word_longer_than_the_row() {
    assert_eq!(rows("abcdefghij", 4), ["abcd", "efgh", "ij"]);
}

#[test]
fn wrap_counts_wide_characters_as_two_cells() {
    assert_eq!(rows("漢漢漢", 5), ["漢漢", "漢"]);
    assert_eq!(rows("漢漢", 1), ["漢", "漢"], "a character wider than the row takes a row alone");
}

#[test]
fn newlines_end_rows_and_a_trailing_one_adds_an_empty_row() {
    assert_eq!(rows("a\n\nb\n", 10), ["a", "", "b", ""]);
    assert_eq!(rows("", 10), [""]);
    assert_eq!(rows("abc", 0), ["a", "b", "c"], "width 0 counts as 1");
}

#[test]
fn locate_and_offset_round_trip() {
    let text = "aaa bbb ccc\n漢x";
    let l = lines(text, 8);
    for at in 0..=text.len() {
        if !text.is_char_boundary(at) {
            continue;
        }
        let (row, col) = locate(text, &l, at);
        assert_eq!(offset_at(text, &l, row, col), at, "at {at} -> ({row}, {col})");
    }
    assert_eq!(locate(text, &l, 8), (1, 0), "a soft break belongs to the next row");
    assert_eq!(offset_at(text, &l, 0, 40), 7, "past a soft row: its last scalar, not the next row");
    assert_eq!(offset_at(text, &l, 1, 40), 11, "past a hard row: its end");
    assert_eq!(offset_at(text, &l, 9, 0), 12, "a row past the end is the last");
    let wide = 12 + "漢".len();
    assert_eq!(offset_at(text, &l, 2, 1), 12, "the right half of a wide character is the character");
    assert_eq!(offset_at(text, &l, 2, 2), wide);
}

#[test]
fn a_64_kib_line_wraps_without_panicking() {
    let text = "a".repeat(crate::notes::text::CAP);
    assert_eq!(lines(&text, 55).len(), crate::notes::text::CAP.div_ceil(55));
}
