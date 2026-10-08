//! The media module's model (media spec docs/specs/2026-10-08-alpheus-media-design.md): the MPRIS
//! players, which one is active, their track, and what the bar and the popup show. Pure: it never
//! names D-Bus (`tray/codec.rs` hands it `Value`s), starts nothing and never reads the clock (its
//! callers pass the `Instant`). Bounded: 16 players, cleaned strings of at most 256 characters,
//! 8 artists.

use crate::grid::char_width;
use crate::tray::codec::clean;
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// status or track change forgets the position. Need::Metadata when `Metadata` is invalidated.
    pub fn apply(&mut self, unique: &str, changed: &[(String, Value)], invalidated: &[String]) -> Need {
        let Players { list, seq, .. } = self;
        let Some(p) = list.iter_mut().find(|p| p.unique == unique) else { return Need::None };
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
                    }
                }
                ("Rate", v) => {
                    if let Some(r) = rate(v) {
                        p.rate = r;
                    }
                }
                ("CanPlay", Value::Bool(b)) => p.caps.play = *b,
                ("CanPause", Value::Bool(b)) => p.caps.pause = *b,
                ("CanGoNext", Value::Bool(b)) => p.caps.next = *b,
                ("CanGoPrevious", Value::Bool(b)) => p.caps.previous = *b,
                _ => {}
            }
        }
        if invalidated.iter().any(|k| k == "Metadata") { Need::Metadata(unique.to_string()) } else { Need::None }
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

