//! Pixel tests of the bar shapes (themes spec § 6 "Painter"), at scales 1 and 2.
//! Each test paints a 12-column bar with hand-made groups through `Fonts::paint_bar` into a buffer
//! pre-filled with 0x11, so an unpainted pixel shows. Pixels are read back as (r, g, b, a),
//! premultiplied as stored.

use super::*;
use crate::config::DEFAULT_HEIGHT;
use crate::grid::{Grid, Group, GroupKind, Hue, Style};
use alpheus_common::theme::contrast::ratio;
use alpheus_common::theme::settings::{BarStyle, ModuleColors};
use alpheus_common::theme::{Rgb, Roles};

const COLUMNS: u16 = 12;
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn fonts() -> Fonts {
    let (p, w) = Fonts::load(&FontConfig::default(), DEFAULT_HEIGHT)
        .expect("no monospace font available: run the tests inside `nix develop` (ALPHEUS_POLICES)");
    assert_eq!(w, None, "JetBrainsMono Nerd Font Mono expected (nix develop)");
    p
}

fn group(kind: GroupKind, start: u16, end: u16, hue: Option<Hue>) -> Group {
    Group { kind, start, end, hue }
}

/// The workspaces on cells 1..4 (no hue), the clock on 6..10 (`hue`): gaps at 0, 4–5 and 10–11.
fn two_blocks(hue: Option<Hue>) -> Vec<Group> {
    vec![group(GroupKind::Workspaces, 1, 4, None), group(GroupKind::Clock, 6, 10, hue)]
}

fn background(style: BarStyle, opacity: f32, module_colors: ModuleColors, groups: &[Group]) -> Background<'_> {
    Background { style, opacity, module_colors, groups }
}

fn opaque(Rgb(r, g, b): Rgb) -> [u8; 4] {
    [r, g, b, 255]
}

struct Painted {
    px: Vec<u8>,
    w: u32,
    h: u32,
    e: u32,
    advance: f32,
}

impl Painted {
    /// `grid` painted at `scale` on a surface `rows` logical px tall.
    fn new(p: &mut Fonts, grid: &Grid, roles: &Roles, bg: &Background, scale: i32, rows: u32) -> Painted {
        let advance = p.advance(scale);
        let e = scale as u32;
        let (w, h) = ((f32::from(grid.width()) * advance).ceil() as u32, rows * e);
        let mut px = vec![0x11u8; (w * h * 4) as usize];
        p.paint_bar(grid, roles, bg, &mut px, w, h, scale);
        Painted { px, w, h, e, advance }
    }

    /// (r, g, b, a) as stored.
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * self.w + x) * 4) as usize;
        [self.px[o + 2], self.px[o + 1], self.px[o], self.px[o + 3]]
    }

    /// Left edge of cell `col`, physical px (the painter's own rounding).
    fn edge(&self, col: u16) -> u32 {
        (f32::from(col) * self.advance).round() as u32
    }

    fn mid(&self, col: u16) -> u32 {
        (self.edge(col) + self.edge(col + 1)) / 2
    }
}

/// Spec § 3, islands: the bar transparent around and between the blocks (the wallpaper shows),
/// each block opaque `pill_bg`.
#[test]
fn islands_gaps_are_transparent_and_blocks_pill_bg() {
    let roles = Roles::default();
    let groups = two_blocks(None);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Islands, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, DEFAULT_HEIGHT);
        let y = s.h / 2;
        for col in [0, 4, 5, 10, 11] {
            assert_eq!(s.at(s.mid(col), y), CLEAR, "scale {scale}: gap cell {col} transparent");
            assert_eq!(s.at(s.mid(col), 0), CLEAR, "scale {scale}: gap cell {col}, top row");
        }
        for col in [2, 7, 8] {
            assert_eq!(s.at(s.mid(col), y), opaque(roles.pill_bg), "scale {scale}: block cell {col} on pill_bg");
        }
    }
}

/// Spec § 3, islands:
/// - each block inset 2 logical px from the bar's top and bottom, the bands transparent;
/// - rounded corners: the very corner pixel transparent, an anti-aliased arc, the inside opaque;
/// - radius min(22 / 2 − 2, 8) = 8 logical px: the straight left edge starts right below it;
/// - a 1 logical px border in `border_over(pill_bg)` along every straight edge.
#[test]
fn islands_corners_are_rounded_and_blocks_bordered() {
    let roles = Roles::default();
    let border = opaque(roles.border_over(roles.pill_bg));
    let groups = two_blocks(None);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Islands, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, DEFAULT_HEIGHT);
        let (x0, x1) = (s.edge(1), s.edge(4));
        let (xc, side) = ((x0 + x1) / 2, 8 * s.e);
        let (top, bottom) = (blocks::ISLAND_INSET * s.e, s.h - blocks::ISLAND_INSET * s.e);
        for y in (0..top).chain(bottom..s.h) {
            assert_eq!(s.at(xc, y), CLEAR, "scale {scale}: row {y}, the inset band above or below the block");
        }
        assert_eq!(s.at(x0, top), CLEAR, "scale {scale}: top left corner pixel outside the arc");
        assert_eq!(s.at(x1 - 1, bottom - 1), CLEAR, "scale {scale}: bottom right corner pixel outside the arc");
        assert_eq!(s.at(x0 + side / 2, top + side / 2), opaque(roles.pill_bg), "scale {scale}: inside the arc, opaque");
        let soft = (top..top + side).any(|cy| (0..side).any(|cx| matches!(s.at(x0 + cx, cy)[3], 1..=254)));
        assert!(soft, "scale {scale}: the arc is anti-aliased");
        assert_eq!(s.at(x0, top + side), border, "scale {scale}: radius 8, the straight left edge starts {side} rows below the top");
        for d in 0..s.e {
            assert_eq!(s.at(xc, top + d), border, "scale {scale}: top border, row {}", top + d);
            assert_eq!(s.at(xc, bottom - 1 - d), border, "scale {scale}: bottom border");
            assert_eq!(s.at(x0 + d, s.h / 2), border, "scale {scale}: left border");
            assert_eq!(s.at(x1 - 1 - d, s.h / 2), border, "scale {scale}: right border");
        }
        assert_eq!(s.at(xc, top + s.e), opaque(roles.pill_bg), "scale {scale}: the border is 1 logical px thick");
        assert_eq!(s.at(x0 + s.e, s.h / 2), opaque(roles.pill_bg), "scale {scale}: left border 1 logical px thick");
        assert_eq!(p.corner_masks(), scale as usize, "one mask per (radius, scale), shared by every block");
    }
}

