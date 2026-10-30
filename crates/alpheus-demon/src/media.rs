//! The media module's model (media spec docs/specs/2026-10-08-alpheus-media-design.md): the MPRIS
//! players, which one is active, their track, and what the bar and the popup show. Pure: it never
//! names D-Bus (`tray/codec.rs` hands it `Value`s), starts nothing and never reads the clock (its
//! callers pass the `Instant`). Bounded: 16 players, cleaned strings of at most 256 characters,
//! 8 artists.

use crate::grid::char_width;
use crate::text::clean;
use std::time::Instant;
use unicode_normalization::UnicodeNormalization;

/// Every MPRIS player's well-known name starts with this.
pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// Players tracked at most (spec § 3): a 17th is refused.
pub const MAX_PLAYERS: usize = 16;
/// `xesam:artist` entries kept.
pub const MAX_ARTISTS: usize = 8;
/// The fastest playback rate believed; a faster one is clamped to it.
pub const MAX_RATE: f64 = 16.0;
/// playerctld's own name after the prefix: a proxy that mirrors another player, always ignored.
const PLAYERCTLD: &str = "playerctld";
const ZWJ: char = '\u{200d}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    /// MPRIS `PlaybackStatus`: "Playing" and "Paused"; anything else ("Stopped", an unknown word, a
    /// wrong case) is Stopped, which hides the player.
    pub fn parse(s: &str) -> Status {
        match s {
            "Playing" => Status::Playing,
            "Paused" => Status::Paused,
            _ => Status::Stopped,
        }
    }

    /// The active player's score (spec § 3): playing 2, paused 1, stopped never shown.
    fn score(self) -> u8 {
        match self {
            Status::Playing => 2,
            Status::Paused => 1,
            Status::Stopped => 0,
        }
    }
}

/// What a click or a key asks the active player (spec § 1, § 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    PlayPause,
    Next,
    Previous,
}

/// A decoded D-Bus variant, built by tray/codec.rs; media.rs never names `dbus`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    StrList(Vec<String>),
    Int(i64),
    Float(f64),
    Bool(bool),
    Map(Vec<(String, Value)>),
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Track {
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub url: Option<String>,
    /// None when unknown: absent, 0, negative or of an unexpected type (a stream).
    pub length_us: Option<u64>,
    pub id: Option<String>,
}

impl Track {
    /// An MPRIS `Metadata` map (spec § 3, Tolerance): `xesam:artist` as `as` or `s`, `mpris:length`
    /// as any integer or a float; every string cleaned and capped (`codec::clean`), a blank one
    /// absent; unknown keys and unexpected types ignored.
    pub fn from_metadata(entries: &[(String, Value)]) -> Track {
        let mut t = Track::default();
        for (key, value) in entries {
            match key.as_str() {
                "xesam:title" => t.title = text(value),
                "xesam:artist" => t.artists = artists(value),
                "xesam:url" => t.url = text(value),
                "mpris:length" => t.length_us = length(value),
                "mpris:trackid" => t.id = text(value),
                _ => {}
            }
        }
        t
    }

    /// The artists joined with ", " (the popup's second row); "" with none.
    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub play: bool,
    pub pause: bool,
    pub next: bool,
    pub previous: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    /// org.mpris.MediaPlayer2.spotify
    pub bus_name: String,
    /// :1.42
    pub unique: String,
    pub identity: Option<String>,
    pub status: Status,
    pub track: Track,
    pub caps: Caps,
    /// 1.0 by default; clamped to 0.0..=MAX_RATE.
    pub rate: f64,
    /// `Players::seq` at its last status change.
    pub changed: u64,
    /// The last position read (`Get Position`, `Seeked`); None after a status or track change until
    /// the popup resyncs it.
    pub position: Option<PositionSync>,
}

impl Player {
    fn new(bus_name: String, unique: String) -> Player {
        Player {
            bus_name,
            unique,
            identity: None,
            status: Status::Stopped,
            track: Track::default(),
            caps: Caps::default(),
            rate: 1.0,
            changed: 0,
            position: None,
        }
    }

