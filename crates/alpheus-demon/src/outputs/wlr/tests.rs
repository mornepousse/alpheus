use super::*;

fn on(name: &str, mode: usize) -> Target {
    Target { name: name.into(), state: Some((mode, 0, 0, 1.0, 0)) }
}

fn off(name: &str) -> Target {
    Target { name: name.into(), state: None }
}

const SHOWN: [(&str, usize); 2] = [("DP-1", 3), ("eDP-1", 1)];

#[test]
fn a_configuration_names_every_head_once_in_the_targets_order() {
    assert_eq!(plan(&SHOWN, &[on("eDP-1", 0), on("DP-1", 2)]), Ok(vec![1, 0]));
    assert_eq!(plan(&SHOWN, &[off("DP-1"), on("eDP-1", KEEP)]), Ok(vec![0, 1]), "KEEP sends no mode");
    assert_eq!(plan(&SHOWN, &[on("DP-1", 0)]), Err(Unsendable), "a head left out is a protocol error");
    assert_eq!(plan(&SHOWN, &[on("DP-1", 0), on("DP-1", 1)]), Err(Unsendable), "twice is a protocol error");
    assert_eq!(plan(&SHOWN, &[on("DP-1", 0), on("HDMI-A-1", 0)]), Err(Unsendable), "a head of no done");
    assert_eq!(plan(&[], &[]), Ok(vec![]));
}

#[test]
fn a_configuration_the_compositor_would_reject_is_not_built() {
    assert_eq!(plan(&SHOWN, &[on("DP-1", 3), on("eDP-1", 0)]), Err(Unsendable), "a mode of no such index");
    let with = |scale: f64, transform: u8| [Target { name: "DP-1".into(), state: Some((0, 0, 0, scale, transform)) }, off("eDP-1")];
    assert!(plan(&SHOWN, &with(1.5, 7)).is_ok());
    assert!(plan(&SHOWN, &with(10.0, 0)).is_ok(), "the largest scale");
    assert!(plan(&SHOWN, &with(1.0 / 256.0, 0)).is_ok(), "the smallest wl_fixed above 0");
    // A scale below 1/256 goes out as wl_fixed 0 (`invalid_scale`); above 10, out of range.
    for (scale, transform) in [(0.0, 0), (-1.0, 0), (f64::NAN, 0), (f64::INFINITY, 0), (1.0, 8), (0.001, 0), (0.0038, 0), (10.01, 0)] {
        assert_eq!(plan(&SHOWN, &with(scale, transform)), Err(Unsendable), "{scale} {transform}");
    }
}

#[test]
fn the_configuration_in_flight_is_dropped_once_replaced_answered_or_given_up() {
    let mut p: Pending<&str> = Pending::default();
    assert_eq!(p.replace(1, "one"), None);
    assert_eq!(p.replace(2, "two"), Some("one"), "a newer one replaces it");
    assert_eq!(p.abandoned(Some(2)), None, "still awaited");
    assert_eq!(p.abandoned(None), Some("two"), "the controller gave up on it");
    assert_eq!(p.abandoned(None), None, "dropped once");
    p.replace(3, "three");
    assert!(!p.answered(&"other") && p.abandoned(Some(3)).is_none(), "not that one's answer");
    assert!(p.answered(&"three"));
    assert_eq!(p.abandoned(None), None, "answered: already destroyed");
    p.replace(4, "four");
    assert_eq!(p.abandoned(Some(5)), Some("four"), "a newer generation in flight");
}