/// A cell background (the active workspace on `accent_container`)
/// stays inside its block, clipped to the block's rows as it is painted (nothing is cleared after
/// the text).
#[test]
fn islands_cell_backgrounds_stay_inside_their_block() {
    let roles = Roles::default();
    let groups = two_blocks(None);
    let mut grid = Grid::empty(COLUMNS);
    grid.write(2, 4, " ", Style::ACTIVE);
    let mut p = fonts();
    for (style, outside) in [(BarStyle::Islands, CLEAR), (BarStyle::Translucent, [0x0f, 0x0f, 0x15, 0xcc])] {
        let mut roles = roles;
        roles.bar_bg = Rgb(0x13, 0x13, 0x1a); // × 0.8, premultiplied: (0x0f, 0x0f, 0x15, 0xcc)
        for scale in [1, 2] {
            let bg = background(style, 0.8, ModuleColors::Icons, &groups);
            let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
            let top = blocks::ISLAND_INSET * s.e;
            assert_eq!(s.at(s.mid(2), s.h / 2), opaque(roles.accent_container), "{style:?} scale {scale}: the active cell");
            assert_eq!(s.at(s.mid(2), top - 1), outside, "{style:?} scale {scale}: not above its block");
            assert_eq!(s.at(s.mid(2), s.h - top), outside, "{style:?} scale {scale}: nor below it");
        }
    }
}

/// No glyph pixel is ever erased. A full block in a block's first cell (over its
/// rounded corner, its border and the inset band) inks every pixel it inks under flat.
#[test]
fn no_glyph_pixel_is_erased_by_the_block_styles() {
    let roles = Roles { fg: Rgb(0xff, 0x00, 0xff), ..Roles::default() }; // unlike every role the blocks use
    let groups = two_blocks(None);
    let mut grid = Grid::empty(COLUMNS);
    grid.write(1, 2, "█", Style::NORMAL);
    grid.write(9, 10, "█", Style::NORMAL);
    let empty = Grid::empty(COLUMNS);
    let mut p = fonts();
    for scale in [1, 2] {
        let flat = background(BarStyle::Flat, 0.8, ModuleColors::Icons, &groups);
        let (ink, none) = (
            Painted::new(&mut p, &grid, &roles, &flat, scale, DEFAULT_HEIGHT),
            Painted::new(&mut p, &empty, &roles, &flat, scale, DEFAULT_HEIGHT),
        );
        for style in [BarStyle::Islands, BarStyle::Translucent] {
            let bg = background(style, 0.8, ModuleColors::Icons, &groups);
            let (with, without) = (
                Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT),
                Painted::new(&mut p, &empty, &roles, &bg, scale, DEFAULT_HEIGHT),
            );
            let mut inked = 0;
            for y in 0..ink.h {
                for x in (ink.edge(1)..ink.edge(2)).chain(ink.edge(9)..ink.edge(10)) {
                    if ink.at(x, y) != none.at(x, y) {
                        inked += 1;
                        assert_ne!(with.at(x, y), without.at(x, y), "{style:?} scale {scale}: the glyph's pixel ({x}, {y}) erased");
                    }
                }
            }
            assert!(inked > 0, "scale {scale}: the full blocks were drawn");
        }
    }
}

/// Under underline: its lines go under the glyphs. Full blocks overflowing a thin bar
/// (a big font on a short bar, as descenders do on any bar) over the bottom line and a block's hue
/// underline keep every pixel they ink under flat; wherever nothing is inked, the lines show as
/// they do with no text.
#[test]
fn no_glyph_pixel_is_erased_by_underline() {
    // 12 px of line for the default 14 px font: a full block reaches the band's last rows.
    const THIN: u32 = 12;
    let (mut p, _) = Fonts::load(&FontConfig::default(), THIN).expect("a monospace font (nix develop)");
    let roles = Roles { fg: Rgb(0xff, 0x00, 0xff), ..Roles::default() }; // unlike every line colour
    let groups = two_blocks(Some(Hue::Blue));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(0, 1, "█", Style::NORMAL); // on the bar: over the bottom line
    grid.write(7, 8, "█", Style::NORMAL); // in the clock's block: over its blue underline
    let empty = Grid::empty(COLUMNS);
    for scale in [1, 2] {
        let flat = background(BarStyle::Flat, 0.8, ModuleColors::Icons, &groups);
        let (ink, none) =
            (Painted::new(&mut p, &grid, &roles, &flat, scale, THIN), Painted::new(&mut p, &empty, &roles, &flat, scale, THIN));
        let bg = background(BarStyle::Underline, 0.8, ModuleColors::Icons, &groups);
        let (with, without) =
            (Painted::new(&mut p, &grid, &roles, &bg, scale, THIN), Painted::new(&mut p, &empty, &roles, &bg, scale, THIN));
        let lines = with.h - 3 * with.e..with.h;
        let mut inked = 0;
        for y in lines {
            for x in (ink.edge(0)..ink.edge(1)).chain(ink.edge(7)..ink.edge(8)) {
                if ink.at(x, y) != none.at(x, y) {
                    inked += 1;
                    assert_ne!(with.at(x, y), without.at(x, y), "scale {scale}: the glyph's pixel ({x}, {y}) erased by a line");
                } else {
                    assert_eq!(with.at(x, y), without.at(x, y), "scale {scale}: ({x}, {y}), no ink: the line shows");
                }
            }
        }
        assert!(inked > 0, "scale {scale}: the full blocks reach the lines");
    }
}

/// Spec § 3, translucent: the bar is `bar_bg` at `opacity`, premultiplied; the blocks stay solid,
/// inset, rounded and bordered as in islands.
#[test]
fn translucent_bar_is_bar_bg_at_opacity_premultiplied_and_blocks_opaque() {
    let roles = Roles { bar_bg: Rgb(200, 100, 50), ..Roles::default() };
    let groups = two_blocks(None);
    let half = [100, 50, 25, 128]; // 200·0.5, 100·0.5, 50·0.5, round(255·0.5)
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Translucent, 0.5, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, DEFAULT_HEIGHT);
        let (x0, x1) = (s.edge(6), s.edge(10));
        let top = blocks::ISLAND_INSET * s.e;
        assert_eq!(s.at(s.mid(5), s.h / 2), half, "scale {scale}: a gap is bar_bg at opacity");
        assert_eq!(s.at(s.mid(0), 0), half, "scale {scale}: the bar's corner too");
        assert_eq!(s.at((x0 + x1) / 2, 0), half, "scale {scale}: the inset band above a block is the bar");
        assert_eq!(s.at(x0, top), half, "scale {scale}: outside a block's arc, the bar shows");
        assert_eq!(s.at((x0 + x1) / 2, s.h / 2), opaque(roles.pill_bg), "scale {scale}: a block is opaque");
        assert_eq!(s.at((x0 + x1) / 2, top), opaque(roles.border_over(roles.pill_bg)), "scale {scale}: bordered as in islands");
    }
    let bg = background(BarStyle::Translucent, 1.0, ModuleColors::Icons, &groups);
    let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, 1, DEFAULT_HEIGHT);
    assert_eq!(s.at(s.mid(5), s.h / 2), opaque(roles.bar_bg), "opacity 1.0: an opaque bar");
}

