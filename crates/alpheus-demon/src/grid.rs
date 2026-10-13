//! Grid of cells: what the bar shows, cell by cell (character, tints,
//! bold), and the clickable zones. This is the tested object; the pixel
//! drawing (`painter`) only copies it.

use crate::config::ModuleId;
use crate::tray::ItemKey;
use std::str::Chars;
use unicode_normalization::{IsNormalized, Recompositions, UnicodeNormalization, is_nfc_quick};
use unicode_width::UnicodeWidthChar;

/// A colour role (themes spec § 1), resolved per surface when painting (`painter::color`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tint {
    /// The surface's own background: `bar_bg` on a bar, `popup_bg` on a popup or the launcher.
    Bg,
    Fg,
    FgDim,
    OnAccent,
    Accent,
    AccentContainer,
    Accent2,
    /// `pill_bg`: a raised cell of the bar (a visible, unfocused workspace).
    Pill,
    Selection,
    Warn,
    Error,
    Urgent,
    /// One of the theme's seven hues (themes spec § 9), for a module's icon or text.
    Hue(Hue),
}

/// The seven named hues of a theme (`Roles::hues`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hue {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Tint,
    pub bg: Tint,
    pub bold: bool,
}

impl Style {
    pub const NORMAL: Style = Style { fg: Tint::Fg, bg: Tint::Bg, bold: false };
    /// Secondary text, inactive items.
    pub const MUTED: Style = Style { fg: Tint::FgDim, bg: Tint::Bg, bold: false };
    /// The active item (spec § 4): the focused workspace, today, the launcher's mode and cursor.
    pub const ACTIVE: Style = Style { fg: Tint::OnAccent, bg: Tint::AccentContainer, bold: false };
    /// The selected row of a menu or of the launcher (spec § 4).
    pub const SELECTED: Style = Style { fg: Tint::Fg, bg: Tint::Selection, bold: false };
    pub const SURFACE: Style = Style { fg: Tint::Fg, bg: Tint::Pill, bold: false };
    pub const ERROR: Style = Style { fg: Tint::Error, bg: Tint::Bg, bold: false };
    /// Low battery, a degraded state.
    pub const WARN: Style = Style { fg: Tint::Warn, bg: Tint::Bg, bold: false };
    /// An urgent workspace.
    pub const URGENT: Style = Style { fg: Tint::Urgent, bg: Tint::Bg, bold: false };
    pub const BOLD_ACCENT: Style = Style { fg: Tint::Accent, bg: Tint::Bg, bold: true };
    /// Accent without bold: gauges, graphs, a live link.
    pub const ACCENT: Style = Style { fg: Tint::Accent, bg: Tint::Bg, bold: false };
    /// The second accent: checks and radios in menus.
    pub const ACCENT2: Style = Style { fg: Tint::Accent2, bg: Tint::Bg, bold: false };
    /// Bold text in the foreground colour: a notification's summary (notifications spec § 3).
    pub const BOLD: Style = Style { fg: Tint::Fg, bg: Tint::Bg, bold: true };

    /// A module's icon or text in one of the theme's hues, on the surface's background.
    pub const fn hue(h: Hue) -> Style {
        Style { fg: Tint::Hue(h), bg: Tint::Bg, bold: false }
    }

    /// Letter code for snapshots.
    pub fn code(self) -> char {
        match self {
            Style::NORMAL => '.',
            Style::MUTED => 'm',
            Style::ACTIVE => 'A',
            Style::SELECTED => 'S',
            Style::SURFACE => 's',
            Style::ERROR => 'e',
            Style::WARN => 'w',
            Style::URGENT => 'u',
            Style::BOLD_ACCENT => 'G',
            Style::ACCENT => 'a',
            Style::ACCENT2 => 'b',
            Style::BOLD => 'N',
            Style { fg: Tint::Hue(h), bg: Tint::Bg, bold: false } => match h {
                Hue::Red => 'r',
                Hue::Orange => 'o',
                Hue::Yellow => 'y',
                Hue::Green => 'g',
                Hue::Cyan => 'c',
                Hue::Blue => 'B',
                Hue::Purple => 'p',
            },
            _ => '?',
        }
    }
}

/// Second cell of a wide character: nothing to draw.
pub const CONT: char = '\0';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

/// What a click targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Workspace {
        reference: String,
        output: String,
    },
    /// A whole system module (volume, network…): `actions` decides by button.
    Module(ModuleId),
    /// One tray icon: its gestures go to the application (task 10), never a command.
    TrayItem(ItemKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    pub start: u16,
    /// Excluded.
    pub end: u16,
    pub target: Target,
}

