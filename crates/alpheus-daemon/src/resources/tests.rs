use super::*;
use crate::config::ResourceItem::{Cpu, Disk, Gpu, Memory, Net, Temperature};
use crate::grid::Hue;
use crate::system::{HISTORY, NetRate, Sample};

fn texts(b: &Boxed) -> Vec<String> {
    b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
}

fn ring(samples: &[Sample]) -> Resources {
    let mut r = Resources::default();
    for s in samples {
        r.push(*s);
    }
    r
}

fn one(s: Sample) -> History {
    ring(&[s]).history
}

/// The last point of the item's first series.
fn last_of(item: ResourceItem, s: Sample) -> Option<u16> {
    *series(item, &one(s))[0].last().expect("a series is never empty")
}

#[test]
fn series_scales_and_their_edges() {
    for (cpu, want) in [(0u8, 0u16), (50, 500), (100, 1000)] {
        assert_eq!(last_of(Cpu, Sample { cpu: Some(cpu), ..Sample::default() }), Some(want), "cpu {cpu}");
    }
    assert_eq!(last_of(Disk, Sample { disk: Some(62), ..Sample::default() }), Some(620));
    let mem = Sample { memory: Some(61), memory_total: Some(155), ..Sample::default() };
    assert_eq!(last_of(Memory, mem), Some(61 * 1000 / 155));
    assert_eq!(last_of(Memory, Sample { memory: Some(61), memory_total: None, ..Sample::default() }), None, "no total, no point");
    assert_eq!(last_of(Memory, Sample { memory: Some(61), memory_total: Some(0), ..Sample::default() }), None, "a zero total: no divide");
    assert_eq!(last_of(Memory, Sample { memory: Some(900), memory_total: Some(155), ..Sample::default() }), Some(1000), "capped");
    for (t, want) in [(-5i16, 0u16), (54, 540), (130, 1000)] {
        assert_eq!(last_of(Temperature, Sample { temperature: Some(t), ..Sample::default() }), Some(want), "{t}°");
        assert_eq!(last_of(Gpu, Sample { gpu: Some(t), ..Sample::default() }), Some(want), "gpu {t}°");
    }
    // The ring is always HISTORY long: leading gaps until it is full (a fixed time axis).
    let h = one(Sample { cpu: Some(50), ..Sample::default() });
    let s = &series(Cpu, &h)[0];
    assert_eq!(s.len(), HISTORY);
    assert!(s[..HISTORY - 1].iter().all(Option::is_none) && s[HISTORY - 1] == Some(500));

    let zero = Sample { net: Some(NetRate { down: 0, up: 0 }), ..Sample::default() };
    let h = one(zero);
    assert_eq!(net_top(&h), Some(NET_FLOOR));
    let n = series(Net, &h);
    assert_eq!(n.len(), 2, "down and up");
    assert!(n.iter().all(|s| s[HISTORY - 1] == Some(0)));
    let huge = Sample { net: Some(NetRate { down: u64::MAX, up: 1 }), ..Sample::default() };
    let h = one(huge);
    assert_eq!(net_top(&h), Some(u64::MAX));
    assert_eq!(series(Net, &h)[0][HISTORY - 1], Some(1000), "no overflow");
    assert_eq!(series(Net, &h)[1][HISTORY - 1], Some(0));
    let none = one(Sample::default());
    assert_eq!(net_top(&none), None);
    assert!(series(Net, &none).iter().all(|s| s.iter().all(Option::is_none)));
    let half = one(Sample { net: Some(NetRate { down: NET_FLOOR * 2, up: NET_FLOOR }), ..Sample::default() });
    assert_eq!(series(Net, &half)[1][HISTORY - 1], Some(500), "both over the one top");
}

#[test]
fn the_rows_for_the_default_items() {
    let s = Sample { cpu: Some(12), memory: Some(61), memory_total: Some(155), temperature: Some(54), ..Sample::default() };
    let r = ring(&[s; 60]);
    let c = ResourcesConfig::default();
    let b = ResourcesPopup::default().render(&r, &c);
    let t = texts(&b);
    assert_eq!(b.rows.len(), 12);
    assert_eq!(b.separators, [10]);
    assert!(t[0].starts_with(" Resources") && t[0].ends_with("last 2 min"), "{:?}", t[0]);
    assert!(t[1].ends_with("12%") && t[1].contains("CPU"), "{:?}", t[1]);
    assert!(t[4].ends_with("6.1G of 15.5G") && t[4].contains("Memory"), "{:?}", t[4]);
    assert!(t[7].ends_with("54°") && t[7].contains("Temperature"), "{:?}", t[7]);
    for blank in [2, 3, 5, 6, 8, 9, 10] {
        assert_eq!(t[blank], "", "row {blank}");
    }
    assert_eq!(t[11], " Monitor…");
    assert_eq!(monitor_row(&c), 11);
    assert_eq!(b.cells(), (WIDTH, 12));
    assert_eq!(b.graphs.len(), 3);
    for (g, (row, hue)) in b.graphs.iter().zip([(2usize, Hue::Orange), (5, Hue::Yellow), (8, Hue::Red)]) {
        assert_eq!((g.row, g.col, g.width, g.rows), (row, 1, 38, GRAPH_ROWS), "{hue:?}");
        assert_eq!(g.series.len(), 1);
        assert_eq!(g.series[0].points.len(), HISTORY);
        assert_eq!(g.series[0].ink, Tint::Hue(hue));
        assert!(g.series[0].fill);
    }
    assert_eq!(b.graphs[0].series[0].points[HISTORY - 1], Some(120), "the newest at the right edge");
    // The icon in its hue, the name plain.
    assert_eq!(b.rows[1].cells()[1].style, Style::hue(Hue::Orange));
    assert_eq!(b.rows[4].cells()[1].style, Style::hue(Hue::Yellow));
    assert_eq!(b.rows[7].cells()[1].style, Style::hue(Hue::Red));
}

