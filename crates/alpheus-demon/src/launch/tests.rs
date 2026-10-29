use super::*;
use crate::compositor::example;
use crate::config::{ModuleId, ResourcesConfig};
use crate::sources::dbus::{Domain, SystemState, messages};
use crate::system::*;
use alpheus_commun::launcher_ipc::DmenuFlags;

const BEFORE: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };
const AFTER: LocalTime = LocalTime { minute: 47, ..BEFORE };

/// Fix round, 2026-10-06 (reviewer finding 4 on task 11): `watch_fd`
/// failing after a successful connection used to log the exact same
/// line as `SessionBus::session()` itself failing, hiding which step
/// actually broke.
#[test]
fn watch_fd_failing_says_which_step_broke_not_no_session_bus() {
    let connect = no_session_bus(std::io::Error::other("boom"));
    let watch = watch_fd_failed(std::io::Error::other("boom"));
    assert_ne!(connect, watch, "two different steps must not share one line");
    assert!(connect.contains("no session bus"), "{connect:?}");
    assert!(!watch.contains("no session bus"), "the bus did connect: {watch:?}");
    assert!(watch.contains("fd"), "names the step that failed: {watch:?}");
}

/// Privacy delivery 2: the camera watch (an inotify instance, a scan) exists only with `privacy`.
#[test]
fn the_camera_watch_starts_only_with_privacy() {
    assert!(camera_wanted(&[ModuleId::Privacy]));
    assert!(camera_wanted(&[ModuleId::Clock, ModuleId::Privacy]));
    assert!(!camera_wanted(&[ModuleId::Clock, ModuleId::Volume]));
    assert!(!camera_wanted(&[]));
    let without: Vec<ModuleId> = ModuleId::ALL.into_iter().filter(|m| *m != ModuleId::Privacy).collect();
    assert!(!camera_wanted(&without));
}

#[test]
fn privacy_alone_starts_the_sound_source() {
    assert!(wanted(&[ModuleId::Privacy]).sound);
    assert_eq!(
        wanted(&[ModuleId::Privacy]),
        Wanted { sound: true, dbus: vec![], resources: false, tray: false, media: false, notifications: false },
        "the capture graph needs pw-dump and nothing else"
    );
    let without: Vec<ModuleId> = ModuleId::ALL
        .into_iter()
        .filter(|m| !matches!(m, ModuleId::Privacy | ModuleId::Volume | ModuleId::Osd | ModuleId::Quick))
        .collect();
    assert!(!wanted(&without).sound, "every other module: no pw-dump");
}

#[test]
fn only_the_sources_the_modules_need() {
    let all = vec![Domain::Network, Domain::Bluetooth, Domain::Battery, Domain::Power];
    assert_eq!(wanted(&ModuleId::ALL), Wanted { sound: true, dbus: all, resources: true, tray: true, media: true, notifications: true });
    assert_eq!(
        wanted(&[ModuleId::Workspaces, ModuleId::Window, ModuleId::Clock]),
        Wanted { sound: false, dbus: vec![], resources: false, tray: false, media: false, notifications: false }
    );
    assert_eq!(
        wanted(&[ModuleId::Battery]),
        Wanted { sound: false, dbus: vec![Domain::Battery], resources: false, tray: false, media: false, notifications: false }
    );
    assert_eq!(
        wanted(&[ModuleId::Bluetooth, ModuleId::Clock]),
        Wanted { sound: false, dbus: vec![Domain::Bluetooth], resources: false, tray: false, media: false, notifications: false }
    );
    assert_eq!(
        wanted(&[ModuleId::Battery, ModuleId::Network]),
        Wanted {
            sound: false,
            dbus: vec![Domain::Network, Domain::Battery],
            resources: false,
            tray: false,
            media: false,
            notifications: false
        },
        "reading order, whatever the bar's order"
    );
    assert_eq!(
        wanted(&[ModuleId::Volume, ModuleId::Resources]),
        Wanted { sound: true, dbus: vec![], resources: true, tray: false, media: false, notifications: false }
    );
    assert_eq!(
        wanted(&[ModuleId::Quick]).dbus,
        vec![Domain::Network, Domain::Bluetooth, Domain::Power],
        "the panel's Wi-Fi row (quick Wi-Fi decision 3), the Bluetooth row (delivery 3 decision 1: the bar's BtState) and power profile: read only, on the system bus"
    );
    assert_eq!(
        wanted(&[ModuleId::Quick, ModuleId::Network]).dbus,
        vec![Domain::Network, Domain::Bluetooth, Domain::Power],
        "the network domain read once for the module and the panel"
    );
}