/// A block of the bar (shapes spec § 2): the modules one style draws as one shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Workspaces,
    Title,
    /// Sound, network, Bluetooth, battery: those present.
    Status,
    /// CPU, RAM, temperature.
    Resources,
    Tray,
    Clock,
    Launcher,
    /// The media module: a block of its own (media spec § 1).
    Media,
    /// The power button: a block of its own (power spec § 1).
    Power,
}

/// One block: cells [start, end) of the bar's grid, its pads included under the block styles;
/// `hue`, its first module's hue (`pills`), None when that module has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    pub kind: GroupKind,
    pub start: u16,
    /// Excluded.
    pub end: u16,
    pub hue: Option<Hue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    cells: Vec<Cell>,
    zones: Vec<Zone>,
    /// The bar's blocks, left to right; empty under `flat` (nothing paints them) and on every
    /// other surface.
    groups: Vec<Group>,
    /// First column of the bar's right-hand side: where the free space ends (the grid's width
    /// when there is none).
    split: u16,
}

/// C0/C1 controls, and Unicode line/paragraph separators (U+2028, U+2029):
/// a width of 1 according to `unicode-width`, but nothing to show as-is in a
/// cell. Treated the same everywhere (`char_width` and `Grid::write`) so
/// they never disagree on their width.
fn is_displayed_control(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// Displayed width: a control character (or treated as one, see
/// `is_displayed_control`) becomes a space (1), a combining mark takes no
/// cell (0), CJK and emoji take 2.
pub(crate) fn char_width(c: char) -> u16 {
    if is_displayed_control(c) { 1 } else { c.width().unwrap_or(0) as u16 }
}

/// Iterates the characters of `s` recomposed to NFC (decomposed accents,
/// coming from macOS for example, recomposed to precomposed form):
/// `text_width`, `truncate` and `Grid::write` all three see this same text,
/// so the announced width stays what is actually written. A combining mark
/// with no precomposed form (e.g. `q\u{301}`) cannot be recomposed: it stays
/// alone in the stream, and so is always skipped further on (width 0),
/// unchanged behaviour.
enum NfcChars<'a> {
    /// `s` is already NFC (the case of nearly all titles): no allocation, we
    /// read its characters directly.
    Direct(Chars<'a>),
    /// `s` is decomposed: we go through the recomposition iterator (still
    /// without an intermediate `String`).
    Recompose(Recompositions<Chars<'a>>),
}

impl<'a> Iterator for NfcChars<'a> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        match self {
            NfcChars::Direct(it) => it.next(),
            NfcChars::Recompose(it) => it.next(),
        }
    }
}

fn nfc_chars(s: &str) -> NfcChars<'_> {
    if is_nfc_quick(s.chars()) == IsNormalized::Yes { NfcChars::Direct(s.chars()) } else { NfcChars::Recompose(s.nfc()) }
}

pub fn text_width(s: &str) -> u16 {
    nfc_chars(s).map(char_width).fold(0u16, u16::saturating_add)
}

