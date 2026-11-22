use super::*;

const INITIAL: &str = include_str!("../../tests/fixtures/pw-dump-initial.json");
const DELTAS: &str = include_str!("../../tests/fixtures/pw-dump-deltas.json");

fn published(text: &str) -> (Vec<SoundState>, io::ErrorKind) {
    published_for(text, Scope::Osd)
}

fn published_for(text: &str, scope: Scope) -> (Vec<SoundState>, io::ErrorKind) {
    let mut seen = Vec::new();
    let e = follow(text.as_bytes(), scope, &mut |s| {
        seen.push(s);
        true
    })
    .unwrap_err();
    (seen, e.kind())
}

const AG06_OUT: &str = "alsa_output.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo";
const AG06_IN: &str = "alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo";

fn vol(percent: u8, muted: bool) -> Option<Volume> {
    Some(Volume { percent, muted })
}

fn name(node: &str, label: &str) -> Option<DeviceName> {
    Some(DeviceName { node: node.into(), label: label.into() })
}

/// The sink part of each publication: what every test written before the OSD compared.
fn sinks(seen: &[SoundState]) -> Vec<Option<Volume>> {
    seen.iter().map(|s| s.sink).collect()
}

#[test]
fn initial_dump_gives_both_defaults() {
    let (seen, end) = published(INITIAL);
    assert_eq!(end, io::ErrorKind::UnexpectedEof);
    assert_eq!(
        seen,
        [SoundState {
            sink: vol(84, false),
            source: vol(100, false),
            sink_name: name(AG06_OUT, "AG06/AG03 Analog Stereo"),
            source_name: name(AG06_IN, "AG06/AG03 Analog Stereo"),
            sinks: None,
            capture: None,
            headset: None,
        }]
    );
}

#[test]
fn only_sinks_and_sources_are_kept() {
    let mut m = Model::default();
    m.apply(serde_json::from_str::<Vec<Object>>(INITIAL).unwrap());
    assert_eq!(m.nodes(), 4, "the fixture's three sinks and its source; not Firefox's stream, nothing else of the dump");
}

#[test]
fn deltas_follow_volume_mute_default_switch_and_removal() {
    let (seen, end) = published(&format!("{INITIAL}{DELTAS}"));
    assert_eq!(sinks(&seen), [vol(84, false), vol(90, false), vol(90, true), vol(94, false), None, vol(90, true)]);
    assert_eq!(seen[4].sink_name, None, "its node removed: the default is gone, not pending");
    assert_eq!(
        seen[3].sink_name,
        name("alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink", "600 Series Chipset Family HD Audio Speaker")
    );
    assert!(seen.iter().all(|s| s.source == vol(100, false)), "the deltas never touch the source");
    assert_eq!(end, io::ErrorKind::UnexpectedEof);
}

#[test]
fn the_default_sink_named_before_its_node_arrives_then_cleared() {
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"sink-a"}}]}]"#;
    let node = r#"[{"id":7,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"sink-a"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    let cleared = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","type":"","value":null}]}]"#;
    // A re-dump listing the key twice: the last occurrence wins.
    let twice = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"sink-b"}},{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"sink-a"}}]}]"#;
    let (seen, _) = published(&format!("{meta}\n{node}\n{cleared}\n{twice}\n"));
    assert_eq!(sinks(&seen), [None, vol(50, false), None, vol(50, false)]);
}

#[test]
fn metadata_cleared_or_odd_never_ends_the_stream() {
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"sink-a"}}]}]"#;
    let node = r#"[{"id":7,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"sink-a"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    // `pw-metadata -d 0`: every key of subject 0 removed, dumped as a null key.
    let cleared = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":null,"type":null,"value":null}]}]"#;
    // An entry with neither subject nor key: skipped, never an error.
    let odd = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"value":1},{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"sink-a"}}]}]"#;
    let (seen, end) = published(&format!("{meta}\n{node}\n{cleared}\n{odd}\n"));
    assert_eq!(sinks(&seen), [None, vol(50, false), None, vol(50, false)]);
    assert_eq!(end, io::ErrorKind::UnexpectedEof, "the stream went on to its end: never InvalidData");
}

#[test]
fn a_redump_without_params_or_description_keeps_what_is_known() {
    let without = r#"[{"id":122,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"alsa_output.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{}}}]"#;
    let (seen, _) = published(&format!("{INITIAL}{without}\n"));
    assert_eq!(seen.len(), 1, "nothing new to publish: the volume and the label are kept");
    assert_eq!(seen[0].sink_name, name(AG06_OUT, "AG06/AG03 Analog Stereo"));
}

#[test]
fn volume_is_the_cube_root_like_wpctl() {
    let p = |v: f64, mute| PropsParam { channel_volumes: Some(vec![v, v]), mute: Some(mute) };
    assert_eq!(to_volume(&p(0.592696, false)), Some(Volume { percent: 84, muted: false }));
    assert_eq!(to_volume(&p(0.0, true)), Some(Volume { percent: 0, muted: true }));
    assert_eq!(to_volume(&p(1.331, false)).map(|v| v.percent), Some(110));
    assert_eq!(to_volume(&PropsParam { channel_volumes: Some(vec![]), mute: None }), None);
    assert_eq!(to_volume(&PropsParam { channel_volumes: None, mute: Some(true) }), None, "a node's other Props param");
    assert_eq!(to_volume(&p(f64::NAN, false)), None);
}

#[test]
fn two_sinks_sharing_a_name_resolve_to_the_lowest_id() {
    let dump = r#"[
            {"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","value":{"name":"dup"}}]},
            {"id":9,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"dup"},"params":{"Props":[{"channelVolumes":[0.729,0.729],"mute":false}]}}},
            {"id":3,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"dup"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}},
            {"id":1,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"dup"},"params":{"Props":[{"channelVolumes":[0.0,0.0],"mute":false}]}}}
        ]"#;
    let mut m = Model::default();
    m.apply(serde_json::from_str::<Vec<Object>>(dump).unwrap());
    assert_eq!(m.state().sink, vol(50, false), "the lower sink id (3) wins; never the source (1), whatever its id");
}

