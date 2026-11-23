//! The system tray (spec docs/specs/2026-10-05-alpheus-tray-design.md):
//! StatusNotifierItem watcher and host, dbusmenu menus, on a second
//! dbus-rs connection, to the session bus, driven by the bar's own calloop
//! loop (no thread). This file is the tray's vocabulary; it knows no D-Bus
//! library.
//!
//! Never:
//! - add a thread: the tray's connection is driven by the bar's own loop
//!   (`the_tray_spawns_no_thread`, and `nothing_resident_while_the_launcher_is_closed` counts what
//!   it does build);
//! - call anything outside the tray's allowlist (`tray_d_bus_is_confined_and_allowlisted`), and
//!   never ask an item for a picture.
//!
//! See: docs/features/tray.md, docs/ARCHITECTURE.md ("Sources: what feeds the loop", "Threads").

pub mod bus;
pub mod codec;
pub mod host;
pub mod hover;
pub mod icons;
pub mod menu;
pub mod menu_session;
pub mod mpris;
pub mod notifications;
pub mod router;
pub mod watcher;

pub const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
pub const WATCHER_INTERFACE: &str = "org.kde.StatusNotifierWatcher";
pub const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
/// The desktop portal (xdg-desktop-portal), its Settings interface and object.
pub const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const SETTINGS_INTERFACE: &str = "org.freedesktop.portal.Settings";
/// The one setting read: 0 no preference, 1 dark, 2 light.
pub const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
pub const COLOR_SCHEME_KEY: &str = "color-scheme";
pub const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";
/// The properties interface: `Get`, `GetAll`, `PropertiesChanged`.
pub const PROPERTIES_INTERFACE: &str = crate::sources::dbus::PROPERTIES;
/// MPRIS (media spec § 3): every player's object, its root and player interfaces.
pub const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
pub const MPRIS_INTERFACE: &str = "org.mpris.MediaPlayer2";
pub const MPRIS_PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
/// Players arriving, leaving and restarting: the bus driver's NameOwnerChanged in the MPRIS namespace.
pub const MPRIS_OWNER_RULE: &str = "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'";
/// Every player's Player properties, on its MPRIS object.
pub const MPRIS_PROPERTIES_RULE: &str = "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/mpris/MediaPlayer2',arg0='org.mpris.MediaPlayer2.Player'";
/// Seeked, added only while the media popup is open.
pub const MPRIS_SEEKED_RULE: &str =
    "type='signal',interface='org.mpris.MediaPlayer2.Player',member='Seeked',path='/org/mpris/MediaPlayer2'";
/// The user's systemd manager (diagnostics spec § 6): its name, object and Manager interface on the session
/// bus; only `NFailedUnits` is read and only `Subscribe` is called.
pub const SYSTEMD_NAME: &str = "org.freedesktop.systemd1";
pub const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
pub const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";
/// The user manager arriving, leaving and restarting (`daemon-reexec` included): the bus driver's NameOwnerChanged for its name.
pub const SYSTEMD_OWNER_RULE: &str = "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.systemd1'";
/// Its Manager properties, which carry the count (and only while a client has subscribed).
pub const SYSTEMD_PROPERTIES_RULE: &str = "type='signal',sender='org.freedesktop.systemd1',path='/org/freedesktop/systemd1',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.freedesktop.systemd1.Manager'";
/// The notification server (notifications spec § 1): its well-known name, object and interface.
pub const NOTIFICATIONS_NAME: &str = "org.freedesktop.Notifications";
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
pub const NOTIFICATIONS_INTERFACE: &str = "org.freedesktop.Notifications";
/// What `GetCapabilities` answers. Without `body` and `actions`, Chromium gives up on the server;
/// with `persistence`, it sends -1 rather than its own 25 s. Markup, hyperlinks, images, icons,
/// sound and inline reply are not advertised.
pub const CAPABILITIES: [&str; 4] = ["actions", "body", "persistence", "x-canonical-private-synchronous"];
/// `GetServerInformation`: name, vendor, (the crate's version), the spec version.
pub const SERVER_NAME: &str = "alpheus";
pub const SERVER_VENDOR: &str = "alpheus";
pub const SPEC_VERSION: &str = "1.3";
/// Well-known name prefixes the startup inventory probes.
pub const ITEM_NAME_PREFIXES: [&str; 2] = ["org.kde.StatusNotifierItem-", "org.freedesktop.StatusNotifierItem-"];
/// Bounds against misbehaving applications (spec § 3).
pub const MAX_ITEMS: usize = 64;
/// Items one connection may register (an application with more is misbehaving).
pub const MAX_ITEMS_PER_OWNER: usize = 16;
pub use crate::text::MAX_TEXT;
pub const MAX_MENU_ENTRIES: usize = 500;
pub const MAX_MENU_DEPTH: usize = 8;

/// An item: its owner's unique name (":1.N", never a well-known name) and its object path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemKey {
    pub owner: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Active,
    Passive,
    NeedsAttention,
}

