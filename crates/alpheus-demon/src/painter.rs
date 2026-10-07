//! Drawing of the grid into an ARGB8888 buffer (wl_shm: bytes B, G, R, A),
//! cell by cell: each cell's background, then a glyph shaped by cosmic-text
//! and placed at the left edge of its cell. The grid stays aligned even
//! when a fallback font (emoji, CJK) has a different advance.
//!
//! Bounded memory: the font database only holds the faces actually useful
//! (cosmic-text otherwise parses every monospace face in its database up
//! front), and the glyph cache is cleared past `MAX_CACHE` entries.
//!
//! Masking: a character with no glyph anywhere (`family`, nor any
//! `fallback`) is simply not drawn, rather than drawn as the resolved
//! face's `.notdef` "missing glyph" box (`shape`'s `gl.glyph_id == 0`
//! check). Its cell keeps the width `Grid::write` already gave it — only
//! the paint is skipped.

use crate::config::FontConfig;
use crate::grid::{CONT, Grid, Tint};
use crate::list::Boxed;
use alpheus_commun::theme::{Palette, Rgb};
use cosmic_text::{Attrs, Buffer, CacheKey, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, fontdb};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Rasterized glyphs kept at most (≈ 1 MB at 14 px).
pub const MAX_CACHE: usize = 2048;

/// Logical px between a boxed surface's edge and its rows' cells, on each side, before the
/// pad cell every row carries.
const FRAME_SIDE: u32 = 4;
/// Logical px radius of a boxed surface's rounded corners.
const FRAME_RADIUS: u32 = 6;
/// Pixels between a row's edges and the ink of a capital letter in it: what `Frame::new`
/// measures the visible margin from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ink {
    /// Left of the cell to the first inked column.
    pub left: u32,
    /// Top of the row to the first inked line.
    pub top: u32,
    /// Last inked line to the bottom of the row.
    pub bottom: u32,
}

/// The pixel frame of a boxed surface (popups, the launcher), physical px: a `stroke` thick line
/// along the surface's edge with `radius` rounded corners, and the room between it and the
/// rows' cells. The room is not the same on every side: the visible gap between the stroke
/// and the text is, so the vertical insets make up for the leading a row carries above and
/// below its letters (`Ink`), and the sides count the pad cell each row starts and ends with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Left and right.
    pub side: u32,
    pub top: u32,
    pub bottom: u32,
    pub stroke: u32,
    pub radius: u32,
}

impl Frame {
    /// At `scale`, for cells `advance` wide and the `ink` of the font's capitals in a row.
    pub fn new(scale: i32, advance: f32, ink: Ink) -> Frame {
        let e = scale.max(1) as u32;
        let side = FRAME_SIDE * e;
        // Stroke to the first column's ink: the side room, the pad cell, the letter's own bearing.
        let gap = side + advance.round().max(0.0) as u32 + ink.left;
        Frame {
            side,
            top: gap.saturating_sub(ink.top).max(e),
            bottom: gap.saturating_sub(ink.bottom).max(e),
            stroke: e,
            radius: FRAME_RADIUS * e,
        }
    }

    /// What the frame adds to the cells' width and height.
    pub fn extra(&self) -> (u32, u32) {
        (2 * self.side, self.top + self.bottom)
    }
}

/// Everything `glyph` needs to blend one shaped glyph, for a given (char,
/// bold) pair at the current scale — computed once by `shaped`, cached, and
/// reused on every later cell, column, scale or redraw.
///
/// A cell's left edge (`x0`, see `paint`'s `edge`) is always an exact
/// integer physical pixel: shifting the glyph by an integer leaves its
/// swash subpixel bin, and therefore its rasterized `cache_key`, unchanged
/// (`cosmic_text::glyph_cache::SubpixelBin` bins on the fractional part of
/// the position alone), and shifts its physical x by that same integer.
/// `dx` below is this glyph's physical x measured at `x0 = 0`: the real
/// draw position for any cell is just `x0 + dx`, no reshaping needed.
#[derive(Clone, Copy)]
struct ShapedGlyph {
    cache_key: CacheKey,
    /// Physical x at `x0 = 0`; add the cell's integer left edge to place it.
    dx: i32,
    /// Physical y (the line's baseline plus the glyph's own y): independent
    /// of the column, constant for the whole (single-line) buffer.
    y: i32,
    color_opt: Option<Color>,
}

