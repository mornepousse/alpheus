//! The media module's MPRIS consumer on a private session bus (`common::PrivateBus`, nothing
//! activatable), with fake players: never the workstation's buses, never a real player. Ignored by
//! default:
//! `nice -n 19 env CARGO_BUILD_JOBS=4 nix develop -c cargo test -p alpheus-demon --test media_private_bus -- --ignored`

mod common;

use alpheus_commun::theme::Mode;
use alpheus_demon::config::MediaConfig;
use alpheus_demon::link::{LinkOutput, SessionLink};
use alpheus_demon::media::{Control, MAX_PLAYERS, MediaView};
use alpheus_demon::tray::bus::{Call, Incoming, SessionBus, refusal};
use alpheus_demon::tray::codec;
use alpheus_demon::tray::mpris::Mpris;
use alpheus_demon::tray::router::{Role, Tray, TrayOutput};
use common::mpris::{FakePlayer, Spec, metadata};
use common::portal::FakePortal;
use common::sni::FakeWatcher;
use common::{Peer, PrivateBus};
use std::cell::RefCell;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

const GET_ALL: &str = "org.freedesktop.DBus.Properties.GetAll(org.mpris.MediaPlayer2.Player)";
const IDENTITY: &str = "org.freedesktop.DBus.Properties.Get(org.mpris.MediaPlayer2, Identity)";
const METADATA: &str = "org.freedesktop.DBus.Properties.Get(org.mpris.MediaPlayer2.Player, Metadata)";

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The consumer's logger in these tests (thread-local: each test pumps on its own thread).
fn note(s: &str) {
    LINES.with(|l| l.borrow_mut().push(s.to_string()));
}

fn lines(pattern: &str) -> usize {
    LINES.with(|l| l.borrow().iter().filter(|x| x.contains(pattern)).count())
}

fn shown(playing: bool, title: &str) -> Option<MediaView> {
    Some(MediaView { playing, title: title.into() })
}

/// `Mpris` on its own `SessionBus`, routed as the tray and the bar's link route it: answers decoded
/// by `codec::reply`, signals by `codec::signal`, calls refused; a flush's drained answers routed
/// too (bounded, as `SETTLE_ROUNDS`).
struct Driver {
    bus: SessionBus,
    mpris: Mpris,
    /// Every `take_view_change`, in order.
    views: Vec<Option<MediaView>>,
}

impl Driver {
    fn start(bus: &PrivateBus) -> Driver {
        Driver::with(bus, &MediaConfig::default())
    }

    fn with(bus: &PrivateBus, cfg: &MediaConfig) -> Driver {
        let mut session = SessionBus::at(&bus.address).unwrap();
        let mpris = Mpris::start_logged(&mut session, cfg, note);
        session.flush();
        Driver { bus: session, mpris, views: Vec::new() }
    }

    fn dispatch(&mut self, incoming: Vec<Incoming>) {
        for i in incoming {
            match i {
                Incoming::Answer { id, call, result } => {
                    let result = result.and_then(|m| codec::reply(&call, &m));
                    self.mpris.answer(&mut self.bus, id, result);
                }
                Incoming::Signal(m) => {
                    self.mpris.signal(&mut self.bus, &codec::signal(&m));
                }
                Incoming::MethodCall(m) => self.bus.send_message(refusal(&m)),
                Incoming::Closed => {}
            }
        }
    }

    fn route(&mut self, incoming: Vec<Incoming>) {
        self.dispatch(incoming);
        for _ in 0..8 {
            self.bus.flush();
            if !self.bus.has_ready() {
                break;
            }
            let ready = self.bus.take_ready();
            self.dispatch(ready);
        }
        if let Some(v) = self.mpris.take_view_change() {
            self.views.push(v);
        }
    }

    /// One pass of the bar's loop: what is readable, then the calls due.
    fn pump(&mut self) {
        let mut incoming = self.bus.read();
        if self.bus.next_deadline().is_some_and(|d| d <= Instant::now()) {
            incoming.extend(self.bus.expire(Instant::now()));
        }
        self.route(incoming);
    }

