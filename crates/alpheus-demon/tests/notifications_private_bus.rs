//! The notifications server on a private session bus (`common::PrivateBus`, nothing activatable),
//! with a fake client and a fake mako: never the workstation's buses, never a real notification.
//! Ignored by default:
//! `nice -n 19 env CARGO_BUILD_JOBS=4 nix develop -c cargo test -p alpheus-demon --test notifications_private_bus -- --ignored`

mod common;

use alpheus_commun::theme::Mode;
use alpheus_demon::config::{MediaConfig, NotificationsConfig};
use alpheus_demon::link::{LinkOutput, SessionLink};
use alpheus_demon::notify::{Center, CloseReason, Effect, NotifyView, Urgency};
use alpheus_demon::tray::bus::{Incoming, SessionBus, refusal};
use alpheus_demon::tray::codec;
use alpheus_demon::tray::notifications::{self, Notifications};
use alpheus_demon::tray::router::{Role, Tray, TrayOutput};
use alpheus_demon::tray::{WATCHER_INTERFACE, WATCHER_NAME, WATCHER_PATH};
use common::PrivateBus;
use common::mpris::{FakePlayer, Spec as PlayerSpec};
use common::notify::{FakeClient, FakeMako, Heard, Hint, INTERFACE, MAKO_FIRST_ID, NAME, Note, PATH};
use common::portal::FakePortal;
use dbus::Message;
use dbus::arg::Variant;
use std::collections::HashSet;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const QUIET: Duration = Duration::from_millis(300);

/// What a pump thread drives: an fd to poll, a deadline to honour, one pass of the bar's loop.
trait Pumped: Send + 'static {
    fn fd(&mut self) -> OwnedFd;
    fn deadline(&self) -> Option<Instant>;
    /// One pass; false once the bus closed (the thread stops then: a dead socket polls readable forever).
    fn pass(&mut self) -> bool;
}

