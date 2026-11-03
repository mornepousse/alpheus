//! The displays' wiring (displays spec § 1): the output manager bound by `outputs::wlr`, its
//! events turned into the controller's, the controller's actions run, and one `DeadlineTimer`
//! following `Controller::next_deadline` (the debounce, the watchdog or the trial; none at rest).
//! DisplayLink heads are told from `[displays] drm_dir`, read once per new head name, never
//! watched, nothing opened under /dev. Logs only the spec's fixed lines.

use crate::config::DisplaysConfig;
use crate::deadline::DeadlineTimer;
use crate::log::write as log;
use crate::outputs::control::{Action, Controller, Event, ReplyTo};
use crate::outputs::model::Head;
use crate::outputs::wlr::Wlr;
use crate::wayland::App;
use alpheus_commun::displays::profiles::{self, Profile};
use alpheus_commun::paths;
use alpheus_commun::theme::settings::MAX_FILE;
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
    /// The answer to the TUI's last try, for the socket verb to send.
    #[allow(dead_code)]
    pub(crate) reply: Option<Result<(), &'static str>>,
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
            reply: None,
        })
    }

    /// Runs the controller's actions; a configuration that cannot be built is answered as
    /// invalid, at once.
    fn run(&mut self, actions: Vec<Action>, qh: &QueueHandle<App>, now: Instant) {
        let mut queue: VecDeque<Action> = actions.into();
        while let Some(action) = queue.pop_front() {
            match action {
                Action::Send(generation, targets) => {
                    if self.wlr.send(generation, &targets, qh).is_err() {
                        queue.extend(self.controller.on_event(Event::Invalid(generation), now));
                    }
                }
                Action::Log(line) => log(line),
                Action::LogProfile(name) => log(&profile_line(&name)),
                Action::Reply(ReplyTo::Try, answer) => self.reply = Some(answer),
            }
        }
        // A configuration the controller gave up on (the watchdog) is destroyed.
        self.wlr.abandon(self.controller.in_flight());
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

    /// The compositor stopped the manager: the timer goes, then everything else.
    pub(crate) fn displays_gone(&mut self) {
        if let Some(mut d) = self.displays.take() {
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
}
