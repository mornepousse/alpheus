//! The bar's pure state: what the sources have said, nothing else.
//! Messages change it; `layout` derives the grid for each screen from it.

use crate::clock::LocalTime;
use crate::compositor::CompositorState;
use crate::config::{BarConfig, ModuleId};
use crate::media::MediaView;
use crate::notify::NotifyView;
use crate::system::{BtState, NetState, PowerState, Resources, Sample, SoundState};
use crate::tray::icons::TrayIcon;
use alpheus_commun::theme::settings::{BarStyle, ModuleColors};

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
    Resources(Sample),
    /// The tray's shown items, rebuilt whole by the wiring (task 11).
    Tray(Vec<TrayIcon>),
    /// What the media module shows; None: no player, or every player stopped (media spec § 1).
    Media(Option<MediaView>),
    /// What the bell shows (notifications spec § 4): the unread count, do-not-disturb, whether the
    /// name is ours. Sent by the wiring from the first word of the server on.
    Notifications(NotifyView),
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
    pub resources: Resources,
    /// The tray's icons, in registration order; empty at start.
    pub tray: Vec<TrayIcon>,
    /// The media module's mark and title; None until a player plays or pauses, and again when none does.
    pub media: Option<MediaView>,
    /// The bell; None until the notification server says anything (no bell drawn).
    pub notifications: Option<NotifyView>,
    /// How much of each module takes its hue (themes spec § 9): the theme's setting, written by the wiring
    /// at start and on every theme reload. `None` until then, the look of delivery 1.
    pub module_colors: ModuleColors,
    /// The bar's shape (shapes spec § 2): its block spacing here, its pixels in the painter. Written
    /// by the wiring like `module_colors`; `Flat` until then.
    pub style: BarStyle,
}

/// Puts `v` in `slot` (None: down); true if it changed.
fn update<T: PartialEq>(slot: &mut Feed<T>, v: Option<T>) -> bool {
    let n = match v {
        Some(x) => Feed::Ready(x),
        None => Feed::Down,
    };
    if *slot == n {
        false
    } else {
        *slot = n;
        true
    }
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
        Bar {
            config,
            compositor: Feed::Waiting,
            clock: None,
            sound: Feed::Waiting,
            network: Feed::Waiting,
            bluetooth: Feed::Waiting,
            battery: Feed::Waiting,
            resources: Resources::default(),
            tray: Vec::new(),
            media: None,
            notifications: None,
            module_colors: ModuleColors::None,
            style: BarStyle::Flat,
        }
    }

    /// Applies a message; true if the display may have changed.
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
            Msg::Clock(h) => {
                if self.clock == Some(h) {
                    false
                } else {
                    self.clock = Some(h);
                    true
                }
            }
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
            Msg::Resources(s) => self.resources.push(s),
            Msg::Tray(icons) => {
                if self.tray == icons {
                    false
                } else {
                    self.tray = icons;
                    true
                }
            }
            Msg::Media(view) => {
                if self.media == view {
                    false
                } else {
                    self.media = view;
                    true
                }
            }
            Msg::Notifications(view) => {
                if self.notifications == Some(view) {
                    false
                } else {
                    self.notifications = Some(view);
                    true
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::example;
    use crate::system::*;
    use alpheus_commun::backends::Volume;

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

    #[test]
    fn messages_and_changes() {
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

    /// OSD task 1: the bar draws the sink only. A microphone or label change is kept (the OSD reads
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

    /// OSD task 3: with the OSD on and the volume module off, the bar draws no sound at all, so no
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
    fn resources_history_bounded_to_60_points() {
        let mut b = Bar::new(BarConfig::default());
        for i in 0..100u8 {
            assert!(b.apply(Msg::Resources(Sample { cpu: Some(i), memory: Some(61), temperature: Some(60) })));
        }
        assert_eq!(b.resources.history.len(), HISTORY);
        assert_eq!((b.resources.history.front(), b.resources.history.back()), (Some(&40), Some(&99)));
        // First reading after startup: no CPU yet (it takes two readings).
        let mut r = Resources::default();
        let s = Sample { cpu: None, memory: Some(61), temperature: None };
        assert!(r.push(s));
        assert!(!r.push(s), "same reading, no CPU: nothing to redraw");
        assert!(r.history.is_empty());
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
        use crate::tray::icons::{Icon, TrayIcon};
        use crate::tray::{ItemKey, Status};
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
