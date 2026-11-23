//! The bar's pure state: what the sources have said, and nothing else.
//!
//! `Msg` is everything a source can say (a compositor's workspaces, a sound reading, a D-Bus
//! domain's state, a timer's chip, the notifications' view) and `Bar::apply` is the one place a
//! message changes the state. It answers whether anything the bar draws changed, so the loop
//! repaints only then; `layout` derives each screen's grid from this state, and `segments` the
//! cells of one module. Nothing here reads a clock, a file or the system: the wiring hands in what
//! it has read.
//!
//! Never:
//! - keep what no module draws: a list the quick panel reads lives in the panel while it is open
//!   (`the_bar_never_keeps_a_sink_list`, in this file's own tests), so the bar at rest stays small;
//! - report a change that is not one: `set_if_changed` is how a message that says the same thing
//!   again costs no repaint (`set_if_changed_stores_and_reports_only_a_change`);
//! - read a source that is down as a state of its own: the module keeps its last reading and shows
//!   its icon with `–` (docs/decisions.md, "A source that is down is not a state").
//!
//! See: docs/ARCHITECTURE.md ("The core"), docs/tour.md (one message from the source to the
//! pixels), docs/features/bar.md ("How it is built").

use crate::clock::LocalTime;
use crate::compositor::CompositorState;
use crate::config::{BarConfig, ModuleId};
use crate::media::MediaView;
use crate::notify::NotifyView;
use crate::privacy::Privacy;
use crate::session_bus::icons::TrayIcon;
use crate::system::{BtState, BtView, NetState, PowerState, ProfileState, Resources, Sample, SoundState, VpnState, WifiState};
use crate::timer::TimerChip;
use alpheus_common::theme::settings::{BarStyle, ModuleColors};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed<T> {
    /// Nothing received yet: "…".
    Waiting,
    /// Source down: "–".
    Down,
    Ready(T),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// None: the compositor source is down.
    Compositor(Option<CompositorState>),
    Clock(LocalTime),
    /// None: that source is down ("–").
    Sound(Option<SoundState>),
    Network(Option<NetState>),
    Bluetooth(Option<BtState>),
    Battery(Option<PowerState>),
    /// power-profiles-daemon (quick spec § 5); None: the system bus is down.
    Profile(Option<ProfileState>),
    /// The quick panel's Wi-Fi (quick Wi-Fi spec § 5), only while the panel is open: moved into it
    /// (`quick_take`), never kept by the bar. `state` None: the panel closed, or the system bus is
    /// down. `open`: the panel's open it was read for (`WifiTap::snapshot`); None, the bus down: any.
    Wifi {
        open: Option<u64>,
        state: Option<WifiState>,
    },
    /// The quick panel's Bluetooth (quick settings delivery 3), only while it is open: the panel's
    /// (`quick/app.rs` takes it), never kept by the bar. `state` None: the panel closed, BlueZ
    /// gone or the system bus down. `open`: as `Wifi`'s (`BtTap::snapshot`).
    Bt {
        open: Option<u64>,
        state: Option<BtView>,
    },
    /// The quick panel's VPN profiles and active VPN connections (quick VPN spec § 5), only while
    /// it is open: the panel's, never kept by the bar. `state` None: the panel closed or the system
    /// bus down. `open`: as `Wifi`'s (`VpnTap::snapshot`).
    Vpn {
        open: Option<u64>,
        state: Option<VpnState>,
    },
    /// systemd's count of failed units on the system bus (diagnostics spec § 6); None: not read,
    /// nobody owns systemd1, its read failed, or the system bus is down.
    Units(Option<u32>),
    /// The failed-units count was just read, whatever it said (sent outside the published state: a
    /// second read of the same count changes nothing the bar draws). No redraw: it asks the user
    /// manager's count again (the user's own link, `failed/app.rs`).
    UnitsRead,
    /// The user manager's count of failed units, from the session bus (the system's is `Units`);
    /// None: not read, no user manager, or the session bus is down.
    UserUnits(Option<u32>),
    Resources(Sample),
    /// The tray's shown items, rebuilt whole by the wiring.
    Tray(Vec<TrayIcon>),
    /// What the media module shows; None: no player, or every player stopped (media spec § 1).
    Media(Option<MediaView>),
    /// What the bell shows (notifications spec § 4): the unread count, do-not-disturb, whether the
    /// name is ours. Sent by the wiring from the first word of the server on.
    Notifications(NotifyView),
    /// What the timer's chip shows (timer spec § 2); None: hidden.
    Timer(Option<TimerChip>),
}

