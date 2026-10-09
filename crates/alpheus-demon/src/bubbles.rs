//! The notification bubbles' layout (notifications spec § 3), pure: each shown notification as a
//! boxed set of rows (the glyph, the app name and the time; the summary in bold; up to three body
//! lines, newlines kept), where each bubble sits in the stack (logical px, a gap between two), which
//! bubble a pointer falls on, the input region, and what a left click does. The surface is
//! `bubbles_surface.rs`'s: nothing here talks to Wayland or a bus, starts a thread or runs a command.

use crate::grid::{Grid, Style};
use crate::list::{self, Boxed};
use crate::media::truncate_title;
use crate::notify::{Center, Entry, Shown};

/// Logical px between two bubbles, and between the bar and the first one.
pub const GAP: u32 = 6;
/// Body lines a bubble shows at most (spec § 2).
pub const BODY_LINES: usize = 3;
/// The action a left click on a bubble (or Enter in the panel) runs (spec § 3).
pub const DEFAULT_ACTION: &str = "default";
/// Cells of the time at the header's right end ("14:02").
const TIME: u16 = 5;

/// One bubble: its notification (id and revision: a late click on a replaced one does nothing) and
/// its rows, padded like the popups' (`list::padded`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BubbleBox {
    pub id: u32,
    pub revision: u32,
    pub boxed: Boxed,
    pub critical: bool,
}

/// "14:02" from minutes since local midnight.
pub fn clock(minute: u16) -> String {
    format!("{:02}:{:02}", (minute / 60) % 24, minute % 60)
}

/// The body as a bubble shows it: split on `\n`, blank lines dropped, at most `BODY_LINES`, each cut
/// to `width` cells (cluster-aware); the last one shown ends with "…" when lines were left out.
pub fn body_lines(body: &str, width: u16) -> Vec<String> {
    let lines: Vec<&str> = body.split('\n').map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let mut out: Vec<String> = lines.iter().take(BODY_LINES).map(|l| truncate_title(l, width)).collect();
    if lines.len() > BODY_LINES
        && let (Some(last), Some(line)) = (out.last_mut(), lines.get(BODY_LINES - 1))
        && !last.ends_with('…')
    {
        let cut = truncate_title(line, width.saturating_sub(1));
        *last = if cut.ends_with('…') { cut } else { format!("{cut}…") };
    }
    out
}

/// One bubble's rows, `width` cells of content.
fn bubble(shown: &Shown, width: u16) -> BubbleBox {
    let e = shown.entry;
    let mut rows = Vec::with_capacity(2 + BODY_LINES);
    let mut header = Grid::empty(width);
    let glyph = if shown.critical { Style::URGENT } else { Style::ACCENT };
    let x = header.write(0, width, &e.glyph.to_string(), glyph);
    let x = header.write(x, width, " ", Style::NORMAL);
    // The app name stops one cell before the time: a long one is cut, the time always shows.
    let app_end = width.saturating_sub(TIME + 1);
    header.write(x, app_end, &truncate_title(&e.app, app_end.saturating_sub(x)), Style::NORMAL);
    header.write(width.saturating_sub(TIME), width, &clock(e.at_minute), Style::MUTED);
    rows.push(header);
    if !e.summary.trim().is_empty() {
        let mut g = Grid::empty(width);
        g.write(0, width, &truncate_title(&e.summary, width), Style::BOLD);
        rows.push(g);
    }
    for line in body_lines(&e.body, width) {
        let mut g = Grid::empty(width);
        g.write(0, width, &line, Style::NORMAL);
        rows.push(g);
    }
    BubbleBox { id: e.id, revision: e.revision, boxed: list::padded(&rows), critical: shown.critical }
}

/// The stack as drawn, top to bottom, in the order given (`Center::bubbles`: newest first).
pub fn stack(shown: &[Shown], width: u16) -> Vec<BubbleBox> {
    shown.iter().map(|s| bubble(s, width)).collect()
}

/// Each bubble's (top, height) in logical px, top to bottom, `gap` px apart.
pub fn spans(heights: &[u32], gap: u32) -> Vec<(u32, u32)> {
    let mut top = 0u32;
    heights
        .iter()
        .map(|&h| {
            let span = (top, h);
            top = top.saturating_add(h).saturating_add(gap);
            span
        })
        .collect()
}

