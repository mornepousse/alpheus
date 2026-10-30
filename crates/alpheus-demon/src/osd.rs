//! The on-screen display's model (OSD spec), pure. It decides when a sound change shows the pill:
//! the baseline after pw-dump (re)starts, a 1 s grace after startup or a default device appearing,
//! a level that must move, the microphone on a mute flip only, a default sink switched (its card
//! kept up to date while it shows). It also builds the pill's one row of cells, reads the backlight's level from its two
//! sysfs texts, and keeps the pill's deadline. The surface is `osd_surface.rs`'s; the backlight
//! files are `backlight.rs`'s. Nothing here talks to Wayland, reads a file, starts a thread or runs
//! a command.

use crate::config::OsdConfig;
use crate::grid::{Grid, Style, Tint};
use crate::list::{self, Boxed};
use crate::media::truncate_title;
use crate::segments::{VOLUME_HIGH, VOLUME_LOW, VOLUME_MEDIUM, VOLUME_OFF};
use crate::system::{DeviceName, SoundState, Volume};
use std::time::{Duration, Instant};

/// After startup, a pw-dump restart, or a default device appearing or switching: no pill (spec § 2).
pub const GRACE: Duration = Duration::from_secs(1);
/// Cells the percentage takes: "100%", " 64%", "255%".
pub const PERCENT_CELLS: u16 = 4;
/// The longest sysfs number read (a u32 has 10 digits).
pub const LEVEL_DIGITS: usize = 10;
// Material Design glyphs of the Nerd Font, checked by name in the post table of
// JetBrainsMonoNerdFontMono-Regular.ttf (3.4.0).
pub const MIC_ON: char = '\u{f036c}'; // md-microphone
pub const MIC_OFF: char = '\u{f036d}'; // md-microphone_off
pub const BRIGHTNESS: char = '\u{f00e0}'; // md-brightness_7 (a sun)

/// What the pill says (spec § 1). One at a time: a newer one replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pill {
    /// The default sink's level.
    Volume(Volume),
    /// The default sink switched: its name in place of the bar, and its level.
    Sink { label: String, volume: Volume },
    /// The default source's mute flipped (its level shown, never a reason to show).
    Mic(Volume),
    /// The backlight, 0 to 100 %.
    Brightness(u8),
}

/// What a sound message means for the OSD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// The first state after pw-dump (re)started: kept, never shown.
    Baseline,
    Quiet,
    Show(Pill),
}

/// The trigger rules' memory: the last sound state, each stream's grace, and the sink switched to
/// whose volume is not known yet (its card waits for it, inside that switch's grace).
#[derive(Debug, Clone, Default)]
pub struct Watch {
    last: Option<SoundState>,
    sink_grace: Option<Instant>,
    source_grace: Option<Instant>,
    card_pending: Option<String>,
}

/// One stream's change.
enum Change {
    Nothing,
    Level(Volume),
    /// A switch (or a level while its card shows): the name card.
    Card(Volume),
}

/// A change worth a sink pill: the mute flipped, or the level moved. Percents are integers (as
/// wpctl and the bar show them), so a level that moved less than half a point never gets here.
pub fn moved(a: Volume, b: Volume) -> bool {
    a.muted != b.muted || a.percent != b.percent
}

/// A change worth a microphone pill: its mute flipped (Mae, 2026-10-09). Never its level: WebRTC
/// apps move the capture gain during a call.
pub fn flipped(a: Volume, b: Volume) -> bool {
    a.muted != b.muted
}

/// One stream (the sink or the source), from (`name`, `volume`) before to after. `worth`: what
/// counts as a change; `card`: a name card is on screen (a change inside the grace updates it);
/// `pending`: the node whose card waits for its volume (None for the source: it has no card).
fn side(
    old: (Option<&DeviceName>, Option<Volume>),
    new: (Option<&DeviceName>, Option<Volume>),
    worth: fn(Volume, Volume) -> bool,
    card: bool,
    grace: &mut Option<Instant>,
    mut pending: Option<&mut Option<String>>,
    now: Instant,
) -> Change {
    let inside = grace.is_some_and(|g| now < g);
    let quiet = inside && !card;
    match (old.0, new.0) {
        (Some(a), Some(b)) if a.node != b.node => {
            *grace = Some(now + GRACE);
            if let Some(p) = pending.as_deref_mut() {
                *p = None;
            }
            match new.1 {
                Some(v) if !quiet => Change::Card(v),
                // Not known yet: the card waits for it, unless an earlier grace keeps quiet.
                None if !quiet => {
                    if let Some(p) = pending {
                        *p = Some(b.node.clone());
                    }
                    Change::Nothing
                }
                _ => Change::Nothing,
            }
        }
        (Some(_), Some(b)) => {
            let waited = pending.as_deref_mut().and_then(|p| p.take_if(|n| *n == b.node));
            match (old.1, new.1) {
                (None, Some(v)) if waited.is_some() && inside => Change::Card(v),
                (None, None) => {
                    // Still waiting for it.
                    if let (Some(p), Some(n)) = (pending, waited) {
                        *p = Some(n);
                    }
                    Change::Nothing
                }
                (Some(a), Some(v)) if worth(a, v) && !inside => Change::Level(v),
                (Some(a), Some(v)) if worth(a, v) && card => Change::Card(v),
                // Inside the grace with no card shown, unmoved, or a volume not known before
                // (known again: the new level is the baseline) or not known now: no pill.
                _ => Change::Nothing,
            }
        }
        (None, None) => Change::Nothing,
        // A default appeared or went: its levels settle (WirePlumber restores them) in silence.
        _ => {
            *grace = Some(now + GRACE);
            if let Some(p) = pending {
                *p = None;
            }
            Change::Nothing
        }
    }
}

