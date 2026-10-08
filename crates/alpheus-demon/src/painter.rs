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

pub(crate) mod blocks;
pub(crate) mod floating;
mod powerline;
#[cfg(test)]
mod shape_tests;

use crate::background::{self, MaskCache};
pub use crate::background::{Background, BarGeometry, Rect, opaque_rects};
use crate::config::FontConfig;
use crate::grid::{CONT, Grid, Group, Hue, Style, Tint};
use crate::list::Boxed;
use crate::placement::FLOAT_SHADOW;
use alpheus_commun::theme::settings::BarStyle;
use alpheus_commun::theme::{Rgb, Roles};
use blocks::OnBlock;
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
        let (radius, pad) = (FRAME_RADIUS * e, advance.round().max(0.0) as u32);
        // Stroke to the first column's ink: the side room, the pad cell, the letter's own bearing.
        // A row with a lot of leading (a tall bar) would need a vertical inset below the corner
        // radius, so the gap grows to keep one, and the side inset grows with it: the visible
        // gaps stay equal on every side.
        let gap = (FRAME_SIDE * e + pad + ink.left).max(ink.top + radius).max(ink.bottom + radius);
        Frame { side: gap - pad - ink.left, top: gap - ink.top, bottom: gap - ink.bottom, stroke: e, radius }
    }

    /// This frame inside a surface of `size` physical px holding `cells` columns × rows: the room
    /// the cells leave over (a compositor-imposed size, the logical rounding of a scale) shared
    /// evenly between the two sides, so the margins stay even.
    pub fn fitted(&self, cells: (u16, u16), advance: f32, row_height: u32, size: (u32, u32)) -> Frame {
        let width = (f32::from(cells.0) * advance).ceil() as u32 + 2 * self.side;
        let height = u32::from(cells.1) * row_height + self.top + self.bottom;
        let (extra_w, extra_h) = (size.0.saturating_sub(width), size.1.saturating_sub(height));
        Frame { side: self.side + extra_w / 2, top: self.top + extra_h / 2, bottom: self.bottom + extra_h - extra_h / 2, ..*self }
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
    /// The bar shapes' corner masks (shapes spec § 3), made on first use per radius and scale.
    masks: MaskCache,
}