#[derive(Debug, Clone)]
pub struct Bar {
    pub config: BarConfig,
    pub compositor: Feed<CompositorState>,
    pub clock: Option<LocalTime>,
    pub sound: Feed<SoundState>,
    pub network: Feed<NetState>,
    pub bluetooth: Feed<BtState>,
    pub battery: Feed<PowerState>,
    /// The power profile, for the quick panel; nothing in the bar draws it.
    pub profile: Feed<ProfileState>,
    pub resources: Resources,
    /// The failed-units counts (the system manager's and the user manager's), drawn by the `failed` module.
    pub failed: Failed,
    /// The tray's icons, in registration order; empty at start.
    pub tray: Vec<TrayIcon>,
    /// The media module's mark and title; None until a player plays or pauses, and again when none does.
    pub media: Option<MediaView>,
    /// The bell; None until the notification server says anything (no bell drawn).
    pub notifications: Option<NotifyView>,
    /// The timer's chip; None until the wiring sends one (and when `[timer] idle = "hidden"` at rest).
    pub timer: Option<TimerChip>,
    /// The privacy indicators (privacy spec §§ 3 and 4), fed by `privacy_at`; empty without the module.
    pub privacy: Privacy,
    /// How much of each module takes its hue (themes spec § 9): the theme's setting, written by the wiring
    /// at start and on every theme reload. `None` until then, the look of delivery 1.
    pub module_colors: ModuleColors,
    /// The bar's shape (shapes spec § 2): its block spacing here, its pixels in the painter. Written
    /// by the wiring like `module_colors`; `Flat` until then.
    pub style: BarStyle,
    /// Shield mode's view (shield spec); None without `[shield]`. Set by `ShieldApp`, never by a message.
    pub shield: Option<crate::shield::ShieldView>,
}

/// The failed units of each manager: None when that count is unknown (a source that is down is not
/// a state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Failed {
    /// The system manager's, from the system bus.
    pub system: Option<u32>,
    /// The user manager's, from the session bus.
    pub user: Option<u32>,
}

/// Puts `v` in `slot`; true if it changed.
fn set_if_changed<T: PartialEq>(slot: &mut T, v: T) -> bool {
    if *slot == v {
        false
    } else {
        *slot = v;
        true
    }
}

/// Puts `v` in `slot` (None: down); true if it changed.
fn update<T: PartialEq>(slot: &mut Feed<T>, v: Option<T>) -> bool {
    set_if_changed(
        slot,
        match v {
            Some(x) => Feed::Ready(x),
            None => Feed::Down,
        },
    )
}

/// What the bar draws of the sound: the sink's volume, or the source's state ("…", "–"); None
/// without the volume module (the OSD alone keeps pw-dump running): nothing drawn, nothing to
/// compare.
fn sound_drawing(f: &Feed<SoundState>, drawn: bool) -> Option<Feed<Option<crate::system::Volume>>> {
    if !drawn {
        return None;
    }
    Some(match f {
        Feed::Waiting => Feed::Waiting,
        Feed::Down => Feed::Down,
        Feed::Ready(s) => Feed::Ready(s.sink),
    })
}

/// Same bar drawing from the compositor: its workspaces and focused window.
/// The launcher's `toplevels` are not drawn: a background title change
/// (niri publishes every one) is stored without a redraw.
fn same_drawing(a: &Feed<CompositorState>, b: &Feed<CompositorState>) -> bool {
    match (a, b) {
        (Feed::Ready(a), Feed::Ready(b)) => a.workspaces == b.workspaces && a.window == b.window,
        (Feed::Waiting, Feed::Waiting) | (Feed::Down, Feed::Down) => true,
        _ => false,
    }
}

