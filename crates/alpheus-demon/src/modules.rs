//! Every per-module fact of the bar, one row of `MODULES` per `ModuleId`, in `ModuleId`'s
//! declaration order (a test pins each row's `id` to its index). Adding a module: a `ModuleId` variant
//! (config.rs), one row here, and, if it can be dropped on a narrow bar, its place in
//! `layout::REMOVAL`; its segment function lives with the others (`segments.rs`). A module with a
//! popup also needs a row of `popup::POPUPS`; one with a new source, a `Needs` flag (`startup::wanted`).
//!
//! Left as `match`es because their arms hold logic, not data: `actions::module_command` (the
//! clicks, which read the configuration), `actions::media_control` (a button to a player call).

use crate::config::ModuleId;
use crate::grid::{GroupKind, Hue};
use crate::layout::{Segment, bar_clock, window, workspaces};
use crate::popup::ModulePopup;
use crate::segments;
use crate::state::Bar;

/// The sources a module asks for (`startup::wanted`): a module that is not in `modules` costs
/// nothing. Plain bit flags, combined with `|` in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Needs(u16);

impl Needs {
    pub(crate) const NONE: Needs = Needs(0);
    /// The pw-dump child.
    pub(crate) const SOUND: Needs = Needs(1);
    /// The D-Bus domains (one flag each).
    pub(crate) const NETWORK: Needs = Needs(1 << 1);
    pub(crate) const BLUETOOTH: Needs = Needs(1 << 2);
    pub(crate) const BATTERY: Needs = Needs(1 << 3);
    pub(crate) const POWER_PROFILE: Needs = Needs(1 << 4);
    pub(crate) const RESOURCES: Needs = Needs(1 << 5);
    /// The tray's session-bus connection.
    pub(crate) const TRAY: Needs = Needs(1 << 6);
    pub(crate) const MEDIA: Needs = Needs(1 << 7);
    pub(crate) const NOTIFICATIONS: Needs = Needs(1 << 8);
    /// The camera devices' inotify watch and scan.
    pub(crate) const CAMERA: Needs = Needs(1 << 9);
    /// The sound source publishes for the OSD too (a microphone or a label change), not the sink's
    /// volume alone.
    pub(crate) const OSD_SCOPE: Needs = Needs(1 << 10);

    pub(crate) const fn or(self, other: Needs) -> Needs {
        Needs(self.0 | other.0)
    }

    pub(crate) const fn contains(self, other: Needs) -> bool {
        self.0 & other.0 == other.0
    }

    /// What `modules` ask for together.
    pub(crate) fn of(modules: &[ModuleId]) -> Needs {
        modules.iter().fold(Needs::NONE, |n, m| n.or(m.spec().needs))
    }
}

/// One module's facts.
pub(crate) struct ModuleSpec {
    /// The module itself; equals the row's index.
    pub id: ModuleId,
    /// Name in bar.toml.
    pub name: &'static str,
    /// The block the module belongs to (shapes spec § 2). Media is a block of its own (media spec
    /// § 1); the bell joins the status block.
    pub group: GroupKind,
    /// A module's fixed hue (themes spec § 9; the CPU's orange for resources), None for the
    /// modules without one.
    pub hue: Option<Hue>,
    /// TUI tab opened by a left click (alpheus `--tab`).
    pub tab: Option<&'static str>,
    /// The module's segment, or None if it has nothing to show; the field is None for the modules
    /// that never draw a cell in the bar.
    pub segment: Option<fn(&Bar, &str) -> Option<Segment>>,
    /// The popup a left press on the module toggles.
    pub popup: Option<ModulePopup>,
    /// The popup only opens with the quick module in `modules` (without it, the module keeps its
    /// own click).
    pub popup_needs_quick: bool,
    /// The sources the module asks for.
    pub needs: Needs,
}

impl ModuleId {
    pub(crate) const fn spec(self) -> &'static ModuleSpec {
        &MODULES[self as usize]
    }
}

/// A row with the defaults: no hue, tab, segment, popup nor need.
const fn row(id: ModuleId, name: &'static str, group: GroupKind) -> ModuleSpec {
    ModuleSpec { id, name, group, hue: None, tab: None, segment: None, popup: None, popup_needs_quick: false, needs: Needs::NONE }
}

pub(crate) const COUNT: usize = 18;