/// `T` on a thread of its own, pumped as the bar's loop pumps it, so the test thread can make
/// blocking calls to it (as `tray_private_bus.rs`'s Pump).
struct Pump<T: Pumped> {
    inner: Arc<Mutex<T>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl<T: Pumped> Pump<T> {
    fn start(mut t: T) -> Pump<T> {
        let fd = t.fd();
        let inner = Arc::new(Mutex::new(t));
        let stop = Arc::new(AtomicBool::new(false));
        let (i, s) = (inner.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                let wait = i.lock().unwrap().deadline().map(|d| d.saturating_duration_since(Instant::now()));
                let ms = wait.map(|w| w.as_millis().min(20) as i32).unwrap_or(20);
                let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: poll(2) on one pollfd this thread owns.
                unsafe { libc::poll(&mut p, 1, ms) };
                if !i.lock().unwrap().pass() {
                    return;
                }
            }
        });
        Pump { inner, stop, thread: Some(thread) }
    }

    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.inner.lock().unwrap())
    }

    /// Waits until `cond` holds (5 s at most), or panics with `what`.
    fn until(&self, what: &str, cond: impl Fn(&mut T) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !self.with(|t| cond(t)) {
            assert!(Instant::now() < end, "never: {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl<T: Pumped> Drop for Pump<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The consumer on its own `SessionBus`, routed as the tray and the bar's link route it: answers
/// decoded by `codec::reply`, signals by `codec::signal`, calls on the Notifications path handled
/// and every other refused, the effects sent after each call; the bubble timer is `pass`'s expiry.
struct Driver {
    bus: SessionBus,
    n: Notifications,
    views: Vec<NotifyView>,
    bubble_changes: usize,
}

impl Driver {
    fn start(bus: &PrivateBus, cfg: &NotificationsConfig, dnd: bool) -> Driver {
        let mut session = SessionBus::at(&bus.address).unwrap();
        let n = Notifications::start(&mut session, cfg, dnd);
        session.flush();
        Driver { bus: session, n, views: Vec::new(), bubble_changes: 0 }
    }

    fn unique(&self) -> String {
        self.bus.unique_name().to_string()
    }

    fn dispatch(&mut self, incoming: Vec<Incoming>) -> bool {
        let mut open = true;
        for i in incoming {
            match i {
                Incoming::Answer { id, call, result } => {
                    let result = result.and_then(|m| codec::reply(&call, &m));
                    if self.n.owns(id) {
                        self.n.answer(id, result);
                    }
                }
                Incoming::Signal(m) => {
                    self.n.signal(&codec::signal(&m));
                }
                Incoming::MethodCall(m) => {
                    // 14:02, as the panel would show it.
                    let reply =
                        if notifications::serves(&m) { self.n.handle(&m, Instant::now(), || 14 * 60 + 2) } else { Some(refusal(&m)) };
                    if let Some(r) = reply {
                        self.bus.send_message(r);
                    }
                    self.send_effects();
                }
                Incoming::Closed => {
                    self.n.lost();
                    open = false;
                }
            }
        }
        open
    }

    fn send_effects(&mut self) {
        for s in self.n.take_effects() {
            self.bus.send_message(s);
        }
    }

    fn route(&mut self, incoming: Vec<Incoming>) -> bool {
        let mut open = self.dispatch(incoming);
        for _ in 0..8 {
            self.bus.flush();
            if !self.bus.has_ready() {
                break;
            }
            let ready = self.bus.take_ready();
            open &= self.dispatch(ready);
        }
        if let Some(v) = self.n.take_change() {
            self.views.push(v);
        }
        if self.n.take_bubbles_change() {
            self.bubble_changes += 1;
        }
        open
    }

    /// The bar acting on the centre (a click, the timer, the panel), its signals sent at once.
    fn act(&mut self, f: impl FnOnce(&mut Center) -> Vec<Effect>) {
        self.n.act(f);
        self.send_effects();
        self.route(Vec::new());
    }

    /// (pending calls, earliest of their deadlines and the bubbles' expiry): (0, None) means no
    /// timer of any kind can be armed.
    fn drained(&self) -> (usize, Option<Instant>) {
        (self.bus.pending(), [self.bus.next_deadline(), self.n.center().next_expiry()].into_iter().flatten().min())
    }

    fn revision(&self, id: u32) -> u32 {
        self.n.center().history().find(|e| e.id == id).map(|e| e.revision).expect("in the history")
    }

    fn live(&self) -> usize {
        self.n.center().history().filter(|e| e.live).count()
    }
}

impl Pumped for Driver {
    fn fd(&mut self) -> OwnedFd {
        self.bus.watch_fd().unwrap()
    }

    fn deadline(&self) -> Option<Instant> {
        self.drained().1
    }

    fn pass(&mut self) -> bool {
        let now = Instant::now();
        let mut incoming = self.bus.read();
        if self.bus.next_deadline().is_some_and(|d| d <= now) {
            incoming.extend(self.bus.expire(now));
        }
        let open = self.route(incoming);
        if open && self.n.center().next_expiry().is_some_and(|d| d <= now) {
            self.act(|c| c.expire(now));
        }
        open
    }
}

/// One-second bubbles (a Notify asking 1000 ms keeps it), so the expiry tests stay short.
fn quick() -> NotificationsConfig {
    NotificationsConfig { timeout: 1, timeout_min: 1, ..NotificationsConfig::default() }
}

/// The consumer pumped, its RequestName answered (held or queued).
fn serve(bus: &PrivateBus, cfg: &NotificationsConfig, dnd: bool) -> Pump<Driver> {
    let pump = Pump::start(Driver::start(bus, cfg, dnd));
    pump.until("the RequestName answered", |d| d.bus.pending() == 0);
    pump
}

/// The consumer holding the name (nobody else asked for it).
fn serving(bus: &PrivateBus, dnd: bool) -> Pump<Driver> {
    let pump = serve(bus, &quick(), dnd);
    pump.until("the name is ours", |d| d.n.held());
    pump
}

/// Spec § 1: while mako holds the name, alpheus waits in the queue, serves nothing and refuses
/// calls made to it directly; a release hands the name over and it serves at once.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn queued_behind_mako_nothing_is_served_until_it_releases_the_name() {
    let bus = PrivateBus::start("notify-queued");
    let mako = FakeMako::start(&bus);
    let server = serve(&bus, &quick(), false);
    let me = server.with(|d| d.unique());
    assert!(!server.with(|d| d.n.held()), "queued behind mako");
    let client = FakeClient::connect(&bus);
    assert_eq!(client.notify(&Note::new("Fictional", "to mako")), Ok(MAKO_FIRST_ID), "mako serves meanwhile");
    assert_eq!(client.capabilities_at(&me).unwrap_err(), UNKNOWN_METHOD, "the queued server answers nothing");
    assert_eq!(client.notify_at(&me, &Note::new("Fictional", "sneaked in")).unwrap_err(), UNKNOWN_METHOD);
    server.with(|d| {
        assert_eq!(d.n.center().history().count(), 0, "nothing ingested while queued");
        assert_eq!(d.views.last().map(|v| v.serving), Some(false), "the bell is dim");
    });
    mako.release();
    server.until("NameAcquired", |d| d.n.held());
    assert_eq!(client.owner(NAME).as_deref(), Some(me.as_str()));
    let id = client.notify(&Note::new("Fictional", "to alpheus")).unwrap();
    assert!(id < MAKO_FIRST_ID, "served by alpheus, not mako: {id}");
    assert_eq!(client.server_information().unwrap(), ("alpheus".into(), "alpheus".into(), env!("CARGO_PKG_VERSION").into(), "1.3".into()));
    let mut caps = client.capabilities().unwrap();
    caps.sort();
    assert_eq!(caps, ["actions", "body", "persistence", "x-canonical-private-synchronous"]);
    assert_eq!(mako.calls(), ["Notify(to mako)"], "nothing more reached mako");
    server.until("the bell lights up", |d| d.views.last().is_some_and(|v| v.serving));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn mako_disconnecting_hands_the_name_over_too() {
    let bus = PrivateBus::start("notify-mako-exits");
    let mako = FakeMako::start(&bus);
    let server = serve(&bus, &quick(), false);
    assert!(!server.with(|d| d.n.held()));
    mako.kill();
    server.until("NameAcquired after mako's connection closed", |d| d.n.held());
    let client = FakeClient::connect(&bus);
    assert!(client.notify(&Note::new("Fictional", "hello")).is_ok_and(|id| id < MAKO_FIRST_ID));
}

/// Review Focus: mako restarting while alpheus holds the name. It queues behind us (flags 0, as we
/// do), nothing changes for the clients, and once alpheus exits the bus hands mako the name: the
/// fallback the spec intends.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_restarted_mako_queues_behind_alpheus_and_nothing_breaks() {
    let bus = PrivateBus::start("notify-mako-restarts");
    let server = serving(&bus, false);
    let client = FakeClient::connect(&bus);
    let first = client.notify(&Note::new("Fictional", "before")).unwrap();
    let mako = FakeMako::queue(&bus);
    let second = client.notify(&Note::new("Fictional", "after")).unwrap();
    assert!(second > first && second < MAKO_FIRST_ID, "still ours: {first} then {second}");
    thread::sleep(QUIET);
    assert!(!mako.holds());
    assert!(mako.calls().is_empty(), "nothing reached the queued mako");
    server.with(|d| {
        assert!(d.n.held(), "no NameLost for a newcomer in the queue");
        let lit = d.views.iter().position(|v| v.serving).expect("served");
        assert!(d.views[lit..].iter().all(|v| v.serving), "the bell never dimmed: {:?}", d.views);
    });
    drop(server);
    assert!(mako.wait_holds(Duration::from_secs(5)), "alpheus gone: the waiting mako takes the name");
    assert_eq!(client.notify(&Note::new("Fictional", "to mako")), Ok(MAKO_FIRST_ID));
}

/// NameAcquired and NameLost count only from the bus driver: a stranger sending copies of them,
/// to us or to everyone, changes nothing (`NameLost` itself, from the driver, is the unit test
/// `name_lost_hides_every_bubble_and_stops_serving`: a private bus cannot make the driver send it
/// to a server that never allowed replacement).
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn forged_name_signals_from_a_stranger_change_nothing() {
    let bus = PrivateBus::start("notify-forged");
    let mako = FakeMako::start(&bus);
    let server = serve(&bus, &quick(), false);
    let me = server.with(|d| d.unique());
    let stranger = FakeClient::connect(&bus);
    stranger.forge("NameAcquired", &me);
    thread::sleep(QUIET);
    assert!(!server.with(|d| d.n.held()), "a stranger's NameAcquired never makes us serve over mako");
    assert_eq!(stranger.capabilities_at(&me).unwrap_err(), UNKNOWN_METHOD);
    mako.release();
    server.until("the real NameAcquired", |d| d.n.held());
    stranger.forge("NameLost", &me);
    thread::sleep(QUIET);
    assert!(server.with(|d| d.n.held()), "nor does its NameLost stop us");
    assert!(stranger.notify(&Note::new("Fictional", "still served")).is_ok_and(|id| id < MAKO_FIRST_ID));
}

/// Spec § 1: a reply goes out for every call, whether or not a bubble shows (do not disturb, a
/// transient one, a malformed call), and a no-reply Notify is still ingested.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn replies_always_arrive_even_when_no_bubble_is_shown() {
    let bus = PrivateBus::start("notify-replies");
    let server = serving(&bus, true);
    let client = FakeClient::connect(&bus);
    let quiet = client.notify(&Note::new("Fictional", "quiet")).unwrap();
    let passing = client.notify(&Note::new("Fictional", "passing").hint("transient", Hint::Bool(true))).unwrap();
    let loud = client.notify(&Note::new("Fictional", "loud").critical()).unwrap();
    assert!(quiet > 0 && passing > quiet && loud > passing, "{quiet} {passing} {loud}");
    server.with(|d| {
        let shown: Vec<u32> = d.n.center().bubbles().iter().map(|s| s.entry.id).collect();
        assert_eq!(shown, [loud], "do not disturb: the critical one only");
        assert!(d.n.center().history().any(|e| e.id == quiet), "the quiet one goes to the history");
    });
    let mut silent = Note::new("Fictional", "no reply wanted").message(NAME);
    silent.set_no_reply(true);
    client.send(silent);
    server.until("the silent one ingested", |d| d.n.center().history().any(|e| e.summary == "no reply wanted"));
    let bad = client.send(Message::new_method_call(NAME, PATH, INTERFACE, "Notify").unwrap().append1("only an app name"));
    assert!(client.replies(&[bad])[0].is_err(), "a malformed Notify: an error, never left hanging");
    assert_eq!(server.with(|d| d.bus.pending()), 0);
}

/// Spec § 1: reasons 1 (expired), 2 (dismissed by the person), 3 (CloseNotification); an unknown id
/// gets the empty reply and no signal.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn close_reasons_are_expired_dismissed_and_closed_by_call() {
    let bus = PrivateBus::start("notify-reasons");
    let server = serving(&bus, false);
    let client = FakeClient::connect(&bus);
    let brief = client.notify(&Note::new("Fictional", "brief").timeout(1000)).unwrap();
    assert!(client.wait_heard(&Heard::Closed(brief, 1)), "expired: reason 1");
    server.with(|d| assert!(d.n.center().history().any(|e| e.id == brief && !e.live), "an expired bubble stays in the history"));
    let dismissed = client.notify(&Note::new("Fictional", "dismiss me").timeout(0)).unwrap(); // 0: timeout_max
    server.with(|d| d.act(|c| c.close(dismissed, CloseReason::Dismissed)));
    assert!(client.wait_heard(&Heard::Closed(dismissed, 2)), "dismissed: reason 2");
    let retracted = client.notify(&Note::new("Fictional", "retract me")).unwrap();
    assert_eq!(client.close(retracted), Ok(0), "an empty reply");
    assert!(client.wait_heard(&Heard::Closed(retracted, 3)), "closed by a call: reason 3");
    assert_eq!(client.close(4_000_000), Ok(0), "an unknown id: empty");
    assert_eq!(client.heard_within(QUIET), Vec::<Heard>::new(), "and no signal");
}

