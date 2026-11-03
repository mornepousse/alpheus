//! The displays' wiring (displays spec § 1): the output manager bound by `outputs::wlr`, its
//! events turned into the controller's, the controller's actions run, and one `DeadlineTimer`
//! following `Controller::next_deadline` (the debounce, the watchdog or the trial; none at rest).
//! DisplayLink heads are told from `[displays] drm_dir`, read once per new head name, never
//! watched, nothing opened under /dev. Logs only the spec's fixed lines.

use crate::config::{DisplaysConfig, ModuleId};
use crate::deadline::DeadlineTimer;
use crate::launcher::host::guarded;
use crate::launcher::socket::{Conn, Pending};
use crate::launcher::socket_glue::reply_guarded;
use crate::log::write as log;
use crate::outputs::control::{Action, Controller, Event, ReplyTo};
use crate::outputs::model::{Head, Target, to_output};
use crate::outputs::wlr::Wlr;
use crate::wayland::App;
use alpheus_commun::displays::profiles::{self, Profile};
use alpheus_commun::displays::wire::{Snapshot, Status, decode_desired, encode_snapshot};
use alpheus_commun::launcher_ipc::{Reply, Request};
use alpheus_commun::paths;
use alpheus_commun::theme::settings::MAX_FILE;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use smithay_client_toolkit::reexports::client::QueueHandle;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use std::cell::Cell;
use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Entries of `drm_dir` looked at, at most (a machine has a handful).
const MAX_DRM_ENTRIES: usize = 512;
/// The line for an invalid or unreadable displays.toml: never its error, which may quote a
/// screen's identity.
const LOG_INVALID_FILE: &str = "displays: displays.toml invalid";
/// Characters of a profile name in its log line.
const MAX_LOGGED_NAME: usize = 64;
/// The verbs' answer without the module (displays spec, the TUI's errors).
pub(crate) const OFF: &str = "displays are off in bar.toml";
/// The verbs' answer when the compositor has no output manager, or stopped it.
pub(crate) const NO_MANAGER: &str = "the compositor has no output management";

/// The output manager, its controller and their timer; on the app only with `displays` in
/// `modules` and a compositor that has the manager.
pub(crate) struct OutputsApp {
    controller: Controller,
    pub(crate) wlr: Wlr,
    /// The controller's next deadline, armed only while there is one.
    timer: DeadlineTimer,
    drm_dir: PathBuf,
    /// displays.toml, read only when the debounce ends with `auto` on.
    profiles_path: PathBuf,
    links: DisplayLinks,
    invalid: InvalidFile,
    /// The TUI's try waiting for the compositor's answer.
    client: TryClient,
}

impl OutputsApp {
    /// Binds the manager; None when the compositor has none.
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<App>, config: &DisplaysConfig) -> Option<OutputsApp> {
        Some(OutputsApp {
            controller: Controller::new(config.auto),
            wlr: Wlr::bind(globals, qh)?,
            timer: DeadlineTimer::new(),
            drm_dir: config.drm_dir.clone(),
            profiles_path: paths::config_dir().join("displays.toml"),
            links: DisplayLinks::default(),
            invalid: InvalidFile::default(),
            client: TryClient::default(),
        })
    }

    /// Runs the controller's actions; a configuration that cannot be built is answered as
    /// invalid, at once.
    fn run(&mut self, actions: Vec<Action>, qh: &QueueHandle<App>, now: Instant) {
        let OutputsApp { controller, wlr, client, .. } = self;
        if let Some(answer) = execute(controller, actions, now, &mut |g, t| wlr.send(g, t, qh).is_ok()) {
            client.answer(try_reply(answer));
        }
        // A configuration the controller gave up on (the watchdog) is destroyed.
        self.wlr.abandon(self.controller.in_flight());
    }

    /// One displays verb: its reply now, or None for a try whose client is kept until the
    /// compositor answers.
    fn verb(&mut self, request: &Request, qh: &QueueHandle<App>) -> Option<Reply> {
        let now = Instant::now();
        let OutputsApp { controller, wlr, .. } = self;
        let reply = verb(controller, request, now, &mut |g, t| wlr.send(g, t, qh).is_ok());
        self.wlr.abandon(self.controller.in_flight());
        reply
    }
}

