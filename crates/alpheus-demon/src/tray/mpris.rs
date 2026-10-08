//! The MPRIS consumer (media spec § 3–4). It finds the players on the session bus, follows their
//! state into `media::Players` (pure), and sends them PlayPause, Next or Previous.
//!
//! It owns no connection. It rides whichever `Outbox` hosts it: the tray's, or the bar's own
//! session link (link.rs). It speaks only through the closed `Call`, `Reply` and `Signal`
//! vocabulary, so it never names the dbus crate.
//!
//! Every call goes to a unique name (from `GetNameOwner` or `NameOwnerChanged`), never to a
//! well-known name, so a call cannot reach a new owner by surprise. Never name a function
//! `method_call` here: the guardrails count that substring.

use super::bus::{Call, CallError, CallId, Outbox};
use super::codec::{Reply, Signal};
use super::{MPRIS_OWNER_RULE, MPRIS_PROPERTIES_RULE};
use crate::config::MediaConfig;
use crate::media::{self, Control, MAX_PLAYERS, MPRIS_PREFIX, MediaView, Need, Players, PositionSync, Value};
use std::collections::HashMap;
use std::time::Instant;

/// Error replies meaning the player is gone or is no MPRIS player (spec § 3): its entry goes. Our own
/// 2 s timeout is not among them: a player busy for 2 s keeps its place, and its next
/// PropertiesChanged fills it in.
const GONE: [&str; 3] =
    ["org.freedesktop.DBus.Error.ServiceUnknown", "org.freedesktop.DBus.Error.NoReply", "org.freedesktop.DBus.Error.UnknownMethod"];

/// A unique name (":1.N"), the only kind of owner the consumer follows: a well-known name could
/// change hands between two calls.
fn is_unique(name: &str) -> bool {
    name.starts_with(':')
}

/// What one of the consumer's calls was for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    /// The startup ListNames.
    List,
    /// GetNameOwner of this well-known name (the startup inventory).
    Owner(String),
    /// GetAll(Player) on this unique name.
    State(String),
    /// Get(root, Identity).
    Identity(String),
    /// Get(Player, Metadata), after an invalidation.
    Metadata(String),
}

pub struct Mpris {
    players: Players,
    asks: HashMap<CallId, Ask>,
    ignore: Vec<String>,
    title_max: u16,
    /// What `take_view_change` last handed out.
    shown: Option<MediaView>,
    /// Something the view depends on may have moved since `shown` was built: `take_view_change`
    /// builds the view (an allocation, `truncate_title`) only then, never on every batch.
    stale: bool,
    /// The cap was reached and logged; cleared when a player leaves.
    full: bool,
    log: fn(&str),
    /// How many times `take_view_change` built the view (the tests' witness of `stale`).
    #[cfg(test)]
    built: usize,
}

impl Mpris {
    pub fn start(out: &mut impl Outbox, cfg: &MediaConfig) -> Mpris {
        Mpris::start_logged(out, cfg, crate::log::write)
    }

    /// The two rules first, sent with no reply (nothing pending for them), so no arrival slips
    /// between them and the list; then the one ListNames (pending, 2 s deadline).
    pub fn start_logged(out: &mut impl Outbox, cfg: &MediaConfig, log: fn(&str)) -> Mpris {
        out.send(Call::AddMatch { rule: MPRIS_OWNER_RULE.to_string() });
        out.send(Call::AddMatch { rule: MPRIS_PROPERTIES_RULE.to_string() });
        let mut m = Mpris {
            players: Players::default(),
            asks: HashMap::new(),
            ignore: cfg.ignore.clone(),
            title_max: cfg.title_max,
            shown: None,
            stale: false,
            full: false,
            log,
            #[cfg(test)]
            built: 0,
        };
        m.ask(out, Call::ListNames, Ask::List);
        m
    }

    fn ask(&mut self, out: &mut impl Outbox, call: Call, why: Ask) {
        let id = out.call(call);
        self.asks.insert(id, why);
    }

    /// Asks `unique` (a followed player) one question. The destination is the owner table's own
    /// value, so a player call never goes to a name the table does not hold: only unique names
    /// enter it (`arrive`).
    fn ask_player(&mut self, out: &mut impl Outbox, unique: &str, call: fn(String) -> Call, why: fn(String) -> Ask) {
        if let Some(dest) = self.players.get(unique).map(|p| p.unique.clone()) {
            self.ask(out, call(dest.clone()), why(dest));
        }
    }

    pub fn owns(&self, id: CallId) -> bool {
        self.asks.contains_key(&id)
    }

