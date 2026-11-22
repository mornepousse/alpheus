use super::*;
use crate::grid::GroupKind;

/// Every variant with the row it must have. The `match` is exhaustive: a variant added to
/// `ModuleId` stops this file compiling until it is listed here, and listed, its row must exist.
macro_rules! rows {
    ($($variant:ident => $index:expr),* $(,)?) => {
        #[allow(dead_code)]
        fn row_of(m: ModuleId) -> usize {
            match m {
                $(ModuleId::$variant => $index),*
            }
        }
        const LISTED: &[(ModuleId, usize)] = &[$((ModuleId::$variant, $index)),*];
    };
}

rows! {
    Launcher => 0, Workspaces => 1, Window => 2, Media => 3, Privacy => 4, Failed => 5, Volume => 6, Network => 7,
    Bluetooth => 8, Battery => 9, Resources => 10, Tray => 11, Notifications => 12, Osd => 13,
    Quick => 14, Displays => 15, Timer => 16, Clock => 17, Power => 18,
}

/// The table is indexed by `ModuleId as usize`: one row per variant, in variant order.
#[test]
fn one_row_per_module_in_variant_order() {
    for &(m, i) in LISTED {
        assert_eq!(m as usize, i, "{m:?}'s discriminant is not its row");
        assert_eq!(MODULES.get(i).map(|r| r.id), Some(m), "{m:?} has no row at {i}");
    }
    assert_eq!(LISTED.len(), MODULES.len(), "a row for no variant, or a variant for no row");
    for (i, spec) in MODULES.iter().enumerate() {
        assert_eq!(spec.id as usize, i, "{} is not at its own index", spec.name);
    }
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
    assert_eq!(no_segment, [ModuleId::Osd, ModuleId::Quick, ModuleId::Displays]);
    let gated: Vec<ModuleId> = MODULES.iter().filter(|m| m.popup_needs_quick).map(|m| m.id).collect();
    assert_eq!(gated, [ModuleId::Volume, ModuleId::Network, ModuleId::Bluetooth]);
    assert!(MODULES.iter().filter(|m| m.popup_needs_quick).all(|m| m.popup == Some(ModulePopup::Quick)));
    assert_eq!(ModuleId::Notifications.spec().group, GroupKind::Status, "the bell joins the status block");
}

/// The failed-units module reads the Units domain, opens the Diag tab, draws its own block and
/// takes no popup.
#[test]
fn the_failed_module_reads_the_units_domain_and_opens_the_diag_tab() {
    let spec = ModuleId::Failed.spec();
    assert_eq!((spec.name, spec.group, spec.tab, spec.popup), ("failed", GroupKind::Failed, Some("diagnostics"), None));
    assert_eq!(spec.hue, None, "its glyph carries the error role");
    assert!(spec.segment.is_some());
    assert_eq!(Needs::of(&[ModuleId::Failed]), Needs::UNITS);
    assert!(!Needs::of(&[ModuleId::Clock]).contains(Needs::UNITS), "nothing else asks for it");
}

#[test]
fn needs_combine_and_test_by_flag() {
    let n = Needs::SOUND.or(Needs::TRAY);
    assert!(n.contains(Needs::SOUND) && n.contains(Needs::TRAY) && !n.contains(Needs::MEDIA));
    assert!(n.contains(Needs::NONE));
    assert_eq!(Needs::of(&[]), Needs::NONE);
    assert_eq!(Needs::of(&[ModuleId::Timer]), Needs::NONE, "the timer reads no source");
    assert_eq!(Needs::of(&[ModuleId::Volume, ModuleId::Tray]), n);
}

/// `ModuleSpec::name` is the word bar.toml spells (serde's `lowercase` on `ModuleId`).
#[test]
fn names_are_the_bar_toml_spelling() {
    for spec in &MODULES {
        let cfg =
            crate::config::BarConfig::from_text(&format!("modules = [\"{}\"]", spec.name)).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
        assert_eq!(cfg.modules, [spec.id], "{}", spec.name);
    }
}
