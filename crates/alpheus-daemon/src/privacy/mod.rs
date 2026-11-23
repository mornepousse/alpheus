//! The privacy indicators' model: which apps hold the microphone, the camera or the screen, and what
//! the bar and the popup show of it.
//!
//! This is the pure half of the privacy module. It holds the PipeWire graph the sound model keeps
//! for it (`GraphNode`, `GraphLink`), the rules that turn that graph into who holds what
//! (`capture`), and `Privacy`, which merges what PipeWire, the video devices (`camera.rs`) and the
//! compositor say. `Privacy` also keeps the ignore list, the 0.5 s off-delay, the compositor's
//! release hold and the default source's mute, and it draws the popup (`render`). The other half,
//! `app.rs`, does the talking to the outside: it feeds `Privacy` the messages and the time and runs
//! the popup and the timer. `camera.rs` and `camera_app.rs` are the camera opened outside PipeWire.
//!
//! Never:
//! - read a clock, a file or a socket, start a thread or a process, or run a command: the caller
//!   always passes `now`, so a test can pick any time it likes;
//! - panic: no `unwrap`, `expect` or `panic!` in the production code of this file or of `app.rs`
//!   (the guardrail `the_privacy_module_has_no_panicking_path` checks both).
//!
//! See: docs/specs/2026-10-16-alpheus-privacy-design.md, docs/features/privacy.md, and
//! docs/decisions.md ("The microphone lights for a recording stream, never a meter or a filter",
//! "A muted app stream still lights the microphone", "A running camera with no link lights it
//! unnamed", "An indicator going off stays shown for half a second", "A compositor's screencast
//! release counts after a one-second hold", "A screen lit by the compositor alone reads
//! `(unnamed)`", "The privacy popup lists apps and selects nothing", "The camera is read from
//! `/proc` after a trigger, never counted").

use crate::grid::{Grid, Hue, Style, text_width};
use crate::list::{self, Boxed};
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

// The glyphs are Material Design icons of the Nerd Font: microphone, camera and monitor_screenshot.
pub const MIC: char = '\u{f036c}';
pub const CAMERA: char = '\u{f0100}';
pub const SCREEN: char = '\u{f0379}';
/// An indicator going off stays shown this long, so that a stream that renegotiates never blinks
/// (privacy spec § 4). See docs/decisions.md, "An indicator going off stays shown for half a
/// second".
pub const OFF_DELAY: Duration = Duration::from_millis(500);
/// A compositor's screencast release counts only after this hold: Hyprland sends a short blip for a
/// screenshot (privacy spec § 3). See docs/decisions.md, "A compositor's screencast release counts
/// after a one-second hold".
pub const CAST_HOLD: Duration = Duration::from_secs(1);
/// App names kept per indicator, at most (privacy spec § 2).
pub const MAX_NAMES: usize = 8;
/// The model keeps no more nodes and links than this (privacy spec § 8).
pub const MAX_NODES: usize = 1024;
pub const MAX_LINKS: usize = 4096;
/// The popup is at most this many cells wide, a longer name being cut (privacy plan 1, "Decisions
/// written down", item 6).
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

    /// Adds one holder: its name in its sorted place (once), or the unnamed flag.
    pub(crate) fn hold(&mut self, app: Option<&str>) {
        let Some(app) = app else {
            self.unnamed = true;
            return;
        };
        // `binary_search_by` gives `Err(at)` when the name is not there yet, with the place where it
        // belongs. A name that would land past `MAX_NAMES` is dropped.
        if let Err(at) = self.names.binary_search_by(|n| n.as_str().cmp(app))
            && at < MAX_NAMES
        {
            self.names.insert(at, app.to_string());
            self.names.truncate(MAX_NAMES);
        }
    }

    /// The union of both: the names through `hold` (sorted, once, capped), `unnamed` combined with
    /// "or".
    pub fn merged(&self, other: &Indicator) -> Indicator {
        let mut m = self.clone();
        m.unnamed |= other.unnamed;
        for n in &other.names {
            m.hold(Some(n));
        }
        m
    }

    /// The same indicator without the ignored names, which are compared whole and without regard to
    /// case. `unnamed` is kept.
    fn without(&self, ignore: &[String]) -> Indicator {
        let mut names = Vec::new();
        for name in &self.names {
            let lowered = name.to_lowercase();
            let mut ignored = false;
            for pattern in ignore {
                if pattern.to_lowercase() == lowered {
                    ignored = true;
                    break;
                }
            }
            if !ignored {
                names.push(name.clone());
            }
        }
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

    /// The same capture without the ignored app names, compared whole and without regard to case
    /// (privacy spec § 7).
    pub fn without(&self, ignore: &[String]) -> Capture {
        Capture { mic: self.mic.without(ignore), camera: self.camera.without(ignore), screen: self.screen.without(ignore) }
    }
}

