//! The `floating` pass (spec § 3). The surface is the bar's band plus a shadow below it
//! (`placement::FLOAT_SHADOW`):
//! - the band is one rounded rectangle on `bar_bg`, radius 10 px, transparent outside its arcs;
//! - under pills, each hued block is a rounded hue rectangle inside it, inset as an island
//!   (`blocks::island_y`, islands radius, no border);
//! - the shadow is black, its alpha falling from 0.35 to 0 downward, following the bar's rounded
//!   silhouette horizontally.

use super::Background;
use super::blocks::{CLEAR, block_fill, block_x, fill_px, fitted_radius, island_radius, island_y, opaque, pills, round_block};
use crate::background::{CornerMask, MaskCache};
use alpheus_common::theme::Roles;

/// Logical px radius of the floating bar's ends (spec § 3).
pub(crate) const FLOAT_RADIUS: u32 = 10;
/// The shadow's alpha right below the bar (spec § 3).
pub(crate) const SHADOW_ALPHA: f32 = 0.35;

/// The shadow's alpha on row `k` of `rows`: 0.35 on the first, falling linearly to 0 on the last.
pub(crate) fn shadow_alpha(k: u32, rows: u32) -> f32 {
    match rows {
        0 => 0.0,
        1 => SHADOW_ALPHA * f32::from(u8::from(k == 0)),
        // The ratio first: (rows − 1) / (rows − 1) is exactly 1.0, so the first row is exactly 0.35.
        _ => SHADOW_ALPHA * (rows.saturating_sub(1).saturating_sub(k) as f32 / (rows - 1) as f32),
    }
}

/// Background pass, the band only: opaque `bar_bg`, and under pills each hued block a plain hue
/// rectangle in the island rows (rounded by `edges`).
pub(crate) fn background(px: &mut [u8], width: u32, band: u32, scale: i32, advance: f32, roles: &Roles, bg: &Background) {
    fill_px(px, width, band, (0, width), (0, band), opaque(roles.bar_bg));
    if pills(bg) {
        let rows = island_y(band, scale);
        for g in bg.groups.iter().filter(|g| g.hue.is_some()) {
            fill_px(px, width, band, block_x(g, advance), rows, opaque(block_fill(roles, bg, g, roles.bar_bg)));
        }
    }
}

/// Edges pass, over the cell backgrounds and under the glyphs (no glyph pixel is ever erased),
/// over the whole surface (`height` = band + shadow):
/// - the pills blocks rounded against `bar_bg`;
/// - the bar's ends rounded against transparency;
/// - the shadow under it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn edges(
    masks: &mut MaskCache,
    px: &mut [u8],
    width: u32,
    height: u32,
    band: u32,
    scale: i32,
    advance: f32,
    roles: &Roles,
    bg: &Background,
) {
    let e = scale.max(1) as u32;
    if pills(bg) {
        let radius = island_radius(band / e);
        let (top, bottom) = island_y(band, scale);
        for g in bg.groups.iter().filter(|g| g.hue.is_some()) {
            let x = block_x(g, advance);
            let mask = masks.get(fitted_radius(radius, x.1.saturating_sub(x.0), bottom - top, scale), scale);
            round_block(px, width, band, x, (top, bottom), mask, opaque(roles.bar_bg), None);
        }
    }
    let mask = masks.get(fitted_radius(FLOAT_RADIUS, width, band, scale), scale);
    round_block(px, width, band, (0, width), (0, band), mask, CLEAR, None);
    shadow(px, width, height, band, mask);
}

/// How much of column `x` the bar's bottom row covers: its rounded ends' outermost row, 255 between
/// them. The shadow follows it, so no square shadow sits under a rounded end.
fn silhouette(x: u32, width: u32, mask: &CornerMask) -> u8 {
    let side = mask.radius;
    let cx = if x < side {
        x
    } else if x >= width.saturating_sub(side) {
        width.saturating_sub(1).saturating_sub(x)
    } else {
        return 255;
    };
    // Row 0 of the top-left quarter: mirrored, the bottom row's outermost coverage.
    mask.outer_at(cx, 0)
}

/// Rows [band, height): premultiplied black at `shadow_alpha`, times the bar's silhouette. Every
/// pixel there is replaced: those rows hold nothing else.
fn shadow(px: &mut [u8], width: u32, height: u32, band: u32, mask: &CornerMask) {
    let rows = height.saturating_sub(band);
    for k in 0..rows {
        let alpha = shadow_alpha(k, rows);
        let row = (band + k) as usize * width as usize;
        for x in 0..width {
            let a = (alpha * f32::from(silhouette(x, width, mask))).round() as u8;
            let o = (row + x as usize) * 4;
            if let Some(d) = px.get_mut(o..o + 4) {
                d.copy_from_slice(&[0, 0, 0, a]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shadow_alpha_falls_from_0_35_to_0() {
        assert_eq!(shadow_alpha(0, 4), 0.35);
        assert_eq!(shadow_alpha(3, 4), 0.0);
        assert!(shadow_alpha(1, 4) > shadow_alpha(2, 4) && shadow_alpha(2, 4) > 0.0);
        assert_eq!(shadow_alpha(7, 8), 0.0, "scale 2: 8 rows, the last one 0");
        assert_eq!(shadow_alpha(0, 1), 0.35, "a single row");
        assert_eq!(shadow_alpha(0, 0), 0.0, "no room: nothing");
        assert_eq!(shadow_alpha(9, 4), 0.0, "past the end: nothing, no underflow");
    }
}