#[test]
fn a_default_metadata_redump_without_props_is_still_recognised() {
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"sink-a"}}]}]"#;
    let sink_a = r#"[{"id":7,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"sink-a"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    let sink_b = r#"[{"id":8,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"sink-b"},"params":{"Props":[{"channelVolumes":[0.216,0.216],"mute":false}]}}}]"#;
    // A re-dump of the already-known default metadata object, carrying
    // only the changed `metadata` entries, no `props` at all.
    let switched = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"sink-b"}}]}]"#;
    let (seen, end) = published(&format!("{meta}\n{sink_a}\n{sink_b}\n{switched}\n"));
    assert_eq!(sinks(&seen), [None, vol(50, false), vol(60, false)]);
    assert_eq!(end, io::ErrorKind::UnexpectedEof);
}

#[test]
fn odd_property_types_never_end_the_stream() {
    // pw-dump prints a string property as a raw JSON number or boolean
    // when its value looks like one (the real dump has "alsa.card":3,
    // "node.driver":true): a node whose node.name or media.class is such
    // a value, or a metadata object whose name is, must not kill the
    // stream; neither must an odd `subject` (negative, or not a number).
    let numeric_name =
        r#"[{"id":5,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":1234,"node.name":true},"params":null}}]"#;
    let numeric_meta_name = r#"[{"id":6,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":777},"metadata":[{"subject":-1,"key":"k","value":1},{"subject":"nope","key":"k2","value":2}]}]"#;
    let null_props = r#"[{"id":7,"type":"PipeWire:Interface:Node","info":{"props":null,"params":{"Props":null}}}]"#;
    let (seen, end) = published(&format!("{INITIAL}{numeric_name}\n{numeric_meta_name}\n{null_props}\n"));
    assert_eq!(sinks(&seen), [vol(84, false)]);
    assert_eq!(seen.len(), 1, "none of this touches a default");
    assert_eq!(end, io::ErrorKind::UnexpectedEof, "odd property types never end the stream");
}

/// The default source (OSD spec § 2): its mute, a switch to a virtual source (echo cancelling,
/// named by its nick), its node going. A stream recording the microphone, with the same node
/// name, is never a source; a source node arriving that is not the default publishes nothing.
#[test]
fn the_default_source_is_followed_like_the_sink() {
    let muted = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo","node.description":"AG06/AG03 Analog Stereo"},"params":{"Props":[{"channelVolumes":[1.0,1.0],"mute":true}]}}}]"#;
    let stream = r#"[{"id":170,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Stream/Input/Audio","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[0.0,0.0],"mute":false}]}}}]"#;
    let echo = r#"[{"id":180,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source/Virtual","node.name":"echo-cancel-source","node.nick":"Echo cancel"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    let switched = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[{"subject":0,"key":"default.audio.source","type":"Spa:String:JSON","value":{"name":"echo-cancel-source"}}]}]"#;
    let gone = r#"[{"id":180,"info":null}]"#;
    let (seen, end) = published(&format!("{INITIAL}{muted}\n{stream}\n{echo}\n{switched}\n{gone}\n"));
    assert_eq!(end, io::ErrorKind::UnexpectedEof);
    let sources: Vec<_> = seen.iter().map(|s| (s.source, s.source_name.as_ref().map(|n| n.label.clone()))).collect();
    let ag06 = Some("AG06/AG03 Analog Stereo".to_string());
    assert_eq!(
        sources,
        [(vol(100, false), ag06.clone()), (vol(100, true), ag06), (vol(50, false), Some("Echo cancel".into())), (None, None)],
        "the stream and the echo node arriving publish nothing"
    );
    assert!(seen.iter().all(|s| s.sink == vol(84, false)), "the sink never moved");
}