    /// Whether the player says it can do `c` now (spec § 1): PlayPause needs `CanPause` while
    /// playing and `CanPlay` otherwise; a capability it lacks makes the action a no-op.
    pub fn can(&self, c: Control) -> bool {
        match c {
            Control::PlayPause if self.status == Status::Playing => self.caps.pause,
            Control::PlayPause => self.caps.play,
            Control::Next => self.caps.next,
            Control::Previous => self.caps.previous,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionSync {
    pub at: Instant,
    pub us: u64,
}

/// What a change asks the D-Bus side to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    None,
    /// `Metadata` came invalidated (its name only): re-read it with one `Get` from this unique name.
    Metadata(String),
}

/// What `Players::apply` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub need: Need,
    /// Something the bar or the popup shows moved: the status, the track or a capability. Never for
    /// Rate, Volume or a value repeated (the view is rebuilt only then).
    pub visible: bool,
}

/// The players known, in discovery order (the popup's order).
#[derive(Debug, Default)]
pub struct Players {
    list: Vec<Player>,
    /// A bus name chosen in the popup: active while it exists and is not Stopped.
    pinned: Option<String>,
    /// Counts status changes, so a tie goes to the latest.
    seq: u64,
}

impl Players {
    /// Never queried (spec § 3): playerctld always, then any player whose bus name contains one of
    /// `ignore`'s substrings (an empty one matches nothing).
    pub fn is_ignored(bus_name: &str, ignore: &[String]) -> bool {
        let own = bus_name.strip_prefix(MPRIS_PREFIX).unwrap_or(bus_name);
        own == PLAYERCTLD
            || own.strip_prefix(PLAYERCTLD).is_some_and(|rest| rest.starts_with('.'))
            || ignore.iter().any(|s| !s.is_empty() && bus_name.contains(s.as_str()))
    }

    /// A new player, Stopped until its properties arrive; false when MAX_PLAYERS are already
    /// tracked, or when that bus name or that unique name already is (one connection owning two
    /// MPRIS names keeps the first: signals carry only the unique name).
    pub fn add(&mut self, bus_name: String, unique: String) -> bool {
        if self.list.len() >= MAX_PLAYERS || self.list.iter().any(|p| p.bus_name == bus_name || p.unique == unique) {
            return false;
        }
        self.list.push(Player::new(bus_name, unique));
        true
    }

    /// Forgets the player of `unique`, and its pin; false when none had it.
    pub fn remove_unique(&mut self, unique: &str) -> bool {
        let Some(i) = self.list.iter().position(|p| p.unique == unique) else { return false };
        let gone = self.list.remove(i);
        if self.pinned.as_deref() == Some(gone.bus_name.as_str()) {
            self.pinned = None;
        }
        true
    }

    /// Its `Identity`, cleaned; a blank one is none.
    pub fn set_identity(&mut self, unique: &str, identity: &str) {
        if let Some(p) = self.list.iter_mut().find(|p| p.unique == unique) {
            p.identity = clean_text(identity);
        }
    }

    /// The Player interface's properties (`GetAll`, `PropertiesChanged`): `PlaybackStatus`,
    /// `Metadata`, `Rate`, `CanPlay`, `CanPause`, `CanGoNext`, `CanGoPrevious`; anything else, or a
    /// value of another type, is ignored (`Position` included: it comes from `set_position`). A
    /// status, track or rate change forgets the position. Need::Metadata when `Metadata` is
    /// invalidated.
    pub fn apply(&mut self, unique: &str, changed: &[(String, Value)], invalidated: &[String]) -> Applied {
        let Players { list, seq, .. } = self;
        let Some(p) = list.iter_mut().find(|p| p.unique == unique) else { return Applied { need: Need::None, visible: false } };
        let (status, caps) = (p.status, p.caps);
        let mut visible = false;
        for (key, value) in changed {
            match (key.as_str(), value) {
                ("PlaybackStatus", Value::Str(s)) => {
                    let status = Status::parse(s);
                    if status != p.status {
                        *seq += 1;
                        p.changed = *seq;
                        p.status = status;
                        p.position = None;
                    }
                }
                ("Metadata", Value::Map(entries)) => {
                    let track = Track::from_metadata(entries);
                    if track != p.track {
                        p.track = track;
                        p.position = None;
                        visible = true;
                    }
                }
                ("Rate", v) => {
                    if let Some(r) = rate(v).filter(|r| *r != p.rate) {
                        p.rate = r;
                        p.position = None;
                    }
                }
                ("CanPlay", Value::Bool(b)) => p.caps.play = *b,
                ("CanPause", Value::Bool(b)) => p.caps.pause = *b,
                ("CanGoNext", Value::Bool(b)) => p.caps.next = *b,
                ("CanGoPrevious", Value::Bool(b)) => p.caps.previous = *b,
                _ => {}
            }
        }
        let visible = visible || p.status != status || p.caps != caps;
        let need = if invalidated.iter().any(|k| k == "Metadata") { Need::Metadata(unique.to_string()) } else { Need::None };
        Applied { need, visible }
    }