/// Power spec § 1: the power button reads nothing: no source, no thread.
#[test]
fn the_power_module_wants_no_source() {
    assert_eq!(
        wanted(&[ModuleId::Power]),
        Wanted { sound: false, dbus: vec![], resources: false, tray: false, media: false, notifications: false }
    );
}

#[test]
fn wanted_tray_only_with_the_module() {
    assert!(wanted(&[ModuleId::Tray]).tray);
    assert!(wanted(&[ModuleId::Workspaces, ModuleId::Tray, ModuleId::Clock]).tray);
    let without: Vec<ModuleId> = ModuleId::ALL.into_iter().filter(|m| *m != ModuleId::Tray).collect();
    assert!(!wanted(&without).tray, "every other module: no session bus");
    assert_eq!(
        wanted(&[ModuleId::Tray]),
        Wanted { sound: false, dbus: vec![], resources: false, tray: true, media: false, notifications: false },
        "the tray alone"
    );
}

#[test]
fn wanted_media_only_with_the_module() {
    assert!(wanted(&[ModuleId::Media]).media);
    let without: Vec<ModuleId> = ModuleId::ALL.into_iter().filter(|m| *m != ModuleId::Media).collect();
    assert!(!wanted(&without).media, "every other module: no MPRIS");
    assert_eq!(
        wanted(&[ModuleId::Media]),
        Wanted { sound: false, dbus: vec![], resources: false, tray: false, media: true, notifications: false },
        "media alone needs no other source"
    );
}

#[test]
fn wanted_notifications_only_with_the_module() {
    assert!(wanted(&[ModuleId::Notifications]).notifications);
    let without: Vec<ModuleId> = ModuleId::ALL.into_iter().filter(|m| *m != ModuleId::Notifications).collect();
    assert!(!wanted(&without).notifications, "every other module: no RequestName");
    assert_eq!(
        wanted(&[ModuleId::Notifications]),
        Wanted { sound: false, dbus: vec![], resources: false, tray: false, media: false, notifications: true },
        "the bell alone needs no other source"
    );
}

#[test]
fn the_resources_period_comes_from_bar_toml() {
    assert_eq!(resources_period(&ResourcesConfig::default()), Duration::from_secs(2));
    assert_eq!(resources_period(&ResourcesConfig { interval: 5, ..ResourcesConfig::default() }), Duration::from_secs(5));
}

#[test]
fn system_messages_redraw_once_and_only_resources_resets_the_clock() {
    let mut c = Counter { bar: Bar::new(BarConfig::default()), dirty: false, redraws: 0, osd_flushes: 0 };
    let s = SystemState {
        network: Some(NetState::Offline),
        bluetooth: Some(BtState::Off),
        battery: Some(PowerState::default()),
        power: None,
        wifi: None,
        wifi_open: 0,
        bt: None,
        bt_open: 0,
    };
    for m in messages(Some(s)) {
        on_message(&mut c, m, || AFTER);
    }
    on_message(&mut c, Msg::Sound(Some(SoundState::default())), || AFTER);
    assert_eq!(c.bar.clock, None, "network, bluetooth, battery, sound: not a clock-refreshing event");
    on_message(&mut c, Msg::Resources(Sample { cpu: Some(5), memory: Some(61), temperature: None }), || AFTER);
    after_dispatch(&mut c);
    assert_eq!(c.redraws, 1, "a burst of system messages: one redraw");
    assert_eq!(c.bar.clock, Some(AFTER), "a resources tick also puts the clock right (B-m4)");
}

