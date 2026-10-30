//! The block styles' passes (themes delivery 2, spec § 3): `islands` and `translucent` (the bar
//! transparent or `bar_bg` at `opacity`, each group a rounded `pill_bg` block with a 1 px border,
//! inset 2 px from the bar's top and bottom), the `pills` fill and its text colour, and the
//! premultiplied pixel helpers the powerline and floating passes share. Every pixel written here
//! is premultiplied ARGB8888 (wl_shm: B, G, R, A).

use super::{Background, color};
use crate::background::{CornerMask, MaskCache, cell_edge, premultiplied};
use crate::config::ModuleId;
use crate::grid::{Group, Style, Tint};

use alpheus_commun::theme::contrast::ratio;
use alpheus_commun::theme::settings::{BarStyle, ModuleColors};
use alpheus_commun::theme::{Rgb, Roles};

/// A premultiplied ARGB8888 pixel in wl_shm byte order: B, G, R, A.
pub(crate) type Px = [u8; 4];

/// Fully transparent.
pub(crate) const CLEAR: Px = [0, 0, 0, 0];

/// The pills inks `pill_inks` keeps, on the stack: one per hued module. A block's hue is its first
/// module's (`ModuleSpec::hue`) and bar.toml rejects a duplicate module, so a bar never has more
/// hued blocks than hued modules. Counted from the modules, a module given a hue later grows it.
pub(crate) const MAX_BLOCKS: usize = hued_modules();

const fn hued_modules() -> usize {
    let (mut n, mut i) = (0, 0);
    while i < ModuleId::ALL.len() {
        if ModuleId::ALL[i].spec().hue.is_some() {
            n += 1;
        }
        i += 1;
    }
    n
}

/// Logical px cap of an island's corner radius (spec § 3).
const ISLAND_RADIUS_CAP: u32 = 8;

/// Logical px between an island block and the bar's top and bottom edges (the
/// mockup): with the radius formula, a block on the default 22 px bar is a pill.
pub(crate) const ISLAND_INSET: u32 = 2;

/// The plain fill (`background::fill_rect`), under the name the block passes use.
pub(crate) use crate::background::fill_rect as fill_px;

/// The pills text colours over the cells [start, end) of one block: `ink` for normal text, `muted`
/// for a muted cell (`Tint::FgDim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OnBlock {
    pub start: u16,
    pub end: u16,
    pub ink: Rgb,
    pub muted: Rgb,
}

pub(crate) fn opaque(c: Rgb) -> Px {
    premultiplied(c, 0xff)
}

