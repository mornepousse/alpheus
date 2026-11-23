//! The displays' wiring on the daemon's state: the output manager bound by `wlr.rs`, its events turned
//! into the controller's, the controller's actions run, and the replies to the socket's displays
//! verbs.
//!
//! This is the glue between three things. `wlr.rs` speaks the compositor's output protocol; `control.rs`
//! is the pure controller (one configuration in flight, the hotplug debounce, the automatic profiles,
//! the DisplayLink 120 Hz rule and the 15 s trial); `model.rs` is the model of the heads. This file hands
//! the controller what happens (a `done`, a verb, the timer firing) and carries out what it answers: it
//! sends a configuration, writes a log line, answers the waiting client, tells the hooks which profile
//! was applied. It is the only file that calls `wlr.send` (a guardrail,
//! `the_daemon_only_reads_from_the_compositor`, checks that); `wlr.rs` builds and applies the
//! configuration. So every change goes through the controller's rules.
//!
//! One `DeadlineTimer` follows `Controller::next_deadline`: the debounce, the watchdog, the trial, or the
//! retry of a profile the 3-a-minute limit skipped; none at rest. DisplayLink heads are told from
//! `[displays] drm_dir`, read once per new head name, never watched, and nothing is opened under `/dev`.
//!
//! Never:
//! - send a configuration that the controller did not ask for, or write to the compositor in any other
//!   way (power management included);
//! - write a screen's identity or an error text that may quote one into a log line: only the spec's fixed
//!   lines, and a profile's name, which is the user's own word (`profile_line`);
//! - start a thread or a command (the module is checked by `the_tray_spawns_no_thread`), or read
//!   anything unbounded: `displays.toml` is read capped, from a regular file only.
//!
//! See: docs/specs/2026-10-30-alpheus-displays-protocol-design.md (§§ 1 and 2),
//! docs/features/displays.md, and docs/decisions.md ("The displays are changed by the daemon, and its
//! trial lives there", "displays.toml is read at each evaluation of the profiles, never watched", "A DisplayLink
//! screen is told from the DRM directory, once per screen name").

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
use alpheus_common::displays::profiles::{self, Profile};
use alpheus_common::displays::wire::{ERROR_NO_MANAGER, ERROR_NO_TRIAL, ERROR_OFF, Snapshot, Status, decode_desired, encode_snapshot};
use alpheus_common::launcher_ipc::{Reply, Request};
use alpheus_common::paths;
use alpheus_common::theme::settings::MAX_FILE;
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
/// A displays.toml parsed past this many bytes hands its parse's peak back to the system.
const RELEASE_AFTER: usize = 8 * 1024;

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
    /// The profiles the compositor accepted, waiting for `displays_applied` to hand them to the hooks.
    applied: Vec<String>,
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
            applied: Vec::new(),
        })
    }

    /// Runs the controller's actions; a configuration that cannot be built is answered as
    /// invalid, at once.
    fn run(&mut self, actions: Vec<Action>, qh: &QueueHandle<App>, now: Instant) {
        let OutputsApp { controller, wlr, client, applied, .. } = self;
        if let Some(answer) = execute(controller, actions, now, &mut |g, t| wlr.send(g, t, qh).is_ok(), applied) {
            client.answer(try_reply(answer));
        }
        // A configuration the controller gave up on (the watchdog) is destroyed.
        self.wlr.abandon(self.controller.in_flight());
    }

    /// One displays verb: its reply now, or None for a try whose client is kept until the
    /// compositor answers.
    fn verb(&mut self, request: &Request, qh: &QueueHandle<App>) -> Option<Reply> {
        let now = Instant::now();
        let OutputsApp { controller, wlr, applied, .. } = self;
        let reply = verb(controller, request, now, &mut |g, t| wlr.send(g, t, qh).is_ok(), applied);
        self.wlr.abandon(self.controller.in_flight());
        reply
    }
}