/// Review Focus: CloseNotification on an id that is only in the history (its bubble expired): the
/// empty reply, no second NotificationClosed, no timer, and the server carries on.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn close_notification_on_an_id_only_in_the_history_is_empty_and_silent() {
    let bus = PrivateBus::start("notify-close-history");
    let server = serving(&bus, false);
    let client = FakeClient::connect(&bus);
    let id = client.notify(&Note::new("Fictional", "brief").timeout(1000)).unwrap();
    assert!(client.wait_heard(&Heard::Closed(id, 1)));
    server.with(|d| assert!(d.n.center().history().any(|e| e.id == id && !e.live), "in the history only"));
    assert_eq!(client.close(id), Ok(0));
    assert_eq!(client.heard_within(QUIET), Vec::<Heard>::new(), "closed once already: no second NotificationClosed");
    server.with(|d| assert_eq!(d.drained(), (0, None), "nothing pending, no timer"));
    assert!(client.notify(&Note::new("Fictional", "next")).is_ok_and(|next| next > id), "the server carries on");
}

/// Review Focus: ActionInvoked is a broadcast. A client listening on one connection hears the
/// action of a notification another connection sent; a late click on a replaced bubble (stale
/// revision) sends nothing; a resident notification stays after its action.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn action_invoked_is_broadcast_to_a_listener_on_another_connection() {
    let bus = PrivateBus::start("notify-actions");
    let server = serving(&bus, false);
    let listener = FakeClient::connect(&bus);
    let sender = FakeClient::connect(&bus);
    let chat = |s: &str| Note::new("Fictional Chat", s).action("default", "Open").action("archive", "Archive");
    let id = sender.notify(&chat("Message from Lina")).unwrap();
    let revision = server.with(|d| d.revision(id));
    server.with(|d| d.act(|c| c.invoke(id, revision, "archive")));
    assert!(listener.wait_heard(&Heard::Invoked(id, "archive".into())), "heard on a connection that never called Notify");
    assert!(sender.wait_heard(&Heard::Invoked(id, "archive".into())), "and by the sender");
    assert!(listener.wait_heard(&Heard::Closed(id, 2)), "not resident: the bubble closes, dismissed");
    let resident = |s: &str| chat(s).hint("resident", Hint::Bool(true));
    let kept = sender.notify(&resident("first")).unwrap();
    let old = server.with(|d| d.revision(kept));
    assert_eq!(sender.notify(&resident("second").replaces(kept)), Ok(kept));
    server.with(|d| d.act(|c| c.invoke(kept, old, "default")));
    assert_eq!(listener.heard_within(QUIET), Vec::<Heard>::new(), "a stale revision does nothing");
    let new = server.with(|d| d.revision(kept));
    server.with(|d| d.act(|c| c.invoke(kept, new, "default")));
    assert!(listener.wait_heard(&Heard::Invoked(kept, "default".into())));
    assert_eq!(listener.heard_within(QUIET), Vec::<Heard>::new(), "resident: no NotificationClosed after its action");
}

