//! PipeWire as `pw-dump -m` describes it: a stream of JSON arrays, the first
//! one the whole graph (≈ 600 KB on the workstation), then only the objects
//! that changed (re-dumped whole) or were removed (`{"id": N, "info":
//! null}`, with no "type"). Read array by array: nothing is kept but each
//! sink's and source's volume and label, and the default sink's and source's
//! names.

use crate::system::{DeviceName, SoundState, Volume};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Read};

/// One object of an array; every field this module doesn't name is skipped.
#[derive(Deserialize)]
pub struct Object {
    pub id: u32,
    /// Absent on a removed object.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub info: Option<Info>,
    /// Metadata objects: their name (`default`, `settings`…).
    #[serde(default)]
    pub props: Option<MetaProps>,
    #[serde(default)]
    pub metadata: Option<Vec<MetaEntry>>,
}

#[derive(Deserialize, Default)]
pub struct Info {
    #[serde(default, deserialize_with = "null_as_default")]
    pub props: NodeProps,
    #[serde(default, deserialize_with = "null_as_default")]
    pub params: Params,
}

#[derive(Deserialize, Default)]
pub struct NodeProps {
    #[serde(rename = "media.class", default, deserialize_with = "lenient_text")]
    pub media_class: Option<String>,
    #[serde(rename = "node.name", default, deserialize_with = "lenient_text")]
    pub node_name: Option<String>,
    #[serde(rename = "node.description", default, deserialize_with = "lenient_text")]
    pub description: Option<String>,
    #[serde(rename = "node.nick", default, deserialize_with = "lenient_text")]
    pub nick: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct Params {
    #[serde(rename = "Props", default, deserialize_with = "null_as_default")]
    pub props: Vec<PropsParam>,
}

#[derive(Deserialize)]
pub struct PropsParam {
    #[serde(rename = "channelVolumes")]
    pub channel_volumes: Option<Vec<f64>>,
    pub mute: Option<bool>,
}

#[derive(Deserialize)]
pub struct MetaProps {
    #[serde(rename = "metadata.name", default, deserialize_with = "lenient_text")]
    pub name: Option<String>,
}

/// One metadata entry. Everything optional: `{"subject":0,"key":null,…}` is
/// how a cleared subject is dumped, and an entry this module doesn't expect
/// must be skipped, not end the stream.
#[derive(Deserialize)]
pub struct MetaEntry {
    #[serde(default, deserialize_with = "lenient_u32")]
    pub subject: Option<u32>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub value: serde_json::Value,
}

/// A property pw-dump may print as a bare JSON number or boolean instead of
/// a string, when its value happens to look like one (the real dump has
/// `"alsa.card":3`, `"node.driver":true` next to genuine strings on the same
/// kind of object): kept as text; anything else (array, object, missing,
/// null) becomes `None` rather than failing the whole batch.
fn lenient_text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        Some(serde_json::Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    })
}

/// A field that should be its default value (an empty struct, an empty
/// vec…) when pw-dump writes it as JSON `null` instead of omitting it.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// `subject` as a plain, non-negative id; a negative number or any other
/// JSON type becomes `None` (never matches a real object id) rather than
/// failing the whole batch.
fn lenient_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        _ => None,
    })
}

/// A node's volume as `wpctl` shows it: the cube root of the mean linear
/// channel volume (PipeWire stores amplitudes, wpctl shows "cubic" volumes).
pub fn to_volume(p: &PropsParam) -> Option<Volume> {
    let channels = p.channel_volumes.as_ref().filter(|c| !c.is_empty())?;
    let mean = channels.iter().sum::<f64>() / channels.len() as f64;
    if !mean.is_finite() || mean < 0.0 {
        return None;
    }
    Some(Volume { percent: (mean.cbrt() * 100.0).round().min(255.0) as u8, muted: p.mute.unwrap_or(false) })
}

/// A label's longest form, in bytes (cut on a character boundary).
pub const LABEL_MAX: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sink,
    Source,
}

/// Audio/Sink, Audio/Source and Audio/Source/… (a virtual source: echo cancelling); streams and
/// everything else: None.
fn kind_of(class: Option<&str>) -> Option<Kind> {
    match class? {
        "Audio/Sink" => Some(Kind::Sink),
        c if c == "Audio/Source" || c.starts_with("Audio/Source/") => Some(Kind::Source),
        _ => None,
    }
}

