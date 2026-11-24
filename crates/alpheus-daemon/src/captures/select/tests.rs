//! Tests of the area selector's model (`select.rs`): the gestures, the clamping and the label's
//! place. No Wayland and no clock: each test feeds the inputs it wants.

use super::*;

const NONE: Mods = Mods { ctrl: false, shift: false, alt: false, logo: false };
const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false, logo: false };
const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false, logo: false };

fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

/// A selection on a 1000 × 600 output with a drag from (100, 100) to (300, 250), button down.
fn dragged() -> Selection {
    let mut s = Selection::new(1000, 600);
    s.apply(SelInput::Press(100, 100));
    s.apply(SelInput::Motion(300, 250));
    s
}

#[test]
fn drag_selects_and_release_confirms() {
    let mut s = Selection::new(1000, 600);
    assert_eq!(s.apply(SelInput::Press(100, 100)), SelState::Selecting);
    assert_eq!(s.apply(SelInput::Motion(300, 250)), SelState::Selecting);
    assert_eq!(s.rect(), Some(rect(100, 100, 200, 150)));
    assert_eq!(s.apply(SelInput::Release), SelState::Done(rect(100, 100, 200, 150)));
}

#[test]
fn a_drag_up_and_to_the_left_gives_the_same_kind_of_rectangle() {
    let mut s = Selection::new(1000, 600);
    s.apply(SelInput::Press(300, 250));
    s.apply(SelInput::Motion(100, 100));
    assert_eq!(s.apply(SelInput::Release), SelState::Done(rect(100, 100, 200, 150)));
}

#[test]
fn a_click_without_drag_cancels() {
    let mut s = Selection::new(1000, 600);
    s.apply(SelInput::Press(100, 100));
    assert_eq!(s.apply(SelInput::Release), SelState::Cancelled);
}

#[test]
fn esc_cancels() {
    let mut s = dragged();
    assert_eq!(s.apply(SelInput::Key(SelKey::Escape, NONE)), SelState::Cancelled);
    let mut idle = Selection::new(1000, 600);
    assert_eq!(idle.apply(SelInput::Key(SelKey::Escape, NONE)), SelState::Cancelled, "before any press too");
}

#[test]
fn the_end_is_latched() {
    let mut s = dragged();
    s.apply(SelInput::Key(SelKey::Escape, NONE));
    assert_eq!(s.apply(SelInput::Press(5, 5)), SelState::Cancelled);
    assert_eq!(s.apply(SelInput::Release), SelState::Cancelled);
    let mut done = dragged();
    let end = done.apply(SelInput::Release);
    assert_eq!(done.apply(SelInput::Motion(900, 500)), end, "a motion after the release changes nothing");
    assert_eq!(done.apply(SelInput::Key(SelKey::Escape, NONE)), end, "nor does Escape");
}

#[test]
fn space_moves_the_selection() {
    let mut s = dragged();
    s.apply(SelInput::Key(SelKey::SpaceDown, NONE));
    s.apply(SelInput::Motion(340, 270));
    assert_eq!(s.rect(), Some(rect(140, 120, 200, 150)), "moved by the pointer's own difference, size kept");
    s.apply(SelInput::Key(SelKey::SpaceUp, NONE));
    s.apply(SelInput::Motion(400, 300));
    assert_eq!(s.rect(), Some(rect(140, 120, 260, 180)), "released: the corner follows the pointer again");
}

#[test]
fn a_move_stops_at_the_edge_and_keeps_the_size() {
    let mut s = dragged();
    s.apply(SelInput::Key(SelKey::SpaceDown, NONE));
    s.apply(SelInput::Motion(5000, 5000));
    assert_eq!(s.rect(), Some(rect(800, 450, 200, 150)));
    s.apply(SelInput::Motion(-5000, -5000));
    assert_eq!(s.rect(), Some(rect(0, 0, 200, 150)));
}

#[test]
fn arrows_nudge_shift_by_ten_ctrl_resizes() {
    let mut s = dragged();
    s.apply(SelInput::Key(SelKey::Right, NONE));
    assert_eq!(s.rect(), Some(rect(101, 100, 200, 150)), "1 px");
    s.apply(SelInput::Key(SelKey::Down, SHIFT));
    assert_eq!(s.rect(), Some(rect(101, 110, 200, 150)), "10 px with Shift");
    s.apply(SelInput::Key(SelKey::Left, NONE));
    s.apply(SelInput::Key(SelKey::Up, NONE));
    assert_eq!(s.rect(), Some(rect(100, 109, 200, 150)));
    s.apply(SelInput::Key(SelKey::Right, CTRL));
    assert_eq!(s.rect(), Some(rect(100, 109, 201, 150)), "Ctrl resizes the far corner");
    s.apply(SelInput::Key(SelKey::Up, Mods { ctrl: true, shift: true, alt: false, logo: false }));
    assert_eq!(s.rect(), Some(rect(100, 109, 201, 140)), "Ctrl+Shift resizes by 10");
    s.apply(SelInput::Key(SelKey::Left, CTRL));
    assert_eq!(s.rect(), Some(rect(100, 109, 200, 140)), "Ctrl+Left narrows");
    s.apply(SelInput::Key(SelKey::Down, CTRL));
    assert_eq!(s.rect(), Some(rect(100, 109, 200, 141)), "Ctrl+Down grows taller");
}

