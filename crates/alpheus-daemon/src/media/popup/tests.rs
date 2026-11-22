use super::*;
use crate::grid::text_width;
use crate::media::{PositionSync, Value};
use crate::segments::{PAUSE, PLAY, SKIP_NEXT, SKIP_PREVIOUS};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const LEN: i64 = 245_000_000; // 4:05

/// What GetAll returns for a fictional track (fixtures are fictional: no real artist or title).
fn props(status: &str, title: &str, artists: &[&str], length_us: Option<i64>) -> Vec<(String, Value)> {
    let mut meta = vec![
        ("mpris:trackid".to_string(), Value::Str(format!("/fake/track/{}", title.len()))),
        ("xesam:title".to_string(), Value::Str(title.to_string())),
        ("xesam:artist".to_string(), Value::StrList(artists.iter().map(|a| a.to_string()).collect())),
    ];
    if let Some(l) = length_us {
        meta.push(("mpris:length".to_string(), Value::Int(l)));
    }
    let can = |k: &str| (k.to_string(), Value::Bool(true));
    vec![
        ("PlaybackStatus".to_string(), Value::Str(status.to_string())),
        ("Metadata".to_string(), Value::Map(meta)),
        ("Rate".to_string(), Value::Float(1.0)),
        can("CanPlay"),
        can("CanPause"),
        can("CanGoNext"),
        can("CanGoPrevious"),
    ]
}

/// `p` with `key` set to `v` (the last entry of a key wins, so the old one is dropped first).
fn set(mut p: Vec<(String, Value)>, key: &str, v: bool) -> Vec<(String, Value)> {
    p.retain(|(k, _)| k != key);
    p.push((key.to_string(), Value::Bool(v)));
    p
}

/// One more player, `:1.n` owning `org.mpris.MediaPlayer2.name`, filled as GetAll would.
fn with(players: &mut Players, name: &str, n: u32, p: Vec<(String, Value)>) -> String {
    let unique = format!(":1.{n}");
    assert!(players.add(format!("{PREFIX}{name}"), unique.clone()));
    let _ = players.apply(&unique, &p, &[]);
    unique
}

/// One player playing "Paper Lanterns" by "The Quiet Hours", 4:05 long, at 1:42 at `t0`.
fn one(t0: Instant) -> Players {
    let mut players = Players::default();
    let u = with(&mut players, "lumen", 7, props("Playing", "Paper Lanterns", &["The Quiet Hours"], Some(LEN)));
    players.set_identity(&u, "Lumen Player");
    players.set_position(&u, PositionSync { at: t0, us: 102_000_000 });
    players
}

fn texts(b: &Boxed) -> Vec<String> {
    b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
}

#[test]
fn one_player_title_artist_time_and_buttons() {
    let t0 = Instant::now();
    let players = one(t0);
    let popup = MediaPopup::new(&players);
    let b = rows(&players, &popup, t0, WIDTH);
    assert_eq!(b.cells(), (WIDTH + 2, 4), "padded: one blank cell each side; no separator nor players with one player");
    assert!(b.separators.is_empty());
    let t = texts(&b);
    assert_eq!(t[0], " Paper Lanterns");
    assert_eq!(t[1], " The Quiet Hours");
    // 20 cells of bar between "1:42 " and " 4:05": 102 / 245 of them filled, rounded down.
    assert_eq!(t[2], format!(" 1:42 {}{} 4:05", "━".repeat(8), "─".repeat(12)));
    assert_eq!(&b.rows[2].styles()[1..12], "mmmm.aaaaaa", "the clocks muted, the elapsed part in the accent");
    let cells = b.rows[3].cells();
    assert_eq!([cells[11].ch, cells[16].ch, cells[21].ch], [SKIP_PREVIOUS, PAUSE, SKIP_NEXT], "centred, ⏸ while playing");
    assert_eq!(&b.rows[3].styles()[10..23], ".....SSS.....", "the play/pause button selected at open");
    assert_eq!(&b.rows[0].styles()[1..2], "G", "the title in bold accent");
}

