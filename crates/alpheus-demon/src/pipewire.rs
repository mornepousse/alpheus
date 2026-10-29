//! PipeWire as `pw-dump -m` describes it: a stream of JSON arrays, the first
//! one the whole graph (≈ 600 KB on the workstation), then only the objects
//! that changed (re-dumped whole) or were removed (`{"id": N, "info":
//! null}`, with no "type"). Read array by array: nothing is kept but each
//! sink's and source's volume and label, and the default sink's and source's
//! names.

use crate::log::write as log;
use crate::privacy::{self, GraphLink, GraphNode};
use crate::system::{DeviceName, Headset, HeadsetMode, Sink, SoundState, Volume};
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
    /// A device's (`bluez5` is the one kept) and a node's device: the headset (quick Bluetooth
    /// spec § 4).
    #[serde(rename = "device.api", default, deserialize_with = "lenient_text")]
    pub device_api: Option<String>,
    #[serde(rename = "device.description", default, deserialize_with = "lenient_text")]
    pub device_description: Option<String>,
    #[serde(rename = "device.id", default, deserialize_with = "lenient_u32")]
    pub device_id: Option<u32>,
}

/// `Object`'s twin for a model built for privacy (privacy spec § 8): the same fields plus what the
/// capture rules read. Only `follow_tapped` with the tap's privacy flag deserializes it; without
/// the module the lean `Object` is parsed and nothing more.
#[derive(Deserialize)]
pub struct PrivacyObject {
    pub id: u32,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub info: Option<PrivacyInfo>,
    #[serde(default)]
    pub props: Option<MetaProps>,
    #[serde(default)]
    pub metadata: Option<Vec<MetaEntry>>,
}

#[derive(Deserialize, Default)]
pub struct PrivacyInfo {
    #[serde(default, deserialize_with = "null_as_default")]
    pub props: PrivacyProps,
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

/// `NodeProps` and what the privacy rules read (spec § 2).
#[derive(Deserialize, Default)]
pub struct PrivacyProps {
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
    #[serde(rename = "device.description", default, deserialize_with = "lenient_text")]
    pub device_description: Option<String>,
    #[serde(rename = "device.id", default, deserialize_with = "lenient_u32")]
    pub device_id: Option<u32>,
}

#[derive(Deserialize, Default)]
pub struct Params {
    #[serde(rename = "Props", default, deserialize_with = "null_as_default")]
    pub props: Vec<PropsParam>,
    /// A device's profiles and its active one (the headset, quick Bluetooth spec § 4).
    #[serde(rename = "EnumProfile", default, deserialize_with = "null_as_default")]
    pub enum_profile: Vec<ProfileParam>,
    #[serde(rename = "Profile", default, deserialize_with = "null_as_default")]
    pub profile: Vec<ProfileParam>,
}

/// One profile of a device, every field lenient: a wrong type reads None, never fails the batch.
#[derive(Deserialize)]
pub struct ProfileParam {
    #[serde(default, deserialize_with = "lenient_u32")]
    pub index: Option<u32>,
    #[serde(default, deserialize_with = "lenient_text")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub priority: Option<i64>,
    #[serde(default, deserialize_with = "lenient_text")]
    pub available: Option<String>,
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

/// A priority as a plain number; any other JSON type becomes `None`.
fn lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
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

/// The most bluez5 devices kept, and the most profiles kept of each (quick Bluetooth spec § 4).
pub const MAX_BT_DEVICES: usize = 8;
pub const MAX_PROFILES: usize = 32;

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
pub(crate) fn drawable(s: &str) -> String {
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
fn graph_node(class: &str, running: bool, p: &PrivacyProps) -> GraphNode {
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
    /// The device it belongs to (`device.id`).
    device: Option<u32>,
}

/// A bluez5 device as the headset row reads it: what a person reads, its profiles, the active one.
#[derive(Debug, Default)]
struct BtDevice {
    label: Option<String>,
    profiles: Vec<BtProfile>,
    active: Option<u32>,
}

/// One profile: its index (what `wpctl set-profile` takes), the mode its name says (`a2dp-sink*`:
/// Hi-Fi, `headset-head-unit*`: Mic; anything else, `off` and `bap-sink` among them, neither), its
/// priority and whether it is available.
#[derive(Debug)]
struct BtProfile {
    index: u32,
    mode: Option<HeadsetMode>,
    priority: i64,
    available: bool,
}

impl BtProfile {
    fn new(p: &ProfileParam) -> Option<BtProfile> {
        let name = p.name.as_deref()?;
        let mode = if name.starts_with("a2dp-sink") {
            Some(HeadsetMode::HiFi)
        } else if name.starts_with("headset-head-unit") {
            Some(HeadsetMode::Mic)
        } else {
            None
        };
        Some(BtProfile { index: p.index?, mode, priority: p.priority.unwrap_or(i64::MIN), available: p.available.as_deref() != Some("no") })
    }
}

/// The highest-priority available profile of `mode`, the lowest index on a tie.
fn pick(profiles: &[BtProfile], mode: HeadsetMode) -> Option<u32> {
    profiles
        .iter()
        .filter(|p| p.mode == Some(mode) && p.available)
        .min_by_key(|p| (std::cmp::Reverse(p.priority), p.index))
        .map(|p| p.index)
}

#[derive(Debug, Default)]
pub struct Model {
    /// Sinks and sources: id → kind, node.name, label, volume once known.
    nodes: HashMap<u32, Node>,
    /// Id of the `default` metadata object, and the default sink's and source's node.name.
    default_meta: Option<u32>,
    default_sink: Option<Named>,
    default_source: Option<Named>,
    /// The bluez5 devices (device id → profiles), bounded (`MAX_BT_DEVICES`, `MAX_PROFILES`).
    bt: HashMap<u32, BtDevice>,
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
            self.apply_one(o);
        }
    }

