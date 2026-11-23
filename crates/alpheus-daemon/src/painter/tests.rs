use super::*;
use crate::config::DEFAULT_HEIGHT;
use crate::grid::Style;
use alpheus_common::theme::Hues;
use alpheus_common::theme::settings::ModuleColors;

fn fonts() -> Fonts {
    let (p, w) = Fonts::load(&FontConfig::default(), DEFAULT_HEIGHT)
        .expect("no monospace font available: run the tests inside `nix develop` (ALPHEUS_POLICES)");
    assert_eq!(w, None, "JetBrainsMono Nerd Font Mono expected (nix develop)");
    p
}

/// 16 different colours, one per role: a tint resolved to the wrong role shows.
fn distinct() -> Roles {
    let c = |i: u8| Rgb(i * 15, 255 - i * 15, i * 7);
    Roles {
        bar_bg: c(1),
        pill_bg: c(2),
        popup_bg: c(3),
        raised: c(4),
        selection: c(5),
        fg: c(6),
        fg_dim: c(7),
        on_accent: c(8),
        accent: c(9),
        accent_container: c(10),
        accent2: c(11),
        accent2_container: c(12),
        ok: c(13),
        warn: c(14),
        error: c(15),
        urgent: c(16),
        hues: Hues {
            red: Rgb(17, 100 + 17, 200),
            orange: Rgb(18, 100 + 18, 200),
            yellow: Rgb(19, 100 + 19, 200),
            green: Rgb(20, 100 + 20, 200),
            cyan: Rgb(21, 100 + 21, 200),
            blue: Rgb(22, 100 + 22, 200),
            purple: Rgb(23, 100 + 23, 200),
        },
    }
}

#[test]
fn every_tint_is_its_role_and_bg_the_surfaces_base() {
    let r = distinct();
    let base = Rgb(1, 2, 3);
    let expected = [
        (Tint::Bg, base),
        (Tint::Fg, r.fg),
        (Tint::FgDim, r.fg_dim),
        (Tint::OnAccent, r.on_accent),
        (Tint::Accent, r.accent),
        (Tint::AccentContainer, r.accent_container),
        (Tint::Accent2, r.accent2),
        (Tint::Pill, r.pill_bg),
        (Tint::Selection, r.selection),
        (Tint::Warn, r.warn),
        (Tint::Error, r.error),
        (Tint::Urgent, r.urgent),
        (Tint::Hue(Hue::Red), r.hues.red),
        (Tint::Hue(Hue::Orange), r.hues.orange),
        (Tint::Hue(Hue::Yellow), r.hues.yellow),
        (Tint::Hue(Hue::Green), r.hues.green),
        (Tint::Hue(Hue::Cyan), r.hues.cyan),
        (Tint::Hue(Hue::Blue), r.hues.blue),
        (Tint::Hue(Hue::Purple), r.hues.purple),
    ];
    for (t, c) in expected {
        assert_eq!(color(&r, base, t), c, "{t:?}");
    }
}

/// Spec § 4: the bar on `bar_bg`; popups and the launcher on `popup_bg`, framed in `fg_dim` at
/// 40 % over it.
#[test]
fn a_bar_is_painted_on_bar_bg_and_a_box_on_popup_bg() {
    let r = distinct();
    let mut p = fonts();
    let av = p.advance(1);
    let (w, h) = ((av * 4.0).ceil() as u32, DEFAULT_HEIGHT);
    let mut px = vec![0u8; (w * h * 4) as usize];
    p.paint(&Grid::empty(4), &r, &mut px, w, h, 1);
    assert_eq!(&px[..4], &[r.bar_bg.2, r.bar_bg.1, r.bar_bg.0, 0xff], "a bar: bar_bg");
    let b = Boxed { rows: vec![text_row(6, ""), text_row(6, "")], separators: vec![], divider: None, ..Boxed::default() };
    let frame = p.frame(1);
    let (bw, bh) = ((6.0 * av).ceil() as u32 + frame.extra().0, 2 * DEFAULT_HEIGHT + frame.extra().1);
    let mut bpx = vec![0u8; (bw * bh * 4) as usize];
    p.paint_boxed(&b, &r, &mut bpx, bw, bh, DEFAULT_HEIGHT, 1, FrameInk::Border);
    let middle = ((bh / 2 * bw + bw / 2) * 4) as usize;
    assert_eq!(&bpx[middle..middle + 4], &[r.popup_bg.2, r.popup_bg.1, r.popup_bg.0, 0xff], "a popup, the launcher: popup_bg");
    let stroke = ((bh / 2 * bw) * 4) as usize; // the left stroke, mid-height
    let ink = r.border_over(r.popup_bg);
    assert_eq!(&bpx[stroke..stroke + 4], &[ink.2, ink.1, ink.0, 0xff], "the frame: fg_dim at 40 % over popup_bg");
}

/// Notifications spec § 3: a critical bubble's frame takes `urgent`; its separator lines keep the
/// border, and a plain box keeps it all round.
#[test]
fn a_critical_box_takes_the_urgent_frame() {
    let r = distinct();
    let mut p = fonts();
    let av = p.advance(1);
    let b = Boxed { rows: vec![text_row(6, ""), text_row(6, ""), text_row(6, "")], separators: vec![1], divider: None, ..Boxed::default() };
    let frame = p.frame(1);
    let (bw, bh) = ((6.0 * av).ceil() as u32 + frame.extra().0, 3 * DEFAULT_HEIGHT + frame.extra().1);
    let stroke = ((bh / 2 * bw) * 4) as usize; // the left stroke, mid-height
    let separator_y = frame.top + DEFAULT_HEIGHT + DEFAULT_HEIGHT.saturating_sub(frame.stroke) / 2;
    let line = ((separator_y * bw + bw / 2) * 4) as usize;
    let border = r.border_over(r.popup_bg);
    assert_ne!(border, r.urgent, "the test bites only if `distinct()` gives urgent its own colour");
    for (ink, edge) in [(FrameInk::Border, border), (FrameInk::Urgent, r.urgent)] {
        let mut px = vec![0u8; (bw * bh * 4) as usize];
        p.paint_boxed(&b, &r, &mut px, bw, bh, DEFAULT_HEIGHT, 1, ink);
        assert_eq!(&px[stroke..stroke + 4], &[edge.2, edge.1, edge.0, 0xff], "{ink:?}: the frame");
        assert_eq!(&px[line..line + 4], &[border.2, border.1, border.0, 0xff], "{ink:?}: the separator keeps the border");
    }
}

/// Frozen copy of `Fonts::glyph` exactly as it shaped text before the
/// memo: reshapes `ch` from scratch every single call, however
/// many times it was already drawn. Kept only so
/// `memoized_shaping_is_pixel_identical_to_reshaping_every_glyph` has
/// an independent ground truth to compare the memoized path against —
/// frozen with respect to the memo, but still masking `.notdef` the
/// same way `shape` does, or a title with an uncovered character (this
/// test's "日本語", not in JetBrainsMono Nerd Font Mono nor DejaVu Sans
/// Mono) would draw a `.notdef` box here while the real, masked path
/// draws nothing, and the two would never match.
#[allow(clippy::too_many_arguments)]
fn glyph_reshaped_every_time(p: &mut Fonts, ch: char, bold: bool, x0: f32, ink: Rgb, px: &mut [u8], width: u32, height: u32) {
    if p.cache.image_cache.len() >= MAX_CACHE {
        p.cache.image_cache.clear();
        p.cache.outline_command_cache.clear();
    }
    let mut s = [0u8; 4];
    p.buffer.set_text(&mut p.fs, ch.encode_utf8(&mut s), attrs(&p.family, bold), Shaping::Advanced);
    p.buffer.shape_until_scroll(&mut p.fs, false);
    let Rgb(r, g, b) = ink;
    let base = Color::rgb(r, g, b);
    for run in p.buffer.layout_runs() {
        for gl in run.glyphs {
            if gl.glyph_id == 0 {
                continue;
            }
            let phys = gl.physical((x0 - gl.x, 0.0), 1.0);
            let tint = gl.color_opt.unwrap_or(base);
            p.cache.with_pixels(&mut p.fs, phys.cache_key, tint, |dx, dy, c| {
                blend(px, width, height, phys.x + dx, run.line_y as i32 + phys.y + dy, c);
            });
        }
    }
}