    pub fn set_position(&mut self, unique: &str, sync: PositionSync) {
        if let Some(p) = self.list.iter_mut().find(|p| p.unique == unique) {
            p.position = Some(sync);
        }
    }

    /// Pins the player of `bus_name` (chosen in the popup); an unknown name changes nothing.
    pub fn pin(&mut self, bus_name: &str) {
        if self.list.iter().any(|p| p.bus_name == bus_name) {
            self.pinned = Some(bus_name.to_string());
        }
    }

    /// The pinned player while it is not Stopped; otherwise the best score, a tie going to the
    /// latest status change. None when every player is Stopped.
    pub fn active(&self) -> Option<&Player> {
        let pinned = self.pinned.as_deref().and_then(|b| self.shown().find(|p| p.bus_name == b));
        pinned.or_else(|| self.shown().max_by_key(|p| (p.status.score(), p.changed)))
    }

    /// The players not Stopped, in discovery order.
    pub fn shown(&self) -> impl Iterator<Item = &Player> {
        self.list.iter().filter(|p| p.status != Status::Stopped)
    }

    /// Whether the player of this well-known name is followed, whatever its status.
    pub fn has_name(&self, bus_name: &str) -> bool {
        self.list.iter().any(|p| p.bus_name == bus_name)
    }

