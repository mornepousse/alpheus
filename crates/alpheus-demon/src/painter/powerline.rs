//! The `powerline` pass (spec § 3):
//! - full-height blocks on an opaque `bar_bg`: the workspaces on `accent_container`, the others
//!   alternating `pill_bg` and `raised`, or their hue under pills;
//! - between them, a one-cell arrow drawn as an analytically anti-aliased triangle in the colour
//!   of the block it comes from, never a Nerd Font glyph. Left-side blocks (ending at or before
//!   `Grid::split`, reconciliation 2) point right, from the free cell after them; right-side blocks
//!   point left, from the free cell before them.

use super::Background;
use super::blocks::{Px, block_fill, block_x, fill_px, opaque, over};
use crate::background::cell_edge;
use crate::grid::{Group, GroupKind};
use alpheus_commun::theme::{Rgb, Roles};

/// Which way an arrow points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Point {
    Right,
    Left,
}

/// Coverage (0.0..=1.0) at (x, y) of the right-pointing triangle with its base on x = 0 (from
/// y = 0 to y = h) and its tip at (w, h / 2). Each slanted edge's coverage is its signed distance
/// to the point, plus one half, clamped; the triangle's is the smaller of the two. The base is
/// pixel-aligned and needs none.
pub(crate) fn arrow_coverage(x: f32, y: f32, w: f32, h: f32) -> f32 {
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let half = h / 2.0;
    let len = (half * half + w * w).sqrt();
    let upper = (y * w - x * half) / len;
    let lower = ((h - y) * w - x * half) / len;
    (upper + 0.5).clamp(0.0, 1.0).min((lower + 0.5).clamp(0.0, 1.0))
}

/// The arrow in the cell x ∈ [x.0, x.1), the band's full height, blended over what is there.
fn arrow(px: &mut [u8], width: u32, height: u32, x: (u32, u32), point: Point, ink: Rgb) {
    let (w, h, src) = (x.1.saturating_sub(x.0) as f32, height as f32, opaque(ink));
    for yy in 0..height {
        for xx in x.0..x.1.min(width) {
            let local = (xx - x.0) as f32 + 0.5;
            let along = match point {
                Point::Right => local,
                Point::Left => w - local,
            };
            let k = (arrow_coverage(along, yy as f32 + 0.5, w, h) * 255.0).round() as u8;
            if k == 0 {
                continue;
            }
            let o = (yy as usize * width as usize + xx as usize) * 4;
            if let Some(d) = px.get_mut(o..o + 4) {
                let p: Px = over(src, [d[0], d[1], d[2], d[3]], k);
                d.copy_from_slice(&p);
            }
        }
    }
}

/// Whether no group covers cell `col`.
fn free(groups: &[Group], col: u16) -> bool {
    !groups.iter().any(|g| g.start <= col && col < g.end)
}

/// Background pass of `powerline`:
/// - the band opaque `bar_bg`;
/// - each group a full-height block: workspaces on `accent_container`, the others alternating
///   `pill_bg` and `raised` (the k-th other block in order: pill_bg first), each the hue of its
///   first module under pills;
/// - the block's arrow in its free neighbouring cell: after it, pointing right, when the block
///   ends at or before `split` (the grid's right-hand side, `Grid::split`); before it, pointing
///   left, otherwise.
///
/// When one free cell lies between a left-side and a right-side block, the first arrow drawn
/// there, the left block's, keeps it.
pub(crate) fn background(px: &mut [u8], width: u32, height: u32, advance: f32, roles: &Roles, bg: &Background, split: u16) {
    fill_px(px, width, height, (0, width), (0, height), opaque(roles.bar_bg));
    let columns = super::columns(width, advance);
    let (mut others, mut last_arrow) = (0usize, None);
    for g in bg.groups {
        let base = if g.kind == GroupKind::Workspaces {
            roles.accent_container
        } else {
            others += 1;
            if others % 2 == 1 { roles.pill_bg } else { roles.raised }
        };
        let fill = block_fill(roles, bg, g, base);
        fill_px(px, width, height, block_x(g, advance), (0, height), opaque(fill));
        let (cell, point) = if g.end <= split { (Some(g.end), Point::Right) } else { (g.start.checked_sub(1), Point::Left) };
        let Some(cell) = cell else { continue };
        if cell < columns && free(bg.groups, cell) && last_arrow != Some(cell) {
            arrow(px, width, height, (cell_edge(cell, advance), cell_edge(cell + 1, advance)), point, fill);
            last_arrow = Some(cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W = 10, h = 20: the edges run from (0, 0) and (0, 20) to the tip (10, 10).
    #[test]
    fn arrow_coverage_is_one_inside_half_on_an_edge_and_zero_outside() {
        let (w, h) = (10.0, 20.0);
        assert_eq!(arrow_coverage(0.5, 10.0, w, h), 1.0, "by the base, on the axis");
        assert_eq!(arrow_coverage(5.5, 5.5, w, h), 0.5, "centred on the upper edge");
        assert_eq!(arrow_coverage(5.5, 14.5, w, h), 0.5, "centred on the lower edge");
        assert_eq!(arrow_coverage(9.5, 9.5, w, h), 0.5, "the tip");
        assert_eq!(arrow_coverage(9.5, 0.5, w, h), 0.0, "beyond the upper edge");
        assert_eq!(arrow_coverage(9.5, 19.5, w, h), 0.0, "beyond the lower edge");
        for (x, y) in [(2.5, 3.0), (7.5, 6.5), (4.0, 9.0)] {
            assert_eq!(arrow_coverage(x, y, w, h), arrow_coverage(x, h - y, w, h), "symmetric about the axis at ({x}, {y})");
        }
    }

    #[test]
    fn a_degenerate_arrow_covers_nothing() {
        assert_eq!(arrow_coverage(0.5, 0.5, 0.0, 20.0), 0.0);
        assert_eq!(arrow_coverage(0.5, 0.5, 10.0, 0.0), 0.0);
    }
}
