//! Tests of the Displays tab's drawing (`ui/displays.rs`): its whole screen at 80 by 24 as a
//! snapshot, the columns an output row keeps, and a panel reference that is not connected. The
//! rest of the tab's drawing is tested in `ui/tests.rs`.

use super::*;
use crate::app::action::Tab;
use crate::app::testing::ready_app;
use crate::ui::testing::draw;
use std::time::Instant;

#[test]
fn the_displays_tab_at_80_by_24_matches_its_snapshot() {
    let mut a = ready_app();
    a.go_tab(Tab::Displays, Instant::now());
    insta::assert_snapshot!("displays_tab", draw(&a, 80, 24).0.join("\n"));
}

#[test]
fn an_output_row_gives_up_its_scale_then_its_connector_as_the_placement_needs_room() {
    // The placement's 10 cells must fit: the scale column starts at 64, the next layout at 51.
    assert_eq!(Cols::fit(74, 10).pos, 64, "exactly enough for the full layout");
    assert_eq!(Cols::fit(73, 10).pos, 51, "one cell short: no scale");
    assert_eq!(Cols::fit(61, 10).pos, 51, "exactly enough without the scale");
    let compact = Cols::fit(60, 10);
    assert_eq!((compact.pos, compact.conn), (41, None), "one cell short: no connector either");
}

#[test]
fn a_reference_screen_that_is_not_connected_shows_the_brand_from_its_description() {
    let a = ready_app();
    let mut panel = Panel {
        output: "eDP-1".into(),
        field: PanelField::Reference,
        mode: crate::displays::testing::mode(2560, 1600, 60),
        scale: crate::displays::Scale::ONE,
        transform: 0,
        place: PlaceChoice::Rel(crate::displays::geometry::Relation::RightOf),
        reference: Some("Charlie Modèle 3 SERIE0003".into()),
        align: crate::displays::geometry::Align::Start,
        free: None,
        query: None,
    };
    assert_eq!(field_value(&a, &panel, PanelField::Reference), "Charlie");
    panel.reference = None;
    assert_eq!(field_value(&a, &panel, PanelField::Reference), "—");
}
