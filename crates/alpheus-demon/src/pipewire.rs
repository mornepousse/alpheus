//! PipeWire as `pw-dump -m` describes it: a stream of JSON arrays, the first
//! one the whole graph (≈ 600 KB on the workstation), then only the objects
//! that changed (re-dumped whole) or were removed (`{"id": N, "info":
//! null}`, with no "type"). Read array by array: nothing is kept but each
//! sink's and source's volume and label, and the default sink's and source's
//! names.

use crate::system::{DeviceName, Sink, SoundState, Volume};
use crate::text::clean;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::{Mutex, MutexGuard, PoisonError};

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

/// The most sinks the quick panel lists (spec § 6).
pub const MAX_SINKS: usize = 32;

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

/// Text the OSD draws: control and invisible formatting characters removed (`text::clean`: a
/// device name posing as another), trimmed, capped at LABEL_MAX bytes on a character boundary.
/// One allocation: `clean`'s, cut in place.
fn drawable(s: &str) -> String {
    let mut t = clean(s);
    let lead = t.len() - t.trim_start().len();
    t.drain(..lead);
    let mut end = t.trim_end().len().min(LABEL_MAX);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    t.truncate(end);
    t
}

/// What a person reads: the description, else the nick (blank text is absent); None: neither.
fn label_of(p: &NodeProps) -> Option<String> {
    [p.description.as_deref(), p.nick.as_deref()].into_iter().flatten().map(drawable).find(|s| !s.is_empty())
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

/// Who reads the published state (`follow`): it publishes only when what they read changed, so
/// nobody wakes for a change they never draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The bar alone (no `osd` module): the default sink's volume.
    Bar,
    /// The OSD too: also the default sink's node and label, the default source's node and mute
    /// (never its level: the microphone pill shows on a mute flip only, Mae 2026-10-09).
    Osd,
    /// The quick panel is open: the OSD's, and the sink list.
    Panel,
}

/// A default named by the `default` metadata: its node.name, the label shown while its node is not
/// in the dump yet, and whether its node went (then the default is gone, not pending).
#[derive(Debug)]
struct Named {
    name: String,
    label: String,
    gone: bool,
}

impl Named {
    fn new(name: String) -> Named {
        Named { label: drawable(&name), name, gone: false }
    }
}

/// A new value for a default, from the metadata: the same name keeps what is known of it.
fn rename(slot: &mut Option<Named>, name: Option<String>) {
    if slot.as_ref().map(|n| &n.name) != name.as_ref() {
        *slot = name.map(Named::new);
    }
}

/// A default as the state shows it: its volume, its node and label.
type Seen<'a> = (Option<Volume>, Option<(&'a str, &'a str)>);

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
    default_sink: Option<Named>,
    default_source: Option<Named>,
}

