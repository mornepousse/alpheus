//! Tests of the tray's bridge (`bridge.rs`): the remembered icon press that a menu answer uses up.

use super::*;
use crate::popup::Anchor as PopupAnchor;

/// The press remembered for an Activate answer is used up by the first answer that uses it. A
/// second, late answer, or one for another icon, finds nothing.
#[test]
fn the_remembered_press_is_consumed_by_the_menu_it_opens() {
    let key = |n: u32| ItemKey { owner: format!(":1.{n}"), path: "/StatusNotifierItem".into() };
    let press = |n: u32| Some((key(n), PopupAnchor { output: "DP-1".into(), start: 3, end: 5 }, Button::Left));
    let mut last = press(1);
    assert!(take_icon_for(&mut last, &key(1)).is_some(), "the first answer opens the menu");
    assert!(last.is_none(), "and spends the press");
    assert!(take_icon_for(&mut last, &key(1)).is_none(), "a late second answer opens nothing");
    let mut last = press(1);
    assert!(take_icon_for(&mut last, &key(2)).is_none(), "another icon's answer opens nothing");
    assert!(last.is_none(), "and the stale press is gone too");
}
