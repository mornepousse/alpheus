use super::*;
use crate::media::{Status, Value};
use crate::session_bus::bus::Method;
use crate::session_bus::bus::testing::FakeOutbox;
use std::cell::RefCell;

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The test's logger: one entry per line the consumer logs (thread-local: tests run in parallel).
fn note(s: &str) {
    LINES.with(|l| l.borrow_mut().push(s.to_string()));
}

fn lines(pattern: &str) -> usize {
    LINES.with(|l| l.borrow().iter().filter(|x| x.contains(pattern)).count())
}

const TIDEWAVE: &str = "org.mpris.MediaPlayer2.tidewave";
const LANTERN: &str = "org.mpris.MediaPlayer2.lanternfm";

fn started() -> (FakeOutbox, Mpris) {
    let mut out = FakeOutbox::default();
    let m = Mpris::start_logged(&mut out, &MediaConfig::default(), note);
    (out, m)
}

fn owner_changed(name: &str, old: &str, new: &str) -> Signal {
    Signal::NameOwnerChanged { name: name.into(), old: old.into(), new: new.into() }
}

/// The id of the latest call equal to `call`.
fn id(out: &FakeOutbox, call: &Call) -> CallId {
    out.calls.iter().rev().find(|(_, c)| c == call).map(|(id, _)| *id).unwrap_or_else(|| panic!("never asked: {call:?}"))
}

fn get_all(unique: &str) -> Call {
    Call::PlayerGetAll { dest: unique.into() }
}

fn identity(unique: &str) -> Call {
    Call::PlayerIdentity { dest: unique.into() }
}

/// A GetAll answer: `status`, `title` by Marlowe Vane (3:20), every control allowed.
fn state(status: &str, title: &str) -> Vec<(String, Value)> {
    vec![
        ("PlaybackStatus".into(), Value::Str(status.into())),
        (
            "Metadata".into(),
            Value::Map(vec![
                ("xesam:title".into(), Value::Str(title.into())),
                ("xesam:artist".into(), Value::StrList(vec!["Marlowe Vane".into()])),
                ("mpris:length".into(), Value::Int(200_000_000)),
            ]),
        ),
        ("CanPlay".into(), Value::Bool(true)),
        ("CanPause".into(), Value::Bool(true)),
        ("CanGoNext".into(), Value::Bool(true)),
        ("CanGoPrevious".into(), Value::Bool(true)),
    ]
}

fn with(mut props: Vec<(String, Value)>, key: &str, v: Value) -> Vec<(String, Value)> {
    props.retain(|(k, _)| k != key);
    props.push((key.into(), v));
    props
}

/// `name` arrives as `unique`; its GetAll answers `props`, its Identity its last name segment.
fn loaded(m: &mut Mpris, out: &mut FakeOutbox, name: &str, unique: &str, props: Vec<(String, Value)>) {
    assert!(m.signal(out, &owner_changed(name, "", unique)));
    let all = id(out, &get_all(unique));
    m.answer(out, all, Ok(Reply::Props(props)));
    let who = id(out, &identity(unique));
    m.answer(out, who, Ok(Reply::Text(name.rsplit('.').next().unwrap_or_default().into())));
}

fn view(playing: bool, title: &str) -> Option<MediaView> {
    Some(MediaView { playing, title: title.into() })
}

#[test]
fn start_sends_the_two_rules_then_one_list_names() {
    let (out, m) = started();
    assert_eq!(
        out.sent,
        [Call::AddMatch { rule: MPRIS_OWNER_RULE.into() }, Call::AddMatch { rule: MPRIS_PROPERTIES_RULE.into() }],
        "rules first (no reply, nothing pending), so no arrival slips between them and the list"
    );
    assert_eq!(out.calls, [(CallId(1), Call::ListNames)]);
    assert!(m.owns(CallId(1)));
    assert_eq!((m.players().len(), m.view()), (0, None));
}

#[test]
fn listed_players_are_resolved_then_read_and_ignored_names_never_asked() {
    let (mut out, mut m) = started();
    let names = [
        ":1.1",
        "org.freedesktop.DBus",
        TIDEWAVE,
        "org.mpris.MediaPlayer2.playerctld",
        "org.mpris.MediaPlayer2.kdeconnect.mpris_phone",
        "org.mpris.MediaPlayer2",
        "org.mpris.MediaPlayer2Extra",
        LANTERN,
    ];
    assert_eq!(out.take(), [(CallId(1), Call::ListNames)]);
    m.answer(&mut out, CallId(1), Ok(Reply::Names(names.iter().map(|s| s.to_string()).collect())));
    let owners = out.ids(Method::GetNameOwner);
    let asked: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
    assert_eq!(
        asked,
        [Call::GetNameOwner { name: TIDEWAVE.into() }, Call::GetNameOwner { name: LANTERN.into() }],
        "MPRIS names only; playerctld and the kdeconnect default never asked"
    );
    assert_eq!(owners.len(), 2);
    m.answer(&mut out, owners[0], Ok(Reply::Owner(":1.7".into())));
    // Lantern left between ListNames and GetNameOwner: nobody owns the name any more.
    m.answer(&mut out, owners[1], Err(CallError::Remote("org.freedesktop.DBus.Error.NameHasNoOwner".into())));
    let asked: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
    assert_eq!(asked, [get_all(":1.7"), identity(":1.7")], "the unique name, never the well-known one");
    assert_eq!(m.players().len(), 1, "the vanished player is never added");
    // playerctld arriving later is never asked either.
    assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.playerctld", "", ":1.9")), "an MPRIS name: ours");
    assert!(out.take().is_empty());
    assert_eq!(m.players().len(), 1);
}