/// Spec § 1, Ids: replaces_id in place, an unknown one taken as is (Electron's tags) with the counter
/// jumping past it, and the x-canonical-private-synchronous tag.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn replaced_by_id_and_by_tag_in_place() {
    let bus = PrivateBus::start("notify-replace");
    let server = serving(&bus, false);
    let client = FakeClient::connect(&bus);
    let id = client.notify(&Note::new("Fictional", "one")).unwrap();
    let before = server.with(|d| d.revision(id));
    assert_eq!(client.notify(&Note::new("Fictional", "two").replaces(id)), Ok(id));
    server.with(|d| {
        let same: Vec<_> = d.n.center().history().filter(|e| e.id == id).collect();
        assert_eq!(same.len(), 1, "replaced in place, not added");
        assert_eq!(same[0].summary, "two");
        assert!(same[0].revision > before, "a new revision: a late click on the old one does nothing");
    });
    assert_eq!(client.notify(&Note::new("Fictional", "electron").replaces(4242)), Ok(4242), "an unknown id is taken as is");
    let fresh = client.notify(&Note::new("Fictional", "fresh")).unwrap();
    assert!(fresh > 4242, "the counter jumped past it: {fresh}");
    let volume = |s: &str| Note::new("Fictional Mixer", s).hint("x-canonical-private-synchronous", Hint::Text("volume".into()));
    let a = client.notify(&volume("40%")).unwrap();
    let b = client.notify(&volume("45%")).unwrap();
    assert_eq!(a, b, "the same tag replaces");
    server.with(|d| assert_eq!(d.n.center().history().filter(|e| e.summary.ends_with('%')).count(), 1));
}

/// Spec § 2: the portal sends an empty app_name (identity from desktop-entry); hints arrive in
/// other types than the spec's (urgency as u32), and image-data, skipped unread, costs nothing.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_portal_style_call_and_odd_hint_types_are_served() {
    let bus = PrivateBus::start("notify-portal");
    let server = serving(&bus, false);
    let client = FakeClient::connect(&bus);
    let portal =
        client.notify(&Note::new("", "Download finished").hint("desktop-entry", Hint::Text("org.example.Fictional".into()))).unwrap();
    let bare = client.notify(&Note::new("", "No name at all")).unwrap();
    let urgent = client.notify(&Note::new("Fictional", "u32 urgency").hint("urgency", Hint::U32(2))).unwrap();
    let pictured = client.notify(&Note::new("Fictional", "with a picture").hint("image-data", Hint::Image(1 << 20))).unwrap();
    server.with(|d| {
        let c = d.n.center();
        assert_eq!(c.history().find(|e| e.id == portal).map(|e| e.identity.as_str()), Some("org.example.Fictional"));
        assert!(c.history().any(|e| e.id == bare), "an empty app_name and no desktop-entry: still served");
        assert_eq!(c.history().find(|e| e.id == urgent).map(|e| e.urgency), Some(Urgency::Critical));
        assert!(c.history().any(|e| e.id == pictured));
    });
}

