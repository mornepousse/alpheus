//! The grid drawn into a shared-memory buffer, cell by cell.
//!
//! Each cell's background first, then a glyph shaped by cosmic-text and placed at the left edge of
//! its cell, so the grid stays aligned even when a fallback face (emoji, CJK) has a different
//! advance. The buffer is ARGB8888 as `wl_shm` takes it (bytes B, G, R, A), premultiplied. The
//! per-style background and line passes are in `background.rs`, which this file runs around the
//! text pass; `plot` draws the resources popup's graphs, whose columns are whole pixels so they
//! tile exactly at any scale.
//!
//! Bounded memory: the font database holds only the faces that turn out to be useful (cosmic-text
//! would otherwise parse every monospace face it can find at start), and the glyph cache is
//! cleared past `MAX_CACHE` entries. A character with no glyph in the family or any fallback is
//! not drawn at all, rather than drawn as the resolved face's `.notdef` box; its cell keeps the
//! width `Grid::write` gave it.
//!
//! Never:
//! - paint with anything but the theme's sixteen roles
//!   (`the_daemon_paints_with_roles_never_the_legacy_palette`);
//! - grow without bound: the cache and the database are the two that could, and both are capped;
//! - block or allocate per frame beyond the pool the surface owns (the memory suite's PSS tests
//!   per popup and per style measure it).
//!
//! See: docs/ARCHITECTURE.md ("The core"), docs/features/themes.md (the styles and their passes),
//! docs/features/resources.md (the graphs), docs/measurements.md (the per-style figures).

pub(crate) mod blocks;
pub(crate) mod floating;
mod powerline;
#[cfg(test)]
mod shape_tests;

use crate::background::{self, MaskCache};
pub use crate::background::{Background, BarGeometry, Rect, opaque_rects};
use crate::config::FontConfig;
use crate::grid::{CONT, Grid, Group, Hue, Style, Tint};
use crate::list::{Boxed, Graph};
use crate::placement::FLOAT_SHADOW;
use alpheus_common::theme::settings::BarStyle;
use alpheus_common::theme::{Rgb, Roles};
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

/// The colour of a boxed surface's frame: the border (popups, the launcher, a bubble), or `urgent`
/// for a critical notification's bubble (notifications spec § 3). Separators and the divider always
/// keep the border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameInk {
    Border,
    Urgent,
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
        for gl in run.glyphs {
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
    ((width_phys as f32 / advance).floor() as u32).min(u32::from(u16::MAX)) as u16
}

/// Cell under a point (x in logical px, like the Wayland pointer).
pub fn column_under(x_logical: f64, scale: i32, advance: f32) -> u16 {
    if advance <= 0.0 || x_logical <= 0.0 {
        return 0;
    }
    ((x_logical * f64::from(scale.max(1))) / f64::from(advance)).floor().min(f64::from(u16::MAX)) as u16
}

fn attrs(family: &str, bold: bool) -> Attrs<'_> {
    let a = Attrs::new().family(Family::Name(family));
    if bold { a.weight(Weight::BOLD) } else { a }
}

/// Which cell backgrounds `cell_backgrounds` keeps inside a block.
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

/// A cell's style under `underline` (2026-10-08): the active workspace has no fill, its text
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

/// Share of a graph's ink (percent) in the body under its line.
pub(crate) const BODY_ALPHA: u16 = 40;

/// `c` at `alpha` % over `base`, per channel, rounded.
pub(crate) fn mix(c: Rgb, base: Rgb, alpha: u16) -> Rgb {
    let a = u32::from(alpha.min(100));
    let m = |c: u8, b: u8| ((u32::from(c) * a + u32::from(b) * (100 - a) + 50) / 100) as u8;
    Rgb(m(c.0, base.0), m(c.1, base.1), m(c.2, base.2))
}