/// Spec § 1, pills:
/// - a hued block is filled with its hue, its text in the higher-contrast of bar_bg and fg;
/// - states win;
/// - a block without a hue stays pill_bg.
#[test]
fn pills_fill_blocks_with_their_hue_and_pick_the_text_by_contrast_and_states_win() {
    let roles = Roles::default();
    let blue = roles.hues.blue;
    assert!(ratio(roles.bar_bg, blue) > ratio(roles.fg, blue), "precondition: on Catppuccin's light blue, bar_bg reads best");
    let groups = two_blocks(Some(Hue::Blue));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(2, 4, "█", Style::ACTIVE);
    grid.write(7, 10, "█", Style::BOLD_ACCENT);
    grid.write(8, 10, "█", Style::hue(Hue::Blue));
    grid.write(9, 10, "█", Style::WARN);
    let mut p = fonts();
    for style in [BarStyle::Islands, BarStyle::Translucent] {
        for scale in [1, 2] {
            let bg = background(style, 0.8, ModuleColors::Pills, &groups);
            let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
            let y = s.h / 2;
            assert_eq!(s.at(s.mid(6), y), opaque(blue), "{style:?} scale {scale}: the clock's block filled with its hue");
            assert_eq!(s.at(s.mid(7), y), opaque(roles.bar_bg), "{style:?} scale {scale}: text in bar_bg, the higher contrast");
            assert_eq!(s.at(s.mid(8), y), opaque(roles.bar_bg), "{style:?} scale {scale}: an icon in the block's hue too");
            assert_eq!(s.at(s.mid(9), y), opaque(roles.warn), "{style:?} scale {scale}: warn wins");
            assert_eq!(s.at(s.mid(1), y), opaque(roles.pill_bg), "{style:?} scale {scale}: no hue, pill_bg");
            assert_eq!(s.at(s.mid(2), y), opaque(roles.on_accent), "{style:?} scale {scale}: the active workspace keeps its colours");
            // The full blocks reach the border's rows, and a glyph is never erased: the
            // border shows on the bare bar.
            let bare = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, DEFAULT_HEIGHT);
            let top = blocks::ISLAND_INSET * s.e;
            assert_eq!(bare.at(s.mid(7), top), opaque(roles.border_over(blue)), "{style:?} scale {scale}: the border over the hue");
        }
    }
}

/// On a light theme the wallpaper shows through the gaps, yet every block keeps a
/// visible border, and the pills text is picked against the block's hue, never what is behind.
#[test]
fn light_themes_keep_the_block_border_and_pick_the_pills_text_against_the_block() {
    let roles = alpheus_common::theme::presets::preset("catppuccin").unwrap().light.unwrap();
    let border = roles.border_over(roles.pill_bg);
    assert_ne!(border, roles.pill_bg, "precondition: the border differs from the block");
    let blue = roles.hues.blue;
    let ink = blocks::on_fill(&roles, blue);
    let other = if ink == roles.bar_bg { roles.fg } else { roles.bar_bg };
    assert!(ratio(ink, blue) >= ratio(other, blue), "the picked ink is the higher contrast on the block");
    let groups = two_blocks(Some(Hue::Blue));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(7, 10, "█", Style::NORMAL);
    let mut p = fonts();
    for style in [BarStyle::Islands, BarStyle::Translucent] {
        for scale in [1, 2] {
            let bg = background(style, 0.8, ModuleColors::Icons, &groups);
            let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
            let top = blocks::ISLAND_INSET * s.e;
            assert_eq!(s.at(s.mid(2), top), opaque(border), "{style:?} scale {scale}: the light block keeps its border");
            let bg = background(style, 0.8, ModuleColors::Pills, &groups);
            let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
            assert_eq!(s.at(s.mid(7), s.h / 2), opaque(ink), "{style:?} scale {scale}: the text picked against the hue");
            let bare = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, DEFAULT_HEIGHT);
            assert_eq!(bare.at(s.mid(7), top), opaque(roles.border_over(blue)), "{style:?} scale {scale}: the hued block's border");
        }
    }
}

/// Spec § 1: under flat and underline, pills paints nothing a pill would; the layout gives it
/// `full`'s look, so the painter output equals `full`'s.
#[test]
fn pills_paint_nothing_extra_under_flat_and_underline() {
    let roles = Roles::default();
    let groups = two_blocks(Some(Hue::Blue));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(7, 10, "█", Style::NORMAL);
    let mut p = fonts();
    for style in [BarStyle::Flat, BarStyle::Underline] {
        for scale in [1, 2] {
            let pills = Painted::new(&mut p, &grid, &roles, &background(style, 0.8, ModuleColors::Pills, &groups), scale, DEFAULT_HEIGHT);
            let full = Painted::new(&mut p, &grid, &roles, &background(style, 0.8, ModuleColors::Full, &groups), scale, DEFAULT_HEIGHT);
            assert!(pills.px == full.px, "{style:?} scale {scale}: pills paints exactly as full");
        }
    }
}

/// A glyph over a transparent pixel stays premultiplied: white text at coverage a over nothing is
/// (a, a, a, a), never an opaque dark fringe.
#[test]
fn a_glyph_over_a_transparent_pixel_stays_premultiplied() {
    let roles = Roles { fg: Rgb(255, 255, 255), ..Roles::default() };
    let groups = two_blocks(None);
    let mut grid = Grid::empty(COLUMNS);
    grid.write(5, 6, "E", Style::NORMAL);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Islands, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
        let mut soft = 0;
        for y in 0..s.h {
            for x in s.edge(5)..s.edge(6) {
                let [r, g, b, a] = s.at(x, y);
                assert!(r == a && g == a && b == a, "scale {scale}: ({x}, {y}) = {:?}", [r, g, b, a]);
                soft += usize::from((1..=254).contains(&a));
            }
        }
        assert!(soft > 0, "scale {scale}: the E's anti-aliased edges were drawn");
    }
}

/// Spec § 5: a zero-width block draws nothing, a block past the bar's end is clipped, and neither
/// panics nor wraps its corners onto the next row.
#[test]
fn degenerate_blocks_draw_nothing_and_never_wrap() {
    let roles = Roles::default();
    let groups = vec![group(GroupKind::Title, 3, 3, None), group(GroupKind::Tray, 11, 14, None)];
    let mut p = fonts();
    for style in [BarStyle::Islands, BarStyle::Translucent] {
        for scale in [1, 2] {
            let bg = background(style, 0.8, ModuleColors::Icons, &groups);
            let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, DEFAULT_HEIGHT);
            let gap = s.at(s.mid(5), s.h / 2);
            assert_eq!(s.at(s.mid(3), s.h / 2), gap, "{style:?} scale {scale}: the empty block left no trace");
            // The clipped block's right corners lie past the buffer's width: wrapped, they would land
            // on the next rows' first cells, which no group covers.
            for y in 0..s.h {
                for x in 0..s.edge(3) {
                    assert_eq!(s.at(x, y), gap, "{style:?} scale {scale}: nothing wrapped onto ({x}, {y})");
                }
            }
        }
    }
}

