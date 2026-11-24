//! The media module's model: the MPRIS players, which one is active, and what the bar and the popup
//! show about it.
//!
//! MPRIS is the D-Bus interface media players use to say what they play and to be controlled.
//! This file is the pure half of the media module. It holds the players that are known (`Players`),
//! their status, track and abilities, the rule that picks the active one, the title the bar shows
//! (`view`, `display_title`, `truncate_title`) and the time the popup shows (`position_now`,
//! `clock`). `popup.rs` builds the popup's rows from it, `app.rs` is the wiring (the popup, its
//! timer, the clicks), and `session_bus/mpris.rs` talks to the bus and feeds this file.
//!
//! Bounded, so that a hostile player cannot grow memory: at most 16 players, strings
//! cleaned and cut at 256 characters, at most 8 artists.
//!
//! Never:
//! - name D-Bus: `session_bus/codec.rs` turns what a player sends into this file's `Value`;
//! - read the clock: the caller passes `now` as an `Instant`, so a test can choose any time;
//! - start a thread or a process, or write anything: the module never writes a player's volume,
//!   never seeks, and never raises or launches a player.
//!
//! See: docs/features/media.md, and docs/decisions.md
//! ("The active media player is the pinned one, else the best status", "One connection owning two
//! MPRIS names keeps the first", "The media popup computes the position, and reads it only while
//! open", "A player is labelled by its identity, else by its name", "A paused player's module is
//! fully dim", "Media titles are cut by display width, never inside a cluster").

use crate::grid::char_width;
use crate::text::clean;
use std::time::Instant;
use unicode_normalization::UnicodeNormalization;

/// Every MPRIS player's well-known name starts with this.
pub const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// Players tracked at most: a 17th is refused.
pub const MAX_PLAYERS: usize = 16;
/// `xesam:artist` entries kept.
pub const MAX_ARTISTS: usize = 8;
/// The fastest playback rate believed; a faster one is clamped to it.
pub const MAX_RATE: f64 = 16.0;
/// playerctld's own name after the prefix: a proxy that mirrors another player, always ignored.
const PLAYERCTLD: &str = "playerctld";
/// Zero-width joiner: it glues the characters on either side of it into one emoji, so a cut may
/// never fall next to it.
const ZWJ: char = '\u{200d}';

/// What a player is doing. `Stopped` hides the player from the bar and the popup.
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

    /// The active player's score: playing 2, paused 1, stopped never shown.
    fn score(self) -> u8 {
        match self {
            Status::Playing => 2,
            Status::Paused => 1,
            Status::Stopped => 0,
        }
    }
}

/// What a click or a key asks the active player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    PlayPause,
    Next,
    Previous,
}

/// A decoded D-Bus variant, built by `session_bus/codec.rs`, so that this file never names `dbus`.
/// An integer of any D-Bus width is an `Int`; a type this file has no use for is `Other`.
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

/// What a player says about the track it plays. Every text is cleaned and capped, and a blank one
/// is `None` (or left out of `artists`).
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
    /// An MPRIS `Metadata` map: `xesam:artist` as `as` or `s`,
    /// `mpris:length` as any integer or a float; every string cleaned and capped (`text::clean`), a
    /// blank one absent; unknown keys and unexpected types ignored (never an error).
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

/// What a player says it can do now (MPRIS `CanPlay`, `CanPause`, `CanGoNext`, `CanGoPrevious`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub play: bool,
    pub pause: bool,
    pub next: bool,
    pub previous: bool,
}

/// One MPRIS player and what is known about it.
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

    /// Whether the player says it can do `c` now: PlayPause needs `CanPause` while
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