impl Watch {
    /// A sound message (`None`: the source went down), at `now`. `card`: the sink's name card is
    /// on screen (`Osd::sound` knows).
    pub fn sound(&mut self, s: Option<&SoundState>, now: Instant, card: bool) -> Heard {
        let Some(new) = s else {
            self.last = None;
            self.card_pending = None;
            return Heard::Quiet;
        };
        let Some(old) = self.last.replace(new.clone()) else {
            self.sink_grace = Some(now + GRACE);
            self.source_grace = Some(now + GRACE);
            self.card_pending = None;
            return Heard::Baseline;
        };
        let sink = side(
            (old.sink_name.as_ref(), old.sink),
            (new.sink_name.as_ref(), new.sink),
            moved,
            card,
            &mut self.sink_grace,
            Some(&mut self.card_pending),
            now,
        );
        let source = side(
            (old.source_name.as_ref(), old.source),
            (new.source_name.as_ref(), new.source),
            flipped,
            false,
            &mut self.source_grace,
            None,
            now,
        );
        match (sink, source) {
            (Change::Level(v), _) => Heard::Show(Pill::Volume(v)),
            (Change::Card(v), _) => {
                Heard::Show(Pill::Sink { label: new.sink_name.as_ref().map(|n| n.label.clone()).unwrap_or_default(), volume: v })
            }
            (Change::Nothing, Change::Level(v)) => Heard::Show(Pill::Mic(v)),
            // The source has no name card (spec § 1): its switch shows nothing.
            (Change::Nothing, Change::Card(_) | Change::Nothing) => Heard::Quiet,
        }
    }
}

/// The OSD's state: its trigger memory, and the pill shown with its deadline (None at rest).
#[derive(Debug, Clone, Default)]
pub struct Osd {
    pub watch: Watch,
    shown: Option<(Pill, Instant)>,
}

impl Osd {
    /// A sound message through the trigger rules, which need to know whether the sink's name card
    /// is on screen (a change inside its grace then updates it).
    pub fn sound(&mut self, s: Option<&SoundState>, now: Instant) -> Heard {
        let card = matches!(&self.shown, Some((Pill::Sink { .. }, until)) if now < *until);
        self.watch.sound(s, now, card)
    }

    /// Shows `pill` (replacing any other) until `now + timeout`.
    pub fn show(&mut self, pill: Pill, now: Instant, timeout: Duration) {
        self.shown = Some((pill, now + timeout));
    }

    pub fn shown(&self) -> Option<&Pill> {
        self.shown.as_ref().map(|(p, _)| p)
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.shown.as_ref().map(|(_, t)| *t)
    }

    /// Hides the pill when its deadline is reached; true when it did.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|t| t <= now) {
            self.shown = None;
            true
        } else {
            false
        }
    }

    pub fn hide(&mut self) {
        self.shown = None;
    }
}

/// How long the pill stays after the last change (`[osd] timeout_ms`).
pub fn timeout(cfg: &OsdConfig) -> Duration {
    Duration::from_millis(u64::from(cfg.timeout_ms))
}

/// The volume's glyph, as the bar's sound module picks it.
pub fn volume_glyph(v: Volume) -> char {
    if v.muted {
        VOLUME_OFF
    } else if v.percent < 34 {
        VOLUME_LOW
    } else if v.percent < 67 {
        VOLUME_MEDIUM
    } else {
        VOLUME_HIGH
    }
}

fn glyph_style(v: Volume) -> Style {
    if v.muted { Style::MUTED } else { Style::ACCENT }
}

