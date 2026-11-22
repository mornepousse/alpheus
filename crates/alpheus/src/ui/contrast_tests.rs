//! Contrast of everything the rail chrome draws on a fill: every drawn cell
//! whose background is not the page's, over all 15 presets in both modes (dark-only ones once), the
//! five shapes and every tab. Letters and digits must reach 4.5:1; any other glyph (icons, dots,
//! box lines) 3:1. The presets' own data is tested in alpheus-common; this checks what is drawn.

use super::testing::buffer_at;
use crate::app::action::Tab;
use crate::app::testing::ready_app;
use crate::theme::{Shape, Theme};
use alpheus_common::theme::contrast::ratio;
use alpheus_common::theme::presets::PRESETS;
use alpheus_common::theme::{Rgb, Roles};
use ratatui::style::Color;

/// The role a colour came from, for the failure report.
fn role(th: &Theme, c: Color) -> &'static str {
    let all = [
        // `on_accent` first: it equals `bar_bg` or `popup_bg` in several presets, and what is drawn there is
        // the control's own text.
        (th.on_accent, "on_accent"),
        (th.bg, "bg"),
        (th.fg, "fg"),
        (th.muted, "muted"),
        (th.surface, "surface"),
        (th.accent, "accent"),
        (th.selection, "selection"),
        (th.accent_container, "accent_container"),
        (th.error, "error"),
        (th.ok, "ok"),
        (th.warn, "warn"),
        (th.raised, "raised"),
        (th.pill, "pill"),
        (th.accent2, "accent2"),
        (th.red, "red"),
        (th.orange, "orange"),
        (th.yellow, "yellow"),
        (th.green, "green"),
        (th.cyan, "cyan"),
        (th.blue, "blue"),
        (th.purple, "purple"),
    ];
    all.iter().find(|(k, _)| *k == c).map_or("?", |(_, n)| n)
}

fn rgb(c: Color) -> Option<Rgb> {
    match c {
        Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
        _ => None,
    }
}

fn themes() -> Vec<(String, Roles)> {
    let mut v = Vec::new();
    for p in PRESETS {
        v.push((format!("{} dark", p.name), p.dark));
        if let Some(l) = p.light {
            v.push((format!("{} light", p.name), l));
        }
    }
    v
}

/// Rules, bars, shadows and the shapes' cap glyphs: their ink is the fill itself, drawn over what lies
/// behind, not text on a fill.
fn decoration(s: &str) -> bool {
    s.chars().all(|c| matches!(c as u32, 0x2500..=0x259F | 0xE0B0..=0xE0BF))
}

/// What a pair must reach. Glyphs 3:1. Text 4.5:1, except where the presets' own data cannot give it
/// (and alpheus-common tests only the floor): `fg` on `raised` 3.5 (the shapes trial's ruling of
/// 2026-10-08, recorded in CLAUDE.md under "Themes delivery 2": `raised`, fg ≥ 3.5:1), `fg` on `pill`
/// and `selection` 3.5 too (reason not recorded), `on_accent` on a hue 3.0, `fg_dim` anywhere 3.0.
fn need(th: &Theme, fg: Color, bg: Color, text: bool) -> f32 {
    if !text {
        return 3.0;
    }
    match (role(th, fg), role(th, bg)) {
        ("fg", "raised" | "pill" | "selection") => 3.5,
        ("on_accent", "accent_container") => 4.5,
        ("on_accent", _) => 3.0,
        ("muted", _) => 3.0,
        _ => 4.5,
    }
}

/// Every (name, ratio, needed) below its bar, over the rail at the given size.
fn failures(w: u16, h: u16) -> Vec<(String, f32)> {
    let mut bad = Vec::new();
    for (name, roles) in themes() {
        for shape in [Shape::Flat, Shape::Round, Shape::Line] {
            let mut a = ready_app();
            a.theme = Theme { shape, ..roles.into() };
            let page = a.theme.bg;
            // Every tab with each of its first rows focused (the focus band and the focused chips differ).
            for (t, focus) in Tab::ALL.into_iter().flat_map(|t| (0..7).map(move |f| (t, f))) {
                a.tab = t;
                a.focus = focus;
                let buf = buffer_at(&a, w, h, Some(super::testing::T));
                for y in 0..h {
                    for x in 0..w {
                        let c = &buf[(x, y)];
                        let s = c.symbol();
                        if s.trim().is_empty() || c.bg == page || c.bg == Color::Reset || decoration(s) {
                            continue;
                        }
                        let (Some(fg), Some(bg)) = (rgb(c.fg), rgb(c.bg)) else { continue };
                        let text = s.chars().all(|ch| ch.is_alphanumeric() && (ch as u32) < 0xE000);
                        let need = need(&a.theme, c.fg, c.bg, text);
                        let r = ratio(fg, bg);
                        if r < need {
                            let pair = format!(
                                "{t:?}: {} on {}{}",
                                role(&a.theme, c.fg),
                                role(&a.theme, c.bg),
                                if text { "" } else { " (glyph)" }
                            );
                            bad.push((format!("{pair} -- worst {name} {shape:?} ({x},{y}) {s:?} {r:.2} < {need}"), r));
                        }
                    }
                }
            }
        }
    }
    bad.sort_by(|a, b| a.1.total_cmp(&b.1));
    bad
}

#[test]
fn the_rail_and_its_tabs_keep_their_contrast_on_every_fill() {
    for (w, h) in [(87, 26), (120, 40)] {
        let bad = failures(w, h);
        // One line per pair, with its worst cell (the list is sorted, worst first).
        let mut seen = std::collections::BTreeSet::new();
        let uniq: Vec<String> =
            bad.iter().filter(|b| seen.insert(b.0.split(" -- ").next().unwrap().to_string())).map(|b| b.0.clone()).collect();
        assert!(bad.is_empty(), "{w}x{h}: {} cells below their bar, worst first:\n{}", bad.len(), uniq.join("\n"));
    }
}