impl Model {
    pub fn apply(&mut self, batch: Vec<Object>) {
        for o in batch {
            if o.kind.is_none() {
                self.remove(o.id);
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
                    rename(&mut self.default_sink, v);
                }
                if let Some(v) = default_of(entries, "default.audio.source") {
                    rename(&mut self.default_source, v);
                }
                continue;
            }
            let Some(info) = o.info else { continue };
            let Some(kind) = kind_of(info.props.media_class.as_deref()) else {
                self.remove(o.id);
                continue;
            };
            let known = self.nodes.get(&o.id);
            // A re-dump without params keeps the last known volume; one without a label, the label.
            let volume = info.params.props.iter().find_map(to_volume).or(known.and_then(|n| n.volume));
            let label = label_of(&info.props).or_else(|| known.map(|n| n.label.clone()));
            let name = info.props.node_name.unwrap_or_default();
            let label = label.unwrap_or_else(|| drawable(&name));
            self.nodes.insert(o.id, Node { kind, name, label, volume });
        }
    }

    /// A node removed (or no longer a sink or a source): a default it was is gone.
    fn remove(&mut self, id: u32) {
        let Some(node) = self.nodes.remove(&id) else { return };
        let named = match node.kind {
            Kind::Sink => self.default_sink.as_mut(),
            Kind::Source => self.default_source.as_mut(),
        };
        if let Some(n) = named.filter(|n| n.name == node.name) {
            n.gone = true;
        }
    }

    /// The default node of `kind` named `name`. Two nodes can share a `node.name` (two instances of
    /// the same device profile): the lowest id, never the HashMap's order.
    fn resolve(&self, kind: Kind, name: &str) -> Option<&Node> {
        self.resolve_id(kind, name).and_then(|id| self.nodes.get(&id))
    }

    /// The id of the node of `kind` named `name`: the lowest id when two share it.
    fn resolve_id(&self, kind: Kind, name: &str) -> Option<u32> {
        self.nodes.iter().filter(|(_, n)| n.kind == kind && n.name == name).map(|(id, _)| *id).min()
    }

    /// The default of `kind` as the state shows it. Its node when in the dump; not there yet (the
    /// metadata came first): that node, its volume unknown, so a switch is seen as one; its node
    /// went: none.
    fn seen(&self, kind: Kind) -> Seen<'_> {
        let named = match kind {
            Kind::Sink => self.default_sink.as_ref(),
            Kind::Source => self.default_source.as_ref(),
        };
        let Some(named) = named else { return (None, None) };
        match self.resolve(kind, &named.name) {
            Some(n) => (n.volume, Some((n.name.as_str(), n.label.as_str()))),
            None if named.gone => (None, None),
            None => (None, Some((named.name.as_str(), named.label.as_str()))),
        }
    }

    /// Whether `state()` would equal `s` on what `scope` reads, without building it (no String
    /// cloned for a delta that changes nothing, as most do: a level of some other node, a port, a
    /// client); the panel, also the sink list (a state without one never matches it).
    pub fn matches(&self, s: &SoundState, scope: Scope) -> bool {
        let (sink, sink_name) = self.seen(Kind::Sink);
        if sink != s.sink {
            return false;
        }
        if scope == Scope::Bar {
            return true;
        }
        let (source, source_name) = self.seen(Kind::Source);
        let osd = sink_name == s.sink_name.as_ref().map(|d| (d.node.as_str(), d.label.as_str()))
            && source.map(|v| v.muted) == s.source.map(|v| v.muted)
            && source_name.map(|(node, _)| node) == s.source_name.as_ref().map(|d| d.node.as_str());
        osd && (scope != Scope::Panel || s.sinks.as_ref().is_some_and(|l| *l == self.sinks()))
    }

    pub fn state(&self) -> SoundState {
        let named = |(node, label): (&str, &str)| DeviceName { node: node.to_string(), label: label.to_string() };
        let ((sink, sink_name), (source, source_name)) = (self.seen(Kind::Sink), self.seen(Kind::Source));
        SoundState { sink, source, sink_name: sink_name.map(named), source_name: source_name.map(named), sinks: None }
    }

    /// Every sink, by label (case-insensitive) then id, the first MAX_SINKS; the default marked
    /// (the node `seen` resolves, whatever its volume).
    pub fn sinks(&self) -> Vec<Sink> {
        let default = self.default_sink.as_ref().filter(|n| !n.gone).and_then(|n| self.resolve_id(Kind::Sink, &n.name));
        let mut v: Vec<Sink> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.kind == Kind::Sink)
            .map(|(id, n)| Sink { id: *id, label: n.label.clone(), default: Some(*id) == default })
            .collect();
        v.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then(a.id.cmp(&b.id)));
        v.truncate(MAX_SINKS);
        v
    }

    /// The state as `scope` reads it: with the sink list under `Scope::Panel` only.
    pub fn state_in(&self, scope: Scope) -> SoundState {
        SoundState { sinks: (scope == Scope::Panel).then(|| self.sinks()), ..self.state() }
    }

    /// Sinks and sources kept in memory (tests: nothing else of the dump is).
    pub fn nodes(&self) -> usize {
        self.nodes.len()
    }
}

/// What the sound thread shares with the loop (quick spec § 5): its pw-dump model, so an opening
/// quick panel reads the sinks at once (pw-dump is silent at rest), and whether that panel is open,
/// so the thread publishes the sink list (`Scope::Panel`) only then. Locked once per pw-dump batch
/// and once per open or close; a poisoned lock is taken over, never a panic.
#[derive(Debug, Default)]
pub struct SoundTap(Mutex<Tapped>);

#[derive(Debug, Default)]
struct Tapped {
    model: Model,
    panel: bool,
    /// The last state published: what a batch is compared with.
    last: Option<SoundState>,
}

impl SoundTap {
    fn lock(&self) -> MutexGuard<'_, Tapped> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The panel opened: the thread publishes the sink list from its next batch on. Returns the
    /// whole state as the panel reads it now (the sinks, and both defaults with their mute), so
    /// what the base scope never published (a microphone mute without the OSD) is never stale.
    pub fn open(&self) -> SoundState {
        let mut t = self.lock();
        t.panel = true;
        t.model.state_in(Scope::Panel)
    }

