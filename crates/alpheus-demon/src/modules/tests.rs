use super::*;
use crate::grid::GroupKind;

/// The table is indexed by `ModuleId as usize`: one row per variant, in variant order.
#[test]
fn one_row_per_module_in_variant_order() {
    for (i, spec) in MODULES.iter().enumerate() {
        assert_eq!(spec.id as usize, i, "{} is not at its own index", spec.name);
        assert_eq!(ModuleId::ALL[i], spec.id);
        assert_eq!(spec.id.spec().name, spec.name);
    }
    assert_eq!(ModuleId::Power as usize + 1, MODULES.len(), "the last variant ends the table");
}

#[test]
fn names_are_unique_and_lowercase() {
    for (i, a) in MODULES.iter().enumerate() {
        assert_eq!(a.name, a.name.to_lowercase());
        assert!(MODULES[i + 1..].iter().all(|b| b.name != a.name), "{} twice", a.name);
    }
}

/// The modules that never draw a cell, and the popups' owners.
#[test]
fn segments_and_popups_belong_to_the_right_modules() {
    let no_segment: Vec<ModuleId> = MODULES.iter().filter(|m| m.segment.is_none()).map(|m| m.id).collect();
    assert_eq!(no_segment, [ModuleId::Osd, ModuleId::Quick]);
    let gated: Vec<ModuleId> = MODULES.iter().filter(|m| m.popup_needs_quick).map(|m| m.id).collect();
    assert_eq!(gated, [ModuleId::Volume, ModuleId::Network, ModuleId::Bluetooth]);
    assert!(MODULES.iter().filter(|m| m.popup_needs_quick).all(|m| m.popup == Some(ModulePopup::Quick)));
    assert_eq!(ModuleId::Notifications.spec().group, GroupKind::Status, "the bell joins the status block");
}

#[test]
fn needs_combine_and_test_by_flag() {
    let n = Needs::SOUND.or(Needs::TRAY);
    assert!(n.contains(Needs::SOUND) && n.contains(Needs::TRAY) && !n.contains(Needs::MEDIA));
    assert!(n.contains(Needs::NONE));
    assert_eq!(Needs::of(&[]), Needs::NONE);
    assert_eq!(Needs::of(&[ModuleId::Volume, ModuleId::Tray]), n);
}
