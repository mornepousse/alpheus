//! The Super+X overview's placement: where each card of the Tools menu sits, which of its tools it
//! shows, and how the arrows move between them.
//!
//! The cards are the Pinned card (the built-ins of `[tools] root` and the user entries with no
//! category) then one per category that holds an entry, in the root's order (`Catalog::root_rows`).
//! They are laid left to right in bands of up to `MAX_CARDS_PER_BAND`, all of one width; a band is
//! as tall as its tallest card and one blank row parts two bands. When the whole does not fit the
//! rows given, cards are folded one row at a time, tallest band first, into their first tools and a
//! last `+N more` row; the Pinned card folds last. `place` works in cells and the drawing (`view.rs`)
//! turns them into pixels.
//!
//! Never:
//! - log, or reach a thread, a file or a command: everything comes from the catalog handed in;
//! - panic, whatever the catalog or the size (a test throws random ones at it).
//!
//! See: docs/superpowers/specs/2026-11-26-superx-overview-design.md ("What the box shows", "Keys").

use super::catalog::{Catalog, Item};

/// The narrowest a card may be, in cells.
pub const MIN_CARD_COLUMNS: u16 = 34;
/// The most cards side by side.
pub const MAX_CARDS_PER_BAND: usize = 3;
/// The blank columns between two cards of a band.
const CARD_GAP: u16 = 1;
/// The widest the overview is placed: a band of the most cards at their narrowest, with their gaps
/// (spec "Box size": about 105 cells with the box's ring).
pub const FULL_BAND_COLUMNS: u16 = MAX_CARDS_PER_BAND as u16 * MIN_CARD_COLUMNS + (MAX_CARDS_PER_BAND as u16 - 1) * CARD_GAP;

/// Which card: the Pinned one, or a category's (an index into `Catalog::categories`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardId {
    Pinned,
    Category(usize),
}

/// One card, in cells from the overview's top left corner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardPlace {
    pub card: CardId,
    pub col: u16,
    pub width: u16,
    pub row: u16,
    /// Its band's height: the header, then the tool rows (and the `+N more` row) of the tallest card.
    pub rows: u16,
    /// The entries shown, as indices into `Catalog::entries`, in the card's order.
    pub shown: Vec<usize>,
    /// How many of its tools the fold hides: a last `+N more` row when not 0.
    pub more: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overview {
    /// Band by band, left to right.
    pub cards: Vec<CardPlace>,
    /// The width the cards take, at most the `max_columns` given.
    pub columns: u16,
    /// The height the bands take, at most the `max_rows` given.
    pub rows: u16,
}

