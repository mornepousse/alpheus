//! The system tray (spec docs/specs/2026-10-05-alpheus-tray-design.md):
//! StatusNotifierItem watcher and host, dbusmenu menus, on a second
//! dbus-rs connection, to the session bus, driven by the bar's own calloop
//! loop (no thread). This file is the tray's vocabulary; it knows no D-Bus
//! library.

pub mod bus;
pub mod codec;
pub mod host;
pub mod router;
pub mod watcher;

pub const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
pub const WATCHER_INTERFACE: &str = "org.kde.StatusNotifierWatcher";
pub const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
pub const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";
/// Well-known name prefixes the startup inventory probes.
pub const ITEM_NAME_PREFIXES: [&str; 2] = ["org.kde.StatusNotifierItem-", "org.freedesktop.StatusNotifierItem-"];
/// Bounds against misbehaving applications (spec § 3).
pub const MAX_ITEMS: usize = 64;
pub const MAX_TEXT: usize = 256;
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
}
