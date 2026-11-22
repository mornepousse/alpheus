//! The Power tab's whole screen at 80 by 24, as a snapshot.

use crate::app::action::Tab;
use crate::app::testing::ready_app;
use crate::ui::testing::draw;
use std::time::Instant;

#[test]
fn the_power_tab_at_80_by_24_matches_its_snapshot() {
    let mut a = ready_app();
    a.go_tab(Tab::Power, Instant::now());
    insta::assert_snapshot!("power_tab", draw(&a, 80, 24).0.join("\n"));
}