/// The surface's logical height: the last bubble's bottom.
pub fn total_height(spans: &[(u32, u32)]) -> u32 {
    spans.last().map_or(0, |&(top, h)| top.saturating_add(h))
}

/// The bubble under a pointer `y` logical px from the surface's top; None in a gap or past the last.
pub fn hit(spans: &[(u32, u32)], y: f64) -> Option<usize> {
    if !y.is_finite() || y < 0.0 {
        return None;
    }
    spans.iter().position(|&(top, h)| y >= f64::from(top) && y < f64::from(top) + f64::from(h))
}

fn px(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// The input region (spec § 3): one rect per bubble, `width` logical px wide, (x, y, w, h); the gaps
/// are left out, so a click between two bubbles reaches the window under them.
pub fn input_rects(spans: &[(u32, u32)], width: u32) -> Vec<(i32, i32, i32, i32)> {
    spans.iter().map(|&(top, h)| (0, px(top), px(width), px(h))).collect()
}

/// What a left click on a bubble does (spec § 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// `ActionInvoked("default")`, then closed (dismissed) unless the notification is resident.
    Invoke { resident: bool },
    /// No `default` action: the click only dismisses.
    Dismiss,
    /// Replaced, expired or closed since it was drawn: a late click does nothing.
    Stale,
}

/// Notification `id`, live (shown, waiting or hidden by do-not-disturb; transient ones included) or
/// in the history.
pub fn find(center: &Center, id: u32) -> Option<&Entry> {
    center.entry(id)
}

/// The left click's rule for the bubble drawn as (`entry`'s id, `revision`).
pub fn activation(entry: Option<&Entry>, revision: u32) -> Activation {
    match entry {
        Some(e) if e.live && e.revision == revision => {
            if e.actions.iter().any(|a| a.key == DEFAULT_ACTION) {
                Activation::Invoke { resident: e.resident }
            } else {
                Activation::Dismiss
            }
        }
        _ => Activation::Stale,
    }
}

/// Whether the stack moves to the focused output: only when what it shows changed (a bubble never
/// jumps under the pointer because the focus followed the mouse), and only to an output known by
/// name that is not the one it is on.
pub fn moves(stack_changed: bool, focused: Option<&str>, on: Option<&str>) -> bool {
    stack_changed && focused.is_some() && focused != on
}

