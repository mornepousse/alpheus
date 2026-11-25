//! Tests of the picker's pixels and inputs (`pick_app.rs`): the lens and its marker, the swatch, how
//! keys and pointer events become model input, and which closes are a silent cancel. No Wayland:
//! the buffers are plain byte vectors.

use super::*;
use smithay_client_toolkit::seat::pointer::PointerEventKind;

const ACCENT: Rgb = Rgb(10, 20, 30);

fn pixel(px: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let o = (y as usize * width as usize + x as usize) * 4;
    [px[o], px[o + 1], px[o + 2], px[o + 3]]
}

/// 81 cells, cell i being (i, 0, 0) as R, G, B, so each can be told apart.
fn cells() -> [Rgb; LENS * LENS] {
    let mut c = [Rgb(0, 0, 0); LENS * LENS];
    for (i, cell) in c.iter_mut().enumerate() {
        *cell = Rgb(i as u8, 0, 0);
    }
    c
}

#[test]
fn the_lens_magnifies_each_pixel_into_a_cell_framed_in_the_accent() {
    let (w, h) = (120, 120);
    let mut px = vec![0xAA; (w * h * 4) as usize];
    let drawn = paint_lens(&mut px, w, h, (10, 10, 10), &cells(), 2, ACCENT);
    assert_eq!(drawn, (8, 8, 102, 102), "the lens and its border");
    // Cell (col 2, row 1) covers x 30..40, y 20..30, opaque, bytes B, G, R, A.
    assert_eq!(pixel(&px, w, 35, 25), [0, 0, 11, 255]);
    assert_eq!(pixel(&px, w, 30, 20), [0, 0, 11, 255]);
    assert_eq!(pixel(&px, w, 99, 99), [0, 0, 80, 255], "the last cell");
    // The border: 2 px outside the cells, in the accent.
    assert_eq!(pixel(&px, w, 8, 50), [30, 20, 10, 255]);
    assert_eq!(pixel(&px, w, 101, 101), [30, 20, 10, 255]);
    assert_eq!(pixel(&px, w, 7, 50), [0xAA; 4], "nothing past the border");
    assert_eq!(pixel(&px, w, 102, 102), [0xAA; 4]);
}

/// The centre cell, the pixel that a click picks, is outlined in a colour that stands out from it:
/// white on a dark pixel, black on a light one; its middle shows the pixel itself.
#[test]
fn the_centre_cell_is_marked_in_a_contrasting_colour() {
    let (w, h) = (120, 120);
    let mut dark = cells();
    dark[40] = Rgb(5, 5, 5);
    let mut px = vec![0; (w * h * 4) as usize];
    paint_lens(&mut px, w, h, (10, 10, 10), &dark, 2, ACCENT);
    // The centre cell covers x 50..60, y 50..60; its outline is 1 px (a tenth of the cell) here.
    assert_eq!(pixel(&px, w, 50, 55), [255, 255, 255, 255], "white outline on a dark pixel");
    assert_eq!(pixel(&px, w, 55, 55), [5, 5, 5, 255], "the pixel itself inside");
    let mut light = cells();
    light[40] = Rgb(250, 240, 230);
    paint_lens(&mut px, w, h, (10, 10, 10), &light, 2, ACCENT);
    assert_eq!(pixel(&px, w, 59, 55), [0, 0, 0, 255], "black outline on a light pixel");
}

#[test]
fn a_lens_partly_outside_the_buffer_is_clipped_and_never_panics() {
    let (w, h) = (40, 30);
    let mut px = vec![0; (w * h * 4) as usize];
    paint_lens(&mut px, w, h, (-50, 20, 10), &cells(), 2, ACCENT);
    paint_lens(&mut px, w, h, (35, 25, 10), &cells(), 2, ACCENT);
    assert_eq!(pixel(&px, w, 39, 29), [0, 0, 0, 255], "cell 0 at the corner");
    // A buffer too small for its size is left alone.
    let mut small = vec![0; 4];
    paint_lens(&mut small, w, h, (0, 0, 10), &cells(), 2, ACCENT);
    assert_eq!(small, [0; 4]);
}

#[test]
fn the_swatch_is_the_colour_framed_in_the_accent() {
    let (w, h) = (30, 30);
    let mut px = vec![0; (w * h * 4) as usize];
    let drawn = paint_swatch(&mut px, w, h, (5, 5, 10), Rgb(1, 2, 3), 1, ACCENT);
    assert_eq!(drawn, (4, 4, 16, 16));
    assert_eq!(pixel(&px, w, 10, 10), [3, 2, 1, 255]);
    assert_eq!(pixel(&px, w, 4, 10), [30, 20, 10, 255]);
    assert_eq!(pixel(&px, w, 3, 10), [0; 4]);
}