/// Frozen copy of `Fonts::paint`, calling `glyph_reshaped_every_time`
/// instead of the memoized `Fonts::glyph`.
fn paint_reshaping_every_glyph(p: &mut Fonts, grid: &Grid, palette: &Roles, px: &mut [u8], width: u32, height: u32, scale: i32) {
    p.set_scale(scale);
    let advance = p.advance;
    let edge = |col: usize| ((col as f32) * advance).round() as u32;
    fill(px, width, 0, width, height, palette.bar_bg);
    for (col, c) in grid.cells().iter().enumerate() {
        if c.style.bg != Tint::Bg {
            fill(px, width, edge(col), edge(col + 1), height, color(palette, palette.bar_bg, c.style.bg));
        }
    }
    for (col, c) in grid.cells().iter().enumerate() {
        if c.ch == ' ' || c.ch == CONT {
            continue;
        }
        glyph_reshaped_every_time(p, c.ch, c.style.bold, edge(col) as f32, color(palette, palette.bar_bg, c.style.fg), px, width, height);
    }
}

#[test]
fn memoized_shaping_is_pixel_identical_to_reshaping_every_glyph() {
    let mut memo = fonts();
    let mut fresh = fonts();
    let pal = Roles::default();
    let av = memo.advance(1);
    assert_eq!(av, fresh.advance(1));
    let mut g = Grid::empty(40);
    // Varied columns (exercises every x0), repeated characters (the
    // memo must actually be hit, not just hold one entry), bold and
    // non-bold, and a CJK glyph (a differently-shaped, wide script).
    g.write(0, 40, "Hello, World! 日本語", Style::NORMAL);
    g.write(16, 40, "Hello!", Style { bold: true, ..Style::NORMAL });
    let (w, h) = ((av * 40.0).ceil() as u32, DEFAULT_HEIGHT);
    let mut memo_px = vec![0u8; (w * h * 4) as usize];
    let mut fresh_px = vec![0u8; (w * h * 4) as usize];
    memo.paint(&g, &pal, &mut memo_px, w, h, 1);
    paint_reshaping_every_glyph(&mut fresh, &g, &pal, &mut fresh_px, w, h, 1);
    assert_eq!(memo_px, fresh_px, "memoized shaping must draw byte-identical pixels");
}

/// Release micro-benchmark (not run by default: `--ignored`), prints
/// µs/paint with and without the shaping memo. Run with:
/// `nix develop -c cargo test --release -p alpheus-daemon --lib painter::tests::glyph_shaping_memo_speeds_up_paint -- --ignored --nocapture`
#[test]
#[ignore = "timing: run in release, numbers are meaningless in debug"]
fn glyph_shaping_memo_speeds_up_paint() {
    let mut memo = fonts();
    let mut fresh = fonts();
    let pal = Roles::default();
    let av = memo.advance(1);
    fresh.advance(1);
    // A realistic bar line: repeats of a modest alphabet, as redrawn
    // every tick, not thousands of distinct glyphs (that's what
    // `glyph_cache_bounded_on_varied_titles` already covers).
    let text = "workspaces  01:23:45  CPU 42%  MEM 38%  firefox — Title of the active window";
    let cols = text.chars().count() as u16;
    let mut g = Grid::empty(cols);
    g.write(0, cols, text, Style::NORMAL);
    let (w, h) = ((av * cols as f32).ceil() as u32, DEFAULT_HEIGHT);
    let mut px = vec![0u8; (w * h * 4) as usize];
    const ITERATIONS: u32 = 300;
    let t0 = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        paint_reshaping_every_glyph(&mut fresh, &g, &pal, &mut px, w, h, 1);
    }
    let before = t0.elapsed() / ITERATIONS;
    let t1 = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        memo.paint(&g, &pal, &mut px, w, h, 1);
    }
    let after = t1.elapsed() / ITERATIONS;
    eprintln!("paint: {before:?}/call reshaping every glyph, {after:?}/call with the shaping memo");
    assert!(after < before, "the memo should speed up a repeated paint: before {before:?}, after {after:?}");
}

#[test]
fn frame_makes_the_visible_gap_equal_on_every_side() {
    // 8.25 px cells (pad cell 8 px), capitals inked from 1 px left, 6 px from the row's top
    // and 7 px from its bottom: side 4 + pad 8 + bearing 1 = 13 px from stroke to text, so the
    // vertical insets make up for the leading.
    let f = Frame::new(1, 8.25, Ink { left: 1, top: 6, bottom: 7 });
    assert_eq!(f, Frame { side: 4, top: 7, bottom: 6, stroke: 1, radius: 6 });
    assert_eq!(f.side + 8 + 1, f.top + 6, "left gap = top gap");
    assert_eq!(f.side + 8 + 1, f.bottom + 7, "left gap = bottom gap");
    let f2 = Frame::new(2, 16.5, Ink { left: 2, top: 12, bottom: 14 });
    assert_eq!(
        f2,
        Frame { side: 8, top: 15, bottom: 13, stroke: 2, radius: 12 },
        "everything scales with the buffer (16.5 px cells: a 17 px pad cell)"
    );
    assert_eq!(f.extra(), (8, 13));
    let tall = Frame::new(1, 8.25, Ink { left: 1, top: 30, bottom: 32 });
    assert!(tall.top >= tall.radius && tall.bottom >= tall.radius, "a corner never reaches the rows: {tall:?}");
    assert_eq!(tall.side + 8 + 1, tall.top + 30, "left gap = top gap, the side follows");
    assert_eq!(tall.side + 8 + 1, tall.bottom + 32, "left gap = bottom gap");
    assert_eq!(Frame::new(0, 8.25, Ink { left: 1, top: 6, bottom: 7 }).stroke, 1, "scale 0 treated as 1");
}

/// A tall bar (height 40) carries more leading than the usual gap covers: the margins stay
/// even and the corners stay clear of the rows.
#[test]
fn a_tall_bar_keeps_the_margins_even_and_the_corners_clear() {
    let (mut p, _) = Fonts::load(&FontConfig::default(), 40).expect("fonts (nix develop)");
    for scale in [1, 2] {
        let f = p.frame(scale);
        let ink = p.measure_ink();
        let pad = p.advance.round() as u32;
        assert!(f.top >= f.radius && f.bottom >= f.radius, "scale {scale}: {f:?}");
        let left = f.side + pad + ink.left;
        assert_eq!(left, f.top + ink.top, "scale {scale}: left gap = top gap ({f:?}, {ink:?})");
        assert_eq!(left, f.bottom + ink.bottom, "scale {scale}: left gap = bottom gap");
    }
}

/// The selection band (a SELECTED first and last row) is as far from the frame at the top and
/// the bottom as it is on the left and the right, at both scales.
#[test]
fn the_selection_band_is_at_an_even_distance_from_the_frame() {
    for scale in [1, 2] {
        let mut p = fonts();
        let mut inv = Grid::empty(6);
        inv.write(0, 6, "", Style::SELECTED);
        for c in 0..6 {
            inv.write(c, c + 1, " ", Style::SELECTED);
        }
        let b = Boxed { rows: vec![inv.clone(), text_row(6, ""), inv], separators: vec![], divider: None, ..Boxed::default() };
        let s = Surface::paint(&mut p, &b, scale);
        let accent = Surface::opaque(Roles::default().selection);
        let (mx, my) = (s.w / 2, s.h / 2);
        let first = |f: &dyn Fn(u32) -> [u8; 4], range: std::ops::Range<u32>| range.into_iter().find(|&k| f(k) == accent);
        let top = first(&|y| s.at(mx, y), 0..s.h).unwrap_or(u32::MAX);
        let bottom = first(&|y| s.at(mx, s.h - 1 - y), 0..s.h).unwrap_or(u32::MAX);
        let left = first(&|x| s.at(x, s.frame.top + s.rh / 2), 0..s.w).unwrap_or(u32::MAX);
        let right = first(&|x| s.at(s.w - 1 - x, s.frame.top + s.rh / 2), 0..s.w).unwrap_or(u32::MAX);
        assert_eq!(left, s.frame.side, "scale {scale}: the band starts one side inset from the edge");
        assert_eq!((top, bottom), (left, left), "scale {scale}: top, bottom = left (my {my})");
        assert!(right.abs_diff(left) <= 1, "scale {scale}: right {right} px (the width is rounded up), left {left}");
    }
}

