//! The tray menu's session wiring: asked by an icon's press, opened on the first layout, patched by
//! later ones, its actions mapped to the tray's calls and popup changes, ended with its D-Bus session.

use crate::actions::Button;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, PopupBody, PopupClose};
use crate::tray::ItemKey;
use crate::tray::menu::{Menu, MenuAction};
use crate::tray::menu_session::MenuSession;
use crate::wayland::{App, position_by};
use smithay_client_toolkit::shell::WaylandSurface;
use std::time::Instant;

/// The tray menus' and tooltip's state, one field of `App`.
#[derive(Default)]
pub(crate) struct TrayMenu {
    /// The item whose tooltip is shown (it leaving closes the tooltip: ItemGone).
    pub(crate) tooltip_item: Option<ItemKey>,
    /// The tray icon last pressed, its cells (where its menu hangs if one is asked for) and the
    /// button that pressed it: a failed Activate's menu, arriving asynchronously, is for the icon
    /// and button the user actually pressed, not necessarily the one under the pointer by then.
    pub(crate) last_icon: Option<(ItemKey, PopupAnchor, Button)>,
    /// The menu asked for (its session opening, then its popup shown) and where it hangs.
    pub(crate) target: Option<(ItemKey, PopupAnchor)>,
    /// The button whose press opened `target`'s menu (Mae, 2026-10-06): a click with THAT
    /// button, or a right click (always), on the same icon only closes its menu.
    pub(crate) opened_by: Option<Button>,
    /// A submenu asked for whose children are being fetched: entered when they arrive.
    pub(crate) submenu_wanted: Option<i32>,
    /// When the menu's session opened: its first layout must come within bus::TIMEOUT (Mae: 2 s).
    pub(crate) asked_at: Option<Instant>,
    /// The icon, the closing button and the time of the menu last closed by `xdg_popup.done`
    /// (`PopupClose::Dismissed`): a compositor may deliver `xdg_popup.done`
    /// before the press that follows it reaches the bar, so the live check alone (`closes_own_
    /// menu`) would let that very press reopen what it just closed — a record of it, consulted
    /// within `RECENT_CLOSE_WINDOW`, makes the race harmless.
    pub(crate) recently_closed: Option<(ItemKey, Button, Instant)>,
}