/// A displays verb behind the error boundary of the other verbs: a panic (a hostile body, a
/// bug) answers `internal error` and never takes the bar down.
fn verb_guarded(f: impl FnOnce() -> Option<Reply>) -> Option<Reply> {
    // `guarded` gives None when `f` panicked; otherwise the verb's own reply, which is itself an
    // `Option` (None: a try whose client waits), hence the double layer.
    match guarded("displays verb", f) {
        Some(reply) => reply,
        None => Some(Reply::Error("internal error".into())),
    }
}

/// Runs the controller's actions; `send` builds and sends one configuration (false: it could
/// not be built, answered as invalid at once). Returns the try's answer when one came; the
/// profiles the compositor accepted are pushed on `applied`.
pub(crate) fn execute(
    controller: &mut Controller,
    actions: Vec<Action>,
    now: Instant,
    send: &mut dyn FnMut(u64, &[Target]) -> bool,
    applied: &mut Vec<String>,
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
            Action::ProfileApplied(name) => applied.push(name),
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
    applied: &mut Vec<String>,
) -> Option<Reply> {
    let actions = match request {
        Request::Outputs => return Some(Reply::Outputs(encode_snapshot(&snapshot(controller, now)))),
        Request::OutputsTry(body) => {
            let steps = match decode_desired(body) {
                Ok(steps) => steps,
                Err(e) => return Some(Reply::Error(format!("malformed request: {e}"))),
            };
            match controller.try_(&steps, now) {
                Ok(actions) => return execute(controller, actions, now, send, applied).map(try_reply),
                Err(e) => return Some(Reply::Error(e.into())),
            }
        }
        // Since the daemon started, no try: a trial it held before is lost (never "done").
        Request::OutputsKeep | Request::OutputsRevert if !controller.trial_seen() => return Some(Reply::Error(ERROR_NO_TRIAL.into())),
        Request::OutputsKeep => controller.keep(now),
        Request::OutputsRevert => controller.revert(now),
        _ => return Some(Reply::Error("not a displays verb".into())),
    };
    // Keep and revert answer no try; what they send is answered by the compositor later.
    execute(controller, actions, now, send, applied);
    Some(Reply::Done)
}

/// The answer to a displays verb when there is no controller: the module is off, or the
/// compositor has no output manager.
pub(crate) fn unavailable(module_on: bool) -> Reply {
    let why = if module_on { ERROR_NO_MANAGER } else { ERROR_OFF };
    Reply::Error(why.into())
}

/// The client of the try in flight, and the watch on its hang-up (edge-triggered, as the
/// dmenu client's: a client that half-closed would stay readable). At most one. The watch exists
/// only while the client waits for the compositor's answer (the watchdog's 5 s at most).
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
        if let Some(pending) = &self.pending
            && pending.hung_up()
        {
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
        self.displays_applied();
    }

    /// The profiles the compositor accepted since the last call go to the hooks.
    fn displays_applied(&mut self) {
        let Some(d) = self.displays.as_mut() else { return };
        for name in std::mem::take(&mut d.applied) {
            self.hooks_displays(&name);
        }
    }

    /// One event to the controller, its actions run, the timer following.
    pub(crate) fn displays_event(&mut self, e: Event) {
        let now = Instant::now();
        let Some(d) = self.displays.as_mut() else { return };
        let actions = d.controller.on_event(e, now);
        d.run(actions, &self.qh, now);
        self.displays_sync();
        self.displays_applied();
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
        app.displays_applied();
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
        self.displays_applied();
    }

    /// The try's client watch fired: true once nobody waits (the watch is removed). `is_none_or` is
    /// true when there are no displays at all, so a watch left behind removes itself.
    fn displays_client_check(&mut self) -> bool {
        self.displays.as_mut().is_none_or(|d| d.client.check())
    }

    /// The daemon stops: a trial, open or flying, gets its state from before sent back (the
    /// shutdown's flush carries it; no answer is waited for).
    pub(crate) fn displays_shutdown(&mut self) {
        let now = Instant::now();
        let Some(d) = self.displays.as_mut() else { return };
        let actions = d.controller.shutdown(now);
        d.run(actions, &self.qh, now);
    }

    /// The compositor stopped the manager: the timer goes, then everything else.
    pub(crate) fn displays_gone(&mut self) {
        if let Some(mut d) = self.displays.take() {
            d.client.answer(Reply::Error(ERROR_NO_MANAGER.into()));
            d.wlr.abandon(None);
            d.timer.sync_opt(self.loop_handle.as_ref(), None, App::displays_tick);
        }
    }
}