/// Shapes `ch` (bold or not) at the buffer's current scale and metrics,
/// collecting every glyph cosmic-text produces for it (almost always
/// exactly one — a lone `char` can still decompose into several glyphs
/// through font fallback or shaping rules, so this never assumes a single
/// result).
///
/// Masking: a glyph whose id is 0 (`.notdef`, reserved by the OpenType spec
/// for exactly this — no face, neither `family` nor any configured
/// fallback, has a cmap entry for `ch`) is dropped here rather than kept
/// for `glyph` to draw. Left in, it would paint whatever "missing glyph"
/// box the resolved face happens to use for `.notdef` (the bug this masks:
/// window-manager spinners and odd title symbols outside every loaded
/// font). Dropping it here, before a `ShapedGlyph` is ever produced, costs
/// nothing extra in `glyph` and needs no change to `Grid::write`'s column
/// accounting: the cell stays exactly as wide as the character's own
/// width already made it, just with nothing painted into it.
fn shape(fs: &mut FontSystem, buffer: &mut Buffer, family: &str, ch: char, bold: bool) -> Vec<ShapedGlyph> {
    let mut s = [0u8; 4];
    buffer.set_text(fs, ch.encode_utf8(&mut s), attrs(family, bold), Shaping::Advanced);
    buffer.shape_until_scroll(fs, false);
    let mut shaped = Vec::new();
    for run in buffer.layout_runs() {
        for gl in run.glyphs.iter() {
            if gl.glyph_id == 0 {
                continue;
            }
            let phys = gl.physical((-gl.x, 0.0), 1.0);
            shaped.push(ShapedGlyph { cache_key: phys.cache_key, dx: phys.x, y: run.line_y as i32 + phys.y, color_opt: gl.color_opt });
        }
    }
    shaped
}

pub struct Fonts {
    fs: FontSystem,
    cache: SwashCache,
    buffer: Buffer,
    family: String,
    size: f32,
    height: u32,
    scale: i32,
    advance: f32,
    /// Advance already measured for each scale seen (a small table: in
    /// practice there are only ever a handful of distinct scales per
    /// session). Avoids reshaping "M" on every alternation.
    advances: Vec<(i32, f32)>,
    /// The frame already measured for each scale seen (like `advances`).
    frames: Vec<(i32, Frame)>,
    /// `shape`'s result, memoized per (char, bold): cosmic-text otherwise
    /// reshapes the exact same glyph on every cell, column and redraw.
    /// Bounded like `cache` (`MAX_CACHE`, cleared past it); also cleared on
    /// a scale change (`set_scale`), since a shape's `cache_key` and
    /// physical offsets are only valid for the metrics they were measured
    /// at.
    shapes: HashMap<(char, bool), Vec<ShapedGlyph>>,
}

pub fn color(p: &Palette, t: Tint) -> Rgb {
    match t {
        Tint::Bg => p.bg,
        Tint::Fg => p.fg,
        Tint::Accent => p.accent,
        Tint::Muted => p.muted,
        Tint::Surface => p.surface,
        Tint::Error => p.error,
    }
}

/// Number of cells in a physical width.
pub fn columns(width_phys: u32, advance: f32) -> u16 {
    if advance <= 0.0 {
        return 0;
    }
    ((width_phys as f32 / advance).floor() as u32).min(u16::MAX as u32) as u16
}

/// Cell under a point (x in logical px, like the Wayland pointer).
pub fn column_under(x_logical: f64, scale: i32, advance: f32) -> u16 {
    if advance <= 0.0 || x_logical <= 0.0 {
        return 0;
    }
    ((x_logical * scale.max(1) as f64) / advance as f64).floor().min(u16::MAX as f64) as u16
}

fn attrs(family: &str, bold: bool) -> Attrs<'_> {
    let a = Attrs::new().family(Family::Name(family));
    if bold { a.weight(Weight::BOLD) } else { a }
}

/// Flat fill over [x0, x1) × [0, height).
fn fill(px: &mut [u8], width: u32, x0: u32, x1: u32, height: u32, Rgb(r, g, b): Rgb) {
    for y in 0..height {
        for x in x0..x1.min(width) {
            // usize right away: `(y * width + x) * 4` can overflow u32 on a
            // large buffer before the conversion.
            let o = (y as usize * width as usize + x as usize) * 4;
            px[o..o + 4].copy_from_slice(&[b, g, r, 0xff]);
        }
    }
}

/// Flat fill over x ∈ [x.0, x.1), y ∈ [y.0, y.1), clipped to the buffer.
fn rect(px: &mut [u8], width: u32, height: u32, x: (u32, u32), y: (u32, u32), Rgb(r, g, b): Rgb) {
    for yy in y.0..y.1.min(height) {
        for xx in x.0..x.1.min(width) {
            let o = (yy as usize * width as usize + xx as usize) * 4;
            px[o..o + 4].copy_from_slice(&[b, g, r, 0xff]);
        }
    }
}