    /// An MPRIS player's name, not ignored (playerctld always is).
    fn followed(&self, bus_name: &str) -> bool {
        bus_name.starts_with(MPRIS_PREFIX) && !Players::is_ignored(bus_name, &self.ignore)
    }

    /// Room for one more: the players plus the names being resolved stay within MAX_PLAYERS. The first
    /// refusal of a full episode is logged; the rest are silent.
    fn room(&mut self, bus_name: &str) -> bool {
        let resolving = self.asks.values().filter(|a| matches!(a, Ask::Owner(_))).count();
        if self.players.len() + resolving < MAX_PLAYERS {
            return true;
        }
        if !self.full {
            self.full = true;
            (self.log)(&format!("media: {MAX_PLAYERS} players already followed; {bus_name} and any more are ignored"));
        }
        false
    }

    /// `bus_name` is owned by `unique`: followed and read once (GetAll, Identity), room permitting.
    /// An owner that is not a unique name never enters the owner table.
    fn arrive(&mut self, out: &mut impl Outbox, bus_name: &str, unique: &str) {
        if !is_unique(unique) || !self.room(bus_name) || !self.players.add(bus_name.to_string(), unique.to_string()) {
            return;
        }
        self.stale = true;
        self.ask_player(out, unique, |dest| Call::PlayerGetAll { dest }, Ask::State);
        self.ask_player(out, unique, |dest| Call::PlayerIdentity { dest }, Ask::Identity);
    }

    fn leave(&mut self, unique: &str) {
        if self.players.remove_unique(unique) {
            self.full = false;
            self.stale = true;
        }
    }

    /// Applies a change (GetAll, a Metadata Get, PropertiesChanged) to a followed player; an
    /// invalidated Metadata is re-read once, never twice in flight. Anyone else's is dropped.
    fn apply(&mut self, out: &mut impl Outbox, unique: &str, changed: &[(String, Value)], invalidated: &[String]) {
        if self.players.get(unique).is_none() {
            return;
        }
        self.stale = true;
        if let Need::Metadata(u) = self.players.apply(unique, changed, invalidated) {
            let asked = self.asks.values().any(|a| matches!(a, Ask::Metadata(x) if *x == u));
            if !asked {
                self.ask_player(out, &u, |dest| Call::PlayerMetadata { dest }, Ask::Metadata);
            }
        }
    }

    pub fn answer(&mut self, out: &mut impl Outbox, id: CallId, reply: Result<Reply, CallError>) {
        let Some(why) = self.asks.remove(&id) else { return };
        match (why, reply) {
            (Ask::List, Ok(Reply::Names(names))) => {
                let wanted: Vec<String> = names.into_iter().filter(|n| self.followed(n)).collect();
                for name in wanted {
                    if self.room(&name) {
                        self.ask(out, Call::GetNameOwner { name: name.clone() }, Ask::Owner(name));
                    }
                }
            }
            (Ask::List, Err(e)) => (self.log)(&format!("media: the players could not be listed ({e:?})")),
            (Ask::Owner(name), Ok(Reply::Owner(unique))) => self.arrive(out, &name, &unique),
            // Gone between ListNames and GetNameOwner (NameHasNoOwner), or no answer: nothing to follow.
            (Ask::Owner(_), _) => {}
            (Ask::State(u) | Ask::Metadata(u), Ok(Reply::Props(props))) => self.apply(out, &u, &props, &[]),
            (Ask::Identity(u), Ok(Reply::Text(name))) => {
                if self.players.get(&u).is_some() {
                    self.players.set_identity(&u, &name);
                    self.stale = true;
                }
            }
            (Ask::State(u) | Ask::Identity(u) | Ask::Metadata(u), Err(CallError::Remote(e))) if GONE.contains(&e.as_str()) => {
                self.leave(&u)
            }
            // A timeout, a malformed answer, a missing Identity: the player stays as it is.
            _ => {}
        }
    }