    /// `apply` for a model built for privacy: the capture graph first (spec § 2), then the same
    /// sinks, sources and defaults as `apply`.
    pub fn apply_privacy(&mut self, batch: Vec<PrivacyObject>) {
        for o in batch {
            let PrivacyObject { id, kind, info, props, metadata } = o;
            let info = info.map(|i| {
                if kind.is_some() {
                    self.track(id, kind.as_deref(), &i);
                }
                let PrivacyProps { media_class, node_name, description, nick, device_api, device_description, device_id, .. } = i.props;
                Info {
                    props: NodeProps { media_class, node_name, description, nick, device_api, device_description, device_id },
                    params: i.params,
                }
            });
            self.apply_one(Object { id, kind, info, props, metadata });
        }
    }

    fn apply_one(&mut self, o: Object) {
        if o.kind.is_none() {
            self.remove(o.id);
            self.graph.remove(&o.id);
            self.links.remove(&o.id);
            self.bt.remove(&o.id);
            if self.default_meta == Some(o.id) {
                self.default_meta = None;
                self.default_sink = None;
                self.default_source = None;
            }
            return;
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
            return;
        }
        let Some(info) = o.info else { return };
        if o.kind.as_deref() == Some("PipeWire:Interface:Device") {
            self.device(o.id, info);
            return;
        }
        let Some(kind) = kind_of(info.props.media_class.as_deref()) else {
            self.remove(o.id);
            return;
        };
        let known = self.nodes.get(&o.id);
        // A re-dump without params keeps the last known volume; one without a label, the label.
        let volume = info.params.props.iter().find_map(to_volume).or(known.and_then(|n| n.volume));
        let label = label_of(&info.props).or_else(|| known.map(|n| n.label.clone()));
        let device = info.props.device_id.or(known.and_then(|n| n.device));
        let name = info.props.node_name.unwrap_or_default();
        let label = label.unwrap_or_else(|| drawable(&name));
        self.nodes.insert(o.id, Node { kind, name, label, volume, device });
    }