/// A position read from a player at `at`: the popup counts on from it (`position_now`).
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
    /// Never queried: playerctld always (`playerctld`, or `playerctld.` followed by
    /// anything; `playerctldx` is another player), then any player whose bus name contains one of
    /// `ignore`'s substrings (an empty one matches nothing).
    pub fn is_ignored(bus_name: &str, ignore: &[String]) -> bool {
        // The name without `org.mpris.MediaPlayer2.`; a name without the prefix is taken whole.
        let own = match bus_name.strip_prefix(MPRIS_PREFIX) {
            Some(rest) => rest,
            None => bus_name,
        };
        if own == PLAYERCTLD {
            return true;
        }
        if let Some(rest) = own.strip_prefix(PLAYERCTLD)
            && rest.starts_with('.')
        {
            return true;
        }
        for substring in ignore {
            if !substring.is_empty() && bus_name.contains(substring.as_str()) {
                return true;
            }
        }
        false
    }

    /// A new player, Stopped until its properties arrive; false when MAX_PLAYERS are already
    /// tracked, or when that bus name or that unique name already is (one connection owning two
    /// MPRIS names keeps the first: signals carry only the unique name). See docs/decisions.md,
    /// "One connection owning two MPRIS names keeps the first".
    pub fn add(&mut self, bus_name: String, unique: String) -> bool {
        if self.list.len() >= MAX_PLAYERS {
            return false;
        }
        for known in &self.list {
            if known.bus_name == bus_name || known.unique == unique {
                return false;
            }
        }
        self.list.push(Player::new(bus_name, unique));
        true
    }

    /// Forgets the player of `unique`, and its pin; false when none had it.
    pub fn remove_unique(&mut self, unique: &str) -> bool {
        let Some(i) = self.list.iter().position(|p| p.unique == unique) else { return false };
        let gone = self.list.remove(i);
        // `as_deref` reads the pinned `Option<String>` as an `Option<&str>`, to compare it with a
        // `&str` without copying the name.
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
        // Taking the two fields out of `self` at once lets the borrow checker see that the player
        // (borrowed from `list`) and the counter `seq` are different things, so both can change.
        let Players { list, seq, .. } = self;
        let Some(p) = list.iter_mut().find(|p| p.unique == unique) else { return Applied { need: Need::None, visible: false } };
        // What was shown before the change, to say afterwards whether the status or an ability moved.
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
                    // A rate that is not a number, or is the one already known, changes nothing.
                    if let Some(r) = rate(v)
                        && r != p.rate
                    {
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
        // An invalidated `Metadata` comes as its name only: its value has to be asked for again.
        let need = if invalidated.iter().any(|k| k == "Metadata") { Need::Metadata(unique.to_string()) } else { Need::None };
        Applied { need, visible }
    }

    /// Records the position just read from the player of `unique`.
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
    /// latest status change. None when every player is Stopped. See docs/decisions.md, "The active
    /// media player is the pinned one, else the best status".
    pub fn active(&self) -> Option<&Player> {
        if let Some(pinned) = self.pinned.as_deref()
            && let Some(player) = self.shown().find(|p| p.bus_name == pinned)
        {
            return Some(player);
        }
        // The largest pair wins: the better status first, then the later change.
        self.shown().max_by_key(|p| (p.status.score(), p.changed))
    }

    /// The players not Stopped, in discovery order.
    pub fn shown(&self) -> impl Iterator<Item = &Player> {
        self.list.iter().filter(|p| p.status != Status::Stopped)
    }

    /// Whether the player of this well-known name is followed, whatever its status.
    pub fn has_name(&self, bus_name: &str) -> bool {
        self.list.iter().any(|p| p.bus_name == bus_name)
    }

    /// The player that owns the unique name `unique` (`:1.42`), whatever its status.
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

/// What the bar shows: `▶`/`⏸` and the title, already truncated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaView {
    pub playing: bool,
    pub title: String,
}

/// The bar's view of the active player; None when there is none (the module takes no cell).
pub fn view(players: &Players, title_max: u16) -> Option<MediaView> {
    // `?` after a call that gives None ends the function with None: no active player, no view.
    let p = players.active()?;
    let title = display_title(&p.track, p.identity.as_deref(), &p.bus_name);
    Some(MediaView { playing: p.status == Status::Playing, title: truncate_title(&title, title_max) })
}

/// The title, else the last path segment of the url (percent-decoded), else the player's name.
pub fn display_title(track: &Track, identity: Option<&str>, bus_name: &str) -> String {
    if let Some(title) = &track.title {
        return title.clone();
    }
    if let Some(url) = track.url.as_deref()
        && let Some(tail) = url_tail(url)
    {
        return tail;
    }
    player_name(identity, bus_name)
}

/// A player's label (the popup's list, the title's last fallback): its `Identity`, else the first
/// label after the MPRIS prefix ("vlc" for org.mpris.MediaPlayer2.vlc.instance4242). See
/// docs/decisions.md, "A player is labelled by its identity, else by its name".
pub fn player_name(identity: Option<&str>, bus_name: &str) -> String {
    if let Some(name) = identity
        && !name.is_empty()
    {
        return name.to_string();
    }
    let own = match bus_name.strip_prefix(MPRIS_PREFIX) {
        Some(rest) => rest,
        None => bus_name,
    };
    // `split('.')` always gives a first part (the whole text when it holds no dot).
    match own.split('.').next() {
        Some(first) => first.to_string(),
        None => own.to_string(),
    }
}

/// `s` cut to `max` cells, ending with "…" when cut: by display width (as `Grid::write` counts it),
/// never inside a cluster (a combining mark, a variation selector or a skin tone after its base, a
/// ZWJ sequence, a flag's two regional indicators). The text is first put in NFC form (composed
/// characters), as `Grid::write` does. The cut counts width the way the grid does and splits no
/// cluster; see
/// docs/decisions.md, "Media titles are cut by display width, never inside a cluster".
pub fn truncate_title(s: &str, max: u16) -> String {
    let chars: Vec<char> = s.nfc().collect();
    // Display width of a run of characters; `saturating_add` stops at the largest `u16` instead of
    // wrapping round.
    let width = |cs: &[char]| cs.iter().map(|c| char_width(*c)).fold(0u16, u16::saturating_add);
    if width(&chars) <= max {
        return chars.into_iter().collect();
    }
    // One cell is kept for the `…`; with `max` 0 there is no room for anything.
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
    // `start`: where the cluster being read begins. `regional`: how many regional indicators it holds.
    let (mut start, mut regional) = (0, 0usize);
    let mut prev: Option<char> = None;
    for (i, &c) in chars.iter().enumerate() {
        let joins = prev == Some(ZWJ) || extends(c) || (is_regional(c) && regional == 1);
        if i > 0 && !joins {
            if let Some(cluster) = chars.get(start..i) {
                out.push(cluster);
            }
            start = i;
            regional = 0;
        }
        if is_regional(c) {
            regional += 1;
        }
        prev = Some(c);
    }
    // The last cluster (none for an empty text).
    if let Some(cluster) = chars.get(start..)
        && !cluster.is_empty()
    {
        out.push(cluster);
    }
    out
}