impl Bar {
    pub fn new(config: BarConfig) -> Bar {
        let privacy = Privacy::new(config.privacy.ignore.clone());
        Bar {
            config,
            privacy,
            compositor: Feed::Waiting,
            clock: None,
            sound: Feed::Waiting,
            network: Feed::Waiting,
            bluetooth: Feed::Waiting,
            battery: Feed::Waiting,
            profile: Feed::Waiting,
            resources: Resources::default(),
            failed: Failed::default(),
            tray: Vec::new(),
            media: None,
            notifications: None,
            timer: None,
            module_colors: ModuleColors::None,
            style: BarStyle::Flat,
            shield: None,
        }
    }

    /// Feeds the privacy indicators from a sound or compositor message, at `now` (privacy spec
    /// §§ 3 and 4); called before `apply`. True when what they show changed. Nothing is read or
    /// kept without the `privacy` module. A compositor that is down counts as not casting.
    pub fn privacy_at(&mut self, m: &Msg, now: Instant) -> bool {
        if !self.config.modules.contains(&ModuleId::Privacy) {
            return false;
        }
        match m {
            Msg::Sound(None) => self.privacy.set_sound(None, false, now),
            Msg::Sound(Some(s)) => self.privacy.set_sound(s.capture.as_ref(), s.source.map(|v| v.muted).unwrap_or(false), now),
            Msg::Compositor(Some(c)) => self.privacy.set_casting(c.casting, c.cast_hold, now),
            Msg::Compositor(None) => self.privacy.set_casting(false, Duration::ZERO, now),
            _ => false,
        }
    }