pub(crate) static MODULES: [ModuleSpec; COUNT] = [
    ModuleSpec { segment: Some(|_, _| Some(segments::launcher())), ..row(ModuleId::Launcher, "launcher", GroupKind::Launcher) },
    ModuleSpec {
        segment: Some(|bar, output| workspaces(&bar.compositor, output)),
        ..row(ModuleId::Workspaces, "workspaces", GroupKind::Workspaces)
    },
    ModuleSpec {
        hue: Some(Hue::Purple),
        segment: Some(|bar, _| window(&bar.compositor, bar.module_colors)),
        ..row(ModuleId::Window, "window", GroupKind::Title)
    },
    ModuleSpec {
        hue: Some(Hue::Purple),
        segment: Some(|bar, _| segments::media(bar.media.as_ref(), bar.module_colors)),
        popup: Some(ModulePopup::Media),
        needs: Needs::MEDIA,
        ..row(ModuleId::Media, "media", GroupKind::Media)
    },
    ModuleSpec {
        segment: Some(|bar, _| segments::privacy(&bar.privacy)),
        popup: Some(ModulePopup::Privacy),
        needs: Needs::SOUND.or(Needs::CAMERA),
        ..row(ModuleId::Privacy, "privacy", GroupKind::Privacy)
    },
    ModuleSpec {
        hue: Some(Hue::Blue),
        tab: Some("sound"),
        segment: Some(|bar, _| segments::volume(&bar.sound, bar.module_colors)),
        popup: Some(ModulePopup::Quick),
        popup_needs_quick: true,
        needs: Needs::SOUND,
        ..row(ModuleId::Volume, "volume", GroupKind::Status)
    },
    ModuleSpec {
        hue: Some(Hue::Cyan),
        tab: Some("network"),
        segment: Some(|bar, _| segments::network(&bar.network, bar.module_colors)),
        popup: Some(ModulePopup::Quick),
        popup_needs_quick: true,
        needs: Needs::NETWORK,
        ..row(ModuleId::Network, "network", GroupKind::Status)
    },
    ModuleSpec {
        hue: Some(Hue::Purple),
        tab: Some("bluetooth"),
        segment: Some(|bar, _| segments::bluetooth(&bar.bluetooth, bar.module_colors)),
        popup: Some(ModulePopup::Quick),
        popup_needs_quick: true,
        needs: Needs::BLUETOOTH,
        ..row(ModuleId::Bluetooth, "bluetooth", GroupKind::Status)
    },
    ModuleSpec {
        hue: Some(Hue::Green),
        tab: Some("home"),
        segment: Some(|bar, _| segments::battery(&bar.battery, &bar.config.battery, bar.module_colors)),
        needs: Needs::BATTERY,
        ..row(ModuleId::Battery, "battery", GroupKind::Status)
    },
    ModuleSpec {
        hue: Some(Hue::Orange),
        segment: Some(|bar, _| segments::resources(&bar.resources, &bar.config.resources.show, bar.module_colors)),
        popup: Some(ModulePopup::Resources),
        needs: Needs::RESOURCES,
        ..row(ModuleId::Resources, "resources", GroupKind::Resources)
    },
    ModuleSpec {
        segment: Some(|bar, _| segments::tray(&bar.tray, &bar.config.tray)),
        needs: Needs::TRAY,
        ..row(ModuleId::Tray, "tray", GroupKind::Tray)
    },
    ModuleSpec {
        segment: Some(|bar, _| segments::notifications(bar.notifications.as_ref())),
        popup: Some(ModulePopup::Panel),
        needs: Needs::NOTIFICATIONS,
        ..row(ModuleId::Notifications, "notifications", GroupKind::Status)
    },
    // The OSD draws its own surface (osd_surface.rs), never in the bar.
    ModuleSpec { needs: Needs::SOUND.or(Needs::OSD_SCOPE), ..row(ModuleId::Osd, "osd", GroupKind::Status) },
    // The quick panel is a popup (quick.rs), never a segment. It reads the sink list, NetworkManager
    // (the Wi-Fi row), BlueZ (the Bluetooth row) and the power profile.
    ModuleSpec {
        needs: Needs::SOUND.or(Needs::NETWORK).or(Needs::BLUETOOTH).or(Needs::POWER_PROFILE),
        ..row(ModuleId::Quick, "quick", GroupKind::Status)
    },
    // The screens are read and changed through the output protocol (outputs_app.rs), never drawn.
    row(ModuleId::Displays, "displays", GroupKind::Status),
    // The focus timer's chip (timer.rs): its popup is wired in a later task.
    ModuleSpec {
        hue: Some(Hue::Yellow),
        segment: Some(|bar, _| segments::timer(bar.timer.as_ref(), bar.module_colors)),
        popup: Some(ModulePopup::Timer),
        ..row(ModuleId::Timer, "timer", GroupKind::Timer)
    },
    ModuleSpec {
        segment: Some(|bar, _| Some(bar_clock(bar))),
        popup: Some(ModulePopup::Calendar),
        ..row(ModuleId::Clock, "clock", GroupKind::Clock)
    },
    // ⏻ alone; no segment when `[quick]` has no session command (power spec § 1).
    ModuleSpec {
        segment: Some(|bar, _| segments::power(&bar.config.quick)),
        popup: Some(ModulePopup::Power),
        ..row(ModuleId::Power, "power", GroupKind::Power)
    },
];

#[cfg(test)]
mod tests;
