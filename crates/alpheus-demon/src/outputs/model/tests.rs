use super::*;
use alpheus_commun::displays::{Active, Mode, Scale, Step};

fn m(width: i32, height: i32, mhz: i32, preferred: bool) -> HeadMode {
    HeadMode { width, height, mhz, preferred }
}

fn head(name: &str, modes: Vec<HeadMode>, current: Option<usize>) -> Head {
    Head {
        name: name.into(),
        description: format!("Acme Panel {name} (via cable)"),
        make: "Acme".into(),
        model: "P24".into(),
        serial: "S1".into(),
        enabled: current.is_some(),
        modes,
        current,
        x: 0,
        y: 0,
        scale: 1.0,
        transform: 0,
        displaylink: false,
    }
}

fn want(w: u32, h: u32, r: u32) -> Mode {
    Mode { width: w, height: h, refresh: r }
}

fn sixty() -> Head {
    head("DP-1", vec![m(1920, 1080, 59_940, false), m(1920, 1080, 60_000, true)], Some(0))
}

fn step(out: &str, mode: Mode, x: i32) -> Step {
    Step { output: out.into(), active: Some(Active { mode, x, y: 0, scale: Scale(1000), transform: 0 }) }
}

#[test]
fn identity_prefers_make_model_serial() {
    assert_eq!(sixty().identity(), "Acme P24 S1");
    let mut h = sixty();
    h.serial.clear();
    assert_eq!(h.identity(), "Acme P24");
    h.make.clear();
    assert_eq!(h.identity(), "P24");
}

#[test]
fn identity_falls_back_to_description() {
    let mut h = sixty();
    h.make.clear();
    h.model.clear();
    assert_eq!(h.identity(), "Acme Panel DP-1 (via cable)");
}

#[test]
fn sixty_resolves_to_the_current_59_94_without_change() {
    let h = sixty();
    assert_eq!(resolve_mode(&h, want(1920, 1080, 60)), Some(0));
    let t = targets(std::slice::from_ref(&h), &[step("DP-1", want(1920, 1080, 60), 0)]).unwrap();
    assert!(is_noop(&[h], &t));
}

#[test]
fn sixty_prefers_preferred_when_current_differs() {
    let mut h = sixty();
    h.modes.push(m(1920, 1080, 60_010, false));
    h.modes.push(m(1280, 720, 60_000, false));
    h.current = Some(3);
    assert_eq!(resolve_mode(&h, want(1920, 1080, 60)), Some(1));
    // Without a preferred one, the highest wins.
    h.modes[1].preferred = false;
    assert_eq!(resolve_mode(&h, want(1920, 1080, 60)), Some(2));
    assert_eq!(resolve_mode(&h, want(1920, 1080, 75)), None);
}

fn dock() -> Head {
    let mut h = head(
        "DVI-I-1",
        vec![m(1920, 1080, 144_000, false), m(1920, 1080, 120_000, false), m(1920, 1080, 60_000, true), m(1280, 720, 100_000, false)],
        Some(2),
    );
    h.displaylink = true;
    h
}

#[test]
fn displaylink_hides_and_lowers_above_120() {
    let h = dock();
    let out = to_output(&h);
    let hz: Vec<u32> = out.modes.iter().map(|m| m.refresh).collect();
    assert_eq!(hz, vec![120, 60, 100]);
    assert_eq!(resolve_mode(&h, want(1920, 1080, 144)), Some(1));
    assert_eq!(resolve_mode(&h, want(1920, 1080, 120)), Some(1));
    // Nothing of that size at or under the cap: preferred of any size.
    let mut only_fast = dock();
    only_fast.modes.remove(1);
    only_fast.modes.remove(1);
    only_fast.current = Some(0);
    assert_eq!(resolve_mode(&only_fast, want(1920, 1080, 144)), Some(1));
    // A non-DisplayLink head keeps its fast mode.
    let mut plain = dock();
    plain.displaylink = false;
    assert_eq!(resolve_mode(&plain, want(1920, 1080, 144)), Some(0));
    assert_eq!(to_output(&plain).modes.len(), 4);
}

#[test]
fn a_current_mode_above_the_cap_still_shows_as_active() {
    let mut h = dock();
    h.current = Some(0);
    let out = to_output(&h);
    assert_eq!(out.active.as_ref().unwrap().mode, want(1920, 1080, 144));
    assert!(!out.modes.contains(&want(1920, 1080, 144)));
}

#[test]
fn over_cap_finds_the_highest_mode_at_or_below_120() {
    let mut h = dock();
    assert_eq!(over_cap(&h), None);
    h.current = Some(0);
    assert_eq!(over_cap(&h), Some(1));
    h.modes.insert(1, m(1920, 1080, 119_880, false));
    h.current = Some(0);
    assert_eq!(over_cap(&h), Some(2));
    h.displaylink = false;
    assert_eq!(over_cap(&h), None);
}

#[test]
fn targets_keep_unmentioned_heads() {
    let mut a = sixty();
    a.x = 1920;
    a.scale = 1.5;
    a.transform = 1;
    let b = head("DP-2", vec![m(2560, 1440, 144_000, true)], Some(0));
    let off = head("DP-3", vec![m(1280, 1024, 60_000, true)], None);
    let t = targets(&[a, b, off], &[step("DP-2", want(2560, 1440, 144), 5)]).unwrap();
    assert_eq!(t.len(), 3);
    assert_eq!(t[0].state, Some((0, 1920, 0, 1.5, 1)));
    assert_eq!(t[1].state, Some((0, 5, 0, 1.0, 0)));
    assert_eq!(t[2].state, None);
}

