//! Tests of the OSD's model (`mod.rs`): when a sound change shows the pill, the pill's row, and the
//! backlight's level. No Wayland, no file, no clock: each test passes the time it wants.

use super::*;
use crate::system::BtDevice;

const OUT: &str = "alsa_output.desk";
const IN: &str = "alsa_input.desk";

/// A volume of `percent`, muted or not.
fn volume(percent: u8, muted: bool) -> Volume {
    Volume { percent, muted }
}

/// A named device: its node name and the label the card shows.
fn device(node: &str, label: &str) -> Option<DeviceName> {
    Some(DeviceName { node: node.into(), label: label.into() })
}

/// A sound state with the default sink at `sink` and the default source at `mic`, each named.
fn sound_state(sink: u8, sink_muted: bool, mic: u8, mic_muted: bool) -> SoundState {
    SoundState {
        sink: Some(volume(sink, sink_muted)),
        source: Some(volume(mic, mic_muted)),
        sink_name: device(OUT, "Desk speakers"),
        source_name: device(IN, "Desk microphone"),
        sinks: None,
        capture: None,
        headset: None,
    }
}

/// `n` milliseconds.
fn millis(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A watch past its start-up grace, its last state `sound_state(84, false, 100, false)`; and that time.
fn settled_watch() -> (Watch, Instant) {
    let t0 = Instant::now();
    let mut w = Watch::default();
    assert_eq!(w.sound(Some(&sound_state(84, false, 100, false)), t0, false), Heard::Baseline);
    (w, t0 + Duration::from_secs(5))
}

/// The text of each row.
fn row_texts(b: &Boxed) -> Vec<String> {
    b.rows.iter().map(|g| g.text()).collect()
}

/// The style code of each cell, one string per row (see `Grid::styles`).
fn row_styles(b: &Boxed) -> Vec<String> {
    b.rows.iter().map(|g| g.styles()).collect()
}

#[test]
fn the_first_state_is_the_baseline_and_a_restart_takes_a_new_one() {
    let (mut w, late) = settled_watch();
    assert_eq!(w.sound(Some(&sound_state(90, false, 100, false)), late, false), Heard::Show(Pill::Volume(volume(90, false))));
    assert_eq!(w.sound(None, late, false), Heard::Quiet, "pw-dump went down");
    assert_eq!(
        w.sound(Some(&sound_state(50, false, 100, false)), late + millis(10), false),
        Heard::Baseline,
        "its first dump after the restart"
    );
    assert_eq!(w.sound(Some(&sound_state(55, false, 100, false)), late + millis(500), false), Heard::Quiet, "inside the new grace");
    assert_eq!(
        w.sound(Some(&sound_state(60, false, 100, false)), late + millis(1010), false),
        Heard::Show(Pill::Volume(volume(60, false)))
    );
}

#[test]
fn nothing_shows_within_the_grace_and_a_change_at_its_end_does() {
    let t0 = Instant::now();
    let mut w = Watch::default();
    assert_eq!(w.sound(Some(&sound_state(84, false, 100, false)), t0, false), Heard::Baseline);
    assert_eq!(
        w.sound(Some(&sound_state(90, false, 100, true)), t0 + millis(999), false),
        Heard::Quiet,
        "WirePlumber restoring levels at login"
    );
    assert_eq!(
        w.sound(Some(&sound_state(91, false, 100, true)), t0 + GRACE, false),
        Heard::Show(Pill::Volume(volume(91, false))),
        "at exactly 1 s"
    );
}

#[test]
fn only_a_moved_level_or_a_flipped_mute_shows() {
    assert!(moved(volume(64, false), volume(65, false)));
    assert!(moved(volume(64, false), volume(64, true)));
    assert!(!moved(volume(64, true), volume(64, true)));
    let (mut w, late) = settled_watch();
    let relabelled = SoundState { sink_name: device(OUT, "Speakers (renamed)"), ..sound_state(84, false, 100, false) };
    assert_eq!(w.sound(Some(&relabelled), late, false), Heard::Quiet, "a label alone is no change");
    assert_eq!(
        w.sound(Some(&sound_state(84, true, 100, false)), late + millis(1), false),
        Heard::Show(Pill::Volume(volume(84, true))),
        "mute flipped"
    );
    let unknown = SoundState { sink: None, ..sound_state(84, true, 100, false) };
    assert_eq!(w.sound(Some(&unknown), late + millis(2), false), Heard::Quiet, "a volume not known yet");
    assert_eq!(
        w.sound(Some(&sound_state(50, false, 100, false)), late + millis(3), false),
        Heard::Quiet,
        "known again, at another level: the new baseline, no pill"
    );
    assert_eq!(
        w.sound(Some(&sound_state(51, false, 100, false)), late + millis(4), false),
        Heard::Show(Pill::Volume(volume(51, false))),
        "then compared with it"
    );
}

#[test]
fn a_default_sink_switch_shows_its_name_then_a_grace() {
    let (mut w, late) = settled_watch();
    let headset = |p: u8| SoundState {
        sink: Some(volume(p, false)),
        sink_name: device("bluez_output.headset", "Headset"),
        ..sound_state(0, false, 100, false)
    };
    assert_eq!(w.sound(Some(&headset(40)), late, false), Heard::Show(Pill::Sink { label: "Headset".into(), volume: volume(40, false) }));
    assert_eq!(w.sound(Some(&headset(45)), late + millis(300), false), Heard::Quiet, "its level restored inside the grace, no pill shown");
    assert_eq!(w.sound(Some(&headset(50)), late + GRACE + millis(1), false), Heard::Show(Pill::Volume(volume(50, false))));
    let speakers = sound_state(70, false, 100, false);
    let back = late + Duration::from_secs(3);
    assert_eq!(w.sound(Some(&speakers), back, false), Heard::Show(Pill::Sink { label: "Desk speakers".into(), volume: volume(70, false) }));
    let other = SoundState { sink_name: device("hdmi", "Screen"), ..sound_state(30, false, 100, false) };
    assert_eq!(
        w.sound(Some(&other), back + millis(200), false),
        Heard::Quiet,
        "a second switch inside the first one's grace, no card shown"
    );
}

/// Inside the grace a switch starts, a shown card follows the default
/// (switch to A, then B 400 ms later: the card says B) and its level (a key pressed during the
/// grace); with no pill shown, the grace still keeps quiet.
#[test]
fn a_shown_card_follows_a_switch_and_a_level_inside_the_grace() {
    let (mut w, late) = settled_watch();
    let a = SoundState { sink_name: device("bluez_output.a", "Headset A"), ..sound_state(40, false, 100, false) };
    let b = |p: u8| SoundState { sink_name: device("bluez_output.b", "Headset B"), ..sound_state(p, false, 100, false) };
    assert_eq!(w.sound(Some(&a), late, false), Heard::Show(Pill::Sink { label: "Headset A".into(), volume: volume(40, false) }));
    assert_eq!(
        w.sound(Some(&b(30)), late + millis(400), true),
        Heard::Show(Pill::Sink { label: "Headset B".into(), volume: volume(30, false) }),
        "the shown card now says B"
    );
    assert_eq!(
        w.sound(Some(&b(35)), late + millis(600), true),
        Heard::Show(Pill::Sink { label: "Headset B".into(), volume: volume(35, false) }),
        "a key inside B's grace: the card's level follows"
    );
    assert_eq!(w.sound(Some(&b(36)), late + millis(700), false), Heard::Quiet, "no pill shown: the grace keeps quiet");
    assert_eq!(w.sound(Some(&b(36)), late + millis(800), true), Heard::Quiet, "unmoved: nothing to update");
    assert_eq!(
        w.sound(Some(&b(50)), late + millis(1400), true),
        Heard::Show(Pill::Volume(volume(50, false))),
        "past B's grace (from 400 ms): the volume pill"
    );
}

/// A switch to a node whose volume is not known yet shows its card once
/// the volume arrives, inside the switch's grace; later than that, the level is a new baseline.
#[test]
fn a_switch_before_its_volume_shows_the_card_once_the_volume_is_known() {
    let (mut w, late) = settled_watch();
    let headset = |v: Option<Volume>| SoundState {
        sink: v,
        sink_name: device("bluez_output.headset", "Headset"),
        ..sound_state(0, false, 100, false)
    };
    assert_eq!(w.sound(Some(&headset(None)), late, false), Heard::Quiet, "its volume not known yet");
    assert_eq!(
        w.sound(Some(&headset(Some(volume(40, false)))), late + millis(50), false),
        Heard::Show(Pill::Sink { label: "Headset".into(), volume: volume(40, false) }),
        "known: the card"
    );
    assert_eq!(w.sound(Some(&headset(Some(volume(45, false)))), late + millis(300), false), Heard::Quiet, "once, then the grace");
    let back = late + Duration::from_secs(3);
    let speakers = |v: Option<Volume>| SoundState { sink: v, ..sound_state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&speakers(None)), back, false), Heard::Quiet);
    assert_eq!(
        w.sound(Some(&speakers(Some(volume(70, false)))), back + GRACE, false),
        Heard::Quiet,
        "known only once the switch's grace is over: a new baseline, no card"
    );
    let hdmi = |v: Option<Volume>| SoundState { sink: v, sink_name: device("hdmi", "Screen"), ..sound_state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&hdmi(None)), back + GRACE + millis(1), false), Heard::Quiet);
    assert_eq!(w.sound(Some(&speakers(None)), back + GRACE + millis(2), false), Heard::Quiet, "switched again before hdmi's volume came");
    assert_eq!(
        w.sound(Some(&hdmi(Some(volume(20, false)))), back + GRACE + millis(3), false),
        Heard::Quiet,
        "hdmi's card is no longer pending"
    );
}

