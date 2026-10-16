//! PipeWire as `pw-dump -m` describes it: a stream of JSON arrays, the first
//! one the whole graph (≈ 600 KB on the workstation), then only the objects
//! that changed (re-dumped whole) or were removed (`{"id": N, "info":
//! null}`, with no "type"). Read array by array: nothing is kept but each
//! sink's and source's volume and label, and the default sink's and source's
//! names.

use crate::log::write as log;
use crate::privacy::{self, GraphLink, GraphNode};
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
    /// A node's (`running`) or a link's (`active`) state; any other value, or a type that is not
    /// text, reads as neither (privacy spec § 2).
    #[serde(default, deserialize_with = "lenient_text")]
    pub state: Option<String>,
    /// A link's ends.
    #[serde(rename = "output-node-id", default, deserialize_with = "lenient_u32")]
    pub output_node: Option<u32>,
    #[serde(rename = "input-node-id", default, deserialize_with = "lenient_u32")]
    pub input_node: Option<u32>,
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
    // What the privacy rules read (spec § 2); parsed always, kept only with the module.
    #[serde(rename = "media.role", default, deserialize_with = "lenient_text")]
    pub media_role: Option<String>,
    #[serde(rename = "device.api", default, deserialize_with = "lenient_text")]
    pub device_api: Option<String>,
    #[serde(rename = "application.name", default, deserialize_with = "lenient_text")]
    pub application_name: Option<String>,
    #[serde(rename = "application.process.binary", default, deserialize_with = "lenient_text")]
    pub process_binary: Option<String>,
    #[serde(rename = "pipewire.access.portal.app_id", default, deserialize_with = "lenient_text")]
    pub portal_app_id: Option<String>,
    #[serde(rename = "stream.monitor", default, deserialize_with = "lenient_text")]
    pub stream_monitor: Option<String>,
    #[serde(rename = "stream.capture.sink", default, deserialize_with = "lenient_text")]
    pub capture_sink: Option<String>,
    #[serde(rename = "node.passive", default, deserialize_with = "lenient_text")]
    pub passive: Option<String>,
    #[serde(rename = "node.link-group", default, deserialize_with = "lenient_text")]
    pub link_group: Option<String>,
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

/// A boolean property as pw-dump prints it: `true` or `"true"`.
fn flag(v: &Option<String>) -> bool {
    v.as_deref() == Some("true")
}

/// A node as the privacy rules need it (spec § 2); `running` is the node's own state.
fn graph_node(class: &str, running: bool, p: &NodeProps) -> GraphNode {
    let camera_device = matches!(p.device_api.as_deref(), Some("v4l2" | "libcamera")) || p.media_role.as_deref() == Some("Camera");
    // The app: the first non-empty name, as a person reads it.
    let app = [&p.portal_app_id, &p.application_name, &p.process_binary, &p.node_name]
        .into_iter()
        .flatten()
        .map(|s| drawable(s))
        .find(|s| !s.is_empty());
    GraphNode {
        class: class.to_string(),
        running,
        monitor: flag(&p.stream_monitor),
        capture_sink: flag(&p.capture_sink),
        link_group: p.link_group.is_some(),
        passive: flag(&p.passive),
        camera_device,
        app,
    }
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
    /// With the `privacy` module (spec § 8): the capture graph, bounded, and whether its bound was
    /// told (nodes, links) in this model's life. Empty and untouched otherwise.
    privacy: bool,
    graph: HashMap<u32, GraphNode>,
    links: HashMap<u32, GraphLink>,
    bound_told: [bool; 2],
}

impl Model {
    /// A model that keeps the capture graph too when `privacy` is on (privacy spec § 8).
    pub fn new(privacy: bool) -> Model {
        Model { privacy, ..Model::default() }
    }