/// The selection is a player (by bus name) or the buttons, not a row number, so the
/// highlight never jumps to a neighbour when rows come or go above it.
#[test]
fn the_selection_follows_its_player_when_rows_or_players_change() {
    let t0 = Instant::now();
    let mut players = Players::default();
    let lumen = with(&mut players, "lumen", 7, props("Playing", "Paper Lanterns", &[], Some(LEN)));
    let beta = with(&mut players, "beta", 8, props("Paused", "Quiet Song", &["Nobody"], Some(LEN)));
    with(&mut players, "gamma", 9, props("Paused", "Other Song", &[], None));
    let mut popup = MediaPopup::new(&players);
    // Title, time, buttons, separator, lumen, beta, gamma: down to gamma.
    for _ in 0..4 {
        key(&mut popup, &players, Key::Down);
    }
    let selected_row = |popup: &mut MediaPopup, players: &Players| {
        popup.fit(players);
        let b = rows(players, popup, t0, WIDTH);
        b.rows.iter().position(|g| g.styles().contains('S')).map(|i| b.rows[i].text().trim().to_string())
    };
    assert_eq!(selected_row(&mut popup, &players).as_deref(), Some("gamma"));
    // An artist row appears above: still gamma.
    let _ = players.apply(&lumen, &props("Playing", "Paper Lanterns", &["The Quiet Hours"], Some(LEN)), &[]);
    assert_eq!(selected_row(&mut popup, &players).as_deref(), Some("gamma"), "a row added above");
    // beta, listed before gamma, leaves: still gamma.
    assert!(players.remove_unique(&beta));
    assert_eq!(selected_row(&mut popup, &players).as_deref(), Some("gamma"), "a player gone above");
    // The buttons follow their row too.
    key(&mut popup, &players, Key::Up);
    key(&mut popup, &players, Key::Up);
    let _ = players.apply(&lumen, &props("Playing", "Paper Lanterns", &[], Some(LEN)), &[]);
    popup.fit(&players);
    assert_eq!(lines(&players).get(popup.selected), Some(&Line::Buttons), "the artist row gone: still the buttons");
    // gamma itself gone: back to the buttons, never its neighbour.
    key(&mut popup, &players, Key::Down);
    key(&mut popup, &players, Key::Down);
    assert_eq!(selected_row(&mut popup, &players).as_deref(), Some("gamma"));
    assert!(players.remove_unique(":1.9"));
    popup.fit(&players);
    assert_eq!(lines(&players).get(popup.selected), Some(&Line::Buttons), "its player gone: the buttons");
}

#[test]
fn three_players_add_a_separator_and_the_list_with_the_active_marked() {
    let t0 = Instant::now();
    let mut players = one(t0);
    with(&mut players, "beta", 8, props("Paused", "Quiet Song", &["Nobody"], Some(LEN)));
    with(&mut players, "gamma", 9, props("Paused", "Other Song", &[], None));
    let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
    assert_eq!(b.cells().1, 8, "title, artist, time, buttons, separator, three players");
    assert_eq!(b.separators, vec![4]);
    assert_eq!(b.rows[4].text().trim(), "", "the separator row stays blank: the painter draws its line");
    let t = texts(&b);
    let mut listed: Vec<&str> = t[5..].iter().map(|r| r.trim_start_matches(' ').trim_start_matches('●').trim()).collect();
    listed.sort_unstable();
    assert_eq!(listed, ["Lumen Player", "beta", "gamma"], "Identity, else the bus name suffix");
    let marked: Vec<&String> = t[5..].iter().filter(|r| r.contains('●')).collect();
    assert_eq!(marked, [&" ● Lumen Player".to_string()], "only the active player carries ●");
}

