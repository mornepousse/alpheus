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

pub mod app;
pub mod camera;
pub mod camera_app;

#[cfg(test)]
mod tests;