/// The percentage, right-aligned in the last `PERCENT_CELLS` before `end`; on the selection band
/// when `selected` (the quick panel's slider rows).
pub(crate) fn percent(g: &mut Grid, end: u16, v: Volume, selected: bool) {
    let style = match (selected, v.muted) {
        (true, _) => Style::SELECTED,
        (false, true) => Style::MUTED,
        (false, false) => Style::NORMAL,
    };
    g.write(end.saturating_sub(PERCENT_CELLS), end, &format!("{:>3}%", v.percent), style);
}

/// "G ━━━━━━━━━━━━━━━━─────────  64%" from `at` to `end` (excluded): the glyph, a space, the bar,
/// one blank cell, the percentage. Filled in the accent, the rest dim; muted, all dim; above
/// 100 %, the whole bar in `warn`. `selected` (the quick panel's slider rows): the whole of it on the
/// selection band, the bar cells keeping their own foreground (accent, dim, warn).
pub(crate) fn level(g: &mut Grid, at: u16, end: u16, glyph: char, v: Volume, selected: bool) {
    let pick = |s: Style| if selected { Style::SELECTED } else { s };
    let bar = end.saturating_sub(at).saturating_sub(3 + PERCENT_CELLS);
    let x = g.write(at, end, &glyph.to_string(), pick(glyph_style(v)));
    let x = g.write(x, end, " ", pick(Style::NORMAL));
    let over = v.percent > 100;
    // percent ≤ 100 here, so filled ≤ bar.
    let filled = if over { bar } else { (u32::from(v.percent) * u32::from(bar) / 100) as u16 };
    let (on, off) = match (v.muted, over) {
        (true, _) => (Style::MUTED, Style::MUTED),
        (false, true) => (Style::WARN, Style::WARN),
        (false, false) => (Style::ACCENT, Style::MUTED),
    };
    let band = |s: Style| if selected { Style { bg: Tint::Selection, ..s } } else { s };
    let (on, off) = (band(on), band(off));
    let x = g.write(x, end, &"━".repeat(usize::from(filled)), on);
    g.write(x, end, &"─".repeat(usize::from(bar.saturating_sub(filled))), off);
    percent(g, end, v, selected);
}

/// The pill's one row: `width` cells of content, padded like the popups' (`list::padded`).
pub fn rows(pill: &Pill, width: u16) -> Boxed {
    let mut g = Grid::empty(width);
    // Glyph, two spaces, the percentage.
    let bar = width.saturating_sub(3 + PERCENT_CELLS);
    match pill {
        Pill::Volume(v) => level(&mut g, 0, width, volume_glyph(*v), *v, false),
        Pill::Mic(v) => level(&mut g, 0, width, if v.muted { MIC_OFF } else { MIC_ON }, *v, false),
        Pill::Brightness(p) => level(&mut g, 0, width, BRIGHTNESS, Volume { percent: *p, muted: false }, false),
        Pill::Sink { label, volume } => {
            let x = g.write(0, width, &volume_glyph(*volume).to_string(), glyph_style(*volume));
            let x = g.write(x, width, " ", Style::NORMAL);
            g.write(x, x.saturating_add(bar), &truncate_title(label, bar), Style::NORMAL);
            percent(&mut g, width, *volume, false);
        }
    }
    list::padded(&[g])
}

/// The pill in a log line; no device name (a label stays out of the journal).
pub fn describe(pill: &Pill) -> String {
    let muted = |v: &Volume| if v.muted { " muted" } else { "" };
    match pill {
        Pill::Volume(v) => format!("volume {}%{}", v.percent, muted(v)),
        Pill::Sink { volume, .. } => format!("default sink changed, {}%{}", volume.percent, muted(volume)),
        Pill::Mic(v) => format!("mic {}%{}", v.percent, muted(v)),
        Pill::Brightness(p) => format!("brightness {p}%"),
    }
}

/// A sysfs number: trimmed, 1 to `LEVEL_DIGITS` ASCII digits, no sign.
pub fn parse_level(text: &str) -> Option<u32> {
    let t = text.trim();
    if t.is_empty() || t.len() > LEVEL_DIGITS || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// The backlight's level in percent from its `brightness` and `max_brightness` texts; None when
/// either is not a small number, or the maximum is 0.
pub fn brightness_percent(brightness: &str, max: &str) -> Option<u8> {
    let (b, m) = (parse_level(brightness)?, parse_level(max)?);
    if m == 0 {
        return None;
    }
    let p = (u64::from(b.min(m)) * 100 + u64::from(m) / 2) / u64::from(m);
    u8::try_from(p).ok()
}

#[cfg(test)]
mod tests;