/// `s` cut to `max` cells, ending with "…" if it was cut.
pub fn truncate(s: &str, max: u16) -> String {
    if text_width(s) <= max {
        // Already an allocation anyway (we return a `String`): we recompose
        // here too, to stay consistent with `write`.
        return nfc_chars(s).collect();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut l = 0u16;
    for c in nfc_chars(s) {
        let w = char_width(c);
        if l.saturating_add(w) > max - 1 {
            break;
        }
        out.push(c);
        l += w;
    }
    out.push('…');
    out
}

impl Grid {
    pub fn empty(columns: u16) -> Grid {
        Grid {
            cells: vec![Cell { ch: ' ', style: Style::NORMAL }; columns as usize],
            zones: Vec::new(),
            groups: Vec::new(),
            split: columns,
        }
    }

    pub fn width(&self) -> u16 {
        self.cells.len() as u16
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The grid without its first and last cell (nothing at all when narrower than 3 cells);
    /// zones are not kept.
    pub fn without_edges(&self) -> Grid {
        let inner = self.cells.get(1..self.cells.len().saturating_sub(1)).unwrap_or(&[]);
        Grid { cells: inner.to_vec(), zones: Vec::new(), groups: Vec::new(), split: inner.len() as u16 }
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// Writes `text` at `col`, never past `end` (excluded) nor the grid;
    /// returns the next column. A wide character that no longer fits is not
    /// cut in half.
    pub fn write(&mut self, col: u16, end: u16, text: &str, style: Style) -> u16 {
        let end = end.min(self.width());
        let mut x = col;
        for c in nfc_chars(text) {
            let c = if is_displayed_control(c) { ' ' } else { c };
            let w = char_width(c);
            if w == 0 {
                continue;
            }
            if x.saturating_add(w) > end {
                break;
            }
            self.clear_wide_edge(x, w);
            self.cells[x as usize] = Cell { ch: c, style };
            if w == 2 {
                self.cells[x as usize + 1] = Cell { ch: CONT, style };
            }
            x += w;
        }
        x
    }

    /// Before writing a cell of width `w` at `x`, clears half of an existing
    /// wide character that would become orphaned: if `x` is the continuation
    /// (`CONT`) of a wide character, its start at `x - 1`; if the cell right
    /// after the write (`x + w`) is a continuation, its start was just
    /// overwritten: that continuation.
    fn clear_wide_edge(&mut self, x: u16, w: u16) {
        let i = x as usize;
        let empty = Cell { ch: ' ', style: Style::NORMAL };
        if i > 0 && self.cells[i].ch == CONT {
            self.cells[i - 1] = empty;
        }
        let after = i + w as usize;
        if after < self.cells.len() && self.cells[after].ch == CONT {
            self.cells[after] = empty;
        }
    }

    pub fn zone(&mut self, start: u16, end: u16, target: Target) {
        if start < end {
            self.zones.push(Zone { start, end, target });
        }
    }

    /// Records a block, cut to the grid; an empty one (`start >= end` once cut) is not kept.
    pub fn group(&mut self, g: Group) {
        let end = g.end.min(self.width());
        if g.start < end {
            self.groups.push(Group { end, ..g });
        }
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn split(&self) -> u16 {
        self.split
    }

    pub fn set_split(&mut self, col: u16) {
        self.split = col.min(self.width());
    }

    pub fn target_at(&self, col: u16) -> Option<&Target> {
        self.zone_at(col).map(|z| &z.target)
    }

    /// The zone under `col` (its target and cells: the menu popup's anchor).
    pub fn zone_at(&self, col: u16) -> Option<&Zone> {
        self.zones.iter().find(|z| (z.start..z.end).contains(&col))
    }

    /// Text of the grid (snapshots), continuations of wide characters omitted.
    pub fn text(&self) -> String {
        self.cells.iter().filter(|c| c.ch != CONT).map(|c| c.ch).collect()
    }

    /// One style code per cell (see `Style::code`).
    pub fn styles(&self) -> String {
        self.cells.iter().map(|c| c.style.code()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_at_finds_the_icon_cells() {
        let key = |n: u32| crate::tray::ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
        let mut g = Grid::empty(10);
        g.zone(1, 2, Target::TrayItem(key(1)));
        g.zone(3, 5, Target::TrayItem(key(2))); // a two-cell glyph
        assert_eq!(g.zone_at(0), None);
        assert_eq!(g.zone_at(1), Some(&Zone { start: 1, end: 2, target: Target::TrayItem(key(1)) }));
        assert_eq!(g.zone_at(2), None, "the blank cell between icons");
        let wide = Zone { start: 3, end: 5, target: Target::TrayItem(key(2)) };
        assert_eq!((g.zone_at(3), g.zone_at(4)), (Some(&wide), Some(&wide)), "both cells give the whole zone");
        assert_eq!(g.zone_at(5), None);
        assert_eq!(g.target_at(4), Some(&Target::TrayItem(key(2))), "target_at agrees");
    }

    /// A block is kept cut to the grid; an empty one is dropped. A fresh grid has none, and its
    /// right-hand side starts past its last cell.
    #[test]
    fn groups_are_cut_to_the_grid_and_empty_ones_dropped() {
        let mut g = Grid::empty(10);
        assert_eq!((g.groups(), g.split()), (&[][..], 10));
        let clock = |start, end| Group { kind: GroupKind::Clock, start, end, hue: None };
        g.group(clock(2, 5));
        g.group(clock(4, 4));
        g.group(clock(8, 30));
        g.group(clock(12, 30));
        assert_eq!(g.groups(), &[clock(2, 5), clock(8, 10)]);
        g.set_split(40);
        assert_eq!(g.split(), 10, "cut to the grid too");
        assert_eq!(g.without_edges().groups(), &[], "a boxed row carries no block");
    }

    #[test]
    fn bold_text_has_its_own_code() {
        let mut g = Grid::empty(3);
        g.write(0, 3, "ab", Style::BOLD);
        assert_eq!(g.styles(), "NN.");
        assert!(Style::BOLD.bold && Style::BOLD.fg == Tint::Fg && Style::BOLD.bg == Tint::Bg);
    }

    /// Every style has its own snapshot letter: a snapshot never confuses two roles.
    #[test]
    fn every_style_has_its_own_code() {
        let all = [
            (Style::NORMAL, '.'),
            (Style::MUTED, 'm'),
            (Style::ACTIVE, 'A'),
            (Style::SELECTED, 'S'),
            (Style::SURFACE, 's'),
            (Style::ERROR, 'e'),
            (Style::WARN, 'w'),
            (Style::URGENT, 'u'),
            (Style::BOLD_ACCENT, 'G'),
            (Style::ACCENT, 'a'),
            (Style::ACCENT2, 'b'),
            (Style::BOLD, 'N'),
            (Style::hue(Hue::Red), 'r'),
            (Style::hue(Hue::Orange), 'o'),
            (Style::hue(Hue::Yellow), 'y'),
            (Style::hue(Hue::Green), 'g'),
            (Style::hue(Hue::Cyan), 'c'),
            (Style::hue(Hue::Blue), 'B'),
            (Style::hue(Hue::Purple), 'p'),
        ];
        for (s, c) in all {
            assert_eq!(s.code(), c, "{s:?}");
        }
        let mut codes: Vec<char> = all.iter().map(|(s, _)| s.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all.len(), "two styles share a letter");
        assert_eq!(Style::ACTIVE, Style { fg: Tint::OnAccent, bg: Tint::AccentContainer, bold: false }, "spec § 4: the active workspace");
        assert_eq!(Style::SELECTED, Style { fg: Tint::Fg, bg: Tint::Selection, bold: false }, "spec § 4: the selection row");
    }

    #[test]
    fn write_and_styles() {
        let mut g = Grid::empty(8);
        let x = g.write(1, 8, "ab", Style::ACTIVE);
        assert_eq!(x, 3);
        g.write(x, 8, "cd", Style::MUTED);
        assert_eq!(g.text(), " abcd   ");
        assert_eq!(g.styles(), ".AAmm...");
    }

    #[test]
    fn never_outside_the_grid() {
        let mut g = Grid::empty(5);
        assert_eq!(g.write(3, 99, "abcdef", Style::NORMAL), 5);
        assert_eq!(g.text(), "   ab");
        assert_eq!(g.write(9, 99, "x", Style::NORMAL), 9, "start outside the grid: nothing written");
        let mut empty = Grid::empty(0);
        empty.write(0, 10, "abc", Style::NORMAL);
        assert_eq!(empty.text(), "");
    }

    #[test]
    fn wide_characters_and_controls() {
        let mut g = Grid::empty(6);
        assert_eq!(g.write(0, 6, "a日本語", Style::NORMAL), 5, "the third wide character does not fit");
        assert_eq!(g.text(), "a日本 ");
        assert_eq!(g.cells()[2].ch, CONT);
        let mut g = Grid::empty(8);
        g.write(0, 8, "a\tb\nc\u{1b}[e\u{301}", Style::NORMAL);
        // Since task 10b, the decomposed "e" + combining accent is
        // recomposed to NFC ("é") before writing, like the rest of the text.
        assert_eq!(g.text(), "a b c [é", "controls as spaces, decomposed accent recomposed");
        assert_eq!(text_width("🦀日a"), 5);
    }

    #[test]
    fn inner_end_bound_respected() {
        // Bites on `let end = end.min(self.width());`: replacing it with
        // `let end = self.width();` (ignoring the parameter) leaves these
        // two assertions red (see fix report).
        assert_eq!(Grid::empty(8).write(0, 3, "abcdef", Style::NORMAL), 3);
        let mut g = Grid::empty(8);
        g.write(0, 3, "abcdef", Style::NORMAL);
        assert_eq!(g.text(), "abc     ");

        // A wide character that doesn't fit in an inner bound (not only the
        // whole grid).
        assert_eq!(Grid::empty(4).write(0, 2, "a日", Style::NORMAL), 1);
    }

    #[test]
    fn wide_never_orphaned() {
        // Overwriting the first half of a wide character with a narrow one
        // must blank its continuation, or it stays shown alone.
        let mut g = Grid::empty(4);
        g.write(1, 4, "日", Style::NORMAL);
        g.write(1, 4, "a", Style::NORMAL);
        assert_eq!(g.cells()[2].ch, ' ', "the continuation of the cleared 日 must not remain");
        assert_eq!(g.text(), " a  ");

        // Writing directly on the `CONT` cell must blank the wide character
        // to its left, or it visually overflows onto the new cell.
        let mut g = Grid::empty(4);
        g.write(1, 4, "日", Style::NORMAL);
        g.write(2, 4, "b", Style::NORMAL);
        assert_eq!(g.cells()[1].ch, ' ', "the 日 whose continuation is overwritten must not remain");
        assert_eq!(g.text(), "  b ");

        // A wide character written over the start of another wide character
        // covers that start with its own continuation: the continuation of
        // the old one, right after, must be blanked.
        let mut g = Grid::empty(4);
        g.write(1, 4, "日", Style::NORMAL);
        g.write(0, 4, "本", Style::NORMAL);
        assert_eq!(g.cells()[2].ch, ' ', "the continuation of the covered 日 must not remain");
        assert_eq!(g.text(), "本  ");
    }

    #[test]
    fn unicode_separators_as_controls() {
        // U+2028 (line separator) and U+2029 (paragraph separator) have a
        // width of 1 according to unicode-width without being `is_control`:
        // treated like controls, as a space, not shown as-is.
        assert_eq!(text_width("a\u{2028}b\u{2029}"), 4);
        let mut g = Grid::empty(4);
        g.write(0, 4, "a\u{2028}b\u{2029}", Style::NORMAL);
        assert_eq!(g.text(), "a b ");
    }

    #[test]
    fn truncation_by_ellipsis() {
        assert_eq!(truncate("Bonjour", 5), "Bonj…");
        assert_eq!(truncate("Bonjour", 7), "Bonjour");
        assert_eq!(truncate("日本語", 4), "日…");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abc", 1), "…");
        assert!(text_width(&truncate("🦀🦀🦀🦀", 5)) <= 5);
    }

    #[test]
    fn decomposed_accents_recomposed() {
        // "e\u{301}lan": a decomposed "e" (e + combining acute accent) must
        // be written recomposed to NFC, like a title from macOS.
        let mut g = Grid::empty(6);
        let x = g.write(0, 6, "e\u{301}lan", Style::NORMAL);
        assert_eq!(g.text(), "élan  ");
        assert_eq!(x, 4);
        assert_eq!(text_width("e\u{301}lan"), 4);
        assert_eq!(truncate("Ame\u{301}lie", 4), "Amé…");
        // Bites on the fast return path of `truncate` (text that already
        // fits): leaving it without going through `nfc_chars` would leave
        // "e\u{301}lan" as-is (decomposed), even though its width is
        // unchanged.
        assert_eq!(truncate("e\u{301}lan", 10), "élan");
    }

    #[test]
    fn combining_mark_without_precomposed_form_always_skipped() {
        // "q" has no precomposed form with the acute accent: the combining
        // mark stays skipped, unchanged behaviour.
        let mut g = Grid::empty(4);
        g.write(0, 4, "q\u{301}x", Style::NORMAL);
        assert_eq!(g.text(), "qx  ");
        assert_eq!(text_width("q\u{301}x"), 2);
    }

    #[test]
    fn already_nfc_unchanged() {
        // A title already in NFC (accents and CJK) is written as before.
        let mut g = Grid::empty(8);
        g.write(0, 8, "é日本", Style::NORMAL);
        assert_eq!(g.text(), "é日本   ");
        assert_eq!(text_width("é日本"), 5);
    }

    #[test]
    fn accent_style_and_module_target() {
        assert_eq!(Style::ACCENT.code(), 'a');
        let mut g = Grid::empty(4);
        g.zone(1, 3, Target::Module(crate::config::ModuleId::Volume));
        assert_eq!(g.target_at(2), Some(&Target::Module(crate::config::ModuleId::Volume)));
    }

    #[test]
    fn clickable_zones() {
        let mut g = Grid::empty(10);
        let t = Target::Workspace { reference: "4".into(), output: "HDMI-A-1".into() };
        g.zone(2, 5, t.clone());
        g.zone(6, 6, t.clone()); // empty: ignored
        assert_eq!(g.target_at(1), None);
        assert_eq!(g.target_at(2), Some(&t));
        assert_eq!(g.target_at(4), Some(&t));
        assert_eq!(g.target_at(5), None);
        assert_eq!(g.zones().len(), 1);
    }
}