/// displays.toml's profiles, and a fingerprint of the file when it is invalid or unreadable
/// (None when absent or valid). At most `MAX_FILE` bytes of a regular file are read; the open
/// never blocks (a FIFO is opened non-blocking, then refused).
pub(crate) fn read_profiles(path: &Path) -> (Vec<Profile>, Option<u64>) {
    read_profiles_with(path, &mut crate::memory::release_memory)
}

/// `read_profiles`; `release` runs after a parse of more than `RELEASE_AFTER` bytes.
fn read_profiles_with(path: &Path, release: &mut dyn FnMut()) -> (Vec<Profile>, Option<u64>) {
    match read_regular(path) {
        Ok(text) => {
            let read = match profiles::parse(&text) {
                Ok(p) => (p, None),
                Err(_) => {
                    let mut h = DefaultHasher::new();
                    text.hash(&mut h);
                    (Vec::new(), Some(h.finish()))
                }
            };
            // The parse peaks at many times the file's size; glibc would keep that high-water.
            if text.len() > RELEASE_AFTER {
                drop(text);
                release();
            }
            read
        }
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
    // `flatten` skips the entries that could not be read.
    for entry in entries.flatten().take(MAX_DRM_ENTRIES) {
        let file_name = entry.file_name();
        let Some(entry_name) = file_name.to_str() else { continue };
        if connector_of(entry_name) != Some(name) {
            continue;
        }
        for link in ["device/device/driver", "device/driver"] {
            let Ok(target) = std::fs::read_link(entry.path().join(link)) else { continue };
            let Some(driver) = target.file_name() else { continue };
            if driver.to_string_lossy().ends_with("evdi") {
                return true;
            }
        }
    }
    false
}

/// The connector of a `card<N>-<connector>` entry.
fn connector_of(entry: &str) -> Option<&str> {
    let rest = entry.strip_prefix("card")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    // The digits are ASCII, so `digits` is a character boundary and the slice cannot panic.
    let connector = rest[digits..].strip_prefix('-')?;
    if connector.is_empty() {
        return None;
    }
    Some(connector)
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
    //! Tests of the displays' wiring: the DRM directory, `displays.toml` reading, the verbs against a
    //! controller with a fake sender, and the waiting client. No Wayland and no real `/sys`.

    use super::*;
    use alpheus_common::temp_path::TempPath;
    use std::os::unix::fs::symlink;

    /// A sysfs-shaped tree: the connector's `device` links to its card, whose `device/driver`
    /// names the driver.
    fn add_drm_card(drm: &Path, card: &str, driver: &str, connectors: &[&str]) {
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
        add_drm_card(&drm, "card0", "i915", &["eDP-1", "DVI-I-10", "HDMI-A-1"]);
        add_drm_card(&drm, "card2", "evdi", &["DVI-I-1", "HDMI-A-11"]);
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

    #[test]
    fn a_driver_name_must_end_in_evdi_and_an_unreadable_name_is_not_displaylink() {
        use std::os::unix::ffi::OsStrExt;
        let drm = TempPath::dir("drm-driver-names");
        add_drm_card(&drm, "card1", "nvidia-evdi", &["DP-7"]);
        add_drm_card(&drm, "card2", "evdi-extra", &["DP-8"]);
        add_drm_card(&drm, "card3", "i915", &["eDP-1"]);
        // A directory whose name is not UTF-8 is skipped, never taken for a match.
        std::fs::create_dir(drm.join(std::ffi::OsStr::from_bytes(b"card4-DP-9\xff"))).unwrap();
        assert!(is_displaylink(&drm, "DP-7"), "a driver name ending in evdi");
        assert!(!is_displaylink(&drm, "DP-8"), "a driver name that only starts with evdi");
        assert!(!is_displaylink(&drm, "eDP-1"), "another screen is not claimed by the unreadable entry");
    }

    /// An enabled head called `name`, with nothing else filled in.
    fn a_head(name: &str) -> Head {
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
        let mut heads = vec![a_head("eDP-1"), a_head("DVI-I-1")];
        links.mark(&mut heads, &mut read);
        assert_eq!((heads[0].displaylink, heads[1].displaylink), (false, true));
        let mut again = vec![a_head("eDP-1"), a_head("DVI-I-1")];
        links.mark(&mut again, &mut read);
        assert!(again[1].displaylink, "kept from the first read");
        links.mark(&mut [a_head("eDP-1")], &mut read);
        let mut back = vec![a_head("eDP-1"), a_head("DVI-I-1")];
        links.mark(&mut back, &mut read);
        assert!(back[1].displaylink);
        assert_eq!(reads, ["eDP-1", "DVI-I-1", "DVI-I-1"], "a head that left and came back is new");
    }

    use crate::outputs::control::WATCHDOG;
    use crate::outputs::model::HeadMode;
    use alpheus_common::displays::wire::{TrialEnd, TrialState, decode_snapshot, encode_desired};
    use alpheus_common::displays::{Active, Mode, Scale, Step};
    use alpheus_common::launcher_ipc::decode_reply;
    use std::time::Duration;

    /// An enabled 1920x1080 60 Hz head at `x`.
    fn a_head_with_a_mode(name: &str, x: i32) -> Head {
        let mut h = a_head(name);
        h.make = format!("Make {name}");
        h.modes = vec![HeadMode { width: 1920, height: 1080, mhz: 60_000, preferred: true }];
        h.current = Some(0);
        h.x = x;
        h
    }

    /// A controller that read two screens side by side.
    fn a_controller_with_two_screens(now: Instant) -> Controller {
        let mut c = Controller::new(false);
        c.on_event(Event::Done(vec![a_head_with_a_mode("DP-1", 0), a_head_with_a_mode("DP-2", 1920)]), now);
        c
    }

    /// The body of an `outputs-try` that puts `output` at `x` with a scale of 2.
    fn scale_two(output: &str, x: i32) -> String {
        let mode = Mode { width: 1920, height: 1080, refresh: 60 };
        encode_desired(&[Step { output: output.into(), active: Some(Active { mode, x, y: 0, scale: Scale(2000), transform: 0 }) }])
    }

    /// Every configuration sent, its generation and targets.
    #[derive(Default)]
    struct Sent(Vec<(u64, Vec<Target>)>);

    impl Sent {
        /// A sender that records every configuration and says it was built.
        fn ok(&mut self) -> impl FnMut(u64, &[Target]) -> bool + '_ {
            |g, t| {
                self.0.push((g, t.to_vec()));
                true
            }
        }
    }

    /// The snapshot a reply carries; any other reply fails the test.
    fn snapshot_of(reply: Option<Reply>) -> Snapshot {
        match reply {
            Some(Reply::Outputs(body)) => decode_snapshot(&body).unwrap(),
            other => panic!("not a snapshot: {other:?}"),
        }
    }

    #[test]
    fn executing_actions_collects_the_profiles_the_compositor_accepted() {
        let now = Instant::now();
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        let mut applied = Vec::new();
        let actions = vec![Action::ProfileApplied("Desk-Test".into()), Action::Log("displays: x")];
        assert_eq!(execute(&mut c, actions, now, &mut sent.ok(), &mut applied), None, "no answer for it");
        assert_eq!(applied, ["Desk-Test"]);
        assert!(sent.0.is_empty());
    }

    #[test]
    fn an_unavailable_verb_says_whether_displays_is_off_or_the_compositor_has_no_manager() {
        assert_eq!(unavailable(false), Reply::Error("displays are off in bar.toml".into()));
        assert_eq!(unavailable(true), Reply::Error("the compositor has no output management".into()));
    }

    #[test]
    fn the_outputs_verb_answers_from_memory() {
        let now = Instant::now();
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        let s = snapshot_of(verb(&mut c, &Request::Outputs, now, &mut sent.ok(), &mut Vec::new()));
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
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        assert_eq!(
            verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok(), &mut Vec::new()),
            None,
            "pending until the answer"
        );
        assert_eq!(sent.0.len(), 1, "one configuration sent");
        let (generation, targets) = sent.0[0].clone();
        assert_eq!(targets[0].state.map(|s| s.3), Some(2.0));
        let answer = c.on_event(Event::Succeeded(generation), now);
        assert_eq!(execute(&mut c, answer, now, &mut sent.ok(), &mut Vec::new()), Some(Ok(())));
        let s = snapshot_of(verb(&mut c, &Request::Outputs, now + Duration::from_secs(3), &mut sent.ok(), &mut Vec::new()));
        assert_eq!(s.trial, TrialState::Open { seconds_left: 12 });
    }

    #[test]
    fn a_refused_try_is_answered_with_its_reason() {
        let now = Instant::now();
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        let answer = verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok(), &mut Vec::new());
        assert_eq!(answer, None);
        let refused = c.on_event(Event::Failed(sent.0[0].0), now);
        assert_eq!(
            execute(&mut c, refused, now, &mut sent.ok(), &mut Vec::new()).map(try_reply),
            Some(Reply::Error("the compositor refused this configuration".into()))
        );
    }

    #[test]
    fn a_try_refused_before_the_compositor_is_answered_at_once() {
        let now = Instant::now();
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        let ask = |c: &mut Controller, body: &str, sent: &mut Sent| {
            verb(c, &Request::OutputsTry(body.into()), now, &mut sent.ok(), &mut Vec::new())
        };
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
        let mut c = a_controller_with_two_screens(now);
        let answer = verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut |_, _| false, &mut Vec::new());
        assert!(matches!(answer, Some(Reply::Error(_))), "{answer:?}");
        assert_eq!(c.in_flight(), None, "nothing waits for an answer");
    }

    /// The client waits 7 s for a try; the watchdog answers it after 5 s at most.
    #[test]
    fn an_unanswered_try_is_answered_by_the_watchdog_before_the_client_gives_up() {
        let now = Instant::now();
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        assert_eq!(verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok(), &mut Vec::new()), None);
        let at = c.next_deadline().expect("the watchdog");
        assert!(at <= now + WATCHDOG && WATCHDOG < alpheus_common::launcher_ipc::OUTPUTS_TRY_TIMEOUT);
        let late = c.tick(&Vec::new, at);
        assert_eq!(execute(&mut c, late, at, &mut sent.ok(), &mut Vec::new()), Some(Ok(())), "possibly applied: done");
    }

    #[test]
    fn keep_and_revert_answer_done() {
        for (request, end, sends) in [(Request::OutputsKeep, TrialEnd::Kept, 0), (Request::OutputsRevert, TrialEnd::Reverted, 1)] {
            let now = Instant::now();
            let mut c = a_controller_with_two_screens(now);
            let mut sent = Sent::default();
            verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok(), &mut Vec::new());
            let answer = c.on_event(Event::Succeeded(sent.0[0].0), now);
            execute(&mut c, answer, now, &mut sent.ok(), &mut Vec::new());
            // The compositor's `done` after the answer: DP-1 at scale 2.
            let mut applied = a_head_with_a_mode("DP-1", 0);
            applied.scale = 2.0;
            assert!(c.on_event(Event::Done(vec![applied, a_head_with_a_mode("DP-2", 1920)]), now).is_empty());
            assert_eq!(verb(&mut c, &request, now, &mut sent.ok(), &mut Vec::new()), Some(Reply::Done), "{request:?}");
            assert_eq!(sent.0.len(), 1 + sends, "{request:?}: the state from before sent back, or nothing");
            if sends == 1 {
                // The end is reported once the compositor showed the state from before.
                assert_eq!(snapshot_of(verb(&mut c, &Request::Outputs, now, &mut sent.ok(), &mut Vec::new())).trial, TrialState::Busy);
                let answer = c.on_event(Event::Succeeded(sent.0[1].0), now);
                execute(&mut c, answer, now, &mut sent.ok(), &mut Vec::new());
                c.on_event(Event::Done(vec![a_head_with_a_mode("DP-1", 0), a_head_with_a_mode("DP-2", 1920)]), now);
            }
            assert_eq!(snapshot_of(verb(&mut c, &Request::Outputs, now, &mut sent.ok(), &mut Vec::new())).trial, TrialState::Ended(end));
        }
        // No trial now, but one was seen (a revert racing the daemon's own deadline): nothing to do.
        let now = Instant::now();
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        verb(&mut c, &Request::OutputsTry(scale_two("DP-1", 0)), now, &mut sent.ok(), &mut Vec::new());
        let refused = c.on_event(Event::Failed(sent.0[0].0), now);
        execute(&mut c, refused, now, &mut sent.ok(), &mut Vec::new());
        assert_eq!(verb(&mut c, &Request::OutputsKeep, now, &mut sent.ok(), &mut Vec::new()), Some(Reply::Done), "no trial: nothing to do");
        assert_eq!(verb(&mut c, &Request::OutputsRevert, now, &mut sent.ok(), &mut Vec::new()), Some(Reply::Done));
        assert_eq!(sent.0.len(), 1, "nothing sent but the try");
    }

    /// A keep or a revert reaching a daemon that never took a try (it
    /// restarted mid-trial) says so, rather than a done the TUI would read as rolled back.
    #[test]
    fn keep_and_revert_without_any_trial_say_the_daemon_restarted() {
        let now = Instant::now();
        let mut c = a_controller_with_two_screens(now);
        let mut sent = Sent::default();
        for request in [Request::OutputsKeep, Request::OutputsRevert] {
            assert_eq!(
                verb(&mut c, &request, now, &mut sent.ok(), &mut Vec::new()),
                Some(Reply::Error("no trial in progress (the daemon restarted)".into())),
                "{request:?}"
            );
        }
        assert!(sent.0.is_empty());
    }

    /// A displays.toml past 8 KiB hands the parse's peak back to the system; a small one does not.
    #[test]
    fn a_large_displays_toml_releases_its_parse_memory() {
        let dir = TempPath::dir("displays-toml-release");
        let path = dir.join("displays.toml");
        let mut released = 0;
        std::fs::write(&path, PROFILE).unwrap();
        assert_eq!(read_profiles_with(&path, &mut || released += 1).0.len(), 1);
        assert_eq!(released, 0, "small");
        std::fs::write(&path, format!("#{}\n{PROFILE}", "a".repeat(8 * 1024))).unwrap();
        assert_eq!(read_profiles_with(&path, &mut || released += 1).0.len(), 1);
        assert_eq!(released, 1, "past 8 KiB");
        std::fs::write(&path, format!("#{}\n[[profile", "a".repeat(8 * 1024))).unwrap();
        assert!(read_profiles_with(&path, &mut || released += 1).1.is_some());
        assert_eq!(released, 2, "parsed, even when invalid");
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