/// Spec § 2, Spam: one identity gets 10 bubbles (the bucket), the rest go to the history only;
/// critical bypasses it; an identical one within 1 s merges; a flood is answered in full, the
/// history capped, nothing left pending.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn spam_is_bounded_merged_and_critical_bypasses_the_bucket() {
    let bus = PrivateBus::start("notify-spam");
    // 20-second bubbles: none may expire before `live()` is read, however loaded the machine.
    let server = serve(&bus, &NotificationsConfig { timeout: 20, ..NotificationsConfig::default() }, false);
    server.until("the name is ours", |d| d.n.held());
    let client = FakeClient::connect(&bus);
    let flood: Vec<u32> = (0..15).map(|i| client.send(Note::new("Fictional Flood", &format!("ping {i}")).message(NAME))).collect();
    let ids: Vec<u32> = client.replies(&flood).into_iter().map(|r| r.unwrap().read1::<u32>().unwrap()).collect();
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 15, "every call answered with its own id");
    server.with(|d| assert_eq!(d.live(), 10, "the bucket: 10 bubbles, 5 in the history only"));
    let urgent = client.notify(&Note::new("Fictional Flood", "urgent").critical()).unwrap();
    server.with(|d| assert!(d.n.center().history().any(|e| e.id == urgent && e.live), "critical bypasses the bucket"));
    let a = client.notify(&Note::new("Fictional Echo", "same").body("same body")).unwrap();
    let b = client.notify(&Note::new("Fictional Echo", "same").body("same body")).unwrap();
    assert_eq!(a, b, "an identical one within 1 s is merged");
    server.with(|d| assert_eq!(d.n.center().history().filter(|e| e.identity == "Fictional Echo").count(), 1));
    let many: Vec<u32> = (0..300).map(|i| client.send(Note::new("Fictional Hostile", &format!("n{i}")).message(NAME))).collect();
    assert!(client.replies(&many).iter().all(Result::is_ok), "every call of a flood answered");
    server.with(|d| {
        assert!(d.n.center().history().count() <= 50, "the history is capped");
        assert_eq!(d.bus.pending(), 0);
    });
}

/// Spec § 7: a session bus that dies leaves no bubble, no timer, nothing pending, the bell dim, and
/// nothing to read again.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_bus_dying_leaves_no_bubble_no_timer_and_nothing_spins() {
    let mut bus = PrivateBus::start("notify-bus-dies");
    let server = serving(&bus, false);
    let client = FakeClient::connect(&bus);
    client.notify(&Note::new("Fictional", "sticky").critical()).unwrap();
    client.notify(&Note::new("Fictional", "brief")).unwrap();
    server.with(|d| assert!(d.drained().1.is_some(), "a bubble's expiry is due"));
    drop(client);
    bus.kill();
    server.until("the bus seen gone", |d| !d.n.held());
    server.with(|d| {
        assert_eq!(d.drained(), (0, None), "nothing pending, no expiry: no timer can arm");
        assert!(d.n.center().bubbles().is_empty(), "no bubble");
        assert_eq!(d.views.last().map(|v| v.serving), Some(false), "the bell dims");
        assert!(d.bus.read().is_empty(), "a read afterwards does nothing");
        assert!(d.n.take_effects().is_empty(), "no signal waits for a dead bus");
    });
}

// ---- On the tray's connection ----

/// The tray with the notifications server, pumped; every non-empty output kept.
struct TrayDriver {
    tray: Tray,
    outs: Vec<TrayOutput>,
}

impl Pumped for TrayDriver {
    fn fd(&mut self) -> OwnedFd {
        self.tray.watch_fd().unwrap()
    }

    fn deadline(&self) -> Option<Instant> {
        [self.tray.next_deadline(), self.tray.notifications().and_then(|n| n.center().next_expiry())].into_iter().flatten().min()
    }

    fn pass(&mut self) -> bool {
        let now = Instant::now();
        let mut out = self.tray.on_readable();
        if self.tray.next_deadline().is_some_and(|d| d <= now) {
            out.merge(self.tray.on_deadline(now));
        }
        if self.tray.notifications().and_then(|n| n.center().next_expiry()).is_some_and(|d| d <= now) {
            self.tray.notifications_do(|n| n.act(|c| c.expire(now)));
        }
        if self.tray.has_deferred() {
            out.merge(self.tray.take_deferred());
        }
        let closed = out.bus_closed;
        if out != TrayOutput::default() {
            self.outs.push(out);
        }
        !closed
    }
}

fn tray_with_notifications(bus: &PrivateBus) -> Pump<TrayDriver> {
    let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), PathBuf::from("/proc"));
    tray.start_notifications(&quick(), false);
    tray.start_notifications(&quick(), false); // a second call is a no-op
    Pump::start(TrayDriver { tray, outs: Vec::new() })
}

fn watcher_property(client: &FakeClient, name: &str) -> Message {
    let m = Message::new_method_call(WATCHER_NAME, WATCHER_PATH, "org.freedesktop.DBus.Properties", "Get")
        .unwrap()
        .append2(WATCHER_INTERFACE, name);
    client.peer.call(m).unwrap()
}