/// Draws `g`'s series in the physical-pixel rectangle `area` = (x0, y0, x1, y1) of `px`
/// (`width` x `height`), clipped to the buffer. The baseline takes the bottom `stroke` rows of the
/// area and the bodies stand on it. Point `i` of `n` covers `[W*i/n, W*(i+1)/n)` columns, so the
/// blocks tile the width exactly; only exact colours are written (crisp at any scale).
#[allow(clippy::too_many_arguments)]
pub(crate) fn plot(
    px: &mut [u8],
    width: u32,
    height: u32,
    area: (u32, u32, u32, u32),
    g: &Graph,
    roles: &Roles,
    base: Rgb,
    stroke: u32,
    scale: i32,
) {
    let (x0, y0, x1, y1) = area;
    if x1 <= x0 || y1 <= y0 || px.len() < width as usize * height as usize * 4 {
        return;
    }
    let bottom = y1.saturating_sub(stroke).max(y0);
    rect(px, width, height, (x0, x1), (bottom, y1), roles.border_over(base));
    let (w, full, tick) = (u64::from(x1 - x0), u64::from(bottom - y0), scale.max(1) as u32);
    // The filled series first, then the lines, over them.
    for fill in [true, false] {
        for s in g.series.iter().filter(|s| s.fill == fill) {
            let n = s.points.len() as u64;
            let ink = color(roles, base, s.ink);
            for (i, v) in s.points.iter().enumerate() {
                let Some(v) = v.filter(|v| *v > 0) else { continue };
                let (i, v) = (i as u64, u64::from(v.min(1000)));
                let (a, b) = (x0 + (w * i / n) as u32, x0 + (w * (i + 1) / n) as u32);
                let h = ((v * full / 1000) as u32).max(tick).min(bottom - y0);
                let top = bottom - h;
                if fill {
                    rect(px, width, height, (a, b), (top, bottom), mix(ink, base, BODY_ALPHA));
                }
                rect(px, width, height, (a, b), (top, (top + tick).min(bottom)), ink);
            }
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
    let mix = |stroke: u8, back: u8, ring: f32, inside: f32| {
        ((f32::from(stroke) * ring + f32::from(back) * inside).round()).clamp(0.0, 255.0) as u8
    };
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
    let a = u32::from(c.a());
    if a == 0 {
        return;
    }
    // x and y are positive and in bounds here: usize right away.
    let o = (y as usize * width as usize + x as usize) * 4;
    let src = [c.b(), c.g(), c.r()];
    for k in 0..3 {
        px[o + k] = ((u32::from(src[k]) * a + u32::from(px[o + k]) * (255 - a)) / 255) as u8;
    }
    // Premultiplied "over": an opaque destination stays exactly 255 (flat, popups: unchanged);
    // a transparent or translucent one (islands, translucent) keeps a valid premultiplied pixel.
    px[o + 3] = ((255 * a + u32::from(px[o + 3]) * (255 - a)) / 255) as u8;
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
    let closest = |target: i32| faces.iter().min_by_key(|f| (i32::from(f.weight.0) - target).abs()).map(|f| f.id);
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
    /// - last the glyphs, in the pills inks under the block styles: no glyph pixel is ever erased.
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
        // The block styles keep a cell background inside its block; flat, underline and
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
            // Over the cell backgrounds, under the glyphs: a descender is never cut.
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
    /// `roles.popup_bg`, the lines `roles.border_over(popup_bg)`, the frame the same or `roles.urgent`
    /// (`frame_ink`).
    #[allow(clippy::too_many_arguments)]
    pub fn paint_boxed(
        &mut self,
        b: &Boxed,
        roles: &Roles,
        px: &mut [u8],
        width: u32,
        height: u32,
        row_height: u32,
        scale: i32,
        frame_ink: FrameInk,
    ) {
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
        let inset = 2 * scale.max(1) as u32;
        for g in &b.graphs {
            let x = (frame.side + self.edge(usize::from(g.col)), frame.side + self.edge(usize::from(g.col) + usize::from(g.width)));
            let y = (
                (frame.top + g.row as u32 * row_height + inset),
                (frame.top + (g.row as u32 + u32::from(g.rows)) * row_height).saturating_sub(inset),
            );
            plot(px, w, h, (x.0, y.0, x.1, y.1), g, roles, base, s, scale);
        }
        let edge = match frame_ink {
            FrameInk::Border => ink,
            FrameInk::Urgent => roles.urgent,
        };
        draw_frame(px, w, h, &frame, edge);
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
mod tests;