/// A displays verb behind the error boundary of the other verbs: a panic (a hostile body, a
/// bug) answers `internal error` and never takes the bar down.
fn verb_guarded(f: impl FnOnce() -> Option<Reply>) -> Option<Reply> {
    guarded("displays verb", f).unwrap_or_else(|| Some(Reply::Error("internal error".into())))
}

/// Runs the controller's actions; `send` builds and sends one configuration (false: it could
/// not be built, answered as invalid at once). Returns the try's answer when one came.
pub(crate) fn execute(
    controller: &mut Controller,
    actions: Vec<Action>,
    now: Instant,
    send: &mut dyn FnMut(u64, &[Target]) -> bool,
) -> Option<Result<(), &'static str>> {
    let mut answer = None;
    let mut queue: VecDeque<Action> = actions.into();
    while let Some(action) = queue.pop_front() {
        match action {
            Action::Send(generation, targets) => {
                if !send(generation, &targets) {
                    queue.extend(controller.on_event(Event::Invalid(generation), now));
                }
            }
            Action::Log(line) => log(line),
            Action::LogProfile(name) => log(&profile_line(&name)),
            Action::Reply(ReplyTo::Try, a) => answer = Some(a),
        }
    }
    answer
}

/// A try's answer on the socket.
fn try_reply(answer: Result<(), &'static str>) -> Reply {
    match answer {
        Ok(()) => Reply::Done,
        Err(e) => Reply::Error(e.into()),
    }
}

/// The snapshot the `outputs` verb sends: the heads of the last `done` and the trial (an end
/// is reported once).
pub(crate) fn snapshot(controller: &mut Controller, now: Instant) -> Snapshot {
    Snapshot { status: Status::Ready, outputs: controller.heads().iter().map(to_output).collect(), trial: controller.trial_state(now) }
}

/// A displays verb against the controller: its reply, or None for a try whose answer comes
/// from the compositor (at most the watchdog later). `send` as in `execute`.
pub(crate) fn verb(
    controller: &mut Controller,
    request: &Request,
    now: Instant,
    send: &mut dyn FnMut(u64, &[Target]) -> bool,
) -> Option<Reply> {
    let actions = match request {
        Request::Outputs => return Some(Reply::Outputs(encode_snapshot(&snapshot(controller, now)))),
        Request::OutputsTry(body) => {
            let steps = match decode_desired(body) {
                Ok(steps) => steps,
                Err(e) => return Some(Reply::Error(format!("malformed request: {e}"))),
            };
            match controller.try_(&steps, now) {
                Ok(actions) => return execute(controller, actions, now, send).map(try_reply),
                Err(e) => return Some(Reply::Error(e.into())),
            }
        }
        Request::OutputsKeep => controller.keep(now),
        Request::OutputsRevert => controller.revert(now),
        _ => return Some(Reply::Error("not a displays verb".into())),
    };
    // Keep and revert answer no try; what they send is answered by the compositor later.
    execute(controller, actions, now, send);
    Some(Reply::Done)
}

/// The answer to a displays verb when there is no controller: the module is off, or the
/// compositor has no output manager.
pub(crate) fn unavailable(module_on: bool) -> Reply {
    Reply::Error(if module_on { NO_MANAGER } else { OFF }.into())
}

