use super::*;

/// Fix round 2026-10-06, requirement 1: a submenu fetch grafts only while the menu's current
/// selection is still the one it was asked for.
#[test]
fn a_submenu_fetch_grafts_only_while_still_selected() {
    assert!(submenu_still_selected(Some(3), Some(3)), "still on it: graft it in");
    assert!(!submenu_still_selected(Some(3), Some(7)), "selection moved elsewhere: drop it");
    assert!(!submenu_still_selected(Some(3), None), "nothing selected any more: drop it");
    assert!(!submenu_still_selected(None, Some(3)), "nothing was ever asked for");
}

/// Review requirement 3: a duplicate failed Activate (two quick left clicks on an item without
/// its own Activate, e.g. Steam) must not close and reopen the menu already asked for it.
#[test]
fn open_menu_for_the_same_key_already_asked_is_a_no_op() {
    let key = |n: u32| ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
    let (a, b) = (key(1), key(2));
    assert!(already_requested(Some(&a), &a), "the same icon: a duplicate failed Activate changes nothing");
    assert!(!already_requested(Some(&a), &b), "a different icon: its own menu still opens");
    assert!(!already_requested(None, &a), "nothing asked yet: opens normally");
}