/// The label (OSD spec § 1, the name card): the description, else the nick, else the node name;
/// blank text is absent; capped at LABEL_MAX bytes on a character boundary.
#[test]
fn the_label_is_the_description_then_the_nick_then_the_node_name_and_is_capped() {
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","value":{"name":"s"}}]}]"#;
    let label = |props: &str| {
        let node = format!(
            r#"[{{"id":7,"type":"PipeWire:Interface:Node","info":{{"props":{{"media.class":"Audio/Sink",{props}}},"params":{{"Props":[{{"channelVolumes":[0.125,0.125],"mute":false}}]}}}}}}]"#
        );
        let (seen, _) = published(&format!("{meta}\n{node}\n"));
        seen.last().and_then(|s| s.sink_name.clone()).map(|n| n.label)
    };
    assert_eq!(label(r#""node.name":"s","node.description":"Desk","node.nick":"D""#).as_deref(), Some("Desk"));
    assert_eq!(label(r#""node.name":"s","node.description":"  ","node.nick":"D""#).as_deref(), Some("D"), "a blank description is absent");
    assert_eq!(label(r#""node.name":"s""#).as_deref(), Some("s"), "neither: the node name");
    let long = "é".repeat(100); // 200 bytes
    let got = label(&format!(r#""node.name":"s","node.description":"{long}""#)).unwrap();
    assert_eq!(got.len(), LABEL_MAX, "capped, on a character boundary");
    assert!(got.chars().all(|c| c == 'é'));
}

/// The OSD draws the label: control characters and invisible formatting characters (bidi
/// overrides, zero-width space) are removed like any drawn text (`text::clean`), a newline is a
/// space; a description of nothing but those is blank. The node name, the identity, stays raw.
#[test]
fn device_labels_are_cleaned_like_any_drawn_text() {
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","value":{"name":"s\u202ex"}}]}]"#;
    let named = |props: &str| {
        let node = format!(
            r#"[{{"id":7,"type":"PipeWire:Interface:Node","info":{{"props":{{"media.class":"Audio/Sink","node.name":"s\u202ex",{props}}},"params":{{"Props":[{{"channelVolumes":[0.125,0.125],"mute":false}}]}}}}}}]"#
        );
        let (seen, _) = published(&format!("{meta}\n{node}\n"));
        seen.last().and_then(|s| s.sink_name.clone()).unwrap()
    };
    let n = named(r#""node.description":"Desk\u202eevil\u200b\n2\u0007""#);
    assert_eq!(n.label, "Deskevil 2");
    assert_eq!(n.node, "s\u{202e}x", "the identity is never altered");
    assert_eq!(named(r#""node.description":"\u202e\u200b\u0007","node.nick":"D""#).label, "D", "nothing drawable: blank");
    assert_eq!(named(r#""node.nick":"\t""#).label, "sx", "the node name as a label is cleaned too");
}

/// `follow` asks `matches` before building a state (no String cloned for a delta that changes
/// nothing): it must be exactly equality with `state()` on what the scope reads, whatever field
/// moved. The bar reads the sink's volume; the OSD also the sink's name and label, and the
/// source's node and mute (never its level: 2026-10-09).
#[test]
fn matches_is_equality_on_what_the_scope_reads() {
    let mut m = Model::default();
    let mut states: Vec<SoundState> = Vec::new();
    let label = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo","node.description":"Renamed"},"params":{}}}]"#;
    let level = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    let muted = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":true}]}}}]"#;
    let elsewhere = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"not-yet"}}]}]"#;
    let text = format!("{INITIAL}{DELTAS}{label}\n{level}\n{muted}\n{elsewhere}\n");
    let osd = |s: &SoundState| (s.sink, s.sink_name.clone(), s.source.map(|v| v.muted), s.source_name.as_ref().map(|n| n.node.clone()));
    for batch in serde_json::Deserializer::from_str(&text).into_iter::<Vec<Object>>() {
        m.apply(batch.unwrap());
        let now = m.state();
        assert!(m.matches(&now, Scope::Osd) && m.matches(&now, Scope::Bar));
        for before in &states {
            assert_eq!(m.matches(before, Scope::Osd), osd(before) == osd(&now), "{before:?} vs {now:?}");
            assert_eq!(m.matches(before, Scope::Bar), before.sink == now.sink, "{before:?} vs {now:?}");
        }
        states.push(now);
    }
    assert!(states.iter().any(|s| s.source_name.as_ref().is_some_and(|n| n.label == "Renamed")));
    assert!(states.iter().any(|s| s.source == Some(Volume { percent: 50, muted: true })));
    assert!(states.iter().any(|s| s.sink_name.as_ref().is_some_and(|n| n.node == "not-yet")));
}

/// Without the OSD, only the sink's volume publishes (the bar draws
/// nothing else): a source's level, mute or label, or a sink's label, wakes nothing. With it,
/// a source's mute does, never its level.
#[test]
fn each_scope_publishes_only_what_it_reads() {
    let level = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    let muted = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":true}]}}}]"#;
    let relabel = r#"[{"id":122,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"alsa_output.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo","node.description":"Renamed"},"params":{}}}]"#;
    let louder = r#"[{"id":122,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"alsa_output.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[0.729,0.729],"mute":false}]}}}]"#;
    let text = format!("{INITIAL}{level}\n{muted}\n{relabel}\n{louder}\n");
    let (bar, _) = published_for(&text, Scope::Bar);
    assert_eq!(sinks(&bar), [vol(84, false), vol(90, false)], "the initial dump, then the sink's level only");
    let (osd, _) = published_for(&text, Scope::Osd);
    let seen: Vec<_> = osd.iter().map(|s| (s.sink, s.source, s.sink_name.as_ref().map(|n| n.label.clone()))).collect();
    let ag06 = Some("AG06/AG03 Analog Stereo".to_string());
    assert_eq!(
        seen,
        [
            (vol(84, false), vol(100, false), ag06.clone()),
            (vol(84, false), vol(50, true), ag06),
            (vol(84, false), vol(50, true), Some("Renamed".into())),
            (vol(90, false), vol(50, true), Some("Renamed".into())),
        ],
        "the mute (with the level it has by then), the sink's label and level; never the source's level alone"
    );
}

/// A default named before its node is in the dump is that node, its
/// volume not known yet (its label the node name until the node comes), so that the OSD sees a
/// switch, never a default gone then back. A default whose node went is gone.
#[test]
fn a_default_named_before_its_node_is_a_switch_and_one_whose_node_went_is_gone() {
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"new-headset"}}]}]"#;
    let node = r#"[{"id":200,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"new-headset","node.description":"New headset"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}]"#;
    let gone = r#"[{"id":200,"info":null}]"#;
    // The metadata re-dumped naming it again, unchanged: still gone.
    let (seen, _) = published(&format!("{INITIAL}{meta}\n{node}\n{gone}\n{meta}\n"));
    let defaults: Vec<_> = seen.iter().map(|s| (s.sink, s.sink_name.clone())).collect();
    assert_eq!(
        defaults,
        [
            (vol(84, false), name(AG06_OUT, "AG06/AG03 Analog Stereo")),
            (None, name("new-headset", "new-headset")),
            (vol(50, false), name("new-headset", "New headset")),
            (None, None),
        ]
    );
}

/// `pw-metadata -d 0`: every key of subject 0 cleared, the sink's and the source's alike.
#[test]
fn a_cleared_subject_clears_both_defaults() {
    let cleared = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":null,"type":null,"value":null}]}]"#;
    let (seen, _) = published(&format!("{INITIAL}{cleared}\n"));
    assert_eq!(seen.last(), Some(&SoundState::default()));
}

/// An array that names only the sink leaves the source where it was (and the other way round).
#[test]
fn an_array_naming_one_default_leaves_the_other() {
    let sink_only = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink"}}]}]"#;
    let (seen, _) = published(&format!("{INITIAL}{sink_only}\n"));
    assert_eq!(seen.last().map(|s| (s.sink, s.source)), Some((vol(94, false), vol(100, false))));
}

#[test]
fn stops_when_the_bar_stops_and_refuses_garbage() {
    assert!(follow(INITIAL.as_bytes(), Scope::Osd, &mut |_| false).is_ok());
    let e = follow(&b"[{\"id\": \"x\"}]"[..], Scope::Osd, &mut |_| true).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
}

const HDMI: &str = "600 Series Chipset Family HD Audio HDMI / DisplayPort 3 Output";
const SPEAKER: &str = "600 Series Chipset Family HD Audio Speaker";
const AG06_LABEL: &str = "AG06/AG03 Analog Stereo";
/// The fixture's `default` metadata naming the laptop speaker (id 99) the default sink.
const TO_SPEAKER: &str = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","metadata":[{"subject":0,"key":"default.audio.sink","type":"Spa:String:JSON","value":{"name":"alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink"}}]}]"#;

fn sink(id: u32, label: &str, default: bool) -> Sink {
    Sink { id, label: label.into(), default }
}

/// Quick spec § 5: every sink with its node id (what `wpctl set-default` takes) and its label,
/// the default marked; the plain state carries no list.
#[test]
fn every_sink_by_label_with_its_node_id_and_the_default_marked() {
    let mut m = Model::default();
    m.apply(serde_json::from_str::<Vec<Object>>(INITIAL).unwrap());
    assert_eq!(m.sinks(), [sink(64, HDMI, false), sink(99, SPEAKER, false), sink(122, AG06_LABEL, true)]);
    assert_eq!(m.state().sinks, None, "the plain state carries no list");
    assert_eq!(m.state_in(Scope::Osd).sinks, None);
    assert_eq!(m.state_in(Scope::Panel).sinks, Some(m.sinks()));
}

/// Spec § 6: at most 32 sinks, the first by label (case-insensitive); labels cleaned like the OSD's.
#[test]
fn the_list_is_capped_and_its_labels_cleaned() {
    let node = |id: u32, desc: &str| {
        format!(
            r#"{{"id":{id},"type":"PipeWire:Interface:Node","info":{{"props":{{"media.class":"Audio/Sink","node.name":"s{id}","node.description":"{desc}"}},"params":{{}}}}}}"#
        )
    };
    // Ids descending while the labels ascend: the order can only come from the labels.
    let many: Vec<String> = (1..=40u32).map(|i| node(2000 - i, &format!("Out {i:02}"))).collect();
    let mut m = Model::default();
    m.apply(serde_json::from_str::<Vec<Object>>(&format!("[{}]", many.join(","))).unwrap());
    let s = m.sinks();
    assert_eq!(s.len(), MAX_SINKS);
    assert_eq!((s[0].label.as_str(), s[31].label.as_str()), ("Out 01", "Out 32"), "the first 32 by label");
    m.apply(serde_json::from_str::<Vec<Object>>(&format!("[{}]", node(7, "desk\\u202e\\u200bTop\\u0007"))).unwrap());
    assert_eq!(m.sinks()[0], sink(7, "deskTop", false), "cleaned (text::clean); \"desk\" sorts before \"Out\" whatever the case");
}

/// The panel's scope publishes a sink going and a default switch; the OSD's ignores the sink going.
#[test]
fn the_panel_scope_publishes_a_sink_going_and_the_default_moving() {
    let text = format!("{INITIAL}[{{\"id\":64,\"info\":null}}]\n{TO_SPEAKER}\n");
    let (osd, _) = published_for(&text, Scope::Osd);
    assert_eq!(osd.len(), 2, "the initial dump, then the switch: a sink going is no news to the OSD");
    assert!(osd.iter().all(|s| s.sinks.is_none()));
    let (panel, _) = published_for(&text, Scope::Panel);
    let lists: Vec<Vec<Sink>> = panel.iter().map(|s| s.sinks.clone().unwrap_or_default()).collect();
    assert_eq!(
        lists,
        [
            vec![sink(64, HDMI, false), sink(99, SPEAKER, false), sink(122, AG06_LABEL, true)],
            vec![sink(99, SPEAKER, false), sink(122, AG06_LABEL, true)],
            vec![sink(99, SPEAKER, true), sink(122, AG06_LABEL, false)],
        ]
    );
}

/// A panel just opened has a last state without the list: the next batch must publish it.
#[test]
fn a_state_without_the_list_never_matches_the_panel_scope() {
    let mut m = Model::default();
    m.apply(serde_json::from_str::<Vec<Object>>(INITIAL).unwrap());
    let plain = m.state_in(Scope::Osd);
    assert!(m.matches(&plain, Scope::Osd));
    assert!(!m.matches(&plain, Scope::Panel));
    let full = m.state_in(Scope::Panel);
    assert!(m.matches(&full, Scope::Panel));
    assert!(m.matches(&full, Scope::Osd) && m.matches(&full, Scope::Bar), "the list is no news to the bar or the OSD");
}

fn run_tapped(text: &str, base: Scope, tap: &SoundTap) -> Vec<SoundState> {
    let mut seen = Vec::new();
    let _ = follow_tapped(text.as_bytes(), base, tap, &mut |s| {
        seen.push(s);
        true
    });
    seen
}

/// Spec § 5: closed, nothing is built; opening reads the sinks at once from the thread's model
/// (pw-dump is silent at rest); closing goes back to the base scope.
#[test]
fn the_tap_reads_the_sinks_at_open_and_the_stream_carries_them_while_open() {
    let tap = SoundTap::default();
    let mut at_open = None;
    let _ = follow_tapped(INITIAL.as_bytes(), Scope::Osd, &tap, &mut |s| {
        assert!(s.sinks.is_none(), "closed: no list");
        // Opened while the stream is live (pw-dump silent after its initial dump).
        at_open = Some(tap.open());
        assert!(tap.is_open());
        tap.close();
        assert!(!tap.is_open());
        true
    });
    assert_eq!(at_open.and_then(|s| s.sinks).map(|l| l.len()), Some(3), "the thread's model: read at once");
    assert!(run_tapped(INITIAL, Scope::Osd, &tap).iter().all(|s| s.sinks.is_none()), "closed again: none");
}

/// Without the OSD (`Scope::Bar`) a microphone mute publishes nothing,
/// so the loop's copy is stale; opening hands over the whole state as the panel reads it —
/// the sinks and both defaults with their mute — read from the thread's model at once.
#[test]
fn opening_reads_the_whole_panel_state_even_what_the_base_scope_never_published() {
    let muted = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[1.0,1.0],"mute":true}]}}}]"#;
    /// pw-dump silent after its last batch, still running: the panel opens then.
    struct OpenAtTheEnd<'a> {
        data: &'a [u8],
        tap: &'a SoundTap,
        open: Option<SoundState>,
    }
    impl Read for OpenAtTheEnd<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.data.is_empty() && self.open.is_none() {
                self.open = Some(self.tap.open());
            }
            self.data.read(buf)
        }
    }
    let tap = SoundTap::default();
    let text = format!("{INITIAL}{muted}\n");
    let mut reader = OpenAtTheEnd { data: text.as_bytes(), tap: &tap, open: None };
    let mut seen = Vec::new();
    let _ = follow_tapped(&mut reader, Scope::Bar, &tap, &mut |s| {
        seen.push(s.source);
        true
    });
    assert_eq!(seen, [vol(100, false)], "the mute is no news to the bar");
    let open = reader.open.unwrap_or_default();
    assert_eq!(open.source, vol(100, true), "the microphone as it is now");
    assert_eq!(open.sink, vol(84, false));
    assert_eq!(open.sinks.map(|l| l.len()), Some(3));
}