/// The client of the try in flight, and the watch on its hang-up (edge-triggered, as the
/// dmenu client's: a client that half-closed would stay readable). At most one.
#[derive(Default)]
pub(crate) struct TryClient {
    pending: Option<Pending>,
    watch: Option<(LoopHandle<'static, App>, RegistrationToken)>,
}

impl TryClient {
    #[cfg(test)]
    fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// Keeps `pending` until the answer; watched for a hang-up when there is a loop. A watch
    /// that cannot be made is not logged (only the spec's fixed lines are): a client gone is then
    /// noticed at its answer, the watchdog's at the latest.
    pub(crate) fn wait(&mut self, pending: Pending, handle: Option<&LoopHandle<'static, App>>) {
        // One try at a time: the controller refuses a second, and this one is told so
        // rather than dropped without a word.
        self.answer(Reply::Error("another change is in progress".into()));
        self.unwatch();
        if let Some(handle) = handle
            && let Ok(fd) = pending.fd().try_clone_to_owned()
        {
            let source = Generic::new(fd, Interest::READ, Mode::Edge);
            let watch = handle.insert_source(source, |_, _, app: &mut App| {
                Ok(if app.displays_client_check() { PostAction::Remove } else { PostAction::Continue })
            });
            self.watch = watch.ok().map(|token| (handle.clone(), token));
        }
        self.pending = Some(pending);
    }

    /// The answer to the waiting client, if one still waits; true when it was sent.
    pub(crate) fn answer(&mut self, reply: Reply) -> bool {
        self.unwatch();
        match self.pending.take() {
            Some(p) => {
                p.reply(&reply);
                true
            }
            None => false,
        }
    }

    /// The client hung up: it is dropped (its answer, when it comes, goes nowhere). True when
    /// nobody waits any more.
    pub(crate) fn check(&mut self) -> bool {
        if self.pending.as_ref().is_some_and(Pending::hung_up) {
            self.pending = None;
        }
        if self.pending.is_none() {
            // The watch's own source returns Remove: forget its token, never remove it twice.
            self.watch = None;
            return true;
        }
        false
    }

    fn unwatch(&mut self) {
        if let Some((handle, token)) = self.watch.take() {
            handle.remove(token);
        }
    }
}

/// `displays: profile "<name>" does not resolve`, the name (the user's own words) on one line.
fn profile_line(name: &str) -> String {
    let name: String = name.chars().take(MAX_LOGGED_NAME).map(|c| if c.is_control() { ' ' } else { c }).collect();
    format!("displays: profile \"{name}\" does not resolve")
}

impl App {
    /// A manager `done`: the heads, their DisplayLink flag set, to the controller.
    pub(crate) fn displays_done(&mut self, mut heads: Vec<Head>) {
        let Some(d) = self.displays.as_mut() else { return };
        let (links, drm_dir) = (&mut d.links, &d.drm_dir);
        links.mark(&mut heads, |name| is_displaylink(drm_dir, name));
        self.displays_event(Event::Done(heads));
    }

    /// One event to the controller, its actions run, the timer following.
    pub(crate) fn displays_event(&mut self, e: Event) {
        let now = Instant::now();
        let Some(d) = self.displays.as_mut() else { return };
        let actions = d.controller.on_event(e, now);
        d.run(actions, &self.qh, now);
        self.displays_sync();
    }

    /// The controller's deadline passed (the timer's source is dropped).
    fn displays_tick(app: &mut App, now: Instant) {
        let Some(d) = app.displays.as_mut() else { return };
        d.timer.fired();
        let path = d.profiles_path.clone();
        let read = Cell::new(None);
        let actions = d.controller.tick(
            &|| {
                let (profiles, invalid) = read_profiles(&path);
                read.set(Some(invalid));
                profiles
            },
            now,
        );
        if let Some(invalid) = read.get()
            && d.invalid.note(invalid)
        {
            log(LOG_INVALID_FILE);
        }
        d.run(actions, &app.qh, now);
        app.displays_sync();
    }

    /// The timer on the controller's next deadline; none at rest.
    pub(crate) fn displays_sync(&mut self) {
        if let Some(d) = self.displays.as_mut() {
            let next = d.controller.next_deadline();
            d.timer.sync_opt(self.loop_handle.as_ref(), next, App::displays_tick);
        }
    }

