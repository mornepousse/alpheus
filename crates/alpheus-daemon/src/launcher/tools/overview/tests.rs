use super::*;
use crate::launcher::config::LauncherConfig;
use crate::launcher::tools::catalog::{BUILTINS, Context, EntryKind, Item};

/// Everything shown but "Stop recording".
fn all() -> Context {
    Context {
        quick: true,
        notifications: true,
        timer: true,
        keep_awake: true,
        night_light: true,
        lock: true,
        session: true,
        reload: true,
        shield: true,
        clipboard: true,
        diagnostics: true,
        nmap: true,
        usb_reset: true,
        shutdown: true,
        shutdown_left: None,
        captures: true,
        recording: false,
        annotate: true,
        ocr: true,
        record: true,
    }
}

fn catalog(text: &str, c: &Context) -> Catalog {
    Catalog::build(&LauncherConfig::from_text(text).unwrap().tools, c, &mut |_| {})
}

fn categories_in_order(cat: &Catalog) -> Vec<CardId> {
    cat.root_rows()
        .into_iter()
        .filter_map(|i| match i {
            Item::Category(k) => Some(CardId::Category(k)),
            Item::Entry(_) => None,
        })
        .collect()
}

/// The cards' indices grouped by band (same `row`).
fn bands(o: &Overview) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (i, c) in o.cards.iter().enumerate() {
        match out.last_mut() {
            Some(b) if o.cards[b[0]].row == c.row => b.push(i),
            _ => out.push(vec![i]),
        }
    }
    out
}

fn assert_inside(o: &Overview, max_columns: u16, max_rows: u16) {
    assert!(o.columns <= max_columns && o.rows <= max_rows, "{}x{} in {max_columns}x{max_rows}", o.columns, o.rows);
    for c in &o.cards {
        assert!(c.col as usize + c.width as usize <= o.columns as usize, "{c:?} past {} columns", o.columns);
        assert!(c.row as usize + c.rows as usize <= o.rows as usize, "{c:?} past {} rows", o.rows);
        let used = 1 + c.shown.len() + usize::from(c.more > 0);
        assert!(used <= c.rows as usize, "{c:?} overflows its rows");
    }
}

#[test]
fn cards_follow_category_order_three_per_band() {
    let cat = catalog("", &all());
    let o = Overview::place(&cat, 110, 60);
    let ids: Vec<CardId> = o.cards.iter().map(|c| c.card).collect();
    assert_eq!(ids, categories_in_order(&cat), "Pinned absent, categories in order");
    assert_eq!(bands(&o).iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 1]);
    let w = o.cards[0].width;
    assert!(w >= MIN_CARD_COLUMNS);
    assert!(o.cards.iter().all(|c| c.width == w), "equal widths");
    for b in bands(&o) {
        for pair in b.windows(2) {
            assert!(o.cards[pair[0]].col + w < o.cards[pair[1]].col + 1, "left to right, no overlap");
        }
    }
    assert!(o.columns <= 110);
    assert_inside(&o, 110, 60);
    for c in &o.cards {
        let CardId::Category(k) = c.card else { panic!() };
        assert_eq!(c.shown, cat.category_rows(k), "nothing folded in a tall box");
        assert_eq!(c.more, 0);
    }
}

#[test]
fn pinned_card_comes_first() {
    let cat = catalog("[tools]\nroot = [\"notes\"]\n", &all());
    let notes = cat.entries.iter().position(|e| matches!(e.kind, EntryKind::Builtin(b) if BUILTINS[b].id == "notes")).unwrap();
    let o = Overview::place(&cat, 110, 60);
    assert_eq!(o.cards[0].card, CardId::Pinned);
    assert_eq!(o.cards[0].shown, [notes]);
    assert_eq!(o.cards[1..].iter().map(|c| c.card).collect::<Vec<_>>(), categories_in_order(&cat));
}

#[test]
fn a_band_shares_its_tallest_card_s_height() {
    let cat = catalog("", &all());
    let o = Overview::place(&cat, 110, 60);
    for b in bands(&o) {
        let tallest = b.iter().map(|&i| 1 + o.cards[i].shown.len()).max().unwrap();
        for &i in &b {
            assert_eq!(o.cards[i].rows as usize, tallest, "card {i}");
        }
    }
}

#[test]
fn bands_are_separated_by_one_blank_row() {
    let cat = catalog("", &all());
    let o = Overview::place(&cat, 110, 60);
    let b = bands(&o);
    assert_eq!(o.cards[b[0][0]].row, 0);
    for pair in b.windows(2) {
        let above = &o.cards[pair[0][0]];
        assert_eq!(o.cards[pair[1][0]].row, above.row + above.rows + 1);
    }
    let last = &o.cards[b[b.len() - 1][0]];
    assert_eq!(o.rows, last.row + last.rows);
}

#[test]
fn a_small_box_folds_every_card() {
    let cat = catalog("", &all());
    let o = Overview::place(&cat, 70, 14);
    assert_eq!(o.cards.len(), categories_in_order(&cat).len(), "every card still placed");
    assert_inside(&o, 70, 14);
    let order = o.order();
    let mut folded = 0;
    for c in &o.cards {
        let CardId::Category(k) = c.card else { panic!() };
        let all_rows = cat.category_rows(k);
        assert_eq!(c.shown.len() + c.more, all_rows.len(), "{c:?}");
        assert_eq!(c.shown, all_rows[..c.shown.len()], "the first tools show");
        if c.more > 0 {
            folded += 1;
            let at = order.iter().position(|s| *s == Slot::More(k)).expect("a +N more slot");
            let card: Vec<Slot> = c.shown.iter().map(|&e| Slot::Entry(e)).chain([Slot::More(k)]).collect();
            assert_eq!(order[at + 1 - card.len()..=at], card, "+N more comes last in its card");
        }
    }
    assert!(folded > 0);
}

