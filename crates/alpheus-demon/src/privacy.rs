//! The privacy indicators' pure core (privacy spec §§ 2 and 4): the PipeWire graph the sound model
//! keeps for them, and the rules that turn it into "which apps hold the microphone, the camera,
//! the screen". Nothing here reads a file, starts a thread or runs a command.

use std::collections::HashMap;
use std::time::Duration;

// Material Design glyphs of the Nerd Font (md-microphone, md-camera, md-monitor_screenshot).
pub const MIC: char = '\u{f036c}';
pub const CAMERA: char = '\u{f0100}';
pub const SCREEN: char = '\u{f0379}';
/// An indicator going off stays shown this long, so a renegotiating stream never blinks (spec § 4).
pub const OFF_DELAY: Duration = Duration::from_millis(500);
/// A compositor's screencast release counts only after this hold (Hyprland's blip, spec § 3).
pub const CAST_HOLD: Duration = Duration::from_secs(1);
/// App names kept per indicator.
pub const MAX_NAMES: usize = 8;
/// Graph bounds (spec § 8): the model keeps no more.
pub const MAX_NODES: usize = 1024;
pub const MAX_LINKS: usize = 4096;

/// One indicator: the apps holding the device, and whether a holder has no name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Indicator {
    /// Sorted, without duplicates, at most `MAX_NAMES`.
    pub names: Vec<String>,
    pub unnamed: bool,
}

impl Indicator {
    pub fn active(&self) -> bool {
        !self.names.is_empty() || self.unnamed
    }

    /// Adds one holder: its name in sorted place (once), or the unnamed flag.
    fn hold(&mut self, app: Option<&str>) {
        let Some(app) = app else {
            self.unnamed = true;
            return;
        };
        if let Err(at) = self.names.binary_search_by(|n| n.as_str().cmp(app)) {
            if at < MAX_NAMES {
                self.names.insert(at, app.to_string());
                self.names.truncate(MAX_NAMES);
            }
        }
    }
}

/// What is captured right now, as the graph says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capture {
    pub mic: Indicator,
    pub camera: Indicator,
    pub screen: Indicator,
}

impl Capture {
    pub fn is_empty(&self) -> bool {
        !self.mic.active() && !self.camera.active() && !self.screen.active()
    }

    /// The same capture without the ignored app names, compared case-insensitively (spec § 7).
    pub fn without(&self, ignore: &[String]) -> Capture {
        let strip = |i: &Indicator| Indicator {
            names: i
                .names
                .iter()
                .filter(|n| {
                    let n = n.to_lowercase();
                    !ignore.iter().any(|g| g.to_lowercase() == n)
                })
                .cloned()
                .collect(),
            unnamed: i.unnamed,
        };
        Capture { mic: strip(&self.mic), camera: strip(&self.camera), screen: strip(&self.screen) }
    }
}

/// A node as the privacy rules need it. `class` is the raw `media.class`; the parser has already
/// resolved `camera_device` (a v4l2 or libcamera device, or the Camera role) and the app name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphNode {
    pub class: String,
    pub running: bool,
    /// `stream.monitor`.
    pub monitor: bool,
    /// `stream.capture.sink`.
    pub capture_sink: bool,
    /// A member of a `node.link-group`.
    pub link_group: bool,
    /// `node.passive`.
    pub passive: bool,
    pub camera_device: bool,
    pub app: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphLink {
    pub output: u32,
    pub input: u32,
    pub active: bool,
}

/// The classes the model keeps for privacy (spec § 2).
pub fn kept(class: &str) -> bool {
    audio_source(class) || matches!(class, "Stream/Input/Audio" | "Stream/Input/Video" | "Stream/Output/Video") || video_source(class)
}

fn audio_source(class: &str) -> bool {
    class == "Audio/Source" || class.starts_with("Audio/Source/")
}

fn video_source(class: &str) -> bool {
    class == "Video/Source" || class.starts_with("Video/Source/")
}

/// Which indicator a running producer node can light, if any.
enum Kind {
    Mic,
    Camera,
    Screen,
}