    /// Applies a message; true if the display may have changed.
    // one row per message: the ones that change nothing stay spelled out, each with its reason.
    #[allow(clippy::match_same_arms)]
    pub fn apply(&mut self, m: Msg) -> bool {
        match m {
            Msg::Compositor(e) => {
                let new = match e {
                    Some(s) => Feed::Ready(s),
                    None => Feed::Down,
                };
                let redraw = !same_drawing(&self.compositor, &new);
                // Always kept: an open launcher reads the toplevels from here.
                self.compositor = new;
                redraw
            }
            Msg::Clock(h) => set_if_changed(&mut self.clock, Some(h)),
            Msg::Sound(s) => {
                // The sink list is the open quick panel's (`quick_take` moved it there): never kept here.
                let s = s.map(|s| SoundState { sinks: None, ..s });
                // The bar draws the sink only: a microphone or label change is kept for the OSD
                // (which reads it from this same message, before) without a redraw.
                let drawn = self.config.modules.contains(&ModuleId::Volume);
                let before = sound_drawing(&self.sound, drawn);
                update(&mut self.sound, s);
                before != sound_drawing(&self.sound, drawn)
            }
            Msg::Network(s) => update(&mut self.network, s),
            Msg::Bluetooth(s) => update(&mut self.bluetooth, s),
            Msg::Battery(s) => update(&mut self.battery, s),
            // Kept for the quick panel (`quick/app.rs` repaints it): no bar redraw.
            Msg::Profile(p) => {
                update(&mut self.profile, p);
                false
            }
            // The open quick panel's (`quick/app.rs` takes it first): never kept, no bar redraw.
            Msg::Wifi { .. } | Msg::Bt { .. } | Msg::Vpn { .. } => false,
            // A count going unknown keeps the last known one (spec § 6): a transient outage neither
            // hides the module nor re-arms the notice.
            Msg::Units(n) => n.is_some() && set_if_changed(&mut self.failed.system, n),
            Msg::UnitsRead => false,
            Msg::UserUnits(n) => n.is_some() && set_if_changed(&mut self.failed.user, n),
            Msg::Resources(s) => self.resources.push(s),
            Msg::Tray(icons) => set_if_changed(&mut self.tray, icons),
            Msg::Media(view) => set_if_changed(&mut self.media, view),
            Msg::Notifications(view) => set_if_changed(&mut self.notifications, Some(view)),
            Msg::Timer(chip) => set_if_changed(&mut self.timer, chip),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::example;
    use crate::system::*;
    use alpheus_common::backends::Volume;

    /// Diagnostics spec § 6: the system manager's count is kept (and says when it changed); the word that it
    /// was read again is no state and no redraw.
    #[test]
    fn the_failed_count_is_kept_and_its_read_word_changes_nothing() {
        let mut b = Bar::new(BarConfig::default());
        assert_eq!(b.failed, Failed::default());
        assert!(b.apply(Msg::Units(Some(2))));
        assert_eq!(b.failed, Failed { system: Some(2), user: None });
        assert!(!b.apply(Msg::Units(Some(2))), "the same count: no redraw");
        assert!(!b.apply(Msg::UnitsRead), "a read is no change");
        assert!(b.apply(Msg::UserUnits(Some(1))));
        assert_eq!(b.failed, Failed { system: Some(2), user: Some(1) });
        assert!(!b.apply(Msg::UserUnits(Some(1))), "the same count: no redraw");
        // A count going unknown keeps the last known one (no flicker, a source down is no state).
        assert!(!b.apply(Msg::UserUnits(None)));
        assert_eq!(b.failed, Failed { system: Some(2), user: Some(1) });
        assert!(!b.apply(Msg::Units(None)));
        assert_eq!(b.failed, Failed { system: Some(2), user: Some(1) });
        assert!(b.apply(Msg::Units(Some(0))), "known again: the new value");
        let mut fresh = Bar::new(BarConfig::default());
        assert!(!fresh.apply(Msg::Units(None)) && !fresh.apply(Msg::UserUnits(None)), "unknown from the start: nothing");
        assert_eq!(fresh.failed, Failed::default());
    }

    const H: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };

    /// Quick spec § 5: the list belongs to the open panel (`quick_take` moves it there first); the bar
    /// never keeps one, whatever reaches it.
    #[test]
    fn the_bar_never_keeps_a_sink_list() {
        let mut b = Bar::new(BarConfig::default());
        let sinks = vec![Sink { id: 99, label: "Speaker".into(), default: true }];
        let s = SoundState { sink: Some(Volume { percent: 84, muted: false }), sinks: Some(sinks), ..SoundState::default() };
        assert!(b.apply(Msg::Sound(Some(s))));
        assert!(matches!(&b.sound, Feed::Ready(s) if s.sinks.is_none()), "{:?}", b.sound);
    }

    // ---- privacy (privacy spec §§ 3, 4 and 7) ----

    fn holding_the_mic() -> SoundState {
        let mic = crate::privacy::Indicator { names: vec!["firefox".into()], unnamed: false };
        SoundState { capture: Some(crate::privacy::Capture { mic, ..Default::default() }), ..SoundState::default() }
    }

    #[test]
    fn privacy_at_feeds_the_bar_only_with_the_module() {
        let now = Instant::now();
        let mut b = Bar::new(BarConfig::default());
        assert!(b.config.modules.contains(&ModuleId::Privacy));
        assert!(b.privacy_at(&Msg::Sound(Some(holding_the_mic())), now), "the microphone lights");
        assert!(b.privacy.shown().mic.active());
        assert!(!b.privacy_at(&Msg::Sound(Some(holding_the_mic())), now), "same capture: nothing changed");
        let mut muted = holding_the_mic();
        muted.source = Some(Volume { percent: 40, muted: true });
        assert!(b.privacy_at(&Msg::Sound(Some(muted)), now), "the mute flip changes what is drawn");
        assert!(b.privacy.mic_muted());
        assert!(b.privacy_at(&Msg::Sound(None), now), "the source down: no capture");
        assert!(!b.privacy.mic_muted());
        // The off-delay keeps it shown for now; it goes once due.
        assert!(b.privacy.deadline().is_some());

        let mut config = BarConfig::default();
        config.modules.retain(|m| *m != ModuleId::Privacy);
        let mut off = Bar::new(config);
        assert!(!off.privacy_at(&Msg::Sound(Some(holding_the_mic())), now));
        assert!(!off.privacy.shown().mic.active() && off.privacy.deadline().is_none(), "nothing kept");
    }

    #[test]
    fn privacy_at_reads_the_compositor_casting_even_when_the_bar_draws_the_same() {
        let now = Instant::now();
        let mut b = Bar::new(BarConfig::default());
        let mut e = example();
        assert!(!b.privacy_at(&Msg::Compositor(Some(e.clone())), now), "not casting");
        e.casting = true;
        assert!(b.privacy_at(&Msg::Compositor(Some(e.clone())), now), "the screen lights");
        assert!(b.privacy.shown().screen.active());
        e.casting = false;
        e.cast_hold = Duration::ZERO;
        b.privacy_at(&Msg::Compositor(Some(e)), now);
        assert!(b.privacy.shown().screen.active(), "the off-delay still applies");
        b.privacy_at(&Msg::Compositor(None), now);
        assert!(b.privacy.deadline().is_some());
        assert!(b.privacy.due(now + crate::privacy::OFF_DELAY));
        assert!(!b.privacy.shown().screen.active());
    }

    #[test]
    fn privacy_at_ignores_every_other_message() {
        let mut b = Bar::new(BarConfig::default());
        assert!(!b.privacy_at(&Msg::Clock(H), Instant::now()));
        assert!(!b.privacy_at(&Msg::Network(None), Instant::now()));
        assert!(!b.privacy.shown().mic.active());
    }

    #[test]
    fn set_if_changed_stores_and_reports_only_a_change() {
        let mut slot = 1;
        assert!(!set_if_changed(&mut slot, 1), "same value: nothing changed");
        assert!(set_if_changed(&mut slot, 2));
        assert_eq!(slot, 2, "the new value is kept");
        assert!(!set_if_changed(&mut slot, 2));
    }

    #[test]
    fn a_message_changes_the_state_and_says_whether_the_bar_must_repaint() {
        let mut b = Bar::new(BarConfig::default());
        assert_eq!(b.compositor, Feed::Waiting);
        assert!(b.apply(Msg::Compositor(Some(example()))));
        assert!(!b.apply(Msg::Compositor(Some(example()))), "same state: nothing to redraw");
        assert!(b.apply(Msg::Compositor(None)));
        assert_eq!(b.compositor, Feed::Down);
        assert!(!b.apply(Msg::Compositor(None)));
        assert!(b.apply(Msg::Clock(H)));
        assert!(!b.apply(Msg::Clock(H)));
        assert_eq!(b.clock, Some(H));
    }

    #[test]
    fn a_toplevels_only_change_is_stored_without_a_redraw() {
        let mut b = Bar::new(BarConfig::default());
        assert!(b.apply(Msg::Compositor(Some(example()))));
        let mut e = example();
        e.toplevels[0].title = "vim notes.md".into();
        assert!(!b.apply(Msg::Compositor(Some(e.clone()))), "a background title: the bar draws the same");
        assert_eq!(b.compositor, Feed::Ready(e.clone()), "but the launcher's list is up to date");
        e.toplevels.pop();
        assert!(!b.apply(Msg::Compositor(Some(e.clone()))), "a window closed elsewhere: same drawing");
        e.window = None;
        assert!(b.apply(Msg::Compositor(Some(e.clone()))), "the focused window drawn: redraw");
        e.workspaces[0].windows = 3;
        assert!(b.apply(Msg::Compositor(Some(e))), "workspaces drawn: redraw");
    }

    #[test]
    fn system_messages_and_changes() {
        let mut b = Bar::new(BarConfig::default());
        assert_eq!((&b.sound, &b.network, &b.bluetooth, &b.battery), (&Feed::Waiting, &Feed::Waiting, &Feed::Waiting, &Feed::Waiting));
        let s = SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() };
        assert!(b.apply(Msg::Sound(Some(s.clone()))));
        assert!(!b.apply(Msg::Sound(Some(s))), "same volume: nothing to redraw");
        assert!(b.apply(Msg::Sound(None)));
        assert_eq!(b.sound, Feed::Down);
        assert!(b.apply(Msg::Network(Some(NetState::Offline))));
        assert!(!b.apply(Msg::Network(Some(NetState::Offline))));
        assert!(b.apply(Msg::Bluetooth(Some(BtState::Off))));
        assert_eq!(b.bluetooth, Feed::Ready(BtState::Off));
        assert!(b.apply(Msg::Battery(Some(PowerState::default()))));
        assert!(!b.apply(Msg::Battery(Some(PowerState::default()))));
        assert!(b.apply(Msg::Battery(None)));
        assert_eq!(b.battery, Feed::Down);
    }