/// A stream that ended (EOF, a parse error, a panic) leaves nothing behind. Through
/// the backoff an opening panel reads an empty model — no sink, no microphone, an empty list —
/// never the dead instance's volumes or node ids (`wpctl set-default` on a recycled id).
#[test]
fn a_stream_that_ended_leaves_no_state_to_open_on() {
    let empty = SoundState { sinks: Some(Vec::new()), ..SoundState::default() };
    let tap = SoundTap::default();
    assert_eq!(run_tapped(INITIAL, Scope::Osd, &tap).len(), 1, "EOF");
    assert_eq!(tap.open(), empty, "after EOF");
    tap.close();
    assert!(follow_tapped(&b"[{\"id\": \"x\"}]"[..], Scope::Osd, &tap, &mut |_| true).is_err());
    assert_eq!(tap.open(), empty, "after a parse error");
    tap.close();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = follow_tapped(INITIAL.as_bytes(), Scope::Osd, &tap, &mut |_| panic!("publish"));
    }));
    assert!(caught.is_err());
    assert_eq!(tap.open(), empty, "after a panic (`supervise` catches it), the lock taken over");
}

/// Closing drops the list the thread kept as its last publication (nothing kept while closed).
#[test]
fn closing_drops_the_list_kept_as_the_last_publication() {
    let tap = SoundTap::default();
    let _ = tap.open();
    let mut checked = false;
    let _ = follow_tapped(INITIAL.as_bytes(), Scope::Osd, &tap, &mut |s| {
        assert!(s.sinks.is_some() && tap.keeps_a_list(), "open: published and kept");
        tap.close();
        checked = !tap.keeps_a_list();
        true
    });
    assert!(checked, "closed: the kept list went");
}

