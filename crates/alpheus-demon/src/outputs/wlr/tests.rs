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
    for (scale, transform) in [(0.0, 0), (-1.0, 0), (f64::NAN, 0), (f64::INFINITY, 0), (1.0, 8)] {
        assert_eq!(plan(&SHOWN, &with(scale, transform)), Err(Unsendable), "{scale} {transform}");
    }
}
