//! Drawing of the grid into an ARGB8888 buffer (wl_shm: bytes B, G, R, A),
//! cell by cell: each cell's background, then a glyph shaped by cosmic-text
//! and placed at the left edge of its cell. The grid stays aligned even
//! when a fallback font (emoji, CJK) has a different advance.
//!
//! Bounded memory: the font database only holds the faces actually useful
//! (cosmic-text otherwise parses every monospace face in its database up
//! front), and the glyph cache is cleared past `MAX_CACHE` entries.

use crate::config::FontConfig;
use crate::grid::{CONT, Grid, Tint};
use alpheus_commun::theme::{Palette, Rgb};
use cosmic_text::{Attrs, Buffer, CacheKey, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, fontdb};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Rasterized glyphs kept at most (≈ 1 MB at 14 px).
pub const MAX_CACHE: usize = 2048;

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
fn shape(fs: &mut FontSystem, buffer: &mut Buffer, family: &str, ch: char, bold: bool) -> Vec<ShapedGlyph> {
    let mut s = [0u8; 4];
    buffer.set_text(fs, ch.encode_utf8(&mut s), attrs(family, bold), Shaping::Advanced);
    buffer.shape_until_scroll(fs, false);
    let mut shaped = Vec::new();
    for run in buffer.layout_runs() {
        for gl in run.glyphs.iter() {
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
        let advance = self.advance;
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
            self.glyph(c.ch, c.style.bold, edge(col) as f32, color(palette, c.style.fg), px, width, height);
        }
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
    /// an independent ground truth to compare the memoized path against.
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

    #[test]
    fn only_useful_faces_are_loaded() {
        // Regular + bold: ≈ 2 MB. The whole database (system + Nerd Font
        // directory) cost ≈ 47 MB at the plan's review.
        let p = fonts();
        assert_eq!(p.loaded_faces(), 2, "faces loaded: {}", p.loaded_faces());
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
        assert!(p.loaded_faces() <= 2, "fallback: one family, not the whole database ({})", p.loaded_faces());
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
        let (p, w) = Fonts::from_dir(Some(&directory), &FontConfig::default(), DEFAULT_HEIGHT)
            .expect("the built collection contains a monospace font");
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