/// The sound model and the watch together: whether pw-dump names the
/// new default before its node arrives, or its node arrives before its volume, the card shows
/// once, with the node's label and level.
#[test]
fn the_card_survives_a_default_named_before_its_node_or_its_volume() {
    const INITIAL: &str = include_str!("../../tests/fixtures/pw-dump-initial.json");
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"new-headset"}}]}]"#;
    let bare = r#"[{"id":200,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"new-headset","node.description":"New headset"},"params":{}}}]"#;
    let full = r#"[{"id":200,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"new-headset","node.description":"New headset"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    for (order, deltas) in [("named first", [meta, bare, full]), ("volume last", [bare, meta, full])] {
        let mut seen = Vec::new();
        let text = format!("{INITIAL}{}\n{}\n{}\n", deltas[0], deltas[1], deltas[2]);
        let _ = crate::pipewire::follow(text.as_bytes(), crate::pipewire::Scope::Osd, &mut |s| {
            seen.push(s);
            true
        });
        let t0 = Instant::now();
        let mut w = Watch::default();
        let heard: Vec<Heard> = seen
            .iter()
            .enumerate()
            .map(|(i, s)| w.sound(Some(s), t0 + Duration::from_secs(i.min(1) as u64 * 5) + millis(10 * i as u64), false))
            .collect();
        assert_eq!(heard[0], Heard::Baseline, "{order}");
        let card = Heard::Show(Pill::Sink { label: "New headset".into(), volume: volume(50, false) });
        assert_eq!(heard.iter().filter(|h| **h != Heard::Quiet).count(), 2, "{order}: the baseline and one card: {heard:?}");
        assert_eq!(heard.last(), Some(&card), "{order}: {seen:?}");
    }
}

