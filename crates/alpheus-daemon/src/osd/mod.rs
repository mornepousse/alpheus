//! The on-screen display's model: when a change shows the pill, and what the pill says.
//!
//! The pill is the small bar that shows a volume, microphone or brightness level, or a passive
//! kind's state (Bluetooth, do-not-disturb, the power profile), for a moment. This file is its pure
//! half. `Watch` decides when a sound change shows it: the baseline after pw-dump (re)starts, a 1 s
//! grace after startup or after a default device appears, a level that must move, the microphone on
//! a mute flip only, a default sink that was switched (its name card kept up to date while it
//! shows). `Passive` does the same for one passive value (baseline, grace, change), and `Kinds`
//! holds one per kind with the mapping from each source's message. `Osd` keeps the pill that is
//! shown and its deadline. `rows` builds the pill's one row of cells, and `brightness_percent` turns
//! the backlight's two sysfs texts into a level. The Wayland surface is `surface.rs`'s; the files of the backlight are read by
//! `backlight.rs`.
//!
//! Never:
//! - talk to Wayland, read a file, start a thread or run a command: texts and times are handed in,
//!   so a test can pass any of them;
//! - read the clock: every method takes `now`;
//! - panic (a guardrail checks).
//!
//! See: docs/features/osd.md and docs/decisions.md
//! ("The microphone pill shows only when its mute flips", "A sound change within a second of a
//! start or a new default device shows nothing", "The OSD counts a volume change in whole
//! percent", "Brightness is shown on request, read once", "An OSD kind is opt-in and brings the
//! source it reads").

use crate::config::OsdConfig;
use crate::grid::{Grid, Style, Tint, text_width};
use crate::list::{self, Boxed};
use crate::media::truncate_title;
use crate::notify::NotifyView;
use crate::quick::PROFILE;
use crate::segments::{BELL, BELL_OFF, BLUETOOTH, VOLUME_HIGH, VOLUME_LOW, VOLUME_MEDIUM, VOLUME_OFF};
use crate::system::{BtState, DeviceName, Profile, ProfileState, SoundState, Volume};
use std::time::{Duration, Instant};

/// After startup, a pw-dump restart, or a default device appearing or switching: no pill. See docs/decisions.md, "A sound change within a second of a start or a new default
/// device shows nothing".
pub const GRACE: Duration = Duration::from_secs(1);
/// Cells the percentage takes: "100%", " 64%", "255%".
pub const PERCENT_CELLS: u16 = 4;
/// The longest sysfs number read (a u32 has 10 digits).
pub const LEVEL_DIGITS: usize = 10;
// Material Design glyphs of the Nerd Font, checked by name in the post table of
// JetBrainsMonoNerdFontMono-Regular.ttf (3.4.0). `\u{f036c}` is the character with that code.
pub const MIC_ON: char = '\u{f036c}'; // md-microphone
pub const MIC_OFF: char = '\u{f036d}'; // md-microphone_off
pub const BRIGHTNESS: char = '\u{f00e0}'; // md-brightness_7 (a sun)

/// What the pill says. One at a time: a newer one replaces it.
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
    /// The Bluetooth adapter powered on (true) or off.
    Bluetooth(bool),
    /// Do-not-disturb turned on (true) or off.
    Dnd(bool),
    /// The active power profile changed to this one.
    Profile(Profile),
}

/// What a sound message means for the OSD: nothing to show, the baseline, or a pill to show.
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

/// One stream's change (a stream is the default sink or the default source).
enum Change {
    Nothing,
    Level(Volume),
    /// A switch (or a level while its card shows): the name card.
    Card(Volume),
}

/// A change worth a sink pill: the mute flipped, or the level moved. Percents are integers (as
/// wpctl and the bar show them), so a level that moved less than half a point never gets here. See
/// docs/decisions.md, "The OSD counts a volume change in whole percent".
pub fn moved(a: Volume, b: Volume) -> bool {
    a.muted != b.muted || a.percent != b.percent
}