#[test]
fn the_rows_keep_every_item_without_a_figure() {
    let c = ResourcesConfig { show: vec![Net, Disk, Gpu], ..ResourcesConfig::default() };
    let empty = ResourcesPopup::default().render(&Resources::default(), &c);
    let t = texts(&empty);
    for row in [1, 4, 7] {
        assert!(t[row].ends_with('—'), "{:?}", t[row]);
    }
    assert_eq!(empty.graphs[0].series.len(), 2, "the net graph has down and up");
    assert_eq!((empty.graphs[1].series.len(), empty.graphs[2].series.len()), (1, 1));
    let (down, up) = (&empty.graphs[0].series[0], &empty.graphs[0].series[1]);
    assert_eq!((down.ink, down.fill), (Tint::Hue(Hue::Cyan), true));
    assert_eq!((up.ink, up.fill), (Tint::FgDim, false));
    let full = Sample { net: Some(NetRate { down: 5, up: 6 }), disk: Some(40), gpu: Some(50), ..Sample::default() };
    let with = ResourcesPopup::default().render(&ring(&[full]), &c);
    assert_eq!(with.cells(), empty.cells(), "the size never depends on the figures");
    assert_eq!(texts(&with).len(), texts(&empty).len());
}

#[test]
fn hot_temperatures_and_net_figures() {
    let c = ResourcesConfig { show: vec![Temperature], ..ResourcesConfig::default() };
    let figure_styles = |t: i16| {
        let b = ResourcesPopup::default().render(&ring(&[Sample { temperature: Some(t), ..Sample::default() }]), &c);
        let cells = b.rows[1].cells();
        let text = b.rows[1].text();
        let at = text.trim_end().chars().count() - format!("{t}°").chars().count();
        cells[at..at + format!("{t}°").chars().count()].iter().map(|x| x.style).collect::<Vec<_>>()
    };
    assert!(figure_styles(85).iter().all(|s| *s == Style::ERROR));
    assert!(figure_styles(84).iter().all(|s| *s == Style::hue(Hue::Red)));
    let c = ResourcesConfig { show: vec![Net], ..ResourcesConfig::default() };
    let peak = Sample { net: Some(NetRate { down: 3_565_158, up: 0 }), ..Sample::default() };
    let now = Sample { net: Some(NetRate { down: 1_258_291, up: 81_920 }), ..Sample::default() };
    let b = ResourcesPopup::default().render(&ring(&[peak, now]), &c);
    let row = &texts(&b)[1];
    assert!(row.ends_with("peak 3.4M \u{f01da} 1.2M \u{f0552} 80K"), "{row:?}");
    let style_of = |needle: &str| {
        let chars = b.rows[1].text().chars().collect::<Vec<_>>();
        let hay: String = chars.iter().collect();
        let col = hay[..hay.find(needle).expect(needle)].chars().count();
        b.rows[1].cells()[col].style
    };
    assert_eq!(style_of("peak"), Style::MUTED);
    assert_eq!(style_of("1.2M"), Style::hue(Hue::Cyan));
}

/// The widest net row: `peak` and both rates at the five-cell cap (`9999G`; 1023 MiB/s itself reads
/// `1.0G`). It must fit the inner cells and leave the
/// icon and the name untouched.
#[test]
fn the_net_row_fits_at_the_maximum_rate() {
    let c = ResourcesConfig { show: vec![Net], ..ResourcesConfig::default() };
    let max = 10_000 * 1024 * 1024 * 1024;
    let s = Sample { net: Some(NetRate { down: max, up: max }), ..Sample::default() };
    let b = ResourcesPopup::default().render(&ring(&[s]), &c);
    let row = &texts(&b)[1];
    assert!(row.ends_with("peak 9999G \u{f01da} 9999G \u{f0552} 9999G"), "{row:?}");
    let width = text_width(row.trim_start());
    assert!(width <= WIDTH - 2, "{width} cells in {row:?}: the inner width is {}", WIDTH - 2);
    let name = row.find("Network").expect("the name survives");
    let figure = row.find("peak").expect("the figure");
    assert!(name + "Network".len() < figure, "the figure overwrote the name: {row:?}");
}