/// A node as the privacy rules need it. `class` is the raw `media.class`; the parser has already
/// worked out `camera_device` (a v4l2 or libcamera device, or the Camera role) and the app name.
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

/// Whether the model keeps a node of this class for privacy (privacy spec § 2).
pub fn kept(class: &str) -> bool {
    audio_source(class) || matches!(class, "Stream/Input/Audio" | "Stream/Input/Video" | "Stream/Output/Video") || video_source(class)
}

fn audio_source(class: &str) -> bool {
    class == "Audio/Source" || class.starts_with("Audio/Source/")
}

fn video_source(class: &str) -> bool {
    class == "Video/Source" || class.starts_with("Video/Source/")
}

/// Which indicator a running producer node can light.
enum Kind {
    Mic,
    Camera,
    Screen,
}

/// The indicator a producer node can light, or None when the node is no producer. A video source
/// is the camera when it is a camera device, and the screen otherwise; a `Stream/Output/Video` (a
/// portal's or niri's screen cast) is the screen.
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

/// Who holds the microphone, the camera and the screen (privacy spec § 2). It walks the links once,
/// then adds the running producers that no link was seen for.
pub fn capture(nodes: &HashMap<u32, GraphNode>, links: &HashMap<u32, GraphLink>) -> Capture {
    let mut out = Capture::default();
    // The running camera and screen producers, until a link from them is seen: one that never gets
    // a link lights its indicator with no app name.
    let mut alone: Vec<u32> = Vec::new();
    for (id, node) in nodes {
        if node.running && matches!(producer(node), Some(Kind::Camera | Kind::Screen)) {
            alone.push(*id);
        }
    }
    for link in links.values() {
        // A link to or from a node the model does not keep is skipped.
        let (Some(from), Some(to)) = (nodes.get(&link.output), nodes.get(&link.input)) else {
            continue;
        };
        // `retain` keeps the ids for which the closure is true: this drops the producer.
        alone.retain(|id| *id != link.output);
        // A link counts when it is active or its consumer runs.
        let live = link.active || to.running;
        match producer(from) {
            Some(Kind::Mic) => {
                // Only a recording stream counts. Meters, visualisers, filters and passive streams
                // do not. See docs/decisions.md, "The microphone lights for a recording stream,
                // never a meter or a filter".
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
            // A screen producer counts when its link is active or the producer itself runs: a
            // portal's producer names no app, so the consumer's name is used (privacy spec § 2).
            Some(Kind::Screen) if link.active || from.running => out.screen.hold(to.app.as_deref()),
            _ => {}
        }
    }
    for id in alone {
        let kind = match nodes.get(&id) {
            Some(node) => producer(node),
            None => None,
        };
        match kind {
            // A running camera with no link lights it with no name. See docs/decisions.md, "A
            // running camera with no link lights it unnamed".
            Some(Kind::Camera) => out.camera.unnamed = true,
            // The same for a screen producer, but only while no screen holder was found by a link.
            Some(Kind::Screen) if !out.screen.active() => out.screen.unnamed = true,
            _ => {}
        }
    }
    out
}

/// The bar's merge of what PipeWire, the video devices and the compositor say (privacy spec §§ 3
/// and 4): the ignore list, the compositor's release hold, the off-delay and the default source's
/// mute. Time is passed in, never read.
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
    /// Per indicator (microphone, camera, screen): when it goes off, once it is no longer live.
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
        // `settle` is the left operand, so it always runs; a mute flip alone counts as a change.
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

    /// The earliest instant at which `due` has something to apply: a pending off, or the end of
    /// the off-delay that follows a compositor release (shown only while PipeWire does not hold the
    /// screen).
    pub fn deadline(&self) -> Option<Instant> {
        let mut earliest = None;
        for off in self.off_at.iter().flatten() {
            keep_earliest(&mut earliest, *off);
        }
        if let Some(release) = self.release_at
            && self.shown.screen.active()
            && !self.pw.screen.active()
        {
            keep_earliest(&mut earliest, release + OFF_DELAY);
        }
        earliest
    }

    /// Applies the offs that are due at `now`. True when what is shown changed.
    pub fn due(&mut self, now: Instant) -> bool {
        self.expire_cast(now);
        let mut changed = false;
        for i in 0..3 {
            if let Some(t) = self.off_at[i]
                && t <= now
            {
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

    /// The popup (privacy spec § 5), drawn inside `list::boxed`'s ring as the power menu is. It has
    /// one heading per shown indicator in the module's order, with its glyph in its hue (the
    /// microphone dim while the default source is muted, as in the bar), then each app indented
    /// 4 cells, or `(unnamed)`. It is as wide as its longest row plus a blank cell on each side, at
    /// most `POPUP_WIDTH`. Nothing on it can be selected (see docs/decisions.md, "The privacy popup
    /// lists apps and selects nothing").
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
        let mut longest = 0;
        for line in &lines {
            longest = longest.max(row_width(line));
        }
        // The rows are built inside a ring of one blank cell, which `list::boxed` removes.
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

    /// A compositor release whose hold has ended stops counting. The screen, if PipeWire does not
    /// hold it, then goes off one delay after the release itself (only a pending off is set).
    fn expire_cast(&mut self, now: Instant) {
        if let Some(r) = self.release_at
            && r <= now
        {
            self.release_at = None;
            if !self.pw.screen.active() && self.shown.screen.active() {
                // `get_or_insert` keeps an off already pending rather than push it back.
                self.off_at[2].get_or_insert(r + OFF_DELAY);
            }
        }
    }

    /// What indicator `i` (0 microphone, 1 camera, 2 screen) is right now, before the off-delay. The
    /// screen is lit, with no name, while the compositor casts (or its release is still on hold)
    /// and PipeWire names nobody. The camera is PipeWire's and the video devices' holders together.
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

    /// An indicator that is live is shown at once; one that is shown and no longer live goes off one
    /// delay from now, unless a pending off already counts. True when what is shown changed.
    fn settle(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for i in 0..3 {
            let live = self.live(i);
            let (active, differs) = (live.active(), *self.slot(i) != *live);
            // The live value is copied out only when it will be stored: it borrows `self`, which
            // `slot_mut` needs to change.
            let update = if active && differs { Some(live.into_owned()) } else { None };
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

/// `best` becomes `t` when `t` is earlier, or when `best` is still empty.
fn keep_earliest(best: &mut Option<Instant>, t: Instant) {
    match *best {
        Some(b) if b <= t => {}
        _ => *best = Some(t),
    }
}

/// The width in cells of one popup row: its text, and its glyph if it has one.
fn row_width(line: &(Option<(char, Style)>, String)) -> u16 {
    let (glyph, text) = line;
    match glyph {
        Some((c, _)) => text_width(text) + text_width(&c.to_string()),
        None => text_width(text),
    }
}

pub mod app;
pub mod camera;
pub mod camera_app;

#[cfg(test)]
mod tests;