/// Workspaces on 1..3, the clock on 6..9, the launcher on 10..11, the right-hand side from cell 5
/// (`Grid::split`):
/// - a right-pointing arrow in cell 3;
/// - left-pointing arrows in cells 5 and 9;
/// - cells 0, 4 and 11 plain bar.
fn powerline_groups(clock: Option<Hue>) -> Vec<Group> {
    vec![group(GroupKind::Workspaces, 1, 3, None), group(GroupKind::Clock, 6, 9, clock), group(GroupKind::Launcher, 10, 11, None)]
}

/// An empty powerline grid whose right-hand side starts at `split`.
fn split_grid(split: u16) -> Grid {
    let mut g = Grid::empty(COLUMNS);
    g.set_split(split);
    g
}

/// Spec § 3, powerline (amended 2026-10-08): full-height blocks on an opaque bar, alternating
/// `pill_bg` and `raised` from the workspaces, which are on `pill_bg`.
#[test]
fn powerline_blocks_are_full_height_on_an_opaque_bar() {
    let roles = Roles::default();
    let groups = powerline_groups(None);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Powerline, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &split_grid(5), &roles, &bg, scale, DEFAULT_HEIGHT);
        assert!((0..s.h).all(|y| (0..s.w).all(|x| s.at(x, y)[3] == 255)), "scale {scale}: the whole bar opaque");
        for y in [0, s.h / 2, s.h - 1] {
            assert_eq!(s.at(s.mid(1), y), opaque(roles.pill_bg), "scale {scale}: workspaces, row {y}");
            assert_eq!(s.at(s.mid(7), y), opaque(roles.raised), "scale {scale}: the block after them, row {y}");
            assert_eq!(s.at(s.mid(10), y), opaque(roles.pill_bg), "scale {scale}: the next one, row {y}");
        }
        for col in [0, 4, 11] {
            assert_eq!(s.at(s.mid(col), s.h / 2), opaque(roles.bar_bg), "scale {scale}: free cell {col} on bar_bg");
        }
    }
}

/// Spec § 3, powerline arrows:
/// - one cell wide, in the colour of the block they come from;
/// - the base against the block, the tip partly covered, symmetric about the axis;
/// - an anti-aliased edge;
/// - right-pointing after a left-side block, left-pointing before a right-side block.
#[test]
fn powerline_arrows_are_one_cell_anti_aliased_triangles_in_their_blocks_colour() {
    let roles = Roles::default();
    let (ac, bar) = (opaque(roles.pill_bg), opaque(roles.bar_bg));
    let groups = powerline_groups(None);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Powerline, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &split_grid(5), &roles, &bg, scale, DEFAULT_HEIGHT);
        let y = s.h / 2; // the row just below the axis
        let (a0, a1) = (s.edge(3), s.edge(4));
        assert_eq!(s.at(a0, y), ac, "scale {scale}: the base, against the workspaces");
        let tip = s.at(a1 - 1, y);
        assert!(tip != ac && tip != bar, "scale {scale}: the tip partly covered, got {tip:?}");
        assert_eq!(s.at(a1, y), bar, "scale {scale}: one cell wide");
        assert_eq!(s.at(a1 - 1, 0), bar, "scale {scale}: above the tip, bar_bg");
        assert!((a0..a1).all(|x| (0..s.h).all(|yy| s.at(x, yy) == s.at(x, s.h - 1 - yy))), "scale {scale}: symmetric");
        let soft = (a0..a1).any(|x| (0..y).any(|yy| ![ac, bar].contains(&s.at(x, yy))));
        assert!(soft, "scale {scale}: the slanted edges are anti-aliased");
        let (b0, b1) = (s.edge(5), s.edge(6));
        assert_eq!(s.at(b1 - 1, y), opaque(roles.raised), "scale {scale}: left-pointing, its base against the clock");
        let tip = s.at(b0, y);
        assert!(tip != opaque(roles.raised) && tip != bar, "scale {scale}: its tip on the left, partly covered, got {tip:?}");
        assert_eq!(s.at(b0 - 1, y), bar, "scale {scale}: one cell wide");
        assert_eq!(s.at(s.edge(10) - 1, y), opaque(roles.pill_bg), "scale {scale}: the launcher's arrow in its colour");
    }
}

/// 2026-10-08: the workspaces block is on `pill_bg` wherever it sits, the active workspace
/// keeps its accent fill on it, and every block differs from its neighbours, those before the
/// workspaces included (the launcher on the left).
#[test]
fn powerline_alternates_from_the_workspaces_and_the_active_workspace_stands_out() {
    let roles = Roles::default();
    let groups = vec![
        group(GroupKind::Launcher, 0, 1, None),
        group(GroupKind::Workspaces, 2, 4, None),
        group(GroupKind::Title, 5, 7, None),
        group(GroupKind::Status, 8, 9, None),
        group(GroupKind::Clock, 10, 12, None),
    ];
    let mut grid = split_grid(8);
    grid.write(2, 3, " ", Style::ACTIVE);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Powerline, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
        let y = s.h / 2;
        let fills: Vec<[u8; 4]> = groups.iter().map(|g| s.at(s.mid(g.end - 1), y)).collect();
        assert_eq!(fills[1], opaque(roles.pill_bg), "scale {scale}: the workspaces on pill_bg");
        assert_eq!(fills[0], opaque(roles.raised), "scale {scale}: the launcher before them differs");
        assert_eq!(fills[2], opaque(roles.raised), "scale {scale}: the block right after them differs");
        assert!(fills.windows(2).all(|f| f[0] != f[1]), "scale {scale}: neighbours differ: {fills:?}");
        assert_eq!(s.at(s.mid(2), y), opaque(roles.accent_container), "scale {scale}: the active workspace's accent fill");
        assert_ne!(roles.accent_container, roles.pill_bg, "the active workspace stands out on its block");
    }
}

/// One free cell between a left-side block and a right-side block holds one arrow: the left
/// block's. The title ends at the split (cell 4): it is on the left side.
#[test]
fn powerline_a_shared_free_cell_holds_the_left_blocks_arrow_only() {
    let roles = Roles::default();
    let groups = vec![group(GroupKind::Title, 1, 4, None), group(GroupKind::Status, 5, 8, None)];
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Powerline, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &split_grid(4), &roles, &bg, scale, DEFAULT_HEIGHT);
        let y = s.h / 2;
        assert_eq!(s.at(s.edge(4), y), opaque(roles.pill_bg), "scale {scale}: the title's arrow, pointing right");
        assert_ne!(s.at(s.edge(5) - 1, y), opaque(roles.raised), "scale {scale}: no left-pointing status arrow over it");
    }
}

