//! The tray menu on the bar: asked for by an icon, opened in a popup, patched, acted on and ended.
//!
//! A menu starts when an icon is pressed (`menu_requested`). It opens in its popup only when its
//! first layout has arrived (`menu_layout_changed`); later layouts patch the open menu. What the
//! user does in it (`MenuAction`) becomes a call to the item or a change of the popup
//! (`menu_action`), and `menu_end` closes the menu's D-Bus session. `TrayMenu`, one field of `App`,
//! holds the state that lives between these calls.
//!
//! Never:
//! - start a thread, a process or a command: the item runs its own menu entries, the bar only sends
//!   the click;
//! - open the menu before its layout has arrived, or when the first layout arrives more than 2 s
//!   after the request.
//!
//! See: docs/specs/2026-10-05-alpheus-tray-design.md, docs/features/tray.md, and docs/decisions.md
//! ("A tray menu is shown only once its layout has arrived", "A tray menu opens with its first entry
//! selected", "A submenu opens in the same popup", "A failed Activate opens the menu of the icon
//! that was pressed", "A tray menu is closed by the button that opened it, or by any right click",
//! "A press right after the compositor closed a menu only closes it", "A late submenu graft is
//! refused once the selection has moved").

use crate::actions::Button;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, OpenPopup, PopupBody, PopupClose};
use crate::session_bus::ItemKey;
use crate::session_bus::codec::Layout;
use crate::session_bus::menu::{Menu, MenuAction};
use crate::session_bus::router::Tray;
use crate::wayland::{App, position_by};
use smithay_client_toolkit::shell::WaylandSurface;
use std::time::Instant;

/// The tray menus' and tooltip's state, one field of `App`.
#[derive(Default)]
pub(crate) struct TrayMenu {
    /// The item whose tooltip is shown. When it leaves the tray the tooltip closes (`ItemGone`).
    pub(crate) tooltip_item: Option<ItemKey>,
    /// The tray icon last pressed, the cells it occupies (where its menu hangs, if one is asked
    /// for) and the button that pressed it. A failed Activate answers asynchronously, and the menu
    /// it asks for belongs to the icon and button the user pressed, not to whatever is under the
    /// pointer by then. See docs/decisions.md, "A failed Activate opens the menu of the icon that
    /// was pressed".
    pub(crate) last_icon: Option<(ItemKey, PopupAnchor, Button)>,
    /// The menu asked for (its session opening, then its popup shown) and where it hangs.
    pub(crate) target: Option<(ItemKey, PopupAnchor)>,
    /// The button whose press opened `target`'s menu. A press with that button, or any right
    /// click, on the same icon only closes its menu. See docs/decisions.md, "A tray menu is closed
    /// by the button that opened it, or by any right click".
    pub(crate) opened_by: Option<Button>,
    /// A submenu asked for whose children are still being fetched: it is entered when they arrive.
    pub(crate) submenu_wanted: Option<i32>,
    /// When the menu's session opened. Its first layout must arrive within `bus::TIMEOUT`.
    pub(crate) asked_at: Option<Instant>,
    /// The icon, the closing button and the time of the menu last closed by `xdg_popup.done`
    /// (`PopupClose::Dismissed`). A compositor may deliver that event before the press that follows
    /// it reaches the bar; the live check alone (`closes_own_menu` in `wayland.rs`) would then
    /// see no menu open and let that very press reopen what it just closed. A press consulted
    /// against this record within `RECENT_CLOSE_WINDOW` only closes. See docs/decisions.md, "A
    /// press right after the compositor closed a menu only closes it".
    pub(crate) recently_closed: Option<(ItemKey, Button, Instant)>,
}

impl App {
    /// Asks for the menu of an item: remembers the clicked icon's cells on that output (`anchor`)
    /// and the button that asked for it. The popup opens when the first layout arrives, and never
    /// if 2 s pass first. See docs/decisions.md, "A tray menu is shown only once its layout has
    /// arrived".
    pub(crate) fn menu_requested(&mut self, key: ItemKey, anchor: PopupAnchor, button: Button) {
        // A menu already open, or whose session is already opening, for this icon is left alone. A
        // duplicate failed Activate (two quick left clicks on Steam, each failing in turn) must not
        // close and reopen the session already under way. This is not the case of a second right
        // click on the same icon, which does close the menu (`closes_own_menu` in `wayland.rs`).
        // `map` kept: clippy's `manual_map` asks for it. It takes the key out of the pair
        // `(key, anchor)`, if a menu was asked for.
        let asked_for = self.menu.target.as_ref().map(|(asked, _)| asked);
        if already_requested(asked_for, &key) {
            return;
        }
        // One menu at a time: a menu on screen goes, and the new session replaces its session.
        if matches!(self.popup_body(), Some(PopupBody::Menu(_))) {
            self.popup_close(PopupClose::Replaced);
        }
        let opened = self.tray.as_mut().is_some_and(|tray| tray.open_menu(&key));
        // `open_menu` has just sent calls of its own (AddMatch, AboutToShow), each with its own 2 s
        // deadline, so the timer is synced now and not only at the next bus event.
        self.tray_sync();
        if opened {
            self.menu.target = Some((key, anchor));
            self.menu.opened_by = Some(button);
            self.menu.submenu_wanted = None;
            self.menu.asked_at = Some(Instant::now());
        } else {
            // A request still waiting for an earlier layout does not outlive this one.
            self.menu_end();
            log("tray: no menu for this item");
        }
    }