#[test]
fn an_unknown_length_hides_the_time_row() {
    let t0 = Instant::now();
    for length in [None, Some(0)] {
        let mut players = Players::default();
        with(&mut players, "radio", 3, props("Playing", "Live Stream", &["Somewhere FM"], length));
        assert_eq!(lines(&players), [Line::Title, Line::Artist, Line::Buttons], "length {length:?}");
        let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
        assert_eq!(b.cells().1, 3);
        assert_eq!(tick_due(true, true, 1.0, 0, length.map(|l| l as u64)), None, "nothing to redraw: no tick");
    }
    let mut players = Players::default();
    with(&mut players, "radio", 3, props("Playing", "Live Stream", &[], None));
    assert_eq!(lines(&players), [Line::Title, Line::Buttons], "no artist: no artist row");
}

#[test]
fn an_incapable_button_is_dim_skipped_and_ignored() {
    let t0 = Instant::now();
    let mut players = Players::default();
    let u = with(&mut players, "lumen", 7, set(props("Playing", "Paper Lanterns", &["The Quiet Hours"], Some(LEN)), "CanGoNext", false));
    players.set_position(&u, PositionSync { at: t0, us: 0 });
    let mut popup = MediaPopup::new(&players);
    let b = rows(&players, &popup, t0, WIDTH);
    assert_eq!(&b.rows[3].styles()[21..22], "m", "⏭ drawn dim");
    assert_eq!(key(&mut popup, &players, Key::Right), MediaAction::Nothing, "→ has no capable button to go to");
    assert_eq!(popup.button, 1);
    assert_eq!(press(&mut popup, &players, (21, 3), WIDTH), MediaAction::Nothing, "a click on ⏭ is ignored");
    assert_eq!(popup.button, 1, "and moves nothing");
    assert_eq!(key(&mut popup, &players, Key::Enter), MediaAction::Control(Control::PlayPause));
    // Nothing capable at all: the button row cannot be selected, and with one player nothing can.
    let mut players = Players::default();
    let mut p = props("Playing", "Paper Lanterns", &[], Some(LEN));
    for k in ["CanPlay", "CanPause", "CanGoNext", "CanGoPrevious"] {
        p = set(p, k, false);
    }
    with(&mut players, "lumen", 7, p);
    let mut popup = MediaPopup::new(&players);
    let b = rows(&players, &popup, t0, WIDTH);
    assert!(!b.rows.iter().any(|g| g.styles().contains('S')), "no selection band anywhere");
    // No artist: title, time, buttons — the buttons are row 2, ⏯ at boxed column 16.
    assert_eq!(&b.rows[2].styles()[16..17], "m", "⏯ dim too (Paused would ask CanPlay, Playing CanPause)");
    for k in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Enter] {
        assert_eq!(key(&mut popup, &players, k), MediaAction::Nothing, "{k:?}");
    }
}

#[test]
fn long_and_wide_titles_are_cut_to_the_popup_width() {
    let t0 = Instant::now();
    for title in ["a".repeat(40), "日本語のとても長い曲のタイトルです日本語".to_string()] {
        let mut players = Players::default();
        with(&mut players, "lumen", 7, props("Playing", &title, &["The Quiet Hours"], Some(LEN)));
        let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
        assert_eq!(b.rows[0].width(), WIDTH + 2, "{title}");
        let shown = b.rows[0].text();
        let shown = shown.trim();
        assert!(shown.ends_with('…'), "cut: {shown}");
        assert!(text_width(shown) <= WIDTH, "{shown}: {} cells", text_width(shown));
        assert_eq!(b.rows[0].cells()[WIDTH as usize + 1].ch, ' ', "the right pad cell is never written over");
    }
}

