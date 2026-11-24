//! Tests of the selector's pixels and inputs (`select_app.rs`): the dim, the punched selection and
//! its border, the card laid over the frame, and how keys and pointer events become model input.
//! No Wayland: the buffers are plain byte vectors.

use super::*;

const ACCENT: Rgb = Rgb(10, 20, 30);

fn pixel(px: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let o = (y as usize * width as usize + x as usize) * 4;
    [px[o], px[o + 1], px[o + 2], px[o + 3]]
}

#[test]
fn the_dim_is_a_quarter_black_and_the_selection_is_clear() {
    let (w, h) = (20, 12);
    let mut px = vec![0xAA; (w * h * 4) as usize];
    paint_dim(&mut px, w, h, Some((6, 4, 6, 4)), 2, ACCENT);
    assert_eq!(u32::from(DIM_ALPHA) * 100 / 255, 25, "a quarter, as designed");
    assert_eq!(pixel(&px, w, 0, 0), [0, 0, 0, DIM_ALPHA], "outside: black at a quarter");
    assert_eq!(pixel(&px, w, 19, 11), [0, 0, 0, DIM_ALPHA]);
    assert_eq!(pixel(&px, w, 6, 4), [0, 0, 0, 0], "inside: clear, the first pixel");
    assert_eq!(pixel(&px, w, 11, 7), [0, 0, 0, 0], "inside: clear, the last pixel");
}

#[test]
fn the_border_is_outside_the_selection_in_the_accent_and_opaque() {
    let (w, h) = (20, 12);
    let mut px = vec![0; (w * h * 4) as usize];
    paint_dim(&mut px, w, h, Some((6, 4, 6, 4)), 2, ACCENT);
    let ink = [30, 20, 10, 0xff];
    assert_eq!(pixel(&px, w, 5, 4), ink, "left strip, next to the selection");
    assert_eq!(pixel(&px, w, 4, 4), ink, "left strip, two px thick");
    assert_eq!(pixel(&px, w, 3, 4), [0, 0, 0, DIM_ALPHA], "no thicker than that");
    assert_eq!(pixel(&px, w, 12, 5), ink, "right strip");
    assert_eq!(pixel(&px, w, 8, 3), ink, "top strip");
    assert_eq!(pixel(&px, w, 8, 8), ink, "bottom strip");
    assert_eq!(pixel(&px, w, 4, 2), ink, "the corner is closed");
    assert_eq!(pixel(&px, w, 13, 9), ink, "the opposite corner too");
}

#[test]
fn a_selection_at_the_edge_clips_its_border_and_never_panics() {
    let (w, h) = (20, 12);
    let mut px = vec![0; (w * h * 4) as usize];
    paint_dim(&mut px, w, h, Some((0, 0, 20, 12)), 4, ACCENT);
    assert_eq!(pixel(&px, w, 0, 0), [0, 0, 0, 0], "the whole output selected: all clear");
    assert_eq!(pixel(&px, w, 19, 11), [0, 0, 0, 0]);
    // Past the buffer on every side: still no panic, nothing outside written.
    paint_dim(&mut px, w, h, Some((-30, -30, 100, 100)), 4, ACCENT);
    paint_dim(&mut px, w, h, Some((i32::MAX - 1, 5, u32::MAX, 3)), 2, ACCENT);
}

#[test]
fn without_a_selection_the_whole_surface_is_dimmed() {
    let (w, h) = (8, 4);
    let mut px = vec![0xFF; (w * h * 4) as usize];
    paint_dim(&mut px, w, h, None, 2, ACCENT);
    assert!(px.chunks(4).all(|p| p == [0, 0, 0, DIM_ALPHA]));
}

#[test]
fn a_buffer_too_small_for_its_size_is_left_alone() {
    let mut px = vec![7; 10];
    paint_dim(&mut px, 20, 12, None, 2, ACCENT);
    assert_eq!(px, vec![7; 10]);
}

#[test]
fn lay_over_keeps_an_opaque_source_and_lets_a_clear_one_show_through() {
    let dst_pixel = [0, 0, 0, DIM_ALPHA];
    let mut dst: Vec<u8> = dst_pixel.repeat(4 * 4);
    // A 2 × 1 card: an opaque pixel, then a transparent one.
    let src = [200, 100, 50, 255, 0, 0, 0, 0];
    lay_over(&mut dst, 4, 4, &src, 2, 1, (1, 2));
    assert_eq!(pixel(&dst, 4, 1, 2), [200, 100, 50, 255], "opaque: exactly the source");
    assert_eq!(pixel(&dst, 4, 2, 2), dst_pixel, "transparent: the frame under it");
    assert_eq!(pixel(&dst, 4, 0, 2), dst_pixel, "nothing else touched");
}