    /// A device object: kept when bluez5 (or known already and re-dumped without its API), its
    /// description and profiles as far as this dump carries them; another API drops it.
    fn device(&mut self, id: u32, info: Info) {
        let Info { props, params } = info;
        match props.device_api.as_deref() {
            Some("bluez5") => {}
            Some(_) => {
                self.bt.remove(&id);
                return;
            }
            None if self.bt.contains_key(&id) => {}
            None => return,
        }
        if !self.bt.contains_key(&id) && self.bt.len() >= MAX_BT_DEVICES {
            return;
        }
        let d = self.bt.entry(id).or_default();
        if let Some(label) = props.device_description.as_deref().map(drawable).filter(|l| !l.is_empty()) {
            d.label = Some(label);
        }
        if !params.enum_profile.is_empty() {
            d.profiles = params.enum_profile.iter().filter_map(BtProfile::new).take(MAX_PROFILES).collect();
        }
        if let Some(active) = params.profile.iter().find_map(|p| p.index) {
            d.active = Some(active);
        }
    }

    /// The default sink's bluez5 device when it offers both kinds of profile and is on one of them
    /// (quick Bluetooth spec § 4): the highest-priority available profile of each kind, by name
    /// prefix (`bap-sink` is neither).
    pub fn headset(&self) -> Option<Headset> {
        let named = self.default_sink.as_ref().filter(|n| !n.gone)?;
        let node = self.resolve(Kind::Sink, &named.name)?;
        let device = node.device?;
        let d = self.bt.get(&device)?;
        let (hifi, mic) = (pick(&d.profiles, HeadsetMode::HiFi)?, pick(&d.profiles, HeadsetMode::Mic)?);
        let active = d.profiles.iter().find(|p| Some(p.index) == d.active)?.mode?;
        Some(Headset { device, label: d.label.clone().unwrap_or_else(|| node.label.clone()), active, hifi, mic })
    }

    /// Keeps what the privacy rules read of one object (spec § 2): a link, or a node of a class
    /// they use. A node re-dumped without its props (only its state moved) keeps the last props.
    fn track(&mut self, id: u32, kind: Option<&str>, info: &PrivacyInfo) {
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
        // A node re-dumped with no state keeps the last one, as a link does.
        let running = info.state.as_deref().map(|s| s == "running").or_else(|| self.graph.get(&id).map(|n| n.running)).unwrap_or(false);
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
        osd && (scope != Scope::Panel || (s.sinks.as_ref().is_some_and(|l| *l == self.sinks()) && s.headset == self.headset()))
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
            headset: None,
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
        let panel = scope == Scope::Panel;
        SoundState { sinks: panel.then(|| self.sinks()), headset: if panel { self.headset() } else { None }, ..self.state() }
    }

    /// Sinks and sources kept in memory (tests: nothing else of the dump is).
    pub fn nodes(&self) -> usize {
        self.nodes.len()
    }

    /// bluez5 devices kept (tests).
    #[cfg(test)]
    fn bt_devices(&self) -> usize {
        self.bt.len()
    }

    /// Profiles kept of one bluez5 device (tests).
    #[cfg(test)]
    fn profiles(&self, device: u32) -> usize {
        self.bt.get(&device).map_or(0, |d| d.profiles.len())
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
            last.headset = None;
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
    let privacy = {
        let mut t = tap.lock();
        t.model = Model::new(t.privacy);
        t.last = None;
        t.privacy
    };
    let _forget = Forget(tap);
    // Without the module the lean `Object` is parsed: nothing more (privacy spec § 8).
    if privacy {
        follow_batches::<PrivacyObject>(output, base, tap, publish, Model::apply_privacy)
    } else {
        follow_batches::<Object>(output, base, tap, publish, Model::apply)
    }
}

fn follow_batches<T: serde::de::DeserializeOwned>(
    output: impl Read,
    base: Scope,
    tap: &SoundTap,
    publish: &mut dyn FnMut(SoundState) -> bool,
    apply: fn(&mut Model, Vec<T>),
) -> io::Result<()> {
    for batch in serde_json::Deserializer::from_reader(io::BufReader::new(output)).into_iter::<Vec<T>>() {
        let batch = batch.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("pw-dump: {e}")))?;
        let state = {
            let mut t = tap.lock();
            let Tapped { model, panel, last, .. } = &mut *t;
            apply(model, batch);
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
mod tests;