    pub fn apply(&mut self, batch: Vec<Object>) {
        for o in batch {
            if o.kind.is_none() {
                self.remove(o.id);
                self.graph.remove(&o.id);
                self.links.remove(&o.id);
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
            if self.privacy {
                self.track(o.id, o.kind.as_deref(), &info);
            }
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

    /// Keeps what the privacy rules read of one object (spec § 2): a link, or a node of a class
    /// they use. A node re-dumped without its props (only its state moved) keeps the last props.
    fn track(&mut self, id: u32, kind: Option<&str>, info: &Info) {
        if kind == Some("PipeWire:Interface:Link") {
            let last = self.links.get(&id).copied();
            let (Some(output), Some(input)) = (info.output_node.or(last.map(|l| l.output)), info.input_node.or(last.map(|l| l.input)))
            else {
                return;
            };
            let active = info.state.as_deref().map(|s| s == "active").or(last.map(|l| l.active)).unwrap_or(false);
            if last.is_none() && self.links.len() >= privacy::MAX_LINKS {
                self.bound(true);
                return;
            }
            self.links.insert(id, GraphLink { output, input, active });
            return;
        }
        let running = info.state.as_deref() == Some("running");
        let node = match info.props.media_class.as_deref() {
            Some(class) if privacy::kept(class) => graph_node(class, running, &info.props),
            Some(_) => {
                self.graph.remove(&id);
                return;
            }
            // No props: a state-only re-dump of a node known already.
            None => {
                if let Some(n) = self.graph.get_mut(&id) {
                    n.running = running;
                }
                return;
            }
        };
        if !self.graph.contains_key(&id) && self.graph.len() >= privacy::MAX_NODES {
            self.bound(false);
            return;
        }
        self.graph.insert(id, node);
    }

    /// Past a bound: the log line, once per kind in this model's life.
    fn bound(&mut self, links: bool) {
        if let Some(line) = self.bound_line(links) {
            log(line);
        }
    }

    /// The line to log for a bound reached, the first time only.
    fn bound_line(&mut self, links: bool) -> Option<&'static str> {
        let told = &mut self.bound_told[usize::from(links)];
        if std::mem::replace(told, true) {
            return None;
        }
        Some(if links { "privacy: graph bounded (links over 4096)" } else { "privacy: graph bounded (nodes over 1024)" })
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
        // The microphone's mute (the dim indicator) and the capture reach the bar under every
        // scope (privacy spec § 8).
        if self.privacy && (self.seen(Kind::Source).0.map(|v| v.muted) != s.source.map(|v| v.muted) || s.capture != Some(self.held())) {
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
        SoundState {
            sink,
            source,
            sink_name: sink_name.map(named),
            source_name: source_name.map(named),
            sinks: None,
            capture: self.privacy.then(|| self.held()),
        }
    }

    fn held(&self) -> privacy::Capture {
        privacy::capture(&self.graph, &self.links)
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
        v.sort_by_cached_key(|s| (s.label.to_lowercase(), s.id));
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

    /// Nodes and links kept for privacy (tests).
    #[cfg(test)]
    fn kept(&self) -> (usize, usize) {
        (self.graph.len(), self.links.len())
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
    /// The `privacy` module is on: every model this tap builds keeps the capture graph.
    privacy: bool,
    panel: bool,
    /// The last state published: what a batch is compared with.
    last: Option<SoundState>,
}

impl SoundTap {
    /// A tap whose model keeps the capture graph when `privacy` is on.
    pub fn new(privacy: bool) -> SoundTap {
        SoundTap(Mutex::new(Tapped { model: Model::new(privacy), privacy, ..Tapped::default() }))
    }

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

    /// Whether the last publication kept is holding a sink list (tests).
    #[cfg(test)]
    fn keeps_a_list(&self) -> bool {
        self.lock().last.as_ref().is_some_and(|l| l.sinks.is_some())
    }
}

/// Empties the tap's model and last state when dropped: a stream that ended, however (its end, a
/// parse error, a panic `supervise` catches), leaves nothing an opening panel could read through
/// the backoff — never a dead PipeWire's volumes or node ids.
struct Forget<'a>(&'a SoundTap);

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        let mut t = self.0.lock();
        t.model = Model::new(t.privacy);
        t.last = None;
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
/// panel is open). A new stream starts from a new model and keeps the panel's flag; the model goes
/// when the stream does (`Forget`).
pub fn follow_tapped(output: impl Read, base: Scope, tap: &SoundTap, publish: &mut dyn FnMut(SoundState) -> bool) -> io::Result<()> {
    {
        let mut t = tap.lock();
        t.model = Model::new(t.privacy);
        t.last = None;
    }
    let _forget = Forget(tap);
    for batch in serde_json::Deserializer::from_reader(io::BufReader::new(output)).into_iter::<Vec<Object>>() {
        let batch = batch.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("pw-dump: {e}")))?;
        let state = {
            let mut t = tap.lock();
            let Tapped { model, panel, last, .. } = &mut *t;
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
                capture: None,
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

    /// Controller ruling P4: without the OSD (`Scope::Bar`) a microphone mute publishes nothing,
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

    /// Fix round 1: a stream that ended (EOF, a parse error, a panic) leaves nothing behind. Through
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
        format!(
            r#"[{{"id":{id},"type":"PipeWire:Interface:Link","info":{{"output-node-id":{from},"input-node-id":{to},"state":"{state}"}}}}]"#
        )
    }

    fn mic(s: &SoundState) -> Option<(Vec<&str>, bool)> {
        s.capture.as_ref().map(|c| (c.mic.names.iter().map(String::as_str).collect(), c.mic.unnamed))
    }

    #[test]
    fn privacy_off_keeps_nothing_more() {
        let batch = format!(
            "{}{}{}{}",
            SOURCE,
            stream(200, "running"),
            r#"[{"id":201,"type":"PipeWire:Interface:Node","info":{"props":{"media.class":"Video/Source","node.name":"v4l2_input.test"}}}]"#,
            link(300, 123, 200, "active")
        );
        let mut m = Model::default();
        m.apply(serde_json::Deserializer::from_str(&batch).into_iter::<Vec<Object>>().flatten().flatten().collect());
        assert_eq!(m.nodes(), 1, "the source only, as today");
        assert_eq!(m.kept(), (0, 0), "no graph, no links");
        let (seen, _) = published_for(&batch, Scope::Osd);
        assert!(seen.iter().all(|s| s.capture.is_none()), "nothing published");
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
        assert_eq!(
            with.iter().map(|s| s.source).collect::<Vec<_>>(),
            [None, vol(100, false), vol(100, true)],
            "the mute flip reaches the bar"
        );
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
        assert_eq!(
            mic(seen.last().unwrap()),
            Some((vec!["42"], false)),
            "monitor and passive streams never light it; a numeric name reads"
        );
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
        m.apply(serde_json::from_str(&nodes).unwrap());
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
        m.apply(serde_json::from_str(&links).unwrap());
        assert_eq!(m.kept().1, crate::privacy::MAX_LINKS);
        assert_eq!(m.bound_line(true), None, "once");
        // An id already kept is still updated at the bound.
        m.apply(serde_json::from_str(r#"[{"id":1000,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Stream/Input/Audio","application.name":"firefox"}}}]"#).unwrap());
        assert_eq!(m.kept().0, crate::privacy::MAX_NODES);
        // A removal frees a place.
        m.apply(serde_json::from_str(r#"[{"id":1000,"info":null}]"#).unwrap());
        assert_eq!(m.kept().0, crate::privacy::MAX_NODES - 1);
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
}