#[test]
fn lay_over_clips_a_card_partly_outside_the_frame() {
    let mut dst = vec![0; 4 * 4 * 4];
    let src = [9; 3 * 3 * 4];
    lay_over(&mut dst, 4, 4, &src, 3, 3, (-1, -1));
    assert_eq!(pixel(&dst, 4, 0, 0), [9; 4]);
    assert_eq!(pixel(&dst, 4, 1, 1), [9; 4]);
    assert_eq!(pixel(&dst, 4, 2, 2), [0; 4], "the card ends at its own size");
    lay_over(&mut dst, 4, 4, &src, 3, 3, (3, 3));
    lay_over(&mut dst, 4, 4, &src, 3, 3, (-50, 90));
    assert_eq!(pixel(&dst, 4, 3, 3), [9; 4]);
}

#[test]
fn the_selector_answers_to_escape_enter_the_arrows_and_space() {
    assert_eq!(sel_key(Keysym::from(key::Escape)), Some(SelKey::Escape));
    assert_eq!(sel_key(Keysym::from(key::Return)), Some(SelKey::Enter));
    assert_eq!(sel_key(Keysym::from(key::KP_Enter)), Some(SelKey::Enter));
    assert_eq!(sel_key(Keysym::from(key::Left)), Some(SelKey::Left));
    assert_eq!(sel_key(Keysym::from(key::Right)), Some(SelKey::Right));
    assert_eq!(sel_key(Keysym::from(key::Up)), Some(SelKey::Up));
    assert_eq!(sel_key(Keysym::from(key::Down)), Some(SelKey::Down));
    assert_eq!(sel_key(Keysym::from(key::space)), Some(SelKey::SpaceDown));
    assert_eq!(sel_key(Keysym::from(key::a)), None);
    assert_eq!(sel_key(Keysym::from(key::Shift_L)), None, "a modifier alone is no key");
}

#[test]
fn a_left_press_starts_a_drag_and_a_right_press_cancels() {
    let press = |button| PointerEventKind::Press { time: 0, button, serial: 1 };
    assert_eq!(pointer_input(&press(0x110), (10.9, 20.2)), Some(SelInput::Press(10, 20)), "the pixel under the pointer");
    assert_eq!(pointer_input(&press(0x111), (10.0, 20.0)), Some(SelInput::Key(SelKey::Escape, Mods::default())));
    assert_eq!(pointer_input(&press(0x112), (10.0, 20.0)), None, "the middle button does nothing");
}

#[test]
fn motion_enter_and_the_left_release_become_input_and_the_rest_none() {
    assert_eq!(pointer_input(&PointerEventKind::Motion { time: 0 }, (5.5, 6.5)), Some(SelInput::Motion(5, 6)));
    assert_eq!(
        pointer_input(&PointerEventKind::Enter { serial: 1 }, (5.0, 6.0)),
        Some(SelInput::Motion(5, 6)),
        "the pointer's place is known at once"
    );
    assert_eq!(pointer_input(&PointerEventKind::Release { time: 0, button: 0x110, serial: 1 }, (0.0, 0.0)), Some(SelInput::Release));
    assert_eq!(pointer_input(&PointerEventKind::Release { time: 0, button: 0x111, serial: 1 }, (0.0, 0.0)), None);
    assert_eq!(pointer_input(&PointerEventKind::Leave { serial: 1 }, (0.0, 0.0)), None);
}

#[test]
fn a_position_left_of_the_surface_floors_below_zero() {
    // The model clamps; the input keeps the sign, so a press left of the edge is pulled to it.
    assert_eq!(pointer_input(&PointerEventKind::Motion { time: 0 }, (-0.5, -3.2)), Some(SelInput::Motion(-1, -4)));
}

/// Only the user's own gestures end a selector silently (Esc, a click without a drag, focus taken
/// elsewhere, another surface opened over it); the compositor taking it away is a failure.
#[test]
fn only_the_users_gestures_are_a_silent_cancel() {
    let r = Rect { x: 1, y: 2, width: 3, height: 4 };
    assert_eq!(SelectClose::Done(r).answer(), SelectorAnswer::Area(r));
    for gesture in [SelectClose::Cancelled, SelectClose::FocusLost, SelectClose::Replaced] {
        assert_eq!(gesture.answer(), SelectorAnswer::Cancelled, "{gesture:?}");
    }
    for taken in [SelectClose::OutputGone, SelectClose::Error] {
        assert_eq!(taken.answer(), SelectorAnswer::Failed, "{taken:?}");
    }
}