/// A row the selection can rest on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// An index into `Catalog::entries`.
    Entry(usize),
    /// The `+N more` row of that category's card: it opens the category's level.
    More(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// A card while it is folded: its entries and how many rows below the header it may use.
struct Fold {
    card: CardId,
    entries: Vec<usize>,
    /// Rows below the header: every entry when not less than their count, else `visible - 1`
    /// entries and the `+N more` row.
    visible: usize,
    /// The fewest `visible` it folds to: 1 (the `+N more` row alone) for a category; for the
    /// Pinned card, whose `+N more` opens nothing, one tool and that row.
    floor: usize,
}

impl Fold {
    fn height(&self) -> usize {
        1 + self.visible.min(self.entries.len())
    }

    fn shown_and_more(&self) -> (Vec<usize>, usize) {
        let n = self.entries.len();
        if self.visible >= n {
            (self.entries.clone(), 0)
        } else {
            let shown = self.visible.saturating_sub(1);
            (self.entries.iter().take(shown).copied().collect(), n - shown)
        }
    }
}

/// A band's height: its tallest card's.
fn band_height(band: &[Fold]) -> usize {
    band.iter().map(Fold::height).max().unwrap_or(0)
}

/// The bands' heights and the blank rows between them.
fn total_height(folds: &[Fold], per_band: usize) -> usize {
    let heights: usize = folds.chunks(per_band).map(band_height).sum();
    heights + folds.chunks(per_band).count().saturating_sub(1)
}

/// Folds one row off the tallest band whose tallest cards may all fold (the Pinned card only when
/// `pinned` is set): each of those cards loses a row. False when no band can.
fn fold_one(folds: &mut [Fold], per_band: usize, pinned: bool) -> bool {
    let foldable = |f: &Fold| f.visible > f.floor && (pinned || f.card != CardId::Pinned);
    let mut best: Option<(usize, usize)> = None;
    for (b, band) in folds.chunks(per_band).enumerate() {
        let h = band_height(band);
        let ok = band.iter().filter(|f| f.height() == h).all(foldable);
        if ok && best.is_none_or(|(_, bh)| h > bh) {
            best = Some((b, h));
        }
    }
    let Some((b, h)) = best else { return false };
    for f in folds.chunks_mut(per_band).nth(b).into_iter().flatten() {
        if f.height() == h {
            f.visible = f.entries.len().min(f.visible).saturating_sub(1);
        }
    }
    true
}

fn cells(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

impl Overview {
    /// The cards of `catalog` placed in at most `max_columns` × `max_rows` cells. A band that does
    /// not fit even with every card folded is left out, with the bands after it (their tools are
    /// still found by the search).
    pub fn place(catalog: &Catalog, max_columns: u16, max_rows: u16) -> Overview {
        let rows = catalog.root_rows();
        let pinned: Vec<usize> = rows.iter().filter_map(|r| if let Item::Entry(e) = *r { Some(e) } else { None }).collect();
        let mut folds: Vec<Fold> = Vec::new();
        if !pinned.is_empty() {
            let floor = pinned.len().min(2);
            folds.push(Fold { card: CardId::Pinned, visible: pinned.len(), entries: pinned, floor });
        }
        for r in &rows {
            if let Item::Category(k) = *r {
                let entries = catalog.category_rows(k);
                folds.push(Fold { card: CardId::Category(k), visible: entries.len(), entries, floor: 1 });
            }
        }
        if folds.is_empty() || max_columns == 0 {
            return Overview::default();
        }
        let fit = usize::from(max_columns.saturating_add(CARD_GAP) / (MIN_CARD_COLUMNS + CARD_GAP));
        let per_band = fit.clamp(1, MAX_CARDS_PER_BAND).min(folds.len());

        while total_height(&folds, per_band) > usize::from(max_rows) {
            if !fold_one(&mut folds, per_band, false) && !fold_one(&mut folds, per_band, true) {
                // Not even fully folded: the last band goes.
                let bands = folds.chunks(per_band).count();
                folds.truncate(bands.saturating_sub(1) * per_band);
            }
        }
        let per_band = per_band.min(folds.len());
        if per_band == 0 {
            return Overview::default();
        }

        let gaps = CARD_GAP.saturating_mul(cells(per_band - 1));
        let width = max_columns.saturating_sub(gaps) / cells(per_band);
        let mut cards = Vec::with_capacity(folds.len());
        let mut top = 0usize;
        for band in folds.chunks(per_band) {
            let h = band_height(band);
            for (j, f) in band.iter().enumerate() {
                let (shown, more) = f.shown_and_more();
                cards.push(CardPlace {
                    card: f.card,
                    col: cells(j).saturating_mul(width.saturating_add(CARD_GAP)),
                    width,
                    row: cells(top),
                    rows: cells(h),
                    shown,
                    more,
                });
            }
            top += h + 1;
        }
        let columns = width.saturating_mul(cells(per_band)).saturating_add(gaps);
        Overview { cards, columns, rows: cells(top.saturating_sub(1)) }
    }

    /// The rows the selection can rest on, card by card, each top to bottom: its shown tools, then
    /// its `+N more` row (the Pinned card's opens nothing and is not one).
    pub fn order(&self) -> Vec<Slot> {
        let mut out = Vec::new();
        for c in &self.cards {
            out.extend(c.shown.iter().map(|&e| Slot::Entry(e)));
            if let (CardId::Category(k), true) = (c.card, c.more > 0) {
                out.push(Slot::More(k));
            }
        }
        out
    }

    /// The `order()` index an arrow leads to from `from`. Up and down move within a card, then to
    /// the card above (its last row) or below (its first row) in the same column, or the band's last
    /// card when the band below is shorter. Left and right go to the neighbour card's row of the
    /// same index, clamped to its last. With nowhere to go the selection stays.
    pub fn step(&self, from: usize, dir: Dir) -> usize {
        // Each card's first index in `order()` and its slot count.
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(self.cards.len());
        let mut start = 0;
        for c in &self.cards {
            let n = c.shown.len() + usize::from(matches!(c.card, CardId::Category(_)) && c.more > 0);
            spans.push((start, n));
            start += n;
        }
        if start == 0 {
            return 0;
        }
        let from = from.min(start - 1);
        let Some(card) = spans.iter().position(|&(s, n)| from >= s && from < s + n) else { return from };
        let first_row = self.cards.first().map(|c| c.row);
        let per_band = self.cards.iter().take_while(|c| Some(c.row) == first_row).count().max(1);
        let (band, col) = (card / per_band, card % per_band);
        let (card_start, card_slots) = spans.get(card).copied().unwrap_or((from, 1));
        let at = from - card_start;
        let slot = |card: usize, row: usize| -> Option<usize> {
            let &(s, n) = spans.get(card)?;
            (n > 0).then(|| s + row.min(n - 1))
        };
        let target = match dir {
            Dir::Up if at > 0 => Some(from - 1),
            Dir::Up => band.checked_sub(1).and_then(|b| slot(b * per_band + col, usize::MAX)),
            Dir::Down if at + 1 < card_slots => Some(from + 1),
            Dir::Down => {
                let below = (band + 1) * per_band;
                let last = spans.len().checked_sub(1).filter(|&l| l >= below);
                last.and_then(|l| slot((below + col).min(l), 0))
            }
            Dir::Left => col.checked_sub(1).and_then(|_| slot(card - 1, at)),
            Dir::Right if col + 1 < per_band => slot(card + 1, at),
            Dir::Right => None,
        };
        target.unwrap_or(from)
    }
}

#[cfg(test)]
mod tests;