/// A string from a player, cleaned and capped, its blanks trimmed; None when nothing is left.
fn clean_text(s: &str) -> Option<String> {
    let c = clean(s);
    let t = c.trim();
    (!t.is_empty()).then(|| t.to_string())
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
/// None when nothing is left.
fn url_tail(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
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
mod tests {
    use super::*;
    use crate::grid::text_width;
    use std::time::Duration;

    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    fn list(xs: &[&str]) -> Value {
        Value::StrList(xs.iter().map(|x| x.to_string()).collect())
    }

    fn props(entries: &[(&str, Value)]) -> Vec<(String, Value)> {
        entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn metadata(entries: &[(&str, Value)]) -> Track {
        Track::from_metadata(&props(entries))
    }

    /// Players with one player per (bus suffix, unique, status), added and set in that order.
    fn players(spec: &[(&str, &str, &str)]) -> Players {
        let mut p = Players::default();
        for (name, unique, status) in spec {
            assert!(p.add(format!("{MPRIS_PREFIX}{name}"), unique.to_string()));
            p.apply(unique, &props(&[("PlaybackStatus", s(status))]), &[]);
        }
        p
    }

    fn active(p: &Players) -> Option<&str> {
        p.active().map(|a| a.unique.as_str())
    }

    // ---- status and metadata (spec § 3, Tolerance and Bounds) ----

    #[test]
    fn status_reads_playing_and_paused_and_anything_else_is_stopped() {
        assert_eq!(Status::parse("Playing"), Status::Playing);
        assert_eq!(Status::parse("Paused"), Status::Paused);
        for other in ["Stopped", "playing", "Buffering", "", "Paused "] {
            assert_eq!(Status::parse(other), Status::Stopped, "{other:?}");
        }
    }

    /// Review focus: an unknown status string is Stopped, so the player is hidden.
    #[test]
    fn an_unknown_status_hides_the_player() {
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        assert_eq!(active(&p), Some(":1.4"));
        p.apply(":1.4", &props(&[("PlaybackStatus", s("Buffering"))]), &[]);
        assert_eq!(p.get(":1.4").map(|x| x.status), Some(Status::Stopped));
        assert_eq!(active(&p), None);
        assert_eq!(view(&p, 30), None, "the module takes no cell");
    }

    #[test]
    fn metadata_is_read_with_its_types_tolerated() {
        let t = metadata(&[
            ("xesam:title", s("Bohemian Rhapsody")),
            ("xesam:artist", list(&["Queen"])),
            ("xesam:url", s("file:///music/Queen/Bohemian%20Rhapsody.flac")),
            ("mpris:length", Value::Int(355_000_000)),
            ("mpris:trackid", s("/org/mpris/MediaPlayer2/Track/1")),
            ("xesam:album", s("A Night at the Opera")),
            ("mpris:artUrl", Value::Other),
        ]);
        assert_eq!(
            t,
            Track {
                title: Some("Bohemian Rhapsody".into()),
                artists: vec!["Queen".into()],
                url: Some("file:///music/Queen/Bohemian%20Rhapsody.flac".into()),
                length_us: Some(355_000_000),
                id: Some("/org/mpris/MediaPlayer2/Track/1".into()),
            }
        );
        assert_eq!(metadata(&[("xesam:artist", s("Queen"))]).artists, ["Queen"], "an artist sent as `s`");
        assert_eq!(metadata(&[("xesam:artist", list(&["Queen", "David Bowie"]))]).artist_line(), "Queen, David Bowie");
        assert_eq!(metadata(&[("xesam:title", Value::Int(3))]).title, None, "a title of the wrong type is none");
        assert_eq!(metadata(&[("xesam:title", s("  \t "))]).title, None, "a blank title is none: the fallbacks apply");
    }

    #[test]
    fn length_is_any_integer_or_a_float_and_unknown_otherwise() {
        let length = |v: Value| metadata(&[("mpris:length", v)]).length_us;
        assert_eq!(length(Value::Int(355_000_000)), Some(355_000_000), "x, i, u or t: the codec gives Int");
        assert_eq!(length(Value::Float(355_000_000.9)), Some(355_000_000), "d");
        assert_eq!(length(Value::Int(-1)), None, "negative");
        assert_eq!(length(Value::Int(0)), None, "0: a stream");
        assert_eq!(length(Value::Float(-2.0)), None);
        assert_eq!(length(Value::Float(f64::NAN)), None);
        assert_eq!(length(Value::Float(f64::INFINITY)), None);
        assert_eq!(length(Value::Bool(true)), None);
    }

    /// Review focus: `mpris:length` sent as a string is unknown, and nothing panics.
    #[test]
    fn a_length_sent_as_a_string_is_unknown() {
        assert_eq!(metadata(&[("mpris:length", s("355000000"))]).length_us, None);
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        let changed = props(&[("Metadata", Value::Map(props(&[("xesam:title", s("Song")), ("mpris:length", s("3:55"))])))]);
        p.apply(":1.4", &changed, &[]);
        p.set_position(":1.4", PositionSync { at: Instant::now(), us: 1_000_000 });
        let player = p.get(":1.4").unwrap();
        assert_eq!((player.track.length_us, position_now(player, Instant::now())), (None, None));
    }

    /// Review focus: an empty artist list is no artist (no row in the popup), never ", ".
    #[test]
    fn an_empty_artist_list_is_no_artist() {
        for v in [list(&[]), list(&["", "  ", "\u{1}"]), s(""), Value::Other] {
            let t = metadata(&[("xesam:artist", v.clone())]);
            assert_eq!((t.artists.len(), t.artist_line()), (0, String::new()), "{v:?}");
        }
        assert_eq!(metadata(&[("xesam:artist", list(&["", "Queen"]))]).artist_line(), "Queen", "blanks skipped");
    }

    #[test]
    fn artists_are_capped_at_8() {
        let many: Vec<String> = (1..=20).map(|i| format!("Artist {i}")).collect();
        let t = metadata(&[("xesam:artist", Value::StrList(many))]);
        assert_eq!(t.artists.len(), MAX_ARTISTS);
        assert_eq!((t.artists[0].as_str(), t.artists[7].as_str()), ("Artist 1", "Artist 8"), "the first 8, in order");
    }

    /// Review focus: a player sending a 100 KB title keeps 256 characters of it.
    #[test]
    fn a_100_kb_title_is_capped() {
        let huge = "é".repeat(50_000);
        assert!(huge.len() >= 100_000);
        let t = metadata(&[("xesam:title", s(&huge)), ("xesam:artist", list(&[huge.as_str()]))]);
        assert_eq!(t.title.as_deref().map(|x| x.chars().count()), Some(crate::tray::MAX_TEXT));
        assert_eq!(t.artists[0].chars().count(), crate::tray::MAX_TEXT);
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        p.set_identity(":1.4", &huge);
        assert_eq!(p.get(":1.4").and_then(|x| x.identity.as_ref()).map(|x| x.chars().count()), Some(crate::tray::MAX_TEXT));
    }

    /// Review focus: a newline in a title never reaches the bar (cleaned to a space, like the tray).
    #[test]
    fn a_newline_in_a_title_is_stripped() {
        let t = metadata(&[("xesam:title", s("Line one\nLine two\r\n")), ("xesam:artist", s("A\u{1b}[31mB"))]);
        assert_eq!(t.title.as_deref(), Some("Line one Line two"), "line breaks spaced, the trailing ones trimmed");
        assert_eq!(t.artists, ["A[31mB"], "the escape character removed");
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        p.apply(":1.4", &props(&[("Metadata", Value::Map(props(&[("xesam:title", s("a\nb"))])))]), &[]);
        let v = view(&p, 30).unwrap();
        assert!(!v.title.contains(['\n', '\r']), "{v:?}");
    }

    // ---- players: discovery, ignore list, apply ----

    #[test]
    fn playerctld_is_always_ignored_then_the_substrings() {
        let none: Vec<String> = Vec::new();
        let kde = vec!["kdeconnect".to_string()];
        assert!(Players::is_ignored("org.mpris.MediaPlayer2.playerctld", &none));
        assert!(Players::is_ignored("org.mpris.MediaPlayer2.playerctld.instance77", &none));
        assert!(!Players::is_ignored("org.mpris.MediaPlayer2.playerctldx", &none), "another player");
        assert!(!Players::is_ignored("org.mpris.MediaPlayer2.spotify", &none));
        assert!(Players::is_ignored("org.mpris.MediaPlayer2.kdeconnect.mpris_000001", &kde));
        assert!(!Players::is_ignored("org.mpris.MediaPlayer2.spotify", &kde));
        assert!(!Players::is_ignored("org.mpris.MediaPlayer2.spotify", &[String::new()]), "an empty substring ignores nothing");
        assert!(Players::is_ignored("org.mpris.MediaPlayer2.firefox.instance_1_3", &["firefox".to_string()]));
    }

    #[test]
    fn the_17th_player_is_refused_and_a_name_counts_once() {
        let mut p = Players::default();
        for i in 0..MAX_PLAYERS {
            assert!(p.add(format!("{MPRIS_PREFIX}p{i}"), format!(":1.{i}")), "{i}");
        }
        assert!(!p.add(format!("{MPRIS_PREFIX}p16"), ":1.16".into()), "the 17th");
        assert_eq!(p.len(), MAX_PLAYERS);
        assert!(p.remove_unique(":1.3"));
        assert!(!p.remove_unique(":1.3"), "already gone");
        assert!(!p.add(format!("{MPRIS_PREFIX}p0"), ":1.99".into()), "that bus name is tracked");
        assert!(!p.add(format!("{MPRIS_PREFIX}other"), ":1.0".into()), "that connection is tracked under another name");
        assert!(p.add(format!("{MPRIS_PREFIX}p16"), ":1.16".into()), "room again");
        assert!(!p.is_empty());
    }

    #[test]
    fn apply_reads_the_player_properties_and_asks_for_invalidated_metadata() {
        let mut p = players(&[("spotify", ":1.4", "Paused")]);
        let changed = props(&[
            ("CanPlay", Value::Bool(true)),
            ("CanPause", Value::Bool(true)),
            ("CanGoNext", Value::Bool(true)),
            ("CanGoPrevious", Value::Bool(false)),
            ("Rate", Value::Float(1.5)),
            ("Volume", Value::Float(0.3)),
            ("Position", Value::Int(42)),
            ("CanPlay", Value::Other),
        ]);
        assert_eq!(p.apply(":1.4", &changed, &[]), Need::None);
        let x = p.get(":1.4").unwrap();
        assert_eq!(x.caps, Caps { play: true, pause: true, next: true, previous: false }, "a value of the wrong type is ignored");
        assert_eq!((x.rate, x.position), (1.5, None), "Volume and Position are not read here");
        assert_eq!(p.apply(":1.4", &[], &["Metadata".into()]), Need::Metadata(":1.4".into()));
        assert_eq!(p.apply(":1.4", &[], &["Volume".into()]), Need::None);
        assert_eq!(p.apply(":1.9", &changed, &["Metadata".into()]), Need::None, "an unknown player");
        for (rate, want) in [(Value::Float(40.0), MAX_RATE), (Value::Float(-1.0), 0.0), (Value::Int(2), 2.0), (Value::Float(f64::NAN), 2.0)]
        {
            p.apply(":1.4", &props(&[("Rate", rate.clone())]), &[]);
            assert_eq!(p.get(":1.4").unwrap().rate, want, "{rate:?}");
        }
    }

    #[test]
    fn a_status_or_track_change_forgets_the_position() {
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        let sync = PositionSync { at: Instant::now(), us: 5 };
        let song = |title: &str| props(&[("Metadata", Value::Map(props(&[("xesam:title", s(title))])))]);
        p.apply(":1.4", &song("One"), &[]);
        p.set_position(":1.4", sync);
        p.apply(":1.4", &song("One"), &[]);
        assert_eq!(p.get(":1.4").unwrap().position, Some(sync), "the same track: kept");
        p.apply(":1.4", &song("Two"), &[]);
        assert_eq!(p.get(":1.4").unwrap().position, None, "a new track");
        p.set_position(":1.4", sync);
        p.apply(":1.4", &props(&[("PlaybackStatus", s("Playing"))]), &[]);
        assert_eq!(p.get(":1.4").unwrap().position, Some(sync), "the same status: kept");
        p.apply(":1.4", &props(&[("PlaybackStatus", s("Paused"))]), &[]);
        assert_eq!(p.get(":1.4").unwrap().position, None, "a status change");
    }

    #[test]
    fn identity_is_cleaned_and_a_blank_one_is_none() {
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        p.set_identity(":1.4", " Spotify\n");
        assert_eq!(p.get(":1.4").unwrap().identity.as_deref(), Some("Spotify"));
        p.set_identity(":1.4", " \t");
        assert_eq!(p.get(":1.4").unwrap().identity, None);
        p.set_identity(":1.9", "Nobody");
    }

    // ---- the active player (spec § 3) ----

    #[test]
    fn playing_beats_paused_and_a_tie_goes_to_the_latest_change() {
        let mut p = players(&[("mpv", ":1.1", "Paused"), ("spotify", ":1.2", "Playing"), ("firefox", ":1.3", "Stopped")]);
        assert_eq!(active(&p), Some(":1.2"), "playing 2, paused 1");
        p.apply(":1.1", &props(&[("PlaybackStatus", s("Playing"))]), &[]);
        assert_eq!(active(&p), Some(":1.1"), "two playing: the latest to start");
        p.apply(":1.2", &props(&[("PlaybackStatus", s("Paused"))]), &[]);
        p.apply(":1.2", &props(&[("PlaybackStatus", s("Playing"))]), &[]);
        assert_eq!(active(&p), Some(":1.2"), "it started again since");
        p.apply(":1.1", &props(&[("PlaybackStatus", s("Stopped"))]), &[]);
        p.apply(":1.2", &props(&[("PlaybackStatus", s("Stopped"))]), &[]);
        assert_eq!(active(&p), None, "every player stopped");
        assert_eq!(p.shown().count(), 0);
    }

    #[test]
    fn a_pinned_player_stays_active_until_it_stops_or_leaves() {
        let mut p = players(&[("mpv", ":1.1", "Paused"), ("spotify", ":1.2", "Playing")]);
        p.pin("org.mpris.MediaPlayer2.mpv");
        assert_eq!(active(&p), Some(":1.1"), "pinned, though only paused");
        p.pin("org.mpris.MediaPlayer2.nothing");
        assert_eq!(active(&p), Some(":1.1"), "an unknown name changes nothing");
        p.apply(":1.1", &props(&[("PlaybackStatus", s("Stopped"))]), &[]);
        assert_eq!(active(&p), Some(":1.2"), "stopped: the score decides");
        p.apply(":1.1", &props(&[("PlaybackStatus", s("Paused"))]), &[]);
        assert_eq!(active(&p), Some(":1.1"), "the pin holds again");
        p.remove_unique(":1.1");
        assert!(p.add("org.mpris.MediaPlayer2.mpv".into(), ":1.7".into()));
        p.apply(":1.7", &props(&[("PlaybackStatus", s("Paused"))]), &[]);
        assert_eq!(active(&p), Some(":1.2"), "the pin left with its player");
    }

    #[test]
    fn shown_lists_the_players_not_stopped_in_discovery_order() {
        let p = players(&[("mpv", ":1.1", "Paused"), ("vlc", ":1.2", "Stopped"), ("spotify", ":1.3", "Playing")]);
        let shown: Vec<&str> = p.shown().map(|x| x.unique.as_str()).collect();
        assert_eq!(shown, [":1.1", ":1.3"]);
    }

    #[test]
    fn controls_follow_the_capabilities() {
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        let caps = |p: &Players| [Control::PlayPause, Control::Next, Control::Previous].map(|c| p.get(":1.4").unwrap().can(c));
        assert_eq!(caps(&p), [false, false, false], "nothing said: nothing allowed");
        p.apply(":1.4", &props(&[("CanPause", Value::Bool(true)), ("CanGoNext", Value::Bool(true))]), &[]);
        assert_eq!(caps(&p), [true, true, false], "playing: PlayPause needs CanPause");
        p.apply(":1.4", &props(&[("PlaybackStatus", s("Paused"))]), &[]);
        assert_eq!(caps(&p), [false, true, false], "paused: PlayPause needs CanPlay");
        p.apply(":1.4", &props(&[("CanPlay", Value::Bool(true)), ("CanGoPrevious", Value::Bool(true))]), &[]);
        assert_eq!(caps(&p), [true, true, true]);
    }

    // ---- what the bar shows (spec § 1) ----

    #[test]
    fn the_title_falls_back_to_the_url_then_the_name() {
        let track = |title: Option<&str>, url: Option<&str>| Track {
            title: title.map(String::from),
            url: url.map(String::from),
            ..Track::default()
        };
        let bus = "org.mpris.MediaPlayer2.vlc.instance4242";
        assert_eq!(display_title(&track(Some("Song"), Some("file:///a/b.mp3")), Some("VLC"), bus), "Song");
        assert_eq!(display_title(&track(None, Some("file:///music/My%20Song.mp3")), Some("VLC"), bus), "My Song.mp3");
        assert_eq!(display_title(&track(None, Some("https://radio.example/live/stream?id=3#x")), None, bus), "stream");
        assert_eq!(display_title(&track(None, Some("file:///music/Album/")), None, bus), "Album", "a trailing slash");
        assert_eq!(display_title(&track(None, Some("file:///a/100%")), None, bus), "100%", "a lone % stays");
        assert_eq!(display_title(&track(None, Some("")), Some("VLC media player"), bus), "VLC media player", "no url: the identity");
        assert_eq!(display_title(&track(None, None), None, bus), "vlc", "no identity: the bus name's first label");
        assert_eq!(player_name(None, "org.mpris.MediaPlayer2.spotify"), "spotify");
        assert_eq!(player_name(Some(""), "org.mpris.MediaPlayer2.spotify"), "spotify");
    }

    #[test]
    fn view_is_none_without_an_active_player_and_truncated_otherwise() {
        assert_eq!(view(&Players::default(), 30), None, "no player");
        assert_eq!(view(&players(&[("mpv", ":1.1", "Stopped")]), 30), None, "every player stopped");
        let mut p = players(&[("spotify", ":1.4", "Playing")]);
        p.apply(":1.4", &props(&[("Metadata", Value::Map(props(&[("xesam:title", s("Bohemian Rhapsody"))])))]), &[]);
        assert_eq!(view(&p, 30), Some(MediaView { playing: true, title: "Bohemian Rhapsody".into() }));
        assert_eq!(view(&p, 8), Some(MediaView { playing: true, title: "Bohemia…".into() }));
        p.apply(":1.4", &props(&[("PlaybackStatus", s("Paused"))]), &[]);
        assert_eq!(view(&p, 30).map(|v| v.playing), Some(false));
    }

    // ---- truncation (spec § 1, amended: no new dependency) ----

    #[test]
    fn truncation_counts_cells_like_the_grid() {
        assert_eq!(truncate_title("Bohemian Rhapsody", 30), "Bohemian Rhapsody");
        assert_eq!(truncate_title("12345678", 8), "12345678", "exactly the width");
        assert_eq!(truncate_title("123456789", 8), "1234567…");
        assert_eq!(truncate_title("日本語のタイトル", 8), "日本語…", "CJK: two cells each, 7 + 1");
        assert_eq!(truncate_title("日本語のタイトル", 9), "日本語の…");
        assert_eq!(truncate_title("🦀🦀🦀🦀", 6), "🦀🦀…");
        assert_eq!(truncate_title("e\u{301}lan", 10), "élan", "recomposed, as the grid writes it");
        assert_eq!(truncate_title("abc", 0), "");
        assert_eq!(truncate_title("abc", 1), "…");
        for (s, max) in [("日本語のタイトル", 8), ("🦀🦀🦀🦀", 5), ("a👨\u{200d}👩\u{200d}👧b", 6), ("ab🇫🇷🇩🇪", 4)]
        {
            assert!(text_width(&truncate_title(s, max)) <= max, "{s} at {max}");
        }
    }

    #[test]
    fn truncation_never_splits_a_cluster() {
        assert_eq!(truncate_title("aq\u{301}bc", 3), "aq\u{301}…", "a combining mark stays with its base");
        assert_eq!(truncate_title("abc\u{2764}\u{fe0f}def", 5), "abc\u{2764}\u{fe0f}…", "a variation selector stays with its base");
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(truncate_title(&format!("ab{family}"), 7), "ab…", "a ZWJ family whole or not at all");
        assert_eq!(truncate_title(&format!("ab{family}"), 8), format!("ab{family}"), "it fits: 2 + 6 cells");
        assert_eq!(truncate_title(&format!("ab{family}cd"), 9), format!("ab{family}…"));
        assert_eq!(truncate_title("ab👍\u{1f3fd}c", 5), "ab…", "a skin tone stays with its hand");
        assert_eq!(truncate_title("ab🇫🇷🇩🇪", 4), "ab…", "a flag is two regional indicators");
        assert_eq!(truncate_title("ab🇫🇷🇩🇪", 5), "ab🇫🇷…");
    }

    // ---- time (spec § 2) ----

    fn timed(status: &str, length: Option<u64>, rate: f64, sync_us: u64, at: Instant) -> Player {
        let mut p = players(&[("spotify", ":1.4", status)]);
        let mut meta = vec![("xesam:title", s("Song"))];
        if let Some(l) = length {
            meta.push(("mpris:length", Value::Int(l as i64)));
        }
        p.apply(":1.4", &props(&[("Metadata", Value::Map(props(&meta))), ("Rate", Value::Float(rate))]), &[]);
        p.set_position(":1.4", PositionSync { at, us: sync_us });
        p.get(":1.4").unwrap().clone()
    }

    #[test]
    fn the_position_runs_at_the_rate_while_playing_and_is_clamped() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(10);
        assert_eq!(position_now(&timed("Playing", Some(300_000_000), 1.0, 5_000_000, t0), later), Some(15_000_000));
        assert_eq!(position_now(&timed("Playing", Some(300_000_000), 2.0, 5_000_000, t0), later), Some(25_000_000), "rate 2");
        assert_eq!(position_now(&timed("Paused", Some(300_000_000), 1.0, 5_000_000, t0), later), Some(5_000_000), "paused: still");
        assert_eq!(position_now(&timed("Playing", Some(12_000_000), 1.0, 5_000_000, t0), later), Some(12_000_000), "clamped to the length");
        assert_eq!(position_now(&timed("Playing", None, 1.0, 5_000_000, t0), later), None, "unknown length");
        assert_eq!(
            position_now(&timed("Playing", Some(300_000_000), 1.0, 5_000_000, later), t0),
            Some(5_000_000),
            "a clock behind the sync"
        );
        let mut unsynced = timed("Playing", Some(300_000_000), 1.0, 0, t0);
        unsynced.position = None;
        assert_eq!(position_now(&unsynced, later), None, "never read");
    }

    #[test]
    fn clock_is_minutes_then_hours() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(102_000_000), "1:42");
        assert_eq!(clock(355_999_999), "5:55");
        assert_eq!(clock(3_599_000_000), "59:59");
        assert_eq!(clock(3_600_000_000), "1:00:00");
        assert_eq!(clock(37_230_000_000), "10:20:30");
        assert_eq!(clock(u64::MAX), "5124095576:01:49");
    }
}
