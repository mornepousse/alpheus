//! Pixel tests of the bar shapes (themes delivery 2, spec § 6 "Painter"), at scales 1 and 2.
//! Each test paints a 12-column bar with hand-made groups through `Fonts::paint_bar` into a buffer
//! pre-filled with 0x11, so an unpainted pixel shows. Pixels are read back as (r, g, b, a),
//! premultiplied as stored.

use super::*;
use crate::config::DEFAULT_HEIGHT;
use crate::grid::{Grid, Group, GroupKind, Hue, Style};
use alpheus_commun::theme::contrast::ratio;
use alpheus_commun::theme::settings::{BarStyle, ModuleColors};
use alpheus_commun::theme::{Rgb, Roles};

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

/// Spec § 3 and reconciliation 4, islands:
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

/// Reconciliation 4 and ruling 11: a cell background (the active workspace on `accent_container`)
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

/// Ruling 11: no glyph pixel is ever erased. A full block in a block's first cell (over its
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
            // The full blocks reach the border's rows, and a glyph is never erased (ruling 11): the
            // border shows on the bare bar.
            let bare = Painted::new(&mut p, &Grid::empty(COLUMNS), &roles, &bg, scale, DEFAULT_HEIGHT);
            let top = blocks::ISLAND_INSET * s.e;
            assert_eq!(bare.at(s.mid(7), top), opaque(roles.border_over(blue)), "{style:?} scale {scale}: the border over the hue");
        }
    }
}

/// Review focus 4: on a light theme the wallpaper shows through the gaps, yet every block keeps a
/// visible border, and the pills text is picked against the block's hue, never what is behind.
#[test]
fn light_themes_keep_the_block_border_and_pick_the_pills_text_against_the_block() {
    let roles = alpheus_commun::theme::presets::preset("catppuccin").unwrap().light.unwrap();
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