#[test]
fn the_pinned_card_folds_last() {
    let text: String = (0..8).map(|i| format!("[[tools.entry]]\nname = \"P{i}\"\nrun = [\"true\"]\n")).collect();
    let cat = catalog(&text, &all());
    let o = Overview::place(&cat, 110, 20);
    assert_inside(&o, 110, 20);
    assert_eq!(o.cards[0].card, CardId::Pinned);
    assert_eq!((o.cards[0].shown.len(), o.cards[0].more), (8, 0), "categories fold first");
    assert!(o.cards.iter().any(|c| c.more > 0));
    // Too small for anything but folding it too: it keeps at least one tool, and its
    // "+N more" row is no slot (no level to open).
    let o = Overview::place(&cat, 110, 5);
    assert_inside(&o, 110, 5);
    let pinned = &o.cards[0];
    assert_eq!(pinned.card, CardId::Pinned);
    assert!(!pinned.shown.is_empty() && pinned.more > 0, "{pinned:?}");
    assert!(o.order().iter().all(|s| matches!(s, Slot::Entry(_) | Slot::More(_))));
    assert_eq!(o.order().first(), pinned.shown.first().map(|&e| Slot::Entry(e)).as_ref());
}

#[test]
fn steps_move_within_and_across_cards() {
    let cat = catalog("", &all());
    let o = Overview::place(&cat, 110, 60);
    let order = o.order();
    let at = |card: usize, row: usize| order.iter().position(|s| *s == Slot::Entry(o.cards[card].shown[row])).unwrap();
    let last = |card: usize| o.cards[card].shown.len() - 1;
    // Within a card.
    assert_eq!(o.step(at(0, 0), Dir::Down), at(0, 1));
    assert_eq!(o.step(at(0, 1), Dir::Up), at(0, 0));
    // Across cards: the same row, clamped.
    assert_eq!(o.step(at(0, 2), Dir::Right), at(1, 2));
    assert_eq!(o.step(at(1, 6), Dir::Right), at(2, last(2)), "clamped to its last row");
    assert_eq!(o.step(at(1, 2), Dir::Left), at(0, 2));
    assert_eq!(o.step(at(0, 2), Dir::Left), at(0, 2), "the first card stays");
    assert_eq!(o.step(at(2, 1), Dir::Right), at(2, 1), "the band's last card stays");
    // Across bands.
    assert_eq!(o.step(at(0, last(0)), Dir::Down), at(3, 0), "past a band's bottom: the card below");
    assert_eq!(o.step(at(2, last(2)), Dir::Down), at(5, 0), "the card below, same column");
    assert_eq!(o.step(at(3, 0), Dir::Up), at(0, last(0)));
    assert_eq!(o.step(at(4, 1), Dir::Down), at(4, 2));
    assert_eq!(o.step(at(4, last(4)), Dir::Down), at(6, 0), "clamped to the last card below");
    assert_eq!(o.step(at(6, last(6)), Dir::Down), at(6, last(6)), "the last card's last slot stays");
    assert_eq!(o.step(at(0, 0), Dir::Up), at(0, 0), "the first slot stays");
    // Out of range is total.
    assert!(o.step(usize::MAX, Dir::Down) < order.len());
}

/// A small deterministic generator (no dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % below.max(1)
    }
}

#[test]
fn place_is_total() {
    let mut r = Lcg(0x5eed);
    for _ in 0..400 {
        let categories = r.next(17);
        let mut text = format!("[tools]\nbuiltins = {}\nroot = [\"notes\", \"lock\"]\n", r.next(2) == 0);
        for k in 0..categories {
            text.push_str(&format!("[[tools.category]]\nname = \"C{k}\"\norder = {}\n", r.next(100)));
        }
        for i in 0..r.next(41) {
            text.push_str(&format!("[[tools.entry]]\nname = \"E{i}\"\nrun = [\"true\"]\n"));
            let k = r.next(categories + 2);
            if k < categories {
                text.push_str(&format!("category = \"C{k}\"\n"));
            }
        }
        let c = if r.next(2) == 0 { all() } else { Context::default() };
        let cat = catalog(&text, &c);
        let (w, h) = (1 + r.next(200) as u16, 1 + r.next(80) as u16);
        let o = Overview::place(&cat, w, h);
        assert_inside(&o, w, h);
        for card in &o.cards {
            assert!(card.shown.iter().all(|&e| e < cat.entries.len()));
        }
        let n = o.order().len();
        for from in 0..n {
            for d in [Dir::Up, Dir::Down, Dir::Left, Dir::Right] {
                assert!(o.step(from, d) < n, "{from} {d:?} at {w}x{h}");
            }
        }
    }
    let empty = Overview::place(&catalog("[tools]\nbuiltins = false\n", &all()), 0, 0);
    assert!(empty.cards.is_empty() && empty.order().is_empty());
    assert_eq!(empty.step(3, Dir::Down), 0);
}