/// A tint's colour on a surface whose own background is `base` (`Tint::Bg`). The painter picks the
/// base from its entry point: `paint` (bars) passes `bar_bg`, `paint_boxed` (popups, the launcher)
/// `popup_bg` — no caller can pass the wrong one.
pub fn color(r: &Roles, base: Rgb, t: Tint) -> Rgb {
    match t {
        Tint::Bg => base,
        Tint::Fg => r.fg,
        Tint::FgDim => r.fg_dim,
        Tint::OnAccent => r.on_accent,
        Tint::Accent => r.accent,
        Tint::AccentContainer => r.accent_container,
        Tint::Accent2 => r.accent2,
        Tint::Pill => r.pill_bg,
        Tint::Selection => r.selection,
        Tint::Warn => r.warn,
        Tint::Error => r.error,
        Tint::Urgent => r.urgent,
        Tint::Hue(h) => match h {
            Hue::Red => r.hues.red,
            Hue::Orange => r.hues.orange,
            Hue::Yellow => r.hues.yellow,
            Hue::Green => r.hues.green,
            Hue::Cyan => r.hues.cyan,
            Hue::Blue => r.hues.blue,
            Hue::Purple => r.hues.purple,
        },
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

/// Which cell backgrounds `cell_backgrounds` keeps inside a block (ruling 11).
#[derive(Debug, Clone, Copy)]
enum Clip<'a> {
    /// No block: every cell background over the band's whole height (flat, underline, floating
    /// without pills, the boxed surfaces).
    None,
    /// Every group is a block (islands, translucent, powerline): a cell background stays inside its
    /// block, and a cell outside every block gets none. A `Tint::Pill` cell on a block whose fill is
    /// `pill_bg` is painted `raised` (`on_pill_bg`).
    Blocks(&'a Background<'a>),
    /// Only the hued groups are blocks (floating's pills): a cell in one stays inside it, a cell on
    /// the bar itself is painted as under `None`.
    Hued(&'a [Group]),
    /// Underline: as `None`, but the active workspace gets no fill (`underlined`), and a `Tint::Pill`
    /// cell, on the blocks' `pill_bg`, is painted `raised` (`on_pill_bg`).
    Underline,
}

/// A cell background `t` on a surface of `pill_bg` (a block of the block styles, underline's
/// blocks): `Tint::Pill` there would vanish, so it is painted one step up, `raised` — a workspace
/// visible on another output (`Style::SURFACE`) stays told apart. Any other tint is its own.
fn on_pill_bg(roles: &Roles, t: Tint) -> Rgb {
    if t == Tint::Pill { roles.raised } else { color(roles, roles.pill_bg, t) }
}

/// A cell's style under `underline` (Mae, 2026-10-08): the active workspace has no fill, its text
/// is `accent`; the 2 px accent underline marks it. Every other style is the cell's own.
fn underlined(s: Style) -> Style {
    if s == Style::ACTIVE { Style { fg: Tint::Accent, bg: Tint::Bg, bold: s.bold } } else { s }
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
/// premultiplied ARGB8888) and the part inside it left as the rows painted it.
fn draw_frame(px: &mut [u8], width: u32, height: u32, frame: &Frame, ink: Rgb) {
    let Rgb(sr, sg, sb) = ink;
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
            for (x, y) in [(cx, cy), (width - 1 - cx, cy), (cx, height - 1 - cy), (width - 1 - cx, height - 1 - cy)] {
                let o = (y as usize * width as usize + x as usize) * 4;
                if let Some(dst) = px.get_mut(o..o + 4) {
                    let (b, g, r) = (mix(sb, dst[0], ring, inner), mix(sg, dst[1], ring, inner), mix(sr, dst[2], ring, inner));
                    dst.copy_from_slice(&[b, g, r, (outer * 255.0).round() as u8]);
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
    // Premultiplied "over": an opaque destination stays exactly 255 (flat, popups: unchanged);
    // a transparent or translucent one (islands, translucent) keeps a valid premultiplied pixel.
    px[o + 3] = ((255 * a + px[o + 3] as u32 * (255 - a)) / 255) as u8;
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
            masks: MaskCache::default(),
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

    /// Corner masks made so far (memory guard: none until a rounded style paints).
    pub fn corner_masks(&self) -> usize {
        self.masks.len()
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

    /// A bar in its style (shapes spec § 3):
    /// - the style's background pass;
    /// - the cell backgrounds (under the block styles, clipped to their block: its x span, and
    ///   under islands, translucent and floating's pills its inset rows);
    /// - the edges pass of islands, translucent and floating (rounded corners, borders, floating's
    ///   shadow), and underline's lines, over the cell backgrounds;
    /// - last the glyphs, in the pills inks under the block styles: no glyph pixel is ever erased
    ///   (ruling 11).
    ///
    /// Under floating the bar's band is the surface minus its shadow rows (`placement::FLOAT_SHADOW`):
    /// the text never reaches the shadow. Under `flat` it paints exactly what `paint` paints, byte
    /// for byte.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_bar(&mut self, grid: &Grid, roles: &Roles, bg: &Background, px: &mut [u8], width: u32, height: u32, scale: i32) {
        if px.len() < (width as usize) * (height as usize) * 4 {
            return; // inconsistent buffer: do nothing rather than panic
        }
        self.set_scale(scale);
        let advance = self.advance;
        let edge = |col: u16| background::cell_edge(col, advance);
        let band = match bg.style {
            BarStyle::Floating => height.saturating_sub(FLOAT_SHADOW.saturating_mul(scale.max(1) as u32)),
            BarStyle::Flat | BarStyle::Underline | BarStyle::Powerline | BarStyle::Islands | BarStyle::Translucent => height,
        };
        // islands: transparent around the blocks; translucent: bar_bg at opacity, premultiplied.
        let outside = match bg.style {
            BarStyle::Islands => Some(blocks::CLEAR),
            BarStyle::Translucent => Some(background::premultiplied(roles.bar_bg, blocks::opacity_alpha(bg.opacity))),
            BarStyle::Flat | BarStyle::Underline | BarStyle::Powerline | BarStyle::Floating => None,
        };
        let mut rows = (0, band);
        match (bg.style, outside) {
            (BarStyle::Underline, _) => background::underline_blocks(px, width, band, roles, bg.groups, edge),
            (_, Some(outside)) => {
                blocks::background(px, width, band, scale, advance, roles, bg, outside);
                rows = blocks::island_y(band, scale);
            }
            (BarStyle::Powerline, _) => powerline::background(px, width, band, advance, roles, bg, grid.split()),
            (BarStyle::Floating, _) => {
                floating::background(px, width, band, scale, advance, roles, bg);
                rows = blocks::island_y(band, scale);
            }
            _ => fill(px, width, 0, width, band, roles.bar_bg),
        }
        // The block styles keep a cell background inside its block (ruling 11); flat, underline and
        // floating without pills draw no block and clip nothing.
        let clip = match bg.style {
            BarStyle::Islands | BarStyle::Translucent | BarStyle::Powerline => Clip::Blocks(bg),
            BarStyle::Floating if blocks::pills(bg) => Clip::Hued(bg.groups),
            BarStyle::Underline => Clip::Underline,
            BarStyle::Flat | BarStyle::Floating => Clip::None,
        };
        self.cell_backgrounds(grid, roles, roles.bar_bg, px, width, band, 0, rows, clip);
        if let Some(outside) = outside {
            blocks::round_islands(&mut self.masks, px, width, band, scale, advance, roles, bg, outside);
        }
        if bg.style == BarStyle::Floating {
            // The whole surface: the bar's ends, its pills blocks, and the shadow below the band.
            floating::edges(&mut self.masks, px, width, height, band, scale, advance, roles, bg);
        }
        if bg.style == BarStyle::Underline {
            // Over the cell backgrounds, under the glyphs: a descender is never cut (ruling 11).
            background::underline_lines(px, width, band, roles, bg, background::active_runs(grid.cells()), edge, self.scale);
        }
        // No inks unless pills applies (`blocks::pills`): flat's glyphs are today's.
        let (on, n) = blocks::pill_inks(roles, bg);
        self.glyphs(grid, roles, roles.bar_bg, px, width, band, 0, &on[..n], bg.style == BarStyle::Underline);
    }

    /// A bar: `grid` on `roles.bar_bg` — `flat`'s reference, which `paint_bar` reproduces byte for byte.
    pub fn paint(&mut self, grid: &Grid, roles: &Roles, px: &mut [u8], width: u32, height: u32, scale: i32) {
        if px.len() < (width as usize) * (height as usize) * 4 {
            return; // inconsistent buffer: do nothing rather than panic
        }
        self.set_scale(scale);
        fill(px, width, 0, width, height, roles.bar_bg);
        self.paint_cells(grid, roles, roles.bar_bg, px, width, height, 0, &[]);
    }

    /// `grid`'s cells in a band `height` px tall, the first one `x0` px from the band's left edge:
    /// the cell backgrounds that differ from the surface's (`base`), then the glyphs (`glyphs`, with
    /// the pills inks `on`). The band's own background is the caller's.
    #[allow(clippy::too_many_arguments)]
    fn paint_cells(&mut self, grid: &Grid, roles: &Roles, base: Rgb, px: &mut [u8], width: u32, height: u32, x0: u32, on: &[OnBlock]) {
        self.cell_backgrounds(grid, roles, base, px, width, height, x0, (0, height), Clip::None);
        self.glyphs(grid, roles, base, px, width, height, x0, on, false);
    }

    /// The backgrounds of `grid`'s cells that differ from the surface's (`base`). A cell inside a
    /// block (`clip`) stays inside its x span and the block's `rows` [top, bottom); a cell on the
    /// bar itself takes the band's whole height.
    #[allow(clippy::too_many_arguments)]
    fn cell_backgrounds(
        &self,
        grid: &Grid,
        roles: &Roles,
        base: Rgb,
        px: &mut [u8],
        width: u32,
        height: u32,
        x0: u32,
        rows: (u32, u32),
        clip: Clip,
    ) {
        let advance = self.advance;
        let edge = |col: usize| x0 + ((col as f32) * advance).round() as u32;
        for (col, c) in grid.cells().iter().enumerate() {
            let style = if matches!(clip, Clip::Underline) { underlined(c.style) } else { c.style };
            if style.bg == Tint::Bg {
                continue;
            }
            let holds = |g: &&Group| usize::from(g.start) <= col && col < usize::from(g.end);
            let mut ink = color(roles, base, style.bg);
            let block = match clip {
                Clip::None => None,
                Clip::Underline => {
                    ink = on_pill_bg(roles, style.bg);
                    None
                }
                Clip::Blocks(bg) => match bg.groups.iter().enumerate().find(|(_, g)| holds(g)) {
                    Some((i, g)) => {
                        let otherwise = if bg.style == BarStyle::Powerline { powerline::base(roles, bg.groups, i) } else { roles.pill_bg };
                        if blocks::block_fill(roles, bg, g, otherwise) == roles.pill_bg {
                            ink = on_pill_bg(roles, style.bg);
                        }
                        Some(g)
                    }
                    None => continue,
                },
                Clip::Hued(groups) => groups.iter().filter(|g| g.hue.is_some()).find(holds),
            };
            let (mut x, mut y) = ((edge(col), edge(col + 1)), (0, height));
            if let Some(g) = block {
                let (b0, b1) = blocks::block_x(g, advance);
                // A guard only: a cell's edges and its block's come from the same rounding
                // (`cell_edge`), so the cell already lies inside the block's span.
                x = (x.0.max(x0 + b0), x.1.min(x0 + b1));
                y = rows;
            }
            rect(px, width, height, x, y, ink);
        }
    }

    /// `grid`'s glyphs. A glyph inside one of the `on` blocks (pills) takes the block's ink (its
    /// muted ink for a muted cell) unless its cell is in a state (`blocks::in_state`). Under
    /// `underline`, the active workspace's text is `accent` (`underlined`).
    #[allow(clippy::too_many_arguments)]
    fn glyphs(
        &mut self,
        grid: &Grid,
        roles: &Roles,
        base: Rgb,
        px: &mut [u8],
        width: u32,
        height: u32,
        x0: u32,
        on: &[OnBlock],
        underline: bool,
    ) {
        let advance = self.advance;
        let edge = |col: usize| x0 + ((col as f32) * advance).round() as u32;
        for (col, c) in grid.cells().iter().enumerate() {
            if c.ch == ' ' || c.ch == CONT {
                continue;
            }
            let style = if underline { underlined(c.style) } else { c.style };
            let own = color(roles, base, style.fg);
            let ink = if blocks::in_state(style) {
                own
            } else {
                on.iter()
                    .find(|b| usize::from(b.start) <= col && col < usize::from(b.end))
                    .map_or(own, |b| if c.style.fg == Tint::FgDim { b.muted } else { b.ink })
            };
            self.glyph(c.ch, c.style.bold, edge(col) as f32, ink, px, width, height);
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
    /// its rounded corners stays transparent). Rows that do not fit are not painted. The surface is
    /// `roles.popup_bg`, the frame and the lines `roles.border_over(popup_bg)`.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_boxed(&mut self, b: &Boxed, roles: &Roles, px: &mut [u8], width: u32, height: u32, row_height: u32, scale: i32) {
        if px.len() < (width as usize) * (height as usize) * 4 {
            return; // inconsistent buffer: do nothing rather than panic
        }
        let frame = self.frame(scale).fitted(b.cells(), self.advance, row_height, (width, height));
        let base = roles.popup_bg;
        fill(px, width, 0, width, height, base);
        let band = width as usize * row_height as usize * 4;
        for (i, row) in b.rows.iter().enumerate() {
            let start = (frame.top as usize + i * row_height as usize) * width as usize * 4;
            let Some(slice) = px.get_mut(start..start + band) else { break };
            self.paint_cells(row, roles, base, slice, width, row_height, frame.side, &[]);
        }
        // A selection band reaching the frame's top or bottom edge is as far from it as from the
        // sides: the first and last rows' coloured cells bleed into the vertical inset.
        let rows_end = frame.top + b.rows.len() as u32 * row_height;
        let up = (frame.side.min(frame.top), frame.top);
        let down = (rows_end, rows_end + frame.bottom.saturating_sub(frame.side));
        for (grid, (y0, y1)) in [(b.rows.first(), up), (b.rows.last(), down)] {
            for (col, c) in grid.map_or(&[][..], |g| g.cells()).iter().enumerate() {
                if c.style.bg != Tint::Bg {
                    let x = (frame.side + self.edge(col), frame.side + self.edge(col + 1));
                    rect(px, width, height, x, (y0, y1), color(roles, base, c.style.bg));
                }
            }
        }
        let ink = roles.border_over(base); // spec § 1: fg_dim at 40 % over the surface
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
        draw_frame(px, w, h, &frame, ink);
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
    use alpheus_commun::theme::Hues;
    use alpheus_commun::theme::settings::ModuleColors;

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
        let b = Boxed { rows: vec![text_row(6, ""), text_row(6, "")], separators: vec![], divider: None };
        let frame = p.frame(1);
        let (bw, bh) = ((6.0 * av).ceil() as u32 + frame.extra().0, 2 * DEFAULT_HEIGHT + frame.extra().1);
        let mut bpx = vec![0u8; (bw * bh * 4) as usize];
        p.paint_boxed(&b, &r, &mut bpx, bw, bh, DEFAULT_HEIGHT, 1);
        let middle = ((bh / 2 * bw + bw / 2) * 4) as usize;
        assert_eq!(&bpx[middle..middle + 4], &[r.popup_bg.2, r.popup_bg.1, r.popup_bg.0, 0xff], "a popup, the launcher: popup_bg");
        let stroke = ((bh / 2 * bw) * 4) as usize; // the left stroke, mid-height
        let ink = r.border_over(r.popup_bg);
        assert_eq!(&bpx[stroke..stroke + 4], &[ink.2, ink.1, ink.0, 0xff], "the frame: fg_dim at 40 % over popup_bg");
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
            glyph_reshaped_every_time(
                p,
                c.ch,
                c.style.bold,
                edge(col) as f32,
                color(palette, palette.bar_bg, c.style.fg),
                px,
                width,
                height,
            );
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
    /// `nix develop -c cargo test --release -p alpheus-demon --lib painter::tests::glyph_shaping_memo_speeds_up_paint -- --ignored --nocapture`
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
            let b = Boxed { rows: vec![inv.clone(), text_row(6, ""), inv], separators: vec![], divider: None };
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
            let b = Boxed { rows: vec![inv.clone(), text_row(6, ""), inv], separators: vec![], divider: None };
            let (advance, frame) = (p.advance(scale), p.frame(scale));
            let rh = DEFAULT_HEIGHT * scale as u32;
            let (w, h) =
                ((6.0 * advance).ceil() as u32 + frame.extra().0 + 14 * scale as u32, 3 * rh + frame.extra().1 + 31 * scale as u32);
            let mut px = vec![0x11u8; (w * h * 4) as usize];
            p.paint_boxed(&b, &Roles::default(), &mut px, w, h, rh, scale);
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
            let b = Boxed { rows: vec![text_row(6, ""), text_row(6, ""), text_row(6, "")], separators: vec![], divider: None };
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
        let b = Boxed { rows: vec![Grid::empty(4), Grid::empty(4)], separators: vec![0], divider: Some(1) };
        let mut px = vec![0x11u8; 100];
        p.paint_boxed(&b, &Roles::default(), &mut px, 60, 40, DEFAULT_HEIGHT, 1);
        assert!(px.iter().all(|b| *b == 0x11), "an inconsistent buffer is left alone");
        // Rows past the buffer's end (the compositor shortened the popup): the rows that fit are painted, no panic.
        let frame = p.frame(1);
        let w = (p.advance(1) * 4.0).ceil() as u32 + frame.extra().0;
        let h = frame.top + DEFAULT_HEIGHT; // the first row only
        let mut px = vec![0x11u8; (w * h * 4) as usize];
        p.paint_boxed(&b, &Roles::default(), &mut px, w, h, DEFAULT_HEIGHT, 1);
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

    /// Mae, 2026-10-08: under underline the active workspace has no accent fill; its text is
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
}