    /// A displays verb from the launcher socket (`launcher/socket_glue.rs`).
    pub(crate) fn displays_verb(&mut self, request: &Request, conn: Conn) {
        let module_on = self.bar.config.modules.contains(&ModuleId::Displays);
        let (qh, handle) = (self.qh.clone(), self.loop_handle.clone());
        let reply = match self.displays.as_mut() {
            Some(d) => verb_guarded(|| d.verb(request, &qh)),
            None => Some(unavailable(module_on)),
        };
        match (reply, self.displays.as_mut()) {
            (Some(reply), _) => reply_guarded(conn, &reply),
            (None, Some(d)) => d.client.wait(conn.into_pending(), handle.as_ref()),
            (None, None) => reply_guarded(conn, &unavailable(module_on)),
        }
        self.displays_sync();
    }

    /// The try's client watch fired: true once nobody waits (the watch is removed).
    fn displays_client_check(&mut self) -> bool {
        self.displays.as_mut().is_none_or(|d| d.client.check())
    }

    /// The compositor stopped the manager: the timer goes, then everything else.
    pub(crate) fn displays_gone(&mut self) {
        if let Some(mut d) = self.displays.take() {
            d.client.answer(Reply::Error(NO_MANAGER.into()));
            d.wlr.abandon(None);
            d.timer.sync_opt(self.loop_handle.as_ref(), None, App::displays_tick);
        }
    }
}

/// displays.toml's profiles, and a fingerprint of the file when it is invalid or unreadable
/// (None when absent or valid). At most `MAX_FILE` bytes of a regular file are read; the open
/// never blocks (a FIFO is opened non-blocking, then refused).
pub(crate) fn read_profiles(path: &Path) -> (Vec<Profile>, Option<u64>) {
    match read_regular(path) {
        Ok(text) => match profiles::parse(&text) {
            Ok(p) => (p, None),
            Err(_) => {
                let mut h = DefaultHasher::new();
                text.hash(&mut h);
                (Vec::new(), Some(h.finish()))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(_) => (Vec::new(), Some(0)),
    }
}

fn read_regular(path: &Path) -> std::io::Result<String> {
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut text = String::new();
    file.take(MAX_FILE).read_to_string(&mut text)?;
    Ok(text)
}

/// The fingerprint of the invalid displays.toml last logged: one line per change of the file.
#[derive(Debug, Default)]
pub(crate) struct InvalidFile {
    last: Option<u64>,
}

impl InvalidFile {
    /// Notes the file's state (`read_profiles`' fingerprint); true when its line is due.
    pub(crate) fn note(&mut self, invalid: Option<u64>) -> bool {
        let due = invalid.is_some() && invalid != self.last;
        self.last = invalid;
        due
    }
}

/// Whether the connector `name` is on a DRM card driven by `evdi` (DisplayLink): the
/// `card<N>-<name>` entry of `drm_dir` whose card's driver link names `evdi`. On sysfs the
/// connector's `device` is its card, whose `device/driver` is the driver; a `device/driver`
/// right under the connector is accepted too. Links are read, never followed into files.
pub(crate) fn is_displaylink(drm_dir: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(drm_dir) else { return false };
    entries.flatten().take(MAX_DRM_ENTRIES).any(|e| {
        e.file_name().to_str().and_then(connector_of) == Some(name)
            && ["device/device/driver", "device/driver"].iter().any(|link| {
                std::fs::read_link(e.path().join(link))
                    .is_ok_and(|target| target.file_name().is_some_and(|f| f.to_string_lossy().ends_with("evdi")))
            })
    })
}

/// The connector of a `card<N>-<connector>` entry.
fn connector_of(entry: &str) -> Option<&str> {
    let rest = entry.strip_prefix("card")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    rest[digits..].strip_prefix('-').filter(|c| !c.is_empty())
}

/// The DisplayLink flag of each head, read once per new head name (`is_displaylink` is
/// injected for the tests).
#[derive(Debug, Default)]
pub(crate) struct DisplayLinks {
    /// The heads of the last `done`, with their flag.
    known: Vec<(String, bool)>,
}

impl DisplayLinks {
    /// Sets `displaylink` on every head; only a name not in the last `done` is read.
    pub(crate) fn mark(&mut self, heads: &mut [Head], mut read: impl FnMut(&str) -> bool) {
        let mut known = Vec::with_capacity(heads.len());
        for h in heads.iter_mut() {
            h.displaylink = match self.known.iter().find(|(n, _)| *n == h.name) {
                Some(&(_, flag)) => flag,
                None => read(&h.name),
            };
            known.push((h.name.clone(), h.displaylink));
        }
        self.known = known;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpheus_commun::temp_path::TempPath;
    use std::os::unix::fs::symlink;

    /// A sysfs-shaped tree: the connector's `device` links to its card, whose `device/driver`
    /// names the driver.
    fn card(drm: &Path, card: &str, driver: &str, connectors: &[&str]) {
        std::fs::create_dir_all(drm.join(card).join("device")).unwrap();
        symlink(format!("../../../bus/pci/drivers/{driver}"), drm.join(card).join("device/driver")).unwrap();
        for c in connectors {
            std::fs::create_dir_all(drm.join(format!("{card}-{c}"))).unwrap();
            symlink(format!("../{card}"), drm.join(format!("{card}-{c}/device"))).unwrap();
        }
    }

    #[test]
    fn displaylink_detection_reads_the_driver_link() {
        let drm = TempPath::dir("drm-displaylink");
        card(&drm, "card0", "i915", &["eDP-1", "DVI-I-10", "HDMI-A-1"]);
        card(&drm, "card2", "evdi", &["DVI-I-1", "HDMI-A-11"]);
        // The connector's own `device/driver` (the brief's shape).
        std::fs::create_dir_all(drm.join("card3-DP-5/device")).unwrap();
        symlink("../../../../bus/platform/drivers/evdi", drm.join("card3-DP-5/device/driver")).unwrap();
        assert!(is_displaylink(&drm, "DVI-I-1"), "a connector of the evdi card");
        assert!(is_displaylink(&drm, "DP-5"), "a driver link right under the connector");
        assert!(!is_displaylink(&drm, "eDP-1"), "i915");
        assert!(!is_displaylink(&drm, "DVI-I-10"), "the whole name, not a prefix of another");
        assert!(!is_displaylink(&drm, "HDMI-A-1"), "not the evdi card's HDMI-A-11");
        assert!(!is_displaylink(&drm, "HEADLESS-1"), "no connector");
        assert!(!is_displaylink(&drm, "../card2"), "never a path");
        assert!(!is_displaylink(&drm.join("missing"), "DVI-I-1"), "no directory: not DisplayLink");
    }

    fn head(name: &str) -> Head {
        Head {
            name: name.into(),
            description: String::new(),
            make: String::new(),
            model: String::new(),
            serial: String::new(),
            enabled: true,
            modes: Vec::new(),
            current: None,
            x: 0,
            y: 0,
            scale: 1.0,
            transform: 0,
            displaylink: false,
        }
    }

    const PROFILE: &str = r#"
[[profile]]
name = "Desk"
[[profile.display]]
description = "Alpha P24 A1"
mode = "1920x1080@60"
placement = { x = 0, y = 0 }
"#;

    #[test]
    fn displays_toml_is_read_capped_from_a_regular_file_only() {
        let dir = TempPath::dir("displays-toml");
        let path = dir.join("displays.toml");
        assert_eq!(read_profiles(&path), (Vec::new(), None), "absent: no profile, nothing wrong");
        std::fs::write(&path, PROFILE).unwrap();
        let (profiles, invalid) = read_profiles(&path);
        assert_eq!((profiles.len(), invalid), (1, None));
        std::fs::write(&path, "[[profile").unwrap();
        assert!(matches!(read_profiles(&path), (p, Some(_)) if p.is_empty()), "invalid");
        // A profile past the first 64 KiB is never read.
        std::fs::write(&path, format!("#{}\n{PROFILE}", "a".repeat(70 * 1024))).unwrap();
        assert_eq!(read_profiles(&path), (Vec::new(), None), "never more than 64 KiB");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(read_profiles(&path), (p, Some(_)) if p.is_empty()), "a directory");
        std::fs::remove_dir(&path).unwrap();
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(matches!(read_profiles(&path), (p, Some(_)) if p.is_empty()), "a FIFO, without blocking");
    }

    #[test]
    fn an_invalid_displays_toml_is_logged_once_per_change() {
        let mut seen = InvalidFile::default();
        assert!(!seen.note(None));
        assert!(seen.note(Some(1)));
        assert!(!seen.note(Some(1)), "the same file at the next debounce");
        assert!(seen.note(Some(2)), "changed, still invalid");
        assert!(!seen.note(None));
        assert!(seen.note(Some(2)), "fixed, then broken again");
    }

    #[test]
    fn displaylink_is_read_once_per_new_head_name() {
        let mut links = DisplayLinks::default();
        let mut reads: Vec<String> = Vec::new();
        let mut read = |n: &str| {
            reads.push(n.to_string());
            n.starts_with("DVI")
        };
        let mut heads = vec![head("eDP-1"), head("DVI-I-1")];
        links.mark(&mut heads, &mut read);
        assert_eq!((heads[0].displaylink, heads[1].displaylink), (false, true));
        let mut again = vec![head("eDP-1"), head("DVI-I-1")];
        links.mark(&mut again, &mut read);
        assert!(again[1].displaylink, "kept from the first read");
        links.mark(&mut [head("eDP-1")], &mut read);
        let mut back = vec![head("eDP-1"), head("DVI-I-1")];
        links.mark(&mut back, &mut read);
        assert!(back[1].displaylink);
        assert_eq!(reads, ["eDP-1", "DVI-I-1", "DVI-I-1"], "a head that left and came back is new");
    }

    use crate::outputs::control::WATCHDOG;
    use crate::outputs::model::HeadMode;
    use alpheus_commun::displays::wire::{TrialEnd, TrialState, decode_snapshot, encode_desired};
    use alpheus_commun::displays::{Active, Mode, Scale, Step};
    use alpheus_commun::launcher_ipc::decode_reply;
    use std::time::Duration;

    /// An enabled 1920x1080 60 Hz head at `x`.
    fn moded(name: &str, x: i32) -> Head {
        let mut h = head(name);
        h.make = format!("Make {name}");
        h.modes = vec![HeadMode { width: 1920, height: 1080, mhz: 60_000, preferred: true }];
        h.current = Some(0);
        h.x = x;
        h
    }

    /// A controller that read two screens side by side.
    fn ready(now: Instant) -> Controller {
        let mut c = Controller::new(false);
        c.on_event(Event::Done(vec![moded("DP-1", 0), moded("DP-2", 1920)]), now);
        c
    }

    fn scale_two(output: &str, x: i32) -> String {
        let mode = Mode { width: 1920, height: 1080, refresh: 60 };
        encode_desired(&[Step { output: output.into(), active: Some(Active { mode, x, y: 0, scale: Scale(2000), transform: 0 }) }])
    }

    /// Every configuration sent, its generation and targets.
    #[derive(Default)]
    struct Sent(Vec<(u64, Vec<Target>)>);

    impl Sent {
        fn ok(&mut self) -> impl FnMut(u64, &[Target]) -> bool + '_ {
            |g, t| {
                self.0.push((g, t.to_vec()));
                true
            }
        }
    }

    fn snapshot_of(reply: Option<Reply>) -> Snapshot {
        match reply {
            Some(Reply::Outputs(body)) => decode_snapshot(&body).unwrap(),
            other => panic!("not a snapshot: {other:?}"),
        }
    }

    #[test]
    fn unavailable_says_why() {
        assert_eq!(unavailable(false), Reply::Error("displays are off in bar.toml".into()));
        assert_eq!(unavailable(true), Reply::Error("the compositor has no output management".into()));
    }

    #[test]
    fn the_outputs_verb_answers_from_memory() {
        let now = Instant::now();
        let mut c = ready(now);
        let mut sent = Sent::default();
        let s = snapshot_of(verb(&mut c, &Request::Outputs, now, &mut sent.ok()));
        assert_eq!(s.status, Status::Ready);
        assert_eq!(
            s.outputs.iter().map(|o| (o.name.as_str(), o.active.as_ref().map(|a| a.x))).collect::<Vec<_>>(),
            [("DP-1", Some(0)), ("DP-2", Some(1920))]
        );
        assert_eq!(s.trial, TrialState::None);
        assert!(sent.0.is_empty(), "no compositor round trip");
    }

    #[test]
    fn a_try_waits_for_the_compositor_and_its_answer_opens_the_trial() {
        let now = Instant::now();
        let mut c = ready(now);
        let mut sent = Sent::default();
        assert_eq!(verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok()), None, "pending until the answer");
        assert_eq!(sent.0.len(), 1, "one configuration sent");
        let (generation, targets) = sent.0[0].clone();
        assert_eq!(targets[0].state.map(|s| s.3), Some(2.0));
        let answer = c.on_event(Event::Succeeded(generation), now);
        assert_eq!(execute(&mut c, answer, now, &mut sent.ok()), Some(Ok(())));
        let s = snapshot_of(verb(&mut c, &Request::Outputs, now + Duration::from_secs(3), &mut sent.ok()));
        assert_eq!(s.trial, TrialState::Open { seconds_left: 12 });
    }

    #[test]
    fn a_refused_try_is_answered_with_its_reason() {
        let now = Instant::now();
        let mut c = ready(now);
        let mut sent = Sent::default();
        let answer = verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok());
        assert_eq!(answer, None);
        let refused = c.on_event(Event::Failed(sent.0[0].0), now);
        assert_eq!(
            execute(&mut c, refused, now, &mut sent.ok()).map(try_reply),
            Some(Reply::Error("the compositor refused this configuration".into()))
        );
    }