#[test]
fn a_compositor_event_or_a_resources_tick_resets_the_clock() {
    let mut b = Bar::new(BarConfig::default());
    assert!(dispatch_msg(&mut b, Msg::Clock(BEFORE), || AFTER));
    assert_eq!(b.clock, Some(BEFORE), "a clock tick does not reread the time");
    assert!(dispatch_msg(&mut b, Msg::Compositor(Some(example())), || AFTER));
    assert_eq!(b.clock, Some(AFTER), "coming out of sleep: the clock follows the first event");
    assert!(!dispatch_msg(&mut b, Msg::Compositor(Some(example())), || AFTER), "nothing new: no redraw");
    // Resources has no down state, so a sleeping laptop with nothing
    // compositor-related happening right after resume (no focus
    // change yet) still gets the clock corrected by the next reading.
    assert!(dispatch_msg(&mut b, Msg::Clock(BEFORE), || AFTER), "clock put back stale for this part of the test");
    assert_eq!(b.clock, Some(BEFORE));
    let sample = Sample { cpu: Some(10), memory: Some(50), temperature: Some(40) };
    assert!(dispatch_msg(&mut b, Msg::Resources(sample), || AFTER));
    assert_eq!(b.clock, Some(AFTER), "a resources tick follows the clock too, with no compositor event needed");
}

/// Replaces `App` (wayland.rs, a real Wayland connection): counts
/// redraws instead of actually drawing.
struct Counter {
    bar: Bar,
    dirty: bool,
    redraws: u32,
    osd_flushes: u32,
}

impl DeferredRedraw for Counter {
    fn bar_mut(&mut self) -> &mut Bar {
        &mut self.bar
    }
    fn dirty_mut(&mut self) -> &mut bool {
        &mut self.dirty
    }
    fn redraw_now(&mut self) {
        self.redraws += 1;
    }
    fn flush_osd(&mut self) {
        self.osd_flushes += 1;
    }
}

/// Final review (slice B): a burst of sound messages updates the OSD's model each time but draws
/// its pill once, after the pass, whether the bar redraws or not (a microphone flip alone does
/// not redraw the bar).
#[test]
fn the_osd_is_drawn_once_per_pass_with_or_without_a_bar_redraw() {
    let mut c = Counter { bar: Bar::new(BarConfig::default()), dirty: false, redraws: 0, osd_flushes: 0 };
    after_dispatch(&mut c);
    assert_eq!((c.redraws, c.osd_flushes), (0, 1), "no bar redraw, the pill's draw still flushed");
    for p in [40, 41, 42] {
        on_message(
            &mut c,
            Msg::Sound(Some(SoundState { sink: Some(Volume { percent: p, muted: false }), ..SoundState::default() })),
            || AFTER,
        );
    }
    after_dispatch(&mut c);
    assert_eq!((c.redraws, c.osd_flushes), (1, 2), "one of each per pass");
}

#[test]
fn a_burst_of_messages_redraws_only_once() {
    let mut c = Counter { bar: Bar::new(BarConfig::default()), dirty: false, redraws: 0, osd_flushes: 0 };
    // A burst of 9 messages that EACH change the display (toggling
    // Some/None, like the up to 9 grids from a single niri burst over 3
    // screens): each one calls `on_message`, none draws directly —
    // otherwise this test would not bite (a repeated message, itself,
    // changes nothing from the second one on: see
    // `state::tests::messages_and_changes`).
    for i in 0..9 {
        let e = if i % 2 == 0 { Some(example()) } else { None };
        on_message(&mut c, Msg::Compositor(e), || BEFORE);
    }
    after_dispatch(&mut c);
    assert_eq!(c.redraws, 1, "burst of 9 messages that all change the display: one redraw, not 9");
    assert!(!c.dirty, "the flag drops back down after the redraw");
    // Nothing new in the next burst: no redraw at all.
    on_message(&mut c, Msg::Compositor(Some(example())), || BEFORE);
    after_dispatch(&mut c);
    assert_eq!(c.redraws, 1, "nothing changed: no further redraw");
}