#[test]
fn targets_refuse_all_off() {
    let h = sixty();
    let off = Step { output: "DP-1".into(), active: None };
    assert_eq!(targets(std::slice::from_ref(&h), &[off]).unwrap_err(), "refused: no screen would stay on");
    let bad = step("DP-1", want(800, 600, 60), 0);
    assert_eq!(targets(&[h], &[bad]).unwrap_err(), "DP-1: no such mode");
}

#[test]
fn noop_tolerates_scale_rounding() {
    let mut h = sixty();
    h.scale = 1.333_333;
    let same = |scale: f64, x: i32, state: bool| {
        let t = vec![Target { name: "DP-1".into(), state: state.then_some((0, x, 0, scale, 0)) }];
        is_noop(std::slice::from_ref(&h), &t)
    };
    assert!(same(1.333, 0, true));
    assert!(!same(1.4, 0, true));
    assert!(!same(1.333, 7, true));
    assert!(!same(1.333, 0, false));
}

#[test]
fn modes_that_round_to_the_same_hz_are_listed_once_in_head_order() {
    let h = head("DP-1", vec![m(1920, 1080, 59_940, false), m(1280, 720, 60_000, false), m(1920, 1080, 60_000, true)], Some(0));
    assert_eq!(to_output(&h).modes, vec![want(1920, 1080, 60), want(1280, 720, 60)]);
}

#[test]
fn an_enabled_head_with_an_unknown_mode_stays_enabled() {
    let mut a = sixty();
    a.current = None;
    a.x = 40;
    let b = head("DP-2", vec![m(2560, 1440, 144_000, true)], Some(0));
    let t = targets(&[a.clone(), b.clone()], &[]).unwrap();
    assert_eq!(t[0].state, Some((KEEP, 40, 0, 1.0, 0)));
    assert!(is_noop(&[a.clone(), b], &t));
    // Not "all off" when the only enabled head has no known mode.
    assert!(targets(std::slice::from_ref(&a), &[]).is_ok());
    // A stale index is the same as an unknown mode.
    a.current = Some(99);
    assert_eq!(targets(std::slice::from_ref(&a), &[]).unwrap()[0].state.unwrap().0, KEEP);
}

#[test]
fn a_current_just_above_the_cap_is_still_lowered() {
    let mut h = dock();
    h.modes.push(m(1920, 1080, 120_400, false));
    h.current = Some(4);
    assert_eq!(over_cap(&h), Some(1));
    // Alone of its size: any size, the preferred first.
    let mut lone = dock();
    lone.modes = vec![m(1920, 1080, 120_400, false), m(1280, 720, 60_000, true), m(1024, 768, 75_000, false)];
    lone.current = Some(0);
    assert_eq!(over_cap(&lone), Some(1));
    // A stale index does not panic.
    lone.current = Some(9);
    assert_eq!(over_cap(&lone), None);
}

#[test]
fn the_cap_is_exactly_120_000_mhz() {
    assert_eq!(DISPLAYLINK_MAX_MHZ, 120_000);
    let mut h = dock();
    h.modes = vec![m(1920, 1080, 120_000, false), m(1920, 1080, 120_001, false)];
    h.current = Some(0);
    let out = to_output(&h);
    assert_eq!(out.modes, vec![want(1920, 1080, 120)]);
    h.current = Some(1);
    assert_eq!(over_cap(&h), Some(0));
    assert_eq!(
        over_cap(&{
            let mut x = h.clone();
            x.current = Some(0);
            x
        }),
        None
    );
}

#[test]
fn the_fallback_prefers_the_preferred_mode_over_the_larger_one() {
    let mut h = dock();
    h.modes = vec![m(3840, 2160, 144_000, false), m(2560, 1440, 60_000, false), m(1280, 720, 60_000, true)];
    h.current = Some(0);
    assert_eq!(resolve_mode(&h, want(3840, 2160, 144)), Some(2));
    h.modes[2].preferred = false;
    assert_eq!(resolve_mode(&h, want(3840, 2160, 144)), Some(1));
}

#[test]
fn noop_fails_on_every_field() {
    let mut h = sixty();
    h.modes.push(m(1280, 720, 60_000, false));
    h.x = 10;
    h.y = 20;
    h.transform = 1;
    let base = (0, 10, 20, 1.0, 1);
    let check = |state: Option<(usize, i32, i32, f64, u8)>| is_noop(std::slice::from_ref(&h), &[Target { name: "DP-1".into(), state }]);
    assert!(check(Some(base)));
    assert!(!check(Some((2, 10, 20, 1.0, 1))));
    assert!(!check(Some((0, 10, 21, 1.0, 1))));
    assert!(!check(Some((0, 10, 20, 1.0, 2))));
    assert!(!check(None));
    assert!(check(Some((0, 10, 20, 1.004, 1))));
    assert!(!check(Some((0, 10, 20, 1.006, 1))));
    let mut off = h.clone();
    off.enabled = false;
    off.current = None;
    assert!(!is_noop(&[off.clone()], &[Target { name: "DP-1".into(), state: Some(base) }]));
    assert!(is_noop(&[off], &[Target { name: "DP-1".into(), state: None }]));
}

#[test]
fn odd_values_are_sanitised() {
    assert_eq!(hz(i32::MAX), 2_147_484);
    assert_eq!(hz(-5), 0);
    assert_eq!((hz(59_499), hz(59_500), hz(59_940)), (59, 60, 60));
    let mut h = sixty();
    h.scale = f64::NAN;
    h.transform = 9;
    assert_eq!(targets(std::slice::from_ref(&h), &[]).unwrap()[0].state, Some((0, 0, 0, 1.0, 0)));
    let mut bad = sixty();
    bad.modes.push(m(-1920, 1080, 60_000, false));
    assert_eq!(to_output(&bad).modes, vec![want(1920, 1080, 60)]);
}
