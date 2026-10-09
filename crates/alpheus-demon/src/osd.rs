//! The on-screen display's model (OSD spec), pure. It decides when a sound change shows the pill:
//! the baseline after pw-dump (re)starts, a 1 s grace after startup or a default device appearing,
//! a level that must move, the microphone kept quiet 500 ms after a sink pill, a default sink
//! switched. It also builds the pill's one row of cells, reads the backlight's level from its two
//! sysfs texts, and keeps the pill's deadline. The surface is `osd_surface.rs`'s; the backlight
//! files are `backlight.rs`'s. Nothing here talks to Wayland, reads a file, starts a thread or runs
//! a command.

use crate::config::OsdConfig;
use crate::grid::{Grid, Style};
use crate::list::{self, Boxed};
use crate::media::truncate_title;
use crate::segments::{VOLUME_HIGH, VOLUME_LOW, VOLUME_MEDIUM, VOLUME_OFF};
use crate::system::{DeviceName, SoundState, Volume};
use std::time::{Duration, Instant};

/// After startup, a pw-dump restart, or a default device appearing or switching: no pill (spec § 2).
pub const GRACE: Duration = Duration::from_secs(1);
/// A microphone change this soon after a sink pill shows nothing (spec § 2).
pub const MIC_AFTER_SINK: Duration = Duration::from_millis(500);
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
    /// The default source's level and mute.
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

/// The trigger rules' memory: the last sound state, each stream's grace, the last sink pill.
#[derive(Debug, Clone, Default)]
pub struct Watch {
    last: Option<SoundState>,
    sink_grace: Option<Instant>,
    source_grace: Option<Instant>,
    sink_shown: Option<Instant>,
}

/// One stream's change, before the microphone rule.
enum Change {
    Nothing,
    Level(Volume),
    Switched(Volume),
}

/// A change worth a pill: the mute flipped, or the level moved. Percents are integers (as wpctl
/// and the bar show them), so a level that moved less than half a point never gets here.
pub fn moved(a: Volume, b: Volume) -> bool {
    a.muted != b.muted || a.percent != b.percent
}

/// One stream (the sink or the source), from (`name`, `volume`) before to after.
fn side(
    old: (Option<&DeviceName>, Option<Volume>),
    new: (Option<&DeviceName>, Option<Volume>),
    grace: &mut Option<Instant>,
    now: Instant,
) -> Change {
    let quiet = grace.is_some_and(|g| now < g);
    match (old.0, new.0) {
        (Some(a), Some(b)) if a.node != b.node => {
            *grace = Some(now + GRACE);
            match new.1 {
                Some(v) if !quiet => Change::Switched(v),
                _ => Change::Nothing,
            }
        }
        (Some(_), Some(_)) => match (old.1, new.1) {
            (Some(a), Some(b)) if !quiet && moved(a, b) => Change::Level(b),
            // Inside the grace, unmoved, or a volume not known before (known again: the new
            // level is the baseline) or not known now: no pill.
            _ => Change::Nothing,
        },
        (None, None) => Change::Nothing,
        // A default appeared or went: its levels settle (WirePlumber restores them) in silence.
        _ => {
            *grace = Some(now + GRACE);
            Change::Nothing
        }
    }
}

