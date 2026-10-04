//! PipeWire as `pw-dump -m` describes it: a stream of JSON arrays, the first
//! one the whole graph (≈ 600 KB on the workstation), then only the objects
//! that changed (re-dumped whole) or were removed (`{"id": N, "info":
//! null}`, with no "type"). Read array by array: nothing is kept but each
//! sink's volume and the default sink's name.

use crate::system::{SoundState, Volume};
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

#[derive(Debug, Default)]
pub struct Model {
    /// Sinks (media.class Audio/Sink): id → (node.name, volume once known).
    sinks: HashMap<u32, (String, Option<Volume>)>,
    /// Id of the `default` metadata object, and the default sink's node.name.
    default_meta: Option<u32>,
    default_sink: Option<String>,
}

impl Model {
    pub fn apply(&mut self, batch: Vec<Object>) {
        for o in batch {
            if o.kind.is_none() {
                self.sinks.remove(&o.id);
                if self.default_meta == Some(o.id) {
                    self.default_meta = None;
                    self.default_sink = None;
                }
                continue;
            }
            let is_default_meta = o.props.as_ref().and_then(|p| p.name.as_deref()) == Some("default") || self.default_meta == Some(o.id);
            if is_default_meta {
                self.default_meta = Some(o.id);
                // The last entry of subject 0 that sets the default sink, or
                // clears every key (null key).
                let entry = o
                    .metadata
                    .iter()
                    .flatten()
                    .rev()
                    .find(|e| e.subject == Some(0) && e.key.as_deref().is_none_or(|k| k == "default.audio.sink"));
                if let Some(e) = entry {
                    self.default_sink = match e.key {
                        None => None,
                        Some(_) => e.value.get("name").and_then(|n| n.as_str()).map(String::from),
                    };
                }
                continue;
            }
            let Some(info) = o.info else { continue };
            if info.props.media_class.as_deref() != Some("Audio/Sink") {
                self.sinks.remove(&o.id);
                continue;
            }
            // A re-dump without params keeps the last known volume.
            let known = self.sinks.get(&o.id).and_then(|(_, v)| *v);
            let volume = info.params.props.iter().find_map(to_volume).or(known);
            self.sinks.insert(o.id, (info.props.node_name.unwrap_or_default(), volume));
        }
    }

    pub fn state(&self) -> SoundState {
        // Two sinks can share a `node.name` (two instances of the same
        // device profile); pick deterministically rather than depending on
        // the HashMap's iteration order.
        let sink = self
            .default_sink
            .as_ref()
            .and_then(|n| self.sinks.iter().filter(|(_, (name, _))| name == n).min_by_key(|(id, _)| **id))
            .and_then(|(_, (_, v))| *v);
        SoundState { sink }
    }

    /// Sinks kept in memory (tests: nothing else of the dump is).
    pub fn sinks(&self) -> usize {
        self.sinks.len()
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

    fn vol(percent: u8, muted: bool) -> SoundState {
        SoundState { sink: Some(Volume { percent, muted }) }
    }

    #[test]
    fn initial_dump_gives_the_default_sink_volume() {
        assert_eq!(published(INITIAL), (vec![vol(84, false)], io::ErrorKind::UnexpectedEof));
    }

    #[test]
    fn only_sinks_are_kept() {
        let mut m = Model::default();
        m.apply(serde_json::from_str::<Vec<Object>>(INITIAL).unwrap());
        assert_eq!(m.sinks(), 3, "the three sinks of the fixture, nothing else of the dump");
    }

    #[test]
    fn deltas_follow_volume_mute_default_switch_and_removal() {
        let (seen, end) = published(&format!("{INITIAL}{DELTAS}"));
        assert_eq!(seen, [vol(84, false), vol(90, false), vol(90, true), vol(94, false), SoundState { sink: None }, vol(90, true)]);
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
        assert_eq!(seen, [SoundState { sink: None }, vol(50, false), SoundState { sink: None }, vol(50, false)]);
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
        assert_eq!(seen, [SoundState { sink: None }, vol(50, false), SoundState { sink: None }, vol(50, false)]);
        assert_eq!(end, io::ErrorKind::UnexpectedEof, "the stream went on to its end: never InvalidData");
    }

    #[test]
    fn a_redump_without_params_keeps_the_known_volume() {
        let without = r#"[{"id":122,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"alsa_output.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{}}}]"#;
        let (seen, _) = published(&format!("{INITIAL}{without}\n"));
        assert_eq!(seen, [vol(84, false)], "nothing new to publish");
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
            {"id":3,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Sink","node.name":"dup"},"params":{"Props":[{"channelVolumes":[0.125,0.125],"mute":false}]}}}
        ]"#;
        let mut m = Model::default();
        m.apply(serde_json::from_str::<Vec<Object>>(dump).unwrap());
        assert_eq!(m.state(), vol(50, false), "the lower id (3) wins, not insertion or hash order");
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
        assert_eq!(seen, [SoundState { sink: None }, vol(50, false), vol(60, false)]);
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
        assert_eq!(seen, [vol(84, false)], "none of this touches the default sink");
        assert_eq!(end, io::ErrorKind::UnexpectedEof, "odd property types never end the stream");
    }

    #[test]
    fn stops_when_the_bar_stops_and_refuses_garbage() {
        assert!(follow(INITIAL.as_bytes(), &mut |_| false).is_ok());
        let e = follow(&b"[{\"id\": \"x\"}]"[..], &mut |_| true).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}
