//! Tests of the colour picker's model (`pick.rs`): the frozen frame read through the output's scale
//! and rotation, the lens, the value's text, the gestures and where the lens goes.

use super::*;
use crate::captures::screencopy::Format;
use crate::captures::{Finish, Flow, Goal, Job, PickAnswer, Scope, Step, notice};

/// A `width` × `height` XRGB frame whose pixel (x, y) is (x, y, 7) as R, G, B.
fn ramp(width: u32, height: u32) -> Frame {
    let mut data = Vec::new();
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[7, y as u8, x as u8, 0]);
        }
    }
    Frame { width, height, stride: width * 4, format: Format::Xrgb8888, y_invert: false, data }
}

fn rgb(x: i64, y: i64) -> Rgb {
    Rgb(x as u8, y as u8, 7)
}

#[test]
fn the_lens_shows_nine_by_nine_around_the_pointer() {
    let shot = Shot::new(ramp(40, 30), Some((40, 30)), 1, Transform::Normal);
    let lens = shot.lens((20.6, 10.2));
    assert_eq!(lens.len(), 81);
    for row in 0..9 {
        for col in 0..9 {
            assert_eq!(lens[row * 9 + col], rgb(16 + col as i64, 6 + row as i64), "cell ({col}, {row})");
        }
    }
    assert_eq!(lens[4 * 9 + 4], shot.colour_at((20.6, 10.2)), "the centre is the pixel under the pointer");
    // At the top left corner, the cells past the edge repeat the edge's pixels.
    let corner = shot.lens((0.0, 0.0));
    assert_eq!(corner[0], rgb(0, 0));
    assert_eq!(corner[4 * 9 + 4], rgb(0, 0));
    assert_eq!(corner[8 * 9 + 8], rgb(4, 4));
    assert_eq!(corner[8 * 9], rgb(0, 4), "clamped on x only");
    // At the bottom right, and past it (a pointer on the last logical pixel's far edge).
    let far = shot.lens((39.9, 29.9));
    assert_eq!(far[4 * 9 + 4], rgb(39, 29));
    assert_eq!(far[8 * 9 + 8], rgb(39, 29));
    assert_eq!(far[0], rgb(35, 25));
    assert_eq!(shot.colour_at((500.0, -3.0)), rgb(39, 0), "a position off the output is clamped to it");
}

/// The pointer's logical position is read in the frame's own pixels: scaled (the scale read off the
/// frame, so a fractional one is exact) and turned back through the output's rotation.
#[test]
fn the_pixel_under_the_pointer_follows_the_scale_and_the_rotation() {
    // Scale 2: logical (10.5, 5.2) is the frame's pixel (21, 10).
    let scaled = Shot::new(ramp(40, 30), Some((20, 15)), 2, Transform::Normal);
    assert_eq!(scaled.colour_at((10.5, 5.2)), rgb(21, 10));
    // A fractional scale, 1.5, read off the frame (the integer scale says 2).
    let fractional = Shot::new(ramp(30, 15), Some((20, 10)), 2, Transform::Normal);
    assert_eq!(fractional.colour_at((10.0, 4.0)), rgb(15, 6));
    // No logical size: the integer scale stands in.
    let unsized_ = Shot::new(ramp(40, 30), None, 2, Transform::Normal);
    assert_eq!(unsized_.colour_at((10.5, 5.2)), rgb(21, 10));
    // Turned 90 degrees: a 40 by 30 buffer shown 30 wide and 40 tall; the shown pixel (x, y) is the
    // buffer's (40 - y - 1, x), as `map_area` maps a 1 by 1 area.
    let turned = Shot::new(ramp(40, 30), Some((30, 40)), 1, Transform::_90);
    assert_eq!(turned.colour_at((3.0, 5.0)), rgb(34, 3));
    let r = crate::captures::screencopy::map_area(crate::captures::Rect { x: 3, y: 5, width: 1, height: 1 }, 1.0, Transform::_90, (30, 40));
    assert_eq!(turned.colour_at((3.0, 5.0)), rgb(i64::from(r.x), i64::from(r.y)));
    // The lens is laid out as the output is shown: the cell right of the centre is the shown pixel
    // right of the pointer.
    assert_eq!(turned.lens((3.0, 5.0))[4 * 9 + 5], turned.colour_at((4.0, 5.0)));
}

#[test]
fn the_value_is_uppercase_hex_by_default() {
    assert_eq!(value_text(Rgb(0x1a, 0x2b, 0xfc), DEFAULT_FORMAT), "#1A2BFC");
    assert_eq!(value_text(Rgb(0, 0, 0), DEFAULT_FORMAT), "#000000");
    assert_eq!(value_text(Rgb(255, 255, 255), "{hex}"), "#FFFFFF");
    assert_eq!(DEFAULT_FORMAT, crate::config::CapturesConfig::default().pick_format, "the config's default");
}