#[test]
fn keys_move_between_the_buttons_and_the_players() {
    let t0 = Instant::now();
    let mut players = one(t0);
    with(&mut players, "beta", 8, props("Paused", "Quiet Song", &[], Some(LEN)));
    with(&mut players, "gamma", 9, props("Paused", "Other Song", &[], Some(LEN)));
    let mut p = MediaPopup::new(&players);
    assert_eq!((p.selected, p.button), (3, 1), "the buttons row, play/pause");
    assert_eq!(key(&mut p, &players, Key::Up), MediaAction::Nothing, "nothing selectable above the buttons");
    assert_eq!(key(&mut p, &players, Key::Right), MediaAction::Redraw);
    assert_eq!(p.button, 2);
    assert_eq!(key(&mut p, &players, Key::Right), MediaAction::Nothing, "clamped, never wrapping");
    assert_eq!(key(&mut p, &players, Key::Enter), MediaAction::Control(Control::Next));
    assert_eq!(key(&mut p, &players, Key::Left), MediaAction::Redraw);
    assert_eq!(key(&mut p, &players, Key::Left), MediaAction::Redraw);
    assert_eq!(key(&mut p, &players, Key::Enter), MediaAction::Control(Control::Previous));
    assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Redraw);
    assert_eq!(p.selected, 5, "over the separator, onto the first player");
    assert_eq!(key(&mut p, &players, Key::Left), MediaAction::Nothing, "← → only on the buttons row");
    let first = players.shown().next().map(|q| q.bus_name.clone()).unwrap();
    assert_eq!(key(&mut p, &players, Key::Enter), MediaAction::Pin(first));
    assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Redraw);
    assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Redraw);
    assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Nothing, "the last player");
    assert_eq!(key(&mut p, &players, Key::Up), MediaAction::Redraw);
    assert_eq!(key(&mut p, &players, Key::Escape), MediaAction::Close);
}

#[test]
fn a_press_maps_cells_to_buttons_and_players() {
    let t0 = Instant::now();
    let mut players = one(t0);
    with(&mut players, "beta", 8, props("Paused", "Quiet Song", &[], Some(LEN)));
    let mut p = MediaPopup::new(&players);
    // Boxed columns: the pad is column 0, so content column c is cell c + 1. Buttons at content
    // 10, 15, 20, each taking the 2 cells either side of it.
    assert_eq!(press(&mut p, &players, (21, 3), WIDTH), MediaAction::Control(Control::Next));
    assert_eq!((p.selected, p.button), (3, 2), "the press selects what it ran");
    assert_eq!(press(&mut p, &players, (9, 3), WIDTH), MediaAction::Control(Control::Previous), "content 8: ⏮'s left edge");
    assert_eq!(press(&mut p, &players, (13, 3), WIDTH), MediaAction::Control(Control::Previous), "content 12: ⏮'s right edge");
    assert_eq!(press(&mut p, &players, (14, 3), WIDTH), MediaAction::Control(Control::PlayPause), "content 13: ⏯'s left edge");
    assert_eq!(press(&mut p, &players, (23, 3), WIDTH), MediaAction::Control(Control::Next), "content 22: ⏭'s right edge");
    assert_eq!(press(&mut p, &players, (24, 3), WIDTH), MediaAction::Nothing, "past ⏭");
    assert_eq!(press(&mut p, &players, (7, 3), WIDTH), MediaAction::Nothing, "before ⏮");
    assert_eq!(press(&mut p, &players, (0, 3), WIDTH), MediaAction::Nothing, "the pad cell");
    assert_eq!(press(&mut p, &players, (5, 0), WIDTH), MediaAction::Nothing, "the title");
    assert_eq!(press(&mut p, &players, (5, 4), WIDTH), MediaAction::Nothing, "the separator");
    let second = players.shown().nth(1).map(|q| q.bus_name.clone()).unwrap();
    assert_eq!(press(&mut p, &players, (3, 6), WIDTH), MediaAction::Pin(second));
    assert_eq!(p.selected, 6);
    assert_eq!(press(&mut p, &players, (3, 40), WIDTH), MediaAction::Nothing, "below the rows");
    // The hit map is the drawn one, at the width the rows were drawn at.
    let wide = rows(&players, &p, t0, 40);
    assert_eq!(wide.rows[3].cells()[26].ch, SKIP_NEXT, "40 wide: ⏭ at content 25");
    assert_eq!(press(&mut p, &players, (26, 3), 40), MediaAction::Control(Control::Next));
    assert_eq!(press(&mut p, &players, (21, 3), 40), MediaAction::Control(Control::PlayPause), "⏯ at content 20");
}