    #[test]
    fn a_try_refused_before_the_compositor_is_answered_at_once() {
        let now = Instant::now();
        let mut c = ready(now);
        let mut sent = Sent::default();
        let ask = |c: &mut Controller, body: &str, sent: &mut Sent| verb(c, &Request::OutputsTry(body.into()), now, &mut sent.ok());
        assert_eq!(
            ask(&mut c, &scale_two("HDMI-A-9", 0), &mut sent),
            Some(Reply::Error("the compositor refused this configuration".into()))
        );
        assert_eq!(
            ask(&mut c, "step\tDP-1\toff\nstep\tDP-2\toff\n", &mut sent),
            Some(Reply::Error("refused: no screen would stay on".into()))
        );
        assert!(matches!(ask(&mut c, "nonsense", &mut sent), Some(Reply::Error(e)) if e.starts_with("malformed request: ")));
        assert_eq!(ask(&mut c, "", &mut sent), Some(Reply::Done), "nothing to try");
        assert!(sent.0.is_empty());
        assert_eq!(ask(&mut c, &scale_two("DP-1", 0), &mut sent), None);
        assert_eq!(ask(&mut c, &scale_two("DP-2", 1920), &mut sent), Some(Reply::Error("another change is in progress".into())));
        assert_eq!(sent.0.len(), 1);
    }