/// Spec § 1 and § 3: under pills a hued block and its arrow take the hue, its text the picked ink,
/// and a state still wins.
#[test]
fn powerline_pills_blocks_and_arrows_take_the_hue() {
    let roles = Roles::default();
    let green = roles.hues.green;
    let groups = powerline_groups(Some(Hue::Green));
    let mut grid = split_grid(5);
    grid.write(7, 9, "█", Style::NORMAL);
    grid.write(8, 9, "█", Style::WARN);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Powerline, 0.8, ModuleColors::Pills, &groups);
        let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
        let y = s.h / 2;
        assert_eq!(s.at(s.mid(6), y), opaque(green), "scale {scale}: the clock's block in its hue");
        assert_eq!(s.at(s.edge(6) - 1, y), opaque(green), "scale {scale}: its arrow in its hue");
        assert_eq!(s.at(s.mid(7), y), opaque(blocks::on_fill(&roles, green)), "scale {scale}: the picked ink");
        assert_eq!(s.at(s.mid(8), y), opaque(roles.warn), "scale {scale}: warn wins over the pick");
        assert_eq!(s.at(s.mid(1), y), opaque(roles.pill_bg), "scale {scale}: no hue, neutral pill_bg");
    }
}

/// Spec § 1 under powerline: pills text reads on its block for every hue, in a dark and a light
/// theme. The glyph is painted in the higher-contrast of bar_bg and fg against the block's own
/// fill, never against flat's bar_bg (which would hide it on a hue close to the bar).
#[test]
fn powerline_pills_text_contrasts_with_its_block() {
    let light = alpheus_common::theme::presets::preset("catppuccin").unwrap().light.unwrap();
    let mut p = fonts();
    for roles in [Roles::default(), light] {
        for hue in [Hue::Red, Hue::Orange, Hue::Yellow, Hue::Green, Hue::Cyan, Hue::Blue, Hue::Purple] {
            let fill = color(&roles, roles.bar_bg, Tint::Hue(hue));
            let groups = powerline_groups(Some(hue));
            let mut grid = split_grid(5);
            grid.write(7, 9, "█", Style::hue(hue));
            let bg = background(BarStyle::Powerline, 0.8, ModuleColors::Pills, &groups);
            let s = Painted::new(&mut p, &grid, &roles, &bg, 1, DEFAULT_HEIGHT);
            let (block, text) = (s.at(s.mid(6), s.h / 2), s.at(s.mid(7), s.h / 2));
            assert_eq!(block, opaque(fill), "{hue:?}: the block is filled with its hue");
            let rgb = |[r, g, b, _]: [u8; 4]| Rgb(r, g, b);
            let best = ratio(roles.bar_bg, fill).max(ratio(roles.fg, fill));
            assert_eq!(ratio(rgb(text), rgb(block)), best, "{hue:?}: the text takes the best contrast with its block");
            assert_ne!(text, block, "{hue:?}: the text is visible on its block");
        }
    }
}

/// The block's inset rectangle: a cell background outside every block (a stray active
/// cell in a gap) is not painted under the block styles; inside a block it is.
#[test]
fn block_styles_clip_cell_backgrounds_to_their_blocks_x_span() {
    let roles = Roles::default();
    let groups = powerline_groups(None);
    let mut grid = split_grid(5);
    grid.write(4, 5, " ", Style::ACTIVE);
    grid.write(7, 8, " ", Style::ACTIVE);
    let empty = split_grid(5);
    let mut p = fonts();
    for style in [BarStyle::Islands, BarStyle::Translucent, BarStyle::Powerline] {
        for scale in [1, 2] {
            let bg = background(style, 0.8, ModuleColors::Icons, &groups);
            let (s, bare) = (
                Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT),
                Painted::new(&mut p, &empty, &roles, &bg, scale, DEFAULT_HEIGHT),
            );
            let y = s.h / 2;
            for x in s.edge(4)..s.edge(5) {
                assert_eq!(s.at(x, y), bare.at(x, y), "{style:?} scale {scale}: the gap cell's background at x {x} not painted");
            }
            assert_eq!(s.at(s.mid(7), y), opaque(roles.accent_container), "{style:?} scale {scale}: inside a block, painted");
        }
    }
    let flat = background(BarStyle::Flat, 0.8, ModuleColors::Icons, &groups);
    let s = Painted::new(&mut p, &grid, &roles, &flat, 1, DEFAULT_HEIGHT);
    assert_eq!(s.at(s.mid(4), s.h / 2), opaque(roles.accent_container), "flat: no blocks, no clipping");
}

/// 2026-10-08: inside a pill a MUTED cell (Bluetooth off, offline,
/// an outage's "–") is the pill's ink mixed 50 % toward the fill, so it reads paler than normal
/// text, which keeps the full ink; states still win. Floating's pills blocks too.
#[test]
fn muted_cells_in_pills_are_the_ink_halfway_to_the_fill() {
    let roles = Roles::default();
    let mut p = fonts();
    let rgb = |[r, g, b, _]: [u8; 4]| [r, g, b];
    let cases = [
        (BarStyle::Islands, Hue::Blue, two_blocks(Some(Hue::Blue)), Grid::empty(COLUMNS), DEFAULT_HEIGHT),
        (BarStyle::Powerline, Hue::Green, powerline_groups(Some(Hue::Green)), split_grid(5), DEFAULT_HEIGHT),
        (BarStyle::Floating, Hue::Purple, two_blocks(Some(Hue::Purple)), Grid::empty(COLUMNS), FLOATING),
    ];
    for (style, hue, groups, mut grid, height) in cases {
        grid.write(6, 7, "█", Style::MUTED);
        grid.write(7, 8, "█", Style::NORMAL);
        grid.write(8, 9, "█", Style::WARN);
        let fill = color(&roles, roles.bar_bg, Tint::Hue(hue));
        let ink = blocks::on_fill(&roles, fill);
        for scale in [1, 2] {
            let bg = background(style, 0.8, ModuleColors::Pills, &groups);
            let s = Painted::new(&mut p, &grid, &roles, &bg, scale, height);
            // The bar's middle row: floating's surface also holds its shadow, below the bar.
            let y = DEFAULT_HEIGHT * s.e / 2;
            let muted = rgb(s.at(s.mid(6), y));
            for (k, (m, (i, f))) in muted.into_iter().zip(rgb(opaque(ink)).into_iter().zip(rgb(opaque(fill)))).enumerate() {
                let half = (i32::from(i) + i32::from(f)) / 2;
                assert!((i32::from(m) - half).abs() <= 1, "{style:?} scale {scale}: channel {k} = {m}, halfway {half}");
                assert!(m >= i.min(f) && m <= i.max(f), "{style:?} scale {scale}: channel {k} between ink and fill");
            }
            assert!(muted != rgb(opaque(ink)) && muted != rgb(opaque(fill)), "{style:?} scale {scale}: strictly between");
            assert_eq!(s.at(s.mid(7), y), opaque(ink), "{style:?} scale {scale}: normal text keeps the full ink");
            assert_eq!(s.at(s.mid(8), y), opaque(roles.warn), "{style:?} scale {scale}: a state still wins");
            let icons = Painted::new(&mut p, &grid, &roles, &background(style, 0.8, ModuleColors::Icons, &groups), scale, height);
            assert_eq!(icons.at(s.mid(6), y), opaque(roles.fg_dim), "{style:?} scale {scale}: without pills, muted is fg_dim");
        }
    }
    // Flat and underline: pills paints as full, muted cells included.
    let groups = two_blocks(Some(Hue::Blue));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(6, 10, "████", Style::MUTED);
    for style in [BarStyle::Flat, BarStyle::Underline] {
        for scale in [1, 2] {
            let pills = Painted::new(&mut p, &grid, &roles, &background(style, 0.8, ModuleColors::Pills, &groups), scale, DEFAULT_HEIGHT);
            let full = Painted::new(&mut p, &grid, &roles, &background(style, 0.8, ModuleColors::Full, &groups), scale, DEFAULT_HEIGHT);
            assert!(pills.px == full.px, "{style:?} scale {scale}: muted unchanged");
        }
    }
}

