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
//!
//! Never:
//! - open a connection of its own: it rides the tray's or the bar's link, and never a second one;
//! - poll a player: the players are listed at start and followed through `NameOwnerChanged` and
//!   `PropertiesChanged`, and the position is read only while the popup is open
//!   (docs/decisions.md, "The media popup computes the position, and reads it only while open");
//! - send anything but the three controls: no seek, no volume, no raise
//!   (docs/features/media.md).
//!
//! See: docs/features/media.md ("How it is built"), docs/features/notifications.md (the panel's
//! card).

use super::bus::{Call, CallError, CallId, Outbox};
use super::codec::{Reply, Signal};
use super::{MPRIS_OWNER_RULE, MPRIS_PROPERTIES_RULE, MPRIS_SEEKED_RULE};
use crate::config::MediaConfig;
use crate::media::{self, Control, MAX_PLAYERS, MPRIS_PREFIX, MediaView, Need, Players, PositionSync, Status, Value};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
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
    /// The startup ListNames; true for a second attempt (a lost one is asked again once).
    List(bool),
    /// GetNameOwner of this well-known name (the startup inventory); true for a second attempt.
    Owner(String, bool),
    /// GetAll(Player) on this unique name.
    State(String),
    /// Get(root, Identity).
    Identity(String),
    /// Get(Player, Metadata), after an invalidation.
    Metadata(String),
    /// Get(Player, Position): the popup opened, or the track or status changed while it is open.
    Position(String),
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
    /// Listed names not resolved yet (MAX_PLAYERS at most, in listing order): the followed players
    /// plus the names in flight (`Ask::Owner`) stay within MAX_PLAYERS, so the rest wait here for
    /// a resolution to end — a failed one frees its slot for the next.
    waiting: Vec<String>,
    /// A ListNames was answered. Until then (lost twice to our timeout, at login on a slow bus), the
    /// next MPRIS arrival lists again: players already running are found without a timer.
    listed: bool,
    /// The Seeked rule is on: the popup is open.
    seeked_rule: bool,
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
            waiting: Vec::new(),
            listed: false,
            seeked_rule: false,
            log,
            #[cfg(test)]
            built: 0,
        };
        m.ask(out, Call::ListNames, Ask::List(false));
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

    /// Room for one more followed player. The first refusal of a full episode is logged; the rest
    /// are silent.
    fn room(&mut self, bus_name: &str) -> bool {
        if self.players.len() < MAX_PLAYERS {
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

    /// `bus_name` was released by `unique`: its player goes, only if it is that name's. One
    /// connection may own two MPRIS names (only the first is followed): the second one released
    /// leaves the first one's player alone. When the player goes, that connection may still own a
    /// second name: one ListNames, whatever `listed` says, finds it (`answer` skips the names
    /// already followed or resolving).
    fn release(&mut self, out: &mut impl Outbox, bus_name: &str, unique: &str) {
        if self.players.get(unique).is_some_and(|p| p.bus_name == bus_name) {
            self.leave(unique);
            // Not `relist`, which asks only while no listing ever succeeded: here one did, and it
            // skipped that connection's second name (its owner was followed already). That name
            // keeps its owner, so no NameOwnerChanged will ever announce it: only a new listing
            // finds it. One in flight serves several departures (its answer comes after them).
            self.list(out);
        }
    }

    /// The GetNameOwner calls in flight.
    fn resolving(&self) -> usize {
        self.asks.values().filter(|a| matches!(a, Ask::Owner(..))).count()
    }

    /// Resolves the waiting names (GetNameOwner), as many as fit next to the followed players and
    /// the names in flight; the others keep waiting for one of those to end. Full of followed
    /// players with nothing in flight, the rest are ignored (`room` logs it).
    fn resolve_waiting(&mut self, out: &mut impl Outbox) {
        while !self.waiting.is_empty() {
            let resolving = self.resolving();
            if self.players.len() + resolving >= MAX_PLAYERS {
                if resolving == 0 {
                    let name = self.waiting.remove(0);
                    self.room(&name);
                    self.waiting.clear();
                }
                return;
            }
            let name = self.waiting.remove(0);
            let asked = self.asks.values().any(|a| matches!(a, Ask::Owner(x, _) if *x == name));
            if !self.players.has_name(&name) && !asked {
                self.ask(out, Call::GetNameOwner { name: name.clone() }, Ask::Owner(name, false));
            }
        }
    }

    /// One ListNames, unless one is in flight already: its answer serves this asker too.
    fn list(&mut self, out: &mut impl Outbox) {
        if !self.asks.values().any(|a| matches!(a, Ask::List(_))) {
            self.ask(out, Call::ListNames, Ask::List(true));
        }
    }

    /// The listing again, after the lost ones (`listed`): once in flight at most.
    fn relist(&mut self, out: &mut impl Outbox) {
        if !self.listed {
            self.list(out);
        }
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
        // While the popup is open, a status or track change moves the position elsewhere: read it
        // again. Closed, nothing is cloned and the position Players::apply cleared stays unknown
        // until `popup_opened` reads it.
        let before = if self.seeked_rule { self.moment(unique) } else { None };
        let applied = self.players.apply(unique, changed, invalidated);
        self.stale |= applied.visible;
        if let Need::Metadata(u) = applied.need {
            let asked = self.asks.values().any(|a| matches!(a, Ask::Metadata(x) if *x == u));
            if !asked {
                self.ask_player(out, &u, |dest| Call::PlayerMetadata { dest }, Ask::Metadata);
            }
        }
        if before.is_some() && before != self.moment(unique) {
            self.ask_position(out, unique.to_string());
        }
    }

    pub fn answer(&mut self, out: &mut impl Outbox, id: CallId, reply: Result<Reply, CallError>) {
        let Some(why) = self.asks.remove(&id) else { return };
        match (why, reply) {
            (Ask::List(_), Ok(Reply::Names(names))) => {
                self.listed = true;
                // A listing after arrivals (`relist`) skips the names already followed, resolving or
                // waiting.
                let wanted: Vec<String> = names
                    .into_iter()
                    .filter(|n| self.followed(n) && !self.players.has_name(n) && !self.waiting.contains(n))
                    .filter(|n| !self.asks.values().any(|a| matches!(a, Ask::Owner(x, _) if x == n)))
                    .collect();
                self.waiting.extend(wanted);
                self.resolve_waiting(out);
                self.waiting.truncate(MAX_PLAYERS);
            }
            (Ask::List(again), Err(e)) => {
                if !again {
                    (self.log)(&format!("media: the players could not be listed ({e:?})"));
                }
                // Lost to our own timeout (a slow bus at login): asked once more, at once.
                if !again && e == CallError::Timeout {
                    self.ask(out, Call::ListNames, Ask::List(true));
                }
            }
            (Ask::Owner(name, _), Ok(Reply::Owner(unique))) => {
                self.arrive(out, &name, &unique);
                self.resolve_waiting(out);
            }
            // Lost to our own timeout: asked once more, in the slot it frees.
            (Ask::Owner(name, false), Err(CallError::Timeout)) => {
                if self.room(&name) {
                    self.ask(out, Call::GetNameOwner { name: name.clone() }, Ask::Owner(name, true));
                } else {
                    self.resolve_waiting(out);
                }
            }
            // Gone between ListNames and GetNameOwner (NameHasNoOwner), or lost twice: nothing to
            // follow, and its slot goes to a waiting name.
            (Ask::Owner(..), _) => self.resolve_waiting(out),
            (Ask::State(u) | Ask::Metadata(u), Ok(Reply::Props(props))) => self.apply(out, &u, &props, &[]),
            (Ask::Identity(u), Ok(Reply::Text(name))) => {
                if self.players.get(&u).is_some() {
                    self.players.set_identity(&u, &name);
                    self.stale = true;
                }
            }
            (Ask::Position(u), Ok(Reply::Position(us))) => {
                self.players.set_position(&u, PositionSync { at: Instant::now(), us: u64::try_from(us).unwrap_or(0) });
            }
            (Ask::State(u) | Ask::Identity(u) | Ask::Metadata(u) | Ask::Position(u), Err(CallError::Remote(e)))
                if GONE.contains(&e.as_str()) =>
            {
                self.leave(&u);
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
                    // Fresher than a GetNameOwner still in flight for this name, or its wait: that
                    // answer is dropped, the wait ends.
                    self.asks.retain(|_, a| !matches!(a, Ask::Owner(n, _) if n == name));
                    self.waiting.retain(|n| n != name);
                    // The slot it held, if it was in flight, goes to a waiting name.
                    self.resolve_waiting(out);
                    // A restart is one signal with both owners: the old one goes, the new one comes.
                    if !old.is_empty() {
                        self.release(out, name, old);
                    }
                    if !new.is_empty() {
                        self.arrive(out, name, new);
                    }
                }
                // The bus answers again: a listing lost at startup is asked once more.
                if !new.is_empty() {
                    self.relist(out);
                }
                true
            }
            Signal::PlayerProps { sender, changed, invalidated } => {
                self.apply(out, sender, changed, invalidated);
                true
            }
            Signal::Seeked { sender, us } => {
                // Followed only while the popup is open (the rule exists only then); a stray one
                // after the RemoveMatch, or from a sender that is no player, changes nothing. The
                // position is not in the bar's view: nothing to rebuild.
                if self.seeked_rule && self.players.get(sender).is_some() {
                    self.players.set_position(sender, PositionSync { at: Instant::now(), us: u64::try_from(*us).unwrap_or(0) });
                }
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

    /// Pins the player of `bus_name` (the popup's choice); an unknown name changes nothing.
    pub fn pin(&mut self, bus_name: &str) {
        if self.players.has_name(bus_name) {
            self.players.pin(bus_name);
            self.stale = true;
        }
    }

    /// The popup opened (spec § 3): `Seeked` is followed from now on (once), and the active player's
    /// position is read — never trusted from an earlier open, since a seek in the player's own
    /// window may have sent no Seeked at all.
    pub fn popup_opened(&mut self, out: &mut impl Outbox) {
        if !self.seeked_rule {
            out.send(Call::AddMatch { rule: MPRIS_SEEKED_RULE.to_string() });
            self.seeked_rule = true;
        }
        self.resync_active(out);
    }

    /// The popup closed: the Seeked rule goes (once). Positions already read stay; they are read
    /// again at the next open.
    pub fn popup_closed(&mut self, out: &mut impl Outbox) {
        if self.seeked_rule {
            out.send(Call::RemoveMatch { rule: MPRIS_SEEKED_RULE.to_string() });
            self.seeked_rule = false;
        }
    }

    /// One `Get Position` for the active player (the popup opened, a player was pinned in it).
    pub fn resync_active(&mut self, out: &mut impl Outbox) {
        if let Some(unique) = self.players.active().map(|p| p.unique.clone()) {
            self.ask_position(out, unique);
        }
    }

    fn ask_position(&mut self, out: &mut impl Outbox, unique: String) {
        self.ask(out, Call::PlayerPosition { dest: unique.clone() }, Ask::Position(unique));
    }

    /// What, of `unique`'s state, moves its position elsewhere: its status, its track and its rate —
    /// exactly what makes `Players::apply` forget the position, any field of
    /// the track included. The track and rate as a hash: compared once before and once after a
    /// change, never cloned.
    fn moment(&self, unique: &str) -> Option<(Status, u64)> {
        self.players.get(unique).map(|p| {
            let mut h = DefaultHasher::new();
            p.track.hash(&mut h);
            p.rate.to_bits().hash(&mut h);
            (p.status, h.finish())
        })
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
mod tests;