    /// The session's layout changed. If the menu is on screen the layout patches it (and may let a
    /// waiting submenu in). Otherwise, if a menu was asked for, this first layout opens its popup.
    pub(crate) fn menu_layout_changed(&mut self) {
        let menu_shown = matches!(self.popup_body(), Some(PopupBody::Menu(_)));
        if menu_shown {
            // The three fields are taken out of `self` together so that the borrow checker sees
            // that the tray, the popup and the menu state are different things.
            let App { tray, popup, menu, .. } = self;
            let submenu_wanted = &mut menu.submenu_wanted;
            let (Some(layout), Some(m)) = (layout_of(tray.as_ref()), menu_of(popup.as_mut())) else {
                return;
            };
            m.relayout(layout);
            if let Some(id) = *submenu_wanted {
                // A late graft is refused in two ways. `Menu::enter` itself (`level_alive`, checked
                // against the current level) refuses an id that is no longer a selectable entry
                // there. `submenu_still_selected` refuses the narrower case that check cannot see:
                // the id is still a valid entry of this level, but the user has moved the selection
                // elsewhere without leaving the level. Steam and Electron send `LayoutUpdated` often,
                // so a late graft could otherwise jump into "More" long after the user looked away.
                // See docs/decisions.md, "A late submenu graft is refused once the selection has
                // moved".
                if submenu_still_selected(Some(id), m.selected()) {
                    if m.enter(id, layout) {
                        *submenu_wanted = None;
                    }
                } else {
                    // The user moved away first: there is nothing to graft.
                    *submenu_wanted = None;
                }
            }
            self.menu_repaint();
            return;
        }
        let Some((_, anchor)) = self.menu.target.clone() else { return };
        // The layout must come within `bus::TIMEOUT` of the request. Each call has its own 2 s, so
        // a layout can land later than that in all; the menu then opens nothing. See
        // docs/decisions.md, "A tray menu is shown only once its layout has arrived".
        let too_late = self.menu.asked_at.is_some_and(|asked_at| asked_at.elapsed() > crate::session_bus::bus::TIMEOUT);
        if too_late {
            self.menu_end();
            log("tray: menu not shown (timeout)");
            return;
        }
        let Some(i) = self.surfaces.iter().position(|s| s.name == anchor.output) else {
            self.menu_end();
            return;
        };
        let (max_columns, max_rows) = self.popup_room(i);
        let Some(layout) = layout_of(self.tray.as_ref()) else { return };
        let mut m = Menu::new();
        // Selects the first entry before the popup ever shows. See docs/decisions.md, "A tray menu
        // opens with its first entry selected".
        m.relayout(layout);
        let (columns, rows) = m.size(layout, max_columns, max_rows);
        // The rows that can be listed are two fewer than the popup's height (the frame): the same
        // figure `Menu::render` uses for its height.
        m.set_rows(usize::from(rows.saturating_sub(2)));
        let grid = m.render(layout, columns, rows);
        let qh = self.qh.clone();
        let (columns, rows) = grid.cells();
        if self.popup_open(&qh, anchor, PopupBody::Menu(m), columns, rows, Input::Grab) {
            self.popup_paint(grid);
            log("menu shown");
        } else {
            self.menu_end();
            log("tray: the menu could not open");
        }
    }

    /// Draws the menu again at its current level, resizing the popup when its size changed.
    fn menu_repaint(&mut self) {
        // The bar is found by the popup's parent surface (its identity), as `popup_close` does: an
        // output's name can change (`update_output`) or be shared by two bars while the menu is open.
        let Some(popup) = self.popup.as_ref() else { return };
        let Some(i) = position_by(&self.surfaces, |s| s.layer.wl_surface(), popup.parent_surface()) else { return };
        let (max_columns, max_rows) = self.popup_room(i);
        let App { tray, popup, .. } = self;
        let (Some(layout), Some(m)) = (layout_of(tray.as_ref()), menu_of(popup.as_mut())) else {
            return;
        };
        let (columns, rows) = m.size(layout, max_columns, max_rows);
        m.set_rows(usize::from(rows.saturating_sub(2)));
        let grid = m.render(layout, columns, rows);
        self.popup_paint(grid);
    }