#[test]
fn a_restart_in_one_signal_replaces_the_player() {
    let (mut out, mut m) = started();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(vec![])));
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert_eq!(m.view(), view(true, "Harbor Lights"));
    out.take();
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, ":1.7", ":1.8")));
    let asked: Vec<Call> = out.calls.iter().map(|(_, c)| c.clone()).collect();
    assert_eq!(
        asked,
        [Call::ListNames, get_all(":1.8"), identity(":1.8")],
        "the old owner's other names listed, the new owner read; no GetNameOwner"
    );
    assert_eq!(m.players().len(), 1, "replaced, not doubled");
    assert_eq!(m.view(), None, "the new owner's state is not known before its GetAll");
    let all = id(&out, &get_all(":1.8"));
    m.answer(&mut out, all, Ok(Reply::Props(state("Paused", "Second Song"))));
    assert_eq!(m.view(), view(false, "Second Song"));
    assert_eq!(m.players().active().map(|p| p.unique.as_str()), Some(":1.8"));
}

#[test]
fn a_leaving_player_is_removed() {
    let (mut out, mut m) = started();
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, ":1.7", "")));
    assert_eq!((m.players().len(), m.view()), (0, None));
    assert!(!m.signal(&mut out, &owner_changed("org.kde.StatusNotifierWatcher", "", ":1.3")), "not an MPRIS name: the tray's");
    assert!(!m.signal(&mut out, &owner_changed(":1.7", ":1.7", "")), "a unique name leaving: the host's");
}

#[test]
fn the_17th_player_is_turned_away_with_one_log_line_per_full_episode() {
    let (mut out, mut m) = started();
    let names: Vec<String> = (0..=MAX_PLAYERS).map(|i| format!("org.mpris.MediaPlayer2.fake{i}")).collect();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(names.clone())));
    let owners = out.ids(Method::GetNameOwner);
    assert_eq!(owners.len(), MAX_PLAYERS, "17 listed, 16 resolved at once, one waiting");
    assert_eq!(lines("players already followed"), 0, "none followed yet: nothing to say");
    for (n, owner) in owners.iter().enumerate() {
        m.answer(&mut out, *owner, Ok(Reply::Owner(format!(":1.{}", 100 + n))));
    }
    assert_eq!(m.players().len(), MAX_PLAYERS);
    assert_eq!(out.ids(Method::GetNameOwner).len(), MAX_PLAYERS, "the 17th never asked");
    assert_eq!(lines(&format!("16 players already followed; {} and any more", names[MAX_PLAYERS])), 1, "16 followed: it is ignored");
    out.take();
    // Still full: another arrival is turned away, without a second line.
    assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.late", "", ":1.200")));
    assert!(out.take().is_empty(), "never queried");
    assert_eq!(lines("players already followed"), 1, "one line while full");
    // One leaves: room for one more, then full again: a new episode, a new line.
    assert!(m.signal(&mut out, &owner_changed(&names[0], ":1.100", "")));
    assert_eq!(out.take().into_iter().map(|(_, c)| c).collect::<Vec<_>>(), [Call::ListNames], "a release lists again");
    assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.later", "", ":1.201")));
    assert_eq!(out.take().len(), 2, "GetAll + Identity for the one that fits");
    assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.latest", "", ":1.202")));
    assert!(out.take().is_empty());
    assert_eq!(lines("players already followed"), 2);
    assert_eq!(m.players().len(), MAX_PLAYERS);
}

#[test]
fn invalidated_metadata_is_re_read_once() {
    let (mut out, mut m) = started();
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    out.take();
    let invalidated = Signal::PlayerProps { sender: ":1.7".into(), changed: vec![], invalidated: vec!["Metadata".into()] };
    assert!(m.signal(&mut out, &invalidated));
    assert!(m.signal(&mut out, &invalidated), "a second invalidation while the first Get is out");
    let asked: Vec<(CallId, Call)> = out.take();
    assert_eq!(asked.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::PlayerMetadata { dest: ":1.7".into() }], "once");
    let meta = Value::Map(vec![("xesam:title".into(), Value::Str("Second Song".into()))]);
    m.answer(&mut out, asked[0].0, Ok(Reply::Props(vec![("Metadata".into(), meta)])));
    assert_eq!(m.view(), view(true, "Second Song"));
    assert!(m.signal(&mut out, &invalidated));
    assert_eq!(out.take().len(), 1, "answered: the next invalidation asks again");
}

