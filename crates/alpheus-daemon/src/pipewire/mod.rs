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
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::{Mutex, MutexGuard, PoisonError};

mod bt;
mod follow;
mod graph;
mod sound;
mod tap;
mod wire;

use bt::BtDevice;
pub use bt::{MAX_BT_DEVICES, MAX_PROFILES};
pub use follow::{follow, follow_tapped};
pub(crate) use sound::drawable;
use sound::{Kind, Named, Node};
pub use sound::{LABEL_MAX, MAX_SINKS, to_volume};
pub use tap::SoundTap;
use tap::{Forget, Tapped};
pub use wire::*;

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
}

#[cfg(test)]
mod tests;