/// The watcher keeps serving while the notifications server waits behind mako; the Notifications
/// path on the same connection is refused (UnknownMethod) until the name is handed over.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_trays_watcher_still_serves_while_notifications_are_queued() {
    let bus = PrivateBus::start("notify-tray-queued");
    let mako = FakeMako::start(&bus);
    let pump = tray_with_notifications(&bus);
    pump.until("the watcher's name ours, every startup call answered", |t| {
        *t.tray.role() == Role::Watcher && t.tray.next_deadline().is_none()
    });
    let client = FakeClient::connect(&bus);
    let me = client.owner(WATCHER_NAME).unwrap();
    assert!(watcher_property(&client, "IsStatusNotifierHostRegistered").read1::<Variant<bool>>().unwrap().0, "the watcher serves");
    assert_eq!(client.capabilities_at(&me).unwrap_err(), UNKNOWN_METHOD, "the Notifications path is refused while queued");
    assert_eq!(client.notify(&Note::new("Fictional", "to mako")), Ok(MAKO_FIRST_ID));
    pump.until("the bell hears: not serving", |t| t.outs.iter().any(|o| o.notifications.is_some_and(|v| !v.serving)));
    mako.release();
    pump.until("serving", |t| t.tray.notifications().is_some_and(|n| n.held()));
    assert!(client.notify(&Note::new("Fictional", "to alpheus")).is_ok_and(|id| id < MAKO_FIRST_ID));
    pump.until("the bell hears: serving, and a bubble", |t| {
        t.outs.iter().any(|o| o.notifications.is_some_and(|v| v.serving)) && t.outs.iter().any(|o| o.bubbles)
    });
    assert_eq!(watcher_property(&client, "ProtocolVersion").read1::<Variant<i32>>().unwrap().0, 0, "the watcher still serves");
}

/// Review Focus: the watcher and the notifications server on one connection, calls interleaved
/// in one burst from one client; each gets its own correct reply, in any order.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn the_watcher_and_notifications_interleave_on_one_connection() {
    let bus = PrivateBus::start("notify-tray-interleaved");
    let pump = tray_with_notifications(&bus);
    pump.until("both names ours", |t| *t.tray.role() == Role::Watcher && t.tray.notifications().is_some_and(|n| n.held()));
    let client = FakeClient::connect(&bus);
    let get = |p: &str| {
        Message::new_method_call(WATCHER_NAME, WATCHER_PATH, "org.freedesktop.DBus.Properties", "Get")
            .unwrap()
            .append2(WATCHER_INTERFACE, p)
    };
    let plain = |member: &str| Message::new_method_call(NAME, PATH, INTERFACE, member).unwrap();
    let introspect = Message::new_method_call(WATCHER_NAME, WATCHER_PATH, "org.freedesktop.DBus.Introspectable", "Introspect").unwrap();
    let serials = [
        client.send(Note::new("Fictional", "first").message(NAME)),
        client.send(get("IsStatusNotifierHostRegistered")),
        client.send(plain("GetCapabilities")),
        client.send(introspect),
        client.send(plain("CloseNotification").append1(4_000_000u32)),
        client.send(get("ProtocolVersion")),
        client.send(plain("GetServerInformation")),
        client.send(Note::new("Fictional", "second").message(NAME)),
    ];
    let r = client.replies(&serials);
    let ok = |i: usize| r[i].as_ref().unwrap_or_else(|e| panic!("call {i}: {e}"));
    let first = ok(0).read1::<u32>().unwrap();
    assert!(ok(1).read1::<Variant<bool>>().unwrap().0);
    assert!(ok(2).read1::<Vec<String>>().unwrap().contains(&"body".to_string()));
    assert!(ok(3).read1::<&str>().unwrap().contains("RegisterStatusNotifierItem"));
    assert_eq!(ok(4).iter_init().count(), 0, "CloseNotification: empty");
    assert_eq!(ok(5).read1::<Variant<i32>>().unwrap().0, 0);
    assert_eq!(ok(6).read1::<&str>().unwrap(), "alpheus");
    assert_eq!(ok(7).read1::<u32>().unwrap(), first + 1);
    let wrong = client.send(Message::new_method_call(NAME, WATCHER_PATH, INTERFACE, "GetCapabilities").unwrap());
    assert!(client.replies(&[wrong])[0].is_err(), "the Notifications interface lives on its own path only");
    pump.with(|t| assert_eq!(t.tray.notifications().map(|n| n.center().history().count()), Some(2)));
}

/// CloseNotification served by the tray: its NotificationClosed (reason 3) leaves right after the
/// reply, not at some later batch.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn close_notification_through_the_tray_is_heard_at_once() {
    let bus = PrivateBus::start("notify-tray-close");
    let pump = tray_with_notifications(&bus);
    pump.until("serving", |t| t.tray.notifications().is_some_and(|n| n.held()));
    let client = FakeClient::connect(&bus);
    let id = client.notify(&Note::new("Fictional", "retract me").timeout(0)).unwrap();
    assert_eq!(client.close(id), Ok(0), "an empty reply");
    assert!(client.wait_heard(&Heard::Closed(id, 3)), "closed by a call: reason 3, sent by the tray");
}