#[test]
fn service_unknown_no_reply_or_unknown_method_remove_the_player_and_nothing_else_does() {
    for gone in ["ServiceUnknown", "NoReply", "UnknownMethod"] {
        let (mut out, mut m) = started();
        assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, "", ":1.7")));
        let all = id(&out, &get_all(":1.7"));
        m.answer(&mut out, all, Err(CallError::Remote(format!("org.freedesktop.DBus.Error.{gone}"))));
        assert_eq!(m.players().len(), 0, "{gone}");
    }
    for kept in [
        CallError::Timeout,
        CallError::BadReply("PlaybackStatus is \"i\"".into()),
        CallError::Remote("org.freedesktop.DBus.Error.InvalidArgs".into()),
    ] {
        let (mut out, mut m) = started();
        assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, "", ":1.7")));
        let who = id(&out, &identity(":1.7"));
        m.answer(&mut out, who, Err(kept.clone()));
        assert_eq!(m.players().len(), 1, "{kept:?}: the player stays");
    }
}

/// A player whose PropertiesChanged arrives before its GetAll answer.
#[test]
fn a_change_arriving_before_the_get_all_answer_is_kept_then_completed() {
    let (mut out, mut m) = started();
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, "", ":1.7")));
    let all = id(&out, &get_all(":1.7"));
    let early = Signal::PlayerProps {
        sender: ":1.7".into(),
        changed: vec![("PlaybackStatus".into(), Value::Str("Playing".into()))],
        invalidated: vec![],
    };
    assert!(m.signal(&mut out, &early));
    assert_eq!(m.players().len(), 1, "applied to the entry already there, never dropped for being early");
    assert_eq!(m.players().active().map(|p| p.status), Some(Status::Playing));
    // The player answered after it signalled: its GetAll is the newer state and completes it.
    m.answer(&mut out, all, Ok(Reply::Props(state("Playing", "Harbor Lights"))));
    assert_eq!(m.view(), view(true, "Harbor Lights"));
}

#[test]
fn control_goes_to_the_active_player_only_when_capable() {
    let (mut out, mut m) = started();
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", with(state("Playing", "Harbor Lights"), "CanGoNext", Value::Bool(false)));
    loaded(&mut m, &mut out, LANTERN, ":1.8", state("Paused", "Night Ferry"));
    out.sent.clear();
    m.control(&mut out, Control::PlayPause);
    m.control(&mut out, Control::Next);
    m.control(&mut out, Control::Previous);
    assert_eq!(
        out.sent,
        [
            Call::PlayerControl { dest: ":1.7".into(), member: Control::PlayPause },
            Call::PlayerControl { dest: ":1.7".into(), member: Control::Previous },
        ],
        "the playing one only; Next dropped: CanGoNext is false"
    );
    assert!(out.calls.iter().all(|(_, c)| !matches!(c, Call::PlayerControl { .. })), "sent, never a pending call");
    let no_pause =
        Signal::PlayerProps { sender: ":1.7".into(), changed: vec![("CanPause".into(), Value::Bool(false))], invalidated: vec![] };
    assert!(m.signal(&mut out, &no_pause));
    out.sent.clear();
    m.control(&mut out, Control::PlayPause);
    assert!(out.sent.is_empty(), "playing and unable to pause: a no-op");
    for (name, unique) in [(TIDEWAVE, ":1.7"), (LANTERN, ":1.8")] {
        assert!(m.signal(&mut out, &owner_changed(name, unique, "")));
    }
    m.control(&mut out, Control::Previous);
    assert!(out.sent.is_empty(), "no player: nothing");
}

#[test]
fn the_view_changes_only_when_what_the_bar_shows_changes() {
    let (mut out, mut m) = started();
    assert_eq!(m.take_view_change(), None, "nothing yet: nothing to repaint");
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert_eq!(m.take_view_change(), Some(view(true, "Harbor Lights")));
    assert_eq!(m.take_view_change(), None, "taken");
    let rate = Signal::PlayerProps { sender: ":1.7".into(), changed: vec![("Rate".into(), Value::Float(2.0))], invalidated: vec![] };
    assert!(m.signal(&mut out, &rate));
    assert_eq!(m.take_view_change(), None, "a chatty property the bar does not show: no repaint");
    let paused = Signal::PlayerProps {
        sender: ":1.7".into(),
        changed: vec![("PlaybackStatus".into(), Value::Str("Paused".into()))],
        invalidated: vec![],
    };
    assert!(m.signal(&mut out, &paused));
    assert_eq!(m.take_view_change(), Some(view(false, "Harbor Lights")));
    let stopped = Signal::PlayerProps {
        sender: ":1.7".into(),
        changed: vec![("PlaybackStatus".into(), Value::Str("Stopped".into()))],
        invalidated: vec![],
    };
    assert!(m.signal(&mut out, &stopped));
    assert_eq!(m.take_view_change(), Some(None), "every player stopped: the module goes");
}

#[test]
fn seeked_resyncs_the_position() {
    let (mut out, mut m) = started();
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    m.popup_opened(&mut out);
    assert!(m.signal(&mut out, &Signal::Seeked { sender: ":1.7".into(), us: 5_000_000 }));
    assert_eq!(m.players().active().and_then(|p| p.position).map(|s| s.us), Some(5_000_000));
    assert!(m.signal(&mut out, &Signal::Seeked { sender: ":1.7".into(), us: -3 }));
    assert_eq!(m.players().active().and_then(|p| p.position).map(|s| s.us), Some(0), "a negative position is 0");
}