    /// Quick spec § 5: the profile is kept for the panel; nothing in the bar draws it.
    #[test]
    fn a_profile_change_is_kept_and_redraws_no_bar() {
        let mut b = Bar::new(BarConfig::default());
        let p = ProfileState { active: Some(Profile::Balanced), offered: Profile::ALL.to_vec() };
        assert!(!b.apply(Msg::Profile(Some(p.clone()))));
        assert_eq!(b.profile, Feed::Ready(p));
        assert!(!b.apply(Msg::Profile(None)));
        assert_eq!(b.profile, Feed::Down);
    }

    /// Quick Wi-Fi spec § 5: the Wi-Fi state is the open panel's (quick_take moves it there first);
    /// the bar keeps nothing and redraws nothing for it.
    #[test]
    fn a_wifi_state_is_never_kept_nor_drawn_by_the_bar() {
        let mut b = Bar::new(BarConfig::default());
        assert!(!b.apply(Msg::Wifi { open: Some(1), state: Some(WifiState { enabled: true, device: None, stale: false }) }));
        assert!(!b.apply(Msg::Wifi { open: None, state: None }));
    }

    /// Quick settings delivery 3: the Bluetooth view is the open panel's, as the Wi-Fi state is.
    #[test]
    fn a_bt_view_is_never_kept_nor_drawn_by_the_bar() {
        let mut b = Bar::new(BarConfig::default());
        assert!(!b.apply(Msg::Bt { open: Some(1), state: Some(BtView::default()) }));
        assert!(!b.apply(Msg::Bt { open: None, state: None }));
    }