#[test]
fn a_device_appearing_or_going_shows_nothing_and_starts_a_grace() {
    let (mut w, late) = settled_watch();
    let none = SoundState { sink: None, sink_name: None, ..sound_state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&none), late, false), Heard::Quiet, "the default sink went");
    assert_eq!(w.sound(Some(&sound_state(84, false, 100, false)), late + millis(100), false), Heard::Quiet, "it came back");
    assert_eq!(w.sound(Some(&sound_state(85, false, 100, false)), late + millis(900), false), Heard::Quiet, "inside its grace");
    assert_eq!(
        w.sound(Some(&sound_state(87, false, 100, false)), late + millis(1050), false),
        Heard::Quiet,
        "the grace counts from when it came back, not from when it went"
    );
    assert_eq!(
        w.sound(Some(&sound_state(86, false, 100, false)), late + millis(1100), false),
        Heard::Show(Pill::Volume(volume(86, false)))
    );
}

#[test]
fn a_default_source_appearing_or_going_shows_nothing_and_starts_its_grace() {
    let (mut w, late) = settled_watch();
    let none = SoundState { source: None, source_name: None, ..sound_state(84, false, 0, false) };
    assert_eq!(w.sound(Some(&none), late, false), Heard::Quiet, "the default source went");
    assert_eq!(w.sound(Some(&sound_state(84, false, 100, true)), late + millis(100), false), Heard::Quiet, "it came back");
    assert_eq!(w.sound(Some(&sound_state(84, false, 90, false)), late + millis(900), false), Heard::Quiet, "a flip inside its grace");
    assert_eq!(
        w.sound(Some(&sound_state(84, false, 85, true)), late + millis(1050), false),
        Heard::Quiet,
        "the grace counts from when it came back, not from when it went"
    );
    assert_eq!(w.sound(Some(&sound_state(84, false, 80, false)), late + millis(1101), false), Heard::Show(Pill::Mic(volume(80, false))));
}