/// The PropertiesChanged rule has no sender filter, so any peer may broadcast
/// one on the MPRIS object. A sender that is not a followed player's unique name is dropped.
#[test]
fn a_stranger_signalling_on_the_mpris_object_changes_nothing() {
    let (mut out, mut m) = started();
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert!(m.take_view_change().is_some());
    out.take();
    let built = m.built;
    let forged = Signal::PlayerProps {
        sender: ":1.99".into(),
        changed: vec![
            ("PlaybackStatus".into(), Value::Str("Paused".into())),
            ("Metadata".into(), Value::Map(vec![("xesam:title".into(), Value::Str("Forged".into()))])),
        ],
        invalidated: vec!["Metadata".into()],
    };
    assert!(m.signal(&mut out, &forged), "an MPRIS signal: never the tray host's");
    assert!(m.signal(&mut out, &Signal::Seeked { sender: ":1.99".into(), us: 1 }));
    assert!(out.take().is_empty(), "nothing asked of a stranger");
    assert_eq!(m.view(), view(true, "Harbor Lights"));
    assert_eq!(m.players().active().and_then(|p| p.position), None);
    assert_eq!(m.take_view_change(), None);
    assert_eq!(m.built, built, "dropped before it reaches the model: nothing to rebuild");
}

/// A call never goes to a well-known name; only owners the bus driver gave
/// as unique names (":1.N") enter the owner table, and every call takes its destination there.
#[test]
fn a_well_known_owner_is_never_followed_nor_called() {
    let (mut out, mut m) = started();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(vec![TIDEWAVE.into()])));
    let owner = out.ids(Method::GetNameOwner)[0];
    out.take();
    m.answer(&mut out, owner, Ok(Reply::Owner(TIDEWAVE.into())));
    assert!(m.signal(&mut out, &owner_changed(LANTERN, "", "org.example.Impostor")));
    assert!(out.take().is_empty(), "no call to a well-known name");
    assert_eq!(m.players().len(), 0);
    m.control(&mut out, Control::PlayPause);
    assert!(out.sent.iter().all(|c| matches!(c, Call::AddMatch { .. })), "{:?}", out.sent);
}

/// `view()` allocates (`truncate_title`), so `take_view_change` builds it only
/// after something that can move it, never on every batch.
#[test]
fn the_view_is_built_only_after_a_change() {
    let (mut out, mut m) = started();
    assert_eq!(m.take_view_change(), None);
    assert_eq!(m.built, 0, "nothing changed yet: nothing built");
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert_eq!(m.take_view_change(), Some(view(true, "Harbor Lights")));
    let built = m.built;
    assert_eq!(m.take_view_change(), None);
    assert!(m.signal(&mut out, &Signal::Seeked { sender: ":1.7".into(), us: 5_000_000 }));
    assert!(!m.signal(&mut out, &owner_changed("org.kde.StatusNotifierWatcher", "", ":1.3")));
    assert_eq!(m.take_view_change(), None);
    assert_eq!(m.built, built, "a seek or a foreign signal: not rebuilt");
    let paused = Signal::PlayerProps {
        sender: ":1.7".into(),
        changed: vec![("PlaybackStatus".into(), Value::Str("Paused".into()))],
        invalidated: vec![],
    };
    assert!(m.signal(&mut out, &paused));
    assert_eq!(m.take_view_change(), Some(view(false, "Harbor Lights")));
    assert_eq!(m.built, built + 1);
}

/// Rate and Volume are shown nowhere, an unknown pin moves nothing: neither
/// rebuilds the view. A capability is shown (the popup's buttons): it does.
#[test]
fn unseen_changes_never_rebuild_the_view() {
    let (mut out, mut m) = started();
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert!(m.take_view_change().is_some());
    let built = m.built;
    let props = |changed: Vec<(String, Value)>| Signal::PlayerProps { sender: ":1.7".into(), changed, invalidated: vec![] };
    assert!(m.signal(&mut out, &props(vec![("Rate".into(), Value::Float(2.0))])));
    assert!(m.signal(&mut out, &props(vec![("Volume".into(), Value::Float(0.4))])));
    assert!(m.signal(&mut out, &props(state("Playing", "Harbor Lights"))), "the same state again");
    m.pin("org.mpris.MediaPlayer2.nobody");
    assert_eq!(m.take_view_change(), None);
    assert_eq!(m.built, built, "Rate, Volume, a repeat, an unknown pin: not rebuilt");
    assert!(m.signal(&mut out, &props(vec![("CanGoNext".into(), Value::Bool(false))])));
    assert_eq!(m.take_view_change(), None, "the bar's text is the same");
    assert_eq!(m.built, built + 1, "but the popup shows the buttons: rebuilt");
    m.pin(TIDEWAVE);
    m.take_view_change();
    assert_eq!(m.built, built + 2, "a known pin: rebuilt");
}