#[test]
fn the_tick_runs_only_while_open_playing_and_before_the_end() {
    let len = Some(LEN as u64);
    assert_eq!(tick_due(false, true, 1.0, 102_250_000, len), None, "closed");
    assert_eq!(tick_due(true, false, 1.0, 102_250_000, len), None, "paused or stopped");
    assert_eq!(tick_due(true, true, 0.0, 102_250_000, len), None, "rate 0: the position stands still");
    assert_eq!(tick_due(true, true, f64::NAN, 102_250_000, len), None);
    assert_eq!(tick_due(true, true, 1.0, 102_250_000, None), None, "unknown length: no time row");
    assert_eq!(tick_due(true, true, 1.0, LEN as u64, len), None, "at the end: the timer stops");
    assert_eq!(tick_due(true, true, 1.0, 102_250_000, len), Some(Duration::from_millis(750) + SLACK), "the next whole second");
    assert_eq!(tick_due(true, true, 1.0, 102_000_000, len), Some(Duration::from_secs(1) + SLACK));
    // Faster than 1×, the whole second waited for is the first one at least
    // 1 − 1/rate real seconds away: about one redraw a real second whatever the rate, never 2
    // or 16, and 1× unchanged.
    assert_eq!(tick_due(true, true, 2.0, 102_250_000, len), Some(Duration::from_micros(875_000) + SLACK), "twice as fast: 104 s");
    assert_eq!(tick_due(true, true, 16.0, 102_000_000, len), Some(Duration::from_micros(937_500) + SLACK), "16×: 117 s");
    assert_eq!(tick_due(true, true, 0.5, 102_250_000, len), Some(Duration::from_millis(1500) + SLACK), "slower: the next second");
    assert_eq!(tick_due(true, true, 1.0, 244_900_000, len), Some(Duration::from_millis(100) + SLACK), "the last tick lands on the end");
    assert_eq!(tick_due(true, true, 2.0, 244_900_000, len), Some(Duration::from_millis(50) + SLACK), "even sooner");
    // 20 real seconds of playing, tick after tick: about 20 redraws at any rate from 1 up.
    for rate in [1.0, 2.0, 4.0, 16.0] {
        let (mut pos, mut real, mut ticks) = (0u64, Duration::ZERO, 0);
        while let Some(due) = tick_due(true, true, rate, pos, Some(10_000_000_000)) {
            real += due;
            if real > Duration::from_secs(20) {
                break;
            }
            pos += (due.as_secs_f64() * 1e6 * rate) as u64;
            ticks += 1;
        }
        assert!((19..=21).contains(&ticks), "rate {rate}: {ticks} ticks in 20 s");
    }
}

/// The position beyond the length (a player reporting 6:40 of a 4:05 track).
#[test]
fn a_position_beyond_the_length_is_clamped_and_stops_the_tick() {
    let t0 = Instant::now();
    let mut players = Players::default();
    let u = with(&mut players, "lumen", 7, props("Playing", "Paper Lanterns", &["The Quiet Hours"], Some(LEN)));
    players.set_position(&u, PositionSync { at: t0, us: 400_000_000 });
    let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
    assert_eq!(texts(&b)[2], format!(" 4:05 {} 4:05", "━".repeat(20)), "full, never past the bar");
    let p = players.get(&u).unwrap();
    let pos = media::position_now(p, t0).unwrap_or(0);
    assert_eq!(tick_due(true, true, p.rate, pos, p.track.length_us), None, "the end is reached: no tick");
    assert_eq!(tick_due(true, true, 1.0, 400_000_000, Some(LEN as u64)), None, "even unclamped");
}

/// A track change while the popup is open. The rows follow, and once the
/// position is read again (Mpris asks on a track change while open) the tick restarts from 0:00.
#[test]
fn a_track_change_while_open_updates_the_rows_and_the_tick() {
    let t0 = Instant::now();
    let mut players = one(t0);
    let mut popup = MediaPopup::new(&players);
    let before = rows(&players, &popup, t0, WIDTH);
    let _ = players.apply(":1.7", &props("Playing", "Second Song", &["Other Band"], Some(200_000_000)), &[]);
    players.set_position(":1.7", PositionSync { at: t0, us: 0 });
    popup.fit(&players);
    let after = rows(&players, &popup, t0, WIDTH);
    assert_ne!(before, after);
    let t = texts(&after);
    assert_eq!((t[0].as_str(), t[1].as_str()), (" Second Song", " Other Band"));
    assert_eq!(t[2], format!(" 0:00 {} 3:20", "─".repeat(20)));
    let p = players.get(":1.7").unwrap();
    assert_eq!(
        tick_due(true, true, p.rate, media::position_now(p, t0).unwrap_or(0), p.track.length_us),
        Some(Duration::from_secs(1) + SLACK)
    );
}