fn capped(s: &str) -> String {
    let mut end = s.len().min(LABEL_MAX);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// What a person reads: the description, else the nick (blank text is absent); None: neither.
fn label_of(p: &NodeProps) -> Option<String> {
    [p.description.as_deref(), p.nick.as_deref()].into_iter().flatten().map(str::trim).find(|s| !s.is_empty()).map(capped)
}

/// The last entry of subject 0 that sets `key`, or clears every key (a null key): Some(the new
/// default); None when the array does not mention it.
fn default_of(entries: &[MetaEntry], key: &str) -> Option<Option<String>> {
    let e = entries.iter().rev().find(|e| e.subject == Some(0) && e.key.as_deref().is_none_or(|k| k == key))?;
    Some(match e.key {
        None => None,
        Some(_) => e.value.get("name").and_then(|n| n.as_str()).map(String::from),
    })
}

#[derive(Debug)]
struct Node {
    kind: Kind,
    name: String,
    label: String,
    volume: Option<Volume>,
}

#[derive(Debug, Default)]
pub struct Model {
    /// Sinks and sources: id → kind, node.name, label, volume once known.
    nodes: HashMap<u32, Node>,
    /// Id of the `default` metadata object, and the default sink's and source's node.name.
    default_meta: Option<u32>,
    default_sink: Option<String>,
    default_source: Option<String>,
}

impl Model {
    pub fn apply(&mut self, batch: Vec<Object>) {
        for o in batch {
            if o.kind.is_none() {
                self.nodes.remove(&o.id);
                if self.default_meta == Some(o.id) {
                    self.default_meta = None;
                    self.default_sink = None;
                    self.default_source = None;
                }
                continue;
            }
            let is_default_meta = o.props.as_ref().and_then(|p| p.name.as_deref()) == Some("default") || self.default_meta == Some(o.id);
            if is_default_meta {
                self.default_meta = Some(o.id);
                let entries = o.metadata.as_deref().unwrap_or_default();
                if let Some(v) = default_of(entries, "default.audio.sink") {
                    self.default_sink = v;
                }
                if let Some(v) = default_of(entries, "default.audio.source") {
                    self.default_source = v;
                }
                continue;
            }
            let Some(info) = o.info else { continue };
            let Some(kind) = kind_of(info.props.media_class.as_deref()) else {
                self.nodes.remove(&o.id);
                continue;
            };
            let known = self.nodes.get(&o.id);
            // A re-dump without params keeps the last known volume; one without a label, the label.
            let volume = info.params.props.iter().find_map(to_volume).or(known.and_then(|n| n.volume));
            let name = info.props.node_name.clone().unwrap_or_default();
            let label = label_of(&info.props).or_else(|| known.map(|n| n.label.clone())).unwrap_or_else(|| capped(&name));
            self.nodes.insert(o.id, Node { kind, name, label, volume });
        }
    }

    /// The default node of `kind` named `name`. Two nodes can share a `node.name` (two instances of
    /// the same device profile): the lowest id, never the HashMap's order.
    fn resolve(&self, kind: Kind, name: Option<&String>) -> Option<&Node> {
        let name = name?;
        self.nodes.iter().filter(|(_, n)| n.kind == kind && &n.name == name).min_by_key(|(id, _)| **id).map(|(_, n)| n)
    }

    pub fn state(&self) -> SoundState {
        let sink = self.resolve(Kind::Sink, self.default_sink.as_ref());
        let source = self.resolve(Kind::Source, self.default_source.as_ref());
        let named = |n: &Node| DeviceName { node: n.name.clone(), label: n.label.clone() };
        SoundState {
            sink: sink.and_then(|n| n.volume),
            source: source.and_then(|n| n.volume),
            sink_name: sink.map(named),
            source_name: source.map(named),
        }
    }

    /// Sinks and sources kept in memory (tests: nothing else of the dump is).
    pub fn nodes(&self) -> usize {
        self.nodes.len()
    }
}

/// Reads `pw-dump -m` until its output ends. Each array is parsed on its own
/// (StreamDeserializer over a BufReader), applied, then dropped; the state
/// is published each time it changes.
pub fn follow(output: impl Read, publish: &mut dyn FnMut(SoundState) -> bool) -> io::Result<()> {
    let mut model = Model::default();
    let mut last: Option<SoundState> = None;
    for batch in serde_json::Deserializer::from_reader(io::BufReader::new(output)).into_iter::<Vec<Object>>() {
        let batch = batch.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("pw-dump: {e}")))?;
        model.apply(batch);
        let s = model.state();
        if last.as_ref() != Some(&s) {
            if !publish(s.clone()) {
                return Ok(());
            }
            last = Some(s);
        }
    }
    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pw-dump: end of output"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INITIAL: &str = include_str!("../tests/fixtures/pw-dump-initial.json");
    const DELTAS: &str = include_str!("../tests/fixtures/pw-dump-deltas.json");

    fn published(text: &str) -> (Vec<SoundState>, io::ErrorKind) {
        let mut seen = Vec::new();
        let e = follow(text.as_bytes(), &mut |s| {
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
        assert_eq!(
            seen[3].sink_name,
            name(
                "alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink",
                "600 Series Chipset Family HD Audio Speaker"
            )
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
        assert_eq!(
            label(r#""node.name":"s","node.description":"  ","node.nick":"D""#).as_deref(),
            Some("D"),
            "a blank description is absent"
        );
        assert_eq!(label(r#""node.name":"s""#).as_deref(), Some("s"), "neither: the node name");
        let long = "é".repeat(100); // 200 bytes
        let got = label(&format!(r#""node.name":"s","node.description":"{long}""#)).unwrap();
        assert_eq!(got.len(), LABEL_MAX, "capped, on a character boundary");
        assert!(got.chars().all(|c| c == 'é'));
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
        assert!(follow(INITIAL.as_bytes(), &mut |_| false).is_ok());
        let e = follow(&b"[{\"id\": \"x\"}]"[..], &mut |_| true).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}