/// The microphone shows only when its mute flips; a level change never does (WebRTC apps move the
/// capture gain during a call). With levels gone, the old rule keeping the microphone quiet 500 ms
/// after a sink pill has nothing left to guard: a flip shows. See docs/decisions.md, "The
/// microphone pill shows only when its mute flips".
#[test]
fn the_microphone_shows_only_when_its_mute_flips() {
    let (mut w, late) = settled_watch();
    for (i, level) in [97, 92, 88, 93, 71, 100, 64].into_iter().enumerate() {
        let t = late + millis(40 * i as u64);
        assert_eq!(w.sound(Some(&sound_state(84, false, level, false)), t, false), Heard::Quiet, "a call's gain control at {level}%");
    }
    let t = late + Duration::from_secs(1);
    assert_eq!(w.sound(Some(&sound_state(84, false, 64, true)), t, false), Heard::Show(Pill::Mic(volume(64, true))), "muted");
    assert_eq!(w.sound(Some(&sound_state(84, false, 50, true)), t + millis(100), false), Heard::Quiet, "a level while muted");
    assert_eq!(
        w.sound(Some(&sound_state(84, false, 50, false)), t + millis(200), false),
        Heard::Show(Pill::Mic(volume(50, false))),
        "live again"
    );
    let u = t + Duration::from_secs(2);
    assert_eq!(w.sound(Some(&sound_state(90, false, 50, false)), u, false), Heard::Show(Pill::Volume(volume(90, false))));
    assert_eq!(
        w.sound(Some(&sound_state(90, false, 50, true)), u + millis(100), false),
        Heard::Show(Pill::Mic(volume(50, true))),
        "100 ms after a sink pill: shown anyway"
    );
    let v = u + Duration::from_secs(2);
    assert_eq!(
        w.sound(Some(&sound_state(80, false, 50, false)), v, false),
        Heard::Show(Pill::Volume(volume(80, false))),
        "both in one message: the sink"
    );
}

#[test]
fn a_default_source_switch_shows_nothing_and_starts_its_grace() {
    let (mut w, late) = settled_watch();
    let webcam = |p: u8, muted: bool| SoundState {
        source: Some(volume(p, muted)),
        source_name: device("alsa_input.webcam", "Webcam"),
        ..sound_state(84, false, 0, false)
    };
    assert_eq!(w.sound(Some(&webcam(60, false)), late, false), Heard::Quiet, "no name card for a source");
    assert_eq!(w.sound(Some(&webcam(70, true)), late + millis(500), false), Heard::Quiet, "a flip inside its grace");
    assert_eq!(w.sound(Some(&webcam(80, false)), late + millis(1001), false), Heard::Show(Pill::Mic(volume(80, false))));
}