use crate::placement::FLOAT_SHADOW;

/// A floating bar's surface: the default bar plus its shadow, logical px.
const FLOATING: u32 = DEFAULT_HEIGHT + FLOAT_SHADOW;

/// Spec § 3, floating:
/// - one rounded rectangle on bar_bg, radius 10 px, the corners transparent;
/// - no blocks without pills.
#[test]
fn floating_bar_is_one_rounded_rectangle_on_bar_bg() {
    let roles = Roles::default();
    let bar = opaque(roles.bar_bg);
    let groups = two_blocks(None);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Floating, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, FLOATING);
        let (band, side) = (DEFAULT_HEIGHT * s.e, 10 * s.e);
        for (x, y) in [(0, 0), (s.w - 1, 0), (0, band - 1), (s.w - 1, band - 1)] {
            assert_eq!(s.at(x, y), CLEAR, "scale {scale}: the bar's corner pixel ({x}, {y}) transparent");
        }
        assert_eq!(s.at(side / 2, side / 2), bar, "scale {scale}: inside the arc, opaque bar_bg");
        assert_eq!(s.at(0, band / 2), bar, "scale {scale}: the straight left end");
        assert!(s.at(0, 8 * s.e)[3] < 255, "scale {scale}: radius 10, not the islands' 8");
        let soft = (0..side).any(|cy| (0..side).any(|cx| matches!(s.at(cx, cy)[3], 1..=254)));
        assert!(soft, "scale {scale}: the arc is anti-aliased");
        for col in [2, 5, 7] {
            assert_eq!(s.at(s.mid(col), band / 2), bar, "scale {scale}: cell {col} on bar_bg, no blocks without pills");
        }
    }
}

/// Spec § 3: the shadow is 4 px below the bar, black, its alpha falling from 0.35 to 0, and
/// absent under the rounded ends' outermost columns.
#[test]
fn floating_shadow_falls_from_0_35_to_0_below_the_bar() {
    let roles = Roles::default();
    let groups = two_blocks(None);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Floating, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, FLOATING);
        let band = DEFAULT_HEIGHT * s.e;
        let x = s.w / 2;
        let alphas: Vec<u8> = (band..s.h).map(|y| s.at(x, y)[3]).collect();
        assert_eq!(alphas.len() as u32, FLOAT_SHADOW * s.e, "scale {scale}: 4 logical px of shadow");
        assert_eq!(alphas[0], 89, "scale {scale}: round(0.35 × 255) right below the bar");
        assert_eq!(alphas.last(), Some(&0), "scale {scale}: down to 0");
        assert!(alphas.windows(2).all(|w| w[0] > w[1]), "scale {scale}: strictly falling, {alphas:?}");
        assert!((band..s.h).all(|y| s.at(x, y)[..3] == [0, 0, 0]), "scale {scale}: black, premultiplied");
        assert_eq!(s.at(0, band), CLEAR, "scale {scale}: no shadow under the rounded end's outermost column");
    }
}

/// The text pass stays in the bar's band: a cell background never spills into the shadow.
#[test]
fn floating_text_and_cell_backgrounds_stay_in_the_bar_band() {
    let roles = Roles::default();
    let groups = two_blocks(None);
    let mut grid = Grid::empty(COLUMNS);
    grid.write(2, 4, " ", Style::ACTIVE);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Floating, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &grid, &roles, &bg, scale, FLOATING);
        let band = DEFAULT_HEIGHT * s.e;
        assert_eq!(s.at(s.mid(2), band - 1), opaque(roles.accent_container), "scale {scale}: the active cell down to the band's end");
        assert_eq!(s.at(s.mid(2), band), [0, 0, 0, 89], "scale {scale}: below it, the shadow");
    }
}

/// Spec § 1: under floating, pills draws each hued block as a rounded hue rectangle inside the bar.
#[test]
fn floating_pills_blocks_are_rounded_hue_blocks_inside_the_bar() {
    let roles = Roles::default();
    let green = roles.hues.green;
    let groups = two_blocks(Some(Hue::Green));
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Floating, 0.8, ModuleColors::Pills, &groups);
        let s = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, FLOATING);
        let band = DEFAULT_HEIGHT * s.e;
        assert_eq!(s.at(s.mid(7), band / 2), opaque(green), "scale {scale}: the clock's block in its hue");
        assert_eq!(s.at(s.mid(7), 0), opaque(roles.bar_bg), "scale {scale}: inset as an island: the bar above it");
        assert_eq!(s.at(s.edge(6), 2 * s.e), opaque(roles.bar_bg), "scale {scale}: outside the block's arc, the bar");
        assert_eq!(s.at(s.mid(2), band / 2), opaque(roles.bar_bg), "scale {scale}: no hue, no block");
    }
}

/// Spec § 1 under floating: pills text reads on its block for every hue, in a dark and a light
/// theme. The ink is chosen against the hue block floating paints, never against the bar.
#[test]
fn floating_pills_text_contrasts_with_its_block() {
    let light = alpheus_common::theme::presets::preset("catppuccin").unwrap().light.unwrap();
    let mut p = fonts();
    let rgb = |[r, g, b, _]: [u8; 4]| Rgb(r, g, b);
    for roles in [Roles::default(), light] {
        for hue in [Hue::Red, Hue::Orange, Hue::Yellow, Hue::Green, Hue::Cyan, Hue::Blue, Hue::Purple] {
            let fill = color(&roles, roles.bar_bg, Tint::Hue(hue));
            let groups = two_blocks(Some(hue));
            let mut grid = Grid::empty(COLUMNS);
            grid.write(7, 8, "█", Style::hue(hue));
            let bg = background(BarStyle::Floating, 0.8, ModuleColors::Pills, &groups);
            for scale in [1, 2] {
                let s = Painted::new(&mut p, &grid, &roles, &bg, scale, FLOATING);
                let y = DEFAULT_HEIGHT * s.e / 2;
                let (block, text) = (s.at(s.mid(8), y), s.at(s.mid(7), y));
                assert_eq!(block, opaque(fill), "{hue:?} scale {scale}: the block is filled with its hue");
                let best = ratio(roles.bar_bg, fill).max(ratio(roles.fg, fill));
                assert_eq!(ratio(rgb(text), rgb(block)), best, "{hue:?} scale {scale}: the text takes the best contrast with its block");
                assert_ne!(text, block, "{hue:?} scale {scale}: the text is visible on its block");
            }
        }
    }
}