    /// Pumps until `done` holds (10 s at most), or panics with `what`.
    fn until(&mut self, what: &str, done: impl Fn(&Driver) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            assert!(Instant::now() < end, "never: {what}");
            self.pump();
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Pumps for `d`, whatever happens.
    fn idle(&mut self, d: Duration) {
        let end = Instant::now() + d;
        while Instant::now() < end {
            self.pump();
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn control(&mut self, c: Control) {
        self.mpris.control(&mut self.bus, c);
        self.route(Vec::new());
    }

    /// (pending calls, next deadline): (0, None) means no timer can be armed.
    fn drained(&self) -> (usize, Option<Instant>) {
        (self.bus.pending(), self.bus.next_deadline())
    }
}

fn settled(d: &Driver) -> bool {
    d.drained() == (0, None)
}

/// Polls the bus driver until nobody owns `name` (5 s at most).
fn wait_gone(bus: &PrivateBus, name: &str) {
    let peer = Peer::connect(bus);
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let m = dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner")
            .unwrap()
            .append1(name);
        if !peer.call(m).unwrap().read1::<bool>().unwrap() {
            return;
        }
        assert!(Instant::now() < end, "{name} still owned");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn players_present_at_start_are_found_and_read_once() {
    let bus = PrivateBus::start("media-start");
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let lantern = FakePlayer::start(&bus, Spec::paused("lanternfm", "Night Ferry"));
    let mut d = Driver::start(&bus);
    d.until("both read", |d| d.mpris.players().len() == 2 && settled(d));
    assert_eq!(d.mpris.view(), shown(true, "Harbor Lights"), "the playing one is active");
    assert_eq!(d.mpris.players().active().and_then(|p| p.identity.clone()).as_deref(), Some("Tidewave"));
    assert_eq!(tidewave.calls(), [GET_ALL, IDENTITY], "one GetAll, one Identity, nothing else");
    assert_eq!(lantern.calls(), [GET_ALL, IDENTITY]);
    assert_eq!(d.views.last(), Some(&shown(true, "Harbor Lights")));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_player_arriving_later_is_found() {
    let bus = PrivateBus::start("media-arrival");
    let mut d = Driver::start(&bus);
    d.until("the empty list answered", settled);
    assert_eq!(d.mpris.view(), None);
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    d.until("found on arrival", |d| d.mpris.view().is_some() && settled(d));
    assert_eq!(d.mpris.view(), shown(true, "Harbor Lights"));
    assert_eq!(tidewave.calls(), [GET_ALL, IDENTITY]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_restart_carried_by_one_name_owner_changed_replaces_the_player() {
    let bus = PrivateBus::start("media-restart");
    let first = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut d = Driver::start(&bus);
    d.until("first read", |d| d.mpris.view().is_some() && settled(d));
    // The fake's own check: the bus really sends ONE signal with both owners.
    let watcher = Peer::connect(&bus);
    let rule = "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.mpris.MediaPlayer2.tidewave'";
    let add = dbus::Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch")
        .unwrap()
        .append1(rule);
    watcher.call(add).unwrap();
    let before = first.calls();
    let second = FakePlayer::replace(&bus, Spec::paused("tidewave", "Second Song"));
    let end = Instant::now() + Duration::from_secs(5);
    let signal = loop {
        assert!(Instant::now() < end, "no NameOwnerChanged");
        if let Ok(Some(m)) = watcher.channel.blocking_pop_message(Duration::from_millis(50))
            && m.member().as_deref() == Some("NameOwnerChanged")
        {
            break m.read3::<String, String, String>().unwrap();
        }
    };
    assert_eq!(signal, ("org.mpris.MediaPlayer2.tidewave".into(), first.unique.clone(), second.unique.clone()), "one signal, both owners");
    d.until("the new owner read", |d| d.mpris.view() == shown(false, "Second Song") && settled(d));
    assert_eq!(d.mpris.players().len(), 1, "replaced, never doubled");
    assert_eq!(d.mpris.players().active().map(|p| p.unique.clone()), Some(second.unique.clone()));
    assert_eq!(second.calls(), [GET_ALL, IDENTITY]);
    assert_eq!(first.calls(), before, "the old owner is not asked again");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_player_leaving_is_removed() {
    let bus = PrivateBus::start("media-leave");
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut d = Driver::start(&bus);
    d.until("read", |d| d.mpris.view().is_some() && settled(d));
    tidewave.kill();
    // Its connection might own another MPRIS name: one listing follows, answered with nothing.
    d.until("removed", |d| d.mpris.players().is_empty() && settled(d));
    assert_eq!(d.mpris.view(), None);
    assert_eq!(d.views.last(), Some(&None), "the module hides");
    assert_eq!(d.drained(), (0, None));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn ignored_names_and_playerctld_are_never_queried() {
    let bus = PrivateBus::start("media-ignore");
    let phone = FakePlayer::start(&bus, Spec::playing("kdeconnect.mpris_phone", "Pocket Song"));
    let mut d = Driver::start(&bus);
    let playerctld = FakePlayer::start(&bus, Spec::playing("playerctld", "Proxy Song"));
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    d.until("tidewave read", |d| d.mpris.view().is_some() && settled(d));
    d.idle(Duration::from_millis(300));
    assert_eq!(d.mpris.players().len(), 1);
    assert_eq!(phone.calls(), Vec::<String>::new(), "kdeconnect (the default ignore list), present at start");
    assert_eq!(playerctld.calls(), Vec::<String>::new(), "playerctld, always ignored, arriving later");
    assert_eq!(tidewave.calls(), [GET_ALL, IDENTITY]);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_17th_player_is_ignored_with_one_log_line() {
    let bus = PrivateBus::start("media-cap");
    let first: Vec<FakePlayer> = (0..MAX_PLAYERS).map(|n| FakePlayer::start(&bus, Spec::paused(&format!("fake{n}"), "Song"))).collect();
    let mut d = Driver::start(&bus);
    d.until("16 read", |d| d.mpris.players().len() == MAX_PLAYERS && settled(d));
    assert_eq!(lines("players already followed"), 0);
    let extra = FakePlayer::start(&bus, Spec::playing("extra", "Too Many"));
    d.idle(Duration::from_millis(500));
    assert_eq!(d.mpris.players().len(), MAX_PLAYERS);
    assert_eq!(extra.calls(), Vec::<String>::new(), "never queried");
    assert_eq!(lines("players already followed"), 1);
    assert!(first.iter().all(|p| p.calls() == [GET_ALL, IDENTITY]));
    assert_eq!(d.drained(), (0, None));
}

/// Review Focus: 50 players arriving at once.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn fifty_players_arriving_at_once_are_capped_at_16_with_one_log_line() {
    let bus = PrivateBus::start("media-fifty");
    let mut d = Driver::start(&bus);
    d.until("the empty list answered", settled);
    // Not pumped while they arrive: their 50 NameOwnerChanged wait in one batch.
    let players: Vec<FakePlayer> = (0..50).map(|n| FakePlayer::start(&bus, Spec::paused(&format!("crowd{n}"), "Song"))).collect();
    d.until("16 read", |d| d.mpris.players().len() == MAX_PLAYERS && settled(d));
    d.idle(Duration::from_millis(500));
    let asked = players.iter().filter(|p| !p.calls().is_empty()).count();
    assert_eq!(asked, MAX_PLAYERS, "34 never queried");
    assert_eq!(lines("players already followed"), 1, "one line, not 34");
    assert_eq!(d.drained(), (0, None));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn invalidated_metadata_is_re_read() {
    let bus = PrivateBus::start("media-invalidated");
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut d = Driver::start(&bus);
    d.until("read", |d| d.mpris.view().is_some() && settled(d));
    tidewave.invalidate_metadata(metadata("Second Song"));
    d.until("re-read", |d| d.mpris.view() == shown(true, "Second Song") && settled(d));
    assert_eq!(tidewave.calls(), [GET_ALL, IDENTITY, METADATA]);
    // A plain change carries its value: nothing is re-read.
    tidewave.set("Metadata", metadata("Third Song"));
    d.until("applied", |d| d.mpris.view() == shown(true, "Third Song"));
    assert_eq!(tidewave.calls().len(), 3);
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_service_unknown_or_unknown_method_answer_removes_the_player() {
    for error in ["org.freedesktop.DBus.Error.ServiceUnknown", "org.freedesktop.DBus.Error.UnknownMethod"] {
        let bus = PrivateBus::start("media-unknown");
        let broken = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights").failing(error));
        let mut d = Driver::start(&bus);
        d.until("answered", |d| broken.calls().len() == 2 && settled(d));
        assert_eq!((d.mpris.players().len(), d.mpris.view()), (0, None), "{error}");
    }
}

/// Review Focus: a player that vanishes between ListNames and GetNameOwner.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_player_vanishing_between_list_names_and_get_name_owner_is_never_added() {
    let bus = PrivateBus::start("media-vanish");
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut session = SessionBus::at(&bus.address).unwrap();
    let mpris = Mpris::start_logged(&mut session, &MediaConfig::default(), note);
    session.flush();
    // The ListNames answer arrives; it is held, not routed yet.
    let mut held = Vec::new();
    let end = Instant::now() + Duration::from_secs(5);
    while !held.iter().any(|i| matches!(i, Incoming::Answer { call: Call::ListNames, .. })) {
        assert!(Instant::now() < end, "no ListNames answer");
        held.extend(session.read());
        thread::sleep(Duration::from_millis(5));
    }
    // The player leaves before the consumer asks who owns its name.
    let calls = tidewave.kill();
    wait_gone(&bus, "org.mpris.MediaPlayer2.tidewave");
    let mut d = Driver { bus: session, mpris, views: Vec::new() };
    d.route(held);
    d.until("GetNameOwner answered", settled);
    d.idle(Duration::from_millis(200));
    assert_eq!(d.mpris.players().len(), 0, "NameHasNoOwner: nothing to follow");
    assert_eq!(calls, Vec::<String>::new(), "never queried");
    assert_eq!(d.drained(), (0, None));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn control_goes_to_the_active_player_only_and_only_when_capable() {
    let bus = PrivateBus::start("media-control");
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights").with("CanGoNext", common::mpris::Prop::Bool(false)));
    let lantern = FakePlayer::start(&bus, Spec::paused("lanternfm", "Night Ferry"));
    let mut d = Driver::start(&bus);
    d.until("both read", |d| d.mpris.players().len() == 2 && settled(d));
    d.control(Control::Next);
    d.control(Control::Previous);
    d.control(Control::PlayPause);
    assert!(tidewave.wait_call("PlayPause", Duration::from_secs(5)).is_some());
    d.idle(Duration::from_millis(300));
    let controls =
        |p: &FakePlayer| -> Vec<String> { p.calls().into_iter().filter(|c| c.starts_with("org.mpris.MediaPlayer2.Player.")).collect() };
    assert_eq!(
        controls(&tidewave),
        ["org.mpris.MediaPlayer2.Player.Previous()", "org.mpris.MediaPlayer2.Player.PlayPause()"],
        "Next never sent: CanGoNext is false"
    );
    assert_eq!(controls(&lantern), Vec::<String>::new(), "the paused one is not active");
    d.until("the pause seen", |d| d.mpris.view() == shown(false, "Harbor Lights"));
    assert_eq!(d.drained(), (0, None), "controls are sent without a reply: nothing pending, no timer");
}

/// Task 3's review: the PropertiesChanged rule has no sender filter, so any peer can broadcast one on
/// the MPRIS object; only a followed player's own count.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_stranger_broadcasting_on_the_mpris_object_changes_nothing() {
    let bus = PrivateBus::start("media-stranger");
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut d = Driver::start(&bus);
    d.until("read", |d| d.mpris.view().is_some() && settled(d));
    let views = d.views.len();
    let stranger = Peer::connect(&bus);
    let mut changed = dbus::arg::PropMap::new();
    changed.insert("PlaybackStatus".into(), dbus::arg::Variant(Box::new("Paused".to_string())));
    let forged = dbus::Message::new_signal(common::mpris::OBJECT, "org.freedesktop.DBus.Properties", "PropertiesChanged").unwrap().append3(
        common::mpris::PLAYER,
        changed,
        vec!["Metadata".to_string()],
    );
    stranger.channel.send(forged).unwrap();
    stranger.channel.flush();
    d.idle(Duration::from_millis(300));
    assert_eq!(d.mpris.view(), shown(true, "Harbor Lights"), "the stranger's pause is not believed");
    assert_eq!(d.views.len(), views, "no repaint");
    assert_eq!(tidewave.calls(), [GET_ALL, IDENTITY], "its invalidated Metadata re-reads nothing");
    assert_eq!(d.drained(), (0, None));
    // The same signal from the player itself does count: the rule delivers it, the sender decides.
    tidewave.set("PlaybackStatus", common::mpris::text("Paused"));
    d.until("the player's own pause", |d| d.mpris.view() == shown(false, "Harbor Lights"));
}

/// Pumps `tray` as the bar's loop does until `done` holds or `within` passes; every non-empty output.
fn pump_tray(tray: &mut Tray, within: Duration, done: impl Fn(&Tray) -> bool) -> Vec<TrayOutput> {
    let end = Instant::now() + within;
    let mut outs = Vec::new();
    loop {
        let mut out = tray.on_readable();
        if tray.next_deadline().is_some_and(|d| d <= Instant::now()) {
            out.merge(tray.on_deadline(Instant::now()));
        }
        if tray.has_deferred() {
            out.merge(tray.take_deferred());
        }
        if out != TrayOutput::default() {
            outs.push(out);
        }
        if done(&*tray) || Instant::now() >= end {
            return outs;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn media_loaded(t: &Tray) -> bool {
    t.media().is_some_and(|m| m.view().is_some()) && t.next_deadline().is_none()
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_tray_carries_the_media_module_on_its_connection() {
    let bus = PrivateBus::start("media-tray");
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), PathBuf::from("/proc"));
    tray.start_media(&MediaConfig::default());
    tray.start_media(&MediaConfig::default()); // a second call is a no-op
    let outs = pump_tray(&mut tray, Duration::from_secs(5), media_loaded);
    assert!(media_loaded(&tray), "{outs:?}");
    assert!(outs.iter().any(|o| o.media == Some(shown(true, "Harbor Lights"))), "{outs:?}");
    assert_eq!(tidewave.calls(), [GET_ALL, IDENTITY], "one consumer, not two");
    tray.media_do(|m, bus| m.control(bus, Control::Next));
    assert_eq!(tidewave.wait_call("Next", Duration::from_secs(5)).as_deref(), Some("org.mpris.MediaPlayer2.Player.Next()"));
    assert_eq!(tray.next_deadline(), None, "nothing pending: no timer");
    // The player's own signals reach the consumer through the tray's routing, and the bar hears of it.
    tidewave.set("PlaybackStatus", common::mpris::text("Paused"));
    let paused = |t: &Tray| t.media().is_some_and(|m| m.view() == shown(false, "Harbor Lights"));
    let outs = pump_tray(&mut tray, Duration::from_secs(5), paused);
    assert!(outs.iter().any(|o| o.media == Some(shown(false, "Harbor Lights"))), "{outs:?}");
}

/// Review Focus: the tray's watcher restarting while media players exist.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_trays_watcher_changing_hands_leaves_the_players_alone() {
    let bus = PrivateBus::start("media-watcher");
    let other = FakeWatcher::start(&bus);
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), PathBuf::from("/proc"));
    tray.start_media(&MediaConfig::default());
    pump_tray(&mut tray, Duration::from_secs(5), |t| matches!(t.role(), Role::HostOnly { .. }) && media_loaded(t));
    assert!(matches!(tray.role(), Role::HostOnly { .. }) && media_loaded(&tray));
    let before = tidewave.calls();
    other.kill();
    let mut outs = pump_tray(&mut tray, Duration::from_secs(5), |t| *t.role() == Role::Watcher && t.next_deadline().is_none());
    outs.extend(pump_tray(&mut tray, Duration::from_millis(300), |_| false));
    assert_eq!(*tray.role(), Role::Watcher, "the takeover still happens: {outs:?}");
    assert_eq!(tray.media().map(|m| m.players().len()), Some(1));
    assert_eq!(tidewave.calls(), before, "nothing re-read");
    assert!(outs.iter().all(|o| o.media.is_none()), "the bar's media text untouched: {outs:?}");
}

/// Review Focus (Tray side; the daemon side is task 5's headless test): the session bus dying.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_dead_session_bus_takes_the_media_module_away() {
    let mut bus = PrivateBus::start("media-bus-dies");
    let _tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), PathBuf::from("/proc"));
    tray.start_media(&MediaConfig::default());
    pump_tray(&mut tray, Duration::from_secs(5), media_loaded);
    bus.kill();
    let outs = pump_tray(&mut tray, Duration::from_secs(5), |_| false);
    let closed = outs.iter().find(|o| o.bus_closed).expect("bus_closed");
    assert_eq!(closed.media, Some(None), "the module goes with the bus");
    assert_eq!(tray.next_deadline(), None, "nothing left pending");
    assert_eq!(tray.on_readable(), TrayOutput::default(), "a read afterwards does nothing");
}

/// Pumps `link` until `done` holds or `within` passes; every non-empty output.
fn pump_link(link: &mut SessionLink, within: Duration, done: impl Fn(&SessionLink) -> bool) -> Vec<LinkOutput> {
    let end = Instant::now() + within;
    let mut outs = Vec::new();
    while !done(&*link) && Instant::now() < end {
        let mut out = link.on_readable();
        if link.next_deadline().is_some_and(|d| d <= Instant::now()) {
            out.merge(link.on_deadline(Instant::now()));
        }
        if out != LinkOutput::default() {
            outs.push(out);
        }
        thread::sleep(Duration::from_millis(5));
    }
    outs
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn one_link_serves_the_players_and_the_portal() {
    let bus = PrivateBus::start("media-link-both");
    let portal = FakePortal::start(&bus, 2);
    let tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    // The bar's order at startup: the media module opens the link, the portal joins it.
    link.start_media(&MediaConfig::default());
    link.start_appearance();
    let outs = pump_link(&mut link, Duration::from_secs(5), |l| {
        l.pending() == 0 && l.portal().is_some() && l.media().is_some_and(|m| m.view().is_some())
    });
    assert!(outs.iter().any(|o| o.mode == Some(Mode::Light)), "{outs:?}");
    assert!(outs.iter().any(|o| o.media == Some(shown(true, "Harbor Lights"))), "{outs:?}");
    assert_eq!(portal.calls().len(), 1, "one ReadOne");
    assert_eq!(tidewave.calls(), [GET_ALL, IDENTITY]);
    link.media_do(|m, bus| m.control(bus, Control::PlayPause));
    assert!(tidewave.wait_call("PlayPause", Duration::from_secs(5)).is_some());
    let outs = pump_link(&mut link, Duration::from_secs(5), |l| l.media().is_some_and(|m| m.view() == shown(false, "Harbor Lights")));
    assert_eq!(outs.last().and_then(|o| o.media.clone()), Some(shown(false, "Harbor Lights")), "the pause seen on the link: {outs:?}");
    assert_eq!((link.pending(), link.next_deadline()), (0, None), "nothing pending: no timer at rest");
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_link_for_the_players_alone_never_asks_the_portal() {
    let bus = PrivateBus::start("media-link-alone");
    let portal = FakePortal::start(&bus, 2);
    let _tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    link.start_media(&MediaConfig::default());
    pump_link(&mut link, Duration::from_secs(5), |l| l.pending() == 0 && l.media().is_some_and(|m| m.view().is_some()));
    portal.set(1);
    let outs = pump_link(&mut link, Duration::from_millis(300), |_| false);
    assert!(outs.iter().all(|o| o.mode.is_none()), "no rule for the portal: {outs:?}");
    assert_eq!((portal.calls().len(), link.portal(), link.follows_portal()), (0, None, false));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_dead_bus_closes_the_link_and_hides_the_media() {
    let mut bus = PrivateBus::start("media-link-dies");
    let _tidewave = FakePlayer::start(&bus, Spec::playing("tidewave", "Harbor Lights"));
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    link.start_media(&MediaConfig::default());
    pump_link(&mut link, Duration::from_secs(5), |l| l.pending() == 0 && l.media().is_some_and(|m| m.view().is_some()));
    bus.kill();
    let outs = pump_link(&mut link, Duration::from_secs(5), |_| false);
    let closed = outs.iter().find(|o| o.closed).expect("closed");
    assert_eq!(closed.media, Some(None));
    assert_eq!((link.pending(), link.next_deadline()), (0, None));
}