/// The bus dying under the tray: the output that says so also dims the bell and takes the bubbles
/// down; no deadline, no expiry left.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_dead_bus_takes_the_notifications_away_with_the_tray() {
    let mut bus = PrivateBus::start("notify-tray-dies");
    let pump = tray_with_notifications(&bus);
    pump.until("serving", |t| t.tray.notifications().is_some_and(|n| n.held()));
    let client = FakeClient::connect(&bus);
    client.notify(&Note::new("Fictional", "sticky").critical()).unwrap();
    client.notify(&Note::new("Fictional", "brief")).unwrap();
    pump.until("two bubbles", |t| t.tray.notifications().is_some_and(|n| n.center().bubbles().len() == 2));
    drop(client);
    bus.kill();
    pump.until("bus_closed", |t| t.outs.iter().any(|o| o.bus_closed));
    pump.with(|t| {
        let closed = t.outs.iter().find(|o| o.bus_closed).unwrap();
        assert_eq!(closed.notifications.map(|v| v.serving), Some(false), "the bell dims with the bus");
        assert!(closed.bubbles, "the bubbles go");
        assert_eq!(t.tray.next_deadline(), None);
        assert_eq!(t.tray.notifications().and_then(|n| n.center().next_expiry()), None);
    });
}

/// Task 4's review: a bubble expiring on the tray's connection (the pump is its timer) is broadcast
/// as NotificationClosed(id, 1): `Tray::notifications_do` sends what `act` made.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_bubble_expiring_on_the_tray_is_heard_with_reason_1() {
    let bus = PrivateBus::start("notify-tray-expire");
    let pump = tray_with_notifications(&bus);
    pump.until("serving", |t| t.tray.notifications().is_some_and(|n| n.held()));
    let client = FakeClient::connect(&bus);
    let id = client.notify(&Note::new("Fictional", "brief")).unwrap();
    assert!(client.wait_heard(&Heard::Closed(id, 1)), "expired after 1 s: reason 1, sent by the tray");
    pump.with(|t| assert_eq!(t.tray.notifications().and_then(|n| n.center().next_expiry()), None, "no timer left"));
}

/// Every RequestName on `bus` from now on, as (sender, the name asked for): a monitor connection
/// (the private bus allows eavesdropping), read for `within`.
struct NameRequests {
    peer: common::Peer,
}

impl NameRequests {
    fn watch(bus: &PrivateBus) -> NameRequests {
        let peer = common::Peer::connect(bus);
        let rule = "type='method_call',interface='org.freedesktop.DBus',member='RequestName'";
        let m =
            Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus.Monitoring", "BecomeMonitor")
                .unwrap()
                .append2(vec![rule], 0u32);
        peer.call(m).unwrap();
        NameRequests { peer }
    }

    fn seen(&self, within: Duration) -> Vec<(String, String)> {
        let end = Instant::now() + within;
        let mut seen = Vec::new();
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match self.peer.channel.blocking_pop_message(left) {
                Ok(Some(m)) if m.member().as_deref() == Some("RequestName") => {
                    seen.push((m.sender().map(|s| s.to_string()).unwrap_or_default(), m.read1::<&str>().unwrap_or("").to_string()));
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        seen
    }
}

/// Task 4's review: `start_notifications` asks for the name exactly once, even called twice, on
/// the tray's connection and on the bar's link alike.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn start_notifications_requests_the_name_once_even_called_twice() {
    let bus = PrivateBus::start("notify-one-request");
    let requests = NameRequests::watch(&bus);
    let mut tray = Tray::start(SessionBus::at(&bus.address).unwrap(), PathBuf::from("/proc"));
    tray.start_notifications(&quick(), false);
    tray.start_notifications(&quick(), false);
    let link_bus = SessionBus::at(&bus.address).unwrap();
    let link_name = link_bus.unique_name().to_string();
    let mut link = SessionLink::new(link_bus);
    link.start_notifications(&quick(), false);
    link.start_notifications(&quick(), false);
    let seen = requests.seen(QUIET);
    let asked = |name: &str| seen.iter().filter(|(_, n)| n == name).map(|(s, _)| s.clone()).collect::<Vec<_>>();
    let notifications = asked(NAME);
    assert_eq!(notifications.len(), 2, "one per connection: {seen:?}");
    assert!(notifications.contains(&link_name), "the link's own: {seen:?}");
    assert_eq!(asked(WATCHER_NAME).len(), 1, "the watcher's, for comparison: {seen:?}");
    assert!(notifications.iter().all(|s| !s.is_empty()) && notifications[0] != notifications[1], "{seen:?}");
}

// ---- On the bar's own link (task 5) ----

struct LinkDriver {
    link: SessionLink,
    outs: Vec<LinkOutput>,
}

impl Pumped for LinkDriver {
    fn fd(&mut self) -> OwnedFd {
        self.link.watch_fd().unwrap()
    }

    fn deadline(&self) -> Option<Instant> {
        [self.link.next_deadline(), self.link.notifications().and_then(|n| n.center().next_expiry())].into_iter().flatten().min()
    }