/// A pause while open. The timer is disarmed, and ⏯ shows play.
#[test]
fn a_pause_while_open_stops_the_tick_and_shows_play() {
    let t0 = Instant::now();
    let mut players = one(t0);
    let _ = players.apply(":1.7", &[("PlaybackStatus".to_string(), Value::Str("Paused".into()))], &[]);
    let p = players.get(":1.7").unwrap();
    assert_eq!(tick_due(true, p.status == Status::Playing, p.rate, 102_000_000, p.track.length_us), None);
    let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
    assert_eq!(b.rows[3].cells()[16].ch, PLAY);
}

/// Until the position is read (the popup just opened, or the read failed), the
/// time row stands at 0:00 — nothing to redraw, so no tick.
#[test]
fn an_unknown_position_never_ticks() {
    let t0 = Instant::now();
    let mut players = Players::default();
    let u = with(&mut players, "lumen", 7, props("Playing", "Paper Lanterns", &[], Some(LEN)));
    let p = players.get(&u).unwrap();
    assert_eq!(player_tick(true, p, t0), None, "position unknown");
    players.set_position(&u, PositionSync { at: t0, us: 102_250_000 });
    let p = players.get(&u).unwrap();
    assert_eq!(player_tick(true, p, t0), Some(Duration::from_millis(750) + SLACK), "read: the next whole second");
    assert_eq!(player_tick(false, p, t0), None, "closed");
}

/// A refresh (a key, a host output) never moves a tick already armed ahead.
#[test]
fn a_refresh_keeps_the_armed_tick() {
    let now = Instant::now();
    let second = Some(Duration::from_secs(1));
    assert_eq!(next_deadline(second, Some(now + Duration::from_millis(300)), now), Some(now + Duration::from_millis(300)), "kept");
    assert_eq!(next_deadline(second, Some(now), now), Some(now + Duration::from_secs(1)), "due now or past: a new one");
    assert_eq!(next_deadline(second, None, now), Some(now + Duration::from_secs(1)), "none armed");
    assert_eq!(next_deadline(None, Some(now + Duration::from_millis(300)), now), None, "no tick due: removed");
}

/// The "media popup shown" line carries what the popup shows on
/// top, as drawn (cut to its width), so a headless test can check the fake track reached it.
#[test]
fn the_heading_is_the_drawn_title_and_artist() {
    let t0 = Instant::now();
    let players = one(t0);
    let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
    assert_eq!(heading(&players, &b), "Paper Lanterns — The Quiet Hours");
    let mut players = Players::default();
    with(&mut players, "radio", 3, props("Playing", &"a".repeat(40), &[], None));
    let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
    assert_eq!(heading(&players, &b), format!("{}…", "a".repeat(29)), "no artist row: the title alone, as drawn");
    assert_eq!(heading(&Players::default(), &Boxed::default()), "", "nothing shown");
}

/// A tick whose rows did not change commits nothing (`popup_draw` compares with `last`): two
/// instants in the same second of the track render the very same rows.
#[test]
fn two_instants_in_the_same_second_render_the_same_rows() {
    let t0 = Instant::now();
    let players = one(t0);
    let popup = MediaPopup::new(&players);
    assert_eq!(rows(&players, &popup, t0, WIDTH), rows(&players, &popup, t0 + Duration::from_millis(400), WIDTH));
    assert_ne!(rows(&players, &popup, t0, WIDTH), rows(&players, &popup, t0 + Duration::from_millis(1_000), WIDTH));
}