/// Under floating: no glyph pixel is erased by the rounding (the bar's ends, the pills
/// blocks' corners), including a full block over the bar's first cell and a pill's first cell.
#[test]
fn no_glyph_pixel_is_erased_by_floating() {
    let mut roles = Roles { fg: Rgb(0xff, 0x00, 0xff), ..Roles::default() };
    // A dark green: the pill's ink is then fg, visible on the pill and on the bar around it alike.
    roles.hues.green = Rgb(0x00, 0x30, 0x00);
    assert_eq!(blocks::on_fill(&roles, roles.hues.green), roles.fg);
    let groups = two_blocks(None);
    let mut grid = Grid::empty(COLUMNS);
    grid.write(0, 1, "█", Style::NORMAL);
    grid.write(6, 7, "█", Style::NORMAL);
    let empty = Grid::empty(COLUMNS);
    let mut p = fonts();
    for scale in [1, 2] {
        let flat = background(BarStyle::Flat, 0.8, ModuleColors::Icons, &groups);
        let (ink, none) = (
            Painted::new(&mut p, &grid, &roles, &flat, scale, DEFAULT_HEIGHT),
            Painted::new(&mut p, &empty, &roles, &flat, scale, DEFAULT_HEIGHT),
        );
        for mc in [ModuleColors::Icons, ModuleColors::Pills] {
            let hued = two_blocks(Some(Hue::Green));
            let bg = background(BarStyle::Floating, 0.8, mc, &hued);
            let (with, without) =
                (Painted::new(&mut p, &grid, &roles, &bg, scale, FLOATING), Painted::new(&mut p, &empty, &roles, &bg, scale, FLOATING));
            let mut inked = 0;
            for y in 0..ink.h {
                for x in (ink.edge(0)..ink.edge(1)).chain(ink.edge(6)..ink.edge(7)) {
                    if ink.at(x, y) != none.at(x, y) {
                        inked += 1;
                        assert_ne!(with.at(x, y), without.at(x, y), "{mc:?} scale {scale}: the glyph's pixel ({x}, {y}) erased");
                    }
                }
            }
            assert!(inked > 0, "scale {scale}: the full blocks were drawn");
        }
    }
}

/// Under floating's pills: a cell background inside a hued block
/// stays in the block's inset rows; one on the bar itself (no hue, no block) spans the band.
#[test]
fn floating_pills_clip_cell_backgrounds_to_their_hued_block_only() {
    let roles = Roles::default();
    let groups = two_blocks(Some(Hue::Green));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(2, 3, " ", Style::ACTIVE);
    grid.write(7, 8, " ", Style::ACTIVE);
    let mut p = fonts();
    for scale in [1, 2] {
        let bg = background(BarStyle::Floating, 0.8, ModuleColors::Pills, &groups);
        let s = Painted::new(&mut p, &grid, &roles, &bg, scale, FLOATING);
        let (band, top) = (DEFAULT_HEIGHT * s.e, blocks::ISLAND_INSET * s.e);
        let active = opaque(roles.accent_container);
        assert_eq!(s.at(s.mid(7), band / 2), active, "scale {scale}: inside the hued block, painted");
        assert_eq!(s.at(s.mid(7), top - 1), opaque(roles.bar_bg), "scale {scale}: not above its block");
        assert_eq!(s.at(s.mid(7), band - top), opaque(roles.bar_bg), "scale {scale}: nor below it");
        assert_eq!(s.at(s.mid(2), 0), active, "scale {scale}: on the bar, the band's top row");
        assert_eq!(s.at(s.mid(2), band - 1), active, "scale {scale}: down to the band's end");
    }
}

/// The text pass is bounded by the bar's band, not the surface: a glyph taller than a short bar
/// (a big font on a thin bar) is cut at the band, never drawn over the shadow.
#[test]
fn floating_text_never_reaches_the_shadow_even_when_it_overflows_the_bar() {
    // 12 px of line for the default 14 px font: a full block overflows the line by a few rows.
    const THIN: u32 = 12;
    let (mut p, _) = Fonts::load(&FontConfig::default(), THIN).expect("a monospace font (nix develop)");
    let roles = Roles { fg: Rgb(0xff, 0x00, 0xff), ..Roles::default() };
    let groups = two_blocks(None);
    let mut grid = Grid::empty(COLUMNS);
    for col in 0..COLUMNS {
        grid.write(col, col + 1, "█", Style::NORMAL);
    }
    for scale in [1, 2] {
        let bg = background(BarStyle::Floating, 0.8, ModuleColors::Icons, &groups);
        let s = Painted::new(&mut p, &grid, &roles, &bg, scale, THIN + FLOAT_SHADOW);
        let bare = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, THIN + FLOAT_SHADOW);
        let band = THIN * s.e;
        assert_ne!(s.at(s.mid(5), band - 1), bare.at(s.mid(5), band - 1), "scale {scale}: the glyph reaches the band's last row");
        for y in band..s.h {
            for x in 0..s.w {
                assert_eq!(s.at(x, y), bare.at(x, y), "scale {scale}: ({x}, {y}) in the shadow is the shadow");
            }
        }
    }
}