    fn pass(&mut self) -> bool {
        let now = Instant::now();
        let mut out = self.link.on_readable();
        if self.link.next_deadline().is_some_and(|d| d <= now) {
            out.merge(self.link.on_deadline(now));
        }
        if self.link.notifications().and_then(|n| n.center().next_expiry()).is_some_and(|d| d <= now) {
            self.link.notifications_do(|n| n.act(|c| c.expire(now)));
        }
        // The App's idle `link_readable`, here.
        if self.link.has_ready() {
            out.merge(self.link.on_readable());
        }
        let closed = out.closed;
        if out != LinkOutput::default() {
            self.outs.push(out);
        }
        !closed
    }
}

/// Spec § 1: with the tray off, ONE link carries the media module, the portal's follower and the
/// notifications server; a change the bar makes reaches it through `has_ready`, never re-entrantly.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn one_link_serves_the_players_the_portal_and_the_notifications() {
    let bus = PrivateBus::start("notify-link-all");
    let _portal = FakePortal::start(&bus, 2);
    let _tidewave = FakePlayer::start(&bus, PlayerSpec::playing("tidewave", "Harbor Lights"));
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    // The bar's order at startup: media, notifications, then the portal joins.
    link.start_media(&MediaConfig::default());
    link.start_notifications(&quick(), false);
    link.start_notifications(&quick(), false); // a second call is a no-op
    link.start_appearance();
    let pump = Pump::start(LinkDriver { link, outs: Vec::new() });
    pump.until("all three ready", |l| {
        l.link.pending() == 0
            && l.link.portal().is_some()
            && l.link.media().is_some_and(|m| m.view().is_some())
            && l.link.notifications().is_some_and(|n| n.held())
    });
    let client = FakeClient::connect(&bus);
    let id = client.notify(&Note::new("Fictional", "on the link")).unwrap();
    assert_eq!(client.close(id), Ok(0));
    assert!(client.wait_heard(&Heard::Closed(id, 3)), "the effect leaves on the link");
    pump.until("the bell and the bubbles heard", |l| {
        l.outs.iter().any(|o| o.notifications.is_some_and(|v| v.serving)) && l.outs.iter().any(|o| o.bubbles)
    });
    pump.with(|l| {
        assert!(l.outs.iter().any(|o| o.mode == Some(Mode::Light)), "the portal followed on the same link");
        l.link.notifications_do(|n| {
            n.act(|c| {
                c.set_dnd(true);
                Vec::new()
            })
        });
        assert!(l.link.has_ready(), "the change waits for an idle read");
        let out = l.link.on_readable();
        assert_eq!(out.notifications.map(|v| v.dnd), Some(true));
        assert!(!l.link.has_ready() || l.link.pending() > 0, "delivered once: the deferred output was taken");
    });
}

/// Without the module: no RequestName on the link, the name stays free, nothing is served.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_link_without_the_module_never_asks_for_the_name() {
    let bus = PrivateBus::start("notify-link-none");
    let _tidewave = FakePlayer::start(&bus, PlayerSpec::playing("tidewave", "Harbor Lights"));
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    link.start_media(&MediaConfig::default());
    let pump = Pump::start(LinkDriver { link, outs: Vec::new() });
    pump.until("the player read", |l| l.link.pending() == 0 && l.link.media().is_some_and(|m| m.view().is_some()));
    let client = FakeClient::connect(&bus);
    assert_eq!(client.owner(NAME), None, "never requested");
    assert_eq!(client.notify(&Note::new("Fictional", "anyone?")).unwrap_err(), "org.freedesktop.DBus.Error.ServiceUnknown");
    pump.with(|l| assert!(l.outs.iter().all(|o| o.notifications.is_none() && !o.bubbles)));
}

#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_dead_bus_closes_the_link_and_dims_the_bell() {
    let mut bus = PrivateBus::start("notify-link-dies");
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    link.start_notifications(&quick(), false);
    let pump = Pump::start(LinkDriver { link, outs: Vec::new() });
    pump.until("serving", |l| l.link.notifications().is_some_and(|n| n.held()));
    let client = FakeClient::connect(&bus);
    client.notify(&Note::new("Fictional", "brief")).unwrap();
    pump.until("a bubble", |l| l.link.notifications().is_some_and(|n| !n.center().bubbles().is_empty()));
    drop(client);
    bus.kill();
    pump.until("closed", |l| l.outs.iter().any(|o| o.closed));
    pump.with(|l| {
        let closed = l.outs.iter().find(|o| o.closed).unwrap();
        assert_eq!(closed.notifications.map(|v| v.serving), Some(false));
        assert!(closed.bubbles);
        assert_eq!((l.link.pending(), l.link.next_deadline()), (0, None));
        assert_eq!(l.link.notifications().and_then(|n| n.center().next_expiry()), None);
    });
}

/// Spec § 1 on the link: queued behind mako (reply 2), the link serves nothing; mako leaving, the
/// bus driver's NameAcquired reaches the server through the link's signal routing.
#[test]
#[ignore = "private dbus-daemon (nix develop)"]
fn a_link_queued_behind_mako_serves_once_the_name_is_handed_over() {
    let bus = PrivateBus::start("notify-link-mako");
    let mako = FakeMako::start(&bus);
    let mut link = SessionLink::new(SessionBus::at(&bus.address).unwrap());
    link.start_notifications(&quick(), false);
    let pump = Pump::start(LinkDriver { link, outs: Vec::new() });
    pump.until("queued, the bell dim", |l| l.link.pending() == 0 && l.outs.iter().any(|o| o.notifications.is_some_and(|v| !v.serving)));
    let client = FakeClient::connect(&bus);
    assert_eq!(client.notify(&Note::new("Fictional", "to mako")), Ok(MAKO_FIRST_ID));
    pump.with(|l| assert!(!l.link.notifications().is_some_and(|n| n.held())));
    mako.release();
    pump.until("serving", |l| l.link.notifications().is_some_and(|n| n.held()));
    assert!(client.notify(&Note::new("Fictional", "to alpheus")).is_ok_and(|id| id < MAKO_FIRST_ID));
    pump.until("the bell hears: serving", |l| l.outs.iter().any(|o| o.notifications.is_some_and(|v| v.serving)));
}