    #[test]
    fn a_try_that_cannot_be_built_is_answered_at_once() {
        let now = Instant::now();
        let mut c = ready(now);
        let answer = verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut |_, _| false);
        assert!(matches!(answer, Some(Reply::Error(_))), "{answer:?}");
        assert_eq!(c.in_flight(), None, "nothing waits for an answer");
    }

    /// The client waits 7 s for a try; the watchdog answers it after 5 s at most.
    #[test]
    fn an_unanswered_try_is_answered_by_the_watchdog_before_the_client_gives_up() {
        let now = Instant::now();
        let mut c = ready(now);
        let mut sent = Sent::default();
        assert_eq!(verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok()), None);
        let at = c.next_deadline().expect("the watchdog");
        assert!(at <= now + WATCHDOG && WATCHDOG < alpheus_commun::launcher_ipc::OUTPUTS_TRY_TIMEOUT);
        let late = c.tick(&Vec::new, at);
        assert_eq!(execute(&mut c, late, at, &mut sent.ok()), Some(Ok(())), "possibly applied: done");
    }

    #[test]
    fn keep_and_revert_answer_done() {
        for (request, end, sends) in [(Request::OutputsKeep, TrialEnd::Kept, 0), (Request::OutputsRevert, TrialEnd::Reverted, 1)] {
            let now = Instant::now();
            let mut c = ready(now);
            let mut sent = Sent::default();
            verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok());
            let answer = c.on_event(Event::Succeeded(sent.0[0].0), now);
            execute(&mut c, answer, now, &mut sent.ok());
            // The compositor's `done` after the answer: DP-1 at scale 2.
            let mut applied = moded("DP-1", 0);
            applied.scale = 2.0;
            assert!(c.on_event(Event::Done(vec![applied, moded("DP-2", 1920)]), now).is_empty());
            assert_eq!(verb(&mut c, &request, now, &mut sent.ok()), Some(Reply::Done), "{request:?}");
            assert_eq!(sent.0.len(), 1 + sends, "{request:?}: the state from before sent back, or nothing");
            if sends == 1 {
                // The end is reported once the compositor showed the state from before.
                assert_eq!(snapshot_of(verb(&mut c, &Request::Outputs, now, &mut sent.ok())).trial, TrialState::Busy);
                let answer = c.on_event(Event::Succeeded(sent.0[1].0), now);
                execute(&mut c, answer, now, &mut sent.ok());
                c.on_event(Event::Done(vec![moded("DP-1", 0), moded("DP-2", 1920)]), now);
            }
            assert_eq!(snapshot_of(verb(&mut c, &Request::Outputs, now, &mut sent.ok())).trial, TrialState::Ended(end));
        }
        let now = Instant::now();
        let mut c = ready(now);
        let mut sent = Sent::default();
        assert_eq!(verb(&mut c, &Request::OutputsKeep, now, &mut sent.ok()), Some(Reply::Done), "no trial: nothing to do");
        assert_eq!(verb(&mut c, &Request::OutputsRevert, now, &mut sent.ok()), Some(Reply::Done));
        assert!(sent.0.is_empty());
    }

    #[test]
    fn a_waiting_client_gets_the_answer_once() {
        let mut client = TryClient::default();
        let (pending, mut peer) = Pending::pair();
        client.wait(pending, None);
        assert!(client.waiting() && !client.check(), "still there");
        assert!(client.answer(Reply::Error("the compositor refused this configuration".into())));
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).unwrap();
        assert_eq!(decode_reply(&bytes).unwrap(), Reply::Error("the compositor refused this configuration".into()));
        assert!(!client.waiting());
        assert!(!client.answer(Reply::Done), "answered once");
    }

    #[test]
    fn a_second_waiting_client_answers_the_first_as_busy() {
        let mut client = TryClient::default();
        let (first, mut first_peer) = Pending::pair();
        let (second, mut second_peer) = Pending::pair();
        client.wait(first, None);
        client.wait(second, None);
        let mut bytes = Vec::new();
        first_peer.read_to_end(&mut bytes).unwrap();
        assert_eq!(decode_reply(&bytes).unwrap(), Reply::Error("another change is in progress".into()));
        assert!(client.waiting(), "the newer one waits");
        assert!(client.answer(Reply::Done));
        let mut bytes = Vec::new();
        second_peer.read_to_end(&mut bytes).unwrap();
        assert_eq!(decode_reply(&bytes).unwrap(), Reply::Done);
    }

    #[test]
    fn a_panicking_verb_answers_internal_error() {
        assert_eq!(verb_guarded(|| panic!("boom")), Some(Reply::Error("internal error".into())));
        assert_eq!(verb_guarded(|| Some(Reply::Done)), Some(Reply::Done));
        assert_eq!(verb_guarded(|| None), None, "a try kept for the compositor stays kept");
    }

    #[test]
    fn a_client_that_hung_up_mid_try_is_dropped_unanswered() {
        let mut client = TryClient::default();
        let (pending, peer) = Pending::pair();
        client.wait(pending, None);
        drop(peer);
        assert!(client.check(), "nobody waits any more");
        assert!(!client.waiting(), "its connection is closed, not kept until the answer");
        assert!(!client.answer(Reply::Done), "the late answer goes nowhere");
    }
}