#[test]
fn the_picker_answers_to_escape_and_enter_only() {
    assert_eq!(pick_key(Keysym::from(key::Escape)), Some(PickInput::Cancel));
    assert_eq!(pick_key(Keysym::from(key::Return)), Some(PickInput::Pick));
    assert_eq!(pick_key(Keysym::from(key::KP_Enter)), Some(PickInput::Pick));
    assert_eq!(pick_key(Keysym::from(key::space)), None);
    assert_eq!(pick_key(Keysym::from(key::Left)), None);
}

#[test]
fn a_left_press_picks_a_right_press_cancels_and_motion_follows() {
    let left = PointerEventKind::Press { time: 0, button: 0x110, serial: 0 };
    let right = PointerEventKind::Press { time: 0, button: 0x111, serial: 0 };
    let middle = PointerEventKind::Press { time: 0, button: 0x112, serial: 0 };
    let release = PointerEventKind::Release { time: 0, button: 0x110, serial: 0 };
    assert_eq!(pick_pointer_input(&left, (1.0, 2.0)), Some(PickInput::Pick));
    assert_eq!(pick_pointer_input(&right, (1.0, 2.0)), Some(PickInput::Cancel));
    assert_eq!(pick_pointer_input(&middle, (1.0, 2.0)), None);
    assert_eq!(pick_pointer_input(&release, (1.0, 2.0)), None);
    assert_eq!(pick_pointer_input(&PointerEventKind::Motion { time: 0 }, (3.5, 4.25)), Some(PickInput::Motion(3.5, 4.25)));
}

/// Only the user's own gestures end the picker silently (Esc, a right click, focus taken elsewhere,
/// another surface opened over it); the compositor taking it away, or a fault, is a failure.
#[test]
fn only_the_users_gestures_are_a_silent_cancel() {
    assert_eq!(PickClose::Picked(Rgb(1, 2, 3)).answer(), PickAnswer::Colour(Rgb(1, 2, 3)));
    for gesture in [PickClose::Cancelled, PickClose::FocusLost, PickClose::Replaced] {
        assert_eq!(gesture.answer(), PickAnswer::Cancelled, "{gesture:?}");
    }
    for taken in [PickClose::OutputGone, PickClose::Error] {
        assert_eq!(taken.answer(), PickAnswer::Failed, "{taken:?}");
    }
}

#[test]
fn the_union_of_two_areas_holds_both() {
    assert_eq!(union(Some((0, 0, 5, 5)), Some((3, 4, 10, 6))), Some((0, 0, 10, 6)));
    assert_eq!(union(None, Some((3, 4, 10, 6))), Some((3, 4, 10, 6)));
    assert_eq!(union(None, None), None);
}

/// What a draw damages: a buffer not painted yet, all of it; else the overlay it wipes, the overlay
/// the screen shows now (the last committed buffer's, another buffer when two alternate) and the new
/// one. Lenses at P1 (buffer A), P2 (B), then P3 (A again): P2 is on the screen and must be damaged.
#[test]
fn a_draw_damages_what_it_wipes_what_the_screen_shows_and_what_it_draws() {
    let whole = (0, 0, 1000, 800);
    let (p1, p2, p3) = ((10, 10, 50, 50), (500, 10, 540, 50), (10, 500, 50, 540));
    assert_eq!(damage(None, Some(p1), Some(p2), whole), Some(whole), "a fresh buffer is damaged whole");
    assert_eq!(damage(Some(Some(p1)), Some(p2), Some(p3), whole), Some((10, 10, 540, 540)));
    assert_eq!(damage(Some(None), Some(p2), None, whole), Some(p2), "the lens on screen goes, even with none drawn");
    assert_eq!(damage(Some(None), None, None, whole), None);
}

/// A redraw is skipped only while the frame's pixel under the pointer stays the same: at scale 2 a
/// motion from 10.2 to 10.7 stays on logical pixel 10 but moves from the frame's pixel 20 to 21,
/// which a click would copy, so the lens must show it.
#[test]
fn the_lens_follows_the_frames_pixel_not_the_logical_one() {
    use crate::captures::screencopy::{Format, Frame};
    let frame = Frame { width: 40, height: 40, stride: 160, format: Format::Xrgb8888, y_invert: false, data: vec![0; 40 * 160] };
    let shot = Shot::new(frame, Some((20, 20)), 2, Transform::Normal);
    assert_ne!(draw_key(&shot, Some((10.2, 5.0))), draw_key(&shot, Some((10.7, 5.0))), "pixel 20, then 21");
    assert_eq!(draw_key(&shot, Some((10.6, 5.0))), draw_key(&shot, Some((10.7, 5.0))), "both on pixel 21: no redraw");
    assert_eq!(draw_key(&shot, None), None);
}
