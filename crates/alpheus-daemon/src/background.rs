//! The bar's shapes: everything of a style that is not text.
//!
//! The per-style background and line passes that `Fonts::paint_bar` runs around the text pass, the
//! anti-aliased corner masks (cached per radius and scale, a few hundred bytes each), and the
//! opaque region each style claims, which is what lets a compositor skip what is behind an opaque
//! bar. Pixels are premultiplied ARGB8888, as `wl_shm` takes them (bytes B, G, R, A).
//!
//! Never:
//! - ask for blur or any other compositor effect: a layer rule of the user's does that
//!   (docs/features/themes.md);
//! - keep a mask that is not used: they are cached per radius and scale, and the memory suite's
//!   `startup_pss_per_style` and `style_switches_no_growth` measure what they cost;
//! - paint with anything but the theme's roles
//!   (`the_daemon_paints_with_roles_never_the_legacy_palette`).
//!
//! See: docs/features/themes.md ("Bar styles"), docs/measurements.md (the per-style figures and
//! the 48 switches).

use crate::grid::{Group, Style};
use alpheus_common::theme::settings::{BarStyle, ModuleColors};
use alpheus_common::theme::{Rgb, Roles};

/// What the bar's background pass needs besides the grid and the roles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Background<'a> {
    pub style: BarStyle,
    /// `translucent`'s bar alpha, 0.3..=1.0 (theme.toml's `opacity`).
    pub opacity: f32,
    pub module_colors: ModuleColors,
    /// The grid's blocks (`Grid::groups`).
    pub groups: &'a [Group],
}

/// A rectangle in logical px, surface-local: what `wl_region.add` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A bar surface's geometry: what turns its cells into logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarGeometry {
    /// Logical width of the bar.
    pub width: u32,
    /// Logical height of the bar (bar.toml's `height`): floating's shadow is not part of it.
    pub height: u32,
    /// Physical px per cell at `scale` (`Fonts::advance`).
    pub advance: f32,
    pub scale: i32,
}

/// Physical x of cell `col`'s left edge for cells `advance` physical px wide: the painter's own
/// rounding (`paint_cells`), shared by every pass and the opaque region.
pub fn cell_edge(col: u16, advance: f32) -> u32 {
    (f32::from(col) * advance).round().max(0.0) as u32
}

impl BarGeometry {
    /// Physical x of cell `col`'s left edge: the painter's own rounding (`cell_edge`).
    pub fn edge(&self, col: u16) -> u32 {
        cell_edge(col, self.advance)
    }

    /// Cells [start, end) as logical x, shrunk to whole logical px inside them (left edge rounded
    /// up, right edge down): an opaque claim never covers a pixel the cells only partly paint.
    pub fn inside(&self, start: u16, end: u16) -> (i32, i32) {
        let e = self.scale.max(1) as u32;
        let (left, right) = (self.edge(start).div_ceil(e), self.edge(end) / e);
        (left as i32, right.max(left) as i32)
    }
}

/// The opaque parts of a bar for `style` (shapes spec § 3), into `out` (cleared first, its capacity
/// kept: no allocation once it has held a bar's rects). `flat`, `underline` and `powerline` paint
/// every pixel opaque: the whole bar.
pub fn opaque_rects(style: BarStyle, groups: &[Group], geometry: &BarGeometry, out: &mut Vec<Rect>) {
    out.clear();
    match style {
        BarStyle::Flat | BarStyle::Underline | BarStyle::Powerline => {
            out.push(Rect { x: 0, y: 0, width: geometry.width as i32, height: geometry.height as i32 });
        }
        // Each block, inset (`island_y`) and shrunk to whole logical px (`inside`), as two rectangles
        // that leave its rounded corners out; the radius is the painter's own (`fitted_radius` over
        // the block's physical size), so the claim never covers a corner pixel.
        BarStyle::Islands | BarStyle::Translucent => {
            use crate::painter::blocks::{fitted_radius, island_radius, island_y};
            let e = geometry.scale.max(1) as u32;
            // The painter's physical rows, shrunk to whole logical px (a tiny bar's odd inset).
            let (y0, y1) = island_y(geometry.height.saturating_mul(e), geometry.scale);
            let (top, bottom) = (y0.div_ceil(e), y1 / e);
            let rows = bottom.saturating_sub(top);
            let radius = island_radius(geometry.height);
            for g in groups {
                let (x0, x1) = geometry.inside(g.start, g.end);
                let physical = geometry.edge(g.end).saturating_sub(geometry.edge(g.start));
                let r = fitted_radius(radius, physical, rows.saturating_mul(e), geometry.scale) as i32;
                let (w, h, y) = (x1 - x0, rows as i32, top as i32);
                if w > 2 * r && h > 0 {
                    out.push(Rect { x: x0 + r, y, width: w - 2 * r, height: h });
                }
                if h > 2 * r && w > 0 {
                    out.push(Rect { x: x0, y: y + r, width: w, height: h - 2 * r });
                }
            }
        }
        // The rounded bar without its corners, as two rectangles; the radius is the painter's own
        // (`floating::edges` fits `FLOAT_RADIUS` to the same physical size). `geometry.height` is
        // the bar alone: the shadow below it is never claimed.
        BarStyle::Floating => {
            use crate::painter::blocks::fitted_radius;
            use crate::painter::floating::FLOAT_RADIUS;
            let e = geometry.scale.max(1) as u32;
            let (pw, ph) = (geometry.width.saturating_mul(e), geometry.height.saturating_mul(e));
            let r = fitted_radius(FLOAT_RADIUS, pw, ph, geometry.scale) as i32;
            let (w, h) = (geometry.width as i32, geometry.height as i32);
            if w > 2 * r && h > 0 {
                out.push(Rect { x: r, y: 0, width: w - 2 * r, height: h });
            }
            if h > 2 * r && w > 0 {
                out.push(Rect { x: 0, y: r, width: w, height: h - 2 * r });
            }
        }
    }
}

