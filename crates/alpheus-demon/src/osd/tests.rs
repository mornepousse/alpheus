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
        sinks: None,
        capture: None,
        headset: None,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A watch past its start-up grace, its last state `state(84, false, 100, false)`; and that time.
fn settled() -> (Watch, Instant) {
    let t0 = Instant::now();
    let mut w = Watch::default();
    assert_eq!(w.sound(Some(&state(84, false, 100, false)), t0, false), Heard::Baseline);
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
    assert_eq!(w.sound(Some(&state(90, false, 100, false)), late, false), Heard::Show(Pill::Volume(vol(90, false))));
    assert_eq!(w.sound(None, late, false), Heard::Quiet, "pw-dump went down");
    assert_eq!(w.sound(Some(&state(50, false, 100, false)), late + ms(10), false), Heard::Baseline, "its first dump after the restart");
    assert_eq!(w.sound(Some(&state(55, false, 100, false)), late + ms(500), false), Heard::Quiet, "inside the new grace");
    assert_eq!(w.sound(Some(&state(60, false, 100, false)), late + ms(1010), false), Heard::Show(Pill::Volume(vol(60, false))));
}

#[test]
fn nothing_shows_within_the_grace_and_a_change_at_its_end_does() {
    let t0 = Instant::now();
    let mut w = Watch::default();
    assert_eq!(w.sound(Some(&state(84, false, 100, false)), t0, false), Heard::Baseline);
    assert_eq!(w.sound(Some(&state(90, false, 100, true)), t0 + ms(999), false), Heard::Quiet, "WirePlumber restoring levels at login");
    assert_eq!(w.sound(Some(&state(91, false, 100, true)), t0 + GRACE, false), Heard::Show(Pill::Volume(vol(91, false))), "at exactly 1 s");
}

#[test]
fn only_a_moved_level_or_a_flipped_mute_shows() {
    assert!(moved(vol(64, false), vol(65, false)));
    assert!(moved(vol(64, false), vol(64, true)));
    assert!(!moved(vol(64, true), vol(64, true)));
    let (mut w, late) = settled();
    let relabelled = SoundState { sink_name: dev(OUT, "Speakers (renamed)"), ..state(84, false, 100, false) };
    assert_eq!(w.sound(Some(&relabelled), late, false), Heard::Quiet, "a label alone is no change");
    assert_eq!(w.sound(Some(&state(84, true, 100, false)), late + ms(1), false), Heard::Show(Pill::Volume(vol(84, true))), "mute flipped");
    let unknown = SoundState { sink: None, ..state(84, true, 100, false) };
    assert_eq!(w.sound(Some(&unknown), late + ms(2), false), Heard::Quiet, "a volume not known yet");
    assert_eq!(
        w.sound(Some(&state(50, false, 100, false)), late + ms(3), false),
        Heard::Quiet,
        "known again, at another level: the new baseline, no pill"
    );
    assert_eq!(
        w.sound(Some(&state(51, false, 100, false)), late + ms(4), false),
        Heard::Show(Pill::Volume(vol(51, false))),
        "then compared with it"
    );
}

#[test]
fn a_default_sink_switch_shows_its_name_then_a_grace() {
    let (mut w, late) = settled();
    let headset =
        |p: u8| SoundState { sink: Some(vol(p, false)), sink_name: dev("bluez_output.headset", "Headset"), ..state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&headset(40)), late, false), Heard::Show(Pill::Sink { label: "Headset".into(), volume: vol(40, false) }));
    assert_eq!(w.sound(Some(&headset(45)), late + ms(300), false), Heard::Quiet, "its level restored inside the grace, no pill shown");
    assert_eq!(w.sound(Some(&headset(50)), late + GRACE + ms(1), false), Heard::Show(Pill::Volume(vol(50, false))));
    let speakers = state(70, false, 100, false);
    let back = late + Duration::from_secs(3);
    assert_eq!(w.sound(Some(&speakers), back, false), Heard::Show(Pill::Sink { label: "Desk speakers".into(), volume: vol(70, false) }));
    let other = SoundState { sink_name: dev("hdmi", "Screen"), ..state(30, false, 100, false) };
    assert_eq!(w.sound(Some(&other), back + ms(200), false), Heard::Quiet, "a second switch inside the first one's grace, no card shown");
}