    /// The panel closed: back to the base scope; the list the thread kept goes.
    pub fn close(&self) {
        let mut t = self.lock();
        t.panel = false;
        if let Some(last) = t.last.as_mut() {
            last.sinks = None;
        }
    }

    pub fn is_open(&self) -> bool {
        self.lock().panel
    }
}

/// Reads `pw-dump -m` until its output ends, with nobody to open a panel (`Scope::Bar` or
/// `Scope::Osd` throughout).
pub fn follow(output: impl Read, scope: Scope, publish: &mut dyn FnMut(SoundState) -> bool) -> io::Result<()> {
    follow_tapped(output, scope, &SoundTap::default(), publish)
}

/// Reads `pw-dump -m` until its output ends. Each array is parsed on its own (StreamDeserializer
/// over a BufReader), without the lock; then, under it, applied to the tap's model and compared
/// with the last state published under the scope in effect (`base`, or `Scope::Panel` while the
/// panel is open). A new stream starts from a new model and keeps the panel's flag.
pub fn follow_tapped(output: impl Read, base: Scope, tap: &SoundTap, publish: &mut dyn FnMut(SoundState) -> bool) -> io::Result<()> {
    {
        let mut t = tap.lock();
        t.model = Model::default();
        t.last = None;
    }
    for batch in serde_json::Deserializer::from_reader(io::BufReader::new(output)).into_iter::<Vec<Object>>() {
        let batch = batch.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("pw-dump: {e}")))?;
        let state = {
            let mut t = tap.lock();
            let Tapped { model, panel, last } = &mut *t;
            model.apply(batch);
            let scope = if *panel { Scope::Panel } else { base };
            if last.as_ref().is_some_and(|l| model.matches(l, scope)) {
                continue;
            }
            let s = model.state_in(scope);
            *last = Some(s.clone());
            s
        };
        if !publish(state) {
            return Ok(());
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
    /// source's node and mute (never its level: Mae, 2026-10-09).
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

    /// Final review (slice A): without the OSD, only the sink's volume publishes (the bar draws
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

    /// Final review (slice A): a default named before its node is in the dump is that node, its
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
        assert!(run_tapped(INITIAL, Scope::Osd, &tap).iter().all(|s| s.sinks.is_none()), "closed: no list");
        assert_eq!(tap.open().sinks.map(|l| l.len()), Some(3), "the model the stream left: read at once");
        assert!(tap.is_open());
        tap.close();
        assert!(!tap.is_open());
        assert!(run_tapped(INITIAL, Scope::Osd, &tap).iter().all(|s| s.sinks.is_none()), "closed again: none");
    }

    /// Controller ruling P4: without the OSD (`Scope::Bar`) a microphone mute publishes nothing,
    /// so the loop's copy is stale; opening hands over the whole state as the panel reads it —
    /// the sinks and both defaults with their mute — read from the thread's model at once.
    #[test]
    fn opening_reads_the_whole_panel_state_even_what_the_base_scope_never_published() {
        let muted = r#"[{"id":123,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.usb-Yamaha_Corporation_AG06_AG03-00.analog-stereo"},"params":{"Props":[{"channelVolumes":[1.0,1.0],"mute":true}]}}}]"#;
        let tap = SoundTap::default();
        let seen = run_tapped(&format!("{INITIAL}{muted}\n"), Scope::Bar, &tap);
        assert_eq!(seen.len(), 1, "the mute is no news to the bar: {seen:?}");
        assert_eq!(seen[0].source, vol(100, false));
        let open = tap.open();
        assert_eq!(open.source, vol(100, true), "the microphone as it is now");
        assert_eq!(open.sink, vol(84, false));
        assert_eq!(open.sinks.map(|l| l.len()), Some(3));
    }

    /// Review Focus 1: PipeWire restarts while the panel is open. The new stream starts from a new
    /// model but keeps the panel's scope: every publication carries the list.
    #[test]
    fn a_restarted_stream_keeps_publishing_the_sinks_while_the_panel_is_open() {
        let tap = SoundTap::default();
        let _ = tap.open();
        let seen = run_tapped(&format!("{INITIAL}{DELTAS}"), Scope::Osd, &tap);
        assert!(!seen.is_empty() && seen.iter().all(|s| s.sinks.is_some()), "{seen:?}");
        assert_eq!(seen[0].sinks.as_ref().map(Vec::len), Some(3));
    }
}