/// Draws `frame` along the edge of the buffer: straight strokes, and in each corner a quarter
/// ring anti-aliased on both of its edges, the part beyond it left transparent (the buffer is
/// premultiplied ARGB8888) and the part inside it the background's, over what the rows left.
fn draw_frame(px: &mut [u8], width: u32, height: u32, frame: &Frame, ink: Rgb, back: Rgb) {
    let (Rgb(sr, sg, sb), Rgb(br, bg, bb)) = (ink, back);
    let r = frame.radius.min(width / 2).min(height / 2);
    let s = frame.stroke.min(r.max(1));
    rect(px, width, height, (r, width.saturating_sub(r)), (0, s), ink);
    rect(px, width, height, (r, width.saturating_sub(r)), (height.saturating_sub(s), height), ink);
    rect(px, width, height, (0, s), (r, height.saturating_sub(r)), ink);
    rect(px, width, height, (width.saturating_sub(s), width), (r, height.saturating_sub(r)), ink);
    let (rf, sf) = (r as f32, s as f32);
    let mix =
        |stroke: u8, back: u8, ring: f32, inside: f32| ((stroke as f32 * ring + back as f32 * inside).round()).clamp(0.0, 255.0) as u8;
    for cy in 0..r {
        for cx in 0..r {
            // Distance of the pixel's centre to the corner circle's centre, in the corner's own
            // axes (the top left one): coverage of the outer and of the inner edge.
            let d = ((rf - cx as f32 - 0.5).powi(2) + (rf - cy as f32 - 0.5).powi(2)).sqrt();
            let outer = (rf - d + 0.5).clamp(0.0, 1.0);
            let inner = (rf - sf - d + 0.5).clamp(0.0, 1.0);
            let ring = outer - inner;
            let pixel = [mix(sb, bb, ring, inner), mix(sg, bg, ring, inner), mix(sr, br, ring, inner), (outer * 255.0).round() as u8];
            for (x, y) in [(cx, cy), (width - 1 - cx, cy), (cx, height - 1 - cy), (width - 1 - cx, height - 1 - cy)] {
                let o = (y as usize * width as usize + x as usize) * 4;
                if let Some(dst) = px.get_mut(o..o + 4) {
                    dst.copy_from_slice(&pixel);
                }
            }
        }
    }
}

/// Blends a glyph pixel (color and coverage) onto the background.
fn blend(px: &mut [u8], width: u32, height: u32, x: i32, y: i32, c: Color) {
    if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
        return;
    }
    let a = c.a() as u32;
    if a == 0 {
        return;
    }
    // x and y are positive and in bounds here: usize right away.
    let o = (y as usize * width as usize + x as usize) * 4;
    let src = [c.b(), c.g(), c.r()];
    for k in 0..3 {
        px[o + k] = ((src[k] as u32 * a + px[o + k] as u32 * (255 - a)) / 255) as u8;
    }
    px[o + 3] = 0xff;
}

fn has_family(f: &fontdb::FaceInfo, family: &str) -> bool {
    f.families.iter().any(|(n, _)| n == family)
}

/// True if `family` has no upright face (normal style, vertical posture) in
/// the database. A family that only has italic or oblique faces is then
/// treated as absent (replacement and warning, see `from_dir`) rather than
/// silently loading no face at all.
fn no_upright_face(db: &fontdb::Database, family: &str) -> bool {
    !db.faces().any(|f| has_family(f, family) && f.style == fontdb::Style::Normal)
}

/// Deterministic sort key for a face, to pick a replacement: by family name
/// then path then index, never by `fontdb`'s traversal order (itself
/// `read_dir`, which changes from one machine to another).
fn sort_key(f: &fontdb::FaceInfo) -> (String, String, u32) {
    let family = f.families.first().map(|(n, _)| n.clone()).unwrap_or_default();
    let path = match &f.source {
        fontdb::Source::File(p) => p.display().to_string(),
        _ => String::new(),
    };
    (family, path, f.index)
}

/// Monospace replacement: among upright faces (normal style), the first by
/// `sort_key`, for a stable pick from one machine to another.
fn find_replacement<'a>(faces: impl Iterator<Item = &'a fontdb::FaceInfo>) -> Option<String> {
    faces
        .filter(|f| f.monospaced && f.style == fontdb::Style::Normal)
        .min_by_key(|f| sort_key(f))
        .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
}