/// One connection may own two MPRIS names; only the first is followed. The
/// second one released must not take the first one's player away.
#[test]
fn a_released_second_name_of_the_same_connection_leaves_the_player() {
    let (mut out, mut m) = started();
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert!(m.signal(&mut out, &owner_changed(LANTERN, "", ":1.7")));
    assert_eq!(m.players().len(), 1, "the second name of :1.7 is not followed");
    assert!(m.signal(&mut out, &owner_changed(LANTERN, ":1.7", "")));
    assert_eq!(m.players().len(), 1, "LANTERN released: TIDEWAVE stays");
    assert_eq!(m.view(), view(true, "Harbor Lights"));
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, ":1.7", "")));
    assert_eq!(m.players().len(), 0, "its own name released: gone");
}

/// When a followed player's name is released, the same connection may still own a
/// second MPRIS name (never followed while the first one was): one ListNames, even once listed,
/// picks it up. A release that removes nothing lists nothing.
/// A resolution that fails frees its slot for a listed name waiting behind the 16 in
/// flight; only followed players fill the cap.
#[test]
fn a_failed_resolution_frees_its_slot_for_a_waiting_name() {
    let (mut out, mut m) = started();
    let names: Vec<String> = (0..=MAX_PLAYERS).map(|i| format!("org.mpris.MediaPlayer2.fake{i}")).collect();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(names.clone())));
    let owners = out.ids(Method::GetNameOwner);
    out.take();
    m.answer(&mut out, owners[0], Err(CallError::Remote("org.freedesktop.DBus.Error.NameHasNoOwner".into())));
    let asked = out.take();
    assert_eq!(
        asked.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
        [Call::GetNameOwner { name: names[MAX_PLAYERS].clone() }],
        "the 17th, in the freed slot"
    );
    for (n, owner) in owners.iter().enumerate().skip(1) {
        m.answer(&mut out, *owner, Ok(Reply::Owner(format!(":1.{}", 100 + n))));
    }
    m.answer(&mut out, asked[0].0, Ok(Reply::Owner(":1.300".into())));
    assert_eq!(m.players().len(), MAX_PLAYERS, "15 + the 17th");
    assert_eq!(lines("players already followed"), 0, "nobody was turned away");
}

/// The last resolution in flight ends because its name vanishes (a NameOwnerChanged
/// with no new owner), not because its answer came: the waiting name still gets the slot.
#[test]
fn a_name_vanishing_mid_resolution_frees_its_slot_for_a_waiting_name() {
    let (mut out, mut m) = started();
    let names: Vec<String> = (0..=MAX_PLAYERS).map(|i| format!("org.mpris.MediaPlayer2.fake{i}")).collect();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(names.clone())));
    let owners = out.ids(Method::GetNameOwner);
    out.take();
    for (n, owner) in owners.iter().enumerate().take(MAX_PLAYERS - 1) {
        m.answer(&mut out, *owner, Ok(Reply::Owner(format!(":1.{}", 100 + n))));
    }
    out.take();
    assert!(m.signal(&mut out, &owner_changed(&names[MAX_PLAYERS - 1], ":1.999", "")));
    let asked: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
    assert_eq!(asked, [Call::GetNameOwner { name: names[MAX_PLAYERS].clone() }], "the 17th, in the freed slot");
}

/// A timed-out resolution is asked once more only if there is room, as the first
/// ask is; a player that arrived meanwhile took the slot.
#[test]
fn a_lost_get_name_owner_is_not_retried_without_room() {
    let (mut out, mut m) = started();
    let names: Vec<String> = (0..MAX_PLAYERS).map(|i| format!("org.mpris.MediaPlayer2.fake{i}")).collect();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(names)));
    let owners = out.ids(Method::GetNameOwner);
    for (n, owner) in owners.iter().enumerate().take(MAX_PLAYERS - 1) {
        m.answer(&mut out, *owner, Ok(Reply::Owner(format!(":1.{}", 100 + n))));
    }
    assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.late", "", ":1.500")));
    out.take();
    assert_eq!(m.players().len(), MAX_PLAYERS);
    m.answer(&mut out, owners[MAX_PLAYERS - 1], Err(CallError::Timeout));
    assert!(!out.take().iter().any(|(_, c)| matches!(c, Call::GetNameOwner { .. })), "full: no retry");
}

/// Only followed players fill the cap. A player arriving while 16 listed names are
/// still being resolved is followed; the one resolution that finds no room is the one turned
/// away, and the line says so truly.
#[test]
fn an_arrival_is_followed_while_listed_names_resolve() {
    let (mut out, mut m) = started();
    let names: Vec<String> = (0..MAX_PLAYERS).map(|i| format!("org.mpris.MediaPlayer2.fake{i}")).collect();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(names.clone())));
    let owners = out.ids(Method::GetNameOwner);
    assert_eq!(owners.len(), MAX_PLAYERS);
    out.take();
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, "", ":1.7")));
    assert_eq!(out.take().len(), 2, "followed at once: GetAll + Identity");
    assert_eq!(lines("players already followed"), 0, "nobody is followed yet but it");
    for (n, owner) in owners.iter().enumerate() {
        m.answer(&mut out, *owner, Ok(Reply::Owner(format!(":1.{}", 100 + n))));
    }
    assert_eq!(m.players().len(), MAX_PLAYERS);
    assert_eq!(lines(&format!("16 players already followed; {} and any more", names[MAX_PLAYERS - 1])), 1);
}