    /// Carries out what the user did in the menu. A click (`Click`) sends the entry's id with the
    /// key's or the press's time to the item, then closes the menu as `Activated`. `Submenu`
    /// enters the submenu, at once if its children are known, else when they arrive. `Close` closes
    /// the menu (`Escape`). `Redraw` draws the menu again.
    pub(crate) fn menu_action(&mut self, a: MenuAction, timestamp: u32) {
        // A move of the selection or going back (both `Redraw`), a different submenu asked for, or
        // an entry run: each means the user no longer waits for the submenu asked for earlier.
        // Left in place, a late `LayoutUpdated` (Steam and Electron send it often) could graft
        // that submenu long after the user moved on.
        if matches!(a, MenuAction::Redraw | MenuAction::Submenu(_) | MenuAction::Click(_)) {
            self.menu.submenu_wanted = None;
        }
        match a {
            MenuAction::Nothing => {}
            MenuAction::Redraw => self.menu_repaint(),
            MenuAction::Close => self.popup_close(PopupClose::Escape),
            MenuAction::Click(id) => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.menu_click(id, timestamp);
                }
                self.tray_sync();
                self.popup_close(PopupClose::Activated);
            }
            MenuAction::Submenu(id) => {
                if let Some(tray) = self.tray.as_mut() {
                    tray.menu_submenu(id);
                }
                // Syncs the deadline timer for the AboutToShow and GetLayout calls just sent.
                self.tray_sync();
                let App { tray, popup, menu, .. } = self;
                let submenu_wanted = &mut menu.submenu_wanted;
                let (Some(layout), Some(m)) = (layout_of(tray.as_ref()), menu_of(popup.as_mut())) else {
                    return;
                };
                if m.enter(id, layout) {
                    *submenu_wanted = None;
                    self.menu_repaint();
                } else {
                    // Fetched lazily (AboutToShow, then GetLayout): entered when its layout is
                    // grafted in (`menu_layout_changed`).
                    *submenu_wanted = Some(id);
                }
            }
        }
    }

    /// The menu asked for or shown is over: its D-Bus session ends too (RemoveMatch), and a line
    /// is logged.
    pub(crate) fn menu_end(&mut self) {
        // The line is logged whenever the app had a request, even if the router already dropped
        // its own session first (its stale-path check closes the session before it ever reports
        // `close_menu`). `close_menu` is harmless when nothing is left to close, but the request
        // did end, and used to go unlogged in that case (commit 40be9a6).
        let had_one = self.menu.target.take().is_some();
        self.menu.opened_by = None;
        self.menu.submenu_wanted = None;
        self.menu.asked_at = None;
        if had_one {
            if let Some(tray) = self.tray.as_mut() {
                tray.close_menu();
            }
            log("tray: menu session closed");
        }
        // Re-arms the deadline timer after `close_menu`.
        self.tray_sync();
    }
}

/// The layout of the tray's menu session, if there is a tray, a session, and a layout read.
fn layout_of(tray: Option<&Tray>) -> Option<&Layout> {
    // Each `?` ends the function with None at the first step that is missing.
    let tray = tray?;
    let session = tray.menu()?;
    session.layout()
}

/// The menu in the open popup, if the popup is a menu.
fn menu_of(popup: Option<&mut OpenPopup>) -> Option<&mut Menu> {
    let popup = popup?;
    match &mut popup.body {
        PopupBody::Menu(menu) => Some(menu),
        _ => None,
    }
}

/// Whether the menu for `key` is already asked for or open (`asked_for` is the key of
/// `menu.target`). Asking again is then a no-op: a duplicate failed Activate (two quick left clicks
/// on an item without an Activate of its own, such as Steam) must not close and reopen the session
/// already under way.
fn already_requested(asked_for: Option<&ItemKey>, key: &ItemKey) -> bool {
    asked_for == Some(key)
}

/// Whether a submenu fetch (`menu.submenu_wanted`) is still worth grafting: only if the menu's
/// current selection is that very id when the layout arrives. Any other action in between clears
/// `menu.submenu_wanted` itself (`menu_action`), but `relayout` can also move the selection without
/// going through it (an entry dropped from the menu, say), so this is the last check before grafting.
fn submenu_still_selected(wanted: Option<i32>, selected: Option<i32>) -> bool {
    wanted.is_some() && wanted == selected
}

#[cfg(test)]
mod tests;