/// A change worth a microphone pill: its mute flipped. Never its level: WebRTC apps move the capture
/// gain during a call. See docs/decisions.md, "The microphone pill shows only when its mute flips".
pub fn flipped(a: Volume, b: Volume) -> bool {
    a.muted != b.muted
}

/// One stream (the sink or the source), from (`name`, `volume`) before to after. `worth`: what
/// counts as a change; `card`: a name card is on screen (a change inside the grace updates it);
/// `pending`: the node whose card waits for its volume (None for the source: it has no card).
/// `grace` is the stream's grace end: it is set here when a default appears, goes or switches.
fn side(
    old: (Option<&DeviceName>, Option<Volume>),
    new: (Option<&DeviceName>, Option<Volume>),
    worth: fn(Volume, Volume) -> bool,
    card: bool,
    grace: &mut Option<Instant>,
    mut pending: Option<&mut Option<String>>,
    now: Instant,
) -> Change {
    // Inside the grace: before its end (at exactly its end it is over).
    let inside = match *grace {
        Some(end) => now < end,
        None => false,
    };
    // A grace keeps quiet unless the name card is on screen (then a change updates the card).
    let quiet = inside && !card;
    match (old.0, new.0) {
        // The default is another device now. (`pending` is an `Option` holding a reference that
        // can change the value; `as_deref_mut` borrows it again, so it can still be used below.)
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
        // The same default as before. `waited` is the node name taken out of `pending` if this very
        // node was the one whose card waited for its volume.
        (Some(_), Some(b)) => {
            let mut waited = None;
            if let Some(p) = pending.as_deref_mut()
                && p.as_deref() == Some(b.node.as_str())
            {
                waited = p.take();
            }
            match (old.1, new.1) {
                (None, Some(v)) if waited.is_some() && inside => Change::Card(v),
                (None, None) => {
                    // Still waiting for it: put the name back.
                    if let Some(p) = pending
                        && let Some(n) = waited
                    {
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
        // `replace` stores the new state and gives back the one that was there: None for the first
        // state after a start or a restart, which becomes the baseline.
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
        // The sink comes first: a message that changes both the sink and the microphone shows the
        // sink.
        match (sink, source) {
            (Change::Level(v), _) => Heard::Show(Pill::Volume(v)),
            (Change::Card(v), _) => {
                let label = match new.sink_name.as_ref() {
                    Some(name) => name.label.clone(),
                    None => String::new(),
                };
                Heard::Show(Pill::Sink { label, volume: v })
            }
            (Change::Nothing, Change::Level(v)) => Heard::Show(Pill::Mic(v)),
            // The source has no name card: its switch shows nothing.
            (Change::Nothing, Change::Card(_) | Change::Nothing) => Heard::Quiet,
        }
    }
}

/// One passive value's trigger memory: the first value after its source (re)starts is the baseline,
/// a `GRACE` follows it, then a change of value shows. The same rules as `Watch::sound`'s, for a
/// value that has no level to move. See docs/decisions.md, "An OSD kind is opt-in and brings the
/// source it reads".
#[derive(Debug, Clone)]
pub struct Passive<T> {
    last: Option<T>,
    grace: Option<Instant>,
}

impl<T> Default for Passive<T> {
    fn default() -> Self {
        Passive { last: None, grace: None }
    }
}

impl<T: PartialEq + Clone> Passive<T> {
    /// A value at `now` (None: the source went down, or has nothing to say); Some: show it.
    pub fn hear(&mut self, value: Option<T>, now: Instant) -> Option<T> {
        let Some(new) = value else {
            self.last = None;
            self.grace = None;
            return None;
        };
        // `replace` keeps the new value and gives back the old one: None for the baseline.
        match self.last.replace(new.clone()) {
            None => {
                self.grace = Some(now + GRACE);
                None
            }
            Some(old) if old == new => None,
            // Inside the grace: before its end (at exactly its end it is over).
            Some(_) if self.grace.is_some_and(|end| now < end) => None,
            Some(_) => Some(new),
        }
    }
}

/// The passive kinds' trigger memories, one per kind, and what each source's message means.
#[derive(Debug, Clone, Default)]
pub struct Kinds {
    pub bluetooth: Passive<bool>,
    pub dnd: Passive<bool>,
    pub profile: Passive<Profile>,
}

impl Kinds {
    /// BlueZ's state: a power flip shows, never a device connecting or leaving (only the power is
    /// compared). No adapter, or the bus down: a reset.
    pub fn bluetooth(&mut self, s: Option<&BtState>, now: Instant) -> Option<Pill> {
        let powered = match s {
            Some(BtState::On { .. }) => Some(true),
            Some(BtState::Off) => Some(false),
            Some(BtState::Absent) | None => None,
        };
        self.bluetooth.hear(powered, now).map(Pill::Bluetooth)
    }

    /// The bell's view: do-not-disturb, only while alpheus holds the notification name (otherwise
    /// another server decides, and the flag shows nothing real).
    pub fn dnd(&mut self, view: NotifyView, now: Instant) -> Option<Pill> {
        let dnd = if view.serving { Some(view.dnd) } else { None };
        self.dnd.hear(dnd, now).map(Pill::Dnd)
    }

    /// power-profiles-daemon's active profile; none active, or the bus down: a reset.
    pub fn profile(&mut self, s: Option<&ProfileState>, now: Instant) -> Option<Pill> {
        self.profile.hear(s.and_then(|s| s.active), now).map(Pill::Profile)
    }
}

/// The OSD's state: its trigger memory, and the pill shown with its deadline (None at rest).
#[derive(Debug, Clone, Default)]
pub struct Osd {
    pub watch: Watch,
    pub kinds: Kinds,
    shown: Option<(Pill, Instant)>,
}

impl Osd {
    /// A sound message through the trigger rules, which need to know whether the sink's name card
    /// is on screen (a change inside its grace then updates it).
    pub fn sound(&mut self, s: Option<&SoundState>, now: Instant) -> Heard {
        // The card is on screen while the shown pill is a sink card whose deadline is not reached.
        let card = matches!(&self.shown, Some((Pill::Sink { .. }, until)) if now < *until);
        self.watch.sound(s, now, card)
    }

    /// Shows `pill` (replacing any other) until `now + timeout`.
    pub fn show(&mut self, pill: Pill, now: Instant, timeout: Duration) {
        self.shown = Some((pill, now + timeout));
    }

    /// The pill that is shown now, if any.
    pub fn shown(&self) -> Option<&Pill> {
        // The pattern `|(pill, _)|` takes the pill out of the pair (pill, deadline).
        self.shown.as_ref().map(|(pill, _)| pill)
    }

    /// When the pill that is shown must go; None at rest.
    pub fn deadline(&self) -> Option<Instant> {
        self.shown.as_ref().map(|(_, until)| *until)
    }

    /// Hides the pill when its deadline is reached; true when it did.
    pub fn expire(&mut self, now: Instant) -> bool {
        match self.deadline() {
            Some(until) if until <= now => {
                self.shown = None;
                true
            }
            _ => false,
        }
    }

    /// Hides the pill whatever its deadline (its surface went away).
    pub fn hide(&mut self) {
        self.shown = None;
    }
}

/// How long the pill stays after the last change (`[osd] timeout_ms`).
pub fn timeout(cfg: &OsdConfig) -> Duration {
    Duration::from_millis(u64::from(cfg.timeout_ms))
}

/// The volume's glyph, as the bar's sound module picks it: muted, then low below 34 %, medium
/// below 67 %, high from there.
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

/// The glyph's style: accent, or dim when muted.
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
    // Above 100 % the whole bar is filled and the real number is still shown.
    // Otherwise percent ≤ 100, so filled ≤ bar.
    let filled = if over { bar } else { (u32::from(v.percent) * u32::from(bar) / 100) as u16 };
    let (on, off) = match (v.muted, over) {
        (true, _) => (Style::MUTED, Style::MUTED),
        (false, true) => (Style::WARN, Style::WARN),
        (false, false) => (Style::ACCENT, Style::MUTED),
    };
    // On the selection band each cell keeps its own colour for the text and takes the band's
    // background.
    let band = |s: Style| if selected { Style { bg: Tint::Selection, ..s } } else { s };
    let (on, off) = (band(on), band(off));
    let x = g.write(x, end, &"━".repeat(usize::from(filled)), on);
    g.write(x, end, &"─".repeat(usize::from(bar.saturating_sub(filled))), off);
    percent(g, end, v, selected);
}

/// A text pill's row in `g`: the glyph in `style`, a space, the label, and the `right` word
/// right-aligned at the end, where the percentage sits on a level pill. The label gives way (cut
/// with an ellipsis) so that one blank cell stays before the word.
pub(crate) fn text_row(g: &mut Grid, glyph: char, style: Style, label: &str, right: &str) {
    let end = g.width();
    let w = text_width(right);
    let x = g.write(0, end, &glyph.to_string(), style);
    let x = g.write(x, end, " ", Style::NORMAL);
    let room = end.saturating_sub(x).saturating_sub(w.saturating_add(1));
    g.write(x, x.saturating_add(room), &truncate_title(label, room), Style::NORMAL);
    g.write(end.saturating_sub(w), end, right, Style::NORMAL);
}

/// "on" or "off", and the glyph's style: accent, or dim when off.
fn switch(on: bool) -> (&'static str, Style) {
    if on { ("on", Style::ACCENT) } else { ("off", Style::MUTED) }
}

/// The pill's one row: `width` cells of content, padded like the popups' (`list::padded`).
pub fn rows(pill: &Pill, width: u16) -> Boxed {
    let mut g = Grid::empty(width);
    // The cells left for the bar or for the sink's name: the glyph and its space, one blank cell
    // before the percentage (3 in all), then the percentage.
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
        Pill::Bluetooth(on) => {
            let (word, style) = switch(*on);
            text_row(&mut g, BLUETOOTH, style, "Bluetooth", word);
        }
        Pill::Dnd(on) => {
            let (word, style) = switch(*on);
            text_row(&mut g, if *on { BELL_OFF } else { BELL }, style, "Do not disturb", word);
        }
        Pill::Profile(p) => text_row(&mut g, PROFILE, Style::ACCENT, "Power profile", p.label()),
    }
    list::padded(&[g])
}

/// The pill in a log line: fixed words and numbers, never a device name nor a profile (a label stays
/// out of the journal; a guardrail reads these arms).
pub fn describe(pill: &Pill) -> String {
    let muted = |v: &Volume| if v.muted { " muted" } else { "" };
    match pill {
        Pill::Volume(v) => format!("volume {}%{}", v.percent, muted(v)),
        Pill::Sink { volume, .. } => format!("default sink changed, {}%{}", volume.percent, muted(volume)),
        Pill::Mic(v) => format!("mic {}%{}", v.percent, muted(v)),
        Pill::Brightness(p) => format!("brightness {p}%"),
        Pill::Bluetooth(true) => "bluetooth on".into(),
        Pill::Bluetooth(false) => "bluetooth off".into(),
        Pill::Dnd(true) => "dnd on".into(),
        Pill::Dnd(false) => "dnd off".into(),
        Pill::Profile(_) => "profile changed".into(),
    }
}

/// A sysfs number: trimmed, 1 to `LEVEL_DIGITS` ASCII digits, no sign. (Ten digits can still be
/// larger than a `u32`; that gives None too.)
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
    // `?` after a call that gives None ends the function with None: a text that is not a number.
    let (b, m) = (parse_level(brightness)?, parse_level(max)?);
    if m == 0 {
        return None;
    }
    // A level above the maximum counts as the maximum. Adding half of `m` before dividing rounds
    // to the nearest percent instead of down. The result is at most 100, so it always fits a `u8`.
    let p = (u64::from(b.min(m)) * 100 + u64::from(m) / 2) / u64::from(m);
    u8::try_from(p).ok()
}

/// The pill's Wayland surface and its timer.
pub mod surface;

#[cfg(test)]
mod tests;