/// Departures in one batch share one listing; a new one is asked once it answered.
#[test]
fn departures_share_one_listing_in_flight() {
    let (mut out, mut m) = started();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(vec![])));
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    loaded(&mut m, &mut out, LANTERN, ":1.8", state("Paused", "Paper Lanterns"));
    out.take();
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, ":1.7", "")));
    assert!(m.signal(&mut out, &owner_changed(LANTERN, ":1.8", "")));
    let asked = out.take();
    assert_eq!(asked.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::ListNames], "one listing for both");
    m.answer(&mut out, asked[0].0, Ok(Reply::Names(vec![])));
    loaded(&mut m, &mut out, TIDEWAVE, ":1.9", state("Playing", "Harbor Lights"));
    out.take();
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, ":1.9", "")));
    assert_eq!(out.take().into_iter().map(|(_, c)| c).collect::<Vec<_>>(), [Call::ListNames], "answered: the next departure lists again");
}

#[test]
fn a_released_player_lists_again_so_a_second_name_of_its_connection_is_followed() {
    let (mut out, mut m) = started();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(vec![])));
    loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
    assert!(m.signal(&mut out, &owner_changed(LANTERN, "", ":1.7")));
    out.take();
    assert!(m.signal(&mut out, &owner_changed(LANTERN, ":1.7", "")));
    assert!(out.take().is_empty(), "LANTERN was never followed: nothing removed, nothing listed");
    assert!(m.signal(&mut out, &owner_changed(LANTERN, "", ":1.7")));
    out.take();
    assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, ":1.7", "")));
    let asked = out.take();
    assert_eq!(asked.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::ListNames], "one listing, though listed already");
    m.answer(&mut out, asked[0].0, Ok(Reply::Names(vec![LANTERN.into()])));
    let owner = out.take();
    assert_eq!(owner.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::GetNameOwner { name: LANTERN.into() }]);
    m.answer(&mut out, owner[0].0, Ok(Reply::Owner(":1.7".into())));
    assert_eq!(out.take().into_iter().map(|(_, c)| c).collect::<Vec<_>>(), [get_all(":1.7"), identity(":1.7")]);
    assert_eq!(m.players().get(":1.7").map(|p| p.bus_name.as_str()), Some(LANTERN), "the second name followed now");
}

/// A ListNames lost to our timeout is asked once more at once; if that
/// one is lost too, the next MPRIS arrival lists again, so players already running are found
/// without a timer. Once listed, never again.
#[test]
fn a_lost_list_names_is_asked_again_once_then_on_the_next_arrival() {
    let (mut out, mut m) = started();
    assert_eq!(out.take(), [(CallId(1), Call::ListNames)]);
    m.answer(&mut out, CallId(1), Err(CallError::Timeout));
    let again = out.take();
    assert_eq!(again.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::ListNames], "one retry at once");
    m.answer(&mut out, again[0].0, Err(CallError::Timeout));
    assert!(out.take().is_empty(), "the retry lost too: no third call, no timer");
    assert_eq!(lines("could not be listed"), 1, "one line");
    // A player arrives: it is followed, and the listing is asked once more.
    assert!(m.signal(&mut out, &owner_changed(LANTERN, "", ":1.8")));
    let asked = out.take();
    let calls: Vec<Call> = asked.iter().map(|(_, c)| c.clone()).collect();
    assert_eq!(calls, [get_all(":1.8"), identity(":1.8"), Call::ListNames]);
    assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.other", "", ":1.9")));
    assert!(!out.take().iter().any(|(_, c)| *c == Call::ListNames), "one in flight: not twice");
    let list = asked[2].0;
    m.answer(&mut out, list, Ok(Reply::Names(vec![TIDEWAVE.into(), LANTERN.into(), "org.mpris.MediaPlayer2.other".into()])));
    let calls: Vec<Call> = out.take().into_iter().map(|(_, c)| c).collect();
    assert_eq!(calls, [Call::GetNameOwner { name: TIDEWAVE.into() }], "only the one not yet followed");
    assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.late", "", ":1.10")));
    assert!(!out.take().iter().any(|(_, c)| *c == Call::ListNames), "listed: never again");
}

/// A GetNameOwner lost to our timeout is asked once more, never twice.
#[test]
fn a_lost_get_name_owner_is_asked_again_once() {
    let (mut out, mut m) = started();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(vec![TIDEWAVE.into()])));
    let first = out.ids(Method::GetNameOwner);
    out.take();
    m.answer(&mut out, first[0], Err(CallError::Timeout));
    let again = out.take();
    assert_eq!(again.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::GetNameOwner { name: TIDEWAVE.into() }]);
    m.answer(&mut out, again[0].0, Err(CallError::Timeout));
    assert!(out.take().is_empty(), "lost twice: given up");
    // A name with no owner any more is not a lost call: never asked again.
    let (mut out, mut m) = started();
    m.answer(&mut out, CallId(1), Ok(Reply::Names(vec![TIDEWAVE.into()])));
    let first = out.ids(Method::GetNameOwner);
    out.take();
    m.answer(&mut out, first[0], Err(CallError::Remote("org.freedesktop.DBus.Error.NameHasNoOwner".into())));
    assert!(out.take().is_empty());
}