/// PipeWire restarts while the panel is open. The new stream starts from a new
/// model but keeps the panel's scope: every publication carries the list.
#[test]
fn a_restarted_stream_keeps_publishing_the_sinks_while_the_panel_is_open() {
    let tap = SoundTap::default();
    let _ = tap.open();
    let seen = run_tapped(&format!("{INITIAL}{DELTAS}"), Scope::Osd, &tap);
    assert!(!seen.is_empty() && seen.iter().all(|s| s.sinks.is_some()), "{seen:?}");
    assert_eq!(seen[0].sinks.as_ref().map(Vec::len), Some(3));
}

fn published_with_privacy(text: &str, scope: Scope) -> (Vec<SoundState>, io::ErrorKind) {
    let tap = SoundTap::new(true);
    let mut seen = Vec::new();
    let e = follow_tapped(text.as_bytes(), scope, &tap, &mut |s| {
        seen.push(s);
        true
    })
    .unwrap_err();
    (seen, e.kind())
}

const SOURCE: &str = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"state":"idle","props":{"media.class":"Audio/Source","node.name":"alsa_input.test"}}}]"#;

fn stream(id: u32, state: &str) -> String {
    format!(
        r#"[{{"id":{id},"type":"PipeWire:Interface:Node","info":{{"state":"{state}","props":{{"media.class":"Stream/Input/Audio","node.name":"firefox","application.name":"firefox"}}}}}}]"#
    )
}