#[test]
fn pick_format_fills_r_g_b_hex() {
    let c = Rgb(26, 43, 252);
    assert_eq!(value_text(c, "rgb({r}, {g}, {b})"), "rgb(26, 43, 252)");
    assert_eq!(value_text(c, "{r} {g} {b} {hex}"), "26 43 252 #1A2BFC");
    assert_eq!(value_text(c, "{b}{b}"), "252252", "each placeholder as often as it is written");
    assert_eq!(value_text(c, "colour"), "colour", "no placeholder: the text as written");
    assert_eq!(value_text(c, "{x} {r"), "{x} {r", "anything else stays as written");
    assert_eq!(value_text(c, "{{r}}"), "{26}");
}

/// Escape (or a right click) ends the picker with nothing picked; the flow then copies nothing and
/// says nothing, and a later gesture changes nothing.
#[test]
fn esc_picks_nothing_and_copies_nothing() {
    let mut picker = Picker::default();
    assert_eq!(picker.apply(PickInput::Motion(10.0, 10.0)), PickState::Picking);
    assert_eq!(picker.apply(PickInput::Cancel), PickState::Cancelled);
    assert_eq!(picker.apply(PickInput::Pick), PickState::Cancelled, "the end is latched");
    let mut flow = Flow::default();
    flow.begin(Job { scope: Scope::Pick, goal: Goal::Copy });
    let g = flow.generation();
    flow.settled(g);
    assert_eq!(flow.grabbed(g, true), Step::Pick);
    let step = flow.picked(g, PickAnswer::Cancelled);
    assert_eq!(step, Step::Finish(Finish::Cancelled), "no copy step");
    assert_eq!(notice(&Finish::Cancelled), None, "no notice");
    assert!(!Finish::Cancelled.touches_the_clipboard());
    assert!(!flow.is_busy());
}

#[test]
fn a_click_or_enter_picks_where_the_pointer_is() {
    let mut picker = Picker::default();
    assert_eq!(picker.apply(PickInput::Pick), PickState::Picking, "nowhere to pick before the pointer came");
    picker.apply(PickInput::Motion(3.0, 4.0));
    picker.apply(PickInput::Motion(5.5, 6.5));
    assert_eq!(picker.pointer(), Some((5.5, 6.5)));
    assert_eq!(picker.apply(PickInput::Pick), PickState::Picked((5.5, 6.5)));
    assert_eq!(picker.apply(PickInput::Motion(1.0, 1.0)), PickState::Picked((5.5, 6.5)), "latched");
}

#[test]
fn the_lens_goes_beside_the_pointer_and_stays_on_the_output() {
    let bounds = (1000, 800);
    let block = (100, 130);
    // Room right and below: there, past a gap, so the pointer's pixel stays in view.
    assert_eq!(lens_origin((200, 300), block, bounds), (200 + LENS_GAP, 300 + LENS_GAP));
    // Near the right edge: to the left of the pointer.
    assert_eq!(lens_origin((950, 300), block, bounds), (950 - LENS_GAP - 100, 300 + LENS_GAP));
    // Near the bottom: above it.
    assert_eq!(lens_origin((200, 780), block, bounds), (200 + LENS_GAP, 780 - LENS_GAP - 130));
    // An output smaller than the block: pinned at its top left, never negative.
    assert_eq!(lens_origin((10, 10), block, (50, 50)), (0, 0));
}

/// The frozen frame is painted into the surface's buffer as it was, opaque, in the buffer's own
/// byte order (B, G, R, A), at the surface's scale; a region outside the buffer paints nothing.
#[test]
fn the_frozen_frame_is_painted_as_it_was() {
    let shot = Shot::new(ramp(4, 2), Some((4, 2)), 1, Transform::Normal);
    let mut px = vec![0u8; 4 * 2 * 4];
    shot.paint(&mut px, 4, 2, 1, (0, 0, 4, 2));
    assert_eq!(&px[..4], &[7, 0, 0, 255], "pixel (0, 0): R 0, G 0, B 7");
    assert_eq!(&px[(4 + 3) * 4..(4 + 3) * 4 + 4], &[7, 1, 3, 255], "pixel (3, 1)");
    // Painted at buffer scale 2 for an output of scale 1: each pixel twice as wide and tall.
    let mut big = vec![0u8; 8 * 4 * 4];
    shot.paint(&mut big, 8, 4, 2, (0, 0, 8, 4));
    assert_eq!(&big[(3 * 8 + 7) * 4..(3 * 8 + 7) * 4 + 4], &[7, 1, 3, 255], "the last physical pixel is pixel (3, 1)");
    assert_eq!(&big[2 * 4..2 * 4 + 4], &[7, 0, 1, 255], "physical (2, 0) is pixel (1, 0)");
    // Only the region is painted.
    let mut part = vec![0u8; 4 * 2 * 4];
    shot.paint(&mut part, 4, 2, 1, (2, 1, 9, 9));
    assert_eq!(&part[..4], &[0, 0, 0, 0], "outside the region");
    assert_eq!(&part[(4 + 2) * 4..(4 + 2) * 4 + 4], &[7, 1, 2, 255], "inside it");
    // A buffer too small for its size is left alone.
    let mut small = vec![0u8; 4];
    shot.paint(&mut small, 4, 2, 1, (0, 0, 4, 2));
    assert_eq!(small, [0, 0, 0, 0]);
}