const FAKE: &str = "org.mpris.MediaPlayer2.fake";
const UNIQUE: &str = ":1.7";

fn meta(title: &str, length_us: i64) -> Value {
    Value::Map(vec![
        ("mpris:trackid".to_string(), Value::Str(format!("/fake/{title}"))),
        ("xesam:title".to_string(), Value::Str(title.to_string())),
        ("mpris:length".to_string(), Value::Int(length_us)),
    ])
}

fn fake_props(status: &str) -> Vec<(String, Value)> {
    vec![
        ("PlaybackStatus".to_string(), Value::Str(status.to_string())),
        ("Metadata".to_string(), meta("Paper Lanterns", 245_000_000)),
        ("CanPlay".to_string(), Value::Bool(true)),
        ("CanPause".to_string(), Value::Bool(true)),
    ]
}

/// Answers every call `m` made as the one fake player would, until it asks nothing more.
fn serve(m: &mut Mpris, out: &mut FakeOutbox, status: &str, position_us: i64) {
    for _ in 0..16 {
        let calls = out.take();
        if calls.is_empty() {
            return;
        }
        for (id, call) in calls {
            let reply = match call {
                Call::ListNames => Reply::Names(vec!["org.freedesktop.DBus".to_string(), FAKE.to_string()]),
                Call::GetNameOwner { .. } => Reply::Owner(UNIQUE.to_string()),
                Call::PlayerGetAll { .. } => Reply::Props(fake_props(status)),
                Call::PlayerIdentity { .. } => Reply::Text("Fake Player".to_string()),
                Call::PlayerMetadata { .. } => Reply::Props(vec![("Metadata".to_string(), meta("Paper Lanterns", 245_000_000))]),
                Call::PlayerPosition { .. } => Reply::Position(position_us),
                _ => Reply::Unit,
            };
            m.answer(out, id, Ok(reply));
        }
    }
    panic!("Mpris kept asking");
}

/// Discovered, every call answered, the sends forgotten.
fn discovered(status: &str) -> (Mpris, FakeOutbox) {
    let mut out = FakeOutbox::default();
    let mut m = Mpris::start(&mut out, &MediaConfig::default());
    serve(&mut m, &mut out, status, 0);
    assert!(m.players().get(UNIQUE).is_some(), "the fake player is known");
    out.sent.clear();
    (m, out)
}

fn position(m: &Mpris) -> Option<u64> {
    m.players().get(UNIQUE).and_then(|p| p.position).map(|s| s.us)
}

fn positions_asked(out: &FakeOutbox) -> Vec<CallId> {
    out.calls.iter().filter(|(_, c)| matches!(c, Call::PlayerPosition { .. })).map(|(id, _)| *id).collect()
}

fn seeked_rules(out: &FakeOutbox, add: bool) -> usize {
    out.sent
        .iter()
        .filter(|c| match c {
            Call::AddMatch { rule } if add => rule == MPRIS_SEEKED_RULE,
            Call::RemoveMatch { rule } if !add => rule == MPRIS_SEEKED_RULE,
            _ => false,
        })
        .count()
}

#[test]
fn the_popup_adds_the_seeked_rule_once_and_reads_the_position() {
    let (mut m, mut out) = discovered("Playing");
    m.popup_opened(&mut out);
    m.popup_opened(&mut out);
    assert_eq!(seeked_rules(&out, true), 1, "one rule, however often it is asked");
    let asked: Vec<Call> = out.calls.iter().map(|(_, c)| c.clone()).collect();
    let read = Call::PlayerPosition { dest: UNIQUE.to_string() };
    assert_eq!(asked, [read.clone(), read], "one read per open, to the unique name, nothing else");
    serve(&mut m, &mut out, "Playing", 100_000_000);
    assert_eq!(position(&m), Some(100_000_000));
    m.popup_closed(&mut out);
    m.popup_closed(&mut out);
    assert_eq!(seeked_rules(&out, false), 1, "removed once");
}

#[test]
fn seeked_moves_the_position_only_while_the_popup_is_open() {
    let (mut m, mut out) = discovered("Playing");
    let seeked = Signal::Seeked { sender: UNIQUE.to_string(), us: 5_000_000 };
    assert!(m.signal(&mut out, &seeked), "an MPRIS signal is ours even when ignored");
    assert_eq!(position(&m), None, "closed: a stray Seeked changes nothing");
    m.popup_opened(&mut out);
    serve(&mut m, &mut out, "Playing", 1_000_000);
    assert!(m.signal(&mut out, &seeked));
    assert_eq!(position(&m), Some(5_000_000));
    let stranger = Signal::Seeked { sender: ":1.99".to_string(), us: 9_000_000 };
    m.signal(&mut out, &stranger);
    assert_eq!(position(&m), Some(5_000_000), "a sender that is no player");
}