fn link(id: u32, from: u32, to: u32, state: &str) -> String {
    format!(r#"[{{"id":{id},"type":"PipeWire:Interface:Link","info":{{"output-node-id":{from},"input-node-id":{to},"state":"{state}"}}}}]"#)
}

fn mic(s: &SoundState) -> Option<(Vec<&str>, bool)> {
    s.capture.as_ref().map(|c| (c.mic.names.iter().map(String::as_str).collect(), c.mic.unnamed))
}

#[test]
fn privacy_off_keeps_nothing_more() {
    let camera =
        r#"[{"id":201,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Video/Source","node.name":"v4l2_input.test"}}}]"#;
    let batch = format!("{}{}{}{}", SOURCE, stream(200, "running"), camera, link(300, 123, 200, "active"));
    let mut m = Model::default();
    m.apply(serde_json::Deserializer::from_str(&batch).into_iter::<Vec<Object>>().flatten().flatten().collect());
    assert_eq!(m.nodes(), 1, "the source only, as today");
    assert_eq!(m.kept(), (0, 0), "no graph, no links");
    let (seen, _) = published_for(&batch, Scope::Osd);
    assert!(seen.iter().all(|s| s.capture.is_none()), "nothing published");
    // Through the stream, with privacy off: the dispatch parses the lean `Object` and keeps no
    // graph (the default sink comes last, so its publication follows the stream, link and camera).
    let late = format!("{}{}{}{}{}", SOURCE, stream(200, "running"), camera, link(300, 123, 200, "active"), INITIAL);
    let tap = SoundTap::default();
    let mut kept = Vec::new();
    let _ = follow_tapped(late.as_bytes(), Scope::Osd, &tap, &mut |_| {
        kept.push(tap.lock().model.kept());
        true
    });
    assert!(kept.len() >= 2, "published again after the link: {kept:?}");
    assert!(kept.iter().all(|k| *k == (0, 0)), "no graph, no links: {kept:?}");
}

#[test]
fn a_recording_stream_is_published_as_a_capture() {
    let text = format!(
        "{}{}{}{}{}",
        SOURCE,
        stream(200, "running"),
        link(300, 123, 200, "active"),
        r#"[{"id":300,"info":null}]"#,
        stream(200, "idle")
    );
    let (seen, _) = published_with_privacy(&text, Scope::Bar);
    assert!(seen.iter().all(|s| s.capture.is_some()), "Some exactly when privacy is on");
    let mics: Vec<_> = seen.iter().map(mic).collect();
    let off = Some((vec![], false));
    let on = Some((vec!["firefox"], false));
    // Source alone; the running stream (no link yet: no holder); the link; its removal while the
    // stream still runs (no holder); the stream going idle changes nothing more.
    assert_eq!(mics, [off.clone(), on.clone(), off.clone()], "{mics:?}");
}

#[test]
fn the_link_removal_and_the_idle_stream_each_switch_it_off() {
    let lit = format!("{}{}{}", SOURCE, stream(200, "idle"), link(300, 123, 200, "active"));
    let (seen, _) = published_with_privacy(&lit, Scope::Bar);
    assert_eq!(mic(seen.last().unwrap()), Some((vec!["firefox"], false)), "an active link lights it");
    let removed = format!("{lit}{}", r#"[{"id":300,"info":null}]"#);
    let (seen, _) = published_with_privacy(&removed, Scope::Bar);
    assert_eq!(mic(seen.last().unwrap()), Some((vec![], false)), "the link gone, the stream idle: off");
}

#[test]
fn the_source_mute_and_the_capture_publish_under_every_scope() {
    let muted = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.test"},"params":{"Props":[{"channelVolumes":[1.0],"mute":true}]}}}]"#;
    let unmuted = muted.replace("true", "false");
    let meta = r#"[{"id":40,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.source","type":"Spa:String:JSON","value":{"name":"alsa_input.test"}}]}]"#;
    let text = format!("{meta}{unmuted}{muted}");
    let (with, _) = published_with_privacy(&text, Scope::Bar);
    assert_eq!(with.iter().map(|s| s.source).collect::<Vec<_>>(), [None, vol(100, false), vol(100, true)], "the mute flip reaches the bar");
    let (without, _) = published_for(&text, Scope::Bar);
    assert_eq!(without.len(), 1, "without privacy the Bar scope publishes the first batch only: {without:?}");
    assert!(without[0].source.is_none());
}

#[test]
fn a_capture_change_alone_publishes() {
    let text = format!("{}{}{}{}", SOURCE, stream(200, "idle"), link(300, 123, 200, "active"), link(300, 123, 200, "paused"));
    let (seen, _) = published_with_privacy(&text, Scope::Bar);
    assert!(seen.len() >= 2, "no sink, no mute change: only the capture moves it ({})", seen.len());
    assert_eq!(mic(&seen[seen.len() - 1]), Some((vec![], false)));
}

#[test]
fn a_link_before_its_nodes_and_a_state_only_redump() {
    // One batch, the link first.
    let one = r#"[{"id":300,"type":"PipeWire:Interface:Link","info":{"output-node-id":123,"input-node-id":200,"state":"active"}},{"id":200,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Stream/Input/Audio","application.name":"firefox"}}},{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.test"}}}]"#.to_string();
    let (seen, _) = published_with_privacy(&one, Scope::Bar);
    assert_eq!(seen.len(), 1);
    assert_eq!(mic(&seen[0]), Some((vec!["firefox"], false)), "lit after the one batch");
    // The consumer re-dumped idle, no props: its app name stays, the link goes quiet.
    let redump = r#"[{"id":200,"type":"PipeWire:Interface:Node","info":{"state":"idle"}}]"#;
    let linkidle = link(300, 123, 200, "idle");
    let (seen, _) = published_with_privacy(&format!("{one}{redump}{linkidle}"), Scope::Bar);
    assert_eq!(mic(seen.last().unwrap()), Some((vec![], false)), "off");
    // Running again, still no props: the name is kept (not unnamed).
    let again = r#"[{"id":200,"type":"PipeWire:Interface:Node","info":{"state":"running"}}]"#;
    let (seen, _) = published_with_privacy(&format!("{one}{redump}{linkidle}{again}"), Scope::Bar);
    assert_eq!(mic(seen.last().unwrap()), Some((vec!["firefox"], false)), "the last props kept");
    // A link re-dumped with its state only keeps its ends.
    let state_only = r#"[{"id":300,"type":"PipeWire:Interface:Link","info":{"state":"active"}}]"#;
    let (seen, _) = published_with_privacy(&format!("{one}{redump}{linkidle}{state_only}"), Scope::Bar);
    assert_eq!(mic(seen.last().unwrap()), Some((vec!["firefox"], false)), "ends kept: active again");
}

#[test]
fn lenient_props_and_states() {
    let node = |id: u32, extra: &str| {
        format!(
            r#"[{{"id":{id},"type":"PipeWire:Interface:Node","info":{{"state":"running","props":{{"media.class":"Stream/Input/Audio"{extra}}}}}}}]"#
        )
    };
    let l = |id: u32, to: u32| link(id, 123, to, "active");
    let text = format!(
        "{SOURCE}{}{}{}{}{}{}{}{}",
        node(200, r#","stream.monitor":true"#),
        l(300, 200),
        node(201, r#","stream.monitor":"true""#),
        l(301, 201),
        node(202, r#","node.passive":"true""#),
        l(302, 202),
        node(203, r#","application.name":42"#),
        l(303, 203),
    );
    let (seen, _) = published_with_privacy(&text, Scope::Bar);
    assert_eq!(mic(seen.last().unwrap()), Some((vec!["42"], false)), "monitor and passive streams never light it; a numeric name reads");
    let odd = format!(
        "{SOURCE}{}{}",
        r#"[{"id":204,"type":"PipeWire:Interface:Node","info":{"state":7,"props":{"media.class":"Stream/Input/Audio","application.name":"firefox"}}}]"#,
        r#"[{"id":301,"type":"PipeWire:Interface:Link","info":{"output-node-id":123,"input-node-id":204,"state":5}}]"#
    );
    let (seen, end) = published_with_privacy(&odd, Scope::Bar);
    assert_eq!(end, io::ErrorKind::UnexpectedEof, "no batch failed");
    assert_eq!(mic(seen.last().unwrap()), Some((vec![], false)), "an odd state reads as not running");
    let unknown = format!("{SOURCE}{}{}", stream(205, "weird"), link(302, 123, 205, "weird"));
    let (seen, _) = published_with_privacy(&unknown, Scope::Bar);
    assert_eq!(mic(seen.last().unwrap()), Some((vec![], false)), "an unknown state reads as not running");
}

#[test]
fn the_app_is_the_first_non_empty_name() {
    let node = |extra: &str| {
        format!(
            r#"[{{"id":200,"type":"PipeWire:Interface:Node","info":{{"state":"running","props":{{"media.class":"Stream/Input/Audio","node.name":"node-name"{extra}}}}}}}]"#
        )
    };
    let app = |extra: &str| {
        let (seen, _) = published_with_privacy(&format!("{SOURCE}{}{}", node(extra), link(300, 123, 200, "active")), Scope::Bar);
        mic(seen.last().unwrap()).map(|(n, _)| n.join(",")).unwrap_or_default()
    };
    assert_eq!(
        app(r#","pipewire.access.portal.app_id":"org.test.App","application.name":"a","application.process.binary":"b""#),
        "org.test.App"
    );
    assert_eq!(app(r#","pipewire.access.portal.app_id":"  ","application.name":"a","application.process.binary":"b""#), "a");
    assert_eq!(app(r#","application.process.binary":"b""#), "b");
    assert_eq!(app(""), "node-name");
}

#[test]
fn a_camera_device_is_by_api_or_role() {
    let cam = |extra: &str| {
        format!(
            r#"[{{"id":201,"type":"PipeWire:Interface:Node","info":{{"state":"running","props":{{"media.class":"Video/Source","node.name":"v4l2_input.test"{extra}}}}}}}]"#
        )
    };
    let screen = |extra: &str| {
        let (seen, _) = published_with_privacy(&cam(extra), Scope::Bar);
        let c = seen.last().unwrap().capture.clone().unwrap();
        (c.camera.unnamed, c.screen.unnamed)
    };
    assert_eq!(screen(r#","device.api":"v4l2""#), (true, false));
    assert_eq!(screen(r#","device.api":"libcamera""#), (true, false));
    assert_eq!(screen(r#","media.role":"Camera""#), (true, false));
    assert_eq!(screen(""), (false, true), "a video source that is no device: the screen");
}

#[test]
fn the_graph_is_bounded() {
    let mut nodes = String::from("[");
    for i in 0..1100u32 {
        if i > 0 {
            nodes.push(',');
        }
        nodes.push_str(&format!(
            r#"{{"id":{},"type":"PipeWire:Interface:Node","info":{{"props":{{"media.class":"Stream/Input/Audio"}}}}}}"#,
            1000 + i
        ));
    }
    nodes.push(']');
    let mut m = Model::new(true);
    m.apply_privacy(serde_json::from_str(&nodes).unwrap());
    assert_eq!(m.kept().0, crate::privacy::MAX_NODES);
    assert_eq!(m.bound_line(false), None, "already told once for this model");
    let mut links = String::from("[");
    for i in 0..4200u32 {
        if i > 0 {
            links.push(',');
        }
        links.push_str(&format!(
            r#"{{"id":{},"type":"PipeWire:Interface:Link","info":{{"output-node-id":1,"input-node-id":2,"state":"idle"}}}}"#,
            10_000 + i
        ));
    }
    links.push(']');
    m.apply_privacy(serde_json::from_str(&links).unwrap());
    assert_eq!(m.kept().1, crate::privacy::MAX_LINKS);
    assert_eq!(m.bound_line(true), None, "once");
    // An id already kept is still updated at the bound.
    m.apply_privacy(serde_json::from_str(r#"[{"id":1000,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Stream/Input/Audio","application.name":"firefox"}}}]"#).unwrap());
    assert_eq!(m.kept().0, crate::privacy::MAX_NODES);
    // A removal frees a place.
    m.apply_privacy(serde_json::from_str(r#"[{"id":1000,"info":null}]"#).unwrap());
    assert_eq!(m.kept().0, crate::privacy::MAX_NODES - 1);
}

#[test]
fn without_privacy_the_lean_objects_are_parsed() {
    // Privacy spec § 8: nothing more is parsed without the module. The lean types hold only
    // what the sinks and sources need; the privacy twin carries the rest.
    let opt = size_of::<Option<String>>();
    let device = size_of::<Option<u32>>();
    assert_eq!(
        size_of::<NodeProps>(),
        6 * opt + device,
        "media.class, node.name, description, nick, device.api, device.description, device.id (the headset)"
    );
    assert_eq!(size_of::<Info>(), size_of::<NodeProps>() + size_of::<Params>(), "props and params: no state, no link ends");
    assert!(size_of::<PrivacyProps>() > size_of::<NodeProps>() && size_of::<PrivacyInfo>() > size_of::<Info>());
}

#[test]
fn a_node_redumped_without_a_state_keeps_the_last_one() {
    // The link is idle: only the running stream holds the microphone.
    let text = format!("{SOURCE}{}{}", stream(200, "running"), link(300, 123, 200, "idle"));
    let nostate = r#"[{"id":200,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Stream/Input/Audio","application.name":"firefox"}}}]"#;
    let (seen, _) = published_with_privacy(&format!("{text}{nostate}"), Scope::Bar);
    assert_eq!(mic(&seen[1]), Some((vec!["firefox"], false)), "lit before the re-dump");
    assert_eq!(mic(seen.last().unwrap()), Some((vec!["firefox"], false)), "no state: still running");
    assert_eq!(seen.len(), 2, "the re-dump changed nothing");
}

#[test]
fn the_bound_is_told_once_per_model() {
    let mut m = Model::new(true);
    assert_eq!(m.bound_line(false), Some("privacy: graph bounded (nodes over 1024)"));
    assert_eq!(m.bound_line(false), None);
    assert_eq!(m.bound_line(true), Some("privacy: graph bounded (links over 4096)"));
    assert_eq!(m.bound_line(true), None);
}

#[test]
fn a_new_stream_starts_a_new_graph_and_forgets_it_at_the_end() {
    let tap = SoundTap::new(true);
    let text = format!("{SOURCE}{}{}", stream(200, "running"), link(300, 123, 200, "active"));
    let _ = run_tapped(&text, Scope::Bar, &tap);
    assert_eq!(tap.open().capture, Some(crate::privacy::Capture::default()), "the dead stream's graph went; privacy stays on");
    tap.close();
    let seen = run_tapped(&text, Scope::Bar, &tap);
    assert_eq!(mic(seen.last().unwrap()), Some((vec!["firefox"], false)), "and a second stream builds it again");
}

const HEADSET_DUMP: &str = include_str!("../../tests/fixtures/pw-dump-bluez-headset.json");
type P<'a> = (u32, &'a str, i64, &'a str);
const PROFILES: [P<'static>; 6] = [
    (0, "off", 0, "yes"),
    (1, "a2dp-sink", 18, "yes"),
    (2, "a2dp-sink-sbc", 17, "yes"),
    (3, "headset-head-unit", 2, "yes"),
    (4, "headset-head-unit-cvsd", 1, "yes"),
    (5, "bap-sink", 30, "yes"),
];

fn device(id: u32, api: &str, profiles: &[P<'_>], active: u32) -> String {
    let listed: Vec<String> = profiles
        .iter()
        .map(|(i, n, p, a)| format!(r#"{{"index":{i},"name":"{n}","description":"{n}","priority":{p},"available":"{a}"}}"#))
        .collect();
    format!(
        r#"{{"id":{id},"type":"PipeWire:Interface:Device","info":{{"props":{{"device.api":"{api}","device.description":"CasqueBT"}},"params":{{"EnumProfile":[{}],"Profile":[{{"index":{active}}}]}}}}}}"#,
        listed.join(",")
    )
}

fn apply_all(m: &mut Model, objects: &[String]) {
    m.apply(serde_json::from_str::<Vec<Object>>(&format!("[{}]", objects.join(","))).unwrap());
}

fn headset_model() -> Model {
    let mut m = Model::default();
    m.apply(serde_json::from_str::<Vec<Object>>(HEADSET_DUMP).unwrap());
    m
}

fn casque(active: HeadsetMode, hifi: u32, mic: u32) -> Option<Headset> {
    Some(Headset { device: 61, label: "CasqueBT".into(), active, hifi, mic })
}

#[test]
fn a_bluez_headset_is_kept_and_published_to_the_panel_only() {
    let m = headset_model();
    assert_eq!(m.headset(), casque(HeadsetMode::HiFi, 1, 3));
    assert_eq!(m.state_in(Scope::Panel).headset, casque(HeadsetMode::HiFi, 1, 3));
    assert_eq!(m.state_in(Scope::Osd).headset, None);
    assert_eq!(m.state().headset, None);
    let mut p = Model::new(true);
    p.apply_privacy(serde_json::from_str::<Vec<PrivacyObject>>(HEADSET_DUMP).unwrap());
    assert_eq!(p.headset(), casque(HeadsetMode::HiFi, 1, 3), "a model built for privacy keeps it too");
}

#[test]
fn profiles_by_priority_and_availability() {
    let mut m = headset_model();
    let mut p = PROFILES;
    p[1].3 = "no"; // a2dp-sink unavailable
    p[4].2 = 5; // cvsd above mSBC
    let mut all = p.to_vec();
    all.push((6, "a2dp-duplex", 40, "yes")); // never a Hi-Fi pick: not an a2dp-sink profile
    apply_all(&mut m, &[device(61, "bluez5", &all, 2)]);
    assert_eq!(m.headset(), casque(HeadsetMode::HiFi, 2, 4));
}

#[test]
fn lone_kind_or_bap_only_hides_it() {
    let mut m = headset_model();
    apply_all(&mut m, &[device(61, "bluez5", &PROFILES[..3], 1)]);
    assert_eq!(m.headset(), None, "Hi-Fi alone");
    apply_all(&mut m, &[device(61, "bluez5", &[(1, "a2dp-sink", 18, "yes"), (3, "headset-head-unit", 2, "no")], 1)]);
    assert_eq!(m.headset(), None, "the Mic kind unavailable");
    apply_all(&mut m, &[device(61, "bluez5", &PROFILES, 5)]);
    assert_eq!(m.headset(), None, "active on bap-sink: neither kind");
    apply_all(&mut m, &[device(61, "bluez5", &PROFILES, 0)]);
    assert_eq!(m.headset(), None, "off");
}

#[test]
fn non_bluez_devices_cost_nothing() {
    let mut m = Model::default();
    m.apply(serde_json::from_str::<Vec<Object>>(INITIAL).unwrap());
    // 94: the AG06's device, the default sink's (its node carries device.id 94).
    apply_all(&mut m, &[device(94, "alsa", &PROFILES, 1)]);
    assert_eq!((m.bt_devices(), m.headset()), (0, None));
}

#[test]
fn matches_panel_sees_a_profile_switch() {
    let mut m = headset_model();
    let s = m.state_in(Scope::Panel);
    assert!(m.matches(&s, Scope::Panel));
    // A re-dump with the params only: the device is kept, its active profile follows.
    m.apply(
        serde_json::from_str::<Vec<Object>>(
            r#"[{"id":61,"type":"PipeWire:Interface:Device","info":{"params":{"Profile":[{"index":3}]}}}]"#,
        )
        .unwrap(),
    );
    assert!(!m.matches(&s, Scope::Panel), "the panel hears the switch");
    assert!(m.matches(&s, Scope::Osd), "the OSD does not");
    assert_eq!(m.state_in(Scope::Panel).headset, casque(HeadsetMode::Mic, 1, 3));
}

#[test]
fn device_removed_drops_it() {
    let mut m = headset_model();
    m.apply(serde_json::from_str::<Vec<Object>>(r#"[{"id":61,"info":null}]"#).unwrap());
    assert_eq!((m.bt_devices(), m.headset()), (0, None));
}

#[test]
fn bounds_8_devices_32_profiles() {
    let mut m = Model::default();
    let all: Vec<String> = (100..110).map(|id| device(id, "bluez5", &PROFILES, 1)).collect();
    apply_all(&mut m, &all);
    assert_eq!(m.bt_devices(), MAX_BT_DEVICES);
    let names: Vec<String> = (0..40).map(|i| format!("a2dp-sink-{i}")).collect();
    let long: Vec<P<'_>> = names.iter().enumerate().map(|(i, n)| (i as u32, n.as_str(), 1, "yes")).collect();
    apply_all(&mut m, &[device(100, "bluez5", &long, 1)]);
    assert_eq!(m.profiles(100), MAX_PROFILES);
}