/// Whether the output that went (`gone`) holds the bubbles placed on `on`. None (the compositor's
/// choice) is never held here: the compositor's own `closed` takes them down.
pub fn holds<T: PartialEq>(on: Option<&T>, gone: &T) -> bool {
    on == Some(gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotificationsConfig;
    use crate::grid::text_width;
    use crate::notify::{Action, CloseReason, Effect, Incoming, Urgency};
    use std::time::Instant;

    /// A live notification as `Center` keeps it (fictional apps and texts), at 14:02.
    fn entry(id: u32, app: &str, summary: &str, body: &str) -> Entry {
        Entry {
            id,
            revision: 1,
            identity: app.to_lowercase(),
            app: app.to_string(),
            glyph: 'M',
            summary: summary.to_string(),
            body: body.to_string(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            at_minute: 14 * 60 + 2,
            live: true,
            read: false,
            resident: false,
        }
    }

    fn shown(e: &Entry) -> Shown<'_> {
        Shown { entry: e, critical: false }
    }

    fn texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
    }

    #[test]
    fn a_bubble_shows_the_glyph_the_app_and_the_time_then_the_summary_in_bold() {
        let e = entry(7, "Mailer", "Weekly report ready", "");
        let s = stack(&[shown(&e)], 40);
        assert_eq!(s.len(), 1);
        let b = &s[0];
        assert_eq!((b.id, b.revision, b.critical), (7, 1, false));
        assert_eq!(b.boxed.cells(), (42, 2), "padded: one blank cell each side; the header and the summary, no body row");
        let t = texts(&b.boxed);
        // Pad, glyph, space, "Mailer" (cells 3 to 8), then blanks up to the time at cells 36 to 40.
        assert_eq!(t[0], format!(" M Mailer{}14:02", " ".repeat(27)));
        assert_eq!(t[1], " Weekly report ready");
        assert_eq!(&b.boxed.rows[0].styles()[1..2], "a", "the glyph in the accent");
        assert_eq!(&b.boxed.rows[0].styles()[3..9], "......", "the app name plain");
        assert_eq!(&b.boxed.rows[0].styles()[36..41], "mmmmm", "the time muted, right-aligned");
        assert_eq!(&b.boxed.rows[1].styles()[1..20], "N".repeat(19), "the summary in bold");
        assert_eq!(b.boxed.rows[0].cells()[41].ch, ' ', "the right pad cell is never written over");
    }

    #[test]
    fn the_body_keeps_its_newlines_up_to_three_lines() {
        let e = entry(1, "Chat", "Lina", "First line\n\nSecond line\r\nThird line\nFourth line");
        let t = texts(&stack(&[shown(&e)], 40)[0].boxed);
        assert_eq!(&t[1..], [" Lina", " First line", " Second line", " Third line…"], "blank lines dropped; the cut marked");
        assert_eq!(body_lines("one\ntwo\nthree", 40), ["one", "two", "three"], "exactly three: nothing marked");
        assert!(body_lines("", 40).is_empty() && body_lines("\n \n", 40).is_empty(), "no body: no row");
        let e = entry(2, "Chat", "", "Only a body");
        assert_eq!(&texts(&stack(&[shown(&e)], 40)[0].boxed)[1..], [" Only a body"], "an empty summary takes no row");
    }

    #[test]
    fn long_and_wide_text_is_cut_to_the_bubble_width() {
        let long = "x".repeat(80);
        let wide = "日本語の通知のとても長い件名です日本語の通知のとても長い件名です";
        let e = entry(1, &long, wide, &format!("{long}\n{wide}"));
        let b = &stack(&[shown(&e)], 24)[0];
        assert_eq!(b.boxed.cells(), (26, 4));
        for g in &b.boxed.rows {
            assert_eq!(g.width(), 26);
            assert_eq!((g.cells()[0].ch, g.cells()[25].ch), (' ', ' '), "the pad cells stay blank");
        }
        let t = texts(&b.boxed);
        assert!(t[0].ends_with("14:02"), "the time survives a long app name: {}", t[0]);
        assert!(t[0].contains('…'), "{}", t[0]);
        for row in &t[1..] {
            assert!(row.ends_with('…'), "cut: {row}");
            assert!(text_width(row.trim()) <= 24, "{row}: {} cells", text_width(row.trim()));
        }
    }

    #[test]
    fn a_critical_bubble_is_flagged_and_its_glyph_urgent() {
        let (a, b) = (entry(1, "Mailer", "One", ""), entry(2, "Monitor", "Disk almost full", ""));
        let s = stack(&[Shown { entry: &b, critical: true }, shown(&a)], 40);
        assert_eq!(s.iter().map(|x| (x.id, x.critical)).collect::<Vec<_>>(), [(2, true), (1, false)], "the order given: newest first");
        assert_eq!(&s[0].boxed.rows[0].styles()[1..2], "u");
        assert_eq!(&s[1].boxed.rows[0].styles()[1..2], "a");
    }

    #[test]
    fn spans_leave_a_gap_and_hit_skips_it() {
        let s = spans(&[50, 70, 40], GAP);
        assert_eq!(s, [(0, 50), (56, 70), (132, 40)]);
        assert_eq!(total_height(&s), 172);
        assert_eq!(hit(&s, 0.0), Some(0));
        assert_eq!(hit(&s, 49.9), Some(0));
        assert_eq!(hit(&s, 50.0), None, "the gap");
        assert_eq!(hit(&s, 55.9), None);
        assert_eq!(hit(&s, 56.0), Some(1));
        assert_eq!(hit(&s, 171.9), Some(2));
        assert_eq!(hit(&s, 172.0), None, "past the last");
        assert_eq!(hit(&s, -1.0), None);
        assert_eq!(hit(&s, f64::NAN), None);
        assert_eq!(input_rects(&s, 300), [(0, 0, 300, 50), (0, 56, 300, 70), (0, 132, 300, 40)], "one rect per bubble, never the gaps");
        assert!(spans(&[], GAP).is_empty());
        assert_eq!(total_height(&[]), 0);
    }

    #[test]
    fn a_left_click_invokes_the_default_action_dismisses_or_is_stale() {
        let mut e = entry(3, "Mailer", "Report", "");
        assert_eq!(activation(Some(&e), 1), Activation::Dismiss, "no default action: it only dismisses");
        e.actions =
            vec![Action { key: "later".into(), label: "Later".into() }, Action { key: DEFAULT_ACTION.into(), label: "Open".into() }];
        assert_eq!(activation(Some(&e), 1), Activation::Invoke { resident: false });
        e.resident = true;
        assert_eq!(activation(Some(&e), 1), Activation::Invoke { resident: true });
        assert_eq!(activation(Some(&e), 2), Activation::Stale, "replaced since it was drawn");
        e.live = false;
        assert_eq!(activation(Some(&e), 1), Activation::Stale, "expired or closed meanwhile");
        assert_eq!(activation(None, 1), Activation::Stale);
    }

    /// Review Focus: the focused output changing while bubbles are shown.
    #[test]
    fn the_stack_moves_to_the_focused_output_only_when_it_changes() {
        assert!(!moves(false, Some("HEADLESS-1"), Some("HEADLESS-2")), "focus alone: they stay put");
        assert!(moves(true, Some("HEADLESS-1"), Some("HEADLESS-2")), "the stack changed: to the focused output");
        assert!(!moves(true, Some("HEADLESS-2"), Some("HEADLESS-2")));
        assert!(!moves(true, None, Some("HEADLESS-2")), "no known focus: where they are");
        assert!(moves(true, Some("HEADLESS-1"), None), "from the compositor's choice to a known focus");
    }

    /// Review Focus: an output unplugged. Only the one the bubbles were placed on takes them down;
    /// placed by the compositor (no output asked), they wait for its `closed`.
    #[test]
    fn only_the_output_holding_the_bubbles_takes_them_down() {
        assert!(holds(Some(&"HEADLESS-2"), &"HEADLESS-2"));
        assert!(!holds(Some(&"HEADLESS-2"), &"HEADLESS-1"), "another output going changes nothing");
        assert!(!holds(None, &"HEADLESS-1"), "the compositor's choice: its own closed tells");
    }

    fn incoming(summary: &str, urgency: Urgency) -> Incoming {
        Incoming {
            app_name: "Monitor".into(),
            replaces_id: 0,
            summary: summary.into(),
            body: String::new(),
            actions: Vec::new(),
            urgency,
            expire_ms: -1,
            desktop_entry: None,
            category: None,
            transient: false,
            resident: false,
            tag: None,
        }
    }

    /// Review Focus: 4 visible and 6 waiting; dismissing the top one shows the next waiting one
    /// (`Center::bubbles` is the newest `max_visible` live ones), and `find` reaches both.
    #[test]
    fn four_shown_six_waiting_and_a_dismissal_shows_the_next() {
        let cfg = NotificationsConfig::default();
        let mut c = Center::new(&cfg, false);
        let now = Instant::now();
        let ids: Vec<u32> = (0..10).map(|n| c.notify(incoming(&format!("Alert {n}"), Urgency::Critical), now, 600)).collect();
        let shown = |c: &Center| stack(&c.bubbles(), cfg.width).iter().map(|b| b.id).collect::<Vec<_>>();
        assert_eq!(shown(&c), [ids[9], ids[8], ids[7], ids[6]], "max_visible, newest on top");
        assert_eq!(find(&c, ids[2]).map(|e| e.id), Some(ids[2]), "a waiting one is found too");
        assert_eq!(c.close(ids[9], CloseReason::Dismissed), [Effect::Closed(ids[9], CloseReason::Dismissed)]);
        assert_eq!(shown(&c), [ids[8], ids[7], ids[6], ids[5]], "the next waiting one shows");
        assert_eq!(activation(find(&c, ids[9]), 1), Activation::Stale, "the dismissed one: a late click does nothing");
    }
}