/// A player that never sends Seeked after a seek in its own UI. Reopening reads the
/// position again; nothing kept from the last open is trusted.
#[test]
fn reopening_reads_the_position_again() {
    let (mut m, mut out) = discovered("Playing");
    m.popup_opened(&mut out);
    serve(&mut m, &mut out, "Playing", 10_000_000);
    m.popup_closed(&mut out);
    // The user seeks to 1:40 in the player's own window; no Seeked comes.
    m.popup_opened(&mut out);
    assert_eq!(positions_asked(&out).len(), 1, "one more read at the reopen");
    serve(&mut m, &mut out, "Playing", 100_000_000);
    assert_eq!(position(&m), Some(100_000_000));
}

/// The track changes while the popup is open. The position is read again (the
/// new track starts elsewhere); with the popup closed, nothing is asked.
#[test]
fn a_track_or_status_change_rereads_the_position_only_while_open() {
    let (mut m, mut out) = discovered("Playing");
    let next_track = Signal::PlayerProps {
        sender: UNIQUE.to_string(),
        changed: vec![("Metadata".to_string(), meta("Second Song", 200_000_000))],
        invalidated: vec![],
    };
    m.signal(&mut out, &next_track);
    assert!(positions_asked(&out).is_empty(), "closed: no read");
    m.popup_opened(&mut out);
    serve(&mut m, &mut out, "Playing", 50_000_000);
    let third = Signal::PlayerProps {
        sender: UNIQUE.to_string(),
        changed: vec![("Metadata".to_string(), meta("Third Song", 180_000_000))],
        invalidated: vec![],
    };
    m.signal(&mut out, &third);
    assert_eq!(positions_asked(&out).len(), 1, "a new track while open: read");
    serve(&mut m, &mut out, "Playing", 0);
    assert_eq!(position(&m), Some(0));
    let pause = Signal::PlayerProps {
        sender: UNIQUE.to_string(),
        changed: vec![("PlaybackStatus".to_string(), Value::Str("Paused".to_string()))],
        invalidated: vec![],
    };
    m.signal(&mut out, &pause);
    assert_eq!(positions_asked(&out).len(), 1, "a pause while open: read");
    serve(&mut m, &mut out, "Paused", 3_000_000);
    let volume_only =
        Signal::PlayerProps { sender: UNIQUE.to_string(), changed: vec![("Volume".to_string(), Value::Float(0.5))], invalidated: vec![] };
    m.signal(&mut out, &volume_only);
    assert!(positions_asked(&out).is_empty(), "neither status nor track: no read");
}

/// `Players::apply` forgets the position on ANY change of the track — an
/// artist corrected alone too — so `moment` must see it, or the popup stays at 0:00 unread.
#[test]
fn an_artist_only_change_while_open_rereads_the_position() {
    let (mut m, mut out) = discovered("Playing");
    m.popup_opened(&mut out);
    serve(&mut m, &mut out, "Playing", 50_000_000);
    let mut entries = match meta("Paper Lanterns", 245_000_000) {
        Value::Map(e) => e,
        _ => unreachable!(),
    };
    entries.push(("xesam:artist".to_string(), Value::StrList(vec!["Someone Else".to_string()])));
    let artist = Signal::PlayerProps {
        sender: UNIQUE.to_string(),
        changed: vec![("Metadata".to_string(), Value::Map(entries))],
        invalidated: vec![],
    };
    m.signal(&mut out, &artist);
    assert_eq!(position(&m), None, "apply forgot it");
    assert_eq!(positions_asked(&out).len(), 1, "so it is read again");
}

/// A new Rate while open moves where the track will be: `Players::apply` forgets the
/// position, so it is read again (else the elapsed time jumps by the time since × the new rate).
#[test]
fn a_rate_change_while_open_rereads_the_position() {
    let (mut m, mut out) = discovered("Playing");
    m.popup_opened(&mut out);
    serve(&mut m, &mut out, "Playing", 50_000_000);
    let rate = |r: f64| Signal::PlayerProps {
        sender: UNIQUE.to_string(),
        changed: vec![("Rate".to_string(), Value::Float(r))],
        invalidated: vec![],
    };
    m.signal(&mut out, &rate(1.0));
    assert!(positions_asked(&out).is_empty(), "the same rate: no read");
    m.signal(&mut out, &rate(2.0));
    assert_eq!(position(&m), None, "apply forgot it");
    assert_eq!(positions_asked(&out).len(), 1, "so it is read again");
    out.take();
    m.popup_closed(&mut out);
    m.signal(&mut out, &rate(1.0));
    assert!(positions_asked(&out).is_empty(), "closed: no read");
}

#[test]
fn a_pin_in_the_open_popup_reads_the_pinned_position() {
    let (mut m, mut out) = discovered("Playing");
    m.popup_opened(&mut out);
    serve(&mut m, &mut out, "Playing", 0);
    m.pin(FAKE);
    m.resync_active(&mut out);
    assert_eq!(out.calls.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(), [Call::PlayerPosition { dest: UNIQUE.to_string() }]);
}
