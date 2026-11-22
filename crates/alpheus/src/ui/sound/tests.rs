use crate::app::action::Tab;
use crate::app::testing::ready_app;
use crate::ui::testing::draw;
use std::time::Instant;

#[test]
fn sound_tab() {
    let mut a = ready_app();
    a.go_tab(Tab::Sound, Instant::now());
    insta::assert_snapshot!("sound_tab", draw(&a, 80, 24).0.join("\n"));
}