impl Watch {
    /// A sound message (`None`: the source went down), at `now`.
    pub fn sound(&mut self, s: Option<&SoundState>, now: Instant) -> Heard {
        let Some(new) = s else {
            self.last = None;
            return Heard::Quiet;
        };
        let Some(old) = self.last.replace(new.clone()) else {
            self.sink_grace = Some(now + GRACE);
            self.source_grace = Some(now + GRACE);
            return Heard::Baseline;
        };
        let sink = side((old.sink_name.as_ref(), old.sink), (new.sink_name.as_ref(), new.sink), &mut self.sink_grace, now);
        let source = side((old.source_name.as_ref(), old.source), (new.source_name.as_ref(), new.source), &mut self.source_grace, now);
        let pill = match sink {
            Change::Level(v) => Some(Pill::Volume(v)),
            Change::Switched(v) => {
                Some(Pill::Sink { label: new.sink_name.as_ref().map(|n| n.label.clone()).unwrap_or_default(), volume: v })
            }
            Change::Nothing => None,
        };
        if let Some(p) = pill {
            self.sink_shown = Some(now);
            return Heard::Show(p);
        }
        match source {
            Change::Level(v) if !self.sink_shown.is_some_and(|t| now < t + MIC_AFTER_SINK) => Heard::Show(Pill::Mic(v)),
            _ => Heard::Quiet,
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

/// The percentage, right-aligned in the row's last `PERCENT_CELLS`.
fn percent(g: &mut Grid, v: Volume) {
    let w = g.width();
    let style = if v.muted { Style::MUTED } else { Style::NORMAL };
    g.write(w.saturating_sub(PERCENT_CELLS), w, &format!("{:>3}%", v.percent), style);
}

/// "G ━━━━━━━━━━━━━━━━─────────  64%": filled in the accent, the rest dim; muted, all dim; above
/// 100 %, the whole bar in `warn`.
fn level(g: &mut Grid, glyph: char, v: Volume, bar: u16) {
    let w = g.width();
    let x = g.write(0, w, &glyph.to_string(), glyph_style(v));
    let x = g.write(x, w, " ", Style::NORMAL);
    let over = v.percent > 100;
    // percent ≤ 100 here, so filled ≤ bar.
    let filled = if over { bar } else { (u32::from(v.percent) * u32::from(bar) / 100) as u16 };
    let (on, off) = match (v.muted, over) {
        (true, _) => (Style::MUTED, Style::MUTED),
        (false, true) => (Style::WARN, Style::WARN),
        (false, false) => (Style::ACCENT, Style::MUTED),
    };
    let x = g.write(x, w, &"━".repeat(usize::from(filled)), on);
    g.write(x, w, &"─".repeat(usize::from(bar.saturating_sub(filled))), off);
    percent(g, v);
}

/// The pill's one row: `width` cells of content, padded like the popups' (`list::padded`).
pub fn rows(pill: &Pill, width: u16) -> Boxed {
    let mut g = Grid::empty(width);
    // Glyph, two spaces, the percentage.
    let bar = width.saturating_sub(3 + PERCENT_CELLS);
    match pill {
        Pill::Volume(v) => level(&mut g, volume_glyph(*v), *v, bar),
        Pill::Mic(v) => level(&mut g, if v.muted { MIC_OFF } else { MIC_ON }, *v, bar),
        Pill::Brightness(p) => level(&mut g, BRIGHTNESS, Volume { percent: *p, muted: false }, bar),
        Pill::Sink { label, volume } => {
            let x = g.write(0, width, &volume_glyph(*volume).to_string(), glyph_style(*volume));
            let x = g.write(x, width, " ", Style::NORMAL);
            g.write(x, x.saturating_add(bar), &truncate_title(label, bar), Style::NORMAL);
            percent(&mut g, *volume);
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
mod tests {
    use super::*;

    const OUT: &str = "alsa_output.desk";
    const IN: &str = "alsa_input.desk";

    fn vol(percent: u8, muted: bool) -> Volume {
        Volume { percent, muted }
    }

    fn dev(node: &str, label: &str) -> Option<DeviceName> {
        Some(DeviceName { node: node.into(), label: label.into() })
    }

    fn state(sink: u8, sink_muted: bool, mic: u8, mic_muted: bool) -> SoundState {
        SoundState {
            sink: Some(vol(sink, sink_muted)),
            source: Some(vol(mic, mic_muted)),
            sink_name: dev(OUT, "Desk speakers"),
            source_name: dev(IN, "Desk microphone"),
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A watch past its start-up grace, its last state `state(84, false, 100, false)`; and that time.
    fn settled() -> (Watch, Instant) {
        let t0 = Instant::now();
        let mut w = Watch::default();
        assert_eq!(w.sound(Some(&state(84, false, 100, false)), t0), Heard::Baseline);
        (w, t0 + Duration::from_secs(5))
    }

    fn texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text()).collect()
    }

    fn styles(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.styles()).collect()
    }

    #[test]
    fn the_first_state_is_the_baseline_and_a_restart_takes_a_new_one() {
        let (mut w, late) = settled();
        assert_eq!(w.sound(Some(&state(90, false, 100, false)), late), Heard::Show(Pill::Volume(vol(90, false))));
        assert_eq!(w.sound(None, late), Heard::Quiet, "pw-dump went down");
        assert_eq!(w.sound(Some(&state(50, false, 100, false)), late + ms(10)), Heard::Baseline, "its first dump after the restart");
        assert_eq!(w.sound(Some(&state(55, false, 100, false)), late + ms(500)), Heard::Quiet, "inside the new grace");
        assert_eq!(w.sound(Some(&state(60, false, 100, false)), late + ms(1010)), Heard::Show(Pill::Volume(vol(60, false))));
    }

    #[test]
    fn nothing_shows_within_the_grace_and_a_change_at_its_end_does() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        assert_eq!(w.sound(Some(&state(84, false, 100, false)), t0), Heard::Baseline);
        assert_eq!(w.sound(Some(&state(90, false, 100, true)), t0 + ms(999)), Heard::Quiet, "WirePlumber restoring levels at login");
        assert_eq!(w.sound(Some(&state(91, false, 100, true)), t0 + GRACE), Heard::Show(Pill::Volume(vol(91, false))), "at exactly 1 s");
    }

    #[test]
    fn only_a_moved_level_or_a_flipped_mute_shows() {
        assert!(moved(vol(64, false), vol(65, false)));
        assert!(moved(vol(64, false), vol(64, true)));
        assert!(!moved(vol(64, true), vol(64, true)));
        let (mut w, late) = settled();
        let relabelled = SoundState { sink_name: dev(OUT, "Speakers (renamed)"), ..state(84, false, 100, false) };
        assert_eq!(w.sound(Some(&relabelled), late), Heard::Quiet, "a label alone is no change");
        assert_eq!(w.sound(Some(&state(84, true, 100, false)), late + ms(1)), Heard::Show(Pill::Volume(vol(84, true))), "mute flipped");
        let unknown = SoundState { sink: None, ..state(84, true, 100, false) };
        assert_eq!(w.sound(Some(&unknown), late + ms(2)), Heard::Quiet, "a volume not known yet");
        assert_eq!(
            w.sound(Some(&state(50, false, 100, false)), late + ms(3)),
            Heard::Quiet,
            "known again, at another level: the new baseline, no pill"
        );
        assert_eq!(
            w.sound(Some(&state(51, false, 100, false)), late + ms(4)),
            Heard::Show(Pill::Volume(vol(51, false))),
            "then compared with it"
        );
    }

    #[test]
    fn a_default_sink_switch_shows_its_name_then_a_grace() {
        let (mut w, late) = settled();
        let headset = |p: u8| SoundState {
            sink: Some(vol(p, false)),
            sink_name: dev("bluez_output.headset", "Headset"),
            ..state(0, false, 100, false)
        };
        assert_eq!(w.sound(Some(&headset(40)), late), Heard::Show(Pill::Sink { label: "Headset".into(), volume: vol(40, false) }));
        assert_eq!(w.sound(Some(&headset(45)), late + ms(300)), Heard::Quiet, "its level restored inside the grace");
        assert_eq!(w.sound(Some(&headset(50)), late + GRACE + ms(1)), Heard::Show(Pill::Volume(vol(50, false))));
        let speakers = state(70, false, 100, false);
        let back = late + Duration::from_secs(3);
        assert_eq!(w.sound(Some(&speakers), back), Heard::Show(Pill::Sink { label: "Desk speakers".into(), volume: vol(70, false) }));
        let other = SoundState { sink_name: dev("hdmi", "Screen"), ..state(30, false, 100, false) };
        assert_eq!(w.sound(Some(&other), back + ms(200)), Heard::Quiet, "a second switch inside the first one's grace");
    }

    #[test]
    fn a_device_appearing_or_going_shows_nothing_and_starts_a_grace() {
        let (mut w, late) = settled();
        let none = SoundState { sink: None, sink_name: None, ..state(0, false, 100, false) };
        assert_eq!(w.sound(Some(&none), late), Heard::Quiet, "the default sink went");
        assert_eq!(w.sound(Some(&state(84, false, 100, false)), late + ms(100)), Heard::Quiet, "it came back");
        assert_eq!(w.sound(Some(&state(85, false, 100, false)), late + ms(900)), Heard::Quiet, "inside its grace");
        assert_eq!(
            w.sound(Some(&state(87, false, 100, false)), late + ms(1050)),
            Heard::Quiet,
            "the grace counts from when it came back, not from when it went"
        );
        assert_eq!(w.sound(Some(&state(86, false, 100, false)), late + ms(1100)), Heard::Show(Pill::Volume(vol(86, false))));
    }

    #[test]
    fn a_default_source_appearing_or_going_shows_nothing_and_starts_its_grace() {
        let (mut w, late) = settled();
        let none = SoundState { source: None, source_name: None, ..state(84, false, 0, false) };
        assert_eq!(w.sound(Some(&none), late), Heard::Quiet, "the default source went");
        assert_eq!(w.sound(Some(&state(84, false, 100, true)), late + ms(100)), Heard::Quiet, "it came back");
        assert_eq!(w.sound(Some(&state(84, false, 90, true)), late + ms(900)), Heard::Quiet, "inside its grace");
        assert_eq!(
            w.sound(Some(&state(84, false, 85, true)), late + ms(1050)),
            Heard::Quiet,
            "the grace counts from when it came back, not from when it went"
        );
        assert_eq!(w.sound(Some(&state(84, false, 80, true)), late + ms(1101)), Heard::Show(Pill::Mic(vol(80, true))));
    }

    #[test]
    fn the_microphone_shows_unless_a_sink_pill_just_did() {
        let (mut w, late) = settled();
        assert_eq!(w.sound(Some(&state(84, false, 100, true)), late), Heard::Show(Pill::Mic(vol(100, true))));
        let t = late + Duration::from_secs(2);
        assert_eq!(w.sound(Some(&state(90, false, 100, true)), t), Heard::Show(Pill::Volume(vol(90, false))));
        assert_eq!(w.sound(Some(&state(90, false, 100, false)), t + ms(499)), Heard::Quiet, "499 ms after a sink pill");
        assert_eq!(w.sound(Some(&state(90, false, 90, false)), t + MIC_AFTER_SINK), Heard::Show(Pill::Mic(vol(90, false))), "at 500 ms");
        let u = t + Duration::from_secs(2);
        assert_eq!(
            w.sound(Some(&state(80, false, 80, true)), u),
            Heard::Show(Pill::Volume(vol(80, false))),
            "both in one message: the sink"
        );
    }

    #[test]
    fn a_default_source_switch_shows_nothing_and_starts_its_grace() {
        let (mut w, late) = settled();
        let webcam = |p: u8| SoundState {
            source: Some(vol(p, false)),
            source_name: dev("alsa_input.webcam", "Webcam"),
            ..state(84, false, 0, false)
        };
        assert_eq!(w.sound(Some(&webcam(60)), late), Heard::Quiet, "no name card for a source");
        assert_eq!(w.sound(Some(&webcam(70)), late + ms(500)), Heard::Quiet, "inside its grace");
        assert_eq!(w.sound(Some(&webcam(80)), late + ms(1001)), Heard::Show(Pill::Mic(vol(80, false))));
    }

    #[test]
    fn the_pill_goes_at_its_deadline_and_each_change_moves_it() {
        let t0 = Instant::now();
        let timeout = timeout(&OsdConfig::default());
        assert_eq!(timeout, ms(1300));
        let mut o = Osd::default();
        assert_eq!((o.shown(), o.deadline()), (None, None), "nothing at rest");
        o.show(Pill::Volume(vol(50, false)), t0, timeout);
        assert_eq!(o.deadline(), Some(t0 + ms(1300)));
        assert!(!o.expire(t0 + ms(1299)));
        o.show(Pill::Volume(vol(55, false)), t0 + ms(1000), timeout);
        assert_eq!(o.shown(), Some(&Pill::Volume(vol(55, false))), "replaced, one pill");
        assert!(!o.expire(t0 + ms(1300)), "the old deadline is gone");
        assert!(o.expire(t0 + ms(2300)), "at exactly the new one");
        assert_eq!((o.shown(), o.deadline()), (None, None));
        assert!(!o.expire(t0 + ms(9000)), "nothing left to hide");
        o.show(Pill::Brightness(10), t0, timeout);
        o.hide();
        assert_eq!(o.shown(), None);
    }

    #[test]
    fn a_volume_pill_is_a_glyph_a_bar_and_the_percentage() {
        let b = rows(&Pill::Volume(vol(64, false)), 32);
        assert_eq!(b.cells(), (34, 1), "32 cells and a blank one each side");
        assert_eq!(texts(&b), [format!(" {VOLUME_MEDIUM} {}{}  64% ", "━".repeat(16), "─".repeat(9))]);
        assert_eq!(styles(&b), [format!(".a.{}{}......", "a".repeat(16), "m".repeat(9))]);
        let b = rows(&Pill::Volume(vol(64, false)), 24);
        assert_eq!(b.cells(), (26, 1));
        assert_eq!(texts(&b), [format!(" {VOLUME_MEDIUM} {}{}  64% ", "━".repeat(10), "─".repeat(7))], "a 17-cell bar");
        assert_eq!(texts(&rows(&Pill::Volume(vol(0, false)), 32)), [format!(" {VOLUME_LOW} {}   0% ", "─".repeat(25))]);
        assert_eq!(texts(&rows(&Pill::Volume(vol(100, false)), 32)), [format!(" {VOLUME_HIGH} {} 100% ", "━".repeat(25))]);
    }

    #[test]
    fn the_glyph_follows_the_level_as_the_bar_does() {
        assert_eq!(volume_glyph(vol(33, false)), VOLUME_LOW);
        assert_eq!(volume_glyph(vol(34, false)), VOLUME_MEDIUM);
        assert_eq!(volume_glyph(vol(66, false)), VOLUME_MEDIUM);
        assert_eq!(volume_glyph(vol(67, false)), VOLUME_HIGH);
        assert_eq!(volume_glyph(vol(80, true)), VOLUME_OFF);
    }

    #[test]
    fn muted_dims_everything_and_keeps_the_level() {
        let b = rows(&Pill::Volume(vol(64, true)), 32);
        assert_eq!(texts(&b), [format!(" {VOLUME_OFF} {}{}  64% ", "━".repeat(16), "─".repeat(9))]);
        assert_eq!(styles(&b), [format!(".m.{}.mmmm.", "m".repeat(25))]);
    }

    #[test]
    fn above_100_the_bar_fills_in_warn_and_shows_the_real_number() {
        let b = rows(&Pill::Volume(vol(130, false)), 32);
        assert_eq!(texts(&b), [format!(" {VOLUME_HIGH} {} 130% ", "━".repeat(25))]);
        assert_eq!(styles(&b), [format!(".a.{}......", "w".repeat(25))]);
        assert_eq!(texts(&rows(&Pill::Volume(vol(255, false)), 32)), [format!(" {VOLUME_HIGH} {} 255% ", "━".repeat(25))]);
    }

    #[test]
    fn the_microphone_and_the_brightness_pills() {
        assert_eq!(texts(&rows(&Pill::Mic(vol(100, false)), 32)), [format!(" {MIC_ON} {} 100% ", "━".repeat(25))]);
        let muted = rows(&Pill::Mic(vol(100, true)), 32);
        assert_eq!(texts(&muted), [format!(" {MIC_OFF} {} 100% ", "━".repeat(25))]);
        assert_eq!(styles(&muted), [format!(".m.{}.mmmm.", "m".repeat(25))]);
        let b = rows(&Pill::Brightness(50), 32);
        assert_eq!(texts(&b), [format!(" {BRIGHTNESS} {}{}  50% ", "━".repeat(12), "─".repeat(13))]);
        assert_eq!(styles(&b), [format!(".a.{}{}......", "a".repeat(12), "m".repeat(13))]);
    }

    #[test]
    fn a_sink_switch_shows_its_name_in_place_of_the_bar() {
        let b = rows(&Pill::Sink { label: "Desk speakers".into(), volume: vol(64, false) }, 32);
        assert_eq!(texts(&b), [format!(" {VOLUME_MEDIUM} Desk speakers{}  64% ", " ".repeat(12))]);
        assert_eq!(styles(&b), [format!(".a.{}......", ".".repeat(25))]);
        let long = rows(&Pill::Sink { label: "A rather long headset name from its vendor".into(), volume: vol(40, false) }, 32);
        let t = &texts(&long)[0];
        assert!(t.contains("…  40% "), "cut to the bar's 25 cells: {t}");
        assert_eq!(crate::grid::text_width(t), 34, "never wider than the pill");
    }

    #[test]
    fn each_pill_is_described_for_the_log() {
        assert_eq!(describe(&Pill::Volume(vol(64, false))), "volume 64%");
        assert_eq!(describe(&Pill::Volume(vol(64, true))), "volume 64% muted");
        assert_eq!(describe(&Pill::Sink { label: "Headset".into(), volume: vol(94, false) }), "default sink changed, 94%");
        assert_eq!(describe(&Pill::Mic(vol(100, true))), "mic 100% muted");
        assert_eq!(describe(&Pill::Brightness(50)), "brightness 50%");
    }

    #[test]
    fn the_backlight_level_is_a_small_integer_or_nothing() {
        assert_eq!(parse_level("19393\n"), Some(19393));
        assert_eq!(parse_level(" 7 "), Some(7));
        assert_eq!(parse_level("4294967295"), Some(u32::MAX));
        for bad in ["", "\n", "-1", "+5", "12a", "1 2", "4294967296", "12345678901", "１２"] {
            assert_eq!(parse_level(bad), None, "{bad:?}");
        }
        assert_eq!(brightness_percent("9696\n", "19393\n"), Some(50));
        assert_eq!(brightness_percent("0\n", "19393\n"), Some(0));
        assert_eq!(brightness_percent("19393\n", "19393\n"), Some(100));
        assert_eq!(brightness_percent("20000\n", "19393\n"), Some(100), "past the maximum: full, never 103");
        assert_eq!(brightness_percent("5\n", "0\n"), None, "a zero maximum");
        assert_eq!(brightness_percent("x\n", "100\n"), None);
        assert_eq!(brightness_percent("50\n", "lots\n"), None);
    }
}