#[test]
fn exit_code_dispatch_failure_returns_1() {
    assert_eq!(exit_code(false), 0, "requested stop: code 0, a supervisor must not relaunch");
    assert_eq!(exit_code(true), 1, "the event loop's dispatch failed: code != 0, a supervisor must relaunch");
}

#[test]
fn end_of_session_recognizes_epipe_and_econnreset_only() {
    assert!(end_of_session(&io::Error::from(io::ErrorKind::BrokenPipe)), "EPIPE: compositor gone, socket closed");
    assert!(end_of_session(&io::Error::from(io::ErrorKind::ConnectionReset)), "ECONNRESET: same thing on read");
    assert!(!end_of_session(&io::Error::from(io::ErrorKind::PermissionDenied)), "a real error stays a failure");
    assert!(!end_of_session(&io::Error::other("protocol error")), "same here");
}

#[test]
fn end_of_session_traverses_calloop_nesting() {
    use smithay_client_toolkit::reexports::calloop::Error as LoopError;
    // Observed on this machine (headless sway, compositor killed):
    // calloop sometimes returns an `OtherError` wrapping a source's
    // `IoError`, instead of returning it directly — the real system
    // error is then at the second level of `source()`, not the first.
    let direct = LoopError::IoError(io::Error::from(io::ErrorKind::BrokenPipe));
    assert!(end_of_session(&direct), "direct case: IoError at the first level");
    let nested = LoopError::OtherError(Box::new(LoopError::IoError(io::Error::from(io::ErrorKind::BrokenPipe))));
    assert!(end_of_session(&nested), "observed case: OtherError(IoError(EPIPE))");
    let unrelated = LoopError::OtherError(Box::new(io::Error::other("protocol error")));
    assert!(!end_of_session(&unrelated), "nested but not EPIPE/ECONNRESET: still a failure");
}

