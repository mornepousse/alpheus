//! The sound model: sinks, sources and the defaults the `default` metadata names, with their
//! volumes and labels.

use super::*;

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

/// The most sinks the quick panel lists.
pub const MAX_SINKS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Sink,
    Source,
}

/// Audio/Sink, Audio/Source and Audio/Source/… (a virtual source: echo cancelling); streams and
/// everything else: None.
pub(super) fn kind_of(class: Option<&str>) -> Option<Kind> {
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

/// What a person reads: the description, else the nick (blank text is absent); None: neither.
pub(super) fn label_of(p: &NodeProps) -> Option<String> {
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

/// A default named by the `default` metadata: its node.name, the label shown while its node is not
/// in the dump yet, and whether its node went (then the default is gone, not pending).
#[derive(Debug)]
pub(super) struct Named {
    pub(super) name: String,
    pub(super) label: String,
    pub(super) gone: bool,
}

impl Named {
    pub(super) fn new(name: String) -> Named {
        Named { label: drawable(&name), name, gone: false }
    }
}

/// A new value for a default, from the metadata: the same name keeps what is known of it.
pub(super) fn rename(slot: &mut Option<Named>, name: Option<String>) {
    if slot.as_ref().map(|n| &n.name) != name.as_ref() {
        *slot = name.map(Named::new);
    }
}

/// A default as the state shows it: its volume, its node and label.
pub(super) type Seen<'a> = (Option<Volume>, Option<(&'a str, &'a str)>);

#[derive(Debug)]
pub(super) struct Node {
    pub(super) kind: Kind,
    pub(super) name: String,
    pub(super) label: String,
    pub(super) volume: Option<Volume>,
    /// The device it belongs to (`device.id`).
    pub(super) device: Option<u32>,
}

impl Model {
    pub fn apply(&mut self, batch: Vec<Object>) {
        for o in batch {
            self.apply_one(o);
        }
    }

    pub(super) fn apply_one(&mut self, o: Object) {
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

    /// A node removed (or no longer a sink or a source): a default it was is gone.
    pub(super) fn remove(&mut self, id: u32) {
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
    pub(super) fn resolve(&self, kind: Kind, name: &str) -> Option<&Node> {
        self.resolve_id(kind, name).and_then(|id| self.nodes.get(&id))
    }

    /// The id of the node of `kind` named `name`: the lowest id when two share it.
    fn resolve_id(&self, kind: Kind, name: &str) -> Option<u32> {
        self.nodes.iter().filter(|(_, n)| n.kind == kind && n.name == name).map(|(id, _)| *id).min()
    }

    /// The default of `kind` as the state shows it. Its node when in the dump; not there yet (the
    /// metadata came first): that node, its volume unknown, so a switch is seen as one; its node
    /// went: none.
    pub(super) fn seen(&self, kind: Kind) -> Seen<'_> {
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
        // scope.
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
}