/// Faces to load for a family: the regular and the bold one (normal style,
/// weight closest to 400 and to 700), with no duplicate. Returned as cloned
/// `FaceInfo` (id reset to `ID::dummy()`, see `from_dir`) rather than as a
/// `Source`: a font packaged as a collection (.ttc, common for system CJK
/// fonts) must only bring THIS face into the final database, never every
/// other face in its file.
fn useful_faces(inventory: &fontdb::Database, family: &str) -> Vec<fontdb::FaceInfo> {
    let faces: Vec<&fontdb::FaceInfo> = inventory.faces().filter(|f| has_family(f, family) && f.style == fontdb::Style::Normal).collect();
    let closest = |target: i32| faces.iter().min_by_key(|f| (f.weight.0 as i32 - target).abs()).map(|f| f.id);
    let mut ids = Vec::new();
    for id in [closest(400), closest(700)].into_iter().flatten() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.into_iter()
        .filter_map(|id| inventory.face(id))
        .map(|f| {
            let mut info = f.clone();
            info.id = fontdb::ID::dummy();
            info
        })
        .collect()
}

impl Fonts {
    pub fn load(font: &FontConfig, height: u32) -> Result<(Fonts, Option<String>), String> {
        let directory = std::env::var_os("ALPHEUS_POLICES")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| option_env!("ALPHEUS_POLICES").map(PathBuf::from));
        Self::from_dir(directory.as_deref(), font, height)
    }

    pub fn from_dir(directory: Option<&Path>, font: &FontConfig, height: u32) -> Result<(Fonts, Option<String>), String> {
        // 1. Inventory (metadata only, discarded afterwards): the package's
        //    directory, then the system fonts if a family is missing from it.
        let mut inventory = fontdb::Database::new();
        if let Some(d) = directory {
            inventory.load_fonts_dir(d);
        }
        let wanted = std::iter::once(&font.family).chain(&font.fallback);
        if wanted.clone().any(|f| no_upright_face(&inventory, f)) {
            inventory.load_system_fonts();
        }
        let mut warnings = Vec::new();
        let family = if no_upright_face(&inventory, &font.family) {
            // Absent, or present only in italic/oblique (no upright face):
            // treated the same, a replacement and a warning.
            let replacement = find_replacement(inventory.faces())
                .ok_or_else(|| format!("no monospace font available (font \"{}\" not found)", font.family))?;
            warnings.push(format!("font \"{}\" not found, replaced by \"{replacement}\"", font.family));
            replacement
        } else {
            font.family.clone()
        };
        // 2. Final database: only the useful faces.
        let mut db = fontdb::Database::new();
        for f in useful_faces(&inventory, &family) {
            db.push_face_info(f);
        }
        for r in &font.fallback {
            let faces = useful_faces(&inventory, r);
            if faces.is_empty() {
                warnings.push(format!("fallback font \"{r}\" not found"));
            }
            for f in faces {
                db.push_face_info(f);
            }
        }
        drop(inventory);
        crate::memory::release_memory();
        db.set_monospace_family(family.clone());
        // The locale only steers cosmic-text's font fallback, never any
        // visible text.
        let mut fs = FontSystem::new_with_locale_and_db("en-US".into(), db);
        let buffer = Buffer::new(&mut fs, Metrics::new(font.size, height as f32));
        let mut p = Fonts {
            fs,
            cache: SwashCache::new(),
            buffer,
            family,
            size: font.size,
            height,
            scale: 0,
            advance: 0.0,
            advances: Vec::new(),
            frames: Vec::new(),
            shapes: HashMap::new(),
        };
        p.set_scale(1);
        let warning = (!warnings.is_empty()).then(|| warnings.join(" ; "));
        Ok((p, warning))
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    /// Faces in cosmic-text's database (memory guard).
    pub fn loaded_faces(&self) -> usize {
        self.fs.db().len()
    }

    /// Rasterized glyphs in cache (memory guard).
    pub fn cached_glyphs(&self) -> usize {
        self.cache.image_cache.len()
    }

    /// Memoized (char, bold) shapes (memory guard).
    pub fn shaped_glyphs(&self) -> usize {
        self.shapes.len()
    }

    /// Size and metrics for scale `e`; measures a cell's advance (memorized
    /// per scale: never remeasured twice). The glyph cache is NOT cleared
    /// here: its swash keys include the size, so glyphs from several scales
    /// coexist (screens at scale 1 and 2 drawn in alternation);
    /// `MAX_CACHE`, in `glyph`, remains the only memory bound.
    fn set_scale(&mut self, e: i32) {
        let e = e.max(1);
        if e == self.scale {
            return;
        }
        self.scale = e;
        // A shape's `cache_key` and physical offsets are only valid for the
        // metrics (font size) they were measured at: a scale change makes
        // every memoized shape stale.
        self.shapes.clear();
        let (t, h) = (self.size * e as f32, (self.height * e as u32) as f32);
        self.buffer.set_metrics_and_size(&mut self.fs, Metrics::new(t, h), None, Some(h));
        self.advance = match self.advances.iter().find(|(scale, _)| *scale == e) {
            Some((_, advance)) => *advance,
            None => {
                self.buffer.set_text(&mut self.fs, "M", attrs(&self.family, false), Shaping::Advanced);
                self.buffer.shape_until_scroll(&mut self.fs, false);
                let advance = self.buffer.layout_runs().flat_map(|r| r.glyphs.iter()).map(|g| g.w).next().unwrap_or(t * 0.6);
                self.advances.push((e, advance));
                advance
            }
        };
    }

    pub fn advance(&mut self, scale: i32) -> f32 {
        self.set_scale(scale);
        self.advance
    }

    pub fn paint(&mut self, grid: &Grid, palette: &Palette, px: &mut [u8], width: u32, height: u32, scale: i32) {
        if px.len() < (width as usize) * (height as usize) * 4 {
            return; // inconsistent buffer: do nothing rather than panic
        }
        self.set_scale(scale);
        fill(px, width, 0, width, height, color(palette, Tint::Bg));
        self.paint_cells(grid, palette, px, width, height, 0);
    }

    /// `grid`'s cells in a band `height` px tall, the first one `x0` px from the band's left edge:
    /// the cell backgrounds that differ from the surface's, then the glyphs. The band's own
    /// background is the caller's.
    fn paint_cells(&mut self, grid: &Grid, palette: &Palette, px: &mut [u8], width: u32, height: u32, x0: u32) {
        let advance = self.advance;
        let edge = |col: usize| x0 + ((col as f32) * advance).round() as u32;
        for (col, c) in grid.cells().iter().enumerate() {
            if c.style.bg != Tint::Bg {
                fill(px, width, edge(col), edge(col + 1), height, color(palette, c.style.bg));
            }
        }
        for (col, c) in grid.cells().iter().enumerate() {
            if c.ch == ' ' || c.ch == CONT {
                continue;
            }
            self.glyph(c.ch, c.style.bold, edge(col) as f32, color(palette, c.style.fg), px, width, height);
        }
    }

    /// The frame of boxed surfaces at `scale`, measured once per scale from the font itself.
    pub fn frame(&mut self, scale: i32) -> Frame {
        self.set_scale(scale);
        if let Some((_, f)) = self.frames.iter().find(|(s, _)| *s == self.scale) {
            return *f;
        }
        let ink = self.measure_ink();
        let frame = Frame::new(self.scale, self.advance, ink);
        self.frames.push((self.scale, frame));
        frame
    }

    /// The ink of a capital "E" in a row, at the current scale (a cap-height letter with a
    /// stem on its left: the same edges every text in the boxes starts from).
    fn measure_ink(&mut self) -> Ink {
        let row = (self.height * self.scale as u32) as i32;
        let (mut left, mut top, mut bottom) = (i32::MAX, i32::MAX, i32::MIN);
        for g in self.shaped('E', false) {
            self.cache.with_pixels(&mut self.fs, g.cache_key, Color::rgb(255, 255, 255), |dx, dy, c| {
                if c.a() > 0 {
                    left = left.min(g.dx + dx);
                    top = top.min(g.y + dy);
                    bottom = bottom.max(g.y + dy);
                }
            });
        }
        if top > bottom {
            // No glyph for it (cannot happen with a monospace face, but never a panic): a plain guess.
            return Ink { left: 0, top: (row / 4).max(0) as u32, bottom: (row / 4).max(0) as u32 };
        }
        Ink { left: left.max(0) as u32, top: top.max(0) as u32, bottom: (row - 1 - bottom).max(0) as u32 }
    }

    /// Paints a boxed surface `width` × `height` px: the surface's background, the rows' cells
    /// `frame.side` px from the left and `frame.top` from the top, each `row_height` px tall, the
    /// separator and divider lines, and last the frame, drawn at the surface's own edge (outside
    /// its rounded corners stays transparent). Rows that do not fit are not painted.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_boxed(&mut self, b: &Boxed, palette: &Palette, px: &mut [u8], width: u32, height: u32, row_height: u32, scale: i32) {
        if px.len() < (width as usize) * (height as usize) * 4 {
            return; // inconsistent buffer: do nothing rather than panic
        }
        let frame = self.frame(scale);
        fill(px, width, 0, width, height, color(palette, Tint::Bg));
        let band = width as usize * row_height as usize * 4;
        for (i, row) in b.rows.iter().enumerate() {
            let start = (frame.top as usize + i * row_height as usize) * width as usize * 4;
            let Some(slice) = px.get_mut(start..start + band) else { break };
            self.paint_cells(row, palette, slice, width, row_height, frame.side);
        }
        let ink = color(palette, Tint::Muted);
        let (w, h, s) = (width, height, frame.stroke);
        for &r in &b.separators {
            let y = frame.top + r as u32 * row_height + row_height.saturating_sub(s) / 2;
            rect(px, w, h, (s, w.saturating_sub(s)), (y, y + s), ink);
        }
        if let Some(c) = b.divider {
            let (left, right) = (self.edge(usize::from(c)), self.edge(usize::from(c) + 1));
            let x = frame.side + left + (right - left).saturating_sub(s) / 2;
            rect(px, w, h, (x, x + s), (s, h.saturating_sub(s)), ink);
        }
        draw_frame(px, w, h, &frame, ink, color(palette, Tint::Bg));
    }

    /// Left edge of cell `col`, px from the cells' origin, at the current scale.
    fn edge(&self, col: usize) -> u32 {
        ((col as f32) * self.advance).round() as u32
    }

    /// `shape`'s result for (`ch`, `bold`) at the current scale, memoized:
    /// cosmic-text otherwise reshapes the exact same glyph on every cell,
    /// column and redraw it appears in. Bounded like `cache` (`MAX_CACHE`):
    /// past the ceiling we start over from an empty memo, same as the
    /// rasterized glyph cache does.
    fn shaped(&mut self, ch: char, bold: bool) -> Vec<ShapedGlyph> {
        if let Some(s) = self.shapes.get(&(ch, bold)) {
            return s.clone();
        }
        if self.shapes.len() >= MAX_CACHE {
            self.shapes.clear();
        }
        let s = shape(&mut self.fs, &mut self.buffer, &self.family, ch, bold);
        self.shapes.insert((ch, bold), s.clone());
        s
    }

    #[allow(clippy::too_many_arguments)]
    fn glyph(&mut self, ch: char, bold: bool, x0: f32, ink: Rgb, px: &mut [u8], width: u32, height: u32) {
        // Bounded memory: past the ceiling, we start over from an empty
        // cache (the shown glyphs are rasterized again, once).
        if self.cache.image_cache.len() >= MAX_CACHE {
            self.cache.image_cache.clear();
            self.cache.outline_command_cache.clear();
        }
        let Rgb(r, g, b) = ink;
        let base = Color::rgb(r, g, b);
        // `x0` is always an exact integer physical pixel (`paint`'s `edge`
        // rounds it): adding it to `dx` (the shape measured at `x0 = 0`)
        // lands on the same subpixel bin, hence the same `cache_key`, as
        // reshaping at this exact `x0` would (see `ShapedGlyph`'s doc).
        let x0 = x0.round() as i32;
        for g in self.shaped(ch, bold) {
            let tint = g.color_opt.unwrap_or(base);
            self.cache.with_pixels(&mut self.fs, g.cache_key, tint, |dx, dy, c| {
                blend(px, width, height, x0 + g.dx + dx, g.y + dy, c);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_HEIGHT;
    use crate::grid::Style;

    fn fonts() -> Fonts {
        let (p, w) = Fonts::load(&FontConfig::default(), DEFAULT_HEIGHT)
            .expect("no monospace font available: run the tests inside `nix develop` (ALPHEUS_POLICES)");
        assert_eq!(w, None, "JetBrainsMono Nerd Font Mono expected (nix develop)");
        p
    }

    /// Frozen copy of `Fonts::glyph` exactly as it shaped text before the
    /// memo (B-I3): reshapes `ch` from scratch every single call, however
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
            for gl in run.glyphs.iter() {
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
    fn paint_reshaping_every_glyph(p: &mut Fonts, grid: &Grid, palette: &Palette, px: &mut [u8], width: u32, height: u32, scale: i32) {
        p.set_scale(scale);
        let advance = p.advance;
        let edge = |col: usize| ((col as f32) * advance).round() as u32;
        fill(px, width, 0, width, height, color(palette, Tint::Bg));
        for (col, c) in grid.cells().iter().enumerate() {
            if c.style.bg != Tint::Bg {
                fill(px, width, edge(col), edge(col + 1), height, color(palette, c.style.bg));
            }
        }
        for (col, c) in grid.cells().iter().enumerate() {
            if c.ch == ' ' || c.ch == CONT {
                continue;
            }
            glyph_reshaped_every_time(p, c.ch, c.style.bold, edge(col) as f32, color(palette, c.style.fg), px, width, height);
        }
    }

    #[test]
    fn memoized_shaping_is_pixel_identical_to_reshaping_every_glyph() {
        let mut memo = fonts();
        let mut fresh = fonts();
        let pal = Palette::default();
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
    /// `nix develop -c cargo test --release -p alpheus-demon --lib painter::tests::glyph_shaping_memo_speeds_up_paint -- --ignored --nocapture`
    #[test]
    #[ignore = "timing: run in release, numbers are meaningless in debug"]
    fn glyph_shaping_memo_speeds_up_paint() {
        let mut memo = fonts();
        let mut fresh = fonts();
        let pal = Palette::default();
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
        let tight = Frame::new(1, 8.25, Ink { left: 0, top: 40, bottom: 40 });
        assert_eq!(
            (tight.top, tight.bottom),
            (1, 1),
            "a leading bigger than the gap never pushes the text past the stroke: never below it"
        );
        assert_eq!(Frame::new(0, 8.25, Ink { left: 1, top: 6, bottom: 7 }).stroke, 1, "scale 0 treated as 1");
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
            let pal = Palette::default();
            let (advance, frame) = (p.advance(scale), p.frame(scale));
            let (cols, rows) = b.cells();
            let rh = DEFAULT_HEIGHT * scale as u32;
            let (w, h) = ((cols as f32 * advance).ceil() as u32 + frame.extra().0, rows as u32 * rh + frame.extra().1);
            let mut px = vec![0x11u8; (w * h * 4) as usize];
            p.paint_boxed(b, &pal, &mut px, w, h, rh, scale);
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
            let b = Boxed { rows: vec![text_row(6, " E"), text_row(6, ""), text_row(6, " E")], separators: vec![], divider: None };
            let s = Surface::paint(&mut p, &b, scale);
            let bg = Surface::opaque(Palette::default().bg);
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
            let b = Boxed { rows: vec![text_row(6, ""), text_row(6, ""), text_row(6, "")], separators: vec![], divider: None };
            let s = Surface::paint(&mut p, &b, scale);
            let pal = Palette::default();
            let (muted, bg) = (Surface::opaque(pal.muted), Surface::opaque(pal.bg));
            let (r, st, w, h) = (s.frame.radius, s.frame.stroke, s.w, s.h);
            let mid = (w / 2, h / 2);
            for k in 0..st {
                assert_eq!(s.at(mid.0, k), muted, "scale {scale}: top stroke line {k}");
                assert_eq!(s.at(mid.0, h - 1 - k), muted, "bottom stroke line {k}");
                assert_eq!(s.at(k, mid.1), muted, "left stroke line {k}");
                assert_eq!(s.at(w - 1 - k, mid.1), muted, "right stroke line {k}");
            }
            assert_eq!(s.at(mid.0, st), bg, "scale {scale}: the stroke is {st} px thick, not more");
            assert_eq!(s.at(st, mid.1), bg);
            assert_eq!(s.at(r, 0), muted, "the straight part starts where the corner ends");
            assert_eq!(s.at(0, r), muted);
            assert_eq!(s.at(w - 1 - r, h - 1), muted);
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
            };
            let s = Surface::paint(&mut p, &b, scale);
            let pal = Palette::default();
            let (muted, bg) = (Surface::opaque(pal.muted), Surface::opaque(pal.bg));
            let (st, w, h, rh) = (s.frame.stroke, s.w, s.h, s.rh);
            let y = s.frame.top + rh + (rh - st) / 2;
            let off_divider = s.frame.side + (3.0 * s.advance).round() as u32;
            for k in 0..st {
                assert_eq!(s.at(off_divider, y + k), muted, "scale {scale}: the separator, line {k}");
                assert_eq!(s.at(st, y + k), muted, "it reaches the left stroke");
                assert_eq!(s.at(w - 1 - st, y + k), muted, "and the right one");
            }
            assert_eq!(s.at(off_divider, y - 1), bg, "{st} px thick: nothing above");
            assert_eq!(s.at(off_divider, y + st), bg, "nor below");
            let x = s.frame.side
                + (5.0 * s.advance).round() as u32
                + ((6.0 * s.advance).round() as u32 - (5.0 * s.advance).round() as u32 - st) / 2;
            for yy in [st, s.frame.top, s.frame.top + 2 * rh + rh / 2, h - 1 - st] {
                for k in 0..st {
                    assert_eq!(s.at(x + k, yy), muted, "scale {scale}: the divider at y {yy}, column {k}");
                }
            }
            assert_eq!(s.at(x - 1, s.frame.top + 2 * rh + rh / 2), bg, "{st} px thick: nothing to its left");
            assert_eq!(s.at(x + st, s.frame.top + 2 * rh + rh / 2), bg, "nor its right");
        }
    }

    #[test]
    fn paint_boxed_with_a_short_buffer_paints_nothing_and_never_panics() {
        let mut p = fonts();
        let b = Boxed { rows: vec![Grid::empty(4), Grid::empty(4)], separators: vec![0], divider: Some(1) };
        let mut px = vec![0x11u8; 100];
        p.paint_boxed(&b, &Palette::default(), &mut px, 60, 40, DEFAULT_HEIGHT, 1);
        assert!(px.iter().all(|b| *b == 0x11), "an inconsistent buffer is left alone");
        // Rows past the buffer's end (the compositor shortened the popup): the rows that fit are painted, no panic.
        let frame = p.frame(1);
        let w = (p.advance(1) * 4.0).ceil() as u32 + frame.extra().0;
        let h = frame.top + DEFAULT_HEIGHT; // the first row only
        let mut px = vec![0x11u8; (w * h * 4) as usize];
        p.paint_boxed(&b, &Palette::default(), &mut px, w, h, DEFAULT_HEIGHT, 1);
        assert_ne!(&px[..3], &[0x11, 0x11, 0x11], "painted");
    }

    #[test]
    fn inverted_background_and_glyphs() {
        let mut p = fonts();
        let mut g = Grid::empty(4);
        g.write(0, 4, "A", Style::NORMAL);
        g.write(1, 4, " ", Style::INVERTED);
        g.write(2, 4, "·", Style::NORMAL);
        let pal = Palette::default();
        let av = p.advance(1);
        let (w, h) = ((av * 4.0).ceil() as u32, DEFAULT_HEIGHT);
        let mut px = vec![0u8; (w * h * 4) as usize];
        p.paint(&g, &pal, &mut px, w, h, 1);
        let read = |x: u32, y: u32| {
            let o = ((y * w + x) * 4) as usize;
            (Rgb(px[o + 2], px[o + 1], px[o]), px[o + 3])
        };
        assert_eq!(read((av * 1.5) as u32, h / 2), (pal.accent, 0xff), "inverted cell: accent background");
        let ink_a = (0..av as u32).any(|x| (0..h).any(|y| read(x, y).0 != pal.bg));
        assert!(ink_a, "no pixel drawn for \"A\"");
        let x3 = (av * 3.0).ceil() as u32;
        assert!((x3..w).all(|x| (0..h).all(|y| read(x, y) == (pal.bg, 0xff))), "empty cell: background, opaque");
    }

    /// Mae's bug report: Claude Code's terminal title starts with "✳ "
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
        let pal = Palette::default();
        let av = p.advance(1);
        let (w, h) = ((av * 3.0).ceil() as u32, DEFAULT_HEIGHT);
        let mut px = vec![0u8; (w * h * 4) as usize];
        p.paint(&g, &pal, &mut px, w, h, 1);
        let read = |x: u32, y: u32| {
            let o = ((y * w + x) * 4) as usize;
            Rgb(px[o + 2], px[o + 1], px[o])
        };
        let ink = (0..av as u32).any(|x| (0..h).any(|y| read(x, y) != pal.bg));
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
        let pal = Palette::default();
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
            (x0..x1).all(|x| (0..h).all(|y| read(x, y) == pal.bg)),
            "a masked character draws no box at all, just its cell's background"
        );
        let ink_b = (x1..w).any(|x| (0..h).any(|y| read(x, y) != pal.bg));
        assert!(ink_b, "the character right after the masked one is still drawn, at its own column (consistent width)");
    }

    #[test]
    fn only_useful_faces_are_loaded() {
        // Regular + bold of the family, plus regular + bold of DejaVu Sans
        // Mono (the default fallback): ≈ 2 MB, DejaVu's own share close to
        // nothing (measured in the release memory suite). The whole
        // database (system + Nerd Font directory) cost ≈ 47 MB at the
        // plan's review.
        let p = fonts();
        assert_eq!(p.loaded_faces(), 4, "faces loaded: {}", p.loaded_faces());
    }

    #[test]
    fn glyph_cache_bounded_on_varied_titles() {
        let mut p = fonts();
        let pal = Palette::default();
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
        let pal = Palette::default();
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
        p.paint(&g, &Palette::default(), &mut px, 100, 22, 1);
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
        std::env::var_os("ALPHEUS_POLICES")
            .map(PathBuf::from)
            .expect("ALPHEUS_POLICES required (nix develop) to build this test's fixtures")
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
        g.write(8, 10, "日", Style::INVERTED);
        p.paint(&g, &Palette::default(), &mut px, w, h, 1); // must not panic
        // Every pixel of the buffer stays well-formed (opaque alpha): no
        // write overflowed its computed location.
        for o in (0..px.len()).step_by(4) {
            assert_eq!(px[o + 3], 0xff, "unpainted pixel at byte {o}");
        }
    }
}