#[test]
fn the_span_follows_the_interval() {
    for (interval, want) in [(1u64, "last 1 min"), (60, "last 60 min")] {
        let c = ResourcesConfig { interval, ..ResourcesConfig::default() };
        let b = ResourcesPopup::default().render(&Resources::default(), &c);
        assert!(texts(&b)[0].ends_with(want), "{interval}: {:?}", texts(&b)[0]);
    }
}

#[test]
fn keys_follow_the_power_menu() {
    let mut p = ResourcesPopup::default();
    assert!(!p.selected);
    assert_eq!(p.key(Key::Enter), ResourcesAction::Nothing, "nothing selected at open");
    assert_eq!(p.key(Key::Char(' ')), ResourcesAction::Nothing);
    assert_eq!(p.key(Key::Down), ResourcesAction::Redraw);
    assert!(p.selected);
    assert_eq!(p.key(Key::Down), ResourcesAction::Nothing, "already selected");
    assert_eq!(p.key(Key::Enter), ResourcesAction::Monitor);
    assert_eq!(p.key(Key::Char(' ')), ResourcesAction::Monitor);
    for k in [Key::Up, Key::Tab, Key::BackTab, Key::Home, Key::End] {
        let mut p = ResourcesPopup::default();
        assert_eq!(p.key(k), ResourcesAction::Redraw, "{k:?}");
        assert!(p.selected, "{k:?}");
    }
    assert_eq!(p.key(Key::Escape), ResourcesAction::Close);
    let mut p = ResourcesPopup::default();
    assert_eq!(p.key(Key::Char('x')), ResourcesAction::Nothing);
    assert!(!p.selected);
}

#[test]
fn the_selection_band_spans_the_monitor_row() {
    let c = ResourcesConfig::default();
    let mut p = ResourcesPopup::default();
    p.key(Key::Down);
    let b = p.render(&Resources::default(), &c);
    assert_eq!(b.rows[usize::from(monitor_row(&c))].styles(), "S".repeat(usize::from(WIDTH)));
}

#[test]
fn the_pointer_selects_and_runs_the_monitor_row() {
    let c = ResourcesConfig::default();
    let m = monitor_row(&c);
    let mut p = ResourcesPopup::default();
    assert_eq!(p.pointer(Some(m), PopupPointer::Motion, &c), ResourcesAction::Redraw);
    assert!(p.selected);
    assert_eq!(p.pointer(Some(m), PopupPointer::Press(Button::Left), &c), ResourcesAction::Monitor);
    assert_eq!(p.pointer(Some(0), PopupPointer::Press(Button::Left), &c), ResourcesAction::Nothing);
    assert_eq!(p.pointer(Some(m), PopupPointer::Press(Button::Right), &c), ResourcesAction::Nothing);
    for kind in [PopupPointer::Scroll(1), PopupPointer::Leave] {
        assert_eq!(p.pointer(Some(m), kind, &c), ResourcesAction::Nothing, "{kind:?}");
        assert_eq!(p.pointer(None, kind, &c), ResourcesAction::Nothing);
    }
    assert_eq!(p.pointer(None, PopupPointer::Motion, &c), ResourcesAction::Nothing);
    let mut fresh = ResourcesPopup::default();
    assert_eq!(fresh.pointer(Some(m), PopupPointer::Press(Button::Left), &c), ResourcesAction::Monitor, "a press needs no hover first");
}

/// `[actions] resources` empty: no separator, no `Monitor…`, nothing to select, run or hover.
#[test]
fn no_monitor_row_without_a_command() {
    let c = ResourcesConfig::default();
    let mut p = ResourcesPopup::new(false);
    let b = p.render(&Resources::default(), &c);
    assert_eq!(b.rows.len(), 10, "the header, three blocks, one blank");
    assert!(b.separators.is_empty());
    assert!(texts(&b).iter().all(|t| !t.contains("Monitor")), "{:?}", texts(&b));
    for k in [Key::Down, Key::Up, Key::Tab, Key::BackTab, Key::Home, Key::End, Key::Enter, Key::Char(' ')] {
        assert_eq!(p.key(k), ResourcesAction::Nothing, "{k:?}");
    }
    assert!(!p.selected);
    // The row the button would have been on, and a left press on it.
    let m = monitor_row(&c);
    assert_eq!(p.pointer(Some(m), PopupPointer::Motion, &c), ResourcesAction::Nothing);
    assert_eq!(p.pointer(Some(m), PopupPointer::Press(Button::Left), &c), ResourcesAction::Nothing);
    assert_eq!(p.key(Key::Escape), ResourcesAction::Close);
    let with = ResourcesPopup::new(true).render(&Resources::default(), &c);
    assert_eq!(with.rows.len(), 12);
}