/// Inside the grace a switch starts, a shown card follows the default
/// (switch to A, then B 400 ms later: the card says B) and its level (a key pressed during the
/// grace); with no pill shown, the grace still keeps quiet.
#[test]
fn a_shown_card_follows_a_switch_and_a_level_inside_the_grace() {
    let (mut w, late) = settled();
    let a = SoundState { sink_name: dev("bluez_output.a", "Headset A"), ..state(40, false, 100, false) };
    let b = |p: u8| SoundState { sink_name: dev("bluez_output.b", "Headset B"), ..state(p, false, 100, false) };
    assert_eq!(w.sound(Some(&a), late, false), Heard::Show(Pill::Sink { label: "Headset A".into(), volume: vol(40, false) }));
    assert_eq!(
        w.sound(Some(&b(30)), late + ms(400), true),
        Heard::Show(Pill::Sink { label: "Headset B".into(), volume: vol(30, false) }),
        "the shown card now says B"
    );
    assert_eq!(
        w.sound(Some(&b(35)), late + ms(600), true),
        Heard::Show(Pill::Sink { label: "Headset B".into(), volume: vol(35, false) }),
        "a key inside B's grace: the card's level follows"
    );
    assert_eq!(w.sound(Some(&b(36)), late + ms(700), false), Heard::Quiet, "no pill shown: the grace keeps quiet");
    assert_eq!(w.sound(Some(&b(36)), late + ms(800), true), Heard::Quiet, "unmoved: nothing to update");
    assert_eq!(
        w.sound(Some(&b(50)), late + ms(1400), true),
        Heard::Show(Pill::Volume(vol(50, false))),
        "past B's grace (from 400 ms): the volume pill"
    );
}

/// A switch to a node whose volume is not known yet shows its card once
/// the volume arrives, inside the switch's grace; later than that, the level is a new baseline.
#[test]
fn a_switch_before_its_volume_shows_the_card_once_the_volume_is_known() {
    let (mut w, late) = settled();
    let headset =
        |v: Option<Volume>| SoundState { sink: v, sink_name: dev("bluez_output.headset", "Headset"), ..state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&headset(None)), late, false), Heard::Quiet, "its volume not known yet");
    assert_eq!(
        w.sound(Some(&headset(Some(vol(40, false)))), late + ms(50), false),
        Heard::Show(Pill::Sink { label: "Headset".into(), volume: vol(40, false) }),
        "known: the card"
    );
    assert_eq!(w.sound(Some(&headset(Some(vol(45, false)))), late + ms(300), false), Heard::Quiet, "once, then the grace");
    let back = late + Duration::from_secs(3);
    let speakers = |v: Option<Volume>| SoundState { sink: v, ..state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&speakers(None)), back, false), Heard::Quiet);
    assert_eq!(
        w.sound(Some(&speakers(Some(vol(70, false)))), back + GRACE, false),
        Heard::Quiet,
        "known only once the switch's grace is over: a new baseline, no card"
    );
    let hdmi = |v: Option<Volume>| SoundState { sink: v, sink_name: dev("hdmi", "Screen"), ..state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&hdmi(None)), back + GRACE + ms(1), false), Heard::Quiet);
    assert_eq!(w.sound(Some(&speakers(None)), back + GRACE + ms(2), false), Heard::Quiet, "switched again before hdmi's volume came");
    assert_eq!(w.sound(Some(&hdmi(Some(vol(20, false)))), back + GRACE + ms(3), false), Heard::Quiet, "hdmi's card is no longer pending");
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
            .map(|(i, s)| w.sound(Some(s), t0 + Duration::from_secs(i.min(1) as u64 * 5) + ms(10 * i as u64), false))
            .collect();
        assert_eq!(heard[0], Heard::Baseline, "{order}");
        let card = Heard::Show(Pill::Sink { label: "New headset".into(), volume: vol(50, false) });
        assert_eq!(heard.iter().filter(|h| **h != Heard::Quiet).count(), 2, "{order}: the baseline and one card: {heard:?}");
        assert_eq!(heard.last(), Some(&card), "{order}: {seen:?}");
    }
}

#[test]
fn a_device_appearing_or_going_shows_nothing_and_starts_a_grace() {
    let (mut w, late) = settled();
    let none = SoundState { sink: None, sink_name: None, ..state(0, false, 100, false) };
    assert_eq!(w.sound(Some(&none), late, false), Heard::Quiet, "the default sink went");
    assert_eq!(w.sound(Some(&state(84, false, 100, false)), late + ms(100), false), Heard::Quiet, "it came back");
    assert_eq!(w.sound(Some(&state(85, false, 100, false)), late + ms(900), false), Heard::Quiet, "inside its grace");
    assert_eq!(
        w.sound(Some(&state(87, false, 100, false)), late + ms(1050), false),
        Heard::Quiet,
        "the grace counts from when it came back, not from when it went"
    );
    assert_eq!(w.sound(Some(&state(86, false, 100, false)), late + ms(1100), false), Heard::Show(Pill::Volume(vol(86, false))));
}