fn producer(n: &GraphNode) -> Option<Kind> {
    if audio_source(&n.class) {
        Some(Kind::Mic)
    } else if video_source(&n.class) {
        Some(if n.camera_device { Kind::Camera } else { Kind::Screen })
    } else if n.class == "Stream/Output/Video" {
        Some(Kind::Screen)
    } else {
        None
    }
}

/// Spec § 2: walks the links once, then adds the running producers no link was seen for.
pub fn capture(nodes: &HashMap<u32, GraphNode>, links: &HashMap<u32, GraphLink>) -> Capture {
    let mut out = Capture::default();
    // Running device sources and screen producers, until a link from them is seen.
    let mut alone: Vec<u32> =
        nodes.iter().filter(|(_, n)| n.running && matches!(producer(n), Some(Kind::Camera | Kind::Screen))).map(|(id, _)| *id).collect();
    for link in links.values() {
        let (Some(from), Some(to)) = (nodes.get(&link.output), nodes.get(&link.input)) else {
            continue;
        };
        alone.retain(|id| *id != link.output);
        let live = link.active || to.running;
        match producer(from) {
            Some(Kind::Mic) => {
                let recorder = to.class == "Stream/Input/Audio" && !(to.monitor || to.capture_sink || to.link_group || to.passive);
                if live && recorder {
                    out.mic.hold(to.app.as_deref());
                }
            }
            Some(Kind::Camera) => {
                if live && to.class == "Stream/Input/Video" {
                    out.camera.hold(to.app.as_deref());
                }
            }
            Some(Kind::Screen) if link.active || from.running => out.screen.hold(to.app.as_deref()),
            _ => {}
        }
    }
    for id in alone {
        match nodes.get(&id).and_then(producer) {
            Some(Kind::Camera) => out.camera.unnamed = true,
            Some(Kind::Screen) if !out.screen.active() => out.screen.unnamed = true,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(class: &str) -> GraphNode {
        GraphNode { class: class.into(), running: true, ..GraphNode::default() }
    }

    fn app(class: &str, name: &str) -> GraphNode {
        GraphNode { app: Some(name.into()), ..node(class) }
    }

    fn link(output: u32, input: u32, active: bool) -> GraphLink {
        GraphLink { output, input, active }
    }

    fn graph(nodes: Vec<(u32, GraphNode)>, links: Vec<GraphLink>) -> Capture {
        let nodes = nodes.into_iter().collect();
        let links = links.into_iter().enumerate().map(|(i, l)| (i as u32 + 100, l)).collect();
        capture(&nodes, &links)
    }

    fn one(names: &[&str], unnamed: bool) -> Indicator {
        Indicator { names: names.iter().map(|s| s.to_string()).collect(), unnamed }
    }

    #[test]
    fn the_microphone_is_a_source_linked_to_a_recording_stream() {
        for source in ["Audio/Source", "Audio/Source/Virtual"] {
            let c = graph(vec![(1, node(source)), (2, app("Stream/Input/Audio", "firefox"))], vec![link(1, 2, true)]);
            assert_eq!(c.mic, one(&["firefox"], false), "{source}");
            assert!(!c.camera.active() && !c.screen.active());
        }
    }

    #[test]
    fn meters_visualisers_filters_and_passive_streams_never_light_it() {
        let flags: [fn(&mut GraphNode); 4] =
            [|n| n.monitor = true, |n| n.capture_sink = true, |n| n.link_group = true, |n| n.passive = true];
        for set in flags {
            let mut consumer = app("Stream/Input/Audio", "pavucontrol");
            set(&mut consumer);
            let c = graph(vec![(1, node("Audio/Source")), (2, consumer)], vec![link(1, 2, true)]);
            assert!(!c.mic.active());
        }
        let c = graph(vec![(1, node("Audio/Source")), (2, app("Stream/Input/Audio/Foo", "x"))], vec![link(1, 2, true)]);
        assert!(!c.mic.active());
    }

    #[test]
    fn an_idle_link_lights_it_only_while_the_stream_runs() {
        let idle = |running| GraphNode { running, ..app("Stream/Input/Audio", "firefox") };
        let lit = |running, active| graph(vec![(1, node("Audio/Source")), (2, idle(running))], vec![link(1, 2, active)]).mic.active();
        assert!(!lit(false, false));
        assert!(lit(true, false));
        assert!(lit(false, true));
    }

    #[test]
    fn an_app_muted_stream_still_lights_the_microphone() {
        // The model has no per-stream mute: a running, linked stream holds the device.
        let mut consumer = app("Stream/Input/Audio", "firefox");
        consumer.passive = false;
        let c = graph(vec![(1, node("Audio/Source")), (2, consumer)], vec![link(1, 2, true)]);
        assert_eq!(c.mic, one(&["firefox"], false));
    }

    #[test]
    fn a_camera_is_a_device_video_source_linked_to_a_video_stream() {
        let cam = GraphNode { camera_device: true, ..node("Video/Source") };
        let c = graph(vec![(1, cam), (2, app("Stream/Input/Video", "org.chromium.Chromium"))], vec![link(1, 2, true)]);
        assert_eq!(c.camera, one(&["org.chromium.Chromium"], false));
        assert!(!c.screen.active());
    }

    #[test]
    fn a_running_camera_with_no_link_lights_it_unnamed() {
        let cam = GraphNode { camera_device: true, ..node("Video/Source") };
        let c = graph(vec![(1, cam.clone())], vec![]);
        assert_eq!(c.camera, one(&[], true));
        let idle = GraphNode { running: false, ..cam };
        assert!(!graph(vec![(1, idle)], vec![]).camera.active());
    }

    #[test]
    fn a_portal_or_niri_producer_is_the_screen() {
        let c = graph(vec![(1, node("Video/Source")), (2, app("Stream/Input/Video", "firefox"))], vec![link(1, 2, true)]);
        assert_eq!(c.screen, one(&["firefox"], false));
        assert!(!c.camera.active());
        let c = graph(vec![(1, node("Stream/Output/Video"))], vec![]);
        assert_eq!(c.screen, one(&[], true));
        let idle = GraphNode { running: false, ..node("Video/Source") };
        let c = graph(vec![(1, idle.clone()), (2, node("Stream/Input/Video"))], vec![link(1, 2, false)]);
        assert!(!c.screen.active());
        assert!(!graph(vec![(1, idle)], vec![]).screen.active());
    }

    #[test]
    fn names_are_deduplicated_sorted_and_capped() {
        let mut nodes = vec![(100, node("Audio/Source"))];
        let mut links = vec![];
        for i in (0..10u32).rev() {
            nodes.push((i + 1, app("Stream/Input/Audio", &format!("app{i}"))));
            links.push(link(100, i + 1, true));
        }
        nodes.push((50, app("Stream/Input/Audio", "app3")));
        links.push(link(100, 50, true));
        let c = graph(nodes.clone(), links.clone());
        let expect: Vec<String> = (0..8).map(|i| format!("app{i}")).collect();
        assert_eq!(c.mic.names, expect);
        assert!(!c.mic.unnamed);
        nodes.push((60, node("Stream/Input/Audio")));
        links.push(link(100, 60, true));
        let c = graph(nodes, links);
        assert_eq!(c.mic.names, expect);
        assert!(c.mic.unnamed);
    }

    #[test]
    fn without_drops_ignored_names_case_insensitively() {
        let c = Capture { mic: one(&["Firefox", "obs"], false), ..Capture::default() };
        let ignore = ["FIREFOX".to_string()];
        assert_eq!(c.without(&ignore).mic, one(&["obs"], false));
        let c = Capture { mic: one(&["Firefox"], false), ..Capture::default() };
        let stripped = c.without(&ignore);
        assert!(!stripped.mic.active() && stripped.is_empty());
        let c = Capture { mic: one(&["Firefox"], true), ..Capture::default() };
        assert!(c.without(&ignore).mic.active());
    }

    #[test]
    fn kept_classes() {
        for yes in
            ["Audio/Source", "Audio/Source/Virtual", "Stream/Input/Audio", "Stream/Input/Video", "Video/Source", "Stream/Output/Video"]
        {
            assert!(kept(yes), "{yes}");
        }
        for no in ["Audio/Sink", "Stream/Output/Audio", "Midi/Bridge"] {
            assert!(!kept(no), "{no}");
        }
    }
}