#[test]
fn settings_absent_or_invalid() {
    use crate::launcher::config::LauncherConfig;
    let d = std::env::temp_dir().join(format!("alpheus-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let mut expected = BarConfig::default();
    crate::config::resolve_wifi_more(&mut expected.quick, &LauncherConfig::default().terminal);
    let (c, w) = settings(&d);
    assert_eq!((c, w.len()), (expected.clone(), 0));
    assert_eq!(expected.quick.wifi_more, Some(vec!["foot".to_string(), "-e".into(), "nmtui".into()]), "More… defaults to nmtui");
    std::fs::write(d.join("bar.toml"), "modules = [\"weather\"]").unwrap();
    let (c, w) = settings(&d);
    assert_eq!(c, expected, "defaults, More… resolved all the same");
    assert!(w.iter().any(|m| m.contains("bar.toml")), "{w:?}");
    // Quick Wi-Fi spec § 4: the launcher's terminal, read once at startup.
    std::fs::write(d.join("bar.toml"), "height = 22\n").unwrap();
    std::fs::write(d.join("launcher.toml"), "terminal = [\"alacritty\", \"-e\"]\n").unwrap();
    assert_eq!(settings(&d).0.quick.wifi_more, Some(vec!["alacritty".to_string(), "-e".into(), "nmtui".into()]));
    std::fs::write(d.join("bar.toml"), "[quick]\nwifi_more = []\n").unwrap();
    assert_eq!(settings(&d).0.quick.wifi_more, Some(vec![]), "given in bar.toml: kept");
    let _ = std::fs::remove_dir_all(&d);
}

/// Themes task 4: the startup theme goes through the shared resolver (load_settings,
/// effective_mode, resolve) and is described in one line.
#[test]
fn the_theme_is_read_resolved_and_described() {
    use alpheus_commun::theme::presets::preset;
    use alpheus_commun::theme::settings::ThemeSource;
    let d = std::env::temp_dir().join(format!("alpheus-theme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let (s, r, w) = theme(&d, None);
    assert_eq!((r, w.len()), (Roles::default(), 0), "nothing written: catppuccin, dark (no portal)");
    assert_eq!(describe(&s, Scheme::Dark), "catppuccin, dark (auto)");
    std::fs::write(d.join("colors.toml"), "bg = 3").unwrap();
    let (s, r, w) = theme(&d, None);
    assert_eq!(s.source, ThemeSource::Wallpaper, "colors.toml without theme.toml: the wallpaper, as before themes");
    assert_eq!(r, Roles::default(), "an invalid one: catppuccin");
    assert!(w.iter().any(|m| m.contains("colors.toml")), "{w:?}");
    std::fs::write(d.join("theme.toml"), "theme = \"gruvbox\"\nmode = \"auto\"\n").unwrap();
    let (s, r, _) = theme(&d, Some(Scheme::Light));
    assert_eq!(r, preset("gruvbox").unwrap().light.unwrap(), "auto follows the portal's word");
    assert_eq!(describe(&s, Scheme::Light), "gruvbox, light (auto)");
    std::fs::write(d.join("theme.toml"), "theme = \"wallpaper\"\nmode = \"dark\"\n").unwrap();
    assert_eq!(describe(&theme(&d, Some(Scheme::Light)).0, Scheme::Dark), "wallpaper, dark");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_socket_bind_failure_is_logged_and_the_bar_runs_without_a_launcher() {
    assert_eq!(launcher_socket_outcome(Ok::<_, &str>(5)), (Some(5), None));
    assert_eq!(
        launcher_socket_outcome(Err::<u8, _>("EACCES")),
        (None, Some("launcher socket: EACCES — the bar runs without a launcher".to_string()))
    );
}

#[test]
fn only_a_large_dmenu_request_releases_memory_once_consumed() {
    assert!(
        releases_memory_once_consumed(&Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }, RELEASE_AFTER),
        "reached the threshold"
    );
    assert!(
        !releases_memory_once_consumed(&Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }, RELEASE_AFTER - 1),
        "a small dmenu list: not worth a malloc_trim (it walks the whole heap)"
    );
    assert!(!releases_memory_once_consumed(&Request::Toggle, RELEASE_AFTER), "a tiny header: nothing to release");
    assert!(!releases_memory_once_consumed(&Request::Close, RELEASE_AFTER));
}

#[test]
fn max_connections_is_enforced() {
    assert!(!too_many_connections(MAX_CONNECTIONS - 1), "room for one more");
    assert!(too_many_connections(MAX_CONNECTIONS), "already at the limit: the next one is refused");
    assert!(too_many_connections(MAX_CONNECTIONS + 1));
}

/// The inflight count itself, not just the cap check above: tracked and
/// released in lockstep, whatever the reason a connection ends (review
/// item M3, final review slice 1).
#[test]
fn inflight_is_tracked_and_released_in_lockstep() {
    let inflight = Cell::new(0usize);
    for i in 1..=MAX_CONNECTIONS {
        assert!(!too_many_connections(inflight.get()), "room for the {i}th connection");
        track_connection(&inflight);
        assert_eq!(inflight.get(), i);
    }
    assert!(too_many_connections(inflight.get()), "at the cap: the next one is refused");
    untrack_connection(&inflight);
    assert_eq!(inflight.get(), MAX_CONNECTIONS - 1, "one released");
    assert!(!too_many_connections(inflight.get()), "room again");
    for _ in 0..MAX_CONNECTIONS {
        untrack_connection(&inflight);
    }
    assert_eq!(inflight.get(), 0, "never goes negative: saturating");
}

/// accept() keeps failing (EMFILE: the error stays pending on a
/// level-triggered source): the listener pauses instead of spinning,
/// and says so once, not once per pass.
#[test]
fn a_failing_accept_pauses_the_listener_and_logs_once() {
    let failing = Cell::new(false);
    let mut lines = Vec::new();
    for _ in 0..3 {
        let mut calls = 0;
        let action = drain_listener(
            || {
                calls += 1;
                Err::<Option<u8>, _>(io::Error::from_raw_os_error(libc::EMFILE))
            },
            |_| panic!("nothing was accepted"),
            &failing,
            &mut |m| lines.push(m.to_string()),
        );
        assert_eq!(action, PostAction::Disable, "a pending error must not spin the loop");
        assert_eq!(calls, 1, "no retry within the same pass");
    }
    assert_eq!(lines.len(), 1, "logged once while it keeps failing: {lines:?}");
    assert!(lines[0].contains("paused"), "{lines:?}");
    // It recovers: everything pending is taken, the failure is forgotten.
    let mut queue = vec![Ok(None), Ok(Some(2u8)), Ok(Some(1))];
    let mut taken = Vec::new();
    let action = drain_listener(|| queue.pop().unwrap(), |c| taken.push(c), &failing, &mut |m| lines.push(m.to_string()));
    assert_eq!((action, taken), (PostAction::Continue, vec![1, 2]));
    assert!(!failing.get());
    assert_eq!(lines.len(), 2, "one line when it accepts again: {lines:?}");
}

/// A hostile request that hits a bug in the decoder: the connection
/// fails, the bar does not.
#[test]
fn a_panic_while_reading_a_request_is_a_failed_request() {
    let progress = guarded_progress(|| panic!("decoder bug"));
    assert!(matches!(progress, Progress::Failed(ref m) if m == "internal error"), "{progress:?}");
    assert!(matches!(guarded_progress(|| Progress::Pending), Progress::Pending));
}

/// Mirrors `a_burst_of_messages_redraws_only_once`, for the launcher's
/// windows: a burst of several `Msg::Compositor` messages in the same
/// `dispatch()` pass must feed the launcher once, never once per
/// message; a pass with none at all must not feed it again on a stale
/// flag.
struct WindowsCounter {
    dirty: bool,
    feeds: u32,
}

impl WindowsFeed for WindowsCounter {
    fn windows_dirty_mut(&mut self) -> &mut bool {
        &mut self.dirty
    }
    fn feed_windows_now(&mut self) {
        self.feeds += 1;
    }
}

#[test]
fn a_burst_of_compositor_messages_feeds_the_launcher_once() {
    let mut c = WindowsCounter { dirty: false, feeds: 0 };
    for i in 0..9 {
        let m = if i % 2 == 0 { Msg::Compositor(Some(example())) } else { Msg::Clock(BEFORE) };
        note_compositor_message(&mut c, &m);
    }
    after_dispatch_windows(&mut c);
    assert_eq!(c.feeds, 1, "a burst of several compositor messages: one re-rank, not N");
    assert!(!c.dirty, "the flag drops back down after the feed");
    // No compositor message in this pass: the stale flag from the
    // previous pass must not still be set, and nothing is fed again.
    note_compositor_message(&mut c, &Msg::Clock(BEFORE));
    after_dispatch_windows(&mut c);
    assert_eq!(c.feeds, 1, "nothing compositor-related this pass: no re-rank");
}

#[test]
fn the_sound_source_publishes_for_the_osd_only_with_its_module() {
    assert_eq!(sound_scope(&[ModuleId::Volume, ModuleId::Osd]), Scope::Osd);
    assert_eq!(sound_scope(&[ModuleId::Osd]), Scope::Osd);
    assert_eq!(sound_scope(&[ModuleId::Volume, ModuleId::Clock]), Scope::Bar, "bar-only: the sink's volume alone");
}

#[test]
fn the_osd_alone_starts_the_sound_source() {
    assert!(wanted(&[ModuleId::Osd]).sound, "the OSD reads pw-dump even without the volume module");
    assert!(wanted(&[ModuleId::Volume]).sound);
    assert!(!wanted(&[ModuleId::Workspaces, ModuleId::Clock]).sound);
}

/// Quick spec § 5: the panel's volume and microphone rows read pw-dump, even without the volume
/// module; the sink list is the panel's to ask for while it is open, never the base scope.
#[test]
fn the_quick_panel_reads_the_sound() {
    assert!(wanted(&[ModuleId::Quick]).sound);
    assert_eq!(sound_scope(&[ModuleId::Quick]), Scope::Bar);
    assert!(!wanted(&[ModuleId::Clock]).sound, "nothing else asks for it");
}