/// A surface bigger than its cells (a compositor-imposed size): the room left over is shared by
/// both sides, the margins stay even.
#[test]
fn a_surface_bigger_than_its_cells_shares_the_room_evenly() {
    for scale in [1, 2] {
        let mut p = fonts();
        let mut inv = Grid::empty(6);
        for c in 0..6 {
            inv.write(c, c + 1, " ", Style::SELECTED);
        }
        let b = Boxed { rows: vec![inv.clone(), text_row(6, ""), inv], separators: vec![], divider: None, ..Boxed::default() };
        let (advance, frame) = (p.advance(scale), p.frame(scale));
        let rh = DEFAULT_HEIGHT * scale as u32;
        let (w, h) = ((6.0 * advance).ceil() as u32 + frame.extra().0 + 14 * scale as u32, 3 * rh + frame.extra().1 + 31 * scale as u32);
        let mut px = vec![0x11u8; (w * h * 4) as usize];
        p.paint_boxed(&b, &Roles::default(), &mut px, w, h, rh, scale, FrameInk::Border);
        let at = |x: u32, y: u32| {
            let o = ((y * w + x) * 4) as usize;
            [px[o + 2], px[o + 1], px[o], px[o + 3]]
        };
        let accent = Surface::opaque(Roles::default().selection);
        let first = |f: &dyn Fn(u32) -> [u8; 4], n: u32| (0..n).find(|&k| f(k) == accent).unwrap_or(u32::MAX);
        let (mx, row) = (w / 2, h / 2 - rh); // inside the first row
        let (left, right) = (first(&|x| at(x, row), w), first(&|x| at(w - 1 - x, row), w));
        let (top, bottom) = (first(&|y| at(mx, y), h), first(&|y| at(mx, h - 1 - y), h));
        assert!(left.abs_diff(right) <= 1, "scale {scale}: left {left} px, right {right} px");
        assert!(top.abs_diff(bottom) <= 1, "scale {scale}: top {top} px, bottom {bottom} px");
        assert!(left > frame.side, "scale {scale}: the extra room is really spread ({left})");
    }
}

#[test]
fn the_fonts_frame_is_measured_once_per_scale() {
    let mut p = fonts();
    let (one, two) = (p.frame(1), p.frame(2));
    assert_eq!((one.stroke, one.radius, one.side), (1, 6, 4));
    assert_eq!((two.stroke, two.radius, two.side), (2, 12, 8));
    assert_eq!(p.frame(1), one, "memoized: the same answer, in either order");
    assert_eq!(p.frames.len(), 2);
    assert!(one.top > one.stroke && one.top < DEFAULT_HEIGHT, "a plausible inset above the letters: {one:?}");
}

/// The pixel tests' surface: `rows` painted in a box of the cells' size plus the frame, the rows
/// `DEFAULT_HEIGHT * scale` tall.
struct Surface {
    px: Vec<u8>,
    w: u32,
    h: u32,
    rh: u32,
    frame: Frame,
    advance: f32,
}

impl Surface {
    fn paint(p: &mut Fonts, b: &Boxed, scale: i32) -> Surface {
        let pal = Roles::default();
        let (advance, frame) = (p.advance(scale), p.frame(scale));
        let (cols, rows) = b.cells();
        let rh = DEFAULT_HEIGHT * scale as u32;
        let (w, h) = ((cols as f32 * advance).ceil() as u32 + frame.extra().0, rows as u32 * rh + frame.extra().1);
        let mut px = vec![0x11u8; (w * h * 4) as usize];
        p.paint_boxed(b, &pal, &mut px, w, h, rh, scale, FrameInk::Border);
        Surface { px, w, h, rh, frame, advance }
    }

    /// (r, g, b, a)
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * self.w + x) * 4) as usize;
        [self.px[o + 2], self.px[o + 1], self.px[o], self.px[o + 3]]
    }

    fn opaque(rgb: Rgb) -> [u8; 4] {
        [rgb.0, rgb.1, rgb.2, 255]
    }
}

fn text_row(columns: u16, text: &str) -> Grid {
    let mut g = Grid::empty(columns);
    g.write(0, columns, text, Style::NORMAL);
    g
}