/// The largest corner radius a mask is made for, physical px: a hostile scale cannot grow it past
/// 2 × 256 × 256 bytes.
pub const MAX_MASK_RADIUS: u32 = 256;

/// The anti-aliased coverage of a rounded corner, `radius` × `radius` physical px, for the
/// top-left quarter (the other three mirror it). `outer`: the corner's disc; `inner`: the same disc
/// shrunk by `stroke`, the inside edge of a border `stroke` px thick.
#[derive(Debug, PartialEq, Eq)]
pub struct CornerMask {
    pub radius: u32,
    pub stroke: u32,
    outer: Vec<u8>,
    inner: Vec<u8>,
}

impl CornerMask {
    fn new(radius: u32, stroke: u32) -> CornerMask {
        let n = radius as usize;
        let (mut outer, mut inner) = (vec![0u8; n * n], vec![0u8; n * n]);
        let (rf, sf) = (radius as f32, stroke as f32);
        let level = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        for cy in 0..n {
            for cx in 0..n {
                // The pixel's centre to the corner circle's centre (the same measure as `draw_frame`).
                let d = ((rf - cx as f32 - 0.5).powi(2) + (rf - cy as f32 - 0.5).powi(2)).sqrt();
                outer[cy * n + cx] = level(rf - d + 0.5);
                inner[cy * n + cx] = level(rf - sf - d + 0.5);
            }
        }
        CornerMask { radius, stroke, outer, inner }
    }

    /// The disc's coverage of pixel (`cx`, `cy`) of the top-left quarter; 0 outside the mask.
    pub fn outer_at(&self, cx: u32, cy: u32) -> u8 {
        self.at(&self.outer, cx, cy)
    }

    /// The shrunk disc's coverage: a border's ring is `outer_at - inner_at`.
    pub fn inner_at(&self, cx: u32, cy: u32) -> u8 {
        self.at(&self.inner, cx, cy)
    }

    fn at(&self, v: &[u8], cx: u32, cy: u32) -> u8 {
        if cx >= self.radius || cy >= self.radius {
            return 0;
        }
        v.get(cy as usize * self.radius as usize + cx as usize).copied().unwrap_or(0)
    }
}

/// Corner masks per (logical radius, scale), each created on its first use and kept: a style
/// that never draws a rounded shape never makes one, and a repaint reuses them (a few KB each).
///
/// Bound: the cache never evicts, its callers bound it. Every radius they ask is fitted
/// (`blocks::fitted_radius`) from the island radius (at most 8) or `floating::FLOAT_RADIUS` (10),
/// so 0..=10: at most 11 masks per scale seen (one or two per session), each
/// 2 × (radius × scale)² bytes with its physical radius capped at `MAX_MASK_RADIUS`. At scale 1
/// and 2 together that is at most 22 masks and about 4 KB.
#[derive(Debug, Default)]
pub struct MaskCache {
    masks: Vec<((u32, i32), CornerMask)>,
}