#[test]
fn a_default_source_appearing_or_going_shows_nothing_and_starts_its_grace() {
    let (mut w, late) = settled();
    let none = SoundState { source: None, source_name: None, ..state(84, false, 0, false) };
    assert_eq!(w.sound(Some(&none), late, false), Heard::Quiet, "the default source went");
    assert_eq!(w.sound(Some(&state(84, false, 100, true)), late + ms(100), false), Heard::Quiet, "it came back");
    assert_eq!(w.sound(Some(&state(84, false, 90, false)), late + ms(900), false), Heard::Quiet, "a flip inside its grace");
    assert_eq!(
        w.sound(Some(&state(84, false, 85, true)), late + ms(1050), false),
        Heard::Quiet,
        "the grace counts from when it came back, not from when it went"
    );
    assert_eq!(w.sound(Some(&state(84, false, 80, false)), late + ms(1101), false), Heard::Show(Pill::Mic(vol(80, false))));
}

/// Mae (2026-10-09): the microphone shows only when its mute flips; a level change never does
/// (WebRTC apps move the capture gain during a call). With levels gone, the old rule keeping
/// the microphone quiet 500 ms after a sink pill has nothing left to guard: a flip shows.
#[test]
fn the_microphone_shows_only_when_its_mute_flips() {
    let (mut w, late) = settled();
    for (i, level) in [97, 92, 88, 93, 71, 100, 64].into_iter().enumerate() {
        let t = late + ms(40 * i as u64);
        assert_eq!(w.sound(Some(&state(84, false, level, false)), t, false), Heard::Quiet, "a call's gain control at {level}%");
    }
    let t = late + Duration::from_secs(1);
    assert_eq!(w.sound(Some(&state(84, false, 64, true)), t, false), Heard::Show(Pill::Mic(vol(64, true))), "muted");
    assert_eq!(w.sound(Some(&state(84, false, 50, true)), t + ms(100), false), Heard::Quiet, "a level while muted");
    assert_eq!(w.sound(Some(&state(84, false, 50, false)), t + ms(200), false), Heard::Show(Pill::Mic(vol(50, false))), "live again");
    let u = t + Duration::from_secs(2);
    assert_eq!(w.sound(Some(&state(90, false, 50, false)), u, false), Heard::Show(Pill::Volume(vol(90, false))));
    assert_eq!(
        w.sound(Some(&state(90, false, 50, true)), u + ms(100), false),
        Heard::Show(Pill::Mic(vol(50, true))),
        "100 ms after a sink pill: shown anyway"
    );
    let v = u + Duration::from_secs(2);
    assert_eq!(
        w.sound(Some(&state(80, false, 50, false)), v, false),
        Heard::Show(Pill::Volume(vol(80, false))),
        "both in one message: the sink"
    );
}

#[test]
fn a_default_source_switch_shows_nothing_and_starts_its_grace() {
    let (mut w, late) = settled();
    let webcam = |p: u8, muted: bool| SoundState {
        source: Some(vol(p, muted)),
        source_name: dev("alsa_input.webcam", "Webcam"),
        ..state(84, false, 0, false)
    };
    assert_eq!(w.sound(Some(&webcam(60, false)), late, false), Heard::Quiet, "no name card for a source");
    assert_eq!(w.sound(Some(&webcam(70, true)), late + ms(500), false), Heard::Quiet, "a flip inside its grace");
    assert_eq!(w.sound(Some(&webcam(80, false)), late + ms(1001), false), Heard::Show(Pill::Mic(vol(80, false))));
}

/// `Osd::sound` tells the rules when the sink's card is on screen: a switch inside the grace
/// updates it then, and only then (another pill shown, or the card past its deadline: quiet).
#[test]
fn the_osd_knows_when_the_card_is_shown() {
    let t0 = Instant::now();
    let timeout = timeout(&OsdConfig::default());
    let a = SoundState { sink_name: dev("a", "A"), ..state(40, false, 100, false) };
    let b = SoundState { sink_name: dev("b", "B"), ..state(30, false, 100, false) };
    let c = SoundState { sink_name: dev("c", "C"), ..state(20, false, 100, false) };
    let mut o = Osd::default();
    assert_eq!(o.sound(Some(&state(84, false, 100, false)), t0), Heard::Baseline);
    let late = t0 + Duration::from_secs(5);
    let Heard::Show(card) = o.sound(Some(&a), late) else { panic!("A's card") };
    o.show(card, late, timeout);
    assert_eq!(o.sound(Some(&b), late + ms(400)), Heard::Show(Pill::Sink { label: "B".into(), volume: vol(30, false) }));
    o.show(Pill::Brightness(50), late + ms(500), timeout);
    assert_eq!(o.sound(Some(&c), late + ms(600)), Heard::Quiet, "the brightness shows, not a card");
    let mut o = Osd::default();
    assert_eq!(o.sound(Some(&state(84, false, 100, false)), t0), Heard::Baseline);
    let Heard::Show(card) = o.sound(Some(&a), late) else { panic!("A's card") };
    o.show(card, late, ms(300));
    assert_eq!(o.sound(Some(&b), late + ms(300)), Heard::Quiet, "the card's deadline reached: no longer shown");
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