impl Status {
    /// "Passive" | "NeedsAttention" | anything else → Active.
    pub fn parse(s: &str) -> Status {
        match s {
            "Passive" => Status::Passive,
            "NeedsAttention" => Status::NeedsAttention,
            _ => Status::Active,
        }
    }
}

/// The item properties the host reads, one `Get` each (never `GetAll`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Prop {
    Id,
    Title,
    Status,
    IconName,
    AttentionIconName,
    ToolTip,
    Menu,
    ItemIsMenu,
}

impl Prop {
    pub const ALL: [Prop; 8] =
        [Prop::Id, Prop::Title, Prop::Status, Prop::IconName, Prop::AttentionIconName, Prop::ToolTip, Prop::Menu, Prop::ItemIsMenu];

    pub fn name(self) -> &'static str {
        match self {
            Prop::Id => "Id",
            Prop::Title => "Title",
            Prop::Status => "Status",
            Prop::IconName => "IconName",
            Prop::AttentionIconName => "AttentionIconName",
            Prop::ToolTip => "ToolTip",
            Prop::Menu => "Menu",
            Prop::ItemIsMenu => "ItemIsMenu",
        }
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// A set of `Prop`s (one bit each).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PropSet(u8);

impl PropSet {
    pub const ALL: PropSet = PropSet(u8::MAX);

    pub fn insert(&mut self, p: Prop) {
        self.0 |= p.bit();
    }

    pub fn remove(&mut self, p: Prop) {
        self.0 &= !p.bit();
    }

    pub fn contains(self, p: Prop) -> bool {
        self.0 & p.bit() != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// In `Prop::ALL` order.
    pub fn iter(self) -> impl Iterator<Item = Prop> {
        Prop::ALL.into_iter().filter(move |p| self.contains(*p))
    }
}

/// A decoded property value (`ToolTip` reduced to its title; pixmaps never decoded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    Text(String),
    Status(Status),
    Path(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

impl Orientation {
    /// "vertical" | "horizontal" (SNI `Scroll`).
    pub fn name(self) -> &'static str {
        match self {
            Orientation::Vertical => "vertical",
            Orientation::Horizontal => "horizontal",
        }
    }
}

/// What the pointer did on a tray icon (spec § 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Left,
    Right,
    Middle,
    Scroll { delta: i32, orientation: Orientation },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_sets_hold_each_property_once() {
        let mut s = PropSet::default();
        assert!(s.is_empty());
        s.insert(Prop::ToolTip);
        s.insert(Prop::Id);
        s.insert(Prop::ToolTip);
        assert_eq!(s.iter().collect::<Vec<_>>(), [Prop::Id, Prop::ToolTip], "Prop::ALL order");
        s.remove(Prop::Id);
        assert!(!s.contains(Prop::Id) && s.contains(Prop::ToolTip));
        assert_eq!(PropSet::ALL.iter().collect::<Vec<_>>(), Prop::ALL);
        assert_eq!(Status::parse("NeedsAttention"), Status::NeedsAttention);
        assert_eq!(Status::parse("Passive"), Status::Passive);
        assert_eq!(Status::parse("passive"), Status::Active, "anything else");
    }

    /// The three MPRIS rules: signals only, from the namespace, the Player interface, the MPRIS object.
    #[test]
    fn the_mpris_rules_are_narrow() {
        assert_eq!(
            MPRIS_OWNER_RULE,
            format!(
                "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='{MPRIS_INTERFACE}'"
            )
        );
        assert_eq!(
            MPRIS_PROPERTIES_RULE,
            format!(
                "type='signal',interface='{PROPERTIES_INTERFACE}',member='PropertiesChanged',path='{MPRIS_PATH}',arg0='{MPRIS_PLAYER_INTERFACE}'"
            )
        );
        assert_eq!(MPRIS_SEEKED_RULE, format!("type='signal',interface='{MPRIS_PLAYER_INTERFACE}',member='Seeked',path='{MPRIS_PATH}'"));
        assert_eq!(format!("{MPRIS_INTERFACE}."), crate::media::MPRIS_PREFIX, "the namespace is the prefix without its dot");
    }

    /// The server's identity (notifications spec § 1): one name, object and interface; what it
    /// advertises; what it says it is.
    #[test]
    fn the_notification_server_names_and_capabilities() {
        assert_eq!(NOTIFICATIONS_NAME, "org.freedesktop.Notifications");
        assert_eq!(NOTIFICATIONS_PATH, "/org/freedesktop/Notifications");
        assert_eq!(NOTIFICATIONS_INTERFACE, NOTIFICATIONS_NAME);
        assert_eq!(CAPABILITIES, ["actions", "body", "persistence", "x-canonical-private-synchronous"]);
        for never in ["body-markup", "body-hyperlinks", "body-images", "icon-static", "sound", "inline-reply"] {
            assert!(!CAPABILITIES.contains(&never), "{never} is not advertised");
        }
        assert_eq!((SERVER_NAME, SERVER_VENDOR, SPEC_VERSION), ("alpheus", "alpheus", "1.3"));
    }
}