impl MaskCache {
    /// The mask for `radius` logical px at `scale` (stroke: one logical px), made on first use.
    pub fn get(&mut self, radius: u32, scale: i32) -> &CornerMask {
        let e = scale.max(1);
        let i = match self.masks.iter().position(|(key, _)| *key == (radius, e)) {
            Some(i) => i,
            None => {
                let phys = radius.saturating_mul(e as u32).min(MAX_MASK_RADIUS);
                self.masks.push(((radius, e), CornerMask::new(phys, (e as u32).min(phys))));
                self.masks.len() - 1
            }
        };
        &self.masks[i].1
    }

    /// Masks made so far.
    pub fn len(&self) -> usize {
        self.masks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.masks.is_empty()
    }
}

/// `c` at `alpha`, premultiplied, as the buffer's bytes (B, G, R, A).
pub fn premultiplied(Rgb(r, g, b): Rgb, alpha: u8) -> [u8; 4] {
    let m = |c: u8| ((u16::from(c) * u16::from(alpha) + 127) / 255) as u8;
    [m(b), m(g), m(r), alpha]
}

/// Writes `pixel` over x ∈ [x.0, x.1), y ∈ [y.0, y.1), clipped to the buffer (whatever was there
/// is replaced, alpha included).
pub fn fill_rect(px: &mut [u8], width: u32, height: u32, x: (u32, u32), y: (u32, u32), pixel: [u8; 4]) {
    for yy in y.0..y.1.min(height) {
        for xx in x.0..x.1.min(width) {
            let o = (yy as usize * width as usize + xx as usize) * 4;
            if let Some(dst) = px.get_mut(o..o + 4) {
                dst.copy_from_slice(&pixel);
            }
        }
    }
}

/// `underline`'s background (shapes spec § 3): the bar on `bar_bg`, each block one tonal step up on
/// `pill_bg`, square. `edge` is the painter's cell edge (physical px).
pub fn underline_blocks(px: &mut [u8], width: u32, height: u32, roles: &Roles, groups: &[Group], edge: impl Fn(u16) -> u32) {
    fill_rect(px, width, height, (0, width), (0, height), premultiplied(roles.bar_bg, 255));
    let pill = premultiplied(roles.pill_bg, 255);
    for g in groups {
        fill_rect(px, width, height, (edge(g.start), edge(g.end)), (0, height), pill);
    }
}

/// `underline`'s lines, over the cell backgrounds and under the text pass (shapes spec § 3): a one logical px bottom line in the
/// border colour across the bar, and two logical px of underline just above it: `accent` under the
/// active workspace (the `Style::ACTIVE` cells); under the module colours (icons, full, pills),
/// also each block's hue under the blocks that have one.
#[allow(clippy::too_many_arguments)]
pub fn underline_lines(
    px: &mut [u8],
    width: u32,
    height: u32,
    roles: &Roles,
    bg: &Background,
    active: impl Iterator<Item = (u16, u16)>,
    edge: impl Fn(u16) -> u32,
    scale: i32,
) {
    let s = scale.max(1) as u32;
    let line = (height.saturating_sub(s), height);
    let under = (height.saturating_sub(s.saturating_mul(3)), height.saturating_sub(s));
    fill_rect(px, width, height, (0, width), line, premultiplied(roles.border_over(roles.bar_bg), 255));
    if bg.module_colors != ModuleColors::None {
        for g in bg.groups {
            if let Some(h) = g.hue {
                let hue = crate::painter::color(roles, roles.bar_bg, crate::grid::Tint::Hue(h));
                fill_rect(px, width, height, (edge(g.start), edge(g.end)), under, premultiplied(hue, 255));
            }
        }
    }
    let accent = premultiplied(roles.accent, 255);
    for (start, end) in active {
        fill_rect(px, width, height, (edge(start), edge(end)), under, accent);
    }
}

