//! Tests of the tray menu's two small decisions (`menu_app.rs`): when a submenu fetch is still worth
//! grafting, and when a menu is already asked for. The rest of the menu is on `App` and is covered
//! by the headless tests.

use super::*;

/// A submenu fetch grafts only while the menu's current selection is still the one it was asked
/// for.
#[test]
fn a_submenu_fetch_grafts_only_while_still_selected() {
    assert!(submenu_still_selected(Some(3), Some(3)), "still on it: graft it in");
    assert!(!submenu_still_selected(Some(3), Some(7)), "selection moved elsewhere: drop it");
    assert!(!submenu_still_selected(Some(3), None), "nothing selected any more: drop it");
    assert!(!submenu_still_selected(None, Some(3)), "nothing was ever asked for");
}

/// A duplicate failed Activate (two quick left clicks on an item without an Activate of its own,
/// such as Steam) must not close and reopen the menu already asked for.
#[test]
fn a_menu_already_asked_for_an_icon_is_not_asked_again() {
    let key = |n: u32| ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
    let (a, b) = (key(1), key(2));
    assert!(already_requested(Some(&a), &a), "the same icon: a duplicate failed Activate changes nothing");
    assert!(!already_requested(Some(&a), &b), "a different icon: its own menu still opens");
    assert!(!already_requested(None, &a), "nothing asked yet: opens normally");
}