#[test]
fn arrows_without_a_drag_do_nothing() {
    let mut s = Selection::new(1000, 600);
    assert_eq!(s.apply(SelInput::Key(SelKey::Right, NONE)), SelState::Selecting);
    assert_eq!(s.rect(), None);
}

#[test]
fn a_zero_area_is_a_cancel() {
    // A line is no area: width or height zero.
    let mut line = Selection::new(1000, 600);
    line.apply(SelInput::Press(100, 100));
    line.apply(SelInput::Motion(300, 100));
    assert_eq!(line.rect(), None);
    assert_eq!(line.apply(SelInput::Release), SelState::Cancelled);
    let mut column = Selection::new(1000, 600);
    column.apply(SelInput::Press(100, 100));
    column.apply(SelInput::Motion(100, 400));
    assert_eq!(column.apply(SelInput::Key(SelKey::Enter, NONE)), SelState::Cancelled, "Enter on an empty rectangle too");
}

#[test]
fn an_area_past_the_edge_is_clamped() {
    let mut s = Selection::new(1000, 600);
    s.apply(SelInput::Press(900, 500));
    s.apply(SelInput::Motion(5000, 5000));
    assert_eq!(s.apply(SelInput::Release), SelState::Done(rect(900, 500, 100, 100)));
    let mut before = Selection::new(1000, 600);
    before.apply(SelInput::Press(100, 100));
    before.apply(SelInput::Motion(-50, -50));
    assert_eq!(before.apply(SelInput::Release), SelState::Done(rect(0, 0, 100, 100)));
    let mut outside = Selection::new(1000, 600);
    outside.apply(SelInput::Press(-20, 2000));
    outside.apply(SelInput::Motion(500, 300));
    assert_eq!(outside.apply(SelInput::Release), SelState::Done(rect(0, 300, 500, 300)), "a press outside is pulled to the edge");
}

#[test]
fn a_resize_cannot_leave_the_output() {
    let mut s = Selection::new(1000, 600);
    s.apply(SelInput::Press(900, 500));
    s.apply(SelInput::Motion(995, 595));
    s.apply(SelInput::Key(SelKey::Right, Mods { ctrl: true, shift: true, alt: false, logo: false }));
    s.apply(SelInput::Key(SelKey::Down, Mods { ctrl: true, shift: true, alt: false, logo: false }));
    assert_eq!(s.rect(), Some(rect(900, 500, 100, 100)));
}

#[test]
fn enter_confirms_the_current_rect() {
    let mut s = dragged();
    s.apply(SelInput::Key(SelKey::Right, SHIFT));
    assert_eq!(s.apply(SelInput::Key(SelKey::Enter, NONE)), SelState::Done(rect(110, 100, 200, 150)));
    let mut idle = Selection::new(1000, 600);
    assert_eq!(idle.apply(SelInput::Key(SelKey::Enter, NONE)), SelState::Selecting, "nothing to confirm yet");
}

#[test]
fn the_label_reads_width_by_height() {
    assert_eq!(dragged().label().as_deref(), Some("200×150"));
    assert_eq!(Selection::new(10, 10).label(), None);
}

#[test]
fn the_label_goes_under_the_corner_then_above_then_inside() {
    let bounds = (1000, 600);
    let label = (80, 24);
    // Room below: under the bottom left corner.
    assert_eq!(label_origin(rect(100, 100, 200, 150), label, bounds), (100, 100 + 150 + LABEL_GAP));
    // No room below: above the top left corner.
    assert_eq!(label_origin(rect(100, 300, 200, 300), label, bounds), (100, 300 - LABEL_GAP - 24));
    // The whole height: inside, near the top left corner.
    assert_eq!(label_origin(rect(100, 0, 200, 600), label, bounds), (100 + LABEL_GAP, LABEL_GAP));
    // Near the right edge: pulled back so it stays on the output.
    assert_eq!(label_origin(rect(950, 100, 50, 50), label, bounds).0, 1000 - 80);
}