/// The runs of `Style::ACTIVE` cells in `cells`, as [start, end) columns: the active workspace.
pub fn active_runs(cells: &[crate::grid::Cell]) -> impl Iterator<Item = (u16, u16)> + '_ {
    let mut col = 0usize;
    std::iter::from_fn(move || {
        while col < cells.len() && cells[col].style != Style::ACTIVE {
            col += 1;
        }
        if col >= cells.len() {
            return None;
        }
        let start = col;
        while col < cells.len() && cells[col].style == Style::ACTIVE {
            col += 1;
        }
        Some((start as u16, col as u16))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Cell, Group, GroupKind};

    const GEOMETRY: BarGeometry = BarGeometry { width: 1280, height: 22, advance: 8.4, scale: 1 };

    // ---- the opaque region ----

    #[test]
    fn flat_underline_and_powerline_are_opaque_over_the_whole_bar() {
        let groups = [Group { kind: GroupKind::Clock, start: 3, end: 9, hue: None }];
        let mut out = Vec::new();
        for style in [BarStyle::Flat, BarStyle::Underline, BarStyle::Powerline] {
            opaque_rects(style, &groups, &GEOMETRY, &mut out);
            assert_eq!(out, [Rect { x: 0, y: 0, width: 1280, height: 22 }], "{style:?}");
        }
        let scaled = BarGeometry { width: 960, height: 30, advance: 16.8, scale: 2 };
        opaque_rects(BarStyle::Flat, &groups, &scaled, &mut out);
        assert_eq!(out, [Rect { x: 0, y: 0, width: 960, height: 30 }], "logical px, not physical");
    }

    /// Spec § 3: floating claims its rounded bar without the corners, in logical px; `geometry.height`
    /// is the bar alone, so the shadow below it is never claimed.
    #[test]
    fn floating_claims_its_bar_without_the_corners_and_never_the_shadow() {
        let mut out = Vec::new();
        let want = [Rect { x: 10, y: 0, width: 1248, height: 22 }, Rect { x: 0, y: 10, width: 1268, height: 2 }];
        opaque_rects(BarStyle::Floating, &[], &BarGeometry { width: 1268, height: 22, advance: 8.4, scale: 1 }, &mut out);
        assert_eq!(out, want, "radius 10");
        opaque_rects(BarStyle::Floating, &[], &BarGeometry { width: 1268, height: 22, advance: 16.8, scale: 2 }, &mut out);
        assert_eq!(out, want, "logical px at scale 2");
        opaque_rects(BarStyle::Floating, &[], &BarGeometry { width: 12, height: 22, advance: 8.4, scale: 1 }, &mut out);
        assert_eq!(out, [Rect { x: 0, y: 6, width: 12, height: 10 }], "a bar too narrow for its radius: what is left");
    }

    /// Spec § 3: islands and translucent claim their blocks, inset, without the rounded corners; in
    /// logical px at every scale; nothing without a block.
    #[test]
    fn islands_and_translucent_claim_their_blocks_without_the_corners() {
        let groups = [Group { kind: GroupKind::Clock, start: 10, end: 20, hue: None }];
        // Cells 10..20 at 8 px: x 80..160; rows 2..20; radius 8.
        let want = [Rect { x: 88, y: 2, width: 64, height: 18 }, Rect { x: 80, y: 10, width: 80, height: 2 }];
        let mut out = Vec::new();
        for style in [BarStyle::Islands, BarStyle::Translucent] {
            opaque_rects(style, &groups, &BarGeometry { width: 1280, height: 22, advance: 8.0, scale: 1 }, &mut out);
            assert_eq!(out, want, "{style:?}");
        }
        opaque_rects(BarStyle::Islands, &groups, &BarGeometry { width: 640, height: 22, advance: 16.0, scale: 2 }, &mut out);
        assert_eq!(out, want, "logical px at scale 2");
        opaque_rects(BarStyle::Islands, &[], &GEOMETRY, &mut out);
        assert_eq!(out, [], "no block, nothing claimed");
        // A block narrower than its corners: the fitted radius, never a negative or a corner claimed.
        let narrow = [Group { kind: GroupKind::Clock, start: 10, end: 12, hue: None }];
        opaque_rects(BarStyle::Islands, &narrow, &BarGeometry { width: 1280, height: 22, advance: 8.0, scale: 1 }, &mut out);
        // Cells 10..12: x 80..96, radius min(8, 16 / 2, 18 / 2) = 8: no straight top edge left.
        assert_eq!(out, [Rect { x: 80, y: 10, width: 16, height: 2 }]);
    }

    /// `opaque_rects` reuses `out`: no allocation per frame once it held a bar's rects.
    #[test]
    fn the_opaque_rects_reuse_their_vector() {
        let mut out = Vec::new();
        opaque_rects(BarStyle::Flat, &[], &GEOMETRY, &mut out);
        let (ptr, cap) = (out.as_ptr(), out.capacity());
        for _ in 0..10 {
            opaque_rects(BarStyle::Underline, &[], &GEOMETRY, &mut out);
        }
        assert_eq!((out.as_ptr(), out.capacity()), (ptr, cap));
    }

    #[test]
    fn cells_become_whole_logical_px_inside_them() {
        let g = BarGeometry { width: 100, height: 22, advance: 8.4, scale: 1 };
        assert_eq!((g.edge(0), g.edge(1), g.edge(5)), (0, 8, 42));
        assert_eq!(g.inside(1, 5), (8, 42));
        let g2 = BarGeometry { width: 100, height: 22, advance: 16.9, scale: 2 };
        // Physical 17 and 85: logical 8.5 and 42.5, shrunk to 9 and 42.
        assert_eq!((g2.edge(1), g2.edge(5)), (17, 85));
        assert_eq!(g2.inside(1, 5), (9, 42));
        assert_eq!(g2.inside(5, 5), (43, 43), "an empty run stays empty, never negative");
    }

    // ---- corner masks ----

    /// The top-left quarter of an 8 px corner: transparent at the very corner, opaque inside, a
    /// partial coverage on the arc; symmetric about its diagonal.
    #[test]
    fn a_corner_mask_is_anti_aliased_on_its_arc() {
        let mut cache = MaskCache::default();
        let m = cache.get(8, 1);
        assert_eq!((m.radius, m.stroke), (8, 1));
        assert_eq!((m.outer_at(0, 0), m.outer_at(1, 1)), (0, 0), "outside the disc");
        let arc = m.outer_at(2, 2);
        assert!(arc > 0 && arc < 255, "the arc is anti-aliased: {arc}");
        assert_eq!((m.outer_at(3, 3), m.outer_at(7, 7), m.outer_at(7, 1)), (255, 255, 255));
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(m.outer_at(x, y), m.outer_at(y, x), "({x}, {y})");
                assert!(m.inner_at(x, y) <= m.outer_at(x, y), "the inner disc is inside the outer");
            }
        }
        assert_eq!(m.inner_at(2, 2), 0, "one px in: the border's ring there");
        assert_eq!(m.inner_at(4, 4), 255);
        assert_eq!(m.outer_at(8, 0), 0, "past the mask: nothing");
    }

    #[test]
    fn a_mask_at_scale_2_is_twice_as_large_with_a_two_px_stroke() {
        let mut cache = MaskCache::default();
        let m = cache.get(8, 2);
        assert_eq!((m.radius, m.stroke), (16, 2));
        assert_eq!(m.outer_at(0, 0), 0);
        assert!((0..16).any(|i| (1..255).contains(&m.outer_at(i, i))), "an anti-aliased arc");
        assert_eq!(m.outer_at(15, 15), 255);
    }

    /// Created on first use, one per (radius, scale), then reused as is: no allocation per frame.
    #[test]
    fn masks_are_cached_per_radius_and_scale() {
        let mut cache = MaskCache::default();
        assert!(cache.is_empty(), "nothing before the first use");
        let first = cache.get(8, 1).outer.as_ptr();
        assert_eq!(cache.get(8, 1).outer.as_ptr(), first, "the same mask again");
        assert_eq!(cache.len(), 1);
        cache.get(8, 2);
        cache.get(10, 1);
        assert_eq!(cache.len(), 3, "another scale, another radius: one more each");
        assert_eq!(cache.get(8, 1).outer.as_ptr(), first, "still the first one");
        cache.get(8, 0);
        assert_eq!(cache.len(), 3, "scale 0 is scale 1");
    }

    #[test]
    fn degenerate_masks_never_panic_nor_grow() {
        let mut cache = MaskCache::default();
        let m = cache.get(0, 1);
        assert_eq!((m.radius, m.outer_at(0, 0)), (0, 0));
        let huge = cache.get(10, 100_000);
        assert_eq!(huge.radius, MAX_MASK_RADIUS, "capped");
        assert_eq!(cache.get(u32::MAX, i32::MAX).radius, MAX_MASK_RADIUS);
        assert_eq!(cache.len(), 3, "one entry per (radius, scale) asked, whatever its size");
        cache.get(u32::MAX, i32::MAX);
        assert_eq!(cache.len(), 3, "asked again: reused");
    }

    // ---- fills ----

    #[test]
    fn every_pixel_written_is_premultiplied() {
        assert_eq!(premultiplied(Rgb(255, 128, 0), 255), [0, 128, 255, 255]);
        assert_eq!(premultiplied(Rgb(255, 128, 0), 128), [0, 64, 128, 128]);
        assert_eq!(premultiplied(Rgb(255, 255, 255), 0), [0, 0, 0, 0]);
    }

    #[test]
    fn active_runs_are_the_active_workspace_cells() {
        let c = |style| Cell { ch: ' ', style };
        let cells = [c(Style::NORMAL), c(Style::ACTIVE), c(Style::ACTIVE), c(Style::URGENT), c(Style::ACTIVE)];
        assert_eq!(active_runs(&cells).collect::<Vec<_>>(), [(1, 3), (4, 5)]);
        assert_eq!(active_runs(&[]).count(), 0);
    }
}