/// Whether `c` joins the character before it into one cluster: it takes no cell, or it is a skin
/// tone modifier (U+1F3FB to U+1F3FF).
fn extends(c: char) -> bool {
    char_width(c) == 0 || matches!(c, '\u{1f3fb}'..='\u{1f3ff}')
}

/// Whether `c` is a regional indicator letter: two of them in a row make a flag.
fn is_regional(c: char) -> bool {
    matches!(c, '\u{1f1e6}'..='\u{1f1ff}')
}

/// Where the track is now: the last position read, plus the time since × Rate
/// while Playing, clamped to [0, length]. None when the position or the length is unknown. MPRIS
/// does not signal the position, so it is computed; see docs/decisions.md, "The media popup
/// computes the position, and reads it only while open".
pub fn position_now(p: &Player, now: Instant) -> Option<u64> {
    let (Some(sync), Some(length)) = (p.position, p.track.length_us) else { return None };
    let advanced = if p.status == Status::Playing {
        // Saturating: f64 → u64 clamps, and a NaN (never stored: `rate` refuses it) is 0.
        // `saturating_duration_since` is zero when `now` is before the sync (a clock behind it).
        (now.saturating_duration_since(sync.at).as_secs_f64() * 1e6 * p.rate) as u64
    } else {
        0
    };
    Some(sync.us.saturating_add(advanced).min(length))
}

/// "m:ss", "h:mm:ss" from one hour. Microseconds in, whole seconds shown (rounded down).
pub fn clock(us: u64) -> String {
    let s = us / 1_000_000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    // `{m:02}` pads to two digits.
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// A string from a player, cleaned and capped, its blanks trimmed; None when nothing is left that
/// takes a cell (combining marks or zero-width characters alone draw nothing).
fn clean_text(s: &str) -> Option<String> {
    let cleaned = clean(s);
    let trimmed = cleaned.trim();
    if trimmed.chars().any(|c| char_width(c) > 0) { Some(trimmed.to_string()) } else { None }
}

/// A string value, cleaned; None for a value of another type or a blank one.
fn text(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => clean_text(s),
        _ => None,
    }
}

/// The artists of `xesam:artist`: one string, or a list of at most `MAX_ARTISTS` non-blank ones.
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
        Value::Int(i) => match u64::try_from(i) {
            Ok(us) => us,
            // A negative length.
            Err(_) => return None,
        },
        // `as u64` stops at the largest value for a float too big, and a float with decimals loses them.
        Value::Float(f) if f.is_finite() && f >= 0.0 => f as u64,
        _ => return None,
    };
    if us > 0 { Some(us) } else { None }
}

/// `Rate`, clamped to 0.0..=MAX_RATE; NaN, infinite or not a number: None (the old rate stays).
fn rate(v: &Value) -> Option<f64> {
    let r = match *v {
        Value::Float(f) => f,
        Value::Int(i) => i as f64,
        _ => return None,
    };
    if r.is_finite() { Some(r.clamp(0.0, MAX_RATE)) } else { None }
}

/// The last path segment of a url, its query and fragment dropped, percent-decoded and cleaned;
/// None when nothing is left. Only the path counts, never the scheme nor the host ("file:///",
/// "https://host/" have none).
fn url_tail(url: &str) -> Option<String> {
    // Everything from the first `?` or `#` on is dropped (`split` always gives a first part).
    let url = match url.split(['?', '#']).next() {
        Some(before) => before,
        None => url,
    };
    let path = match url.split_once("://") {
        // After the scheme, the path starts at the first `/` (none: the url has no path).
        Some((_, rest)) => match rest.find('/') {
            Some(i) => &rest[i..],
            None => "",
        },
        None => url,
    };
    // The last segment, ignoring slashes at the end; `?` gives None if there were none.
    let tail = path.trim_end_matches('/').rsplit('/').next()?;
    clean_text(&percent_decode(tail))
}

/// `%XX` escapes decoded (invalid UTF-8 replaced); a lone or malformed `%` stays as it is.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match (b, hex_digit_at(bytes, i + 1), hex_digit_at(bytes, i + 2)) {
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

/// The value of the hexadecimal digit at `bytes[at]`; None past the end or for another character.
fn hex_digit_at(bytes: &[u8], at: usize) -> Option<u32> {
    match bytes.get(at) {
        Some(byte) => char::from(*byte).to_digit(16),
        None => None,
    }
}

pub mod app;
pub mod popup;

#[cfg(test)]
mod tests;