/// `Osd::sound` tells the rules when the sink's card is on screen: a switch inside the grace
/// updates it then, and only then (another pill shown, or the card past its deadline: quiet).
#[test]
fn the_osd_knows_when_the_card_is_shown() {
    let t0 = Instant::now();
    let timeout = timeout(&OsdConfig::default());
    let a = SoundState { sink_name: device("a", "A"), ..sound_state(40, false, 100, false) };
    let b = SoundState { sink_name: device("b", "B"), ..sound_state(30, false, 100, false) };
    let c = SoundState { sink_name: device("c", "C"), ..sound_state(20, false, 100, false) };
    let mut o = Osd::default();
    assert_eq!(o.sound(Some(&sound_state(84, false, 100, false)), t0), Heard::Baseline);
    let late = t0 + Duration::from_secs(5);
    let Heard::Show(card) = o.sound(Some(&a), late) else { panic!("A's card") };
    o.show(card, late, timeout);
    assert_eq!(o.sound(Some(&b), late + millis(400)), Heard::Show(Pill::Sink { label: "B".into(), volume: volume(30, false) }));
    o.show(Pill::Brightness(50), late + millis(500), timeout);
    assert_eq!(o.sound(Some(&c), late + millis(600)), Heard::Quiet, "the brightness shows, not a card");
    let mut o = Osd::default();
    assert_eq!(o.sound(Some(&sound_state(84, false, 100, false)), t0), Heard::Baseline);
    let Heard::Show(card) = o.sound(Some(&a), late) else { panic!("A's card") };
    o.show(card, late, millis(300));
    assert_eq!(o.sound(Some(&b), late + millis(300)), Heard::Quiet, "the card's deadline reached: no longer shown");
}

#[test]
fn the_pill_goes_at_its_deadline_and_each_change_moves_it() {
    let t0 = Instant::now();
    let timeout = timeout(&OsdConfig::default());
    assert_eq!(timeout, millis(1300));
    let mut o = Osd::default();
    assert_eq!((o.shown(), o.deadline()), (None, None), "nothing at rest");
    o.show(Pill::Volume(volume(50, false)), t0, timeout);
    assert_eq!(o.deadline(), Some(t0 + millis(1300)));
    assert!(!o.expire(t0 + millis(1299)));
    o.show(Pill::Volume(volume(55, false)), t0 + millis(1000), timeout);
    assert_eq!(o.shown(), Some(&Pill::Volume(volume(55, false))), "replaced, one pill");
    assert!(!o.expire(t0 + millis(1300)), "the old deadline is gone");
    assert!(o.expire(t0 + millis(2300)), "at exactly the new one");
    assert_eq!((o.shown(), o.deadline()), (None, None));
    assert!(!o.expire(t0 + millis(9000)), "nothing left to hide");
    o.show(Pill::Brightness(10), t0, timeout);
    o.hide();
    assert_eq!(o.shown(), None);
}

#[test]
fn a_volume_pill_is_a_glyph_a_bar_and_the_percentage() {
    let b = rows(&Pill::Volume(volume(64, false)), 32);
    assert_eq!(b.cells(), (34, 1), "32 cells and a blank one each side");
    assert_eq!(row_texts(&b), [format!(" {VOLUME_MEDIUM} {}{}  64% ", "━".repeat(16), "─".repeat(9))]);
    assert_eq!(row_styles(&b), [format!(".a.{}{}......", "a".repeat(16), "m".repeat(9))]);
    let b = rows(&Pill::Volume(volume(64, false)), 24);
    assert_eq!(b.cells(), (26, 1));
    assert_eq!(row_texts(&b), [format!(" {VOLUME_MEDIUM} {}{}  64% ", "━".repeat(10), "─".repeat(7))], "a 17-cell bar");
    assert_eq!(row_texts(&rows(&Pill::Volume(volume(0, false)), 32)), [format!(" {VOLUME_LOW} {}   0% ", "─".repeat(25))]);
    assert_eq!(row_texts(&rows(&Pill::Volume(volume(100, false)), 32)), [format!(" {VOLUME_HIGH} {} 100% ", "━".repeat(25))]);
}

#[test]
fn the_glyph_follows_the_level_as_the_bar_does() {
    assert_eq!(volume_glyph(volume(33, false)), VOLUME_LOW);
    assert_eq!(volume_glyph(volume(34, false)), VOLUME_MEDIUM);
    assert_eq!(volume_glyph(volume(66, false)), VOLUME_MEDIUM);
    assert_eq!(volume_glyph(volume(67, false)), VOLUME_HIGH);
    assert_eq!(volume_glyph(volume(80, true)), VOLUME_OFF);
}