impl App {
    /// The item's menu: remembered anchor (the clicked icon's zone on that output) and the button
    /// that asked for it; the popup opens when the first layout arrives (question 8) — or never,
    /// when 2 s pass first.
    pub(crate) fn menu_requested(&mut self, key: ItemKey, anchor: PopupAnchor, button: Button) {
        // A menu already open, or whose session is already opening, for this
        // icon is untouched — a duplicate failed Activate (two quick left clicks on Steam, each
        // erroring in turn) must not close and reopen the session already in flight for it. Distinct
        // from a second RIGHT click on the same icon (`closes_own_menu`, in the Press arm below),
        // which does close it (Mae's decision).
        if already_requested(self.menu.target.as_ref().map(|(k, _)| k), &key) {
            return;
        }
        // One menu at a time: a menu shown goes (its session is replaced by the new one).
        if matches!(self.popup_body(), Some(PopupBody::Menu(_))) {
            self.popup_close(PopupClose::Replaced);
        }
        let opened = self.tray.as_mut().is_some_and(|t| t.open_menu(&key));
        // `open_menu` just sent calls of its own (AddMatch, AboutToShow), each
        // with its own 2 s deadline — re-arm the timer now, not only at the next bus event.
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

    /// The session's layout changed: the first one opens the popup, later ones patch it.
    pub(crate) fn menu_layout_changed(&mut self) {
        let menu_shown = matches!(self.popup_body(), Some(PopupBody::Menu(_)));
        if menu_shown {
            let App { tray, popup, menu, .. } = self;
            let submenu_wanted = &mut menu.submenu_wanted;
            let (Some(layout), Some(PopupBody::Menu(m))) =
                (tray.as_ref().and_then(|t| t.menu()).and_then(MenuSession::layout), popup.as_mut().map(|p| &mut p.body))
            else {
                return;
            };
            m.relayout(layout);
            if let Some(id) = *submenu_wanted {
                // A stale
                // asynchronous graft is refused two ways now. `Menu::enter` itself
                // (`level_alive`, checked against the CURRENT level) catches an id that is no
                // longer a selectable entry there at all. `submenu_still_selected` catches the
                // narrower case `level_alive` cannot see: the id IS still a valid entry of this
                // level, but the user has since moved the selection elsewhere (a hover, a scroll)
                // without leaving the level — Steam and Electron re-emit `LayoutUpdated` often
                // enough that a late graft can otherwise jump into "More" long after the user
                // looked elsewhere.
                if submenu_still_selected(Some(id), m.selected()) {
                    if m.enter(id, layout) {
                        *submenu_wanted = None;
                    }
                } else {
                    *submenu_wanted = None; // stale: the user moved away first, nothing to graft
                }
            }
            self.menu_repaint();
            return;
        }
        let Some((key, anchor)) = self.menu.target.clone() else { return };
        // Mae: at most 2 s from the request to the menu — each call has its own 2 s, so a layout can
        // land later than that; it opens nothing then.
        if self.menu.asked_at.is_some_and(|t| t.elapsed() > crate::tray::bus::TIMEOUT) {
            self.menu_end();
            log("tray: menu not shown (timeout)");
            return;
        }
        let Some(i) = self.surfaces.iter().position(|s| s.name == anchor.output) else {
            self.menu_end();
            return;
        };
        let (max_columns, max_rows) = self.popup_room(i);
        let Some(layout) = self.tray.as_ref().and_then(|t| t.menu()).and_then(MenuSession::layout) else { return };
        let mut m = Menu::new();
        m.relayout(layout); // the first entry selected before the popup ever shows
        let (columns, rows) = m.size(layout, max_columns, max_rows);
        m.set_rows(usize::from(rows.saturating_sub(2))); // consistent with render's height (size.1 − 2)
        let grid = m.render(layout, columns, rows);
        let qh = self.qh.clone();
        let (columns, rows) = grid.cells();
        if self.popup_open(&qh, anchor, PopupBody::Menu(m), columns, rows, Input::Grab) {
            self.popup_paint(grid);
            log(&format!("menu shown for {}{}", key.owner, key.path));
        } else {
            self.menu_end();
            log("tray: the menu could not open");
        }
    }

    /// The menu re-rendered at its current level (the popup resized when its size changed).
    fn menu_repaint(&mut self) {
        // The bar by the popup's parent SURFACE (identity), like `popup_close`: its output's name can
        // change (`update_output`) or be shared by two bars while the menu is open.
        let Some(i) = self.popup.as_ref().and_then(|p| position_by(&self.surfaces, |s| s.layer.wl_surface(), p.parent_surface())) else {
            return;
        };
        let (max_columns, max_rows) = self.popup_room(i);
        let App { tray, popup, .. } = self;
        let (Some(layout), Some(PopupBody::Menu(m))) =
            (tray.as_ref().and_then(|t| t.menu()).and_then(MenuSession::layout), popup.as_mut().map(|p| &mut p.body))
        else {
            return;
        };
        let (columns, rows) = m.size(layout, max_columns, max_rows);
        m.set_rows(usize::from(rows.saturating_sub(2)));
        let grid = m.render(layout, columns, rows);
        self.popup_paint(grid);
    }

    /// MenuAction → Tray calls and popup changes (Click: menu_click with the key or press time, then
    /// close Activated; Submenu: menu_submenu, enter when grafted; Close: close_menu + popup_close).
    pub(crate) fn menu_action(&mut self, a: MenuAction, timestamp: u32) {
        // Any of these three actions invalidates a pending
        // submenu fetch (`submenu_wanted`) — a selection move or going back (both Redraw), a
        // DIFFERENT submenu asked for, or an entry run all mean the user is no longer waiting on
        // the one that was asked for before. Left uncleared, a late `LayoutUpdated` (Steam,
        // Electron re-emit it often) could otherwise graft it in long after the user moved on.
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
                self.tray_sync(); // re-arms the deadline timer for AboutToShow/GetLayout
                let App { tray, popup, menu, .. } = self;
                let submenu_wanted = &mut menu.submenu_wanted;
                let (Some(layout), Some(PopupBody::Menu(m))) =
                    (tray.as_ref().and_then(|t| t.menu()).and_then(MenuSession::layout), popup.as_mut().map(|p| &mut p.body))
                else {
                    return;
                };
                if m.enter(id, layout) {
                    *submenu_wanted = None;
                    self.menu_repaint();
                } else {
                    // Fetched lazily (AboutToShow, GetLayout): entered when grafted (menu_changed).
                    *submenu_wanted = Some(id);
                }
            }
        }
    }

    /// The menu asked for or shown is over: its D-Bus session too (RemoveMatch), logged.
    pub(crate) fn menu_end(&mut self) {
        // Logged whenever there WAS an app-level request —
        // even if the router already dropped its own session object first (the stale-path check
        // closes it before ever surfacing `close_menu`): `tray.close_menu()` is harmless once
        // there is nothing left to close, but the request DID end, and used to go unlogged then.
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
        self.tray_sync(); // re-arms the deadline timer after close_menu
    }
}

/// `open_menu` for the key whose menu is already open, or whose session is
/// already opening, is a no-op — a duplicate failed Activate (two quick left clicks on an item
/// without its own Activate, e.g. Steam) must not close and reopen the session already in flight.
fn already_requested(menu_for: Option<&ItemKey>, key: &ItemKey) -> bool {
    menu_for == Some(key)
}

/// A submenu fetch (`submenu_wanted`) is worth grafting only
/// if the menu's current selection is still that very id when the layout arrives — any other
/// action in between (a move, going back, a different submenu chosen) clears `submenu_wanted`
/// itself (`menu_action`), but `relayout`'s own bookkeeping can also shift the selection without
/// going through it (an entry dropped from the menu, say); this is the last check before grafting.
fn submenu_still_selected(wanted: Option<i32>, selected: Option<i32>) -> bool {
    wanted.is_some() && wanted == selected
}

#[cfg(test)]
mod tests;