    /// True for an MPRIS signal (NameOwnerChanged of an `org.mpris.MediaPlayer2.*` name, a Player
    /// PropertiesChanged, Seeked): the consumer's, never the tray host's. PropertiesChanged and
    /// Seeked count only from a followed player's unique name: their rules carry no sender, so any
    /// peer may broadcast them.
    pub fn signal(&mut self, out: &mut impl Outbox, s: &Signal) -> bool {
        match s {
            Signal::NameOwnerChanged { name, old, new } if name.starts_with(MPRIS_PREFIX) => {
                if self.followed(name) {
                    // Fresher than a GetNameOwner still in flight for this name: that answer is dropped.
                    self.asks.retain(|_, a| !matches!(a, Ask::Owner(n) if n == name));
                    // A restart is one signal with both owners: the old one goes, the new one comes.
                    if !old.is_empty() {
                        self.leave(old);
                    }
                    if !new.is_empty() {
                        self.arrive(out, name, new);
                    }
                }
                true
            }
            Signal::PlayerProps { sender, changed, invalidated } => {
                self.apply(out, sender, changed, invalidated);
                true
            }
            Signal::Seeked { sender, us } => {
                // The position is not in the bar's view: nothing to rebuild.
                self.players.set_position(sender, PositionSync { at: Instant::now(), us: u64::try_from(*us).unwrap_or(0) });
                true
            }
            _ => false,
        }
    }

    /// PlayPause, Next or Previous to the active player, if it can (spec § 1, `Player::can`): sent
    /// with no reply expected, so nothing is pending and no timer arms. The destination is the
    /// owner table's unique name.
    pub fn control(&mut self, out: &mut impl Outbox, c: Control) {
        if let Some(p) = self.players.active().filter(|p| p.can(c)) {
            out.send(Call::PlayerControl { dest: p.unique.clone(), member: c });
        }
    }

    pub fn pin(&mut self, bus_name: &str) {
        self.players.pin(bus_name);
        self.stale = true;
    }

    pub fn players(&self) -> &Players {
        &self.players
    }

    /// The bar's view, built now (it allocates: `truncate_title`). Never per redraw: the wiring
    /// keeps what `take_view_change` handed out.
    pub fn view(&self) -> Option<MediaView> {
        media::view(&self.players, self.title_max)
    }

    /// The view when it differs from the one last taken (the bar repaints only then). Built only
    /// when something it depends on may have moved since.
    pub fn take_view_change(&mut self) -> Option<Option<MediaView>> {
        if !self.stale {
            return None;
        }
        self.stale = false;
        #[cfg(test)]
        {
            self.built += 1;
        }
        let now = self.view();
        if now == self.shown {
            return None;
        }
        self.shown = now.clone();
        Some(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{Status, Value};
    use crate::tray::bus::Method;
    use crate::tray::bus::testing::FakeOutbox;
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
        loaded(&mut m, &mut out, TIDEWAVE, ":1.7", state("Playing", "Harbor Lights"));
        assert_eq!(m.view(), view(true, "Harbor Lights"));
        out.take();
        assert!(m.signal(&mut out, &owner_changed(TIDEWAVE, ":1.7", ":1.8")));
        let asked: Vec<Call> = out.calls.iter().map(|(_, c)| c.clone()).collect();
        assert_eq!(asked, [get_all(":1.8"), identity(":1.8")], "the new owner is read; no GetNameOwner");
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
        assert_eq!(owners.len(), MAX_PLAYERS, "17 listed, 16 resolved");
        assert_eq!(lines("players already followed"), 1);
        for (n, owner) in owners.iter().enumerate() {
            m.answer(&mut out, *owner, Ok(Reply::Owner(format!(":1.{}", 100 + n))));
        }
        assert_eq!(m.players().len(), MAX_PLAYERS);
        out.take();
        // Still full: another arrival is turned away, without a second line.
        assert!(m.signal(&mut out, &owner_changed("org.mpris.MediaPlayer2.late", "", ":1.200")));
        assert!(out.take().is_empty(), "never queried");
        assert_eq!(lines("players already followed"), 1, "one line while full");
        // One leaves: room for one more, then full again: a new episode, a new line.
        assert!(m.signal(&mut out, &owner_changed(&names[0], ":1.100", "")));
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

    /// Review Focus: a player whose PropertiesChanged arrives before its GetAll answer.
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
        assert!(m.signal(&mut out, &Signal::Seeked { sender: ":1.7".into(), us: 5_000_000 }));
        assert_eq!(m.players().active().and_then(|p| p.position).map(|s| s.us), Some(5_000_000));
        assert!(m.signal(&mut out, &Signal::Seeked { sender: ":1.7".into(), us: -3 }));
        assert_eq!(m.players().active().and_then(|p| p.position).map(|s| s.us), Some(0), "a negative position is 0");
    }

    /// Task 3's review: the PropertiesChanged rule has no sender filter, so any peer may broadcast
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

    /// Task 3's review: a call never goes to a well-known name; only owners the bus driver gave
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

    /// Task 3's review: `view()` allocates (`truncate_title`), so `take_view_change` builds it only
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
}