    /// Quick VPN: the VPN reading is the open panel's too.
    #[test]
    fn a_vpn_state_is_never_kept_nor_drawn_by_the_bar() {
        let mut b = Bar::new(BarConfig::default());
        assert!(!b.apply(Msg::Vpn { open: Some(1), state: Some(VpnState::default()) }));
        assert!(!b.apply(Msg::Vpn { open: None, state: None }));
    }

    /// The bar draws the sink only. A microphone or label change is kept (the OSD reads
    /// it from the same message) but redraws nothing.
    #[test]
    fn a_microphone_or_label_change_alone_does_not_redraw_the_bar() {
        let mut b = Bar::new(BarConfig::default());
        let s = SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() };
        assert!(b.apply(Msg::Sound(Some(s.clone()))));
        let mic = SoundState { source: Some(Volume { percent: 100, muted: true }), ..s.clone() };
        assert!(!b.apply(Msg::Sound(Some(mic.clone()))), "the bar draws the sink only");
        assert_eq!(b.sound, Feed::Ready(mic), "kept all the same");
        let named = |node: &str| Some(DeviceName { node: node.into(), label: node.into() });
        let switched = SoundState { sink_name: named("speaker"), ..s.clone() };
        assert!(!b.apply(Msg::Sound(Some(switched.clone()))), "another sink at the same volume: the bar draws the same");
        assert_eq!(b.sound, Feed::Ready(switched), "kept for the OSD's name card");
        let louder = SoundState { sink: Some(Volume { percent: 85, muted: false }), ..s };
        assert!(b.apply(Msg::Sound(Some(louder))), "the sink moved: redraw");
        assert!(b.apply(Msg::Sound(None)), "down: redraw");
    }

    /// With the OSD on and the volume module off, the bar draws no sound at all, so no
    /// sound message re-lays it out (the OSD alone keeps pw-dump running).
    #[test]
    fn without_the_volume_module_a_sound_change_never_redraws_the_bar() {
        let mut config = BarConfig::default();
        config.modules.retain(|m| *m != ModuleId::Volume);
        assert!(config.modules.contains(&ModuleId::Osd));
        let mut b = Bar::new(config);
        let s = SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() };
        assert!(!b.apply(Msg::Sound(Some(s.clone()))), "the first state");
        let louder = SoundState { sink: Some(Volume { percent: 90, muted: true }), ..s };
        assert!(!b.apply(Msg::Sound(Some(louder.clone()))), "the sink moved");
        assert_eq!(b.sound, Feed::Ready(louder), "kept all the same");
        assert!(!b.apply(Msg::Sound(None)), "down");
    }

    #[test]
    fn a_repeated_sample_is_not_a_change() {
        let mut r = Resources::default();
        let s = Sample { cpu: Some(12), memory: Some(61), temperature: Some(45), ..Sample::default() };
        assert!(r.push(s), "the first reading");
        assert!(!r.push(s), "the same reading again: nothing to redraw");
        assert!(r.push(Sample { cpu: Some(13), ..s }), "the CPU moved");
        let mut b = Bar::new(BarConfig::default());
        assert!(b.apply(Msg::Resources(s)));
        assert!(!b.apply(Msg::Resources(s)));
        assert_eq!(b.resources.history.len(), 2, "both stored, though the second changed nothing");
    }

    #[test]
    fn media_message_redraws_only_on_change() {
        use crate::media::MediaView;
        let song = |playing| Some(MediaView { playing, title: "Bohemian Rhapsody".into() });
        let mut b = Bar::new(BarConfig::default());
        assert_eq!(b.media, None, "nothing at start");
        assert!(!b.apply(Msg::Media(None)), "still nothing: no redraw");
        assert!(b.apply(Msg::Media(song(true))));
        assert!(!b.apply(Msg::Media(song(true))), "the same mark and title: no redraw");
        assert!(b.apply(Msg::Media(song(false))), "paused: the mark changes");
        assert_eq!(b.media, song(false));
        assert!(b.apply(Msg::Media(None)), "the last player left");
        assert_eq!(b.media, None);
    }

    #[test]
    fn a_timer_message_redraws_only_on_change() {
        use crate::timer::{ChipLook, DONE, TIMER, TimerChip};
        let chip = |text: Option<&str>| Some(TimerChip { glyph: TIMER, text: text.map(str::to_string), look: ChipLook::Running });
        let mut b = Bar::new(BarConfig::default());
        assert_eq!(b.timer, None, "nothing at start");
        assert!(!b.apply(Msg::Timer(None)), "still nothing: no redraw");
        assert!(b.apply(Msg::Timer(chip(Some("25m")))));
        assert!(!b.apply(Msg::Timer(chip(Some("25m")))), "the same chip: no redraw");
        assert!(b.apply(Msg::Timer(chip(Some("24m")))), "a minute later");
        assert_eq!(b.timer, chip(Some("24m")));
        let done = Some(TimerChip { glyph: DONE, text: Some("done".into()), look: ChipLook::Done });
        assert!(b.apply(Msg::Timer(done)));
        assert!(b.apply(Msg::Timer(None)), "hidden");
        assert_eq!(b.timer, None);
    }

    #[test]
    fn notifications_message_redraws_only_on_change() {
        use crate::notify::NotifyView;
        let v = |unread| NotifyView { unread, dnd: false, serving: true };
        let mut b = Bar::new(BarConfig::default());
        assert_eq!(b.notifications, None, "nothing heard at start: no bell");
        assert!(b.apply(Msg::Notifications(v(0))), "the first word shows the bell");
        assert!(!b.apply(Msg::Notifications(v(0))), "the same view: no redraw");
        assert!(b.apply(Msg::Notifications(v(2))));
        assert!(b.apply(Msg::Notifications(NotifyView { dnd: true, ..v(2) })));
        assert_eq!(b.notifications, Some(NotifyView { unread: 2, dnd: true, serving: true }));
    }

    #[test]
    fn tray_message_redraws_only_on_change() {
        use crate::session_bus::icons::{Icon, TrayIcon};
        use crate::session_bus::{ItemKey, Status};
        let icon = |c: char, status| TrayIcon {
            key: ItemKey { owner: ":1.7".into(), path: "/StatusNotifierItem".into() },
            icon: Icon::Letter(c),
            status,
        };
        let mut b = Bar::new(BarConfig::default());
        assert!(b.tray.is_empty(), "empty at start");
        assert!(!b.apply(Msg::Tray(vec![])), "still empty: nothing to redraw");
        assert!(b.apply(Msg::Tray(vec![icon('N', Status::Active)])));
        assert!(!b.apply(Msg::Tray(vec![icon('N', Status::Active)])), "same icons: nothing to redraw");
        assert!(b.apply(Msg::Tray(vec![icon('N', Status::NeedsAttention)])), "a status change redraws");
        assert_eq!(b.tray, vec![icon('N', Status::NeedsAttention)]);
        assert!(b.apply(Msg::Tray(vec![])));
        assert!(b.tray.is_empty());
    }
}