#[test]
fn muted_dims_everything_and_keeps_the_level() {
    let b = rows(&Pill::Volume(volume(64, true)), 32);
    assert_eq!(row_texts(&b), [format!(" {VOLUME_OFF} {}{}  64% ", "━".repeat(16), "─".repeat(9))]);
    assert_eq!(row_styles(&b), [format!(".m.{}.mmmm.", "m".repeat(25))]);
}

#[test]
fn above_100_the_bar_fills_in_warn_and_shows_the_real_number() {
    let b = rows(&Pill::Volume(volume(130, false)), 32);
    assert_eq!(row_texts(&b), [format!(" {VOLUME_HIGH} {} 130% ", "━".repeat(25))]);
    assert_eq!(row_styles(&b), [format!(".a.{}......", "w".repeat(25))]);
    assert_eq!(row_texts(&rows(&Pill::Volume(volume(255, false)), 32)), [format!(" {VOLUME_HIGH} {} 255% ", "━".repeat(25))]);
}

#[test]
fn the_microphone_and_the_brightness_pills() {
    assert_eq!(row_texts(&rows(&Pill::Mic(volume(100, false)), 32)), [format!(" {MIC_ON} {} 100% ", "━".repeat(25))]);
    let muted = rows(&Pill::Mic(volume(100, true)), 32);
    assert_eq!(row_texts(&muted), [format!(" {MIC_OFF} {} 100% ", "━".repeat(25))]);
    assert_eq!(row_styles(&muted), [format!(".m.{}.mmmm.", "m".repeat(25))]);
    let b = rows(&Pill::Brightness(50), 32);
    assert_eq!(row_texts(&b), [format!(" {BRIGHTNESS} {}{}  50% ", "━".repeat(12), "─".repeat(13))]);
    assert_eq!(row_styles(&b), [format!(".a.{}{}......", "a".repeat(12), "m".repeat(13))]);
}

#[test]
fn a_sink_switch_shows_its_name_in_place_of_the_bar() {
    let b = rows(&Pill::Sink { label: "Desk speakers".into(), volume: volume(64, false) }, 32);
    assert_eq!(row_texts(&b), [format!(" {VOLUME_MEDIUM} Desk speakers{}  64% ", " ".repeat(12))]);
    assert_eq!(row_styles(&b), [format!(".a.{}......", ".".repeat(25))]);
    let long = rows(&Pill::Sink { label: "A rather long headset name from its vendor".into(), volume: volume(40, false) }, 32);
    let t = &row_texts(&long)[0];
    assert!(t.contains("…  40% "), "cut to the bar's 25 cells: {t}");
    assert_eq!(text_width(t), 34, "never wider than the pill");
}

