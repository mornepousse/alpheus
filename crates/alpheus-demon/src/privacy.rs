//! The privacy indicators' pure core (privacy spec §§ 2 and 4): the PipeWire graph the sound model
//! keeps for them, and the rules that turn it into "which apps hold the microphone, the camera,
//! the screen". Nothing here reads a file, starts a thread or runs a command.

use crate::grid::{Grid, Hue, Style, text_width};
use crate::list::{self, Boxed};
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

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
/// The popup's width in cells at most (privacy spec § 5); a longer name is cut.
pub const POPUP_WIDTH: u16 = 32;

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
    pub(crate) fn hold(&mut self, app: Option<&str>) {
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

    /// The union of both: names through `hold` (sorted, once, capped), `unnamed` OR'ed.
    pub fn merged(&self, other: &Indicator) -> Indicator {
        let mut m = self.clone();

        m.unnamed |= other.unnamed;
        for n in &other.names {
            m.hold(Some(n));
        }
        m
    }

    /// The same without the ignored names, compared case-insensitively.
    fn without(&self, ignore: &[String]) -> Indicator {
        let names = self
            .names
            .iter()
            .filter(|n| {
                let n = n.to_lowercase();
                !ignore.iter().any(|g| g.to_lowercase() == n)
            })
            .cloned()
            .collect();
        Indicator { names, unnamed: self.unnamed }
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
        Capture { mic: self.mic.without(ignore), camera: self.camera.without(ignore), screen: self.screen.without(ignore) }
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

/// The bar's merge of what PipeWire and the compositor say (spec §§ 3 and 4): the ignore list,
/// the compositor's release hold, the off-delay, and the default source's mute. Time is passed in,
/// never read.
#[derive(Debug, Clone)]
pub struct Privacy {
    ignore: Vec<String>,
    /// PipeWire's last capture, the ignored names already removed.
    pw: Capture,
    mic_muted: bool,
    /// The holders of the video devices, the ignored names already removed (privacy delivery 2).
    devices: Indicator,
    /// The compositor says a screen is being cast.
    casting: bool,
    /// After a release: the instant it counts, when the hold ends.
    release_at: Option<Instant>,
    shown: Capture,
    /// Per indicator (mic, camera, screen): when it goes off, once it is no longer live.
    off_at: [Option<Instant>; 3],
}

impl Privacy {
    pub fn new(ignore: Vec<String>) -> Privacy {
        Privacy {
            ignore,
            pw: Capture::default(),
            devices: Indicator::default(),
            mic_muted: false,
            casting: false,
            release_at: None,
            shown: Capture::default(),
            off_at: [None; 3],
        }
    }

    /// PipeWire's capture (`None`: the sound source is down, counted as nothing captured) and the
    /// default source's mute. True when what is shown changed.
    pub fn set_sound(&mut self, capture: Option<&Capture>, mic_muted: bool, now: Instant) -> bool {
        self.expire_cast(now);
        let pw = match (capture, self.ignore.is_empty()) {
            (Some(c), true) => c.clone(),
            (Some(c), false) => c.without(&self.ignore),
            (None, _) => Capture::default(),
        };
        let muted_flipped = self.mic_muted != mic_muted;
        if pw == self.pw && !muted_flipped {
            return false;
        }
        self.pw = pw;
        self.mic_muted = mic_muted;
        self.settle(now) || muted_flipped
    }

    /// The holders of the video devices, as a scan found them. True when what is shown changed.
    pub fn set_devices(&mut self, camera: Indicator, now: Instant) -> bool {
        self.expire_cast(now);
        let devices = camera.without(&self.ignore);

        if devices == self.devices {
            return false;
        }
        self.devices = devices;
        self.settle(now)
    }

    /// The compositor's casting, and how long its release still counts. True when what is shown
    /// changed.
    pub fn set_casting(&mut self, casting: bool, hold: Duration, now: Instant) -> bool {
        self.expire_cast(now);
        let mut changed = false;
        if casting {
            if !self.casting || self.release_at.is_some() {
                self.casting = true;
                self.release_at = None;
                changed |= self.settle(now);
            }
        } else if self.casting {
            self.casting = false;
            self.release_at = Some(now + hold);
            self.expire_cast(now);
            changed |= self.settle(now);
        }
        changed
    }

    /// The earliest instant `due` has something to apply.
    pub fn deadline(&self) -> Option<Instant> {
        let release = self.release_at.filter(|_| self.shown.screen.active() && !self.pw.screen.active()).map(|r| r + OFF_DELAY);
        self.off_at.iter().flatten().copied().chain(release).min()
    }

    /// Applies the offs that are due at `now`. True when what is shown changed.
    pub fn due(&mut self, now: Instant) -> bool {
        self.expire_cast(now);
        let mut changed = false;
        for i in 0..3 {
            if self.off_at[i].is_some_and(|t| t <= now) {
                self.off_at[i] = None;
                let live = self.live(i).into_owned();
                if *self.slot(i) != live {
                    *self.slot_mut(i) = live;
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn shown(&self) -> &Capture {
        &self.shown
    }

    pub fn mic_muted(&self) -> bool {
        self.mic_muted
    }

    /// The popup (privacy spec § 5), drawn inside `list::boxed`'s ring as the power menu is: one
    /// heading per shown indicator in the module's order, its glyph in its hue (the microphone dim
    /// while the default source is muted, as in the bar), then each app indented 4 cells, or
    /// `(unnamed)`. As wide as the longest row and a blank cell on each side, at most
    /// `POPUP_WIDTH`. Nothing on it is selectable.
    pub fn render(&self) -> Boxed {
        let mic = if self.mic_muted { Style::MUTED } else { Style::hue(Hue::Orange) };
        let mut lines: Vec<(Option<(char, Style)>, String)> = Vec::new();
        for (i, (glyph, style, word)) in
            [(MIC, mic, "Microphone"), (CAMERA, Style::hue(Hue::Green), "Camera"), (SCREEN, Style::hue(Hue::Red), "Screen")]
                .into_iter()
                .enumerate()
        {
            let shown = self.slot(i);
            if !shown.active() {
                continue;
            }
            lines.push((Some((glyph, style)), format!(" {word}")));
            for name in &shown.names {
                lines.push((None, format!("    {name}")));
            }
            if shown.names.is_empty() {
                lines.push((None, "    (unnamed)".into()));
            }
        }
        let width = |(g, t): &(Option<(char, Style)>, String)| text_width(t) + g.map_or(0, |(c, _)| text_width(&c.to_string()));
        let longest = lines.iter().map(width).max().unwrap_or(0);
        let columns = longest.saturating_add(2).min(POPUP_WIDTH) + 2;
        let mut out = vec![Grid::empty(columns)];
        for (glyph, text) in &lines {
            let mut g = Grid::empty(columns);
            let mut x = 2;
            if let Some((c, style)) = glyph {
                x = g.write(x, columns - 1, &c.to_string(), *style);
            }
            g.write(x, columns - 1, text, Style::NORMAL);
            out.push(g);
        }
        out.push(Grid::empty(columns));
        list::boxed(&out, &[], None)
    }

    /// A compositor release whose hold has ended stops counting: the screen, if PipeWire does not
    /// hold it, goes off one delay after the release itself (only a pending off is set).
    fn expire_cast(&mut self, now: Instant) {
        if let Some(r) = self.release_at.filter(|r| *r <= now) {
            self.release_at = None;
            if !self.pw.screen.active() && self.shown.screen.active() {
                self.off_at[2].get_or_insert(r + OFF_DELAY);
            }
        }
    }

    /// What indicator `i` is right now, before the delay.
    fn live(&self, i: usize) -> Cow<'_, Indicator> {
        let pw = match i {
            0 => &self.pw.mic,
            1 => &self.pw.camera,
            _ => &self.pw.screen,
        };
        if i == 2 && !pw.active() && (self.casting || self.release_at.is_some()) {
            Cow::Owned(Indicator { names: Vec::new(), unnamed: true })
        } else if i == 1 && self.devices.active() {
            Cow::Owned(pw.merged(&self.devices))
        } else {
            Cow::Borrowed(pw)
        }
    }

    fn slot(&self, i: usize) -> &Indicator {
        match i {
            0 => &self.shown.mic,
            1 => &self.shown.camera,
            _ => &self.shown.screen,
        }
    }

    fn slot_mut(&mut self, i: usize) -> &mut Indicator {
        match i {
            0 => &mut self.shown.mic,
            1 => &mut self.shown.camera,
            _ => &mut self.shown.screen,
        }
    }

    /// An indicator live is shown at once; one shown and no longer live goes off one delay from
    /// now, unless a pending off already counts.
    fn settle(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for i in 0..3 {
            let live = self.live(i);
            let (active, differs) = (live.active(), *self.slot(i) != *live);
            let update = (active && differs).then(|| live.into_owned());
            if active {
                self.off_at[i] = None;
                if let Some(live) = update {
                    *self.slot_mut(i) = live;
                    changed = true;
                }
            } else if self.slot(i).active() {
                self.off_at[i].get_or_insert(now + OFF_DELAY);
            }
        }
        changed
    }
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

    fn mic_of(names: &[&str]) -> Capture {
        Capture { mic: one(names, false), ..Capture::default() }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn lighting_up_is_immediate_and_going_off_waits_the_delay() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        assert!(p.set_sound(Some(&mic_of(&["firefox"])), false, t0));
        assert!(p.shown().mic.active());
        assert_eq!(p.deadline(), None);
        let t1 = t0 + Duration::from_secs(1);
        assert!(!p.set_sound(Some(&Capture::default()), false, t1), "still shown");
        assert!(p.shown().mic.active());
        assert_eq!(p.deadline(), Some(t1 + OFF_DELAY));
        assert!(!p.due(t1 + ms(400)));
        assert!(p.shown().mic.active());
        assert!(p.due(t1 + OFF_DELAY));
        assert!(p.shown().is_empty());
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn a_later_setter_never_pushes_the_deadline_back() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_sound(Some(&mic_of(&["firefox"])), false, t0);
        p.set_sound(Some(&Capture::default()), false, t0 + ms(100));
        p.set_sound(Some(&Capture::default()), true, t0 + ms(300)); // a mute flip settles again
        p.set_casting(true, Duration::ZERO, t0 + ms(350));
        p.set_casting(false, Duration::ZERO, t0 + ms(360));
        p.set_sound(Some(&Capture::default()), false, t0 + ms(400));
        assert_eq!(p.deadline(), Some(t0 + ms(100) + OFF_DELAY));
    }

    #[test]
    fn coming_back_within_the_delay_never_blinks() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_sound(Some(&mic_of(&["firefox"])), false, t0);
        p.set_sound(Some(&Capture::default()), false, t0 + ms(100));
        assert!(!p.shown().is_empty());
        p.set_sound(Some(&mic_of(&["firefox"])), false, t0 + ms(400));
        assert!(!p.shown().is_empty());
        assert_eq!(p.deadline(), None, "the pending off is cleared");
        assert!(!p.due(t0 + Duration::from_secs(5)));
        assert!(p.shown().mic.active());
    }

    #[test]
    fn hyprland_releases_count_after_the_hold() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        assert!(p.set_casting(true, CAST_HOLD, t0));
        assert!(p.shown().screen.unnamed && p.shown().screen.names.is_empty());
        assert_eq!(p.deadline(), None);
        let r = t0 + ms(500);
        assert!(!p.set_casting(false, CAST_HOLD, r));
        assert!(p.shown().screen.active());
        assert_eq!(p.deadline(), Some(r + CAST_HOLD + OFF_DELAY));
        assert!(!p.due(r + CAST_HOLD + ms(200)), "the hold ended 200 ms ago: the delay counts from the release");
        assert!(p.shown().screen.active());
        assert_eq!(p.deadline(), Some(r + CAST_HOLD + OFF_DELAY));
        assert!(!p.due(r + CAST_HOLD + OFF_DELAY - ms(1)));
        assert!(p.due(r + CAST_HOLD + OFF_DELAY));
        assert!(p.shown().is_empty());
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn a_cast_coming_back_during_the_hold_never_blinks() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_casting(true, CAST_HOLD, t0);
        p.set_casting(false, CAST_HOLD, t0 + ms(100));
        p.set_casting(true, CAST_HOLD, t0 + ms(600));
        assert_eq!(p.deadline(), None);
        assert!(!p.due(t0 + Duration::from_secs(9)));
        assert!(p.shown().screen.active());
    }

    #[test]
    fn a_zero_hold_goes_off_at_release_plus_the_delay() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_casting(true, Duration::ZERO, t0);
        p.set_casting(false, Duration::ZERO, t0 + ms(200));
        assert_eq!(p.deadline(), Some(t0 + ms(200) + OFF_DELAY));
        assert!(p.due(t0 + ms(200) + OFF_DELAY));
        assert!(p.shown().is_empty());
    }

    #[test]
    fn the_screen_merges_pipewire_and_the_compositor() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        let named = Capture { screen: one(&["firefox"], false), ..Capture::default() };
        p.set_sound(Some(&named), false, t0);
        p.set_casting(true, CAST_HOLD, t0);
        assert_eq!(p.shown().screen, one(&["firefox"], false));
        let mut q = Privacy::new(Vec::new());
        q.set_casting(true, CAST_HOLD, t0);
        assert_eq!(q.shown().screen, one(&[], true));
        // PipeWire's names appear when its graph catches up with the compositor.
        q.set_sound(Some(&named), false, t0 + ms(100));
        assert_eq!(q.shown().screen, one(&["firefox"], false));
    }

    #[test]
    fn ignore_applies_before_the_delay() {
        let t0 = Instant::now();
        let mut p = Privacy::new(vec!["obs".into()]);
        assert!(!p.set_sound(Some(&mic_of(&["obs"])), false, t0));
        assert!(p.shown().is_empty());
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn the_microphone_dims_with_the_default_source_mute() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        assert!(!p.mic_muted());
        assert!(p.set_sound(Some(&mic_of(&["firefox"])), true, t0));
        assert!(p.mic_muted());
        assert!(p.set_sound(Some(&mic_of(&["firefox"])), false, t0 + ms(10)), "the mute alone changes it");
        assert!(!p.mic_muted());
        assert!(!p.set_sound(Some(&mic_of(&["firefox"])), false, t0 + ms(20)), "nothing changed");
    }

    /// Privacy spec § 5: one heading per active indicator in the module's order, its glyph in its
    /// hue, then one row per app indented 4 cells, or `(unnamed)`; the box as wide as the longest
    /// row and a blank cell on each side, at most `POPUP_WIDTH`.
    #[test]
    fn the_popup_lists_each_indicator_and_its_apps() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_sound(Some(&mic_of(&["firefox"])), false, t0);
        p.set_casting(true, CAST_HOLD, t0);
        let b = p.render();
        let want = [format!("{MIC} Microphone"), "    firefox".into(), format!("{SCREEN} Screen"), "    (unnamed)".into()];
        let texts: Vec<String> = b.rows.iter().map(|g| g.text().trim_end().to_string()).collect();
        assert_eq!(texts, want.iter().map(|r| format!(" {r}")).collect::<Vec<_>>());
        let longest = want.iter().map(|r| crate::grid::text_width(r)).max().unwrap_or(0);
        assert_eq!(b.cells(), (longest + 2, 4));
        let (mic, screen) = (b.rows[0].styles(), b.rows[2].styles());
        assert!(mic.starts_with(".o.") && screen.starts_with(".r."), "glyphs in their hues: {mic} {screen}");
        assert!(b.rows[1].styles().chars().all(|c| c == '.'), "names in the normal style");

        let long = "n".repeat(60);
        p.set_sound(Some(&mic_of(&[long.as_str()])), false, t0);
        assert_eq!(p.render().cells().0, POPUP_WIDTH, "capped");
        p.set_sound(Some(&mic_of(&["firefox"])), true, t0);
        assert!(p.render().rows[0].styles().starts_with(".m."), "a muted microphone dims, as in the bar");
    }

    #[test]
    fn none_means_down() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_sound(Some(&mic_of(&["firefox"])), false, t0);
        assert!(!p.set_sound(None, false, t0 + ms(100)));
        assert!(p.shown().mic.active());
        assert_eq!(p.deadline(), Some(t0 + ms(100) + OFF_DELAY));
        assert!(p.due(t0 + ms(100) + OFF_DELAY));
        assert!(p.shown().is_empty());
    }

    #[test]
    fn a_device_holder_lights_the_camera_at_once() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        assert!(p.set_devices(one(&["fakecam"], false), t0));
        assert_eq!(p.shown().camera.names, ["fakecam"]);
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn device_and_pipewire_names_merge_once() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        let pw = Capture { camera: one(&["recorder"], false), ..Capture::default() };
        p.set_sound(Some(&pw), false, t0);
        assert!(p.set_devices(one(&["fakecam", "recorder"], false), t0));
        assert_eq!(p.shown().camera.names, ["fakecam", "recorder"]);
    }

    #[test]
    fn a_device_holder_goes_through_the_off_delay() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_devices(one(&["fakecam"], false), t0);
        assert!(!p.set_devices(Indicator::default(), t0));
        assert_eq!(p.deadline(), Some(t0 + OFF_DELAY));
        assert!(!p.due(t0 + ms(499)));
        assert!(p.due(t0 + OFF_DELAY));
        assert!(!p.shown().camera.active());
    }

    #[test]
    fn ignore_drops_a_device_holder() {
        let t0 = Instant::now();
        let mut p = Privacy::new(vec!["FakeCam".into()]);
        assert!(!p.set_devices(one(&["fakecam"], false), t0));
        assert!(p.shown().is_empty());
    }

    #[test]
    fn a_device_back_within_the_delay_never_blinks() {
        let t0 = Instant::now();
        let mut p = Privacy::new(Vec::new());
        p.set_devices(one(&["fakecam"], false), t0);
        p.set_devices(Indicator::default(), t0);
        p.set_devices(one(&["fakecam"], false), t0 + ms(200));
        assert_eq!(p.deadline(), None);
        assert!(p.shown().camera.active());
    }
    #[test]
    fn merged_ors_unnamed_and_caps_the_names() {
        assert!(one(&["a"], false).merged(&one(&[], true)).unnamed);
        assert!(one(&[], true).merged(&one(&["a"], false)).unnamed);
        let many: Vec<String> = (0..12).map(|n| format!("app{n:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(one(&many[..6], false).merged(&one(&many[6..], false)).names.len(), MAX_NAMES);
    }
}