/// `translucent`'s `opacity` as the alpha `background::premultiplied` takes (one
/// premultiply helper): round(opacity × 255), clamped; a value that is not a number is opaque.
pub(crate) fn opacity_alpha(opacity: f32) -> u8 {
    if opacity.is_nan() {
        return 0xff;
    }
    (opacity.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `src` with coverage `cov` (0..=255) over `dst`, both premultiplied: a per-channel mix.
pub(crate) fn over(src: Px, dst: Px, cov: u8) -> Px {
    let k = u32::from(cov);
    let f = |s: u8, d: u8| ((u32::from(s) * k + u32::from(d) * (255 - k) + 127) / 255) as u8;
    [f(src[0], dst[0]), f(src[1], dst[1]), f(src[2], dst[2]), f(src[3], dst[3])]
}

/// An island's corner radius, logical px, for a bar `bar_height` logical px tall (spec § 3).
pub(crate) fn island_radius(bar_height: u32) -> u32 {
    (bar_height / 2).saturating_sub(2).min(ISLAND_RADIUS_CAP)
}

/// An island block's physical rows [top, bottom) in a band `height` physical px tall at `scale`:
/// `ISLAND_INSET` logical px off each edge, never past the middle.
pub(crate) fn island_y(height: u32, scale: i32) -> (u32, u32) {
    let top = ISLAND_INSET.saturating_mul(scale.max(1) as u32).min(height / 2);
    (top, height - top)
}

/// The logical radius a block `w` × `h` physical px can take at `scale`: `radius`, or less when
/// the block is too small for it (0 for a degenerate block).
pub(crate) fn fitted_radius(radius: u32, w: u32, h: u32, scale: i32) -> u32 {
    let e = scale.max(1) as u32;
    let d = e.saturating_mul(2);
    radius.min(w / d).min(h / d)
}

/// A group's physical x span: its cells' edges, rounded as `paint_cells` rounds them.
pub(crate) fn block_x(g: &Group, advance: f32) -> (u32, u32) {
    (cell_edge(g.start, advance), cell_edge(g.end, advance))
}

/// Whether `pills` applies: only under the block styles (spec § 1). Under flat and underline the
/// layout gives it `full`'s look, and the painter does nothing more.
pub(crate) fn pills(bg: &Background) -> bool {
    bg.module_colors == ModuleColors::Pills && !matches!(bg.style, BarStyle::Flat | BarStyle::Underline)
}

/// A block's fill: its first module's hue under pills, `otherwise` (the style's own) else.
pub(crate) fn block_fill(roles: &Roles, bg: &Background, g: &Group, otherwise: Rgb) -> Rgb {
    match g.hue {
        Some(h) if pills(bg) => color(roles, roles.bar_bg, Tint::Hue(h)),
        _ => otherwise,
    }
}

/// The text on a pill of `fill`: whichever of `bar_bg` and `fg` contrasts more with it (WCAG 2
/// ratio); `bar_bg` on a tie.
pub(crate) fn on_fill(roles: &Roles, fill: Rgb) -> Rgb {
    if ratio(roles.bar_bg, fill) >= ratio(roles.fg, fill) { roles.bar_bg } else { roles.fg }
}

/// A muted cell's ink on a pill (Mae, 2026-10-08): the pill's `ink` mixed 50 % toward its `fill`,
/// paler than normal text, as `fg_dim` is beside `fg` on the bar.
pub(crate) fn muted_on_fill(ink: Rgb, fill: Rgb) -> Rgb {
    let half = |a: u8, b: u8| (u16::from(a) + u16::from(b)).div_ceil(2) as u8;
    Rgb(half(ink.0, fill.0), half(ink.1, fill.1), half(ink.2, fill.2))
}

/// A cell whose colours win over a pill's ink: warn, error (attention), urgent, or any cell with
/// its own background (the active workspace on `accent_container`).
pub(crate) fn in_state(s: Style) -> bool {
    s.bg != Tint::Bg || matches!(s.fg, Tint::Warn | Tint::Error | Tint::Urgent)
}

/// The pills ink of every hued block, on the stack (no allocation per frame); none when pills does
/// not apply, so the text pass is then exactly today's.
pub(crate) fn pill_inks(roles: &Roles, bg: &Background) -> ([OnBlock; MAX_BLOCKS], usize) {
    let mut on = [OnBlock { start: 0, end: 0, ink: roles.fg, muted: roles.fg_dim }; MAX_BLOCKS];
    let mut n = 0;
    if pills(bg) {
        for g in bg.groups {
            let Some(h) = g.hue else { continue };
            if n == MAX_BLOCKS {
                break;
            }
            let fill = color(roles, roles.bar_bg, Tint::Hue(h));
            let ink = on_fill(roles, fill);
            on[n] = OnBlock { start: g.start, end: g.end, ink, muted: muted_on_fill(ink, fill) };
            n += 1;
        }
    }
    (on, n)
}

/// A corner pixel: `painted` where the inner disc covers it, `ink` on the ring between the inner
/// and outer edges, `outside` beyond the outer one.
fn ring(painted: Px, ink: Px, outside: Px, outer: u8, inner: u8) -> Px {
    let (o, i) = (u32::from(outer), u32::from(inner.min(outer)));
    let f = |p: u8, k: u8, b: u8| ((u32::from(p) * i + u32::from(k) * (o - i) + u32::from(b) * (255 - o) + 127) / 255) as u8;
    [
        f(painted[0], ink[0], outside[0]),
        f(painted[1], ink[1], outside[1]),
        f(painted[2], ink[2], outside[2]),
        f(painted[3], ink[3], outside[3]),
    ]
}

/// Rounds the block x ∈ [x.0, x.1) × y ∈ [y.0, y.1) (physical px) once its inside is painted:
/// - the four corners blended toward `outside` beyond `mask`'s arc;
/// - with `border` (colour, stroke px), a ring along the edge: straight strokes and anti-aliased
///   corner rings.
///
/// A mask larger than half the block draws no corner (callers fit it: `fitted_radius`). Pixels
/// beyond the buffer are skipped, never wrapped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn round_block(
    px: &mut [u8],
    width: u32,
    height: u32,
    x: (u32, u32),
    y: (u32, u32),
    mask: &CornerMask,
    outside: Px,
    border: Option<(Px, u32)>,
) {
    let (w, h) = (x.1.saturating_sub(x.0), y.1.saturating_sub(y.0));
    if w == 0 || h == 0 {
        return;
    }
    let r = if mask.radius <= w / 2 && mask.radius <= h / 2 { mask.radius } else { 0 };
    if let Some((ink, stroke)) = border {
        let s = stroke.min(w / 2).min(h / 2);
        fill_px(px, width, height, (x.0 + r, x.1 - r), (y.0, y.0 + s), ink);
        fill_px(px, width, height, (x.0 + r, x.1 - r), (y.1 - s, y.1), ink);
        fill_px(px, width, height, (x.0, x.0 + s), (y.0 + r, y.1 - r), ink);
        fill_px(px, width, height, (x.1 - s, x.1), (y.0 + r, y.1 - r), ink);
    }
    for cy in 0..r {
        for cx in 0..r {
            // The corner mask, top-left quarter; the other three corners mirror it.
            let (outer, inner) = (mask.outer_at(cx, cy), mask.inner_at(cx, cy));
            for (xx, yy) in [(x.0 + cx, y.0 + cy), (x.1 - 1 - cx, y.0 + cy), (x.0 + cx, y.1 - 1 - cy), (x.1 - 1 - cx, y.1 - 1 - cy)] {
                if xx >= width || yy >= height {
                    continue;
                }
                let o = (yy as usize * width as usize + xx as usize) * 4;
                let Some(d) = px.get_mut(o..o + 4) else { continue };
                let painted = [d[0], d[1], d[2], d[3]];
                let p = match border {
                    Some((ink, _)) => ring(painted, ink, outside, outer, inner),
                    None => over(painted, outside, outer),
                };
                d.copy_from_slice(&p);
            }
        }
    }
}

/// Background pass of `islands` (`outside` = `CLEAR`) and `translucent` (`outside` = `bar_bg`
/// at `opacity`):
/// - the whole band `outside`;
/// - each group a plain block in `pill_bg`, or its hue under pills, in the rows `island_y` gives
///   (inset 2 logical px top and bottom).
///
/// The cell backgrounds then go in those rows only, and `round_islands` rounds and
/// borders the blocks before the glyphs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn background(px: &mut [u8], width: u32, height: u32, scale: i32, advance: f32, roles: &Roles, bg: &Background, outside: Px) {
    fill_px(px, width, height, (0, width), (0, height), outside);
    let rows = island_y(height, scale);
    for g in bg.groups {
        fill_px(px, width, height, block_x(g, advance), rows, opaque(block_fill(roles, bg, g, roles.pill_bg)));
    }
}

