//! Tests of the do-not-disturb detail (`dnd.rs`): the six choices and their labels, and the entry the
//! state marks. No clock, no file, no daemon.

use super::*;

#[test]
fn the_six_choices_and_their_labels() {
    let labels: Vec<String> = CHOICES.iter().map(|c| label(*c, (8, 0))).collect();
    assert_eq!(labels, ["Off", "30 minutes", "1 hour", "2 hours", "Until 08:00", "Until I turn it off"]);
    assert_eq!(label(DndChoice::Until, (22, 5)), "Until 22:05");
}

#[test]
fn the_mark_sits_on_the_entry_the_state_matches() {
    let until = (8, 0);
    assert_eq!(marked(Some(false), None, until), Some(0));
    assert_eq!(marked(Some(true), None, until), Some(5));
    assert_eq!(marked(Some(true), Some((8, 0)), until), Some(4));
    assert_eq!(marked(Some(true), Some((14, 30)), until), None);
    assert_eq!(marked(None, None, until), None);
}