/// A workspace visible on another output (`Style::SURFACE`, a `Tint::Pill` background) sits on a
/// `pill_bg` block under islands, translucent, powerline and underline: it is painted `raised`, one
/// step up, never `pill_bg` on `pill_bg` (it would look like any other workspace). Under flat and
/// floating it sits on `bar_bg` and keeps `pill_bg`.
#[test]
fn a_visible_workspace_on_a_pill_bg_block_is_raised() {
    let roles = Roles::default();
    let groups = two_blocks(None);
    let mut grid = Grid::empty(COLUMNS);
    grid.write(2, 3, " ", Style::SURFACE);
    let mut p = fonts();
    for scale in [1, 2] {
        for style in [BarStyle::Islands, BarStyle::Translucent, BarStyle::Powerline, BarStyle::Underline] {
            for mc in [ModuleColors::Icons, ModuleColors::Pills] {
                let bg = background(style, 0.8, mc, &groups);
                let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT);
                let y = s.h / 2;
                assert_eq!(s.at(s.mid(2), y), opaque(roles.raised), "{style:?} {mc:?} scale {scale}: the visible workspace raised");
                assert_eq!(s.at(s.mid(3), y), opaque(roles.pill_bg), "{style:?} {mc:?} scale {scale}: its block on pill_bg");
            }
        }
        // A block that is not `pill_bg` (powerline's `raised` neighbour, a pills hue): `pill_bg` shows.
        let mut other = Grid::empty(COLUMNS);
        other.write(7, 8, " ", Style::SURFACE);
        let hued = two_blocks(Some(Hue::Blue));
        for (style, mc, groups) in [(BarStyle::Powerline, ModuleColors::Icons, &groups), (BarStyle::Islands, ModuleColors::Pills, &hued)] {
            let s = Painted::new(&mut p, &other, &roles, &background(style, 0.8, mc, groups), scale, DEFAULT_HEIGHT);
            assert_eq!(s.at(s.mid(7), s.h / 2), opaque(roles.pill_bg), "{style:?} {mc:?} scale {scale}: off pill_bg, pill_bg");
        }
        for style in [BarStyle::Flat, BarStyle::Floating] {
            let bg = background(style, 0.8, ModuleColors::Icons, &groups);
            let s = Painted::new(&mut p, &grid, &roles, &bg, scale, DEFAULT_HEIGHT + FLOAT_SHADOW);
            let y = DEFAULT_HEIGHT * s.e / 2;
            assert_eq!(s.at(s.mid(2), y), opaque(roles.pill_bg), "{style:?} scale {scale}: on bar_bg, pill_bg stands out");
            assert_eq!(s.at(s.mid(3), y), opaque(roles.bar_bg), "{style:?} scale {scale}: no block around it");
        }
    }
}

/// A visible workspace is told apart from its block (and powerline's blocks from their neighbours)
/// only if `raised` is not `pill_bg`: true of a wallpaper theme from a legacy file (derived one step
/// up) and of every preset half (a derived step was chosen for the three whose palettes gave both
/// roles the same tone, 2026-10-08).
#[test]
fn raised_differs_from_pill_bg_in_every_theme() {
    use alpheus_common::theme::presets::PRESETS;
    use alpheus_common::theme::{Mode, parse_colors};
    let mut same = Vec::new();
    for preset in PRESETS {
        for (half, roles) in [("dark", Some(preset.dark)), ("light", preset.light)] {
            if roles.is_some_and(|r| r.raised == r.pill_bg) {
                same.push(format!("{} {half}", preset.name));
            }
        }
    }
    assert!(same.is_empty(), "preset halves whose raised is their pill_bg: {same:?}");
    let legacy = "bg = \"#1e1e2e\"\nfg = \"#cdd6f4\"\naccent = \"#cba6f7\"\nmuted = \"#6c7086\"\nsurface = \"#313244\"\nerror = \"#f38ba8\"\nok = \"#a6e3a1\"\n";
    for mode in [Mode::Dark, Mode::Light] {
        let r = parse_colors(legacy, mode).expect("a legacy wallpaper file");
        assert_ne!(r.raised, r.pill_bg, "a wallpaper theme, {mode:?}");
    }
}

/// Spec § 3, buffers: the opaque region a style claims (`opaque_rects`) is painted opaque. Every
/// physical pixel of every claimed logical px has alpha 255, for each style, with and without
/// pills, at scales 1 and 2: a compositor that skips blending there never shows a half-painted
/// pixel.
#[test]
fn every_claimed_opaque_px_is_painted_opaque() {
    let roles = Roles::default();
    let groups = two_blocks(Some(Hue::Blue));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(1, 3, "1 ", Style::ACTIVE);
    grid.write(7, 9, "ab", Style::NORMAL);
    grid.write(0, 1, "█", Style::NORMAL);
    let mut p = fonts();
    let mut rects = Vec::new();
    let styles = [BarStyle::Flat, BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Underline, BarStyle::Translucent];
    for style in styles {
        for mc in [ModuleColors::Icons, ModuleColors::Pills] {
            for scale in [1, 2] {
                let rows = if style == BarStyle::Floating { FLOATING } else { DEFAULT_HEIGHT };
                let s = Painted::new(&mut p, &grid, &roles, &background(style, 0.8, mc, &groups), scale, rows);
                let geometry = BarGeometry { width: s.w / s.e, height: DEFAULT_HEIGHT, advance: s.advance, scale };
                opaque_rects(style, &groups, &geometry, &mut rects);
                assert!(!rects.is_empty(), "{style:?} {mc:?} scale {scale}: something is claimed");
                for r in &rects {
                    let (x0, y0) = (r.x as u32 * s.e, r.y as u32 * s.e);
                    for y in y0..y0 + r.height as u32 * s.e {
                        for x in x0..x0 + r.width as u32 * s.e {
                            assert_eq!(s.at(x, y)[3], 255, "{style:?} {mc:?} scale {scale}: ({x}, {y}) claimed in {r:?}");
                        }
                    }
                }
            }
        }
    }
}

/// Spec § 5, degenerate sizes: scale 3, a bar 1 logical px tall, and a floating surface shorter
/// than its shadow (no band left) paint without a panic, every pixel valid premultiplied ARGB
/// (no channel above its alpha), for every style, with and without pills.
#[test]
fn degenerate_sizes_never_panic_and_stay_premultiplied() {
    // Bright: a channel left unpremultiplied at translucent's alpha 0.5 would exceed it.
    let roles = Roles { fg: Rgb(0xff, 0xff, 0xff), bar_bg: Rgb(0xf0, 0xe0, 0xd0), ..Roles::default() };
    let groups = two_blocks(Some(Hue::Blue));
    let mut grid = Grid::empty(COLUMNS);
    grid.write(0, COLUMNS, "█g1 █g2 █g3 ", Style::NORMAL);
    grid.write(2, 3, "1", Style::ACTIVE);
    let mut p = fonts();
    let styles = [BarStyle::Flat, BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Underline, BarStyle::Translucent];
    let sizes = [(3, DEFAULT_HEIGHT + FLOAT_SHADOW), (1, 1), (2, 1), (1, FLOAT_SHADOW - 1), (2, FLOAT_SHADOW / 2)];
    for style in styles {
        for mc in [ModuleColors::Icons, ModuleColors::Pills] {
            for (scale, rows) in sizes {
                let s = Painted::new(&mut p, &grid, &roles, &background(style, 0.5, mc, &groups), scale, rows);
                for y in 0..s.h {
                    for x in 0..s.w {
                        let [r, g, b, a] = s.at(x, y);
                        assert!(r <= a && g <= a && b <= a, "{style:?} {mc:?} scale {scale}, {rows} px: ({x}, {y}) = {:?}", [r, g, b, a]);
                    }
                }
            }
        }
    }
}
