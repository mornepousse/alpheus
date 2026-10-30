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

/// An unknown status string is Stopped, so the player is hidden.
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

/// `mpris:length` sent as a string is unknown, and nothing panics.
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

/// An empty artist list is no artist (no row in the popup), never ", ".
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

/// A player sending a 100 KB title keeps 256 characters of it.
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

/// A newline in a title never reaches the bar (cleaned to a space, like the tray).
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
    assert_eq!(p.apply(":1.4", &changed, &[]).need, Need::None);
    let x = p.get(":1.4").unwrap();
    assert_eq!(x.caps, Caps { play: true, pause: true, next: true, previous: false }, "a value of the wrong type is ignored");
    assert_eq!((x.rate, x.position), (1.5, None), "Volume and Position are not read here");
    assert_eq!(p.apply(":1.4", &[], &["Metadata".into()]).need, Need::Metadata(":1.4".into()));
    assert_eq!(p.apply(":1.4", &[], &["Volume".into()]).need, Need::None);
    assert_eq!(p.apply(":1.9", &changed, &["Metadata".into()]).need, Need::None, "an unknown player");
    for (rate, want) in [
        (Value::Float(40.0), MAX_RATE),
        (Value::Float(-1.0), 0.0),
        (Value::Int(2), 2.0),
        (Value::Float(f64::NAN), 2.0),
        (Value::Float(f64::INFINITY), 2.0),
        (Value::Float(f64::NEG_INFINITY), 2.0),
    ] {
        p.apply(":1.4", &props(&[("Rate", rate.clone())]), &[]);
        assert_eq!(p.get(":1.4").unwrap().rate, want, "{rate:?}");
    }
}

/// Only what the bar or the popup shows (status, track, capabilities) asks for
/// the view to be rebuilt; Rate, Volume or a repeat of the same value never does.
#[test]
fn apply_says_whether_anything_shown_changed() {
    let mut p = players(&[("spotify", ":1.4", "Paused")]);
    let visible = |p: &mut Players, changed: &[(String, Value)]| p.apply(":1.4", changed, &[]).visible;
    assert!(visible(&mut p, &props(&[("PlaybackStatus", s("Playing"))])), "a status");
    assert!(!visible(&mut p, &props(&[("PlaybackStatus", s("Playing"))])), "the same status again");
    let song = props(&[("Metadata", Value::Map(props(&[("xesam:title", s("One"))])))]);
    assert!(visible(&mut p, &song), "a track");
    assert!(!visible(&mut p, &song), "the same track again");
    assert!(visible(&mut p, &props(&[("CanGoNext", Value::Bool(true))])), "a capability");
    assert!(!visible(&mut p, &props(&[("CanGoNext", Value::Bool(true))])), "the same capability again");
    assert!(!visible(&mut p, &props(&[("Rate", Value::Float(2.0)), ("Volume", Value::Float(0.3))])), "Rate, Volume: unseen");
    assert!(!p.apply(":1.4", &[], &["Metadata".into()]).visible, "an invalidation alone: nothing yet");
    assert!(!p.apply(":1.9", &props(&[("PlaybackStatus", s("Stopped"))]), &[]).visible, "an unknown player");
}

#[test]
fn a_status_track_or_rate_change_forgets_the_position() {
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
    // A new Rate too, or the time since the last read would be counted at it.
    p.set_position(":1.4", sync);
    p.apply(":1.4", &props(&[("Rate", Value::Float(1.0))]), &[]);
    assert_eq!(p.get(":1.4").unwrap().position, Some(sync), "the same rate: kept");
    p.apply(":1.4", &props(&[("Rate", Value::Float(2.0))]), &[]);
    assert_eq!(p.get(":1.4").unwrap().position, None, "a rate change");
    p.set_position(":1.4", sync);
    p.apply(":1.4", &props(&[("Rate", Value::Float(f64::NAN))]), &[]);
    assert_eq!(p.get(":1.4").unwrap().position, Some(sync), "a rate refused: kept");
}

#[test]
fn identity_is_cleaned_and_a_blank_one_is_none() {
    let mut p = players(&[("spotify", ":1.4", "Playing")]);
    p.set_identity(":1.4", " Spotify\n");
    assert_eq!(p.get(":1.4").unwrap().identity.as_deref(), Some("Spotify"));
    p.set_identity(":1.4", " \t");
    assert_eq!(p.get(":1.4").unwrap().identity, None);
    p.set_identity(":1.9", "Nobody");
    assert_eq!(p.len(), 1, "an unknown player is not added");
    assert_eq!(p.get(":1.4").unwrap().identity, None, "and the known one is untouched");
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
    let track =
        |title: Option<&str>, url: Option<&str>| Track { title: title.map(String::from), url: url.map(String::from), ..Track::default() };
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
    // Never the scheme nor the host — a url with no path segment is no title.
    assert_eq!(display_title(&track(None, Some("file:///")), Some("VLC"), bus), "VLC", "file:/// alone");
    assert_eq!(display_title(&track(None, Some("https://radio.example/")), None, bus), "vlc", "a host alone");
    assert_eq!(display_title(&track(None, Some("https://radio.example")), None, bus), "vlc", "a host, no slash");
    assert_eq!(display_title(&track(None, Some("https://radio.example/?id=3")), None, bus), "vlc", "a host and a query");
    assert_eq!(display_title(&track(None, Some("/music/Song.ogg")), None, bus), "Song.ogg", "a bare path");
}

/// A title of combining marks or zero-width characters alone draws nothing: it is
/// blank, so the url or the player's name stands in.
#[test]
fn a_title_that_takes_no_cell_is_blank() {
    let meta = |title: &str| {
        Track::from_metadata(&props(&[("xesam:title", s(title)), ("xesam:artist", Value::StrList(vec!["\u{200b}".into(), "Ann".into()]))]))
    };
    let t = meta("\u{301}\u{302}\u{200d}");
    assert_eq!(t.title, None);
    assert_eq!(t.artists, ["Ann"], "a zero-width artist is none either");
    assert_eq!(display_title(&t, Some("VLC"), "org.mpris.MediaPlayer2.vlc"), "VLC");
    assert_eq!(meta("e\u{301}").title.as_deref(), Some("e\u{301}"), "a mark on a base stays");
    let mut p = players(&[("spotify", ":1.4", "Playing")]);
    p.set_identity(":1.4", "\u{fe0f}");
    assert_eq!(p.get(":1.4").unwrap().identity, None);
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
    assert_eq!(position_now(&timed("Playing", Some(300_000_000), 1.0, 5_000_000, later), t0), Some(5_000_000), "a clock behind the sync");
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