#[test]
fn each_pill_is_described_for_the_log() {
    assert_eq!(describe(&Pill::Volume(volume(64, false))), "volume 64%");
    assert_eq!(describe(&Pill::Volume(volume(64, true))), "volume 64% muted");
    assert_eq!(describe(&Pill::Sink { label: "Headset".into(), volume: volume(94, false) }), "default sink changed, 94%");
    assert_eq!(describe(&Pill::Mic(volume(100, true))), "mic 100% muted");
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

// ---- The passive kinds: Bluetooth, do-not-disturb, the power profile. ----

/// A watch that heard its baseline `value` at `t0`; and `t0`.
fn heard_baseline<T: PartialEq + Clone + std::fmt::Debug>(value: T) -> (Passive<T>, Instant) {
    let t0 = Instant::now();
    let mut p = Passive::default();
    assert_eq!(p.hear(Some(value), t0), None, "the baseline never shows");
    (p, t0)
}

/// Bluetooth on with these connected devices.
fn bt_on(names: &[&str]) -> BtState {
    BtState::On { devices: names.iter().map(|n| BtDevice { name: (*n).into(), battery: None }).collect() }
}

/// The bell's view.
fn bell(dnd: bool, serving: bool) -> NotifyView {
    NotifyView { unread: 0, dnd, serving }
}

/// power-profiles-daemon with `active`.
fn profile(active: Option<Profile>) -> ProfileState {
    ProfileState { active, offered: Profile::ALL.to_vec() }
}

#[test]
fn passive_first_value_is_the_baseline() {
    let (mut p, t0) = heard_baseline(false);
    assert_eq!(p.hear(Some(true), t0 + Duration::from_secs(5)), Some(true), "a change after the grace shows");
    let mut fresh: Passive<bool> = Passive::default();
    assert_eq!(fresh.hear(Some(true), t0 + Duration::from_secs(60)), None, "the first value, however late");
}

#[test]
fn passive_inside_the_grace_nothing_shows() {
    let (mut p, t0) = heard_baseline(false);
    assert_eq!(p.hear(Some(true), t0 + GRACE - millis(1)), None, "a change just inside the grace");
    assert_eq!(p.hear(Some(true), t0 + GRACE + millis(1)), None, "that value became the last: unchanged now");
    assert_eq!(p.hear(Some(false), t0 + GRACE + millis(2)), Some(false));
}

#[test]
fn passive_at_the_grace_end_a_change_shows() {
    let (mut p, t0) = heard_baseline(Profile::Balanced);
    assert_eq!(p.hear(Some(Profile::Performance), t0 + GRACE), Some(Profile::Performance), "at exactly its end the grace is over");
}

#[test]
fn passive_the_same_value_never_shows() {
    let (mut p, t0) = heard_baseline(true);
    for n in 1..5 {
        assert_eq!(p.hear(Some(true), t0 + Duration::from_secs(n)), None);
    }
}

#[test]
fn passive_a_source_going_down_takes_a_new_baseline() {
    let (mut p, t0) = heard_baseline(false);
    let late = t0 + Duration::from_secs(5);
    assert_eq!(p.hear(None, late), None, "the source went down");
    assert_eq!(p.hear(Some(true), late + millis(10)), None, "its first value after the restart is a baseline");
    assert_eq!(p.hear(Some(false), late + millis(500)), None, "inside the new grace");
    assert_eq!(p.hear(Some(true), late + millis(1100)), Some(true));
}

#[test]
fn bluetooth_device_changes_do_not_show() {
    let t0 = Instant::now();
    let mut k = Kinds::default();
    assert_eq!(k.bluetooth(Some(&bt_on(&[])), t0), None, "the baseline");
    let late = t0 + Duration::from_secs(5);
    assert_eq!(k.bluetooth(Some(&bt_on(&["Headset"])), late), None, "a device connecting");
    assert_eq!(k.bluetooth(Some(&bt_on(&["Headset", "Mouse"])), late + millis(10)), None);
    assert_eq!(k.bluetooth(Some(&bt_on(&[])), late + millis(20)), None, "devices leaving");
}

#[test]
fn bluetooth_a_power_flip_shows_on_and_off() {
    let t0 = Instant::now();
    let mut k = Kinds::default();
    assert_eq!(k.bluetooth(Some(&BtState::Off), t0), None);
    let late = t0 + Duration::from_secs(5);
    assert_eq!(k.bluetooth(Some(&bt_on(&["Headset"])), late), Some(Pill::Bluetooth(true)));
    assert_eq!(k.bluetooth(Some(&BtState::Off), late + millis(10)), Some(Pill::Bluetooth(false)));
    assert_eq!(k.bluetooth(Some(&BtState::Absent), late + millis(20)), None, "the adapter went: a reset");
    assert_eq!(k.bluetooth(Some(&bt_on(&[])), late + millis(30)), None, "a new baseline");
    assert_eq!(k.bluetooth(None, late + millis(40)), None, "the bus went: a reset too");
    assert_eq!(k.bluetooth(Some(&BtState::Off), late + millis(50)), None, "a new baseline");
}

#[test]
fn dnd_shows_only_while_serving() {
    let t0 = Instant::now();
    let mut k = Kinds::default();
    assert_eq!(k.dnd(bell(false, false), t0), None, "not serving: nothing to follow");
    let late = t0 + Duration::from_secs(5);
    assert_eq!(k.dnd(bell(true, false), late), None, "still not serving");
    assert_eq!(k.dnd(bell(true, true), late + millis(10)), None, "the name acquired: the baseline");
    let later = late + Duration::from_secs(5);
    assert_eq!(k.dnd(bell(false, true), later), Some(Pill::Dnd(false)));
    assert_eq!(k.dnd(NotifyView { unread: 3, ..bell(false, true) }, later + millis(10)), None, "an unread count is not a change");
    assert_eq!(k.dnd(bell(true, true), later + millis(20)), Some(Pill::Dnd(true)));
    assert_eq!(k.dnd(bell(false, false), later + millis(30)), None, "the name lost: a reset");
    assert_eq!(k.dnd(bell(true, true), later + millis(40)), None, "a new baseline");
}

#[test]
fn profile_a_change_shows_its_label() {
    let t0 = Instant::now();
    let mut k = Kinds::default();
    assert_eq!(k.profile(Some(&profile(Some(Profile::Balanced))), t0), None);
    let late = t0 + Duration::from_secs(5);
    assert_eq!(k.profile(Some(&profile(Some(Profile::PowerSaver))), late), Some(Pill::Profile(Profile::PowerSaver)));
    assert_eq!(k.profile(Some(&profile(None)), late + millis(10)), None, "no active profile: a reset");
    assert_eq!(k.profile(Some(&profile(Some(Profile::Performance))), late + millis(20)), None, "a new baseline");
    assert_eq!(k.profile(None, late + millis(30)), None);
    let b = rows(&Pill::Profile(Profile::PowerSaver), 32);
    assert!(row_texts(&b)[0].ends_with(" saver "), "the panel's label: {:?}", row_texts(&b));
}

#[test]
fn rows_of_a_text_pill() {
    let b = rows(&Pill::Bluetooth(true), 32);
    assert_eq!(b.cells(), (34, 1));
    assert_eq!(row_texts(&b), [format!(" {BLUETOOTH} Bluetooth{}on ", " ".repeat(19))]);
    assert_eq!(row_styles(&b), [format!(".a.{}.", ".".repeat(30))]);
    let off = rows(&Pill::Bluetooth(false), 32);
    assert_eq!(row_texts(&off), [format!(" {BLUETOOTH} Bluetooth{}off ", " ".repeat(18))]);
    assert_eq!(row_styles(&off), [format!(".m.{}.", ".".repeat(30))], "off dims the glyph");
    assert_eq!(row_texts(&rows(&Pill::Dnd(true), 32)), [format!(" {BELL_OFF} Do not disturb{}on ", " ".repeat(14))]);
    let dnd_off = rows(&Pill::Dnd(false), 32);
    assert_eq!(row_texts(&dnd_off), [format!(" {BELL} Do not disturb{}off ", " ".repeat(13))]);
    assert_eq!(row_styles(&dnd_off), [format!(".m.{}.", ".".repeat(30))]);
    let p = rows(&Pill::Profile(Profile::Performance), 32);
    assert_eq!(row_texts(&p), [format!(" {PROFILE} Power profile{}performance ", " ".repeat(6))]);
    assert_eq!(row_styles(&p), [format!(".a.{}.", ".".repeat(30))]);
    // The narrowest pill: the label gives way to the word, one blank cell between them.
    let narrow = rows(&Pill::Profile(Profile::Performance), 24);
    let t = &row_texts(&narrow)[0];
    assert!(t.ends_with("… performance "), "{t}");
    assert_eq!(text_width(t), 26, "never wider than the pill");
}

#[test]
fn osd_passive_pills_are_described_in_fixed_words() {
    assert_eq!(describe(&Pill::Bluetooth(true)), "bluetooth on");
    assert_eq!(describe(&Pill::Bluetooth(false)), "bluetooth off");
    assert_eq!(describe(&Pill::Dnd(true)), "dnd on");
    assert_eq!(describe(&Pill::Dnd(false)), "dnd off");
    for p in Profile::ALL {
        assert_eq!(describe(&Pill::Profile(p)), "profile changed", "never the profile itself");
    }
}