/// The first and the last row start with an "E" after the pad cell: the ink's distance to the
/// frame is the same on the left, at the top and at the bottom (within a pixel), at both scales.
#[test]
fn the_text_sits_at_an_even_distance_from_the_frame() {
    for scale in [1, 2] {
        let mut p = fonts();
        let b = Boxed {
            rows: vec![text_row(6, " E"), text_row(6, ""), text_row(6, " E")],
            separators: vec![],
            divider: None,
            ..Boxed::default()
        };
        let s = Surface::paint(&mut p, &b, scale);
        let bg = Surface::opaque(Roles::default().popup_bg);
        let (r, st) = (s.frame.radius, s.frame.stroke);
        let (mut min_x, mut min_y, mut max_y) = (u32::MAX, u32::MAX, 0);
        for y in st..s.h - st {
            for x in r..s.w - r {
                if s.at(x, y) != bg {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        let (left, top, bottom) = (min_x - st, min_y - st, (s.h - st - 1) - max_y);
        assert!(left.abs_diff(top) <= 1, "scale {scale}: left {left} px, top {top} px");
        assert!(left.abs_diff(bottom) <= 1, "scale {scale}: left {left} px, bottom {bottom} px");
        assert!(left > 4 * scale as u32, "scale {scale}: the gap is a real margin, not the stroke ({left} px)");
    }
}

#[test]
fn the_frame_is_a_stroke_at_the_surface_edge_with_rounded_corners() {
    for scale in [1, 2] {
        let mut p = fonts();
        let b =
            Boxed { rows: vec![text_row(6, ""), text_row(6, ""), text_row(6, "")], separators: vec![], divider: None, ..Boxed::default() };
        let s = Surface::paint(&mut p, &b, scale);
        let pal = Roles::default();
        let (border, bg) = (Surface::opaque(pal.border_over(pal.popup_bg)), Surface::opaque(pal.popup_bg));
        let (r, st, w, h) = (s.frame.radius, s.frame.stroke, s.w, s.h);
        let mid = (w / 2, h / 2);
        for k in 0..st {
            assert_eq!(s.at(mid.0, k), border, "scale {scale}: top stroke line {k}");
            assert_eq!(s.at(mid.0, h - 1 - k), border, "bottom stroke line {k}");
            assert_eq!(s.at(k, mid.1), border, "left stroke line {k}");
            assert_eq!(s.at(w - 1 - k, mid.1), border, "right stroke line {k}");
        }
        assert_eq!(s.at(mid.0, st), bg, "scale {scale}: the stroke is {st} px thick, not more");
        assert_eq!(s.at(st, mid.1), bg);
        assert_eq!(s.at(r, 0), border, "the straight part starts where the corner ends");
        assert_eq!(s.at(0, r), border);
        assert_eq!(s.at(w - 1 - r, h - 1), border);
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(s.at(x, y)[3], 0, "scale {scale}: outside the rounded corner stays transparent ({x}, {y})");
        }
        // Where the middle of the stroke crosses the corner's diagonal.
        let c = (r as f32 - (r as f32 - st as f32 / 2.0) * std::f32::consts::FRAC_1_SQRT_2).floor() as u32;
        assert_eq!(s.at(c, c)[3], 255, "scale {scale}: inside the arc: opaque");
        assert_ne!(s.at(c, c), bg, "and on the stroke, not on the background");
        assert_eq!(s.at(r, r), bg, "the corner's centre is inside, on the background");
    }
}

#[test]
fn separators_and_the_divider_are_lines_of_one_stroke_across_the_box() {
    for scale in [1, 2] {
        let mut p = fonts();
        let b = Boxed {
            rows: vec![text_row(8, " E"), text_row(8, ""), text_row(8, " E"), text_row(8, " E")],
            separators: vec![1],
            divider: Some(5),
            ..Boxed::default()
        };
        let s = Surface::paint(&mut p, &b, scale);
        let pal = Roles::default();
        let (border, bg) = (Surface::opaque(pal.border_over(pal.popup_bg)), Surface::opaque(pal.popup_bg));
        let (st, w, h, rh) = (s.frame.stroke, s.w, s.h, s.rh);
        let y = s.frame.top + rh + (rh - st) / 2;
        let off_divider = s.frame.side + (3.0 * s.advance).round() as u32;
        for k in 0..st {
            assert_eq!(s.at(off_divider, y + k), border, "scale {scale}: the separator, line {k}");
            assert_eq!(s.at(st, y + k), border, "it reaches the left stroke");
            assert_eq!(s.at(w - 1 - st, y + k), border, "and the right one");
        }
        assert_eq!(s.at(off_divider, y - 1), bg, "{st} px thick: nothing above");
        assert_eq!(s.at(off_divider, y + st), bg, "nor below");
        let x = s.frame.side
            + (5.0 * s.advance).round() as u32
            + ((6.0 * s.advance).round() as u32 - (5.0 * s.advance).round() as u32 - st) / 2;
        for yy in [st, s.frame.top, s.frame.top + 2 * rh + rh / 2, h - 1 - st] {
            for k in 0..st {
                assert_eq!(s.at(x + k, yy), border, "scale {scale}: the divider at y {yy}, column {k}");
            }
        }
        assert_eq!(s.at(x - 1, s.frame.top + 2 * rh + rh / 2), bg, "{st} px thick: nothing to its left");
        assert_eq!(s.at(x + st, s.frame.top + 2 * rh + rh / 2), bg, "nor its right");
    }
}

#[test]
fn paint_boxed_with_a_short_buffer_paints_nothing_and_never_panics() {
    let mut p = fonts();
    let b = Boxed { rows: vec![Grid::empty(4), Grid::empty(4)], separators: vec![0], divider: Some(1), ..Boxed::default() };
    let mut px = vec![0x11u8; 100];
    p.paint_boxed(&b, &Roles::default(), &mut px, 60, 40, DEFAULT_HEIGHT, 1, FrameInk::Border);
    assert!(px.iter().all(|b| *b == 0x11), "an inconsistent buffer is left alone");
    // Rows past the buffer's end (the compositor shortened the popup): the rows that fit are painted, no panic.
    let frame = p.frame(1);
    let w = (p.advance(1) * 4.0).ceil() as u32 + frame.extra().0;
    let h = frame.top + DEFAULT_HEIGHT; // the first row only
    let mut px = vec![0x11u8; (w * h * 4) as usize];
    p.paint_boxed(&b, &Roles::default(), &mut px, w, h, DEFAULT_HEIGHT, 1, FrameInk::Border);
    assert_ne!(&px[..3], &[0x11, 0x11, 0x11], "painted");
}

#[test]
fn active_background_and_glyphs() {
    let mut p = fonts();
    let mut g = Grid::empty(4);
    g.write(0, 4, "A", Style::NORMAL);
    g.write(1, 4, " ", Style::ACTIVE);
    g.write(2, 4, "·", Style::NORMAL);
    let pal = Roles::default();
    let av = p.advance(1);
    let (w, h) = ((av * 4.0).ceil() as u32, DEFAULT_HEIGHT);
    let mut px = vec![0u8; (w * h * 4) as usize];
    p.paint(&g, &pal, &mut px, w, h, 1);
    let read = |x: u32, y: u32| {
        let o = ((y * w + x) * 4) as usize;
        (Rgb(px[o + 2], px[o + 1], px[o]), px[o + 3])
    };
    assert_eq!(read((av * 1.5) as u32, h / 2), (pal.accent_container, 0xff), "active cell: accent_container background");
    let ink_a = (0..av as u32).any(|x| (0..h).any(|y| read(x, y).0 != pal.bar_bg));
    assert!(ink_a, "no pixel drawn for \"A\"");
    let x3 = (av * 3.0).ceil() as u32;
    assert!((x3..w).all(|x| (0..h).all(|y| read(x, y) == (pal.bar_bg, 0xff))), "empty cell: background, opaque");
}

/// A bug report: Claude Code's terminal title starts with "✳ "
/// (U+2733) — a glyph JetBrainsMono Nerd Font Mono does not have
/// (checked via its cmap table) but DejaVu Sans Mono does, hence the
/// default `fallback`. Bites on dropping that default
/// (`FontConfig::default`'s `fallback: Vec::new()` instead of
/// `vec!["DejaVu Sans Mono".into()]`): with no fallback loaded,
/// `family` alone has no glyph for ✳ either, so masking (see `shape`)
/// would drop it and this test would go red.
#[test]
fn fallback_font_draws_a_symbol_the_main_family_lacks() {
    let mut p = fonts();
    let mut g = Grid::empty(3);
    g.write(0, 3, "\u{2733}", Style::NORMAL); // ✳, eight-spoked asterisk
    let pal = Roles::default();
    let av = p.advance(1);
    let (w, h) = ((av * 3.0).ceil() as u32, DEFAULT_HEIGHT);
    let mut px = vec![0u8; (w * h * 4) as usize];
    p.paint(&g, &pal, &mut px, w, h, 1);
    let read = |x: u32, y: u32| {
        let o = ((y * w + x) * 4) as usize;
        Rgb(px[o + 2], px[o + 1], px[o])
    };
    let ink = (0..av as u32).any(|x| (0..h).any(|y| read(x, y) != pal.bar_bg));
    assert!(ink, "✳ must be drawn from the fallback font, not masked away");
    // Directly confirms it is a real glyph (from DejaVu Sans Mono), not
    // what masking would otherwise have dropped.
    assert!(!p.shaped('\u{2733}', false).is_empty(), "shaping ✳ must produce a real glyph, not an empty (masked) result");
}

/// A Cherokee letter (U+13A0): no loaded face has it (neither
/// JetBrainsMono Nerd Font Mono — itself a reasonably complete font,
/// it does carry the braille block the bug report also mentioned, so
/// that one turned out not to exercise masking at all — nor the
/// default fallback DejaVu Sans Mono; checked via their cmap tables),
/// so it must be masked: no `.notdef` box drawn, and the character
/// after it stays at its own exact column. Bites on removing the
/// `gl.glyph_id == 0` filter in `shape`: the character's cell would
/// then show whatever "missing glyph" box the resolved face paints
/// for `.notdef`, and `shaped` would stop returning an empty vec.
#[test]
fn uncovered_character_is_masked_with_a_consistent_width() {
    let mut p = fonts();
    assert!(p.shaped('\u{13A0}', false).is_empty(), "no font covers U+13A0: shaping it must yield nothing to draw");
    let mut g = Grid::empty(3);
    g.write(0, 3, "A", Style::NORMAL);
    g.write(1, 3, "\u{13A0}", Style::NORMAL);
    g.write(2, 3, "B", Style::NORMAL);
    let pal = Roles::default();
    let av = p.advance(1);
    let (w, h) = ((av * 3.0).ceil() as u32, DEFAULT_HEIGHT);
    let mut px = vec![0u8; (w * h * 4) as usize];
    p.paint(&g, &pal, &mut px, w, h, 1);
    let read = |x: u32, y: u32| {
        let o = ((y * w + x) * 4) as usize;
        Rgb(px[o + 2], px[o + 1], px[o])
    };
    let (x0, x1) = ((av * 1.0).round() as u32, (av * 2.0).round() as u32);
    assert!(
        (x0..x1).all(|x| (0..h).all(|y| read(x, y) == pal.bar_bg)),
        "a masked character draws no box at all, just its cell's background"
    );
    let ink_b = (x1..w).any(|x| (0..h).any(|y| read(x, y) != pal.bar_bg));
    assert!(ink_b, "the character right after the masked one is still drawn, at its own column (consistent width)");
}

/// The OSD's own glyphs (osd/mod.rs) are in the bar's font: shaped, never masked.
#[test]
fn the_osd_glyphs_are_in_the_font() {
    let mut p = fonts();
    for c in [crate::osd::MIC_ON, crate::osd::MIC_OFF, crate::osd::BRIGHTNESS] {
        assert!(!p.shaped(c, false).is_empty(), "U+{:X} has no glyph in the bar's font", u32::from(c));
    }
}

#[test]
fn the_resource_glyphs_are_in_the_font() {
    let mut p = fonts();
    for c in [crate::segments::DOWNLOAD, crate::segments::UPLOAD, crate::segments::HARDDISK, crate::segments::GPU] {
        assert!(!p.shaped(c, false).is_empty(), "U+{:X} has no glyph in the bar's font", u32::from(c));
    }
}

#[test]
fn the_timer_glyphs_are_in_the_font() {
    use crate::timer::popup::{PLUS, STOP};
    use crate::timer::{BREAK, DONE, STOPWATCH, TIMER, WORK};
    let mut p = fonts();
    for c in [TIMER, WORK, BREAK, STOPWATCH, DONE, PLUS, STOP] {
        assert!(!p.shaped(c, false).is_empty(), "U+{:X} has no glyph in the bar's font", u32::from(c));
    }
}

#[test]
fn only_useful_faces_are_loaded() {
    // Regular + bold of the family, plus regular + bold of DejaVu Sans
    // Mono (the default fallback): ≈ 2 MB, DejaVu's own share close to
    // nothing (measured in the release memory suite). The whole
    // database (system + Nerd Font directory) cost ≈ 47 MB when measured.
    let p = fonts();
    assert_eq!(p.loaded_faces(), 4, "faces loaded: {}", p.loaded_faces());
}

#[test]
fn glyph_cache_bounded_on_varied_titles() {
    let mut p = fonts();
    let pal = Roles::default();
    let av = p.advance(1);
    let (w, h) = ((av * 256.0).ceil() as u32, DEFAULT_HEIGHT);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut highest = 0;
    let mut highest_shaped = 0;
    // 4,096 distinct glyphs (md icons from the Nerd Font), 256 per title.
    for block in 0..16u32 {
        let title: String = (0..256u32).filter_map(|i| char::from_u32(0xF0001 + block * 256 + i)).collect();
        let mut g = Grid::empty(256);
        g.write(0, 256, &title, Style::NORMAL);
        p.paint(&g, &pal, &mut px, w, h, 1);
        highest = highest.max(p.cached_glyphs());
        highest_shaped = highest_shaped.max(p.shaped_glyphs());
        assert!(p.cached_glyphs() <= MAX_CACHE, "cache: {} glyphs", p.cached_glyphs());
        assert!(p.shaped_glyphs() <= MAX_CACHE, "shape memo: {} entries", p.shaped_glyphs());
    }
    assert!(highest > 1000, "the cache does serve a purpose ({highest})");
    assert!(highest_shaped > 1000, "the shape memo does serve a purpose ({highest_shaped})");
}

/// Bites on `self.shapes.clear()` being removed from `set_scale`: a
/// shape's `cache_key` and physical offsets are only valid for the
/// metrics they were measured at, so carrying them over to a new scale
/// would draw the wrong glyph (or misplace it) at the new size.
#[test]
fn shape_memo_cleared_on_scale_change() {
    let mut p = fonts();
    p.advance(1);
    p.shaped('A', false);
    assert_eq!(p.shaped_glyphs(), 1);
    p.advance(2); // scale change
    assert_eq!(p.shaped_glyphs(), 0, "a scale change must drop every memoized shape");
}

#[test]
fn scale_2_doubles_the_cell() {
    let mut p = fonts();
    let (a1, a2) = (p.advance(1), p.advance(2));
    assert!(a1 > 4.0 && (a2 - 2.0 * a1).abs() <= 1.0, "{a1} {a2}");
}

/// A screen at scale 2 and another at 1: `draw` alternates scales every
/// frame. The swash keys include the size (glyphs of both scales
/// coexist): the cache must therefore never be cleared on alternation,
/// only bounded by MAX_CACHE. Bites on `self.cache.image_cache.clear()`
/// in `set_scale`: restoring it would make every return to scale 1 drop
/// the cache back to holding only that one scale instead of staying
/// stable across both.
#[test]
fn mixed_scales_no_flush_on_each_alternation() {
    let mut p = fonts();
    let pal = Roles::default();
    let mut g = Grid::empty(4);
    g.write(0, 4, "Ab1", Style::NORMAL);
    let av2 = p.advance(2);
    let (w, h) = ((av2 * 4.0).ceil() as u32, DEFAULT_HEIGHT * 2);
    let mut px = vec![0u8; (w * h * 4) as usize];
    p.paint(&g, &pal, &mut px, w, h, 1);
    let n1 = p.cached_glyphs();
    p.paint(&g, &pal, &mut px, w, h, 2);
    let n2 = p.cached_glyphs();
    assert!(n2 > n1, "scale 2's glyphs add up to scale 1's: {n1} then {n2}");
    for _ in 0..4 {
        p.paint(&g, &pal, &mut px, w, h, 1);
        assert_eq!(p.cached_glyphs(), n2, "flush on return to scale 1: the cache should not shrink");
        p.paint(&g, &pal, &mut px, w, h, 2);
        assert_eq!(p.cached_glyphs(), n2, "flush on return to scale 2: the cache should not shrink");
    }
}

#[test]
fn missing_family_falls_back_with_warning() {
    let requested = FontConfig { family: "A Font That Does Not Exist".into(), ..FontConfig::default() };
    let (p, w) = Fonts::load(&requested, DEFAULT_HEIGHT).expect("fallback to a monospace font");
    let w = w.expect("warning expected");
    assert!(w.contains("A Font That Does Not Exist") && w.contains(p.family()), "{w}");
    assert_ne!(p.family(), "A Font That Does Not Exist");
    // The replacement family's own faces (<= 2) plus the default
    // fallback's (DejaVu Sans Mono, <= 2): still not the whole database.
    assert!(p.loaded_faces() <= 4, "fallback: one family and one fallback, not the whole database ({})", p.loaded_faces());
}

#[test]
fn missing_fallback_family_warned() {
    let requested = FontConfig { fallback: vec!["Missing Fallback".into()], ..FontConfig::default() };
    let (p, w) = Fonts::load(&requested, DEFAULT_HEIGHT).unwrap();
    assert!(w.unwrap().contains("Missing Fallback"));
    assert_eq!(p.loaded_faces(), 2);
}

#[test]
fn columns_and_click() {
    assert_eq!(columns(1920, 8.4), 228);
    assert_eq!(columns(100, 0.0), 0);
    assert_eq!(column_under(100.0, 2, 16.8), 11);
    assert_eq!(column_under(-3.0, 1, 8.4), 0);
    assert_eq!(column_under(10.0, 0, 8.4), 1, "scale 0 treated as 1");
}

#[test]
fn buffer_too_small_without_panic() {
    let mut p = fonts();
    let mut g = Grid::empty(10);
    g.write(0, 10, "abcdefghij", Style::NORMAL);
    let mut px = vec![0u8; 16];
    p.paint(&g, &Roles::default(), &mut px, 100, 22, 1);
}

/// Test directory (unique name per pid, like `config.rs`), dropped on
/// its own at the end of the test (`Drop`): a forgotten manual cleanup
/// in a test would leave the directory behind forever.
struct DroppedDir(PathBuf);

impl std::ops::Deref for DroppedDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for DroppedDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_dir(name: &str) -> DroppedDir {
    let d = std::env::temp_dir().join(format!("alpheus-{}-painter-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    DroppedDir(d)
}

fn fonts_root() -> PathBuf {
    std::env::var_os("ALPHEUS_POLICES").map(PathBuf::from).expect("ALPHEUS_POLICES required (nix develop) to build this test's fixtures")
}

/// Shifts every table offset of a standalone sfnt (TrueType/OpenType,
/// not a collection) by `shift`: its header (12 bytes), then
/// `numTables` 16-byte records (tag, checksum, offset, length), where
/// only the offset field (3rd, at bytes 8..12 of the record) changes.
/// Needed to pack a standalone font into a collection: its table
/// offsets are absolute (from the start of the FILE), so wrong once the
/// font is moved to another position in the combined file.
fn shift_sfnt_tables(sfnt: &mut [u8], shift: u32) {
    let num_tables = u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize;
    for i in 0..num_tables {
        let o = 12 + i * 16 + 8;
        let old = u32::from_be_bytes([sfnt[o], sfnt[o + 1], sfnt[o + 2], sfnt[o + 3]]);
        sfnt[o..o + 4].copy_from_slice(&(old + shift).to_be_bytes());
    }
}

/// Assembles a `.ttc` file (a collection) from whole TTF fonts: `ttcf`
/// magic, version (ignored by ttf-parser), face count, their offsets,
/// then the fonts glued end to end (table offsets shifted to their
/// position in the combined file, or the whole collection is
/// malformed). Used to check that a collection only brings the chosen
/// face into the final database, never every other face in the same
/// file (the trap this fix targets).
fn build_ttc(directory: &Path, faces: &[&str]) -> PathBuf {
    let root = fonts_root();
    let mut contents: Vec<Vec<u8>> = faces.iter().map(|f| std::fs::read(root.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"))).collect();
    let header = 12 + 4 * contents.len() as u32;
    let mut offsets = Vec::new();
    let mut pos = header;
    for c in &contents {
        offsets.push(pos);
        pos += c.len() as u32;
    }
    for (c, &offset) in contents.iter_mut().zip(&offsets) {
        shift_sfnt_tables(c, offset);
    }
    let mut ttc = Vec::new();
    ttc.extend_from_slice(b"ttcf");
    ttc.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // version 1.0, read but ignored
    ttc.extend_from_slice(&(contents.len() as u32).to_be_bytes());
    for d in &offsets {
        ttc.extend_from_slice(&d.to_be_bytes());
    }
    for c in &contents {
        ttc.extend_from_slice(c);
    }
    let path = directory.join("built.ttc");
    std::fs::write(&path, &ttc).unwrap();
    path
}

#[test]
fn ttc_collection_one_face_per_chosen_weight() {
    let directory = test_dir("ttc");
    let path = build_ttc(&directory, &["JetBrainsMonoNerdFontMono-Regular.ttf", "JetBrainsMonoNerdFontMono-Bold.ttf"]);
    // No fallback: this test is only about the collection's own face
    // selection. The default fallback (DejaVu Sans Mono) is absent
    // from this isolated directory, which would otherwise make
    // `from_dir` fall back to scanning every system font (same as a
    // genuinely missing family) — unrelated to what this test checks.
    let font = FontConfig { fallback: Vec::new(), ..FontConfig::default() };
    let (p, w) = Fonts::from_dir(Some(&directory), &font, DEFAULT_HEIGHT).expect("the built collection contains a monospace font");
    assert_eq!(w, None, "{w:?}");
    // Regular + bold, each once: not 4 (the collection loaded twice by
    // `load_font_source`, once per chosen weight).
    assert_eq!(p.loaded_faces(), 2, "faces loaded from the collection: {}", p.loaded_faces());
    // Not enough on its own: "2 faces" could also come from a
    // same-named system font (JetBrainsMono Nerd Font Mono is installed
    // on this machine) if the built collection had in fact not been
    // read. Check that it really is THAT one: every loaded face has
    // `path` as its source, and both of the collection's indices (0 and
    // 1) are there, with no duplicate or extra face.
    let mut index: Vec<u32> = Vec::new();
    for f in p.fs.db().faces() {
        let source = match &f.source {
            fontdb::Source::File(c) => c,
            // `fontdb` memory-maps files (the "memmap" feature): the
            // path stays reachable, right next to the mapped bytes.
            fontdb::Source::SharedFile(c, _) => c,
            other => panic!("unexpected source (not a file): {other:?}"),
        };
        assert_eq!(source, &path, "unexpected source (not the built collection): {}", source.display());
        index.push(f.index);
    }
    index.sort_unstable();
    assert_eq!(index, vec![0, 1], "indices of the faces loaded from the collection");
}

/// Fake face (no real file: `push_face_info` does not read the source),
/// to test the selection logic without depending on the fonts present
/// on the machine running the test.
fn fake_face(family: &str, path: &str, style: fontdb::Style) -> fontdb::FaceInfo {
    fontdb::FaceInfo {
        id: fontdb::ID::dummy(),
        source: fontdb::Source::File(path.into()),
        index: 0,
        families: vec![(family.to_string(), fontdb::Language::English_UnitedStates)],
        post_script_name: String::new(),
        style,
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        monospaced: true,
    }
}

#[test]
fn no_upright_face_treats_an_italic_only_family_as_absent() {
    let mut db = fontdb::Database::new();
    db.push_face_info(fake_face("Fake Family", "/fake-italic.ttf", fontdb::Style::Italic));
    // Present in the database, but only in italic: absent in the sense
    // of `from_dir` (replacement and warning expected).
    assert!(no_upright_face(&db, "Fake Family"), "a family with only italic must count as absent");
    db.push_face_info(fake_face("Fake Family", "/fake-upright.ttf", fontdb::Style::Normal));
    // One upright face is now enough to consider it present.
    assert!(!no_upright_face(&db, "Fake Family"));
}

#[test]
fn deterministic_replacement_by_name_then_path() {
    // Pure sorting logic (no real files: `push_face_info` doesn't read
    // the source): insertion order must not decide, only the family
    // name then the path.
    let mut db = fontdb::Database::new();
    let face = |family: &str, path: &str| fontdb::FaceInfo {
        id: fontdb::ID::dummy(),
        source: fontdb::Source::File(path.into()),
        index: 0,
        families: vec![(family.to_string(), fontdb::Language::English_UnitedStates)],
        post_script_name: String::new(),
        style: fontdb::Style::Normal,
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        monospaced: true,
    };
    // Inserted first, but not first alphabetically.
    db.push_face_info(face("Zorro Mono", "/z/a.ttf"));
    db.push_face_info(face("Alpha Mono", "/b/z.ttf"));
    db.push_face_info(face("Alpha Mono", "/a/a.ttf"));
    assert_eq!(find_replacement(db.faces()), Some("Alpha Mono".into()));
}

#[test]
fn wide_char_in_last_column_and_grid_wider_than_buffer() {
    let mut p = fonts();
    let av = p.advance(1);
    // Narrow buffer (3 cells), sized for itself.
    let (w, h) = ((av * 3.0).ceil() as u32, DEFAULT_HEIGHT);
    let mut px = vec![0u8; (w * h * 4) as usize];
    // Grid much wider (10 cells) than the buffer, with a wide character
    // (CJK, 2 columns) placed right at the far edge, in inverted style
    // (accent background): also exercises the per-cell background, not
    // just the glyph drawing.
    let mut g = Grid::empty(10);
    g.write(8, 10, "日", Style::ACTIVE);
    p.paint(&g, &Roles::default(), &mut px, w, h, 1); // must not panic
    // Every pixel of the buffer stays well-formed (opaque alpha): no
    // write overflowed its computed location.
    for o in (0..px.len()).step_by(4) {
        assert_eq!(px[o + 3], 0xff, "unpainted pixel at byte {o}");
    }
}

// ---- bar shapes (shapes spec § 3) ----

/// The layout tests' bar (launcher, workspaces, title, clock), hues on.
fn fixture_bar() -> crate::state::Bar {
    use crate::state::{Bar, Msg};
    let mut b = Bar::new(crate::config::BarConfig::default());
    b.apply(Msg::Compositor(Some(crate::compositor::example())));
    b.apply(Msg::Clock(crate::clock::LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 }));
    b.module_colors = ModuleColors::Full;
    b
}

/// A bar `columns` wide at `scale`: its physical size and a buffer full of `junk`.
fn bar_buffer(p: &mut Fonts, columns: u16, scale: i32, junk: u8) -> (u32, u32, Vec<u8>) {
    let av = p.advance(scale);
    let (w, h) = ((av * f32::from(columns)).ceil() as u32, DEFAULT_HEIGHT * scale as u32);
    (w, h, vec![junk; (w * h * 4) as usize])
}

/// `flat` stays today's bar to the byte (shapes spec § 3): `paint_bar` under flat writes every
/// byte `paint` writes, whatever the buffer held, the opacity, the module colours or the groups.
#[test]
fn paint_bar_under_flat_is_paint_to_the_byte() {
    let pal = distinct();
    let mut p = fonts();
    for (columns, scale) in [(100u16, 1), (60, 1), (100, 2), (40, 2)] {
        let bar = fixture_bar();
        let grid = bar.grid("HDMI-A-1", columns);
        // An underline grid's blocks, handed to flat: it must ignore them.
        let mut under = fixture_bar();
        under.style = BarStyle::Underline;
        let groups = under.grid("HDMI-A-1", columns).groups().to_vec();
        assert!(!groups.is_empty());
        let (w, h, mut old) = bar_buffer(&mut p, columns, scale, 0x5a);
        let mut new = vec![0xa5u8; old.len()];
        p.paint(&grid, &pal, &mut old, w, h, scale);
        let bg = Background { style: BarStyle::Flat, opacity: 0.3, module_colors: ModuleColors::Pills, groups: &groups };
        p.paint_bar(&grid, &pal, &bg, &mut new, w, h, scale);
        assert!(old == new, "{columns} columns at scale {scale}: flat must be today's paint");
    }
    assert_eq!(p.corner_masks(), 0, "flat never makes a corner mask");
}

/// (r, g, b, a) of pixel (x, y) in a `w`-wide buffer.
fn rgba(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * w + x) * 4) as usize;
    [px[o + 2], px[o + 1], px[o], px[o + 3]]
}

fn opaque(c: Rgb) -> [u8; 4] {
    [c.0, c.1, c.2, 255]
}

/// A 20-cell grid: an active workspace on cells 1..4, blanks elsewhere; one status block on
/// cells 6..12 (hue blue) and one clock block on 14..19 (no hue).
fn underline_fixture() -> (Grid, Vec<crate::grid::Group>) {
    use crate::grid::{Group, GroupKind};
    let mut g = Grid::empty(20);
    g.write(1, 20, "   ", Style::ACTIVE);
    let groups = vec![
        Group { kind: GroupKind::Status, start: 6, end: 12, hue: Some(Hue::Blue) },
        Group { kind: GroupKind::Clock, start: 14, end: 19, hue: None },
    ];
    (g, groups)
}

/// `underline` (shapes spec § 3): the bar on bar_bg, the blocks on pill_bg, a one logical px
/// bottom line in the border colour, two logical px of accent under the active workspace; the
/// blocks' hues underline them under the module colours only.
#[test]
fn underline_paints_blocks_a_bottom_line_and_the_active_underline() {
    let pal = distinct();
    let mut p = fonts();
    let (grid, groups) = underline_fixture();
    for scale in [1, 2] {
        let s = scale as u32;
        let (w, h, mut px) = bar_buffer(&mut p, 20, scale, 0x33);
        let av = p.advance(scale);
        let mid = |col: f32| ((col + 0.5) * av) as u32;
        let bg = Background { style: BarStyle::Underline, opacity: 0.8, module_colors: ModuleColors::None, groups: &groups };
        p.paint_bar(&grid, &pal, &bg, &mut px, w, h, scale);
        let border = pal.border_over(pal.bar_bg);
        for x in [0, mid(8.0), w - 1] {
            assert_eq!(rgba(&px, w, x, h - 1), opaque(border), "the bottom line at x {x}, scale {scale}");
            assert_eq!(rgba(&px, w, x, h - s), opaque(border), "one logical px thick, scale {scale}");
        }
        assert_eq!(rgba(&px, w, mid(0.0), h - s - 1), opaque(pal.bar_bg), "outside a block: bar_bg");
        assert_eq!(rgba(&px, w, mid(8.0), h / 2), opaque(pal.pill_bg), "a block: pill_bg");
        assert_eq!(rgba(&px, w, mid(16.0), h / 2), opaque(pal.pill_bg));
        assert_eq!(rgba(&px, w, mid(13.0), h / 2), opaque(pal.bar_bg), "between blocks");
        assert_eq!(rgba(&px, w, mid(8.0), h - s - 1), opaque(pal.pill_bg), "no hue underline under none");
        for y in [h - 3 * s, h - s - 1] {
            assert_eq!(rgba(&px, w, mid(2.0), y), opaque(pal.accent), "the active underline at y {y}, scale {scale}");
        }
        assert_eq!(rgba(&px, w, mid(2.0), h - 3 * s - 1), opaque(pal.bar_bg), "two logical px only: the cell above, unfilled");
        assert_eq!(rgba(&px, w, mid(0.0), h - 2 * s), opaque(pal.bar_bg), "no underline off the active cells");
        // Under the module colours, a block with a hue is underlined in it; one without is not.
        for mc in [ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills] {
            let bg = Background { module_colors: mc, ..bg };
            p.paint_bar(&grid, &pal, &bg, &mut px, w, h, scale);
            assert_eq!(rgba(&px, w, mid(8.0), h - 2 * s), opaque(pal.hues.blue), "{mc:?}: the status block's blue");
            assert_eq!(rgba(&px, w, mid(16.0), h - 2 * s), opaque(pal.pill_bg), "{mc:?}: the clock has no hue");
            assert_eq!(rgba(&px, w, mid(2.0), h - 2 * s), opaque(pal.accent), "{mc:?}: the active workspace keeps its accent");
        }
    }
    assert_eq!(p.corner_masks(), 0, "underline is square: no corner mask");
}

/// 2026-10-08: under underline the active workspace has no accent fill; its text is
/// `accent` and the 2 px accent underline marks it, visible against the pixels just above it.
/// An urgent workspace keeps its colour.
#[test]
fn underline_marks_the_active_workspace_by_its_text_and_line_not_a_fill() {
    use crate::grid::{Group, GroupKind};
    let pal = distinct();
    let mut p = fonts();
    let mut grid = Grid::empty(20);
    grid.write(1, 2, " ", Style::ACTIVE);
    grid.write(2, 3, "1", Style::ACTIVE);
    grid.write(3, 4, " ", Style::ACTIVE);
    grid.write(5, 6, "3", Style::URGENT);
    let groups = vec![Group { kind: GroupKind::Workspaces, start: 0, end: 7, hue: None }];
    for scale in [1, 2] {
        let s = scale as u32;
        let (w, h, mut px) = bar_buffer(&mut p, 20, scale, 0x33);
        let av = p.advance(scale);
        let cell = |col: u32| ((col as f32 * av).round() as u32, ((col + 1) as f32 * av).round() as u32);
        let bg = Background { style: BarStyle::Underline, opacity: 0.8, module_colors: ModuleColors::None, groups: &groups };
        p.paint_bar(&grid, &pal, &bg, &mut px, w, h, scale);
        let (x0, x1) = (cell(1).0, cell(3).1);
        let above = h - 3 * s;
        let colours: Vec<[u8; 4]> = (x0..x1).flat_map(|x| (0..above).map(move |y| (x, y))).map(|(x, y)| rgba(&px, w, x, y)).collect();
        assert!(!colours.contains(&opaque(pal.accent_container)), "scale {scale}: no accent fill on the active workspace");
        assert!(!colours.contains(&opaque(pal.on_accent)), "scale {scale}: no on_accent text");
        let (d0, d1) = cell(2);
        let digit = (d0..d1).any(|x| (0..above).any(|y| rgba(&px, w, x, y) == opaque(pal.accent)));
        assert!(digit, "scale {scale}: the active workspace's text in accent");
        assert_eq!(rgba(&px, w, (x0 + x1) / 2 - 1, above / 2), opaque(pal.pill_bg), "scale {scale}: the block's own fill behind it");
        for x in [x0, (x0 + x1) / 2, x1 - 1] {
            for y in [h - 3 * s, h - s - 1] {
                assert_eq!(rgba(&px, w, x, y), opaque(pal.accent), "scale {scale}: the underline at ({x}, {y})");
            }
            assert_ne!(rgba(&px, w, x, above - 1), opaque(pal.accent), "scale {scale}: visible: the pixel above it differs at x {x}");
        }
        let (u0, u1) = cell(5);
        let urgent = (u0..u1).any(|x| (0..above).any(|y| rgba(&px, w, x, y) == opaque(pal.urgent)));
        assert!(urgent, "scale {scale}: an urgent workspace keeps its colour");
        assert_eq!(rgba(&px, w, (u0 + u1) / 2, h - 2 * s), opaque(pal.pill_bg), "scale {scale}: no underline under it");
    }
}

/// Every pixel of an underline bar is written, opaque: the buffer's previous frame never shows.
#[test]
fn underline_writes_every_pixel_opaque() {
    let pal = distinct();
    let mut p = fonts();
    let (grid, groups) = underline_fixture();
    let (w, h, mut px) = bar_buffer(&mut p, 20, 1, 0x00);
    let bg = Background { style: BarStyle::Underline, opacity: 0.8, module_colors: ModuleColors::Icons, groups: &groups };
    p.paint_bar(&grid, &pal, &bg, &mut px, w, h, 1);
    assert!(px.chunks(4).all(|c| c[3] == 0xff), "a transparent pixel left");
}

#[test]
fn paint_bar_with_a_short_buffer_paints_nothing_and_never_panics() {
    let mut p = fonts();
    let (grid, groups) = underline_fixture();
    let mut px = vec![7u8; 16];
    for style in BarStyle::ALL {
        let bg = Background { style, opacity: 0.8, module_colors: ModuleColors::Pills, groups: &groups };
        p.paint_bar(&grid, &distinct(), &bg, &mut px, 100, 22, 1);
        assert_eq!(px, vec![7u8; 16], "{style:?}");
    }
}

mod graphs {
    use super::*;
    use crate::list::Series;

    const BG: Rgb = Rgb(10, 20, 30);

    fn px(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0u8; (w * h * 4) as usize];
        rect(&mut v, w, h, (0, w), (0, h), BG);
        v
    }

    fn get(v: &[u8], w: u32, x: u32, y: u32) -> Rgb {
        let o = ((y * w + x) * 4) as usize;
        Rgb(v[o + 2], v[o + 1], v[o])
    }

    fn graph(points: Vec<Option<u16>>, ink: Tint, fill: bool) -> Graph {
        Graph { row: 0, col: 0, width: 0, rows: 0, series: vec![Series { points, ink, fill }] }
    }

    /// Body rows of a column: the first row (from the top of the area) holding something else than `BG`
    /// and the baseline, or None.
    fn top_of(v: &[u8], w: u32, x: u32, y0: u32, bottom: u32) -> Option<u32> {
        (y0..bottom).find(|&y| get(v, w, x, y) != BG)
    }

    #[test]
    fn mix_is_rounded_per_channel() {
        assert_eq!(mix(Rgb(255, 0, 100), Rgb(0, 0, 0), 40), Rgb(102, 0, 40));
        assert_eq!(mix(Rgb(255, 0, 100), Rgb(0, 0, 0), 0), Rgb(0, 0, 0));
        assert_eq!(mix(Rgb(1, 0, 0), Rgb(0, 0, 0), 50), Rgb(1, 0, 0), "half rounds up");
        assert_eq!(mix(Rgb(0, 0, 0), Rgb(1, 1, 1), 40), Rgb(1, 1, 1), "0.6 rounds up");
        assert_eq!(mix(Rgb(255, 0, 100), Rgb(1, 1, 1), 100), Rgb(255, 0, 100));
    }

    /// 60 points over fewer than 60 px: some blocks are empty, the others tile the width exactly.
    #[test]
    fn plot_tiles_columns_exactly_at_every_scale() {
        let roles = distinct();
        let ink = color(&roles, BG, Tint::Hue(Hue::Cyan));
        for scale in 1..=3u32 {
            let (w, h) = (80 * scale, 40 * scale);
            let (x0, x1, y0, y1) = (10 * scale, 10 * scale + 37 * scale, 4 * scale, 4 * scale + 20 * scale + scale);
            let g = graph(vec![Some(1000); 60], Tint::Hue(Hue::Cyan), true);
            let mut v = px(w, h);
            plot(&mut v, w, h, (x0, y0, x1, y1), &g, &roles, BG, scale, scale as i32);
            let bottom = y1 - scale;
            for x in 0..w {
                let inside = (x0..x1).contains(&x);
                assert_eq!(get(&v, w, x, y0) == ink, inside, "scale {scale}, column {x}: ink at the area's top row");
                for y in y0 + scale..bottom {
                    assert_eq!(get(&v, w, x, y) != BG, inside, "scale {scale}, column {x}, row {y}: body");
                }
            }
        }
    }

    #[test]
    fn plot_heights_and_gaps() {
        let roles = distinct();
        let (w, h, stroke) = (40, 30, 1);
        let (y0, y1) = (2, 2 + 20 + stroke); // H = 20, bottom = 22
        let bottom = y1 - stroke;
        let g = graph(vec![None, Some(0), Some(500), Some(1000)], Tint::Hue(Hue::Cyan), true);
        let mut v = px(w, h);
        plot(&mut v, w, h, (0, y0, 40, y1), &g, &roles, BG, stroke, 1);
        let line = roles.border_over(BG);
        for x in [0, 5, 9, 10, 15, 19] {
            assert_eq!(top_of(&v, w, x, y0, bottom), None, "block 0 or 1 at {x}: only the baseline");
        }
        for x in 0..40 {
            assert_eq!(get(&v, w, x, bottom), line, "the baseline under every column");
        }
        assert_eq!(top_of(&v, w, 20, y0, bottom), Some(bottom - 10));
        assert_eq!(top_of(&v, w, 29, y0, bottom), Some(bottom - 10));
        assert_eq!(top_of(&v, w, 30, y0, bottom), Some(y0));
        assert_eq!(top_of(&v, w, 39, y0, bottom), Some(y0));
        // The minimum stroke: a tiny value is `scale` px tall, at any scale.
        for scale in 1..=3u32 {
            let g = graph(vec![Some(1)], Tint::Hue(Hue::Cyan), true);
            let mut v = px(w, h);
            plot(&mut v, w, h, (0, 0, 10, 20 + scale), &g, &roles, BG, scale, scale as i32);
            let bottom = 20;
            assert_eq!(top_of(&v, w, 3, 0, bottom), Some(bottom - scale), "scale {scale}");
        }
    }

    #[test]
    fn plot_writes_only_exact_colours() {
        let roles = distinct();
        let (w, h, stroke) = (47, 30, 2);
        let g = Graph {
            row: 0,
            col: 0,
            width: 0,
            rows: 0,
            series: vec![
                Series { points: (0..60).map(|i| Some(i * 16)).collect(), ink: Tint::Hue(Hue::Cyan), fill: true },
                Series { points: (0..60).map(|i| (i % 7 != 0).then_some(1000 - i * 10)).collect(), ink: Tint::FgDim, fill: false },
            ],
        };
        let mut v = px(w, h);
        plot(&mut v, w, h, (3, 2, 44, 28), &g, &roles, BG, stroke, 1);
        let cyan = color(&roles, BG, Tint::Hue(Hue::Cyan));
        let allowed = [BG, roles.border_over(BG), mix(cyan, BG, BODY_ALPHA), cyan, roles.fg_dim];
        for y in 0..h {
            for x in 0..w {
                let c = get(&v, w, x, y);
                assert!(allowed.contains(&c), "({x}, {y}) is {c:?}");
            }
        }
    }

    #[test]
    fn plot_line_series_draws_only_its_top() {
        let roles = distinct();
        let (w, h) = (40, 30);
        let g = graph(vec![Some(500); 10], Tint::Hue(Hue::Cyan), false);
        let mut v = px(w, h);
        plot(&mut v, w, h, (0, 0, 40, 21), &g, &roles, BG, 1, 1);
        let cyan = color(&roles, BG, Tint::Hue(Hue::Cyan));
        assert_eq!(get(&v, w, 5, 10), cyan, "the line at bottom (20) - 10");
        for y in (0..10).chain(11..20) {
            assert_eq!(get(&v, w, 5, y), BG, "row {y}");
        }
    }

    #[test]
    fn plot_draws_lines_over_the_fills() {
        let roles = distinct();
        let (w, h) = (20, 30);
        let g = Graph {
            row: 0,
            col: 0,
            width: 0,
            rows: 0,
            series: vec![
                Series { points: vec![Some(500); 4], ink: Tint::FgDim, fill: false },
                Series { points: vec![Some(1000); 4], ink: Tint::Hue(Hue::Cyan), fill: true },
            ],
        };
        let mut v = px(w, h);
        plot(&mut v, w, h, (0, 0, 20, 21), &g, &roles, BG, 1, 1);
        assert_eq!(get(&v, w, 5, 10), roles.fg_dim, "the line wins over the body, whatever the order written");
    }

    #[test]
    fn plot_clips_to_the_buffer() {
        let roles = distinct();
        let (w, h) = (20, 12);
        let g = graph(vec![Some(1000); 7], Tint::Hue(Hue::Cyan), true);
        let mut v = px(w, h);
        let len = v.len();
        plot(&mut v, w, h, (10, 4, 50, 40), &g, &roles, BG, 1, 1);
        assert_eq!(v.len(), len);
        assert_ne!(get(&v, w, 19, 4), BG, "the visible part is drawn");
        assert_eq!(get(&v, w, 9, 6), BG, "left of the area: untouched");
        let mut v = px(w, h);
        plot(&mut v, w, h, (30, 20, 60, 50), &g, &roles, BG, 1, 1);
        assert_eq!(v, px(w, h), "wholly outside: nothing written");
        plot(&mut v, w, h, (5, 5, 5, 9), &g, &roles, BG, 1, 1);
        plot(&mut v, w, h, (5, 9, 9, 5), &g, &roles, BG, 1, 1);
        plot(&mut v, w, h, (0, 0, 10, 1), &g, &roles, BG, 3, 1);
    }

    #[test]
    fn paint_boxed_draws_a_graph_where_its_cells_are() {
        let mut p = fonts();
        let roles = Roles::default(); // what `Surface::paint` uses
        let mut b = Boxed { rows: (0..6).map(|_| text_row(12, "")).collect(), ..Boxed::default() };
        b.graphs.push(Graph {
            row: 2,
            col: 2,
            width: 8,
            rows: 2,
            series: vec![Series { points: vec![Some(1000); 8], ink: Tint::Hue(Hue::Cyan), fill: false }],
        });
        let scale = 1;
        let s = Surface::paint(&mut p, &b, scale);
        let cyan = Surface::opaque(roles.hues.cyan);
        let (f, rh) = (&s.frame, s.rh);
        let inset = 2 * scale as u32;
        let (x0, x1) = (f.side + p.edge(2), f.side + p.edge(10));
        let (y0, y1) = (f.top + 2 * rh + inset, f.top + 4 * rh - inset);
        let mut seen = 0;
        for y in 0..s.h {
            for x in 0..s.w {
                if s.at(x, y) == cyan {
                    seen += 1;
                    assert!((x0..x1).contains(&x) && (y0..y1).contains(&y), "cyan at ({x}, {y}) outside {x0}..{x1} x {y0}..{y1}");
                }
            }
        }
        assert!(seen >= (x1 - x0) as usize - 8, "the line is drawn: {seen}");
        assert_eq!(
            (s.at(x0 + 1, y0), s.at(x0 + 1, y0 - 1)),
            (cyan, Surface::opaque(Roles::default().popup_bg)),
            "the line sits on the area's first row"
        );
    }
}