/// Edges pass of `islands` and `translucent`, over the cell backgrounds and under the glyphs (no
/// glyph pixel is ever erased): each block's corners rounded to the island radius
/// (cached mask per radius and scale), and a 1 logical px border in `border_over` its fill.
#[allow(clippy::too_many_arguments)]
pub(crate) fn round_islands(
    masks: &mut MaskCache,
    px: &mut [u8],
    width: u32,
    height: u32,
    scale: i32,
    advance: f32,
    roles: &Roles,
    bg: &Background,
    outside: Px,
) {
    let e = scale.max(1) as u32;
    let radius = island_radius(height / e);
    let (top, bottom) = island_y(height, scale);
    for g in bg.groups {
        let x = block_x(g, advance);
        let fill = block_fill(roles, bg, g, roles.pill_bg);
        let mask = masks.get(fitted_radius(radius, x.1.saturating_sub(x.0), bottom - top, scale), scale);
        round_block(px, width, height, x, (top, bottom), mask, outside, Some((opaque(roles.border_over(fill)), e)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Hue;

    /// One premultiply helper (`background::premultiplied`, a `u8` alpha); the
    /// opacity becomes that alpha rounded to nearest.
    #[test]
    fn opacity_becomes_an_alpha_rounded_to_nearest() {
        assert_eq!(opacity_alpha(0.5), 128, "round(127.5)");
        assert_eq!(opacity_alpha(0.3), 77, "round(76.5)");
        assert_eq!(opacity_alpha(1.0), 255);
        assert_eq!(opacity_alpha(0.0), 0);
        assert_eq!(opacity_alpha(7.0), 255, "clamped");
        assert_eq!(opacity_alpha(f32::NAN), 255, "never a transparent bar from a bad value");
        assert_eq!(premultiplied(Rgb(200, 100, 50), opacity_alpha(0.5)), [25, 50, 100, 128], "B, G, R halved");
    }

    #[test]
    fn over_mixes_premultiplied_pixels_by_coverage() {
        assert_eq!(over([255, 255, 255, 255], CLEAR, 255), [255, 255, 255, 255]);
        assert_eq!(over([255, 255, 255, 255], CLEAR, 0), CLEAR);
        assert_eq!(over([200, 100, 0, 255], CLEAR, 128), [100, 50, 0, 128]);
        assert_eq!(over([0, 0, 0, 255], [200, 200, 200, 255], 128), [100, 100, 100, 255], "opaque over opaque stays opaque");
    }

    fn rgba(px: &[u8], w: u32, x: u32, y: u32) -> Px {
        let o = ((y * w + x) * 4) as usize;
        [px[o], px[o + 1], px[o + 2], px[o + 3]]
    }

    /// A block rounded toward a transparent bar: its four corners transparent, its arcs partly
    /// covered, its straight edges and inside its own pixel, premultiplied; at scales 1 and 2.
    #[test]
    fn a_rounded_block_has_transparent_corners_and_an_opaque_inside() {
        for e in [1u32, 2] {
            let mut cache = MaskCache::default();
            let m = cache.get(8, e as i32);
            let (w, h) = (40 * e, 22 * e);
            let pill = premultiplied(Rgb(10, 200, 30), 255);
            let mut px = vec![0u8; (w * h * 4) as usize];
            fill_px(&mut px, w, h, (0, w), (0, h), pill);
            round_block(&mut px, w, h, (0, w), (0, h), m, CLEAR, None);
            for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
                assert_eq!(rgba(&px, w, x, y), CLEAR, "corner ({x}, {y}) at scale {e}");
            }
            assert_eq!(rgba(&px, w, w / 2, h / 2), pill, "inside");
            assert_eq!(rgba(&px, w, w / 2, 0), pill, "the straight top edge");
            assert_eq!(rgba(&px, w, 0, h / 2), pill, "the straight left edge");
            let arc = (0..8 * e).map(|i| rgba(&px, w, i, i)[3]).find(|a| *a > 0 && *a < 255);
            assert!(arc.is_some(), "an anti-aliased arc at scale {e}");
            let a = rgba(&px, w, 2 * e, 2 * e);
            assert!(a[0] <= a[3] && a[1] <= a[3] && a[2] <= a[3], "premultiplied: {a:?}");
        }
    }

    /// Toward a translucent bar (translucent's `outside`): a corner pixel the disc fully covers is
    /// the block's own, never the bar showing through (no corner squares); one beyond the arc is the
    /// bar.
    #[test]
    fn a_block_rounded_toward_a_translucent_bar_has_no_corner_squares() {
        for e in [1u32, 2] {
            let mut cache = MaskCache::default();
            let m = cache.get(8, e as i32);
            let (w, h) = (40 * e, 22 * e);
            let (pill, glass) = (premultiplied(Rgb(10, 200, 30), 255), premultiplied(Rgb(200, 100, 50), 128));
            let mut px = vec![0u8; (w * h * 4) as usize];
            fill_px(&mut px, w, h, (0, w), (0, h), pill);
            round_block(&mut px, w, h, (0, w), (0, h), m, glass, None);
            assert_eq!(rgba(&px, w, 0, 0), glass, "scale {e}: beyond the arc, the bar");
            let r = m.radius;
            let full: Vec<_> = (0..r).flat_map(|y| (0..r).map(move |x| (x, y))).filter(|&(x, y)| m.outer_at(x, y) == 255).collect();
            assert!(!full.is_empty(), "scale {e}: the corner has fully covered pixels");
            for (x, y) in full {
                for (xx, yy) in [(x, y), (w - 1 - x, y), (x, h - 1 - y), (w - 1 - x, h - 1 - y)] {
                    assert_eq!(rgba(&px, w, xx, yy), pill, "scale {e}: corner pixel ({xx}, {yy}) fully covered");
                }
            }
        }
    }

    /// A block narrower or lower than twice the radius gets no corner (callers fit the radius,
    /// `fitted_radius`); a fill past the buffer is clipped, never a panic.
    #[test]
    fn a_block_too_small_for_its_corners_is_left_square() {
        let mut cache = MaskCache::default();
        let m = cache.get(8, 1);
        let (w, h) = (40u32, 22u32);
        let pill = premultiplied(Rgb(10, 200, 30), 255);
        for (x, y) in [((0, 15), (0, h)), ((0, w), (0, 15)), ((5, 5), (0, h))] {
            let mut px = vec![0u8; (w * h * 4) as usize];
            fill_px(&mut px, w, h, x, y, pill);
            let before = px.clone();
            round_block(&mut px, w, h, x, y, m, CLEAR, None);
            assert!(px == before, "{x:?} {y:?}");
        }
        let mut px = vec![9u8; (w * h * 4) as usize];
        fill_px(&mut px, w, h, (35, 99), (20, 99), pill);
        assert_eq!(rgba(&px, w, 39, 21), pill, "clipped, no panic");
    }

    /// Every hued block gets its ink, even with each hued module in a block of its own (the most a
    /// bar can have: bar.toml rejects duplicates): none is left in `fg` on its hue.
    #[test]
    fn every_hued_block_gets_its_pills_ink() {
        use crate::grid::GroupKind;
        let roles = Roles::default();
        let groups: Vec<Group> = (0u16..)
            .zip(ModuleId::ALL.iter().filter_map(|&m| m.spec().hue))
            .map(|(i, hue)| Group { kind: GroupKind::Status, start: 2 * i, end: 2 * i + 1, hue: Some(hue) })
            .collect();
        assert!(groups.len() >= 6, "the hued modules: {}", groups.len());
        let bg = Background { style: BarStyle::Islands, opacity: 0.8, module_colors: ModuleColors::Pills, groups: &groups };
        let (on, n) = pill_inks(&roles, &bg);
        assert_eq!(n, groups.len(), "one ink per hued block");
        assert_eq!(on[n - 1].start, groups[n - 1].start, "the last one included");
    }

    /// Spec § 3: half the bar height minus 2 px, capped at 8.
    #[test]
    fn island_radius_is_half_the_height_minus_2_capped_at_8() {
        assert_eq!(island_radius(22), 8, "the default bar: 9 capped to 8");
        assert_eq!(island_radius(16), 6);
        assert_eq!(island_radius(3), 0);
        assert_eq!(island_radius(0), 0);
    }

    /// Island blocks are inset 2 logical px top and bottom; never past the middle.
    #[test]
    fn island_rows_are_inset_two_logical_px() {
        assert_eq!(island_y(22, 1), (2, 20));
        assert_eq!(island_y(44, 2), (4, 40));
        assert_eq!(island_y(3, 1), (1, 2), "a tiny bar: the inset shrinks, never crosses");
        assert_eq!(island_y(0, 1), (0, 0));
    }

    /// A block too small for the radius takes the largest that fits; a degenerate one none.
    #[test]
    fn fitted_radius_shrinks_to_the_block() {
        assert_eq!(fitted_radius(8, 100, 44, 2), 8);
        assert_eq!(fitted_radius(8, 20, 44, 2), 5, "20 physical px at scale 2: 10 logical, radius 5");
        assert_eq!(fitted_radius(8, 0, 22, 1), 0);
        assert_eq!(fitted_radius(8, 100, 0, 1), 0);
    }

    /// Spec § 1: the text on a pill takes whichever of bar_bg and fg contrasts more with the hue.
    #[test]
    fn on_fill_takes_whichever_of_bar_bg_and_fg_contrasts_more() {
        let mut r = Roles { bar_bg: Rgb(0x10, 0x10, 0x10), fg: Rgb(0xf0, 0xf0, 0xf0), ..Roles::default() };
        assert_eq!(on_fill(&r, Rgb(0xf9, 0xe2, 0xaf)), r.bar_bg, "a light hue: the dark bar_bg");
        assert_eq!(on_fill(&r, Rgb(0x30, 0x30, 0x80)), r.fg, "a dark hue: the light fg");
        r.bar_bg = Rgb(0xee, 0xee, 0xee);
        r.fg = Rgb(0x20, 0x20, 0x20);
        assert_eq!(on_fill(&r, Rgb(0xf9, 0xe2, 0xaf)), r.fg, "a light theme: the same rule, the other way round");
    }

    /// Spec § 1: warn, error, urgent and attention (rendered as ERROR) win over the pill's ink, and
    /// so does any cell carrying its own background (the active workspace).
    #[test]
    fn states_are_warn_error_urgent_and_any_cell_with_its_own_background() {
        for s in [Style::WARN, Style::ERROR, Style::URGENT, Style::ACTIVE, Style::SURFACE, Style::SELECTED] {
            assert!(in_state(s), "{s:?}");
        }
        for s in [Style::NORMAL, Style::MUTED, Style::hue(Hue::Green), Style::ACCENT, Style::BOLD_ACCENT] {
            assert!(!in_state(s), "{s:?}");
        }
    }
}