    pub fn get(&self, unique: &str) -> Option<&Player> {
        self.list.iter().find(|p| p.unique == unique)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// What the bar shows (spec § 1): `▶`/`⏸` and the title, already truncated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaView {
    pub playing: bool,
    pub title: String,
}

/// The bar's view of the active player; None when there is none (the module takes no cell).
pub fn view(players: &Players, title_max: u16) -> Option<MediaView> {
    let p = players.active()?;
    let title = display_title(&p.track, p.identity.as_deref(), &p.bus_name);
    Some(MediaView { playing: p.status == Status::Playing, title: truncate_title(&title, title_max) })
}

/// The title, else the last path segment of the url (percent-decoded), else the player's name.
pub fn display_title(track: &Track, identity: Option<&str>, bus_name: &str) -> String {
    if let Some(title) = &track.title {
        return title.clone();
    }
    if let Some(tail) = track.url.as_deref().and_then(url_tail) {
        return tail;
    }
    player_name(identity, bus_name)
}

/// A player's label (the popup's list, the title's last fallback): its `Identity`, else the first
/// label after the MPRIS prefix ("vlc" for org.mpris.MediaPlayer2.vlc.instance4242).
pub fn player_name(identity: Option<&str>, bus_name: &str) -> String {
    if let Some(name) = identity.filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    let own = bus_name.strip_prefix(MPRIS_PREFIX).unwrap_or(bus_name);
    own.split('.').next().unwrap_or(own).to_string()
}

/// `s` cut to `max` cells, ending with "…" when cut: by display width (as `Grid::write` counts it),
/// never inside a cluster (a combining mark, a variation selector or a skin tone after its base, a
/// ZWJ sequence, a flag's two regional indicators).
pub fn truncate_title(s: &str, max: u16) -> String {
    let chars: Vec<char> = s.nfc().collect();
    let width = |cs: &[char]| cs.iter().map(|c| char_width(*c)).fold(0u16, u16::saturating_add);
    if width(&chars) <= max {
        return chars.into_iter().collect();
    }
    let Some(budget) = max.checked_sub(1) else { return String::new() };
    let mut out = String::new();
    let mut used = 0u16;
    for cluster in clusters(&chars) {
        let w = width(cluster);
        if used.saturating_add(w) > budget {
            break;
        }
        out.extend(cluster);
        used += w;
    }
    out.push('…');
    out
}

/// `chars` in clusters: a char joins the one before it when it takes no cell (combining marks,
/// variation selectors, ZWJ, tags), is a skin tone modifier, follows a ZWJ, or is the second
/// regional indicator of a flag.
fn clusters(chars: &[char]) -> Vec<&[char]> {
    let mut out = Vec::new();
    let (mut start, mut regional) = (0, 0usize);
    let mut prev: Option<char> = None;
    for (i, &c) in chars.iter().enumerate() {
        let joins = prev == Some(ZWJ) || extends(c) || (is_regional(c) && regional == 1);
        if i > 0 && !joins {
            out.extend(chars.get(start..i));
            start = i;
            regional = 0;
        }
        if is_regional(c) {
            regional += 1;
        }
        prev = Some(c);
    }
    out.extend(chars.get(start..).filter(|c| !c.is_empty()));
    out
}

fn extends(c: char) -> bool {
    char_width(c) == 0 || matches!(c, '\u{1f3fb}'..='\u{1f3ff}')
}

fn is_regional(c: char) -> bool {
    matches!(c, '\u{1f1e6}'..='\u{1f1ff}')
}

/// Where the track is now (spec § 2, Time): the last position read, plus the time since × Rate
/// while Playing, clamped to [0, length]. None when the position or the length is unknown.
pub fn position_now(p: &Player, now: Instant) -> Option<u64> {
    let (sync, length) = (p.position?, p.track.length_us?);
    let advanced = if p.status == Status::Playing {
        // Saturating: f64 → u64 clamps, and a NaN (never stored: `rate` refuses it) is 0.
        (now.saturating_duration_since(sync.at).as_secs_f64() * 1e6 * p.rate) as u64
    } else {
        0
    };
    Some(sync.us.saturating_add(advanced).min(length))
}

/// "m:ss", "h:mm:ss" from one hour.
pub fn clock(us: u64) -> String {
    let s = us / 1_000_000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// A string from a player, cleaned and capped, its blanks trimmed; None when nothing is left that
/// takes a cell (combining marks or zero-width characters alone draw nothing).
fn clean_text(s: &str) -> Option<String> {
    let c = clean(s);
    let t = c.trim();
    t.chars().any(|c| char_width(c) > 0).then(|| t.to_string())
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => clean_text(s),
        _ => None,
    }
}

fn artists(v: &Value) -> Vec<String> {
    match v {
        Value::Str(s) => clean_text(s).into_iter().collect(),
        Value::StrList(list) => list.iter().filter_map(|s| clean_text(s)).take(MAX_ARTISTS).collect(),
        _ => Vec::new(),
    }
}

/// `mpris:length` in µs: any integer type (`Int`) or a float; negative, 0, NaN or anything else
/// is unknown.
fn length(v: &Value) -> Option<u64> {
    let us = match *v {
        Value::Int(i) => u64::try_from(i).ok()?,
        Value::Float(f) if f.is_finite() && f >= 0.0 => f as u64,
        _ => return None,
    };
    (us > 0).then_some(us)
}

/// `Rate`, clamped to 0.0..=MAX_RATE; NaN, infinite or not a number: None (the old rate stays).
fn rate(v: &Value) -> Option<f64> {
    let r = match *v {
        Value::Float(f) => f,
        Value::Int(i) => i as f64,
        _ => return None,
    };
    r.is_finite().then(|| r.clamp(0.0, MAX_RATE))
}

/// The last path segment of a url, its query and fragment dropped, percent-decoded and cleaned;
/// None when nothing is left. Only the path counts, never the scheme nor the host ("file:///",
/// "https://host/" have none).
fn url_tail(url: &str) -> Option<String> {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |i| &rest[i..]),
        None => url,
    };
    let tail = path.trim_end_matches('/').rsplit('/').next()?;
    clean_text(&percent_decode(tail))
}

/// `%XX` escapes decoded (invalid UTF-8 replaced); a lone or malformed `%` stays as it is.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |o: usize| bytes.get(i + o).and_then(|h| char::from(*h).to_digit(16));
        match (b, hex(1), hex(2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests;
